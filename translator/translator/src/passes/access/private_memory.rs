use super::*;

pub(in crate::passes) fn neutralize_null_access_chains(ctx: &mut Ctx, entry_idx: usize) {
    let mut null_ptrs: HashSet<Word> = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .filter(|inst| inst.class.opcode == Op::ConstantNull)
        .filter_map(|inst| {
            let result = inst.result_id?;
            let result_type = inst.result_type?;
            pointer_pointee(ctx, result_type).map(|_| result)
        })
        .collect();

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out = Vec::with_capacity(insts.len());
        for inst in insts {
            if matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst
                .operands
                .first()
                .and_then(|operand| match operand {
                    Operand::IdRef(base) => Some(null_ptrs.contains(base)),
                    _ => None,
                })
                .unwrap_or(false)
            {
                if let (Some(result_type), Some(result)) = (inst.result_type, inst.result_id) {
                    null_ptrs.insert(result);
                    push_null_copy(ctx, result_type, result, &mut out);
                }
                continue;
            }

            if inst.class.opcode == Op::Load
                && inst
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(ptr) => Some(null_ptrs.contains(ptr)),
                        _ => None,
                    })
                    .unwrap_or(false)
            {
                if let (Some(result_type), Some(result)) = (inst.result_type, inst.result_id) {
                    if pointer_pointee(ctx, result_type).is_some() {
                        null_ptrs.insert(result);
                    }
                    push_null_copy(ctx, result_type, result, &mut out);
                }
                continue;
            }

            if inst.class.opcode == Op::Store
                && inst
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(ptr) => Some(null_ptrs.contains(ptr)),
                        _ => None,
                    })
                    .unwrap_or(false)
            {
                continue;
            }

            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
}

pub(in crate::passes) fn neutralize_private_placeholder_access_chains(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let named_ids: HashSet<Word> = ctx
        .module
        .debug_names
        .iter()
        .filter(|inst| inst.class.opcode == Op::Name)
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .collect();
    let mut roots: HashSet<Word> = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .filter(|inst| inst.class.opcode == Op::Variable)
        .filter_map(|inst| {
            let result = inst.result_id?;
            if named_ids.contains(&result) {
                return None;
            }
            if !matches!(
                inst.operands.first(),
                Some(Operand::StorageClass(StorageClass::Private))
            ) {
                return None;
            }
            if !inst
                .operands
                .get(1)
                .and_then(|operand| match operand {
                    Operand::IdRef(init) => Some(is_constant_null_id(ctx, *init)),
                    _ => None,
                })
                .unwrap_or(false)
            {
                return None;
            }
            private_pointer_pointee(ctx, inst.result_type?).map(|_| result)
        })
        .collect();

    let mut dependencies = Vec::<(Word, Vec<Word>)>::new();
    for inst in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
    {
        let (Some(result_type), Some(result)) = (inst.result_type, inst.result_id) else {
            continue;
        };
        if private_pointer_pointee(ctx, result_type).is_none() {
            continue;
        }
        let deps = match inst.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::CopyObject => {
                inst.operands.first().and_then(|operand| match operand {
                    Operand::IdRef(base) => Some(vec![*base]),
                    _ => None,
                })
            }
            Op::Select => match inst.operands.as_slice() {
                [_, Operand::IdRef(on_true), Operand::IdRef(on_false), ..] => {
                    Some(vec![*on_true, *on_false])
                }
                _ => None,
            },
            Op::Phi if !inst.operands.is_empty() && inst.operands.len().is_multiple_of(2) => inst
                .operands
                .chunks_exact(2)
                .map(|pair| match pair.first() {
                    Some(Operand::IdRef(value)) => Some(*value),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        };
        if let Some(mut deps) = deps {
            deps.sort_unstable();
            deps.dedup();
            dependencies.push((result, deps));
        }
    }
    let mut waiting = HashMap::<Word, Vec<usize>>::default();
    let mut remaining = Vec::with_capacity(dependencies.len());
    let mut ready = Vec::new();
    for (candidate, (result, deps)) in dependencies.iter().enumerate() {
        let mut missing = 0usize;
        for dependency in deps {
            if !roots.contains(dependency) {
                missing += 1;
                waiting.entry(*dependency).or_default().push(candidate);
            }
        }
        remaining.push(missing);
        if missing == 0 {
            ready.push(*result);
        }
    }
    while let Some(root) = ready.pop() {
        if !roots.insert(root) {
            continue;
        }
        if let Some(dependents) = waiting.remove(&root) {
            for candidate in dependents {
                remaining[candidate] -= 1;
                if remaining[candidate] == 0 {
                    ready.push(dependencies[candidate].0);
                }
            }
        }
    }

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out = Vec::with_capacity(insts.len());
        for inst in insts {
            let neutralize = match inst.class.opcode {
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
                    matches!(inst.operands.first(), Some(Operand::IdRef(base)) if roots.contains(base))
                }
                Op::Select => {
                    matches!(
                        inst.operands.as_slice(),
                        [_, Operand::IdRef(on_true), Operand::IdRef(on_false), ..]
                            if roots.contains(on_true) && roots.contains(on_false)
                    )
                }
                Op::Phi => {
                    !inst.operands.is_empty()
                        && inst.operands.len().is_multiple_of(2)
                        && inst.operands.chunks_exact(2).all(|pair| {
                            matches!(pair.first(), Some(Operand::IdRef(value)) if roots.contains(value))
                        })
                }
                _ => false,
            };
            if neutralize {
                if let (Some(result_type), Some(result)) = (inst.result_type, inst.result_id) {
                    if private_pointer_pointee(ctx, result_type).is_some() {
                        let placeholder = private_zero_pointer_for_type(ctx, result_type)?;
                        out.push(Instruction::new(
                            Op::CopyObject,
                            Some(result_type),
                            Some(result),
                            vec![Operand::IdRef(placeholder)],
                        ));
                        continue;
                    }
                }
            }
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
    let roots = roots.into_iter().collect::<Vec<_>>();
    crate::passes::resources::rewrites::rewrite_private_zero_root_loads(ctx, &roots);
    Ok(())
}

pub(in crate::passes) fn lower_private_memory_atomics(ctx: &mut Ctx, entry_idx: usize) {
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out = Vec::with_capacity(insts.len());
        for inst in insts {
            let Some(bitwise_op) = private_atomic_bitwise_op(inst.class.opcode) else {
                out.push(inst);
                continue;
            };
            let (Some(result_type), Some(result)) = (inst.result_type, inst.result_id) else {
                out.push(inst);
                continue;
            };
            let [Operand::IdRef(ptr), _, _, Operand::IdRef(value), ..] = inst.operands.as_slice()
            else {
                out.push(inst);
                continue;
            };
            if !private_pointer_matches_pointee(ctx, *ptr, result_type) {
                out.push(inst);
                continue;
            }

            let updated = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Load,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(*ptr)],
            ));
            out.push(Instruction::new(
                bitwise_op,
                Some(result_type),
                Some(updated),
                vec![Operand::IdRef(result), Operand::IdRef(*value)],
            ));
            out.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(*ptr), Operand::IdRef(updated)],
            ));
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
}

pub(in crate::passes) fn private_atomic_bitwise_op(op: Op) -> Option<Op> {
    match op {
        Op::AtomicAnd => Some(Op::BitwiseAnd),
        Op::AtomicOr => Some(Op::BitwiseOr),
        _ => None,
    }
}

pub(in crate::passes) fn private_pointer_matches_pointee(
    ctx: &Ctx,
    ptr: Word,
    pointee: Word,
) -> bool {
    let Some(ptr_ty) = value_result_type(ctx, ptr) else {
        return false;
    };
    private_pointer_pointee(ctx, ptr_ty) == Some(pointee)
}

pub(in crate::passes) fn private_zero_pointer_for_type(
    ctx: &mut Ctx,
    ptr_ty: Word,
) -> Result<Word, String> {
    let pointee = private_pointer_pointee(ctx, ptr_ty).ok_or("private pointer type")?;
    let init = ctx.get_or_create(Op::ConstantNull, Some(pointee), vec![]);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(ptr_ty),
        Some(var),
        vec![
            Operand::StorageClass(StorageClass::Private),
            Operand::IdRef(init),
        ],
    ));
    ctx.interface.push(var);
    Ok(var)
}

pub(in crate::passes) fn private_pointer_pointee(ctx: &Ctx, ptr_ty: Word) -> Option<Word> {
    let inst = type_def_of(ctx, ptr_ty)?;
    if inst.class.opcode != Op::TypePointer {
        return None;
    }
    if !matches!(
        inst.operands.first(),
        Some(Operand::StorageClass(StorageClass::Private))
    ) {
        return None;
    }
    match inst.operands.get(1) {
        Some(Operand::IdRef(pointee)) => Some(*pointee),
        _ => None,
    }
}

pub(in crate::passes) fn is_constant_null_id(ctx: &Ctx, id: Word) -> bool {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .any(|inst| inst.result_id == Some(id) && inst.class.opcode == Op::ConstantNull)
}

pub(in crate::passes) fn push_null_copy(
    ctx: &mut Ctx,
    result_type: Word,
    result: Word,
    out: &mut Vec<Instruction>,
) {
    if pointer_pointee(ctx, result_type).is_some() {
        out.push(Instruction::new(
            Op::Undef,
            Some(result_type),
            Some(result),
            vec![],
        ));
        return;
    }
    let zero = ctx.get_or_create(Op::ConstantNull, Some(result_type), vec![]);
    out.push(Instruction::new(
        Op::CopyObject,
        Some(result_type),
        Some(result),
        vec![Operand::IdRef(zero)],
    ));
}

pub(in crate::passes) fn hoist_function_variables(ctx: &mut Ctx, entry_idx: usize) {
    let mut vars = Vec::new();
    for block in &mut ctx.module.functions[entry_idx].blocks {
        block.instructions.retain(|inst| {
            if is_function_variable(inst) {
                vars.push(inst.clone());
                false
            } else {
                true
            }
        });
    }
    if let Some(first) = ctx.module.functions[entry_idx].blocks.first_mut() {
        first.instructions.splice(0..0, vars);
    }
}

pub(in crate::passes) fn is_function_variable(inst: &Instruction) -> bool {
    inst.class.opcode == Op::Variable
        && matches!(
            inst.operands.first(),
            Some(Operand::StorageClass(StorageClass::Function))
        )
}

pub(in crate::passes) fn guard_integer_division_by_zero(ctx: &mut Ctx, entry_idx: usize) {
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out = Vec::with_capacity(insts.len());
        for mut inst in insts {
            if !matches!(inst.class.opcode, Op::UDiv | Op::SDiv | Op::UMod | Op::SRem) {
                out.push(inst);
                continue;
            }
            let Some(result_ty) = inst.result_type else {
                out.push(inst);
                continue;
            };
            let Some(Operand::IdRef(denom)) = inst.operands.get(1) else {
                out.push(inst);
                continue;
            };
            let denom = *denom;
            if integer_constant_is_never_zero(ctx, denom) {
                out.push(inst);
                continue;
            }
            let Some((zero, one, pred_ty)) = integer_division_guard_values(ctx, result_ty) else {
                out.push(inst);
                continue;
            };

            let is_zero = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IEqual,
                Some(pred_ty),
                Some(is_zero),
                vec![Operand::IdRef(denom), Operand::IdRef(zero)],
            ));
            let safe_denom = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Select,
                Some(result_ty),
                Some(safe_denom),
                vec![
                    Operand::IdRef(is_zero),
                    Operand::IdRef(one),
                    Operand::IdRef(denom),
                ],
            ));
            inst.operands[1] = Operand::IdRef(safe_denom);
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
}

pub(in crate::passes) fn integer_division_guard_values(
    ctx: &mut Ctx,
    ty: Word,
) -> Option<(Word, Word, Word)> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeInt => {
            let zero = ctx.const_int_of(ty, 0);
            let one = ctx.const_int_of(ty, 1);
            let pred_ty = ctx.ty_bool();
            Some((zero, one, pred_ty))
        }
        Op::TypeVector => {
            let elem = match def.operands.first()? {
                Operand::IdRef(elem) => *elem,
                _ => return None,
            };
            let lanes = match def.operands.get(1)? {
                Operand::LiteralBit32(lanes) => *lanes,
                _ => return None,
            };
            let elem_def = type_def_of(ctx, elem)?;
            if elem_def.class.opcode != Op::TypeInt {
                return None;
            }
            let zero_elem = ctx.const_int_of(elem, 0);
            let one_elem = ctx.const_int_of(elem, 1);
            let zero = const_composite_splat(ctx, ty, zero_elem, lanes);
            let one = const_composite_splat(ctx, ty, one_elem, lanes);
            let pred_ty = ctx.ty_vec_bool(lanes);
            Some((zero, one, pred_ty))
        }
        _ => None,
    }
}

pub(in crate::passes) fn const_composite_splat(
    ctx: &mut Ctx,
    ty: Word,
    value: Word,
    lanes: u32,
) -> Word {
    ctx.const_composite(ty, vec![value; lanes as usize])
}
