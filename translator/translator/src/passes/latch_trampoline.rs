use crate::fxhash::HashMap;
use crate::spirv_module::{Block, Function, Module, Operand};
use spirv::{Op, Word};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LatchTrampoline {
    pub(crate) latch: Word,
    pub(crate) trampoline: Word,
    pub(crate) loop_merge: Word,
}

fn label_of(block: &Block) -> Option<Word> {
    block.label.as_ref()?.result_id
}

fn id_of(operand: Option<&Operand>) -> Option<Word> {
    match operand {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    }
}

fn operand_references(module: &Module) -> HashMap<Word, usize> {
    let mut counts = HashMap::default();
    for instruction in module.all_inst_iter() {
        for operand in &instruction.operands {
            if let Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) =
                operand
            {
                *counts.entry(*id).or_insert(0) += 1;
            }
        }
    }
    counts
}

pub(crate) fn latch_break_trampolines(
    function: &Function,
    references: &HashMap<Word, usize>,
) -> Vec<LatchTrampoline> {
    let loops = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::LoopMerge)
        .filter_map(|instruction| {
            Some((
                id_of(instruction.operands.get(1))?,
                id_of(instruction.operands.first())?,
            ))
        })
        .collect::<HashMap<_, _>>();
    let by_label = function
        .blocks
        .iter()
        .filter_map(|block| Some((label_of(block)?, block)))
        .collect::<HashMap<_, _>>();
    let mut sites = Vec::new();
    for block in &function.blocks {
        let Some(latch) = label_of(block) else {
            continue;
        };
        let [.., merge, branch] = block.instructions.as_slice() else {
            continue;
        };
        if merge.class.opcode != Op::SelectionMerge || branch.class.opcode != Op::BranchConditional
        {
            continue;
        }
        let Some(trampoline) = id_of(merge.operands.first()) else {
            continue;
        };
        let (Some(true_target), Some(false_target)) =
            (id_of(branch.operands.get(1)), id_of(branch.operands.get(2)))
        else {
            continue;
        };
        let continue_target = match (true_target == trampoline, false_target == trampoline) {
            (true, false) => false_target,
            (false, true) => true_target,
            _ => continue,
        };
        let Some(&loop_merge) = loops.get(&continue_target) else {
            continue;
        };
        let Some(tramp) = by_label.get(&trampoline) else {
            continue;
        };
        let [only] = tramp.instructions.as_slice() else {
            continue;
        };
        if only.class.opcode != Op::Branch || only.operands != [Operand::IdRef(loop_merge)] {
            continue;
        }
        let Some(merge_block) = by_label.get(&loop_merge) else {
            continue;
        };
        let phi_parents = merge_block
            .instructions
            .iter()
            .filter(|instruction| instruction.class.opcode == Op::Phi)
            .flat_map(|phi| phi.operands.iter().skip(1).step_by(2))
            .filter(|operand| **operand == Operand::IdRef(trampoline))
            .count();
        if references.get(&trampoline).copied().unwrap_or(0) != 2 + phi_parents {
            continue;
        }
        sites.push(LatchTrampoline {
            latch,
            trampoline,
            loop_merge,
        });
    }
    sites
}

pub(crate) fn fold_latch_break_trampolines(module: &mut Module) -> usize {
    let references = operand_references(module);
    let mut folded = 0;
    for function in &mut module.functions {
        for site in latch_break_trampolines(function, &references) {
            for block in &mut function.blocks {
                let label = label_of(block);
                if label == Some(site.latch) {
                    let at = block.instructions.len() - 2;
                    debug_assert_eq!(block.instructions[at].class.opcode, Op::SelectionMerge);
                    block.instructions.remove(at);
                    let branch = block
                        .instructions
                        .last_mut()
                        .expect("the latch keeps its branch");
                    for operand in branch.operands.iter_mut().skip(1).take(2) {
                        if *operand == Operand::IdRef(site.trampoline) {
                            *operand = Operand::IdRef(site.loop_merge);
                        }
                    }
                } else if label == Some(site.loop_merge) {
                    for phi in block
                        .instructions
                        .iter_mut()
                        .filter(|instruction| instruction.class.opcode == Op::Phi)
                    {
                        for parent in phi.operands.iter_mut().skip(1).step_by(2) {
                            if *parent == Operand::IdRef(site.trampoline) {
                                *parent = Operand::IdRef(site.latch);
                            }
                        }
                    }
                }
            }
            function
                .blocks
                .retain(|block| label_of(block) != Some(site.trampoline));
            folded += 1;
        }
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Instruction;

    const COND: Word = 21;
    const HEADER: Word = 10;
    const LATCH: Word = 20;
    const CONTINUE: Word = 30;
    const TRAMPOLINE: Word = 31;
    const MERGE: Word = 40;
    const PHI: Word = 41;

    fn inst(opcode: Op, result: Option<Word>, operands: Vec<Operand>) -> Instruction {
        Instruction::new(opcode, result.map(|_| 1), result, operands)
    }

    fn op(opcode: Op, operands: &[Word]) -> Instruction {
        Instruction::new(
            opcode,
            None,
            None,
            operands.iter().map(|id| Operand::IdRef(*id)).collect(),
        )
    }

    fn block(label: Word, instructions: Vec<Instruction>) -> Block {
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions,
        }
    }

    fn selection_merge(merge: Word) -> Instruction {
        Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(merge),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        )
    }

    fn module_with(true_target: Word, false_target: Word) -> Module {
        let mut module = Module::new();
        module.functions.push(Function {
            def: Some(Instruction::new(Op::Function, Some(2), Some(3), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![
                block(5, vec![op(Op::Branch, &[HEADER])]),
                block(
                    HEADER,
                    vec![
                        Instruction::new(
                            Op::LoopMerge,
                            None,
                            None,
                            vec![
                                Operand::IdRef(MERGE),
                                Operand::IdRef(CONTINUE),
                                Operand::LoopControl(spirv::LoopControl::NONE),
                            ],
                        ),
                        op(Op::Branch, &[LATCH]),
                    ],
                ),
                block(
                    LATCH,
                    vec![
                        selection_merge(TRAMPOLINE),
                        op(Op::BranchConditional, &[COND, true_target, false_target]),
                    ],
                ),
                block(CONTINUE, vec![op(Op::Branch, &[HEADER])]),
                block(TRAMPOLINE, vec![op(Op::Branch, &[MERGE])]),
                block(
                    MERGE,
                    vec![
                        inst(
                            Op::Phi,
                            Some(PHI),
                            vec![Operand::IdRef(7), Operand::IdRef(TRAMPOLINE)],
                        ),
                        op(Op::Return, &[]),
                    ],
                ),
            ],
        });
        module
    }

    fn base() -> Module {
        module_with(CONTINUE, TRAMPOLINE)
    }

    fn block_of(module: &Module, label: Word) -> Option<&Block> {
        module.functions[0]
            .blocks
            .iter()
            .find(|block| label_of(block) == Some(label))
    }

    fn terminator(module: &Module, label: Word) -> Vec<Operand> {
        block_of(module, label)
            .and_then(|block| block.instructions.last())
            .map(|instruction| instruction.operands.clone())
            .unwrap_or_default()
    }

    fn ids(values: &[Word]) -> Vec<Operand> {
        values.iter().map(|id| Operand::IdRef(*id)).collect()
    }

    fn left_alone(mut module: Module) -> bool {
        let before = format!("{:?}", module.functions);
        let folded = fold_latch_break_trampolines(&mut module);
        folded == 0 && format!("{:?}", module.functions) == before
    }

    #[test]
    fn a_do_while_bottom_test_through_a_trampoline_becomes_a_direct_continue_break_branch() {
        let mut module = base();
        assert_eq!(fold_latch_break_trampolines(&mut module), 1);
        assert_eq!(
            terminator(&module, LATCH),
            ids(&[COND, CONTINUE, MERGE]),
            "the latch must branch to the continue target and the loop merge directly"
        );
    }

    #[test]
    fn the_selection_merge_in_front_of_it_is_gone() {
        let mut module = base();
        fold_latch_break_trampolines(&mut module);
        let merges = module
            .all_inst_iter()
            .filter(|instruction| instruction.class.opcode == Op::SelectionMerge)
            .count();
        assert_eq!(merges, 0, "OpSelectionMerge left: {merges}");
    }

    #[test]
    fn the_trampoline_block_is_deleted_and_nothing_names_it() {
        let mut module = base();
        fold_latch_break_trampolines(&mut module);
        assert!(block_of(&module, TRAMPOLINE).is_none());
        let named = operand_references(&module)
            .get(&TRAMPOLINE)
            .copied()
            .unwrap_or(0);
        assert_eq!(named, 0, "operands still naming the trampoline: {named}");
    }

    #[test]
    fn a_merge_phi_from_the_trampoline_is_repointed_to_the_latch() {
        let mut module = base();
        fold_latch_break_trampolines(&mut module);
        let phi = block_of(&module, MERGE)
            .and_then(|block| block.instructions.first())
            .expect("merge phi");
        assert_eq!(phi.operands, ids(&[7, LATCH]));
    }

    #[test]
    fn exactly_one_block_and_two_instructions_go() {
        let mut module = base();
        let count = |module: &Module| {
            let function = &module.functions[0];
            (
                function.blocks.len(),
                function
                    .blocks
                    .iter()
                    .map(|block| block.instructions.len())
                    .sum::<usize>(),
            )
        };
        let (blocks, instructions) = count(&module);
        fold_latch_break_trampolines(&mut module);
        assert_eq!(count(&module), (blocks - 1, instructions - 2));
    }

    #[test]
    fn the_false_edge_continue_order_is_rewritten_too() {
        let mut module = module_with(TRAMPOLINE, CONTINUE);
        assert_eq!(fold_latch_break_trampolines(&mut module), 1);
        assert_eq!(terminator(&module, LATCH), ids(&[COND, MERGE, CONTINUE]));
    }

    #[test]
    fn a_second_fold_finds_nothing() {
        let mut module = base();
        fold_latch_break_trampolines(&mut module);
        assert!(left_alone(module), "the fold is not idempotent");
    }

    #[test]
    fn a_trampoline_that_does_real_work_is_left_alone() {
        let mut module = base();
        let tramp = module.functions[0]
            .blocks
            .iter_mut()
            .find(|block| label_of(block) == Some(TRAMPOLINE))
            .expect("trampoline");
        tramp
            .instructions
            .insert(0, inst(Op::IAdd, Some(33), ids(&[5, 6])));
        assert!(left_alone(module));
    }

    #[test]
    fn a_trampoline_that_jumps_anywhere_but_the_loop_merge_is_left_alone() {
        let mut module = base();
        let tramp = module.functions[0]
            .blocks
            .iter_mut()
            .find(|block| label_of(block) == Some(TRAMPOLINE))
            .expect("trampoline");
        tramp.instructions = vec![op(Op::Branch, &[50])];
        assert!(left_alone(module));
    }

    #[test]
    fn a_trampoline_named_elsewhere_is_left_alone() {
        let mut module = base();
        module.debug_names.push(Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(TRAMPOLINE),
                Operand::LiteralString("tramp".to_string()),
            ],
        ));
        assert!(left_alone(module));
    }

    #[test]
    fn a_branch_whose_other_arm_is_not_a_continue_target_is_left_alone() {
        let mut module = base();
        let header = module.functions[0]
            .blocks
            .iter_mut()
            .find(|block| label_of(block) == Some(HEADER))
            .expect("header");
        header.instructions[0].operands[1] = Operand::IdRef(39);
        assert!(left_alone(module));
    }
}
