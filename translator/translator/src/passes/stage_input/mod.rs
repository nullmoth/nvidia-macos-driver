use super::*;
use crate::meta::{primitive_air_type_from_name, ExecutionGroupFact};
use crate::passes::access::{is_unsigned_byte_scalar, single_member_array_scalar_elem};
use crate::passes::air_calls::subgroup_local_invocation_id_input_var;
use crate::passes::stage_output::handle_static_sampler;
mod decorations;
mod kernel_grid;
mod kernel_values;
mod layout;

pub(in crate::passes) use decorations::*;
pub(in crate::passes) use kernel_grid::{
    bind_kernel_grid_push_constant_once, load_kernel_dispatch_component,
    materialize_kernel_dispatch_field,
};
pub(in crate::passes) use kernel_values::const_ivec;
use kernel_values::{
    bind_kernel_uvec3_builtin, bind_kernel_uvec3_builtin_var, const_kernel_local_size,
    is_raw_uint_buffer_block,
};
use layout::*;

pub(in crate::passes) use layout::{
    decorate_block_struct, drop_unconsumed_placeholder_descriptor_loads, layout_ty_size_align,
    round_up,
};

mod air_layout;
pub(in crate::passes) use air_layout::*;
const DEFAULT_WORKGROUP_MEMORY_ELEMENTS: u32 = 512;

const MAX_WORKGROUP_MEMORY_BYTES: u32 = 32768;

fn workgroup_array_elements(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    element_ty: Word,
    metal_index: Option<u32>,
    param: Word,
    default_elements: u32,
) -> Result<u32, String> {
    let mut defs = defs.clone();
    for instruction in &ctx.new_globals {
        if let Some(id) = instruction.result_id {
            defs.entry(id).or_insert_with(|| instruction.clone());
        }
    }
    let (element_size, _) = layout_ty_size_align(ctx, element_ty, &defs);
    let Some(length) = metal_index
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| ctx.threadgroup_memory_lengths.get(index))
        .copied()
        .flatten()
    else {
        if element_size == 0 {
            return Ok(default_elements);
        }
        let affordable = (MAX_WORKGROUP_MEMORY_BYTES / element_size).max(1);
        return Ok(default_elements.min(affordable));
    };
    if element_size == 0 {
        return Err(format!(
            "threadgroup param {param} has a zero-sized element type, so its {length}-byte binding names no elements"
        ));
    }
    let elements = length / element_size;
    if elements == 0 {
        return Err(format!(
            "threadgroup param {param} is bound with {length} bytes, which is less than one {element_size}-byte element"
        ));
    }
    Ok(elements)
}

pub(in crate::passes) fn fragment_imageblock_projection_type_matches(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    format: FragmentImageblockFormat,
) -> bool {
    let scalar = if format.lanes == 1 {
        ty
    } else {
        let Some(definition) = defs.get(&ty) else {
            return false;
        };
        match definition.operands.as_slice() {
            [Operand::IdRef(scalar), Operand::LiteralBit32(lanes)]
                if definition.class.opcode == Op::TypeVector && *lanes == format.lanes =>
            {
                *scalar
            }
            _ => return false,
        }
    };
    let Some(definition) = defs.get(&scalar) else {
        return false;
    };
    match format.component {
        ImageComp::Float => {
            definition.class.opcode == Op::TypeFloat
                && definition.operands.as_slice() == [Operand::LiteralBit32(format.bits)]
        }
        ImageComp::Uint => {
            definition.class.opcode == Op::TypeInt
                && definition.operands.as_slice()
                    == [Operand::LiteralBit32(format.bits), Operand::LiteralBit32(0)]
        }
        ImageComp::Sint => false,
    }
}

pub(in crate::passes) enum ParamBinding {
    HeapImage {
        slot: u32,
        image_ty: Word,
        dim: (Dim, bool),
        comp: ImageComp,
        multisampled: bool,
    },
    LoadVar { var: Word, ty: Word },
    LoadVarBoolFromUint { var: Word, bool_ty: Word },
    LoadVarBoolToInt {
        var: Word,
        bool_ty: Word,
        int_ty: Word,
    },
    LoadVarConverted {
        var: Word,
        load_ty: Word,
        param_ty: Word,
    },
    LoadVarBitcast {
        var: Word,
        load_ty: Word,
        param_ty: Word,
    },
    LoadVarBitAnd {
        var: Word,
        load_ty: Word,
        param_ty: Word,
        mask: u32,
    },
    LoadVarShiftRight {
        var: Word,
        load_ty: Word,
        param_ty: Word,
        shift: u32,
    },
    LoadVarComponent {
        var: Word,
        vec_ty: Word,
        scalar_ty: Word,
        out_ty: Word,
        comp: u32,
    },
    LoadVarVectorPrefix {
        var: Word,
        vec_ty: Word,
        scalar_ty: Word,
        prefix_ty: Word,
        out_ty: Word,
        lanes: u32,
    },
    LoadThreadsPerGrid {
        var: Word,
        vec_ty: Word,
        out_ty: Word,
        lanes: u32,
    },
    LoadKernelDispatchField {
        var: Word,
        first_member: u32,
        out_ty: Word,
        lanes: u32,
    },
    LoadBuiltinPlusKernelDispatchField {
        builtin_var: Word,
        dispatch_var: Word,
        first_member: u32,
        out_ty: Word,
        lanes: u32,
    },
    LoadKernelLocalSize { out_ty: Word, lanes: u32 },
    LoadKernelRequestedLocalSize { out_ty: Word, lanes: u32 },
    LoadKernelGroupsPerThreadgroup { out_ty: Word, lanes: u32 },
    Image {
        var: Word,
        image_ty: Word,
        dim: (Dim, bool),
        comp: ImageComp,
        multisampled: bool,
    },
    ImageArray {
        var: Word,
        elem_image_ty: Word,
        dim: (Dim, bool),
        comp: ImageComp,
        multisampled: bool,
        runtime_specialization: Option<(u32, crate::reflect::RuntimeStorageImageState)>,
    },
    StorageImage {
        var: Word,
        image_ty: Word,
        dim: (Dim, bool),
        comp: ImageComp,
        runtime_specialization: Option<(u32, crate::reflect::RuntimeStorageImageState)>,
    },
    InputAttachment {
        var: Word,
        image_ty: Word,
        read_ty: Word,
        param_ty: Word,
    },
    FragmentImageblockProjection {
        coord_var: Word,
        param_ty: Word,
        members: Vec<(Word, Word, Word, FragmentImageblockFormat)>,
    },
    Sampler {
        var: Word,
        specialized_state: Option<StaticSamplerState>,
    },
    Buffer { var: Word, wrap: BufWrap },
    StageInput {
        var: Word,
        value_ty: Word,
        index_var: Word,
        dispatch_var: Option<Word>,
    },
    WorkgroupMemory { var: Word },
    Value { val: Word },
    ZeroValue { val: Word },
    ZeroPointer { var: Word },
}

fn declared_descriptor_counts(
    stage: Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
) -> Result<(), String> {
    let counts = match stage {
        Stage::Fragment => frag.map(|meta| &meta.declared_descriptor_counts),
        Stage::Vertex => vert.map(|meta| &meta.declared_descriptor_counts),
        Stage::Kernel => kern.map(|meta| &meta.declared_descriptor_counts),
    };
    let Some(counts) = counts else {
        return Ok(());
    };
    let mut declared = counts.iter().collect::<Vec<_>>();
    declared.sort();
    for (&idx, &count) in declared {
        let is_sampler = match stage {
            Stage::Fragment => matches!(
                frag.and_then(|m| m.role_of(idx)),
                Some(FragRole::Sampler(_))
            ),
            Stage::Vertex => matches!(
                vert.and_then(|m| m.role_of(idx)),
                Some(VertRole::Sampler(_))
            ),
            Stage::Kernel => matches!(
                kern.and_then(|m| m.role_of(idx)),
                Some(KernRole::Sampler(_))
            ),
        };
        if is_sampler {
            return Err(format!(
                "entry parameter {idx} is an array of {count} samplers, which this translator has \
                 no descriptor-array lowering for; emitting the module would sample with a \
                 synthesized default sampler instead of the one the shader selected"
            ));
        }
        let texture_type_name = match stage {
            Stage::Fragment => frag.and_then(|m| m.texture_type_name(idx)),
            Stage::Vertex => vert.and_then(|m| m.texture_type_name(idx)),
            Stage::Kernel => kern.and_then(|m| m.texture_type_name(idx)),
        };
        let Some(name) = texture_type_name else {
            continue;
        };
        if crate::meta::texture_shape_from_name(name).array_length != Some(count) {
            return Err(format!(
                "entry parameter {idx} occupies {count} descriptors per `air.location_index`, \
                 which its type name `{name}` does not state; emitting the module would size the \
                 descriptor array from a type name the ABI contradicts"
            ));
        }
    }
    Ok(())
}

pub(in crate::passes) enum BufWrap {
    Direct,
    RecordArray { block_ty: Word, elem_ty: Word },
    Collapsed {
        block_ty: Word,
        prepend_member0: bool,
        typed_aliases: Vec<(Word, Word)>,
    },
}

fn entry_parameters_read_by_the_body(module: &Module) -> HashSet<Word> {
    module
        .functions
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
        .flat_map(|instruction| instruction.operands.iter())
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn required_resource_binding(param: Word, binding: Option<u32>) -> Result<u32, String> {
    binding.ok_or_else(|| {
        format!("descriptor-backed entry parameter %{param} has no AIR descriptor ABI binding")
    })
}

pub(super) fn build_stage_input(
    ctx: &mut Ctx,
    entry_idx: usize,
    stage: &Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
) -> Result<HashMap<Word, Instruction>, String> {
    let descriptor_layout = ctx.descriptor_layout;
    let defs = type_defs(&ctx.module);
    let params: Vec<(Word, Word)> = ctx.module.functions[entry_idx]
        .parameters
        .iter()
        .map(|p| {
            Ok((
                p.result_id.ok_or("entry param missing result id")?,
                p.result_type.ok_or("entry param missing result type")?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let texture_type_hints = texture_type_hints(&params, stage, frag, vert, kern);
    let tex_dims = texture_dims(ctx, entry_idx, &texture_type_hints);
    let mut wtex_candidates = texture_storage_hints(&params, stage, frag, vert, kern);
    for (pid, shape) in write_texture_dims(ctx, entry_idx) {
        if matches!(shape.2, ImageFormat::R32ui | ImageFormat::R32i) {
            wtex_candidates.insert(pid, shape);
        } else {
            wtex_candidates.entry(pid).or_insert(shape);
        }
    }
    let sampled_required = sampled_binding_required_operands(ctx, entry_idx);
    let wtex_dims: HashMap<Word, (Dim, bool, ImageFormat, ImageComp)> = wtex_candidates
        .into_iter()
        .filter(|(pid, _)| !sampled_required.contains(pid))
        .collect();

    let mut bindings: Vec<(Word, ParamBinding)> = vec![];
    let mut buffer_structs: Vec<(Word, Word)> = vec![];
    let mut fragcoord_var: Option<Word> = None;
    let mut pointcoord_var: Option<Word> = None;
    let mut front_facing_var: Option<Word> = None;
    let mut primitive_id_var: Option<Word> = None;
    let mut sample_id_var: Option<Word> = None;
    let mut layer_var: Option<Word> = None;
    let mut sample_mask_in_var: Option<Word> = None;
    let mut bary_coord_var: Option<Word> = None;
    let mut bary_coord_no_persp_var: Option<Word> = None;
    let mut tess_coord_var: Option<Word> = None;
    let mut local_invocation_index_var: Option<Word> = None;
    let mut num_workgroups_var: Option<Word> = None;
    let mut global_invocation_id_var: Option<Word> = None;
    let mut kernel_grid_push_constant_var: Option<Word> = None;
    if let Some(range) = ctx.kernel_dispatch.push_constant_range() {
        bind_kernel_grid_push_constant_once(ctx, &mut kernel_grid_push_constant_var, range.offset);
    }
    let stage_input_bindings = kern.map(KernMeta::stage_input_bindings).unwrap_or_default();

    let unmodelled = match stage {
        Stage::Fragment => frag.map(|meta| meta.unmodelled_input_params.as_slice()),
        Stage::Vertex => vert.map(|meta| meta.unmodelled_input_params.as_slice()),
        Stage::Kernel => kern.map(|meta| meta.unmodelled_input_params.as_slice()),
    }
    .unwrap_or_default();
    let read_params = entry_parameters_read_by_the_body(&ctx.module);
    if let Some((param, role)) = unmodelled.iter().find(|(param, _)| {
        params
            .get(*param as usize)
            .is_some_and(|(pid, _)| read_params.contains(pid))
    }) {
        return Err(format!(
            "entry parameter {param} declares AIR role `air.{role}`, which has no lowering; \
             emitting the module would silently read a zero in its place"
        ));
    }

    declared_descriptor_counts(*stage, frag, vert, kern)?;

    if let Some(reason) = vert.and_then(|meta| meta.undecoded_patch_shape.as_deref()) {
        return Err(format!(
            "entry declares a tessellation patch this translator cannot read ({reason}); \
             emitting the module would silently drop the tessellation stage"
        ));
    }

    let unmodelled_attributes = match stage {
        Stage::Fragment => frag.map(|meta| meta.unmodelled_stage_attributes.as_slice()),
        Stage::Vertex => vert.map(|meta| meta.unmodelled_stage_attributes.as_slice()),
        Stage::Kernel => kern.map(|meta| meta.unmodelled_stage_attributes.as_slice()),
    }
    .unwrap_or_default();
    if let Some(attribute) = unmodelled_attributes.first() {
        return Err(format!(
            "entry declares stage attribute `{attribute}`, which this translator does not model; \
             emitting the module would silently drop what it asks of the stage"
        ));
    }

    if let Some(meta) = kern {
        for param in &meta.aliased_implicit_imageblock_params {
            let Some(layout) = meta.imageblock_layouts.get(param) else {
                return Err(format!(
                    "entry parameter {param} is an imageblock aliased onto the implicit \
                     imageblock and declares no member layout, so no render-target plane can be \
                     matched to it"
                ));
            };
            match crate::meta::aliased_imageblock_planes(layout) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Err(format!(
                        "entry parameter {param} is an imageblock aliased onto the implicit \
                         imageblock and declares no members, so no render-target plane can be \
                         matched to it"
                    ))
                }
                Err(reason) => {
                    return Err(format!(
                        "entry parameter {param} is an imageblock aliased onto the implicit \
                         imageblock: {reason}"
                    ))
                }
            }
        }
    }

    if let Some(meta) = frag.filter(|meta| meta.early_fragment_tests) {
        for (members, role) in [
            (&meta.depth_members, "air.depth"),
            (&meta.stencil_members, "air.stencil"),
        ] {
            if !members.is_empty() {
                return Err(format!(
                    "entry declares `early_fragment_tests` and writes `{role}`; the test runs \
                     before the value the shader computes for it exists"
                ));
            }
        }
    }

    for (i, (pid, pty)) in params.iter().enumerate() {
        let idx = i as u32;
        let role_is = |s: &str| match stage {
            Stage::Fragment => match frag.and_then(|m| m.role_of(idx)) {
                Some(FragRole::Position) => s == "position",
                Some(FragRole::PointCoord) => s == "point_coord",
                Some(FragRole::FrontFacing) => s == "front_facing",
                Some(FragRole::BarycentricCoord { .. }) => s == "barycentric_coord",
                Some(FragRole::PrimitiveId) => s == "primitive_id",
                Some(FragRole::SampleId) => s == "sample_id",
                Some(FragRole::SampleMaskIn) => s == "sample_mask_in",
                Some(FragRole::ViewportArrayIndex) => s == "viewport_array_index",
                Some(FragRole::RenderTargetArrayIndex) => s == "render_target_array_index",
                Some(FragRole::AmplificationId) => s == "amplification_id",
                Some(FragRole::AmplificationCount) => s == "amplification_count",
                Some(FragRole::Varying(_)) => s == "varying",
                Some(FragRole::Texture(_)) => s == "texture",
                Some(FragRole::Sampler(_)) => s == "sampler",
                Some(FragRole::Buffer(_) | FragRole::AccelerationStructureShadow(_)) => {
                    s == "buffer"
                }
                Some(FragRole::ColorInput(_)) => s == "color_input",
                Some(FragRole::ImageblockData) => s == "imageblock_data",
                Some(FragRole::VariantAbsentTexture) => s == "variant_absent_texture",
                _ => s == "other",
            },
            Stage::Vertex => match vert.and_then(|m| m.role_of(idx)) {
                Some(VertRole::VertexInput(_)) => s == "varying",
                Some(VertRole::Buffer(_) | VertRole::AccelerationStructureShadow(_)) => {
                    s == "buffer"
                }
                Some(VertRole::Texture(_)) => s == "texture",
                Some(VertRole::Sampler(_)) => s == "sampler",
                Some(VertRole::VertexId) => s == "vertex_id",
                Some(VertRole::InstanceId) => s == "instance_id",
                Some(VertRole::BaseVertex) => s == "base_vertex",
                Some(VertRole::BaseInstance) => s == "base_instance",
                Some(VertRole::PatchControlPoints) => s == "patch_control_points",
                Some(VertRole::PatchInput(_)) => s == "patch_input",
                Some(VertRole::PositionInPatch) => s == "position_in_patch",
                Some(VertRole::PatchId) => s == "patch_id",
                Some(VertRole::AmplificationId) => s == "amplification_id",
                Some(VertRole::AmplificationCount) => s == "amplification_count",
                Some(VertRole::VariantAbsentTexture) => s == "variant_absent_texture",
                _ => s == "other",
            },
            Stage::Kernel => match kern.and_then(|m| m.role_of(idx)) {
                Some(
                    KernRole::Buffer(_)
                    | KernRole::AccelerationStructureShadow(_)
                    | KernRole::PrimitiveAccelerationStructureShadow(_),
                ) => s == "buffer",
                Some(KernRole::Texture(_)) => s == "texture",
                Some(KernRole::Sampler(_)) => s == "sampler",
                Some(KernRole::ThreadsPerThreadgroup) => s == "threads_per_threadgroup",
                Some(KernRole::DispatchThreadsPerThreadgroup) => {
                    s == "dispatch_threads_per_threadgroup"
                }
                Some(KernRole::ThreadPositionInThreadgroup) => {
                    s == "thread_position_in_threadgroup"
                }
                Some(KernRole::ThreadgroupsPerGrid) => s == "threadgroups_per_grid",
                Some(KernRole::ThreadsPerGrid) => s == "threads_per_grid",
                Some(KernRole::ThreadgroupPositionInGrid) => s == "threadgroup_position_in_grid",
                Some(KernRole::ThreadIndexInThreadgroup) => s == "thread_index_in_threadgroup",
                Some(KernRole::ThreadPositionInGrid) => s == "thread_position_in_grid",
                Some(KernRole::StageInput(_)) => s == "stage_in",
                Some(KernRole::VariantAbsentTexture) => s == "variant_absent_texture",
                _ => s == "other",
            },
        };
        let execution_group = match stage {
            Stage::Fragment => match frag.and_then(|meta| meta.role_of(idx)) {
                Some(FragRole::ExecutionGroup { fact, lanes }) => Some((*fact, *lanes)),
                _ => None,
            },
            Stage::Vertex => match vert.and_then(|meta| meta.role_of(idx)) {
                Some(VertRole::ExecutionGroup { fact, lanes }) => Some((*fact, *lanes)),
                _ => None,
            },
            Stage::Kernel => match kern.and_then(|meta| meta.role_of(idx)) {
                Some(KernRole::ExecutionGroup { fact, lanes }) => Some((*fact, *lanes)),
                _ => None,
            },
        };
        let loc = match stage {
            Stage::Fragment => match frag.and_then(|m| m.role_of(idx)) {
                Some(FragRole::Varying(l)) => *l,
                _ => 0,
            },
            Stage::Vertex => match vert.and_then(|m| m.role_of(idx)) {
                Some(VertRole::VertexInput(l)) => *l,
                _ => 0,
            },
            Stage::Kernel => 0,
        };
        let resource_binding = match stage {
            Stage::Fragment => match frag.and_then(|m| m.role_of(idx)) {
                Some(FragRole::Texture(b)) => {
                    Some(texture_resource_binding(descriptor_layout, *b)?)
                }
                Some(FragRole::Sampler(b)) => {
                    Some(sampler_resource_binding(descriptor_layout, *b)?)
                }
                Some(FragRole::Buffer(b) | FragRole::AccelerationStructureShadow(b)) => {
                    Some(buffer_resource_binding(descriptor_layout, *b)?)
                }
                Some(FragRole::ColorInput(b)) => {
                    Some(color_input_resource_binding(descriptor_layout, *b)?)
                }
                _ => None,
            },
            Stage::Vertex => match vert.and_then(|m| m.role_of(idx)) {
                Some(VertRole::Buffer(b) | VertRole::AccelerationStructureShadow(b)) => {
                    Some(buffer_resource_binding(descriptor_layout, *b)?)
                }
                Some(VertRole::Texture(b)) => {
                    Some(texture_resource_binding(descriptor_layout, *b)?)
                }
                Some(VertRole::Sampler(b)) => {
                    Some(sampler_resource_binding(descriptor_layout, *b)?)
                }
                _ => None,
            },
            Stage::Kernel => match kern.and_then(|m| m.role_of(idx)) {
                Some(
                    KernRole::Buffer(b)
                    | KernRole::AccelerationStructureShadow(b)
                    | KernRole::PrimitiveAccelerationStructureShadow(b),
                ) => Some(buffer_resource_binding(descriptor_layout, *b)?),
                Some(KernRole::Texture(b)) => {
                    Some(texture_resource_binding(descriptor_layout, *b)?)
                }
                Some(KernRole::Sampler(b)) => {
                    Some(sampler_resource_binding(descriptor_layout, *b)?)
                }
                _ => None,
            },
        };
        let storage_resource_binding = match stage {
            Stage::Fragment => match frag.and_then(|m| m.role_of(idx)) {
                Some(FragRole::Texture(binding)) => Some(storage_texture_resource_binding(
                    descriptor_layout,
                    *binding,
                )?),
                _ => None,
            },
            Stage::Vertex => match vert.and_then(|m| m.role_of(idx)) {
                Some(VertRole::Texture(binding)) => Some(storage_texture_resource_binding(
                    descriptor_layout,
                    *binding,
                )?),
                _ => None,
            },
            Stage::Kernel => match kern.and_then(|m| m.role_of(idx)) {
                Some(KernRole::Texture(binding)) => Some(storage_texture_resource_binding(
                    descriptor_layout,
                    *binding,
                )?),
                _ => None,
            },
        };
        let metal_texture_index = match stage {
            Stage::Fragment => frag.and_then(|meta| match meta.role_of(idx) {
                Some(FragRole::Texture(index)) => Some(*index),
                _ => None,
            }),
            Stage::Vertex => vert.and_then(|meta| match meta.role_of(idx) {
                Some(VertRole::Texture(index)) => Some(*index),
                _ => None,
            }),
            Stage::Kernel => kern.and_then(|meta| match meta.role_of(idx) {
                Some(KernRole::Texture(index)) => Some(*index),
                _ => None,
            }),
        };

        let is_threadgroup_buffer = matches!(stage, Stage::Kernel)
            && role_is("buffer")
            && (kern.and_then(|m| m.buffer_address_space(idx)) == Some(3)
                || ptr_storage(&defs, *pty) == Some(StorageClass::Workgroup));

        let texture_type_name = match stage {
            Stage::Fragment => frag.and_then(|m| m.texture_type_name(idx)),
            Stage::Vertex => vert.and_then(|m| m.texture_type_name(idx)),
            Stage::Kernel => kern.and_then(|m| m.texture_type_name(idx)),
        };
        let texture_handle_shape = texture_type_name.map(crate::meta::texture_shape_from_name);
        let is_array_texture = texture_handle_shape.is_some_and(|shape| shape.array_ref);

        if let (Stage::Kernel, Some(KernRole::StageInput(_))) =
            (stage, kern.and_then(|m| m.role_of(idx)))
        {
            let rta = ctx.ty_runtime_array(*pty);
            let block_ty = ctx.module.fresh_id();
            ctx.new_globals.push(type_inst(
                Op::TypeStruct,
                block_ty,
                vec![Operand::IdRef(rta)],
            ));
            let pptr = ctx.ty_ptr(StorageClass::StorageBuffer, block_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ));
            let binding = stage_input_bindings
                .get(&idx)
                .copied()
                .ok_or_else(|| format!("kernel stage_in parameter {idx} missing binding"))?;
            let binding = buffer_resource_binding(descriptor_layout, binding)?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            let index_var = bind_kernel_v3uint_builtin_once(
                ctx,
                &mut global_invocation_id_var,
                BuiltIn::GlobalInvocationId,
            );
            bindings.push((
                *pid,
                ParamBinding::StageInput {
                    var,
                    value_ty: *pty,
                    index_var,
                    dispatch_var: kernel_grid_push_constant_var,
                },
            ));
            buffer_structs.push((var, block_ty));
        } else if role_is("texture") && is_array_texture {
            let (elem_image_ty, dim, arrayed, comp, multisampled, runtime_specialization) =
                if let Some((dim, arrayed, fmt, comp)) = wtex_dims.get(pid).copied() {
                    let metal_index = metal_texture_index
                        .ok_or("storage texture array has no Metal texture index")?;
                    let (fmt, state) =
                        ctx.specialize_storage_image_format(metal_index, fmt, comp)?;
                    (
                        ctx.ty_storage_image(dim, arrayed, fmt, comp),
                        dim,
                        arrayed,
                        comp,
                        false,
                        state.map(|state| (metal_index, state)),
                    )
                } else {
                    let shape = tex_dims
                        .get(pid)
                        .copied()
                        .or_else(|| texture_type_hints.get(pid).copied())
                        .unwrap_or(ImageShape {
                            dim: Dim::Dim2D,
                            arrayed: false,
                            comp: ImageComp::Float,
                            multisampled: false,
                        });
                    (
                        ctx.ty_image_ms(shape.dim, shape.arrayed, shape.comp, shape.multisampled),
                        shape.dim,
                        shape.arrayed,
                        shape.comp,
                        shape.multisampled,
                        None,
                    )
                };
            let descriptor_count = texture_handle_shape
                .map(|shape| shape.descriptor_count())
                .ok_or("texture handle array has no decoded type name to size it from")?;
            let array_ty = ctx.ty_array(elem_image_ty, descriptor_count);
            let pptr = ctx.ty_ptr(StorageClass::UniformConstant, array_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ));
            let binding = required_resource_binding(
                *pid,
                if wtex_dims.contains_key(pid) {
                    storage_resource_binding
                } else {
                    resource_binding
                },
            )?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            ctx.interface_buffer_var(var);
            bindings.push((
                *pid,
                ParamBinding::ImageArray {
                    var,
                    elem_image_ty,
                    dim: (dim, arrayed),
                    comp,
                    multisampled,
                    runtime_specialization,
                },
            ));
        } else if role_is("texture") && wtex_dims.contains_key(pid) {
            let (dim, arrayed, fmt, comp) = wtex_dims
                .get(pid)
                .copied()
                .ok_or("write-texture dims missing for bound param")?;
            let metal_index =
                metal_texture_index.ok_or("storage texture has no Metal texture index")?;
            let (fmt, runtime_state) =
                ctx.specialize_storage_image_format(metal_index, fmt, comp)?;
            let image_ty = ctx.ty_storage_image(dim, arrayed, fmt, comp);
            let pptr = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ));
            let binding = required_resource_binding(*pid, storage_resource_binding)?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            ctx.interface_buffer_var(var);
            bindings.push((
                *pid,
                ParamBinding::StorageImage {
                    var,
                    image_ty,
                    dim: (dim, arrayed),
                    comp,
                    runtime_specialization: runtime_state.map(|state| (metal_index, state)),
                },
            ));
        } else if role_is("texture") {
            let shape = tex_dims
                .get(pid)
                .copied()
                .or_else(|| texture_type_hints.get(pid).copied())
                .unwrap_or(ImageShape {
                    dim: Dim::Dim2D,
                    arrayed: false,
                    comp: ImageComp::Float,
                    multisampled: false,
                });
            let image_ty =
                ctx.ty_image_ms(shape.dim, shape.arrayed, shape.comp, shape.multisampled);
            if crate::passes::resources::bindless_all_on() && shape.dim != Dim::DimBuffer {
                if let Some(slot) = metal_texture_index.filter(|s| *s < 128) {
                    if crate::passes::resources::address_table_usable(ctx) {
                        bindings.push((
                            *pid,
                            ParamBinding::HeapImage {
                                slot,
                                image_ty,
                                dim: (shape.dim, shape.arrayed),
                                comp: shape.comp,
                                multisampled: shape.multisampled,
                            },
                        ));
                        continue;
                    }
                }
            }
            let pptr = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ));
            let binding = required_resource_binding(*pid, resource_binding)?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            ctx.interface_buffer_var(var);
            bindings.push((
                *pid,
                ParamBinding::Image {
                    var,
                    image_ty,
                    dim: (shape.dim, shape.arrayed),
                    comp: shape.comp,
                    multisampled: shape.multisampled,
                },
            ));
        } else if let Some(color_index) = color_input_index(stage, frag, idx) {
            let (sampled_ty, read_ty) = input_attachment_read_types(ctx, &defs, *pty)?;
            let image_ty = ctx.ty_input_attachment(sampled_ty);
            let pptr = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ));
            let binding = required_resource_binding(*pid, resource_binding)?;
            // set 3: the driver binds the open pass's colour views there for each fetching draw, apart from the per-draw
            // resource sets 0/1, so every pipeline of a pass keeps compatible layouts for sets 0-2
            decorate_binding(&mut ctx.module, var, crate::reflect::INPUT_ATTACHMENT_SET, binding);
            decorate_input_attachment_index(&mut ctx.module, var, color_index);
            ctx.interface_buffer_var(var);
            bindings.push((
                *pid,
                ParamBinding::InputAttachment {
                    var,
                    image_ty,
                    read_ty,
                    param_ty: *pty,
                },
            ));
        } else if role_is("sampler") {
            let sty = ctx.ty_sampler();
            let pptr = ctx.ty_ptr(StorageClass::UniformConstant, sty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ));
            let binding = required_resource_binding(*pid, resource_binding)?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            ctx.interface_buffer_var(var);
            let metal_index = match stage {
                Stage::Fragment => frag.and_then(|meta| match meta.role_of(idx) {
                    Some(FragRole::Sampler(index)) => Some(*index),
                    _ => None,
                }),
                Stage::Vertex => vert.and_then(|meta| match meta.role_of(idx) {
                    Some(VertRole::Sampler(index)) => Some(*index),
                    _ => None,
                }),
                Stage::Kernel => kern.and_then(|meta| match meta.role_of(idx) {
                    Some(KernRole::Sampler(index)) => Some(*index),
                    _ => None,
                }),
            }
            .ok_or_else(|| format!("sampler parameter {pid} has no Metal sampler index"))?;
            let specialized_state = usize::try_from(metal_index)
                .ok()
                .and_then(|index| ctx.runtime_sampler_states.get(index))
                .copied()
                .flatten()
                .map(RuntimeSamplerState::lowering_state);
            bindings.push((
                *pid,
                ParamBinding::Sampler {
                    var,
                    specialized_state,
                },
            ));
        } else if is_threadgroup_buffer {
            let pointee = ptr_pointee(&defs, *pty)
                .ok_or_else(|| format!("threadgroup param {pid} type {pty} is not a pointer"))?;
            let layout_ty = kern
                .and_then(|m| m.layout_of(idx))
                .map(|layout| build_workgroup_air_type(ctx, layout));
            let metal_index = kern.and_then(|m| match m.role_of(idx) {
                Some(KernRole::Buffer(index)) => Some(*index),
                _ => None,
            });
            let raw = (layout_ty.is_none() && is_raw_workgroup_array(&defs, pointee))
                .then(|| array_type(&defs, pointee))
                .flatten();
            let (element_ty, default_elements) = match raw {
                Some((elem, len)) => (elem, len),
                None => (
                    layout_ty.unwrap_or(pointee),
                    DEFAULT_WORKGROUP_MEMORY_ELEMENTS,
                ),
            };
            let elements = workgroup_array_elements(
                ctx,
                &defs,
                element_ty,
                metal_index,
                *pid,
                default_elements,
            )?;
            let array_ty = if elements == default_elements && raw.is_some() {
                pointee
            } else {
                ctx.ty_array(element_ty, elements)
            };
            let ptr_ty = ctx.ty_ptr(StorageClass::Workgroup, array_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(ptr_ty),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ));
            bindings.push((*pid, ParamBinding::WorkgroupMemory { var }));
        } else if role_is("buffer") {
            let pointee = ptr_pointee(&defs, *pty)
                .ok_or_else(|| format!("buffer param {pid} type {pty} is not a pointer"))?;
            let pointee_op = defs.get(&pointee).map(|d| d.class.opcode);
            let is_struct = pointee_op == Some(Op::TypeStruct);
            let is_raw_uint_block = is_raw_uint_buffer_block(&defs, pointee);
            let pointee_is_scalar =
                matches!(pointee_op, Some(Op::TypeFloat | Op::TypeInt | Op::TypeBool));
            let layout = match stage {
                Stage::Fragment => frag.and_then(|m| m.layout_of(idx)),
                Stage::Vertex => vert.and_then(|m| m.layout_of(idx)),
                Stage::Kernel => kern.and_then(|m| m.layout_of(idx)),
            };
            let primitive_buffer_air_type = match stage {
                Stage::Kernel => kern
                    .and_then(|m| m.buffer_type_name(idx))
                    .and_then(primitive_air_type_from_name),
                Stage::Fragment | Stage::Vertex => None,
            };
            let mut typed_alias_elements = Vec::new();
            let (struct_ty, wrap) = if is_raw_uint_block {
                let carries_indirect_arguments = kern.is_some_and(|meta| {
                    meta.embedded_arguments
                        .iter()
                        .any(|argument| argument.buffer_param_index == idx)
                });
                if carries_indirect_arguments {
                    (pointee, BufWrap::Direct)
                } else if let Some(at) = layout
                    .filter(|_| buffer_has_access_chains(&ctx.module.functions[entry_idx], *pid))
                {
                    let st = ctx.build_air_type(at);
                    let mut layout_defs = defs.clone();
                    for g in &ctx.new_globals {
                        if let Some(id) = g.result_id {
                            layout_defs.entry(id).or_insert_with(|| g.clone());
                        }
                    }
                    let all_chains_match_struct = buffer_access_chains_match_struct_path(
                        &layout_defs,
                        &ctx.module.functions[entry_idx],
                        *pid,
                        st,
                    );
                    let has_struct_chain = all_chains_match_struct
                        || buffer_has_struct_path_access_chain(
                            &layout_defs,
                            &ctx.module.functions[entry_idx],
                            *pid,
                            st,
                        );
                    let flat_scalar_element = body_buf_flat_scalar_element_type(
                        ctx,
                        &ctx.module.functions[entry_idx],
                        *pid,
                    );
                    if flat_scalar_element.is_some()
                        || ctx.emit_sidecar.all_device_buffers_raw
                        || ctx.emit_sidecar.flat_raw_buffer_params.contains(&idx)
                    {
                        (
                            pointee,
                            BufWrap::Collapsed {
                                block_ty: pointee,
                                prepend_member0: !access_chains_include_wrapper_member0(
                                    &defs,
                                    &ctx.module.functions[entry_idx],
                                    *pid,
                                ),
                                typed_aliases: vec![],
                            },
                        )
                    } else if has_struct_chain {
                        (
                            st,
                            BufWrap::Collapsed {
                                block_ty: st,
                                prepend_member0: false,
                                typed_aliases: vec![],
                            },
                        )
                    } else {
                        (pointee, BufWrap::Direct)
                    }
                } else {
                    (pointee, BufWrap::Direct)
                }
            } else if is_struct
                && struct_buffer_needs_record_array(
                    &defs,
                    &ctx.module.functions[entry_idx],
                    *pid,
                    pointee,
                )
            {
                let elem = ctx.clone_type_for_record_array_element(pointee, &defs);
                let rta = ctx.ty_runtime_array(elem);
                let st = ctx.module.fresh_id();
                ctx.new_globals
                    .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(rta)]));
                (
                    st,
                    BufWrap::RecordArray {
                        block_ty: st,
                        elem_ty: elem,
                    },
                )
            } else if is_struct {
                (pointee, BufWrap::Direct)
            } else if let Some(at) = layout {
                let has_access_chains =
                    buffer_has_access_chains(&ctx.module.functions[entry_idx], *pid);
                let flat_elem = pointee_is_scalar
                    .then(|| body_buf_elem_type(ctx, &ctx.module.functions[entry_idx], *pid))
                    .flatten()
                    .or_else(|| {
                        body_buf_flat_scalar_element_type(
                            ctx,
                            &ctx.module.functions[entry_idx],
                            *pid,
                        )
                    });
                if !has_access_chains {
                    let rta = ctx.ty_runtime_array(pointee);
                    let st = ctx.module.fresh_id();
                    ctx.new_globals
                        .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(rta)]));
                    (
                        st,
                        BufWrap::Collapsed {
                            block_ty: st,
                            prepend_member0: true,
                            typed_aliases: vec![],
                        },
                    )
                } else {
                    let st = ctx.build_air_type(at);
                    let mut layout_defs = defs.clone();
                    for g in &ctx.new_globals {
                        if let Some(id) = g.result_id {
                            layout_defs.entry(id).or_insert_with(|| g.clone());
                        }
                    }
                    if let Some(elem) = flat_elem {
                        let already_has_wrapper_member0 = access_chains_include_wrapper_member0(
                            &defs,
                            &ctx.module.functions[entry_idx],
                            *pid,
                        );
                        let rta = ctx.ty_runtime_array(elem);
                        let st = ctx.module.fresh_id();
                        ctx.new_globals.push(type_inst(
                            Op::TypeStruct,
                            st,
                            vec![Operand::IdRef(rta)],
                        ));
                        (
                            st,
                            BufWrap::Collapsed {
                                block_ty: st,
                                prepend_member0: !already_has_wrapper_member0,
                                typed_aliases: vec![],
                            },
                        )
                    } else if buffer_access_chains_match_struct_path(
                        &layout_defs,
                        &ctx.module.functions[entry_idx],
                        *pid,
                        st,
                    ) {
                        (
                            st,
                            BufWrap::Collapsed {
                                block_ty: st,
                                prepend_member0: false,
                                typed_aliases: vec![],
                            },
                        )
                    } else if !pointee_is_scalar
                        && buffer_has_multi_index_access_chains(
                            &ctx.module.functions[entry_idx],
                            *pid,
                        )
                        && struct_buffer_needs_record_array(
                            &layout_defs,
                            &ctx.module.functions[entry_idx],
                            *pid,
                            st,
                        )
                    {
                        let rta = ctx.ty_runtime_array(st);
                        let block = ctx.module.fresh_id();
                        ctx.new_globals.push(type_inst(
                            Op::TypeStruct,
                            block,
                            vec![Operand::IdRef(rta)],
                        ));
                        (
                            block,
                            BufWrap::RecordArray {
                                block_ty: block,
                                elem_ty: st,
                            },
                        )
                    } else {
                        (
                            st,
                            BufWrap::Collapsed {
                                block_ty: st,
                                prepend_member0: false,
                                typed_aliases: vec![],
                            },
                        )
                    }
                }
            } else {
                if let Some(air_ty) = primitive_buffer_air_type.as_ref().filter(|air_ty| {
                    !matches!(**air_ty, AirType::Scalar(_))
                        && buffer_has_multi_index_access_chains(
                            &ctx.module.functions[entry_idx],
                            *pid,
                        )
                }) {
                    let elem = ctx.build_air_type(air_ty);
                    let rta = ctx.ty_runtime_array(elem);
                    let st = ctx.module.fresh_id();
                    ctx.new_globals
                        .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(rta)]));
                    (
                        st,
                        BufWrap::RecordArray {
                            block_ty: st,
                            elem_ty: elem,
                        },
                    )
                } else {
                    let already_has_wrapper_member0 = access_chains_include_wrapper_member0(
                        &defs,
                        &ctx.module.functions[entry_idx],
                        *pid,
                    );
                    let body_elements =
                        body_buf_elem_types(ctx, &ctx.module.functions[entry_idx], *pid);
                    let elem = body_elements.first().copied().unwrap_or(pointee);
                    if body_elements.len() > 1
                        && body_elements
                            .iter()
                            .all(|element| buffer_typed_alias_element(&defs, *element))
                    {
                        typed_alias_elements = body_elements;
                    }
                    let rta = ctx.ty_runtime_array(elem);
                    let st = ctx.module.fresh_id();
                    ctx.new_globals
                        .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(rta)]));
                    (
                        st,
                        BufWrap::Collapsed {
                            block_ty: st,
                            prepend_member0: !already_has_wrapper_member0,
                            typed_aliases: vec![],
                        },
                    )
                }
            };
            let source_layout_ty = layout.map(|air_layout| ctx.build_air_type(air_layout));
            let uptr = ctx.ty_ptr(StorageClass::StorageBuffer, struct_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(uptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ));
            if let Some(source_layout_ty) = source_layout_ty.filter(|_| {
                single_member_array_scalar_elem(ctx, struct_ty)
                    .is_some_and(|element| is_unsigned_byte_scalar(ctx, element))
            }) {
                ctx.emit_sidecar
                    .buffer_root_source_types
                    .insert(var, source_layout_ty);
            }
            let binding = required_resource_binding(*pid, resource_binding)?;
            decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
            let mut wrap = wrap;
            if let BufWrap::Collapsed { typed_aliases, .. } = &mut wrap {
                let primary_element = single_member_array_scalar_elem(ctx, struct_ty);
                for element in typed_alias_elements
                    .into_iter()
                    .filter(|element| Some(*element) != primary_element)
                {
                    let runtime_array = ctx.ty_runtime_array(element);
                    let alias_block = ctx.module.fresh_id();
                    ctx.new_globals.push(type_inst(
                        Op::TypeStruct,
                        alias_block,
                        vec![Operand::IdRef(runtime_array)],
                    ));
                    let alias_pointer = ctx.ty_ptr(StorageClass::StorageBuffer, alias_block);
                    let alias_var = ctx.module.fresh_id();
                    ctx.new_globals.push(Instruction::new(
                        Op::Variable,
                        Some(alias_pointer),
                        Some(alias_var),
                        vec![Operand::StorageClass(StorageClass::StorageBuffer)],
                    ));
                    decorate_binding(&mut ctx.module, alias_var, descriptor_layout.set, binding);
                    typed_aliases.push((element, alias_var));
                    buffer_structs.push((alias_var, alias_block));
                }
            }
            bindings.push((*pid, ParamBinding::Buffer { var, wrap }));
            buffer_structs.push((var, struct_ty));
        } else if role_is("varying") {
            if matches!(stage, Stage::Fragment) && is_scalar_bool(&defs, *pty) {
                let uint_ty = ctx.ty_uint();
                let pptr = ctx.ty_ptr(StorageClass::Input, uint_ty);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pptr),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_location(&mut ctx.module, var, loc);
                decorate_flat(&mut ctx.module, var);
                ctx.interface.push(var);
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarBoolFromUint { var, bool_ty: *pty },
                ));
            } else if matches!(stage, Stage::Fragment) && type_contains_bool(&defs, *pty) {
                return Err(format!(
                    "fragment bool stage input at location {loc} is unsupported: Vulkan user \
                     Input/Output interfaces cannot use OpTypeBool"
                ));
            } else {
                let interface_ty = match stage {
                    Stage::Fragment => fragment_varying_interface_type(ctx, frag, loc, *pty, &defs),
                    Stage::Vertex => vertex_attribute_interface_type(ctx, vert, loc, *pty, &defs),
                    Stage::Kernel => *pty,
                };
                let pptr = ctx.ty_ptr(StorageClass::Input, interface_ty);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pptr),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_location(&mut ctx.module, var, loc);
                if matches!(stage, Stage::Fragment) {
                    decorate_interpolation(
                        &mut ctx.module,
                        var,
                        frag.map(|m| m.varying_interpolation(loc))
                            .unwrap_or_default(),
                        fragment_input_needs_flat(&defs, *pty),
                    );
                }
                ctx.interface.push(var);
                if interface_ty == *pty {
                    bindings.push((*pid, ParamBinding::LoadVar { var, ty: *pty }));
                } else if matches!(stage, Stage::Vertex | Stage::Fragment)
                    && (type_int_shape(&defs, *pty).is_some()
                        || defs.get(pty).is_some_and(|definition| {
                            definition.class.opcode == Op::TypeVector
                                && definition
                                    .operands
                                    .first()
                                    .and_then(|operand| match operand {
                                        Operand::IdRef(element) => Some(*element),
                                        _ => None,
                                    })
                                    .is_some_and(|element| type_int_shape(&defs, element).is_some())
                        }))
                {
                    let binding = if matches!(
                        (
                            integer_component_width(ctx, interface_ty),
                            integer_component_width(ctx, *pty)
                        ),
                        (Some(interface_bits), Some(param_bits)) if interface_bits == param_bits
                    ) {
                        ParamBinding::LoadVarBitcast {
                            var,
                            load_ty: interface_ty,
                            param_ty: *pty,
                        }
                    } else {
                        ParamBinding::LoadVarConverted {
                            var,
                            load_ty: interface_ty,
                            param_ty: *pty,
                        }
                    };
                    bindings.push((*pid, binding));
                } else if float_component_width(ctx, interface_ty).is_some()
                    && float_component_width(ctx, *pty).is_some()
                {
                    bindings.push((
                        *pid,
                        ParamBinding::LoadVarConverted {
                            var,
                            load_ty: interface_ty,
                            param_ty: *pty,
                        },
                    ));
                } else {
                    bindings.push((
                        *pid,
                        ParamBinding::LoadVarBitcast {
                            var,
                            load_ty: interface_ty,
                            param_ty: *pty,
                        },
                    ));
                }
            }
        } else if role_is("viewport_array_index") {
            let uint_ty = ctx.ty_uint();
            let pptr = ctx.ty_ptr(StorageClass::Input, uint_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            decorate_builtin(&mut ctx.module, var, BuiltIn::ViewportIndex);
            decorate_flat(&mut ctx.module, var);
            ctx.interface.push(var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("render_target_array_index") {
            let uint_ty = ctx.ty_uint();
            let var = bind_kernel_uint_builtin_once(ctx, &mut layer_var, BuiltIn::Layer);
            decorate_flat(&mut ctx.module, var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("patch_input") {
            let location = match vert.and_then(|meta| meta.role_of(idx)) {
                Some(VertRole::PatchInput(location)) => *location,
                _ => unreachable!("patch_input role has a location"),
            };
            let pptr = ctx.ty_ptr(StorageClass::Input, *pty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            decorate_location(&mut ctx.module, var, location);
            decorate_patch(&mut ctx.module, var);
            ctx.interface.push(var);
            bindings.push((*pid, ParamBinding::LoadVar { var, ty: *pty }));
        } else if role_is("position_in_patch") {
            let vec_ty = ctx.ty_vecf(3);
            let var = if let Some(var) = tess_coord_var {
                var
            } else {
                let pptr = ctx.ty_ptr(StorageClass::Input, vec_ty);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pptr),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_builtin(&mut ctx.module, var, BuiltIn::TessCoord);
                ctx.interface.push(var);
                tess_coord_var = Some(var);
                var
            };
            let binding = match tess_coord_prefix_lanes(&defs, *pty, ctx.ty_float())? {
                None => ParamBinding::LoadVar { var, ty: vec_ty },
                Some(lanes) => ParamBinding::LoadVarVectorPrefix {
                    var,
                    vec_ty,
                    scalar_ty: ctx.ty_float(),
                    prefix_ty: *pty,
                    out_ty: *pty,
                    lanes,
                },
            };
            bindings.push((*pid, binding));
        } else if role_is("patch_id") {
            let uint_ty = ctx.ty_uint();
            let var =
                bind_kernel_uint_builtin_once(ctx, &mut primitive_id_var, BuiltIn::PrimitiveId);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if matches!(stage, Stage::Vertex)
            && vert.is_some_and(VertMeta::is_tessellation_evaluation)
            && (role_is("instance_id")
                || role_is("amplification_id")
                || role_is("amplification_count"))
        {
            let role = vert
                .and_then(|meta| meta.role_of(idx))
                .expect("decoded role");
            let location = vert
                .and_then(|meta| meta.tessellation_system_input_location(role))
                .expect("tessellation system input location");
            let pptr = ctx.ty_ptr(StorageClass::Input, *pty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            decorate_location(&mut ctx.module, var, location);
            decorate_patch(&mut ctx.module, var);
            ctx.interface.push(var);
            bindings.push((*pid, ParamBinding::LoadVar { var, ty: *pty }));
        } else if role_is("amplification_count") {
            let val = ctx.const_int_of(*pty, i64::from(ctx.vertex_amplification_count));
            bindings.push((*pid, ParamBinding::Value { val }));
        } else if role_is("amplification_id") && ctx.vertex_amplification_count == 1 {
            let val = ctx.const_int_of(*pty, 0);
            bindings.push((*pid, ParamBinding::Value { val }));
        } else if role_is("amplification_id") {
            let uint_ty = ctx.ty_uint();
            let pptr = ctx.ty_ptr(StorageClass::Input, uint_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            decorate_builtin(&mut ctx.module, var, BuiltIn::ViewIndex);
            if matches!(stage, Stage::Fragment) {
                decorate_flat(&mut ctx.module, var);
            }
            ctx.interface.push(var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("vertex_id") || role_is("instance_id") || role_is("base_vertex") || role_is("base_instance") {
            // Metal's vertex_id / instance_id already include the base, like Vulkan's VertexIndex / InstanceIndex, so the
            // bases map one to one. WAS: base_vertex / base_instance had no lowering and every pipeline using them was
            // refused (Blender 4.2's specialized UI shaders: the app crashed at startup, 10-09).
            let builtin = if role_is("vertex_id") {
                BuiltIn::VertexIndex
            } else if role_is("base_vertex") {
                BuiltIn::BaseVertex
            } else if role_is("base_instance") {
                BuiltIn::BaseInstance
            } else {
                BuiltIn::InstanceIndex
            };
            let uint_ty = ctx.ty_uint();
            let pptr = ctx.ty_ptr(StorageClass::Input, uint_ty);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(pptr),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            decorate_builtin(&mut ctx.module, var, builtin);
            ctx.interface.push(var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("threads_per_threadgroup") {
            let lanes = scalar_or_vector_component(&defs, *pty)
                .and_then(|(_, lanes)| lanes)
                .unwrap_or(1);
            bindings.push((
                *pid,
                ParamBinding::LoadKernelLocalSize {
                    out_ty: *pty,
                    lanes,
                },
            ));
        } else if role_is("dispatch_threads_per_threadgroup") {
            let lanes = scalar_or_vector_component(&defs, *pty)
                .and_then(|(_, lanes)| lanes)
                .unwrap_or(1);
            bindings.push((
                *pid,
                ParamBinding::LoadKernelRequestedLocalSize {
                    out_ty: *pty,
                    lanes,
                },
            ));
        } else if role_is("thread_position_in_threadgroup") {
            bind_kernel_uvec3_builtin(
                ctx,
                &defs,
                &mut bindings,
                *pid,
                *pty,
                BuiltIn::LocalInvocationId,
            );
        } else if role_is("threadgroups_per_grid") {
            if matches!(
                ctx.kernel_dispatch,
                crate::reflect::KernelDispatch::Workgroups
            ) {
                let var = bind_kernel_v3uint_builtin_once(
                    ctx,
                    &mut num_workgroups_var,
                    BuiltIn::NumWorkgroups,
                );
                bind_kernel_uvec3_builtin_var(ctx, &defs, &mut bindings, *pid, *pty, var);
            } else {
                let var = kernel_grid_push_constant_var.expect("exact dispatch payload bound");
                let lanes = scalar_or_vector_component(&defs, *pty)
                    .and_then(|(_, lanes)| lanes)
                    .unwrap_or(1);
                bindings.push((
                    *pid,
                    ParamBinding::LoadKernelDispatchField {
                        var,
                        first_member: 9,
                        out_ty: *pty,
                        lanes,
                    },
                ));
            }
        } else if role_is("threads_per_grid") {
            match ctx.kernel_dispatch {
                crate::reflect::KernelDispatch::ThreadsFixed { .. }
                | crate::reflect::KernelDispatch::ThreadsDynamic { .. } => {
                    let var = kernel_grid_push_constant_var.expect("exact dispatch payload bound");
                    let lanes = scalar_or_vector_component(&defs, *pty)
                        .and_then(|(_, lanes)| lanes)
                        .unwrap_or(1);
                    bindings.push((
                        *pid,
                        ParamBinding::LoadKernelDispatchField {
                            var,
                            first_member: 0,
                            out_ty: *pty,
                            lanes,
                        },
                    ));
                }
                crate::reflect::KernelDispatch::Workgroups => {
                    let var = bind_kernel_v3uint_builtin_once(
                        ctx,
                        &mut num_workgroups_var,
                        BuiltIn::NumWorkgroups,
                    );
                    bind_kernel_threads_per_grid(ctx, &defs, &mut bindings, *pid, *pty, var);
                }
            }
        } else if role_is("threadgroup_position_in_grid") {
            if matches!(
                ctx.kernel_dispatch,
                crate::reflect::KernelDispatch::Workgroups
            ) {
                bind_kernel_uvec3_builtin(
                    ctx,
                    &defs,
                    &mut bindings,
                    *pid,
                    *pty,
                    BuiltIn::WorkgroupId,
                );
            } else {
                let builtin_var = bind_kernel_v3uint_builtin(ctx, BuiltIn::WorkgroupId);
                let dispatch_var =
                    kernel_grid_push_constant_var.expect("exact dispatch payload bound");
                let lanes = scalar_or_vector_component(&defs, *pty)
                    .and_then(|(_, lanes)| lanes)
                    .unwrap_or(1);
                bindings.push((
                    *pid,
                    ParamBinding::LoadBuiltinPlusKernelDispatchField {
                        builtin_var,
                        dispatch_var,
                        first_member: 6,
                        out_ty: *pty,
                        lanes,
                    },
                ));
            }
        } else if role_is("thread_index_in_threadgroup") {
            let uint_ty = ctx.ty_uint();
            let var = bind_kernel_uint_builtin_once(
                ctx,
                &mut local_invocation_index_var,
                BuiltIn::LocalInvocationIndex,
            );
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if let Some((fact, lanes)) = execution_group {
            let uint_ty = ctx.ty_uint();
            let binding = match fact {
                ExecutionGroupFact::ThreadIndexInGroup => ParamBinding::LoadVarBitAnd {
                    var: subgroup_local_invocation_id_input_var(ctx, uint_ty),
                    load_ty: uint_ty,
                    param_ty: *pty,
                    mask: lanes - 1,
                },
                ExecutionGroupFact::GroupIndexInThreadgroup => ParamBinding::LoadVarShiftRight {
                    var: bind_kernel_uint_builtin_once(
                        ctx,
                        &mut local_invocation_index_var,
                        BuiltIn::LocalInvocationIndex,
                    ),
                    load_ty: uint_ty,
                    param_ty: *pty,
                    shift: lanes.trailing_zeros(),
                },
                ExecutionGroupFact::GroupsPerThreadgroup => {
                    ParamBinding::LoadKernelGroupsPerThreadgroup {
                        out_ty: *pty,
                        lanes,
                    }
                }
                ExecutionGroupFact::ThreadsPerGroup => ParamBinding::Value {
                    val: const_kernel_local_size(ctx, &defs, *pty, [lanes, 1, 1])
                        .unwrap_or_else(|| ctx.const_uint(lanes)),
                },
            };
            bindings.push((*pid, binding));
        } else if role_is("thread_position_in_grid") {
            let var = bind_kernel_v3uint_builtin_once(
                ctx,
                &mut global_invocation_id_var,
                BuiltIn::GlobalInvocationId,
            );
            if matches!(
                ctx.kernel_dispatch,
                crate::reflect::KernelDispatch::Workgroups
            ) {
                bind_kernel_uvec3_builtin_var(ctx, &defs, &mut bindings, *pid, *pty, var);
            } else {
                let dispatch_var =
                    kernel_grid_push_constant_var.expect("exact dispatch payload bound");
                let lanes = scalar_or_vector_component(&defs, *pty)
                    .and_then(|(_, lanes)| lanes)
                    .unwrap_or(1);
                bindings.push((
                    *pid,
                    ParamBinding::LoadBuiltinPlusKernelDispatchField {
                        builtin_var: var,
                        dispatch_var,
                        first_member: 3,
                        out_ty: *pty,
                        lanes,
                    },
                ));
            }
        } else if role_is("position") {
            let v4 = ctx.ty_vecf(4);
            if *pty == v4 {
                let var = if let Some(v) = fragcoord_var {
                    v
                } else {
                    let pptr = ctx.ty_ptr(StorageClass::Input, v4);
                    let var = ctx.module.fresh_id();
                    ctx.new_globals.push(Instruction::new(
                        Op::Variable,
                        Some(pptr),
                        Some(var),
                        vec![Operand::StorageClass(StorageClass::Input)],
                    ));
                    decorate_builtin(&mut ctx.module, var, BuiltIn::FragCoord);
                    ctx.interface.push(var);
                    fragcoord_var = Some(var);
                    var
                };
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: v4 }));
            } else {
                return Err(format!(
                    "[[position]] parameter {idx} is not a float4; FragCoord is a 4-component \
                     float builtin and reading a zero in its place puts the fragment at the origin"
                ));
            }
        } else if role_is("point_coord") {
            let v2 = ctx.ty_vecf(2);
            if *pty == v2 {
                let var = if let Some(v) = pointcoord_var {
                    v
                } else {
                    let pptr = ctx.ty_ptr(StorageClass::Input, v2);
                    let var = ctx.module.fresh_id();
                    ctx.new_globals.push(Instruction::new(
                        Op::Variable,
                        Some(pptr),
                        Some(var),
                        vec![Operand::StorageClass(StorageClass::Input)],
                    ));
                    decorate_builtin(&mut ctx.module, var, BuiltIn::PointCoord);
                    ctx.interface.push(var);
                    pointcoord_var = Some(var);
                    var
                };
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: v2 }));
            } else {
                return Err(format!(
                    "[[point_coord]] parameter {idx} is not a float2; PointCoord is a 2-component \
                     float builtin"
                ));
            }
        } else if role_is("sample_mask_in") {
            if type_int_shape(&defs, *pty).is_none() {
                return Err(format!(
                    "[[sample_mask]] parameter {idx} is not an integer; SampleMask is a coverage \
                     bitmask"
                ));
            }
            let uint_ty = ctx.ty_uint();
            let mask_ty = ctx.ty_array(uint_ty, 1);
            let var = if let Some(existing) = sample_mask_in_var {
                existing
            } else {
                let pptr = ctx.ty_ptr(StorageClass::Input, mask_ty);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pptr),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_builtin(&mut ctx.module, var, BuiltIn::SampleMask);
                ctx.interface.push(var);
                sample_mask_in_var = Some(var);
                var
            };
            bindings.push((
                *pid,
                ParamBinding::LoadVarComponent {
                    var,
                    vec_ty: mask_ty,
                    scalar_ty: uint_ty,
                    out_ty: *pty,
                    comp: 0,
                },
            ));
        } else if role_is("barycentric_coord") {
            let v3 = ctx.ty_vecf(3);
            if *pty != v3 {
                return Err(format!(
                    "[[barycentric_coord]] parameter {idx} is not a float3; \
                     BaryCoord is a 3-component float builtin"
                ));
            }
            let no_perspective = matches!(
                frag.and_then(|meta| meta.role_of(idx)),
                Some(FragRole::BarycentricCoord {
                    no_perspective: true
                })
            );
            let slot = if no_perspective {
                &mut bary_coord_no_persp_var
            } else {
                &mut bary_coord_var
            };
            let var = if let Some(existing) = *slot {
                existing
            } else {
                let pptr = ctx.ty_ptr(StorageClass::Input, v3);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pptr),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_builtin(
                    &mut ctx.module,
                    var,
                    if no_perspective {
                        BuiltIn::BaryCoordNoPerspKHR
                    } else {
                        BuiltIn::BaryCoordKHR
                    },
                );
                ctx.interface.push(var);
                *slot = Some(var);
                var
            };
            bindings.push((*pid, ParamBinding::LoadVar { var, ty: v3 }));
        } else if role_is("front_facing") {
            let bool_ty = ctx.ty_bool();
            let int_param_width = crate::spirv_module::type_int_width(&defs, *pty);
            let declared_type = frag.and_then(|meta| meta.input_type_names.get(&idx));
            let declared_bool = declared_type.is_some_and(|name| name == "bool");
            if (*pty == bool_ty && declared_type.is_none_or(|name| name == "bool"))
                || (declared_bool && matches!(int_param_width, Some(w) if w <= 32))
            {
                let var = if let Some(v) = front_facing_var {
                    v
                } else {
                    let pptr = ctx.ty_ptr(StorageClass::Input, bool_ty);
                    let var = ctx.module.fresh_id();
                    ctx.new_globals.push(Instruction::new(
                        Op::Variable,
                        Some(pptr),
                        Some(var),
                        vec![Operand::StorageClass(StorageClass::Input)],
                    ));
                    decorate_builtin(&mut ctx.module, var, BuiltIn::FrontFacing);
                    ctx.interface.push(var);
                    front_facing_var = Some(var);
                    var
                };
                if *pty == bool_ty {
                    bindings.push((*pid, ParamBinding::LoadVar { var, ty: bool_ty }));
                } else {
                    bindings.push((
                        *pid,
                        ParamBinding::LoadVarBoolToInt {
                            var,
                            bool_ty,
                            int_ty: *pty,
                        },
                    ));
                }
            } else {
                return Err(format!(
                    "[[front_facing]] parameter {idx} is not a bool; FrontFacing is a boolean \
                     builtin and reading a zero in its place claims every triangle is back-facing"
                ));
            }
        } else if role_is("primitive_id") {
            let uint_ty = ctx.ty_uint();
            let var =
                bind_kernel_uint_builtin_once(ctx, &mut primitive_id_var, BuiltIn::PrimitiveId);
            decorate_flat(&mut ctx.module, var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("sample_id") {
            let uint_ty = ctx.ty_uint();
            let var = bind_kernel_uint_builtin_once(ctx, &mut sample_id_var, BuiltIn::SampleId);
            decorate_flat(&mut ctx.module, var);
            if *pty == uint_ty {
                bindings.push((*pid, ParamBinding::LoadVar { var, ty: uint_ty }));
            } else {
                bindings.push((
                    *pid,
                    ParamBinding::LoadVarConverted {
                        var,
                        load_ty: uint_ty,
                        param_ty: *pty,
                    },
                ));
            }
        } else if role_is("imageblock_data") {
            let imageblock = frag
                .and_then(|meta| meta.fragment_imageblock.as_ref())
                .ok_or_else(|| {
                    format!(
                        "fragment imageblock parameter {idx} has no decoded AIR layout contract"
                    )
                })?;
            let projection = imageblock
                .inputs
                .iter()
                .find(|projection| projection.interface_index == idx)
                .ok_or_else(|| format!("fragment imageblock parameter {idx} has no projection"))?;
            let projected_types = defs
                .get(pty)
                .filter(|definition| definition.class.opcode == Op::TypeStruct)
                .ok_or_else(|| {
                    format!("fragment imageblock parameter {idx} is not a struct value")
                })?
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(ty) => Some(*ty),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if projected_types.len() != projection.members.len() {
                return Err(format!(
                    "fragment imageblock parameter {idx} exposes {} fields but AIR projects {}",
                    projected_types.len(),
                    projection.members.len()
                ));
            }
            let coord_var = if let Some(var) = fragcoord_var {
                var
            } else {
                let coord_ty = ctx.ty_vecf(4);
                let pointer_ty = ctx.ty_ptr(StorageClass::Input, coord_ty);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(pointer_ty),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                decorate_builtin(&mut ctx.module, var, BuiltIn::FragCoord);
                ctx.interface.push(var);
                fragcoord_var = Some(var);
                ctx.fragment_imageblock_coord_var = Some(var);
                var
            };
            let mut members = Vec::with_capacity(projection.members.len());
            for (projected_ty, projected) in
                projected_types.into_iter().zip(projection.members.iter())
            {
                let master = imageblock
                    .members
                    .get(projected.master_member as usize)
                    .ok_or_else(|| {
                        format!(
                            "fragment imageblock parameter {idx} references missing master member {}",
                            projected.master_member
                        )
                    })?;
                let format = fragment_imageblock_format(&master.type_name).ok_or_else(|| {
                    format!(
                        "fragment imageblock parameter {idx} member {} has unsupported master type {}",
                        projected.projection_member, master.type_name
                    )
                })?;
                if !fragment_imageblock_projection_type_matches(&defs, projected_ty, format) {
                    return Err(format!(
                        "fragment imageblock parameter {idx} member {} does not match AIR master type {}",
                        projected.projection_member, master.type_name
                    ));
                }
                let (image_var, image_ty) =
                    ctx.fragment_imageblock_var(projected.master_member, &master.type_name)?;
                members.push((image_var, image_ty, projected_ty, format));
            }
            bindings.push((
                *pid,
                ParamBinding::FragmentImageblockProjection {
                    coord_var,
                    param_ty: *pty,
                    members,
                },
            ));
        } else if let Some(pointee) = data_pointer_pointee(&defs, *pty) {
            let var = ctx.zero_private_var(pointee);
            if role_is("variant_absent_texture") {
                ctx.variant_absent_texture_values.insert(var);
            }
            bindings.push((*pid, ParamBinding::ZeroPointer { var }));
        } else {
            let z = ctx.const_zero(*pty, &defs);
            bindings.push((*pid, ParamBinding::ZeroValue { val: z }));
        }
    }

    let mut all_defs = defs.clone();
    for g in &ctx.new_globals {
        if let Some(id) = g.result_id {
            all_defs.entry(id).or_insert_with(|| g.clone());
        }
    }
    split_explicit_layout_type_aliases(ctx, &buffer_structs, &mut all_defs);
    for g in &ctx.new_globals {
        if let Some(id) = g.result_id {
            all_defs.entry(id).or_insert_with(|| g.clone());
        }
    }
    let mut block_decorated: HashSet<Word> = HashSet::new();
    for (var, struct_ty) in &buffer_structs {
        if block_decorated.insert(*struct_ty) {
            decorate_block_struct(ctx, *struct_ty, &all_defs);
        }
        ctx.interface_buffer_var(*var);
    }

    handle_static_sampler(ctx)?;
    include_existing_private_globals(ctx);

    let embedded_textures = match stage {
        Stage::Fragment => frag.map(|meta| meta.embedded_textures.as_slice()),
        Stage::Vertex => vert.map(|meta| meta.embedded_textures.as_slice()),
        Stage::Kernel => kern.map(|meta| meta.embedded_textures.as_slice()),
    };
    register_embedded_textures(ctx, entry_idx, embedded_textures)?;
    let embedded_samplers = match stage {
        Stage::Fragment => frag.map(|meta| meta.embedded_samplers.as_slice()),
        Stage::Vertex => vert.map(|meta| meta.embedded_samplers.as_slice()),
        Stage::Kernel => kern.map(|meta| meta.embedded_samplers.as_slice()),
    };
    register_embedded_samplers(ctx, entry_idx, embedded_samplers)?;

    apply_bindings(ctx, entry_idx, bindings, &buffer_structs, &all_defs)?;
    if frag
        .and_then(|meta| meta.fragment_imageblock.as_ref())
        .is_some()
    {
        ctx.uses_fragment_imageblock = true;
        ctx.fragment_imageblock_coord_var = fragcoord_var;
        let block = ctx.module.functions[entry_idx]
            .blocks
            .first_mut()
            .ok_or_else(|| "fragment imageblock entry has no block".to_string())?;
        let insert_at = block
            .instructions
            .iter()
            .position(|instruction| instruction.class.opcode != Op::Variable)
            .unwrap_or(block.instructions.len());
        block.instructions.insert(
            insert_at,
            Instruction::new(Op::BeginInvocationInterlockEXT, None, None, vec![]),
        );
    }
    lower_patch_control_point_calls(ctx, entry_idx, vert, &all_defs)?;
    lower_buffer_address_facts(ctx, entry_idx, kern)?;

    Ok(defs)
}

fn buffer_typed_alias_element(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    let Some(definition) = defs.get(&ty) else {
        return false;
    };
    if matches!(definition.class.opcode, Op::TypeInt | Op::TypeFloat) {
        return true;
    }
    if definition.class.opcode != Op::TypeVector {
        return false;
    }
    let (Some(Operand::IdRef(element)), Some(Operand::LiteralBit32(lanes))) =
        (definition.operands.first(), definition.operands.get(1))
    else {
        return false;
    };
    (2..=4).contains(lanes)
        && defs
            .get(element)
            .is_some_and(|element| matches!(element.class.opcode, Op::TypeInt | Op::TypeFloat))
}

fn float_component_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    let def = crate::passes::value_queries::type_def_of(ctx, ty)?;
    let scalar_ty = if def.class.opcode == Op::TypeVector {
        match def.operands.first()? {
            Operand::IdRef(component) => *component,
            _ => return None,
        }
    } else {
        ty
    };
    let def = crate::passes::value_queries::type_def_of(ctx, scalar_ty)?;
    if def.class.opcode != Op::TypeFloat {
        return None;
    }
    match def.operands.first()? {
        Operand::LiteralBit32(bits) => Some(*bits),
        _ => None,
    }
}

fn integer_component_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    let definition = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .find(|instruction| instruction.result_id == Some(ty))?;
    match definition.class.opcode {
        Op::TypeInt => match definition.operands.first() {
            Some(Operand::LiteralBit32(bits)) => Some(*bits),
            _ => None,
        },
        Op::TypeVector => match definition.operands.first() {
            Some(Operand::IdRef(element)) => integer_component_width(ctx, *element),
            _ => None,
        },
        _ => None,
    }
}

fn tess_coord_prefix_lanes(
    defs: &HashMap<Word, Instruction>,
    param_ty: Word,
    float_ty: Word,
) -> Result<Option<u32>, String> {
    match scalar_or_vector_component(defs, param_ty) {
        Some((component, Some(3))) if component == float_ty => Ok(None),
        Some((component, Some(2))) if component == float_ty => Ok(Some(2)),
        _ => Err("position_in_patch parameter must be float2 or float3".to_string()),
    }
}

fn lower_patch_control_point_calls(
    ctx: &mut Ctx,
    entry_idx: usize,
    vert: Option<&VertMeta>,
    defs: &HashMap<Word, Instruction>,
) -> Result<(), String> {
    let Some(tessellation) = vert.and_then(|meta| meta.tessellation.as_ref()) else {
        return Ok(());
    };
    let Some(control_point_function) = tessellation.control_point_function.as_ref() else {
        return Ok(());
    };
    let function_id = ctx.module.debug_names.iter().find_map(|instruction| {
        let [Operand::IdRef(id), Operand::LiteralString(name)] = instruction.operands.as_slice()
        else {
            return None;
        };
        (instruction.class.opcode == Op::Name && name == control_point_function).then_some(*id)
    });
    let Some(function_id) = function_id else {
        return Err(format!(
            "tessellation control-point function {:?} has no emitted declaration",
            control_point_function
        ));
    };
    let call_result_type = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find(|instruction| {
            instruction.class.opcode == Op::FunctionCall
                && instruction.operands.first() == Some(&Operand::IdRef(function_id))
        })
        .and_then(|instruction| instruction.result_type);
    let Some(call_result_type) = call_result_type else {
        return Ok(());
    };
    let member_types = defs
        .get(&call_result_type)
        .filter(|definition| definition.class.opcode == Op::TypeStruct)
        .map(|definition| {
            definition
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .ok_or("tessellation control-point accessor must return a struct")?;
    if member_types.len() != tessellation.control_point_fields.len() {
        return Err(format!(
            "tessellation control-point metadata has {} fields but accessor returns {}",
            tessellation.control_point_fields.len(),
            member_types.len()
        ));
    }

    let mut inputs = Vec::with_capacity(member_types.len());
    for (member_ty, field) in member_types
        .iter()
        .copied()
        .zip(&tessellation.control_point_fields)
    {
        let array_ty = ctx.ty_array(member_ty, tessellation.control_point_count);
        let pointer_ty = ctx.ty_ptr(StorageClass::Input, array_ty);
        let var = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pointer_ty),
            Some(var),
            vec![Operand::StorageClass(StorageClass::Input)],
        ));
        decorate_location(&mut ctx.module, var, field.location);
        ctx.interface.push(var);
        inputs.push((var, member_ty));
    }

    for block_idx in 0..ctx.module.functions[entry_idx].blocks.len() {
        let old = ctx.module.functions[entry_idx].blocks[block_idx]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for instruction in old {
            if instruction.class.opcode != Op::FunctionCall
                || instruction.operands.first() != Some(&Operand::IdRef(function_id))
            {
                rewritten.push(instruction);
                continue;
            }
            let result = instruction
                .result_id
                .ok_or("tessellation control-point call has no result")?;
            let result_type = instruction
                .result_type
                .ok_or("tessellation control-point call has no result type")?;
            if result_type != call_result_type {
                return Err(
                    "tessellation control-point accessor has inconsistent return types".into(),
                );
            }
            let index = instruction
                .operands
                .get(1)
                .cloned()
                .ok_or("tessellation control-point call has no index")?;
            let mut members = Vec::with_capacity(inputs.len());
            for (var, member_ty) in &inputs {
                let member_pointer_ty = ctx.ty_ptr(StorageClass::Input, *member_ty);
                let pointer = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::AccessChain,
                    Some(member_pointer_ty),
                    Some(pointer),
                    vec![Operand::IdRef(*var), index.clone()],
                ));
                let member = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::Load,
                    Some(*member_ty),
                    Some(member),
                    vec![Operand::IdRef(pointer)],
                ));
                members.push(Operand::IdRef(member));
            }
            rewritten.push(Instruction::new(
                Op::CompositeConstruct,
                Some(result_type),
                Some(result),
                members,
            ));
        }
        ctx.module.functions[entry_idx].blocks[block_idx].instructions = rewritten;
    }
    Ok(())
}

fn register_embedded_samplers(
    ctx: &mut Ctx,
    entry_idx: usize,
    embedded: Option<&[crate::meta::EmbeddedSampler]>,
) -> Result<(), String> {
    let descriptor_layout = ctx.descriptor_layout;
    let Some(embedded) = embedded else {
        return Ok(());
    };
    if embedded.is_empty() {
        return Ok(());
    }
    let sty = ctx.ty_sampler();
    let mut loads: Vec<Instruction> = vec![];
    let mut replacements = Vec::new();
    for samp in embedded.iter().copied() {
        let Some(buffer_root) = ctx.module.functions[entry_idx]
            .parameters
            .get(samp.buffer_param_index as usize)
            .and_then(|parameter| parameter.result_id)
        else {
            continue;
        };
        let uses: Vec<Word> = ctx
            .emit_sidecar
            .buffer_pointer_field_loads
            .iter()
            .filter(|fact| {
                fact.root == buffer_root && fact.byte_offset == u64::from(samp.field_offset)
            })
            .map(|fact| fact.id)
            .collect();
        let pptr = ctx.ty_ptr(StorageClass::UniformConstant, sty);
        let var = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pptr),
            Some(var),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        let binding =
            embedded_sampler_resource_binding(descriptor_layout, samp.synthetic_sampler_index)?;
        decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
        ctx.interface_buffer_var(var);
        let lid = ctx.module.fresh_id();
        loads.push(Instruction::new(
            Op::Load,
            Some(sty),
            Some(lid),
            vec![Operand::IdRef(var)],
        ));
        replacements.extend(uses.into_iter().map(|id| (id, lid)));
    }
    for (placeholder, sampler) in replacements {
        replace_id_in_function(&mut ctx.module.functions[entry_idx], placeholder, sampler);
    }
    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        let at = first
            .instructions
            .iter()
            .position(|i| i.class.opcode != Op::Variable)
            .unwrap_or(first.instructions.len());
        for (k, ld) in loads.into_iter().enumerate() {
            first.instructions.insert(at + k, ld);
        }
    }
    Ok(())
}

pub(in crate::passes) fn bindless_heap_var(
    ctx: &mut Ctx,
    image_ty: Word,
    dims: (Dim, bool),
    comp: ImageComp,
) -> Word {
    if let Some(&var) = ctx.bindless_heap_vars.get(&image_ty) {
        return var;
    }
    let array = ctx.ty_runtime_array(image_ty);
    let pointer = ctx.ty_ptr(StorageClass::UniformConstant, array);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pointer),
        Some(var),
        vec![Operand::StorageClass(StorageClass::UniformConstant)],
    ));
    let binding = if dims.0 == Dim::DimBuffer {
        crate::reflect::BINDLESS_UTEXEL_BINDING
    } else {
        crate::reflect::BINDLESS_HEAP_BINDING
    };
    decorate_binding(
        &mut ctx.module,
        var,
        crate::reflect::BINDLESS_HEAP_SET,
        binding,
    );
    ctx.interface_buffer_var(var);
    ctx.image_array_vars
        .insert(var, (image_ty, dims, comp, false));
    ctx.bindless_heap_vars.insert(image_ty, var);
    var
}

pub(in crate::passes) fn bindless_sampler_heap_var(ctx: &mut Ctx) -> Word {
    let sty = ctx.ty_sampler();
    if let Some(&var) = ctx.bindless_heap_vars.get(&sty) {
        return var;
    }
    let array = ctx.ty_runtime_array(sty);
    let pointer = ctx.ty_ptr(StorageClass::UniformConstant, array);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pointer),
        Some(var),
        vec![Operand::StorageClass(StorageClass::UniformConstant)],
    ));
    decorate_binding(
        &mut ctx.module,
        var,
        crate::reflect::BINDLESS_HEAP_SET,
        crate::reflect::BINDLESS_SAMPLER_BINDING,
    );
    ctx.interface_buffer_var(var);
    ctx.bindless_heap_vars.insert(sty, var);
    var
}

pub(in crate::passes) fn bindless_storage_heap_var(
    ctx: &mut Ctx,
    image_ty: Word,
    dims: (Dim, bool),
    comp: ImageComp,
) -> Word {
    if let Some(&var) = ctx.bindless_heap_vars.get(&image_ty) {
        return var;
    }
    let array = ctx.ty_runtime_array(image_ty);
    let pointer = ctx.ty_ptr(StorageClass::UniformConstant, array);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pointer),
        Some(var),
        vec![Operand::StorageClass(StorageClass::UniformConstant)],
    ));
    let binding = if dims.0 == Dim::DimBuffer {
        crate::reflect::BINDLESS_STEXEL_BINDING
    } else {
        crate::reflect::BINDLESS_STORAGE_BINDING
    };
    decorate_binding(
        &mut ctx.module,
        var,
        crate::reflect::BINDLESS_HEAP_SET,
        binding,
    );
    ctx.interface_buffer_var(var);
    ctx.image_array_vars
        .insert(var, (image_ty, dims, comp, false));
    ctx.bindless_heap_vars.insert(image_ty, var);
    var
}

fn register_embedded_textures(
    ctx: &mut Ctx,
    entry_idx: usize,
    embedded: Option<&[crate::meta::EmbeddedTexture]>,
) -> Result<(), String> {
    let descriptor_layout = ctx.descriptor_layout;
    let Some(embedded) = embedded else {
        return Ok(());
    };
    if embedded.is_empty() {
        return Ok(());
    }
    let mut loads: Vec<Instruction> = vec![];
    let mut replacements = Vec::new();
    for tex in embedded.iter().copied() {
        if tex.array_length == Some(0) {
            continue;
        }
        let Some(buffer_root) = ctx.module.functions[entry_idx]
            .parameters
            .get(tex.buffer_param_index as usize)
            .and_then(|parameter| parameter.result_id)
        else {
            continue;
        };
        let (image_ty, runtime_specialization) = if let Some(format) = tex.storage_format {
            let (format, state) = ctx.specialize_storage_image_format(
                tex.synthetic_texture_index,
                format.to_spirv_format(),
                tex.comp,
            )?;
            (
                ctx.ty_storage_image(tex.dim, tex.arrayed, format, tex.comp),
                state,
            )
        } else {
            (ctx.ty_image(tex.dim, tex.arrayed, tex.comp), None)
        };
        if crate::reflect::bindless_fixed_fields_on()
            && tex.storage_format.is_none()
            && tex.array_length.is_none()
        {
            let off = u64::from(tex.field_offset);
            let heap_ids: std::collections::BTreeSet<Word> = ctx
                .emit_sidecar
                .buffer_pointer_heap_loads
                .iter()
                .filter(|f| f.root == buffer_root && f.byte_offset == off)
                .map(|f| f.id)
                .collect();
            let unrouted = ctx.emit_sidecar.buffer_pointer_field_loads.iter().any(|f| {
                f.root == buffer_root && f.byte_offset == off && !heap_ids.contains(&f.id)
            });
            if !heap_ids.is_empty() && !unrouted {
                let heap = bindless_heap_var(ctx, image_ty, (tex.dim, tex.arrayed), tex.comp);
                let facts: Vec<crate::emit_sidecar::BufferPointerHeapLoad> = ctx
                    .emit_sidecar
                    .buffer_pointer_heap_loads
                    .iter()
                    .filter(|f| heap_ids.contains(&f.id))
                    .cloned()
                    .collect();
                for fact in facts {
                    ctx.emit_sidecar
                        .buffer_pointer_dynamic_field_loads
                        .retain(|d| d.id != fact.id);
                    ctx.emit_sidecar.buffer_pointer_dynamic_field_loads.push(
                        crate::emit_sidecar::BufferPointerDynamicFieldLoad {
                            id: fact.id,
                            root: heap,
                            byte_offset: 0,
                            index: fact.slot,
                        },
                    );
                }
                ctx.emit_sidecar
                    .buffer_pointer_field_loads
                    .retain(|f| !heap_ids.contains(&f.id));
                continue;
            }
        }
        let binding_ty = tex
            .array_length
            .map(|length| ctx.ty_array(image_ty, length))
            .unwrap_or(image_ty);
        let pptr = ctx.ty_ptr(StorageClass::UniformConstant, binding_ty);
        let var = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pptr),
            Some(var),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        let binding = if tex.storage_format.is_some() {
            storage_texture_resource_binding(descriptor_layout, tex.synthetic_texture_index)?
        } else {
            texture_resource_binding(descriptor_layout, tex.synthetic_texture_index)?
        };
        decorate_binding(&mut ctx.module, var, descriptor_layout.set, binding);
        ctx.interface_buffer_var(var);
        if let Some(length) = tex.array_length {
            ctx.image_array_vars
                .insert(var, (image_ty, (tex.dim, tex.arrayed), tex.comp, false));
            ctx.register_runtime_storage_image_value(
                var,
                tex.synthetic_texture_index,
                runtime_specialization,
            );
            for fact in &mut ctx.emit_sidecar.buffer_pointer_field_loads {
                let end = u64::from(tex.field_offset) + u64::from(length) * 8;
                if fact.root == buffer_root
                    && fact.byte_offset >= u64::from(tex.field_offset)
                    && fact.byte_offset < end
                    && (fact.byte_offset - u64::from(tex.field_offset)) % 8 == 0
                {
                    fact.root = var;
                    fact.byte_offset -= u64::from(tex.field_offset);
                }
            }
            for fact in &mut ctx.emit_sidecar.buffer_pointer_dynamic_field_loads {
                if fact.root == buffer_root && fact.byte_offset == u64::from(tex.field_offset) {
                    fact.root = var;
                    fact.byte_offset = 0;
                }
            }
            continue;
        }
        if tex.storage_format.is_none() {
            let heap_facts: Vec<crate::emit_sidecar::BufferPointerHeapLoad> = ctx
                .emit_sidecar
                .buffer_pointer_heap_loads
                .iter()
                .filter(|fact| {
                    fact.root == buffer_root && fact.byte_offset == u64::from(tex.field_offset)
                })
                .cloned()
                .collect();
            if !heap_facts.is_empty() {
                let heap = bindless_heap_var(ctx, image_ty, (tex.dim, tex.arrayed), tex.comp);
                for fact in heap_facts {
                    ctx.emit_sidecar
                        .buffer_pointer_dynamic_field_loads
                        .retain(|dynamic| dynamic.id != fact.id);
                    ctx.emit_sidecar.buffer_pointer_dynamic_field_loads.push(
                        crate::emit_sidecar::BufferPointerDynamicFieldLoad {
                            id: fact.id,
                            root: heap,
                            byte_offset: 0,
                            index: fact.slot,
                        },
                    );
                }
            }
        }
        let lid = ctx.module.fresh_id();
        loads.push(Instruction::new(
            Op::Load,
            Some(image_ty),
            Some(lid),
            vec![Operand::IdRef(var)],
        ));
        ctx.image_dims.insert(lid, (tex.dim, tex.arrayed));
        ctx.image_comp.insert(lid, tex.comp);
        if tex.storage_format.is_some() {
            ctx.image_storage.insert(lid);
            ctx.register_runtime_storage_image_value(
                var,
                tex.synthetic_texture_index,
                runtime_specialization,
            );
            ctx.register_runtime_storage_image_value(
                lid,
                tex.synthetic_texture_index,
                runtime_specialization,
            );
        }
        replacements.extend(
            ctx.emit_sidecar
                .buffer_pointer_field_loads
                .iter()
                .filter(|fact| {
                    fact.root == buffer_root && fact.byte_offset == u64::from(tex.field_offset)
                })
                .map(|fact| (fact.id, lid)),
        );
    }
    for (placeholder, image) in replacements {
        replace_id_in_function(&mut ctx.module.functions[entry_idx], placeholder, image);
    }
    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        let at = first
            .instructions
            .iter()
            .position(|i| i.class.opcode != Op::Variable)
            .unwrap_or(first.instructions.len());
        for (k, ld) in loads.into_iter().enumerate() {
            first.instructions.insert(at + k, ld);
        }
    }
    Ok(())
}

impl Ctx {
    fn clone_type_for_record_array_element(
        &mut self,
        ty: Word,
        defs: &HashMap<Word, Instruction>,
    ) -> Word {
        let mut memo = HashMap::new();
        self.clone_type_for_record_array_element_inner(ty, defs, &mut memo)
    }

    fn clone_type_for_record_array_element_inner(
        &mut self,
        ty: Word,
        defs: &HashMap<Word, Instruction>,
        memo: &mut HashMap<Word, Word>,
    ) -> Word {
        if let Some(&cloned) = memo.get(&ty) {
            return cloned;
        }
        let Some(def) = defs.get(&ty).cloned() else {
            return ty;
        };
        match def.class.opcode {
            Op::TypeStruct => {
                let members = def
                    .operands
                    .iter()
                    .map(|op| match op {
                        Operand::IdRef(member_ty) => Operand::IdRef(
                            self.clone_type_for_record_array_element_inner(*member_ty, defs, memo),
                        ),
                        other => other.clone(),
                    })
                    .collect::<Vec<_>>();
                let cloned = self.module.fresh_id();
                memo.insert(ty, cloned);
                if let Some(offsets) = self.air_struct_offsets.get(&ty).cloned() {
                    self.air_struct_offsets.insert(cloned, offsets);
                }
                self.new_globals
                    .push(type_inst(Op::TypeStruct, cloned, members));
                cloned
            }
            Op::TypeArray | Op::TypeRuntimeArray => {
                let Some(Operand::IdRef(elem)) = def.operands.first() else {
                    return ty;
                };
                let cloned_elem = self.clone_type_for_record_array_element_inner(*elem, defs, memo);
                if cloned_elem == *elem {
                    return ty;
                }
                let mut operands = def.operands.clone();
                operands[0] = Operand::IdRef(cloned_elem);
                let cloned = self.module.fresh_id();
                memo.insert(ty, cloned);
                self.new_globals
                    .push(type_inst(def.class.opcode, cloned, operands));
                cloned
            }
            _ => ty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_buffer_alias_elements_include_numeric_vectors() {
        let defs = HashMap::from([
            (
                1,
                Instruction::new(
                    Op::TypeFloat,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(32)],
                ),
            ),
            (
                2,
                Instruction::new(
                    Op::TypeVector,
                    None,
                    Some(2),
                    vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
                ),
            ),
            (
                3,
                Instruction::new(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(1)]),
            ),
        ]);

        assert!(buffer_typed_alias_element(&defs, 1));
        assert!(buffer_typed_alias_element(&defs, 2));
        assert!(!buffer_typed_alias_element(&defs, 3));
    }
    use crate::spirv_module::Instruction;
    use crate::spirv_module::ModuleHeader;
    use crate::spirv_module::Operand;
    use spirv::Op;

    fn ty(op: Op, id: u32, operands: Vec<Operand>) -> Instruction {
        Instruction::new(op, None, Some(id), operands)
    }

    #[test]
    fn tess_coord_binding_preserves_float3_and_only_truncates_float2() {
        let mut defs = HashMap::new();
        defs.insert(1, ty(Op::TypeFloat, 1, vec![Operand::LiteralBit32(32)]));
        defs.insert(
            2,
            ty(
                Op::TypeVector,
                2,
                vec![Operand::IdRef(1), Operand::LiteralBit32(2)],
            ),
        );
        defs.insert(
            3,
            ty(
                Op::TypeVector,
                3,
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
        );
        defs.insert(
            4,
            ty(
                Op::TypeVector,
                4,
                vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
            ),
        );

        assert_eq!(tess_coord_prefix_lanes(&defs, 3, 1), Ok(None));
        assert_eq!(tess_coord_prefix_lanes(&defs, 2, 1), Ok(Some(2)));
        assert!(tess_coord_prefix_lanes(&defs, 4, 1).is_err());
    }

    #[test]
    fn fragment_input_needs_flat_only_for_integer_and_double() {
        let mut defs = HashMap::new();
        defs.insert(
            1,
            ty(
                Op::TypeInt,
                1,
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
        );
        defs.insert(2, ty(Op::TypeFloat, 2, vec![Operand::LiteralBit32(32)]));
        defs.insert(3, ty(Op::TypeFloat, 3, vec![Operand::LiteralBit32(64)]));
        defs.insert(
            4,
            ty(
                Op::TypeVector,
                4,
                vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
            ),
        );
        defs.insert(
            5,
            ty(
                Op::TypeVector,
                5,
                vec![Operand::IdRef(2), Operand::LiteralBit32(2)],
            ),
        );
        defs.insert(
            6,
            ty(
                Op::TypeVector,
                6,
                vec![Operand::IdRef(3), Operand::LiteralBit32(3)],
            ),
        );
        assert!(fragment_input_needs_flat(&defs, 1), "uint scalar");
        assert!(!fragment_input_needs_flat(&defs, 2), "float32 scalar");
        assert!(fragment_input_needs_flat(&defs, 3), "double scalar");
        assert!(fragment_input_needs_flat(&defs, 4), "uint vector");
        assert!(!fragment_input_needs_flat(&defs, 5), "float32 vector");
        assert!(fragment_input_needs_flat(&defs, 6), "double vector");
    }

    #[test]
    fn type_contains_bool_descends_through_vectors() {
        let mut defs = HashMap::new();
        defs.insert(1, ty(Op::TypeBool, 1, vec![]));
        defs.insert(2, ty(Op::TypeFloat, 2, vec![Operand::LiteralBit32(32)]));
        defs.insert(
            3,
            ty(
                Op::TypeVector,
                3,
                vec![Operand::IdRef(1), Operand::LiteralBit32(2)],
            ),
        );
        defs.insert(
            4,
            ty(
                Op::TypeVector,
                4,
                vec![Operand::IdRef(2), Operand::LiteralBit32(2)],
            ),
        );

        assert!(type_contains_bool(&defs, 1));
        assert!(type_contains_bool(&defs, 3));
        assert!(!type_contains_bool(&defs, 2));
        assert!(!type_contains_bool(&defs, 4));
    }

    #[test]
    fn layout_types_reachable_from_walks_nested_struct_and_array_members() {
        let mut defs = HashMap::new();
        defs.insert(
            1,
            ty(
                Op::TypeInt,
                1,
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
        );
        defs.insert(2, ty(Op::TypeStruct, 2, vec![Operand::IdRef(1)]));
        defs.insert(3, ty(Op::TypeStruct, 3, vec![Operand::IdRef(1)]));
        defs.insert(
            4,
            ty(Op::TypeArray, 4, vec![Operand::IdRef(3), Operand::IdRef(1)]),
        );
        defs.insert(
            5,
            ty(
                Op::TypeStruct,
                5,
                vec![Operand::IdRef(1), Operand::IdRef(2), Operand::IdRef(4)],
            ),
        );
        defs.insert(6, ty(Op::TypeStruct, 6, vec![Operand::IdRef(1)]));

        let roots: HashSet<Word> = [5].into_iter().collect();
        let reachable = layout_types_reachable_from(&roots, &defs);

        assert!(reachable.contains(&5), "root block struct");
        assert!(reachable.contains(&2), "directly nested struct");
        assert!(reachable.contains(&3), "struct nested through an array");
        assert!(reachable.contains(&4), "array on the layout path");
        assert!(!reachable.contains(&1), "scalars are not layout composites");
        assert!(!reachable.contains(&6), "unrelated struct not reachable");
    }

    #[test]
    fn split_workgroup_layout_aliases_clones_nested_aggregate_paths() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(50));
        module.types_global_values = vec![
            ty(
                Op::TypeInt,
                1,
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(2),
                vec![Operand::LiteralBit32(4)],
            ),
            ty(Op::TypeArray, 3, vec![Operand::IdRef(1), Operand::IdRef(2)]),
            ty(Op::TypeStruct, 4, vec![Operand::IdRef(3)]),
            ty(Op::TypeStruct, 5, vec![Operand::IdRef(4)]),
            ty(Op::TypeStruct, 6, vec![Operand::IdRef(4)]),
            ty(Op::TypeArray, 7, vec![Operand::IdRef(6), Operand::IdRef(2)]),
            ty(
                Op::TypePointer,
                8,
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(7),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(8),
                Some(9),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            ty(
                Op::TypePointer,
                10,
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(4),
                ],
            ),
            ty(
                Op::TypePointer,
                11,
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            ty(
                Op::TypePointer,
                12,
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(12),
                Some(13),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];

        let mut defs = module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
            .collect::<HashMap<_, _>>();
        let mut ctx = Ctx::new(module);
        split_explicit_layout_type_aliases(&mut ctx, &[(13, 5)], &mut defs);

        let pointer_pointee = |id| {
            defs.get(&id)
                .and_then(|inst| inst.operands.get(1))
                .and_then(|operand| match operand {
                    Operand::IdRef(pointee) => Some(*pointee),
                    _ => None,
                })
                .expect("pointer pointee")
        };
        let workgroup_root = pointer_pointee(8);
        let function_pointee = pointer_pointee(10);
        assert_ne!(workgroup_root, 7, "array-root Workgroup graph cloned");
        assert_ne!(
            function_pointee, 4,
            "aggregate-copy Function pointer cloned"
        );
        assert_eq!(pointer_pointee(11), 4, "StorageBuffer keeps laid-out type");
        assert_eq!(pointer_pointee(12), 5, "Block pointer remains unchanged");

        let cloned_graph =
            layout_types_reachable_from(&[workgroup_root].into_iter().collect(), &defs);
        assert!(!cloned_graph.contains(&3), "shared decorated array removed");
        assert!(
            !cloned_graph.contains(&4),
            "shared decorated struct removed"
        );
        let root_pos = ctx
            .module
            .types_global_values
            .iter()
            .position(|inst| inst.result_id == Some(workgroup_root))
            .expect("cloned root definition");
        let pointer_pos = ctx
            .module
            .types_global_values
            .iter()
            .position(|inst| inst.result_id == Some(8))
            .expect("Workgroup pointer definition");
        assert!(root_pos < pointer_pos, "clone defined before pointer use");
        assert_eq!(ctx.ty_ptr(StorageClass::Workgroup, workgroup_root), 8);
        assert_ne!(ctx.ty_ptr(StorageClass::Workgroup, 7), 8);
    }

    #[test]
    fn split_explicit_layout_aliases_isolates_function_only_structs() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            ty(
                Op::TypeInt,
                1,
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            ty(Op::TypeStruct, 2, vec![Operand::IdRef(1)]),
            ty(
                Op::TypePointer,
                3,
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(2),
                ],
            ),
            ty(
                Op::TypePointer,
                4,
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            ty(
                Op::TypePointer,
                7,
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(5),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(Op::ConstantNull, Some(2), Some(6), vec![]),
        ];

        let mut defs = module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
            .collect::<HashMap<_, _>>();
        let mut ctx = Ctx::new(module);
        split_explicit_layout_type_aliases(&mut ctx, &[(5, 2)], &mut defs);

        let pointee = |id| match defs.get(&id).and_then(|inst| inst.operands.get(1)) {
            Some(Operand::IdRef(pointee)) => *pointee,
            _ => panic!("pointer pointee"),
        };
        assert_ne!(pointee(3), 2, "Function pointer receives undecorated clone");
        assert_eq!(
            pointee(7),
            pointee(3),
            "Private pointer receives the same undecorated clone"
        );
        assert_eq!(pointee(4), 2, "StorageBuffer keeps laid-out type");
        assert_eq!(
            ctx.module
                .types_global_values
                .iter()
                .find(|instruction| instruction.result_id == Some(6))
                .and_then(|instruction| instruction.result_type),
            Some(pointee(3)),
            "pre-existing aggregate values follow the unlaid Function type"
        );
    }

    #[test]
    fn split_explicit_layout_aliases_isolates_nested_block_root() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            ty(
                Op::TypeInt,
                1,
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            ty(Op::TypeStruct, 2, vec![Operand::IdRef(1)]),
            ty(Op::TypeStruct, 3, vec![Operand::IdRef(2)]),
            ty(
                Op::TypePointer,
                4,
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(5),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            ty(
                Op::TypePointer,
                6,
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        let mut defs = module
            .types_global_values
            .iter()
            .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
            .collect::<HashMap<_, _>>();
        let mut ctx = Ctx::new(module);

        split_explicit_layout_type_aliases(&mut ctx, &[(5, 2), (7, 3)], &mut defs);

        let nested = match defs
            .get(&3)
            .and_then(|definition| definition.operands.first())
        {
            Some(Operand::IdRef(member)) => *member,
            _ => panic!("outer block member"),
        };
        assert_ne!(nested, 2, "nested occurrence receives a non-Block clone");
        assert_eq!(
            defs.get(&nested).map(|definition| definition.class.opcode),
            Some(Op::TypeStruct)
        );
        assert_eq!(
            defs.get(&2)
                .and_then(|definition| definition.operands.first()),
            Some(&Operand::IdRef(1)),
            "independently bound root remains unchanged"
        );
    }
}
