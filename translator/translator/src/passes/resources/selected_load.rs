use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Decoration, Op, StorageClass, Word};

#[derive(Clone, Copy)]
struct Arm {
    id: Word,
    pointer_ty: Word,
    storage: StorageClass,
    pointee: Word,
}

#[derive(Clone, Copy)]
struct Plan {
    condition: Word,
    true_arm: Arm,
    false_arm: Arm,
    value_ty: Word,
}

struct EmitState<'a> {
    globals: &'a mut Vec<Instruction>,
    constants: &'a mut HashMap<u32, Word>,
    unsigned_ints: &'a mut HashMap<u32, Word>,
    pointer_strides: &'a mut Vec<(Word, u32)>,
    next_id: &'a mut Word,
}

pub(in crate::passes) fn materialize_interface_selected_loads(module: &mut Module) -> bool {
    let type_defs = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect::<HashMap<_, _>>();
    let mut value_types = type_defs
        .iter()
        .filter_map(|(id, inst)| inst.result_type.map(|ty| (*id, ty)))
        .collect::<HashMap<_, _>>();
    for inst in module.all_inst_iter() {
        if let (Some(id), Some(ty)) = (inst.result_id, inst.result_type) {
            value_types.insert(id, ty);
        }
    }
    let value_defs = module
        .types_global_values
        .iter()
        .chain(module.functions.iter().flat_map(|function| {
            function
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter())
        }))
        .filter_map(|inst| inst.result_id.map(|id| (id, inst)))
        .collect::<HashMap<_, _>>();
    let pointer_root = |mut id: Word| -> Option<Word> {
        let mut visited = HashSet::default();
        loop {
            if !visited.insert(id) {
                return None;
            }
            let def = value_defs.get(&id)?;
            match def.class.opcode {
                Op::Variable => return Some(id),
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::CopyObject => {
                    match def.operands.first()? {
                        Operand::IdRef(base) => id = *base,
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    };
    let pointer_info = |id: Word| -> Option<(Word, StorageClass, Word)> {
        let pointer_ty = value_types.get(&id).copied()?;
        let def = type_defs.get(&pointer_ty)?;
        if def.class.opcode != Op::TypePointer {
            return None;
        }
        match (def.operands.first()?, def.operands.get(1)?) {
            (Operand::StorageClass(storage), Operand::IdRef(pointee)) => {
                Some((pointer_ty, *storage, *pointee))
            }
            _ => None,
        }
    };

    let mut candidates = HashMap::<Word, (Word, Arm, Arm)>::default();
    for inst in module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
    {
        let (
            Op::Select,
            Some(result),
            [Operand::IdRef(condition), Operand::IdRef(true_id), Operand::IdRef(false_id)],
        ) = (inst.class.opcode, inst.result_id, inst.operands.as_slice())
        else {
            continue;
        };
        let Some((_, result_storage, _)) = pointer_info(result) else {
            continue;
        };
        let Some((true_ty, true_storage, true_pointee)) = pointer_info(*true_id) else {
            continue;
        };
        let Some((false_ty, false_storage, false_pointee)) = pointer_info(*false_id) else {
            continue;
        };
        let storages = [result_storage, true_storage, false_storage];
        let crosses_storage = true_storage != result_storage || false_storage != result_storage;
        let crosses_binding = storages
            .iter()
            .all(|storage| *storage == StorageClass::StorageBuffer)
            && pointer_root(*true_id)
                .zip(pointer_root(*false_id))
                .is_some_and(|(true_root, false_root)| true_root != false_root);
        if !storages.contains(&StorageClass::StorageBuffer)
            || storages.iter().any(|storage| {
                !matches!(
                    storage,
                    StorageClass::StorageBuffer | StorageClass::Private | StorageClass::Function
                )
            })
            || (!crosses_storage && !crosses_binding)
        {
            continue;
        }
        candidates.insert(
            result,
            (
                *condition,
                Arm {
                    id: *true_id,
                    pointer_ty: true_ty,
                    storage: true_storage,
                    pointee: true_pointee,
                },
                Arm {
                    id: *false_id,
                    pointer_ty: false_ty,
                    storage: false_storage,
                    pointee: false_pointee,
                },
            ),
        );
    }

    let mut plans = HashMap::<Word, Plan>::default();
    for (select, (condition, true_arm, false_arm)) in candidates {
        let mut value_ty = None;
        let mut load_count = 0usize;
        let mut invalid_use = false;
        for inst in module.all_inst_iter() {
            for (operand_index, operand) in inst.operands.iter().enumerate() {
                if *operand != Operand::IdRef(select) {
                    continue;
                }
                if inst.class.opcode != Op::Load || operand_index != 0 {
                    invalid_use = true;
                    break;
                }
                let Some(load_ty) = inst.result_type else {
                    invalid_use = true;
                    break;
                };
                if value_ty
                    .replace(load_ty)
                    .is_some_and(|prior| prior != load_ty)
                {
                    invalid_use = true;
                    break;
                }
                load_count += 1;
            }
            if invalid_use {
                break;
            }
        }
        if invalid_use || load_count == 0 {
            continue;
        }
        let Some(value_ty) = value_ty else {
            continue;
        };
        if arm_is_loadable(&type_defs, true_arm, value_ty)
            && arm_is_loadable(&type_defs, false_arm, value_ty)
        {
            plans.insert(
                select,
                Plan {
                    condition,
                    true_arm,
                    false_arm,
                    value_ty,
                },
            );
        }
    }
    if plans.is_empty() {
        return false;
    }

    let removed_selects = plans.keys().copied().collect::<HashSet<_>>();
    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(1);
    let uint = find_uint(&type_defs);
    let mut new_globals = Vec::new();
    let mut constants = HashMap::default();
    let mut unsigned_ints = type_defs
        .iter()
        .filter_map(|(id, inst)| {
            (inst.class.opcode == Op::TypeInt
                && inst.operands.get(1) == Some(&Operand::LiteralBit32(0)))
            .then(|| match inst.operands.first() {
                Some(Operand::LiteralBit32(bits)) => Some((*bits, *id)),
                _ => None,
            })
            .flatten()
        })
        .collect::<HashMap<_, _>>();
    let mut pointer_strides: Vec<(Word, u32)> = Vec::new();
    for function in &mut module.functions {
        for block in &mut function.blocks {
            let old = block.instructions.clone();
            let mut rewritten = Vec::with_capacity(old.len());
            for inst in old {
                if inst
                    .result_id
                    .is_some_and(|result| plans.contains_key(&result))
                {
                    continue;
                }
                let Some(Operand::IdRef(pointer)) = inst.operands.first() else {
                    rewritten.push(inst);
                    continue;
                };
                let Some(plan) = plans
                    .get(pointer)
                    .copied()
                    .filter(|_| inst.class.opcode == Op::Load)
                else {
                    rewritten.push(inst);
                    continue;
                };
                let Some(uint) = uint else {
                    rewritten.push(inst);
                    continue;
                };
                let true_value = emit_arm_load(
                    plan.true_arm,
                    plan.value_ty,
                    uint,
                    &type_defs,
                    &mut rewritten,
                    &mut EmitState {
                        globals: &mut new_globals,
                        constants: &mut constants,
                        unsigned_ints: &mut unsigned_ints,
                        pointer_strides: &mut pointer_strides,
                        next_id: &mut next_id,
                    },
                )
                .expect("preflight proved mixed-select true arm loadable");
                let false_value = emit_arm_load(
                    plan.false_arm,
                    plan.value_ty,
                    uint,
                    &type_defs,
                    &mut rewritten,
                    &mut EmitState {
                        globals: &mut new_globals,
                        constants: &mut constants,
                        unsigned_ints: &mut unsigned_ints,
                        pointer_strides: &mut pointer_strides,
                        next_id: &mut next_id,
                    },
                )
                .expect("preflight proved mixed-select false arm loadable");
                rewritten.push(Instruction::new(
                    Op::Select,
                    Some(plan.value_ty),
                    inst.result_id,
                    vec![
                        Operand::IdRef(plan.condition),
                        Operand::IdRef(true_value),
                        Operand::IdRef(false_value),
                    ],
                ));
            }
            block.instructions = rewritten;
        }
    }
    module.types_global_values.extend(new_globals);
    for (pointer_ty, stride) in pointer_strides {
        if !module.annotations.iter().any(|inst| {
            inst.class.opcode == Op::Decorate
                && inst.operands
                    == [
                        Operand::IdRef(pointer_ty),
                        Operand::Decoration(Decoration::ArrayStride),
                        Operand::LiteralBit32(stride),
                    ]
        }) {
            module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(pointer_ty),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(stride),
                ],
            ));
        }
    }
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    let targets_removed = |inst: &Instruction| matches!(inst.operands.first(), Some(Operand::IdRef(id)) if removed_selects.contains(id));
    module.debug_names.retain(|inst| !targets_removed(inst));
    module.annotations.retain(|inst| !targets_removed(inst));
    true
}

fn arm_is_loadable(types: &HashMap<Word, Instruction>, arm: Arm, value_ty: Word) -> bool {
    if arm.pointee == value_ty {
        return true;
    }
    arm.storage == StorageClass::StorageBuffer
        && scalar_bits(types, arm.pointee).is_some_and(|source| {
            scalar_bits(types, value_ty).is_some_and(|value| {
                matches!(value, 16 | 32) && source < value && value % source == 0
            })
        })
        && types
            .get(&arm.pointee)
            .is_some_and(|ty| ty.class.opcode == Op::TypeInt)
}

fn emit_arm_load(
    arm: Arm,
    value_ty: Word,
    uint: Word,
    types: &HashMap<Word, Instruction>,
    sink: &mut Vec<Instruction>,
    state: &mut EmitState<'_>,
) -> Option<Word> {
    if arm.pointee == value_ty {
        let result = fresh(state.next_id);
        sink.push(Instruction::new(
            Op::Load,
            Some(value_ty),
            Some(result),
            vec![Operand::IdRef(arm.id)],
        ));
        return Some(result);
    }
    let source_bits = scalar_bits(types, arm.pointee)?;
    let value_bits = scalar_bits(types, value_ty)?;
    let units = value_bits / source_bits;
    if !state
        .pointer_strides
        .iter()
        .any(|(pointer_ty, _)| *pointer_ty == arm.pointer_ty)
    {
        state
            .pointer_strides
            .push((arm.pointer_ty, source_bits / 8));
    }
    let mut assembled = None;
    for unit in 0..units {
        let index = constant(uint, unit, state.globals, state.constants, state.next_id);
        let pointer = fresh(state.next_id);
        sink.push(Instruction::new(
            Op::PtrAccessChain,
            Some(arm.pointer_ty),
            Some(pointer),
            vec![Operand::IdRef(arm.id), Operand::IdRef(index)],
        ));
        let loaded = fresh(state.next_id);
        sink.push(Instruction::new(
            Op::Load,
            Some(arm.pointee),
            Some(loaded),
            vec![Operand::IdRef(pointer)],
        ));
        let widened = fresh(state.next_id);
        sink.push(Instruction::new(
            Op::UConvert,
            Some(uint),
            Some(widened),
            vec![Operand::IdRef(loaded)],
        ));
        let piece = if unit == 0 {
            widened
        } else {
            let shift = constant(
                uint,
                unit * source_bits,
                state.globals,
                state.constants,
                state.next_id,
            );
            let shifted = fresh(state.next_id);
            sink.push(Instruction::new(
                Op::ShiftLeftLogical,
                Some(uint),
                Some(shifted),
                vec![Operand::IdRef(widened), Operand::IdRef(shift)],
            ));
            shifted
        };
        assembled = Some(if let Some(previous) = assembled {
            let combined = fresh(state.next_id);
            sink.push(Instruction::new(
                Op::BitwiseOr,
                Some(uint),
                Some(combined),
                vec![Operand::IdRef(previous), Operand::IdRef(piece)],
            ));
            combined
        } else {
            piece
        });
    }
    let assembled = assembled?;
    let bits = if value_bits == 32 {
        assembled
    } else {
        let narrow_ty = if let Some(ty) = state.unsigned_ints.get(&value_bits).copied() {
            ty
        } else {
            let ty = fresh(state.next_id);
            state.globals.push(Instruction::new(
                Op::TypeInt,
                None,
                Some(ty),
                vec![Operand::LiteralBit32(value_bits), Operand::LiteralBit32(0)],
            ));
            state.unsigned_ints.insert(value_bits, ty);
            ty
        };
        let narrowed = fresh(state.next_id);
        sink.push(Instruction::new(
            Op::UConvert,
            Some(narrow_ty),
            Some(narrowed),
            vec![Operand::IdRef(assembled)],
        ));
        if value_ty == narrow_ty {
            return Some(narrowed);
        }
        narrowed
    };
    if value_ty == uint {
        return Some(bits);
    }
    let result = fresh(state.next_id);
    sink.push(Instruction::new(
        Op::Bitcast,
        Some(value_ty),
        Some(result),
        vec![Operand::IdRef(bits)],
    ));
    Some(result)
}

fn find_uint(types: &HashMap<Word, Instruction>) -> Option<Word> {
    types.iter().find_map(|(id, inst)| {
        (inst.class.opcode == Op::TypeInt
            && inst.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)])
        .then_some(*id)
    })
}

fn scalar_bits(types: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let inst = types.get(&ty)?;
    match inst.class.opcode {
        Op::TypeInt | Op::TypeFloat => match inst.operands.first()? {
            Operand::LiteralBit32(bits) => Some(*bits),
            _ => None,
        },
        _ => None,
    }
}

fn constant(
    uint: Word,
    value: u32,
    globals: &mut Vec<Instruction>,
    constants: &mut HashMap<u32, Word>,
    next_id: &mut Word,
) -> Word {
    *constants.entry(value).or_insert_with(|| {
        let id = fresh(next_id);
        globals.push(Instruction::new(
            Op::Constant,
            Some(uint),
            Some(id),
            vec![Operand::LiteralBit32(value)],
        ));
        id
    })
}

fn fresh(next_id: &mut Word) -> Word {
    let id = *next_id;
    *next_id += 1;
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};
    use spirv::FunctionControl;

    #[test]
    fn mixed_private_and_raw_buffer_loads_replay_as_scalar_values() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeBool, None, Some(4), vec![]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(5),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(8),
                vec![Operand::StorageClass(StorageClass::Private)],
            ),
            Instruction::new(Op::ConstantTrue, Some(4), Some(9), vec![]),
        ];
        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(2),
            Some(20),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(21),
            ],
        ));
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        function.blocks = vec![Block {
            label: Some(Instruction::new(Op::Label, None, Some(22), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::Select,
                    Some(6),
                    Some(23),
                    vec![Operand::IdRef(9), Operand::IdRef(8), Operand::IdRef(7)],
                ),
                Instruction::new(Op::Load, Some(2), Some(24), vec![Operand::IdRef(23)]),
                Instruction::new(Op::Return, None, None, vec![]),
            ],
        }];
        module.functions.push(function);

        assert!(materialize_interface_selected_loads(&mut module));
        assert_eq!(module.functions[0].blocks.len(), 1);
        let instructions = &module.functions[0].blocks[0].instructions;
        assert!(instructions
            .iter()
            .all(|inst| { inst.class.opcode != Op::Select || inst.result_type == Some(2) }));
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| inst.class.opcode == Op::Load && inst.result_type == Some(1))
                .count(),
            4
        );
        assert!(instructions.iter().any(|inst| {
            inst.class.opcode == Op::Bitcast
                && inst.result_type == Some(2)
                && inst.result_id != Some(24)
        }));
        assert!(module.annotations.iter().any(|inst| {
            inst.class.opcode == Op::Decorate
                && inst.operands
                    == [
                        Operand::IdRef(5),
                        Operand::Decoration(Decoration::ArrayStride),
                        Operand::LiteralBit32(1),
                    ]
        }));
        assert!(!materialize_interface_selected_loads(&mut module));
    }

    #[test]
    fn distinct_storage_buffer_roots_select_loaded_values() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeBool, None, Some(3), vec![]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(5),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(6),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(Op::ConstantTrue, Some(3), Some(7), vec![]),
        ];
        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(1),
            Some(20),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(21),
            ],
        ));
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        function.blocks = vec![Block {
            label: Some(Instruction::new(Op::Label, None, Some(22), vec![])),
            instructions: vec![
                Instruction::new(
                    Op::Select,
                    Some(4),
                    Some(23),
                    vec![Operand::IdRef(7), Operand::IdRef(5), Operand::IdRef(6)],
                ),
                Instruction::new(Op::Load, Some(1), Some(24), vec![Operand::IdRef(23)]),
                Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(24)]),
            ],
        }];
        module.functions.push(function);

        assert!(materialize_interface_selected_loads(&mut module));
        let instructions = &module.functions[0].blocks[0].instructions;
        assert_eq!(
            instructions
                .iter()
                .filter(|inst| inst.class.opcode == Op::Load && inst.result_type == Some(1))
                .count(),
            2
        );
        assert!(instructions.iter().any(|inst| {
            inst.class.opcode == Op::Select
                && inst.result_type == Some(1)
                && inst.result_id == Some(24)
        }));
        assert!(instructions
            .iter()
            .all(|inst| { inst.class.opcode != Op::Select || inst.result_type != Some(4) }));
    }
}
