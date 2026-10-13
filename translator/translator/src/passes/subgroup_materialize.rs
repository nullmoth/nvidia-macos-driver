use super::Ctx;
use crate::fxhash::HashMap;
use crate::spirv_module::{Instruction, Operand};
use spirv::{Op, StorageClass, Word};

fn is_subgroup_op(opcode: Op) -> bool {
    matches!(
        opcode,
        Op::GroupNonUniformAll
            | Op::GroupNonUniformAllEqual
            | Op::GroupNonUniformAny
            | Op::GroupNonUniformBallot
            | Op::GroupNonUniformBallotBitCount
            | Op::GroupNonUniformBallotBitExtract
            | Op::GroupNonUniformBallotFindLSB
            | Op::GroupNonUniformBallotFindMSB
            | Op::GroupNonUniformBitwiseAnd
            | Op::GroupNonUniformBitwiseOr
            | Op::GroupNonUniformBitwiseXor
            | Op::GroupNonUniformBroadcast
            | Op::GroupNonUniformBroadcastFirst
            | Op::GroupNonUniformElect
            | Op::GroupNonUniformFAdd
            | Op::GroupNonUniformFMax
            | Op::GroupNonUniformFMin
            | Op::GroupNonUniformFMul
            | Op::GroupNonUniformIAdd
            | Op::GroupNonUniformIMul
            | Op::GroupNonUniformLogicalAnd
            | Op::GroupNonUniformLogicalOr
            | Op::GroupNonUniformLogicalXor
            | Op::GroupNonUniformRotateKHR
            | Op::GroupNonUniformSMax
            | Op::GroupNonUniformSMin
            | Op::GroupNonUniformShuffle
            | Op::GroupNonUniformShuffleDown
            | Op::GroupNonUniformShuffleUp
            | Op::GroupNonUniformShuffleXor
            | Op::GroupNonUniformUMax
            | Op::GroupNonUniformUMin
    )
}

fn forwards_as_expression(opcode: Op) -> bool {
    matches!(
        opcode,
        Op::CopyObject
            | Op::Bitcast
            | Op::FNegate
            | Op::SNegate
            | Op::Not
            | Op::FAdd
            | Op::FSub
            | Op::FMul
            | Op::FDiv
            | Op::IAdd
            | Op::ISub
            | Op::IMul
            | Op::UDiv
            | Op::SDiv
            | Op::UMod
            | Op::SMod
            | Op::SRem
            | Op::FMod
            | Op::FRem
            | Op::BitwiseAnd
            | Op::BitwiseOr
            | Op::BitwiseXor
            | Op::ShiftLeftLogical
            | Op::ShiftRightLogical
            | Op::ShiftRightArithmetic
            | Op::ConvertFToS
            | Op::ConvertFToU
            | Op::ConvertSToF
            | Op::ConvertUToF
            | Op::FConvert
            | Op::SConvert
            | Op::UConvert
            | Op::CompositeConstruct
            | Op::CompositeExtract
            | Op::CompositeInsert
            | Op::VectorShuffle
            | Op::VectorTimesScalar
            | Op::ExtInst
            | Op::Select
    )
}

pub(in crate::passes) fn materialize_selected_subgroup_results(ctx: &mut Ctx) {
    for function_idx in 0..ctx.module.functions.len() {
        materialize_in_function(ctx, function_idx);
    }
}

fn materialize_in_function(ctx: &mut Ctx, function_idx: usize) {
    let function = &ctx.module.functions[function_idx];
    let mut reads: HashMap<Word, usize> = HashMap::default();
    for block in &function.blocks {
        for inst in &block.instructions {
            for operand in &inst.operands {
                if let Operand::IdRef(id) = operand {
                    *reads.entry(*id).or_default() += 1;
                }
            }
        }
    }

    let mut spill: Vec<(usize, usize, Word, Word)> = Vec::new();
    for (block_idx, block) in function.blocks.iter().enumerate() {
        let defs: HashMap<Word, (usize, Op, Word)> = block
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, inst)| {
                Some((
                    inst.result_id?,
                    (index, inst.class.opcode, inst.result_type?),
                ))
            })
            .collect();
        let mut queue: Vec<Word> = block
            .instructions
            .iter()
            .filter(|inst| inst.class.opcode == Op::Select)
            .flat_map(|inst| inst.operands.iter().skip(1))
            .filter_map(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect();
        let mut seen = crate::fxhash::HashSet::default();
        while let Some(id) = queue.pop() {
            if !seen.insert(id) || reads.get(&id).copied().unwrap_or(0) != 1 {
                continue;
            }
            let Some(&(index, opcode, result_type)) = defs.get(&id) else {
                continue;
            };
            if is_subgroup_op(opcode) {
                spill.push((block_idx, index, id, result_type));
                continue;
            }
            if !forwards_as_expression(opcode) {
                continue;
            }
            queue.extend(
                block.instructions[index]
                    .operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::IdRef(operand) => Some(*operand),
                        _ => None,
                    }),
            );
        }
    }
    if spill.is_empty() {
        return;
    }

    let mut variables = Vec::new();
    let mut edits: HashMap<usize, Vec<(usize, Word, Word, Word)>> = HashMap::default();
    for (block_idx, index, result, result_type) in spill {
        let ptr_type = ctx.ty_ptr(StorageClass::Function, result_type);
        let variable = ctx.module.fresh_id();
        let reload = ctx.module.fresh_id();
        variables.push(Instruction::new(
            Op::Variable,
            Some(ptr_type),
            Some(variable),
            vec![Operand::StorageClass(StorageClass::Function)],
        ));
        edits
            .entry(block_idx)
            .or_default()
            .push((index, result, variable, reload));
    }

    let function = &mut ctx.module.functions[function_idx];
    for (block_idx, mut block_edits) in edits {
        block_edits.sort_by_key(|(index, ..)| std::cmp::Reverse(*index));
        for (index, result, variable, reload) in block_edits {
            let result_type = function.blocks[block_idx].instructions[index].result_type;
            function.blocks[block_idx].instructions.splice(
                index + 1..index + 1,
                [
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(variable), Operand::IdRef(result)],
                    ),
                    Instruction::new(
                        Op::Load,
                        result_type,
                        Some(reload),
                        vec![Operand::IdRef(variable)],
                    ),
                ],
            );
            for inst in function.blocks[block_idx]
                .instructions
                .iter_mut()
                .skip(index + 3)
            {
                for operand in inst.operands.iter_mut() {
                    if *operand == Operand::IdRef(result) {
                        *operand = Operand::IdRef(reload);
                    }
                }
            }
        }
    }
    let entry = &mut function.blocks[0];
    let after_variables = entry
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(entry.instructions.len());
    entry
        .instructions
        .splice(after_variables..after_variables, variables);
}
