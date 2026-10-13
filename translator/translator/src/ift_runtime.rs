use crate::fxhash::HashSet;

pub const OPAQUE_TRIANGLE_ID: u64 = 0x8000_0000_0000_0001;
pub const TABLE_BUFFERS: u64 = 31;

#[derive(Debug, Clone)]
struct IsectFn {
    id: u64,
    symbol: String,
    bounding_box: bool,
    result_ty: String,
    params: Vec<(String, Role)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Role {
    Origin,
    Direction,
    MinDistance,
    MaxDistance,
    PrimitiveId,
    GeometryId,
    InstanceId,
    UserInstanceId,
    WorldOrigin,
    WorldDirection,
    Barycentric,
    FrontFacing,
    Distance,
    Buffer(u64),
}

fn metadata_nodes(module: &str) -> crate::fxhash::HashMap<String, String> {
    module
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if !t.starts_with('!') {
                return None;
            }
            let (k, v) = t.split_once(" = ")?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn node_refs(body: &str) -> Vec<String> {
    body.split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace())
        .filter(|t| t.starts_with('!') && t[1..].chars().all(|c| c.is_ascii_digit()) && t.len() > 1)
        .map(str::to_string)
        .collect()
}

fn role_of(node: &str) -> Result<(u32, Role), String> {
    let idx: u32 = node
        .split("i32 ")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("intersection function argument node has no index: {node}"))?;
    let quoted: Vec<&str> = node
        .split("!\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap_or(""))
        .collect();
    let role = match quoted.first().copied().unwrap_or("") {
        "air.origin" => Role::Origin,
        "air.direction" => Role::Direction,
        "air.min_distance" => Role::MinDistance,
        "air.max_distance" => Role::MaxDistance,
        "air.primitive_id" => Role::PrimitiveId,
        "air.geometry_id" => Role::GeometryId,
        "air.instance_id" => Role::InstanceId,
        "air.user_instance_id" => Role::UserInstanceId,
        "air.world_space_origin" => Role::WorldOrigin,
        "air.world_space_direction" => Role::WorldDirection,
        "air.barycentric_coord" => Role::Barycentric,
        "air.front_facing" => Role::FrontFacing,
        "air.distance" => Role::Distance,
        "air.buffer" => {
            let loc = node
                .split("!\"air.location_index\", i32 ")
                .nth(1)
                .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| {
                    format!("intersection function buffer argument has no location: {node}")
                })?;
            if loc >= TABLE_BUFFERS {
                return Err(format!("intersection function buffer(%{loc}) is past the table's {TABLE_BUFFERS} buffer slots"));
            }
            Role::Buffer(loc)
        }
        other => {
            return Err(format!(
                "intersection function argument role {other} is not carried yet"
            ))
        }
    };
    Ok((idx, role))
}

fn intersection_functions(
    candidates: &[(String, String)],
    tags: &str,
) -> Result<Vec<IsectFn>, String> {
    let mut out = Vec::new();
    for (k, (symbol, module)) in candidates.iter().enumerate() {
        let nodes = metadata_nodes(module);
        let Some(list) = nodes.get("!air.intersection") else {
            continue;
        };
        let at = format!("@{symbol},");
        let entry = node_refs(list)
            .into_iter()
            .filter_map(|r| nodes.get(&r).cloned())
            .find(|n| n.contains(&at));
        let Some(entry) = entry else { continue };
        let bounding_box = entry.contains("!\"air.bounding_box\"");
        if !bounding_box && !entry.contains("!\"air.triangle\"") {
            return Err(format!("intersection function {symbol}: only bounding_box and triangle functions are carried"));
        }
        for want in tags.split('.') {
            if !entry.contains(&format!("!\"air.{want}\"")) {
                return Err(format!(
                    "intersection function {symbol} does not declare the query's tag {want}"
                ));
            }
        }
        let def = module
            .lines()
            .find(|l| l.trim_start().starts_with("define ") && l.contains(&format!("@{symbol}(")))
            .ok_or_else(|| {
                format!("intersection function {symbol}: no definition in its module")
            })?;
        let head = def.trim_start().trim_start_matches("define ");
        let at_sym = head.find(&format!("@{symbol}(")).unwrap();
        let ret_part = &head[..at_sym];
        let result_ty = ret_part
            .split_whitespace()
            .filter(|w| {
                !matches!(
                    *w,
                    "internal"
                        | "noundef"
                        | "zeroext"
                        | "signext"
                        | "local_unnamed_addr"
                        | "dso_local"
                        | "fastcc"
                        | "hidden"
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let ok_result = if bounding_box {
            result_ty == "<{ i1, float }>" || result_ty == "{ i1, float }"
        } else {
            result_ty == "i1"
        };
        if !ok_result {
            return Err(format!("intersection function {symbol}: result {result_ty} is not carried (bool for triangles, {{bool, float}} for boxes)"));
        }
        let open = head.find('(').unwrap();
        let close = head
            .rfind(')')
            .ok_or("definition without a parameter list")?;
        let param_types: Vec<String> = split_top(&head[open + 1..close])
            .into_iter()
            .map(|p| {
                let toks: Vec<&str> = p.split_whitespace().collect();
                let mut ty = Vec::new();
                for t in toks {
                    if t.starts_with('%')
                        || t.starts_with('"')
                        || matches!(
                            t,
                            "noundef"
                                | "nocapture"
                                | "readonly"
                                | "writeonly"
                                | "nonnull"
                                | "captures(none)"
                        )
                    {
                        break;
                    }
                    ty.push(t);
                }
                ty.join(" ")
            })
            .collect();
        let args_node = node_refs(&entry)
            .get(1)
            .and_then(|r| nodes.get(r))
            .cloned()
            .ok_or_else(|| format!("{symbol}: no argument list node"))?;
        let mut roles = vec![None; param_types.len()];
        for r in node_refs(&args_node) {
            let n = nodes
                .get(&r)
                .ok_or_else(|| format!("{symbol}: argument node {r} missing"))?;
            let (i, role) = role_of(n)?;
            *roles.get_mut(i as usize).ok_or_else(|| {
                format!(
                    "{symbol}: argument {i} beyond its {} parameters",
                    param_types.len()
                )
            })? = Some(role);
        }
        let mut params = Vec::new();
        for (i, (ty, role)) in param_types.into_iter().zip(roles).enumerate() {
            params.push((
                ty,
                role.ok_or_else(|| format!("{symbol}: parameter {i} has no argument role"))?,
            ));
        }
        out.push(IsectFn {
            id: k as u64 + 1,
            symbol: symbol.clone(),
            bounding_box,
            result_ty,
            params,
        });
    }
    Ok(out)
}

fn split_top(s: &str) -> Vec<String> {
    let (mut depth, mut cur, mut out) = (0i32, String::new(), Vec::new());
    for c in s.chars() {
        match c {
            '(' | '{' | '<' | '[' => depth += 1,
            ')' | '}' | '>' | ']' => depth -= 1,
            _ => {}
        }
        if c == ',' && depth == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn arg_type(a: &str) -> Result<String, String> {
    let a = a.trim();
    let bytes: Vec<char> = a.chars().collect();
    let close_of = |open: char, close: char| -> Option<usize> {
        let mut d = 0i32;
        for (i, c) in bytes.iter().enumerate() {
            if *c == open {
                d += 1;
            }
            if *c == close {
                d -= 1;
                if d == 0 {
                    return Some(i);
                }
            }
        }
        None
    };
    let ty = if a.starts_with("<{") {
        close_of('<', '>').map(|e| a[..=e].to_string())
    } else if a.starts_with('<') {
        close_of('<', '>').map(|e| a[..=e].to_string())
    } else if a.starts_with('{') {
        close_of('{', '}').map(|e| a[..=e].to_string())
    } else if a.starts_with("ptr addrspace(") {
        a.find(')').map(|e| a[..=e].to_string())
    } else {
        a.split_whitespace().next().map(str::to_string)
    };
    ty.ok_or_else(|| format!("intersection tables: operand type of {a} did not parse"))
}

fn defining_line<'a>(entry_body: &'a str, value: &str) -> Option<&'a str> {
    let key = format!("{value} = ");
    entry_body
        .lines()
        .find(|l| l.trim_start().starts_with(&key))
}

pub fn rewrite_runtime_intersection_tables(
    ll: &str,
    entry: &str,
    candidates: &[(String, String)],
) -> Result<String, String> {
    let def_at = ll
        .find(&format!("@{entry}("))
        .and_then(|at| ll[..at].rfind("define "))
        .ok_or_else(|| format!("intersection tables: entry {entry} has no definition"))?;
    let body_end = ll[def_at..]
        .find("\n}\n")
        .map(|e| def_at + e + 3)
        .ok_or("intersection tables: entry body is unterminated")?;
    let mut lines: Vec<String> = ll[def_at..body_end].lines().map(str::to_string).collect();
    let mut declares: Vec<String> = Vec::new();
    let mut table_params: HashSet<u32> = HashSet::default();
    let mut n = 0usize;
    let mut i = 1usize;
    while i < lines.len() {
        let line = lines[i].clone();
        let Some(call_at) = line.find("@air.intersect.") else {
            i += 1;
            continue;
        };
        if !line.contains("call ") {
            i += 1;
            continue;
        }
        let name_end = line[call_at..]
            .find('(')
            .map(|e| call_at + e)
            .ok_or("air.intersect call without arguments")?;
        let tags = line[call_at + "@air.intersect.".len()..name_end].to_string();
        if tags.contains("motion") || tags.contains("curve") {
            return Err(format!(
                "intersection tables: air.intersect.{tags} (motion / curve) is not carried yet"
            ));
        }
        let close = line.rfind(')').ok_or("air.intersect call unterminated")?;
        let args = split_top(&line[name_end + 1..close]);
        if args.len() != 20 {
            return Err(format!(
                "intersection tables: air.intersect.{tags} has {} operands, expected 20",
                args.len()
            ));
        }
        let table_value = args[6].split_whitespace().last().unwrap_or("").to_string();
        let body_text = lines.join("\n");
        let is_null = defining_line(&body_text, &table_value)
            .is_some_and(|d| d.contains("@air.get_null_intersection_function_table"));
        if is_null || table_value == "null" {
            i += 1;
            continue;
        }
        if args[7].split_whitespace().last() != Some("null") {
            return Err("intersection tables: a ray payload is not carried yet".to_string());
        }
        let fns = intersection_functions(candidates, &tags)?;
        if let Some(n) = table_value
            .strip_prefix('%')
            .and_then(|v| v.parse::<u32>().ok())
        {
            table_params.insert(n);
        }
        let (res, _) = line
            .split_once(" = ")
            .ok_or("air.intersect result unnamed")?;
        let res = res.trim().to_string();
        let ret_ty = line[line.find("call ").unwrap() + 5..call_at]
            .trim()
            .to_string();
        let mut orig = None;
        for j in (1..i).rev() {
            let t = lines[j].trim();
            if let Some((lbl, _)) = t.split_once(':') {
                if !lbl.is_empty()
                    && !lbl.contains(' ')
                    && !t.starts_with(';')
                    && !lbl.starts_with('%')
                {
                    orig = Some(format!("%{lbl}"));
                    break;
                }
            }
        }
        let orig = match orig {
            Some(l) => l,
            None => {
                let head = &lines[0];
                let open = head.find('(').ok_or("entry define without parameters")?;
                let closep = head.rfind(')').ok_or("entry define unterminated")?;
                format!("%{}", split_top(&head[open + 1..closep]).len())
            }
        };
        let prefix = format!("ift{n}");
        n += 1;
        let inline = emit_inline(&prefix, &res, &ret_ty, &tags, &args, &fns, &mut declares)?;
        let mut succ = Vec::new();
        let mut k = i + 1;
        while k < lines.len() {
            let t = lines[k].trim_start();
            let is_term = t.starts_with("br ")
                || t.starts_with("switch ")
                || t.starts_with("ret ")
                || t.starts_with("ret\n")
                || t == "ret void"
                || t.starts_with("unreachable");
            if is_term {
                let mut term = lines[k].clone();
                if t.starts_with("switch ") {
                    let mut m = k + 1;
                    while m < lines.len() && !lines[m - 1].contains(']') {
                        term.push_str(&lines[m]);
                        m += 1;
                    }
                }
                for part in term.split("label ").skip(1) {
                    if let Some(l) = part.split([',', ' ', ']']).next() {
                        succ.push(l.trim().to_string());
                    }
                }
                break;
            }
            k += 1;
        }
        let cont = format!("%{prefix}.cont");
        for s in &succ {
            let want = format!("{}:", s.trim_start_matches('%'));
            if let Some(at) = lines.iter().position(|l| {
                l.trim_start().starts_with(&want)
                    && (l.trim_start().len() == want.len()
                        || l.trim_start()[want.len()..].starts_with(' '))
            }) {
                let mut m = at + 1;
                while m < lines.len() && lines[m].contains(" = phi ") {
                    lines[m] = lines[m].replace(&format!(", {orig} ]"), &format!(", {cont} ]"));
                    m += 1;
                }
            }
        }
        let mut repl: Vec<String> = vec![format!("  br label %{prefix}.entry")];
        repl.extend(inline.lines().map(str::to_string));
        repl.push(format!("{prefix}.cont:"));
        let rl = repl.len();
        lines.splice(i..=i, repl);
        i += rl;
    }
    if n == 0 {
        return Ok(ll.to_string());
    }
    let mut out = String::with_capacity(ll.len() + 16384);
    out.push_str(&ll[..def_at]);
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out.push_str(&ll[body_end..]);
    out.push('\n');
    let mut have: HashSet<String> = out
        .lines()
        .filter(|l| l.trim_start().starts_with("declare ") || l.trim_start().starts_with("define "))
        .filter_map(|l| {
            l.find('@')
                .map(|a| l[a..].split('(').next().unwrap_or("").to_string())
        })
        .collect();
    for d in declares {
        let g = d
            .find('@')
            .map(|a| d[a..].split('(').next().unwrap_or("").to_string())
            .unwrap_or_default();
        if have.insert(g) {
            out.push_str(&d);
            out.push('\n');
        }
    }
    for (_, module) in candidates {
        append_module_dedupe(&mut out, module);
    }
    let mut result = String::with_capacity(out.len() + 256);
    for line in out.lines() {
        let mut l = line.to_string();
        if l.trim_start().starts_with('!') && l.contains("!\"air.intersection_function_table\"") {
            let idx = l
                .split("!{i32 ")
                .nth(1)
                .and_then(|r| r.split(',').next())
                .and_then(|v| v.trim().parse::<u32>().ok());
            if idx.is_some_and(|i| table_params.contains(&i)) {
                l = l.replacen("!\"air.intersection_function_table\"", "!\"air.buffer\"", 1);
                let at = l
                    .find("!\"air.arg_type_name\", !\"intersection_function_table")
                    .ok_or_else(|| {
                        format!("intersection table metadata has an unexpected shape: {line}")
                    })?;
                let tail_start = at + "!\"air.arg_type_name\", !\"".len();
                let close = l[tail_start..]
                    .find('"')
                    .ok_or("intersection table metadata type name unterminated")?
                    + tail_start
                    + 1;
                l.replace_range(at..close, "!\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 8, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\", !\"ulong\"");
            }
        }
        result.push_str(&l);
        result.push('\n');
    }
    Ok(result)
}

fn append_module_dedupe(output: &mut String, module: &str) {
    let already: HashSet<String> = output
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            if t.starts_with("define ") || t.starts_with("declare ") {
                t.find('@')
                    .map(|at| t[at..].split('(').next().unwrap_or("").to_string())
            } else if t.starts_with('@') {
                t.split(" = ").next().map(|g| g.trim().to_string())
            } else {
                None
            }
        })
        .collect();
    let mut skipping = false;
    for line in module.lines() {
        let t = line.trim_start();
        if skipping {
            if t == "}" {
                skipping = false;
            }
            continue;
        }
        if t.starts_with("define ") || t.starts_with("declare ") {
            let g = t
                .find('@')
                .map(|at| t[at..].split('(').next().unwrap_or("").to_string())
                .unwrap_or_default();
            if already.contains(&g) {
                skipping = t.starts_with("define ") && !t.ends_with('}');
                continue;
            }
        } else if t.starts_with('@') {
            if let Some(g) = t.split(" = ").next() {
                if already.contains(g.trim()) {
                    continue;
                }
            }
        }
        if t.starts_with("; ModuleID =")
            || t.starts_with("source_filename =")
            || t.starts_with("target datalayout =")
            || t.starts_with("target triple =")
            || t.starts_with("attributes #")
            || t.starts_with('!')
        {
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
}

fn emit_inline(
    p: &str,
    res: &str,
    ret_ty: &str,
    tags: &str,
    args: &[String],
    fns: &[IsectFn],
    declares: &mut Vec<String>,
) -> Result<String, String> {
    let q = |f: &str| format!("@air.{f}_intersection_query.{tags}");
    let get = |what: &str, ty: &str, committed: bool, decl: &mut Vec<String>| -> String {
        let g = format!(
            "@air.get_{}_{what}_intersection_query.{tags}",
            if committed { "committed" } else { "candidate" }
        );
        decl.push(format!("declare {ty} {g}(ptr)"));
        format!("call {ty} {g}(ptr %{p}.q)")
    };
    let mut names = Vec::new();
    for a in args.iter() {
        let ty = arg_type(a)?;
        let rest = a.trim()[ty.len()..].trim();
        let value = rest
            .split_whitespace()
            .skip_while(|w| {
                matches!(
                    *w,
                    "readonly" | "noundef" | "nocapture" | "captures(none)" | "nonnull"
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        names.push((ty, value));
    }
    let named = |i: usize| -> String { format!("{} {}", names[i].0, names[i].1) };
    declares.push(format!("declare ptr {}()", q("allocate")));
    declares.push(format!(
        "declare void {}(ptr, <3 x float>, <3 x float>, float, float, ptr addrspace(1), i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i1, i1)",
        q("reset")
    ));
    declares.push(format!("declare i1 {}(ptr)", q("next")));
    declares.push(format!(
        "declare void @air.commit_triangle_intersection_intersection_query.{tags}(ptr)"
    ));
    declares.push(format!(
        "declare void @air.commit_bounding_box_intersection_intersection_query.{tags}(ptr, float)"
    ));
    let mut s = String::new();
    s.push_str(&format!(
        "{p}.entry:\n  %{p}.q = call ptr {}()\n",
        q("allocate")
    ));
    let reset_args: Vec<String> = (0..=5).chain(9..=19).map(named).collect();
    s.push_str(&format!(
        "  call void {}(ptr %{p}.q, {})\n  br label %{p}.loop\n",
        q("reset"),
        reset_args.join(", ")
    ));
    s.push_str(&format!("{p}.loop:\n  %{p}.more = call i1 {}(ptr %{p}.q)\n  br i1 %{p}.more, label %{p}.cand, label %{p}.done\n", q("next")));
    let x = |n: &str| format!("%{p}.{n}");
    s.push_str(&format!("{p}.cand:\n"));
    s.push_str(&format!(
        "  {} = {}\n",
        x("ty"),
        get("intersection_type", "i32", false, declares)
    ));
    s.push_str(&format!(
        "  {} = {}\n",
        x("sbt"),
        get("nvmtl_sbt_offset", "i32", false, declares)
    ));
    s.push_str(&format!(
        "  {} = {}\n",
        x("geo"),
        get("geometry_id", "i32", false, declares)
    ));
    s.push_str(&format!(
        "  {b} = and i32 {sbt}, 8388607\n  {f} = lshr i32 {sbt}, 23\n  {g} = mul i32 {f}, {geo}\n  {sl} = add i32 {b}, {g}\n",
        b = x("base"), sbt = x("sbt"), f = x("addg"), g = x("gadd"), geo = x("geo"), sl = x("slot")
    ));
    s.push_str(&format!(
        "  {} = add i32 {}, {TABLE_BUFFERS}\n  {} = zext i32 {} to i64\n",
        x("slotw"),
        x("slot"),
        x("slot64"),
        x("slotw")
    ));
    s.push_str(&format!(
        "  {} = getelementptr i64, ptr addrspace(1) {}, i64 {}\n  {} = load i64, ptr addrspace(1) {}, align 8\n",
        x("idp"), names[6].1, x("slot64"), x("fid"), x("idp")
    ));
    s.push_str(&format!(
        "  {} = icmp eq i32 {}, 2\n  br i1 {}, label %{p}.box, label %{p}.tri\n",
        x("isbox"),
        x("ty"),
        x("isbox")
    ));
    let boxes: Vec<&IsectFn> = fns.iter().filter(|f| f.bounding_box).collect();
    let tris: Vec<&IsectFn> = fns.iter().filter(|f| !f.bounding_box).collect();
    s.push_str(&format!(
        "{p}.box:\n  switch i64 {}, label %{p}.loop [",
        x("fid")
    ));
    for f in &boxes {
        s.push_str(&format!(" i64 {}, label %{p}.box{}", f.id, f.id));
    }
    s.push_str(" ]\n");
    s.push_str(&format!(
        "{p}.tri:\n  switch i64 {}, label %{p}.tri_commit [ i64 0, label %{p}.tri_commit i64 {OPAQUE_TRIANGLE_ID}, label %{p}.tri_commit",
        x("fid")
    ));
    for f in &tris {
        s.push_str(&format!(" i64 {}, label %{p}.tri{}", f.id, f.id));
    }
    s.push_str(" ]\n");
    s.push_str(&format!(
        "{p}.tri_commit:\n  call void @air.commit_triangle_intersection_intersection_query.{tags}(ptr %{p}.q)\n  br label %{p}.loop\n"
    ));
    for f in boxes.iter().chain(tris.iter()) {
        let lbl = format!("{p}.{}{}", if f.bounding_box { "box" } else { "tri" }, f.id);
        s.push_str(&format!("{lbl}:\n"));
        s.push_str(&format!(
            "  %{lbl}.cty = {}\n",
            get("intersection_type", "i32", true, declares)
        ));
        s.push_str(&format!(
            "  %{lbl}.cd = {}\n",
            get("distance", "float", true, declares)
        ));
        s.push_str(&format!("  %{lbl}.none = icmp eq i32 %{lbl}.cty, 0\n  %{lbl}.max = select i1 %{lbl}.none, float {}, float %{lbl}.cd\n", names[3].1));
        let mut call_args = Vec::new();
        for (k, (ty, role)) in f.params.iter().enumerate() {
            let v = format!("%{lbl}.p{k}");
            let line = match role {
                Role::Origin => format!("  {v} = {}\n", get("ray_origin", ty, false, declares)),
                Role::Direction => format!("  {v} = {}\n", get("ray_direction", ty, false, declares)),
                Role::MinDistance => format!("  {v} = fadd float {}, 0.0\n", names[2].1),
                Role::MaxDistance => format!("  {v} = fadd float %{lbl}.max, 0.0\n"),
                Role::PrimitiveId => format!("  {v} = {}\n", get("primitive_id", ty, false, declares)),
                Role::GeometryId => format!("  {v} = {}\n", get("geometry_id", ty, false, declares)),
                Role::InstanceId => format!("  {v} = {}\n", get("instance_id", ty, false, declares)),
                Role::UserInstanceId => format!("  {v} = {}\n", get("user_instance_id", ty, false, declares)),
                Role::WorldOrigin | Role::WorldDirection => {
                    let g = if *role == Role::WorldOrigin { "world_space_ray_origin" } else { "world_space_ray_direction" };
                    declares.push(format!("declare {ty} @air.get_{g}_intersection_query.{tags}(ptr)"));
                    format!("  {v} = call {ty} @air.get_{g}_intersection_query.{tags}(ptr %{p}.q)\n")
                }
                Role::Barycentric => format!("  {v} = {}\n", get("triangle_barycentric_coord", ty, false, declares)),
                Role::FrontFacing => format!("  {v} = {}\n", get("triangle_front_facing", ty, false, declares)),
                Role::Distance => format!("  {v} = {}\n", get("triangle_distance", ty, false, declares)),
                Role::Buffer(loc) => format!(
                    "  {v}.at = getelementptr ptr addrspace(1), ptr addrspace(1) {}, i64 {loc}\n  {v} = load {ty}, ptr addrspace(1) {v}.at, align 8\n",
                    names[6].1
                ),
            };
            s.push_str(&line);
            call_args.push(format!("{ty} {v}"));
        }
        if f.bounding_box {
            s.push_str(&format!(
                "  %{lbl}.r = call {} @{}({})\n",
                f.result_ty,
                f.symbol,
                call_args.join(", ")
            ));
            s.push_str(&format!("  %{lbl}.acc = extractvalue {} %{lbl}.r, 0\n  %{lbl}.t = extractvalue {} %{lbl}.r, 1\n", f.result_ty, f.result_ty));
            s.push_str(&format!(
                "  %{lbl}.ge = fcmp oge float %{lbl}.t, {}\n  %{lbl}.le = fcmp ole float %{lbl}.t, %{lbl}.max\n  %{lbl}.in = and i1 %{lbl}.ge, %{lbl}.le\n  %{lbl}.ok = and i1 %{lbl}.acc, %{lbl}.in\n",
                names[2].1
            ));
            s.push_str(&format!(
                "  br i1 %{lbl}.ok, label %{lbl}.commit, label %{p}.loop\n{lbl}.commit:\n"
            ));
            s.push_str(&format!(
                "  call void @air.commit_bounding_box_intersection_intersection_query.{tags}(ptr %{p}.q, float %{lbl}.t)\n  br label %{p}.loop\n"
            ));
        } else {
            s.push_str(&format!(
                "  %{lbl}.acc = call i1 @{}({})\n",
                f.symbol,
                call_args.join(", ")
            ));
            s.push_str(&format!(
                "  br i1 %{lbl}.acc, label %{p}.tri_commit, label %{p}.loop\n"
            ));
        }
    }
    s.push_str(&format!("{p}.done:\n"));
    let members = split_top(ret_ty.trim().trim_start_matches('{').trim_end_matches('}'));
    if members.len() != 9 {
        return Err(format!(
            "intersection tables: result {ret_ty} is not the 9-member intersection_result"
        ));
    }
    let comm = [
        ("intersection_type", 0usize),
        ("distance", 1),
        ("primitive_id", 2),
        ("geometry_id", 3),
        ("instance_id", 5),
        ("user_instance_id", 6),
        ("triangle_barycentric_coord", 7),
        ("triangle_front_facing", 8),
    ];
    let mut agg = "undef".to_string();
    for (k, (what, slot)) in comm.iter().enumerate() {
        s.push_str(&format!(
            "  %{p}.c{k} = {}\n",
            get(what, &members[*slot], true, declares)
        ));
        s.push_str(&format!(
            "  %{p}.agg{k} = insertvalue {ret_ty} {agg}, {} %{p}.c{k}, {slot}\n",
            members[*slot]
        ));
        agg = format!("%{p}.agg{k}");
    }
    s.push_str(&format!(
        "  {res} = insertvalue {ret_ty} {agg}, {} null, 4\n  br label %{p}.cont\n",
        members[4]
    ));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERNEL: &str = r#"define void @k(ptr addrspace(1) %0, ptr addrspace(1) %1, ptr addrspace(1) %2) {
  %4 = tail call { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect.instancing.triangle_data(<3 x float> zeroinitializer, <3 x float> <float 0.000000e+00, float 0.000000e+00, float 1.000000e+00>, float 0.000000e+00, float 1.000000e+01, ptr addrspace(1) readonly %0, i32 255, ptr addrspace(1) readonly %2, ptr null, i64 0, i32 0, i32 0, i32 0, i32 0, i32 0, i32 3, i32 -1, i32 -1, i32 0, i1 false, i1 false) #1
  %5 = extractvalue { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } %4, 1
  store float %5, ptr addrspace(1) %1, align 4
  ret void
}
"#;
    const SPHERE: &str = r#"define <{ i1, float }> @sphereIntersection(<3 x float> noundef %0, <3 x float> noundef %1, float noundef %2, float noundef %3, i32 noundef %4, ptr addrspace(1) noundef readonly "air-buffer-no-alias" %5) local_unnamed_addr #3 {
  %7 = insertvalue <{ i1, float }> undef, i1 true, 0
  %8 = insertvalue <{ i1, float }> %7, float %2, 1
  ret <{ i1, float }> %8
}
!air.intersection = !{!35}
!35 = !{ptr @sphereIntersection, !36, !39, !"air.bounding_box", !"air.instancing", !"air.triangle_data"}
!36 = !{!37, !38}
!37 = !{!"air.accept_intersection", !"air.arg_type_name", !"bool", !"air.arg_name", !"accept"}
!38 = !{!"air.distance", !"air.arg_type_name", !"float", !"air.arg_name", !"distance"}
!39 = !{!40, !41, !42, !43, !44, !45}
!40 = !{i32 0, !"air.origin", !"air.arg_type_name", !"float3", !"air.arg_name", !"origin"}
!41 = !{i32 1, !"air.direction", !"air.arg_type_name", !"float3", !"air.arg_name", !"direction"}
!42 = !{i32 2, !"air.min_distance", !"air.arg_type_name", !"float", !"air.arg_name", !"minDistance"}
!43 = !{i32 3, !"air.max_distance", !"air.arg_type_name", !"float", !"air.arg_name", !"maxDistance"}
!44 = !{i32 4, !"air.primitive_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"prim"}
!45 = !{i32 5, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 16, !"air.arg_type_align_size", i32 16, !"air.arg_type_name", !"float4", !"air.arg_name", !"spheres"}
"#;
    const ALPHA: &str = r#"define i1 @alphaTest(i32 noundef %0, <2 x float> noundef %1) local_unnamed_addr #4 {
  %3 = and i32 %0, 1
  %4 = icmp eq i32 %3, 0
  ret i1 %4
}
!air.intersection = !{!46}
!46 = !{ptr @alphaTest, !47, !49, !"air.triangle", !"air.instancing", !"air.triangle_data"}
!47 = !{!48}
!48 = !{!"air.accept_intersection", !"air.arg_type_name", !"bool"}
!49 = !{!50, !51}
!50 = !{i32 0, !"air.primitive_id", !"air.arg_type_name", !"uint", !"air.arg_name", !"prim"}
!51 = !{i32 1, !"air.barycentric_coord", !"air.arg_type_name", !"float2", !"air.arg_name", !"bary", !"air.arg_unused"}
"#;

    #[test]
    fn a_table_bearing_intersect_becomes_a_query_loop_calling_both_linked_functions() {
        let cands = vec![
            ("sphereIntersection".to_string(), SPHERE.to_string()),
            ("alphaTest".to_string(), ALPHA.to_string()),
        ];
        let out = rewrite_runtime_intersection_tables(KERNEL, "k", &cands).unwrap();
        assert!(!out.contains("call { i32, float, i32, i32, ptr addrspace(1), i32, i32, <2 x float>, i1 } @air.intersect."), "{out}");
        assert!(out.contains("br label %ift0.entry"), "{out}");
        assert!(out.contains("ift0.cont:"), "{out}");
        assert!(out.contains("i64 1, label %ift0.box1"), "{out}");
        assert!(out.contains("i64 2, label %ift0.tri2"), "{out}");
        assert!(
            out.contains("call <{ i1, float }> @sphereIntersection("),
            "{out}"
        );
        assert!(out.contains("call i1 @alphaTest("), "{out}");
        assert!(
            out.contains(
                "@air.get_candidate_nvmtl_sbt_offset_intersection_query.instancing.triangle_data"
            ),
            "{out}"
        );
        assert!(
            out.contains("define <{ i1, float }> @sphereIntersection"),
            "the candidate body is appended: {out}"
        );
    }

    #[test]
    fn the_table_argument_becomes_a_u64_buffer_at_its_location() {
        let k = format!("{KERNEL}!0 = !{{i32 2, !\"air.intersection_function_table\", !\"air.location_index\", i32 3, i32 1, !\"air.read_write\", !\"air.arg_type_name\", !\"intersection_function_table<instancing, triangle_data>\", !\"air.arg_name\", !\"table\"}}\n");
        let cands = vec![("sphereIntersection".to_string(), SPHERE.to_string())];
        let out = rewrite_runtime_intersection_tables(&k, "k", &cands).unwrap();
        assert!(out.contains("!0 = !{i32 2, !\"air.buffer\", !\"air.location_index\", i32 3, i32 1, !\"air.read_write\", !\"air.address_space\", i32 1, !\"air.arg_type_size\", i32 8, !\"air.arg_type_align_size\", i32 8, !\"air.arg_type_name\", !\"ulong\", !\"air.arg_name\", !\"table\"}"), "{out}");
    }

    #[test]
    fn a_null_table_intersect_is_left_alone() {
        let k = KERNEL.replace(
            "ptr addrspace(1) readonly %2, ptr null",
            "ptr addrspace(1) readonly null, ptr null",
        );
        let out = rewrite_runtime_intersection_tables(&k, "k", &[]).unwrap();
        assert_eq!(out, k);
    }

    #[test]
    fn a_payload_is_refused_by_name() {
        let k = KERNEL.replace("ptr null, i64 0", "ptr %1, i64 16");
        let err = rewrite_runtime_intersection_tables(&k, "k", &[]).unwrap_err();
        assert!(err.contains("payload"), "{err}");
    }
}
