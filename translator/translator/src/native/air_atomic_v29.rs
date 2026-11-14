//! The AIR 2.9 spelling of `air.atomic.{global,local}.*`.
//!
//! The toolchain for macOS 27 (target `air64_v29-apple-macosx27.0.0`) inserts one more `i32` operand before the trailing
//! volatile flag of every global and local atomic, and declares it the same way:
//!
//! ```llvm
//! ; AIR 2.7 / 2.8
//! declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i1)
//! ; AIR 2.9
//! declare i32 @air.atomic.global.add.u.i32(ptr addrspace(1), i32, i32, i32, i32, i1)
//! ```
//!
//! The operand is the call's `mem_flags` (none 0, device 1, threadgroup 2, device|threadgroup 3, texture 4): MSL 4.1
//! lets `atomic_*_explicit` take a memory order and flags, and the memory_order operand then carries acquire (2),
//! release (3), acq_rel (4) or seq_cst (5) instead of only relaxed (0). Measured with `-std=metal4.1`; earlier
//! standards always emit 0 there. The emitter's operand-count checks refused every atomic in a shader built for the
//! default macOS 27 target. This pass drops the operand, which gives the older spelling the emitter already handles; a
//! non-zero value is first folded into each memory_order literal above bit 8 (`order | flags << 8`), where
//! `atomic_requested_semantics` reads it back.
//!
//! `air.atomic.fence` is unchanged in 2.9 and is not touched.

use std::borrow::Cow;

const PREFIX: &str = "@air.atomic.";

/// Operand count of the pre-2.9 spelling, or None for a callee this pass does not rewrite.
fn old_arity(callee: &str) -> Option<usize> {
    let rest = callee.strip_prefix("air.atomic.")?;
    if !(rest.starts_with("global.") || rest.starts_with("local.")) {
        return None;
    }
    // ptr, [value,] order, scope, volatile: load has no value; compare-exchange has expected (a pointer), desired, and
    // a second (failure) order
    Some(if rest.contains(".load.") {
        4
    } else if rest.contains(".cmpxchg.") {
        7
    } else {
        5
    })
}

pub(crate) fn lower_air29_atomics(san_ll: &str) -> Cow<'_, str> {
    // wait and notify are rewritten first, into a load loop that this pass then puts in the older spelling
    match super::air_atomic_wait::lower_wait_notify(san_ll) {
        Cow::Owned(waits) => Cow::Owned(lower_air29_atomics_spelling(&waits).into_owned()),
        Cow::Borrowed(_) => lower_air29_atomics_spelling(san_ll),
    }
}

fn lower_air29_atomics_spelling(san_ll: &str) -> Cow<'_, str> {
    if !san_ll.contains(PREFIX) {
        return Cow::Borrowed(san_ll);
    }
    let rewritten: Vec<Option<String>> = san_ll.split_inclusive('\n').map(rewrite_line).collect();
    if rewritten.iter().all(Option::is_none) {
        return Cow::Borrowed(san_ll);
    }
    let mut out = String::with_capacity(san_ll.len());
    for (line, new) in san_ll.split_inclusive('\n').zip(rewritten) {
        out.push_str(new.as_deref().unwrap_or(line));
    }
    Cow::Owned(out)
}

/// Positions of the memory_order operands in the pre-2.9 spelling: load (ptr, order, scope, volatile),
/// compare-exchange (ptr, expected, desired, success, failure, scope, volatile), everything else (ptr, value, order, ...).
fn order_positions(callee: &str) -> &'static [usize] {
    if callee.contains(".load.") {
        &[1]
    } else if callee.contains(".cmpxchg.") {
        &[3, 4]
    } else {
        &[2]
    }
}

fn rewrite_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    let is_call = t.contains("call ");
    let is_declare = t.starts_with("declare ");
    if !is_call && !is_declare {
        return None;
    }
    let at = line.find(PREFIX)?;
    let name_end = at + line[at..].find('(')?;
    let callee = &line[at + 1..name_end];
    let arity = old_arity(callee)?;
    let open = name_end;
    let close = matching_paren(line, open)?;
    let mut args = super::air_text::split_args(&line[open + 1..close]);
    if args.len() != arity + 1 {
        return None;
    }
    let extra = args.len() - 2;
    if is_declare {
        if args[extra].trim() != "i32" {
            return None;
        }
    } else {
        // the operand is mem_flags (device 1, threadgroup 2, texture 4); a shader that names none carries 0
        let flags: i64 = args[extra].trim().strip_prefix("i32 ")?.trim().parse().ok()?;
        if !(0..=255).contains(&flags) {
            return None;
        }
        if flags != 0 {
            // Fold it into the order operand(s) above bit 8 so the emitter reads order and flags from one literal and
            // every other consumer of the call keeps its older operand count.
            for &i in order_positions(callee) {
                let order: i64 = args[i].trim().strip_prefix("i32 ")?.trim().parse().ok()?;
                if !(0..=255).contains(&order) {
                    return None;
                }
                args[i] = format!("i32 {}", order | (flags << 8));
            }
        }
    }
    args.remove(extra);
    Some(format!("{}{}{}", &line[..=open], args.join(", "), &line[close..]))
}

fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, ch) in s[open..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const V29: &str = "declare i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* nocapture, i32, i32, i32, i32, i1) local_unnamed_addr #2\n\
  %13 = tail call i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* nocapture %12, i32 %11, i32 0, i32 2, i32 0, i1 true) #4\n";

    #[test]
    fn drops_the_extra_operand_from_call_and_declare() {
        let out = lower_air29_atomics(V29);
        assert!(out.contains("(i32 addrspace(1)* nocapture, i32, i32, i32, i1)"), "{out}");
        assert!(
            out.contains("(i32 addrspace(1)* nocapture %12, i32 %11, i32 0, i32 2, i1 true) #4"),
            "{out}"
        );
    }

    #[test]
    fn load_and_compare_exchange_use_their_own_arity() {
        let ll = "  %a = call i32 @air.atomic.global.load.i32(i32 addrspace(1)* %p, i32 0, i32 2, i32 0, i1 true)\n\
  %b = call i32 @air.atomic.global.cmpxchg.weak.i32(i32 addrspace(1)* %p, i32* %e, i32 3, i32 0, i32 0, i32 2, i32 0, i1 true)\n";
        let out = lower_air29_atomics(ll);
        assert!(out.contains("load.i32(i32 addrspace(1)* %p, i32 0, i32 2, i1 true)"), "{out}");
        assert!(
            out.contains("cmpxchg.weak.i32(i32 addrspace(1)* %p, i32* %e, i32 3, i32 0, i32 0, i32 2, i1 true)"),
            "{out}"
        );
    }

    #[test]
    fn older_spelling_is_untouched() {
        let ll = "  %13 = tail call i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* %12, i32 %11, i32 0, i32 2, i1 true)\n";
        assert!(matches!(lower_air29_atomics(ll), Cow::Borrowed(_)));
    }

    #[test]
    fn mem_flags_are_folded_into_every_order_operand() {
        let ll = "  %13 = call i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* %12, i32 %11, i32 4, i32 2, i32 1, i1 false)\n\
  %b = call i32 @air.atomic.global.cmpxchg.weak.i32(i32 addrspace(1)* %p, i32* %e, i32 3, i32 4, i32 2, i32 2, i32 3, i1 false)\n";
        let out = lower_air29_atomics(ll);
        assert!(out.contains("i32 %11, i32 260, i32 2, i1 false)"), "{out}");
        assert!(out.contains("i32 3, i32 772, i32 770, i32 2, i1 false)"), "{out}");
    }

    #[test]
    fn an_out_of_range_flags_operand_is_left_alone() {
        let ll = "  %13 = call i32 @air.atomic.global.add.u.i32(i32 addrspace(1)* %12, i32 %11, i32 0, i32 2, i32 700, i1 true)\n";
        assert!(matches!(lower_air29_atomics(ll), Cow::Borrowed(_)));
    }

    #[test]
    fn fence_is_not_rewritten() {
        let ll = "  call void @air.atomic.fence(i32 1, i32 5, i32 2)\n";
        assert!(matches!(lower_air29_atomics(ll), Cow::Borrowed(_)));
    }
}
