//! Civilization VI's model, fire and terrain fragment shaders (D3D shaders converted to Metal) filter shadows with
//! `depth2d::gather_compare(sampler, coord, reference, offset)`, AIR `air.gather_compare_depth_2d.f32`. The translator
//! did not know the family ("unrecognized AIR intrinsic family"), so every such shader was refused and units,
//! buildings and terrain features were missing from the map. With a runtime sampler the call is
//! `OpImageDrefGather` (the Vulkan sampler carries the compare function), the constant offset as `ConstOffset`.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};
use metal2vulkan::passes::TransformOptions;

fn spirv_val(spv: &[u8]) {
    let out = std::process::Command::new("spirv-val").args(["--target-env", "vulkan1.2", "-"]).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut c| {
            use std::io::Write; c.stdin.take().unwrap().write_all(spv)?; c.wait_with_output() });
    if let Ok(out) = out { assert!(out.status.success(), "spirv-val: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)); }
}

#[test]
fn a_depth2d_gather_compare_with_offsets_is_a_dref_gather_on_the_first_attempt() {
    let ll = include_str!("fixtures/civ6/gather_compare_depth_2d_offsets.ll");
    // The plugin's first attempt (as written, no pointer retry): Civ VI's shaders must not need the retry.
    let spv = translate_native_no_retry_constructed_with_options(ll, Stage::Fragment, TransformOptions::default())
        .expect("the gather_compare shadow taps must translate");
    let asm = disassemble(&spv).expect("disassemble");
    if std::env::var_os("M2V_SHOW_ASM").is_some() { eprintln!("{asm}"); }
    let gathers: Vec<&str> = asm.lines().filter(|l| l.contains("OpImageDrefGather")).collect();
    assert_eq!(gathers.len(), 4, "{asm}");
    assert!(gathers.iter().all(|l| l.contains("ConstOffset")), "every tap keeps its offset:\n{asm}");
    assert!(!asm.contains("OpImageGather "), "no plain gather:\n{asm}");
    // The four offsets, in the order of the calls.
    let def = |id: &str| asm.lines().find(|l| l.trim_start().starts_with(&format!("{id} = "))).unwrap_or("").to_string();
    let int = |id: &str| def(id).split_whitespace().last().and_then(|v| v.parse::<i32>().ok());
    let offsets: Vec<(i32, i32)> = gathers.iter().map(|l| {
        let c = def(l.split_whitespace().last().unwrap());
        let w: Vec<&str> = c.split_whitespace().collect();
        (int(w[w.len() - 2]).unwrap(), int(w[w.len() - 1]).unwrap())
    }).collect();
    assert_eq!(offsets, vec![(-1, -1), (1, -1), (-1, 1), (1, 1)], "{asm}");
    spirv_val(&spv);
}
