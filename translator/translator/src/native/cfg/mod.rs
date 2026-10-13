pub(in crate::native) mod graph;
pub(super) mod loopforest;
pub(super) mod clone_crossarm;
mod exit_check;
pub(super) mod structured_emit;
mod blocks;
pub(super) mod structured_order;

pub(super) use clone_crossarm::rename_tokens;
pub(in crate::native) use exit_check::EmittedDominators;

pub(super) fn loop_forest_is_empty(blocks: &[BodyBlock]) -> bool {
    loopforest::analyze(blocks).loops.is_empty()
}
pub(super) use structured_emit::{
    cond_other_witness_lines, cond_phi_shared_witness_lines, construct_tree_gate_witness_lines,
    construct_tree_reject_reason, exceeds_local_structured_plan_budget,
    privatize_reused_emitted_merge_targets, renest_cond_phi_shared_own_arm,
    renest_loop_exit_sibling, renest_straddle_loop_merge, renest_whole_cfg_dispatch,
    requires_loop_exit_sibling_dispatch, requires_shared_loop_entry_ownership,
    straddle_witness_lines, structured_plan, structured_plan_construct_tree,
    structured_reject_reason, PlanCacheScope, CROSS_ARM_EDGE_MAX_BLOCKS,
};

pub(super) use blocks::{
    funnel_shared_branch_dispatches, implicit_entry_block_name, index_branch_merges_by_header,
    infer_bounded_branch_merges_by_header, infer_branch_merges, infer_direct_branch_merges,
    infer_loop_merges, infer_switch_merges, infer_switch_merges_bounded,
    lower_unstructured_switches, refunnel_one_deep_shared_arm, split_source_body_blocks,
    switch_default_is_inferred_merge,
};
#[cfg(test)]
pub(super) use blocks::{infer_direct_switch_merges, split_body_blocks};
#[cfg(test)]
pub(super) fn id_ref_operand(operand: &crate::spirv_module::Operand) -> Option<spirv::Word> {
    let crate::spirv_module::Operand::IdRef(id) = operand else {
        return None;
    };
    Some(*id)
}

#[derive(Clone, Debug)]
pub(super) struct LoopMergeInfo {
    pub(super) merge: String,
    pub(super) continue_target: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum BlockRole {
    #[default]
    Normal,
    LMerge,
    TerminalExitReturn,
    SwitchBypass,
    ConstructTreeRoute,
}

#[derive(Clone, Debug)]
pub(super) struct BodyBlock {
    pub(super) name: String,
    pub(super) role: BlockRole,
    pub(super) typed: Option<std::sync::Arc<crate::native::tir::TirBlock>>,
}

impl BodyBlock {
    pub(super) fn typed_mut(&mut self) -> Option<&mut crate::native::tir::TirBlock> {
        self.typed.as_mut().map(std::sync::Arc::make_mut)
    }

    #[cfg(test)]
    pub(super) fn lines(&self) -> Vec<String> {
        crate::native::tir::render_block_lines(
            self.typed
                .as_ref()
                .expect("test fixture block must have a typed carrier"),
        )
    }
}
