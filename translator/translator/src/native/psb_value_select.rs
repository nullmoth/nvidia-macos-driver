use crate::fxhash::{HashMap, HashSet};
use super::cfg::{graph::spirv_block_successors_by_label, EmittedDominators};
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Decoration, Op, StorageClass, Word};

const MAX_VALUE_STORE_REPLAY_NODES: usize = 4096;

fn trace_root(
    id: Word,
    value_def: &HashMap<Word, Instruction>,
    var_storage: &HashMap<Word, StorageClass>,
) -> Option<Word> {
    if var_storage.contains_key(&id) {
        return Some(id);
    }
    let def = value_def.get(&id)?;
    match def.class.opcode {
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::CopyObject => {
            let Operand::IdRef(base) = def.operands.first()? else {
                return None;
            };
            trace_root(*base, value_def, var_storage)
        }
        _ => None,
    }
}

struct Discovery {
    type_defs: HashMap<Word, Instruction>,
    var_storage: HashMap<Word, StorageClass>,
    value_def: HashMap<Word, Instruction>,
    value_type: HashMap<Word, Word>,
    closure: HashSet<Word>,
}

fn discover_value_select(module: &Module) -> Option<Discovery> {
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    let var_storage: HashMap<Word, StorageClass> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Variable)
        .filter_map(|i| {
            let id = i.result_id?;
            match i.operands.first()? {
                Operand::StorageClass(s) => Some((id, *s)),
                _ => None,
            }
        })
        .collect();

    let ptr_info = |ty: Word| -> Option<(StorageClass, Word)> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypePointer {
            return None;
        }
        match (inst.operands.first()?, inst.operands.get(1)?) {
            (Operand::StorageClass(s), Operand::IdRef(pointee)) => Some((*s, *pointee)),
            _ => None,
        }
    };
    let is_buffer_var =
        |id: Word| -> bool { var_storage.get(&id) == Some(&StorageClass::StorageBuffer) };

    let mut value_def: HashMap<Word, Instruction> = HashMap::default();
    let mut value_type: HashMap<Word, Word> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if let Some(rid) = inst.result_id {
                    if let Some(rty) = inst.result_type {
                        if ptr_info(rty)
                            .is_some_and(|(storage, _)| storage == StorageClass::StorageBuffer)
                        {
                            value_type.insert(rid, rty);
                            value_def.insert(rid, inst.clone());
                        }
                    }
                }
            }
        }
    }

    let is_sb_pointer = |id: Word| -> bool {
        value_type
            .get(&id)
            .and_then(|t| ptr_info(*t))
            .map(|(sc, _)| sc == StorageClass::StorageBuffer)
            .unwrap_or(false)
    };

    let pointer_operands = |inst: &Instruction| -> Vec<Word> {
        match inst.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => inst
                .operands
                .first()
                .and_then(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .into_iter()
                .collect(),
            Op::Select => inst.operands[1..]
                .iter()
                .filter_map(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .collect(),
            Op::Phi => inst
                .operands
                .chunks(2)
                .filter_map(|c| match c.first() {
                    Some(Operand::IdRef(v)) => Some(*v),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .collect(),
            Op::CopyObject => inst
                .operands
                .first()
                .and_then(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    };

    let mut cross_binding_merges: Vec<Word> = Vec::new();
    for (id, def) in &value_def {
        if !matches!(def.class.opcode, Op::Select | Op::Phi) {
            continue;
        }
        if !is_sb_pointer(*id) {
            continue;
        }
        let roots: HashSet<Word> = pointer_operands(def)
            .iter()
            .filter_map(|p| trace_root(*p, &value_def, &var_storage))
            .collect();
        if roots.len() >= 2 {
            cross_binding_merges.push(*id);
        }
    }
    if cross_binding_merges.is_empty() {
        return None;
    }

    let mut children: HashMap<Word, Vec<Word>> = HashMap::default();
    let mut parents: HashMap<Word, Vec<Word>> = HashMap::default();
    for (id, def) in &value_def {
        if !is_sb_pointer(*id) {
            continue;
        }
        for p in pointer_operands(def) {
            if is_sb_pointer(p) {
                children.entry(*id).or_default().push(p);
                parents.entry(p).or_default().push(*id);
            }
        }
    }
    let mut closure: HashSet<Word> = HashSet::default();
    let mut stack: Vec<Word> = cross_binding_merges.clone();
    while let Some(v) = stack.pop() {
        if !closure.insert(v) {
            continue;
        }
        for n in children.get(&v).into_iter().flatten() {
            stack.push(*n);
        }
        for n in parents.get(&v).into_iter().flatten() {
            stack.push(*n);
        }
    }

    for &v in &closure {
        let def = value_def.get(&v)?;
        match def.class.opcode {
            Op::Select | Op::Phi => {}
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::CopyObject => {
                let base = match def.operands.first() {
                    Some(Operand::IdRef(b)) => *b,
                    _ => return None,
                };
                let base_in_closure = closure.contains(&base);
                let leaf_root = trace_root(v, &value_def, &var_storage);
                if !base_in_closure && leaf_root.is_none() {
                    return None;
                }
            }
            _ => return None,
        }
    }

    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                let in_closure_def = inst
                    .result_id
                    .map(|r| closure.contains(&r))
                    .unwrap_or(false);
                if in_closure_def {
                    continue;
                }
                match inst.class.opcode {
                    Op::Load => {
                        if let Some(Operand::IdRef(pointer)) = inst.operands.first() {
                            if closure.contains(pointer) {
                                let pointee = value_type
                                    .get(pointer)
                                    .and_then(|pointer_type| ptr_info(*pointer_type))
                                    .map(|(_, pointee)| pointee)?;
                                if inst.result_type != Some(pointee) {
                                    return None;
                                }
                            }
                        }
                        continue;
                    }
                    Op::Store
                        if matches!(inst.operands.first(), Some(Operand::IdRef(id)) if closure.contains(id))
                            && !inst
                                .operands
                                .iter()
                                .skip(1)
                                .any(|op| matches!(op, Operand::IdRef(id) if closure.contains(id)))
                        => {
                            continue;
                        }
                    _ => {}
                }
                for op in &inst.operands {
                    if let Operand::IdRef(id) = op {
                        if closure.contains(id) {
                            return None;
                        }
                    }
                }
            }
        }
    }
    for inst in &module.annotations {
        let _ = inst;
    }

    Some(Discovery {
        type_defs,
        var_storage,
        value_def,
        value_type,
        closure,
    })
}

fn rewrite_scalar_ptr_byte_loads(module: &mut Module) -> bool {
    let mut type_defs: HashMap<Word, Instruction> = HashMap::default();
    let mut value_type: HashMap<Word, Word> = HashMap::default();
    let mut const_by_ty_value: HashMap<(Word, u64), Word> = HashMap::default();
    for inst in &module.types_global_values {
        if let Some(result) = inst.result_id {
            type_defs.insert(result, inst.clone());
            if let Some(ty) = inst.result_type {
                value_type.insert(result, ty);
            }
            if inst.class.opcode == Op::Constant {
                if let Some(ty) = inst.result_type {
                    match inst.operands.first() {
                        Some(Operand::LiteralBit32(v)) => {
                            const_by_ty_value.insert((ty, u64::from(*v)), result);
                        }
                        Some(Operand::LiteralBit64(v)) => {
                            const_by_ty_value.insert((ty, *v), result);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if let Some(result) = inst.result_id {
                    if let Some(ty) = inst.result_type {
                        value_type.insert(result, ty);
                    }
                }
            }
        }
    }

    let ptr_info = |ty: Word| -> Option<(StorageClass, Word)> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypePointer {
            return None;
        }
        match (inst.operands.first()?, inst.operands.get(1)?) {
            (Operand::StorageClass(storage), Operand::IdRef(pointee)) => Some((*storage, *pointee)),
            _ => None,
        }
    };
    let int_width = |ty: Word| -> Option<u32> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypeInt {
            return None;
        }
        match inst.operands.first()? {
            Operand::LiteralBit32(width) => Some(*width),
            _ => None,
        }
    };

    let mut ptr_array_stride: HashMap<Word, u32> = HashMap::default();
    for ann in &module.annotations {
        if ann.class.opcode == Op::Decorate
            && ann.operands.get(1) == Some(&Operand::Decoration(Decoration::ArrayStride))
        {
            if let (Some(Operand::IdRef(id)), Some(Operand::LiteralBit32(stride))) =
                (ann.operands.first(), ann.operands.get(2))
            {
                ptr_array_stride.insert(*id, *stride);
            }
        }
    }

    let mut use_count: HashMap<Word, usize> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                for operand in &inst.operands {
                    if let Operand::IdRef(id) = operand {
                        *use_count.entry(*id).or_default() += 1;
                    }
                }
            }
        }
    }
    let mut byte_select_use: HashMap<Word, Word> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if inst.class.opcode != Op::Select {
                    continue;
                }
                let Some(select_ty) = inst.result_type else {
                    continue;
                };
                if int_width(select_ty) != Some(8) {
                    continue;
                }
                for operand in inst.operands.iter().skip(1) {
                    if let Operand::IdRef(id) = operand {
                        byte_select_use.insert(*id, select_ty);
                    }
                }
            }
        }
    }

    #[derive(Clone)]
    struct Candidate {
        load_inst: usize,
        ptr_id: Word,
        ptr_ty: Word,
        scalar_ty: Word,
        scalar_width: u32,
        offset: Word,
        offset_ty: Word,
        load_result: Word,
        output_ty: Word,
        load_tail: Vec<Operand>,
    }

    let mut candidates: HashMap<(usize, usize, usize), Candidate> = HashMap::default();
    for (fi, function) in module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            let mut load_by_ptr: HashMap<Word, (usize, Word, Word, Vec<Operand>)> = HashMap::default();
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::Load {
                    continue;
                }
                let (Some(load_ty), Some(load_result), Some(Operand::IdRef(ptr))) =
                    (inst.result_type, inst.result_id, inst.operands.first())
                else {
                    continue;
                };
                if use_count.get(ptr) != Some(&1) {
                    continue;
                }
                load_by_ptr.insert(
                    *ptr,
                    (
                        ii,
                        load_ty,
                        load_result,
                        inst.operands.iter().skip(1).cloned().collect(),
                    ),
                );
            }

            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::PtrAccessChain || inst.operands.len() != 2 {
                    continue;
                }
                let (
                    Some(ptr_ty),
                    Some(ptr_id),
                    Some(Operand::IdRef(base)),
                    Some(Operand::IdRef(offset)),
                ) = (
                    inst.result_type,
                    inst.result_id,
                    inst.operands.first(),
                    inst.operands.get(1),
                )
                else {
                    continue;
                };
                let Some((load_inst, load_ty, load_result, load_tail)) =
                    load_by_ptr.get(&ptr_id).cloned()
                else {
                    continue;
                };
                if load_inst <= ii {
                    continue;
                }
                let Some((StorageClass::StorageBuffer, scalar_ty)) = ptr_info(ptr_ty) else {
                    continue;
                };
                if value_type.get(base) != Some(&ptr_ty) {
                    continue;
                }
                let Some(scalar_width) = int_width(scalar_ty) else {
                    continue;
                };
                let Some(load_width) = int_width(load_ty) else {
                    continue;
                };
                if scalar_width <= 8 || scalar_width % 8 != 0 || scalar_width > 64 {
                    continue;
                }
                if load_width != 8 {
                    if load_ty != scalar_ty || use_count.get(&load_result) != Some(&1) {
                        continue;
                    }
                    let Some(output_ty) = byte_select_use.get(&load_result).copied() else {
                        continue;
                    };
                    if int_width(output_ty) != Some(8) {
                        continue;
                    }
                    let Some(offset_ty) = value_type.get(offset).copied() else {
                        continue;
                    };
                    if int_width(offset_ty).is_none() {
                        continue;
                    }
                    candidates.insert(
                        (fi, bi, ii),
                        Candidate {
                            load_inst,
                            ptr_id,
                            ptr_ty,
                            scalar_ty,
                            scalar_width,
                            offset: *offset,
                            offset_ty,
                            load_result,
                            output_ty,
                            load_tail,
                        },
                    );
                    continue;
                }
                let Some(offset_ty) = value_type.get(offset).copied() else {
                    continue;
                };
                if int_width(offset_ty).is_none() {
                    continue;
                }
                candidates.insert(
                    (fi, bi, ii),
                    Candidate {
                        load_inst,
                        ptr_id,
                        ptr_ty,
                        scalar_ty,
                        scalar_width,
                        offset: *offset,
                        offset_ty,
                        load_result,
                        output_ty: load_ty,
                        load_tail,
                    },
                );
            }
        }
    }
    if candidates.is_empty() {
        return false;
    }

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut fresh = || {
        let id = next_id;
        next_id += 1;
        id
    };
    let mut new_globals = Vec::<Instruction>::new();
    let mut new_annotations = Vec::<Instruction>::new();

    let const_id = |ty: Word,
                    value: u64,
                    const_by_ty_value: &mut HashMap<(Word, u64), Word>,
                    new_globals: &mut Vec<Instruction>,
                    fresh: &mut dyn FnMut() -> Word|
     -> Word {
        if let Some(id) = const_by_ty_value.get(&(ty, value)) {
            return *id;
        }
        let id = fresh();
        let width = int_width(ty).unwrap_or(32);
        let literal = if width > 32 {
            Operand::LiteralBit64(value)
        } else {
            Operand::LiteralBit32(value as u32)
        };
        new_globals.push(Instruction::new(
            Op::Constant,
            Some(ty),
            Some(id),
            vec![literal],
        ));
        const_by_ty_value.insert((ty, value), id);
        id
    };

    let mut any = false;
    for (fi, function) in module.functions.iter_mut().enumerate() {
        for (bi, block) in function.blocks.iter_mut().enumerate() {
            let block_candidates = candidates
                .iter()
                .filter_map(|(&(cfi, cbi, ptr_inst), candidate)| {
                    (cfi == fi && cbi == bi).then_some((ptr_inst, candidate.clone()))
                })
                .collect::<HashMap<_, _>>();
            if block_candidates.is_empty() {
                continue;
            }

            let mut load_rewrites: HashMap<usize, (Candidate, Word, Word)> = HashMap::default();
            let mut rewritten = Vec::<Instruction>::new();
            for (ii, inst) in block.instructions.iter().cloned().enumerate() {
                if let Some(candidate) = block_candidates.get(&ii) {
                    let byte_width = u64::from(candidate.scalar_width / 8);
                    match ptr_array_stride.get(&candidate.ptr_ty).copied() {
                        Some(stride) if u64::from(stride) != byte_width => {
                            rewritten.push(inst);
                            continue;
                        }
                        Some(_) => {}
                        None => {
                            new_annotations.push(Instruction::new(
                                Op::Decorate,
                                None,
                                None,
                                vec![
                                    Operand::IdRef(candidate.ptr_ty),
                                    Operand::Decoration(Decoration::ArrayStride),
                                    Operand::LiteralBit32(byte_width as u32),
                                ],
                            ));
                            ptr_array_stride.insert(candidate.ptr_ty, byte_width as u32);
                        }
                    }

                    let divisor = const_id(
                        candidate.offset_ty,
                        byte_width,
                        &mut const_by_ty_value,
                        &mut new_globals,
                        &mut fresh,
                    );
                    let element_index = fresh();
                    let byte_remainder = fresh();
                    rewritten.push(Instruction::new(
                        Op::UDiv,
                        Some(candidate.offset_ty),
                        Some(element_index),
                        vec![Operand::IdRef(candidate.offset), Operand::IdRef(divisor)],
                    ));
                    rewritten.push(Instruction::new(
                        Op::UMod,
                        Some(candidate.offset_ty),
                        Some(byte_remainder),
                        vec![Operand::IdRef(candidate.offset), Operand::IdRef(divisor)],
                    ));
                    let mut ptr = inst;
                    ptr.operands[1] = Operand::IdRef(element_index);
                    rewritten.push(ptr);
                    load_rewrites.insert(
                        candidate.load_inst,
                        (candidate.clone(), byte_remainder, divisor),
                    );
                    any = true;
                    continue;
                }

                if let Some((candidate, byte_remainder, _divisor)) = load_rewrites.get(&ii).cloned()
                {
                    let byte_index = if candidate.offset_ty == candidate.scalar_ty {
                        byte_remainder
                    } else {
                        let converted = fresh();
                        rewritten.push(Instruction::new(
                            Op::UConvert,
                            Some(candidate.scalar_ty),
                            Some(converted),
                            vec![Operand::IdRef(byte_remainder)],
                        ));
                        converted
                    };
                    let eight = const_id(
                        candidate.scalar_ty,
                        8,
                        &mut const_by_ty_value,
                        &mut new_globals,
                        &mut fresh,
                    );
                    let mask = const_id(
                        candidate.scalar_ty,
                        0xff,
                        &mut const_by_ty_value,
                        &mut new_globals,
                        &mut fresh,
                    );
                    let shift = fresh();
                    rewritten.push(Instruction::new(
                        Op::IMul,
                        Some(candidate.scalar_ty),
                        Some(shift),
                        vec![Operand::IdRef(byte_index), Operand::IdRef(eight)],
                    ));
                    let scalar_load = fresh();
                    let mut load_ops = vec![Operand::IdRef(candidate.ptr_id)];
                    load_ops.extend(candidate.load_tail.iter().cloned());
                    rewritten.push(Instruction::new(
                        Op::Load,
                        Some(candidate.scalar_ty),
                        Some(scalar_load),
                        load_ops,
                    ));
                    let shifted = fresh();
                    rewritten.push(Instruction::new(
                        Op::ShiftRightLogical,
                        Some(candidate.scalar_ty),
                        Some(shifted),
                        vec![Operand::IdRef(scalar_load), Operand::IdRef(shift)],
                    ));
                    let masked = fresh();
                    rewritten.push(Instruction::new(
                        Op::BitwiseAnd,
                        Some(candidate.scalar_ty),
                        Some(masked),
                        vec![Operand::IdRef(shifted), Operand::IdRef(mask)],
                    ));
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(candidate.output_ty),
                        Some(candidate.load_result),
                        vec![Operand::IdRef(masked)],
                    ));
                    continue;
                }

                rewritten.push(inst);
            }
            block.instructions = rewritten;
        }
    }

    if any {
        module.types_global_values.extend(new_globals);
        module.annotations.extend(new_annotations);
        if let Some(header) = module.header.as_mut() {
            header.bound = next_id;
        }
    }
    any
}

fn invocation_varying_ids(module: &Module) -> HashSet<Word> {
    let mut varying: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::Variable
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(StorageClass::Input))
                )
        })
        .filter_map(|inst| inst.result_id)
        .collect();
    loop {
        let mut changed = false;
        for function in &module.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    let Some(result) = inst.result_id else {
                        continue;
                    };
                    if varying.contains(&result) {
                        continue;
                    }
                    if inst
                        .operands
                        .iter()
                        .any(|op| matches!(op, Operand::IdRef(id) if varying.contains(id)))
                    {
                        varying.insert(result);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return varying;
        }
    }
}

fn store_arm_varies_by_invocation(
    ptr: Word,
    value_def: &HashMap<Word, Instruction>,
    var_storage: &HashMap<Word, StorageClass>,
    varying: &HashSet<Word>,
) -> bool {
    let mut seen: HashSet<Word> = HashSet::default();
    let mut work = vec![ptr];
    while let Some(id) = work.pop() {
        if !seen.insert(id) || var_storage.contains_key(&id) {
            continue;
        }
        let Some(def) = value_def.get(&id) else {
            continue;
        };
        match def.class.opcode {
            Op::Select => {
                let Some(Operand::IdRef(condition)) = def.operands.first() else {
                    return true;
                };
                if varying.contains(condition) {
                    return true;
                }
                work.extend(def.operands.iter().skip(1).filter_map(|op| match op {
                    Operand::IdRef(arm) => Some(*arm),
                    _ => None,
                }));
            }
            Op::Phi => return true,
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::CopyObject => {
                if let Some(Operand::IdRef(base)) = def.operands.first() {
                    work.push(*base);
                }
            }
            _ => {}
        }
    }
    false
}

pub(super) fn rewrite_cross_binding_pointer_merges_to_values(module: &mut Module) -> bool {
    let Some(Discovery {
        type_defs,
        var_storage,
        value_def,
        value_type,
        closure,
    }) = discover_value_select(module)
    else {
        return false;
    };
    let ptr_info = |ty: Word| -> Option<(StorageClass, Word)> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypePointer {
            return None;
        }
        match (inst.operands.first()?, inst.operands.get(1)?) {
            (Operand::StorageClass(s), Operand::IdRef(pointee)) => Some((*s, *pointee)),
            _ => None,
        }
    };

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut fresh = || {
        let id = next_id;
        next_id += 1;
        id
    };

    let mut ptr_type_for: HashMap<Word, Word> = HashMap::default();
    for i in &module.types_global_values {
        if i.class.opcode == Op::TypePointer {
            if let (
                Some(id),
                Some(Operand::StorageClass(StorageClass::StorageBuffer)),
                Some(Operand::IdRef(pe)),
            ) = (i.result_id, i.operands.first(), i.operands.get(1))
            {
                ptr_type_for.entry(*pe).or_insert(id);
            }
        }
    }
    let mut new_ptr_types: Vec<Instruction> = Vec::new();
    let _ptr_type = |pointee: Word,
                     ptr_type_for: &mut HashMap<Word, Word>,
                     new_ptr_types: &mut Vec<Instruction>,
                     fresh: &mut dyn FnMut() -> Word|
     -> Word {
        if let Some(id) = ptr_type_for.get(&pointee) {
            return *id;
        }
        let id = fresh();
        new_ptr_types.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(id),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(pointee),
            ],
        ));
        ptr_type_for.insert(pointee, id);
        id
    };

    let const_value: HashMap<Word, u32> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Constant)
        .filter_map(|i| match (i.result_id, i.operands.first()) {
            (Some(id), Some(Operand::LiteralBit32(v))) => Some((id, *v)),
            _ => None,
        })
        .collect();
    let walk_member = |aggregate: Word, index: Word| -> Option<Word> {
        let inst = type_defs.get(&aggregate)?;
        match inst.class.opcode {
            Op::TypeVector | Op::TypeMatrix | Op::TypeArray | Op::TypeRuntimeArray => {
                match inst.operands.first()? {
                    Operand::IdRef(elem) => Some(*elem),
                    _ => None,
                }
            }
            Op::TypeStruct => {
                let member_idx = *const_value.get(&index)? as usize;
                match inst.operands.get(member_idx)? {
                    Operand::IdRef(m) => Some(*m),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let step_pointee = |pointee: Word, opcode: Op, indices: &[Word]| -> Option<Word> {
        let mut cur = pointee;
        let walk_from = if opcode == Op::PtrAccessChain {
            1
        } else {
            0
        };
        for &idx in &indices[walk_from..] {
            cur = walk_member(cur, idx)?;
        }
        Some(cur)
    };

    #[derive(Clone)]
    struct Step {
        opcode: Op,
        indices: Vec<Word>,
    }

    fn phi_suffix_indices_dominate_predecessors(
        ptr: Word,
        suffix: &[Step],
        value_def: &HashMap<Word, Instruction>,
        block_of_label: &HashMap<Word, (usize, usize)>,
        value_available_at: &dyn Fn(Word, (usize, usize)) -> bool,
        visited: &mut HashSet<(Word, Vec<(u32, Vec<Word>)>)>,
    ) -> bool {
        let key = (
            ptr,
            suffix
                .iter()
                .map(|step| (step.opcode as u32, step.indices.clone()))
                .collect::<Vec<_>>(),
        );
        if !visited.insert(key) {
            return true;
        }
        let Some(def) = value_def.get(&ptr) else {
            return true;
        };
        match def.class.opcode {
            Op::Select => def.operands.iter().skip(1).all(|operand| match operand {
                Operand::IdRef(arm) => phi_suffix_indices_dominate_predecessors(
                    *arm,
                    suffix,
                    value_def,
                    block_of_label,
                    value_available_at,
                    visited,
                ),
                _ => false,
            }),
            Op::Phi => def.operands.chunks_exact(2).all(|arm| {
                let (Operand::IdRef(value), Operand::IdRef(predecessor)) = (&arm[0], &arm[1])
                else {
                    return false;
                };
                let Some(&predecessor_block) = block_of_label.get(predecessor) else {
                    return false;
                };
                suffix
                    .iter()
                    .flat_map(|step| &step.indices)
                    .all(|index| value_available_at(*index, predecessor_block))
                    && phi_suffix_indices_dominate_predecessors(
                        *value,
                        suffix,
                        value_def,
                        block_of_label,
                        value_available_at,
                        visited,
                    )
            }),
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
                let Some(Operand::IdRef(base)) = def.operands.first() else {
                    return false;
                };
                let Some(indices) = def.operands[1..]
                    .iter()
                    .map(|operand| match operand {
                        Operand::IdRef(index) => Some(*index),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                else {
                    return false;
                };
                let mut extended = Vec::with_capacity(suffix.len() + 1);
                extended.push(Step {
                    opcode: def.class.opcode,
                    indices,
                });
                extended.extend_from_slice(suffix);
                phi_suffix_indices_dominate_predecessors(
                    *base,
                    &extended,
                    value_def,
                    block_of_label,
                    value_available_at,
                    visited,
                )
            }
            Op::CopyObject => match def.operands.first() {
                Some(Operand::IdRef(source)) => phi_suffix_indices_dominate_predecessors(
                    *source,
                    suffix,
                    value_def,
                    block_of_label,
                    value_available_at,
                    visited,
                ),
                _ => false,
            },
            _ => true,
        }
    }

    fn replay_type(
        ptr: Word,
        suffix: &[Step],
        value_type: &HashMap<Word, Word>,
        ptr_info: &dyn Fn(Word) -> Option<(StorageClass, Word)>,
        step_pointee: &dyn Fn(Word, Op, &[Word]) -> Option<Word>,
    ) -> Option<Word> {
        let base_pointee = value_type
            .get(&ptr)
            .and_then(|t| ptr_info(*t))
            .map(|(_, pe)| pe)?;
        let mut cur = base_pointee;
        for step in suffix {
            cur = step_pointee(cur, step.opcode, &step.indices)?;
        }
        Some(cur)
    }

    let mut def_block: HashMap<Word, (usize, usize)> = HashMap::default();
    let mut block_of_label: HashMap<Word, (usize, usize)> = HashMap::default();
    for (fi, function) in module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            if let Some(lbl) = block.label.as_ref().and_then(|l| l.result_id) {
                block_of_label.insert(lbl, (fi, bi));
            }
            for inst in &block.instructions {
                if let Some(r) = inst.result_id {
                    def_block.insert(r, (fi, bi));
                }
            }
        }
    }
    let block_labels = module
        .functions
        .iter()
        .map(|function| {
            function
                .blocks
                .iter()
                .map(|block| block.label.as_ref().and_then(|label| label.result_id))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let dominators = module
        .functions
        .iter()
        .zip(&block_labels)
        .map(|(function, labels)| {
            labels.first().copied().flatten()?;
            let labels = labels.iter().copied().flatten().collect::<Vec<_>>();
            Some(EmittedDominators::new(
                &labels,
                &spirv_block_successors_by_label(&function.blocks),
            ))
        })
        .collect::<Vec<_>>();
    let value_available_at = |value: Word, target: (usize, usize)| {
        let Some(&(def_function, def_block_index)) = def_block.get(&value) else {
            return true;
        };
        if def_function != target.0 {
            return false;
        }
        let Some(def_label) = block_labels[def_function][def_block_index] else {
            return false;
        };
        let Some(target_label) = block_labels[target.0][target.1] else {
            return false;
        };
        dominators[target.0]
            .as_ref()
            .is_some_and(|tree| tree.dominates(def_label, target_label))
    };

    let mut block_appends: HashMap<(usize, usize), Vec<Instruction>> = HashMap::default();
    let mut block_phis: HashMap<(usize, usize), Vec<Instruction>> = HashMap::default();

    let mut memo_phi: HashMap<(Word, Vec<(u32, Vec<Word>)>), Word> = HashMap::default();

    #[allow(clippy::too_many_arguments)]
    fn vreplay(
        ptr: Word,
        suffix: &[Step],
        sink: &mut Vec<Instruction>,
        value_def: &HashMap<Word, Instruction>,
        value_type: &HashMap<Word, Word>,
        var_storage: &HashMap<Word, StorageClass>,
        def_block: &HashMap<Word, (usize, usize)>,
        block_of_label: &HashMap<Word, (usize, usize)>,
        ptr_info: &dyn Fn(Word) -> Option<(StorageClass, Word)>,
        step_pointee: &dyn Fn(Word, Op, &[Word]) -> Option<Word>,
        memo_phi: &mut HashMap<(Word, Vec<(u32, Vec<Word>)>), Word>,
        block_appends: &mut HashMap<(usize, usize), Vec<Instruction>>,
        block_phis: &mut HashMap<(usize, usize), Vec<Instruction>>,
        ptr_type_for: &mut HashMap<Word, Word>,
        new_ptr_types: &mut Vec<Instruction>,
        fresh: &mut dyn FnMut() -> Word,
    ) -> Option<Word> {
        let value_pointee = replay_type(ptr, suffix, value_type, ptr_info, step_pointee)?;

        macro_rules! ptr_ty {
            ($pe:expr) => {{
                let pe = $pe;
                if let Some(id) = ptr_type_for.get(&pe) {
                    *id
                } else {
                    let id = fresh();
                    new_ptr_types.push(Instruction::new(
                        Op::TypePointer,
                        None,
                        Some(id),
                        vec![
                            Operand::StorageClass(StorageClass::StorageBuffer),
                            Operand::IdRef(pe),
                        ],
                    ));
                    ptr_type_for.insert(pe, id);
                    id
                }
            }};
        }

        let def = value_def.get(&ptr);

        let is_leaf = var_storage.contains_key(&ptr)
            || def.is_some_and(|d| {
                matches!(
                    d.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) && matches!(d.operands.first(), Some(Operand::IdRef(b)) if trace_root(*b, value_def, var_storage).is_some() || var_storage.contains_key(b))
            });

        if is_leaf {
            let mut cur_ptr = ptr;
            let mut cur_pointee = value_type
                .get(&ptr)
                .and_then(|t| ptr_info(*t))
                .map(|(_, pe)| pe)?;
            for step in suffix {
                let next_pointee = step_pointee(cur_pointee, step.opcode, &step.indices)?;
                let rty = ptr_ty!(next_pointee);
                let ac = fresh();
                let mut ops = vec![Operand::IdRef(cur_ptr)];
                for &idx in &step.indices {
                    ops.push(Operand::IdRef(idx));
                }
                sink.push(Instruction::new(step.opcode, Some(rty), Some(ac), ops));
                cur_ptr = ac;
                cur_pointee = next_pointee;
            }
            let load = fresh();
            sink.push(Instruction::new(
                Op::Load,
                Some(value_pointee),
                Some(load),
                vec![Operand::IdRef(cur_ptr)],
            ));
            let _ = cur_pointee;
            return Some(load);
        }

        let def = def?;
        match def.class.opcode {
            Op::Select => {
                let cond = match def.operands.first()? {
                    Operand::IdRef(c) => *c,
                    _ => return None,
                };
                let a = match def.operands.get(1)? {
                    Operand::IdRef(x) => *x,
                    _ => return None,
                };
                let b = match def.operands.get(2)? {
                    Operand::IdRef(x) => *x,
                    _ => return None,
                };
                let va = vreplay(
                    a,
                    suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )?;
                let vb = vreplay(
                    b,
                    suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )?;
                let res = fresh();
                sink.push(Instruction::new(
                    Op::Select,
                    Some(value_pointee),
                    Some(res),
                    vec![Operand::IdRef(cond), Operand::IdRef(va), Operand::IdRef(vb)],
                ));
                Some(res)
            }
            Op::Phi => {
                let key = (
                    ptr,
                    suffix
                        .iter()
                        .map(|s| (s.opcode as u32, s.indices.clone()))
                        .collect::<Vec<_>>(),
                );
                if let Some(v) = memo_phi.get(&key) {
                    return Some(*v);
                }
                let phi_block = *def_block.get(&ptr)?;
                let arms: Vec<(Word, Word)> = def
                    .operands
                    .chunks(2)
                    .filter_map(|c| match (c.first(), c.get(1)) {
                        (Some(Operand::IdRef(v)), Some(Operand::IdRef(p))) => Some((*v, *p)),
                        _ => None,
                    })
                    .collect();
                if arms.len() * 2 != def.operands.len() {
                    return None;
                }
                let res = fresh();
                memo_phi.insert(key, res);
                let mut phi_ops: Vec<Operand> = Vec::with_capacity(arms.len() * 2);
                for (val, pred) in arms {
                    let pred_loc = *block_of_label.get(&pred)?;
                    let mut arm_sink: Vec<Instruction> = Vec::new();
                    let arm_val = vreplay(
                        val,
                        suffix,
                        &mut arm_sink,
                        value_def,
                        value_type,
                        var_storage,
                        def_block,
                        block_of_label,
                        ptr_info,
                        step_pointee,
                        memo_phi,
                        block_appends,
                        block_phis,
                        ptr_type_for,
                        new_ptr_types,
                        fresh,
                    )?;
                    block_appends
                        .entry(pred_loc)
                        .or_default()
                        .append(&mut arm_sink);
                    phi_ops.push(Operand::IdRef(arm_val));
                    phi_ops.push(Operand::IdRef(pred));
                }
                block_phis
                    .entry(phi_block)
                    .or_default()
                    .push(Instruction::new(
                        Op::Phi,
                        Some(value_pointee),
                        Some(res),
                        phi_ops,
                    ));
                Some(res)
            }
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
                let base = match def.operands.first()? {
                    Operand::IdRef(b) => *b,
                    _ => return None,
                };
                let indices: Vec<Word> = def.operands[1..]
                    .iter()
                    .filter_map(|o| match o {
                        Operand::IdRef(i) => Some(*i),
                        _ => None,
                    })
                    .collect();
                if indices.len() != def.operands.len() - 1 {
                    return None;
                }
                let mut new_suffix = Vec::with_capacity(suffix.len() + 1);
                new_suffix.push(Step {
                    opcode: def.class.opcode,
                    indices,
                });
                new_suffix.extend_from_slice(suffix);
                vreplay(
                    base,
                    &new_suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )
            }
            Op::CopyObject => {
                let src = match def.operands.first()? {
                    Operand::IdRef(b) => *b,
                    _ => return None,
                };
                vreplay(
                    src,
                    suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn vstore(
        ptr: Word,
        suffix: &[Step],
        object: Word,
        store_tail: &[Operand],
        sink: &mut Vec<Instruction>,
        value_def: &HashMap<Word, Instruction>,
        value_type: &HashMap<Word, Word>,
        var_storage: &HashMap<Word, StorageClass>,
        def_block: &HashMap<Word, (usize, usize)>,
        block_of_label: &HashMap<Word, (usize, usize)>,
        ptr_info: &dyn Fn(Word) -> Option<(StorageClass, Word)>,
        step_pointee: &dyn Fn(Word, Op, &[Word]) -> Option<Word>,
        memo_phi: &mut HashMap<(Word, Vec<(u32, Vec<Word>)>), Word>,
        block_appends: &mut HashMap<(usize, usize), Vec<Instruction>>,
        block_phis: &mut HashMap<(usize, usize), Vec<Instruction>>,
        ptr_type_for: &mut HashMap<Word, Word>,
        new_ptr_types: &mut Vec<Instruction>,
        fresh: &mut dyn FnMut() -> Word,
        budget: &mut usize,
    ) -> Option<()> {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let value_pointee = replay_type(ptr, suffix, value_type, ptr_info, step_pointee)?;

        macro_rules! ptr_ty {
            ($pe:expr) => {{
                let pe = $pe;
                if let Some(id) = ptr_type_for.get(&pe) {
                    *id
                } else {
                    let id = fresh();
                    new_ptr_types.push(Instruction::new(
                        Op::TypePointer,
                        None,
                        Some(id),
                        vec![
                            Operand::StorageClass(StorageClass::StorageBuffer),
                            Operand::IdRef(pe),
                        ],
                    ));
                    ptr_type_for.insert(pe, id);
                    id
                }
            }};
        }

        let def = value_def.get(&ptr);
        let is_leaf = var_storage.contains_key(&ptr)
            || def.is_some_and(|d| {
                matches!(
                    d.class.opcode,
                    Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                ) && matches!(d.operands.first(), Some(Operand::IdRef(b)) if trace_root(*b, value_def, var_storage).is_some() || var_storage.contains_key(b))
            });
        if is_leaf {
            let mut cur_ptr = ptr;
            let mut cur_pointee = value_type
                .get(&ptr)
                .and_then(|t| ptr_info(*t))
                .map(|(_, pe)| pe)?;
            for step in suffix {
                let next_pointee = step_pointee(cur_pointee, step.opcode, &step.indices)?;
                let rty = ptr_ty!(next_pointee);
                let access = fresh();
                let mut operands = vec![Operand::IdRef(cur_ptr)];
                operands.extend(step.indices.iter().copied().map(Operand::IdRef));
                sink.push(Instruction::new(
                    step.opcode,
                    Some(rty),
                    Some(access),
                    operands,
                ));
                cur_ptr = access;
                cur_pointee = next_pointee;
            }
            if cur_pointee != value_pointee {
                return None;
            }
            let mut operands = vec![Operand::IdRef(cur_ptr), Operand::IdRef(object)];
            operands.extend(store_tail.iter().cloned());
            sink.push(Instruction::new(Op::Store, None, None, operands));
            return Some(());
        }

        let def = def?;
        match def.class.opcode {
            Op::Select => {
                let cond = match def.operands.first()? {
                    Operand::IdRef(c) => *c,
                    _ => return None,
                };
                let true_ptr = match def.operands.get(1)? {
                    Operand::IdRef(p) => *p,
                    _ => return None,
                };
                let false_ptr = match def.operands.get(2)? {
                    Operand::IdRef(p) => *p,
                    _ => return None,
                };
                let true_old = vreplay(
                    true_ptr,
                    suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )?;
                let false_old = vreplay(
                    false_ptr,
                    suffix,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                )?;
                let true_object = fresh();
                sink.push(Instruction::new(
                    Op::Select,
                    Some(value_pointee),
                    Some(true_object),
                    vec![
                        Operand::IdRef(cond),
                        Operand::IdRef(object),
                        Operand::IdRef(true_old),
                    ],
                ));
                let false_object = fresh();
                sink.push(Instruction::new(
                    Op::Select,
                    Some(value_pointee),
                    Some(false_object),
                    vec![
                        Operand::IdRef(cond),
                        Operand::IdRef(false_old),
                        Operand::IdRef(object),
                    ],
                ));
                vstore(
                    true_ptr,
                    suffix,
                    true_object,
                    store_tail,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                    budget,
                )?;
                vstore(
                    false_ptr,
                    suffix,
                    false_object,
                    store_tail,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                    budget,
                )
            }
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
                let base = match def.operands.first()? {
                    Operand::IdRef(b) => *b,
                    _ => return None,
                };
                let indices: Vec<Word> = def.operands[1..]
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::IdRef(index) => Some(*index),
                        _ => None,
                    })
                    .collect();
                if indices.len() != def.operands.len() - 1 {
                    return None;
                }
                let mut new_suffix = Vec::with_capacity(suffix.len() + 1);
                new_suffix.push(Step {
                    opcode: def.class.opcode,
                    indices,
                });
                new_suffix.extend_from_slice(suffix);
                vstore(
                    base,
                    &new_suffix,
                    object,
                    store_tail,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                    budget,
                )
            }
            Op::CopyObject => {
                let source = match def.operands.first()? {
                    Operand::IdRef(p) => *p,
                    _ => return None,
                };
                vstore(
                    source,
                    suffix,
                    object,
                    store_tail,
                    sink,
                    value_def,
                    value_type,
                    var_storage,
                    def_block,
                    block_of_label,
                    ptr_info,
                    step_pointee,
                    memo_phi,
                    block_appends,
                    block_phis,
                    ptr_type_for,
                    new_ptr_types,
                    fresh,
                    budget,
                )
            }
            Op::Phi => None,
            _ => None,
        }
    }

    let mut load_remap: HashMap<Word, Word> = HashMap::default();
    let mut loads_to_delete: HashSet<Word> = HashSet::default();
    let mut load_replay_insts: HashMap<Word, Vec<Instruction>> = HashMap::default();
    let mut stores_to_delete: HashSet<(usize, usize, usize)> = HashSet::default();
    let mut store_replay_insts: HashMap<(usize, usize, usize), Vec<Instruction>> = HashMap::default();
    {
        let mut loads: Vec<(Word, Word, (usize, usize))> = Vec::new();
        let mut stores: Vec<(usize, usize, usize, Word, Word, Vec<Operand>)> = Vec::new();
        for (fi, function) in module.functions.iter().enumerate() {
            for (bi, block) in function.blocks.iter().enumerate() {
                for (ii, inst) in block.instructions.iter().enumerate() {
                    if inst.class.opcode == Op::Load {
                        if let Some(Operand::IdRef(p)) = inst.operands.first() {
                            if closure.contains(p) {
                                if let Some(r) = inst.result_id {
                                    loads.push((r, *p, (fi, bi)));
                                }
                            }
                        }
                    }
                    if inst.class.opcode == Op::Store {
                        if let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(object))) =
                            (inst.operands.first(), inst.operands.get(1))
                        {
                            if closure.contains(ptr) {
                                stores.push((
                                    fi,
                                    bi,
                                    ii,
                                    *ptr,
                                    *object,
                                    inst.operands.iter().skip(2).cloned().collect(),
                                ));
                            }
                        }
                    }
                }
            }
        }
        if !stores.is_empty() {
            let varying = invocation_varying_ids(module);
            if stores.iter().any(|(_, _, _, ptr, _, _)| {
                store_arm_varies_by_invocation(*ptr, &value_def, &var_storage, &varying)
            }) {
                return false;
            }
        }
        for ptr in loads
            .iter()
            .map(|(_, ptr, _)| ptr)
            .chain(stores.iter().map(|(_, _, _, ptr, _, _)| ptr))
        {
            if !phi_suffix_indices_dominate_predecessors(
                *ptr,
                &[],
                &value_def,
                &block_of_label,
                &value_available_at,
                &mut HashSet::default(),
            ) {
                return false;
            }
        }
        for (load_res, ptr, _load_block) in loads {
            let mut sink: Vec<Instruction> = Vec::new();
            let replayed = vreplay(
                ptr,
                &[],
                &mut sink,
                &value_def,
                &value_type,
                &var_storage,
                &def_block,
                &block_of_label,
                &ptr_info,
                &step_pointee,
                &mut memo_phi,
                &mut block_appends,
                &mut block_phis,
                &mut ptr_type_for,
                &mut new_ptr_types,
                &mut fresh,
            );
            match replayed {
                Some(v) => {
                    load_remap.insert(load_res, v);
                    loads_to_delete.insert(load_res);
                    load_replay_insts.insert(load_res, sink);
                }
                None => return false,
            }
        }
        for (fi, bi, ii, ptr, object, store_tail) in stores {
            let mut sink: Vec<Instruction> = Vec::new();
            let mut budget = MAX_VALUE_STORE_REPLAY_NODES;
            if vstore(
                ptr,
                &[],
                object,
                &store_tail,
                &mut sink,
                &value_def,
                &value_type,
                &var_storage,
                &def_block,
                &block_of_label,
                &ptr_info,
                &step_pointee,
                &mut memo_phi,
                &mut block_appends,
                &mut block_phis,
                &mut ptr_type_for,
                &mut new_ptr_types,
                &mut fresh,
                &mut budget,
            )
            .is_none()
            {
                return false;
            }
            stores_to_delete.insert((fi, bi, ii));
            store_replay_insts.insert((fi, bi, ii), sink);
        }
    }
    let mut planned_def_block = def_block.clone();
    for (location, instructions) in block_phis.iter().chain(&block_appends) {
        for instruction in instructions {
            if let Some(result) = instruction.result_id {
                planned_def_block.insert(result, *location);
            }
        }
    }
    let planned_value_available_at = |value: Word, target: (usize, usize)| {
        let Some(&(def_function, def_block_index)) = planned_def_block.get(&value) else {
            return true;
        };
        if def_function != target.0 {
            return false;
        }
        let Some(def_label) = block_labels[def_function][def_block_index] else {
            return false;
        };
        let Some(target_label) = block_labels[target.0][target.1] else {
            return false;
        };
        dominators[target.0]
            .as_ref()
            .is_some_and(|tree| tree.dominates(def_label, target_label))
    };
    let planned_phis_are_valid = block_phis.values().flatten().all(|phi| {
        phi.operands.len().is_multiple_of(2)
            && phi.operands.chunks_exact(2).all(|incoming| {
                let (Operand::IdRef(value), Operand::IdRef(predecessor)) =
                    (&incoming[0], &incoming[1])
                else {
                    return false;
                };
                block_of_label
                    .get(predecessor)
                    .is_some_and(|location| planned_value_available_at(*value, *location))
            })
    });
    if !planned_phis_are_valid {
        return false;
    }
    if load_remap.is_empty() && stores_to_delete.is_empty() {
        return false;
    }

    apply_value_domain_rewrite(
        module,
        new_ptr_types,
        block_phis,
        block_appends,
        load_replay_insts,
        &loads_to_delete,
        store_replay_insts,
        &stores_to_delete,
        &closure,
        &load_remap,
        next_id,
    );
    rewrite_scalar_ptr_byte_loads(module);
    true
}

#[allow(clippy::too_many_arguments)]
fn apply_value_domain_rewrite(
    module: &mut Module,
    mut new_ptr_types: Vec<Instruction>,
    mut block_phis: HashMap<(usize, usize), Vec<Instruction>>,
    mut block_appends: HashMap<(usize, usize), Vec<Instruction>>,
    mut load_replay_insts: HashMap<Word, Vec<Instruction>>,
    loads_to_delete: &HashSet<Word>,
    mut store_replay_insts: HashMap<(usize, usize, usize), Vec<Instruction>>,
    stores_to_delete: &HashSet<(usize, usize, usize)>,
    closure: &HashSet<Word>,
    load_remap: &HashMap<Word, Word>,
    next_id: Word,
) {
    module.types_global_values.append(&mut new_ptr_types);

    let is_terminator = |op: Op| -> bool {
        matches!(
            op,
            Op::Branch
                | Op::BranchConditional
                | Op::Switch
                | Op::Return
                | Op::ReturnValue
                | Op::Kill
                | Op::Unreachable
                | Op::TerminateInvocation
                | Op::EmitMeshTasksEXT
        )
    };
    let mut referenced_by_new: HashSet<Word> = HashSet::default();
    for insts in block_phis
        .values()
        .chain(block_appends.values())
        .chain(load_replay_insts.values())
        .chain(store_replay_insts.values())
    {
        for inst in insts {
            if let Some(rt) = inst.result_type {
                referenced_by_new.insert(rt);
            }
            for op in &inst.operands {
                if let Operand::IdRef(w) = op {
                    referenced_by_new.insert(*w);
                }
            }
        }
    }
    for (fi, function) in module.functions.iter_mut().enumerate() {
        for (bi, block) in function.blocks.iter_mut().enumerate() {
            let phis = block_phis.remove(&(fi, bi)).unwrap_or_default();
            let appends = block_appends.remove(&(fi, bi)).unwrap_or_default();

            let old = block.instructions.clone();
            let mut rebuilt: Vec<Instruction> = Vec::with_capacity(old.len());
            for (ii, inst) in old.into_iter().enumerate() {
                let store_site = (fi, bi, ii);
                if stores_to_delete.contains(&store_site) {
                    if let Some(replay) = store_replay_insts.remove(&store_site) {
                        rebuilt.extend(replay);
                    }
                    continue;
                }
                if let Some(r) = inst.result_id {
                    if loads_to_delete.contains(&r) {
                        if let Some(replay) = load_replay_insts.remove(&r) {
                            rebuilt.extend(replay);
                        }
                        continue;
                    }
                    if closure.contains(&r) && !referenced_by_new.contains(&r) {
                        continue;
                    }
                }
                rebuilt.push(inst);
            }
            block.instructions = rebuilt;

            if !phis.is_empty() {
                let at = block
                    .instructions
                    .iter()
                    .position(|i| i.class.opcode != Op::Phi)
                    .unwrap_or(block.instructions.len());
                for (k, p) in phis.into_iter().enumerate() {
                    block.instructions.insert(at + k, p);
                }
            }

            if !appends.is_empty() {
                let at = block
                    .instructions
                    .iter()
                    .position(|i| {
                        matches!(i.class.opcode, Op::SelectionMerge | Op::LoopMerge)
                            || is_terminator(i.class.opcode)
                    })
                    .unwrap_or(block.instructions.len());
                for (k, a) in appends.into_iter().enumerate() {
                    block.instructions.insert(at + k, a);
                }
            }
        }
    }

    if !load_remap.is_empty() {
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for inst in &mut block.instructions {
                    for op in &mut inst.operands {
                        if let Operand::IdRef(id) = op {
                            if let Some(&v) = load_remap.get(id) {
                                *op = Operand::IdRef(v);
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};
    use spirv::{Capability, MemoryModel};

    fn inst(op: Op, ty: Option<Word>, res: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, res, ops)
    }

    fn build_two_buffer_select() -> Module {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(50));
        m.capabilities = vec![inst(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        )];
        m.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        m.entry_points = vec![inst(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                Operand::IdRef(30),
                Operand::LiteralString("main".to_string()),
                Operand::IdRef(20),
                Operand::IdRef(21),
            ],
        )];
        m.execution_modes = vec![inst(
            Op::ExecutionMode,
            None,
            None,
            vec![
                Operand::IdRef(30),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
            ],
        )];
        m.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(1), vec![]),
            inst(Op::TypeFunction, None, Some(2), vec![Operand::IdRef(1)]),
            inst(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeFloat,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(5), vec![Operand::IdRef(4)]),
            inst(Op::TypeStruct, None, Some(6), vec![Operand::IdRef(5)]),
            inst(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(6),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            inst(Op::TypeBool, None, Some(9), vec![]),
            inst(
                Op::Constant,
                Some(3),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(9), Some(11), vec![]),
            inst(
                Op::Constant,
                Some(3),
                Some(12),
                vec![Operand::LiteralBit32(2)],
            ),
            inst(
                Op::Variable,
                Some(7),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(7),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        m.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(5),
                    Operand::Decoration(spirv::Decoration::ArrayStride),
                    Operand::LiteralBit32(4),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(6),
                    Operand::Decoration(spirv::Decoration::Block),
                ],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(6),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(spirv::Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(spirv::Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(spirv::Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(spirv::Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(spirv::Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(31), vec![]));
        block.instructions = vec![
            inst(
                Op::InBoundsAccessChain,
                Some(8),
                Some(40),
                vec![Operand::IdRef(20), Operand::IdRef(10), Operand::IdRef(12)],
            ),
            inst(
                Op::InBoundsAccessChain,
                Some(8),
                Some(41),
                vec![Operand::IdRef(21), Operand::IdRef(10), Operand::IdRef(12)],
            ),
            inst(
                Op::Select,
                Some(8),
                Some(42),
                vec![Operand::IdRef(11), Operand::IdRef(41), Operand::IdRef(40)],
            ),
            inst(Op::Load, Some(4), Some(43), vec![Operand::IdRef(42)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.def = Some(inst(
            Op::Function,
            Some(1),
            Some(30),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(2),
            ],
        ));
        func.end = Some(inst(Op::FunctionEnd, None, None, vec![]));
        func.blocks = vec![block];
        m.functions = vec![func];
        m
    }

    fn build_mixed_uchar_uint_byte_select() -> Module {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(60));
        m.capabilities = vec![
            inst(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(Capability::Shader)],
            ),
            inst(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(
                    Capability::VariablePointersStorageBuffer,
                )],
            ),
            inst(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(Capability::Int8)],
            ),
            inst(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(Capability::StorageBuffer8BitAccess)],
            ),
        ];
        m.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        m.entry_points = vec![inst(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                Operand::IdRef(30),
                Operand::LiteralString("main".to_string()),
                Operand::IdRef(20),
                Operand::IdRef(21),
            ],
        )];
        m.execution_modes = vec![inst(
            Op::ExecutionMode,
            None,
            None,
            vec![
                Operand::IdRef(30),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
                Operand::LiteralBit32(1),
            ],
        )];
        m.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(1), vec![]),
            inst(Op::TypeFunction, None, Some(2), vec![Operand::IdRef(1)]),
            inst(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(5), vec![]),
            inst(Op::TypeRuntimeArray, None, Some(6), vec![Operand::IdRef(4)]),
            inst(Op::TypeStruct, None, Some(7), vec![Operand::IdRef(6)]),
            inst(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(7),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            inst(
                Op::TypeRuntimeArray,
                None,
                Some(10),
                vec![Operand::IdRef(3)],
            ),
            inst(Op::TypeStruct, None, Some(11), vec![Operand::IdRef(10)]),
            inst(
                Op::TypePointer,
                None,
                Some(12),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(11),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(13),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(14),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(15),
                vec![Operand::LiteralBit32(5)],
            ),
            inst(Op::ConstantTrue, Some(5), Some(16), vec![]),
            inst(
                Op::Variable,
                Some(8),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(12),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        m.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(6),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(1),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(10),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(4),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(9),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(1),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(7), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(11), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(7),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(11),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::DescriptorSet),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(31), vec![]));
        block.instructions = vec![
            inst(
                Op::AccessChain,
                Some(9),
                Some(40),
                vec![Operand::IdRef(20), Operand::IdRef(14), Operand::IdRef(14)],
            ),
            inst(
                Op::AccessChain,
                Some(13),
                Some(41),
                vec![Operand::IdRef(21), Operand::IdRef(14), Operand::IdRef(14)],
            ),
            inst(
                Op::Select,
                Some(9),
                Some(42),
                vec![Operand::IdRef(16), Operand::IdRef(40), Operand::IdRef(41)],
            ),
            inst(
                Op::PtrAccessChain,
                Some(9),
                Some(43),
                vec![Operand::IdRef(42), Operand::IdRef(15)],
            ),
            inst(Op::Load, Some(4), Some(44), vec![Operand::IdRef(43)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.def = Some(inst(
            Op::Function,
            Some(1),
            Some(30),
            vec![
                Operand::FunctionControl(spirv::FunctionControl::NONE),
                Operand::IdRef(2),
            ],
        ));
        func.end = Some(inst(Op::FunctionEnd, None, None, vec![]));
        func.blocks = vec![block];
        m.functions = vec![func];
        m
    }

    #[test]
    fn two_buffer_select_lowers_to_value_domain() {
        let mut m = build_two_buffer_select();
        assert!(rewrite_cross_binding_pointer_merges_to_values(&mut m));

        assert!(matches!(
            m.memory_model.as_ref().unwrap().operands.first(),
            Some(Operand::AddressingModel(spirv::AddressingModel::Logical))
        ));

        let insts = &m.functions[0].blocks[0].instructions;
        for i in insts {
            if i.class.opcode == Op::Select {
                let ty = i.result_type.unwrap();
                assert_eq!(ty, 4, "select must be over values, not pointers");
            }
        }
        let n_loads = insts
            .iter()
            .filter(|i| i.class.opcode == Op::Load && i.result_type == Some(4))
            .count();
        assert_eq!(n_loads, 2);

        let words: Vec<u32> = m.assemble();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let tmp = std::env::temp_dir().join(format!("m2v_vsel_{}.spv", std::process::id()));
        std::fs::write(&tmp, &bytes).unwrap();
        let out = std::process::Command::new("spirv-val")
            .arg(&tmp)
            .output()
            .expect("spirv-val on PATH");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            out.status.success(),
            "spirv-val failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn reinterpret_load_through_buffer_select_stays_for_byte_or_address_domain_lowering() {
        let mut module = build_two_buffer_select();
        let load = module.functions[0].blocks[0]
            .instructions
            .iter_mut()
            .find(|instruction| instruction.result_id == Some(43))
            .expect("fixture load");
        load.result_type = Some(3);
        let before = module.assemble();

        assert!(!rewrite_cross_binding_pointer_merges_to_values(&mut module));
        assert_eq!(module.assemble(), before);
    }

    #[test]
    fn pointer_phi_with_post_merge_index_stays_for_psb_lowering() {
        let mut module = build_two_buffer_select();
        let mut entry = Block::new();
        entry.label = Some(inst(Op::Label, None, Some(31), vec![]));
        entry.instructions = vec![inst(
            Op::BranchConditional,
            None,
            None,
            vec![Operand::IdRef(16), Operand::IdRef(32), Operand::IdRef(33)],
        )];
        let mut true_arm = Block::new();
        true_arm.label = Some(inst(Op::Label, None, Some(32), vec![]));
        true_arm.instructions = vec![
            inst(
                Op::AccessChain,
                Some(9),
                Some(40),
                vec![Operand::IdRef(20), Operand::IdRef(14), Operand::IdRef(14)],
            ),
            inst(Op::Branch, None, None, vec![Operand::IdRef(34)]),
        ];
        let mut false_arm = Block::new();
        false_arm.label = Some(inst(Op::Label, None, Some(33), vec![]));
        false_arm.instructions = vec![
            inst(
                Op::AccessChain,
                Some(13),
                Some(41),
                vec![Operand::IdRef(21), Operand::IdRef(14), Operand::IdRef(14)],
            ),
            inst(Op::Branch, None, None, vec![Operand::IdRef(34)]),
        ];
        let mut merge = Block::new();
        merge.label = Some(inst(Op::Label, None, Some(34), vec![]));
        merge.instructions = vec![
            inst(
                Op::Phi,
                Some(9),
                Some(42),
                vec![
                    Operand::IdRef(40),
                    Operand::IdRef(32),
                    Operand::IdRef(41),
                    Operand::IdRef(33),
                ],
            ),
            inst(
                Op::IAdd,
                Some(3),
                Some(45),
                vec![Operand::IdRef(14), Operand::IdRef(15)],
            ),
            inst(
                Op::PtrAccessChain,
                Some(9),
                Some(43),
                vec![Operand::IdRef(42), Operand::IdRef(45)],
            ),
            inst(Op::Load, Some(4), Some(44), vec![Operand::IdRef(43)]),
            inst(Op::Return, None, None, vec![]),
        ];
        module.functions[0].blocks = vec![entry, true_arm, false_arm, merge];
        let before = module.assemble();

        assert!(!rewrite_cross_binding_pointer_merges_to_values(&mut module));
        assert_eq!(module.assemble(), before);
    }

    #[test]
    fn mixed_uchar_uint_pointer_select_replays_uint_arm_as_byte_extract() {
        let mut m = build_mixed_uchar_uint_byte_select();
        assert!(rewrite_cross_binding_pointer_merges_to_values(&mut m));

        let insts = &m.functions[0].blocks[0].instructions;
        assert!(insts
            .iter()
            .all(|inst| inst.class.opcode != Op::Select || inst.result_type != Some(9)));
        assert!(
            insts.iter().any(|inst| inst.class.opcode == Op::UDiv),
            "uint arm must compute scalar element index"
        );
        assert!(
            insts
                .iter()
                .any(|inst| inst.class.opcode == Op::ShiftRightLogical),
            "uint arm must extract the selected byte"
        );
        assert!(
            m.annotations.iter().any(|inst| {
                inst.class.opcode == Op::Decorate
                    && inst.operands
                        == vec![
                            Operand::IdRef(13),
                            Operand::Decoration(Decoration::ArrayStride),
                            Operand::LiteralBit32(4),
                        ]
            }),
            "uint scalar pointer base needs ArrayStride 4"
        );

        let words: Vec<u32> = m.assemble();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let tmp =
            std::env::temp_dir().join(format!("m2v_vsel_mixed_byte_{}.spv", std::process::id()));
        std::fs::write(&tmp, &bytes).unwrap();
        let out = std::process::Command::new("spirv-val")
            .arg(&tmp)
            .output()
            .expect("spirv-val on PATH");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            out.status.success(),
            "spirv-val failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn store_through_a_merge_the_invocation_chooses_is_declined() {
        let mut m = build_two_buffer_select();
        m.types_global_values.push(inst(
            Op::Constant,
            Some(4),
            Some(44),
            vec![Operand::LiteralBit32(0)],
        ));
        m.types_global_values.push(inst(
            Op::TypePointer,
            None,
            Some(45),
            vec![
                Operand::StorageClass(StorageClass::Input),
                Operand::IdRef(3),
            ],
        ));
        m.types_global_values.push(inst(
            Op::Variable,
            Some(45),
            Some(46),
            vec![Operand::StorageClass(StorageClass::Input)],
        ));
        let block = &mut m.functions[0].blocks[0];
        block.instructions.retain(|i| i.result_id != Some(43));
        block.instructions.insert(
            0,
            inst(Op::Load, Some(3), Some(47), vec![Operand::IdRef(46)]),
        );
        block.instructions.insert(
            1,
            inst(
                Op::IEqual,
                Some(9),
                Some(48),
                vec![Operand::IdRef(47), Operand::IdRef(10)],
            ),
        );
        for instruction in block.instructions.iter_mut() {
            if instruction.result_id == Some(42) {
                instruction.operands[0] = Operand::IdRef(48);
            }
        }
        let ret_pos = block
            .instructions
            .iter()
            .position(|i| i.class.opcode == Op::Return)
            .unwrap();
        block.instructions.insert(
            ret_pos,
            inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(42), Operand::IdRef(44)],
            ),
        );
        let before = m.clone();
        assert!(
            !rewrite_cross_binding_pointer_merges_to_values(&mut m),
            "a store whose arm the invocation chooses must not be replayed in the value domain"
        );
        assert_eq!(
            m.functions[0].blocks[0].instructions.len(),
            before.functions[0].blocks[0].instructions.len(),
            "declining must leave the module untouched"
        );
        assert!(
            m.functions[0].blocks[0]
                .instructions
                .iter()
                .any(|inst| inst.result_id == Some(42)),
            "the pointer select survives for PSB to lower"
        );
    }

    #[test]
    fn store_through_merge_lowers_to_value_domain_rmw() {
        let mut m = build_two_buffer_select();
        m.types_global_values.push(inst(
            Op::Constant,
            Some(4),
            Some(44),
            vec![Operand::LiteralBit32(0)],
        ));
        let block = &mut m.functions[0].blocks[0];
        block.instructions.retain(|i| i.result_id != Some(43));
        let ret_pos = block
            .instructions
            .iter()
            .position(|i| i.class.opcode == Op::Return)
            .unwrap();
        block.instructions.insert(
            ret_pos,
            inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(42), Operand::IdRef(44)],
            ),
        );
        assert!(rewrite_cross_binding_pointer_merges_to_values(&mut m));

        let insts = &m.functions[0].blocks[0].instructions;
        assert!(insts
            .iter()
            .all(|inst| { inst.class.opcode != Op::Select || inst.result_type != Some(8) }));
        assert_eq!(
            insts
                .iter()
                .filter(|inst| inst.class.opcode == Op::Store)
                .count(),
            2,
            "one direct RMW store per concrete buffer arm"
        );

        let words: Vec<u32> = m.assemble();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let tmp = std::env::temp_dir().join(format!("m2v_vstore_{}.spv", std::process::id()));
        std::fs::write(&tmp, &bytes).unwrap();
        let out = std::process::Command::new("spirv-val")
            .arg(&tmp)
            .output()
            .expect("spirv-val on PATH");
        let _ = std::fs::remove_file(&tmp);
        assert!(
            out.status.success(),
            "spirv-val failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
