use super::*;

pub(in crate::passes) fn is_llvm_bfloat_fmuladd(name: &str) -> bool {
    name.starts_with("llvm.fmuladd.") && name.contains("bf16")
}

pub(in crate::passes) fn lower_bfloat_fmuladd(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 3 {
        return Err(format!("{name} expects 3 operands"));
    }
    if !is_u16_scalar(ctx, rty) {
        return Err(format!(
            "{name} currently expects a scalar u16 bfloat storage result"
        ));
    }
    for arg in args {
        let arg_ty = value_result_type(ctx, *arg)
            .ok_or_else(|| format!("{name} has an operand with no result type"))?;
        if !is_u16_scalar(ctx, arg_ty) {
            return Err(format!(
                "{name} currently expects scalar u16 bfloat storage operands"
            ));
        }
    }

    let mut out = vec![];
    let a = widen_bf16_to_f32(ctx, &mut out, args[0], 1);
    let b = widen_bf16_to_f32(ctx, &mut out, args[1], 1);
    let c = widen_bf16_to_f32(ctx, &mut out, args[2], 1);
    let fma = ctx.module.fresh_id();
    let ext = ctx.glsl();
    let float = ctx.ty_float();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float),
        Some(fma),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Fma as u32),
            Operand::IdRef(a),
            Operand::IdRef(b),
            Operand::IdRef(c),
        ],
    ));
    narrow_f32_to_bf16(ctx, &mut out, fma, 1, rty, res);
    Ok(out)
}

pub(super) fn widen_bf16_to_f32(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    bits: Word,
    n: u32,
) -> Word {
    let u32_ty = ty_u32_shaped(ctx, n);
    let f32_ty = ty_f32_shaped(ctx, n);
    let widened = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(u32_ty),
        Some(widened),
        vec![Operand::IdRef(bits)],
    ));
    let shamt = shift_amount_16(ctx, n);
    let shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(u32_ty),
        Some(shifted),
        vec![Operand::IdRef(widened), Operand::IdRef(shamt)],
    ));
    let f = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Bitcast,
        Some(f32_ty),
        Some(f),
        vec![Operand::IdRef(shifted)],
    ));
    f
}

pub(super) fn narrow_f32_to_bf16(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    f32val: Word,
    n: u32,
    rty: Word,
    res: Word,
) {
    let u32_ty = ty_u32_shaped(ctx, n);
    let as_u32 = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Bitcast,
        Some(u32_ty),
        Some(as_u32),
        vec![Operand::IdRef(f32val)],
    ));
    let shamt = shift_amount_16(ctx, n);
    let high = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(u32_ty),
        Some(high),
        vec![Operand::IdRef(as_u32), Operand::IdRef(shamt)],
    ));
    let one = shaped_u32_const(ctx, n, 1);
    let lsb = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(u32_ty),
        Some(lsb),
        vec![Operand::IdRef(high), Operand::IdRef(one)],
    ));
    let bias_base = shaped_u32_const(ctx, n, 0x7fff);
    let bias = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(u32_ty),
        Some(bias),
        vec![Operand::IdRef(bias_base), Operand::IdRef(lsb)],
    ));
    let rounded = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(u32_ty),
        Some(rounded),
        vec![Operand::IdRef(as_u32), Operand::IdRef(bias)],
    ));
    let shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(u32_ty),
        Some(shifted),
        vec![Operand::IdRef(rounded), Operand::IdRef(shamt)],
    ));
    let shifted = select_canonical_bfloat_nan_bits(ctx, out, as_u32, shifted, n);
    out.push(Instruction::new(
        Op::UConvert,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(shifted)],
    ));
}

fn select_canonical_bfloat_nan_bits(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    bits: Word,
    narrowed: Word,
    n: u32,
) -> Word {
    let u32_ty = ty_u32_shaped(ctx, n);
    let bool_ty = ty_bool_shaped(ctx, n);
    let exp_mask = shaped_u32_const(ctx, n, 0x7f80_0000);
    let mant_mask = shaped_u32_const(ctx, n, 0x007f_ffff);
    let zero = shaped_u32_const(ctx, n, 0);

    let exp_bits = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(u32_ty),
        Some(exp_bits),
        vec![Operand::IdRef(bits), Operand::IdRef(exp_mask)],
    ));
    let exp_all_ones = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(exp_all_ones),
        vec![Operand::IdRef(exp_bits), Operand::IdRef(exp_mask)],
    ));

    let mant_bits = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(u32_ty),
        Some(mant_bits),
        vec![Operand::IdRef(bits), Operand::IdRef(mant_mask)],
    ));
    let mant_nonzero = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::INotEqual,
        Some(bool_ty),
        Some(mant_nonzero),
        vec![Operand::IdRef(mant_bits), Operand::IdRef(zero)],
    ));

    let is_nan = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::LogicalAnd,
        Some(bool_ty),
        Some(is_nan),
        vec![Operand::IdRef(exp_all_ones), Operand::IdRef(mant_nonzero)],
    ));

    let canonical_nan = shaped_u32_const(ctx, n, 0x7fc0);
    let selected = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(u32_ty),
        Some(selected),
        vec![
            Operand::IdRef(is_nan),
            Operand::IdRef(canonical_nan),
            Operand::IdRef(narrowed),
        ],
    ));
    selected
}

pub(in crate::passes) fn nan_aware_if_precise(name: &str, op: GLSLstd450) -> GLSLstd450 {
    use spirv::GlslStd450Op::*;
    if name.starts_with("air.fast_") || name.starts_with("air.native_") {
        return op;
    }
    match op {
        FMin => NMin,
        FMax => NMax,
        FClamp => NClamp,
        other => other,
    }
}

pub(in crate::passes) fn glsl_extinst(name: &str) -> Option<GLSLstd450> {
    use spirv::GlslStd450Op::*;
    let op = if is_air_math(name, "atan2") {
        Atan2
    } else if is_air_math(name, "asin") {
        Asin
    } else if is_air_math(name, "acos") {
        Acos
    } else if is_air_math(name, "atan") {
        Atan
    } else if name.starts_with("llvm.maxnum.")
        || is_air_math(name, "fmax")
        || is_air_math(name, "max")
    {
        FMax
    } else if name.starts_with("llvm.minnum.")
        || is_air_math(name, "fmin")
        || is_air_math(name, "min")
    {
        FMin
    } else if is_air_math(name, "sqrt") {
        Sqrt
    } else if is_air_math(name, "rsqrt") {
        InverseSqrt
    } else if name.starts_with("llvm.fabs.") || is_air_math(name, "fabs") {
        FAbs
    } else if is_air_math(name, "pow") || is_air_math(name, "powr") {
        Pow
    } else if is_air_math(name, "sign") {
        FSign
    } else if is_air_math(name, "mix") {
        FMix
    } else if is_air_math(name, "floor") {
        Floor
    } else if is_air_math(name, "ceil") {
        Ceil
    } else if is_air_math(name, "round") {
        Round
    } else if is_air_math(name, "rint") {
        RoundEven
    } else if is_air_math(name, "trunc") {
        Trunc
    } else if is_air_math(name, "tan") {
        Tan
    } else if is_air_math(name, "sinh") {
        Sinh
    } else if is_air_math(name, "cosh") {
        Cosh
    } else if is_air_math(name, "tanh") {
        Tanh
    } else if is_air_math(name, "asinh") {
        Asinh
    } else if is_air_math(name, "acosh") {
        Acosh
    } else if is_air_math(name, "atanh") {
        Atanh
    } else if is_air_math(name, "sin") {
        Sin
    } else if is_air_math(name, "cos") {
        Cos
    } else if is_air_math(name, "exp2") {
        Exp2
    } else if is_air_math(name, "exp") {
        Exp
    } else if is_air_math(name, "log2") {
        Log2
    } else if is_air_math(name, "log") {
        Log
    } else if is_air_math(name, "fract") {
        Fract
    } else if is_air_math(name, "fma")
        || name.starts_with("llvm.fmuladd.")
        || (is_air_math(name, "mad") && (name.ends_with("f32") || name.ends_with("f16")))
    {
        Fma
    } else if is_air_math(name, "clamp") {
        FClamp
    } else {
        return None;
    };
    Some(nan_aware_if_precise(name, op))
}

pub(in crate::passes) fn is_air_math(name: &str, stem: &str) -> bool {
    let Some(rest) = name.strip_prefix("air.") else {
        return false;
    };
    let rest = rest
        .strip_prefix("fast_")
        .or_else(|| rest.strip_prefix("native_"))
        .unwrap_or(rest);
    rest.starts_with(stem) && rest.as_bytes().get(stem.len()) == Some(&b'.')
}

pub(in crate::passes) type GLSLstd450 = spirv::GlslStd450Op;
