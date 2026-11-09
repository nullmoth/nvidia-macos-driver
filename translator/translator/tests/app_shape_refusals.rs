//! Shader shapes that real applications ship and the translator used to refuse. Each fixture is the Metal compiler's
//! own output for a small source that has the same shape; the refusal text in each comment is what the applications'
//! logs showed.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_app_shape_refusals_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn translate(ll: &str, stage: Stage) -> Result<String, String> {
    let spv = translate_sanitized_native(ll, stage, &tmp())?;
    disassemble(&spv).map_err(|e| e.to_string())
}

const PTRCMP: &str = include_str!("fixtures/app_shapes/ptrcmp.ll");

/// "pointer icmp is only supported against null: ne ... %7, %2" -- a fragment function compared a pointer chosen by
/// a select against one of the buffer arguments it was chosen from.
#[test]
fn a_selected_buffer_pointer_compared_with_one_of_its_arms_translates() {
    let asm = translate(PTRCMP, Stage::Fragment).expect("translate");
    assert!(asm.contains("OpSelect"), "the comparison folds into a select of the arm answers:\n{asm}");
}

#[test]
fn two_different_buffer_arguments_still_compare_unequal() {
    let ll = PTRCMP.replace("icmp ne <4 x float> addrspace(1)* %7, %2", "icmp ne <4 x float> addrspace(1)* %2, %3");
    assert_ne!(ll, PTRCMP);
    translate(&ll, Stage::Fragment).expect("translate");
}
