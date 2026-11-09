//! Kernels compiled by the AIR 2.9 toolchain carry one more i32 operand on every atomic intrinsic (ahead of the
//! trailing volatile flag). Roblox and Adobe's compute shaders arrive in that form and were refused with
//! "air.atomic.global.add.u.i32 expects 5 operands". The fixtures are the same kernel built for AIR 2.8 and 2.9.
use metal2vulkan::passes::Stage;
use metal2vulkan::{disassemble, translate_sanitized_native};
use std::path::PathBuf;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("m2v_air29_atomic_operands_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

const V28: &str = include_str!("fixtures/air29/atomics_v28.ll");
const V29: &str = include_str!("fixtures/air29/atomics_v29.ll");

fn asm(ll: &str) -> Result<String, String> {
    let spv = translate_sanitized_native(ll, Stage::Kernel, &tmp())?;
    disassemble(&spv).map_err(|e| e.to_string())
}

/// SPIR-V text with the ids renumbered, so two modules compare by structure.
fn shape(asm: &str) -> Vec<String> {
    asm.lines()
        .map(|l| l.split_whitespace().map(|w| if w.starts_with('%') { "%" } else { w }).collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn the_air28_atomic_kernel_translates() {
    let a = asm(V28).expect("AIR 2.8 atomics translate");
    assert!(a.contains("OpAtomicIAdd") && a.contains("OpAtomicCompareExchange") && a.contains("OpAtomicLoad"), "{a}");
}

#[test]
fn the_air29_atomic_kernel_with_the_extra_operand_translates() {
    let a = asm(V29).expect("AIR 2.9 atomics translate");
    for op in ["OpAtomicIAdd", "OpAtomicISub", "OpAtomicSMax", "OpAtomicUMin", "OpAtomicAnd", "OpAtomicOr", "OpAtomicXor",
               "OpAtomicExchange", "OpAtomicCompareExchange", "OpAtomicStore", "OpAtomicLoad", "OpAtomicFAddEXT"] {
        assert!(a.contains(op), "{op} is missing:\n{a}");
    }
}

#[test]
fn both_toolchains_lower_the_same_kernel_to_the_same_structure() {
    assert_eq!(shape(&asm(V28).unwrap()), shape(&asm(V29).unwrap()));
}
