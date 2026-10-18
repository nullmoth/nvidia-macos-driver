//! Apple's GL compiles a uniform block member as a GEP over float slots of the block's byte buffer and loads the
//! member's own type through it: an ivec3 at offset 0 is `getelementptr float, ptr %ubo, i32 0` then
//! `load <3 x i32>`. Minecraft's chunk vertex shaders read their chunk offset (ivec3) this way; the translator took
//! the zero-offset GEP for the buffer itself and failed "reinterpret load bit width mismatch Float (32) vs
//! Vector(Int(32), 3) (96)", so Apple's GL ran those shaders on the CPU.
//!
//! The same shaders sample the lightmap through a GLSL function taking a sampler2D: Apple's GL passes every
//! texture/sampler pair with the index of the one meant, a constant the caller stores, and picks with a switch whose
//! phis have pointer type over loaded images and samplers ("sampler operand is a pointer whose exact sampler state
//! was not recovered").
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_gl_uniform_vector_loads_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn spirv_val(spv: &[u8]) {
    let out = std::process::Command::new("spirv-val").args(["--target-env", "vulkan1.2", "-"]).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut c| {
            use std::io::Write; c.stdin.take().unwrap().write_all(spv)?; c.wait_with_output() });
    if let Ok(out) = out { assert!(out.status.success(), "spirv-val: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)); }
}

#[test]
fn an_ivec3_uniform_at_offset_zero_of_a_float_gep_translates() {
    let ll = include_str!("fixtures/gl/ivec3_uniform_through_float_gep.ll");
    let spv = translate_sanitized_native(ll, Stage::Vertex, &tmp()).expect("the ivec3 uniform load must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpConvertSToF"), "the ivec3 difference is converted to float:\n{asm}");
    spirv_val(&spv);
}

#[test]
fn a_sampler2d_parameter_chosen_by_a_constant_index_samples_that_pair() {
    let ll = include_str!("fixtures/gl/sampler2d_param_chosen_by_constant.ll");
    let spv = translate_sanitized_native(ll, Stage::Vertex, &tmp()).expect("the lightmap lookup must translate");
    let asm = disassemble(&spv).expect("disassemble");
    if std::env::var_os("M2V_SHOW_ASM").is_some() { eprintln!("{asm}"); }
    assert!(asm.contains("OpImageSampleExplicitLod"), "{asm}");
    assert!(!asm.lines().any(|l| l.contains("OpPhi") && (l.contains("Image") || l.contains("Sampler"))), "no image/sampler phi left:\n{asm}");
    spirv_val(&spv);
}

