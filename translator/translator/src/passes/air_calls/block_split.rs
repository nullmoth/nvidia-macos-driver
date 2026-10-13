use crate::fxhash::HashSet;
use crate::spirv_module::{Block, Function, Instruction, Operand};
use spirv::{Op, Word};

pub(in crate::passes) struct CallSiteSplit {
    pub(in crate::passes) prefix: Vec<Instruction>,
    pub(in crate::passes) suffix: Vec<Instruction>,
    block: usize,
    old_label: Word,
    continuation: Option<Word>,
    successors: HashSet<Word>,
    loop_exit_passthrough: Option<(Word, Word)>,
    pending_loop_merge: Option<Instruction>,
    leading_variables: usize,
}

impl CallSiteSplit {
    pub(in crate::passes) fn open(
        ctx: &mut crate::passes::Ctx,
        entry_idx: usize,
        block: usize,
        inst: usize,
        what: &str,
    ) -> Result<Self, String> {
        let old_label = ctx.module.functions[entry_idx].blocks[block]
            .label
            .as_ref()
            .and_then(|label| label.result_id)
            .ok_or_else(|| format!("{what} appears in a block without a label"))?;
        let old_insts = ctx.module.functions[entry_idx].blocks[block]
            .instructions
            .clone();
        let prefix = old_insts[..inst].to_vec();
        let mut suffix = old_insts[inst + 1..].to_vec();
        if suffix.is_empty()
            || !suffix
                .last()
                .is_some_and(|inst| is_block_terminator(inst.class.opcode))
        {
            return Err(format!(
                "{what} lowering requires the source block to retain its terminator"
            ));
        }
        if prefix
            .last()
            .is_some_and(|inst| matches!(inst.class.opcode, Op::SelectionMerge | Op::LoopMerge))
        {
            return Err(format!(
                "{what} lowering cannot split between a structured merge and its terminator"
            ));
        }

        let loop_merge = suffix
            .iter()
            .position(|inst| inst.class.opcode == Op::LoopMerge)
            .map(|index| suffix.remove(index));
        let mut loop_exit_passthrough = None;
        if let Some(loop_merge) = &loop_merge {
            let merge_target = loop_merge
                .operands
                .first()
                .and_then(|operand| match operand {
                    Operand::IdRef(target) => Some(*target),
                    _ => None,
                })
                .ok_or_else(|| format!("{what} loop split found a malformed OpLoopMerge"))?;
            let terminator = suffix
                .last_mut()
                .ok_or_else(|| format!("{what} loop split lost its terminator"))?;
            match terminator.class.opcode {
                Op::Branch => {}
                Op::BranchConditional => {
                    let exits_at_merge = terminator
                        .operands
                        .iter()
                        .skip(1)
                        .any(|operand| *operand == Operand::IdRef(merge_target));
                    if !exits_at_merge {
                        return Err(format!(
                            "{what} loop-header conditional does not target its loop merge"
                        ));
                    }
                    let private_merge = ctx.module.fresh_id();
                    for operand in terminator.operands.iter_mut().skip(1) {
                        if *operand == Operand::IdRef(merge_target) {
                            *operand = Operand::IdRef(private_merge);
                        }
                    }
                    let terminator_index = suffix.len() - 1;
                    suffix.insert(
                        terminator_index,
                        Instruction::new(
                            Op::SelectionMerge,
                            None,
                            None,
                            vec![
                                Operand::IdRef(private_merge),
                                Operand::SelectionControl(spirv::SelectionControl::NONE),
                            ],
                        ),
                    );
                    loop_exit_passthrough = Some((private_merge, merge_target));
                }
                _ => {
                    return Err(format!(
                        "{what} lowering cannot split this loop-header terminator honestly"
                    ));
                }
            }
        }
        let successors = terminator_successors(suffix.last().expect("suffix terminator"));
        Ok(Self {
            prefix,
            suffix,
            block,
            old_label,
            continuation: None,
            successors,
            loop_exit_passthrough,
            pending_loop_merge: loop_merge,
            leading_variables: leading_variable_count(&old_insts),
        })
    }

    pub(in crate::passes) fn branch_prefix_to(&mut self, target: Word) {
        if let Some(loop_merge) = self.pending_loop_merge.take() {
            self.prefix.push(loop_merge);
        }
        self.prefix.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(target)],
        ));
    }

    pub(in crate::passes) fn entry_label(&self) -> Word {
        self.old_label
    }

    pub(in crate::passes) fn continuation(&mut self, ctx: &mut crate::passes::Ctx) -> Word {
        *self
            .continuation
            .get_or_insert_with(|| ctx.module.fresh_id())
    }

    pub(in crate::passes) fn finish(
        mut self,
        ctx: &mut crate::passes::Ctx,
        entry_idx: usize,
        mut blocks: Vec<Block>,
    ) {
        let continuation = self.continuation(ctx);
        let Self {
            mut prefix,
            suffix,
            block,
            old_label,
            successors,
            loop_exit_passthrough,
            leading_variables,
            ..
        } = self;
        let current = &ctx.module.functions[entry_idx].blocks[block].instructions;
        let grown = leading_variable_count(current);
        if grown > leading_variables {
            let added = current[leading_variables..grown].to_vec();
            prefix.splice(leading_variables..leading_variables, added);
        }
        blocks.push(labelled_block(continuation, suffix));
        if let Some((private_merge, merge_target)) = loop_exit_passthrough {
            blocks.push(labelled_block(
                private_merge,
                vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(merge_target)],
                )],
            ));
        }
        ctx.module.functions[entry_idx].blocks[block].instructions = prefix;
        ctx.module.functions[entry_idx]
            .blocks
            .splice(block + 1..block + 1, blocks);
        if let Some((private_merge, merge_target)) = loop_exit_passthrough {
            let merge_successor = crate::fxhash::set_from([merge_target]);
            rewrite_successor_phi_predecessors(
                &mut ctx.module.functions[entry_idx],
                &merge_successor,
                old_label,
                private_merge,
            );
            let ordinary_successors = successors
                .difference(&merge_successor)
                .copied()
                .collect::<HashSet<_>>();
            rewrite_successor_phi_predecessors(
                &mut ctx.module.functions[entry_idx],
                &ordinary_successors,
                old_label,
                continuation,
            );
        } else {
            rewrite_successor_phi_predecessors(
                &mut ctx.module.functions[entry_idx],
                &successors,
                old_label,
                continuation,
            );
        }
    }
}

pub(in crate::passes) fn labelled_block(label: Word, instructions: Vec<Instruction>) -> Block {
    Block {
        label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
        instructions,
    }
}

pub(in crate::passes) use crate::spirv_module::is_block_terminator;

fn leading_variable_count(instructions: &[Instruction]) -> usize {
    instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(instructions.len())
}

fn terminator_successors(inst: &Instruction) -> HashSet<Word> {
    let mut out = HashSet::default();
    match inst.class.opcode {
        Op::Branch => {
            if let Some(Operand::IdRef(label)) = inst.operands.first() {
                out.insert(*label);
            }
        }
        Op::BranchConditional => {
            for operand in inst.operands.iter().skip(1).take(2) {
                if let Operand::IdRef(label) = operand {
                    out.insert(*label);
                }
            }
        }
        Op::Switch => {
            for operand in inst.operands.iter().skip(1) {
                if let Operand::IdRef(label) = operand {
                    out.insert(*label);
                }
            }
        }
        _ => {}
    }
    out
}

fn rewrite_successor_phi_predecessors(
    function: &mut Function,
    successors: &HashSet<Word>,
    old_label: Word,
    new_label: Word,
) {
    if successors.is_empty() {
        return;
    }
    for block in &mut function.blocks {
        let Some(label) = block.label.as_ref().and_then(|label| label.result_id) else {
            continue;
        };
        if !successors.contains(&label) {
            continue;
        }
        for inst in &mut block.instructions {
            if inst.class.opcode != Op::Phi {
                break;
            }
            for pair in inst.operands.chunks_mut(2) {
                if pair.len() == 2 && pair[1] == Operand::IdRef(old_label) {
                    pair[1] = Operand::IdRef(new_label);
                }
            }
        }
    }
}
