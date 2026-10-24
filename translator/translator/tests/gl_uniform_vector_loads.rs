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


// Dome Keeper (Godot's GL compatibility renderer): the helper is called twice, the caller stores the same unit before
// each call, and the emitter lowers the switch over units to a chain of compares (one header per case, the last cases
// joined by an inner phi). The sample must use the pair for unit 3, the third (texture/sampler index 2).
#[test]
fn a_sampler2d_parameter_stored_before_each_of_two_calls_samples_that_pair() {
    let ll = include_str!("fixtures/gl/sampler2d_param_two_calls_switch_chain.ll");
    let spv = translate_sanitized_native(ll, Stage::Fragment, &tmp()).expect("the outline lookups must translate");
    let asm = disassemble(&spv).expect("disassemble");
    if std::env::var_os("M2V_SHOW_ASM").is_some() { eprintln!("{asm}"); }
    assert!(!asm.lines().any(|l| l.contains("OpPhi") && (l.contains("Image") || l.contains("Sampler"))), "no image/sampler phi left:\n{asm}");
    // Both OpSampledImage combine the third sampler (its variable is bound right after the first two) and the third
    // texture (read from the descriptor heap at the third of the texture slots the shader reads).
    let lines: Vec<Vec<&str>> = asm.lines().map(|l| l.split_whitespace().collect()).collect();
    let def = |id: &str| lines.iter().find(|w| w.len() > 2 && w[0] == id && w[1] == "=").cloned().unwrap_or_default();
    let binding = |var: &str| lines.iter().find(|w| w.len() == 4 && w[0] == "OpDecorate" && w[1] == var && w[2] == "Binding")
        .and_then(|w| w[3].parse::<u32>().ok());
    let constant = |id: &str| { let d = def(id); (d.get(2) == Some(&"OpConstant")).then(|| d.last().and_then(|v| v.parse::<u32>().ok())).flatten() };
    // image = OpLoad (OpAccessChain heap ([OpBitwiseAnd] (OpLoad (OpAccessChain table 0 <slot> 0)) [mask])) -> slot
    let image_slot = |image: &str| -> Option<u32> {
        let load = def(image);
        let chain = def(load.last()?);
        let mut index = def(chain.last()?);
        if index.get(2) == Some(&"OpBitwiseAnd") { index = def(index.get(4)?); }
        if index.get(2) != Some(&"OpLoad") { return None; }
        let table = def(index.last()?);
        constant(table.get(table.len().checked_sub(2)?)?)
    };
    let image_slots: Vec<u32> = lines.iter().filter(|w| w.len() == 5 && w[2] == "OpLoad").filter_map(|w| image_slot(w[0])).collect();
    let sampler_bindings: Vec<u32> = lines.iter().filter(|w| w.len() == 5 && w[2] == "OpLoad").filter_map(|w| binding(w[4])).collect();
    assert_eq!((image_slots.len(), sampler_bindings.len()), (4, 4), "four textures and four samplers are read:\n{asm}");
    let (mut image_slots, mut sampler_bindings) = (image_slots, sampler_bindings);
    image_slots.sort(); sampler_bindings.sort();
    let samples: Vec<&Vec<&str>> = lines.iter().filter(|w| w.contains(&"OpSampledImage")).collect();
    assert_eq!(samples.len(), 2, "{asm}");
    for s in samples {
        let (image, sampler) = (s[s.len() - 2], s[s.len() - 1]);
        assert_eq!(image_slot(image), Some(image_slots[2]), "{image} is not the third texture:\n{asm}");
        assert_eq!(def(sampler).last().and_then(|v| binding(v)), Some(sampler_bindings[2]), "{sampler} is not the third sampler:\n{asm}");
    }
    spirv_val(&spv);
}
