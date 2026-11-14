//! Metal ray-traced curves (`intersector<curve_data>`) on Vulkan ray queries, which have no curve primitive.
//!
//! The driver builds a curve geometry as one axis-aligned box per segment and keeps the segment's control points in the
//! structure's side table, the buffer bound at the reserved binding beside the structure (reflect
//! `RAY_INSTANCE_USER_ID_TABLE_BINDING`). Its 32-bit words:
//!
//! ```text
//! word 2*i     user instance id of instance i (an instance structure; unused for a primitive structure)
//! word 2*i+1   offset of the curve block of instance i's primitive structure, 0 = no curves (i = 0 for a primitive
//!              structure, which has no instances)
//! block b:     b[0] = geometry count, b[1+g] = offset of geometry g's segment records from b, 0 = not a curve geometry
//! record:      16 floats (x, y, z, radius) of control points 0..3, then MTLCurveBasis, control points per segment,
//!              MTLCurveType (1 = flat) and MTLCurveEndCaps: RECORD_WORDS words per segment
//! ```
//!
//! An `air.intersect.*.curve_data` call with no intersection function table becomes a call to a function defined here
//! that runs the same ray as an intersection query: a box candidate that has a curve record goes to the built-in curve
//! intersector (`curve_intersect.ll`, generated from tests/curves/curve_intersect.metal) and is committed at its
//! distance, other box candidates are not hits (Metal runs no function for them without a table), triangle candidates
//! are committed as the table-less intersector does. The committed box reports intersection_type curve and the curve
//! parameter of the committed hit.
//!
//! Left untouched, so the translator still names them unsupported: a curve query with an intersection function table
//! or payload, multi-level instancing, motion, world-space data, function buffers, and `intersection_query<curve_data>`.

use super::air_text::{last_token, split_args};
use std::borrow::Cow;
use std::collections::BTreeSet;

/// Words per segment record; the plugin writes records of this size (NVMTLCurves.m, NVMTL_CURVE_RECORD_WORDS).
pub(crate) const RECORD_WORDS: u32 = 20;
const INTERSECTOR: &str = include_str!("curve_intersect.ll");
/// The query getter that reads one word of the side table (lowered in passes/air_calls/ray_query.rs).
pub(crate) const SIDE_WORD: &str = "nvmtl_side_word";
const CURVE_TYPE: u32 = 3;
const BOX_TYPE: u32 = 2;

struct Shape {
    tags: String,
    query_suffix: String,
    instanced: bool,
    triangle_data: bool,
    params: Vec<String>,
    result_ty: String,
}

/// The type of a call operand: a bracketed aggregate or vector, `ptr` with its address space, or one word. A constant
/// operand such as `<3 x float> <float 0.0, ...>` holds spaces, so the type is read from the front.
fn arg_type(arg: &str) -> String {
    let a = arg.trim();
    if let Some(open) = a.chars().next().filter(|c| matches!(c, '<' | '{' | '[')) {
        let close = match open { '<' => '>', '{' => '}', _ => ']' };
        let mut depth = 0i32;
        for (i, ch) in a.char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return a[..=i].to_string();
                }
            }
        }
        return a.to_string();
    }
    let first = a.split_whitespace().next().unwrap_or("");
    if first == "ptr" {
        if let Some(rest) = a[3..].trim_start().strip_prefix("addrspace(") {
            if let Some(end) = rest.find(')') {
                return format!("ptr addrspace({})", &rest[..end]);
            }
        }
    }
    first.to_string()
}

fn shape_of(tags: &str, args: &[String], result_ty: &str) -> Option<Shape> {
    let tokens: Vec<&str> = tags.split('.').collect();
    if !tokens.contains(&"curve_data")
        || !tokens
            .iter()
            .all(|t| matches!(*t, "curve_data" | "instancing" | "triangle_data" | "extended_limits"))
    {
        return None;
    }
    let instanced = tokens.contains(&"instancing");
    let query_tags: Vec<&str> = tokens.iter().copied().filter(|t| *t != "curve_data").collect();
    let expected = if instanced { 20 } else { 18 };
    if args.len() != expected {
        return None;
    }
    let members = split_args(result_ty.trim().trim_start_matches('{').trim_end_matches('}'));
    let triangle_data = tokens.contains(&"triangle_data");
    let want = 5 + if instanced { 2 } else { 0 } + if triangle_data { 2 } else { 0 } + 1;
    if members.len() != want || members.last().map(String::as_str) != Some("float") {
        return None;
    }
    Some(Shape {
        tags: tags.to_string(),
        query_suffix: if query_tags.is_empty() { String::new() } else { format!(".{}", query_tags.join(".")) },
        instanced,
        triangle_data,
        params: args.iter().map(|a| arg_type(a)).collect(),
        result_ty: result_ty.trim().to_string(),
    })
}

fn helper_name(tags: &str) -> String {
    format!("@nvmtl.curve_trace.{tags}")
}

fn getter(qs: &str, what: &str, ty: &str, committed: bool, decls: &mut Vec<String>) -> String {
    let g = format!("@air.get_{}_{what}_intersection_query{qs}", if committed { "committed" } else { "candidate" });
    decls.push(format!("declare {ty} {g}(ptr)"));
    format!("call {ty} {g}(ptr %q)")
}

/// The blocks from `box:` (a box candidate of query `%q`) to `%h`, the built-in intersector's answer for it: a box with
/// no curve record branches to `nocurve`; a curve hit branches to `commit`, a miss to `loop`.
fn curve_candidate(qs: &str, instanced: bool, nocurve: &str, tmin: &str, tmax_ray: &str, decls: &mut Vec<String>) -> String {
    let word = format!("@air.get_candidate_{SIDE_WORD}_intersection_query{qs}");
    decls.push(format!("declare i32 {word}(ptr, i32)"));
    let mut get = |what: &str, ty: &str, committed: bool| getter(qs, what, ty, committed, decls);
    let mut b = String::from("box:\n");
    if instanced {
        b.push_str(&format!("  %iid = {}\n  %iw = shl i32 %iid, 1\n  %iw1 = or i32 %iw, 1\n", get("instance_id", "i32", false)));
    } else {
        b.push_str("  %iw1 = add i32 0, 1\n");
    }
    b.push_str(&format!(
        "  %blk = call i32 {word}(ptr %q, i32 %iw1)\n  %hasblk = icmp ne i32 %blk, 0\n  br i1 %hasblk, label %geo, label %{nocurve}\n"
    ));
    b.push_str(&format!(
        "geo:\n  %g = {}\n  %ng = call i32 {word}(ptr %q, i32 %blk)\n  %gin = icmp ult i32 %g, %ng\n  br i1 %gin, label %goff, label %{nocurve}\n",
        get("geometry_id", "i32", false)
    ));
    b.push_str(&format!(
        "goff:\n  %ga = add i32 %blk, 1\n  %ga2 = add i32 %ga, %g\n  %go = call i32 {word}(ptr %q, i32 %ga2)\n  %isc = icmp ne i32 %go, 0\n  br i1 %isc, label %rec, label %{nocurve}\n"
    ));
    b.push_str(&format!(
        "rec:\n  %p = {}\n  %pr = mul i32 %p, {RECORD_WORDS}\n  %r0 = add i32 %blk, %go\n  %r = add i32 %r0, %pr\n",
        get("primitive_id", "i32", false)
    ));
    for k in 0..RECORD_WORDS {
        b.push_str(&format!("  %wa{k} = add i32 %r, {k}\n  %w{k} = call i32 {word}(ptr %q, i32 %wa{k})\n"));
        if k < 16 {
            b.push_str(&format!("  %f{k} = bitcast i32 %w{k} to float\n"));
        }
    }
    for c in 0..4u32 {
        let mut prev = "undef".to_string();
        for l in 0..4u32 {
            let v = format!("%cp{c}.{l}");
            b.push_str(&format!("  {v} = insertelement <4 x float> {prev}, float %f{}, i64 {l}\n", c * 4 + l));
            prev = v;
        }
    }
    b.push_str(&format!("  %o = {}\n  %d = {}\n", get("ray_origin", "<3 x float>", false), get("ray_direction", "<3 x float>", false)));
    b.push_str(&format!(
        "  %cty = {}\n  %cd = {}\n  %none = icmp eq i32 %cty, 0\n  %tmax = select i1 %none, float {tmax_ray}, float %cd\n",
        get("intersection_type", "i32", true),
        get("distance", "float", true)
    ));
    b.push_str(&format!("  %h = call <4 x float> @nvmtl.curve_intersect(<4 x float> %cp0.3, <4 x float> %cp1.3, <4 x float> %cp2.3, <4 x float> %cp3.3, i32 %w16, i32 %w17, i32 %w18, i32 %w19, <3 x float> %o, <3 x float> %d, float {tmin}, float %tmax)\n"));
    b.push_str("  %hx = extractelement <4 x float> %h, i64 0\n  %hit = fcmp ogt float %hx, 5.000000e-01\n  br i1 %hit, label %commit, label %loop\n");
    b
}

fn emit_helper(s: &Shape) -> (String, Vec<String>) {
    let qs = &s.query_suffix;
    let table = if s.instanced { 6 } else { 5 };
    let mut decls = Vec::new();
    let mut get_decls = Vec::new();
    let mut get = |what: &str, ty: &str, committed: bool| getter(qs, what, ty, committed, &mut get_decls);
    let word = format!("@air.get_candidate_{SIDE_WORD}_intersection_query{qs}");
    let params: Vec<String> = s.params.iter().enumerate().map(|(i, t)| format!("{t} %a{i}")).collect();
    let reset_args: Vec<String> = s
        .params
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < table || *i > table + 2)
        .map(|(i, t)| format!("{t} %a{i}"))
        .collect();
    let reset_types: Vec<String> = s
        .params
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < table || *i > table + 2)
        .map(|(_, t)| t.clone())
        .collect();
    let mut b = String::new();
    b.push_str(&format!("define internal {} {}({}) {{\nentry:\n", s.result_ty, helper_name(&s.tags), params.join(", ")));
    b.push_str(&format!("  %q = call ptr @air.allocate_intersection_query{qs}()\n"));
    b.push_str(&format!("  call void @air.reset_intersection_query{qs}(ptr %q, {})\n", reset_args.join(", ")));
    b.push_str("  %us = alloca float, align 4\n  store float -1.000000e+00, ptr %us, align 4\n  br label %loop\n");
    b.push_str(&format!("loop:\n  %more = call i1 @air.next_intersection_query{qs}(ptr %q)\n  br i1 %more, label %cand, label %done\n"));
    b.push_str(&format!(
        "cand:\n  %ty = {}\n  %isbox = icmp eq i32 %ty, {BOX_TYPE}\n  br i1 %isbox, label %box, label %tri\n",
        get("intersection_type", "i32", false)
    ));
    b.push_str(&format!(
        "tri:\n  call void @air.commit_triangle_intersection_intersection_query{qs}(ptr %q)\n  br label %loop\n"
    ));
    b.push_str(&curve_candidate(qs, s.instanced, "loop", "%a2", "%a3", &mut decls));
    b.push_str(&format!(
        "commit:\n  %ht = extractelement <4 x float> %h, i64 1\n  %hu = extractelement <4 x float> %h, i64 2\n  call void @air.commit_bounding_box_intersection_intersection_query{qs}(ptr %q, float %ht)\n  store float %hu, ptr %us, align 4\n  br label %loop\n"
    ));
    // the result: the committed hit, its box type reported as a curve, then the curve parameter last
    let members = split_args(s.result_ty.trim_start_matches('{').trim_end_matches('}'));
    let mut fields: Vec<(&str, usize)> = vec![("distance", 1), ("primitive_id", 2), ("geometry_id", 3)];
    if s.instanced {
        fields.extend([("instance_id", 5), ("user_instance_id", 6)]);
    }
    if s.triangle_data {
        let at = if s.instanced { 7 } else { 5 };
        fields.extend([("triangle_barycentric_coord", at), ("triangle_front_facing", at + 1)]);
    }
    b.push_str(&format!(
        "done:\n  %ct = {}\n  %ctb = icmp eq i32 %ct, {BOX_TYPE}\n  %rt = select i1 %ctb, i32 {CURVE_TYPE}, i32 %ct\n  %v0 = insertvalue {} undef, i32 %rt, 0\n",
        get("intersection_type", "i32", true),
        s.result_ty
    ));
    let mut prev = "%v0".to_string();
    for (k, (what, slot)) in fields.iter().enumerate() {
        let ty = &members[*slot];
        b.push_str(&format!(
            "  %c{k} = {}\n  %v{n} = insertvalue {} {prev}, {ty} %c{k}, {slot}\n",
            get(what, ty, true),
            s.result_ty,
            n = k + 1
        ));
        prev = format!("%v{}", k + 1);
    }
    let last = members.len() - 1;
    b.push_str(&format!(
        "  %vp = insertvalue {} {prev}, {} null, 4\n  %cu = load float, ptr %us, align 4\n  %vu = insertvalue {} %vp, float %cu, {last}\n  call void @air.deallocate_intersection_query{qs}(ptr %q)\n  ret {} %vu\n}}\n",
        s.result_ty, members[4], s.result_ty, s.result_ty
    ));
    decls.extend(get_decls);
    decls.push(format!("declare ptr @air.allocate_intersection_query{qs}()"));
    decls.push(format!("declare void @air.deallocate_intersection_query{qs}(ptr)"));
    decls.push(format!("declare void @air.reset_intersection_query{qs}(ptr, {})", reset_types.join(", ")));
    decls.push(format!("declare i1 @air.next_intersection_query{qs}(ptr)"));
    decls.push(format!("declare void @air.commit_triangle_intersection_intersection_query{qs}(ptr)"));
    decls.push(format!("declare void @air.commit_bounding_box_intersection_intersection_query{qs}(ptr, float)"));
    decls.push(format!("declare i32 {word}(ptr, i32)"));
    (b, decls)
}

/// The value an argument names, and whether it is the null table (`null` or a call of air.get_null_intersection_function_table).
fn is_null_table(ll: &str, arg: &str) -> bool {
    let v = last_token(arg);
    if v == "null" {
        return true;
    }
    let def = format!("{v} = ");
    ll.lines()
        .map(str::trim_start)
        .find(|l| l.starts_with(&def))
        .is_some_and(|l| l.contains("@air.get_null_intersection_function_table"))
}

pub(crate) fn lower_curve_intersections(san_ll: &str) -> Cow<'_, str> {
    match lower_curve_queries(san_ll) {
        Some(q) => Cow::Owned(lower_curve_intersect_calls(&q).into_owned()),
        None => lower_curve_intersect_calls(san_ll),
    }
}

/// `next` of a curve query: the next candidate the shader sees. A box candidate with a curve record is run through the
/// built-in intersector and comes back as a curve candidate {more, 1, t, u} when it hits; a miss is skipped; any other
/// candidate comes back as it is {more, 0, 0, 0}. `tmax_ray` is the ray's own maximum distance (kept at reset).
fn emit_query_next(qs: &str, instanced: bool, decls: &mut Vec<String>) -> String {
    let name = format!("@nvmtl.curve_next{qs}");
    let ty = "{ i1, i32, float, float }";
    let mut b = format!("define internal {ty} {name}(ptr %q, float %tx) {{\nentry:\n  br label %loop\n");
    b.push_str(&format!("loop:\n  %more = call i1 @air.next_intersection_query{qs}(ptr %q)\n  br i1 %more, label %cand, label %none\n"));
    b.push_str(&format!("none:\n  ret {ty} zeroinitializer\n"));
    b.push_str(&format!(
        "cand:\n  %ty = {}\n  %isbox = icmp eq i32 %ty, 2\n  br i1 %isbox, label %box, label %pass\n",
        getter(qs, "intersection_type", "i32", false, decls)
    ));
    b.push_str(&format!("pass:\n  ret {ty} {{ i1 true, i32 0, float 0.000000e+00, float 0.000000e+00 }}\n"));
    let tmin = format!("@air.get_ray_min_distance_intersection_query{qs}");
    decls.push(format!("declare float {tmin}(ptr)"));
    let cand = curve_candidate(qs, instanced, "pass", "%tmin", "%tx", decls);
    // the ray's minimum distance is read in the record block, before the intersector call
    let cand = cand.replacen("  %o = ", &format!("  %tmin = call float {tmin}(ptr %q)\n  %o = "), 1);
    b.push_str(&cand);
    b.push_str(&format!(
        "commit:\n  %ht = extractelement <4 x float> %h, i64 1\n  %hu = extractelement <4 x float> %h, i64 2\n  %r1 = insertvalue {ty} {{ i1 true, i32 1, float 0.000000e+00, float 0.000000e+00 }}, float %ht, 2\n  %r2 = insertvalue {ty} %r1, float %hu, 3\n  ret {ty} %r2\n}}\n"
    ));
    decls.push(format!("declare i1 @air.next_intersection_query{qs}(ptr)"));
    b
}

/// `intersection_query<curve_data>` (`air.*_intersection_query.*curve_data`): the same query without curve_data, its
/// `next` wrapped by emit_query_next, and the curve state (candidate flag, distance and parameter; committed flag and
/// parameter; the ray's maximum distance) in Function variables beside the query. Returns None when the module has no
/// curve query.
fn lower_curve_queries(ll: &str) -> Option<String> {
    if !ll.contains("_intersection_query.") || !ll.contains("curve_data") {
        return None;
    }
    let mut out = String::with_capacity(ll.len() + 16384);
    let mut state: Vec<(String, String)> = Vec::new();
    let mut nexts: Vec<(String, bool)> = Vec::new();
    let mut decls: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut changed = false;
    for line in ll.lines() {
        let t = line.trim_start();
        let Some(at) = line.find("_intersection_query.") else { out.push_str(line); out.push('\n'); continue };
        let Some(open) = line[at..].find('(').map(|o| at + o) else { out.push_str(line); out.push('\n'); continue };
        let tags = &line[at + "_intersection_query.".len()..open];
        if !tags.split('.').any(|x| x == "curve_data") {
            out.push_str(line); out.push('\n'); continue;
        }
        if t.starts_with("declare ") {
            changed = true;
            continue; // the curve spellings are all replaced below
        }
        let base_tags: Vec<&str> = tags.split('.').filter(|x| *x != "curve_data").collect();
        if base_tags.iter().any(|x| !matches!(*x, "instancing" | "triangle_data" | "extended_limits")) {
            return None;
        }
        let qs = if base_tags.is_empty() { String::new() } else { format!(".{}", base_tags.join(".")) };
        let instanced = base_tags.contains(&"instancing");
        let Some(callee_at) = line[..at].rfind("@air.") else { return None };
        let stem = &line[callee_at + "@air.".len()..at];
        let close = line.rfind(')')?;
        let args = split_args(&line[open + 1..close]);
        let result = t.split_once(" = ").map(|(r, _)| r.trim().to_string());
        let indent = &line[..line.len() - t.len()];
        let renamed = format!("{}@air.{stem}_intersection_query{qs}{}", &line[..callee_at], &line[open..]);
        let query = args.first().map(|a| last_token(a).to_string()).unwrap_or_default();
        let st = |field: &str| -> Option<String> {
            state.iter().find(|(q, _)| *q == query).map(|(_, p)| format!("{p}.{field}"))
        };
        let k = n;
        n += 1;
        changed = true;
        let mut new_state = None;
        let mut emit = |s: String| { out.push_str(&s); out.push('\n'); };
        match stem {
            "allocate" => {
                let q = result.clone()?;
                let p = format!("%nvcq{k}");
                emit(renamed);
                for (f, ty, zero) in [("cf", "i32", "0"), ("ct", "float", "0.000000e+00"), ("cu", "float", "0.000000e+00"),
                                      ("kf", "i32", "0"), ("ku", "float", "0.000000e+00"), ("tx", "float", "0.000000e+00")] {
                    emit(format!("{indent}{p}.{f} = alloca {ty}, align 4"));
                    emit(format!("{indent}store {ty} {zero}, ptr {p}.{f}, align 4"));
                }
                new_state = Some((q, p));
            }
            "reset" => {
                emit(renamed);
                emit(format!("{indent}store i32 0, ptr {}, align 4", st("cf")?));
                emit(format!("{indent}store i32 0, ptr {}, align 4", st("kf")?));
                emit(format!("{indent}store {}, ptr {}, align 4", args.get(4)?.trim(), st("tx")?));
            }
            "next" => {
                let r = result.clone()?;
                let ty = "{ i1, i32, float, float }";
                emit(format!("{indent}%nvcn{k}.tx = load float, ptr {}, align 4", st("tx")?));
                emit(format!("{indent}%nvcn{k}.r = call {ty} @nvmtl.curve_next{qs}(ptr {query}, float %nvcn{k}.tx)"));
                emit(format!("{indent}{r} = extractvalue {ty} %nvcn{k}.r, 0"));
                for (i, f, fty) in [(1, "cf", "i32"), (2, "ct", "float"), (3, "cu", "float")] {
                    emit(format!("{indent}%nvcn{k}.{f} = extractvalue {ty} %nvcn{k}.r, {i}"));
                    emit(format!("{indent}store {fty} %nvcn{k}.{f}, ptr {}, align 4", st(f)?));
                }
                if !nexts.iter().any(|(q, _)| *q == qs) {
                    nexts.push((qs.clone(), instanced));
                }
            }
            "get_candidate_intersection_type" | "get_committed_intersection_type" => {
                let r = result.clone()?;
                let committed = stem.starts_with("get_committed");
                emit(format!("{indent}%nvct{k}.b = call i32 @air.{stem}_intersection_query{qs}(ptr {query})"));
                emit(format!("{indent}%nvct{k}.f = load i32, ptr {}, align 4", st(if committed { "kf" } else { "cf" })?));
                emit(format!("{indent}%nvct{k}.c = icmp ne i32 %nvct{k}.f, 0"));
                if committed {
                    emit(format!("{indent}%nvct{k}.x = icmp eq i32 %nvct{k}.b, 2"));
                    emit(format!("{indent}%nvct{k}.y = and i1 %nvct{k}.c, %nvct{k}.x"));
                    emit(format!("{indent}{r} = select i1 %nvct{k}.y, i32 3, i32 %nvct{k}.b"));
                } else {
                    emit(format!("{indent}{r} = select i1 %nvct{k}.c, i32 3, i32 %nvct{k}.b"));
                }
                decls.push(format!("declare i32 @air.{stem}_intersection_query{qs}(ptr)"));
            }
            "get_candidate_curve_parameter" => emit(format!("{indent}{} = load float, ptr {}, align 4", result.clone()?, st("cu")?)),
            "get_candidate_curve_distance" => emit(format!("{indent}{} = load float, ptr {}, align 4", result.clone()?, st("ct")?)),
            "get_committed_curve_parameter" => emit(format!("{indent}{} = load float, ptr {}, align 4", result.clone()?, st("ku")?)),
            "commit_curve_intersection" => {
                emit(format!("{indent}%nvcc{k}.t = load float, ptr {}, align 4", st("ct")?));
                emit(format!("{indent}call void @air.commit_bounding_box_intersection_intersection_query{qs}(ptr {query}, float %nvcc{k}.t)"));
                emit(format!("{indent}%nvcc{k}.u = load float, ptr {}, align 4", st("cu")?));
                emit(format!("{indent}store float %nvcc{k}.u, ptr {}, align 4", st("ku")?));
                emit(format!("{indent}store i32 1, ptr {}, align 4", st("kf")?));
                decls.push(format!("declare void @air.commit_bounding_box_intersection_intersection_query{qs}(ptr, float)"));
            }
            "commit_triangle_intersection" | "commit_bounding_box_intersection" => {
                emit(renamed);
                emit(format!("{indent}store i32 0, ptr {}, align 4", st("kf")?));
            }
            _ => emit(renamed),
        }
        if let Some(x) = new_state {
            state.push(x);
        }
        // every renamed call needs its declaration in the base spelling
        if let Some(d) = ll.lines().find(|l| l.trim_start().starts_with("declare ") && l.contains(&format!("@air.{stem}_intersection_query.{tags}("))) {
            decls.push(d.replace(&format!("@air.{stem}_intersection_query.{tags}("), &format!("@air.{stem}_intersection_query{qs}(")));
        }
    }
    if !changed {
        return None;
    }
    let mut tail = String::new();
    for (qs, instanced) in &nexts {
        tail.push_str(&emit_query_next(qs, *instanced, &mut decls));
        tail.push('\n');
    }
    let mut have: BTreeSet<String> = out
        .lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("declare ") || l.starts_with("define "))
        .filter_map(|l| l.find('@').map(|a| l[a..].split('(').next().unwrap_or("").to_string()))
        .collect();
    for d in decls {
        let g = d.find('@').map(|a| d[a..].split('(').next().unwrap_or("").to_string()).unwrap_or_default();
        if g.contains("curve_parameter") || g.contains("curve_distance") || g.contains("commit_curve") {
            continue;
        }
        if have.insert(g) {
            tail.push_str(&d);
            tail.push('\n');
        }
    }
    if !nexts.is_empty() && !out.contains("@nvmtl.curve_intersect(") {
        for l in INTERSECTOR.lines() {
            let t = l.trim_start();
            if t.starts_with(';') {
                continue;
            }
            if t.starts_with("declare ") {
                let g = t.find('@').map(|a| t[a..].split('(').next().unwrap_or("").to_string()).unwrap_or_default();
                if !have.insert(g) {
                    continue;
                }
            }
            tail.push_str(l);
            tail.push('\n');
        }
    }
    let insert_at = out
        .lines()
        .scan(0usize, |pos, l| {
            let start = *pos;
            *pos += l.len() + 1;
            Some((start, l))
        })
        .find(|(_, l)| l.starts_with('!') || l.starts_with("attributes #"))
        .map(|(s, _)| s)
        .unwrap_or(out.len());
    out.insert_str(insert_at, &tail);
    Some(out)
}

fn lower_curve_intersect_calls(san_ll: &str) -> Cow<'_, str> {
    if !san_ll.contains(".curve_data(") {
        return Cow::Borrowed(san_ll);
    }
    let mut helpers: Vec<Shape> = Vec::new();
    let mut out = String::with_capacity(san_ll.len() + 32768);
    let mut changed = false;
    for line in san_ll.lines() {
        let rewritten = (|| {
            let at = line.find("@air.intersect.")?;
            if !line.contains(" call ") || line.trim_start().starts_with("declare") {
                return None;
            }
            let open = at + line[at..].find('(')?;
            let tags = line[at + "@air.intersect.".len()..open].to_string();
            let close = {
                let mut depth = 0i32;
                let mut end = None;
                for (i, ch) in line[open..].char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                end?
            };
            let args = split_args(&line[open + 1..close]);
            let call_at = line.find(" call ")? + " call ".len();
            let result_ty = line[call_at..at].trim().trim_start_matches("fast ").to_string();
            let shape = shape_of(&tags, &args, &result_ty)?;
            if !shape.params.iter().any(|t| t.starts_with("ptr")) {
                return None;
            }
            let table = if shape.instanced { 6 } else { 5 };
            if !is_null_table(san_ll, &args[table]) || last_token(&args[table + 1]) != "null" {
                return None;
            }
            let name = helper_name(&tags);
            let replaced = format!("{}{}{}", &line[..at], name, &line[open..]);
            if !helpers.iter().any(|h| h.tags == shape.tags) {
                helpers.push(shape);
            }
            Some(replaced)
        })();
        match rewritten {
            Some(l) => {
                out.push_str(&l);
                changed = true;
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    if !changed {
        return Cow::Borrowed(san_ll);
    }
    let mut have: BTreeSet<String> = out
        .lines()
        .map(str::trim_start)
        .filter(|l| l.starts_with("declare ") || l.starts_with("define "))
        .filter_map(|l| l.find('@').map(|a| l[a..].split('(').next().unwrap_or("").to_string()))
        .collect();
    let mut tail = String::new();
    for h in &helpers {
        let (body, decls) = emit_helper(h);
        tail.push_str(&body);
        tail.push('\n');
        for d in decls {
            let g = d.find('@').map(|a| d[a..].split('(').next().unwrap_or("").to_string()).unwrap_or_default();
            if have.insert(g) {
                tail.push_str(&d);
                tail.push('\n');
            }
        }
    }
    let have_intersector = san_ll.contains("@nvmtl.curve_intersect(<4 x float>");
    for l in INTERSECTOR.lines().filter(|_| !have_intersector) {
        let t = l.trim_start();
        if t.starts_with(';') {
            continue;
        }
        if t.starts_with("declare ") {
            let g = t.find('@').map(|a| t[a..].split('(').next().unwrap_or("").to_string()).unwrap_or_default();
            if !have.insert(g) {
                continue;
            }
        }
        tail.push_str(l);
        tail.push('\n');
    }
    // the helpers go before the module's metadata, which LLVM text keeps last
    let insert_at = out
        .lines()
        .scan(0usize, |pos, l| {
            let start = *pos;
            *pos += l.len() + 1;
            Some((start, l))
        })
        .find(|(_, l)| l.starts_with('!') || l.starts_with("attributes #"))
        .map(|(s, _)| s)
        .unwrap_or(out.len());
    out.insert_str(insert_at, &tail);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMITIVE: &str = "define void @trace_curves(ptr addrspace(1) %0, ptr addrspace(1) %1) {
  %3 = tail call ptr addrspace(1) @air.get_null_intersection_function_table()
  %4 = tail call { i32, float, i32, i32, ptr addrspace(1), float } @air.intersect.curve_data(<3 x float> zeroinitializer, <3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, float 0.000000e+00, float 1.000000e+03, ptr addrspace(1) readonly %0, ptr addrspace(1) readonly %3, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 4, i32 -1, i32 -1, i32 0, i1 false)
  %5 = extractvalue { i32, float, i32, i32, ptr addrspace(1), float } %4, 5
  store float %5, ptr addrspace(1) %1, align 4
  ret void
}
declare ptr addrspace(1) @air.get_null_intersection_function_table()
declare { i32, float, i32, i32, ptr addrspace(1), float } @air.intersect.curve_data(<3 x float>, <3 x float>, float, float, ptr addrspace(1), ptr addrspace(1), ptr, i64, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1)
!0 = !{}
";

    #[test]
    fn a_table_less_curve_intersector_becomes_a_query_loop_with_the_curve_intersector() {
        let out = lower_curve_intersections(PRIMITIVE).into_owned();
        assert!(out.contains("call { i32, float, i32, i32, ptr addrspace(1), float } @nvmtl.curve_trace.curve_data("));
        assert!(!out.lines().any(|l| l.contains(" call ") && l.contains("@air.intersect.")));
        assert!(out.contains("call void @air.reset_intersection_query(ptr %q, <3 x float> %a0, <3 x float> %a1, float %a2, float %a3, ptr addrspace(1) %a4, i32 %a8,"), "{}", out.lines().find(|l| l.contains("reset_intersection_query(ptr %q")).unwrap_or(""));
        assert!(out.contains("define internal <4 x float> @nvmtl.curve_intersect("));
        assert!(out.contains(&format!("@air.get_candidate_{SIDE_WORD}_intersection_query(ptr %q, i32 %iw1)")));
        assert!(out.contains("%vu = insertvalue { i32, float, i32, i32, ptr addrspace(1), float } %vp, float %cu, 5"));
        // the helpers sit before the metadata
        assert!(out.find("@nvmtl.curve_intersect(").unwrap() < out.find("!0 = !{}").unwrap());
        assert_eq!(out.matches("declare float @air.dot.v3f32").count(), 1);
    }

    const QUERY: &str = "define void @k(ptr addrspace(1) %0, ptr addrspace(1) %1) {
  %3 = tail call ptr @air.allocate_intersection_query.curve_data()
  tail call void @air.reset_intersection_query.curve_data(ptr %3, <3 x float> zeroinitializer, <3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, float 0.000000e+00, float 1.000000e+03, ptr addrspace(1) readonly %0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 4, i32 -1, i32 -1, i32 0, i1 false)
  br label %4
4:
  %5 = tail call i1 @air.next_intersection_query.curve_data(ptr %3)
  br i1 %5, label %6, label %9
6:
  %7 = tail call i32 @air.get_candidate_intersection_type_intersection_query.curve_data(ptr %3)
  %8 = icmp eq i32 %7, 3
  tail call void @air.commit_curve_intersection_intersection_query.curve_data(ptr %3)
  br label %4
9:
  %10 = tail call i32 @air.get_committed_intersection_type_intersection_query.curve_data(ptr %3)
  %11 = tail call fast float @air.get_committed_curve_parameter_intersection_query.curve_data(ptr %3)
  store float %11, ptr addrspace(1) %1, align 4
  tail call void @air.deallocate_intersection_query.curve_data(ptr %3)
  ret void
}
declare ptr @air.allocate_intersection_query.curve_data()
declare void @air.reset_intersection_query.curve_data(ptr, <3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, i32, i32, i32, i32, i32, i32, i32, i32, i1)
declare i1 @air.next_intersection_query.curve_data(ptr)
declare i32 @air.get_candidate_intersection_type_intersection_query.curve_data(ptr)
declare void @air.commit_curve_intersection_intersection_query.curve_data(ptr)
declare i32 @air.get_committed_intersection_type_intersection_query.curve_data(ptr)
declare float @air.get_committed_curve_parameter_intersection_query.curve_data(ptr)
declare void @air.deallocate_intersection_query.curve_data(ptr)
!0 = !{}
";

    #[test]
    fn a_curve_query_becomes_a_plain_query_whose_next_runs_the_curve_intersector() {
        let out = lower_curve_intersections(QUERY).into_owned();
        // no curve spelling is left for the ray-query lowering to refuse
        assert!(!out.contains("curve_data"), "{out}");
        assert!(out.contains("%3 = tail call ptr @air.allocate_intersection_query()"));
        assert!(out.contains("call { i1, i32, float, float } @nvmtl.curve_next(ptr %3, float %nvcn"));
        assert!(out.contains("define internal { i1, i32, float, float } @nvmtl.curve_next(ptr %q, float %tx)"));
        // a committed curve is a committed box at the candidate's distance, reported as a curve
        assert!(out.contains("call void @air.commit_bounding_box_intersection_intersection_query(ptr %3, float %nvcc"));
        assert!(out.contains("%10 = select i1 %nvct"));
        assert!(out.contains("%11 = load float, ptr %nvcq0.ku, align 4"));
        assert_eq!(out.matches("define internal <4 x float> @nvmtl.curve_intersect(").count(), 1);
        assert!(out.find("@nvmtl.curve_next(ptr %q").unwrap() < out.find("!0 = !{}").unwrap());
    }

    #[test]
    fn a_curve_intersector_with_a_function_table_is_left_for_the_translator_to_refuse() {
        let with_table = PRIMITIVE.replace("ptr addrspace(1) readonly %3, ptr null", "ptr addrspace(1) readonly %1, ptr null");
        assert!(matches!(lower_curve_intersections(&with_table), Cow::Borrowed(_)));
    }

    #[test]
    fn an_instanced_curve_intersector_reads_the_instance_block() {
        let inst = PRIMITIVE
            .replace("{ i32, float, i32, i32, ptr addrspace(1), float }", "{ i32, float, i32, i32, ptr addrspace(1), i32, i32, float }")
            .replace("@air.intersect.curve_data(", "@air.intersect.instancing.curve_data(")
            .replace("ptr addrspace(1) readonly %0, ptr addrspace(1) readonly %3", "ptr addrspace(1) readonly %0, i32 255, ptr addrspace(1) readonly %3")
            .replace("i32 -1, i32 0, i1 false)\n  %5", "i32 -1, i32 0, i1 false, i1 false)\n  %5");
        let out = lower_curve_intersections(&inst).into_owned();
        assert!(out.contains("@nvmtl.curve_trace.instancing.curve_data("), "{out}");
        assert!(out.contains("@air.get_candidate_instance_id_intersection_query.instancing(ptr %q)"));
        assert!(out.contains("i32 %a5, i32 %a9,"));
        assert!(out.contains("%vu = insertvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, float } %vp, float %cu, 7"));
    }
}
