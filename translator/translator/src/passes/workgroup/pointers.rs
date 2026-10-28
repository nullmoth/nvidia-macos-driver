use super::*;
use crate::passes::access::{
    direct_scalar_width, is_unsigned_byte_scalar, single_member_array_scalar_elem,
};
use crate::passes::resources::rewrites::*;
use crate::passes::stage_input::{layout_ty_size_align, round_up};

pub(in crate::passes) fn rewrite_workgroup_root_access(
    ctx: &mut Ctx,
    entry_idx: usize,
    var: Word,
    defs: &HashMap<Word, Instruction>,
) {
    let types = combined_type_defs(ctx, defs);
    let value_types = combined_value_types(ctx, entry_idx);
    let Some(var_ptr_ty) = value_types.get(&var).copied() else {
        return;
    };
    let Some(array_ty) = ptr_pointee(&types, var_ptr_ty) else {
        return;
    };
    let raw_byte_element = types.get(&array_ty).and_then(|definition| {
        if definition.class.opcode != Op::TypeArray {
            return None;
        }
        let Operand::IdRef(element) = definition.operands.first()? else {
            return None;
        };
        is_unsigned_byte_scalar(ctx, *element).then_some(*element)
    });

    let mut want: Vec<Word> = vec![];
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            match inst.class.opcode {
                Op::Load if inst.operands.first() == Some(&Operand::IdRef(var)) => {
                    if let Some(t) = inst.result_type {
                        if !want.contains(&t) {
                            want.push(t);
                        }
                    }
                }
                Op::Store if inst.operands.first() == Some(&Operand::IdRef(var)) => {
                    let Some(Operand::IdRef(object)) = inst.operands.get(1) else {
                        continue;
                    };
                    let Some(t) = value_types.get(object).copied() else {
                        continue;
                    };
                    if !want.contains(&t) {
                        want.push(t);
                    }
                }
                Op::Select | Op::Phi => {
                    let uses_var = if inst.class.opcode == Op::Select {
                        inst.operands
                            .get(1..)
                            .is_some_and(|arms| arms.contains(&Operand::IdRef(var)))
                    } else {
                        inst.operands
                            .iter()
                            .step_by(2)
                            .any(|operand| operand == &Operand::IdRef(var))
                    };
                    if !uses_var {
                        continue;
                    }
                    let Some(target) = inst.result_type.and_then(|ty| ptr_pointee(&types, ty))
                    else {
                        continue;
                    };
                    if !want.contains(&target) {
                        want.push(target);
                    }
                }
                _ => {}
            }
        }
    }
    if want.is_empty() {
        return;
    }

    let u0 = ctx.const_uint(0);
    let mut leaf_chain: HashMap<Word, Word> = HashMap::default();
    let mut injected: Vec<Instruction> = vec![];
    for target in want {
        let (pointee, path) = if let Some(path) = path_to_leaf(&types, array_ty, target) {
            (target, path)
        } else if let Some(byte) = raw_byte_element
            .filter(|_| matches!(direct_scalar_width(ctx, target), Some(16 | 32 | 64)))
        {
            (byte, vec![0])
        } else {
            continue;
        };
        let ptr_ty = ctx.ty_ptr(StorageClass::Workgroup, pointee);
        let id = ctx.module.fresh_id();
        let mut ops = vec![Operand::IdRef(var)];
        for _ in &path {
            ops.push(Operand::IdRef(u0));
        }
        injected.push(Instruction::new(
            Op::InBoundsAccessChain,
            Some(ptr_ty),
            Some(id),
            ops,
        ));
        leaf_chain.insert(target, id);
    }
    if leaf_chain.is_empty() {
        return;
    }

    for blk in &mut ctx.module.functions[entry_idx].blocks {
        for inst in &mut blk.instructions {
            match inst.class.opcode {
                Op::Load if inst.operands.first() == Some(&Operand::IdRef(var)) => {
                    if let Some(&chain) = inst.result_type.as_ref().and_then(|t| leaf_chain.get(t))
                    {
                        inst.operands[0] = Operand::IdRef(chain);
                    }
                }
                Op::Store if inst.operands.first() == Some(&Operand::IdRef(var)) => {
                    let Some(Operand::IdRef(object)) = inst.operands.get(1) else {
                        continue;
                    };
                    if let Some(&chain) = value_types.get(object).and_then(|t| leaf_chain.get(t)) {
                        inst.operands[0] = Operand::IdRef(chain);
                    }
                }
                Op::Select | Op::Phi => {
                    let Some(chain) = inst
                        .result_type
                        .and_then(|ty| ptr_pointee(&types, ty))
                        .and_then(|target| leaf_chain.get(&target))
                        .copied()
                    else {
                        continue;
                    };
                    if inst.class.opcode == Op::Select {
                        for operand in inst.operands.iter_mut().skip(1) {
                            if *operand == Operand::IdRef(var) {
                                *operand = Operand::IdRef(chain);
                            }
                        }
                    } else {
                        for operand in inst.operands.iter_mut().step_by(2) {
                            if *operand == Operand::IdRef(var) {
                                *operand = Operand::IdRef(chain);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        let at = first
            .instructions
            .iter()
            .position(|i| i.class.opcode != Op::Variable)
            .unwrap_or(first.instructions.len());
        for (k, chain) in injected.into_iter().enumerate() {
            first.instructions.insert(at + k, chain);
        }
    }
}

pub(in crate::passes) fn rewrite_collapsed_buffer(
    ctx: &mut Ctx,
    entry_idx: usize,
    pid: Word,
    var: Word,
    block_ty: Word,
    prepend_member0: bool,
    typed_aliases: &[(Word, Word)],
    defs: &HashMap<Word, Instruction>,
) {
    let u0 = ctx.const_uint(0);

    let mut types = defs.clone();
    for g in &ctx.new_globals {
        if let Some(id) = g.result_id {
            types.entry(id).or_insert_with(|| g.clone());
        }
    }
    let value_types = combined_value_types(ctx, entry_idx);
    let desired_pointees = pointer_leaf_use_types(ctx, entry_idx, &value_types);
    let raw_byte_transport = single_member_array_scalar_elem(ctx, block_ty)
        .is_some_and(|element| is_unsigned_byte_scalar(ctx, element));

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let n_inst = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .len();
        for ii in 0..n_inst {
            let (old_indices, result_type, result_id) = {
                let inst = &ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                if matches!(
                    inst.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) && matches!(
                    inst.operands.first(),
                    Some(Operand::IdRef(base)) if *base == pid || *base == var
                ) {
                    (
                        inst.operands[1..].to_vec(),
                        inst.result_type,
                        inst.result_id,
                    )
                } else {
                    (vec![], None, None)
                }
            };
            if old_indices.is_empty() {
                continue;
            }

            let original_pointee = result_type.and_then(|ty| ptr_pointee(&types, ty));

            let flattened_scalar_index = (|| {
                if !prepend_member0 {
                    return None;
                }
                let element = single_member_array_scalar_elem(ctx, block_ty)?;
                let original = original_pointee?;
                if original != element && !types_structurally_match(ctx, &types, original, element)
                {
                    return None;
                }
                let result = result_id?;
                let fact = ctx.emit_sidecar.buffer_access_offsets.iter().find(|fact| {
                    fact.id == result && matches!(fact.root, root if root == pid || root == var)
                })?;
                let (size, align) = layout_ty_size_align(ctx, element, &types);
                let stride = u64::from(round_up(size, align));
                if stride == 0 || !fact.byte_offset.is_multiple_of(stride) {
                    return None;
                }
                let index = u32::try_from(fact.byte_offset / stride).ok()?;
                Some((element, ctx.const_uint(index)))
            })();
            if let Some((element, index)) = flattened_scalar_index {
                let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, element);
                let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                inst.operands = vec![
                    Operand::IdRef(var),
                    Operand::IdRef(u0),
                    Operand::IdRef(index),
                ];
                inst.result_type = Some(ptr_ty);
                continue;
            }

            if let Some(alias) = original_pointee.and_then(|pointee| {
                typed_aliases
                    .iter()
                    .find_map(|(element, var)| (*element == pointee).then_some(*var))
            }) {
                let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                inst.operands[0] = Operand::IdRef(alias);
                if prepend_member0 {
                    inst.operands.insert(1, Operand::IdRef(u0));
                }
                continue;
            }

            if prepend_member0 {
                if let Some(pointee) =
                    type_after_spirv_access_operands(&types, block_ty, &old_indices).filter(
                        |pointee| {
                            raw_byte_transport
                                || original_pointee.is_some_and(|original| {
                                    *pointee == original
                                        || types_structurally_match(ctx, &types, *pointee, original)
                                })
                        },
                    )
                {
                    let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, pointee);
                    let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                    inst.operands[0] = Operand::IdRef(var);
                    inst.result_type = Some(ptr_ty);
                    continue;
                }
            }

            let direct_root = if prepend_member0 {
                runtime_array_block_element_type(&types, block_ty)
            } else {
                Some(block_ty)
            };
            let exact_offset_path = direct_root.and_then(|root_ty| {
                let result = result_id?;
                let pointee = original_pointee?;
                let fact = ctx.emit_sidecar.buffer_access_offsets.iter().find(|fact| {
                    fact.id == result && matches!(fact.root, root if root == pid || root == var)
                })?;
                let byte_offset = u32::try_from(fact.byte_offset).ok()?;
                let path = air_struct_access_path_at_byte_offset(
                    ctx,
                    &types,
                    root_ty,
                    byte_offset,
                    pointee,
                )?;
                Some((
                    path.into_iter()
                        .map(|member| Operand::IdRef(ctx.const_uint(member)))
                        .collect(),
                    pointee,
                ))
            });
            let remapped = exact_offset_path.or_else(|| {
                direct_root.and_then(|root_ty| {
                    remap_collapsed_direct_air_struct_access(
                        ctx,
                        &types,
                        root_ty,
                        &old_indices,
                        result_type,
                    )
                    .or_else(|| {
                        result_id
                            .and_then(|rid| desired_pointees.get(&rid).and_then(|ty| *ty))
                            .and_then(|desired_pointee| {
                                remap_direct_air_struct_access_to_pointee(
                                    ctx,
                                    &types,
                                    root_ty,
                                    &old_indices,
                                    desired_pointee,
                                )
                            })
                    })
                })
            });
            if let Some((indices, pointee)) = remapped {
                let mut operands = vec![Operand::IdRef(var)];
                if prepend_member0 {
                    operands.push(Operand::IdRef(u0));
                    if indices.is_empty()
                        || direct_root.is_some_and(|root| {
                            types
                                .get(&root)
                                .is_some_and(|def| def.class.opcode == Op::TypeStruct)
                        })
                    {
                        operands.push(Operand::IdRef(u0));
                    }
                }
                operands.extend(indices);
                let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, pointee);
                let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                inst.operands = operands;
                inst.result_type = Some(ptr_ty);
                continue;
            }

            let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
            inst.operands[0] = Operand::IdRef(var);
            if prepend_member0 {
                inst.operands.insert(1, Operand::IdRef(u0));
            }
        }
    }

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
    let mut leaf_chain: HashMap<Word, Word> = HashMap::default();
    let mut injected: Vec<Instruction> = vec![];
    for t in want {
        if let Some(alias) = typed_aliases
            .iter()
            .find_map(|(element, var)| (*element == t).then_some(*var))
        {
            let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, t);
            let id = ctx.module.fresh_id();
            injected.push(Instruction::new(
                Op::AccessChain,
                Some(ptr_ty),
                Some(id),
                vec![
                    Operand::IdRef(alias),
                    Operand::IdRef(u0),
                    Operand::IdRef(u0),
                ],
            ));
            leaf_chain.insert(t, id);
            continue;
        }
        let Some(path) = path_to_leaf(&types, block_ty, t) else {
            continue;
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
        leaf_chain.insert(t, id);
    }
    for blk in &mut ctx.module.functions[entry_idx].blocks {
        for inst in &mut blk.instructions {
            match inst.class.opcode {
                Op::Load if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    if let Some(&c) = inst.result_type.as_ref().and_then(|t| leaf_chain.get(t)) {
                        inst.operands[0] = Operand::IdRef(c);
                    }
                }
                Op::Store if inst.operands.first() == Some(&Operand::IdRef(pid)) => {
                    let Some(Operand::IdRef(object)) = inst.operands.get(1) else {
                        continue;
                    };
                    if let Some(&c) = value_types.get(object).and_then(|t| leaf_chain.get(t)) {
                        inst.operands[0] = Operand::IdRef(c);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        for (k, ld) in injected.into_iter().enumerate() {
            first.instructions.insert(k, ld);
        }
    }

    let still_used = ctx.module.functions[entry_idx].blocks.iter().any(|b| {
        b.instructions.iter().any(|i| {
            i.operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(r) if *r == pid))
        })
    });
    let sidecar_used = ctx
        .emit_sidecar
        .local_pointer_field_stores
        .iter()
        .any(|fact| fact.source == pid);
    if still_used || sidecar_used {
        let mut cur = block_ty;
        let mut path = vec![];
        while let Some(next) = types.get(&cur).and_then(member0_type) {
            path.push(0u32);
            cur = next;
        }
        let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, cur);
        let id = ctx.module.fresh_id();
        let mut ops = vec![Operand::IdRef(var)];
        for _ in &path {
            ops.push(Operand::IdRef(u0));
        }
        if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
            let at = first
                .instructions
                .iter()
                .position(|instruction| instruction.class.opcode != Op::Variable)
                .unwrap_or(first.instructions.len());
            first.instructions.insert(
                at,
                Instruction::new(Op::AccessChain, Some(ptr_ty), Some(id), ops),
            );
        }
        if still_used {
            replace_id_in_function(&mut ctx.module.functions[entry_idx], pid, id);
        }
        if sidecar_used {
            ctx.emit_sidecar
                .remap_local_pointer_field_store_sources(&crate::fxhash::map_from([(pid, id)]));
        }
    }
}

pub(in crate::passes) fn rewrite_pointer_storage(
    ctx: &mut Ctx,
    entry_idx: usize,
    root_vars: &[Word],
    target_sc: StorageClass,
    defs: &HashMap<Word, Instruction>,
) -> Result<(), String> {
    let direct_roots: HashSet<Word> = root_vars
        .iter()
        .copied()
        .filter(|root| {
            target_sc == StorageClass::PhysicalStorageBuffer
                || value_result_type(ctx, *root)
                    .and_then(|ty| type_def_of(ctx, ty))
                    .is_none_or(|ty| {
                        ty.operands.first()
                            != Some(&Operand::StorageClass(StorageClass::PhysicalStorageBuffer))
                    })
        })
        .collect();
    let mut roots = direct_roots.clone();
    let mut changed = true;
    let mut new_ptr_types: HashMap<(Word, Word), Word> = HashMap::default();
    let mut value_types = combined_value_types(ctx, entry_idx);
    let desired_pointees = pointer_leaf_use_types(ctx, entry_idx, &value_types);
    let mut value_defs = combined_value_defs(ctx, entry_idx);
    let types = combined_type_defs(ctx, defs);
    let mut scaled_vector_strides = HashSet::default();
    let mut vector_stride_plans = vec![];

    while changed {
        changed = false;
        let n_blocks = ctx.module.functions[entry_idx].blocks.len();
        for bi in 0..n_blocks {
            let n_inst = ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .len();
            for ii in 0..n_inst {
                let (op, rty, rid, base, operands) = {
                    let inst = &ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                    let base = inst.operands.first().and_then(|o| match o {
                        Operand::IdRef(b) => Some(*b),
                        _ => None,
                    });
                    (
                        inst.class.opcode,
                        inst.result_type,
                        inst.result_id,
                        base,
                        inst.operands.clone(),
                    )
                };
                let rooted = if matches!(
                    op,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) {
                    base.map(|id| roots.contains(&id)).unwrap_or(false)
                } else if matches!(op, Op::CopyObject)
                    || (op == Op::Bitcast
                        && rty
                            .and_then(|ty| pointer_pointee_including_new(ctx, &types, ty))
                            .is_some())
                {
                    base.map(|id| roots.contains(&id)).unwrap_or(false)
                } else if matches!(op, Op::Phi) {
                    ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
                        .operands
                        .iter()
                        .step_by(2)
                        .any(|operand| matches!(operand, Operand::IdRef(id) if roots.contains(id)))
                } else if matches!(op, Op::Select) {
                    ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
                        .operands
                        .iter()
                        .skip(1)
                        .any(|operand| matches!(operand, Operand::IdRef(id) if roots.contains(id)))
                } else {
                    false
                };
                let physical_base_boundary = target_sc != StorageClass::PhysicalStorageBuffer
                    && base
                        .and_then(|base| value_types.get(&base))
                        .and_then(|ty| types.get(ty))
                        .and_then(|ty| match ty.operands.first() {
                            Some(Operand::StorageClass(storage)) => Some(*storage),
                            _ => None,
                        })
                        == Some(StorageClass::PhysicalStorageBuffer);
                if !rooted || physical_base_boundary {
                    continue;
                }
                if let Some(rid) = rid {
                    if roots.insert(rid) {
                        changed = true;
                    }
                }
                if let Some(old_ty) = rty {
                    let old_pointee =
                        pointer_pointee_including_new(ctx, defs, old_ty).ok_or_else(|| {
                            format!("buffer access-chain result type {old_ty} not a pointer")
                        })?;
                    let rerooted_flat_struct_scalar = op == Op::PtrAccessChain
                        && operands.len() == 2
                        && base.is_some_and(|base| direct_roots.contains(&base))
                        && base
                            .and_then(|base| value_types.get(&base))
                            .and_then(|ty| pointer_pointee_including_new(ctx, &types, *ty))
                            .and_then(|pointee| types.get(&pointee))
                            .is_some_and(|def| def.class.opcode == Op::TypeStruct)
                        && types.get(&old_pointee).is_some_and(|def| {
                            matches!(def.class.opcode, Op::TypeInt | Op::TypeFloat | Op::TypeBool)
                        });
                    let base_pointee = base
                        .and_then(|base| value_types.get(&base))
                        .and_then(|ty| pointer_pointee_including_new(ctx, &types, *ty));
                    let rooted_vector_stride_pointee = if matches!(
                        op,
                        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                    ) && operands.len() == 2
                    {
                        rid.and_then(|rid| desired_pointees.get(&rid).copied().flatten())
                            .filter(|desired| {
                                types.get(desired).is_some_and(|def| {
                                    def.class.opcode == Op::TypeVector
                                        && def.operands.first()
                                            == Some(&Operand::IdRef(old_pointee))
                                })
                            })
                            .filter(|desired| {
                                base_pointee.is_some_and(|base_pointee| {
                                    base_pointee == *desired
                                        || types_structurally_match(
                                            ctx,
                                            &types,
                                            base_pointee,
                                            *desired,
                                        )
                                })
                            })
                    } else {
                        None
                    };
                    let pointee = if rerooted_flat_struct_scalar {
                        old_pointee
                    } else if let Some(vector) = rooted_vector_stride_pointee {
                        vector
                    } else if let Some(pointee) = rewritten_rooted_pointer_pointee(
                        ctx,
                        &types,
                        &value_types,
                        &roots,
                        op,
                        base,
                        &operands,
                    ) {
                        pointee
                    } else {
                        canonical_rooted_access_pointee(
                            ctx,
                            &types,
                            &value_types,
                            &roots,
                            op,
                            base,
                            old_pointee,
                        )
                    };
                    if target_sc == StorageClass::StorageBuffer {
                        if let (Some(rid), Some((index, index_ty, lanes))) = (
                            rid,
                            rooted_pointee_stride_plan(
                                ctx,
                                &types,
                                &value_types,
                                op,
                                old_ty,
                                pointee,
                                &operands,
                            ),
                        ) {
                            if scaled_vector_strides.insert(rid) {
                                vector_stride_plans.push((rid, index, index_ty, lanes));
                            }
                        }
                    }
                    let new_ty = if let Some(&t) = new_ptr_types.get(&(old_ty, pointee)) {
                        t
                    } else {
                        let t = ctx.ty_ptr(target_sc, pointee);
                        new_ptr_types.insert((old_ty, pointee), t);
                        t
                    };
                    if let Some(rid) = rid {
                        value_types.insert(rid, new_ty);
                    }
                    if matches!(op, Op::Phi | Op::Select) {
                        let replacements = rewritten_pointer_constant_operands(
                            ctx,
                            &mut value_types,
                            &mut value_defs,
                            op,
                            &operands,
                            new_ty,
                        );
                        if !replacements.is_empty() {
                            let inst =
                                &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                            for (operand_idx, replacement) in replacements {
                                inst.operands[operand_idx] = Operand::IdRef(replacement);
                            }
                        }
                    }
                    ctx.module.functions[entry_idx].blocks[bi].instructions[ii].result_type =
                        Some(new_ty);
                    if rooted_vector_stride_pointee.is_some() {
                        ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
                            .class
                            .opcode = Op::PtrAccessChain;
                    } else if rerooted_flat_struct_scalar {
                        ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
                            .class
                            .opcode = Op::InBoundsAccessChain;
                    }
                }
                if target_sc == StorageClass::Workgroup
                    && op == Op::PtrAccessChain
                    && base.is_some_and(|id| direct_roots.contains(&id))
                {
                    let inst = ctx.module.functions[entry_idx].blocks[bi].instructions[ii].clone();
                    ctx.module.functions[entry_idx].blocks[bi].instructions[ii] = Instruction::new(
                        Op::InBoundsAccessChain,
                        inst.result_type,
                        inst.result_id,
                        inst.operands,
                    );
                }
            }
        }
    }

    for (result, index, index_ty, stride) in vector_stride_plans {
        let factor = ctx.const_int_of(index_ty, i64::from(stride));
        let scaled = ctx.module.fresh_id();
        let scale = Instruction::new(
            Op::IMul,
            Some(index_ty),
            Some(scaled),
            vec![Operand::IdRef(index), Operand::IdRef(factor)],
        );
        for block in &mut ctx.module.functions[entry_idx].blocks {
            let Some(pos) = block
                .instructions
                .iter()
                .position(|inst| inst.result_id == Some(result))
            else {
                continue;
            };
            block.instructions[pos].operands[1] = Operand::IdRef(scaled);
            block.instructions.insert(pos, scale);
            break;
        }
    }
    Ok(())
}

pub(in crate::passes) fn rooted_pointee_stride_plan(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    op: Op,
    old_ptr_ty: Word,
    new_pointee: Word,
    operands: &[Operand],
) -> Option<(Word, Word, u32)> {
    if op != Op::PtrAccessChain || operands.len() != 2 {
        return None;
    }
    let old_pointee = pointer_pointee_including_new(ctx, types, old_ptr_ty)?;
    if types.get(&old_pointee)?.class.opcode != Op::TypeVector {
        return None;
    }
    if !matches!(
        types.get(&new_pointee)?.class.opcode,
        Op::TypeInt | Op::TypeFloat
    ) {
        return None;
    }
    let (old_size, old_align) = layout_ty_size_align(ctx, old_pointee, types);
    let old_stride = round_up(old_size, old_align);
    let (new_size, _) = layout_ty_size_align(ctx, new_pointee, types);
    if new_size == 0 || !old_stride.is_multiple_of(new_size) {
        return None;
    }
    let factor = old_stride / new_size;
    if factor <= 1 {
        return None;
    }
    let Operand::IdRef(index) = operands[1] else {
        return None;
    };
    let index_ty = *value_types.get(&index)?;
    (types.get(&index_ty)?.class.opcode == Op::TypeInt).then_some((index, index_ty, factor))
}

pub(in crate::passes) fn rewritten_rooted_pointer_pointee(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    roots: &HashSet<Word>,
    op: Op,
    base: Option<Word>,
    operands: &[Operand],
) -> Option<Word> {
    if matches!(
        op,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        let base_ptr_ty = value_types.get(&base?)?;
        let base_pointee = pointer_pointee_including_new(ctx, types, *base_ptr_ty)?;
        let access_operands = if op == Op::PtrAccessChain {
            operands.get(2..)?
        } else {
            operands.get(1..)?
        };
        return type_after_spirv_access_operands(types, base_pointee, access_operands);
    }
    if op == Op::CopyObject {
        let source_ty = value_types.get(&base?)?;
        return pointer_pointee_including_new(ctx, types, *source_ty);
    }
    if op == Op::Phi {
        return rooted_operand_pointer_pointee(ctx, types, value_types, roots, operands, 0, 2);
    }
    if op == Op::Select {
        return rooted_operand_pointer_pointee(ctx, types, value_types, roots, operands, 1, 1);
    }
    None
}

pub(in crate::passes) fn rooted_operand_pointer_pointee(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    roots: &HashSet<Word>,
    operands: &[Operand],
    skip: usize,
    step: usize,
) -> Option<Word> {
    let mut fallback = None;
    for operand in operands.iter().skip(skip).step_by(step) {
        let Operand::IdRef(id) = operand else {
            continue;
        };
        let Some(ptr_ty) = value_types.get(id) else {
            continue;
        };
        let Some(pointee) = pointer_pointee_including_new(ctx, types, *ptr_ty) else {
            continue;
        };
        if roots.contains(id) {
            return Some(pointee);
        }
        fallback.get_or_insert(pointee);
    }
    fallback
}
