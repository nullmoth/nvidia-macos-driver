//! `array<sampler, N>` entry arguments read with a runtime index.
//!
//! An entry argument `array<sampler, N>` arrives as a byval `[N x ptr addrspace(2)]` and is read as
//!
//! ```llvm
//! %g = getelementptr inbounds [N x %"struct.metal::sampler"], ptr %p, i64 0, i64 %i, i32 0
//! %s = load ptr addrspace(2), ptr %g
//! %r = call ... @air.sample_texture_2d...(ptr addrspace(1) %tex, ptr addrspace(2) %s, ...)
//! ```
//!
//! The entry lowering gives every element its own sampler variable at its own binding (the plugin's layout holds one
//! sampler per binding), so an element read with a constant index is a plain variable. A runtime index would have to
//! choose between sampler variables, and neither a sampler nor a pointer to one can be the operand of a select or phi
//! in Vulkan SPIR-V. So the choice is made after the sample instead: the call is repeated once per element with that
//! element's constant load, and the results are selected on `%i == k`. Every repeat executes, so implicit derivatives
//! stay defined. A sampler load is rewritten only when all its uses are the sampler operand of a sample or gather call;
//! anything else is left as written and refused later by the sampler pass with its usual message.

use std::borrow::Cow;
use std::collections::HashMap;

/// Largest array expanded. `air.max_samplers` is 16, the most a stage has.
const MAX_ELEMENTS: usize = 16;

pub(crate) fn lower_dynamic_sampler_arrays(ll: &str) -> Cow<'_, str> {
    if !ll.contains("addrspace(2)") || !ll.contains("[") {
        return Cow::Borrowed(ll);
    }
    let lines: Vec<&str> = ll.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut changed = false;
    let mut start = 0;
    while start < lines.len() {
        if lines[start].starts_with("define ") {
            let end = (start..lines.len()).find(|&i| lines[i] == "}").unwrap_or(lines.len() - 1);
            match rewrite_function(&lines[start..=end]) {
                Some(new) => {
                    changed = true;
                    out.extend(new);
                }
                None => out.extend(lines[start..=end].iter().map(|l| l.to_string())),
            }
            start = end + 1;
        } else {
            out.push(lines[start].to_string());
            start += 1;
        }
    }
    if changed {
        Cow::Owned(out.join("\n"))
    } else {
        Cow::Borrowed(ll)
    }
}

struct DynamicLoad {
    /// `%s`, the loaded sampler
    value: String,
    /// `ptr %p` operand base of the array and its element count
    base: String,
    count: usize,
    /// `i64 %i` index operand (value only, with its type)
    index: String,
    index_ty: String,
    /// element access spelling: true for `[N x %"struct.metal::sampler"] ... , i32 0`, false for `[N x ptr addrspace(2)]`
    wrapped: bool,
    align: String,
    gep_line: usize,
    load_line: usize,
}

fn rewrite_function(body: &[&str]) -> Option<Vec<String>> {
    // pass 1: geps by result name
    let mut geps: HashMap<String, (String, usize, String, String, bool, usize)> = HashMap::new();
    for (i, line) in body.iter().enumerate() {
        let t = line.trim();
        let Some((res, rest)) = t.split_once(" = getelementptr ") else { continue };
        let rest = rest.strip_prefix("inbounds ").unwrap_or(rest);
        let Some(after) = rest.strip_prefix('[') else { continue };
        let Some((count_text, after)) = after.split_once(" x ") else { continue };
        let Ok(count) = count_text.parse::<usize>() else { continue };
        let (wrapped, after) = if let Some(a) = after.strip_prefix("%\"struct.metal::sampler\"], ") {
            (true, a)
        } else if let Some(a) = after.strip_prefix("ptr addrspace(2)], ") {
            (false, a)
        } else {
            continue;
        };
        let parts = super::air_text::split_args(after);
        // ptr %p, i64 0, i64 %i [, i32 0]
        let want = if wrapped { 4 } else { 3 };
        if parts.len() != want || !parts[0].starts_with("ptr ") || parts[1] != "i64 0" {
            continue;
        }
        let Some((ity, idx)) = parts[2].split_once(' ') else { continue };
        if !idx.starts_with('%') || (wrapped && parts[3] != "i32 0") || count < 2 || count > MAX_ELEMENTS {
            continue; // constant indices are the entry lowering's job
        }
        geps.insert(res.to_string(), (parts[0].clone(), count, idx.to_string(), ity.to_string(), wrapped, i));
    }
    if geps.is_empty() {
        return None;
    }
    // pass 2: loads of those geps
    let mut loads: Vec<DynamicLoad> = Vec::new();
    for (i, line) in body.iter().enumerate() {
        let t = line.trim();
        let Some((res, rest)) = t.split_once(" = load ptr addrspace(2), ptr ") else { continue };
        let (gname, tail) = rest.split_once(',').map_or((rest, ""), |(g, t)| (g, t));
        let Some((base, count, index, index_ty, wrapped, gep_line)) = geps.get(gname.trim()).cloned() else { continue };
        let align = tail.split(',').map(str::trim).find(|p| p.starts_with("align ")).unwrap_or("align 8").to_string();
        loads.push(DynamicLoad { value: res.to_string(), base, count, index, index_ty, wrapped, align, gep_line, load_line: i });
    }
    if loads.is_empty() {
        return None;
    }
    // pass 3: every use of a loaded sampler must be argument 1 of a sample or gather call
    let mut ok: Vec<&DynamicLoad> = Vec::new();
    'load: for l in &loads {
        for (i, line) in body.iter().enumerate() {
            if i == l.load_line || !mentions(line, &l.value) {
                continue;
            }
            match sampler_call(line) {
                Some(call) if call.args.len() > 1 && last_token(&call.args[1]) == l.value && (0..call.args.len()).all(|a| a == 1 || last_token(&call.args[a]) != l.value) => {}
                _ => continue 'load,
            }
        }
        ok.push(l);
    }
    if ok.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() + 32);
    for (i, line) in body.iter().enumerate() {
        if let Some(l) = ok.iter().find(|l| l.load_line == i) {
            for k in 0..l.count {
                let elem = if l.wrapped {
                    format!("getelementptr inbounds [{} x %\"struct.metal::sampler\"], {}, i64 0, {} {}, i32 0", l.count, l.base, l.index_ty, k)
                } else {
                    format!("getelementptr inbounds [{} x ptr addrspace(2)], {}, i64 0, {} {}", l.count, l.base, l.index_ty, k)
                };
                out.push(format!("  {} = {}", elem_ptr(&l.value, k), elem));
                out.push(format!("  {} = load ptr addrspace(2), ptr {}, {}", elem_val(&l.value, k), elem_ptr(&l.value, k), l.align));
            }
            continue;
        }
        if ok.iter().any(|l| l.gep_line == i) && !loads_other_use(body, i, &ok) {
            continue; // the index gep has no reader left
        }
        if let Some(l) = ok.iter().find(|l| mentions(line, &l.value)) {
            out.extend(expand_call(line, l)?);
            continue;
        }
        out.push(line.to_string());
    }
    Some(out)
}

fn loads_other_use(body: &[&str], gep_line: usize, ok: &[&DynamicLoad]) -> bool {
    let name = match body[gep_line].trim().split_once(" = ") {
        Some((n, _)) => n,
        None => return true,
    };
    body.iter().enumerate().any(|(i, l)| i != gep_line && mentions(l, name) && !ok.iter().any(|o| o.load_line == i))
}

fn elem_ptr(value: &str, k: usize) -> String {
    format!("%sa.{}.p{k}", &value[1..])
}
fn elem_val(value: &str, k: usize) -> String {
    format!("%sa.{}.s{k}", &value[1..])
}

fn mentions(line: &str, name: &str) -> bool {
    let b = line.as_bytes();
    let mut from = 0;
    while let Some(p) = line[from..].find(name) {
        let at = from + p;
        let end = at + name.len();
        let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'.' || b[at - 1] == b'_' || b[at - 1] == b'%');
        let after_ok = end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'.' || b[end] == b'_' || b[end] == b'$' || b[end] == b'-');
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn last_token(arg: &str) -> &str {
    arg.split_whitespace().last().unwrap_or("")
}

struct SamplerCall {
    result: String,
    head: String, // up to and including "@callee("
    ret: String,
    args: Vec<String>,
    tail: String, // from ")" on
}

fn sampler_call(line: &str) -> Option<SamplerCall> {
    let t = line.trim();
    let (result, rest) = t.split_once(" = ")?;
    let at = rest.find("@air.")?;
    let callee_end = at + rest[at..].find('(')?;
    let callee = &rest[at + 1..callee_end];
    let is_sample = callee.starts_with("air.sample_texture") || callee.starts_with("air.sample_depth")
        || callee.starts_with("air.sample_compare") || callee.starts_with("air.gather");
    if !is_sample || !rest.contains("call ") {
        return None;
    }
    let close = matching_paren(rest, callee_end)?;
    let args = super::air_text::split_args(&rest[callee_end + 1..close]);
    let before = &rest[..at];
    let after_call = before.split_once("call ")?.1;
    let ret = after_call
        .split_whitespace()
        .skip_while(|w| matches!(*w, "fast" | "nnan" | "ninf" | "nsz" | "arcp" | "contract" | "afn" | "reassoc"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(SamplerCall {
        result: result.to_string(),
        head: format!("{}@{}(", before, callee),
        ret,
        args,
        tail: rest[close..].to_string(),
    })
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

fn expand_call(line: &str, l: &DynamicLoad) -> Option<Vec<String>> {
    let call = sampler_call(line)?;
    let res = call.result.trim_start_matches('%').to_string();
    let mut out = Vec::new();
    for k in 0..l.count {
        let mut args = call.args.clone();
        let a = &args[1];
        let cut = a.rfind(&l.value)?;
        args[1] = format!("{}{}", &a[..cut], elem_val(&l.value, k));
        out.push(format!("  %sa.{res}.r{k} = {}{}{}", call.head, args.join(", "), call.tail));
    }
    for k in 1..l.count {
        out.push(format!("  %sa.{res}.c{k} = icmp eq {} {}, {k}", l.index_ty, l.index));
    }
    let fields: Vec<String> = if call.ret.starts_with('{') {
        let inner = call.ret.trim_start_matches('{').trim_end_matches('}').trim();
        super::air_text::split_args(inner)
    } else {
        vec![call.ret.clone()]
    };
    let aggregate = call.ret.starts_with('{');
    let mut picked = Vec::new();
    for (f, ty) in fields.iter().enumerate() {
        let part = |k: usize, out: &mut Vec<String>| -> String {
            if aggregate {
                let n = format!("%sa.{res}.e{f}_{k}");
                out.push(format!("  {n} = extractvalue {} %sa.{res}.r{k}, {f}", call.ret));
                n
            } else {
                format!("%sa.{res}.r{k}")
            }
        };
        let mut prev = part(0, &mut out);
        for k in 1..l.count {
            let cur = part(k, &mut out);
            let n = format!("%sa.{res}.v{f}_{k}");
            out.push(format!("  {n} = select i1 %sa.{res}.c{k}, {ty} {cur}, {ty} {prev}"));
            prev = n;
        }
        picked.push(prev);
    }
    if aggregate {
        let mut acc = "poison".to_string();
        for (f, ty) in fields.iter().enumerate() {
            let name = if f + 1 == fields.len() { format!("%{res}") } else { format!("%sa.{res}.i{f}") };
            out.push(format!("  {name} = insertvalue {} {acc}, {ty} {}, {f}", call.ret, picked[f]));
            acc = name;
        }
    } else {
        // name the final select as the call's own result
        let last = out.last_mut()?;
        let (_, rest) = last.split_once(" = ")?;
        *last = format!("  %{res} = {rest}");
        let _ = picked;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IR: &str = "define void @k(ptr readonly byval([2 x ptr addrspace(2)]) %1, ptr addrspace(1) %2, i32 %3) {\n\
  %9 = zext i32 %3 to i64\n\
  %10 = bitcast ptr %1 to ptr\n\
  %11 = getelementptr inbounds [2 x %\"struct.metal::sampler\"], ptr %10, i64 0, i64 %9, i32 0\n\
  %12 = load ptr addrspace(2), ptr %11, align 8, !tbaa.struct !23\n\
  %15 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly %14, ptr addrspace(2) readonly captures(none) %12, <2 x float> splat (float 5.000000e-01), i1 true) #5, !alias.scope !24\n\
  ret void\n\
}";

    #[test]
    fn a_runtime_indexed_sampler_becomes_one_sample_per_element_and_a_select() {
        let out = lower_dynamic_sampler_arrays(IR);
        assert!(!out.contains("i64 %9, i32 0"), "{out}");
        assert!(out.contains("%sa.12.s0 = load ptr addrspace(2), ptr %sa.12.p0, align 8"), "{out}");
        assert!(out.contains("%sa.15.r1 = tail call { <4 x float>, i8 } @air.sample_texture_2d.v4f32(ptr addrspace(1) readonly %14, ptr addrspace(2) readonly captures(none) %sa.12.s1,"), "{out}");
        assert!(out.contains("%sa.15.c1 = icmp eq i64 %9, 1"), "{out}");
        assert!(out.contains("select i1 %sa.15.c1, <4 x float> %sa.15.e0_1, <4 x float> %sa.15.e0_0"), "{out}");
        assert!(out.contains("%15 = insertvalue { <4 x float>, i8 } %sa.15.i0, i8 %sa.15.v1_1, 1"), "{out}");
    }

    #[test]
    fn a_sampler_with_another_use_is_left_alone() {
        let ir = IR.replace("ret void", "store ptr addrspace(2) %12, ptr addrspace(1) %2\nret void");
        assert!(matches!(lower_dynamic_sampler_arrays(&ir), Cow::Borrowed(_)));
    }

    #[test]
    fn a_constant_index_is_not_touched() {
        let ir = IR.replace("i64 %9, i32 0", "i64 1, i32 0");
        assert!(matches!(lower_dynamic_sampler_arrays(&ir), Cow::Borrowed(_)));
    }
}
