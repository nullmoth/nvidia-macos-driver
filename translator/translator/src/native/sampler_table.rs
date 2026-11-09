//! A program-scope array of samplers.
//!
//! `constexpr constant sampler table[N] = {...}` becomes a module constant holding one struct per element, each naming
//! the element's `@__air_sampler_state.K` global, and the shader loads a sampler pointer out of it:
//!
//! ```llvm
//! @table = internal addrspace(2) constant [2 x %"struct.metal::sampler"] [ { ptr addrspace(2) @__air_sampler_state.1 }, ... ]
//! %g = getelementptr inbounds [2 x %"struct.metal::sampler"], ptr addrspace(2) @table, i64 0, i64 %i, i32 0
//! %s = load ptr addrspace(2), ptr addrspace(2) %g
//! ```
//!
//! The emitter has no initializer for an array of such structs and refused the module ("unsupported global initializer"),
//! which is how an application that builds its render shaders at start-up (Goodnotes) failed to open any document. Every
//! element is already a static sampler, so the load is replaced by the element's global: a constant index names it
//! directly, and a computed index becomes a chain of selects over the element globals. The table is then unused and removed.
//! A module that uses the table any other way keeps it, and keeps the refusal.

use std::borrow::Cow;

const SAMPLER_TYPE: &str = "%\"struct.metal::sampler\"";

struct Table {
    name: String,
    elements: Vec<String>,
}

pub(crate) fn lower_global_sampler_tables(san_ll: &str) -> Cow<'_, str> {
    if !san_ll.contains(SAMPLER_TYPE) || !san_ll.contains("x %\"struct.metal::sampler\"]") {
        return Cow::Borrowed(san_ll);
    }
    let tables: Vec<Table> = san_ll.lines().filter_map(parse_table).collect();
    if tables.is_empty() {
        return Cow::Borrowed(san_ll);
    }
    let mut text = san_ll.to_string();
    let mut changed = false;
    for table in &tables {
        if let Some(lowered) = lower_one_table(&text, table) {
            text = lowered;
            changed = true;
        }
    }
    if changed {
        Cow::Owned(text)
    } else {
        Cow::Borrowed(san_ll)
    }
}

/// `@name = ... constant [N x %"struct.metal::sampler"] [ %"struct.metal::sampler" { ptr addrspace(2) @g }, ...`
fn parse_table(line: &str) -> Option<Table> {
    let name = line.strip_prefix('@')?.split_once(" = ")?.0;
    let (head, body) = line.split_once(" constant [")?;
    if !head.contains("addrspace(2)") {
        return None;
    }
    let (count, rest) = body.split_once(" x ")?;
    let count: usize = count.parse().ok()?;
    let rest = rest.strip_prefix(SAMPLER_TYPE)?.strip_prefix("] [")?;
    let mut elements = Vec::new();
    let mut tail = rest;
    while let Some(at) = tail.find("ptr addrspace(2) @") {
        let after = &tail[at + "ptr addrspace(2) ".len()..];
        let end = after.find([' ', '}', ',']).unwrap_or(after.len());
        elements.push(after[..end].to_string());
        tail = &after[end..];
    }
    (count > 0 && elements.len() == count).then(|| Table { name: name.to_string(), elements })
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$' | '-')
}

/// `text` with every whole-token `from` replaced by `to`.
fn replace_token(text: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(from) {
        let before_ok = !rest[..at].chars().next_back().is_some_and(is_ident_char);
        let after = &rest[at + from.len()..];
        let after_ok = !after.chars().next().is_some_and(is_ident_char);
        out.push_str(&rest[..at]);
        out.push_str(if before_ok && after_ok { to } else { from });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn count_token(text: &str, token: &str) -> usize {
    let mut count = 0;
    let mut rest = text;
    while let Some(at) = rest.find(token) {
        let before_ok = !rest[..at].chars().next_back().is_some_and(is_ident_char);
        let after = &rest[at + token.len()..];
        count += usize::from(before_ok && !after.chars().next().is_some_and(is_ident_char));
        rest = after;
    }
    count
}

fn lower_one_table(text: &str, table: &Table) -> Option<String> {
    let global = format!("@{}", table.name);
    let type_array = format!("[{} x {SAMPLER_TYPE}]", table.elements.len());
    // address of an element's pointer field: (result, index operand)
    let mut addresses: Vec<(String, String)> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with(&format!("{global} = ")) {
            continue;
        }
        if let Some((result, index)) = parse_element_address(t, &global, &type_array) {
            addresses.push((result, index));
            continue;
        }
        lines.push(line.to_string());
    }
    if addresses.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut renames: Vec<(String, String)> = Vec::new();
    let mut dynamic: Vec<(String, String)> = Vec::new();
    for line in &lines {
        let t = line.trim();
        let loaded = t.split_once(" = load ptr addrspace(2), ptr addrspace(2) ").and_then(|(result, rest)| {
            let address = rest.split([',', ' ']).next()?;
            let (_, index) = addresses.iter().find(|(a, _)| a == address)?;
            Some((result.to_string(), index.clone()))
        });
        let Some((result, index)) = loaded else {
            out.push(line.clone());
            continue;
        };
        if let Ok(constant) = index.parse::<usize>() {
            let element = table.elements.get(constant)?;
            renames.push((result, element.clone()));
            continue;
        }
        dynamic.push((result, index));
    }
    // a sampler picked at run time has no one state for a call to carry, so each sampling call that reads it is made
    // once per element and its result picked by the index; a sample has no side effect
    let mut expanded: Vec<String> = Vec::with_capacity(out.len());
    for line in out {
        let Some((result, index)) = dynamic.iter().find(|(r, _)| replace_token(&line, r, "") != line) else {
            expanded.push(line);
            continue;
        };
        let t = line.trim();
        let call = t.split_once(" = ").filter(|(_, c)| {
            let c = c.strip_prefix("tail ").unwrap_or(c);
            c.starts_with("call ") && (c.contains(" @air.sample_") || c.contains(" @air.gather_"))
        });
        let Some((value, call)) = call.filter(|_| count_token(t, result) == 1) else {
            return None;
        };
        let ty = call.strip_prefix("tail ").unwrap_or(call).strip_prefix("call ")?.split_once(" @air.")?.0.to_string();
        let indent = &line[..line.len() - line.trim_start().len()];
        let base = value.trim_start_matches('%');
        for (k, element) in table.elements.iter().enumerate() {
            expanded.push(format!("{indent}%{base}.t{k} = {}", replace_token(call, result, element)));
        }
        let mut current = format!("%{base}.t{}", table.elements.len() - 1);
        for k in (0..table.elements.len() - 1).rev() {
            let cond = format!("%{base}.c{k}");
            let pick = if k == 0 { value.to_string() } else { format!("%{base}.m{k}") };
            expanded.push(format!("{indent}{cond} = icmp eq i64 {index}, {k}"));
            expanded.push(format!("{indent}{pick} = select i1 {cond}, {ty} %{base}.t{k}, {ty} {current}"));
            current = pick;
        }
        if table.elements.len() == 1 {
            return None;
        }
    }
    let out = expanded;
    let mut joined = out.join("\n");
    joined.push('\n');
    for (from, to) in &renames {
        joined = replace_token(&joined, from, to);
    }
    // any other use of the table, or of an address that is still read some other way, leaves the module as it was
    if joined.contains(&global) {
        return None;
    }
    for (address, _) in &addresses {
        let used = joined.lines().any(|l| {
            let t = l.trim();
            !t.starts_with(&format!("{address} = ")) && replace_token(t, address, "") != t
        });
        if used {
            return None;
        }
    }
    Some(joined)
}

/// `%r = getelementptr [inbounds] [N x sampler], ptr addrspace(2) @table, i64 0, i64 IDX, i32 0`, or the flat spelling.
fn parse_element_address(line: &str, global: &str, type_array: &str) -> Option<(String, String)> {
    let (result, rest) = line.split_once(" = getelementptr ")?;
    let rest = rest.strip_prefix("inbounds ").unwrap_or(rest);
    let pieces: Vec<&str> = rest.split(", ").map(str::trim).collect();
    let base_ty = if pieces.first() == Some(&type_array) {
        true
    } else if pieces.first() == Some(&SAMPLER_TYPE) {
        false
    } else {
        return None;
    };
    if pieces.get(1) != Some(&format!("ptr addrspace(2) {global}").as_str()) {
        return None;
    }
    let indices = pieces.get(2..)?;
    let (index, field) = match (base_ty, indices) {
        (true, [zero, index, field]) if *zero == "i64 0" || *zero == "i32 0" => (index, field),
        (false, [index, field]) => (index, field),
        _ => return None,
    };
    if *field != "i32 0" {
        return None;
    }
    // the compares written for a computed index are 64-bit
    let index = index.strip_prefix("i64 ")?;
    if !index.chars().all(|c| c.is_ascii_digit()) && !index.starts_with('%') {
        return None;
    }
    Some((result.trim().to_string(), index.to_string()))
}
