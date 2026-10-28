use super::*;
use AirScalar::*;

const FRAG_LL: &str = r#"
!air.fragment = !{!15}
!15 = !{ptr @F, !16, !18}
!16 = !{!17}
!17 = !{!"air.render_target", i32 0, i32 0}
!18 = !{!19, !20, !21, !22, !23, !24}
!19 = !{i32 0, !"air.position", !"air.center"}
!20 = !{i32 1, !"air.fragment_input", !"generated", !"air.arg_type_name", !"float2", !"air.arg_name", !"texCoord"}
!21 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"texture2d<float, sample>"}
!22 = !{i32 3, !"air.buffer", !"air.buffer_size", i32 32, !"air.location_index", i32 5, i32 1}
!23 = !{i32 4, !"air.point_coord", !"air.arg_type_name", !"float2", !"air.arg_name", !"pointCoord"}
!24 = !{i32 5, !"air.front_facing", !"air.arg_type_name", !"bool", !"air.arg_name", !"front"}
"#;

#[test]
fn fragment_roles() {
    let m = parse_air_fragment_meta(FRAG_LL).unwrap();
    assert_eq!(m.n_render_targets, 1);
    assert_eq!(m.render_target_indices, vec![0]);
    assert_eq!(m.role_of(0), Some(&FragRole::Position));
    assert_eq!(m.role_of(1), Some(&FragRole::Varying(0)));
    assert_eq!(m.role_of(2), Some(&FragRole::Texture(0)));
    assert_eq!(m.role_of(3), Some(&FragRole::Buffer(5)));
    assert_eq!(m.role_of(4), Some(&FragRole::PointCoord));
    assert_eq!(m.role_of(5), Some(&FragRole::FrontFacing));
    assert_eq!(m.texture_type_name(2), Some("texture2d<float, sample>"));
    assert_eq!(m.varying_type(0), Some("float2"));
    assert_eq!(m.varying_name(0), Some("texCoord"));
    assert_eq!(m.varying_user_semantic(0), Some("generated"));
    assert!(!m.varying_is_flat(0));
}

#[test]
fn every_interpolation_marker_is_decoded_or_deliberately_ignored() {
    let with = |markers: &str| {
        let ll = FRAG_LL.replace(
            r#"!"air.fragment_input", !"generated","#,
            &format!(r#"!"air.fragment_input", !"generated", {markers}"#),
        );
        parse_air_fragment_meta(&ll)
            .unwrap()
            .varying_interpolation(0)
    };

    assert_eq!(
        with(r#"!"air.center", !"air.perspective","#),
        VaryingInterpolation::default(),
        "AIR's defaults are Vulkan's defaults and decode to no decoration"
    );
    assert!(with(r#"!"air.center", !"air.no_perspective","#).no_perspective);
    assert!(!with(r#"!"air.center", !"air.perspective","#).no_perspective);

    assert_eq!(
        with(r#"!"air.center", !"air.perspective","#).sampling,
        VaryingSampling::Center
    );
    assert_eq!(
        with(r#"!"air.centroid", !"air.perspective","#).sampling,
        VaryingSampling::Centroid
    );
    assert_eq!(
        with(r#"!"air.sample", !"air.no_perspective","#).sampling,
        VaryingSampling::Sample
    );

    assert!(with(r#"!"air.flat","#).flat);
    assert!(!with(r#"!"air.center", !"air.perspective","#).flat);

    assert_eq!(with(""), VaryingInterpolation::default());
}

#[test]
fn the_two_sample_mask_directions_are_distinct_roles() {
    let ll = FRAG_LL
        .replace(
            r#"!20 = !{i32 1, !"air.fragment_input", !"generated","#,
            r#"!20 = !{i32 1, !"air.sample_mask_in","#,
        )
        .replace(
            r#"!17 = !{!"air.render_target", i32 0, i32 0}"#,
            r#"!17 = !{!"air.sample_mask", !"air.arg_type_name", !"uint"}"#,
        );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.role_of(1), Some(&FragRole::SampleMaskIn));
    assert!(
        m.is_sample_mask_member(0),
        "and the return member is the output"
    );
    assert!(
        m.unmodelled_input_params.is_empty(),
        "`air.sample_mask_in` has a lowering"
    );
}

#[test]
fn a_disabled_builtin_argument_declares_no_input_variable() {
    for role in ["sample_mask_in", "barycentric_coord"] {
        let ll = FRAG_LL.replace(
            r#"!20 = !{i32 1, !"air.fragment_input", !"generated","#,
            &format!(
                r#"!20 = !{{i32 1, !"air.function_constant", !97, !"air.{role}", !"air.center", !"air.perspective","#
            ),
        ) + "\n!97 = !{ptr addrspace(2) @off.MTL_FC_INIT_0_b, !\"bool\", !\"off\", i32 0, i1 false}\n";
        let m = parse_air_fragment_meta(&ll).unwrap();
        assert_eq!(
            m.role_of(1),
            Some(&FragRole::Other),
            "an off-by-default `air.{role}` argument must not claim its builtin"
        );
    }
}

#[test]
fn a_barycentric_argument_keeps_its_perspective_axis() {
    let with = |markers: &str| {
        let ll = FRAG_LL.replace(
            r#"!20 = !{i32 1, !"air.fragment_input", !"generated","#,
            &format!(r#"!20 = !{{i32 1, !"air.barycentric_coord", {markers}"#),
        );
        let m = parse_air_fragment_meta(&ll).unwrap();
        m.role_of(1).expect("decoded role").clone()
    };
    assert_eq!(
        with(r#"!"air.center", !"air.perspective","#),
        FragRole::BarycentricCoord {
            no_perspective: false
        }
    );
    assert_eq!(
        with(r#"!"air.center", !"air.no_perspective","#),
        FragRole::BarycentricCoord {
            no_perspective: true
        }
    );
    assert_eq!(
        with(r#"!"air.centroid", !"air.perspective","#),
        FragRole::BarycentricCoord {
            no_perspective: false
        },
        "SPIR-V has no centroid barycentric builtin, so the sampling axis is deliberately ignored"
    );
}

#[test]
fn the_sample_marker_is_read_per_argument_not_per_module() {
    let ll = FRAG_LL
        .replace(
            r#"!"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"texture2d<float, sample>""#,
            r#"!"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>""#,
        )
        .replace(
            r#"!"air.fragment_input", !"generated","#,
            r#"!"air.fragment_input", !"generated", !"air.center", !"air.perspective","#,
        );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.role_of(2), Some(&FragRole::Texture(0)));
    assert_eq!(
        m.varying_interpolation(0).sampling,
        VaryingSampling::Center,
        "the texture argument's `air.sample` access qualifier is not the varying's sampling rate"
    );
}

#[test]
fn fragment_flat_varying_metadata() {
    let ll = FRAG_LL.replace(
        r#"!20 = !{i32 1, !"air.fragment_input", !"generated", !"air.arg_type_name", !"float2", !"air.arg_name", !"texCoord"}"#,
        r#"!20 = !{i32 1, !"air.fragment_input", !"generated", !"air.flat", !"air.arg_type_name", !"float2", !"air.arg_name", !"texCoord"}"#,
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert!(m.varying_is_flat(0));
}

#[test]
fn fragment_render_target_array_index_role() {
    let ll = r#"
!air.fragment = !{!0}
!0 = !{ptr @F, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0}
!3 = !{!4}
!4 = !{i32 0, !"air.render_target_array_index", !"air.arg_type_name", !"ushort", !"air.arg_name", !"layer"}
"#;
    let meta = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(meta.role_of(0), Some(&FragRole::RenderTargetArrayIndex));
}

#[test]
fn fragment_function_constant_location_uses_static_init_default() {
    let ll = r#"
@_ZL32__metal_implicit_attr_int_expr_1.78 = internal addrspace(2) global i32 0, align 4
@__metal_implicit_fc_pred_1.80 = internal addrspace(2) global i8 1, align 1
@_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j = internal unnamed_addr addrspace(2) externally_initialized constant <4 x i32> undef, section "air.fc_initializer", align 16

define internal void @_GLOBAL__sub_I_shader_filter_distance.metal() section "air.static_init" {
entry:
  %1 = load <4 x i32>, ptr addrspace(2) @_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j
  %2 = extractelement <4 x i32> %1, i64 0
  %12 = and i32 %2, 131072
  %13 = icmp eq i32 %12, 0
  %14 = select i1 %13, i32 4, i32 6
  store i32 %14, ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_1.78
  ret void
}

!air.fragment = !{!0}
!0 = !{ptr @F, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0}
!3 = !{!4}
!4 = !{i32 0, !"air.function_constant", !5, !"air.texture", !"air.location_index", ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_1.78, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<half, read>", !"air.arg_name", !"dest_tex"}
!5 = !{ptr addrspace(2) @__metal_implicit_fc_pred_1.80, !"bool", !"uses_dest"}
"#;
    let m = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&FragRole::Texture(4)));
}

#[test]
fn each_threadgroup_size_role_has_its_own_lowering() {
    let ll = |role: &str| {
        format!(
            r#"
define void @K(i32 %size) {{
  ret void
}}

!air.kernel = !{{!0}}
!0 = !{{ptr @K, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.{role}", !"air.arg_type_name", !"uint", !"air.arg_name", !"size"}}
"#
        )
    };
    for (role, expected) in [
        ("threads_per_threadgroup", KernRole::ThreadsPerThreadgroup),
        (
            "dispatch_threads_per_threadgroup",
            KernRole::DispatchThreadsPerThreadgroup,
        ),
    ] {
        let meta = parse_air_kernel_meta(&ll(role)).expect("kernel metadata");
        assert_eq!(
            meta.role_of(0),
            Some(&expected),
            "`air.{role}` decodes as its own threadgroup-size role"
        );
        assert!(
            meta.unmodelled_input_params.is_empty(),
            "`air.{role}` has a lowering, so it must not be reported as unmodelled"
        );
    }
}

#[test]
fn kernel_function_constant_texture_with_a_literal_location_is_bound_when_disabled_by_default() {
    let ll = r#"
@texture_enabled = internal addrspace(2) global i8 0, align 1

define void @K(ptr addrspace(1) %texture, ptr addrspace(2) %sampler) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @K, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.function_constant", !5, !"air.texture", !"air.location_index", i32 40, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 15, i32 1, !"air.arg_type_name", !"sampler"}
!5 = !{ptr addrspace(2) @texture_enabled, !"bool", !"texture_enabled"}
"#;
    let meta = parse_air_kernel_meta(ll).expect("kernel metadata");
    assert_eq!(meta.role_of(0), Some(&KernRole::Texture(40)));
    assert_eq!(meta.role_of(1), Some(&KernRole::Sampler(15)));
}

#[test]
fn kernel_function_constant_texture_summed_onto_a_live_texture_slot_is_not_bound() {
    let ll = r#"
@live_enabled = internal addrspace(2) global i8 1, align 1
@gated_enabled = internal addrspace(2) global i8 0, align 1
@live_location = internal addrspace(2) global i32 0, align 4
@gated_location = internal addrspace(2) global i32 0, align 4

define internal void @_GLOBAL__sub_I_alternatives.metal() #0 section "air.static_init" {
  store i32 3, ptr addrspace(2) @live_location, align 4
  store i32 3, ptr addrspace(2) @gated_location, align 4
  ret void
}

define void @K(ptr addrspace(1) %live, ptr addrspace(1) %gated) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @K, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.function_constant", !5, !"air.texture", !"air.location_index", ptr addrspace(2) @live_location, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!4 = !{i32 1, !"air.function_constant", !6, !"air.texture", !"air.location_index", ptr addrspace(2) @gated_location, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d_array<float, sample>"}
!5 = !{ptr addrspace(2) @live_enabled, !"bool", !"live_enabled"}
!6 = !{ptr addrspace(2) @gated_enabled, !"bool", !"gated_enabled"}
"#;
    let meta = parse_air_kernel_meta(ll).expect("kernel metadata");
    assert_eq!(meta.role_of(0), Some(&KernRole::Texture(3)));
    assert_eq!(meta.role_of(1), Some(&KernRole::VariantAbsentTexture));
}

#[test]
fn function_constant_texture_with_absent_location_is_not_bound() {
    let ll = r#"
@texture_location = internal addrspace(2) global i32 -1, align 4
@texture_enabled = internal addrspace(2) global i8 0, align 1

define void @K(ptr addrspace(1) %texture) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @K, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.function_constant", !4, !"air.texture", !"air.location_index", ptr addrspace(2) @texture_location, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>"}
!4 = !{ptr addrspace(2) @texture_enabled, !"bool", !"texture_enabled"}
"#;

    let meta = parse_air_kernel_meta(ll).expect("kernel metadata");
    assert_eq!(meta.role_of(0), Some(&KernRole::VariantAbsentTexture));
}

#[test]
fn kernel_function_constant_buffer_locations_preserve_shared_bindings() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @K, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.function_constant", !6, !"air.buffer", !"air.location_index", i32 29}
!4 = !{i32 1, !"air.function_constant", !7, !"air.buffer", !"air.location_index", i32 29}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 30}
!6 = !{ptr addrspace(2) @float_enabled, !"bool", !"float_enabled"}
!7 = !{ptr addrspace(2) @half_enabled, !"bool", !"half_enabled"}
"#;

    let meta = parse_air_kernel_meta(ll).expect("kernel metadata");
    assert_eq!(
        meta.function_constant_buffer_locations,
        crate::fxhash::map_from([(0, 29), (1, 29)])
    );
}

#[test]
fn device_buffer_array_uses_literal_base_instead_of_static_next_location() {
    let ll = r#"
@next_buffer = internal addrspace(2) global i32 1, align 4

!air.kernel = !{!0}
!0 = !{ptr @K, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, ptr addrspace(2) @next_buffer, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_name", !"array_ref<void>", !"air.arg_name", !"buffers"}
"#;
    let meta = parse_air_kernel_meta(ll).expect("kernel metadata");
    assert_eq!(meta.role_of(0), Some(&KernRole::Buffer(0)));
    assert_eq!(meta.buffer_type_name(0), Some("array_ref<void>"));
}

#[test]
fn fragment_function_constant_render_target_disabled_by_default() {
    let ll = r#"
!air.fragment = !{!0}
!0 = !{ptr @F, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!3 = !{i32 7, !"air.function_constant", !5, !"air.render_target", i32 1, i32 0, !"air.arg_type_name", !"half2"}
!4 = !{}
!5 = !{ptr addrspace(2) @__metal_implicit_fc_pred_1, !"bool", !"uses_coverage"}
"#;
    let m = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(m.n_render_targets, 1);
    assert_eq!(m.render_target_members, vec![(0, 0)]);
    assert_eq!(m.render_target_type_name(1), None);
}

#[test]
fn fragment_function_constant_render_target_static_true_is_recognized() {
    let ll = r#"
@__metal_implicit_fc_pred_1 = internal unnamed_addr addrspace(2) global i8 1, align 1
!air.fragment = !{!0}
!0 = !{ptr @F, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!3 = !{i32 7, !"air.function_constant", !5, !"air.render_target", i32 1, i32 0, !"air.arg_type_name", !"half2"}
!4 = !{}
!5 = !{ptr addrspace(2) @__metal_implicit_fc_pred_1, !"bool", !"uses_coverage"}
"#;
    let m = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(m.n_render_targets, 2);
    assert_eq!(m.render_target_members, vec![(0, 0), (1, 1)]);
    assert_eq!(m.render_target_type_name(1), Some("half2"));
}

#[test]
fn fragment_color_input_uses_render_target_location() {
    let ll = r#"
!air.fragment = !{!0}
!0 = !{ptr @F, !1, !4}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!4 = !{!5}
!5 = !{i32 2, !"air.render_target", i32 2, !"air.arg_type_name", !"float", !"air.arg_name", !"d0"}
"#;
    let m = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(m.role_of(2), Some(&FragRole::ColorInput(2)));
    assert_eq!(m.color_input_type_name(2), Some("float"));
}

#[test]
fn fragment_render_target_location_uses_static_init_default() {
    let ll = r#"
@_ZL32__metal_implicit_attr_int_expr_5 = internal addrspace(2) global i32 0, align 4
@_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j = internal unnamed_addr addrspace(2) externally_initialized constant <4 x i32> undef, section "air.fc_initializer", align 16

define internal void @_GLOBAL__sub_I_shader.metal() section "air.static_init" {
entry:
  %1 = load <4 x i32>, ptr addrspace(2) @_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j
  %2 = extractelement <4 x i32> %1, i64 0
  %3 = and i32 %2, 131072
  %4 = icmp eq i32 %3, 0
  %5 = select i1 %4, i32 4, i32 6
  store i32 %5, ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_5
  ret void
}

!air.fragment = !{!0}
!0 = !{ptr @F, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!3 = !{!"air.render_target", ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_5, !"air.arg_type_name", !"half4"}
!4 = !{}
"#;
    let m = parse_air_fragment_meta(ll).unwrap();
    assert_eq!(m.render_target_members, vec![(0, 0), (1, 4)]);
    assert_eq!(m.render_target_indices, vec![0, 4]);
}

#[test]
fn fragment_render_target_indices() {
    let ll = FRAG_LL.replace(
        "!17 = !{!\"air.render_target\", i32 0, i32 0}",
        "!17 = !{!\"air.render_target\", i32 3, i32 0}",
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.n_render_targets, 1);
    assert_eq!(m.render_target_members, vec![(0, 3)]);
    assert_eq!(m.render_target_indices, vec![3]);
}

#[test]
fn fragment_render_target_type_names() {
    let ll = FRAG_LL.replace(
        "!17 = !{!\"air.render_target\", i32 0, i32 0}",
        "!17 = !{!\"air.render_target\", i32 0, i32 0, !\"air.arg_type_name\", !\"int4\"}",
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.render_target_type_name(0), Some("int4"));
}

#[test]
fn fragment_stencil_output_is_not_a_render_target() {
    let ll = FRAG_LL.replace(
        "!17 = !{!\"air.render_target\", i32 0, i32 0}",
        "!17 = !{!\"air.stencil\", !\"air.arg_type_name\", !\"uint\", !\"air.arg_name\", !\"stencil\"}",
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.n_render_targets, 0);
    assert!(m.render_target_members.is_empty());
    assert!(m.render_target_indices.is_empty());
    assert_eq!(m.stencil_members, vec![0]);
    assert!(m.is_stencil_member(0));
    assert!(!m.is_stencil_member(1));
}

#[test]
fn fragment_depth_output_is_not_a_render_target() {
    let ll = FRAG_LL.replace(
        "!17 = !{!\"air.render_target\", i32 0, i32 0}",
        "!17 = !{!\"air.depth\", !\"air.depth_qualifier\", !\"air.any\", !\"air.arg_type_name\", !\"float\", !\"air.arg_name\", !\"depth\"}",
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(m.n_render_targets, 0);
    assert!(m.render_target_members.is_empty());
    assert_eq!(m.depth_members, vec![0]);
    assert_eq!(m.depth_qualifier, Some(DepthQualifier::Any));
    let reflection = crate::reflect::ShaderReflection::from_fragment(&m, Some("F"));
    assert_eq!(reflection.depth_qualifier, Some(DepthQualifier::Any));
    assert!(m.is_depth_member(0));
    assert!(!m.is_depth_member(1));
}

#[test]
fn each_non_color_fragment_output_role_is_recognised() {
    let with = |node: &str| {
        let ll = FRAG_LL.replace(r#"!17 = !{!"air.render_target", i32 0, i32 0}"#, node);
        parse_air_fragment_meta(&ll).unwrap()
    };

    let depth = with(r#"!17 = !{!"air.depth", !"air.arg_type_name", !"float"}"#);
    assert!(depth.is_depth_member(0));
    assert!(!depth.is_stencil_member(0) && !depth.is_sample_mask_member(0));

    let stencil = with(r#"!17 = !{!"air.stencil", !"air.arg_type_name", !"uint"}"#);
    assert!(stencil.is_stencil_member(0));
    assert!(!stencil.is_depth_member(0) && !stencil.is_sample_mask_member(0));

    let mask = with(r#"!17 = !{!"air.sample_mask", !"air.arg_type_name", !"uint"}"#);
    assert!(mask.is_sample_mask_member(0));
    assert!(!mask.is_depth_member(0) && !mask.is_stencil_member(0));
    assert_eq!(
        mask.n_render_targets, 0,
        "a coverage mask is not a color attachment"
    );
    assert!(mask.render_target_members.is_empty());
}

#[test]
fn a_disabled_sample_mask_output_is_not_reported() {
    let ll = FRAG_LL.replace(
        r#"!17 = !{!"air.render_target", i32 0, i32 0}"#,
        r#"!17 = !{!"air.function_constant", !99, !"air.sample_mask", !"air.arg_type_name", !"uint"}
!99 = !{ptr addrspace(2) @off.MTL_FC_INIT_0_b, !"bool", !"off", i32 0, i1 false}"#,
    );
    let m = parse_air_fragment_meta(&ll).unwrap();
    assert!(
        !m.is_sample_mask_member(0),
        "an off-by-default mask must not claim the builtin"
    );
}

const VERT_LL: &str = r#"
!air.vertex = !{!15}
!15 = !{ptr @V, !16, !19}
!16 = !{!17, !18}
!19 = !{!20, !21, !22}
!20 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"float3", !"air.arg_name", !"position"}
!21 = !{i32 1, !"air.vertex_input", !"air.location_index", i32 1, i32 1, !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
!22 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 64, !"air.location_index", i32 4, i32 1}
"#;

#[test]
fn vertex_roles() {
    let m = parse_air_vertex_meta(VERT_LL).unwrap();
    assert_eq!(m.role_of(0), Some(&VertRole::VertexInput(0)));
    assert_eq!(m.role_of(1), Some(&VertRole::VertexInput(1)));
    assert_eq!(m.role_of(2), Some(&VertRole::Buffer(4)));
    assert_eq!(m.vertex_input_type(0), Some("float3"));
    assert_eq!(m.vertex_input_name(0), Some("position"));
    assert_eq!(m.vertex_input_type(1), Some("float2"));
    assert_eq!(m.vertex_input_name(1), Some("uv"));
}

const VERT_BUILTIN_LL: &str = r#"
!air.vertex = !{!14}
!14 = !{ptr @vmain, !15, !17}
!15 = !{!16, !20, !21}
!16 = !{!"air.position", !"air.arg_type_name", !"float4"}
!17 = !{!18, !19}
!18 = !{i32 0, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vid"}
!19 = !{i32 1, !"air.instance_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"iid"}
!20 = !{!"air.viewport_array_index", !"air.arg_type_name", !"uint", !"air.arg_name", !"viewport"}
!21 = !{!"air.clip_distance", !"air.arg_type_name", !"float", !"air.arg_name", !"clip"}
"#;

#[test]
fn vertex_builtin_roles() {
    let m = parse_air_vertex_meta(VERT_BUILTIN_LL).unwrap();
    assert_eq!(m.role_of(0), Some(&VertRole::VertexId));
    assert_eq!(m.role_of(1), Some(&VertRole::InstanceId));
    assert_eq!(m.output_role_of(0), Some(&VertOutRole::Position));
    assert_eq!(m.output_role_of(1), Some(&VertOutRole::ViewportArrayIndex));
    assert_eq!(m.output_role_of(2), Some(&VertOutRole::ClipDistance));
}

#[test]
fn invariance_is_decoded_per_output_member() {
    let plain = parse_air_vertex_meta(VERT_BUILTIN_LL).unwrap();
    assert!(
        !plain.output_is_invariant(0),
        "a position without the marker is not invariant"
    );

    let ll = VERT_BUILTIN_LL.replace(
        r#"!16 = !{!"air.position","#,
        r#"!16 = !{!"air.position", !"air.invariant","#,
    );
    assert_ne!(ll, VERT_BUILTIN_LL);
    let m = parse_air_vertex_meta(&ll).unwrap();
    assert!(m.output_is_invariant(0));
    assert_eq!(
        m.output_role_of(0),
        Some(&VertOutRole::Position),
        "the marker must not displace the role it qualifies"
    );
    assert!(
        !m.output_is_invariant(1),
        "the viewport index is not marked"
    );
    assert!(!m.output_is_invariant(2), "the clip distance is not marked");
}

#[test]
fn function_constant_wrapped_function_tables_keep_their_linkage_roles() {
    let ll = r#"
!air.vertex = !{!0}
!0 = !{ptr @main, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.function_constant", !5, !"air.visible_function_table", !"air.location_index", i32 7, i32 1, !"air.read"}
!4 = !{i32 1, !"air.function_constant", !6, !"air.intersection_function_table", !"air.location_index", i32 8, i32 1, !"air.read"}
!5 = !{ptr addrspace(2) @visible_enabled, !"bool", !"visible_enabled"}
!6 = !{ptr addrspace(2) @intersection_enabled, !"bool", !"intersection_enabled"}
"#;
    let meta = parse_air_vertex_meta(ll).unwrap();
    assert_eq!(meta.role_of(0), Some(&VertRole::VisibleFunctionTable(7)));
    assert_eq!(
        meta.role_of(1),
        Some(&VertRole::IntersectionFunctionTable(8))
    );
}

#[test]
fn vertex_patch_contract_preserves_domain_control_points_and_system_locations() {
    let ll = r#"
!air.vertex = !{!0}
!0 = !{ptr @vmain, !1, !2, !10}
!1 = !{!3}
!2 = !{!4, !7, !8, !9}
!3 = !{!"air.position", !"air.arg_type_name", !"float4"}
!4 = !{i32 0, !"air.patch_control_point_input", !5, !6}
!5 = !{!"air.patch_control_point_function", ptr @control.MTL_CONTROL_POINT_FN}
!6 = !{!"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"float3"}
!7 = !{i32 1, !"air.function_constant", !11, !"air.patch_input", !"air.location_index", i32 5, i32 1, !"air.arg_type_name", !"float4"}
!8 = !{i32 2, !"air.instance_id", !"air.arg_type_name", !"uint"}
!9 = !{i32 3, !"air.amplification_count", !"air.arg_type_name", !"ushort"}
!10 = !{!"air.patch", !"quad", !"air.patch_control_point", i32 16}
!11 = !{ptr addrspace(2) @fc, !"bool", !"enabled"}
"#;
    let meta = parse_air_vertex_meta(ll).unwrap();
    let tessellation = meta.tessellation.as_ref().unwrap();
    assert_eq!(tessellation.domain, PatchDomain::Quad);
    assert_eq!(tessellation.control_point_count, 16);
    assert_eq!(
        tessellation.control_point_function.as_deref(),
        Some("control.MTL_CONTROL_POINT_FN")
    );
    assert_eq!(tessellation.control_point_fields[0].location, 2);
    assert_eq!(meta.role_of(0), Some(&VertRole::PatchControlPoints));
    assert_eq!(meta.role_of(1), Some(&VertRole::PatchInput(5)));
    assert_eq!(meta.role_of(2), Some(&VertRole::InstanceId));
    assert_eq!(meta.role_of(3), Some(&VertRole::AmplificationCount));
    assert_eq!(
        meta.tessellation_system_input_location(&VertRole::InstanceId),
        Some(6)
    );
    assert_eq!(
        meta.tessellation_system_input_location(&VertRole::AmplificationCount),
        Some(8)
    );
}

#[test]
fn vertex_render_target_array_index_role() {
    let ll = r#"
!air.vertex = !{!0}
!0 = !{ptr @vmain, !1, !5}
!1 = !{!2, !3, !4}
!2 = !{!"air.position", !"air.arg_type_name", !"float4"}
!3 = !{!"air.render_target_array_index", !"air.arg_type_name", !"uint", !"air.arg_name", !"layer"}
!4 = !{!"air.vertex_output", !"generated(7varyingf)", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"varying"}
!5 = !{}
"#;
    let m = parse_air_vertex_meta(ll).unwrap();
    assert_eq!(m.output_role_of(0), Some(&VertOutRole::Position));
    assert_eq!(
        m.output_role_of(1),
        Some(&VertOutRole::RenderTargetArrayIndex)
    );
    assert_eq!(m.output_role_of(2), Some(&VertOutRole::Varying(0)));
    assert_eq!(m.output_varying_type(0), Some("float"));
    assert_eq!(m.output_varying_name(0), Some("varying"));
    assert_eq!(
        m.output_varying_user_semantic(0),
        Some("generated(7varyingf)")
    );
}

#[test]
fn a_gated_vertex_output_keeps_the_varying_slot_it_declares() {
    let ll = r#"
!air.vertex = !{!0}
!0 = !{ptr @vmain, !1, !6}
!1 = !{!2, !3, !5}
!2 = !{!"air.position", !"air.arg_type_name", !"float4"}
!3 = !{!"air.function_constant", !4, !"air.vertex_output", !"air.arg_type_name", !"float4", !"air.arg_name", !"optional"}
!4 = !{ptr addrspace(2) @__metal_implicit_fc_pred_0, !"bool", !"enabled"}
!5 = !{!"air.vertex_output", !"air.arg_type_name", !"float", !"air.arg_name", !"varying"}
!6 = !{}
"#;
    let m = parse_air_vertex_meta(ll).unwrap();
    assert_eq!(m.output_role_of(0), Some(&VertOutRole::Position));
    assert_eq!(m.output_role_of(1), Some(&VertOutRole::Varying(0)));
    assert_eq!(m.output_role_of(2), Some(&VertOutRole::Varying(1)));

    let disabled = parse_air_vertex_meta(&ll.replace(
        "!\"air.function_constant\", !4,",
        "!\"air.function_constant_disabled\", !4,",
    ))
    .unwrap();
    assert_eq!(
        disabled.output_role_of(1),
        Some(&VertOutRole::FunctionConstantDisabled)
    );
    assert_eq!(disabled.output_role_of(2), Some(&VertOutRole::Varying(0)));
}

const KERN_LL: &str = r#"
!air.kernel = !{!14}
!14 = !{ptr @k, !15, !16}
!15 = !{}
!16 = !{!17, !18, !19, !20, !21, !22, !23, !24, !25, !26, !27, !28, !29, !30}
!17 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"float", !"air.arg_name", !"a"}
!18 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"float", !"air.arg_name", !"b"}
!19 = !{i32 2, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read_write", !"air.arg_type_name", !"float", !"air.arg_name", !"c"}
!20 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
!21 = !{i32 4, !"air.threads_per_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"lsize"}
!22 = !{i32 5, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"lid"}
!23 = !{i32 6, !"air.threadgroups_per_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gsize"}
!24 = !{i32 7, !"air.threadgroup_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"gid"}
!25 = !{i32 8, !"air.thread_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"thread_id"}
!26 = !{i32 9, !"air.thread_index_in_simdgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"lane"}
!27 = !{i32 10, !"air.simdgroup_index_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"simd_group"}
!28 = !{i32 11, !"air.threads_per_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"grid_size"}
!29 = !{i32 12, !"air.threads_per_simdgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"simd_width"}
!30 = !{i32 13, !"air.simdgroups_per_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"num_simd_groups"}
"#;

#[test]
fn kernel_roles() {
    let m = parse_air_kernel_meta(KERN_LL).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::Buffer(0)));
    assert_eq!(m.role_of(1), Some(&KernRole::Buffer(1)));
    assert_eq!(m.role_of(2), Some(&KernRole::Buffer(2)));
    assert_eq!(m.buffer_address_space(0), Some(1));
    assert_eq!(m.buffer_address_space(1), Some(1));
    assert_eq!(m.buffer_address_space(2), Some(1));
    assert_eq!(m.buffer_type_name(0), Some("float"));
    assert_eq!(m.buffer_type_name(1), Some("float"));
    assert_eq!(m.buffer_type_name(2), Some("float"));
    assert_eq!(m.role_of(3), Some(&KernRole::ThreadPositionInGrid));
    assert_eq!(m.role_of(4), Some(&KernRole::ThreadsPerThreadgroup));
    assert_eq!(m.role_of(5), Some(&KernRole::ThreadPositionInThreadgroup));
    assert_eq!(m.role_of(6), Some(&KernRole::ThreadgroupsPerGrid));
    assert_eq!(m.role_of(7), Some(&KernRole::ThreadgroupPositionInGrid));
    assert_eq!(m.role_of(8), Some(&KernRole::ThreadIndexInThreadgroup));
    assert_eq!(
        m.role_of(9),
        Some(&KernRole::ExecutionGroup {
            fact: ExecutionGroupFact::ThreadIndexInGroup,
            lanes: 32,
        })
    );
    assert_eq!(
        m.role_of(10),
        Some(&KernRole::ExecutionGroup {
            fact: ExecutionGroupFact::GroupIndexInThreadgroup,
            lanes: 32,
        })
    );
    assert_eq!(m.role_of(11), Some(&KernRole::ThreadsPerGrid));
    assert_eq!(
        m.role_of(12),
        Some(&KernRole::ExecutionGroup {
            fact: ExecutionGroupFact::ThreadsPerGroup,
            lanes: 32,
        })
    );
    assert_eq!(
        m.role_of(13),
        Some(&KernRole::ExecutionGroup {
            fact: ExecutionGroupFact::GroupsPerThreadgroup,
            lanes: 32,
        })
    );
}

#[test]
fn every_execution_group_marker_is_listed_as_modelled() {
    for &(group, lanes) in AIR_EXECUTION_GROUPS {
        for (marker, fact) in [
            (
                format!("thread_index_in_{group}"),
                ExecutionGroupFact::ThreadIndexInGroup,
            ),
            (
                format!("{group}_index_in_threadgroup"),
                ExecutionGroupFact::GroupIndexInThreadgroup,
            ),
            (
                format!("{group}s_per_threadgroup"),
                ExecutionGroupFact::GroupsPerThreadgroup,
            ),
            (
                format!("threads_per_{group}"),
                ExecutionGroupFact::ThreadsPerGroup,
            ),
        ] {
            assert_eq!(
                execution_group_role(&marker),
                Some((fact, lanes)),
                "`air.{marker}` names a {group}, which is {lanes} threads wide"
            );
            for stage in [Stage::Kernel, Stage::Fragment, Stage::Vertex] {
                let answerable = stage == Stage::Kernel || !fact.needs_a_threadgroup();
                assert_eq!(
                    air_input_role_is_modelled(stage, &marker),
                    answerable,
                    "`air.{marker}` at {stage:?}: a fact about the group is answered at every \
                     stage, a fact about the threadgroup only where there is one"
                );
                let dispatched = format!("dispatch_{marker}");
                assert_eq!(
                    air_input_role_is_modelled(stage, &dispatched),
                    answerable,
                    "`air.{dispatched}` is `air.{marker}` under Vulkan"
                );
            }
            for inventory in [FRAGMENT_INPUT_ROLES, VERTEX_INPUT_ROLES, KERNEL_INPUT_ROLES] {
                assert!(
                    !inventory.contains(&marker.as_str()),
                    "`air.{marker}` is decoded from its shape and must not also be listed"
                );
            }
        }
    }
    for marker in [
        "threads_per_threadgroup",
        "thread_index_in_threadgroup",
        "threadgroups_per_grid",
        "threads_per_grid",
        "thread_position_in_grid",
    ] {
        assert_eq!(
            execution_group_role(marker),
            None,
            "`air.{marker}` is not an execution-group fact"
        );
    }
}

#[test]
fn function_table_roles_preserve_metal_buffer_indices() {
    let ll = r#"
define void @k(ptr addrspace(1) %visible, ptr addrspace(1) %intersection) { ret void }
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.visible_function_table", !"air.location_index", i32 7}
!4 = !{i32 1, !"air.intersection_function_table", !"air.location_index", i32 9}
"#;
    let meta = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(meta.role_of(0), Some(&KernRole::VisibleFunctionTable(7)));
    assert_eq!(
        meta.role_of(1),
        Some(&KernRole::IntersectionFunctionTable(9))
    );
}

#[test]
fn kernel_texture_sampler_roles() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<uint, read>", !"air.arg_name", !"inTex"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 2, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"s"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::Texture(5)));
    assert_eq!(m.texture_type_name(0), Some("texture2d<uint, read>"));
    assert_eq!(m.role_of(1), Some(&KernRole::Sampler(2)));
}

#[test]
fn kernel_stage_in_is_preserved_as_role() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.stage_in", !"air.location_index", i32 7, i32 1, !"air.arg_type_name", !"uint3", !"air.arg_name", !"pointIndices"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::StageInput(7)));
}

#[test]
fn kernel_indirect_buffer_is_a_buffer_role() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 1, !"air.indirect_buffer", !"air.buffer_size", i32 8, !"air.location_index", i32 3, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"count", !"air.indirect_argument", !5, i32 4, i32 4, i32 0, !"uint", !"stride", !"air.indirect_argument", !6}
!5 = !{}
!6 = !{}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(1), Some(&KernRole::Buffer(3)));
    assert_eq!(m.buffer_address_space(1), Some(2));
    assert_eq!(m.buffer_type_size(1), Some(8));
    assert!(matches!(m.layout_of(1), Some(AirType::Struct(fields)) if fields.len() == 2));
    assert_eq!(m.embedded_arguments.len(), 2);
    assert_eq!(m.embedded_arguments[0].buffer_param_index, 1);
    assert_eq!(m.embedded_arguments[0].buffer_index, 3);
    assert_eq!(m.embedded_arguments[0].field_ordinal, 0);
    assert_eq!(m.embedded_arguments[0].field_offset, 0);
    assert_eq!(m.embedded_arguments[1].field_ordinal, 1);
    assert_eq!(m.embedded_arguments[1].field_offset, 4);
}

#[test]
fn kernel_acceleration_structure_introspection_uses_shadow_role() {
    let ll = r#"
define void @k(ptr addrspace(1) %as, ptr addrspace(1) %out) {
  %count = call i32 @air.get_instance_count_instance_acceleration_structure(ptr addrspace(1) %as)
  store i32 %count, ptr addrspace(1) %out
  ret void
}
declare i32 @air.get_instance_count_instance_acceleration_structure(ptr addrspace(1))

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.instance_acceleration_structure", !"air.location_index", i32 8, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(
        m.role_of(0),
        Some(&KernRole::AccelerationStructureShadow(8))
    );
    assert_eq!(m.role_of(1), Some(&KernRole::Buffer(0)));
}

#[test]
fn kernel_primitive_acceleration_structure_is_always_a_literal_geometry_resource() {
    let ll = r#"
define void @k(ptr addrspace(1) %as, ptr addrspace(1) %out) {
  store i32 7, ptr addrspace(1) %out
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.primitive_acceleration_structure", !"air.location_index", i32 5, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<>", !"air.arg_name", !"as"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let meta = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(
        meta.role_of(0),
        Some(&KernRole::PrimitiveAccelerationStructure(5))
    );
    assert_eq!(meta.role_of(1), Some(&KernRole::Buffer(0)));
}

#[test]
fn kernel_imageblock_layout_is_not_a_buffer_layout() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{i32 0, i32 16, i32 0, !"float4", !"v"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::Other));
    assert!(m.layout_of(0).is_none());
    assert!(matches!(
        m.imageblock_layout_of(0),
        Some(AirType::Struct(fields))
            if matches!(
                fields.as_slice(),
                [AirMember {
                    offset: 0,
                    ty: AirType::Vec {
                        scalar: Float,
                        lanes: 4
                    }
                }]
            )
    ));
}

#[test]
fn kernel_function_constant_imageblock_keeps_its_cell_layout() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.function_constant", !4, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !5, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"imageblock<ImageBlockData, layout_explicit>", !"air.arg_name", !"imgBlk"}
!4 = !{ptr addrspace(2) @__metal_implicit_fc_pred_0, !"bool", !"enabled"}
!5 = !{i32 0, i32 16, i32 0, !"float4", !"v"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::Other));
    assert!(matches!(
        m.imageblock_layout_of(0),
        Some(AirType::Struct(fields))
            if matches!(
                fields.as_slice(),
                [AirMember {
                    offset: 0,
                    ty: AirType::Vec {
                        scalar: Float,
                        lanes: 4
                    }
                }]
            )
    ));
}

#[test]
fn kernel_implicit_imageblock_inventory_preserves_attachment_rate_index_and_access() {
    let ll = r#"
define void @k() {
  %a = call <4 x half> @air.load.implicit_imageblock.v4f16(i32 2, <2 x i16> zeroinitializer, i32 3, i16 1)
  call void @air.store.implicit_imageblock.v4f16(<4 x half> %a, i32 2, <2 x i16> zeroinitializer, i32 5, i16 1)
  %b = call <2 x half> @air.load.implicit_imageblock.v2f16(i32 3, <2 x i16> zeroinitializer, i32 0, i16 0)
  call void @air.store.implicit_imageblock.i32(i32 7, i32 4, <2 x i16> zeroinitializer, i32 0, i16 0)
  ret void
}

declare <4 x half> @air.load.implicit_imageblock.v4f16(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.v4f16(<4 x half>, i32, <2 x i16>, i32, i16)
declare <2 x half> @air.load.implicit_imageblock.v2f16(i32, <2 x i16>, i32, i16)
declare void @air.store.implicit_imageblock.i32(i32, i32, <2 x i16>, i32, i16)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    let meta = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(
        meta.implicit_imageblock_attachments,
        [
            ImplicitImageblockAttachment {
                attachment: 2,
                data_rate: 1,
                max_index: Some(5),
                format: TextureFormat::Rgba16f,
                reads: true,
                writes: true,
            },
            ImplicitImageblockAttachment {
                attachment: 3,
                data_rate: 0,
                max_index: Some(0),
                format: TextureFormat::Rg16f,
                reads: true,
                writes: false,
            },
            ImplicitImageblockAttachment {
                attachment: 4,
                data_rate: 0,
                max_index: Some(0),
                format: TextureFormat::R32ui,
                reads: false,
                writes: true,
            },
        ]
    );
}

#[test]
fn unknown_implicit_imageblock_suffix_cannot_disappear_from_reflection() {
    let ll = r#"
define void @k() {
  %value = call <3 x half> @air.load.implicit_imageblock.v3f16(i32 0, <2 x i16> zeroinitializer, i32 0, i16 0)
  ret void
}
declare <3 x half> @air.load.implicit_imageblock.v3f16(i32, <2 x i16>, i32, i16)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !1}
!1 = !{}
"#;
    assert!(parse_air_kernel_meta(ll).is_none());
    assert!(implicit_imageblock_texture_format("air.load.implicit_imageblock.v3f16").is_err());
}

#[test]
fn kernel_threadgroup_buffer_records_address_space() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 3, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 3, !"air.struct_type_info", !4, !"air.arg_type_name", !"Temp", !"air.arg_name", !"temp"}
!4 = !{i32 0, i32 4, i32 0, !"packed_float1", !"value"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(3), Some(&KernRole::Buffer(0)));
    assert_eq!(m.buffer_address_space(3), Some(3));
    assert!(matches!(m.layout_of(3), Some(AirType::Struct(fields)) if fields.len() == 1));
}

#[test]
fn kernel_threadgroup_buffer_infers_address_space_from_signature() {
    let ll = r#"
define void @k(ptr addrspace(3) %temp, i32 %i) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"temp"}
!4 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"uint", !"air.arg_name", !"i"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.role_of(0), Some(&KernRole::Buffer(0)));
    assert_eq!(m.buffer_address_space(0), Some(3));
}

#[test]
fn kernel_meta_variants_share_one_parse_and_preserve_fc_buffer_projection() {
    let ll = r#"
define void @k(ptr addrspace(1) %optional) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.function_constant", !4, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"optional"}
!4 = !{i32 0}
"#;

    let (default, promoted, entry) = parse_air_kernel_meta_variants(ll);
    let default = default.expect("default kernel metadata");
    let promoted = promoted.expect("promoted kernel metadata");

    assert_eq!(entry.as_deref(), Some("k"));
    assert_eq!(default.role_of(0), Some(&KernRole::Other));
    assert_eq!(promoted.role_of(0), Some(&KernRole::Buffer(7)));
    assert_eq!(promoted.buffer_address_space(0), Some(1));
    assert_eq!(promoted.buffer_type_size(0), Some(4));
    assert_eq!(promoted.buffer_type_name(0), Some("float"));
}

#[test]
fn a_gated_buffer_is_bound_only_when_the_module_drives_its_gate_on() {
    let ll = |stored: &str| {
        format!(
            r#"
@pred = internal addrspace(2) global i8 undef, align 1

define internal void @ctor() section "air.static_init" {{
  store i8 {stored}, ptr addrspace(2) @pred, align 1
  ret void
}}

define void @k(ptr addrspace(1) %optional) {{
  ret void
}}

!air.kernel = !{{!0}}
!0 = !{{ptr @k, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.function_constant", !4, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"optional"}}
!4 = !{{ptr addrspace(2) @pred, !"bool", !"kOptional"}}
"#
        )
    };

    let on = parse_air_kernel_meta(&ll("1")).expect("gate on");
    assert_eq!(on.role_of(0), Some(&KernRole::Buffer(7)));
    assert_eq!(on.buffer_address_space(0), Some(1));
    assert_eq!(on.buffer_type_name(0), Some("float"));

    let off = parse_air_kernel_meta(&ll("0")).expect("gate off");
    assert_eq!(off.role_of(0), Some(&KernRole::Other));

    let unresolved = parse_air_kernel_meta(&ll("%runtime")).expect("gate unresolved");
    assert_eq!(unresolved.role_of(0), Some(&KernRole::Other));
}

#[test]
fn a_vertex_gated_buffer_follows_the_same_gate_evidence() {
    let ll = |stored: &str| {
        format!(
            r#"
@pred = internal addrspace(2) global i8 undef, align 1

define internal void @ctor() section "air.static_init" {{
  store i8 {stored}, ptr addrspace(2) @pred, align 1
  ret void
}}

define void @v(ptr addrspace(1) %optional) {{
  ret void
}}

!air.vertex = !{{!0}}
!0 = !{{ptr @v, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.function_constant", !4, !"air.buffer", !"air.location_index", i32 5, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"optional"}}
!4 = !{{ptr addrspace(2) @pred, !"bool", !"kOptional"}}
"#
        )
    };

    let on = parse_air_vertex_meta(&ll("1")).expect("gate on");
    assert_eq!(on.role_of(0), Some(&VertRole::Buffer(5)));

    let off = parse_air_vertex_meta(&ll("0")).expect("gate off");
    assert_eq!(off.role_of(0), Some(&VertRole::Other));
}

#[test]
fn a_fragment_gated_buffer_keeps_its_literal_slot_whichever_way_the_gate_resolves() {
    let ll = |stored: &str| {
        format!(
            r#"
@pred = internal addrspace(2) global i8 undef, align 1

define internal void @ctor() section "air.static_init" {{
  store i8 {stored}, ptr addrspace(2) @pred, align 1
  ret void
}}

define void @f(ptr addrspace(1) %optional) {{
  ret void
}}

!air.fragment = !{{!0}}
!0 = !{{ptr @f, !1, !2}}
!1 = !{{!5}}
!5 = !{{!"air.render_target", i32 0, i32 0}}
!2 = !{{!3}}
!3 = !{{i32 0, !"air.function_constant", !4, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"optional"}}
!4 = !{{ptr addrspace(2) @pred, !"bool", !"kOptional"}}
"#
        )
    };

    for stored in ["1", "0", "%runtime"] {
        let meta = parse_air_fragment_meta(&ll(stored)).expect("fragment meta");
        assert_eq!(
            meta.role_of(0),
            Some(&FragRole::Buffer(7)),
            "gate stored as {stored}"
        );
    }
}

#[test]
fn primitive_air_type_names_include_64_bit_integers() {
    assert_eq!(
        primitive_air_type_from_name("ulong"),
        Some(AirType::Scalar(ULong))
    );
    assert_eq!(
        primitive_air_type_from_name("long2"),
        Some(AirType::Vec {
            scalar: SLong,
            lanes: 2
        })
    );
}

#[test]
fn nested_bitfield_struct_type_info_uses_declared_storage() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Header", !"air.arg_name", !"header"}
!4 = !{!"air.struct_type_info", !5, i32 0, i32 4, i32 0, !"Header::(anonymous)", !"flags", i32 4, i32 4, i32 0, !"float", !"tail"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"mode", i32 0, i32 1, i32 0, !"bool", !"enabled", i32 1, i32 4, i32 0, !"uint", !"depth"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(
        m.layout_of(0),
        Some(&AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Scalar(UInt)
            },
            AirMember {
                offset: 4,
                ty: AirType::Scalar(Float)
            }
        ]))
    );
}

#[test]
fn top_level_bitfield_struct_type_info_has_no_reconstructed_layout() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Packed", !"air.arg_name", !"packed"}
!4 = !{i32 0, i32 4, i32 0, !"uint", !"pixel_x", i32 1, i32 4, i32 0, !"uint", !"pixel_y", i32 3, i32 4, i32 0, !"uint", !"matid"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.layout_of(0), None);
}

#[test]
fn union_struct_type_info_has_no_reconstructed_layout() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Params", !"air.arg_name", !"params"}
!4 = !{i32 0, i32 4, i32 0, !"float", !"as_float", i32 0, i32 4, i32 0, !"int", !"as_int", i32 0, i32 4, i32 0, !"uint", !"as_uint"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(m.layout_of(0), None);
}

#[test]
fn a_refused_nested_node_costs_only_its_own_member() {
    let ll = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.struct_type_info", !4, !"air.arg_type_size", i32 12, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Outer", !"air.arg_name", !"outer"}
!4 = !{i32 0, i32 4, i32 0, !"float", !"lead", !"air.struct_type_info", !5, i32 4, i32 4, i32 0, !"Bits", !"bits", i32 8, i32 4, i32 0, !"uint", !"tail"}
!5 = !{i32 0, i32 4, i32 0, !"uint", !"x", i32 1, i32 4, i32 0, !"uint", !"y"}
"#;
    let m = parse_air_kernel_meta(ll).unwrap();
    assert_eq!(
        m.layout_of(0),
        Some(&AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Scalar(Float)
            },
            AirMember {
                offset: 4,
                ty: AirType::Scalar(UInt)
            },
            AirMember {
                offset: 8,
                ty: AirType::Scalar(UInt)
            }
        ]))
    );
}

#[test]
fn kernel_entry_name() {
    assert_eq!(entry_name(KERN_LL, "kernel").as_deref(), Some("k"));
}

#[test]
fn quoted_kernel_entry_name() {
    let ll = r#"
define void @"re::df::pack"(ptr addrspace(1) %out) {
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @"re::df::pack", !1, !2}
!1 = !{}
!2 = !{!3}
!3 = distinct !{i32 0, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;
    let (meta, _, entry) = parse_air_kernel_meta_variants(ll);
    assert_eq!(entry.as_deref(), Some("re::df::pack"));
    let meta = meta.expect("kernel metadata");
    assert_eq!(meta.role_of(0), Some(&KernRole::Buffer(7)));
    assert_eq!(meta.buffer_address_space(0), Some(1));
}

const RECON_LL: &str = r#"
!air.fragment = !{!0}
!0 = !{ptr @F, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0}
!3 = !{!4, !5, !9}
!4 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.struct_type_info", !6, !"air.arg_type_name", !"Pair"}
!5 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 96, !"air.struct_type_info", !7, !"air.arg_type_name", !"PCC"}
!6 = !{i32 0, i32 8, i32 0, !"packed_float2", !"_a", i32 8, i32 8, i32 0, !"packed_float2", !"_b", i32 16, i32 4, i32 0, !"packed_float1", !"_c"}
!7 = !{i32 0, i32 48, i32 0, !"float3x4", !"_m", !"air.struct_type_info", !8, i32 48, i32 48, i32 0, !"Curve", !"_c"}
!8 = !{i32 0, i32 16, i32 0, !"float3", !"_x", i32 16, i32 16, i32 0, !"float3", !"_y"}
!9 = !{i32 2, !"air.buffer", !"air.buffer_size", i32 32, !"air.struct_type_info", !10, !"air.arg_type_name", !"HistogramLayout"}
!10 = !{i32 0, i32 4, i32 4, !"uint", !"indices", i32 16, i32 4, i32 4, !"uint", !"counts"}
"#;

#[test]
fn struct_type_info_reconstruction() {
    let m = parse_air_fragment_meta(RECON_LL).unwrap();
    assert_eq!(
        m.layout_of(0),
        Some(&AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::PackedVec {
                    scalar: Float,
                    lanes: 2
                }
            },
            AirMember {
                offset: 8,
                ty: AirType::PackedVec {
                    scalar: Float,
                    lanes: 2
                }
            },
            AirMember {
                offset: 16,
                ty: AirType::PackedVec {
                    scalar: Float,
                    lanes: 1
                }
            }
        ]))
    );
    assert_eq!(
        m.layout_of(1),
        Some(&AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Matrix {
                    scalar: Float,
                    cols: 3,
                    rows: 4
                }
            },
            AirMember {
                offset: 48,
                ty: AirType::Struct(vec![
                    AirMember {
                        offset: 0,
                        ty: AirType::Vec {
                            scalar: Float,
                            lanes: 3
                        }
                    },
                    AirMember {
                        offset: 16,
                        ty: AirType::Vec {
                            scalar: Float,
                            lanes: 3
                        }
                    }
                ])
            },
        ]))
    );
    assert_eq!(
        m.layout_of(2),
        Some(&AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Array {
                    elem: Box::new(AirType::Scalar(UInt)),
                    len: 4
                }
            },
            AirMember {
                offset: 16,
                ty: AirType::Array {
                    elem: Box::new(AirType::Scalar(UInt)),
                    len: 4
                }
            },
        ]))
    );
}

#[test]
fn parse_function_constants_reads_fc_initializer_globals() {
    let ll = "\
target triple = \"air64-apple-macosx\"
@_ZL32__metal_implicit_attr_int_expr_1.78 = internal addrspace(2) global i32 0, align 4
@_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j = internal unnamed_addr addrspace(2) externally_initialized constant <4 x i32> undef, section \"air.fc_initializer\", align 16
@some.other.MTL_FC_INIT_3_i = internal addrspace(2) externally_initialized constant i32 undef, section \"air.fc_initializer\", align 4
define void @k() {
entry:
  %1 = load <4 x i32>, ptr addrspace(2) @_ZN2RB6Shader8Constant13_shader_stateE.MTL_FC_INIT_0_Dv4_j
  ret void
}

";
    let fcs = parse_function_constants(ll);
    assert_eq!(fcs.len(), 2, "two distinct FC indices");
    assert_eq!(fcs[0].index, 0);
    assert_eq!(fcs[0].name, "_ZN2RB6Shader8Constant13_shader_stateE");
    assert_eq!(fcs[0].type_name, "<4 x i32>");
    assert_eq!(fcs[0].abi_type_encoding, "Dv4_j");
    assert_eq!(fcs[1].index, 3);
    assert_eq!(fcs[1].type_name, "i32");
    assert_eq!(fcs[1].abi_type_encoding, "i");
    assert!(parse_function_constants("@x = global i32 0").is_empty());
}

#[test]
fn fragment_imageblock_projects_members_by_semantic() {
    let ll = r#"
define { <4 x half>, { half } } @f({ half } %tile) { ret { <4 x half>, { half } } poison }
!air.fragment = !{!0}
!0 = !{ptr @f, !1, !2}
!1 = !{!3, !4}
!2 = !{!5}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4"}
!4 = !{!"air.imageblock_data", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !6, !"air.imageblock_master", !7}
!5 = !{i32 0, !"air.imageblock_data", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !6, !"air.imageblock_master", !7}
!6 = !{i32 0, i32 2, i32 0, !"half", !"user(depth)"}
!7 = !{i32 0, i32 2, i32 0, !"half", !"user(other)", !"air.raster_order_group", i32 1, i32 2, i32 2, i32 0, !"half", !"user(depth)", !"air.raster_order_group", i32 3}
"#;
    let meta = parse_air_fragment_meta(ll).expect("fragment metadata");
    assert_eq!(meta.role_of(0), Some(&FragRole::ImageblockData));
    let reflection = crate::reflect::ShaderReflection::from_fragment(&meta, Some("f"));
    let reflected = reflection
        .fragment_imageblock
        .expect("reflected imageblock master");
    assert_eq!(reflected.members[0].binding, None);
    assert_eq!(reflected.members[1].binding, Some(225));
    assert_eq!(
        reflected.members[1].access,
        crate::reflect::ResourceAccess::ReadWrite
    );
    let imageblock = meta.fragment_imageblock.expect("imageblock master");
    assert_eq!(imageblock.sample_size, 8);
    assert_eq!(imageblock.members[1].offset, 2);
    assert_eq!(imageblock.members[1].raster_order_group, 3);
    assert_eq!(imageblock.inputs[0].members[0].master_member, 1);
    assert_eq!(imageblock.outputs[0].interface_index, 1);
    assert_eq!(imageblock.outputs[0].members[0].master_member, 1);
}

#[test]
fn direct_fragment_imageblock_data_layout_is_its_own_master() {
    let ll = r#"
define { { <4 x half>, i16 } } @f({ <4 x half>, i16 } %tile) { ret { { <4 x half>, i16 } } poison }
!air.fragment = !{!0}
!0 = !{ptr @f, !1, !2}
!1 = !{!3}
!2 = !{!4}
!3 = !{!"air.imageblock_data", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !5}
!4 = !{i32 0, !"air.imageblock_data", !"air.imageblock_data_size", i32 16, !"air.struct_type_info", !5}
!5 = !{i32 0, i32 8, i32 0, !"half4", !"color", !"air.raster_order_group", i32 0, i32 8, i32 2, i32 0, !"ushort", !"depth", !"air.raster_order_group", i32 1}
"#;
    let imageblock = parse_air_fragment_meta(ll)
        .expect("fragment metadata")
        .fragment_imageblock
        .expect("direct imageblock master");
    assert_eq!(imageblock.sample_size, 16);
    assert_eq!(imageblock.members.len(), 2);
    assert_eq!(imageblock.members[0].type_name, "half4");
    assert_eq!(imageblock.members[1].semantic, "depth");
    assert_eq!(imageblock.members[1].raster_order_group, 1);
    assert_eq!(imageblock.inputs[0].members[1].master_member, 1);
    assert_eq!(imageblock.outputs[0].members[0].master_member, 0);
}

#[test]
fn the_location_index_operand_pair_is_read_by_position() {
    use super::{location_operands, LocationOperand};
    let global = "@_ZL32__metal_implicit_attr_int_expr_2.150".to_string();
    for (body, index, count) in [
        (
            r#"i32 0, !"air.texture", !"air.location_index", i32 4, i32 2, !"air.sample""#,
            LocationOperand::Literal(4),
            Some(LocationOperand::Literal(2)),
        ),
        (
            r#"i32 0, !"air.buffer", !"air.location_index", i32 7, ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_2.150, !"air.read_write""#,
            LocationOperand::Literal(7),
            Some(LocationOperand::Global(global.clone())),
        ),
        (
            r#"i32 0, !"air.texture", !"air.location_index", ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_2.150, i32 1, !"air.sample""#,
            LocationOperand::Global(global.clone()),
            Some(LocationOperand::Literal(1)),
        ),
        (
            r#"i32 0, !"air.texture", !"air.location_index", ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_2.150, ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_2.150, !"air.write""#,
            LocationOperand::Global(global.clone()),
            Some(LocationOperand::Global(global.clone())),
        ),
    ] {
        let decoded = location_operands(body).expect("every shape decodes");
        assert_eq!(decoded.index, index, "slot operand of `{body}`");
        assert_eq!(decoded.count, count, "count operand of `{body}`");
    }

    let quoted = r#"i32 0, !"air.texture", !"air.arg_type_name", !"array<texture2d<half, sample>, 2>", !"air.location_index", i32 4, i32 2"#;
    let decoded = location_operands(quoted).expect("decodes past a comma-bearing string");
    assert_eq!(decoded.index, LocationOperand::Literal(4));
    assert_eq!(decoded.count, Some(LocationOperand::Literal(2)));
}

#[test]
fn a_function_constant_count_does_not_become_the_location_index() {
    let ll = FRAG_LL.replace(
        r#"!"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"texture2d<float, sample>""#,
        r#"!"air.location_index", i32 0, ptr addrspace(2) @extent.1, !"air.arg_type_name", !"texture2d<float, sample>""#,
    );
    let ll = format!(
        "@extent.1 = internal addrspace(2) global i32 9, align 4\n\
         define internal void @init() section \"air.static_init\" {{\n\
         entry:\n  store i32 9, ptr addrspace(2) @extent.1, align 4\n  ret void\n}}\n{ll}"
    );
    let meta = parse_air_fragment_meta(&ll).unwrap();
    assert_eq!(
        meta.role_of(2),
        Some(&FragRole::Texture(0)),
        "the texture binds at the slot AIR states, not at the value of its count global"
    );
}

#[test]
fn a_function_constant_render_target_location_is_the_operand_after_its_marker() {
    const FRAGMENT: &str = r#"
@_ZL32__metal_implicit_attr_int_expr_0.103 = internal addrspace(2) global i32 0, align 4

define internal void @_GLOBAL__sub_I_targets.metal() section "air.static_init" {
entry:
  store i32 SLOT, ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_0.103
  ret void
}

!air.fragment = !{!0}
!0 = !{ptr @F, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"half4", !"air.arg_name", !"color"}
!3 = !{!"air.render_target", ptr addrspace(2) @_ZL32__metal_implicit_attr_int_expr_0.103, i32 0, !"air.arg_type_name", !"half4", !"air.arg_name", !"extra"}
!4 = !{}
"#;

    let resolved = FRAGMENT.replace("i32 SLOT", "i32 3");
    let meta = parse_air_fragment_meta(&resolved).unwrap();
    assert_eq!(
        meta.render_target_members,
        vec![(0, 0), (1, 3)],
        "the second member's Location is what its function constant initializes to, not the \
         dual-source index sitting after it"
    );

    let unresolved = resolved.replace(
        "@_ZL32__metal_implicit_attr_int_expr_0.103, i32 0, !\"air.arg_type_name\", !\"half4\", !\"air.arg_name\", !\"extra\"",
        "@__air_location_this_module_never_defines, i32 0, !\"air.arg_type_name\", !\"half4\", !\"air.arg_name\", !\"extra\"",
    );
    let meta = parse_air_fragment_meta(&unresolved).unwrap();
    assert_eq!(meta.render_target_members, vec![(0, 0), (1, 1)]);
}

#[test]
fn a_promoted_resource_role_reads_the_same_wrapped_as_bare() {
    for role in FC_PROMOTED_RESOURCE_ROLES {
        let bare = vec![(*role).to_string()];
        let wrapped = vec!["function_constant".to_string(), (*role).to_string()];
        assert_eq!(
            fc_promoted_role(&wrapped, false),
            Some(*role),
            "a gated {role} keeps its role"
        );
        assert_eq!(
            fc_promoted_role(&wrapped, false),
            fc_promoted_role(&bare, false),
            "and reads the same as the ungated declaration"
        );
        assert_eq!(
            fc_promoted_role(&wrapped, false),
            primary_role(&wrapped),
            "and the same as the fragment decode's reader"
        );
    }
}

#[test]
fn every_vertex_output_role_is_read_past_the_function_constant_wrapper() {
    for (role, _) in VERTEX_OUTPUT_ROLES {
        let bare = vec![(*role).to_string()];
        let wrapped = vec!["function_constant".to_string(), (*role).to_string()];
        let read = |strs: &[String]| {
            gated_role(strs, |role| vertex_output_kind(role).is_some()).map(str::to_string)
        };
        assert_eq!(
            read(&wrapped),
            Some((*role).to_string()),
            "a gated air.{role} return member declares air.{role}"
        );
        assert_eq!(read(&wrapped), read(&bare), "same as the bare declaration");
        assert_eq!(
            read(&wrapped),
            primary_role(&wrapped).map(str::to_string),
            "and same as the fragment decode's reader, which reads every wrapped role"
        );
    }
}

#[test]
fn a_gated_vertex_builtin_is_absent_and_a_gated_varying_is_not() {
    let module = |role: &str| {
        format!(
            r#"
@off = internal addrspace(2) global i8 0, align 1

!air.vertex = !{{!0}}
!0 = !{{ptr @vmain, !1, !9}}
!1 = !{{!2, !3, !5}}
!2 = !{{!"air.position", !"air.arg_type_name", !"float4"}}
!3 = !{{!"air.function_constant", !4, !"air.{role}", !"air.arg_type_name", !"float", !"air.arg_name", !"gated"}}
!4 = !{{ptr addrspace(2) @off, !"bool", !"gate"}}
!5 = !{{!"air.vertex_output", !"air.arg_type_name", !"float", !"air.arg_name", !"after"}}
!9 = !{{}}
"#
        )
    };
    for (role, kind) in VERTEX_OUTPUT_ROLES {
        if matches!(kind, VertexOutputKind::Position) {
            continue;
        }
        let m = parse_air_vertex_meta(&module(role)).unwrap();
        if kind.is_system_value() {
            assert_eq!(
                m.output_role_of(1),
                Some(&VertOutRole::FunctionConstantDisabled),
                "an off air.{role} builtin is absent"
            );
            assert_eq!(
                m.output_role_of(2),
                Some(&VertOutRole::Varying(0)),
                "and never consumed a varying location to begin with"
            );
        } else {
            assert_eq!(
                m.output_role_of(1),
                Some(&VertOutRole::Varying(0)),
                "a gated air.{role} keeps its slot"
            );
            assert_eq!(
                m.output_role_of(2),
                Some(&VertOutRole::Varying(1)),
                "and the member behind it keeps its own"
            );
        }
    }
}

#[test]
fn an_unpromoted_wrapped_role_stays_the_function_constant() {
    let bare_wrapper = vec!["function_constant".to_string()];
    assert_eq!(
        fc_promoted_role(&bare_wrapper, true),
        Some("function_constant")
    );

    let wrapped_buffer = vec!["function_constant".to_string(), "buffer".to_string()];
    assert_eq!(
        fc_promoted_role(&wrapped_buffer, false),
        Some("function_constant")
    );
    assert_eq!(fc_promoted_role(&wrapped_buffer, true), Some("buffer"));
}

#[test]
fn a_declared_role_is_read_past_the_wrapper_by_position() {
    for (node, expected) in [
        (
            r#"i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1"#,
            Some("buffer"),
        ),
        (
            r#"i32 0, !"air.function_constant", !9, !"air.texture", !"air.arg_type_name", !"texture2d<float, sample>""#,
            Some("texture"),
        ),
        (
            r#"i32 0, !"air.function_constant", !9, !"air.amplification_id""#,
            Some("amplification_id"),
        ),
        (
            r#"i32 0, !"air.function_constant", !9, !"air.arg_type_name", !"uint""#,
            None,
        ),
        (
            r#"i32 0, !"air.function_constant", !"air.arg_type_name", !"uint""#,
            None,
        ),
        (
            r#"i32 0, !"air.function_constant_disabled", !9, !"air.texture""#,
            None,
        ),
        (r#"i32 0"#, None),
    ] {
        assert_eq!(declared_role(node).as_deref(), expected, "reading `{node}`");
    }
}

#[test]
fn kernel_stage_input_slots_do_not_depend_on_the_argument_list_order() {
    const FORWARD: &str = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_name", !"float", !"air.arg_name", !"params"}
!4 = !{i32 1, !"air.stage_in", !"air.location_index", i32 7, i32 1, !"air.arg_type_name", !"uint3", !"air.arg_name", !"first"}
!5 = !{i32 2, !"air.stage_in", !"air.location_index", i32 9, i32 1, !"air.arg_type_name", !"uint3", !"air.arg_name", !"second"}
"#;
    const REVERSED: &str = r#"
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!5, !4, !3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_name", !"float", !"air.arg_name", !"params"}
!4 = !{i32 1, !"air.stage_in", !"air.location_index", i32 7, i32 1, !"air.arg_type_name", !"uint3", !"air.arg_name", !"first"}
!5 = !{i32 2, !"air.stage_in", !"air.location_index", i32 9, i32 1, !"air.arg_type_name", !"uint3", !"air.arg_name", !"second"}
"#;
    let forward = parse_air_kernel_meta(FORWARD).expect("kernel metadata");
    let reversed = parse_air_kernel_meta(REVERSED).expect("kernel metadata");
    assert_eq!(
        forward.stage_input_bindings(),
        reversed.stage_input_bindings(),
        "the same kernel listed in a different order got different stage-input slots"
    );
    assert_eq!(
        forward.stage_input_bindings(),
        crate::fxhash::map_from([(1, 0), (2, 2)]),
    );
}

#[test]
fn every_kernel_role_that_names_a_buffer_index_reports_it() {
    for (role, slot) in [
        (KernRole::Buffer(3), Some(3)),
        (KernRole::AccelerationStructureShadow(4), Some(4)),
        (KernRole::PrimitiveAccelerationStructure(5), Some(5)),
        (KernRole::PrimitiveAccelerationStructureShadow(6), Some(6)),
        (KernRole::VisibleFunctionTable(7), Some(7)),
        (KernRole::IntersectionFunctionTable(8), Some(8)),
        (KernRole::Texture(0), None),
        (KernRole::Sampler(0), None),
        (KernRole::StageInput(0), None),
        (KernRole::ThreadPositionInGrid, None),
        (KernRole::Other, None),
    ] {
        assert_eq!(role.buffer_table_slot(), slot, "{role:?}");
    }
}

#[test]
fn an_unmodelled_role_behind_an_unresolved_gate_is_still_refused() {
    let module = |stage: &str, list: &str, resolved: bool| {
        let stored = if resolved { "0" } else { "%opaque" };
        format!(
            r#"
@gate = internal addrspace(2) global i8 0, align 1

declare i8 @opaque()

define internal void @_GLOBAL__sub_I.metal() #0 section "air.static_init" {{
  %opaque = call i8 @opaque()
  store i8 {stored}, ptr addrspace(2) @gate, align 1
  ret void
}}

define void @E() {{
  ret void
}}

!air.{stage} = !{{!0}}
!0 = !{{ptr @E, !1, !2}}
{list}
!9 = !{{ptr addrspace(2) @gate, !"bool", !"gate"}}
"#
        )
    };
    let unmodelled_roles = |stage: &str, list: &str, resolved: bool| -> Vec<(u32, String)> {
        let ll = module(stage, list, resolved);
        match stage {
            "kernel" => {
                parse_air_kernel_meta(&ll)
                    .expect("kernel metadata")
                    .unmodelled_input_params
            }
            "vertex" => {
                parse_air_vertex_meta(&ll)
                    .expect("vertex metadata")
                    .unmodelled_input_params
            }
            _ => {
                let meta = parse_air_fragment_meta(&ll).expect("fragment metadata");
                if list.contains("air.vertex_output") {
                    meta.unmodelled_output_members
                } else {
                    meta.unmodelled_input_params
                }
            }
        }
    };
    let empty_first = "!1 = !{}";
    let sites = [
        (
            "kernel entry parameter",
            "kernel",
            format!(
                "{empty_first}\n!2 = !{{!3}}\n!3 = !{{i32 0, !\"air.function_constant\", !9, \
                 !\"air.render_target_array_index\"}}"
            ),
        ),
        (
            "fragment entry parameter",
            "fragment",
            format!(
                "{empty_first}\n!2 = !{{!3}}\n!3 = !{{i32 0, !\"air.function_constant\", !9, \
                 !\"air.simdgroup_index_in_threadgroup\"}}"
            ),
        ),
        (
            "vertex entry parameter",
            "vertex",
            format!(
                "{empty_first}\n!2 = !{{!3}}\n!3 = !{{i32 0, !\"air.function_constant\", !9, \
                 !\"air.point_coord\"}}"
            ),
        ),
        (
            "fragment return member",
            "fragment",
            "!1 = !{!3}\n!2 = !{}\n!3 = !{!\"air.function_constant\", !9, \
             !\"air.vertex_output\"}"
                .to_string(),
        ),
    ];
    for (site, stage, list) in sites {
        assert!(
            unmodelled_roles(stage, &list, true).is_empty(),
            "{site}: a gate this module's initializers drive to zero leaves the declaration out of \
             the variant, so there is nothing to refuse"
        );
        assert_eq!(
            unmodelled_roles(stage, &list, false).len(),
            1,
            "{site}: the gate is unresolved, so the role is still one this stage cannot lower"
        );
    }
}

#[test]
fn a_gated_on_system_value_is_the_system_value_at_every_stage() {
    let module = |stage: &str, list: &str, on: bool| {
        let value = u8::from(on);
        format!(
            r#"
@gate = internal addrspace(2) global i8 {value}, align 1

define void @E(i32 %sys) {{
  ret void
}}

!air.{stage} = !{{!0}}
!0 = !{{ptr @E, !1, !2}}
!1 = !{{}}
!2 = !{{!3}}
{list}
!9 = !{{ptr addrspace(2) @gate, !"bool", !"gate"}}
"#
        )
    };
    let node = |role: &str| {
        format!(
            "!3 = !{{i32 0, !\"air.function_constant\", !9, !\"air.{role}\", \
             !\"air.arg_type_name\", !\"uint\", !\"air.arg_name\", !\"sys\"}}"
        )
    };

    for (role, expected) in [
        (
            "thread_index_in_simdgroup",
            KernRole::ExecutionGroup {
                fact: ExecutionGroupFact::ThreadIndexInGroup,
                lanes: 32,
            },
        ),
        (
            "thread_index_in_threadgroup",
            KernRole::ThreadIndexInThreadgroup,
        ),
    ] {
        let list = node(role);
        assert_eq!(
            parse_air_kernel_meta(&module("kernel", &list, true))
                .expect("kernel metadata")
                .role_of(0),
            Some(&expected),
            "a gated-ON air.{role} is the builtin the kernel reads"
        );
        assert_eq!(
            parse_air_kernel_meta(&module("kernel", &list, false))
                .expect("kernel metadata")
                .role_of(0),
            Some(&KernRole::Other),
            "...and a gated-OFF one is absent, so a zero is what Metal defines"
        );
    }

    for (role, expected) in [
        ("instance_id", VertRole::InstanceId),
        ("vertex_id", VertRole::VertexId),
    ] {
        let list = node(role);
        assert_eq!(
            parse_air_vertex_meta(&module("vertex", &list, true))
                .expect("vertex metadata")
                .role_of(0),
            Some(&expected),
            "a gated-ON air.{role} is the builtin the vertex shader reads"
        );
        assert_eq!(
            parse_air_vertex_meta(&module("vertex", &list, false))
                .expect("vertex metadata")
                .role_of(0),
            Some(&VertRole::Other),
            "...and a gated-OFF one is absent"
        );
    }

    for (role, expected) in [
        ("front_facing", FragRole::FrontFacing),
        ("sample_id", FragRole::SampleId),
    ] {
        let list = node(role);
        assert_eq!(
            parse_air_fragment_meta(&module("fragment", &list, true))
                .expect("fragment metadata")
                .role_of(0),
            Some(&expected),
        );
        assert_eq!(
            parse_air_fragment_meta(&module("fragment", &list, false))
                .expect("fragment metadata")
                .role_of(0),
            Some(&FragRole::Other),
        );
    }
}

#[test]
fn every_arrayed_texture_spelling_reflects_as_arrayed() {
    use crate::meta::{texture_shape_from_name, TextureDimension};

    let arrayed: [(&str, TextureDimension, bool); 8] = [
        (
            "texture1d_array<float, sample>",
            TextureDimension::D1,
            false,
        ),
        (
            "texture2d_array<float, sample>",
            TextureDimension::D2,
            false,
        ),
        (
            "texture2d_ms_array<float, read>",
            TextureDimension::D2,
            true,
        ),
        ("depth2d_array<float, sample>", TextureDimension::D2, false),
        ("depth2d_ms_array<float, read>", TextureDimension::D2, true),
        (
            "texturecube_array<float, sample>",
            TextureDimension::Cube,
            false,
        ),
        (
            "depthcube_array<float, sample>",
            TextureDimension::Cube,
            false,
        ),
        (
            "array<texture2d_ms_array<float, read>, 4>",
            TextureDimension::D2,
            true,
        ),
    ];
    for (name, dimension, multisampled) in arrayed {
        let shape = texture_shape_from_name(name);
        assert!(shape.arrayed, "{name} is arrayed");
        assert_eq!(shape.dimension, dimension, "{name}");
        assert_eq!(shape.multisampled, multisampled, "{name}");
    }

    let single: [(&str, TextureDimension, bool); 7] = [
        ("texture1d<float, sample>", TextureDimension::D1, false),
        ("texture2d<float, sample>", TextureDimension::D2, false),
        ("texture2d_ms<float, read>", TextureDimension::D2, true),
        ("texture3d<float, sample>", TextureDimension::D3, false),
        ("texturecube<float, sample>", TextureDimension::Cube, false),
        ("depth2d<float, sample>", TextureDimension::D2, false),
        (
            "texture_buffer<float, read>",
            TextureDimension::Buffer,
            false,
        ),
    ];
    for (name, dimension, multisampled) in single {
        let shape = texture_shape_from_name(name);
        assert!(!shape.arrayed, "{name} is a single texture");
        assert_eq!(shape.dimension, dimension, "{name}");
        assert_eq!(shape.multisampled, multisampled, "{name}");
    }
}

const VERT_RT_LL: &str = r#"
declare { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.vertex = !{!15}
!15 = !{ptr @V, !16, !19}
!16 = !{!17, !18}
!19 = !{!20, !21}
!20 = !{i32 0, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vid"}
!21 = !{i32 1, !"air.instance_acceleration_structure", !"air.location_index", i32 3, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<instancing>", !"air.arg_name", !"as"}
"#;

const FRAG_RT_LL: &str = r#"
declare { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)

!air.fragment = !{!15}
!15 = !{ptr @F, !16, !18}
!16 = !{!17}
!17 = !{!"air.render_target", i32 0, i32 0}
!18 = !{!19, !20}
!19 = !{i32 0, !"air.position", !"air.center"}
!20 = !{i32 1, !"air.primitive_acceleration_structure", !"air.location_index", i32 4, i32 1, !"air.read", !"air.arg_type_name", !"acceleration_structure<>", !"air.arg_name", !"as"}
"#;

#[test]
fn vertex_queried_acceleration_structure_is_a_buffer_slot_shadow() {
    let m = parse_air_vertex_meta(VERT_RT_LL).unwrap();
    assert_eq!(m.role_of(0), Some(&VertRole::VertexId));
    assert_eq!(
        m.role_of(1),
        Some(&VertRole::AccelerationStructureShadow(3))
    );
    assert!(
        m.unmodelled_input_params.is_empty(),
        "{:?}",
        m.unmodelled_input_params
    );
}

#[test]
fn fragment_queried_acceleration_structure_is_a_buffer_slot_shadow() {
    let m = parse_air_fragment_meta(FRAG_RT_LL).unwrap();
    assert_eq!(
        m.role_of(1),
        Some(&FragRole::AccelerationStructureShadow(4))
    );
    assert!(
        m.unmodelled_input_params.is_empty(),
        "{:?}",
        m.unmodelled_input_params
    );
}

#[test]
fn unqueried_acceleration_structure_stays_unmodelled() {
    let frag = FRAG_RT_LL.replace("@air.intersect.", "@air.unrelated.");
    let m = parse_air_fragment_meta(&frag).unwrap();
    assert_ne!(
        m.role_of(1),
        Some(&FragRole::AccelerationStructureShadow(4))
    );
    assert!(
        m.unmodelled_input_params
            .iter()
            .any(|(idx, role)| *idx == 1 && role.contains("primitive_acceleration_structure")),
        "{:?}",
        m.unmodelled_input_params
    );
    let vert = VERT_RT_LL.replace("@air.intersect.", "@air.unrelated.");
    let m = parse_air_vertex_meta(&vert).unwrap();
    assert_ne!(
        m.role_of(1),
        Some(&VertRole::AccelerationStructureShadow(3))
    );
    assert!(
        m.unmodelled_input_params
            .iter()
            .any(|(idx, role)| *idx == 1 && role.contains("instance_acceleration_structure")),
        "{:?}",
        m.unmodelled_input_params
    );
}
