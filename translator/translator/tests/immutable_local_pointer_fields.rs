use metal2vulkan::{inline_and_forward_object_fields, translate_native_no_retry_constructed_with_options};
use metal2vulkan::passes::{Stage, TransformOptions};

const DOMINATING: &str = include_str!("fixtures/local_pointer_fields/dominating_nonentry.ll");

fn assert_keeps_pointer_load(input: &str, name: &str) {
    let output = inline_and_forward_object_fields(input).unwrap_or_else(|| input.to_string());
    assert!(output.contains(name), "a possibly clobbered or uninitialized field must remain a load:\n{output}");
}

#[test]
fn a_nonentry_store_dominating_both_loop_loads_preserves_the_local_root() {
    let output = inline_and_forward_object_fields(DOMINATING).expect("the immutable field must forward");
    for load in ["%first = load ptr", "%later = load ptr"] {
        assert!(!output.contains(load), "{output}");
    }
    let bytes = translate_native_no_retry_constructed_with_options(&output, Stage::Kernel, TransformOptions::default())
        .expect("the two loop-edge pointers now have the same real alloca source");
    let scratch = std::env::temp_dir().join(format!("m2v_immutable_fields_{}", std::process::id()));
    let verdict = metal2vulkan::tools::spirv_val_bytes(&bytes, &scratch);
    let _ = std::fs::remove_dir_all(scratch);
    verdict.expect("the forwarded kernel must validate as Vulkan 1.2 SPIR-V");
}

#[test]
fn a_store_on_only_one_predecessor_does_not_initialize_the_field() {
    assert_keeps_pointer_load(include_str!("fixtures/local_pointer_fields/branch_store_does_not_dominate.ll"), "%first = load ptr");
}

#[test]
fn a_second_store_through_an_equivalent_gep_prevents_forwarding() {
    assert_keeps_pointer_load(include_str!("fixtures/local_pointer_fields/second_alias_store.ll"), "%later = load ptr");
}

#[test]
fn equivalent_geps_with_different_index_widths_still_alias() {
    let input = include_str!("fixtures/local_pointer_fields/second_alias_store.ll")
        .replace("%field_alias = getelementptr inbounds %Capture, ptr %object, i64 0", "%field_alias = getelementptr inbounds %Capture, ptr %object, i32 0");
    assert_keeps_pointer_load(&input, "%first = load ptr");
}

#[test]
fn a_dynamic_gep_may_alias_a_constant_pointer_field() {
    assert_keeps_pointer_load(include_str!("fixtures/local_pointer_fields/dynamic_gep.ll"), "%first = load ptr");
}

#[test]
fn a_memset_of_the_object_clobbers_its_pointer_fields() {
    assert_keeps_pointer_load(include_str!("fixtures/local_pointer_fields/memset_escape.ll"), "%first = load ptr");
}

#[test]
fn an_aggregate_store_clobbers_the_pointer_leaf() {
    let input = DOMINATING.replace("  %first = load ptr", "  store %Capture { i32 0, ptr %b }, ptr %object, align 8\n  %first = load ptr");
    assert_keeps_pointer_load(&input, "%first = load ptr");
}

#[test]
fn an_unknown_call_is_not_a_lifetime_marker_because_of_its_comment() {
    let input = DOMINATING.replace("define void @main", "declare void @clobber(ptr)\ndefine void @main")
        .replace("  %first = load ptr", "  call void @clobber(ptr %object) ; @llvm.lifetime.start.p0\n  %first = load ptr");
    assert_keeps_pointer_load(&input, "%first = load ptr");
}

#[test]
fn a_volatile_pointer_load_remains_observable() {
    let input = DOMINATING.replace("%first = load ptr", "%first = load volatile ptr");
    assert_keeps_pointer_load(&input, "%first = load volatile ptr");
}

#[test]
fn a_pointer_load_in_another_address_space_is_not_the_stored_ssa_value() {
    let input = DOMINATING
        .replace("%Capture = type { i32, ptr }", "%Capture = type { i32, ptr addrspace(2) }")
        .replace("load ptr, ptr %field", "load ptr addrspace(2), ptr %field")
        .replace("phi ptr [", "phi ptr addrspace(2) [")
        .replace("load i32, ptr %p", "load i32, ptr addrspace(2) %p");
    assert_keeps_pointer_load(&input, "%first = load ptr addrspace(2)");
    assert_keeps_pointer_load(&input, "%later = load ptr addrspace(2)");
}

#[test]
fn quoted_address_or_block_names_decline_the_new_alias_proof() {
    let input = DOMINATING.replace("%field", "%\"field\"")
        .replace("  %first = load ptr", "  %alias = bitcast ptr %\"field\" to ptr\n  store ptr %b, ptr %alias, align 8\n  %first = load ptr");
    assert_keeps_pointer_load(&input, "%first = load ptr");
    let input = include_str!("fixtures/local_pointer_fields/branch_store_does_not_dominate.ll")
        .replace("%setup", "%\"setup\"").replace("setup:", "\"setup\":");
    assert_keeps_pointer_load(&input, "%first = load ptr");
}
