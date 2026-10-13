use super::*;

pub(in crate::passes) fn trace_to_array_element_zero(
    ctx: &Ctx,
    func: &Function,
    base: Word,
    elem_scalar: Word,
    sc: StorageClass,
    ptr_info: &HashMap<Word, (StorageClass, Word)>,
) -> Option<Word> {
    let mut visited: HashSet<Word> = HashSet::default();
    let mut stack = vec![base];
    let mut found: Option<Word> = None;
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let def = find_def_in_func(func, id)?;
        match def.class.opcode {
            Op::Phi => {
                let mut k = 0;
                while k < def.operands.len() {
                    if let Some(Operand::IdRef(v)) = def.operands.get(k) {
                        stack.push(*v);
                    }
                    k += 2;
                }
            }
            Op::CopyObject => {
                let Some(Operand::IdRef(v)) = def.operands.first() else {
                    return None;
                };
                stack.push(*v);
            }
            Op::InBoundsAccessChain | Op::AccessChain => {
                if def.operands.len() != 2 {
                    return None;
                }
                let Some(Operand::IdRef(arr)) = def.operands.first() else {
                    return None;
                };
                let Some(Operand::IdRef(idx)) = def.operands.get(1) else {
                    return None;
                };
                if const_u32(ctx, *idx) != Some(0) {
                    return None;
                }
                let arr_ptr_ty = value_result_type(ctx, *arr)?;
                let &(arr_sc, arr_pointee) = ptr_info.get(&arr_ptr_ty)?;
                if arr_sc != sc || array_element_type(ctx, arr_pointee) != Some(elem_scalar) {
                    return None;
                }
                match found {
                    Some(prev) if prev != *arr => return None,
                    _ => found = Some(*arr),
                }
            }
            _ => return None,
        }
    }
    found
}

pub(in crate::passes) fn remap_word_index_to_struct_member(ctx: &mut Ctx, entry_idx: usize) {
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

    let mut edits: Vec<(usize, usize, u32)> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            let Some(&(_, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = inst.operands[1..].to_vec();
            if indices.is_empty() {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let (reached, consumed) = walk_into_type_partial(ctx, base_pointee, &indices);
            if consumed == indices.len() && reached == result_pointee {
                continue;
            }
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
            let Some(Operand::IdRef(last_id)) = indices.last() else {
                continue;
            };
            let Some(word) = const_u32(ctx, *last_id) else {
                continue;
            };
            let Some(byte) = word.checked_mul(4) else {
                continue;
            };
            let mut found: Option<u32> = None;
            for m in 0..sdef.operands.len() {
                if member_offset.get(&(struct_ty, m as u32)).copied() != Some(byte) {
                    continue;
                }
                let Some(Operand::IdRef(mty)) = sdef.operands.get(m) else {
                    continue;
                };
                if *mty != result_pointee {
                    continue;
                }
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some(m as u32);
            }
            let Some(member) = found else {
                continue;
            };
            if member == word {
                continue;
            }
            edits.push((bi, ii, member));
        }
    }

    for (bi, ii, member) in edits {
        let member_id = ctx.const_uint(member);
        if let Some(last) = ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
            .operands
            .last_mut()
        {
            *last = Operand::IdRef(member_id);
        }
    }
}

pub(in crate::passes) fn remap_overflow_word_index_to_outer_member(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
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
    let mut array_stride: HashMap<Word, u32> = HashMap::default();
    for inst in &ctx.module.annotations {
        match inst.class.opcode {
            Op::MemberDecorate => {
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
            Op::Decorate => {
                if let (
                    Some(Operand::IdRef(ty)),
                    Some(Operand::Decoration(Decoration::ArrayStride)),
                    Some(Operand::LiteralBit32(stride)),
                ) = (
                    inst.operands.first(),
                    inst.operands.get(1),
                    inst.operands.get(2),
                ) {
                    array_stride.insert(*ty, *stride);
                }
            }
            _ => {}
        }
    }

    let prefix_byte = |start: Word, prefix: &[Operand]| -> Option<u32> {
        let mut cur = start;
        let mut byte: u32 = 0;
        for op in prefix {
            let Operand::IdRef(idx_id) = op else {
                return None;
            };
            let idx = const_u32(ctx, *idx_id)?;
            let def = type_def_of(ctx, cur)?;
            match def.class.opcode {
                Op::TypeStruct => {
                    byte = byte.checked_add(*member_offset.get(&(cur, idx))?)?;
                    cur = match def.operands.get(idx as usize) {
                        Some(Operand::IdRef(m)) => *m,
                        _ => return None,
                    };
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    let stride = *array_stride.get(&cur)?;
                    byte = byte.checked_add(stride.checked_mul(idx)?)?;
                    cur = match def.operands.first() {
                        Some(Operand::IdRef(elem)) => *elem,
                        _ => return None,
                    };
                }
                _ => return None,
            }
        }
        Some(byte)
    };

    let mut edits: Vec<(usize, usize, u32)> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            let Some(&(_, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = inst.operands[1..].to_vec();
            if indices.len() < 2 {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some(bdef) = type_def_of(ctx, base_pointee) else {
                continue;
            };
            if bdef.class.opcode != Op::TypeStruct {
                continue;
            }
            let (reached, consumed) = walk_into_type_partial(ctx, base_pointee, &indices);
            if consumed == indices.len() && reached == result_pointee {
                continue;
            }
            let prefix = &indices[..indices.len() - 1];
            let Some(pbyte) = prefix_byte(base_pointee, prefix) else {
                continue;
            };
            let Some(Operand::IdRef(last_id)) = indices.last() else {
                continue;
            };
            let Some(word) = const_u32(ctx, *last_id) else {
                continue;
            };
            let Some(abs_byte) = word.checked_mul(4).and_then(|b| b.checked_add(pbyte)) else {
                continue;
            };
            let mut found: Option<u32> = None;
            for m in 0..bdef.operands.len() {
                if member_offset.get(&(base_pointee, m as u32)).copied() != Some(abs_byte) {
                    continue;
                }
                let Some(Operand::IdRef(mty)) = bdef.operands.get(m) else {
                    continue;
                };
                if *mty != result_pointee {
                    continue;
                }
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some(m as u32);
            }
            let Some(member) = found else {
                continue;
            };
            edits.push((bi, ii, member));
        }
    }

    for (bi, ii, member) in edits {
        let member_id = ctx.const_uint(member);
        let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
        inst.operands.truncate(1);
        inst.operands.push(Operand::IdRef(member_id));
    }
}

pub(in crate::passes) fn remap_dynamic_word_index_to_array_member(ctx: &mut Ctx, entry_idx: usize) {
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
    let mut array_stride: HashMap<Word, u32> = HashMap::default();
    for inst in &ctx.module.annotations {
        match inst.class.opcode {
            Op::MemberDecorate => {
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
            Op::Decorate => {
                if let (
                    Some(Operand::IdRef(ty)),
                    Some(Operand::Decoration(Decoration::ArrayStride)),
                    Some(Operand::LiteralBit32(stride)),
                ) = (
                    inst.operands.first(),
                    inst.operands.get(1),
                    inst.operands.get(2),
                ) {
                    array_stride.insert(*ty, *stride);
                }
            }
            _ => {}
        }
    }

    let is_word_scalar = |ctx: &Ctx, ty: Word| -> bool {
        match type_def_of(ctx, ty) {
            Some(def) => match def.class.opcode {
                Op::TypeInt | Op::TypeFloat => {
                    matches!(def.operands.first(), Some(Operand::LiteralBit32(32)))
                }
                _ => false,
            },
            None => false,
        }
    };

    let mut value_def: HashMap<Word, (Op, Vec<Operand>)> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if let Some(id) = inst.result_id {
                value_def.insert(id, (inst.class.opcode, inst.operands.clone()));
            }
        }
    }
    let split_const_plus_dyn = |ctx: &Ctx, id: Word| -> Option<(u32, Word)> {
        let (op, ops) = value_def.get(&id)?;
        if *op != Op::IAdd {
            return None;
        }
        let (Operand::IdRef(a), Operand::IdRef(b)) = (ops.first()?, ops.get(1)?) else {
            return None;
        };
        match (const_u32(ctx, *a), const_u32(ctx, *b)) {
            (Some(w), None) => Some((w, *b)),
            (None, Some(w)) => Some((w, *a)),
            _ => None,
        }
    };

    let prefix_byte = |start: Word, prefix: &[Operand]| -> Option<u32> {
        let mut cur = start;
        let mut byte: u32 = 0;
        for op in prefix {
            let Operand::IdRef(idx_id) = op else {
                return None;
            };
            let idx = const_u32(ctx, *idx_id)?;
            let def = type_def_of(ctx, cur)?;
            match def.class.opcode {
                Op::TypeStruct => {
                    byte = byte.checked_add(*member_offset.get(&(cur, idx))?)?;
                    cur = match def.operands.get(idx as usize) {
                        Some(Operand::IdRef(m)) => *m,
                        _ => return None,
                    };
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    let stride = *array_stride.get(&cur)?;
                    byte = byte.checked_add(stride.checked_mul(idx)?)?;
                    cur = match def.operands.first() {
                        Some(Operand::IdRef(elem)) => *elem,
                        _ => return None,
                    };
                }
                _ => return None,
            }
        }
        Some(byte)
    };

    let mut edits: Vec<(usize, usize, u32, Word)> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            let Some(&(_, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            if !is_word_scalar(ctx, result_pointee) {
                continue;
            }
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = inst.operands[1..].to_vec();
            if indices.len() < 2 {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some(bdef) = type_def_of(ctx, base_pointee) else {
                continue;
            };
            if bdef.class.opcode != Op::TypeStruct {
                continue;
            }
            let (reached, consumed) = walk_into_type_partial(ctx, base_pointee, &indices);
            if consumed == indices.len() && reached == result_pointee {
                continue;
            }
            let prefix = &indices[..indices.len() - 1];
            let Some(pbyte) = prefix_byte(base_pointee, prefix) else {
                continue;
            };
            let Some(Operand::IdRef(last_id)) = indices.last() else {
                continue;
            };
            let Some((word, dyn_id)) = split_const_plus_dyn(ctx, *last_id) else {
                continue;
            };
            let Some(abs_byte) = word.checked_mul(4).and_then(|b| b.checked_add(pbyte)) else {
                continue;
            };
            let mut found: Option<u32> = None;
            for m in 0..bdef.operands.len() {
                if member_offset.get(&(base_pointee, m as u32)).copied() != Some(abs_byte) {
                    continue;
                }
                let Some(Operand::IdRef(mty)) = bdef.operands.get(m) else {
                    continue;
                };
                let Some(mdef) = type_def_of(ctx, *mty) else {
                    continue;
                };
                if mdef.class.opcode != Op::TypeArray {
                    continue;
                }
                if array_stride.get(mty).copied() != Some(4) {
                    continue;
                }
                let Some(Operand::IdRef(elem)) = mdef.operands.first() else {
                    continue;
                };
                if *elem != result_pointee {
                    continue;
                }
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some(m as u32);
            }
            let Some(member) = found else {
                continue;
            };
            edits.push((bi, ii, member, dyn_id));
        }
    }

    for (bi, ii, member, dyn_id) in edits {
        let member_id = ctx.const_uint(member);
        let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
        inst.operands.truncate(1);
        inst.operands.push(Operand::IdRef(member_id));
        inst.operands.push(Operand::IdRef(dyn_id));
    }
}

struct ExactWordTarget {
    path: Vec<u32>,
    ty: Word,
}

fn exact_word_path(
    ctx: &Ctx,
    member_offset: &HashMap<(Word, u32), u32>,
    array_stride: &HashMap<Word, u32>,
    ty: Word,
    byte_offset: u32,
) -> Option<ExactWordTarget> {
    let definition = type_def_of(ctx, ty)?;
    match definition.class.opcode {
        Op::TypeInt | Op::TypeFloat
            if byte_offset == 0
                && matches!(definition.operands.first(), Some(Operand::LiteralBit32(32))) =>
        {
            Some(ExactWordTarget {
                path: Vec::new(),
                ty,
            })
        }
        Op::TypeStruct => (0..definition.operands.len()).rev().find_map(|member| {
            let offset = member_offset.get(&(ty, member as u32)).copied()?;
            let relative = byte_offset.checked_sub(offset)?;
            let Operand::IdRef(member_ty) = definition.operands[member] else {
                return None;
            };
            let mut target =
                exact_word_path(ctx, member_offset, array_stride, member_ty, relative)?;
            target.path.insert(0, member as u32);
            Some(target)
        }),
        Op::TypeArray => {
            let (Some(Operand::IdRef(element)), Some(Operand::IdRef(length))) =
                (definition.operands.first(), definition.operands.get(1))
            else {
                return None;
            };
            let length = const_u32(ctx, *length)?;
            let stride = array_stride.get(&ty).copied()?;
            let index = byte_offset / stride;
            if index >= length {
                return None;
            }
            let mut target = exact_word_path(
                ctx,
                member_offset,
                array_stride,
                *element,
                byte_offset % stride,
            )?;
            target.path.insert(0, index);
            Some(target)
        }
        Op::TypeVector => {
            let (Some(Operand::IdRef(element)), Some(Operand::LiteralBit32(lanes))) =
                (definition.operands.first(), definition.operands.get(1))
            else {
                return None;
            };
            let element_definition = type_def_of(ctx, *element)?;
            let Some(Operand::LiteralBit32(width)) = element_definition.operands.first() else {
                return None;
            };
            let stride = width.checked_div(8)?;
            let index = byte_offset / stride;
            if index >= *lanes {
                return None;
            }
            let mut target = exact_word_path(
                ctx,
                member_offset,
                array_stride,
                *element,
                byte_offset % stride,
            )?;
            target.path.insert(0, index);
            Some(target)
        }
        _ => None,
    }
}

pub(in crate::passes) fn remap_dynamic_word_index_to_array_struct_field(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
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
    let mut array_stride: HashMap<Word, u32> = HashMap::default();
    for inst in &ctx.module.annotations {
        match inst.class.opcode {
            Op::MemberDecorate => {
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
            Op::Decorate => {
                if let (
                    Some(Operand::IdRef(ty)),
                    Some(Operand::Decoration(Decoration::ArrayStride)),
                    Some(Operand::LiteralBit32(stride)),
                ) = (
                    inst.operands.first(),
                    inst.operands.get(1),
                    inst.operands.get(2),
                ) {
                    array_stride.insert(*ty, *stride);
                }
            }
            _ => {}
        }
    }

    let is_word_scalar = |ctx: &Ctx, ty: Word| -> bool {
        match type_def_of(ctx, ty) {
            Some(def) => match def.class.opcode {
                Op::TypeInt | Op::TypeFloat => {
                    matches!(def.operands.first(), Some(Operand::LiteralBit32(32)))
                }
                _ => false,
            },
            None => false,
        }
    };

    let mut value_def: HashMap<Word, (Op, Vec<Operand>)> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if let Some(id) = inst.result_id {
                value_def.insert(id, (inst.class.opcode, inst.operands.clone()));
            }
        }
    }
    let split_const_plus_other = |ctx: &Ctx, id: Word| -> Option<(u32, Word)> {
        let (op, ops) = value_def.get(&id)?;
        if *op != Op::IAdd {
            return None;
        }
        let (Operand::IdRef(a), Operand::IdRef(b)) = (ops.first()?, ops.get(1)?) else {
            return None;
        };
        match (const_u32(ctx, *a), const_u32(ctx, *b)) {
            (Some(w), None) => Some((w, *b)),
            (None, Some(w)) => Some((w, *a)),
            _ => None,
        }
    };
    let split_dyn_times_const = |ctx: &Ctx, id: Word| -> Option<(Word, u32)> {
        let (op, ops) = value_def.get(&id)?;
        if *op != Op::IMul {
            return None;
        }
        let (Operand::IdRef(a), Operand::IdRef(b)) = (ops.first()?, ops.get(1)?) else {
            return None;
        };
        match (const_u32(ctx, *a), const_u32(ctx, *b)) {
            (Some(s), None) => Some((*b, s)),
            (None, Some(s)) => Some((*a, s)),
            _ => None,
        }
    };

    let prefix_byte = |start: Word, prefix: &[Operand]| -> Option<u32> {
        let mut cur = start;
        let mut byte: u32 = 0;
        for op in prefix {
            let Operand::IdRef(idx_id) = op else {
                return None;
            };
            let idx = const_u32(ctx, *idx_id)?;
            let def = type_def_of(ctx, cur)?;
            match def.class.opcode {
                Op::TypeStruct => {
                    byte = byte.checked_add(*member_offset.get(&(cur, idx))?)?;
                    cur = match def.operands.get(idx as usize) {
                        Some(Operand::IdRef(m)) => *m,
                        _ => return None,
                    };
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    let stride = *array_stride.get(&cur)?;
                    byte = byte.checked_add(stride.checked_mul(idx)?)?;
                    cur = match def.operands.first() {
                        Some(Operand::IdRef(elem)) => *elem,
                        _ => return None,
                    };
                }
                _ => return None,
            }
        }
        Some(byte)
    };

    let mut uses: HashMap<Word, Vec<(usize, usize, Op)>> = HashMap::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            for op in &inst.operands {
                if let Operand::IdRef(r) = op {
                    uses.entry(*r)
                        .or_default()
                        .push((bi, ii, inst.class.opcode));
                }
            }
        }
    }

    struct ChainEdit {
        bi: usize,
        ii: usize,
        cid: Word,
        member: u32,
        elem: Word,
        elem_bias: u32,
        field_path: Vec<u32>,
        field_ty: Word,
        storage: StorageClass,
    }
    let mut edits: Vec<ChainEdit> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            let Some(&(storage, result_pointee)) = ptr_info.get(&result_type) else {
                continue;
            };
            if !is_word_scalar(ctx, result_pointee) {
                continue;
            }
            let Some(cid) = inst.result_id else {
                continue;
            };
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let indices: Vec<Operand> = inst.operands[1..].to_vec();
            if indices.len() < 2 {
                continue;
            }
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let Some(bdef) = type_def_of(ctx, base_pointee) else {
                continue;
            };
            if bdef.class.opcode != Op::TypeStruct {
                continue;
            }
            let (reached, consumed) = walk_into_type_partial(ctx, base_pointee, &indices);
            if consumed == indices.len() && reached == result_pointee {
                continue;
            }
            let prefix = &indices[..indices.len() - 1];
            let Some(pbyte) = prefix_byte(base_pointee, prefix) else {
                continue;
            };
            let Some(Operand::IdRef(last_id)) = indices.last() else {
                continue;
            };
            let Some((word, mul_id)) = split_const_plus_other(ctx, *last_id) else {
                continue;
            };
            let Some((elem_dyn, stride_words)) = split_dyn_times_const(ctx, mul_id) else {
                continue;
            };
            if stride_words == 0 {
                continue;
            }
            let Some(abs_byte) = word.checked_mul(4).and_then(|b| b.checked_add(pbyte)) else {
                continue;
            };
            let Some(elem_stride_bytes) = stride_words.checked_mul(4) else {
                continue;
            };
            let mut found: Option<(u32, u32, u32, Word)> = None;
            for m in 0..bdef.operands.len() {
                let Some(&off) = member_offset.get(&(base_pointee, m as u32)) else {
                    continue;
                };
                if abs_byte < off {
                    continue;
                }
                let Some(Operand::IdRef(mty)) = bdef.operands.get(m) else {
                    continue;
                };
                let Some(mdef) = type_def_of(ctx, *mty) else {
                    continue;
                };
                if mdef.class.opcode != Op::TypeArray {
                    continue;
                }
                if array_stride.get(mty).copied() != Some(elem_stride_bytes) {
                    continue;
                }
                let (Some(Operand::IdRef(elem_ty)), Some(Operand::IdRef(length_id))) =
                    (mdef.operands.first(), mdef.operands.get(1))
                else {
                    continue;
                };
                let Some(length) = const_u32(ctx, *length_id) else {
                    continue;
                };
                let relative = abs_byte - off;
                let elem_bias = relative / elem_stride_bytes;
                if elem_bias >= length {
                    continue;
                }
                let Some(edef) = type_def_of(ctx, *elem_ty) else {
                    continue;
                };
                if edef.class.opcode != Op::TypeStruct {
                    continue;
                }
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some((m as u32, elem_bias, relative % elem_stride_bytes, *elem_ty));
            }
            let Some((member, elem_bias, field_byte, elem_ty)) = found else {
                continue;
            };
            let Some(target) =
                exact_word_path(ctx, &member_offset, &array_stride, elem_ty, field_byte)
            else {
                continue;
            };
            let all_loads = uses
                .get(&cid)
                .map(|u| u.iter().all(|&(_, _, op)| op == Op::Load))
                .unwrap_or(false);
            if !all_loads {
                continue;
            }
            edits.push(ChainEdit {
                bi,
                ii,
                cid,
                member,
                elem: elem_dyn,
                elem_bias,
                field_path: target.path,
                field_ty: target.ty,
                storage,
            });
        }
    }

    if edits.is_empty() {
        return;
    }

    let cid_field: HashMap<Word, Word> = edits.iter().map(|e| (e.cid, e.field_ty)).collect();
    let mut load_splits: HashMap<(usize, usize), (Word, Word)> = HashMap::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Load {
                continue;
            }
            let Some(Operand::IdRef(ptr)) = inst.operands.first() else {
                continue;
            };
            let Some(&field_ty) = cid_field.get(ptr) else {
                continue;
            };
            let Some(orig_ty) = inst.result_type else {
                continue;
            };
            if orig_ty != field_ty {
                load_splits.insert((bi, ii), (field_ty, orig_ty));
            }
        }
    }

    for e in &edits {
        let field_ptr_ty = ctx.ty_ptr(e.storage, e.field_ty);
        let member_id = ctx.const_uint(e.member);
        let field_ids = e
            .field_path
            .iter()
            .map(|field| ctx.const_uint(*field))
            .collect::<Vec<_>>();
        let base = match ctx.module.functions[entry_idx].blocks[e.bi].instructions[e.ii]
            .operands
            .first()
        {
            Some(Operand::IdRef(b)) => *b,
            _ => continue,
        };
        let inst = &mut ctx.module.functions[entry_idx].blocks[e.bi].instructions[e.ii];
        inst.result_type = Some(field_ptr_ty);
        inst.operands = vec![
            Operand::IdRef(base),
            Operand::IdRef(member_id),
            Operand::IdRef(e.elem),
        ];
        inst.operands
            .extend(field_ids.into_iter().map(Operand::IdRef));
    }

    let mut by_block: HashMap<usize, Vec<usize>> = HashMap::default();
    for &(bi, ii) in load_splits.keys() {
        by_block.entry(bi).or_default().push(ii);
    }
    for (bi, mut iis) in by_block {
        iis.sort_unstable();
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out: Vec<Instruction> = Vec::with_capacity(insts.len() + iis.len());
        let mut next = 0usize;
        for (ii, inst) in insts.into_iter().enumerate() {
            if next < iis.len() && iis[next] == ii {
                next += 1;
                let (field_ty, orig_ty) = load_splits[&(bi, ii)];
                let ptr = match inst.operands.first() {
                    Some(Operand::IdRef(p)) => *p,
                    _ => {
                        out.push(inst);
                        continue;
                    }
                };
                let mem_operand = inst.operands.get(1).cloned();
                let load_id = ctx.module.fresh_id();
                let mut load_ops = vec![Operand::IdRef(ptr)];
                if let Some(m) = mem_operand {
                    load_ops.push(m);
                }
                out.push(Instruction::new(
                    Op::Load,
                    Some(field_ty),
                    Some(load_id),
                    load_ops,
                ));
                out.push(Instruction::new(
                    Op::Bitcast,
                    Some(orig_ty),
                    inst.result_id,
                    vec![Operand::IdRef(load_id)],
                ));
            } else {
                out.push(inst);
            }
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }

    for e in edits.iter().filter(|edit| edit.elem_bias != 0) {
        let Some(elem_ty) = value_result_type(ctx, e.elem) else {
            continue;
        };
        let bias = ctx.const_int_of(elem_ty, i64::from(e.elem_bias));
        let biased_elem = ctx.module.fresh_id();
        let Some((block_index, instruction_index)) = ctx.module.functions[entry_idx]
            .blocks
            .iter()
            .enumerate()
            .find_map(|(block_index, block)| {
                block
                    .instructions
                    .iter()
                    .position(|instruction| instruction.result_id == Some(e.cid))
                    .map(|instruction_index| (block_index, instruction_index))
            })
        else {
            continue;
        };
        let block = &mut ctx.module.functions[entry_idx].blocks[block_index];
        block.instructions.insert(
            instruction_index,
            Instruction::new(
                Op::IAdd,
                Some(elem_ty),
                Some(biased_elem),
                vec![Operand::IdRef(e.elem), Operand::IdRef(bias)],
            ),
        );
        if let Some(Operand::IdRef(element)) = block.instructions[instruction_index + 1]
            .operands
            .get_mut(2)
        {
            *element = biased_elem;
        }
    }
}
