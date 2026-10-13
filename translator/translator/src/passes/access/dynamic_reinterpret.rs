use super::*;

pub(in crate::passes) fn rewrite_dynamic_struct_index_reinterpret(
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

    let struct_single_array_elem = |ctx: &Ctx, struct_ty: Word| -> Option<Word> {
        let def = type_def_of(ctx, struct_ty)?;
        if def.class.opcode != Op::TypeStruct || def.operands.len() != 1 {
            return None;
        }
        let Operand::IdRef(member0) = def.operands.first()? else {
            return None;
        };
        let mdef = type_def_of(ctx, *member0)?;
        if !matches!(mdef.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
            return None;
        }
        let Operand::IdRef(elem) = mdef.operands.first()? else {
            return None;
        };
        direct_scalar_width(ctx, *elem).map(|_| *elem)
    };

    struct Plan {
        bi: usize,
        ii: usize,
        ac_id: Word,
        base: Word,
        dyn_idx: Word,
        elem_ty: Word,
        result_pointee: Word,
        sc: StorageClass,
        reinterpret: bool,
    }
    let mut plans: Vec<Plan> = Vec::new();
    let blocks = &ctx.module.functions[entry_idx].blocks;
    for (bi, block) in blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            if inst.operands.len() != 2 {
                continue;
            }
            let (Some(ac_id), Some(result_type)) = (inst.result_id, inst.result_type) else {
                continue;
            };
            let Some(&(sc, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let Some(Operand::IdRef(dyn_idx)) = inst.operands.get(1) else {
                continue;
            };
            if const_u32(ctx, *dyn_idx).is_some() {
                continue;
            }
            let Some(base_ptr_ty) = value_result_type(ctx, *base) else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some(elem_ty) = struct_single_array_elem(ctx, base_pointee) else {
                continue;
            };
            let (Some(ew), Some(pw)) = (
                direct_scalar_width(ctx, elem_ty),
                direct_scalar_width(ctx, result_pointee),
            ) else {
                continue;
            };
            if ew != pw {
                continue;
            }
            plans.push(Plan {
                bi,
                ii,
                ac_id,
                base: *base,
                dyn_idx: *dyn_idx,
                elem_ty,
                result_pointee,
                sc,
                reinterpret: result_pointee != elem_ty,
            });
        }
    }
    if plans.is_empty() {
        return Ok(());
    }

    let mut use_sites: HashMap<Word, Vec<(usize, usize, bool)>> = HashMap::default();
    let mut disqualified: HashSet<Word> = HashSet::default();
    let plan_ids: HashSet<Word> = plans.iter().map(|p| p.ac_id).collect();
    let result_pointee_of: HashMap<Word, Word> =
        plans.iter().map(|p| (p.ac_id, p.result_pointee)).collect();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst
                .result_id
                .map(|r| plan_ids.contains(&r))
                .unwrap_or(false)
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            if inst.result_type == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, true));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                Op::Store => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            let obj = match inst.operands.get(1) {
                                Some(Operand::IdRef(o)) => value_result_type(ctx, *o),
                                _ => None,
                            };
                            if obj == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, false));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                _ => {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            if plan_ids.contains(id) {
                                disqualified.insert(*id);
                            }
                        }
                    }
                }
            }
        }
    }
    plans.retain(|p| !disqualified.contains(&p.ac_id) && use_sites.contains_key(&p.ac_id));
    if plans.is_empty() {
        return Ok(());
    }

    let member0 = ctx.const_uint(0);
    let mut new_ptr_ty: HashMap<(StorageClass, Word), Word> = HashMap::default();
    for p in &plans {
        new_ptr_ty
            .entry((p.sc, p.elem_ty))
            .or_insert_with(|| ctx.ty_ptr(p.sc, p.elem_ty));
    }

    let load_split_id: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|p| use_sites.get(&p.ac_id).into_iter().flatten())
        .filter(|(_, _, is_load)| *is_load)
        .map(|&(bi, ii, _)| ((bi, ii), 0))
        .collect::<HashMap<_, _>>()
        .into_keys()
        .map(|k| (k, ctx.module.fresh_id()))
        .collect();
    let store_cast_id: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|p| use_sites.get(&p.ac_id).into_iter().flatten())
        .filter(|(_, _, is_load)| !*is_load)
        .map(|&(bi, ii, _)| ((bi, ii), 0))
        .collect::<HashMap<_, _>>()
        .into_keys()
        .map(|k| (k, ctx.module.fresh_id()))
        .collect();

    let ac_at: HashMap<(usize, usize), &Plan> = plans.iter().map(|p| ((p.bi, p.ii), p)).collect();
    let elem_ty_of_load: HashMap<(usize, usize), (Word, Word)> = plans
        .iter()
        .flat_map(|p| {
            use_sites
                .get(&p.ac_id)
                .into_iter()
                .flatten()
                .map(move |&(bi, ii, is_load)| ((bi, ii), (p.elem_ty, p.result_pointee, is_load)))
        })
        .map(|((bi, ii), (e, rp, _))| ((bi, ii), (e, rp)))
        .collect();

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut newv: Vec<Instruction> = Vec::with_capacity(old.len() + 4);
        for (ii, inst) in old.into_iter().enumerate() {
            if let Some(p) = ac_at.get(&(bi, ii)) {
                let new_ty = new_ptr_ty[&(p.sc, p.elem_ty)];
                newv.push(Instruction::new(
                    inst.class.opcode,
                    Some(new_ty),
                    Some(p.ac_id),
                    vec![
                        Operand::IdRef(p.base),
                        Operand::IdRef(member0),
                        Operand::IdRef(p.dyn_idx),
                    ],
                ));
                let _ = p.reinterpret;
                continue;
            }
            if let Some(&load_id) = load_split_id.get(&(bi, ii)) {
                let (elem_ty, result_pointee) = elem_ty_of_load[&(bi, ii)];
                let ptr = match inst.operands.first() {
                    Some(Operand::IdRef(p)) => *p,
                    _ => return Err("load split site lost its pointer operand".to_string()),
                };
                let res = inst.result_id.ok_or("load has a result id")?;
                let mut load_ops = vec![Operand::IdRef(ptr)];
                load_ops.extend(inst.operands.iter().skip(1).cloned());
                newv.push(Instruction::new(
                    Op::Load,
                    Some(elem_ty),
                    Some(load_id),
                    load_ops,
                ));
                if elem_ty == result_pointee {
                    newv.push(Instruction::new(
                        Op::CopyObject,
                        Some(result_pointee),
                        Some(res),
                        vec![Operand::IdRef(load_id)],
                    ));
                } else {
                    newv.push(Instruction::new(
                        Op::Bitcast,
                        Some(result_pointee),
                        Some(res),
                        vec![Operand::IdRef(load_id)],
                    ));
                }
                continue;
            }
            if let Some(&cast_id) = store_cast_id.get(&(bi, ii)) {
                let (elem_ty, _result_pointee) = elem_ty_of_load[&(bi, ii)];
                let ptr = match inst.operands.first() {
                    Some(Operand::IdRef(p)) => *p,
                    _ => return Err("store cast site lost its pointer operand".to_string()),
                };
                let obj = match inst.operands.get(1) {
                    Some(Operand::IdRef(o)) => *o,
                    _ => return Err("store cast site lost its object operand".to_string()),
                };
                newv.push(Instruction::new(
                    Op::Bitcast,
                    Some(elem_ty),
                    Some(cast_id),
                    vec![Operand::IdRef(obj)],
                ));
                let mut store_ops = vec![Operand::IdRef(ptr), Operand::IdRef(cast_id)];
                store_ops.extend(inst.operands.iter().skip(2).cloned());
                newv.push(Instruction::new(Op::Store, None, None, store_ops));
                continue;
            }
            newv.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = newv;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct DynamicBlockView {
    bi: usize,
    ii: usize,
    opcode: Op,
    ac_id: Word,
    base: Word,
    dyn_idx: Word,
    index_ty: Word,
    sc: StorageClass,
    result_ptr_ty: Word,
    result_pointee: Word,
    elem_ty: Word,
}

fn pointer_storage_and_pointee(ctx: &Ctx) -> HashMap<Word, (StorageClass, Word)> {
    let mut info = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(sc)), Some(Operand::IdRef(pointee))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                info.insert(id, (*sc, *pointee));
            }
        }
    }
    info
}

fn dynamic_block_views(
    ctx: &Ctx,
    entry_idx: usize,
    admits: impl Fn(&Ctx, &DynamicBlockView) -> bool,
) -> Vec<DynamicBlockView> {
    let ptr_info = pointer_storage_and_pointee(ctx);
    let mut views = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                || inst.operands.len() != 2
            {
                continue;
            }
            let (Some(ac_id), Some(result_ptr_ty)) = (inst.result_id, inst.result_type) else {
                continue;
            };
            let Some(&(sc, result_pointee)) = ptr_info.get(&result_ptr_ty) else {
                continue;
            };
            let (Some(Operand::IdRef(base)), Some(Operand::IdRef(dyn_idx))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            if const_u32(ctx, *dyn_idx).is_some() {
                continue;
            }
            let Some(index_ty) = value_result_type(ctx, *dyn_idx) else {
                continue;
            };
            if type_def_of(ctx, index_ty).is_none_or(|def| def.class.opcode != Op::TypeInt) {
                continue;
            }
            let Some(base_ptr_ty) = value_result_type(ctx, *base) else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some(elem_ty) = single_member_array_scalar_elem(ctx, base_pointee) else {
                continue;
            };
            if walk_into_type(ctx, base_pointee, &inst.operands[1..]).is_some() {
                continue;
            }
            let view = DynamicBlockView {
                bi,
                ii,
                opcode: inst.class.opcode,
                ac_id,
                base: *base,
                dyn_idx: *dyn_idx,
                index_ty,
                sc,
                result_ptr_ty,
                result_pointee,
                elem_ty,
            };
            if admits(ctx, &view) {
                views.push(view);
            }
        }
    }
    views
}

fn exact_block_view_uses(
    ctx: &Ctx,
    entry_idx: usize,
    views: &mut Vec<DynamicBlockView>,
    admits_value: impl Fn(&Ctx, &DynamicBlockView, Word) -> bool,
) -> HashMap<Word, Vec<(usize, usize)>> {
    let view_of: HashMap<Word, DynamicBlockView> =
        views.iter().map(|view| (view.ac_id, *view)).collect();
    let mut uses: HashMap<Word, Vec<(usize, usize)>> = HashMap::default();
    let mut disqualified: HashSet<Word> = HashSet::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.result_id.is_some_and(|id| view_of.contains_key(&id))
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            for chain_id in inst.operands.iter().filter_map(|operand| match operand {
                Operand::IdRef(id) if view_of.contains_key(id) => Some(*id),
                _ => None,
            }) {
                let value_ty = match inst.class.opcode {
                    Op::Load if only_alignment_hint(&inst.operands[1..]) => inst.result_type,
                    Op::Store
                        if inst.operands.len() >= 2
                            && inst.operands.first() == Some(&Operand::IdRef(chain_id))
                            && only_alignment_hint(&inst.operands[2..]) =>
                    {
                        inst.operands.get(1).and_then(|operand| match operand {
                            Operand::IdRef(value) => value_result_type(ctx, *value),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                if value_ty.is_some_and(|ty| admits_value(ctx, &view_of[&chain_id], ty)) {
                    uses.entry(chain_id).or_default().push((bi, ii));
                } else {
                    disqualified.insert(chain_id);
                }
            }
        }
    }
    views.retain(|view| !disqualified.contains(&view.ac_id) && uses.contains_key(&view.ac_id));
    uses
}

fn replay_block_view_uses(
    ctx: &mut Ctx,
    entry_idx: usize,
    views: &[DynamicBlockView],
    uses: &HashMap<Word, Vec<(usize, usize)>>,
    mut replay: impl FnMut(
        &mut Ctx,
        &DynamicBlockView,
        &Instruction,
        &mut Vec<Instruction>,
    ) -> Result<(), String>,
) -> Result<(), String> {
    let chain_at: HashSet<(usize, usize)> = views.iter().map(|view| (view.bi, view.ii)).collect();
    let view_at: HashMap<(usize, usize), DynamicBlockView> = views
        .iter()
        .flat_map(|view| {
            uses.get(&view.ac_id)
                .into_iter()
                .flatten()
                .map(move |&at| (at, *view))
        })
        .collect();
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len() + 10);
        for (ii, inst) in old.into_iter().enumerate() {
            if chain_at.contains(&(bi, ii)) {
                continue;
            }
            let Some(view) = view_at.get(&(bi, ii)).copied() else {
                rewritten.push(inst);
                continue;
            };
            replay(ctx, &view, &inst, &mut rewritten)?;
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
    Ok(())
}

fn only_alignment_hint(operands: &[Operand]) -> bool {
    match operands {
        [] => true,
        [Operand::MemoryAccess(access), Operand::LiteralBit32(_)] => {
            *access == spirv::MemoryAccess::ALIGNED
        }
        _ => false,
    }
}

pub(in crate::passes) fn fold_block_view_element_offsets(ctx: &mut Ctx, entry_idx: usize) {
    loop {
        let views = dynamic_block_views(ctx, entry_idx, |_, _| true);
        if views.is_empty() {
            return;
        }
        let view_of: HashMap<Word, DynamicBlockView> =
            views.iter().map(|view| (view.ac_id, *view)).collect();
        let mut folds: Vec<(usize, usize, DynamicBlockView, Word, Word)> = Vec::new();
        for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::PtrAccessChain || inst.operands.len() != 2 {
                    continue;
                }
                let (Some(result), Some(result_ty)) = (inst.result_id, inst.result_type) else {
                    continue;
                };
                let (Some(Operand::IdRef(base)), Some(Operand::IdRef(offset))) =
                    (inst.operands.first(), inst.operands.get(1))
                else {
                    continue;
                };
                let Some(view) = view_of.get(base) else {
                    continue;
                };
                if result_ty != view.result_ptr_ty {
                    continue;
                }
                let Some(offset_ty) = value_result_type(ctx, *offset) else {
                    continue;
                };
                if int_type_width(ctx, offset_ty)
                    .zip(int_type_width(ctx, view.index_ty))
                    .is_none_or(|(offset_bits, index_bits)| offset_bits > index_bits)
                {
                    continue;
                }
                folds.push((bi, ii, *view, *offset, result));
            }
        }
        if folds.is_empty() {
            return;
        }
        let planned: HashMap<(usize, usize), (DynamicBlockView, Word, Word)> = folds
            .into_iter()
            .map(|(bi, ii, view, offset, result)| ((bi, ii), (view, offset, result)))
            .collect();
        let n_blocks = ctx.module.functions[entry_idx].blocks.len();
        for bi in 0..n_blocks {
            let old = ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .clone();
            let mut rewritten = Vec::with_capacity(old.len() + planned.len() * 2);
            for (ii, inst) in old.into_iter().enumerate() {
                let Some(&(view, offset, result)) = planned.get(&(bi, ii)) else {
                    rewritten.push(inst);
                    continue;
                };
                let offset_ty = value_result_type(ctx, offset).expect("offset type was checked");
                let widened = if offset_ty == view.index_ty {
                    offset
                } else {
                    let id = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(view.index_ty),
                        Some(id),
                        vec![Operand::IdRef(offset)],
                    ));
                    id
                };
                let sum = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::IAdd,
                    Some(view.index_ty),
                    Some(sum),
                    vec![Operand::IdRef(view.dyn_idx), Operand::IdRef(widened)],
                ));
                rewritten.push(Instruction::new(
                    view.opcode,
                    Some(view.result_ptr_ty),
                    Some(result),
                    vec![Operand::IdRef(view.base), Operand::IdRef(sum)],
                ));
            }
            ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
        }
    }
}

fn int_type_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    let def = type_def_of(ctx, ty)?;
    match (def.class.opcode, def.operands.first()) {
        (Op::TypeInt, Some(Operand::LiteralBit32(bits))) => Some(*bits),
        _ => None,
    }
}

fn store_object(inst: &Instruction, what: &str) -> Result<Word, String> {
    match inst.operands.get(1) {
        Some(Operand::IdRef(value)) => Ok(*value),
        _ => Err(format!("{what} store lost its object operand")),
    }
}

pub(in crate::passes) fn rewrite_dynamic_struct_index_subword_reinterpret(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut views = dynamic_block_views(ctx, entry_idx, |ctx, view| {
        view.sc == StorageClass::StorageBuffer
            && is_unsigned_int_width(ctx, view.elem_ty, 32)
            && direct_scalar_width(ctx, view.result_pointee)
                .is_some_and(|bits| (8..32).contains(&bits) && 32 % bits == 0)
    });
    if views.is_empty() {
        return Ok(());
    }
    let uses = exact_block_view_uses(ctx, entry_idx, &mut views, |_, view, value_ty| {
        value_ty == view.result_pointee
    });
    if views.is_empty() {
        return Ok(());
    }

    let uint = ctx.ty_uint();
    let ptr_uint = ctx.ty_ptr(StorageClass::StorageBuffer, uint);
    let member0 = ctx.const_uint(0);
    replay_block_view_uses(ctx, entry_idx, &views, &uses, |ctx, view, inst, out| {
        let result_bits = direct_scalar_width(ctx, view.result_pointee)
            .ok_or("subword view lost its scalar width")?;
        let (word_ptr, shift_bits) =
            emit_dynamic_lane_address(ctx, view, result_bits, ptr_uint, member0, out);
        let result_uint_ty = uint_type_of_width(ctx, result_bits);
        match inst.class.opcode {
            Op::Load => {
                let result = inst.result_id.ok_or("subword load has a result id")?;
                let loaded_word = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Load,
                    Some(uint),
                    Some(loaded_word),
                    vec![Operand::IdRef(word_ptr)],
                ));
                let shifted = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::ShiftRightLogical,
                    Some(uint),
                    Some(shifted),
                    vec![Operand::IdRef(loaded_word), Operand::IdRef(shift_bits)],
                ));
                let mask = ctx.const_uint((1u32 << result_bits) - 1);
                let masked = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::BitwiseAnd,
                    Some(uint),
                    Some(masked),
                    vec![Operand::IdRef(shifted), Operand::IdRef(mask)],
                ));
                let narrowed = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::UConvert,
                    Some(result_uint_ty),
                    Some(narrowed),
                    vec![Operand::IdRef(masked)],
                ));
                let op = if view.result_pointee == result_uint_ty {
                    Op::CopyObject
                } else {
                    Op::Bitcast
                };
                out.push(Instruction::new(
                    op,
                    Some(view.result_pointee),
                    Some(result),
                    vec![Operand::IdRef(narrowed)],
                ));
            }
            Op::Store => {
                let object = store_object(inst, "subword")?;
                let object_bits = if view.result_pointee == result_uint_ty {
                    object
                } else {
                    let bits = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(result_uint_ty),
                        Some(bits),
                        vec![Operand::IdRef(object)],
                    ));
                    bits
                };
                let object_word = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::UConvert,
                    Some(uint),
                    Some(object_word),
                    vec![Operand::IdRef(object_bits)],
                ));
                let shifted_object = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::ShiftLeftLogical,
                    Some(uint),
                    Some(shifted_object),
                    vec![Operand::IdRef(object_word), Operand::IdRef(shift_bits)],
                ));
                let lane_mask_base = ctx.const_uint((1u32 << result_bits) - 1);
                let lane_mask = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::ShiftLeftLogical,
                    Some(uint),
                    Some(lane_mask),
                    vec![Operand::IdRef(lane_mask_base), Operand::IdRef(shift_bits)],
                ));
                let keep_mask = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Not,
                    Some(uint),
                    Some(keep_mask),
                    vec![Operand::IdRef(lane_mask)],
                ));
                let scope = ctx.const_uint(Scope::Device as u32);
                let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
                let cleared = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::AtomicAnd,
                    Some(uint),
                    Some(cleared),
                    vec![
                        Operand::IdRef(word_ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                        Operand::IdRef(keep_mask),
                    ],
                ));
                let merged = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::AtomicOr,
                    Some(uint),
                    Some(merged),
                    vec![
                        Operand::IdRef(word_ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                        Operand::IdRef(shifted_object),
                    ],
                ));
            }
            _ => return Err("subword dynamic-chain use was not load/store".to_string()),
        }
        Ok(())
    })
}

fn is_unsigned_int_width(ctx: &Ctx, ty: Word, bits: u32) -> bool {
    type_def_of(ctx, ty).is_some_and(|def| {
        def.class.opcode == Op::TypeInt
            && def.operands.first() == Some(&Operand::LiteralBit32(bits))
            && def.operands.get(1) == Some(&Operand::LiteralBit32(0))
    })
}

fn emit_dynamic_lane_address(
    ctx: &mut Ctx,
    view: &DynamicBlockView,
    result_bits: u32,
    ptr_uint: Word,
    member0: Word,
    out: &mut Vec<Instruction>,
) -> (Word, Word) {
    let lanes_per_word = 32 / result_bits;
    let divisor = ctx.const_int_of(view.index_ty, i64::from(lanes_per_word));
    let word_idx = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UDiv,
        Some(view.index_ty),
        Some(word_idx),
        vec![Operand::IdRef(view.dyn_idx), Operand::IdRef(divisor)],
    ));
    let lane_mask = ctx.const_int_of(view.index_ty, i64::from(lanes_per_word - 1));
    let lane = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(view.index_ty),
        Some(lane),
        vec![Operand::IdRef(view.dyn_idx), Operand::IdRef(lane_mask)],
    ));
    let uint = ctx.ty_uint();
    let lane32 = if view.index_ty == uint {
        lane
    } else {
        let converted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::UConvert,
            Some(uint),
            Some(converted),
            vec![Operand::IdRef(lane)],
        ));
        converted
    };
    let lane_bits = ctx.const_uint(result_bits);
    let shift_bits = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(shift_bits),
        vec![Operand::IdRef(lane32), Operand::IdRef(lane_bits)],
    ));
    let word_ptr = emit_block_element_pointer(ctx, view, ptr_uint, member0, word_idx, out);
    (word_ptr, shift_bits)
}

fn emit_block_element_pointer(
    ctx: &mut Ctx,
    view: &DynamicBlockView,
    elem_ptr_ty: Word,
    member0: Word,
    index: Word,
    out: &mut Vec<Instruction>,
) -> Word {
    let ptr = ctx.module.fresh_id();
    out.push(Instruction::new(
        view.opcode,
        Some(elem_ptr_ty),
        Some(ptr),
        vec![
            Operand::IdRef(view.base),
            Operand::IdRef(member0),
            Operand::IdRef(index),
        ],
    ));
    ptr
}

fn emit_index_offset(
    ctx: &mut Ctx,
    view: &DynamicBlockView,
    base: Word,
    lane: u32,
    out: &mut Vec<Instruction>,
) -> Word {
    if lane == 0 {
        return base;
    }
    let offset = ctx.const_int_of(view.index_ty, i64::from(lane));
    let id = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(view.index_ty),
        Some(id),
        vec![Operand::IdRef(base), Operand::IdRef(offset)],
    ));
    id
}

pub(in crate::passes) fn rewrite_dynamic_struct_index_wide_word_reinterpret(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut views = dynamic_block_views(ctx, entry_idx, |ctx, view| {
        view.sc == StorageClass::StorageBuffer
            && is_unsigned_int_width(ctx, view.elem_ty, 32)
            && direct_scalar_width(ctx, view.result_pointee) == Some(64)
    });
    if views.is_empty() {
        return Ok(());
    }
    let uses = exact_block_view_uses(ctx, entry_idx, &mut views, |_, view, value_ty| {
        value_ty == view.result_pointee
    });
    if views.is_empty() {
        return Ok(());
    }

    let uint = ctx.ty_uint();
    let ptr_uint = ctx.ty_ptr(StorageClass::StorageBuffer, uint);
    let member0 = ctx.const_uint(0);
    replay_block_view_uses(ctx, entry_idx, &views, &uses, |ctx, view, inst, out| {
        let result_uint_ty = uint_type_of_width(ctx, 64);
        let words_per_value = ctx.const_int_of(view.index_ty, 2);
        let word_base = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IMul,
            Some(view.index_ty),
            Some(word_base),
            vec![
                Operand::IdRef(view.dyn_idx),
                Operand::IdRef(words_per_value),
            ],
        ));
        match inst.class.opcode {
            Op::Load => {
                let result = inst.result_id.ok_or("wide-word load has a result id")?;
                let mut assembled: Option<Word> = None;
                for word_lane in 0..2 {
                    let word_index = emit_index_offset(ctx, view, word_base, word_lane, out);
                    let word_ptr =
                        emit_block_element_pointer(ctx, view, ptr_uint, member0, word_index, out);
                    let loaded = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Load,
                        Some(uint),
                        Some(loaded),
                        vec![Operand::IdRef(word_ptr)],
                    ));
                    let widened = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::UConvert,
                        Some(result_uint_ty),
                        Some(widened),
                        vec![Operand::IdRef(loaded)],
                    ));
                    let placed = if word_lane == 0 {
                        widened
                    } else {
                        let shift = ctx.const_uint(32);
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::ShiftLeftLogical,
                            Some(result_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(widened), Operand::IdRef(shift)],
                        ));
                        id
                    };
                    assembled = Some(match assembled {
                        None => placed,
                        Some(prev) => {
                            let id = ctx.module.fresh_id();
                            out.push(Instruction::new(
                                Op::BitwiseOr,
                                Some(result_uint_ty),
                                Some(id),
                                vec![Operand::IdRef(prev), Operand::IdRef(placed)],
                            ));
                            id
                        }
                    });
                }
                let packed = assembled.ok_or("wide-word load should assemble at least one word")?;
                let op = if view.result_pointee == result_uint_ty {
                    Op::CopyObject
                } else {
                    Op::Bitcast
                };
                out.push(Instruction::new(
                    op,
                    Some(view.result_pointee),
                    Some(result),
                    vec![Operand::IdRef(packed)],
                ));
            }
            Op::Store => {
                let object = store_object(inst, "wide-word")?;
                let object_bits = if view.result_pointee == result_uint_ty {
                    object
                } else {
                    let bits = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(result_uint_ty),
                        Some(bits),
                        vec![Operand::IdRef(object)],
                    ));
                    bits
                };
                for word_lane in 0..2 {
                    let shifted = if word_lane == 0 {
                        object_bits
                    } else {
                        let shift = ctx.const_uint(32);
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::ShiftRightLogical,
                            Some(result_uint_ty),
                            Some(id),
                            vec![Operand::IdRef(object_bits), Operand::IdRef(shift)],
                        ));
                        id
                    };
                    let word_value = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::UConvert,
                        Some(uint),
                        Some(word_value),
                        vec![Operand::IdRef(shifted)],
                    ));
                    let word_index = emit_index_offset(ctx, view, word_base, word_lane, out);
                    let word_ptr =
                        emit_block_element_pointer(ctx, view, ptr_uint, member0, word_index, out);
                    out.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(word_ptr), Operand::IdRef(word_value)],
                    ));
                }
            }
            _ => return Err("wide-word dynamic-chain use was not load/store".to_string()),
        }
        Ok(())
    })
}

fn vector_components(ctx: &Ctx, ty: Word) -> Option<(Word, u32)> {
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeVector {
        return None;
    }
    let (Operand::IdRef(component), Operand::LiteralBit32(lanes)) =
        (def.operands.first()?, def.operands.get(1)?)
    else {
        return None;
    };
    (*lanes >= 2 && direct_scalar_width(ctx, *component).is_some()).then_some((*component, *lanes))
}

pub(in crate::passes) fn rewrite_dynamic_struct_index_vector_reinterpret(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut views = dynamic_block_views(ctx, entry_idx, |ctx, view| {
        vector_components(ctx, view.result_pointee).is_some_and(|(component_ty, _)| {
            direct_scalar_width(ctx, view.elem_ty) == direct_scalar_width(ctx, component_ty)
        })
    });
    if views.is_empty() {
        return Ok(());
    }
    let uses = exact_block_view_uses(ctx, entry_idx, &mut views, |_, view, value_ty| {
        value_ty == view.result_pointee
    });
    if views.is_empty() {
        return Ok(());
    }

    let member0 = ctx.const_uint(0);
    let mut elem_ptr_ty = HashMap::default();
    for view in &views {
        elem_ptr_ty
            .entry((view.sc, view.elem_ty))
            .or_insert_with(|| ctx.ty_ptr(view.sc, view.elem_ty));
    }
    replay_block_view_uses(ctx, entry_idx, &views, &uses, |ctx, view, inst, out| {
        let (component_ty, lanes) =
            vector_components(ctx, view.result_pointee).ok_or("vector view lost its shape")?;
        let elem_ptr_ty = elem_ptr_ty[&(view.sc, view.elem_ty)];
        let lane_base = ctx.module.fresh_id();
        let lane_factor = ctx.const_int_of(view.index_ty, i64::from(lanes));
        out.push(Instruction::new(
            Op::IMul,
            Some(view.index_ty),
            Some(lane_base),
            vec![Operand::IdRef(view.dyn_idx), Operand::IdRef(lane_factor)],
        ));
        let mut lane_ptrs = Vec::with_capacity(lanes as usize);
        for lane in 0..lanes {
            let lane_index = emit_index_offset(ctx, view, lane_base, lane, out);
            lane_ptrs.push(emit_block_element_pointer(
                ctx,
                view,
                elem_ptr_ty,
                member0,
                lane_index,
                out,
            ));
        }
        match inst.class.opcode {
            Op::Load => {
                let result = inst.result_id.ok_or("vector load has a result id")?;
                let mut components = Vec::with_capacity(lanes as usize);
                for ptr in lane_ptrs {
                    let loaded = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Load,
                        Some(view.elem_ty),
                        Some(loaded),
                        vec![Operand::IdRef(ptr)],
                    ));
                    let component = if view.elem_ty == component_ty {
                        loaded
                    } else {
                        let cast = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::Bitcast,
                            Some(component_ty),
                            Some(cast),
                            vec![Operand::IdRef(loaded)],
                        ));
                        cast
                    };
                    components.push(Operand::IdRef(component));
                }
                out.push(Instruction::new(
                    Op::CompositeConstruct,
                    Some(view.result_pointee),
                    Some(result),
                    components,
                ));
            }
            Op::Store => {
                let object = store_object(inst, "vector")?;
                for (lane, ptr) in lane_ptrs.into_iter().enumerate() {
                    let component = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(component_ty),
                        Some(component),
                        vec![Operand::IdRef(object), Operand::LiteralBit32(lane as u32)],
                    ));
                    let element = if view.elem_ty == component_ty {
                        component
                    } else {
                        let cast = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::Bitcast,
                            Some(view.elem_ty),
                            Some(cast),
                            vec![Operand::IdRef(component)],
                        ));
                        cast
                    };
                    out.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(ptr), Operand::IdRef(element)],
                    ));
                }
            }
            _ => return Err("vector dynamic-chain use was not load/store".to_string()),
        }
        Ok(())
    })
}

pub(in crate::passes) fn rewrite_dynamic_homogeneous_struct_index_load(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let value_types = function_value_types(ctx, entry_idx);
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(sc)), Some(Operand::IdRef(pointee))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*sc, *pointee));
            }
        }
    }

    fn homogeneous_struct_member_path(
        ctx: &Ctx,
        ty: Word,
        result_pointee: Word,
    ) -> Option<(Word, u32, Vec<u32>)> {
        let def = type_def_of(ctx, ty)?;
        if def.class.opcode != Op::TypeStruct || def.operands.is_empty() {
            return None;
        }
        let first = match def.operands.first()? {
            Operand::IdRef(member) => *member,
            _ => return None,
        };
        if def
            .operands
            .iter()
            .all(|operand| matches!(operand, Operand::IdRef(member) if *member == first))
            && first == result_pointee
        {
            return Some((first, def.operands.len() as u32, Vec::new()));
        }
        if def.operands.len() == 1 {
            let (member, len, mut prefix) =
                homogeneous_struct_member_path(ctx, first, result_pointee)?;
            prefix.insert(0, 0);
            Some((member, len, prefix))
        } else {
            None
        }
    }
    let bool_select_type = |ctx: &mut Ctx, ty: Word| -> Option<Word> {
        match type_def_of(ctx, ty).map(|def| def.class.opcode) {
            Some(Op::TypeInt | Op::TypeFloat | Op::TypeBool | Op::TypePointer) => {
                Some(ctx.ty_bool())
            }
            Some(Op::TypeVector) => {
                let def = type_def_of(ctx, ty)?;
                let Some(Operand::LiteralBit32(lanes)) = def.operands.get(1) else {
                    return None;
                };
                Some(ctx.ty_vec_bool(*lanes))
            }
            _ => None,
        }
    };
    let selectable_member_type = |ctx: &Ctx, ty: Word| -> bool {
        match type_def_of(ctx, ty).map(|def| def.class.opcode) {
            Some(Op::TypeInt | Op::TypeFloat | Op::TypeBool | Op::TypePointer) => true,
            Some(Op::TypeVector) => type_def_of(ctx, ty)
                .is_some_and(|def| matches!(def.operands.get(1), Some(Operand::LiteralBit32(_)))),
            _ => false,
        }
    };

    #[derive(Clone)]
    struct Plan {
        bi: usize,
        ii: usize,
        chain_id: Word,
        base: Word,
        dyn_idx: Word,
        index_ty: Word,
        member_ty: Word,
        ptr_ty: Word,
        opcode: Op,
        members: u32,
        prefix: Vec<u32>,
    }
    let mut plans = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                || inst.operands.len() != 2
            {
                continue;
            }
            let (Some(chain_id), Some(ptr_ty)) = (inst.result_id, inst.result_type) else {
                continue;
            };
            let Some(&(_, result_pointee)) = ptr_info.get(&ptr_ty) else {
                continue;
            };
            let (Some(Operand::IdRef(base)), Some(Operand::IdRef(dyn_idx))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            if const_u32(ctx, *dyn_idx).is_some() {
                continue;
            }
            let Some(index_ty) = value_types.get(dyn_idx).copied() else {
                continue;
            };
            if type_def_of(ctx, index_ty).is_none_or(|def| def.class.opcode != Op::TypeInt) {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some((member_ty, members, prefix)) =
                homogeneous_struct_member_path(ctx, base_pointee, result_pointee)
            else {
                continue;
            };
            if !selectable_member_type(ctx, member_ty) {
                continue;
            }
            plans.push(Plan {
                bi,
                ii,
                chain_id,
                base: *base,
                dyn_idx: *dyn_idx,
                index_ty,
                member_ty,
                ptr_ty,
                opcode: inst.class.opcode,
                members,
                prefix,
            });
        }
    }
    if plans.is_empty() {
        return Ok(());
    }

    let plan_ids: HashSet<Word> = plans.iter().map(|plan| plan.chain_id).collect();
    let mut use_sites: HashMap<Word, Vec<(usize, usize)>> = HashMap::default();
    let mut disqualified = HashSet::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.result_id.is_some_and(|id| plan_ids.contains(&id))
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            for chain_id in inst.operands.iter().filter_map(|operand| match operand {
                Operand::IdRef(id) if plan_ids.contains(id) => Some(*id),
                _ => None,
            }) {
                if inst.class.opcode == Op::Load
                    && inst.operands.len() == 1
                    && matches!(inst.operands.first(), Some(Operand::IdRef(id)) if *id == chain_id)
                {
                    use_sites.entry(chain_id).or_default().push((bi, ii));
                } else {
                    disqualified.insert(chain_id);
                }
            }
        }
    }
    plans.retain(|plan| {
        !disqualified.contains(&plan.chain_id) && use_sites.contains_key(&plan.chain_id)
    });
    if plans.is_empty() {
        return Ok(());
    }

    let plan_by_id: HashMap<Word, Plan> = plans
        .iter()
        .map(|plan| (plan.chain_id, plan.clone()))
        .collect();
    let chain_at: HashMap<(usize, usize), Word> = plans
        .iter()
        .map(|plan| ((plan.bi, plan.ii), plan.chain_id))
        .collect();
    let use_at: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|plan| {
            use_sites
                .get(&plan.chain_id)
                .into_iter()
                .flatten()
                .map(move |&(bi, ii)| ((bi, ii), plan.chain_id))
        })
        .collect();

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len() + 16);
        for (ii, inst) in old.into_iter().enumerate() {
            if chain_at.contains_key(&(bi, ii)) {
                continue;
            }
            let Some(chain_id) = use_at.get(&(bi, ii)).copied() else {
                rewritten.push(inst);
                continue;
            };
            let plan = &plan_by_id[&chain_id];
            let result = inst
                .result_id
                .ok_or("dynamic homogeneous struct load lost result id")?;
            let Some(cond_ty) = bool_select_type(ctx, plan.member_ty) else {
                rewritten.push(inst);
                continue;
            };
            let mut selected = None;
            for member in 0..plan.members {
                let member_idx = ctx.const_int_of(plan.index_ty, i64::from(member));
                let ptr = ctx.module.fresh_id();
                let mut chain_ops = Vec::with_capacity(plan.prefix.len() + 2);
                chain_ops.push(Operand::IdRef(plan.base));
                for prefix in &plan.prefix {
                    chain_ops.push(Operand::IdRef(ctx.const_uint(*prefix)));
                }
                chain_ops.push(Operand::IdRef(member_idx));
                rewritten.push(Instruction::new(
                    plan.opcode,
                    Some(plan.ptr_ty),
                    Some(ptr),
                    chain_ops,
                ));
                let loaded = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::Load,
                    Some(plan.member_ty),
                    Some(loaded),
                    vec![Operand::IdRef(ptr)],
                ));
                let Some(prev) = selected else {
                    selected = Some(loaded);
                    continue;
                };
                let cmp = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::IEqual,
                    Some(ctx.ty_bool()),
                    Some(cmp),
                    vec![Operand::IdRef(plan.dyn_idx), Operand::IdRef(member_idx)],
                ));
                let cond = if cond_ty == ctx.ty_bool() {
                    cmp
                } else {
                    let cond = ctx.module.fresh_id();
                    let lanes = type_def_of(ctx, cond_ty)
                        .and_then(|def| match def.operands.get(1) {
                            Some(Operand::LiteralBit32(lanes)) => Some(*lanes),
                            _ => None,
                        })
                        .ok_or("vector bool type missing lane count")?;
                    rewritten.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(cond_ty),
                        Some(cond),
                        (0..lanes).map(|_| Operand::IdRef(cmp)).collect(),
                    ));
                    cond
                };
                let select = if member + 1 == plan.members {
                    result
                } else {
                    ctx.module.fresh_id()
                };
                rewritten.push(Instruction::new(
                    Op::Select,
                    Some(plan.member_ty),
                    Some(select),
                    vec![
                        Operand::IdRef(cond),
                        Operand::IdRef(loaded),
                        Operand::IdRef(prev),
                    ],
                ));
                selected = Some(select);
            }
            if plan.members == 1 {
                let selected = selected.ok_or("homogeneous struct member load missing value")?;
                rewritten.push(Instruction::new(
                    Op::CopyObject,
                    Some(plan.member_ty),
                    Some(result),
                    vec![Operand::IdRef(selected)],
                ));
            }
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
    Ok(())
}

pub(in crate::passes) fn single_member_array_scalar_elem(
    ctx: &Ctx,
    struct_ty: Word,
) -> Option<Word> {
    let def = type_def_of(ctx, struct_ty)?;
    if def.class.opcode != Op::TypeStruct || def.operands.len() != 1 {
        return None;
    }
    let Operand::IdRef(member0) = def.operands.first()? else {
        return None;
    };
    let mdef = type_def_of(ctx, *member0)?;
    if !matches!(mdef.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
        return None;
    }
    let Operand::IdRef(elem) = mdef.operands.first()? else {
        return None;
    };
    direct_scalar_width(ctx, *elem).map(|_| *elem)
}

pub(in crate::passes) fn rewrite_chained_element_reinterpret(
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

    let mut chain_def: HashMap<Word, Instruction> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                if let Some(rid) = inst.result_id {
                    chain_def.insert(rid, inst.clone());
                }
            }
        }
    }

    let two_index_zero_lead = |ctx: &Ctx, inst: &Instruction| -> Option<(Word, Word)> {
        if inst.operands.len() != 3 {
            return None;
        }
        let (Operand::IdRef(base), Operand::IdRef(i0), Operand::IdRef(i1)) =
            (&inst.operands[0], &inst.operands[1], &inst.operands[2])
        else {
            return None;
        };
        if const_u32(ctx, *i0) != Some(0) {
            return None;
        }
        Some((*base, *i1))
    };

    struct Plan {
        bi: usize,
        ii: usize,
        ac_id: Word,
        buf: Word,
        inner_idx: Word,
        out_idx: Word,
        elem_ty: Word,
        result_pointee: Word,
        sc: StorageClass,
    }
    let mut plans: Vec<Plan> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let (Some(ac_id), Some(result_type)) = (inst.result_id, inst.result_type) else {
                continue;
            };
            let Some((inner, out_idx)) = two_index_zero_lead(ctx, inst) else {
                continue;
            };
            let Some(&(sc, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            if !ptr_access_chain_allowed_storage(sc) {
                continue;
            }
            let Some(pw) = direct_scalar_width(ctx, result_pointee) else {
                continue;
            };
            let Some(inner_inst) = chain_def.get(&inner) else {
                continue;
            };
            let Some((buf, inner_idx)) = two_index_zero_lead(ctx, inner_inst) else {
                continue;
            };
            let Some(inner_result_type) = inner_inst.result_type else {
                continue;
            };
            let Some(&(_, inner_pointee)) = ptr_info.get(&inner_result_type) else {
                continue;
            };
            let Some(buf_ptr_ty) = value_result_type(ctx, buf) else {
                continue;
            };
            let Some(&(_, buf_pointee)) = ptr_info.get(&buf_ptr_ty) else {
                continue;
            };
            let Some(elem_ty) = single_member_array_scalar_elem(ctx, buf_pointee) else {
                continue;
            };
            if inner_pointee != elem_ty {
                continue;
            }
            if direct_scalar_width(ctx, elem_ty) != Some(pw) {
                continue;
            }
            if walk_into_type(ctx, inner_pointee, &inst.operands[1..]).is_some() {
                continue;
            }
            plans.push(Plan {
                bi,
                ii,
                ac_id,
                buf,
                inner_idx,
                out_idx,
                elem_ty,
                result_pointee,
                sc,
            });
        }
    }
    if plans.is_empty() {
        return Ok(());
    }

    let plan_ids: HashSet<Word> = plans.iter().map(|p| p.ac_id).collect();
    let result_pointee_of: HashMap<Word, Word> =
        plans.iter().map(|p| (p.ac_id, p.result_pointee)).collect();
    let mut use_sites: HashMap<Word, Vec<(usize, usize, bool)>> = HashMap::default();
    let mut disqualified: HashSet<Word> = HashSet::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst
                .result_id
                .map(|r| plan_ids.contains(&r))
                .unwrap_or(false)
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            if inst.result_type == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, true));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                Op::Store => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            let obj = match inst.operands.get(1) {
                                Some(Operand::IdRef(o)) => value_result_type(ctx, *o),
                                _ => None,
                            };
                            if obj == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, false));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                _ => {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            if plan_ids.contains(id) {
                                disqualified.insert(*id);
                            }
                        }
                    }
                }
            }
        }
    }
    plans.retain(|p| !disqualified.contains(&p.ac_id) && use_sites.contains_key(&p.ac_id));
    if plans.is_empty() {
        return Ok(());
    }

    let member0 = ctx.const_uint(0);
    let uint_ty = ctx.ty_uint();
    let mut new_ptr_ty: HashMap<(StorageClass, Word), Word> = HashMap::default();
    let mut sum_id: HashMap<Word, Word> = HashMap::default();
    for p in &plans {
        new_ptr_ty
            .entry((p.sc, p.elem_ty))
            .or_insert_with(|| ctx.ty_ptr(p.sc, p.elem_ty));
        sum_id.insert(p.ac_id, ctx.module.fresh_id());
    }
    let load_split_id: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|p| use_sites.get(&p.ac_id).into_iter().flatten())
        .filter(|(_, _, is_load)| *is_load)
        .map(|&(bi, ii, _)| ((bi, ii), 0))
        .collect::<HashMap<_, _>>()
        .into_keys()
        .map(|k| (k, ctx.module.fresh_id()))
        .collect();
    let store_cast_id: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|p| use_sites.get(&p.ac_id).into_iter().flatten())
        .filter(|(_, _, is_load)| !*is_load)
        .map(|&(bi, ii, _)| ((bi, ii), 0))
        .collect::<HashMap<_, _>>()
        .into_keys()
        .map(|k| (k, ctx.module.fresh_id()))
        .collect();

    let ac_at: HashMap<(usize, usize), &Plan> = plans.iter().map(|p| ((p.bi, p.ii), p)).collect();
    let elem_ty_of_use: HashMap<(usize, usize), (Word, Word)> = plans
        .iter()
        .flat_map(|p| {
            use_sites
                .get(&p.ac_id)
                .into_iter()
                .flatten()
                .map(move |&(bi, ii, _)| ((bi, ii), (p.elem_ty, p.result_pointee)))
        })
        .collect();

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut newv: Vec<Instruction> = Vec::with_capacity(old.len() + 4);
        for (ii, inst) in old.into_iter().enumerate() {
            if let Some(p) = ac_at.get(&(bi, ii)) {
                let sum = sum_id[&p.ac_id];
                newv.push(Instruction::new(
                    Op::IAdd,
                    Some(uint_ty),
                    Some(sum),
                    vec![Operand::IdRef(p.inner_idx), Operand::IdRef(p.out_idx)],
                ));
                let new_ty = new_ptr_ty[&(p.sc, p.elem_ty)];
                newv.push(Instruction::new(
                    inst.class.opcode,
                    Some(new_ty),
                    Some(p.ac_id),
                    vec![
                        Operand::IdRef(p.buf),
                        Operand::IdRef(member0),
                        Operand::IdRef(sum),
                    ],
                ));
                continue;
            }
            if let Some(&load_id) = load_split_id.get(&(bi, ii)) {
                let (elem_ty, result_pointee) = elem_ty_of_use[&(bi, ii)];
                let ptr = match inst.operands.first() {
                    Some(Operand::IdRef(p)) => *p,
                    _ => return Err("load split site lost its pointer operand".to_string()),
                };
                let res = inst.result_id.ok_or("load has a result id")?;
                let mut load_ops = vec![Operand::IdRef(ptr)];
                load_ops.extend(inst.operands.iter().skip(1).cloned());
                newv.push(Instruction::new(
                    Op::Load,
                    Some(elem_ty),
                    Some(load_id),
                    load_ops,
                ));
                newv.push(Instruction::new(
                    Op::Bitcast,
                    Some(result_pointee),
                    Some(res),
                    vec![Operand::IdRef(load_id)],
                ));
                continue;
            }
            if let Some(&cast_id) = store_cast_id.get(&(bi, ii)) {
                let (elem_ty, _result_pointee) = elem_ty_of_use[&(bi, ii)];
                let ptr = match inst.operands.first() {
                    Some(Operand::IdRef(p)) => *p,
                    _ => return Err("store cast site lost its pointer operand".to_string()),
                };
                let obj = match inst.operands.get(1) {
                    Some(Operand::IdRef(o)) => *o,
                    _ => return Err("store cast site lost its object operand".to_string()),
                };
                newv.push(Instruction::new(
                    Op::Bitcast,
                    Some(elem_ty),
                    Some(cast_id),
                    vec![Operand::IdRef(obj)],
                ));
                let mut store_ops = vec![Operand::IdRef(ptr), Operand::IdRef(cast_id)];
                store_ops.extend(inst.operands.iter().skip(2).cloned());
                newv.push(Instruction::new(Op::Store, None, None, store_ops));
                continue;
            }
            newv.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = newv;
    }
    Ok(())
}

pub(in crate::passes) fn rewrite_byte_buffer_chained_reinterpret(
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

    let is_int = |ctx: &Ctx, ty: Word| -> bool {
        type_def_of(ctx, ty)
            .map(|d| d.class.opcode == Op::TypeInt)
            .unwrap_or(false)
    };
    let is_float = |ctx: &Ctx, ty: Word| -> bool {
        type_def_of(ctx, ty).is_some_and(|def| def.class.opcode == Op::TypeFloat)
    };
    let array_stride: HashMap<Word, u32> = ctx
        .module
        .annotations
        .iter()
        .filter_map(|inst| match inst.operands.as_slice() {
            [
                Operand::IdRef(ty),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(stride),
            ] if inst.class.opcode == Op::Decorate => Some((*ty, *stride)),
            _ => None,
        })
        .collect();

    let mut chain_def: HashMap<Word, Instruction> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                if let Some(rid) = inst.result_id {
                    chain_def.insert(rid, inst.clone());
                }
            }
        }
    }

    struct Plan {
        bi: usize,
        ii: usize,
        ac_id: Word,
        buf: Word,
        prefix: Vec<Operand>,
        inner_idx: Word,
        elem_ty: Word,
        result_pointee: Word,
        out_idx: Word,
        ratio: u32,
        slot_w: u32,
    }
    let mut plans: Vec<Plan> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let (Some(ac_id), Some(result_type)) = (inst.result_id, inst.result_type) else {
                continue;
            };
            let (Operand::IdRef(inner), Some(Operand::IdRef(out_idx))) = (
                &inst.operands[0],
                match inst.operands.as_slice() {
                    [_, index] => Some(index),
                    [_, Operand::IdRef(zero), index] if const_u32(ctx, *zero) == Some(0) => {
                        Some(index)
                    }
                    _ => None,
                },
            ) else {
                continue;
            };
            let Some(&(sc, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            if sc != StorageClass::StorageBuffer {
                continue;
            }
            let Some(w) = direct_scalar_width(ctx, result_pointee) else {
                continue;
            };
            if !is_int(ctx, result_pointee) {
                continue;
            }
            let Some(inner_inst) = chain_def.get(inner) else {
                continue;
            };
            if inner_inst.operands.len() < 2 {
                continue;
            }
            let (Some(Operand::IdRef(buf)), Some(Operand::IdRef(inner_idx))) =
                (inner_inst.operands.first(), inner_inst.operands.last())
            else {
                continue;
            };
            let prefix = inner_inst.operands[1..inner_inst.operands.len() - 1].to_vec();
            let Some(inner_result_type) = inner_inst.result_type else {
                continue;
            };
            let Some(&(_, inner_pointee)) = ptr_info.get(&inner_result_type) else {
                continue;
            };
            let Some(buf_ptr_ty) = value_result_type(ctx, *buf) else {
                continue;
            };
            let Some(&(_, buf_pointee)) = ptr_info.get(&buf_ptr_ty) else {
                continue;
            };
            let Some(parent_ty) = walk_into_type(ctx, buf_pointee, &prefix) else {
                continue;
            };
            let Some(parent_def) = type_def_of(ctx, parent_ty) else {
                continue;
            };
            if !matches!(
                parent_def.class.opcode,
                Op::TypeArray | Op::TypeRuntimeArray
            ) {
                continue;
            }
            let Some(Operand::IdRef(elem_ty)) = parent_def.operands.first() else {
                continue;
            };
            if inner_pointee != *elem_ty || (!is_int(ctx, *elem_ty) && !is_float(ctx, *elem_ty)) {
                continue;
            }
            let Some(v) = direct_scalar_width(ctx, *elem_ty) else {
                continue;
            };
            if array_stride.get(&parent_ty).copied() != Some(v / 8) {
                continue;
            }
            if v == 0 || w % v != 0 {
                continue;
            }
            let ratio = w / v;
            if ratio < 2 {
                continue;
            }
            if walk_into_type(ctx, inner_pointee, &inst.operands[1..]).is_some() {
                continue;
            }
            plans.push(Plan {
                bi,
                ii,
                ac_id,
                buf: *buf,
                prefix,
                inner_idx: *inner_idx,
                elem_ty: *elem_ty,
                result_pointee,
                out_idx: *out_idx,
                ratio,
                slot_w: v,
            });
        }
    }
    if plans.is_empty() {
        return Ok(());
    }

    let plan_ids: HashSet<Word> = plans.iter().map(|p| p.ac_id).collect();
    let result_pointee_of: HashMap<Word, Word> =
        plans.iter().map(|p| (p.ac_id, p.result_pointee)).collect();
    let mut use_sites: HashMap<Word, Vec<(usize, usize, bool)>> = HashMap::default();
    let mut disqualified: HashSet<Word> = HashSet::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst
                .result_id
                .map(|r| plan_ids.contains(&r))
                .unwrap_or(false)
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            if inst.result_type == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, true));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                Op::Store => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if plan_ids.contains(ptr) {
                            let obj = match inst.operands.get(1) {
                                Some(Operand::IdRef(o)) => value_result_type(ctx, *o),
                                _ => None,
                            };
                            if obj == result_pointee_of.get(ptr).copied() {
                                use_sites.entry(*ptr).or_default().push((bi, ii, false));
                            } else {
                                disqualified.insert(*ptr);
                            }
                        }
                    }
                }
                _ => {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            if plan_ids.contains(id) {
                                disqualified.insert(*id);
                            }
                        }
                    }
                }
            }
        }
    }
    plans.retain(|p| !disqualified.contains(&p.ac_id) && use_sites.contains_key(&p.ac_id));
    if plans.is_empty() {
        return Ok(());
    }

    let uint_ty = ctx.ty_uint();
    let mut elem_ptr_ty: HashMap<Word, Word> = HashMap::default();
    let mut sum_id: HashMap<Word, Word> = HashMap::default();
    let mut ratio_const: HashMap<u32, Word> = HashMap::default();
    for p in &plans {
        elem_ptr_ty
            .entry(p.elem_ty)
            .or_insert_with(|| ctx.ty_ptr(StorageClass::StorageBuffer, p.elem_ty));
        sum_id.insert(p.ac_id, ctx.module.fresh_id());
        ratio_const
            .entry(p.ratio)
            .or_insert_with(|| ctx.const_uint(p.ratio));
    }

    let ac_at: HashMap<(usize, usize), Word> =
        plans.iter().map(|p| ((p.bi, p.ii), p.ac_id)).collect();
    let plan_by_id: HashMap<Word, &Plan> = plans.iter().map(|p| (p.ac_id, p)).collect();
    let use_at: HashMap<(usize, usize), Word> = plans
        .iter()
        .flat_map(|p| {
            use_sites
                .get(&p.ac_id)
                .into_iter()
                .flatten()
                .map(move |&(bi, ii, _)| ((bi, ii), p.ac_id))
        })
        .collect();

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut newv: Vec<Instruction> = Vec::with_capacity(old.len() + 8);
        for (ii, inst) in old.into_iter().enumerate() {
            if let Some(&ac_id) = ac_at.get(&(bi, ii)) {
                let plan = plan_by_id[&ac_id];
                let sum = sum_id[&ac_id];
                let mul = ctx.module.fresh_id();
                newv.push(Instruction::new(
                    Op::IMul,
                    Some(uint_ty),
                    Some(mul),
                    vec![
                        Operand::IdRef(plan.out_idx),
                        Operand::IdRef(ratio_const[&plan.ratio]),
                    ],
                ));
                newv.push(Instruction::new(
                    Op::IAdd,
                    Some(uint_ty),
                    Some(sum),
                    vec![Operand::IdRef(plan.inner_idx), Operand::IdRef(mul)],
                ));
                continue;
            }
            if let Some(&ac_id) = use_at.get(&(bi, ii)) {
                let plan = plan_by_id[&ac_id];
                let (buf, elem_ty, result_pointee, ratio, slot_w) = (
                    plan.buf,
                    plan.elem_ty,
                    plan.result_pointee,
                    plan.ratio,
                    plan.slot_w,
                );
                let sum = sum_id[&ac_id];
                let eptr = elem_ptr_ty[&elem_ty];
                let slot_int_ty = if is_float(ctx, elem_ty) {
                    ctx.get_or_create(
                        Op::TypeInt,
                        None,
                        vec![Operand::LiteralBit32(slot_w), Operand::LiteralBit32(0)],
                    )
                } else {
                    elem_ty
                };
                let mut slot_ptr = Vec::with_capacity(ratio as usize);
                for j in 0..ratio {
                    let idx = if j == 0 {
                        sum
                    } else {
                        let off = ctx.const_uint(j);
                        let id = ctx.module.fresh_id();
                        newv.push(Instruction::new(
                            Op::IAdd,
                            Some(uint_ty),
                            Some(id),
                            vec![Operand::IdRef(sum), Operand::IdRef(off)],
                        ));
                        id
                    };
                    let pid = ctx.module.fresh_id();
                    let mut operands = vec![Operand::IdRef(buf)];
                    operands.extend(plan.prefix.iter().cloned());
                    operands.push(Operand::IdRef(idx));
                    newv.push(Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(eptr),
                        Some(pid),
                        operands,
                    ));
                    slot_ptr.push(pid);
                }
                if inst.class.opcode == Op::Load {
                    let res = inst.result_id.ok_or("load has a result id")?;
                    let mut acc: Option<Word> = None;
                    for (j, &pid) in slot_ptr.iter().enumerate() {
                        let raw = ctx.module.fresh_id();
                        newv.push(Instruction::new(
                            Op::Load,
                            Some(elem_ty),
                            Some(raw),
                            vec![Operand::IdRef(pid)],
                        ));
                        let raw_bits = if slot_int_ty == elem_ty {
                            raw
                        } else {
                            let bits = ctx.module.fresh_id();
                            newv.push(Instruction::new(
                                Op::Bitcast,
                                Some(slot_int_ty),
                                Some(bits),
                                vec![Operand::IdRef(raw)],
                            ));
                            bits
                        };
                        let wide = ctx.module.fresh_id();
                        newv.push(Instruction::new(
                            Op::UConvert,
                            Some(result_pointee),
                            Some(wide),
                            vec![Operand::IdRef(raw_bits)],
                        ));
                        let shifted = if j == 0 {
                            wide
                        } else {
                            let sh = ctx.const_uint(j as u32 * slot_w);
                            let sid = ctx.module.fresh_id();
                            newv.push(Instruction::new(
                                Op::ShiftLeftLogical,
                                Some(result_pointee),
                                Some(sid),
                                vec![Operand::IdRef(wide), Operand::IdRef(sh)],
                            ));
                            sid
                        };
                        acc = Some(match acc {
                            None => shifted,
                            Some(prev) => {
                                let last = j as u32 == ratio - 1;
                                let oid = if last { res } else { ctx.module.fresh_id() };
                                newv.push(Instruction::new(
                                    Op::BitwiseOr,
                                    Some(result_pointee),
                                    Some(oid),
                                    vec![Operand::IdRef(prev), Operand::IdRef(shifted)],
                                ));
                                oid
                            }
                        });
                    }
                    let _ = acc;
                } else {
                    let obj = match inst.operands.get(1) {
                        Some(Operand::IdRef(o)) => *o,
                        _ => {
                            return Err(
                                "store through reinterpret chain lost its object".to_string()
                            )
                        }
                    };
                    for (j, &pid) in slot_ptr.iter().enumerate() {
                        let shifted = if j == 0 {
                            obj
                        } else {
                            let sh = ctx.const_uint(j as u32 * slot_w);
                            let sid = ctx.module.fresh_id();
                            newv.push(Instruction::new(
                                Op::ShiftRightLogical,
                                Some(result_pointee),
                                Some(sid),
                                vec![Operand::IdRef(obj), Operand::IdRef(sh)],
                            ));
                            sid
                        };
                        let narrow_bits = ctx.module.fresh_id();
                        newv.push(Instruction::new(
                            Op::UConvert,
                            Some(slot_int_ty),
                            Some(narrow_bits),
                            vec![Operand::IdRef(shifted)],
                        ));
                        let narrow = if slot_int_ty == elem_ty {
                            narrow_bits
                        } else {
                            let value = ctx.module.fresh_id();
                            newv.push(Instruction::new(
                                Op::Bitcast,
                                Some(elem_ty),
                                Some(value),
                                vec![Operand::IdRef(narrow_bits)],
                            ));
                            value
                        };
                        newv.push(Instruction::new(
                            Op::Store,
                            None,
                            None,
                            vec![Operand::IdRef(pid), Operand::IdRef(narrow)],
                        ));
                    }
                }
                continue;
            }
            newv.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = newv;
    }
    Ok(())
}
