use super::*;

#[cfg(test)]
thread_local! {
    static INTERFACE_ADDRESS_CONSTRUCTION_COUNT: std::cell::Cell<usize> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_address_construction_counts() {
    INTERFACE_ADDRESS_CONSTRUCTION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn address_construction_count() -> usize {
    INTERFACE_ADDRESS_CONSTRUCTION_COUNT.with(std::cell::Cell::get)
}

fn defined_result_ids(module: &Module) -> HashSet<Word> {
    module
        .all_inst_iter()
        .filter_map(|inst| inst.result_id)
        .collect()
}

fn drop_debug_targets(module: &mut Module, targets: &HashSet<Word>) -> bool {
    let targets_removed_id = |inst: &Instruction| -> bool {
        matches!(inst.operands.first(), Some(Operand::IdRef(id)) if targets.contains(id))
    };

    let before_names = module.debug_names.len();
    let before_annotations = module.annotations.len();
    module.debug_names.retain(|inst| !targets_removed_id(inst));
    module.annotations.retain(|inst| !targets_removed_id(inst));
    module.debug_names.len() != before_names || module.annotations.len() != before_annotations
}

#[cfg(test)]
fn drop_dangling_debug_targets_module(module: &mut Module) -> bool {
    let defined = defined_result_ids(module);
    let dangling = module
        .debug_names
        .iter()
        .chain(&module.annotations)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(id)) if !defined.contains(id) => Some(*id),
            _ => None,
        })
        .collect();
    drop_debug_targets(module, &dangling)
}

fn finish_id_deleting_rewrite(
    module: &mut Module,
    defined_before: HashSet<Word>,
    changed: bool,
) -> bool {
    if changed {
        let defined_after = defined_result_ids(module);
        let removed = defined_before.difference(&defined_after).copied().collect();
        drop_debug_targets(module, &removed);
    }
    changed
}

pub(crate) fn close_private_vector_word_views_module(module: &mut Module) -> bool {
    let defined_before = defined_result_ids(module);
    let changed = private_vector_word::close_private_vector_word_views(module);
    finish_id_deleting_rewrite(module, defined_before, changed)
}

pub(crate) fn construct_interface_cross_binding_pointer_phis_module(
    module: &mut Module,
    layout: crate::reflect::DescriptorLayout,
) -> Option<spirv::Word> {
    let address_table = psb::construct_cross_binding_pointer_phis_with_layout(module, layout);
    if address_table.is_some() {
        add_native_module_capabilities(module);
    }
    address_table
}

pub(crate) fn construct_interface_cross_binding_pointer_merges_module(
    module: &mut Module,
    layout: crate::reflect::DescriptorLayout,
) -> Option<spirv::Word> {
    #[cfg(test)]
    INTERFACE_ADDRESS_CONSTRUCTION_COUNT.with(|count| count.set(count.get() + 1));
    let pointer_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypePointer)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let value_types = module
        .all_inst_iter()
        .filter_map(|instruction| {
            let result_type = instruction.result_type?;
            pointer_types
                .contains(&result_type)
                .then_some((instruction.result_id?, result_type))
        })
        .collect::<HashMap<_, _>>();
    let has_pointer_bitcast = module.all_inst_iter().any(|instruction| {
        instruction.class.opcode == Op::Bitcast
            && (instruction
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
                || instruction.operands.first().is_some_and(|operand| {
                    matches!(operand, Operand::IdRef(id) if value_types
                        .get(id)
                        .is_some_and(|ty| pointer_types.contains(ty)))
                }))
    });
    let has_non_pointer_memory_operand = module.all_inst_iter().any(|instruction| {
        if !matches!(instruction.class.opcode, Op::Load | Op::Store) {
            return false;
        }
        let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
            return true;
        };
        !value_types
            .get(pointer)
            .is_some_and(|ty| pointer_types.contains(ty))
    });
    if has_pointer_bitcast || has_non_pointer_memory_operand {
        return None;
    }
    let address_table = psb::construct_cross_binding_pointer_merges_with_layout(module, layout);
    if address_table.is_some() {
        add_native_module_capabilities(module);
    }
    address_table
}

pub(crate) fn construct_interface_cross_binding_pointer_values_module(module: &mut Module) -> bool {
    let changed = psb_value_select::rewrite_cross_binding_pointer_merges_to_values(module);
    if changed {
        add_native_module_capabilities(module);
    }
    changed
}

pub(crate) fn construct_opaque_image_selects_module(module: &mut Module) -> bool {
    let changed = opaque_image_select::construct_opaque_image_selects(module);
    if changed {
        add_native_module_capabilities(module);
    }
    changed
}

pub(crate) fn construct_workgroup_atomic_floats_module(module: &mut Module) -> bool {
    let changed = wg_atomic::construct_workgroup_atomic_floats(module);
    if changed {
        add_native_module_capabilities(module);
    }
    changed
}

pub(crate) fn close_inlined_bda_pointer_tables_module(module: &mut Module) -> bool {
    let Some(address_type) = module.types_global_values.iter().find_map(|instruction| {
        (instruction.class.opcode == Op::TypeInt
            && instruction.operands.first() == Some(&Operand::LiteralBit32(64)))
        .then_some(instruction.result_id?)
    }) else {
        return false;
    };
    let pointer_pointees = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            if instruction.class.opcode != Op::TypePointer {
                return None;
            }
            match instruction.operands.get(1)? {
                Operand::IdRef(pointee) => Some((instruction.result_id?, *pointee)),
                _ => None,
            }
        })
        .collect::<HashMap<_, _>>();
    let value_types = module
        .functions
        .iter()
        .flat_map(|function| function.parameters.iter().chain(function.all_inst_iter()))
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();

    let mut candidates = Vec::new();
    for (function_index, function) in module.functions.iter().enumerate() {
        let mut uses = HashMap::<Word, Vec<(usize, usize)>>::default();
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                for operand in &instruction.operands {
                    if let Operand::IdRef(id) = operand {
                        uses.entry(*id)
                            .or_default()
                            .push((block_index, instruction_index));
                    }
                }
            }
        }
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if instruction.class.opcode != Op::PtrAccessChain || instruction.operands.len() != 2
                {
                    continue;
                }
                let (Some(chain), Some(result_type), Some(Operand::IdRef(base))) = (
                    instruction.result_id,
                    instruction.result_type,
                    instruction.operands.first(),
                ) else {
                    continue;
                };
                if value_types.get(base) != Some(&address_type)
                    || !pointer_pointees
                        .get(&result_type)
                        .is_some_and(|pointee| pointer_pointees.contains_key(pointee))
                {
                    continue;
                }
                let Some(chain_uses) = uses.get(&chain) else {
                    continue;
                };
                if chain_uses.iter().all(|&(use_block, use_instruction)| {
                    let user = &function.blocks[use_block].instructions[use_instruction];
                    user.class.opcode == Op::Load
                        && user.operands.first() == Some(&Operand::IdRef(chain))
                        && user
                            .result_type
                            .is_some_and(|ty| pointer_pointees.contains_key(&ty))
                }) {
                    candidates.push((
                        function_index,
                        block_index,
                        instruction_index,
                        *base,
                        chain,
                        chain_uses.clone(),
                    ));
                }
            }
        }
    }
    if candidates.is_empty() {
        return false;
    }

    let physical_address_pointer = module
        .types_global_values
        .iter()
        .find_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer
                && instruction.operands.as_slice()
                    == [
                        Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                        Operand::IdRef(address_type),
                    ])
            .then_some(instruction.result_id?)
        })
        .unwrap_or_else(|| {
            let id = module.fresh_id();
            module.types_global_values.push(Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![
                    Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                    Operand::IdRef(address_type),
                ],
            ));
            module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(id),
                    Operand::Decoration(spirv::Decoration::ArrayStride),
                    Operand::LiteralBit32(8),
                ],
            ));
            id
        });

    candidates.sort_unstable_by_key(|(function, block, instruction, ..)| {
        (*function, *block, *instruction)
    });
    candidates.reverse();
    for (function_index, block_index, instruction_index, base, chain, chain_uses) in candidates {
        for (use_block, use_instruction) in chain_uses {
            let load = &mut module.functions[function_index].blocks[use_block].instructions
                [use_instruction];
            load.result_type = Some(address_type);
            load.operands.truncate(1);
            load.operands
                .push(Operand::MemoryAccess(spirv::MemoryAccess::ALIGNED));
            load.operands.push(Operand::LiteralBit32(8));
        }
        let converted = module.fresh_id();
        let block = &mut module.functions[function_index].blocks[block_index];
        block.instructions.insert(
            instruction_index,
            Instruction::new(
                Op::ConvertUToPtr,
                Some(physical_address_pointer),
                Some(converted),
                vec![Operand::IdRef(base)],
            ),
        );
        let chain_instruction = &mut block.instructions[instruction_index + 1];
        debug_assert_eq!(chain_instruction.result_id, Some(chain));
        chain_instruction.result_type = Some(physical_address_pointer);
        chain_instruction.operands[0] = Operand::IdRef(converted);
    }
    add_native_module_capabilities(module);
    true
}

pub(crate) fn construct_physical_atomic_pointer_lvalues_module(module: &mut Module) -> bool {
    fn is_atomic(op: Op) -> bool {
        matches!(
            op,
            Op::AtomicLoad
                | Op::AtomicStore
                | Op::AtomicExchange
                | Op::AtomicCompareExchange
                | Op::AtomicCompareExchangeWeak
                | Op::AtomicIIncrement
                | Op::AtomicIDecrement
                | Op::AtomicIAdd
                | Op::AtomicISub
                | Op::AtomicSMin
                | Op::AtomicUMin
                | Op::AtomicSMax
                | Op::AtomicUMax
                | Op::AtomicAnd
                | Op::AtomicOr
                | Op::AtomicXor
        )
    }

    let definitions = module
        .all_inst_iter()
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
        .collect::<HashMap<_, _>>();
    let physical_pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer
                && instruction.operands.first()
                    == Some(&Operand::StorageClass(StorageClass::PhysicalStorageBuffer)))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();

    let mut candidates = Vec::with_capacity(module.functions.len());
    for function in &module.functions {
        let mut function_candidates = Vec::new();
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                if !is_atomic(instruction.class.opcode) {
                    continue;
                }
                let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
                    continue;
                };
                let Some(definition) = definitions.get(pointer) else {
                    continue;
                };
                let (Some(pointer_type), Some(Operand::IdRef(address))) =
                    (definition.result_type, definition.operands.first())
                else {
                    continue;
                };
                if physical_pointer_types.contains(&pointer_type)
                    && definition.class.opcode == Op::ConvertUToPtr
                {
                    function_candidates.push((
                        block_index,
                        instruction_index,
                        pointer_type,
                        *address,
                    ));
                }
            }
        }
        candidates.push(function_candidates);
    }
    if candidates.iter().all(Vec::is_empty) {
        return false;
    }

    let candidate_pointer_types = crate::emission_order::dedup_in_encounter_order(
        candidates
            .iter()
            .flatten()
            .map(|(_, _, pointer_type, _)| *pointer_type),
    );
    let pointer_pointees = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            let pointer_type = instruction.result_id?;
            if instruction.class.opcode != Op::TypePointer
                || !candidate_pointer_types.contains(&pointer_type)
            {
                return None;
            }
            let Operand::IdRef(pointee) = instruction.operands.get(1)? else {
                return None;
            };
            Some((pointer_type, *pointee))
        })
        .collect::<HashMap<_, _>>();
    let mut next_id = module
        .header
        .as_ref()
        .map(|header| header.bound)
        .unwrap_or(1);
    let zero = module
        .types_global_values
        .iter()
        .find_map(|instruction| {
            let result_type = instruction.result_type?;
            let type_definition = definitions.get(&result_type)?;
            let is_zero = match instruction.class.opcode {
                Op::Constant => matches!(
                    instruction.operands.first(),
                    Some(Operand::LiteralBit32(0) | Operand::LiteralBit64(0))
                ),
                _ => false,
            };
            (type_definition.class.opcode == Op::TypeInt
                && type_definition.operands.first() == Some(&Operand::LiteralBit32(32))
                && is_zero)
                .then_some(instruction.result_id?)
        })
        .unwrap_or_else(|| {
            let integer_type = module
                .types_global_values
                .iter()
                .find_map(|instruction| {
                    (instruction.class.opcode == Op::TypeInt
                        && instruction.operands.first() == Some(&Operand::LiteralBit32(32)))
                    .then_some(instruction.result_id?)
                })
                .unwrap_or_else(|| {
                    let integer_type = next_id;
                    next_id += 1;
                    module.types_global_values.push(Instruction::new(
                        Op::TypeInt,
                        None,
                        Some(integer_type),
                        vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
                    ));
                    integer_type
                });
            let zero = next_id;
            next_id += 1;
            module.types_global_values.push(Instruction::new(
                Op::Constant,
                Some(integer_type),
                Some(zero),
                vec![Operand::LiteralBit32(0)],
            ));
            zero
        });
    let mut wrapper_pointer_types = HashMap::<Word, Word>::default();
    for pointer_type in candidate_pointer_types {
        let pointee = pointer_pointees[&pointer_type];
        let wrapper = next_id;
        let wrapper_pointer = next_id + 1;
        next_id += 2;
        module.types_global_values.push(Instruction::new(
            Op::TypeStruct,
            None,
            Some(wrapper),
            vec![Operand::IdRef(pointee)],
        ));
        module.types_global_values.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(wrapper_pointer),
            vec![
                Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                Operand::IdRef(wrapper),
            ],
        ));
        module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(wrapper),
                Operand::Decoration(spirv::Decoration::Block),
            ],
        ));
        module.annotations.push(Instruction::new(
            Op::MemberDecorate,
            None,
            None,
            vec![
                Operand::IdRef(wrapper),
                Operand::LiteralBit32(0),
                Operand::Decoration(spirv::Decoration::Offset),
                Operand::LiteralBit32(0),
            ],
        ));
        wrapper_pointer_types.insert(pointer_type, wrapper_pointer);
    }

    for (function, sites) in module.functions.iter_mut().zip(candidates) {
        let sites = sites
            .into_iter()
            .map(|(block, instruction, pointer_type, address)| {
                ((block, instruction), (pointer_type, address))
            })
            .collect::<HashMap<_, _>>();
        for (block_index, block) in function.blocks.iter_mut().enumerate() {
            let original = block.instructions.clone();
            let mut rewritten = Vec::with_capacity(original.len() + sites.len() * 2);
            for (instruction_index, mut instruction) in original.into_iter().enumerate() {
                let Some((pointer_type, address)) =
                    sites.get(&(block_index, instruction_index)).copied()
                else {
                    rewritten.push(instruction);
                    continue;
                };
                let wrapper_pointer = next_id;
                next_id += 1;
                rewritten.push(Instruction::new(
                    Op::ConvertUToPtr,
                    Some(wrapper_pointer_types[&pointer_type]),
                    Some(wrapper_pointer),
                    vec![Operand::IdRef(address)],
                ));
                let member_pointer = next_id;
                next_id += 1;
                rewritten.push(Instruction::new(
                    Op::AccessChain,
                    Some(pointer_type),
                    Some(member_pointer),
                    vec![Operand::IdRef(wrapper_pointer), Operand::IdRef(zero)],
                ));
                instruction.operands[0] = Operand::IdRef(member_pointer);
                rewritten.push(instruction);
            }
            block.instructions = rewritten;
        }
    }
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

pub(crate) fn prune_constant_branches_module(module: &mut Module) -> Result<(), String> {
    if !prune_constant_branches_module_if_changed(module) {
        return Err("native emitter: no constant branch to prune".to_string());
    }
    Ok(())
}

fn prune_constant_branches_module_if_changed(module: &mut Module) -> bool {
    let defined_before = defined_result_ids(module);
    let changed = constfold::prune_constant_branches(module);
    if !changed {
        return false;
    }
    finish_id_deleting_rewrite(module, defined_before, changed);
    add_native_module_capabilities(module);
    true
}

pub(crate) fn prune_constant_cfg_module_if_changed(module: &mut Module) -> bool {
    let defined_before = defined_result_ids(module);
    let changed = constfold::prune_constant_cfg(module);
    if !changed {
        return false;
    }
    finish_id_deleting_rewrite(module, defined_before, changed);
    add_native_module_capabilities(module);
    true
}

pub(crate) fn prune_unused_null_and_undef_constants_module(module: &mut Module) -> bool {
    let mut referenced = HashSet::default();
    for instruction in module
        .entry_points
        .iter()
        .chain(&module.execution_modes)
        .chain(&module.annotations)
        .chain(&module.types_global_values)
        .chain(
            module
                .functions
                .iter()
                .flat_map(|function| function.all_inst_iter()),
        )
    {
        referenced.extend(
            instruction
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                }),
        );
    }
    let removed = module
        .types_global_values
        .iter()
        .filter(|instruction| matches!(instruction.class.opcode, Op::ConstantNull | Op::Undef))
        .filter_map(|instruction| instruction.result_id)
        .filter(|id| !referenced.contains(id))
        .collect::<HashSet<_>>();
    if removed.is_empty() {
        return false;
    }
    module.types_global_values.retain(|instruction| {
        instruction
            .result_id
            .is_none_or(|id| !removed.contains(&id))
    });
    module.debug_names.retain(|instruction| {
        !instruction
            .operands
            .first()
            .is_some_and(|operand| matches!(operand, Operand::IdRef(id) if removed.contains(id)))
    });
    module.annotations.retain(|instruction| {
        !instruction
            .operands
            .first()
            .is_some_and(|operand| matches!(operand, Operand::IdRef(id) if removed.contains(id)))
    });
    true
}

pub(crate) fn lower_unobserved_bda_aggregate_pointer_fields_module(
    module: &mut Module,
) -> Result<bool, String> {
    let physical = module.memory_model.as_ref().is_some_and(|inst| {
        matches!(
            inst.operands.first(),
            Some(Operand::AddressingModel(
                spirv::AddressingModel::PhysicalStorageBuffer64
            ))
        )
    });
    if !physical {
        return Ok(false);
    }

    let type_defs = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect::<HashMap<_, _>>();
    let pointer_types = type_defs
        .iter()
        .filter_map(|(id, inst)| (inst.class.opcode == Op::TypePointer).then_some(*id))
        .collect::<HashSet<_>>();
    let all_value_types = module
        .types_global_values
        .iter()
        .chain(
            module
                .functions
                .iter()
                .flat_map(|function| function.all_inst_iter()),
        )
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    let address_ty = module
        .types_global_values
        .iter()
        .find_map(|inst| {
            (inst.class.opcode == Op::TypeInt
                && matches!(inst.operands.first(), Some(Operand::LiteralBit32(64)))
                && matches!(inst.operands.get(1), Some(Operand::LiteralBit32(0))))
            .then_some(inst.result_id?)
        })
        .ok_or("native construction: PhysicalStorageBuffer64 module has no ulong type")?;
    let target_field =
        |mut aggregate_ty: Word, indices: &[Operand]| -> Option<(Word, usize, Word)> {
            let mut target = None;
            for index in indices {
                let Operand::LiteralBit32(member) = index else {
                    return None;
                };
                let definition = type_defs.get(&aggregate_ty)?;
                let Operand::IdRef(member_ty) = definition.operands.get(*member as usize)? else {
                    return None;
                };
                target = Some((aggregate_ty, *member as usize, *member_ty));
                aggregate_ty = *member_ty;
            }
            target
        };

    let mut candidates = HashSet::default();
    for function in &module.functions {
        for inst in function.all_inst_iter() {
            if inst.class.opcode != Op::CompositeInsert {
                continue;
            }
            if let Some((owner, member, leaf_ty)) = inst
                .result_type
                .and_then(|ty| target_field(ty, &inst.operands[2..]))
            {
                let object_ty = inst.operands.first().and_then(|operand| match operand {
                    Operand::IdRef(id) => all_value_types.get(id).copied(),
                    _ => None,
                });
                let object_is_pointer = object_ty.is_some_and(|ty| pointer_types.contains(&ty));
                let object_is_address = object_ty == Some(address_ty);
                if pointer_types.contains(&leaf_ty) && (object_is_pointer || object_is_address) {
                    candidates.insert((owner, member));
                }
            }
        }
    }
    if candidates.is_empty() {
        return Ok(false);
    }

    for function in &module.functions {
        for inst in function.all_inst_iter() {
            if inst.class.opcode == Op::CompositeExtract {
                let composite_ty = inst.operands.first().and_then(|operand| match operand {
                    Operand::IdRef(id) => all_value_types.get(id).copied(),
                    _ => None,
                });
                if let Some((owner, member, _)) =
                    composite_ty.and_then(|ty| target_field(ty, &inst.operands[1..]))
                {
                    candidates.remove(&(owner, member));
                }
            }
        }
    }
    if candidates.is_empty() {
        return Ok(false);
    }

    let mut containing_types = candidates
        .iter()
        .map(|(owner, _)| *owner)
        .collect::<HashSet<_>>();
    loop {
        let before = containing_types.len();
        for (id, definition) in &type_defs {
            if matches!(
                definition.class.opcode,
                Op::TypeStruct | Op::TypeArray | Op::TypeRuntimeArray
            ) && definition.operands.iter().any(
                |operand| matches!(operand, Operand::IdRef(ty) if containing_types.contains(ty)),
            ) {
                containing_types.insert(*id);
            }
        }
        if containing_types.len() == before {
            break;
        }
    }
    let mut aggregate_escapes = false;
    for function in &module.functions {
        for inst in function.all_inst_iter() {
            for (operand_index, operand) in inst.operands.iter().enumerate() {
                let Operand::IdRef(id) = operand else {
                    continue;
                };
                if !all_value_types
                    .get(id)
                    .is_some_and(|ty| containing_types.contains(ty))
                {
                    continue;
                }
                let structural_use = matches!(
                    (inst.class.opcode, operand_index),
                    (Op::CompositeInsert, 1) | (Op::CompositeExtract, 0) | (Op::CopyObject, 0)
                );
                if !structural_use {
                    aggregate_escapes = true;
                }
            }
        }
    }
    if aggregate_escapes {
        return Ok(false);
    }

    let zero = module
        .types_global_values
        .iter()
        .find_map(|inst| {
            (inst.result_type == Some(address_ty) && inst.class.opcode == Op::ConstantNull)
                .then_some(inst.result_id?)
        })
        .ok_or("native construction: PhysicalStorageBuffer64 module has no ulong zero")?;

    for inst in &mut module.types_global_values {
        let Some(owner) = inst.result_id else {
            continue;
        };
        for (member, operand) in inst.operands.iter_mut().enumerate() {
            if candidates.contains(&(owner, member)) {
                *operand = Operand::IdRef(address_ty);
            }
        }
    }
    for function in &mut module.functions {
        for inst in function.all_inst_iter_mut() {
            if inst.class.opcode != Op::CompositeInsert {
                continue;
            }
            let Some((owner, member, _)) = inst
                .result_type
                .and_then(|ty| target_field(ty, &inst.operands[2..]))
            else {
                continue;
            };
            let object_is_pointer = inst
                .operands
                .first()
                .and_then(|operand| match operand {
                    Operand::IdRef(id) => all_value_types.get(id),
                    _ => None,
                })
                .is_some_and(|ty| pointer_types.contains(ty));
            if object_is_pointer && candidates.contains(&(owner, member)) {
                inst.operands[0] = Operand::IdRef(zero);
            }
        }
    }
    Ok(true)
}

pub(crate) fn eliminate_dead_pointer_values_module(
    module: &mut Module,
    preserved_pointer_ids: &HashSet<Word>,
) -> bool {
    let pointer_types = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypePointer)
        .filter_map(|instruction| instruction.result_id)
        .collect::<HashSet<_>>();
    let mut preserved_ids = preserved_pointer_ids.clone();
    preserved_ids.extend(module.functions.iter().flat_map(|function| {
        function.all_inst_iter().filter_map(|instruction| {
            let result = instruction.result_id?;
            let result_type = instruction.result_type?;
            (!pointer_types.contains(&result_type)).then_some(result)
        })
    }));
    constfold::dce_preserving(module, &preserved_ids)
}

pub(crate) fn eliminate_dead_values_module(
    module: &mut Module,
    preserved_ids: &HashSet<Word>,
) -> bool {
    constfold::dce_preserving(module, preserved_ids)
}

pub(crate) fn construct_cfg_functions_module(
    module: &mut Module,
    construction_names: &HashSet<String>,
) -> Result<(), String> {
    let named = module
        .debug_names
        .iter()
        .filter_map(|instruction| {
            if instruction.class.opcode != Op::Name {
                return None;
            }
            match (instruction.operands.first(), instruction.operands.get(1)) {
                (Some(Operand::IdRef(id)), Some(Operand::LiteralString(name)))
                    if construction_names.contains(name) =>
                {
                    Some(*id)
                }
                _ => None,
            }
        })
        .collect::<HashSet<_>>();
    let mut selected = named.clone();
    selected.extend(module.functions.iter().filter_map(|function| {
        let has_unowned_header = blocks_have_unowned_selection_header(&function.blocks);
        let has_unowned_backedge = function_has_unowned_backedge(function);
        (has_unowned_header || has_unowned_backedge)
            .then_some(function.def.as_ref().and_then(|def| def.result_id))
            .flatten()
    }));
    if !crate::env_vars::no_pointer_remat() {
        let mut bound = module
            .header
            .as_ref()
            .map(|header| header.bound)
            .unwrap_or(1);
        let mut repaired = 0;
        for function in &mut module.functions {
            repaired += crate::native::owned_cfg::rematerialize_nondominating_pure_uses(
                function, &mut bound,
            );
        }
        if let Some(header) = module.header.as_mut() {
            header.bound = bound;
        }
        if repaired > 0 && crate::env_vars::reloop_why() {
            eprintln!("REMAT-PURE uses={repaired}");
        }
    }
    let value_types = module
        .all_inst_iter()
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    selected.extend(module.functions.iter().filter_map(|function| {
        crate::native::owned_cfg::owned_function_construction_error(function, &value_types)?;
        function.def.as_ref().and_then(|def| def.result_id)
    }));
    selected.retain(|id| {
        !module.functions.iter().any(|function| {
            function.def.as_ref().and_then(|def| def.result_id) == Some(*id)
                && function_is_already_structured(function)
        })
    });
    if selected.is_empty() {
        return Ok(());
    }
    let defined_before = defined_result_ids(module);
    let nested = crate::native::reloop_nest::structure_selected_functions(module, &selected);
    let mut remaining = selected.clone();
    remaining.retain(|id| !nested.contains(id));
    let (relooped, declines) = if remaining.is_empty() {
        (false, Vec::new())
    } else if named.len() == construction_names.len() || !nested.is_empty() {
        relooper::rewrite_selected_to_relooper(
            module,
            relooper::default_max_relooper_blocks(),
            &remaining,
        )
    } else {
        (
            relooper::rewrite_to_relooper(module, relooper::default_max_relooper_blocks()),
            Vec::new(),
        )
    };
    let changed = relooped || !nested.is_empty();
    if !changed {
        return Err(format!(
            "native emitter: selected CFG function cannot be constructed{}",
            construction_decline_detail(&declines)
        ));
    }
    finish_id_deleting_rewrite(module, defined_before, changed);
    add_native_module_capabilities(module);
    Ok(())
}

fn construction_decline_detail(declines: &[relooper::ReloopDecline]) -> String {
    if declines.is_empty() {
        return String::new();
    }
    const SHOWN: usize = 4;
    let ceiling = relooper::relooper_block_ceiling(relooper::default_max_relooper_blocks());
    let reasons = declines
        .iter()
        .take(SHOWN)
        .map(|decline| {
            let name = decline
                .function
                .map(|id| format!("%{id}"))
                .unwrap_or_else(|| "an unnamed function".to_string());
            let limit = if decline.reason == relooper::TOO_MANY_BLOCKS {
                format!(" (the state-machine ceiling is {ceiling} blocks)")
            } else {
                String::new()
            };
            format!(
                "{name} with {} blocks: {}{limit}",
                decline.blocks, decline.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let rest = declines.len().saturating_sub(SHOWN);
    let more = if rest > 0 {
        format!(" (and {rest} more)")
    } else {
        String::new()
    };
    format!("; the state-machine construction declined {reasons}{more}")
}

pub(crate) fn blocks_have_unowned_selection_header(blocks: &[crate::spirv_module::Block]) -> bool {
    !unowned_selection_header_labels(blocks).is_empty()
}

pub(crate) fn unowned_selection_header_labels(
    blocks: &[crate::spirv_module::Block],
) -> Vec<Option<Word>> {
    let mut declared_boundaries = HashSet::default();
    for instruction in blocks.iter().flat_map(|block| &block.instructions) {
        let boundary_count = match instruction.class.opcode {
            Op::SelectionMerge => 1,
            Op::LoopMerge => 2,
            _ => 0,
        };
        declared_boundaries.extend(instruction.operands.iter().take(boundary_count).filter_map(
            |operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            },
        ));
    }
    blocks
        .iter()
        .filter_map(|block| {
            let (terminator, prefix) = block.instructions.split_last()?;
            if prefix.last().is_some_and(|instruction| {
                matches!(instruction.class.opcode, Op::SelectionMerge | Op::LoopMerge)
            }) {
                return None;
            }
            let requires_merge = match terminator.class.opcode {
                Op::Switch => true,
                Op::BranchConditional => {
                    let targets = terminator
                        .operands
                        .iter()
                        .skip(1)
                        .take(2)
                        .filter_map(|operand| match operand {
                            Operand::IdRef(id) => Some(*id),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    match targets.as_slice() {
                        [true_target, false_target] => {
                            true_target != false_target
                                && !declared_boundaries.contains(true_target)
                                && !declared_boundaries.contains(false_target)
                        }
                        _ => true,
                    }
                }
                _ => false,
            };
            requires_merge.then(|| block.label.as_ref().and_then(|label| label.result_id))
        })
        .collect()
}

fn function_is_already_structured(function: &crate::spirv_module::Function) -> bool {
    let [block] = function.blocks.as_slice() else {
        return false;
    };
    !matches!(
        block
            .instructions
            .last()
            .map(|instruction| instruction.class.opcode),
        Some(Op::Branch | Op::BranchConditional | Op::Switch)
    )
}

pub(crate) fn function_has_unowned_backedge(function: &crate::spirv_module::Function) -> bool {
    let labels = function
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| block.label.as_ref()?.result_id.map(|id| (id, index)))
        .collect::<HashMap<_, _>>();
    if function.blocks.is_empty() || labels.len() != function.blocks.len() {
        return false;
    }
    let successors = function
        .blocks
        .iter()
        .map(|block| {
            let Some(terminator) = block.instructions.last() else {
                return Vec::new();
            };
            match terminator.class.opcode {
                Op::Branch => terminator
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(id) => labels.get(id).copied(),
                        _ => None,
                    })
                    .into_iter()
                    .collect(),
                Op::BranchConditional => terminator
                    .operands
                    .iter()
                    .skip(1)
                    .take(2)
                    .filter_map(|operand| match operand {
                        Operand::IdRef(id) => labels.get(id).copied(),
                        _ => None,
                    })
                    .collect(),
                Op::Switch => terminator
                    .operands
                    .iter()
                    .enumerate()
                    .skip(1)
                    .filter(|(index, _)| index % 2 == 1)
                    .filter_map(|(_, operand)| match operand {
                        Operand::IdRef(id) => labels.get(id).copied(),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        })
        .collect::<Vec<Vec<usize>>>();
    let mut predecessors = vec![Vec::new(); function.blocks.len()];
    for (block, block_successors) in successors.iter().enumerate() {
        for &successor in block_successors {
            predecessors[successor].push(block);
        }
    }
    let (reachable, dominance, _) = crate::dominators::dominance(&successors, &predecessors);
    let loop_headers = function
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            block
                .instructions
                .iter()
                .any(|instruction| instruction.class.opcode == Op::LoopMerge)
                .then_some(index)
        })
        .collect::<HashSet<_>>();
    successors.iter().enumerate().any(|(block, targets)| {
        reachable[block]
            && targets.iter().any(|target| {
                let (Some((target_in, target_out)), Some((block_in, block_out))) =
                    (dominance[*target], dominance[block])
                else {
                    return false;
                };
                target_in <= block_in && block_out <= target_out && !loop_headers.contains(target)
            })
    })
}

pub const BOUNDED_RELOOPER_MAX_BLOCKS: usize = 1024;

pub const CFG_EMIT_RELOOPER_MAX_BLOCKS: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};
    use spirv::{AddressingModel, Decoration, MemoryModel};

    fn inst(op: Op, ty: Option<Word>, id: Option<Word>, operands: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, operands)
    }

    fn set_logical_memory_model(module: &mut Module) {
        module.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
    }

    #[test]
    fn physical_atomic_conversion_is_constructed_as_an_offset_zero_lvalue() {
        let uint = 1;
        let ulong = 2;
        let physical_uint = 3;
        let zero = 4;
        let address = 5;
        let uchar = 6;
        let uchar_zero = 7;
        let uint_null = 8;
        let converted = 11;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(12));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(uint),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(ulong),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(physical_uint),
                vec![
                    Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                    Operand::IdRef(uint),
                ],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(uchar),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantNull, Some(uchar), Some(uchar_zero), vec![]),
            inst(Op::ConstantNull, Some(uint), Some(uint_null), vec![]),
            inst(
                Op::Constant,
                Some(uint),
                Some(zero),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(ulong),
                Some(address),
                vec![Operand::LiteralBit64(64)],
            ),
        ];
        let mut function = Function::new();
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(10), vec![])),
            instructions: vec![
                inst(
                    Op::ConvertUToPtr,
                    Some(physical_uint),
                    Some(converted),
                    vec![Operand::IdRef(address)],
                ),
                inst(
                    Op::AtomicStore,
                    None,
                    None,
                    vec![
                        Operand::IdRef(converted),
                        Operand::IdScope(zero),
                        Operand::IdMemorySemantics(zero),
                        Operand::IdRef(zero),
                    ],
                ),
                inst(Op::Return, None, None, vec![]),
            ],
        });
        module.functions.push(function);

        assert!(construct_physical_atomic_pointer_lvalues_module(
            &mut module
        ));
        assert!(!construct_physical_atomic_pointer_lvalues_module(
            &mut module
        ));

        let body = &module.functions[0].blocks[0].instructions;
        let atomic = body
            .iter()
            .find(|instruction| instruction.class.opcode == Op::AtomicStore)
            .expect("atomic store");
        let Operand::IdRef(member_pointer) = atomic.operands[0] else {
            panic!("atomic pointer is an id")
        };
        let member = body
            .iter()
            .find(|instruction| instruction.result_id == Some(member_pointer))
            .expect("member pointer");
        assert_eq!(member.class.opcode, Op::AccessChain);
        assert_eq!(member.result_type, Some(physical_uint));
        assert_eq!(member.operands.get(1), Some(&Operand::IdRef(zero)));
        let Operand::IdRef(wrapper_pointer) = member.operands[0] else {
            panic!("wrapper pointer is an id")
        };
        assert!(body.iter().any(|instruction| {
            instruction.result_id == Some(wrapper_pointer)
                && instruction.class.opcode == Op::ConvertUToPtr
                && instruction.operands == [Operand::IdRef(address)]
        }));
        assert!(module.annotations.iter().any(|instruction| {
            instruction.class.opcode == Op::MemberDecorate
                && instruction.operands.get(1) == Some(&Operand::LiteralBit32(0))
                && instruction.operands.get(2) == Some(&Operand::Decoration(Decoration::Offset))
                && instruction.operands.get(3) == Some(&Operand::LiteralBit32(0))
        }));
    }

    fn module_with_typed_instruction(instruction: Instruction) -> Module {
        let mut module = Module::new();
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
                vec![Operand::LiteralBit32(32)],
            ),
            inst(Op::TypeBool, None, Some(3), vec![]),
            inst(
                Op::Constant,
                Some(1),
                Some(4),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(5),
                vec![Operand::LiteralBit32(2)],
            ),
            inst(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(0x3f80_0000)],
            ),
            inst(Op::ConstantTrue, Some(3), Some(7), vec![]),
            inst(
                Op::TypeVector,
                None,
                Some(8),
                vec![Operand::IdRef(1), Operand::LiteralBit32(2)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(9),
                vec![Operand::IdRef(3), Operand::LiteralBit32(2)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(10),
                vec![Operand::IdRef(3), Operand::LiteralBit32(4)],
            ),
            inst(Op::Undef, Some(8), Some(11), vec![]),
            inst(Op::Undef, Some(8), Some(12), vec![]),
            inst(Op::Undef, Some(9), Some(13), vec![]),
            inst(Op::Undef, Some(10), Some(14), vec![]),
        ];
        let mut function = Function::new();
        function.blocks = vec![Block {
            label: Some(inst(Op::Label, None, Some(20), vec![])),
            instructions: vec![instruction, inst(Op::Return, None, None, vec![])],
        }];
        function.def = Some(inst(
            Op::Function,
            Some(30),
            Some(32),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(31),
            ],
        ));
        function.end = Some(inst(Op::FunctionEnd, None, None, vec![]));
        module.functions.push(function);
        let mut header = ModuleHeader::new(100);
        header.set_version(1, 5);
        module.header = Some(header);
        module.capabilities.push(inst(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(spirv::Capability::Shader)],
        ));
        set_logical_memory_model(&mut module);
        module.entry_points.push(inst(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                Operand::IdRef(32),
                Operand::LiteralString("main".to_string()),
            ],
        ));
        module.execution_modes.push(inst(
            Op::ExecutionMode,
            None,
            None,
            vec![
                Operand::IdRef(32),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
            ],
        ));
        module
            .types_global_values
            .push(inst(Op::TypeVoid, None, Some(30), vec![]));
        module.types_global_values.push(inst(
            Op::TypeFunction,
            None,
            Some(31),
            vec![Operand::IdRef(30)],
        ));
        module
    }

    fn owned_invalid_error(module: &Module) -> Option<String> {
        match owned_module_failure(module) {
            Some(OwnedModuleFailure::Invalid(error)) => Some(error),
            Some(OwnedModuleFailure::CfgConstruction(error)) => {
                panic!("ordinary value typing was misclassified as CFG construction: {error}")
            }
            Some(OwnedModuleFailure::TypeConstruction(error)) => {
                panic!("ordinary value typing was misclassified as type construction: {error}")
            }
            Some(OwnedModuleFailure::RawBufferConstruction(error)) => {
                panic!(
                    "ordinary value typing was misclassified as raw-buffer construction: {error}"
                )
            }
            None => None,
        }
    }

    #[test]
    fn owned_validity_check_enforces_ordinary_instruction_type_classes() {
        let cases = [
            (
                inst(
                    Op::IAdd,
                    Some(1),
                    Some(21),
                    vec![Operand::IdRef(4), Operand::IdRef(6)],
                ),
                "native emitter: owned IAdd operands do not match its result type: operand 1 is \
                 %6, of type %2 (TypeFloat 32), but the result type is %1 (TypeInt 32 0)",
            ),
            (
                inst(
                    Op::IAdd,
                    Some(2),
                    Some(21),
                    vec![Operand::IdRef(6), Operand::IdRef(6)],
                ),
                "native emitter: owned IAdd has an invalid result type class",
            ),
            (
                inst(
                    Op::FAdd,
                    Some(1),
                    Some(21),
                    vec![Operand::IdRef(4), Operand::IdRef(5)],
                ),
                "native emitter: owned FAdd has an invalid result type class",
            ),
            (
                inst(
                    Op::LogicalAnd,
                    Some(3),
                    Some(21),
                    vec![Operand::IdRef(7), Operand::IdRef(4)],
                ),
                "native emitter: owned LogicalAnd operands do not match its result type: \
                 operand 1 is %4, of type %1 (TypeInt 32 0), but the result type is %3 (TypeBool)",
            ),
            (
                inst(Op::CopyObject, Some(1), Some(21), vec![Operand::IdRef(6)]),
                "native emitter: owned CopyObject operands do not match its result type: \
                 operand 0 is %6, of type %2 (TypeFloat 32), but the result type is %1 (TypeInt 32 0)",
            ),
            (
                inst(
                    Op::Select,
                    Some(1),
                    Some(21),
                    vec![Operand::IdRef(4), Operand::IdRef(4), Operand::IdRef(5)],
                ),
                "native emitter: owned OpSelect condition does not match its result lane shape",
            ),
            (
                inst(
                    Op::Select,
                    Some(8),
                    Some(21),
                    vec![Operand::IdRef(14), Operand::IdRef(11), Operand::IdRef(12)],
                ),
                "native emitter: owned OpSelect condition does not match its result lane shape",
            ),
        ];

        for (instruction, expected) in cases {
            let module = module_with_typed_instruction(instruction);
            assert_eq!(owned_invalid_error(&module).as_deref(), Some(expected));
        }
    }

    #[test]
    fn owned_validity_check_enforces_comparison_classes_and_lane_shapes() {
        let cases = [
            (
                inst(
                    Op::IEqual,
                    Some(3),
                    Some(21),
                    vec![Operand::IdRef(4), Operand::IdRef(6)],
                ),
                "native emitter: owned IEqual operands have different types",
            ),
            (
                inst(
                    Op::IEqual,
                    Some(3),
                    Some(21),
                    vec![Operand::IdRef(7), Operand::IdRef(7)],
                ),
                "native emitter: owned IEqual has an invalid operand type class",
            ),
            (
                inst(
                    Op::FOrdEqual,
                    Some(1),
                    Some(21),
                    vec![Operand::IdRef(6), Operand::IdRef(6)],
                ),
                "native emitter: owned FOrdEqual result does not preserve its Boolean lane shape",
            ),
            (
                inst(
                    Op::IEqual,
                    Some(10),
                    Some(21),
                    vec![Operand::IdRef(11), Operand::IdRef(12)],
                ),
                "native emitter: owned IEqual result does not preserve its Boolean lane shape",
            ),
        ];

        for (instruction, expected) in cases {
            let module = module_with_typed_instruction(instruction);
            assert_eq!(owned_invalid_error(&module).as_deref(), Some(expected));
        }
    }

    #[test]
    fn owned_arithmetic_validity_check_matches_vulkan_validation() {
        let module = module_with_typed_instruction(inst(
            Op::IAdd,
            Some(2),
            Some(21),
            vec![Operand::IdRef(4), Operand::IdRef(5)],
        ));

        assert_eq!(
            owned_invalid_error(&module).as_deref(),
            Some(
                "native emitter: owned IAdd operands do not match its result type: operand 0 is \
                 %4, of type %1 (TypeInt 32 0), but the result type is %2 (TypeFloat 32)"
            )
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let tmp = std::env::temp_dir().join(format!(
            "metal2vulkan_owned_arithmetic_type_{}",
            std::process::id()
        ));
        let validation = crate::tools::spirv_val_bytes(&bytes, &tmp);
        let _ = std::fs::remove_dir(&tmp);
        assert!(
            validation.is_err(),
            "spirv-val must reject the malformed arithmetic type contract"
        );
    }

    #[test]
    fn drops_debug_records_for_undefined_targets() {
        let mut module = Module::default();
        module.types_global_values = vec![inst(
            Op::TypeInt,
            None,
            Some(1),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        )];
        module.debug_names = vec![
            inst(
                Op::Name,
                None,
                None,
                vec![Operand::IdRef(1), Operand::LiteralString("live".into())],
            ),
            inst(
                Op::Name,
                None,
                None,
                vec![Operand::IdRef(99), Operand::LiteralString("dead".into())],
            ),
        ];
        module.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(1),
                    Operand::Decoration(Decoration::RelaxedPrecision),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(99),
                    Operand::Decoration(Decoration::RelaxedPrecision),
                ],
            ),
        ];

        assert!(drop_dangling_debug_targets_module(&mut module));
        assert_eq!(module.debug_names.len(), 1);
        assert_eq!(module.annotations.len(), 1);
        assert_eq!(
            module.debug_names[0].operands.first(),
            Some(&Operand::IdRef(1))
        );
        assert_eq!(
            module.annotations[0].operands.first(),
            Some(&Operand::IdRef(1))
        );
    }

    #[test]
    fn prunes_unobserved_pointer_null_and_its_debug_records() {
        let mut module = Module::default();
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(2),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::ConstantNull, Some(2), Some(3), vec![]),
        ];
        module.debug_names.push(inst(
            Op::Name,
            None,
            None,
            vec![Operand::IdRef(3), Operand::LiteralString("dead".into())],
        ));

        assert!(prune_unused_null_and_undef_constants_module(&mut module));
        assert!(module
            .types_global_values
            .iter()
            .all(|instruction| instruction.result_id != Some(3)));
        assert!(module.debug_names.is_empty());
    }

    fn module_with_forwarded_bda_address(extracted_path: Vec<Operand>) -> Module {
        let mut module = Module::new();
        module.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::PhysicalStorageBuffer64),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::TypeStruct,
                None,
                Some(4),
                vec![Operand::IdRef(3), Operand::IdRef(2)],
            ),
            inst(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            inst(Op::ConstantNull, Some(2), Some(6), vec![]),
            inst(
                Op::Constant,
                Some(2),
                Some(7),
                vec![Operand::LiteralBit32(1), Operand::LiteralBit32(0)],
            ),
            inst(Op::Undef, Some(5), Some(8), vec![]),
        ];
        let extract_type = if extracted_path.last() == Some(&Operand::LiteralBit32(0)) {
            3
        } else {
            2
        };
        let mut extract_operands = vec![Operand::IdRef(10)];
        extract_operands.extend(extracted_path);
        let mut function = Function::new();
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(9), vec![])),
            instructions: vec![
                inst(
                    Op::CompositeInsert,
                    Some(5),
                    Some(10),
                    vec![
                        Operand::IdRef(7),
                        Operand::IdRef(8),
                        Operand::LiteralBit32(0),
                        Operand::LiteralBit32(0),
                    ],
                ),
                inst(
                    Op::CompositeExtract,
                    Some(extract_type),
                    Some(11),
                    extract_operands,
                ),
            ],
        });
        module.functions.push(function);
        module
    }

    #[test]
    fn constructs_unobserved_pointer_field_for_forwarded_bda_address() {
        let mut module = module_with_forwarded_bda_address(vec![
            Operand::LiteralBit32(0),
            Operand::LiteralBit32(1),
        ]);

        assert!(lower_unobserved_bda_aggregate_pointer_fields_module(&mut module).unwrap());
        let nested = module
            .types_global_values
            .iter()
            .find(|instruction| instruction.result_id == Some(4))
            .unwrap();
        assert_eq!(nested.operands.first(), Some(&Operand::IdRef(2)));

        let mut observed = module_with_forwarded_bda_address(vec![
            Operand::LiteralBit32(0),
            Operand::LiteralBit32(0),
        ]);
        assert!(!lower_unobserved_bda_aggregate_pointer_fields_module(&mut observed).unwrap());
        let nested = observed
            .types_global_values
            .iter()
            .find(|instruction| instruction.result_id == Some(4))
            .unwrap();
        assert_eq!(nested.operands.first(), Some(&Operand::IdRef(3)));
    }

    #[test]
    fn unchanged_owner_does_not_mask_an_unrelated_dangling_record() {
        let mut module = Module::default();
        module.debug_names.push(inst(
            Op::Name,
            None,
            None,
            vec![Operand::IdRef(99), Operand::LiteralString("unowned".into())],
        ));

        assert!(!finish_id_deleting_rewrite(
            &mut module,
            HashSet::default(),
            false
        ));
        assert_eq!(module.debug_names.len(), 1);
    }

    #[test]
    fn relooper_removes_only_records_for_ids_in_its_replaced_cfg() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(3));
        let mut function = Function::new();
        function.blocks = vec![
            Block {
                label: Some(inst(Op::Label, None, Some(1), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(2)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(2), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
        ];
        module.functions.push(function);
        module.debug_names = vec![
            inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(1),
                    Operand::LiteralString("old-entry".into()),
                ],
            ),
            inst(
                Op::Name,
                None,
                None,
                vec![Operand::IdRef(99), Operand::LiteralString("unowned".into())],
            ),
        ];

        let defined_before = defined_result_ids(&module);
        let changed = relooper::rewrite_to_relooper(&mut module, 16);
        assert!(changed, "expected a construction");
        finish_id_deleting_rewrite(&mut module, defined_before, changed);
        add_native_module_capabilities(&mut module);
        assert_eq!(module.debug_names.len(), 1);
        assert_eq!(module.debug_names[0].operands[0], Operand::IdRef(99));
    }

    #[test]
    fn missing_rejected_helper_identity_does_not_construct_unselected_module() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(3));
        let mut function = Function::new();
        function.blocks = vec![
            Block {
                label: Some(inst(Op::Label, None, Some(1), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(2)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(2), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
        ];
        module.functions.push(function);

        construct_cfg_functions_module(
            &mut module,
            &crate::fxhash::set_from(["inlined_helper".to_string()]),
        )
        .expect("unselected module remains unchanged");
        assert_eq!(module.functions[0].blocks.len(), 2);
    }

    #[test]
    fn a_named_function_folded_to_one_returning_block_needs_no_construction() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        let mut function = Function::new();
        function.def = Some(inst(Op::Function, None, Some(15), vec![]));
        function.blocks = vec![Block {
            label: Some(inst(Op::Label, None, Some(16), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        }];
        module.functions.push(function);
        module.debug_names.push(inst(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(15),
                Operand::LiteralString("folded_helper".to_string()),
            ],
        ));

        construct_cfg_functions_module(
            &mut module,
            &crate::fxhash::set_from(["folded_helper".to_string()]),
        )
        .expect("a single returning block is already structured");
        assert_eq!(module.functions[0].blocks.len(), 1);
    }

    #[test]
    fn a_named_function_whose_one_block_branches_to_itself_stays_selected() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        let mut function = Function::new();
        function.def = Some(inst(Op::Function, None, Some(15), vec![]));
        function.blocks = vec![Block {
            label: Some(inst(Op::Label, None, Some(16), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(16)])],
        }];
        module.functions.push(function);
        module.debug_names.push(inst(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(15),
                Operand::LiteralString("self_loop".to_string()),
            ],
        ));

        let error = construct_cfg_functions_module(
            &mut module,
            &crate::fxhash::set_from(["self_loop".to_string()]),
        )
        .expect_err("a self-branching block is not already structured");
        assert!(
            error.contains("cannot be constructed"),
            "expected the selection to survive to the decline, got {error}"
        );
    }

    #[test]
    fn unowned_conditional_selects_construction_without_validation() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(14));
        let mut function = Function::new();
        function.def = Some(inst(Op::Function, None, Some(10), vec![]));
        function.blocks = vec![
            Block {
                label: Some(inst(Op::Label, None, Some(11), vec![])),
                instructions: vec![inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(1), Operand::IdRef(12), Operand::IdRef(13)],
                )],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(12), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(13), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
        ];
        module.functions.push(function);

        construct_cfg_functions_module(&mut module, &HashSet::default())
            .expect("unowned header construction");
        assert_ne!(module.functions[0].blocks.len(), 3);
    }

    #[test]
    fn backedge_to_selection_header_selects_construction_without_validation() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(16));
        let mut function = Function::new();
        function.def = Some(inst(Op::Function, None, Some(10), vec![]));
        function.blocks = vec![
            Block {
                label: Some(inst(Op::Label, None, Some(11), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(12)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(12), vec![])),
                instructions: vec![
                    inst(
                        Op::SelectionMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(15),
                            Operand::SelectionControl(spirv::SelectionControl::NONE),
                        ],
                    ),
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(1), Operand::IdRef(13), Operand::IdRef(15)],
                    ),
                ],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(13), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(12)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(15), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
        ];
        module.functions.push(function);

        construct_cfg_functions_module(&mut module, &HashSet::default())
            .expect("unowned backedge construction");
        assert_ne!(module.functions[0].blocks.len(), 4);
    }

    fn loop_of_unstructured_diamonds(diamonds: u32) -> Module {
        let (void, uint, bool_ty, fn_ty) = (1, 2, 3, 4);
        let (uint_0, uint_1, uint_64) = (5, 6, 7);
        let (entry, header, latch, exit) = (100, 101, 102, 103);
        let (i, cmp, next) = (300, 301, 302);
        let body = |k: u32| 1000 + k * 10;
        let then = |k: u32| 1001 + k * 10;
        let join = |k: u32| 1002 + k * 10;
        let cond = |k: u32| 5000 + k * 10;
        let bumped = |k: u32| 5001 + k * 10;
        let merged = |k: u32| 5002 + k * 10;

        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20000));
        set_logical_memory_model(&mut module);
        module.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(void), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(uint),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(bool_ty), vec![]),
            inst(
                Op::TypeFunction,
                None,
                Some(fn_ty),
                vec![Operand::IdRef(void)],
            ),
            inst(
                Op::Constant,
                Some(uint),
                Some(uint_0),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(uint),
                Some(uint_1),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(uint),
                Some(uint_64),
                vec![Operand::LiteralBit32(64)],
            ),
        ];

        let mut function = Function::new();
        function.def = Some(inst(
            Op::Function,
            Some(void),
            Some(20),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(fn_ty),
            ],
        ));
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(entry), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(header)])],
        });
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(header), vec![])),
            instructions: vec![
                inst(
                    Op::Phi,
                    Some(uint),
                    Some(i),
                    vec![
                        Operand::IdRef(uint_0),
                        Operand::IdRef(entry),
                        Operand::IdRef(next),
                        Operand::IdRef(latch),
                    ],
                ),
                inst(
                    Op::ULessThan,
                    Some(bool_ty),
                    Some(cmp),
                    vec![Operand::IdRef(i), Operand::IdRef(uint_64)],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![
                        Operand::IdRef(cmp),
                        Operand::IdRef(body(0)),
                        Operand::IdRef(exit),
                    ],
                ),
            ],
        });
        for k in 0..diamonds {
            let after = if k + 1 == diamonds {
                latch
            } else {
                body(k + 1)
            };
            function.blocks.push(Block {
                label: Some(inst(Op::Label, None, Some(body(k)), vec![])),
                instructions: vec![
                    inst(
                        Op::IEqual,
                        Some(bool_ty),
                        Some(cond(k)),
                        vec![Operand::IdRef(i), Operand::IdRef(uint_1)],
                    ),
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![
                            Operand::IdRef(cond(k)),
                            Operand::IdRef(then(k)),
                            Operand::IdRef(join(k)),
                        ],
                    ),
                ],
            });
            function.blocks.push(Block {
                label: Some(inst(Op::Label, None, Some(then(k)), vec![])),
                instructions: vec![
                    inst(
                        Op::IAdd,
                        Some(uint),
                        Some(bumped(k)),
                        vec![Operand::IdRef(i), Operand::IdRef(uint_1)],
                    ),
                    inst(Op::Branch, None, None, vec![Operand::IdRef(join(k))]),
                ],
            });
            function.blocks.push(Block {
                label: Some(inst(Op::Label, None, Some(join(k)), vec![])),
                instructions: vec![
                    inst(
                        Op::Phi,
                        Some(uint),
                        Some(merged(k)),
                        vec![
                            Operand::IdRef(bumped(k)),
                            Operand::IdRef(then(k)),
                            Operand::IdRef(i),
                            Operand::IdRef(body(k)),
                        ],
                    ),
                    inst(Op::Branch, None, None, vec![Operand::IdRef(after)]),
                ],
            });
        }
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(latch), vec![])),
            instructions: vec![
                inst(
                    Op::IAdd,
                    Some(uint),
                    Some(next),
                    vec![Operand::IdRef(i), Operand::IdRef(uint_1)],
                ),
                inst(Op::Branch, None, None, vec![Operand::IdRef(header)]),
            ],
        });
        function.blocks.push(Block {
            label: Some(inst(Op::Label, None, Some(exit), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        });
        module.functions.push(function);
        module
    }

    fn emitted_shape(function: &Function) -> (usize, usize, usize, usize) {
        let body = || function.blocks.iter().flat_map(|block| &block.instructions);
        let blocks = function.blocks.len();
        let loop_merges = body()
            .filter(|instruction| instruction.class.opcode == Op::LoopMerge)
            .count();
        let switch_cases = body()
            .filter(|instruction| instruction.class.opcode == Op::Switch)
            .map(|instruction| instruction.operands.len().saturating_sub(2) / 2)
            .sum();
        let function_variables = body()
            .filter(|instruction| {
                instruction.class.opcode == Op::Variable
                    && matches!(
                        instruction.operands.first(),
                        Some(Operand::StorageClass(StorageClass::Function))
                    )
            })
            .count();
        (blocks, loop_merges, switch_cases, function_variables)
    }

    #[test]
    fn reducible_control_flow_is_not_constructed_as_a_whole_function_dispatch() {
        const DIAMONDS: u32 = 16;
        let mut module = loop_of_unstructured_diamonds(DIAMONDS);
        let before = module.functions[0].blocks.len();
        construct_cfg_functions_module(&mut module, &HashSet::default()).expect("construction");

        let (blocks, loop_merges, switch_cases, function_variables) =
            emitted_shape(&module.functions[0]);
        assert!(
            loop_merges >= 1,
            "the loop must survive as a real loop construct, not as a dispatch over its body; \
             got {loop_merges} OpLoopMerge across {blocks} blocks"
        );
        assert!(
            switch_cases * 2 < blocks,
            "dispatch must cover a minority of blocks; {switch_cases} switch cases over {blocks} \
             blocks is the whole-function state machine that hangs the driver"
        );
        assert!(
            function_variables < DIAMONDS as usize,
            "nesting must keep crossing values in registers; {function_variables} function-scope \
             variables for {DIAMONDS} diamonds is the register-demotion the driver chokes on"
        );
        assert!(
            before > 0 && blocks > 0,
            "the function must still have a body"
        );
    }

    #[test]
    fn a_larger_reducible_function_still_nests_instead_of_flattening() {
        let mut module = loop_of_unstructured_diamonds(64);
        construct_cfg_functions_module(&mut module, &HashSet::default()).expect("construction");
        let (blocks, loop_merges, switch_cases, function_variables) =
            emitted_shape(&module.functions[0]);
        assert!(loop_merges >= 1, "{blocks} blocks, no loop construct");
        assert!(
            switch_cases * 2 < blocks,
            "{switch_cases} switch cases over {blocks} blocks is a dispatch state machine"
        );
        assert!(
            function_variables < 64,
            "{function_variables} function-scope variables is register demotion at scale"
        );
    }
}
