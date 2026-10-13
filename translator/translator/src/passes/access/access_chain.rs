use super::*;
use crate::passes::stage_input::{layout_ty_size_align, round_up};

pub(in crate::passes) fn const_int_like(ctx: &mut Ctx, like: Word, value: u64) -> Word {
    let Some(ty) = value_result_type(ctx, like) else {
        return ctx.const_uint(value as u32);
    };
    let Some(def) = type_def_of(ctx, ty) else {
        return ctx.const_uint(value as u32);
    };
    if def.class.opcode != Op::TypeInt {
        return ctx.const_uint(value as u32);
    }
    match def.operands.first() {
        Some(Operand::LiteralBit32(64)) => {
            let id = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Constant,
                Some(ty),
                Some(id),
                vec![Operand::LiteralBit64(value)],
            ));
            id
        }
        _ => ctx.const_uint(value as u32),
    }
}

pub(in crate::passes) fn narrow_access_chain_indices(ctx: &mut Ctx, entry_idx: usize) {
    let value_types = function_value_types(ctx, entry_idx);
    let defs: HashMap<Word, Instruction> = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| Some((i.result_id?, i.clone())))
        .collect();
    let wg_narrow = std::env::var_os("NVMTL_NO_WG_NARROW").is_none();
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let mut memo: HashMap<Word, Word> = HashMap::default();
        let mut new_insts: Vec<Instruction> = vec![];
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        for mut inst in insts {
            if !matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) {
                new_insts.push(inst);
                continue;
            }
            let mut pre: Vec<Instruction> = vec![];
            let workgroup = wg_narrow
                && inst
                    .result_type
                    .is_some_and(|t| pointer_is_workgroup(ctx, t));
            for oi in 1..inst.operands.len() {
                let idx = match inst.operands[oi] {
                    Operand::IdRef(r) => r,
                    _ => continue,
                };
                let idx_ty = match value_types.get(&idx).copied() {
                    Some(t) => t,
                    None => continue,
                };
                let Some(signed) = int64_signedness(ctx, idx_ty) else {
                    continue;
                };
                let _ = signed;
                if let Some(v) = const_i64_value(ctx, idx) {
                    if u32::try_from(v).is_ok() {
                        let c32 = ctx.const_uint(v as u32);
                        inst.operands[oi] = Operand::IdRef(c32);
                    }
                } else if workgroup {
                    let narrow =
                        narrow_index_low32(ctx, &defs, &value_types, &mut memo, &mut pre, idx, 0);
                    inst.operands[oi] = Operand::IdRef(narrow);
                }
            }
            new_insts.extend(pre);
            new_insts.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = new_insts;
    }
}

fn pointer_is_workgroup(ctx: &Ctx, ptr_ty: Word) -> bool {
    type_def_of(ctx, ptr_ty).is_some_and(|d| {
        d.class.opcode == Op::TypePointer
            && matches!(
                d.operands.first(),
                Some(Operand::StorageClass(StorageClass::Workgroup))
            )
    })
}

fn int_type_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    let def = type_def_of(ctx, ty)?;
    match (def.class.opcode, def.operands.first()) {
        (Op::TypeInt, Some(Operand::LiteralBit32(w))) => Some(*w),
        _ => None,
    }
}

fn narrow_index_low32(
    ctx: &mut Ctx,
    defs: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    memo: &mut HashMap<Word, Word>,
    pre: &mut Vec<Instruction>,
    v: Word,
    depth: u32,
) -> Word {
    if let Some(&n) = memo.get(&v) {
        return n;
    }
    let uint = ctx.ty_uint();
    if let Some(c) = const_i64_value(ctx, v) {
        let n = ctx.const_uint(c as u32);
        memo.insert(v, n);
        return n;
    }
    let def = if depth < 48 {
        defs.get(&v).cloned()
    } else {
        None
    };
    let rebuilt = match def {
        Some(d)
            if matches!(
                d.class.opcode,
                Op::IAdd | Op::ISub | Op::IMul | Op::BitwiseAnd | Op::BitwiseOr | Op::BitwiseXor
            ) && d.operands.len() == 2 =>
        {
            match (&d.operands[0], &d.operands[1]) {
                (Operand::IdRef(l), Operand::IdRef(r)) => {
                    let (l, r) = (*l, *r);
                    let nl = narrow_index_low32(ctx, defs, value_types, memo, pre, l, depth + 1);
                    let nr = narrow_index_low32(ctx, defs, value_types, memo, pre, r, depth + 1);
                    let id = ctx.module.fresh_id();
                    pre.push(Instruction::new(
                        d.class.opcode,
                        Some(uint),
                        Some(id),
                        vec![Operand::IdRef(nl), Operand::IdRef(nr)],
                    ));
                    Some(id)
                }
                _ => None,
            }
        }
        Some(d) if d.class.opcode == Op::ShiftLeftLogical && d.operands.len() == 2 => {
            match (&d.operands[0], &d.operands[1]) {
                (Operand::IdRef(l), Operand::IdRef(s)) => match const_i64_value(ctx, *s) {
                    Some(k) if k < 32 => {
                        let l = *l;
                        let nl =
                            narrow_index_low32(ctx, defs, value_types, memo, pre, l, depth + 1);
                        let k32 = ctx.const_uint(k as u32);
                        let id = ctx.module.fresh_id();
                        pre.push(Instruction::new(
                            Op::ShiftLeftLogical,
                            Some(uint),
                            Some(id),
                            vec![Operand::IdRef(nl), Operand::IdRef(k32)],
                        ));
                        Some(id)
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        Some(d) if matches!(d.class.opcode, Op::UConvert | Op::SConvert) => {
            match d.operands.first() {
                Some(Operand::IdRef(src)) => {
                    let src = *src;
                    match value_types.get(&src).and_then(|t| int_type_width(ctx, *t)) {
                        Some(32) => Some(src),
                        Some(w) if w < 32 => {
                            let id = ctx.module.fresh_id();
                            pre.push(Instruction::new(
                                d.class.opcode,
                                Some(uint),
                                Some(id),
                                vec![Operand::IdRef(src)],
                            ));
                            Some(id)
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    };
    let n = rebuilt.unwrap_or_else(|| {
        let id = ctx.module.fresh_id();
        pre.push(Instruction::new(
            Op::UConvert,
            Some(uint),
            Some(id),
            vec![Operand::IdRef(v)],
        ));
        id
    });
    memo.insert(v, n);
    n
}

pub(in crate::passes) fn lower_scalar_i64_arithmetic_to_u32_halves(ctx: &mut Ctx) {
    ctx.module.sync_id_bound_from_instructions();
    if let Some(staged_bound) = ctx
        .new_globals
        .iter()
        .filter_map(|instruction| instruction.result_id)
        .max()
        .map(|id| id.saturating_add(1))
    {
        if ctx.module.id_bound() < staged_bound {
            ctx.module.set_id_bound(staged_bound);
        }
    }
    let mut int_types: HashMap<Word, (u32, u32)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode != Op::TypeInt {
            continue;
        }
        let (Some(id), Some(Operand::LiteralBit32(width)), Some(Operand::LiteralBit32(signed))) =
            (inst.result_id, inst.operands.first(), inst.operands.get(1))
        else {
            continue;
        };
        int_types.insert(id, (*width, *signed));
    }

    let mut value_types: HashMap<Word, Word> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let (Some(result), Some(ty)) = (inst.result_id, inst.result_type) {
            value_types.insert(result, ty);
        }
    }
    for function in &ctx.module.functions {
        for param in &function.parameters {
            if let (Some(result), Some(ty)) = (param.result_id, param.result_type) {
                value_types.insert(result, ty);
            }
        }
        for block in &function.blocks {
            for inst in &block.instructions {
                if let (Some(result), Some(ty)) = (inst.result_id, inst.result_type) {
                    value_types.insert(result, ty);
                }
            }
        }
    }

    let uint = ctx.ty_uint();
    let ulong = ctx.ty_ulong();
    let bool_ty = ctx.ty_bool();
    let shift32 = ctx.const_int_of(ulong, 32);
    let shift16 = ctx.const_uint(16);
    let mask16 = ctx.const_uint(0xffff);
    let zero = ctx.const_uint(0);
    let one = ctx.const_uint(1);

    for function_idx in 0..ctx.module.functions.len() {
        for block_idx in 0..ctx.module.functions[function_idx].blocks.len() {
            let insts = ctx.module.functions[function_idx].blocks[block_idx]
                .instructions
                .clone();
            let mut out = Vec::with_capacity(insts.len());
            for inst in insts {
                if !matches!(inst.class.opcode, Op::IAdd | Op::ISub | Op::IMul)
                    || inst.operands.len() != 2
                    || !matches!(
                        inst.result_type.and_then(|ty| int_types.get(&ty).copied()),
                        Some((64, _))
                    )
                {
                    out.push(inst);
                    continue;
                }
                let (Operand::IdRef(lhs), Operand::IdRef(rhs)) =
                    (inst.operands[0].clone(), inst.operands[1].clone())
                else {
                    out.push(inst);
                    continue;
                };
                let Some(result_ty) = inst.result_type else {
                    out.push(inst);
                    continue;
                };
                let Some(result_id) = inst.result_id else {
                    out.push(inst);
                    continue;
                };
                let Some(lhs_ty) = value_types.get(&lhs).copied() else {
                    out.push(inst);
                    continue;
                };
                let Some(rhs_ty) = value_types.get(&rhs).copied() else {
                    out.push(inst);
                    continue;
                };
                if !matches!(int_types.get(&lhs_ty).copied(), Some((64, _)))
                    || !matches!(int_types.get(&rhs_ty).copied(), Some((64, _)))
                {
                    out.push(inst);
                    continue;
                }

                let lhs_u64 = bitcast_i64_to_ulong(ctx, &mut out, lhs, lhs_ty, ulong);
                let rhs_u64 = bitcast_i64_to_ulong(ctx, &mut out, rhs, rhs_ty, ulong);
                let lowered = match inst.class.opcode {
                    Op::IAdd => scalar_i64_add_as_u32_halves(
                        ctx, &mut out, lhs_u64, rhs_u64, uint, ulong, bool_ty, shift32, zero, one,
                    ),
                    Op::ISub => scalar_i64_sub_as_u32_halves(
                        ctx, &mut out, lhs_u64, rhs_u64, uint, ulong, bool_ty, shift32, zero, one,
                    ),
                    Op::IMul => scalar_i64_mul_as_u32_halves(
                        ctx, &mut out, lhs_u64, rhs_u64, uint, ulong, shift32, shift16, mask16,
                    ),
                    _ => unreachable!(),
                };
                if result_ty == ulong {
                    out.last_mut()
                        .expect("lowering emits a final instruction")
                        .result_id = Some(result_id);
                    value_types.insert(result_id, ulong);
                } else {
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(result_ty),
                        Some(result_id),
                        vec![Operand::IdRef(lowered)],
                    ));
                    value_types.insert(result_id, result_ty);
                }
            }
            ctx.module.functions[function_idx].blocks[block_idx].instructions = out;
        }
    }
}

fn bitcast_i64_to_ulong(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    value_ty: Word,
    ulong: Word,
) -> Word {
    if value_ty == ulong {
        return value;
    }
    let cast = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Bitcast,
        Some(ulong),
        Some(cast),
        vec![Operand::IdRef(value)],
    ));
    cast
}

fn scalar_i64_add_as_u32_halves(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lhs: Word,
    rhs: Word,
    uint: Word,
    ulong: Word,
    bool_ty: Word,
    shift32: Word,
    zero: Word,
    one: Word,
) -> Word {
    let (lhs_lo, lhs_hi) = u64_halves(ctx, out, lhs, uint, ulong, shift32);
    let (rhs_lo, rhs_hi) = u64_halves(ctx, out, rhs, uint, ulong, shift32);
    let low = iadd_u32(ctx, out, lhs_lo, rhs_lo, uint);
    let carry = ult_u32_select_bit(ctx, out, low, lhs_lo, uint, bool_ty, zero, one);
    let high_base = iadd_u32(ctx, out, lhs_hi, rhs_hi, uint);
    let high = iadd_u32(ctx, out, high_base, carry, uint);
    assemble_u64_halves(ctx, out, low, high, ulong, shift32)
}

fn scalar_i64_sub_as_u32_halves(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lhs: Word,
    rhs: Word,
    uint: Word,
    ulong: Word,
    bool_ty: Word,
    shift32: Word,
    zero: Word,
    one: Word,
) -> Word {
    let (lhs_lo, lhs_hi) = u64_halves(ctx, out, lhs, uint, ulong, shift32);
    let (rhs_lo, rhs_hi) = u64_halves(ctx, out, rhs, uint, ulong, shift32);
    let borrow = ult_u32_select_bit(ctx, out, lhs_lo, rhs_lo, uint, bool_ty, zero, one);
    let low = isub_u32(ctx, out, lhs_lo, rhs_lo, uint);
    let high_base = isub_u32(ctx, out, lhs_hi, rhs_hi, uint);
    let high = isub_u32(ctx, out, high_base, borrow, uint);
    assemble_u64_halves(ctx, out, low, high, ulong, shift32)
}

fn scalar_i64_mul_as_u32_halves(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lhs: Word,
    rhs: Word,
    uint: Word,
    ulong: Word,
    shift32: Word,
    shift16: Word,
    mask16: Word,
) -> Word {
    let lhs_lo = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(lhs_lo),
        vec![Operand::IdRef(lhs)],
    ));
    let rhs_lo = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(rhs_lo),
        vec![Operand::IdRef(rhs)],
    ));

    let lhs_shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(ulong),
        Some(lhs_shifted),
        vec![Operand::IdRef(lhs), Operand::IdRef(shift32)],
    ));
    let lhs_hi = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(lhs_hi),
        vec![Operand::IdRef(lhs_shifted)],
    ));
    let rhs_shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(ulong),
        Some(rhs_shifted),
        vec![Operand::IdRef(rhs), Operand::IdRef(shift32)],
    ));
    let rhs_hi = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(rhs_hi),
        vec![Operand::IdRef(rhs_shifted)],
    ));

    let (lo, carry) =
        mul_u32_to_u64_halves_via_u16(ctx, out, lhs_lo, rhs_lo, uint, shift16, mask16);

    let lhs_hi_rhs_lo = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(lhs_hi_rhs_lo),
        vec![Operand::IdRef(lhs_hi), Operand::IdRef(rhs_lo)],
    ));
    let lhs_lo_rhs_hi = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(lhs_lo_rhs_hi),
        vec![Operand::IdRef(lhs_lo), Operand::IdRef(rhs_hi)],
    ));
    let high_partial = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(high_partial),
        vec![Operand::IdRef(carry), Operand::IdRef(lhs_hi_rhs_lo)],
    ));
    let high = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(high),
        vec![Operand::IdRef(high_partial), Operand::IdRef(lhs_lo_rhs_hi)],
    ));

    let low64 = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(ulong),
        Some(low64),
        vec![Operand::IdRef(lo)],
    ));
    let high64 = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(ulong),
        Some(high64),
        vec![Operand::IdRef(high)],
    ));
    let high_shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(ulong),
        Some(high_shifted),
        vec![Operand::IdRef(high64), Operand::IdRef(shift32)],
    ));
    let product = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(ulong),
        Some(product),
        vec![Operand::IdRef(high_shifted), Operand::IdRef(low64)],
    ));
    product
}

fn mul_u32_to_u64_halves_via_u16(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lhs: Word,
    rhs: Word,
    uint: Word,
    shift16: Word,
    mask16: Word,
) -> (Word, Word) {
    let lhs_lo = and_u32(ctx, out, lhs, mask16, uint);
    let lhs_hi_shifted = shr_u32(ctx, out, lhs, shift16, uint);
    let lhs_hi = and_u32(ctx, out, lhs_hi_shifted, mask16, uint);
    let rhs_lo = and_u32(ctx, out, rhs, mask16, uint);
    let rhs_hi_shifted = shr_u32(ctx, out, rhs, shift16, uint);
    let rhs_hi = and_u32(ctx, out, rhs_hi_shifted, mask16, uint);

    let p0 = imul_u32(ctx, out, lhs_lo, rhs_lo, uint);
    let p1 = imul_u32(ctx, out, lhs_hi, rhs_lo, uint);
    let p2 = imul_u32(ctx, out, lhs_lo, rhs_hi, uint);
    let p3 = imul_u32(ctx, out, lhs_hi, rhs_hi, uint);

    let p0_lo = and_u32(ctx, out, p0, mask16, uint);
    let p0_hi = shr_u32(ctx, out, p0, shift16, uint);
    let p1_lo = and_u32(ctx, out, p1, mask16, uint);
    let p1_hi = shr_u32(ctx, out, p1, shift16, uint);
    let p2_lo = and_u32(ctx, out, p2, mask16, uint);
    let p2_hi = shr_u32(ctx, out, p2, shift16, uint);

    let middle_a = iadd_u32(ctx, out, p0_hi, p1_lo, uint);
    let middle = iadd_u32(ctx, out, middle_a, p2_lo, uint);
    let middle_lo = and_u32(ctx, out, middle, mask16, uint);
    let middle_carry = shr_u32(ctx, out, middle, shift16, uint);
    let middle_lo_shifted = shl_u32(ctx, out, middle_lo, shift16, uint);
    let low = or_u32(ctx, out, p0_lo, middle_lo_shifted, uint);

    let high_a = iadd_u32(ctx, out, p3, p1_hi, uint);
    let high_b = iadd_u32(ctx, out, high_a, p2_hi, uint);
    let high = iadd_u32(ctx, out, high_b, middle_carry, uint);
    (low, high)
}

fn u64_halves(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    uint: Word,
    ulong: Word,
    shift32: Word,
) -> (Word, Word) {
    let lo = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(lo),
        vec![Operand::IdRef(value)],
    ));
    let shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(ulong),
        Some(shifted),
        vec![Operand::IdRef(value), Operand::IdRef(shift32)],
    ));
    let hi = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(hi),
        vec![Operand::IdRef(shifted)],
    ));
    (lo, hi)
}

fn assemble_u64_halves(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lo: Word,
    hi: Word,
    ulong: Word,
    shift32: Word,
) -> Word {
    let low64 = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(ulong),
        Some(low64),
        vec![Operand::IdRef(lo)],
    ));
    let high64 = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(ulong),
        Some(high64),
        vec![Operand::IdRef(hi)],
    ));
    let high_shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(ulong),
        Some(high_shifted),
        vec![Operand::IdRef(high64), Operand::IdRef(shift32)],
    ));
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(ulong),
        Some(result),
        vec![Operand::IdRef(high_shifted), Operand::IdRef(low64)],
    ));
    result
}

fn imul_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, lhs: Word, rhs: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn isub_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, lhs: Word, rhs: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn iadd_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, lhs: Word, rhs: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn ult_u32_select_bit(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lhs: Word,
    rhs: Word,
    uint: Word,
    bool_ty: Word,
    zero: Word,
    one: Word,
) -> Word {
    let pred = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ULessThan,
        Some(bool_ty),
        Some(pred),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(result),
        vec![
            Operand::IdRef(pred),
            Operand::IdRef(one),
            Operand::IdRef(zero),
        ],
    ));
    result
}

fn and_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, lhs: Word, rhs: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn or_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, lhs: Word, rhs: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn shl_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, base: Word, shift: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(base), Operand::IdRef(shift)],
    ));
    result
}

fn shr_u32(ctx: &mut Ctx, out: &mut Vec<Instruction>, base: Word, shift: Word, uint: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(uint),
        Some(result),
        vec![Operand::IdRef(base), Operand::IdRef(shift)],
    ));
    result
}

pub(in crate::passes) fn rewrite_scalar_pointer_arithmetic_access_chains(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
    let mut pointer_storage = HashMap::default();
    let aggregate_types = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .filter(|inst| {
            matches!(
                inst.class.opcode,
                Op::TypeStruct | Op::TypeArray | Op::TypeRuntimeArray | Op::TypeMatrix
            )
        })
        .filter_map(|inst| inst.result_id)
        .collect::<HashSet<_>>();
    let mut pointer_pointees = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (
                Some(result),
                Some(Operand::StorageClass(storage)),
                Some(Operand::IdRef(pointee)),
            ) = (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                pointer_storage.insert(result, *storage);
                pointer_pointees.insert(result, *pointee);
            }
        }
    }

    let mut id_types = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
            id_types.insert(result, result_type);
        }
    }
    for function in &ctx.module.functions {
        for inst in &function.parameters {
            if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                id_types.insert(result, result_type);
            }
        }
        for block in &function.blocks {
            for inst in &block.instructions {
                if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                    id_types.insert(result, result_type);
                }
            }
        }
    }

    for block in &mut ctx.module.functions[entry_idx].blocks {
        for inst in &mut block.instructions {
            if inst.class.opcode != Op::InBoundsAccessChain {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            if !pointer_storage
                .get(&result_type)
                .is_some_and(|storage| ptr_access_chain_allowed_storage(*storage))
            {
                continue;
            }
            if pointer_pointees
                .get(&result_type)
                .is_some_and(|pointee| aggregate_types.contains(pointee))
            {
                continue;
            }
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            if id_types.get(base) != Some(&result_type) {
                continue;
            }
            *inst = Instruction::new(
                Op::PtrAccessChain,
                inst.result_type,
                inst.result_id,
                inst.operands.clone(),
            );
        }
    }
}

pub(in crate::passes) fn expose_nullable_memory_bases(ctx: &mut Ctx, entry_idx: usize) {
    let null_ids: HashSet<Word> = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .filter(|inst| inst.class.opcode == Op::ConstantNull)
        .filter_map(|inst| inst.result_id)
        .collect();
    let concrete_arm: HashMap<Word, Word> = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|inst| {
            let result = inst.result_id?;
            let arms = match inst.class.opcode {
                Op::Select => {
                    let [_, Operand::IdRef(on_true), Operand::IdRef(on_false)] =
                        inst.operands.as_slice()
                    else {
                        return None;
                    };
                    vec![*on_true, *on_false]
                }
                Op::Phi => inst
                    .operands
                    .chunks_exact(2)
                    .filter_map(|pair| match pair.first() {
                        Some(Operand::IdRef(value)) => Some(*value),
                        _ => None,
                    })
                    .collect(),
                _ => return None,
            };
            if !arms.iter().any(|arm| null_ids.contains(arm)) {
                return None;
            }
            let mut concrete = arms
                .into_iter()
                .filter(|arm| !null_ids.contains(arm))
                .collect::<HashSet<_>>();
            (concrete.len() == 1).then(|| (result, concrete.drain().next().unwrap()))
        })
        .collect();

    let dominance =
        crate::passes::spirv_cfg::BlockDominance::of(&ctx.module.functions[entry_idx].blocks);
    let definition_sites: HashMap<Word, (usize, usize)> = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(block_index, block)| {
            block.instructions.iter().enumerate().filter_map(
                move |(instruction_index, instruction)| {
                    Some((instruction.result_id?, (block_index, instruction_index)))
                },
            )
        })
        .collect();
    for (block_index, block) in ctx.module.functions[entry_idx]
        .blocks
        .iter_mut()
        .enumerate()
    {
        for (instruction_index, inst) in block.instructions.iter_mut().enumerate() {
            if !matches!(
                inst.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::InBoundsPtrAccessChain
                    | Op::Load
                    | Op::Store
                    | Op::AtomicLoad
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
                    | Op::AtomicFAddEXT
                    | Op::AtomicFMinEXT
                    | Op::AtomicFMaxEXT
            ) {
                continue;
            }
            let Some(Operand::IdRef(base)) = inst.operands.first_mut() else {
                continue;
            };
            let Some(concrete) = concrete_arm.get(base) else {
                continue;
            };
            if let Some(&(definition_block, definition_index)) = definition_sites.get(concrete) {
                let available = if definition_block == block_index {
                    definition_index < instruction_index
                } else {
                    dominance.dominates(definition_block, block_index)
                };
                if !available {
                    continue;
                }
            }
            *base = *concrete;
        }
    }

    loop {
        let used = ctx.module.functions[entry_idx]
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
            .flat_map(|instruction| instruction.operands.iter())
            .filter_map(|operand| match operand {
                Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => {
                    Some(*id)
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        let mut changed = false;
        for block in &mut ctx.module.functions[entry_idx].blocks {
            block.instructions.retain(|instruction| {
                let dead_nullable_merge = instruction.result_id.is_some_and(|result| {
                    concrete_arm.contains_key(&result) && !used.contains(&result)
                });
                changed |= dead_nullable_merge;
                !dead_nullable_merge
            });
        }
        if !changed {
            break;
        }
    }
}

pub(in crate::passes) fn decorate_ptr_access_chain_base_strides(ctx: &mut Ctx) {
    let query_defs = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .filter_map(|instruction| Some((instruction.result_id?, instruction)))
        .collect::<HashMap<_, _>>();
    let value_types = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .chain(ctx.module.functions.iter().flat_map(|function| {
            function
                .parameters
                .iter()
                .chain(function.blocks.iter().flat_map(|block| &block.instructions))
        }))
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    let mut storage_plans = Vec::new();
    for (function_index, function) in ctx.module.functions.iter().enumerate() {
        for (block_index, block) in function.blocks.iter().enumerate() {
            for (instruction_index, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::PtrAccessChain {
                    continue;
                }
                let (Some(result_type), Some(Operand::IdRef(base))) =
                    (inst.result_type, inst.operands.first())
                else {
                    continue;
                };
                let Some(result_pointer) = query_defs.get(&result_type) else {
                    continue;
                };
                let (Some(Operand::StorageClass(result_storage)), Some(Operand::IdRef(pointee))) = (
                    result_pointer.operands.first(),
                    result_pointer.operands.get(1),
                ) else {
                    continue;
                };
                let Some((base_storage, base_pointee)) = value_types
                    .get(base)
                    .and_then(|ty| query_defs.get(ty))
                    .and_then(|ty| match (ty.operands.first(), ty.operands.get(1)) {
                        (Some(Operand::StorageClass(storage)), Some(Operand::IdRef(pointee))) => {
                            Some((*storage, *pointee))
                        }
                        _ => None,
                    })
                else {
                    continue;
                };
                let selected_pointee = if inst.operands.len() == 2 {
                    base_pointee
                } else {
                    *pointee
                };
                if base_storage != *result_storage || selected_pointee != *pointee {
                    storage_plans.push((
                        function_index,
                        block_index,
                        instruction_index,
                        base_storage,
                        selected_pointee,
                    ));
                }
            }
        }
    }
    for (function_index, block_index, instruction_index, storage, pointee) in storage_plans {
        let pointer_type = ctx.ty_ptr(storage, pointee);
        ctx.module.functions[function_index].blocks[block_index].instructions[instruction_index]
            .result_type = Some(pointer_type);
    }

    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(result) = inst.result_id {
            defs.insert(result, inst.clone());
        }
    }

    let mut already: HashSet<Word> = HashSet::default();
    for ann in &ctx.module.annotations {
        if ann.class.opcode == Op::Decorate
            && ann.operands.get(1) == Some(&Operand::Decoration(Decoration::ArrayStride))
        {
            if let Some(Operand::IdRef(t)) = ann.operands.first() {
                already.insert(*t);
            }
        }
    }

    let mut base_ptr_types: Vec<Word> = Vec::new();
    for function in &ctx.module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if inst.class.opcode != Op::PtrAccessChain {
                    continue;
                }
                if let Some(t) = inst.result_type {
                    if !base_ptr_types.contains(&t) {
                        base_ptr_types.push(t);
                    }
                }
                if let Some(Operand::IdRef(base)) = inst.operands.first() {
                    if let Some(t) = value_types.get(base).copied() {
                        if !base_ptr_types.contains(&t) {
                            base_ptr_types.push(t);
                        }
                    }
                }
            }
        }
    }

    for ptr_ty in base_ptr_types {
        if already.contains(&ptr_ty) {
            continue;
        }
        let Some(def) = defs.get(&ptr_ty) else {
            continue;
        };
        if def.class.opcode != Op::TypePointer {
            continue;
        }
        let Some(Operand::StorageClass(storage)) = def.operands.first() else {
            continue;
        };
        if !matches!(
            storage,
            StorageClass::StorageBuffer | StorageClass::PhysicalStorageBuffer
        ) {
            continue;
        }
        let Some(Operand::IdRef(pointee)) = def.operands.get(1) else {
            continue;
        };
        let (size, align) = layout_ty_size_align(ctx, *pointee, &defs);
        let stride = round_up(size, align).max(1);
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(ptr_ty),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(stride),
            ],
        ));
        already.insert(ptr_ty);
    }
}

pub(in crate::passes) use crate::spirv_module::ptr_access_chain_allowed_storage;

pub(in crate::passes) fn walk_into_type(
    ctx: &Ctx,
    mut cur: Word,
    indices: &[Operand],
) -> Option<Word> {
    for op in indices {
        let def = type_def_of(ctx, cur)?;
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(idx_id) = op else {
                    return None;
                };
                let cdef = type_def_of(ctx, *idx_id)?;
                if cdef.class.opcode != Op::Constant {
                    return None;
                }
                let member = match cdef.operands.first()? {
                    Operand::LiteralBit32(v) => *v as usize,
                    _ => return None,
                };
                match def.operands.get(member)? {
                    Operand::IdRef(m) => *m,
                    _ => return None,
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                match def.operands.first()? {
                    Operand::IdRef(elem) => *elem,
                    _ => return None,
                }
            }
            _ => return None,
        };
    }
    Some(cur)
}

pub(in crate::passes) fn walk_into_type_partial(
    ctx: &Ctx,
    mut cur: Word,
    indices: &[Operand],
) -> (Word, usize) {
    for (n, op) in indices.iter().enumerate() {
        let Some(def) = type_def_of(ctx, cur) else {
            return (cur, n);
        };
        let next = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(idx_id) = op else {
                    return (cur, n);
                };
                let Some(cdef) = type_def_of(ctx, *idx_id) else {
                    return (cur, n);
                };
                if cdef.class.opcode != Op::Constant {
                    return (cur, n);
                }
                let member = match cdef.operands.first() {
                    Some(Operand::LiteralBit32(v)) => *v as usize,
                    _ => return (cur, n),
                };
                match def.operands.get(member) {
                    Some(Operand::IdRef(m)) => *m,
                    _ => return (cur, n),
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                match def.operands.first() {
                    Some(Operand::IdRef(elem)) => *elem,
                    _ => return (cur, n),
                }
            }
            _ => return (cur, n),
        };
        cur = next;
    }
    (cur, indices.len())
}

pub(in crate::passes) fn drop_overindexed_zero_tail(ctx: &mut Ctx, entry_idx: usize) {
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

    let mut edits: Vec<(usize, usize, usize)> = Vec::new();
    let mut reinterpret_edits: Vec<(usize, usize, usize, Word, StorageClass, Word)> = Vec::new();
    let mut identities: Vec<(Word, Word)> = Vec::new();
    let mut uses = HashMap::<Word, Vec<(usize, usize)>>::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, instruction) in block.instructions.iter().enumerate() {
            for operand in &instruction.operands {
                if let Operand::IdRef(id) = operand {
                    uses.entry(*id).or_default().push((bi, ii));
                }
            }
        }
    }
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
            if consumed >= indices.len() {
                continue;
            }
            let all_zero_tail = indices[consumed..].iter().all(|op| match op {
                Operand::IdRef(id) => const_u32(ctx, *id) == Some(0),
                _ => false,
            });
            if !all_zero_tail {
                continue;
            }
            let reached_width = direct_scalar_width(ctx, reached);
            let result_width = direct_scalar_width(ctx, result_pointee);
            if reached != result_pointee
                && reached_width == result_width
                && reached_width.is_some()
                && inst.result_id.is_some_and(|result| {
                    uses.get(&result).is_some_and(|sites| {
                        !sites.is_empty()
                            && sites.iter().all(|&(use_block, use_instruction)| {
                                let user = &ctx.module.functions[entry_idx].blocks[use_block]
                                    .instructions[use_instruction];
                                user.class.opcode == Op::Load
                                    && user.operands.first() == Some(&Operand::IdRef(result))
                                    && user.result_type == Some(result_pointee)
                            })
                    })
                })
            {
                reinterpret_edits.push((
                    bi,
                    ii,
                    consumed,
                    inst.result_id.expect("checked above"),
                    storage,
                    reached,
                ));
                continue;
            }
            if reached != result_pointee || reached_width.is_none() {
                continue;
            }
            if consumed == 0 {
                if value_types.get(base).copied() == Some(result_type) {
                    if let Some(result) = inst.result_id {
                        identities.push((result, *base));
                    }
                }
            } else {
                edits.push((bi, ii, consumed));
            }
        }
    }
    for (bi, ii, consumed) in edits {
        ctx.module.functions[entry_idx].blocks[bi].instructions[ii]
            .operands
            .truncate(1 + consumed);
    }
    let reinterpret_loads = reinterpret_edits
        .iter()
        .map(|&(_, _, _, pointer, _, reached)| (pointer, reached))
        .collect::<HashMap<_, _>>();
    for &(bi, ii, consumed, _, storage, reached) in &reinterpret_edits {
        let pointer_type = ctx.ty_ptr(storage, reached);
        let instruction = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
        instruction.result_type = Some(pointer_type);
        instruction.operands.truncate(consumed + 1);
    }
    let mut load_edits = Vec::new();
    for (block_index, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (instruction_index, current) in block.instructions.iter().enumerate() {
            if current.class.opcode != Op::Load {
                continue;
            }
            let Some(Operand::IdRef(pointer)) = current.operands.first() else {
                continue;
            };
            let (Some(&reached), Some(original_type), Some(original_result)) = (
                reinterpret_loads.get(pointer),
                current.result_type,
                current.result_id,
            ) else {
                continue;
            };
            load_edits.push((
                block_index,
                instruction_index,
                reached,
                original_type,
                original_result,
            ));
        }
    }
    load_edits.reverse();
    for (block_index, instruction_index, reached, original_type, original_result) in load_edits {
        let loaded = ctx.module.fresh_id();
        let block = &mut ctx.module.functions[entry_idx].blocks[block_index];
        let mut native_load = block.instructions[instruction_index].clone();
        native_load.result_type = Some(reached);
        native_load.result_id = Some(loaded);
        block.instructions[instruction_index] = native_load;
        block.instructions.insert(
            instruction_index + 1,
            Instruction::new(
                Op::Bitcast,
                Some(original_type),
                Some(original_result),
                vec![Operand::IdRef(loaded)],
            ),
        );
    }
    if !identities.is_empty() {
        let replacements = identities.iter().copied().collect::<HashMap<_, _>>();
        ctx.emit_sidecar.remap_ids(&replacements);
        let dead = replacements.keys().copied().collect::<HashSet<_>>();
        let function = &mut ctx.module.functions[entry_idx];
        for (from, to) in identities {
            replace_id_in_function(function, from, to);
        }
        for block in &mut function.blocks {
            block
                .instructions
                .retain(|instruction| instruction.result_id.is_none_or(|id| !dead.contains(&id)));
        }
    }
}

pub(in crate::passes) fn lower_private_low_byte_word_load(ctx: &mut Ctx, function_idx: usize) {
    let value_types = function_value_types(ctx, function_idx);
    let mut ptr_info = HashMap::<Word, (StorageClass, Word)>::default();
    for instruction in ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
    {
        if instruction.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(storage)), Some(Operand::IdRef(pointee))) = (
                instruction.result_id,
                instruction.operands.first(),
                instruction.operands.get(1),
            ) {
                ptr_info.insert(id, (*storage, *pointee));
            }
        }
    }
    let function = &ctx.module.functions[function_idx];
    let mut definitions = HashMap::<Word, (usize, usize)>::default();
    let mut users = HashMap::<Word, Vec<(usize, usize)>>::default();
    for (block_index, block) in function.blocks.iter().enumerate() {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            if let Some(id) = instruction.result_id {
                definitions.insert(id, (block_index, instruction_index));
            }
            for operand in &instruction.operands {
                if let Operand::IdRef(id) = operand {
                    users
                        .entry(*id)
                        .or_default()
                        .push((block_index, instruction_index));
                }
            }
        }
    }

    struct Edit {
        block: usize,
        instruction: usize,
        base: Word,
        scalar_type: Word,
        replacement: Word,
        extracts: Vec<Word>,
        dead: HashSet<Word>,
    }
    let mut edits = Vec::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        for (instruction_index, chain) in block.instructions.iter().enumerate() {
            if !matches!(
                chain.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain
            ) {
                continue;
            }
            let (Some(chain_id), Some(chain_type), Some(Operand::IdRef(base))) =
                (chain.result_id, chain.result_type, chain.operands.first())
            else {
                continue;
            };
            if !chain.operands[1..].iter().all(
                |operand| matches!(operand, Operand::IdRef(id) if const_u32(ctx, *id) == Some(0)),
            ) {
                continue;
            }
            let Some(base_type) = value_types.get(base).copied() else {
                continue;
            };
            let (
                Some((StorageClass::Private, scalar_type)),
                Some((StorageClass::Private, wide_type)),
            ) = (
                ptr_info.get(&base_type).copied(),
                ptr_info.get(&chain_type).copied(),
            )
            else {
                continue;
            };
            let (Some(scalar_bits), Some(wide_bits)) = (
                direct_scalar_width(ctx, scalar_type),
                direct_scalar_width(ctx, wide_type),
            ) else {
                continue;
            };
            if scalar_bits != 8 || wide_bits <= scalar_bits {
                continue;
            }
            let Some(chain_users) = users.get(&chain_id) else {
                continue;
            };
            if chain_users.is_empty() {
                continue;
            }
            let mut extracts = Vec::new();
            let mut dead = crate::fxhash::set_from([chain_id]);
            let mut valid = true;
            for &(load_block, load_index) in chain_users {
                let load = &function.blocks[load_block].instructions[load_index];
                let Some(load_id) = load.result_id else {
                    valid = false;
                    break;
                };
                if load.class.opcode != Op::Load || load.result_type != Some(wide_type) {
                    valid = false;
                    break;
                }
                dead.insert(load_id);
                let Some(load_users) = users.get(&load_id) else {
                    valid = false;
                    break;
                };
                for &(cast_block, cast_index) in load_users {
                    let cast = &function.blocks[cast_block].instructions[cast_index];
                    let Some(cast_id) = cast.result_id else {
                        valid = false;
                        break;
                    };
                    let vector_matches = cast.result_type.and_then(|ty| {
                        let definition = type_def_of(ctx, ty)?;
                        match definition.operands.as_slice() {
                            [Operand::IdRef(element), Operand::LiteralBit32(lanes)]
                                if definition.class.opcode == Op::TypeVector =>
                            {
                                Some((*element, *lanes))
                            }
                            _ => None,
                        }
                    }) == Some((scalar_type, wide_bits / scalar_bits));
                    if cast.class.opcode != Op::Bitcast || !vector_matches {
                        valid = false;
                        break;
                    }
                    dead.insert(cast_id);
                    let Some(cast_users) = users.get(&cast_id) else {
                        valid = false;
                        break;
                    };
                    for &(extract_block, extract_index) in cast_users {
                        let extract = &function.blocks[extract_block].instructions[extract_index];
                        if extract.class.opcode != Op::CompositeExtract
                            || extract.result_type != Some(scalar_type)
                            || extract.operands.get(1) != Some(&Operand::LiteralBit32(0))
                        {
                            valid = false;
                            break;
                        }
                        let Some(extract_id) = extract.result_id else {
                            valid = false;
                            break;
                        };
                        extracts.push(extract_id);
                        dead.insert(extract_id);
                    }
                }
            }
            if valid && !extracts.is_empty() {
                edits.push(Edit {
                    block: block_index,
                    instruction: instruction_index,
                    base: *base,
                    scalar_type,
                    replacement: 0,
                    extracts,
                    dead,
                });
            }
        }
    }
    for edit in &mut edits {
        edit.replacement = ctx.module.fresh_id();
    }
    for edit in &edits {
        ctx.module.functions[function_idx].blocks[edit.block]
            .instructions
            .insert(
                edit.instruction,
                Instruction::new(
                    Op::Load,
                    Some(edit.scalar_type),
                    Some(edit.replacement),
                    vec![Operand::IdRef(edit.base)],
                ),
            );
    }
    let replacements = edits
        .iter()
        .flat_map(|edit| {
            edit.extracts
                .iter()
                .map(move |extract| (*extract, edit.replacement))
        })
        .collect::<HashMap<_, _>>();
    if replacements.is_empty() {
        return;
    }
    ctx.emit_sidecar.remap_ids(&replacements);
    let dead = edits
        .into_iter()
        .flat_map(|edit| edit.dead)
        .collect::<HashSet<_>>();
    let function = &mut ctx.module.functions[function_idx];
    for (from, to) in replacements {
        replace_id_in_function(function, from, to);
    }
    for block in &mut function.blocks {
        block
            .instructions
            .retain(|instruction| instruction.result_id.is_none_or(|id| !dead.contains(&id)));
    }
}

pub(in crate::passes) fn reroot_demoted_array_element_overindex(ctx: &mut Ctx, entry_idx: usize) {
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

    let func = ctx.module.functions[entry_idx].clone();
    let mut edits: Vec<(usize, usize, Word)> = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            if inst.operands.len() != 2 {
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
            let Some(base_ptr_ty) = value_types.get(base).copied() else {
                continue;
            };
            let Some(&(base_sc, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            if base_pointee != result_pointee || direct_scalar_width(ctx, base_pointee).is_none() {
                continue;
            }
            let Some(array_id) =
                trace_to_array_element_zero(ctx, &func, *base, base_pointee, base_sc, &ptr_info)
            else {
                continue;
            };
            edits.push((bi, ii, array_id));
        }
    }
    for (bi, ii, array_id) in edits {
        ctx.module.functions[entry_idx].blocks[bi].instructions[ii].operands[0] =
            Operand::IdRef(array_id);
    }
}
