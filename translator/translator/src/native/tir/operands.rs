use super::*;
use crate::fxhash::HashMap;
use crate::native::ir::{LlGep, LlType, LlValue, TypedValue};
use crate::native::lex::{matching_paren, strip_comment};
use crate::native::parse::{
    parse_call, parse_constant_vector, parse_identity_ptr_bitcast, parse_load,
    parse_phi_incoming_values, parse_type, parse_typed_value, parse_value, parse_vector_i32_values,
    split_top_level, strip_call_prefix, LlCall, LlLoad,
};

pub(in crate::native) fn resolve_identity_ptr_bitcast(line: &str) -> Option<(String, String)> {
    parse_identity_ptr_bitcast(strip_comment(line).trim())
}

pub(in crate::native) fn resolve_phi_incoming_values(
    line: &str,
    opcode: &str,
) -> Option<Vec<LlValue>> {
    if opcode != "phi" {
        return None;
    }
    let cleaned = strip_comment(line).trim();
    let rhs = cleaned.split_once(" = ")?.1.trim();
    let rest = rhs.strip_prefix("phi ")?;
    parse_phi_incoming_values(rest).ok()
}

pub(in crate::native) fn resolve_select_arms(
    line: &str,
    opcode: &str,
) -> Option<(TypedValue, TypedValue)> {
    if opcode != "select" {
        return None;
    }
    let cleaned = strip_comment(line).trim();
    let rhs = cleaned.split_once(" = ")?.1.trim();
    let rest = rhs.strip_prefix("select ")?;
    let parts = split_top_level(rest, ',');
    if parts.len() != 3 {
        return None;
    }
    Some((
        parse_typed_value(parts[1]).ok()?,
        parse_typed_value(parts[2]).ok()?,
    ))
}

pub(in crate::native) fn resolve_load_inst(line: &str, opcode: &str) -> Option<LlLoad> {
    if opcode != "load" {
        return None;
    }
    let cleaned = strip_comment(line).trim();
    let rhs = cleaned.split_once(" = ")?.1.trim();
    let rest = rhs.strip_prefix("load ")?;
    parse_load(rest).ok()
}

pub(in crate::native) fn resolve_store(
    line: &str,
    opcode: &str,
) -> Option<(TypedValue, TypedValue)> {
    if opcode != "store" {
        return None;
    }
    let cleaned = strip_comment(line).trim();
    let rest = cleaned.strip_prefix("store ")?;
    let parts = split_top_level(rest, ',');
    if parts.len() < 2 {
        return None;
    }
    Some((
        parse_typed_value(parts[0]).ok()?,
        parse_typed_value(parts[1]).ok()?,
    ))
}

pub(in crate::native) fn resolve_alias_call(line: &str) -> Option<LlCall> {
    let cleaned = strip_comment(line).trim();
    let call_text = cleaned
        .split_once(" = ")
        .and_then(|(_, rhs)| strip_call_prefix(rhs.trim()))
        .or_else(|| strip_call_prefix(cleaned))?;
    parse_call(call_text).ok()
}

pub(in crate::native) fn resolve_emit_scan_call(line: &str) -> Option<Result<LlCall, String>> {
    let cleaned = strip_comment(line).trim();
    if crate::native::parse::is_ignored_call_line(cleaned) {
        return None;
    }
    let call_text = if let Some((_, rhs)) = cleaned.split_once(" = ") {
        strip_call_prefix(rhs.trim())
    } else {
        strip_call_prefix(cleaned)
    }?;
    if !call_text.contains('@') {
        return None;
    }
    Some(parse_call(call_text))
}

pub(in crate::native) fn resolve_gep(line: &str) -> Option<LlGep> {
    let rhs = rhs_of(line);
    let opcode = rhs.split_whitespace().next()?;
    if opcode != "getelementptr" {
        return None;
    }
    let after_opcode = rhs[opcode.len()..].trim_start();
    crate::native::parse::parse_gep(after_opcode).ok()
}

pub(in crate::native) fn resolve_call(line: &str) -> Option<LlCall> {
    let rhs = rhs_of(line);
    let opcode = rhs.split_whitespace().next()?;
    let after_opcode = rhs[opcode.len()..].trim_start();
    let after_call = match opcode {
        "call" => after_opcode,
        "tail" | "musttail" | "notail" => after_opcode.strip_prefix("call ")?.trim_start(),
        _ => return None,
    };
    parse_call(after_call).ok()
}

pub(in crate::native) fn resolve_aggregate_indices(line: &str, opcode: &str) -> Option<Vec<u32>> {
    let skip = match opcode {
        "extractvalue" => 1,
        "insertvalue" => 2,
        _ => return None,
    };
    let rest = line.split_once(" = ")?.1.trim();
    let after_opcode = rest[opcode.len()..].trim_start();
    let parts = split_top_level(after_opcode, ',');
    if parts.len() <= skip {
        return None;
    }
    parts[skip..]
        .iter()
        .map(|idx| crate::native::lex::parse_u32(idx.trim()).ok())
        .collect()
}

pub(in crate::native) fn resolve_shuffle_mask(line: &str) -> Option<(u32, Vec<u32>)> {
    let rest = line.split_once(" = ")?.1.trim();
    let opcode = rest.split_whitespace().next().unwrap_or("");
    let after_opcode = rest[opcode.len()..].trim_start();
    let parts = split_top_level(after_opcode, ',');
    if parts.len() != 3 {
        return None;
    }
    let mask = parts[2];
    let TypedValue { ty: mask_ty, .. } = parse_constant_vector(mask).ok()?;
    let LlType::Vector(mask_elem, lanes) = mask_ty else {
        return None;
    };
    if *mask_elem != LlType::Int(32) {
        return None;
    }
    let indexes = parse_vector_i32_values(mask).ok()?;
    Some((lanes, indexes))
}

pub(in crate::native) fn resolve_bitcast(line: &str, opcode: &str) -> Option<(TypedValue, String)> {
    if opcode != "bitcast" {
        return None;
    }
    let cleaned = crate::native::lex::strip_comment(line).trim();
    let rhs = cleaned.split_once(" = ")?.1.trim();
    let after_opcode = rhs[opcode.len()..].trim_start();
    let (src_text, dst_text) = after_opcode.split_once(" to ")?;
    let src = parse_typed_value(src_text).ok()?;
    Some((src, dst_text.to_string()))
}

pub(in crate::native) fn resolve_icmp_rest(line: &str, opcode: &str) -> Option<String> {
    if opcode != "icmp" {
        return None;
    }
    let cleaned = crate::native::lex::strip_comment(line).trim();
    let rhs = cleaned.split_once(" = ")?.1.trim();
    let after_opcode = rhs[opcode.len()..].trim_start();
    Some(after_opcode.to_string())
}

pub(in crate::native) fn resolve_operands(line: &str) -> Vec<TirOperand> {
    let rhs = rhs_of(line);
    let mut words = rhs.split_whitespace();
    let Some(opcode) = words.next() else {
        return Vec::new();
    };
    let after_opcode = rhs[opcode.len()..].trim_start();
    match opcode {
        "add" | "sub" | "mul" | "udiv" | "sdiv" | "urem" | "srem" | "and" | "or" | "xor"
        | "shl" | "lshr" | "ashr" | "fadd" | "fsub" | "fmul" | "fdiv" | "frem" => {
            two_operands_shared_type(skip_flag_tokens(after_opcode))
        }
        "icmp" | "fcmp" => {
            let rest = skip_flag_tokens(after_opcode);
            let rest = rest
                .split_once(char::is_whitespace)
                .map(|(_, r)| r)
                .unwrap_or("");
            two_operands_shared_type(rest.trim())
        }
        "select" => split_top_level(after_opcode, ',')
            .iter()
            .filter(|c| !c.trim_start().starts_with('!'))
            .map(|c| operand_from_chunk(c.trim()))
            .collect(),
        "trunc" | "zext" | "sext" | "fptrunc" | "fpext" | "fptoui" | "fptosi" | "uitofp"
        | "sitofp" | "ptrtoint" | "inttoptr" | "bitcast" | "addrspacecast" => {
            let value = after_opcode
                .split(" to ")
                .next()
                .unwrap_or(after_opcode)
                .trim();
            vec![operand_from_chunk(value)]
        }
        "freeze" | "fneg" => vec![operand_from_chunk(skip_flag_tokens(after_opcode))],
        "load" => {
            let fields = split_top_level(skip_flag_tokens(after_opcode), ',');
            match fields.get(1) {
                Some(ptr) => vec![operand_from_chunk(ptr.trim())],
                None => vec![TirOperand::Unresolved],
            }
        }
        "store" => {
            let fields = split_top_level(skip_flag_tokens(after_opcode), ',');
            match (fields.first(), fields.get(1)) {
                (Some(v), Some(p)) => {
                    vec![operand_from_chunk(v.trim()), operand_from_chunk(p.trim())]
                }
                _ => vec![TirOperand::Unresolved],
            }
        }
        "phi" => resolve_phi_operands(after_opcode),
        "extractelement" | "insertelement" | "shufflevector" => split_top_level(after_opcode, ',')
            .iter()
            .map(|c| operand_from_chunk(c.trim()))
            .collect(),
        "extractvalue" => match split_top_level(after_opcode, ',').first() {
            Some(agg) => vec![operand_from_chunk(agg.trim())],
            None => vec![TirOperand::Unresolved],
        },
        "insertvalue" => {
            let chunks = split_top_level(after_opcode, ',');
            match (chunks.first(), chunks.get(1)) {
                (Some(agg), Some(elt)) => {
                    vec![
                        operand_from_chunk(agg.trim()),
                        operand_from_chunk(elt.trim()),
                    ]
                }
                _ => vec![TirOperand::Unresolved],
            }
        }
        "getelementptr" => {
            let chunks = split_top_level(after_opcode, ',');
            if chunks.len() < 2 {
                return vec![TirOperand::Unresolved];
            }
            chunks[1..]
                .iter()
                .map(|c| operand_from_chunk(c.trim()))
                .collect()
        }
        "call" => resolve_call_operands(after_opcode),
        "tail" | "musttail" | "notail" => match after_opcode.strip_prefix("call ") {
            Some(rest) => resolve_call_operands(rest.trim_start()),
            None => vec![TirOperand::Unresolved],
        },
        "alloca" => match split_top_level(after_opcode, ',').get(1) {
            Some(field) if !field.trim_start().starts_with("align") => {
                vec![operand_from_chunk(field.trim())]
            }
            _ => Vec::new(),
        },
        _ => {
            if after_opcode.is_empty() {
                Vec::new()
            } else {
                vec![TirOperand::Unresolved]
            }
        }
    }
}

pub(in crate::native) fn resolve_call_operands(after_call: &str) -> Vec<TirOperand> {
    let Some(at) = after_call.find('@') else {
        return vec![TirOperand::Unresolved];
    };
    let Some(open) = after_call[at..].find('(').map(|p| p + at) else {
        return vec![TirOperand::Unresolved];
    };
    let Some(close) = matching_paren(after_call, open) else {
        return vec![TirOperand::Unresolved];
    };
    let args_text = after_call[open + 1..close].trim();
    if args_text.is_empty() {
        return Vec::new();
    }
    split_top_level(args_text, ',')
        .iter()
        .map(|c| operand_from_chunk(c.trim()))
        .collect()
}

pub(in crate::native) fn skip_flag_tokens(s: &str) -> &str {
    let mut s = s.trim_start();
    loop {
        let Some((head, rest)) = s.split_once(char::is_whitespace) else {
            return s;
        };
        if OPERAND_FLAG_TOKENS.contains(&head) {
            s = rest.trim_start();
        } else {
            return s;
        }
    }
}

pub(in crate::native) fn two_operands_shared_type(region: &str) -> Vec<TirOperand> {
    let chunks: Vec<&str> = split_top_level(region, ',')
        .into_iter()
        .filter(|c| !c.trim_start().starts_with('!'))
        .collect();
    if chunks.len() != 2 {
        return vec![TirOperand::Unresolved];
    }
    let first = chunks[0].trim();
    let Ok(tv) = parse_typed_value(first) else {
        return vec![TirOperand::Unresolved, TirOperand::Unresolved];
    };
    let ty = tv.ty.clone();
    vec![
        operand_from_typed_value(&tv),
        operand_from_bare(chunks[1].trim(), ty),
    ]
}

pub(in crate::native) fn resolve_phi_operands(after_opcode: &str) -> Vec<TirOperand> {
    let Some(open) = after_opcode.find('[') else {
        return vec![TirOperand::Unresolved];
    };
    let Ok(ty) = parse_type(after_opcode[..open].trim()) else {
        return vec![TirOperand::Unresolved];
    };
    split_top_level(&after_opcode[open..], ',')
        .iter()
        .filter_map(|pair| {
            let inner = pair.trim().trim_start_matches('[').trim_end_matches(']');
            split_top_level(inner, ',')
                .first()
                .map(|val| operand_from_bare(val.trim(), ty.clone()))
        })
        .collect()
}

pub(in crate::native) fn operand_from_chunk(chunk: &str) -> TirOperand {
    match parse_typed_value(chunk) {
        Ok(tv) => operand_from_typed_value(&tv),
        Err(_) => TirOperand::Unresolved,
    }
}

pub(in crate::native) fn operand_from_bare(token: &str, ty: LlType) -> TirOperand {
    if token.starts_with('%') {
        return TirOperand::Value {
            name: token.to_string(),
            ty,
        };
    }
    if let Ok(tv) = parse_typed_value(token) {
        return operand_from_typed_value(&tv);
    }
    match parse_value(token) {
        Ok(value) => TirOperand::Const { value, ty },
        Err(_) => TirOperand::Unresolved,
    }
}

pub(in crate::native) fn operand_from_typed_value(tv: &TypedValue) -> TirOperand {
    match &tv.value {
        LlValue::Local(name) => TirOperand::Value {
            name: name.clone(),
            ty: tv.ty.clone(),
        },
        value => TirOperand::Const {
            value: value.clone(),
            ty: tv.ty.clone(),
        },
    }
}

pub(in crate::native) fn instruction_uses(line: &str, result: Option<&str>) -> Vec<String> {
    let rhs = line.split_once('=').map(|(_, r)| r.trim()).unwrap_or(line);
    let mut uses = Vec::new();
    if rhs.starts_with("phi ") {
        if let Some(open) = rhs.find('[') {
            for pair in split_top_level(&rhs[open..], ',') {
                let inner = pair.trim().trim_start_matches('[').trim_end_matches(']');
                if let Some(val) = split_top_level(inner, ',').first() {
                    collect_value_names(val, &mut uses);
                }
            }
        }
        return dedup_keep_order(uses, result);
    }
    collect_value_names(rhs, &mut uses);
    dedup_keep_order(uses, result)
}

pub(in crate::native) fn collect_value_names(s: &str, out: &mut Vec<String>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            if i > start + 1 {
                out.push(s[start..i].to_string());
            }
        } else {
            i += 1;
        }
    }
}

pub(in crate::native) fn dedup_keep_order(names: Vec<String>, result: Option<&str>) -> Vec<String> {
    let mut seen = crate::fxhash::HashSet::default();
    names
        .into_iter()
        .filter(|n| Some(n.as_str()) != result && seen.insert(n.clone()))
        .collect()
}

pub(in crate::native) fn result_name(line: &str) -> Option<String> {
    let (lhs, _) = line.split_once('=')?;
    let lhs = lhs.trim();
    if lhs.starts_with('%') {
        Some(lhs.to_string())
    } else {
        None
    }
}

pub(in crate::native) fn resolve_result(
    line: &str,
    named_types: &HashMap<String, LlType>,
) -> Option<(String, Option<LlType>)> {
    let name = result_name(line)?;
    let (_, rhs) = line.split_once('=')?;
    let rhs = rhs.trim();
    let opcode = rhs.split_whitespace().next()?;

    let ty = match opcode {
        "add" | "sub" | "mul" | "and" | "or" | "xor" | "shl" | "lshr" | "ashr" | "udiv"
        | "sdiv" | "urem" | "srem" | "fadd" | "fsub" | "fmul" | "fdiv" | "frem" => {
            first_type_after_op(rhs)
        }
        "load" => {
            let after = rhs.strip_prefix("load ")?;
            let ty_text = split_top_level(after, ',').into_iter().next()?;
            parse_type(strip_keywords(ty_text).trim()).ok()
        }
        "bitcast" | "trunc" | "zext" | "sext" | "fptrunc" | "fpext" | "sitofp" | "uitofp"
        | "fptosi" | "fptoui" | "ptrtoint" | "inttoptr" | "addrspacecast" => {
            let (_, dst) = rhs.rsplit_once(" to ")?;
            parse_type(dst.trim()).ok()
        }
        "icmp" | "fcmp" => match first_type_after(rhs, 1) {
            Some(LlType::Vector(_, n)) => Some(LlType::Vector(Box::new(LlType::Bool), n)),
            Some(_) => Some(LlType::Bool),
            None => None,
        },
        "select" => {
            let parts = split_top_level(rhs.strip_prefix("select ")?, ',');
            parts.get(1).and_then(|arm| first_type_token(arm.trim()))
        }
        "phi" => first_type_after_op(rhs),
        "call" | "tail" => {
            let after = rhs.strip_prefix("tail ").unwrap_or(rhs);
            let after = after.strip_prefix("call ")?;
            first_type_after(after, 0)
        }
        "extractelement" => match first_type_after_op(rhs) {
            Some(LlType::Vector(elem, _)) => Some(*elem),
            other => other,
        },
        "extractvalue" => {
            let parts = split_top_level(rhs, ',');
            let agg = first_type_after_op(rhs)?;
            let indices: Vec<Option<usize>> = parts[1..]
                .iter()
                .filter_map(|p| p.split_whitespace().last().map(|t| t.parse().ok()))
                .collect();
            extract_aggregate_member(agg, &indices, named_types)
        }
        "insertelement" | "insertvalue" => first_type_after_op(rhs),
        "fneg" | "freeze" => first_type_after_op(rhs),
        "alloca" => Some(LlType::Ptr(operand_addrspace(rhs))),
        "shufflevector" => {
            let parts = split_top_level(rhs, ',');
            let elem = match first_type_after_op(rhs) {
                Some(LlType::Vector(e, _)) => *e,
                _ => return Some((name, None)),
            };
            let mask_len = parts.last().and_then(|m| match first_type_token(m.trim()) {
                Some(LlType::Vector(_, n)) => Some(n),
                _ => None,
            });
            mask_len.map(|n| LlType::Vector(Box::new(elem), n))
        }
        "getelementptr" => {
            let parts = split_top_level(rhs, ',');
            parts
                .get(1)
                .map(|base| LlType::Ptr(operand_addrspace(base)))
        }
        _ => None,
    };
    Some((name, ty))
}

pub(in crate::native) fn first_type_after_op(rhs: &str) -> Option<LlType> {
    first_type_after(rhs, 1)
}

pub(in crate::native) fn first_type_after(rhs: &str, start: usize) -> Option<LlType> {
    let toks: Vec<&str> = rhs.split_whitespace().collect();
    for begin in start..toks.len() {
        for end in (begin + 1)..=toks.len() {
            let candidate = toks[begin..end].join(" ");
            let candidate = candidate.trim_end_matches(',');
            if let Ok(ty) = parse_type(candidate) {
                return Some(ty);
            }
        }
    }
    None
}

pub(in crate::native) fn first_type_token(s: &str) -> Option<LlType> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    for end in 1..=toks.len() {
        let candidate = toks[..end].join(" ");
        if let Ok(ty) = parse_type(candidate.trim_end_matches(',')) {
            return Some(ty);
        }
    }
    None
}

pub(in crate::native) fn extract_aggregate_member(
    mut ty: LlType,
    indices: &[Option<usize>],
    named_types: &HashMap<String, LlType>,
) -> Option<LlType> {
    for &idx in indices {
        if let LlType::Named(name) = &ty {
            ty = named_types.get(name).cloned()?;
        }
        ty = match ty {
            LlType::Struct(members) => members.into_iter().nth(idx?)?,
            LlType::Array(elem, _) => *elem,
            LlType::Vector(elem, _) => *elem,
            _ => return None,
        };
    }
    Some(ty)
}

pub(in crate::native) fn operand_addrspace(operand: &str) -> u32 {
    operand
        .find("addrspace(")
        .and_then(|p| {
            let after = &operand[p + "addrspace(".len()..];
            after.find(')').and_then(|e| after[..e].trim().parse().ok())
        })
        .unwrap_or(0)
}

pub(in crate::native) fn is_flag_keyword(tok: &str) -> bool {
    matches!(
        tok,
        "nuw"
            | "nsw"
            | "exact"
            | "fast"
            | "nnan"
            | "ninf"
            | "nsz"
            | "arcp"
            | "contract"
            | "afn"
            | "reassoc"
            | "volatile"
    )
}

pub(in crate::native) fn strip_keywords(s: &str) -> String {
    s.split_whitespace()
        .filter(|t| !is_flag_keyword(t))
        .collect::<Vec<_>>()
        .join(" ")
}
