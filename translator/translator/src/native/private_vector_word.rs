use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Op, StorageClass, Word};

#[derive(Clone, Copy)]
struct Plan {
    base: Option<Base>,
    vector_ty: Word,
    element_ty: Word,
    pair_ty: Option<Word>,
    low_lane: u32,
}

#[derive(Clone, Copy)]
enum Base {
    Existing(Word),
    ArrayElement {
        root: Word,
        pointer_ty: Word,
        index: Word,
        in_bounds: bool,
    },
}

pub(super) fn close_private_vector_word_views(module: &mut Module) -> bool {
    let mut pointer_info = HashMap::<Word, (StorageClass, Word)>::default();
    let mut vector_info = HashMap::<Word, (Word, u32)>::default();
    let mut scalar_bits = HashMap::<Word, u32>::default();
    let mut constants = HashMap::<Word, u32>::default();
    let mut constant_ids = HashMap::<(Word, u32), Word>::default();
    let mut arrays = HashMap::<Word, (Word, Word)>::default();
    let mut pair_type = HashMap::<Word, Word>::default();
    for instruction in &module.types_global_values {
        let Some(id) = instruction.result_id else {
            continue;
        };
        match instruction.class.opcode {
            Op::TypePointer => {
                if let [Operand::StorageClass(storage), Operand::IdRef(pointee)] =
                    instruction.operands.as_slice()
                {
                    pointer_info.insert(id, (*storage, *pointee));
                }
            }
            Op::TypeVector => {
                if let [Operand::IdRef(element), Operand::LiteralBit32(lanes)] =
                    instruction.operands.as_slice()
                {
                    vector_info.insert(id, (*element, *lanes));
                    if *lanes == 2 {
                        pair_type.insert(*element, id);
                    }
                }
            }
            Op::TypeArray => {
                if let [Operand::IdRef(element), Operand::IdRef(length)] =
                    instruction.operands.as_slice()
                {
                    arrays.insert(id, (*element, *length));
                }
            }
            Op::TypeInt | Op::TypeFloat => {
                if let Some(Operand::LiteralBit32(bits)) = instruction.operands.first() {
                    scalar_bits.insert(id, *bits);
                }
            }
            Op::Constant => {
                if let Some(Operand::LiteralBit32(value)) = instruction.operands.first() {
                    constants.insert(id, *value);
                    if let Some(ty) = instruction.result_type {
                        constant_ids.insert((ty, *value), id);
                    }
                }
            }
            _ => {}
        }
    }

    let mut value_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    for function in &module.functions {
        for instruction in &function.parameters {
            if let (Some(id), Some(ty)) = (instruction.result_id, instruction.result_type) {
                value_types.insert(id, ty);
            }
        }
        for block in &function.blocks {
            for instruction in &block.instructions {
                if let (Some(id), Some(ty)) = (instruction.result_id, instruction.result_type) {
                    value_types.insert(id, ty);
                }
            }
        }
    }

    let mut uses = HashMap::<Word, Vec<Op>>::default();
    let mut access_defs = HashMap::<Word, Instruction>::default();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                if matches!(
                    instruction.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain
                ) {
                    if let Some(id) = instruction.result_id {
                        access_defs.insert(id, instruction.clone());
                    }
                }
                for operand in &instruction.operands {
                    if let Operand::IdRef(id) = operand {
                        uses.entry(*id).or_default().push(instruction.class.opcode);
                    }
                }
            }
        }
    }

    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(1);
    let mut new_pair_types = Vec::<(Word, Instruction)>::new();
    let mut new_constants = Vec::<Instruction>::new();
    let mut plans = HashMap::<Word, Plan>::default();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                if !matches!(
                    instruction.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain
                ) || instruction.operands.len() != 3
                {
                    continue;
                }
                let (Some(result), Some(result_ptr_ty)) =
                    (instruction.result_id, instruction.result_type)
                else {
                    continue;
                };
                let Some((StorageClass::Private, result_pointee)) =
                    pointer_info.get(&result_ptr_ty).copied()
                else {
                    continue;
                };
                if scalar_bits.get(&result_pointee) != Some(&32)
                    || !uses.get(&result).is_some_and(|uses| {
                        !uses.is_empty() && uses.iter().all(|opcode| *opcode == Op::Load)
                    })
                {
                    continue;
                }
                let [Operand::IdRef(base), Operand::IdRef(zero), Operand::IdRef(word)] =
                    instruction.operands.as_slice()
                else {
                    continue;
                };
                if constants.get(zero) != Some(&0) {
                    continue;
                }
                let Some(base_ptr_ty) = value_types.get(base).copied() else {
                    continue;
                };
                let Some((StorageClass::Private, vector_ty)) =
                    pointer_info.get(&base_ptr_ty).copied()
                else {
                    continue;
                };
                let Some((element_ty, lanes)) = vector_info.get(&vector_ty).copied() else {
                    continue;
                };
                let Some(element_bits @ (16 | 32)) = scalar_bits.get(&element_ty).copied() else {
                    continue;
                };
                let Some(word) = constants.get(word).copied() else {
                    continue;
                };
                let lanes_per_word = 32 / element_bits;
                let words_per_vector = lanes / lanes_per_word;
                if words_per_vector == 0 {
                    continue;
                }
                let vector_offset = word / words_per_vector;
                let low_lane = (word % words_per_vector) * lanes_per_word;
                let base = if vector_offset == 0 {
                    Some(Base::Existing(*base))
                } else {
                    let Some(base_definition) = access_defs.get(base) else {
                        continue;
                    };
                    let [Operand::IdRef(root), Operand::IdRef(base_index)] =
                        base_definition.operands.as_slice()
                    else {
                        continue;
                    };
                    let Some(root_ptr_ty) = value_types.get(root).copied() else {
                        continue;
                    };
                    let Some((StorageClass::Private, array_ty)) =
                        pointer_info.get(&root_ptr_ty).copied()
                    else {
                        continue;
                    };
                    let Some((array_element, length_id)) = arrays.get(&array_ty).copied() else {
                        continue;
                    };
                    if array_element != vector_ty {
                        continue;
                    }
                    let (Some(base_index_value), Some(length)) = (
                        constants.get(base_index).copied(),
                        constants.get(&length_id).copied(),
                    ) else {
                        continue;
                    };
                    let Some(index_value) = base_index_value.checked_add(vector_offset) else {
                        continue;
                    };
                    if index_value >= length {
                        if instruction.class.opcode == Op::InBoundsAccessChain
                            && base_definition.class.opcode == Op::InBoundsAccessChain
                        {
                            None
                        } else {
                            continue;
                        }
                    } else {
                        let Some(index_ty) = value_types.get(base_index).copied() else {
                            continue;
                        };
                        let index =
                            *constant_ids
                                .entry((index_ty, index_value))
                                .or_insert_with(|| {
                                    let id = next_id;
                                    next_id += 1;
                                    new_constants.push(Instruction::new(
                                        Op::Constant,
                                        Some(index_ty),
                                        Some(id),
                                        vec![Operand::LiteralBit32(index_value)],
                                    ));
                                    id
                                });
                        Some(Base::ArrayElement {
                            root: *root,
                            pointer_ty: base_ptr_ty,
                            index,
                            in_bounds: base_definition.class.opcode == Op::InBoundsAccessChain,
                        })
                    }
                };
                let pair_ty = (element_bits == 16).then(|| {
                    *pair_type.entry(element_ty).or_insert_with(|| {
                        let id = next_id;
                        next_id += 1;
                        new_pair_types.push((
                            element_ty,
                            Instruction::new(
                                Op::TypeVector,
                                None,
                                Some(id),
                                vec![Operand::IdRef(element_ty), Operand::LiteralBit32(2)],
                            ),
                        ));
                        id
                    })
                });
                plans.insert(
                    result,
                    Plan {
                        base,
                        vector_ty,
                        element_ty,
                        pair_ty,
                        low_lane,
                    },
                );
            }
        }
    }
    if plans.is_empty() {
        return false;
    }

    if !new_pair_types.is_empty() || !new_constants.is_empty() {
        let first_variable = module
            .types_global_values
            .iter()
            .position(|instruction| instruction.class.opcode == Op::Variable)
            .unwrap_or(module.types_global_values.len());
        let additions = new_pair_types
            .into_iter()
            .map(|(_, instruction)| instruction)
            .chain(new_constants);
        module
            .types_global_values
            .splice(first_variable..first_variable, additions);
    }

    let planned_ids = plans.keys().copied().collect::<HashSet<_>>();
    for function in &mut module.functions {
        for block in &mut function.blocks {
            let old = block.instructions.clone();
            let mut rebuilt = Vec::with_capacity(old.len());
            for instruction in old {
                if instruction
                    .result_id
                    .is_some_and(|id| planned_ids.contains(&id))
                    && matches!(
                        instruction.class.opcode,
                        Op::AccessChain | Op::InBoundsAccessChain
                    )
                {
                    continue;
                }
                if instruction.class.opcode == Op::Load {
                    let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
                        rebuilt.push(instruction);
                        continue;
                    };
                    let Some(plan) = plans.get(pointer).copied() else {
                        rebuilt.push(instruction);
                        continue;
                    };
                    let (Some(result), Some(result_ty)) =
                        (instruction.result_id, instruction.result_type)
                    else {
                        rebuilt.push(instruction);
                        continue;
                    };
                    if scalar_bits.get(&result_ty) != Some(&32) {
                        rebuilt.push(instruction);
                        continue;
                    }
                    let Some(base) = plan.base else {
                        rebuilt.push(Instruction::new(
                            Op::Undef,
                            Some(result_ty),
                            Some(result),
                            vec![],
                        ));
                        continue;
                    };
                    let load_base = match base {
                        Base::Existing(base) => base,
                        Base::ArrayElement {
                            root,
                            pointer_ty,
                            index,
                            in_bounds,
                        } => {
                            let pointer = next_id;
                            next_id += 1;
                            rebuilt.push(Instruction::new(
                                if in_bounds {
                                    Op::InBoundsAccessChain
                                } else {
                                    Op::AccessChain
                                },
                                Some(pointer_ty),
                                Some(pointer),
                                vec![Operand::IdRef(root), Operand::IdRef(index)],
                            ));
                            pointer
                        }
                    };
                    let vector = next_id;
                    next_id += 1;
                    let mut load_operands = vec![Operand::IdRef(load_base)];
                    load_operands.extend(instruction.operands.iter().skip(1).cloned());
                    rebuilt.push(Instruction::new(
                        Op::Load,
                        Some(plan.vector_ty),
                        Some(vector),
                        load_operands,
                    ));
                    let bits = if let Some(pair_ty) = plan.pair_ty {
                        let pair = next_id;
                        next_id += 1;
                        rebuilt.push(Instruction::new(
                            Op::VectorShuffle,
                            Some(pair_ty),
                            Some(pair),
                            vec![
                                Operand::IdRef(vector),
                                Operand::IdRef(vector),
                                Operand::LiteralBit32(plan.low_lane),
                                Operand::LiteralBit32(plan.low_lane + 1),
                            ],
                        ));
                        pair
                    } else {
                        let component = next_id;
                        next_id += 1;
                        rebuilt.push(Instruction::new(
                            Op::CompositeExtract,
                            Some(plan.element_ty),
                            Some(component),
                            vec![Operand::IdRef(vector), Operand::LiteralBit32(plan.low_lane)],
                        ));
                        component
                    };
                    rebuilt.push(Instruction::new(
                        if plan.element_ty == result_ty && plan.pair_ty.is_none() {
                            Op::CopyObject
                        } else {
                            Op::Bitcast
                        },
                        Some(result_ty),
                        Some(result),
                        vec![Operand::IdRef(bits)],
                    ));
                    continue;
                }
                rebuilt.push(instruction);
            }
            block.instructions = rebuilt;
        }
    }
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};

    fn inst(op: Op, ty: Option<Word>, result: Option<Word>, operands: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, result, operands)
    }

    #[test]
    fn private_half4_word_load_becomes_vector_shuffle_bitcast() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(64));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(16)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(2), Operand::LiteralBit32(4)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::TypeArray,
                None,
                Some(6),
                vec![Operand::IdRef(3), Operand::IdRef(12)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(11),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(12),
                vec![Operand::LiteralBit32(3)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(13),
                vec![Operand::LiteralBit32(2)],
            ),
            inst(
                Op::Variable,
                Some(7),
                Some(20),
                vec![Operand::StorageClass(StorageClass::Private)],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::InBoundsAccessChain,
                Some(4),
                Some(21),
                vec![Operand::IdRef(20), Operand::IdRef(10)],
            ),
            inst(
                Op::InBoundsAccessChain,
                Some(5),
                Some(31),
                vec![Operand::IdRef(21), Operand::IdRef(10), Operand::IdRef(13)],
            ),
            inst(Op::Load, Some(1), Some(32), vec![Operand::IdRef(31)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut function = Function::new();
        function.blocks.push(block);
        module.functions.push(function);
        module.debug_names = vec![
            inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(31),
                    Operand::LiteralString("packed-word-address".into()),
                ],
            ),
            inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(63),
                    Operand::LiteralString("unrelated-dangling".into()),
                ],
            ),
        ];

        assert!(crate::native::rewrites::close_private_vector_word_views_module(&mut module));
        let body = &module.functions[0].blocks[0].instructions;
        assert!(!body.iter().any(|instruction| {
            instruction.class.opcode == Op::InBoundsAccessChain && instruction.result_id == Some(31)
        }));
        let bitcast = body
            .iter()
            .find(|instruction| instruction.result_id == Some(32))
            .expect("original load result is preserved");
        assert_eq!(bitcast.class.opcode, Op::Bitcast);
        let shuffle = body
            .iter()
            .find(|instruction| instruction.class.opcode == Op::VectorShuffle)
            .expect("two half lanes are selected");
        assert_eq!(
            shuffle.operands[2..],
            [Operand::LiteralBit32(0), Operand::LiteralBit32(1)]
        );
        assert!(body.iter().any(|instruction| {
            matches!(
                instruction.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain
            ) && instruction.result_type == Some(4)
                && instruction.operands == [Operand::IdRef(20), Operand::IdRef(11)]
        }));
        assert!(body.iter().any(|instruction| {
            instruction.class.opcode == Op::Load && instruction.result_type == Some(3)
        }));
        assert_eq!(module.debug_names.len(), 1);
        assert_eq!(module.debug_names[0].operands[0], Operand::IdRef(63));
    }
}
