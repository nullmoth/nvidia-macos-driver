use crate::fxhash::HashMap;
#[cfg(test)]
use crate::fxhash::HashSet;
#[cfg(test)]
use crate::spirv_module::{Function, Module, Operand};
#[cfg(test)]
use spirv::Op;
use spirv::Word;

pub(in crate::native) struct EmittedDominators {
    index: HashMap<Word, usize>,
    #[cfg(test)]
    immediate_dominator: Vec<Option<usize>>,
    intervals: Vec<Option<(usize, usize)>>,
}

impl EmittedDominators {
    pub(in crate::native) fn new(
        labels: &[Word],
        successors_by_label: &HashMap<Word, Vec<Word>>,
    ) -> Self {
        let index = labels
            .iter()
            .enumerate()
            .map(|(idx, label)| (*label, idx))
            .collect::<HashMap<_, _>>();
        let mut successors = vec![Vec::new(); labels.len()];
        for (&label, targets) in successors_by_label {
            let Some(&from) = index.get(&label) else {
                continue;
            };
            for target in targets {
                if let Some(&to) = index.get(target) {
                    successors[from].push(to);
                }
            }
        }
        let predecessors = crate::dominators::build_predecessors(&successors);
        let (_, intervals, _parents) = crate::dominators::dominance(&successors, &predecessors);
        Self {
            index,
            #[cfg(test)]
            immediate_dominator: _parents,
            intervals,
        }
    }

    pub(in crate::native) fn dominates(&self, dominator: Word, node: Word) -> bool {
        let (Some(&dominator), Some(&node)) = (self.index.get(&dominator), self.index.get(&node))
        else {
            return false;
        };
        crate::dominators::dominates_interval(&self.intervals, dominator, node)
    }

    #[cfg(test)]
    fn serialization_respects_dominance(&self) -> bool {
        self.immediate_dominator
            .iter()
            .enumerate()
            .all(|(node, dominator)| dominator.is_none_or(|dominator| dominator <= node))
    }
}

#[cfg(test)]
fn functions_violating_constructed_cfg(module: &Module) -> HashSet<Word> {
    module
        .functions
        .iter()
        .filter(|function| {
            function_has_invalid_selection_exit(function)
                || function_has_unmarked_back_edge(function)
                || function_has_unmarked_selection(function)
                || function_has_displaced_merge(function)
                || function_has_late_dominator(function)
                || function_has_non_dominating_value_use(function)
        })
        .filter_map(|function| function.def.as_ref()?.result_id)
        .collect()
}

#[cfg(test)]
fn function_has_unmarked_selection(function: &Function) -> bool {
    function.blocks.iter().any(|block| {
        let Some((terminator, prefix)) = block.instructions.split_last() else {
            return false;
        };
        if !matches!(terminator.class.opcode, Op::BranchConditional | Op::Switch) {
            return false;
        }
        !prefix.last().is_some_and(|instruction| {
            matches!(instruction.class.opcode, Op::SelectionMerge | Op::LoopMerge)
        })
    })
}

#[cfg(test)]
fn function_has_displaced_merge(function: &Function) -> bool {
    function.blocks.iter().any(|block| {
        let Some((terminator, prefix)) = block.instructions.split_last() else {
            return false;
        };
        prefix
            .iter()
            .enumerate()
            .find(|(_, instruction)| {
                matches!(instruction.class.opcode, Op::SelectionMerge | Op::LoopMerge)
            })
            .is_some_and(|(merge_index, merge)| {
                merge_index + 1 != prefix.len()
                    || match merge.class.opcode {
                        Op::SelectionMerge => {
                            !matches!(terminator.class.opcode, Op::BranchConditional | Op::Switch)
                        }
                        Op::LoopMerge => {
                            !matches!(terminator.class.opcode, Op::Branch | Op::BranchConditional)
                        }
                        _ => false,
                    }
            })
    })
}

#[cfg(test)]
fn function_has_late_dominator(function: &Function) -> bool {
    let labels = function
        .blocks
        .iter()
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        return false;
    }
    let successors = super::graph::spirv_block_successors_by_label(&function.blocks);
    let dominators = EmittedDominators::new(&labels, &successors);
    !dominators.serialization_respects_dominance()
}

#[cfg(test)]
fn function_has_non_dominating_value_use(function: &Function) -> bool {
    let labels = function
        .blocks
        .iter()
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        return false;
    }
    let successors = super::graph::spirv_block_successors_by_label(&function.blocks);
    let dominators = EmittedDominators::new(&labels, &successors);
    let definitions = function
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block_index, block)| {
            block.instructions.iter().enumerate().filter_map(
                move |(instruction_index, instruction)| {
                    Some((instruction.result_id?, (block_index, instruction_index)))
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let label_indices = labels
        .iter()
        .copied()
        .enumerate()
        .map(|(index, label)| (label, index))
        .collect::<HashMap<_, _>>();

    for (use_block, block) in function.blocks.iter().enumerate() {
        for (use_instruction, instruction) in block.instructions.iter().enumerate() {
            if instruction.class.opcode == Op::Phi {
                for pair in instruction.operands.chunks_exact(2) {
                    let (Operand::IdRef(value), Operand::IdRef(predecessor)) = (&pair[0], &pair[1])
                    else {
                        continue;
                    };
                    let (Some(&(definition_block, _)), Some(&predecessor_block)) =
                        (definitions.get(value), label_indices.get(predecessor))
                    else {
                        continue;
                    };
                    if definition_block != predecessor_block
                        && !dominators.dominates(labels[definition_block], *predecessor)
                    {
                        return true;
                    }
                }
                continue;
            }
            for operand in &instruction.operands {
                let Operand::IdRef(value) = operand else {
                    continue;
                };
                let Some(&(definition_block, definition_instruction)) = definitions.get(value)
                else {
                    continue;
                };
                let dominates = if definition_block == use_block {
                    definition_instruction < use_instruction
                } else {
                    dominators.dominates(labels[definition_block], labels[use_block])
                };
                if !dominates {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
fn function_has_unmarked_back_edge(function: &Function) -> bool {
    let labels = function
        .blocks
        .iter()
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        return false;
    }
    let successors = super::graph::spirv_block_successors_by_label(&function.blocks);
    let dominators = EmittedDominators::new(&labels, &successors);
    let loop_headers = function
        .blocks
        .iter()
        .filter(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| instruction.class.opcode == Op::LoopMerge)
        })
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<HashSet<_>>();
    for (source, targets) in &successors {
        for target in targets {
            if dominators.dominates(*target, *source) && !loop_headers.contains(target) {
                if crate::env_vars::reloop_why() {
                    eprintln!(
                        "UNMARKED-BACKEDGE function={:?} source={source} target={target}",
                        function
                            .def
                            .as_ref()
                            .and_then(|definition| definition.result_id)
                    );
                }
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
fn function_has_invalid_selection_exit(function: &Function) -> bool {
    let labels = function
        .blocks
        .iter()
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<Vec<_>>();
    if labels.is_empty() {
        return false;
    }
    let successors = super::graph::spirv_block_successors_by_label(&function.blocks);
    let dominators = EmittedDominators::new(&labels, &successors);
    let terminal_targets = function
        .blocks
        .iter()
        .filter(|block| {
            block.instructions.last().is_some_and(|terminator| {
                matches!(
                    terminator.class.opcode,
                    Op::Return | Op::ReturnValue | Op::Unreachable
                )
            })
        })
        .filter_map(|block| block.label.as_ref()?.result_id)
        .collect::<HashSet<_>>();
    let loop_constructs = function
        .blocks
        .iter()
        .filter_map(|block| {
            let header = block.label.as_ref()?.result_id?;
            let merge = block
                .instructions
                .iter()
                .find(|instruction| instruction.class.opcode == Op::LoopMerge)?;
            let (Some(Operand::IdRef(merge)), Some(Operand::IdRef(continue_target))) =
                (merge.operands.first(), merge.operands.get(1))
            else {
                return None;
            };
            Some((header, *merge, *continue_target))
        })
        .collect::<Vec<_>>();

    for block in &function.blocks {
        let Some(header) = block.label.as_ref().and_then(|label| label.result_id) else {
            continue;
        };
        let Some(merge) = block
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::SelectionMerge)
            .and_then(|instruction| instruction.operands.first())
            .and_then(|operand| match operand {
                Operand::IdRef(merge) => Some(*merge),
                _ => None,
            })
        else {
            continue;
        };
        let enclosing_loop_targets = loop_constructs
            .iter()
            .filter(|(loop_header, loop_merge, _)| {
                dominators.dominates(*loop_header, header)
                    && !dominators.dominates(*loop_merge, header)
            })
            .flat_map(|(_, loop_merge, continue_target)| [*loop_merge, *continue_target])
            .collect::<HashSet<_>>();
        let members = labels
            .iter()
            .copied()
            .filter(|label| {
                dominators.dominates(header, *label) && !dominators.dominates(merge, *label)
            })
            .collect::<HashSet<_>>();
        for (source, targets) in &successors {
            if members.contains(source) {
                continue;
            }
            if let Some(target) = targets
                .iter()
                .find(|target| **target != header && members.contains(target))
            {
                if crate::env_vars::reloop_why() {
                    eprintln!(
                        "NONHEADER-CONSTRUCT-ENTRY function={:?} header={header} merge={merge} source={source} target={target}",
                        function.def.as_ref().and_then(|definition| definition.result_id)
                    );
                }
                return true;
            }
        }

        for member in members {
            for target in successors.get(&member).into_iter().flatten() {
                let stays_in_selection =
                    dominators.dominates(header, *target) && !dominators.dominates(merge, *target);
                let is_enclosing_loop_back_edge =
                    loop_constructs
                        .iter()
                        .any(|(loop_header, loop_merge, continue_target)| {
                            *target == *loop_header
                                && dominators.dominates(*loop_header, header)
                                && !dominators.dominates(*loop_merge, header)
                                && dominators.dominates(*continue_target, member)
                        });
                let is_structured_exit = *target == merge
                    || enclosing_loop_targets.contains(target)
                    || is_enclosing_loop_back_edge
                    || terminal_targets.contains(target);
                if !stays_in_selection && !is_structured_exit {
                    if crate::env_vars::reloop_why() {
                        let target_block = function.blocks.iter().find(|block| {
                            block.label.as_ref().and_then(|label| label.result_id) == Some(*target)
                        });
                        let claims = function
                            .blocks
                            .iter()
                            .filter_map(|block| {
                                let claim_header = block.label.as_ref()?.result_id?;
                                let claim = block.instructions.iter().find(|instruction| {
                                    matches!(
                                        instruction.class.opcode,
                                        Op::SelectionMerge | Op::LoopMerge
                                    ) && instruction
                                        .operands
                                        .iter()
                                        .any(|operand| operand == &Operand::IdRef(*target))
                                })?;
                                Some((
                                    claim.class.opcode,
                                    claim_header,
                                    dominators.dominates(claim_header, header),
                                    dominators.dominates(*target, header),
                                ))
                            })
                            .collect::<Vec<_>>();
                        eprintln!(
                            "STRUCTURED-EXIT function={:?} header={header} merge={merge} header_dominates_merge={} member={member} target={target} target_ops={:?} claims={claims:?}",
                            function.def.as_ref().and_then(|definition| definition.result_id),
                            dominators.dominates(header, merge),
                            target_block.map(|block| block.instructions.iter().map(|instruction| instruction.class.opcode).collect::<Vec<_>>())
                        );
                    }
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Instruction};

    fn instruction(opcode: Op, result_id: Option<Word>, operands: Vec<Operand>) -> Instruction {
        Instruction::new(opcode, None, result_id, operands)
    }

    fn block(label: Word, instructions: Vec<Instruction>) -> Block {
        Block {
            label: Some(instruction(Op::Label, Some(label), vec![])),
            instructions,
        }
    }

    fn branch(target: Word) -> Instruction {
        instruction(Op::Branch, None, vec![Operand::IdRef(target)])
    }

    fn conditional(if_true: Word, if_false: Word) -> Instruction {
        instruction(
            Op::BranchConditional,
            None,
            vec![
                Operand::IdRef(99),
                Operand::IdRef(if_true),
                Operand::IdRef(if_false),
            ],
        )
    }

    fn selection(merge: Word) -> Instruction {
        instruction(
            Op::SelectionMerge,
            None,
            vec![
                Operand::IdRef(merge),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        )
    }

    fn test_module(blocks: Vec<Block>) -> Module {
        let mut module = Module::default();
        module.functions.push(Function {
            def: Some(instruction(Op::Function, Some(100), vec![])),
            blocks,
            ..Function::default()
        });
        module
    }

    #[test]
    fn detects_inner_selection_arm_exiting_to_outer_selection_merge() {
        let module = test_module(vec![
            block(1, vec![selection(8), conditional(2, 7)]),
            block(2, vec![selection(6), conditional(3, 6)]),
            block(3, vec![branch(8)]),
            block(6, vec![branch(8)]),
            block(7, vec![branch(8)]),
            block(8, vec![branch(9)]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn accepts_nested_selection_exiting_through_its_own_merge() {
        let module = test_module(vec![
            block(1, vec![selection(8), conditional(2, 7)]),
            block(2, vec![selection(6), conditional(3, 6)]),
            block(3, vec![branch(6)]),
            block(6, vec![branch(8)]),
            block(7, vec![branch(8)]),
            block(8, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn accepts_inner_selection_break_to_enclosing_loop_merge() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(
                2,
                vec![
                    instruction(
                        Op::LoopMerge,
                        None,
                        vec![
                            Operand::IdRef(9),
                            Operand::IdRef(8),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    branch(3),
                ],
            ),
            block(3, vec![selection(6), conditional(4, 6)]),
            block(4, vec![branch(9)]),
            block(6, vec![branch(8)]),
            block(8, vec![branch(2)]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn accepts_selection_back_edge_from_enclosing_loop_continue_target() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(
                2,
                vec![
                    instruction(
                        Op::LoopMerge,
                        None,
                        vec![
                            Operand::IdRef(9),
                            Operand::IdRef(8),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    branch(3),
                ],
            ),
            block(3, vec![selection(7), conditional(7, 8)]),
            block(7, vec![branch(9)]),
            block(8, vec![branch(2)]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn detects_selection_arm_bypassing_enclosing_loop_continue_target() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(
                2,
                vec![
                    instruction(
                        Op::LoopMerge,
                        None,
                        vec![
                            Operand::IdRef(9),
                            Operand::IdRef(8),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    branch(3),
                ],
            ),
            block(3, vec![selection(7), conditional(4, 7)]),
            block(4, vec![branch(2)]),
            block(7, vec![branch(8)]),
            block(8, vec![branch(2)]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn detects_non_dominating_ordinary_value_use() {
        let module = test_module(vec![
            block(1, vec![selection(4), conditional(2, 3)]),
            block(
                2,
                vec![
                    instruction(
                        Op::IAdd,
                        Some(50),
                        vec![Operand::IdRef(90), Operand::IdRef(91)],
                    ),
                    branch(4),
                ],
            ),
            block(3, vec![branch(4)]),
            block(
                4,
                vec![
                    instruction(
                        Op::IAdd,
                        Some(51),
                        vec![Operand::IdRef(50), Operand::IdRef(91)],
                    ),
                    instruction(Op::Return, None, vec![]),
                ],
            ),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn accepts_phi_values_defined_on_their_incoming_edges() {
        let module = test_module(vec![
            block(1, vec![selection(4), conditional(2, 3)]),
            block(
                2,
                vec![
                    instruction(
                        Op::IAdd,
                        Some(50),
                        vec![Operand::IdRef(90), Operand::IdRef(91)],
                    ),
                    branch(4),
                ],
            ),
            block(
                3,
                vec![
                    instruction(
                        Op::IAdd,
                        Some(51),
                        vec![Operand::IdRef(90), Operand::IdRef(91)],
                    ),
                    branch(4),
                ],
            ),
            block(
                4,
                vec![
                    instruction(
                        Op::Phi,
                        Some(52),
                        vec![
                            Operand::IdRef(50),
                            Operand::IdRef(2),
                            Operand::IdRef(51),
                            Operand::IdRef(3),
                        ],
                    ),
                    instruction(Op::Return, None, vec![]),
                ],
            ),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn detects_phi_value_not_dominating_its_incoming_edge() {
        let module = test_module(vec![
            block(1, vec![selection(4), conditional(2, 3)]),
            block(
                2,
                vec![
                    instruction(
                        Op::IAdd,
                        Some(50),
                        vec![Operand::IdRef(90), Operand::IdRef(91)],
                    ),
                    branch(4),
                ],
            ),
            block(3, vec![branch(4)]),
            block(
                4,
                vec![
                    instruction(
                        Op::Phi,
                        Some(52),
                        vec![
                            Operand::IdRef(50),
                            Operand::IdRef(2),
                            Operand::IdRef(50),
                            Operand::IdRef(3),
                        ],
                    ),
                    instruction(Op::Return, None, vec![]),
                ],
            ),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn accepts_selection_arm_exiting_to_shared_terminal_block() {
        let module = test_module(vec![
            block(1, vec![selection(8), conditional(2, 7)]),
            block(2, vec![selection(6), conditional(3, 6)]),
            block(3, vec![branch(9)]),
            block(6, vec![branch(8)]),
            block(7, vec![branch(9)]),
            block(8, vec![instruction(Op::Return, None, vec![])]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn detects_post_merge_reentry_into_selection_terminal() {
        let module = test_module(vec![
            block(1, vec![selection(6), conditional(3, 6)]),
            block(3, vec![branch(9)]),
            block(6, vec![branch(7)]),
            block(7, vec![branch(9)]),
            block(9, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn detects_back_edge_to_header_without_loop_merge() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(2, vec![conditional(3, 4)]),
            block(3, vec![branch(2)]),
            block(4, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn accepts_back_edge_to_declared_loop_header() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(
                2,
                vec![
                    instruction(
                        Op::LoopMerge,
                        None,
                        vec![
                            Operand::IdRef(4),
                            Operand::IdRef(3),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    conditional(3, 4),
                ],
            ),
            block(3, vec![branch(2)]),
            block(4, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert!(functions_violating_constructed_cfg(&module).is_empty());
    }

    #[test]
    fn detects_block_serialized_before_its_dominator() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(3, vec![instruction(Op::Return, None, vec![])]),
            block(2, vec![branch(3)]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn detects_conditional_without_merge_declaration() {
        let module = test_module(vec![
            block(1, vec![conditional(2, 3)]),
            block(2, vec![instruction(Op::Return, None, vec![])]),
            block(3, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }

    #[test]
    fn detects_loop_merge_displaced_from_its_terminator() {
        let module = test_module(vec![
            block(1, vec![branch(2)]),
            block(
                2,
                vec![
                    instruction(
                        Op::LoopMerge,
                        None,
                        vec![
                            Operand::IdRef(4),
                            Operand::IdRef(3),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    instruction(
                        Op::IAdd,
                        Some(50),
                        vec![Operand::IdRef(90), Operand::IdRef(91)],
                    ),
                    branch(3),
                ],
            ),
            block(3, vec![branch(2)]),
            block(4, vec![instruction(Op::Return, None, vec![])]),
        ]);
        assert_eq!(
            functions_violating_constructed_cfg(&module),
            crate::fxhash::set_from([100])
        );
    }
}
