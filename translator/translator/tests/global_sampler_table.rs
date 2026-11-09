//! A program-scope array of samplers (`constexpr constant sampler table[N]`) is a module constant with one struct per
//! element. The emitter refused it ("unsupported global initializer"), so an application that builds such a render shader
//! at start-up (Goodnotes) crashed when it opened any document.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};

fn tmp() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_global_sampler_table_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn sampler_bindings_used(asm: &str) -> Vec<u32> {
    // OpSampledImage %t %image %sampler where the sampler is an OpLoad of a decorated variable
    let mut bindings = Vec::new();
    for line in asm.lines() {
        let Some(rest) = line.trim().strip_prefix("OpDecorate ") else { continue };
        let mut it = rest.split_whitespace();
        let (_var, kind) = (it.next(), it.next());
        if kind == Some("Binding") {
            if let Some(n) = it.next().and_then(|n| n.parse::<u32>().ok()) {
                if n >= 160 && n < 176 {
                    bindings.push(n);
                }
            }
        }
    }
    bindings.sort();
    bindings
}

#[test]
fn a_table_of_static_samplers_indexed_by_constants_samples_with_each_element() {
    let ll = include_str!("fixtures/gl/global_sampler_table_constant_index.ll");
    let spv = translate_sanitized_native(ll, Stage::Fragment, &tmp()).expect("the table must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert_eq!(asm.matches("OpImageSampleImplicitLod").count(), 2, "{asm}");
    assert_eq!(sampler_bindings_used(&asm), vec![160, 161], "one sampler per element:\n{asm}");
}

#[test]
fn a_table_of_static_samplers_indexed_at_run_time_samples_once_per_element_and_picks_by_index() {
    let ll = include_str!("fixtures/gl/global_sampler_table_dynamic_index.ll");
    let spv = translate_sanitized_native(ll, Stage::Fragment, &tmp()).expect("the table must translate");
    let asm = disassemble(&spv).expect("disassemble");
    // table[k] is sampled once per element (2) and table[1] once
    assert_eq!(asm.matches("OpImageSampleImplicitLod").count(), 3, "{asm}");
    assert!(asm.contains("OpSelect"), "the element is picked by the index:\n{asm}");
    assert_eq!(sampler_bindings_used(&asm), vec![160, 161], "{asm}");
}
