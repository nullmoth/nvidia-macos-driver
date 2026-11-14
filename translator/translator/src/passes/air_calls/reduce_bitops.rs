use super::*;
use spirv::GroupOperation;

pub(in crate::passes) fn lower_quad_sum(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
) -> Result<Vec<Instruction>, String> {
    lower_quad_combine(ctx, result, result_type, value, false)
}

pub(in crate::passes) fn lower_quad_product(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
) -> Result<Vec<Instruction>, String> {
    lower_quad_combine(ctx, result, result_type, value, true)
}

// A quad's sum or product: two exclusive-or exchanges, one along each axis, combined with the operation.
fn lower_quad_combine(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
    product: bool,
) -> Result<Vec<Instruction>, String> {
    let elem_ty = element_type(ctx, result_type);
    let Some(elem_def) = type_def_of(ctx, elem_ty) else {
        return Err("quad reduction element type is undefined".to_string());
    };
    let add = match (elem_def.class.opcode, product) {
        (Op::TypeFloat, false) => Op::FAdd,
        (Op::TypeInt, false) => Op::IAdd,
        (Op::TypeFloat, true) => Op::FMul,
        (Op::TypeInt, true) => Op::IMul,
        _ => return Err("quad reduction element type is not numeric".to_string()),
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let mask1 = ctx.const_uint(1);
    let horiz = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffleXor,
        Some(result_type),
        Some(horiz),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(value),
            Operand::IdRef(mask1),
        ],
    ));
    let pair = ctx.module.fresh_id();
    insts.push(Instruction::new(
        add,
        Some(result_type),
        Some(pair),
        vec![Operand::IdRef(value), Operand::IdRef(horiz)],
    ));
    let mask2 = ctx.const_uint(2);
    let vert = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffleXor,
        Some(result_type),
        Some(vert),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(pair),
            Operand::IdRef(mask2),
        ],
    ));
    insts.push(Instruction::new(
        add,
        Some(result_type),
        Some(result),
        vec![Operand::IdRef(pair), Operand::IdRef(vert)],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_quad_integer_extrema(
    ctx: &mut Ctx,
    name: &str,
    result: Word,
    result_type: Word,
    value: Word,
) -> Result<Vec<Instruction>, String> {
    let is_max = name.starts_with("air.quad_max.");
    let compare = match (is_max, name.contains(".s."), name.contains(".u.")) {
        (true, true, false) => Op::SGreaterThan,
        (true, false, true) => Op::UGreaterThan,
        (false, true, false) => Op::SLessThan,
        (false, false, true) => Op::ULessThan,
        _ => {
            return Err(format!(
                "unsupported quad integer extrema intrinsic: {name}"
            ))
        }
    };
    let result_definition =
        type_def_of(ctx, result_type).ok_or_else(|| format!("{name} result type is undefined"))?;
    let lanes = match result_definition.class.opcode {
        Op::TypeInt => 1,
        Op::TypeVector => match result_definition.operands.get(1) {
            Some(Operand::LiteralBit32(lanes)) => *lanes,
            _ => return Err(format!("{name} result vector has no lane count")),
        },
        _ => return Err(format!("{name} result is not an integer scalar or vector")),
    };
    let bool_type = if lanes == 1 {
        ctx.ty_bool()
    } else {
        ctx.ty_vec_bool(lanes)
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut out = Vec::new();
    let mut winner = value;
    for (axis, mask) in [1, 2].into_iter().enumerate() {
        let peer = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::GroupNonUniformShuffleXor,
            Some(result_type),
            Some(peer),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(winner),
                Operand::IdRef(ctx.const_uint(mask)),
            ],
        ));
        let predicate = ctx.module.fresh_id();
        out.push(Instruction::new(
            compare,
            Some(bool_type),
            Some(predicate),
            vec![Operand::IdRef(winner), Operand::IdRef(peer)],
        ));
        let selected = if axis == 1 {
            result
        } else {
            ctx.module.fresh_id()
        };
        out.push(Instruction::new(
            Op::Select,
            Some(result_type),
            Some(selected),
            vec![
                Operand::IdRef(predicate),
                Operand::IdRef(winner),
                Operand::IdRef(peer),
            ],
        ));
        winner = selected;
    }
    Ok(out)
}

pub(in crate::passes) fn group_reduce_operands(
    ctx: &mut Ctx,
    scope: Word,
    operation: GroupOperation,
    value: Word,
) -> Vec<Operand> {
    if matches!(operation, GroupOperation::Reduce) {
        let cluster = ctx.const_uint(32);
        return vec![
            Operand::IdScope(scope),
            Operand::GroupOperation(GroupOperation::ClusteredReduce),
            Operand::IdRef(value),
            Operand::IdRef(cluster),
        ];
    }
    vec![
        Operand::IdScope(scope),
        Operand::GroupOperation(operation),
        Operand::IdRef(value),
    ]
}

pub(in crate::passes) fn lower_simd_sum(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
    operation: GroupOperation,
) -> Result<Vec<Instruction>, String> {
    let elem_ty = element_type(ctx, result_type);
    let Some(elem_def) = type_def_of(ctx, elem_ty) else {
        return Err("simd sum element type is undefined".to_string());
    };
    let (op, subtract) = match elem_def.class.opcode {
        Op::TypeFloat => (Op::GroupNonUniformFAdd, Op::FSub),
        Op::TypeInt => (Op::GroupNonUniformIAdd, Op::ISub),
        _ => return Err("simd sum element type is not numeric".to_string()),
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let operands = group_reduce_operands(ctx, scope, operation, value);
    if !matches!(
        operation,
        GroupOperation::ExclusiveScan | GroupOperation::InclusiveScan
    ) {
        return Ok(vec![Instruction::new(
            op,
            Some(result_type),
            Some(result),
            operands,
        )]);
    }

    let native = ctx.module.fresh_id();
    let mut insts = vec![Instruction::new(
        op,
        Some(result_type),
        Some(native),
        operands,
    )];
    let exclusive = if operation == GroupOperation::InclusiveScan {
        let exclusive = ctx.module.fresh_id();
        insts.push(Instruction::new(
            subtract,
            Some(result_type),
            Some(exclusive),
            vec![Operand::IdRef(native), Operand::IdRef(value)],
        ));
        exclusive
    } else {
        native
    };
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let uint = ctx.ty_uint();
    let local_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(local_lane),
        vec![Operand::IdRef(lane), Operand::IdRef(ctx.const_uint(31))],
    ));
    let base_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(base_lane),
        vec![Operand::IdRef(lane), Operand::IdRef(local_lane)],
    ));
    let partition_prefix = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(partition_prefix),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(exclusive),
            Operand::IdRef(base_lane),
        ],
    ));
    insts.push(Instruction::new(
        subtract,
        Some(result_type),
        Some(result),
        vec![Operand::IdRef(native), Operand::IdRef(partition_prefix)],
    ));
    Ok(insts)
}

/// simd_product and the prefix products. The reduction is a clustered reduce of 32 lanes like the sum. The scans are
/// built from shuffles instead of the native scan: a product has no inverse (the sum's scan removes the other 32-lane
/// partitions of a wider subgroup by subtracting), so the inclusive scan doubles its reach each step within the
/// partition and the exclusive scan shifts it up one lane, with 1 in the first lane. Scalar 32-bit float and integer only.
pub(in crate::passes) fn lower_simd_product(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
    operation: GroupOperation,
) -> Result<Vec<Instruction>, String> {
    let Some(def) = type_def_of(ctx, result_type) else {
        return Err("simd product type is undefined".to_string());
    };
    let (mul, group_mul, one) = match (def.class.opcode, def.operands.first()) {
        (Op::TypeFloat, Some(Operand::LiteralBit32(32))) => {
            (Op::FMul, Op::GroupNonUniformFMul, ctx.const_float(1.0))
        }
        (Op::TypeInt, _) => (Op::IMul, Op::GroupNonUniformIMul, ctx.const_int_of(result_type, 1)),
        _ => {
            return Err(
                "unsupported simd product: only scalar 32-bit float and integer values are lowered".to_string()
            )
        }
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    if operation == GroupOperation::Reduce {
        let operands = group_reduce_operands(ctx, scope, operation, value);
        return Ok(vec![Instruction::new(group_mul, Some(result_type), Some(result), operands)]);
    }
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let mut insts = Vec::new();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(local),
        vec![Operand::IdRef(lane), Operand::IdRef(ctx.const_uint(31))],
    ));
    let mut running = value;
    for delta in [1u32, 2, 4, 8, 16] {
        let below = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffleUp,
            Some(result_type),
            Some(below),
            vec![Operand::IdScope(scope), Operand::IdRef(running), Operand::IdRef(ctx.const_uint(delta))],
        ));
        let product = ctx.module.fresh_id();
        insts.push(Instruction::new(
            mul,
            Some(result_type),
            Some(product),
            vec![Operand::IdRef(running), Operand::IdRef(below)],
        ));
        let has_source = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::UGreaterThanEqual,
            Some(bool_ty),
            Some(has_source),
            vec![Operand::IdRef(local), Operand::IdRef(ctx.const_uint(delta))],
        ));
        let next = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::Select,
            Some(result_type),
            Some(next),
            vec![Operand::IdRef(has_source), Operand::IdRef(product), Operand::IdRef(running)],
        ));
        running = next;
    }
    if operation == GroupOperation::InclusiveScan {
        insts.push(Instruction::new(
            Op::CopyObject,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(running)],
        ));
        return Ok(insts);
    }
    let shifted = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffleUp,
        Some(result_type),
        Some(shifted),
        vec![Operand::IdScope(scope), Operand::IdRef(running), Operand::IdRef(ctx.const_uint(1))],
    ));
    let first = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(first),
        vec![Operand::IdRef(local), Operand::IdRef(ctx.const_uint(0))],
    ));
    insts.push(Instruction::new(
        Op::Select,
        Some(result_type),
        Some(result),
        vec![Operand::IdRef(first), Operand::IdRef(one), Operand::IdRef(shifted)],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_bitwise(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    value: Word,
    operation: GroupOperation,
    op: Op,
) -> Result<Vec<Instruction>, String> {
    let elem_ty = element_type(ctx, result_type);
    let Some(elem_def) = type_def_of(ctx, elem_ty) else {
        return Err("simd bitwise element type is undefined".to_string());
    };
    if elem_def.class.opcode != Op::TypeInt {
        return Err("simd bitwise element type is not integer".to_string());
    }
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let operands = group_reduce_operands(ctx, scope, operation, value);
    Ok(vec![Instruction::new(
        op,
        Some(result_type),
        Some(result),
        operands,
    )])
}

#[derive(Clone, Copy)]
pub(in crate::passes) enum SimdExtrema {
    Min,
    Max,
}

pub(in crate::passes) fn lower_simd_extrema(
    ctx: &mut Ctx,
    name: &str,
    result: Word,
    result_type: Word,
    value: Word,
    operation: GroupOperation,
    extrema: SimdExtrema,
) -> Result<Vec<Instruction>, String> {
    let elem_ty = element_type(ctx, result_type);
    let Some(elem_def) = type_def_of(ctx, elem_ty) else {
        return Err("simd extrema element type is undefined".to_string());
    };
    let mut reduce_ty = result_type;
    let op = match elem_def.class.opcode {
        Op::TypeFloat => match extrema {
            SimdExtrema::Min => Op::GroupNonUniformFMin,
            SimdExtrema::Max => Op::GroupNonUniformFMax,
        },
        Op::TypeInt => {
            let signed = if name.contains(".s.") {
                true
            } else if name.contains(".u.") {
                false
            } else {
                return Err(format!(
                    "{name} is an integer subgroup extrema with neither a .s. nor a .u. marker, so                      which comparison AIR wants is unstated"
                ));
            };
            reduce_ty = signed_int_type(ctx, result_type, signed)
                .ok_or_else(|| format!("{name} result type is not an integer shape"))?;
            match (extrema, signed) {
                (SimdExtrema::Min, true) => Op::GroupNonUniformSMin,
                (SimdExtrema::Min, false) => Op::GroupNonUniformUMin,
                (SimdExtrema::Max, true) => Op::GroupNonUniformSMax,
                (SimdExtrema::Max, false) => Op::GroupNonUniformUMax,
            }
        }
        _ => return Err("simd extrema element type is not numeric".to_string()),
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    if reduce_ty == result_type {
        let operands = group_reduce_operands(ctx, scope, operation, value);
        return Ok(vec![Instruction::new(
            op,
            Some(result_type),
            Some(result),
            operands,
        )]);
    }
    let mut out = Vec::new();
    let input = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Bitcast,
        Some(reduce_ty),
        Some(input),
        vec![Operand::IdRef(value)],
    ));
    let reduced = ctx.module.fresh_id();
    let operands = group_reduce_operands(ctx, scope, operation, input);
    out.push(Instruction::new(
        op,
        Some(reduce_ty),
        Some(reduced),
        operands,
    ));
    out.push(Instruction::new(
        Op::Bitcast,
        Some(result_type),
        Some(result),
        vec![Operand::IdRef(reduced)],
    ));
    Ok(out)
}

fn signed_int_type(ctx: &mut Ctx, ty: Word, signed: bool) -> Option<Word> {
    let def = type_def_of(ctx, ty)?.clone();
    match def.class.opcode {
        Op::TypeInt => {
            let Some(&Operand::LiteralBit32(width)) = def.operands.first() else {
                return None;
            };
            Some(ctx.get_or_create(
                Op::TypeInt,
                None,
                vec![
                    Operand::LiteralBit32(width),
                    Operand::LiteralBit32(u32::from(signed)),
                ],
            ))
        }
        Op::TypeVector => {
            let Some(&Operand::IdRef(elem)) = def.operands.first() else {
                return None;
            };
            let Some(&Operand::LiteralBit32(lanes)) = def.operands.get(1) else {
                return None;
            };
            let elem = signed_int_type(ctx, elem, signed)?;
            Some(ctx.get_or_create(
                Op::TypeVector,
                None,
                vec![Operand::IdRef(elem), Operand::LiteralBit32(lanes)],
            ))
        }
        _ => None,
    }
}

pub(in crate::passes) fn composite_shape(ctx: &Ctx, ty: Word) -> Option<(Word, u32)> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeVector => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(elem)) => *elem,
                _ => return None,
            };
            let lanes = match def.operands.get(1) {
                Some(Operand::LiteralBit32(lanes)) => *lanes,
                _ => return None,
            };
            Some((elem, lanes))
        }
        Op::TypeArray => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(elem)) => *elem,
                _ => return None,
            };
            let len_id = match def.operands.get(1) {
                Some(Operand::IdRef(len_id)) => *len_id,
                _ => return None,
            };
            constant_u32(ctx, len_id).map(|lanes| (elem, lanes))
        }
        _ => None,
    }
}

pub(in crate::passes) fn constant_u32(ctx: &Ctx, id: Word) -> Option<u32> {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .find(|inst| inst.class.opcode == Op::Constant && inst.result_id == Some(id))
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        })
}

pub(in crate::passes) fn is_f32_scalar(ctx: &Ctx, ty: Word) -> bool {
    type_def_of(ctx, ty)
        .map(|def| {
            def.class.opcode == Op::TypeFloat
                && def.operands.first() == Some(&Operand::LiteralBit32(32))
        })
        .unwrap_or(false)
}

pub(in crate::passes) fn is_f32_scalar_or_vector(ctx: &Ctx, ty: Word) -> bool {
    is_f32_scalar(ctx, ty)
        || vector_type_shape(ctx, ty).is_some_and(|(elem, _)| is_f32_scalar(ctx, elem))
}

pub(in crate::passes) fn is_u16_scalar(ctx: &Ctx, ty: Word) -> bool {
    is_uint_scalar_width(ctx, ty, 16)
}

pub(in crate::passes) fn is_uint_scalar_width(ctx: &Ctx, ty: Word, width: u32) -> bool {
    type_def_of(ctx, ty)
        .map(|def| {
            def.class.opcode == Op::TypeInt
                && def.operands.first() == Some(&Operand::LiteralBit32(width))
                && def.operands.get(1) == Some(&Operand::LiteralBit32(0))
        })
        .unwrap_or(false)
}

pub(in crate::passes) fn is_int_scalar_width(ctx: &Ctx, ty: Word, width: u32) -> bool {
    int_scalar_width(ctx, ty) == Some(width)
}

pub(in crate::passes) fn int_vector_width(ctx: &Ctx, ty: Word) -> Option<(u32, u32)> {
    let (elem, lanes) = vector_type_shape(ctx, ty)?;
    int_scalar_width(ctx, elem).map(|width| (lanes, width))
}

pub(in crate::passes) fn int_scalar_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    type_def_of(ctx, ty).and_then(|def| match (def.class.opcode, def.operands.first()) {
        (Op::TypeInt, Some(Operand::LiteralBit32(width))) => Some(*width),
        _ => None,
    })
}

pub(in crate::passes) fn rotate_left_i32(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    value: Word,
    shift: Word,
) -> Vec<Instruction> {
    let mask = ctx.const_int_of(rty, 31);
    let normalized_shift = ctx.module.fresh_id();
    let left = ctx.module.fresh_id();
    let width = ctx.const_int_of(rty, 32);
    let inverse_unmasked = ctx.module.fresh_id();
    let inverse_shift = ctx.module.fresh_id();
    let right = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(normalized_shift),
            vec![Operand::IdRef(shift), Operand::IdRef(mask)],
        ),
        Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(left),
            vec![Operand::IdRef(value), Operand::IdRef(normalized_shift)],
        ),
        Instruction::new(
            Op::ISub,
            Some(rty),
            Some(inverse_unmasked),
            vec![Operand::IdRef(width), Operand::IdRef(normalized_shift)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(inverse_shift),
            vec![Operand::IdRef(inverse_unmasked), Operand::IdRef(mask)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(right),
            vec![Operand::IdRef(value), Operand::IdRef(inverse_shift)],
        ),
        Instruction::new(
            Op::BitwiseOr,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(left), Operand::IdRef(right)],
        ),
    ]
}

pub(in crate::passes) fn byte_swap_i32(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    value: Word,
) -> Vec<Instruction> {
    let mask_byte0 = ctx.const_int_of(rty, 0x0000_00ff);
    let mask_byte1 = ctx.const_int_of(rty, 0x0000_ff00);
    let mask_byte2 = ctx.const_int_of(rty, 0x00ff_0000);
    let mask_byte3 = ctx.const_int_of(rty, 0xff00_0000);
    let shift8 = ctx.const_int_of(rty, 8);
    let shift24 = ctx.const_int_of(rty, 24);

    let b0 = ctx.module.fresh_id();
    let b1 = ctx.module.fresh_id();
    let b2 = ctx.module.fresh_id();
    let b3 = ctx.module.fresh_id();
    let b0_hi = ctx.module.fresh_id();
    let b1_hi = ctx.module.fresh_id();
    let b2_lo = ctx.module.fresh_id();
    let b3_lo = ctx.module.fresh_id();
    let hi = ctx.module.fresh_id();
    let lo = ctx.module.fresh_id();

    vec![
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(b0),
            vec![Operand::IdRef(value), Operand::IdRef(mask_byte0)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(b1),
            vec![Operand::IdRef(value), Operand::IdRef(mask_byte1)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(b2),
            vec![Operand::IdRef(value), Operand::IdRef(mask_byte2)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(b3),
            vec![Operand::IdRef(value), Operand::IdRef(mask_byte3)],
        ),
        Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(b0_hi),
            vec![Operand::IdRef(b0), Operand::IdRef(shift24)],
        ),
        Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(b1_hi),
            vec![Operand::IdRef(b1), Operand::IdRef(shift8)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(b2_lo),
            vec![Operand::IdRef(b2), Operand::IdRef(shift8)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(b3_lo),
            vec![Operand::IdRef(b3), Operand::IdRef(shift24)],
        ),
        Instruction::new(
            Op::BitwiseOr,
            Some(rty),
            Some(hi),
            vec![Operand::IdRef(b0_hi), Operand::IdRef(b1_hi)],
        ),
        Instruction::new(
            Op::BitwiseOr,
            Some(rty),
            Some(lo),
            vec![Operand::IdRef(b2_lo), Operand::IdRef(b3_lo)],
        ),
        Instruction::new(
            Op::BitwiseOr,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(hi), Operand::IdRef(lo)],
        ),
    ]
}

pub(in crate::passes) fn trailing_zero_steps(width: u32) -> Vec<(i64, u32)> {
    let mut steps = Vec::new();
    let mut shift = width / 2;
    while shift >= 1 {
        let mask = ((1u64 << shift) - 1) as i64;
        steps.push((mask, shift));
        shift /= 2;
    }
    steps
}

pub(in crate::passes) fn leading_zero_steps(width: u32) -> Vec<(i64, u32)> {
    let mut steps = Vec::new();
    let mut shift = width / 2;
    while shift >= 1 {
        let low = (1u64 << shift) - 1;
        let mask = (low << (width - shift)) as i64;
        steps.push((mask, shift));
        shift /= 2;
    }
    steps
}

pub(in crate::passes) fn ctz_scalar(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    arg: Word,
    width: u32,
) -> Vec<Instruction> {
    let bool_ty = ctx.ty_bool();
    let zero = ctx.const_int_of(rty, 0);
    let width_const = ctx.const_int_of(rty, i64::from(width));
    let is_zero = ctx.module.fresh_id();
    let mut out = vec![Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(is_zero),
        vec![Operand::IdRef(arg), Operand::IdRef(zero)],
    )];

    let mut y = arg;
    let mut count = zero;
    for (mask, shift) in trailing_zero_steps(width) {
        let mask = ctx.const_int_of(rty, mask);
        let shift = ctx.const_int_of(rty, i64::from(shift));
        let masked = ctx.module.fresh_id();
        let take = ctx.module.fresh_id();
        let shifted = ctx.module.fresh_id();
        let add = ctx.module.fresh_id();
        let next_count = ctx.module.fresh_id();
        let next_y = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(masked),
            vec![Operand::IdRef(y), Operand::IdRef(mask)],
        ));
        out.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(take),
            vec![Operand::IdRef(masked), Operand::IdRef(zero)],
        ));
        out.push(Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(shifted),
            vec![Operand::IdRef(y), Operand::IdRef(shift)],
        ));
        out.push(Instruction::new(
            Op::Select,
            Some(rty),
            Some(add),
            vec![
                Operand::IdRef(take),
                Operand::IdRef(shift),
                Operand::IdRef(zero),
            ],
        ));
        out.push(Instruction::new(
            Op::IAdd,
            Some(rty),
            Some(next_count),
            vec![Operand::IdRef(count), Operand::IdRef(add)],
        ));
        out.push(Instruction::new(
            Op::Select,
            Some(rty),
            Some(next_y),
            vec![
                Operand::IdRef(take),
                Operand::IdRef(shifted),
                Operand::IdRef(y),
            ],
        ));
        count = next_count;
        y = next_y;
    }
    out.push(Instruction::new(
        Op::Select,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(is_zero),
            Operand::IdRef(width_const),
            Operand::IdRef(count),
        ],
    ));
    out
}

pub(in crate::passes) fn clz_scalar(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    arg: Word,
    width: u32,
) -> Vec<Instruction> {
    let bool_ty = ctx.ty_bool();
    let zero = ctx.const_int_of(rty, 0);
    let width_const = ctx.const_int_of(rty, i64::from(width));
    let is_zero = ctx.module.fresh_id();
    let mut out = vec![Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(is_zero),
        vec![Operand::IdRef(arg), Operand::IdRef(zero)],
    )];

    let mut y = arg;
    let mut count = zero;
    for (mask, shift) in leading_zero_steps(width) {
        let mask = ctx.const_int_of(rty, mask);
        let shift = ctx.const_int_of(rty, i64::from(shift));
        let masked = ctx.module.fresh_id();
        let take = ctx.module.fresh_id();
        let shifted = ctx.module.fresh_id();
        let add = ctx.module.fresh_id();
        let next_count = ctx.module.fresh_id();
        let next_y = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(masked),
            vec![Operand::IdRef(y), Operand::IdRef(mask)],
        ));
        out.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(take),
            vec![Operand::IdRef(masked), Operand::IdRef(zero)],
        ));
        out.push(Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(shifted),
            vec![Operand::IdRef(y), Operand::IdRef(shift)],
        ));
        out.push(Instruction::new(
            Op::Select,
            Some(rty),
            Some(add),
            vec![
                Operand::IdRef(take),
                Operand::IdRef(shift),
                Operand::IdRef(zero),
            ],
        ));
        out.push(Instruction::new(
            Op::IAdd,
            Some(rty),
            Some(next_count),
            vec![Operand::IdRef(count), Operand::IdRef(add)],
        ));
        out.push(Instruction::new(
            Op::Select,
            Some(rty),
            Some(next_y),
            vec![
                Operand::IdRef(take),
                Operand::IdRef(shifted),
                Operand::IdRef(y),
            ],
        ));
        count = next_count;
        y = next_y;
    }
    out.push(Instruction::new(
        Op::Select,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(is_zero),
            Operand::IdRef(width_const),
            Operand::IdRef(count),
        ],
    ));
    out
}

pub(in crate::passes) fn popcount_i64_as_u32_halves(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    arg: Word,
) -> Vec<Instruction> {
    let uint = ctx.ty_uint();
    let shift = ctx.const_uint(32);
    let low = ctx.module.fresh_id();
    let shifted = ctx.module.fresh_id();
    let high = ctx.module.fresh_id();
    let low_count = ctx.module.fresh_id();
    let high_count = ctx.module.fresh_id();
    let count = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::UConvert,
            Some(uint),
            Some(low),
            vec![Operand::IdRef(arg)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(shifted),
            vec![Operand::IdRef(arg), Operand::IdRef(shift)],
        ),
        Instruction::new(
            Op::UConvert,
            Some(uint),
            Some(high),
            vec![Operand::IdRef(shifted)],
        ),
        Instruction::new(
            Op::BitCount,
            Some(uint),
            Some(low_count),
            vec![Operand::IdRef(low)],
        ),
        Instruction::new(
            Op::BitCount,
            Some(uint),
            Some(high_count),
            vec![Operand::IdRef(high)],
        ),
        Instruction::new(
            Op::IAdd,
            Some(uint),
            Some(count),
            vec![Operand::IdRef(low_count), Operand::IdRef(high_count)],
        ),
        Instruction::new(
            Op::UConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(count)],
        ),
    ]
}

pub(in crate::passes) fn popcount_i64_vector_as_u32_halves(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    arg: Word,
    lanes: u32,
) -> Vec<Instruction> {
    let uint_vec = ctx.ty_vec_uint(lanes);
    let shift_scalar = ctx.const_uint(32);
    let shift = splat(ctx, uint_vec, shift_scalar, lanes);
    let low = ctx.module.fresh_id();
    let shifted = ctx.module.fresh_id();
    let high = ctx.module.fresh_id();
    let low_count = ctx.module.fresh_id();
    let high_count = ctx.module.fresh_id();
    let count = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::UConvert,
            Some(uint_vec),
            Some(low),
            vec![Operand::IdRef(arg)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(shifted),
            vec![Operand::IdRef(arg), Operand::IdRef(shift)],
        ),
        Instruction::new(
            Op::UConvert,
            Some(uint_vec),
            Some(high),
            vec![Operand::IdRef(shifted)],
        ),
        Instruction::new(
            Op::BitCount,
            Some(uint_vec),
            Some(low_count),
            vec![Operand::IdRef(low)],
        ),
        Instruction::new(
            Op::BitCount,
            Some(uint_vec),
            Some(high_count),
            vec![Operand::IdRef(high)],
        ),
        Instruction::new(
            Op::IAdd,
            Some(uint_vec),
            Some(count),
            vec![Operand::IdRef(low_count), Operand::IdRef(high_count)],
        ),
        Instruction::new(
            Op::UConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(count)],
        ),
    ]
}
