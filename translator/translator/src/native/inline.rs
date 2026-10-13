use crate::fxhash::{HashMap, HashSet};
use super::lex::split_top_level;

pub(super) fn inline_direct_function_pointer_consumers(
    san_ll: &str,
    direct_functions: &HashSet<String>,
) -> String {
    let mut source = san_ll.to_string();
    loop {
        let Some(items) = parse_items(&source) else {
            return source;
        };
        let internal = items
            .iter()
            .filter_map(|item| {
                let Item::Func(function) = item else {
                    return None;
                };
                let signature = parse_def_header(&function.header)?;
                signature.internal.then_some(signature.name)
            })
            .collect::<HashSet<_>>();
        let selected = items.iter().find_map(|item| {
            let Item::Func(function) = item else {
                return None;
            };
            function.body.iter().find_map(|line| {
                let call = parse_call(line)?;
                if !internal.contains(&call.callee) {
                    return None;
                }
                call.args
                    .iter()
                    .find(|argument| direct_functions.contains(*argument))
                    .map(|argument| (call.callee.clone(), argument.clone()))
            })
        });
        let Some((consumer, function)) = selected else {
            return source;
        };
        let targets = crate::fxhash::set_from([consumer]);
        let Some(inlined) = try_inline(&source, Some((&targets, &function, None))) else {
            return source;
        };
        if inlined == source {
            return source;
        }
        source = inlined;
    }
}

#[cfg(test)]
pub(super) fn inline_nonrecursive_internal_calls(san_ll: &str) -> String {
    match try_inline(san_ll, None) {
        Some(out) => out,
        None => san_ll.to_string(),
    }
}

pub(super) struct PointerConsumerInlining {
    pub(super) source: String,
    pub(super) requires_relooper: bool,
}

pub(super) fn inline_pointer_select_consumers(
    san_ll: &str,
    entry_name: Option<&str>,
) -> PointerConsumerInlining {
    let Some(items) = parse_items(san_ll) else {
        return PointerConsumerInlining {
            source: san_ll.to_string(),
            requires_relooper: false,
        };
    };
    let eligible_callees = items
        .iter()
        .filter_map(|item| {
            let Item::Func(function) = item else {
                return None;
            };
            let signature = parse_def_header(&function.header)?;
            signature.internal.then_some(signature.name)
        })
        .collect::<HashSet<_>>();
    let mut selected_consumers = Vec::new();
    for item in &items {
        let Item::Func(function) = item else {
            continue;
        };
        if let Some(entry_name) = entry_name {
            let Some(signature) = parse_def_header(&function.header) else {
                continue;
            };
            if signature.name.trim_start_matches('@') != entry_name {
                continue;
            }
        }
        let pointer_selects = function
            .body
            .iter()
            .filter_map(|line| {
                let (true_value, false_value) =
                    crate::native::tir::resolve_select_arms(line, "select")?;
                (matches!(true_value.ty, crate::native::ir::LlType::Ptr(_))
                    && matches!(false_value.ty, crate::native::ir::LlType::Ptr(_)))
                .then(|| crate::native::tir::result_name(line))
                .flatten()
            })
            .collect::<HashSet<_>>();
        for call in function
            .body
            .iter()
            .filter_map(|line| parse_call(line))
            .filter(|call| eligible_callees.contains(&call.callee))
        {
            for argument in call.args {
                if pointer_selects.contains(&argument)
                    && !selected_consumers.contains(&(argument.clone(), call.callee.clone()))
                {
                    selected_consumers.push((argument, call.callee.clone()));
                }
            }
        }
    }
    let mut source = san_ll.to_string();
    let mut changed = false;
    for (selected, consumer) in selected_consumers {
        let targets = crate::fxhash::set_from([consumer]);
        if let Some(inlined) = try_inline(&source, Some((&targets, selected.as_str(), entry_name)))
        {
            changed |= inlined != source;
            source = inlined;
        }
    }
    PointerConsumerInlining {
        source,
        requires_relooper: changed,
    }
}

pub(super) fn inline_cursor_call_sites(
    san_ll: &str,
    sites: &HashSet<(String, String)>,
) -> Option<String> {
    let mut ordered = sites.iter().collect::<Vec<_>>();
    ordered.sort();
    let mut source = san_ll.to_string();
    let mut changed = false;
    for (callee, argument) in ordered {
        let targets = crate::fxhash::set_from([format!("@{}", callee.trim_start_matches('@'))]);
        if let Some(inlined) = try_inline(&source, Some((&targets, argument.as_str(), None))) {
            changed |= inlined != source;
            source = inlined;
        }
    }
    changed.then_some(source)
}

#[derive(Clone)]
struct FuncBlock {
    header: String,
    body: Vec<String>,
}

enum Item {
    Func(FuncBlock),
    Raw(Vec<String>),
}

struct DefSig {
    name: String,
    internal: bool,
    ret_ty: String,
    params: Vec<String>,
    static_initializer: bool,
}

fn parse_items(san_ll: &str) -> Option<Vec<Item>> {
    let lines: Vec<&str> = san_ll.lines().collect();
    let mut items: Vec<Item> = Vec::new();
    let mut raw_start = 0;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("define") && line.trim_end().ends_with('{') {
            if raw_start != i {
                items.push(Item::Raw(
                    lines[raw_start..i]
                        .iter()
                        .map(|line| (*line).to_string())
                        .collect(),
                ));
            }
            let start = i + 1;
            let mut j = start;
            while j < lines.len() && lines[j].trim() != "}" {
                j += 1;
            }
            if j >= lines.len() {
                return None;
            }
            items.push(Item::Func(FuncBlock {
                header: line.to_string(),
                body: lines[start..j].iter().map(|s| s.to_string()).collect(),
            }));
            i = j + 1;
            raw_start = i;
        } else {
            i += 1;
        }
    }
    if raw_start != lines.len() {
        items.push(Item::Raw(
            lines[raw_start..]
                .iter()
                .map(|line| (*line).to_string())
                .collect(),
        ));
    }
    Some(items)
}

fn parse_def_header(header: &str) -> Option<DefSig> {
    let h = header.trim();
    let rest = h.strip_prefix("define")?.trim_start();
    let at = rest.find('@')?;
    let paren = rest[at..].find('(')? + at;
    let name = rest[at..paren].trim().to_string();
    if name.len() < 2 {
        return None;
    }
    let pre = rest[..at].trim();
    let pre_words: Vec<&str> = pre.split_whitespace().collect();
    let internal = pre_words.contains(&"internal");
    let ret_ty = ret_type_from_pre(pre).unwrap_or_default();
    let close = matching_paren(rest, paren)?;
    let params_str = &rest[paren + 1..close];
    let params = parse_param_names(params_str);
    let static_initializer =
        crate::air_static_init::tail_declares_static_init_section(&rest[close + 1..]);
    Some(DefSig {
        name,
        internal,
        ret_ty,
        params,
        static_initializer,
    })
}

fn ret_type_from_pre(pre: &str) -> Option<String> {
    const KW: &[&str] = &[
        "internal",
        "fastcc",
        "coldcc",
        "cc",
        "weak",
        "weak_odr",
        "linkonce",
        "linkonce_odr",
        "private",
        "external",
        "available_externally",
        "dso_local",
        "dso_preemptable",
        "hidden",
        "protected",
        "default",
        "signext",
        "zeroext",
        "noundef",
    ];
    let toks = super::lex::split_top_level_whitespace(pre);
    let mut idx = 0;
    while idx < toks.len() && KW.contains(&toks[idx]) {
        idx += 1;
    }
    if idx >= toks.len() {
        return None;
    }
    Some(toks[idx..].join(" "))
}

fn parse_param_names(params_str: &str) -> Vec<String> {
    let s = params_str.trim();
    if s.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for chunk in split_top_level(s, ',') {
        let chunk = chunk.trim();
        if chunk == "..." {
            out.push(String::new());
            continue;
        }
        match last_local_token(chunk) {
            Some(name) => out.push(name.to_string()),
            None => out.push(String::new()),
        }
    }
    out
}

fn last_local_token(chunk: &str) -> Option<&str> {
    let tok = super::lex::split_top_level_whitespace(chunk.trim())
        .last()
        .copied()?;
    tok.starts_with('%').then_some(tok)
}

fn matching_paren(s: &str, open: usize) -> Option<usize> {
    super::lex::matching_paren(s, open)
}

struct CallSite {
    result: Option<String>,
    callee: String,
    ret_ty: String,
    args: Vec<String>,
}

fn parse_call(line: &str) -> Option<CallSite> {
    if !contains_bytes(line.as_bytes(), b"call ") {
        return None;
    }
    let trimmed = line.trim();
    let (result, rhs) = match trimmed.find(" = ") {
        Some(eq) if trimmed[..eq].trim().starts_with('%') => (
            Some(trimmed[..eq].trim().to_string()),
            trimmed[eq + 3..].trim(),
        ),
        _ => (None, trimmed),
    };
    let rhs = strip_leading_word(rhs, &["tail", "musttail", "notail"]);
    let rhs = rhs.trim_start();
    let after_call = rhs.strip_prefix("call ")?;
    let at = after_call.find('@')?;
    if after_call[..at].contains('(') {
        return None;
    }
    let paren = after_call[at..].find('(')? + at;
    let callee = after_call[at..paren].trim().to_string();
    if callee.len() < 2 || callee.contains(char::is_whitespace) {
        return None;
    }
    let close = matching_paren(after_call, paren)?;
    let args_str = &after_call[paren + 1..close];
    let ret_ty = ret_type_from_call_prefix(&after_call[..at]);
    let args = parse_call_args(args_str)?;
    Some(CallSite {
        result,
        callee,
        ret_ty,
        args,
    })
}

fn ret_type_from_call_prefix(prefix: &str) -> String {
    const SKIP: &[&str] = &[
        "fastcc",
        "coldcc",
        "cc",
        "tailcc",
        "swiftcc",
        "swifttailcc",
        "cfguard_checkcc",
        "fast",
        "nnan",
        "ninf",
        "nsz",
        "arcp",
        "contract",
        "afn",
        "reassoc",
        "signext",
        "zeroext",
        "noundef",
        "nonnull",
        "noalias",
        "inreg",
        "returned",
    ];
    let toks = super::lex::split_top_level_whitespace(prefix.trim());
    let mut idx = 0;
    while idx < toks.len() {
        if SKIP.contains(&toks[idx]) {
            idx += 1;
        } else if (toks[idx] == "align" || toks[idx] == "dereferenceable")
            && idx + 1 < toks.len()
            && toks[idx + 1].chars().all(|c| c.is_ascii_digit())
        {
            idx += 2;
        } else {
            break;
        }
    }
    toks[idx..].join(" ")
}

fn parse_call_args(args_str: &str) -> Option<Vec<String>> {
    let s = args_str.trim();
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for chunk in split_top_level(s, ',') {
        let chunk = chunk.trim();
        if chunk == "..." {
            return None;
        }
        out.push(arg_value_token(chunk)?);
    }
    Some(out)
}

fn arg_value_token(chunk: &str) -> Option<String> {
    let toks = super::lex::split_top_level_whitespace(chunk);
    let last = *toks.last()?;
    if last.starts_with('%') {
        return Some(last.to_string());
    }
    Some(last.to_string())
}

fn strip_leading_word<'a>(s: &'a str, words: &[&str]) -> &'a str {
    let t = s.trim_start();
    for w in words {
        if let Some(rest) = t.strip_prefix(w) {
            if rest.starts_with(char::is_whitespace) {
                return rest;
            }
        }
    }
    s
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$' || c == '-'
}

fn rename_line(
    line: &str,
    value_map: &HashMap<String, String>,
    label_map: &HashMap<String, String>,
) -> String {
    if let Some(rewritten) = rewrite_block_label_line(line, label_map) {
        return rewritten;
    }

    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '%' {
            let start = i;
            i += 1;
            let name_start = i;
            while i < bytes.len() && is_name_char(bytes[i] as char) {
                i += 1;
            }
            let name = &line[start..i];
            let bare = &line[name_start..i];
            if preceded_by_label_keyword(&out) {
                if let Some(rep) = label_map.get(bare) {
                    out.push('%');
                    out.push_str(rep);
                } else if let Some(rep) = value_map.get(name) {
                    out.push_str(rep);
                } else {
                    out.push_str(name);
                }
            } else if let Some(rep) = value_map.get(name) {
                out.push_str(rep);
            } else {
                out.push_str(name);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }

    let out = rename_bare_label_refs(&out, label_map);
    rename_phi_pred_labels(&out, label_map)
}

fn rename_phi_pred_labels(line: &str, label_map: &HashMap<String, String>) -> String {
    if label_map.is_empty() || !is_phi_line(line) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        let Some(open) = rest.find('[') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..=open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            out.push_str(after);
            break;
        };
        let inner = &after[..close];
        let parts = split_top_level(inner, ',');
        if parts.len() == 2 {
            let val = parts[0].trim();
            let lbl = parts[1].trim();
            let lbl_bare = lbl.strip_prefix('%').unwrap_or(lbl);
            match label_map.get(lbl_bare) {
                Some(rep) => out.push_str(&format!(" {val}, %{rep} ")),
                None => out.push_str(inner),
            }
        } else {
            out.push_str(inner);
        }
        out.push(']');
        rest = &after[close + 1..];
    }
    out
}

fn rewrite_block_label_line(line: &str, label_map: &HashMap<String, String>) -> Option<String> {
    let ws_len = line.len() - line.trim_start().len();
    let (ws, rest) = line.split_at(ws_len);
    let colon = rest.find(':')?;
    let ident = &rest[..colon];
    if ident.is_empty() {
        return None;
    }
    let bare = ident.strip_prefix('%').unwrap_or(ident);
    if !bare.chars().all(is_name_char) {
        return None;
    }
    let after = &rest[colon + 1..];
    let after_trim = after.trim_start();
    if !after_trim.is_empty() && !after_trim.starts_with(';') {
        return None;
    }
    let new_bare = label_map
        .get(bare)
        .cloned()
        .unwrap_or_else(|| bare.to_string());
    let new_after = rewrite_preds_comment(after, label_map);
    Some(format!("{ws}{new_bare}:{new_after}"))
}

fn rewrite_preds_comment(after: &str, label_map: &HashMap<String, String>) -> String {
    let bytes = after.as_bytes();
    let mut out = String::with_capacity(after.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '%' {
            let name_start = i + 1;
            let mut j = name_start;
            while j < bytes.len() && is_name_char(bytes[j] as char) {
                j += 1;
            }
            let bare = &after[name_start..j];
            if let Some(rep) = label_map.get(bare) {
                out.push('%');
                out.push_str(rep);
            } else {
                out.push_str(&after[i..j]);
            }
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn rename_bare_label_refs(line: &str, label_map: &HashMap<String, String>) -> String {
    if label_map.is_empty() || !line.contains("label ") {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if line[i..].starts_with("label")
            && (i == 0 || !is_name_char(bytes[i - 1] as char))
            && line[i + 5..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace())
        {
            out.push_str("label");
            let mut j = i + 5;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                out.push(bytes[j] as char);
                j += 1;
            }
            if j < bytes.len() && bytes[j] as char != '%' {
                let s = j;
                while j < bytes.len() && is_name_char(bytes[j] as char) {
                    j += 1;
                }
                let bare = &line[s..j];
                if let Some(rep) = label_map.get(bare) {
                    out.push_str(rep);
                } else {
                    out.push_str(bare);
                }
            }
            i = j;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn preceded_by_label_keyword(out: &str) -> bool {
    let trimmed = out.trim_end();
    if trimmed.len() == out.len() {
        return false;
    }
    match trimmed.rsplit(|c: char| c.is_whitespace()).next() {
        Some(w) => w == "label",
        None => false,
    }
}

fn try_inline(
    san_ll: &str,
    targets: Option<(&HashSet<String>, &str, Option<&str>)>,
) -> Option<String> {
    let items = parse_items(san_ll)?;

    let mut internal: HashMap<String, (DefSig, Vec<String>)> = HashMap::default();
    let mut has_any_func = false;
    for it in &items {
        if let Item::Func(f) = it {
            has_any_func = true;
            let sig = parse_def_header(&f.header)?;
            if sig.internal
                && targets.is_none_or(|(targets, _selected, _entry)| targets.contains(&sig.name))
            {
                if sig.params.iter().any(|p| p.is_empty()) {
                    continue;
                }
                internal.insert(sig.name.clone(), (sig, f.body.clone()));
            }
        }
    }
    if !has_any_func || internal.is_empty() {
        return None;
    }

    if internal_callgraph_has_cycle(&internal) {
        return None;
    }

    let mut any_call = false;
    for it in &items {
        if let Item::Func(f) = it {
            for line in &f.body {
                if let Some(c) = parse_call(line) {
                    if internal.contains_key(&c.callee) {
                        any_call = true;
                        break;
                    }
                }
            }
        }
        if any_call {
            break;
        }
    }
    if !any_call {
        return None;
    }

    let mut counter = next_inline_ordinal(san_ll);
    let mut out_items: Vec<Item> = Vec::with_capacity(items.len());
    for it in items {
        match it {
            Item::Raw(r) => out_items.push(Item::Raw(r)),
            Item::Func(f) => {
                let selected_pointer = targets.and_then(|(_, selected, entry_name)| {
                    let signature = parse_def_header(&f.header)?;
                    entry_name
                        .is_none_or(|entry_name| {
                            signature.name.trim_start_matches('@') == entry_name
                        })
                        .then_some(selected)
                });
                let new_body = if targets.is_some() && selected_pointer.is_none() {
                    f.body
                } else {
                    inline_body_to_fixpoint(f.body, &internal, &mut counter, selected_pointer)?
                };
                out_items.push(Item::Func(FuncBlock {
                    header: f.header,
                    body: new_body,
                }));
            }
        }
    }

    let out_items = drop_dead_internal_functions(out_items);
    Some(render(&out_items, san_ll.ends_with('\n')))
}

fn next_inline_ordinal(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut next = 0usize;
    let mut cursor = 0usize;
    while let Some(relative) = find_dot_inl(&bytes[cursor..]) {
        let start = cursor + relative + 4;
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end > start && bytes.get(end) == Some(&b'.') {
            if let Ok(ordinal) = source[start..end].parse::<usize>() {
                next = next.max(ordinal.saturating_add(1));
            }
        }
        cursor = start;
        if cursor >= source.len() {
            break;
        }
    }
    next
}

fn drop_dead_internal_functions(items: Vec<Item>) -> Vec<Item> {
    let internal_names: Vec<String> = items
        .iter()
        .filter_map(|it| match it {
            Item::Func(f) => parse_def_header(&f.header)
                .filter(|s| s.internal)
                .map(|s| s.name),
            _ => None,
        })
        .collect();
    if internal_names.is_empty() {
        return items;
    }
    let item_runs: Vec<HashSet<&str>> = items
        .iter()
        .map(|it| match it {
            Item::Raw(r) => symbol_runs(r),
            Item::Func(f) => symbol_runs(&f.body),
        })
        .collect();
    let mut live: crate::fxhash::HashSet<String> = crate::fxhash::HashSet::default();
    loop {
        let mut changed = false;
        for (item_index, it) in items.iter().enumerate() {
            let (lines, self_name): (&[String], Option<String>) = match it {
                Item::Raw(r) => (r.as_slice(), None),
                Item::Func(f) => {
                    let sig = match parse_def_header(&f.header) {
                        Some(s) => s,
                        None => continue,
                    };
                    let implicit_constructor_root = sig.static_initializer;
                    if sig.internal && !implicit_constructor_root && !live.contains(&sig.name) {
                        continue;
                    }
                    (f.body.as_slice(), Some(sig.name))
                }
            };
            for name in &internal_names {
                if Some(name) == self_name.as_ref() || live.contains(name) {
                    continue;
                }
                let mentioned = if is_run_symbol(name) {
                    item_runs[item_index].contains(name.as_str())
                } else {
                    lines_mention_symbol(lines, name)
                };
                if mentioned {
                    live.insert(name.clone());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    drop(item_runs);
    items
        .into_iter()
        .filter(|it| match it {
            Item::Func(f) => match parse_def_header(&f.header) {
                Some(sig) if sig.internal => sig.static_initializer || live.contains(&sig.name),
                _ => true,
            },
            _ => true,
        })
        .collect()
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    let mut i = 0;
    while let Some(rel) = hay[i..].iter().position(|&b| b == needle[0]) {
        let at = i + rel;
        if hay[at..].starts_with(needle) {
            return true;
        }
        i = at + 1;
    }
    false
}

fn symbol_runs(lines: &[String]) -> HashSet<&str> {
    let mut runs = HashSet::default();
    for line in lines {
        let bytes = line.as_bytes();
        let mut i = 0;
        while let Some(rel) = bytes[i..].iter().position(|&b| b == b'@') {
            let at = i + rel;
            let mut end = at + 1;
            while end < bytes.len() && bytes[end] < 0x80 && is_name_char(bytes[end] as char) {
                end += 1;
            }
            runs.insert(&line[at..end]);
            i = end;
        }
    }
    runs
}

fn is_run_symbol(sym: &str) -> bool {
    let bytes = sym.as_bytes();
    bytes.first() == Some(&b'@')
        && bytes[1..]
            .iter()
            .all(|&b| b < 0x80 && is_name_char(b as char))
}

fn find_symbol_at(hay: &[u8], sym: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(rel) = hay[i..].iter().position(|&b| b == b'@') {
        let at = i + rel;
        if hay[at..].starts_with(sym) {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

fn find_dot_inl(hay: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(rel) = hay[i..].iter().position(|&b| b == b'.') {
        let at = i + rel;
        if hay[at..].starts_with(b".inl") {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

fn lines_mention_symbol(lines: &[String], sym: &str) -> bool {
    if sym.as_bytes().first() == Some(&b'@') {
        let sb = sym.as_bytes();
        return lines.iter().any(|l| {
            let lb = l.as_bytes();
            let mut start = 0;
            while let Some(pos) = find_symbol_at(&lb[start..], sb) {
                let after = start + pos + sb.len();
                if l[after..].chars().next().is_none_or(|c| !is_name_char(c)) {
                    return true;
                }
                start = after;
            }
            false
        });
    }
    lines.iter().any(|l| {
        let mut start = 0;
        while let Some(pos) = l[start..].find(sym) {
            let after = start + pos + sym.len();
            if l[after..].chars().next().is_none_or(|c| !is_name_char(c)) {
                return true;
            }
            start = after;
        }
        false
    })
}

fn render(items: &[Item], trailing_newline: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    for it in items {
        match it {
            Item::Raw(r) => lines.extend(r.iter().cloned()),
            Item::Func(f) => {
                lines.push(f.header.clone());
                lines.extend(f.body.iter().cloned());
                lines.push("}".to_string());
            }
        }
    }
    let mut s = lines.join("\n");
    if trailing_newline {
        s.push('\n');
    }
    s
}

fn internal_callgraph_has_cycle(internal: &HashMap<String, (DefSig, Vec<String>)>) -> bool {
    let mut edges: HashMap<&str, Vec<String>> = HashMap::default();
    for (name, (_, body)) in internal {
        let mut callees = Vec::new();
        for line in body {
            if let Some(c) = parse_call(line) {
                if internal.contains_key(&c.callee) {
                    callees.push(c.callee.clone());
                }
            }
        }
        edges.insert(name.as_str(), callees);
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Grey,
        Black,
    }
    let mut color: HashMap<&str, Color> = internal
        .keys()
        .map(|k| (k.as_str(), Color::White))
        .collect();
    fn dfs<'a>(
        n: &'a str,
        edges: &HashMap<&'a str, Vec<String>>,
        color: &mut HashMap<&'a str, Color>,
    ) -> bool {
        color.insert(n, Color::Grey);
        if let Some(cs) = edges.get(n) {
            for c in cs {
                match color.get(c.as_str()).copied() {
                    Some(Color::Grey) => return true,
                    Some(Color::White) => {
                        let key = edges.keys().find(|k| **k == c.as_str()).copied();
                        if let Some(k) = key {
                            if dfs(k, edges, color) {
                                return true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        color.insert(n, Color::Black);
        false
    }
    let names: Vec<&str> = internal.keys().map(|k| k.as_str()).collect();
    for n in names {
        if color.get(n).copied() == Some(Color::White) && dfs(n, &edges, &mut color) {
            return true;
        }
    }
    false
}

fn inline_body_to_fixpoint(
    mut body: Vec<String>,
    internal: &HashMap<String, (DefSig, Vec<String>)>,
    counter: &mut usize,
    selected_pointer: Option<&str>,
) -> Option<Vec<String>> {
    let mut budget = 100_000usize;
    loop {
        let Some(idx) = find_inlinable_call(&body, internal, selected_pointer) else {
            return Some(body);
        };
        body = inline_one(body, idx, internal, counter, selected_pointer.is_some())?;
        budget -= 1;
        if budget == 0 {
            return None;
        }
    }
}

fn find_inlinable_call(
    body: &[String],
    internal: &HashMap<String, (DefSig, Vec<String>)>,
    selected_pointer: Option<&str>,
) -> Option<usize> {
    for (i, line) in body.iter().enumerate() {
        if let Some(c) = parse_call(line) {
            if internal.contains_key(&c.callee)
                && selected_pointer
                    .is_none_or(|selected| c.args.iter().any(|argument| argument == selected))
            {
                return Some(i);
            }
        }
    }
    None
}

fn inline_one(
    body: Vec<String>,
    idx: usize,
    internal: &HashMap<String, (DefSig, Vec<String>)>,
    counter: &mut usize,
    preserve_caller_cfg: bool,
) -> Option<Vec<String>> {
    let call = parse_call(&body[idx])?;
    let (sig, callee_body) = internal.get(&call.callee)?;
    if sig.params.len() != call.args.len() {
        return None;
    }
    if preserve_caller_cfg && is_single_block_leaf(callee_body) {
        return inline_single_block_leaf(body, idx, &call, sig, callee_body, counter);
    }

    let k = *counter;
    *counter += 1;
    let prefix = format!(".inl{k}");
    let entry_label = format!("{prefix}.entry");
    let cont_label = format!("{prefix}.cont");

    let (local_values, local_labels) = collect_callee_locals(callee_body)?;

    let mut value_map: HashMap<String, String> = HashMap::default();
    for (p, a) in sig.params.iter().zip(call.args.iter()) {
        value_map.insert(p.clone(), a.clone());
    }
    for v in &local_values {
        if value_map.contains_key(v) {
            value_map.insert(v.clone(), format!("%{prefix}.{}", &v[1..]));
        } else {
            value_map.insert(v.clone(), format!("%{prefix}.{}", &v[1..]));
        }
    }
    for (p, a) in sig.params.iter().zip(call.args.iter()) {
        value_map.insert(p.clone(), a.clone());
    }

    let mut label_map: HashMap<String, String> = HashMap::default();
    for l in &local_labels {
        label_map.insert(l.clone(), format!("{prefix}.{l}"));
    }
    if let Some(id) = implicit_entry_block_id(&sig.params, callee_body) {
        label_map.entry(id).or_insert_with(|| entry_label.clone());
    }

    let mut renamed: Vec<String> = Vec::new();
    let mut returns: Vec<(String, String)> = Vec::new();
    let mut cur_block = entry_label.clone();
    let has_explicit_entry = callee_body
        .iter()
        .find(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with(';')
        })
        .is_some_and(|line| block_label_id(line).is_some());
    if !has_explicit_entry {
        renamed.push(format!("{entry_label}:"));
    }
    for line in callee_body {
        if let Some(bare) = block_label_id(line) {
            let mapped = label_map
                .get(bare)
                .cloned()
                .unwrap_or_else(|| bare.to_string());
            cur_block = mapped;
            renamed.push(rename_line(line, &value_map, &label_map));
            continue;
        }
        if let Some(ret) = parse_ret(line) {
            match ret {
                RetKind::Value(_ty, _val) => {
                    let renamed_val = substitute_value_token(&_val, &value_map);
                    returns.push((renamed_val, cur_block.clone()));
                }
                RetKind::Void => {
                    returns.push((String::new(), cur_block.clone()));
                }
            }
            let indent = leading_ws(line);
            renamed.push(format!("{indent}br label %{cont_label}"));
            continue;
        }
        renamed.push(rename_line(line, &value_map, &label_map));
    }

    let before: Vec<String> = body[..idx].to_vec();
    let after: Vec<String> = body[idx + 1..].to_vec();

    let bcall_label = enclosing_block_label(&before);

    let mut after_relabeled: Vec<String> = after
        .iter()
        .map(|l| relabel_phi_preds(l, bcall_label.as_deref(), &cont_label))
        .collect();

    let mut new_body: Vec<String> = Vec::with_capacity(body.len() + renamed.len() + 4);
    new_body.extend(before);
    let call_indent = leading_ws(&body[idx]);
    new_body.push(format!("{call_indent}br label %{entry_label}"));
    new_body.extend(renamed);
    new_body.push(format!("{cont_label}:"));
    if let Some(result) = &call.result {
        let non_void: Vec<&(String, String)> =
            returns.iter().filter(|(v, _)| !v.is_empty()).collect();
        if non_void.is_empty() {
            return None;
        }
        if non_void.len() == 1 {
            let mut return_value = HashMap::default();
            return_value.insert(result.clone(), non_void[0].0.clone());
            for line in &mut after_relabeled {
                *line = rename_line(line, &return_value, &HashMap::default());
            }
        } else {
            let ret_ty = if !call.ret_ty.is_empty() {
                call.ret_ty.clone()
            } else {
                sig.ret_ty.clone()
            };
            if ret_ty.is_empty() {
                return None;
            }
            let arms: Vec<String> = non_void
                .iter()
                .map(|(v, blk)| format!("[ {v}, %{blk} ]"))
                .collect();
            new_body.push(format!(
                "{call_indent}{result} = phi {ret_ty} {}",
                arms.join(", ")
            ));
        }
    }
    new_body.extend(after_relabeled);

    Some(new_body)
}

fn inline_single_block_leaf(
    body: Vec<String>,
    idx: usize,
    call: &CallSite,
    sig: &DefSig,
    callee_body: &[String],
    counter: &mut usize,
) -> Option<Vec<String>> {
    let prefix = format!(".inl{}", *counter);
    *counter += 1;
    let (local_values, _) = collect_callee_locals(callee_body)?;
    let mut value_map = HashMap::default();
    for (parameter, argument) in sig.params.iter().zip(&call.args) {
        value_map.insert(parameter.clone(), argument.clone());
    }
    for value in local_values {
        value_map.insert(value.clone(), format!("%{prefix}.{}", &value[1..]));
    }
    for (parameter, argument) in sig.params.iter().zip(&call.args) {
        value_map.insert(parameter.clone(), argument.clone());
    }

    let mut replacement = Vec::new();
    let mut returned = None;
    for line in callee_body {
        if block_label_id(line).is_some() {
            continue;
        }
        if let Some(ret) = parse_ret(line) {
            returned = Some(match ret {
                RetKind::Value(_, value) => substitute_value_token(&value, &value_map),
                RetKind::Void => String::new(),
            });
            continue;
        }
        replacement.push(rename_line(line, &value_map, &HashMap::default()));
    }
    let returned = returned?;
    let mut after = body[idx + 1..].to_vec();
    if let Some(result) = &call.result {
        if returned.is_empty() {
            return None;
        }
        let result_map = crate::fxhash::map_from([(result.clone(), returned)]);
        for line in &mut after {
            *line = rename_line(line, &result_map, &HashMap::default());
        }
    }
    let mut new_body = Vec::with_capacity(body.len() + replacement.len());
    new_body.extend_from_slice(&body[..idx]);
    new_body.extend(replacement);
    new_body.extend(after);
    Some(new_body)
}

fn is_single_block_leaf(body: &[String]) -> bool {
    body.iter()
        .filter(|line| block_label_id(line).is_some())
        .count()
        <= 1
        && body.iter().filter(|line| parse_ret(line).is_some()).count() == 1
        && !body.iter().any(|line| {
            let line = line.trim_start();
            line.starts_with("br ")
                || line.starts_with("switch ")
                || line.starts_with("indirectbr ")
                || line.starts_with("unreachable")
        })
}

fn substitute_value_token(tok: &str, value_map: &HashMap<String, String>) -> String {
    if let Some(rep) = value_map.get(tok) {
        rep.clone()
    } else {
        tok.to_string()
    }
}

enum RetKind {
    Value(String, String),
    Void,
}

fn parse_ret(line: &str) -> Option<RetKind> {
    let t = line.trim();
    let rest = t.strip_prefix("ret")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    if rest == "void" {
        return Some(RetKind::Void);
    }
    let toks = super::lex::split_top_level_whitespace(rest);
    if toks.len() < 2 {
        return None;
    }
    let val = (*toks.last()?).to_string();
    let ty = toks[..toks.len() - 1].join(" ");
    Some(RetKind::Value(ty, val))
}

fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

fn block_label_id(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let colon = rest.find(':')?;
    let ident = &rest[..colon];
    if ident.is_empty() {
        return None;
    }
    let bare = ident.strip_prefix('%').unwrap_or(ident);
    if !bare.chars().all(is_name_char) {
        return None;
    }
    let after = rest[colon + 1..].trim_start();
    if !after.is_empty() && !after.starts_with(';') {
        return None;
    }
    Some(bare)
}

fn collect_callee_locals(body: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let mut values: Vec<String> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut seen_v: HashSet<String> = HashSet::default();
    let mut seen_l: HashSet<String> = HashSet::default();
    for line in body {
        if let Some(bare) = block_label_id(line) {
            if seen_l.insert(bare.to_string()) {
                labels.push(bare.to_string());
            }
            continue;
        }
        if let Some(lhs) = def_lhs(line) {
            if seen_v.insert(lhs.to_string()) {
                values.push(lhs.to_string());
            }
        }
    }
    Some((values, labels))
}

fn implicit_entry_block_id(params: &[String], body: &[String]) -> Option<String> {
    for line in body {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') {
            continue;
        }
        if block_label_id(line).is_some() {
            return None;
        }
        break;
    }
    let mut ids: Vec<u32> = Vec::with_capacity(params.len());
    for p in params {
        let bare = p.strip_prefix('%')?;
        if let Ok(id) = bare.parse::<u32>() {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    if ids.iter().enumerate().any(|(i, id)| *id != i as u32) {
        return None;
    }
    Some(ids.len().to_string())
}

fn def_lhs(line: &str) -> Option<&str> {
    let t = line.trim();
    let eq = t.find(" = ")?;
    let lhs = t[..eq].trim();
    lhs.starts_with('%').then_some(lhs)
}

fn enclosing_block_label(before: &[String]) -> Option<String> {
    for line in before.iter().rev() {
        if let Some(bare) = block_label_id(line) {
            return Some(bare.to_string());
        }
    }
    None
}

fn relabel_phi_preds(line: &str, bcall: Option<&str>, cont: &str) -> String {
    let Some(bcall) = bcall else {
        return line.to_string();
    };
    if !is_phi_line(line) {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        let Some(open) = rest.find('[') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..=open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            out.push_str(after);
            break;
        };
        let inner = &after[..close];
        let parts = split_top_level(inner, ',');
        if parts.len() == 2 {
            let val = parts[0].trim();
            let lbl = parts[1].trim();
            let lbl_bare = lbl.strip_prefix('%').unwrap_or(lbl);
            if lbl_bare == bcall {
                out.push_str(&format!(" {val}, %{cont} "));
            } else {
                out.push_str(inner);
            }
        } else {
            out.push_str(inner);
        }
        out.push(']');
        rest = &after[close + 1..];
    }
    out
}

fn is_phi_line(line: &str) -> bool {
    if let Some(eq) = line.trim().find(" = ") {
        let rhs = line.trim()[eq + 3..].trim_start();
        return rhs.starts_with("phi ") || rhs == "phi";
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_inlining_starts_after_existing_namespace() {
        let source = "%x.inl2.value = add i32 1, 2\n.inl17.block:\n  ret void\n";
        assert_eq!(next_inline_ordinal(source), 18);
        assert_eq!(next_inline_ordinal("define void @f() { ret void }\n"), 0);
    }

    #[test]
    fn rename_is_token_accurate() {
        let mut vm = HashMap::default();
        vm.insert("%1".to_string(), "%.inl0.1".to_string());
        let line = "  %10 = add i32 %1, %11";
        let out = rename_line(line, &vm, &HashMap::default());
        assert_eq!(out, "  %10 = add i32 %.inl0.1, %11", "got: {out}");
    }

    #[test]
    fn rename_maps_label_positions() {
        let mut lm = HashMap::default();
        lm.insert("5".to_string(), ".inl0.5".to_string());
        let line = "  br label %5";
        let out = rename_line(line, &HashMap::default(), &lm);
        assert_eq!(out, "  br label %.inl0.5", "got: {out}");
    }

    #[test]
    fn no_internal_calls_is_identical() {
        let src = "\
define void @main(i32 %0) {
  %1 = add i32 %0, 1
  ret void
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn declared_not_defined_is_not_inlined() {
        let src = "\
declare i32 @helper(i32)
define i32 @main(i32 %0) {
  %1 = call i32 @helper(i32 %0)
  ret i32 %1
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn intrinsic_call_is_not_inlined() {
        let src = "\
declare float @llvm.fabs.f32(float)
define float @main(float %0) {
  %1 = call float @llvm.fabs.f32(float %0)
  ret float %1
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn indirect_call_is_not_inlined() {
        let src = "\
define i32 @main(i32 %0, ptr %fp) {
  %1 = call i32 %fp(i32 %0)
  ret i32 %1
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn single_return_value_callee_inlined() {
        let src = "\
define internal i32 @add1(i32 %0) {
  %1 = add i32 %0, 1
  ret i32 %1
}
define i32 @main(i32 %0) {
  %r = call i32 @add1(i32 %0)
  %2 = mul i32 %r, 2
  ret i32 %2
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(!out.contains("call i32 @add1"), "call not removed:\n{out}");
        assert!(
            out.contains(".inl0.1 = add i32 %0, 1"),
            "body not inlined:\n{out}"
        );
        assert!(
            !out.contains("%r = phi i32"),
            "redundant result phi:\n{out}"
        );
        assert!(out.contains(".inl0.cont:"), "no cont block:\n{out}");
        assert!(
            out.contains("br label %.inl0.entry"),
            "no entry branch:\n{out}"
        );
        assert!(
            out.contains("%2 = mul i32 %.inl0.1, 2"),
            "post-call result not forwarded:\n{out}"
        );
    }

    #[test]
    fn void_callee_inlined() {
        let src = "\
define internal void @sideeffect(ptr %0, i32 %1) {
  store i32 %1, ptr %0
  ret void
}
define void @main(ptr %0, i32 %1) {
  call void @sideeffect(ptr %0, i32 %1)
  ret void
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(
            !out.contains("call void @sideeffect"),
            "call not removed:\n{out}"
        );
        assert!(
            out.contains("store i32 %1, ptr %0"),
            "store not inlined:\n{out}"
        );
        assert!(!out.contains("= phi"), "void return should not phi:\n{out}");
        assert!(
            out.contains("br label %.inl0.cont"),
            "ret not turned to branch:\n{out}"
        );
    }

    #[test]
    fn two_return_callee_builds_phi() {
        let src = "\
define internal i32 @sel(i1 %0, i32 %1, i32 %2) {
  br i1 %0, label %t, label %f
t:
  ret i32 %1
f:
  ret i32 %2
}
define i32 @main(i1 %0, i32 %1, i32 %2) {
  %r = call i32 @sel(i1 %0, i32 %1, i32 %2)
  ret i32 %r
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(!out.contains("call i32 @sel"), "call not removed:\n{out}");
        assert!(out.contains("%r = phi i32"), "no merge phi:\n{out}");
        assert!(out.contains("%.inl0.t"), "missing t pred:\n{out}");
        assert!(out.contains("%.inl0.f"), "missing f pred:\n{out}");
        assert_eq!(
            out.matches("br label %.inl0.cont").count(),
            2,
            "want 2 branches to cont:\n{out}"
        );
        assert!(out.contains("[ %1, %.inl0.t ]"), "arm t wrong:\n{out}");
        assert!(out.contains("[ %2, %.inl0.f ]"), "arm f wrong:\n{out}");
    }

    #[test]
    fn transitive_inlining() {
        let src = "\
define internal i32 @inner(i32 %0) {
  %1 = add i32 %0, 1
  ret i32 %1
}
define internal i32 @outer(i32 %0) {
  %1 = call i32 @inner(i32 %0)
  %2 = mul i32 %1, 2
  ret i32 %2
}
define i32 @main(i32 %0) {
  %r = call i32 @outer(i32 %0)
  ret i32 %r
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(
            !out.contains("call i32 @outer"),
            "outer call remains:\n{out}"
        );
        assert!(
            !out.contains("call i32 @inner"),
            "inner call remains:\n{out}"
        );
        let prefixes: crate::fxhash::HashSet<&str> = out
            .match_indices(".inl")
            .filter_map(|(i, _)| out[i..].split('.').nth(1))
            .collect();
        assert!(
            prefixes.len() >= 2,
            "expected >=2 distinct inline prefixes, got {prefixes:?}:\n{out}"
        );
        assert!(
            !out.contains("define internal i32 @outer"),
            "dead @outer not swept:\n{out}"
        );
        assert!(
            !out.contains("define internal i32 @inner"),
            "dead @inner not swept:\n{out}"
        );
        assert!(out.contains("add i32"), "inner body missing:\n{out}");
        assert!(out.contains("mul i32"), "outer body missing:\n{out}");
    }

    #[test]
    fn implicit_entry_id_and_fast_fastcc_are_handled() {
        let src = "\
define internal fast fastcc float @sel(float %0, float %1, float %.a, float %.b, float %2, float %3, float %4, float %5) {
  %7 = fcmp olt float %0, %1
  br i1 %7, label %8, label %9
8:
  br label %9
9:
  %10 = phi float [ %0, %6 ], [ %1, %8 ]
  %11 = fcmp olt float %10, %.a
  br i1 %11, label %12, label %13
12:
  ret float %10
13:
  ret float %.b
}
define float @main(float %0, float %1, float %2, float %3, float %4, float %5, float %6, float %7) {
  %r = call fast fastcc float @sel(float %0, float %1, float %2, float %3, float %4, float %5, float %6, float %7)
  ret float %r
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(
            !out.contains("call fast fastcc"),
            "call not inlined:\n{out}"
        );
        assert!(
            !out.contains("[ %0, %6 ]"),
            "implicit entry id %6 left un-renamed:\n{out}"
        );
        assert!(
            out.contains(".inl0.entry"),
            "entry label not minted:\n{out}"
        );
        assert!(out.contains("%r = phi float"), "result phi missing:\n{out}");
        assert!(
            !out.contains("phi fast fastcc"),
            "cc/fast-math leaked into phi type:\n{out}"
        );
    }

    #[test]
    fn recursive_pair_bails() {
        let src = "\
define internal i32 @a(i32 %0) {
  %1 = call i32 @b(i32 %0)
  ret i32 %1
}
define internal i32 @b(i32 %0) {
  %1 = call i32 @a(i32 %0)
  ret i32 %1
}
define i32 @main(i32 %0) {
  %r = call i32 @a(i32 %0)
  ret i32 %r
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn direct_self_recursion_bails() {
        let src = "\
define internal i32 @rec(i32 %0) {
  %1 = call i32 @rec(i32 %0)
  ret i32 %1
}
define i32 @main(i32 %0) {
  %r = call i32 @rec(i32 %0)
  ret i32 %r
}
";
        assert_eq!(inline_nonrecursive_internal_calls(src), src);
    }

    #[test]
    fn multiblock_callee_labels_prefixed() {
        let src = "\
define internal i32 @clamp(i32 %0) {
  %1 = icmp slt i32 %0, 0
  br i1 %1, label %neg, label %pos
neg:
  br label %done
pos:
  br label %done
done:
  %2 = phi i32 [ 0, %neg ], [ %0, %pos ]
  ret i32 %2
}
define i32 @main(i32 %0) {
  %r = call i32 @clamp(i32 %0)
  ret i32 %r
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(!out.contains("call i32 @clamp"), "call remains:\n{out}");
        assert!(
            out.contains("phi i32 [ 0, %.inl0.neg ], [ %0, %.inl0.pos ]"),
            "inner phi not renamed:\n{out}"
        );
        assert!(out.contains(".inl0.neg:"), "neg label missing:\n{out}");
        assert!(out.contains(".inl0.done:"), "done label missing:\n{out}");
    }

    #[test]
    fn caller_phi_pred_relabeled_to_cont() {
        let src = "\
define internal i32 @id(i32 %0) {
  ret i32 %0
}
define i32 @main(i32 %0) {
entry:
  br label %loop
loop:
  %r = call i32 @id(i32 %0)
  %next = add i32 %r, 1
  %acc = phi i32 [ 0, %entry ], [ %next, %loop ]
  br label %loop
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(
            out.contains("[ %next, %.inl0.cont ]"),
            "phi pred not relabeled to cont:\n{out}"
        );
        assert!(out.contains("[ 0, %entry ]"), "entry arm changed:\n{out}");
    }

    #[test]
    fn caller_phi_pred_before_call_value_relabeled_to_cont() {
        let src = "\
define internal i32 @helper(i32 %0) {
  ret i32 %0
}
define i32 @main(i32 %0, i1 %c) {
entry:
  br label %bcall
bcall:
  %pre = add i32 %0, 7
  %r = call i32 @helper(i32 %0)
  br i1 %c, label %taken, label %merge
taken:
  br label %merge
merge:
  %m = phi i32 [ %pre, %bcall ], [ %r, %taken ]
  ret i32 %m
}
";
        let out = inline_nonrecursive_internal_calls(src);
        assert!(!out.contains("call i32 @helper"), "call remains:\n{out}");
        assert!(
            out.contains("[ %pre, %.inl0.cont ]"),
            "before-call value's phi pred not relabeled to cont:\n{out}"
        );
        assert!(
            !out.contains("[ %pre, %bcall ]"),
            "stale %bcall phi predecessor survived:\n{out}"
        );
    }

    #[test]
    fn pointer_select_consumer_inline_is_structurally_planned() {
        let src = r#"
@fallback = internal addrspace(2) global i32 0
@fc_default = internal addrspace(2) global i8 0

define internal void @_GLOBAL__sub_I_defaults() section "air.static_init" {
entry:
  store i8 1, ptr addrspace(2) @fc_default
  ret void
}

define internal i32 @consume(ptr addrspace(2) %pointer, i1 %branch) {
entry:
  br i1 %branch, label %left, label %right
left:
  %value = load i32, ptr addrspace(2) %pointer
  ret i32 %value
right:
  ret i32 0
}

define internal i32 @unrelated(i32 %value) {
entry:
  %sum = add i32 %value, 1
  ret i32 %sum
}

define internal i32 @consume_leaf(ptr addrspace(2) %pointer) {
entry:
  %value = load i32, ptr addrspace(2) %pointer
  ret i32 %value
}

define i32 @main(ptr addrspace(2) %runtime, i1 %choose) {
entry:
  %first = select i1 %choose, ptr addrspace(2) %runtime, ptr addrspace(2) @fallback
  %selected = select i1 %choose, ptr addrspace(2) @fallback, ptr addrspace(2) %first
  %leaf = call i32 @consume_leaf(ptr addrspace(2) %selected)
  %consumed = call i32 @consume(ptr addrspace(2) %selected, i1 %choose)
  %sum = add i32 %leaf, %consumed
  %other = call i32 @unrelated(i32 %sum)
  ret i32 %other
}
"#;
        let plan = inline_pointer_select_consumers(src, Some("main"));
        let out = &plan.source;
        assert!(plan.requires_relooper);
        assert!(!out.contains("call i32 @consume"), "{out}");
        assert!(out.contains("call i32 @unrelated"), "{out}");
        assert!(out.contains("define internal i32 @unrelated"), "{out}");
        assert!(
            out.contains("define internal void @_GLOBAL__sub_I_defaults"),
            "implicit constructor root was swept:\n{out}"
        );
        assert!(out.contains(".left:"), "{out}");

        assert!(!out.contains("call i32 @consume_leaf"), "{out}");
    }
}

#[cfg(test)]
mod b52_equiv_tests {
    use super::*;

    fn old_lines_mention_symbol(lines: &[String], sym: &str) -> bool {
        lines.iter().any(|l| {
            let mut start = 0;
            while let Some(pos) = l[start..].find(sym) {
                let after = start + pos + sym.len();
                if l[after..].chars().next().is_none_or(|c| !is_name_char(c)) {
                    return true;
                }
                start = after;
            }
            false
        })
    }

    #[test]
    fn symbol_runs_and_the_at_scan_answer_exactly_what_str_find_answered() {
        let lines: Vec<String> = [
            "  %r = call float @foo(float %x)",
            "  call void @foo.bar()",
            "x@foo",
            "@foo",
            "@foo-1 @foo$ @@foo",
            "@foo\u{e9} tail",
            "store ptr @\"quoted name\", ptr %p",
            "",
            "@",
            "@ @foo2",
            "@fo",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let syms = [
            "@foo",
            "@foo.bar",
            "@foo-1",
            "@foo$",
            "@foo2",
            "@",
            "@fo",
            "@bar",
            "@\"quoted name\"",
        ];
        let mut sets: Vec<Vec<String>> = lines.iter().map(|l| vec![l.clone()]).collect();
        sets.push(lines.clone());
        let (mut hits, mut misses, mut via_runs) = (0, 0, 0);
        for set in &sets {
            let runs = symbol_runs(set);
            for sym in syms {
                let want = old_lines_mention_symbol(set, sym);
                assert_eq!(
                    lines_mention_symbol(set, sym),
                    want,
                    "at-scan {sym} in {set:?}"
                );
                if is_run_symbol(sym) {
                    assert_eq!(runs.contains(sym), want, "runs {sym} in {set:?}");
                    via_runs += 1;
                }
                if want {
                    hits += 1;
                } else {
                    misses += 1;
                }
            }
        }
        assert!(
            !is_run_symbol("@\"quoted name\"") && is_run_symbol("@foo.bar") && is_run_symbol("@")
        );
        assert!(!is_run_symbol("foo") && !is_run_symbol("@foo\u{e9}"));
        assert!(
            hits >= 10 && misses >= 10 && via_runs >= 80,
            "hits {hits}, misses {misses}, via_runs {via_runs}"
        );
    }

    #[test]
    fn contains_bytes_finds_every_placement_and_parse_call_still_parses_calls() {
        assert!(contains_bytes(b"call ", b"call "));
        assert!(contains_bytes(b"ccall x", b"call "));
        assert!(contains_bytes(b"xx tail call @f()", b"call "));
        assert!(!contains_bytes(b"call", b"call "));
        assert!(!contains_bytes(b"", b"call "));
        assert!(!contains_bytes(b"%r = add i32 %a, %b", b"call "));
        let call = parse_call("  %r = tail call float @foo(float %x)").expect("a call parses");
        assert_eq!(call.callee, "@foo");
        assert!(parse_call("  %r = fadd float %x, %y").is_none());
        assert!(parse_call("  %r = callx float @foo(float %x)").is_none());
    }

    #[test]
    fn find_dot_inl_is_the_leftmost_occurrence_str_find_gave() {
        for hay in [
            "",
            ".inl",
            "a.inl3.x",
            "..inl",
            ".in.inl",
            "x.inlq.inl7.",
            ".i",
            "no dots",
        ] {
            assert_eq!(
                find_dot_inl(hay.as_bytes()),
                hay.find(".inl"),
                "hay {hay:?}"
            );
        }
    }
}

pub(crate) fn forward_builder_pointer_fields(san_ll: &str) -> std::borrow::Cow<'_, str> {
    if !contains_bytes(san_ll.as_bytes(), b"insertvalue") || !contains_bytes(san_ll.as_bytes(), b"extractvalue") {
        return std::borrow::Cow::Borrowed(san_ll);
    }
    let Some(mut items) = parse_items(san_ll) else {
        return std::borrow::Cow::Borrowed(san_ll);
    };
    let mut builders: HashMap<String, Vec<(Vec<String>, usize)>> = HashMap::default();
    for item in &items {
        if let Item::Func(f) = item {
            if let Some((name, fields)) = builder_pointer_fields(f) {
                builders.insert(name, fields);
            }
        }
    }
    if builders.is_empty() {
        return std::borrow::Cow::Borrowed(san_ll);
    }
    let mut changed = false;
    for item in &mut items {
        let Item::Func(f) = item else { continue };
        let mut results: HashMap<String, (&Vec<(Vec<String>, usize)>, Vec<String>)> = HashMap::default();
        for line in &f.body {
            if let Some(call) = parse_call(line) {
                if let (Some(result), Some(fields)) = (call.result, builders.get(&call.callee)) {
                    results.insert(result, (fields, call.args));
                }
            }
        }
        if results.is_empty() {
            continue;
        }
        let mut value_map: HashMap<String, String> = HashMap::default();
        let mut drop = Vec::new();
        for (n, line) in f.body.iter().enumerate() {
            let Some((dst, agg, path)) = parse_extractvalue(line) else { continue };
            let Some((fields, args)) = results.get(&agg) else { continue };
            let Some((_, pos)) = fields.iter().find(|(p, _)| *p == path) else { continue };
            match args.get(*pos) {
                Some(arg) if arg.starts_with('%') => {
                    value_map.insert(dst, arg.clone());
                    drop.push(n);
                }
                _ => {}
            }
        }
        if value_map.is_empty() {
            continue;
        }
        let empty = HashMap::default();
        let body = std::mem::take(&mut f.body);
        f.body = body
            .into_iter()
            .enumerate()
            .filter(|(n, _)| !drop.contains(n))
            .map(|(_, line)| rename_line(&line, &value_map, &empty))
            .collect();
        changed = true;
    }
    if !changed {
        return std::borrow::Cow::Borrowed(san_ll);
    }
    let mut out = String::with_capacity(san_ll.len());
    for item in items {
        match item {
            Item::Raw(lines) => {
                for line in lines {
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            Item::Func(f) => {
                out.push_str(&f.header);
                out.push('\n');
                for line in f.body {
                    out.push_str(&line);
                    out.push('\n');
                }
                out.push_str("}\n");
            }
        }
    }
    std::borrow::Cow::Owned(out)
}

fn builder_pointer_fields(f: &FuncBlock) -> Option<(String, Vec<(Vec<String>, usize)>)> {
    let sig = parse_def_header(&f.header)?;
    if !sig.internal {
        return None;
    }
    let mut chain: HashMap<String, (String, Vec<String>, String)> = HashMap::default();
    let mut ret = None;
    for line in &f.body {
        let t = line.trim();
        if t.is_empty() || t.starts_with(';') || t.ends_with(':') || t.starts_with("entry:") {
            continue;
        }
        if let Some(rest) = t.strip_prefix("ret ") {
            if ret.is_some() {
                return None;
            }
            ret = Some(rest.split_whitespace().last()?.to_string());
            continue;
        }
        let (dst, agg, path, val) = parse_insertvalue(t)?;
        chain.insert(dst, (agg, path, val));
    }
    let mut cur = ret?;
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut fields = Vec::new();
    while let Some((agg, path, val)) = chain.get(&cur) {
        if !seen.contains(path) {
            seen.push(path.clone());
            if let Some(pos) = sig.params.iter().position(|p| p == val) {
                if param_is_pointer(&f.header, val) {
                    fields.push((path.clone(), pos));
                }
            }
        }
        cur = agg.clone();
    }
    if cur != "poison" && cur != "undef" {
        return None;
    }
    Some((sig.name, fields))
}

fn param_is_pointer(header: &str, name: &str) -> bool {
    let Some(open) = header.find('(') else { return false };
    let params = &header[open + 1..];
    for piece in split_top_level_commas(params) {
        let piece = piece.trim().trim_end_matches(|c| c == ')' || c == '{').trim();
        if piece.split_whitespace().last() == Some(name) {
            return piece.starts_with("ptr");
        }
    }
    false
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0i32, false, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '<' | '{' | '[' | '(' if !quoted => depth += 1,
            '>' | '}' | ']' | ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn parse_insertvalue(t: &str) -> Option<(String, String, Vec<String>, String)> {
    let (dst, rhs) = t.split_once(" = ")?;
    let rhs = rhs.trim().strip_prefix("insertvalue ")?;
    let parts = split_top_level_commas(rhs);
    if parts.len() < 3 {
        return None;
    }
    let agg = parts[0].split_whitespace().last()?.to_string();
    let val = parts[1].split_whitespace().last()?.to_string();
    let path: Vec<String> = parts[2..].iter().map(|p| p.trim().to_string()).collect();
    if path.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some((dst.trim().to_string(), agg, path, val))
}

fn parse_extractvalue(line: &str) -> Option<(String, String, Vec<String>)> {
    if !contains_bytes(line.as_bytes(), b"extractvalue ") {
        return None;
    }
    let (dst, rhs) = line.trim().split_once(" = ")?;
    let rhs = rhs.trim().strip_prefix("extractvalue ")?;
    let parts = split_top_level_commas(rhs);
    if parts.len() < 2 {
        return None;
    }
    let agg = parts[0].split_whitespace().last()?.to_string();
    let path: Vec<String> = parts[1..]
        .iter()
        .map(|p| p.trim().to_string())
        .take_while(|p| !p.starts_with('!'))
        .collect();
    if path.is_empty() || path.iter().any(|p| !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some((dst.trim().to_string(), agg, path))
}

#[cfg(test)]
mod builder_forward_tests {
    use super::forward_builder_pointer_fields;

    const CI: &str = "%\"struct.coreimage::Sampler.131\" = type { ptr addrspace(1), { ptr addrspace(2) }, ptr addrspace(2), <2 x float> }\ndefine internal fastcc %\"struct.coreimage::Sampler.131\" @b(ptr addrspace(1) %0, ptr addrspace(2) %1, ptr addrspace(2) noundef align 16 %2, <2 x float> %3) {\n  %5 = insertvalue %\"struct.coreimage::Sampler.131\" poison, ptr addrspace(1) %0, 0\n  %6 = insertvalue %\"struct.coreimage::Sampler.131\" %5, ptr addrspace(2) %1, 1, 0\n  %7 = insertvalue %\"struct.coreimage::Sampler.131\" %6, ptr addrspace(2) %2, 2\n  %8 = insertvalue %\"struct.coreimage::Sampler.131\" %7, <2 x float> %3, 3\n  ret %\"struct.coreimage::Sampler.131\" %8\n}\ndefine void @k(ptr addrspace(2) %buffer0, ptr addrspace(1) %t, ptr addrspace(2) %s, <2 x float> %c) {\n  %9 = getelementptr i8, ptr addrspace(2) %buffer0, i64 80\n  %14 = call fastcc %\"struct.coreimage::Sampler.131\" @b(ptr addrspace(1) %t, ptr addrspace(2) %s, ptr addrspace(2) %9, <2 x float> %c)\n  %16 = extractvalue %\"struct.coreimage::Sampler.131\" %14, 1, 0\n  %17 = extractvalue %\"struct.coreimage::Sampler.131\" %14, 2\n  %18 = extractvalue %\"struct.coreimage::Sampler.131\" %14, 3\n  %170 = load <4 x float>, ptr addrspace(2) %17, align 16\n  ret void\n}\n";

    #[test]
    fn a_builder_pointer_field_reads_as_the_call_argument() {
        let out = forward_builder_pointer_fields(CI);
        assert!(out.contains("load <4 x float>, ptr addrspace(2) %9, align 16"), "{out}");
        assert!(out.contains("%170 = load"), "a longer name sharing the prefix must stay: {out}");
        assert!(!out.contains("%17 = extractvalue"), "{out}");
        assert!(!out.contains("%16 = extractvalue"), "the nested sampler pointer forwards too: {out}");
    }

    #[test]
    fn a_non_pointer_field_keeps_its_extractvalue() {
        let out = forward_builder_pointer_fields(CI);
        assert!(out.contains("%18 = extractvalue"), "only pointer fields are forwarded: {out}");
    }

    #[test]
    fn a_helper_that_computes_is_not_a_builder() {
        let src = CI.replace("  ret %\"struct", "  %x = add i32 1, 2\n  ret %\"struct");
        assert_ne!(src, CI, "the negative arm must actually change the builder");
        assert!(matches!(forward_builder_pointer_fields(&src), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn a_module_without_aggregates_is_borrowed() {
        let src = "define void @k() {\n  ret void\n}\n";
        assert!(matches!(forward_builder_pointer_fields(src), std::borrow::Cow::Borrowed(_)));
    }
}
