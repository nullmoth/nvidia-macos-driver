use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

// Apple's GL compiler lowers GLSL `bvec` comparisons (WebRender's fragment shaders in Firefox) to `icmp` on
// `<N x i1>`. SPIR-V has no integer compare on booleans: before this, the native emitter refused the shader
// ("icmp needs integer scalar/vector operands, got Vector(Bool, 4)"), the GL pipeline failed to build and the GL
// engine fell back to software vertex and fragment processing.
const BOOL_COMPARE_FRAGMENT: &str = r#"target triple = "air64_v28-apple-macosx26.5.0"

define <4 x float> @frag(<4 x float> %pos, <4 x float> %a, <4 x float> %b) {
entry:
  %ba = fcmp olt <4 x float> %a, %b
  %bb = fcmp ogt <4 x float> %a, zeroinitializer
  %eq = icmp eq <4 x i1> %ba, %bb
  %ne = icmp ne <4 x i1> %ba, %bb
  %gt = icmp ugt <4 x i1> %ba, %bb
  %le = icmp sle <4 x i1> %ba, %bb
  %e0 = extractelement <4 x i1> %eq, i32 0
  %n1 = extractelement <4 x i1> %ne, i32 1
  %g2 = extractelement <4 x i1> %gt, i32 2
  %l3 = extractelement <4 x i1> %le, i32 3
  %x0 = extractelement <4 x i1> %ba, i32 0
  %y0 = extractelement <4 x i1> %bb, i32 0
  %s = icmp eq i1 %x0, %y0
  %f0 = select i1 %e0, float 1.000000e+00, float 0.000000e+00
  %f1 = select i1 %n1, float 1.000000e+00, float 0.000000e+00
  %f2 = select i1 %g2, float 1.000000e+00, float 0.000000e+00
  %f3a = select i1 %l3, float 1.000000e+00, float 0.000000e+00
  %f3 = select i1 %s, float %f3a, float 5.000000e-01
  %v0 = insertelement <4 x float> undef, float %f0, i32 0
  %v1 = insertelement <4 x float> %v0, float %f1, i32 1
  %v2 = insertelement <4 x float> %v1, float %f2, i32 2
  %v3 = insertelement <4 x float> %v2, float %f3, i32 3
  ret <4 x float> %v3
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !3}
!1 = !{!2}
!2 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}
!3 = !{!4, !5, !6}
!4 = !{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos"}
!5 = !{i32 1, !"air.fragment_input", !"generated(a)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"a"}
!6 = !{i32 2, !"air.fragment_input", !"generated(b)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"b"}
"#;

#[test]
fn icmp_on_boolean_vectors_uses_logical_ops() {
    let spv = translate_native_no_retry_constructed_with_options(
        BOOL_COMPARE_FRAGMENT, Stage::Fragment, TransformOptions::default(),
    )
        .expect("a fragment shader comparing <4 x i1> values must translate");
    let asm = disassemble(&spv).expect("disassemble");
    for op in ["OpLogicalEqual", "OpLogicalNotEqual", "OpLogicalAnd", "OpLogicalOr", "OpLogicalNot"] {
        assert!(asm.contains(op), "expected {op} in:\n{asm}");
    }
    for op in ["OpIEqual", "OpINotEqual", "OpUGreaterThan", "OpSLessThanEqual"] {
        assert!(!asm.contains(op), "{op} on booleans is invalid SPIR-V:\n{asm}");
    }
}
