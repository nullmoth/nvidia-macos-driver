use super::super::relooper::block_label;
use crate::fxhash::HashMap;
use crate::spirv_module::{Block, Operand};
use spirv::{Op, Word};

pub(super) fn reads_the_flow_variable_unwritten(
    blocks: &[Block],
    flow_variable: Option<Word>,
) -> Option<String> {
    let flow_variable = flow_variable?;
    let graph = FlowGraph::of(blocks, flow_variable)?;
    let written_on_entry = graph.definitely_written_on_entry();
    for (index, block) in blocks.iter().enumerate() {
        let mut written = written_on_entry[index];
        for instruction in &block.instructions {
            if instruction.operands.first() != Some(&Operand::IdRef(flow_variable)) {
                continue;
            }
            match instruction.class.opcode {
                Op::Load if !written => {
                    let label = graph.labels[index];
                    return Some(format!(
                        "nesting dispatches in %{label} on a flow value it may not have written"
                    ));
                }
                Op::Store => written = true,
                _ => {}
            }
        }
    }
    None
}

struct FlowGraph {
    labels: Vec<Word>,
    predecessors: Vec<Vec<usize>>,
    stores: Vec<bool>,
}

impl FlowGraph {
    fn of(blocks: &[Block], flow_variable: Word) -> Option<Self> {
        let labels = blocks.iter().map(block_label).collect::<Option<Vec<_>>>()?;
        let index = labels
            .iter()
            .enumerate()
            .map(|(index, label)| (*label, index))
            .collect::<HashMap<_, _>>();
        let mut predecessors = vec![Vec::new(); blocks.len()];
        for (source, block) in blocks.iter().enumerate() {
            for target in successors(block) {
                if let Some(target) = index.get(&target) {
                    predecessors[*target].push(source);
                }
            }
        }
        let stores = blocks
            .iter()
            .map(|block| {
                block.instructions.iter().any(|instruction| {
                    instruction.class.opcode == Op::Store
                        && instruction.operands.first() == Some(&Operand::IdRef(flow_variable))
                })
            })
            .collect();
        Some(Self {
            labels,
            predecessors,
            stores,
        })
    }

    fn definitely_written_on_entry(&self) -> Vec<bool> {
        let mut on_entry = (0..self.labels.len())
            .map(|index| index != 0)
            .collect::<Vec<_>>();
        let mut changed = true;
        while changed {
            changed = false;
            for index in 1..self.labels.len() {
                if self.predecessors[index].is_empty() {
                    continue;
                }
                let merged = self.predecessors[index]
                    .iter()
                    .all(|source| on_entry[*source] || self.stores[*source]);
                if on_entry[index] != merged {
                    on_entry[index] = merged;
                    changed = true;
                }
            }
        }
        on_entry
    }
}

fn successors(block: &Block) -> Vec<Word> {
    let Some(terminator) = block.instructions.last() else {
        return Vec::new();
    };
    let operands = match terminator.class.opcode {
        Op::Branch => &terminator.operands[..],
        Op::BranchConditional => &terminator.operands[1..3.min(terminator.operands.len())],
        Op::Switch => &terminator.operands[1..],
        _ => return Vec::new(),
    };
    operands
        .iter()
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect()
}
