pub(super) fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut out = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            continue;
        }
        match c {
            '<' | '[' | '{' | '(' => depth += 1,
            '>' | ']' | '}' | ')' => depth -= 1,
            _ if c == sep && depth == 0 => {
                out.push(s[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

// Attachments are not operands. Leave the original instruction and all module
// metadata intact; only skip attachments in comma-separated operand parsers.
pub(super) fn split_instruction_operands(s: &str) -> Vec<&str> {
    split_top_level(instruction_without_attachments(s), ',')
}

pub(super) fn instruction_without_attachments(s: &str) -> &str {
    let s = strip_comment(s);
    for part in split_top_level(s, ',') {
        let Some((kind, node)) = part.split_once(char::is_whitespace) else {
            continue;
        };
        if kind.starts_with('!')
            && kind.len() > 1
            && kind[1..].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
            && node.trim_start().starts_with('!')
        {
            let offset = part.as_ptr() as usize - s.as_ptr() as usize;
            let prefix = s[..offset].trim_end();
            return prefix.strip_suffix(',').unwrap_or(prefix).trim_end();
        }
    }
    s
}

pub(super) fn split_top_level_whitespace(s: &str) -> Vec<&str> {
    let mut depth = 0i32;
    let mut start = None;
    let mut out = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            if start.is_none() {
                start = Some(i);
            }
            in_string = true;
            continue;
        }
        match c {
            '<' | '[' | '{' | '(' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            '>' | ']' | '}' | ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(st) = start.take() {
                    out.push(&s[st..i]);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(i);
                }
            }
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

pub(super) fn strip_comment(s: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            continue;
        }
        if c == ';' {
            return &s[..i];
        }
    }
    s
}

pub(super) fn matching_paren(s: &str, open: usize) -> Option<usize> {
    matching_delim(s, open, '(', ')')
}

pub(super) fn matching_angle(s: &str, open: usize) -> Option<usize> {
    matching_delim(s, open, '<', '>')
}

pub(super) fn matching_delim(s: &str, open: usize, left: char, right: char) -> Option<usize> {
    if s[open..].chars().next()? != left {
        return None;
    }
    let mut depth = 0i32;
    for (i, c) in s.char_indices().skip_while(|(i, _)| *i < open) {
        if c == left {
            depth += 1;
        } else if c == right {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

pub(super) fn parse_u32(s: &str) -> Result<u32, String> {
    s.parse()
        .map_err(|e| format!("native emitter: expected u32 integer `{s}`: {e}"))
}

pub(super) fn parse_float_literal(s: &str) -> Result<f64, String> {
    let looks_float = s.contains('.') || s.contains('e') || s.contains('E');
    if !looks_float {
        return Err(format!("native emitter: not a decimal float literal `{s}`"));
    }
    s.parse()
        .map_err(|e| format!("native emitter: expected float `{s}`: {e}"))
}

pub(super) fn parse_float32_hex_literal(s: &str) -> Option<u32> {
    let hex = s.strip_prefix("f0x")?;
    if hex.len() != 8 {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

pub(super) fn parse_half_literal(s: &str) -> Option<u16> {
    let hex = s.strip_prefix("0xH")?;
    u16::from_str_radix(hex, 16).ok()
}

pub(super) fn parse_bfloat_literal(s: &str) -> Option<u16> {
    let hex = s.strip_prefix("0xR")?;
    u16::from_str_radix(hex, 16).ok()
}

pub(super) fn parse_hex_literal(s: &str) -> Option<u64> {
    let hex = s.strip_prefix("0x")?;
    u64::from_str_radix(hex, 16).ok()
}

pub(super) fn parse_i64_literal(s: &str) -> Result<i64, String> {
    if !s.starts_with('-') {
        return Err(format!("native emitter: not a signed integer `{s}`"));
    }
    s.parse()
        .map_err(|e| format!("native emitter: expected signed integer `{s}`: {e}"))
}

pub(super) fn parse_u64_literal(s: &str) -> Result<u64, String> {
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
            .map_err(|e| format!("native emitter: expected hex integer `{s}`: {e}"))
    } else {
        s.parse()
            .map_err(|e| format!("native emitter: expected integer `{s}`: {e}"))
    }
}

pub(super) fn parse_float_special_literal(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix('+').unwrap_or(s);
    match s {
        "qnan" => Some(0x7FC0_0000),
        "-qnan" => Some(0xFFC0_0000),
        "snan" => Some(0x7F80_0001),
        "-snan" => Some(0xFF80_0001),
        "inf" => Some(0x7F80_0000),
        "-inf" => Some(0xFF80_0000),
        "nan" => Some(0x7FC0_0000),
        "-nan" => Some(0xFFC0_0000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float32_bit_literal_requires_exact_width() {
        assert_eq!(parse_float32_hex_literal("f0x358637BD"), Some(0x3586_37bd));
        assert_eq!(parse_float32_hex_literal("f0x00000000"), Some(0));
        assert_eq!(parse_float32_hex_literal("f0x1234567"), None);
        assert_eq!(parse_float32_hex_literal("f0x123456789"), None);
        assert_eq!(parse_float32_hex_literal("f0x1234567g"), None);
    }

    #[test]
    fn instruction_operands_exclude_only_top_level_attachments() {
        assert_eq!(
            split_instruction_operands("[ %a, %entry ], [ %b, %loop ], !dbg !41, !custom !{i32 1}"),
            vec!["[ %a, %entry ]", "[ %b, %loop ]"]
        );
        assert_eq!(
            split_instruction_operands(r#"{ i32, i8 } %"v, !dbg !41", 1, !dbg !42 ; note"#),
            vec![r#"{ i32, i8 } %"v, !dbg !41""#, "1"]
        );
        assert_eq!(split_instruction_operands("i32 %v, not_an_index"), vec!["i32 %v", "not_an_index"]);
        assert_eq!(split_instruction_operands("i32 %v, !malformed"), vec!["i32 %v", "!malformed"]);
    }

    #[test]
    fn split_top_level_respects_nesting_and_strings() {
        assert_eq!(split_top_level("a, b, c", ','), vec!["a", "b", "c"]);
        assert_eq!(
            split_top_level("<2 x i32>, i8, [4 x i8]", ','),
            vec!["<2 x i32>", "i8", "[4 x i8]"]
        );
        assert_eq!(split_top_level("f(a, b), c", ','), vec!["f(a, b)", "c"]);
        assert_eq!(
            split_top_level(r#"%"a,b", i8"#, ','),
            vec![r#"%"a,b""#, "i8"]
        );
        assert_eq!(split_top_level(r#""a\",b""#, ','), vec![r#""a\",b""#]);
        assert_eq!(split_top_level("", ','), vec![""]);
        assert_eq!(split_top_level("  x  ", ','), vec!["x"]);
    }

    #[test]
    fn split_top_level_whitespace_groups_bracketed_and_quoted() {
        assert_eq!(split_top_level_whitespace("i32 %0"), vec!["i32", "%0"]);
        assert_eq!(
            split_top_level_whitespace("<2 x i32> %v"),
            vec!["<2 x i32>", "%v"]
        );
        assert_eq!(
            split_top_level_whitespace(r#"%"a b" %c"#),
            vec![r#"%"a b""#, "%c"]
        );
        assert_eq!(split_top_level_whitespace("   a   b   "), vec!["a", "b"]);
        assert!(split_top_level_whitespace("").is_empty());
    }

    #[test]
    fn strip_comment_ignores_semicolons_in_strings() {
        assert_eq!(strip_comment("add i32 %a ; a comment").trim(), "add i32 %a");
        assert_eq!(strip_comment(r#"call @"a;b"()"#), r#"call @"a;b"()"#);
        assert_eq!(strip_comment(r#""x\";y""#), r#""x\";y""#);
        assert_eq!(strip_comment("plain text"), "plain text");
    }

    #[test]
    fn matching_delim_finds_balanced_close_or_none() {
        assert_eq!(matching_paren("(a (b) c)", 0), Some(8));
        assert_eq!(matching_angle("<2 x <4 x i8>>", 0), Some(13));
        assert_eq!(matching_paren("(a (b)", 0), None);
        assert_eq!(matching_paren("[a]", 0), None);
        assert_eq!(matching_paren("ptr (x)", 4), Some(6));
    }

    #[test]
    fn literal_parsers_cover_prefixes_and_errors() {
        assert_eq!(parse_half_literal("0xH3C00"), Some(0x3C00));
        assert_eq!(parse_half_literal("3.0"), None);
        assert_eq!(parse_bfloat_literal("0xR3F80"), Some(0x3F80));
        assert_eq!(parse_bfloat_literal("0xH3C00"), None);
        assert_eq!(parse_hex_literal("0xFF"), Some(255));
        assert_eq!(parse_hex_literal("255"), None);
        assert_eq!(parse_u32("42").unwrap(), 42);
        assert!(parse_u32("-1").is_err());
        assert!(parse_u32("4.0").is_err());
        assert_eq!(parse_i64_literal("-7").unwrap(), -7);
        assert!(parse_i64_literal("7").is_err());
        assert_eq!(parse_u64_literal("0x10").unwrap(), 16);
        assert_eq!(parse_u64_literal("16").unwrap(), 16);
        assert!(parse_u64_literal("nope").is_err());
        assert_eq!(parse_float_literal("1.5").unwrap(), 1.5);
        assert_eq!(parse_float_literal("1e3").unwrap(), 1000.0);
        assert!(parse_float_literal("10").is_err());
    }
}
