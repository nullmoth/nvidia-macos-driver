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

const SAMP1: &str = include_str!("fixtures/app_shapes/samp1.ll");

/// "sampler operand is a pointer whose exact sampler state was not recovered" -- a fragment function picked one of two
/// runtime samplers with a condition read from a constant buffer.
#[test]
fn a_sampler_chosen_by_a_condition_samples_with_each_candidate() {
    let asm = translate(SAMP1, Stage::Fragment).expect("translate");
    let samples = asm.lines().filter(|l| l.contains("OpImageSample")).count();
    assert!(samples >= 2, "one sample per candidate sampler, {samples} found:\n{asm}");
    assert!(asm.contains("OpSelect"), "the chosen result is selected:\n{asm}");
}

const WGPTR1: &str = include_str!("fixtures/app_shapes/wgptr1.ll");

/// "owned Load violates its pointer-pointee and value-type contract ... points at TypeVector, but the result type is
/// TypeFloat" -- a threadgroup float4 array read through a float pointer (a filter kernel's scalar view of its tile).
#[test]
fn a_threadgroup_vector_array_indexed_as_scalars_splits_into_vector_and_lane() {
    let asm = translate(WGPTR1, Stage::Kernel).expect("translate");
    assert!(asm.contains("OpUDiv") && asm.contains("OpUMod"), "the scalar index splits into vector and lane:\n{asm}");
}
