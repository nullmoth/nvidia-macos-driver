//! A vector read through a pointer to a struct whose first member is a scalar array of the vector's size.
//!
//! DaVinci Resolve's viewer vertex shader takes a buffer of `struct Slot { packed_uint4 words; }` (LLVM
//! `{ { [4 x i32] } }`) and, after the compiler's scalar replacement, reads a slot as one `load <4 x float>` through the
//! slot struct. The emitter lowered a vector read of a bare scalar array but refused one behind struct wrappers
//! ("cannot reinterpret load ... with non-bitcastable pointee"), the pipeline never built, and the viewer drew nothing.
use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

const WRAPPED: &str = r#"target triple = "air64_v28-apple-macosx26.7.0"

%struct.Slot = type { { [4 x i32] } }

define <{ <4 x float> }> @vs(ptr addrspace(2) readonly %slots, i32 %vid) {
entry:
  %n = zext i32 %vid to i64
  %slot = getelementptr inbounds %struct.Slot, ptr addrspace(2) %slots, i64 %n
  %v = load <4 x float>, ptr addrspace(2) %slot, align 16
  %next = getelementptr inbounds %struct.Slot, ptr addrspace(2) %slots, i64 1
  %w = load <4 x float>, ptr addrspace(2) %next, align 16
  %sum = fadd fast <4 x float> %v, %w
  %ret = insertvalue <{ <4 x float> }> undef, <4 x float> %sum, 0
  ret <{ <4 x float> }> %ret
}

!air.vertex = !{!0}
!air.version = !{!7}
!air.language_version = !{!8}

!0 = !{ptr @vs, !1, !3}
!1 = !{!2}
!2 = !{!"air.position", !"air.arg_type_name", !"float4", !"air.arg_name", !"position"}
!3 = !{!4, !6}
!4 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.struct_type_info", !5, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"Slot", !"air.arg_name", !"slots"}
!5 = !{i32 0, i32 16, i32 0, !"packed_uint4", !"words"}
!6 = !{i32 1, !"air.vertex_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"vid"}
!7 = !{i32 2, i32 8, i32 0}
!8 = !{!"Metal", i32 2, i32 0, i32 0}
"#;

fn translate(ll: &str) -> Result<Vec<u8>, String> {
    translate_native_no_retry_constructed_with_options(ll, Stage::Vertex, TransformOptions::default())
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
        assert!(
            out.status.success(),
            "spirv-val: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn lanes_are_bitcast_words(spv: &[u8]) {
    let asm = disassemble(spv).expect("disassemble");
    let count = |op: &str| asm.lines().filter(|l| l.contains(op)).count();
    assert_eq!(count("OpCompositeConstruct"), 2, "one vector built per load:\n{asm}");
    assert_eq!(count("OpBitcast"), 8, "four int lanes per load become float lanes:\n{asm}");
}

#[test]
fn a_float4_read_of_a_slot_struct_wrapping_four_ints_loads_the_four_words() {
    let spv = translate(WRAPPED).unwrap_or_else(|e| panic!("the read must translate: {e}"));
    lanes_are_bitcast_words(&spv);
    spirv_val(&spv);
}

#[test]
fn a_float4_read_of_a_slot_struct_holding_the_array_one_level_down_loads_the_four_words() {
    let flat = WRAPPED.replace("type { { [4 x i32] } }", "type { [4 x i32] }");
    assert_ne!(flat, WRAPPED);
    let spv = translate(&flat).unwrap_or_else(|e| panic!("the read must translate: {e}"));
    lanes_are_bitcast_words(&spv);
    spirv_val(&spv);
}

#[test]
fn a_float4_read_of_a_slot_struct_whose_array_is_one_lane_short_is_still_refused() {
    // three words cannot supply four lanes: translating this would invent the fourth
    let short = WRAPPED.replace("type { { [4 x i32] } }", "type { { [3 x i32] } }");
    assert_ne!(short, WRAPPED);
    if let Ok(spv) = translate(&short) {
        panic!("a 3-word slot read as float4 translated ({} bytes)", spv.len());
    }
}
