//! A kernel buffer of structs passed into a one-block helper that copies two i32 fields as one i64 (OpenCL keypoint
//! copies do this). The entry translated it; the same code inside a helper failed "reinterpret load bit width mismatch
//! Int(32) vs Int(64)", because the inliner bound the buffer to a proxy and the emitter lost the struct layout.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_opencl_helper_struct_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

#[test]
fn an_i64_copy_of_two_struct_fields_inside_a_helper_translates() {
    let ll = include_str!("fixtures/opencl/helper_struct_i64_fields.ll");
    let spv = translate_sanitized_native(ll, Stage::Kernel, &tmp()).expect("the helper's i64 field copy must translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpShiftLeftLogical"), "the two words are combined into one i64:\n{asm}");
}
