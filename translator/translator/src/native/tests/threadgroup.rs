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
use crate::fxhash::{HashMap, HashSet};
use crate::passes::{self, Stage};
use crate::spirv_module::load_bytes;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Instruction};
use crate::{disassemble, meta, tools};
use spirv::{Decoration, Op, Scope, SelectionControl, StorageClass, Word};
use std::path::PathBuf;

#[test]
fn fragment_quad_active_mask_and_narrow_popcount_are_vulkan_valid() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define <4 x half> @frag() {
entry:
  %mask = call i16 @air.quad_active_threads_mask()
  %quad = and i16 %mask, 15
  %count = call i16 @air.popcount.i16(i16 %quad)
  %value = uitofp i16 %count to half
  %v0 = insertelement <4 x half> poison, half %value, i32 0
  %v1 = shufflevector <4 x half> %v0, <4 x half> poison, <4 x i32> zeroinitializer
  ret <4 x half> %v1
}

declare i16 @air.quad_active_threads_mask()
declare i16 @air.popcount.i16(i16)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!3 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_quad_active_mask_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpGroupNonUniformBallot"), "{asm}");
    let bitcount = asm
        .lines()
        .find(|line| line.contains("OpBitCount"))
        .expect("OpBitCount");
    assert!(bitcount.contains(&uint32_type_id(&asm)), "{asm}");
    assert!(asm.contains("Flat"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_workgroup_array_vector_load_uses_first_element() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@tile = internal unnamed_addr addrspace(3) global [256 x <4 x half>] undef, align 8

define void @k(ptr addrspace(1) %out) {
entry:
  %v = load <4 x half>, ptr addrspace(3) @tile, align 8
  store <4 x half> %v, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_workgroup_array_vec_load_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpBitcast"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_ushort2_threads_per_threadgroup_uses_specialized_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(<2 x i16> %lsize) {
entry:
  %x = extractelement <2 x i16> %lsize, i32 0
  %ok = icmp uge i16 %x, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"lsize"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_ushort2_threads_per_threadgroup_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSpecConstantComposite"), "{asm}");
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
fn native_simd_shuffle_and_barrier_lower_to_subgroup_ops() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, i16 %lane) {
entry:
  %out = tail call i32 @air.simd_shuffle.u.i32(i32 %v, i16 %lane)
  %signed = tail call i32 @air.simd_shuffle.s.i32(i32 %out, i16 %lane)
  %ok = icmp uge i32 %signed, 0
  tail call void @air.simdgroup.barrier(i32 0, i32 4)
  ret void
}

declare i32 @air.simd_shuffle.u.i32(i32, i16)
declare i32 @air.simd_shuffle.s.i32(i32, i16)
declare void @air.simdgroup.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_simd_shuffle_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        whole_workgroup_options(),
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert_eq!(asm.matches("OpGroupNonUniformShuffle").count(), 2, "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpControlBarrier"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_quad_sum_lowers_to_shuffle_xor_butterfly() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %v, ptr addrspace(1) %out) {
entry:
  %sum = tail call float @air.quad_sum.f32(float %v)
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.quad_sum.f32(float)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_quad_sum_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(
        !asm.contains("OpCapability GroupNonUniformQuad"),
        "quad_sum must not require the GroupNonUniformQuad capability: {asm}"
    );
    assert_eq!(
        asm.matches("OpGroupNonUniformShuffleXor").count(),
        2,
        "{asm}"
    );
    assert_eq!(asm.matches("OpFAdd").count(), 2, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_quad_integer_extrema_stay_inside_aligned_quad() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, ptr addrspace(1) %out) {
entry:
  %max = tail call i32 @air.quad_max.u.i32(i32 %v)
  %min = tail call i32 @air.quad_min.u.i32(i32 %v)
  %sum = add i32 %max, %min
  store i32 %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.quad_max.u.i32(i32)
declare i32 @air.quad_min.u.i32(i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_quad_integer_extrema_{}",
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
    assert_eq!(
        asm.matches("OpGroupNonUniformShuffleXor").count(),
        4,
        "{asm}"
    );
    assert_eq!(asm.matches("OpUGreaterThan").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpULessThan").count(), 2, "{asm}");
    assert!(!asm.contains("OpGroupNonUniformUMax"), "{asm}");
    assert!(!asm.contains("OpGroupNonUniformUMin"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_simd_sum_uses_metal_32_lane_cluster() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %v, ptr addrspace(1) %out) {
entry:
  %sum = tail call float @air.simd_sum.f32(float %v)
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_sum.f32(float)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_simd_sum_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);

    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpGroupNonUniformFAdd"), "{asm}");
    assert!(asm.contains("ClusteredReduce"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformClustered"),
        "{asm}"
    );
}

#[test]
fn native_air_simd_is_first_selects_each_32_lane_partition() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {
entry:
  %first = tail call i1 @air.simd_is_first()
  %word = select i1 %first, i32 1, i32 0
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i1 @air.simd_is_first()

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_is_first_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(asm.contains("BuiltIn SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_quad_is_first_selects_each_four_lane_partition() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {
entry:
  %first = tail call i1 @air.quad_is_first()
  %word = select i1 %first, i32 1, i32 0
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i1 @air.quad_is_first()

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_quad_is_first_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(asm.contains("BuiltIn SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.trim_end().ends_with(" 3")),
        "{asm}"
    );
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
fn native_air_quad_all_lowers_to_four_lane_xor_butterfly() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, ptr addrspace(1) %out) {
entry:
  %ok = icmp ne i32 %v, 0
  %all = tail call i1 @air.quad_all(i1 %ok)
  %word = select i1 %all, i32 1, i32 0
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i1 @air.quad_all(i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_quad_all_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(asm.contains("OpCapability GroupNonUniformShuffle"), "{asm}");
    assert!(!asm.contains("OpCapability GroupNonUniformVote"), "{asm}");
    assert_eq!(
        asm.matches("OpGroupNonUniformShuffleXor").count(),
        2,
        "{asm}"
    );
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_quad_any_lowers_to_four_lane_xor_butterfly() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, ptr addrspace(1) %out) {
entry:
  %ok = icmp ne i32 %v, 0
  %any = tail call i1 @air.quad_any(i1 %ok)
  %word = select i1 %any, i32 1, i32 0
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i1 @air.quad_any(i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_quad_any_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(asm.contains("OpCapability GroupNonUniformShuffle"), "{asm}");
    assert!(!asm.contains("OpCapability GroupNonUniformVote"), "{asm}");
    assert_eq!(
        asm.matches("OpGroupNonUniformShuffleXor").count(),
        2,
        "{asm}"
    );
    assert!(asm.contains("OpBitwiseOr"), "{asm}");
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
fn air_simd_any_all_vote_over_the_lane_s_own_thirty_two_lane_partition() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, ptr addrspace(1) %out) {
entry:
  %ok = icmp ne i32 %v, 0
  %any = tail call i1 @air.simd_any(i1 %ok)
  %all = tail call i1 @air.simd_all(i1 %ok)
  %any_word = select i1 %any, i32 1, i32 0
  %all_word = select i1 %all, i32 2, i32 0
  %word = or i32 %any_word, %all_word
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i1 @air.simd_any(i1)
declare i1 @air.simd_all(i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_any_all_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(asm.contains("OpCapability GroupNonUniformBallot"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformBallot"), "{asm}");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(asm.contains("OpINotEqual"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(!asm.contains("OpGroupNonUniformAny"), "{asm}");
    assert!(!asm.contains("OpGroupNonUniformAll"), "{asm}");
    assert!(!asm.contains("OpCapability GroupNonUniformVote"), "{asm}");
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
fn air_simd_ballot_i64_is_the_lane_s_own_thirty_two_lane_partition_word() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, ptr addrspace(1) %out) {
entry:
  %ok = icmp ne i32 %v, 0
  %mask = tail call i64 @air.simd_ballot.i64(i1 %ok)
  %lo = trunc i64 %mask to i32
  store i32 %lo, ptr addrspace(1) %out, align 4
  ret void
}

declare i64 @air.simd_ballot.i64(i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_ballot_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(asm.contains("OpCapability GroupNonUniformBallot"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformBallot"), "{asm}");
    assert!(asm.contains("OpVectorExtractDynamic"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(!asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(!asm.contains("OpBitwiseOr"), "{asm}");
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
fn native_air_get_simdgroup_size_i16_lowers_to_width_constant() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {
entry:
  %width16 = tail call i16 @air.get_simdgroup_size.i16()
  %width = zext i16 %width16 to i32
  store i32 %width, ptr addrspace(1) %out, align 4
  ret void
}

declare i16 @air.get_simdgroup_size.i16()

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simdgroup_size_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.trim_end().ends_with(" 32")),
        "{asm}"
    );
    assert!(
        !asm.contains("air.get_simdgroup_size"),
        "intrinsic call survived lowering:\n{asm}"
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
fn native_air_simd_shuffle_and_fill_up_tests_the_unreduced_delta() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(half %x, half %fill, i16 %delta, i16 %width, ptr addrspace(1) %out) {
entry:
  %up = tail call half @air.simd_shuffle_and_fill_up.f16(half %x, half %fill, i16 %delta, i16 %width)
  %f = fpext half %up to float
  store float %f, ptr addrspace(1) %out, align 4
  ret void
}

declare half @air.simd_shuffle_and_fill_up.f16(half, half, i16, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 4, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_fill_up_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let module = load_bytes(&spv).expect("load spv");
    let defs = module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst)))
        .collect::<crate::fxhash::HashMap<_, _>>();
    let operand = |inst: &Instruction, index: usize| match inst.operands.get(index) {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    };
    let condition = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::Select)
        .filter_map(|inst| operand(inst, 0))
        .filter_map(|id| defs.get(&id).copied())
        .find(|inst| inst.class.opcode == Op::UGreaterThanEqual)
        .unwrap_or_else(|| panic!("the data-vs-fill select has no lane bound test:\n{spv:?}"));
    let bound = operand(condition, 1).and_then(|id| defs.get(&id).copied());
    assert!(
        bound.is_none_or(|inst| inst.class.opcode != Op::UMod),
        "the lane is compared against a reduced delta, so a delta of one whole cluster leaves \
         every lane in bounds instead of none of them"
    );
    assert!(
        module
            .all_inst_iter()
            .any(|inst| inst.class.opcode == Op::UMod),
        "the wrap still needs the reduced delta"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_air_simd_shuffle_and_fill_down_lowers_to_clustered_subgroup_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(half %x, half %fill, i16 %delta, ptr addrspace(1) %out) {
entry:
  %width = tail call i16 @air.get_simdgroup_size.i16()
  %down = tail call half @air.simd_shuffle_and_fill_down.f16(half %x, half %fill, i16 %delta, i16 %width)
  %f = fpext half %down to float
  store float %f, ptr addrspace(1) %out, align 4
  ret void
}

declare i16 @air.get_simdgroup_size.i16()
declare half @air.simd_shuffle_and_fill_down.f16(half, half, i16, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_fill_down_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert!(asm.contains("SubgroupLocalInvocationId"), "{asm}");
    assert_eq!(
        asm.lines()
            .filter(|line| line.contains("= OpGroupNonUniformShuffle "))
            .count(),
        2,
        "{asm}"
    );
    assert!(asm.contains("OpUMod"), "{asm}");
    assert!(asm.contains("OpISub"), "{asm}");
    assert!(asm.contains("OpULessThan"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(
        !asm.contains("air.simd_shuffle_and_fill_down"),
        "intrinsic call survived lowering:\n{asm}"
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
fn native_air_simd_broadcast_lowers_to_subgroup_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, i16 %lane, ptr addrspace(1) %out) {
entry:
  %sx = tail call float @air.simd_broadcast.f32(float %x, i16 %lane)
  store float %sx, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_broadcast.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_broadcast_{}",
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
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_shuffle_lowers_to_the_callers_own_simdgroup_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, i16 %lane, ptr addrspace(1) %out) {
entry:
  %sx = tail call float @air.simd_shuffle.f32(float %x, i16 %lane)
  store float %sx, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_shuffle.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_shuffle_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_shuffle_resolves_the_same_lane_for_every_element_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, float %f, i16 %lane, ptr addrspace(1) %out) {
entry:
  %si = tail call i32 @air.simd_shuffle.u.i32(i32 %v, i16 %lane)
  %ss = tail call i32 @air.simd_shuffle.s.i32(i32 %si, i16 %lane)
  %sf = tail call float @air.simd_shuffle.f32(float %f, i16 %lane)
  %fi = sitofp i32 %ss to float
  %sum = fadd float %sf, %fi
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.simd_shuffle.u.i32(i32, i16)
declare i32 @air.simd_shuffle.s.i32(i32, i16)
declare float @air.simd_shuffle.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_lane_model_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpGroupNonUniformShuffle").count(), 3, "{asm}");

    let adds: Vec<&str> = asm
        .lines()
        .map(str::trim)
        .filter(|l| l.contains("= OpIAdd "))
        .map(|l| l.split_whitespace().next().expect("result id"))
        .collect();
    let mut lanes = Vec::new();
    for line in asm.lines().map(str::trim) {
        if let Some(rest) = line.split("= OpGroupNonUniformShuffle ").nth(1) {
            lanes.push(rest.split_whitespace().last().expect("lane operand"));
        }
    }
    assert_eq!(lanes.len(), 3, "{asm}");
    for lane in &lanes {
        assert!(
            adds.contains(lane),
            "lane {lane} is not an OpIAdd result\n{asm}"
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
fn native_air_simd_shuffle_down_lowers_to_32_lane_absolute_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, half %h, i16 %delta, ptr addrspace(1) %out) {
entry:
  %sx = tail call float @air.simd_shuffle_down.f32(float %x, i16 %delta)
  %sh = tail call half @air.simd_shuffle_down.f16(half %h, i16 %delta)
  %hf = fpext half %sh to float
  %sum = fadd float %sx, %hf
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_shuffle_down.f32(float, i16)
declare half @air.simd_shuffle_down.f16(half, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_down_{}",
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
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert_eq!(asm.matches("OpGroupNonUniformShuffle").count(), 2, "{asm}");
    assert!(!asm.contains("OpGroupNonUniformShuffleDown"), "{asm}");
    assert!(asm.contains("SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
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
fn native_air_simd_shuffle_rotate_down_lowers_to_wrapping_subgroup_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, i16 %delta, ptr addrspace(1) %out) {
entry:
  %sx = tail call float @air.simd_shuffle_rotate_down.f32(float %x, i16 %delta)
  store float %sx, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_shuffle_rotate_down.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_rotate_down_{}",
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
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert_eq!(asm.matches("OpGroupNonUniformShuffle").count(), 1, "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(!asm.contains("air.simd_shuffle_rotate_down"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_shuffle_up_lowers_to_32_lane_absolute_shuffle() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %x, i16 %delta, ptr addrspace(1) %out) {
entry:
  %sx = tail call i32 @air.simd_shuffle_up.u.i32(i32 %x, i16 %delta)
  store i32 %sx, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.simd_shuffle_up.u.i32(i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"int", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_up_{}",
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
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(!asm.contains("OpGroupNonUniformShuffleUp"), "{asm}");
    assert!(asm.contains("SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpBitwiseAnd"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
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
fn native_air_simd_shuffle_xor_lowers_to_subgroup_shuffle_xor() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(half %x, i16 %mask, ptr addrspace(1) %out) {
entry:
  %sx = tail call half @air.simd_shuffle_xor.f16(half %x, i16 %mask)
  %f = fpext half %sx to float
  store float %f, ptr addrspace(1) %out, align 4
  ret void
}

declare half @air.simd_shuffle_xor.f16(half, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_shuffle_xor_{}",
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
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformShuffleXor"), "{asm}");
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
fn native_air_quad_shuffle_down_vector_lowers_to_subgroup_shuffle_down() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(<4 x float> %x, i16 %delta, ptr addrspace(1) %out) {
entry:
  %sx = tail call <4 x float> @air.quad_shuffle_down.v4f32(<4 x float> %x, i16 %delta)
  store <4 x float> %sx, ptr addrspace(1) %out, align 16
  ret void
}

declare <4 x float> @air.quad_shuffle_down.v4f32(<4 x float>, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_quad_shuffle_down_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffleRelative"),
        "{asm}"
    );
    assert_eq!(
        asm.matches("OpGroupNonUniformShuffleDown").count(),
        1,
        "{asm}"
    );
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
fn native_air_simd_prefix_exclusive_sum_lowers_to_subgroup_scan() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, ptr addrspace(1) %out) {
entry:
  %sum = tail call float @air.simd_prefix_exclusive_sum.f32(float %x)
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_prefix_exclusive_sum.f32(float)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_native_prefix_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformArithmetic"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformFAdd"), "{asm}");
    assert!(asm.contains("ExclusiveScan"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_vector_u16_prefix_exclusive_sum_lowers_componentwise() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(2) %input, ptr addrspace(1) %out) {
entry:
  %x = load <4 x i16>, ptr addrspace(2) %input, align 8
  %sum = tail call <4 x i16> @air.simd_prefix_exclusive_sum.u.v4i16(<4 x i16> %x)
  store <4 x i16> %sum, ptr addrspace(1) %out, align 8
  ret void
}

declare <4 x i16> @air.simd_prefix_exclusive_sum.u.v4i16(<4 x i16>)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"input"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"ushort4", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_vector_u16_prefix_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability Int16"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformIAdd"), "{asm}");
    assert!(asm.contains("ExclusiveScan"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_sum_lowers_to_subgroup_reduce() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, ptr addrspace(1) %out) {
entry:
  %sum = tail call float @air.simd_sum.f32(float %x)
  store float %sum, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_sum.f32(float)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_sum_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformClustered"),
        "{asm}"
    );
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformArithmetic"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformFAdd"), "{asm}");
    assert!(asm.contains("ClusteredReduce"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_or_i8_lowers_to_subgroup_bitwise_reduce() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i8 %x, ptr addrspace(1) %out) {
entry:
  %mask = tail call i8 @air.simd_or.u.i8(i8 %x)
  %wide = zext i8 %mask to i32
  store i32 %wide, ptr addrspace(1) %out, align 4
  ret void
}

declare i8 @air.simd_or.u.i8(i8)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_or_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformClustered"),
        "{asm}"
    );
    assert!(
        !asm_has_line(&asm, "OpCapability GroupNonUniformArithmetic"),
        "{asm}"
    );
    assert!(asm.contains("OpCapability Int8"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformBitwiseOr"), "{asm}");
    assert!(asm.contains("ClusteredReduce"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_simd_inclusive_sum_min_max_lower_to_subgroup_arithmetic() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(float %x, ptr addrspace(1) %out) {
entry:
  %sum = tail call float @air.simd_prefix_inclusive_sum.f32(float %x)
  %min = tail call float @air.simd_min.f32(float %x)
  %max = tail call float @air.simd_max.f32(float %x)
  %a = fadd fast float %sum, %min
  %b = fadd fast float %a, %max
  store float %b, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_prefix_inclusive_sum.f32(float)
declare float @air.simd_min.f32(float)
declare float @air.simd_max.f32(float)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simd_extrema_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability GroupNonUniform"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformArithmetic"),
        "{asm}"
    );
    assert!(asm.contains("OpGroupNonUniformFAdd"), "{asm}");
    assert!(asm.contains("InclusiveScan"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformFMin"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformFMax"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_air_signed_subgroup_extrema_take_their_comparison_from_the_air_name() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i16 %x, ptr addrspace(1) %out) {
entry:
  %smin = tail call i16 @air.simd_min.s.i16(i16 %x)
  %umin = tail call i16 @air.simd_min.u.i16(i16 %x)
  %smax = tail call i16 @air.simd_max.s.i16(i16 %x)
  %a = add i16 %smin, %umin
  %b = add i16 %a, %smax
  store i16 %b, ptr addrspace(1) %out, align 2
  ret void
}

declare i16 @air.simd_min.s.i16(i16)
declare i16 @air.simd_min.u.i16(i16)
declare i16 @air.simd_max.s.i16(i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"short", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_signed_simd_extrema_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpGroupNonUniformSMin"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformSMax"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformUMin"), "{asm}");
    assert!(!asm.contains("OpGroupNonUniformUMax"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_threads_per_grid_shares_num_workgroups_builtin() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(<2 x i32> %grid_size, <2 x i32> %group_count) {
entry:
  %sx = extractelement <2 x i32> %grid_size, i64 0
  %sy = extractelement <2 x i32> %grid_size, i64 1
  %gx = extractelement <2 x i32> %group_count, i64 0
  %gy = extractelement <2 x i32> %group_count, i64 1
  %sum0 = add i32 %sx, %sy
  %sum1 = add i32 %gx, %gy
  %sum = add i32 %sum0, %sum1
  %ok = icmp uge i32 %sum, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.threads_per_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"grid_size"}
!4 = !{i32 1, !"air.threadgroups_per_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"group_count"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threads_per_grid_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [32, 2, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::Workgroups),
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("LocalSize 32 2 1"), "{asm}");
    assert!(asm.contains("BuiltIn NumWorkgroups"), "{asm}");
    assert_eq!(asm.matches("BuiltIn NumWorkgroups").count(), 1, "{asm}");
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(!asm.contains("OpUndef"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.contains("32")),
        "{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.contains("2")),
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
fn native_kernel_threads_per_grid_accepts_exact_dispatch_threads_shape() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(<2 x i32> %grid_size) {
entry:
  %sx = extractelement <2 x i32> %grid_size, i64 0
  %sy = extractelement <2 x i32> %grid_size, i64 1
  %sum = add i32 %sx, %sy
  %ok = icmp uge i32 %sum, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.threads_per_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"grid_size"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_exact_threads_per_grid_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [5, 2, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::ThreadsFixed {
                threads_per_grid: [21, 3, 1],
            }),
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("BuiltIn WorkgroupSize"), "{asm}");
    assert_eq!(asm.matches("SpecId").count(), 3, "{asm}");
    assert!(asm.contains("PushConstant"), "{asm}");
    assert!(!asm.contains("BuiltIn NumWorkgroups"), "{asm}");
    assert!(!asm.contains("OpIMul"), "{asm}");
    assert_eq!(asm.matches("OpUGreaterThanEqual").count(), 1, "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_fixed_dispatch_threads_uses_specialized_boundary_regions() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_fixed_dispatch_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [8, 8, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::ThreadsFixed {
                threads_per_grid: [57, 9, 1],
            }),
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("BuiltIn WorkgroupSize"), "{asm}");
    assert_eq!(asm.matches("SpecId").count(), 3, "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");

    let divisible = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [8, 8, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::ThreadsFixed {
                threads_per_grid: [64, 16, 1],
            }),
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate divisible grid");
    let divisible_asm = disassemble(&divisible).expect("disassemble divisible grid");
    assert!(
        !divisible_asm.contains("OpBranchConditional"),
        "{divisible_asm}"
    );
    assert!(
        divisible_asm.contains("BuiltIn WorkgroupSize"),
        "{divisible_asm}"
    );
}

#[test]
fn native_kernel_dynamic_dispatch_threads_shares_reflected_push_constant_grid() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(<3 x i32> %grid_size) {
entry:
  %x = extractelement <3 x i32> %grid_size, i64 0
  %ok = icmp uge i32 %x, 0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.threads_per_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"grid_size"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_dynamic_dispatch_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let options = passes::TransformOptions {
        kernel_local_size: [8, 8, 1],
        kernel_dispatch: Some(crate::reflect::KernelDispatch::ThreadsDynamic { offset: 16 }),
        ..passes::TransformOptions::default()
    };
    let (spv, reflection) =
        crate::translate_sanitized_native_reflected(ll, Stage::Kernel, &tmp, options)
            .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("PushConstant"), "{asm}");
    for offset in (16..64).step_by(4) {
        assert!(asm.contains(&format!("Offset {offset}")), "{asm}");
    }
    assert_eq!(asm.matches("OpUGreaterThanEqual").count(), 1, "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    assert_eq!(
        reflection.kernel_dispatch,
        Some(crate::reflect::KernelDispatch::ThreadsDynamic { offset: 16 })
    );
    assert_eq!(
        reflection
            .kernel_dispatch
            .and_then(crate::reflect::KernelDispatch::push_constant_range),
        Some(crate::reflect::KernelDispatchPushConstantRange {
            offset: 16,
            size: 48,
        })
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
}

#[test]
fn native_kernel_default_dispatch_uses_dynamic_grid_and_requires_explicit_workgroups() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_default_dispatch_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (guarded, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("default translation");
    let guarded_asm = disassemble(&guarded).expect("disassemble guarded default");
    assert!(
        guarded_asm.contains("BuiltIn WorkgroupSize"),
        "{guarded_asm}"
    );
    assert_eq!(guarded_asm.matches("SpecId").count(), 3);
    assert!(!guarded_asm.contains("OpBranchConditional"));
    assert_eq!(
        reflection.kernel_dispatch,
        Some(crate::reflect::KernelDispatch::ThreadsDynamic {
            offset: crate::reflect::DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
        })
    );
    tools::spirv_val_bytes(&guarded, &tmp).expect("spirv-val guarded default");

    let workgroups = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_dispatch: Some(crate::reflect::KernelDispatch::Workgroups),
            ..passes::TransformOptions::default()
        },
    )
    .expect("explicit whole-workgroup translation");
    let workgroups_asm = disassemble(&workgroups).expect("disassemble whole workgroups");
    assert!(!workgroups_asm.contains("PushConstant"), "{workgroups_asm}");
    assert!(
        !workgroups_asm.contains("OpBranchConditional"),
        "{workgroups_asm}"
    );
}

#[test]
fn native_kernel_partial_dispatch_threads_preserves_source_workgroup_barrier() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  tail call void @air.wg.barrier(i32 0, i32 1)
  ret void
}

declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_partial_barrier_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("boundary decomposition keeps every workgroup barrier uniform");
    let asm = disassemble(&spv).expect("disassemble exact-thread barrier");
    assert!(asm.contains("OpControlBarrier"), "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val exact-thread barrier");

    let workgroups = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_dispatch: Some(crate::reflect::KernelDispatch::Workgroups),
            ..passes::TransformOptions::default()
        },
    )
    .expect("an explicit whole-workgroup proof keeps the barrier uniform");
    tools::spirv_val_bytes(&workgroups, &tmp).expect("spirv-val whole-workgroup barrier");
}

#[test]
fn native_kernel_partial_dispatch_ignores_barrier_in_unreachable_helper() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  ret void
}

define internal void @dead_helper() {
entry:
  tail call void @air.wg.barrier(i32 0, i32 1)
  ret void
}

declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_dead_barrier_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("a dead helper's barrier does not constrain entry dispatch");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("BuiltIn WorkgroupSize"), "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    assert!(!asm.contains("OpControlBarrier"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val dead barrier helper");
}

#[test]
fn native_kernel_partial_dispatch_preserves_barrier_in_reachable_helper() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k() {
entry:
  tail call void @barrier_helper()
  ret void
}

define internal void @barrier_helper() {
entry:
  tail call void @air.wg.barrier(i32 0, i32 1)
  ret void
}

declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_reachable_barrier_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("boundary decomposition keeps a reachable helper barrier uniform");
    let asm = disassemble(&spv).expect("disassemble reachable barrier");
    assert!(asm.contains("OpControlBarrier"), "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val reachable barrier");
}

#[test]
fn native_kernel_generated_workgroup_initialization_needs_no_dispatch_cull() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@scratch = internal addrspace(3) global [1 x i32] zeroinitializer, align 4

define void @k() {
entry:
  %slot = getelementptr [1 x i32], ptr addrspace(3) @scratch, i64 0, i64 0
  store i32 7, ptr addrspace(3) %slot, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_generated_barrier_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [8, 1, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::ThreadsFixed {
                threads_per_grid: [9, 1, 1],
            }),
            ..passes::TransformOptions::default()
        },
    )
    .expect("translator-owned barrier remains uniform");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpControlBarrier"), "{asm}");
    assert!(!asm.contains("OpBranchConditional"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
}

#[test]
fn native_kernel_threadgroup_packed_float1_record_array_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Temp = type { [1 x float], i32 }

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %value = getelementptr inbounds %struct.Temp, ptr addrspace(3) %temp, i64 %idx, i32 0, i64 0
  store float 1.000000e+00, ptr addrspace(3) %value, align 4
  %count = getelementptr inbounds %struct.Temp, ptr addrspace(3) %temp, i64 %idx, i32 1
  store i32 %i, ptr addrspace(3) %count, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !5, !"air.arg_type_name", !"Temp", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!5 = !{i32 0, i32 4, i32 0, !"packed_float1", !"value", i32 4, i32 4, i32 0, !"uint", !"count"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_packed_float1_record_array_{}",
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
fn native_raw_workgroup_vector_view_is_valid_before_retries() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(3) %tile, ptr addrspace(1) %out, i32 %index) {
entry:
  %value = call <3 x half> @load3(ptr addrspace(3) %tile, i32 %index)
  store <3 x half> %value, ptr addrspace(1) %out, align 8
  ret void
}

define internal <3 x half> @load3(ptr addrspace(3) %tile, i32 %index) {
entry:
  %wide = getelementptr <4 x half>, ptr addrspace(3) %tile, i32 %index
  %alias = bitcast ptr addrspace(3) %wide to ptr addrspace(3)
  %value = load <3 x half>, ptr addrspace(3) %alias, align 8
  ret <3 x half> %value
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"tile"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half3*", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_workgroup_vector_view_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_native_no_retry(ll, Stage::Kernel).expect("primary translate");
    tools::spirv_val_bytes(&spv, &tmp).expect("primary SPIR-V validates");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Workgroup"), "{asm}");
    assert!(asm.contains("OpBitcast"), "{asm}");
}

#[test]
fn native_kernel_threadgroup_param_direct_scalar_load_uses_element_zero() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds float, ptr addrspace(3) %temp, i64 %idx
  store float 2.000000e+00, ptr addrspace(3) %temp, align 4
  %root = load float, ptr addrspace(3) %temp, align 4
  store float %root, ptr addrspace(3) %slot, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_threadgroup_direct_scalar_root_load_{}",
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
    let module = load_bytes(&spv).expect("load spv");
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
    let direct_workgroup_load = module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            inst.class.opcode == Op::Load
                && inst
                    .operands
                    .first()
                    .and_then(id_ref_operand)
                    .is_some_and(|id| workgroup_vars.contains(&id))
        });
    assert!(!direct_workgroup_load, "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn subgroup_results_a_select_reads_are_spilled_to_a_variable() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out, i16 %lane) {
entry:
  %x = uitofp i16 %lane to float
  %up = tail call float @air.simd_shuffle_up.f32(float %x, i16 1)
  %down = tail call float @air.simd_shuffle_down.f32(float %x, i16 2)
  %hi = fadd float %down, 1.000000e+00
  %c = icmp ult i16 %lane, 30
  %pick = select i1 %c, float %hi, float %up
  store float %pick, ptr addrspace(1) %out, align 4
  ret void
}

declare float @air.simd_shuffle_up.f32(float, i16)
declare float @air.simd_shuffle_down.f32(float, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.thread_index_in_simdgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"lane"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_subgroup_materialize_{}",
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
    let module = load_bytes(&spv).expect("load spv");
    let shuffles = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::GroupNonUniformShuffle)
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    assert_eq!(shuffles.len(), 2, "{asm}");
    for shuffle in &shuffles {
        let readers = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.operands.contains(&Operand::IdRef(*shuffle)))
            .collect::<Vec<_>>();
        assert_eq!(readers.len(), 1, "{asm}");
        assert_eq!(readers[0].class.opcode, Op::Store, "{asm}");
    }
    let select_reads_a_shuffle = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::Select)
        .any(|inst| {
            inst.operands
                .iter()
                .any(|operand| matches!(operand, Operand::IdRef(id) if shuffles.contains(id)))
        });
    assert!(!select_reads_a_shuffle, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn kernel_threadgroup_array_length_follows_the_caller() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds i32, ptr addrspace(3) %temp, i64 %idx
  store i32 7, ptr addrspace(3) %slot, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_array_length_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let elements = |options: crate::passes::TransformOptions| -> u32 {
        let spv = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
            .expect("translate");
        let module = load_bytes(&spv).expect("load spv");
        let constants = module
            .types_global_values
            .iter()
            .filter_map(|inst| match (inst.class.opcode, inst.operands.first()) {
                (Op::Constant, Some(Operand::LiteralBit32(value))) => {
                    Some((inst.result_id?, *value))
                }
                _ => None,
            })
            .collect::<crate::fxhash::HashMap<_, _>>();
        module
            .types_global_values
            .iter()
            .find_map(|inst| {
                (inst.class.opcode == Op::TypeArray)
                    .then(|| {
                        constants
                            .get(&id_ref_operand(inst.operands.get(1)?)?)
                            .copied()
                    })
                    .flatten()
            })
            .expect("a Workgroup array")
    };
    assert_eq!(elements(whole_workgroup_options()), 512);
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 4352)
                .expect("length")
        ),
        1088
    );
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 4354)
                .expect("length")
        ),
        1088
    );
    let too_small = whole_workgroup_options()
        .with_threadgroup_memory_length(0, 2)
        .expect("length");
    let error = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, too_small)
        .expect_err("a two-byte binding holds no uint");
    assert!(error.contains("less than one 4-byte element"), "{error}");
    assert!(
        crate::passes::TransformOptions::default()
            .with_threadgroup_memory_length(0, 0)
            .is_err(),
        "a zero length must be refused"
    );
}

#[test]
fn kernel_threadgroup_element_size_comes_from_the_air_struct() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%struct.Wide = type { i32, [3 x <4 x float>], i32, i32, i32, i32 }

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds %struct.Wide, ptr addrspace(3) %temp, i64 %idx, i32 0
  store i32 7, ptr addrspace(3) %slot, align 16
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !4, !"air.arg_type_size", i32 80, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"Wide", !"air.arg_name", !"temp"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"type", i32 16, i32 48, i32 0, !"float3x3", !"transform", i32 64, i32 4, i32 0, !"uint", !"numInliers", i32 68, i32 4, i32 0, !"int", !"err"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_element_size_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let elements = |options: crate::passes::TransformOptions| -> u32 {
        let spv = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
            .expect("translate");
        let module = load_bytes(&spv).expect("load spv");
        let constants = module
            .types_global_values
            .iter()
            .filter_map(|inst| match (inst.class.opcode, inst.operands.first()) {
                (Op::Constant, Some(Operand::LiteralBit32(value))) => {
                    Some((inst.result_id?, *value))
                }
                _ => None,
            })
            .collect::<crate::fxhash::HashMap<_, _>>();
        let types = module
            .types_global_values
            .iter()
            .filter_map(|inst| Some((inst.result_id?, inst)))
            .collect::<crate::fxhash::HashMap<_, _>>();
        let pointer = module
            .types_global_values
            .iter()
            .find(|inst| {
                inst.class.opcode == Op::Variable
                    && inst.operands.first()
                        == Some(&Operand::StorageClass(spirv::StorageClass::Workgroup))
            })
            .and_then(|inst| inst.result_type)
            .expect("a Workgroup variable");
        let array = types
            .get(&pointer)
            .and_then(|inst| id_ref_operand(inst.operands.get(1)?))
            .expect("the variable's pointee");
        let length = types
            .get(&array)
            .filter(|inst| inst.class.opcode == Op::TypeArray)
            .and_then(|inst| id_ref_operand(inst.operands.get(1)?))
            .expect("a Workgroup array");
        constants.get(&length).copied().expect("its length")
    };
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 4000)
                .expect("length")
        ),
        50
    );
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 4079)
                .expect("length")
        ),
        50
    );
    let error = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        whole_workgroup_options()
            .with_threadgroup_memory_length(0, 40)
            .expect("length"),
    )
    .expect_err("40 bytes holds no 80-byte element");
    assert!(error.contains("less than one 80-byte element"), "{error}");
    let guessed = elements(whole_workgroup_options());
    assert_eq!(guessed, 409);
    assert!(
        guessed * 80 <= 32768,
        "the guess must name memory a pipeline can declare: {} bytes",
        guessed * 80
    );
}

#[test]
fn kernel_raw_threadgroup_array_length_also_follows_the_caller() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(3) %temp, i32 %i) {
entry:
  %wide = getelementptr inbounds i32, ptr addrspace(3) %temp, i64 0
  store i32 0, ptr addrspace(3) %wide, align 4
  %idx = zext i32 %i to i64
  %slot = getelementptr inbounds i16, ptr addrspace(3) %temp, i64 %idx
  %v = load i16, ptr addrspace(3) %slot, align 2
  %w = add i16 %v, 1
  store i16 %w, ptr addrspace(3) %slot, align 2
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"ushort", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_raw_threadgroup_array_length_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let elements = |options: crate::passes::TransformOptions| -> u32 {
        let spv = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
            .expect("translate");
        let module = load_bytes(&spv).expect("load spv");
        let constants = module
            .types_global_values
            .iter()
            .filter_map(|inst| match (inst.class.opcode, inst.operands.first()) {
                (Op::Constant, Some(Operand::LiteralBit32(value))) => {
                    Some((inst.result_id?, *value))
                }
                _ => None,
            })
            .collect::<crate::fxhash::HashMap<_, _>>();
        module
            .types_global_values
            .iter()
            .find_map(|inst| {
                (inst.class.opcode == Op::TypeArray)
                    .then(|| {
                        constants
                            .get(&id_ref_operand(inst.operands.get(1)?)?)
                            .copied()
                    })
                    .flatten()
            })
            .expect("a Workgroup array")
    };
    assert_eq!(elements(whole_workgroup_options()), 2048);
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 12288)
                .expect("length")
        ),
        3072
    );
    assert_eq!(
        elements(
            whole_workgroup_options()
                .with_threadgroup_memory_length(0, 1024)
                .expect("length")
        ),
        256
    );
}

#[test]
fn native_wg_barrier_device_flag_orders_storage_buffer_memory() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %out) {
entry:
  store i32 1, ptr addrspace(1) %out, align 4
  tail call void @air.wg.barrier(i32 1, i32 1)
  %v = load i32, ptr addrspace(1) %out, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_wg_barrier_device_{}",
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
    assert!(asm.contains("OpControlBarrier"), "{asm}");
    assert!(asm.contains(" 584"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_threadgroup_atomic_i32_lowers_to_workgroup_spirv() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
@local_counts = internal addrspace(3) global [1 x %"struct.metal::_atomic"] zeroinitializer, align 4

define void @k() {
entry:
  %p = getelementptr inbounds [1 x %"struct.metal::_atomic"], ptr addrspace(3) @local_counts, i64 0, i64 0, i32 0
  tail call void @air.atomic.local.store.i32(ptr addrspace(3) %p, i32 0, i32 0, i32 1, i1 true)
  tail call void @air.wg.barrier(i32 2, i32 1)
  %v = tail call i32 @air.atomic.local.load.i32(ptr addrspace(3) %p, i32 0, i32 1, i1 true)
  %old = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) %p, i32 1, i32 0, i32 1, i1 true)
  %sub_old = tail call i32 @air.atomic.local.sub.u.i32(ptr addrspace(3) nonnull captures(none) inttoptr (i64 1024 to ptr addrspace(3)), i32 1, i32 0, i32 1, i1 true)
  %signed_old = tail call i32 @air.atomic.local.add.s.i32(ptr addrspace(3) %p, i32 -1, i32 0, i32 1, i1 true)
  %signed_max_old = tail call i32 @air.atomic.local.max.s.i32(ptr addrspace(3) %p, i32 -3, i32 0, i32 1, i1 true)
  %max_old = tail call i32 @air.atomic.local.max.u.i32(ptr addrspace(3) %p, i32 9, i32 0, i32 1, i1 true)
  %signed_min_old = tail call i32 @air.atomic.local.min.s.i32(ptr addrspace(3) %p, i32 -9, i32 0, i32 1, i1 true)
  %min_old = tail call i32 @air.atomic.local.min.u.i32(ptr addrspace(3) %p, i32 3, i32 0, i32 1, i1 true)
  %masked = tail call i32 @air.atomic.local.and.u.i32(ptr addrspace(3) %p, i32 255, i32 0, i32 1, i1 true)
  %mask = tail call i32 @air.atomic.local.or.u.i32(ptr addrspace(3) %p, i32 2, i32 0, i32 1, i1 true)
  ret void
}

declare void @air.atomic.local.store.i32(ptr addrspace(3), i32, i32, i32, i1)
declare void @air.wg.barrier(i32, i32)
declare i32 @air.atomic.local.load.i32(ptr addrspace(3), i32, i32, i1)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.sub.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.add.s.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.max.s.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.max.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.min.s.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.min.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.and.u.i32(ptr addrspace(3), i32, i32, i32, i1)
declare i32 @air.atomic.local.or.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_{}",
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
    assert!(asm.contains("Workgroup"), "{asm}");
    assert!(asm.contains("OpControlBarrier"), "{asm}");
    assert!(asm.contains("OpAtomicStore"), "{asm}");
    assert!(asm.contains("OpAtomicLoad"), "{asm}");
    assert!(asm.matches("OpAtomicIAdd").count() >= 2, "{asm}");
    assert!(asm.contains("OpAtomicISub"), "{asm}");
    assert!(asm.contains("OpAtomicSMax"), "{asm}");
    assert!(asm.contains("OpAtomicUMax"), "{asm}");
    assert!(asm.contains("OpAtomicSMin"), "{asm}");
    assert!(asm.contains("OpAtomicUMin"), "{asm}");
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
fn native_threadgroup_atomic_fixed_loop_is_flattened_and_unrolled() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }
@local_counts = internal addrspace(3) global [4 x %"struct.metal::_atomic"] zeroinitializer, align 4

define void @k() {
entry:
  %initp = getelementptr inbounds [4 x %"struct.metal::_atomic"], ptr addrspace(3) @local_counts, i64 0, i64 0, i32 0
  store i32 0, ptr addrspace(3) %initp, align 4
  tail call void @air.wg.barrier(i32 2, i32 1)
  br label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %loop ]
  %bin64 = zext i32 %i to i64
  %p = getelementptr inbounds [4 x %"struct.metal::_atomic"], ptr addrspace(3) @local_counts, i64 0, i64 %bin64, i32 0
  %old = tail call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) %p, i32 1, i32 0, i32 1, i1 true)
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, 4
  br i1 %done, label %exit, label %loop

exit:
  tail call void @air.wg.barrier(i32 2, i32 1)
  ret void
}

declare void @air.wg.barrier(i32, i32)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_threadgroup_atomic_unroll_{}",
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
    assert!(!asm.contains("OpTypeStruct %uint"), "{asm}");
    assert_eq!(asm.matches("OpAtomicIAdd").count(), 4, "{asm}");
    assert!(!asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_simdgroup_matrix_8x8_lowers_through_scalar_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %out) {
entry:
  %a = insertelement <64 x float> zeroinitializer, float 1.000000e+00, i64 0
  %b = insertelement <64 x float> zeroinitializer, float 2.000000e+00, i64 0
  %c = insertelement <64 x float> zeroinitializer, float 3.000000e+00, i64 0
  %m = tail call <64 x float> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f32.v64f32.v64f32.v64f32(<64 x float> %a, <64 x float> %b, <64 x float> %c)
  %lane = extractelement <64 x float> %m, i64 0
  store float %lane, ptr addrspace(1) %out, align 4
  ret void
}

declare <64 x float> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f32.v64f32.v64f32.v64f32(<64 x float>, <64 x float>, <64 x float>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simdgroup_matrix_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpTypeVector %float 64"), "{asm}");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains(" Fma "), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_agx2_distributed_matmad_lowers_to_partitioned_subgroup_shuffles() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %out_f, ptr addrspace(1) %out_h) {
entry:
  %f8 = call <2 x float> @llvm.agx2.f32matmad8x8.v2f32(<2 x float> zeroinitializer, <2 x float> zeroinitializer, <2 x float> zeroinitializer)
  %f4 = call <2 x float> @llvm.agx2.f32matmad4x4.v2f32(<2 x float> %f8, <2 x float> %f8, <2 x float> %f8)
  %h8 = call <2 x half> @llvm.agx2.f16matmad8x8.v2f16(<2 x half> zeroinitializer, <2 x half> zeroinitializer, <2 x half> zeroinitializer)
  %h4 = call <2 x half> @llvm.agx2.f16matmad4x4.v2f16(<2 x half> %h8, <2 x half> %h8, <2 x half> %h8)
  store <2 x float> %f4, ptr addrspace(1) %out_f, align 8
  store <2 x half> %h4, ptr addrspace(1) %out_h, align 4
  ret void
}

declare <2 x float> @llvm.agx2.f32matmad8x8.v2f32(<2 x float>, <2 x float>, <2 x float>)
declare <2 x float> @llvm.agx2.f32matmad4x4.v2f32(<2 x float>, <2 x float>, <2 x float>)
declare <2 x half> @llvm.agx2.f16matmad8x8.v2f16(<2 x half>, <2 x half>, <2 x half>)
declare <2 x half> @llvm.agx2.f16matmad4x4.v2f16(<2 x half>, <2 x half>, <2 x half>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float2*", !"air.arg_name", !"out_f"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half2*", !"air.arg_name", !"out_h"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_agx2_distributed_matmad_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("llvm.agx2."), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(asm.contains("BuiltIn SubgroupLocalInvocationId"), "{asm}");
    assert_eq!(
        asm.lines()
            .filter(|line| line.contains("= OpGroupNonUniformShuffle "))
            .count(),
        72,
        "{asm}"
    );
    assert_eq!(asm.matches(" Fma ").count(), 48, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_simdgroup_matrix_16x16_distributed_mac_lowers_and_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(i16 %lane, ptr addrspace(1) %out_f, ptr addrspace(1) %out_i) {
entry:
  %transpose = icmp eq i16 %lane, 0
  %f = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f16.v8f16.v8f32(<8 x half> zeroinitializer, i1 %transpose, <8 x half> zeroinitializer, i1 true, <8 x float> zeroinitializer)
  %i = call <8 x i32> @air.simdgroup_matrix_16x16x16_widening_multiply_accumulate.s.u.v8i32.v8i8.v8i8.v8i32(<8 x i8> zeroinitializer, i1 false, <8 x i8> zeroinitializer, i1 %transpose, <8 x i32> zeroinitializer)
  store <8 x float> %f, ptr addrspace(1) %out_f, align 32
  store <8 x i32> %i, ptr addrspace(1) %out_i, align 32
  ret void
}

declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f16.v8f16.v8f32(<8 x half>, i1, <8 x half>, i1, <8 x float>)
declare <8 x i32> @air.simdgroup_matrix_16x16x16_widening_multiply_accumulate.s.u.v8i32.v8i8.v8i8.v8i32(<8 x i8>, i1, <8 x i8>, i1, <8 x i32>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_index_in_simdgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"lane"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float8*", !"air.arg_name", !"out_f"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"int8*", !"air.arg_name", !"out_i"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simdgroup_matrix_16x16_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("simdgroup_matrix_16x16x16"), "{asm}");
    assert!(
        asm_has_line(&asm, "OpCapability GroupNonUniformShuffle"),
        "{asm}"
    );
    assert!(asm.contains("BuiltIn SubgroupLocalInvocationId"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(asm.contains("OpSConvert"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(asm.contains(" Fma "), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_agx3_packed_igemm_reuses_distributed_matrix_lowering() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %out) {
entry:
  %result = call <8 x i32> @llvm.agx3.igemm.v8i32.i64.i64.v8i32(i8 16, i8 16, i8 16, i16 9, i64 578437695752307201, i16 75, i64 1157159078456920585, i16 75, <8 x i32> zeroinitializer, i16 9)
  store <8 x i32> %result, ptr addrspace(1) %out, align 32
  ret void
}

declare <8 x i32> @llvm.agx3.igemm.v8i32.i64.i64.v8i32(i8, i8, i8, i16, i64, i16, i64, i16, <8 x i32>, i16)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"int8*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_agx3_packed_igemm_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("llvm.agx3.igemm"), "{asm}");
    assert!(asm.contains("OpShiftRightLogical"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("OpSConvert"), "{asm}");
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_simdgroup_matrix_16x16_all_observed_float_encodings_validate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %out) {
entry:
  %f32 = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f32.v8f32.v8f32(<8 x float> zeroinitializer, i1 false, <8 x float> zeroinitializer, i1 false, <8 x float> zeroinitializer)
  store <8 x float> %f32, ptr addrspace(1) %out, align 32
  %bf16 = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8bf16.v8bf16.v8f32(<8 x bfloat> zeroinitializer, i1 false, <8 x bfloat> zeroinitializer, i1 false, <8 x float> zeroinitializer)
  store <8 x float> %bf16, ptr addrspace(1) %out, align 32
  %e4m3 = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e4m3.v8f16.v8f32(<8 x i8> zeroinitializer, i1 false, <8 x half> zeroinitializer, i1 false, <8 x float> zeroinitializer)
  store <8 x float> %e4m3, ptr addrspace(1) %out, align 32
  %e4m3fn = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e4m3fn.v8f8e4m3fn.v8f32(<8 x i8> zeroinitializer, i1 false, <8 x i8> zeroinitializer, i1 false, <8 x float> zeroinitializer)
  store <8 x float> %e4m3fn, ptr addrspace(1) %out, align 32
  %e5m2 = call <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e5m2.v8f8e5m2.v8f32(<8 x i8> zeroinitializer, i1 false, <8 x i8> zeroinitializer, i1 false, <8 x float> zeroinitializer)
  store <8 x float> %e5m2, ptr addrspace(1) %out, align 32
  ret void
}

declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f32.v8f32.v8f32(<8 x float>, i1, <8 x float>, i1, <8 x float>)
declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8bf16.v8bf16.v8f32(<8 x bfloat>, i1, <8 x bfloat>, i1, <8 x float>)
declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e4m3.v8f16.v8f32(<8 x i8>, i1, <8 x half>, i1, <8 x float>)
declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e4m3fn.v8f8e4m3fn.v8f32(<8 x i8>, i1, <8 x i8>, i1, <8 x float>)
declare <8 x float> @air.simdgroup_matrix_16x16x16_multiply_accumulate.f.f.v8f32.v8f8e5m2.v8f8e5m2.v8f32(<8 x i8>, i1, <8 x i8>, i1, <8 x float>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float8*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simdgroup_matrix_16x16_float_types_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("simdgroup_matrix_16x16x16"), "{asm}");
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert!(asm.contains("OpShiftLeftLogical"), "{asm}");
    assert!(asm.contains(" Ldexp "), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_simdgroup_matrix_8x8_full_pipeline_lowers_and_validates() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main(ptr addrspace(1) %in_h, ptr addrspace(1) %in_f, ptr addrspace(1) %out) {
entry:
  %pv1 = insertelement <2 x i64> <i64 poison, i64 8>, i64 8, i64 0
  %pv2 = insertelement <2 x i64> <i64 1, i64 poison>, i64 8, i64 1
  %ph = getelementptr inbounds half, ptr addrspace(1) %in_h, i64 0
  %pf = getelementptr inbounds float, ptr addrspace(1) %in_f, i64 0
  %po = getelementptr inbounds float, ptr addrspace(1) %out, i64 0
  %A = call <64 x half> @air.simdgroup_matrix_8x8_load.v64f16.p1f16(ptr addrspace(1) %ph, <2 x i64> %pv1, <2 x i64> %pv2, <2 x i64> zeroinitializer)
  %B = call <64 x float> @air.simdgroup_matrix_8x8_load.v64f32.p1f32(ptr addrspace(1) %pf, <2 x i64> %pv1, <2 x i64> %pv2, <2 x i64> zeroinitializer)
  %C = call <64 x float> @air.simdgroup_matrix_8x8_init_diag.v64f32.f32(float 1.000000e+00)
  %D = call <64 x float> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f32.v64f32.v64f16.v64f32(<64 x float> %B, <64 x half> %A, <64 x float> %C)
  %Dh = call <64 x half> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f16.v64f32.v64f16.v64f32(<64 x float> %B, <64 x half> %A, <64 x float> %C)
  %dh0 = extractelement <64 x half> %Dh, i64 0
  %dhf = fpext half %dh0 to float
  store float %dhf, ptr addrspace(1) %po
  %scaled = fmul fast <64 x float> %D, %D
  call void @air.simdgroup_matrix_8x8_store.v64f32.p1f32(<64 x float> %scaled, ptr addrspace(1) %po, <2 x i64> %pv1, <2 x i64> %pv2, <2 x i64> zeroinitializer)
  ret void
}

declare <64 x half> @air.simdgroup_matrix_8x8_load.v64f16.p1f16(ptr addrspace(1), <2 x i64>, <2 x i64>, <2 x i64>)
declare <64 x float> @air.simdgroup_matrix_8x8_load.v64f32.p1f32(ptr addrspace(1), <2 x i64>, <2 x i64>, <2 x i64>)
declare <64 x float> @air.simdgroup_matrix_8x8_init_diag.v64f32.f32(float)
declare <64 x float> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f32.v64f32.v64f16.v64f32(<64 x float>, <64 x half>, <64 x float>)
declare <64 x half> @air.simdgroup_matrix_8x8_multiply_accumulate.v64f16.v64f32.v64f16.v64f32(<64 x float>, <64 x half>, <64 x float>)
declare void @air.simdgroup_matrix_8x8_store.v64f32.p1f32(<64 x float>, ptr addrspace(1), <2 x i64>, <2 x i64>, <2 x i64>)

!air.kernel = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"half", !"air.arg_name", !"in_h"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"in_f"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_simdgroup_matrix_full_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let out = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(!asm.contains("OpTypeVector %float 64"), "{asm}");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_declared_simdgroup_lane_is_the_lane_the_shuffle_reads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(i32 %v, i16 %lane, ptr addrspace(1) %out) {
entry:
  %same = tail call i32 @air.simd_shuffle.u.i32(i32 %v, i16 %lane)
  %first = tail call i1 @air.simd_is_first()
  %word = select i1 %first, i32 %same, i32 0
  store i32 %word, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.simd_shuffle.u.i32(i32, i16)
declare i1 @air.simd_is_first()

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 1, !"air.thread_index_in_simdgroup", !"air.arg_type_name", !"ushort", !"air.arg_name", !"lane"}
!4 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_declared_simd_lane_{}",
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
    assert!(asm.contains("OpGroupNonUniformShuffle"), "{asm}");
    assert_eq!(
        asm.matches("BuiltIn SubgroupLocalInvocationId").count(),
        1,
        "one lane variable, shared by the declared role and the intrinsic\n{asm}"
    );
    assert!(
        !asm.contains("BuiltIn LocalInvocationIndex"),
        "the lane inside a simdgroup is not a fact about the threadgroup\n{asm}"
    );
    let lane_var = asm
        .lines()
        .map(str::trim)
        .find(|line| line.contains("BuiltIn SubgroupLocalInvocationId"))
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("lane variable decoration")
        .to_string();
    assert!(
        asm.lines()
            .any(|line| line.contains(" = OpLoad ") && line.trim_end().ends_with(&lane_var)),
        "the declared lane loads that variable\n{asm}"
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
fn native_barrier_execution_scope_comes_from_the_operand_not_the_callee() {
    fn barrier_scopes(ll: &str, label: &str) -> Vec<(u32, u32, u32)> {
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_barrier_scope_{label}_{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&tmp);
        let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
        if std::process::Command::new("spirv-val")
            .arg("--version")
            .output()
            .is_ok()
        {
            tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
        }
        let module = load_bytes(&spv).expect("load spv");
        let constants: HashMap<Word, u32> = module
            .types_global_values
            .iter()
            .filter(|inst| inst.class.opcode == Op::Constant)
            .filter_map(|inst| match (inst.result_id, inst.operands.first()) {
                (Some(id), Some(Operand::LiteralBit32(value))) => Some((id, *value)),
                _ => None,
            })
            .collect();
        let resolve = |operand: &Operand| match operand {
            Operand::IdScope(id) | Operand::IdMemorySemantics(id) | Operand::IdRef(id) => {
                *constants.get(id).expect("barrier operand is a constant")
            }
            other => panic!("unexpected barrier operand {other:?}"),
        };
        let scopes = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.class.opcode == Op::ControlBarrier)
            .map(|inst| {
                (
                    resolve(&inst.operands[0]),
                    resolve(&inst.operands[1]),
                    resolve(&inst.operands[2]),
                )
            })
            .collect();
        let _ = std::fs::remove_dir_all(&tmp);
        scopes
    }

    let ll = |calls: &str| {
        format!(
            r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %out) {{
entry:
{calls}
  store i32 0, ptr addrspace(1) %out, align 4
  ret void
}}

declare void @air.wg.barrier(i32, i32)
declare void @air.simdgroup.barrier(i32, i32)

!air.kernel = !{{!0}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"int*", !"air.arg_name", !"out"}}
"#
        )
    };

    let workgroup_memory = {
        use spirv::MemorySemantics;
        (MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY).bits()
    };
    assert_eq!(
        barrier_scopes(
            &ll("  tail call void @air.simdgroup.barrier(i32 2, i32 1)\n  \
                 tail call void @air.simdgroup.barrier(i32 2, i32 4)"),
            "simd"
        ),
        vec![
            (
                Scope::Workgroup as u32,
                Scope::Workgroup as u32,
                workgroup_memory
            ),
            (
                Scope::Subgroup as u32,
                Scope::Subgroup as u32,
                workgroup_memory
            ),
        ]
    );
    assert_eq!(
        barrier_scopes(&ll("  tail call void @air.wg.barrier(i32 8, i32 1)"), "wg"),
        vec![(
            Scope::Workgroup as u32,
            Scope::Workgroup as u32,
            workgroup_memory
        )]
    );

    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_barrier_scope_bad_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let err = crate::translate_sanitized_native(
        &ll("  tail call void @air.wg.barrier(i32 2, i32 2)"),
        Stage::Kernel,
        &tmp,
    )
    .expect_err("undefined barrier scope");
    assert!(err.contains("execution scope 2"), "{err}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_dense_barrier_kernel_preserves_source_reconvergence() {
    let mut ll = String::from(
        "target triple = \"spirv-unknown-vulkan1.2\"\n\
         define void @k(ptr addrspace(1) %out) {\n\
         entry:\n  %input = load i32, ptr addrspace(1) %out, align 4\n\
         \x20 %c = icmp ne i32 %input, 0\n  br label %n0\n",
    );
    for i in 0..70 {
        ll.push_str(&format!(
            "n{i}:\n  br i1 %c, label %t{i}, label %n{next}\n\
             t{i}:\n  store volatile i32 {i}, ptr addrspace(1) %out, align 4\n\
             \x20 br label %n{next}\n",
            next = i + 1,
        ));
    }
    ll.push_str(
        "n70:\n  call void @air.wg.barrier(i32 3, i32 1)\n  ret void\n}\n\
         declare void @air.wg.barrier(i32, i32)\n\
         !air.kernel = !{!0}\n!0 = !{ptr @k, !1, !2}\n!1 = !{}\n!2 = !{!3}\n\
         !3 = !{i32 0, !\"air.buffer\", !\"air.location_index\", i32 0, i32 1, !\"air.read_write\", !\"air.arg_type_name\", !\"uint*\", !\"air.arg_name\", !\"out\"}\n",
    );
    let spv = crate::translate_native_no_retry_constructed_with_options(
        &ll,
        Stage::Kernel,
        passes::TransformOptions::default(),
    )
    .expect("bounded barrier kernel translates");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpControlBarrier"));
    assert!(
        !asm.contains("OpSwitch"),
        "barrier must not enter a per-lane dispatcher"
    );
    let tmp = std::env::temp_dir().join(format!("m2v_dense_barrier_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("isolated validation directory");
    let validation = tools::spirv_val_bytes(&spv, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    validation.expect("valid structured barriers");
}

const TGCEIL_K32_LL: &str = r#"; ModuleID = 'k32.metal'
source_filename = "k32.metal"
target datalayout = "e-p:64:64:64-i1:8:8-i8:8:8-i16:16:16-i32:32:32-i64:64:64-f32:32:32-f64:64:64-v16:16:16-v24:32:32-v32:32:32-v48:64:64-v64:64:64-v96:128:128-v128:128:128-v192:256:256-v256:256:256-v512:512:512-v1024:1024:1024-n8:16:32"
target triple = "air64_v27-apple-macosx15.7.0"

; Function Attrs: argmemonly mustprogress nofree norecurse nosync nounwind willreturn writeonly
define void @k32(i32 addrspace(1)* nocapture noundef writeonly "air-buffer-no-alias" %0, i32 noundef %1) local_unnamed_addr #0 {
  %3 = mul i32 %1, 3
  %4 = add i32 %3, 1
  %5 = zext i32 %1 to i64
  %6 = getelementptr inbounds i32, i32 addrspace(1)* %0, i64 %5
  store i32 %4, i32 addrspace(1)* %6, align 4, !tbaa !22, !alias.scope !26
  ret void
}

attributes #0 = { argmemonly mustprogress nofree norecurse nosync nounwind willreturn writeonly "approx-func-fp-math"="true" "frame-pointer"="all" "max-work-group-size"="32" "min-legal-vector-width"="0" "no-builtins" "no-infs-fp-math"="true" "no-nans-fp-math"="true" "no-signed-zeros-fp-math"="true" "no-trapping-math"="true" "stack-protector-buffer-size"="8" "unsafe-fp-math"="true" }

!llvm.module.flags = !{!0, !1, !2, !3, !4, !5, !6, !7, !8}
!air.kernel = !{!9}
!air.compile_options = !{!15, !16, !17}
!llvm.ident = !{!18}
!air.version = !{!19}
!air.language_version = !{!20}
!air.source_file_name = !{!21}

!0 = !{i32 2, !"SDK Version", [2 x i32] [i32 27, i32 0]}
!1 = !{i32 1, !"wchar_size", i32 4}
!2 = !{i32 7, !"frame-pointer", i32 2}
!3 = !{i32 7, !"air.max_device_buffers", i32 31}
!4 = !{i32 7, !"air.max_constant_buffers", i32 31}
!5 = !{i32 7, !"air.max_threadgroup_buffers", i32 31}
!6 = !{i32 7, !"air.max_textures", i32 128}
!7 = !{i32 7, !"air.max_read_write_textures", i32 8}
!8 = !{i32 7, !"air.max_samplers", i32 16}
!9 = !{void (i32 addrspace(1)*, i32)* @k32, !10, !11, !14}
!10 = !{}
!11 = !{!12, !13}
!12 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"o"}
!13 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"t"}
!14 = !{!"air.max_work_group_size", i32 32}
!15 = !{!"air.compile.denorms_disable"}
!16 = !{!"air.compile.fast_math_enable"}
!17 = !{!"air.compile.framebuffer_fetch_enable"}
!18 = !{!"Apple metal version 32023.917 (metalfe-32023.917.2)"}
!19 = !{i32 2, i32 7, i32 0}
!20 = !{!"Metal", i32 3, i32 2, i32 0}
!21 = !{!"k32.metal"}
!22 = !{!23, !23, i64 0}
!23 = !{!"int", !24, i64 0}
!24 = !{!"omnipotent char", !25, i64 0}
!25 = !{!"Simple C++ TBAA"}
!26 = !{!27}
!27 = distinct !{!27, !28, !"air-alias-scope-arg(0)"}
!28 = distinct !{!28, !"air-alias-scopes(k32)"}
"#;

fn tgceil_translate(ll: &str, options: passes::TransformOptions) -> Result<Vec<u8>, String> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("metal2vulkan_tgceil_{}_{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let r = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(&tmp);
    r
}

fn tgceil_spec_defaults(asm: &str) -> Vec<String> {
    asm.lines()
        .filter(|l| l.contains("OpSpecConstant ") || l.contains("OpSpecConstant\t"))
        .filter_map(|l| l.split_whitespace().last().map(str::to_string))
        .collect()
}

#[test]
fn a_kernel_whose_ceiling_is_below_the_placeholder_translates_with_the_ceiling_as_its_default() {
    let spv = tgceil_translate(TGCEIL_K32_LL, passes::TransformOptions::default())
        .expect("k32 must translate");
    let asm = disassemble(&spv).expect("disassemble");
    let d = tgceil_spec_defaults(&asm);
    assert!(
        d.iter().any(|v| v == "32"),
        "the x spec constant must default to the ceiling 32; spec defaults {d:?}\n{asm}"
    );
    assert!(
        !d.iter().any(|v| v == "64"),
        "no spec constant may still default to the placeholder 64; spec defaults {d:?}\n{asm}"
    );
    assert!(
        !asm.contains("LocalSize 64"),
        "no literal LocalSize 64 may survive\n{asm}"
    );
}

#[test]
fn an_explicit_workgroups_local_size_past_the_ceiling_is_still_refused() {
    let e = tgceil_translate(
        TGCEIL_K32_LL,
        passes::TransformOptions {
            kernel_local_size: [64, 1, 1],
            kernel_dispatch: Some(crate::reflect::KernelDispatch::Workgroups),
            ..passes::TransformOptions::default()
        },
    )
    .expect_err("an explicit 64-thread Workgroups shape past a ceiling of 32 must refuse");
    assert!(e.contains("ceiling of 32"), "{e}");
}

#[test]
fn a_caller_chosen_dynamic_shape_past_the_ceiling_is_still_refused() {
    let e = tgceil_translate(
        TGCEIL_K32_LL,
        passes::TransformOptions {
            kernel_local_size: [128, 1, 1],
            ..passes::TransformOptions::default()
        },
    )
    .expect_err("a caller-chosen 128 past a ceiling of 32 must refuse");
    assert!(
        e.contains("128 threads") && e.contains("ceiling of 32"),
        "{e}"
    );
}

#[test]
fn a_ceiling_above_the_placeholder_leaves_the_placeholder_alone() {
    let a = "!{!\"air.max_work_group_size\", i32 32}";
    assert_eq!(
        TGCEIL_K32_LL.matches(a).count(),
        1,
        "the k32 AIR carries its ceiling node exactly once"
    );
    let ll = TGCEIL_K32_LL.replace(a, "!{!\"air.max_work_group_size\", i32 128}");
    let spv =
        tgceil_translate(&ll, passes::TransformOptions::default()).expect("k128 must translate");
    let asm = disassemble(&spv).expect("disassemble");
    let d = tgceil_spec_defaults(&asm);
    assert!(
        d.iter().any(|v| v == "64"),
        "a ceiling of 128 keeps the placeholder 64; spec defaults {d:?}\n{asm}"
    );
}

#[test]
fn native_atomic_integer_constant_addresses_reject_overlapping_widths() {
    let ll = r#"
define void @k() {
entry:
  call void @air.atomic.local.max.u.i64(ptr addrspace(3) inttoptr (i64 1024 to ptr addrspace(3)), i64 4294967296, i32 0, i32 1, i1 true)
  %old = call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) inttoptr (i64 1028 to ptr addrspace(3)), i32 1, i32 0, i32 1, i1 true)
  ret void
}
declare void @air.atomic.local.max.u.i64(ptr addrspace(3), i64, i32, i32, i1)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{}
"#;
    let tmp = std::env::temp_dir().join(format!("m2v_atomic_overlap_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("scratch");
    let error = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect_err("overlap cannot use independent Workgroup slots");
    assert!(
        error.contains("overlapping Workgroup atomic integer addresses"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}
