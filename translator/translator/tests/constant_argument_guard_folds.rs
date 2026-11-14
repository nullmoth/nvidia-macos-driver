//! A state word that reaches a helper function by value must decide the helper's branches.
//!
//! Shape: one `uint4` function constant, a static initializer that splits it into words, an
//! entry point that wraps a word in a single-field struct and passes it to an internal helper,
//! and a helper that calls an unresolved visible function only when a flag bit is set.

use metal2vulkan::passes::{Stage, TransformOptions};

const HEAD: &str = r#"target triple = "air64_v27-apple-macosx15.7.0"

%struct.State = type { i32 }

@state.MTL_FC_INIT_0_Dv4_j = internal unnamed_addr addrspace(2) externally_initialized constant <4 x i32> undef, section "air.fc_initializer", align 16
@word1 = internal unnamed_addr addrspace(2) global i32 0, align 4
@word2 = internal unnamed_addr addrspace(2) global i32 0, align 4

declare void @hook.MTL_UNRESOLVED_VISIBLE_FN(ptr addrspace(1)) local_unnamed_addr section "air.externally_defined"

define internal void @_GLOBAL__sub_I_state() section "air.static_init" {
  %1 = load <4 x i32>, ptr addrspace(2) @state.MTL_FC_INIT_0_Dv4_j, align 16
  %2 = extractelement <4 x i32> %1, i64 1
  store i32 %2, ptr addrspace(2) @word1, align 4
  %3 = extractelement <4 x i32> %1, i64 2
  store i32 %3, ptr addrspace(2) @word2, align 4
  ret void
}

define internal fastcc void @composite(ptr addrspace(1) %out, %struct.State %state) {
entry:
  %word = extractvalue %struct.State %state, 0
  %flag = and i32 %word, 1073741824
  %plain = icmp eq i32 %flag, 0
  br i1 %plain, label %write, label %custom

custom:
  call void @hook.MTL_UNRESOLVED_VISIBLE_FN(ptr addrspace(1) %out)
  br label %write

write:
  store i32 1, ptr addrspace(1) %out, align 4
  ret void
}
"#;

const ONE_SITE: &str = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  %w = load i32, ptr addrspace(2) @word1, align 4
  %s = insertvalue %struct.State poison, i32 %w, 0
  call fastcc void @composite(ptr addrspace(1) %out, %struct.State %s)
  ret void
}
"#;

const TWO_SITES: &str = r#"
define void @k(ptr addrspace(1) %out) {
entry:
  %w1 = load i32, ptr addrspace(2) @word1, align 4
  %s1 = insertvalue %struct.State poison, i32 %w1, 0
  call fastcc void @composite(ptr addrspace(1) %out, %struct.State %s1)
  %w2 = load i32, ptr addrspace(2) @word2, align 4
  %s2 = insertvalue %struct.State poison, i32 %w2, 0
  call fastcc void @composite(ptr addrspace(1) %out, %struct.State %s2)
  ret void
}
"#;

const TAIL: &str = r#"
!air.kernel = !{!0}
!air.function_constants = !{!5}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"int", !"air.arg_name", !"out"}
!5 = !{ptr addrspace(2) @state.MTL_FC_INIT_0_Dv4_j, !"uint4", !"state", i32 0, i1 true}
"#;

const FLAG: u32 = 1 << 30;
const PLAIN: u32 = 147_456;
const LIVE: &str = "is called on a live path";

fn translate(entry: &str, words: [u32; 4]) -> Result<usize, String> {
    let module = format!("{HEAD}{entry}{TAIL}");
    let payload = words.iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<u8>>();
    let specialized = metal2vulkan::specialize_air_function_constants(&module, &[(0, payload)])?;
    metal2vulkan::translate_native_no_retry_constructed_with_options(
        specialized.as_ref(),
        Stage::Kernel,
        TransformOptions::default(),
    )
    .map(|spv| spv.len())
}

#[test]
fn a_clear_flag_in_a_by_value_state_word_folds_the_helpers_visible_call_away() {
    let r = translate(ONE_SITE, [5, PLAIN, 0, 0]);
    assert!(
        matches!(r, Ok(n) if n > 0),
        "flag clear at the only call site => the helper's hook call is dead and must fold: {r:?}"
    );
}

#[test]
fn a_set_flag_keeps_the_helpers_visible_call_live_and_refused() {
    let r = translate(ONE_SITE, [5, PLAIN | FLAG, 0, 0]);
    assert!(
        matches!(&r, Err(e) if e.contains(LIVE)),
        "flag set => the hook call is reachable and must still be refused, got {r:?}"
    );
}

#[test]
fn two_call_sites_passing_the_same_clear_word_still_fold() {
    let r = translate(TWO_SITES, [5, PLAIN, PLAIN, 0]);
    assert!(
        matches!(r, Ok(n) if n > 0),
        "both sites agree on a clear word => the hook call is dead: {r:?}"
    );
}

#[test]
fn a_second_call_site_with_the_flag_set_keeps_the_call_live() {
    let r = translate(TWO_SITES, [5, PLAIN, PLAIN | FLAG, 0]);
    assert!(
        matches!(&r, Err(e) if e.contains(LIVE)),
        "one site sets the flag => the first site's constant must not decide the helper, got {r:?}"
    );
}
