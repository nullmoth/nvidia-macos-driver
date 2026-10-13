#![allow(unused_imports)]
use super::super::cfg::{
    id_ref_operand, infer_branch_merges, infer_loop_merges, infer_switch_merges,
    lower_unstructured_switches, split_body_blocks, BodyBlock,
};
use super::super::emit_vulkan_spirv;
use super::super::emitter::Emitter;
use super::super::ir::{LlType, LlValue};
use super::super::parse::{parse_type, parse_typed_value};
use super::*;
use crate::passes::{self, Stage};
use crate::spirv_module::load_bytes;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Instruction};
use crate::{disassemble, meta, tools};
use spirv::{Capability, Decoration, Op, Scope, SelectionControl, StorageClass, Word};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[test]
fn native_null_rooted_pointer_recurrence_is_valid_by_construction() {
    let ll = r#"
define void @walk(ptr addrspace(1) %flag) {
entry:
  %raw = load i32, ptr addrspace(1) %flag, align 4
  %again = icmp ne i32 %raw, 0
  br label %loop

loop:
  %cursor = phi ptr addrspace(2) [ null, %entry ], [ %next, %loop ]
  %next = getelementptr float, ptr addrspace(2) %cursor, i64 1
  %isnull = icmp eq ptr addrspace(2) %cursor, null
  %encoded = zext i1 %isnull to i32
  store i32 %encoded, ptr addrspace(1) %flag, align 4
  br i1 %again, label %loop, label %done

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @walk, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"flag"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_null_rooted_pointer_recurrence_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = emit_vulkan_spirv(ll).expect("emit null-rooted pointer recurrence");
    let module = load_bytes(&spv).expect("load spv");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::TypePointer)
                .then_some(inst.result_id)
                .flatten()
        })
        .collect::<HashSet<_>>();
    assert!(
        module.all_inst_iter().all(|inst| {
            inst.class.opcode != Op::Phi
                || !inst
                    .result_type
                    .is_some_and(|ty| pointer_types.contains(&ty))
        }),
        "null-rooted recurrence must not emit a logical pointer OpPhi"
    );
    assert!(
        module.types_global_values.iter().any(|inst| {
            inst.class.opcode == Op::Undef
                && inst
                    .result_type
                    .is_some_and(|ty| pointer_types.contains(&ty))
        }),
        "null-rooted recurrence must use a correctly typed global pointer payload"
    );
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect::<Vec<_>>();

    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_whole_object_copy_into_a_device_buffer_is_a_store_not_a_copy_memory() {
    let ll = r#"
source_filename = "case.metal"

%struct.Pair = type { float, float }

define void @copy_into_device(ptr addrspace(1) noundef captures(none) "air-buffer-no-alias" %0) {
  %2 = alloca %struct.Pair, align 4
  %3 = getelementptr inbounds %struct.Pair, ptr addrspace(1) %0, i64 1
  call void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) %3, ptr %2, i64 8, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p0.i64(ptr addrspace(1), ptr, i64, i1)

attributes #0 = { nounwind }

!air.kernel = !{!0}
!0 = !{ptr @copy_into_device, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Pair", !"air.arg_name", !"pairs"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_device_copy_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_reinterprets_metadata_ulong_field_as_uint2() {
    let ll = r#"
source_filename = "case.metal"

%struct.CCDebugInfo = type { %union.anon.121 }
%union.anon.121 = type { <2 x i32> }

define void @CC_LogComputeFunction(ptr addrspace(1) noundef readonly align 8 captures(none) dereferenceable(8) "air-buffer-no-alias" %0, ptr addrspace(1) noundef writeonly align 8 captures(none) dereferenceable(8) "air-buffer-no-alias" %1) local_unnamed_addr #0 {
  %3 = getelementptr inbounds %struct.CCDebugInfo, ptr addrspace(1) %0, i64 0, i32 0, i32 0
  %4 = load <2 x i32>, ptr addrspace(1) %3, align 8
  %5 = getelementptr inbounds %struct.CCDebugInfo, ptr addrspace(1) %1, i64 0, i32 0, i32 0
  store <2 x i32> %4, ptr addrspace(1) %5, align 8
  ret void
}

attributes #0 = { nounwind }

!air.kernel = !{!0}
!0 = !{ptr @CC_LogComputeFunction, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"CCDebugInfo", !"air.arg_name", !"logInfo"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 8, i32 0, !"CCDebugInfo::(anonymous)", !""}
!5 = !{i32 0, i32 8, i32 0, !"uint2", !"frame_id_uint", i32 0, i32 8, i32 0, !"ulong", !"frame_id"}
!6 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"CCDebugInfo", !"air.arg_name", !"logBuffer"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_uint2_ulong_reinterpret_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(!asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(!asm.contains("OpBitwiseOr"), "{asm}");
    assert!(!asm.contains("OpTypeInt 64"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_primary_device_pointer_load_store_deref_lowers_to_physical() {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %in) {
entry:
  %p = load ptr addrspace(1), ptr addrspace(1) %in, align 8
  %isnull = icmp eq ptr addrspace(1) %p, null
  br i1 %isnull, label %done, label %write

write:
  store ptr addrspace(1) %p, ptr addrspace(1) %out, align 8
  %wide = load i64, ptr addrspace(1) %out, align 8
  %narrow = trunc i64 %wide to i32
  %wide_gep = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %wide
  %mixed_gep = getelementptr inbounds i8, ptr addrspace(1) %wide_gep, i32 %narrow
  %f = load float, ptr addrspace(1) %mixed_gep, align 4
  store float %f, ptr addrspace(1) %out, align 4
  br label %done

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"in"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("primary emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("PhysicalStorageBuffer64"),
        "expected PhysicalStorageBuffer64 addressing model:\n{asm}"
    );
    assert!(
        asm.contains("OpCapability PhysicalStorageBufferAddresses"),
        "{asm}"
    );
    assert!(
        asm.contains("OpConvertUToPtr"),
        "expected a device-address deref:\n{asm}"
    );
    assert!(
        asm.contains("OpIEqual"),
        "loaded device-address nullness must be constructed from its integer value:\n{asm}"
    );
    assert!(
        asm.contains("OpSConvert"),
        "mixed-width GEP composition must sign-extend its i32 offset:\n{asm}"
    );
}

#[test]
fn native_device_address_model_distinguishes_memory_from_opaque_handles() {
    let memory = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %in) {
entry:
  %p = load ptr addrspace(1), ptr addrspace(1) %in, align 8
  %v = load i32, ptr addrspace(1) %p, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

"#;
    let opaque = r#"
define i32 @k(i64 %handle) {
entry:
  %texture = inttoptr i64 %handle to ptr addrspace(1)
  %width = call i32 @air.get_width_texture_2d(ptr addrspace(1) %texture, i32 0)
  ret i32 %width
}
declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
"#;
    let memory = crate::native::ir::LlModule::parse(memory).expect("parse memory pointer");
    let opaque = crate::native::ir::LlModule::parse(opaque).expect("parse opaque handle");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &memory
    ));
    assert!(!crate::native::emit_tiers::requires_device_address_model(
        &opaque
    ));
}

#[test]
fn native_device_buffer_array_stores_materialized_direct_address_in_local_aggregate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%ArrayRef = type { ptr, i64 }
%State = type { %ArrayRef }
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers) {
entry:
  %buffer = load ptr addrspace(1), ptr %buffers, align 8
  store i32 7, ptr addrspace(1) %buffer, align 4
  call void @scan(ptr %buffers)
  ret void
}

define internal void @scan(ptr %buffers) {
entry:
  %state = alloca %State, align 8
  call void @init(ptr %state, ptr %buffers)
  ret void
}

define internal void @init(ptr %state, ptr %buffers) {
entry:
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0, i32 0
  store ptr %buffers, ptr %field, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let parsed = crate::native::ir::LlModule::parse(ll).expect("parse device-buffer-array AIR");
    assert!(
        crate::native::emit_tiers::requires_device_address_model(&parsed),
        "device-buffer-array pointer loads require physical addresses"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_device_buffer_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    let address_table = module
        .annotations
        .iter()
        .find_map(|instruction| match instruction.operands.as_slice() {
            [
                Operand::IdRef(id),
                Operand::Decoration(Decoration::Binding),
                Operand::LiteralBit32(640),
            ] => Some(*id),
            _ => None,
        })
        .expect("buffer-address table binding");
    let constants = module
        .types_global_values
        .iter()
        .filter_map(
            |instruction| match (instruction.result_id, instruction.operands.as_slice()) {
                (Some(id), [Operand::LiteralBit32(value)])
                    if instruction.class.opcode == Op::Constant =>
                {
                    Some((id, *value))
                }
                _ => None,
            },
        )
        .collect::<HashMap<_, _>>();
    let slot_zero_pointers = module
        .all_inst_iter()
        .filter_map(|instruction| {
            let operands = instruction.operands.as_slice();
            (matches!(
                instruction.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain
            ) && operands.first() == Some(&Operand::IdRef(address_table))
                && operands.len() == 4
                && matches!(operands[1], Operand::IdRef(id) if constants.get(&id) == Some(&0))
                && matches!(operands[2], Operand::IdRef(id) if constants.get(&id)
                    == Some(&32))
                && matches!(operands[3], Operand::IdRef(id) if constants.get(&id).is_some_and(|value| *value <= 1)))
            .then_some(instruction.result_id)
            .flatten()
        })
        .collect::<HashSet<_>>();
    let address_words = module
        .all_inst_iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::Load
                && matches!(instruction.operands.first(), Some(Operand::IdRef(pointer)) if slot_zero_pointers.contains(pointer)))
            .then_some(instruction.result_id)
            .flatten()
        })
        .collect::<HashSet<_>>();
    assert_eq!(address_words.len(), 2, "{}", disassemble(&spv).unwrap());

    let mut address_dependents = address_words;
    loop {
        let newly_dependent = module
            .all_inst_iter()
            .filter_map(|instruction| {
                let result = instruction.result_id?;
                (!address_dependents.contains(&result)
                    && instruction.operands.iter().any(
                        |operand| matches!(operand, Operand::IdRef(id) if address_dependents.contains(id)),
                    ))
                .then_some(result)
            })
            .collect::<Vec<_>>();
        if newly_dependent.is_empty() {
            break;
        }
        address_dependents.extend(newly_dependent);
    }
    assert!(
        module.all_inst_iter().any(|instruction| {
            instruction.class.opcode == Op::Store
                && matches!(instruction.operands.get(1), Some(Operand::IdRef(value)) if address_dependents.contains(value))
        }),
        "direct buffer address was not stored into the local aggregate:\n{}",
        disassemble(&spv).unwrap()
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_device_buffer_array_keeps_direct_buffer_pointer_phi_network_modeled() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers, ptr addrspace(1) %src, ptr addrspace(1) %dst, i1 %inner_again, i1 %outer_again) {
entry:
  %array_buffer = load ptr addrspace(1), ptr %buffers, align 8
  store i32 9, ptr addrspace(1) %array_buffer, align 4
  call void @copy_nested(ptr addrspace(1) %src, ptr addrspace(1) %dst, i1 %inner_again, i1 %outer_again)
  ret void
}

define internal void @copy_nested(ptr addrspace(1) %src, ptr addrspace(1) %dst, i1 %inner_again, i1 %outer_again) {
entry:
  br label %outer

outer:
  %src_outer = phi ptr addrspace(1) [ %src, %entry ], [ %src_inner, %inner_exit ]
  %dst_outer = phi ptr addrspace(1) [ %dst, %entry ], [ %dst_inner, %inner_exit ]
  br label %inner

inner:
  %src_inner = phi ptr addrspace(1) [ %src_outer, %outer ], [ %src_next, %inner_body ]
  %dst_inner = phi ptr addrspace(1) [ %dst_outer, %outer ], [ %dst_next, %inner_body ]
  br i1 %inner_again, label %inner_body, label %inner_exit

inner_body:
  %src_next = getelementptr inbounds i16, ptr addrspace(1) %src_inner, i64 1
  %dst_next = getelementptr inbounds i16, ptr addrspace(1) %dst_inner, i64 1
  br label %inner

inner_exit:
  %value = load i16, ptr addrspace(1) %src_inner, align 2
  store i16 %value, ptr addrspace(1) %dst_inner, align 2
  br i1 %outer_again, label %outer, label %exit

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 2, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"ushort", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 2, !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"ushort", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_bda_direct_pointer_phi_network_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load translated module");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(
        asm.contains("Binding 640"),
        "direct-buffer addresses were pruned:\n{asm}"
    );
    let i16_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypeInt
                && matches!(
                    instruction.operands.first(),
                    Some(Operand::LiteralBit32(16))
                ))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    let loaded_i16 = module
        .all_inst_iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::Load
                && instruction
                    .result_type
                    .is_some_and(|ty| i16_types.contains(&ty)))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(
        !loaded_i16.is_empty(),
        "direct source load was replaced with zero:\n{asm}"
    );
    assert!(
        module.all_inst_iter().any(|instruction| {
            instruction.class.opcode == Op::Store
                && matches!(instruction.operands.get(1), Some(Operand::IdRef(value)) if loaded_i16.contains(value))
        }),
        "direct destination store was discarded:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_device_buffer_array_through_a_generic_state_pointer_stores_through_the_table() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%ArrayRef = type { ptr, i64 }
%State = type { %ArrayRef, %ArrayRef }

define void @k(ptr %buffers) {
entry:
  %state = alloca %State, align 8
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0, i32 0
  store ptr %buffers, ptr %field, align 8
  %secondary = insertvalue %State poison, ptr null, 1, 0
  call void @write(ptr %state)
  ret void
}

define internal void @write(ptr %state) {
entry:
  %opaque = bitcast ptr %state to ptr
  %table = load ptr, ptr %opaque, align 8
  %buffer = load ptr addrspace(1), ptr %table, align 8
  store i32 7, ptr addrspace(1) %buffer, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_device_buffer_array_null_aggregate_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect(
        "the bitcast spelling reads the table as a device address now, so the store must translate",
    );
    let gep = ll.replace(
        "  %opaque = bitcast ptr %state to ptr\n",
        "  %opaque = getelementptr inbounds %State, ptr %state, i64 0, i32 0, i32 0\n",
    );
    assert_ne!(gep, ll, "the GEP spelling must differ from the bitcast one");
    let gep_spv = crate::translate_sanitized_native(&gep, Stage::Kernel, &tmp)
        .expect("the GEP spelling is batch 32's pointer-table path and must translate");
    let module = load_bytes(&spv).expect("load translated module");
    let asm = disassemble(&spv).expect("disassemble");
    let ids = |opcode: Op, keep: &dyn Fn(&Instruction) -> bool| {
        module
            .types_global_values
            .iter()
            .filter(|instruction| instruction.class.opcode == opcode && keep(instruction))
            .filter_map(|instruction| instruction.result_id)
            .collect::<HashSet<Word>>()
    };
    let psb = ids(Op::TypePointer, &|instruction| {
        matches!(
            instruction.operands.first(),
            Some(Operand::StorageClass(StorageClass::PhysicalStorageBuffer))
        )
    });
    let sevens = ids(Op::Constant, &|instruction| {
        matches!(instruction.operands.first(), Some(Operand::LiteralBit32(7)))
    });
    let nulls = ids(Op::ConstantNull, &|_| true);
    let mut address_of = HashMap::<Word, Word>::new();
    let mut loaded_from = HashMap::<Word, Word>::new();
    let zeros = ids(Op::Constant, &|instruction| {
        matches!(instruction.operands.first(), Some(Operand::LiteralBit32(0)))
    });
    let mut converted_ty = HashMap::<Word, Word>::new();
    let mut b78_wrapper_types = Vec::<Word>::new();
    for instruction in module.all_inst_iter() {
        match (
            instruction.class.opcode,
            instruction.result_id,
            instruction.operands.first(),
        ) {
            (Op::ConvertUToPtr, Some(id), Some(Operand::IdRef(source)))
                if instruction.result_type.is_some_and(|ty| psb.contains(&ty)) =>
            {
                address_of.insert(id, *source);
                converted_ty.insert(id, instruction.result_type.unwrap_or(0));
            }
            (Op::AccessChain, Some(id), Some(Operand::IdRef(base)))
                if instruction.operands.len() == 2
                    && matches!(instruction.operands.get(1), Some(Operand::IdRef(index)) if zeros.contains(index))
                    && converted_ty.contains_key(base) =>
            {
                address_of.insert(id, address_of[base]);
                b78_wrapper_types.push(converted_ty[base]);
            }
            (Op::Load, Some(id), Some(Operand::IdRef(pointer))) => {
                loaded_from.insert(id, *pointer);
            }
            _ => {}
        }
    }
    let two_hops = module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Store
            && matches!(instruction.operands.get(1), Some(Operand::IdRef(value)) if sevens.contains(value))
            && matches!(instruction.operands.first(), Some(Operand::IdRef(pointer)) if address_of
                .get(pointer)
                .and_then(|word| loaded_from.get(word))
                .is_some_and(|table| address_of.contains_key(table)))
    });
    assert!(
        two_hops,
        "the 7 must be stored through the device pointer READ FROM the table (two OpConvertUToPtr hops):\n{asm}"
    );
    for ty in &b78_wrapper_types {
        let pointee = module
            .types_global_values
            .iter()
            .find(|instruction| instruction.result_id == Some(*ty))
            .and_then(|instruction| match instruction.operands.get(1) {
                Some(Operand::IdRef(pointee)) => Some(*pointee),
                _ => None,
            });
        let nonwritable = pointee.is_some_and(|pointee| {
            module.annotations.iter().any(|annotation| {
                annotation.class.opcode == Op::MemberDecorate
                    && matches!(annotation.operands.first(), Some(Operand::IdRef(id)) if *id == pointee)
                    && matches!(annotation.operands.get(2), Some(Operand::Decoration(Decoration::NonWritable)))
            })
        });
        assert!(
            nonwritable,
            "a member-0 hop the check followed must be the NonWritable table wrapper:\n{asm}"
        );
    }
    let null_copies = module
        .all_inst_iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::CopyObject
                && matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if nulls.contains(id))
        })
        .count();
    assert_eq!(null_copies, 0, "no load may fold to OpConstantNull:\n{asm}");
    assert!(
        spv == gep_spv,
        "the bitcast spelling must translate byte-identically to the GEP spelling ({} vs {} B)",
        spv.len(),
        gep_spv.len()
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_device_buffer_table_the_helper_also_writes_is_still_refused() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%ArrayRef = type { ptr, i64 }
%State = type { %ArrayRef, %ArrayRef }

define void @k(ptr %buffers) {
entry:
  %state = alloca %State, align 8
  %field = getelementptr inbounds %State, ptr %state, i64 0, i32 0, i32 0
  store ptr %buffers, ptr %field, align 8
  %secondary = insertvalue %State poison, ptr null, 1, 0
  call void @write(ptr %state)
  ret void
}

define internal void @write(ptr %state) {
entry:
  %opaque = bitcast ptr %state to ptr
  %table = load ptr, ptr %opaque, align 8
  %buffer = load ptr addrspace(1), ptr %table, align 8
  store i32 7, ptr addrspace(1) %buffer, align 4
  %slot1 = getelementptr inbounds ptr addrspace(1), ptr %table, i64 1
  store ptr addrspace(1) %buffer, ptr %slot1, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_device_buffer_table_written_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .err()
        .unwrap_or_else(|| {
            panic!(
                "a table the helper writes into is not a proven device address, so the store read through it must not translate"
            )
        });
    assert!(
        error.contains("addresses nothing"),
        "the refusal must name the placeholder that swallowed the store: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_device_buffer_array_reinterprets_aggregate_prefix() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%ArrayRef = type { ptr, i64 }
%Texture = type { ptr addrspace(1) }
%InOut = type { %ArrayRef, %ArrayRef, %ArrayRef, %Texture }
%State = type { %InOut, i32 }
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers) {
entry:
  %aggregate = insertvalue %InOut poison, ptr %buffers, 0, 0
  %table = extractvalue %InOut %aggregate, 0, 0
  %slot = getelementptr inbounds i64, ptr %table, i64 1
  %buffer = load ptr addrspace(1), ptr %slot, align 8
  %element = getelementptr inbounds i32, ptr addrspace(1) %buffer, i64 0
  store i32 7, ptr addrspace(1) %element, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_device_buffer_array_nested_callback_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer).then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(module.types_global_values.iter().all(|instruction| {
        instruction.class.opcode != Op::TypePointer
            || !matches!(
                instruction.operands.as_slice(),
                [
                    Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                    Operand::IdRef(pointee)
                ] if pointer_types.contains(pointee)
            )
    }), "device pointer-table slots must be addressed as 64-bit payloads, not as physical pointers to logical pointers:\n{}", disassemble(&spv).unwrap());
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_generic_callback_table_cursor_is_refused_not_silently_emptied() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers) {
entry:
  call void @callback(ptr %buffers)
  ret void
}

define internal void @callback(ptr %state) {
entry:
  %alias = bitcast ptr %state to ptr
  %table = load ptr, ptr %alias, align 8
  %slot = getelementptr inbounds ptr addrspace(1), ptr %table, i64 1
  %slot_alias = bitcast ptr %slot to ptr
  %buffer = load ptr addrspace(1), ptr %slot_alias, align 8
  store i32 7, ptr addrspace(1) %buffer, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let parsed = crate::native::ir::LlModule::parse(ll).expect("parse callback table cursor");
    assert!(
        crate::native::emit_tiers::requires_device_address_model(&parsed),
        "a generic callback cursor over device-pointer elements requires physical addresses"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_generic_callback_table_cursor_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .err()
        .unwrap_or_else(|| {
            panic!(
                "the store through the placeholder cannot be emitted, so this must not translate"
            )
        });
    assert!(
        error.contains("addresses nothing"),
        "the refusal must name the placeholder that swallowed the store: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
#[ignore = "known defect: the device-address store is refused, not kept; see the doc comment"]
fn native_generic_callback_table_cursor_keeps_its_store() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers) {
entry:
  call void @callback(ptr %buffers)
  ret void
}

define internal void @callback(ptr %state) {
entry:
  %alias = bitcast ptr %state to ptr
  %table = load ptr, ptr %alias, align 8
  %slot = getelementptr inbounds ptr addrspace(1), ptr %table, i64 1
  %slot_alias = bitcast ptr %slot to ptr
  %buffer = load ptr addrspace(1), ptr %slot_alias, align 8
  store i32 7, ptr addrspace(1) %buffer, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_callback_table_cursor_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .unwrap_or_else(|error| panic!("the translation is refused rather than emitted: {error}"));
    let asm = disassemble(&spv).expect("disassemble");
    let _ = std::fs::remove_dir_all(tmp);
    assert!(
        asm.contains("OpStore"),
        "the AIR stores 7 through the device pointer it loads from the table, so the module must \
         contain a store; it contains none:\n{asm}"
    );
}

#[test]
fn native_device_pointer_round_trips_through_direct_pointer_alloca() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@next_buffer = internal addrspace(2) global i32 1

define void @k(ptr %buffers) {
entry:
  %table = load ptr, ptr %buffers, align 8
  %slot = getelementptr inbounds ptr addrspace(1), ptr %table, i64 0
  %slot_alias = bitcast ptr %slot to ptr
  %buffer = load ptr addrspace(1), ptr %slot_alias, align 8
  %cursor = alloca ptr addrspace(1), align 8
  store ptr addrspace(1) %buffer, ptr %cursor, align 8
  %current = load ptr addrspace(1), ptr %cursor, align 8
  %next = getelementptr inbounds i32, ptr addrspace(1) %current, i64 1
  store ptr addrspace(1) %next, ptr %cursor, align 8
  %destination = load ptr addrspace(1), ptr %cursor, align 8
  store i32 7, ptr addrspace(1) %destination, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_direct_pointer_alloca_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_device_address_model_follows_argument_buffer_pointer_into_helper() {
    let ll = r#"
%Args = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args) {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0
  %buffer = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  call void @atomic_helper(ptr addrspace(1) %buffer)
  ret void
}

define internal void @atomic_helper(ptr addrspace(1) %buffer) {
entry:
  %element = getelementptr inbounds i32, ptr addrspace(1) %buffer, i64 1
  %old = call i32 @air.atomic.global.min.s.i32(ptr addrspace(1) %element, i32 7, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.min.s.i32(ptr addrspace(1), i32, i32, i32, i1)
"#;
    let parsed = crate::native::ir::LlModule::parse(ll).expect("parse argument-buffer pointer");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));
}

#[test]
fn native_device_address_model_follows_direct_argument_buffer_pointer_dereference() {
    let ll = r#"
%Args = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, i64 %index) {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0
  %buffer = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  %element = getelementptr inbounds [4 x i8], ptr addrspace(1) %buffer, i64 %index, i64 3
  %value = load i8, ptr addrspace(1) %element, align 1
  ret void
}
"#;
    let parsed = crate::native::ir::LlModule::parse(ll).expect("parse argument-buffer pointer");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));
}

#[test]
fn native_device_address_model_keeps_mixed_argument_buffer_handles_opaque() {
    let ll = r#"
%Args = type { ptr addrspace(1), ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, <2 x i32> %coord) {
entry:
  %texture_field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0
  %texture = load ptr addrspace(1), ptr addrspace(2) %texture_field, align 8
  call void @write(ptr addrspace(1) %texture, <2 x i32> %coord)
  %buffer_field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 1
  %buffer = load ptr addrspace(1), ptr addrspace(2) %buffer_field, align 8
  %element = getelementptr inbounds i32, ptr addrspace(1) %buffer, i64 1
  store i32 7, ptr addrspace(1) %element, align 4
  ret void
}

define internal void @write(ptr addrspace(1) %texture, <2 x i32> %coord) {
entry:
  call void @air.gather_depth_probe(ptr addrspace(1) %texture)
  ret void
}

declare void @air.gather_depth_probe(ptr addrspace(1))
"#;
    let parsed = crate::native::ir::LlModule::parse(ll).expect("parse mixed argument buffer");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));

    let opaque =
        crate::native::emitter::functions::opaque_resource_pointer_values_by_function(&parsed);
    let kernel = opaque.get("k").expect("kernel pointer classification");
    assert!(kernel.contains("%texture"), "{opaque:?}");
    assert!(!kernel.contains("%buffer"));
    assert!(!kernel.contains("%element"));
}

#[test]
fn native_device_address_model_constructs_dynamic_raw_helper_cursor_physically() {
    let ll = r#"
%Container = type { i32 }

define void @k(ptr addrspace(1) %buffer, i64 %index) {
entry:
  %element = getelementptr inbounds %Container, ptr addrspace(1) %buffer, i64 %index, i32 0
  call void @atomic_helper(ptr addrspace(1) %element)
  ret void
}

define internal void @atomic_helper(ptr addrspace(1) %element) {
entry:
  %old = call i32 @air.atomic.global.min.s.i32(ptr addrspace(1) %element, i32 7, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.min.s.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uchar"}
!4 = !{i32 1, !"air.thread_position_in_grid"}
"#;
    let metadata = crate::meta::parse_air_kernel_meta(ll).expect("parse metadata");
    let mut parsed =
        crate::native::ir::LlModule::parse_with_stage_meta(ll, Some(&metadata), Some("k"))
            .expect("parse dynamic raw helper cursor");
    parsed
        .raw_buffer_params
        .insert(("k".to_string(), "%buffer".to_string()));
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));
}

#[test]
fn native_device_address_model_constructs_integer_atomic_float_view_physically() {
    let ll = r#"
define void @k(ptr addrspace(1) %buffer, i64 %index) {
entry:
  %element = getelementptr inbounds <3 x float>, ptr addrspace(1) %buffer, i64 %index, i64 1
  %integer_view = bitcast ptr addrspace(1) %element to ptr addrspace(1)
  %old = call i32 @air.atomic.global.min.s.i32(ptr addrspace(1) %integer_view, i32 7, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.min.s.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float3"}
!4 = !{i32 1, !"air.thread_position_in_grid"}
"#;
    let metadata = crate::meta::parse_air_kernel_meta(ll).expect("parse metadata");
    let parsed = crate::native::ir::LlModule::parse_with_stage_meta(ll, Some(&metadata), Some("k"))
        .expect("parse integer atomic float view");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));
}

#[test]
fn native_device_address_model_follows_float_atomic_view_into_helper() {
    let ll = r#"
define internal void @atomic_bits(ptr addrspace(1) %slot) {
entry:
  %integer_view = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  %old = call i32 @air.atomic.global.load.i32(ptr addrspace(1) %integer_view, i32 0, i32 2, i1 true)
  ret void
}

define void @k(ptr addrspace(1) %floats) {
entry:
  call void @atomic_bits(ptr addrspace(1) %floats)
  ret void
}

declare i32 @air.atomic.global.load.i32(ptr addrspace(1), i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"floats"}
"#;
    let metadata = crate::meta::parse_air_kernel_meta(ll).expect("parse metadata");
    let parsed = crate::native::ir::LlModule::parse_with_stage_meta(ll, Some(&metadata), Some("k"))
        .expect("parse helper integer atomic float view");
    assert!(crate::native::emit_tiers::requires_device_address_model(
        &parsed
    ));
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_helper_atomic_float_address_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_StorageBuffer"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_bda_helper_constructs_function_constant_buffer_address_from_table() {
    let ll = r#"
@enabled = internal unnamed_addr addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1

define void @k(ptr addrspace(1) %optional, ptr addrspace(1) %floats) {
entry:
  call void @load_byte(ptr addrspace(1) %optional, ptr addrspace(1) %floats, i1 true)
  %float_slot = getelementptr inbounds float, ptr addrspace(1) %floats, i64 0
  %integer_view = bitcast ptr addrspace(1) %float_slot to ptr addrspace(1)
  %old = call i32 @air.atomic.global.min.s.i32(ptr addrspace(1) %integer_view, i32 7, i32 0, i32 2, i1 true)
  ret void
}

define internal void @load_byte(ptr addrspace(1) %buffer, ptr addrspace(1) %output, i1 %read) {
entry:
  br i1 %read, label %load, label %done
load:
  %field = getelementptr inbounds i8, ptr addrspace(1) %buffer, i64 1
  %value = load i8, ptr addrspace(1) %field, align 1
  store i8 %value, ptr addrspace(1) %output, align 1
  br label %done
done:
  ret void
}

declare i32 @air.atomic.global.min.s.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.function_constant", !5, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uchar"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float"}
!5 = !{ptr addrspace(2) @enabled, !"bool", !"enabled"}
"#;
    let spv =
        crate::translate_native_no_retry(ll, Stage::Kernel).expect("construct primary BDA module");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("Binding 640"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_bda_fc_buffer_address_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        tools::spirv_val_bytes(&spv, &tmp).expect("primary spirv-val");
        let _ = std::fs::remove_dir_all(tmp);
    }
}

#[test]
fn native_bda_pointer_phi_merges_buffer_root_with_child_device_address() {
    let ll = r#"
define internal void @walk(ptr addrspace(1) %root, ptr addrspace(1) %out) {
entry:
  br label %walk

walk:
  %cursor = phi ptr addrspace(1) [ %root, %entry ], [ %child, %body ]
  %done = icmp eq ptr addrspace(1) %cursor, null
  br i1 %done, label %exit, label %body

body:
  %child_addr = load i64, ptr addrspace(1) %cursor, align 8
  %child = inttoptr i64 %child_addr to ptr addrspace(1)
  br label %walk

exit:
  store i32 1, ptr addrspace(1) %out, align 4
  ret void
}

define void @k(ptr addrspace(1) %root, ptr addrspace(1) %out) {
entry:
  call void @walk(ptr addrspace(1) %root, ptr addrspace(1) %out)
  %opaque = insertvalue { ptr addrspace(1) } poison, ptr addrspace(1) null, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"root"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_address_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.matches("OpPhi").count() >= 2, "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    assert!(
        !asm.contains("OpConstantNull %_ptr_UniformConstant_uchar"),
        "a provably-null opaque aggregate field must use integer zero in BDA mode:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_bda_pointer_phi_materializes_cross_root_select_address() {
    let ll = r#"
define internal float @read_selected(ptr addrspace(1) %left, ptr addrspace(1) %right, ptr addrspace(1) %table, i1 %choose_left, i1 %direct) {
entry:
  br i1 %direct, label %selected_block, label %loaded_block

selected_block:
  %selected = select i1 %choose_left, ptr addrspace(1) %left, ptr addrspace(1) %right
  br label %merge

loaded_block:
  %address = load i64, ptr addrspace(1) %table, align 8
  %loaded = inttoptr i64 %address to ptr addrspace(1)
  br label %merge

merge:
  %pointer = phi ptr addrspace(1) [ %selected, %selected_block ], [ %loaded, %loaded_block ]
  %value = load float, ptr addrspace(1) %pointer, align 4
  ret float %value
}

define void @k(ptr addrspace(1) %left, ptr addrspace(1) %right, ptr addrspace(1) %table, ptr addrspace(1) %out, ptr addrspace(1) %choose_left_input, ptr addrspace(1) %direct_input) {
entry:
  %choose_left_word = load i32, ptr addrspace(1) %choose_left_input, align 4
  %choose_left = icmp ne i32 %choose_left_word, 0
  %direct_word = load i32, ptr addrspace(1) %direct_input, align 4
  %direct = icmp ne i32 %direct_word, 0
  %value = call float @read_selected(ptr addrspace(1) %left, ptr addrspace(1) %right, ptr addrspace(1) %table, i1 %choose_left, i1 %direct)
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7, !8}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"left"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"right"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"table"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!7 = !{i32 4, !"air.buffer", !"air.location_index", i32 4, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"choose_left_input"}
!8 = !{i32 5, !"air.buffer", !"air.location_index", i32 5, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"direct_input"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_cross_root_select_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_bda_pointer_phi_materializes_forward_gep_addresses_on_edges() {
    let ll = r#"
define float @k(ptr addrspace(1) %root) {
entry:
  %loaded_pointer = load ptr addrspace(1), ptr addrspace(1) %root, align 8
  %cond = icmp eq ptr addrspace(1) %loaded_pointer, null
  br i1 %cond, label %left, label %right

left:
  %left_alias = bitcast ptr addrspace(1) %loaded_pointer to ptr addrspace(1)
  %left_pointer = getelementptr inbounds float, ptr addrspace(1) %left_alias, i64 1
  br label %merge

right:
  %right_alias = bitcast ptr addrspace(1) %loaded_pointer to ptr addrspace(1)
  %right_pointer = getelementptr inbounds float, ptr addrspace(1) %right_alias, i64 2
  br label %merge

merge:
  %pointer = phi ptr addrspace(1) [ %left_pointer, %left ], [ %right_pointer, %right ]
  %value = load float, ptr addrspace(1) %pointer, align 4
  ret float %value
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"root"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_forward_gep_address_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_bda_physical_and_direct_pointer_select_replays_loaded_values() {
    let ll = r#"
define internal <4 x float> @read_selected(ptr addrspace(1) %table, ptr addrspace(1) %zero, i1 %take_source) {
entry:
  %address = load i64, ptr addrspace(1) %table, align 8
  %physical = inttoptr i64 %address to ptr addrspace(1)
  %selected = select i1 %take_source, ptr addrspace(1) %physical, ptr addrspace(1) %zero
  %value = load <4 x float>, ptr addrspace(1) %selected, align 16
  ret <4 x float> %value
}

define void @k(ptr addrspace(1) %table, ptr addrspace(1) %zero, ptr addrspace(1) %out, <3 x i32> %gid) {
entry:
  %x = extractelement <3 x i32> %gid, i64 0
  %take_source = icmp eq i32 %x, 0
  %value = call <4 x float> @read_selected(ptr addrspace(1) %table, ptr addrspace(1) %zero, i1 %take_source)
  store <4 x float> %value, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"table"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"zero"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_physical_direct_select_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpSelect") && line.contains("_ptr_")),
        "mixed-storage selection must be replayed in the value domain:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_loop_pointer_select_replays_loaded_values() {
    let ll = r#"
@optional_raw = internal addrspace(2) global i8 0, align 1

define internal <4 x i16> @read_selected(ptr addrspace(1) %source, ptr addrspace(1) %raw, i1 %choose, i1 %again) {
entry:
  %raw_byte = load i8, ptr addrspace(1) %raw, align 1
  br label %loop

loop:
  %typed_pointer = phi ptr addrspace(1) [ %source, %entry ], [ %next, %body ]
  %opaque_alias = bitcast ptr addrspace(1) %typed_pointer to ptr addrspace(1)
  %selected = select i1 %choose, ptr addrspace(1) %opaque_alias, ptr addrspace(1) %raw
  %value = load <4 x i16>, ptr addrspace(1) %selected, align 8
  br i1 %again, label %body, label %exit

body:
  %next = getelementptr inbounds i16, ptr addrspace(1) %typed_pointer, i64 4
  br label %loop

exit:
  ret <4 x i16> %value
}

define void @k(ptr addrspace(1) %source, ptr addrspace(1) %raw, ptr addrspace(1) %out, i1 %choose, i1 %again) {
entry:
  %value = call <4 x i16> @read_selected(ptr addrspace(1) %source, ptr addrspace(1) %raw, i1 %choose, i1 %again)
  store <4 x i16> %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"source"}
!4 = !{i32 1, !"air.function_constant", !8, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"raw"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.function_constant", !"air.arg_type_name", !"bool", !"air.arg_name", !"choose"}
!7 = !{i32 4, !"air.function_constant", !"air.arg_type_name", !"bool", !"air.arg_name", !"again"}
!8 = !{ptr addrspace(2) @optional_raw, !"bool", !"has_raw"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_incompatible_opaque_select_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpSelect") && line.contains("_ptr_")),
        "incompatible opaque-pointer selection must be replayed in the value domain:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_construct_tree_preserves_struct_members_in_bda_pointer_state() {
    let ll = r#"
%Pair = type { float, float }

define void @k(ptr addrspace(1) %root, ptr addrspace(1) %out, i1 %direct, i1 %again) {
entry:
  %local = alloca %Pair, align 4
  %field = getelementptr inbounds %Pair, ptr %local, i64 0, i32 1
  br i1 %direct, label %sibling, label %walk

walk:
  %cursor = phi ptr addrspace(1) [ %root, %entry ], [ %child, %body ]
  br label %body

body:
  %child_addr = load i64, ptr addrspace(1) %cursor, align 8
  %child = inttoptr i64 %child_addr to ptr addrspace(1)
  br i1 %again, label %walk, label %sibling

sibling:
  store float 2.0, ptr %field, align 4
  store i32 1, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"root"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_construct_tree_struct_member_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp)
        .expect("construct-tree BDA translation");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.matches("OpInBoundsAccessChain").count() >= 2, "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    let module = load_bytes(&spv).expect("load constructed BDA module");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::TypePointer)
                .then_some(inst.result_id)
                .flatten()
        })
        .collect::<HashSet<_>>();
    assert!(module.all_inst_iter().all(|inst| {
        inst.class.opcode != Op::Phi
            || !inst
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
    }));
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_bda_inlined_helper_atomic_uses_a_physical_word_pointer() {
    let ll = r#"
define internal void @increment_leaf(ptr addrspace(1) %counters, i32 %index) {
entry:
  %slot = getelementptr inbounds i32, ptr addrspace(1) %counters, i32 %index
  %old = call i32 @air.atomic.global.add.s.i32(ptr addrspace(1) %slot, i32 1, i32 0, i32 2, i1 true)
  ret void
}

define internal void @increment(ptr addrspace(1) %counters, i32 %index) {
entry:
  br label %body

body:
  call void @increment_leaf(ptr addrspace(1) %counters, i32 %index)
  ret void
}

define void @k(ptr addrspace(1) %counters) {
entry:
  call void @increment(ptr addrspace(1) %counters, i32 3)
  ret void
}

declare i32 @air.atomic.global.add.s.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"counters"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bda_helper_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_bda_probe(ll, Stage::Kernel, &tmp).expect("BDA translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicIAdd"), "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_mtl_force_not_checked_i64_load_uses_bda_inttoptr_address() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %src) {
entry:
  %addr = load i64, ptr addrspace(1) %src, align 8
  %p = inttoptr i64 %addr to ptr addrspace(1)
  %field = getelementptr inbounds i8, ptr addrspace(1) %p, i64 8
  %v = tail call i64 @mtl.force_not_checked.load.i64.p1(ptr addrspace(1) %field)
  store i64 %v, ptr addrspace(1) %out, align 8
  ret void
}

declare extern_weak i64 @mtl.force_not_checked.load.i64.p1(ptr addrspace(1)) section "air.externally_defined"

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"src"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_mtl_force_not_checked_bda_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_direct_buffer_pointer_store_loads_runtime_address_sidecar() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %source) {
entry:
  store ptr addrspace(1) %source, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"source"}
"#;
    let kern = meta::parse_air_kernel_meta(ll);
    let entry_name = meta::entry_name(ll, "kernel");
    let emitted = crate::native::emit_vulkan_spirv_all_buffers_raw_with_sidecar(
        ll,
        kern.as_ref(),
        entry_name.as_deref(),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
        &HashSet::new(),
        &HashSet::new(),
    )
    .expect("emit raw-buffer tier with typed sidecar");
    let mut address_words = emitted
        .sidecar
        .buffer_address_words
        .iter()
        .map(|fact| (fact.param_index, fact.component))
        .collect::<Vec<_>>();
    address_words.sort_unstable();
    assert_eq!(address_words, vec![(1, 0), (1, 1)]);
    assert!(emitted
        .sidecar
        .buffer_address_words
        .iter()
        .all(|fact| fact.id != 0));

    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_direct_buffer_address_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 640"), "{asm}");
    assert!(asm.contains("ArrayStride 8"), "{asm}");
    assert!(asm.contains("OpAccessChain"), "{asm}");
    assert!(!asm.contains("metal2vulkan.buffer_address_word"), "{asm}");
    assert!(!asm.contains("OpConvertPtrToU"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_bda_as_data_pointer_intrinsic_is_device_address_passthrough() {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %in) {
entry:
  %p = load ptr addrspace(1), ptr addrspace(1) %in, align 8
  %d = call ptr addrspace(1) @air.get_data_pointer_instance_acceleration_structure(ptr addrspace(1) %p)
  store ptr addrspace(1) %d, ptr addrspace(1) %out, align 8
  %g = getelementptr inbounds i8, ptr addrspace(1) %d, i64 136
  %f = load float, ptr addrspace(1) %g, align 4
  store float %f, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(1) @air.get_data_pointer_instance_acceleration_structure(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"in"}
"#;
    let spv = crate::native::emit_vulkan_spirv_all_buffers_raw_bda(ll).expect("bda emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("PhysicalStorageBuffer64"),
        "AS data-pointer passthrough must reach the device-address path:\n{asm}"
    );
    assert!(
        asm.contains("OpConvertUToPtr"),
        "expected a device-address deref:\n{asm}"
    );
}

#[test]
fn native_bda_loads_device_address_from_local_aggregate() {
    let ll = r#"
%AddressBox = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %source) {
entry:
  %incoming = load ptr addrspace(1), ptr addrspace(1) %source, align 8
  %local = alloca %AddressBox, align 8
  %field = getelementptr inbounds %AddressBox, ptr %local, i32 0, i32 0
  store ptr addrspace(1) %incoming, ptr %field, align 8
  %address = load ptr addrspace(1), ptr %field, align 8
  %value_ptr = getelementptr inbounds i32, ptr addrspace(1) %address, i64 0
  %value = load i32, ptr addrspace(1) %value_ptr, align 4
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"source"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_bda_local_aggregate_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_acceleration_structure_shadow_lowers_count_child_and_payload_store() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %as, ptr addrspace(1) %out, i32 %idx) {
entry:
  %count = call i32 @air.get_instance_count_instance_acceleration_structure(ptr addrspace(1) %as)
  store i32 %count, ptr addrspace(1) %out, align 4
  %child = call ptr addrspace(1) @air.get_primitive_acceleration_structure_instance_acceleration_structure(ptr addrspace(1) %as, i32 %idx)
  %child_slot = getelementptr inbounds i8, ptr addrspace(1) %out, i64 8
  store ptr addrspace(1) %child, ptr addrspace(1) %child_slot, align 8
  %bits = ptrtoint ptr addrspace(1) %child to i64
  %bits_slot = getelementptr inbounds i8, ptr addrspace(1) %out, i64 16
  store i64 %bits, ptr addrspace(1) %bits_slot, align 8
  ret void
}

declare i32 @air.get_instance_count_instance_acceleration_structure(ptr addrspace(1))
declare ptr addrspace(1) @air.get_primitive_acceleration_structure_instance_acceleration_structure(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.instance_acceleration_structure", !"air.location_index", i32 8, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_as_shadow_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 8"), "{asm}");
    assert!(asm.contains("OpTypeInt 64"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_unused_primitive_acceleration_structure_needs_no_vulkan_binding() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %as, ptr addrspace(1) %out) {
entry:
  store i32 7, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.primitive_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<>", !"air.arg_name", !"as"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_primitive_as_shadow_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("Binding 5"), "{asm}");
    assert!(asm.contains("Binding 0"), "{asm}");
}

#[test]
fn native_callback_free_single_instance_triangle_query_uses_shadow_binding() {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %as, ptr addrspace(1) %table) {
entry:
  %hit = call { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, <3 x float> <float 0.000000e+00, float 0.000000e+00, float -1.000000e+00>, float 0.000000e+00, float 1.000000e+01, ptr addrspace(1) %as, i32 255, ptr addrspace(1) %table, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  %instance = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %hit, 6
  store i32 %instance, ptr addrspace(1) %out, align 4
  ret void
}

declare { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.instance_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!5 = !{i32 2, !"air.intersection_function_table", !"air.location_index", i32 6}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_single_instance_intersection_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 5"), "{asm}");
    assert!(!asm.contains("Binding 6"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_callback_free_multi_level_query_requires_hierarchy_path_metadata() {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %as, ptr addrspace(1) %table) {
entry:
  %instance_ids = alloca i32, align 4
  %user_instance_ids = alloca i32, align 4
  store i32 -1, ptr %instance_ids, align 4
  store i32 -1, ptr %user_instance_ids, align 4
  %hit = call { i32, float, i32, i32, ptr addrspace(1), i8 } @air.intersect.multi_level_instancing(<3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, <3 x float> <float 0.000000e+00, float 0.000000e+00, float -1.000000e+00>, float 0.000000e+00, float 1.000000e+01, ptr addrspace(1) %as, i32 255, ptr addrspace(1) %table, ptr null, i64 0, i8 2, ptr %instance_ids, ptr %user_instance_ids, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  %opaque = extractvalue { i32, float, i32, i32, ptr addrspace(1), i8 } %hit, 4
  %opaque_is_null = icmp eq ptr addrspace(1) %opaque, null
  %opaque_is_null32 = zext i1 %opaque_is_null to i32
  %path_length8 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i8 } %hit, 5
  %path_length = zext i8 %path_length8 to i32
  %instance_id = load i32, ptr %instance_ids, align 4
  %user_instance_id = load i32, ptr %user_instance_ids, align 4
  store i32 %path_length, ptr addrspace(1) %out, align 4
  %out_instance = getelementptr i32, ptr addrspace(1) %out, i64 1
  store i32 %instance_id, ptr addrspace(1) %out_instance, align 4
  %out_user = getelementptr i32, ptr addrspace(1) %out, i64 2
  store i32 %user_instance_id, ptr addrspace(1) %out_user, align 4
  %out_opaque = getelementptr i32, ptr addrspace(1) %out, i64 3
  store i32 %opaque_is_null32, ptr addrspace(1) %out_opaque, align 4
  ret void
}

declare { i32, float, i32, i32, ptr addrspace(1), i8 } @air.intersect.multi_level_instancing(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i8, ptr, ptr, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint4", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.instance_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!5 = !{i32 2, !"air.intersection_function_table", !"air.location_index", i32 6}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_multi_level_intersection_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect_err("multi-level paths require a runtime hierarchy ABI");
    assert!(
        error.contains("multi-level instancing requires hierarchy path metadata"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

fn native_world_space_intersection_fixture(triangle: bool) -> String {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %as, ptr addrspace(1) %table) {
entry:
  %hit = call { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } @air.intersect.instancing.world_space_data(<3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, <3 x float> <float 0.000000e+00, float 0.000000e+00, float -1.000000e+00>, float 0.000000e+00, float 1.000000e+01, ptr addrspace(1) %as, i32 255, ptr addrspace(1) %table, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  %column0 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 7
  %v0 = extractelement <3 x float> %column0, i32 0
  %p0 = getelementptr float, ptr addrspace(1) %out, i64 0
  store float %v0, ptr addrspace(1) %p0, align 4
  %v1 = extractelement <3 x float> %column0, i32 1
  %p1 = getelementptr float, ptr addrspace(1) %out, i64 1
  store float %v1, ptr addrspace(1) %p1, align 4
  %v2 = extractelement <3 x float> %column0, i32 2
  %p2 = getelementptr float, ptr addrspace(1) %out, i64 2
  store float %v2, ptr addrspace(1) %p2, align 4
  %column1 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 8
  %v3 = extractelement <3 x float> %column1, i32 0
  %p3 = getelementptr float, ptr addrspace(1) %out, i64 3
  store float %v3, ptr addrspace(1) %p3, align 4
  %v4 = extractelement <3 x float> %column1, i32 1
  %p4 = getelementptr float, ptr addrspace(1) %out, i64 4
  store float %v4, ptr addrspace(1) %p4, align 4
  %v5 = extractelement <3 x float> %column1, i32 2
  %p5 = getelementptr float, ptr addrspace(1) %out, i64 5
  store float %v5, ptr addrspace(1) %p5, align 4
  %column2 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 9
  %v6 = extractelement <3 x float> %column2, i32 0
  %p6 = getelementptr float, ptr addrspace(1) %out, i64 6
  store float %v6, ptr addrspace(1) %p6, align 4
  %v7 = extractelement <3 x float> %column2, i32 1
  %p7 = getelementptr float, ptr addrspace(1) %out, i64 7
  store float %v7, ptr addrspace(1) %p7, align 4
  %v8 = extractelement <3 x float> %column2, i32 2
  %p8 = getelementptr float, ptr addrspace(1) %out, i64 8
  store float %v8, ptr addrspace(1) %p8, align 4
  %column3 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 10
  %v9 = extractelement <3 x float> %column3, i32 0
  %p9 = getelementptr float, ptr addrspace(1) %out, i64 9
  store float %v9, ptr addrspace(1) %p9, align 4
  %v10 = extractelement <3 x float> %column3, i32 1
  %p10 = getelementptr float, ptr addrspace(1) %out, i64 10
  store float %v10, ptr addrspace(1) %p10, align 4
  %v11 = extractelement <3 x float> %column3, i32 2
  %p11 = getelementptr float, ptr addrspace(1) %out, i64 11
  store float %v11, ptr addrspace(1) %p11, align 4
  %column4 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 11
  %v12 = extractelement <3 x float> %column4, i32 0
  %p12 = getelementptr float, ptr addrspace(1) %out, i64 12
  store float %v12, ptr addrspace(1) %p12, align 4
  %v13 = extractelement <3 x float> %column4, i32 1
  %p13 = getelementptr float, ptr addrspace(1) %out, i64 13
  store float %v13, ptr addrspace(1) %p13, align 4
  %v14 = extractelement <3 x float> %column4, i32 2
  %p14 = getelementptr float, ptr addrspace(1) %out, i64 14
  store float %v14, ptr addrspace(1) %p14, align 4
  %column5 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 12
  %v15 = extractelement <3 x float> %column5, i32 0
  %p15 = getelementptr float, ptr addrspace(1) %out, i64 15
  store float %v15, ptr addrspace(1) %p15, align 4
  %v16 = extractelement <3 x float> %column5, i32 1
  %p16 = getelementptr float, ptr addrspace(1) %out, i64 16
  store float %v16, ptr addrspace(1) %p16, align 4
  %v17 = extractelement <3 x float> %column5, i32 2
  %p17 = getelementptr float, ptr addrspace(1) %out, i64 17
  store float %v17, ptr addrspace(1) %p17, align 4
  %column6 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 13
  %v18 = extractelement <3 x float> %column6, i32 0
  %p18 = getelementptr float, ptr addrspace(1) %out, i64 18
  store float %v18, ptr addrspace(1) %p18, align 4
  %v19 = extractelement <3 x float> %column6, i32 1
  %p19 = getelementptr float, ptr addrspace(1) %out, i64 19
  store float %v19, ptr addrspace(1) %p19, align 4
  %v20 = extractelement <3 x float> %column6, i32 2
  %p20 = getelementptr float, ptr addrspace(1) %out, i64 20
  store float %v20, ptr addrspace(1) %p20, align 4
  %column7 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } %hit, 14
  %v21 = extractelement <3 x float> %column7, i32 0
  %p21 = getelementptr float, ptr addrspace(1) %out, i64 21
  store float %v21, ptr addrspace(1) %p21, align 4
  %v22 = extractelement <3 x float> %column7, i32 1
  %p22 = getelementptr float, ptr addrspace(1) %out, i64 22
  store float %v22, ptr addrspace(1) %p22, align 4
  %v23 = extractelement <3 x float> %column7, i32 2
  %p23 = getelementptr float, ptr addrspace(1) %out, i64 23
  store float %v23, ptr addrspace(1) %p23, align 4
  ret void
}

declare { i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> } @air.intersect.instancing.world_space_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.instance_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!5 = !{i32 2, !"air.intersection_function_table", !"air.location_index", i32 6}
"#;
    if !triangle {
        return ll.to_string();
    }
    let mut ll = ll.replace("{ i32, float, i32, i32, ptr addrspace(1), i32, i32, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> }", "{ i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> }")
        .replace("air.intersect.instancing.world_space_data", "air.intersect.instancing.triangle_data.world_space_data");
    for column in 0..8 {
        ll = ll.replace(&format!("%column{column} = extractvalue {{ i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> }} %hit, {}", column + 7),
            &format!("%column{column} = extractvalue {{ i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> }} %hit, {}", column + 9));
    }
    ll
}

#[test]
fn native_callback_free_instance_world_space_data_uses_query_transforms() {
    for triangle in [false, true] {
        let ll = native_world_space_intersection_fixture(triangle);
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_world_space_{}_{}",
            std::process::id(),
            triangle
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp).expect("translate");
        let asm = disassemble(&spv).expect("disassemble");
        assert!(asm.contains("Binding 5"), "{asm}");
        assert!(!asm.contains("Binding 6"), "{asm}");
        assert!(
            asm.contains("OpRayQueryGetIntersectionWorldToObjectKHR"),
            "{asm}"
        );
        assert!(
            asm.contains("OpRayQueryGetIntersectionObjectToWorldKHR"),
            "{asm}"
        );
        assert!(asm.contains("OpTypeMatrix"), "{asm}");
        if std::process::Command::new("spirv-val")
            .arg("--version")
            .output()
            .is_ok()
        {
            tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[test]
fn native_world_space_intersection_rejects_wrong_field_type() {
    let ll = native_world_space_intersection_fixture(false)
        .replace("{ i32, float, i32, i32", "{ i32, i32, i32, i32");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_world_space_bad_type_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp)
        .expect_err("wrong distance type");
    assert!(
        error.contains("intersection result field 1 must be float"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_ignores_llvm_metadata_root_globals() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@llvm.used = appending global [1 x ptr] [ptr @helper], section "llvm.metadata"
@llvm.compiler.used = appending global [1 x ptr] [ptr @helper], section "llvm.metadata"

define void @main() {
entry:
  ret void
}

define internal void @helper() {
entry:
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(!asm.contains("llvm.used"), "{asm}");
    assert!(!asm.contains("llvm.compiler.used"), "{asm}");
}

#[test]
fn sanitized_ll_drops_llvm_metadata_root_globals() {
    let ll = r#"
target triple = "air64-apple-macosx"
@llvm.used = appending global [1 x ptr] [ptr @helper], section "llvm.metadata"
@llvm.compiler.used = appending global [1 x ptr] [ptr @helper], section "llvm.metadata"

define void @main() {
entry:
  ret void
}

define internal void @helper() {
entry:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_sanitize_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let src = tmp.join("metadata.ll");
    std::fs::write(&src, ll).expect("write fixture ll");
    let sanitized = tools::air_to_sanitized_ll(src.to_str().unwrap(), &tmp).expect("sanitize .ll");
    assert!(sanitized.contains(tools::VULKAN_TRIPLE), "{sanitized}");
    assert!(!sanitized.contains("@llvm.used"), "{sanitized}");
    assert!(!sanitized.contains("@llvm.compiler.used"), "{sanitized}");
}

#[test]
fn native_memcpy_to_typed_alloca_first_field_uses_copy_memory() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Light = type { <4 x float>, <4 x float>, <3 x float> }
%struct.Params = type { %struct.Light, i32 }

define void @main() {
entry:
  %src = alloca %struct.Light, align 16
  %dst = alloca %struct.Params, align 16
  %src_raw = bitcast ptr %src to ptr
  %dst_raw = bitcast ptr %dst to ptr
  call void @llvm.memcpy.p0.p0.i64(ptr %dst_raw, ptr %src_raw, i64 48, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_typed_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_memcpy_between_named_prefix_wrappers_lowers_both_directions() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Frame = type { %union.FrameUnion }
%union.FrameUnion = type { %struct.FrameInner }
%struct.FrameInner = type { <3 x float>, <3 x float>, <3 x float> }
%struct.Geometry = type { %struct.Frame, <3 x float> }

define void @main() {
entry:
  %frame = alloca %struct.Frame, align 16
  %geometry = alloca %struct.Geometry, align 16
  %frame_raw = bitcast ptr %frame to ptr
  %geometry_raw = bitcast ptr %geometry to ptr
  call void @llvm.memcpy.p0.p0.i64(ptr %geometry_raw, ptr %frame_raw, i64 48, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr %frame_raw, ptr %geometry_raw, i64 48, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_named_prefix_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpCopyMemory").count(), 2, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_memcpy_from_named_wrapper_to_bare_array_lowers_by_elements() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Matrix = type { [3 x <3 x float>] }

define void @main() {
entry:
  %wrapped = alloca %struct.Matrix, align 16
  %bare = alloca [3 x <3 x float>], align 16
  %wrapped_raw = bitcast ptr %wrapped to ptr
  %bare_raw = bitcast ptr %bare to ptr
  call void @llvm.memcpy.p0.p0.i64(ptr %bare_raw, ptr %wrapped_raw, i64 48, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_wrapper_to_array_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpCopyMemory").count(), 1, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_memcpy_source_first_field_targets_destination_array_first_element() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Matrix = type { [4 x <4 x float>] }
%struct.Input = type { %struct.Matrix, %struct.Matrix }

@source = internal addrspace(2) constant %struct.Input zeroinitializer, align 16
@element_source = internal addrspace(2) constant %struct.Matrix zeroinitializer, align 16

define void @main() {
entry:
  %matrices = alloca [2 x %struct.Matrix], align 16
  %dst = bitcast ptr %matrices to ptr
  call void @llvm.memcpy.p0.p2.i64(ptr align 16 dereferenceable(64) %dst, ptr addrspace(2) align 16 dereferenceable(64) @source, i64 64, i1 false)
  %direct_matrices = alloca [2 x %struct.Matrix], align 16
  %direct_dst = bitcast ptr %direct_matrices to ptr
  call void @llvm.memcpy.p0.p2.i64(ptr align 16 dereferenceable(64) %direct_dst, ptr addrspace(2) align 16 dereferenceable(64) @element_source, i64 64, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_array_element_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_memcpy_from_opaque_byval_array_preserves_explicit_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Wrapper = type { [3 x i64] }

define void @main(ptr readonly byval([3 x i64]) %src) {
entry:
  %dst = alloca %struct.Wrapper, align 8
  %field = getelementptr inbounds %struct.Wrapper, ptr %dst, i64 0, i32 0
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 dereferenceable(24) %field, ptr align 8 dereferenceable(24) %src, i64 24, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_byval_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpCopyMemory").count(), 1, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_memcpy_struct_prefix_skips_trailing_padding_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Aggregate = type { <4 x float>, i32, float, [4 x i8] }

define void @main() {
entry:
  %src = alloca %struct.Aggregate, align 16
  %dst = alloca %struct.Aggregate, align 16
  %src_raw = bitcast ptr %src to ptr
  %dst_raw = bitcast ptr %dst to ptr
  call void @llvm.memcpy.p0.p0.i64(ptr %dst_raw, ptr %src_raw, i64 24, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_prefix_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpCopyMemory").count(), 3, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_buffer_memcpy_preserves_byte_offsets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %src_head = load i32, ptr addrspace(1) %src, align 4
  %src_off = getelementptr inbounds i8, ptr addrspace(1) %src, i64 136
  %dst_off = getelementptr inbounds i8, ptr addrspace(1) %dst, i64 32
  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) noundef align 4 dereferenceable(24) %dst_off, ptr addrspace(1) noundef align 8 dereferenceable(24) %src_off, i64 24, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let constants = module
        .types_global_values
        .iter()
        .filter_map(|inst| match (inst.class.opcode, inst.operands.last()) {
            (Op::Constant, Some(Operand::LiteralBit32(value))) => Some(*value),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for word_offset in [32, 52, 34, 39] {
        assert!(
            constants.contains(&word_offset),
            "missing word offset {word_offset} in {constants:?}\n{asm}"
        );
    }
}

#[test]
fn native_raw_buffer_zero_memset_clears_entire_byte_range() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Counter = type { i32, [12 x i8] }

define void @main(i32 %tid, ptr addrspace(1) %counters) {
entry:
  %idx = zext i32 %tid to i64
  %tail = getelementptr inbounds %Counter, ptr addrspace(1) %counters, i64 %idx, i32 1, i64 0
  tail call void @llvm.memset.p1.i64(ptr addrspace(1) noundef align 4 dereferenceable(12) %tail, i8 0, i64 12, i1 false)
  ret void
}

declare void @llvm.memset.p1.i64(ptr addrspace(1), i8, i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Counter", !"air.arg_name", !"counters"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"value"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_zero_memset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert_eq!(asm.matches("OpStore").count(), 12, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_buffer_memcpy_preserves_partial_byte_range() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Particle = type { [3 x float], half, half, [3 x float], half, i16, float, half, half }

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %dst, i32 %tid) {
entry:
  %idx = zext i32 %tid to i64
  %src_vel = getelementptr inbounds %Particle, ptr addrspace(1) %src, i64 %idx, i32 3
  %src_raw = bitcast ptr addrspace(1) %src_vel to ptr addrspace(1)
  %dst_vel = getelementptr inbounds %Particle, ptr addrspace(1) %dst, i64 %idx, i32 3
  %dst_raw = bitcast ptr addrspace(1) %dst_vel to ptr addrspace(1)
  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) noundef align 4 dereferenceable(14) %dst_raw, ptr addrspace(1) noundef align 4 dereferenceable(14) %src_raw, i64 14, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 40, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Particle", !"air.arg_name", !"src"}
!4 = !{i32 0, i32 12, i32 0, !"packed_float3", !"position", i32 12, i32 2, i32 0, !"half", !"size", i32 14, i32 2, i32 0, !"half", !"angle", i32 16, i32 12, i32 0, !"packed_float3", !"velocity", i32 28, i32 2, i32 0, !"half", !"angularVelocity", i32 30, i32 2, i32 0, !"short", !"colorIndex", i32 32, i32 4, i32 0, !"float", !"depth", i32 36, i32 2, i32 0, !"half", !"wigglePhase", i32 38, i32 2, i32 0, !"half", !"wiggleFrequency"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 40, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Particle", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_partial_raw_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(asm.matches("OpAtomicAnd").count() >= 14, "{asm}");
    assert!(asm.matches("OpAtomicOr").count() >= 14, "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let constants = module
        .types_global_values
        .iter()
        .filter_map(|inst| match (inst.class.opcode, inst.operands.last()) {
            (Op::Constant, Some(Operand::LiteralBit32(value))) => Some(*value),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for byte_offset in [16, 17, 28, 29] {
        assert!(
            constants.contains(&byte_offset),
            "missing byte offset {byte_offset} in {constants:?}\n{asm}"
        );
    }
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_typed_constant_memcpy_to_dynamic_raw_buffer_preserves_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type { [4 x float] }

define void @main(ptr addrspace(1) %bytes, ptr addrspace(2) %params) {
entry:
  %base = load i32, ptr addrspace(1) %bytes, align 4
  %base64 = zext i32 %base to i64
  %dyn = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %base64
  %dst = getelementptr inbounds i8, ptr addrspace(1) %dyn, i64 32
  %src = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 0
  %src_raw = bitcast ptr addrspace(2) %src to ptr addrspace(2)
  tail call void @llvm.memcpy.p1.p2.i64(ptr addrspace(1) align 16 dereferenceable(16) %dst, ptr addrspace(2) align 16 dereferenceable(16) %src_raw, i64 16, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p2.i64(ptr addrspace(1), ptr addrspace(2), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"bytes"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!5 = !{!"air.struct_type_info", !6, i32 0, i32 4, i32 4, !"float", !"values"}
!6 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_typed_to_raw_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert_eq!(asm.matches("OpStore").count(), 4, "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_metadata_struct_memcpy_to_private_alloca_uses_copy_memory() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Eyes = type { [4 x <2 x float>], [4 x <2 x float>], i32 }

define void @main(ptr addrspace(2) %eyes) {
entry:
  %dst = alloca %Eyes, align 8
  %dst_raw = bitcast ptr %dst to ptr
  %src_raw = bitcast ptr addrspace(2) %eyes to ptr addrspace(2)
  tail call void @llvm.memcpy.p0.p2.i64(ptr align 8 dereferenceable(72) %dst_raw, ptr addrspace(2) align 8 dereferenceable(72) %src_raw, i64 72, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 72, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 72, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"simple_lens_model_eyes", !"air.arg_name", !"eyes"}
!4 = !{i32 0, i32 8, i32 4, !"float2", !"leftEyes", i32 32, i32 8, i32 4, !"float2", !"rightEyes", i32 64, i32 4, i32 0, !"int", !"numValidEyes"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_metadata_struct_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_metadata_struct_memcpy_with_trailing_padding_copies_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Matrix = type { [4 x <4 x float>] }
%Agg = type <{ %Matrix, <4 x float>, i32, float, float, [4 x i8] }>

define void @main(i32 %tid, ptr addrspace(2) %src) {
entry:
  %dst = alloca %Agg, align 16
  %idx = zext i32 %tid to i64
  %src_elem = getelementptr inbounds %Agg, ptr addrspace(2) %src, i64 %idx
  %dst_raw = bitcast ptr %dst to ptr
  %src_raw = bitcast ptr addrspace(2) %src_elem to ptr addrspace(2)
  tail call void @llvm.memcpy.p0.p2.i64(ptr align 16 dereferenceable(96) %dst_raw, ptr addrspace(2) align 16 dereferenceable(96) %src_raw, i64 96, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p2.i64(ptr, ptr addrspace(2), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 96, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 96, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"da_tile_aggregation_t", !"air.arg_name", !"src"}
!5 = !{i32 0, i32 64, i32 0, !"float4x4", !"AtA", i32 64, i32 16, i32 0, !"float4", !"Atb", i32 80, i32 4, i32 0, !"uint", !"pixel_cnt", i32 84, i32 4, i32 0, !"float", !"min_distance", i32 88, i32 4, i32 0, !"float", !"max_distance"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_metadata_struct_padding_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpCopyMemory").count(), 12, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_metadata_memcpy_through_private_vector_struct_preserves_padding() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Corner = type { <2 x float>, float }

define void @main(ptr addrspace(1) %dst, ptr addrspace(1) %src, i32 %i, i32 %j) {
entry:
  %tmp = alloca %Corner, align 8
  %ip = zext i32 %i to i64
  %jp = zext i32 %j to i64
  %dst_i = getelementptr inbounds %Corner, ptr addrspace(1) %dst, i64 %ip
  %src_j = getelementptr inbounds %Corner, ptr addrspace(1) %src, i64 %jp
  %tmp_raw = bitcast ptr %tmp to ptr
  %dst_raw = bitcast ptr addrspace(1) %dst_i to ptr addrspace(1)
  %src_raw = bitcast ptr addrspace(1) %src_j to ptr addrspace(1)
  call void @llvm.memcpy.p0.p1.i64(ptr align 8 dereferenceable(16) %tmp_raw, ptr addrspace(1) align 8 dereferenceable(16) %dst_raw, i64 16, i1 false)
  call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) align 8 dereferenceable(16) %dst_raw, ptr addrspace(1) align 8 dereferenceable(16) %src_raw, i64 16, i1 false)
  call void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) align 8 dereferenceable(16) %src_raw, ptr align 8 dereferenceable(16) %tmp_raw, i64 16, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p1.i64(ptr, ptr addrspace(1), i64, i1)
declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)
declare void @llvm.memcpy.p1.p0.i64(ptr addrspace(1), ptr, i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"Corner", !"air.arg_name", !"dst"}
!4 = !{i32 0, i32 8, i32 0, !"float2", !"corner", i32 8, i32 4, i32 0, !"float", !"score"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"Corner", !"air.arg_name", !"src"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!7 = !{i32 3, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"j"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_vector_struct_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(asm.matches("OpStore").count() >= 8, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_record_array_metadata_gep_remaps_padding_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Constants = type <{ i32, i32, [8 x i8], <2 x i32>, i8, [3 x i8], i32 }>

define void @main(ptr addrspace(2) %constants, ptr addrspace(1) %out, i32 %idx) {
entry:
  %tail = getelementptr inbounds %Constants, ptr addrspace(2) %constants, i64 0, i32 6
  %tail_value = load i32, ptr addrspace(2) %tail, align 4
  %idx64 = zext i32 %idx to i64
  %record_head = getelementptr inbounds %Constants, ptr addrspace(2) %constants, i64 %idx64, i32 0
  %head_value = load i32, ptr addrspace(2) %record_head, align 4
  %sum = add i32 %tail_value, %head_value
  store i32 %sum, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"constants_t", !"air.arg_name", !"constants"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"head0", i32 4, i32 4, i32 0, !"uint", !"head1", i32 16, i32 8, i32 0, !"uint2", !"dims", i32 24, i32 1, i32 0, !"uchar", !"flag", i32 28, i32 4, i32 0, !"uint", !"tail"}
!5 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_record_array_metadata_gep_padding_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let kern = meta::parse_air_kernel_meta(ll);
    let ir = super::super::ir::LlModule::parse_with_stage_meta(ll, kern.as_ref(), Some("main"))
        .expect("parse typed IR");
    assert!(
        !ir.entry_param_requires_raw_layout(Some("main"), 0),
        "equal-extent source structs must retain ordinal remapping"
    );
    let kern_meta = meta::parse_air_kernel_meta(ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        ll,
        kern_meta.as_ref(),
        Some("main"),
        kern_meta.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit with AIR layout sidecar");
    assert!(
        emitted
            .sidecar
            .air_struct_offsets
            .values()
            .any(|offsets| offsets == &[0, 4, 8, 16, 24, 25, 28]),
        "the tail sits at byte 28 behind pads at 8 and 25: {:?}",
        emitted.sidecar.air_struct_offsets
    );
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("ArrayStride 32"),
        "the `i64 %idx64` record index strides by the declared 32 bytes: {asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_padded_struct_memcpy_destination_becomes_raw() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%BBox = type { <3 x float>, <3 x float> }

define void @copy(i32 %tid, ptr addrspace(2) %index, ptr addrspace(1) %dst, ptr addrspace(1) %src) {
entry:
  %is_zero = icmp eq i32 %tid, 0
  br i1 %is_zero, label %copy_block, label %done

copy_block:
  %idx = load i32, ptr addrspace(2) %index, align 4
  %idx64 = zext i32 %idx to i64
  %dst_slot = getelementptr inbounds %BBox, ptr addrspace(1) %dst, i64 %idx64
  %dst_raw = bitcast ptr addrspace(1) %dst_slot to ptr addrspace(1)
  %src_raw = bitcast ptr addrspace(1) %src to ptr addrspace(1)
  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) align 16 dereferenceable(32) %dst_raw, ptr addrspace(1) align 16 dereferenceable(32) %src_raw, i64 32, i1 false)
  br label %done

done:
  ret void
}

declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @copy, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !7}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !6, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"_MPSAxisAlignedBoundingBox", !"air.arg_name", !"dst"}
!6 = !{i32 0, i32 16, i32 0, !"float3", !"min", i32 16, i32 16, i32 0, !"float3", !"max"}
!7 = !{i32 3, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !6, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"_MPSAxisAlignedBoundingBox", !"air.arg_name", !"src"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(asm.matches("OpStore").count() >= 8, "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let constants = module
        .types_global_values
        .iter()
        .filter_map(|inst| match (inst.class.opcode, inst.operands.last()) {
            (Op::Constant, Some(Operand::LiteralBit32(value))) => Some(*value),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for word_offset in [0, 4, 7, 8] {
        assert!(
            constants.contains(&word_offset),
            "missing word offset {word_offset} in {constants:?}\n{asm}"
        );
    }
}

#[test]
fn native_raw_buffer_vector_i32_store_splits_to_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %buf) {
entry:
  %head = load i32, ptr addrspace(1) %buf, align 4
  %typed = bitcast ptr addrspace(1) %buf to ptr addrspace(1)
  %slot = getelementptr inbounds <4 x i32>, ptr addrspace(1) %typed, i64 1
  %v0 = insertelement <4 x i32> poison, i32 %head, i64 0
  %v1 = insertelement <4 x i32> %v0, i32 20, i64 1
  %v2 = insertelement <4 x i32> %v1, i32 30, i64 2
  %v3 = insertelement <4 x i32> %v2, i32 40, i64 3
  store <4 x i32> %v3, ptr addrspace(1) %slot, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"buf"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_vec_i32_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        whole_workgroup_options(),
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpCompositeExtract").count(), 4, "{asm}");
    assert_eq!(asm.matches("OpStore").count(), 4, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_direct_wide_vector_buffer_store_uses_aggregate_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out) {
entry:
  store <6 x i32> zeroinitializer, ptr addrspace(1) %out, align 32
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 32, !"air.arg_type_name", !"uint6", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_direct_wide_vector_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load spv");
    let uint = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
        })
        .and_then(|inst| inst.result_id)
        .expect("uint type");
    let array = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeArray
                && inst.operands.first() == Some(&Operand::IdRef(uint))
        })
        .and_then(|inst| inst.result_id)
        .expect("uint array type");
    let ptr_array = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands
                    == [
                        Operand::StorageClass(StorageClass::StorageBuffer),
                        Operand::IdRef(array),
                    ]
        })
        .and_then(|inst| inst.result_id)
        .expect("array storage pointer");
    let ptr_uint = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands
                    == [
                        Operand::StorageClass(StorageClass::StorageBuffer),
                        Operand::IdRef(uint),
                    ]
        })
        .and_then(|inst| inst.result_id);
    let access_chain_types = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::AccessChain)
        .filter_map(|inst| inst.result_type)
        .collect::<Vec<_>>();
    assert!(access_chain_types.contains(&ptr_array));
    assert!(ptr_uint.is_none_or(|ptr_uint| !access_chain_types.contains(&ptr_uint)));
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_workgroup_global_array_scalar_store_uses_first_element() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@counts = internal unnamed_addr addrspace(3) global [33 x i16] undef, align 2

define void @k(ptr addrspace(1) %out) {
entry:
  store i16 0, ptr addrspace(3) @counts, align 2
  %slot = getelementptr inbounds [33 x i16], ptr addrspace(3) @counts, i64 0, i64 0
  %v = load i16, ptr addrspace(3) %slot, align 2
  store i16 %v, ptr addrspace(1) %out, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"ushort*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_workgroup_array_scalar_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let workgroup_vars = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_vars.is_empty(), "{asm}");
    let null_ids = module
        .types_global_values
        .iter()
        .filter_map(|inst| (inst.class.opcode == Op::ConstantNull).then_some(inst.result_id?))
        .collect::<HashSet<_>>();
    let direct_workgroup_store = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_vars.contains(&id))
                && inst
                    .operands
                    .get(1)
                    .and_then(id_ref_operand)
                    .is_some_and(|id| !null_ids.contains(&id))
        });
    assert!(!direct_workgroup_store, "{asm}");
    let workgroup_access_chains = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            (inst.class.opcode == Op::InBoundsAccessChain
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_vars.contains(&id)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_access_chains.is_empty(), "{asm}");
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_access_chains.contains(&id))),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_workgroup_global_array_vector_store_uses_first_element() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@faces = internal unnamed_addr addrspace(3) global [8 x <4 x float>] undef, align 16

define void @k(ptr addrspace(1) %out) {
entry:
  store <4 x float> zeroinitializer, ptr addrspace(3) @faces, align 16
  %slot = getelementptr inbounds [8 x <4 x float>], ptr addrspace(3) @faces, i64 0, i64 0
  %v = load <4 x float>, ptr addrspace(3) %slot, align 16
  store <4 x float> %v, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_workgroup_array_vector_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let workgroup_vars = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_vars.is_empty(), "{asm}");
    let null_ids = module
        .types_global_values
        .iter()
        .filter_map(|inst| (inst.class.opcode == Op::ConstantNull).then_some(inst.result_id?))
        .collect::<HashSet<_>>();
    let direct_workgroup_store = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_vars.contains(&id))
                && inst
                    .operands
                    .get(1)
                    .and_then(id_ref_operand)
                    .is_some_and(|id| !null_ids.contains(&id))
        });
    assert!(!direct_workgroup_store, "{asm}");
    let workgroup_access_chains = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            (inst.class.opcode == Op::InBoundsAccessChain
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_vars.contains(&id)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_access_chains.is_empty(), "{asm}");
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_access_chains.contains(&id))),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_memcpy_from_void_buffer_to_typed_struct_copies_raw_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Draw = type { i32, i32, i32, i32, i32 }

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %src_head_ptr = bitcast ptr addrspace(1) %src to ptr addrspace(1)
  %src_head = load i32, ptr addrspace(1) %src_head_ptr, align 4
  %dst_tail = getelementptr inbounds %Draw, ptr addrspace(1) %dst, i64 0, i32 4
  %dst_raw = bitcast ptr addrspace(1) %dst to ptr addrspace(1)
  tail call void @llvm.memcpy.p1.p1.i64(ptr addrspace(1) %dst_raw, ptr addrspace(1) %src, i64 20, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p1.i64(ptr addrspace(1), ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"void", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 20, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"MTLDrawIndexedPrimitivesIndirectArguments", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_to_typed_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpStore").count(), 5, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_memcpy_copies_one_element_prefix_of_typed_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Matrix = type { [4 x <4 x float>] }

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %dst = alloca [2 x %Matrix], align 16
  %src_head = load i32, ptr addrspace(1) %src, align 4
  %dst_raw = bitcast ptr %dst to ptr
  %src_raw = bitcast ptr addrspace(1) %src to ptr addrspace(1)
  call void @llvm.memcpy.p0.p1.i64(ptr align 16 %dst_raw, ptr addrspace(1) align 16 %src_raw, i64 64, i1 false)
  %first = getelementptr inbounds [2 x %Matrix], ptr %dst, i64 0, i64 0, i32 0, i64 0
  %value = load <4 x float>, ptr %first, align 16
  store <4 x float> %value, ptr addrspace(1) %out, align 16
  ret void
}

declare void @llvm.memcpy.p0.p1.i64(ptr, ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"void"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float4"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_to_typed_array_prefix_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module =
        load_bytes(super::super::emit_vulkan_spirv_all_buffers_raw(ll).expect("native raw emit"))
            .expect("load native spv");
    let spv = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_raw_to_typed_memcpy_constructs_subword_struct_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Payload = type { float, float, i8, float, i32 }

define void @main(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %tmp = alloca %Payload, align 4
  %tmp_bytes = bitcast ptr %tmp to ptr
  %src_bytes = bitcast ptr addrspace(1) %src to ptr addrspace(1)
  call void @llvm.memcpy.p0.p1.i64(ptr %tmp_bytes, ptr addrspace(1) %src_bytes, i64 20, i1 false)
  %byte_ptr = getelementptr inbounds %Payload, ptr %tmp, i64 0, i32 2
  %byte = load i8, ptr %byte_ptr, align 1
  store i8 %byte, ptr addrspace(1) %out, align 1
  ret void
}

declare void @llvm.memcpy.p0.p1.i64(ptr, ptr addrspace(1), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"void"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uchar"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_to_typed_subword_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module =
        load_bytes(super::super::emit_vulkan_spirv_all_buffers_raw(ll).expect("native raw emit"))
            .expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_char_buffer_struct_array_stores_lower_as_raw_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Src = type { i32, i32 }
%Dst = type { i32, i32, i32, i32 }

define void @main(i32 %tid, ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %src_header_byte = getelementptr inbounds i8, ptr addrspace(1) %src, i64 28
  %src_header = bitcast ptr addrspace(1) %src_header_byte to ptr addrspace(1)
  %count = load i32, ptr addrspace(1) %src_header, align 4
  %src_off_byte = getelementptr inbounds i8, ptr addrspace(1) %src, i64 96
  %src_off_ptr = bitcast ptr addrspace(1) %src_off_byte to ptr addrspace(1)
  %off = load i64, ptr addrspace(1) %src_off_ptr, align 8
  %src_base_byte = getelementptr inbounds i8, ptr addrspace(1) %src, i64 %off
  %src_base = bitcast ptr addrspace(1) %src_base_byte to ptr addrspace(1)
  %idx = zext i32 %tid to i64
  %src0p = getelementptr inbounds %Src, ptr addrspace(1) %src_base, i64 %idx, i32 0
  %src0 = load i32, ptr addrspace(1) %src0p, align 4
  %dst_base = bitcast ptr addrspace(1) %dst to ptr addrspace(1)
  %dst0p = getelementptr inbounds %Dst, ptr addrspace(1) %dst_base, i64 %idx, i32 0
  store i32 %src0, ptr addrspace(1) %dst0p, align 4
  %dst1p = getelementptr inbounds %Dst, ptr addrspace(1) %dst_base, i64 %idx, i32 1
  store i32 %count, ptr addrspace(1) %dst1p, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_char_struct_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpStore"), "{asm}");
    for line in asm
        .lines()
        .filter(|line| line.contains("OpInBoundsAccessChain"))
    {
        let operand_count = line
            .split_once("OpInBoundsAccessChain")
            .map(|(_, operands)| operands.split_whitespace().count())
            .unwrap_or(0);
        assert!(
            operand_count <= 4,
            "raw word access chain should not index through a scalar: {line}\n{asm}"
        );
    }
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_i64_copy_uses_access_alignment_for_dynamic_byte_base() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main(i32 %tid, ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %src_base = load i64, ptr addrspace(1) %src, align 8
  %dst_base = load i64, ptr addrspace(1) %dst, align 8
  %src_byte = getelementptr inbounds i8, ptr addrspace(1) %src, i64 %src_base
  %dst_byte = getelementptr inbounds i8, ptr addrspace(1) %dst, i64 %dst_base
  %idx = zext i32 %tid to i64
  %src_item = getelementptr inbounds i64, ptr addrspace(1) %src_byte, i64 %idx
  %dst_item = getelementptr inbounds i64, ptr addrspace(1) %dst_byte, i64 %idx
  %word = load i64, ptr addrspace(1) %src_item, align 8
  store i64 %word, ptr addrspace(1) %dst_item, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(!asm.contains("OpCopyMemory"), "{asm}");
}

#[test]
fn native_raw_unaligned_i32_store_splits_to_byte_atomics() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @StorePacked(i32 %idx, ptr addrspace(1) %bytes) {
entry:
  %idx64 = zext i32 %idx to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %idx64
  %word = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  store i32 287454020, ptr addrspace(1) %word, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @StorePacked, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"bytes"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_unaligned_i32_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpAtomicAnd").count(), 4, "{asm}");
    assert_eq!(asm.matches("OpAtomicOr").count(), 4, "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_unaligned_float_load_reassembles_bytes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @LoadPacked(i32 %idx, ptr addrspace(1) %bytes, ptr addrspace(1) %out) {
entry:
  %idx64 = zext i32 %idx to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %idx64
  %word = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  %v = load float, ptr addrspace(1) %word, align 1
  %dst = getelementptr inbounds float, ptr addrspace(1) %out, i64 %idx64
  store float %v, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @LoadPacked, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"bytes"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_unaligned_float_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_indirect_buffer_pointer_fields_use_device_addresses() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Params = type { i16, ptr addrspace(1), i32 }

define void @main(i32 %tid, ptr addrspace(2) %params) {
entry:
  tail call void @helper(i32 %tid, ptr addrspace(2) %params)
  ret void
}

define internal void @helper(i32 %tid, ptr addrspace(2) %params) {
entry:
  %kindp = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 0
  %kind = load i16, ptr addrspace(2) %kindp
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 1
  %base = load ptr addrspace(1), ptr addrspace(2) %field
  %arg = getelementptr inbounds i8, ptr addrspace(1) %base, i64 4
  store i32 %tid, ptr addrspace(1) %arg, align 4
  tail call void @llvm.memcpy.p1.p2.i64(ptr addrspace(1) %arg, ptr addrspace(2) %params, i64 4, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p2.i64(ptr addrspace(1), ptr addrspace(2), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid"}
!4 = !{i32 1, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_name", !"params"}
!5 = !{i32 0, i32 2, i32 0, !"ushort", !"kind", !"air.indirect_argument", !6, i32 8, i32 8, i32 0, !"uchar", !"payload", !"air.indirect_argument", !7, i32 16, i32 4, i32 0, !"uint", !"count", !"air.indirect_argument", !8}
!6 = !{}
!7 = !{}
!8 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_indirect_buffer_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(asm.contains("PhysicalStorageBuffer64"), "{asm}");
    assert!(asm.contains("OpConvertUToPtr"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(!asm.contains("RuntimeArray %_ptr"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_direct_vector_store_infers_storage_buffer_element() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {
entry:
  store <4 x i32> <i32 40, i32 80, i32 120, i32 255>, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint4", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_direct_vector_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(asm.contains("OpTypeVector") && asm.contains(" 4"), "{asm}");
    assert!(asm.contains("ArrayStride 16"), "{asm}");
    assert!(!asm.contains("OpTypeInt 8 0"), "{asm}");
    assert!(asm.lines().any(|line| line.contains("OpStore")), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_vector_stride_from_scalar_lane_scales_and_splits_store() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %srcp = getelementptr inbounds <4 x i16>, ptr addrspace(1) %src, i64 3
  %value = load <4 x i16>, ptr addrspace(1) %srcp, align 8
  %lane2 = getelementptr inbounds <4 x i16>, ptr addrspace(1) %dst, i64 0, i64 2
  %lane2_alias = bitcast ptr addrspace(1) %lane2 to ptr addrspace(1)
  %record3_lane2 = getelementptr inbounds <4 x i16>, ptr addrspace(1) %lane2_alias, i64 3
  store <4 x i16> %value, ptr addrspace(1) %record3_lane2, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"dst"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let module = load_bytes(&spv).expect("load native spv");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpIMul"), "vector stride not scaled:\n{asm}");
    assert_eq!(
        module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.class.opcode == Op::Store)
            .count(),
        4,
        "the vector payload must split into four scalar stores:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_vector_stride_scalar_lane_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_raw_uint_struct_metadata_reconstructs_typed_layout() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::matrix" = type { [3 x <3 x float>] }
%struct.Params = type { <3 x float>, %"struct.metal::matrix" }

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %params) {
entry:
  %basep = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 0
  %base = load <3 x float>, ptr addrspace(2) %basep, align 16
  %rowp = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 1, i32 0, i64 2
  %row = load <3 x float>, ptr addrspace(2) %rowp, align 16
  %sum = fadd <3 x float> %base, %row
  store <3 x float> %sum, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float3", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 80, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!5 = !{i32 0, i32 16, i32 0, !"float3", !"base", i32 16, i32 48, i32 0, !"float3x3", !"matrix", i32 64, i32 1, i32 0, !"bool", !"enabled", i32 65, i32 1, i32 0, !"bool", !"mode", i32 66, i32 2, i32 0, !"ushort", !"count", i32 68, i32 1, i32 0, !"bool", !"flag"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_uint_struct_metadata_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    let transformed = load_bytes(&out).expect("load transformed SPIR-V");
    let uint32 = transformed
        .types_global_values
        .iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::TypeInt
                && instruction.operands.as_slice()
                    == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
        })
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    assert!(!transformed.types_global_values.iter().any(|instruction| {
        instruction.class.opcode == Op::TypeRuntimeArray
            && matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if uint32.contains(id))
    }), "{asm}");
    assert!(
        transformed.types_global_values.iter().any(|instruction| {
            instruction.class.opcode == Op::TypeStruct && instruction.operands.len() == 6
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_void_tail_call_propagates_array_buffer_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @sum_sh(ptr addrspace(1) %out, ptr addrspace(1) %input) {
entry:
  tail call fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(1) %input)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(1) %input) {
entry:
  %rgb = getelementptr inbounds [3 x float], ptr addrspace(1) %input, i64 2, i64 1
  %v = load float, ptr addrspace(1) %rgb, align 4
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @sum_sh, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"packed_float3", !"air.arg_name", !"input"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_tail_call_array_buffer_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("ArrayStride 12"), "{asm}");
    assert!(!asm.contains("OpTypeInt 8 0"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_inline_raw_buffer_gep_argument_keeps_descriptor_root_and_offset() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"

define void @k(ptr addrspace(1) %out) {
entry:
  %shifted = getelementptr inbounds i32, ptr addrspace(1) %out, i64 4
  tail call fastcc void @helper(ptr addrspace(1) %shifted)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %dst) {
entry:
  store i32 7, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_inline_raw_buffer_gep_arg_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("Binding 0").count(), 1, "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpVariable %_ptr_Private_uint Private")),
        "offset helper store must remain descriptor-backed:\n{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpInBoundsAccessChain")),
        "descriptor-backed helper store must preserve the four-word offset:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_indirect_helper_gep_drops_signed_zero_record_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Params = type <{ ptr addrspace(2), i32, i32, i32, [4 x i8], ptr addrspace(2), [20 x i8] }>

define void @main(i32 %tid, ptr addrspace(1) %params) {
entry:
  %tid64 = zext i32 %tid to i64
  %record = getelementptr inbounds %struct.Params, ptr addrspace(1) %params, i64 %tid64
  tail call void @helper(ptr addrspace(1) %record)
  ret void
}

define internal void @helper(ptr addrspace(1) %params) {
entry:
  %count_field = getelementptr inbounds %struct.Params, ptr addrspace(1) %params, i64 0, i32 5
  %count_buffer = load ptr addrspace(2), ptr addrspace(1) %count_field
  %count = load i32, ptr addrspace(2) %count_buffer
  %out = getelementptr inbounds %struct.Params, ptr addrspace(1) %params, i64 0, i32 6, i64 0
  store i32 %count, ptr addrspace(1) %out
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid"}
!4 = !{i32 1, !"air.indirect_buffer", !"air.location_index", i32 4, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !5, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!5 = !{i32 0, i32 8, i32 0, !"uchar", !"inputArguments", !"air.indirect_argument", !6, i32 8, i32 4, i32 0, !"int", !"commandType", !"air.indirect_argument", !7, i32 12, i32 4, i32 0, !"uint", !"commandStride", !"air.indirect_argument", !8, i32 16, i32 4, i32 0, !"uint", !"commandIndex", !"air.indirect_argument", !9, i32 24, i32 8, i32 0, !"uint", !"commandCount", !"air.indirect_argument", !10, i32 32, i32 1, i32 20, !"uchar", !"outputArguments", !"air.indirect_argument", !11}
!6 = !{}
!7 = !{}
!8 = !{}
!9 = !{}
!10 = !{}
!11 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_indirect_helper_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpInBoundsAccessChain") && line.contains("%uint_0 %uint_6")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_thread_position_uint3_binds_full_global_invocation_id() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(<3 x i32> %tid) {
entry:
  %ok = icmp uge <3 x i32> %tid, zeroinitializer
  %all = tail call i1 @air.all.v3i1(<3 x i1> %ok)
  ret void
}

declare i1 @air.all.v3i1(<3 x i1>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_thread_position_uint3_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("BuiltIn GlobalInvocationId"), "{asm}");
    assert!(asm.contains("OpUGreaterThanEqual"), "{asm}");
    assert!(!asm.contains("OpCompositeExtract %uint"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_quad_shuffle_float_lowers_to_quad_local_subgroup_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, i16 %lane, ptr addrspace(1) %out) {
entry:
  %sx = tail call float @air.quad_shuffle.f32(float %x, i16 %lane)
  store float %sx, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.quad_shuffle.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_quad_shuffle_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(asm.contains("BuiltIn SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selected_buffer_pointer_gep_preserves_typed_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %gid, ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out) {
entry:
  %idx = zext i32 %gid to i64
  %cond = icmp eq i32 %gid, 0
  %selected = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %src = getelementptr inbounds i32, ptr addrspace(1) %selected, i64 %idx
  %value = load i32, ptr addrspace(1) %src, align 4
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %idx
  store i32 %value, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_buffer_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("_ptr_StorageBuffer_uchar"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selected_buffer_pointer_gep_store_replays_values_per_arm() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %gid, ptr addrspace(1) %a, ptr addrspace(1) %b) {
entry:
  %idx = zext i32 %gid to i64
  %cond = icmp eq i32 %gid, 0
  %selected = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %dst = getelementptr inbounds i32, ptr addrspace(1) %selected, i64 %idx
  store i32 %gid, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_buffer_gep_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let mut module = load_bytes(&spv).expect("load native spv");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_selects = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Select
                && inst
                    .result_type
                    .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    let body = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let stores = body
        .iter()
        .filter(|inst| inst.class.opcode == Op::Store)
        .collect::<Vec<_>>();
    assert_eq!(pointer_selects, 0, "{asm}");
    assert_eq!(stores.len(), 2, "one store per candidate buffer: {asm}");
    let store_targets = stores
        .iter()
        .filter_map(|inst| inst.operands.first().cloned())
        .collect::<std::collections::HashSet<_>>();
    assert!(
        !body.iter().any(|inst| inst.class.opcode == Op::Load
            && inst
                .operands
                .first()
                .is_some_and(|ptr| store_targets.contains(ptr))),
        "a guarded store has nothing to read back: {asm}"
    );
    assert_eq!(
        body.iter()
            .filter(|inst| inst.class.opcode == Op::BranchConditional)
            .count(),
        2,
        "each arm's store is guarded by that arm being selected: {asm}"
    );
    let stored_values = stores
        .iter()
        .filter_map(|inst| inst.operands.get(1).cloned())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        stored_values.len(),
        1,
        "both arms store the SAME new value, never a per-arm merge of it: {asm}"
    );
    assert!(
        !crate::native::construct_interface_cross_binding_pointer_values_module(&mut module),
        "interface/finalization ownership must leave no value-replayable pointer closure"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_multiblock_helper_pointer_select_consumer_is_planned_before_emission() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define internal i32 @consume(ptr addrspace(1) %pointer, i1 %choose) {
entry:
  br i1 %choose, label %load, label %zero
load:
  %value = load i32, ptr addrspace(1) %pointer, align 4
  ret i32 %value
zero:
  ret i32 0
}

define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %gid) {
entry:
  %choose = icmp eq i32 %gid, 0
  %selected = select i1 %choose, ptr addrspace(1) %a, ptr addrspace(1) %b
  %value = call i32 @consume(ptr addrspace(1) %selected, i1 %choose)
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary construction");
    let module = load_bytes(&spv).expect("load primary SPIR-V");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| instruction.class.opcode == Op::FunctionCall),
        "{asm}"
    );
    assert!(
        !module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| {
                instruction.class.opcode == Op::Select
                    && instruction
                        .result_type
                        .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
            }),
        "{asm}"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_multiblock_selected_consumer_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_dynamic_local_buffer_table_vector_load_is_primary_valid() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %gid, ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, ptr addrspace(1) %out) {
entry:
  %table = alloca [3 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [3 x ptr addrspace(1)], ptr %table, i32 0, i32 0
  %slot1 = getelementptr inbounds [3 x ptr addrspace(1)], ptr %table, i32 0, i32 1
  %slot2 = getelementptr inbounds [3 x ptr addrspace(1)], ptr %table, i32 0, i32 2
  store ptr addrspace(1) %a, ptr %slot0, align 8
  store ptr addrspace(1) %b, ptr %slot1, align 8
  store ptr addrspace(1) %c, ptr %slot2, align 8
  %selected.slot = getelementptr inbounds [3 x ptr addrspace(1)], ptr %table, i32 0, i32 %gid
  %selected = load ptr addrspace(1), ptr %selected.slot, align 8
  %offset = zext i32 %gid to i64
  %source = getelementptr inbounds <4 x float>, ptr addrspace(1) %selected, i64 %offset
  %value = load <4 x float>, ptr addrspace(1) %source, align 16
  %destination = getelementptr inbounds <4 x float>, ptr addrspace(1) %out, i64 %offset
  store <4 x float> %value, ptr addrspace(1) %destination, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"a"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"b"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"c"}
!7 = !{i32 4, !"air.buffer", !"air.location_index", i32 4, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let module = load_bytes(&spv).expect("load native spv");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| {
                inst.class.opcode == Op::Select
                    && inst
                        .result_type
                        .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
            }),
        "{asm}"
    );
}

#[test]
fn native_selected_i8_buffer_bitcast_vector_load_uses_raw_arms() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %gid) {
entry:
  %cond = icmp eq i32 %gid, 0
  %idx = zext i32 %gid to i64
  %selected = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %slot = getelementptr inbounds i8, ptr addrspace(1) %selected, i64 %idx
  %wide = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  %value = load <4 x i16>, ptr addrspace(1) %wide, align 8
  store <4 x i16> %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_i8_buffer_bitcast_vector_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let pointer_bitcasts = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Bitcast
                && inst
                    .result_type
                    .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    assert_eq!(pointer_bitcasts, 0, "{asm}");
    let v4u16_loads = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Load
                && inst
                    .result_type
                    .is_some_and(|ty| is_unsigned_int_vector(&module, ty, 16, 4))
        })
        .count();
    assert_eq!(v4u16_loads, 0, "{asm}");
    let v4u16_constructs = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::CompositeConstruct
                && inst
                    .result_type
                    .is_some_and(|ty| is_unsigned_int_vector(&module, ty, 16, 4))
        })
        .count();
    assert_eq!(v4u16_constructs, 2, "{asm}");
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::Select
                && inst
                    .result_type
                    .is_some_and(|ty| is_unsigned_int_vector(&module, ty, 16, 4))),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_selected_i8_buffer_bitcast_vector_store_uses_selected_raw_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, i32 %gid) {
entry:
  %cond = icmp eq i32 %gid, 0
  %selected = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %offset = zext i32 %gid to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %selected, i64 %offset
  %wide = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  store <4 x i16> <i16 1, i16 2, i16 3, i16 4>, ptr addrspace(1) %wide, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_i8_buffer_bitcast_vector_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let pointer_selects = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            inst.class.opcode == Op::Select
                && inst
                    .result_type
                    .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    assert_eq!(pointer_selects, 0, "{asm}");
    let selection_merges = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::SelectionMerge)
        .count();
    assert!(selection_merges > 0, "{asm}");
    assert_no_pointer_bitcasts(&spv);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_existing_struct_buffer_uses_air_member_offsets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type { <3 x float>, <2 x float>, [8 x i8] }

define void @k(ptr addrspace(2) readonly align 16 %params, ptr addrspace(1) %out) {
entry:
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 1
  %value = load <2 x float>, ptr addrspace(2) %field, align 16
  store <2 x float> %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 0, i32 12, i32 0, !"float3", !"a", i32 16, i32 8, i32 0, !"float2", !"b"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"float2*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_existing_struct_offsets_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let kern = meta::parse_air_kernel_meta(ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit with AIR layout sidecar");
    assert!(
        emitted
            .sidecar
            .air_struct_offsets
            .values()
            .any(|offsets| offsets == &[0, 16, 24]),
        "{:?}",
        emitted.sidecar.air_struct_offsets
    );
    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::MappedNatural
    );
    assert!(
        emitted
            .sidecar
            .buffer_access_offsets
            .iter()
            .any(|fact| fact.byte_offset == 16),
        "the field GEP must preserve its exact source byte address: {:?}",
        emitted.sidecar.buffer_access_offsets
    );
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpMemberDecorate"), "{asm}");
    assert!(asm.contains("Offset 16"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_record_array_buffer_clones_block_element_struct() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type { i32 }

define void @k(ptr addrspace(2) %direct, ptr addrspace(1) %records, ptr addrspace(1) %out, i32 %idx) {
entry:
  %dptr = getelementptr inbounds %struct.Params, ptr addrspace(2) %direct, i64 0, i32 0
  %d = load i32, ptr addrspace(2) %dptr, align 4
  %idx64 = zext i32 %idx to i64
  %rptr = getelementptr inbounds %struct.Params, ptr addrspace(1) %records, i64 %idx64, i32 0
  %r = load i32, ptr addrspace(1) %rptr, align 4
  %sum = add i32 %d, %r
  store i32 %sum, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !7, !8}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"direct"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"x"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !6, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"records"}
!6 = !{i32 0, i32 4, i32 0, !"uint", !"x"}
!7 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
!8 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_record_array_clone_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let block_types = module
        .annotations
        .iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::Decorate {
                return None;
            }
            match inst.operands.as_slice() {
                [Operand::IdRef(target), Operand::Decoration(Decoration::Block)] => Some(*target),
                _ => None,
            }
        })
        .collect::<HashSet<_>>();
    let runtime_array_block_elements = module
        .types_global_values
        .iter()
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(elem)) if inst.class.opcode == Op::TypeRuntimeArray => Some(*elem),
            _ => None,
        })
        .filter(|elem| block_types.contains(elem))
        .collect::<Vec<_>>();
    assert!(asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(
        runtime_array_block_elements.is_empty(),
        "runtime array elements must not be Block-decorated: {runtime_array_block_elements:?}\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_existing_struct_offsets_skip_backend_padding_arrays() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type <{ i32, [4 x i8], <2 x i32>, float, float }>

define void @k(ptr addrspace(2) readonly align 8 %params, ptr addrspace(1) %out) {
entry:
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 2
  %value = load <2 x i32>, ptr addrspace(2) %field, align 8
  store <2 x i32> %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 24, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 8, i32 8, i32 0, !"uint2", !"b", i32 16, i32 4, i32 0, !"float", !"c", i32 20, i32 4, i32 0, !"float", !"d"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"uint2*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_existing_struct_padding_offsets_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let transformed = load_bytes(&spv).expect("load transformed spv");
    let struct_ty = transformed
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeStruct && inst.operands.len() == 5)
        .and_then(|inst| inst.result_id)
        .unwrap_or_else(|| panic!("five-member params struct\n{asm}"));
    let mut offsets = vec![None; 5];
    for inst in &transformed.annotations {
        if inst.class.opcode != Op::MemberDecorate {
            continue;
        }
        let [Operand::IdRef(target), Operand::LiteralBit32(member), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(offset)] =
            inst.operands.as_slice()
        else {
            continue;
        };
        if *target == struct_ty && (*member as usize) < offsets.len() {
            offsets[*member as usize] = Some(*offset);
        }
    }
    assert_eq!(
        offsets,
        vec![Some(0), Some(4), Some(8), Some(16), Some(20)],
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_existing_struct_offsets_place_unaligned_padding_at_byte_cursor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type <{ i32, i8, [3 x i8], <2 x i32>, float }>

define void @k(ptr addrspace(2) readonly align 8 %params, ptr addrspace(1) %out) {
entry:
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 3
  %value = load <2 x i32>, ptr addrspace(2) %field, align 8
  store <2 x i32> %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 20, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 1, i32 0, !"uchar", !"flag", i32 8, i32 8, i32 0, !"uint2", !"b", i32 16, i32 4, i32 0, !"float", !"c"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"uint2*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_existing_struct_unaligned_padding_offsets_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let transformed = load_bytes(&spv).expect("load transformed spv");
    let struct_ty = transformed
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeStruct && inst.operands.len() == 5)
        .and_then(|inst| inst.result_id)
        .expect("five-member params struct");
    let mut offsets = vec![None; 5];
    for inst in &transformed.annotations {
        if inst.class.opcode != Op::MemberDecorate {
            continue;
        }
        let [Operand::IdRef(target), Operand::LiteralBit32(member), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(offset)] =
            inst.operands.as_slice()
        else {
            continue;
        };
        if *target == struct_ty && (*member as usize) < offsets.len() {
            offsets[*member as usize] = Some(*offset);
        }
    }
    assert_eq!(
        offsets,
        vec![Some(0), Some(4), Some(5), Some(8), Some(16)],
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_local_size_option_updates_execution_and_builtin() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(<2 x i32> %threads) {
entry:
  %sx = extractelement <2 x i32> %threads, i64 0
  %sy = extractelement <2 x i32> %threads, i64 1
  %sum = add i32 %sx, %sy
  %ok = icmp uge i32 %sum, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.threads_per_threadgroup", !"air.arg_type_name", !"uint2", !"air.arg_name", !"threads"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_local_size_option_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [256, 2, 1],
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("BuiltIn WorkgroupSize"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpSpecConstant") && line.contains("256")),
        "{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpSpecConstant") && line.contains("2")),
        "{asm}"
    );
    assert!(asm.contains("OpSpecConstantComposite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_stores_check_wide_byte_offset_before_u32_narrowing() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %offsets, ptr addrspace(1) %dst) {
entry:
  %offset = load i64, ptr addrspace(1) %offsets, align 8
  %target = getelementptr inbounds i8, ptr addrspace(1) %dst, i64 %offset
  store i32 7, ptr addrspace(1) %target, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"offsets"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_wide_raw_store_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let unchecked = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary emit");
    let unchecked_asm = disassemble(&unchecked).expect("disassemble primary store");
    assert!(
        unchecked_asm.contains("OpULessThanEqual"),
        "{unchecked_asm}"
    );
    assert!(
        unchecked_asm.contains("OpSelectionMerge"),
        "{unchecked_asm}"
    );
    let subword_ll = ll.replacen("store i32 7,", "store i16 7,", 1);
    let subword_unchecked =
        crate::translate_native_no_retry(&subword_ll, Stage::Kernel).expect("subword primary emit");
    let subword_asm = disassemble(&subword_unchecked).expect("disassemble subword store");
    assert!(subword_asm.contains("OpAtomicAnd"), "{subword_asm}");
    assert!(subword_asm.contains("OpAtomicOr"), "{subword_asm}");
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpULessThanEqual"), "{asm}");
    assert!(asm.contains("OpSelectionMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_wide_raw_store_control_flow_keeps_loop_on_source_header() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %offsets, ptr addrspace(1) %dst) {
entry:
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %latch ]
  %offset = load i64, ptr addrspace(1) %offsets, align 8
  %target = getelementptr inbounds i8, ptr addrspace(1) %dst, i64 %offset
  store i32 %i, ptr addrspace(1) %target, align 4
  %more = icmp ult i32 %i, 1
  br i1 %more, label %latch, label %exit

latch:
  %next = add i32 %i, 1
  br label %loop

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"offsets"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_wide_raw_store_loop_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    assert!(asm.contains("OpULessThanEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_wide_raw_store_uses_its_emitted_exit_as_phi_predecessor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %offsets, ptr addrspace(1) %dst) {
entry:
  %offset = load i64, ptr addrspace(1) %offsets, align 8
  %left = icmp eq i64 %offset, 0
  br i1 %left, label %store, label %skip

store:
  %target = getelementptr inbounds i8, ptr addrspace(1) %dst, i64 %offset
  store i32 7, ptr addrspace(1) %target, align 4
  br label %merge

skip:
  br label %merge

merge:
  %value = phi i32 [ 1, %store ], [ 2, %skip ]
  store i32 %value, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"offsets"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_wide_raw_store_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary emit");
    let module = load_bytes(&spv).expect("primary module loads");
    let function = module.functions.first().expect("entry function");
    let phi = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .find(|instruction| instruction.class.opcode == Op::Phi)
        .expect("merge phi");
    let predecessor = match phi.operands.get(1) {
        Some(Operand::IdRef(predecessor)) => *predecessor,
        other => panic!("missing first phi predecessor: {other:?}"),
    };
    let predecessor_block = function
        .blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(predecessor))
        .expect("phi predecessor block");
    let guard_label = function
        .blocks
        .iter()
        .find(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| instruction.class.opcode == Op::ULessThanEqual)
        })
        .and_then(|block| block.label.as_ref())
        .and_then(|label| label.result_id)
        .expect("wide-store guard header");
    assert_ne!(
        predecessor, guard_label,
        "the source entry label does not own the emitted outgoing edge"
    );
    assert!(
        predecessor_block
            .instructions
            .last()
            .is_some_and(|instruction| instruction.class.opcode == Op::Branch),
        "phi must name the robust-store guard's emitted exit block"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_struct_record_memcpy_scalarizes_to_leaf_indices() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Tile = type { i32, i32, i32, i32 }

define void @k(ptr addrspace(3) %scratch, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %src_idx = add i64 %idx, 1
  %dst = getelementptr inbounds %struct.Tile, ptr addrspace(3) %scratch, i64 %idx
  %src = getelementptr inbounds %struct.Tile, ptr addrspace(3) %scratch, i64 %src_idx
  tail call void @llvm.memcpy.p3.p3.i64(ptr addrspace(3) %dst, ptr addrspace(3) %src, i64 16, i1 false)
  ret void
}

declare void @llvm.memcpy.p3.p3.i64(ptr addrspace(3), ptr addrspace(3), i64, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Tile", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 4, i32 0, !"uint", !"b", i32 8, i32 4, i32 0, !"uint", !"c", i32 12, i32 4, i32 0, !"uint", !"d"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_struct_memcpy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let workgroup_var = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup))
        })
        .and_then(|inst| inst.result_id)
        .expect("workgroup var");
    let array_ty = variable_pointee_type(&module, workgroup_var).expect("workgroup array type");
    let elem_ty = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeArray && inst.result_id == Some(array_ty))
        .and_then(|inst| inst.operands.first())
        .and_then(|operand| match operand {
            Operand::IdRef(elem_ty) => Some(*elem_ty),
            _ => None,
        })
        .expect("workgroup array element type");
    let member_types = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeStruct && inst.result_id == Some(elem_ty))
        .map(|inst| {
            inst.operands
                .iter()
                .filter_map(id_ref_operand)
                .collect::<Vec<_>>()
        })
        .expect("workgroup array element struct type");
    assert_eq!(member_types.len(), 4, "{asm}");
    assert!(
        member_types.iter().all(|ty| *ty == member_types[0]),
        "{asm}"
    );
    let uint_ptr = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands
                    == [
                        Operand::StorageClass(StorageClass::Workgroup),
                        Operand::IdRef(member_types[0]),
                    ]
        })
        .and_then(|inst| inst.result_id)
        .expect("workgroup member pointer type");
    let constants = module
        .all_inst_iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::Constant {
                return None;
            }
            match (inst.result_id, inst.operands.first()) {
                (Some(id), Some(Operand::LiteralBit32(value))) => Some((id, *value)),
                _ => None,
            }
        })
        .collect::<HashMap<_, _>>();
    let member_chains = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            if !matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain)
                || inst.result_type != Some(uint_ptr)
                || inst.operands.first() != Some(&Operand::IdRef(workgroup_var))
                || inst.operands.len() != 3
            {
                return None;
            }
            inst.operands
                .get(2)
                .and_then(id_ref_operand)
                .and_then(|id| constants.get(&id).copied())
        })
        .collect::<HashSet<_>>();
    let expected_members = [0, 1, 2, 3].into_iter().collect::<HashSet<_>>();
    assert_eq!(member_chains, expected_members, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    assert!(asm.matches("OpLoad").count() >= 4, "{asm}");
    assert!(asm.matches("OpStore").count() >= 4, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_struct_member_store_splits_flattened_record_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Tile = type { i32, i32, i32, i32 }

define void @k(ptr addrspace(3) %scratch, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %field = getelementptr inbounds %struct.Tile, ptr addrspace(3) %scratch, i64 %idx, i32 1
  store i32 %i, ptr addrspace(3) %field, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Tile", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 4, i32 0, !"uint", !"b", i32 8, i32 4, i32 0, !"uint", !"c", i32 12, i32 4, i32 0, !"uint", !"d"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_struct_member_flat_index_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let uint = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
        })
        .and_then(|inst| inst.result_id)
        .expect("uint type");
    let uint_ptr = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands
                    == [
                        Operand::StorageClass(StorageClass::Workgroup),
                        Operand::IdRef(uint),
                    ]
        })
        .and_then(|inst| inst.result_id)
        .expect("workgroup uint pointer");
    let workgroup_vars = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_vars.is_empty(), "{asm}");
    let has_leaf_chain = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain)
                && inst.result_type == Some(uint_ptr)
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|base| workgroup_vars.contains(&base))
                && inst.operands.len() == 3
        });
    assert!(has_leaf_chain, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_param_uses_pointer_addrspace_without_metadata_address_space() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %value = getelementptr inbounds float, ptr addrspace(3) %temp, i64 %idx
  store float 1.000000e+00, ptr addrspace(3) %value, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_addrspace_fallback_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Workgroup"), "{asm}");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.contains("512")),
        "{asm}"
    );
    assert!(!asm.contains("DescriptorSet"), "{asm}");
    assert!(!asm.contains("Binding"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_address_load_eq_null_compares_the_address() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Params = type { ptr addrspace(1), i32 }

define void @main(ptr addrspace(2) %params) {
entry:
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 0
  %out = load ptr addrspace(1), ptr addrspace(2) %field
  %isnull = icmp eq ptr addrspace(1) %out, null
  br i1 %isnull, label %done, label %write

write:
  store i32 7, ptr addrspace(1) %out, align 4
  br label %done

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_name", !"params"}
!4 = !{i32 0, i32 8, i32 0, !"uint", !"out", !"air.indirect_argument", !5, i32 8, i32 4, i32 0, !"uint", !"tag", !"air.indirect_argument", !6}
!5 = !{}
!6 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_ptr_nullness_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(asm.contains("OpTypeInt 64 0"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_address_loads_compare_materialized_addresses() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Params = type { ptr addrspace(1), ptr addrspace(1) }

define void @main(ptr addrspace(2) %params) {
entry:
  %a_field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 0
  %a = load ptr addrspace(1), ptr addrspace(2) %a_field
  %b_field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 1
  %b = load ptr addrspace(1), ptr addrspace(2) %b_field
  %same = icmp eq ptr addrspace(1) %a, %b
  br i1 %same, label %done, label %check_different

check_different:
  %different = icmp ne ptr addrspace(1) %a, %b
  br i1 %different, label %write, label %done

write:
  store i32 7, ptr addrspace(1) %a, align 4
  br label %done

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_name", !"params"}
!4 = !{i32 0, i32 8, i32 0, !"uint", !"a", !"air.indirect_argument", !5, i32 8, i32 8, i32 0, !"uint", !"b", !"air.indirect_argument", !6}
!5 = !{}
!6 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_ptr_payload_eq_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(
        module,
        Stage::Kernel,
        None,
        None,
        meta::parse_air_kernel_meta(ll).as_ref(),
        meta::entry_name(ll, "kernel").as_deref(),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|w| w.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.matches("OpIEqual").count() >= 4, "{asm}");
    assert!(asm.contains("OpLogicalNot"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_direct_pointer_param_icmp_folds_identity() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @same(ptr addrspace(1) %a) {
entry:
  %c = icmp eq ptr addrspace(1) %a, %a
  ret i1 %c
}

define i1 @distinct_eq(ptr addrspace(1) %a, ptr addrspace(1) %b) {
entry:
  %c = icmp eq ptr addrspace(1) %a, %b
  ret i1 %c
}

define i1 @distinct_ne(ptr addrspace(1) %a, ptr addrspace(1) %b) {
entry:
  %c = icmp ne ptr addrspace(1) %a, %b
  ret i1 %c
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantTrue"), "{asm}");
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    assert!(!asm.contains("OpIEqual"), "{asm}");
    assert!(!asm.contains("OpINotEqual"), "{asm}");
}

#[test]
fn native_pointer_icmp_compares_flattened_gep_provenance_indices() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@rot = internal addrspace(2) constant [2 x [4 x i32]] [[4 x i32] [i32 13, i32 15, i32 26, i32 6], [4 x i32] [i32 17, i32 29, i32 16, i32 24]], align 4

define i32 @walk(i32 %row) {
entry:
  %masked = and i32 %row, 1
  %idx = zext i32 %masked to i64
  %end = getelementptr inbounds [2 x [4 x i32]], ptr addrspace(2) @rot, i64 0, i64 %idx, i64 4
  %begin = getelementptr inbounds [2 x [4 x i32]], ptr addrspace(2) @rot, i64 0, i64 %idx, i64 0
  br label %loop

loop:
  %p = phi ptr addrspace(2) [ %begin, %entry ], [ %next, %body ]
  %acc = phi i32 [ 0, %entry ], [ %sum, %body ]
  %value = load i32, ptr addrspace(2) %p, align 4
  %sum = add i32 %acc, %value
  %next = getelementptr inbounds i32, ptr addrspace(2) %p, i64 1
  %done = icmp eq ptr addrspace(2) %next, %end
  br i1 %done, label %exit, label %body

body:
  br label %loop

exit:
  ret i32 %sum
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(asm.contains("OpLogicalAnd"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn native_pointer_icmp_reserves_forward_gep_phi_root_provenance() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@rot = internal addrspace(2) constant [2 x [4 x i32]] [[4 x i32] [i32 13, i32 15, i32 26, i32 6], [4 x i32] [i32 17, i32 29, i32 16, i32 24]], align 4

define i32 @walk(i64 %row) {
entry:
  %end = getelementptr inbounds [2 x [4 x i32]], ptr addrspace(2) @rot, i64 0, i64 %row, i64 4
  %start = getelementptr inbounds [2 x [4 x i32]], ptr addrspace(2) @rot, i64 0, i64 %row, i64 0
  br label %loop

loop:
  %p = phi ptr addrspace(2) [ %next, %loop ], [ %start, %entry ]
  %acc = phi i32 [ %value, %loop ], [ 0, %entry ]
  %value = load i32, ptr addrspace(2) %p, align 4
  %next = getelementptr inbounds i32, ptr addrspace(2) %p, i64 1
  %done = icmp eq ptr addrspace(2) %next, %end
  br i1 %done, label %exit, label %loop

exit:
  ret i32 %acc
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_forward_gep_phi_root_icmp_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_function_pointer_icmp_compares_loop_cursor_to_end_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Box = type { [3 x float], [3 x float], float }

define void @k() {
entry:
  %arr = alloca [2 x %struct.Box], align 4
  %start = getelementptr inbounds [2 x %struct.Box], ptr %arr, i64 0, i64 0
  %end = getelementptr inbounds [2 x %struct.Box], ptr %arr, i64 0, i64 2
  br label %loop

loop:
  %p = phi ptr [ %start, %entry ], [ %next, %loop ]
  %f = getelementptr inbounds %struct.Box, ptr %p, i64 0, i32 2
  store float 1.0, ptr %f, align 4
  %next = getelementptr inbounds %struct.Box, ptr %p, i64 1
  %done = icmp eq ptr %next, %end
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_function_pointer_loop_end_icmp_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_entry_pointer_param_eq_null_uses_bound_resource_nullness() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @k(ptr addrspace(2) %maybe) {
entry:
  %isnull = icmp eq ptr addrspace(2) %maybe, null
  ret i1 %isnull
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"maybe"}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn native_alloca_eq_null_uses_intrinsic_nonnullness() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @k() {
entry:
  %slot = alloca i32, align 4
  %isnull = icmp eq ptr %slot, null
  ret i1 %isnull
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn native_literal_null_eq_null_folds_true() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @k() {
entry:
  %isnull = icmp eq ptr null, null
  ret i1 %isnull
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantTrue"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn typed_inline_keeps_helper_parameter_semantics_until_emission_finishes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i1 @k() {
entry:
  %slot = alloca i32, align 4
  %same = call i1 @same_pointer(ptr %slot, ptr %slot)
  ret i1 %same
}

define internal i1 @same_pointer(ptr %left, ptr %right) {
entry:
  %same = icmp eq ptr %left, %right
  ret i1 %same
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(
        module
            .functions
            .iter()
            .filter(|function| !function.blocks.is_empty())
            .count(),
        1,
        "the helper body is typed-inlined before emission"
    );
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    let definitions = module
        .all_inst_iter()
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    assert!(
        module.all_inst_iter().all(|instruction| {
            instruction.operands.iter().all(|operand| match operand {
                Operand::IdRef(id) => definitions.contains(id),
                _ => true,
            })
        }),
        "deferred helper-parameter ids must be fully substituted"
    );
}

#[test]
fn typed_inline_materializes_pointer_field_gep_for_helper_parameter_store() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Wrap = type { ptr addrspace(1), i64 }

define void @k(ptr addrspace(1) %src) {
entry:
  %wrap = alloca %Wrap, align 8
  call void @init(ptr %wrap, ptr addrspace(1) %src)
  ret void
}

define internal void @init(ptr %w, ptr addrspace(1) %p) {
entry:
  %field = getelementptr inbounds %Wrap, ptr %w, i64 0, i32 0
  store ptr addrspace(1) %p, ptr %field, align 8
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native spv");
    assert_eq!(
        module
            .functions
            .iter()
            .filter(|function| !function.blocks.is_empty())
            .count(),
        1,
        "the helper body is typed-inlined before emission"
    );
    let definitions = module
        .all_inst_iter()
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    assert!(
        module.all_inst_iter().all(|instruction| {
            instruction.operands.iter().all(|operand| match operand {
                Operand::IdRef(id) => definitions.contains(id),
                _ => true,
            })
        }),
        "inlined pointer-field store must not reference a missing helper GEP"
    );
}

#[test]
fn typed_inline_records_storage_for_extracted_pointer_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Ext = type { ptr addrspace(2), ptr addrspace(2), ptr addrspace(2) }
%Entity = type { float, i32 }

define void @k(ptr addrspace(2) %a, ptr addrspace(2) %b, ptr addrspace(2) %c, ptr addrspace(1) %out) {
entry:
  %ext0 = insertvalue %Ext poison, ptr addrspace(2) %a, 0
  %ext1 = insertvalue %Ext %ext0, ptr addrspace(2) %b, 1
  %ext2 = insertvalue %Ext %ext1, ptr addrspace(2) %c, 2
  %value = call i32 @read_entity(%Ext %ext2)
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

define internal i32 @read_entity(%Ext %ext) {
entry:
  %entity = extractvalue %Ext %ext, 2
  %field = getelementptr inbounds %Entity, ptr addrspace(2) %entity, i64 0, i32 1
  %value = load i32, ptr addrspace(2) %field, align 4
  ret i32 %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native SPIR-V");
    assert_eq!(
        module
            .functions
            .iter()
            .filter(|function| !function.blocks.is_empty())
            .count(),
        1,
        "the helper body is typed-inlined before emission"
    );
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
}

#[test]
fn native_by_value_buffer_member_keeps_llvm_leading_zero_during_flattening() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%View = type { [944 x i8], i32 }
%Wrapper = type { ptr addrspace(2) }

define void @k(ptr addrspace(2) %view, ptr addrspace(1) %out) {
entry:
  %wrapped = insertvalue %Wrapper poison, ptr addrspace(2) %view, 0
  %value = call fastcc i32 @read_member(%Wrapper %wrapped)
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

define internal fastcc i32 @read_member(%Wrapper %wrapped) {
entry:
  %view = extractvalue %Wrapper %wrapped, 0
  %field = getelementptr inbounds %View, ptr addrspace(2) %view, i64 0, i32 1
  %value = load i32, ptr addrspace(2) %field, align 4
  ret i32 %value
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 948, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 948, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"View", !"air.arg_name", !"view"}
!4 = !{i32 0, i32 1, i32 944, !"uchar", !"padding", i32 944, i32 4, i32 0, !"uint", !"value"}
!5 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_by_value_buffer_member_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("translate by-value buffer member");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.contains("OpPtrAccessChain %_ptr_StorageBuffer_View"),
        "{asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn typed_inline_retains_pruned_helper_type_capability() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  %slot = alloca i32, align 4
  %same = call ptr @identity(ptr %slot)
  ret void
}

define internal ptr @identity(ptr %pointer) {
entry:
  ret ptr %pointer
}
"#;
    let module =
        load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native SPIR-V");
    assert!(
        module.capabilities.iter().any(|instruction| {
            matches!(
                instruction.operands.as_slice(),
                [Operand::Capability(Capability::Int8)]
            )
        }),
        "pruning a helper must retain the type capability its residual emission requested"
    );
    assert_eq!(
        module
            .functions
            .iter()
            .filter(|function| !function.blocks.is_empty())
            .count(),
        1,
        "the capability is retained after the helper body is pruned"
    );
}

#[test]
fn a_width_capability_no_type_needs_does_not_survive_finalization() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out, i32 %gid) {
entry:
  %slot = alloca i32, align 4
  %same = call ptr @identity(ptr %slot)
  store i32 7, ptr %same, align 4
  %v = load i32, ptr %same, align 4
  %i = zext i32 %gid to i64
  %o = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %i
  store i32 %v, ptr addrspace(1) %o, align 4
  ret void
}

define internal ptr @identity(ptr %pointer) {
entry:
  ret ptr %pointer
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_unused_width_capability_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let module = load_bytes(&spv).expect("load native spv");
    let _ = std::fs::remove_dir_all(&tmp);

    let scalar_widths: Vec<(Op, u32)> = module
        .types_global_values
        .iter()
        .filter(|inst| matches!(inst.class.opcode, Op::TypeInt | Op::TypeFloat))
        .filter_map(|inst| match inst.operands.first() {
            Some(&Operand::LiteralBit32(width)) => Some((inst.class.opcode, width)),
            _ => None,
        })
        .collect();
    assert!(
        !scalar_widths.contains(&(Op::TypeInt, 8)),
        "the fixture has no 8-bit type, saw {scalar_widths:?}"
    );

    let unbacked: Vec<Capability> = module
        .capabilities
        .iter()
        .filter_map(|inst| match inst.operands.first() {
            Some(&Operand::Capability(capability)) => Some(capability),
            _ => None,
        })
        .filter(|capability| {
            !scalar_widths.iter().any(|&(opcode, width)| {
                crate::native::scalar_width_capability(opcode, width) == Some(*capability)
            })
        })
        .filter(|capability| {
            matches!(
                capability,
                Capability::Int8
                    | Capability::Int16
                    | Capability::Int64
                    | Capability::Float16
                    | Capability::Float64
            )
        })
        .collect();
    assert!(
        unbacked.is_empty(),
        "no width capability outlives the type that asked for it, saw {unbacked:?}"
    );
}

#[test]
fn native_internal_pointer_param_eq_null_uses_callsite_nonnullness() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out) {
entry:
  %slot = alloca i32, align 4
  call fastcc void @nonnull_helper(ptr %slot)
  call fastcc void @nullable_helper(ptr null)
  ret void
}

define internal fastcc void @nonnull_helper(ptr %p) {
entry:
  %isnull = icmp eq ptr %p, null
  ret void
}

define internal fastcc void @nullable_helper(ptr %p) {
entry:
  %isnull = icmp eq ptr %p, null
  ret void
}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    let emitter = Emitter::new(ir.clone());
    let nonnull = emitter
        .infer_function_param_nonnull(&ir.functions)
        .expect("infer nonnull params");
    assert!(nonnull.contains(&("nonnull_helper".to_string(), 0)));
    assert!(!nonnull.contains(&("nullable_helper".to_string(), 0)));
    let nullness = emitter
        .infer_function_param_nullness(&ir.functions)
        .expect("infer observed nullness params");
    assert!(!nullness.contains(&("nonnull_helper".to_string(), 0)));
    assert!(nullness.contains(&("nullable_helper".to_string(), 0)));
}

#[test]
fn native_multiblock_helper_carries_nullable_pointer_shadow() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out, i1 %choose) {
entry:
  %isnull = call i1 @nullable_helper(ptr addrspace(2) null, i1 %choose)
  %word = zext i1 %isnull to i32
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

define internal i1 @nullable_helper(ptr addrspace(2) %maybe, i1 %choose) {
entry:
  br i1 %choose, label %check, label %other
check:
  %isnull = icmp eq ptr addrspace(2) %maybe, null
  ret i1 %isnull
other:
  ret i1 false
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load native SPIR-V");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        module
            .functions
            .iter()
            .any(|function| function.parameters.len() == 3),
        "the helper carries its two authored parameters plus one nullness shadow"
    );
    assert!(asm.contains("OpConstantTrue"), "{asm}");
}

#[test]
fn native_internal_pointer_param_from_entry_gep_is_nonnull() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Params = type { [4 x i32], [4 x i32] }

define void @k(ptr addrspace(2) %params) {
entry:
  %field = getelementptr inbounds %Params, ptr addrspace(2) %params, i64 0, i32 1, i64 0
  call fastcc void @helper(ptr addrspace(2) %field)
  ret void
}

define internal fastcc void @helper(ptr addrspace(2) %maybe) {
entry:
  %isnull = icmp eq ptr addrspace(2) %maybe, null
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    let emitter = Emitter::new(ir.clone());
    let nonnull = emitter
        .infer_function_param_nonnull(&ir.functions)
        .expect("infer nonnull params");
    assert!(nonnull.contains(&("helper".to_string(), 0)));

    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    assert!(!asm.contains("OpPtrEqual"), "{asm}");
}

#[test]
fn native_internal_pointer_param_from_nonzero_inbounds_loaded_gep_is_nonnull() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Params = type { [4 x i32], [4 x i32] }
%Args = type { ptr addrspace(2) }

define void @k(ptr %args) {
entry:
  %slot = getelementptr inbounds %Args, ptr %args, i64 0, i32 0
  %base = load ptr addrspace(2), ptr %slot
  %field = getelementptr inbounds %Params, ptr addrspace(2) %base, i64 0, i32 1, i64 0
  call fastcc void @helper(ptr addrspace(2) %field)
  ret void
}

define internal fastcc void @helper(ptr addrspace(2) %maybe) {
entry:
  %isnull = icmp eq ptr addrspace(2) %maybe, null
  ret void
}
"#;
    let ir = super::super::ir::LlModule::parse_with_stage_meta(ll, None, Some("k")).expect("parse");
    let emitter = Emitter::new(ir.clone());
    let nonnull = emitter
        .infer_function_param_nonnull(&ir.functions)
        .expect("infer nonnull params");
    assert!(nonnull.contains(&("helper".to_string(), 0)));
}

#[test]
fn native_inttoptr_lowers_to_unmodeled_pointer_value() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @carry_int_pointer(i1 %cond, i64 %addr, ptr addrspace(2) %fallback) {
entry:
  br i1 %cond, label %from_int, label %from_param

from_int:
  %p = inttoptr i64 %addr to ptr addrspace(2)
  br label %merge

from_param:
  br label %merge

merge:
  %m = phi ptr addrspace(2) [ %p, %from_int ], [ %fallback, %from_param ]
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVariable"), "{asm}");
    assert!(asm.contains("Private"), "{asm}");
    assert!(asm.contains("OpPhi"), "{asm}");
    assert!(!asm.contains("OpBitcast"), "{asm}");
}

#[test]
fn native_unmodeled_gep_to_pointer_field_uses_byte_placeholder() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Header = type { i64, ptr addrspace(2) }
define i32 @pointer_field(i64 %addr) {
entry:
  %base = inttoptr i64 %addr to ptr addrspace(2)
  %field = getelementptr inbounds %Header, ptr addrspace(2) %base, i64 0, i32 1
  %p = load ptr addrspace(2), ptr addrspace(2) %field
  %elt = getelementptr inbounds i32, ptr addrspace(2) %p, i64 0
  %v = load i32, ptr addrspace(2) %elt
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVariable"), "{asm}");
    assert!(!asm.contains("_ptr_Private__ptr_"), "{asm}");
}

#[test]
fn native_unmodeled_pointer_placeholder_uses_storage_only_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Inner = type { ptr addrspace(2), i64 }
%Outer = type { %Inner, i32 }
define void @main() {
entry:
  %base = inttoptr i64 0 to ptr addrspace(1)
  %field = getelementptr inbounds %Outer, ptr addrspace(1) %base, i64 0, i32 0
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_storage_only_placeholder_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("_ptr_Private__ptr_"), "{asm}");
    assert!(!asm.contains("OpTypeStruct %_ptr_UniformConstant"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gep_preserves_device_addrspace_for_loads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.S = type { i32 }
define i32 @load_device(ptr addrspace(1) %p) {
entry:
  %g = getelementptr inbounds %struct.S, ptr addrspace(1) %p, i64 0, i32 0
  %v = load i32, ptr addrspace(1) %g
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_threadgroup_atomic_param_struct_gep_keeps_backing_array_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }

define void @k(ptr addrspace(3) noundef align 4 captures(none) dereferenceable(4) "air-buffer-no-alias" %0, <2 x i16> noundef %1) local_unnamed_addr {
  %3 = getelementptr inbounds %"struct.metal::_atomic", ptr addrspace(3) %0, i64 0, i32 0
  tail call void @air.atomic.local.store.i32(ptr addrspace(3) captures(none) %3, i32 0, i32 0, i32 1, i1 true)
  %4 = tail call i32 @air.atomic.local.load.i32(ptr addrspace(3) captures(none) %3, i32 0, i32 1, i1 true)
  ret void
}

declare void @air.atomic.local.store.i32(ptr addrspace(3) captures(none), i32, i32, i32, i1)
declare i32 @air.atomic.local.load.i32(ptr addrspace(3) captures(none), i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !5, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"counter"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"value", !"air.atomic"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_param_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicStore"), "{asm}");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    let store_ptr = asm
        .lines()
        .find(|l| l.contains("OpAtomicStore"))
        .and_then(|l| l.split_whitespace().find(|w| w.starts_with('%')))
        .expect("OpAtomicStore pointer operand")
        .to_string();
    let chain = asm
        .lines()
        .find(|l| l.contains(&format!("{store_ptr} = OpInBoundsAccessChain")))
        .expect("atomic pointer access chain");
    let n_indices = chain.split(" = ").nth(1).map_or(0, |rhs| {
        rhs.split_whitespace()
            .filter(|w| w.starts_with('%'))
            .count()
            - 2
    });
    assert_eq!(
        n_indices, 1,
        "expected flattened scalar element index: {chain}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_atomic_global_struct_pointer_peels_i32_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
@counter = internal addrspace(3) global %"struct.metal::_atomic" zeroinitializer, align 4

define void @k() {
entry:
  tail call void @air.atomic.local.store.i32(ptr addrspace(3) @counter, i32 0, i32 0, i32 1, i1 true)
  tail call void @air.wg.barrier(i32 2, i32 1)
  %v = tail call i32 @air.atomic.local.load.i32(ptr addrspace(3) @counter, i32 0, i32 1, i1 true)
  %old = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) @counter, i32 1, i32 0, i32 1, i1 true)
  ret void
}

declare void @air.atomic.local.store.i32(ptr addrspace(3), i32, i32, i32, i1)
declare void @air.wg.barrier(i32, i32)
declare i32 @air.atomic.local.load.i32(ptr addrspace(3), i32, i32, i1)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_struct_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        whole_workgroup_options(),
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpAtomicStore"), "{asm}");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(asm.contains("OpAtomicIAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_atomic_multifield_global_peels_first_atomic_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
%struct.Counters = type { %"struct.metal::_atomic", %"struct.metal::_atomic", %"struct.metal::_atomic" }
@counters = internal addrspace(3) global %struct.Counters zeroinitializer, align 4

define void @k() {
entry:
  tail call void @air.atomic.local.store.i32(ptr addrspace(3) @counters, i32 0, i32 0, i32 1, i1 true)
  %old = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) @counters, i32 1, i32 0, i32 1, i1 true)
  ret void
}

declare void @air.atomic.local.store.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_multifield_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicStore"), "{asm}");
    assert!(asm.contains("OpAtomicIAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_atomic_union_singleton_array_peels_first_scalar_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
%union.atomicUint = type { [1 x %"struct.metal::_atomic"] }
@counter = internal addrspace(3) global %union.atomicUint undef, align 4

define void @k() {
entry:
  %old = tail call i32 @air.atomic.local.max.u.i32(ptr addrspace(3) @counter, i32 7, i32 0, i32 1, i1 true)
  ret void
}

declare i32 @air.atomic.local.max.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_union_singleton_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpAtomicUMax"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_atomic_array_load_uses_storage_buffer_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%"struct.metal::_atomic" = type { i32 }
%struct.Counters = type <{ [16 x %"struct.metal::_atomic"] }>

define void @k(ptr addrspace(1) %bytes, ptr addrspace(2) %base_ptr, ptr addrspace(2) %idx_ptr, ptr addrspace(1) %out) {
entry:
  %base = load i32, ptr addrspace(2) %base_ptr, align 4
  %base64 = zext i32 %base to i64
  %raw = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %base64
  %counters = bitcast ptr addrspace(1) %raw to ptr addrspace(1)
  %idx = load i32, ptr addrspace(2) %idx_ptr, align 4
  %idx64 = zext i32 %idx to i64
  %slot = getelementptr inbounds %struct.Counters, ptr addrspace(1) %counters, i64 0, i32 0, i64 %idx64, i32 0
  %v = tail call i32 @air.atomic.global.load.i32(ptr addrspace(1) %slot, i32 0, i32 2, i1 true)
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.atomic.global.load.i32(ptr addrspace(1), i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"bytes"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"base"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_atomic_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(
        !asm.contains("OpVariable %_ptr_Workgroup_uint Workgroup"),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_atomic_int_on_float3_buffer_lowers_via_raw_word() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %bbox) {
entry:
  %l0 = getelementptr inbounds <3 x float>, ptr addrspace(1) %bbox, i64 0, i64 0
  %p0 = bitcast ptr addrspace(1) %l0 to ptr addrspace(1)
  %o0 = tail call i32 @air.atomic.global.min.s.i32(ptr addrspace(1) %p0, i32 1056964608, i32 0, i32 2, i1 true)
  %l2 = getelementptr inbounds <3 x float>, ptr addrspace(1) %bbox, i64 0, i64 2
  %p2 = bitcast ptr addrspace(1) %l2 to ptr addrspace(1)
  %o2 = tail call i32 @air.atomic.global.max.s.i32(ptr addrspace(1) %p2, i32 2, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.min.s.i32(ptr addrspace(1), i32, i32, i32, i1)
declare i32 @air.atomic.global.max.s.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float3", !"air.arg_name", !"bbox"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_atomic_int_float3_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicSMin"), "{asm}");
    assert!(asm.contains("OpAtomicSMax"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_atomic_i32_lowers_to_device_scope_spirv() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %count) {
entry:
  %old = tail call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %count, i32 1, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"count"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicIAdd"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let atomic = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::AtomicIAdd)
        .expect("atomic add");
    let scope_id = match atomic.operands.get(1) {
        Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
        other => panic!("unexpected atomic scope operand: {other:?}"),
    };
    let scope_value = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
        .expect("scope constant");
    assert_eq!(scope_value, Scope::Device as u32, "{asm}");
}

#[test]
fn native_device_atomic_store_i32_lowers_to_device_scope_spirv() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %count) {
entry:
  tail call void @air.atomic.global.store.i32(ptr addrspace(1) %count, i32 0, i32 0, i32 2, i1 true)
  ret void
}

declare void @air.atomic.global.store.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"count"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicStore"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let atomic = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::AtomicStore)
        .expect("atomic store");
    let scope_id = match atomic.operands.get(1) {
        Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
        other => panic!("unexpected atomic scope operand: {other:?}"),
    };
    let scope_value = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
        .expect("scope constant");
    assert_eq!(scope_value, Scope::Device as u32, "{asm}");
}

#[test]
fn native_device_atomic_cmpxchg_weak_i32_updates_compare_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define i32 @atomic_cmpxchg(ptr addrspace(1) %slot, i32 %expected, i32 %desired) {
entry:
  %compare = alloca i32, align 4
  store i32 %expected, ptr %compare, align 4
  %old = call i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1) %slot, ptr %compare, i32 %desired, i32 0, i32 0, i32 2, i1 true)
  %seen = load i32, ptr %compare, align 4
  %sum = add i32 %old, %seen
  ret i32 %sum
}

declare i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1), ptr, i32, i32, i32, i32, i1)
"#;
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let atomic = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::AtomicCompareExchange)
        .expect("atomic cmpxchg");
    let scope_id = match atomic.operands.get(1) {
        Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
        other => panic!("unexpected atomic scope operand: {other:?}"),
    };
    let scope_value = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
        .expect("scope constant");
    assert_eq!(scope_value, Scope::Device as u32);
    assert!(
        module
            .all_inst_iter()
            .any(|inst| inst.class.opcode == Op::Store),
        "expected compare pointer update store"
    );
}

#[test]
fn native_device_atomic_f32_add_lowers_to_ext_atomic_float() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%"struct.metal::_atomic" = type { float }

define void @k(ptr addrspace(1) %stats, ptr addrspace(1) %out) {
entry:
  %slot = getelementptr inbounds %"struct.metal::_atomic", ptr addrspace(1) %stats, i64 1, i32 0
  %old = tail call fast float @air.atomic.global.add.f32(ptr addrspace(1) %slot, float 1.250000e+00, i32 0, i32 2, i1 true)
  store float %old, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.atomic.global.add.f32(ptr addrspace(1), float, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"stats"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_device_atomic_f32_add_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("OpExtension \"SPV_EXT_shader_atomic_float_add\""),
        "{asm}"
    );
    assert!(asm.contains("OpCapability AtomicFloat32AddEXT"), "{asm}");
    assert!(asm.contains("OpAtomicFAddEXT"), "{asm}");
    assert_no_pointer_bitcasts(&spv);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_atomic_f32_sub_lowers_to_negated_ext_atomic_float_add() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%"struct.metal::_atomic" = type { float }

define void @k(ptr addrspace(1) %stats, ptr addrspace(1) %out) {
entry:
  %slot = getelementptr inbounds %"struct.metal::_atomic", ptr addrspace(1) %stats, i64 1, i32 0
  %old = tail call fast float @air.atomic.global.sub.f32(ptr addrspace(1) %slot, float 1.250000e+00, i32 0, i32 2, i1 true)
  store float %old, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.atomic.global.sub.f32(ptr addrspace(1), float, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"stats"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_device_atomic_f32_sub_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("OpExtension \"SPV_EXT_shader_atomic_float_add\""),
        "{asm}"
    );
    assert!(asm.contains("OpCapability AtomicFloat32AddEXT"), "{asm}");
    assert!(asm.contains("OpFNegate"), "{asm}");
    assert!(asm.contains("OpAtomicFAddEXT"), "{asm}");
    assert_no_pointer_bitcasts(&spv);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_unmodeled_device_atomic_cmpxchg_uses_workgroup_scratch() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define i32 @atomic_cmpxchg_unmodeled(i32 %expected, i32 %desired) {
entry:
  %slot = inttoptr i64 0 to ptr addrspace(1)
  %compare = alloca i32, align 4
  store i32 %expected, ptr %compare, align 4
  %old = call i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1) %slot, ptr %compare, i32 %desired, i32 0, i32 0, i32 2, i1 true)
  ret i32 %old
}

declare i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1), ptr, i32, i32, i32, i32, i1)
"#;
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let atomic = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::AtomicCompareExchange)
        .expect("atomic cmpxchg");
    let ptr = match atomic.operands.first() {
        Some(Operand::IdRef(id)) => *id,
        other => panic!("unexpected atomic pointer operand: {other:?}"),
    };
    let ptr_def = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(ptr))
        .expect("atomic pointer definition");
    assert_eq!(ptr_def.class.opcode, Op::Variable);
    assert!(
        matches!(
            ptr_def.operands.first(),
            Some(Operand::StorageClass(StorageClass::Workgroup))
        ),
        "{ptr_def:?}"
    );
}

#[test]
fn native_device_atomic_umax_i32_lowers_through_kernel_transform() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Counter = type { i32 }

define void @k(ptr addrspace(1) %max_value) {
entry:
  %slot = getelementptr inbounds %struct.Counter, ptr addrspace(1) %max_value, i64 0, i32 0
  %old = tail call i32 @air.atomic.global.max.u.i32(ptr addrspace(1) %slot, i32 7, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.max.u.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"max_value"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_device_atomic_umax_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicUMax"), "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let atomic = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::AtomicUMax)
        .expect("atomic umax");
    let scope_id = match atomic.operands.get(1) {
        Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
        other => panic!("unexpected atomic scope operand: {other:?}"),
    };
    let scope_value = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
        .expect("scope constant");
    assert_eq!(scope_value, Scope::Device as u32, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_device_atomic_i32_variants_lower_through_kernel_transform() {
    let cases = [
        (
            "air.atomic.global.add.s.i32",
            Op::AtomicIAdd,
            "OpAtomicIAdd",
        ),
        ("air.atomic.global.and.u.i32", Op::AtomicAnd, "OpAtomicAnd"),
        ("air.atomic.global.or.u.i32", Op::AtomicOr, "OpAtomicOr"),
        ("air.atomic.global.xor.u.i32", Op::AtomicXor, "OpAtomicXor"),
        (
            "air.atomic.global.xchg.i32",
            Op::AtomicExchange,
            "OpAtomicExchange",
        ),
        (
            "air.atomic.global.max.s.i32",
            Op::AtomicSMax,
            "OpAtomicSMax",
        ),
        (
            "air.atomic.global.min.s.i32",
            Op::AtomicSMin,
            "OpAtomicSMin",
        ),
        (
            "air.atomic.global.min.u.i32",
            Op::AtomicUMin,
            "OpAtomicUMin",
        ),
        (
            "air.atomic.global.sub.s.i32",
            Op::AtomicISub,
            "OpAtomicISub",
        ),
        (
            "air.atomic.global.sub.u.i32",
            Op::AtomicISub,
            "OpAtomicISub",
        ),
    ];
    for (callee, opcode, opname) in cases {
        let ll = format!(
            r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Counter = type {{ i32 }}

define void @k(ptr addrspace(1) %value) {{
entry:
  %slot = getelementptr inbounds %struct.Counter, ptr addrspace(1) %value, i64 0, i32 0
  %old = tail call i32 @{callee}(ptr addrspace(1) %slot, i32 7, i32 0, i32 2, i1 true)
  ret void
}}

declare i32 @{callee}(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{{!0}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"int", !"air.arg_name", !"value"}}
"#
        );
        if opcode == Op::AtomicXor {
            let raw = emit_vulkan_spirv(&ll)
                .expect("native emitter recognizes AIR XOR before the kernel transform");
            assert!(load_bytes(&raw)
                .unwrap()
                .all_inst_iter()
                .any(|i| i.class.opcode == Op::AtomicXor));
        }
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_device_atomic_minmax_{}_{}",
            opname,
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp)
            .unwrap_or_else(|e| panic!("translate {callee}: {e}"));
        let asm = disassemble(&spv).expect("disassemble");
        assert!(asm.contains(opname), "{asm}");
        let module = load_bytes(&spv).expect("load native spv");
        let atomic = module
            .all_inst_iter()
            .find(|inst| inst.class.opcode == opcode)
            .unwrap_or_else(|| panic!("missing {opname}"));
        let scope_id = match atomic.operands.get(1) {
            Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
            other => panic!("unexpected atomic scope operand: {other:?}"),
        };
        let scope_value = module
            .all_inst_iter()
            .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
            .and_then(|inst| match inst.operands.first() {
                Some(Operand::LiteralBit32(value)) => Some(*value),
                _ => None,
            })
            .expect("scope constant");
        assert_eq!(scope_value, Scope::Device as u32, "{asm}");
        if std::process::Command::new("spirv-val")
            .arg("--version")
            .output()
            .is_ok()
        {
            tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
        }
    }
}

#[test]
fn native_device_atomic_i64_variants_lower_through_kernel_transform() {
    let cases = [
        (
            "air.atomic.global.max.u.i64",
            Op::AtomicUMax,
            "OpAtomicUMax",
        ),
        (
            "air.atomic.global.max.s.i64",
            Op::AtomicSMax,
            "OpAtomicSMax",
        ),
        (
            "air.atomic.global.min.u.i64",
            Op::AtomicUMin,
            "OpAtomicUMin",
        ),
        (
            "air.atomic.global.min.s.i64",
            Op::AtomicSMin,
            "OpAtomicSMin",
        ),
    ];
    for returns_void in [false, true] {
        for (callee, opcode, opname) in cases {
            assert_eq!(
                crate::air_intrinsics::integer_atomic_rmw(callee),
                Some((opcode, 64))
            );
            let mut ll = format!(
                r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Counter = type {{ i64 }}

define void @k(ptr addrspace(1) %value) {{
entry:
  %slot = getelementptr inbounds %struct.Counter, ptr addrspace(1) %value, i64 0, i32 0
  %old = tail call i64 @{callee}(ptr addrspace(1) %slot, i64 4294967303, i32 0, i32 2, i1 true)
  ret void
}}

declare i64 @{callee}(ptr addrspace(1), i64, i32, i32, i1)

!air.kernel = !{{!0}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"ulong", !"air.arg_name", !"value"}}
"#
            );
            if returns_void {
                ll = ll
                    .replace("%old = tail call i64", "tail call void")
                    .replace("declare i64", "declare void");
            }
            let tmp = std::env::temp_dir().join(format!(
                "metal2vulkan_device_atomic64_minmax_{}_{}_{}",
                opname,
                returns_void,
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&tmp);
            let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp)
                .unwrap_or_else(|e| panic!("translate {callee}: {e}"));
            let asm = disassemble(&spv).expect("disassemble");
            assert!(asm.contains(opname), "{asm}");
            assert!(asm.contains("OpCapability Int64Atomics"), "{asm}");
            let module = load_bytes(&spv).expect("load native spv");
            let atomic = module
                .all_inst_iter()
                .find(|inst| inst.class.opcode == opcode)
                .unwrap_or_else(|| panic!("missing {opname}"));
            let atomic_type = module
                .all_inst_iter()
                .find(|inst| inst.result_id == atomic.result_type)
                .expect("atomic result type");
            assert_eq!(atomic_type.class.opcode, Op::TypeInt);
            assert_eq!(
                atomic_type.operands.first(),
                Some(&Operand::LiteralBit32(64))
            );
            let scope_id = match atomic.operands.get(1) {
                Some(Operand::IdScope(id) | Operand::IdRef(id)) => *id,
                other => panic!("unexpected atomic scope operand: {other:?}"),
            };
            let scope_value = module
                .all_inst_iter()
                .find(|inst| inst.result_id == Some(scope_id) && inst.class.opcode == Op::Constant)
                .and_then(|inst| match inst.operands.first() {
                    Some(Operand::LiteralBit32(value)) => Some(*value),
                    _ => None,
                })
                .expect("scope constant");
            assert_eq!(scope_value, Scope::Device as u32, "{asm}");
            if std::process::Command::new("spirv-val")
                .arg("--version")
                .output()
                .is_ok()
            {
                tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
            }
        }
    }
}

#[test]
fn native_threadgroup_global_array_opaque_gep_uses_access_chain() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [400 x float] undef, align 4
@nested = internal addrspace(3) global [4 x [324 x float]] undef, align 4

define void @k(i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %p = getelementptr inbounds float, ptr addrspace(3) @shared, i64 %idx
  store float 1.000000e+00, ptr addrspace(3) %p, align 4
  %nested = getelementptr inbounds [324 x float], ptr addrspace(3) @nested, i64 0, i64 %idx
  store float 2.000000e+00, ptr addrspace(3) %nested, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_global_array_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_scalar_array_struct_view_folds_field_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.half6 = type { half, half, half, half, half, half }
@randomCoords = internal unnamed_addr addrspace(3) global [1024 x half] undef, align 8

define void @k(i32 %i, ptr addrspace(1) %out) {
entry:
  %idx = zext i32 %i to i64
  %slot0 = getelementptr inbounds %struct.half6, ptr addrspace(3) @randomCoords, i64 %idx, i32 0
  %v0 = load half, ptr addrspace(3) %slot0, align 2
  %slot5 = getelementptr inbounds %struct.half6, ptr addrspace(3) @randomCoords, i64 %idx, i32 5
  %v5 = load half, ptr addrspace(3) %slot5, align 2
  store half %v0, ptr addrspace(1) %out, align 2
  %out5 = getelementptr inbounds half, ptr addrspace(1) %out, i64 1
  store half %v5, ptr addrspace(1) %out5, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_scalar_array_struct_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let half = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeFloat && inst.operands == [Operand::LiteralBit32(16)]
        })
        .and_then(|inst| inst.result_id)
        .expect("half type");
    let half_ptr = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands
                    == [
                        Operand::StorageClass(StorageClass::Workgroup),
                        Operand::IdRef(half),
                    ]
        })
        .and_then(|inst| inst.result_id)
        .expect("workgroup half pointer");
    let workgroup_vars = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup)))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!workgroup_vars.is_empty(), "{asm}");
    let random_coord_chains = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain)
                && inst.result_type == Some(half_ptr)
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|base| workgroup_vars.contains(&base))
        })
        .collect::<Vec<_>>();
    assert!(!random_coord_chains.is_empty(), "{asm}");
    assert!(
        random_coord_chains
            .iter()
            .all(|inst| inst.operands.len() == 2),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_union_global_raw_array_gep_indexes_first_scalar_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
%union.histogram_t = type { [64 x %"struct.metal::_atomic"] }
@wholeFaceHistogram = internal addrspace(3) global %union.histogram_t undef, align 4

define void @k(i32 %i) {
entry:
  %idx64 = zext i32 %i to i64
  %raw = getelementptr inbounds [64 x i32], ptr addrspace(3) @wholeFaceHistogram, i64 0, i64 %idx64
  store i32 0, ptr addrspace(3) %raw, align 4
  %atomic = getelementptr inbounds %union.histogram_t, ptr addrspace(3) @wholeFaceHistogram, i64 0, i32 0, i64 %idx64, i32 0
  %old = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) %atomic, i32 1, i32 0, i32 1, i1 true)
  ret void
}

declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_union_raw_array_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_load_accepts_constant_gep_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@tg_values = internal unnamed_addr addrspace(3) global [128 x i32] undef, align 4

define void @k(ptr addrspace(1) %out) {
entry:
  %p = getelementptr inbounds [128 x i32], ptr addrspace(3) @tg_values, i64 0, i64 127
  store i32 7, ptr addrspace(3) %p, align 4
  %v = load i32, ptr addrspace(3) getelementptr inbounds ([128 x i32], ptr addrspace(3) @tg_values, i64 0, i64 127), align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_constant_gep_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Workgroup"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_vector_global_i32_gep_reinterprets_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [64 x <4 x i16>] undef, align 8

define void @k(i32 %i, ptr addrspace(1) %out) {
entry:
  %idx = zext i32 %i to i64
  %p = getelementptr inbounds i32, ptr addrspace(3) @shared, i64 %idx
  store i32 %i, ptr addrspace(3) %p, align 4
  %loaded = load i32, ptr addrspace(3) %p, align 4
  store i32 %loaded, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_vector_word_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(!asm.contains("OpVectorInsertDynamic"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_vector_bitcast_i64_store_reinterprets_whole_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [64 x <4 x i16>] undef, align 8

define void @main() {
entry:
  %slot = getelementptr inbounds <4 x i16>, ptr addrspace(3) @shared, i64 0
  %raw = bitcast ptr addrspace(3) %slot to ptr addrspace(3)
  store i64 81985529216486895, ptr addrspace(3) %raw, align 8
  %loaded = load i64, ptr addrspace(3) %raw, align 8
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_vector_i64_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let function_insts = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let stored_objects = function_insts
        .iter()
        .filter(|inst| inst.class.opcode == Op::Store)
        .filter_map(|inst| inst.operands.get(1).and_then(id_ref_operand))
        .collect::<HashSet<_>>();
    assert!(
        function_insts.iter().any(|inst| {
            inst.class.opcode == Op::Bitcast
                && inst
                    .result_id
                    .is_some_and(|id| stored_objects.contains(&id))
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_vector_bitcast_wide_store_splits_to_vector_slots() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [64 x <4 x half>] undef, align 8

define void @main() {
entry:
  %slot = getelementptr inbounds <4 x half>, ptr addrspace(3) @shared, i64 0
  %raw = bitcast ptr addrspace(3) %slot to ptr addrspace(3)
  store <4 x float> <float 1.000000e+00, float 2.000000e+00, float 3.000000e+00, float 4.000000e+00>, ptr addrspace(3) %raw, align 16
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_vector_wide_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let spv = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpCompositeConstruct").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpBitcast").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpPtrAccessChain").count(), 0, "{asm}");
    assert_eq!(asm.matches("OpInBoundsAccessChain").count(), 2, "{asm}");
    let null_ids: Vec<&str> = asm
        .lines()
        .filter(|line| line.contains("= OpConstantNull"))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    let body_stores = asm
        .lines()
        .filter(|line| line.trim_start().starts_with("OpStore"))
        .filter(|line| {
            !null_ids
                .iter()
                .any(|id| line.split_whitespace().any(|token| token == *id))
        })
        .count();
    assert_eq!(body_stores, 2, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_vector_param_gep_then_raw_i32_reinterprets_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [64 x <4 x i16>] undef, align 8

define void @k(i32 %i, ptr addrspace(1) %out) {
entry:
  tail call void @helper(i32 %i, ptr addrspace(3) @shared, ptr addrspace(1) %out)
  ret void
}

define internal void @helper(i32 %i, ptr addrspace(3) %scratch, ptr addrspace(1) %out) {
entry:
  %idx = zext i32 %i to i64
  %vecp = getelementptr inbounds <4 x i16>, ptr addrspace(3) %scratch, i64 %idx
  store <4 x i16> zeroinitializer, ptr addrspace(3) %vecp, align 8
  store <4 x i16> zeroinitializer, ptr addrspace(3) %scratch, align 8
  %base = load <4 x i16>, ptr addrspace(3) %scratch, align 8
  %raw = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  %wordp = getelementptr inbounds i32, ptr addrspace(3) %raw, i64 %idx
  store i32 %i, ptr addrspace(3) %wordp, align 4
  %loaded = load i32, ptr addrspace(3) %wordp, align 4
  store i32 %loaded, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_vector_param_word_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let native_spv = emit_vulkan_spirv(ll).expect("native emit");
    let native_asm = disassemble(&native_spv).expect("disassemble native");
    assert!(
        !native_asm.contains("OpVectorInsertDynamic"),
        "{native_asm}"
    );
    assert!(
        native_asm.contains("OpVectorExtractDynamic"),
        "{native_asm}"
    );
    assert!(
        !native_asm
            .lines()
            .any(|line| line.contains(" OpBitcast ") && line.contains("_ptr_")),
        "{native_asm}"
    );
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpVectorInsertDynamic"), "{asm}");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains(" OpBitcast ") && line.contains("_ptr_")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_i32_param_gep_uses_callsite_vector_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@shared = internal addrspace(3) global [64 x <4 x i16>] undef, align 8

define void @k(i32 %i, ptr addrspace(1) %out) {
entry:
  tail call void @prefix(i32 %i, ptr addrspace(3) @shared, ptr addrspace(1) %out)
  ret void
}

define internal void @prefix(i32 %i, ptr addrspace(3) %scratch, ptr addrspace(1) %out) {
entry:
  %idx = zext i32 %i to i64
  %wordp = getelementptr inbounds i32, ptr addrspace(3) %scratch, i64 %idx
  store i32 %i, ptr addrspace(3) %wordp, align 4
  %loaded = load i32, ptr addrspace(3) %wordp, align 4
  store i32 %loaded, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_i32_param_word_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(!asm.contains("OpVectorInsertDynamic"), "{asm}");
    assert!(
        !asm.contains("OpInBoundsAccessChain %_ptr_Workgroup_uint"),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threadgroup_mixed_half_uint_scratch_uses_raw_word_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %halfp = getelementptr inbounds half, ptr addrspace(3) %scratch, i64 0
  store half 0xH3C00, ptr addrspace(3) %halfp, align 2
  %raw = bitcast ptr addrspace(3) %halfp to ptr addrspace(3)
  %wordp = getelementptr inbounds i32, ptr addrspace(3) %raw, i64 %idx
  store i32 %i, ptr addrspace(3) %wordp, align 4
  %loaded = load i32, ptr addrspace(3) %wordp, align 4
  store i32 %loaded, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_mixed_half_uint_scratch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let uint = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
        })
        .and_then(|inst| inst.result_id)
        .expect("uint type");
    let workgroup_var = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup))
        })
        .and_then(|inst| inst.result_id)
        .expect("workgroup var");
    let workgroup_array = variable_pointee_type(&module, workgroup_var).expect("workgroup array");
    let array_def = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeArray && inst.result_id == Some(workgroup_array))
        .expect("workgroup array type");
    let [Operand::IdRef(elem_ty), Operand::IdRef(len_id)] = array_def.operands.as_slice() else {
        panic!("workgroup scratch should be a fixed array: {asm}");
    };
    assert_eq!(*elem_ty, uint, "{asm}");
    let len = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::Constant && inst.result_id == Some(*len_id))
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
        .expect("workgroup array length");
    assert_eq!(len, 2048, "{asm}");
    assert!(asm.contains("OpAtomicOr"), "{asm}");
    assert!(!asm.contains("_arr_uchar_uint_512"), "{asm}");
    assert!(
        !asm.contains("OpPtrAccessChain %_ptr_Workgroup_uint"),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_threadgroup_helper_call_uses_identity_bitcast_root() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out) {
entry:
  %alias = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  call void @helper(ptr addrspace(3) %alias, ptr addrspace(1) %out)
  ret void
}

define internal void @helper(ptr addrspace(3) %scratch, ptr addrspace(1) %out) {
entry:
  br label %body

body:
  %scalar = getelementptr inbounds float, ptr addrspace(3) %scratch, i64 0
  store float 1.0, ptr addrspace(3) %scalar, align 4
  %vectors = bitcast ptr addrspace(3) %scratch to ptr addrspace(3)
  %vector = getelementptr inbounds <4 x float>, ptr addrspace(3) %vectors, i64 0
  %loaded = load <4 x float>, ptr addrspace(3) %vector, align 16
  %first = extractelement <4 x float> %loaded, i64 0
  store float %first, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_threadgroup_helper_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains(" Workgroup"), "{asm}");
    assert!(!asm.contains(" Private"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_threadgroup_dynamic_gep_allows_negative_constant_rebase() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(3) %scratch, ptr addrspace(1) %out, i32 %i) {
entry:
  %ok = icmp uge i32 %i, 128
  br i1 %ok, label %body, label %exit

body:
  %idx = zext i32 %i to i64
  %halfp = getelementptr inbounds half, ptr addrspace(3) %scratch, i64 0
  store half 0xH3C00, ptr addrspace(3) %halfp, align 2
  %raw = bitcast ptr addrspace(3) %halfp to ptr addrspace(3)
  %advanced = getelementptr inbounds i32, ptr addrspace(3) %raw, i64 %idx
  %rebased = getelementptr inbounds i32, ptr addrspace(3) %advanced, i64 -128
  store i32 %i, ptr addrspace(3) %rebased, align 4
  %loaded = load i32, ptr addrspace(3) %rebased, align 4
  %as_float = bitcast i32 %loaded to float
  store float %as_float, ptr addrspace(1) %out, align 4
  br label %exit

exit:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"scratch"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_negative_raw_rebase_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    assert!(
        module.all_inst_iter().any(|inst| {
            inst.class.opcode == Op::Constant
                && inst.operands.last() == Some(&Operand::LiteralBit32(4294967168))
        }),
        "{asm}"
    );
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(asm.contains("Workgroup"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_pointer_bitcast_load_reinterprets_loaded_bits() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.S = type { float }
define i32 @load_bits(ptr addrspace(1) %p) {
entry:
  %field = getelementptr inbounds %struct.S, ptr addrspace(1) %p, i64 0, i32 0
  %bits_ptr = bitcast ptr addrspace(1) %field to ptr addrspace(1)
  %bits = load i32, ptr addrspace(1) %bits_ptr
  ret i32 %bits
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert_eq!(asm.matches("OpLoad").count(), 1, "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
}

#[test]
fn native_pointer_bitcast_load_reads_first_aggregate_word() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.TextureBufferClearParams = type { i32, %union.anon }
%union.anon = type { [4 x float] }
define void @k(ptr addrspace(1) %dst, ptr addrspace(2) %clear, i32 %tid) {
entry:
  %field = getelementptr inbounds %struct.TextureBufferClearParams, ptr addrspace(2) %clear, i64 0, i32 1
  %bits_ptr = bitcast ptr addrspace(2) %field to ptr addrspace(2)
  %bits = load i32, ptr addrspace(2) %bits_ptr, align 4
  %v0 = insertelement <4 x i32> undef, i32 %bits, i64 0
  %v1 = insertelement <4 x i32> %v0, i32 %bits, i64 1
  %v2 = insertelement <4 x i32> %v1, i32 %bits, i64 2
  %v3 = insertelement <4 x i32> %v2, i32 %bits, i64 3
  tail call void @air.write_texture_buffer_1d.u.v4i32(ptr addrspace(1) %dst, i32 %tid, <4 x i32> %v3, i32 2)
  ret void
}

declare void @air.write_texture_buffer_1d.u.v4i32(ptr addrspace(1), i32, <4 x i32>, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture_buffer<uint, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 20, !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 20, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"TextureBufferClearParams", !"air.arg_name", !"clear"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_aggregate_reinterpret_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_leading_vector_aggregate_load_bitcasts_to_int_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <3 x i32> @load_leading_vec() {
entry:
  %buf = alloca [4 x <3 x float>], align 16
  %v = load <3 x i32>, ptr %buf, align 16
  ret <3 x i32> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(!asm.contains("non-bitcastable"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_"), "{asm}");
}

#[test]
fn native_scalar_array_load_rebuilds_same_shape_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <3 x float> @load_array_as_vector() {
entry:
  %buf = alloca [3 x float], align 16
  %value = load <3 x float>, ptr %buf, align 16
  ret <3 x float> %value
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_"), "{asm}");
}

#[test]
fn native_scalar_array_load_bitcasts_each_same_width_vector_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <3 x i32> @load_array_as_reinterpreted_vector() {
entry:
  %buf = alloca [3 x float], align 16
  %value = load <3 x i32>, ptr %buf, align 16
  ret <3 x i32> %value
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert_eq!(asm.matches("OpBitcast").count(), 3, "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_"), "{asm}");
}

#[test]
fn native_vector_store_rebuilds_same_shape_scalar_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @store_vector_as_array(<3 x float> %value) {
entry:
  %buf = alloca [3 x float], align 16
  store <3 x float> %value, ptr %buf, align 16
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
}

#[test]
fn native_pointer_bitcast_load_packs_leading_float_fields_as_i64() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Keypoint = type { float, float, i32 }
define i64 @load_prefix_bits(ptr %p) {
entry:
  %field = getelementptr inbounds %struct.Keypoint, ptr %p, i64 0, i32 2
  %keep = load i32, ptr %field, align 4
  %alias = bitcast ptr %p to ptr
  %bits = load i64, ptr %alias, align 4
  ret i64 %bits
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    assert!(!asm.contains("non-bitcastable"), "{asm}");
    assert!(
        !asm.lines().any(|line| line.contains("OpCopyObject %_ptr_")),
        "{asm}"
    );
}

#[test]
fn native_pointer_alias_load_packs_wide_byte_vector_as_i64() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i64 @load_vector_bits(ptr %p, i64 %index) {
entry:
  %element = getelementptr inbounds [8 x <8 x i8>], ptr %p, i64 0, i64 %index
  %alias = bitcast ptr %element to ptr
  %bits = load i64, ptr %alias, align 8
  ret i64 %bits
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    assert!(!asm.contains("OpBitcast %ulong"), "{asm}");
    assert!(
        !asm.lines().any(|line| line.contains("OpCopyObject %_ptr_")),
        "{asm}"
    );
}

#[test]
fn native_pointer_bitcast_store_splits_i64_into_leading_float_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Keypoint = type { float, float, i32 }
define void @store_prefix_bits(ptr %p, i64 %bits) {
entry:
  %field = getelementptr inbounds %struct.Keypoint, ptr %p, i64 0, i32 2
  store i32 7, ptr %field, align 4
  %alias = bitcast ptr %p to ptr
  store i64 %bits, ptr %alias, align 4
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.matches("OpBitcast").count() >= 2, "{asm}");
    assert!(!asm.contains("reinterpret"), "{asm}");
}

#[test]
fn native_pointer_bitcast_load_reads_first_pointer_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Cache = type { ptr addrspace(1), float }
define i32 @load_first_pointer_field(ptr addrspace(1) %buf) {
entry:
  %cache = alloca %struct.Cache, align 8
  %field = getelementptr inbounds %struct.Cache, ptr %cache, i64 0, i32 0
  store ptr addrspace(1) %buf, ptr %field, align 8
  %bits_ptr = bitcast ptr %cache to ptr
  %loaded = load ptr addrspace(1), ptr %bits_ptr, align 8
  %word_ptr = getelementptr inbounds i32, ptr addrspace(1) %loaded, i64 0
  %word = load i32, ptr addrspace(1) %word_ptr, align 4
  ret i32 %word
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("non-bitcastable"), "{asm}");
}

#[test]
fn native_local_aggregate_byte_view_uses_array_storage_for_dynamic_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%union.ETCBlock = type { %struct.Bits }
%struct.Bits = type { i64 }
define void @k(ptr addrspace(1) %out, i32 %idx) {
entry:
  %block = alloca %union.ETCBlock, align 8
  %alias = bitcast ptr %block to ptr
  %wide = zext i32 %idx to i64
  %byte = getelementptr inbounds [8 x i8], ptr %alias, i64 0, i64 %wide
  store i8 7, ptr %byte, align 1
  %field = getelementptr inbounds %union.ETCBlock, ptr %block, i64 0, i32 0, i32 0
  %bits = load i64, ptr %field, align 8
  %dst = getelementptr inbounds i64, ptr addrspace(1) %out, i64 0
  store i64 %bits, ptr addrspace(1) %dst, align 8
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_local_aggregate_byte_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("k"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_local_aggregate_multi_view_remodels_to_byte_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%union.ETCBlock = type { %struct.anon.9 }
%struct.anon.9 = type { i64 }
%struct.anon = type { %union.anon, %union.anon }
%union.anon = type { %struct.anon.0 }
%struct.anon.0 = type { i32 }
define void @k(ptr addrspace(1) %out, i32 %idx) {
entry:
  %block = alloca %union.ETCBlock, align 4
  %alias = bitcast ptr %block to ptr
  %wide = zext i32 %idx to i64
  %byte = getelementptr inbounds [8 x i8], ptr %alias, i64 0, i64 %wide
  store i8 7, ptr %byte, align 1
  %view = bitcast ptr %block to ptr
  %hi = getelementptr inbounds %struct.anon, ptr %view, i64 0, i32 1, i32 0, i32 0
  %word = load i32, ptr %hi, align 4
  %field = getelementptr %union.ETCBlock, ptr %block, i64 0, i32 0, i32 0
  %bits = load i64, ptr %field, align 4
  %low = trunc i64 %bits to i32
  %sum = add i32 %word, %low
  store i32 %sum, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_local_aggregate_multi_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_local_scalar_byte_view_packs_half_lanes_in_byte_storage() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {
entry:
  %slot = alloca float, align 4
  %low_alias = bitcast ptr %slot to ptr
  store half 0xH3C00, ptr %low_alias, align 2
  %high_alias = bitcast ptr %slot to ptr
  %high = getelementptr inbounds i8, ptr %high_alias, i64 2
  store half 0xH4000, ptr %high, align 2
  %packed = load float, ptr %slot, align 4
  store float %packed, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_local_scalar_byte_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.contains("OpPtrAccessChain %_ptr_Function_uchar"),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_global_byte_table_reinterpret_view_remodels_to_byte_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@tbl = internal unnamed_addr addrspace(2) constant <{ [4 x i8], <{ i8, i8, [2 x i8] }> }> <{ [4 x i8] c"", <{ i8, i8, [2 x i8] }> <{ i8 5, i8 6, [2 x i8] zeroinitializer }> }>, align 1
define void @k(ptr addrspace(1) %out, i32 %r, i32 %c) {
entry:
  %rw = zext i32 %r to i64
  %cw = zext i32 %c to i64
  %p = getelementptr inbounds [2 x [4 x i8]], ptr addrspace(2) @tbl, i64 0, i64 %rw, i64 %cw
  %b = load i8, ptr addrspace(2) %p, align 1
  %w = zext i8 %b to i32
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_global_byte_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_local_aggregate_callee_view_valid_after_inline_sroa() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%union.ETCBlock = type { %struct.anon.9 }
%struct.anon.9 = type { i64 }
%struct.anon = type { %union.anon, %union.anon }
%union.anon = type { %struct.anon.0 }
%struct.anon.0 = type { i32 }
define void @k(ptr addrspace(1) %out, i32 %idx) {
  %block = alloca %union.ETCBlock, align 4
  %alias = bitcast ptr %block to ptr
  %wide = zext i32 %idx to i64
  %byte = getelementptr inbounds [8 x i8], ptr %alias, i64 0, i64 %wide
  store i8 7, ptr %byte, align 1
  %word = call fastcc i32 @helper(ptr noundef nonnull %block)
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}
define internal fastcc i32 @helper(ptr noundef %0) {
  %view = bitcast ptr %0 to ptr
  %hi = getelementptr inbounds %struct.anon, ptr %view, i64 0, i32 1, i32 0, i32 0
  %word = load i32, ptr %hi, align 4
  ret i32 %word
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_local_aggregate_callee_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_function_aggregate_pointer_field_store_load_replays_pointer_value() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Cache = type { ptr addrspace(1), ptr addrspace(1), float }
define void @store_load_pointer_field(ptr addrspace(1) %buf) {
entry:
  %cache = alloca %struct.Cache, align 8
  %field = getelementptr inbounds %struct.Cache, ptr %cache, i64 0, i32 0
  store ptr addrspace(1) %buf, ptr %field, align 8
  %loaded = load ptr addrspace(1), ptr %field, align 8
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpCopyObject"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
}

#[test]
fn native_by_value_aggregate_carries_pointer_outside_serialized_struct() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Pair = type { ptr addrspace(1), i64 }
define void @carry_pointer(ptr addrspace(1) %buf, ptr addrspace(1) %out) {
entry:
  %with.ptr = insertvalue %struct.Pair poison, ptr addrspace(1) %buf, 0
  %pair = insertvalue %struct.Pair %with.ptr, i64 7, 1
  %replayed = extractvalue %struct.Pair %pair, 0
  store i32 9, ptr addrspace(1) %replayed, align 4
  %payload = extractvalue %struct.Pair %pair, 1
  store i64 %payload, ptr addrspace(1) %out, align 8
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load spv");
    let type_defs = module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction)))
        .collect::<std::collections::HashMap<_, _>>();
    let pointer_types = type_defs
        .iter()
        .filter_map(|(id, instruction)| {
            (instruction.class.opcode == Op::TypePointer).then_some(*id)
        })
        .collect::<std::collections::HashSet<_>>();

    assert!(module.types_global_values.iter().all(|instruction| {
        instruction.class.opcode != Op::TypeStruct
            || instruction
                .operands
                .iter()
                .all(|operand| !matches!(operand, Operand::IdRef(id) if pointer_types.contains(id)))
    }));
    assert!(module
        .functions
        .iter()
        .flat_map(|function| function.all_inst_iter())
        .any(|instruction| instruction.class.opcode == Op::Store
            && matches!(instruction.operands.get(1), Some(Operand::IdRef(_)))));
}

#[test]
fn native_local_pointer_field_load_preserves_stored_pointer_storage() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Cache = type { ptr }
define i8 @load_local_pointer_field() {
entry:
  %slot = alloca i8, align 1
  %cache = alloca %struct.Cache, align 8
  store i8 7, ptr %slot, align 1
  %field = getelementptr inbounds %struct.Cache, ptr %cache, i64 0, i32 0
  store ptr %slot, ptr %field, align 8
  %loaded = load ptr, ptr %field, align 8
  %byte = load i8, ptr %loaded, align 1
  ret i8 %byte
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let copy = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::CopyObject)
        .expect("copy object");
    let source = match copy.operands.first() {
        Some(Operand::IdRef(id)) => *id,
        other => panic!("unexpected copy source {other:?}"),
    };
    let source_type = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(source))
        .and_then(|inst| inst.result_type)
        .expect("source type");
    assert_eq!(copy.result_type, Some(source_type), "{asm}");
}

#[test]
fn native_null_local_pointer_field_uses_integer_payload() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Cache = type { ptr, i64 }
define i1 @null_local_pointer_field() {
entry:
  %cache = alloca %struct.Cache, align 8
  %field = getelementptr inbounds %struct.Cache, ptr %cache, i64 0, i32 0
  store ptr null, ptr %field, align 8
  %loaded = load ptr, ptr %field, align 8
  %is_null = icmp eq ptr %loaded, null
  ret i1 %is_null
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let module = load_bytes(&spv).expect("load spv");
    let invalid_pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::TypePointer
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(
                        StorageClass::Private | StorageClass::Function
                    ))
                ))
            .then_some(inst.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!module.types_global_values.iter().any(|inst| {
        inst.class.opcode == Op::ConstantNull
            && inst
                .result_type
                .is_some_and(|ty| invalid_pointer_types.contains(&ty))
    }));
}

#[test]
fn native_concrete_pointer_round_trips_through_opaque_by_value_wrapper() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Wrapper = type { ptr, i64 }
define void @k(ptr addrspace(1) %out) {
entry:
  %slot = alloca i64, align 8
  store i64 7, ptr %slot, align 8
  %with_pointer = insertvalue %Wrapper poison, ptr %slot, 0
  %complete = insertvalue %Wrapper %with_pointer, i64 1, 1
  %loaded_pointer = extractvalue %Wrapper %complete, 0
  %value = load i64, ptr %loaded_pointer, align 8
  %truncated = trunc i64 %value to i32
  store i32 %truncated, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_opaque_pointer_wrapper_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("opaque by-value wrapper must preserve its concrete pointer");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_inlined_byte_struct_view_extracts_from_same_size_scalar_alloca() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Flags = type { i8, i8, i8, i8, i8, i8, i8, i8 }
define void @k(ptr addrspace(1) %out) {
entry:
  %bits = alloca i64, align 8
  %alias = bitcast ptr %bits to ptr
  store i64 257, ptr %bits, align 8
  %flag = call fastcc i8 @read_flag(ptr %alias)
  %word = zext i8 %flag to i32
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

define internal fastcc i8 @read_flag(ptr %flags) {
entry:
  %field = getelementptr inbounds %Flags, ptr %flags, i64 0, i32 1
  %value = load i8, ptr %field, align 1
  %set = icmp ne i8 %value, 0
  br i1 %set, label %present, label %absent
present:
  ret i8 %value
absent:
  ret i8 0
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_scalar_alloca_byte_struct_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_function_pointer_array_dynamic_load_selects_stored_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @store_via_pointer_table(ptr addrspace(1) %a, ptr addrspace(1) %b, i32 %idx) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot0, align 8
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot1, align 8
  %wide = zext i32 %idx to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %wide
  %dst = load ptr addrspace(1), ptr %slot, align 8
  %value = getelementptr inbounds float, ptr addrspace(1) %dst, i64 0
  store float 1.000000e+00, ptr addrspace(1) %value, align 4
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(!asm.contains("non-bitcastable result Ptr"), "{asm}");
}

#[test]
fn native_function_pointer_matrix_dynamic_load_selects_stored_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @store_via_pointer_matrix(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, ptr addrspace(1) %d, i32 %row, i32 %column) {
entry:
  %table = alloca [2 x [2 x ptr addrspace(1)]], align 8
  %slot00 = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot00, align 8
  %slot01 = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot01, align 8
  %slot10 = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 1, i64 0
  store ptr addrspace(1) %c, ptr %slot10, align 8
  %slot11 = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 1, i64 1
  store ptr addrspace(1) %d, ptr %slot11, align 8
  %wide_row = zext i32 %row to i64
  %wide_column = zext i32 %column to i64
  %slot = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 %wide_row, i64 %wide_column
  %dst = load ptr addrspace(1), ptr %slot, align 8
  store float 1.000000e+00, ptr addrspace(1) %dst, align 4
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLogicalAnd"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("non-bitcastable result Ptr"), "{asm}");
}

#[test]
fn native_bound_buffer_pointer_array_preserves_dynamic_table_sources() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @load_via_pointer_table(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %idx) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot0, align 8
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot1, align 8
  %wide = zext i32 %idx to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %wide
  %src = load ptr addrspace(1), ptr %slot, align 8
  %value = load i8, ptr addrspace(1) %src, align 1
  store i8 %value, ptr addrspace(1) %out, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @load_via_pointer_table, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 1, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 1, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 1, !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bound_pointer_table_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_primary_validated(ll, Stage::Kernel, &tmp)
        .expect("bound pointer table primary must validate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_pointer_bitcast_widening_vector_load_pads_extra_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @load_wide_row() {
entry:
  %matrix = alloca [1 x <3 x float>], align 16
  %row = getelementptr inbounds [1 x <3 x float>], ptr %matrix, i64 0, i64 0
  store <3 x float> <float 1.000000e+00, float 2.000000e+00, float 3.000000e+00>, ptr %row, align 16
  %alias = bitcast ptr %row to ptr
  %wide = load <4 x float>, ptr %alias, align 16
  %lane = extractelement <4 x float> %wide, i32 2
  ret float %lane
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
}

#[test]
fn native_pointer_bitcast_vector_load_store_uses_scalar_lanes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @half4_alias() {
entry:
  %buf = alloca [16 x half], align 16
  %src = getelementptr inbounds [16 x half], ptr %buf, i64 0, i64 0
  %src_alias = bitcast ptr %src to ptr
  %v = load <4 x half>, ptr %src_alias, align 8
  %dst = getelementptr inbounds [16 x half], ptr %buf, i64 0, i64 4
  %dst_alias = bitcast ptr %dst to ptr
  store <4 x half> %v, ptr %dst_alias, align 8
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_half4_scalar_lanes_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("half4_alias"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(
        !asm.contains("reinterpret load bit width mismatch"),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_ptr_network_widen_whole_vs_part_emits_valid_primary() {
    let ll = r#"
source_filename = "case.metal"
define void @wvp(ptr addrspace(1) noundef align 4 "air-buffer-no-alias" %0) local_unnamed_addr #0 {
  %v = load <4 x float>, ptr addrspace(1) %0, align 16
  %p = getelementptr inbounds float, ptr addrspace(1) %0, i64 4
  %s = load float, ptr addrspace(1) %p, align 4
  %e = extractelement <4 x float> %v, i32 0
  %t = fadd float %e, %s
  %q = getelementptr inbounds float, ptr addrspace(1) %0, i64 8
  store float %t, ptr addrspace(1) %q, align 4
  ret void
}
attributes #0 = { nounwind }
!air.kernel = !{!0}
!0 = !{ptr @wvp, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 0, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"buf"}
"#;
    let widened = super::super::vec_scalar_merge::lower_with_widen_for_test(ll);
    assert!(
        !widened.contains("load <4 x float>"),
        "whole-vs-part load not scalarized:\n{widened}"
    );

    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_ptr_network_widen_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_native_no_retry(&widened, Stage::Kernel).expect("primary emit");
    let asm = disassemble(&out).expect("disassemble");
    if std::env::var("DBG_ASM").is_ok() {
        eprintln!("{asm}");
    }
    assert!(
        asm.matches("OpCompositeInsert").count() >= 4,
        "vector not rebuilt from scalar lanes:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_same_width_vector_reinterpret_store_bitcasts_object() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @v2f_into_v4h(<2 x float> %v) {
entry:
  %buf = alloca <4 x half>, align 8
  store <2 x float> %v, ptr %buf, align 8
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(!asm.contains("does not match Object"), "{asm}");
}

#[test]
fn a_subword_vector_stores_back_through_the_word_pointer_it_loaded_from() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @subword_roundtrip(ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %in, ptr addrspace(1) noundef captures(none) "air-buffer-no-alias" %out, i32 %gid) {
entry:
  %i = zext i32 %gid to i64
  %pin = getelementptr inbounds i32, ptr addrspace(1) %in, i64 %i
  %v = load <2 x i16>, ptr addrspace(1) %pin, align 4
  %w = shufflevector <2 x i16> %v, <2 x i16> poison, <2 x i32> <i32 1, i32 0>
  %pout = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %i
  store <2 x i16> %w, ptr addrspace(1) %pout, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @subword_roundtrip, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"in"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let bitcasts = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::Bitcast)
        .count();
    assert_eq!(bitcasts, 2, "{asm}");
    assert!(!asm.contains("OpCompositeConstruct"), "{asm}");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_subword_roundtrip_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn a_vector_stores_into_a_same_width_scalar_array_view() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @vec_into_word_array(<4 x float> %v) {
entry:
  %a = alloca [4 x i32], align 16
  store <4 x float> %v, ptr %a, align 16
  %p = getelementptr inbounds [4 x i32], ptr %a, i64 0, i64 2
  %w = load i32, ptr %p, align 4
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert_eq!(asm.matches("OpBitcast").count(), 4, "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("violates its pointer-pointee"), "{asm}");
}

#[test]
fn a_vector_loads_from_same_width_scalar_slots() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @word_vector_from_float_slots(float %f) {
entry:
  %b = alloca [4 x float], align 16
  %s0 = getelementptr inbounds [4 x float], ptr %b, i64 0, i64 0
  store float %f, ptr %s0, align 16
  %v = load <4 x i32>, ptr %s0, align 16
  %e = extractelement <4 x i32> %v, i32 2
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert_eq!(asm.matches("OpBitcast").count(), 4, "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(
        !asm.contains("reinterpret load bit width mismatch"),
        "{asm}"
    );
}

#[test]
fn a_leading_vector_member_loads_at_a_different_lane_count() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.HalfLead = type { <4 x half>, i32 }

define void @half4_head_as_float2(<4 x half> %h) {
entry:
  %s = alloca %struct.HalfLead, align 16
  %hp = getelementptr inbounds %struct.HalfLead, ptr %s, i64 0, i32 0
  store <4 x half> %h, ptr %hp, align 8
  %f = load <2 x float>, ptr %s, align 8
  %e = extractelement <2 x float> %f, i32 1
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(!asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(!asm.contains("non-bitcastable pointee"), "{asm}");
}

#[test]
fn native_pointer_bitcast_i32_word_load_constructs_i16_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%union.Everything = type { <4 x i32> }

define <4 x i16> @load_half_words() {
entry:
  %scratch = alloca %union.Everything, align 16
  %word2 = getelementptr inbounds %union.Everything, ptr %scratch, i64 0, i32 0, i64 2
  store i32 131073, ptr %word2, align 4
  %word3 = getelementptr inbounds %union.Everything, ptr %scratch, i64 0, i32 0, i64 3
  store i32 262147, ptr %word3, align 4
  %alias = bitcast ptr %word2 to ptr
  %v = load <4 x i16>, ptr %alias, align 8
  ret <4 x i16> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(
        !asm.contains("reinterpret load bit width mismatch"),
        "{asm}"
    );
}

#[test]
fn native_pointer_bitcast_narrowing_vector_store_drops_extra_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::matrix" = type { [3 x <3 x float>] }
define void @main() {
entry:
  %v0 = insertelement <4 x float> poison, float 1.000000e+00, i32 0
  %v1 = insertelement <4 x float> %v0, float 2.000000e+00, i32 1
  %v2 = insertelement <4 x float> %v1, float 3.000000e+00, i32 2
  %v3 = insertelement <4 x float> %v2, float 4.000000e+00, i32 3
  %matrix = alloca [1 x %"struct.metal::matrix"], align 16
  %slot = getelementptr inbounds [1 x %"struct.metal::matrix"], ptr %matrix, i64 0, i64 0
  %row0 = bitcast ptr %slot to ptr
  %row1 = getelementptr inbounds [1 x %"struct.metal::matrix"], ptr %matrix, i64 0, i64 0, i32 0, i64 1
  %row1_alias = bitcast ptr %row1 to ptr
  store <4 x float> %v3, ptr %row0, align 16
  store <4 x float> %v3, ptr %row1_alias, align 16
  ret void
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_narrowing_vector_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpVectorShuffle").count(), 2, "{asm}");
    assert!(asm.contains(" 0 1 2"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_i64_load_store_reinterprets_i32_pair_struct() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Pair = type { i32, i32 }
define void @copy_pair(ptr addrspace(1) %dst, ptr addrspace(1) %src, i64 %idx) {
entry:
  %src_record = getelementptr inbounds %struct.Pair, ptr addrspace(1) %src, i64 %idx
  %src_bits = bitcast ptr addrspace(1) %src_record to ptr addrspace(1)
  %bits = load i64, ptr addrspace(1) %src_bits, align 4
  %dst_record = getelementptr inbounds %struct.Pair, ptr addrspace(1) %dst, i64 %idx
  %dst_bits = bitcast ptr addrspace(1) %dst_record to ptr addrspace(1)
  store i64 %bits, ptr addrspace(1) %dst_bits, align 4
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
}

#[test]
fn native_loaded_pointer_can_feed_gep() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.S = type { ptr addrspace(1) }
define i32 @load_loaded_pointer(ptr addrspace(2) %p, i64 %idx) {
entry:
  %field = getelementptr inbounds %struct.S, ptr addrspace(2) %p, i64 0, i32 0
  %base = load ptr addrspace(1), ptr addrspace(2) %field
  %elt = getelementptr inbounds i32, ptr addrspace(1) %base, i64 %idx
  %v = load i32, ptr addrspace(1) %elt
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
}

#[test]
fn native_i8_pointer_can_load_wider_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @load_from_byte_pointer(ptr addrspace(2) %p, i64 %idx) {
entry:
  %word = shl i64 %idx, 2
  %byte = getelementptr inbounds i8, ptr addrspace(2) %p, i64 %word
  %alias = bitcast ptr addrspace(2) %byte to ptr addrspace(2)
  %v = load i32, ptr addrspace(2) %alias
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains(" OpBitcast ") && line.contains("_ptr_")),
        "{asm}"
    );
}

#[test]
fn native_i8_buffer_can_store_wider_depth_stencil_words() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"

define void @CopyD32S8ToBuffer(<2 x i32> %tid, ptr addrspace(1) %depth, ptr addrspace(1) %stencil, ptr addrspace(1) %output, ptr addrspace(2) %rowPitch) {
entry:
  %depthSample = tail call { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1) %depth, <2 x i32> %tid, i32 0, i32 1)
  %depthVec = extractvalue { <4 x float>, i8 } %depthSample, 0
  %depthValue = extractelement <4 x float> %depthVec, i64 0
  %stencilSample = tail call { <4 x i32>, i8 } @air.read_texture_2d.u.v4i32(ptr addrspace(1) %stencil, <2 x i32> %tid, i32 0, i32 1)
  %stencilVec = extractvalue { <4 x i32>, i8 } %stencilSample, 0
  %stencilValue = extractelement <4 x i32> %stencilVec, i64 0
  %pitch = load i32, ptr addrspace(2) %rowPitch, align 4
  %y = extractelement <2 x i32> %tid, i64 1
  %rowBytes32 = mul i32 %pitch, %y
  %rowBytes = zext i32 %rowBytes32 to i64
  %row = getelementptr inbounds i8, ptr addrspace(1) %output, i64 %rowBytes
  %x = extractelement <2 x i32> %tid, i64 0
  %xBytes32 = shl i32 %x, 3
  %xBytes = zext i32 %xBytes32 to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %row, i64 %xBytes
  %depthOut = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  store float %depthValue, ptr addrspace(1) %depthOut, align 4
  %stencilSlot = getelementptr inbounds i8, ptr addrspace(1) %slot, i64 4
  %stencilOut = bitcast ptr addrspace(1) %stencilSlot to ptr addrspace(1)
  store i32 %stencilValue, ptr addrspace(1) %stencilOut, align 4
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, i32, i32)
declare { <4 x i32>, i8 } @air.read_texture_2d.u.v4i32(ptr addrspace(1), <2 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @CopyD32S8ToBuffer, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"threadID"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"depthView"}
!5 = !{i32 2, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<uint, read>", !"air.arg_name", !"stencilView"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"output"}
!7 = !{i32 4, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"rowAndPlanePitch"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_i8_buffer_raw_stores_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("OpUDiv"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_i8_buffer_bitcast_float_load_uses_raw_offset() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @ByteFloatLoad(ptr addrspace(1) %bytes, ptr addrspace(1) %out, i32 %tid) {
entry:
  %byte32 = shl i32 %tid, 2
  %byte = zext i32 %byte32 to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %byte
  %floatSlot = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  %v = load float, ptr addrspace(1) %floatSlot, align 4
  %dst = getelementptr inbounds float, ptr addrspace(1) %out, i32 %tid
  store float %v, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @ByteFloatLoad, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"bytes"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_i8_buffer_bitcast_float_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    let binding0_count = asm
        .lines()
        .filter(|line| line.contains("OpDecorate") && line.contains(" Binding 0"))
        .count();
    assert_eq!(binding0_count, 1, "{asm}");
    assert!(
        asm.lines().any(|line| line.contains(" OpBitcast ")),
        "{asm}"
    );
    assert!(
        !asm.lines()
            .any(|line| line.contains(" OpBitcast ") && line.contains("_ptr_")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_struct_array_dynamic_member_index_preserves_offset() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%Counters = type <{ i32, [16 x i32] }>

define void @ParticleLike(i32 %idx, ptr addrspace(2) %header, ptr addrspace(1) %data, ptr addrspace(1) %out) {
entry:
  %base32 = load i32, ptr addrspace(2) %header, align 4
  %base64 = sext i32 %base32 to i64
  %base = getelementptr inbounds i8, ptr addrspace(1) %data, i64 %base64
  %typed = bitcast ptr addrspace(1) %base to ptr addrspace(1)
  %idx64 = zext i32 %idx to i64
  %slot = getelementptr inbounds %Counters, ptr addrspace(1) %typed, i64 0, i32 1, i64 %idx64
  %v = load i32, ptr addrspace(1) %slot, align 4
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %idx64
  store i32 %v, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @ParticleLike, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"header"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"data"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_struct_array_dynamic_index_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(!asm.contains("raw buffer offset is not modelable"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_i8_load_uses_dynamic_byte_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @ByteLane(i32 %tid, ptr addrspace(1) %bytes, ptr addrspace(1) %out) {
entry:
  %wide_i32 = shl i32 %tid, 2
  %wide_i64 = zext i32 %wide_i32 to i64
  %wide_byte = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %wide_i64
  %wide_ptr = bitcast ptr addrspace(1) %wide_byte to ptr addrspace(1)
  %wide = load <4 x i8>, ptr addrspace(1) %wide_ptr, align 4
  %wide0 = extractelement <4 x i8> %wide, i64 0
  %idx = zext i32 %tid to i64
  %byte_ptr = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %idx
  %byte = load i8, ptr addrspace(1) %byte_ptr, align 1
  %byte32 = zext i8 %byte to i32
  %wide32 = zext i8 %wide0 to i32
  %sum = add i32 %byte32, %wide32
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %idx
  store i32 %sum, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @ByteLane, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"bytes"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_i8_dynamic_lane_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(asm.contains("OpUDiv"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_i8_vector_source_gep_load_uses_scalar_byte_loads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @ByteVectorGep(i32 %tid, ptr addrspace(1) %bytes, ptr addrspace(1) %out) {
entry:
  %lane = zext i32 %tid to i64
  %base_lane = getelementptr inbounds <4 x i8>, ptr addrspace(1) %bytes, i64 0, i64 %lane
  %base_bytes = bitcast ptr addrspace(1) %base_lane to ptr addrspace(1)
  %quad_i32 = shl i32 %tid, 2
  %quad = zext i32 %quad_i32 to i64
  %quad_ptr = getelementptr inbounds <4 x i8>, ptr addrspace(1) %base_bytes, i64 %quad
  %wide = load <4 x i8>, ptr addrspace(1) %quad_ptr, align 4
  %wide0 = extractelement <4 x i8> %wide, i64 0
  %idx = zext i32 %tid to i64
  %dst = getelementptr inbounds i8, ptr addrspace(1) %out, i64 %idx
  store i8 %wide0, ptr addrspace(1) %dst, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @ByteVectorGep, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uchar4", !"air.arg_name", !"bytes"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_i8_vector_source_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(
        !asm.lines().any(|line| line.contains("OpLoad %v4uchar")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_pointer_load_from_dynamic_byte_offset_uses_placeholder() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @PointerLoad(i32 %idx, ptr addrspace(1) %bytes, ptr addrspace(1) %out) {
entry:
  %idx64 = zext i32 %idx to i64
  %slot = getelementptr inbounds i8, ptr addrspace(1) %bytes, i64 %idx64
  %ptr_slot = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  %loaded = load ptr addrspace(1), ptr addrspace(1) %ptr_slot, align 1
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %idx64
  store i32 0, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @PointerLoad, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"bytes"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_pointer_load_dynamic_byte_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpVariable"), "{asm}");
    assert!(!asm.contains("raw dynamic byte stride"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_scalar_array_reinterpret_gep_composes_byte_offsets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @DecodePackedRGB8(i32 %x, ptr addrspace(1) %src, ptr addrspace(2) %stride, ptr addrspace(1) %out) {
entry:
  %row_stride = load i32, ptr addrspace(2) %stride, align 4
  %row_bytes = mul i32 %row_stride, %x
  %row64 = zext i32 %row_bytes to i64
  %row_ptr = getelementptr inbounds [3 x i8], ptr addrspace(1) %src, i64 0, i64 %row64
  %typed = bitcast ptr addrspace(1) %row_ptr to ptr addrspace(1)
  %x64 = zext i32 %x to i64
  %rptr = getelementptr inbounds [3 x i8], ptr addrspace(1) %typed, i64 %x64, i64 0
  %gptr = getelementptr inbounds [3 x i8], ptr addrspace(1) %typed, i64 %x64, i64 1
  %bptr = getelementptr inbounds [3 x i8], ptr addrspace(1) %typed, i64 %x64, i64 2
  %r = load i8, ptr addrspace(1) %rptr, align 1
  %g = load i8, ptr addrspace(1) %gptr, align 1
  %b = load i8, ptr addrspace(1) %bptr, align 1
  %r32 = zext i8 %r to i32
  %g32 = zext i8 %g to i32
  %b32 = zext i8 %b to i32
  %rg = add i32 %r32, %g32
  %rgb = add i32 %rg, %b32
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %x64
  store i32 %rgb, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @DecodePackedRGB8, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"x"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 3, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"packed_uchar3", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"stride"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_scalar_array_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpPtrAccessChain") && line.contains("_ptr_StorageBuffer_uchar")
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_local_pointer_table_gep_infers_buffer_param_pointees() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, i32 %which, i64 %offset, float %v) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot0, align 8
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot1, align 8
  %idx = zext i32 %which to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %idx
  %base = load ptr addrspace(1), ptr %slot, align 8
  %ptr = getelementptr inbounds float, ptr addrspace(1) %base, i64 %offset
  store float %v, ptr addrspace(1) %ptr, align 4
  ret void
}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    assert_eq!(
        ir.ptr_pointees.get(&("k".to_string(), "%a".to_string())),
        Some(&LlType::Float)
    );
    assert_eq!(
        ir.ptr_pointees.get(&("k".to_string(), "%b".to_string())),
        Some(&LlType::Float)
    );
}

#[test]
fn native_local_pointer_table_dynamic_gep_selects_per_arm_access_chains() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, i32 %which, i64 %offset, float %v) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot0, align 8
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot1, align 8
  %idx = zext i32 %which to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %idx
  %base = load ptr addrspace(1), ptr %slot, align 8
  %ptr = getelementptr inbounds float, ptr addrspace(1) %base, i64 %offset
  store float %v, ptr addrspace(1) %ptr, align 4
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(asm.contains("OpAccessChain"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
}

#[test]
fn native_local_pointer_table_vector_gep_keeps_vector_stride() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %which) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %a0 = getelementptr inbounds <4 x float>, ptr addrspace(1) %a, i64 0
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a0, ptr %slot0, align 8
  %b0 = getelementptr inbounds <4 x float>, ptr addrspace(1) %b, i64 0
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b0, ptr %slot1, align 8
  %idx = zext i32 %which to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %idx
  %base = load ptr addrspace(1), ptr %slot, align 8
  %ptr = getelementptr inbounds <4 x float>, ptr addrspace(1) %base, i64 %idx
  %value = load <4 x float>, ptr addrspace(1) %ptr, align 16
  %lane = extractelement <4 x float> %value, i32 0
  store float %lane, ptr addrspace(1) %out, align 4
  ret void
}
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"which"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_local_pointer_table_vector_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_vector_store_reinterprets_same_width_scalar_lanes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @store_half2_lanes(ptr addrspace(3) %scratch, i32 %idx, <4 x float> %packed) {
entry:
  %wide = zext i32 %idx to i64
  %slot = getelementptr inbounds <2 x half>, ptr addrspace(3) %scratch, i64 %wide
  %cast = bitcast ptr addrspace(3) %slot to ptr addrspace(3)
  store <4 x float> %packed, ptr addrspace(3) %cast, align 16
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
}

#[test]
fn native_scalar_store_reinterprets_same_width_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %buf, i16 %tid) {
entry:
  %idx = zext i16 %tid to i64
  %slot = getelementptr inbounds i32, ptr addrspace(1) %buf, i64 %idx
  %cast = bitcast ptr addrspace(1) %slot to ptr addrspace(1)
  store float 1.000000e+00, ptr addrspace(1) %cast, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"buf"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_scalar_store_reinterpret_{}",
        std::process::id()
    ));
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
}

#[test]
fn native_selected_raw_byte_pointer_half_store_keeps_raw_offset() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %a, i32 %i) {
entry:
  %wide = zext i32 %i to i64
  %cond = icmp eq i32 %i, 0
  %wordp = getelementptr inbounds i32, ptr addrspace(1) %a, i64 %wide
  %word = load i32, ptr addrspace(1) %wordp, align 4
  %abyte = getelementptr inbounds i8, ptr addrspace(1) %a, i64 0
  %bbyte = getelementptr inbounds i8, ptr addrspace(1) %a, i64 4
  %sel = select i1 %cond, ptr addrspace(1) %abyte, ptr addrspace(1) %bbyte
  %halfp = getelementptr inbounds half, ptr addrspace(1) %sel, i64 %wide
  store half 0xH3C00, ptr addrspace(1) %halfp, align 2
  store i32 %word, ptr addrspace(1) %wordp, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_raw_half_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("_ptr_Private_uint"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_load_accepts_mul_by_aligned_select_byte_step() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @load_raw_step(ptr addrspace(2) %p, i1 %wide, i32 %idx) {
entry:
  %tag = load i32, ptr addrspace(2) %p
  %step32 = select i1 %wide, i32 16, i32 36
  %bytes32 = mul i32 %idx, %step32
  %bytes = zext i32 %bytes32 to i64
  %byte = getelementptr inbounds i8, ptr addrspace(2) %p, i64 %bytes
  %v = load i32, ptr addrspace(2) %byte
  %out = add i32 %v, %tag
  ret i32 %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_raw_half_load_uses_dynamic_lane_for_half_stride() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @load_raw_half(ptr addrspace(1) %p, i32 %idx) {
entry:
  %tag = load i32, ptr addrspace(1) %p
  %idx64 = zext i32 %idx to i64
  %halfp = getelementptr inbounds half, ptr addrspace(1) %p, i64 %idx64
  %v = load half, ptr addrspace(1) %halfp
  ret half %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_raw_subword_stores_use_dynamic_lane_read_modify_write() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @store_raw_subwords(ptr addrspace(1) %p, i32 %idx, half %h, i16 %s, i8 %b) {
entry:
  %tag = load i32, ptr addrspace(1) %p
  %idx64 = zext i32 %idx to i64
  %bytep = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %idx64
  %halfp = bitcast ptr addrspace(1) %bytep to ptr addrspace(1)
  store half %h, ptr addrspace(1) %halfp, align 2
  %next = getelementptr inbounds i8, ptr addrspace(1) %bytep, i64 1
  %shortp = bitcast ptr addrspace(1) %next to ptr addrspace(1)
  store i16 %s, ptr addrspace(1) %shortp, align 1
  store i8 %b, ptr addrspace(1) %bytep, align 1
  ret void
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpAtomicAnd"), "{asm}");
    assert!(asm.contains("OpAtomicOr"), "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("OpBitwiseXor"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
}

#[test]
fn native_raw_i16_load_assembles_unaligned_byte_offset() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i16 @load_unaligned_u16(ptr addrspace(1) %p, i32 %idx) {
entry:
  %idx64 = zext i32 %idx to i64
  %bytep = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %idx64
  %arrayp = bitcast ptr addrspace(1) %bytep to ptr addrspace(1)
  %shortp = getelementptr inbounds [2 x i16], ptr addrspace(1) %arrayp, i64 %idx64, i64 0
  %v = load i16, ptr addrspace(1) %shortp, align 1
  ret i16 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
}

#[test]
fn native_raw_i64_load_combines_two_words() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i64 @load_raw_i64(ptr addrspace(2) %p, i64 %idx) {
entry:
  %tag = load i32, ptr addrspace(2) %p
  %byte = getelementptr inbounds i8, ptr addrspace(2) %p, i64 8
  %v = load i64, ptr addrspace(2) %byte
  %tag64 = zext i32 %tag to i64
  %out = xor i64 %v, %tag64
  ret i64 %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    assert!(asm.contains("OpBitwiseXor"), "{asm}");
}

#[test]
fn native_direct_load_infers_pointer_pointee_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @load_scalar(ptr addrspace(2) %p) {
entry:
  %v = load float, ptr addrspace(2) %p
  ret float %v
}
"#;
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load spv");
    let param_ptr_ty = module.functions[0].parameters[0]
        .result_type
        .expect("param pointer type");
    let ptr = module
        .types_global_values
        .iter()
        .find(|inst| inst.result_id == Some(param_ptr_ty))
        .expect("pointer type");
    let Operand::IdRef(pointee) = ptr.operands[1] else {
        panic!("pointer type should have pointee operand");
    };
    let pointee_ty = module
        .types_global_values
        .iter()
        .find(|inst| inst.result_id == Some(pointee))
        .expect("pointee type");
    assert_eq!(pointee_ty.class.opcode, Op::TypeFloat);
    assert_eq!(pointee_ty.operands[0], Operand::LiteralBit32(32));
}

#[test]
fn native_gep_keeps_dynamic_first_index_for_element_buffers() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @load_indexed(ptr addrspace(1) %p, i64 %idx) {
entry:
  %g = getelementptr inbounds float, ptr addrspace(1) %p, i64 %idx
  %v = load float, ptr addrspace(1) %g
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_identity_bitcast_param_gep_uses_access_chain() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @load_indexed_alias(ptr addrspace(1) %p, i64 %idx) {
entry:
  %alias = bitcast ptr addrspace(1) %p to ptr addrspace(1)
  %g = getelementptr inbounds float, ptr addrspace(1) %alias, i64 %idx
  %v = load float, ptr addrspace(1) %g
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
}

#[test]
fn native_kernel_packed_buffer_metadata_keeps_record_lane_geps_valid() {
    let ll = r#"
source_filename = "packed-buffer"

define void @packed_kernel(ptr addrspace(1) %src, ptr addrspace(1) %dst, <3 x i32> %gid) {
entry:
  %x = extractelement <3 x i32> %gid, i64 0
  %idx = zext i32 %x to i64
  %src0 = getelementptr inbounds [3 x float], ptr addrspace(1) %src, i64 %idx, i64 0
  %a = load float, ptr addrspace(1) %src0, align 4
  %src1 = getelementptr inbounds [3 x float], ptr addrspace(1) %src, i64 0, i64 1
  %b = load float, ptr addrspace(1) %src1, align 4
  %dst0 = getelementptr inbounds [2 x float], ptr addrspace(1) %dst, i64 %idx, i64 0
  store float %a, ptr addrspace(1) %dst0, align 4
  %dst1 = getelementptr inbounds [2 x float], ptr addrspace(1) %dst, i64 0, i64 1
  store float %b, ptr addrspace(1) %dst1, align 4
  ret void
}

!air.version = !{!0}
!air.language_version = !{!1}
!air.kernel = !{!2}
!0 = !{i32 2, i32 8, i32 0}
!1 = !{!"Metal", i32 3, i32 0, i32 0}
!2 = !{ptr @packed_kernel, !3, !4}
!3 = !{}
!4 = !{!5, !6, !7}
!5 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"packed_float3", !"air.arg_name", !"src"}
!6 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"packed_float2", !"air.arg_name", !"dst"}
!7 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_packed_buffer_gep_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble");
    if std::env::var("DBG_ASM").is_ok() {
        eprintln!("{asm}");
    }
    assert!(asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_integer_alloca_retyped_by_half_array_gep_view() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @local_half_array_view(i64 %idx, half %value) {
entry:
  %slot = alloca i64, align 8
  %alias = bitcast ptr %slot to ptr
  store i64 0, ptr %slot, align 8
  %p = getelementptr inbounds [4 x half], ptr %alias, i64 0, i64 %idx
  store half %value, ptr %p, align 2
  %v = load half, ptr %p, align 2
  ret half %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("OpConstantNull"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
}

#[test]
fn native_collapses_redundant_wrapper_gep_on_derived_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%air.arrblk.0 = type { [0 x float] }
define float @wrapped(ptr addrspace(1) %b, i64 %idx) {
entry:
  %arrayidx = getelementptr inbounds %air.arrblk.0, ptr addrspace(1) %b, i64 0, i32 0, i64 %idx
  %aircanon.0 = getelementptr inbounds %air.arrblk.0, ptr addrspace(1) %arrayidx, i64 0, i32 0, i64 0
  %v = load float, ptr addrspace(1) %aircanon.0
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let chains = asm.matches("OpInBoundsAccessChain").count();
    assert_eq!(chains, 1, "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_composes_zero_wrapper_gep_offsets_from_derived_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%air.arrblk.0 = type { [0 x <4 x float>] }
define <4 x float> @wrapped_offset(ptr addrspace(2) %b) {
entry:
  %arrayidx = getelementptr inbounds %air.arrblk.0, ptr addrspace(2) %b, i64 0, i32 0, i64 7
  %next = getelementptr inbounds %air.arrblk.0, ptr addrspace(2) %arrayidx, i64 0, i32 0, i64 1
  %v = load <4 x float>, ptr addrspace(2) %next
  ret <4 x float> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let access_chains = asm
        .lines()
        .filter(|line| line.contains("OpInBoundsAccessChain"))
        .collect::<Vec<_>>();
    assert_eq!(access_chains.len(), 2, "{asm}");
    assert!(asm.lines().any(|line| line.ends_with(" 8")), "{asm}");
    assert!(!asm.lines().any(|line| line.ends_with(" 1")), "{asm}");
}

#[test]
fn native_zero_offset_gep_emits_no_operandless_access_chain() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @indexless_gep(ptr addrspace(3) %shared) {
entry:
  %same = getelementptr i32, ptr addrspace(3) %shared, i64 0
  store i32 7, ptr addrspace(3) %same, align 4
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpAccessChain")
                || line.contains("OpInBoundsAccessChain")
                || line.contains("OpPtrAccessChain")
                || line.contains("OpInBoundsPtrAccessChain")
        }),
        "{asm}"
    );
}

#[test]
fn native_composes_dynamic_zero_wrapper_gep_offsets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%air.arrblk.0 = type { [0 x <4 x float>] }
define <4 x float> @wrapped_offset(ptr addrspace(2) %b, i64 %idx) {
entry:
  %arrayidx = getelementptr inbounds %air.arrblk.0, ptr addrspace(2) %b, i64 0, i32 0, i64 %idx
  %next = getelementptr inbounds %air.arrblk.0, ptr addrspace(2) %arrayidx, i64 0, i32 0, i64 1
  %v = load <4 x float>, ptr addrspace(2) %next
  ret <4 x float> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert_eq!(asm.matches("OpInBoundsAccessChain").count(), 2, "{asm}");
}

#[test]
fn native_composes_linear_scalar_gep_offsets() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @scalar_offset(ptr addrspace(2) %b, i64 %idx) {
entry:
  %base = getelementptr inbounds float, ptr addrspace(2) %b, i64 %idx
  %next = getelementptr inbounds float, ptr addrspace(2) %base, i64 1
  %v = load float, ptr addrspace(2) %next
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpIAdd"), "{asm}");
    let access_chains = asm
        .lines()
        .filter(|line| line.contains("OpInBoundsAccessChain"))
        .collect::<Vec<_>>();
    assert_eq!(access_chains.len(), 2, "{asm}");
    let first_result = access_chains[0].split('=').next().unwrap().trim();
    assert!(
        !access_chains[1].contains(&format!(" {first_result} ")),
        "{asm}"
    );
}

fn wide_half_load_hints(align: u32) -> usize {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) noundef readonly "air-buffer-no-alias" %0, ptr addrspace(1) noundef "air-buffer-no-alias" %1, i32 noundef %2) local_unnamed_addr {
  %4 = zext i32 %2 to i64
  %5 = getelementptr inbounds <4 x half>, ptr addrspace(1) %0, i64 %4
  %6 = load <4 x half>, ptr addrspace(1) %5, align ALIGN
  %7 = getelementptr inbounds <4 x half>, ptr addrspace(1) %1, i64 %4
  store <4 x half> %6, ptr addrspace(1) %7, align ALIGN
  ret void
}

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19, !20}
!18 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"src"}
!19 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"dst"}
!20 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#
    .replace("ALIGN", &align.to_string());
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_wide_half_load_{align}_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    module
        .all_inst_iter()
        .filter(|i| {
            i.class.opcode == Op::Load
                && matches!(i.operands.as_slice(),
                    [_, Operand::MemoryAccess(a), Operand::LiteralBit32(8)] if *a == spirv::MemoryAccess::ALIGNED)
        })
        .count()
}

fn wide_float_load_hints(lanes: u32, align: u32) -> usize {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) noundef readonly "air-buffer-no-alias" %0, ptr addrspace(1) noundef "air-buffer-no-alias" %1, i32 noundef %2) local_unnamed_addr {
  %4 = zext i32 %2 to i64
  %5 = getelementptr inbounds <LANES x float>, ptr addrspace(1) %0, i64 %4
  %6 = load <LANES x float>, ptr addrspace(1) %5, align ALIGN
  %7 = getelementptr inbounds <LANES x float>, ptr addrspace(1) %1, i64 %4
  store <LANES x float> %6, ptr addrspace(1) %7, align ALIGN
  ret void
}

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19, !20}
!18 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 SIZE, !"air.arg_type_align_size", i32 ALIGN, !"air.arg_type_name", !"floatN", !"air.arg_name", !"src"}
!19 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 SIZE, !"air.arg_type_align_size", i32 ALIGN, !"air.arg_type_name", !"floatN", !"air.arg_name", !"dst"}
!20 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#
    .replace("LANES", &lanes.to_string())
    .replace("SIZE", &(lanes * 4).to_string())
    .replace("ALIGN", &align.to_string());
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_wide_float_load_{lanes}_{align}_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    module
        .all_inst_iter()
        .filter(|i| {
            i.class.opcode == Op::Load
                && matches!(i.operands.as_slice(),
                    [_, Operand::MemoryAccess(a), Operand::LiteralBit32(n)] if *a == spirv::MemoryAccess::ALIGNED && *n >= 8)
        })
        .count()
}

#[test]
fn native_float2_and_float4_device_loads_state_their_air_alignment_on_even_lane_words() {
    assert_eq!(
        wide_float_load_hints(2, 8),
        1,
        "an align-8 float2 states Aligned 8 on its lane-0 word, once"
    );
    assert_eq!(
        wide_float_load_hints(4, 16),
        2,
        "an align-16 float4 states it on lanes 0 and 2 (two pairs)"
    );
    assert_eq!(
        wide_float_load_hints(2, 4),
        0,
        "an align-4 float2 states nothing"
    );
}

#[test]
fn native_half4_device_load_states_its_air_alignment_on_the_first_word() {
    assert_eq!(
        wide_half_load_hints(8),
        1,
        "an align-8 half4 states Aligned 8 on its first word, once"
    );
    assert_eq!(
        wide_half_load_hints(4),
        0,
        "an align-4 half4 states nothing"
    );
}

fn float4_store_aligned16(elem: &str) -> usize {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) noundef readonly "air-buffer-no-alias" %0, ptr addrspace(1) noundef "air-buffer-no-alias" %1, i32 noundef %2) local_unnamed_addr {
  %4 = zext i32 %2 to i64
  %5 = getelementptr inbounds <4 x float>, ptr addrspace(1) %0, i64 %4
  %6 = load <4 x float>, ptr addrspace(1) %5, align 16
  %7 = getelementptr inbounds ELEM, ptr addrspace(1) %1, i64 %4
  store <4 x float> %6, ptr addrspace(1) %7, align 16
  ret void
}

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19, !20}
!18 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"src"}
!19 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"dst"}
!20 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#
    .replace("ELEM", elem);
    let tag = if elem == "float" { "f" } else { "f4" };
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_b78_a2_{tag}_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    module
        .all_inst_iter()
        .filter(|i| {
            i.class.opcode == Op::Store
                && matches!(i.operands.as_slice(),
                    [_, _, Operand::MemoryAccess(a), Operand::LiteralBit32(16)] if a.contains(spirv::MemoryAccess::ALIGNED))
        })
        .count()
}

#[test]
fn b78_a2_a_float4_store_through_a_float_index_is_not_promised_16() {
    assert_eq!(
        float4_store_aligned16("float"),
        0,
        "a float4 stored at float_ptr + off states no Aligned 16"
    );
    assert_eq!(
        float4_store_aligned16("<4 x float>"),
        1,
        "a float4 stored at float4_ptr + i states Aligned 16 once"
    );
}

fn masked_shift_sites(spv: &[u8]) -> (usize, usize) {
    let module = load_bytes(spv).expect("load translated module");
    let widths: HashMap<Word, u32> = module
        .types_global_values
        .iter()
        .filter(|t| t.class.opcode == Op::TypeInt)
        .filter_map(|t| match t.operands.first() {
            Some(Operand::LiteralBit32(w)) => Some((t.result_id?, *w)),
            _ => None,
        })
        .collect();
    let defs: HashMap<Word, (Op, Option<u32>, Vec<Word>)> = module
        .all_inst_iter()
        .filter_map(|i| {
            let ids = i
                .operands
                .iter()
                .filter_map(|o| match o {
                    Operand::IdRef(x) => Some(*x),
                    _ => None,
                })
                .collect();
            Some((
                i.result_id?,
                (
                    i.class.opcode,
                    i.result_type.and_then(|t| widths.get(&t).copied()),
                    ids,
                ),
            ))
        })
        .collect();
    let is = |id: Word, op: Op, width: u32| {
        defs.get(&id)
            .filter(|d| d.0 == op && d.1 == Some(width))
            .map(|d| d.2.clone())
    };
    let (mut narrow, mut wide) = (0, 0);
    for &id in defs.keys() {
        if let Some(and) =
            is(id, Op::UConvert, 32).and_then(|src| is(*src.first()?, Op::BitwiseAnd, 16))
        {
            if and
                .iter()
                .any(|&x| is(x, Op::ShiftLeftLogical, 16).is_some())
            {
                narrow += 1;
            }
        }
        if let Some(zext) =
            is(id, Op::ShiftLeftLogical, 32).and_then(|src| is(*src.first()?, Op::UConvert, 32))
        {
            if zext
                .first()
                .is_some_and(|&x| is(x, Op::BitwiseAnd, 16).is_some())
            {
                wide += 1;
            }
        }
    }
    (narrow, wide)
}

#[test]
fn native_masked_16_bit_shift_under_a_zero_extend_is_widened_before_the_shift() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) noundef "air-buffer-no-alias" %0, i32 noundef %1) local_unnamed_addr {
  %3 = trunc i32 %1 to i16
  %4 = shl i16 %3, 3
  %5 = and i16 %4, 248
  %6 = zext i16 %5 to i32
  %7 = zext i32 %1 to i64
  %8 = getelementptr inbounds i32, ptr addrspace(1) %0, i64 %7
  store i32 %6, ptr addrspace(1) %8, align 4
  ret void
}

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19}
!18 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"dst"}
!19 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_widen_shl_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    assert_eq!(
        masked_shift_sites(&spv),
        (0, 1),
        "{}",
        disassemble(&spv).unwrap_or_default()
    );
}

#[test]
fn native_whole_buffer_param_select_loads_values_before_selecting() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %0, ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %1, ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %2, <2 x i16> noundef %3) local_unnamed_addr {
  %5 = extractelement <2 x i16> %3, i64 0
  %6 = icmp eq i16 %5, 1
  %7 = select i1 %6, ptr addrspace(1) %2, ptr addrspace(1) %1
  %8 = load <4 x half>, ptr addrspace(1) %7, align 8
  %9 = tail call fast <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half> %8)
  %10 = insertelement <2 x i16> %3, i16 2, i64 1
  tail call void @air.write_texture_2d.i16.v4f32(ptr addrspace(1) captures(none) %0, <2 x i16> %10, <4 x float> %9, i16 0, i32 3)
  ret void
}

declare void @air.write_texture_2d.i16.v4f32(ptr addrspace(1) captures(none), <2 x i16>, <4 x float>, i16, i32)
declare <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half>)

!air.kernel = !{!15}
!15 = !{ptr @k, !16, !17}
!16 = !{}
!17 = !{!18, !19, !20, !21}
!18 = !{i32 0, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<float, read_write>", !"air.arg_name", !"tex"}
!19 = !{i32 1, !"air.buffer", !"air.location_index", i32 12, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"bufL"}
!20 = !{i32 2, !"air.buffer", !"air.location_index", i32 13, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"bufR"}
!21 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_whole_buffer_select_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_selects = asm
        .lines()
        .filter(|line| line.contains("OpSelect") && line.contains("_ptr_"))
        .collect::<Vec<_>>();
    assert!(pointer_selects.is_empty(), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_storage_buffer_pointer_select_replays_two_typed_leaf_loads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Buffer = type { [4 x half], [4 x half] }
define half @main(ptr addrspace(1) %buf, i1 %cond, i32 %idx) {
entry:
  %a = getelementptr inbounds %Buffer, ptr addrspace(1) %buf, i64 0, i32 0
  %b = getelementptr inbounds %Buffer, ptr addrspace(1) %buf, i64 0, i32 1
  %sel = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %p = getelementptr inbounds [4 x half], ptr addrspace(1) %sel, i64 0, i32 %idx
  %v = load half, ptr addrspace(1) %p, align 2
  ret half %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let pointer_selects = asm
        .lines()
        .filter(|line| {
            line.contains("OpSelect")
                && (line.contains("_ptr_StorageBuffer") || line.contains("_ptr_UniformConstant"))
        })
        .collect::<Vec<_>>();
    let value_loads = asm.lines().filter(|line| line.contains("OpLoad")).count();
    assert!(pointer_selects.is_empty(), "{asm}");
    assert_eq!(value_loads, 2, "{asm}");
    assert!(
        asm.lines().any(|line| {
            line.contains("OpSelect")
                && !line.contains("_ptr_StorageBuffer")
                && !line.contains("_ptr_UniformConstant")
        }),
        "{asm}"
    );
}

#[test]
fn native_mixed_private_uniform_pointer_select_replays_loaded_values() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Config = type { i32, i32 }
@default_config = internal addrspace(2) global %Config zeroinitializer, align 4

define i32 @main(ptr addrspace(2) %argument_config, i1 %use_default) {
entry:
  %selected = select i1 %use_default, ptr addrspace(2) @default_config, ptr addrspace(2) %argument_config
  %field = getelementptr inbounds %Config, ptr addrspace(2) %selected, i64 0, i32 1
  %value = load i32, ptr addrspace(2) %field, align 4
  ret i32 %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_selects = asm
        .lines()
        .filter(|line| line.contains("OpSelect") && line.contains("_ptr_"))
        .collect::<Vec<_>>();
    assert!(pointer_selects.is_empty(), "{asm}");
    assert_eq!(asm.matches("OpLoad").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpSelect").count(), 1, "{asm}");
}

#[test]
fn native_inlined_helper_forwards_mixed_storage_pointer_select() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Config = type { i32, i32 }
@default_config = internal addrspace(2) global %Config zeroinitializer, align 4

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %argument_config) {
entry:
  %selected = select i1 true, ptr addrspace(2) @default_config, ptr addrspace(2) %argument_config
  %value = call i32 @read_second(ptr addrspace(2) %selected)
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

define internal i32 @read_second(ptr addrspace(2) %config) {
entry:
  %field = getelementptr inbounds %Config, ptr addrspace(2) %config, i64 0, i32 1
  %value = load i32, ptr addrspace(2) %field, align 4
  ret i32 %value
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Config", !"air.arg_name", !"argument_config"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_inline_selected_pointer_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_selects = asm
        .lines()
        .filter(|line| line.contains("OpSelect") && line.contains("_ptr_"))
        .collect::<Vec<_>>();
    assert!(pointer_selects.is_empty(), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(asm.matches("OpLoad").count() >= 2, "{asm}");
    let module = load_bytes(&spv).expect("load native spv");
    let id_named = |name: &str| {
        module
            .debug_names
            .iter()
            .filter(|instruction| instruction.class.opcode == Op::Name)
            .find(|instruction| {
                matches!(instruction.operands.get(1), Some(Operand::LiteralString(n)) if n == name)
            })
            .and_then(|instruction| match instruction.operands.first() {
                Some(Operand::IdRef(id)) => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no id named {name}: {asm}"))
    };
    let body = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    let derived_from = |root: Word| {
        body.iter()
            .find(|instruction| {
                matches!(
                    instruction.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain
                ) && instruction.operands.first() == Some(&Operand::IdRef(root))
            })
            .and_then(|chain| chain.result_id)
            .and_then(|chain| {
                body.iter().find(|instruction| {
                    instruction.class.opcode == Op::Load
                        && instruction.operands.first() == Some(&Operand::IdRef(chain))
                })
            })
            .and_then(|load| load.result_id)
    };
    let stored = body
        .iter()
        .find(|instruction| instruction.class.opcode == Op::Store)
        .and_then(|store| store.operands.get(1))
        .cloned()
        .expect("the entry stores its result");
    assert_eq!(
        Some(stored),
        derived_from(id_named("default_config")).map(Operand::IdRef),
        "the `i1 true` arm is the default config's own load: {asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_selected_pointer_reads_narrow_prefix_in_value_space() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
@narrow = internal addrspace(2) global i32 7, align 4

define <4 x float> @frag(<4 x float> %position, ptr addrspace(2) %wide, i1 %runtime) {
entry:
  %declared = load i64, ptr addrspace(2) %wide, align 8
  %selected = select i1 %runtime, ptr addrspace(2) %wide, ptr addrspace(2) @narrow
  %value = load i32, ptr addrspace(2) %selected, align 4
  %bits = bitcast i32 %value to float
  %out = insertelement <4 x float> zeroinitializer, float %bits, i32 0
  ret <4 x float> %out
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"wide"}
!6 = !{i32 2, !"air.fragment_input", !"runtime", !"air.flat", !"air.arg_type_name", !"bool", !"air.arg_name", !"runtime"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_pointer_narrow_prefix_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_nested_pointer_select_load_replays_every_level_at_the_load_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define float @nested(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, i1 %ab, i1 %abc) {
entry:
  %a_seed = load float, ptr addrspace(1) %a, align 4
  %b_seed = load float, ptr addrspace(1) %b, align 4
  %first = select i1 %ab, ptr addrspace(1) %a, ptr addrspace(1) %b
  %second = select i1 %abc, ptr addrspace(1) %c, ptr addrspace(1) %first
  %value = load float, ptr addrspace(1) %second, align 4
  ret float %value
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let float_type = module
        .types_global_values
        .iter()
        .find(|instruction| instruction.class.opcode == Op::TypeFloat)
        .and_then(|instruction| instruction.result_id)
        .expect("float type");
    let select_types = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::Select)
        .map(|instruction| instruction.result_type)
        .collect::<Vec<_>>();
    assert_eq!(select_types, vec![Some(float_type); 2], "{asm}");
}

#[test]
fn native_nested_pointer_select_gep_store_replays_typed_leaf_accesses() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @nested(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c) {
entry:
  %first = select i1 true, ptr addrspace(1) %a, ptr addrspace(1) %b
  %second = select i1 false, ptr addrspace(1) %c, ptr addrspace(1) %first
  %dst = getelementptr inbounds float, ptr addrspace(1) %second, i64 0
  store float 1.000000e+00, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @nested, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"c"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let float_type = module
        .types_global_values
        .iter()
        .find(|instruction| instruction.class.opcode == Op::TypeFloat)
        .and_then(|instruction| instruction.result_id)
        .expect("float type");
    let pointer_selects = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.class.opcode == Op::Select
                && instruction
                    .result_type
                    .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    let body = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert_eq!(pointer_selects, 0, "{asm}");
    let stores = body
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Store)
        .collect::<Vec<_>>();
    assert_eq!(
        stores.len(),
        1,
        "only the selected buffer is written: {asm}"
    );
    let (Some(Operand::IdRef(target)), Some(Operand::IdRef(value))) =
        (stores[0].operands.first(), stores[0].operands.get(1))
    else {
        panic!("a store takes two ids: {asm}");
    };
    assert_eq!(
        module
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(*value))
            .and_then(|i| i.result_type),
        Some(float_type),
        "the one write stores the float constant: {asm}"
    );
    let root = body
        .iter()
        .find(|instruction| instruction.result_id == Some(*target))
        .and_then(|instruction| match instruction.operands.first() {
            Some(Operand::IdRef(base)) => Some(*base),
            _ => None,
        })
        .expect("the store target is an access chain");
    let binding = module
        .annotations
        .iter()
        .find(|i| {
            i.class.opcode == Op::Decorate
                && i.operands.first() == Some(&Operand::IdRef(root))
                && matches!(i.operands.get(1), Some(Operand::Decoration(d)) if *d == Decoration::Binding)
        })
        .and_then(|i| match i.operands.get(2) {
            Some(Operand::LiteralBit32(binding)) => Some(*binding),
            _ => None,
        });
    assert_eq!(binding, Some(0), "the literals select buffer `a`: {asm}");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_selected_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn a_merged_pointer_at_a_constant_index_is_addressed_not_descended() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @merged_constant_index(ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %a, ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %b, ptr addrspace(1) noundef captures(none) "air-buffer-no-alias" %out, i32 %gid) {
entry:
  %lo = icmp ult i32 %gid, 4
  br i1 %lo, label %take_a, label %take_b

take_a:
  br label %merge

take_b:
  br label %merge

merge:
  %p = phi ptr addrspace(1) [ %a, %take_a ], [ %b, %take_b ]
  %e1 = getelementptr inbounds i32, ptr addrspace(1) %p, i64 1
  %v1 = load i32, ptr addrspace(1) %e1, align 4
  %i = zext i32 %gid to i64
  %o0 = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %i
  store i32 %v1, ptr addrspace(1) %o0, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @merged_constant_index, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load native spv");
    let physical_steps = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::PtrAccessChain)
        .count();
    assert!(physical_steps >= 1, "{asm}");
    assert!(asm.contains("OpConvertPtrToU"), "{asm}");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_merged_constant_index_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_pointer_phi_merges_selected_access_trees_leafwise() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @nested_phi(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, i1 %choose, i1 %advance) {
entry:
  %first = select i1 %choose, ptr addrspace(1) %a, ptr addrspace(1) %b
  %second = select i1 %choose, ptr addrspace(1) %c, ptr addrspace(1) %first
  br i1 %advance, label %offset, label %direct

offset:
  %advanced = getelementptr inbounds float, ptr addrspace(1) %second, i64 1
  br label %merge

direct:
  br label %merge

merge:
  %joined = phi ptr addrspace(1) [ %advanced, %offset ], [ %second, %direct ]
  %dst = getelementptr inbounds float, ptr addrspace(1) %joined, i64 0
  store float 1.000000e+00, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @nested_phi, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"c"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    let asm = disassemble(&spv).expect("disassemble");
    let mut module = load_bytes(&spv).expect("load native spv");
    let phis = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::Phi)
        .collect::<Vec<_>>();
    let pointer_phis = phis
        .iter()
        .filter(|instruction| {
            instruction
                .result_type
                .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    assert_eq!(pointer_phis, 0, "{asm}");
    assert_eq!(phis.len(), 3, "{asm}");
    let store_roots = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::Store)
        .filter_map(|instruction| match instruction.operands.first() {
            Some(Operand::IdRef(pointer)) => Some(*pointer),
            _ => None,
        })
        .filter_map(|pointer| {
            module
                .functions
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.instructions)
                .find(|instruction| instruction.result_id == Some(pointer))
                .and_then(|chain| match chain.operands.first() {
                    Some(Operand::IdRef(root)) => Some(*root),
                    _ => None,
                })
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(store_roots.len(), 3, "{asm}");
    assert_eq!(
        crate::native::construct_interface_cross_binding_pointer_merges_module(
            &mut module,
            crate::reflect::DescriptorLayout::default(),
        ),
        None,
        "final resource construction must leave no address-domain closure"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_selected_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_walked_storage_buffer_pointer_becomes_an_index_phi() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @walk(ptr addrspace(2) %src, ptr addrspace(1) %dst, i32 %gid) {
entry:
  br label %outer

outer:
  %oi = phi i32 [ 0, %entry ], [ %oi_next, %latch ]
  %p_outer = phi ptr addrspace(2) [ %src, %entry ], [ %p_step, %latch ]
  %acc_outer = phi float [ 0.000000e+00, %entry ], [ %acc_inner, %latch ]
  br label %inner

inner:
  %ii = phi i32 [ 0, %outer ], [ %ii_next, %inner ]
  %p_inner = phi ptr addrspace(2) [ %p_outer, %outer ], [ %p_step, %inner ]
  %acc_in = phi float [ %acc_outer, %outer ], [ %acc_next, %inner ]
  %v = load float, ptr addrspace(2) %p_inner, align 4
  %acc_next = fadd float %acc_in, %v
  %p_step = getelementptr inbounds float, ptr addrspace(2) %p_inner, i64 1
  %ii_next = add i32 %ii, 1
  %done_in = icmp sge i32 %ii_next, 4
  br i1 %done_in, label %latch, label %inner

latch:
  %acc_inner = phi float [ %acc_next, %inner ]
  %oi_next = add i32 %oi, 1
  %done_out = icmp sge i32 %oi_next, 2
  br i1 %done_out, label %exit, label %outer

exit:
  store float %acc_inner, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @walk, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let pointer_phis = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::Phi)
        .filter(|instruction| {
            instruction
                .result_type
                .is_some_and(|ty| pointer_type_storage_class(&module, ty).is_some())
        })
        .count();
    assert_eq!(pointer_phis, 0, "{asm}");
    assert!(!asm.contains("VariablePointersStorageBuffer"), "{asm}");
    assert!(asm.contains("OpAccessChain"), "{asm}");
}

#[test]
fn native_scalar_store_to_private_aggregate_root_targets_first_field() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Config = type { i8, i32 }
@config = internal addrspace(2) global %Config zeroinitializer, align 4

define void @initialize(i8 %value) {
entry:
  store i8 %value, ptr addrspace(2) @config, align 4
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert_eq!(asm.matches("OpInBoundsAccessChain").count(), 1, "{asm}");
    let store = asm
        .lines()
        .find(|line| line.contains("OpStore"))
        .expect("scalar store");
    assert!(!store.contains("%config "), "{asm}");
}

#[test]
fn native_selected_storage_buffer_gep_chain_replays_two_typed_leaf_loads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Buffer = type { [8 x float], [8 x float] }
define float @main(ptr addrspace(1) %buf, i1 %cond, i32 %base, i32 %idx) {
entry:
  %a = getelementptr inbounds %Buffer, ptr addrspace(1) %buf, i64 0, i32 0, i32 0
  %b = getelementptr inbounds %Buffer, ptr addrspace(1) %buf, i64 0, i32 1, i32 0
  %sel = select i1 %cond, ptr addrspace(1) %a, ptr addrspace(1) %b
  %p = getelementptr inbounds float, ptr addrspace(1) %sel, i32 %base
  %q = getelementptr inbounds float, ptr addrspace(1) %p, i32 %idx
  %v = load float, ptr addrspace(1) %q, align 4
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let pointer_selects = asm
        .lines()
        .filter(|line| {
            line.contains("OpSelect")
                && (line.contains("_ptr_StorageBuffer") || line.contains("_ptr_UniformConstant"))
        })
        .collect::<Vec<_>>();
    let value_loads = asm.lines().filter(|line| line.contains("OpLoad")).count();
    assert!(pointer_selects.is_empty(), "{asm}");
    assert_eq!(value_loads, 2, "{asm}");
    assert!(
        asm.lines().any(|line| {
            line.contains("OpSelect")
                && !line.contains("_ptr_StorageBuffer")
                && !line.contains("_ptr_UniformConstant")
        }),
        "{asm}"
    );
}

#[test]
fn native_pointer_select_null_arm_load_selects_values() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @main(ptr addrspace(2) %buf, i1 %cond, i64 %idx) {
entry:
  %p = getelementptr inbounds float, ptr addrspace(2) %buf, i64 %idx
  %sel = select i1 %cond, ptr addrspace(2) null, ptr addrspace(2) %p
  %v = load float, ptr addrspace(2) %sel, align 4
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let pointer_types = asm
        .lines()
        .filter(|line| line.contains("OpTypePointer"))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    let pointer_selects = asm
        .lines()
        .filter(|line| {
            line.contains("OpSelect")
                && line
                    .split_whitespace()
                    .nth(3)
                    .is_some_and(|ty| pointer_types.contains(ty))
        })
        .collect::<Vec<_>>();
    let float_type = asm
        .lines()
        .find(|line| line.contains("OpTypeFloat 32"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing float type:\n{asm}"));
    assert!(pointer_selects.is_empty(), "{asm}");
    assert!(
        asm.lines().any(|line| {
            line.contains("OpSelect") && line.split_whitespace().nth(3) == Some(float_type)
        }),
        "{asm}"
    );
    assert!(asm.lines().any(|line| line.contains("OpLoad")), "{asm}");
}

#[test]
fn native_pointer_select_null_rooted_gep_arm_materializes_undefined_value() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Buffer = type { [16 x float] }
define float @main(ptr addrspace(2) %buf, i1 %cond, i64 %idx) {
entry:
  %present = getelementptr inbounds %Buffer, ptr addrspace(2) %buf, i64 0, i32 0, i64 %idx
  %missing = getelementptr inbounds %Buffer, ptr addrspace(2) null, i64 0, i32 0, i64 %idx
  %sel = select i1 %cond, ptr addrspace(2) %missing, ptr addrspace(2) %present
  %v = load float, ptr addrspace(2) %sel, align 4
  ret float %v
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_types = asm
        .lines()
        .filter(|line| line.contains("OpTypePointer"))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    let undefined_pointers = asm
        .lines()
        .filter(|line| line.contains("OpUndef"))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let result = words.next()?;
            let _equals = words.next()?;
            let _opcode = words.next()?;
            let ty = words.next()?;
            pointer_types.contains(ty).then_some(result)
        })
        .collect::<HashSet<_>>();
    assert!(
        asm.lines()
            .filter(|line| line.contains("OpLoad"))
            .all(|line| line
                .split_whitespace()
                .last()
                .is_none_or(|pointer| !undefined_pointers.contains(pointer))),
        "{asm}"
    );
}

#[test]
fn native_load_through_undefined_pointer_constructs_undefined_value() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @main(i1 %done) {
entry:
  br label %loop
loop:
  %missing = phi ptr addrspace(2) [ null, %entry ], [ %next, %loop ]
  %v = load float, ptr addrspace(2) %missing, align 4
  %next = getelementptr inbounds float, ptr addrspace(2) %missing, i64 1
  br i1 %done, label %exit, label %loop
exit:
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let pointer_types = asm
        .lines()
        .filter(|line| line.contains("OpTypePointer"))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    let undefined_pointers = asm
        .lines()
        .filter(|line| line.contains("OpUndef"))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let result = words.next()?;
            let _equals = words.next()?;
            let _opcode = words.next()?;
            let ty = words.next()?;
            pointer_types.contains(ty).then_some(result)
        })
        .collect::<HashSet<_>>();
    assert!(
        asm.lines()
            .filter(|line| line.contains("OpLoad"))
            .all(|line| line
                .split_whitespace()
                .last()
                .is_none_or(|pointer| !undefined_pointers.contains(pointer))),
        "{asm}"
    );
    assert!(
        asm.lines().any(|line| line.contains("OpCopyObject")),
        "{asm}"
    );
}

#[test]
fn native_pointer_select_null_arm_gep_uses_concrete_arm() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @main(ptr addrspace(1) %buf, i1 %cond, i64 %base, i64 %idx) {
entry:
  %p = getelementptr inbounds float, ptr addrspace(1) %buf, i64 %base
  %sel = select i1 %cond, ptr addrspace(1) null, ptr addrspace(1) %p
  %q = getelementptr inbounds float, ptr addrspace(1) %sel, i64 %idx
  %v = load float, ptr addrspace(1) %q, align 4
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    let pointer_types = asm
        .lines()
        .filter(|line| line.contains("OpTypePointer"))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    let pointer_selects = asm
        .lines()
        .filter(|line| {
            line.contains("OpSelect")
                && line
                    .split_whitespace()
                    .nth(3)
                    .is_some_and(|ty| pointer_types.contains(ty))
        })
        .collect::<Vec<_>>();
    assert!(pointer_selects.is_empty(), "{asm}");
    assert!(
        asm.lines()
            .filter(|line| line.contains("OpInBoundsAccessChain"))
            .count()
            >= 2,
        "{asm}"
    );
}

#[test]
fn native_pointer_select_null_arm_call_arg_materializes_selected_pointer() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
define internal fastcc float @helper(ptr addrspace(1) %p, i32 %count) {
entry:
  %x = getelementptr inbounds [3 x float], ptr addrspace(1) %p, i64 1, i64 0
  %v = load float, ptr addrspace(1) %x, align 4
  ret float %v
}

define void @main(ptr addrspace(1) %buf, ptr addrspace(1) %out, i32 %tid) {
entry:
  %cond = icmp ne i32 %tid, 0
  %base = zext i32 %tid to i64
  %p = getelementptr inbounds [3 x float], ptr addrspace(1) %buf, i64 %base
  %sel = select i1 %cond, ptr addrspace(1) %p, ptr addrspace(1) null
  %n = select i1 %cond, i32 2, i32 0
  %v = tail call fast fastcc float @helper(ptr addrspace(1) %sel, i32 %n)
  %dst = getelementptr inbounds float, ptr addrspace(1) %out, i64 %base
  store float %v, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"packed_float3", !"air.arg_name", !"buf"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_pointer_select_call_arg_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_buffer_pointer_select_null_arm_rewrites_to_value_select() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %buf, ptr addrspace(2) %flag) {
entry:
  %flag_value = load i32, ptr addrspace(2) %flag, align 4
  %cond = icmp eq i32 %flag_value, 0
  %p = getelementptr inbounds float, ptr addrspace(1) %buf, i64 0
  %sel = select i1 %cond, ptr addrspace(1) null, ptr addrspace(1) %p
  %v = load float, ptr addrspace(1) %sel, align 4
  store float %v, ptr addrspace(1) %p, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"buf"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"flag"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_buffer_pointer_select_null_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let pointer_types = asm
        .lines()
        .filter(|line| line.contains("OpTypePointer"))
        .filter_map(|line| line.split_whitespace().next())
        .collect::<HashSet<_>>();
    let pointer_selects = asm
        .lines()
        .filter(|line| {
            line.contains("OpSelect")
                && line
                    .split_whitespace()
                    .nth(3)
                    .is_some_and(|ty| pointer_types.contains(ty))
        })
        .collect::<Vec<_>>();
    let float_type = asm
        .lines()
        .find(|line| line.contains("OpTypeFloat 32"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing float type:\n{asm}"));
    assert!(pointer_selects.is_empty(), "{asm}");
    assert!(
        asm.lines().any(|line| {
            line.contains("OpSelect") && line.split_whitespace().nth(3) == Some(float_type)
        }),
        "{asm}"
    );
    assert!(
        asm.lines().any(|line| {
            line.contains("OpTypePointer StorageBuffer")
                && line.split_whitespace().last() == Some(float_type)
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_workgroup_global_does_not_reuse_block_decorated_buffer_struct() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }

@tg = internal addrspace(3) global %"struct.metal::_atomic" zeroinitializer, align 4

define void @k(ptr addrspace(1) %histogram, i32 %i) {
entry:
  tail call void @air.atomic.local.store.i32(ptr addrspace(3) @tg, i32 0, i32 0, i32 1, i1 true)
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds %"struct.metal::_atomic", ptr addrspace(1) %histogram, i64 %idx, i32 0
  %local = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) @tg, i32 1, i32 0, i32 1, i1 true)
  %global = tail call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %slot, i32 %local, i32 0, i32 2, i1 true)
  ret void
}

declare void @air.atomic.local.store.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"metal::_atomic", !"air.arg_name", !"histogram"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"__s"}
!5 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_workgroup_block_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let block_types = module
        .annotations
        .iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::Decorate {
                return None;
            }
            match inst.operands.as_slice() {
                [Operand::IdRef(target), Operand::Decoration(Decoration::Block)] => Some(*target),
                _ => None,
            }
        })
        .collect::<HashSet<_>>();
    let workgroup_block_pointees = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup))
        })
        .filter_map(|inst| inst.result_id)
        .filter_map(|var| variable_pointee_type(&module, var))
        .filter(|pointee| block_types.contains(pointee))
        .collect::<Vec<_>>();
    assert!(
        workgroup_block_pointees.is_empty(),
        "Workgroup variables must not point at Block-decorated struct types: {workgroup_block_pointees:?}\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_bool_vector_index_lowers_to_integer_dynamic_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define half @pick(<2 x half> %v, half %x, i1 %idx) {
entry:
  %old = extractelement <2 x half> %v, i1 %idx
  %next = insertelement <2 x half> %v, half %x, i1 %idx
  %new = extractelement <2 x half> %next, i32 0
  %out = fadd half %old, %new
  ret half %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(asm.contains("OpVectorInsertDynamic"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
}

#[test]
fn native_fast_sincos_writes_cosine_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %slot = alloca float
  %sin = call fast float @air.fast_sincos.f32(float 1.000000e+00, ptr %slot)
  %cos = load float, ptr %slot
  %sum = fadd fast float %sin, %cos
  ret void
}

declare float @air.fast_sincos.f32(float, ptr)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_fast_sincos_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains(" Sin "), "{asm}");
    assert!(asm.contains(" Cos "), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_sincos_f16_writes_cosine_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  %slot = alloca half
  %sin = call fast half @air.sincos.f16(half 0xH3C00, ptr %slot)
  %cos = load half, ptr %slot
  %sum = fadd fast half %sin, %cos
  ret void
}

declare half @air.sincos.f16(half, ptr)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_sincos_f16_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpCapability Float16"), "{asm}");
    assert!(asm.contains(" Sin "), "{asm}");
    assert!(asm.contains(" Cos "), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_half_buffer_loads_as_float_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %in, ptr addrspace(1) %out) {
entry:
  %p = getelementptr inbounds half, ptr addrspace(1) %in, i64 4
  %v = load <4 x float>, ptr addrspace(1) %p, align 16
  store <4 x float> %v, ptr addrspace(1) %out, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"half", !"air.arg_name", !"in"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_half_to_v4float_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_promoted_lut_global_can_be_loaded() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@.air.lut.0 = private unnamed_addr addrspace(2) constant [1 x float] [float 1.000000e+00]
define float @lut() {
entry:
  %p = getelementptr inbounds [1 x float], ptr @.air.lut.0, i64 0, i64 0
  %v = load float, ptr %p
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("Private"), "{asm}");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(!asm.contains("OpConstantNull"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_promoted_lut_global_preserves_array_initializer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@.air.lut.0 = private unnamed_addr addrspace(2) constant [2 x float] [float 1.000000e+00, float 2.000000e+00]
define float @lut(i64 %idx) {
entry:
  %p = getelementptr inbounds [2 x float], ptr @.air.lut.0, i64 0, i64 %idx
  %v = load float, ptr %p
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(!asm.contains("OpConstantNull"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_promoted_struct_global_preserves_zero_member_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@.air.lut.0 = private unnamed_addr addrspace(2) constant <{ [20 x [3 x [2 x i16]]], [12 x [3 x [2 x i16]]] }> <{ [20 x [3 x [2 x i16]]] zeroinitializer, [12 x [3 x [2 x i16]]] zeroinitializer }>
define i16 @lut() {
entry:
  %p = getelementptr inbounds <{ [20 x [3 x [2 x i16]]], [12 x [3 x [2 x i16]]] }>, ptr addrspace(2) @.air.lut.0, i64 0, i32 0, i64 19, i64 1, i64 0
  %v = load i16, ptr addrspace(2) %p
  ret i16 %v
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_inline_struct_global_gep_preserves_zero_member_index() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@.air.lut.0 = private unnamed_addr addrspace(2) constant <{ [20 x [3 x [2 x i16]]], [12 x [3 x [2 x i16]]] }> <{ [20 x [3 x [2 x i16]]] zeroinitializer, [12 x [3 x [2 x i16]]] zeroinitializer }>
define i16 @read(ptr addrspace(2) %p) {
entry:
  %v = load i16, ptr addrspace(2) %p
  ret i16 %v
}
define void @lut(ptr addrspace(1) writeonly %out) {
entry:
  %v = call i16 @read(ptr addrspace(2) getelementptr inbounds (<{ [20 x [3 x [2 x i16]]], [12 x [3 x [2 x i16]]] }>, ptr addrspace(2) @.air.lut.0, i64 0, i32 0, i64 19, i64 1, i64 0))
  store i16 %v, ptr addrspace(1) %out, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @lut, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 2, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"ushort", !"air.arg_name", !"out"}
"#;
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary construction");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
}

#[test]
fn native_member0_array_element_gep_from_flat_scalar_global_uses_view_indices() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@.air.lut.0 = private unnamed_addr addrspace(2) constant <{ [20 x [3 x [2 x i16]]], [12 x [3 x [2 x i16]]] }> <{ [20 x [3 x [2 x i16]]] zeroinitializer, [12 x [3 x [2 x i16]]] zeroinitializer }>
define i16 @lut() {
entry:
  %p = getelementptr inbounds [3 x [2 x i16]], ptr addrspace(2) @.air.lut.0, i64 19, i64 1, i64 0
  %v = load i16, ptr addrspace(2) %p
  ret i16 %v
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpTypeStruct"), "{asm}");
    let access = asm
        .lines()
        .find(|line| line.contains("OpInBoundsAccessChain"))
        .unwrap_or_else(|| panic!("missing access chain:\n{asm}"));
    assert_eq!(
        access.matches('%').count(),
        6,
        "expected result, type, root, and three view index ids:\n{access}\n{asm}"
    );
}

#[test]
fn native_gep_through_vector_yields_lane_pointer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.uniform" = type { [2 x <4 x float>] }
define float @lane(ptr addrspace(2) %u) {
entry:
  %p = getelementptr inbounds %"struct.uniform", ptr addrspace(2) %u, i64 0, i32 0, i64 1, i64 2
  %v = load float, ptr addrspace(2) %p
  ret float %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_gep_strips_nuw_nusw_flags() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::matrix.21" = type { [1 x <4 x float>] }
define <4 x float> @matrix(ptr addrspace(2) %m) {
entry:
  %p = getelementptr inbounds nuw %"struct.metal::matrix.21", ptr addrspace(2) %m, i64 0, i32 0, i64 0
  %v = load <4 x float>, ptr addrspace(2) %p
  ret <4 x float> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
}

#[test]
fn native_parser_accepts_typed_null_pointer_literal() {
    let value = parse_typed_value("ptr addrspace(2) noundef null").expect("parse typed null");
    assert_eq!(value.ty, LlType::Ptr(2));
    assert!(matches!(value.value, LlValue::Zero));
}

#[test]
fn native_global_byte_string_semicolon_is_not_comment() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@bytes = internal unnamed_addr addrspace(2) constant [4 x i8] c"A;B\00", align 1

define void @keep() {
entry:
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(asm.contains("OpVariable"), "{asm}");
}

#[test]
fn native_global_array_of_struct_constants_lowers() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Point = type { <2 x float>, <2 x float> }
@points = private unnamed_addr addrspace(2) constant [2 x %struct.Point] [%struct.Point { <2 x float> <float 1.0, float 2.0>, <2 x float> <float 3.0, float 4.0> }, %struct.Point { <2 x float> <float 5.0, float 6.0>, <2 x float> <float 7.0, float 8.0> }], align 16

define void @keep() {
entry:
  ret void
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantComposite"), "{asm}");
    assert!(asm.contains("OpVariable"), "{asm}");
}

#[test]
fn native_constant_selected_private_table_preserves_access_chain_storage() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
@first = internal unnamed_addr addrspace(2) constant [2 x <2 x i8>] [<2 x i8> zeroinitializer, <2 x i8> <i8 1, i8 2>], align 2
@second = internal unnamed_addr addrspace(2) constant [2 x <2 x i8>] [<2 x i8> <i8 3, i8 4>, <2 x i8> <i8 5, i8 6>], align 2

define void @k(ptr addrspace(1) %out, i32 %index) {
entry:
  %selected = select i1 true, ptr addrspace(2) @first, ptr addrspace(2) @second
  %wide = zext i32 %index to i64
  %element = getelementptr inbounds <2 x i8>, ptr addrspace(2) %selected, i64 %wide
  %value = load <2 x i8>, ptr addrspace(2) %element, align 2
  store <2 x i8> %value, ptr addrspace(1) %out, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 2, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"char2", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_private_table_storage_reconcile_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load spv");
    for instruction in module.all_inst_iter().filter(|instruction| {
        matches!(
            instruction.class.opcode,
            Op::AccessChain
                | Op::InBoundsAccessChain
                | Op::PtrAccessChain
                | Op::InBoundsPtrAccessChain
        )
    }) {
        let Operand::IdRef(base) = instruction.operands[0] else {
            continue;
        };
        let base_type = module
            .all_inst_iter()
            .find_map(|candidate| {
                (candidate.result_id == Some(base)).then_some(candidate.result_type)
            })
            .flatten()
            .expect("access-chain base type");
        assert_eq!(
            pointer_type_storage_class(&module, instruction.result_type.expect("result type")),
            pointer_type_storage_class(&module, base_type),
            "access-chain result must inherit its base storage class"
        );
    }
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_direct_struct_buffer_drops_gep_root_zero_after_metadata_rebuild() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Header = type { %Atomic, i32 }
%Atomic = type { i32 }

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %field = getelementptr inbounds %Header, ptr addrspace(1) %src, i64 0, i32 0, i32 0
  %v = load i32, ptr addrspace(1) %field, align 4
  %dst = getelementptr inbounds i32, ptr addrspace(1) %out, i64 0
  store i32 %v, ptr addrspace(1) %dst, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"src"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 8, i32 0, !"Header::flags", !"flags", i32 8, i32 4, i32 0, !"uint", !"tail"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"low", i32 4, i32 4, i32 0, !"uint", !"high"}
!6 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_direct_struct_buffer_root_zero_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpInBoundsAccessChain")
                && line.contains("_ptr_StorageBuffer_uint")
                && line.matches("%uint_0").count() >= 3
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_packed_single_lane_struct_buffer_uses_one_storage_binding() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Out = type { [1 x i32], [1 x i32], [1 x i32] }

define void @k(ptr addrspace(1) %out) {
entry:
  call fastcc void @helper(ptr addrspace(1) %out)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %out) {
entry:
  %min = getelementptr inbounds %Out, ptr addrspace(1) %out, i64 0, i32 0, i64 0
  store i32 11, ptr addrspace(1) %min, align 4
  %max = getelementptr inbounds %Out, ptr addrspace(1) %out, i64 0, i32 1, i64 0
  store i32 22, ptr addrspace(1) %max, align 4
  %avg = getelementptr inbounds %Out, ptr addrspace(1) %out, i64 0, i32 2, i64 0
  store i32 33, ptr addrspace(1) %avg, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 12, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Out", !"air.arg_name", !"out"}
!4 = !{i32 0, i32 4, i32 0, !"packed_int1", !"min", i32 4, i32 4, i32 0, !"packed_int1", !"max", i32 8, i32 4, i32 0, !"packed_int1", !"avg"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_packed_single_lane_struct_buffer_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("Binding 0").count(), 1, "{asm}");
    assert!(
        !asm.contains("OpTypeRuntimeArray %uint"),
        "packed struct should not create a raw uint alias\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_helper_keeps_scalar_constant_pointee_over_global_array_arg() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
@weights = internal addrspace(2) constant [7 x half] [half 0xH3C00, half 0xH4000, half 0xH4200, half 0xH4400, half 0xH4500, half 0xH4600, half 0xH4700], align 2

define void @k(ptr addrspace(1) %out, i32 %idx) {
entry:
  call fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(2) @weights, i32 %idx)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(2) %weights, i32 %idx) {
entry:
  %first = load half, ptr addrspace(2) %weights, align 2
  %wide = zext i32 %idx to i64
  %p = getelementptr inbounds half, ptr addrspace(2) %weights, i64 %wide
  %second = load half, ptr addrspace(2) %p, align 2
  %sum = fadd half %first, %second
  store half %sum, ptr addrspace(1) %out, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
"#;
    let ir = super::super::ir::LlModule::parse(ll).expect("parse");
    assert_eq!(
        ir.ptr_pointees
            .get(&("helper".to_string(), "%weights".to_string())),
        Some(&LlType::Half)
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_scalar_constant_call_array_arg_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpLoad %half %weights") || line.contains("OpLoad %half @_")
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_direct_call_pointer_result_carries_storage_to_followup_gep() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Matrix = type { [4 x <4 x float>] }
%View = type { [2 x %Matrix] }

define void @k(ptr addrspace(2) %view, ptr addrspace(1) %out, i16 %eye) {
entry:
  %ret = tail call ptr addrspace(2) @helper(i16 %eye, ptr addrspace(2) %view)
  %lane = getelementptr inbounds %Matrix, ptr addrspace(2) %ret, i64 0, i32 0, i64 0
  %value = load <4 x float>, ptr addrspace(2) %lane, align 16
  store <4 x float> %value, ptr addrspace(1) %out, align 16
  ret void
}

declare ptr addrspace(2) @helper(i16, ptr addrspace(2))
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpFunctionCall"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_union_head_bitcast_load_does_not_create_same_binding_raw_alias() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Params = type { %Union }
%Union = type { %ColorUint }
%ColorUint = type { i32, i32, i32, i32 }

define void @k(ptr addrspace(2) %params, ptr addrspace(1) %out) {
entry:
  %union = getelementptr inbounds %Params, ptr addrspace(2) %params, i64 0, i32 0
  %alias = bitcast ptr addrspace(2) %union to ptr addrspace(2)
  %v = load float, ptr addrspace(2) %alias, align 4
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !8}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 16, i32 0, !"Union", !"color"}
!5 = !{!"air.struct_type_info", !6, i32 0, i32 4, i32 0, !"ColorUchar", !"uchar", !"air.struct_type_info", !7, i32 0, i32 16, i32 0, !"ColorUint", !"uint32"}
!6 = !{i32 0, i32 4, i32 0, !"uint", !"packed"}
!7 = !{i32 0, i32 4, i32 0, !"uint", !"x", i32 4, i32 4, i32 0, !"uint", !"y", i32 8, i32 4, i32 0, !"uint", !"z", i32 12, i32 4, i32 0, !"uint", !"w"}
!8 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_union_head_bitcast_no_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(
        asm.matches(" Binding 0").count(),
        1,
        "buffer(0) must not also grow a raw same-binding StorageBuffer alias:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_metadata_byte_field_word_load_store_uses_raw_alias() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Header = type { %Flags }
%Flags = type { i32, i32, i32 }

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %sp = getelementptr inbounds %Header, ptr addrspace(1) %src, i64 0, i32 0, i32 2
  %v = load i32, ptr addrspace(1) %sp, align 4
  %dp = getelementptr inbounds %Header, ptr addrspace(1) %dst, i64 0, i32 0, i32 2
  store i32 %v, ptr addrspace(1) %dp, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"src"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 12, i32 0, !"Flags", !"flags"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 4, i32 0, !"uint", !"b", i32 8, i32 1, i32 0, !"bool", !"c"}
!6 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_metadata_byte_field_word_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpLoad %uint") && line.contains("_ptr_StorageBuffer_uchar")
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }

    let layout = crate::reflect::DescriptorLayout {
        set: 5,
        ..Default::default()
    };
    let custom = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_descriptor_layout(layout)
            .expect("custom raw-alias layout"),
    )
    .expect("custom raw-alias translation");
    let custom_asm = disassemble(&custom).expect("disassemble custom raw alias");
    assert!(custom_asm.contains("DescriptorSet 5"), "{custom_asm}");
    assert!(!custom_asm.contains("DescriptorSet 0"), "{custom_asm}");
    tools::spirv_val_bytes(&custom, &tmp).expect("custom raw-alias spirv-val");
}

#[test]
fn native_unaligned_metadata_byte_field_word_load_store_uses_raw_alias() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
%Header = type { %Flags }
%Flags = type { i32, i32, i8, i8, i32 }

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %sp = getelementptr inbounds %Header, ptr addrspace(1) %src, i64 0, i32 0, i32 3
  %v = load i32, ptr addrspace(1) %sp, align 1
  %dp = getelementptr inbounds %Header, ptr addrspace(1) %dst, i64 0, i32 0, i32 3
  store i32 %v, ptr addrspace(1) %dp, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"src"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 16, i32 0, !"Flags", !"flags"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 4, i32 0, !"uint", !"b", i32 8, i32 1, i32 0, !"bool", !"c", i32 9, i32 1, i32 0, !"bool", !"d", i32 12, i32 4, i32 0, !"uint", !"e"}
!6 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_unaligned_metadata_byte_field_word_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.lines().any(|line| {
            line.contains("OpLoad %uint") && line.contains("_ptr_StorageBuffer_uchar")
        }),
        "{asm}"
    );
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.contains("OpAtomicAnd"), "{asm}");
    assert!(asm.contains("OpAtomicOr"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_wrapped_raw_buffer_vector_load_keeps_descriptor_provenance() {
    let ll = r#"
%S = type { [200 x i32], <3 x float> }
%W = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %wrapped = insertvalue %W poison, ptr addrspace(1) %src, 0
  %root = extractvalue %W %wrapped, 0
  %ptr = getelementptr inbounds %S, ptr addrspace(1) %root, i64 0, i32 1
  %value = load <3 x float>, ptr addrspace(1) %ptr, align 16
  %lane = extractelement <3 x float> %value, i32 0
  %byte_ptr = getelementptr inbounds i8, ptr addrspace(1) %root, i64 3
  %byte = load i8, ptr addrspace(1) %byte_ptr, align 1
  %wide = zext i8 %byte to i32
  %as_float = uitofp i32 %wide to float
  %sum = fadd float %lane, %as_float
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 1024, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_wrapped_raw_vector_provenance_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpCompositeConstruct"), "{asm}");
    assert_eq!(
        asm.lines()
            .filter(|line| line.contains("Binding 0"))
            .count(),
        2,
        "{asm}"
    );
    assert!(asm.contains("ArrayStride 16"), "{asm}");
    assert!(asm.contains("ArrayStride 1"), "{asm}");
    assert_eq!(
        asm.lines().filter(|line| line.contains("OpLoad")).count(),
        2,
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_raw_induction_phi_stays_raw_under_network_seed() {
    let ll = r#"
define void @raw_ptr_induction(ptr addrspace(1) noundef readonly captures(none) "air-buffer-no-alias" %0, ptr addrspace(1) noundef writeonly "air-buffer-no-alias" %1, <3 x i32> noundef %2) local_unnamed_addr #0 {
  %4 = bitcast ptr addrspace(1) %0 to ptr addrspace(1)
  br label %5

5:                                                ; preds = %5, %3
  %6 = phi i32 [ 0, %3 ], [ %14, %5 ]
  %7 = phi ptr addrspace(1) [ %4, %3 ], [ %13, %5 ]
  %8 = phi float [ 0.000000e+00, %3 ], [ %12, %5 ]
  %9 = load <4 x float>, ptr addrspace(1) %7, align 16
  %10 = extractelement <4 x float> %9, i64 0
  %11 = extractelement <4 x float> %9, i64 1
  %12 = fadd fast float %8, %10
  %13 = getelementptr inbounds <4 x float>, ptr addrspace(1) %7, i64 1
  %14 = add nuw i32 %6, 1
  %15 = icmp ult i32 %14, 8
  br i1 %15, label %5, label %16

16:                                               ; preds = %5
  %17 = fadd fast float %12, %11
  %18 = extractelement <3 x i32> %2, i64 0
  %19 = zext i32 %18 to i64
  %20 = getelementptr inbounds float, ptr addrspace(1) %1, i64 %19
  store float %17, ptr addrspace(1) %20, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @raw_ptr_induction, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 128, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_raw_induction_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(
        !asm.lines().any(|l| l.contains("OpPhi %_ptr_")),
        "expected no pointer-typed OpPhi (raw index phi instead):\n{asm}"
    );
    assert!(asm.contains("Binding 0"), "src buffer unbound:\n{asm}");
    assert!(asm.contains("Binding 1"), "dst buffer unbound:\n{asm}");
    assert!(
        asm.lines().filter(|l| l.contains("OpLoad")).count() >= 4,
        "expected the <4 x float> load modeled as real raw word loads:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_same_buffer_pointer_phi_is_constructed_as_an_index_phi() {
    let ll = r#"
define void @same_buffer_phi(ptr addrspace(1) noundef writeonly "air-buffer-no-alias" %out, ptr addrspace(1) noundef readonly "air-buffer-no-alias" %src, <3 x i32> %gid) local_unnamed_addr #0 {
entry:
  %x = extractelement <3 x i32> %gid, i64 0
  %cond = icmp eq i32 %x, 0
  br i1 %cond, label %left, label %right

left:
  %p0 = getelementptr inbounds float, ptr addrspace(1) %src, i64 1
  br label %merge

right:
  %p1 = getelementptr inbounds float, ptr addrspace(1) %src, i64 2
  br label %merge

merge:
  %p = phi ptr addrspace(1) [ %p0, %left ], [ %p1, %right ]
  %v = load float, ptr addrspace(1) %p, align 4
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @same_buffer_phi, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"gid"}
"#;
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(
        !asm.lines().any(|line| line.contains("OpPhi %_ptr_")),
        "same-root pointer phi must be represented by its varying index:\n{asm}"
    );
    assert!(!asm.contains("VariablePointersStorageBuffer"), "{asm}");
    let module = load_bytes(&out).expect("load emitted module");
    let integer_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeInt)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    assert!(
        module.functions.iter().any(|function| function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| instruction.class.opcode == Op::Phi
                && instruction
                    .result_type
                    .is_some_and(|ty| integer_types.contains(&ty)))),
        "expected an integer index phi:\n{asm}"
    );
}

#[test]
fn native_raw_pointer_phi_admits_a_null_arm() {
    let ll = include_str!("../../../validation/fixtures/public/kernel_nullable_branch_cursor.ll");
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_null_arm_raw_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    let module = load_bytes(&out).expect("load translated module");
    let src = module
        .annotations
        .iter()
        .find_map(|instruction| match instruction.operands.as_slice() {
            [
                Operand::IdRef(id),
                Operand::Decoration(Decoration::Binding),
                Operand::LiteralBit32(0),
            ] => Some(*id),
            _ => None,
        })
        .expect("src buffer binding");
    let body = module
        .functions
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
        .collect::<Vec<_>>();
    let src_chains = body
        .iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::InBoundsAccessChain
                && matches!(instruction.operands.first(), Some(Operand::IdRef(base)) if *base == src)
        })
        .count();
    assert!(
        src_chains >= 5,
        "expected the merged cursor's load addressed on the buffer, found {src_chains} chains:\n{asm}"
    );
    let integer_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeInt)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let bool_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeBool)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let phis = body
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Phi)
        .collect::<Vec<_>>();
    assert!(
        phis.iter().any(|instruction| instruction
            .result_type
            .is_some_and(|ty| integer_types.contains(&ty))),
        "expected an integer index phi for the merged cursor:\n{asm}"
    );
    assert!(
        phis.iter().any(|instruction| instruction
            .result_type
            .is_some_and(|ty| bool_types.contains(&ty))),
        "expected the nullness phi the null comparison reads:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_nested_loop_raw_induction_cycle_stays_raw() {
    let ll = include_str!(
        "../../../validation/fixtures/public/kernel_nested_loop_constant_weight_walk.ll"
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_nested_induction_cycle_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    let module = load_bytes(&out).expect("load translated module");
    let weights = module
        .annotations
        .iter()
        .find_map(|instruction| match instruction.operands.as_slice() {
            [
                Operand::IdRef(id),
                Operand::Decoration(Decoration::Binding),
                Operand::LiteralBit32(0),
            ] => Some(*id),
            _ => None,
        })
        .expect("weights buffer binding");
    let weight_chains = module
        .functions
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
        .filter(|instruction| {
            instruction.class.opcode == Op::InBoundsAccessChain
                && matches!(instruction.operands.first(), Some(Operand::IdRef(base)) if *base == weights)
        })
        .count();
    assert!(
        weight_chains >= 4,
        "expected the weight load addressed on the buffer, found {weight_chains} chains:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_select_fed_pointer_induction_is_constructed_in_the_index_domain() {
    let ll = r#"
define void @select_fed_pointer_induction(ptr addrspace(1) noundef writeonly "air-buffer-no-alias" %out, ptr addrspace(1) noundef readonly "air-buffer-no-alias" %src, <3 x i32> %gid) local_unnamed_addr #0 {
entry:
  %x = extractelement <3 x i32> %gid, i64 0
  %count = and i32 %x, 7
  %initial = zext i32 %count to i64
  br label %loop

loop:
  %cursor = phi ptr addrspace(1) [ %next, %loop ], [ %src, %entry ]
  %remaining = phi i64 [ %decremented, %loop ], [ %initial, %entry ]
  %half = lshr i64 %remaining, 1
  %middle = getelementptr inbounds i64, ptr addrspace(1) %cursor, i64 %half
  %candidate = getelementptr inbounds i64, ptr addrspace(1) %middle, i64 1
  %take = icmp ne i64 %half, 0
  %next = select i1 %take, ptr addrspace(1) %candidate, ptr addrspace(1) %cursor
  %decremented = add i64 %remaining, -1
  %again = icmp sgt i64 %decremented, 0
  br i1 %again, label %loop, label %exit

exit:
  %value = load i64, ptr addrspace(1) %next, align 8
  store i64 %value, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @select_fed_pointer_induction, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"gid"}
"#;
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    let module = load_bytes(&out).expect("load emitted module");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer).then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    assert!(!module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Phi
            && instruction
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
    }));
    assert!(!module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Select
            && instruction
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
    }));
    assert!(!asm.contains("VariablePointersStorageBuffer"), "{asm}");
    let integer_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeInt)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    assert!(module.functions.iter().any(|function| function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| instruction.class.opcode == Op::Phi
            && instruction
                .result_type
                .is_some_and(|ty| integer_types.contains(&ty)))));
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_select_fed_pointer_induction_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).expect("create validation scratch");
        let validation = tools::spirv_val_bytes(&out, &tmp);
        std::fs::remove_dir_all(&tmp).expect("remove validation scratch");
        validation.expect("spirv-val");
    }
}

#[test]
fn native_dead_null_storage_buffer_pointer_phi_is_pruned_before_emission() {
    let ll = r#"
define void @dead_null_phi(ptr addrspace(1) noundef writeonly "air-buffer-no-alias" %out, ptr addrspace(1) noundef readonly "air-buffer-no-alias" %src, <3 x i32> %gid) local_unnamed_addr #0 {
entry:
  br i1 false, label %null_arm, label %real_arm

null_arm:
  br label %merge

real_arm:
  %p = getelementptr inbounds i8, ptr addrspace(1) %src, i64 4
  br label %merge

merge:
  %q = phi ptr addrspace(1) [ null, %null_arm ], [ %p, %real_arm ]
  %v = load i8, ptr addrspace(1) %q, align 1
  store i8 %v, ptr addrspace(1) %out, align 1
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @dead_null_phi, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"src"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"gid"}
"#;
    let out = crate::translate_native_no_retry(ll, Stage::Kernel).expect("no-retry translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(!asm.contains("VariablePointersStorageBuffer"), "{asm}");
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpPhi %_ptr_StorageBuffer")),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_native_dead_null_phi_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).expect("create validation scratch");
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
        std::fs::remove_dir_all(&tmp).expect("remove validation scratch");
    }
}

#[test]
fn native_forward_unmodeled_device_gep_phi_uses_typed_zero_not_pointer_phi() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Box = type { [3 x float], [3 x float] }
%struct.Table = type { ptr addrspace(1), ptr addrspace(1) }

define float @k(ptr addrspace(2) %table, i1 %cond, i64 %idx) {
entry:
  %f0 = getelementptr inbounds %struct.Table, ptr addrspace(2) %table, i64 0, i32 0
  %b0 = load ptr addrspace(1), ptr addrspace(2) %f0, align 8
  %f1 = getelementptr inbounds %struct.Table, ptr addrspace(2) %table, i64 0, i32 1
  %b1 = load ptr addrspace(1), ptr addrspace(2) %f1, align 8
  br i1 %cond, label %lhs, label %rhs

merge:
  %p = phi ptr addrspace(1) [ %lp, %lhs ], [ %rp, %rhs ]
  %x = getelementptr inbounds %struct.Box, ptr addrspace(1) %p, i64 0, i32 0, i64 0
  %v = load float, ptr addrspace(1) %x, align 4
  ret float %v

lhs:
  %lp = getelementptr inbounds %struct.Box, ptr addrspace(1) %b0, i64 %idx
  br label %merge

rhs:
  %rp = getelementptr inbounds %struct.Box, ptr addrspace(1) %b1, i64 %idx
  br label %merge
}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_forward_unmodeled_gep_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble");
    assert!(
        !asm.lines().any(|line| line.contains("OpPhi %_ptr_")),
        "unmodeled forward-GEP arms must not emit a pointer phi:\n{asm}"
    );
    assert!(
        !asm.lines().any(|line| line.contains("OpSelect %_ptr_")),
        "unmodeled forward-GEP arms must not emit a pointer select:\n{asm}"
    );
    assert!(
        !asm.contains("OpTypePointer Private")
            && asm.contains("OpConstantNull")
            && asm.contains("OpCopyObject"),
        "expected typed zero values without private pointer backing:\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

const VECTOR_ALLOCATION_STRIDE_LL: &str = r#"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

%struct.Image = type <{ i8, i8, i8, i8, <3 x i8>, i8, i8, [2 x i8] }>

define void @k(ptr addrspace(2) noalias readonly dereferenceable(12) %cfg, ptr addrspace(1) %out) {
entry:
  %vp = getelementptr inbounds %struct.Image, ptr addrspace(2) %cfg, i64 0, i32 4
  %v = load <3 x i8>, ptr addrspace(2) %vp, align 4
  %tp = getelementptr inbounds %struct.Image, ptr addrspace(2) %cfg, i64 0, i32 5
  %t = load i8, ptr addrspace(2) %tp, align 4
  %up = getelementptr inbounds %struct.Image, ptr addrspace(2) %cfg, i64 0, i32 6
  %u = load i8, ptr addrspace(2) %up, align 1
  %lane = extractelement <3 x i8> %v, i32 2
  %s0 = add i8 %lane, %t
  %s1 = add i8 %s0, %u
  %z = zext i8 %s1 to i32
  store i32 %z, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Image", !"air.arg_name", !"cfg"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;

fn vector_allocation_stride_with_metadata() -> String {
    let with_reference = VECTOR_ALLOCATION_STRIDE_LL.replacen(
        "!\"air.arg_type_size\", i32 12",
        "!\"air.struct_type_info\", !5, !\"air.arg_type_size\", i32 12",
        1,
    );
    format!(
        "{with_reference}\n!5 = !{{i32 0, i32 1, i32 0, !\"uchar\", !\"a\", i32 1, i32 1, i32 0, !\"uchar\", !\"b\", i32 2, i32 1, i32 0, !\"uchar\", !\"c\", i32 3, i32 1, i32 0, !\"uchar\", !\"d\", i32 4, i32 4, i32 0, !\"uchar3\", !\"v\", i32 8, i32 1, i32 0, !\"uchar\", !\"t\", i32 9, i32 1, i32 0, !\"uchar\", !\"u\"}}\n"
    )
}

const NONCANONICAL_VECTOR_ALIGNMENT_LL: &str = r#"
target datalayout = "e-p:64:64-v24:64:64-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

%struct.Params = type <{ <3 x i8>, i8 }>

define void @k(ptr addrspace(2) noalias readonly dereferenceable(16) %params, ptr addrspace(1) %out) {
entry:
  %field = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 1
  %value = load i8, ptr addrspace(2) %field, align 8
  %wide = zext i8 %value to i32
  store i32 %wide, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;

fn three_lane_byte_vector_struct_offsets(asm: &str) -> Vec<(u32, u32)> {
    let result_id = |line: &str| -> Option<String> {
        let name = line.split_whitespace().next()?;
        name.starts_with('%').then(|| name.to_string())
    };
    let defines = |line: &str, opcode: &str, operands: &[&str]| -> Option<String> {
        let (lhs, rhs) = line.split_once('=')?;
        let mut words = rhs.split_whitespace();
        (words.next()? == opcode).then_some(())?;
        let rest = words.collect::<Vec<_>>();
        (rest == operands).then(|| lhs.trim().to_string())
    };
    let byte_ty = asm
        .lines()
        .map(str::trim)
        .find_map(|line| defines(line, "OpTypeInt", &["8", "0"]))
        .unwrap_or_else(|| panic!("no 8-bit integer type was emitted:\n{asm}"));
    let vector_ty = asm
        .lines()
        .map(str::trim)
        .find_map(|line| defines(line, "OpTypeVector", &[byte_ty.as_str(), "3"]))
        .unwrap_or_else(|| panic!("no <3 x i8> type was emitted:\n{asm}"));
    let struct_ty = asm
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("OpTypeStruct"))
        .find(|line| line.split_whitespace().any(|word| word == vector_ty))
        .and_then(result_id)
        .unwrap_or_else(|| panic!("no struct with a <3 x i8> member was emitted:\n{asm}"));
    let mut offsets = asm
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            let rest = line.strip_prefix("OpMemberDecorate ")?;
            let mut parts = rest.split_whitespace();
            (parts.next()? == struct_ty).then_some(())?;
            let member: u32 = parts.next()?.parse().ok()?;
            (parts.next()? == "Offset").then_some(())?;
            Some((member, parts.next()?.parse().ok()?))
        })
        .collect::<Vec<(u32, u32)>>();
    offsets.sort_unstable();
    offsets
}

#[test]
fn native_three_lane_vector_member_advances_by_its_allocation_size() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_vector_allocation_stride_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(VECTOR_ALLOCATION_STRIDE_LL, Stage::Kernel, &tmp)
        .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(
        three_lane_byte_vector_struct_offsets(&asm),
        vec![
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 8),
            (6, 9),
            (7, 10)
        ],
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_uchar3_struct_preserves_authoritative_air_offsets() {
    let ll = vector_allocation_stride_with_metadata();
    let kern = meta::parse_air_kernel_meta(&ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        &ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit with AIR layout sidecar");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings,
        vec![crate::emit_sidecar::AirStructLayoutMapping {
            param_index: 0,
            struct_ty: emitted.sidecar.air_struct_layout_mappings[0].struct_ty,
            status: crate::emit_sidecar::AirStructLayoutMappingStatus::MappedNatural,
        }]
    );
    assert!(
        emitted
            .sidecar
            .air_struct_offsets
            .values()
            .any(|offsets| offsets == &[0, 1, 2, 3, 4, 8, 9, 10]),
        "{:?}",
        emitted.sidecar.air_struct_offsets
    );
}

#[test]
fn native_struct_layout_overlap_leaves_typed_sidecar_evidence() {
    let ll = vector_allocation_stride_with_metadata().replace("!\"uchar3\"", "!\"float3\"");
    let kern = meta::parse_air_kernel_meta(&ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        &ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit mismatched AIR layout");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::EmittedIsUntypedBuffer
    );
    assert!(emitted.sidecar.air_struct_offsets.is_empty());
}

#[test]
fn native_raw_metadata_mismatch_constructs_block_member_access() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %words) {
entry:
  %slot = getelementptr inbounds i32, ptr addrspace(2) %words, i64 236
  %value = load i32, ptr addrspace(2) %slot, align 4
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 1024, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 1024, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"WordBlock", !"air.arg_name", !"words"}
!5 = !{i32 0, i32 4, i32 256, !"uint", !"data"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_metadata_block_access_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines().any(|line| {
            (line.contains("OpAccessChain") || line.contains("OpInBoundsAccessChain"))
                && line.split_whitespace().count() == 7
        }),
        "{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn file_translation_threads_source_datalayout_into_emitted_offsets() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_source_datalayout_layout_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let source = tmp.join("vector-layout.ll");
    std::fs::write(&source, NONCANONICAL_VECTOR_ALIGNMENT_LL).expect("write source fixture");
    let spv = crate::translate(
        source.to_str().expect("UTF-8 fixture path"),
        Stage::Kernel,
        &tmp,
    )
    .expect("translate source file");
    let asm = disassemble(&spv).expect("disassemble");

    assert_eq!(
        three_lane_byte_vector_struct_offsets(&asm),
        vec![(0, 0), (1, 8)],
        "{asm}"
    );
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn default_ownership_candidate_three_lane_vector_member_advances_by_allocation_size() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_vector_allocation_stride_retry_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let kern = meta::parse_air_kernel_meta(VECTOR_ALLOCATION_STRIDE_LL);
    let construction = crate::construction::ConstructionCtx::new(
        VECTOR_ALLOCATION_STRIDE_LL,
        Stage::Kernel,
        None,
        None,
        kern.as_ref(),
        Some("k"),
        &tmp,
        passes::TransformOptions::default(),
        crate::layout::AirDataLayout::from_ir(VECTOR_ALLOCATION_STRIDE_LL)
            .expect("parse datalayout"),
    );
    let emitted = crate::tools::emit_vulkan_spirv_with_sidecar(
        VECTOR_ALLOCATION_STRIDE_LL,
        &tmp,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("primary emission");
    let spv = construction
        .finish(emitted)
        .expect("finished ownership candidate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(
        three_lane_byte_vector_struct_offsets(&asm),
        vec![
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 8),
            (6, 9),
            (7, 10)
        ],
        "{asm}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

const OPAQUE_STRUCT_MEMBER_LL: &str = r#"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

%struct.Inner = type <{ float, float }>
%struct.Cfg = type <{ i32, %struct.Inner, i32 }>

define void @k(ptr addrspace(2) noalias readonly dereferenceable(16) %cfg, ptr addrspace(1) %out) {
entry:
  %p = getelementptr inbounds %struct.Cfg, ptr addrspace(2) %cfg, i64 0, i32 2
  %v = load i32, ptr addrspace(2) %p, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Cfg", !"air.arg_name", !"cfg"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 8, i32 0, !"Inner", !"b", i32 12, i32 4, i32 0, !"uint", !"c"}
"#;

#[test]
fn native_opaque_struct_member_decodes_at_its_declared_size() {
    let kern = meta::parse_air_kernel_meta(OPAQUE_STRUCT_MEMBER_LL).expect("kernel metadata");
    assert_eq!(
        kern.buffer_layouts.get(&0),
        Some(&meta::AirType::Struct(vec![
            meta::AirMember {
                offset: 0,
                ty: meta::AirType::Scalar(meta::AirScalar::UInt),
            },
            meta::AirMember {
                offset: 4,
                ty: meta::AirType::Opaque { size: 8 },
            },
            meta::AirMember {
                offset: 12,
                ty: meta::AirType::Scalar(meta::AirScalar::UInt),
            },
        ])),
        "an unmodelled member name must decode as its declared byte size, not as a float leaf"
    );
}

#[test]
fn native_opaque_struct_member_keeps_the_declared_offsets() {
    let kern = meta::parse_air_kernel_meta(OPAQUE_STRUCT_MEMBER_LL);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        OPAQUE_STRUCT_MEMBER_LL,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit with AIR layout sidecar");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::MappedNatural,
        "the emitted member occupies exactly the eight declared bytes, so it is the member AIR meant"
    );
    assert!(
        emitted
            .sidecar
            .air_struct_offsets
            .values()
            .any(|offsets| offsets == &[0, 4, 12]),
        "{:?}",
        emitted.sidecar.air_struct_offsets
    );
}

const BARE_BYTE_PAD_LL: &str = r#"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

%struct.Inst = type <{ float, i8, i8, i16 }>

define void @k(ptr addrspace(2) noalias readonly dereferenceable(8) %inst, ptr addrspace(1) %out) {
entry:
  %p = getelementptr inbounds %struct.Inst, ptr addrspace(2) %inst, i64 0, i32 3
  %v = load i16, ptr addrspace(2) %p, align 2
  %w = zext i16 %v to i32
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Inst", !"air.arg_name", !"inst"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
!5 = !{i32 0, i32 4, i32 0, !"float", !"width", i32 4, i32 1, i32 0, !"char", !"cullMode", i32 6, i32 2, i32 0, !"ushort", !"clippingOffset"}
"#;

const CONSTANT_MEMBER_HELPER_LL: &str = r#"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

%struct.Division = type { i32, i32 }
%struct.Params = type { i32, i32, %struct.Division }

define void @k(ptr addrspace(2) noalias readonly dereferenceable(16) %params, ptr addrspace(1) %out) {
entry:
  %division = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 2
  %quotient = tail call fastcc i32 @divide(ptr addrspace(2) %division)
  store i32 %quotient, ptr addrspace(1) %out, align 4
  ret void
}

define internal fastcc i32 @divide(ptr addrspace(2) readonly %p) {
entry:
  %shift.ptr = getelementptr inbounds %struct.Division, ptr addrspace(2) %p, i64 0, i32 1
  %shift = load i32, ptr addrspace(2) %shift.ptr, align 4
  %positive = icmp sgt i32 %shift, 0
  br i1 %positive, label %shifted, label %plain

shifted:
  %shifted.value = shl i32 1, %shift
  br label %join

plain:
  %divisor.ptr = getelementptr inbounds %struct.Division, ptr addrspace(2) %p, i64 0, i32 0
  %divisor = load i32, ptr addrspace(2) %divisor.ptr, align 4
  br label %join

join:
  %result = phi i32 [ %shifted.value, %shifted ], [ %divisor, %plain ]
  ret i32 %result
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;

#[test]
fn native_constant_member_pointer_reaches_a_helper_as_a_cursor() {
    assert_constant_member_helper_reads_the_member(CONSTANT_MEMBER_HELPER_LL);
}

// CoreImage's stitched kernels define their helpers before the kernel. The helper learns its
// parameter's byte cursor from the call, so emitted first it read the constant buffer at offset 0
// (ci_lanczosDownH's sampler transform came from bytes 0..31 instead of 80..111). A single-block
// helper is inlined as text before emission and never showed it.
#[test]
fn native_constant_member_cursor_reaches_a_multi_block_helper_defined_before_the_kernel() {
    let kernel_start = CONSTANT_MEMBER_HELPER_LL
        .find("define void @k(")
        .expect("kernel");
    let helper_start = CONSTANT_MEMBER_HELPER_LL
        .find("define internal fastcc i32 @divide(")
        .expect("helper");
    let helper_end = helper_start
        + CONSTANT_MEMBER_HELPER_LL[helper_start..]
            .find("\n}\n")
            .expect("helper end")
        + 3;
    let reordered = format!(
        "{}{}\n{}{}",
        &CONSTANT_MEMBER_HELPER_LL[..kernel_start],
        &CONSTANT_MEMBER_HELPER_LL[helper_start..helper_end],
        &CONSTANT_MEMBER_HELPER_LL[kernel_start..helper_start],
        &CONSTANT_MEMBER_HELPER_LL[helper_end..]
    );
    assert!(
        reordered.find("@divide(").expect("helper")
            < reordered.find("define void @k(").expect("kernel"),
        "the helper must be defined before the kernel:\n{reordered}"
    );
    assert_constant_member_helper_reads_the_member(&reordered);
}

fn assert_constant_member_helper_reads_the_member(ll: &str) {
    let kern = meta::parse_air_kernel_meta(ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit the constant-member helper call");
    let bytes = passes::transform(
        emitted.module,
        Stage::Kernel,
        None,
        None,
        kern.as_ref(),
        Some("k"),
    )
    .expect("interface transform")
    .assemble()
    .iter()
    .flat_map(|word| word.to_le_bytes())
    .collect::<Vec<_>>();
    let asm = disassemble(&bytes).expect("disassemble");

    let nulls = asm
        .lines()
        .filter_map(|line| {
            line.split_once(" = OpConstantNull ")
                .map(|(result, _)| result.trim().to_string())
        })
        .collect::<Vec<_>>();
    assert!(
        !asm.lines().any(|line| {
            nulls
                .iter()
                .any(|null| line.contains(" = OpCopyObject ") && line.trim_end().ends_with(null))
        }),
        "the helper's reads must reach the constant buffer, not a null placeholder:\n{asm}"
    );

    let constants = asm
        .lines()
        .filter_map(|line| {
            let (result, rest) = line.split_once(" = OpConstant ")?;
            let value = rest.split_whitespace().nth(1)?.parse::<u32>().ok()?;
            Some((value, result.trim().to_string()))
        })
        .collect::<HashMap<_, _>>();
    let params = asm
        .lines()
        .find(|line| line.contains("Binding 0"))
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("params binding")
        .to_string();
    for word in [2u32, 3] {
        let index = constants.get(&word).expect("word index constant");
        assert!(
            asm.lines().any(|line| {
                line.contains("AccessChain")
                    && line.contains(&params)
                    && line.trim_end().ends_with(index)
            }),
            "no access chain reaches params word {word}:\n{asm}"
        );
    }
}

#[test]
fn native_single_byte_pad_between_members_keeps_the_declared_offsets() {
    let kern = meta::parse_air_kernel_meta(BARE_BYTE_PAD_LL);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        BARE_BYTE_PAD_LL,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit with AIR layout sidecar");

    assert_ne!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::EmittedShapeMismatch,
        "the bare i8 at offset 5 is the hole between `char` at 4 and `ushort` at 6, not a member \
         AIR failed to declare"
    );
    assert!(
        emitted
            .sidecar
            .air_struct_offsets
            .values()
            .any(|offsets| offsets == &[0, 4, 5, 6]),
        "{:?}",
        emitted.sidecar.air_struct_offsets
    );
}

#[test]
fn native_opaque_struct_member_of_the_wrong_size_is_still_a_mismatch() {
    let ll = OPAQUE_STRUCT_MEMBER_LL.replace(
        "i32 4, i32 8, i32 0, !\"Inner\"",
        "i32 4, i32 4, i32 0, !\"Inner\"",
    );
    let kern = meta::parse_air_kernel_meta(&ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        &ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit mismatched AIR layout");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::EmittedShapeMismatch
    );
    assert!(emitted.sidecar.air_struct_offsets.is_empty());
}

const BYTE_ADDRESSED_BUFFER_LL: &str = r#"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-n8:16:32"
target triple = "air64-apple-macosx10.15.0"

define void @k(ptr addrspace(2) noalias readonly %cfg, ptr addrspace(1) %out, i32 %tid) {
entry:
  %off = zext i32 %tid to i64
  %p = getelementptr inbounds i8, ptr addrspace(2) %cfg, i64 %off
  %v = load i32, ptr addrspace(2) %p, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Cfg", !"air.arg_name", !"cfg"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"a", i32 4, i32 4, i32 0, !"float", !"b", i32 8, i32 4, i32 0, !"uint", !"c"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;

#[test]
fn native_byte_addressed_buffer_is_untyped_rather_than_mismatched() {
    let kern = meta::parse_air_kernel_meta(BYTE_ADDRESSED_BUFFER_LL);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        BYTE_ADDRESSED_BUFFER_LL,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit byte-addressed buffer");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::EmittedIsUntypedBuffer,
        "a raw buffer has no members to carry the declared offsets, which is not a disagreement"
    );
    assert!(emitted.sidecar.air_struct_offsets.is_empty());
}

#[test]
fn native_typed_buffer_with_wrong_member_shapes_stays_a_mismatch() {
    let ll = OPAQUE_STRUCT_MEMBER_LL.replace(
        "i32 4, i32 8, i32 0, !\"Inner\"",
        "i32 4, i32 4, i32 0, !\"uint\"",
    );
    let kern = meta::parse_air_kernel_meta(&ll);
    let emitted = crate::native::emit_vulkan_spirv_with_sidecar(
        &ll,
        kern.as_ref(),
        Some("k"),
        kern.as_ref().map(|meta| &meta.buffer_layouts),
    )
    .expect("emit mismatched AIR layout");

    assert_eq!(
        emitted.sidecar.air_struct_layout_mappings[0].status,
        crate::emit_sidecar::AirStructLayoutMappingStatus::EmittedShapeMismatch
    );
}

#[test]
fn native_raw_struct_member_offsets_match_the_declared_air_layout() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Params = type <{ i32, i8, [11 x i8], [12 x <4 x float>], float, [12 x i8] }>

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %params) {
entry:
  %arr = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 3
  %m = call fastcc float @sum(ptr addrspace(2) %arr)
  %tail = getelementptr inbounds %struct.Params, ptr addrspace(2) %params, i64 0, i32 4
  %value = load float, ptr addrspace(2) %tail, align 4
  %r = fadd float %value, %m
  store float %r, ptr addrspace(1) %out, align 4
  ret void
}

define internal fastcc float @sum(ptr addrspace(2) %p) {
entry:
  %g = getelementptr inbounds [12 x <4 x float>], ptr addrspace(2) %p, i64 0, i64 1, i64 2
  %v = load float, ptr addrspace(2) %g, align 4
  ret float %v
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 224, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 224, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_struct_member_offsets_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate declared-offset struct");
    let _ = std::fs::remove_dir_all(&tmp);

    let params = reflection
        .binding_at(crate::reflect::ResourceKind::Buffer, 1)
        .expect("params binding");
    assert_eq!(
        params.extent,
        Some(crate::reflect::BufferExtent::Object { bytes: 224 }),
        "AIR declares one 224-byte object"
    );
    let footprint = params.footprint.as_ref().expect("params footprint");
    assert!(
        !footprint.has_unbounded_access,
        "every access has a constant offset: {footprint:?}"
    );
    assert_eq!(
        footprint.static_ranges,
        vec![
            crate::reflect::BufferByteRange {
                offset: 40,
                size: 4
            },
            crate::reflect::BufferByteRange {
                offset: 208,
                size: 4
            },
        ],
        "both reads land where AIR's own offsets put them"
    );
}

#[test]
fn native_constant_buffer_member_pointer_survives_helper_inlining() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.P = type <{ i32, [12 x i8], [4 x <4 x float>] }>

define void @k(ptr addrspace(1) %out, ptr addrspace(2) %params) {
entry:
  %c = getelementptr inbounds %struct.P, ptr addrspace(2) %params, i64 0, i32 0
  %n = load i32, ptr addrspace(2) %c, align 4
  %m = getelementptr inbounds %struct.P, ptr addrspace(2) %params, i64 0, i32 2
  %v = call fastcc float @lane(ptr addrspace(2) %m)
  %w = uitofp i32 %n to float
  %r = fadd float %v, %w
  store float %r, ptr addrspace(1) %out, align 4
  ret void
}

define internal fastcc float @lane(ptr addrspace(2) %p) {
entry:
  %g = getelementptr inbounds [4 x <4 x float>], ptr addrspace(2) %p, i64 0, i64 2, i64 1
  %v = load float, ptr addrspace(2) %g, align 4
  ret float %v
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 80, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 80, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"P", !"air.arg_name", !"params"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_constant_member_helper_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate constant member pointer");
    let _ = std::fs::remove_dir_all(&tmp);

    let footprint = reflection
        .binding_at(crate::reflect::ResourceKind::Buffer, 1)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("params footprint");
    assert_eq!(
        footprint.static_ranges,
        vec![
            crate::reflect::BufferByteRange { offset: 0, size: 4 },
            crate::reflect::BufferByteRange {
                offset: 52,
                size: 4
            },
        ],
        "the helper reads matrix column 2 lane 1 through the descriptor, not a zero placeholder"
    );

    let module = crate::spirv_module::load_bytes(&spv).expect("load spv");
    let nulls = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::ConstantNull)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                assert!(
                    !instruction
                        .operands
                        .iter()
                        .any(|operand| matches!(operand, Operand::IdRef(id) if nulls.contains(id))),
                    "no executable instruction consumes a null placeholder: {instruction:?}"
                );
            }
        }
    }
}

#[test]
fn native_packed_half3_member_does_not_fabricate_an_overlap() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.LineState = type { <2 x float>, <4 x half>, [3 x half], half, <2 x half> }

define void @k(ptr addrspace(1) %states, ptr addrspace(1) %out) {
entry:
  %w = getelementptr inbounds %struct.LineState, ptr addrspace(1) %states, i64 0, i32 3
  %value = load half, ptr addrspace(1) %w, align 2
  store half %value, ptr addrspace(1) %out, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 32, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"LineState", !"air.arg_name", !"states"}
!4 = !{i32 0, i32 8, i32 0, !"float2", !"endOffset", i32 8, i32 8, i32 0, !"half4", !"color", i32 16, i32 6, i32 0, !"packed_half3", !"colorVelocity", i32 22, i32 2, i32 0, !"half", !"widthMultiplier", i32 24, i32 4, i32 0, !"half2", !"endVelocity"}
!5 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 2, !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half*", !"air.arg_name", !"out"}
"#;
    let kern = meta::parse_air_kernel_meta(ll);
    let module = crate::native::ir::LlModule::parse_with_stage_meta(ll, kern.as_ref(), Some("k"))
        .expect("parse typed IR");
    assert!(
        !module.air_metadata_requires_byte_view(
            kern.as_ref()
                .and_then(|meta| meta.buffer_layouts.get(&0))
                .expect("LineState layout")
        ),
        "adjacent members are not a union"
    );

    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_packed_half3_overlap_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate adjacent packed_half3 struct");
    let _ = std::fs::remove_dir_all(&tmp);

    let states = reflection
        .binding_at(crate::reflect::ResourceKind::Buffer, 0)
        .expect("states binding");
    let footprint = states.footprint.as_ref().expect("states footprint");
    assert_eq!(
        footprint.static_ranges,
        vec![crate::reflect::BufferByteRange {
            offset: 22,
            size: 2
        }],
        "the half is read as a half at 22, not as the word at 20"
    );
}

#[test]
fn a_scalar_float_assembles_from_a_byte_view_the_way_a_vector_does() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.attach = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %buf, ptr addrspace(1) %out, i32 %n) {
entry:
  %slot = alloca %struct.attach, align 8
  %f = getelementptr inbounds %struct.attach, ptr %slot, i64 0, i32 0
  store ptr addrspace(1) %buf, ptr %f, align 8
  call fastcc void @helper(ptr %slot, ptr addrspace(1) %out, i32 %n)
  ret void
}

define internal fastcc void @helper(ptr %a, ptr addrspace(1) %out, i32 %n) {
  %f = getelementptr inbounds %struct.attach, ptr %a, i64 0, i32 0
  %p = load ptr addrspace(1), ptr %f, align 8
  %o = sext i32 %n to i64
  %base = getelementptr inbounds i8, ptr addrspace(1) %p, i64 %o
  %b = getelementptr inbounds i8, ptr addrspace(1) %base, i64 12
  %c = bitcast ptr addrspace(1) %b to ptr addrspace(1)
  %v = load float, ptr addrspace(1) %c, align 4
  store float %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"char", !"air.arg_name", !"buf"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"n"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_scalar_byte_view_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let module = load_bytes(&spv).expect("load native spv");
    let _ = std::fs::remove_dir_all(&tmp);

    let private_pointer_types: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Private))
        })
        .filter_map(|inst| inst.result_id)
        .collect();
    let loads: Vec<&Instruction> = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::Load)
        .collect();
    assert!(
        !loads.iter().any(|inst| inst
            .result_type
            .is_some_and(|ty| private_pointer_types.contains(&ty))),
        "no value is loaded out of a Private placeholder"
    );

    let byte_types: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands.first() == Some(&Operand::LiteralBit32(8))
        })
        .filter_map(|inst| inst.result_id)
        .collect();
    let byte_loads = loads
        .iter()
        .filter(|inst| inst.result_type.is_some_and(|ty| byte_types.contains(&ty)))
        .count();
    assert!(
        byte_loads >= 4,
        "the float is assembled from at least its four bytes, saw {byte_loads}"
    );
}

#[test]
fn a_device_address_field_reached_by_bitcast_atomics_like_one_reached_by_gep() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.cache = type { ptr addrspace(1), ptr addrspace(1) }

define void @k(ptr addrspace(1) %slots, ptr addrspace(1) %aux, ptr addrspace(1) %out, i32 %gid) {
entry:
  %c = alloca %struct.cache, align 8
  %f0 = getelementptr inbounds %struct.cache, ptr %c, i64 0, i32 0
  store ptr addrspace(1) %slots, ptr %f0, align 8
  %f1 = getelementptr inbounds %struct.cache, ptr %c, i64 0, i32 1
  store ptr addrspace(1) %aux, ptr %f1, align 8
  %r = call fastcc i32 @helper(ptr %c, i32 %gid)
  %o = zext i32 %gid to i64
  %op = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %o
  store i32 %r, ptr addrspace(1) %op, align 4
  ret void
}

define internal fastcc i32 @helper(ptr %a, i32 %i) {
  %e = alloca i32, align 4
  %b = bitcast ptr %a to ptr
  %p = load ptr addrspace(1), ptr %b, align 8
  store i32 0, ptr %e, align 4
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds float, ptr addrspace(1) %p, i64 %idx
  %new = add i32 %i, 100
  %old = call i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1) %slot, ptr %e, i32 %new, i32 0, i32 0, i32 2, i1 true)
  ret i32 %old
}

declare i32 @air.atomic.global.cmpxchg.weak.i32(ptr addrspace(1), ptr, i32, i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"slots"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"aux"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_bitcast_field_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let module = load_bytes(&spv).expect("load native spv");
    let _ = std::fs::remove_dir_all(&tmp);

    let pointer_storage: HashMap<Word, StorageClass> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| match (inst.result_id, inst.operands.first()) {
            (Some(id), Some(Operand::StorageClass(storage))) => Some((id, *storage)),
            _ => None,
        })
        .collect();
    let value_types: HashMap<Word, Word> = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect();

    let atomics: Vec<&Instruction> = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::AtomicCompareExchange)
        .collect();
    assert_eq!(atomics.len(), 1, "one compare-exchange survives");
    let Some(Operand::IdRef(pointer)) = atomics[0].operands.first() else {
        panic!("compare-exchange has no pointer operand");
    };
    let storage = value_types
        .get(pointer)
        .and_then(|ty| pointer_storage.get(ty))
        .copied();
    assert_eq!(
        storage,
        Some(StorageClass::PhysicalStorageBuffer),
        "the atomic runs on the device address the field held, not on a Private placeholder"
    );
}

#[test]
fn both_arms_of_a_merge_take_the_element_type_the_merged_gep_names() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %bytes, ptr addrspace(1) %words, ptr addrspace(1) %out, i32 %gid) {
entry:
  %c = icmp eq i32 %gid, 0
  %alias = bitcast ptr addrspace(1) %bytes to ptr addrspace(1)
  %sel = select i1 %c, ptr addrspace(1) %alias, ptr addrspace(1) %words
  %i = zext i32 %gid to i64
  %p = getelementptr inbounds i32, ptr addrspace(1) %sel, i64 %i
  %v = load i32, ptr addrspace(1) %p, align 4
  %o = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %i
  store i32 %v, ptr addrspace(1) %o, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"bytes"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"words"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_merge_arm_element_type_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let module = load_bytes(&spv).expect("load native spv");
    let _ = std::fs::remove_dir_all(&tmp);

    let int_widths: HashMap<Word, u32> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypeInt)
        .filter_map(|inst| match inst.operands.first() {
            Some(&Operand::LiteralBit32(bits)) => Some((inst.result_id?, bits)),
            _ => None,
        })
        .collect();
    let runtime_array_widths: Vec<u32> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypeRuntimeArray)
        .filter_map(|inst| match inst.operands.first() {
            Some(&Operand::IdRef(elem)) => int_widths.get(&elem).copied(),
            _ => None,
        })
        .collect();
    assert!(
        !runtime_array_widths.is_empty() && runtime_array_widths.iter().all(|&bits| bits == 32),
        "both merged arms are 32-bit runtime arrays, saw {runtime_array_widths:?}"
    );

    let body: Vec<&Instruction> = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .collect();
    let index_of = |id: Word| -> Option<Word> {
        body.iter()
            .find(|inst| inst.result_id == Some(id))
            .filter(|inst| matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain))
            .and_then(|inst| match inst.operands.last() {
                Some(&Operand::IdRef(index)) => Some(index),
                _ => None,
            })
    };
    let selects: Vec<&&Instruction> = body
        .iter()
        .filter(|inst| inst.class.opcode == Op::Select)
        .filter(|inst| inst.result_type.and_then(|ty| int_widths.get(&ty)) == Some(&32))
        .collect();
    assert_eq!(selects.len(), 1, "the merge is one value-domain select");
    let arm_indices: Vec<Option<Word>> = selects[0].operands[1..]
        .iter()
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .map(|id| {
            body.iter()
                .find(|inst| inst.result_id == Some(id))
                .filter(|inst| inst.class.opcode == Op::Load)
                .and_then(|inst| match inst.operands.first() {
                    Some(&Operand::IdRef(ptr)) => index_of(ptr),
                    _ => None,
                })
        })
        .collect();
    assert!(
        arm_indices[0].is_some() && arm_indices[0] == arm_indices[1],
        "both arms load at the same index, saw {arm_indices:?}"
    );
}

#[test]
fn an_identity_bitcast_of_a_pointer_merge_keeps_the_merge() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %out, i32 %gid) {
entry:
  %c = icmp eq i32 %gid, 0
  %sel = select i1 %c, ptr addrspace(1) %a, ptr addrspace(1) %b
  %alias = bitcast ptr addrspace(1) %sel to ptr addrspace(1)
  %i = zext i32 %gid to i64
  %p = getelementptr inbounds i32, ptr addrspace(1) %alias, i64 %i
  %v = load i32, ptr addrspace(1) %p, align 4
  %o = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %i
  store i32 %v, ptr addrspace(1) %o, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_merge_alias_bitcast_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let module = load_bytes(&spv).expect("load native spv");
    let _ = std::fs::remove_dir_all(&tmp);

    let body: Vec<&Instruction> = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .collect();

    let pointer_types: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| inst.result_id)
        .collect();
    assert!(
        !body.iter().any(|inst| inst.class.opcode == Op::Select
            && inst
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))),
        "no pointer select survives"
    );
    let root_of = |mut id: Word| -> Option<Word> {
        loop {
            let def = body.iter().find(|inst| inst.result_id == Some(id));
            match def {
                Some(inst)
                    if matches!(
                        inst.class.opcode,
                        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                    ) =>
                {
                    match inst.operands.first() {
                        Some(&Operand::IdRef(base)) => id = base,
                        _ => return None,
                    }
                }
                _ => return Some(id),
            }
        }
    };
    let selects: Vec<&&Instruction> = body
        .iter()
        .filter(|inst| inst.class.opcode == Op::Select)
        .collect();
    assert_eq!(selects.len(), 1, "the merge is one value-domain select");
    let arm_roots: Vec<Option<Word>> = selects[0].operands[1..]
        .iter()
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .map(|id| {
            body.iter()
                .find(|inst| inst.result_id == Some(id))
                .filter(|inst| inst.class.opcode == Op::Load)
                .and_then(|inst| match inst.operands.first() {
                    Some(&Operand::IdRef(ptr)) => root_of(ptr),
                    _ => None,
                })
        })
        .collect();
    assert!(
        arm_roots.iter().all(Option::is_some) && arm_roots[0] != arm_roots[1],
        "each arm loads from its own descriptor root, saw {arm_roots:?}"
    );
}

#[test]
fn native_loop_carried_device_address_phi_reserves_the_loaded_address() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %root, i32 %tid) {
entry:
  %slot = zext i32 %tid to i64
  br label %loop

loop:
  %cur = phi ptr addrspace(1) [ %root, %entry ], [ %next, %body ]
  %i = phi i32 [ 0, %entry ], [ %i1, %body ]
  %done = icmp sge i32 %i, 4
  br i1 %done, label %exit, label %body

body:
  %child = load ptr addrspace(1), ptr addrspace(1) %cur, align 8
  %next = getelementptr inbounds i32, ptr addrspace(1) %child, i64 2
  %i1 = add i32 %i, 1
  br label %loop

exit:
  %v = load i32, ptr addrspace(1) %cur, align 4
  %op = getelementptr inbounds i32, ptr addrspace(1) %out, i64 %slot
  store i32 %v, ptr addrspace(1) %op, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"uint*", !"air.arg_name", !"root"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_loop_carried_device_address_phi_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");

    // The address phi is the loop header's: a selection-merge phi elsewhere (the null-checked load helper's)
    // also has two 64-bit incoming values, and picking among `defs.values()` made this test depend on hash order.
    let mut defs: HashMap<String, (String, String, Vec<String>)> = HashMap::new();
    let mut type_decl: HashMap<String, String> = HashMap::new();
    let mut returns: HashMap<String, Vec<String>> = HashMap::new();
    let mut header_phis: Vec<String> = Vec::new();
    let (mut function, mut block_phis) = (String::new(), Vec::new());
    for line in asm.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["OpLoopMerge", ..] => header_phis.append(&mut block_phis),
            ["OpReturnValue", value] => returns
                .entry(function.clone())
                .or_default()
                .push(value.to_string()),
            _ => {}
        }
        let [result, "=", opcode, rest @ ..] = words.as_slice() else {
            continue;
        };
        match *opcode {
            "OpFunction" => function = result.to_string(),
            "OpLabel" => block_phis.clear(),
            "OpPhi" => block_phis.push(result.to_string()),
            _ => {}
        }
        if opcode.starts_with("OpType") {
            type_decl.insert(result.to_string(), format!("{opcode} {}", rest.join(" ")));
            continue;
        }
        let [ty, operands @ ..] = rest else {
            continue;
        };
        let operands = operands
            .iter()
            .filter(|word| word.starts_with('%'))
            .map(|word| word.to_string())
            .collect();
        defs.insert(
            result.to_string(),
            (opcode.to_string(), ty.to_string(), operands),
        );
    }
    let address_type = type_decl
        .iter()
        .find(|(_, decl)| decl.as_str() == "OpTypeInt 64 0")
        .map(|(id, _)| id.clone())
        .expect("a 64-bit integer type");

    let address_phis: Vec<&String> = header_phis
        .iter()
        .filter(|id| {
            defs.get(*id).is_some_and(|(opcode, ty, operands)| {
                opcode == "OpPhi" && *ty == address_type && operands.len() == 4
            })
        })
        .collect();
    assert_eq!(
        address_phis.len(),
        1,
        "one 64-bit loop-carried address phi: {address_phis:?}"
    );
    let address_phi = address_phis[0].clone();
    let backedge = defs[&address_phi].2[2].clone();

    // Follow the backedge value down to the load, into the helpers it calls.
    let mut pending = vec![backedge];
    let mut seen: HashSet<String> = HashSet::new();
    let mut loaded_address = false;
    while let Some(id) = pending.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some((opcode, ty, operands)) = defs.get(&id) else {
            continue;
        };
        let declared = type_decl.get(ty).cloned().unwrap_or_default();
        assert!(
            !declared.starts_with("OpTypePointer") && declared != "OpTypeBool",
            "the loop-carried address cone reached {id} = {opcode} typed {declared}, \
             which is not an address"
        );
        if opcode == "OpLoad" {
            assert_eq!(
                declared, "OpTypeInt 64 0",
                "the child address must be loaded 64 bits wide, saw {id} = OpLoad {declared}"
            );
            loaded_address = true;
            continue;
        }
        if id == address_phi {
            continue;
        }
        if opcode == "OpFunctionCall" {
            let (callee, arguments) = operands.split_first().expect("a callee");
            pending.extend(returns.get(callee).into_iter().flatten().cloned());
            pending.extend(arguments.iter().cloned());
            continue;
        }
        pending.extend(operands.iter().cloned());
    }
    assert!(
        loaded_address,
        "the backedge address must derive from a 64-bit load of the child address"
    );
}

#[test]
fn native_pointee_typed_reinterpret_cast_takes_the_byte_view_path() {
    let typed = r#"
source_filename = "case.metal"

define void @k(i32 addrspace(1)* nocapture readonly "air-buffer-no-alias" %0, i32 addrspace(1)* nocapture writeonly "air-buffer-no-alias" %1, i32 %2) {
  %4 = bitcast i32 addrspace(1)* %0 to i8 addrspace(1)*
  %5 = getelementptr inbounds i32, i32 addrspace(1)* %0, i64 1
  %6 = bitcast i32 addrspace(1)* %5 to i16 addrspace(1)*
  %7 = zext i32 %2 to i64
  %8 = getelementptr inbounds i8, i8 addrspace(1)* %4, i64 %7
  %9 = load i8, i8 addrspace(1)* %8, align 1
  %10 = zext i8 %9 to i32
  %11 = getelementptr inbounds i16, i16 addrspace(1)* %6, i64 %7
  %12 = load i16, i16 addrspace(1)* %11, align 2
  %13 = zext i16 %12 to i32
  %14 = add nuw nsw i32 %13, %10
  %15 = getelementptr inbounds i32, i32 addrspace(1)* %1, i64 %7
  store i32 %14, i32 addrspace(1)* %15, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{void (i32 addrspace(1)*, i32 addrspace(1)*, i32)* @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"o"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"g"}
"#;
    let opaque = typed
        .replace("i32 addrspace(1)*", "ptr addrspace(1)")
        .replace("i8 addrspace(1)*", "ptr addrspace(1)")
        .replace("i16 addrspace(1)*", "ptr addrspace(1)")
        .replace("(ptr addrspace(1), ptr addrspace(1), i32)*", "");

    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_pointee_typed_reinterpret_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let from_typed =
        crate::translate_sanitized_native(typed, Stage::Kernel, &tmp).expect("typed pointers");
    let from_opaque =
        crate::translate_sanitized_native(&opaque, Stage::Kernel, &tmp).expect("opaque pointers");
    assert_eq!(from_typed, from_opaque);

    let asm = disassemble(&from_typed).expect("disassemble");
    assert_eq!(asm.matches("OpUDiv").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpVectorExtractDynamic").count(), 2, "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
    assert_eq!(asm.matches("Binding 0").count(), 1, "{asm}");
}

#[test]
fn a_constant_space_pointer_loaded_from_a_buffer_is_a_device_address() {
    let ll = r#"
%struct.inputs = type { ptr addrspace(1), ptr addrspace(2) }

define void @constant_nested_pointer(ptr addrspace(1) readonly align 8 captures(none) "air-buffer-no-alias" %0, i32 %1) {
entry:
  %2 = getelementptr inbounds %struct.inputs, ptr addrspace(1) %0, i64 0, i32 1
  %3 = load ptr addrspace(2), ptr addrspace(1) %2, align 8
  %4 = zext i32 %1 to i64
  %5 = getelementptr inbounds i32, ptr addrspace(2) %3, i64 %4
  %6 = load i32, ptr addrspace(2) %5, align 4
  %7 = getelementptr inbounds %struct.inputs, ptr addrspace(1) %0, i64 0, i32 0
  %8 = load ptr addrspace(1), ptr addrspace(1) %7, align 8
  %9 = zext i32 %6 to i64
  %10 = getelementptr inbounds i32, ptr addrspace(1) %8, i64 %9
  store i32 %6, ptr addrspace(1) %10, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @constant_nested_pointer, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"inputs", !"air.arg_name", !"in"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("the nested-pointer module must emit");
    let module = load_bytes(&spv).expect("load spv");
    let pointer_storage = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::StorageClass(class)) => Some((inst.result_id?, *class)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let physical_loads = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Load)
        .filter(|inst| match inst.operands.first() {
            Some(Operand::IdRef(pointer)) => {
                value_types
                    .get(pointer)
                    .and_then(|ty| pointer_storage.get(ty))
                    == Some(&StorageClass::PhysicalStorageBuffer)
            }
            _ => false,
        })
        .count();
    assert!(
        physical_loads > 0,
        "the read through the constant-space pointer must be an address load"
    );
    let nulls = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::ConstantNull)
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    for function in &module.functions {
        for inst in function.all_inst_iter() {
            for operand in &inst.operands {
                if let Operand::IdRef(id) = operand {
                    assert!(
                        !nulls.contains(id)
                            || (inst.class.opcode == Op::Phi
                                && function.blocks.iter().any(|block| block
                                    .instructions
                                    .iter()
                                    .any(
                                        |candidate| candidate.class.opcode == Op::BranchConditional
                                    ))
                                && function
                                    .all_inst_iter()
                                    .any(|candidate| candidate.class.opcode == Op::Load)),
                        "a null stands in for the value the shader read: {inst:?}"
                    );
                }
            }
        }
    }
    let guarded = module
        .functions
        .iter()
        .filter(|function| {
            function.parameters.len() == 2
                && function.blocks.len() == 3
                && function.blocks[0].instructions.iter().any(|instruction| {
                    instruction.class.opcode == Op::INotEqual
                        && instruction.operands.first()
                            == function.parameters[0]
                                .result_id
                                .as_ref()
                                .map(|id| Operand::IdRef(*id))
                                .as_ref()
                })
                && function.blocks[1]
                    .instructions
                    .iter()
                    .any(|instruction| instruction.class.opcode == Op::Load)
                && function.blocks[2]
                    .instructions
                    .iter()
                    .any(|instruction| instruction.class.opcode == Op::Phi)
        })
        .count();
    assert!(
        guarded > 0,
        "physical reads must guard the original base before applying its byte offset"
    );
}

#[test]
fn a_byte_blob_memcpy_between_a_cursor_and_a_local_reaches_the_buffer() {
    let ll = r#"
%struct.Row = type { i32, [12 x i8], [12 x i8] }

define void @byte_run_copy(ptr addrspace(1) captures(none) "air-buffer-no-alias" %out, i32 %tid) {
entry:
  %blob = alloca [12 x i8], align 4
  %p = getelementptr inbounds [12 x i8], ptr %blob, i64 0, i64 0
  %byte = getelementptr inbounds i8, ptr addrspace(1) %out, i64 4
  %seed = load i8, ptr addrspace(1) %byte, align 1
  %idx = zext i32 %tid to i64
  %row = getelementptr inbounds %struct.Row, ptr addrspace(1) %out, i64 %idx
  %srcp = getelementptr inbounds i8, ptr addrspace(1) %row, i64 4
  call void @llvm.memcpy.p0.p1.i64(ptr noundef nonnull align 4 %p, ptr addrspace(1) noundef align 4 %srcp, i64 12, i1 false)
  store i8 %seed, ptr %p, align 4
  %dstp = getelementptr inbounds i8, ptr addrspace(1) %row, i64 20
  call void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) noundef align 4 %dstp, ptr noundef nonnull align 4 %p, i64 12, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) noalias writeonly captures(none), ptr noalias readonly captures(none), i64, i1 immarg)
declare void @llvm.memcpy.p0.p1.i64(ptr noalias writeonly captures(none), ptr addrspace(1) noalias readonly captures(none), i64, i1 immarg)

!air.kernel = !{!0}
!0 = !{ptr @byte_run_copy, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("the blob-copy module must emit");
    let module = load_bytes(&spv).expect("load spv");
    let pointer_storage = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::StorageClass(class)) => Some((inst.result_id?, *class)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let buffer_class = module
        .all_inst_iter()
        .filter(|inst| matches!(inst.class.opcode, Op::Variable | Op::FunctionParameter))
        .filter_map(|inst| pointer_storage.get(&inst.result_type?).copied())
        .find(|class| !matches!(class, StorageClass::Private | StorageClass::Function))
        .expect("the entry declares a descriptor-backed buffer");
    let mut buffer_stores = 0;
    let mut local_stores = 0;
    for inst in module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Store)
    {
        let Some(Operand::IdRef(pointer)) = inst.operands.first() else {
            continue;
        };
        match value_types
            .get(pointer)
            .and_then(|ty| pointer_storage.get(ty))
        {
            Some(class) if *class == buffer_class => buffer_stores += 1,
            Some(StorageClass::Function) => local_stores += 1,
            other => panic!("a copied byte landed in {other:?}"),
        }
    }
    assert_eq!(
        (buffer_stores, local_stores),
        (3, 13),
        "both directions of the blob copy must reach their destination"
    );
}

#[test]
fn a_struct_memcpy_through_a_dynamic_cursor_reaches_the_buffer() {
    let ll = r#"
%struct.matrix = type { [4 x <3 x float>] }

define void @memcpy_dynamic_cursor(ptr addrspace(1) captures(none) "air-buffer-no-alias" %out, i32 %tid) {
entry:
  %local = alloca %struct.matrix, align 16
  %byte = getelementptr inbounds i8, ptr addrspace(1) %out, i64 4
  %seed = load i8, ptr addrspace(1) %byte, align 1
  %f = uitofp i8 %seed to float
  %v = insertelement <3 x float> <float 1.000000e+00, float 2.000000e+00, float poison>, float %f, i64 2
  %r0 = getelementptr inbounds %struct.matrix, ptr %local, i64 0, i32 0, i64 0
  store <3 x float> %v, ptr %r0, align 16
  %r1 = getelementptr inbounds %struct.matrix, ptr %local, i64 0, i32 0, i64 1
  store <3 x float> %v, ptr %r1, align 16
  %r2 = getelementptr inbounds %struct.matrix, ptr %local, i64 0, i32 0, i64 2
  store <3 x float> %v, ptr %r2, align 16
  %r3 = getelementptr inbounds %struct.matrix, ptr %local, i64 0, i32 0, i64 3
  store <3 x float> %v, ptr %r3, align 16
  %idx = zext i32 %tid to i64
  %dst = getelementptr inbounds %struct.matrix, ptr addrspace(1) %out, i64 %idx
  call void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) noundef align 16 %dst, ptr noundef nonnull align 16 %local, i64 64, i1 false)
  ret void
}

declare void @llvm.memcpy.p1.p0.i64(ptr addrspace(1) noalias writeonly captures(none), ptr noalias readonly captures(none), i64, i1 immarg)

!air.kernel = !{!0}
!0 = !{ptr @memcpy_dynamic_cursor, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 64, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"matrix", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("the memcpy module must emit");
    let module = load_bytes(&spv).expect("load spv");
    assert!(
        !module
            .all_inst_iter()
            .any(|inst| inst.class.opcode == Op::CopyMemory),
        "a whole-object copy has nowhere to land: the destination is a byte cursor"
    );
    let pointer_storage = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::StorageClass(class)) => Some((inst.result_id?, *class)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let buffer_class = module
        .all_inst_iter()
        .filter(|inst| matches!(inst.class.opcode, Op::Variable | Op::FunctionParameter))
        .filter_map(|inst| pointer_storage.get(&inst.result_type?).copied())
        .find(|class| !matches!(class, StorageClass::Private | StorageClass::Function))
        .expect("the entry declares a descriptor-backed buffer");
    let store_classes = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Store)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(pointer)) => {
                pointer_storage.get(value_types.get(pointer)?).copied()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        !store_classes.contains(&StorageClass::Private),
        "no word of the copy may land in the placeholder that stands in for the cursor"
    );
    let buffer_stores = store_classes
        .iter()
        .filter(|class| **class == buffer_class)
        .count();
    assert_eq!(
        buffer_stores, 12,
        "every data word of the copy must reach the buffer"
    );
}

#[test]
fn a_dynamic_raw_cursor_reaches_the_buffer_by_splicing_the_call_away() {
    let ll = r#"
define void @dyn_cursor_helper(ptr addrspace(1) %out, i32 %tid) {
entry:
  %byte = getelementptr inbounds i8, ptr addrspace(1) %out, i64 4
  %seed = load i32, ptr addrspace(1) %byte, align 4
  %idx = zext i32 %tid to i64
  %cursor = getelementptr inbounds <4 x float>, ptr addrspace(1) %out, i64 %idx
  tail call fastcc void @store_helper(ptr addrspace(1) %cursor, i32 %seed)
  ret void
}

define internal fastcc void @store_helper(ptr addrspace(1) %p, i32 %k) {
entry:
  %c = icmp ugt i32 %k, 0
  br i1 %c, label %do, label %done

do:
  store <4 x float> <float 1.000000e+00, float 2.000000e+00, float 3.000000e+00, float 4.000000e+00>, ptr addrspace(1) %p, align 16
  br label %done

done:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @dyn_cursor_helper, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let spv = emit_vulkan_spirv(ll).expect("the spliced module must emit");
    let module = load_bytes(&spv).expect("load spv");
    assert!(
        !module
            .all_inst_iter()
            .any(|inst| inst.class.opcode == Op::FunctionCall),
        "the refused call must be spliced away, not emitted"
    );
    let pointer_storage = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypePointer)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::StorageClass(class)) => Some((inst.result_id?, *class)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let store_classes = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Store)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(pointer)) => {
                Some(pointer_storage.get(value_types.get(pointer)?).copied()?)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let buffer_class = module
        .all_inst_iter()
        .filter(|inst| matches!(inst.class.opcode, Op::Variable | Op::FunctionParameter))
        .filter_map(|inst| pointer_storage.get(&inst.result_type?).copied())
        .find(|class| !matches!(class, StorageClass::Private | StorageClass::Function))
        .expect("the entry declares a descriptor-backed buffer");
    assert_eq!(
        store_classes,
        vec![buffer_class; 4],
        "every component of the helper's store must reach the buffer, not Private scratch"
    );

    let external_helper = ll.replace(
        "define internal fastcc void @store_helper",
        "define fastcc void @store_helper",
    );
    let error = emit_vulkan_spirv(&external_helper)
        .expect_err("a cursor into an unspliceable helper must not emit a module");
    assert!(
        error.contains("byte cursor cannot cross the call"),
        "refusal must name the cursor, got: {error}"
    );

    let unused = r#"
define void @dyn_cursor_unused(ptr addrspace(1) %out, i32 %tid) {
entry:
  %byte = getelementptr inbounds i8, ptr addrspace(1) %out, i64 4
  %seed = load i32, ptr addrspace(1) %byte, align 4
  %idx = zext i32 %tid to i64
  %cursor = getelementptr inbounds <4 x float>, ptr addrspace(1) %out, i64 %idx
  %flag = tail call fastcc i32 @classify(ptr addrspace(1) %cursor, i32 %seed)
  %wide = insertelement <4 x i32> undef, i32 %flag, i64 0
  %bits = bitcast <4 x i32> %wide to <4 x float>
  store <4 x float> %bits, ptr addrspace(1) %out, align 16
  ret void
}

define internal fastcc i32 @classify(ptr addrspace(1) %p, i32 %k) {
entry:
  %c = icmp ugt i32 %k, 0
  br i1 %c, label %yes, label %no

yes:
  %isnull = icmp eq ptr addrspace(1) %p, null
  %v = zext i1 %isnull to i32
  ret i32 %v

no:
  ret i32 7
}

!air.kernel = !{!0}
!0 = !{ptr @dyn_cursor_unused, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let spv = emit_vulkan_spirv(unused).expect("an undereferenced placeholder must still emit");
    let module = load_bytes(&spv).expect("load spv");
    let private_pointers = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::TypePointer
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Private))
        })
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let stores = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Store)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(pointer)) => Some(*pointer),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores.len(), 4, "the entry's own vector store must survive");
    assert!(
        stores.iter().all(|pointer| !value_types
            .get(pointer)
            .is_some_and(|ty| private_pointers.contains(ty))),
        "no store may land in Private scratch when the placeholder is never dereferenced"
    );
}

#[test]
fn native_helper_alloca_keeps_parameter_pointer_chain_real() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%ArrayRef = type { ptr, i64 }
%InOut = type { %ArrayRef }
%Arrays = type <{ %InOut, i32, [4 x i8] }>
@buffer_count = internal addrspace(2) global i32 3

define void @k(ptr %buffers, <3 x i32> %tid) {
entry:
  %arrays = alloca %Arrays, align 8
  %data_field = getelementptr inbounds %Arrays, ptr %arrays, i64 0, i32 0, i32 0, i32 0
  store ptr %buffers, ptr %data_field, align 8
  %inout = getelementptr inbounds %Arrays, ptr %arrays, i64 0, i32 0
  %i = extractelement <3 x i32> %tid, i64 0
  %v = call float @read_two(ptr %inout, i32 %i)
  %out_slot = getelementptr inbounds ptr addrspace(1), ptr %buffers, i64 1
  %out = load ptr addrspace(1), ptr %out_slot, align 8
  %idx = zext i32 %i to i64
  %dst = getelementptr inbounds float, ptr addrspace(1) %out, i64 %idx
  store float %v, ptr addrspace(1) %dst, align 4
  ret void
}

define float @read_two(ptr %io, i32 %i) {
entry:
  %slot = alloca ptr, align 8
  store ptr %io, ptr %slot, align 8
  %first = bitcast ptr %io to ptr
  %data = load ptr, ptr %first, align 8
  %b0 = load ptr addrspace(1), ptr %data, align 8
  %idx = zext i32 %i to i64
  %p0 = getelementptr inbounds float, ptr addrspace(1) %b0, i64 %idx
  %x0 = load float, ptr addrspace(1) %p0, align 4
  %again = load ptr, ptr %slot, align 8
  %field = getelementptr inbounds %InOut, ptr %again, i64 0, i32 0, i32 0
  %data2 = load ptr, ptr %field, align 8
  %b2_slot = getelementptr inbounds ptr addrspace(1), ptr %data2, i64 2
  %b2 = load ptr addrspace(1), ptr %b2_slot, align 8
  %p2 = getelementptr inbounds float, ptr addrspace(1) %b2, i64 %idx
  %x2 = load float, ptr addrspace(1) %p2, align 4
  %r = fsub float %x0, %x2
  ret float %r
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @buffer_count, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_helper_alloca_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load translated module");
    let nulls = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::ConstantNull)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<Word>>();
    let float_types = module
        .types_global_values
        .iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::TypeFloat
                && matches!(
                    instruction.operands.first(),
                    Some(Operand::LiteralBit32(32))
                )
        })
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<Word>>();
    let null_copies = module
        .all_inst_iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::CopyObject
                && matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if nulls.contains(id))
        })
        .count();
    let float_loads = module
        .all_inst_iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::Load
                && instruction
                    .result_type
                    .is_some_and(|ty| float_types.contains(&ty))
        })
        .count();
    assert_eq!(
        (null_copies, float_loads),
        (0, 2),
        "(null-folded copies, real float loads): both helper reads must survive"
    );
}

#[test]
fn native_query_structure_loaded_from_an_argument_buffer_field_converts_words_4_and_5() {
    fn asm_of(spv: &[u8]) -> String {
        disassemble(spv)
            .expect("disassemble")
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }
    let ll = r#"
%"struct.metal::raytracing::_acceleration_structure" = type { %struct._instance_acceleration_structure_t addrspace(1)* }
%struct._instance_acceleration_structure_t = type opaque
%struct._intersection_function_table_t = type opaque
%struct.Args2 = type { i32, float, %"struct.metal::raytracing::_acceleration_structure", %"struct.metal::raytracing::_acceleration_structure" }

define void @k(%struct.Args2 addrspace(2)* nocapture noundef readonly align 8 dereferenceable(24) "air-buffer-no-alias" %0, i32 addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %1) local_unnamed_addr {
  %3 = getelementptr inbounds %struct.Args2, %struct.Args2 addrspace(2)* %0, i64 0, i32 3, i32 0
  %4 = load %struct._instance_acceleration_structure_t addrspace(1)*, %struct._instance_acceleration_structure_t addrspace(1)* addrspace(2)* %3, align 8
  %5 = tail call %struct._intersection_function_table_t addrspace(1)* @air.get_null_intersection_function_table()
  %6 = tail call { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float> <float 0.000000e+00, float 0.000000e+00, float -1.000000e+00>, <3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, float 0.000000e+00, float 1.000000e+02, %struct._instance_acceleration_structure_t addrspace(1)* readonly %4, i32 255, %struct._intersection_function_table_t addrspace(1)* readonly %5, i8* null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 3, i32 -1, i32 -1, i32 0, i1 false, i1 false)
  %7 = extractvalue { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, <2 x float>, i1 } %6, 0
  store i32 %7, i32 addrspace(1)* %1, align 4
  ret void
}

declare %struct._intersection_function_table_t addrspace(1)* @air.get_null_intersection_function_table() local_unnamed_addr
declare { i32, float, i32, i32, i8 addrspace(1)*, i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float>, <3 x float>, float, float, %struct._instance_acceleration_structure_t addrspace(1)* readonly, i32, %struct._intersection_function_table_t addrspace(1)* readonly, i8*, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1) local_unnamed_addr

!air.kernel = !{!0}
!0 = !{void (%struct.Args2 addrspace(2)*, i32 addrspace(1)*)* @k, !1, !2}
!1 = !{}
!2 = !{!3, !9}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 24, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_size", i32 24, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"Args2", !"air.arg_name", !"a"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"pad", !"air.indirect_argument", !5, i32 4, i32 4, i32 0, !"float", !"pad2", !"air.indirect_argument", !6, i32 8, i32 8, i32 0, !"acceleration_structure<instancing>", !"a", !"air.indirect_argument", !7, i32 16, i32 8, i32 0, !"acceleration_structure<instancing>", !"b", !"air.indirect_argument", !8}
!5 = !{i32 0, !"air.indirect_constant", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"pad"}
!6 = !{i32 1, !"air.indirect_constant", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"pad2"}
!7 = !{i32 2, !"air.instance_acceleration_structure", !"air.location_index", i32 2, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"a"}
!8 = !{i32 3, !"air.instance_acceleration_structure", !"air.location_index", i32 3, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"b"}
!9 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_argbuf_structure_field_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = asm_of(&spv);
    assert!(
        asm.contains("OpConvertUToAccelerationStructureKHR"),
        "{asm}"
    );
    assert!(asm.contains("OpRayQueryInitializeKHR"), "{asm}");
    let uint_ty = asm
        .lines()
        .find(|l| l.ends_with("= OpTypeInt 32 0"))
        .and_then(|l| l.split_whitespace().next())
        .expect("uint type")
        .to_string();
    let constant = |value: u32| -> Option<String> {
        asm.lines()
            .find(|l| l.ends_with(&format!("= OpConstant {uint_ty} {value}")))
            .and_then(|l| l.split_whitespace().next())
            .map(str::to_string)
    };
    let chain = |word: u32| -> bool {
        let (Some(c0), Some(cw)) = (constant(0), constant(word)) else {
            return false;
        };
        asm.lines()
            .any(|l| l.contains("= OpAccessChain ") && l.ends_with(&format!(" {c0} {cw}")))
    };
    assert!(
        chain(4) && chain(5),
        "expected word chains [0, 4] and [0, 5]\n{asm}"
    );
    assert!(
        !chain(2) && !chain(3),
        "field `a` (words 2, 3) must not be read\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
}
