use super::*;
use crate::native::cfg::loopforest::post_idom;

pub(in crate::native) const SPLIT_PREFIX: &str = "%metal2vulkan.lmerge.";

pub(in crate::native) const TEXITRET_TOKEN: &str = "texitret";
pub(in crate::native) const TLOOPRET_TOKEN: &str = "tloopret";
pub(in crate::native) const SEL_TOKEN: &str = "sel";

pub(in crate::native) const CONT_PREFIX: &str = "%metal2vulkan.cont.";
pub(in crate::native) const EXIT_EDGE_PREFIX: &str = "%metal2vulkan.exitedge.";
pub(in crate::native) const EXIT_SEL_PREFIX: &str = "%metal2vulkan.exitsel.";

pub(in crate::native) fn is_terminal_exit_return_name(name: &str) -> bool {
    name.strip_prefix(SPLIT_PREFIX)
        .is_some_and(|rest| rest.starts_with(TEXITRET_TOKEN))
}

pub(in crate::native) fn role_for_name(name: &str) -> BlockRole {
    if is_terminal_exit_return_name(name) {
        BlockRole::TerminalExitReturn
    } else if name.starts_with(SPLIT_PREFIX) {
        BlockRole::LMerge
    } else if name.starts_with(super::super::blocks::SWITCH_BYPASS_PREFIX) {
        BlockRole::SwitchBypass
    } else {
        BlockRole::Normal
    }
}

pub(in crate::native) struct StructuredPlan {
    pub(in crate::native) blocks: Vec<BodyBlock>,
    pub(in crate::native) loop_merges: HashMap<String, LoopMergeInfo>,
    pub(in crate::native) branch_merges: HashMap<(String, String), String>,
    pub(in crate::native) branch_merges_by_header: HashMap<String, String>,
    pub(in crate::native) switch_merges: HashMap<String, String>,
}

pub(in crate::native) const CROSS_ARM_EDGE_MAX_BLOCKS: usize = 300;

pub(in crate::native) const LOOP_EXIT_SELECTION_MAX_BLOCKS: usize = 300;

pub(in crate::native) const REGION_CROSS_ARM_LADDER_MAX_BLOCKS: usize = 300;

pub(in crate::native) const SHARED_CONTINUATION_LADDER_MAX_BLOCKS: usize = 300;

pub(in crate::native) const SELECTION_SYNTH_GROWTH_MAX_BLOCKS: usize = 300;

pub(in crate::native) const STRUCTURED_PLAN_MAX_BLOCKS: usize = 3000;

const LOCAL_STRUCTURED_PLAN_WORK_BUDGET: usize = 4096;
const LOCAL_STRUCTURED_PLAN_MAX_BLOCKS: usize = 128;

pub(in crate::native) fn exceeds_local_structured_plan_budget(blocks: &[BodyBlock]) -> bool {
    if blocks.len() > LOCAL_STRUCTURED_PLAN_MAX_BLOCKS {
        return true;
    }
    let branching = blocks
        .iter()
        .filter(|block| block_successors(block).len() > 1)
        .count();
    blocks.len().saturating_mul(branching.max(1)) > LOCAL_STRUCTURED_PLAN_WORK_BUDGET
}

pub(in crate::native) fn blocks_changed(before: &[BodyBlock], after: &[BodyBlock]) -> bool {
    before.len() != after.len()
        || before
            .iter()
            .zip(after)
            .any(|(left, right)| left.name != right.name || block_body_changed(left, right))
}

pub(in crate::native) fn requires_shared_loop_entry_ownership(blocks: &[BodyBlock]) -> bool {
    let forest = analyze(blocks);
    let post_idoms = post_idom(blocks);
    let mut predecessors = HashMap::<String, Vec<&str>>::default();
    for block in blocks {
        for successor in block_successors(block) {
            predecessors
                .entry(successor)
                .or_default()
                .push(block.name.as_str());
        }
    }
    for loop_info in &forest.loops {
        let loop_body = loop_info
            .body
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let forward_predecessors = predecessors
            .get(loop_info.header.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|predecessor| !loop_body.contains(predecessor))
            .collect::<Vec<_>>();
        if forward_predecessors.len() < 2 {
            continue;
        }
        for header in blocks {
            if conditional_branch_targets(header).is_none() {
                continue;
            }
            let Some(natural_merge) = post_idoms.get(&header.name) else {
                continue;
            };
            let owned = forward_predecessors
                .iter()
                .filter(|predecessor| forest.dominates(&header.name, predecessor))
                .copied()
                .collect::<Vec<_>>();
            if owned.is_empty() || owned.len() == forward_predecessors.len() {
                continue;
            }
            if owned
                .iter()
                .any(|predecessor| !forest.dominates(natural_merge, predecessor))
            {
                return true;
            }
        }
    }
    false
}

fn block_body_changed(left: &BodyBlock, right: &BodyBlock) -> bool {
    match (&left.typed, &right.typed) {
        (Some(l), Some(r)) => format!("{l:?}") != format!("{r:?}"),
        (None, None) => false,
        _ => true,
    }
}

pub(in crate::native) fn structured_plan(blocks: &[BodyBlock]) -> Option<StructuredPlan> {
    if blocks.len() > STRUCTURED_PLAN_MAX_BLOCKS {
        return None;
    }
    let deep_shared = privatize_shared_continuations_for_ladder(blocks);
    if deep_shared.len() != blocks.len() {
        if let Some(plan) = structured_plan_ladder(&deep_shared, false) {
            return finalize_loop_role_switches(plan);
        }
        if let Some(plan) = structured_plan_ladder(&deep_shared, true) {
            return finalize_loop_role_switches(plan);
        }
    }
    drop(deep_shared);

    if blocks.len() > TERMINAL_EXIT_SELECTION_MAX_BLOCKS {
        return None;
    } else {
        if let Some(plan) = structured_plan_ladder(blocks, false) {
            return finalize_loop_role_switches(plan);
        }
        if let Some(plan) = structured_plan_ladder(blocks, true) {
            return finalize_loop_role_switches(plan);
        }
    }
    if let Some(plan) = structured_plan_divergent_exit(blocks) {
        return finalize_loop_role_switches(plan);
    }
    None
}

fn finalize_loop_role_switches(plan: StructuredPlan) -> Option<StructuredPlan> {
    if plan.blocks.len() > LOOP_EXIT_SELECTION_MAX_BLOCKS {
        return Some(plan);
    }
    let loop_roles = plan
        .loop_merges
        .values()
        .flat_map(|info| [&info.merge, &info.continue_target])
        .collect::<HashSet<_>>();
    let targets_loop_role = plan.blocks.iter().any(|block| {
        block.typed.as_ref().is_some_and(|typed| {
            matches!(
                typed.terminator,
                crate::native::tir::TirTerminator::Switch { .. }
            ) && block_successors(block)
                .iter()
                .any(|target| loop_roles.contains(target))
        })
    });
    if !targets_loop_role {
        return Some(plan);
    }
    let lowered_switches = super::blocks::lower_loop_exit_switches(&plan.blocks);
    if !blocks_changed(&plan.blocks, &lowered_switches) {
        return Some(plan);
    }
    structured_plan(&lowered_switches)
}

pub(in crate::native) fn structured_plan_divergent_exit(
    blocks: &[BodyBlock],
) -> Option<StructuredPlan> {
    let single_exit = super::clone_crossarm::separate_divergent_selection_exits(blocks)?;
    let shared_exit = super::clone_crossarm::privatize_shared_phi_exit_predecessors(&single_exit);
    if !blocks_changed(&single_exit, &shared_exit) {
        return None;
    }
    for allow_bare_exit in [false, true] {
        if let Some(plan) = structured_plan_ladder(&shared_exit, allow_bare_exit) {
            return Some(plan);
        }
    }
    None
}

fn structured_plan_terminal_attempts(
    blocks: &[BodyBlock],
    allow_bare_exit: bool,
) -> Option<StructuredPlan> {
    if blocks.len() > TERMINAL_EXIT_SELECTION_MAX_BLOCKS {
        return None;
    }
    let prepared_terminal = prepare_terminal_exit_selection(blocks);
    for loop_exit_selection in [true, false] {
        for (converge, break_aware) in [(false, false), (true, false), (true, true)] {
            if let Some(plan) = structured_plan_inner8(
                blocks,
                converge,
                break_aware,
                false,
                allow_bare_exit,
                loop_exit_selection,
                true,
                false,
                prepared_terminal.as_ref(),
            ) {
                return Some(plan);
            }
        }
    }
    None
}

pub(in crate::native) fn structured_plan_ladder(
    blocks: &[BodyBlock],
    allow_bare_exit: bool,
) -> Option<StructuredPlan> {
    let force_converge = crate::env_vars::converge_inloop();
    if let Some(plan) =
        structured_plan_inner4(blocks, force_converge, false, false, allow_bare_exit)
    {
        return Some(plan);
    }
    let privatized = super::clone_crossarm::privatize_trivial_cross_arm(blocks);
    if privatized.len() != blocks.len() {
        if let Some(plan) =
            structured_plan_inner4(&privatized, force_converge, false, false, allow_bare_exit)
        {
            return Some(plan);
        }
    }
    drop(privatized);
    let region = privatize_region_cross_arm_for_ladder(blocks);
    if region.len() != blocks.len() {
        if let Some(plan) =
            structured_plan_inner4(&region, force_converge, false, false, allow_bare_exit)
        {
            return Some(plan);
        }
    }
    if let Some(plan) = structured_plan_inner4(blocks, true, false, false, allow_bare_exit) {
        return Some(plan);
    }
    if blocks.len() <= LOOP_EXIT_SELECTION_MAX_BLOCKS {
        let lowered_switches = super::blocks::lower_loop_exit_switches(blocks);
        if blocks_changed(blocks, &lowered_switches) {
            if let Some(plan) =
                structured_plan_inner5(&lowered_switches, true, false, false, allow_bare_exit, true)
            {
                return Some(plan);
            }
        } else if let Some(plan) =
            structured_plan_inner5(blocks, true, false, false, allow_bare_exit, true)
        {
            return Some(plan);
        }
    } else if let Some(plan) =
        structured_plan_inner5(blocks, true, false, false, allow_bare_exit, true)
    {
        return Some(plan);
    }
    if switch_gate_excludes(blocks) {
        return structured_plan_terminal_attempts(blocks, allow_bare_exit);
    }
    if let Some(plan) = structured_plan_inner4(blocks, true, true, false, allow_bare_exit) {
        return Some(plan);
    }
    {
        if let Some(destraddled) = restructure_straddle_loop_merges(blocks) {
            if let Some(plan) =
                structured_plan_inner4(&destraddled, force_converge, false, false, allow_bare_exit)
            {
                return Some(plan);
            }
            if let Some(plan) =
                structured_plan_inner4(&destraddled, true, false, false, allow_bare_exit)
            {
                return Some(plan);
            }
            if let Some(plan) =
                structured_plan_inner4(&destraddled, true, true, false, allow_bare_exit)
            {
                return Some(plan);
            }
        }
    }
    {
        if let Some(destraddled) = restructure_straddle_loop_merges_with(blocks, true, true) {
            if let Some(plan) =
                structured_plan_inner4(&destraddled, true, false, false, allow_bare_exit)
            {
                return Some(plan);
            }
            if let Some(plan) =
                structured_plan_inner4(&destraddled, true, true, false, allow_bare_exit)
            {
                return Some(plan);
            }
            let region2 = privatize_region_cross_arm_for_ladder(&destraddled);
            if region2.len() != destraddled.len() {
                if let Some(plan) =
                    structured_plan_inner4(&region2, true, false, false, allow_bare_exit)
                {
                    return Some(plan);
                }
                if let Some(plan) =
                    structured_plan_inner4(&region2, true, true, false, allow_bare_exit)
                {
                    return Some(plan);
                }
            }
        }
    }
    if region.len() != blocks.len() {
        if let Some(plan) = structured_plan_inner4(&region, true, false, false, allow_bare_exit) {
            return Some(plan);
        }
        if let Some(plan) = structured_plan_inner4(&region, true, true, false, allow_bare_exit) {
            return Some(plan);
        }
    }
    if !switch_gate_excludes(blocks) {
        for &(bl, cv, br) in &[
            (blocks, force_converge, false),
            (blocks, true, false),
            (blocks, true, true),
        ] {
            if let Some(plan) = structured_plan_inner4(bl, cv, br, true, allow_bare_exit) {
                return Some(plan);
            }
        }
        if region.len() != blocks.len() {
            for &(cv, br) in &[(force_converge, false), (true, false), (true, true)] {
                if let Some(plan) = structured_plan_inner4(&region, cv, br, true, allow_bare_exit) {
                    return Some(plan);
                }
            }
        }
    }
    if blocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS {
        let cloned = super::clone_crossarm::privatize_cross_arm_edge(blocks);
        if cloned.len() != blocks.len() && cloned.len() <= CROSS_ARM_EDGE_MAX_BLOCKS {
            for &(cv, br) in &[(force_converge, false), (true, false), (true, true)] {
                if let Some(plan) = structured_plan_inner4(&cloned, cv, br, false, allow_bare_exit)
                {
                    return Some(plan);
                }
            }
        }
    }
    if blocks.len() <= CROSS_ARM_EDGE_MAX_BLOCKS {
        for &(dcv, dbr) in &[(true, true), (force_converge, false), (true, false)] {
            let cloned = privatize_synthesized_cross_arm_shared(blocks, dcv, dbr);
            if cloned.len() != blocks.len() && cloned.len() <= CROSS_ARM_EDGE_MAX_BLOCKS {
                for &(cv, br) in &[(force_converge, false), (true, false), (true, true)] {
                    if let Some(plan) =
                        structured_plan_inner4(&cloned, cv, br, false, allow_bare_exit)
                    {
                        return Some(plan);
                    }
                }
            }
        }
    }
    if let Some(plan) = structured_plan_terminal_attempts(blocks, allow_bare_exit) {
        return Some(plan);
    }
    None
}

pub(in crate::native) fn privatize_region_cross_arm_for_ladder(
    blocks: &[BodyBlock],
) -> Vec<BodyBlock> {
    if blocks.len() > REGION_CROSS_ARM_LADDER_MAX_BLOCKS {
        blocks.to_vec()
    } else {
        let cloned = super::clone_crossarm::privatize_region_cross_arm(blocks);
        if cloned.len() > REGION_CROSS_ARM_LADDER_MAX_BLOCKS {
            blocks.to_vec()
        } else {
            cloned
        }
    }
}

pub(in crate::native) fn privatize_shared_continuations_for_ladder(
    blocks: &[BodyBlock],
) -> Vec<BodyBlock> {
    if blocks.len() > SHARED_CONTINUATION_LADDER_MAX_BLOCKS {
        return blocks.to_vec();
    }
    let switch_private = super::clone_crossarm::privatize_switch_case_continuations(blocks);
    if switch_private.len() > SHARED_CONTINUATION_LADDER_MAX_BLOCKS {
        return blocks.to_vec();
    }
    let deep_shared = super::clone_crossarm::privatize_deep_shared_continuations(&switch_private);
    if deep_shared.len() > SHARED_CONTINUATION_LADDER_MAX_BLOCKS {
        blocks.to_vec()
    } else {
        deep_shared
    }
}

pub(in crate::native) fn selection_synth_growth_exceeds_ladder_cap(
    source_blocks: usize,
    synthesized_blocks: usize,
) -> bool {
    let allowed_growth = SELECTION_SYNTH_GROWTH_MAX_BLOCKS.max(source_blocks);
    synthesized_blocks.saturating_sub(source_blocks) > allowed_growth
}

pub(in crate::native) fn is_switch_block(b: &BodyBlock) -> bool {
    b.typed.as_ref().is_some_and(|carrier| {
        matches!(
            carrier.terminator,
            crate::native::tir::TirTerminator::Switch { .. }
        )
    })
}

pub(in crate::native) fn blocks_contain_multilevel_break_switch(blocks: &[BodyBlock]) -> bool {
    let switch_succs: HashMap<&str, Vec<String>> = blocks
        .iter()
        .filter(|b| is_switch_block(b))
        .map(|b| (b.name.as_str(), block_successors(b)))
        .collect();
    if switch_succs.is_empty() {
        return false;
    }
    let forest = analyze(blocks);
    for l in &forest.loops {
        let body: HashSet<&str> = l.body.iter().map(String::as_str).collect();
        for (name, succs) in &switch_succs {
            if *name == l.header.as_str() || !body.contains(name) {
                continue;
            }
            if succs.iter().any(|s| !body.contains(s.as_str())) {
                return true;
            }
        }
    }
    false
}

pub(in crate::native) fn switch_gate_excludes(blocks: &[BodyBlock]) -> bool {
    blocks_contain_multilevel_break_switch(blocks)
}
