use metal2vulkan::passes::Stage;
use metal2vulkan::translate_sanitized_native;
use std::env;
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = env::temp_dir().join(format!("m2v_must_fallback_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn assert_fallback(ll: &str, needle: &str) {
    match translate_sanitized_native(ll, Stage::Kernel, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK (Err) but translate succeeded ({} bytes); \
             wrong-but-valid SPIR-V defeats the floor-safety guarantee",
            spv.len()
        ),
        Err(e) => assert!(
            e.contains(needle),
            "FALLBACK diagnostic should mention {needle:?}; got: {e}"
        ),
    }
}

const HEAD: &str = r#"
target triple = "air64_v28-apple-macosx26.5.0"

%Input = type { [4 x i32] }
%Output = type { [4 x i32] }

define void @k(ptr addrspace(2) %in, ptr addrspace(1) %out) {
entry:
  %a0p = getelementptr inbounds %Input, ptr addrspace(2) %in, i64 0, i32 0, i64 0
  %a0 = load i32, ptr addrspace(2) %a0p
"#;

const TAIL: &str = r#"
  %o0 = getelementptr inbounds %Output, ptr addrspace(1) %out, i64 0, i32 0, i64 0
  store i32 %r, ptr addrspace(1) %o0
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 16, !"air.struct_type_info", !5, !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"Input", !"air.arg_name", !"in"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 16, !"air.struct_type_info", !5, !"air.location_index", i32 1, i32 1, !"air.read_write", !"air.arg_type_name", !"Output", !"air.arg_name", !"out"}
!5 = !{i32 0, i32 16, i32 0, !"uint", !"v0", i32 4, i32 4, i32 0, !"uint", !"v1", i32 8, i32 4, i32 0, !"uint", !"v2", i32 12, i32 4, i32 0, !"uint", !"v3"}
"#;

fn kernel_with(op: &str) -> String {
    format!("{HEAD}{op}{TAIL}")
}

fn kernel_with_declarations(op: &str, declarations: &str) -> String {
    format!("{HEAD}{op}{TAIL}{declarations}")
}

#[test]
fn base_kernel_translates() {
    let base = kernel_with("  %r = add i32 %a0, %a0\n");
    assert!(
        translate_sanitized_native(&base, Stage::Kernel, &tmp()).is_ok(),
        "the negative-suite base template must itself translate"
    );
}

#[test]
fn raytracing_intersect_intrinsic_fallbacks() {
    let ll = kernel_with("  %r = call i32 @air.intersect.f32.i32(i32 %a0, i32 %a0, i32 %a0)\n");
    assert_fallback(&ll, "unknown or duplicate token f32");
}

#[test]
fn agx3_emask_intrinsic_fallbacks() {
    let ll = kernel_with("  %r = call i32 @llvm.agx3.emask.i32(i32 %a0)\n");
    assert_fallback(&ll, "@llvm.agx3.");
}

#[test]
fn texture_atomic_fallbacks() {
    let ll = kernel_with(
        "  %r = call i32 @air.atomic_fetch_add.explicit.texture.2d.i32(i32 %a0, i32 %a0)\n",
    );
    assert_fallback(&ll, "@air.atomic_fetch_add.explicit.texture");
}

#[test]
fn visible_function_table_call_fallbacks() {
    let ll = kernel_with_declarations(
        "  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %out, i32 0)\n\
         \x20 %r = call i32 %fp(ptr addrspace(2) %in)\n",
        "declare ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1), i32)\n",
    );
    assert_fallback(
        &ll,
        "unsupported indirect call through function pointer %fp",
    );
}

const FC_GATED_VFT: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

@enabled.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@kEnabled = internal unnamed_addr addrspace(2) global i8 0, align 1

declare i1 @air.is_function_constant_defined(ptr addrspace(2))
declare ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1), i32)

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %1 = load i8, ptr addrspace(2) @enabled.MTL_FC_INIT_0_b, align 1
  %2 = call i1 @air.is_function_constant_defined(ptr addrspace(2) @enabled.MTL_FC_INIT_0_b)
  %3 = icmp ne i8 %1, 0
  %4 = select i1 %2, i1 %3, i1 false
  %5 = zext i1 %4 to i8
  store i8 %5, ptr addrspace(2) @kEnabled, align 1
  ret void
}

define internal fastcc float @fetch(ptr addrspace(1) %table, ptr addrspace(1) %data) {
  %fp = call ptr @air.get_function_pointer_visible_function_table(ptr addrspace(1) %table, i32 0)
  %r = call float %fp(ptr addrspace(1) %data)
  ret float %r
}

define void @k(ptr addrspace(1) %out, ptr addrspace(1) %table) {
entry:
  %e = load i8, ptr addrspace(2) @kEnabled, align 1
  %c = icmp eq i8 %e, 0
  br i1 %c, label %ARMS

use:
  %v = call fastcc float @fetch(ptr addrspace(1) %table, ptr addrspace(1) %out)
  br label %done

done:
  %r = phi float [ 0.000000e+00, %entry ], [ %v, %use ]
  store float %r, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!air.function_constants = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.function_constant", !6, !"air.visible_function_table", !"air.location_index", i32 1, i32 1, !"air.read", !"air.arg_type_name", !"visible_function_table", !"air.arg_name", !"table"}
!6 = !{ptr addrspace(2) @enabled.MTL_FC_INIT_0_b, !"bool", !"enabled", i32 0, i1 false}
"#;

fn fc_gated_vft(arms: &str) -> String {
    assert!(
        FC_GATED_VFT.contains("label %ARMS"),
        "the branch placeholder must survive edits to the template"
    );
    FC_GATED_VFT.replace("label %ARMS", arms)
}

#[test]
fn function_constant_gated_visible_function_table_call_is_folded_away() {
    let ll = fc_gated_vft("label %done, label %use");
    let spv = translate_sanitized_native(&ll, Stage::Kernel, &tmp()).expect(
        "an off-by-default function constant makes the visible-function-table region dead; \
         folding it is what lets such shaders translate at all",
    );
    assert!(
        !spv.is_empty(),
        "the folded kernel must still emit a module"
    );
}

#[test]
fn a_static_initializer_is_recognised_by_its_air_section_not_its_name() {
    let dead_side = fc_gated_vft("label %done, label %use");

    let renamed = dead_side.replace("_GLOBAL__sub_I_fc", "air_static_ctor");
    assert_ne!(
        renamed, dead_side,
        "the initializer name must be substituted"
    );
    translate_sanitized_native(&renamed, Stage::Kernel, &tmp())
        .expect("an initializer keeps its meaning when only its name changes");

    let unsectioned = dead_side.replace(" section \"air.static_init\"", "");
    assert_ne!(unsectioned, dead_side, "the section must be removed");
    assert_fallback(
        &unsectioned,
        "unsupported indirect call through function pointer %fp",
    );
}

#[test]
fn live_visible_function_table_call_still_fallbacks_under_a_function_constant() {
    let ll = fc_gated_vft("label %use, label %done");
    assert_fallback(
        &ll,
        "unsupported indirect call through function pointer %fp",
    );
}

#[test]
fn intersection_function_buffer_fallbacks() {
    let signature = "{ i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 }";
    let ll = kernel_with_declarations(
        &format!(
            "  %hit = call {signature} @air.intersect.intersection_function_buffer.triangle_data(\
             <3 x float> zeroinitializer, <3 x float> zeroinitializer, float 0.0, float 1.0, \
             ptr addrspace(1) %out, ptr addrspace(1) %out, i64 0, i64 1, ptr null, i64 0, i32 0, \
             i32 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 -1, i32 -1, i32 0, i1 false, i1 false)\n\
             \x20 %r = extractvalue {signature} %hit, 0\n"
        ),
        "declare { i32, float, i32, i32, ptr addrspace(1), <2 x float>, i1 } \
         @air.intersect.intersection_function_buffer.triangle_data(<3 x float>, <3 x float>, float, \
         float, ptr addrspace(1), ptr addrspace(1), i64, i64, ptr, i64, i32, i32, i32, i32, i32, \
         i32, i32, i32, i32, i32, i1, i1)\n",
    );
    assert_fallback(&ll, "air.intersect.intersection_function_buffer");
}

#[test]
fn an_output_member_with_no_lowering_fallbacks() {
    const FRAGMENT: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

define <{ <4 x float>, i32 }> @frag(<4 x float> %pos, i32 %extra) {
entry:
  %r0 = insertvalue <{ <4 x float>, i32 }> undef, <4 x float> %pos, 0
  %r1 = insertvalue <{ <4 x float>, i32 }> %r0, i32 %extra, 1
  ret <{ <4 x float>, i32 }> %r1
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"color"}
!3 = !{!"air.ROLE", !"air.arg_type_name", !"uint", !"air.arg_name", !"extra"}
!4 = !{!5, !6}
!5 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos"}
!6 = !{i32 1, !"air.fragment_input", !"generated(e)", !"air.flat", !"air.arg_type_name", !"uint", !"air.arg_name", !"extra"}
"#;

    let unknown = FRAGMENT.replace("air.ROLE", "air.coverage_of_a_role_that_does_not_exist");
    match translate_sanitized_native(&unknown, Stage::Fragment, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes); an output member \
             nothing writes is a silently wrong module",
            spv.len()
        ),
        Err(e) => {
            assert!(
                e.contains("air.coverage_of_a_role_that_does_not_exist"),
                "the diagnostic should name the role it cannot lower; got: {e}"
            );
            assert!(
                e.contains("return member 1"),
                "and the member it sits on; got: {e}"
            );
        }
    }

    let known = FRAGMENT.replace(r#"!"air.ROLE""#, r#"!"air.sample_mask""#);
    assert!(
        translate_sanitized_native(&known, Stage::Fragment, &tmp()).is_ok(),
        "the same module with a role that does have a lowering must translate"
    );
}

#[test]
fn an_unmodelled_vertex_output_member_fallbacks() {
    const VERTEX: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

define <{ <4 x float>, <2 x float> }> @vert(<4 x float> %p, <2 x float> %uv) {
entry:
  %r0 = insertvalue <{ <4 x float>, <2 x float> }> undef, <4 x float> %p, 0
  %r1 = insertvalue <{ <4 x float>, <2 x float> }> %r0, <2 x float> %uv, 1
  ret <{ <4 x float>, <2 x float> }> %r1
}

!air.vertex = !{!0}
!0 = !{ptr @vert, !1, !4}
!1 = !{!2, !3}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!3 = !{!"air.ROLE", !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
!4 = !{!5, !6}
!5 = !{i32 0, !"air.vertex_input", !"air.location_index", i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"p"}
!6 = !{i32 1, !"air.vertex_input", !"air.location_index", i32 1, !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}
"#;

    let unknown = VERTEX.replace("air.ROLE", "air.output_role_that_does_not_exist");
    match translate_sanitized_native(&unknown, Stage::Vertex, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes)",
            spv.len()
        ),
        Err(e) => assert!(
            e.contains("air.output_role_that_does_not_exist"),
            "the diagnostic should name the role; got: {e}"
        ),
    }

    let known = VERTEX.replace(
        r#"!"air.ROLE""#,
        r#"!"air.vertex_output", !"generated(uv)""#,
    );
    assert!(
        translate_sanitized_native(&known, Stage::Vertex, &tmp()).is_ok(),
        "the same module with a modelled role must translate"
    );
}

#[test]
fn an_entry_parameter_with_no_lowering_fallbacks() {
    const KERNEL: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

define void @k(ptr addrspace(1) %out, i32 %sys) {
entry:
  %p = getelementptr i32, ptr addrspace(1) %out, i64 0
  store i32 %sys, ptr addrspace(1) %p
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.ROLE", !"air.arg_type_name", !"uint", !"air.arg_name", !"sys"}
"#;

    let unknown = KERNEL.replace("air.ROLE", "air.system_value_that_does_not_exist");
    match translate_sanitized_native(&unknown, Stage::Kernel, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes); a system value read \
             as zero is a silently wrong module",
            spv.len()
        ),
        Err(e) => {
            assert!(
                e.contains("air.system_value_that_does_not_exist"),
                "the diagnostic should name the role; got: {e}"
            );
            assert!(
                e.contains("entry parameter 1"),
                "and the parameter it sits on; got: {e}"
            );
        }
    }

    let known = KERNEL.replace(r#"!"air.ROLE""#, r#"!"air.thread_index_in_threadgroup""#);
    assert!(
        translate_sanitized_native(&known, Stage::Kernel, &tmp()).is_ok(),
        "the same module with a modelled role must translate"
    );

    let constant = KERNEL.replace(r#"!"air.ROLE""#, r#"!"air.function_constant""#);
    assert!(
        translate_sanitized_native(&constant, Stage::Kernel, &tmp()).is_ok(),
        "a bare function-constant parameter has no role to reject"
    );

    let gated = |constructor: &str| {
        KERNEL
            .replace(
                r#"!"air.ROLE""#,
                r#"!"air.function_constant", !5, !"air.system_value_that_does_not_exist""#,
            )
            .replace(
                "!air.kernel",
                &format!(
                    "@gate = internal addrspace(2) global i8 0, align 1\n\
                     @mirror = internal addrspace(2) global i8 undef, align 1\n\
                     define internal void @ctor() #0 section \"air.static_init\" {{\n\
                     {constructor}\n  ret void\n}}\n\
                     !5 = !{{ptr addrspace(2) @gate, !\"bool\", !\"gate\"}}\n!air.kernel"
                ),
            )
    };
    let disabled = gated("  store i8 0, ptr addrspace(2) @gate, align 1");
    assert!(
        translate_sanitized_native(&disabled, Stage::Kernel, &tmp()).is_ok(),
        "a gate this module drives to zero leaves the parameter out; there is nothing to reject"
    );
    let unresolved = gated(
        "  %v = load i8, ptr addrspace(2) @mirror, align 1\n  \
         store i8 %v, ptr addrspace(2) @gate, align 1",
    );
    match translate_sanitized_native(&unresolved, Stage::Kernel, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes); a function-constant \
             wrapper is not evidence the parameter is absent",
            spv.len()
        ),
        Err(e) => assert!(
            e.contains("air.system_value_that_does_not_exist"),
            "the diagnostic should name the wrapped role; got: {e}"
        ),
    }
}

#[test]
fn a_builtin_parameter_of_the_wrong_type_fallbacks() {
    const FRAGMENT: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

define <4 x float> @frag(TYPE %sys) {
entry:
  %v0 = insertelement <4 x float> undef, float 0.000000e+00, i32 0
  %v1 = insertelement <4 x float> %v0, float 0.000000e+00, i32 1
  %v2 = insertelement <4 x float> %v1, float 0.000000e+00, i32 2
  %v3 = insertelement <4 x float> %v2, float 1.000000e+00, i32 3
  ret <4 x float> %v3
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4}
!4 = !{i32 0, !"air.ROLE", !"air.center", !"air.arg_type_name", !"NAME"}
"#;

    let build = |role: &str, name: &str, ty: &str| {
        FRAGMENT
            .replace("air.ROLE", &format!("air.{role}"))
            .replace("NAME", name)
            .replace("TYPE", ty)
    };

    for (role, name, ty) in [
        ("position", "float4", "<4 x float>"),
        ("point_coord", "float2", "<2 x float>"),
        ("front_facing", "bool", "i1"),
        ("front_facing", "bool", "i8"),
        ("front_facing", "bool", "i32"),
    ] {
        assert!(
            translate_sanitized_native(&build(role, name, ty), Stage::Fragment, &tmp()).is_ok(),
            "[[{role}]] of type {name} must translate"
        );
    }

    for (role, name, ty, needle) in [
        ("position", "float2", "<2 x float>", "float4"),
        ("point_coord", "float4", "<4 x float>", "float2"),
        ("front_facing", "uint", "i32", "bool"),
        ("front_facing", "uchar", "i8", "bool"),
        ("front_facing", "uint", "i1", "bool"),
    ] {
        match translate_sanitized_native(&build(role, name, ty), Stage::Fragment, &tmp()) {
            Ok(spv) => panic!(
                "expected a FALLBACK for [[{role}]] typed {name}, got {} bytes",
                spv.len()
            ),
            Err(e) => assert!(
                e.contains(role) && e.contains(needle),
                "the diagnostic should name the attribute and the type it needs; got: {e}"
            ),
        }
    }
}

#[test]
fn no_function_definitions_fallbacks() {
    assert_fallback(
        "target triple = \"air64_v28-apple-macosx26.5.0\"\n",
        "no function definitions found",
    );
}

#[test]
fn truncated_module_fallbacks() {
    let ll = "target triple = \"air64_v28-apple-macosx26.5.0\"\n\
              define void @k(ptr %x) {\n\
              entry:\n\
              \x20 %a = load i32, ptr %x\n";
    assert_fallback(ll, "unterminated function");
}

#[test]
fn byte_view_vector_store_into_a_word_block_fallbacks() {
    let ll = r#"target triple = "air64_v28-apple-macosx26.5.0"

%struct.view = type { ptr addrspace(2), ptr addrspace(1) }

define void @k(ptr addrspace(1) %0) {
  %2 = alloca %struct.view, align 8
  %3 = getelementptr %struct.view, ptr %2, i64 0, i32 1
  store ptr addrspace(1) %0, ptr %3, align 8
  %4 = getelementptr i8, ptr addrspace(1) %0, i64 0
  %5 = load i32, ptr addrspace(1) %0, align 16
  call fastcc void @store_vec(ptr %2)
  ret void
}

define fastcc void @store_vec(ptr %0) {
  br label %2

2:                                                ; preds = %1
  br label %4

4:                                                ; preds = %2
  %5 = getelementptr %struct.view, ptr %0, i64 0, i32 1
  %6 = load ptr addrspace(1), ptr %5, align 8
  %7 = zext i32 0 to i64
  %8 = getelementptr i8, ptr addrspace(1) %6, i64 %7
  store <3 x float> zeroinitializer, ptr addrspace(1) %8, align 16
  br label %9

9:                                                ; preds = %4
  ret void
}

!air.kernel = !{!0}

!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !7, !8, !9}
!3 = !{i32 0, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint", !"air.arg_name", !"index"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 280, !"air.location_index", i32 4, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 280, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"hdr", !"air.arg_name", !"header"}
!5 = !{!"air.struct_type_info", !6, i32 0, i32 8, i32 35, !"hdr_entry", !"entries"}
!6 = !{i32 0, i32 4, i32 0, !"int", !"offset", i32 4, i32 2, i32 0, !"short", !"type", i32 6, i32 2, i32 0, !"short", !"stride"}
!7 = !{i32 2, !"air.buffer", !"air.location_index", i32 5, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"data"}
!8 = !{i32 3, !"air.buffer", !"air.buffer_size", i32 280, !"air.location_index", i32 6, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 280, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"hdr", !"air.arg_name", !"header"}
!9 = !{i32 4, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 1, !"air.arg_type_align_size", i32 1, !"air.arg_type_name", !"uchar", !"air.arg_name", !"data"}"#;
    assert_fallback(ll, "no dynamic-struct-index rewrite repaired");
}

#[test]
fn a_runtime_selected_sampler_state_fallbacks() {
    const FRAGMENT: &str = r#"target triple = "spirv-unknown-vulkan1.2"

@__air_sampler_state = internal addrspace(2) constant i64 -9188470239253757879, align 8
@__air_sampler_state.1 = internal addrspace(2) constant i64 -9188470239253755831, align 8

define <4 x float> @frag(<4 x float> %position, <2 x float> %coord, ptr addrspace(1) %tex) {
entry:
  %edge = extractelement <2 x float> %coord, i64 0
  %wide = fcmp oge float %edge, 1.000000e+00
SAMPLER_CHOICE
  %sample = call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) %tex, ptr addrspace(2) SAMPLER_OPERAND, <2 x float> %coord, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00, float 0.000000e+00, i32 0)
  %color = extractvalue { <4 x float>, i8 } %sample, 0
  ret <4 x float> %color
}
declare { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float, float, i32)
!air.fragment = !{!0}
!air.sampler_states = !{!7, !8}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!5 = !{i32 1, !"air.fragment_input", !"generated(coord)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float2", !"air.arg_name", !"coord"}
!6 = !{i32 2, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"tex"}
!7 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state}
!8 = !{!"air.sampler_state", ptr addrspace(2) @__air_sampler_state.1}
"#;

    let selected = FRAGMENT
        .replace(
            "SAMPLER_CHOICE",
            "  %s = select i1 %wide, ptr addrspace(2) @__air_sampler_state, \
             ptr addrspace(2) @__air_sampler_state.1",
        )
        .replace("SAMPLER_OPERAND", "%s");
    match translate_sanitized_native(&selected, Stage::Fragment, &tmp()) {
        Ok(spv) => panic!(
            "expected a clean FALLBACK but translate succeeded ({} bytes); a sample through a \
             default sampler neither branch asked for is a silently wrong module",
            spv.len()
        ),
        Err(e) => assert!(
            e.contains("sampler operand is a pointer"),
            "FALLBACK diagnostic should name the unrecovered sampler operand; got: {e}"
        ),
    }

    for state in ["@__air_sampler_state", "@__air_sampler_state.1"] {
        let fixed = FRAGMENT
            .replace("SAMPLER_CHOICE", "")
            .replace("SAMPLER_OPERAND", state);
        assert!(
            translate_sanitized_native(&fixed, Stage::Fragment, &tmp()).is_ok(),
            "sampling unconditionally through {state} must keep translating"
        );
    }
}

#[test]
fn a_texture_array_whose_declared_length_contradicts_its_type_name_fallbacks() {
    const KERNEL: &str = r#"target triple = "spirv-unknown-vulkan1.2"

define void @k(ptr readonly captures(none) %imgs, ptr addrspace(1) %out) {
entry:
  %slot = getelementptr inbounds ptr addrspace(1), ptr %imgs, i32 3
  %tex = load ptr addrspace(1), ptr %slot, align 8
  %w = tail call i32 @air.get_width_texture_2d(ptr addrspace(1) %tex, i32 0)
  store i32 %w, ptr addrspace(1) %out, align 4
  ret void
}
declare i32 @air.get_width_texture_2d(ptr addrspace(1), i32)
!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 COUNT, !"air.sample", !"air.arg_type_name", !"TEXTURE_TYPE", !"air.arg_name", !"imgs"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
"#;
    let fixed = |count: &str, name: &str| {
        KERNEL
            .replace("i32 COUNT", count)
            .replace("TEXTURE_TYPE", name)
    };

    for (count, name, why) in [
        (
            "i32 3",
            "array<texture2d<float, sample>, 4>",
            "a length the ABI contradicts",
        ),
        (
            "i32 4",
            "texture2d<float, sample>",
            "a count on a name that is not an array at all",
        ),
    ] {
        match translate_sanitized_native(&fixed(count, name), Stage::Kernel, &tmp()) {
            Ok(spv) => panic!(
                "expected a clean FALLBACK for {why} but translate succeeded ({} bytes)",
                spv.len()
            ),
            Err(e) => assert!(
                e.contains("descriptors per `air.location_index`"),
                "the diagnostic should name the ABI count for {why}; got: {e}"
            ),
        }
    }

    assert!(
        translate_sanitized_native(
            &fixed("i32 4", "array<texture2d<float, sample>, 4>"),
            Stage::Kernel,
            &tmp()
        )
        .is_ok(),
        "the same kernel whose two statements of the length agree must translate"
    );
}

const FC_VALUE_GATED_WRITE: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

@size.MTL_FC_INIT_0_t = internal addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@kSamplingSize = internal unnamed_addr addrspace(2) global i16 0, align 2
@tg = internal addrspace(3) global float undef, align 4
@tgi = internal addrspace(3) global i32 undef, align 4

declare void @air.write_texture_2d.v4f32(ptr addrspace(1), <2 x i32>, <4 x float>, i32, i32)
declare i32 @air.atomic.local.add.u.i32(ptr addrspace(3), i32, i32, i32, i1)

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %1 = load i16, ptr addrspace(2) @size.MTL_FC_INIT_0_t, align 2
  store i16 %1, ptr addrspace(2) @kSamplingSize, align 2
  ret void
}

define void @k(ptr addrspace(1) %tex, ptr addrspace(1) %out, <2 x i32> %gid) {
entry:
  %s = load i16, ptr addrspace(2) @kSamplingSize, align 2
  %c = icmp eq i16 %s, SENTINEL
  br i1 %c, label %write, label %done

write:
  GATED
  br label %done

done:
  ALWAYS
  ret void
}

!air.kernel = !{!0}
!air.function_constants = !{!6}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"tex"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
!5 = !{i32 2, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
!6 = !{ptr addrspace(2) @size.MTL_FC_INIT_0_t, !"ushort", !"kSamplingSize", i32 0, i1 true}
"#;

const TEXTURE_WRITE: &str = "call void @air.write_texture_2d.v4f32(ptr addrspace(1) %tex, <2 x i32> %gid, <4 x float> zeroinitializer, i32 0, i32 2)";
const DEVICE_STORE: &str = "store float 1.000000e+00, ptr addrspace(1) %out, align 4";
const THREADGROUP_STORE: &str = "store float 1.000000e+00, ptr addrspace(3) @tg, align 4";
const THREADGROUP_ATOMIC: &str = "%bump = call i32 @air.atomic.local.add.u.i32(ptr addrspace(3) \
                                  @tgi, i32 1, i32 0, i32 1, i1 true)";

fn fc_value_gated_write(sentinel: &str, gated: &str, ungated: &str) -> String {
    for placeholder in ["SENTINEL", "GATED", "ALWAYS"] {
        assert!(
            FC_VALUE_GATED_WRITE.contains(placeholder),
            "the {placeholder} placeholder must survive edits to the template"
        );
    }
    FC_VALUE_GATED_WRITE
        .replace("SENTINEL", sentinel)
        .replace("GATED", gated)
        .replace("ALWAYS", ungated)
}

fn assert_translates(ll: &str, why: &str) {
    let spv = translate_sanitized_native(ll, Stage::Kernel, &tmp())
        .unwrap_or_else(|error| panic!("{why}; got FALLBACK: {error}"));
    assert!(!spv.is_empty(), "{why}; got an empty module");
}

const ERASED: &str =
    "no write survived folding 1 function constant(s) the caller supplied no value for";

#[test]
fn a_function_constant_value_that_selects_the_write_still_translates() {
    assert_translates(
        &fc_value_gated_write("0", TEXTURE_WRITE, ""),
        "the folded constant selects the write, so the module writes its texture",
    );
}

#[test]
fn a_function_constant_value_that_erases_every_write_fallbacks() {
    assert_fallback(&fc_value_gated_write("1", TEXTURE_WRITE, ""), ERASED);
}

#[test]
fn a_function_constant_value_that_erases_every_device_store_fallbacks() {
    assert_fallback(&fc_value_gated_write("1", DEVICE_STORE, ""), ERASED);
}

#[test]
fn a_threadgroup_store_is_not_the_write_the_air_promised() {
    assert_fallback(
        &fc_value_gated_write("1", DEVICE_STORE, THREADGROUP_STORE),
        ERASED,
    );
}

#[test]
fn a_threadgroup_atomic_is_not_the_write_the_air_promised() {
    assert_fallback(
        &fc_value_gated_write("1", DEVICE_STORE, THREADGROUP_ATOMIC),
        ERASED,
    );
}

#[test]
fn a_threadgroup_store_alongside_a_live_device_store_keeps_the_module() {
    assert_translates(
        &fc_value_gated_write("0", DEVICE_STORE, THREADGROUP_STORE),
        "the folded constant selects the device store, so threadgroup scratch beside it is \
         irrelevant",
    );
}

#[test]
fn one_surviving_write_of_another_kind_keeps_the_module() {
    assert_translates(
        &fc_value_gated_write("1", TEXTURE_WRITE, DEVICE_STORE),
        "the device store runs unconditionally, so the module still writes",
    );
    assert_translates(
        &fc_value_gated_write("1", DEVICE_STORE, TEXTURE_WRITE),
        "the texture write runs unconditionally, so the module still writes",
    );
}

const IMAGEBLOCK_ONLY_WRITE: &str = r#"target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::_imageblock_base" = type { ptr addrspace(4) }

define void @k(%"struct.metal::_imageblock_base" %blk, <2 x i16> %tid, ptr addrspace(1) %out) {
entry:
  %cell = tail call ptr addrspace(4) @air.imageblock_data(<2 x i16> %tid, i32 0, i16 0)
  store <4 x half> zeroinitializer, ptr addrspace(4) %cell, align 8
  ALWAYS
  ret void
}

declare ptr addrspace(4) @air.imageblock_data(<2 x i16>, i32, i16)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5, !6}
!3 = !{i32 0, !"air.imageblock", !"explicit", !"air.imageblock_data_size", i32 8, !"air.struct_type_info", !4, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"imageblock<ColorBlock, layout_explicit>", !"air.arg_name", !"colorBlock"}
!4 = !{i32 0, i32 8, i32 0, !"half4", !"color"}
!5 = !{i32 1, !"air.thread_position_in_threadgroup", !"air.arg_type_name", !"ushort2", !"air.arg_name", !"tid"}
!6 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"out"}
"#;

fn imageblock_only_write(ungated: &str) -> String {
    assert!(
        IMAGEBLOCK_ONLY_WRITE.contains("ALWAYS"),
        "the ALWAYS placeholder must survive edits to the template"
    );
    IMAGEBLOCK_ONLY_WRITE.replace("ALWAYS", ungated)
}

#[test]
fn an_imageblock_is_not_a_write_a_caller_can_observe() {
    assert_fallback(
        &imageblock_only_write(""),
        "the entry's only write is into an imageblock",
    );
}

#[test]
fn an_imageblock_store_alongside_a_device_store_keeps_the_module() {
    assert_translates(
        &imageblock_only_write(DEVICE_STORE),
        "the device store runs unconditionally, so staging an imageblock cell beside it is \
         irrelevant",
    );
}

const FC_GATED_DEVICE_ATOMIC: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

@size.MTL_FC_INIT_0_t = internal addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@kSamplingSize = internal unnamed_addr addrspace(2) global i16 0, align 2

declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1) captures(none), i32, i32, i32, i1)
declare i32 @air.atomic.global.load.u.i32(ptr addrspace(1) captures(none), i32, i32)

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
  %1 = load i16, ptr addrspace(2) @size.MTL_FC_INIT_0_t, align 2
  store i16 %1, ptr addrspace(2) @kSamplingSize, align 2
  ret void
}

define void @k(ptr addrspace(1) %out, <2 x i32> %gid) {
entry:
  %s = load i16, ptr addrspace(2) @kSamplingSize, align 2
  %c = icmp eq i16 %s, SENTINEL
  br i1 %c, label %write, label %done

write:
  GATED
  br label %done

done:
  ALWAYS
  ret void
}

!air.kernel = !{!0}
!air.function_constants = !{!4}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !5}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!5 = !{i32 1, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
!4 = !{ptr addrspace(2) @size.MTL_FC_INIT_0_t, !"ushort", !"kSamplingSize", i32 0, i1 true}
"#;

const DEVICE_ATOMIC_ADD: &str =
    "%bump = call i32 @air.atomic.global.add.u.i32(ptr addrspace(1) %out, i32 1, i32 0, i32 2, i1 true)";
const DEVICE_ATOMIC_LOAD: &str =
    "%seen = call i32 @air.atomic.global.load.u.i32(ptr addrspace(1) %out, i32 0, i32 2)";

fn fc_gated_device_atomic(sentinel: &str, gated: &str, ungated: &str) -> String {
    for placeholder in ["SENTINEL", "GATED", "ALWAYS"] {
        assert!(
            FC_GATED_DEVICE_ATOMIC.contains(placeholder),
            "the {placeholder} placeholder must survive edits to the template"
        );
    }
    FC_GATED_DEVICE_ATOMIC
        .replace("SENTINEL", sentinel)
        .replace("GATED", gated)
        .replace("ALWAYS", ungated)
}

#[test]
fn a_device_atomic_is_a_write_the_air_promised() {
    assert_fallback(&fc_gated_device_atomic("1", DEVICE_ATOMIC_ADD, ""), ERASED);
}

#[test]
fn a_surviving_device_atomic_keeps_the_module() {
    assert_translates(
        &fc_gated_device_atomic("0", DEVICE_ATOMIC_ADD, ""),
        "the folded constant selects the atomic, so the module still bumps its counter",
    );
}

#[test]
fn a_kernel_whose_only_device_atomic_is_a_load_still_translates() {
    assert_translates(
        &fc_gated_device_atomic("1", DEVICE_ATOMIC_LOAD, ""),
        "an atomic load is not a write, so nothing was erased",
    );
}
