#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod air_intrinsics;
pub(crate) mod air_static_init;
pub mod as_shadow;
mod b78_tcg;
mod construction;
mod dominators;
mod emission_order;
mod emit_sidecar;
mod emitted_effects;
pub mod env_vars;
mod fc_air_specialize;
mod fc_specialize;
pub(crate) mod float16;
pub mod fxhash;
pub mod ift_runtime;
mod layout;
pub mod linked_functions;
pub mod mesh_lower;
pub mod meta;
pub mod native;
pub mod passes;
mod passthrough;
pub mod reflect;
mod spirv_binary;
mod spirv_module;
mod spirv_operand;
mod spirv_variable_ptr;
pub mod tools;
pub(crate) mod types;

pub use fc_air_specialize::specialize_air_function_constants;
pub use fc_specialize::{
    specialize_function_constant_bytes, specialize_function_constants,
    specialize_function_constants_zero,
};
pub use passthrough::{
    translate_passthrough, translate_passthrough_specialized, translate_vertex_observer,
};

use crate::spirv_module::{load_bytes as load_owned_module, Module};
use std::borrow::Cow;
use std::path::Path;

pub fn detect_stage(src: &str, tmp: &Path) -> Result<passes::Stage, String> {
    let ll = tools::air_to_sanitized_ll(src, tmp)?;
    if ll.contains("!air.vertex =") {
        Ok(passes::Stage::Vertex)
    } else if ll.contains("!air.fragment =") {
        Ok(passes::Stage::Fragment)
    } else if ll.contains("!air.kernel =") {
        Ok(passes::Stage::Kernel)
    } else {
        Err(
            "metal2vulkan: no !air.vertex/!air.fragment/!air.kernel stage metadata in module"
                .into(),
        )
    }
}

pub fn translate(src: &str, stage: passes::Stage, tmp: &Path) -> Result<Vec<u8>, String> {
    translate_with_options(src, stage, tmp, passes::TransformOptions::default())
}

pub fn translate_with_options(
    src: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
) -> Result<Vec<u8>, String> {
    let (san_ll, datalayout) = tools::air_to_sanitized_ll_with_datalayout(src, tmp)?;
    let datalayout = datalayout
        .as_deref()
        .map(layout::AirDataLayout::parse)
        .transpose()?;
    translate_sanitized_native_with_options_and_layout(&san_ll, stage, tmp, options, datalayout)
}

pub fn translate_sanitized_native(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
) -> Result<Vec<u8>, String> {
    translate_sanitized_native_with_options(san_ll, stage, tmp, passes::TransformOptions::default())
}

fn lower_air_text_if_enabled(san_ll: &str) -> Cow<'_, str> {
    native::lower_air_text(san_ll)
}

fn reject_unsupported_metal_linked_functions(san_ll: &str) -> Result<(), String> {
    if san_ll.contains(".MTL_VISIBLE_FN_REF") || san_ll.contains("!air.visible_function_references")
    {
        return Err(
            "native emitter: unsupported Metal visible function reference; dynamic linked \
             functions are not expressible in Logical SPIR-V"
                .into(),
        );
    }
    Ok(())
}

fn reject_function_constant_erased_effects(san_ll: &str, module: &Module) -> Result<(), String> {
    if !emitted_effects::air_declares_an_observable_write(san_ll) {
        return Ok(());
    }
    if emitted_effects::module_has_an_observable_effect(module) {
        return Ok(());
    }
    let unsupplied = meta::function_constants_without_a_supplied_value(san_ll);
    if unsupplied.is_empty() {
        return Ok(());
    }
    let named = unsupplied
        .iter()
        .take(4)
        .map(|constant| format!("{} (index {})", constant.name, constant.index))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "the entry writes a texture or a device buffer, but no write survived folding {} function \
         constant(s) the caller supplied no value for -- {named}. AIR declares no default for a \
         function constant, so the zero folded in its place is a value the shader never asked for; \
         emitting the module would silently do nothing where Metal writes",
        unsupplied.len(),
    ))
}

fn reject_imageblock_only_effects(san_ll: &str, module: &Module) -> Result<(), String> {
    if !emitted_effects::air_declares_an_imageblock_write(san_ll) {
        return Ok(());
    }
    if emitted_effects::module_has_an_observable_effect(module) {
        return Ok(());
    }
    Err(
        "the entry's only write is into an imageblock, which this translator stages in \
         per-invocation or per-threadgroup memory because Vulkan has no tile-resolve step; \
         emitting the module would hand a caller a dispatch that writes nothing where Metal \
         resolves the tile into its attachments"
            .to_string(),
    )
}

fn options_for_air(
    san_ll: &str,
    mut options: passes::TransformOptions,
) -> Result<passes::TransformOptions, String> {
    options
        .descriptor_layout
        .validate()
        .map_err(|error| error.to_string())?;
    if let Some(dispatch) = options.kernel_dispatch {
        dispatch.validate()?;
    }
    options.validate_runtime_samplers()?;
    options.validate_runtime_storage_images()?;
    if san_ll.contains("air.compile.denorms_disable") {
        options.denorm_flush_to_zero_f32 = true;
    }
    Ok(options)
}

struct StageMeta {
    frag: Option<meta::FragMeta>,
    vert: Option<meta::VertMeta>,
    kern: Option<meta::KernMeta>,
    entry_name: Option<String>,
}

fn parse_stage_meta(san_ll: &str, stage: passes::Stage) -> StageMeta {
    match stage {
        passes::Stage::Fragment => {
            let (frag, entry_name) = meta::parse_air_fragment_meta_with_entry(san_ll);
            StageMeta {
                frag,
                vert: None,
                kern: None,
                entry_name,
            }
        }
        passes::Stage::Vertex => {
            let (vert, entry_name) = meta::parse_air_vertex_meta_with_entry(san_ll);
            StageMeta {
                frag: None,
                vert,
                kern: None,
                entry_name,
            }
        }
        passes::Stage::Kernel => {
            let (kern, _, entry_name) = meta::parse_air_kernel_meta_variants(san_ll);
            StageMeta {
                frag: None,
                vert: None,
                kern,
                entry_name,
            }
        }
    }
}

fn stage_buffer_layouts<'a>(
    stage: passes::Stage,
    frag: Option<&'a meta::FragMeta>,
    vert: Option<&'a meta::VertMeta>,
    kern: Option<&'a meta::KernMeta>,
) -> Option<&'a crate::fxhash::HashMap<u32, meta::AirType>> {
    match stage {
        passes::Stage::Fragment => frag.map(|meta| &meta.buffer_layouts),
        passes::Stage::Vertex => vert.map(|meta| &meta.buffer_layouts),
        passes::Stage::Kernel => kern.map(|meta| &meta.buffer_layouts),
    }
}

fn is_runtime_storage_image_binding(binding: &reflect::ResourceBinding, metal_index: u32) -> bool {
    binding.metal_index == metal_index
        && (binding.kind == reflect::ResourceKind::StorageImage
            || matches!(
                binding.kind,
                reflect::ResourceKind::TextureArray
                    | reflect::ResourceKind::EmbeddedArgBufferTexture
            ) && binding.access == Some(reflect::ResourceAccess::Storage))
}

fn build_reflection(
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    options: &passes::TransformOptions,
) -> Result<reflect::ShaderReflection, String> {
    if let Some(dispatch) = options.kernel_dispatch {
        dispatch.validate()?;
    }
    if !matches!(stage, passes::Stage::Kernel) && options.kernel_dispatch.is_some() {
        return Err("kernel dispatch bounds are only valid for kernel stages".to_string());
    }
    let mut reflection = match stage {
        passes::Stage::Fragment => {
            reflect::ShaderReflection::from_fragment(&frag.cloned().unwrap_or_default(), entry_name)
        }
        passes::Stage::Vertex => {
            reflect::ShaderReflection::from_vertex(&vert.cloned().unwrap_or_default(), entry_name)
        }
        passes::Stage::Kernel => reflect::ShaderReflection::from_kernel(
            &kern.cloned().unwrap_or_default(),
            entry_name,
            options.kernel_local_size,
        ),
    };
    if matches!(stage, passes::Stage::Kernel) {
        reflection.kernel_dispatch = Some(
            options
                .kernel_dispatch
                .unwrap_or_else(reflect::KernelDispatch::safe_default),
        );
    }
    let runtime_sampler_indices = reflection
        .bindings
        .iter()
        .filter(|binding| binding.kind == reflect::ResourceKind::Sampler)
        .map(|binding| binding.metal_index)
        .collect::<std::collections::BTreeSet<_>>();
    reflection.runtime_sampler_specializations = options
        .runtime_sampler_states
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(metal_index, state)| {
            let metal_index = u32::try_from(metal_index).ok()?;
            if !runtime_sampler_indices.contains(&metal_index) {
                return None;
            }
            Some(reflect::RuntimeSamplerSpecialization {
                metal_index,
                state: state?,
            })
        })
        .collect();
    for (metal_index, state) in options
        .runtime_storage_image_states
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(metal_index, state)| Some((u32::try_from(metal_index).ok()?, state?)))
    {
        let mut applied = false;
        let spirv_format = state.format.explicit_format();
        for binding in reflection
            .bindings
            .iter_mut()
            .filter(|binding| is_runtime_storage_image_binding(binding, metal_index))
        {
            applied = true;
            if let Some(shape) = binding.texture_shape.as_mut() {
                shape.storage_format = spirv_format;
            }
        }
        if !applied {
            continue;
        }
        reflection.runtime_storage_image_specializations.push(
            reflect::RuntimeStorageImageSpecialization {
                metal_index,
                state,
                spirv_format,
            },
        );
    }
    reflection.apply_descriptor_layout(options.descriptor_layout)?;
    Ok(reflection)
}

fn validate_reflected_runtime_storage_images(
    reflection: &reflect::ShaderReflection,
    options: &passes::TransformOptions,
) -> Result<(), String> {
    for (metal_index, state) in options
        .runtime_storage_image_states
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(metal_index, state)| Some((u32::try_from(metal_index).ok()?, state?)))
    {
        let specialized = reflection
            .runtime_storage_image_specializations
            .iter()
            .any(|specialization| specialization.metal_index == metal_index);
        if !specialized {
            return Err(format!(
                "runtime storage image {metal_index}: no reflected storage-image binding exists for runtime format {:?}",
                state.format
            ));
        }
        let runtime_component = state.format.component();
        for binding in reflection
            .bindings
            .iter()
            .filter(|binding| is_runtime_storage_image_binding(binding, metal_index))
        {
            let Some(shape) = binding.texture_shape else {
                return Err(format!(
                    "runtime storage image {metal_index}: reflected storage-image binding has no texture shape"
                ));
            };
            if shape.component != runtime_component {
                return Err(format!(
                    "runtime storage image {metal_index}: AIR texels are {:?}, but runtime format {:?} is {runtime_component:?}",
                    shape.component, state.format
                ));
            }
        }
    }
    Ok(())
}

pub fn translate_sanitized_native_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
) -> Result<Vec<u8>, String> {
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: datalayout parse start");
    }
    let datalayout = layout::AirDataLayout::from_ir(san_ll)?;
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: datalayout parse complete");
    }
    translate_sanitized_native_with_options_and_layout(san_ll, stage, tmp, options, datalayout)
}

pub fn translate_sanitized_native_specialized_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    function_constants: &[(u32, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    let datalayout = layout::AirDataLayout::from_ir(san_ll)?;
    let specialized =
        fc_air_specialize::specialize_air_function_constants(san_ll, function_constants)?;
    translate_sanitized_native_with_options_and_layout(
        specialized.as_ref(),
        stage,
        tmp,
        options,
        datalayout,
    )
}

pub fn translate_sanitized_native_specialized_reflected_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    function_constants: &[(u32, Vec<u8>)],
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    let datalayout = layout::AirDataLayout::from_ir(san_ll)?;
    let specialized =
        fc_air_specialize::specialize_air_function_constants(san_ll, function_constants)?;
    translate_sanitized_native_reflected_with_layout(
        specialized.as_ref(),
        stage,
        tmp,
        options,
        datalayout,
    )
}

pub fn translate_sanitized_native_owned_with_options(
    san_ll: String,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
) -> Result<Vec<u8>, String> {
    let datalayout = layout::AirDataLayout::from_ir(&san_ll)?;
    let lowered = native::lower_air_text_owned(san_ll);
    translate_sanitized_native_pre_lowered_with_layout(&lowered, stage, tmp, options, datalayout)
}

fn translate_sanitized_native_with_options_and_layout(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    datalayout: Option<layout::AirDataLayout>,
) -> Result<Vec<u8>, String> {
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: AIR pre-lowering start");
    }
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: AIR pre-lowering complete");
    }
    translate_sanitized_native_pre_lowered_with_layout(san_ll, stage, tmp, options, datalayout)
}

fn translate_sanitized_native_pre_lowered_with_layout(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    datalayout: Option<layout::AirDataLayout>,
) -> Result<Vec<u8>, String> {
    reject_unsupported_metal_linked_functions(san_ll)?;
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: stage metadata parse start");
    }
    let stage_meta = parse_stage_meta(san_ll, stage);
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: stage metadata parse complete");
    }
    let options = options_for_air(san_ll, options)?;
    if env_vars::retry_debug() {
        eprintln!("[retry-debug] translate: construction core start");
    }
    translate_sanitized_with_meta(
        san_ll,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        tmp,
        options,
        datalayout,
    )
}

pub fn translate_sanitized_native_linked_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    linkage: &linked_functions::LinkedFunctionLinkage,
) -> Result<Vec<u8>, String> {
    let specialized = specialize_linked_module(san_ll, stage, linkage)?;
    translate_sanitized_native_with_options(&specialized, stage, tmp, options)
}

pub fn specialize_linked_module(
    san_ll: &str,
    stage: passes::Stage,
    linkage: &linked_functions::LinkedFunctionLinkage,
) -> Result<String, String> {
    let stage_name = match stage {
        passes::Stage::Kernel => "kernel",
        passes::Stage::Vertex => "vertex",
        passes::Stage::Fragment => "fragment",
    };
    let entry_name = meta::entry_name(san_ll, stage_name)
        .ok_or_else(|| format!("linked translation found no AIR {stage_name} entry"))?;
    let specialized =
        linked_functions::specialize_visible_function_tables(san_ll, &entry_name, linkage)?;
    let specialized =
        linked_functions::specialize_visible_function_references(&specialized, linkage)?;
    let specialized = linked_functions::specialize_opaque_triangle_intersection_tables(
        &specialized,
        &entry_name,
        linkage,
    )?;
    Ok(specialized)
}

pub fn specialize_runtime_table_module(
    san_ll: &str,
    stage: passes::Stage,
    references: Vec<linked_functions::LinkedFunctionReference>,
    candidates: &[(String, String)],
) -> Result<String, String> {
    let stage_name = match stage {
        passes::Stage::Kernel => "kernel",
        passes::Stage::Vertex => "vertex",
        passes::Stage::Fragment => "fragment",
    };
    let entry_name = meta::entry_name(san_ll, stage_name)
        .ok_or_else(|| format!("runtime-table link found no AIR {stage_name} entry"))?;
    let indices: Vec<u32> = if matches!(stage, passes::Stage::Kernel) {
        let kmeta = meta::parse_air_kernel_meta(san_ll)
            .ok_or_else(|| "runtime-table link: kernel metadata did not parse".to_string())?;
        kmeta
            .roles
            .iter()
            .filter(|(_, r)| matches!(r, meta::KernRole::VisibleFunctionTable(_)))
            .map(|(i, _)| *i)
            .collect()
    } else {
        let mut v: Vec<u32> = san_ll
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with('!') && l.contains("!\"air.visible_function_table\""))
            .filter_map(|l| {
                l.split("!{i32 ")
                    .nth(1)?
                    .split(',')
                    .next()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let rewritten = linked_functions::rewrite_runtime_table_lookups(san_ll, &entry_name, &indices)?;
    let rewritten =
        ift_runtime::rewrite_runtime_intersection_tables(&rewritten, &entry_name, candidates)?;
    let entries: Vec<linked_functions::LinkedFunction> = candidates
        .iter()
        .enumerate()
        .map(
            |(k, (symbol, module_ll))| linked_functions::LinkedFunction {
                index: k as u32 + 1,
                symbol: symbol.clone(),
                module_ll: module_ll.clone(),
            },
        )
        .collect();
    if entries.is_empty() && !indices.is_empty() {
        return Err(
            "the kernel reads a visible function table but the pipeline links no functions".into(),
        );
    }
    let linkage = linked_functions::LinkedFunctionLinkage {
        visible_references: references,
        visible_tables: indices
            .iter()
            .map(|&parameter_index| linked_functions::LinkedFunctionTable {
                parameter_index,
                size: entries.len() as u32 + 1,
                entries: entries.clone(),
            })
            .collect(),
        intersection_tables: Vec::new(),
    };
    let _dedupe = linked_functions::DedupeDependencies::on();
    specialize_linked_module(&rewritten, stage, &linkage)
}

pub fn translate_sanitized_native_linked_specialized_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    linkage: &linked_functions::LinkedFunctionLinkage,
    function_constants: &[(u32, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    let specialized = specialize_linked_module(san_ll, stage, linkage)?;
    translate_sanitized_native_specialized_with_options(
        &specialized,
        stage,
        tmp,
        options,
        function_constants,
    )
}

pub fn translate_sanitized_native_linked_specialized_reflected_with_options(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    linkage: &linked_functions::LinkedFunctionLinkage,
    function_constants: &[(u32, Vec<u8>)],
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    let specialized = specialize_linked_module(san_ll, stage, linkage)?;
    translate_sanitized_native_specialized_reflected_with_options(
        &specialized,
        stage,
        tmp,
        options,
        function_constants,
    )
}

pub fn translate_reflected(
    src: &str,
    stage: passes::Stage,
    tmp: &Path,
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    translate_reflected_with_options(src, stage, tmp, passes::TransformOptions::default())
}

pub fn translate_reflected_with_options(
    src: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    let (san_ll, datalayout) = tools::air_to_sanitized_ll_with_datalayout(src, tmp)?;
    let parsed_datalayout = datalayout
        .as_deref()
        .map(layout::AirDataLayout::parse)
        .transpose()?;
    let (spv, mut reflection) = translate_sanitized_native_reflected_with_layout(
        &san_ll,
        stage,
        tmp,
        options,
        parsed_datalayout,
    )?;
    reflection.datalayout = datalayout;
    Ok((spv, reflection))
}

pub fn reflect_sanitized(
    san_ll: &str,
    stage: passes::Stage,
    options: passes::TransformOptions,
) -> Result<reflect::ShaderReflection, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    let stage_meta = parse_stage_meta(san_ll, stage);
    let options = options_for_air(san_ll, options)?;
    let mut reflection = build_reflection(
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        &options,
    )?;
    validate_reflected_runtime_storage_images(&reflection, &options)?;
    reflection.function_constants = meta::parse_function_constants(san_ll);
    reflection.refine_buffer_access_from_entry(san_ll);
    reflection.add_static_samplers(san_ll)?;
    if stage == passes::Stage::Kernel
        && native::requires_device_address_model_for_source(
            san_ll,
            stage_meta.kern.as_ref(),
            stage_meta.entry_name.as_deref(),
        )
    {
        reflection.add_buffer_address_table()?;
    }
    reflection.validate_descriptor_abi()?;
    Ok(reflection)
}

pub fn reflect_sanitized_specialized(
    san_ll: &str,
    stage: passes::Stage,
    options: passes::TransformOptions,
    function_constants: &[(u32, Vec<u8>)],
) -> Result<reflect::ShaderReflection, String> {
    let specialized =
        fc_air_specialize::specialize_air_function_constants(san_ll, function_constants)?;
    reflect_sanitized(specialized.as_ref(), stage, options)
}

pub fn translate_sanitized_native_reflected(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    let datalayout = layout::AirDataLayout::from_ir(san_ll)?;
    translate_sanitized_native_reflected_with_layout(san_ll, stage, tmp, options, datalayout)
}

fn translate_sanitized_native_reflected_with_layout(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
    options: passes::TransformOptions,
    datalayout: Option<layout::AirDataLayout>,
) -> Result<(Vec<u8>, reflect::ShaderReflection), String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    reject_unsupported_metal_linked_functions(san_ll)?;
    let stage_meta = parse_stage_meta(san_ll, stage);
    let options = options_for_air(san_ll, options)?;
    passes::validate_kernel_dispatch_options(stage, options)?;
    let mut reflection = build_reflection(
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        &options,
    )?;
    validate_reflected_runtime_storage_images(&reflection, &options)?;
    reflection.function_constants = meta::parse_function_constants(san_ll);
    reflection.refine_buffer_access_from_entry(san_ll);
    reflection.add_static_samplers(san_ll)?;
    reflection.validate_descriptor_abi()?;
    let finished = translate_sanitized_with_meta_prevalidated_carrier(
        san_ll,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        tmp,
        options,
        datalayout,
    )?;
    reflection.report_ray_instance_user_id_table(finished.ray_instance_user_id_table_binding)?;
    reflection.reconcile_buffer_address_table(&finished.module);
    reflection.fragment_sample_positions =
        finished
            .fragment_sample_positions
            .then_some(reflect::SamplePositionPushConstantRange {
                offset: 96,
                size: 64,
                positions: 8,
                stride: 8,
            });
    reflection.report_synthesized_placeholders(
        &finished.module,
        &finished.placeholder_descriptor_bindings,
    );
    reflection.reconcile_texture_shapes(&finished.module);
    reflection.retract_unbound_static_samplers(&finished.module);
    reflection.add_buffer_footprints(&finished.module)?;
    reflection.validate_descriptor_abi()?;
    Ok((finished.bytes, reflection))
}

pub fn translate_native_no_retry(san_ll: &str, stage: passes::Stage) -> Result<Vec<u8>, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    reject_unsupported_metal_linked_functions(&lowered)?;
    let stage_meta = parse_stage_meta(&lowered, stage);
    translate_native_no_retry_with_meta(
        &lowered,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
    )
}

pub fn translate_native_no_retry_with_options(
    san_ll: &str,
    stage: passes::Stage,
    options: passes::TransformOptions,
) -> Result<Vec<u8>, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    reject_unsupported_metal_linked_functions(&lowered)?;
    let stage_meta = parse_stage_meta(&lowered, stage);
    emit_finish_primary_module(
        &lowered,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        options,
    )
    .map(|finished| finished.bytes)
}

pub fn translate_native_no_retry_constructed_with_options(
    san_ll: &str,
    stage: passes::Stage,
    options: passes::TransformOptions,
) -> Result<Vec<u8>, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    reject_unsupported_metal_linked_functions(san_ll)?;
    let stage_meta = parse_stage_meta(san_ll, stage);
    passes::validate_kernel_dispatch_options(stage, options)?;
    let air_data_layout = crate::layout::AirDataLayout::from_ir(san_ll)?;
    let tmp = Path::new("");
    let rc = construction::ConstructionCtx::new(
        san_ll,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        tmp,
        options,
        air_data_layout,
    );
    let primary = match tools::emit_vulkan_spirv_with_outcome(
        san_ll,
        tmp,
        rc.kern,
        rc.entry_name,
        stage_buffer_layouts(rc.stage, rc.frag, rc.vert, rc.kern),
    ) {
        Ok(emitted) => {
            rc.remember_ordinary_plan_rejections(&emitted);
            rc.finish_primary_carrier(emitted)
        }
        Err(failure) => {
            rc.remember_raw_buffer_layout_rejection(failure.rejected.raw_buffer_layout_required);
            rc.remember_ordinary_plan_rejection_set(&failure.rejected.ordinary_plan_functions);
            rc.remember_ownership_plan_rejection_set(&failure.rejected.ownership_plan_functions);
            Err(failure.error)
        }
    };
    // the inline retry builds its buffers raw first: on Blender Eevee's deferred light shader the typed construction of
    // the inlined module validated and rendered black, the raw one lit the scene as the reference GPU does
    if raw_buffers_first_on() {
        if let Ok(finished) = rc.construct_raw() {
            return Ok(finished.bytes);
        }
    }
    let constructed = match primary {
        Ok(finished) => finished,
        Err(emit_err) if rc.needs_raw_construction() => {
            rc.construct_raw().map_err(|construction_error| {
                format!("{emit_err}; raw construction failed: {construction_error}")
            })?
        }
        Err(emit_err) => return Err(emit_err),
    };
    Ok(constructed.bytes)
}

fn translate_native_no_retry_with_meta(
    san_ll: &str,
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
) -> Result<Vec<u8>, String> {
    emit_finish_primary_module(
        san_ll,
        stage,
        frag,
        vert,
        kern,
        entry_name,
        passes::TransformOptions::default(),
    )
    .map(|finished| finished.bytes)
}

fn emit_finish_primary_module(
    san_ll: &str,
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    options: passes::TransformOptions,
) -> Result<FinishedModule, String> {
    passes::validate_kernel_dispatch_options(stage, options)?;
    let air_data_layout = crate::layout::AirDataLayout::from_ir(san_ll)?;
    tools::emit_vulkan_spirv_with_sidecar(
        san_ll,
        Path::new(""),
        kern,
        entry_name,
        stage_buffer_layouts(stage, frag, vert, kern),
    )
    .and_then(|emitted| {
        finish_module(
            emitted,
            stage,
            frag,
            vert,
            kern,
            entry_name,
            air_data_layout.as_ref(),
            options,
            FinishConstruction::Primary,
        )
        .map_err(|failure| failure.error)
    })
}

pub fn translate_native_primary_validated(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
) -> Result<Vec<u8>, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    reject_unsupported_metal_linked_functions(san_ll)?;
    let stage_meta = parse_stage_meta(san_ll, stage);
    let finished = emit_finish_primary_module(
        san_ll,
        stage,
        stage_meta.frag.as_ref(),
        stage_meta.vert.as_ref(),
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        passes::TransformOptions::default(),
    )?;
    if let Some(path) = env_vars::retry_dump() {
        let _ = std::fs::write(path, &finished.bytes);
    }
    tools::spirv_val_bytes(&finished.bytes, tmp)?;
    Ok(finished.bytes)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinishConstruction {
    Plain,
    Primary,
    RawRelooper,
}

#[derive(Clone)]
struct FinishedModule {
    module: Module,
    bytes: Vec<u8>,
    placeholder_descriptor_bindings: Vec<u32>,
    ray_instance_user_id_table_binding: Option<u32>,
    fragment_sample_positions: bool,
}

impl FinishedModule {
    fn new(
        module: Module,
        placeholder_descriptor_bindings: Vec<u32>,
        ray_instance_user_id_table_binding: Option<u32>,
        fragment_sample_positions: bool,
    ) -> Self {
        let bytes = assemble_finished_module(&module);
        Self {
            module,
            bytes,
            placeholder_descriptor_bindings,
            ray_instance_user_id_table_binding,
            fragment_sample_positions,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinishFailureKind {
    Other,
    RawBufferConstruction,
    CfgConstruction,
}

#[derive(Debug)]
struct FinishFailure {
    kind: FinishFailureKind,
    error: String,
}

impl FinishFailure {
    fn cfg(error: String) -> Self {
        Self {
            kind: FinishFailureKind::CfgConstruction,
            error,
        }
    }
}

impl From<String> for FinishFailure {
    fn from(error: String) -> Self {
        Self {
            kind: FinishFailureKind::Other,
            error,
        }
    }
}

impl From<native::OwnedModuleFailure> for FinishFailure {
    fn from(failure: native::OwnedModuleFailure) -> Self {
        match failure {
            native::OwnedModuleFailure::Invalid(error)
            | native::OwnedModuleFailure::TypeConstruction(error) => Self::from(error),
            native::OwnedModuleFailure::RawBufferConstruction(error) => Self {
                kind: FinishFailureKind::RawBufferConstruction,
                error,
            },
            native::OwnedModuleFailure::CfgConstruction(error) => Self::cfg(error),
        }
    }
}

#[cfg(test)]
thread_local! {
    static FINISH_ASSEMBLE_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

fn assemble_finished_module(module: &Module) -> Vec<u8> {
    #[cfg(test)]
    FINISH_ASSEMBLE_COUNT.with(|count| count.set(count.get() + 1));
    module
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect()
}

#[cfg(test)]
fn reset_finish_assemble_count() {
    FINISH_ASSEMBLE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn finish_assemble_count() -> usize {
    FINISH_ASSEMBLE_COUNT.with(std::cell::Cell::get)
}

fn finish_module(
    mut emitted: emit_sidecar::EmittedSpirv,
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    air_data_layout: Option<&layout::AirDataLayout>,
    options: passes::TransformOptions,
    construction: FinishConstruction,
) -> Result<FinishedModule, FinishFailure> {
    emitted.sidecar.air_data_layout = air_data_layout.cloned();
    let retry_debug = env_vars::retry_debug();
    if retry_debug {
        for mapping in &emitted.sidecar.air_struct_layout_mappings {
            match mapping.status {
                emit_sidecar::AirStructLayoutMappingStatus::MappedNatural => {}
                emit_sidecar::AirStructLayoutMappingStatus::MappedExplicit => eprintln!(
                    "[retry-debug] AIR struct layout param={} type={:?}: exact metadata differs from natural layout; using exact offsets",
                    mapping.param_index, mapping.struct_ty
                ),
                emit_sidecar::AirStructLayoutMappingStatus::EmittedIsUntypedBuffer => eprintln!(
                    "[retry-debug] AIR struct layout param={} type={:?}: emitted as an untyped buffer; declared offsets do not apply",
                    mapping.param_index, mapping.struct_ty
                ),
                status => eprintln!(
                    "[retry-debug] AIR struct layout param={} type={:?}: unmapped ({status:?}); using datalayout-derived natural layout",
                    mapping.param_index, mapping.struct_ty
                ),
            }
        }
        eprintln!("[retry-debug] finish: passes start");
    }
    let passes::Transformed {
        module: mut out,
        mut sidecar,
        placeholder_descriptor_bindings,
        ray_instance_user_id_table_binding,
        fragment_sample_positions,
    } = passes::transform_with_options_and_sidecar(
        emitted.module,
        emitted.sidecar,
        stage,
        frag,
        vert,
        kern,
        entry_name,
        options,
    )?;
    native::close_inlined_bda_pointer_tables_module(&mut out);
    b78_tcg::apply_air_access_aligns(&mut out, &sidecar.air_access_aligns);
    let preserved_pointer_facts = sidecar
        .local_pointer_field_stores
        .iter()
        .map(|fact| fact.id)
        .collect::<crate::fxhash::HashSet<_>>();
    if native::lower_unobserved_bda_aggregate_pointer_fields_module(&mut out)? {
        native::eliminate_dead_values_module(&mut out, &preserved_pointer_facts);
    }
    if retry_debug {
        eprintln!("[retry-debug] finish: passes complete; canonicalize start");
    }
    let mut retained_global_ids = sidecar
        .local_pointer_field_stores
        .iter()
        .map(|fact| fact.id)
        .collect::<Vec<_>>();
    passes::canonicalize_ids_and_remap_sidecar(&mut out, &mut retained_global_ids, &mut sidecar);
    if retry_debug {
        eprintln!("[retry-debug] finish: canonicalize complete");
    }
    if construction == FinishConstruction::Primary {
        native::prune_constant_cfg_module_if_changed(&mut out);
    }
    native::prune_unused_null_and_undef_constants_module(&mut out);
    let mut cfg_construction_functions = sidecar.ownership_plan_rejected_functions.clone();
    cfg_construction_functions.extend(
        sidecar
            .post_lowering_cfg_construction_functions
            .iter()
            .cloned(),
    );
    native::construct_cfg_functions_module(&mut out, &cfg_construction_functions)
        .map_err(FinishFailure::cfg)?;
    native::construct_physical_atomic_pointer_lvalues_module(&mut out);
    native::split_mismatched_copy_memory_module(&mut out);
    native::lower_stencil_export_module(&mut out);
    passes::drop_unreferenced_global_variables(&mut out);
    native::prune_unused_null_and_undef_constants_module(&mut out);
    if construction == FinishConstruction::RawRelooper {
        passes::canonicalize_ids(&mut out);
    }
    passes::drop_unreferenced_scalar_types(&mut out);
    passes::drop_unused_scalar_width_capabilities(&mut out);
    passes::drop_unrequired_capabilities(&mut out);
    reflect::decorate_unwritten_descriptors(&mut out);
    let _ = b78_tcg::state_readonly_psb(&mut out, kern, options.descriptor_layout);
    native::decay_short_access_chains_module(&mut out);
    if let Some(failure) = native::owned_module_failure(&out) {
        if let Some(path) = env_vars::retry_dump() {
            let _ = std::fs::write(path, assemble_finished_module(&out));
        }
        return Err(failure.into());
    }
    passes::validate_descriptor_bindings_with_ray_table(
        &out,
        options.descriptor_layout,
        ray_instance_user_id_table_binding,
    )?;
    let declared = spirv_module::descriptor_bindings_in_set(&out, options.descriptor_layout.set);
    let placeholder_descriptor_bindings = placeholder_descriptor_bindings
        .into_iter()
        .filter(|binding| declared.contains(binding))
        .collect::<Vec<_>>();
    let ray_instance_user_id_table_binding =
        ray_instance_user_id_table_binding.filter(|b| declared.contains(b));
    let fragment_sample_positions =
        fragment_sample_positions.is_some() && reflect::has_sample_position_payload(&out);
    let finished = FinishedModule::new(
        out,
        placeholder_descriptor_bindings,
        ray_instance_user_id_table_binding,
        fragment_sample_positions,
    );
    if retry_debug {
        eprintln!("[retry-debug] finish: assembly complete");
    }
    Ok(finished)
}

pub fn translate_raw_tiers_probe(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
) -> Vec<Result<Vec<u8>, String>> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    if let Err(error) = reject_unsupported_metal_linked_functions(san_ll) {
        return vec![Err(error.clone()), Err(error)];
    }
    let stage_meta = parse_stage_meta(san_ll, stage);
    let opts = passes::TransformOptions::default();
    if let Err(error) = passes::validate_kernel_dispatch_options(stage, opts) {
        return vec![Err(error.clone()), Err(error)];
    }
    let air_data_layout = layout::AirDataLayout::from_ir(san_ll);
    let run = |emitted: Result<emit_sidecar::EmittedSpirv, String>| -> Result<Vec<u8>, String> {
        let air_data_layout = air_data_layout.as_ref().map_err(Clone::clone)?;
        emitted.and_then(|b| {
            finish_module(
                b,
                stage,
                stage_meta.frag.as_ref(),
                stage_meta.vert.as_ref(),
                stage_meta.kern.as_ref(),
                stage_meta.entry_name.as_deref(),
                air_data_layout.as_ref(),
                opts,
                FinishConstruction::Plain,
            )
            .map(|finished| finished.bytes)
            .map_err(|failure| failure.error)
        })
    };
    vec![
        run(tools::emit_vulkan_spirv_all_buffers_raw_with_sidecar(
            san_ll,
            tmp,
            stage_meta.kern.as_ref(),
            stage_meta.entry_name.as_deref(),
            stage_buffer_layouts(
                stage,
                stage_meta.frag.as_ref(),
                stage_meta.vert.as_ref(),
                stage_meta.kern.as_ref(),
            ),
            &Default::default(),
            &Default::default(),
        )),
        run(
            tools::emit_vulkan_spirv_all_buffers_raw_with_workgroup_sidecar(
                san_ll,
                tmp,
                stage_meta.kern.as_ref(),
                stage_meta.entry_name.as_deref(),
                stage_buffer_layouts(
                    stage,
                    stage_meta.frag.as_ref(),
                    stage_meta.vert.as_ref(),
                    stage_meta.kern.as_ref(),
                ),
                &Default::default(),
                &Default::default(),
            ),
        ),
    ]
}

pub fn translate_bda_probe(
    san_ll: &str,
    stage: passes::Stage,
    tmp: &Path,
) -> Result<Vec<u8>, String> {
    let lowered = lower_air_text_if_enabled(san_ll);
    let san_ll = lowered.as_ref();
    reject_unsupported_metal_linked_functions(san_ll)?;
    let stage_meta = parse_stage_meta(san_ll, stage);
    let opts = passes::TransformOptions::default();
    passes::validate_kernel_dispatch_options(stage, opts)?;
    let air_data_layout = layout::AirDataLayout::from_ir(san_ll)?;
    tools::emit_vulkan_spirv_all_buffers_raw_bda_with_sidecar(
        san_ll,
        tmp,
        stage_meta.kern.as_ref(),
        stage_meta.entry_name.as_deref(),
        stage_buffer_layouts(
            stage,
            stage_meta.frag.as_ref(),
            stage_meta.vert.as_ref(),
            stage_meta.kern.as_ref(),
        ),
        &Default::default(),
        &Default::default(),
    )
    .and_then(|b| {
        finish_module(
            b,
            stage,
            stage_meta.frag.as_ref(),
            stage_meta.vert.as_ref(),
            stage_meta.kern.as_ref(),
            stage_meta.entry_name.as_deref(),
            air_data_layout.as_ref(),
            opts,
            FinishConstruction::Plain,
        )
        .map(|finished| finished.bytes)
        .map_err(|failure| failure.error)
    })
}

fn translate_sanitized_with_meta(
    san_ll: &str,
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    tmp: &Path,
    options: passes::TransformOptions,
    datalayout: Option<layout::AirDataLayout>,
) -> Result<Vec<u8>, String> {
    passes::validate_kernel_dispatch_options(stage, options)?;
    translate_sanitized_with_meta_prevalidated_carrier(
        san_ll, stage, frag, vert, kern, entry_name, tmp, options, datalayout,
    )
    .map(|finished| finished.bytes)
}

fn translate_sanitized_with_meta_prevalidated_carrier(
    san_ll: &str,
    stage: passes::Stage,
    frag: Option<&meta::FragMeta>,
    vert: Option<&meta::VertMeta>,
    kern: Option<&meta::KernMeta>,
    entry_name: Option<&str>,
    tmp: &Path,
    options: passes::TransformOptions,
    datalayout: Option<layout::AirDataLayout>,
) -> Result<FinishedModule, String> {
    let retry_debug_on = env_vars::retry_debug();
    if retry_debug_on {
        eprintln!("[retry-debug] translate: construction context start");
    }
    let rc = construction::ConstructionCtx::new(
        san_ll, stage, frag, vert, kern, entry_name, tmp, options, datalayout,
    );
    if retry_debug_on {
        eprintln!("[retry-debug] translate: primary emission start");
    }
    let primary_emitted = tools::emit_vulkan_spirv_with_outcome(
        san_ll,
        tmp,
        rc.kern,
        rc.entry_name,
        stage_buffer_layouts(rc.stage, rc.frag, rc.vert, rc.kern),
    );
    let primary_finished = match primary_emitted {
        Ok(emitted) => {
            rc.remember_ordinary_plan_rejections(&emitted);
            rc.finish_primary_carrier(emitted)
        }
        Err(failure) => {
            rc.remember_raw_buffer_layout_rejection(failure.rejected.raw_buffer_layout_required);
            rc.remember_ordinary_plan_rejection_set(&failure.rejected.ordinary_plan_functions);
            rc.remember_ownership_plan_rejection_set(&failure.rejected.ownership_plan_functions);
            Err(failure.error)
        }
    };
    let translated = match primary_finished {
        Ok(finished) => Ok(finished),
        Err(emit_err) if rc.needs_raw_construction() => {
            let constructed = rc.construct_raw().map_err(|construction_error| {
                format!("{emit_err}; raw construction failed: {construction_error}")
            })?;
            Ok(constructed)
        }
        Err(emit_err) => Err(emit_err),
    }
    .and_then(|constructed| {
        if let Some(path) = env_vars::retry_dump() {
            let _ = std::fs::write(path, &constructed.bytes);
        }
        tools::spirv_val_bytes(&constructed.bytes, tmp)?;
        reject_function_constant_erased_effects(san_ll, &constructed.module)?;
        reject_imageblock_only_effects(san_ll, &constructed.module)?;
        if retry_debug_on {
            eprintln!("[retry-debug] constructed module validated in-translate");
        }
        Ok(constructed)
    });
    if crate::env_vars::tier_census() {
        let label = match &translated {
            Ok(_) => "default",
            Err(_) => "fallback",
        };
        eprintln!("[tier-census] {label}");
    }
    translated
}

pub fn canonicalize_spirv_bytes(spv: &[u8]) -> Result<Vec<u8>, String> {
    let mut module = load_owned_module(spv).map_err(|e| format!("SPIR-V load: {e:?}"))?;
    passes::canonicalize_ids(&mut module);
    Ok(module
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect())
}

pub use passes::loop_budget::{LoopBudgetReport, DEFAULT_LOOP_BUDGET};

pub fn instrument_spirv_loop_budget(
    spv: &[u8],
    budget: u32,
) -> Result<(Vec<u8>, LoopBudgetReport), String> {
    let mut module = load_owned_module(spv).map_err(|e| format!("SPIR-V load: {e:?}"))?;
    let report = passes::loop_budget::instrument_loop_budget(&mut module, budget);
    let bytes: Vec<u8> = module
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    if report.needs_revalidation() {
        tools::spirv_val_bytes(&bytes, &std::env::temp_dir()).map_err(|error| {
            format!(
                "loop budget rewrote {} loop header(s) to return on exhaustion and the result does \
                 not validate, so the module is not safe to dispatch: {error}",
                report.loops_bounded_via_early_return
            )
        })?;
    }
    Ok((bytes, report))
}

pub fn disassemble(spv: &[u8]) -> Result<String, String> {
    let m = load_owned_module(spv).map_err(|e| format!("SPIR-V load: {e:?}"))?;
    Ok(m.disassemble())
}

#[cfg(test)]
mod single_meta_parse_tests {
    use super::*;

    #[test]
    fn specialized_reflection_includes_function_constant_gated_argument_buffer() {
        let ll = r#"
@enabled.MTL_FC_INIT_7_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@enabled_pred = internal addrspace(2) global i8 0, align 1

declare i1 @air.is_function_constant_defined(ptr addrspace(2))

define internal void @_GLOBAL__sub_I_enabled() section "air.static_init" {
  %value = load i8, ptr addrspace(2) @enabled.MTL_FC_INIT_7_b
  %defined = call i1 @air.is_function_constant_defined(ptr addrspace(2) @enabled.MTL_FC_INIT_7_b)
  %set = icmp ne i8 %value, 0
  %selected = select i1 %defined, i1 %set, i1 false
  %byte = zext i1 %selected to i8
  store i8 %byte, ptr addrspace(2) @enabled_pred
  ret void
}

define void @k(ptr addrspace(2) %args) {
  ret void
}

!air.vertex = !{!0}
!air.function_constants = !{!8}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.function_constant", !4, !"air.indirect_buffer", !"air.location_index", i32 30, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"Args"}
!4 = !{ptr addrspace(2) @enabled_pred, !"bool", !"enabled"}
!5 = !{i32 0, i32 8, i32 0, !"void", !"data", !"air.indirect_argument", !6}
!6 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"void"}
!8 = !{ptr addrspace(2) @enabled.MTL_FC_INIT_7_b, !"bool", !"enabled", i32 7, i1 false}
"#;

        let default = reflect_sanitized(
            ll,
            passes::Stage::Vertex,
            passes::TransformOptions::default(),
        )
        .expect("reflect default");
        assert!(!default
            .bindings
            .iter()
            .any(|binding| binding.metal_index == 30));

        let disabled = reflect_sanitized_specialized(
            ll,
            passes::Stage::Vertex,
            passes::TransformOptions::default(),
            &[(7, vec![0])],
        )
        .expect("reflect explicitly disabled");
        assert!(!disabled
            .bindings
            .iter()
            .any(|binding| binding.metal_index == 30));

        let enabled = reflect_sanitized_specialized(
            ll,
            passes::Stage::Vertex,
            passes::TransformOptions::default(),
            &[(7, vec![1])],
        )
        .expect("reflect enabled");
        assert!(enabled.bindings.iter().any(|binding| {
            binding.kind == reflect::ResourceKind::Buffer && binding.metal_index == 30
        }));
        assert!(enabled.bindings.iter().any(|binding| {
            binding.kind == reflect::ResourceKind::EmbeddedArgBufferBuffer
                && binding
                    .embedded_source
                    .is_some_and(|source| source.buffer_index == 30 && source.field_offset == 0)
        }));
    }

    #[test]
    fn metadata_only_reflection_reports_the_address_table_a_device_pointer_needs() {
        let ll = r#"
define void @k(ptr addrspace(1) %out, i64 %address) {
  %p = inttoptr i64 %address to ptr addrspace(1)
  %v = load i32, ptr addrspace(1) %p, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1}
!4 = !{i32 1, !"air.thread_position_in_grid"}
"#;
        let reflection = reflect_sanitized(
            ll,
            passes::Stage::Kernel,
            passes::TransformOptions::default(),
        )
        .expect("reflect device-address kernel");
        let table = reflection
            .bindings
            .iter()
            .find(|binding| binding.kind == reflect::ResourceKind::BufferAddressTable)
            .expect("buffer-address table reflection");
        assert_eq!(
            table.descriptor.map(|descriptor| descriptor.binding),
            Some(reflect::SYNTHETIC_BINDING_BASE)
        );

        let dead = ll.replace(
            "  %v = load i32, ptr addrspace(1) %p, align 4\n  store i32 %v, ptr addrspace(1) %out, align 4\n",
            "",
        );
        assert!(!reflect_sanitized(
            &dead,
            passes::Stage::Kernel,
            passes::TransformOptions::default(),
        )
        .expect("reflect dead-pointer kernel")
        .bindings
        .iter()
        .any(|binding| binding.kind == reflect::ResourceKind::BufferAddressTable));
    }

    #[test]
    fn metadata_only_reflection_sees_a_device_pointer_a_text_scan_cannot() {
        let ll = r#"
%struct.Handles = type { ptr addrspace(1), i32 }

define void @k(ptr addrspace(1) %handles, ptr addrspace(1) %out) {
entry:
  %slot = getelementptr inbounds %struct.Handles, ptr addrspace(1) %handles, i64 0, i32 0
  %device = load ptr addrspace(1), ptr addrspace(1) %slot, align 8
  %value = load i32, ptr addrspace(1) %device, align 4
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"Handles", !"air.arg_name", !"handles"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
        let metadata_only = reflect_sanitized(
            ll,
            passes::Stage::Kernel,
            passes::TransformOptions::default(),
        )
        .expect("reflect device-address kernel");
        assert!(
            metadata_only
                .bindings
                .iter()
                .any(|binding| binding.kind == reflect::ResourceKind::BufferAddressTable),
            "{:?}",
            metadata_only
                .bindings
                .iter()
                .map(|binding| binding.kind)
                .collect::<Vec<_>>()
        );

        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_metadata_address_table_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let (_, translated) = translate_sanitized_native_reflected(
            ll,
            passes::Stage::Kernel,
            &tmp,
            passes::TransformOptions::default(),
        )
        .expect("translate device-address kernel");
        let _ = std::fs::remove_dir_all(&tmp);
        let table_bindings = |reflection: &reflect::ShaderReflection| {
            reflection
                .bindings
                .iter()
                .filter(|binding| binding.kind == reflect::ResourceKind::BufferAddressTable)
                .filter_map(|binding| binding.descriptor.map(|descriptor| descriptor.binding))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            table_bindings(&metadata_only),
            table_bindings(&translated),
            "metadata-only and reflected translation must agree on the address table"
        );
    }

    const SIMPLE_KERNEL: &str = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  store i32 7, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;

    #[test]
    fn production_kernel_emit_reuses_one_stage_meta_parse() {
        meta::reset_air_meta_parse_count();
        let stage_meta = parse_stage_meta(SIMPLE_KERNEL, passes::Stage::Kernel);
        tools::emit_vulkan_spirv_with_sidecar(
            SIMPLE_KERNEL,
            Path::new(""),
            stage_meta.kern.as_ref(),
            stage_meta.entry_name.as_deref(),
            stage_buffer_layouts(
                passes::Stage::Kernel,
                stage_meta.frag.as_ref(),
                stage_meta.vert.as_ref(),
                stage_meta.kern.as_ref(),
            ),
        )
        .expect("production emitter consumes threaded metadata");

        assert_eq!(meta::air_meta_parse_count(), 1);
    }

    #[test]
    fn owned_type_failure_cannot_select_alternate_construction() {
        let failure = FinishFailure::from(native::OwnedModuleFailure::TypeConstruction(
            "invalid owned type graph".to_string(),
        ));
        assert_eq!(failure.kind, FinishFailureKind::Other);
        assert_eq!(failure.error, "invalid owned type graph");
    }

    #[test]
    fn validating_primary_finish_assembles_once() {
        let tmp =
            std::env::temp_dir().join(format!("metal2vulkan_finish_once_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        reset_finish_assemble_count();
        spirv_module::reset_load_bytes_count();
        let spv = translate_sanitized_native(SIMPLE_KERNEL, passes::Stage::Kernel, &tmp)
            .expect("simple primary translation validates");
        tools::spirv_val_bytes(&spv, &tmp).expect("simple primary spirv-val");
        assert_eq!(
            finish_assemble_count(),
            1,
            "a validating primary must not assemble fallback bytes"
        );
        assert_eq!(
            spirv_module::load_bytes_count(),
            0,
            "production translation must not parse its serialized output for repair"
        );
    }

    #[test]
    fn selected_raw_relooper_finish_assembles_once() {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_raw_relooper_finish_once_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let stage_meta = parse_stage_meta(SIMPLE_KERNEL, passes::Stage::Kernel);
        let construction = construction::ConstructionCtx::new(
            SIMPLE_KERNEL,
            passes::Stage::Kernel,
            stage_meta.frag.as_ref(),
            stage_meta.vert.as_ref(),
            stage_meta.kern.as_ref(),
            stage_meta.entry_name.as_deref(),
            &tmp,
            passes::TransformOptions::default(),
            layout::AirDataLayout::from_ir(SIMPLE_KERNEL).expect("AIR datalayout"),
        );
        reset_finish_assemble_count();
        spirv_module::reset_load_bytes_count();
        native::reset_address_construction_counts();

        let finished = construction
            .construct_raw_relooper()
            .expect("raw relooper construction");
        tools::spirv_val_bytes(&finished.bytes, &tmp).expect("raw relooper spirv-val");
        assert_eq!(
            finish_assemble_count(),
            1,
            "the selected raw-relooper representation must have no intermediate assembly"
        );
        assert_eq!(
            spirv_module::load_bytes_count(),
            0,
            "raw-relooper construction must remain owned through final assembly"
        );
        assert_eq!(
            native::address_construction_count(),
            1,
            "raw-relooper address closure must be owned by interface construction"
        );
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn reflected_translation_retains_the_finished_owned_module() {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_reflected_owned_module_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        spirv_module::reset_load_bytes_count();

        let (spv, reflection) = translate_sanitized_native_reflected(
            SIMPLE_KERNEL,
            passes::Stage::Kernel,
            &tmp,
            passes::TransformOptions::default(),
        )
        .expect("simple reflected translation validates");

        assert!(!spv.is_empty());
        assert!(reflection.bindings.iter().any(|binding| {
            binding.kind == reflect::ResourceKind::Buffer && binding.footprint.is_some()
        }));
        assert_eq!(
            spirv_module::load_bytes_count(),
            0,
            "reflection must analyze the finished owned module without parsing output bytes"
        );
    }
}

/// The retry form of an AIR module that failed as written: every internal call inlined and pointer fields of local objects
/// forwarded (see native::inline). None when it would not change the module.
pub fn inline_and_forward_object_fields(ll: &str) -> Option<String> {
    native::inline_and_forward_object_fields(ll)
}

thread_local! {
    static ARGBUF_SAMPLERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with samplers held in argument buffers read through the sampler heap (the plugin's binding 1), as the retry of
/// a module that failed without it does. Off otherwise: on by default it took over sampler operands three app modules
/// already translated another way (measured on a 12,446-shader corpus).
pub fn with_argument_buffer_samplers<R>(f: impl FnOnce() -> R) -> R {
    ARGBUF_SAMPLERS.with(|c| c.set(true));
    RAW_BUFFERS_FIRST.with(|c| c.set(true));
    let r = f();
    ARGBUF_SAMPLERS.with(|c| c.set(false));
    RAW_BUFFERS_FIRST.with(|c| c.set(false));
    r
}

thread_local! {
    static RAW_BUFFERS_FIRST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Inside the inline retry (with_argument_buffer_samplers), every buffer is built raw before the typed construction is
/// tried; outside it the typed construction comes first, as it always has.
pub(crate) fn raw_buffers_first_on() -> bool {
    RAW_BUFFERS_FIRST.with(|c| c.get())
}

pub(crate) fn argument_buffer_samplers_on() -> bool {
    ARGBUF_SAMPLERS.with(|c| c.get())
        || matches!(std::env::var("NVMTL_ARGBUF_SAMPLERS").as_deref(), Ok("1"))
}
