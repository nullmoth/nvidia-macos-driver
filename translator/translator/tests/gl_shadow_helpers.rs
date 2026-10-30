use metal2vulkan::inline_and_forward_object_fields;
use std::collections::HashSet;

#[test]
fn named_helper_entry_is_the_branch_target() {
    let source = "\
define internal float @pick(i32 %mode, float %a, float %b) {
allocaBlock:
  br label %block
block:
  switch i32 %mode, label %after [
    i32 4, label %case
  ]
case:
  br label %after
after:
  %value = phi float [ %a, %block ], [ %b, %case ]
  ret float %value
}
define float @main(i32 %mode, float %a, float %b) {
  %result = call float @pick(i32 %mode, float %a, float %b)
  ret float %result
}
";
    let output = inline_and_forward_object_fields(source).expect("helper is inlined");
    assert!(!output.contains("call float @pick"));
    let labels: HashSet<_> = output.lines().filter_map(|line| line.trim().strip_suffix(':')).collect();
    for (_, rest) in output.match_indices("label %").map(|(at, _)| (at, &output[at + 7..])) {
        let target: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || "._-$".contains(*c)).collect();
        assert!(labels.contains(target.as_str()), "undefined target %{target}:\n{output}");
    }
    assert!(output.contains("br label %.inl0.allocaBlock"));
}

fn sampler_phi(intrinsic: &str) -> String {
    format!("\
define {{ float, i8 }} @main(ptr addrspace(1) %tex, ptr addrspace(2) %s0, ptr addrspace(2) %s1, i1 %mode) {{
entry:
  br i1 %mode, label %other, label %after
other:
  br label %after
after:
  %sampler = phi ptr addrspace(2) [ %s0, %entry ], [ %s1, %other ]
  %result = call {{ float, i8 }} @{intrinsic}(ptr addrspace(1) %tex, ptr addrspace(2) %sampler, i32 1)
  ret {{ float, i8 }} %result
}}
")
}

#[test]
fn compare_samples_are_split_by_sampler_state() {
    let source = sampler_phi("air.sample_compare_depth_2d.f32");
    let output = inline_and_forward_object_fields(&source).expect("sampler phi is split");
    assert!(!output.contains("phi ptr addrspace(2)"));
    assert!(output.contains("phi i32 [ 0, %entry ], [ 1, %other ]"));
    assert!(output.contains("ptr addrspace(2) %s0, i32 1)"));
    assert!(output.contains("ptr addrspace(2) %s1, i32 1)"));
    assert_eq!(output.matches("@air.sample_compare_depth_2d.f32(").count(), 2);
    assert!(output.contains("%result = select i1 "));
}

#[test]
fn unrelated_air_calls_are_not_duplicated() {
    let source = sampler_phi("air.unrelated_side_effect");
    let output = inline_and_forward_object_fields(&source).unwrap_or(source);
    assert!(output.contains("phi ptr addrspace(2)"));
    assert_eq!(output.matches("@air.unrelated_side_effect(").count(), 1);
}

#[test]
fn edge_local_sampler_does_not_become_an_unconditional_read() {
    let source = sampler_phi("air.sample_compare_depth_2d.f32")
        .replace("other:\n", "other:\n  %local = load ptr addrspace(2), ptr null, align 8\n")
        .replace("[ %s1, %other ]", "[ %local, %other ]");
    let output = inline_and_forward_object_fields(&source).unwrap_or(source);
    assert!(output.contains("phi ptr addrspace(2)"));
    assert_eq!(output.matches("@air.sample_compare_depth_2d.f32(").count(), 1);
}

// #449: one field of a local object, written through a bitcast of one GEP and read back through a GEP of another.
fn env_field(read_ty: &str) -> String {
    format!("\
%Envs = type {{ {{ ptr, ptr }}, [2 x {{ ptr, ptr }}] }}
define ptr addrspace(2) @main(ptr byval([2 x ptr addrspace(2)]) %samplers) {{
  %envs = alloca %Envs, align 8
  %array = getelementptr inbounds %Envs, ptr %envs, i64 0, i32 1
  %slot = getelementptr inbounds %Envs, ptr %envs, i64 0, i32 1, i64 1, i32 1
  %slot.cast = bitcast ptr %slot to ptr
  %element = getelementptr inbounds [2 x ptr addrspace(2)], ptr %samplers, i64 0, i64 1
  store ptr %element, ptr %slot.cast, align 8
  %env = getelementptr inbounds [2 x {{ ptr, ptr }}], ptr %array, i64 0, i64 1
  %field = getelementptr inbounds {read_ty}, ptr %env, i64 0, i32 1
  %sampler.ptr = load ptr, ptr %field, align 8
  %wrapped = getelementptr inbounds {{ ptr addrspace(2) }}, ptr %sampler.ptr, i64 0, i32 0
  %sampler = load ptr addrspace(2), ptr %wrapped, align 8
  ret ptr addrspace(2) %sampler
}}
")
}

#[test]
fn a_field_read_through_another_spelling_of_its_address_is_forwarded() {
    let output = inline_and_forward_object_fields(&env_field("{ ptr, ptr }")).expect("the field is forwarded");
    assert!(!output.contains("%sampler.ptr = load"), "the env read must take the stored element:\n{output}");
    assert!(
        output.contains("%wrapped = getelementptr inbounds [2 x ptr addrspace(2)], ptr %samplers, i64 0, i64 1"),
        "the element is one GEP on the byval array, through its wrapper:\n{output}"
    );
}

#[test]
fn a_read_whose_type_is_not_the_field_layout_is_not_forwarded() {
    // member 1 of `{ i32, i32, ptr }` is at byte 4, member 1 of `{ ptr, ptr }` at byte 8: the same index path, another
    // field, so it must not meet the store
    let output = inline_and_forward_object_fields(&env_field("{ i32, i32, ptr }")).unwrap_or_default();
    assert!(
        output.is_empty() || output.contains("%sampler.ptr = load ptr, ptr %field"),
        "a differently laid out read must keep its load:\n{output}"
    );
}
