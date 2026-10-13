use crate::fxhash::{HashMap, HashSet};

pub(super) fn location_index_with_static(
    body: &str,
    fallback: u32,
    static_int_globals: &HashMap<String, u32>,
) -> u32 {
    super::declared_slot(body, "air.location_index", fallback, static_int_globals)
}

fn parse_global_name(s: &str) -> Option<String> {
    let at = s.find('@')?;
    let name = s[at..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(*c, ',' | ')' | '(' | '[' | ']'))
        .collect::<String>();
    if name.len() > 1 {
        Some(name)
    } else {
        None
    }
}

#[derive(Clone, Debug)]
enum StaticValue {
    Bool(bool),
    Int(u64),
    Vector(Vec<u64>),
}

#[derive(Clone, Debug)]
pub(crate) enum StaticIntValue {
    Scalar(u32),
    Vector(Vec<u32>),
}

pub(super) fn static_init_int_global_values(ll: &str) -> HashMap<String, u32> {
    static_init_global_values(ll)
        .into_iter()
        .filter_map(|(global, value)| match value {
            StaticValue::Bool(value) => Some((global, u32::from(value))),
            StaticValue::Int(value) => Some((global, value as u32)),
            StaticValue::Vector(_) => None,
        })
        .collect()
}

fn static_init_global_values(ll: &str) -> HashMap<String, StaticValue> {
    let mut globals = parse_static_global_initializers(ll);
    let mut unknown_stores = HashSet::default();
    let mut env: HashMap<String, StaticValue> = HashMap::default();
    let mut in_static_init = false;

    for raw in ll.lines() {
        let line = raw.split(';').next().unwrap_or(raw).trim();
        if line.starts_with("define ") {
            in_static_init = crate::air_static_init::define_line_declares_static_initializer(line);
            env.clear();
            continue;
        }
        if !in_static_init {
            continue;
        }
        if line == "}" {
            in_static_init = false;
            continue;
        }
        if line.is_empty() || line.ends_with(':') || line.starts_with("switch ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("store ") {
            let mut parts = rest.splitn(2, ',');
            let Some(value) = parts.next() else { continue };
            let Some(ptr) = parts.next() else { continue };
            let Some(global) = parse_global_name(ptr) else {
                continue;
            };
            if let Some(evaluated) = eval_value_token(value_token(value), &env, &globals) {
                unknown_stores.remove(&global);
                let mask = integer_result_width_and_mask(value)
                    .map_or(u64::from(u32::MAX), |(_, mask)| mask);
                globals.insert(
                    global,
                    match evaluated {
                        StaticValue::Int(evaluated) => StaticValue::Int(evaluated & mask),
                        evaluated => evaluated,
                    },
                );
            } else {
                globals.remove(&global);
                unknown_stores.insert(global);
            }
            continue;
        }
        let Some((name, rhs)) = line.split_once(" = ") else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with('%') {
            continue;
        }
        if let Some(value) = eval_static_rhs(rhs.trim(), &env, &globals) {
            env.insert(name.to_string(), value);
        }
    }

    globals.retain(|global, _| !unknown_stores.contains(global));
    globals
}

pub(crate) fn static_init_foldable_global_values(ll: &str) -> HashMap<String, StaticIntValue> {
    let mut values = static_init_global_values(ll);
    let mut derived_globals = ll
        .lines()
        .filter_map(|raw| {
            let line = raw.split(';').next().unwrap_or(raw).trim();
            (line.starts_with('@') && line.contains("air.fc_initializer"))
                .then(|| line.split_once(" = ").map(|(name, _)| name.to_string()))
                .flatten()
        })
        .collect::<HashSet<_>>();
    let initializer_globals = derived_globals.clone();
    let mut derived_locals = HashSet::<String>::default();
    let mut in_constructor = false;
    for raw in ll.lines() {
        let line = raw.split(';').next().unwrap_or(raw).trim();
        if line.starts_with("define ") {
            in_constructor = crate::air_static_init::define_line_declares_static_initializer(line);
            derived_locals.clear();
            continue;
        }
        if line == "}" {
            in_constructor = false;
            continue;
        }
        if !in_constructor {
            continue;
        }
        if let Some(rest) = line.strip_prefix("store ") {
            let mut parts = rest.splitn(2, ',');
            let value = parts.next().map(value_token);
            let target = parts.next().and_then(parse_global_name);
            if let (Some(value), Some(target)) = (value, target) {
                if derived_locals.contains(value) || derived_globals.contains(value) {
                    derived_globals.insert(target);
                } else {
                    derived_globals.remove(&target);
                }
            }
            continue;
        }
        let Some((result, rhs)) = line.split_once(" = ") else {
            continue;
        };
        let literal_predicate =
            rhs.starts_with("icmp ") && !rhs.contains('%') && !rhs.contains('@');
        if result.starts_with('%')
            && (literal_predicate
                || derived_globals
                    .iter()
                    .chain(&derived_locals)
                    .any(|symbol| references_symbol(rhs, symbol)))
        {
            derived_locals.insert(result.to_string());
        }
    }
    values.retain(|global, _| derived_globals.contains(global));

    let extended_names = values
        .keys()
        .filter(|name| !name.as_bytes()[1..].iter().all(|&b| symbol_byte(b)))
        .cloned()
        .collect::<Vec<_>>();
    let mut in_static_init = false;
    let mut in_function = false;
    for raw in ll.lines() {
        let line = raw.split(';').next().unwrap_or(raw).trim();
        if line.starts_with("define ") {
            in_function = true;
            in_static_init = crate::air_static_init::define_line_declares_static_initializer(line);
            continue;
        }
        if line == "}" {
            in_function = false;
            in_static_init = false;
            continue;
        }
        if !in_function || in_static_init || line.is_empty() {
            continue;
        }
        if !line.contains('@') {
            continue;
        }
        let loaded = line
            .split_once(" = load ")
            .and_then(|(_, load)| parse_global_name(load));
        for (start, _) in line.match_indices('@') {
            if start != 0 && symbol_byte(line.as_bytes()[start - 1]) {
                continue;
            }
            let end = line.as_bytes()[start + 1..]
                .iter()
                .position(|&b| !symbol_byte(b))
                .map_or(line.len(), |len| start + 1 + len);
            let global = &line[start..end];
            if loaded.as_deref() != Some(global) {
                values.remove(global);
            }
        }
        for global in &extended_names {
            if loaded.as_ref() != Some(global) && references_symbol(line, global) {
                values.remove(global);
            }
        }
    }
    values.retain(|global, _| !initializer_globals.contains(global));
    values
        .into_iter()
        .map(|(global, value)| {
            let value = match value {
                StaticValue::Bool(value) => StaticIntValue::Scalar(u32::from(value)),
                StaticValue::Int(value) => StaticIntValue::Scalar(value as u32),
                StaticValue::Vector(values) => {
                    StaticIntValue::Vector(values.into_iter().map(|value| value as u32).collect())
                }
            };
            (global, value)
        })
        .collect()
}

fn symbol_byte(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'$')
}

fn references_symbol(text: &str, symbol: &str) -> bool {
    text.match_indices(symbol).any(|(start, _)| {
        let end = start + symbol.len();
        let boundary = |byte: u8| !matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'$');
        (start == 0 || boundary(text.as_bytes()[start - 1]))
            && (end == text.len() || boundary(text.as_bytes()[end]))
    })
}

fn parse_static_global_initializers(ll: &str) -> HashMap<String, StaticValue> {
    let mut globals = HashMap::default();
    for raw in ll.lines() {
        let line = raw.split(';').next().unwrap_or(raw).trim();
        if !line.starts_with('@') || !(line.contains(" global ") || line.contains(" constant ")) {
            continue;
        }
        let Some((name, rest)) = line.split_once(" = ") else {
            continue;
        };
        let Some(value) = integer_initializer(rest) else {
            continue;
        };
        globals.insert(name.trim().to_string(), value);
    }
    globals
}

fn integer_initializer(rest: &str) -> Option<StaticValue> {
    let typed_init = rest
        .split_once(" global ")
        .map(|(_, init)| init)
        .or_else(|| rest.split_once(" constant ").map(|(_, init)| init))?;
    if typed_init.starts_with('<') {
        let type_end = typed_init.find('>')?;
        let vector_ty = &typed_init[1..type_end];
        let (lanes, element_ty) = vector_ty.split_once(" x ")?;
        let lanes = lanes.parse::<usize>().ok()?;
        let width = element_ty.strip_prefix('i')?.parse::<u32>().ok()?;
        if width == 0 || width > 32 {
            return None;
        }
        let vector = typed_init[type_end + 1..].trim_start();
        let vector = if vector.starts_with('<') {
            &vector[..=vector.find('>')?]
        } else {
            vector.split(',').next().unwrap_or(vector).trim()
        };
        if vector == "undef" || vector == "zeroinitializer" {
            return rest
                .contains("air.fc_initializer")
                .then(|| StaticValue::Vector(vec![0; lanes]));
        }
        let values = vector.strip_prefix('<')?.strip_suffix('>')?;
        let mask = if width == 32 {
            u64::from(u32::MAX)
        } else {
            (1_u64 << width) - 1
        };
        let values = values
            .split(',')
            .map(|lane| {
                let (ty, value) = lane.trim().split_once(' ')?;
                (ty == element_ty)
                    .then(|| {
                        value
                            .parse::<u64>()
                            .or_else(|_| value.parse::<i64>().map(|value| value as u64))
                            .ok()
                    })
                    .flatten()
                    .map(|value| value & mask)
            })
            .collect::<Option<Vec<_>>>()?;
        return (values.len() == lanes).then_some(StaticValue::Vector(values));
    }
    let mut tokens = typed_init.split_whitespace();
    let ty = tokens.next()?;
    let width = ty.strip_prefix('i')?.parse::<u32>().ok()?;
    if !matches!(width, 8 | 16 | 32 | 64) {
        return None;
    }
    let (_, mask) = integer_result_width_and_mask(ty)?;
    let value = tokens.next()?.trim_end_matches(',');
    if value == "undef" && rest.contains("air.fc_initializer") {
        return Some(StaticValue::Int(0));
    }
    value
        .parse::<u64>()
        .or_else(|_| value.parse::<i64>().map(|value| value as u64))
        .ok()
        .map(|value| StaticValue::Int(value & mask))
}

const INTEGER_BINARY_OPCODES: &[&str] = &[
    "add", "sub", "mul", "and", "or", "xor", "udiv", "urem", "sdiv", "srem", "shl", "lshr", "ashr",
];

fn signed_of(value: u64, width: u32) -> i64 {
    let shift = 64 - width.min(64);
    ((value << shift) as i64) >> shift
}

fn eval_static_rhs(
    rhs: &str,
    env: &HashMap<String, StaticValue>,
    globals: &HashMap<String, StaticValue>,
) -> Option<StaticValue> {
    if rhs.contains("@air.is_function_constant_defined(") {
        return Some(StaticValue::Bool(false));
    }
    if let Some((name, rest)) = ["umax", "umin", "smax", "smin"]
        .into_iter()
        .find_map(|name| {
            rhs.split_once(&format!("@llvm.{name}."))
                .map(|(_, rest)| (name, rest))
        })
    {
        let arguments = rest.split_once('(')?.1.rsplit_once(')')?.0;
        let (lhs, rhs) = eval_binary_int(arguments, env, globals)?;
        let (width, mask) = integer_result_width_and_mask(arguments)?;
        let (lhs, rhs) = (lhs & mask, rhs & mask);
        let value = match name {
            "umax" => lhs.max(rhs),
            "umin" => lhs.min(rhs),
            "smax" => signed_of(lhs, width).max(signed_of(rhs, width)) as u64,
            "smin" => signed_of(lhs, width).min(signed_of(rhs, width)) as u64,
            _ => unreachable!("matched min/max intrinsic"),
        };
        return Some(StaticValue::Int(value & mask));
    }
    if rhs.contains("@air.normalize_function_constant_predicate.") {
        let arguments = rhs.split_once('(')?.1.rsplit_once(')')?.0;
        let value = eval_int_operand(arguments, env, globals)?;
        return Some(StaticValue::Int(u64::from(value != 0)));
    }
    if rhs.starts_with("load ") {
        let global = parse_global_name(rhs)?;
        return globals.get(&global).cloned();
    }
    if let Some(rest) = rhs.strip_prefix("extractelement ") {
        let parts = rest.split(',').collect::<Vec<_>>();
        let vector = eval_value_token(parts.first()?.split_whitespace().last()?, env, globals)?;
        let idx = eval_int_operand(parts.get(1)?, env, globals)? as usize;
        let StaticValue::Vector(values) = vector else {
            return None;
        };
        return values.get(idx).copied().map(StaticValue::Int);
    }
    if let Some((opcode, rest)) = INTEGER_BINARY_OPCODES.iter().find_map(|opcode| {
        rhs.strip_prefix(opcode)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(|rest| (*opcode, rest))
    }) {
        let (lhs, rhs) = eval_binary_int(rest, env, globals)?;
        let (width, mask) = integer_result_width_and_mask(rest)?;
        let (lhs, rhs) = (lhs & mask, rhs & mask);
        let value = match opcode {
            "add" => lhs.wrapping_add(rhs),
            "sub" => lhs.wrapping_sub(rhs),
            "mul" => lhs.wrapping_mul(rhs),
            "and" => lhs & rhs,
            "or" => lhs | rhs,
            "xor" => lhs ^ rhs,
            "udiv" => lhs.checked_div(rhs)?,
            "urem" => lhs.checked_rem(rhs)?,
            "sdiv" => signed_of(lhs, width).checked_div(signed_of(rhs, width))? as u64,
            "srem" => signed_of(lhs, width).checked_rem(signed_of(rhs, width))? as u64,
            "shl" if rhs < u64::from(width) => lhs.checked_shl(rhs.try_into().ok()?)?,
            "lshr" if rhs < u64::from(width) => lhs.checked_shr(rhs.try_into().ok()?)?,
            "ashr" if rhs < u64::from(width) => (signed_of(lhs, width) >> rhs.min(63)) as u64,
            "shl" | "lshr" | "ashr" => return None,
            _ => unreachable!("matched integer opcode"),
        };
        return Some(StaticValue::Int(value & mask));
    }
    if let Some(rest) = rhs.strip_prefix("icmp ") {
        let (predicate, operands) = rest.split_once(' ')?;
        let (lhs, rhs) = eval_binary_int(operands, env, globals)?;
        let (width, mask) = integer_result_width_and_mask(operands)?;
        let (lhs, rhs) = (lhs & mask, rhs & mask);
        let value = match predicate {
            "eq" => lhs == rhs,
            "ne" => lhs != rhs,
            "ugt" => lhs > rhs,
            "uge" => lhs >= rhs,
            "ult" => lhs < rhs,
            "ule" => lhs <= rhs,
            "sgt" => signed_of(lhs, width) > signed_of(rhs, width),
            "sge" => signed_of(lhs, width) >= signed_of(rhs, width),
            "slt" => signed_of(lhs, width) < signed_of(rhs, width),
            "sle" => signed_of(lhs, width) <= signed_of(rhs, width),
            _ => return None,
        };
        return Some(StaticValue::Bool(value));
    }
    if let Some(rest) = rhs.strip_prefix("select ") {
        let parts = rest.split(',').collect::<Vec<_>>();
        let cond = eval_bool_operand(parts.first()?, env, globals)?;
        let chosen = if cond { parts.get(1)? } else { parts.get(2)? };
        return eval_value_token(value_token(chosen), env, globals);
    }
    if let Some(rest) = rhs
        .strip_prefix("trunc ")
        .or_else(|| rhs.strip_prefix("zext "))
    {
        let (value, to_ty) = rest.split_once(" to ")?;
        let int = eval_int_operand(value, env, globals)?;
        let (_, mask) = integer_result_width_and_mask(to_ty)?;
        return Some(StaticValue::Int(int & mask));
    }
    if rhs.contains("function_constant_predicate") {
        let open = rhs.rfind('(')?;
        let close = rhs.rfind(')')?;
        return Some(StaticValue::Int(eval_int_operand(
            &rhs[open + 1..close],
            env,
            globals,
        )?));
    }
    None
}

fn eval_binary_int(
    rest: &str,
    env: &HashMap<String, StaticValue>,
    globals: &HashMap<String, StaticValue>,
) -> Option<(u64, u64)> {
    let parts = rest.split(',').collect::<Vec<_>>();
    Some((
        eval_int_operand(parts.first()?, env, globals)?,
        eval_int_operand(parts.get(1)?, env, globals)?,
    ))
}

fn integer_result_width_and_mask(text: &str) -> Option<(u32, u64)> {
    let width = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .find_map(|token| token.strip_prefix('i')?.parse::<u32>().ok())?;
    match width {
        1..=63 => Some((width, (1_u64 << width) - 1)),
        64 => Some((width, u64::MAX)),
        _ => None,
    }
}

fn eval_int_operand(
    text: &str,
    env: &HashMap<String, StaticValue>,
    globals: &HashMap<String, StaticValue>,
) -> Option<u64> {
    match eval_value_token(value_token(text), env, globals)? {
        StaticValue::Bool(value) => Some(u64::from(value)),
        StaticValue::Int(value) => Some(value),
        StaticValue::Vector(_) => None,
    }
}

fn eval_bool_operand(
    text: &str,
    env: &HashMap<String, StaticValue>,
    globals: &HashMap<String, StaticValue>,
) -> Option<bool> {
    match eval_value_token(value_token(text), env, globals)? {
        StaticValue::Bool(value) => Some(value),
        StaticValue::Int(value) => Some(value != 0),
        StaticValue::Vector(_) => None,
    }
}

fn eval_value_token(
    token: &str,
    env: &HashMap<String, StaticValue>,
    globals: &HashMap<String, StaticValue>,
) -> Option<StaticValue> {
    let token = token.trim().trim_end_matches(',');
    if token == "true" {
        return Some(StaticValue::Bool(true));
    }
    if token == "false" {
        return Some(StaticValue::Bool(false));
    }
    if token.starts_with('%') {
        return env.get(token).cloned();
    }
    if token.starts_with('@') {
        return globals.get(token).cloned();
    }
    token
        .parse::<u64>()
        .or_else(|_| token.parse::<i64>().map(|value| value as u64))
        .ok()
        .map(StaticValue::Int)
}

fn value_token(text: &str) -> &str {
    text.split_whitespace()
        .last()
        .unwrap_or_else(|| text.trim())
        .trim_end_matches(',')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_foldable(ll: &str) -> HashMap<String, StaticIntValue> {
        let mut values = static_init_global_values(ll);
        let mut derived_globals = ll
            .lines()
            .filter_map(|raw| {
                let line = raw.split(';').next().unwrap_or(raw).trim();
                (line.starts_with('@') && line.contains("air.fc_initializer"))
                    .then(|| line.split_once(" = ").map(|(name, _)| name.to_string()))
                    .flatten()
            })
            .collect::<HashSet<_>>();
        let initializer_globals = derived_globals.clone();
        let mut derived_locals = HashSet::<String>::default();
        let mut in_constructor = false;
        for raw in ll.lines() {
            let line = raw.split(';').next().unwrap_or(raw).trim();
            if line.starts_with("define ") {
                in_constructor =
                    crate::air_static_init::define_line_declares_static_initializer(line);
                derived_locals.clear();
                continue;
            }
            if line == "}" {
                in_constructor = false;
                continue;
            }
            if !in_constructor {
                continue;
            }
            if let Some(rest) = line.strip_prefix("store ") {
                let mut parts = rest.splitn(2, ',');
                let value = parts.next().map(value_token);
                let target = parts.next().and_then(parse_global_name);
                if let (Some(value), Some(target)) = (value, target) {
                    if derived_locals.contains(value) || derived_globals.contains(value) {
                        derived_globals.insert(target);
                    } else {
                        derived_globals.remove(&target);
                    }
                }
                continue;
            }
            let Some((result, rhs)) = line.split_once(" = ") else {
                continue;
            };
            if result.starts_with('%')
                && derived_globals
                    .iter()
                    .chain(&derived_locals)
                    .any(|symbol| references_symbol(rhs, symbol))
            {
                derived_locals.insert(result.to_string());
            }
        }
        values.retain(|global, _| derived_globals.contains(global));

        let mut in_static_init = false;
        let mut in_function = false;
        for raw in ll.lines() {
            let line = raw.split(';').next().unwrap_or(raw).trim();
            if line.starts_with("define ") {
                in_function = true;
                in_static_init =
                    crate::air_static_init::define_line_declares_static_initializer(line);
                continue;
            }
            if line == "}" {
                in_function = false;
                in_static_init = false;
                continue;
            }
            if !in_function || in_static_init || line.is_empty() {
                continue;
            }
            values.retain(|global, _| {
                if !references_symbol(line, global) {
                    return true;
                }
                let Some(load) = line.split_once(" = load ").map(|(_, load)| load) else {
                    return false;
                };
                parse_global_name(load).as_ref() == Some(global)
            });
        }
        values.retain(|global, _| !initializer_globals.contains(global));
        values
            .into_iter()
            .map(|(global, value)| {
                let value = match value {
                    StaticValue::Bool(value) => StaticIntValue::Scalar(u32::from(value)),
                    StaticValue::Int(value) => StaticIntValue::Scalar(value as u32),
                    StaticValue::Vector(values) => StaticIntValue::Vector(
                        values.into_iter().map(|value| value as u32).collect(),
                    ),
                };
                (global, value)
            })
            .collect()
    }

    #[test]
    fn many_immutable_cells_match_reference_with_sparse_escapes() {
        let mut ll = String::from(
            "@fc = internal addrspace(2) global i32 7, section \"air.fc_initializer\"\n",
        );
        for i in 0..64 {
            ll.push_str(&format!(
                "@cell{i} = internal addrspace(2) global i32 undef\n"
            ));
        }
        ll.push_str("define internal void @init() section \"air.static_init\" {\n%x = load i32, ptr addrspace(2) @fc\n");
        for i in 0..64 {
            ll.push_str(&format!("store i32 %x, ptr addrspace(2) @cell{i}\n"));
        }
        ll.push_str("ret void\n}\ndefine void @use() {\n");
        for i in 0..512 {
            ll.push_str(&format!(
                "%v{i} = load i32, ptr addrspace(2) @cell{}\n",
                i % 64
            ));
        }
        for i in (0..64).step_by(7) {
            ll.push_str(&format!("call void @escape(ptr addrspace(2) @cell{i})\n"));
        }
        ll.push_str("ret void\n}\n");
        let actual = static_init_foldable_global_values(&ll);
        let expected = reference_foldable(&ll);
        assert_eq!(actual.len(), 54);
        assert_eq!(actual.len(), expected.len());
        for (key, value) in expected {
            assert_eq!(
                format!("{:?}", actual.get(&key).unwrap()),
                format!("{value:?}")
            );
        }
    }

    #[test]
    fn indexed_global_uses_match_full_scan() {
        let mut prefix = String::new();
        for name in [
            "cell",
            "cell.more",
            "cell$2",
            "cell_2",
            "cell-extended",
            "\"quoted\"",
        ] {
            prefix.push_str(&format!(
                "@{name} = internal addrspace(2) global i32 7, section \"air.fc_initializer\"\n"
            ));
        }
        prefix.push_str("@mirror = internal addrspace(2) global i32 undef\n\ndefine internal void @init() section \"air.static_init\" {\n %x = load i32, ptr addrspace(2) @cell\n store i32 %x, ptr addrspace(2) @mirror\n ret void\n}\n");
        for body in [
            "%x = load i32, ptr addrspace(2) @mirror",
            "store i32 9, ptr addrspace(2) @mirror",
            "call void @escape(ptr addrspace(2) @mirror)",
            "%x = load i32, ptr addrspace(2) @mirror.extra",
            "%x = load i32, ptr addrspace(2) @cell",
            "%x = add i32 1, 2",
        ] {
            let ll = format!("{prefix}\ndefine void @use() {{\n{body}\nret void\n}}\n");
            let actual = static_init_foldable_global_values(&ll);
            let expected = reference_foldable(&ll);
            assert_eq!(
                format!("{:?}", actual.get("@mirror")),
                format!("{:?}", expected.get("@mirror")),
                "{body}"
            );
        }
    }
    #[test]
    fn vector_initializer_is_not_misclassified_as_a_scalar() {
        let ll = r#"
@fc.MTL_FC_INIT_3_Dv4_j = internal addrspace(2) externally_initialized constant <4 x i32> <i32 1, i32 2, i32 3, i32 4>, section "air.fc_initializer", align 16
@mirror = internal addrspace(2) global i32 undef, align 4

define internal void @_GLOBAL__sub_I_vector_fc() section "air.static_init" {
entry:
  %value = load <4 x i32>, ptr addrspace(2) @fc.MTL_FC_INIT_3_Dv4_j
  %lane = extractelement <4 x i32> %value, i64 2
  store i32 %lane, ptr addrspace(2) @mirror
  ret void
}

define i32 @use() {
entry:
  %value = load i32, ptr addrspace(2) @mirror
  ret i32 %value
}
"#;

        let values = static_init_int_global_values(ll);
        assert_eq!(values.get("@mirror"), Some(&3));
        assert!(!values.contains_key("@fc.MTL_FC_INIT_3_Dv4_j"));
        let foldable = static_init_foldable_global_values(ll);
        assert!(matches!(
            foldable.get("@mirror"),
            Some(StaticIntValue::Scalar(3))
        ));
        assert!(!foldable.contains_key("@fc.MTL_FC_INIT_3_Dv4_j"));
    }

    #[test]
    fn signed_masked_function_constant_initializer_is_foldable() {
        let ll = r#"
@fc.MTL_FC_INIT_2_t = internal addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@rounded = internal addrspace(2) global i16 undef, align 2
@negative = internal addrspace(2) global i16 -1, align 2

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %value = load i16, ptr addrspace(2) @fc.MTL_FC_INIT_2_t
  %biased = add i16 %value, 15
  %masked = and i16 %biased, -16
  store i16 %masked, ptr addrspace(2) @rounded
  ret void
}

define i16 @use() {
entry:
  %value = load i16, ptr addrspace(2) @rounded
  ret i16 %value
}
"#;

        let values = static_init_foldable_global_values(ll);
        assert!(matches!(
            values.get("@rounded"),
            Some(StaticIntValue::Scalar(0))
        ));
        assert_eq!(
            static_init_int_global_values(ll).get("@negative"),
            Some(&65535)
        );
    }

    #[test]
    fn out_of_width_shift_is_not_folded() {
        let ll = r#"
@fc.MTL_FC_INIT_2_t = internal addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@shifted = internal addrspace(2) global i16 undef, align 2

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %value = load i16, ptr addrspace(2) @fc.MTL_FC_INIT_2_t
  %value.shifted = shl i16 %value, 16
  store i16 %value.shifted, ptr addrspace(2) @shifted
  ret void
}

define i16 @use() {
entry:
  %value = load i16, ptr addrspace(2) @shifted
  ret i16 %value
}
"#;

        assert!(!static_init_foldable_global_values(ll).contains_key("@shifted"));
    }

    #[test]
    fn function_constant_predicate_normalization_preserves_an_enabled_default() {
        let ll = r#"
@fc.MTL_FC_INIT_1_b = internal addrspace(2) externally_initialized constant i8 1, section "air.fc_initializer", align 1
@predicate = internal addrspace(2) global i8 0, align 1

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %value = load i8, ptr addrspace(2) @fc.MTL_FC_INIT_1_b
  %normalized = tail call i8 @air.normalize_function_constant_predicate.i8(i8 %value)
  store i8 %normalized, ptr addrspace(2) @predicate
  ret void
}
"#;

        assert_eq!(
            static_init_int_global_values(ll).get("@predicate"),
            Some(&1)
        );
    }

    #[test]
    fn every_static_initializer_integer_expression_folds() {
        for (expression, expected) in [
            ("%a = add i16 %one, 15", 16),
            ("%a = sub i16 %one, 15", 0xfff2),
            ("%a = mul i16 %one, 15", 15),
            ("%a = udiv i16 %seven, 2", 3),
            ("%a = urem i16 %seven, 2", 1),
            ("%a = sdiv i16 %minus_one, 1", 0xffff),
            ("%a = srem i16 %seven, 2", 1),
            ("%a = and i16 %seven, 2", 2),
            ("%a = or i16 %one, 2", 3),
            ("%a = xor i16 %one, 3", 2),
            ("%a = shl i16 %one, 3", 8),
            ("%a = lshr i16 %minus_one, 12", 0xf),
            ("%a = ashr i16 %minus_one, 12", 0xffff),
            ("%a = icmp ugt i16 %seven, 2", 1),
            ("%a = icmp uge i16 %seven, 7", 1),
            ("%a = icmp ult i16 %seven, 2", 0),
            ("%a = icmp ule i16 %seven, 7", 1),
            ("%a = icmp ugt i16 %minus_one, 7", 1),
            ("%a = icmp sgt i16 %minus_one, 7", 0),
            ("%a = icmp slt i16 %minus_one, 7", 1),
            ("%a = icmp sge i16 %minus_one, %minus_one", 1),
            ("%a = icmp sle i16 %minus_one, 7", 1),
            ("%a = icmp eq i16 %seven, 7", 1),
            ("%a = icmp ne i16 %seven, 7", 0),
            ("%a = tail call i16 @llvm.umax.i16(i16 %seven, i16 2)", 7),
            ("%a = tail call i16 @llvm.umin.i16(i16 %seven, i16 2)", 2),
            (
                "%a = tail call i16 @llvm.umax.i16(i16 %minus_one, i16 2)",
                0xffff,
            ),
            (
                "%a = tail call i16 @llvm.smax.i16(i16 %minus_one, i16 2)",
                2,
            ),
            (
                "%a = tail call i16 @llvm.smin.i16(i16 %minus_one, i16 2)",
                0xffff,
            ),
        ] {
            let ll = format!(
                r#"
@one = internal addrspace(2) global i16 1, align 2
@seven = internal addrspace(2) global i16 7, align 2
@minus_one = internal addrspace(2) global i16 -1, align 2
@folded = internal addrspace(2) global i16 undef, align 2

define internal void @_GLOBAL__sub_I_fold() section "air.static_init" {{
entry:
  %one = load i16, ptr addrspace(2) @one
  %seven = load i16, ptr addrspace(2) @seven
  %minus_one = load i16, ptr addrspace(2) @minus_one
  {expression}
  store i16 %a, ptr addrspace(2) @folded
  ret void
}}
"#
            );
            assert_eq!(
                static_init_int_global_values(&ll).get("@folded"),
                Some(&expected),
                "{expression}"
            );
        }
    }

    #[test]
    fn an_undefined_division_does_not_fold() {
        for expression in [
            "%a = udiv i16 %seven, 0",
            "%a = urem i16 %seven, 0",
            "%a = sdiv i16 %seven, 0",
            "%a = srem i16 %seven, 0",
        ] {
            let ll = format!(
                r#"
@seven = internal addrspace(2) global i16 7, align 2
@folded = internal addrspace(2) global i16 undef, align 2

define internal void @_GLOBAL__sub_I_fold() section "air.static_init" {{
entry:
  %seven = load i16, ptr addrspace(2) @seven
  {expression}
  store i16 %a, ptr addrspace(2) @folded
  ret void
}}
"#
            );
            assert_eq!(
                static_init_int_global_values(&ll).get("@folded"),
                None,
                "{expression}"
            );
        }
    }

    #[test]
    fn a_64_bit_function_constant_initializer_is_seeded() {
        let ll = r#"
@bits.MTL_FC_INIT_1_m = internal addrspace(2) externally_initialized constant i64 undef, section "air.fc_initializer", align 8
@mirror = internal addrspace(2) global i64 undef, align 8
@enabled = internal addrspace(2) global i8 undef, align 1

define internal void @_GLOBAL__sub_I_wide() section "air.static_init" {
entry:
  %value = load i64, ptr addrspace(2) @bits.MTL_FC_INIT_1_m
  store i64 %value, ptr addrspace(2) @mirror
  %high = lshr i64 %value, 63
  %narrow = trunc i64 %high to i8
  store i8 %narrow, ptr addrspace(2) @enabled
  ret void
}
"#;
        let values = static_init_int_global_values(ll);
        assert_eq!(values.get("@mirror"), Some(&0));
        assert_eq!(values.get("@enabled"), Some(&0));
    }

    #[test]
    fn a_stored_64_bit_value_keeps_its_high_half() {
        let ll = r#"
@bits = internal addrspace(2) global i64 1311768467463790320, align 8
@mirror = internal addrspace(2) global i64 undef, align 8
@high = internal addrspace(2) global i32 undef, align 4
@low = internal addrspace(2) global i32 undef, align 4
@residue = internal addrspace(2) global i64 undef, align 8

define internal void @_GLOBAL__sub_I_wide() section "air.static_init" {
entry:
  %value = load i64, ptr addrspace(2) @bits
  store i64 %value, ptr addrspace(2) @mirror
  %stored = load i64, ptr addrspace(2) @mirror
  %shifted = lshr i64 %stored, 32
  %top = trunc i64 %shifted to i32
  store i32 %top, ptr addrspace(2) @high
  %bottom = trunc i64 %stored to i32
  store i32 %bottom, ptr addrspace(2) @low
  %widened = zext i32 %bottom to i64
  %residue = lshr i64 %widened, 32
  store i64 %residue, ptr addrspace(2) @residue
  ret void
}
"#;
        let values = static_init_int_global_values(ll);
        assert_eq!(values.get("@high"), Some(&0x1234_5678));
        assert_eq!(values.get("@low"), Some(&0x9abc_def0));
        assert_eq!(values.get("@residue"), Some(&0));
    }
}
