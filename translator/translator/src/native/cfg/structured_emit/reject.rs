use super::*;

pub(in crate::native) fn plan_self_check_reason(
    ordered: &[BodyBlock],
    header_merge: &HashMap<String, String>,
    loop_merges: &HashMap<String, LoopMergeInfo>,
) -> Option<&'static str> {
    let mut merge_owner = HashMap::<&str, &str>::default();
    for (header, info) in loop_merges {
        merge_owner.insert(info.merge.as_str(), header.as_str());
    }
    for (header, merge_block) in header_merge {
        if merge_owner
            .insert(merge_block.as_str(), header.as_str())
            .is_some_and(|owner| owner != header)
        {
            return Some("selection:merge-reused");
        }
    }

    let plan_forest = analyze(ordered);
    for block in ordered {
        for successor in block_successors(block) {
            if plan_forest.dominates(&successor, &block.name)
                && !loop_merges.contains_key(&successor)
            {
                if crate::env_vars::spi_why() {
                    eprintln!(
                        "[spi-why]   backedge-target-unowned block={} -> {}",
                        block.name, successor,
                    );
                }
                return Some("loop:backedge-target-unowned");
            }
        }
    }
    for l in &plan_forest.loops {
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let ml = info.merge.as_str();
        for (ch, cm) in header_merge {
            if ch == &l.header {
                continue;
            }
            let inside =
                plan_forest.dominates(ch, &l.header) && !plan_forest.dominates(cm, &l.header);
            if inside && plan_forest.dominates(cm, ml) {
                if crate::env_vars::straddle_admit() {
                    continue;
                }
                return Some("selection:straddle-loop-merge");
            }
        }
    }

    let plan_loop_headers: HashSet<&str> = plan_forest
        .loops
        .iter()
        .map(|l| l.header.as_str())
        .collect();
    let is_enclosing_break = |b: &str, a: &str| -> bool {
        plan_forest.loops.iter().any(|l| {
            l.body.iter().any(|n| n == b)
                && (l.header == a
                    || loop_merges
                        .get(&l.header)
                        .is_some_and(|i| i.merge == a || i.continue_target == a))
        })
    };

    let selection_merges = header_merge
        .values()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    for block in ordered {
        if !selection_merges.contains(block.name.as_str()) {
            continue;
        }
        for successor in block_successors(block) {
            let mut owner = Some(block.name.as_str());
            while let Some(header) = owner {
                let Some(merge) = header_merge.get(header) else {
                    owner = plan_forest.idom(header);
                    continue;
                };
                if plan_loop_headers.contains(header) || plan_forest.dominates(merge, &block.name) {
                    owner = plan_forest.idom(header);
                    continue;
                }
                let remains_inside = plan_forest.dominates(header, &successor)
                    && !plan_forest.dominates(merge, &successor);
                if successor != *merge
                    && !remains_inside
                    && !is_enclosing_break(&block.name, &successor)
                {
                    if crate::env_vars::spi_why() {
                        eprintln!(
                            "[spi-why]   nested-exit-bypass header={} merge={} block={} -> {}",
                            header, merge, block.name, successor,
                        );
                    }
                    return Some("selection:nested-exit-bypass");
                }
                owner = plan_forest.idom(header);
            }
        }
    }
    for b in ordered {
        if plan_loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let Some(m) = header_merge.get(&b.name) else {
            continue;
        };
        let unreachable_terminal_merge = ordered
            .iter()
            .find(|block| block.name == *m)
            .is_some_and(|block| block.role == BlockRole::LMerge && is_bare_unreachable(block));
        if !plan_forest.dominates(&b.name, m) && !unreachable_terminal_merge {
            if crate::env_vars::spi_why() {
                eprintln!(
                    "[spi-why]   merge-not-dominated header={} merge={}",
                    b.name, m,
                );
            }
            return Some("selection:merge-not-dominated");
        }
        for a in block_successors(b) {
            if &a == m || is_enclosing_break(&b.name, &a) {
                continue;
            }
            if !plan_forest.dominates(&b.name, &a) {
                if crate::env_vars::spi_why() {
                    eprintln!(
                        "[spi-why]   cross-arm-shared header={} arm={} merge={}",
                        b.name, a, m
                    );
                }
                return Some("selection:cross-arm-shared");
            }
        }
    }

    let targets: HashMap<&str, (String, String)> = ordered
        .iter()
        .filter(|h| !plan_loop_headers.contains(h.name.as_str()))
        .filter_map(|h| {
            let m = header_merge.get(&h.name)?;
            let (t0, t1) = conditional_branch_targets(h)?;
            if &t0 == m || &t1 == m || t0 == t1 {
                return None;
            }
            Some((h.name.as_str(), (t0, t1)))
        })
        .collect();
    for b in ordered {
        for s in block_successors(b) {
            if block_ends_in_void_return(ordered, &s) || block_ends_in_unreachable(ordered, &s) {
                continue;
            }
            if plan_forest.dominates(&b.name, &s) {
                continue;
            }
            let mut child: &str = &b.name;
            while let Some(cur) = plan_forest.idom(child) {
                if let Some((x, y)) = targets.get(cur) {
                    let sibling = if child == x {
                        Some(y)
                    } else if child == y {
                        Some(x)
                    } else {
                        None
                    };
                    if let Some(t1) = sibling {
                        if plan_forest.dominates(t1, &s) {
                            if crate::env_vars::spi_why() {
                                eprintln!(
                                        "[spi-why]   cross-arm-edge header={} sibling={} block={} -> {}",
                                        cur, t1, b.name, s
                                    );
                            }
                            return Some("selection:cross-arm-edge");
                        }
                    }
                }
                child = cur;
            }
        }
    }

    None
}

pub(in crate::native) fn block_ends_in_void_return(blocks: &[BodyBlock], name: &str) -> bool {
    let Some(block) = blocks.iter().find(|block| block.name == name) else {
        return false;
    };
    block
        .typed
        .as_ref()
        .is_some_and(|t| matches!(t.terminator, crate::native::tir::TirTerminator::Ret(None)))
}

pub(in crate::native) fn structured_reject_reason(blocks: &[BodyBlock]) -> Option<String> {
    let deep_shared = super::privatize_shared_continuations_for_ladder(blocks);
    if deep_shared.len() != blocks.len() && structured_plan(&deep_shared).is_some() {
        return None;
    }
    let force_converge = crate::env_vars::converge_inloop();
    if structured_plan_inner(blocks, force_converge, false).is_some() {
        return None;
    }
    let privatized = super::clone_crossarm::privatize_trivial_cross_arm(blocks);
    if privatized.len() != blocks.len()
        && structured_plan_inner4(&privatized, force_converge, false, false, false).is_some()
    {
        return None;
    }
    let region = super::privatize_region_cross_arm_for_ladder(blocks);
    if region.len() != blocks.len()
        && structured_plan_inner4(&region, force_converge, false, false, false).is_some()
    {
        return None;
    }
    if structured_plan_inner4(blocks, true, false, false, false).is_some() {
        return None;
    }
    if !switch_gate_excludes(blocks)
        && structured_plan_inner4(blocks, true, true, false, false).is_some()
    {
        return None;
    }
    if !switch_gate_excludes(blocks) {
        if let Some(destraddled) = restructure_straddle_loop_merges(blocks) {
            if structured_plan_inner4(&destraddled, force_converge, false, false, false).is_some()
                || structured_plan_inner4(&destraddled, true, false, false, false).is_some()
                || structured_plan_inner4(&destraddled, true, true, false, false).is_some()
            {
                return None;
            }
        }
    }
    if !switch_gate_excludes(blocks) {
        if let Some(destraddled) = restructure_straddle_loop_merges_with(blocks, true, true) {
            if structured_plan_inner4(&destraddled, true, false, false, false).is_some()
                || structured_plan_inner4(&destraddled, true, true, false, false).is_some()
            {
                return None;
            }
            let region2 = super::privatize_region_cross_arm_for_ladder(&destraddled);
            if region2.len() != destraddled.len()
                && (structured_plan_inner4(&region2, true, false, false, false).is_some()
                    || structured_plan_inner4(&region2, true, true, false, false).is_some())
            {
                return None;
            }
        }
    }
    if !switch_gate_excludes(blocks)
        && region.len() != blocks.len()
        && (structured_plan_inner4(&region, true, false, false, false).is_some()
            || structured_plan_inner4(&region, true, true, false, false).is_some())
    {
        return None;
    }
    if !switch_gate_excludes(blocks)
        && (structured_plan_inner4(blocks, force_converge, false, true, false).is_some()
            || structured_plan_inner4(blocks, true, false, true, false).is_some()
            || structured_plan_inner4(blocks, true, true, true, false).is_some())
    {
        return None;
    }
    if blocks.len() <= LOOP_EXIT_SELECTION_MAX_BLOCKS {
        let lowered_switches = super::blocks::lower_loop_exit_switches(blocks);
        let admits = if blocks_changed(blocks, &lowered_switches) {
            structured_plan_inner5(&lowered_switches, true, false, false, false, true).is_some()
        } else {
            structured_plan_inner5(blocks, true, false, false, false, true).is_some()
        };
        if admits {
            return None;
        }
    } else if structured_plan_inner5(blocks, true, false, false, false, true).is_some() {
        return None;
    }
    if [(false, false), (true, false), (true, true)]
        .into_iter()
        .any(|(converge, break_aware)| {
            structured_plan_inner6(blocks, converge, break_aware, false, false, true, true)
                .is_some()
        })
    {
        return None;
    }
    if [(false, false), (true, false), (true, true)]
        .into_iter()
        .any(|(converge, break_aware)| {
            structured_plan_inner6(blocks, converge, break_aware, false, false, false, true)
                .is_some()
        })
    {
        return None;
    }
    if structured_plan_divergent_exit(blocks).is_some() {
        return None;
    }
    if region.len() != blocks.len() {
        return reject_reason_inner(&region);
    }
    reject_reason_inner(&privatized)
}

pub(in crate::native) fn construct_tree_reject_reason(blocks: &[BodyBlock]) -> Option<String> {
    if structured_plan_construct_tree(blocks).is_some() {
        None
    } else {
        reject_reason_inner(blocks)
    }
}

pub(in crate::native) fn construct_tree_gate_witness_lines(blocks: &[BodyBlock]) -> Vec<String> {
    let mut out = Vec::new();
    if structured_plan_construct_tree(blocks).is_some() {
        out.push(format!("gate=ADMIT blocks={}", blocks.len()));
        return out;
    }
    let (lblocks, loop_merges) = forest_loop_merges(blocks, false, false);
    let lforest = analyze(&lblocks);
    for loop_info in &lforest.loops {
        if !loop_merges.contains_key(&loop_info.header) {
            out.push(format!(
                "gate=loop-uncovered blocks={} header={} loop_body_blocks={}",
                blocks.len(),
                loop_info.header,
                loop_info.body.len(),
            ));
            return out;
        }
    }
    let (sblocks, mut branch, mut branch_by_header, switch) =
        unique_selection_merges_with_construct_tree_ownership(
            &lblocks,
            &loop_merges,
            false,
            false,
            &HashMap::default(),
        );
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|block| block.name.as_str()).collect();
    let natural_merges = selection_merges(&lblocks, &lforest);
    let mut header_merge: HashMap<String, String> = HashMap::default();
    for (header, info) in &loop_merges {
        header_merge.insert(header.clone(), info.merge.clone());
    }
    for block in &sblocks {
        let is_switch = is_switch_block(block);
        if loop_headers.contains(block.name.as_str()) {
            if is_switch {
                out.push(format!(
                    "gate=loop-header-switch blocks={} header={}",
                    blocks.len(),
                    block.name,
                ));
                return out;
            }
            continue;
        }
        if is_switch {
            match switch.get(&block.name) {
                Some(merge) => {
                    header_merge.insert(block.name.clone(), merge.clone());
                }
                None => {
                    let successors = block_successors(block);
                    let mut successor_sample =
                        successors.iter().take(8).cloned().collect::<Vec<_>>();
                    if successors.len() > successor_sample.len() {
                        successor_sample.push("…".to_string());
                    }
                    let mut successor_roles = successors
                        .iter()
                        .filter_map(|successor| {
                            sblocks
                                .iter()
                                .find(|candidate| &candidate.name == successor)
                                .map(|candidate| {
                                    format!("{}:{}", candidate.name, block_role_label(candidate))
                                })
                        })
                        .collect::<Vec<_>>();
                    successor_roles.sort();
                    let mut successor_chains = successors
                        .iter()
                        .filter_map(|successor| {
                            single_successor_chain(&sblocks, successor, 6)
                                .map(|chain| format!("{successor}->{}", chain.join("->")))
                        })
                        .collect::<Vec<_>>();
                    successor_chains.sort();
                    out.push(format!(
                        "gate=switch-no-merge blocks={} header={} successors=[{}] successor_roles=[{}] successor_chains=[{}]",
                        blocks.len(),
                        block.name,
                        successor_sample.join(","),
                        successor_roles.join(","),
                        successor_chains.join(","),
                    ));
                    return out;
                }
            }
            continue;
        }
        let successors = block_successors(block);
        let distinct: HashSet<&str> = successors
            .iter()
            .map(String::as_str)
            .filter(|target| names.contains(target))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let Some((t, f)) = conditional_branch_targets(block) else {
            out.push(format!(
                "gate=no-cond-targets blocks={} header={}",
                blocks.len(),
                block.name,
            ));
            return out;
        };
        if bare_loop_exit_branch_with_passthroughs(
            &sblocks,
            &forest,
            &loop_merges,
            &block.name,
            &t,
            &f,
        ) {
            branch.remove(&(t.clone(), f.clone()));
            branch_by_header.remove(&block.name);
            continue;
        }
        if let Some(merge) = branch_by_header
            .get(&block.name)
            .or_else(|| branch.get(&(t.clone(), f.clone())))
        {
            header_merge.insert(block.name.clone(), merge.clone());
            continue;
        }
        if let Some(merge) = loop_break_continue_merge(&forest, &loop_merges, &block.name, &t, &f) {
            header_merge.insert(block.name.clone(), merge);
            continue;
        }
        let natural = natural_merges
            .get(&block.name)
            .map(String::as_str)
            .unwrap_or("-");
        let has_phi = natural != "-" && block_has_phi(&lblocks, natural);
        let mut dom_preds = lblocks
            .iter()
            .filter(|predecessor| {
                natural != "-"
                    && lforest.dominates(&block.name, &predecessor.name)
                    && block_successors(predecessor)
                        .iter()
                        .any(|successor| successor == natural)
            })
            .map(|predecessor| predecessor.name.clone())
            .collect::<Vec<_>>();
        dom_preds.sort();
        let mut dom_pred_sample = dom_preds.iter().take(8).cloned().collect::<Vec<_>>();
        if dom_preds.len() > dom_pred_sample.len() {
            dom_pred_sample.push("…".to_string());
        }
        let mut current_dom_preds = sblocks
            .iter()
            .filter(|predecessor| {
                natural != "-"
                    && forest.dominates(&block.name, &predecessor.name)
                    && block_successors(predecessor)
                        .iter()
                        .any(|successor| successor == natural)
            })
            .map(|predecessor| predecessor.name.clone())
            .collect::<Vec<_>>();
        current_dom_preds.sort();
        let mut current_dom_pred_sample = current_dom_preds
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>();
        if current_dom_preds.len() > current_dom_pred_sample.len() {
            current_dom_pred_sample.push("…".to_string());
        }
        let mut original_pred_current_chains = dom_preds
            .iter()
            .filter_map(|pred| {
                single_successor_chain(&sblocks, pred, 8)
                    .map(|chain| format!("{pred}->{}", chain.join("->")))
            })
            .collect::<Vec<_>>();
        original_pred_current_chains.sort();
        let mut original_pred_current_chain_sample = original_pred_current_chains
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>();
        if original_pred_current_chains.len() > original_pred_current_chain_sample.len() {
            original_pred_current_chain_sample.push("…".to_string());
        }
        let reverse_present = branch.contains_key(&(f.clone(), t.clone()));
        out.push(format!(
            "gate=branch-no-merge blocks={} sblocks={} header={} arms=({},{}) natural={} natural_phi={} dom_pred_count={} dom_preds=[{}] current_dom_pred_count={} current_dom_preds=[{}] original_pred_current_chains=[{}] branch_reverse_present={}",
            blocks.len(),
            sblocks.len(),
            block.name,
            t,
            f,
            natural,
            has_phi as u8,
            dom_preds.len(),
            dom_pred_sample.join(","),
            current_dom_preds.len(),
            current_dom_pred_sample.join(","),
            original_pred_current_chain_sample.join(","),
            reverse_present,
        ));
        return out;
    }

    let order = structured_order(&sblocks, &forest, |header| {
        header_merge.get(header).cloned()
    });
    let rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect();
    let mut ordered = sblocks.clone();
    ordered.sort_by_key(|block| rank.get(block.name.as_str()).copied().unwrap_or(usize::MAX));
    if let Some(reason) = plan_self_check_reason(&ordered, &header_merge, &loop_merges) {
        if reason == "selection:merge-not-dominated" {
            let ordered_loop_headers: HashSet<&str> = forest
                .loops
                .iter()
                .map(|loop_info| loop_info.header.as_str())
                .collect();
            if let Some((header, merge)) = ordered.iter().find_map(|block| {
                if ordered_loop_headers.contains(block.name.as_str()) {
                    return None;
                }
                let merge = header_merge.get(&block.name)?;
                (!forest.dominates(&block.name, merge)).then_some((&block.name, merge))
            }) {
                let reachable = crate::native::cfg::graph::Cfg::from_blocks(&sblocks)
                    .map(|cfg| cfg.reachable_from(&cfg.entry))
                    .unwrap_or_default();
                let header_idom = forest.idom(header).unwrap_or("-");
                let merge_idom = forest.idom(merge).unwrap_or("-");
                let source_natural = natural_merges
                    .get(header)
                    .map(String::as_str)
                    .unwrap_or("-");
                let final_arms = sblocks
                    .iter()
                    .find(|block| block.name == *header)
                    .and_then(conditional_branch_targets)
                    .map(|(t, f)| format!("({t},{f})"))
                    .unwrap_or_else(|| "(-,-)".to_string());
                let mut preds = sblocks
                    .iter()
                    .filter(|candidate| {
                        block_successors(candidate)
                            .iter()
                            .any(|target| target == merge)
                    })
                    .map(|candidate| candidate.name.clone())
                    .collect::<Vec<_>>();
                preds.sort();
                let mut pred_sample = preds.iter().take(8).cloned().collect::<Vec<_>>();
                if preds.len() > pred_sample.len() {
                    pred_sample.push("…".to_string());
                }
                let mut pred_roles = sblocks
                    .iter()
                    .filter(|candidate| preds.iter().any(|pred| pred == &candidate.name))
                    .map(|candidate| format!("{}:{}", candidate.name, block_role_label(candidate)))
                    .collect::<Vec<_>>();
                pred_roles.sort();
                let mut pred_role_sample = pred_roles.iter().take(8).cloned().collect::<Vec<_>>();
                if pred_roles.len() > pred_role_sample.len() {
                    pred_role_sample.push("…".to_string());
                }
                let mut pred_sources = preds
                    .iter()
                    .map(|pred| {
                        let mut sources = sblocks
                            .iter()
                            .filter(|candidate| {
                                block_successors(candidate)
                                    .iter()
                                    .any(|target| target == pred)
                            })
                            .map(|candidate| {
                                format!("{}:{}", candidate.name, block_role_label(candidate))
                            })
                            .collect::<Vec<_>>();
                        sources.sort();
                        format!("{pred}<-[{}]", sources.join("|"))
                    })
                    .collect::<Vec<_>>();
                pred_sources.sort();
                let mut pred_source_sample =
                    pred_sources.iter().take(8).cloned().collect::<Vec<_>>();
                if pred_sources.len() > pred_source_sample.len() {
                    pred_source_sample.push("…".to_string());
                }
                out.push(format!(
                    "gate=self-check blocks={} reason={reason} header={} merge={} arms={} source_natural={} header_reachable={} merge_reachable={} header_idom={} merge_idom={} merge_pred_count={} merge_preds=[{}] merge_pred_roles=[{}] merge_pred_sources=[{}]",
                    blocks.len(),
                    header,
                    merge,
                    final_arms,
                    source_natural,
                    reachable.contains(header) as u8,
                    reachable.contains(merge) as u8,
                    header_idom,
                    merge_idom,
                    preds.len(),
                    pred_sample.join(","),
                    pred_role_sample.join(","),
                    pred_source_sample.join(","),
                ));
            } else {
                out.push(format!(
                    "gate=self-check blocks={} reason={reason}",
                    blocks.len()
                ));
            }
        } else {
            out.push(format!(
                "gate=self-check blocks={} reason={reason}",
                blocks.len()
            ));
        }
    } else {
        out.push(format!(
            "gate=unknown blocks={} sblocks={} header_merges={}",
            blocks.len(),
            sblocks.len(),
            header_merge.len(),
        ));
    }
    out
}

pub(in crate::native) fn cond_other_witness_lines(blocks: &[BodyBlock]) -> Vec<String> {
    let mut out = Vec::new();
    append_cond_other_witness_lines("source", blocks, &mut out);

    let deep_shared = super::privatize_shared_continuations_for_ladder(blocks);
    if blocks_changed(blocks, &deep_shared) {
        append_cond_other_witness_lines("deep-shared", &deep_shared, &mut out);
    }

    let privatized = super::clone_crossarm::privatize_trivial_cross_arm(blocks);
    if blocks_changed(blocks, &privatized) {
        append_cond_other_witness_lines("trivial", &privatized, &mut out);
    }

    let region = super::privatize_region_cross_arm_for_ladder(blocks);
    if blocks_changed(blocks, &region) {
        append_cond_other_witness_lines("region", &region, &mut out);
    }

    out.sort();
    out.dedup();
    out
}

pub(in crate::native) fn cond_phi_shared_witness_lines(blocks: &[BodyBlock]) -> Vec<String> {
    let mut out = Vec::new();
    append_cond_phi_shared_witness_lines("source", blocks, &mut out);
    out.sort();
    out.dedup();
    out
}

fn append_cond_other_witness_lines(graph: &str, blocks: &[BodyBlock], out: &mut Vec<String>) {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, false, false);
    let lforest = analyze(&lblocks);
    let (sblocks, branch, _switch) = unique_selection_merges(&lblocks, &loop_merges, false);
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|block| block.name.as_str()).collect();
    let natural_merges = selection_merges(&lblocks, &lforest);

    for block in &sblocks {
        if loop_headers.contains(block.name.as_str()) || is_switch_block(block) {
            continue;
        }
        let succs = block_successors(block);
        let distinct: HashSet<&str> = succs
            .iter()
            .map(String::as_str)
            .filter(|target| names.contains(target))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let Some((t, f)) = conditional_branch_targets(block) else {
            continue;
        };
        let looped_arms = lblocks
            .iter()
            .find(|candidate| candidate.name == block.name)
            .and_then(conditional_branch_targets)
            .map(|(lt, lf)| format!("({lt},{lf})"))
            .unwrap_or_else(|| "(-,-)".to_string());
        if branch.contains_key(&(t.clone(), f.clone())) {
            continue;
        }
        let Some(natural) = natural_merges.get(&block.name) else {
            continue;
        };
        if block_has_phi(&lblocks, natural) {
            continue;
        }
        let mut dom_preds = lblocks
            .iter()
            .filter(|predecessor| {
                lforest.dominates(&block.name, &predecessor.name)
                    && block_successors(predecessor)
                        .iter()
                        .any(|successor| successor == natural)
            })
            .map(|predecessor| predecessor.name.clone())
            .collect::<Vec<_>>();
        if dom_preds.is_empty() {
            continue;
        }
        dom_preds.sort();

        let enclosing_loop = lforest
            .loops
            .iter()
            .filter(|loop_info| loop_info.body.iter().any(|node| node == &block.name))
            .min_by_key(|loop_info| loop_info.body.len());
        let (loop_header, loop_merge, loop_continue, loop_body_count, exit_switches, arm_roles) =
            if let Some(loop_info) = enclosing_loop {
                let body: HashSet<&str> = loop_info.body.iter().map(String::as_str).collect();
                let info = loop_merges.get(&loop_info.header);
                let mut switches = lblocks
                    .iter()
                    .filter(|candidate| {
                        candidate.name != loop_info.header
                            && body.contains(candidate.name.as_str())
                            && is_switch_block(candidate)
                    })
                    .filter_map(|candidate| {
                        let mut exits = block_successors(candidate)
                            .into_iter()
                            .filter(|successor| !body.contains(successor.as_str()))
                            .collect::<Vec<_>>();
                        if exits.is_empty() {
                            return None;
                        }
                        exits.sort();
                        Some(format!("{}=>{}", candidate.name, exits.join("|")))
                    })
                    .collect::<Vec<_>>();
                switches.sort();
                (
                    loop_info.header.as_str(),
                    info.map(|info| info.merge.as_str()).unwrap_or("-"),
                    info.map(|info| info.continue_target.as_str())
                        .unwrap_or("-"),
                    loop_info.body.len(),
                    switches,
                    info.map(|info| {
                        format!(
                            "({},{})",
                            cond_other_target_role(&sblocks, &t, info),
                            cond_other_target_role(&sblocks, &f, info)
                        )
                    })
                    .unwrap_or_else(|| "(-,-)".to_string()),
                )
            } else {
                ("-", "-", "-", 0, Vec::new(), "(-,-)".to_string())
            };
        let mut dom_pred_sample = dom_preds.iter().take(8).cloned().collect::<Vec<_>>();
        if dom_preds.len() > dom_pred_sample.len() {
            dom_pred_sample.push("…".to_string());
        }
        let mut switch_sample = exit_switches.iter().take(8).cloned().collect::<Vec<_>>();
        if exit_switches.len() > switch_sample.len() {
            switch_sample.push("…".to_string());
        }
        out.push(format!(
            "graph={graph} blocks={} header={} looped_arms={} arms=({},{}) arm_roles={} natural={} dom_pred_count={} dom_preds=[{}] loop={} loop_merge={} loop_continue={} loop_body_blocks={} exit_switch_count={} exit_switches=[{}]",
            blocks.len(),
            block.name,
            looped_arms,
            t,
            f,
            arm_roles,
            natural,
            dom_preds.len(),
            dom_pred_sample.join(","),
            loop_header,
            loop_merge,
            loop_continue,
            loop_body_count,
            exit_switches.len(),
            switch_sample.join(","),
        ));
    }
}

fn append_cond_phi_shared_witness_lines(graph: &str, blocks: &[BodyBlock], out: &mut Vec<String>) {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, false, false);
    let lforest = analyze(&lblocks);
    let (sblocks, branch, _switch) = unique_selection_merges(&lblocks, &loop_merges, false);
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|block| block.name.as_str()).collect();
    let natural_merges = selection_merges(&lblocks, &lforest);

    for block in &sblocks {
        if loop_headers.contains(block.name.as_str()) || is_switch_block(block) {
            continue;
        }
        let succs = block_successors(block);
        let distinct: HashSet<&str> = succs
            .iter()
            .map(String::as_str)
            .filter(|target| names.contains(target))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let Some((t, f)) = conditional_branch_targets(block) else {
            continue;
        };
        if branch.contains_key(&(t.clone(), f.clone())) {
            continue;
        }
        let Some(natural) = natural_merges.get(&block.name) else {
            continue;
        };
        if !block_has_phi(&lblocks, natural) {
            continue;
        }

        let mut dom_preds = lblocks
            .iter()
            .filter(|predecessor| {
                lforest.dominates(&block.name, &predecessor.name)
                    && block_successors(predecessor)
                        .iter()
                        .any(|successor| successor == natural)
            })
            .map(|predecessor| predecessor.name.clone())
            .collect::<Vec<_>>();
        dom_preds.sort();
        let loop_role = loop_merges.iter().find_map(|(header, info)| {
            if info.merge.as_str() == natural.as_str() {
                Some((header.as_str(), "merge"))
            } else if info.continue_target.as_str() == natural.as_str() {
                Some((header.as_str(), "continue"))
            } else {
                None
            }
        });
        let sibling_claims = natural_merges
            .values()
            .filter(|merge| merge.as_str() == natural.as_str())
            .count();
        let class = if dom_preds.is_empty() {
            "selection:cond-phi-shared/own-arm".to_string()
        } else if let Some((loop_header, role)) = loop_role {
            let in_loop = lforest
                .loops
                .iter()
                .find(|loop_info| loop_info.header.as_str() == loop_header)
                .map(|loop_info| loop_info.body.iter().any(|name| name == &block.name))
                .unwrap_or(false);
            let site = if in_loop { "inloop" } else { "outer" };
            format!("selection:cond-phi-shared/loop-role/{role}-{site}")
        } else if sibling_claims > 1 {
            "selection:cond-phi-shared/sibling".to_string()
        } else {
            "selection:cond-phi-shared/uncollided".to_string()
        };

        let mut dom_pred_sample = dom_preds.iter().take(8).cloned().collect::<Vec<_>>();
        if dom_preds.len() > dom_pred_sample.len() {
            dom_pred_sample.push("…".to_string());
        }
        let (phi_count, phi_sample) = phi_summary(&lblocks, natural);
        let (loop_role_header, loop_role_kind) = loop_role.unwrap_or(("-", "-"));
        out.push(format!(
            "graph={graph} blocks={} class={class} header={} arms=({},{}) natural={} natural_phi_count={} natural_phi_sample=[{}] dom_pred_count={} dom_preds=[{}] sibling_claims={} loop_role_header={} loop_role_kind={}",
            blocks.len(),
            block.name,
            t,
            f,
            natural,
            phi_count,
            phi_sample.join(","),
            dom_preds.len(),
            dom_pred_sample.join(","),
            sibling_claims,
            loop_role_header,
            loop_role_kind,
        ));
    }
}

fn phi_summary(blocks: &[BodyBlock], name: &str) -> (usize, Vec<String>) {
    let Some(block) = blocks.iter().find(|block| block.name == name) else {
        return (0, Vec::new());
    };
    let Some(carrier) = &block.typed else {
        return (0, Vec::new());
    };
    let phis = carrier
        .insts
        .iter()
        .filter(|inst| inst.is_phi())
        .collect::<Vec<_>>();
    let mut sample = phis
        .iter()
        .take(8)
        .map(|inst| {
            inst.result
                .clone()
                .unwrap_or_else(|| "<no-result>".to_string())
        })
        .collect::<Vec<_>>();
    if phis.len() > sample.len() {
        sample.push("…".to_string());
    }
    (phis.len(), sample)
}

fn single_successor_chain(blocks: &[BodyBlock], start: &str, limit: usize) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = HashSet::default();
    let mut current = start.to_string();
    for _ in 0..limit {
        let block = blocks.iter().find(|block| block.name == current)?;
        let successors = block_successors(block);
        if successors.len() != 1 {
            if !successors.is_empty() {
                out.push(format!("{{{}}}", successors.join("|")));
            }
            return Some(out);
        }
        let next = successors[0].clone();
        out.push(next.clone());
        if !seen.insert(next.clone()) {
            out.push("cycle".to_string());
            return Some(out);
        }
        current = next;
    }
    out.push("…".to_string());
    Some(out)
}

fn block_role_label(block: &BodyBlock) -> &'static str {
    match block.role {
        BlockRole::Normal => "ordinary",
        BlockRole::LMerge => "lmerge",
        BlockRole::TerminalExitReturn => "terminal-exit-return",
        BlockRole::SwitchBypass => "switch-bypass",
        BlockRole::ConstructTreeRoute => "construct-tree-route",
    }
}

fn cond_other_target_role(blocks: &[BodyBlock], target: &str, loop_info: &LoopMergeInfo) -> String {
    if target == loop_info.merge {
        return "loop-merge".to_string();
    }
    if target == loop_info.continue_target {
        return "loop-continue".to_string();
    }

    let Some(block) = blocks.iter().find(|block| block.name == target) else {
        return "unknown".to_string();
    };
    let role = block_role_label(block);
    if block.role != BlockRole::LMerge {
        return role.to_string();
    }

    let mut successors = block_successors(block);
    successors.sort();
    successors.dedup();
    if successors.len() != 1 {
        return format!("lmerge->{}", successors.join("|"));
    }
    let successor = &successors[0];
    if successor == &loop_info.merge {
        "lmerge->loop-merge".to_string()
    } else if successor == &loop_info.continue_target {
        "lmerge->loop-continue".to_string()
    } else {
        format!("lmerge->{successor}")
    }
}

pub(in crate::native) fn reject_reason_inner(blocks: &[BodyBlock]) -> Option<String> {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, false, false);
    let lforest = analyze(&lblocks);
    let plans = lforest.structured_plan();
    for l in &lforest.loops {
        if !loop_merges.contains_key(&l.header) {
            let kinds = plans
                .iter()
                .find(|p| p.header == l.header)
                .map(|p| {
                    if p.restructure.is_empty() {
                        "loop-uncovered".to_string()
                    } else {
                        let mut k: Vec<String> =
                            p.restructure.iter().map(|r| format!("{r:?}")).collect();
                        k.sort();
                        let mut reason = format!("loop:{}", k.join("+"));
                        if p.restructure.contains(&Restructure::MultipleExits) {
                            let mut exits = l.exits.clone();
                            exits.sort();
                            exits.dedup();
                            let phi = exits.iter().any(|e| block_has_phi(&lblocks, e));
                            reason.push_str(&format!("[k={},phi={}]", exits.len(), phi as u8));
                        }
                        reason
                    }
                })
                .unwrap_or_else(|| "loop-uncovered".to_string());
            return Some(kinds);
        }
    }
    let (sblocks, branch, switch) = unique_selection_merges(&lblocks, &loop_merges, false);
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|b| b.name.as_str()).collect();
    for b in &sblocks {
        let is_switch = is_switch_block(b);
        if loop_headers.contains(b.name.as_str()) {
            if is_switch {
                return Some("loop:loop-header-switch".to_string());
            }
            continue;
        }
        if is_switch {
            if !switch.contains_key(&b.name) {
                return Some("selection:switch-no-merge".to_string());
            }
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
        let Some((t, f)) = conditional_branch_targets(b) else {
            return Some("selection:no-targets".to_string());
        };
        if !branch.contains_key(&(t, f)) {
            let nat_forest = analyze(&lblocks);
            let sel = selection_merges(&lblocks, &nat_forest);
            let Some(natural) = sel.get(&b.name) else {
                return Some("selection:cond-no-natural".to_string());
            };
            if block_has_phi(&lblocks, natural) {
                let has_dom_pred = lblocks.iter().any(|p| {
                    nat_forest.dominates(&b.name, &p.name)
                        && block_successors(p).iter().any(|s| s == natural)
                });
                let loop_role = loop_merges.iter().find_map(|(h, i)| {
                    if i.merge.as_str() == natural.as_str() {
                        Some((h.as_str(), "merge"))
                    } else if i.continue_target.as_str() == natural.as_str() {
                        Some((h.as_str(), "continue"))
                    } else {
                        None
                    }
                });
                let sibling_claims = sel
                    .values()
                    .filter(|m| m.as_str() == natural.as_str())
                    .count();
                if !has_dom_pred {
                    return Some("selection:cond-phi-shared/own-arm".to_string());
                }
                if let Some((loop_h, role)) = loop_role {
                    let in_loop = nat_forest
                        .loops
                        .iter()
                        .find(|l| l.header.as_str() == loop_h)
                        .map(|l| l.body.iter().any(|n| n.as_str() == b.name.as_str()))
                        .unwrap_or(false);
                    let site = if in_loop { "inloop" } else { "outer" };
                    return Some(format!("selection:cond-phi-shared/loop-role/{role}-{site}"));
                }
                if sibling_claims > 1 {
                    return Some("selection:cond-phi-shared/sibling".to_string());
                }
                return Some("selection:cond-phi-shared/uncollided".to_string());
            }
            let has_dom_pred = lblocks.iter().any(|p| {
                nat_forest.dominates(&b.name, &p.name)
                    && block_successors(p).iter().any(|s| s == natural)
            });
            if !has_dom_pred {
                return Some("selection:cond-shared-arm".to_string());
            }
            return Some("selection:cond-other".to_string());
        }
    }

    let mut header_merge: HashMap<String, String> = HashMap::default();
    for (h, info) in &loop_merges {
        header_merge.insert(h.clone(), info.merge.clone());
    }
    for b in &sblocks {
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let is_switch = is_switch_block(b);
        if is_switch {
            if let Some(m) = switch.get(&b.name) {
                header_merge.insert(b.name.clone(), m.clone());
            }
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
        if let Some((t, f)) = conditional_branch_targets(b) {
            if let Some(m) = branch.get(&(t, f)) {
                header_merge.insert(b.name.clone(), m.clone());
            }
        }
    }
    let order = structured_order(&sblocks, &forest, |h| header_merge.get(h).cloned());
    let rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut ordered = sblocks.clone();
    ordered.sort_by_key(|b| rank.get(b.name.as_str()).copied().unwrap_or(usize::MAX));
    plan_self_check_reason(&ordered, &header_merge, &loop_merges).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb_role as bb;
    use super::*;

    #[test]
    fn plan_self_check_rejects_merge_claimed_by_two_headers() {
        let blocks = vec![
            bb(
                "%outer",
                BlockRole::Normal,
                &["br i1 %a, label %inner, label %merge"],
            ),
            bb(
                "%inner",
                BlockRole::Normal,
                &["br i1 %b, label %work, label %merge"],
            ),
            bb("%work", BlockRole::Normal, &["br label %merge"]),
            bb("%merge", BlockRole::Normal, &["ret void"]),
        ];
        let header_merges = crate::fxhash::map_from([
            ("%outer".to_string(), "%merge".to_string()),
            ("%inner".to_string(), "%merge".to_string()),
        ]);

        assert_eq!(
            plan_self_check_reason(&blocks, &header_merges, &HashMap::default()),
            Some("selection:merge-reused")
        );
    }

    #[test]
    fn plan_self_check_rejects_child_merge_bypassing_parent_selection_merge() {
        let blocks = vec![
            bb(
                "%outer",
                BlockRole::Normal,
                &["br i1 %a, label %nested, label %live"],
            ),
            bb(
                "%nested",
                BlockRole::Normal,
                &["br i1 %b, label %inner, label %nested_merge"],
            ),
            bb(
                "%inner",
                BlockRole::Normal,
                &["br i1 %c, label %work, label %nested_merge"],
            ),
            bb("%work", BlockRole::Normal, &["br label %inner_merge"]),
            bb("%inner_merge", BlockRole::LMerge, &["br label %done"]),
            bb("%nested_merge", BlockRole::LMerge, &["br label %live"]),
            bb("%live", BlockRole::Normal, &["br label %done"]),
            bb("%done", BlockRole::Normal, &["ret void"]),
        ];
        let header_merges = crate::fxhash::map_from([
            ("%outer".to_string(), "%done".to_string()),
            ("%nested".to_string(), "%nested_merge".to_string()),
            ("%inner".to_string(), "%inner_merge".to_string()),
        ]);

        assert_eq!(
            plan_self_check_reason(&blocks, &header_merges, &HashMap::default()),
            Some("selection:nested-exit-bypass")
        );
    }

    #[test]
    fn plan_self_check_rejects_backedge_without_loop_header_ownership() {
        let blocks = vec![
            bb("%entry", BlockRole::Normal, &["br label %header"]),
            bb(
                "%header",
                BlockRole::Normal,
                &["br i1 %condition, label %body, label %exit"],
            ),
            bb("%body", BlockRole::Normal, &["br label %header"]),
            bb("%exit", BlockRole::Normal, &["ret void"]),
        ];

        assert_eq!(
            plan_self_check_reason(&blocks, &HashMap::default(), &HashMap::default()),
            Some("loop:backedge-target-unowned")
        );
    }

    #[test]
    fn cond_other_target_role_reports_synthetic_loop_routes() {
        let blocks = vec![
            bb("%lm", BlockRole::LMerge, &["br label %merge"]),
            bb("%cont", BlockRole::Normal, &["br label %head"]),
            bb("%merge", BlockRole::Normal, &["ret void"]),
        ];
        let loop_info = LoopMergeInfo {
            merge: "%merge".to_string(),
            continue_target: "%cont".to_string(),
        };

        assert_eq!(
            cond_other_target_role(&blocks, "%lm", &loop_info),
            "lmerge->loop-merge"
        );
        assert_eq!(
            cond_other_target_role(&blocks, "%cont", &loop_info),
            "loop-continue"
        );
    }
}
