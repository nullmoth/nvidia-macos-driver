use metal2vulkan::passes::{Stage, TransformOptions};
use metal2vulkan::{disassemble, translate_native_no_retry_constructed_with_options};

#[test]
fn vertex_outputs_survive_a_buffer_layout_type_alias() {
    // The return aggregate has the same shape as a nested buffer record. Input
    // layout construction clones that shared type; outputs must use the clone.
    let air = include_str!("fixtures/stage_output/buffer_return_alias.ll");
    let bytes = translate_native_no_retry_constructed_with_options(
        air, Stage::Vertex, TransformOptions::default(),
    ).expect("translate vertex");
    let assembly = disassemble(&bytes).expect("disassemble");
    for decoration in ["BuiltIn Position", "Location 0"] {
        let variable = assembly.lines().find_map(|line| {
            let tail = line.trim().strip_prefix("OpDecorate ")?;
            let (variable, actual) = tail.split_once(' ')?;
            (actual == decoration).then_some(variable)
        }).unwrap_or_else(|| panic!("missing {decoration} output:\n{assembly}"));
        assert!(assembly.lines().any(|line| line.trim().starts_with(
            &format!("OpStore {variable} ")
        )), "{decoration} output is never written");
    }
}
