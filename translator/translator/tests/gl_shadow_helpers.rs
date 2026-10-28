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
