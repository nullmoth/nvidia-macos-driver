use super::*;

pub(in crate::native) fn block_defs(b: &BodyBlock) -> Vec<String> {
    b.typed
        .as_ref()
        .map(|t| t.insts.iter().filter_map(|i| i.result.clone()).collect())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct DominatedRegionCloneWitness {
    pub header: String,
    pub arm: String,
    pub reason: &'static str,
    pub region_blocks: usize,
    pub region_cap: usize,
    pub boundary_count: usize,
    pub boundary_cap: usize,
    pub boundary_sample: Vec<String>,
    pub redirect_count: usize,
    pub external_pred_count: usize,
    pub arm_cycle_pred_count: usize,
    pub first_missing_carrier: Option<String>,
    pub first_empty_phi_block: Option<String>,
}

pub(in crate::native) struct DominatedRegionClone {
    pub blocks: Vec<BodyBlock>,
    pub renamed: HashMap<String, String>,
}

pub(in crate::native) fn dominated_region_clone_witness(
    blocks: &[BodyBlock],
    header: &str,
    arm: &str,
) -> DominatedRegionCloneWitness {
    let forest = analyze(blocks);
    let preds = predecessors(blocks);
    let by_name: HashMap<&str, &BodyBlock> = blocks.iter().map(|b| (b.name.as_str(), b)).collect();
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();

    if !by_name.contains_key(arm) {
        return DominatedRegionCloneWitness {
            header: header.to_string(),
            arm: arm.to_string(),
            reason: "arm_missing",
            region_blocks: 0,
            region_cap: 0,
            boundary_count: 0,
            boundary_cap: MAX_REGION_BOUNDARIES,
            boundary_sample: Vec::new(),
            redirect_count: 0,
            external_pred_count: 0,
            arm_cycle_pred_count: 0,
            first_missing_carrier: None,
            first_empty_phi_block: None,
        };
    }

    let mut region: HashSet<String> = HashSet::default();
    let mut boundary: HashSet<String> = HashSet::default();
    let mut stack = vec![arm.to_string()];
    while let Some(n) = stack.pop() {
        if !region.insert(n.clone()) {
            continue;
        }
        let Some(b) = by_name.get(n.as_str()) else {
            continue;
        };
        for s in block_successors(b) {
            if !names.contains(s.as_str()) {
                continue;
            }
            if forest.dominates(arm, &s) {
                if !region.contains(&s) {
                    stack.push(s);
                }
            } else {
                boundary.insert(s);
            }
        }
    }

    let mut boundary_sample = boundary.iter().cloned().collect::<Vec<_>>();
    boundary_sample.sort();
    boundary_sample.truncate(8);

    let region_cap = if boundary.len() == 1 {
        MAX_SINGLE_BOUNDARY_REGION_BLOCKS
    } else {
        MAX_REGION_BLOCKS
    };
    let arm_cycle_pred_count = preds
        .get(arm)
        .into_iter()
        .flatten()
        .filter(|p| region.contains(*p))
        .count();
    let redirect: HashSet<String> = preds
        .get(arm)
        .into_iter()
        .flatten()
        .filter(|p| forest.dominates(header, p))
        .cloned()
        .collect();
    let external_pred_count = preds
        .get(arm)
        .into_iter()
        .flatten()
        .filter(|p| !redirect.contains(*p))
        .count();

    let mut first_missing_carrier = None;
    let mut first_empty_phi_block = None;
    if !boundary.is_empty()
        && boundary.len() <= MAX_REGION_BOUNDARIES
        && region.len() <= region_cap
        && arm_cycle_pred_count == 0
        && !redirect.is_empty()
        && external_pred_count > 0
    {
        for src in blocks.iter().filter(|block| region.contains(&block.name)) {
            let keep = |pred: &str| {
                if src.name == arm {
                    redirect.contains(pred)
                } else {
                    region.contains(pred)
                }
            };
            let Some(src_carrier) = &src.typed else {
                first_missing_carrier = Some(src.name.clone());
                break;
            };
            if src_carrier.insts.iter().any(|inst| {
                inst.phi_incoming().as_ref().is_some_and(|(_, incoming)| {
                    !incoming.is_empty() && !incoming.iter().any(|(_, p)| keep(p))
                })
            }) {
                first_empty_phi_block = Some(src.name.clone());
                break;
            }
        }
    }

    let reason = if boundary.is_empty() {
        "boundary_empty"
    } else if boundary.len() > MAX_REGION_BOUNDARIES {
        "boundary_over_cap"
    } else if region.len() > region_cap {
        "region_over_cap"
    } else if arm_cycle_pred_count > 0 {
        "arm_cycle"
    } else if redirect.is_empty() {
        "redirect_empty"
    } else if external_pred_count == 0 {
        "no_external_pred"
    } else if first_missing_carrier.is_some() {
        "missing_carrier"
    } else if first_empty_phi_block.is_some() {
        "phi_empty_after_partition"
    } else {
        "cloneable"
    };

    DominatedRegionCloneWitness {
        header: header.to_string(),
        arm: arm.to_string(),
        reason,
        region_blocks: region.len(),
        region_cap,
        boundary_count: boundary.len(),
        boundary_cap: MAX_REGION_BOUNDARIES,
        boundary_sample,
        redirect_count: redirect.len(),
        external_pred_count,
        arm_cycle_pred_count,
        first_missing_carrier,
        first_empty_phi_block,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct RegionCrossArmFixpointWitness {
    pub input_blocks: usize,
    pub output_blocks: usize,
    pub max_blocks: usize,
    pub rounds: usize,
    pub stop_reason: &'static str,
    pub next_blocks: Option<usize>,
    pub stop_candidate: Option<DominatedRegionCloneWitness>,
}

#[cfg(test)]
pub(in crate::native) fn privatize_region(
    blocks: &[BodyBlock],
    header: &str,
    arm: &str,
    counter: &mut usize,
) -> Option<Vec<BodyBlock>> {
    let forest = analyze(blocks);
    let preds = predecessors(blocks);
    let by_name: HashMap<&str, &BodyBlock> = blocks.iter().map(|b| (b.name.as_str(), b)).collect();

    let mut region: HashSet<String> = HashSet::default();
    let mut stack = vec![arm.to_string()];
    while let Some(n) = stack.pop() {
        if !region.insert(n.clone()) {
            continue;
        }
        let Some(b) = by_name.get(n.as_str()) else {
            continue;
        };
        for s in block_successors(b) {
            if by_name.contains_key(s.as_str()) && !region.contains(&s) {
                stack.push(s);
            }
        }
    }
    if region.len() > MAX_REGION_BLOCKS {
        return None;
    }

    if preds
        .get(arm)
        .into_iter()
        .flatten()
        .any(|p| region.contains(p))
    {
        return None;
    }

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
    if cloned_labels_overlap_ssa_values(blocks, &region) {
        return None;
    }

    let id = *counter;
    *counter += 1;
    let mut rename: HashMap<String, String> = HashMap::default();
    let ordered_region: Vec<&BodyBlock> = blocks
        .iter()
        .filter(|block| region.contains(&block.name))
        .collect();
    for block in &ordered_region {
        rename.insert(block.name.clone(), fresh(&block.name, id));
        for def in block_defs(block) {
            rename.insert(def.clone(), fresh(&def, id));
        }
    }

    let mut out: Vec<BodyBlock> = blocks.to_vec();

    for b in out.iter_mut() {
        if b.name == arm {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.rebuild_phi_incomings(|pred| !redirect.contains(pred));
            }
        }
    }

    let arm_clone = rename.get(arm).cloned()?;
    for b in out.iter_mut() {
        if redirect.contains(&b.name) {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.redirect_successor(arm, &arm_clone);
            }
        }
    }

    for src in &ordered_region {
        let n = &src.name;
        let clone_name = rename.get(n)?.clone();
        let keep = |pred: &str| {
            if n == arm {
                redirect.contains(pred)
            } else {
                region.contains(pred)
            }
        };
        let Some(src_carrier) = &src.typed else {
            return None;
        };
        for inst in &src_carrier.insts {
            if let Some((_, incoming)) = &inst.phi_incoming() {
                if !incoming.is_empty() && !incoming.iter().any(|(_, p)| keep(p)) {
                    return None;
                }
            }
        }
        let role = role_for_name(&clone_name);
        let mut c = (**src_carrier).clone();
        c.rebuild_phi_incomings(keep);
        c.rename(&rename);
        out.push(BodyBlock {
            name: clone_name,
            role,
            typed: Some(c.into()),
        });
    }

    Some(out)
}

pub(in crate::native) fn privatize_dominated_region_with_renames(
    blocks: &[BodyBlock],
    header: &str,
    arm: &str,
    counter: &mut usize,
) -> Option<DominatedRegionClone> {
    let forest = analyze(blocks);
    let preds = predecessors(blocks);
    let by_name: HashMap<&str, &BodyBlock> = blocks.iter().map(|b| (b.name.as_str(), b)).collect();
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();

    let mut region: HashSet<String> = HashSet::default();
    let mut boundary: HashSet<String> = HashSet::default();
    let mut stack = vec![arm.to_string()];
    while let Some(n) = stack.pop() {
        if !region.insert(n.clone()) {
            continue;
        }
        let Some(b) = by_name.get(n.as_str()) else {
            continue;
        };
        for s in block_successors(b) {
            if !names.contains(s.as_str()) {
                continue;
            }
            if forest.dominates(arm, &s) {
                if !region.contains(&s) {
                    stack.push(s);
                }
            } else {
                boundary.insert(s);
            }
        }
    }
    if boundary.is_empty() || boundary.len() > MAX_REGION_BOUNDARIES {
        return None;
    }
    let region_cap = if boundary.len() == 1 {
        MAX_SINGLE_BOUNDARY_REGION_BLOCKS
    } else {
        MAX_REGION_BLOCKS
    };
    if region.len() > region_cap {
        return None;
    }
    if preds
        .get(arm)
        .into_iter()
        .flatten()
        .any(|p| region.contains(p))
    {
        return None;
    }

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
    if cloned_labels_overlap_ssa_values(blocks, &region) {
        return None;
    }

    let id = *counter;
    *counter += 1;
    let mut rename: HashMap<String, String> = HashMap::default();
    let ordered_region: Vec<&BodyBlock> = blocks
        .iter()
        .filter(|block| region.contains(&block.name))
        .collect();
    for block in &ordered_region {
        rename.insert(block.name.clone(), fresh(&block.name, id));
        for def in block_defs(block) {
            rename.insert(def.clone(), fresh(&def, id));
        }
    }
    let arm_clone = rename.get(arm).cloned()?;

    let mut out: Vec<BodyBlock> = blocks.to_vec();

    for b in out.iter_mut() {
        if b.name == arm {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.rebuild_phi_incomings(|p| !redirect.contains(p));
            }
        }
    }
    for b in out.iter_mut() {
        if redirect.contains(&b.name) {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.redirect_successor(arm, &arm_clone);
            }
        }
    }
    for b in out.iter_mut() {
        if boundary.contains(&b.name) {
            if let Some(t) = &mut b.typed {
                let t = std::sync::Arc::make_mut(t);
                t.mirror_region_incomings(&region, &rename);
            }
        }
    }
    for src in &ordered_region {
        let n = &src.name;
        let clone_name = rename.get(n)?.clone();
        let keep = |pred: &str| {
            if n == arm {
                redirect.contains(pred)
            } else {
                region.contains(pred)
            }
        };
        let Some(src_carrier) = &src.typed else {
            return None;
        };
        for inst in &src_carrier.insts {
            if let Some((_, incoming)) = &inst.phi_incoming() {
                if !incoming.is_empty() && !incoming.iter().any(|(_, p)| keep(p)) {
                    return None;
                }
            }
        }
        let role = role_for_name(&clone_name);
        let mut c = (**src_carrier).clone();
        c.rebuild_phi_incomings(keep);
        c.rename(&rename);
        out.push(BodyBlock {
            name: clone_name,
            role,
            typed: Some(c.into()),
        });
    }
    Some(DominatedRegionClone {
        blocks: out,
        renamed: rename,
    })
}

pub(in crate::native) fn privatize_dominated_region(
    blocks: &[BodyBlock],
    header: &str,
    arm: &str,
    counter: &mut usize,
) -> Option<Vec<BodyBlock>> {
    privatize_dominated_region_with_renames(blocks, header, arm, counter)
        .map(|cloned| cloned.blocks)
}

#[cfg(test)]
pub(in crate::native) fn mirror_region_incomings(
    line: &str,
    region: &HashSet<String>,
    rename: &HashMap<String, String>,
) -> Option<String> {
    let (head, body) = line.split_once("phi ")?;
    let ty_end = body.find('[')?;
    let ty = body[..ty_end].trim_end();
    let rest = &body[ty_end..];
    let mut incomings: Vec<String> = Vec::new();
    let mut added = false;
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
                    let inc = rest[start..=i].trim().to_string();
                    let from_region = phi_incoming_pred(&inc)
                        .map(|p| region.contains(&p))
                        .unwrap_or(false);
                    incomings.push(inc.clone());
                    if from_region {
                        incomings.push(rename_tokens(&inc, rename));
                        added = true;
                    }
                }
            }
            _ => {}
        }
    }
    if !added {
        return None;
    }
    Some(format!("{head}phi {ty} {}", incomings.join(", ")))
}

pub(in crate::native) fn privatize_region_cross_arm(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    let mut cur: Vec<BodyBlock> = blocks.to_vec();
    let mut counter = 0usize;
    let max_blocks = blocks.len().saturating_add(MAX_REGION_CLONE_GROWTH);
    for _ in 0..MAX_ROUNDS {
        if cur.len() >= max_blocks {
            break;
        }
        let Some((header, arm)) = find_cross_arm(&cur) else {
            break;
        };
        let Some(next) = privatize_dominated_region(&cur, &header, &arm, &mut counter) else {
            break;
        };
        if next.len() > max_blocks {
            break;
        }
        cur = next;
    }
    cur
}

pub(in crate::native) fn privatize_region_cross_arm_with_witness(
    blocks: &[BodyBlock],
) -> (Vec<BodyBlock>, RegionCrossArmFixpointWitness) {
    let mut cur: Vec<BodyBlock> = blocks.to_vec();
    let mut counter = 0usize;
    let max_blocks = blocks.len().saturating_add(MAX_REGION_CLONE_GROWTH);
    let mut rounds = 0usize;
    let mut stop_reason = "round_cap";
    let mut next_blocks = None;
    let mut stop_candidate = None;
    for _ in 0..MAX_ROUNDS {
        if cur.len() >= max_blocks {
            stop_reason = "growth_cap_reached";
            break;
        }
        let Some((header, arm)) = find_cross_arm(&cur) else {
            stop_reason = "no_cross_arm";
            break;
        };
        let witness = dominated_region_clone_witness(&cur, &header, &arm);
        if witness.reason != "cloneable" {
            stop_reason = "clone_declined";
            stop_candidate = Some(witness);
            break;
        }
        let Some(next) = privatize_dominated_region(&cur, &header, &arm, &mut counter) else {
            stop_reason = "clone_declined_unmatched";
            stop_candidate = Some(witness);
            break;
        };
        if next.len() > max_blocks {
            stop_reason = "next_over_growth_cap";
            next_blocks = Some(next.len());
            stop_candidate = Some(witness);
            break;
        }
        cur = next;
        rounds += 1;
    }
    let witness = RegionCrossArmFixpointWitness {
        input_blocks: blocks.len(),
        output_blocks: cur.len(),
        max_blocks,
        rounds,
        stop_reason,
        next_blocks,
        stop_candidate,
    };
    (cur, witness)
}

pub(in crate::native) fn privatize_deep_shared_continuations(
    blocks: &[BodyBlock],
) -> Vec<BodyBlock> {
    if blocks.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
        return blocks.to_vec();
    }
    let mut cur = blocks.to_vec();
    let mut counter = DEEP_SHARED_COUNTER_START;
    for _ in 0..MAX_ROUNDS {
        if cur.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
            break;
        }
        let mut next = None;
        for (header, continuation) in find_deep_shared_continuations(&cur) {
            if let Some(cloned) =
                privatize_dominated_region(&cur, &header, &continuation, &mut counter)
            {
                next = Some(cloned);
                break;
            }
        }
        let Some(cloned) = next else {
            break;
        };
        cur = cloned;
    }
    cur
}

pub(in crate::native) fn privatize_shared_phi_exit_predecessors(
    blocks: &[BodyBlock],
) -> Vec<BodyBlock> {
    if blocks.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
        return blocks.to_vec();
    }
    let mut cur = blocks.to_vec();
    let mut counter = SHARED_EXIT_COUNTER_START;
    for _ in 0..MAX_ROUNDS {
        if cur.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
            break;
        }
        let forest = analyze(&cur);
        let selection = selection_merges(&cur, &forest);
        let mut claims: HashMap<&str, usize> = HashMap::default();
        for merge in selection.values() {
            *claims.entry(merge.as_str()).or_default() += 1;
        }
        let preds = predecessors(&cur);
        let mut headers = selection.keys().collect::<Vec<_>>();
        headers.sort_by_key(|header| {
            let mut depth = 0usize;
            let mut node = header.as_str();
            while let Some(parent) = forest.idom(node) {
                depth += 1;
                node = parent;
            }
            std::cmp::Reverse(depth)
        });

        let mut next = None;
        'headers: for header in headers {
            let natural = &selection[header];
            if claims.get(natural.as_str()).copied().unwrap_or(0) < 2
                || forest.dominates(header, natural)
                || !crate::native::cfg::structured_emit::block_has_phi(&cur, natural)
            {
                continue;
            }
            for predecessor in preds.get(natural).into_iter().flatten() {
                if forest.dominates(header, predecessor) {
                    continue;
                }
                let mut incoming = preds.get(predecessor).into_iter().flatten();
                let has_nested = incoming.clone().any(|pred| forest.dominates(header, pred));
                let has_enclosing = incoming.any(|pred| !forest.dominates(header, pred));
                if !has_nested || !has_enclosing {
                    continue;
                }
                if let Some(cloned) =
                    privatize_dominated_region(&cur, header, predecessor, &mut counter)
                {
                    next = Some(cloned);
                    break 'headers;
                }
            }
        }
        let Some(cloned) = next else {
            break;
        };
        cur = cloned;
    }
    cur
}

pub(in crate::native) fn privatize_switch_case_continuations(
    blocks: &[BodyBlock],
) -> Vec<BodyBlock> {
    if blocks.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
        return blocks.to_vec();
    }
    let mut cur = blocks.to_vec();
    let mut counter = SWITCH_CASE_COUNTER_START;
    for _ in 0..MAX_ROUNDS {
        if cur.len() > MAX_DEEP_SHARED_CONTINUATION_BLOCKS {
            break;
        }
        let mut next = None;
        let candidates = find_switch_case_shared_continuations(&cur);
        if crate::env_vars::switch_tail_why() {
            eprintln!("switch-tail candidates: {candidates:?}");
        }
        for (case_root, continuation) in candidates {
            if let Some(cloned) =
                privatize_dominated_region(&cur, &case_root, &continuation, &mut counter)
            {
                next = Some(cloned);
                break;
            }
        }
        let Some(cloned) = next else {
            break;
        };
        cur = cloned;
    }
    cur
}
