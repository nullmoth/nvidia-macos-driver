use super::*;

pub(in crate::passes) fn combined_value_defs(
    ctx: &Ctx,
    entry_idx: usize,
) -> HashMap<Word, Instruction> {
    let mut value_defs = HashMap::default();
    for inst in ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
    {
        if let Some(id) = inst.result_id {
            value_defs.insert(id, inst.clone());
        }
    }
    for param in &ctx.module.functions[entry_idx].parameters {
        if let Some(id) = param.result_id {
            value_defs.insert(id, param.clone());
        }
    }
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if let Some(id) = inst.result_id {
                value_defs.insert(id, inst.clone());
            }
        }
    }
    value_defs
}

pub(in crate::passes) fn rewritten_pointer_constant_operands(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    value_defs: &mut HashMap<Word, Instruction>,
    op: Op,
    operands: &[Operand],
    new_ty: Word,
) -> Vec<(usize, Word)> {
    operands
        .iter()
        .enumerate()
        .filter_map(|(operand_idx, operand)| {
            if op == Op::Select && operand_idx == 0 {
                return None;
            }
            if op == Op::Phi && operand_idx % 2 == 1 {
                return None;
            }
            let Operand::IdRef(id) = operand else {
                return None;
            };
            let old_ty = value_types.get(id).copied()?;
            if old_ty == new_ty {
                return None;
            }
            let def = value_defs.get(id)?;
            if !matches!(def.class.opcode, Op::ConstantNull | Op::Undef) {
                return None;
            }
            let replacement = ctx.module.fresh_id();
            let replacement_inst =
                Instruction::new(def.class.opcode, Some(new_ty), Some(replacement), vec![]);
            ctx.new_globals.push(replacement_inst.clone());
            value_types.insert(replacement, new_ty);
            value_defs.insert(replacement, replacement_inst);
            Some((operand_idx, replacement))
        })
        .collect()
}

pub(in crate::passes) fn rewrite_flattened_workgroup_leaf_accesses(
    ctx: &mut Ctx,
    entry_idx: usize,
    root_vars: &[Word],
    defs: &HashMap<Word, Instruction>,
) {
    let roots = root_vars.iter().copied().collect::<HashSet<_>>();
    let mut types = combined_type_defs(ctx, defs);
    let mut value_types = combined_value_types(ctx, entry_idx);
    let mut value_defs = combined_value_defs(ctx, entry_idx);
    let desired_pointees = pointer_leaf_use_types(ctx, entry_idx, &value_types);
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();

    for bi in 0..n_blocks {
        let old_insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut new_insts = Vec::with_capacity(old_insts.len());
        for mut inst in old_insts {
            let mut prefix = vec![];
            if let Some((new_ty, operands)) = flattened_workgroup_leaf_rewrite(
                ctx,
                &types,
                &mut value_types,
                &value_defs,
                &roots,
                &desired_pointees,
                &inst,
                &mut prefix,
            ) {
                inst.result_type = Some(new_ty);
                inst.operands = operands;
            }
            for prefix_inst in &prefix {
                if let (Some(id), Some(ty)) = (prefix_inst.result_id, prefix_inst.result_type) {
                    value_types.insert(id, ty);
                }
                if let Some(id) = prefix_inst.result_id {
                    types.insert(id, prefix_inst.clone());
                    value_defs.insert(id, prefix_inst.clone());
                }
            }
            new_insts.extend(prefix);
            if let (Some(id), Some(ty)) = (inst.result_id, inst.result_type) {
                value_types.insert(id, ty);
            }
            if let Some(id) = inst.result_id {
                value_defs.insert(id, inst.clone());
            }
            new_insts.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = new_insts;
    }
}

pub(in crate::passes) fn flattened_workgroup_leaf_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    value_defs: &HashMap<Word, Instruction>,
    roots: &HashSet<Word>,
    desired_pointees: &HashMap<Word, Option<Word>>,
    inst: &Instruction,
    prefix: &mut Vec<Instruction>,
) -> Option<(Word, Vec<Operand>)> {
    if !matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
        return None;
    }
    let rid = inst.result_id?;
    let desired_pointee = desired_pointees.get(&rid).and_then(|ty| *ty)?;
    let result_ty = inst.result_type?;
    let current_pointee = pointer_pointee_including_new(ctx, types, result_ty)?;
    if current_pointee == desired_pointee {
        return None;
    }
    let [Operand::IdRef(base), Operand::IdRef(flat_index)] = inst.operands.as_slice() else {
        return None;
    };
    if !roots.contains(base) {
        return None;
    }
    let base_ty = value_types.get(base).copied()?;
    let base_pointee = pointer_pointee_including_new(ctx, types, base_ty)?;
    let indexed_pointee =
        type_after_spirv_access_operands(types, base_pointee, &[Operand::IdRef(*flat_index)])?;
    if indexed_pointee != current_pointee {
        return None;
    }
    let leaf_count = homogeneous_leaf_count(types, indexed_pointee, desired_pointee)?;
    if leaf_count <= 1 {
        return None;
    }
    let (record_index, leaf_index) = split_flat_workgroup_index(
        ctx,
        types,
        value_types,
        value_defs,
        indexed_pointee,
        *flat_index,
        leaf_count,
        prefix,
    )?;
    let ptr_ty = ctx.ty_ptr(StorageClass::Workgroup, desired_pointee);
    Some((
        ptr_ty,
        vec![
            Operand::IdRef(*base),
            Operand::IdRef(record_index),
            Operand::IdRef(leaf_index),
        ],
    ))
}

pub(in crate::passes) fn pointer_leaf_use_types(
    ctx: &Ctx,
    entry_idx: usize,
    value_types: &HashMap<Word, Word>,
) -> HashMap<Word, Option<Word>> {
    let mut desired = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            match inst.class.opcode {
                Op::Load => {
                    if let (Some(Operand::IdRef(ptr)), Some(result_ty)) =
                        (inst.operands.first(), inst.result_type)
                    {
                        record_pointer_leaf_use(&mut desired, *ptr, result_ty);
                    }
                }
                Op::Store => {
                    if let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(value))) =
                        (inst.operands.first(), inst.operands.get(1))
                    {
                        if let Some(value_ty) = value_types.get(value).copied() {
                            record_pointer_leaf_use(&mut desired, *ptr, value_ty);
                        }
                    }
                }
                op if atomic_i32_value_operands(op).is_some() => {
                    if let Some(Operand::IdRef(ptr)) = inst.operands.first() {
                        if op == Op::AtomicStore {
                            if let Some(Operand::IdRef(value)) = inst.operands.get(3) {
                                if let Some(value_ty) = value_types.get(value).copied() {
                                    record_pointer_leaf_use(&mut desired, *ptr, value_ty);
                                }
                            }
                        } else if let Some(result_ty) = inst.result_type {
                            record_pointer_leaf_use(&mut desired, *ptr, result_ty);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    desired
}

pub(in crate::passes) fn record_pointer_leaf_use(
    desired: &mut HashMap<Word, Option<Word>>,
    ptr: Word,
    pointee: Word,
) {
    desired
        .entry(ptr)
        .and_modify(|existing| {
            if *existing != Some(pointee) {
                *existing = None;
            }
        })
        .or_insert(Some(pointee));
}

pub(in crate::passes) fn homogeneous_leaf_count(
    types: &HashMap<Word, Instruction>,
    ty: Word,
    desired_leaf: Word,
) -> Option<u32> {
    if ty == desired_leaf {
        return Some(1);
    }
    let def = types.get(&ty)?;
    match def.class.opcode {
        Op::TypeStruct => {
            let mut count = 0u32;
            for operand in &def.operands {
                let Operand::IdRef(member_ty) = operand else {
                    return None;
                };
                count =
                    count.checked_add(homogeneous_leaf_count(types, *member_ty, desired_leaf)?)?;
            }
            (count > 0).then_some(count)
        }
        Op::TypeArray => {
            let [Operand::IdRef(elem_ty), Operand::IdRef(len_id)] = def.operands.as_slice() else {
                return None;
            };
            let elem_count = homogeneous_leaf_count(types, *elem_ty, desired_leaf)?;
            const_u32(types, *len_id)?.checked_mul(elem_count)
        }
        _ => None,
    }
}

pub(in crate::passes) fn split_flat_workgroup_index(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    value_defs: &HashMap<Word, Instruction>,
    indexed_pointee: Word,
    flat_index: Word,
    leaf_count: u32,
    prefix: &mut Vec<Instruction>,
) -> Option<(Word, Word)> {
    if let Some(index) = const_u32(types, flat_index) {
        return Some((
            ctx.const_uint(index / leaf_count),
            ctx.const_uint(index % leaf_count),
        ));
    }

    if types
        .get(&indexed_pointee)
        .is_some_and(|def| def.class.opcode == Op::TypeStruct)
    {
        let (record, member) =
            decompose_flattened_struct_index(types, value_defs, flat_index, leaf_count)?;
        let record = coerce_workgroup_index_to_uint(ctx, types, value_types, record, prefix)?;
        return Some((record, ctx.const_uint(member)));
    }

    let uint = ctx.ty_uint();
    let index = coerce_workgroup_index_to_uint(ctx, types, value_types, flat_index, prefix)?;
    let divisor = ctx.const_uint(leaf_count);
    let record_index = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::UDiv,
        Some(uint),
        Some(record_index),
        vec![Operand::IdRef(index), Operand::IdRef(divisor)],
    ));
    value_types.insert(record_index, uint);
    let leaf_index = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(leaf_index),
        vec![Operand::IdRef(index), Operand::IdRef(divisor)],
    ));
    value_types.insert(leaf_index, uint);
    Some((record_index, leaf_index))
}

pub(in crate::passes) fn coerce_workgroup_index_to_uint(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    index: Word,
    prefix: &mut Vec<Instruction>,
) -> Option<Word> {
    let uint = ctx.ty_uint();
    let index_ty = value_types.get(&index).copied()?;
    if is_uint_type(types, index_ty) {
        return Some(index);
    }
    if !is_integer_type_with_width(types, index_ty, 64) {
        return None;
    }
    let converted = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(converted),
        vec![Operand::IdRef(index)],
    ));
    value_types.insert(converted, uint);
    Some(converted)
}

pub(in crate::passes) fn decompose_flattened_struct_index(
    types: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    flat_index: Word,
    leaf_count: u32,
) -> Option<(Word, u32)> {
    if let Some(record) = scaled_record_index(types, value_defs, flat_index, leaf_count) {
        return Some((record, 0));
    }

    let inst = value_defs.get(&flat_index)?;
    if inst.class.opcode != Op::IAdd {
        return None;
    }
    let [Operand::IdRef(a), Operand::IdRef(b)] = inst.operands.as_slice() else {
        return None;
    };
    if let Some(member) = const_u32(types, *a).filter(|member| *member < leaf_count) {
        let record = scaled_record_index(types, value_defs, *b, leaf_count)?;
        return Some((record, member));
    }
    let member = const_u32(types, *b).filter(|member| *member < leaf_count)?;
    let record = scaled_record_index(types, value_defs, *a, leaf_count)?;
    Some((record, member))
}

pub(in crate::passes) fn scaled_record_index(
    types: &HashMap<Word, Instruction>,
    value_defs: &HashMap<Word, Instruction>,
    scaled_index: Word,
    leaf_count: u32,
) -> Option<Word> {
    let inst = value_defs.get(&scaled_index)?;
    if inst.class.opcode != Op::IMul {
        return None;
    }
    let [Operand::IdRef(a), Operand::IdRef(b)] = inst.operands.as_slice() else {
        return None;
    };
    if const_u32(types, *a) == Some(leaf_count) {
        return Some(*b);
    }
    if const_u32(types, *b) == Some(leaf_count) {
        return Some(*a);
    }
    None
}

pub(in crate::passes) fn rewrite_structural_result_types(
    ctx: &mut Ctx,
    entry_idx: usize,
    defs: &HashMap<Word, Instruction>,
) {
    let types = combined_type_defs(ctx, defs);
    let mut value_types = combined_value_types(ctx, entry_idx);
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let n_inst = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .len();
        for ii in 0..n_inst {
            let replacement = {
                let inst = &ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                structural_result_replacement(ctx, &types, &value_types, inst)
            };
            if let Some(replacement) = replacement {
                let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
                inst.result_type = Some(replacement);
                if let Some(id) = inst.result_id {
                    value_types.insert(id, replacement);
                }
            }
        }
    }
}

pub(in crate::passes) fn structural_result_replacement(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    inst: &Instruction,
) -> Option<Word> {
    let result_ty = inst.result_type?;
    let replacement = match inst.class.opcode {
        Op::Load => {
            let ptr = inst.operands.first().and_then(|operand| match operand {
                Operand::IdRef(ptr) => Some(*ptr),
                _ => None,
            })?;
            let ptr_ty = value_types.get(&ptr).copied()?;
            pointer_pointee_including_new(ctx, types, ptr_ty)?
        }
        Op::CompositeExtract => {
            let Operand::IdRef(composite) = inst.operands.first()? else {
                return None;
            };
            let mut selected = value_types.get(composite).copied()?;
            for index in inst.operands.iter().skip(1) {
                let Operand::LiteralBit32(index) = index else {
                    return None;
                };
                let definition = type_def_including_new(ctx, types, selected)?;
                selected = match definition.class.opcode {
                    Op::TypeStruct => match definition.operands.get(*index as usize)? {
                        Operand::IdRef(member) => *member,
                        _ => return None,
                    },
                    Op::TypeArray | Op::TypeVector | Op::TypeMatrix => {
                        match definition.operands.first()? {
                            Operand::IdRef(element) => *element,
                            _ => return None,
                        }
                    }
                    _ => return None,
                };
            }
            selected
        }
        _ => return None,
    };
    (result_ty != replacement && types_structurally_match(ctx, types, result_ty, replacement))
        .then_some(replacement)
}

pub(in crate::passes) fn pointer_pointee_including_new(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    ptr_ty: Word,
) -> Option<Word> {
    ptr_pointee(defs, ptr_ty).or_else(|| {
        ctx.new_globals.iter().find_map(|g| {
            if g.result_id == Some(ptr_ty) && g.class.opcode == Op::TypePointer {
                if let Operand::IdRef(p) = g.operands[1] {
                    return Some(p);
                }
            }
            None
        })
    })
}

pub(in crate::passes) fn canonical_rooted_access_pointee(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    roots: &HashSet<Word>,
    op: Op,
    base: Option<Word>,
    old_pointee: Word,
) -> Word {
    if !matches!(
        op,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        return old_pointee;
    }
    let Some(base) = base.filter(|base| roots.contains(base)) else {
        return old_pointee;
    };
    let Some(base_ty) = value_types.get(&base).copied() else {
        return old_pointee;
    };
    let Some(base_pointee) = pointer_pointee_including_new(ctx, defs, base_ty) else {
        return old_pointee;
    };
    if types_structurally_match(ctx, defs, base_pointee, old_pointee) {
        base_pointee
    } else {
        old_pointee
    }
}

pub(in crate::passes) fn types_structurally_match(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    a: Word,
    b: Word,
) -> bool {
    fn inner(
        ctx: &Ctx,
        defs: &HashMap<Word, Instruction>,
        a: Word,
        b: Word,
        seen: &mut HashSet<(Word, Word)>,
    ) -> bool {
        if a == b {
            return true;
        }
        if !seen.insert((a, b)) {
            return true;
        }
        let Some(a_def) = type_def_including_new(ctx, defs, a) else {
            return false;
        };
        let Some(b_def) = type_def_including_new(ctx, defs, b) else {
            return false;
        };
        if a_def.class.opcode != b_def.class.opcode {
            return false;
        }
        match a_def.class.opcode {
            Op::TypeStruct => {
                a_def.operands.len() == b_def.operands.len()
                    && a_def
                        .operands
                        .iter()
                        .zip(&b_def.operands)
                        .all(|(a_op, b_op)| {
                            let (Operand::IdRef(a_member), Operand::IdRef(b_member)) = (a_op, b_op)
                            else {
                                return a_op == b_op;
                            };
                            inner(ctx, defs, *a_member, *b_member, seen)
                        })
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                if a_def.operands.len() != b_def.operands.len() {
                    return false;
                }
                let Some((a_first, a_rest)) = a_def.operands.split_first() else {
                    return true;
                };
                let Some((b_first, b_rest)) = b_def.operands.split_first() else {
                    return false;
                };
                let (Operand::IdRef(a_elem), Operand::IdRef(b_elem)) = (a_first, b_first) else {
                    return a_def.operands == b_def.operands;
                };
                inner(ctx, defs, *a_elem, *b_elem, seen) && a_rest == b_rest
            }
            Op::TypePointer => {
                if a_def.operands.len() != 2 || b_def.operands.len() != 2 {
                    return false;
                }
                if a_def.operands[0] != b_def.operands[0] {
                    return false;
                }
                let (Operand::IdRef(a_pointee), Operand::IdRef(b_pointee)) =
                    (&a_def.operands[1], &b_def.operands[1])
                else {
                    return false;
                };
                inner(ctx, defs, *a_pointee, *b_pointee, seen)
            }
            _ => a_def.operands == b_def.operands,
        }
    }

    inner(ctx, defs, a, b, &mut HashSet::default())
}

pub(in crate::passes) fn type_def_including_new(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    ty: Word,
) -> Option<Instruction> {
    defs.get(&ty).cloned().or_else(|| {
        ctx.new_globals
            .iter()
            .find(|g| g.result_id == Some(ty))
            .cloned()
    })
}
