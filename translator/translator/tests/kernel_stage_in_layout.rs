//! A compute pipeline's `[[stage_in]]` attributes are read from the buffer by the stride, offset and format of its stage-input
//! descriptor. The translator used to read each member as an array of its own type at its natural size, so a float3 attribute
//! in a 12-byte stride came out misaligned from the second element on (189 of 192 components wrong in a copy test), and a
//! struct with several attributes needed one buffer per member instead of the one the application bound.
use metal2vulkan::passes::{
    KernelStageIn, StageInAttribute, StageInFormat, StageInLayout, StageInStep, Stage, TransformOptions,
};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

const KERNEL: &str = include_str!("fixtures/gl/kernel_stage_in_three_attributes.ll");

fn layout(stride: u32, offsets: [u32; 3]) -> KernelStageIn {
    let mut s = KernelStageIn::default();
    s.attributes[0] = Some(StageInAttribute { attribute: 0, buffer_index: 1, offset: offsets[0], format: StageInFormat::Float3 });
    s.attributes[1] = Some(StageInAttribute { attribute: 1, buffer_index: 1, offset: offsets[1], format: StageInFormat::UChar4Normalized });
    s.attributes[2] = Some(StageInAttribute { attribute: 2, buffer_index: 1, offset: offsets[2], format: StageInFormat::Float2 });
    s.layouts[0] = Some(StageInLayout { buffer_index: 1, stride, step: StageInStep::GridX, rate: 1 });
    s
}

fn translate(stage_in: KernelStageIn) -> Result<Vec<u8>, String> {
    translate_native_no_retry_constructed_with_options(
        KERNEL,
        Stage::Kernel,
        TransformOptions { kernel_stage_in: stage_in, ..TransformOptions::default() },
    )
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
        assert!(out.status.success(), "spirv-val: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn three_attributes_in_one_24_byte_stride_are_read_from_one_buffer_by_the_descriptor() {
    let spv = translate(layout(24, [0, 12, 16])).unwrap_or_else(|e| panic!("the layout must translate: {e}"));
    let asm = disassemble(&spv).expect("disassemble");
    // the kernel's own output array, and one runtime array of 32-bit words for all three attributes
    assert_eq!(asm.matches("OpTypeRuntimeArray").count(), 2, "{asm}");
    assert!(asm.lines().any(|l| l.trim_end().ends_with("OpConstant  %7  6") || l.contains("OpIMul") && asm.contains("  6\n")), "the element stride is six words:\n{asm}");
    // 3 + 1 + 2 words are loaded, and the packed bytes are unpacked as unorm
    assert!(asm.contains("UnpackUnorm4x8"), "{asm}");
    assert!(asm.matches("OpAccessChain").count() >= 6, "{asm}");
    spirv_val(&spv);
}

#[test]
fn a_12_byte_stride_changes_the_element_step_and_nothing_else() {
    let wide = translate(layout(24, [0, 12, 16])).unwrap();
    let narrow = translate(layout(12, [0, 0, 0])).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(wide, narrow, "the stride is part of the module");
    let asm = disassemble(&narrow).unwrap();
    assert!(asm.contains("  3\n") && asm.contains("OpIMul"), "a 12-byte stride steps three words per element:\n{asm}");
    spirv_val(&narrow);
}

#[test]
fn a_stride_that_is_not_a_whole_number_of_words_is_refused_not_misread() {
    match translate(layout(22, [0, 12, 16])) {
        Ok(spv) => panic!("a 22-byte stride translated ({} bytes)", spv.len()),
        Err(e) => assert!(e.contains("4-byte aligned"), "{e}"),
    }
}

#[test]
fn without_a_descriptor_the_old_layout_stays() {
    let spv = translate(KernelStageIn::default()).unwrap_or_else(|e| panic!("{e}"));
    let asm = disassemble(&spv).unwrap();
    assert!(asm.matches("OpTypeRuntimeArray").count() >= 4, "one array per member and the output as before:\n{asm}");
}
