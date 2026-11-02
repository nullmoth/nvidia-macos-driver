//! DaVinci Resolve's viewer display shader (VS_Main) takes its per-object buffer as `struct GenericUniformHeader {
//! packed_uint4 header; }`, which is `{ { [4 x i32] } }`, and reads float4 uniforms out of the slots after the header:
//! `load <4 x float>` through `getelementptr %struct.GenericUniformHeader, ptr %objectHeader, i64 N`. The native emitter
//! only read a vector from a scalar array when the pointer's pointee was the array itself, so it refused the shader
//! ("cannot reinterpret load ... with non-bitcastable pointee Struct([Struct([Array(Int(32), 4)])]) to Vector(Float, 4)")
//! and Resolve's viewer stayed black with "Failed to build display shader".
use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

const TWO_LEVEL_HEADER: &str = r#"target triple = "air64_v28-apple-macosx26.7.0"

%struct.GenericUniformHeader = type { { [4 x i32] } }

define <{ <4 x float> }> @VS_Main(ptr addrspace(2) readonly %objectHeader, i32 %vid) {
entry:
  %0 = getelementptr inbounds %struct.GenericUniformHeader, ptr addrspace(2) %objectHeader, i64 0, i32 0, i32 0, i64 1
  %flags = load i32, ptr addrspace(2) %0, align 4
  %add.ptr = getelementptr inbounds %struct.GenericUniformHeader, ptr addrspace(2) %objectHeader, i64 1
  %1 = bitcast ptr addrspace(2) %add.ptr to ptr addrspace(2)
  %copyload = load <4 x float>, ptr addrspace(2) %1, align 16
  %slot = and i32 %flags, 3
  %idx = zext i32 %slot to i64
  %rect.ptr = getelementptr inbounds %struct.GenericUniformHeader, ptr addrspace(2) %add.ptr, i64 %idx
  %rect = load <4 x float>, ptr addrspace(2) %rect.ptr, align 16
  %pos = fadd fast <4 x float> %copyload, %rect
  %ret = insertvalue <{ <4 x float> }> undef, <4 x float> %pos, 0
  ret <{ <4 x float> }> %ret
}

!air.vertex = !{!0}
!air.version = !{!7}
!air.language_version = !{!8}

!0 = !{ptr @VS_Main, !1, !3}
!1 = !{!2}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"clipSpacePosition"}
!3 = !{!4, !6}
!4 = !{i32 0, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"GenericUniformHeader", !"air.arg_name", !"objectHeader"}
!5 = !{i32 0, i32 16, i32 0, !"packed_uint4", !"header"}
!6 = !{i32 1, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vid"}
!7 = !{i32 2, i32 8, i32 0}
!8 = !{!"Metal", i32 2, i32 0, i32 0}
"#;

fn translate(ll: &str) -> Vec<u8> {
    translate_native_no_retry_constructed_with_options(ll, Stage::Vertex, TransformOptions::default())
        .unwrap_or_else(|e| panic!("a float4 read from a header slot must translate: {e}"))
}

fn spirv_val(spv: &[u8]) {
    let out = std::process::Command::new("spirv-val").args(["--target-env", "vulkan1.2", "-"]).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().and_then(|mut c| {
            use std::io::Write; c.stdin.take().unwrap().write_all(spv)?; c.wait_with_output() });
    match out {
        Ok(out) => assert!(out.status.success(), "spirv-val: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
        Err(e) => eprintln!("spirv-val not runnable ({e}) - the independent check did NOT run"),
    }
}

// Both loads read the slot's four words and bitcast each to a float lane.
fn assert_lanes_are_bitcast_words(asm: &str) {
    if std::env::var_os("M2V_SHOW_ASM").is_some() { eprintln!("{asm}"); }
    let count = |op: &str| asm.lines().filter(|l| l.contains(op)).count();
    assert_eq!(count("OpCompositeConstruct"), 2, "one vector per load:\n{asm}");
    assert_eq!(count("OpBitcast"), 8, "four lanes per load:\n{asm}");
}

#[test]
fn a_float4_read_from_a_two_level_header_slot_loads_its_words() {
    let spv = translate(TWO_LEVEL_HEADER);
    assert_lanes_are_bitcast_words(&disassemble(&spv).expect("disassemble"));
    spirv_val(&spv);
}

#[test]
fn a_float4_read_from_a_one_level_header_slot_loads_its_words() {
    let one_level = TWO_LEVEL_HEADER
        .replace("type { { [4 x i32] } }", "type { [4 x i32] }")
        .replace("i64 0, i32 0, i32 0, i64 1", "i64 0, i32 0, i64 1");
    assert!(one_level.contains("type { [4 x i32] }"), "the one-level header was not substituted");
    let spv = translate(&one_level);
    assert_lanes_are_bitcast_words(&disassemble(&spv).expect("disassemble"));
    spirv_val(&spv);
}
