use super::footprint::{addressing_is_logical, descriptor_keys, Analyzer, DescriptorKey};
use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Decoration, Op, StorageClass, Word};
use std::collections::BTreeSet;

pub(crate) fn decorate_unwritten_descriptors(module: &mut Module) {
    decorate_unwritten_storage_buffers(module);
    decorate_unwritten_storage_images(module);
}

fn storage_buffer_descriptor_variables(module: &Module) -> Vec<(Word, DescriptorKey)> {
    let decorations = descriptor_keys(module);
    module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
        .filter(|instruction| {
            matches!(
                instruction.operands.first(),
                Some(Operand::StorageClass(StorageClass::StorageBuffer))
            )
        })
        .filter_map(|instruction| {
            let id = instruction.result_id?;
            Some((id, *decorations.get(&id)?))
        })
        .collect()
}

fn decorate_unwritten_storage_buffers(module: &mut Module) {
    if !addressing_is_logical(module) {
        return;
    }
    let variables = storage_buffer_descriptor_variables(module);
    if variables.is_empty() {
        return;
    }
    let targets = variables
        .iter()
        .map(|(_, key)| *key)
        .collect::<BTreeSet<_>>();
    let analysis = Analyzer::new(module, &targets).analyze();
    let unwritten = targets
        .iter()
        .filter(|key| !analysis.escaped.contains(key))
        .filter(|key| !analysis.observed.get(key).is_some_and(|seen| seen.writes))
        .copied()
        .collect::<BTreeSet<_>>();

    for (id, key) in variables {
        if !unwritten.contains(&key) {
            continue;
        }
        module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(id),
                Operand::Decoration(Decoration::NonWritable),
            ],
        ));
    }
}

fn storage_image_descriptor_variables(module: &Module) -> Vec<(Word, DescriptorKey)> {
    let storage_images = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeImage)
        .filter(|instruction| instruction.operands.get(5) == Some(&Operand::LiteralBit32(2)))
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let pointers = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypePointer)
        .filter(|instruction| {
            matches!(
                instruction.operands.first(),
                Some(Operand::StorageClass(StorageClass::UniformConstant))
            )
        })
        .filter(|instruction| {
            matches!(instruction.operands.get(1), Some(Operand::IdRef(pointee)) if storage_images.contains(pointee))
        })
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let decorations = descriptor_keys(module);
    module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
        .filter(|instruction| {
            instruction
                .result_type
                .is_some_and(|ty| pointers.contains(&ty))
        })
        .filter_map(|instruction| {
            Some((
                instruction.result_id?,
                *decorations.get(&instruction.result_id?)?,
            ))
        })
        .collect()
}

fn image_operand_role(opcode: Op, position: usize) -> Option<ImageUse> {
    match opcode {
        Op::Load if position == 0 => Some(ImageUse::Propagates),
        Op::ImageTexelPointer if position == 0 => Some(ImageUse::Writes),
        Op::CopyObject | Op::Image | Op::SampledImage if position == 0 => {
            Some(ImageUse::Propagates)
        }
        Op::ImageWrite if position == 0 => Some(ImageUse::Writes),
        Op::ImageRead
        | Op::ImageFetch
        | Op::ImageSparseRead
        | Op::ImageSparseFetch
        | Op::ImageQuerySize
        | Op::ImageQuerySizeLod
        | Op::ImageQueryLevels
        | Op::ImageQuerySamples
        | Op::ImageQueryFormat
        | Op::ImageQueryOrder
        | Op::ImageQueryLod
            if position == 0 =>
        {
            Some(ImageUse::Reads)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageUse {
    Propagates,
    Reads,
    Writes,
}

fn decorate_unwritten_storage_images(module: &mut Module) {
    let variables = storage_image_descriptor_variables(module);
    if variables.is_empty() {
        return;
    }
    let mut roots = variables
        .iter()
        .map(|(id, key)| (*id, *key))
        .collect::<HashMap<_, _>>();
    loop {
        let mut grew = false;
        for instruction in module.all_inst_iter() {
            let Some(result) = instruction.result_id else {
                continue;
            };
            if roots.contains_key(&result) {
                continue;
            }
            let source = instruction
                .operands
                .iter()
                .enumerate()
                .find_map(|(position, operand)| {
                    let Operand::IdRef(id) = operand else {
                        return None;
                    };
                    (image_operand_role(instruction.class.opcode, position)
                        == Some(ImageUse::Propagates))
                    .then(|| roots.get(id).copied())
                    .flatten()
                });
            if let Some(key) = source {
                roots.insert(result, key);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    let mut disqualified = BTreeSet::<DescriptorKey>::new();
    for instruction in module
        .functions
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
    {
        for (position, operand) in instruction.operands.iter().enumerate() {
            let Operand::IdRef(id) = operand else {
                continue;
            };
            let Some(key) = roots.get(id).copied() else {
                continue;
            };
            match image_operand_role(instruction.class.opcode, position) {
                Some(ImageUse::Reads | ImageUse::Propagates) => {}
                Some(ImageUse::Writes) | None => {
                    disqualified.insert(key);
                }
            }
        }
    }

    for (id, key) in variables {
        if disqualified.contains(&key) {
            continue;
        }
        module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(id),
                Operand::Decoration(Decoration::NonWritable),
            ],
        ));
    }
}
