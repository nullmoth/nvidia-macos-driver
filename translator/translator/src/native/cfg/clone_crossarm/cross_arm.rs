use super::*;

pub(in crate::native) fn privatize_cross_arm_edge(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    let mut cur: Vec<BodyBlock> = blocks.to_vec();
    let mut counter = 1_000_000usize;
    const EDGE_ROUNDS: usize = 8;
    let cap = blocks.len() + MAX_REGION_BLOCKS * 4;
    for _ in 0..EDGE_ROUNDS {
        if cur.len() > cap {
            break;
        }
        let Some((header, arm)) = find_cross_arm_edge(&cur) else {
            break;
        };
        let Some(next) = privatize_dominated_region(&cur, &header, &arm, &mut counter) else {
            break;
        };
        cur = next;
    }
    cur
}

pub(in crate::native) fn privatize_trivial_cross_arm(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    privatize_trivial_cross_arm_for_headers(blocks, None)
}

#[cfg(test)]
pub(in crate::native) fn privatize_trivial_cross_arm_for_emitted_headers(
    blocks: &[BodyBlock],
    headers: &HashSet<String>,
) -> Vec<BodyBlock> {
    privatize_trivial_cross_arm_for_headers(blocks, Some(headers))
}

fn privatize_trivial_cross_arm_for_headers(
    blocks: &[BodyBlock],
    headers: Option<&HashSet<String>>,
) -> Vec<BodyBlock> {
    let mut cur: Vec<BodyBlock> = blocks.to_vec();
    let mut counter = 0usize;
    for _ in 0..MAX_ROUNDS {
        let Some((header, arm)) = find_trivial_cross_arm_for_headers(&cur, headers) else {
            break;
        };
        let Some(next) = privatize_trivial(&cur, &header, &arm, &mut counter) else {
            break;
        };
        cur = next;
    }
    cur
}

#[cfg(test)]
pub(in crate::native) fn find_trivial_cross_arm(blocks: &[BodyBlock]) -> Option<(String, String)> {
    find_trivial_cross_arm_for_headers(blocks, None)
}

fn find_trivial_cross_arm_for_headers(
    blocks: &[BodyBlock],
    eligible_headers: Option<&HashSet<String>>,
) -> Option<(String, String)> {
    let forest = analyze(blocks);
    let pidom = post_idom(blocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let is_enclosing_break = |b: &str, a: &str| -> bool {
        forest.loops.iter().any(|l| {
            l.body.iter().any(|n| n == b) && (l.header == a || l.exits.iter().any(|e| e == a))
        })
    };
    let by_name: HashMap<&str, &BodyBlock> = blocks.iter().map(|b| (b.name.as_str(), b)).collect();
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    for b in blocks {
        if eligible_headers.is_some_and(|headers| !headers.contains(&b.name)) {
            continue;
        }
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let succs = block_successors(b);
        let distinct: HashSet<&str> = succs
            .iter()
            .map(String::as_str)
            .filter(|t| names.contains(t))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let merge = pidom.get(&b.name).map(String::as_str);
        let mut arms: Vec<&str> = distinct.into_iter().collect();
        arms.sort_unstable();
        for a in arms {
            if Some(a) == merge || is_enclosing_break(&b.name, a) {
                continue;
            }
            if loop_headers.contains(a) {
                continue;
            }
            if forest.dominates(&b.name, a) {
                continue;
            }
            let Some(ablk) = by_name.get(a) else { continue };
            if !is_trivial_passthrough(ablk) || !arm_defs_safe_to_clone(a, ablk, blocks) {
                continue;
            }
            return Some((b.name.clone(), a.to_string()));
        }
    }
    None
}

pub(in crate::native) fn is_trivial_passthrough(b: &BodyBlock) -> bool {
    if block_successors(b).len() != 1 {
        return false;
    }
    b.typed
        .as_ref()
        .is_some_and(|t| matches!(t.terminator, crate::native::tir::TirTerminator::Br(_)))
}

pub(in crate::native) fn arm_defs_safe_to_clone(
    arm: &str,
    arm_block: &BodyBlock,
    blocks: &[BodyBlock],
) -> bool {
    let defs: HashSet<String> = arm_block
        .typed
        .as_ref()
        .map(|t| t.insts.iter().filter_map(|i| i.result.clone()).collect())
        .unwrap_or_default();
    if defs.is_empty() {
        return true;
    }
    for b in blocks {
        if b.name == arm {
            continue;
        }
        let mentions = b.typed.as_ref().is_some_and(|t| {
            t.insts
                .iter()
                .any(|i| !i.is_phi() && i.uses_any(|use_name| defs.contains(use_name)))
                || terminator_mentions(&t.terminator)
                    .iter()
                    .any(|u| defs.contains(u))
        });
        if mentions {
            return false;
        }
    }
    true
}

fn terminator_mentions(term: &crate::native::tir::TirTerminator) -> Vec<String> {
    use crate::native::tir::TirTerminator;
    let mut m: Vec<String> = term.successors().iter().map(|s| s.to_string()).collect();
    match term {
        TirTerminator::BrCond { cond, .. } => m.push(cond.clone()),
        TirTerminator::Switch { selector, .. } => m.push(selector.clone()),
        TirTerminator::Ret(Some(v)) if v.starts_with('%') => m.push(v.clone()),
        _ => {}
    }
    m
}

pub(in crate::native) fn privatize_trivial(
    blocks: &[BodyBlock],
    header: &str,
    arm: &str,
    counter: &mut usize,
) -> Option<Vec<BodyBlock>> {
    let forest = analyze(blocks);
    let preds = predecessors(blocks);
    let redirect: HashSet<String> = preds
        .get(arm)
        .into_iter()
        .flatten()
        .filter(|p| forest.dominates(header, p))
        .cloned()
        .collect();
    if redirect.is_empty() {
        return None;
    }
    let keeps_original = preds
        .get(arm)
        .into_iter()
        .flatten()
        .any(|p| !redirect.contains(p));
    if !keeps_original {
        return None;
    }

    let by_name: HashMap<&str, &BodyBlock> = blocks.iter().map(|b| (b.name.as_str(), b)).collect();
    let arm_block = by_name.get(arm)?;
    if cloned_labels_overlap_ssa_values(blocks, &crate::fxhash::set_from([arm.to_string()])) {
        return None;
    }
    let succ = block_successors(arm_block);
    let s = succ.first()?.clone();

    let id = *counter;
    *counter += 1;
    let mut rename: HashMap<String, String> = HashMap::default();
    rename.insert(arm.to_string(), fresh(arm, id));
    for def in block_defs(arm_block) {
        rename.insert(def.clone(), fresh(&def, id));
    }
    let arm_clone = rename.get(arm).cloned()?;

    let mut out: Vec<BodyBlock> = blocks.to_vec();

    for b in out.iter_mut() {
        if redirect.contains(&b.name) {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.redirect_successor(arm, &arm_clone);
            }
        }
    }

    for b in out.iter_mut() {
        if b.name == arm {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.rebuild_phi_incomings(|p| !redirect.contains(p));
            }
        }
    }

    for b in out.iter_mut() {
        if b.name == s {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.duplicate_phi_incoming(arm, &arm_clone, &rename);
            }
        }
    }

    let role = role_for_name(&arm_clone);
    let typed = arm_block.typed.as_ref().map(|src| {
        let mut c = (**src).clone();
        c.rebuild_phi_incomings(|p| redirect.contains(p));
        c.rename(&rename);
        c.into()
    });
    out.push(BodyBlock {
        name: arm_clone,
        role,
        typed,
    });
    Some(out)
}
