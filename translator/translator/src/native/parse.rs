use super::ir::{LlDeclaration, LlFunction, LlGep, LlGlobal, LlType, LlValue, TypedValue};
pub(super) use super::lex::{
    matching_angle, matching_paren, parse_bfloat_literal, parse_float32_hex_literal,
    parse_float_literal, parse_float_special_literal, parse_half_literal, parse_hex_literal,
    parse_i64_literal, parse_u32, parse_u64_literal, split_instruction_operands, split_top_level, split_top_level_whitespace,
    strip_comment,
};
use crate::spirv_module::Operand;
use spirv::Op;
#[cfg(test)]
use std::ops::Range;

#[derive(Clone, Debug)]
pub(super) struct LlCall {
    pub(super) ret: LlType,
    pub(super) callee: String,
    pub(super) args: Vec<TypedValue>,
    pub(super) arg_aligns: Vec<Option<u64>>,
}

#[derive(Clone, Debug)]
pub(super) struct LlLoad {
    pub(super) result_ty: LlType,
    pub(super) ptr: TypedValue,
    pub(super) align: Option<u64>,
}

#[derive(Clone, Debug)]
pub(super) struct LlSwitch {
    pub(super) selector: TypedValue,
    pub(super) default_label: String,
    pub(super) cases: Vec<(LlValue, String)>,
}

#[cfg(test)]
pub(super) fn parse_function(
    lines: &[&str],
    start: usize,
) -> Result<(LlFunction, Range<usize>, usize), String> {
    let head = strip_comment(lines[start]).trim();
    let function = parse_function_header(head)?;
    let name = function.name.clone();
    let mut i = start + 1;
    while i < lines.len() {
        let line = strip_comment(lines[i]).trim();
        if line == "}" {
            return Ok((function, start + 1..i, i + 1));
        }
        i += 1;
    }
    Err(format!("native emitter: unterminated function {name}"))
}

pub(super) fn parse_function_header(head: &str) -> Result<LlFunction, String> {
    let at = head
        .find('@')
        .ok_or_else(|| format!("native emitter: define without function name: {head}"))?;
    let (name, open) = parse_global_symbol_with_params(head, at, "define")?;
    let close = matching_paren(head, open)
        .ok_or_else(|| format!("native emitter: unmatched params in define: {head}"))?;
    let ret = parse_return_type(&head["define".len()..at])?;
    let (params, byval_param_pointees) = parse_params(&head[open + 1..close])?;
    let is_static_initializer =
        crate::air_static_init::tail_declares_static_init_section(&head[close + 1..]);
    Ok(LlFunction {
        name,
        ret,
        params,
        byval_param_pointees,
        is_static_initializer,
        blocks: Vec::new(),
        loop_controls: crate::fxhash::HashMap::default(),
    })
}

pub(super) fn collect_switch(lines: &[&str], start: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut i = start;
    while i < lines.len() {
        let part = strip_comment(lines[i]).trim();
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(part);
        if part.contains(']') {
            return Ok((out, i + 1));
        }
        i += 1;
    }
    Err(format!(
        "native emitter: unterminated switch starting at `{}`",
        strip_comment(lines[start]).trim()
    ))
}

pub(super) fn parse_declaration(line: &str) -> Result<LlDeclaration, String> {
    let head = strip_comment(line).trim();
    let at = head
        .find('@')
        .ok_or_else(|| format!("native emitter: declare without function name: {head}"))?;
    let (name, open) = parse_global_symbol_with_params(head, at, "declare")?;
    let close = matching_paren(head, open)
        .ok_or_else(|| format!("native emitter: unmatched params in declare: {head}"))?;
    let ret = parse_return_type(&head["declare".len()..at])?;
    let params = parse_decl_params(&head[open + 1..close])?;
    Ok(LlDeclaration { name, ret, params })
}

fn parse_global_symbol_with_params(
    s: &str,
    at: usize,
    context: &str,
) -> Result<(String, usize), String> {
    let after_at = at + 1;
    if s[after_at..].starts_with('"') {
        let (name, end) = parse_quoted_global_symbol(s, after_at)?;
        let open = s[end..]
            .find('(')
            .map(|p| p + end)
            .ok_or_else(|| format!("native emitter: {context} without params: {s}"))?;
        return Ok((name, open));
    }
    let open = s[at..]
        .find('(')
        .map(|p| p + at)
        .ok_or_else(|| format!("native emitter: {context} without params: {s}"))?;
    Ok((s[after_at..open].to_string(), open))
}

fn parse_quoted_global_symbol(s: &str, quote: usize) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut pos = quote + 1;
    while pos < s.len() {
        let ch = s[pos..]
            .chars()
            .next()
            .ok_or_else(|| format!("native emitter: malformed quoted symbol: {s}"))?;
        pos += ch.len_utf8();
        match ch {
            '"' => return Ok((out, pos)),
            '\\' => {
                let hi = s[pos..].chars().next();
                let lo = hi.and_then(|hi| {
                    let next = pos + hi.len_utf8();
                    s[next..].chars().next()
                });
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() {
                        pos += hi.len_utf8() + lo.len_utf8();
                        let byte =
                            ((hi.to_digit(16).unwrap() << 4) | lo.to_digit(16).unwrap()) as u8;
                        out.push(byte as char);
                        continue;
                    }
                }
                let escaped = s[pos..].chars().next().ok_or_else(|| {
                    format!("native emitter: unterminated quoted symbol escape: {s}")
                })?;
                pos += escaped.len_utf8();
                out.push(escaped);
            }
            _ => out.push(ch),
        }
    }
    Err(format!("native emitter: unterminated quoted symbol: {s}"))
}

pub(super) fn parse_global(line: &str) -> Result<LlGlobal, String> {
    let (name, rest) = line
        .split_once(" = ")
        .ok_or_else(|| format!("native emitter: malformed global: {line}"))?;
    let addrspace = parse_addrspace(rest)?;
    let typed_init = rest
        .split_once(" constant ")
        .map(|(_, typed_init)| typed_init)
        .or_else(|| {
            rest.split_once(" global ")
                .map(|(_, typed_init)| typed_init)
        })
        .ok_or_else(|| format!("native emitter: global is not a constant/global: {line}"))?;
    let typed_init = split_top_level(typed_init.trim(), ',')
        .into_iter()
        .next()
        .ok_or_else(|| format!("native emitter: global has no initializer: {line}"))?;
    let initializer = parse_typed_value(typed_init.trim())?;
    Ok(LlGlobal {
        name: name.trim().to_string(),
        addrspace,
        ty: initializer.ty.clone(),
        initializer: Some(initializer),
    })
}

fn parse_addrspace(s: &str) -> Result<u32, String> {
    let Some(start) = s.find("addrspace(") else {
        return Ok(0);
    };
    let after = &s[start + "addrspace(".len()..];
    let Some(end) = after.find(')') else {
        return Err(format!(
            "native emitter: malformed addrspace in global: {s}"
        ));
    };
    after[..end]
        .parse::<u32>()
        .map_err(|e| format!("native emitter: bad global addrspace in `{s}`: {e}"))
}

pub(super) fn parse_decl_params(s: &str) -> Result<Vec<LlType>, String> {
    if s.trim().is_empty() {
        return Ok(vec![]);
    }
    split_top_level(s, ',')
        .into_iter()
        .map(parse_type_prefix)
        .collect()
}

pub(crate) fn parse_return_type(prefix: &str) -> Result<LlType, String> {
    let toks = split_top_level_whitespace(prefix);
    for i in 0..toks.len() {
        let candidate = toks[i..].join(" ");
        if let Ok(ty) = parse_type(&candidate) {
            return Ok(ty);
        }
    }
    Err(format!(
        "native emitter: could not parse function return type from `{}`",
        prefix.trim()
    ))
}

pub(super) fn parse_params(
    s: &str,
) -> Result<(Vec<(String, LlType)>, Vec<Option<LlType>>), String> {
    let mut out = Vec::new();
    let mut byval_pointees = Vec::new();
    if s.trim().is_empty() {
        return Ok((out, byval_pointees));
    }
    for raw in split_top_level(s, ',') {
        let raw = raw.trim();
        let pct = raw
            .rfind('%')
            .ok_or_else(|| format!("native emitter: parameter has no SSA name: {raw}"))?;
        let name = raw[pct..]
            .split_whitespace()
            .next()
            .ok_or_else(|| format!("native emitter: malformed parameter name: {raw}"))?
            .to_string();
        let ty = parse_type_prefix(raw[..pct].trim())?;
        let byval_pointee = raw.find("byval(").map(|byval| {
            let open = byval + "byval".len();
            let close = matching_paren(raw, open)
                .ok_or_else(|| format!("native emitter: unmatched byval type: {raw}"))?;
            parse_type(raw[open + 1..close].trim())
        });
        out.push((name, ty));
        byval_pointees.push(byval_pointee.transpose()?);
    }
    Ok((out, byval_pointees))
}

pub(crate) fn parse_type_prefix(s: &str) -> Result<LlType, String> {
    let toks = split_top_level_whitespace(s);
    for i in (1..=toks.len()).rev() {
        let candidate = toks[..i].join(" ");
        if let Ok(ty) = parse_type(&candidate) {
            return Ok(ty);
        }
    }
    Err(format!(
        "native emitter: could not parse type prefix from `{s}`"
    ))
}

pub(super) fn parse_typed_value(s: &str) -> Result<TypedValue, String> {
    let s = s.trim();
    let toks = split_top_level_whitespace(s);
    let mut last_value_err = None;
    for value_start in (1..toks.len()).rev() {
        let value_text = toks[value_start..].join(" ");
        let value = match parse_value(value_text.trim()) {
            Ok(value) => value,
            Err(err) => {
                last_value_err = Some(err);
                continue;
            }
        };
        for ty_start in 0..value_start {
            for ty_end in ((ty_start + 1)..=value_start).rev() {
                let ty_text = toks[ty_start..ty_end].join(" ");
                if let Ok(ty) = parse_type(&ty_text) {
                    return Ok(TypedValue { ty, value });
                }
            }
        }
    }
    if let Some(err) = last_value_err {
        return Err(format!(
            "native emitter: malformed typed value `{s}` ({err})"
        ));
    }
    Err(format!("native emitter: malformed typed value `{s}`"))
}

pub(super) fn parse_value(s: &str) -> Result<LlValue, String> {
    let s = s.trim();
    if s == "undef" || s == "poison" {
        Ok(LlValue::Undef)
    } else if s == "zeroinitializer" || s == "null" {
        Ok(LlValue::Zero)
    } else if s == "true" {
        Ok(LlValue::Bool(true))
    } else if s == "false" {
        Ok(LlValue::Bool(false))
    } else if s.starts_with('<') && s.ends_with('>') {
        let inner = s[1..s.len() - 1].trim();
        if inner.starts_with('{') && inner.ends_with('}') {
            let fields = split_top_level(&inner[1..inner.len() - 1], ',')
                .into_iter()
                .map(parse_typed_value)
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(LlValue::Struct(fields));
        }
        let lanes = split_top_level(inner, ',')
            .into_iter()
            .map(parse_typed_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LlValue::Vector(lanes))
    } else if s.starts_with('[') && s.ends_with(']') {
        let elems = split_top_level(&s[1..s.len() - 1], ',')
            .into_iter()
            .map(parse_typed_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LlValue::Array(elems))
    } else if s.starts_with('{') && s.ends_with('}') {
        let fields = split_top_level(&s[1..s.len() - 1], ',')
            .into_iter()
            .map(parse_typed_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(LlValue::Struct(fields))
    } else if s.starts_with("c\"") && s.ends_with('"') {
        parse_byte_string_literal(s).map(|bytes| {
            LlValue::Array(
                bytes
                    .into_iter()
                    .map(|byte| TypedValue {
                        ty: LlType::Int(8),
                        value: LlValue::Int(byte as u64),
                    })
                    .collect(),
            )
        })
    } else if let Some(inner) = s.strip_prefix("splat (").and_then(|s| s.strip_suffix(')')) {
        Ok(LlValue::Splat(Box::new(parse_typed_value(inner)?)))
    } else if let Some(rest) = s.strip_prefix("getelementptr ") {
        Ok(LlValue::Gep(Box::new(parse_gep(rest)?)))
    } else if let Some(inner) = s
        .strip_prefix("inttoptr (")
        .and_then(|value| value.strip_suffix(')'))
    {
        let (source, destination) = inner.rsplit_once(" to ").ok_or_else(|| {
            format!("native emitter: malformed inttoptr constant expression `{s}`")
        })?;
        Ok(LlValue::IntToPtr {
            source: Box::new(parse_typed_value(source)?),
            destination: parse_type(destination.trim())?,
        })
    } else if let Some(inner) = s
        .strip_prefix("bitcast (")
        .and_then(|value| value.strip_suffix(')'))
    {
        let (source, destination) = inner.rsplit_once(" to ").ok_or_else(|| {
            format!("native emitter: malformed bitcast constant expression `{s}`")
        })?;
        let source = parse_typed_value(source)?;
        let destination = parse_type(destination.trim())?;
        if !matches!(source.ty, LlType::Ptr(_)) || !matches!(destination, LlType::Ptr(_)) {
            return Err(format!(
                "native emitter: unsupported non-pointer bitcast constant expression `{s}`"
            ));
        }
        Ok(source.value)
    } else if s.starts_with('%') {
        Ok(LlValue::Local(s.to_string()))
    } else if s.starts_with('@') {
        Ok(LlValue::Global(s.to_string()))
    } else if let Some(bits) = parse_half_literal(s) {
        Ok(LlValue::HalfBits(bits))
    } else if let Some(bits) = parse_bfloat_literal(s) {
        Ok(LlValue::BFloatBits(bits))
    } else if let Some(bits) = parse_float32_hex_literal(s) {
        Ok(LlValue::Float32Bits(bits))
    } else if let Some(bits) = parse_hex_literal(s) {
        Ok(LlValue::Hex(bits))
    } else if let Ok(v) = parse_i64_literal(s) {
        Ok(LlValue::SignedInt(v))
    } else if let Ok(v) = parse_u64_literal(s) {
        Ok(LlValue::Int(v))
    } else if let Some(bits) = parse_float_special_literal(s) {
        Ok(LlValue::Float32Bits(bits))
    } else if let Ok(v) = parse_float_literal(s) {
        Ok(LlValue::Float(v))
    } else {
        Err(format!("native emitter: unsupported value `{s}`"))
    }
}

fn parse_byte_string_literal(s: &str) -> Result<Vec<u8>, String> {
    let body = s
        .strip_prefix("c\"")
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| format!("native emitter: malformed byte string `{s}`"))?;
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        if i + 1 >= bytes.len() {
            return Err(format!("native emitter: trailing byte string escape `{s}`"));
        }
        if i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        let escaped = match bytes[i + 1] {
            b'\\' => b'\\',
            b'"' => b'"',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            other => {
                return Err(format!(
                    "native emitter: unsupported byte string escape `\\{}` in `{s}`",
                    other as char
                ));
            }
        };
        out.push(escaped);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `fcmp ord` / `fcmp uno`: Some(true) when both operands must be numbers, Some(false) when either
/// may be NaN. Vulkan SPIR-V has no OpOrdered/OpUnordered (they need the Kernel capability), so the
/// emitter builds them from OpIsNan.
pub(super) fn fcmp_ordering_predicate(s: &str) -> Option<bool> {
    split_top_level_whitespace(s)
        .into_iter()
        .find_map(|tok| match tok {
            "ord" => Some(true),
            "uno" => Some(false),
            _ => None,
        })
}

pub(super) fn fcmp_predicate(s: &str) -> Option<Op> {
    let pred = split_top_level_whitespace(s).into_iter().find(|tok| {
        matches!(
            *tok,
            "oeq"
                | "ogt"
                | "oge"
                | "olt"
                | "ole"
                | "one"
                | "ueq"
                | "ugt"
                | "uge"
                | "ult"
                | "ule"
                | "une"
        )
    })?;
    Some(match pred {
        "oeq" => Op::FOrdEqual,
        "ogt" => Op::FOrdGreaterThan,
        "oge" => Op::FOrdGreaterThanEqual,
        "olt" => Op::FOrdLessThan,
        "ole" => Op::FOrdLessThanEqual,
        "one" => Op::FOrdNotEqual,
        "ueq" => Op::FUnordEqual,
        "ugt" => Op::FUnordGreaterThan,
        "uge" => Op::FUnordGreaterThanEqual,
        "ult" => Op::FUnordLessThan,
        "ule" => Op::FUnordLessThanEqual,
        "une" => Op::FUnordNotEqual,
        _ => return None,
    })
}

pub(super) fn icmp_predicate(s: &str) -> Option<Op> {
    let pred = split_top_level_whitespace(s).into_iter().find(|tok| {
        matches!(
            *tok,
            "eq" | "ne" | "ugt" | "uge" | "ult" | "ule" | "sgt" | "sge" | "slt" | "sle"
        )
    })?;
    Some(match pred {
        "eq" => Op::IEqual,
        "ne" => Op::INotEqual,
        "ugt" => Op::UGreaterThan,
        "uge" => Op::UGreaterThanEqual,
        "ult" => Op::ULessThan,
        "ule" => Op::ULessThanEqual,
        "sgt" => Op::SGreaterThan,
        "sge" => Op::SGreaterThanEqual,
        "slt" => Op::SLessThan,
        "sle" => Op::SLessThanEqual,
        _ => return None,
    })
}

pub(super) fn int_compare_result_type(operand: &LlType) -> Result<LlType, String> {
    match operand {
        LlType::Int(_) => Ok(LlType::Bool),
        LlType::Vector(elem, lanes) if matches!(elem.as_ref(), LlType::Int(_)) => {
            Ok(LlType::Vector(Box::new(LlType::Bool), *lanes))
        }
        other => Err(format!(
            "native emitter: icmp needs integer scalar/vector operands, got {other:?}"
        )),
    }
}

pub(super) fn float_compare_result_type(operand: &LlType) -> Result<LlType, String> {
    match operand {
        LlType::Float | LlType::Half => Ok(LlType::Bool),
        LlType::Vector(elem, lanes) if matches!(elem.as_ref(), LlType::Float | LlType::Half) => {
            Ok(LlType::Vector(Box::new(LlType::Bool), *lanes))
        }
        other => Err(format!(
            "native emitter: fcmp needs float scalar/vector operands, got {other:?}"
        )),
    }
}

pub(super) fn is_ignored_intrinsic(name: &str) -> bool {
    name.starts_with("llvm.lifetime.")
        || name.starts_with("llvm.umax.")
        || name.starts_with("llvm.umin.")
        || name.starts_with("llvm.smax.")
        || name.starts_with("llvm.smin.")
        || name.starts_with("llvm.abs.")
        || name.starts_with("llvm.usub.sat.")
}

pub(super) fn is_ignored_call_line(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix("tail ").unwrap_or(line);
    if !line.starts_with("call ") {
        return false;
    }
    if line.contains("@llvm.lifetime.") {
        return true;
    }
    let Some(open) = line.find('(') else {
        return false;
    };
    let Some(close) = matching_paren(line, open) else {
        return false;
    };
    let args = line[open + 1..close].trim();
    !args.is_empty()
        && split_top_level(args, ',')
            .iter()
            .all(|arg| arg.trim().starts_with("metadata "))
}

pub(super) fn strip_call_prefix(s: &str) -> Option<&str> {
    let s = s.strip_prefix("tail ").unwrap_or(s);
    s.strip_prefix("call ")
}

fn indirect_call_diagnostic(s: &str) -> String {
    let fp = s
        .find('(')
        .map(|open| {
            s[..open]
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string()
        })
        .filter(|t| t.starts_with('%'))
        .unwrap_or_default();
    if fp.is_empty() {
        format!("native emitter: call without callee: {s}")
    } else {
        format!(
            "native emitter: unsupported indirect call through function pointer {fp} \
             (Metal visible function table); not expressible in Logical SPIR-V: {s}"
        )
    }
}

pub(super) fn parse_call(s: &str) -> Result<LlCall, String> {
    let at = match s.find('@') {
        Some(at) => at,
        None => return Err(indirect_call_diagnostic(s)),
    };
    let (callee, open) = parse_global_symbol_with_params(s, at, "call")?;
    let close = matching_paren(s, open)
        .ok_or_else(|| format!("native emitter: unmatched call parens: {s}"))?;
    let ret = parse_return_type(&s[..at])?;
    let args_text = &s[open + 1..close];
    let (args, arg_aligns) = if args_text.trim().is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let arg_texts = split_top_level(args_text, ',');
        let mut args = Vec::with_capacity(arg_texts.len());
        let mut arg_aligns = Vec::with_capacity(arg_texts.len());
        for arg_text in arg_texts {
            arg_aligns.push(parse_argument_align(arg_text));
            args.push(parse_typed_value(arg_text)?);
        }
        (args, arg_aligns)
    };
    Ok(LlCall {
        ret,
        callee,
        args,
        arg_aligns,
    })
}

fn parse_argument_align(s: &str) -> Option<u64> {
    let toks = split_top_level_whitespace(s);
    toks.windows(2).find_map(|window| {
        if window[0] == "align" {
            window[1].parse().ok()
        } else {
            None
        }
    })
}

pub(super) fn parse_gep(s: &str) -> Result<LlGep, String> {
    let inbounds = split_top_level_whitespace(s).contains(&"inbounds");
    let s = strip_wrapping_parens(strip_native_gep_flags(s));
    let parts = split_top_level(s, ',');
    if parts.len() < 3 {
        return Err(format!("native emitter: malformed getelementptr: {s}"));
    }
    let source_ty = parse_type(parts[0])?;
    let base = parse_typed_value(parts[1])?;
    let indices = parts[2..]
        .iter()
        .map(|idx| parse_typed_value(idx))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LlGep {
        inbounds,
        source_ty,
        base,
        indices,
    })
}

fn strip_wrapping_parens(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with('(') && matching_paren(s, 0) == Some(s.len() - 1) {
        return s[1..s.len() - 1].trim();
    }
    s
}

pub(super) fn strip_native_gep_flags(mut s: &str) -> &str {
    s = s.trim();
    loop {
        let mut matched = false;
        for flag in ["inbounds", "nuw", "nusw"] {
            if let Some(rest) = s.strip_prefix(flag) {
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    s = rest.trim_start();
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            return s;
        }
    }
}

pub(super) fn parse_load(s: &str) -> Result<LlLoad, String> {
    let parts = split_top_level(s, ',');
    if parts.len() < 2 {
        return Err(format!("native emitter: malformed load: {s}"));
    }
    Ok(LlLoad {
        result_ty: parse_type(parts[0])?,
        ptr: parse_typed_value(parts[1])?,
        align: parse_memory_alignment(&parts[2..])?,
    })
}

pub(super) fn parse_memory_alignment(parts: &[&str]) -> Result<Option<u64>, String> {
    for part in parts {
        let part = part.trim();
        let Some(value) = part.strip_prefix("align ") else {
            continue;
        };
        return value
            .trim()
            .parse()
            .map(Some)
            .map_err(|e| format!("native emitter: malformed memory alignment `{part}`: {e}"));
    }
    Ok(None)
}

pub(super) fn parse_phi(rest: &str) -> Result<(LlType, Vec<(LlValue, String)>), String> {
    let (phi_ty, incoming_text) = split_phi_type_and_incoming(rest)?;
    let incoming = split_instruction_operands(&incoming_text)
        .into_iter()
        .map(|incoming| {
            let incoming = incoming
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or_else(|| format!("native emitter: malformed phi incoming: {incoming}"))?;
            let parts = split_top_level(incoming, ',');
            if parts.len() != 2 {
                return Err(format!(
                    "native emitter: malformed phi incoming fields: {incoming}"
                ));
            }
            Ok((
                parse_phi_incoming_value(parts[0].trim())?,
                parts[1].trim().to_string(),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((phi_ty, incoming))
}

fn split_phi_type_and_incoming(rest: &str) -> Result<(LlType, String), String> {
    let tokens = split_top_level_whitespace(rest);
    for incoming_start in 1..tokens.len() {
        if !tokens[incoming_start].starts_with('[') {
            continue;
        }
        let type_text = tokens[..incoming_start].join(" ");
        if let Ok(ty) = parse_type(&type_text) {
            return Ok((ty, tokens[incoming_start..].join(" ")));
        }
    }
    Err(format!("native emitter: malformed phi: {rest}"))
}

pub(super) fn parse_switch(s: &str) -> Result<LlSwitch, String> {
    let s = s.trim();
    let body = s
        .strip_prefix("switch ")
        .ok_or_else(|| format!("native emitter: malformed switch: {s}"))?;
    let open = body
        .find('[')
        .ok_or_else(|| format!("native emitter: switch missing case list: {s}"))?;
    let close = body
        .rfind(']')
        .ok_or_else(|| format!("native emitter: switch missing case terminator: {s}"))?;
    if close < open {
        return Err(format!("native emitter: malformed switch case list: {s}"));
    }
    let head = body[..open].trim();
    let head_parts = split_top_level(head, ',');
    if head_parts.len() != 2 {
        return Err(format!("native emitter: malformed switch header: {s}"));
    }
    let selector = parse_typed_value(head_parts[0])?;
    let default_label = parse_label_operand(head_parts[1])
        .ok_or_else(|| format!("native emitter: malformed switch default: {s}"))?;
    let mut cases = Vec::new();
    let mut rest = body[open + 1..close].trim();
    while !rest.is_empty() {
        let (value_text, after_value) = rest
            .split_once(',')
            .ok_or_else(|| format!("native emitter: malformed switch case: {rest}"))?;
        let value = parse_typed_value(value_text)?;
        if value.ty != selector.ty {
            return Err(format!(
                "native emitter: switch case type {:?} does not match selector {:?}",
                value.ty, selector.ty
            ));
        }
        let after_label = after_value
            .trim()
            .strip_prefix("label ")
            .ok_or_else(|| format!("native emitter: malformed switch case label: {rest}"))?;
        let label_end = after_label
            .find(char::is_whitespace)
            .unwrap_or(after_label.len());
        let label = after_label[..label_end].to_string();
        rest = after_label[label_end..].trim();
        cases.push((value.value, label));
    }
    Ok(LlSwitch {
        selector,
        default_label,
        cases,
    })
}

pub(super) fn parse_label_operand(s: &str) -> Option<String> {
    let label = s.trim().strip_prefix("label ")?.trim();
    Some(label.split_whitespace().next()?.to_string())
}

pub(super) fn switch_literal_operand(
    value: &LlValue,
    selector_ty: &LlType,
) -> Result<Operand, String> {
    let LlType::Int(bits) = selector_ty else {
        return Err(format!(
            "native emitter: switch selector must be integer, got {selector_ty:?}"
        ));
    };
    if *bits == 0 || *bits > 64 {
        return Err(format!(
            "native emitter: switch selector width {bits} is not covered"
        ));
    }
    let max = if *bits == 64 {
        u64::MAX as u128
    } else {
        (1u128 << *bits) - 1
    };
    let raw = match value {
        LlValue::Int(v) | LlValue::Hex(v) => {
            let v = *v as u128;
            if v > max {
                return Err(format!(
                    "native emitter: switch case value {v} overflows i{bits}"
                ));
            }
            v
        }
        LlValue::SignedInt(v) if *v >= 0 => {
            let v = *v as u128;
            if v > max {
                return Err(format!(
                    "native emitter: switch case value {v} overflows i{bits}"
                ));
            }
            v
        }
        LlValue::SignedInt(v) => {
            let modulus = if *bits == 64 {
                1u128 << 64
            } else {
                1u128 << *bits
            };
            let min = -(1i128 << (*bits - 1));
            if (*v as i128) < min {
                return Err(format!(
                    "native emitter: switch case value {v} overflows i{bits}"
                ));
            }
            (modulus as i128 + *v as i128) as u128
        }
        other => {
            return Err(format!(
                "native emitter: switch case must be an integer literal, got {other:?}"
            ))
        }
    };
    if *bits <= 32 {
        Ok(Operand::LiteralBit32(raw as u32))
    } else {
        Ok(Operand::LiteralBit64(raw as u64))
    }
}

pub(super) fn parse_identity_ptr_bitcast(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    let eq = t.find(" = bitcast ")?;
    let res = t[..eq].trim().to_string();
    if !res.starts_with('%') {
        return None;
    }
    let rest = &t[eq + " = bitcast ".len()..];
    let to = rest.find(" to ")?;
    let src = rest[..to].trim();
    let dst = rest[to + " to ".len()..].trim();
    let base = src.rsplit(char::is_whitespace).next()?;
    if !base.starts_with('%') {
        return None;
    }
    let src_ty = src[..src.len() - base.len()].trim();
    let identical =
        src_ty == dst || matches!((parse_type(src_ty), parse_type(dst)), (Ok(a), Ok(b)) if a == b);
    if !identical {
        return None;
    }
    Some((res, base.to_string()))
}

pub(super) fn parse_phi_incoming_values(rest: &str) -> Result<Vec<LlValue>, String> {
    let (_, incoming_text) = split_phi_type_and_incoming(rest)?;
    split_instruction_operands(&incoming_text)
        .into_iter()
        .map(|incoming| {
            let incoming = incoming
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or_else(|| format!("native emitter: malformed phi incoming: {incoming}"))?;
            let parts = split_top_level(incoming, ',');
            if parts.len() != 2 {
                return Err(format!(
                    "native emitter: malformed phi incoming fields: {incoming}"
                ));
            }
            parse_phi_incoming_value(parts[0].trim())
        })
        .collect()
}

fn parse_phi_incoming_value(token: &str) -> Result<LlValue, String> {
    if let Ok(tv) = parse_typed_value(token) {
        return Ok(tv.value);
    }
    parse_value(token)
}

pub(super) fn parse_type(s: &str) -> Result<LlType, String> {
    let s = strip_leading_fast_math_flags(s.trim());
    if s == "void" {
        return Ok(LlType::Void);
    }
    if s == "metadata" {
        return Ok(LlType::Void);
    }
    if s == "float" {
        return Ok(LlType::Float);
    }
    if s == "half" {
        return Ok(LlType::Half);
    }
    if s == "bfloat" {
        return Ok(LlType::BFloat);
    }
    if let Some(bits) = s.strip_prefix('i') {
        if bits.chars().all(|c| c.is_ascii_digit()) {
            return Ok(LlType::Int(
                bits.parse()
                    .map_err(|e| format!("native emitter: bad int type {s}: {e}"))?,
            ));
        }
    }
    if let Some(name) = s.strip_prefix('%') {
        if let Some(inner) = name
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .filter(|inner| !inner.contains('"'))
        {
            return Ok(LlType::Named(format!("%\"{inner}\"")));
        }
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        {
            return Ok(LlType::Named(format!("%{name}")));
        }
    }
    if s == "ptr" {
        return Ok(LlType::Ptr(0));
    }
    if let Some(rest) = s.strip_prefix("ptr addrspace(") {
        let n = rest
            .strip_suffix(')')
            .ok_or_else(|| format!("native emitter: malformed pointer type `{s}`"))?;
        return Ok(LlType::Ptr(n.parse().map_err(|e| {
            format!("native emitter: bad addrspace in `{s}`: {e}")
        })?));
    }
    if s.starts_with('<') && s.ends_with('>') {
        let inner = s[1..s.len() - 1].trim();
        if inner.starts_with('{') && inner.ends_with('}') {
            return parse_struct_fields(&inner[1..inner.len() - 1]).map(LlType::Struct);
        }
        let (lanes, elem) = split_vector_type(inner)?;
        return Ok(LlType::Vector(Box::new(parse_type(elem)?), lanes));
    }
    if s.starts_with('{') && s.ends_with('}') {
        return parse_struct_fields(&s[1..s.len() - 1]).map(LlType::Struct);
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = s[1..s.len() - 1].trim();
        let (len, elem) = split_vector_type(inner)?;
        return Ok(LlType::Array(Box::new(parse_type(elem)?), len));
    }
    if let Some(pointee) = s.strip_suffix('*') {
        let (pointee, space) = split_trailing_addrspace(pointee.trim_end())?;
        if !pointee.ends_with(')') {
            parse_type(pointee)?;
        }
        return Ok(LlType::Ptr(space));
    }
    Err(format!("native emitter: unsupported type `{s}`"))
}

fn split_trailing_addrspace(s: &str) -> Result<(&str, u32), String> {
    let Some(open) = s.strip_suffix(')').and_then(|s| s.rfind(" addrspace(")) else {
        return Ok((s, 0));
    };
    let digits = &s[open + " addrspace(".len()..s.len() - 1];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Ok((s, 0));
    }
    let space = digits
        .parse()
        .map_err(|e| format!("native emitter: bad addrspace in `{s}*`: {e}"))?;
    Ok((&s[..open], space))
}

fn strip_leading_fast_math_flags(mut s: &str) -> &str {
    loop {
        let toks = split_top_level_whitespace(s);
        let Some(flag) = toks.first().copied() else {
            return s;
        };
        if !matches!(
            flag,
            "fast" | "nnan" | "ninf" | "nsz" | "arcp" | "contract" | "afn" | "reassoc" | "volatile"
        ) {
            return s;
        }
        s = s[flag.len()..].trim_start();
    }
}

pub(super) fn parse_struct_fields(s: &str) -> Result<Vec<LlType>, String> {
    split_top_level(s, ',')
        .into_iter()
        .map(parse_type)
        .collect::<Result<Vec<_>, _>>()
}

pub(super) fn split_vector_type(s: &str) -> Result<(u32, &str), String> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '[' | '{' | '(' => depth += 1,
            '>' | ']' | '}' | ')' => depth -= 1,
            'x' if depth == 0 => {
                let before = s[..i].trim();
                let after = s[i + 1..].trim();
                if before.chars().all(|c| c.is_ascii_digit()) {
                    return Ok((
                        before
                            .parse()
                            .map_err(|e| format!("native emitter: bad vector lane count: {e}"))?,
                        after,
                    ));
                }
            }
            _ => {}
        }
    }
    Err(format!("native emitter: malformed vector type `<{s}>`"))
}

pub(super) fn parse_constant_vector(s: &str) -> Result<TypedValue, String> {
    let s = s.trim();
    let end_ty = matching_angle(s, 0)
        .ok_or_else(|| format!("native emitter: malformed constant vector `{s}`"))?;
    let ty = parse_type(&s[..=end_ty])?;
    Ok(TypedValue {
        ty,
        value: LlValue::Undef,
    })
}

pub(super) fn parse_vector_i32_values(s: &str) -> Result<Vec<u32>, String> {
    let s = s.trim();
    let end_ty = matching_angle(s, 0)
        .ok_or_else(|| format!("native emitter: malformed vector constant `{s}`"))?;
    let ty = parse_type(&s[..=end_ty])?;
    let LlType::Vector(_, lanes) = ty else {
        return Err(format!(
            "native emitter: vector constant type is not a vector `{s}`"
        ));
    };
    let values = s[end_ty + 1..].trim();
    if values == "zeroinitializer" {
        return Ok(vec![0; lanes as usize]);
    }
    if !values.starts_with('<') || !values.ends_with('>') {
        return Err(format!(
            "native emitter: vector constant has no value list `{s}`"
        ));
    }
    split_top_level(&values[1..values.len() - 1], ',')
        .into_iter()
        .map(|v| {
            let v = v.trim();
            let n = v
                .strip_prefix("i32 ")
                .ok_or_else(|| format!("native emitter: expected i32 vector lane, got `{v}`"))?;
            match n.trim() {
                "poison" | "undef" => Ok(u32::MAX),
                n => parse_u32(n),
            }
        })
        .collect()
}

#[cfg(test)]
mod indirect_call_tests {
    use super::*;

    #[test]
    fn indirect_call_through_register_is_named() {
        let s = "fast <4 x float> %86(ptr addrspace(2) %0, ptr %91)";
        let msg = indirect_call_diagnostic(s);
        assert!(msg.contains("indirect call"), "{msg}");
        assert!(msg.contains("function pointer %86"), "{msg}");
        assert!(!msg.contains("call without callee"), "{msg}");
    }

    #[test]
    fn malformed_call_without_register_falls_back_to_generic() {
        let s = "void ()";
        let msg = indirect_call_diagnostic(s);
        assert!(msg.contains("call without callee"), "{msg}");
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parse_type_scalars_and_ints() {
        assert_eq!(parse_type("void").unwrap(), LlType::Void);
        assert_eq!(parse_type("metadata").unwrap(), LlType::Void);
        assert_eq!(parse_type("float").unwrap(), LlType::Float);
        assert_eq!(parse_type("half").unwrap(), LlType::Half);
        assert_eq!(parse_type("bfloat").unwrap(), LlType::BFloat);
        assert_eq!(parse_type("i32").unwrap(), LlType::Int(32));
        assert_eq!(parse_type("i1").unwrap(), LlType::Int(1));
        assert_eq!(parse_type("nnan ninf float").unwrap(), LlType::Float);
    }

    #[test]
    fn parse_type_pointers() {
        assert_eq!(parse_type("ptr").unwrap(), LlType::Ptr(0));
        assert_eq!(parse_type("ptr addrspace(1)").unwrap(), LlType::Ptr(1));
        assert!(parse_type("ptr addrspace(2").is_err());
        assert!(parse_type("ptr addrspace(x)").is_err());
    }

    #[test]
    fn parse_type_typed_pointers() {
        assert_eq!(parse_type("i32*").unwrap(), LlType::Ptr(0));
        assert_eq!(parse_type("i32 addrspace(1)*").unwrap(), LlType::Ptr(1));
        assert_eq!(parse_type("float addrspace(2)*").unwrap(), LlType::Ptr(2));
        assert_eq!(
            parse_type("<4 x float> addrspace(1)*").unwrap(),
            LlType::Ptr(1)
        );
        assert_eq!(
            parse_type("%struct.Foo addrspace(3)*").unwrap(),
            LlType::Ptr(3)
        );
        assert_eq!(parse_type("i32 addrspace(1)**").unwrap(), LlType::Ptr(0));
        assert!(parse_type("notatype*").is_err());
        assert!(parse_type("i32 addrspace(x)*").is_err());
    }

    #[test]
    fn return_type_keeps_the_complete_pointer_spelling() {
        for (text, expected) in [
            ("%struct.Foo addrspace(1)*", LlType::Ptr(1)),
            (
                r#"noundef %"struct.Named Type" addrspace(2)*"#,
                LlType::Ptr(2),
            ),
            ("fastcc ptr addrspace(3)", LlType::Ptr(3)),
            ("i32 (i32) addrspace(4)*", LlType::Ptr(4)),
            ("zeroext i32", LlType::Int(32)),
            (
                "fast <4 x float>",
                LlType::Vector(Box::new(LlType::Float), 4),
            ),
        ] {
            assert_eq!(
                parse_return_type(text).expect("return type"),
                expected,
                "{text}"
            );
        }
        let call =
            parse_call("%struct.Table addrspace(1)* @get_table()").expect("typed pointer call");
        assert_eq!(call.ret, LlType::Ptr(1));
        let declaration = parse_declaration("declare %struct.Table addrspace(1)* @get_table()")
            .expect("typed pointer declaration");
        assert_eq!(declaration.ret, LlType::Ptr(1));
    }

    #[test]
    fn parse_type_prefix_keeps_the_typed_pointer_star() {
        assert_eq!(
            parse_type_prefix(r#"i32 addrspace(1)* nocapture readonly "air-buffer-no-alias""#)
                .unwrap(),
            LlType::Ptr(1)
        );
        let (params, _) = parse_params(r#"i32 addrspace(1)* nocapture %0, i32 %1"#).unwrap();
        assert_eq!(
            params,
            vec![
                ("%0".to_string(), LlType::Ptr(1)),
                ("%1".to_string(), LlType::Int(32)),
            ]
        );
    }

    #[test]
    fn parse_quoted_function_and_call_names() {
        let lines = [
            r#"define void @"re::df::pack"(ptr addrspace(1) %out) {"#,
            "  ret void",
            "}",
        ];
        let (function, _, _) = parse_function(&lines, 0).expect("parse function");
        assert_eq!(function.name, "re::df::pack");
        let declaration = parse_declaration(r#"declare void @"helper::quoted"(ptr addrspace(1))"#)
            .expect("parse declaration");
        assert_eq!(declaration.name, "helper::quoted");
        let call =
            parse_call(r#"void @"helper::quoted"(ptr addrspace(1) %out)"#).expect("parse call");
        assert_eq!(call.callee, "helper::quoted");
    }

    #[test]
    fn identity_ptr_bitcast_sees_through_the_pointee_typed_spelling() {
        assert_eq!(
            parse_identity_ptr_bitcast("%4 = bitcast i32 addrspace(1)* %0 to i8 addrspace(1)*"),
            Some(("%4".to_string(), "%0".to_string()))
        );
        assert_eq!(
            parse_identity_ptr_bitcast("%4 = bitcast ptr addrspace(1) %0 to ptr addrspace(1)"),
            Some(("%4".to_string(), "%0".to_string()))
        );
        assert_eq!(
            parse_identity_ptr_bitcast("%4 = bitcast i32 addrspace(1)* %0 to i8 addrspace(3)*"),
            None
        );
        assert_eq!(
            parse_identity_ptr_bitcast("%4 = bitcast i32 %0 to float"),
            None
        );
    }

    #[test]
    fn parse_type_quoted_name_stops_at_its_own_closing_quote() {
        assert!(parse_type(r#"%"struct.metal::_atomic" nocapture "air-buffer-no-alias""#).is_err());
        assert_eq!(
            parse_type_prefix(
                r#"%"struct.metal::_atomic" addrspace(1)* nocapture "air-buffer-no-alias""#
            )
            .unwrap(),
            LlType::Ptr(1)
        );
    }

    #[test]
    fn parse_type_named_and_quoted() {
        assert_eq!(
            parse_type("%struct.Foo").unwrap(),
            LlType::Named("%struct.Foo".to_string())
        );
        assert_eq!(
            parse_type(r#"%"a.b c""#).unwrap(),
            LlType::Named(r#"%"a.b c""#.to_string())
        );
    }

    #[test]
    fn parse_type_vectors_arrays_structs() {
        assert_eq!(
            parse_type("<4 x float>").unwrap(),
            LlType::Vector(Box::new(LlType::Float), 4)
        );
        assert_eq!(
            parse_type("<2 x <4 x i8>>").unwrap(),
            LlType::Vector(Box::new(LlType::Vector(Box::new(LlType::Int(8)), 4)), 2)
        );
        assert_eq!(
            parse_type("[3 x i16]").unwrap(),
            LlType::Array(Box::new(LlType::Int(16)), 3)
        );
        assert_eq!(
            parse_type("{ float, i32 }").unwrap(),
            LlType::Struct(vec![LlType::Float, LlType::Int(32)])
        );
        assert_eq!(
            parse_type("<{ i8, i8 }>").unwrap(),
            LlType::Struct(vec![LlType::Int(8), LlType::Int(8)])
        );
    }

    #[test]
    fn parse_type_rejects_garbage() {
        assert!(parse_type("banana").is_err());
        assert!(parse_type("iZ").is_err());
        assert!(parse_type("").is_err());
    }

    #[test]
    fn parse_typed_value_backtracks_over_multiword_types() {
        let tv = parse_typed_value("i32 7").unwrap();
        assert_eq!(tv.ty, LlType::Int(32));
        assert!(matches!(tv.value, LlValue::Int(7)));

        let tv = parse_typed_value("<4 x float> zeroinitializer").unwrap();
        assert_eq!(tv.ty, LlType::Vector(Box::new(LlType::Float), 4));
        assert!(matches!(tv.value, LlValue::Zero));

        let tv = parse_typed_value("ptr addrspace(1) %0").unwrap();
        assert_eq!(tv.ty, LlType::Ptr(1));
        assert!(matches!(tv.value, LlValue::Local(ref s) if s == "%0"));

        let tv = parse_typed_value(
            "ptr addrspace(3) nonnull captures(none) inttoptr (i64 1024 to ptr addrspace(3))",
        )
        .unwrap();
        assert_eq!(tv.ty, LlType::Ptr(3));
        assert!(matches!(
            tv.value,
            LlValue::IntToPtr {
                source,
                destination: LlType::Ptr(3),
            } if matches!(source.value, LlValue::Int(1024))
        ));

        assert!(parse_typed_value("i32").is_err());
        assert!(parse_typed_value("").is_err());
    }

    #[test]
    fn parse_typed_float32_bit_literal_preserves_bits() {
        let tv = parse_typed_value("float f0x358637BD").unwrap();
        assert_eq!(tv.ty, LlType::Float);
        assert!(matches!(tv.value, LlValue::Float32Bits(0x3586_37bd)));
    }

    #[test]
    fn switch_literal_operand_widths_and_overflow() {
        let op = switch_literal_operand(&LlValue::Int(5), &LlType::Int(32)).unwrap();
        assert!(matches!(op, Operand::LiteralBit32(5)));
        let op = switch_literal_operand(&LlValue::Int(5), &LlType::Int(64)).unwrap();
        assert!(matches!(op, Operand::LiteralBit64(5)));
        let op = switch_literal_operand(&LlValue::SignedInt(-1), &LlType::Int(8)).unwrap();
        assert!(matches!(op, Operand::LiteralBit32(0xFF)));
        assert!(switch_literal_operand(&LlValue::Int(256), &LlType::Int(8)).is_err());
        assert!(switch_literal_operand(&LlValue::Int(0), &LlType::Float).is_err());
        assert!(switch_literal_operand(&LlValue::Int(0), &LlType::Int(0)).is_err());
        assert!(switch_literal_operand(&LlValue::Int(0), &LlType::Int(128)).is_err());
    }

    #[test]
    fn parsers_never_panic_on_ascii_mutations() {
        let seeds = [
            "i32 7",
            "<4 x float> %0",
            "ptr addrspace(1) %v",
            "{ float, i32 } zeroinitializer",
            "[3 x i16] undef",
            "half 0xH3C00",
            "<2 x <4 x i8>> %x",
        ];
        let alphabet = b"<>[]{}(),% x0123456789ifloat\"";
        for seed in seeds {
            let bytes = seed.as_bytes();
            for pos in 0..bytes.len() {
                for &b in alphabet {
                    let mut m = bytes.to_vec();
                    m[pos] = b;
                    if let Ok(s) = std::str::from_utf8(&m) {
                        let _ = parse_type(s);
                        let _ = parse_typed_value(s);
                    }
                    let mut d = bytes.to_vec();
                    d.remove(pos);
                    if let Ok(s) = std::str::from_utf8(&d) {
                        let _ = parse_type(s);
                        let _ = parse_typed_value(s);
                    }
                    let mut ins = bytes.to_vec();
                    ins.insert(pos, b);
                    if let Ok(s) = std::str::from_utf8(&ins) {
                        let _ = parse_type(s);
                        let _ = parse_typed_value(s);
                    }
                }
            }
        }
    }
}
