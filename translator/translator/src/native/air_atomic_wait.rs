//! MSL 4.1 atomic wait and notify (`air.atomic.{global,local}.wait[.simdgroup].<ty>` and `...notify.{one,all}...`).
//!
//! macOS 27.2's MPS single-pass scans (`single_pass_scan_*`) publish a tile's status with an atomic store, `notify`, and
//! spin in `wait` on the previous tile's status. Measured spelling (MPSNDArray 27.2, AIR 2.9):
//!
//! ```llvm
//! declare i32 @air.atomic.global.wait.simdgroup.i32(ptr addrspace(1), i32, i32, i32, i32, i1)
//! ;                                                  pointer          expected order scope flags volatile
//! declare void @air.atomic.global.notify.one.simdgroup.i32(ptr addrspace(1), i1)
//! %331 = call i32 @air.atomic.global.wait.simdgroup.i32(ptr addrspace(1) %p, i32 0, i32 2, i32 2, i32 1, i1 false)
//! switch i32 %331, label %350 [ i32 2, label %332  i32 1, label %340 ]   ; the result is the value that ended the wait
//! ```
//!
//! Vulkan has no wait or wake. `wait` returns once the value differs from `expected`, so it is lowered to the loop it
//! stands for: an atomic load (same order, scope, flags, volatile) repeated while it reads `expected`; the value read is
//! the result. `notify` only wakes waiters, and a spinning waiter needs no wake, so it is removed. The wait's block is
//! split, so phis in its successors that named the block now name the loop's exit block.
//!
//! The pass runs on text before the AIR 2.9 operand fold, so the load it writes uses the 2.9 spelling as well.
//! A wait it cannot rewrite exactly (first block of a function, unexpected operand shape) is left as it is, and the
//! emitter refuses it by name.

use std::borrow::Cow;

struct Wait {
    result: String,
    space: &'static str,
    ty: String,
    pointer: String,
    expected: String,
    order: String,
    scope: String,
    flags: String,
    volatile: String,
}

/// `  %r = [tail ]call <ty> @air.atomic.<space>.wait[.simdgroup].<ty>(<ptr operand>, <ty> E, i32 O, i32 S, i32 F, i1 V)...`
fn parse_wait(line: &str) -> Option<Wait> {
    let t = line.trim();
    let (result, rest) = t.split_once(" = ")?;
    let rest = rest.strip_prefix("tail ").unwrap_or(rest);
    let rest = rest.strip_prefix("call ")?;
    let at = rest.find("@air.atomic.")?;
    let after = &rest[at + "@air.atomic.".len()..];
    let (space, after) = if let Some(a) = after.strip_prefix("global.") { ("global", a) }
        else if let Some(a) = after.strip_prefix("local.") { ("local", a) } else { return None };
    let after = after.strip_prefix("wait.")?;
    let after = after.strip_prefix("simdgroup.").unwrap_or(after);
    let open = after.find('(')?;
    let ty = after[..open].to_string();
    let close = after.rfind(')')?;
    let args: Vec<&str> = after[open + 1..close].split(", ").collect();
    if args.len() != 6 || !result.starts_with('%') {
        return None;
    }
    let value = |a: &str, want: &str| a.strip_prefix(want).map(|v| v.trim().to_string());
    Some(Wait {
        result: result.to_string(),
        space,
        expected: value(args[1], &format!("{ty} "))?,
        order: value(args[2], "i32 ")?,
        scope: value(args[3], "i32 ")?,
        flags: value(args[4], "i32 ")?,
        volatile: value(args[5], "i1 ")?,
        pointer: args[0].to_string(),
        ty,
    })
}

fn is_notify(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("tail ").unwrap_or(t);
    t.starts_with("call void @air.atomic.global.notify.") || t.starts_with("call void @air.atomic.local.notify.")
}

/// A block label line (`name:` or `123:`, optionally followed by a `; preds` comment).
fn label_of(line: &str) -> Option<&str> {
    if line.starts_with(' ') || line.starts_with('\t') || line.starts_with(';') {
        return None;
    }
    let head = line.split(';').next()?.trim_end();
    let name = head.strip_suffix(':')?;
    (!name.is_empty() && !name.contains(' ')).then_some(name)
}

pub(crate) fn lower_wait_notify(ll: &str) -> Cow<'_, str> {
    if !ll.contains("@air.atomic.global.wait.") && !ll.contains("@air.atomic.local.wait.")
        && !ll.contains(".notify.")
    {
        return Cow::Borrowed(ll);
    }
    let mut lines: Vec<String> = ll.lines().map(str::to_string).collect();
    let mut changed = false;
    let mut loads_needed: Vec<(String, &'static str, String)> = Vec::new();
    let mut n = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        if is_notify(&lines[i]) {
            lines.remove(i);
            changed = true;
            continue;
        }
        let Some(w) = parse_wait(&lines[i]) else { i += 1; continue };
        // the label of the block holding the wait; the first block of a function has no written label
        let mut j = i;
        let mut block = None;
        while j > 0 {
            j -= 1;
            if lines[j].starts_with("define ") { break; }
            if let Some(name) = label_of(&lines[j]) { block = Some(name.to_string()); break; }
        }
        let Some(block) = block else { i += 1; continue };
        n += 1;
        let loop_l = format!("aw.loop.{n}");
        let done_l = format!("aw.done.{n}");
        let v = format!("%aw.v.{n}");
        let same = format!("%aw.same.{n}");
        let load = format!("@air.atomic.{}.load.{}", w.space, w.ty);
        let replacement = vec![
            format!("  br label %{loop_l}"),
            format!("{loop_l}:"),
            format!("  {v} = call {} {load}({}, i32 {}, i32 {}, i32 {}, i1 {})", w.ty, w.pointer, w.order, w.scope, w.flags, w.volatile),
            format!("  {same} = icmp eq {} {v}, {}", w.ty, w.expected),
            format!("  br i1 {same}, label %{loop_l}, label %{done_l}"),
            format!("{done_l}:"),
            format!("  {} = add {} {v}, 0", w.result, w.ty),
        ];
        let ptr_ty = if w.space == "global" { "ptr addrspace(1)" } else { "ptr addrspace(3)" };
        loads_needed.push((load.clone(), w.space, format!("declare {} {load}({ptr_ty}, i32, i32, i32, i1)", w.ty)));
        lines.splice(i..=i, replacement);
        // successors of the split block now come from the loop's exit block: rename their phi operands
        let mut k = i + 7;
        let mut successors = Vec::new();
        while k < lines.len() && label_of(&lines[k]).is_none() && !lines[k].starts_with('}') {
            let mut rest = lines[k].as_str();
            while let Some(p) = rest.find("label %") {
                let name: String = rest[p + 7..].chars().take_while(|c| c.is_ascii_alphanumeric() || "._-$".contains(*c)).collect();
                successors.push(name);
                rest = &rest[p + 7..];
            }
            k += 1;
        }
        let old = format!("%{block} ]");
        let new = format!("%{done_l} ]");
        let mut m = k;
        while m < lines.len() && !lines[m].starts_with('}') {
            if let Some(name) = label_of(&lines[m]) {
                if successors.iter().any(|s| s == name) {
                    let mut p = m + 1;
                    while p < lines.len() && lines[p].trim_start().contains(" = phi ") {
                        if lines[p].contains(&old) { lines[p] = lines[p].replace(&old, &new); }
                        p += 1;
                    }
                }
            }
            m += 1;
        }
        // a successor that is the split block's own predecessor appears before it
        let mut m = i;
        while m > 0 && !lines[m].starts_with("define ") {
            m -= 1;
            if let Some(name) = label_of(&lines[m]) {
                if successors.iter().any(|s| s == name) {
                    let mut p = m + 1;
                    while p < lines.len() && lines[p].trim_start().contains(" = phi ") {
                        if lines[p].contains(&old) { lines[p] = lines[p].replace(&old, &new); }
                        p += 1;
                    }
                }
            }
        }
        changed = true;
        i += 7;
    }
    if !changed {
        return Cow::Borrowed(ll);
    }
    for (callee, _, decl) in loads_needed {
        if !lines.iter().any(|l| l.starts_with("declare ") && l.contains(&format!("{callee}("))) {
            lines.push(decl);
        }
    }
    let mut out = lines.join("\n");
    if ll.ends_with('\n') { out.push('\n'); }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LL: &str = "define void @k(ptr addrspace(1) %p) {
entry:
  br label %328
328:
  %331 = tail call i32 @air.atomic.global.wait.simdgroup.i32(ptr addrspace(1) captures(none) %p, i32 0, i32 2, i32 2, i32 1, i1 false) #11
  switch i32 %331, label %350 [
    i32 2, label %332
  ]
332:
  %x = phi i32 [ 7, %328 ]
  tail call void @air.atomic.global.notify.one.simdgroup.i32(ptr addrspace(1) captures(none) %p, i1 false) #10
  br label %350
350:
  %y = phi i32 [ 1, %328 ], [ 2, %332 ]
  ret void
}
declare i32 @air.atomic.global.wait.simdgroup.i32(ptr addrspace(1) captures(none), i32, i32, i32, i32, i1)
";

    #[test]
    fn a_wait_becomes_a_load_loop_that_exits_on_a_changed_value() {
        let out = lower_wait_notify(LL);
        assert!(out.contains("aw.loop.1:\n  %aw.v.1 = call i32 @air.atomic.global.load.i32(ptr addrspace(1) captures(none) %p, i32 2, i32 2, i32 1, i1 false)"), "{out}");
        assert!(out.contains("%aw.same.1 = icmp eq i32 %aw.v.1, 0"), "{out}");
        assert!(out.contains("br i1 %aw.same.1, label %aw.loop.1, label %aw.done.1"), "{out}");
        assert!(out.contains("%331 = add i32 %aw.v.1, 0"), "{out}");
        assert!(out.contains("declare i32 @air.atomic.global.load.i32(ptr addrspace(1), i32, i32, i32, i1)"), "{out}");
    }

    #[test]
    fn successor_phis_name_the_loop_exit_and_others_are_untouched() {
        let out = lower_wait_notify(LL);
        assert!(out.contains("%x = phi i32 [ 7, %aw.done.1 ]"), "{out}");
        assert!(out.contains("%y = phi i32 [ 1, %aw.done.1 ], [ 2, %332 ]"), "{out}");
    }

    #[test]
    fn a_notify_is_removed_and_text_without_either_is_borrowed() {
        assert!(!lower_wait_notify(LL).contains("notify"));
        assert!(matches!(lower_wait_notify("define void @k() {\n  ret void\n}\n"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_wait_in_a_functions_first_block_is_left_for_the_emitter_to_refuse() {
        let ll = "define void @k(ptr addrspace(1) %p) {\n  %r = call i32 @air.atomic.global.wait.i32(ptr addrspace(1) %p, i32 0, i32 2, i32 2, i32 1, i1 false)\n  ret void\n}\n";
        assert!(lower_wait_notify(ll).contains("@air.atomic.global.wait.i32"));
    }
}
