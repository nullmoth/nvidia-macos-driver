use crate::fxhash::HashMap;
use crate::spirv_module::Block;
use spirv::Word;

pub(in crate::passes) use crate::spirv_module::block_successors;

pub(in crate::passes) use crate::spirv_module::block_successors_by_label;

pub(in crate::passes) struct BlockDominance {
    reachable: Vec<bool>,
    intervals: Vec<Option<(usize, usize)>>,
}

impl BlockDominance {
    pub(in crate::passes) fn of(blocks: &[Block]) -> Self {
        let positions: HashMap<Word, usize> = blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| Some((block.label.as_ref()?.result_id?, index)))
            .collect();
        let successors = blocks
            .iter()
            .map(|block| {
                block_successors(block)
                    .into_iter()
                    .filter_map(|label| positions.get(&label).copied())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let predecessors = crate::dominators::build_predecessors(&successors);
        let (reachable, intervals, _) = crate::dominators::dominance(&successors, &predecessors);
        Self {
            reachable,
            intervals,
        }
    }

    pub(in crate::passes) fn dominates(&self, dominator: usize, block: usize) -> bool {
        if !self.reachable.get(block).copied().unwrap_or(false) {
            return false;
        }
        crate::dominators::dominates_interval(&self.intervals, dominator, block)
    }
}
