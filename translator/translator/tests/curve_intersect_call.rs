//! The curve intersection call, from a kernel the Metal compiler produced (fixtures/curves/curve_bezier_round.ll: an
//! `intersector<curve_data, instancing>` with a cubic round Bezier assumption, reading distance, curve_parameter and primitive_id).
//! The call must become a ray query whose result reads the committed hit's first barycentric as curve_parameter.
use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, tools, translate_native_no_retry_constructed_with_options};

const CURVE: &str = include_str!("fixtures/curves/curve_bezier_round.ll");
const TRIANGLE: &str = include_str!("fixtures/hwrt/rt3-rq.ll");

fn asm_of(spv: &[u8]) -> String {
    disassemble(spv).expect("disassemble").lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join("\n")
}

fn translate(ll: &str) -> Vec<u8> {
    translate_native_no_retry_constructed_with_options(ll, Stage::Kernel, TransformOptions::default()).expect("translates")
}

#[test]
fn curve_call_becomes_a_ray_query_that_reads_the_curve_parameter_from_the_first_barycentric() {
    let spv = translate(CURVE);
    let asm = asm_of(&spv);
    assert!(asm.contains("OpRayQueryInitializeKHR"), "{asm}");
    let bary = asm
        .lines()
        .find(|l| l.contains("OpRayQueryGetIntersectionBarycentricsKHR"))
        .unwrap_or_else(|| panic!("no barycentrics read for curve_parameter\n{asm}"));
    let id = bary.split_whitespace().next().unwrap().to_string();
    assert!(
        asm.lines().any(|l| l.contains("OpCompositeExtract") && l.contains(&id) && l.trim_end().ends_with(" 0")),
        "the first component of {id} must be extracted\n{asm}"
    );
    assert!(asm.contains("OpRayQueryGetIntersectionPrimitiveIndexKHR"), "{asm}");
    assert!(asm.contains("OpRayQueryGetIntersectionTKHR"), "{asm}");
    let dir = std::env::temp_dir().join(format!("m2v_curve_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    tools::spirv_val_bytes(&spv, &dir).expect("spirv-val");
}

#[test]
fn control_a_curve_call_through_an_intersection_function_table_is_still_refused_by_name() {
    let ll = CURVE.replace("@air.intersect.instancing.curve_data", "@air.intersect.intersection_function_buffer.instancing.curve_data");
    let err = translate_native_no_retry_constructed_with_options(&ll, Stage::Kernel, TransformOptions::default())
        .expect_err("a curve call with an intersection function table is not carried");
    assert!(err.contains("not carried"), "{err}");
}

#[test]
fn control_motion_curves_are_still_refused_by_name() {
    let ll = CURVE.replace("@air.intersect.instancing.curve_data", "@air.intersect.instancing.curve_data.primitive_motion");
    let err = translate_native_no_retry_constructed_with_options(&ll, Stage::Kernel, TransformOptions::default())
        .expect_err("motion curves are not carried");
    assert!(err.contains("not carried"), "{err}");
}
