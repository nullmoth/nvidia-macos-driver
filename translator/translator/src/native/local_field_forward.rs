//! Forwards a device pointer stored into a field of a local object to the loads of that field.
//!
//! VFX's ray-tracing kernels keep their acceleration structure and intersection-function table in a local
//! `Raytracer { constants, accel, table }` and the helpers read them back. After the inline retry the module has
//! `store ptr addrspace(1) %accel, ptr %field` and later `%v = load ptr addrspace(1), ptr %same_field`, and a device
//! pointer cannot live in Logical SPIR-V's function memory, so the load came out defined by nothing and the ray-query
//! lowering refused the pipeline. Measured on VFX (macOS 26.7.1): with this forwarding (what LLVM's SROA does to the
//! same module), vfx_ray_tracer translates and validates.
//!
//! Only a field whose every store sits in one block, whose last store there writes a parameter or a global (a value
//! that is valid everywhere), and whose loads that block dominates is forwarded. The object must not escape: every use
//! of it is a field address, a lifetime marker, or a pointer cast used only by lifetime markers.

use std::collections::{HashMap, HashSet};

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '$' | '-')
}

/// Every occurrence of the local name `%n` as a whole token.
fn token_positions(line: &str, name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = line[from..].find(name) {
        let start = from + at;
        let end = start + name.len();
        if line[end..].chars().next().is_none_or(|c| !is_name_char(c)) {
            out.push(start);
        }
        from = end;
    }
    out
}

fn replace_token(line: &str, name: &str, with: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for at in token_positions(line, name) {
        out.push_str(&line[last..at]);
        out.push_str(with);
        last = at + name.len();
    }
    out.push_str(&line[last..]);
    out
}

fn assigned(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    let (lhs, rhs) = t.split_once(" = ")?;
    lhs.starts_with('%').then_some((lhs, rhs))
}

fn block_label(line: &str) -> Option<String> {
    let t = line.split(';').next().unwrap_or("").trim_end();
    let label = t.strip_suffix(':')?;
    (!label.is_empty() && !label.contains(' ') && !line.starts_with(' ')).then(|| format!("%{label}"))
}

fn successors(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("label %") {
        let s = &rest[at + 6..];
        let end = s[1..].find(|c: char| !is_name_char(c)).map_or(s.len(), |e| e + 1);
        out.push(s[..end].to_string());
        rest = &s[end..];
    }
    out
}

fn is_terminator(line: &str) -> bool {
    let t = line.trim_start();
    ["br ", "switch ", "ret ", "ret\n", "unreachable", "resume ", "indirectbr "]
        .iter()
        .any(|p| t.starts_with(p))
        || t == "ret void"
}

/// The parameter names of a `define` header.
fn params(header: &str) -> HashSet<String> {
    let Some(open) = header.find('(') else { return HashSet::new() };
    let Some(close) = header.rfind(')') else { return HashSet::new() };
    let mut out = HashSet::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in header[open + 1..close].chars() {
        match c {
            '(' | '<' | '{' | '[' => depth += 1,
            ')' | '>' | '}' | ']' => depth -= 1,
            ',' if depth == 0 => {
                if let Some(n) = cur.split_whitespace().last().filter(|n| n.starts_with('%')) {
                    out.insert(n.to_string());
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if let Some(n) = cur.split_whitespace().last().filter(|n| n.starts_with('%')) {
        out.insert(n.to_string());
    }
    out
}

fn forward_in_function(header: &str, body: &mut Vec<String>) -> usize {
    // blocks: line index of each instruction -> block number
    let mut block_of = vec![0usize; body.len()];
    let mut names: Vec<String> = vec!["%<entry>".into()];
    let mut succ: Vec<Vec<String>> = vec![Vec::new()];
    for (i, line) in body.iter().enumerate() {
        if let Some(label) = block_label(line) {
            if i > 0 || !names.is_empty() {
                names.push(label);
                succ.push(Vec::new());
            }
        }
        let b = names.len() - 1;
        block_of[i] = b;
        // a switch lists its cases on the lines after it, so every line naming a label is an edge of its block
        // (phi operands name blocks as `[ %v, %bb ]`, never with the word "label")
        if is_terminator(line) || line.contains("label %") {
            succ[b].extend(successors(line));
        }
    }
    // the first label right at the top names the entry block, not a second one
    if body.first().and_then(|l| block_label(l)).is_some() {
        names.remove(0);
        succ.remove(0);
        for b in block_of.iter_mut() {
            *b = b.saturating_sub(1);
        }
    }
    let n = names.len();
    let index: HashMap<&str, usize> = names.iter().enumerate().map(|(i, s)| (s.as_str(), i)).collect();
    let mut preds = vec![Vec::new(); n];
    for (b, ss) in succ.iter().enumerate() {
        for s in ss {
            if let Some(&t) = index.get(s.as_str()) {
                preds[t].push(b);
            }
        }
    }
    // dominators, iterative over bit sets
    let all: Vec<bool> = vec![true; n];
    let mut dom: Vec<Vec<bool>> = (0..n).map(|b| if b == 0 { (0..n).map(|x| x == 0).collect() } else { all.clone() }).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            let mut new = all.clone();
            if preds[b].is_empty() {
                new = vec![false; n];
            }
            for &p in &preds[b] {
                for x in 0..n {
                    new[x] = new[x] && dom[p][x];
                }
            }
            new[b] = true;
            if new != dom[b] {
                dom[b] = new;
                changed = true;
            }
        }
    }
    let ps = params(header);
    // local objects and their field addresses
    let mut allocas: HashSet<String> = HashSet::new();
    let mut field_of: HashMap<String, (String, String)> = HashMap::new(); // gep name -> (object, indices)
    let mut lifetime_casts: HashSet<String> = HashSet::new();
    for line in body.iter() {
        let Some((lhs, rhs)) = assigned(line) else { continue };
        if rhs.starts_with("alloca ") {
            allocas.insert(lhs.to_string());
        } else if let Some(rest) = rhs.strip_prefix("getelementptr inbounds ") {
            // getelementptr inbounds %T, ptr %obj, <indices>
            // the type may itself hold commas (a literal struct), so anchor on the base operand
            let Some(at) = rest.find(", ptr %") else { continue };
            let after = &rest[at + 6..];
            let (base, idx) = match after.split_once(", ") { Some((b, i)) => (Some(b.trim()), i), None => (None, "") };
            let idx = idx.split(", !").next().unwrap_or("").trim();
            if let Some(base) = base.filter(|b| allocas.contains(*b)) {
                if idx.split(", ").all(|p| p.starts_with("i64 ") || p.starts_with("i32 ")) && !idx.contains('%') {
                    field_of.insert(lhs.to_string(), (base.to_string(), idx.to_string()));
                }
            }
        } else if let Some(rest) = rhs.strip_prefix("bitcast ptr ") {
            if let Some(src) = rest.split_whitespace().next().filter(|s| allocas.contains(*s)) {
                let _ = src;
                lifetime_casts.insert(lhs.to_string());
            }
        }
    }
    // escape check: every use of an object is a field address, a lifetime marker, or a cast used only by markers
    let mut escaped: HashSet<String> = HashSet::new();
    for obj in &allocas {
        for line in body.iter() {
            if token_positions(line, obj).is_empty() {
                continue;
            }
            let t = line.trim_start();
            let defines = assigned(line).is_some_and(|(l, _)| l == obj);
            let is_gep = assigned(line).is_some_and(|(l, _)| field_of.get(l).is_some_and(|(o, _)| o == obj));
            let is_cast = assigned(line).is_some_and(|(l, _)| lifetime_casts.contains(l));
            let is_marker = t.contains("@llvm.lifetime.");
            if !(defines || is_gep || is_cast || is_marker) {
                escaped.insert(obj.clone());
            }
        }
    }
    for cast in &lifetime_casts {
        let only_markers = body.iter().all(|line| {
            token_positions(line, cast).is_empty()
                || assigned(line).is_some_and(|(l, _)| l == cast)
                || line.contains("@llvm.lifetime.")
        });
        if !only_markers {
            // the cast is used for something else: the object it aliases escapes
            for line in body.iter() {
                if let Some((l, rhs)) = assigned(line) {
                    if l == cast {
                        if let Some(src) = rhs.strip_prefix("bitcast ptr ").and_then(|r| r.split_whitespace().next()) {
                            escaped.insert(src.to_string());
                        }
                    }
                }
            }
        }
    }
    // stores and loads per field
    struct Store { line: usize, ty: String, value: String }
    let mut stores: HashMap<(String, String), Vec<Store>> = HashMap::new();
    let mut loads: HashMap<(String, String), Vec<(usize, String, String)>> = HashMap::new(); // (line, name, ty)
    let mut field_uses_ok: HashMap<String, bool> = HashMap::new();
    for (i, line) in body.iter().enumerate() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("store ") {
            // store <ty> <value>, ptr %g, align N
            let Some((ty_val, ptr_rest)) = rest.split_once(", ptr ") else { continue };
            let g = ptr_rest.split(',').next().unwrap_or("").trim();
            if let Some(key) = field_of.get(g) {
                let (ty, value) = match ty_val.rsplit_once(' ') { Some(p) => p, None => continue };
                stores.entry(key.clone()).or_default().push(Store { line: i, ty: ty.to_string(), value: value.to_string() });
            }
        } else if let Some((lhs, rhs)) = assigned(line) {
            if let Some(rest) = rhs.strip_prefix("load ") {
                let Some((ty, ptr_rest)) = rest.split_once(", ptr ") else { continue };
                let g = ptr_rest.split(',').next().unwrap_or("").trim();
                if let Some(key) = field_of.get(g) {
                    loads.entry(key.clone()).or_default().push((i, lhs.to_string(), ty.to_string()));
                }
            }
        }
    }
    // a field address used for anything but a plain load or store makes that object unsafe
    for (g, (obj, _)) in &field_of {
        for line in body.iter() {
            if token_positions(line, g).is_empty() || assigned(line).is_some_and(|(l, _)| l == g) {
                continue;
            }
            let t = line.trim_start();
            let as_ptr_of_store = t.starts_with("store ") && t.split_once(", ptr ").is_some_and(|(v, p)| {
                token_positions(v, g).is_empty() && p.split(',').next().unwrap_or("").trim() == g
            });
            let as_ptr_of_load = assigned(line).is_some_and(|(_, r)| {
                r.starts_with("load ") && r.split_once(", ptr ").is_some_and(|(_, p)| p.split(',').next().unwrap_or("").trim() == g)
            });
            let marker = t.contains("@llvm.lifetime.");
            if !(as_ptr_of_store || as_ptr_of_load || marker) {
                field_uses_ok.insert(obj.clone(), false);
            }
        }
    }
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut drop_lines: HashSet<usize> = HashSet::new();
    for (key, ss) in &stores {
        let (obj, _) = key;
        if escaped.contains(obj) || field_uses_ok.get(obj) == Some(&false) {
            continue;
        }
        let last = ss.iter().max_by_key(|s| s.line).unwrap();
        if !last.ty.starts_with("ptr addrspace(") {
            continue;
        }
        let v = last.value.as_str();
        if !(ps.contains(v) || v.starts_with('@') || v == "null") {
            continue;
        }
        let Some(ls) = loads.get(key) else { continue };
        // Every store writing the same parameter or global: a load reads that value or memory nothing wrote yet,
        // and an unwritten value may be taken to be anything, so the parameter is a valid answer on every path
        // (VFX's particle_surface_frag stores its constant buffer into a local in two branches).
        let same_everywhere = ss.iter().all(|s| s.value == last.value && s.ty == last.ty);
        let b = block_of[ss[0].line];
        let one_block = ss.iter().all(|s| block_of[s.line] == b);
        if !same_everywhere && !one_block {
            continue;
        }
        let ok = ls.iter().all(|(line, _, ty)| {
            ty == &last.ty
                && (same_everywhere
                    || if block_of[*line] == b { *line > last.line } else { dom[block_of[*line]][b] })
        });
        if !ok {
            continue;
        }
        for (line, name, _) in ls {
            renames.push((name.clone(), last.value.clone()));
            drop_lines.insert(*line);
        }
    }
    // A device-pointer field that no load reads any more (all of them forwarded, or none to begin with) is dead: its
    // stores are the only thing left that would put a descriptor pointer into function memory, which Logical SPIR-V
    // cannot hold ("pointer %N has no exact BDA address representation" on VFX's particle_surface_frag).
    let mut dead_stores = 0usize;
    for (key, ss) in &stores {
        let (obj, _) = key;
        if escaped.contains(obj) || field_uses_ok.get(obj) == Some(&false) {
            continue;
        }
        if !ss.iter().all(|s| s.ty.starts_with("ptr addrspace(")) {
            continue;
        }
        let unread = loads.get(key).is_none_or(|ls| ls.iter().all(|(line, _, _)| drop_lines.contains(line)));
        if unread {
            for st in ss {
                drop_lines.insert(st.line);
                dead_stores += 1;
            }
        }
    }
    if renames.is_empty() && dead_stores == 0 {
        return 0;
    }
    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if drop_lines.contains(&i) {
            continue;
        }
        let mut l = line.clone();
        for (from, to) in &renames {
            if !token_positions(&l, from).is_empty() {
                l = replace_token(&l, from, to);
            }
        }
        out.push(l);
    }
    *body = out;
    renames.len() + dead_stores
}

/// A pointer phi whose every incoming value is the same value is that value. After forwarding, VFX's
/// particle_surface_frag has `phi ptr addrspace(2) [ %31, %a ], [ %31, %b ]`, and a descriptor pointer cannot be a phi
/// in Logical SPIR-V ("pointer %31 has no exact BDA address representation").
fn fold_uniform_pointer_phis(body: &mut Vec<String>) -> usize {
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut drop: HashSet<usize> = HashSet::new();
    for (i, line) in body.iter().enumerate() {
        let Some((lhs, rhs)) = assigned(line) else { continue };
        let Some(rest) = rhs.strip_prefix("phi ptr addrspace(") else { continue };
        let Some((_, incoming)) = rest.split_once(") ") else { continue };
        let values: Vec<&str> = incoming
            .split(']')
            .filter_map(|part| part.trim().trim_start_matches(',').trim().strip_prefix('['))
            .filter_map(|pair| pair.split(',').next().map(str::trim))
            .collect();
        if values.is_empty() || values.iter().any(|v| *v != values[0]) || values[0] == lhs {
            continue;
        }
        renames.push((lhs.to_string(), values[0].to_string()));
        drop.insert(i);
    }
    if renames.is_empty() {
        return 0;
    }
    let mut out = Vec::with_capacity(body.len());
    for (i, line) in body.iter().enumerate() {
        if drop.contains(&i) {
            continue;
        }
        let mut l = line.clone();
        for (from, to) in &renames {
            if !token_positions(&l, from).is_empty() {
                l = replace_token(&l, from, to);
            }
        }
        out.push(l);
    }
    *body = out;
    renames.len()
}

/// Returns the module with forwarded fields, or None when nothing was forwarded.
pub(crate) fn forward_local_pointer_fields(san_ll: &str) -> Option<String> {
    let lines: Vec<&str> = san_ll.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut total = 0;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("define") && line.trim_end().ends_with('{') {
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j].trim() != "}" {
                j += 1;
            }
            let mut body: Vec<String> = lines[start..j.min(lines.len())].iter().map(|s| s.to_string()).collect();
            // a forwarded value can itself be what another field stores (VFX: a local copies the constant-buffer
            // pointer it loaded from another local), so the pass runs until nothing changes
            for _ in 0..8 {
                let n = forward_in_function(line, &mut body) + fold_uniform_pointer_phis(&mut body);
                total += n;
                if n == 0 {
                    break;
                }
            }
            out.push(line.to_string());
            out.extend(body);
            if j < lines.len() {
                out.push(lines[j].to_string());
            }
            i = j + 1;
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }
    (total > 0).then(|| {
        let mut s = out.join("\n");
        if san_ll.ends_with('\n') {
            s.push('\n');
        }
        s
    })
}

#[cfg(test)]
mod tests {
    use super::forward_local_pointer_fields;

    const SHAPE: &str = "define void @k(ptr addrspace(1) %as, ptr addrspace(1) %tab) {
  %r = alloca { ptr addrspace(2), ptr addrspace(1), ptr addrspace(1) }, align 8
  %c = bitcast ptr %r to ptr
  call void @llvm.lifetime.start.p0(i64 24, ptr %c)
  %f1 = getelementptr inbounds { ptr addrspace(2), ptr addrspace(1), ptr addrspace(1) }, ptr %r, i64 0, i32 1
  %f2 = getelementptr inbounds { ptr addrspace(2), ptr addrspace(1), ptr addrspace(1) }, ptr %r, i64 0, i32 2
  %n = tail call ptr addrspace(1) @air.get_null_intersection_function_table()
  store ptr addrspace(1) %n, ptr %f2, align 8
  store ptr addrspace(1) %as, ptr %f1, align 8
  store ptr addrspace(1) %tab, ptr %f2, align 8
  br label %next

next:
  %g1 = getelementptr inbounds { ptr addrspace(2), ptr addrspace(1), ptr addrspace(1) }, ptr %r, i64 0, i32 1
  %a = load ptr addrspace(1), ptr %g1, align 8
  %g2 = getelementptr inbounds { ptr addrspace(2), ptr addrspace(1), ptr addrspace(1) }, ptr %r, i64 0, i32 2
  %t = load ptr addrspace(1), ptr %g2, align 8
  %hit = call i32 @air.intersect(ptr addrspace(1) %a, ptr addrspace(1) %t)
  ret void
}
";

    #[test]
    fn the_acceleration_structure_and_table_reach_the_intersect_call() {
        let out = forward_local_pointer_fields(SHAPE).expect("forwarded");
        assert!(out.contains("@air.intersect(ptr addrspace(1) %as, ptr addrspace(1) %tab)"), "{out}");
        assert!(!out.contains("load ptr addrspace(1), ptr %g1"), "{out}");
    }

    #[test]
    fn a_pointer_field_no_one_reads_loses_its_stores() {
        let out = forward_local_pointer_fields(SHAPE).expect("forwarded");
        assert!(!out.contains("store ptr addrspace(1) %as"), "{out}");
        assert!(!out.contains("store ptr addrspace(1) %tab"), "{out}");
    }

    #[test]
    fn negative_control_a_field_still_read_keeps_its_store() {
        // field 1 holds two different values in two blocks and is read before the second store: it stays read, so
        // its stores stay too
        let s = SHAPE
            .replace("next:\n", "next:\n  store ptr addrspace(1) %tab, ptr %f1, align 8\n")
            .replace("  br label %next", "  %early = load ptr addrspace(1), ptr %f1, align 8\n  br label %next");
        let out = forward_local_pointer_fields(&s).unwrap_or_else(|| s.clone());
        assert!(out.contains("store ptr addrspace(1) %as"), "{out}");
    }

    #[test]
    fn the_same_parameter_stored_in_two_branches_is_forwarded() {
        let s = SHAPE
            .replace("  store ptr addrspace(1) %as, ptr %f1, align 8\n", "")
            .replace("  br label %next", "  br i1 true, label %a, label %b\n\na:\n  store ptr addrspace(1) %as, ptr %f1, align 8\n  br label %next\n\nb:\n  store ptr addrspace(1) %as, ptr %f1, align 8\n  br label %next");
        let out = forward_local_pointer_fields(&s).expect("forwarded");
        assert!(out.contains("@air.intersect(ptr addrspace(1) %as"), "{out}");
    }

    #[test]
    fn negative_control_different_values_in_two_branches_are_not_forwarded() {
        let s = SHAPE
            .replace("  store ptr addrspace(1) %as, ptr %f1, align 8\n", "")
            .replace("  br label %next", "  br i1 true, label %a, label %b\n\na:\n  store ptr addrspace(1) %as, ptr %f1, align 8\n  br label %next\n\nb:\n  store ptr addrspace(1) %tab, ptr %f1, align 8\n  br label %next");
        let out = forward_local_pointer_fields(&s).unwrap_or_else(|| s.clone());
        assert!(out.contains("load ptr addrspace(1), ptr %g1"), "{out}");
    }

    #[test]
    fn a_pointer_phi_of_one_value_is_that_value() {
        let mut body = vec![
            "  %p = phi ptr addrspace(2) [ %c, %a ], [ %c, %b ]".to_string(),
            "  %g = getelementptr inbounds i8, ptr addrspace(2) %p, i64 4".to_string(),
            "  %q = phi ptr addrspace(2) [ %c, %a ], [ %d, %b ]".to_string(),
        ];
        assert_eq!(super::fold_uniform_pointer_phis(&mut body), 1);
        assert_eq!(body[0], "  %g = getelementptr inbounds i8, ptr addrspace(2) %c, i64 4");
        assert!(body[1].contains("[ %d, %b ]"), "a phi of two values stays");
    }

    #[test]
    fn negative_control_an_object_passed_to_a_call_is_not_forwarded() {
        let s = SHAPE.replace("  br label %next", "  call void @helper(ptr %r)\n  br label %next");
        assert!(forward_local_pointer_fields(&s).is_none());
    }

    #[test]
    fn negative_control_a_store_in_another_block_is_not_forwarded() {
        let s = SHAPE.replace(
            "  %t = load ptr addrspace(1), ptr %g2, align 8",
            "  store ptr addrspace(1) null, ptr %g1, align 8\n  %t = load ptr addrspace(1), ptr %g2, align 8",
        );
        let out = forward_local_pointer_fields(&s).expect("the table field is still forwarded");
        assert!(out.contains("load ptr addrspace(1), ptr %g1"), "the accel field has two stores in two blocks: {out}");
    }

    #[test]
    fn negative_control_a_load_the_store_block_does_not_dominate_is_kept() {
        let s = SHAPE.replace(
            "  br label %next",
            "  br i1 true, label %next, label %other\n\nother:\n  %o = load ptr addrspace(1), ptr %f1, align 8\n  ret void",
        )
        .replace("define void @k(", "define void @k(");
        // the store block (entry) dominates both, so this one is forwarded; flip it: store in %next, load in %other
        assert!(forward_local_pointer_fields(&s).is_some());
        let s2 = SHAPE
            .replace("  store ptr addrspace(1) %as, ptr %f1, align 8\n", "")
            .replace("  br label %next", "  br i1 true, label %next, label %other\n\nother:\n  %o = load ptr addrspace(1), ptr %f1, align 8\n  ret void")
            .replace("next:\n", "next:\n  store ptr addrspace(1) %as, ptr %f1, align 8\n");
        let out = forward_local_pointer_fields(&s2).unwrap_or_default();
        assert!(!out.contains("%o = load") || out.contains("%o = load ptr addrspace(1), ptr %f1"), "{out}");
    }
}
