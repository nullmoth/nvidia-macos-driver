mod air_text;
mod async_copy;
mod cfg;
#[cfg(test)]
mod cfg_testkit;
mod constfold;
mod atomicrmw_text;
mod diagnostics;
mod dynamic_memcpy;
mod emitter;
mod emit_tiers;
mod imageblock;
mod inline;
pub(crate) mod ir;
mod lex;
mod opaque_image_select;
mod owned_cfg;
mod parse;
mod private_vector_word;
mod psb;
mod psb_value_select;
pub(crate) mod ray_intersection;
mod reloop_nest;
mod relooper;
mod render;
mod rewrites;
mod vec_scalar_merge;
mod atomic_image_format;
mod copy_memory_logical;
mod stencil_export;
mod wg_atomic;

mod tir;
mod cl_constant;

use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
pub(crate) fn lower_air_text(san_ll: &str) -> std::borrow::Cow<'_, str> {
    let lowered = match cl_constant::lower_cl_constant_args(san_ll) {
        std::borrow::Cow::Borrowed(borrowed) => async_copy::lower_simdgroup_async_copy(borrowed),
        std::borrow::Cow::Owned(owned) => std::borrow::Cow::Owned(async_copy::lower_simdgroup_async_copy_owned(owned)),
    };
    let lowered = match lowered {
        std::borrow::Cow::Borrowed(borrowed) => {
            dynamic_memcpy::lower_dynamic_length_memcpy(borrowed)
        }
        std::borrow::Cow::Owned(owned) => {
            std::borrow::Cow::Owned(dynamic_memcpy::lower_dynamic_length_memcpy_owned(owned))
        }
    };
    let lowered = match lowered {
        std::borrow::Cow::Borrowed(borrowed) => atomicrmw_text::lower_raw_atomicrmw(borrowed),
        std::borrow::Cow::Owned(owned) => {
            std::borrow::Cow::Owned(atomicrmw_text::lower_raw_atomicrmw_owned(owned))
        }
    };
    match lowered {
        std::borrow::Cow::Borrowed(borrowed) => inline::forward_builder_pointer_fields(borrowed),
        std::borrow::Cow::Owned(owned) => match inline::forward_builder_pointer_fields(&owned) {
            std::borrow::Cow::Borrowed(_) => std::borrow::Cow::Owned(owned),
            std::borrow::Cow::Owned(forwarded) => std::borrow::Cow::Owned(forwarded),
        },
    }
}

pub(crate) fn lower_air_text_owned(san_ll: String) -> String {
    let lowered = atomicrmw_text::lower_raw_atomicrmw_owned(dynamic_memcpy::lower_dynamic_length_memcpy_owned(
        async_copy::lower_simdgroup_async_copy_owned(match cl_constant::lower_cl_constant_args(&san_ll) {
            std::borrow::Cow::Borrowed(_) => san_ll,
            std::borrow::Cow::Owned(lowered) => lowered,
        }),
    ));
    match inline::forward_builder_pointer_fields(&lowered) {
        std::borrow::Cow::Borrowed(_) => lowered,
        std::borrow::Cow::Owned(forwarded) => forwarded,
    }
}
pub use diagnostics::{
    cond_other_witness_report, irreducible_region_report, param_pointee_check,
    straddle_region_report, straddle_witness_report, structured_reject_loop_classes,
    structured_reject_reasons, tir_gep_pointee_report, tir_pointee_check, tir_self_check,
    tir_storage_check, tir_structured_self_check, ParamPointeeStats, PointeeCheckStats,
    StorageCheckStats, TirCheckStats,
};
#[cfg(test)]
pub(in crate::native) use emit_tiers::emit_vulkan_spirv_from_typed_blocks;
pub use emit_tiers::{
    emit_vulkan_spirv, emit_vulkan_spirv_all_buffers_raw, emit_vulkan_spirv_all_buffers_raw_bda,
    emit_vulkan_spirv_all_buffers_raw_relooper_feed,
    emit_vulkan_spirv_all_buffers_raw_with_workgroup,
    emit_vulkan_spirv_with_primitive_phi_metadata,
};
pub(crate) use emit_tiers::{
    emit_vulkan_spirv_all_buffers_raw_bda_with_sidecar,
    emit_vulkan_spirv_all_buffers_raw_relooper_feed_with_sidecar,
    emit_vulkan_spirv_all_buffers_raw_with_sidecar,
    emit_vulkan_spirv_all_buffers_raw_with_workgroup_sidecar, emit_vulkan_spirv_with_outcome,
    emit_vulkan_spirv_with_sidecar,
};
use emitter::Emitter;
use ir::LlModule;

pub(crate) fn inline_direct_function_pointer_consumers(
    san_ll: &str,
    direct_functions: &std::collections::HashSet<String>,
) -> String {
    inline::inline_direct_function_pointer_consumers(san_ll, direct_functions)
}

pub(crate) use owned_cfg::{
    owned_module_failure, owned_module_failures, scalar_width_capability, OwnedModuleFailure,
};
pub(crate) use parse::{
    parse_return_type as parse_llvm_return_type, parse_type_prefix as parse_llvm_type_prefix,
};
pub(crate) use rewrites::close_private_vector_word_views_module;
#[cfg(test)]
pub(crate) use rewrites::{address_construction_count, reset_address_construction_counts};
pub(crate) use rewrites::{
    close_inlined_bda_pointer_tables_module, construct_cfg_functions_module,
    construct_interface_cross_binding_pointer_merges_module,
    construct_interface_cross_binding_pointer_phis_module,
    construct_interface_cross_binding_pointer_values_module, construct_opaque_image_selects_module,
    construct_atomic_storage_image_formats_module,
    split_mismatched_copy_memory_module,
    construct_physical_atomic_pointer_lvalues_module, construct_workgroup_atomic_floats_module,
    eliminate_dead_pointer_values_module, eliminate_dead_values_module,
    lower_unobserved_bda_aggregate_pointer_fields_module, prune_constant_branches_module,
    prune_constant_cfg_module_if_changed, prune_unused_null_and_undef_constants_module,
    unowned_selection_header_labels,
};
pub use rewrites::{BOUNDED_RELOOPER_MAX_BLOCKS, CFG_EMIT_RELOOPER_MAX_BLOCKS};
use spirv::{Capability, Op, StorageClass, Word};
use std::collections::{HashMap, HashSet};

fn add_native_module_capabilities(module: &mut Module) {
    crate::spirv_variable_ptr::lower_storage_buffer_pointer_phis(module);
    crate::spirv_variable_ptr::lower_zero_base_storage_buffer_ptr_access_chains(module);
    let (has_storage_buffer_pointer_merge, has_other_pointer_merge) =
        crate::spirv_variable_ptr::variable_pointer_requirement(module);
    module
        .capabilities
        .retain(|inst| match inst.operands.as_slice() {
            [Operand::Capability(Capability::VariablePointersStorageBuffer)] => {
                has_storage_buffer_pointer_merge
            }
            [Operand::Capability(Capability::VariablePointers)] => has_other_pointer_merge,
            _ => true,
        });
    if has_storage_buffer_pointer_merge {
        require_capability(module, Capability::VariablePointersStorageBuffer);
    }
    if has_other_pointer_merge {
        require_capability(module, Capability::VariablePointers);
    }
}

fn require_capability(module: &mut Module, capability: Capability) {
    if module.capabilities.iter().any(|inst| {
        matches!(
            inst.operands.as_slice(),
            [Operand::Capability(existing)] if *existing == capability
        )
    }) {
        return;
    }
    module.capabilities.push(Instruction::new(
        Op::Capability,
        None,
        None,
        vec![Operand::Capability(capability)],
    ));
}

#[cfg(test)]
mod tests;

pub(crate) fn requires_device_address_model_for_source(
    san_ll: &str,
    kern: Option<&crate::meta::KernMeta>,
    entry: Option<&str>,
) -> bool {
    let san_ll = lower_air_text(san_ll);
    let san_ll = vec_scalar_merge::lower_vector_scalar_pointer_merge(&san_ll);
    let san_ll = inline::inline_pointer_select_consumers(&san_ll, entry).source;
    ir::LlModule::parse_with_stage_meta(&san_ll, kern, entry)
        .is_ok_and(|parsed| emit_tiers::requires_device_address_model(&parsed))
}

/// See inline::inline_and_forward_object_fields: the retry form of a module that failed as written.
pub(crate) fn inline_and_forward_object_fields(san_ll: &str) -> Option<String> {
    inline::inline_and_forward_object_fields(san_ll)
}

/// See stencil_export: a fragment stencil export lowered to a per-bit discard the driver replays.
pub(crate) fn lower_stencil_export_module(module: &mut crate::spirv_module::Module) -> bool {
    stencil_export::lower_stencil_export(module)
}
