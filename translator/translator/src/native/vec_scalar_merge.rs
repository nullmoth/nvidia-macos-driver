use super::lex::split_top_level;
use crate::fxhash::{HashMap, HashSet};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn lower_vector_scalar_pointer_merge(san_ll: &str) -> Cow<'_, str> {
    lower_impl(san_ll, true)
}

fn lower_impl(san_ll: &str, widen: bool) -> Cow<'_, str> {
    let lines: Vec<&str> = san_ll.lines().collect();
    let mut out: Option<Vec<String>> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("define") && line.trim_end().ends_with('{') {
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j] != "}" {
                j += 1;
            }
            if let Some(rewritten) = rewrite_function_body(&lines[start..j], widen) {
                let output = out.get_or_insert_with(|| {
                    let mut prefix = Vec::with_capacity(lines.len());
                    prefix.extend(lines[..i].iter().map(|line| (*line).to_string()));
                    prefix
                });
                output.push(line.to_string());
                output.extend(rewritten);
                if j < lines.len() {
                    output.push(lines[j].to_string());
                }
            } else if let Some(output) = &mut out {
                output.extend(
                    lines[i..j.min(lines.len() - 1) + 1]
                        .iter()
                        .map(|line| (*line).to_string()),
                );
            }
            i = j + 1;
        } else {
            if let Some(output) = &mut out {
                output.push(line.to_string());
            }
            i += 1;
        }
    }
    let Some(out) = out else {
        return Cow::Borrowed(san_ll);
    };
    let mut result = out.join("\n");
    if san_ll.ends_with('\n') {
        result.push('\n');
    }
    Cow::Owned(result)
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Pointee {
    Scalar(String),
    Vec(String, usize),
    Unknown,
}

struct Def<'a> {
    name: &'a str,
    opcode: &'a str,
    rest: &'a str,
}

fn parse_def(line: &str) -> Option<Def<'_>> {
    let line = line.trim();
    let eq = line.find(" = ")?;
    let name = line[..eq].trim();
    if !name.starts_with('%') {
        return None;
    }
    let rhs = line[eq + 3..].trim();
    let opcode = rhs.split_whitespace().next()?;
    let rest = rhs[opcode.len()..].trim();
    Some(Def { name, opcode, rest })
}

fn vec_elem_lanes(ty: &str) -> Option<(String, usize)> {
    let ty = ty.trim();
    let inner = ty.strip_prefix('<')?.strip_suffix('>')?.trim();
    let (n, elem) = inner.split_once(" x ")?;
    let lanes: usize = n.trim().parse().ok()?;
    let elem = elem.trim();
    if !is_simple_scalar(elem) {
        return None;
    }
    Some((elem.to_string(), lanes))
}

fn is_simple_scalar(ty: &str) -> bool {
    !ty.is_empty()
        && ty.chars().all(|c| c.is_ascii_alphanumeric())
        && ty.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

fn split_ptr_operand(chunk: &str) -> Option<(&str, &str)> {
    let chunk = chunk.trim();
    let name_start = chunk.rfind(char::is_whitespace)? + 1;
    let name = chunk[name_start..].trim();
    if !name.starts_with('%') {
        return None;
    }
    Some((chunk[..name_start].trim(), name))
}

fn trailing_local(chunk: &str) -> Option<&str> {
    let tok = chunk.trim().rsplit(char::is_whitespace).next()?.trim();
    tok.starts_with('%').then_some(tok)
}

struct GepInfo<'a> {
    inbounds: bool,
    src_ty: &'a str,
    base_chunk: &'a str,
    base_name: &'a str,
    indices: Vec<&'a str>,
}

fn parse_gep(rest: &str) -> Option<GepInfo<'_>> {
    let (inbounds, rest) = match rest.strip_prefix("inbounds ") {
        Some(r) => (true, r.trim()),
        None => (false, rest.trim()),
    };
    let chunks = split_top_level(rest, ',');
    if chunks.len() < 2 {
        return None;
    }
    let (_, base_name) = split_ptr_operand(chunks[1])?;
    Some(GepInfo {
        inbounds,
        src_ty: chunks[0].trim(),
        base_chunk: chunks[1].trim(),
        base_name,
        indices: chunks[2..].to_vec(),
    })
}

fn pointer_neighbours(def: &Def) -> Vec<String> {
    match def.opcode {
        "getelementptr" => parse_gep(def.rest)
            .map(|g| vec![g.base_name.to_string()])
            .unwrap_or_default(),
        "bitcast" => bitcast_source(def.rest)
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
        "phi" => phi_arm_values(def.rest),
        "select" => select_arm_values(def.rest),
        _ => Vec::new(),
    }
}

fn bitcast_source(rest: &str) -> Option<&str> {
    let (lhs, rhs) = rest.split_once(" to ")?;
    if !lhs.trim_start().starts_with("ptr") || !rhs.trim_start().starts_with("ptr") {
        return None;
    }
    trailing_local(lhs)
}

fn phi_arm_values(rest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut search = rest;
    while let Some(open) = search.find('[') {
        let after = &search[open + 1..];
        let Some(close) = after.find(']') else { break };
        let inner = &after[..close];
        if let Some(val) = inner.split(',').next() {
            let val = val.trim();
            if val.starts_with('%') {
                out.push(val.to_string());
            }
        }
        search = &after[close + 1..];
    }
    out
}

fn select_arm_values(rest: &str) -> Vec<String> {
    let parts = split_top_level(rest, ',');
    if parts.len() != 3 {
        return Vec::new();
    }
    parts[1..]
        .iter()
        .filter(|p| p.trim_start().starts_with("ptr"))
        .filter_map(|p| trailing_local(p))
        .map(str::to_string)
        .collect()
}

fn classify(defs: &BTreeMap<String, Def>) -> (HashMap<String, Pointee>, HashSet<String>) {
    let mut pointees: HashMap<String, Pointee> = HashMap::default();
    let mut mismatches: HashSet<String> = HashSet::default();
    for _ in 0..=defs.len() {
        let mut changed = false;
        let mut round_mismatch = HashSet::default();
        for (name, def) in defs {
            let p = compute_pointee(def, &pointees, &mut round_mismatch, name);
            if pointees.get(name) != Some(&p) {
                pointees.insert(name.clone(), p);
                changed = true;
            }
        }
        mismatches = round_mismatch;
        if !changed {
            break;
        }
    }
    (pointees, mismatches)
}

fn compute_pointee(
    def: &Def,
    pointees: &HashMap<String, Pointee>,
    mismatches: &mut HashSet<String>,
    name: &str,
) -> Pointee {
    match def.opcode {
        "getelementptr" => {
            let Some(g) = parse_gep(def.rest) else {
                return Pointee::Unknown;
            };
            if let Some((elem, lanes)) = vec_elem_lanes(g.src_ty) {
                if g.indices.len() == 1 {
                    Pointee::Vec(elem, lanes)
                } else {
                    Pointee::Scalar(elem)
                }
            } else if is_simple_scalar(g.src_ty) {
                Pointee::Scalar(g.src_ty.to_string())
            } else {
                Pointee::Unknown
            }
        }
        "bitcast" => bitcast_source(def.rest)
            .and_then(|s| pointees.get(s).cloned())
            .unwrap_or(Pointee::Unknown),
        "phi" => merge_arms(&phi_arm_values(def.rest), pointees, mismatches, name),
        "select" => merge_arms(&select_arm_values(def.rest), pointees, mismatches, name),
        _ => Pointee::Unknown,
    }
}

fn merge_arms(
    arms: &[String],
    pointees: &HashMap<String, Pointee>,
    mismatches: &mut HashSet<String>,
    name: &str,
) -> Pointee {
    let mut elems: HashSet<String> = HashSet::default();
    let mut saw_scalar = false;
    let mut vec_lanes: Option<usize> = None;
    let mut consistent_vec = true;
    for arm in arms {
        match pointees.get(arm) {
            Some(Pointee::Scalar(e)) => {
                elems.insert(e.clone());
                saw_scalar = true;
            }
            Some(Pointee::Vec(e, n)) => {
                elems.insert(e.clone());
                match vec_lanes {
                    Some(prev) if prev != *n => consistent_vec = false,
                    _ => vec_lanes = Some(*n),
                }
            }
            _ => {}
        }
    }
    if elems.len() != 1 || !consistent_vec {
        return Pointee::Unknown;
    }
    let elem = elems.into_iter().next().unwrap();
    match (saw_scalar, vec_lanes) {
        (true, Some(_)) => {
            mismatches.insert(name.to_string());
            Pointee::Scalar(elem)
        }
        (true, None) => Pointee::Scalar(elem),
        (false, Some(n)) => Pointee::Vec(elem, n),
        (false, None) => Pointee::Unknown,
    }
}

fn pointer_adjacency(defs: &BTreeMap<String, Def>) -> BTreeMap<String, Vec<String>> {
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, def) in defs {
        for nb in pointer_neighbours(def) {
            adj.entry(name.clone()).or_default().push(nb.clone());
            adj.entry(nb).or_default().push(name.clone());
        }
    }
    adj
}

fn component(defs: &BTreeMap<String, Def>, seeds: &HashSet<String>) -> HashSet<String> {
    let adj = pointer_adjacency(defs);
    let mut seen: HashSet<String> = HashSet::default();
    let mut queue: VecDeque<String> = seeds.iter().cloned().collect();
    for s in seeds {
        seen.insert(s.clone());
    }
    while let Some(cur) = queue.pop_front() {
        if let Some(nbs) = adj.get(&cur) {
            for nb in nbs {
                if seen.insert(nb.clone()) {
                    queue.push_back(nb.clone());
                }
            }
        }
    }
    seen
}

#[derive(Default)]
struct UseWidths {
    scalars: HashSet<String>,
    vectors: HashSet<String>,
    other: bool,
    logical: bool,
}

fn memory_reloaded_pointers(body: &[&str]) -> HashSet<String> {
    let mut out = HashSet::default();
    for line in body {
        if let Some(def) = parse_def(line) {
            if def.opcode == "load" {
                let loaded_ty = split_top_level(def.rest, ',')
                    .first()
                    .map(|c| c.trim().to_string())
                    .unwrap_or_default();
                if loaded_ty.starts_with("ptr") {
                    out.insert(def.name.to_string());
                }
            }
        }
    }
    out
}

fn is_word_addressable_ptr(prefix: &str) -> bool {
    prefix.contains("addrspace(1)") || prefix.contains("addrspace(2)")
}

fn scalar_byte_width(elem: &str) -> Option<usize> {
    match elem {
        "i8" => Some(1),
        "i16" | "half" | "bfloat" => Some(2),
        "i32" | "float" => Some(4),
        "i64" | "double" => Some(8),
        other => other
            .strip_prefix('i')
            .and_then(|b| b.parse::<usize>().ok())
            .map(|bits| bits.div_ceil(8)),
    }
}

fn use_widths(body: &[&str]) -> HashMap<String, UseWidths> {
    let mut map: HashMap<String, UseWidths> = HashMap::default();
    let mut observe = |name: &str, ty: &str, ptr_prefix: &str| {
        let entry = map.entry(name.to_string()).or_default();
        if let Some((elem, _)) = vec_elem_lanes(ty) {
            entry.vectors.insert(elem);
        } else if is_simple_scalar(ty.trim()) {
            entry.scalars.insert(ty.trim().to_string());
        } else {
            entry.other = true;
        }
        entry.logical |= !is_word_addressable_ptr(ptr_prefix);
    };
    for line in body {
        let trimmed = line.trim();
        if let Some(def) = parse_def(line) {
            match def.opcode {
                "load" => {
                    let chunks = split_top_level(def.rest, ',');
                    if chunks.len() >= 2 {
                        if let Some((prefix, ptr)) = split_ptr_operand(chunks[1]) {
                            observe(ptr, chunks[0].trim(), prefix);
                        }
                    }
                }
                "getelementptr" => {
                    if let Some(g) = parse_gep(def.rest) {
                        let prefix = split_ptr_operand(g.base_chunk).map_or("", |(p, _)| p);
                        observe(g.base_name, g.src_ty, prefix);
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("store ") {
            let chunks = split_top_level(rest, ',');
            if chunks.len() >= 2 {
                if let (Some((ty, _)), Some((prefix, ptr))) =
                    (split_ptr_operand(chunks[0]), split_ptr_operand(chunks[1]))
                {
                    observe(ptr, ty, prefix);
                }
            }
        }
    }
    map
}

fn widen_targets(body: &[&str], defs: &BTreeMap<String, Def>) -> Vec<(HashSet<String>, String)> {
    let widths = use_widths(body);
    let reloaded = memory_reloaded_pointers(body);
    let adj = pointer_adjacency(defs);
    let mut nodes: BTreeSet<String> = adj.keys().cloned().collect();
    nodes.extend(widths.keys().cloned());

    let mut visited: HashSet<String> = HashSet::default();
    let mut targets: Vec<(HashSet<String>, String)> = Vec::new();
    for start in &nodes {
        if !visited.insert(start.clone()) {
            continue;
        }
        let mut members: HashSet<String> = HashSet::default();
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(start.clone());
        members.insert(start.clone());
        while let Some(cur) = queue.pop_front() {
            if let Some(nbs) = adj.get(&cur) {
                for nb in nbs {
                    if visited.insert(nb.clone()) {
                        members.insert(nb.clone());
                        queue.push_back(nb.clone());
                    }
                }
            }
        }
        let mut elems: HashSet<String> = HashSet::default();
        let (mut has_scalar, mut has_vector, mut other, mut logical) = (false, false, false, false);
        let mut memory_ptr = false;
        for m in &members {
            memory_ptr |= reloaded.contains(m);
            if let Some(w) = widths.get(m) {
                for s in &w.scalars {
                    elems.insert(s.clone());
                    has_scalar = true;
                }
                for v in &w.vectors {
                    elems.insert(v.clone());
                    has_vector = true;
                }
                other |= w.other;
                logical |= w.logical;
            }
        }
        if !other && !logical && !memory_ptr && has_scalar && has_vector && elems.len() == 1 {
            let elem = elems.into_iter().next().unwrap();
            if scalar_byte_width(&elem).is_some_and(|w| w < 4) {
                continue;
            }
            targets.push((members, elem));
        }
    }
    targets
}

enum Index {
    Const(i128),
    Dyn { value: String, ity: String },
}

fn parse_index(operand: &str) -> Option<Index> {
    let mut it = operand.split_whitespace();
    let ity = it.next()?.to_string();
    let val = it.next()?.trim();
    if let Ok(c) = val.parse::<i128>() {
        Some(Index::Const(c))
    } else if val.starts_with('%') {
        Some(Index::Dyn {
            value: val.to_string(),
            ity,
        })
    } else {
        None
    }
}

const VSM_NAME_PREFIX: &str = ".vsm";

struct Rewriter {
    counter: usize,
}

impl Rewriter {
    fn fresh(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        format!("%{VSM_NAME_PREFIX}{n}")
    }

    fn rewrite_gep(&mut self, name: &str, g: &GepInfo, elem: &str, lanes: usize) -> Vec<String> {
        let inbounds = if g.inbounds { "inbounds " } else { "" };
        let mut lines = Vec::new();
        let i0 = parse_index(g.indices[0]).unwrap();
        let (mut acc_const, mut acc_dyn): (i128, Option<(String, String)>) = match i0 {
            Index::Const(c) => (c * lanes as i128, None),
            Index::Dyn { value, ity } => {
                let m = self.fresh();
                lines.push(format!("  {m} = mul {ity} {value}, {lanes}"));
                (0, Some((m, ity)))
            }
        };
        if g.indices.len() >= 2 {
            if let Some(i1) = parse_index(g.indices[1]) {
                match i1 {
                    Index::Const(c) => acc_const += c,
                    Index::Dyn { value, ity } => {
                        acc_dyn = Some(match acc_dyn {
                            Some((acc, _)) => {
                                let a = self.fresh();
                                lines.push(format!("  {a} = add {ity} {acc}, {value}"));
                                (a, ity)
                            }
                            None => {
                                if acc_const == 0 {
                                    (value, ity)
                                } else {
                                    let a = self.fresh();
                                    lines.push(format!("  {a} = add {ity} {value}, {acc_const}"));
                                    acc_const = 0;
                                    (a, ity)
                                }
                            }
                        });
                    }
                }
            }
        }
        let (idx_ty, idx_val) = match acc_dyn {
            Some((v, ity)) if acc_const == 0 => (ity, v),
            Some((v, ity)) => {
                let a = self.fresh();
                lines.push(format!("  {a} = add {ity} {v}, {acc_const}"));
                (ity, a)
            }
            None => ("i64".to_string(), acc_const.to_string()),
        };
        let _ = elem;
        lines.push(format!(
            "  {name} = getelementptr {inbounds}{elem}, {base}, {idx_ty} {idx_val}",
            base = g.base_chunk
        ));
        lines
    }

    fn split_load(
        &mut self,
        name: &str,
        elem: &str,
        lanes: usize,
        ptr_prefix: &str,
        ptr_name: &str,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        let mut elem_ids = Vec::with_capacity(lanes);
        for k in 0..lanes {
            let p = self.fresh();
            let l = self.fresh();
            lines.push(format!(
                "  {p} = getelementptr inbounds {elem}, {ptr_prefix} {ptr_name}, i64 {k}"
            ));
            lines.push(format!("  {l} = load {elem}, {ptr_prefix} {p}, align 4"));
            elem_ids.push(l);
        }
        let vty = format!("<{lanes} x {elem}>");
        let mut prev = "undef".to_string();
        for (k, elem_id) in elem_ids.iter().enumerate() {
            let out = if k + 1 == lanes {
                name.to_string()
            } else {
                self.fresh()
            };
            lines.push(format!(
                "  {out} = insertelement {vty} {prev}, {elem} {elem_id}, i32 {k}"
            ));
            prev = out;
        }
        lines
    }

    fn split_store(
        &mut self,
        value: &str,
        elem: &str,
        lanes: usize,
        ptr_prefix: &str,
        ptr_name: &str,
    ) -> Vec<String> {
        let vty = format!("<{lanes} x {elem}>");
        let mut lines = Vec::new();
        for k in 0..lanes {
            let x = self.fresh();
            let p = self.fresh();
            lines.push(format!("  {x} = extractelement {vty} {value}, i32 {k}"));
            lines.push(format!(
                "  {p} = getelementptr inbounds {elem}, {ptr_prefix} {ptr_name}, i64 {k}"
            ));
            lines.push(format!("  store {elem} {x}, {ptr_prefix} {p}, align 4"));
        }
        lines
    }
}

fn rewrite_function_body(body: &[&str], widen: bool) -> Option<Vec<String>> {
    let defs: BTreeMap<String, Def> = body
        .iter()
        .filter_map(|line| parse_def(line))
        .map(|d| (d.name.to_string(), d))
        .collect();

    let mut comp_elem: HashMap<String, String> = HashMap::default();

    let (pointees, mismatches) = classify(&defs);
    if let Some(elem) = unify_mismatch_elem(&mismatches, &pointees) {
        for m in component(&defs, &mismatches) {
            comp_elem.entry(m).or_insert_with(|| elem.clone());
        }
    }
    if widen {
        for (members, elem) in widen_targets(body, &defs) {
            for m in members {
                comp_elem.entry(m).or_insert_with(|| elem.clone());
            }
        }
    }

    if comp_elem.is_empty() {
        return None;
    }
    if body.iter().any(|l| l.contains(VSM_NAME_PREFIX)) {
        return None;
    }
    let mut rw = Rewriter { counter: 0 };
    let mut out: Vec<String> = Vec::with_capacity(body.len());
    for line in body {
        if let Some(rewritten) = rewrite_line(line, &comp_elem, &mut rw) {
            out.extend(rewritten);
        } else {
            out.push(line.to_string());
        }
    }
    Some(out)
}

fn unify_mismatch_elem(
    mismatches: &HashSet<String>,
    pointees: &HashMap<String, Pointee>,
) -> Option<String> {
    let mut elem: Option<String> = None;
    for m in mismatches {
        if let Some(Pointee::Scalar(e)) = pointees.get(m) {
            match &elem {
                Some(prev) if prev != e => return None,
                _ => elem = Some(e.clone()),
            }
        }
    }
    elem
}

fn rewrite_line(
    line: &str,
    comp_elem: &HashMap<String, String>,
    rw: &mut Rewriter,
) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if let Some(def) = parse_def(line) {
        match def.opcode {
            "getelementptr" => {
                let elem = comp_elem.get(def.name)?;
                let g = parse_gep(def.rest)?;
                let (e, lanes) = vec_elem_lanes(g.src_ty)?;
                if &e != elem {
                    return None;
                }
                return Some(rw.rewrite_gep(def.name, &g, &e, lanes));
            }
            "load" => {
                let chunks = split_top_level(def.rest, ',');
                if chunks.len() < 2 {
                    return None;
                }
                let (e, lanes) = vec_elem_lanes(chunks[0].trim())?;
                let (ptr_prefix, ptr_name) = split_ptr_operand(chunks[1])?;
                let elem = comp_elem.get(ptr_name)?;
                if &e != elem {
                    return None;
                }
                return Some(rw.split_load(def.name, &e, lanes, ptr_prefix, ptr_name));
            }
            _ => return None,
        }
    }
    if let Some(rest) = trimmed.strip_prefix("store ") {
        let chunks = split_top_level(rest, ',');
        if chunks.len() < 2 {
            return None;
        }
        let (ty, value) = split_ptr_operand(chunks[0])?;
        let (e, lanes) = vec_elem_lanes(ty)?;
        let (ptr_prefix, ptr_name) = split_ptr_operand(chunks[1])?;
        let elem = comp_elem.get(ptr_name)?;
        if &e != elem {
            return None;
        }
        return Some(rw.split_store(value, &e, lanes, ptr_prefix, ptr_name));
    }
    None
}

#[cfg(test)]
pub(in crate::native) fn lower_with_widen_for_test(san_ll: &str) -> String {
    lower_impl(san_ll, true).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cyclic_scalar_vector_merge_resolves_the_same_way_every_run() {
        let src = "\
define void @k(ptr addrspace(1) %fp, ptr addrspace(1) %ip, i64 %n) {
entry:
  %sf = getelementptr inbounds float, ptr addrspace(1) %fp, i64 %n
  %si = getelementptr inbounds i32, ptr addrspace(1) %ip, i64 %n
  %vf = getelementptr inbounds <4 x float>, ptr addrspace(1) %fp, i64 %n
  br label %loop
loop:
  %a = phi ptr addrspace(1) [ %vf, %entry ], [ %b, %loop ]
  %b = phi ptr addrspace(1) [ %si, %entry ], [ %a, %loop ]
  %c = phi ptr addrspace(1) [ %sf, %entry ], [ %a, %loop ]
  %v = load <4 x float>, ptr addrspace(1) %c, align 16
  store <4 x float> %v, ptr addrspace(1) %c, align 16
  br label %loop
}
";
        let out = lower_vector_scalar_pointer_merge(src);
        assert!(
            !out.contains("load <4 x float>"),
            "the mismatch fixpoint was not the one reached:\n{out}"
        );
        assert!(
            !out.contains("store <4 x float>"),
            "the mismatch fixpoint was not the one reached:\n{out}"
        );
        assert!(
            !out.contains("getelementptr inbounds <4 x float>"),
            "the mismatch fixpoint was not the one reached:\n{out}"
        );
    }

    #[test]
    fn scalar_vector_pointer_merge_is_scalarized() {
        let src = "\
define void @k(ptr addrspace(1) %src, ptr addrspace(1) %dst, i64 %n) {
entry:
  %s0 = getelementptr inbounds float, ptr addrspace(1) %src, i64 %n
  %d0 = getelementptr inbounds float, ptr addrspace(1) %dst, i64 %n
  br label %loop
loop:
  %p = phi ptr addrspace(1) [ %pn, %loop ], [ %s0, %entry ]
  %q = phi ptr addrspace(1) [ %qn, %loop ], [ %d0, %entry ]
  %v = load <4 x float>, ptr addrspace(1) %p, align 16
  store <4 x float> %v, ptr addrspace(1) %q, align 16
  %pn = getelementptr inbounds <4 x float>, ptr addrspace(1) %p, i64 %n
  %qn = getelementptr inbounds <4 x float>, ptr addrspace(1) %q, i64 %n
  br label %loop
}
";
        let out = lower_vector_scalar_pointer_merge(src);
        assert!(
            !out.contains("load <4 x float>"),
            "vector load not split:\n{out}"
        );
        assert!(
            !out.contains("store <4 x float>"),
            "vector store not split:\n{out}"
        );
        assert!(
            !out.contains("getelementptr inbounds <4 x float>"),
            "vector gep not scalarized:\n{out}"
        );
        assert!(out.contains("mul i64 %n, 4"), "stride not scaled:\n{out}");
        assert_eq!(
            out.matches("load float,").count(),
            4,
            "want 4 loads:\n{out}"
        );
        assert!(
            out.contains("%v = insertelement <4 x float>"),
            "vector not rebuilt into %v:\n{out}"
        );
        assert_eq!(
            out.matches("extractelement <4 x float>").count(),
            4,
            "want 4 extracts:\n{out}"
        );
    }

    #[test]
    fn unrelated_module_is_untouched() {
        let src = "\
define void @k(ptr addrspace(1) %src) {
entry:
  %p = getelementptr inbounds <4 x float>, ptr addrspace(1) %src, i64 0
  %v = load <4 x float>, ptr addrspace(1) %p, align 16
  ret void
}
";
        assert_eq!(lower_vector_scalar_pointer_merge(src), src);
    }

    #[test]
    fn use_width_whole_vs_part_phi_is_scalarized_only_when_widen() {
        let src = "\
define void @k(ptr addrspace(1) %m, ptr addrspace(1) %n, i1 %c, i64 %i) {
entry:
  %a = getelementptr inbounds float, ptr addrspace(1) %m, i64 %i
  %b = getelementptr inbounds float, ptr addrspace(1) %n, i64 %i
  br label %loop
loop:
  %p = phi ptr addrspace(1) [ %a, %entry ], [ %b, %loop ]
  %v = load <4 x float>, ptr addrspace(1) %p, align 16
  br label %loop
}
";
        assert_eq!(lower_impl(src, false), src);

        let out = lower_impl(src, true);
        assert!(
            !out.contains("load <4 x float>"),
            "vector load not split under widen:\n{out}"
        );
        assert_eq!(
            out.matches("load float,").count(),
            4,
            "want 4 scalar loads:\n{out}"
        );
        assert!(
            out.contains("%v = insertelement <4 x float>"),
            "vector not rebuilt into %v:\n{out}"
        );
        assert!(out.contains("%a = getelementptr inbounds float"), "{out}");
        assert!(out.contains("%b = getelementptr inbounds float"), "{out}");
    }

    #[test]
    fn use_width_single_pointer_mixed_width_is_scalarized() {
        let src = "\
define void @k(ptr addrspace(1) %buf) {
entry:
  %v = load <4 x float>, ptr addrspace(1) %buf, align 16
  %s = load float, ptr addrspace(1) %buf, align 4
  ret void
}
";
        assert_eq!(lower_impl(src, false), src);
        let out = lower_impl(src, true);
        assert!(!out.contains("load <4 x float>"), "{out}");
        assert_eq!(out.matches("load float,").count(), 5, "{out}");
    }

    #[test]
    fn use_width_reinterpret_mix_is_not_widened() {
        let src = "\
define void @k(ptr addrspace(1) %buf) {
entry:
  %v = load <4 x float>, ptr addrspace(1) %buf, align 16
  %w = load i32, ptr addrspace(1) %buf, align 4
  ret void
}
";
        assert_eq!(lower_impl(src, true), src);
    }

    #[test]
    fn use_width_logical_workgroup_pointer_is_not_widened() {
        let src = "\
define void @k(ptr addrspace(3) %tg) {
entry:
  %v = load <4 x float>, ptr addrspace(3) %tg, align 16
  %s = load float, ptr addrspace(3) %tg, align 4
  ret void
}
";
        assert_eq!(lower_impl(src, true), src);
    }

    #[test]
    fn use_width_aggregate_gep_component_is_excluded() {
        let src = "\
define void @k(ptr addrspace(1) %buf) {
entry:
  %p = getelementptr inbounds [16 x float], ptr addrspace(1) %buf, i64 0, i64 0
  %v = load <4 x float>, ptr addrspace(1) %p, align 16
  %q = getelementptr inbounds [16 x float], ptr addrspace(1) %buf, i64 0, i64 4
  %s = load float, ptr addrspace(1) %q, align 4
  ret void
}
";
        assert_eq!(lower_impl(src, true), src);
    }

    #[test]
    fn use_width_memory_reloaded_pointer_component_is_excluded() {
        let src = "\
define void @k(ptr addrspace(1) %slot, i64 %i) {
entry:
  %p = load ptr addrspace(1), ptr addrspace(1) %slot, align 8
  %s = load float, ptr addrspace(1) %p, align 4
  %g = getelementptr inbounds <4 x float>, ptr addrspace(1) %p, i64 %i
  %v = load <4 x float>, ptr addrspace(1) %g, align 16
  ret void
}
";
        assert_eq!(lower_impl(src, true), src);
        let ssa = src.replace(
            "  %p = load ptr addrspace(1), ptr addrspace(1) %slot, align 8\n",
            "",
        );
        let ssa = ssa.replace("%slot, i64 %i", "%p, i64 %i");
        assert!(
            !lower_impl(&ssa, true).contains("load <4 x float>"),
            "SSA-visible whole-vs-part should still widen:\n{}",
            lower_impl(&ssa, true)
        );
    }

    #[test]
    fn use_width_subword_component_is_excluded() {
        let i8_src = "\
define void @k(ptr addrspace(1) %b, i64 %i) {
entry:
  %v = load <4 x i8>, ptr addrspace(1) %b, align 4
  %p = getelementptr inbounds i8, ptr addrspace(1) %b, i64 4
  %s = load i8, ptr addrspace(1) %p, align 1
  ret void
}
";
        assert_eq!(lower_impl(i8_src, true), i8_src);
        let f32_src = i8_src
            .replace("<4 x i8>", "<4 x float>")
            .replace("inbounds i8", "inbounds float")
            .replace("load i8", "load float")
            .replace("align 4", "align 16");
        assert!(
            !lower_impl(&f32_src, true).contains("load <4 x float>"),
            "float whole-vs-part should still widen:\n{}",
            lower_impl(&f32_src, true)
        );
    }

    #[test]
    fn scalar_byte_width_recognizes_primitive_tokens() {
        assert_eq!(scalar_byte_width("i8"), Some(1));
        assert_eq!(scalar_byte_width("half"), Some(2));
        assert_eq!(scalar_byte_width("i16"), Some(2));
        assert_eq!(scalar_byte_width("float"), Some(4));
        assert_eq!(scalar_byte_width("i32"), Some(4));
        assert_eq!(scalar_byte_width("i64"), Some(8));
        assert_eq!(scalar_byte_width("i24"), Some(3));
        assert_eq!(scalar_byte_width("ptr"), None);
    }

    #[test]
    fn pure_vector_merge_is_untouched() {
        let src = "\
define void @k(ptr addrspace(1) %a, ptr addrspace(1) %b, i1 %c) {
entry:
  %pa = getelementptr inbounds <4 x float>, ptr addrspace(1) %a, i64 0
  %pb = getelementptr inbounds <4 x float>, ptr addrspace(1) %b, i64 0
  %m = select i1 %c, ptr addrspace(1) %pa, ptr addrspace(1) %pb
  %v = load <4 x float>, ptr addrspace(1) %m, align 16
  ret void
}
";
        assert!(matches!(
            lower_vector_scalar_pointer_merge(src),
            Cow::Borrowed(value) if std::ptr::eq(value, src)
        ));
    }
}
