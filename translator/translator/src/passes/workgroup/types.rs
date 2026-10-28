use super::*;
use crate::passes::stage_input::array_type;

pub(in crate::passes) fn split_explicit_layout_type_aliases(
    ctx: &mut Ctx,
    buffer_structs: &[(Word, Word)],
    defs: &mut HashMap<Word, Instruction>,
) {
    let block_structs = buffer_structs
        .iter()
        .map(|(_, struct_ty)| *struct_ty)
        .collect::<HashSet<_>>();
    if block_structs.is_empty() {
        return;
    }

    split_nested_block_root_aliases(ctx, &block_structs, defs);

    let laid_out_types = layout_types_reachable_from(&block_structs, defs);

    let mut shallow = vec![];
    for (idx, inst) in ctx.module.types_global_values.iter().enumerate() {
        if inst.class.opcode != Op::Variable
            || inst.operands.first() != Some(&Operand::StorageClass(StorageClass::Workgroup))
        {
            continue;
        }
        let Some(ptr_ty) = inst.result_type else {
            continue;
        };
        let Some(pointee) = ptr_pointee(defs, ptr_ty) else {
            continue;
        };
        let Some(def) = defs.get(&pointee) else {
            continue;
        };
        if def.class.opcode == Op::TypeStruct && laid_out_types.contains(&pointee) {
            shallow.push((idx, def.operands.clone()));
        }
    }
    for (idx, operands) in shallow.into_iter().rev() {
        let clone_ty = ctx.module.fresh_id();
        let clone_inst = type_inst(Op::TypeStruct, clone_ty, operands);
        let clone_ptr_ty = ctx.module.fresh_id();
        let clone_ptr_inst = type_inst(
            Op::TypePointer,
            clone_ptr_ty,
            vec![
                Operand::StorageClass(StorageClass::Workgroup),
                Operand::IdRef(clone_ty),
            ],
        );
        ctx.module
            .types_global_values
            .insert(idx, clone_inst.clone());
        ctx.module
            .types_global_values
            .insert(idx + 1, clone_ptr_inst.clone());
        ctx.module.types_global_values[idx + 2].result_type = Some(clone_ptr_ty);
        defs.insert(clone_ty, clone_inst);
        defs.insert(clone_ptr_ty, clone_ptr_inst);
    }
    let unlaid_roots = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .filter(|inst| {
            inst.class.opcode == Op::TypePointer
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(
                        StorageClass::Workgroup | StorageClass::Function | StorageClass::Private
                    ))
                )
        })
        .filter_map(|inst| inst.operands.get(1))
        .filter_map(|operand| match operand {
            Operand::IdRef(pointee) => Some(*pointee),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut resolved = HashMap::default();
    let mut cloned_types = vec![];
    for root in unlaid_roots {
        clone_unlaid_layout_conflicts(
            ctx,
            root,
            &laid_out_types,
            defs,
            &mut resolved,
            &mut cloned_types,
        );
    }
    let cloned = resolved
        .into_iter()
        .filter(|(old, new)| old != new)
        .collect::<HashMap<_, _>>();
    if cloned.is_empty() {
        return;
    }

    for inst in &cloned_types {
        if let Some(id) = inst.result_id {
            defs.insert(id, inst.clone());
        }
    }

    let insert_at = ctx
        .module
        .types_global_values
        .iter()
        .position(|inst| {
            inst.class.opcode == Op::TypePointer
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(
                        StorageClass::Workgroup | StorageClass::Function | StorageClass::Private
                    ))
                )
                && matches!(
                    inst.operands.get(1),
                    Some(Operand::IdRef(pointee)) if cloned.contains_key(pointee)
                )
        })
        .unwrap_or(ctx.module.types_global_values.len());
    ctx.module
        .types_global_values
        .splice(insert_at..insert_at, cloned_types);

    let mut cache_updates = vec![];
    repoint_unlaid_pointer_types(
        &mut ctx.module.types_global_values,
        &cloned,
        defs,
        &mut cache_updates,
    );
    repoint_unlaid_pointer_types(&mut ctx.new_globals, &cloned, defs, &mut cache_updates);
    for instruction in ctx
        .module
        .all_inst_iter_mut()
        .chain(ctx.new_globals.iter_mut())
    {
        if let Some(replacement) = instruction
            .result_type
            .and_then(|result_type| cloned.get(&result_type).copied())
        {
            instruction.result_type = Some(replacement);
        }
    }
    for (id, old_operands, new_operands) in cache_updates {
        let old_key = (Op::TypePointer, None, old_operands);
        if ctx.struct_cache.get(&old_key) == Some(&id) {
            ctx.struct_cache.remove(&old_key);
        }
        ctx.struct_cache
            .insert((Op::TypePointer, None, new_operands), id);
    }
}

fn split_nested_block_root_aliases(
    ctx: &mut Ctx,
    block_structs: &HashSet<Word>,
    defs: &mut HashMap<Word, Instruction>,
) {
    let mut roots = block_structs.iter().copied().collect::<Vec<_>>();
    roots.sort_unstable();
    for root in roots {
        let Some(root_def) = defs.get(&root).cloned() else {
            continue;
        };
        if root_def.class.opcode != Op::TypeStruct {
            continue;
        }
        let mut resolved = HashMap::default();
        let mut cloned_types = Vec::new();
        let mut operands = root_def.operands.clone();
        let mut changed = false;
        for operand in &mut operands {
            let Operand::IdRef(member) = operand else {
                continue;
            };
            let cloned = clone_nested_block_path(
                ctx,
                *member,
                block_structs,
                defs,
                &mut resolved,
                &mut cloned_types,
            );
            changed |= cloned != *member;
            *member = cloned;
        }
        if !changed {
            continue;
        }
        let Some(root_position) = ctx
            .module
            .types_global_values
            .iter()
            .position(|instruction| instruction.result_id == Some(root))
        else {
            continue;
        };
        for instruction in &cloned_types {
            if let Some(id) = instruction.result_id {
                defs.insert(id, instruction.clone());
            }
        }
        ctx.module
            .types_global_values
            .splice(root_position..root_position, cloned_types);
        let root_position = ctx
            .module
            .types_global_values
            .iter()
            .position(|instruction| instruction.result_id == Some(root))
            .expect("existing block root remains present");
        let old_operands = ctx.module.types_global_values[root_position]
            .operands
            .clone();
        ctx.module.types_global_values[root_position].operands = operands.clone();
        let old_key = (Op::TypeStruct, None, old_operands);
        if ctx.struct_cache.get(&old_key) == Some(&root) {
            ctx.struct_cache.remove(&old_key);
        }
        ctx.struct_cache
            .insert((Op::TypeStruct, None, operands), root);
        defs.insert(root, ctx.module.types_global_values[root_position].clone());
    }
}

fn clone_nested_block_path(
    ctx: &mut Ctx,
    ty: Word,
    block_structs: &HashSet<Word>,
    defs: &HashMap<Word, Instruction>,
    resolved: &mut HashMap<Word, Word>,
    cloned_types: &mut Vec<Instruction>,
) -> Word {
    if let Some(&resolved) = resolved.get(&ty) {
        return resolved;
    }
    let Some(definition) = defs.get(&ty) else {
        resolved.insert(ty, ty);
        return ty;
    };
    let mut operands = definition.operands.clone();
    let mut child_changed = false;
    match definition.class.opcode {
        Op::TypeStruct => {
            for operand in &mut operands {
                let Operand::IdRef(member) = operand else {
                    continue;
                };
                let cloned = clone_nested_block_path(
                    ctx,
                    *member,
                    block_structs,
                    defs,
                    resolved,
                    cloned_types,
                );
                child_changed |= cloned != *member;
                *member = cloned;
            }
        }
        Op::TypeArray | Op::TypeRuntimeArray => {
            if let Some(Operand::IdRef(element)) = operands.first_mut() {
                let cloned = clone_nested_block_path(
                    ctx,
                    *element,
                    block_structs,
                    defs,
                    resolved,
                    cloned_types,
                );
                child_changed = cloned != *element;
                *element = cloned;
            }
        }
        _ => {
            resolved.insert(ty, ty);
            return ty;
        }
    }
    if !child_changed && !block_structs.contains(&ty) {
        resolved.insert(ty, ty);
        return ty;
    }
    let cloned = ctx.module.fresh_id();
    cloned_types.push(type_inst(definition.class.opcode, cloned, operands));
    if definition.class.opcode == Op::TypeStruct {
        if let Some(offsets) = ctx.air_struct_offsets.get(&ty).cloned() {
            ctx.air_struct_offsets.insert(cloned, offsets);
        }
    }
    resolved.insert(ty, cloned);
    cloned
}

pub(in crate::passes) fn clone_unlaid_layout_conflicts(
    ctx: &mut Ctx,
    ty: Word,
    laid_out_types: &HashSet<Word>,
    defs: &HashMap<Word, Instruction>,
    resolved: &mut HashMap<Word, Word>,
    cloned_types: &mut Vec<Instruction>,
) -> Word {
    if let Some(&resolved_ty) = resolved.get(&ty) {
        return resolved_ty;
    }
    let Some(def) = defs.get(&ty) else {
        resolved.insert(ty, ty);
        return ty;
    };
    let mut operands = def.operands.clone();
    let mut child_changed = false;
    match def.class.opcode {
        Op::TypeStruct => {
            for operand in &mut operands {
                let Operand::IdRef(member) = operand else {
                    continue;
                };
                let cloned = clone_unlaid_layout_conflicts(
                    ctx,
                    *member,
                    laid_out_types,
                    defs,
                    resolved,
                    cloned_types,
                );
                child_changed |= cloned != *member;
                *member = cloned;
            }
        }
        Op::TypeArray | Op::TypeRuntimeArray => {
            if let Some(Operand::IdRef(elem)) = operands.first_mut() {
                let cloned = clone_unlaid_layout_conflicts(
                    ctx,
                    *elem,
                    laid_out_types,
                    defs,
                    resolved,
                    cloned_types,
                );
                child_changed = cloned != *elem;
                *elem = cloned;
            }
        }
        _ => {
            resolved.insert(ty, ty);
            return ty;
        }
    }
    if !child_changed && !laid_out_types.contains(&ty) {
        resolved.insert(ty, ty);
        return ty;
    }

    let cloned = ctx.module.fresh_id();
    cloned_types.push(type_inst(def.class.opcode, cloned, operands));
    if def.class.opcode == Op::TypeStruct {
        if let Some(offsets) = ctx.air_struct_offsets.get(&ty).cloned() {
            ctx.air_struct_offsets.insert(cloned, offsets);
        }
    }
    resolved.insert(ty, cloned);
    cloned
}

pub(in crate::passes) fn repoint_unlaid_pointer_types(
    instructions: &mut [Instruction],
    cloned: &HashMap<Word, Word>,
    defs: &mut HashMap<Word, Instruction>,
    cache_updates: &mut Vec<(Word, Vec<Operand>, Vec<Operand>)>,
) {
    for inst in instructions {
        if inst.class.opcode != Op::TypePointer {
            continue;
        }
        let Some(Operand::StorageClass(storage)) = inst.operands.first() else {
            continue;
        };
        let storage = *storage;
        if !matches!(
            storage,
            StorageClass::Workgroup | StorageClass::Function | StorageClass::Private
        ) {
            continue;
        }
        let Some(Operand::IdRef(pointee)) = inst.operands.get(1) else {
            continue;
        };
        let pointee = *pointee;
        let Some(&new_pointee) = cloned.get(&pointee) else {
            continue;
        };
        let Some(id) = inst.result_id else { continue };
        let old_operands = inst.operands.clone();
        inst.operands[1] = Operand::IdRef(new_pointee);
        cache_updates.push((id, old_operands, inst.operands.clone()));
        defs.insert(id, inst.clone());
    }
}

pub(in crate::passes) fn layout_types_reachable_from(
    roots: &HashSet<Word>,
    defs: &HashMap<Word, Instruction>,
) -> HashSet<Word> {
    let mut seen: HashSet<Word> = HashSet::default();
    let mut stack: Vec<Word> = roots.iter().copied().collect();
    while let Some(ty) = stack.pop() {
        let Some(def) = defs.get(&ty) else { continue };
        match def.class.opcode {
            Op::TypeStruct => {
                if !seen.insert(ty) {
                    continue;
                }
                for op in &def.operands {
                    if let Operand::IdRef(member) = op {
                        stack.push(*member);
                    }
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray => {
                if !seen.insert(ty) {
                    continue;
                }
                if let Some(Operand::IdRef(elem)) = def.operands.first() {
                    stack.push(*elem);
                }
            }
            _ => {}
        }
    }
    seen
}

pub(in crate::passes) fn build_workgroup_air_type(ctx: &mut Ctx, ty: &AirType) -> Word {
    match ty {
        AirType::Scalar(scalar) => ctx.ty_air_scalar(*scalar),
        AirType::Vec { scalar, lanes } => ctx.ty_air_vec(*scalar, *lanes),
        AirType::PackedVec { scalar, lanes } => {
            let elem = ctx.ty_air_scalar(*scalar);
            fresh_array_type(ctx, elem, *lanes)
        }
        AirType::Array { elem, len } => {
            let elem_ty = build_workgroup_air_type(ctx, elem);
            fresh_array_type(ctx, elem_ty, *len)
        }
        AirType::Matrix { scalar, cols, rows } => {
            let col = ctx.ty_air_vec(*scalar, *rows);
            let arr = fresh_array_type(ctx, col, *cols);
            let st = ctx.module.fresh_id();
            ctx.new_globals
                .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(arr)]));
            st
        }
        AirType::Struct(members) => {
            let fields = members
                .iter()
                .map(|member| Operand::IdRef(build_workgroup_air_type(ctx, &member.ty)))
                .collect();
            let st = ctx.module.fresh_id();
            ctx.new_globals.push(type_inst(Op::TypeStruct, st, fields));
            st
        }
        AirType::Opaque { size } => {
            build_workgroup_air_type(ctx, &crate::meta::storage_air_type_for_size(*size))
        }
    }
}

pub(in crate::passes) fn is_raw_workgroup_array(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
) -> bool {
    let Some((elem, _len)) = array_type(defs, ty) else {
        return false;
    };
    defs.get(&elem).is_some_and(|inst| {
        inst.class.opcode == Op::TypeInt
            && inst.operands.first() == Some(&Operand::LiteralBit32(32))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(0))
    })
}

pub(in crate::passes) fn fresh_array_type(ctx: &mut Ctx, elem: Word, len: u32) -> Word {
    let len_c = ctx.const_uint(len);
    let id = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeArray,
        id,
        vec![Operand::IdRef(elem), Operand::IdRef(len_c)],
    ));
    id
}
