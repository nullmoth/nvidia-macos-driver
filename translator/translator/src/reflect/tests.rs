use super::*;

#[test]
fn kernel_dispatch_push_constant_range_is_exact_and_validated() {
    assert_eq!(
        KernelDispatch::safe_default(),
        KernelDispatch::ThreadsDynamic {
            offset: DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
        }
    );
    let dispatch = KernelDispatch::ThreadsDynamic { offset: 16 };
    assert_eq!(
        dispatch.push_constant_range(),
        Some(KernelDispatchPushConstantRange {
            offset: 16,
            size: KERNEL_DISPATCH_PUSH_CONSTANT_SIZE,
        })
    );
    assert!(dispatch.validate().is_ok());
    assert!(KernelDispatch::ThreadsDynamic { offset: 2 }
        .validate()
        .is_err());
    assert!(KernelDispatch::ThreadsDynamic {
        offset: u32::MAX - 3,
    }
    .validate()
    .is_err());
    assert_eq!(
        KernelDispatch::ThreadsFixed {
            threads_per_grid: [21, 3, 1],
        }
        .push_constant_range(),
        Some(KernelDispatchPushConstantRange {
            offset: DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
            size: 48,
        })
    );
}

#[test]
fn exact_thread_plan_partitions_interior_and_edges_without_surplus_lanes() {
    let plan = KernelDispatch::ThreadsDynamic { offset: 16 }
        .plan([8, 4, 2], Some([17, 6, 3]))
        .expect("plan");
    assert_eq!(plan.threadgroups_per_grid, [3, 2, 2]);
    assert_eq!(plan.regions.len(), 8);
    let launched = plan
        .regions
        .iter()
        .map(|region| {
            region
                .local_size
                .into_iter()
                .zip(region.group_count)
                .map(|(local, groups)| u64::from(local) * u64::from(groups))
                .product::<u64>()
        })
        .sum::<u64>();
    assert_eq!(launched, 17 * 6 * 3);
    assert!(plan.regions.contains(&KernelDispatchRegion {
        local_size: [1, 2, 1],
        group_count: [1, 1, 1],
        thread_base: [16, 4, 2],
        threadgroup_base: [2, 1, 1],
    }));

    let empty = KernelDispatch::ThreadsDynamic { offset: 0 }
        .plan([8, 4, 2], Some([0, 6, 3]))
        .expect("a zero-sized Metal grid is a no-op");
    assert_eq!(empty.threadgroups_per_grid, [0, 2, 2]);
    assert!(empty.regions.is_empty());

    assert!(KernelDispatch::ThreadsFixed {
        threads_per_grid: [17, 6, 3],
    }
    .plan([8, 4, 2], Some([18, 6, 3]))
    .is_err());
}
use crate::meta::{
    parse_air_fragment_meta, parse_air_vertex_meta, EmbeddedTexture, KernMeta, KernRole,
};

const FRAG_LL: &str = r#"
!air.fragment = !{!15}
!15 = !{ptr @F, !16, !18}
!16 = !{!17}
!17 = !{!"air.render_target", i32 0, i32 0}
!18 = !{!19, !20, !21, !22, !23}
!19 = !{i32 0, !"air.position", !"air.center"}
!20 = !{i32 1, !"air.fragment_input", !"generated", !"air.arg_type_name", !"float2", !"air.arg_name", !"texCoord"}
!21 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"texture2d<float, sample>"}
!22 = !{i32 3, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 5, i32 1}
!23 = !{i32 4, !"air.sampler", !"air.location_index", i32 2, i32 1}
"#;

#[test]
fn fragment_reflection_matches_abi_convention() {
    assert_eq!(ADDRESS_SPACE_DEVICE, 1);
    assert_eq!(ADDRESS_SPACE_CONSTANT, 2);
    assert_eq!(ADDRESS_SPACE_THREADGROUP, 3);

    let meta = parse_air_fragment_meta(FRAG_LL).unwrap();
    let r = ShaderReflection::from_fragment(&meta, Some("myFragment"));

    assert_eq!(r.reflection_version, REFLECTION_VERSION);
    assert_eq!(r.stage, ShaderStage::Fragment);
    assert_eq!(r.entry_point.as_deref(), Some("myFragment"));

    let tex = r.binding_at(ResourceKind::Texture, 0).expect("texture 0");
    assert_eq!(
        tex.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: TEXTURE_BINDING_BASE,
            count: 1,
        })
    );
    assert_eq!(tex.param_index, Some(2));
    assert_eq!(tex.type_name.as_deref(), Some("texture2d<float, sample>"));
    assert_eq!(tex.access, Some(ResourceAccess::Sampled));

    let buf = r.binding_at(ResourceKind::Buffer, 5).expect("buffer 5");
    assert_eq!(
        buf.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: 5,
            count: 1,
        })
    );
    assert_eq!(buf.declared_size, Some(32));

    let smp = r.binding_at(ResourceKind::Sampler, 2).expect("sampler 2");
    assert_eq!(
        smp.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: SAMPLER_BINDING_BASE + 2,
            count: 1,
        })
    );

    let v = r
        .varyings
        .iter()
        .find(|v| v.location == 0)
        .expect("varying 0");
    assert_eq!(v.type_name.as_deref(), Some("float2"));
    assert_eq!(v.name.as_deref(), Some("texCoord"));
    assert_eq!(v.user_semantic.as_deref(), Some("generated"));

    assert_eq!(r.render_targets.len(), 1);
    assert_eq!(r.render_targets[0].member_index, 0);
    assert_eq!(r.render_targets[0].location, 0);

    assert!(r
        .bindings
        .iter()
        .all(|b| b.kind != ResourceKind::ColorInput));
}

#[test]
fn reflected_buffer_footprint_reports_static_and_global_id_strides() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(i32 %gid, ptr addrspace(1) %input, ptr addrspace(1) %output) {
entry:
  %wide = zext i32 %gid to i64
  %shifted = add i64 %wide, 2
  %source = getelementptr inbounds i32, ptr addrspace(1) %input, i64 %shifted
  %value = load i32, ptr addrspace(1) %source, align 4
  %fixed1 = getelementptr inbounds i32, ptr addrspace(1) %input, i64 1
  %a = load i32, ptr addrspace(1) %fixed1, align 4
  %fixed2 = getelementptr inbounds i32, ptr addrspace(1) %input, i64 2
  %b = load i32, ptr addrspace(1) %fixed2, align 4
  %sum = add i32 %value, %a
  %sum2 = add i32 %sum, %b
  store i32 %sum2, ptr addrspace(1) %output, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"input"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_buffer_footprint_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let plain = crate::translate_sanitized_native(ll, crate::passes::Stage::Kernel, &tmp)
        .expect("plain translate");
    assert_eq!(spv, plain, "footprint reflection must remain byte-neutral");

    let input = reflection
        .binding_at(ResourceKind::Buffer, 1)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("input footprint");
    assert_eq!(
        input.static_ranges,
        vec![BufferByteRange { offset: 4, size: 8 }]
    );
    assert_eq!(
        input.strided_accesses,
        vec![BufferStridedAccess {
            base_offset: 8,
            access_size: 4,
            terms: vec![BufferStrideTerm {
                source: BufferIndexSource::GlobalInvocationIdX,
                stride: 4,
            }],
        }],
        "{input:?}"
    );
    assert!(!input.has_unbounded_access);

    let output = reflection
        .binding_at(ResourceKind::Buffer, 2)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("output footprint");
    assert_eq!(
        output.static_ranges,
        vec![BufferByteRange { offset: 0, size: 4 }]
    );
    assert!(output.strided_accesses.is_empty());
    assert!(!output.has_unbounded_access);

    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_buffer_footprint_is_empty_for_a_descriptor_the_module_never_declares() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(i32 %gid, ptr addrspace(1) %output, ptr addrspace(2) %params) {
entry:
  tail call fastcc void @helper(ptr addrspace(1) %output, ptr addrspace(2) %params, i32 %gid)
  ret void
}

define internal fastcc void @helper(ptr addrspace(1) %out, ptr addrspace(2) %ignored, i32 %gid) {
entry:
  store i32 %gid, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
!5 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"params", !"air.arg_name", !"params"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_unreachable_descriptor_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let plain = crate::translate_sanitized_native(ll, crate::passes::Stage::Kernel, &tmp)
        .expect("plain translate");
    assert_eq!(spv, plain, "footprint reflection must remain byte-neutral");

    let params = reflection
        .binding_at(ResourceKind::Buffer, 2)
        .expect("params binding is still reflected");
    let footprint = params.footprint.as_ref().expect("params footprint");
    assert!(footprint.static_ranges.is_empty(), "{footprint:?}");
    assert!(footprint.strided_accesses.is_empty(), "{footprint:?}");
    assert!(
        !footprint.has_unbounded_access,
        "a descriptor the Logical module never declares executes zero bytes: {footprint:?}"
    );
    assert_eq!(
        params.access,
        Some(ResourceAccess::Unused),
        "AIR declares this parameter air.read, but the same absence that empties its footprint \
         also proves no access can happen through it"
    );

    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_buffer_footprint_marks_data_dependent_index_unbounded() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr addrspace(1) %indices, ptr addrspace(1) %data, ptr addrspace(1) %output) {
entry:
  %index = load i32, ptr addrspace(1) %indices, align 4
  %wide = zext i32 %index to i64
  %source = getelementptr inbounds i32, ptr addrspace(1) %data, i64 %wide
  %value = load i32, ptr addrspace(1) %source, align 4
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"indices"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"data"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_unbounded_footprint_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");

    let indices = reflection
        .binding_at(ResourceKind::Buffer, 0)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("indices footprint");
    assert_eq!(
        indices.static_ranges,
        vec![BufferByteRange { offset: 0, size: 4 }]
    );
    assert!(!indices.has_unbounded_access);

    let data = reflection
        .binding_at(ResourceKind::Buffer, 1)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("data footprint");
    assert!(data.static_ranges.is_empty());
    assert!(data.strided_accesses.is_empty());
    assert!(data.has_unbounded_access);

    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_vertex_buffer_footprints_use_vertex_and_instance_indices() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define <4 x float> @v(i32 %vid, i32 %iid, ptr addrspace(1) %vertices, ptr addrspace(1) %instances) {
entry:
  %vwide = zext i32 %vid to i64
  %vptr = getelementptr inbounds float, ptr addrspace(1) %vertices, i64 %vwide
  %x = load float, ptr addrspace(1) %vptr, align 4
  %iwide = zext i32 %iid to i64
  %iptr = getelementptr inbounds float, ptr addrspace(1) %instances, i64 %iwide
  %y = load float, ptr addrspace(1) %iptr, align 4
  %p0 = insertelement <4 x float> poison, float %x, i64 0
  %p1 = insertelement <4 x float> %p0, float %y, i64 1
  %p2 = insertelement <4 x float> %p1, float 0.000000e+00, i64 2
  %p3 = insertelement <4 x float> %p2, float 1.000000e+00, i64 3
  ret <4 x float> %p3
}

!air.vertex = !{!0}
!0 = !{ptr @v, !1, !2}
!1 = !{!3}
!2 = !{!4, !5, !6, !7}
!3 = !{!"air.position", !"air.arg_type_name", !"float4"}
!4 = !{i32 0, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vid"}
!5 = !{i32 1, !"air.instance_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"iid"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"vertices"}
!7 = !{i32 3, !"air.buffer", !"air.location_index", i32 4, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"instances"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_vertex_footprint_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Vertex,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");

    for (metal_index, source) in [
        (3, BufferIndexSource::VertexIndex),
        (4, BufferIndexSource::InstanceIndex),
    ] {
        let footprint = reflection
            .binding_at(ResourceKind::Buffer, metal_index)
            .and_then(|binding| binding.footprint.as_ref())
            .expect("buffer footprint");
        assert_eq!(
            footprint.strided_accesses,
            [BufferStridedAccess {
                base_offset: 0,
                access_size: 4,
                terms: vec![BufferStrideTerm { source, stride: 4 }],
            }],
            "metal buffer {metal_index}: {footprint:?}"
        );
        assert!(!footprint.has_unbounded_access);
    }

    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_buffer_footprint_preserves_multiaxis_affine_terms() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(<3 x i32> %tid, ptr addrspace(1) %grid, ptr addrspace(1) %output) {
entry:
  %x = extractelement <3 x i32> %tid, i64 0
  %y = extractelement <3 x i32> %tid, i64 1
  %xwide = zext i32 %x to i64
  %ywide = zext i32 %y to i64
  %source = getelementptr inbounds [8 x i32], ptr addrspace(1) %grid, i64 %xwide, i64 %ywide
  %value = load i32, ptr addrspace(1) %source, align 4
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint3", !"air.arg_name", !"tid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"grid"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_multiaxis_footprint_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let footprint = reflection
        .binding_at(ResourceKind::Buffer, 0)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("grid footprint");
    assert_eq!(
        footprint.strided_accesses,
        [BufferStridedAccess {
            base_offset: 0,
            access_size: 4,
            terms: vec![
                BufferStrideTerm {
                    source: BufferIndexSource::GlobalInvocationIdX,
                    stride: 32,
                },
                BufferStrideTerm {
                    source: BufferIndexSource::GlobalInvocationIdY,
                    stride: 4,
                },
            ],
        }],
        "{footprint:?}"
    );
    assert!(!footprint.has_unbounded_access);
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_buffer_footprint_covers_every_cross_binding_pointer_arm() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @k(i32 %gid, ptr addrspace(1) %a, ptr addrspace(1) %b, ptr addrspace(1) %output) {
entry:
  %wide = zext i32 %gid to i64
  %choose_a = icmp eq i32 %gid, 0
  %selected = select i1 %choose_a, ptr addrspace(1) %a, ptr addrspace(1) %b
  %source = getelementptr inbounds i32, ptr addrspace(1) %selected, i64 %wide
  %value = load i32, ptr addrspace(1) %source, align 4
  store i32 %value, ptr addrspace(1) %output, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"a"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"b"}
!6 = !{i32 3, !"air.buffer", !"air.location_index", i32 3, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"output"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_pointer_arms_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let expected = [BufferStridedAccess {
        base_offset: 0,
        access_size: 4,
        terms: vec![BufferStrideTerm {
            source: BufferIndexSource::GlobalInvocationIdX,
            stride: 4,
        }],
    }];
    for metal_index in [1, 2] {
        let footprint = reflection
            .binding_at(ResourceKind::Buffer, metal_index)
            .and_then(|binding| binding.footprint.as_ref())
            .expect("selected buffer footprint");
        assert_eq!(footprint.strided_accesses, expected, "{footprint:?}");
        assert!(!footprint.has_unbounded_access);
    }
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_buffer_footprint_counts_atomic_memory_width() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_atomic" = type { i32 }

define void @k(ptr addrspace(1) %counts) {
entry:
  %slot = getelementptr inbounds %"struct.metal::_atomic", ptr addrspace(1) %counts, i64 3, i32 0
  %old = tail call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %slot, i32 1, i32 0, i32 2, i1 true)
  ret void
}

declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i1)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"atomic_uint", !"air.arg_name", !"counts"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_atomic_footprint_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let footprint = reflection
        .binding_at(ResourceKind::Buffer, 0)
        .and_then(|binding| binding.footprint.as_ref())
        .expect("atomic footprint");
    assert_eq!(
        footprint.static_ranges,
        [BufferByteRange {
            offset: 12,
            size: 4,
        }],
        "{footprint:?}"
    );
    assert!(!footprint.has_unbounded_access);
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn a_synthesized_read_sampler_reports_the_state_it_needs() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state.1 = internal addrspace(2) constant i64 -9188470239253738862, align 8

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out, i32 %gid) {
entry:
  %read = tail call { <4 x float>, i8 } @air.read_texture_cube.v4f32(ptr addrspace(1) %tex, <2 x i32> zeroinitializer, i32 0, i32 0, i32 0)
  %fetched = extractvalue { <4 x float>, i8 } %read, 0
  %sample = tail call { <4 x float>, i8 } @air.sample_texture_cube.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state.1, <3 x float> <float 1.000000e+00, float 0.000000e+00, float 0.000000e+00>, i1 true, float 0.000000e+00, float 0.000000e+00, i32 0)
  %sampled = extractvalue { <4 x float>, i8 } %sample, 0
  %value = fadd <4 x float> %fetched, %sampled
  store <4 x float> %value, ptr addrspace(1) %out, align 16
  ret void
}

declare { <4 x float>, i8 } @air.read_texture_cube.v4f32(ptr addrspace(1), <2 x i32>, i32, i32, i32)
declare { <4 x float>, i8 } @air.sample_texture_cube.v4f32(ptr addrspace(1), ptr addrspace(2), <3 x float>, i1, float, float, i32)

!air.kernel = !{!0}
!air.sampler_states = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texturecube<float>"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!6 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.1}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_synth_read_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Kernel,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let synthesized = reflection
        .bindings
        .iter()
        .find(|binding| binding.kind == ResourceKind::SynthesizedReadSampler)
        .expect("the cube read synthesizes a sampler");
    let state = synthesized
        .static_sampler
        .expect("the synthesized sampler reports the state a consumer must bind");
    assert_eq!(state.min_filter, SamplerFilter::Nearest);
    assert_eq!(state.mag_filter, SamplerFilter::Nearest);
    assert_eq!(state.mip_filter, SamplerMipFilter::Nearest);
    assert_eq!(state.address_mode_s, SamplerAddressMode::ClampToEdge);
    assert_eq!(state.address_mode_t, SamplerAddressMode::ClampToEdge);
    assert_eq!(state.address_mode_r, SamplerAddressMode::ClampToEdge);
    assert_eq!(state.coordinates, SamplerCoordinates::Normalized);
    assert_eq!(state.max_anisotropy, 1);
    assert_eq!(state.lod_bias, 0.0);
    assert_eq!(state.raw_words, [0; 2]);
}

#[test]
fn an_inline_integer_sampler_reflects_as_a_static_sampler() {
    // Older Metal toolchains (here: Apple LLVM 902.14, SDK 10.15.4) write a constexpr sampler's packed state word
    // straight into the call as an inttoptr constant, with no `!air.sampler_states` entry. 0x8000000000080a92 is
    // address::repeat with filter::linear.
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %sample = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) inttoptr (i64 -9223372036854248814 to ptr addrspace(2)), <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00)
  %value = tail call <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half> %sample)
  ret <4 x float> %value
}

declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float)
declare <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half>)

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<half, sample>"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_inline_integer_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (_, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Fragment,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("an inline integer sampler translates");
    let static_samplers = reflection
        .bindings
        .iter()
        .filter(|binding| binding.kind == ResourceKind::StaticSampler)
        .collect::<Vec<_>>();
    assert_eq!(static_samplers.len(), 1);
    let state = static_samplers[0]
        .static_sampler
        .expect("the inline word is reported as the sampler's state");
    assert_eq!(state.address_mode_s, SamplerAddressMode::Repeat);
    assert_eq!(state.address_mode_t, SamplerAddressMode::Repeat);
    assert_eq!(state.address_mode_r, SamplerAddressMode::Repeat);
    assert_eq!(state.min_filter, SamplerFilter::Linear);
    assert_eq!(state.mag_filter, SamplerFilter::Linear);
    assert_eq!(state.mip_filter, SamplerMipFilter::None);
    assert_eq!(state.coordinates, SamplerCoordinates::Normalized);
    let _ = std::fs::remove_dir_all(tmp);
}

#[test]
fn reflected_static_samplers_stay_in_sampler_band_and_carry_state() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state.118 = internal addrspace(2) constant i64 -9188470239253755319, align 8
@__air_sampler_state.119 = internal addrspace(2) constant i64 -9188470239253757806, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex, ptr addrspace(2) %runtime_sampler, <4 x float> %color0) {
entry:
  %sample0 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state.118, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value0 = extractvalue { <4 x float>, i8 } %sample0, 0
  %sample1 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) @__air_sampler_state.119, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %value1 = extractvalue { <4 x float>, i8 } %sample1, 0
  %value = fadd <4 x float> %value0, %value1
  ret <4 x float> %value
}

declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)

!air.fragment = !{!0}
!air.sampler_states = !{!9, !8}
!air.compile_options = !{!10}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6, !7, !11}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!7 = !{i32 3, !"air.sampler", !"air.location_index", i32 0, i32 1}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.118}
!9 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.119}
!10 = !{!"air.compile.framebuffer_fetch_enable"}
!11 = !{i32 4, !"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_reflect_static_sampler_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let (spv, reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Fragment,
        &tmp,
        crate::passes::TransformOptions::default(),
    )
    .expect("translate");
    let asm = crate::disassemble(&spv).expect("disassemble");
    assert!(asm.contains("Binding 160"), "{asm}");
    assert!(asm.contains("Binding 161"), "{asm}");
    assert!(asm.contains("Binding 162"), "{asm}");
    assert!(asm.contains("Binding 192"), "{asm}");
    assert!(!asm.contains("Binding 193"), "{asm}");
    assert!(!asm.contains("Binding 194"), "{asm}");
    let color_input = reflection
        .binding_at(ResourceKind::ColorInput, 0)
        .expect("framebuffer-fetch input");
    assert_eq!(color_input.type_name.as_deref(), Some("float4"));

    let static_samplers = reflection
        .bindings
        .iter()
        .filter(|binding| binding.kind == ResourceKind::StaticSampler)
        .collect::<Vec<_>>();
    assert_eq!(static_samplers.len(), 2);
    assert_eq!(
        static_samplers
            .iter()
            .filter_map(|binding| binding.descriptor.map(|descriptor| descriptor.binding))
            .collect::<Vec<_>>(),
        vec![161, 162]
    );
    let linear = static_samplers[0]
        .static_sampler
        .expect("linear static state");
    assert_eq!(linear.min_filter, SamplerFilter::Linear);
    assert_eq!(linear.mag_filter, SamplerFilter::Linear);
    assert_eq!(linear.address_mode_s, SamplerAddressMode::ClampToEdge);
    assert_eq!(linear.address_mode_t, SamplerAddressMode::ClampToEdge);
    assert_eq!(linear.address_mode_r, SamplerAddressMode::ClampToEdge);
    assert_eq!(linear.compare_function, SamplerCompareFunction::Never);
    assert_eq!(linear.lod_min_clamp, 0.0);
    assert_eq!(linear.lod_max_clamp, 65504.0);

    let repeat = static_samplers[1]
        .static_sampler
        .expect("repeat static state");
    assert_eq!(repeat.min_filter, SamplerFilter::Nearest);
    assert_eq!(repeat.mag_filter, SamplerFilter::Nearest);
    assert_eq!(repeat.address_mode_s, SamplerAddressMode::Repeat);
    assert_eq!(repeat.address_mode_t, SamplerAddressMode::Repeat);
    assert_eq!(repeat.address_mode_r, SamplerAddressMode::Repeat);

    let layout = DescriptorLayout {
        set: 3,
        samplers: DescriptorBindingRange {
            start: 800,
            end: 832,
        },
        ..Default::default()
    };
    let (custom_spv, custom_reflection) = crate::translate_sanitized_native_reflected(
        ll,
        crate::passes::Stage::Fragment,
        &tmp,
        crate::passes::TransformOptions::default()
            .with_descriptor_layout(layout)
            .expect("custom sampler layout"),
    )
    .expect("translate custom sampler layout");
    let custom_asm = crate::disassemble(&custom_spv).expect("disassemble custom sampler layout");
    for binding in [800, 801, 802] {
        assert!(
            custom_asm.contains(&format!("Binding {binding}")),
            "{custom_asm}"
        );
    }
    assert!(custom_asm.contains("DescriptorSet 3"), "{custom_asm}");
    assert_eq!(custom_reflection.descriptor_layout, layout);
    assert_eq!(
        custom_reflection
            .bindings
            .iter()
            .filter(|binding| binding.kind == ResourceKind::StaticSampler)
            .filter_map(|binding| binding.descriptor)
            .collect::<Vec<_>>(),
        vec![
            DescriptorLocation {
                set: 3,
                binding: 801,
                count: 1,
            },
            DescriptorLocation {
                set: 3,
                binding: 802,
                count: 1,
            },
        ]
    );
    crate::tools::spirv_val_bytes(&custom_spv, &tmp).expect("custom sampler spirv-val");
}

#[test]
fn fragment_and_vertex_buffer_address_space_exported_when_known() {
    use crate::meta::{FragRole, VertRole};
    let mut frag = crate::meta::FragMeta {
        roles: vec![(0, FragRole::Buffer(0)), (1, FragRole::Buffer(1))],
        ..Default::default()
    };
    frag.buffer_address_spaces.insert(0, ADDRESS_SPACE_CONSTANT);
    frag.buffer_type_sizes.insert(0, 64);
    let rf = ShaderReflection::from_fragment(&frag, Some("f"));
    let b0 = rf.binding_at(ResourceKind::Buffer, 0).expect("buffer 0");
    assert_eq!(b0.address_space, Some(ADDRESS_SPACE_CONSTANT));
    assert_eq!(b0.declared_size, Some(64));
    let b1 = rf.binding_at(ResourceKind::Buffer, 1).expect("buffer 1");
    assert_eq!(b1.address_space, None);
    assert_eq!(b1.declared_size, None);

    let mut vert = crate::meta::VertMeta {
        roles: vec![(0, VertRole::Buffer(3))],
        ..Default::default()
    };
    vert.buffer_address_spaces.insert(0, ADDRESS_SPACE_DEVICE);
    let rv = ShaderReflection::from_vertex(&vert, Some("v"));
    let vb = rv
        .binding_at(ResourceKind::Buffer, 3)
        .expect("vertex buffer 3");
    assert_eq!(vb.address_space, Some(ADDRESS_SPACE_DEVICE));
}

const VERT_LL: &str = r#"
!air.vertex = !{!5}
!5 = !{ptr @V, !6, !8}
!6 = !{!7, !11}
!7 = !{i32 0, !"air.position", !"air.center"}
!8 = !{!9, !10}
!9 = !{i32 0, !"air.vertex_input", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos", !"air.location_index", i32 0, i32 1}
!10 = !{i32 1, !"air.buffer", !"air.location_index", i32 2, i32 1}
!11 = !{!"air.vertex_output", !"generated(2uvDv2_f)", !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
"#;

#[test]
fn vertex_reflection_exports_attributes_and_buffer() {
    let meta = parse_air_vertex_meta(VERT_LL).unwrap();
    let r = ShaderReflection::from_vertex(&meta, Some("myVertex"));
    assert_eq!(r.stage, ShaderStage::Vertex);

    let buf = r.binding_at(ResourceKind::Buffer, 2).expect("buffer 2");
    assert_eq!(
        buf.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: 2,
            count: 1,
        })
    );

    let attr = r
        .vertex_attributes
        .iter()
        .find(|a| a.location == 0)
        .expect("attribute 0");
    assert_eq!(attr.type_name.as_deref(), Some("float4"));
    assert_eq!(attr.name.as_deref(), Some("pos"));
    assert_eq!(
        r.varyings,
        [Varying {
            location: 0,
            type_name: Some("float2".into()),
            name: Some("uv".into()),
            user_semantic: Some("generated(2uvDv2_f)".into()),
        }]
    );
}

#[test]
fn tessellation_reflection_exports_patch_interface_locations() {
    let ll = r#"
!air.vertex = !{!0}
!0 = !{ptr @tes, !1, !2, !8}
!1 = !{!3}
!2 = !{!4, !7, !9}
!3 = !{!"air.position", !"air.arg_type_name", !"float4"}
!4 = !{i32 0, !"air.patch_control_point_input", !5, !6}
!5 = !{!"air.patch_control_point_function", ptr @control.MTL_CONTROL_POINT_FN}
!6 = !{!"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"float3"}
!7 = !{i32 1, !"air.patch_input", !"air.location_index", i32 4, i32 1, !"air.arg_type_name", !"float4"}
!8 = !{!"air.patch", !"quad", !"air.patch_control_point", i32 16}
!9 = !{i32 2, !"air.instance_id", !"air.arg_type_name", !"uint"}
"#;
    let meta = parse_air_vertex_meta(ll).unwrap();
    let reflection = ShaderReflection::from_vertex(&meta, Some("tes"));
    assert_eq!(reflection.stage, ShaderStage::TessellationEvaluation);
    let tessellation = reflection.tessellation.unwrap();
    assert_eq!(tessellation.control_point_count, 16);
    assert_eq!(tessellation.control_point_locations, [1]);
    assert_eq!(tessellation.patch_input_locations, [4]);
    assert_eq!(tessellation.control_point_attributes[0].location, 1);
    assert_eq!(
        tessellation.control_point_attributes[0]
            .type_name
            .as_deref(),
        Some("float3")
    );
    assert_eq!(tessellation.patch_attributes[0].location, 4);
    assert_eq!(
        tessellation.patch_attributes[0].type_name.as_deref(),
        Some("float4")
    );
    assert_eq!(
        tessellation.instance_id,
        Some(TessellationAttribute {
            location: 5,
            type_name: Some("uint".into()),
        })
    );
}

#[test]
fn kernel_reflection_threadgroup_buffer_has_no_descriptor() {
    let mut meta = KernMeta {
        roles: vec![
            (0, KernRole::Buffer(0)),
            (1, KernRole::Buffer(1)),
            (2, KernRole::Texture(0)),
        ],
        ..Default::default()
    };
    meta.buffer_address_spaces.insert(0, ADDRESS_SPACE_DEVICE);
    meta.buffer_address_spaces
        .insert(1, ADDRESS_SPACE_THREADGROUP);
    meta.buffer_type_sizes.insert(0, 64);
    meta.texture_type_names
        .insert(2, "texture2d<float, sample>".to_string());
    meta.embedded_textures.push(EmbeddedTexture {
        buffer_param_index: 0,
        buffer_index: 0,
        field_offset: 8,
        field_ordinal: 1,
        argument_index: 0,
        dim: spirv::Dim::Dim2D,
        arrayed: false,
        comp: crate::passes::ImageComp::Float,
        storage_format: None,
        array_length: Some(2),
        synthetic_texture_index: 3,
    });
    meta.embedded_arguments.push(crate::meta::EmbeddedArgument {
        buffer_param_index: 0,
        buffer_index: 0,
        field_ordinal: 1,
        field_offset: 8,
        argument_index: 0,
        resource_buffer_index: None,
        resource_address_space: None,
        resource_declared_size: None,
        resource_access: None,
    });
    meta.embedded_arguments.push(crate::meta::EmbeddedArgument {
        buffer_param_index: 0,
        buffer_index: 0,
        field_ordinal: 2,
        field_offset: 16,
        argument_index: 7,
        resource_buffer_index: Some(7),
        resource_address_space: Some(ADDRESS_SPACE_DEVICE),
        resource_declared_size: Some(32),
        resource_access: Some(crate::meta::BufferAccess::ReadWrite),
    });

    let r = ShaderReflection::from_kernel(&meta, Some("myKernel"), [64, 1, 1]);
    assert_eq!(r.stage, ShaderStage::Kernel);
    assert_eq!(r.local_size, Some([64, 1, 1]));

    let dev = r
        .binding_at(ResourceKind::Buffer, 0)
        .expect("device buffer");
    assert_eq!(
        dev.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: 0,
            count: 1,
        })
    );
    assert_eq!(dev.declared_size, Some(64));
    assert_eq!(dev.address_space, Some(ADDRESS_SPACE_DEVICE));

    let tg = r
        .binding_at(ResourceKind::ThreadgroupBuffer, 1)
        .expect("threadgroup buffer");
    assert_eq!(tg.descriptor, None);
    assert_eq!(tg.address_space, Some(ADDRESS_SPACE_THREADGROUP));

    let tex = r.binding_at(ResourceKind::Texture, 0).expect("texture");
    assert_eq!(
        tex.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: TEXTURE_BINDING_BASE,
            count: 1,
        })
    );

    let emb = r
        .binding_at(ResourceKind::EmbeddedArgBufferTexture, 3)
        .expect("embedded texture");
    assert_eq!(
        emb.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: TEXTURE_BINDING_BASE + 3,
            count: 2,
        })
    );
    assert_eq!(emb.param_index, None);
    assert_eq!(
        emb.embedded_source,
        Some(EmbeddedArgBuffer {
            buffer_param_index: 0,
            buffer_index: 0,
            field_offset: 8,
            field_ordinal: 1,
            argument_index: 0,
            resource_buffer_index: None,
        })
    );
    let shape = emb.texture_shape.expect("embedded texture_shape");
    assert_eq!(shape.dimension, crate::meta::TextureDimension::D2);
    assert_eq!(shape.component, crate::meta::TextureComponent::Float);
    assert!(!shape.writable);
    assert!(shape.array_ref);
    assert_eq!(shape.array_length, Some(2));

    let nested = r
        .binding_at(ResourceKind::EmbeddedArgBufferBuffer, 7)
        .expect("embedded buffer");
    assert_eq!(nested.descriptor, None);
    assert_eq!(nested.param_index, None);
    assert_eq!(
        nested.embedded_source,
        Some(EmbeddedArgBuffer {
            buffer_param_index: 0,
            buffer_index: 0,
            field_offset: 16,
            field_ordinal: 2,
            argument_index: 7,
            resource_buffer_index: Some(7),
        })
    );
}

#[test]
fn kernel_stage_inputs_share_the_lowerings_synthetic_buffer_allocator() {
    let mut meta = KernMeta {
        roles: vec![
            (0, KernRole::Buffer(0)),
            (1, KernRole::StageInput(6)),
            (2, KernRole::StageInput(9)),
        ],
        ..Default::default()
    };
    meta.stage_input_type_names.insert(1, "uint3".into());
    meta.stage_input_type_names.insert(2, "float2".into());
    let reflection = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    let first = reflection
        .binding_at(ResourceKind::KernelStageInput, 1)
        .expect("first free buffer slot");
    assert_eq!(first.param_index, Some(1));
    assert_eq!(first.stage_input_location, Some(6));
    assert_eq!(first.type_name.as_deref(), Some("uint3"));
    assert_eq!(first.access, Some(ResourceAccess::ReadOnly));
    assert_eq!(first.extent, Some(BufferExtent::Unbounded));
    assert_eq!(
        first.descriptor,
        Some(DescriptorLocation {
            set: RESOURCE_DESCRIPTOR_SET,
            binding: 1,
            count: 1,
        })
    );
    let second = reflection
        .binding_at(ResourceKind::KernelStageInput, 2)
        .expect("second free buffer slot");
    assert_eq!(second.stage_input_location, Some(9));
    assert_eq!(second.descriptor.expect("descriptor").binding, 2);
}

#[test]
fn imageblock_layouts_exported_for_kernel() {
    use crate::meta::{AirScalar, AirType};
    let mut meta = KernMeta {
        roles: vec![(0, KernRole::Buffer(0))],
        ..Default::default()
    };
    let tile = AirType::Vec {
        scalar: AirScalar::Float,
        lanes: 4,
    };
    meta.imageblock_layouts.insert(2, tile.clone());
    let r = ShaderReflection::from_kernel(&meta, Some("k"), [8, 8, 1]);
    assert_eq!(r.imageblock_layouts.len(), 1);
    assert_eq!(r.imageblock_layouts[0].param_index, 2);
    assert_eq!(r.imageblock_layouts[0].type_layout, tile);
    let frag = parse_air_fragment_meta(FRAG_LL).unwrap();
    assert!(ShaderReflection::from_fragment(&frag, None)
        .imageblock_layouts
        .is_empty());
}

#[test]
fn primitive_acceleration_structure_has_a_distinct_geometry_shadow_contract() {
    let meta = KernMeta {
        roles: vec![(0, KernRole::PrimitiveAccelerationStructureShadow(5))],
        ..KernMeta::default()
    };
    let reflection = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    let binding = reflection
        .binding_at(ResourceKind::PrimitiveAccelerationStructure, 5)
        .expect("primitive acceleration structure");
    assert_eq!(binding.param_index, Some(0));
    assert_eq!(binding.access, Some(ResourceAccess::ReadOnly));
    assert_eq!(
        binding.descriptor,
        Some(DescriptorLocation {
            set: 0,
            binding: 5,
            count: 1,
        })
    );

    let native_only = KernMeta {
        roles: vec![(0, KernRole::PrimitiveAccelerationStructure(6))],
        ..KernMeta::default()
    };
    let reflection = ShaderReflection::from_kernel(&native_only, Some("k"), [1, 1, 1]);
    let binding = reflection
        .binding_at(ResourceKind::PrimitiveAccelerationStructure, 6)
        .expect("native-only primitive acceleration structure");
    assert_eq!(binding.descriptor, None);
}

#[test]
fn vertex_builtins_report_index_and_position_usage() {
    let mut meta = crate::meta::VertMeta {
        roles: vec![
            (0, VertRole::VertexId),
            (1, VertRole::InstanceId),
            (2, VertRole::Buffer(0)),
        ],
        ..Default::default()
    };
    meta.output_roles = vec![crate::meta::VertOutRole::Position];
    let r = ShaderReflection::from_vertex(&meta, Some("v"));
    let b = r.vertex_builtins.expect("vertex_builtins present");
    assert!(b.uses_vertex_index);
    assert!(b.uses_instance_index);
    assert!(b.writes_position);

    let plain = crate::meta::VertMeta {
        roles: vec![(0, VertRole::Buffer(0))],
        ..Default::default()
    };
    let rp = ShaderReflection::from_vertex(&plain, Some("v"));
    let bp = rp.vertex_builtins.expect("vertex_builtins present");
    assert_eq!(bp, VertexBuiltins::default());

    let kern = KernMeta::default();
    assert_eq!(
        ShaderReflection::from_kernel(&kern, Some("k"), [1, 1, 1]).vertex_builtins,
        None
    );
}

#[test]
fn texture_access_classification_matches_declared_qualifier() {
    let cases = [
        (
            0u32,
            "texture2d<float, sample>",
            ResourceKind::Texture,
            ResourceAccess::Sampled,
        ),
        (
            1,
            "texture2d<uint, read>",
            ResourceKind::Texture,
            ResourceAccess::Sampled,
        ),
        (
            2,
            "texture2d<uint, write>",
            ResourceKind::StorageImage,
            ResourceAccess::Storage,
        ),
        (
            3,
            "texture2d<float, read_write>",
            ResourceKind::StorageImage,
            ResourceAccess::Storage,
        ),
        (
            4,
            "array_ref<texture2d<float, sample>>",
            ResourceKind::TextureArray,
            ResourceAccess::Sampled,
        ),
        (
            5,
            "array_ref<texture2d<float, write>>",
            ResourceKind::TextureArray,
            ResourceAccess::Storage,
        ),
        (
            6,
            "array<texture2d<half, sample>, 32>",
            ResourceKind::TextureArray,
            ResourceAccess::Sampled,
        ),
    ];
    let mut meta = KernMeta {
        roles: cases
            .iter()
            .enumerate()
            .map(|(i, (n, ..))| (i as u32, KernRole::Texture(*n)))
            .collect(),
        ..Default::default()
    };
    for (i, (_, name, ..)) in cases.iter().enumerate() {
        meta.texture_type_names.insert(i as u32, name.to_string());
    }
    let r = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    for (n, name, kind, access) in cases {
        let b = r
            .binding_at(kind, n)
            .unwrap_or_else(|| panic!("binding {n}"));
        assert_eq!(b.kind, kind, "kind for texture {n}");
        assert_eq!(b.access, Some(access), "access for texture {n}");
        let shape = b.texture_shape.expect("texture shape");
        assert_eq!(
            shape.array_length,
            (name == "array<texture2d<half, sample>, 32>").then_some(32),
            "fixed array length for {name}"
        );
        assert_eq!(
            b.descriptor.expect("texture descriptor").count,
            match (kind, shape.array_length) {
                (ResourceKind::TextureArray, Some(length)) => length,
                (ResourceKind::TextureArray, None) =>
                    crate::meta::TEXTURE_HANDLE_ARRAY_DESCRIPTOR_COUNT,
                _ => 1,
            },
            "descriptor count for {name}"
        );
    }
}

#[test]
fn texture_shape_exports_dimensionality_arrayed_multisampled_component() {
    use crate::meta::{TextureComponent as TC, TextureDimension as TD};
    let cases = [
        (
            0u32,
            "texture1d<float, sample>",
            TD::D1,
            false,
            false,
            TC::Float,
        ),
        (
            1,
            "texture1d_array<float, sample>",
            TD::D1,
            true,
            false,
            TC::Float,
        ),
        (2, "texture2d<uint, read>", TD::D2, false, false, TC::Uint),
        (
            3,
            "texture2d_array<int, sample>",
            TD::D2,
            true,
            false,
            TC::Sint,
        ),
        (
            4,
            "texture2d_ms<float, read>",
            TD::D2,
            false,
            true,
            TC::Float,
        ),
        (
            5,
            "texture3d<half, sample>",
            TD::D3,
            false,
            false,
            TC::Float,
        ),
        (
            6,
            "texturecube<float, sample>",
            TD::Cube,
            false,
            false,
            TC::Float,
        ),
        (
            7,
            "texturecube_array<float, sample>",
            TD::Cube,
            true,
            false,
            TC::Float,
        ),
        (
            8,
            "texture_buffer<uint, read>",
            TD::Buffer,
            false,
            false,
            TC::Uint,
        ),
        (
            9,
            "array_ref<texture2d_array<uint, sample>>",
            TD::D2,
            true,
            false,
            TC::Uint,
        ),
    ];
    let mut meta = KernMeta {
        roles: cases
            .iter()
            .enumerate()
            .map(|(i, (n, ..))| (i as u32, KernRole::Texture(*n)))
            .collect(),
        ..Default::default()
    };
    for (i, (_, name, ..)) in cases.iter().enumerate() {
        meta.texture_type_names.insert(i as u32, name.to_string());
    }
    let r = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    for (n, name, dim, arrayed, ms, comp) in cases {
        let b = r
            .bindings
            .iter()
            .find(|b| b.metal_index == n && b.texture_shape.is_some())
            .unwrap_or_else(|| panic!("texture binding {n} ({name})"));
        let shape = b.texture_shape.expect("texture_shape present");
        assert_eq!(shape.dimension, dim, "dim for {name}");
        assert_eq!(shape.arrayed, arrayed, "arrayed for {name}");
        assert_eq!(shape.multisampled, ms, "multisampled for {name}");
        assert_eq!(shape.component, comp, "component for {name}");
        assert_eq!(shape.storage_format, None, "storage_format for {name}");
    }
}

#[test]
fn storage_image_texel_format_exported() {
    use crate::meta::TextureFormat as TF;
    let cases = [
        (0u32, "texture2d<float, write>", TF::R32f),
        (1, "texture2d<half, write>", TF::Rgba16f),
        (2, "texture2d<uint, write>", TF::Rgba8ui),
        (3, "texture2d<ushort, read_write>", TF::Rgba16ui),
        (4, "texture2d<int, write>", TF::Rgba8i),
        (5, "texture2d<short, write>", TF::Rgba16i),
    ];
    let mut meta = KernMeta {
        roles: cases
            .iter()
            .enumerate()
            .map(|(i, (n, ..))| (i as u32, KernRole::Texture(*n)))
            .collect(),
        ..Default::default()
    };
    for (i, (_, name, _)) in cases.iter().enumerate() {
        meta.texture_type_names.insert(i as u32, name.to_string());
    }
    let r = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    for (n, name, fmt) in cases {
        let b = r
            .binding_at(ResourceKind::StorageImage, n)
            .unwrap_or_else(|| panic!("storage image {n} ({name})"));
        let shape = b.texture_shape.expect("texture_shape present");
        assert_eq!(shape.storage_format, Some(fmt), "storage_format for {name}");
        assert!(shape.writable, "writable for {name}");
    }
}

#[test]
fn constant_buffer_is_read_only_device_is_unknown() {
    let mut meta = KernMeta {
        roles: vec![(0, KernRole::Buffer(0)), (1, KernRole::Buffer(1))],
        ..Default::default()
    };
    meta.buffer_address_spaces.insert(0, ADDRESS_SPACE_CONSTANT);
    meta.buffer_address_spaces.insert(1, ADDRESS_SPACE_DEVICE);
    let r = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    assert_eq!(
        r.binding_at(ResourceKind::Buffer, 0).unwrap().access,
        Some(ResourceAccess::ReadOnly)
    );
    assert_eq!(r.binding_at(ResourceKind::Buffer, 1).unwrap().access, None);
}

#[cfg(feature = "serde")]
#[test]
fn reflection_serde_round_trips() {
    let meta = parse_air_fragment_meta(FRAG_LL).unwrap();
    let r = ShaderReflection::from_fragment(&meta, Some("myFragment"));
    let json = serde_json::to_string(&r).expect("serialize reflection");
    let back: ShaderReflection = serde_json::from_str(&json).expect("deserialize reflection");
    assert_eq!(r, back);
    assert_eq!(back.reflection_version, REFLECTION_VERSION);
}

#[cfg(feature = "serde")]
#[test]
fn reflection_serde_covers_every_reflection_field() {
    use crate::meta::{texture_shape_from_name, AirScalar, AirType, FunctionConstant};
    let storage_tex = texture_shape_from_name("texture2d<float, write>");
    assert!(storage_tex.writable && storage_tex.storage_format.is_some());
    let r = ShaderReflection {
        reflection_version: REFLECTION_VERSION,
        descriptor_layout: DescriptorLayout::default(),
        stage: ShaderStage::Kernel,
        entry_point: Some("k".to_string()),
        bindings: vec![
            ResourceBinding {
                kind: ResourceKind::Buffer,
                metal_index: 0,
                descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(0)),
                param_index: Some(0),
                stage_input_location: None,
                address_space: Some(2),
                declared_size: Some(64),
                extent: Some(BufferExtent::Object { bytes: 64 }),
                footprint: Some(BufferFootprint {
                    static_ranges: vec![BufferByteRange {
                        offset: 16,
                        size: 12,
                    }],
                    strided_accesses: vec![BufferStridedAccess {
                        base_offset: 32,
                        access_size: 16,
                        terms: vec![BufferStrideTerm {
                            source: BufferIndexSource::GlobalInvocationIdX,
                            stride: 16,
                        }],
                    }],
                    has_unbounded_access: true,
                }),
                type_layout: Some(AirType::Vec {
                    scalar: AirScalar::Float,
                    lanes: 4,
                }),
                type_name: Some("float4".to_string()),
                texture_shape: None,
                embedded_source: None,
                access: Some(ResourceAccess::ReadOnly),
                static_sampler: None,
            },
            ResourceBinding {
                kind: ResourceKind::Texture,
                metal_index: 0,
                descriptor: ResourceBinding::descriptor_at(texture_resource_binding(0)),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: None,
                footprint: None,
                type_layout: None,
                type_name: Some("texture2d<float, write>".to_string()),
                texture_shape: Some(storage_tex),
                embedded_source: Some(EmbeddedArgBuffer {
                    buffer_param_index: 0,
                    buffer_index: 0,
                    field_offset: 8,
                    field_ordinal: 1,
                    argument_index: 0,
                    resource_buffer_index: None,
                }),
                access: Some(ResourceAccess::Storage),
                static_sampler: None,
            },
            ResourceBinding {
                kind: ResourceKind::StaticSampler,
                metal_index: 1,
                descriptor: ResourceBinding::descriptor_at(sampler_resource_binding(1)),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: None,
                footprint: None,
                type_layout: None,
                type_name: None,
                texture_shape: None,
                embedded_source: None,
                access: None,
                static_sampler: Some(
                    StaticSamplerState::from_air_words([
                        0x807b_ff00_0008_0a49,
                        0x0000_0000_0000_3c00,
                    ])
                    .expect("static sampler"),
                ),
            },
        ],
        argument_buffer_fields: vec![EmbeddedArgBuffer {
            buffer_param_index: 0,
            buffer_index: 0,
            field_offset: 8,
            field_ordinal: 1,
            argument_index: 0,
            resource_buffer_index: None,
        }],
        vertex_attributes: vec![VertexAttribute {
            location: 0,
            type_name: Some("float4".to_string()),
            name: Some("pos".to_string()),
        }],
        varyings: vec![Varying {
            location: 0,
            type_name: Some("float2".to_string()),
            name: Some("uv".to_string()),
            user_semantic: Some("user(uv)".to_string()),
        }],
        render_targets: vec![RenderTarget {
            member_index: 0,
            location: 0,
            type_name: Some("float4".to_string()),
        }],
        depth_members: vec![1],
        depth_qualifier: None,
        stencil_members: vec![2],
        local_size: Some([8, 8, 1]),
        max_work_group_size: Some(256),
        kernel_dispatch: Some(KernelDispatch::ThreadsDynamic { offset: 16 }),
        fragment_sample_positions: None,
        vertex_builtins: Some(VertexBuiltins {
            uses_vertex_index: true,
            uses_instance_index: true,
            writes_position: true,
        }),
        tessellation: None,
        imageblock_layouts: vec![ImageblockLayout {
            param_index: 3,
            type_layout: AirType::Scalar(AirScalar::Float),
        }],
        implicit_imageblock_attachments: Vec::new(),
        fragment_imageblock: None,
        datalayout: Some("e-p:64:64-i64:64-n32:64".to_string()),
        runtime_sampler_specializations: vec![RuntimeSamplerSpecialization {
            metal_index: 0,
            state: RuntimeSamplerState {
                min_filter: SamplerFilter::Nearest,
                mag_filter: SamplerFilter::Nearest,
                mip_filter: SamplerMipFilter::None,
                address_mode_s: SamplerAddressMode::ClampToEdge,
                address_mode_t: SamplerAddressMode::ClampToEdge,
                address_mode_r: SamplerAddressMode::ClampToEdge,
                coordinates: SamplerCoordinates::Normalized,
                compare_function: SamplerCompareFunction::None,
                max_anisotropy: 1,
                lod_min_clamp: 0.0,
                lod_max_clamp: 65504.0,
                border_color: SamplerBorderColor::TransparentBlack,
                reduction: SamplerReduction::WeightedAverage,
                lod_bias: 0.0,
            },
        }],
        runtime_storage_image_specializations: vec![RuntimeStorageImageSpecialization {
            metal_index: 1,
            state: RuntimeStorageImageState {
                format: RuntimeStorageImageFormat::Rgba8Unorm,
                capabilities: RuntimeStorageImageCapabilities {
                    storage_image: true,
                    storage_image_atomic: false,
                    read_without_format: false,
                    write_without_format: false,
                },
            },
            spirv_format: Some(crate::meta::TextureFormat::Rgba8),
        }],
        function_constants: vec![FunctionConstant {
            index: 0,
            name: "myConst".to_string(),
            type_name: "i32".to_string(),
            abi_type_encoding: "i".to_string(),
        }],
    };
    let json = serde_json::to_string(&r).expect("serialize reflection");
    let back: ShaderReflection = serde_json::from_str(&json).expect("deserialize reflection");
    assert_eq!(r, back);
    assert_eq!(back.reflection_version, REFLECTION_VERSION);
}

#[test]
fn buffer_reflection_exports_extent_type_size_and_refined_access() {
    let ll = r#"
define { float } @f(ptr addrspace(2) readonly %object, ptr addrspace(1) readonly %array, ptr addrspace(1) %unused) {
entry:
  %a = load float, ptr addrspace(2) %object, align 4
  %b = load float, ptr addrspace(1) %array, align 4
  %sum = fadd float %a, %b
  %result = insertvalue { float } undef, float %sum, 0
  ret { float } %result
}

!air.fragment = !{!0}
!0 = !{ptr @f, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 16, !"air.arg_type_name", !"Params"}
!5 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_name", !"float"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"void"}
"#;
    let meta = parse_air_fragment_meta(ll).expect("fragment metadata");
    let mut reflection = ShaderReflection::from_fragment(&meta, Some("f"));
    reflection.refine_buffer_access_from_entry(ll);

    let object = reflection.binding_at(ResourceKind::Buffer, 0).unwrap();
    assert_eq!(object.declared_size, Some(16));
    assert_eq!(object.extent, Some(BufferExtent::Object { bytes: 16 }));
    assert_eq!(object.type_name.as_deref(), Some("Params"));
    assert_eq!(object.access, Some(ResourceAccess::ReadOnly));

    let array = reflection.binding_at(ResourceKind::Buffer, 1).unwrap();
    assert_eq!(array.declared_size, Some(4));
    assert_eq!(array.extent, Some(BufferExtent::Unbounded));
    assert_eq!(array.type_name.as_deref(), Some("float"));
    assert_eq!(array.access, Some(ResourceAccess::ReadOnly));

    let unused = reflection.binding_at(ResourceKind::Buffer, 2).unwrap();
    assert_eq!(unused.declared_size, None);
    assert_eq!(unused.extent, Some(BufferExtent::Unbounded));
    assert_eq!(unused.type_name.as_deref(), Some("void"));
    assert_eq!(unused.access, Some(ResourceAccess::Unused));
}

#[test]
fn abi_base_constants_are_the_contract() {
    assert_eq!(RESOURCE_DESCRIPTOR_SET, 0);
    assert_eq!(BUFFER_BINDING_BASE, 0);
    assert_eq!(TEXTURE_BINDING_BASE, 32);
    assert_eq!(SAMPLER_BINDING_BASE, 160);
    assert_eq!(COLOR_INPUT_BINDING_BASE, 192);
    assert_eq!(IMAGEBLOCK_BINDING_BASE, 200);
    assert_eq!(IMAGEBLOCK_DATA_RATE_STRIDE, 3);
    assert_eq!(FRAGMENT_IMAGEBLOCK_BINDING_BASE, 224);
    assert_eq!(STORAGE_TEXTURE_BINDING_BASE, 480);
    assert_eq!(SYNTHETIC_BINDING_BASE, 640);
    assert_eq!(TEXTURE_ARGUMENT_COUNT, 128);
    assert_eq!(TEXTURE_BINDING_RANGE.len(), Some(TEXTURE_ARGUMENT_COUNT));
    assert_eq!(
        STORAGE_TEXTURE_BINDING_RANGE.len(),
        Some(TEXTURE_ARGUMENT_COUNT)
    );

    assert_eq!(buffer_resource_binding(31), Some(31));
    assert_eq!(buffer_resource_binding(32), None);
    assert_eq!(texture_resource_binding(127), Some(159));
    assert_eq!(texture_resource_binding(128), None);
    assert_eq!(storage_texture_resource_binding(127), Some(607));
    assert_eq!(storage_texture_resource_binding(128), None);
    assert_eq!(sampler_resource_binding(15), Some(175));
    assert_eq!(sampler_resource_binding(16), None);
    assert_eq!(SAMPLER_BINDING_RANGE.binding(31), Some(191));
    assert_eq!(SAMPLER_BINDING_RANGE.binding(32), None);
    assert_eq!(color_input_resource_binding(7), Some(199));
    assert_eq!(color_input_resource_binding(8), None);
    assert_eq!(imageblock_resource_binding(7, 2), Some(223));
    assert_eq!(imageblock_resource_binding(8, 0), None);
    assert_eq!(imageblock_resource_binding(0, 3), None);
    assert_eq!(fragment_imageblock_resource_binding(255), Some(479));
    assert_eq!(fragment_imageblock_resource_binding(256), None);
}

#[test]
fn descriptor_layout_rejects_overlap_reversal_version_and_overflow_with_typed_errors() {
    let default = DescriptorLayout::default();
    let layout = DescriptorLayout {
        sampled_textures: DescriptorBindingRange {
            start: default.buffers.end - 1,
            ..default.sampled_textures
        },
        ..default
    };
    assert!(matches!(
        layout.validate(),
        Err(DescriptorLayoutError::OverlappingRanges { .. })
    ));

    let layout = DescriptorLayout {
        buffers: DescriptorBindingRange { start: 4, end: 3 },
        ..Default::default()
    };
    assert!(matches!(
        layout.validate(),
        Err(DescriptorLayoutError::ReversedRange {
            class: "buffers",
            ..
        })
    ));

    let layout = DescriptorLayout {
        version: DESCRIPTOR_LAYOUT_VERSION + 1,
        ..Default::default()
    };
    assert!(matches!(
        layout.validate(),
        Err(DescriptorLayoutError::UnsupportedVersion { .. })
    ));
    assert_eq!(
        DescriptorBindingRange::from_base_count(u32::MAX, 1),
        Err(DescriptorLayoutError::RangeOverflow {
            base: u32::MAX,
            count: 1,
        })
    );
}

#[test]
fn descriptor_abi_distinguishes_images_from_texel_buffers() {
    for (ordinary, texel) in [
        ("texture2d<float, sample>", "texture_buffer<float, sample>"),
        ("texture2d<float, write>", "texture_buffer<float, write>"),
    ] {
        let mut meta = KernMeta {
            roles: vec![(0, KernRole::Texture(0)), (1, KernRole::Texture(0))],
            ..Default::default()
        };
        meta.texture_type_names.insert(0, ordinary.to_string());
        meta.texture_type_names.insert(1, texel.to_string());
        let reflection = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);

        let error = reflection
            .validate_descriptor_abi()
            .expect_err("image and texel-buffer descriptors cannot alias");
        assert!(error.contains("shared incompatibly"), "{error}");
    }
}

#[test]
fn function_tables_are_reflected_as_descriptor_free_link_resources() {
    let meta = KernMeta {
        roles: vec![
            (0, KernRole::VisibleFunctionTable(7)),
            (1, KernRole::IntersectionFunctionTable(9)),
        ],
        ..Default::default()
    };
    let reflection = ShaderReflection::from_kernel(&meta, Some("k"), [1, 1, 1]);
    assert!(reflection.bindings.iter().any(|binding| {
        binding.kind == ResourceKind::VisibleFunctionTable
            && binding.metal_index == 7
            && binding.descriptor.is_none()
    }));
    assert!(reflection.bindings.iter().any(|binding| {
        binding.kind == ResourceKind::IntersectionFunctionTable
            && binding.metal_index == 9
            && binding.descriptor.is_none()
    }));
}

#[test]
fn runtime_storage_formats_have_complete_component_and_spirv_mappings() {
    use crate::meta::{TextureComponent as C, TextureFormat as F};
    use RuntimeStorageImageFormat as R;

    for (runtime, component, explicit) in [
        (R::R8Unorm, C::Float, Some(F::R8)),
        (R::Rgba8Unorm, C::Float, Some(F::Rgba8)),
        (R::Bgra8Unorm, C::Float, None),
        (R::R16Float, C::Float, Some(F::R16f)),
        (R::Rg16Float, C::Float, Some(F::Rg16f)),
        (R::Rg32Float, C::Float, Some(F::Rg32f)),
        (R::Rgba16Float, C::Float, Some(F::Rgba16f)),
        (R::R32Float, C::Float, Some(F::R32f)),
        (R::Rgba32Float, C::Float, Some(F::Rgba32f)),
        (R::R16Uint, C::Uint, Some(F::R16ui)),
        (R::R32Uint, C::Uint, Some(F::R32ui)),
        (R::Rgba8Uint, C::Uint, Some(F::Rgba8ui)),
        (R::Rgba16Uint, C::Uint, Some(F::Rgba16ui)),
        (R::Rgba32Uint, C::Uint, Some(F::Rgba32ui)),
        (R::R32Sint, C::Sint, Some(F::R32i)),
        (R::Rgba8Sint, C::Sint, Some(F::Rgba8i)),
        (R::Rgba32Sint, C::Sint, Some(F::Rgba32i)),
    ] {
        assert_eq!(runtime.component(), component, "{runtime:?}");
        assert_eq!(runtime.explicit_format(), explicit, "{runtime:?}");
    }
    assert!(R::R32Uint.supports_atomics());
    assert!(R::R32Sint.supports_atomics());
    assert!(!R::Rgba32Uint.supports_atomics());
}

#[test]
fn every_static_sampler_bit_is_either_decoded_or_deliberately_ignored() {
    let decoded = 2
        | (3 << 3)
        | (1 << 6)
        | (1 << 9)
        | (2 << 11)
        | (2 << 13)
        | (1 << 15)
        | (3 << 16)
        | (4 << 20)
        | (0x4080 << 24)
        | (0x4200 << 40)
        | (1 << 56)
        | (2 << 58);
    let expected = StaticSamplerState {
        address_mode_s: SamplerAddressMode::Repeat,
        address_mode_t: SamplerAddressMode::MirroredRepeat,
        address_mode_r: SamplerAddressMode::ClampToEdge,
        mag_filter: SamplerFilter::Linear,
        min_filter: SamplerFilter::Bicubic,
        mip_filter: SamplerMipFilter::Linear,
        coordinates: SamplerCoordinates::Pixel,
        compare_function: SamplerCompareFunction::Greater,
        max_anisotropy: 5,
        lod_min_clamp: 2.25,
        lod_max_clamp: 3.0,
        border_color: SamplerBorderColor::OpaqueBlack,
        reduction: SamplerReduction::Maximum,
        lod_bias: 1.0,
        raw_words: [decoded, 0x3c00],
    };
    assert_eq!(
        StaticSamplerState::from_air_words([decoded, 0x3c00]).expect("decode"),
        expected,
    );

    let ignored_word0 = 0xf << 60;
    let ignored_word1 = !0xffff_u64;
    let mut with_ignored = expected;
    with_ignored.raw_words = [decoded | ignored_word0, 0x3c00 | ignored_word1];
    assert_eq!(
        StaticSamplerState::from_air_words(with_ignored.raw_words).expect("decode"),
        with_ignored,
    );

    for (field, shift, width, code) in [
        ("address_mode_s", 0, 0x7_u64, 4_u64),
        ("address_mode_t", 3, 0x7, 5),
        ("address_mode_r", 6, 0x7, 7),
        ("mag_filter", 9, 0x3, 3),
        ("min_filter", 11, 0x3, 3),
        ("mip_filter", 13, 0x3, 3),
        ("compare_function", 16, 0xf, 9),
        ("border_color", 56, 0x3, 3),
        ("reduction", 58, 0x3, 3),
    ] {
        let word = (decoded & !(width << shift)) | (code << shift);
        assert!(
            StaticSamplerState::from_air_words([word, 0x3c00])
                .unwrap_err()
                .contains("unsupported"),
            "an unsupported {field} code should not be decoded into a guess",
        );
    }
}

#[test]
fn every_metal_constexpr_sampler_option_decodes_to_what_the_frontend_emits() {
    let default_state = StaticSamplerState {
        address_mode_s: SamplerAddressMode::ClampToEdge,
        address_mode_t: SamplerAddressMode::ClampToEdge,
        address_mode_r: SamplerAddressMode::ClampToEdge,
        mag_filter: SamplerFilter::Nearest,
        min_filter: SamplerFilter::Nearest,
        mip_filter: SamplerMipFilter::None,
        coordinates: SamplerCoordinates::Normalized,
        compare_function: SamplerCompareFunction::Never,
        max_anisotropy: 1,
        lod_min_clamp: 0.0,
        lod_max_clamp: 65504.0,
        border_color: SamplerBorderColor::TransparentBlack,
        reduction: SamplerReduction::WeightedAverage,
        lod_bias: 0.0,
        raw_words: [0x007b_ff00_0008_0049, 0],
    };
    let rows: &[(&str, u64, u64, fn(&mut StaticSamplerState))] = &[
        ("", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("coord::normalized", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("coord::pixel", 0x007b_ff00_0008_8049, 0, |s| {
            s.coordinates = SamplerCoordinates::Pixel;
        }),
        ("filter::nearest", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("filter::linear", 0x007b_ff00_0008_0a49, 0, |s| {
            s.mag_filter = SamplerFilter::Linear;
            s.min_filter = SamplerFilter::Linear;
        }),
        ("filter::bicubic", 0x007b_ff00_0008_1449, 0, |s| {
            s.mag_filter = SamplerFilter::Bicubic;
            s.min_filter = SamplerFilter::Bicubic;
        }),
        (
            "mag_filter::linear, min_filter::nearest",
            0x007b_ff00_0008_0249,
            0,
            |s| s.mag_filter = SamplerFilter::Linear,
        ),
        ("address::clamp_to_edge", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("address::clamp_to_zero", 0x007b_ff00_0008_0000, 0, |s| {
            s.address_mode_s = SamplerAddressMode::ClampToZero;
            s.address_mode_t = SamplerAddressMode::ClampToZero;
            s.address_mode_r = SamplerAddressMode::ClampToZero;
        }),
        ("address::clamp_to_border", 0x007b_ff00_0008_0000, 0, |s| {
            s.address_mode_s = SamplerAddressMode::ClampToZero;
            s.address_mode_t = SamplerAddressMode::ClampToZero;
            s.address_mode_r = SamplerAddressMode::ClampToZero;
        }),
        ("address::repeat", 0x007b_ff00_0008_0092, 0, |s| {
            s.address_mode_s = SamplerAddressMode::Repeat;
            s.address_mode_t = SamplerAddressMode::Repeat;
            s.address_mode_r = SamplerAddressMode::Repeat;
        }),
        ("address::mirrored_repeat", 0x007b_ff00_0008_00db, 0, |s| {
            s.address_mode_s = SamplerAddressMode::MirroredRepeat;
            s.address_mode_t = SamplerAddressMode::MirroredRepeat;
            s.address_mode_r = SamplerAddressMode::MirroredRepeat;
        }),
        (
            "address::clamp_to_border, border_color::opaque_black",
            0x017b_ff00_0008_0000,
            0,
            |s| {
                s.address_mode_s = SamplerAddressMode::ClampToBorder;
                s.address_mode_t = SamplerAddressMode::ClampToBorder;
                s.address_mode_r = SamplerAddressMode::ClampToBorder;
                s.border_color = SamplerBorderColor::OpaqueBlack;
            },
        ),
        (
            "address::clamp_to_border, border_color::opaque_white",
            0x027b_ff00_0008_0000,
            0,
            |s| {
                s.address_mode_s = SamplerAddressMode::ClampToBorder;
                s.address_mode_t = SamplerAddressMode::ClampToBorder;
                s.address_mode_r = SamplerAddressMode::ClampToBorder;
                s.border_color = SamplerBorderColor::OpaqueWhite;
            },
        ),
        (
            "s_address::repeat, t_address::mirrored_repeat, r_address::clamp_to_zero",
            0x007b_ff00_0008_001a,
            0,
            |s| {
                s.address_mode_s = SamplerAddressMode::Repeat;
                s.address_mode_t = SamplerAddressMode::MirroredRepeat;
                s.address_mode_r = SamplerAddressMode::ClampToZero;
            },
        ),
        ("mip_filter::none", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("mip_filter::nearest", 0x007b_ff00_0008_2049, 0, |s| {
            s.mip_filter = SamplerMipFilter::Nearest;
        }),
        ("mip_filter::linear", 0x007b_ff00_0008_4049, 0, |s| {
            s.mip_filter = SamplerMipFilter::Linear;
        }),
        ("max_anisotropy(5)", 0x007b_ff00_0048_0049, 0, |s| {
            s.max_anisotropy = 5;
        }),
        ("lod_clamp(2.0f, 3.0f)", 0x0042_0040_0008_0049, 0, |s| {
            s.lod_min_clamp = 2.0;
            s.lod_max_clamp = 3.0;
        }),
        ("lod_clamp(3.7f, 4.0f)", 0x0044_0043_6608_0049, 0, |s| {
            s.lod_min_clamp = 947.0 / 256.0;
            s.lod_max_clamp = 4.0;
        }),
        ("lod_clamp(1.1f, 2.2f)", 0x0040_663c_6608_0049, 0, |s| {
            s.lod_min_clamp = 563.0 / 512.0;
            s.lod_max_clamp = 563.0 / 256.0;
        }),
        ("compare_func::less", 0x007b_ff00_0001_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::Less;
        }),
        ("compare_func::less_equal", 0x007b_ff00_0002_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::LessEqual;
        }),
        ("compare_func::greater", 0x007b_ff00_0003_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::Greater;
        }),
        (
            "compare_func::greater_equal",
            0x007b_ff00_0004_0049,
            0,
            |s| {
                s.compare_function = SamplerCompareFunction::GreaterEqual;
            },
        ),
        ("compare_func::equal", 0x007b_ff00_0005_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::Equal;
        }),
        ("compare_func::not_equal", 0x007b_ff00_0006_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::NotEqual;
        }),
        ("compare_func::always", 0x007b_ff00_0007_0049, 0, |s| {
            s.compare_function = SamplerCompareFunction::Always;
        }),
        ("compare_func::never", 0x007b_ff00_0008_0049, 0, |_| {}),
        ("max_anisotropy(16)", 0x007b_ff00_00f8_0049, 0, |s| {
            s.max_anisotropy = 16
        }),
        ("lod_clamp(0.0f, 0.0f)", 0x0000_0000_0008_0049, 0, |s| {
            s.lod_max_clamp = 0.0
        }),
        ("bias(2.0f)", 0x007b_ff00_0008_0049, 0x4000, |s| {
            s.lod_bias = 2.0
        }),
        ("bias(-0.5f)", 0x007b_ff00_0008_0049, 0xb800, |s| {
            s.lod_bias = -0.5
        }),
    ];
    for (options, word, second, apply) in rows {
        let mut expected = default_state;
        apply(&mut expected);
        expected.raw_words = [*word, *second];
        assert_eq!(
            StaticSamplerState::from_air_words([*word, *second]).expect("decode"),
            expected,
            "constexpr sampler s({options}) emits {word:#018x}, {second:#018x}",
        );
    }
}

#[test]
fn the_default_layout_reapplied_is_the_identity_on_every_binding() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6, !7}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1}
!4 = !{i32 1, !"air.texture", !"air.location_index", i32 5, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!5 = !{i32 2, !"air.sampler", !"air.location_index", i32 3, i32 1}
!6 = !{i32 3, !"air.texture", !"air.location_index", i32 9, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>"}
!7 = !{i32 4, !"air.stage_in", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"float3"}
"#;
    let meta = crate::meta::parse_air_kernel_meta(ll).expect("parse kernel meta");
    let mut reflection = ShaderReflection::from_kernel(&meta, None, [1, 1, 1]);
    let before = reflection.bindings.clone();
    let mut kinds = before
        .iter()
        .filter(|binding| binding.descriptor.is_some())
        .map(|binding| format!("{:?}", binding.kind))
        .collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            "Buffer",
            "KernelStageInput",
            "Sampler",
            "StorageImage",
            "Texture"
        ],
        "one descriptor band per kind the default helpers assign: {before:?}"
    );
    reflection
        .apply_descriptor_layout(DescriptorLayout::default())
        .expect("reapply the default layout");
    assert_eq!(
        reflection.bindings, before,
        "the default layout's answer and the default helpers' answer are the same answer"
    );
}

#[test]
fn render_stage_acceleration_structure_shadow_reflects_at_the_buffer_descriptor() {
    let descriptor = |b: &ResourceBinding| b.descriptor.as_ref().map(|d| (d.set, d.binding));

    let frag = crate::meta::FragMeta {
        roles: vec![
            (0, FragRole::Buffer(0)),
            (4, FragRole::AccelerationStructureShadow(3)),
        ],
        ..Default::default()
    };
    let rf = ShaderReflection::from_fragment(&frag, Some("f"));
    let shadow = rf
        .binding_at(ResourceKind::AccelerationStructureShadow, 3)
        .expect("fragment shadow at buffer 3");
    assert_eq!(shadow.param_index, Some(4));
    let plain = crate::meta::FragMeta {
        roles: vec![(0, FragRole::Buffer(3))],
        ..Default::default()
    };
    let rp = ShaderReflection::from_fragment(&plain, Some("f"));
    let buffer3 = rp
        .binding_at(ResourceKind::Buffer, 3)
        .expect("fragment buffer 3");
    assert!(
        descriptor(shadow).is_some(),
        "the shadow consumes a descriptor"
    );
    assert_eq!(
        descriptor(shadow),
        descriptor(buffer3),
        "the shadow takes buffer(3)'s descriptor"
    );

    let vert = crate::meta::VertMeta {
        roles: vec![
            (0, VertRole::VertexId),
            (1, VertRole::AccelerationStructureShadow(0)),
        ],
        ..Default::default()
    };
    let rv = ShaderReflection::from_vertex(&vert, Some("v"));
    let vshadow = rv
        .binding_at(ResourceKind::AccelerationStructureShadow, 0)
        .expect("vertex shadow at buffer 0");
    assert_eq!(vshadow.param_index, Some(1));
    let vplain = crate::meta::VertMeta {
        roles: vec![(0, VertRole::Buffer(0))],
        ..Default::default()
    };
    let rvp = ShaderReflection::from_vertex(&vplain, Some("v"));
    let vbuffer0 = rvp
        .binding_at(ResourceKind::Buffer, 0)
        .expect("vertex buffer 0");
    assert!(
        descriptor(vshadow).is_some(),
        "the shadow consumes a descriptor"
    );
    assert_eq!(
        descriptor(vshadow),
        descriptor(vbuffer0),
        "the shadow takes buffer(0)'s descriptor"
    );
}
