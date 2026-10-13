use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::native::ir::{LlType, LlValue};
use crate::native::lex::strip_comment;
use crate::native::parse::{parse_type, split_top_level};

pub(in crate::native) fn resolve_alloca_ty(line: &str) -> Option<LlType> {
    let line = strip_comment(line).trim();
    let rhs = line.split_once(" = ")?.1.trim();
    let opcode = rhs.split_whitespace().next().unwrap_or("");
    let rest = rhs[opcode.len()..].trim_start();
    let parts = split_top_level(rest, ',');
    parse_type(parts.first()?.trim()).ok()
}

pub(in crate::native) fn rhs_of(line: &str) -> &str {
    line.split_once('=')
        .map(|(_, rhs)| rhs.trim())
        .unwrap_or(line)
}

pub(in crate::native) fn resolve_gep_pointee(
    rhs: &str,
    named_types: &HashMap<String, LlType>,
) -> Option<LlType> {
    let after = rhs.strip_prefix("getelementptr ")?;
    let after = after.strip_prefix("inbounds ").unwrap_or(after);
    let parts = split_top_level(after, ',');
    if parts.len() < 3 {
        return None;
    }
    let source_ty = parse_type(parts[0].trim()).ok()?;
    let indices: Vec<Option<usize>> = parts[3..]
        .iter()
        .map(|p| p.split_whitespace().last().and_then(|t| t.parse().ok()))
        .collect();
    extract_aggregate_member(source_ty, &indices, named_types)
}

pub(in crate::native) fn infer_use_pointees<B: AsRef<TirBlock>>(
    blocks: &[B],
) -> (HashMap<String, LlType>, usize, HashSet<String>) {
    let mut map: HashMap<String, LlType> = HashMap::default();
    let mut conflicts = 0usize;
    let mut byte_viewed: HashSet<String> = HashSet::default();
    for tb in blocks {
        let tb = tb.as_ref();
        for inst in &tb.insts {
            if let Some((ptr, pointee)) = deref_implied_pointee(inst) {
                if pointee == LlType::Int(8) {
                    byte_viewed.insert(ptr.to_string());
                    if inst.opcode == "getelementptr" {
                        if let Some(result) = &inst.result {
                            byte_viewed.insert(result.clone());
                        }
                    }
                }
                record_use_pointee(&mut map, &mut conflicts, ptr, pointee);
            }
            for (ptr, pointee) in atomic_call_pointees(inst) {
                if pointee == LlType::Int(8) {
                    byte_viewed.insert(ptr.clone());
                }
                record_use_pointee(&mut map, &mut conflicts, &ptr, pointee);
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for tb in blocks {
            let tb = tb.as_ref();
            for inst in &tb.insts {
                let Some(result) = &inst.result else { continue };
                if byte_viewed.contains(result) || !matches!(inst.result_ty, Some(LlType::Ptr(_))) {
                    continue;
                }
                let op = inst.opcode.as_str();
                if !matches!(
                    op,
                    "bitcast" | "select" | "phi" | "freeze" | "getelementptr"
                ) {
                    continue;
                }
                let tainted_operand = inst.operands.iter().any(|operand| match operand {
                    TirOperand::Value { name, ty } => {
                        matches!(ty, LlType::Ptr(_)) && byte_viewed.contains(name)
                    }
                    _ => false,
                }) || (op == "phi"
                    && inst.phi_values().is_some_and(|values| {
                        values.into_iter().any(|value| {
                            matches!(value, LlValue::Local(name) if byte_viewed.contains(name))
                        })
                    }));
                if tainted_operand {
                    byte_viewed.insert(result.clone());
                    changed = true;
                }
            }
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for tb in blocks {
            let tb = tb.as_ref();
            for inst in &tb.insts {
                let Some(result) = &inst.result else { continue };
                if !matches!(inst.result_ty, Some(LlType::Ptr(_))) {
                    continue;
                }
                let op = inst.opcode.as_str();
                if !matches!(op, "select" | "phi" | "freeze") {
                    continue;
                }
                let mut members: Vec<&str> = vec![result.as_str()];
                if op == "phi" {
                    if let Some(values) = inst.phi_values() {
                        for value in values {
                            if let LlValue::Local(name) = value {
                                members.push(name.as_str());
                            }
                        }
                    }
                } else {
                    for operand in &inst.operands {
                        if let TirOperand::Value { name, ty } = operand {
                            if matches!(ty, LlType::Ptr(_)) {
                                members.push(name.as_str());
                            }
                        }
                    }
                }
                let best = members
                    .iter()
                    .filter_map(|m| map.get(*m))
                    .max_by_key(|t| pointee_richness(t))
                    .cloned();
                let Some(best) = best else { continue };
                let best_rank = pointee_richness(&best);
                for m in &members {
                    if map.get(*m).map(pointee_richness) != Some(best_rank) {
                        map.insert(m.to_string(), best.clone());
                        changed = true;
                    }
                }
            }
        }
    }
    (map, conflicts, byte_viewed)
}

pub(in crate::native) fn atomic_call_pointees(inst: &TirInst) -> Vec<(String, LlType)> {
    let Some(call) = &inst.call() else {
        return Vec::new();
    };
    if !(call.callee.starts_with("air.atomic.global.")
        || call.callee.starts_with("air.atomic.local."))
    {
        return Vec::new();
    }
    let element = match &inst.result_ty {
        Some(t) => t.clone(),
        None => match call.args.iter().find(|tv| !matches!(tv.ty, LlType::Ptr(_))) {
            Some(tv) => tv.ty.clone(),
            None => return Vec::new(),
        },
    };
    call.args
        .iter()
        .filter_map(|tv| match (&tv.ty, &tv.value) {
            (LlType::Ptr(_), LlValue::Local(name)) => Some((name.clone(), element.clone())),
            _ => None,
        })
        .collect()
}

pub(in crate::native) fn use_pointee_coverage(tir: &TirFunction) -> (usize, usize, usize) {
    let conflicts = infer_use_pointees(&tir.blocks).1;
    let resolved = tir.use_pointees.len();
    let beyond_gep = tir
        .use_pointees
        .keys()
        .filter(|k| !tir.pointer_pointees.contains_key(*k))
        .count();
    (resolved, beyond_gep, conflicts)
}
