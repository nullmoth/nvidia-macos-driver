use super::*;

impl LlModule {
    pub(in crate::native) fn parse(ll: &str) -> Result<Self, String> {
        let kern = meta::parse_air_kernel_meta(ll);
        let entry_name = meta::entry_name(ll, "kernel");
        Self::parse_inner(ll, false, kern.as_ref(), entry_name.as_deref())
    }

    pub(in crate::native) fn parse_with_stage_meta(
        ll: &str,
        kern: Option<&meta::KernMeta>,
        entry_name: Option<&str>,
    ) -> Result<Self, String> {
        Self::parse_inner(ll, false, kern, entry_name)
    }

    #[cfg(test)]
    pub(in crate::native) fn parse_with_primitive_phi_metadata(ll: &str) -> Result<Self, String> {
        let kern = meta::parse_air_kernel_meta(ll);
        let entry_name = meta::entry_name(ll, "kernel");
        Self::parse_inner(ll, true, kern.as_ref(), entry_name.as_deref())
    }

    pub(in crate::native) fn parse_with_primitive_phi_metadata_and_stage_meta(
        ll: &str,
        kern: Option<&meta::KernMeta>,
        entry_name: Option<&str>,
    ) -> Result<Self, String> {
        Self::parse_inner(ll, true, kern, entry_name)
    }

    pub(in crate::native) fn parse_inner(
        ll: &str,
        primitive_phi_metadata: bool,
        kern: Option<&meta::KernMeta>,
        entry_name: Option<&str>,
    ) -> Result<Self, String> {
        let mut types = HashMap::default();
        let mut loop_metadata: HashMap<String, String> = HashMap::default();
        for raw_line in ll.lines() {
            let line = strip_comment(raw_line).trim();
            if line.starts_with('!')
                && (line.contains("\"llvm.loop.") || line.contains(" = distinct !{"))
            {
                if let Some((id, node)) = line.split_once(" = ") {
                    loop_metadata.insert(id.trim().to_string(), node.trim().to_string());
                }
                continue;
            }
            if line.starts_with('%') && line.contains(" = type ") {
                let (name, body) = line
                    .split_once(" = type ")
                    .ok_or_else(|| format!("native emitter: malformed type alias: {line}"))?;
                if body.trim() == "opaque" {
                    continue;
                }
                types.insert(name.trim().to_string(), parse_type(body.trim())?);
            }
        }
        let mut functions = Vec::new();
        let mut declarations = Vec::new();
        let mut globals = Vec::new();
        let mut lines = ll.lines();
        while let Some(raw_line) = lines.next() {
            let line = strip_comment(raw_line).trim();
            if line.starts_with('%') && line.contains(" = type ") {
                continue;
            }
            if line.starts_with("define ") {
                let mut func = parse_function_header(line)?;
                let mut body = Vec::new();
                let mut terminated = false;
                for body_line in lines.by_ref() {
                    if strip_comment(body_line).trim() == "}" {
                        terminated = true;
                        break;
                    }
                    body.push(body_line);
                }
                if !terminated {
                    return Err(format!(
                        "native emitter: unterminated function {}",
                        func.name
                    ));
                }
                let entry = crate::native::cfg::implicit_entry_block_name(&func);
                let entry_label = entry.clone();
                let threaded = thread_switch_fanin(&body, &entry_label);
                let body: Vec<&str> = match &threaded {
                    Some(lines) => lines.iter().map(String::as_str).collect(),
                    None => body,
                };
                func.blocks = crate::native::cfg::split_source_body_blocks(&body, entry, &types)?;
                func.loop_controls =
                    source_loop_controls(&body, &entry_label, &func.blocks, &loop_metadata);
                functions.push(func);
                continue;
            }
            if line.starts_with("declare ") {
                let decl = parse_declaration(line)?;
                if !is_ignored_intrinsic(&decl.name) {
                    declarations.push(decl);
                }
                continue;
            }
            if is_ignored_global(line) {
                continue;
            }
            if line.starts_with('@') && (line.contains(" constant ") || line.contains(" global ")) {
                globals.push(parse_global(line)?);
                continue;
            }
        }
        if functions.is_empty() {
            return Err("native emitter: no function definitions found".into());
        }
        let entry_functions = entry_name
            .map(|name| crate::fxhash::set_from([name.to_string()]))
            .unwrap_or_else(|| infer_entry_functions(ll));
        let metadata_byte_buffer_params =
            infer_metadata_byte_buffer_params(kern, entry_name, &functions);
        let metadata_data_buffer_params =
            infer_metadata_data_buffer_params(kern, entry_name, &functions);
        let metadata_primitive_buffer_pointees =
            infer_metadata_primitive_buffer_pointees(kern, entry_name, &functions);
        let metadata_fc_buffer_locations =
            infer_metadata_fc_buffer_locations(kern, entry_name, &functions);
        let imageblock_dimensions = infer_apv_imageblock_dimensions(ll);
        let cross_coordinate_imageblock =
            infer_cross_coordinate_imageblock(&functions, &entry_functions);
        let imageblock_threads_per_threadgroup_param = kern
            .and_then(|meta| {
                meta.roles.iter().find_map(|(index, role)| {
                    matches!(role, KernRole::ThreadsPerThreadgroup).then_some(*index as usize)
                })
            })
            .and_then(|index| {
                functions
                    .iter()
                    .find(|function| entry_functions.contains(&function.name))
                    .and_then(|function| function.params.get(index))
                    .map(|(name, _)| name.clone())
            });
        let aliased_imageblock_planes = kern
            .and_then(|meta| {
                meta.aliased_implicit_imageblock_params
                    .first()
                    .and_then(|param| meta.aliased_implicit_imageblock_planes.get(param))
            })
            .cloned()
            .unwrap_or_default();
        let imageblock_shared_cells = cross_coordinate_imageblock
            || !aliased_imageblock_planes.is_empty()
            || calls_imageblock_slice_write(&functions, &entry_functions);
        let imageblock_statics = meta::static_init_foldable_global_values(ll);
        let imageblock_cell_scale = infer_imageblock_cell_scale(
            &functions,
            &entry_functions,
            &imageblock_statics,
            imageblock_threads_per_threadgroup_param.as_deref(),
        );
        let imageblock_cell_scale_xy = infer_imageblock_lane_scales(
            &functions,
            &entry_functions,
            &imageblock_statics,
            imageblock_threads_per_threadgroup_param.as_deref(),
        );
        if imageblock_shared_cells
            && imageblock_dimensions.is_none()
            && imageblock_threads_per_threadgroup_param.is_none()
            && !declarations
                .iter()
                .any(|declaration| declaration.name == IMAGEBLOCK_WIDTH_INTRINSIC)
        {
            declarations.push(LlDeclaration {
                name: IMAGEBLOCK_WIDTH_INTRINSIC.to_string(),
                ret: LlType::Int(32),
                params: Vec::new(),
            });
        }
        let imageblock_nonzero_byte_field =
            infer_imageblock_nonzero_byte_field(&functions, &entry_functions);
        let imageblock_data_pointee = infer_imageblock_data_pointee(
            kern,
            imageblock_dimensions.is_some()
                || imageblock_shared_cells
                || imageblock_nonzero_byte_field,
        );

        for function in &functions {
            for inst in function.carrier_insts() {
                let Some(call) = inst.call().as_deref() else {
                    continue;
                };
                if let Some(family) = crate::meta::AirIntersectionFamily::parse(&call.callee)? {
                    if family.instancing == crate::meta::AirIntersectionInstancing::MultiLevel {
                        return Err("ray query: multi-level instancing requires hierarchy path metadata; unsupported".into());
                    }
                }
            }
        }
        let mut module = Self {
            air_data_layout: crate::layout::AirDataLayout::from_ir(ll)?,
            types,
            functions,
            declarations,
            globals,
            static_init_globals: meta::static_init_foldable_global_values(ll),
            entry_name: entry_name.map(str::to_string),
            preinlined_static_initializers: HashSet::default(),
            preinlined_helper_pointer_loads: HashSet::default(),
            preinlined_helper_type_capabilities: HashSet::default(),
            entry_functions,
            ptr_pointees: HashMap::default(),
            local_alloca_pointees: HashMap::default(),
            imageblock_data_pointee,
            imageblock_dimensions,
            imageblock_shared_cells,
            aliased_imageblock_planes,
            imageblock_threads_per_threadgroup_param,
            imageblock_cell_scale,
            imageblock_cell_scale_xy,
            metadata_pointee_params: HashSet::default(),
            metadata_pointee_sizes: HashMap::default(),
            metadata_byte_buffer_params,
            metadata_data_buffer_params,
            has_kernel_metadata: kern.is_some(),
            metadata_primitive_buffer_pointees,
            metadata_fc_buffer_locations,
            raw_buffer_params: HashSet::default(),
            call_connected_raw_params: HashSet::default(),
            param_connected_raw_params: HashSet::default(),
        };
        module.infer_metadata_buffer_pointees(kern, entry_name);
        module.infer_pointer_pointees();
        if primitive_phi_metadata {
            module.infer_metadata_primitive_buffer_pointees(kern, entry_name);
        }
        module.infer_local_alloca_pointees();
        module.infer_raw_buffer_params();
        module.infer_call_connected_raw_buffer_params();
        module.propagate_raw_buffer_params();
        module.propagate_data_buffer_params();
        Ok(module)
    }
}

fn thread_switch_fanin(body: &[&str], entry_label: &str) -> Option<Vec<String>> {
    let mut lines: Vec<String> = body.iter().map(|line| line.to_string()).collect();
    let mut threaded = 0usize;
    while threaded < 64 {
        match thread_one_switch_fanin(&lines, entry_label, threaded) {
            Some(next) => {
                lines = next;
                threaded += 1;
            }
            None => break,
        }
    }
    (threaded > 0).then_some(lines)
}

struct SourceBlock {
    label: String,
    start: usize,
    end: usize,
    has_label_line: bool,
}

fn source_blocks(lines: &[String], entry_label: &str) -> Vec<SourceBlock> {
    let mut blocks = Vec::new();
    let mut current = SourceBlock {
        label: entry_label.to_string(),
        start: 0,
        end: 0,
        has_label_line: false,
    };
    for (i, raw) in lines.iter().enumerate() {
        if let Some(label) = strip_comment(raw).trim().strip_suffix(':') {
            current.end = i;
            let next = SourceBlock {
                label: format!("%{label}"),
                start: i,
                end: 0,
                has_label_line: true,
            };
            blocks.push(std::mem::replace(&mut current, next));
        }
    }
    current.end = lines.len();
    blocks.push(current);
    blocks
}

fn source_block_instructions(lines: &[String], block: &SourceBlock) -> Vec<usize> {
    (block.start..block.end)
        .filter(|&i| {
            !strip_comment(&lines[i]).trim().is_empty()
                && !(block.has_label_line && i == block.start)
        })
        .collect()
}

fn label_operands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("label %") {
        let tail = &rest[at + "label ".len()..];
        let end = tail
            .find(|c: char| c == ',' || c == ']' || c.is_whitespace())
            .unwrap_or(tail.len());
        out.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '$' | '-')
}

fn token_positions(line: &str, name: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(offset) = line[from..].find(name) {
        let at = from + offset;
        if !line[at + name.len()..]
            .chars()
            .next()
            .is_some_and(is_ident_char)
        {
            out.push(at);
        }
        from = at + name.len();
    }
    out
}

fn replace_token(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last = 0;
    for at in token_positions(line, from) {
        out.push_str(&line[last..at]);
        out.push_str(to);
        last = at + from.len();
    }
    out.push_str(&line[last..]);
    out
}

fn phi_result_name(line: &str) -> Option<&str> {
    strip_comment(line)
        .trim()
        .split_once(" = phi ")
        .map(|(name, _)| name.trim())
}

fn phi_parts(line: &str) -> Option<(String, Vec<(String, String)>, String)> {
    let line = strip_comment(line).trim_end();
    let at = line.find(" = phi ")? + " = phi ".len();
    let tail = &line[at..];
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (offset, c) in tail.char_indices() {
        match c {
            '[' | '(' | '{' | '<' => {
                if depth == 0 && c == '[' {
                    start = offset;
                }
                depth += 1;
            }
            ']' | ')' | '}' | '>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 && c == ']' {
                    let inner = &tail[start + 1..offset];
                    if crate::native::lex::split_top_level(inner, ',').len() == 2 {
                        spans.push((start, offset + 1));
                    }
                }
            }
            _ => {}
        }
    }
    let first = spans.first()?.0;
    let last = spans.last()?.1;
    let entries = spans
        .iter()
        .map(|(s, e)| {
            let parts = crate::native::lex::split_top_level(&tail[s + 1..e - 1], ',');
            (parts[0].trim().to_string(), parts[1].trim().to_string())
        })
        .collect();
    Some((
        line[..at + first].to_string(),
        entries,
        tail[last..].to_string(),
    ))
}

fn render_phi(head: &str, entries: &[(String, String)], tail: &str) -> String {
    let entries: Vec<String> = entries
        .iter()
        .map(|(v, p)| format!("[ {v}, {p} ]"))
        .collect();
    format!("{head}{}{tail}", entries.join(", "))
}

fn parse_fanin_switch(joined: &str) -> Option<(Vec<i128>, String, String, String)> {
    let rest = joined.trim().strip_prefix("switch ")?;
    let open = rest.find('[')?;
    let close = rest.rfind(']')?;
    let (selector, default) = rest[..open].split_once(',')?;
    let (ty, selector) = selector.trim().split_once(' ')?;
    let bits = ty.strip_prefix('i')?;
    if bits.is_empty() || !bits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let default = default.trim().strip_prefix("label ")?.trim();
    let tokens: Vec<&str> = rest[open + 1..close]
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.len() < 8 || !tokens.len().is_multiple_of(4) {
        return None;
    }
    let target = tokens[3];
    let mut values = Vec::new();
    for case in tokens.chunks(4) {
        if case[0] != ty || case[2] != "label" || case[3] != target {
            return None;
        }
        values.push(case[1].parse::<i128>().ok()?);
    }
    (target != default).then(|| {
        (
            values,
            target.to_string(),
            default.to_string(),
            selector.trim().to_string(),
        )
    })
}

fn thread_one_switch_fanin(lines: &[String], entry_label: &str, k: usize) -> Option<Vec<String>> {
    let text: Vec<&str> = lines.iter().map(String::as_str).collect();
    let blocks = source_blocks(lines, entry_label);
    let index: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| (block.label.as_str(), i))
        .collect();
    let insts: Vec<Vec<usize>> = blocks
        .iter()
        .map(|block| source_block_instructions(lines, block))
        .collect();
    let succs: Vec<Vec<String>> = insts
        .iter()
        .map(|ins| {
            ins.iter()
                .flat_map(|&i| label_operands(strip_comment(&lines[i])))
                .collect()
        })
        .collect();
    'blocks: for (si, s) in blocks.iter().enumerate() {
        if !s.has_label_line {
            continue;
        }
        let ins = &insts[si];
        let Some(first_other) = ins
            .iter()
            .position(|&i| phi_result_name(&lines[i]).is_none())
        else {
            continue;
        };
        let sw_at = ins[first_other];
        if !strip_comment(&lines[sw_at]).trim().starts_with("switch ") {
            continue;
        }
        let Ok((joined, next)) = crate::native::parse::collect_switch(&text, sw_at) else {
            continue;
        };
        if ins.iter().any(|&i| i >= next) {
            continue;
        }
        let Some((cases, target, default, selector)) = parse_fanin_switch(&joined) else {
            continue;
        };
        let (Some(&ti), Some(&di)) = (index.get(target.as_str()), index.get(default.as_str()))
        else {
            continue;
        };
        if ti == si || di == si {
            continue;
        }
        let phis = &ins[..first_other];
        let Some(&sel_at) = phis
            .iter()
            .find(|&&i| phi_result_name(&lines[i]) == Some(selector.as_str()))
        else {
            continue;
        };
        let Some((_, sel_entries, _)) = phi_parts(&lines[sel_at]) else {
            continue;
        };
        let (mut to_target, mut to_default): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        for (value, pred) in &sel_entries {
            let Ok(value) = value.parse::<i128>() else {
                continue 'blocks;
            };
            if *pred == s.label || to_target.contains(pred) || to_default.contains(pred) {
                continue 'blocks;
            }
            if cases.contains(&value) {
                to_target.push(pred.clone());
            } else {
                to_default.push(pred.clone());
            }
        }
        if to_target.is_empty() || to_default.is_empty() {
            continue;
        }
        if (0..lines.len())
            .filter(|&i| i < s.start || i >= s.end)
            .any(|i| !token_positions(strip_comment(&lines[i]), &selector).is_empty())
        {
            continue;
        }
        let sd = format!("%metal2vulkan.swthread.{k}");
        let mut others = Vec::new();
        for &i in phis.iter().filter(|&&i| i != sel_at) {
            let (Some(name), Some(parts)) = (phi_result_name(&lines[i]), phi_parts(&lines[i]))
            else {
                continue 'blocks;
            };
            let fresh = format!("%metal2vulkan.swthread.{k}.{}", others.len());
            others.push((i, name.to_string(), fresh, parts));
        }
        let mut reach = vec![false; blocks.len()];
        let mut work = vec![di];
        while let Some(b) = work.pop() {
            if b == si || reach[b] {
                continue;
            }
            reach[b] = true;
            work.extend(
                succs[b]
                    .iter()
                    .filter_map(|label| index.get(label.as_str()).copied()),
            );
        }
        let mut out: Vec<String> = lines.to_vec();
        let names_any = |line: &str| {
            others
                .iter()
                .any(|o| !token_positions(line, &o.1).is_empty())
        };
        for b in (0..blocks.len()).filter(|&b| reach[b]) {
            for &i in &insts[b] {
                let line = strip_comment(&lines[i]);
                if b == di && phi_result_name(line).is_some() {
                    let Some((head, mut entries, tail)) = phi_parts(line) else {
                        continue 'blocks;
                    };
                    if !entries.iter().any(|(_, p)| *p == s.label) {
                        if names_any(line) {
                            continue 'blocks;
                        }
                        continue;
                    }
                    for (value, pred) in entries.iter_mut() {
                        if *pred == s.label {
                            *pred = sd.clone();
                            if let Some(o) = others.iter().find(|o| o.1 == *value) {
                                *value = o.2.clone();
                            }
                        }
                    }
                    let rendered = render_phi(&head, &entries, &tail);
                    if names_any(&rendered) {
                        continue 'blocks;
                    }
                    out[i] = rendered;
                } else if names_any(line) {
                    continue 'blocks;
                }
            }
        }
        for &i in &insts[ti] {
            if phi_result_name(&out[i]).is_none() {
                continue;
            }
            let Some((head, entries, tail)) = phi_parts(&out[i]) else {
                continue 'blocks;
            };
            let mut seen = false;
            let kept: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, pred)| {
                    if *pred != s.label {
                        return true;
                    }
                    let first = !seen;
                    seen = true;
                    first
                })
                .cloned()
                .collect();
            if kept.len() != entries.len() {
                out[i] = render_phi(&head, &kept, &tail);
            }
        }
        let to_s = format!("label {}", s.label);
        let to_sd = format!("label {sd}");
        for pred in &to_default {
            let Some(&pi) = index.get(pred.as_str()) else {
                continue 'blocks;
            };
            let hits: Vec<usize> = insts[pi]
                .iter()
                .copied()
                .filter(|&i| !token_positions(strip_comment(&out[i]), &to_s).is_empty())
                .collect();
            if hits.len() != 1 || token_positions(strip_comment(&out[hits[0]]), &to_s).len() != 1 {
                continue 'blocks;
            }
            out[hits[0]] = replace_token(strip_comment(&out[hits[0]]), &to_s, &to_sd);
        }
        let mut replacement: Vec<String> = Vec::new();
        for (i, line) in out.iter().enumerate().take(s.end).skip(s.start) {
            if i == sel_at || (sw_at..next).contains(&i) {
                if i == sw_at {
                    replacement.push(format!("  br label {target}"));
                }
                continue;
            }
            if let Some(o) = others.iter().find(|o| o.0 == i) {
                let (head, entries, tail) = &o.3;
                let kept: Vec<(String, String)> = entries
                    .iter()
                    .filter(|(_, p)| to_target.contains(p))
                    .cloned()
                    .collect();
                replacement.push(render_phi(head, &kept, tail));
                continue;
            }
            replacement.push(line.clone());
        }
        replacement.push(format!("{}:", &sd[1..]));
        for (_, name, fresh, (head, entries, tail)) in &others {
            let kept: Vec<(String, String)> = entries
                .iter()
                .filter(|(_, p)| to_default.contains(p))
                .cloned()
                .collect();
            replacement.push(render_phi(&replace_token(head, name, fresh), &kept, tail));
        }
        replacement.push(format!("  br label {default}"));
        out.splice(s.start..s.end, replacement);
        return Some(out);
    }
    None
}

fn source_loop_controls(
    body: &[&str],
    entry_label: &str,
    blocks: &[crate::native::cfg::BodyBlock],
    loop_metadata: &HashMap<String, String>,
) -> HashMap<String, (spirv::LoopControl, Option<u32>)> {
    let mut controls = HashMap::default();
    if loop_metadata.is_empty() {
        return controls;
    }
    let mut latch_hints: HashMap<String, (spirv::LoopControl, Option<u32>)> = HashMap::default();
    let mut current = entry_label.to_string();
    for raw in body {
        let line = strip_comment(raw).trim();
        if let Some(label) = line.strip_suffix(':') {
            current = format!("%{label}");
            continue;
        }
        let Some((_, tail)) = line.split_once("!llvm.loop ") else {
            continue;
        };
        let id = tail
            .split(|c: char| c == ',' || c.is_whitespace())
            .next()
            .unwrap_or_default();
        if let Some(hint) = unroll_hint(id, loop_metadata) {
            latch_hints.insert(current.clone(), hint);
        }
    }
    if latch_hints.is_empty() {
        return controls;
    }
    let forest = crate::native::cfg::loopforest::analyze(blocks);
    for (latch, hint) in latch_hints {
        let mut closed = forest
            .loops
            .iter()
            .filter(|natural_loop| natural_loop.latches.contains(&latch));
        if let (Some(natural_loop), None) = (closed.next(), closed.next()) {
            let entry = controls.entry(natural_loop.header.clone()).or_insert(hint);
            if hint.0 == spirv::LoopControl::DONT_UNROLL {
                *entry = hint;
            }
        }
    }
    controls
}

fn unroll_hint(
    id: &str,
    loop_metadata: &HashMap<String, String>,
) -> Option<(spirv::LoopControl, Option<u32>)> {
    let node = loop_metadata.get(id)?;
    let inner = node.split_once('{')?.1.rsplit_once('}')?.0;
    let (mut disable, mut unroll, mut count) = (false, false, None);
    for child in inner
        .split(',')
        .map(str::trim)
        .filter(|c| c.starts_with('!') && *c != id)
    {
        let Some(property) = loop_metadata.get(child) else {
            continue;
        };
        if property.contains("\"llvm.loop.unroll.disable\"") {
            disable = true;
        } else if property.contains("\"llvm.loop.unroll.full\"")
            || property.contains("\"llvm.loop.unroll.enable\"")
        {
            unroll = true;
        } else if property.contains("\"llvm.loop.unroll.count\"") {
            count = property
                .rsplit_once("i32 ")
                .and_then(|(_, n)| n.trim_end_matches('}').trim().parse::<u32>().ok());
        }
    }
    match (disable, count, unroll) {
        (true, _, _) | (_, Some(1), _) => Some((spirv::LoopControl::DONT_UNROLL, None)),
        (_, Some(n), _) if n > 1 => Some((spirv::LoopControl::PARTIAL_COUNT, Some(n))),
        (_, _, true) => Some((spirv::LoopControl::UNROLL, None)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threaded_stage_meta_preserves_kernel_inference() {
        let ll = r#"
define void @k(ptr addrspace(1) %input) {
entry:
  %value = load float, ptr addrspace(1) %input, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 2, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float", !"air.arg_name", !"input"}
"#;
        let reparsed = LlModule::parse(ll).expect("ordinary parse");
        let kern = meta::parse_air_kernel_meta(ll);
        let threaded =
            LlModule::parse_with_stage_meta(ll, kern.as_ref(), Some("k")).expect("threaded parse");

        assert_eq!(threaded.entry_functions, reparsed.entry_functions);
        assert_eq!(threaded.ptr_pointees, reparsed.ptr_pointees);
        assert_eq!(
            threaded.metadata_pointee_params,
            reparsed.metadata_pointee_params
        );
        assert_eq!(
            threaded.metadata_pointee_sizes,
            reparsed.metadata_pointee_sizes
        );
        assert_eq!(
            threaded.metadata_byte_buffer_params,
            reparsed.metadata_byte_buffer_params
        );
        assert_eq!(
            threaded.metadata_data_buffer_params,
            reparsed.metadata_data_buffer_params
        );
        assert_eq!(
            threaded.metadata_primitive_buffer_pointees,
            reparsed.metadata_primitive_buffer_pointees
        );
        assert_eq!(
            threaded.call_connected_raw_params,
            reparsed.call_connected_raw_params
        );
        assert_eq!(
            threaded.param_connected_raw_params,
            reparsed.param_connected_raw_params
        );
        assert_eq!(
            threaded.imageblock_data_pointee,
            reparsed.imageblock_data_pointee
        );
        assert_eq!(
            threaded.imageblock_threads_per_threadgroup_param,
            reparsed.imageblock_threads_per_threadgroup_param
        );
    }
}
