//! A fragment shader that zeroes the padding tail of a local output struct through a byte pointer (wgpu's `fs_fill`,
//! issue #330). The struct is modelled as a byte array once a byte GEP reaches into it; the memset that clears the
//! tail failed to translate because it was lowered as if the array were a buffer of words.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_function_local_padding_memset_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn shader(tail: &str) -> String {
    format!(
        r#"target triple = "air64_v28-apple-macosx26.5.0"
%struct.FillOut = type <{{ <4 x float>, [12 x i8] }}>
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
define <4 x float> @fs_fill(<4 x float> %pos, <4 x float> %c) {{
entry:
  %a = alloca %struct.FillOut, align 16
  %f0 = getelementptr inbounds %struct.FillOut, ptr %a, i32 0, i32 0
  store <4 x float> %c, ptr %f0, align 16
  %p = getelementptr inbounds i8, ptr %a, i64 16
{tail}
  %v = load <4 x float>, ptr %f0, align 16
  ret <4 x float> %v
}}
!air.fragment = !{{!0}}
!0 = !{{ptr @fs_fill, !1, !4}}
!1 = !{{!2}}
!2 = !{{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float4", !"air.arg_name", !"color"}}
!4 = !{{!5, !6}}
!5 = !{{i32 0, !"air.position", !"air.center", !"air.no_perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"pos"}}
!6 = !{{i32 1, !"air.fragment_input", !"user(locn0)", !"air.center", !"air.perspective", !"air.arg_type_name", !"float4", !"air.arg_name", !"c"}}
"#
    )
}

fn translates(tail: &str) -> String {
    let spv = translate_sanitized_native(&shader(tail), Stage::Fragment, &tmp()).expect("translate");
    disassemble(&spv).expect("disassemble")
}

#[test]
fn memset_of_a_local_struct_padding_tail_translates() {
    let asm = translates("  call void @llvm.memset.p0.i64(ptr %p, i8 0, i64 12, i1 false)");
    assert!(asm.contains("OpStore"), "the cleared tail and the output colour are stored:\n{asm}");
}

#[test]
fn one_byte_memset_into_the_tail_translates() {
    translates("  call void @llvm.memset.p0.i64(ptr %p, i8 0, i64 1, i1 false)");
}

#[test]
fn a_plain_byte_store_into_the_tail_still_translates() {
    translates("  store i8 0, ptr %p, align 1");
}
