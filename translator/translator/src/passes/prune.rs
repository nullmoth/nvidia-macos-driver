use crate::fxhash::HashSet;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Function};
use spirv::{Op, Word};

pub fn prune_unreachable_blocks(module: &mut Module) {
    for function in &mut module.functions {
        prune_function(function);
    }
}

fn block_label(block: &Block) -> Option<Word> {
    block.label.as_ref().and_then(|label| label.result_id)
}

fn referenced_blocks(block: &Block) -> Vec<Word> {
    let mut out = Vec::new();
    for inst in &block.instructions {
        match inst.class.opcode {
            Op::Branch
            | Op::BranchConditional
            | Op::Switch
            | Op::SelectionMerge
            | Op::LoopMerge => {
                for operand in &inst.operands {
                    if let Operand::IdRef(id) = operand {
                        out.push(*id);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn prune_function(function: &mut Function) {
    let Some(entry) = function.blocks.first().and_then(block_label) else {
        return;
    };
    let labels: HashSet<Word> = function.blocks.iter().filter_map(block_label).collect();
    let mut reachable: HashSet<Word> = HashSet::default();
    let mut work = vec![entry];
    while let Some(id) = work.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let Some(block) = function
            .blocks
            .iter()
            .find(|block| block_label(block) == Some(id))
        else {
            continue;
        };
        for target in referenced_blocks(block) {
            if labels.contains(&target) && !reachable.contains(&target) {
                work.push(target);
            }
        }
    }
    if reachable.len() == labels.len() {
        return;
    }
    function
        .blocks
        .retain(|block| block_label(block).is_some_and(|id| reachable.contains(&id)));
    for block in &mut function.blocks {
        for inst in &mut block.instructions {
            if inst.class.opcode != Op::Phi {
                continue;
            }
            let mut kept = Vec::with_capacity(inst.operands.len());
            for pair in inst.operands.chunks(2) {
                let pred_alive = match pair.get(1) {
                    Some(Operand::IdRef(pred)) => reachable.contains(pred),
                    _ => true,
                };
                if pred_alive {
                    kept.extend(pair.iter().cloned());
                }
            }
            debug_assert!(
                !kept.is_empty(),
                "phi lost every incoming pair while pruning unreachable blocks"
            );
            inst.operands = kept;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Instruction;
    use crate::spirv_module::ModuleHeader;

    fn label_ids(function: &Function) -> Vec<Word> {
        function.blocks.iter().filter_map(block_label).collect()
    }

    #[test]
    fn prunes_unreachable_block_and_drops_its_phi_pair() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));

        let uint = 10;
        let v_reach = 11;
        let v_dead = 12;
        let phi_res = 20;
        let entry = 1;
        let body = 2;
        let orphan = 3;
        let merge = 4;
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(uint),
                Some(v_reach),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(uint),
                Some(v_dead),
                vec![Operand::LiteralBit32(2)],
            ),
        ];

        let block = |label: Word, insts: Vec<Instruction>| Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions: insts,
        };
        module.functions.push(Function {
            def: Some(Instruction::new(Op::Function, Some(uint), Some(50), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![
                block(
                    entry,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(body)],
                    )],
                ),
                block(
                    body,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(merge)],
                    )],
                ),
                block(
                    orphan,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(merge)],
                    )],
                ),
                block(
                    merge,
                    vec![
                        Instruction::new(
                            Op::Phi,
                            Some(uint),
                            Some(phi_res),
                            vec![
                                Operand::IdRef(v_reach),
                                Operand::IdRef(body),
                                Operand::IdRef(v_dead),
                                Operand::IdRef(orphan),
                            ],
                        ),
                        Instruction::new(Op::Return, None, None, vec![]),
                    ],
                ),
            ],
        });

        prune_unreachable_blocks(&mut module);

        let func = &module.functions[0];
        assert_eq!(
            label_ids(func),
            vec![entry, body, merge],
            "the unreachable orphan block is removed, order otherwise preserved"
        );
        let phi = &func.blocks[2].instructions[0];
        assert_eq!(phi.class.opcode, Op::Phi);
        assert_eq!(
            phi.operands,
            vec![Operand::IdRef(v_reach), Operand::IdRef(body)],
            "the phi drops the pruned-predecessor pair, keeps the reachable one"
        );

        let before = format!("{:?}", module.functions);
        prune_unreachable_blocks(&mut module);
        assert_eq!(before, format!("{:?}", module.functions));
    }

    #[test]
    fn keeps_fully_reachable_function_unchanged() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        let block = |label: Word, insts: Vec<Instruction>| Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions: insts,
        };
        module.functions.push(Function {
            def: Some(Instruction::new(Op::Function, None, Some(50), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![
                block(
                    1,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(2)],
                    )],
                ),
                block(2, vec![Instruction::new(Op::Return, None, None, vec![])]),
            ],
        });
        let before = format!("{:?}", module.functions);
        prune_unreachable_blocks(&mut module);
        assert_eq!(before, format!("{:?}", module.functions));
    }
}
