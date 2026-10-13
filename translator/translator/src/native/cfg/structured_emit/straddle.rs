use super::*;

pub(in crate::native) fn structured_plan_inner(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
) -> Option<StructuredPlan> {
    structured_plan_inner4(blocks, converge_inloop, break_aware, false, false)
}

pub(in crate::native) fn structured_plan_inner4(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    multi_exit_clone: bool,
    allow_bare_exit: bool,
) -> Option<StructuredPlan> {
    structured_plan_inner5(
        blocks,
        converge_inloop,
        break_aware,
        multi_exit_clone,
        allow_bare_exit,
        false,
    )
}

pub(in crate::native) fn structured_plan_inner5(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    multi_exit_clone: bool,
    allow_bare_exit: bool,
    loop_exit_selection: bool,
) -> Option<StructuredPlan> {
    structured_plan_inner6(
        blocks,
        converge_inloop,
        break_aware,
        multi_exit_clone,
        allow_bare_exit,
        loop_exit_selection,
        false,
    )
}

pub(in crate::native) fn structured_plan_inner6(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    multi_exit_clone: bool,
    allow_bare_exit: bool,
    loop_exit_selection: bool,
    terminal_exit_selection: bool,
) -> Option<StructuredPlan> {
    structured_plan_inner7(
        blocks,
        converge_inloop,
        break_aware,
        multi_exit_clone,
        allow_bare_exit,
        loop_exit_selection,
        terminal_exit_selection,
        false,
    )
}

pub(in crate::native) fn structured_plan_construct_tree(
    blocks: &[BodyBlock],
) -> Option<StructuredPlan> {
    if let Some(plan) =
        structured_plan_inner7(blocks, false, false, false, true, false, false, true)
    {
        return Some(plan);
    }

    for (converge_inloop, break_aware) in [(true, false), (true, true), (false, false)] {
        if let Some(plan) = structured_plan_inner7(
            blocks,
            converge_inloop,
            break_aware,
            false,
            true,
            true,
            false,
            true,
        ) {
            return Some(plan);
        }
    }

    if let Some(cloned) = privatize_direct_construct_tree_cross_arm(blocks) {
        let shared_private =
            privatize_direct_construct_tree_shared_continuations(&cloned).unwrap_or(cloned);
        for &(converge_inloop, break_aware) in &[(false, false), (true, false), (true, true)] {
            if let Some(plan) = structured_plan_inner7(
                &shared_private,
                converge_inloop,
                break_aware,
                false,
                true,
                false,
                false,
                true,
            ) {
                return Some(plan);
            }
        }
    }
    None
}

fn privatize_direct_construct_tree_cross_arm(blocks: &[BodyBlock]) -> Option<Vec<BodyBlock>> {
    const DIRECT_CROSS_ARM_ROUNDS: usize = 24;
    const DIRECT_CROSS_ARM_GROWTH_CAP: usize = 3000;
    let mut cur = blocks.to_vec();
    let mut changed = false;
    let mut counter = 7_000_000usize;
    let max_blocks = blocks.len().saturating_add(DIRECT_CROSS_ARM_GROWTH_CAP);
    for round in 0..DIRECT_CROSS_ARM_ROUNDS {
        let Some((header, target)) = find_direct_construct_tree_cross_arm(&cur, round > 0) else {
            break;
        };
        let Some(next) =
            clone_crossarm::privatize_dominated_region(&cur, &header, &target, &mut counter)
        else {
            break;
        };
        if next.len() > max_blocks {
            break;
        }
        cur = next;
        changed = true;
    }
    changed.then_some(cur)
}

fn privatize_direct_construct_tree_shared_continuations(
    blocks: &[BodyBlock],
) -> Option<Vec<BodyBlock>> {
    const DIRECT_SHARED_ROUNDS: usize = 64;
    const DIRECT_SHARED_GROWTH_CAP: usize = 4_000;
    let mut cur = blocks.to_vec();
    let mut changed = false;
    let mut counter = 7_500_000usize;
    let max_blocks = blocks.len().saturating_add(DIRECT_SHARED_GROWTH_CAP);
    for _ in 0..DIRECT_SHARED_ROUNDS {
        let mut next = None;
        for (header, continuation) in clone_crossarm::find_deep_shared_continuations(&cur) {
            let Some(cloned) = clone_crossarm::privatize_dominated_region(
                &cur,
                &header,
                &continuation,
                &mut counter,
            ) else {
                continue;
            };
            if cloned.len() > max_blocks {
                continue;
            }
            next = Some(cloned);
            break;
        }
        let Some(cloned) = next else {
            break;
        };
        cur = cloned;
        changed = true;
    }
    changed.then_some(cur)
}

fn find_direct_construct_tree_cross_arm(
    blocks: &[BodyBlock],
    reverse: bool,
) -> Option<(String, String)> {
    let forest = analyze(blocks);
    let loop_headers: HashSet<&str> = forest
        .loops
        .iter()
        .map(|loop_info| loop_info.header.as_str())
        .collect();
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let names = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    let indices: Box<dyn Iterator<Item = usize>> = if reverse {
        Box::new((0..blocks.len()).rev())
    } else {
        Box::new(0..blocks.len())
    };
    for index in indices {
        let block = &blocks[index];
        if loop_headers.contains(block.name.as_str()) {
            continue;
        }
        let Some((true_target, false_target)) = conditional_branch_targets(block) else {
            continue;
        };
        for target in [true_target, false_target] {
            if !names.contains(target.as_str()) || forest.dominates(&block.name, &target) {
                continue;
            }
            let mut child = block.name.as_str();
            while let Some(parent) = forest.idom(child) {
                if loop_headers.contains(parent) {
                    child = parent;
                    continue;
                }
                if let Some(parent_block) = by_name.get(parent) {
                    if let Some((left, right)) = conditional_branch_targets(parent_block) {
                        let sibling = if child == left {
                            Some(right)
                        } else if child == right {
                            Some(left)
                        } else {
                            None
                        };
                        if sibling.is_some_and(|sibling| forest.dominates(&sibling, &target)) {
                            return Some((block.name.clone(), target));
                        }
                    }
                }
                child = parent;
            }
        }
    }
    None
}

fn structured_plan_inner7(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    multi_exit_clone: bool,
    allow_bare_exit: bool,
    loop_exit_selection: bool,
    terminal_exit_selection: bool,
    construct_tree_owned: bool,
) -> Option<StructuredPlan> {
    structured_plan_inner8(
        blocks,
        converge_inloop,
        break_aware,
        multi_exit_clone,
        allow_bare_exit,
        loop_exit_selection,
        terminal_exit_selection,
        construct_tree_owned,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::native) fn structured_plan_inner8(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    multi_exit_clone: bool,
    allow_bare_exit: bool,
    loop_exit_selection: bool,
    terminal_exit_selection: bool,
    construct_tree_owned: bool,
    prepared_terminal: Option<&TerminalExitSelectionPlan>,
) -> Option<StructuredPlan> {
    let spi = crate::env_vars::spi_why();
    let tag = blocks.first().map(|b| b.name.clone()).unwrap_or_default();
    macro_rules! spi_reject {
        ($why:expr) => {
            if spi {
                eprintln!(
                    "[spi-why] fn0={} nblk={} converge={} break_aware={} loop_exit={} REJECT {}",
                    tag,
                    blocks.len(),
                    converge_inloop,
                    break_aware,
                    loop_exit_selection,
                    $why
                );
            }
        };
    }
    let terminal_seed = if terminal_exit_selection && prepared_terminal.is_none() {
        privatize_single_loop_return_exit(blocks)
    } else {
        None
    };
    let terminal_input = terminal_seed.as_deref().unwrap_or(blocks);
    let computed_terminal = if terminal_exit_selection && prepared_terminal.is_none() {
        terminal_exit_selection_merges(terminal_input)
    } else {
        None
    };
    let terminal = prepared_terminal.or(computed_terminal.as_ref());
    let terminal_blocks = terminal
        .map(|plan| plan.blocks.as_slice())
        .unwrap_or(terminal_input);
    let (base_lblocks, mut loop_merges) = super::plan_cache::forest_loop_merges_cached(
        terminal_blocks,
        converge_inloop,
        multi_exit_clone,
    );
    let terminal_dispatch = if terminal_exit_selection {
        terminal_unreachable_selection_merges(&base_lblocks)
    } else {
        None
    };
    if terminal_exit_selection && terminal.is_none() && terminal_dispatch.is_none() {
        spi_reject!("terminal-exit-no-candidate");
        return None;
    }
    let mut lblocks = terminal_dispatch
        .as_ref()
        .map(|plan| plan.blocks.clone())
        .unwrap_or(base_lblocks);
    let mut terminal_merges = HashMap::new();
    if let Some(plan) = terminal {
        terminal_merges.extend(plan.merges.clone());
    }
    if let Some(plan) = &terminal_dispatch {
        terminal_merges.extend(plan.merges.clone());
    }
    if construct_tree_owned {
        if let Some(plan) = direct_terminal_exit_selection_merges(&lblocks, &terminal_merges) {
            lblocks = plan.blocks;
            terminal_merges.extend(plan.merges);
        }
        coalesce_sibling_conditional_dispatches(&mut lblocks);
        privatize_nondominated_loop_merges(&mut lblocks, &mut loop_merges);
    }
    let lforest = analyze(&lblocks);
    for l in &lforest.loops {
        if !loop_merges.contains_key(&l.header) {
            if crate::env_vars::spi_why() {
                eprintln!(
                    "[spi-why]   uncovered-loop header={} latches={:?} exits={:?} parent={:?}",
                    l.header, l.latches, l.exits, l.parent,
                );
            }
            spi_reject!(format!("loop-uncovered header={}", l.header));
            return None;
        }
    }
    let (sblocks, mut branch, mut branch_merges_by_header, switch) =
        super::plan_cache::selection_cached(
            &lblocks,
            &loop_merges,
            &terminal_merges,
            break_aware,
            loop_exit_selection,
            construct_tree_owned,
            || {
                let (mut sblocks, mut branch, mut branch_merges_by_header, switch) =
                    if construct_tree_owned {
                        unique_selection_merges_with_construct_tree_ownership(
                            &lblocks,
                            &loop_merges,
                            break_aware,
                            loop_exit_selection,
                            &terminal_merges,
                        )
                    } else {
                        unique_selection_merges_with_loop_exit_and_forced(
                            &lblocks,
                            &loop_merges,
                            break_aware,
                            loop_exit_selection,
                            &terminal_merges,
                        )
                    };
                if !construct_tree_owned {
                    index_branch_merges_by_header(
                        &sblocks,
                        &loop_merges,
                        &branch,
                        &mut branch_merges_by_header,
                    );
                }
                normalize_continue_selection_merge_targets(
                    &mut sblocks,
                    &loop_merges,
                    &mut branch_merges_by_header,
                );
                if !construct_tree_owned {
                    branch = sblocks
                        .iter()
                        .filter_map(|block| {
                            let (true_target, false_target) = conditional_branch_targets(block)?;
                            let merge = branch_merges_by_header.get(&block.name)?;
                            Some(((true_target, false_target), merge.clone()))
                        })
                        .collect();
                }
                (sblocks, branch, branch_merges_by_header, switch)
            },
        );
    if selection_synth_growth_exceeds_ladder_cap(lblocks.len(), sblocks.len()) {
        spi_reject!(format!(
            "selection-synth-growth nblk={} from={}",
            sblocks.len(),
            lblocks.len()
        ));
        return None;
    }

    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|b| b.name.as_str()).collect();
    let mut header_merge: HashMap<String, String> = HashMap::new();
    for (h, info) in &loop_merges {
        header_merge.insert(h.clone(), info.merge.clone());
    }
    let mut skipped_bare_exit = false;
    for b in &sblocks {
        let is_switch = is_switch_block(b);
        if loop_headers.contains(b.name.as_str()) {
            if is_switch {
                spi_reject!(format!("loop-header-switch header={}", b.name));
                return None;
            }
            continue;
        }
        if is_switch {
            match switch.get(&b.name) {
                Some(m) => header_merge.insert(b.name.clone(), m.clone()),
                None => {
                    spi_reject!(format!("switch-no-merge header={}", b.name));
                    return None;
                }
            };
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
            spi_reject!(format!("no-cond-targets header={}", b.name));
            return None;
        };
        if construct_tree_owned
            && allow_bare_exit
            && (bare_loop_exit_branch_with_passthroughs(
                &sblocks,
                &forest,
                &loop_merges,
                &b.name,
                &t,
                &f,
            ) || bare_enclosing_selection_region_escape(
                &sblocks,
                &forest,
                &branch_merges_by_header,
                &b.name,
                &t,
            ) || bare_enclosing_selection_region_escape(
                &sblocks,
                &forest,
                &branch_merges_by_header,
                &b.name,
                &f,
            ))
        {
            branch.remove(&(t.clone(), f.clone()));
            branch_merges_by_header.remove(&b.name);
            skipped_bare_exit = true;
            continue;
        }
        let merge = match branch_merges_by_header
            .get(&b.name)
            .or_else(|| branch.get(&(t.clone(), f.clone())))
        {
            Some(m) => m.clone(),
            None => {
                if let Some(cont) =
                    loop_break_continue_merge(&forest, &loop_merges, &b.name, &t, &f)
                {
                    cont
                } else if allow_bare_exit
                    && (bare_loop_exit_branch_with_passthroughs(
                        &sblocks,
                        &forest,
                        &loop_merges,
                        &b.name,
                        &t,
                        &f,
                    ) || bare_enclosing_selection_region_escape(
                        &sblocks,
                        &forest,
                        &branch_merges_by_header,
                        &b.name,
                        &t,
                    ) || bare_enclosing_selection_region_escape(
                        &sblocks,
                        &forest,
                        &branch_merges_by_header,
                        &b.name,
                        &f,
                    ))
                {
                    skipped_bare_exit = true;
                    continue;
                } else if construct_tree_owned && allow_bare_exit {
                    skipped_bare_exit = true;
                    continue;
                } else {
                    spi_reject!(format!(
                        "branch-no-merge header={} arms=({},{})",
                        b.name, t, f
                    ));
                    if spi && sblocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS {
                        eprintln!("[spi-why]   --- sblocks skeleton (name -> terminator) ---");
                        for sb in &sblocks {
                            let (phi, term) = sb
                                .typed
                                .as_ref()
                                .map(|t| {
                                    (
                                        t.insts.iter().filter(|i| i.is_phi()).count(),
                                        format!("{:?}", t.terminator),
                                    )
                                })
                                .unwrap_or((0, String::new()));
                            eprintln!("[spi-why]   {:24} phi={} | {}", sb.name, phi, term);
                        }
                        eprintln!(
                            "[spi-why]   branch keys: {:?}",
                            branch.keys().collect::<Vec<_>>()
                        );
                        eprintln!("[spi-why]   loop_merges: {loop_merges:?}");
                    }
                    return None;
                }
            }
        };
        header_merge.insert(b.name.clone(), merge);
    }

    let order = if terminal_exit_selection {
        structured_order_terminal(&sblocks, &forest, |h| header_merge.get(h).cloned())
    } else {
        structured_order(&sblocks, &forest, |h| header_merge.get(h).cloned())
    };
    let rank: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let mut ordered = sblocks;
    ordered.sort_by_key(|b| rank.get(b.name.as_str()).copied().unwrap_or(usize::MAX));

    if let Some(reason) = plan_self_check_reason(&ordered, &header_merge, &loop_merges) {
        let tree_owned_residual = construct_tree_owned
            && matches!(
                reason,
                "selection:cross-arm-shared" | "selection:straddle-loop-merge"
            );
        if !tree_owned_residual {
            spi_reject!(format!("self-check {reason}"));
            return None;
        }
    }

    if construct_tree_owned {
        if let Some(reason) = dominance_loop_exit_escape_reason(&ordered, &loop_merges) {
            spi_reject!(format!("self-check {reason}"));
            return None;
        }
    }
    if (skipped_bare_exit || loop_exit_selection) && !construct_tree_owned {
        if let Some(reason) = bare_exit_escape_reason(&ordered, &header_merge, &loop_merges) {
            spi_reject!(format!("self-check {reason}"));
            return None;
        }
    }
    if let Some(reason) = conflicting_phi_predecessor_reason(&ordered) {
        spi_reject!(format!("self-check {reason}"));
        return None;
    }

    Some(StructuredPlan {
        blocks: ordered,
        loop_merges,
        branch_merges: branch,
        branch_merges_by_header,
        switch_merges: switch,
    })
}

fn conflicting_phi_predecessor_reason(blocks: &[BodyBlock]) -> Option<String> {
    for block in blocks {
        let Some(carrier) = &block.typed else {
            continue;
        };
        for inst in &carrier.insts {
            let Some((_, incoming)) = inst.phi_incoming() else {
                continue;
            };
            let mut value_by_predecessor = HashMap::new();
            for (value, predecessor) in incoming {
                if value_by_predecessor
                    .insert(predecessor.as_str(), value)
                    .is_some_and(|existing| existing != value)
                {
                    return Some(format!(
                        "phi-conflicting-predecessor block={} pred={predecessor}",
                        block.name
                    ));
                }
            }
        }
    }
    None
}

pub(in crate::native) fn restructure_straddle_loop_merges(
    blocks: &[BodyBlock],
) -> Option<Vec<BodyBlock>> {
    restructure_straddle_loop_merges_with(blocks, crate::env_vars::converge_inloop(), false)
}

pub(in crate::native) fn restructure_straddle_loop_merges_with(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
) -> Option<Vec<BodyBlock>> {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, converge_inloop, false);
    let lforest = analyze(&lblocks);
    for l in &lforest.loops {
        if !loop_merges.contains_key(&l.header) {
            return None;
        }
    }
    let (sblocks, branch, switch) = unique_selection_merges(&lblocks, &loop_merges, break_aware);

    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|b| b.name.as_str()).collect();
    let mut header_merge: HashMap<String, String> = HashMap::new();
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

    let mut straddles: Vec<(String, String)> = Vec::new();
    for l in &forest.loops {
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let ml = info.merge.clone();
        for (ch, cm) in &header_merge {
            if ch == &l.header {
                continue;
            }
            let inside = forest.dominates(ch, &l.header) && !forest.dominates(cm, &l.header);
            if inside && forest.dominates(cm, &ml) {
                straddles.push((l.header.clone(), ml));
                break;
            }
        }
    }
    if straddles.is_empty() {
        return None;
    }

    let mut out = blocks.to_vec();
    let mut counter = out.len();
    let mut changed = false;
    for (header, ml) in &straddles {
        let det_forest = analyze(&out);
        if det_forest.loop_for_header(header).is_none() || !out.iter().any(|b| &b.name == ml) {
            continue;
        }
        let split = if block_has_phi(&out, ml) {
            split_phi_overlap(&mut out, &det_forest, header, ml, &mut counter)
        } else {
            split_no_phi_overlap(&mut out, &det_forest, header, ml, &mut counter)
        };
        changed |= split.is_some();
    }
    changed.then_some(out)
}

pub(in crate::native) fn straddle_witness_lines(blocks: &[BodyBlock]) -> Vec<String> {
    let mut out = Vec::new();
    append_straddle_witness_modes("source", blocks, &mut out);
    if let Some(destraddled) = restructure_straddle_loop_merges(blocks) {
        let (region, region_witness) =
            clone_crossarm::privatize_region_cross_arm_with_witness(&destraddled);
        append_straddle_summary_line(
            "source-destraddled",
            &destraddled,
            Some(&region_witness),
            &mut out,
        );
        if blocks_changed(&destraddled, &region) {
            append_straddle_summary_line("source-destraddled-region", &region, None, &mut out);
        }
        append_straddle_witness_modes("source-destraddled", &destraddled, &mut out);
    }
    if let Some(destraddled) = restructure_straddle_loop_merges_with(blocks, true, true) {
        let (region, region_witness) =
            clone_crossarm::privatize_region_cross_arm_with_witness(&destraddled);
        append_straddle_summary_line(
            "source-derived-destraddled",
            &destraddled,
            Some(&region_witness),
            &mut out,
        );
        if blocks_changed(&destraddled, &region) {
            append_straddle_summary_line(
                "source-derived-destraddled-region",
                &region,
                None,
                &mut out,
            );
        }
        append_straddle_witness_modes("source-derived-destraddled", &destraddled, &mut out);
    }

    let deep_shared = privatize_shared_continuations_for_ladder(blocks);
    if blocks_changed(blocks, &deep_shared) {
        append_straddle_witness_modes("deep-shared", &deep_shared, &mut out);
    }

    let trivial = clone_crossarm::privatize_trivial_cross_arm(blocks);
    if blocks_changed(blocks, &trivial) {
        append_straddle_witness_modes("trivial", &trivial, &mut out);
    }

    let region = privatize_region_cross_arm_for_ladder(blocks);
    if blocks_changed(blocks, &region) {
        append_straddle_witness_modes("region", &region, &mut out);
    }

    let lowered_switches = blocks::lower_loop_exit_switches(blocks);
    if blocks_changed(blocks, &lowered_switches) {
        append_straddle_witness_modes("loop-switch-lowered", &lowered_switches, &mut out);
    }

    out.sort();
    out.dedup();
    out
}

fn append_straddle_summary_line(
    graph: &str,
    blocks: &[BodyBlock],
    region_fixpoint: Option<&clone_crossarm::RegionCrossArmFixpointWitness>,
    out: &mut Vec<String>,
) {
    let reason = super::reject::reject_reason_inner(blocks).unwrap_or_else(|| "ADMIT".to_string());
    let raw_clone = clone_crossarm::find_cross_arm(blocks)
        .map(|(header, arm)| clone_crossarm::dominated_region_clone_witness(blocks, &header, &arm));
    let synth_clone = first_synthesized_cross_arm_clone_witness(blocks);
    let raw_fields = clone_witness_fields("raw", raw_clone, None);
    let synth_fields = match synth_clone {
        Some((mode, witness)) => clone_witness_fields("synth", Some(witness), Some(mode)),
        None => clone_witness_fields("synth", None, None),
    };
    let region_fields = region_fixpoint
        .map(region_fixpoint_fields)
        .unwrap_or_default();
    out.push(format!(
        "graph={graph} blocks={} post_split_base_reason={} switch_gate={} {} {}{}",
        blocks.len(),
        reason,
        blocks_contain_multilevel_break_switch(blocks),
        raw_fields,
        synth_fields,
        region_fields,
    ));
}

fn first_synthesized_cross_arm_clone_witness(
    blocks: &[BodyBlock],
) -> Option<((bool, bool), clone_crossarm::DominatedRegionCloneWitness)> {
    for mode in [(false, false), (true, false), (true, true)] {
        let Some((header, arm)) = find_synthesized_cross_arm_shared(blocks, mode.0, mode.1) else {
            continue;
        };
        return Some((
            mode,
            clone_crossarm::dominated_region_clone_witness(blocks, &header, &arm),
        ));
    }
    None
}

fn clone_witness_fields(
    prefix: &str,
    witness: Option<clone_crossarm::DominatedRegionCloneWitness>,
    mode: Option<(bool, bool)>,
) -> String {
    let Some(witness) = witness else {
        return format!("{prefix}_cross_arm=none");
    };
    let mode = mode
        .map(|(converge, break_aware)| {
            format!(" {prefix}_mode=converge:{converge},break_aware:{break_aware}")
        })
        .unwrap_or_default();
    let missing_carrier = witness.first_missing_carrier.as_deref().unwrap_or("none");
    let empty_phi = witness.first_empty_phi_block.as_deref().unwrap_or("none");
    format!(
        "{prefix}_cross_arm={}->{}{} {prefix}_clone_reason={} {prefix}_region_blocks={} {prefix}_region_cap={} {prefix}_boundary_count={} {prefix}_boundary_cap={} {prefix}_boundary_sample=[{}] {prefix}_redirect_count={} {prefix}_external_pred_count={} {prefix}_arm_cycle_pred_count={} {prefix}_missing_carrier={} {prefix}_empty_phi_block={}",
        witness.header,
        witness.arm,
        mode,
        witness.reason,
        witness.region_blocks,
        witness.region_cap,
        witness.boundary_count,
        witness.boundary_cap,
        witness.boundary_sample.join(","),
        witness.redirect_count,
        witness.external_pred_count,
        witness.arm_cycle_pred_count,
        missing_carrier,
        empty_phi,
    )
}

fn region_fixpoint_fields(witness: &clone_crossarm::RegionCrossArmFixpointWitness) -> String {
    let next_blocks = witness
        .next_blocks
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string());
    let candidate = witness
        .stop_candidate
        .clone()
        .map(|candidate| clone_witness_fields("region_stop", Some(candidate), None))
        .unwrap_or_else(|| "region_stop_cross_arm=none".to_string());
    format!(
        " region_fixpoint_stop={} region_fixpoint_rounds={} region_fixpoint_input_blocks={} region_fixpoint_output_blocks={} region_fixpoint_max_blocks={} region_fixpoint_next_blocks={} {}",
        witness.stop_reason,
        witness.rounds,
        witness.input_blocks,
        witness.output_blocks,
        witness.max_blocks,
        next_blocks,
        candidate,
    )
}

fn append_straddle_witness_modes(graph: &str, blocks: &[BodyBlock], out: &mut Vec<String>) {
    for (converge, break_aware) in [(false, false), (true, false), (true, true)] {
        append_straddle_witness_lines(graph, blocks, converge, break_aware, out);
    }
}

fn append_straddle_witness_lines(
    graph: &str,
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
    out: &mut Vec<String>,
) {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, converge_inloop, false);
    let lforest = analyze(&lblocks);
    for loop_info in &lforest.loops {
        if !loop_merges.contains_key(&loop_info.header) {
            return;
        }
    }
    let (sblocks, branch, switch) = unique_selection_merges(&lblocks, &loop_merges, break_aware);
    let forest = analyze(&sblocks);
    let input_forest = analyze(blocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|b| b.name.as_str()).collect();
    let source_names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let sblocks_by_name: HashMap<&str, &BodyBlock> = sblocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect();
    let mut header_merge: HashMap<String, String> = HashMap::new();
    for (h, info) in &loop_merges {
        header_merge.insert(h.clone(), info.merge.clone());
    }
    for b in &sblocks {
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        if is_switch_block(b) {
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

    let multilevel_switches = multilevel_break_switch_witnesses(blocks);
    let switch_gate = !multilevel_switches.is_empty();
    for l in &forest.loops {
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let ml = info.merge.as_str();
        for (ch, cm) in &header_merge {
            if ch == &l.header {
                continue;
            }
            let inside = forest.dominates(ch, &l.header) && !forest.dominates(cm, &l.header);
            if !(inside && forest.dominates(cm, ml)) {
                continue;
            }
            let in_loop_preds = sblocks
                .iter()
                .filter(|candidate| {
                    l.body.iter().any(|node| node == &candidate.name)
                        && block_successors(candidate)
                            .iter()
                            .any(|target| target == ml)
                })
                .map(|candidate| candidate.name.clone())
                .collect::<Vec<_>>();
            let mut pred_sample = in_loop_preds.iter().take(8).cloned().collect::<Vec<_>>();
            if in_loop_preds.len() > pred_sample.len() {
                pred_sample.push("…".to_string());
            }
            let input_preds = input_forest
                .loop_for_header(&l.header)
                .map(|input_loop| {
                    blocks
                        .iter()
                        .filter(|candidate| {
                            input_loop.body.iter().any(|node| node == &candidate.name)
                                && block_successors(candidate)
                                    .iter()
                                    .any(|target| target == ml)
                        })
                        .map(|candidate| candidate.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut input_pred_sample = input_preds.iter().take(8).cloned().collect::<Vec<_>>();
            if input_preds.len() > input_pred_sample.len() {
                input_pred_sample.push("…".to_string());
            }
            let mut switch_sample = multilevel_switches
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            if multilevel_switches.len() > switch_sample.len() {
                switch_sample.push("…".to_string());
            }
            let closure = straddle_closure_stats(&sblocks, &forest, &l.body, ml, ch, cm);
            out.push(format!(
                "graph={graph} blocks={} converge={} break_aware={} switch_gate={} loop={} loop_merge={} loop_merge_role={} loop_merge_in_input={} loop_merge_phi={} loop_continue={} loop_body_blocks={} owner={} owner_kind={} owner_merge={} owner_merge_role={} in_loop_pred_count={} in_loop_preds=[{}] input_split_pred_count={} input_split_preds=[{}] multilevel_switch_count={} multilevel_switches=[{}] ct_owner_arm_blocks={} ct_merge_tail_blocks={} ct_closure_blocks={} ct_closure_edges={} ct_entry_count={} ct_entries=[{}] ct_exit_count={} ct_exits=[{}] ct_exit_phi_incoming_count={} ct_exit_phi_value_count={} ct_exit_phi_pointer_count={} ct_exit_phi_sample=[{}] ct_nonphi_escape_count={} ct_nonphi_pointer_escape_count={} ct_nonphi_escape_sample=[{}] ct_enclosing_selection_count={} ct_enclosing_selection_expanded_blocks={} ct_enclosing_selection_sample=[{}]",
                blocks.len(),
                converge_inloop,
                break_aware,
                switch_gate,
                l.header,
                ml,
                block_role_label(&sblocks_by_name, ml),
                source_names.contains(ml),
                block_has_phi(&sblocks, ml),
                info.continue_target,
                l.body.len(),
                ch,
                header_kind(&sblocks_by_name, &loop_headers, ch),
                cm,
                block_role_label(&sblocks_by_name, cm),
                in_loop_preds.len(),
                pred_sample.join(","),
                input_preds.len(),
                input_pred_sample.join(","),
                multilevel_switches.len(),
                switch_sample.join(","),
                closure.owner_arm_blocks,
                closure.merge_tail_blocks,
                closure.closure_blocks,
                closure.closure_edges,
                closure.entry_count,
                closure.entry_sample.join(","),
                closure.exit_count,
                closure.exit_sample.join(","),
                closure.exit_phi_incoming_count,
                closure.exit_phi_value_count,
                closure.exit_phi_pointer_count,
                closure.exit_phi_sample.join(","),
                closure.nonphi_escape_count,
                closure.nonphi_pointer_escape_count,
                closure.nonphi_escape_sample.join(","),
                closure.enclosing_selection_count,
                closure.enclosing_selection_expanded_blocks,
                closure.enclosing_selection_sample.join(","),
            ));
        }
    }
}

fn header_kind<'a>(
    blocks_by_name: &HashMap<&'a str, &'a BodyBlock>,
    loop_headers: &HashSet<&str>,
    name: &str,
) -> &'static str {
    if loop_headers.contains(name) {
        return "loop";
    }
    let Some(block) = blocks_by_name.get(name) else {
        return "missing";
    };
    if is_switch_block(block) {
        return "switch";
    }
    if conditional_branch_targets(block).is_some() {
        "cond"
    } else {
        "other"
    }
}

fn block_role_label<'a>(
    blocks_by_name: &HashMap<&'a str, &'a BodyBlock>,
    name: &str,
) -> &'static str {
    let Some(block) = blocks_by_name.get(name) else {
        return "missing";
    };
    match block.role {
        BlockRole::Normal => "normal",
        BlockRole::LMerge => "lmerge",
        BlockRole::TerminalExitReturn => "terminal-exit-return",
        BlockRole::SwitchBypass => "switch-bypass",
        BlockRole::ConstructTreeRoute => "construct-tree-route",
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StraddleClosureStats {
    owner_arm_blocks: usize,
    merge_tail_blocks: usize,
    closure_blocks: usize,
    closure_edges: usize,
    entry_count: usize,
    entry_sample: Vec<String>,
    exit_count: usize,
    exit_sample: Vec<String>,
    exit_phi_incoming_count: usize,
    exit_phi_value_count: usize,
    exit_phi_pointer_count: usize,
    exit_phi_sample: Vec<String>,
    nonphi_escape_count: usize,
    nonphi_pointer_escape_count: usize,
    nonphi_escape_sample: Vec<String>,
    enclosing_selection_count: usize,
    enclosing_selection_expanded_blocks: usize,
    enclosing_selection_sample: Vec<String>,
}

fn straddle_closure_stats(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_body: &[String],
    loop_merge: &str,
    owner: &str,
    owner_merge: &str,
) -> StraddleClosureStats {
    let names: HashSet<&str> = blocks.iter().map(|block| block.name.as_str()).collect();
    let mut preds: HashMap<String, Vec<String>> = HashMap::new();
    for block in blocks {
        for succ in block_successors(block) {
            if names.contains(succ.as_str()) {
                preds.entry(succ).or_default().push(block.name.clone());
            }
        }
    }

    let mut reaches_loop_merge = HashSet::new();
    let mut stack = vec![loop_merge.to_string()];
    while let Some(node) = stack.pop() {
        if !reaches_loop_merge.insert(node.clone()) {
            continue;
        }
        if let Some(predecessors) = preds.get(&node) {
            stack.extend(predecessors.iter().cloned());
        }
    }

    let loop_body: HashSet<&str> = loop_body.iter().map(String::as_str).collect();
    let mut owner_arm_blocks = 0usize;
    let mut merge_tail_blocks = 0usize;
    let mut closure = HashSet::new();
    for block in blocks {
        let name = block.name.as_str();
        let in_owner_arm = forest.dominates(owner, name)
            && !forest.dominates(owner_merge, name)
            && reaches_loop_merge.contains(name);
        let in_merge_tail =
            forest.dominates(owner_merge, name) && reaches_loop_merge.contains(name);
        if in_owner_arm {
            owner_arm_blocks += 1;
        }
        if in_merge_tail {
            merge_tail_blocks += 1;
        }
        if in_owner_arm || in_merge_tail || loop_body.contains(name) || name == loop_merge {
            closure.insert(name.to_string());
        }
    }

    let selection_naturals = selection_merges(blocks, forest);
    let mut enclosing_selection_expanded = closure.clone();
    let mut enclosing_selection_sample = Vec::new();
    for block in blocks {
        if conditional_branch_targets(block).is_none() {
            continue;
        }
        let Some(natural) = selection_naturals.get(&block.name) else {
            continue;
        };
        let intersects = closure
            .iter()
            .any(|name| forest.dominates(&block.name, name) && !forest.dominates(natural, name));
        if !intersects {
            continue;
        }
        let before = enclosing_selection_expanded.len();
        for candidate in blocks {
            if forest.dominates(&block.name, &candidate.name)
                && !forest.dominates(natural, &candidate.name)
            {
                enclosing_selection_expanded.insert(candidate.name.clone());
            }
        }
        enclosing_selection_expanded.insert(natural.clone());
        let added = enclosing_selection_expanded.len().saturating_sub(before);
        enclosing_selection_sample.push(format!("{}->{}(+{})", block.name, natural, added));
    }
    enclosing_selection_sample.sort();
    enclosing_selection_sample.dedup();
    let enclosing_selection_count = enclosing_selection_sample.len();
    let mut enclosing_selection_sample = enclosing_selection_sample
        .iter()
        .take(8)
        .cloned()
        .collect::<Vec<_>>();
    if enclosing_selection_count > enclosing_selection_sample.len() {
        enclosing_selection_sample.push("…".to_string());
    }

    let mut closure_edges = 0usize;
    let mut entries = Vec::new();
    let mut exits = Vec::new();
    for block in blocks {
        for succ in block_successors(block) {
            if !names.contains(succ.as_str()) {
                continue;
            }
            let from_inside = closure.contains(&block.name);
            let to_inside = closure.contains(&succ);
            if from_inside && to_inside {
                closure_edges += 1;
            } else if from_inside {
                exits.push(format!("{}->{}", block.name, succ));
            } else if to_inside {
                entries.push(format!("{}->{}", block.name, succ));
            }
        }
    }
    entries.sort();
    entries.dedup();
    let mut entry_sample = entries.iter().take(8).cloned().collect::<Vec<_>>();
    if entries.len() > entry_sample.len() {
        entry_sample.push("…".to_string());
    }
    exits.sort();
    exits.dedup();
    let mut exit_sample = exits.iter().take(8).cloned().collect::<Vec<_>>();
    if exits.len() > exit_sample.len() {
        exit_sample.push("…".to_string());
    }

    let def_ty = blocks
        .iter()
        .filter(|block| closure.contains(&block.name))
        .flat_map(|block| {
            block
                .typed
                .as_ref()
                .into_iter()
                .flat_map(|carrier| &carrier.insts)
                .filter_map(|inst| Some((inst.result.clone()?, inst.result_ty.clone())))
        })
        .collect::<HashMap<_, _>>();
    let is_pointer_def = |name: &str| {
        def_ty
            .get(name)
            .and_then(|ty| ty.as_ref())
            .is_some_and(|ty| matches!(ty, crate::native::ir::LlType::Ptr(_)))
    };

    let mut exit_phi_incoming_count = 0usize;
    let mut exit_phi_value_count = 0usize;
    let mut exit_phi_pointer_count = 0usize;
    let mut exit_phi_sample = Vec::new();
    let mut nonphi_escapes = Vec::new();
    for block in blocks {
        if closure.contains(&block.name) {
            continue;
        }
        let Some(carrier) = &block.typed else {
            continue;
        };
        for inst in &carrier.insts {
            if let Some((_, incoming)) = &inst.phi_incoming() {
                for (value, predecessor) in incoming {
                    if !closure.contains(predecessor) {
                        continue;
                    }
                    exit_phi_incoming_count += 1;
                    let mut locals = Vec::new();
                    collect_llvalue_locals(value, &mut locals);
                    let value_locals = locals
                        .into_iter()
                        .filter(|name| def_ty.contains_key(name))
                        .collect::<Vec<_>>();
                    exit_phi_value_count += value_locals.len();
                    exit_phi_pointer_count += value_locals
                        .iter()
                        .filter(|name| is_pointer_def(name))
                        .count();
                    if exit_phi_sample.len() < 8 {
                        let result = inst.result.as_deref().unwrap_or("_");
                        let values = if value_locals.is_empty() {
                            "const".to_string()
                        } else {
                            value_locals.join("|")
                        };
                        exit_phi_sample.push(format!(
                            "{}:{}<-{}:{values}",
                            block.name, result, predecessor
                        ));
                    }
                }
                continue;
            }
            inst.visit_uses(|used| {
                if def_ty.contains_key(used) {
                    nonphi_escapes.push((used.to_string(), block.name.clone()));
                }
            });
        }
        for used in terminator_uses(carrier) {
            if def_ty.contains_key(&used) {
                nonphi_escapes.push((used, block.name.clone()));
            }
        }
    }
    nonphi_escapes.sort();
    nonphi_escapes.dedup();
    let nonphi_pointer_escape_count = nonphi_escapes
        .iter()
        .filter(|(name, _)| is_pointer_def(name))
        .count();
    let mut nonphi_escape_sample = nonphi_escapes
        .iter()
        .take(8)
        .map(|(name, block)| format!("{name}->{block}"))
        .collect::<Vec<_>>();
    if nonphi_escapes.len() > nonphi_escape_sample.len() {
        nonphi_escape_sample.push("…".to_string());
    }

    StraddleClosureStats {
        owner_arm_blocks,
        merge_tail_blocks,
        closure_blocks: closure.len(),
        closure_edges,
        entry_count: entries.len(),
        entry_sample,
        exit_count: exits.len(),
        exit_sample,
        exit_phi_incoming_count,
        exit_phi_value_count,
        exit_phi_pointer_count,
        exit_phi_sample,
        nonphi_escape_count: nonphi_escapes.len(),
        nonphi_pointer_escape_count,
        nonphi_escape_sample,
        enclosing_selection_count,
        enclosing_selection_expanded_blocks: enclosing_selection_expanded.len(),
        enclosing_selection_sample,
    }
}

fn collect_llvalue_locals(value: &crate::native::ir::LlValue, out: &mut Vec<String>) {
    use crate::native::ir::LlValue;
    match value {
        LlValue::Local(name) => out.push(name.clone()),
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                collect_llvalue_locals(&value.value, out);
            }
        }
        LlValue::Splat(value) => collect_llvalue_locals(&value.value, out),
        LlValue::Gep(gep) => {
            collect_llvalue_locals(&gep.base.value, out);
            for index in &gep.indices {
                collect_llvalue_locals(&index.value, out);
            }
        }
        LlValue::IntToPtr { source, .. } => collect_llvalue_locals(&source.value, out),
        LlValue::Global(_)
        | LlValue::Bool(_)
        | LlValue::Int(_)
        | LlValue::SignedInt(_)
        | LlValue::Hex(_)
        | LlValue::Float(_)
        | LlValue::Float32Bits(_)
        | LlValue::HalfBits(_)
        | LlValue::BFloatBits(_)
        | LlValue::Zero
        | LlValue::Undef => {}
    }
}

fn multilevel_break_switch_witnesses(blocks: &[BodyBlock]) -> Vec<String> {
    let forest = analyze(blocks);
    let mut out = Vec::new();
    for loop_info in &forest.loops {
        let body: HashSet<&str> = loop_info.body.iter().map(String::as_str).collect();
        for block in blocks {
            if block.name == loop_info.header
                || !body.contains(block.name.as_str())
                || !is_switch_block(block)
            {
                continue;
            }
            let mut exits = block_successors(block)
                .into_iter()
                .filter(|target| !body.contains(target.as_str()))
                .collect::<Vec<_>>();
            if exits.is_empty() {
                continue;
            }
            exits.sort();
            exits.dedup();
            out.push(format!(
                "{}:{}=>{}",
                loop_info.header,
                block.name,
                exits.join("|")
            ));
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(in crate::native) fn find_synthesized_cross_arm_shared(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
) -> Option<(String, String)> {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, converge_inloop, false);
    let lforest = analyze(&lblocks);
    for l in &lforest.loops {
        if !loop_merges.contains_key(&l.header) {
            return None;
        }
    }
    let (sblocks, branch, switch) = unique_selection_merges(&lblocks, &loop_merges, break_aware);
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let names: HashSet<&str> = sblocks.iter().map(|b| b.name.as_str()).collect();
    let mut header_merge: HashMap<String, String> = HashMap::new();
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
    let is_enclosing_break = |b: &str, a: &str| -> bool {
        forest.loops.iter().any(|l| {
            l.body.iter().any(|n| n == b)
                && loop_merges
                    .get(&l.header)
                    .is_some_and(|i| i.merge == a || i.continue_target == a)
        })
    };
    let raw: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let raw_forest = analyze(blocks);
    let raw_loop_headers = raw_forest
        .loops
        .iter()
        .map(|natural_loop| natural_loop.header.as_str())
        .collect::<HashSet<_>>();
    let raw_loop_latches = raw_forest
        .loops
        .iter()
        .flat_map(|natural_loop| natural_loop.latches.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let raw_loop_exits = raw_forest
        .loops
        .iter()
        .flat_map(|natural_loop| natural_loop.exits.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let clone_is_loop_local = |owner: &str, continuation: &str| {
        super::clone_crossarm::shared_clone_is_loop_local(
            blocks,
            &raw_forest,
            owner,
            continuation,
            &raw_loop_headers,
            &raw_loop_latches,
            &raw_loop_exits,
        )
    };
    for b in &sblocks {
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let Some(m) = header_merge.get(&b.name) else {
            continue;
        };
        for a in block_successors(b) {
            if &a == m || is_enclosing_break(&b.name, &a) {
                continue;
            }
            if !forest.dominates(&b.name, &a)
                && raw.contains(b.name.as_str())
                && raw.contains(a.as_str())
                && clone_is_loop_local(&b.name, &a)
            {
                return Some((b.name.clone(), a));
            }
        }
    }
    let targets: HashMap<&str, (String, String)> = sblocks
        .iter()
        .filter(|h| !loop_headers.contains(h.name.as_str()))
        .filter_map(|h| {
            let m = header_merge.get(&h.name)?;
            let (t0, t1) = conditional_branch_targets(h)?;
            if &t0 == m || &t1 == m || t0 == t1 {
                return None;
            }
            Some((h.name.as_str(), (t0, t1)))
        })
        .collect();
    for b in &sblocks {
        for s in block_successors(b) {
            if forest.dominates(&b.name, &s) {
                continue;
            }
            let mut child: &str = &b.name;
            while let Some(cur) = forest.idom(child) {
                if let Some((x, y)) = targets.get(cur) {
                    let sibling = if child == x {
                        Some(y)
                    } else if child == y {
                        Some(x)
                    } else {
                        None
                    };
                    if let Some(t1) = sibling {
                        if forest.dominates(t1, &s)
                            && raw.contains(child)
                            && raw.contains(s.as_str())
                            && clone_is_loop_local(child, &s)
                        {
                            return Some((child.to_string(), s));
                        }
                    }
                }
                child = cur;
            }
        }
    }
    None
}

pub(in crate::native) fn privatize_synthesized_cross_arm_shared(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
) -> Vec<BodyBlock> {
    let mut cur: Vec<BodyBlock> = blocks.to_vec();
    let mut counter = 2_000_000usize;
    const ROUNDS: usize = 16;
    let cap = (blocks.len() + 512).min(CROSS_ARM_EDGE_MAX_BLOCKS);
    let source_instructions = typed_instruction_count(blocks);
    let instruction_cap = source_instructions.saturating_mul(4).saturating_add(256);
    for _ in 0..ROUNDS {
        if cur.len() > cap {
            break;
        }
        let Some((header, arm)) =
            find_synthesized_cross_arm_shared(&cur, converge_inloop, break_aware)
        else {
            break;
        };
        let Some(next) =
            super::clone_crossarm::privatize_dominated_region(&cur, &header, &arm, &mut counter)
        else {
            break;
        };
        cur = next;
        if typed_instruction_count(&cur) > instruction_cap {
            return blocks.to_vec();
        }
        if cur.len() > cap {
            break;
        }
    }
    cur
}

fn typed_instruction_count(blocks: &[BodyBlock]) -> usize {
    blocks.iter().fold(0usize, |count, block| {
        count.saturating_add(
            block
                .typed
                .as_ref()
                .map_or(1, |typed| typed.insts.len().saturating_add(1)),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;

    #[test]
    fn straddle_closure_counts_owner_arm_and_merge_tail() {
        let blocks = vec![
            bb("%guard", &["br i1 %c0, label %loop, label %out"]),
            bb("%loop", &["br label %body"]),
            bb("%body", &["br i1 %c1, label %loop, label %selmerge"]),
            bb("%selmerge", &["br label %lmerge"]),
            bb("%lmerge", &["%v = add i32 1, 2", "br label %after"]),
            bb("%out", &["ret void"]),
            bb("%after", &["%x = phi i32 [ %v, %lmerge ]", "ret void"]),
        ];
        let forest = analyze(&blocks);
        let loop_info = forest
            .loop_for_header("%loop")
            .expect("synthetic loop is discovered");
        let stats = straddle_closure_stats(
            &blocks,
            &forest,
            &loop_info.body,
            "%lmerge",
            "%guard",
            "%selmerge",
        );

        assert_eq!(stats.owner_arm_blocks, 3);
        assert_eq!(stats.merge_tail_blocks, 2);
        assert_eq!(stats.closure_blocks, 5);
        assert_eq!(stats.closure_edges, 5);
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.exit_count, 2);
        assert_eq!(stats.exit_phi_incoming_count, 1);
        assert_eq!(stats.exit_phi_value_count, 1);
        assert_eq!(stats.exit_phi_pointer_count, 0);
        assert_eq!(stats.nonphi_escape_count, 0);
        assert_eq!(stats.nonphi_pointer_escape_count, 0);
        assert!(stats.exit_sample.contains(&"%guard->%out".to_string()));
        assert!(stats.exit_sample.contains(&"%lmerge->%after".to_string()));
        assert!(stats
            .exit_phi_sample
            .contains(&"%after:%x<-%lmerge:%v".to_string()));
    }

    #[test]
    fn final_phi_contract_rejects_conflicting_predecessor_values() {
        let blocks = vec![
            bb("%entry", &["br label %merge"]),
            bb(
                "%merge",
                &["%v = phi i32 [ 1, %entry ], [ 2, %entry ]", "ret void"],
            ),
        ];
        assert_eq!(
            conflicting_phi_predecessor_reason(&blocks).as_deref(),
            Some("phi-conflicting-predecessor block=%merge pred=%entry")
        );
    }
}
