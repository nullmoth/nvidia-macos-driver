use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

const KERNEL: &str = r#"
target triple = "air64-apple-macosx11.0.0"
define void @metadata_probe(ptr addrspace(1) %out) {
entry:
  %input = load i32, ptr addrspace(1) %out
  %condition = icmp eq i32 %input, 0
  br i1 %condition, label %left, label %right
left:
  br label %merge
right:
  br label %merge
merge:
  %value = phi i32 [ 7, %left ], [ %input, %right ]ATTACHMENT
  %pair = insertvalue { i32, i32 } undef, i32 %value, 0ATTACHMENT
  %result = extractvalue { i32, i32 } %pair, 0ATTACHMENT
  %wide = zext i32 %result to i64ATTACHMENT
  %narrow = trunc i64 %wide to i32ATTACHMENT
  %vector = insertelement <2 x i32> undef, i32 %narrow, i32 0ATTACHMENT
  %splat = shufflevector <2 x i32> %vector, <2 x i32> undef, <2 x i32> zeroinitializerATTACHMENT
  %scalar = extractelement <2 x i32> %splat, i32 1ATTACHMENT
  store i32 %scalar, ptr addrspace(1) %out
  ret voidATTACHMENT
}
!air.kernel = !{!0}
!0 = !{ptr @metadata_probe, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!20 = !{}
!21 = !{i32 1}
"#;

#[test]
fn phi_and_aggregate_metadata_preserve_translated_instructions() {
    let translate = |attachment| {
        let source = KERNEL.replace("ATTACHMENT", attachment);
        let spv = translate_native_no_retry_constructed_with_options(
            &source, Stage::Kernel, TransformOptions::default(),
        ).expect("instruction metadata must not become an operand");
        disassemble(&spv).expect("SPIR-V disassembles")
    };
    let baseline = translate("");
    assert!(baseline.contains("OpStore"));
    assert!(baseline.contains("OpPhi"));
    assert_eq!(translate(", !dbg !20"), baseline);
    assert_eq!(translate(", !dbg !20, !custom !21"), baseline);
}
