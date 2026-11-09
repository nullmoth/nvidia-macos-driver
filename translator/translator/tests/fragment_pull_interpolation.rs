//! `interpolant<T>` fragment inputs: the shader receives a handle and asks for the value at a pixel offset, at a sample, or
//! at the centroid (Metal's pull-model interpolation). The translator refused every such shader ("unrecognized AIR
//! intrinsic family: air.interpolate_..."). Each request becomes the matching GLSL.std.450 interpolation on the Input
//! variable, which keeps the perspective and sampling decorations of the input it names.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};

fn tmp() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_pull_interp_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn module(calls: &str, perspective: &str) -> String {
    format!(
        r#"target triple = "spirv-unknown-vulkan1.2"

define <4 x float> @frag(<4 x float> %0, ptr addrspace(1) %1) {{
{calls}
}}

declare <2 x float> @air.interpolate_offset_{perspective}.v2f32(ptr addrspace(1), <2 x float>)
declare <2 x float> @air.interpolate_sample_{perspective}.v2f32(ptr addrspace(1), i32)
declare <2 x float> @air.interpolate_centroid_{perspective}.v2f32(ptr addrspace(1))

!air.fragment = !{{!9}}
!air.version = !{{!19}}
!air.language_version = !{{!20}}
!9 = !{{ptr @frag, !10, !12}}
!10 = !{{!11}}
!11 = !{{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4"}}
!12 = !{{!13, !14}}
!13 = !{{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"p", !"air.arg_unused"}}
!14 = !{{i32 1, !"air.fragment_input", !"generated(2uvDv2_f)", !"air.interpolation_function", !"air.{perspective}", !"air.arg_type_name", !"float2", !"air.arg_name", !"uv"}}
!19 = !{{i32 2, i32 7, i32 0}}
!20 = !{{!"Metal", i32 3, i32 2, i32 0}}
"#
    )
}

fn translate(calls: &str, perspective: &str) -> (String, Vec<u8>) {
    let spv = translate_sanitized_native(&module(calls, perspective), Stage::Fragment, &tmp())
        .unwrap_or_else(|e| panic!("pull-model interpolation must translate: {e}"));
    (disassemble(&spv).expect("disassemble"), spv)
}

fn spirv_val(spv: &[u8]) {
    let out = std::process::Command::new("spirv-val")
        .args(["--target-env", "vulkan1.2", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(spv)?;
            c.wait_with_output()
        });
    if let Ok(out) = out {
        assert!(out.status.success(), "spirv-val: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_value_asked_for_at_an_offset_and_at_the_centroid_is_interpolated_on_the_input_variable() {
    let (asm, spv) = translate(
        "  %a = call <2 x float> @air.interpolate_offset_perspective.v2f32(ptr addrspace(1) %1, <2 x float> <float 2.5e-1, float 2.5e-1>)\n  %b = call <2 x float> @air.interpolate_centroid_perspective.v2f32(ptr addrspace(1) %1)\n  %c = shufflevector <2 x float> %a, <2 x float> %b, <4 x i32> <i32 0, i32 1, i32 2, i32 3>\n  ret <4 x float> %c",
        "perspective",
    );
    assert!(asm.contains("OpCapability InterpolationFunction"), "{asm}");
    assert_eq!(asm.matches("InterpolateAtOffset").count(), 1, "{asm}");
    assert_eq!(asm.matches("InterpolateAtCentroid").count(), 1, "{asm}");
    assert!(!asm.contains("NoPerspective"), "a perspective input keeps perspective interpolation:\n{asm}");
    spirv_val(&spv);
}

#[test]
fn a_value_asked_for_at_a_sample_of_a_no_perspective_input_keeps_that_interpolation() {
    let (asm, spv) = translate(
        "  %a = call <2 x float> @air.interpolate_sample_no_perspective.v2f32(ptr addrspace(1) %1, i32 1)\n  %c = shufflevector <2 x float> %a, <2 x float> %a, <4 x i32> <i32 0, i32 1, i32 0, i32 1>\n  ret <4 x float> %c",
        "no_perspective",
    );
    assert_eq!(asm.matches("InterpolateAtSample").count(), 1, "{asm}");
    assert!(asm.contains("NoPerspective"), "{asm}");
    spirv_val(&spv);
}
