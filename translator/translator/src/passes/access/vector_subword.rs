use super::*;

pub(in crate::passes) fn rewrite_flat_scalar_ptr_access_through_vector_array(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
    #[derive(Clone, Copy)]
    struct Plan {
        bi: usize,
        ii: usize,
        base: Word,
        flat: Word,
        index_ty: Word,
        lanes: u32,
        result_ty: Word,
        result: Word,
    }

    let mut plans = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, instruction) in block.instructions.iter().enumerate() {
            if instruction.class.opcode != Op::PtrAccessChain || instruction.operands.len() != 3 {
                continue;
            }
            let (
                Some(result_ty),
                Some(result),
                Some(Operand::IdRef(base)),
                Some(Operand::IdRef(element)),
                Some(Operand::IdRef(flat)),
            ) = (
                instruction.result_type,
                instruction.result_id,
                instruction.operands.first(),
                instruction.operands.get(1),
                instruction.operands.get(2),
            )
            else {
                continue;
            };
            if const_u32(ctx, *element) != Some(0) {
                continue;
            }
            let Some(result_scalar) = pointer_pointee(ctx, result_ty) else {
                continue;
            };
            let Some(base_ty) = value_result_type(ctx, *base) else {
                continue;
            };
            let Some(base_struct) = pointer_pointee(ctx, base_ty) else {
                continue;
            };
            let Some(struct_def) = type_def_of(ctx, base_struct) else {
                continue;
            };
            let [Operand::IdRef(array_ty)] = struct_def.operands.as_slice() else {
                continue;
            };
            if struct_def.class.opcode != Op::TypeStruct {
                continue;
            }
            let Some(array_def) = type_def_of(ctx, *array_ty) else {
                continue;
            };
            if !matches!(array_def.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
                continue;
            }
            let Some(Operand::IdRef(vector_ty)) = array_def.operands.first() else {
                continue;
            };
            let Some(vector_def) = type_def_of(ctx, *vector_ty) else {
                continue;
            };
            let (Some(Operand::IdRef(component)), Some(Operand::LiteralBit32(lanes))) =
                (vector_def.operands.first(), vector_def.operands.get(1))
            else {
                continue;
            };
            if vector_def.class.opcode != Op::TypeVector
                || *component != result_scalar
                || *lanes == 0
            {
                continue;
            }
            let Some(index_ty) = value_result_type(ctx, *flat) else {
                continue;
            };
            let Some(index_def) = type_def_of(ctx, index_ty) else {
                continue;
            };
            if index_def.class.opcode != Op::TypeInt
                || !matches!(index_def.operands.get(1), Some(Operand::LiteralBit32(0)))
            {
                continue;
            }
            plans.push(Plan {
                bi,
                ii,
                base: *base,
                flat: *flat,
                index_ty,
                lanes: *lanes,
                result_ty,
                result,
            });
        }
    }
    if plans.is_empty() {
        return;
    }
    let by_site = plans
        .into_iter()
        .map(|plan| ((plan.bi, plan.ii), plan))
        .collect::<HashMap<_, _>>();
    let member_zero = ctx.const_uint(0);
    for bi in 0..ctx.module.functions[entry_idx].blocks.len() {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for (ii, instruction) in old.into_iter().enumerate() {
            let Some(plan) = by_site.get(&(bi, ii)).copied() else {
                rewritten.push(instruction);
                continue;
            };
            let (vector_index, lane) = if let Some(flat) = const_u32(ctx, plan.flat) {
                (
                    ctx.const_int_of(plan.index_ty, i64::from(flat / plan.lanes)),
                    ctx.const_int_of(plan.index_ty, i64::from(flat % plan.lanes)),
                )
            } else {
                let lanes = ctx.const_int_of(plan.index_ty, i64::from(plan.lanes));
                let vector_index = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::UDiv,
                    Some(plan.index_ty),
                    Some(vector_index),
                    vec![Operand::IdRef(plan.flat), Operand::IdRef(lanes)],
                ));
                let lane = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::UMod,
                    Some(plan.index_ty),
                    Some(lane),
                    vec![Operand::IdRef(plan.flat), Operand::IdRef(lanes)],
                ));
                (vector_index, lane)
            };
            rewritten.push(Instruction::new(
                Op::InBoundsAccessChain,
                Some(plan.result_ty),
                Some(plan.result),
                vec![
                    Operand::IdRef(plan.base),
                    Operand::IdRef(member_zero),
                    Operand::IdRef(vector_index),
                    Operand::IdRef(lane),
                ],
            ));
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
}

pub(in crate::passes) fn drop_writeonly_dead_local_array_stores(ctx: &mut Ctx, entry_idx: usize) {
    let value_types = function_value_types(ctx, entry_idx);
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut candidates: Vec<Word> = Vec::new();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if inst.class.opcode == Op::Variable {
                if let (Some(id), Some(ty)) = (inst.result_id, inst.result_type) {
                    if matches!(ptr_info.get(&ty), Some((StorageClass::Function, _))) {
                        candidates.push(id);
                    }
                }
            }
        }
    }

    let candidate_set = candidates.iter().copied().collect::<HashSet<_>>();
    let chain_parents = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(
                instruction.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            )
        })
        .filter_map(|instruction| {
            let result = instruction.result_id?;
            let Operand::IdRef(base) = instruction.operands.first()? else {
                return None;
            };
            Some((result, *base))
        })
        .collect::<HashMap<_, _>>();
    let mut pointer_roots = candidates
        .iter()
        .copied()
        .map(|candidate| (candidate, candidate))
        .collect::<HashMap<_, _>>();
    for result in chain_parents.keys().copied() {
        let mut current = result;
        let mut seen = HashSet::default();
        while seen.insert(current) {
            let Some(parent) = chain_parents.get(&current).copied() else {
                break;
            };
            if candidate_set.contains(&parent) {
                pointer_roots.insert(result, parent);
                break;
            }
            current = parent;
        }
    }

    let mut states = candidates
        .iter()
        .copied()
        .map(|candidate| (candidate, (true, false)))
        .collect::<HashMap<_, _>>();
    for instruction in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
    {
        match instruction.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
                if instruction
                    .result_id
                    .is_some_and(|result| pointer_roots.contains_key(&result))
                {
                    continue;
                }
                for root in instruction
                    .operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::IdRef(id) => pointer_roots.get(id).copied(),
                        _ => None,
                    })
                {
                    states.entry(root).and_modify(|state| state.0 = false);
                }
            }
            Op::Store => {
                let pointer = instruction
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(id) => Some(*id),
                        _ => None,
                    });
                let object = instruction
                    .operands
                    .get(1)
                    .and_then(|operand| match operand {
                        Operand::IdRef(id) => Some(*id),
                        _ => None,
                    });
                if let Some(root) = object.and_then(|id| pointer_roots.get(&id)).copied() {
                    states.entry(root).and_modify(|state| state.0 = false);
                }
                if let (Some(slot), Some(object)) = (pointer, object) {
                    if let Some(root) = pointer_roots.get(&slot).copied() {
                        let slot_pointee = value_types
                            .get(&slot)
                            .copied()
                            .and_then(|ty| ptr_info.get(&ty).map(|(_, pointee)| *pointee));
                        let object_type = value_types.get(&object).copied();
                        if matches!((slot_pointee, object_type), (Some(slot), Some(object)) if slot != object)
                        {
                            states.entry(root).and_modify(|state| state.1 = true);
                        }
                    }
                }
            }
            _ => {
                for root in instruction
                    .operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::IdRef(id) => pointer_roots.get(id).copied(),
                        _ => None,
                    })
                {
                    states.entry(root).and_modify(|state| state.0 = false);
                }
            }
        }
    }
    let dead_roots = states
        .into_iter()
        .filter_map(|(root, (write_only, invalid))| (write_only && invalid).then_some(root))
        .collect::<HashSet<_>>();
    if dead_roots.is_empty() {
        return;
    }
    for block in &mut ctx.module.functions[entry_idx].blocks {
        block
            .instructions
            .retain(|instruction| match instruction.class.opcode {
                Op::Store => !instruction
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(id) => pointer_roots.get(id),
                        _ => None,
                    })
                    .is_some_and(|root| dead_roots.contains(root)),
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => !instruction
                    .result_id
                    .and_then(|result| pointer_roots.get(&result))
                    .is_some_and(|root| dead_roots.contains(root)),
                _ => true,
            });
    }
}

pub(in crate::passes) fn lower_cross_member_subword_store(ctx: &mut Ctx, entry_idx: usize) {
    let value_types = function_value_types(ctx, entry_idx);
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }
    let mut member_offset: HashMap<(Word, u32), u32> = HashMap::default();
    for inst in &ctx.module.annotations {
        if inst.class.opcode == Op::MemberDecorate {
            if let (
                Some(Operand::IdRef(sty)),
                Some(Operand::LiteralBit32(m)),
                Some(Operand::Decoration(Decoration::Offset)),
                Some(Operand::LiteralBit32(off)),
            ) = (
                inst.operands.first(),
                inst.operands.get(1),
                inst.operands.get(2),
                inst.operands.get(3),
            ) {
                member_offset.insert((*sty, *m), *off);
            }
        }
    }
    let mut chain_defs: HashMap<Word, Instruction> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                if let Some(rid) = inst.result_id {
                    chain_defs.insert(rid, inst.clone());
                }
            }
        }
    }

    struct StorePlan {
        bi: usize,
        ii: usize,
        obj: Word,
        obj_ty: Word,
        obj_bits: u32,
        base: Word,
        prefix: Vec<Operand>,
        storage: StorageClass,
        members: Vec<(u32, Word, u32)>,
    }
    let mut plans: Vec<StorePlan> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Store {
                continue;
            }
            let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(obj))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let Some(chain) = chain_defs.get(ptr) else {
                continue;
            };
            let Some(ptr_ty) = chain.result_type else {
                continue;
            };
            let Some(&(storage, pointee)) = ptr_info.get(&ptr_ty) else {
                continue;
            };
            let Some(obj_ty) = value_types.get(obj).copied() else {
                continue;
            };
            if obj_ty == pointee {
                continue;
            }
            let Some(obj_bits) = direct_scalar_width(ctx, obj_ty) else {
                continue;
            };
            if obj_bits == 0 || obj_bits % 8 != 0 || obj_bits > 64 {
                continue;
            }
            let Some(Operand::IdRef(base)) = chain.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = chain.operands[1..].to_vec();
            if indices.len() < 2 {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let prefix = &indices[..indices.len() - 1];
            let (struct_ty, prefix_consumed) = walk_into_type_partial(ctx, base_pointee, prefix);
            if prefix_consumed != prefix.len() {
                continue;
            }
            let Some(sdef) = type_def_of(ctx, struct_ty) else {
                continue;
            };
            if sdef.class.opcode != Op::TypeStruct {
                continue;
            }
            let Some(Operand::IdRef(member_id)) = indices.last() else {
                continue;
            };
            let Some(member) = const_u32(ctx, *member_id) else {
                continue;
            };
            let Some(&base_off) = member_offset.get(&(struct_ty, member)) else {
                continue;
            };
            let write_lo = base_off;
            let Some(write_hi) = write_lo.checked_add(obj_bits / 8) else {
                continue;
            };
            let mut members: Vec<(u32, Word, u32)> = Vec::new();
            let mut cursor = write_lo;
            let mut ok = true;
            while cursor < write_hi {
                let mut hit: Option<(u32, Word, u32)> = None;
                for m in 0..sdef.operands.len() as u32 {
                    if member_offset.get(&(struct_ty, m)).copied() != Some(cursor) {
                        continue;
                    }
                    let Some(Operand::IdRef(mty)) = sdef.operands.get(m as usize) else {
                        continue;
                    };
                    let Some(mbits) = direct_scalar_width(ctx, *mty) else {
                        continue;
                    };
                    hit = Some((m, *mty, mbits));
                    break;
                }
                let Some((m, mty, mbits)) = hit else {
                    ok = false;
                    break;
                };
                if cursor + mbits / 8 > write_hi {
                    ok = false;
                    break;
                }
                members.push((m, mty, mbits));
                cursor += mbits / 8;
            }
            if !ok || cursor != write_hi || members.len() < 2 {
                continue;
            }
            plans.push(StorePlan {
                bi,
                ii,
                obj: *obj,
                obj_ty,
                obj_bits,
                base: *base,
                prefix: prefix.to_vec(),
                storage,
                members,
            });
        }
    }
    if plans.is_empty() {
        return;
    }

    let mut by_block: HashMap<usize, Vec<StorePlan>> = HashMap::default();
    for plan in plans {
        by_block.entry(plan.bi).or_default().push(plan);
    }
    for (bi, mut block_plans) in by_block {
        block_plans.sort_by_key(|p| p.ii);
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out: Vec<Instruction> = Vec::with_capacity(insts.len());
        let mut next = 0usize;
        for (ii, inst) in insts.into_iter().enumerate() {
            if next < block_plans.len() && block_plans[next].ii == ii {
                let plan = &block_plans[next];
                next += 1;
                let obj_uint_ty = uint_type_of_width(ctx, plan.obj_bits);
                let obj_uint = if plan.obj_ty == obj_uint_ty {
                    plan.obj
                } else {
                    let id = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(obj_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(plan.obj)],
                    ));
                    id
                };
                let mut shift = 0u32;
                for &(m, mty, mbits) in &plan.members {
                    let member_uint_ty = uint_type_of_width(ctx, mbits);
                    let shifted = if shift == 0 {
                        obj_uint
                    } else {
                        let amt = ctx.const_uint(shift);
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::ShiftRightLogical,
                            Some(obj_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(obj_uint), Operand::IdRef(amt)],
                        ));
                        id
                    };
                    let narrowed = if plan.obj_bits == mbits {
                        shifted
                    } else {
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::UConvert,
                            Some(member_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(shifted)],
                        ));
                        id
                    };
                    let val = if mty == member_uint_ty {
                        narrowed
                    } else {
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::Bitcast,
                            Some(mty),
                            Some(id),
                            vec![Operand::IdRef(narrowed)],
                        ));
                        id
                    };
                    let member_idx_id = ctx.const_uint(m);
                    let ptr_ty = ctx.ty_ptr(plan.storage, mty);
                    let mptr = ctx.module.fresh_id();
                    let mut ops = Vec::with_capacity(plan.prefix.len() + 2);
                    ops.push(Operand::IdRef(plan.base));
                    ops.extend(plan.prefix.iter().cloned());
                    ops.push(Operand::IdRef(member_idx_id));
                    out.push(Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(ptr_ty),
                        Some(mptr),
                        ops,
                    ));
                    out.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(mptr), Operand::IdRef(val)],
                    ));
                    shift += mbits;
                }
                continue;
            }
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
}

pub(in crate::passes) fn lower_subword_scalar_store(ctx: &mut Ctx, entry_idx: usize) {
    let value_types = function_value_types(ctx, entry_idx);
    let value_defs = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| Some((instruction.result_id?, instruction.clone())))
        .collect::<HashMap<_, _>>();
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    struct StorePlan {
        bi: usize,
        ii: usize,
        ptr: Word,
        storage: StorageClass,
        pointee_ty: Word,
        pointee_bits: u32,
        obj: Word,
        obj_ty: Word,
        obj_bits: u32,
    }
    let mut plans: Vec<StorePlan> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Store {
                continue;
            }
            let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(obj))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let Some(ptr_ty) = value_types.get(ptr).copied() else {
                continue;
            };
            let Some(&(storage, pointee_ty)) = ptr_info.get(&ptr_ty) else {
                continue;
            };
            if !ptr_access_chain_allowed_storage(storage) {
                continue;
            }
            let ptr_is_element = value_defs
                .get(ptr)
                .map(|definition| store_split_pointer_is_element(ctx, definition, pointee_ty))
                .unwrap_or(false);
            if !ptr_is_element {
                continue;
            }
            let Some(obj_ty) = value_types.get(obj).copied() else {
                continue;
            };
            if obj_ty == pointee_ty {
                continue;
            }
            let Some(pointee_bits) = direct_scalar_width(ctx, pointee_ty) else {
                continue;
            };
            let Some(obj_bits) = direct_scalar_or_vector_width(ctx, obj_ty) else {
                continue;
            };
            if pointee_bits == 0
                || obj_bits <= pointee_bits
                || obj_bits % pointee_bits != 0
                || obj_bits > 64
            {
                continue;
            }
            plans.push(StorePlan {
                bi,
                ii,
                ptr: *ptr,
                storage,
                pointee_ty,
                pointee_bits,
                obj: *obj,
                obj_ty,
                obj_bits,
            });
        }
    }
    if plans.is_empty() {
        return;
    }

    let mut by_block: HashMap<usize, Vec<StorePlan>> = HashMap::default();
    for plan in plans {
        by_block.entry(plan.bi).or_default().push(plan);
    }
    for (bi, mut block_plans) in by_block {
        block_plans.sort_by_key(|p| p.ii);
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out: Vec<Instruction> = Vec::with_capacity(insts.len());
        let mut next = 0usize;
        for (ii, inst) in insts.into_iter().enumerate() {
            if next < block_plans.len() && block_plans[next].ii == ii {
                let plan = &block_plans[next];
                next += 1;
                let slots = plan.obj_bits / plan.pointee_bits;
                let obj_uint_ty = uint_type_of_width(ctx, plan.obj_bits);
                let pointee_uint_ty = uint_type_of_width(ctx, plan.pointee_bits);
                let ptr_ty = ctx.ty_ptr(plan.storage, plan.pointee_ty);
                let obj_uint = if plan.obj_ty == obj_uint_ty {
                    plan.obj
                } else {
                    let id = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(obj_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(plan.obj)],
                    ));
                    id
                };
                for k in 0..slots {
                    let shifted = if k == 0 {
                        obj_uint
                    } else {
                        let amt = ctx.const_uint(k * plan.pointee_bits);
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::ShiftRightLogical,
                            Some(obj_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(obj_uint), Operand::IdRef(amt)],
                        ));
                        id
                    };
                    let narrowed = if plan.obj_bits == plan.pointee_bits {
                        shifted
                    } else {
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::UConvert,
                            Some(pointee_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(shifted)],
                        ));
                        id
                    };
                    let val = if plan.pointee_ty == pointee_uint_ty {
                        narrowed
                    } else {
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::Bitcast,
                            Some(plan.pointee_ty),
                            Some(id),
                            vec![Operand::IdRef(narrowed)],
                        ));
                        id
                    };
                    let slot_ptr = if k == 0 {
                        plan.ptr
                    } else {
                        let kid = ctx.const_uint(k);
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::PtrAccessChain,
                            Some(ptr_ty),
                            Some(id),
                            vec![Operand::IdRef(plan.ptr), Operand::IdRef(kid)],
                        ));
                        id
                    };
                    out.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(slot_ptr), Operand::IdRef(val)],
                    ));
                }
                continue;
            }
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
}

fn store_split_pointer_is_element(ctx: &Ctx, inst: &Instruction, pointee_ty: Word) -> bool {
    match inst.class.opcode {
        Op::PtrAccessChain => true,
        Op::AccessChain | Op::InBoundsAccessChain => {
            access_chain_final_index_enters_array_element(ctx, inst, pointee_ty)
        }
        _ => false,
    }
}

fn access_chain_final_index_enters_array_element(
    ctx: &Ctx,
    inst: &Instruction,
    pointee_ty: Word,
) -> bool {
    let Some(Operand::IdRef(base)) = inst.operands.first() else {
        return false;
    };
    let Some(base_ty) = value_result_type(ctx, *base) else {
        return false;
    };
    let Some(mut cur) = pointer_pointee(ctx, base_ty) else {
        return false;
    };
    let indices = &inst.operands[1..];
    let Some((last, prefix)) = indices.split_last() else {
        return false;
    };
    for op in prefix {
        let Some(def) = type_def_of(ctx, cur) else {
            return false;
        };
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(idx_id) = op else {
                    return false;
                };
                let Some(member) = const_u32(ctx, *idx_id) else {
                    return false;
                };
                match def.operands.get(member as usize) {
                    Some(Operand::IdRef(member_ty)) => *member_ty,
                    _ => return false,
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                match def.operands.first() {
                    Some(Operand::IdRef(elem_ty)) => *elem_ty,
                    _ => return false,
                }
            }
            _ => return false,
        };
    }
    if !matches!(last, Operand::IdRef(_)) {
        return false;
    }
    let Some(def) = type_def_of(ctx, cur) else {
        return false;
    };
    if !matches!(def.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
        return false;
    }
    match def.operands.first() {
        Some(Operand::IdRef(elem_ty)) => *elem_ty == pointee_ty,
        _ => false,
    }
}

pub(in crate::passes) fn direct_scalar_or_vector_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    if let Some(bits) = direct_scalar_width(ctx, ty) {
        return Some(bits);
    }
    let vector = type_def_of(ctx, ty)?;
    if vector.class.opcode != Op::TypeVector {
        return None;
    }
    let (Some(Operand::IdRef(element)), Some(Operand::LiteralBit32(lanes))) =
        (vector.operands.first(), vector.operands.get(1))
    else {
        return None;
    };
    direct_scalar_width(ctx, *element)?.checked_mul(*lanes)
}

pub(in crate::passes) fn uint_type_of_width(ctx: &mut Ctx, bits: u32) -> Word {
    for inst in ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
    {
        if inst.class.opcode == Op::TypeInt
            && inst.operands.first() == Some(&Operand::LiteralBit32(bits))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(0))
        {
            if let Some(id) = inst.result_id {
                return id;
            }
        }
    }
    let id = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::TypeInt,
        None,
        Some(id),
        vec![Operand::LiteralBit32(bits), Operand::LiteralBit32(0)],
    ));
    id
}

pub(in crate::passes) struct SubwordPart {
    pub(in crate::passes) member_index: u32,
    pub(in crate::passes) member_offset: u32,
    pub(in crate::passes) member_ty: Word,
    pub(in crate::passes) member_bits: u32,
    pub(in crate::passes) shift_right: u32,
    pub(in crate::passes) keep_bits: u32,
    pub(in crate::passes) shift_left: u32,
}

pub(in crate::passes) struct SubwordPlan {
    pub(in crate::passes) bi: usize,
    pub(in crate::passes) ii: usize,
    pub(in crate::passes) result_id: Word,
    pub(in crate::passes) result_ty: Word,
    pub(in crate::passes) result_bits: u32,
    pub(in crate::passes) base: Word,
    pub(in crate::passes) prefix: Vec<Operand>,
    pub(in crate::passes) storage: StorageClass,
    pub(in crate::passes) parts: Vec<SubwordPart>,
    pub(in crate::passes) exact_buffer_offset: Option<(Word, u64)>,
    pub(in crate::passes) addressed_member_offset: u32,
    pub(in crate::passes) source_pointer: Word,
}

pub(in crate::passes) fn lower_cross_member_subword_load(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }
    let mut member_offset: HashMap<(Word, u32), u32> = HashMap::default();
    for inst in &ctx.module.annotations {
        if inst.class.opcode == Op::MemberDecorate {
            if let (
                Some(Operand::IdRef(sty)),
                Some(Operand::LiteralBit32(m)),
                Some(Operand::Decoration(Decoration::Offset)),
                Some(Operand::LiteralBit32(off)),
            ) = (
                inst.operands.first(),
                inst.operands.get(1),
                inst.operands.get(2),
                inst.operands.get(3),
            ) {
                member_offset.insert((*sty, *m), *off);
            }
        }
    }
    let mut chain_defs: HashMap<Word, Instruction> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                if let Some(rid) = inst.result_id {
                    chain_defs.insert(rid, inst.clone());
                }
            }
        }
    }
    let types = crate::passes::resources::rewrites::combined_type_defs(ctx, &HashMap::default());
    let value_types = crate::passes::resources::rewrites::combined_value_types(ctx, entry_idx);
    let exact_offsets = ctx
        .emit_sidecar
        .buffer_access_offsets
        .iter()
        .map(|fact| (fact.id, (fact.root, fact.byte_offset)))
        .collect::<HashMap<_, _>>();

    let mut plans: Vec<SubwordPlan> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Load {
                continue;
            }
            let (Some(result_ty), Some(result_id)) = (inst.result_type, inst.result_id) else {
                continue;
            };
            let Some(Operand::IdRef(ptr)) = inst.operands.first() else {
                continue;
            };
            let Some(chain) = chain_defs.get(ptr) else {
                continue;
            };
            let Some(ptr_ty) = chain.result_type else {
                continue;
            };
            let Some(&(storage, pointee)) = ptr_info.get(&ptr_ty) else {
                continue;
            };
            if pointee == result_ty {
                continue;
            }
            let Some(result_bits) = direct_scalar_width(ctx, result_ty) else {
                continue;
            };
            if result_bits == 0 || result_bits % 8 != 0 || result_bits > 32 {
                continue;
            }
            let Some(Operand::IdRef(base)) = chain.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = chain.operands[1..].to_vec();
            if indices.len() < 2 {
                continue;
            }
            let Some(base_ptr_ty) = value_result_type(ctx, *base) else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let prefix = &indices[..indices.len() - 1];
            let (struct_ty, prefix_consumed) = walk_into_type_partial(ctx, base_pointee, prefix);
            if prefix_consumed != prefix.len() {
                continue;
            }
            let Some(sdef) = type_def_of(ctx, struct_ty) else {
                continue;
            };
            if sdef.class.opcode != Op::TypeStruct {
                continue;
            }
            let Some(Operand::IdRef(member_id)) = indices.last() else {
                continue;
            };
            let Some(member) = const_u32(ctx, *member_id) else {
                continue;
            };
            let Some(&base_off) = member_offset.get(&(struct_ty, member)) else {
                continue;
            };
            let read_lo = base_off;
            let Some(read_hi) = read_lo.checked_add(result_bits / 8) else {
                continue;
            };
            let mut covered: Vec<(u32, Word, u32, u32)> = Vec::new();
            for m in 0..sdef.operands.len() as u32 {
                let Some(&off) = member_offset.get(&(struct_ty, m)) else {
                    continue;
                };
                let Some(Operand::IdRef(mty)) = sdef.operands.get(m as usize) else {
                    continue;
                };
                let Some(mbits) = direct_scalar_width(ctx, *mty) else {
                    continue;
                };
                let msize = mbits / 8;
                if msize == 0 {
                    continue;
                }
                if off < read_hi && off + msize > read_lo {
                    covered.push((m, *mty, off, msize));
                }
            }
            if covered.len() < 2 {
                continue;
            }
            covered.sort_by_key(|c| c.2);
            let mut cursor = covered[0].2;
            if cursor > read_lo {
                continue;
            }
            let mut tiled = true;
            for &(_, _, off, msize) in &covered {
                if off != cursor {
                    tiled = false;
                    break;
                }
                cursor = off + msize;
            }
            if !tiled || cursor < read_hi {
                continue;
            }
            let mut parts: Vec<SubwordPart> = Vec::new();
            for &(m, mty, off, msize) in &covered {
                let lo = read_lo.max(off);
                let hi = read_hi.min(off + msize);
                parts.push(SubwordPart {
                    member_index: m,
                    member_offset: off,
                    member_ty: mty,
                    member_bits: msize * 8,
                    shift_right: (lo - off) * 8,
                    keep_bits: (hi - lo) * 8,
                    shift_left: (lo - read_lo) * 8,
                });
            }
            plans.push(SubwordPlan {
                bi,
                ii,
                result_id,
                result_ty,
                result_bits,
                base: *base,
                prefix: prefix.to_vec(),
                storage,
                parts,
                exact_buffer_offset: inherited_exact_byte_offset(
                    ctx,
                    *ptr,
                    &exact_offsets,
                    &chain_defs,
                    &types,
                    &value_types,
                    &mut HashSet::default(),
                ),
                addressed_member_offset: read_lo,
                source_pointer: *ptr,
            });
        }
    }

    if plans.is_empty() {
        return Ok(());
    }

    let retired_source_pointers = plans
        .iter()
        .map(|plan| plan.source_pointer)
        .collect::<Vec<_>>();
    let mut by_block: HashMap<usize, Vec<SubwordPlan>> = HashMap::default();
    for plan in plans {
        by_block.entry(plan.bi).or_default().push(plan);
    }
    for (bi, mut block_plans) in by_block {
        block_plans.sort_by_key(|p| p.ii);
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out: Vec<Instruction> = Vec::with_capacity(insts.len());
        let mut next = 0usize;
        for (ii, inst) in insts.into_iter().enumerate() {
            if next < block_plans.len() && block_plans[next].ii == ii {
                let plan = &block_plans[next];
                next += 1;
                emit_subword_assembly(ctx, plan, &mut out)?;
                continue;
            }
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
    crate::passes::resources::retire_dead_pointer_projections(
        ctx,
        entry_idx,
        retired_source_pointers,
    );
    Ok(())
}

pub(in crate::passes) fn emit_subword_assembly(
    ctx: &mut Ctx,
    plan: &SubwordPlan,
    out: &mut Vec<Instruction>,
) -> Result<(), String> {
    let result_uint = uint_type_of_width(ctx, plan.result_bits);
    let mut acc: Option<Word> = None;
    for part in &plan.parts {
        let member_idx_id = ctx.const_uint(part.member_index);
        let ptr_ty = ctx.ty_ptr(plan.storage, part.member_ty);
        let ptr = ctx.module.fresh_id();
        let mut ops = Vec::with_capacity(plan.prefix.len() + 2);
        ops.push(Operand::IdRef(plan.base));
        ops.extend(plan.prefix.iter().cloned());
        ops.push(Operand::IdRef(member_idx_id));
        out.push(Instruction::new(
            Op::InBoundsAccessChain,
            Some(ptr_ty),
            Some(ptr),
            ops,
        ));
        if let Some((root, addressed_offset)) = plan.exact_buffer_offset {
            let relative = i64::from(part.member_offset) - i64::from(plan.addressed_member_offset);
            if let Some(byte_offset) = addressed_offset.checked_add_signed(relative) {
                ctx.emit_sidecar.buffer_access_offsets.push(
                    crate::emit_sidecar::BufferAccessOffset {
                        id: ptr,
                        root,
                        byte_offset,
                    },
                );
            }
        }
        let loaded = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(part.member_ty),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));
        let member_uint = uint_type_of_width(ctx, part.member_bits);
        let mut cur = if part.member_ty == member_uint {
            loaded
        } else {
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Bitcast,
                Some(member_uint),
                Some(id),
                vec![Operand::IdRef(loaded)],
            ));
            id
        };
        if part.shift_right > 0 {
            let amt = ctx.const_uint(part.shift_right);
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(member_uint),
                Some(id),
                vec![Operand::IdRef(cur), Operand::IdRef(amt)],
            ));
            cur = id;
        }
        if part.member_bits != plan.result_bits {
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::UConvert,
                Some(result_uint),
                Some(id),
                vec![Operand::IdRef(cur)],
            ));
            cur = id;
        }
        if part.keep_bits < plan.result_bits {
            let mask = if part.keep_bits >= 32 {
                u32::MAX
            } else {
                (1u32 << part.keep_bits) - 1
            };
            let mask_id = ctx.const_int_of(result_uint, mask as i64);
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::BitwiseAnd,
                Some(result_uint),
                Some(id),
                vec![Operand::IdRef(cur), Operand::IdRef(mask_id)],
            ));
            cur = id;
        }
        if part.shift_left > 0 {
            let amt = ctx.const_uint(part.shift_left);
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftLeftLogical,
                Some(result_uint),
                Some(id),
                vec![Operand::IdRef(cur), Operand::IdRef(amt)],
            ));
            cur = id;
        }
        acc = Some(match acc {
            None => cur,
            Some(prev) => {
                let id = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::BitwiseOr,
                    Some(result_uint),
                    Some(id),
                    vec![Operand::IdRef(prev), Operand::IdRef(cur)],
                ));
                id
            }
        });
    }
    let packed = acc.ok_or("covered.len() >= 2")?;
    let op = if plan.result_ty == result_uint {
        Op::CopyObject
    } else {
        Op::Bitcast
    };
    out.push(Instruction::new(
        op,
        Some(plan.result_ty),
        Some(plan.result_id),
        vec![Operand::IdRef(packed)],
    ));
    Ok(())
}

pub(in crate::passes) fn find_def_in_func(func: &Function, id: Word) -> Option<Instruction> {
    for b in &func.blocks {
        for i in &b.instructions {
            if i.result_id == Some(id) {
                return Some(i.clone());
            }
        }
    }
    None
}

pub(in crate::passes) fn operand_id(inst: &Instruction, n: usize) -> Option<Word> {
    match inst.operands.get(n) {
        Some(Operand::IdRef(x)) => Some(*x),
        _ => None,
    }
}

pub(in crate::passes) fn imul_by(ctx: &Ctx, inst: &Instruction, w: u32) -> bool {
    inst.class.opcode == Op::IMul
        && (0..2).any(|n| operand_id(inst, n).and_then(|x| const_u32(ctx, x)) == Some(w))
}

pub(in crate::passes) fn index_is_word_addressed(
    ctx: &Ctx,
    func: &Function,
    idx: Word,
    w: u32,
) -> bool {
    if const_u32(ctx, idx).is_some() {
        return true;
    }
    let Some(def) = find_def_in_func(func, idx) else {
        return false;
    };
    match def.class.opcode {
        Op::IMul => imul_by(ctx, &def, w),
        Op::IAdd => {
            let a = operand_id(&def, 0);
            let b = operand_id(&def, 1);
            let a_const = a.and_then(|x| const_u32(ctx, x)).is_some();
            let b_const = b.and_then(|x| const_u32(ctx, x)).is_some();
            let a_mul = a
                .and_then(|x| find_def_in_func(func, x))
                .map(|d| imul_by(ctx, &d, w))
                .unwrap_or(false);
            let b_mul = b
                .and_then(|x| find_def_in_func(func, x))
                .map(|d| imul_by(ctx, &d, w))
                .unwrap_or(false);
            (a_const && b_mul) || (b_const && a_mul)
        }
        _ => false,
    }
}
