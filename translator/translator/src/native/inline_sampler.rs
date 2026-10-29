//! Samplers written into a call as an integer constant.
//!
//! Current Metal toolchains give every `constexpr sampler` a module-level constant (`@__air_sampler_state.N`) and list it
//! in the `!air.sampler_states` metadata; reflection and the sampler pass both start from that list. Older toolchains
//! (seen: "Apple LLVM version 902.14", SDK 10.15.4, Machinarium) write the same packed state word straight into the
//! call instead:
//!
//! ```llvm
//! call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %tex,
//!     ptr addrspace(2) inttoptr (i64 -9223372036854248814 to ptr addrspace(2)), <2 x float> %uv, ...)
//! ```
//!
//! Nothing downstream recognises that operand, so the whole fragment function was refused with "sampler operand is a
//! pointer whose exact sampler state was not recovered". The word has the same layout as the current one (bit 63 set,
//! address modes in bits 0-8, filters in 9-12, ...; the LOD clamp field is empty, which `StaticSamplerState` reads as a
//! maximum LOD of 0 -- harmless for a sampler without a mip filter, and the only reading of an unspecified field).
//!
//! This rewrites such a call into the current form -- a global per distinct word plus its metadata node -- so every
//! later step (reflection, binding allocation, the sampler pass, the plugin's sampler creation) sees an ordinary static
//! sampler and nothing else needs to know the old spelling. Modules that already carry `!air.sampler_states`, and
//! integers whose bit 63 is clear, are left exactly as they were.

use std::borrow::Cow;

const GLOBAL_PREFIX: &str = "__air_sampler_state";
const CONST_OPEN: &str = "inttoptr (i64 ";
const CONST_CLOSE: &str = " to ptr addrspace(2))";

pub(crate) fn lower_inline_sampler_constants(san_ll: &str) -> Cow<'_, str> {
    if !san_ll.contains(CONST_OPEN)
        || san_ll.contains("!air.sampler_states")
        || san_ll.contains(GLOBAL_PREFIX)
    {
        return Cow::Borrowed(san_ll);
    }
    let mut words: Vec<i64> = Vec::new();
    let mut body = String::with_capacity(san_ll.len() + 512);
    for line in san_ll.split_inclusive('\n') {
        match rewrite_call_line(line, &mut words) {
            Some(rewritten) => body.push_str(&rewritten),
            None => body.push_str(line),
        }
        // stop scanning as soon as the cap below is passed: the word search is linear in the words seen
        if words.len() > 16 {
            return Cow::Borrowed(san_ll);
        }
    }
    // a stage has 16 sampler indices; more distinct words than that is not a shader this rewrite is for
    if words.is_empty() || words.len() > 16 {
        return Cow::Borrowed(san_ll);
    }
    let Some(globals_at) = first_function_line_offset(&body) else {
        return Cow::Borrowed(san_ll);
    };

    let mut globals = String::new();
    for (index, word) in words.iter().enumerate() {
        globals.push_str(&format!(
            "@{GLOBAL_PREFIX}.{index} = internal addrspace(2) constant i64 {word}, align 8\n"
        ));
    }
    body.insert_str(globals_at, &globals);

    if !body.ends_with('\n') {
        body.push('\n');
    }
    let Some(mut node) = next_metadata_id(&body) else {
        return Cow::Borrowed(san_ll);
    };
    if node.checked_add(words.len() as u64).is_none() {
        return Cow::Borrowed(san_ll);
    }
    let mut refs = Vec::with_capacity(words.len());
    let mut nodes = String::new();
    for index in 0..words.len() {
        nodes.push_str(&format!(
            "!{node} = !{{!\"air.sampler_state\", ptr addrspace(2) @{GLOBAL_PREFIX}.{index}}}\n"
        ));
        refs.push(format!("!{node}"));
        node += 1;
    }
    body.push_str(&format!("!air.sampler_states = !{{{}}}\n", refs.join(", ")));
    body.push_str(&nodes);
    Cow::Owned(body)
}

/// Only the sampling and gathering intrinsics take a sampler, and in all of them it is the one `ptr addrspace(2)`
/// operand, so the constant is replaced only on a call line to one of them.
fn is_sampling_call(line: &str) -> bool {
    line.contains("call ")
        && (line.contains("@air.sample_")
            || line.contains("@air.gather_")
            || line.contains("@air.calculate_clamped_lod")
            || line.contains("@air.calculate_unclamped_lod"))
}

fn rewrite_call_line(line: &str, words: &mut Vec<i64>) -> Option<String> {
    if !line.contains(CONST_OPEN) || !is_sampling_call(line) {
        return None;
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut changed = false;
    while let Some(at) = rest.find(CONST_OPEN) {
        let after = &rest[at + CONST_OPEN.len()..];
        let Some(end) = after.find(CONST_CLOSE) else {
            break;
        };
        match after[..end].parse::<i64>() {
            // bit 63 is the marker every packed sampler state carries; any other integer is not one
            Ok(word) if (word as u64) >> 63 == 1 => {
                let index = match words.iter().position(|seen| *seen == word) {
                    Some(index) => index,
                    None => {
                        words.push(word);
                        words.len() - 1
                    }
                };
                out.push_str(&rest[..at]);
                out.push_str(&format!("@{GLOBAL_PREFIX}.{index}"));
                rest = &after[end + CONST_CLOSE.len()..];
                changed = true;
            }
            _ => {
                out.push_str(&rest[..at + CONST_OPEN.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// Byte offset of the first top-level `define`/`declare`: module-level constants are written before the functions.
fn first_function_line_offset(ll: &str) -> Option<usize> {
    let mut offset = 0;
    for line in ll.split_inclusive('\n') {
        if line.starts_with("define ") || line.starts_with("declare ") {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}

/// One past the highest numbered metadata node; None when that would not fit (the module is left as it was).
fn next_metadata_id(ll: &str) -> Option<u64> {
    ll.lines()
        .filter_map(|line| {
            let node = line.trim_start().strip_prefix('!')?;
            let digits: String = node.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() || !node[digits.len()..].starts_with(" = ") {
                return None;
            }
            digits.parse::<u64>().ok()
        })
        .max()
        .map_or(Some(0), |max| max.checked_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME_WORD: i64 = -9223372036854248814;

    fn legacy_module(call_sampler: &str) -> String {
        format!(
            "target triple = \"air64-apple-macosx10.12.0\"\n\
             \n\
             define <4 x float> @frag(<2 x float> %uv, ptr addrspace(1) %tex) {{\n\
             \x20 %s = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) readonly captures(none) %tex, ptr addrspace(2) readonly captures(none) {call_sampler}, <2 x float> %uv, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00)\n\
             \x20 %c = tail call <4 x float> @air.convert.f.v4f32.f.v4f16(<4 x half> %s)\n\
             \x20 ret <4 x float> %c\n\
             }}\n\
             \n\
             declare <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1), ptr addrspace(2), <2 x float>, i1, <2 x i32>, i1, float)\n\
             \n\
             !air.fragment = !{{!0}}\n\
             !0 = !{{ptr @frag, !1, !1}}\n\
             !1 = !{{}}\n"
        )
    }

    fn inline_constant(word: i64) -> String {
        format!("inttoptr (i64 {word} to ptr addrspace(2))")
    }

    #[test]
    fn an_inline_sampler_word_becomes_a_global_and_a_metadata_entry() {
        let ll = legacy_module(&inline_constant(GAME_WORD));
        let lowered = lower_inline_sampler_constants(&ll);
        let lowered = lowered.as_ref();
        assert!(!lowered.contains("inttoptr"), "{lowered}");
        assert!(
            lowered.contains(&format!(
                "@__air_sampler_state.0 = internal addrspace(2) constant i64 {GAME_WORD}, align 8"
            )),
            "{lowered}"
        );
        assert!(
            lowered.contains("ptr addrspace(2) readonly captures(none) @__air_sampler_state.0,"),
            "{lowered}"
        );
        // the new node takes the next free id (the module's highest is !1)
        assert!(lowered.contains("!air.sampler_states = !{!2}"), "{lowered}");
        assert!(
            lowered.contains(
                "!2 = !{!\"air.sampler_state\", ptr addrspace(2) @__air_sampler_state.0}"
            ),
            "{lowered}"
        );
        // constants go before the first function
        let global_at = lowered.find("@__air_sampler_state.0 =").unwrap();
        assert!(global_at < lowered.find("define ").unwrap(), "{lowered}");
    }

    #[test]
    fn the_rewrite_is_idempotent() {
        let ll = legacy_module(&inline_constant(GAME_WORD));
        let once = lower_inline_sampler_constants(&ll).into_owned();
        assert!(matches!(
            lower_inline_sampler_constants(&once),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn equal_words_share_one_global_and_distinct_words_are_numbered_in_order() {
        let other: i64 = -9223372036854248887;
        let mut ll = legacy_module(&inline_constant(GAME_WORD));
        let extra = format!(
            "  %t = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) {}, <2 x float> %uv, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00)\n  %u = tail call <4 x half> @air.sample_texture_2d.v4f16(ptr addrspace(1) %tex, ptr addrspace(2) {}, <2 x float> %uv, i1 true, <2 x i32> zeroinitializer, i1 false, float 0.000000e+00)\n",
            inline_constant(other),
            inline_constant(GAME_WORD)
        );
        ll = ll.replace(
            "  ret <4 x float> %c\n",
            &format!("{extra}  ret <4 x float> %c\n"),
        );
        let lowered = lower_inline_sampler_constants(&ll).into_owned();
        assert_eq!(
            lowered
                .matches("internal addrspace(2) constant i64")
                .count(),
            2,
            "{lowered}"
        );
        assert!(
            lowered.contains("constant i64 -9223372036854248887, align 8"),
            "{lowered}"
        );
        assert_eq!(
            lowered.matches("@__air_sampler_state.0,").count(),
            2,
            "{lowered}"
        );
        assert_eq!(
            lowered.matches("@__air_sampler_state.1,").count(),
            1,
            "{lowered}"
        );
        assert!(
            lowered.contains("!air.sampler_states = !{!2, !3}"),
            "{lowered}"
        );
    }

    #[test]
    fn a_module_with_sampler_metadata_is_left_alone() {
        let mut ll = legacy_module(&inline_constant(GAME_WORD));
        ll.push_str("!air.sampler_states = !{}\n");
        assert!(matches!(
            lower_inline_sampler_constants(&ll),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn integers_that_are_not_sampler_words_are_left_alone() {
        // bit 63 clear
        let ll = legacy_module(&inline_constant(0x1234));
        assert!(matches!(
            lower_inline_sampler_constants(&ll),
            Cow::Borrowed(_)
        ));
        // an address constant in a call that takes no sampler
        let ll = "define void @f() {\n  call void @other(ptr addrspace(2) inttoptr (i64 -9223372036854248814 to ptr addrspace(2)))\n  ret void\n}\n";
        assert!(matches!(
            lower_inline_sampler_constants(ll),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_module_without_inline_constants_is_returned_borrowed() {
        let ll = "define void @f() {\n  ret void\n}\n";
        assert!(matches!(
            lower_inline_sampler_constants(ll),
            Cow::Borrowed(_)
        ));
    }
}
