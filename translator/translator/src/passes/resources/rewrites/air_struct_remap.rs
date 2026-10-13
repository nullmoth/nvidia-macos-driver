use super::*;
use crate::passes::stage_input::layout_ty_size_align;

pub(in crate::passes) fn type_after_spirv_access_operands(
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    operands: &[Operand],
) -> Option<Word> {
    let mut cur = root_ty;
    for operand in operands {
        let def = types.get(&cur)?;
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(index) = operand else {
                    return None;
                };
                let member = const_u32(types, *index)? as usize;
                let Operand::IdRef(member_ty) = def.operands.get(member)? else {
                    return None;
                };
                *member_ty
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                let Operand::IdRef(elem_ty) = def.operands.first()? else {
                    return None;
                };
                *elem_ty
            }
            _ => return None,
        };
    }
    Some(cur)
}

pub(in crate::passes) fn rewrite_record_array_buffer(
    ctx: &mut Ctx,
    entry_idx: usize,
    pid: Word,
    var: Word,
    block_ty: Word,
    elem_ty: Word,
    defs: &HashMap<Word, Instruction>,
) -> HashSet<Word> {
    let u0 = ctx.const_uint(0);
    let mut nested_air_ordinal_roots = HashSet::default();

    let mut types = defs.clone();
    for g in &ctx.new_globals {
        if let Some(id) = g.result_id {
            types.entry(id).or_insert_with(|| g.clone());
        }
    }

    let value_types = combined_value_types(ctx, entry_idx);
    let desired_pointees = pointer_leaf_use_types(ctx, entry_idx, &value_types);
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let n_inst = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .len();
        for ii in 0..n_inst {
            let rewritten_pointee = {
                let (old_indices, result_type, result_id) = {
                    let inst = &ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                    if !matches!(
                        inst.class.opcode,
                        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                    ) || inst.operands.first() != Some(&Operand::IdRef(pid))
                    {
                        (vec![], None, None)
                    } else {
                        (
                            inst.operands[1..].to_vec(),
                            inst.result_type,
                            inst.result_id,
                        )
                    }
                };
                if old_indices.is_empty() {
                    None
                } else {
                    let (operands, pointee) = if let Some((indices, pointee)) =
                        remap_direct_air_struct_access(
                            ctx,
                            &types,
                            elem_ty,
                            &old_indices,
                            result_type,
                        ) {
                        let mut operands = vec![
                            Operand::IdRef(var),
                            Operand::IdRef(u0),
                            Operand::IdRef(u0),
                        ];
                        operands.extend(indices);
                        (operands, Some(pointee))
                    } else if let Some((indices, pointee)) = result_id
                        .and_then(|rid| desired_pointees.get(&rid).and_then(|ty| *ty))
                        .and_then(|desired_pointee| {
                            remap_direct_air_struct_access_to_pointee(
                                ctx,
                                &types,
                                elem_ty,
                                &old_indices,
                                desired_pointee,
                            )
                        })
                    {
                        let mut operands = vec![
                            Operand::IdRef(var),
                            Operand::IdRef(u0),
                            Operand::IdRef(u0),
                        ];
                        operands.extend(indices);
                        (operands, Some(pointee))
                    } else if let Some((record, indices, pointee)) = remap_indexed_air_struct_access(
                        ctx,
                        &types,
                        elem_ty,
                        &old_indices,
                        result_type,
                    ) {
                        let mut operands = vec![
                            Operand::IdRef(var),
                            Operand::IdRef(u0),
                            record,
                        ];
                        operands.extend(indices);
                        (operands, Some(pointee))
                    } else if let Some((indices, pointee)) = resolve_record_member_fallback(
                        ctx,
                        &types,
                        elem_ty,
                        &old_indices,
                        result_type,
                        result_id,
                        &desired_pointees,
                    ) {
                        let mut operands = vec![
                            Operand::IdRef(var),
                            Operand::IdRef(u0),
                            Operand::IdRef(u0),
                        ];
                        operands.extend(indices);
                        (operands, Some(pointee))
                    } else {
                        let mut operands = vec![
                            Operand::IdRef(var),
                            Operand::IdRef(u0),
                        ];
                        operands.extend(old_indices);
                        let pointee =
                            type_after_spirv_access_operands(&types, block_ty, &operands[1..]);
                        (operands, pointee)
                    };
                    if let (Some(result_id), Some(result_ptr_ty), Some(pointee)) =
                        (result_id, result_type, pointee)
                    {
                        if let Some(result_pointee) = ptr_pointee(&types, result_ptr_ty) {
                            if !types_structurally_match(ctx, &types, pointee, result_pointee)
                                && types_match_with_elided_air_padding(
                                    ctx,
                                    &types,
                                    pointee,
                                    result_pointee,
                                )
                            {
                                nested_air_ordinal_roots.insert(result_id);
                            }
                        }
                    }
                    ctx.module.functions[entry_idx].blocks[bi].instructions[ii].operands = operands;
                    pointee
                }
            };
            if let Some(pointee) = rewritten_pointee {
                let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, pointee);
                ctx.module.functions[entry_idx].blocks[bi].instructions[ii].result_type =
                    Some(ptr_ty);
            }
        }
    }

    rewrite_flattened_record_array_indices(ctx, entry_idx, var, block_ty, defs);

    let mut want: Vec<Word> = vec![];
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            match inst.class.opcode {
                Op::Load if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    if let Some(t) = inst.result_type {
                        if !want.contains(&t) {
                            want.push(t);
                        }
                    }
                }
                Op::Store if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    let Some(Operand::IdRef(object)) = inst.operands.get(1) else {
                        continue;
                    };
                    if let Some(t) = value_types.get(object).copied() {
                        if !want.contains(&t) {
                            want.push(t);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut direct_root_chain: HashMap<Word, Word> = HashMap::default();
    let mut injected: Vec<Instruction> = vec![];
    for t in want {
        let path = if t == elem_ty {
            vec![0, 0]
        } else {
            let Some(mut leaf) = path_to_leaf(&types, elem_ty, t) else {
                continue;
            };
            let mut path = vec![0, 0];
            path.append(&mut leaf);
            path
        };
        let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, t);
        let id = ctx.module.fresh_id();
        let mut ops = vec![Operand::IdRef(var)];
        for _ in &path {
            ops.push(Operand::IdRef(u0));
        }
        injected.push(Instruction::new(
            Op::AccessChain,
            Some(ptr_ty),
            Some(id),
            ops,
        ));
        direct_root_chain.insert(t, id);
    }
    for blk in &mut ctx.module.functions[entry_idx].blocks {
        for inst in &mut blk.instructions {
            match inst.class.opcode {
                Op::Load if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    if let Some(&chain) = inst
                        .result_type
                        .as_ref()
                        .and_then(|t| direct_root_chain.get(t))
                    {
                        inst.operands[0] = Operand::IdRef(chain);
                    }
                }
                Op::Store if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    let Some(Operand::IdRef(object)) = inst.operands.get(1) else {
                        continue;
                    };
                    if let Some(&chain) = value_types
                        .get(object)
                        .and_then(|t| direct_root_chain.get(t))
                    {
                        inst.operands[0] = Operand::IdRef(chain);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        for (k, chain) in injected.into_iter().enumerate() {
            first.instructions.insert(k, chain);
        }
    }

    let still_used = ctx.module.functions[entry_idx].blocks.iter().any(|b| {
        b.instructions.iter().any(|i| {
            i.operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(r) if *r == pid))
        })
    });
    if still_used {
        let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, elem_ty);
        let id = ctx.module.fresh_id();
        let ops = vec![Operand::IdRef(var), Operand::IdRef(u0), Operand::IdRef(u0)];
        if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
            first.instructions.insert(
                0,
                Instruction::new(Op::AccessChain, Some(ptr_ty), Some(id), ops),
            );
        }
        replace_id_in_function(&mut ctx.module.functions[entry_idx], pid, id);
    }

    let _ = block_ty;
    nested_air_ordinal_roots
}

fn remap_indexed_air_struct_access(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    elem_ty: Word,
    indices: &[Operand],
    result_ptr_ty: Option<Word>,
) -> Option<(Operand, Vec<Operand>, Word)> {
    let (record, suffix) = indices.split_first()?;
    let Operand::IdRef(record_id) = record else {
        return None;
    };
    if const_u32(types, *record_id).is_some() || suffix.is_empty() {
        return None;
    }
    let (remapped, pointee) =
        remap_direct_air_struct_access(ctx, types, elem_ty, suffix, result_ptr_ty)?;
    Some((record.clone(), remapped, pointee))
}

fn rewrite_flattened_record_array_indices(
    ctx: &mut Ctx,
    entry_idx: usize,
    var: Word,
    block_ty: Word,
    defs: &HashMap<Word, Instruction>,
) {
    let types = combined_type_defs(ctx, defs);
    let value_types = combined_value_types(ctx, entry_idx);
    for bi in 0..ctx.module.functions[entry_idx].blocks.len() {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for mut instruction in old {
            let plan = flattened_record_array_index_plan(
                ctx,
                &types,
                &value_types,
                var,
                block_ty,
                &instruction,
            );
            if let Some((start, consumed, index_ty, terms)) = plan {
                let mut flat = None;
                for (index, coefficient) in terms {
                    let term = if coefficient == 1 {
                        index
                    } else {
                        let coefficient = ctx.const_int_of(index_ty, coefficient as i64);
                        let scaled = ctx.module.fresh_id();
                        rewritten.push(Instruction::new(
                            Op::IMul,
                            Some(index_ty),
                            Some(scaled),
                            vec![Operand::IdRef(index), Operand::IdRef(coefficient)],
                        ));
                        scaled
                    };
                    flat = Some(match flat {
                        None => term,
                        Some(accumulator) => {
                            let sum = ctx.module.fresh_id();
                            rewritten.push(Instruction::new(
                                Op::IAdd,
                                Some(index_ty),
                                Some(sum),
                                vec![Operand::IdRef(accumulator), Operand::IdRef(term)],
                            ));
                            sum
                        }
                    });
                }
                if let Some(flat) = flat {
                    instruction
                        .operands
                        .splice(start..start + consumed, [Operand::IdRef(flat)]);
                }
            }
            rewritten.push(instruction);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
}

fn flattened_record_array_index_plan(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    var: Word,
    block_ty: Word,
    instruction: &Instruction,
) -> Option<(usize, usize, Word, Vec<(Word, u64)>)> {
    if !matches!(
        instruction.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) || instruction.operands.first() != Some(&Operand::IdRef(var))
    {
        return None;
    }
    let result = instruction.result_id?;
    let result_pointee = ptr_pointee(types, instruction.result_type?)?;
    let affine = ctx
        .emit_sidecar
        .buffer_access_affine_offsets
        .iter()
        .find(|fact| fact.id == result)?;
    let mut affine_stride = HashMap::default();
    for &(index, stride) in &affine.terms {
        if affine_stride.insert(index, stride).is_some() {
            return None;
        }
    }

    let indices = &instruction.operands[1..];
    let mut cur = block_ty;
    for (position, operand) in indices.iter().enumerate() {
        let definition = types.get(&cur)?;
        match definition.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(index) = operand else {
                    return None;
                };
                let member = const_u32(types, *index)? as usize;
                let Operand::IdRef(member_ty) = definition.operands.get(member)? else {
                    return None;
                };
                cur = *member_ty;
            }
            Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                let Operand::IdRef(element) = definition.operands.first()? else {
                    return None;
                };
                cur = *element;
            }
            Op::TypeArray => {
                let Operand::IdRef(element) = definition.operands.first()? else {
                    return None;
                };
                let Operand::IdRef(length_id) = definition.operands.get(1)? else {
                    return None;
                };
                let length = u64::from(const_u32(types, *length_id)?);
                let (element_size, element_align) = layout_ty_size_align(ctx, *element, types);
                let element_stride = u64::from(round_up(element_size, element_align));
                if element_stride == 0 {
                    return None;
                }
                for consumed in 2..=indices.len() - position {
                    let reached = type_after_spirv_access_operands(
                        types,
                        *element,
                        &indices[position + consumed..],
                    );
                    if !reached.is_some_and(|reached| {
                        reached == result_pointee
                            || types_structurally_match(ctx, types, reached, result_pointee)
                    }) {
                        continue;
                    }
                    let mut index_ty = None;
                    let mut terms = Vec::with_capacity(consumed);
                    for operand in &indices[position..position + consumed] {
                        let Operand::IdRef(index) = operand else {
                            return None;
                        };
                        if const_u32(types, *index).is_some() {
                            return None;
                        }
                        let ty = *value_types.get(index)?;
                        if index_ty.replace(ty).is_some_and(|old| old != ty) {
                            return None;
                        }
                        let stride = *affine_stride.get(index)?;
                        if !stride.is_multiple_of(element_stride) {
                            return None;
                        }
                        terms.push((*index, stride / element_stride));
                    }
                    let coefficients = terms
                        .iter()
                        .map(|(_, coefficient)| *coefficient)
                        .collect::<Vec<_>>();
                    if coefficients.last() != Some(&1)
                        || coefficients.first().is_none_or(|first| {
                            *first == 0 || *first > length || !length.is_multiple_of(*first)
                        })
                        || coefficients
                            .windows(2)
                            .any(|pair| pair[0] <= pair[1] || !pair[0].is_multiple_of(pair[1]))
                        || coefficients
                            .iter()
                            .any(|coefficient| *coefficient >= length)
                    {
                        return None;
                    }
                    return Some((position + 1, consumed, index_ty?, terms));
                }
                cur = *element;
            }
            _ => return None,
        }
    }
    None
}

pub(in crate::passes) fn remap_nested_air_struct_accesses(
    ctx: &mut Ctx,
    entry_idx: usize,
    seeds: &HashSet<Word>,
    defs: &HashMap<Word, Instruction>,
) {
    let types = combined_type_defs(ctx, defs);
    let mut value_types = combined_value_types(ctx, entry_idx);
    let mut roots = seeds.clone();
    let mut processed = HashSet::default();
    let mut changed = true;

    while changed {
        changed = false;
        let n_blocks = ctx.module.functions[entry_idx].blocks.len();
        for bi in 0..n_blocks {
            let n_inst = ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .len();
            for ii in 0..n_inst {
                let (op, result_id, result_type, base, operands) = {
                    let inst = &ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                    let base = inst.operands.first().and_then(|operand| match operand {
                        Operand::IdRef(base) => Some(*base),
                        _ => None,
                    });
                    (
                        inst.class.opcode,
                        inst.result_id,
                        inst.result_type,
                        base,
                        inst.operands.clone(),
                    )
                };
                let Some(result_id) = result_id.filter(|result| !processed.contains(result)) else {
                    continue;
                };
                let Some(base) = base.filter(|base| roots.contains(base)) else {
                    continue;
                };
                if !matches!(
                    op,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) {
                    continue;
                }
                let Some(base_ptr_ty) = value_types.get(&base).copied() else {
                    continue;
                };
                let Some(base_pointee) = pointer_pointee_including_new(ctx, &types, base_ptr_ty)
                else {
                    continue;
                };
                let Some((remapped, pointee)) =
                    remap_air_struct_access_operands(ctx, &types, base_pointee, op, &operands)
                else {
                    continue;
                };
                let Some(old_ptr_ty) = result_type else {
                    continue;
                };
                let Some(old_pointee) = pointer_pointee_including_new(ctx, &types, old_ptr_ty)
                else {
                    continue;
                };
                if old_pointee != pointee
                    && !types_structurally_match(ctx, &types, old_pointee, pointee)
                    && !types_match_with_elided_air_padding(ctx, &types, pointee, old_pointee)
                {
                    continue;
                }
                let Some(storage) = ptr_storage(&types, old_ptr_ty) else {
                    continue;
                };
                let new_ptr_ty = ctx.ty_ptr(storage, pointee);
                let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                inst.operands = remapped;
                inst.result_type = Some(new_ptr_ty);
                value_types.insert(result_id, new_ptr_ty);
                processed.insert(result_id);
                if roots.insert(result_id) {
                    changed = true;
                }
            }
        }
    }
}

pub(in crate::passes) fn remap_air_struct_access_operands(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    base_pointee: Word,
    op: Op,
    operands: &[Operand],
) -> Option<(Vec<Operand>, Word)> {
    let first_index = if op == Op::PtrAccessChain { 2 } else { 1 };
    let mut remapped = operands.to_vec();
    let mut cur = base_pointee;
    for operand_index in first_index..operands.len() {
        let def = types.get(&cur)?;
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(index) = operands[operand_index] else {
                    return None;
                };
                let member = remap_air_struct_member_index(ctx, types, cur, index)?;
                remapped[operand_index] = Operand::IdRef(ctx.const_uint(member));
                let Operand::IdRef(member_ty) = def.operands.get(member as usize)? else {
                    return None;
                };
                *member_ty
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                let Operand::IdRef(elem_ty) = def.operands.first()? else {
                    return None;
                };
                *elem_ty
            }
            _ => return None,
        };
    }
    Some((remapped, cur))
}

pub(in crate::passes) fn remap_direct_air_struct_access(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
    result_ptr_ty: Option<Word>,
) -> Option<(Vec<Operand>, Word)> {
    let result_pointee = ptr_pointee(types, result_ptr_ty?)?;
    remap_direct_air_struct_access_to_pointee(ctx, types, root_ty, indices, result_pointee)
}

pub(in crate::passes) fn resolve_record_member_fallback(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
    result_ptr_ty: Option<Word>,
    result_id: Option<Word>,
    desired_pointees: &HashMap<Word, Option<Word>>,
) -> Option<(Vec<Operand>, Word)> {
    let mut targets = Vec::new();
    if let Some(rp) = result_ptr_ty.and_then(|t| ptr_pointee(types, t)) {
        targets.push(rp);
    }
    if let Some(dp) = result_id.and_then(|rid| desired_pointees.get(&rid).and_then(|ty| *ty)) {
        if !targets.contains(&dp) {
            targets.push(dp);
        }
    }
    for target in targets {
        if let Some(resolved) = resolve_record_member_path(ctx, types, root_ty, indices, target) {
            return Some((resolved, target));
        }
    }
    None
}

pub(in crate::passes) fn resolve_record_member_path(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    cur_ty: Word,
    indices: &[Operand],
    target: Word,
) -> Option<Vec<Operand>> {
    let Some((idx, rest)) = indices.split_first() else {
        return (cur_ty == target || types_structurally_match(ctx, types, cur_ty, target))
            .then(Vec::new);
    };
    let def = types.get(&cur_ty)?;
    match def.class.opcode {
        Op::TypeStruct => {
            let Operand::IdRef(idx_id) = idx else {
                return None;
            };
            let member_count = def.operands.len() as u32;
            let remapped = remap_air_struct_member_index(ctx, types, cur_ty, *idx_id);
            let raw = const_u32(types, *idx_id).filter(|r| *r < member_count);
            let mut candidates: Vec<u32> = Vec::new();
            if let Some(remapped) = remapped {
                candidates.push(remapped);
            }
            if let Some(raw) = raw {
                if !candidates.contains(&raw) {
                    candidates.push(raw);
                }
            }
            for cand in candidates {
                let Some(Operand::IdRef(member_ty)) = def.operands.get(cand as usize) else {
                    continue;
                };
                if let Some(mut tail) =
                    resolve_record_member_path(ctx, types, *member_ty, rest, target)
                {
                    let mut resolved = vec![Operand::IdRef(ctx.const_uint(cand))];
                    resolved.append(&mut tail);
                    return Some(resolved);
                }
            }
            None
        }
        Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
            let Some(Operand::IdRef(elem_ty)) = def.operands.first() else {
                return None;
            };
            let elem_ty = *elem_ty;
            let mut tail = resolve_record_member_path(ctx, types, elem_ty, rest, target)?;
            let mut resolved = vec![idx.clone()];
            resolved.append(&mut tail);
            Some(resolved)
        }
        _ => None,
    }
}

pub(in crate::passes) fn remap_collapsed_direct_air_struct_access(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
    result_ptr_ty: Option<Word>,
) -> Option<(Vec<Operand>, Word)> {
    let result_pointee = ptr_pointee(types, result_ptr_ty?)?;
    if let Some(remapped) =
        remap_direct_air_struct_access_to_pointee(ctx, types, root_ty, indices, result_pointee)
    {
        return Some(remapped);
    }
    unique_compact_root_member_access(ctx, types, root_ty, indices, result_pointee)
}

pub(in crate::passes) fn air_struct_access_path_at_byte_offset(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    byte_offset: u32,
    target: Word,
) -> Option<Vec<u32>> {
    fn walk(
        ctx: &Ctx,
        types: &HashMap<Word, Instruction>,
        ty: Word,
        byte_offset: u32,
        target: Word,
    ) -> Option<Vec<u32>> {
        if byte_offset == 0 && (ty == target || types_structurally_match(ctx, types, ty, target)) {
            return Some(Vec::new());
        }
        let definition = types.get(&ty)?;
        match definition.class.opcode {
            Op::TypeStruct => {
                for (member, operand) in definition.operands.iter().enumerate() {
                    let Operand::IdRef(member_ty) = operand else {
                        return None;
                    };
                    let member_offset = ctx
                        .air_struct_offsets
                        .get(&ty)
                        .and_then(|offsets| offsets.get(member))
                        .copied()
                        .or_else(|| {
                            crate::layout::spirv_struct_member(
                                ty,
                                member,
                                types,
                                crate::layout::SpirvLayout::natural(ctx.air_data_layout.as_ref()),
                            )
                            .map(|(offset, _)| offset)
                        })?;
                    let (size, align) = layout_ty_size_align(ctx, *member_ty, types);
                    let extent = round_up(size, align);
                    if byte_offset < member_offset
                        || byte_offset >= member_offset.checked_add(extent)?
                    {
                        continue;
                    }
                    let mut suffix =
                        walk(ctx, types, *member_ty, byte_offset - member_offset, target)?;
                    suffix.insert(0, member as u32);
                    return Some(suffix);
                }
                None
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                let Operand::IdRef(element_ty) = definition.operands.first()? else {
                    return None;
                };
                let (size, align) = layout_ty_size_align(ctx, *element_ty, types);
                let stride = match definition.class.opcode {
                    Op::TypeVector => size,
                    _ => round_up(size, align),
                };
                if stride == 0 {
                    return None;
                }
                let element = byte_offset / stride;
                if definition.class.opcode == Op::TypeArray {
                    let Operand::IdRef(length) = definition.operands.get(1)? else {
                        return None;
                    };
                    if element >= const_u32(types, *length)? {
                        return None;
                    }
                } else if matches!(definition.class.opcode, Op::TypeVector | Op::TypeMatrix) {
                    let Operand::LiteralBit32(length) = definition.operands.get(1)? else {
                        return None;
                    };
                    if element >= *length {
                        return None;
                    }
                }
                let mut suffix = walk(ctx, types, *element_ty, byte_offset % stride, target)?;
                suffix.insert(0, element);
                Some(suffix)
            }
            _ => None,
        }
    }

    walk(ctx, types, root_ty, byte_offset, target)
}

pub(in crate::passes) fn unique_compact_root_member_access(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
    result_pointee: Word,
) -> Option<(Vec<Operand>, Word)> {
    let root = types.get(&root_ty)?;
    if root.class.opcode != Op::TypeStruct || indices.is_empty() {
        return None;
    }
    let suffix = &indices[1..];
    let mut candidate = None;
    for (member, operand) in root.operands.iter().enumerate() {
        let Operand::IdRef(member_ty) = operand else {
            return None;
        };
        let reached = if suffix.is_empty() {
            Some(*member_ty)
        } else {
            type_after_spirv_access_operands(types, *member_ty, suffix)
        };
        let Some(reached) = reached else {
            continue;
        };
        if reached != result_pointee
            && !types_structurally_match(ctx, types, reached, result_pointee)
        {
            continue;
        }
        if candidate.is_some() {
            return None;
        }
        candidate = Some((member as u32, reached));
    }
    let (member, reached) = candidate?;
    let mut remapped = Vec::with_capacity(indices.len());
    remapped.push(Operand::IdRef(ctx.const_uint(member)));
    remapped.extend_from_slice(suffix);
    Some((remapped, reached))
}

pub(in crate::passes) fn remap_direct_air_struct_access_to_pointee(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
    result_pointee: Word,
) -> Option<(Vec<Operand>, Word)> {
    let mut cur = root_ty;
    let mut remapped = Vec::with_capacity(indices.len());

    for index in indices {
        let def = types.get(&cur)?;
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(index_id) = index else {
                    return None;
                };
                let member = remap_air_struct_member_index(ctx, types, cur, *index_id)?;
                let Operand::IdRef(member_ty) = def.operands.get(member as usize)? else {
                    return None;
                };
                remapped.push(Operand::IdRef(ctx.const_uint(member)));
                *member_ty
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                let Operand::IdRef(elem_ty) = def.operands.first()? else {
                    return None;
                };
                remapped.push(index.clone());
                *elem_ty
            }
            _ => return None,
        };
    }

    (cur == result_pointee
        || types_structurally_match(ctx, types, cur, result_pointee)
        || types_match_with_elided_air_padding(ctx, types, cur, result_pointee))
    .then_some((remapped, cur))
}

pub(in crate::passes) fn types_match_with_elided_air_padding(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    compact: Word,
    padded: Word,
) -> bool {
    fn inner(
        ctx: &Ctx,
        types: &HashMap<Word, Instruction>,
        compact: Word,
        padded: Word,
        seen: &mut HashSet<(Word, Word)>,
    ) -> bool {
        if types_structurally_match(ctx, types, compact, padded) {
            return true;
        }
        if !seen.insert((compact, padded)) {
            return true;
        }
        let (Some(compact_def), Some(padded_def)) = (types.get(&compact), types.get(&padded))
        else {
            return false;
        };
        if compact_def.class.opcode != Op::TypeStruct
            || padded_def.class.opcode != Op::TypeStruct
            || !ctx.air_struct_offsets.contains_key(&compact)
            || layout_ty_size_align(ctx, compact, types) != layout_ty_size_align(ctx, padded, types)
        {
            return false;
        }

        let mut compact_index = 0usize;
        let mut padded_index = 0usize;
        while compact_index < compact_def.operands.len() && padded_index < padded_def.operands.len()
        {
            let (Operand::IdRef(compact_member), Operand::IdRef(padded_member)) = (
                &compact_def.operands[compact_index],
                &padded_def.operands[padded_index],
            ) else {
                return false;
            };
            if inner(ctx, types, *compact_member, *padded_member, seen) {
                compact_index += 1;
                padded_index += 1;
            } else if is_backend_padding_array(types, *padded_member) {
                padded_index += 1;
            } else {
                return false;
            }
        }
        compact_index == compact_def.operands.len()
            && padded_def.operands[padded_index..].iter().all(|operand| {
                matches!(operand, Operand::IdRef(member) if is_backend_padding_array(types, *member))
            })
    }

    inner(ctx, types, compact, padded, &mut HashSet::default())
}

pub(in crate::passes) fn remap_air_struct_member_index(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    struct_ty: Word,
    index_id: Word,
) -> Option<u32> {
    let index = const_u32(types, index_id)?;
    let def = types.get(&struct_ty)?;
    if def.class.opcode != Op::TypeStruct {
        return None;
    }
    let member_count = def.operands.len();
    let Some(offsets) = ctx.air_struct_offsets.get(&struct_ty) else {
        return (index < member_count as u32).then_some(index);
    };
    if offsets.len() != member_count {
        return (index < member_count as u32).then_some(index);
    }

    let mut padded_index = 0u32;
    let mut cursor = 0u32;
    for (compact_index, operand) in def.operands.iter().enumerate() {
        let Operand::IdRef(member_ty) = operand else {
            return None;
        };
        let offset = offsets[compact_index];
        if offset > cursor {
            if padded_index == index {
                return None;
            }
            padded_index += 1;
        }
        if padded_index == index {
            return Some(compact_index as u32);
        }
        let (size, align) = layout_ty_size_align(ctx, *member_ty, types);
        cursor = offset.saturating_add(round_up(size, align));
        padded_index += 1;
    }
    None
}
