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

fn translate_one_cell_tile(
    san_ll: &str,
    stage: Stage,
    tmp: &std::path::Path,
) -> Result<Vec<u8>, String> {
    crate::translate_sanitized_native_with_options(san_ll, stage, tmp, one_cell_tile_options())
}

fn one_cell_tile_options() -> passes::TransformOptions {
    passes::TransformOptions {
        kernel_local_size: [1, 1, 1],
        ..passes::TransformOptions::default()
    }
}

fn runtime_sampler_state(
    coordinates: crate::reflect::SamplerCoordinates,
    filter: crate::reflect::SamplerFilter,
    address: crate::reflect::SamplerAddressMode,
) -> crate::reflect::RuntimeSamplerState {
    crate::reflect::RuntimeSamplerState {
        min_filter: filter,
        mag_filter: filter,
        mip_filter: crate::reflect::SamplerMipFilter::None,
        address_mode_s: address,
        address_mode_t: address,
        address_mode_r: address,
        coordinates,
        compare_function: crate::reflect::SamplerCompareFunction::None,
        max_anisotropy: 1,
        lod_min_clamp: 0.0,
        lod_max_clamp: 0.0,
        border_color: crate::reflect::SamplerBorderColor::TransparentBlack,
        reduction: crate::reflect::SamplerReduction::WeightedAverage,
        lod_bias: 0.0,
    }
}

fn runtime_storage_image_state(
    format: crate::reflect::RuntimeStorageImageFormat,
    read_without_format: bool,
    write_without_format: bool,
) -> crate::reflect::RuntimeStorageImageState {
    crate::reflect::RuntimeStorageImageState {
        format,
        capabilities: crate::reflect::RuntimeStorageImageCapabilities {
            storage_image: true,
            storage_image_atomic: false,
            read_without_format,
            write_without_format,
        },
    }
}

const RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL: &str = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst0, ptr addrspace(1) %dst1, <2 x i32> %tid) {
entry:
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %dst0, <2 x i32> %tid, <4 x float> <float 1.000000e+00, float 2.000000e+00, float 3.000000e+00, float 4.000000e+00>, i32 0, i32 2)
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %dst1, <2 x i32> %tid, <4 x float> <float 4.000000e+00, float 3.000000e+00, float 2.000000e+00, float 1.000000e+00>, i32 0, i32 2)
  ret void
}

declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst0"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst1"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;

const RUNTIME_STORAGE_IMAGE_READ_LL: &str = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %out, <2 x i32> %tid) {
entry:
  %read = tail call { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1) %src, <2 x i32> %tid, i32 0, i32 3)
  %color = extractvalue { <4 x float>, i8 } %read, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<float, read_write>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;

const UNSIGNED_TEXTURE_ATOMIC_LL: &str = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @textureAtomic(<2 x i16> %coord, ptr addrspace(1) %image) {
entry:
  %old = call <4 x i32> @air.atomic_fetch_max_explicit_texture_2d.i16.u.v4i32(ptr addrspace(1) %image, <2 x i16> %coord, <2 x i16> <i16 1, i16 0>, <4 x i32> <i32 7, i32 7, i32 7, i32 7>, i32 0, i32 3)
  ret void
}

declare <4 x i32> @air.atomic_fetch_max_explicit_texture_2d.i16.u.v4i32(ptr addrspace(1), <2 x i16>, <2 x i16>, <4 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @textureAtomic, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"coord"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<uint, read_write>", !"air.arg_name", !"image"}
"#;

#[test]
fn native_unsigned_texture_fetch_max_uses_scalar_atomic_image_format() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_texture_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(UNSIGNED_TEXTURE_ATOMIC_LL, Stage::Kernel, &tmp)
        .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("R32ui"), "{asm}");
    assert!(asm.contains("OpImageTexelPointer"), "{asm}");
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
fn runtime_storage_image_atomic_requires_scalar_format_and_host_support() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_storage_atomic_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let state = |format, storage_image_atomic| crate::reflect::RuntimeStorageImageState {
        format,
        capabilities: crate::reflect::RuntimeStorageImageCapabilities {
            storage_image: true,
            storage_image_atomic,
            read_without_format: false,
            write_without_format: false,
        },
    };

    let missing = crate::translate_sanitized_native_with_options(
        UNSIGNED_TEXTURE_ATOMIC_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                state(crate::reflect::RuntimeStorageImageFormat::R32Uint, false),
            )
            .unwrap(),
    )
    .expect_err("atomic format without host atomic support");
    assert!(
        missing.contains("lacks storage-image atomic support"),
        "{missing}"
    );

    let vector = crate::translate_sanitized_native_with_options(
        UNSIGNED_TEXTURE_ATOMIC_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                state(crate::reflect::RuntimeStorageImageFormat::Rgba32Uint, true),
            )
            .unwrap(),
    )
    .expect_err("vector storage formats cannot implement image atomics");
    assert!(
        vector.contains("cannot implement storage-image atomics"),
        "{vector}"
    );

    let spv = crate::translate_sanitized_native_with_options(
        UNSIGNED_TEXTURE_ATOMIC_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                state(crate::reflect::RuntimeStorageImageFormat::R32Uint, true),
            )
            .unwrap(),
    )
    .expect("scalar atomic specialization");
    let asm = disassemble(&spv).expect("disassemble atomic specialization");
    assert!(asm.contains("R32ui"), "{asm}");
    assert!(asm.contains("OpAtomicUMax"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("atomic specialization spirv-val");
}

#[test]
fn native_write_declared_texture_binds_as_storage_even_if_the_body_only_queries_it() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, ptr addrspace(1) %out) {
entry:
  %w = call i32 @air.get_width_texture_2d(ptr addrspace(1) %dst, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}
declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_write_declared_query_only_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("2D 0 0 0 2"),
        "a write-declared texture binds as a storage image:\n{asm}"
    );
    let reflected = reflection
        .binding_at(crate::reflect::ResourceKind::StorageImage, 0)
        .and_then(|resource| resource.descriptor)
        .expect("reflection reports it as a storage image");
    assert_eq!(
        reflected.binding,
        crate::reflect::STORAGE_TEXTURE_BINDING_BASE
    );
    assert!(
        asm.contains(&format!("Binding {}", reflected.binding)),
        "the module has to decorate the binding reflection reports:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_buffer_texture_size_query_uses_query_size_without_lod() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %t, ptr addrspace(1) %out) {
entry:
  %w = call i32 @air.get_width_texture_buffer(ptr addrspace(1) %t, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}
declare i32 @air.get_width_texture_buffer(ptr addrspace(1), i32)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture_buffer<int, read>", !"air.arg_name", !"t"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_buffer_texture_size_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("a buffer texture size query must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("Buffer 0 0 0 1"),
        "expected a sampled buffer image:\n{asm}"
    );
    assert!(
        asm.contains("OpImageQuerySize ") && !asm.contains("OpImageQuerySizeLod"),
        "a buffer image has no LOD to query with:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn native_array_ref_texture_lowers_to_descriptor_array() {
    let ll = r#"
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @widthTexArray(ptr readonly captures(none) %0, ptr addrspace(1) noundef writeonly captures(none) %1) local_unnamed_addr #0 {
  %3 = load ptr addrspace(1), ptr %0, align 8
  %4 = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) readonly captures(none) %3, i32 0) #1
  store i32 %4, ptr addrspace(1) %1, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1) readonly captures(none), i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(none) }

!air.kernel = !{!0}
!0 = !{ptr @widthTexArray, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"array_ref<texture2d<float, sample>>", !"air.arg_name", !"imgs"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_array_ref_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    // element i reads the texture bound at index i through the per-draw table and the bindless heap
    assert!(
        asm.contains("OpTypeArray") || asm.contains("OpTypeRuntimeArray"),
        "expected an indexed image array:\n{asm}"
    );
    assert!(
        asm.contains("OpAccessChain") && asm.contains("OpImageQuerySizeLod"),
        "expected indexed image access + size query:\n{asm}"
    );
    assert!(
        !asm.contains("is not a logical pointer"),
        "must not emit an illegal pointer load:\n{asm}"
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
fn native_array_ref_write_texture_lowers_to_storage_descriptor_array() {
    let ll = r#"
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @writeTexArrays(ptr readonly captures(none) %0, ptr readonly captures(none) %1) local_unnamed_addr #0 {
  %3 = load ptr addrspace(1), ptr %0, align 8
  tail call void @air.write_texture_2d.i16.v4f32(ptr addrspace(1) %3, <2 x i16> zeroinitializer, <4 x float> zeroinitializer, i16 0, i32 2) #1
  %4 = load ptr addrspace(1), ptr %1, align 8
  tail call void @air.write_texture_2d_array.i16.v4f32(ptr addrspace(1) %4, <2 x i16> zeroinitializer, i16 0, <4 x float> zeroinitializer, i16 0, i32 2) #1
  ret void
}

declare void @air.write_texture_2d.i16.v4f32(ptr addrspace(1), <2 x i16>, <4 x float>, i16, i32) local_unnamed_addr #1
declare void @air.write_texture_2d_array.i16.v4f32(ptr addrspace(1), <2 x i16>, i16, <4 x float>, i16, i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @writeTexArrays, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"array_ref<texture2d<float, write>>", !"air.arg_name", !"dst_a"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"array_ref<texture2d_array<float, write>>", !"air.arg_name", !"dst_b"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_array_ref_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("R32f"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 2, "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert!(asm.contains("Binding 481"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_dynamic_array_ref_write_uses_runtime_descriptor_element() {
    let ll = r#"
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @writeTexArray(ptr readonly captures(none) %textures, i32 %index) local_unnamed_addr #0 {
  %wide = zext i32 %index to i64
  %field = getelementptr inbounds %"struct.metal::texture2d", ptr %textures, i64 %wide, i32 0
  %texture = load ptr addrspace(1), ptr %field, align 8
  tail call void @air.write_texture_2d.i16.v4f32(ptr addrspace(1) %texture, <2 x i16> zeroinitializer, <4 x float> zeroinitializer, i16 0, i32 2) #1
  ret void
}

declare void @air.write_texture_2d.i16.v4f32(ptr addrspace(1), <2 x i16>, <4 x float>, i16, i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @writeTexArray, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"array_ref<texture2d<float, write>>", !"air.arg_name", !"textures"}
!4 = !{i32 1, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_dynamic_array_ref_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("OpAccessChain"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("BuiltIn LocalInvocationIndex"), "{asm}");
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
fn native_byval_fixed_write_texture_array_uses_fixed_descriptor_elements() {
    let ll = r#"
define void @writeFixedArray(ptr readonly byval([2 x ptr addrspace(1)]) %textures) local_unnamed_addr #0 {
entry:
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %textures, i64 0, i64 0
  %tex0 = load ptr addrspace(1), ptr %slot0, align 8
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %tex0, <2 x i16> zeroinitializer, <4 x half> zeroinitializer, i16 0, i32 2) #1
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %textures, i64 0, i64 1
  %tex1 = load ptr addrspace(1), ptr %slot1, align 8
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %tex1, <2 x i16> zeroinitializer, <4 x half> zeroinitializer, i16 0, i32 2) #1
  ret void
}

declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @writeFixedArray, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 2, !"air.write", !"air.arg_type_name", !"array<texture2d<half, write>, 2>", !"air.arg_name", !"textures"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_byval_fixed_write_texture_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 2, "{asm}");
    assert!(
        asm.contains("OpAccessChain"),
        "expected fixed descriptor-array element accesses:\n{asm}"
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
fn native_dead_byval_texture_array_copy_is_removed_after_descriptor_binding() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::texture2d" = type { ptr addrspace(1) }
%"struct.metal::array" = type { [3 x %"struct.metal::texture2d"] }
%Context = type { i32, %"struct.metal::array" }

define <4 x float> @frag(ptr readonly byval([3 x ptr addrspace(1)]) %textures) {
entry:
  %context = alloca %Context, align 8
  %field = getelementptr inbounds %Context, ptr %context, i64 0, i32 1
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 dereferenceable(24) %field, ptr align 8 dereferenceable(24) %textures, i64 24, i1 false)
  ret <4 x float> zeroinitializer
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4}
!4 = !{i32 0, !"air.texture", !"air.location_index", i32 5, i32 3, !"air.sample", !"air.arg_type_name", !"array<texture2d<half, sample>, 3>", !"air.arg_name", !"textures"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_dead_byval_texture_array_copy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
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
fn native_embedded_argument_buffer_write_texture_lowers_to_storage_image() {
    let ll = r#"
%Args = type <{ %"struct.metal::texture2d", i16, [6 x i8] }>
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, <2 x i32> %coord) local_unnamed_addr #0 {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  tail call fastcc void @helper(ptr addrspace(1) %tex, <2 x i32> %coord) #2
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %tex, <2 x i32> %coord) unnamed_addr #1 {
entry:
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %tex, <2 x i32> %coord, <4 x float> zeroinitializer, i32 0, i32 2) #3
  ret void
}

declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32) local_unnamed_addr #3

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind }
attributes #2 = { convergent nounwind }
attributes #3 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !7}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Args", !"air.arg_name", !"args"}
!4 = !{i32 0, i32 8, i32 0, !"texture2d<float, write>", !"output", !"air.indirect_argument", !5, i32 8, i32 2, i32 0, !"short", !"radius", !"air.indirect_argument", !6}
!5 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"output"}
!6 = !{i32 1, !"air.indirect_constant", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"short", !"air.arg_name", !"radius"}
!7 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"coord"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_embedded_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("R32f"), "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 1, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }

    let custom_layout = crate::reflect::DescriptorLayout {
        set: 4,
        storage_textures: crate::reflect::DescriptorBindingRange {
            start: 800,
            end: 928,
        },
        ..Default::default()
    };
    let (custom_spv, custom_reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_descriptor_layout(custom_layout)
            .expect("custom embedded-texture layout"),
    )
    .expect("custom embedded-texture translation");
    let custom_asm = disassemble(&custom_spv).expect("disassemble custom embedded texture");
    assert!(custom_asm.contains("DescriptorSet 4"), "{custom_asm}");
    assert!(custom_asm.contains("Binding 800"), "{custom_asm}");
    assert_eq!(custom_reflection.descriptor_layout, custom_layout);
    assert_eq!(
        custom_reflection
            .binding_at(crate::reflect::ResourceKind::EmbeddedArgBufferTexture, 0,)
            .and_then(|binding| binding.descriptor),
        Some(crate::reflect::DescriptorLocation {
            set: 4,
            binding: 800,
            count: 1,
        })
    );
    tools::spirv_val_bytes(&custom_spv, &tmp).expect("custom embedded texture spirv-val");

    let state = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Rgba8Unorm,
        false,
        false,
    );
    let (specialized, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, state)
            .unwrap(),
    )
    .expect("specialize embedded storage texture");
    let specialized_asm = disassemble(&specialized).expect("disassemble specialized texture");
    assert!(specialized_asm.contains("Rgba8"), "{specialized_asm}");
    assert!(!specialized_asm.contains("R32f"), "{specialized_asm}");
    let binding = reflection
        .binding_at(crate::reflect::ResourceKind::EmbeddedArgBufferTexture, 0)
        .expect("reflected embedded storage texture");
    assert_eq!(
        binding.texture_shape.and_then(|shape| shape.storage_format),
        Some(crate::meta::TextureFormat::Rgba8)
    );
    assert_eq!(
        reflection.runtime_storage_image_specializations,
        [crate::reflect::RuntimeStorageImageSpecialization {
            metal_index: 0,
            state,
            spirv_format: Some(crate::meta::TextureFormat::Rgba8),
        }]
    );
    tools::spirv_val_bytes(&specialized, &tmp).expect("specialized spirv-val");

    let error = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                runtime_storage_image_state(
                    crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
                    false,
                    false,
                ),
            )
            .unwrap(),
    )
    .expect_err("embedded formatless write requires the host feature");
    assert!(error.contains("write-without-format"), "{error}");

    let formatless_state = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        false,
        true,
    );
    let (formatless, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, formatless_state)
            .unwrap(),
    )
    .expect("embedded formatless write uses the supplied host feature");
    let formatless_asm = disassemble(&formatless).expect("disassemble formatless texture");
    assert!(
        formatless_asm.contains("OpCapability StorageImageWriteWithoutFormat"),
        "{formatless_asm}"
    );
    assert!(formatless_asm.contains("2 Unknown"), "{formatless_asm}");
    assert_eq!(
        reflection.runtime_storage_image_specializations,
        [crate::reflect::RuntimeStorageImageSpecialization {
            metal_index: 0,
            state: formatless_state,
            spirv_format: None,
        }]
    );
    tools::spirv_val_bytes(&formatless, &tmp).expect("formatless embedded spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_embedded_fixed_texture_array_materializes_dynamic_placeholder_use() {
    let ll = r#"
%Args = type <{ i64, [2 x %"struct.metal::texture2d"] }>
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, i32 %index) local_unnamed_addr #0 {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 1, i32 %index, i32 0
  %tex = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %tex, <2 x i16> zeroinitializer, <4 x half> zeroinitializer, i16 0, i32 2) #1
  ret void
}

declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !6}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 24, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Args", !"air.arg_name", !"args"}
!4 = !{i32 8, i32 16, i32 0, !"array<texture2d<half, write>, 2>", !"outputs", !"air.indirect_argument", !5}
!5 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 2, !"air.write", !"air.arg_type_name", !"array<texture2d<half, write>, 2>", !"air.arg_name", !"outputs"}
!6 = !{i32 1, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_embedded_fixed_texture_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeArray"), "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(asm.contains("OpAccessChain"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 1, "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    let specialized = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                runtime_storage_image_state(
                    crate::reflect::RuntimeStorageImageFormat::R16Float,
                    false,
                    false,
                ),
            )
            .unwrap(),
    )
    .expect("specialize embedded storage texture array");
    let specialized_asm = disassemble(&specialized).expect("disassemble specialized array");
    assert!(specialized_asm.contains("R16f"), "{specialized_asm}");
    assert!(!specialized_asm.contains("Rgba16f"), "{specialized_asm}");
    assert_eq!(
        specialized_asm.matches("OpImageWrite").count(),
        1,
        "{specialized_asm}"
    );
    tools::spirv_val_bytes(&specialized, &tmp).expect("specialized array spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_embedded_argument_buffer_routes_multiple_write_texture_shapes_by_field() {
    let ll = r#"
%Args = type <{ %"struct.metal::texture2d_array", %"struct.metal::texture2d" }>
%"struct.metal::texture2d_array" = type { ptr addrspace(1) }
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, <2 x i32> %coord) local_unnamed_addr #0 {
entry:
  %array_field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0, i32 0
  %array_tex = load ptr addrspace(1), ptr addrspace(2) %array_field, align 8
  %plain_field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 1, i32 0
  %plain_tex = load ptr addrspace(1), ptr addrspace(2) %plain_field, align 8
  tail call void @air.write_texture_2d_array.v4f32(ptr addrspace(1) %array_tex, <2 x i32> %coord, i32 0, <4 x float> zeroinitializer, i32 0, i32 2) #1
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %plain_tex, <2 x i32> %coord, <4 x float> zeroinitializer, i32 0, i32 2) #1
  ret void
}

declare void @air.write_texture_2d_array.v4f32(ptr addrspace(1), <2 x i32>, i32, <4 x float>, i32, i32) local_unnamed_addr #1
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32) local_unnamed_addr #1

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !7}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Args", !"air.arg_name", !"args"}
!4 = !{i32 0, i32 8, i32 0, !"texture2d_array<float, write>", !"array_output", !"air.indirect_argument", !5, i32 8, i32 8, i32 0, !"texture2d<float, write>", !"plain_output", !"air.indirect_argument", !6}
!5 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<float, write>", !"air.arg_name", !"array_output"}
!6 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"plain_output"}
!7 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"coord"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_embedded_multi_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert!(asm.contains("Binding 481"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 2, "{asm}");
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
fn native_embedded_argument_buffer_field_survives_nested_helper_inlining() {
    let ll = r#"
%Args = type <{ %"struct.metal::texture2d", %"struct.metal::texture2d" }>
%"struct.metal::texture2d" = type { ptr addrspace(1) }

define void @k(ptr addrspace(2) %args, <2 x i32> %coord) local_unnamed_addr #0 {
entry:
  tail call fastcc void @outer(ptr addrspace(2) %args, <2 x i32> %coord) #2
  ret void
}

define internal fastcc void @outer(ptr addrspace(2) %buffer, <2 x i32> %coord) unnamed_addr #1 {
entry:
  tail call fastcc void @inner(ptr addrspace(2) %buffer, <2 x i32> %coord) #2
  ret void
}

define internal fastcc void @inner(ptr addrspace(2) %buffer, <2 x i32> %coord) unnamed_addr #1 {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %buffer, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %tex, <2 x i32> %coord, <4 x float> zeroinitializer, i32 0, i32 2) #3
  ret void
}

declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32) local_unnamed_addr #3

attributes #0 = { convergent nounwind }
attributes #1 = { convergent nounwind }
attributes #2 = { convergent nounwind }
attributes #3 = { convergent nounwind memory(argmem: write) }

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !7}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Args", !"air.arg_name", !"args"}
!4 = !{i32 0, i32 8, i32 0, !"texture2d<float, write>", !"output", !"air.indirect_argument", !5, i32 8, i32 8, i32 0, !"texture2d<float, write>", !"other", !"air.indirect_argument", !6}
!5 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"output"}
!6 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"other"}
!7 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"coord"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_embedded_nested_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert!(asm.contains("Binding 481"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 1, "{asm}");
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
fn native_get_null_texture_models_unmodeled_placeholder() {
    let ll = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  %tex = call ptr addrspace(1) @air.get_null_texture_2d()
  %width = call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %width, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(1) @air.get_null_texture_2d()
declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_null_texture_store_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("null-texture kernel must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpFunction"), "{asm}");
    assert!(asm.contains("Binding 32"), "{asm}");
    for line in asm.lines() {
        let Some(binding) = line
            .split_whitespace()
            .collect::<Vec<_>>()
            .split_first()
            .and_then(|(head, rest)| (*head == "OpDecorate").then_some(rest))
            .and_then(|rest| match rest {
                [_, "Binding", value] => value.parse::<u32>().ok(),
                _ => None,
            })
        else {
            continue;
        };
        assert!(
            !crate::reflect::SAMPLER_BINDING_RANGE.contains(binding)
                && !crate::reflect::COLOR_INPUT_BINDING_RANGE.contains(binding),
            "the translator-owned placeholder must stay in the texture band; got:\n{asm}"
        );
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_late_null_texture_does_not_poison_disjoint_aggregate_fields() {
    let ll = r#"
%Holder = type { i32, ptr addrspace(1) }

define void @k(ptr addrspace(1) %out) {
entry:
  %tex = call ptr addrspace(1) @air.get_null_texture_2d()
  %with_value = insertvalue %Holder poison, i32 7, 0
  %with_texture = insertvalue %Holder %with_value, ptr addrspace(1) %tex, 1
  %value = extractvalue %Holder %with_texture, 0
  store i32 %value, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(1) @air.get_null_texture_2d()

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_late_null_texture_aggregate_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("late null texture must not invalidate a disjoint aggregate field");
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
fn native_texture_parameter_round_trips_through_private_aggregate_field() {
    let ll = r#"
%Holder = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %holder = alloca %Holder, align 8
  %slot = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 0
  store ptr addrspace(1) %tex, ptr %slot, align 8
  %loaded = load ptr addrspace(1), ptr %slot, align 8
  %width = call i32 @air.get_width_texture_2d(ptr addrspace(1) %loaded, i32 0)
  store i32 %width, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_private_texture_field_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("private texture field must preserve its resource identity");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
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
fn native_is_null_texture_of_get_null_texture_lowers_to_true() {
    let ll = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  %tex = call ptr addrspace(1) @air.get_null_texture_2d()
  %isnull = call i1 @air.is_null_texture_2d(ptr addrspace(1) %tex)
  br i1 %isnull, label %null, label %bound
null:
  store i32 1, ptr addrspace(1) %out, align 4
  br label %done
bound:
  store i32 0, ptr addrspace(1) %out, align 4
  br label %done
done:
  ret void
}

declare ptr addrspace(1) @air.get_null_texture_2d()
declare i1 @air.is_null_texture_2d(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_null_texture_predicate_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("is-null-texture kernel must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("OpConstantTrue"),
        "is_null_texture(get_null_texture()) must lower to constant TRUE; got:\n{asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_texture_sampler_metadata_bindings() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler) {
entry:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_texture_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeImage") && line.contains(" 2D ")),
        "{asm}"
    );
    assert!(asm.contains("OpTypeSampler"), "{asm}");
    assert!(asm.contains("Binding 37"), "{asm}");
    assert!(asm.contains("Binding 162"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_sampler_in_by_value_aggregate_survives_multiblock_helper_inlining() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Fetcher = type { ptr addrspace(1), ptr addrspace(2) }

define void @k(ptr addrspace(1) %texture, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %with_texture = insertvalue %Fetcher poison, ptr addrspace(1) %texture, 0
  %fetcher = insertvalue %Fetcher %with_texture, ptr addrspace(2) %sampler, 1
  call fastcc void @sample_helper(%Fetcher %fetcher, ptr addrspace(1) %out)
  ret void
}

define internal fastcc void @sample_helper(%Fetcher %fetcher, ptr addrspace(1) %out) {
entry:
  br label %body
body:
  %texture = extractvalue %Fetcher %fetcher, 0
  %sampler = extractvalue %Fetcher %fetcher, 1
  %sample = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %texture, ptr addrspace(2) %sampler, <2 x float> zeroinitializer, i1 false, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"texture"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_by_value_sampler_helper_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("translate sampler carried through by-value helper aggregate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpSampledImage"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_texture_and_sampler_binding_bands_cover_high_and_last_abi_indices() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex40, ptr addrspace(1) %tex127, ptr addrspace(2) %sampler8, ptr addrspace(2) %sampler15) {
entry:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 40, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"tex40"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 127, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"tex127"}
!5 = !{i32 2, !"air.sampler", !"air.location_index", i32 8, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler8"}
!6 = !{i32 3, !"air.sampler", !"air.location_index", i32 15, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler15"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_high_texture_sampler_bindings_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("high-index resources must translate");
    let asm = disassemble(&spv).expect("disassemble");

    for (kind, metal_index, binding) in [
        (crate::reflect::ResourceKind::Texture, 40, 72),
        (crate::reflect::ResourceKind::Texture, 127, 159),
        (crate::reflect::ResourceKind::Sampler, 8, 168),
        (crate::reflect::ResourceKind::Sampler, 15, 175),
    ] {
        let reflected = reflection
            .binding_at(kind, metal_index)
            .and_then(|resource| resource.descriptor)
            .expect("reflected descriptor");
        assert_eq!(reflected.set, 0);
        assert_eq!(reflected.binding, binding);
        assert!(asm.contains(&format!("Binding {binding}")), "{asm}");
    }
    reflection
        .validate_descriptor_abi()
        .expect("reflection descriptor ABI");

    let mut collided = load_bytes(&spv).expect("load translated module");
    for decoration in &mut collided.annotations {
        if decoration.class.opcode == Op::Decorate
            && decoration.operands.get(1) == Some(&Operand::Decoration(Decoration::Binding))
            && decoration.operands.get(2) == Some(&Operand::LiteralBit32(168))
        {
            decoration.operands[2] = Operand::LiteralBit32(72);
        }
    }
    let error = passes::validate_descriptor_bindings(
        &collided,
        crate::reflect::DescriptorLayout::default(),
    )
    .expect_err("a sampler moved into the texture band must be rejected");
    assert!(error.contains("outside its ABI band"), "{error}");

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
fn native_sampled_and_storage_views_of_one_metal_texture_use_distinct_bands() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@dest_location = internal addrspace(2) global i32 40, align 4
@source_location = internal addrspace(2) global i32 40, align 4
@dest_enabled = internal addrspace(2) global i8 1, align 1
@source_enabled = internal addrspace(2) global i8 1, align 1

define void @k(ptr addrspace(1) %dest, ptr addrspace(1) %source) {
entry:
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.function_constant", !5, !"air.texture", !"air.location_index", ptr addrspace(2) @dest_location, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dest"}
!4 = !{i32 1, !"air.function_constant", !6, !"air.texture", !"air.location_index", ptr addrspace(2) @source_location, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"source"}
!5 = !{ptr addrspace(2) @dest_enabled, !"bool", !"dest_enabled"}
!6 = !{ptr addrspace(2) @source_enabled, !"bool", !"source_enabled"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_sampled_storage_texture_bindings_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default(),
    )
    .expect("sampled and storage texture views must translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sampled = reflection
        .binding_at(crate::reflect::ResourceKind::Texture, 40)
        .and_then(|resource| resource.descriptor)
        .expect("sampled descriptor");
    let storage = reflection
        .binding_at(crate::reflect::ResourceKind::StorageImage, 40)
        .and_then(|resource| resource.descriptor)
        .expect("storage descriptor");
    assert_eq!(sampled.binding, 72);
    assert_eq!(storage.binding, 520);
    assert!(asm.contains("Binding 72"), "{asm}");
    assert!(asm.contains("Binding 520"), "{asm}");
    reflection
        .validate_descriptor_abi()
        .expect("reflection descriptor ABI");
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
fn native_kernel_texture_sample_without_lod_uses_explicit_lod_zero() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 2.500000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 7.500000e-01, i32 1
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> %coord)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  %x = extractelement <4 x float> %color, i64 0
  store float %x, ptr addrspace(1) %out, align 4
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_sample_explicit_lod0_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(!asm.contains("OpImageSampleImplicitLod"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_texture_sample_const_offset_uses_image_operand() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 2.500000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 7.500000e-01, i32 1
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> %coord, i1 true, <2 x i32> <i32 1, i32 -1>, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_sample_const_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSampleExplicitLod"))
        .expect("find OpImageSampleExplicitLod");
    assert!(sample.contains("Lod"), "{sample}\n\n{asm}");
    assert!(sample.contains("ConstOffset"), "{sample}\n\n{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn runtime_sampler_specialization_switches_the_same_air_between_native_and_pixel_sampling() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> <float 2.500000e+00, float -5.000000e-01>, i1 true, <2 x i32> <i32 1, i32 -1>, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_specialization_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let normalized_state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Normalized,
        crate::reflect::SamplerFilter::Linear,
        crate::reflect::SamplerAddressMode::ClampToZero,
    );
    let normalized_options = passes::TransformOptions::default()
        .with_runtime_sampler(0, normalized_state)
        .unwrap();
    let (normalized_spv, normalized_reflection) =
        crate::translate_sanitized_native_reflected(ll, Stage::Kernel, &tmp, normalized_options)
            .expect("normalized specialization");
    let normalized_asm = disassemble(&normalized_spv).expect("disassemble normalized");
    assert!(normalized_asm.contains("OpImageSampleExplicitLod"));
    assert!(normalized_asm.contains("ConstOffset"));
    assert_eq!(
        normalized_reflection.runtime_sampler_specializations,
        vec![crate::reflect::RuntimeSamplerSpecialization {
            metal_index: 0,
            state: normalized_state,
        }]
    );

    let pixel_state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Linear,
        crate::reflect::SamplerAddressMode::ClampToZero,
    );
    let pixel_options = passes::TransformOptions::default()
        .with_runtime_sampler(0, pixel_state)
        .unwrap();
    let (pixel_spv, pixel_reflection) =
        crate::translate_sanitized_native_reflected(ll, Stage::Kernel, &tmp, pixel_options)
            .expect("pixel specialization");
    let pixel_asm = disassemble(&pixel_spv).expect("disassemble pixel");
    assert!(pixel_asm.contains("OpImageFetch"), "{pixel_asm}");
    assert!(pixel_asm.contains("OpIAdd"), "{pixel_asm}");
    assert!(pixel_asm.contains("OpLogicalAnd"), "{pixel_asm}");
    assert!(!pixel_asm.contains("OpSampledImage"), "{pixel_asm}");
    assert!(!pixel_asm.contains("OpImageSample"), "{pixel_asm}");
    assert_eq!(
        pixel_reflection.runtime_sampler_specializations,
        vec![crate::reflect::RuntimeSamplerSpecialization {
            metal_index: 0,
            state: pixel_state,
        }]
    );
    tools::spirv_val_bytes(&pixel_spv, &tmp).expect("pixel spirv-val");

    for (address, required, forbidden) in [
        (
            crate::reflect::SamplerAddressMode::ClampToEdge,
            "OpImageFetch",
            "OpLogicalAnd",
        ),
        (
            crate::reflect::SamplerAddressMode::Repeat,
            "OpSMod",
            "OpLogicalAnd",
        ),
        (
            crate::reflect::SamplerAddressMode::MirroredRepeat,
            "OpSGreaterThanEqual",
            "OpLogicalAnd",
        ),
    ] {
        let state = runtime_sampler_state(
            crate::reflect::SamplerCoordinates::Pixel,
            crate::reflect::SamplerFilter::Nearest,
            address,
        );
        let options = passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .unwrap();
        let spv = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
            .expect("pixel address-mode specialization");
        let asm = disassemble(&spv).expect("disassemble pixel address mode");
        assert!(
            asm.contains(required),
            "missing {required} for {address:?}\n{asm}"
        );
        assert!(
            !asm.contains(forbidden),
            "unexpected {forbidden} for {address:?}\n{asm}"
        );
        assert!(!asm.contains("OpSampledImage"), "{address:?}\n{asm}");
        assert!(!asm.contains("OpImageSample"), "{address:?}\n{asm}");
        tools::spirv_val_bytes(&spv, &tmp).expect("address-mode spirv-val");
    }

    let border_state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::ClampToBorder,
    );
    let border_spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_sampler(0, border_state)
            .unwrap(),
    )
    .expect("transparent border specialization");
    let border_asm = disassemble(&border_spv).expect("disassemble border mode");
    assert!(border_asm.contains("OpLogicalAnd"), "{border_asm}");
    assert!(!border_asm.contains("OpSampledImage"), "{border_asm}");
    tools::spirv_val_bytes(&border_spv, &tmp).expect("border-mode spirv-val");
}

#[test]
fn runtime_pixel_sampler_rejects_unmodeled_pipeline_state() {
    let base = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::ClampToEdge,
    );
    let cases = [
        (
            crate::reflect::RuntimeSamplerState {
                mag_filter: crate::reflect::SamplerFilter::Linear,
                ..base
            },
            "mixed min/mag",
        ),
        (
            crate::reflect::RuntimeSamplerState {
                mip_filter: crate::reflect::SamplerMipFilter::Linear,
                ..base
            },
            "linear mip",
        ),
        (
            crate::reflect::RuntimeSamplerState {
                max_anisotropy: 2,
                ..base
            },
            "anisotropy",
        ),
        (
            crate::reflect::RuntimeSamplerState {
                lod_bias: 1.0,
                ..base
            },
            "LOD bias",
        ),
        (
            crate::reflect::RuntimeSamplerState {
                lod_min_clamp: 1.0,
                lod_max_clamp: 2.0,
                ..base
            },
            "minimum LOD clamp",
        ),
        (
            crate::reflect::RuntimeSamplerState {
                address_mode_s: crate::reflect::SamplerAddressMode::ClampToBorder,
                border_color: crate::reflect::SamplerBorderColor::OpaqueWhite,
                ..base
            },
            "transparent-black border",
        ),
    ];
    for (state, expected) in cases {
        let error = passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .expect_err("unsupported state must be refused");
        assert!(error.contains(expected), "{error}");
    }
    let error = passes::TransformOptions::default()
        .with_runtime_sampler(16, base)
        .expect_err("out-of-range sampler index");
    assert!(error.contains("range 0..16"), "{error}");
}

#[test]
fn runtime_pixel_sampler_accepts_the_default_lod_maximum() {
    let base = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::ClampToEdge,
    );
    for lod_max_clamp in [0.0, 1.0, 65504.0] {
        passes::TransformOptions::default()
            .with_runtime_sampler(
                0,
                crate::reflect::RuntimeSamplerState {
                    lod_max_clamp,
                    ..base
                },
            )
            .unwrap_or_else(|error| {
                panic!("a maximum LOD of {lod_max_clamp} cannot exclude level zero: {error}")
            });
    }
}

#[test]
fn air_static_pixel_sampler_with_mixed_filters_is_refused() {
    let state = crate::reflect::StaticSamplerState::from_air_words([34901797601050624, 0])
        .expect("decode AIR sampler words");
    assert_eq!(state.coordinates, crate::reflect::SamplerCoordinates::Pixel);
    let mixed = crate::reflect::StaticSamplerState {
        mag_filter: crate::reflect::SamplerFilter::Linear,
        min_filter: crate::reflect::SamplerFilter::Nearest,
        ..state
    };
    let error = mixed
        .validate_lowering()
        .expect_err("mixed min/mag filters have no pixel-coordinate lowering");
    assert!(error.contains("mixed min/mag"), "{error}");
    state
        .validate_lowering()
        .expect("a corpus-shaped constexpr sampler lowers");
    assert_eq!(state.lod_max_clamp, 65504.0);
}

#[test]
fn runtime_sampler_specialization_uses_fragment_metal_index_not_descriptor_binding() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @frag(ptr addrspace(1) %tex, ptr addrspace(2) %sampler) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> <float 2.500000e+00, float -5.000000e-01>, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{!4, !5}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!4 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!5 = !{i32 1, !"air.sampler", !"air.location_index", i32 3, i32 1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_runtime_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::Repeat,
    );
    let options = passes::TransformOptions::default()
        .with_runtime_sampler(3, state)
        .unwrap()
        .with_runtime_sampler(7, state)
        .unwrap();
    let (spv, reflection) =
        crate::translate_sanitized_native_reflected(ll, Stage::Fragment, &tmp, options)
            .expect("fragment specialization");
    let asm = disassemble(&spv).expect("disassemble fragment specialization");
    assert!(asm.contains("Binding 163"), "{asm}");
    assert!(asm.contains("OpSMod"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert_eq!(
        reflection.runtime_sampler_specializations,
        [crate::reflect::RuntimeSamplerSpecialization {
            metal_index: 3,
            state,
        }]
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("fragment spirv-val");
}

#[test]
fn runtime_pixel_sampler_linearly_filters_1d_with_two_exact_fetches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_1d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, float 2.500000e+00, i1 true, i32 1, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_1d.v4f32(ptr addrspace(1), ptr addrspace(2), float, i1, i32, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture1d<float, sample>"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_dimension_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Linear,
        crate::reflect::SamplerAddressMode::ClampToEdge,
    );
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .unwrap(),
    )
    .expect("1D pixel-linear specialization");
    let asm = disassemble(&spv).expect("disassemble 1D pixel-linear specialization");
    assert_eq!(asm.matches("OpImageFetch").count(), 2, "{asm}");
    assert_eq!(asm.matches("OpVectorTimesScalar").count(), 2, "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("1D pixel-linear spirv-val");
}

#[test]
fn runtime_pixel_comparison_sampler_fetches_then_compares() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %sample = tail call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, i32 0, <2 x float> <float 2.500000e+00, float -5.000000e-01>, float 5.000000e-01, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, float, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_compare_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let mut state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::ClampToZero,
    );
    state.compare_function = crate::reflect::SamplerCompareFunction::LessEqual;
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .unwrap(),
    )
    .expect("pixel comparison specialization");
    let asm = disassemble(&spv).expect("disassemble pixel comparison");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpFOrdLessThanEqual"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("comparison spirv-val");

    state.compare_function = crate::reflect::SamplerCompareFunction::None;
    let error = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .unwrap(),
    )
    .expect_err("comparison-disabled sampler must be refused");
    assert!(error.contains("comparison enabled"), "{error}");
}

#[test]
fn runtime_sampler_specialization_refuses_pointer_selected_state_loss() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler0, ptr addrspace(2) %sampler1, ptr addrspace(1) %out, i32 %lane) {
entry:
  %condition = icmp eq i32 %lane, 0
  %selected = select i1 %condition, ptr addrspace(2) %sampler0, ptr addrspace(2) %sampler1
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %selected, <2 x float> <float 2.500000e+00, float -5.000000e-01>, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1}
!5 = !{i32 2, !"air.sampler", !"air.location_index", i32 1, i32 1}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*"}
!7 = !{i32 4, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_select_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::ClampToEdge,
    );
    let options = passes::TransformOptions::default()
        .with_runtime_sampler(0, state)
        .unwrap()
        .with_runtime_sampler(1, state)
        .unwrap();
    let error = crate::translate_sanitized_native_with_options(ll, Stage::Kernel, &tmp, options)
        .expect_err("pointer-selected runtime sampler state must not silently disappear");
    assert!(error.contains("pointer selection"), "{error}");
}

#[test]
fn runtime_sampler_specialization_refuses_integer_lod_query_pointer_state_loss() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @frag(<4 x float> %position, ptr addrspace(1) %tex, ptr addrspace(2) %sampler0, ptr addrspace(2) %sampler1) {
entry:
  %x = extractelement <4 x float> %position, i32 0
  %condition = fcmp oge float %x, 0.000000e+00
  %selected = select i1 %condition, ptr addrspace(2) %sampler0, ptr addrspace(2) %sampler1
  %lod = tail call fast float @air.calculate_unclamped_lod_texture_2d(ptr addrspace(1) readonly captures(none) %tex, ptr addrspace(2) readonly captures(none) %selected, <2 x float> <float 2.500000e-01, float 7.500000e-01>, i32 0)
  %out = insertelement <4 x float> zeroinitializer, float %lod, i32 0
  ret <4 x float> %out
}

declare float @air.calculate_unclamped_lod_texture_2d(ptr addrspace(1) readonly captures(none), ptr addrspace(2) readonly captures(none), <2 x float>, i32)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{!4, !5, !6, !7}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>"}
!6 = !{i32 2, !"air.sampler", !"air.location_index", i32 0, i32 1}
!7 = !{i32 3, !"air.sampler", !"air.location_index", i32 1, i32 1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_integer_lod_select_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let mut state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Normalized,
        crate::reflect::SamplerFilter::Linear,
        crate::reflect::SamplerAddressMode::ClampToEdge,
    );
    state.lod_bias = 0.5;
    state.lod_max_clamp = 8.0;
    let options = passes::TransformOptions::default()
        .with_runtime_sampler(0, state)
        .unwrap()
        .with_runtime_sampler(1, state)
        .unwrap();
    let error = crate::translate_sanitized_native_with_options(ll, Stage::Fragment, &tmp, options)
        .expect_err("integer LOD query must not replace a lost runtime specialization");
    assert!(error.contains("pointer selection"), "{error}");
}

#[test]
fn runtime_pixel_sampler_specializes_texture_gather() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %gather = tail call { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> <float 2.500000e+00, float -5.000000e-01>, i1 true, <2 x i32> <i32 1, i32 -1>, i32 0, i32 0)
  %color = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_sampler_gather_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let state = runtime_sampler_state(
        crate::reflect::SamplerCoordinates::Pixel,
        crate::reflect::SamplerFilter::Nearest,
        crate::reflect::SamplerAddressMode::Repeat,
    );
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_sampler(0, state)
            .unwrap(),
    )
    .expect("pixel gather specialization");
    let asm = disassemble(&spv).expect("disassemble pixel gather");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpSMod"), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("pixel gather spirv-val");
}

#[test]
fn native_kernel_texture_sample_dynamic_offset_adjusts_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, ptr addrspace(1) %out, i32 %lane) {
entry:
  %c0 = insertelement <2 x float> poison, float 2.500000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 7.500000e-01, i32 1
  %x = or i32 %lane, -8
  %o0 = insertelement <2 x i32> undef, i32 %x, i32 0
  %offset = insertelement <2 x i32> %o0, i32 -5, i32 1
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, <2 x float> %coord, i1 true, <2 x i32> %offset, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"int", !"air.arg_name", !"lane"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_sample_dynamic_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSampleExplicitLod"))
        .expect("find OpImageSampleExplicitLod");
    assert!(sample.contains("Lod"), "{sample}\n\n{asm}");
    assert!(!sample.contains("ConstOffset"), "{sample}\n\n{asm}");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpFDiv"), "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_depth_sample_uses_explicit_lod_zero() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 2.500000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 7.500000e-01, i32 1
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, i32 0, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_depth_sample_explicit_lod0_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(!asm.contains("OpImageSampleImplicitLod"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_fragment_depth_sample_level_flag_selects_explicit_lod() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth) {
entry:
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 true, float 2.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  %out0 = insertelement <4 x float> zeroinitializer, float %value, i32 0
  ret <4 x float> %out0
}

declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_depth_sample_explicit_level_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSampleExplicitLod"))
        .expect("find OpImageSampleExplicitLod");
    assert!(sample.contains("Lod"), "{sample}\n\n{asm}");
    assert!(!asm.contains("OpImageSampleImplicitLod"), "{asm}");
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
fn native_fragment_depth_sample_without_the_level_flag_stays_implicit() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth) {
entry:
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  %out0 = insertelement <4 x float> zeroinitializer, float %value, i32 0
  ret <4 x float> %out0
}

declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_depth_sample_implicit_level_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageSampleImplicitLod"), "{asm}");
    assert!(!asm.contains("OpImageSampleExplicitLod"), "{asm}");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_fragment_depth_sample_constant_offset_reaches_the_sample() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth) {
entry:
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, i1 true, <2 x i32> <i32 1, i32 -1>, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  %out0 = insertelement <4 x float> zeroinitializer, float %value, i32 0
  ret <4 x float> %out0
}

declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_depth_sample_const_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSample"))
        .expect("find the depth sample");
    assert!(sample.contains("ConstOffset"), "{sample}\n\n{asm}");
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
fn native_fragment_depth_sample_dynamic_offset_adjusts_the_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth, ptr addrspace(1) %offsets) {
entry:
  %off = load <2 x i32>, ptr addrspace(1) %offsets, align 8
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, i1 true, <2 x i32> %off, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  %out0 = insertelement <4 x float> zeroinitializer, float %value, i32 0
  ret <4 x float> %out0
}

declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!7 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"int2*", !"air.arg_name", !"offsets"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_depth_sample_dynamic_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpFDiv"), "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
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
fn native_fragment_compare_depth_constant_offset_reaches_the_sample() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth) {
entry:
  %s = tail call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, float 5.000000e-01, i1 true, <2 x i32> <i32 1, i32 -1>, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %v = extractvalue { float, i8 } %s, 0
  %o = insertelement <4 x float> zeroinitializer, float %v, i32 0
  ret <4 x float> %o
}

declare { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, float, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_compare_depth_const_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSample"))
        .expect("find the compare-depth sample");
    assert!(sample.contains("ConstOffset"), "{sample}\n\n{asm}");
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
fn native_fragment_compare_depth_dynamic_offset_adjusts_the_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth, ptr addrspace(1) %offs) {
entry:
  %off = load <2 x i32>, ptr addrspace(1) %offs, align 8
  %s = tail call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> %coord, float 5.000000e-01, i1 true, <2 x i32> %off, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %v = extractvalue { float, i8 } %s, 0
  %o = insertelement <4 x float> zeroinitializer, float %v, i32 0
  ret <4 x float> %o
}

declare { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, float, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!7 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"int2*", !"air.arg_name", !"offs"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_compare_depth_dynamic_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpFDiv"), "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
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
fn native_kernel_pixel_linear_depth_sample_uses_four_addressed_fetches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601053184, i64 0], align 8
define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out) {
entry:
  %sample = tail call { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 0, <2 x float> <float 2.500000e-01, float 7.500000e-01>, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}
declare { float, i8 } @air.sample_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i1, float, float, i32)
!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_linear_depth_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_depth_2d_array_sample_appends_the_air_layer_operand() {
    let ll = r#"
define void @k(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, ptr addrspace(1) %out) {
entry:
  %sample = tail call { float, i8 } @air.sample_depth_2d_array.f32(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, i32 1, <2 x float> <float 2.500000e-01, float 7.500000e-01>, i32 3, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value = extractvalue { float, i8 } %sample, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare { float, i8 } @air.sample_depth_2d_array.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i32, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d_array<float, sample>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_depth_2d_array_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeVector") && line.ends_with(" 3")),
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
fn native_fragment_sampler_select_over_distinct_static_states_is_refused() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8
@__air_sampler_state.1 = internal addrspace(2) constant i64 -9188470239253755831, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %edge = extractelement <2 x float> %coord, i64 0
  %wide = fcmp oge float %edge, 1.000000e+00
  %selected = select i1 %wide, ptr addrspace(2) @__air_sampler_state, ptr addrspace(2) @__air_sampler_state.1
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %selected, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8, !9}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
!9 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_static_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp)
        .expect_err("a sampler selected between two distinct states must not be answered");
    assert!(
        error.contains("sampler operand is a pointer"),
        "unexpected refusal: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_cube_read_through_air_read_sampler_uses_the_synthesized_default_sampler() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %cube, ptr addrspace(2) %smp, ptr addrspace(1) %out, <2 x i32> %gid) {
entry:
  %readsmp = tail call ptr addrspace(2) @air.get_read_sampler()
  %read = tail call { <4 x float>, i8 } @air.read_texture_cube.i32.v4f32(ptr addrspace(1) %cube, ptr addrspace(2) %readsmp, <2 x i32> %gid, i32 0, i32 0, i32 0)
  %texel = extractvalue { <4 x float>, i8 } %read, 0
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_cube.v4f32(ptr addrspace(1) %cube, ptr addrspace(2) %smp, <3 x float> <float 1.0, float 0.0, float 0.0>, i1 true, float 0.000000e+00, float 0.000000e+00, i32 0)
  %sampled = extractvalue { <4 x float>, i8 } %sample, 0
  %sum = fadd <4 x float> %texel, %sampled
  %x = extractelement <4 x float> %sum, i32 0
  store float %x, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(2) @air.get_read_sampler()
declare { <4 x float>, i8 } @air.read_texture_cube.i32.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x i32>, i32, i32, i32)
declare { <4 x float>, i8 } @air.sample_texture_cube.v4f32(ptr addrspace(1), ptr addrspace(2), <3 x float>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texturecube<float, sample>", !"air.arg_name", !"cube"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"smp"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_cube_read_default_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Cube 0 0 0 1 Unknown"), "{asm}");
    assert_eq!(
        asm.matches("OpImageSampleExplicitLod").count(),
        2,
        "both the cube read and the cube sample lower to explicit-LOD samples: {asm}"
    );
    assert!(asm.contains("Binding 160"), "{asm}");
    assert!(asm.contains("Binding 161"), "{asm}");
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
fn native_fragment_compare_depth_sampler_select_over_distinct_static_states_is_refused() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8
@__air_sampler_state.1 = internal addrspace(2) constant i64 -9188470239253755831, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %depth) {
entry:
  %x = extractelement <4 x float> %position, i32 0
  %cond = fcmp ogt float %x, 0.000000e+00
  %selected = select i1 %cond, ptr addrspace(2) @__air_sampler_state, ptr addrspace(2) @__air_sampler_state.1
  %shadow_sample = tail call { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1) %depth, ptr addrspace(2) %selected, i32 0, <2 x float> %coord, float 5.000000e-01, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %shadow = extractvalue { float, i8 } %shadow_sample, 0
  %out0 = insertelement <4 x float> zeroinitializer, float %shadow, i32 0
  ret <4 x float> %out0
}

declare { float, i8 } @air.sample_compare_depth_2d.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, float, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8, !9}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
!9 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_static_sampler_compare_depth_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp)
        .expect_err("a sampler selected between two distinct states must not be answered");
    assert!(
        error.contains("sampler operand is a pointer"),
        "unexpected refusal: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_fragment_compare_depth_2d_array_appends_layer_before_comparison() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239254151095, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, i32 %layer, float %reference, ptr addrspace(1) %depth) {
entry:
  %sample = tail call { float, i8 } @air.sample_compare_depth_2d_array.f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 1, <2 x float> %coord, i32 %layer, float %reference, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %shadow = extractvalue { float, i8 } %sample, 0
  %out = insertelement <4 x float> zeroinitializer, float %shadow, i32 0
  ret <4 x float> %out
}

declare { float, i8 } @air.sample_compare_depth_2d_array.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i32, float, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7, !9}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.fragment_input", !"generated(layer)", !"air.center", !"air.flat", !"air.arg_type_name", !"uint", !"air.arg_name", !"layer"}
!7 = !{i32 3, !"air.fragment_input", !"generated(reference)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float", !"air.arg_name", !"reference"}
!9 = !{i32 4, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d_array<float, sample>", !"air.arg_name", !"depth"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_compare_depth_2d_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageSampleImplicitLod"), "{asm}");
    assert!(asm.contains("OpConvertUToF"), "{asm}");
    assert!(asm.contains("OpFOrdLessThanEqual"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_fragment_sample_level_follows_the_flag_air_sets_beside_it() {
    let scratch = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601038921, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %lod = extractelement <4 x float> %position, i32 2
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, SLOT, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_sample_level_flag_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let implicit = scratch.replace("SLOT", "i1 false");
    assert_ne!(implicit, scratch, "the template has no level slot");
    let asm = disassemble(
        &crate::translate_sanitized_native(&implicit, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.contains("OpImageSampleImplicitLod"),
        "a plain sample() picks its level from the fragment's derivatives: {asm}"
    );
    assert!(
        !asm.contains("OpImageSampleExplicitLod"),
        "the zero in the level slot is a placeholder, not a level: {asm}"
    );

    let explicit = scratch.replace("SLOT", "i1 true");
    let asm = disassemble(
        &crate::translate_sanitized_native(&explicit, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpImageSampleExplicitLod") && line.contains("Lod")),
        "level(lod) is an explicit level: {asm}"
    );

    let vertex = implicit
        .replace("!air.fragment = !{!0}", "!air.vertex = !{!0}")
        .replace(
            "!2 = !{!\"air.render_target\", i32 0, i32 0, !\"air.arg_type_name\", !\"float4\"}",
            "!2 = !{!\"air.position\", !\"air.arg_type_name\", !\"float4\"}",
        )
        .replace(
            "!5 = !{i32 1, !\"air.fragment_input\", !\"generated(coord)\", !\"air.center\", !\"air.perspective\", !\"air.arg_type_name\", !\"float2\", !\"air.arg_name\", !\"coord\"}",
            "!5 = !{i32 1, !\"air.vertex_input\", !\"air.location_index\", i32 1, !\"air.arg_type_name\", !\"float2\", !\"air.arg_name\", !\"coord\"}",
        )
        .replace(
            "!4 = !{i32 0, !\"air.position\", !\"air.center\", !\"air.arg_type_name\", !\"float4\", !\"air.arg_name\", !\"position\"}",
            "!4 = !{i32 0, !\"air.vertex_input\", !\"air.location_index\", i32 0, !\"air.arg_type_name\", !\"float4\", !\"air.arg_name\", !\"position\"}",
        );
    let asm = disassemble(
        &crate::translate_sanitized_native(&vertex, Stage::Vertex, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        !asm.contains("OpImageSampleImplicitLod"),
        "a vertex invocation has no derivatives: {asm}"
    );
    assert!(
        asm.contains("OpImageSampleExplicitLod"),
        "the vertex sample still has to name a level: {asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_fragment_sample_bias_reaches_the_implicit_level() {
    let scratch = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601038921, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, SLOT, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_sample_bias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let biased = scratch.replace("SLOT", "i1 false, float -2.500000e-01");
    assert_ne!(biased, scratch, "the template has no level slot");
    let asm = disassemble(
        &crate::translate_sanitized_native(&biased, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpImageSampleImplicitLod") && line.contains("Bias")),
        "bias(b) shifts the level the derivatives give, so it rides the implicit sample: {asm}"
    );
    assert!(
        !asm.contains("OpImageSampleExplicitLod"),
        "a bias is not a level: {asm}"
    );

    let plain = scratch.replace("SLOT", "i1 false, float 0.000000e+00");
    let asm = disassemble(
        &crate::translate_sanitized_native(&plain, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.contains("OpImageSampleImplicitLod") && !asm.contains("Bias"),
        "the zero placeholder is not a bias: {asm}"
    );

    let levelled = scratch.replace("SLOT", "i1 true, float -2.500000e-01");
    let asm = disassemble(
        &crate::translate_sanitized_native(&levelled, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpImageSampleExplicitLod") && line.contains("Lod")),
        "the flag says this slot is a level: {asm}"
    );
    assert!(!asm.contains("Bias"), "a level is not a bias: {asm}");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_fragment_sample_level_slot_without_its_flag_is_neither() {
    let scratch = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601038921, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %u = extractelement <2 x float> %coord, i32 0
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_1d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, float %u, i1 false, i32 0, FLAG, float 5.000000e-01, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_1d.v4f32(ptr addrspace(1), ptr addrspace(2), float, i1, i32, FLAGTY, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture1d<float, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_sample_flagless_slot_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let flagged = scratch.replace("FLAGTY", "i1").replace("FLAG", "i1 false");
    let asm = disassemble(
        &crate::translate_sanitized_native(&flagged, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpImageSampleImplicitLod") && line.contains("Bias")),
        "the flag says this slot is a bias: {asm}"
    );

    let flagless = scratch.replace("FLAGTY", "i32").replace("FLAG", "i32 0");
    let asm = disassemble(
        &crate::translate_sanitized_native(&flagless, Stage::Fragment, &tmp).expect("translate"),
    )
    .expect("disassemble");
    assert!(
        asm.contains("OpImageSampleImplicitLod"),
        "a slot with no flag leaves the sample implicit: {asm}"
    );
    assert!(
        !asm.contains("Bias") && !asm.contains("OpImageSampleExplicitLod"),
        "a float with no flag before it is neither a level nor a bias: {asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_fragment_gradient_sample_carries_its_derivatives() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601038921, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %dx = fmul <2 x float> %coord, <float 2.000000e+00, float 2.000000e+00>
  %dy = fmul <2 x float> %coord, <float 4.000000e+00, float 4.000000e+00>
  %sample = tail call { <4 x half>, i8 } @air.sample_texture_2d_grad.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, <2 x float> %dx, <2 x float> %dy, float 0.000000e+00, i1 true, <2 x i32> zeroinitializer, i32 0)
  %half4 = extractvalue { <4 x half>, i8 } %sample, 0
  %color = fpext <4 x half> %half4 to <4 x float>
  ret <4 x float> %color
}

declare { <4 x half>, i8 } @air.sample_texture_2d_grad.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, <2 x float>, <2 x float>, float, i1, <2 x i32>, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_gradient_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv =
        crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("gradient translate");
    let asm = disassemble(&spv).expect("disassemble");
    let sample = asm
        .lines()
        .find(|line| line.contains("OpImageSampleExplicitLod"))
        .unwrap_or_else(|| panic!("a gradient sample names its level: {asm}"));
    assert!(
        sample.contains("Grad"),
        "the derivatives Metal was handed choose the level: {sample}"
    );
    assert!(
        !sample.contains(" Lod "),
        "SPIR-V forbids the Lod image operand beside Grad: {sample}"
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
fn native_fragment_selected_texture2d_array_sample_keeps_array_shape() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601036873, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex_a, ptr addrspace(1) %tex_b) {
entry:
  %x = extractelement <4 x float> %position, i32 0
  %cond = fcmp ogt float %x, 0.000000e+00
  %selected = select i1 %cond, ptr addrspace(1) %tex_a, ptr addrspace(1) %tex_b
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d_array.v4f32(ptr addrspace(1) %selected, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i32 1, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_2d_array.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i32, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<float, sample>", !"air.arg_name", !"tex_a"}
!7 = !{i32 3, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<float, sample>", !"air.arg_name", !"tex_b"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_selected_texture2d_array_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeImage") && line.contains(" 2D 0 1 0 1 ")),
        "{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeVector") && line.ends_with(" 3")),
        "{asm}"
    );
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpSampledImage"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_function_constant_texture_with_binding_keeps_array_shape() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@predicate = internal addrspace(2) global i8 0, align 1
@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020489, i64 0], align 8

define <4 x float> @frag(ptr addrspace(1) %tex) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_1d_array.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, float 2.500000e-01, i32 2, i1 false, i32 0, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}

declare { <4 x float>, i8 } @air.sample_texture_1d_array.v4f32(ptr addrspace(1), ptr addrspace(2), float, i32, i1, i32, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4}
!4 = !{i32 0, !"air.function_constant", !5, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture1d_array<float, sample>", !"air.arg_name", !"tex"}
!5 = !{ptr addrspace(2) @predicate, !"bool", !"supportsTexture"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_function_constant_texture_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeImage") && line.contains(" 1D 0 1 0 1 ")),
        "{asm}"
    );
    assert!(asm.contains("Binding 1"), "{asm}");
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
fn native_fragment_calculate_unclamped_lod_uses_image_query_lod() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601036873, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %lod = tail call fast float @air.calculate_unclamped_lod_texture_2d(ptr addrspace(1) readonly captures(none) %tex, ptr addrspace(2) readonly captures(none) @__air_sampler_state, <2 x float> %coord, i32 0)
  %out0 = insertelement <4 x float> zeroinitializer, float %lod, i32 0
  ret <4 x float> %out0
}

declare float @air.calculate_unclamped_lod_texture_2d(ptr addrspace(1) readonly captures(none), ptr addrspace(2) readonly captures(none), <2 x float>, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_query_lod_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    assert!(asm.contains("OpSampledImage"), "{asm}");
    assert!(asm.contains("OpImageQueryLod"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpCompositeExtract") && line.ends_with(" 1")),
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
fn native_fragment_calculate_clamped_lod_uses_selected_mipmap_level() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601036873, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %lod = tail call fast float @air.calculate_clamped_lod_texture_2d(ptr addrspace(1) readonly captures(none) %tex, ptr addrspace(2) readonly captures(none) @__air_sampler_state, <2 x float> %coord, i32 0)
  %out0 = insertelement <4 x float> zeroinitializer, float %lod, i32 0
  ret <4 x float> %out0
}

declare float @air.calculate_clamped_lod_texture_2d(ptr addrspace(1) readonly captures(none), ptr addrspace(2) readonly captures(none), <2 x float>, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_query_clamped_lod_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    assert!(asm.contains("OpImageQueryLod"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpCompositeExtract") && line.ends_with(" 0")),
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
fn native_fragment_calculate_lod_replays_selected_texture_through_helper() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601036873, i64 0], align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %left, ptr addrspace(1) %right) {
entry:
  %x = extractelement <4 x float> %position, i32 0
  %condition = fcmp olt float %x, 0.000000e+00
  %texture = select i1 %condition, ptr addrspace(1) %left, ptr addrspace(1) %right
  %lod = call fastcc float @query_lod(ptr addrspace(1) %texture, <2 x float> %coord)
  %out0 = insertelement <4 x float> zeroinitializer, float %lod, i32 0
  ret <4 x float> %out0
}

define internal fastcc float @query_lod(ptr addrspace(1) %texture, <2 x float> %coord) {
entry:
  %lod = call float @air.calculate_clamped_lod_texture_2d(ptr addrspace(1) %texture, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i32 0)
  ret float %lod
}

declare float @air.calculate_clamped_lod_texture_2d(ptr addrspace(1), ptr addrspace(2), <2 x float>, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"left"}
!7 = !{i32 3, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"right"}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_selected_query_lod_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQueryLod"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_fragment_raster_sample_count_requires_pipeline_contract() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define <4 x float> @frag(<4 x float> %position) {
entry:
  %samples = tail call i32 @air.get_num_samples.i32(i32 0)
  %bits = bitcast i32 %samples to float
  %out0 = insertelement <4 x float> zeroinitializer, float %bits, i32 0
  ret <4 x float> %out0
}

declare i32 @air.get_num_samples.i32(i32)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_raster_samples_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp)
        .expect_err("unknown pipeline state must remain unsupported");
    assert!(error.contains("raster_sample_count"), "{error}");

    let options = crate::passes::TransformOptions {
        raster_sample_count: Some(4),
        ..crate::passes::TransformOptions::default()
    };
    let spv = crate::translate_sanitized_native_with_options(ll, Stage::Fragment, &tmp, options)
        .expect("translate with pipeline contract");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("air.get_num_samples.i32"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.ends_with(" 4")),
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
fn native_depth_cube_num_mip_levels_uses_image_query_levels() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out) {
entry:
  %levels = tail call i32 @air.get_num_mip_levels_depth_cube(ptr addrspace(1) readonly captures(none) %depth)
  store i32 %levels, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_mip_levels_depth_cube(ptr addrspace(1) readonly captures(none))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depthcube<float, sample>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_depth_cube_mip_levels_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    assert!(asm.contains("OpImageQueryLevels"), "{asm}");
    assert!(!asm.contains("OpBitcast"), "{asm}");
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
fn native_gather_texture_accepts_null_offset_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_null_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
    for line in asm.lines().filter(|line| line.contains("OpImageGather")) {
        assert!(!line.contains("ConstOffset"), "{line}");
    }
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
fn native_gather_texture_uint_result_uses_integer_image_type() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x i32>, i8 } %gather, 0
  store <4 x i32> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"int4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_uint_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
    assert!(asm.contains("OpTypeImage"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_texture_from_byval_array_dynamic_field_uses_descriptor_array() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

%"struct.metal::texture2d" = type { ptr addrspace(1) }

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(i32 %idx, ptr readonly byval([2 x %"struct.metal::texture2d"]) %textures, ptr addrspace(1) %out) {
entry:
  %base = bitcast ptr %textures to ptr
  %wide = zext i32 %idx to i64
  %slot = getelementptr inbounds [2 x %"struct.metal::texture2d"], ptr %base, i64 0, i64 %wide, i32 0
  %tex = load ptr addrspace(1), ptr %slot, align 8
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x i32>, i8 } %gather, 0
  tail call void @air.write_texture_2d.u.v4i32(ptr addrspace(1) %out, <2 x i32> zeroinitializer, <4 x i32> %color, i32 0, i32 2)
  ret void
}

declare { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)
declare void @air.write_texture_2d.u.v4i32(ptr addrspace(1), <2 x i32>, <4 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"idx"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 2, !"air.sample", !"air.arg_type_name", !"array<texture2d<uint, sample>, 2>", !"air.arg_name", !"textures"}
!5 = !{i32 2, !"air.texture", !"air.location_index", i32 4, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<uint, write>", !"air.arg_name", !"out"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_byval_texture_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    // element i reads the texture bound at index i through the per-draw table and the bindless heap
    assert!(asm.contains("OpTypeArray") || asm.contains("OpTypeRuntimeArray"), "{asm}");
    assert!(
        asm.contains("OpAccessChain"),
        "expected indexed descriptor-array access:\n{asm}"
    );
    assert!(asm.contains("OpImageGather"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
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
fn native_gather_texture_uint_pixel_sampler_extracts_integer_components() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x i32>, i8 } %gather, 0
  store <4 x i32> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x i32>, i8 } @air.gather_texture_2d.u.v4i32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"int4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_uint_pixel_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    for line in asm
        .lines()
        .filter(|line| line.contains("OpCompositeExtract"))
    {
        assert!(!line.contains("%float"), "{line}\n{asm}");
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
fn native_gather_texture_half_result_converts_struct_member() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_half_result_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
    assert!(asm.contains("OpFConvert"), "{asm}");
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
fn native_gather_texture_2d_array_uses_layer_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d_array.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i32 1, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d_array.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i32, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<half, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_2d_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::ImageGather),
        "{asm}"
    );
    assert!(
        module.types_global_values.iter().any(|inst| {
            inst.class.opcode == Op::TypeImage
                && inst.operands.get(1) == Some(&Operand::Dim(spirv::Dim::Dim2D))
                && inst.operands.get(3) == Some(&Operand::LiteralBit32(1))
        }),
        "{asm}"
    );
    assert!(
        module
            .functions
            .iter()
            .flat_map(|func| &func.blocks)
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.class.opcode == Op::CompositeConstruct
                && inst
                    .result_type
                    .is_some_and(|ty| is_float_vector(&module, ty, 3))),
        "{asm}"
    );
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
fn native_gather_texture_accepts_splat_offset_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> splat (i32 -7), i32 0, i32 0)
  %color = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_splat_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
    for line in asm.lines().filter(|line| line.contains("OpImageGather")) {
        assert!(line.contains("ConstOffset"), "{line}\n\n{asm}");
    }
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
fn native_gather_texture_accepts_literal_offset_vector() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> <i32 -7, i32 -5>, i32 0, i32 0)
  %color = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_literal_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
    for line in asm.lines().filter(|line| line.contains("OpImageGather")) {
        assert!(line.contains("ConstOffset"), "{line}\n\n{asm}");
    }
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
fn native_gather_texture_dynamic_offset_adjusts_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out, i32 %lane) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %x = or i32 %lane, -8
  %o0 = insertelement <2 x i32> undef, i32 %x, i32 0
  %offset = insertelement <2 x i32> %o0, i32 -5, i32 1
  %gather = tail call { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> %offset, i32 0, i32 0)
  %color = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"int", !"air.arg_name", !"lane"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_dynamic_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let gather = asm
        .lines()
        .find(|line| line.contains("OpImageGather"))
        .expect("find OpImageGather");
    assert!(!gather.contains("ConstOffset"), "{gather}\n\n{asm}");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpFDiv"), "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_texture_pixel_sampler_lowers_to_fetches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253725111, align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 4.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 6.000000e+00, i32 1
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> <i32 -1, i32 3>, i32 0, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_pixel_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpFSub"), "{asm}");
    assert!(asm.contains("0.5"), "{asm}");
    assert!(asm.contains("OpFConvert"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_texture_pixel_center_sampler_skips_high_bit_bias() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253725111, align 8

define void @k(<2 x i16> %index, ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %base = shl <2 x i16> %index, splat (i16 1)
  %basef = tail call fast <2 x float> @air.convert.f.v2f32.u.v2i16(<2 x i16> %base)
  %coord = fadd fast <2 x float> %basef, splat (float 5.000000e-01)
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 2, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)
declare <2 x float> @air.convert.f.v2f32.u.v2i16(<2 x i16>)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"index"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_pixel_center_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
    assert!(!asm.contains(" -1"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_texture_array_pixel_sampler_integer_coord_uses_half_texel_footprint() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(<2 x i32> %index, ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %base = shl <2 x i32> %index, splat (i32 1)
  %coord = tail call fast <2 x float> @air.convert.f.v2f32.u.v2i32(<2 x i32> %base)
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)
declare <2 x float> @air.convert.f.v2f32.u.v2i32(<2 x i32>)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"index"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_array_pixel_sampler_integer_coord_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpFSub"), "{asm}");
    assert!(asm.contains("0.5"), "{asm}");
    assert!(asm.contains("OpFConvert"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_write_texture_pixel_sampler_uses_storage_reads() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253725111, align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 4.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 6.000000e+00, i32 1
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> <i32 -1, i32 3>, i32 0, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_write_texture_pixel_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageRead").count(), 4, "{asm}");
    assert!(asm.contains("OpImageQuerySize "), "{asm}");
    assert!(!asm.contains("OpImageFetch"), "{asm}");
    assert!(!asm.contains("OpImageQuerySizeLod"), "{asm}");
    let formatless = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(
                0,
                runtime_storage_image_state(
                    crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
                    true,
                    false,
                ),
            )
            .unwrap(),
    )
    .expect("pixel gather storage reads use the formatless read contract");
    let formatless_asm = disassemble(&formatless).expect("disassemble formatless gather");
    assert!(
        formatless_asm.contains("OpCapability StorageImageReadWithoutFormat"),
        "{formatless_asm}"
    );
    assert!(!formatless_asm.contains("StorageImageWriteWithoutFormat"));
    tools::spirv_val_bytes(&formatless, &tmp).expect("formatless pixel gather spirv-val");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_texture_array_pixel_sampler_lowers_to_fetches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 2, i32 0)
  %color = extractvalue { <4 x half>, i8 } %gather, 0
  store <4 x half> %color, ptr addrspace(1) %out, align 8
  ret void
}

declare { <4 x half>, i8 } @air.gather_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"half4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_array_pixel_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_gather_depth_lowers_to_component_zero_gather() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601020416, i64 0], align 8

define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out) {
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call { <4 x float>, i8 } @air.gather_depth_2d.v4f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 1, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i32 0)
  %values = extractvalue { <4 x float>, i8 } %gather, 0
  store <4 x float> %values, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.gather_depth_2d.v4f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i1, <2 x i32>, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d<float, sample>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_gather_depth_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageGather"), "{asm}");
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
fn native_gather_depth_2d_array_normalized_sampler_gathers() {
    let spv = gather_depth_2d_array_spv(34901797601020416, "normalized");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    assert!(asm.contains("OpImageGather"), "{asm}");
    assert!(asm.contains("OpConvertUToF"), "{asm}");
    assert_eq!(image_fetch_count(&module), 0, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
}

#[test]
fn native_gather_depth_2d_array_pixel_sampler_reconstructs_the_footprint() {
    let spv = gather_depth_2d_array_spv(559_689, "pixel");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    assert_eq!(image_fetch_count(&module), 4, "{asm}");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains(" Floor "), "{asm}");
    assert!(!asm.contains("OpImageGather"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
}

fn image_fetch_count(module: &crate::spirv_module::Module) -> usize {
    module
        .functions
        .iter()
        .flat_map(|func| &func.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::ImageFetch)
        .count()
}

fn gather_depth_2d_array_spv(sampler_word: i64, label: &str) -> Vec<u8> {
    let ll = format!(
        r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 {sampler_word}, i64 0], align 8

define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out) {{
entry:
  %c0 = insertelement <2 x float> poison, float 5.000000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 5.000000e-01, i32 1
  %gather = tail call {{ <4 x float>, i8 }} @air.gather_depth_2d_array.v4f32(ptr addrspace(1) %depth, ptr addrspace(2) @__air_sampler_state, i32 1, <2 x float> %coord, i32 2, i1 true, <2 x i32> zeroinitializer, i32 0)
  %values = extractvalue {{ <4 x float>, i8 }} %gather, 0
  store <4 x float> %values, ptr addrspace(1) %out, align 16
  ret void
}}

declare {{ <4 x float>, i8 }} @air.gather_depth_2d_array.v4f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x float>, i32, i1, <2 x i32>, i32)

!air.kernel = !{{!0}}
!air.sampler_states = !{{!5}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3, !4}}
!3 = !{{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"depth2d_array<float, sample>", !"air.arg_name", !"depth"}}
!4 = !{{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}}
!5 = !{{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}}
"#
    );
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_gather_depth_2d_array_{label}_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(&ll, Stage::Kernel, &tmp).expect("translate");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
    spv
}

#[test]
fn native_kernel_unused_texture1d_uses_metadata_dimension() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %lut, ptr addrspace(1) %out) {
entry:
  store i32 1, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture1d<float, sample>", !"air.arg_name", !"lut", !"air.arg_unused"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_unused_texture1d_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeImage"), "{asm}");
    assert!(asm.contains(" 1D "), "{asm}");
    assert!(asm.contains("Binding 32"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_uint_write_texture_uses_uint_storage_image() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %tid) {
entry:
  %v0 = insertelement <4 x i32> undef, i32 1, i64 0
  %v1 = insertelement <4 x i32> %v0, i32 2, i64 1
  %v2 = insertelement <4 x i32> %v1, i32 3, i64 2
  %v3 = insertelement <4 x i32> %v2, i32 4, i64 3
  tail call void @air.write_texture_2d.u.v4i32(ptr addrspace(1) %dst, <2 x i32> %tid, <4 x i32> %v3, i32 0, i32 2)
  ret void
}

declare void @air.write_texture_2d.u.v4i32(ptr addrspace(1), <2 x i32>, <4 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 7, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<uint, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_uint_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba8ui"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("Binding 487"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn runtime_storage_image_specialization_splits_shared_image_pointer_and_load_types() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_storage_split_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let rgba8 = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Rgba8Unorm,
        false,
        false,
    );
    let rgba32 = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Rgba32Float,
        false,
        false,
    );
    let options = passes::TransformOptions::default()
        .with_runtime_storage_image(0, rgba8)
        .unwrap()
        .with_runtime_storage_image(1, rgba32)
        .unwrap();
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        &tmp,
        options,
    )
    .expect("runtime storage-image specialization");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba8"), "{asm}");
    assert!(asm.contains("Rgba32f"), "{asm}");
    assert_eq!(asm.matches("OpImageWrite").count(), 2, "{asm}");

    let module = load_bytes(&spv).expect("load specialized module");
    let defs = module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction)))
        .collect::<HashMap<_, _>>();
    let image_type_at_binding = |binding: u32| {
        let variable = module
            .annotations
            .iter()
            .find_map(|decoration| {
                (decoration.class.opcode == Op::Decorate
                    && decoration.operands.get(1)
                        == Some(&Operand::Decoration(Decoration::Binding))
                    && decoration.operands.get(2) == Some(&Operand::LiteralBit32(binding)))
                .then(|| match decoration.operands.first() {
                    Some(Operand::IdRef(id)) => Some(*id),
                    _ => None,
                })
                .flatten()
            })
            .expect("storage-image variable");
        let variable_type = defs[&variable].result_type.expect("variable pointer type");
        let image_type = match defs[&variable_type].operands.get(1) {
            Some(Operand::IdRef(image_type)) => *image_type,
            other => panic!("storage-image pointer has no image pointee: {other:?}"),
        };
        let load = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .find(|instruction| {
                instruction.class.opcode == Op::Load
                    && instruction.operands.first() == Some(&Operand::IdRef(variable))
            })
            .expect("storage-image load");
        assert_eq!(load.result_type, Some(image_type));
        (image_type, defs[&image_type].operands.get(6).cloned())
    };
    let (rgba8_type, rgba8_format) = image_type_at_binding(480);
    let (rgba32_type, rgba32_format) = image_type_at_binding(481);
    assert_ne!(rgba8_type, rgba32_type, "differently specialized bindings");
    assert_eq!(
        rgba8_format,
        Some(Operand::ImageFormat(spirv::ImageFormat::Rgba8))
    );
    assert_eq!(
        rgba32_format,
        Some(Operand::ImageFormat(spirv::ImageFormat::Rgba32f))
    );

    for (index, state, format) in [
        (0, rgba8, crate::meta::TextureFormat::Rgba8),
        (1, rgba32, crate::meta::TextureFormat::Rgba32f),
    ] {
        let binding = reflection
            .binding_at(crate::reflect::ResourceKind::StorageImage, index)
            .expect("reflected storage image");
        assert_eq!(
            binding.texture_shape.and_then(|shape| shape.storage_format),
            Some(format)
        );
        assert!(reflection.runtime_storage_image_specializations.contains(
            &crate::reflect::RuntimeStorageImageSpecialization {
                metal_index: index,
                state,
                spirv_format: Some(format),
            }
        ));
    }
    tools::spirv_val_bytes(&spv, &tmp).expect("specialized spirv-val");
}

#[test]
fn runtime_storage_image_formatless_capabilities_follow_actual_access() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_storage_formatless_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let bgra_write = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        false,
        true,
    );
    let (write_spv, write_reflection) = crate::translate_sanitized_native_reflected(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, bgra_write)
            .unwrap(),
    )
    .expect("formatless write specialization");
    let write_asm = disassemble(&write_spv).expect("disassemble write");
    assert!(write_asm.contains("OpCapability StorageImageWriteWithoutFormat"));
    assert!(!write_asm.contains("OpCapability StorageImageReadWithoutFormat"));
    assert!(write_asm.contains("Unknown"), "{write_asm}");
    assert_eq!(
        write_reflection.runtime_storage_image_specializations,
        [crate::reflect::RuntimeStorageImageSpecialization {
            metal_index: 0,
            state: bgra_write,
            spirv_format: None,
        }]
    );
    assert_eq!(
        write_reflection
            .binding_at(crate::reflect::ResourceKind::StorageImage, 0)
            .and_then(|binding| binding.texture_shape)
            .and_then(|shape| shape.storage_format),
        None
    );
    tools::spirv_val_bytes(&write_spv, &tmp).expect("formatless write spirv-val");

    let bgra_read = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        true,
        false,
    );
    let read_spv = crate::translate_sanitized_native_with_options(
        RUNTIME_STORAGE_IMAGE_READ_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, bgra_read)
            .unwrap(),
    )
    .expect("formatless read specialization");
    let read_asm = disassemble(&read_spv).expect("disassemble read");
    assert!(read_asm.contains("OpCapability StorageImageReadWithoutFormat"));
    assert!(!read_asm.contains("OpCapability StorageImageWriteWithoutFormat"));
    assert!(read_asm.contains("OpImageRead"), "{read_asm}");
    tools::spirv_val_bytes(&read_spv, &tmp).expect("formatless read spirv-val");
}

#[test]
fn runtime_storage_image_formatless_partial_write_requires_read_and_write_features() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %tid) {
entry:
  %texel = insertelement <4 x float> undef, float 1.000000e+00, i64 0
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %dst, <2 x i32> %tid, <4 x float> %texel, i32 0, i32 2)
  ret void
}
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_storage_formatless_rmw_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let both = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        true,
        true,
    );
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, both)
            .unwrap(),
    )
    .expect("formatless read-modify-write specialization");
    let asm = disassemble(&spv).expect("disassemble read-modify-write");
    assert!(asm.contains("OpCapability StorageImageReadWithoutFormat"));
    assert!(asm.contains("OpCapability StorageImageWriteWithoutFormat"));
    assert!(asm.contains("OpImageRead"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    tools::spirv_val_bytes(&spv, &tmp).expect("formatless read-modify-write spirv-val");

    let write_only = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        false,
        true,
    );
    let error = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, write_only)
            .unwrap(),
    )
    .expect_err("hidden preservation read requires read-without-format");
    assert!(error.contains("read-without-format"), "{error}");
}

#[test]
fn runtime_storage_image_specialization_refuses_incompatible_or_missing_contracts() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_runtime_storage_refusals_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let uint_state = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Rgba8Uint,
        false,
        false,
    );
    let incompatible = crate::translate_sanitized_native_with_options(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, uint_state)
            .unwrap(),
    )
    .expect_err("float AIR texels cannot target an integer runtime format");
    assert!(
        incompatible.contains("AIR texels are Float"),
        "{incompatible}"
    );
    let reflected_incompatible = crate::reflect_sanitized(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, uint_state)
            .unwrap(),
    )
    .expect_err("metadata-only reflection must enforce the executable component contract");
    assert!(
        reflected_incompatible.contains("AIR texels are Float"),
        "{reflected_incompatible}"
    );

    let bgra_missing_write = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
        true,
        false,
    );
    let missing_feature = crate::translate_sanitized_native_with_options(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(0, bgra_missing_write)
            .unwrap(),
    )
    .expect_err("formatless write without host feature");
    assert!(
        missing_feature.contains("write-without-format"),
        "{missing_feature}"
    );

    let rgba8 = runtime_storage_image_state(
        crate::reflect::RuntimeStorageImageFormat::Rgba8Unorm,
        false,
        false,
    );
    let missing_binding = crate::translate_sanitized_native_with_options(
        RUNTIME_STORAGE_IMAGE_WRITE_PAIR_LL,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions::default()
            .with_runtime_storage_image(7, rgba8)
            .unwrap(),
    )
    .expect_err("specialization index must name a storage binding");
    assert!(
        missing_binding.contains("no storage-image binding"),
        "{missing_binding}"
    );

    let unsupported = crate::reflect::RuntimeStorageImageState {
        format: crate::reflect::RuntimeStorageImageFormat::Rgba8Unorm,
        capabilities: crate::reflect::RuntimeStorageImageCapabilities::default(),
    };
    let missing_storage = passes::TransformOptions::default()
        .with_runtime_storage_image(0, unsupported)
        .expect_err("format must support storage usage");
    assert!(missing_storage.contains("lacks storage-image format support"));
}

#[test]
fn native_kernel_helper_wrapped_write_texture_uses_single_storage_image() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Out = type <{ %Tex, <2 x i16> }>
%Tex = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %dst) {
entry:
  %out = alloca %Out, align 8
  %tex_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  store ptr addrspace(1) %dst, ptr %tex_field, align 8
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1
  store <2 x i16> zeroinitializer, ptr %coord_field, align 8
  call void @helper(ptr %out)
  ret void
}

define internal void @helper(ptr %out) {
entry:
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1
  %coord = load <2 x i16>, ptr %coord_field, align 8
  %tex_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr %tex_field, align 8
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %tex, <2 x i16> %coord, <4 x half> zeroinitializer, i16 0, i32 2)
  ret void
}

declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
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
fn native_kernel_multisample_sample_count_query_uses_image_query_samples() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_texture_2d_ms(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_texture_2d_ms(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d_ms<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_ms_sample_count_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySamples"), "{asm}");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_multisample_depth_sample_count_query_uses_image_query_samples() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d_ms<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_depth_ms_sample_count_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySamples"), "{asm}");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_single_sample_texture_sample_count_query_is_constant_one() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_texture_2d(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_texture_2d(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_single_sample_count_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpImageQuerySamples"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_multisample_array_sample_count_query_uses_image_query_samples() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_texture_2d_ms_array(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_texture_2d_ms_array(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d_ms_array<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_ms_array_sample_count_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySamples"), "{asm}");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_multisample_depth_array_sample_count_query_uses_image_query_samples() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_depth_2d_ms_array(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_depth_2d_ms_array(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d_ms_array<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_depth_ms_array_sample_count_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySamples"), "{asm}");
    assert!(asm.contains("OpCapability ImageQuery"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_multisample_sample_count_query_bounds_the_per_sample_loop() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_texture_2d_ms(ptr addrspace(1) %tex)
  br label %loop

loop:
  %s = phi i32 [ 0, %entry ], [ %s.next, %body ]
  %acc = phi i32 [ 0, %entry ], [ %acc.next, %body ]
  %more = icmp slt i32 %s, %n
  br i1 %more, label %body, label %done

body:
  %acc.next = add i32 %acc, %s
  %s.next = add i32 %s, 1
  br label %loop

done:
  store i32 %acc, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_texture_2d_ms(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d_ms<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_ms_sample_loop_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySamples"), "{asm}");

    let mut derived = Vec::new();
    for line in asm.lines() {
        let Some((result, rhs)) = line.split_once(" = ") else {
            continue;
        };
        let result = result.trim();
        if rhs.contains("OpImageQuerySamples") {
            derived.push(result.to_string());
            continue;
        }
        let forwards = ["OpCopyObject", "OpBitcast", "OpUConvert", "OpSConvert"]
            .iter()
            .any(|op| rhs.starts_with(op));
        if forwards && derived.iter().any(|id| rhs.contains(id.as_str())) {
            derived.push(result.to_string());
        }
    }
    assert!(!derived.is_empty(), "no OpImageQuerySamples result: {asm}");

    let bounds_the_loop = asm.lines().any(|line| {
        ["OpSLessThan", "OpULessThan", "OpINotEqual", "OpIEqual"]
            .iter()
            .any(|op| line.contains(op))
            && derived.iter().any(|id| line.contains(id.as_str()))
    });
    assert!(
        bounds_the_loop,
        "the queried sample count must bound the per-sample loop; a constant bound here is the \
         defect that made a four-sample kernel visit only sample zero: {asm}"
    );
    assert!(asm.contains("OpLoopMerge"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_helper_wrapped_texture_query_resolves_loaded_image() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Out = type { %Tex }
%Tex = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %out) {
entry:
  %wrap = alloca %Out, align 8
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  store ptr addrspace(1) %src, ptr %tex_field, align 8
  call void @helper(ptr %wrap, ptr addrspace(1) %out)
  ret void
}

define internal void @helper(ptr %wrap, ptr addrspace(1) %out) {
entry:
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr %tex_field, align 8
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_texture_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
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
fn native_kernel_helper_wrapped_write_texture_query_resolves_loaded_image() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Out = type { %Tex }
%Tex = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %dst, ptr addrspace(1) %out) {
entry:
  %wrap = alloca %Out, align 8
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  store ptr addrspace(1) %dst, ptr %tex_field, align 8
  call void @helper(ptr %wrap, ptr addrspace(1) %out)
  ret void
}

define internal void @helper(ptr %wrap, ptr addrspace(1) %out) {
entry:
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr %tex_field, align 8
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_write_texture_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySize "), "{asm}");
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
fn native_kernel_helper_wrapped_multi_texture_query_resolves_field_source() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Out = type { %Tex }
%Tex = type { ptr addrspace(1) }

define void @k(ptr addrspace(1) %src0, ptr addrspace(1) %src1, ptr addrspace(1) %dst, ptr addrspace(1) %out) {
entry:
  %wrap = alloca %Out, align 8
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  store ptr addrspace(1) %src0, ptr %tex_field, align 8
  call void @helper(ptr %wrap, ptr addrspace(1) %out)
  ret void
}

define internal void @helper(ptr %wrap, ptr addrspace(1) %out) {
entry:
  %tex_field = getelementptr inbounds %Out, ptr %wrap, i64 0, i32 0, i32 0
  %tex = load ptr addrspace(1), ptr %tex_field, align 8
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src0"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src1"}
!5 = !{i32 2, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_multi_texture_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
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
fn native_kernel_selected_texture_query_clones_queries_before_select() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %src0, ptr addrspace(1) %src1, ptr addrspace(1) %out, i32 %tid) {
entry:
  %cond = icmp eq i32 %tid, 0
  %tex = select i1 %cond, ptr addrspace(1) %src0, ptr addrspace(1) %src1
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  %h = tail call i32 @air.get_height_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  %out1 = getelementptr inbounds i32, ptr addrspace(1) %out, i64 1
  store i32 %h, ptr addrspace(1) %out1, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
declare i32 @air.get_height_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src0"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src1"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint2", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_selected_texture_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let mut module = load_bytes(&spv).expect("load native spv");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::TypePointer)
                .then_some(inst.result_id)
                .flatten()
        })
        .collect::<HashSet<_>>();
    let pointer_select = module.all_inst_iter().find(|inst| {
        inst.class.opcode == Op::Select
            && inst
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
    });
    assert!(pointer_select.is_none(), "{pointer_select:?}\n{asm}");
    assert_eq!(asm.matches("OpImageQuerySizeLod").count(), 4, "{asm}");
    assert!(
        !crate::native::construct_opaque_image_selects_module(&mut module),
        "final resource construction must leave no portable opaque-image closure"
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
fn native_kernel_nested_texture_phi_query_constructs_integer_tag_tree() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, ptr addrspace(1) %out, i32 %tid) {
entry:
  %outer_cond = icmp eq i32 %tid, 0
  br i1 %outer_cond, label %outer_arm, label %inner_head

inner_head:
  %inner_cond = icmp eq i32 %tid, 1
  br i1 %inner_cond, label %inner_left, label %inner_right

inner_left:
  br label %inner_merge

inner_right:
  br label %inner_merge

inner_merge:
  %inner = phi ptr addrspace(1) [ %a, %inner_left ], [ %b, %inner_right ]
  br label %outer_merge

outer_arm:
  br label %outer_merge

outer_merge:
  %texture = phi ptr addrspace(1) [ %c, %outer_arm ], [ %inner, %inner_merge ]
  %width = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %texture, i32 0)
  store i32 %width, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"c"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!7 = !{i32 4, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_nested_texture_phi_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageQuerySizeLod").count(), 3, "{asm}");
    assert!(asm.matches("OpPhi").count() >= 2, "{asm}");
    let mut module = load_bytes(&spv).expect("load native spv");
    assert!(
        !crate::native::construct_opaque_image_selects_module(&mut module),
        "final resource construction must leave no nested opaque-image phi closure"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_shared_imageblock_without_a_stride_parameter_uses_the_workgroup_size() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %tid) {
entry:
  %mine = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> <half 0xH3C00, half 0xH4000, half 0xH4200, half 0xH4400>, ptr addrspace(4) %mine, align 8
  tail call void @air.wg.barrier(i32 2, i32 1)
  %origin = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  %v = load <4 x half>, ptr addrspace(4) %origin, align 8
  store <4 x half> %v, ptr addrspace(1) %dst, align 8
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half4*", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_imageblock_stride_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");

    let cell_array = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::Variable
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(StorageClass::Workgroup))
                )
        })
        .and_then(|inst| inst.result_id)
        .unwrap_or_else(|| panic!("shared imageblock cells must be Workgroup:\n{asm}"));

    let workgroup_size = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::SpecConstantComposite
                && module.annotations.iter().any(|annotation| {
                    annotation.class.opcode == Op::Decorate
                        && annotation.operands.first()
                            == Some(&Operand::IdRef(inst.result_id.unwrap()))
                        && annotation.operands.iter().any(|operand| {
                            matches!(operand, Operand::BuiltIn(spirv::BuiltIn::WorkgroupSize))
                        })
                })
        })
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::IdRef(x)) => Some(*x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no WorkgroupSize spec constant:\n{asm}"));

    let copies = module
        .all_inst_iter()
        .filter(|inst| {
            inst.class.opcode == Op::CopyObject
                && inst.operands.first() == Some(&Operand::IdRef(workgroup_size))
        })
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let strides = module
        .all_inst_iter()
        .filter(|inst| inst.class.opcode == Op::IMul)
        .filter(|inst| {
            inst.operands.iter().any(|operand| match operand {
                Operand::IdRef(id) => *id == workgroup_size || copies.contains(id),
                _ => false,
            })
        })
        .count();
    assert!(
        strides >= 2,
        "both tile coordinates must linearize on the workgroup width, got {strides}:\n{asm}"
    );
    assert!(
        module.all_inst_iter().any(|inst| {
            matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain)
                && inst.operands.first() == Some(&Operand::IdRef(cell_array))
        }),
        "the linearized index must address the shared cell array:\n{asm}"
    );
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_aliased_onto_the_implicit_one_reaches_the_render_target() {
    let scratch = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  %v = load <4 x half>, ptr addrspace(4) %ptr, align 8
  store <4 x half> %v, ptr addrspace(1) %dst, align 8
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, ALIAS!"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half4*", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_alias_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let plane_traffic = |source: &str| {
        let spv = crate::translate_sanitized_native(source, Stage::Kernel, &tmp)
            .expect("an imageblock kernel translates");
        let module = load_bytes(&spv).expect("load spv");
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
        let positions = |opcode| {
            module
                .all_inst_iter()
                .enumerate()
                .filter(|(_, inst)| inst.class.opcode == opcode)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        let reads = positions(Op::ImageRead);
        let writes = positions(Op::ImageWrite);
        let bracketing = match (reads.first(), writes.first()) {
            (Some(read), Some(write)) => positions(Op::ControlBarrier)
                .into_iter()
                .filter(|barrier| barrier > read && barrier < write)
                .count(),
            _ => 0,
        };
        (reads.len(), writes.len(), bracketing)
    };

    let plain = scratch.replace("ALIAS", "");
    assert_ne!(plain, scratch, "the template has no ALIAS splice point");
    assert_eq!(
        plane_traffic(&plain),
        (0, 0, 0),
        "tile-local scratch reaches no render-target plane"
    );

    let aliased = scratch.replace("ALIAS", "!\"air.alias_implicit_imageblock\", ");
    assert_eq!(
        plane_traffic(&aliased),
        (1, 1, 2),
        "the one aliased member must be filled from its plane on entry and written back on \
         return, each behind a threadgroup barrier"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_aliased_member_without_a_plane_format_is_refused() {
    let source = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  %v = load <4 x half>, ptr addrspace(4) %ptr, align 8
  store <4 x half> %v, ptr addrspace(1) %dst, align 8
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !4, !"air.alias_implicit_imageblock", !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v", i32 8, i32 12, i32 0, !"float3", !"w"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"half4*", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_alias_gap_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native(source, Stage::Kernel, &tmp)
        .expect_err("a member with no plane format has no attachment to alias");
    assert!(
        error.contains("parameter 0") && error.contains("implicit imageblock"),
        "the refusal must name the parameter and what its storage aliases: {error}"
    );
    assert!(
        error.contains("member 1"),
        "the refusal must name the member that has no plane: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_half_cells_write_to_a_float_texel_name() {
    // MPS Lanczos stores half4 cells and writes them with the `.v4f32` slice write; Metal
    // converts the cell to the pixel, so this must widen the cell, not refuse the view.
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i32> %gid, <2 x i16> %tid) {
entry:
  %width = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %dst, i32 0)
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %ptr, align 8
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f32(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> zeroinitializer, <2 x i32> %gid, i32 0, i1 false, i32 2)
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i32>, i32, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_half_to_float_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpFConvert"), "the half cell must be widened: {asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_slice_write_uses_private_scratch_and_storage_image() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i32> %gid, <2 x i16> %tid) {
entry:
  %width = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %dst, i32 0)
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %ptr, align 8
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> zeroinitializer, <2 x i32> %gid, i32 0, i1 false, i32 2)
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i32>, i32, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_slice_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    let missing_formatless = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        one_cell_tile_options()
            .with_runtime_storage_image(
                1,
                runtime_storage_image_state(
                    crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
                    false,
                    false,
                ),
            )
            .unwrap(),
    )
    .expect_err("imageblock slice write must require formatless write support");
    assert!(
        missing_formatless.contains("write-without-format"),
        "{missing_formatless}"
    );
    let formatless = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        one_cell_tile_options()
            .with_runtime_storage_image(
                1,
                runtime_storage_image_state(
                    crate::reflect::RuntimeStorageImageFormat::Bgra8Unorm,
                    false,
                    true,
                ),
            )
            .unwrap(),
    )
    .expect("formatless imageblock slice write");
    let formatless_asm = disassemble(&formatless).expect("disassemble formatless imageblock write");
    assert!(
        formatless_asm.contains("OpCapability StorageImageWriteWithoutFormat"),
        "{formatless_asm}"
    );
    assert!(formatless_asm.contains("Unknown"), "{formatless_asm}");
    tools::spirv_val_bytes(&formatless, &tmp).expect("formatless imageblock write spirv-val");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_implicit_imageblock_uses_indexed_storage_attachment_planes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %block, <2 x i16> %tid) {
entry:
  %color = call <4 x half> @air.load.implicit_imageblock.v4f16(i32 0, <2 x i16> %tid, i32 1, i16 0)
  call void @air.store.implicit_imageblock.v4f16(<4 x half> %color, i32 0, <2 x i16> %tid, i32 1, i16 0)
  %other = call <4 x float> @air.load.implicit_imageblock.v4f32(i32 2, <2 x i16> %tid, i32 0, i16 0)
  call void @air.store.implicit_imageblock.v4f32(<4 x float> %other, i32 2, <2 x i16> %tid, i32 0, i16 0)
  %scalar = call half @air.load.implicit_imageblock.f16(i32 1, <2 x i16> %tid, i32 0, i16 0)
  call void @air.store.implicit_imageblock.f16(half %scalar, i32 1, <2 x i16> %tid, i32 0, i16 0)
  %integer = call i32 @air.load.implicit_imageblock.i32(i32 3, <2 x i16> %tid, i32 0, i16 0)
  call void @air.store.implicit_imageblock.i32(i32 %integer, i32 3, <2 x i16> %tid, i32 0, i16 0)
  %pair = call <2 x half> @air.load.implicit_imageblock.v2f16(i32 4, <2 x i16> %tid, i32 0, i16 0)
  call void @air.store.implicit_imageblock.v2f16(<2 x half> %pair, i32 4, <2 x i16> %tid, i32 0, i16 0)
  ret void
}

declare <4 x half> @air.load.implicit_imageblock.v4f16(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16)
declare <4 x float> @air.load.implicit_imageblock.v4f32(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.v4f32(<4 x float>, i32, <2 x i16>, i32, i16)
declare half @air.load.implicit_imageblock.f16(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.f16(half, i32, <2 x i16>, i32, i16)
declare i32 @air.load.implicit_imageblock.i32(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.i32(i32, i32, <2 x i16>, i32, i16)
declare <2 x half> @air.load.implicit_imageblock.v2f16(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.v2f16(<2 x half>, i32, <2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.imageblock", !"implicit", !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<ColorData, layout_implicit>", !"air.arg_name", !"block"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"color", !"air.render_target", i32 0, i32 16, i32 16, i32 0, !"float4", !"other", !"air.render_target", i32 2, i32 32, i32 4, i32 0, !"uint", !"integer", !"air.render_target", i32 3}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_implicit_imageblock_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageRead"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(asm.contains("Rgba32f"), "{asm}");
    assert!(asm.contains("R16f"), "{asm}");
    assert!(asm.contains("R32ui"), "{asm}");
    assert!(asm.contains("Rg16f"), "{asm}");
    assert!(!asm.contains("OpBitcast"), "{asm}");
    assert!(asm.contains("Binding 200"), "{asm}");
    assert!(asm.contains("Binding 206"), "{asm}");
    assert!(!asm.contains("air.load.implicit_imageblock"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_embedded_array_imageblock_write_routes_field_and_slice() {
    let ll = r#"
%Args = type <{ %"struct.metal::texture2d_array" }>
%"struct.metal::texture2d_array" = type { ptr addrspace(1) }
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(ptr addrspace(2) %args, %"struct.metal::_imageblock_base" %block, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %field = getelementptr inbounds %Args, ptr addrspace(2) %args, i64 0, i32 0, i32 0
  %dst = load ptr addrspace(1), ptr addrspace(2) %field, align 8
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %ptr, align 8
  tail call void @air.write_imageblock_slice_to_texture_2d_array.i16.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 3, i16 0, i1 false, i32 3)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d_array.i16.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !6, !8, !9}
!3 = !{i32 0, !"air.indirect_buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Args", !"air.arg_name", !"args"}
!4 = !{i32 0, i32 8, i32 0, !"texture2d_array<half, write>", !"dst", !"air.indirect_argument", !5}
!5 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 1, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !7, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"block"}
!7 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!8 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!9 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_embedded_array_imageblock_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeImage") && line.contains("2D 0 1 0 2 Rgba16f")),
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
fn native_kernel_imageblock_slice_write_zero_extent_writes_transparent_zero() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %ptr, align 8
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 true, <2 x i16> zeroinitializer, <2 x i16> <i16 0, i16 1>, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_zero_extent_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpIEqual"), "{asm}");
    assert!(asm.contains("OpConstantNull"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_imageblock_full_threadgroup_block_copy_fits_a_narrow_cell() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %ptr, align 8
  %base = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %base, i1 true, <2 x i16> zeroinitializer, <2 x i16> splat (i16 32), <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_full_tile_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let length = asm
        .lines()
        .find_map(|line| {
            line.split_once(" = OpTypeArray ")?
                .1
                .split_whitespace()
                .nth(1)
        })
        .expect("imageblock cell array");
    let cells = asm
        .lines()
        .find_map(|line| {
            line.split_once(&format!("{length} = OpConstant "))?
                .1
                .split_whitespace()
                .nth(1)
        })
        .expect("cell array length");
    assert_eq!(cells, "1024", "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
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
fn native_kernel_imageblock_block_copy_is_bounded_by_the_threadgroup_byte_budget() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i32> %gid, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <8 x float> zeroinitializer, ptr addrspace(4) %ptr, align 32
  %base = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f32(ptr addrspace(1) %dst, ptr addrspace(4) %base, i1 true, <2 x i16> zeroinitializer, <2 x i16> splat (i16 32), <2 x i32> %gid, i32 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i32>, i32, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 32, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 32, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 32, i32 0, !"float8", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_wide_cell_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let err = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect_err("32-byte cell tile");
    assert!(err.contains("imageblock holds 512 cells"), "{err}");
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_runtime_slice_region_walks_a_spec_constant_trip_count() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid, <2 x i16> %tg_size) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x i16> zeroinitializer, ptr addrspace(4) %ptr, align 8
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4i16(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 true, <2 x i16> zeroinitializer, <2 x i16> %tg_size, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4i16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7, !8}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"short4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<short, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!8 = !{i32 4, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tgSize"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_runtime_slice_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let copy_loops = asm
        .split_once("OpControlBarrier")
        .map_or(asm.matches("OpLoopMerge").count(), |(_, after)| {
            after.matches("OpLoopMerge").count()
        });
    assert_eq!(copy_loops, 1, "{asm}");
    assert!(
        asm.matches("OpLoopMerge").count() - copy_loops <= 1,
        "one fill loop at most: {asm}"
    );
    let copy = asm
        .split_once("OpControlBarrier")
        .map_or(asm.as_str(), |(_, after)| after);
    assert_eq!(asm.matches("OpImageWrite").count(), 1, "{asm}");
    let result_of = |needle: &str| {
        copy.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in {asm}"))
            .split_whitespace()
            .next()
            .expect("result")
            .to_string()
    };
    let defining = |id: &str| {
        asm.lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} = ")))
            .unwrap_or_else(|| panic!("no definition of {id} in {asm}"))
            .to_string()
    };
    let operands = |line: &str| {
        line.split_whitespace()
            .skip(4)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let counted = defining(&result_of("OpULessThan "));
    let total = operands(&counted).last().expect("bound").to_string();
    let product = defining(&total);
    assert!(product.contains("OpIMul "), "{asm}");
    let axes = operands(&product);
    let sources = axes
        .iter()
        .map(|axis| operands(&defining(axis)))
        .collect::<Vec<_>>();
    assert!(
        sources.iter().all(|from| from.len() == 2)
            && sources[0][0] == sources[1][0]
            && sources[0][1] != sources[1][1],
        "the two factors must be the two lanes of one size operand: {axes:?} {sources:?} in {asm}"
    );
    assert!(asm.contains("OpSpecConstant"), "{asm}");
    let guard = copy
        .lines()
        .filter(|line| line.contains("OpULessThan "))
        .nth(1)
        .unwrap_or_else(|| panic!("no cell-index guard in {asm}"))
        .to_string();
    let limit = operands(&guard).last().expect("limit").to_string();
    assert!(
        defining(&limit).contains("OpConstant "),
        "the guard must bound the cell index by the array's own length: {asm}"
    );
    let joined = defining(&result_of("OpLogicalAnd "));
    assert_eq!(
        operands(&joined),
        vec![
            counted
                .split_whitespace()
                .next()
                .expect("result")
                .to_string(),
            guard.split_whitespace().next().expect("result").to_string(),
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
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_slice_write_retypes_byte_offset_half4() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  %field = getelementptr inbounds i8, ptr addrspace(4) %ptr, i64 8
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %field, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"a", i32 8, i32 8, i32 0, !"half4", !"b"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_slice_write_byte_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba16f"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
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
fn native_kernel_imageblock_slice_write_two_channel_half_pads_to_v4() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %store_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <2 x half> zeroinitializer, ptr addrspace(4) %store_ptr, align 4
  %write_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v2f16(ptr addrspace(1) %dst, ptr addrspace(4) %write_ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v2f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 4, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 4, i32 0, !"half2", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_slice_write_v2f16_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_imageblock_slice_write_int16_widens_to_v4_sint() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %store_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x i16> zeroinitializer, ptr addrspace(4) %store_ptr, align 8
  %write_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4i16(ptr addrspace(1) %dst, ptr addrspace(4) %write_ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4i16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"short4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<short, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_slice_write_v4i16_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpSConvert"), "{asm}");
    assert!(!asm.contains("air.imageblock"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_imageblock_dimensions_lower_to_single_slot_extent() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x float> zeroinitializer, ptr addrspace(4) %ptr, align 16
  %width = tail call i16 @air.get_imageblock_width()
  %size_x = insertelement <2 x i16> undef, i16 %width, i64 0
  %height = tail call i16 @air.get_imageblock_height()
  %size = insertelement <2 x i16> %size_x, i16 %height, i64 1
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v4f32(ptr addrspace(1) %dst, ptr addrspace(4) %ptr, i1 false, <2 x i16> %size, <2 x i16> zeroinitializer, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare i16 @air.get_imageblock_width()
declare i16 @air.get_imageblock_height()
declare void @air.write_imageblock_slice_to_texture_2d.i16.v4f32(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 16, i32 0, !"float4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<float, read_write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_dimensions_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpSpecConstant") && line.trim_end().ends_with(" 1")),
        "{asm}"
    );
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("air.get_imageblock"), "{asm}");
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
fn native_kernel_imageblock_data_uses_metadata_float4_pointee() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, <2 x i16> %tid, ptr addrspace(1) %out) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x float> zeroinitializer, ptr addrspace(4) %ptr, align 16
  %loaded = load <4 x float>, ptr addrspace(4) %ptr, align 16
  store <4 x float> %loaded, ptr addrspace(1) %out, align 16
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 16, i32 0, !"float4", !"v"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_data_float4_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(asm.contains("OpTypeFloat 32"), "{asm}");
    assert!(!asm.contains("OpTypeFloat 16"), "{asm}");
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
fn native_kernel_imageblock_widens_the_tile_by_the_per_thread_block() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %base = shl <2 x i16> %tid, splat (i16 1)
  %c0 = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %base, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %c0, align 8
  %right = or <2 x i16> %base, <i16 1, i16 0>
  %c1 = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %right, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %c1, align 8
  %origin = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %origin, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_wide_tile_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [2, 2, 1],
            ..passes::TransformOptions::default()
        },
    )
    .expect("a tile twice the threadgroup in each axis has a linearisation");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let constants = module
        .types_global_values
        .iter()
        .filter_map(|inst| match (inst.class.opcode, inst.operands.first()) {
            (Op::Constant, Some(Operand::LiteralBit32(value))) => Some((inst.result_id?, *value)),
            _ => None,
        })
        .collect::<crate::fxhash::HashMap<_, _>>();
    let second_operand_constant = |inst: &Instruction| {
        constants
            .get(&id_ref_operand(inst.operands.get(1)?)?)
            .copied()
    };
    let cells = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == Op::TypeArray)
        .and_then(second_operand_constant);
    assert_eq!(cells, Some(2048), "{asm}");
    let body = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(
        body.iter()
            .filter(|inst| inst.class.opcode == Op::IMul)
            .any(|inst| second_operand_constant(inst) == Some(2)),
        "{asm}"
    );
    assert!(
        body.iter()
            .filter(|inst| inst.class.opcode == Op::ULessThan)
            .any(|inst| second_operand_constant(inst) == Some(16)),
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
fn native_kernel_imageblock_refuses_a_nonlinear_cell_coordinate() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %base = mul <2 x i16> %tid, %gid
  %c0 = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %base, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %c0, align 8
  %right = or <2 x i16> %base, <i16 1, i16 0>
  %c1 = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %right, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %c1, align 8
  %origin = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %origin, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_nonlinear_tile_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let error = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [2, 2, 1],
            ..passes::TransformOptions::default()
        },
    )
    .expect_err("a coordinate scaled by a runtime value names no tile width");
    assert!(
        error.contains("not a linear function of a thread position"),
        "the refusal must name what it could not place inside the tile: {error}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_slice_write_copies_the_whole_block() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %store_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %store_ptr, align 8
  %origin = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %origin, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_block_copy_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native_with_options(
        ll,
        Stage::Kernel,
        &tmp,
        passes::TransformOptions {
            kernel_local_size: [3, 2, 1],
            ..passes::TransformOptions::default()
        },
    )
    .expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let copy_loops = asm
        .split_once("OpControlBarrier")
        .map_or(asm.matches("OpLoopMerge").count(), |(_, after)| {
            after.matches("OpLoopMerge").count()
        });
    assert_eq!(copy_loops, 1, "{asm}");
    assert!(
        asm.matches("OpLoopMerge").count() - copy_loops <= 1,
        "one fill loop at most: {asm}"
    );
    let copy = asm
        .split_once("OpControlBarrier")
        .map_or(asm.as_str(), |(_, after)| after);
    assert_eq!(asm.matches("OpImageWrite").count(), 1, "{asm}");
    let rhs = |needle: &str| {
        copy.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in {asm}"))
            .split_whitespace()
            .last()
            .expect("operand")
            .to_string()
    };
    let result_of = |needle: &str| {
        copy.lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no {needle} in {asm}"))
            .split_whitespace()
            .next()
            .expect("result")
            .to_string()
    };
    let constant = |id: &str| {
        asm.lines()
            .find(|line| line.starts_with(&format!("{id} = OpConstant ")))
            .map(|line| line.split_whitespace().last().expect("literal").to_string())
    };
    assert_eq!(
        constant(&rhs("OpULessThan ")).as_deref(),
        Some("6"),
        "{asm}"
    );
    let guard = copy
        .lines()
        .filter(|line| line.contains("OpULessThan "))
        .nth(1)
        .unwrap_or_else(|| panic!("no cell-index guard in {asm}"))
        .to_string();
    assert_eq!(
        constant(guard.split_whitespace().last().expect("limit")).as_deref(),
        Some("1024"),
        "{asm}"
    );
    assert!(asm.contains("OpLogicalAnd "), "{asm}");
    let dx = result_of("OpUMod ");
    let dy = result_of("OpUDiv ");
    let row_step = asm
        .lines()
        .find(|line| line.contains("OpIMul ") && line.contains(&format!(" {dy} ")))
        .unwrap_or_else(|| panic!("no row step in {asm}"))
        .to_string();
    let stride = row_step
        .split_whitespace()
        .last()
        .expect("stride")
        .to_string();
    assert!(
        asm.lines()
            .filter(|line| line.contains("OpIMul ") && line.ends_with(&stride))
            .count()
            >= 2,
        "{asm}"
    );
    let row = row_step.split_whitespace().next().expect("row").to_string();
    let cell_index = asm
        .lines()
        .find(|line| line.contains("OpIAdd ") && line.ends_with(&format!("{row} {dx}")))
        .map(|line| line.split_whitespace().next().expect("result").to_string())
        .unwrap_or_else(|| panic!("no `dy * stride + dx` cell index in {asm}"));
    assert!(
        asm.lines()
            .any(|line| line.contains("OpInBoundsAccessChain ") && line.ends_with(&cell_index)),
        "the loop must index the cell array with `dy * stride + dx`: {asm}"
    );
    let delta = asm
        .lines()
        .find(|line| line.contains("OpCompositeConstruct") && line.ends_with(&format!("{dx} {dy}")))
        .map(|line| line.split_whitespace().next().expect("result").to_string())
        .unwrap_or_else(|| panic!("no (dx, dy) offset in {asm}"));
    assert!(
        asm.lines()
            .any(|line| line.contains("OpIAdd ") && line.ends_with(&delta)),
        "{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Workgroup")),
        "{asm}"
    );
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Private")),
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
fn native_kernel_imageblock_data_calls_share_one_scratch_allocation() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %store_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %store_ptr, align 8
  %write_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %write_ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_shared_scratch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let scratch_vars = asm
        .lines()
        .filter(|line| {
            line.contains("OpVariable") && (line.contains("Private") || line.contains("Workgroup"))
        })
        .count();
    assert_eq!(scratch_vars, 1, "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_cross_coordinate_imageblock_is_shared_either_way() {
    let scratch = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid STRIDE_PARAM) {
entry:
  %store_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %store_ptr, align 8
  tail call void @air.wg.barrier(i32 8, i32 1)
  %write_ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> zeroinitializer, i32 0, i16 0)
  tail call void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1) %dst, ptr addrspace(4) %write_ptr, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.wg.barrier(i32, i32)
declare void @air.write_imageblock_slice_to_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7 STRIDE_META}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"v"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!8 = !{i32 4, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tpt"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_cross_coordinate_imageblock_shared_or_refused_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);

    let strided = scratch
        .replace("STRIDE_PARAM", ", <2 x i16> %tpt")
        .replace("STRIDE_META", ", !8");
    let spv = translate_one_cell_tile(&strided, Stage::Kernel, &tmp)
        .expect("a dispatch-supplied row stride linearizes the shared tile");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Workgroup")),
        "the shared tile has to live in Workgroup storage: {asm}"
    );

    let strideless = scratch
        .replace(" STRIDE_PARAM", "")
        .replace(" STRIDE_META", "");
    assert_ne!(
        strideless, scratch,
        "the template has no stride splice point"
    );
    let strideless = translate_one_cell_tile(&strideless, Stage::Kernel, &tmp)
        .expect("a shared tile with no stride parameter linearizes on the workgroup width");
    let strideless_asm = disassemble(&strideless).expect("disassemble");
    assert!(
        strideless_asm
            .lines()
            .any(|line| line.contains("OpVariable") && line.contains("Workgroup")),
        "the shared tile has to live in Workgroup storage: {strideless_asm}"
    );
    assert!(
        strideless_asm.contains("BuiltIn WorkgroupSize"),
        "the row stride has to be the workgroup size, not an invented constant: {strideless_asm}"
    );
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn native_kernel_imageblock_private_byte_view_loads_typed_half_lane() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, <2 x i16> %tid, ptr addrspace(1) %out) {
entry:
  %base = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store half 0xH3C00, ptr addrspace(4) %base, align 2
  %byte_lane = getelementptr inbounds i8, ptr addrspace(4) %base, i64 2
  %lane = bitcast ptr addrspace(4) %byte_lane to ptr addrspace(4)
  store half 0xH4000, ptr addrspace(4) %lane, align 2
  %value = load half, ptr addrspace(4) %lane, align 2
  store half %value, ptr addrspace(4) %base, align 2
  store half %value, ptr addrspace(1) %out, align 2
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 2, i32 0, !"half", !"a", i32 2, i32 2, i32 0, !"half", !"b", i32 4, i32 2, i32 0, !"half", !"c", i32 6, i32 2, i32 0, !"half", !"d"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_private_byte_half_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpLoad"), "{asm}");
    assert!(!asm.contains("OpPtrAccessChain"), "{asm}");
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
fn native_kernel_cross_coordinate_imageblock_uses_shared_workgroup_cells() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, <2 x i16> %tid, <2 x i16> %threads, ptr addrspace(1) %out) {
entry:
  %own = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store half 0xH3C00, ptr addrspace(4) %own, align 2
  tail call void @air.wg.barrier(i32 8, i32 1)
  %neighbor_coord = add <2 x i16> %tid, <i16 1, i16 0>
  %neighbor = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %neighbor_coord, i32 0, i16 0)
  %value = load half, ptr addrspace(4) %neighbor, align 2
  store half %value, ptr addrspace(4) %own, align 2
  store half %value, ptr addrspace(1) %out, align 2
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.wg.barrier(i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 2, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 2, i32 0, !"half", !"v"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!6 = !{i32 2, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"threads"}
!7 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 2, !"air.arg_type_align_size", i32 2, !"air.arg_type_name", !"half", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_cross_coordinate_imageblock_{}",
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
    assert!(
        asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Workgroup")),
        "{asm}"
    );
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Private")),
        "{asm}"
    );
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.ends_with(" 1024")),
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
fn native_kernel_cross_coordinate_imageblock_write_reads_struct_prefix_without_pointer_bitcast() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid, <2 x i16> %threads) {
entry:
  %own = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store half 0xH3C00, ptr addrspace(4) %own, align 2
  %neighbor_coord = add <2 x i16> %tid, <i16 1, i16 0>
  %neighbor = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %neighbor_coord, i32 0, i16 0)
  store half 0xH4000, ptr addrspace(4) %neighbor, align 2
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.f16(ptr addrspace(1) %dst, ptr addrspace(4) %own, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7, !8}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 2, i32 0, !"half", !"luma", i32 4, i32 4, i32 0, !"half2", !"chroma"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!8 = !{i32 4, !"air.threads_per_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"threads"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_cross_coordinate_imageblock_prefix_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_Workgroup"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_private_imageblock_byte_field_keeps_complete_cell_layout() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, ptr addrspace(1) %dst, <2 x i16> %gid, <2 x i16> %tid) {
entry:
  %cell = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  %luma = bitcast ptr addrspace(4) %cell to ptr addrspace(4)
  store half 0xH3C00, ptr addrspace(4) %luma, align 2
  %chroma = getelementptr inbounds i8, ptr addrspace(4) %cell, i64 4
  store <2 x half> zeroinitializer, ptr addrspace(4) %chroma, align 4
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.f16(ptr addrspace(1) %dst, ptr addrspace(4) %cell, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  tail call void @air.write_imageblock_slice_to_texture_2d.i16.v2f16(ptr addrspace(1) %dst, ptr addrspace(4) %chroma, i1 false, <2 x i16> zeroinitializer, <2 x i16> undef, <2 x i16> %gid, i16 0, i1 false, i32 2)
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)
declare void @air.write_imageblock_slice_to_texture_2d.i16.f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)
declare void @air.write_imageblock_slice_to_texture_2d.i16.v2f16(ptr addrspace(1), ptr addrspace(4), i1, <2 x i16>, <2 x i16>, <2 x i16>, i16, i1, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 2, i32 0, !"half", !"luma", i32 4, i32 4, i32 0, !"half2", !"chroma"}
!5 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!6 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!7 = !{i32 3, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_private_imageblock_byte_field_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = translate_one_cell_tile(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageWrite").count(), 2, "{asm}");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(!asm.contains("OpBitcast %_ptr_Private"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_backend_imageblock_dimensions_use_shared_workgroup_cells() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %img_blk, <2 x i16> %tid, ptr addrspace(1) %out) {
entry:
  %ptr = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store i32 7, ptr addrspace(4) %ptr, align 4
  %staged = load i32, ptr addrspace(4) %ptr, align 4
  store i32 %staged, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !7}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 4, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"v"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!7 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!apv.imageblock_dimensions = !{!6}
!6 = !{i32 4, i32 1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_imageblock_workgroup_scratch_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Workgroup")),
        "{asm}"
    );
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpVariable") && line.contains("Private")),
        "{asm}"
    );
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
fn native_kernel_helper_wrapped_sample_and_write_textures_use_single_images() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Out = type <{ %Tex, %Tex, <2 x i16> }>
%Tex = type { ptr addrspace(1) }

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601053257, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %out = alloca %Out, align 8
  %src_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  store ptr addrspace(1) %src, ptr %src_field, align 8
  %dst_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1, i32 0
  store ptr addrspace(1) %dst, ptr %dst_field, align 8
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 2
  store <2 x i16> zeroinitializer, ptr %coord_field, align 8
  call void @helper(ptr %out)
  ret void
}

define internal void @helper(ptr %out) {
entry:
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 2
  %coord = load <2 x i16>, ptr %coord_field, align 8
  %src_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  %src = load ptr addrspace(1), ptr %src_field, align 8
  %dst_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1, i32 0
  %dst = load ptr addrspace(1), ptr %dst_field, align 8
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> zeroinitializer, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %coord, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_sample_write_textures_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("OpImageSampleExplicitLod") || asm.contains("OpImageFetch"),
        "{asm}"
    );
    assert!(asm.contains("OpImageWrite"), "{asm}");
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
fn native_kernel_separate_helper_wrapped_sample_and_write_textures_recover_field_sources() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%In = type { %Tex }
%Out = type <{ %Tex, <2 x i16>, i8 }>
%Tex = type { ptr addrspace(1) }

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601053257, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst) {
entry:
  %out = alloca %Out, align 8
  %in = alloca %In, align 8
  %dst_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  store ptr addrspace(1) %dst, ptr %dst_field, align 8
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1
  store <2 x i16> zeroinitializer, ptr %coord_field, align 8
  %flag_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 2
  store i8 0, ptr %flag_field, align 4
  %src_field = getelementptr inbounds %In, ptr %in, i64 0, i32 0, i32 0
  store ptr addrspace(1) %src, ptr %src_field, align 8
  call void @helper(ptr %in, ptr %out)
  ret void
}

define internal void @helper(ptr %in, ptr %out) {
entry:
  %coord_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 1
  %coord = load <2 x i16>, ptr %coord_field, align 8
  %src_field = getelementptr inbounds %In, ptr %in, i64 0, i32 0, i32 0
  %src = load ptr addrspace(1), ptr %src_field, align 8
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> zeroinitializer, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %dst_field = getelementptr inbounds %Out, ptr %out, i64 0, i32 0, i32 0
  %dst = load ptr addrspace(1), ptr %dst_field, align 8
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %coord, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!5 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_helper_wrapped_separate_sample_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        asm.contains("OpImageSampleExplicitLod") || asm.contains("OpImageFetch"),
        "{asm}"
    );
    assert!(asm.contains("OpImageWrite"), "{asm}");
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
fn native_kernel_uint_array_write_texture_combines_layer_coord() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <3 x i32> %tid) {
entry:
  %x = extractelement <3 x i32> %tid, i64 0
  %y = extractelement <3 x i32> %tid, i64 1
  %layer = extractelement <3 x i32> %tid, i64 2
  %xy0 = insertelement <2 x i32> poison, i32 %x, i64 0
  %xy = insertelement <2 x i32> %xy0, i32 %y, i64 1
  %v0 = insertelement <4 x i32> undef, i32 1, i64 0
  %v1 = insertelement <4 x i32> %v0, i32 2, i64 1
  %v2 = insertelement <4 x i32> %v1, i32 3, i64 2
  %v3 = insertelement <4 x i32> %v2, i32 4, i64 3
  tail call void @air.write_texture_2d_array.u.v4i32(ptr addrspace(1) %dst, <2 x i32> %xy, i32 %layer, <4 x i32> %v3, i32 0, i32 2)
  ret void
}

declare void @air.write_texture_2d_array.u.v4i32(ptr addrspace(1), <2 x i32>, i32, <4 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<uint, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_uint_array_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba8ui"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("Binding 482"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_cube_write_texture_combines_face_coord() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %xy) {
entry:
  %v0 = insertelement <4 x float> poison, float 2.500000e-01, i64 0
  %v1 = insertelement <4 x float> %v0, float 5.000000e-01, i64 1
  %v2 = insertelement <4 x float> %v1, float 7.500000e-01, i64 2
  %color = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  tail call void @air.write_texture_cube.v4f32(ptr addrspace(1) %dst, <2 x i32> %xy, i32 2, <4 x float> %color, i32 0, i32 2)
  ret void
}

declare void @air.write_texture_cube.v4f32(ptr addrspace(1), <2 x i32>, i32, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texturecube<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"xy"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_cube_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Cube 0 0 0 2 R32f"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_fragment_cube_sample_grad_carries_three_lane_derivatives() {
    // World of Warcraft's sky / reflection fragment shaders: texturecube.sample(s, dir, gradientcube(dx, dy)).
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @f(ptr addrspace(1) %tex, ptr addrspace(2) %smp, <3 x float> %dir, <3 x float> %dx, <3 x float> %dy) {
entry:
  %s = tail call { <4 x float>, i8 } @air.sample_texture_cube_grad.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) %smp, <3 x float> %dir, <3 x float> %dx, <3 x float> %dy, float 0.000000e+00, i32 0)
  %c = extractvalue { <4 x float>, i8 } %s, 0
  ret <4 x float> %c
}

declare { <4 x float>, i8 } @air.sample_texture_cube_grad.v4f32(ptr addrspace(1), ptr addrspace(2), <3 x float>, <3 x float>, <3 x float>, float, i32)

!air.fragment = !{!0}
!0 = !{ptr @f, !1, !2}
!1 = !{!3}
!2 = !{!4, !5, !6, !7, !8}
!3 = !{i32 0, !"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!4 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texturecube<float, sample>", !"air.arg_name", !"tex"}
!5 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"smp"}
!6 = !{i32 2, !"air.fragment_input", !"user(locn0)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float3", !"air.arg_name", !"dir"}
!7 = !{i32 3, !"air.fragment_input", !"user(locn1)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float3", !"air.arg_name", !"dx"}
!8 = !{i32 4, !"air.fragment_input", !"user(locn2)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float3", !"air.arg_name", !"dy"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_cube_sample_grad_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Cube 0 0 0 1 Unknown"), "{asm}");
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(asm.contains("Grad"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_cube_array_write_texture_folds_slice_and_face_into_layer() {
    // World of Warcraft's texture-clear kernel: texturecube_array<float, write>.write(texel, xy, face, slice).
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %xy) {
entry:
  %x = extractelement <2 x i32> %xy, i64 0
  %y = extractelement <2 x i32> %xy, i64 1
  %face = and i32 %x, 5
  %slice = lshr i32 %y, 3
  %v0 = insertelement <4 x float> poison, float 2.500000e-01, i64 0
  %v1 = insertelement <4 x float> %v0, float 5.000000e-01, i64 1
  %v2 = insertelement <4 x float> %v1, float 7.500000e-01, i64 2
  %color = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  %w = tail call i32 @air.get_width_texture_cube_array(ptr addrspace(1) %dst, i32 0)
  %n = tail call i32 @air.get_array_size_texture_cube_array(ptr addrspace(1) %dst)
  tail call void @air.write_texture_cube_array.v4f32(ptr addrspace(1) %dst, <2 x i32> %xy, i32 %face, i32 %slice, <4 x float> %color, i32 0, i32 2)
  ret void
}

declare i32 @air.get_width_texture_cube_array(ptr addrspace(1), i32)
declare i32 @air.get_array_size_texture_cube_array(ptr addrspace(1))
declare void @air.write_texture_cube_array.v4f32(ptr addrspace(1), <2 x i32>, i32, i32, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texturecube_array<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"xy"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_cube_array_write_texture_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability ImageCubeArray"), "{asm}");
    assert!(asm.contains("Cube 0 1 0 2 R32f"), "{asm}");
    assert!(asm.contains("OpIMul"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpImageQuerySize"), "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_read_texture_array_widens_narrow_coord_before_layer_combine() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %sampler = tail call ptr addrspace(2) @air.get_read_sampler()
  %read = tail call { <4 x half>, i8 } @air.read_texture_2d_array.i16.v4f16(ptr addrspace(1) %src, ptr addrspace(2) %sampler, <2 x i16> %gid, i16 0, <2 x i16> zeroinitializer, i16 0, i32 0)
  %texel = extractvalue { <4 x half>, i8 } %read, 0
  tail call void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, i16 0, <4 x half> %texel, i16 0, i32 2)
  ret void
}

declare ptr addrspace(2) @air.get_read_sampler()
declare { <4 x half>, i8 } @air.read_texture_2d_array.i16.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x i16>, i16, <2 x i16>, i16, i32)
declare void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1), <2 x i16>, i16, <4 x half>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_read_texture_array_narrow_coord_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpCompositeConstruct"), "{asm}");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_read_depth_2d_i16_with_sampler_uses_vector_coord() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out, <2 x i16> %gid) {
entry:
  %sampler = tail call ptr addrspace(2) @air.get_read_sampler()
  %read = tail call { float, i8 } @air.read_depth_2d.i16.f32(ptr addrspace(1) %depth, ptr addrspace(2) %sampler, i32 1, <2 x i16> %gid, <2 x i16> zeroinitializer, i16 0, i32 1)
  %value = extractvalue { float, i8 } %read, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare ptr addrspace(2) @air.get_read_sampler()
declare { float, i8 } @air.read_depth_2d.i16.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x i16>, <2 x i16>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_read_depth_sampler_coord_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let fetch = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::ImageFetch)
        .expect("image fetch");
    let coord = match fetch.operands.get(1) {
        Some(Operand::IdRef(id)) => *id,
        other => panic!("unexpected image fetch coordinate operand: {other:?}"),
    };
    let coord_ty = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord))
        .and_then(|inst| inst.result_type)
        .expect("coordinate type");
    let coord_ty_inst = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord_ty))
        .expect("coordinate type definition");
    assert_eq!(coord_ty_inst.class.opcode, Op::TypeVector, "{asm}");
    assert!(
        matches!(
            coord_ty_inst.operands.get(1),
            Some(Operand::LiteralBit32(2))
        ),
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
fn native_read_depth_2d_i16_sample_index_uses_vector_coord() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %depth, ptr addrspace(1) %out, <2 x i16> %gid) {
entry:
  %read = tail call { float, i8 } @air.read_depth_2d.i16.f32(ptr addrspace(1) %depth, i32 1, <2 x i16> %gid, i16 0, i32 1)
  %value = extractvalue { float, i8 } %read, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare { float, i8 } @air.read_depth_2d.i16.f32(ptr addrspace(1), i32, <2 x i16>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d<float, read>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_read_depth_sample_index_coord_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let fetch = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::ImageFetch)
        .expect("image fetch");
    let coord = match fetch.operands.get(1) {
        Some(Operand::IdRef(id)) => *id,
        other => panic!("unexpected image fetch coordinate operand: {other:?}"),
    };
    let coord_ty = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord))
        .and_then(|inst| inst.result_type)
        .expect("coordinate type");
    let coord_ty_inst = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord_ty))
        .expect("coordinate type definition");
    assert_eq!(coord_ty_inst.class.opcode, Op::TypeVector, "{asm}");
    assert!(
        matches!(
            coord_ty_inst.operands.get(1),
            Some(Operand::LiteralBit32(2))
        ),
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
fn native_kernel_pixel_sampler_array_sample_lowers_to_fetch() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %c0 = insertelement <2 x float> poison, float 1.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 2.000000e+00, i32 1
  %sample = tail call <4 x half> @air.sample_texture_2d_array.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i16 0, i1 false, float 0.000000e+00, i32 0)
  tail call void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, i16 0, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare <4 x half> @air.sample_texture_2d_array.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i16, i1, float, i32)
declare void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1), <2 x i16>, i16, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_sampler_array_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpConvertFToS"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_pixel_linear_array_sampler_lowers_to_weighted_fetches() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601053184, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %c0 = insertelement <2 x float> poison, float 1.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 2.000000e+00, i32 1
  %sample = tail call { <4 x half>, i8 } @air.sample_texture_2d_array.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i32 1, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x half>, i8 } %sample, 0
  tail call void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, i16 1, <4 x half> %color, i16 0, i32 2)
  ret void
}

declare { <4 x half>, i8 } @air.sample_texture_2d_array.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i32, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d_array.i16.v4f16(ptr addrspace(1), <2 x i16>, i16, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d_array<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_linear_array_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains(" Floor "), "{asm}");
    assert!(asm.contains("OpVectorTimesScalar"), "{asm}");
    assert_eq!(asm.matches("OpFOrdEqual").count(), 4, "{asm}");
    assert!(asm.contains("OpFAdd"), "{asm}");
    assert!(asm.contains("OpLogicalAnd"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_integer_pixel_linear_sampler_lowers_to_fetch() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601053257, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %c0 = insertelement <2 x float> poison, float 2.500000e-01, i32 0
  %coord = insertelement <2 x float> %c0, float 7.500000e-01, i32 1
  %sample = tail call { <4 x i16>, i8 } @air.sample_texture_2d.u.v4i16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x i16>, i8 } %sample, 0
  tail call void @air.write_texture_2d.i16.u.v4i16(ptr addrspace(1) %dst, <2 x i16> %gid, <4 x i16> %color, i16 0, i32 2)
  ret void
}

declare { <4 x i16>, i8 } @air.sample_texture_2d.u.v4i16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.u.v4i16(ptr addrspace(1), <2 x i16>, <4 x i16>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<uint, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<ushort, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_integer_pixel_linear_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpConvertFToS"), "{asm}");
    assert!(asm.contains(" Floor "), "{asm}");
    assert!(!asm.contains("OpFMul"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_pixel_sampler_zero_address_guards_fetch_result() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050624, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %signed = tail call fast float @air.convert.f.f32.s.i32(i32 1)
  %coord_x = fadd fast float %signed, 8.000000e+00
  %c0 = insertelement <2 x float> poison, float %coord_x, i32 0
  %coord = insertelement <2 x float> %c0, float 2.000000e+00, i32 1
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare float @air.convert.f.f32.s.i32(i32)
declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_sampler_zero_address_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpLogicalAnd"), "{asm}");
    assert!(asm.contains("OpConstantNull"), "{asm}");
    assert_eq!(asm.matches("OpTypeInt 32 1").count(), 1, "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_pixel_sampler_2d_sample_offset_lowers_to_fetch_coord_add() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid) {
entry:
  %c0 = insertelement <2 x float> poison, float 1.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 2.000000e+00, i32 1
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> <i32 3, i32 0>, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_sampler_2d_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_pixel_sampler_dynamic_offset_lowers_to_fetch_coord_add() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant [2 x i64] [i64 34901797601050697, i64 0], align 8

define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %gid, i32 %lane) {
entry:
  %c0 = insertelement <2 x float> poison, float 1.000000e+00, i32 0
  %coord = insertelement <2 x float> %c0, float 2.000000e+00, i32 1
  %x = or i32 %lane, -8
  %o0 = insertelement <2 x i32> undef, i32 %x, i32 0
  %offset = insertelement <2 x i32> %o0, i32 0, i32 1
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %src, ptr addrspace(2) @__air_sampler_state, <2 x float> %coord, i1 true, <2 x i32> %offset, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  tail call void @air.write_texture_2d.i16.v4f16(ptr addrspace(1) %dst, <2 x i16> %gid, <4 x half> %sample, i16 0, i32 2)
  ret void
}

declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
declare void @air.write_texture_2d.i16.v4f16(ptr addrspace(1), <2 x i16>, <4 x half>, i16, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!7}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<half, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"gid"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"int", !"air.arg_name", !"lane"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_pixel_sampler_dynamic_offset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpIAdd"), "{asm}");
    assert!(!asm.contains("OpSampledImage"), "{asm}");
    assert!(!asm.contains("OpImageSample"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_texture_buffer_write_uses_storage_texel_buffer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, i32 %tid) {
entry:
  %v0 = insertelement <4 x float> undef, float 2.500000e-01, i64 0
  %v1 = insertelement <4 x float> %v0, float 5.000000e-01, i64 1
  %v2 = insertelement <4 x float> %v1, float 7.500000e-01, i64 2
  %v3 = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  tail call void @air.write_texture_buffer_1d.v4f32(ptr addrspace(1) %dst, i32 %tid, <4 x float> %v3, i32 2)
  ret void
}

declare void @air.write_texture_buffer_1d.v4f32(ptr addrspace(1), i32, <4 x float>, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture_buffer<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_texture_buffer_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability ImageBuffer"), "{asm}");
    assert!(!asm.contains("OpCapability SampledBuffer"), "{asm}");
    assert!(asm.contains("Buffer 0 0 0 2 R32f"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("Binding 480"), "{asm}");
    assert!(!asm.contains("OpCapability Sampled1D"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_texture_buffer_read_omits_lod_operand() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %out, i32 %tid) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.read_texture_buffer_1d.v4f32(ptr addrspace(1) %src, i32 %tid, i32 1)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_buffer_1d.v4f32(ptr addrspace(1), i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture_buffer<float, read>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_texture_buffer_read_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability SampledBuffer"), "{asm}");
    assert!(asm.contains("Buffer 0 0 0 1 Unknown"), "{asm}");
    let fetch = asm
        .lines()
        .find(|line| line.contains("OpImageFetch"))
        .expect("OpImageFetch");
    assert!(!fetch.contains(" Lod "), "{fetch}\n\n{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_texture1d_write_declares_storage_image1d_capability() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, i16 %x) {
entry:
  %v0 = insertelement <4 x float> undef, float 2.500000e-01, i64 0
  %v1 = insertelement <4 x float> %v0, float 5.000000e-01, i64 1
  %v2 = insertelement <4 x float> %v1, float 7.500000e-01, i64 2
  %v3 = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  tail call void @air.write_texture_1d.i16.v4f32(ptr addrspace(1) %dst, i16 %x, <4 x float> %v3, i16 0, i32 2)
  ret void
}

declare void @air.write_texture_1d.i16.v4f32(ptr addrspace(1), i16, <4 x float>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture1d<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort", !"air.arg_name", !"x"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_texture1d_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCapability Image1D"), "{asm}");
    assert!(!asm.contains("OpCapability Sampled1D"), "{asm}");
    assert!(asm.contains("1D 0 0 0 2 R32f"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_ushort_texture_read_write_uses_16bit_uint_images() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, <2 x i16> %tid) {
entry:
  %coord = shl <2 x i16> %tid, <i16 1, i16 1>
  %sample = tail call { <4 x i16>, i8 } @air.read_texture_2d.i16.u.v4i16(ptr addrspace(1) %src, <2 x i16> %coord, i16 0, i32 1)
  %color = extractvalue { <4 x i16>, i8 } %sample, 0
  %wide = zext <4 x i16> %color to <4 x i32>
  %quot = udiv <4 x i32> %wide, %wide
  %zero = icmp eq <4 x i32> %wide, zeroinitializer
  %selected = select <4 x i1> %zero, <4 x i32> zeroinitializer, <4 x i32> %quot
  %narrow = trunc <4 x i32> %selected to <4 x i16>
  %isnull = tail call i1 @air.is_null_texture_2d(ptr addrspace(1) %dst)
  tail call void @air.write_texture_2d.i16.u.v4i16(ptr addrspace(1) %dst, <2 x i16> %coord, <4 x i16> %narrow, i16 0, i32 2)
  ret void
}

declare { <4 x i16>, i8 } @air.read_texture_2d.i16.u.v4i16(ptr addrspace(1), <2 x i16>, i16, i32)
declare i1 @air.is_null_texture_2d(ptr addrspace(1))
declare void @air.write_texture_2d.i16.u.v4i16(ptr addrspace(1), <2 x i16>, <4 x i16>, i16, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<ushort, read>", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<ushort, write>", !"air.arg_name", !"dst"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_ushort_texture_read_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba16ui"), "{asm}");
    assert!(asm.contains("OpImageFetch"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(asm.contains("OpUConvert"), "{asm}");
    assert!(asm.contains("OpUDiv"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        if let Err(err) = tools::spirv_val_bytes(&spv, &tmp) {
            panic!("spirv-val: {err}\n{asm}");
        }
    }
}

#[test]
fn native_fragment_multisample_texture_read_uses_sample_operand() {
    let ll = r#"
target datalayout = "e-p:64:64:64"
target triple = "air64-apple-macosx14.0.0"

define <{ <4 x i32> }> @frag(ptr addrspace(1) %tex, i32 %sampleId) local_unnamed_addr #0 {
entry:
  %read = tail call { <4 x i32>, i8 } @air.read_texture_2d_ms.s.v4i32(ptr addrspace(1) %tex, <2 x i32> zeroinitializer, i32 %sampleId, i32 1)
  %color = extractvalue { <4 x i32>, i8 } %read, 0
  %out = insertvalue <{ <4 x i32> }> undef, <4 x i32> %color, 0
  ret <{ <4 x i32> }> %out
}

declare { <4 x i32>, i8 } @air.read_texture_2d_ms.s.v4i32(ptr addrspace(1), <2 x i32>, i32, i32)

attributes #0 = { nounwind }

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"int4"}
!3 = !{!4, !5}
!4 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d_ms<int, read>", !"air.arg_name", !"tex"}
!5 = !{i32 1, !"air.sample_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"sampleId"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_msaa_texture_read_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("BuiltIn SampleId"), "{asm}");
    assert!(asm.contains("OpCapability SampleRateShading"), "{asm}");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpTypeImage") && line.contains(" 1 1 Unknown")),
        "{asm}"
    );
    assert!(
        asm.lines()
            .any(|line| line.contains("OpImageFetch") && line.contains("Sample")),
        "{asm}"
    );
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpImageFetch") && line.contains("Lod")),
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
fn native_fragment_multisample_depth_read_takes_the_sample_after_the_coordinate() {
    let ll = r#"
target datalayout = "e-p:64:64:64"
target triple = "air64-apple-macosx14.0.0"

define <{ float }> @frag(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, i32 %sampleId) local_unnamed_addr #0 {
entry:
  %read = tail call { float, i8 } @air.read_depth_2d_ms.f32(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, i32 1, <2 x i32> zeroinitializer, i32 %sampleId, i32 1)
  %depth = extractvalue { float, i8 } %read, 0
  %out = insertvalue <{ float }> undef, float %depth, 0
  ret <{ float }> %out
}

declare { float, i8 } @air.read_depth_2d_ms.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x i32>, i32, i32)

attributes #0 = { nounwind }

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.depth", !"air.depth_qualifier", !"air.any", !"air.arg_type_name", !"float"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d_ms<float, read>", !"air.arg_name", !"tex"}
!5 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!6 = !{i32 2, !"air.sample_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"sampleId"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_fragment_msaa_depth_read_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let fetch = asm
        .lines()
        .find(|line| line.contains("OpImageFetch") && line.contains("Sample"))
        .expect(&asm);
    assert!(
        !asm.lines()
            .any(|line| line.contains("OpImageFetch") && line.contains("Lod")),
        "{asm}"
    );
    let sample = fetch
        .rsplit_once("Sample ")
        .expect(&asm)
        .1
        .trim()
        .to_string();
    let sample_id_variable = asm
        .lines()
        .find(|line| line.contains("OpDecorate") && line.contains("BuiltIn SampleId"))
        .and_then(|line| line.split_whitespace().nth(1))
        .expect(&asm)
        .to_string();
    let loaded = asm
        .lines()
        .find(|line| line.contains(" = OpLoad ") && line.ends_with(&sample_id_variable))
        .and_then(|line| line.split_whitespace().next())
        .expect(&asm);
    assert_eq!(
        loaded, sample,
        "fetch does not sample at the sample id\n{asm}"
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
fn native_multisample_texture_size_query_uses_query_size_without_lod() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %w = tail call i32 @air.get_width_texture_2d_ms(ptr addrspace(1) %tex)
  %h = tail call i32 @air.get_height_texture_2d_ms(ptr addrspace(1) %tex)
  store i32 %w, ptr addrspace(1) %out, align 4
  %out1 = getelementptr inbounds i32, ptr addrspace(1) %out, i64 1
  store i32 %h, ptr addrspace(1) %out1, align 4
  ret void
}

declare i32 @air.get_width_texture_2d_ms(ptr addrspace(1))
declare i32 @air.get_height_texture_2d_ms(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d_ms<half, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint*", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_ms_texture_size_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySize "), "{asm}");
    assert!(!asm.contains("OpImageQuerySizeLod"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_dynamic_texture_matrix_read_selects_texel_values() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %c, ptr addrspace(1) %d, ptr addrspace(1) %out, <2 x i32> %tid) {
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
  %row32 = extractelement <2 x i32> %tid, i64 0
  %column32 = extractelement <2 x i32> %tid, i64 1
  %row = zext i32 %row32 to i64
  %column = zext i32 %column32 to i64
  %slot = getelementptr inbounds [2 x [2 x ptr addrspace(1)]], ptr %table, i64 0, i64 %row, i64 %column
  %texture = load ptr addrspace(1), ptr %slot, align 8
  %sample = tail call { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1) %texture, <2 x i32> %tid, i32 0, i32 3)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7, !8}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"c"}
!6 = !{i32 3, !"air.texture", !"air.location_index", i32 3, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>", !"air.arg_name", !"d"}
!7 = !{i32 4, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!8 = !{i32 5, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_dynamic_texture_matrix_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageFetch").count(), 4, "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_dynamic_texture_table_sample_drops_stale_pointer_selects() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(2) %sampler, ptr addrspace(1) %out, i32 %index) {
entry:
  %table = alloca [2 x ptr addrspace(1)], align 8
  %slot0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 0
  store ptr addrspace(1) %a, ptr %slot0, align 8
  %slot1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  store ptr addrspace(1) %b, ptr %slot1, align 8
  %wide = zext i32 %index to i64
  %slot = getelementptr inbounds [2 x ptr addrspace(1)], ptr %table, i64 0, i64 %wide
  %texture = load ptr addrspace(1), ptr %slot, align 8
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %texture, ptr addrspace(2) %sampler, <2 x float> zeroinitializer, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  store <4 x float> %color, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"b"}
!5 = !{i32 2, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"sampler"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4*", !"air.arg_name", !"out"}
!7 = !{i32 4, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_dynamic_texture_table_sample_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let mut module = load_bytes(&spv).expect("load native spv");
    // Both table entries live in one descriptor array, so the choice folds into the *index* and a
    // single sample remains; the image-typed select itself (which NVK traps on) must be gone.
    assert_eq!(asm.matches("OpImageSampleExplicitLod").count(), 1, "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    assert!(!asm.contains("OpSelect  %image"), "{asm}");
    assert!(
        !crate::native::construct_opaque_image_selects_module(&mut module),
        "final resource construction must leave no sampled-image selection closure"
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
fn native_kernel_undef_texel_lanes_preserve_storage_image_channels() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %tid) {
entry:
  %texel = insertelement <4 x i16> undef, i16 7, i64 0
  tail call void @air.write_texture_2d.u.v4i16(ptr addrspace(1) %dst, <2 x i32> %tid, <4 x i16> %texel, i32 0, i32 2)
  ret void
}

declare void @air.write_texture_2d.u.v4i16(ptr addrspace(1), <2 x i32>, <4 x i16>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<ushort, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_undef_texel_lanes_preserve_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Rgba16ui"), "{asm}");
    assert!(asm.contains("OpImageRead"), "{asm}");
    assert!(asm.contains("OpCompositeExtract"), "{asm}");
    assert!(asm.contains("OpCompositeInsert"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        if let Err(err) = tools::spirv_val_bytes(&spv, &tmp) {
            panic!("spirv-val: {err}\n{asm}");
        }
    }
}

#[test]
fn native_kernel_same_texture_read_write_uses_storage_image_read() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %tex, <2 x i32> %tid) {
entry:
  %sample = tail call { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1) %tex, <2 x i32> %tid, i32 0, i32 3)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %tex, <2 x i32> %tid, <4 x float> %color, i32 0, i32 3)
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, i32, i32)
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<float, read_write>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_same_texture_read_write_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("R32f"), "{asm}");
    assert!(asm.contains("OpImageRead"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    assert!(!asm.contains("OpImageFetch"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_write_texture_size_query_uses_storage_query() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %tid) {
entry:
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %dst, i32 0)
  %h = tail call i32 @air.get_height_texture_2d(ptr addrspace(1) %dst, i32 0)
  %wf = tail call fast float @air.convert.f.f32.u.i32(i32 %w)
  %hf = tail call fast float @air.convert.f.f32.u.i32(i32 %h)
  %v0 = insertelement <4 x float> undef, float %wf, i64 0
  %v1 = insertelement <4 x float> %v0, float %hf, i64 1
  %v2 = insertelement <4 x float> %v1, float 0.000000e+00, i64 2
  %v3 = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %dst, <2 x i32> %tid, <4 x float> %v3, i32 0, i32 2)
  ret void
}

declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
declare i32 @air.get_height_texture_2d(ptr addrspace(1), i32)
declare float @air.convert.f.f32.u.i32(i32)
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 7, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_write_texture_size_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpImageQuerySize "), "{asm}");
    assert!(!asm.contains("OpImageQuerySizeLod"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_kernel_write_texture_mip_query_uses_single_level_constant() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %dst, <2 x i32> %tid) {
entry:
  %levels = tail call i32 @air.get_num_mip_levels_texture_2d(ptr addrspace(1) %dst)
  %levels_f = tail call fast float @air.convert.f.f32.u.i32(i32 %levels)
  %v0 = insertelement <4 x float> undef, float %levels_f, i64 0
  %v1 = insertelement <4 x float> %v0, float 0.000000e+00, i64 1
  %v2 = insertelement <4 x float> %v1, float 0.000000e+00, i64 2
  %v3 = insertelement <4 x float> %v2, float 1.000000e+00, i64 3
  tail call void @air.write_texture_2d.v4f32(ptr addrspace(1) %dst, <2 x i32> %tid, <4 x float> %v3, i32 0, i32 2)
  ret void
}

declare i32 @air.get_num_mip_levels_texture_2d(ptr addrspace(1))
declare float @air.convert.f.f32.u.i32(i32)
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 7, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"dst"}
!4 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"tid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_kernel_write_texture_mip_query_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(!asm.contains("OpImageQueryLevels"), "{asm}");
    assert!(asm.contains("OpImageWrite"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_texture_fence_lowers_to_image_memory_barrier() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"

define void @texture_fence(ptr addrspace(1) %tex) {
entry:
  tail call void @air.fence_texture_2d(ptr addrspace(1) %tex)
  ret void
}

declare void @air.fence_texture_2d(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @texture_fence, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"texture2d<float, read_write>", !"air.arg_name", !"tex"}
"#;
    let tmp =
        std::env::temp_dir().join(format!("metal2vulkan_texture_fence_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpMemoryBarrier"), "{asm}");
    assert!(!asm.contains("air.fence_texture"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_opaque_named_type_bodies_do_not_block_a_texture_handle() {
    let ll = r#"
source_filename = "case.metal"

%struct._texture_2d_t = type opaque
%struct._sampler_t = type opaque

define void @k(%struct._texture_2d_t addrspace(1)* nocapture readonly %0, %struct._sampler_t addrspace(2)* nocapture readonly %1, <4 x float> addrspace(1)* nocapture writeonly "air-buffer-no-alias" %2, <2 x i32> %3) {
  %5 = tail call fast <2 x float> @air.convert.f.v2f32.u.v2i32(<2 x i32> %3)
  %7 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(%struct._texture_2d_t addrspace(1)* nocapture readonly %0, %struct._sampler_t addrspace(2)* nocapture readonly %1, <2 x float> %5, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %8 = extractvalue { <4 x float>, i8 } %7, 0
  %9 = extractelement <2 x i32> %3, i64 0
  %10 = zext i32 %9 to i64
  %11 = getelementptr inbounds <4 x float>, <4 x float> addrspace(1)* %2, i64 %10
  store <4 x float> %8, <4 x float> addrspace(1)* %11, align 16
  ret void
}

declare <2 x float> @air.convert.f.v2f32.u.v2i32(<2 x i32>)
declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(%struct._texture_2d_t addrspace(1)*, %struct._sampler_t addrspace(2)*, <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.kernel = !{!9}
!9 = !{void (%struct._texture_2d_t addrspace(1)*, %struct._sampler_t addrspace(2)*, <4 x float> addrspace(1)*, <2 x i32>)* @k, !10, !11}
!10 = !{}
!11 = !{!12, !13, !14, !15}
!12 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"src"}
!13 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"s"}
!14 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"o"}
!15 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"g"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_opaque_named_type_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpTypeImage"), "{asm}");
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(!asm.contains("OpTypeStruct\n"), "{asm}");
}

#[test]
fn native_nested_texture_wrappers_survive_return_and_argument_helpers() {
    let ll = r#"%Slot = type { ptr addrspace(1) }
%Pair = type { float, %Slot, %Slot }
define internal fastcc %Pair @pack(ptr addrspace(1) %a, ptr addrspace(1) %b) {
entry:
  %p0 = insertvalue %Pair poison, float 0.5, 0
  %p1 = insertvalue %Pair %p0, ptr addrspace(1) %a, 1, 0
  %p2 = insertvalue %Pair %p1, ptr addrspace(1) %b, 2, 0
  ret %Pair %p2
}
define internal fastcc void @paint(%Slot %a, %Slot %b) {
entry:
  %left = extractvalue %Slot %a, 0
  %right = extractvalue %Slot %b, 0
  call void @air.write_texture_2d.v4f32(ptr addrspace(1) %left, <2 x i32> zeroinitializer, <4 x float> <float 1.0, float 0.0, float 0.0, float 1.0>, i32 0, i32 2)
  call void @air.write_texture_2d.v4f32(ptr addrspace(1) %right, <2 x i32> zeroinitializer, <4 x float> <float 0.0, float 1.0, float 0.0, float 1.0>, i32 0, i32 2)
  ret void
}
define void @nested_resources(ptr addrspace(1) %a, ptr addrspace(1) %b) {
entry:
  %pair = call fastcc %Pair @pack(ptr addrspace(1) %a, ptr addrspace(1) %b)
  %left = extractvalue %Pair %pair, 1
  %right = extractvalue %Pair %pair, 2
  call fastcc void @paint(%Slot %left, %Slot %right)
  ret void
}
declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)
!air.kernel = !{!0}
!0 = !{ptr @nested_resources, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"a"}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"b"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "m2v_nested_resource_helpers_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("scratch");
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp)
        .expect("nested resource translation");
    tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    let asm = disassemble(&spv).expect("disassemble");
    let writes = asm
        .lines()
        .filter(|line| line.contains("OpImageWrite"))
        .map(|line| {
            line.split_whitespace()
                .nth(1)
                .expect("image operand")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2, "{asm}");
    assert_ne!(
        writes[0], writes[1],
        "distinct nested fields must retain distinct resources: {asm}"
    );
    std::fs::remove_dir_all(tmp).expect("remove scratch");
}

#[test]
fn native_multisample_depth_query_rejects_wrong_image_and_scalar_shapes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out) {
entry:
  %n = tail call i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1) %tex)
  store i32 %n, ptr addrspace(1) %out, align 4
  ret void
}

declare i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d_ms<float, read>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let cases = [
        (
            ll.replace("depth2d_ms<float, read>", "depth2d<float, read>"),
            "multisample/array ABI",
        ),
        (
            ll.replace(
                "air.get_num_samples_depth_2d_ms",
                "air.get_num_samples_depth_2d_ms_array",
            ),
            "multisample/array ABI",
        ),
        (
            ll.replace("call i32", "call i64")
                .replace("declare i32", "declare i64")
                .replace("store i32 %n", "store i64 %n"),
            "32-bit integer result",
        ),
        (
            ll.replace("(ptr addrspace(1) %tex)", "(ptr addrspace(1) %tex, i32 0)")
                .replace(
                    "declare i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1))",
                    "declare i32 @air.get_num_samples_depth_2d_ms(ptr addrspace(1), i32)",
                ),
            "exactly one image operand",
        ),
    ];
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_depth_query_invalid_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    for (source, reason) in cases {
        let error = crate::translate_sanitized_native(&source, Stage::Kernel, &tmp)
            .expect_err("unsupported image query shape must fail");
        assert!(error.contains(reason), "{error}; expected {reason}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
