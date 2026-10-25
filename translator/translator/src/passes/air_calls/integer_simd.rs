use super::conversions::lower_convert;
use super::*;
use spirv::GroupOperation;

pub(in crate::passes) fn lower_integer_op(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Option<Vec<Instruction>>, String> {
    if name.starts_with("air.abs_diff.") && args.len() == 2 {
        let (min_op, max_op) = if name.starts_with("air.abs_diff.u.") {
            (GLSLstd450::UMin, GLSLstd450::UMax)
        } else if name.starts_with("air.abs_diff.s.") {
            (GLSLstd450::SMin, GLSLstd450::SMax)
        } else {
            return Err(format!("unhandled abs_diff intrinsic: {name}"));
        };
        let ext = ctx.glsl();
        let lo = ctx.module.fresh_id();
        let hi = ctx.module.fresh_id();
        return Ok(Some(vec![
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(hi),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(max_op as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                ],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(lo),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(min_op as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                ],
            ),
            Instruction::new(
                Op::ISub,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(hi), Operand::IdRef(lo)],
            ),
        ]));
    }
    if name.starts_with("air.abs.") && args.len() == 1 {
        if name.starts_with("air.abs.u.") {
            return Ok(Some(vec![Instruction::new(
                Op::CopyObject,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(args[0])],
            )]));
        }
        let ext = ctx.glsl();
        return Ok(Some(vec![Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::SAbs as u32),
                Operand::IdRef(args[0]),
            ],
        )]));
    }
    if name.starts_with("llvm.bitreverse.") {
        let width = int_scalar_width(ctx, rty)
            .or_else(|| int_vector_width(ctx, rty).map(|(_, width)| width));
        if args.len() != 1 || width != Some(32) {
            return Err(format!(
                "{name} requires one 32-bit integer scalar/vector operand"
            ));
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitReverse,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name.starts_with("air.reverse_bits.") && args.len() == 1 {
        if let Some(width) = int_scalar_width(ctx, rty) {
            if width < 32 {
                let uint = ctx.ty_uint();
                let widened = ctx.module.fresh_id();
                let reversed = ctx.module.fresh_id();
                let shift = ctx.const_int_of(uint, i64::from(32 - width));
                let shifted = ctx.module.fresh_id();
                return Ok(Some(vec![
                    Instruction::new(
                        Op::UConvert,
                        Some(uint),
                        Some(widened),
                        vec![Operand::IdRef(args[0])],
                    ),
                    Instruction::new(
                        Op::BitReverse,
                        Some(uint),
                        Some(reversed),
                        vec![Operand::IdRef(widened)],
                    ),
                    Instruction::new(
                        Op::ShiftRightLogical,
                        Some(uint),
                        Some(shifted),
                        vec![Operand::IdRef(reversed), Operand::IdRef(shift)],
                    ),
                    Instruction::new(
                        Op::UConvert,
                        Some(rty),
                        Some(res),
                        vec![Operand::IdRef(shifted)],
                    ),
                ]));
            }
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitReverse,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name == "llvm.bswap.i32" && args.len() == 1 {
        if !is_int_scalar_width(ctx, rty, 32) {
            return Err("llvm.bswap.i32 result type is not i32".to_string());
        }
        return Ok(Some(byte_swap_i32(ctx, res, rty, args[0])));
    }
    if name.starts_with("air.rotate.") && args.len() == 2 {
        let width = int_scalar_width(ctx, rty)
            .ok_or_else(|| format!("{name} result type is not a scalar integer"))?;
        if width != 32 {
            return Err(format!("{name} currently supports i32 results"));
        }
        let value_ty = value_result_type(ctx, args[0])
            .ok_or_else(|| format!("{name} value operand has no result type"))?;
        let shift_ty = value_result_type(ctx, args[1])
            .ok_or_else(|| format!("{name} shift operand has no result type"))?;
        if value_ty != rty || shift_ty != rty {
            return Err(format!("{name} operand/result type mismatch"));
        }
        return Ok(Some(rotate_left_i32(ctx, res, rty, args[0], args[1])));
    }
    if name.starts_with("air.extract_bits.u.") && args.len() == 3 {
        let (lanes, width) = validate_extract_bits_shape(ctx, name, rty, args)?;
        if width == 64 {
            if lanes != 1 {
                return Err(format!("{name} currently supports 64-bit scalar results"));
            }
            return Ok(Some(extract_bits_i64(ctx, res, rty, args, false)?));
        }
        if width < 32 {
            return Ok(Some(extract_bits_narrow(ctx, res, rty, args, lanes, false)));
        }
        if width != 32 {
            return Err(format!("{name} unsupported result width {width}"));
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitFieldUExtract,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(args[0]),
                Operand::IdRef(args[1]),
                Operand::IdRef(args[2]),
            ],
        )]));
    }
    if name.starts_with("air.extract_bits.s.") && args.len() == 3 {
        let (lanes, width) = validate_extract_bits_shape(ctx, name, rty, args)?;
        if width == 64 {
            if lanes != 1 {
                return Err(format!("{name} currently supports 64-bit scalar results"));
            }
            return Ok(Some(extract_bits_i64(ctx, res, rty, args, true)?));
        }
        if width < 32 {
            return Ok(Some(extract_bits_narrow(ctx, res, rty, args, lanes, true)));
        }
        if width != 32 {
            return Err(format!("{name} unsupported result width {width}"));
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitFieldSExtract,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(args[0]),
                Operand::IdRef(args[1]),
                Operand::IdRef(args[2]),
            ],
        )]));
    }
    if name.starts_with("air.insert_bits.") && args.len() == 4 {
        let (lanes, width) = validate_insert_bits_shape(ctx, name, rty, args)?;
        if width == 64 {
            if lanes != 1 {
                return Err(format!("{name} currently supports 64-bit scalar results"));
            }
            return Ok(Some(insert_bits_i64(ctx, res, rty, args)?));
        }
        if width < 32 {
            return Ok(Some(insert_bits_narrow(ctx, res, rty, args, lanes)));
        }
        if width != 32 {
            return Err(format!("{name} unsupported result width {width}"));
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitFieldInsert,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(args[0]),
                Operand::IdRef(args[1]),
                Operand::IdRef(args[2]),
                Operand::IdRef(args[3]),
            ],
        )]));
    }
    if name.starts_with("air.popcount.") && args.len() == 1 {
        if let Some(arg_ty) = value_result_type(ctx, args[0]) {
            if is_int_scalar_width(ctx, rty, 64) && is_int_scalar_width(ctx, arg_ty, 64) {
                return Ok(Some(popcount_i64_as_u32_halves(ctx, res, rty, args[0])));
            }
            if let (Some((result_lanes, 64)), Some((arg_lanes, 64))) =
                (int_vector_width(ctx, rty), int_vector_width(ctx, arg_ty))
            {
                if result_lanes == arg_lanes {
                    return Ok(Some(popcount_i64_vector_as_u32_halves(
                        ctx,
                        res,
                        rty,
                        args[0],
                        result_lanes,
                    )));
                }
            }
            if let (Some(result_width), Some(arg_width)) =
                (int_scalar_width(ctx, rty), int_scalar_width(ctx, arg_ty))
            {
                if result_width < 32 && arg_width < 32 {
                    let uint = ctx.ty_uint();
                    let widened = ctx.module.fresh_id();
                    let counted = ctx.module.fresh_id();
                    return Ok(Some(vec![
                        Instruction::new(
                            Op::UConvert,
                            Some(uint),
                            Some(widened),
                            vec![Operand::IdRef(args[0])],
                        ),
                        Instruction::new(
                            Op::BitCount,
                            Some(uint),
                            Some(counted),
                            vec![Operand::IdRef(widened)],
                        ),
                        Instruction::new(
                            Op::UConvert,
                            Some(rty),
                            Some(res),
                            vec![Operand::IdRef(counted)],
                        ),
                    ]));
                }
            }
            if let (Some((result_lanes, result_width)), Some((arg_lanes, arg_width))) =
                (int_vector_width(ctx, rty), int_vector_width(ctx, arg_ty))
            {
                if result_lanes == arg_lanes && result_width < 32 && arg_width < 32 {
                    let uint_vec = ctx.ty_vec_uint(result_lanes);
                    let widened = ctx.module.fresh_id();
                    let counted = ctx.module.fresh_id();
                    return Ok(Some(vec![
                        Instruction::new(
                            Op::UConvert,
                            Some(uint_vec),
                            Some(widened),
                            vec![Operand::IdRef(args[0])],
                        ),
                        Instruction::new(
                            Op::BitCount,
                            Some(uint_vec),
                            Some(counted),
                            vec![Operand::IdRef(widened)],
                        ),
                        Instruction::new(
                            Op::UConvert,
                            Some(rty),
                            Some(res),
                            vec![Operand::IdRef(counted)],
                        ),
                    ]));
                }
            }
        }
        return Ok(Some(vec![Instruction::new(
            Op::BitCount,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name.starts_with("air.ctz.") && args.len() == 2 {
        let width = int_scalar_width(ctx, rty)
            .ok_or_else(|| format!("{name} result type is not a scalar integer"))?;
        if !matches!(width, 8 | 16 | 32 | 64) {
            return Err(format!("{name} unsupported result width {width}"));
        }
        let arg_ty = value_result_type(ctx, args[0])
            .ok_or_else(|| format!("{name} operand has no result type"))?;
        if arg_ty != rty {
            return Err(format!("{name} operand/result type mismatch"));
        }
        return Ok(Some(ctz_scalar(ctx, res, rty, args[0], width)));
    }
    if name.starts_with("air.clz.") && args.len() == 2 {
        let width = int_scalar_width(ctx, rty)
            .ok_or_else(|| format!("{name} result type is not a scalar integer"))?;
        if !matches!(width, 8 | 16 | 32 | 64) {
            return Err(format!("{name} unsupported result width {width}"));
        }
        let arg_ty = value_result_type(ctx, args[0])
            .ok_or_else(|| format!("{name} operand has no result type"))?;
        if arg_ty != rty {
            return Err(format!("{name} operand/result type mismatch"));
        }
        return Ok(Some(clz_scalar(ctx, res, rty, args[0], width)));
    }
    if let Some(op) = wide_multiply_op(name, args.len()) {
        if int_scalar_width(ctx, rty) != Some(32) {
            return Err(format!("{name} currently supports i32 results"));
        }
        return Ok(Some(lower_wide_multiply(ctx, res, rty, op, args)));
    }
    Ok(None)
}

#[derive(Clone, Copy)]
struct WideMultiplyOp {
    signed: bool,
    high_half: bool,
    fused: bool,
}

fn wide_multiply_op(name: &str, argc: usize) -> Option<WideMultiplyOp> {
    const MEMBERS: [(&str, bool, bool, usize); 3] = [
        ("air.mul_hi.", true, false, 2),
        ("air.mad_hi.", true, true, 3),
        ("air.mad_sat.", false, true, 3),
    ];
    let (_, high_half, fused, _) = MEMBERS
        .into_iter()
        .find(|&(stem, _, _, wanted)| argc == wanted && name.starts_with(stem))?;
    let signed = match name.split('.').nth(2)? {
        "s" => true,
        "u" => false,
        _ => return None,
    };
    Some(WideMultiplyOp {
        signed,
        high_half,
        fused,
    })
}

fn lower_wide_multiply(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    op: WideMultiplyOp,
    args: &[Word],
) -> Vec<Instruction> {
    let wide = ctx.ty_ulong();
    let (widen, narrow) = if op.signed {
        (Op::SConvert, Op::SConvert)
    } else {
        (Op::UConvert, Op::UConvert)
    };
    let mut insts = Vec::new();
    let widened = |ctx: &mut Ctx, insts: &mut Vec<Instruction>, arg: Word| {
        let id = ctx.module.fresh_id();
        insts.push(Instruction::new(
            widen,
            Some(wide),
            Some(id),
            vec![Operand::IdRef(arg)],
        ));
        id
    };
    let a = widened(ctx, &mut insts, args[0]);
    let b = widened(ctx, &mut insts, args[1]);
    let product = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IMul,
        Some(wide),
        Some(product),
        vec![Operand::IdRef(a), Operand::IdRef(b)],
    ));

    if op.high_half {
        let hi = ctx.module.fresh_id();
        let thirty_two = ctx.const_uint(32);
        insts.push(Instruction::new(
            if op.signed {
                Op::ShiftRightArithmetic
            } else {
                Op::ShiftRightLogical
            },
            Some(wide),
            Some(hi),
            vec![Operand::IdRef(product), Operand::IdRef(thirty_two)],
        ));
        let narrowed = if op.fused { ctx.module.fresh_id() } else { res };
        insts.push(Instruction::new(
            narrow,
            Some(rty),
            Some(narrowed),
            vec![Operand::IdRef(hi)],
        ));
        if op.fused {
            insts.push(Instruction::new(
                Op::IAdd,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(narrowed), Operand::IdRef(args[2])],
            ));
        }
        return insts;
    }

    let c = widened(ctx, &mut insts, args[2]);
    let sum = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(wide),
        Some(sum),
        vec![Operand::IdRef(product), Operand::IdRef(c)],
    ));

    let ext = ctx.glsl();
    let (clamp, lo, hi) = if op.signed {
        (
            GLSLstd450::SClamp,
            ctx.const_int_of(wide, i64::from(i32::MIN)),
            ctx.const_int_of(wide, i64::from(i32::MAX)),
        )
    } else {
        (
            GLSLstd450::UClamp,
            ctx.const_int_of(wide, 0),
            ctx.const_int_of(wide, i64::from(u32::MAX)),
        )
    };
    let clamped = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ExtInst,
        Some(wide),
        Some(clamped),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(clamp as u32),
            Operand::IdRef(sum),
            Operand::IdRef(lo),
            Operand::IdRef(hi),
        ],
    ));
    insts.push(Instruction::new(
        narrow,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(clamped)],
    ));
    insts
}

fn integer_shape(ctx: &Ctx, ty: Word) -> Option<(u32, u32)> {
    int_scalar_width(ctx, ty)
        .map(|width| (1, width))
        .or_else(|| int_vector_width(ctx, ty))
}

fn scalar_integer_operand(ctx: &Ctx, value: Word) -> bool {
    value_result_type(ctx, value).is_some_and(|ty| int_scalar_width(ctx, ty).is_some())
}

fn validate_extract_bits_shape(
    ctx: &Ctx,
    name: &str,
    rty: Word,
    args: &[Word],
) -> Result<(u32, u32), String> {
    let shape = integer_shape(ctx, rty)
        .ok_or_else(|| format!("{name} result is not an integer scalar or vector"))?;
    if value_result_type(ctx, args[0]) != Some(rty) {
        return Err(format!(
            "{name} base operand does not match its result type"
        ));
    }
    if !scalar_integer_operand(ctx, args[1]) || !scalar_integer_operand(ctx, args[2]) {
        return Err(format!("{name} offset and count must be integer scalars"));
    }
    Ok(shape)
}

fn validate_insert_bits_shape(
    ctx: &Ctx,
    name: &str,
    rty: Word,
    args: &[Word],
) -> Result<(u32, u32), String> {
    let shape = integer_shape(ctx, rty)
        .ok_or_else(|| format!("{name} result is not an integer scalar or vector"))?;
    if value_result_type(ctx, args[0]) != Some(rty) || value_result_type(ctx, args[1]) != Some(rty)
    {
        return Err(format!(
            "{name} base and insert operands do not match its result type"
        ));
    }
    if !scalar_integer_operand(ctx, args[2]) || !scalar_integer_operand(ctx, args[3]) {
        return Err(format!("{name} offset and count must be integer scalars"));
    }
    Ok(shape)
}

fn bitfield_widened_type(ctx: &mut Ctx, lanes: u32) -> Word {
    if lanes == 1 {
        ctx.ty_uint()
    } else {
        ctx.ty_vec_uint(lanes)
    }
}

fn extract_bits_narrow(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
    lanes: u32,
    signed: bool,
) -> Vec<Instruction> {
    let widened_type = bitfield_widened_type(ctx, lanes);
    let widened_base = ctx.module.fresh_id();
    let extracted = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::UConvert,
            Some(widened_type),
            Some(widened_base),
            vec![Operand::IdRef(args[0])],
        ),
        Instruction::new(
            if signed {
                Op::BitFieldSExtract
            } else {
                Op::BitFieldUExtract
            },
            Some(widened_type),
            Some(extracted),
            vec![
                Operand::IdRef(widened_base),
                Operand::IdRef(args[1]),
                Operand::IdRef(args[2]),
            ],
        ),
        Instruction::new(
            Op::UConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(extracted)],
        ),
    ]
}

fn insert_bits_narrow(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
    lanes: u32,
) -> Vec<Instruction> {
    let widened_type = bitfield_widened_type(ctx, lanes);
    let widened_base = ctx.module.fresh_id();
    let widened_insert = ctx.module.fresh_id();
    let inserted = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::UConvert,
            Some(widened_type),
            Some(widened_base),
            vec![Operand::IdRef(args[0])],
        ),
        Instruction::new(
            Op::UConvert,
            Some(widened_type),
            Some(widened_insert),
            vec![Operand::IdRef(args[1])],
        ),
        Instruction::new(
            Op::BitFieldInsert,
            Some(widened_type),
            Some(inserted),
            vec![
                Operand::IdRef(widened_base),
                Operand::IdRef(widened_insert),
                Operand::IdRef(args[2]),
                Operand::IdRef(args[3]),
            ],
        ),
        Instruction::new(
            Op::UConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(inserted)],
        ),
    ]
}

fn insert_bits_i64(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let _offset_ty = value_result_type(ctx, args[2])
        .filter(|ty| int_scalar_width(ctx, *ty).is_some())
        .ok_or("air.insert_bits i64 offset is not a scalar integer")?;
    let count_ty = value_result_type(ctx, args[3])
        .filter(|ty| int_scalar_width(ctx, *ty).is_some())
        .ok_or("air.insert_bits i64 count is not a scalar integer")?;
    let all_ones = ctx.const_int_of(rty, -1);
    let zero_value = ctx.const_int_of(rty, 0);
    let zero_count = ctx.const_int_of(count_ty, 0);
    let width = ctx.const_int_of(count_ty, 64);
    let remaining = ctx.module.fresh_id();
    let candidate_mask = ctx.module.fresh_id();
    let count_is_zero = ctx.module.fresh_id();
    let field_mask = ctx.module.fresh_id();
    let positioned_mask = ctx.module.fresh_id();
    let inserted_bits = ctx.module.fresh_id();
    let positioned_bits = ctx.module.fresh_id();
    let inverse_mask = ctx.module.fresh_id();
    let cleared_base = ctx.module.fresh_id();
    let bool_ty = ctx.ty_bool();
    Ok(vec![
        Instruction::new(
            Op::ISub,
            Some(count_ty),
            Some(remaining),
            vec![Operand::IdRef(width), Operand::IdRef(args[3])],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(rty),
            Some(candidate_mask),
            vec![Operand::IdRef(all_ones), Operand::IdRef(remaining)],
        ),
        Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(count_is_zero),
            vec![Operand::IdRef(args[3]), Operand::IdRef(zero_count)],
        ),
        Instruction::new(
            Op::Select,
            Some(rty),
            Some(field_mask),
            vec![
                Operand::IdRef(count_is_zero),
                Operand::IdRef(zero_value),
                Operand::IdRef(candidate_mask),
            ],
        ),
        Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(positioned_mask),
            vec![Operand::IdRef(field_mask), Operand::IdRef(args[2])],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(inserted_bits),
            vec![Operand::IdRef(args[1]), Operand::IdRef(field_mask)],
        ),
        Instruction::new(
            Op::ShiftLeftLogical,
            Some(rty),
            Some(positioned_bits),
            vec![Operand::IdRef(inserted_bits), Operand::IdRef(args[2])],
        ),
        Instruction::new(
            Op::Not,
            Some(rty),
            Some(inverse_mask),
            vec![Operand::IdRef(positioned_mask)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(rty),
            Some(cleared_base),
            vec![Operand::IdRef(args[0]), Operand::IdRef(inverse_mask)],
        ),
        Instruction::new(
            Op::BitwiseOr,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(cleared_base),
                Operand::IdRef(positioned_bits),
            ],
        ),
    ])
}

fn extract_bits_i64(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
    signed: bool,
) -> Result<Vec<Instruction>, String> {
    let offset_ty = value_result_type(ctx, args[1])
        .filter(|ty| int_scalar_width(ctx, *ty).is_some())
        .ok_or("air.extract_bits i64 offset is not a scalar integer")?;
    let count_ty = value_result_type(ctx, args[2])
        .filter(|ty| int_scalar_width(ctx, *ty).is_some())
        .ok_or("air.extract_bits i64 count is not a scalar integer")?;
    if offset_ty != count_ty {
        return Err("air.extract_bits i64 offset/count type mismatch".into());
    }
    let width = ctx.const_int_of(count_ty, 64);
    let zero_count = ctx.const_int_of(count_ty, 0);
    let zero_value = ctx.const_int_of(rty, 0);
    let count_is_zero = ctx.module.fresh_id();
    let bool_ty = ctx.ty_bool();
    let mut instructions = vec![Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(count_is_zero),
        vec![Operand::IdRef(args[2]), Operand::IdRef(zero_count)],
    )];
    let extracted = ctx.module.fresh_id();
    if signed {
        let end = ctx.module.fresh_id();
        let left_amount = ctx.module.fresh_id();
        let right_amount = ctx.module.fresh_id();
        let shifted_left = ctx.module.fresh_id();
        instructions.extend([
            Instruction::new(
                Op::IAdd,
                Some(count_ty),
                Some(end),
                vec![Operand::IdRef(args[1]), Operand::IdRef(args[2])],
            ),
            Instruction::new(
                Op::ISub,
                Some(count_ty),
                Some(left_amount),
                vec![Operand::IdRef(width), Operand::IdRef(end)],
            ),
            Instruction::new(
                Op::ISub,
                Some(count_ty),
                Some(right_amount),
                vec![Operand::IdRef(width), Operand::IdRef(args[2])],
            ),
            Instruction::new(
                Op::ShiftLeftLogical,
                Some(rty),
                Some(shifted_left),
                vec![Operand::IdRef(args[0]), Operand::IdRef(left_amount)],
            ),
            Instruction::new(
                Op::ShiftRightArithmetic,
                Some(rty),
                Some(extracted),
                vec![Operand::IdRef(shifted_left), Operand::IdRef(right_amount)],
            ),
        ]);
    } else {
        let all_ones = ctx.const_int_of(rty, -1);
        let remaining = ctx.module.fresh_id();
        let candidate_mask = ctx.module.fresh_id();
        let field_mask = ctx.module.fresh_id();
        let shifted = ctx.module.fresh_id();
        instructions.extend([
            Instruction::new(
                Op::ISub,
                Some(count_ty),
                Some(remaining),
                vec![Operand::IdRef(width), Operand::IdRef(args[2])],
            ),
            Instruction::new(
                Op::ShiftRightLogical,
                Some(rty),
                Some(candidate_mask),
                vec![Operand::IdRef(all_ones), Operand::IdRef(remaining)],
            ),
            Instruction::new(
                Op::Select,
                Some(rty),
                Some(field_mask),
                vec![
                    Operand::IdRef(count_is_zero),
                    Operand::IdRef(zero_value),
                    Operand::IdRef(candidate_mask),
                ],
            ),
            Instruction::new(
                Op::ShiftRightLogical,
                Some(rty),
                Some(shifted),
                vec![Operand::IdRef(args[0]), Operand::IdRef(args[1])],
            ),
            Instruction::new(
                Op::BitwiseAnd,
                Some(rty),
                Some(extracted),
                vec![Operand::IdRef(shifted), Operand::IdRef(field_mask)],
            ),
        ]);
    }
    instructions.push(Instruction::new(
        Op::Select,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(count_is_zero),
            Operand::IdRef(zero_value),
            Operand::IdRef(extracted),
        ],
    ));
    Ok(instructions)
}

fn lower_quad_vote(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    predicate: Word,
    all: bool,
) -> Result<Vec<Instruction>, String> {
    if !is_bool_type(ctx, rty) {
        return Err("quad vote result type is not bool".to_string());
    }
    let uint = ctx.ty_uint();
    let one = ctx.const_uint(1);
    let zero = ctx.const_uint(0);
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let word = ctx.module.fresh_id();
    let peer_one = ctx.module.fresh_id();
    let pair = ctx.module.fresh_id();
    let peer_two = ctx.module.fresh_id();
    let quad = ctx.module.fresh_id();
    let op = if all { Op::BitwiseAnd } else { Op::BitwiseOr };
    Ok(vec![
        Instruction::new(
            Op::Select,
            Some(uint),
            Some(word),
            vec![
                Operand::IdRef(predicate),
                Operand::IdRef(one),
                Operand::IdRef(zero),
            ],
        ),
        Instruction::new(
            Op::GroupNonUniformShuffleXor,
            Some(uint),
            Some(peer_one),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(word),
                Operand::IdRef(one),
            ],
        ),
        Instruction::new(
            op,
            Some(uint),
            Some(pair),
            vec![Operand::IdRef(word), Operand::IdRef(peer_one)],
        ),
        Instruction::new(
            Op::GroupNonUniformShuffleXor,
            Some(uint),
            Some(peer_two),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(pair),
                Operand::IdRef(ctx.const_uint(2)),
            ],
        ),
        Instruction::new(
            op,
            Some(uint),
            Some(quad),
            vec![Operand::IdRef(pair), Operand::IdRef(peer_two)],
        ),
        Instruction::new(
            if all { Op::IEqual } else { Op::INotEqual },
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(quad),
                Operand::IdRef(if all { one } else { zero }),
            ],
        ),
    ])
}

/// The subgroup lane of the first ACTIVE lane in this lane's 32-lane Metal SIMD-group. simd_broadcast_first and
/// simd_is_first are defined over active lanes; taking the cluster's lane 0 read an inactive lane wherever lane 0 was not
/// running (outside the primitive, a discarded pixel, a branch it did not take) and broadcast garbage: Eevee's light loop
/// took its word range from it and ran for ~10^8 iterations per pixel until the channel timed out.
fn metal_simd_first_active_lane_u32(ctx: &mut Ctx, lane: Word, insts: &mut Vec<Instruction>) -> Word {
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let truth = ctx.const_bool_of(bool_ty, true);
    let simd_lane = metal_simd_lane_local_u32(ctx, lane, insts);
    let base = metal_simd_lane_base_u32(ctx, lane, simd_lane, insts);
    // the cluster's 32 bits are one word of the ballot, because clusters are 32-lane aligned
    let word = subgroup_partition_ballot_word(ctx, insts, lane, truth);
    let ext = ctx.glsl();
    let first_bit = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ExtInst,
        Some(uint),
        Some(first_bit),
        vec![Operand::IdRef(ext), Operand::LiteralExtInstInteger(GLSLstd450::FindILsb as u32), Operand::IdRef(word)],
    ));
    let first = ctx.module.fresh_id();
    insts.push(Instruction::new(Op::IAdd, Some(uint), Some(first), vec![Operand::IdRef(base), Operand::IdRef(first_bit)]));
    first
}

fn subgroup_partition_ballot_word(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    lane: Word,
    predicate: Word,
) -> Word {
    let uint = ctx.ty_uint();
    let ballot_ty = ctx.ty_vec_uint(4);
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let ballot = ctx.module.fresh_id();
    let word_index = ctx.module.fresh_id();
    let word = ctx.module.fresh_id();
    insts.extend([
        Instruction::new(
            Op::GroupNonUniformBallot,
            Some(ballot_ty),
            Some(ballot),
            vec![Operand::IdScope(scope), Operand::IdRef(predicate)],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(uint),
            Some(word_index),
            vec![Operand::IdRef(lane), Operand::IdRef(ctx.const_uint(5))],
        ),
        Instruction::new(
            Op::VectorExtractDynamic,
            Some(uint),
            Some(word),
            vec![Operand::IdRef(ballot), Operand::IdRef(word_index)],
        ),
    ]);
    word
}

fn lower_quad_active_threads_mask(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
) -> Result<Vec<Instruction>, String> {
    if int_scalar_width(ctx, rty) != Some(16) {
        return Err("air.quad_active_threads_mask result is not i16".to_string());
    }
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let active = ctx.const_bool_of(bool_ty, true);
    let mut insts = Vec::new();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let word = subgroup_partition_ballot_word(ctx, &mut insts, lane, active);
    let quad_shift = ctx.module.fresh_id();
    let shifted = ctx.module.fresh_id();
    let masked = ctx.module.fresh_id();
    insts.extend([
        Instruction::new(
            Op::BitwiseAnd,
            Some(uint),
            Some(quad_shift),
            vec![Operand::IdRef(lane), Operand::IdRef(ctx.const_uint(28))],
        ),
        Instruction::new(
            Op::ShiftRightLogical,
            Some(uint),
            Some(shifted),
            vec![Operand::IdRef(word), Operand::IdRef(quad_shift)],
        ),
        Instruction::new(
            Op::BitwiseAnd,
            Some(uint),
            Some(masked),
            vec![Operand::IdRef(shifted), Operand::IdRef(ctx.const_uint(15))],
        ),
        Instruction::new(
            Op::UConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(masked)],
        ),
    ]);
    Ok(insts)
}

fn lower_simd_vote(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    predicate: Word,
    all: bool,
) -> Result<Vec<Instruction>, String> {
    if !is_bool_type(ctx, rty) {
        return Err("simd vote result type is not bool".to_string());
    }
    let mut insts = Vec::new();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let voted = subgroup_partition_ballot_word(ctx, &mut insts, lane, predicate);
    if all {
        let bool_ty = ctx.ty_bool();
        let active = ctx.const_bool_of(bool_ty, true);
        let active_word = subgroup_partition_ballot_word(ctx, &mut insts, lane, active);
        insts.push(Instruction::new(
            Op::IEqual,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(voted), Operand::IdRef(active_word)],
        ));
    } else {
        let zero = ctx.const_uint(0);
        insts.push(Instruction::new(
            Op::INotEqual,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(voted), Operand::IdRef(zero)],
        ));
    }
    Ok(insts)
}

fn lower_simd_ballot(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    predicate: Word,
) -> Result<Vec<Instruction>, String> {
    let width = int_scalar_width(ctx, rty);
    if width != Some(64) && width != Some(32) {
        return Err("air.simd_ballot result is not i64 or i32".to_string());
    }
    let mut insts = Vec::new();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let word = subgroup_partition_ballot_word(ctx, &mut insts, lane, predicate);
    insts.push(Instruction::new(
        if width == Some(32) {
            Op::CopyObject
        } else {
            Op::UConvert
        },
        Some(rty),
        Some(res),
        vec![Operand::IdRef(word)],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_op(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Option<Vec<Instruction>>, String> {
    if name.starts_with("air.is_uniform.") && args.len() == 1 {
        if !is_bool_type(ctx, rty) {
            return Err(format!("{name} result type is not bool"));
        }
        let asserted = ctx.const_bool_of(rty, true);
        return Ok(Some(vec![Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(asserted)],
        )]));
    }
    if matches!(name, "air.quad_all" | "air.quad_any") && args.len() == 1 {
        return lower_quad_vote(ctx, res, rty, args[0], name == "air.quad_all").map(Some);
    }
    if matches!(name, "air.simd_all" | "air.simd_any") && args.len() == 1 {
        return lower_simd_vote(ctx, res, rty, args[0], name == "air.simd_all").map(Some);
    }
    if (name == "air.simd_ballot.i64" || name == "air.simd_ballot.i32") && args.len() == 1 {
        return lower_simd_ballot(ctx, res, rty, args[0]).map(Some);
    }
    if name == "air.quad_active_threads_mask" && args.is_empty() {
        return lower_quad_active_threads_mask(ctx, res, rty).map(Some);
    }
    if name.starts_with("air.get_simdgroup_size.") && args.is_empty() {
        if int_scalar_width(ctx, rty).is_none() {
            return Err(format!("{name} result is not a scalar integer"));
        }
        let width = ctx.const_int_of(rty, 32);
        return Ok(Some(vec![Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(width)],
        )]));
    }
    if name == "air.quad_is_first" && args.is_empty() {
        if !is_bool_type(ctx, rty) {
            return Err(format!("{name} result type is not bool"));
        }
        let mut insts = Vec::new();
        let lane = subgroup_lane_index_u32(ctx, &mut insts);
        let uint = ctx.ty_uint();
        let quad_lane = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint),
            Some(quad_lane),
            vec![Operand::IdRef(lane), Operand::IdRef(ctx.const_uint(3))],
        ));
        insts.push(Instruction::new(
            Op::IEqual,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(quad_lane), Operand::IdRef(ctx.const_uint(0))],
        ));
        return Ok(Some(insts));
    }
    if name == "air.simd_is_helper_thread" && args.is_empty() {
        if !is_bool_type(ctx, rty) {
            return Err(format!("{name} result type is not bool"));
        }
        if !matches!(ctx.stage, Stage::Fragment) {
            let f = ctx.const_bool_of(rty, false);
            return Ok(Some(vec![Instruction::new(
                Op::CopyObject,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(f)],
            )]));
        }
        return Ok(Some(vec![Instruction::new(
            Op::IsHelperInvocationEXT,
            Some(rty),
            Some(res),
            vec![],
        )]));
    }
    if name == "air.simd_is_first" && args.is_empty() {
        if !is_bool_type(ctx, rty) {
            return Err(format!("{name} result type is not bool"));
        }
        let mut insts = Vec::new();
        let lane = subgroup_lane_index_u32(ctx, &mut insts);
        let first = metal_simd_first_active_lane_u32(ctx, lane, &mut insts);
        insts.push(Instruction::new(
            Op::IEqual,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(lane), Operand::IdRef(first)],
        ));
        return Ok(Some(insts));
    }
    if name.starts_with("air.simd_broadcast_first.") && args.len() == 1 {
        let scope = ctx.const_uint(Scope::Subgroup as u32);
        let mut insts = Vec::new();
        let lane = subgroup_lane_index_u32(ctx, &mut insts);
        let first = metal_simd_first_active_lane_u32(ctx, lane, &mut insts);
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffle,
            Some(rty),
            Some(res),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(args[0]),
                Operand::IdRef(first),
            ],
        ));
        return Ok(Some(insts));
    }
    if (name.starts_with("air.simd_broadcast.") || name.starts_with("air.simd_shuffle."))
        && args.len() == 2
    {
        let scope = ctx.const_uint(Scope::Subgroup as u32);
        let mut insts = Vec::new();
        let index = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
        let lane = metal_simd_absolute_lane_u32(ctx, index, &mut insts);
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffle,
            Some(rty),
            Some(res),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(args[0]),
                Operand::IdRef(lane),
            ],
        ));
        return Ok(Some(insts));
    }
    if name.starts_with("air.simd_shuffle_down.") && args.len() == 2 {
        return lower_simd_shuffle_down(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.simd_shuffle_rotate_down.") && args.len() == 2 {
        return lower_simd_shuffle_rotate_down(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.simd_shuffle_and_fill_down.") && args.len() == 4 {
        return lower_simd_shuffle_and_fill_down(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.simd_shuffle_and_fill_up.") && args.len() == 4 {
        return lower_simd_shuffle_and_fill_up(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.simd_shuffle_up.") && args.len() == 2 {
        return lower_simd_shuffle_up(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.simd_shuffle_xor.") && args.len() == 2 {
        let scope = ctx.const_uint(Scope::Subgroup as u32);
        let mut insts = Vec::new();
        let mask = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffleXor,
            Some(rty),
            Some(res),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(args[0]),
                Operand::IdRef(mask),
            ],
        ));
        return Ok(Some(insts));
    }
    if name.starts_with("air.quad_shuffle.") && args.len() == 2 {
        return lower_quad_shuffle(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.quad_broadcast.") && args.len() == 2 {
        return lower_quad_shuffle(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.quad_sum.") && args.len() == 1 {
        return lower_quad_sum(ctx, res, rty, args[0]).map(Some);
    }
    if (name.starts_with("air.quad_min.") || name.starts_with("air.quad_max.")) && args.len() == 1 {
        return lower_quad_integer_extrema(ctx, name, res, rty, args[0]).map(Some);
    }
    if name.starts_with("air.quad_shuffle_xor.") && args.len() == 2 {
        let scope = ctx.const_uint(Scope::Subgroup as u32);
        let mut insts = Vec::new();
        let mask = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffleXor,
            Some(rty),
            Some(res),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(args[0]),
                Operand::IdRef(mask),
            ],
        ));
        return Ok(Some(insts));
    }
    if name.starts_with("air.quad_shuffle_rotate_down.") && args.len() == 2 {
        return lower_quad_shuffle_rotate_down(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.quad_shuffle_up.") && args.len() == 2 {
        return lower_quad_shuffle_up(ctx, res, rty, args).map(Some);
    }
    if name.starts_with("air.quad_shuffle_down.") && args.len() == 2 {
        let scope = ctx.const_uint(Scope::Subgroup as u32);
        let mut insts = Vec::new();
        let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
        insts.push(Instruction::new(
            Op::GroupNonUniformShuffleDown,
            Some(rty),
            Some(res),
            vec![
                Operand::IdScope(scope),
                Operand::IdRef(args[0]),
                Operand::IdRef(delta),
            ],
        ));
        return Ok(Some(insts));
    }
    if name.starts_with("air.simd_prefix_exclusive_sum.") && args.len() == 1 {
        return lower_simd_sum(ctx, res, rty, args[0], GroupOperation::ExclusiveScan).map(Some);
    }
    if name.starts_with("air.simd_prefix_inclusive_sum.") && args.len() == 1 {
        return lower_simd_sum(ctx, res, rty, args[0], GroupOperation::InclusiveScan).map(Some);
    }
    if name.starts_with("air.simd_sum.") && args.len() == 1 {
        return lower_simd_sum(ctx, res, rty, args[0], GroupOperation::Reduce).map(Some);
    }
    if name.starts_with("air.simd_or.") && args.len() == 1 {
        return lower_simd_bitwise(
            ctx,
            res,
            rty,
            args[0],
            GroupOperation::Reduce,
            Op::GroupNonUniformBitwiseOr,
        )
        .map(Some);
    }
    if name.starts_with("air.simd_xor.") && args.len() == 1 {
        return lower_simd_bitwise(
            ctx,
            res,
            rty,
            args[0],
            GroupOperation::Reduce,
            Op::GroupNonUniformBitwiseXor,
        )
        .map(Some);
    }
    if name.starts_with("air.simd_and.") && args.len() == 1 {
        return lower_simd_bitwise(
            ctx,
            res,
            rty,
            args[0],
            GroupOperation::Reduce,
            Op::GroupNonUniformBitwiseAnd,
        )
        .map(Some);
    }
    if name.starts_with("air.simd_min.") && args.len() == 1 {
        return lower_simd_extrema(
            ctx,
            name,
            res,
            rty,
            args[0],
            GroupOperation::Reduce,
            SimdExtrema::Min,
        )
        .map(Some);
    }
    if name.starts_with("air.simd_max.") && args.len() == 1 {
        return lower_simd_extrema(
            ctx,
            name,
            res,
            rty,
            args[0],
            GroupOperation::Reduce,
            SimdExtrema::Max,
        )
        .map(Some);
    }
    if name.starts_with("air.all") && args.len() == 1 {
        return Ok(Some(vec![Instruction::new(
            Op::All,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name.starts_with("air.any") && args.len() == 1 {
        return Ok(Some(vec![Instruction::new(
            Op::Any,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name.contains("function_constant_predicate") && args.len() == 1 {
        return Ok(Some(vec![Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(args[0])],
        )]));
    }
    if name == "air.is_function_constant_defined" {
        if args.len() != 1 {
            return Err(format!("{name} expects one function-constant operand"));
        }
        let fc_index = ctx.module.debug_names.iter().find_map(|inst| {
            if inst.class.opcode != Op::Name {
                return None;
            }
            match (inst.operands.first(), inst.operands.get(1)) {
                (Some(Operand::IdRef(id)), Some(Operand::LiteralString(name)))
                    if *id == args[0] =>
                {
                    crate::fc_specialize::fc_init_index(name)
                }
                _ => None,
            }
        });
        let c = ctx.const_bool_of(rty, false);
        if let Some(index) = fc_index {
            ctx.module.debug_names.push(Instruction::new(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(res),
                    Operand::LiteralString(crate::fc_specialize::fc_defined_name(index)),
                ],
            ));
        }
        return Ok(Some(vec![Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(c)],
        )]));
    }
    Ok(None)
}

pub(in crate::passes) fn lower_one(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    if name.starts_with("air.") {
        match crate::air_intrinsics::air_intrinsic_disposition(name) {
            None => return Err(format!("unrecognized AIR intrinsic family: {name}")),
            Some(crate::air_intrinsics::AirIntrinsicDisposition::NoVulkanEquivalent) => {
                return Err(format!(
                    "AIR family {name} has no Vulkan equivalent; emitting the module would \
                     silently drop what the call does"
                ))
            }
            Some(crate::air_intrinsics::AirIntrinsicDisposition::RequiresResourceModel) => {
                return Err(format!(
                    "AIR family {name} requires rasterization-rate-map parameter data decoding; \
                     an unconditional identity would ignore the map data and layer"
                ))
            }
            Some(_) => {}
        }
    }
    if let Some(builtin) = cl_work_item_builtin(name) {
        return lower_cl_work_item(ctx, name, builtin, res, rty, args);
    }
    if let Some(global) = cl_size_query(name) {
        return lower_cl_size(ctx, name, global, res, rty, args);
    }
    if name == "air.get_sample_position.v2f32" {
        return lower_sample_position(ctx, res, rty, args);
    }
    if name.starts_with("air.sample_texture") {
        return lower_sample(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.gather_texture") {
        return lower_gather(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.gather_depth") {
        return lower_gather_depth(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.gather_compare_depth") {
        return lower_gather_compare_depth(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.sample_compare_depth") {
        return lower_sample_compare_depth(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.sample_depth") {
        return lower_sample_depth(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.read_depth") {
        return lower_read_depth(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.read_texture") {
        return lower_read(ctx, name, res, rty, args, v4);
    }
    if name.starts_with("air.write_texture") {
        return lower_write(ctx, name, args, v4);
    }
    if crate::air_intrinsics::is_texture_atomic(name) {
        return lower_atomic_texture(ctx, name, res, rty, args);
    }
    if name == "air.calculate_unclamped_lod_texture_2d" {
        return lower_calculate_lod_texture_2d(ctx, name, res, rty, args, 1);
    }
    if name == "air.calculate_clamped_lod_texture_2d" {
        return lower_calculate_lod_texture_2d(ctx, name, res, rty, args, 0);
    }
    if name.starts_with("air.write_imageblock_slice_to_texture") {
        return lower_imageblock_slice_write(ctx, name, args, v4);
    }
    if name.starts_with("air.store.implicit_imageblock.") {
        return lower_implicit_imageblock_store(ctx, name, args);
    }
    if name.starts_with("air.discard_fragment") {
        return Ok(vec![Instruction::new(
            Op::DemoteToHelperInvocation,
            None,
            None,
            vec![],
        )]);
    }
    if name.starts_with("air.fence_texture") {
        return lower_fence_texture(ctx, name, res, rty, args);
    }
    if name == "air.get_descriptor_size_tensor" {
        return lower_descriptor_size_tensor(ctx, name, res, rty, args);
    }
    if name == "air.atomic.global.store.i32" {
        return lower_atomic_global_store_i32(ctx, name, res, rty, args);
    }
    if name.starts_with("air.simdgroup_matrix_8x8_store.") {
        return lower_simdgroup_matrix_8x8_store(ctx, args);
    }
    if name == "air.atomic.global.max.u.i32"
        || name == "air.atomic.global.and.u.i32"
        || name == "air.atomic.global.or.u.i32"
        || name == "air.atomic.global.or.s.i32"
        || name == "air.atomic.global.and.s.i32"
        || name == "air.atomic.global.xor.s.i32"
        || name == "air.atomic.global.xor.u.i32"
        || name == "air.atomic.global.xchg.i32"
        || name == "air.atomic.local.max.s.i32"
        || name == "air.atomic.local.max.u.i32"
        || name == "air.atomic.local.min.s.i32"
        || name == "air.atomic.local.min.u.i32"
        || name == "air.atomic.local.and.u.i32"
        || name == "air.atomic.local.or.u.i32"
        || name == "air.atomic.local.xor.u.i32"
        || name == "air.atomic.local.xchg.i32"
    {
        return lower_atomic_integer_rmw(ctx, name, res, rty, args);
    }
    if is_image_size_query(name) {
        return lower_image_size_query(ctx, name, res, rty, args);
    }
    if name.starts_with("air.is_null_texture") {
        return lower_is_null_texture(ctx, name, res, rty, args);
    }
    if name.starts_with("air.get_num_mip_levels_texture")
        || name.starts_with("air.get_num_mip_levels_depth")
    {
        return lower_get_num_mip_levels(ctx, name, res, rty, args);
    }
    if crate::air_intrinsics::is_image_sample_count_query(name) {
        return lower_get_num_samples_texture(ctx, name, res, rty, args);
    }
    if name == "air.get_num_samples.i32" {
        return lower_raster_sample_count(ctx, name, res, rty);
    }
    if name == "air.get_imageblock_width" || name == "air.get_imageblock_height" {
        return lower_get_imageblock_extent(ctx, name, res, rty);
    }

    if name == "air.get_read_sampler" {
        return lower_get_read_sampler(ctx, res);
    }
    if name.starts_with("air.get_null_texture") {
        return lower_get_null_texture(ctx, name, res);
    }
    if name == "llvm.assume" {
        return Ok(Vec::new());
    }
    if name == "llvm.agx3.yield" {
        if args.len() != 1 {
            return Err(format!("{name} expects 1 operand"));
        }
        return Ok(Vec::new());
    }
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err(format!("air.* call {name} has no result")),
    };

    if name == "llvm.agx3.edgecheck" {
        return lower_agx3_edgecheck(ctx, name, res, rty, args);
    }
    if name == "llvm.agx3.igemm.v8i32.i64.i64.v8i32" {
        return lower_agx3_igemm_16x16_mac(ctx, name, res, rty, args);
    }
    if name.starts_with("air.load.implicit_imageblock.") {
        return lower_implicit_imageblock_load(ctx, name, res, rty, args);
    }
    if let Some(dimension) = agx2_matmad_dimension(name) {
        return lower_agx2_matmad(ctx, name, res, rty, args, dimension);
    }

    if name.starts_with("air.convert.") {
        return lower_convert(ctx, name, res, rty, args);
    }
    if name.starts_with("air.dfdx") || name.starts_with("air.fast_dfdx") {
        let [arg] = args else {
            return Err(format!("{name} requires one operand"));
        };
        return half_deriv(ctx, Op::DPdx, res, rty, *arg);
    }
    if name.starts_with("air.dfdy") || name.starts_with("air.fast_dfdy") {
        let [arg] = args else {
            return Err(format!("{name} requires one operand"));
        };
        return half_deriv(ctx, Op::DPdy, res, rty, *arg);
    }
    if name.starts_with("air.fwidth") {
        let [arg] = args else {
            return Err(format!("{name} requires one operand"));
        };
        return half_deriv(ctx, Op::Fwidth, res, rty, *arg);
    }
    if name.starts_with("air.dot") && args.len() == 2 {
        return lower_dot(ctx, name, res, rty, args);
    }
    if let Some(signature) = crate::air_intrinsics::matrix8_intrinsic(name) {
        return lower_simdgroup_matrix_8x8_mac(ctx, name, signature, res, rty, args);
    }
    if name.starts_with("air.simdgroup_matrix_16x16x16_multiply_accumulate.")
        || name.starts_with("air.simdgroup_matrix_16x16x16_widening_multiply_accumulate.")
    {
        return lower_simdgroup_matrix_16x16_mac(ctx, name, res, rty, args);
    }
    if name.starts_with("air.simdgroup_matrix_8x8_init_diag.") {
        return lower_simdgroup_matrix_8x8_init_diag(ctx, res, rty, args);
    }
    if name.starts_with("air.simdgroup_matrix_8x8_load.") {
        return lower_simdgroup_matrix_8x8_load(ctx, res, rty, args);
    }
    if let Some(insts) = lower_simd_op(ctx, name, res, rty, args)? {
        return Ok(insts);
    }
    if name.starts_with("air.pack.") {
        return lower_pack(ctx, name, res, rty, args);
    }
    if name.starts_with("air.unpack.unorm.rgb10a2") {
        return lower_unpack_rgb10a2(ctx, res, rty, args);
    }
    if name.starts_with("air.unpack.unorm.rg11b10f") {
        return lower_unpack_rg11b10f(ctx, res, rty, args);
    }
    if name.starts_with("air.unpack.unorm.rgb9e5") {
        return lower_unpack_rgb9e5(ctx, res, rty, args);
    }
    if name.starts_with("air.unpack.") {
        return lower_unpack(ctx, name, res, rty, args);
    }
    if let Some(insts) = lower_integer_op(ctx, name, res, rty, args)? {
        return Ok(insts);
    }
    lower_float_math(ctx, name, res, rty, args)
}

fn lower_dot(
    ctx: &Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if type_def_of(ctx, rty).is_none_or(|definition| definition.class.opcode != Op::TypeFloat) {
        return Err(format!("{name} result is not a floating-point scalar"));
    }
    let Some(left_type) = value_result_type(ctx, args[0]) else {
        return Err(format!("{name} left operand has no result type"));
    };
    let Some(right_type) = value_result_type(ctx, args[1]) else {
        return Err(format!("{name} right operand has no result type"));
    };
    let component_matches = left_type == right_type
        && vector_type_shape(ctx, left_type).is_some_and(|(component, _)| component == rty);
    if !component_matches {
        return Err(format!(
            "{name} operands are not identical vectors of its result component type"
        ));
    }
    Ok(vec![Instruction::new(
        Op::Dot,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(args[0]), Operand::IdRef(args[1])],
    )])
}

fn cl_work_item_builtin(name: &str) -> Option<BuiltIn> {
    let rest = name.strip_prefix("air.")?;
    let (stem, ty) = rest.split_once('.')?;
    if ty != "i32" {
        return None;
    }
    match stem {
        "get_global_id" => Some(BuiltIn::GlobalInvocationId),
        "get_local_id" => Some(BuiltIn::LocalInvocationId),
        "get_group_id" => Some(BuiltIn::WorkgroupId),
        "get_num_groups" => Some(BuiltIn::NumWorkgroups),
        _ => None,
    }
}

// OpenCL get_local_size / get_global_size (Apple's OpenCL hands its kernels to the Metal driver as AIR). Geekbench 6's
// OpenCL run stopped at its first workload: gemm and softmax kernels use get_local_size, the translation failed, and
// clSetKernelArg returned CL_INVALID_ARG_INDEX (-49) for a kernel that had no function. The local size is the pipeline's
// WorkgroupSize (the same spec constants [[threads_per_threadgroup]] reads); the global size is groups x local size.
fn cl_size_query(name: &str) -> Option<bool> {
    match name {
        "air.get_local_size.i32" => Some(false),
        "air.get_global_size.i32" => Some(true),
        _ => None,
    }
}

fn lower_cl_size(
    ctx: &mut Ctx,
    name: &str,
    global: bool,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let (Some(res), Some(rty), Some(&dim)) = (res, rty, args.first()) else {
        return Err(format!("{name}: expected one dimension operand and a result"));
    };
    let uint = ctx.ty_uint();
    let v3 = ctx.ty_vec_uint(3);
    let size = ctx.kernel_workgroup_size_id();
    let mut insts = Vec::new();
    let vector = if global {
        let groups = match existing_builtin_input_var(ctx, BuiltIn::NumWorkgroups, v3) {
            Some(var) => var,
            None => {
                let ptr_ty = ctx.ty_ptr(StorageClass::Input, v3);
                let var = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Variable,
                    Some(ptr_ty),
                    Some(var),
                    vec![Operand::StorageClass(StorageClass::Input)],
                ));
                ctx.module.annotations.push(Instruction::new(
                    Op::Decorate,
                    None,
                    None,
                    vec![
                        Operand::IdRef(var),
                        Operand::Decoration(Decoration::BuiltIn),
                        Operand::BuiltIn(BuiltIn::NumWorkgroups),
                    ],
                ));
                ctx.interface.push(var);
                var
            }
        };
        let loaded = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::Load, Some(v3), Some(loaded), vec![Operand::IdRef(groups)]));
        let product = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::IMul,
            Some(v3),
            Some(product),
            vec![Operand::IdRef(loaded), Operand::IdRef(size)],
        ));
        product
    } else {
        size
    };
    if rty == uint {
        insts.push(Instruction::new(
            Op::VectorExtractDynamic,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(vector), Operand::IdRef(dim)],
        ));
    } else {
        let lane = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::VectorExtractDynamic,
            Some(uint),
            Some(lane),
            vec![Operand::IdRef(vector), Operand::IdRef(dim)],
        ));
        insts.push(Instruction::new(Op::Bitcast, Some(rty), Some(res), vec![Operand::IdRef(lane)]));
    }
    Ok(insts)
}

fn lower_cl_work_item(
    ctx: &mut Ctx,
    name: &str,
    builtin: BuiltIn,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let (Some(res), Some(rty), Some(&dim)) = (res, rty, args.first()) else {
        return Err(format!("{name}: expected one dimension operand and a result"));
    };
    let uint = ctx.ty_uint();
    let v3 = ctx.ty_vec_uint(3);
    let var = match existing_builtin_input_var(ctx, builtin, v3) {
        Some(var) => var,
        None => {
            let ptr_ty = ctx.ty_ptr(StorageClass::Input, v3);
            let var = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::Variable,
                Some(ptr_ty),
                Some(var),
                vec![Operand::StorageClass(StorageClass::Input)],
            ));
            ctx.module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(var), Operand::Decoration(Decoration::BuiltIn), Operand::BuiltIn(builtin)],
            ));
            ctx.interface.push(var);
            var
        }
    };
    let loaded = ctx.module.fresh_id();
    let mut insts = vec![Instruction::new(Op::Load, Some(v3), Some(loaded), vec![Operand::IdRef(var)])];
    if rty == uint {
        insts.push(Instruction::new(
            Op::VectorExtractDynamic,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(loaded), Operand::IdRef(dim)],
        ));
    } else {
        let lane = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::VectorExtractDynamic,
            Some(uint),
            Some(lane),
            vec![Operand::IdRef(loaded), Operand::IdRef(dim)],
        ));
        insts.push(Instruction::new(Op::Bitcast, Some(rty), Some(res), vec![Operand::IdRef(lane)]));
    }
    Ok(insts)
}
