use super::*;
use crate::native::tir::RetTerm;

pub(in crate::native) const URET_PREFIX: &str = "%metal2vulkan.uret";

#[cfg(test)]
pub(in crate::native) fn unify_returns(blocks: &[BodyBlock]) -> Option<Vec<BodyBlock>> {
    unify_return_like_exits(blocks, false)
}

fn unify_returns_and_unreachable(blocks: &[BodyBlock]) -> Option<Vec<BodyBlock>> {
    unify_return_like_exits(blocks, true)
}

#[derive(Clone)]
enum ReturnLike {
    Return(Option<(String, String)>),
    Unreachable,
}

fn unify_return_like_exits(
    blocks: &[BodyBlock],
    include_unreachable: bool,
) -> Option<Vec<BodyBlock>> {
    let mut exits: Vec<(usize, ReturnLike)> = Vec::new();
    for (i, b) in blocks.iter().enumerate() {
        let Some(t) = &b.typed else { continue };
        match t.ret_term() {
            RetTerm::Void => exits.push((i, ReturnLike::Return(None))),
            RetTerm::Value { ty, val } => exits.push((i, ReturnLike::Return(Some((ty, val))))),
            RetTerm::Unrenderable => return None,
            RetTerm::NotRet
                if include_unreachable
                    && matches!(t.terminator, crate::native::tir::TirTerminator::Unreachable) =>
            {
                exits.push((i, ReturnLike::Unreachable));
            }
            RetTerm::NotRet => {}
        }
    }
    if exits.len() < 2 {
        return None;
    }
    let real_returns = exits.iter().filter_map(|(_, exit)| match exit {
        ReturnLike::Return(value) => Some(value),
        ReturnLike::Unreachable => None,
    });
    let first_return = real_returns.clone().next()?;
    let is_void = first_return.is_none();
    let ret_ty = first_return.as_ref().map(|(ty, _)| ty.clone());
    for value in real_returns {
        if value.is_none() != is_void {
            return None;
        }
        if let (Some(want), Some((ty, _))) = (&ret_ty, value) {
            if ty != want {
                return None;
            }
        }
    }

    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let mut exit = URET_PREFIX.to_string();
    let mut n = 0usize;
    while names.contains(exit.as_str()) {
        exit = format!("{URET_PREFIX}.{n}");
        n += 1;
    }

    let mut out: Vec<BodyBlock> = blocks.to_vec();
    let mut incomings: Vec<String> = Vec::new();
    for (idx, return_like) in &exits {
        match return_like {
            ReturnLike::Return(Some((_, val))) => {
                incomings.push(format!("[ {val}, {} ]", out[*idx].name));
            }
            ReturnLike::Unreachable if !is_void => {
                incomings.push(format!("[ undef, {} ]", out[*idx].name));
            }
            ReturnLike::Return(None) | ReturnLike::Unreachable => {}
        }
        if let Some(t) = &mut out[*idx].typed {
            let t = std::sync::Arc::make_mut(t);
            t.set_unconditional_branch(&exit);
        }
    }

    let exit_lines = if is_void {
        vec!["ret void".to_string()]
    } else {
        let ty = ret_ty.as_ref()?;
        vec![
            format!("{URET_PREFIX}.v = phi {ty} {}", incomings.join(", ")),
            format!("ret {ty} {URET_PREFIX}.v"),
        ]
    };
    let role = role_for_name(&exit);
    let typed = crate::native::tir::lower_block_carrier(&exit, &exit_lines, &HashMap::default());
    out.push(BodyBlock {
        name: exit,
        role,
        typed: typed.map(Into::into),
    });
    Some(out)
}

pub(in crate::native) fn separate_divergent_selection_exits(
    blocks: &[BodyBlock],
) -> Option<Vec<BodyBlock>> {
    unify_returns_and_unreachable(blocks)
}

pub(in crate::native) fn fresh(orig: &str, id: usize) -> String {
    let stripped = orig.strip_prefix('%').unwrap_or(orig);
    format!("%xa{id}_{stripped}")
}

pub(in crate::native) fn clone_source_name(cloned: &str) -> Option<String> {
    let (id, original) = cloned.strip_prefix("%xa")?.split_once('_')?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then(|| format!("%{original}"))
}

pub(in crate::native) fn cloned_labels_overlap_ssa_values(
    blocks: &[BodyBlock],
    labels: &HashSet<String>,
) -> bool {
    for block in blocks {
        let Some(carrier) = block.typed.as_ref() else {
            return true;
        };
        for instruction in &carrier.insts {
            if instruction
                .result
                .as_ref()
                .is_some_and(|result| labels.contains(result))
            {
                return true;
            }
            let mut overlaps = false;
            instruction.visit_uses(|name| overlaps |= labels.contains(name));
            if overlaps {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
pub(in crate::native) fn line_def(line: &str) -> Option<String> {
    let t = line.trim_start();
    if !t.starts_with('%') {
        return None;
    }
    let eq = t.find('=')?;
    let lhs = t[..eq].trim();
    if lhs.contains(char::is_whitespace) || !lhs.starts_with('%') {
        return None;
    }
    Some(lhs.to_string())
}

#[cfg(test)]
pub(in crate::native) fn rebuild_phi(line: &str, keep: impl Fn(&str) -> bool) -> Option<String> {
    let (head, body) = line.split_once("phi ")?;
    let ty_end = body.find('[')?;
    let ty = body[..ty_end].trim_end();
    let rest = &body[ty_end..];
    let mut kept: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let inc = &rest[start..=i];
                    if let Some(pred) = phi_incoming_pred(inc) {
                        if keep(&pred) {
                            kept.push(inc.trim().to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if kept.is_empty() {
        return None;
    }
    Some(format!("{head}phi {ty} {}", kept.join(", ")))
}

#[cfg(test)]
pub(in crate::native) fn phi_incoming_pred(inc: &str) -> Option<String> {
    let inner = inc.trim().strip_prefix('[')?.strip_suffix(']')?;
    let comma = inner.rfind(',')?;
    Some(inner[comma + 1..].trim().to_string())
}

pub(in crate::native) fn rename_tokens(line: &str, map: &HashMap<String, String>) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let mut j = i + 1;
            while j < bytes.len() && is_ident_byte(bytes[j]) {
                j += 1;
            }
            let token = &line[i..j];
            match map.get(token) {
                Some(repl) => out.push_str(repl),
                None => out.push_str(token),
            }
            i = j;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

pub(in crate::native) fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}
