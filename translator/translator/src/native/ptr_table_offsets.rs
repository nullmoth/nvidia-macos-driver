//! A private table of device or threadgroup pointers into ONE buffer becomes a table of element indices.
//!
//! llama.cpp's matrix-vector kernels (and the reproducer in issue #401) keep a small private array of
//! pointers, each a row of the same input buffer:
//!
//! ```text
//!   %7  = alloca [2 x ptr addrspace(1)], align 8
//!   %30 = getelementptr inbounds half, ptr addrspace(1) %1, i64 %29      ; %1 is a kernel argument
//!   %32 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %7, i64 0, i64 %31
//!   store ptr addrspace(1) %30, ptr %32, align 8
//!   ...
//!   %41 = load ptr addrspace(1), ptr %40, align 8
//!   %56 = getelementptr inbounds half, ptr addrspace(1) %41, i64 %55
//! ```
//!
//! Under Logical addressing a StorageBuffer or Workgroup pointer cannot be stored to Function memory, so
//! the emitter had no model for the table. A device table translated anyway: the stores were dropped and
//! every load through a loaded pointer read a typed zero, so the kernel returned all zeros with no error.
//! A threadgroup table was refused ("an unmodeled pointer placeholder"). Both are measured in #401.
//!
//! Every pointer stored here is `base + index` for the same base, so the table only has to remember the
//! index: the alloca becomes `[N x i64]`, each store writes the index, and each load rebuilds
//! `getelementptr T, base, index`, which is the form the emitter already handles for direct addressing.
//!
//! DELIBERATELY NARROW. A table is rewritten only when all of this holds, and is left to the existing
//! error otherwise:
//!   * its only users are lifetime markers, no-op bitcasts used only by lifetime markers, and
//!     `getelementptr [N x ptr addrspace(K)]` slots (or the alloca itself as slot 0);
//!   * every slot is used only by a whole-pointer store or load of `ptr addrspace(K)`;
//!   * every stored pointer is the base itself or ONE `getelementptr inbounds T, base, i64 idx`, with the
//!     same `T` and the same base for every store;
//!   * that base, after no-op bitcasts, is a function argument or a global, so it dominates every load.

const LIFETIME: [&str; 2] = ["call void @llvm.lifetime.start.p0(", "call void @llvm.lifetime.end.p0("];

/// `%name` as an operand, so `%7` does not match `%70`.
fn uses_value(line: &str, name: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        if after.chars().next().is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.') {
            return true;
        }
        rest = after;
    }
    false
}

/// `  %x = rhs` -> (`%x`, `rhs`).
fn def(line: &str) -> Option<(&str, &str)> {
    let t = line.trim();
    let (name, rhs) = t.split_once(" = ")?;
    (name.starts_with('%') && !name.contains(' ')).then_some((name, rhs))
}

/// The parameter names of a `define` line, read as the `%name` tokens of its parameter list.
fn params(define: &str) -> Vec<String> {
    let Some(open) = define.find('(') else { return Vec::new() };
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in define[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else { return Vec::new() };
    define[open + 1..close]
        .split(|c: char| c == ',' || c == ' ' || c == ')')
        .filter(|t| t.starts_with('%') && t.len() > 1)
        .map(str::to_string)
        .collect()
}

struct Table {
    alloca: usize,
    count: String,
    space: String,
    slots: Vec<(usize, String)>,      // slot GEP lines (and the alloca itself as slot 0)
    stores: Vec<(usize, String)>,     // (line, index operand)
    loads: Vec<(usize, String, String)>, // (line, loaded name, slot name)
    base: String,
    elem: String,
}

/// `alloca [N x ptr addrspace(K)], ...` -> (N, K).
fn table_alloca(rhs: &str) -> Option<(String, String)> {
    let rest = rhs.strip_prefix("alloca [")?;
    let (n, rest) = rest.split_once(" x ptr addrspace(")?;
    let (k, rest) = rest.split_once(")]")?;
    (n.bytes().all(|b| b.is_ascii_digit()) && k.bytes().all(|b| b.is_ascii_digit()) && !k.is_empty())
        .then_some(())?;
    (rest.is_empty() || rest.starts_with(',')).then_some((n.to_string(), k.to_string()))
}

fn analyse(lines: &[&str], span: (usize, usize), alloca: usize, count: &str, space: &str) -> Option<Table> {
    let (name, _) = def(lines[alloca])?;
    let ptr_ty = format!("ptr addrspace({space})");
    let table_ty = format!("[{count} x {ptr_ty}]");
    let defs: std::collections::HashMap<&str, (usize, &str)> =
        (span.0 + 1..span.1).filter_map(|i| def(lines[i]).map(|(n, r)| (n, (i, r)))).collect();
    let roots: Vec<String> = params(lines[span.0]);
    let root_of = |v: &str| -> Option<String> {
        let mut v = v.to_string();
        for _ in 0..8 {
            if v.starts_with('@') || roots.iter().any(|r| *r == v) {
                return Some(v);
            }
            let (_, rhs) = defs.get(v.as_str())?;
            let src = rhs.strip_prefix(&format!("bitcast {ptr_ty} "))?;
            let (src, to) = src.split_once(" to ")?;
            (to.trim() == ptr_ty).then_some(())?;
            v = src.trim().to_string();
        }
        None
    };

    let mut slots = vec![(alloca, name.to_string())];
    let mut views: Vec<usize> = Vec::new();
    for i in span.0 + 1..span.1 {
        if i == alloca || !uses_value(lines[i], name) {
            continue;
        }
        let t = lines[i].trim();
        if LIFETIME.iter().any(|l| t.starts_with(l)) {
            continue;
        }
        let (d, rhs) = def(lines[i])?;
        if let Some(rest) = rhs.strip_prefix("bitcast ptr ") {
            // a no-op view of the table, allowed only if lifetime markers are all it feeds
            (rest.split_once(" to ")?.0.trim() == name).then_some(())?;
            let only_lifetime = (span.0 + 1..span.1).filter(|&j| j != i && uses_value(lines[j], d)).all(|j| {
                let u = lines[j].trim();
                LIFETIME.iter().any(|l| u.starts_with(l))
            });
            only_lifetime.then_some(())?;
            views.push(i);
            continue;
        }
        let gep = rhs
            .strip_prefix("getelementptr inbounds ")
            .or_else(|| rhs.strip_prefix("getelementptr "))?;
        let rest = gep.strip_prefix(&table_ty)?.strip_prefix(", ptr ")?;
        (rest.split(',').next()?.trim() == name).then_some(())?;
        slots.push((i, d.to_string()));
    }

    let mut stores = Vec::new();
    let mut loads = Vec::new();
    let mut base: Option<String> = None;
    let mut elem: Option<String> = None;
    for (si, slot) in &slots {
        for i in span.0 + 1..span.1 {
            if i == *si || !uses_value(lines[i], slot) {
                continue;
            }
            if views.contains(&i) || slots.iter().any(|(j, _)| *j == i) {
                continue; // a slot GEP naming the alloca, handled as its own slot
            }
            let t = lines[i].trim();
            if LIFETIME.iter().any(|l| t.starts_with(l)) && slot == name {
                continue;
            }
            if let Some(rest) = t.strip_prefix(&format!("store {ptr_ty} ")) {
                let (value, rest) = rest.split_once(", ptr ")?;
                (rest.split(',').next()?.trim() == slot).then_some(())?;
                (!uses_value(value, slot)).then_some(())?;
                let (b, e, idx) = if let Some(r) = root_of(value.trim()) {
                    (r, None, "0".to_string())
                } else {
                    let (_, rhs) = defs.get(value.trim())?;
                    let g = rhs.strip_prefix("getelementptr inbounds ")?;
                    let (ty, rest) = g.split_once(&format!(", {ptr_ty} "))?;
                    let (p, idx) = rest.split_once(", i64 ")?;
                    (!idx.contains(',')).then_some(())?;
                    (ty.trim() != "" && !ty.contains(&ptr_ty)).then_some(())?;
                    (root_of(p.trim())?, Some(ty.trim().to_string()), idx.trim().to_string())
                };
                if base.get_or_insert_with(|| b.clone()) != &b {
                    return None;
                }
                if let Some(e) = e {
                    if elem.get_or_insert_with(|| e.clone()) != &e {
                        return None;
                    }
                }
                stores.push((i, idx));
                continue;
            }
            let (d, rhs) = def(lines[i])?;
            let rest = rhs.strip_prefix(&format!("load {ptr_ty}, ptr "))?;
            (rest.split(',').next()?.trim() == slot).then_some(())?;
            loads.push((i, d.to_string(), slot.clone()));
        }
    }
    if stores.is_empty() || loads.is_empty() {
        return None;
    }
    Some(Table {
        alloca,
        count: count.to_string(),
        space: space.to_string(),
        slots,
        stores,
        loads,
        base: base?,
        elem: elem.unwrap_or_else(|| "i8".to_string()),
    })
}

fn rewrite(ll: &str) -> Option<String> {
    if !ll.contains(" x ptr addrspace(") {
        return None;
    }
    let lines: Vec<&str> = ll.lines().collect();
    let mut spans = Vec::new();
    let mut start = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with("define ") {
            start = Some(i);
        } else if t == "}" {
            if let Some(s) = start.take() {
                spans.push((s, i));
            }
        }
    }
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let mut extra: std::collections::HashMap<usize, String> = Default::default();
    let mut changed = false;
    for span in spans {
        for i in span.0 + 1..span.1 {
            let Some((_, rhs)) = def(lines[i]) else { continue };
            let Some((count, space)) = table_alloca(rhs) else { continue };
            let Some(t) = analyse(&lines, span, i, &count, &space) else { continue };
            let ptr_ty = format!("ptr addrspace({})", t.space);
            let old_ty = format!("[{} x {ptr_ty}]", t.count);
            let new_ty = format!("[{} x i64]", t.count);
            out[t.alloca] = lines[t.alloca].replacen(&old_ty, &new_ty, 1);
            for (si, _) in &t.slots {
                if *si != t.alloca {
                    out[*si] = lines[*si].replacen(&old_ty, &new_ty, 1);
                }
            }
            let indent = |l: &str| l.len() - l.trim_start().len();
            for (si, idx) in &t.stores {
                let l = lines[*si];
                let slot = l.trim().split(", ptr ").nth(1).and_then(|r| r.split(',').next()).unwrap_or("").trim();
                out[*si] = format!("{}store i64 {idx}, ptr {slot}, align 8", &l[..indent(l)]);
            }
            for (li, name, slot) in &t.loads {
                let l = lines[*li];
                let pad = &l[..indent(l)];
                let off = format!("%ptrtab.{}", name.trim_start_matches('%'));
                out[*li] = format!("{pad}{off} = load i64, ptr {slot}, align 8");
                extra.insert(
                    *li,
                    format!("{pad}{name} = getelementptr inbounds {}, {ptr_ty} {}, i64 {off}", t.elem, t.base),
                );
            }
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    let mut s = String::with_capacity(ll.len() + 256);
    for (i, l) in out.iter().enumerate() {
        s.push_str(l);
        s.push('\n');
        if let Some(e) = extra.get(&i) {
            s.push_str(e);
            s.push('\n');
        }
    }
    if !ll.ends_with('\n') {
        s.pop();
    }
    Some(s)
}

pub(crate) fn offset_pointer_tables(ll: &str) -> std::borrow::Cow<'_, str> {
    match rewrite(ll) {
        Some(out) => std::borrow::Cow::Owned(out),
        None => std::borrow::Cow::Borrowed(ll),
    }
}

pub(crate) fn offset_pointer_tables_owned(ll: String) -> String {
    match rewrite(&ll) {
        Some(out) => out,
        None => ll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #401 device kernel, reduced to the table: two rows of one buffer stored, one loaded back.
    const ROWS: &str = concat!(
        "define void @probe(ptr addrspace(1) noundef \"air-buffer-no-alias\" %1, i64 %r, i64 %j) {\n",
        "  %7 = alloca [2 x ptr addrspace(1)], align 8\n",
        "  %10 = bitcast ptr %7 to ptr\n",
        "  call void @llvm.lifetime.start.p0(ptr %7)\n",
        "  %30 = getelementptr inbounds half, ptr addrspace(1) %1, i64 %r\n",
        "  %32 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %7, i64 0, i64 %j\n",
        "  store ptr addrspace(1) %30, ptr %32, align 8, !tbaa !41\n",
        "  %40 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %7, i64 0, i64 %j\n",
        "  %41 = load ptr addrspace(1), ptr %40, align 8, !tbaa !41\n",
        "  %56 = getelementptr inbounds half, ptr addrspace(1) %41, i64 %j\n",
        "  %57 = load half, ptr addrspace(1) %56, align 2\n",
        "  call void @llvm.lifetime.end.p0(ptr %7)\n",
        "  ret void\n",
        "}\n",
    );

    #[test]
    fn a_table_of_rows_of_one_buffer_holds_indices() {
        let out = rewrite(ROWS).expect("the #401 form must rewrite");
        assert!(out.contains("%7 = alloca [2 x i64], align 8"), "{out}");
        assert!(out.contains("store i64 %r, ptr %32, align 8\n"), "{out}");
        assert!(out.contains("%ptrtab.41 = load i64, ptr %40, align 8\n"), "{out}");
        assert!(
            out.contains("%41 = getelementptr inbounds half, ptr addrspace(1) %1, i64 %ptrtab.41\n"),
            "the loaded pointer must be rebuilt from the base argument:\n{out}"
        );
        assert!(!out.contains("x ptr addrspace(1)]"), "no pointer table may remain:\n{out}");
    }

    #[test]
    fn a_threadgroup_table_through_a_bitcast_base_holds_indices() {
        let ll = concat!(
            "define void @probe(ptr addrspace(3) noundef %4, i64 %r) {\n",
            "  %10 = alloca [2 x ptr addrspace(3)], align 8\n",
            "  %27 = bitcast ptr addrspace(3) %4 to ptr addrspace(3)\n",
            "  %53 = getelementptr inbounds float, ptr addrspace(3) %27, i64 %r\n",
            "  %55 = getelementptr inbounds [2 x ptr addrspace(3)], ptr %10, i64 0, i64 %r\n",
            "  store ptr addrspace(3) %53, ptr %55, align 8\n",
            "  %56 = getelementptr inbounds float, ptr addrspace(3) %53, i64 %r\n",
            "  %69 = load ptr addrspace(3), ptr %55, align 8\n",
            "  store float 1.0, ptr addrspace(3) %69, align 4\n",
            "  ret void\n",
            "}\n",
        );
        let out = rewrite(ll).expect("the threadgroup form must rewrite");
        assert!(out.contains("%69 = getelementptr inbounds float, ptr addrspace(3) %4, i64 %ptrtab.69"), "{out}");
    }

    #[test]
    fn two_different_buffers_in_one_table_refuse() {
        let ll = concat!(
            "define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, i64 %i) {\n",
            "  %t = alloca [2 x ptr addrspace(1)], align 8\n",
            "  %pa = getelementptr inbounds float, ptr addrspace(1) %a, i64 %i\n",
            "  %pb = getelementptr inbounds float, ptr addrspace(1) %b, i64 %i\n",
            "  %s0 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %t, i64 0, i64 0\n",
            "  %s1 = getelementptr inbounds [2 x ptr addrspace(1)], ptr %t, i64 0, i64 1\n",
            "  store ptr addrspace(1) %pa, ptr %s0, align 8\n",
            "  store ptr addrspace(1) %pb, ptr %s1, align 8\n",
            "  %l = load ptr addrspace(1), ptr %s0, align 8\n",
            "  ret void\n",
            "}\n",
        );
        assert!(rewrite(ll).is_none(), "an index cannot say which buffer it belongs to");
    }

    #[test]
    fn two_element_types_refuse() {
        let ll = ROWS.replace(
            "  %40 = getelementptr",
            "  %31 = getelementptr inbounds float, ptr addrspace(1) %1, i64 %r\n  store ptr addrspace(1) %31, ptr %32, align 8\n  %40 = getelementptr",
        );
        assert!(rewrite(&ll).is_none(), "a half index and a float index do not share a scale");
    }

    #[test]
    fn a_table_that_escapes_refuses() {
        let ll = ROWS.replace("  ret void\n", "  call void @sink(ptr %7)\n  ret void\n");
        assert!(rewrite(&ll).is_none(), "a table passed to a call may be written where this pass cannot see");
    }

    #[test]
    fn a_base_computed_in_the_body_refuses() {
        let ll = ROWS
            .replace("%1, i64 %r, i64 %j)", "%1, i64 %r, i64 %j, i1 %c)")
            .replace(
                "  %30 = getelementptr inbounds half, ptr addrspace(1) %1, i64 %r\n",
                "  %b = select i1 %c, ptr addrspace(1) %1, ptr addrspace(1) %1\n  %30 = getelementptr inbounds half, ptr addrspace(1) %b, i64 %r\n",
            );
        assert!(rewrite(&ll).is_none(), "only an argument or a global is known to dominate every load");
    }

    #[test]
    fn nothing_to_do_is_borrowed() {
        let ll = "define void @k() {\n  ret void\n}\n";
        assert!(matches!(offset_pointer_tables(ll), std::borrow::Cow::Borrowed(_)));
    }
}
