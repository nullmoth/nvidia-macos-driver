use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageblockConversion {
    None,
    Float,
    Bitcast,
}

pub(in crate::passes) fn lower_implicit_imageblock_load(
    ctx: &mut Ctx,
    name: &str,
    result: Word,
    result_ty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 4 {
        return Err(format!(
            "{name} expects attachment, coordinate, index, and data rate"
        ));
    }
    let (attachment, data_rate) = implicit_imageblock_constants(ctx, name, args)?;
    let (format, comp, storage_ty, conversion, lanes) =
        implicit_imageblock_format(ctx, name, result_ty)?;
    let (var, image_ty) = ctx.implicit_imageblock_var(attachment, data_rate, format, comp)?;
    let mut out = Vec::new();
    let image = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(image_ty),
        Some(image),
        vec![Operand::IdRef(var)],
    ));
    ctx.image_dims.insert(image, (Dim::Dim2D, true));
    ctx.image_comp.insert(image, comp);
    ctx.image_storage.insert(image);
    let coord = build_fetch_coord(ctx, Dim::Dim2D, true, args[1], Some(args[2]), &mut out)?;
    let read_result = if conversion != ImageblockConversion::None || lanes != 4 {
        ctx.module.fresh_id()
    } else {
        result
    };
    out.push(Instruction::new(
        Op::ImageRead,
        Some(storage_ty),
        Some(read_result),
        vec![Operand::IdRef(image), Operand::IdRef(coord)],
    ));
    let (converted_source, converted_source_ty) = if lanes == 1 {
        let component = if conversion != ImageblockConversion::None {
            ctx.module.fresh_id()
        } else {
            result
        };
        let component_ty = match comp {
            ImageComp::Float => ctx.ty_float(),
            ImageComp::Uint => ctx.ty_uint(),
            ImageComp::Sint => ctx.ty_sint(),
        };
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(component_ty),
            Some(component),
            vec![Operand::IdRef(read_result), Operand::LiteralBit32(0)],
        ));
        (component, component_ty)
    } else if lanes < 4 {
        let prefix = ctx.module.fresh_id();
        let prefix_ty = match comp {
            ImageComp::Float => ctx.ty_vecf(lanes),
            ImageComp::Uint => ctx.ty_vec_uint(lanes),
            ImageComp::Sint => ctx.ty_vec_sint(lanes),
        };
        out.push(Instruction::new(
            Op::VectorShuffle,
            Some(prefix_ty),
            Some(prefix),
            std::iter::once(Operand::IdRef(read_result))
                .chain(std::iter::once(Operand::IdRef(read_result)))
                .chain((0..lanes).map(Operand::LiteralBit32))
                .collect(),
        ));
        (prefix, prefix_ty)
    } else {
        (read_result, storage_ty)
    };
    if conversion != ImageblockConversion::None {
        let instruction = match conversion {
            ImageblockConversion::Float => Instruction::new(
                Op::FConvert,
                Some(result_ty),
                Some(result),
                vec![Operand::IdRef(converted_source)],
            ),
            ImageblockConversion::Bitcast => {
                copy_or_bitcast_result(result_ty, result, converted_source_ty, converted_source)
            }
            ImageblockConversion::None => unreachable!(),
        };
        out.push(instruction);
    }
    Ok(out)
}

pub(in crate::passes) fn lower_implicit_imageblock_store(
    ctx: &mut Ctx,
    name: &str,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 5 {
        return Err(format!(
            "{name} expects value, attachment, coordinate, index, and data rate"
        ));
    }
    let abi_args = &args[1..];
    let (attachment, data_rate) = implicit_imageblock_constants(ctx, name, abi_args)?;
    let value_ty = value_result_type(ctx, args[0])
        .ok_or_else(|| format!("{name} value has no result type"))?;
    let (format, comp, storage_ty, conversion, lanes) =
        implicit_imageblock_format(ctx, name, value_ty)?;
    let (var, image_ty) = ctx.implicit_imageblock_var(attachment, data_rate, format, comp)?;
    let mut out = Vec::new();
    let image = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(image_ty),
        Some(image),
        vec![Operand::IdRef(var)],
    ));
    ctx.image_dims.insert(image, (Dim::Dim2D, true));
    ctx.image_comp.insert(image, comp);
    ctx.image_storage.insert(image);
    let coord = build_fetch_coord(
        ctx,
        Dim::Dim2D,
        true,
        abi_args[1],
        Some(abi_args[2]),
        &mut out,
    )?;
    let value = if conversion != ImageblockConversion::None {
        let storage_value_ty = if lanes == 1 {
            match conversion {
                ImageblockConversion::Float => ctx.ty_float(),
                ImageblockConversion::Bitcast => match comp {
                    ImageComp::Float => ctx.ty_float(),
                    ImageComp::Uint => ctx.ty_uint(),
                    ImageComp::Sint => ctx.ty_sint(),
                },
                ImageblockConversion::None => unreachable!(),
            }
        } else if lanes < 4 {
            match comp {
                ImageComp::Float => ctx.ty_vecf(lanes),
                ImageComp::Uint => ctx.ty_vec_uint(lanes),
                ImageComp::Sint => ctx.ty_vec_sint(lanes),
            }
        } else {
            storage_ty
        };
        let converted = ctx.module.fresh_id();
        let instruction = match conversion {
            ImageblockConversion::Float => Instruction::new(
                Op::FConvert,
                Some(storage_value_ty),
                Some(converted),
                vec![Operand::IdRef(args[0])],
            ),
            ImageblockConversion::Bitcast => {
                copy_or_bitcast_result(storage_value_ty, converted, value_ty, args[0])
            }
            ImageblockConversion::None => unreachable!(),
        };
        out.push(instruction);
        converted
    } else {
        args[0]
    };
    let value = if lanes == 1 {
        let texel = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(storage_ty),
            Some(texel),
            vec![Operand::IdRef(value); 4],
        ));
        texel
    } else if lanes < 4 {
        let texel = ctx.module.fresh_id();
        let operands = std::iter::once(Operand::IdRef(value))
            .chain(std::iter::once(Operand::IdRef(value)))
            .chain(
                (0..4)
                    .map(|lane| Operand::LiteralBit32(if lane < lanes { lane } else { u32::MAX })),
            )
            .collect();
        out.push(Instruction::new(
            Op::VectorShuffle,
            Some(storage_ty),
            Some(texel),
            operands,
        ));
        texel
    } else {
        value
    };
    out.push(Instruction::new(
        Op::ImageWrite,
        None,
        None,
        vec![
            Operand::IdRef(image),
            Operand::IdRef(coord),
            Operand::IdRef(value),
        ],
    ));
    Ok(out)
}

fn implicit_imageblock_constants(
    ctx: &Ctx,
    name: &str,
    args: &[Word],
) -> Result<(u32, u32), String> {
    let attachment = constant_u32(ctx, args[0])
        .ok_or_else(|| format!("{name} requires a constant render-target attachment"))?;
    let data_rate = constant_u32(ctx, args[3])
        .ok_or_else(|| format!("{name} requires a constant imageblock data rate"))?;
    Ok((attachment, data_rate))
}

fn implicit_imageblock_format(
    ctx: &mut Ctx,
    name: &str,
    value_ty: Word,
) -> Result<(ImageFormat, ImageComp, Word, ImageblockConversion, u32), String> {
    use crate::meta::TextureFormat;

    let format = crate::meta::implicit_imageblock_texture_format(name)?
        .ok_or_else(|| format!("{name} is not an implicit imageblock intrinsic"))?;
    let lowered = match format {
        TextureFormat::R16f => {
            require_storage_image_extended_formats(ctx);
            (
                ImageFormat::R16f,
                ImageComp::Float,
                ctx.ty_vecf(4),
                ImageblockConversion::Float,
                1,
            )
        }
        TextureFormat::Rg16f => {
            require_storage_image_extended_formats(ctx);
            (
                ImageFormat::Rg16f,
                ImageComp::Float,
                ctx.ty_vecf(4),
                ImageblockConversion::Float,
                2,
            )
        }
        TextureFormat::Rgba16f => (
            ImageFormat::Rgba16f,
            ImageComp::Float,
            ctx.ty_vecf(4),
            ImageblockConversion::Float,
            4,
        ),
        TextureFormat::R32f => {
            require_storage_image_extended_formats(ctx);
            (
                ImageFormat::R32f,
                ImageComp::Float,
                ctx.ty_vecf(4),
                ImageblockConversion::None,
                1,
            )
        }
        TextureFormat::Rgba32f => (
            ImageFormat::Rgba32f,
            ImageComp::Float,
            value_ty,
            ImageblockConversion::None,
            4,
        ),
        TextureFormat::R32ui => {
            require_storage_image_extended_formats(ctx);
            (
                ImageFormat::R32ui,
                ImageComp::Uint,
                ctx.ty_vec_uint(4),
                ImageblockConversion::Bitcast,
                1,
            )
        }
        _ => return Err(format!("{name} has unsupported implicit imageblock format")),
    };
    Ok(lowered)
}

fn require_storage_image_extended_formats(ctx: &mut Ctx) {
    let capability = spirv::Capability::StorageImageExtendedFormats;
    if ctx
        .module
        .capabilities
        .iter()
        .any(|instruction| instruction.operands.as_slice() == [Operand::Capability(capability)])
    {
        return;
    }
    ctx.module.capabilities.push(Instruction::new(
        Op::Capability,
        None,
        None,
        vec![Operand::Capability(capability)],
    ));
}

pub(in crate::passes) fn lower_float_math(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if (name.starts_with("air.fast_sincos.") || name.starts_with("air.sincos.")) && args.len() == 2
    {
        let ext = ctx.glsl();
        let cos = ctx.module.fresh_id();
        return Ok(vec![
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(res),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(GLSLstd450::Sin as u32),
                    Operand::IdRef(args[0]),
                ],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(cos),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(GLSLstd450::Cos as u32),
                    Operand::IdRef(args[0]),
                ],
            ),
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(args[1]), Operand::IdRef(cos)],
            ),
        ]);
    }
    if (name.starts_with("air.cospi.") || name.starts_with("air.fast_cospi.")) && args.len() == 1 {
        return lower_metal_pi_scaled_sin_cos(ctx, res, rty, args[0], PiScaled::Cos);
    }
    if (name.starts_with("air.sinpi.") || name.starts_with("air.fast_sinpi.")) && args.len() == 1 {
        return lower_metal_pi_scaled_sin_cos(ctx, res, rty, args[0], PiScaled::Sin);
    }
    if (name.starts_with("air.tanpi.") || name.starts_with("air.fast_tanpi.")) && args.len() == 1 {
        return lower_metal_pi_scaled_sin_cos(ctx, res, rty, args[0], PiScaled::Tan);
    }
    if (name.starts_with("air.exp10.") || name.starts_with("air.fast_exp10.")) && args.len() == 1 {
        let ext = ctx.glsl();
        let n = vector_len(ctx, rty);
        let ten = splat_or_scalar(ctx, rty, 10.0, n);
        return Ok(vec![Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Pow as u32),
                Operand::IdRef(ten),
                Operand::IdRef(args[0]),
            ],
        )]);
    }
    if name.starts_with("air.fast_fmod.") || name.starts_with("air.fmod.") {
        if args.len() != 2 {
            return Err(format!("{name} expects two operands"));
        }
        let ext = ctx.glsl();
        let quotient = ctx.module.fresh_id();
        let truncated = ctx.module.fresh_id();
        let product = ctx.module.fresh_id();
        return Ok(vec![
            Instruction::new(
                Op::FDiv,
                Some(rty),
                Some(quotient),
                vec![Operand::IdRef(args[0]), Operand::IdRef(args[1])],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(truncated),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(GLSLstd450::Trunc as u32),
                    Operand::IdRef(quotient),
                ],
            ),
            Instruction::new(
                Op::FMul,
                Some(rty),
                Some(product),
                vec![Operand::IdRef(args[1]), Operand::IdRef(truncated)],
            ),
            Instruction::new(
                Op::FSub,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(args[0]), Operand::IdRef(product)],
            ),
        ]);
    }
    if (name.starts_with("air.fast_log10.") || name.starts_with("air.log10.")) && args.len() == 1 {
        return lower_post_scaled_glsl_unary(
            ctx,
            res,
            rty,
            args[0],
            std::f32::consts::LOG10_2,
            GLSLstd450::Log2,
        );
    }
    if args.len() == 2 {
        let int_minmax = if name.starts_with("air.min.s.") {
            Some(GLSLstd450::SMin)
        } else if name.starts_with("air.min.u.") {
            Some(GLSLstd450::UMin)
        } else if name.starts_with("air.max.s.") {
            Some(GLSLstd450::SMax)
        } else if name.starts_with("air.max.u.") {
            Some(GLSLstd450::UMax)
        } else {
            None
        };
        if let Some(op) = int_minmax {
            if matches!(op, GLSLstd450::SMin | GLSLstd450::SMax) {
                return lower_signed_integer_minmax(ctx, res, rty, args, op);
            }
            let ext = ctx.glsl();
            return Ok(vec![Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(res),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(op as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                ],
            )]);
        }
    }
    if args.len() == 3 {
        let int_clamp = if name.starts_with("air.clamp.s.") {
            Some(GLSLstd450::SClamp)
        } else if name.starts_with("air.clamp.u.") {
            Some(GLSLstd450::UClamp)
        } else {
            None
        };
        if let Some(op) = int_clamp {
            let ext = ctx.glsl();
            return Ok(vec![Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(res),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(op as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                    Operand::IdRef(args[2]),
                ],
            )]);
        }
    }
    if args.len() == 3 {
        let ternary_minmax = if name.contains("fmax3") {
            Some(GLSLstd450::FMax)
        } else if name.contains("fmin3") {
            Some(GLSLstd450::FMin)
        } else if name.starts_with("air.max3.u.") {
            Some(GLSLstd450::UMax)
        } else if name.starts_with("air.min3.u.") {
            Some(GLSLstd450::UMin)
        } else if name.starts_with("air.max3.s.") {
            Some(GLSLstd450::SMax)
        } else if name.starts_with("air.min3.s.") {
            Some(GLSLstd450::SMin)
        } else {
            None
        }
        .map(|op| nan_aware_if_precise(name, op));
        if let Some(op) = ternary_minmax {
            let ext = ctx.glsl();
            let tmp = ctx.module.fresh_id();
            return Ok(vec![
                Instruction::new(
                    Op::ExtInst,
                    Some(rty),
                    Some(tmp),
                    vec![
                        Operand::IdRef(ext),
                        Operand::LiteralExtInstInteger(op as u32),
                        Operand::IdRef(args[0]),
                        Operand::IdRef(args[1]),
                    ],
                ),
                Instruction::new(
                    Op::ExtInst,
                    Some(rty),
                    Some(res),
                    vec![
                        Operand::IdRef(ext),
                        Operand::LiteralExtInstInteger(op as u32),
                        Operand::IdRef(tmp),
                        Operand::IdRef(args[2]),
                    ],
                ),
            ]);
        }
    }
    if name.contains("fmedian3") && args.len() == 3 {
        let ext = ctx.glsl();
        let fmin = nan_aware_if_precise(name, GLSLstd450::FMin);
        let fmax = nan_aware_if_precise(name, GLSLstd450::FMax);
        let min_ab = ctx.module.fresh_id();
        let max_ab = ctx.module.fresh_id();
        let min_max_ab_c = ctx.module.fresh_id();
        return Ok(vec![
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(min_ab),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(fmin as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                ],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(max_ab),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(fmax as u32),
                    Operand::IdRef(args[0]),
                    Operand::IdRef(args[1]),
                ],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(min_max_ab_c),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(fmin as u32),
                    Operand::IdRef(max_ab),
                    Operand::IdRef(args[2]),
                ],
            ),
            Instruction::new(
                Op::ExtInst,
                Some(rty),
                Some(res),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(fmax as u32),
                    Operand::IdRef(min_ab),
                    Operand::IdRef(min_max_ab_c),
                ],
            ),
        ]);
    }
    if is_llvm_bfloat_fmuladd(name) {
        return lower_bfloat_fmuladd(ctx, name, res, rty, args);
    }
    if is_air_math(name, "ldexp") && args.len() == 2 {
        return lower_fast_ldexp(ctx, name, res, rty, args);
    }
    if name.starts_with("air.fast_tanh.") && args.len() == 1 {
        return lower_fast_tanh(ctx, res, rty, args[0]);
    }
    let trig_flush = std::env::var("NVMTL_XLATE_TRIG_FLUSH")
        .map(|v| v == "1")
        .unwrap_or(false);
    if trig_flush
        && name.starts_with("air.fast_cos.")
        && args.len() == 1
        && is_f32_scalar_or_vector(ctx, rty)
    {
        return Ok(lower_fast_trig(ctx, res, rty, args[0], GLSLstd450::Cos));
    }
    if trig_flush
        && name.starts_with("air.fast_sin.")
        && args.len() == 1
        && is_f32_scalar_or_vector(ctx, rty)
    {
        return Ok(lower_fast_trig(ctx, res, rty, args[0], GLSLstd450::Sin));
    }
    if let Some(glsl_op) = glsl_extinst(name) {
        if args.len() == 1
            && glsl_op == GLSLstd450::Round
            && (is_f32_scalar_or_vector(ctx, rty) || is_half_scalar_or_vector(ctx, rty))
        {
            return Ok(lower_metal_round(ctx, res, rty, args[0]));
        }
        if args.len() == 1 && matches!(glsl_op, GLSLstd450::Round | GLSLstd450::RoundEven) {
            return Ok(half_glsl_op(ctx, glsl_op, res, rty, args));
        }
        if matches!(glsl_op, GLSLstd450::Tanh | GLSLstd450::Atan2)
            && !name.starts_with("air.fast_")
            && args.len() == usize::from(glsl_op == GLSLstd450::Atan2) + 1
        {
            return Ok(half_glsl_op(ctx, glsl_op, res, rty, args));
        }
        if args.len() == 2
            && is_air_math(name, "pow")
            && (is_half_scalar_or_vector(ctx, rty) || is_f32_scalar_or_vector(ctx, rty))
        {
            return Ok(lower_metal_pow(ctx, res, rty, args[0], args[1]));
        }
        let ext = ctx.glsl();
        let mut ops = vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(glsl_op as u32),
        ];
        for a in args {
            ops.push(Operand::IdRef(*a));
        }
        return Ok(vec![Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(res),
            ops,
        )]);
    }
    if name.contains("saturate") && args.len() == 1 {
        let ext = ctx.glsl();
        let clamp = nan_aware_if_precise(name, GLSLstd450::FClamp);
        let (zero, one) = scalar_zero_one(ctx, rty);
        let (lo, hi) = clamp_edges(ctx, rty, zero, one);
        return Ok(vec![Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(clamp as u32),
                Operand::IdRef(args[0]),
                Operand::IdRef(lo),
                Operand::IdRef(hi),
            ],
        )]);
    }

    Err(format!("unhandled air.* intrinsic: {name}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PiScaled {
    Sin,
    Cos,
    Tan,
}

fn lower_metal_pi_scaled_sin_cos(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    x: Word,
    which: PiScaled,
) -> Result<Vec<Instruction>, String> {
    let float_ty = float_equivalent(ctx, rty);
    match type_def_of(ctx, float_ty) {
        Some(def) if def.class.opcode == Op::TypeFloat => {}
        _ => return Err("pi-scaled sine/cosine currently supports scalar float only".to_string()),
    }
    let ext = ctx.glsl();
    let bool_ty = ctx.ty_bool();
    let half = ctx.const_float(0.5);
    let two = ctx.const_float(2.0);
    let pi = ctx.const_float(std::f32::consts::PI);
    let zero = ctx.const_float(0.0);
    let one = ctx.const_float(1.0);
    let minus_one = ctx.const_float(-1.0);
    let infinity = ctx.const_float(f32::INFINITY);
    let quiet_nan = ctx.const_float(f32::from_bits(0x7fc0_0000));
    let mut out = Vec::new();

    let emit =
        |ctx: &mut Ctx, out: &mut Vec<Instruction>, op: Op, operands: Vec<Operand>| -> Word {
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(op, Some(float_ty), Some(id), operands));
            id
        };
    let xf = if float_ty == rty {
        x
    } else {
        emit(ctx, &mut out, Op::FConvert, vec![Operand::IdRef(x)])
    };

    let round_even = |ctx: &mut Ctx, out: &mut Vec<Instruction>, value: Word| -> Word {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(id),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::RoundEven as u32),
                Operand::IdRef(value),
            ],
        ));
        id
    };

    let scaled = emit(
        ctx,
        &mut out,
        Op::FMul,
        vec![Operand::IdRef(xf), Operand::IdRef(half)],
    );
    let whole = round_even(ctx, &mut out, scaled);
    let doubled = emit(
        ctx,
        &mut out,
        Op::FMul,
        vec![Operand::IdRef(whole), Operand::IdRef(two)],
    );
    let reduced = emit(
        ctx,
        &mut out,
        Op::FSub,
        vec![Operand::IdRef(xf), Operand::IdRef(doubled)],
    );
    let twice = emit(
        ctx,
        &mut out,
        Op::FMul,
        vec![Operand::IdRef(reduced), Operand::IdRef(two)],
    );
    let quadrant = round_even(ctx, &mut out, twice);
    let offset = emit(
        ctx,
        &mut out,
        Op::FMul,
        vec![Operand::IdRef(quadrant), Operand::IdRef(half)],
    );
    let residue = emit(
        ctx,
        &mut out,
        Op::FSub,
        vec![Operand::IdRef(reduced), Operand::IdRef(offset)],
    );
    let angle = emit(
        ctx,
        &mut out,
        Op::FMul,
        vec![Operand::IdRef(pi), Operand::IdRef(residue)],
    );

    let sine = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(sine),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Sin as u32),
            Operand::IdRef(angle),
        ],
    ));
    let cosine = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(cosine),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Cos as u32),
            Operand::IdRef(angle),
        ],
    ));

    let compare =
        |ctx: &mut Ctx, out: &mut Vec<Instruction>, quadrant: Word, against: Word| -> Word {
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FOrdEqual,
                Some(bool_ty),
                Some(id),
                vec![Operand::IdRef(quadrant), Operand::IdRef(against)],
            ));
            id
        };
    let at_zero = compare(ctx, &mut out, quadrant, zero);
    let at_one = compare(ctx, &mut out, quadrant, one);
    let at_minus_one = compare(ctx, &mut out, quadrant, minus_one);

    let sine_arms = |ctx: &mut Ctx, out: &mut Vec<Instruction>| -> (Word, Word, Word, Word) {
        let negated_sine = emit(ctx, out, Op::FNegate, vec![Operand::IdRef(sine)]);
        let negated_cosine = emit(ctx, out, Op::FNegate, vec![Operand::IdRef(cosine)]);
        (sine, cosine, negated_cosine, negated_sine)
    };
    let cosine_arms = |ctx: &mut Ctx, out: &mut Vec<Instruction>| -> (Word, Word, Word, Word) {
        let negated_sine = emit(ctx, out, Op::FNegate, vec![Operand::IdRef(sine)]);
        let at_axis = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(at_axis),
            vec![Operand::IdRef(residue), Operand::IdRef(zero)],
        ));
        let signed_zero = emit(
            ctx,
            out,
            Op::Select,
            vec![
                Operand::IdRef(at_axis),
                Operand::IdRef(zero),
                Operand::IdRef(negated_sine),
            ],
        );
        let negated_cosine = emit(ctx, out, Op::FNegate, vec![Operand::IdRef(cosine)]);
        (cosine, signed_zero, sine, negated_cosine)
    };
    let pick_quadrant = |ctx: &mut Ctx,
                         out: &mut Vec<Instruction>,
                         (arm_zero, arm_one, arm_minus_one, arm_two): (Word, Word, Word, Word)|
     -> Word {
        let inner = emit(
            ctx,
            out,
            Op::Select,
            vec![
                Operand::IdRef(at_minus_one),
                Operand::IdRef(arm_minus_one),
                Operand::IdRef(arm_two),
            ],
        );
        let middle = emit(
            ctx,
            out,
            Op::Select,
            vec![
                Operand::IdRef(at_one),
                Operand::IdRef(arm_one),
                Operand::IdRef(inner),
            ],
        );
        emit(
            ctx,
            out,
            Op::Select,
            vec![
                Operand::IdRef(at_zero),
                Operand::IdRef(arm_zero),
                Operand::IdRef(middle),
            ],
        )
    };
    let selected = match which {
        PiScaled::Sin => {
            let arms = sine_arms(ctx, &mut out);
            pick_quadrant(ctx, &mut out, arms)
        }
        PiScaled::Cos => {
            let arms = cosine_arms(ctx, &mut out);
            pick_quadrant(ctx, &mut out, arms)
        }
        PiScaled::Tan => {
            let sin_arms = sine_arms(ctx, &mut out);
            let numerator = pick_quadrant(ctx, &mut out, sin_arms);
            let cos_arms = cosine_arms(ctx, &mut out);
            let denominator = pick_quadrant(ctx, &mut out, cos_arms);
            emit(
                ctx,
                &mut out,
                Op::FDiv,
                vec![Operand::IdRef(numerator), Operand::IdRef(denominator)],
            )
        }
    };

    let magnitude = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(magnitude),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::FAbs as u32),
            Operand::IdRef(xf),
        ],
    ));
    let finite = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FOrdLessThan,
        Some(bool_ty),
        Some(finite),
        vec![Operand::IdRef(magnitude), Operand::IdRef(infinity)],
    ));
    let guarded = if float_ty == rty {
        res
    } else {
        ctx.module.fresh_id()
    };
    out.push(Instruction::new(
        Op::Select,
        Some(float_ty),
        Some(guarded),
        vec![
            Operand::IdRef(finite),
            Operand::IdRef(selected),
            Operand::IdRef(quiet_nan),
        ],
    ));
    if float_ty != rty {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(guarded)],
        ));
    }
    Ok(out)
}

fn lower_metal_round(ctx: &mut Ctx, res: Word, rty: Word, x: Word) -> Vec<Instruction> {
    let float_ty = float_equivalent(ctx, rty);
    let lanes = vector_len(ctx, float_ty);
    let bool_ty = if lanes == 1 {
        ctx.ty_bool()
    } else {
        ctx.ty_vec_bool(lanes)
    };
    let ext = ctx.glsl();
    let up_edge = splat_or_scalar(ctx, float_ty, 0.5, lanes);
    let down_edge = splat_or_scalar(ctx, float_ty, -0.5, lanes);
    let plus_one = splat_or_scalar(ctx, float_ty, 1.0, lanes);
    let minus_one = splat_or_scalar(ctx, float_ty, -1.0, lanes);
    let zero = splat_or_scalar(ctx, float_ty, 0.0, lanes);
    let mut out = Vec::new();
    let xf = if float_ty == rty {
        x
    } else {
        let widened = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(float_ty),
            Some(widened),
            vec![Operand::IdRef(x)],
        ));
        widened
    };
    let truncated = ctx.module.fresh_id();
    let remainder = ctx.module.fresh_id();
    let reaches_up = ctx.module.fresh_id();
    let reaches_down = ctx.module.fresh_id();
    let down_or_zero = ctx.module.fresh_id();
    let adjust = ctx.module.fresh_id();
    let rounded = if float_ty == rty {
        res
    } else {
        ctx.module.fresh_id()
    };
    out.extend([
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(truncated),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Trunc as u32),
                Operand::IdRef(xf),
            ],
        ),
        Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(remainder),
            vec![Operand::IdRef(xf), Operand::IdRef(truncated)],
        ),
        Instruction::new(
            Op::FOrdGreaterThanEqual,
            Some(bool_ty),
            Some(reaches_up),
            vec![Operand::IdRef(remainder), Operand::IdRef(up_edge)],
        ),
        Instruction::new(
            Op::FOrdLessThanEqual,
            Some(bool_ty),
            Some(reaches_down),
            vec![Operand::IdRef(remainder), Operand::IdRef(down_edge)],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(down_or_zero),
            vec![
                Operand::IdRef(reaches_down),
                Operand::IdRef(minus_one),
                Operand::IdRef(zero),
            ],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(adjust),
            vec![
                Operand::IdRef(reaches_up),
                Operand::IdRef(plus_one),
                Operand::IdRef(down_or_zero),
            ],
        ),
        Instruction::new(
            Op::FAdd,
            Some(float_ty),
            Some(rounded),
            vec![Operand::IdRef(truncated), Operand::IdRef(adjust)],
        ),
    ]);
    if float_ty != rty {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(rounded)],
        ));
    }
    out
}

fn lower_fast_trig(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    x: Word,
    op: GLSLstd450,
) -> Vec<Instruction> {
    let ext = ctx.glsl();
    let lanes = vector_len(ctx, rty);
    let bool_ty = if lanes == 1 {
        ctx.ty_bool()
    } else {
        ctx.ty_vec_bool(lanes)
    };
    let zero = ctx.const_float(0.0);
    let threshold = ctx.const_float(6_588_397.5);
    let (zero, threshold) = clamp_edges(ctx, rty, zero, threshold);
    let abs = ctx.module.fresh_id();
    let raw = ctx.module.fresh_id();
    let too_large = ctx.module.fresh_id();
    vec![
        Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(abs),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::FAbs as u32),
                Operand::IdRef(x),
            ],
        ),
        Instruction::new(
            Op::FUnordGreaterThan,
            Some(bool_ty),
            Some(too_large),
            vec![Operand::IdRef(abs), Operand::IdRef(threshold)],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(raw),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(op as u32),
                Operand::IdRef(x),
            ],
        ),
        Instruction::new(
            Op::Select,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(too_large),
                Operand::IdRef(zero),
                Operand::IdRef(raw),
            ],
        ),
    ]
}

pub(in crate::passes) fn lower_signed_integer_minmax(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
    op: GLSLstd450,
) -> Result<Vec<Instruction>, String> {
    let signed_ty =
        signed_integer_type_like(ctx, rty).ok_or("air signed integer min/max result is not int")?;
    let mut out = Vec::new();
    let lhs = bitcast_integer_to_type(ctx, &mut out, args[0], signed_ty)?;
    let rhs = bitcast_integer_to_type(ctx, &mut out, args[1], signed_ty)?;
    let signed_res = if signed_ty == rty {
        res
    } else {
        ctx.module.fresh_id()
    };
    let ext = ctx.glsl();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(signed_ty),
        Some(signed_res),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(op as u32),
            Operand::IdRef(lhs),
            Operand::IdRef(rhs),
        ],
    ));
    if signed_res != res {
        out.push(Instruction::new(
            Op::Bitcast,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(signed_res)],
        ));
    }
    Ok(out)
}

pub(in crate::passes) fn bitcast_integer_to_type(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    target_ty: Word,
) -> Result<Word, String> {
    let value_ty = value_result_type(ctx, value).ok_or("air signed min/max arg has no type")?;
    if value_ty == target_ty {
        return Ok(value);
    }
    let value_shape =
        integer_shape(ctx, value_ty).ok_or("air signed min/max arg is not integer-shaped")?;
    let target_shape =
        integer_shape(ctx, target_ty).ok_or("air signed min/max target is not integer-shaped")?;
    if value_shape != target_shape {
        return Err("air signed min/max arg shape does not match result".into());
    }
    let cast = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Bitcast,
        Some(target_ty),
        Some(cast),
        vec![Operand::IdRef(value)],
    ));
    Ok(cast)
}

pub(in crate::passes) fn signed_integer_type_like(ctx: &mut Ctx, ty: Word) -> Option<Word> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeInt => {
            let bits = match def.operands.first()? {
                Operand::LiteralBit32(bits) => *bits,
                _ => return None,
            };
            Some(signed_integer_scalar_type(ctx, bits))
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
            let signed_elem = signed_integer_type_like(ctx, elem)?;
            Some(integer_vector_type(ctx, signed_elem, lanes))
        }
        _ => None,
    }
}

pub(in crate::passes) fn signed_integer_scalar_type(ctx: &mut Ctx, bits: u32) -> Word {
    for inst in ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
    {
        if inst.class.opcode == Op::TypeInt
            && inst.operands.first() == Some(&Operand::LiteralBit32(bits))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(1))
        {
            if let Some(id) = inst.result_id {
                return id;
            }
        }
    }
    let id = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeInt,
        id,
        vec![Operand::LiteralBit32(bits), Operand::LiteralBit32(1)],
    ));
    id
}

pub(in crate::passes) fn integer_vector_type(ctx: &mut Ctx, elem: Word, lanes: u32) -> Word {
    for inst in ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
    {
        if inst.class.opcode == Op::TypeVector
            && inst.operands.first() == Some(&Operand::IdRef(elem))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(lanes))
        {
            if let Some(id) = inst.result_id {
                return id;
            }
        }
    }
    let id = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeVector,
        id,
        vec![Operand::IdRef(elem), Operand::LiteralBit32(lanes)],
    ));
    id
}

use crate::passes::air_calls::images::integer_shape;

pub(in crate::passes) fn imageblock_region_is_explicit(ctx: &Ctx, args: &[Word]) -> bool {
    let has_size_flag = value_def_instruction(ctx, args[2]).map(|def| def.class.opcode);
    let explicit_size_def = value_def_instruction(ctx, args[4]).map(|def| def.class.opcode);
    let explicit_usable = !matches!(explicit_size_def, Some(Op::Undef) | None);
    explicit_usable && !matches!(has_size_flag, Some(Op::ConstantFalse))
}

pub(in crate::passes) fn static_imageblock_region(ctx: &Ctx, args: &[Word]) -> Option<[u32; 2]> {
    if imageblock_region_is_explicit(ctx, args) {
        return constant_uvec2_components(ctx, args[4]);
    }
    let scale = emitted_tile_cell_scale(ctx, *args.get(1)?)?;
    Some([
        ctx.kernel_local_size[0].checked_mul(scale)?,
        ctx.kernel_local_size[1].checked_mul(scale)?,
    ])
}

fn emitted_tile_cell_scale(ctx: &Ctx, cell_pointer: Word) -> Option<u32> {
    let (_, cell_index, _) = imageblock_cell_chain(ctx, cell_pointer).ok()?;
    let Some(width) = imageblock_row_stride(ctx, cell_index) else {
        return Some(1);
    };
    let def = value_def_instruction(ctx, width)?;
    if def.class.opcode != Op::IMul {
        return Some(1);
    }
    let Some(Operand::IdRef(scale)) = def.operands.get(1) else {
        return None;
    };
    match value_def_instruction(ctx, *scale)?.operands.first() {
        Some(Operand::LiteralBit32(literal)) => Some(*literal),
        _ => None,
    }
}

fn imageblock_slice_region_is_one_texel(ctx: &Ctx, args: &[Word]) -> Result<(), String> {
    match static_imageblock_region(ctx, args) {
        Some([width, height]) if width * height <= 1 => Ok(()),
        Some([width, height]) => Err(format!(
            "air.write_imageblock_slice_to_texture copies a {width}x{height} block of imageblock \
             cells, and this translator stages one cell per invocation; emitting the module would \
             write a single texel where Metal writes the block"
        )),
        None => Err(
            "air.write_imageblock_slice_to_texture carries a runtime block extent, and this \
             translator stages one cell per invocation; emitting the module would write a single \
             texel where Metal writes the block"
                .into(),
        ),
    }
}

pub(in crate::passes) fn constant_uvec2_components(ctx: &Ctx, value: Word) -> Option<[u32; 2]> {
    let def = value_def_instruction(ctx, value)?;
    match def.class.opcode {
        Op::ConstantNull => Some([0, 0]),
        Op::ConstantComposite => {
            let mut out = [0u32; 2];
            for (slot, operand) in out.iter_mut().zip(def.operands.iter()) {
                let Operand::IdRef(component) = operand else {
                    return None;
                };
                let component = value_def_instruction(ctx, *component)?;
                match (component.class.opcode, component.operands.first()) {
                    (Op::Constant, Some(Operand::LiteralBit32(literal))) => *slot = *literal,
                    (Op::ConstantNull, _) => *slot = 0,
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

pub(in crate::passes) fn lower_imageblock_slice_write(
    ctx: &mut Ctx,
    name: &str,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    if args.len() < 6 {
        return Err("air.write_imageblock_slice_to_texture missing operands".into());
    }
    imageblock_slice_region_is_one_texel(ctx, args)?;
    let ptr_ty = value_result_type(ctx, args[1])
        .ok_or("air.write_imageblock_slice_to_texture pointer has no result type")?;
    let texel_ty = pointer_pointee_type(ctx, ptr_ty)
        .ok_or("air.write_imageblock_slice_to_texture pointer is not typed")?;
    let storage = ptr_storage(&type_defs(&ctx.module), ptr_ty).unwrap_or(StorageClass::Private);
    let write_texel_ty = imageblock_slice_texel_type(ctx, name, v4).unwrap_or(texel_ty);
    let texel_ptr = if write_texel_ty == texel_ty {
        args[1]
    } else if let Some((stored_ty, path)) =
        imageblock_other_float_width_subobject(ctx, texel_ty, write_texel_ty)
    {
        // The call name carries the texture's write type (`.v4f32`), but the imageblock may
        // store its cells at the other float width: MPS Lanczos keeps `half4` cells and writes
        // them to a float texture. Metal converts the cell to the pixel on the write, so this
        // loads the cell as stored and widens or narrows it, instead of refusing the view.
        let mut out = Vec::new();
        let stored_ptr = if path.is_empty() {
            args[1]
        } else {
            let retyped_ptr_ty = ctx.ty_ptr(storage, stored_ty);
            let retyped = ctx.module.fresh_id();
            let mut operands = Vec::with_capacity(path.len() + 1);
            operands.push(Operand::IdRef(args[1]));
            operands.extend(
                path.into_iter()
                    .map(|index| Operand::IdRef(ctx.const_uint(index))),
            );
            out.push(Instruction::new(
                Op::InBoundsAccessChain,
                Some(retyped_ptr_ty),
                Some(retyped),
                operands,
            ));
            retyped
        };
        let stored = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(stored_ty),
            Some(stored),
            vec![Operand::IdRef(stored_ptr)],
        ));
        let texel = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(write_texel_ty),
            Some(texel),
            vec![Operand::IdRef(stored)],
        ));
        return lower_imageblock_slice_write_texel(ctx, name, args, v4, texel, write_texel_ty, out);
    } else {
        let path = imageblock_zero_offset_subobject_path(ctx, texel_ty, write_texel_ty)
            .ok_or_else(|| {
                format!(
                    "air.write_imageblock_slice_to_texture cannot view pointee type %{texel_ty} \
                     as texel type %{write_texel_ty} through a zero-offset aggregate field"
                )
            })?;
        let retyped_ptr_ty = ctx.ty_ptr(storage, write_texel_ty);
        let retyped = ctx.module.fresh_id();
        let mut operands = Vec::with_capacity(path.len() + 1);
        operands.push(Operand::IdRef(args[1]));
        operands.extend(
            path.into_iter()
                .map(|index| Operand::IdRef(ctx.const_uint(index))),
        );
        let mut out = vec![Instruction::new(
            Op::InBoundsAccessChain,
            Some(retyped_ptr_ty),
            Some(retyped),
            operands,
        )];
        let texel = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(write_texel_ty),
            Some(texel),
            vec![Operand::IdRef(retyped)],
        ));
        return lower_imageblock_slice_write_texel(ctx, name, args, v4, texel, write_texel_ty, out);
    };
    let texel = ctx.module.fresh_id();
    let out = vec![Instruction::new(
        Op::Load,
        Some(write_texel_ty),
        Some(texel),
        vec![Operand::IdRef(texel_ptr)],
    )];
    lower_imageblock_slice_write_texel(ctx, name, args, v4, texel, write_texel_ty, out)
}

/// The zero-offset subobject of `source` that has `target`'s lane count at the other float
/// width (half <-> float), with its access path. None when the shapes are not that pair.
fn imageblock_other_float_width_subobject(
    ctx: &mut Ctx,
    source: Word,
    target: Word,
) -> Option<(Word, Vec<u32>)> {
    let (target_elem, lanes) = vector_type_shape(ctx, target).unwrap_or((target, 1));
    let other = if is_f32_scalar(ctx, target_elem) {
        if lanes == 1 { ctx.ty_half() } else { ctx.ty_vech(lanes) }
    } else if is_half_scalar(ctx, target_elem) {
        if lanes == 1 { ctx.ty_float() } else { ctx.ty_vecf(lanes) }
    } else {
        return None;
    };
    let path = imageblock_zero_offset_subobject_path(ctx, source, other)?;
    Some((other, path))
}

pub(in crate::passes) fn imageblock_zero_offset_subobject_path(
    ctx: &Ctx,
    source: Word,
    target: Word,
) -> Option<Vec<u32>> {
    if source == target {
        return Some(Vec::new());
    }
    let source_def = type_def_of(ctx, source)?;
    let first_child = match source_def.class.opcode {
        Op::TypeStruct | Op::TypeArray => match source_def.operands.first() {
            Some(Operand::IdRef(child)) => *child,
            _ => return None,
        },
        _ => return None,
    };
    let mut path = imageblock_zero_offset_subobject_path(ctx, first_child, target)?;
    path.insert(0, 0);
    Some(path)
}

pub(in crate::passes) fn lower_imageblock_slice_write_texel(
    ctx: &mut Ctx,
    name: &str,
    args: &[Word],
    v4: Word,
    texel: Word,
    texel_ty: Word,
    mut out: Vec<Instruction>,
) -> Result<Vec<Instruction>, String> {
    let mut img = resolve_image_value(ctx, args[0]);
    if !image_is_storage(ctx, img) {
        img = recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Storage)
            .ok_or_else(|| {
                format!("air.write_imageblock_slice_to_texture on non-storage image id {img}")
            })?;
    }
    ctx.require_runtime_storage_image_use(img, RuntimeStorageImageUse::Write)?;
    let (dim, arrayed) = ctx
        .image_dims
        .get(&img)
        .copied()
        .unwrap_or((Dim::Dim2D, false));
    let layer = if arrayed {
        Some(
            args.get(6)
                .copied()
                .ok_or("air.write_imageblock_slice_to_texture array texture missing layer")?,
        )
    } else {
        None
    };
    let coord32 = build_fetch_coord(ctx, dim, arrayed, args[5], layer, &mut out)?;
    let region_gate =
        gate_imageblock_region_in_bounds(ctx, args, img, dim, arrayed, coord32, &mut out)?;
    let comp = ctx
        .image_comp
        .get(&img)
        .copied()
        .unwrap_or(ImageComp::Float);
    let (elem, lanes) = vector_type_shape(ctx, texel_ty).unwrap_or((texel_ty, 1));
    let texel32 = if comp != ImageComp::Float {
        build_int_write_texel(ctx, comp, texel, elem, lanes, &mut out)?
    } else if lanes == 4 {
        if is_f32_scalar(ctx, elem) {
            texel
        } else if is_half_scalar(ctx, elem) {
            let converted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FConvert,
                Some(v4),
                Some(converted),
                vec![Operand::IdRef(texel)],
            ));
            converted
        } else {
            return Err(
                "air.write_imageblock_slice_to_texture: unsupported texel component".into(),
            );
        }
    } else if lanes == 1 {
        let f32_ty = ctx.ty_float();
        let scalar32 = if is_f32_scalar(ctx, elem) {
            texel
        } else if is_half_scalar(ctx, elem) {
            let converted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FConvert,
                Some(f32_ty),
                Some(converted),
                vec![Operand::IdRef(texel)],
            ));
            converted
        } else {
            return Err(
                "air.write_imageblock_slice_to_texture: unsupported texel component".into(),
            );
        };
        let zero = ctx.const_float(0.0);
        let padded = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(v4),
            Some(padded),
            vec![
                Operand::IdRef(scalar32),
                Operand::IdRef(zero),
                Operand::IdRef(zero),
                Operand::IdRef(zero),
            ],
        ));
        padded
    } else if lanes == 2 || lanes == 3 {
        let f32_ty = ctx.ty_float();
        let src32 = if is_f32_scalar(ctx, elem) {
            texel
        } else if is_half_scalar(ctx, elem) {
            let fvec = ctx.ty_vecf(lanes);
            let converted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FConvert,
                Some(fvec),
                Some(converted),
                vec![Operand::IdRef(texel)],
            ));
            converted
        } else {
            return Err(
                "air.write_imageblock_slice_to_texture: unsupported texel component".into(),
            );
        };
        let zero = ctx.const_float(0.0);
        let mut comps = Vec::with_capacity(4);
        for i in 0..lanes {
            let c = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::CompositeExtract,
                Some(f32_ty),
                Some(c),
                vec![Operand::IdRef(src32), Operand::LiteralBit32(i)],
            ));
            comps.push(Operand::IdRef(c));
        }
        for _ in lanes..4 {
            comps.push(Operand::IdRef(zero));
        }
        let padded = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(v4),
            Some(padded),
            comps,
        ));
        padded
    } else {
        return Err("air.write_imageblock_slice_to_texture: texel is not v4".into());
    };
    let texel32_ty = match comp {
        ImageComp::Float => v4,
        ImageComp::Sint => ctx.ty_vec_sint(4),
        ImageComp::Uint => ctx.ty_vec_uint(4),
    };
    let texel32 = zero_texel_for_empty_imageblock_region(
        ctx,
        texel32,
        texel32_ty,
        region_gate.empty,
        &mut out,
    )?;
    out.push(Instruction::new(
        Op::ImageWrite,
        None,
        None,
        vec![
            Operand::IdRef(img),
            Operand::IdRef(region_gate.coord),
            Operand::IdRef(texel32),
        ],
    ));
    Ok(out)
}

fn zero_texel_for_empty_imageblock_region(
    ctx: &mut Ctx,
    texel: Word,
    texel_ty: Word,
    region_empty: Word,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let zero_texel = ctx.get_or_create(Op::ConstantNull, Some(texel_ty), vec![]);
    let selected = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(texel_ty),
        Some(selected),
        vec![
            Operand::IdRef(region_empty),
            Operand::IdRef(zero_texel),
            Operand::IdRef(texel),
        ],
    ));
    Ok(selected)
}

pub(in crate::passes) fn build_int_write_texel(
    ctx: &mut Ctx,
    comp: ImageComp,
    texel: Word,
    elem: Word,
    lanes: u32,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    if lanes != 4 {
        return Err(
            "air.write_imageblock_slice_to_texture: non-float imageblock write with non-v4 texel"
                .into(),
        );
    }
    let (v4int_ty, signed) = match comp {
        ImageComp::Sint => (ctx.ty_vec_sint(4), true),
        ImageComp::Uint => (ctx.ty_vec_uint(4), false),
        ImageComp::Float => {
            return Err("build_int_write_texel called for a float image".into());
        }
    };
    let already32 = (signed && is_int_scalar_width(ctx, elem, 32))
        || (!signed && is_uint_scalar_width(ctx, elem, 32));
    if already32 {
        return Ok(texel);
    }
    let op = if signed { Op::SConvert } else { Op::UConvert };
    let converted = ctx.module.fresh_id();
    out.push(Instruction::new(
        op,
        Some(v4int_ty),
        Some(converted),
        vec![Operand::IdRef(texel)],
    ));
    Ok(converted)
}

struct ImageblockRegionGate {
    coord: Word,
    empty: Word,
}

fn gate_imageblock_region_in_bounds(
    ctx: &mut Ctx,
    args: &[Word],
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord32: Word,
    out: &mut Vec<Instruction>,
) -> Result<ImageblockRegionGate, String> {
    let spatial: u32 = match dim {
        Dim::Dim1D | Dim::DimBuffer => 1,
        Dim::Dim3D => 3,
        _ => 2,
    };
    let ncomp = spatial + u32::from(arrayed);
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let size_ty = if ncomp == 1 {
        uint
    } else {
        ctx.ty_vec_uint(ncomp)
    };
    let dims = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ImageQuerySize,
        Some(size_ty),
        Some(dims),
        vec![Operand::IdRef(img)],
    ));
    let extract = |ctx: &mut Ctx, out: &mut Vec<Instruction>, composite: Word, index: u32| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(id),
            vec![Operand::IdRef(composite), Operand::LiteralBit32(index)],
        ));
        id
    };
    let has_size_flag = value_def_instruction(ctx, args[2]).map(|def| def.class.opcode);
    let local_size = ctx.kernel_local_size_ids();
    let implicit = [local_size[0], local_size[1]];
    let explicit = if imageblock_region_is_explicit(ctx, args) {
        let src_ty = value_result_type(ctx, args[4])
            .ok_or("air.write_imageblock_slice_to_texture: size operand has no type")?;
        let wide = if scalar_bit_width(ctx, src_ty) == 32 {
            args[4]
        } else {
            let uint2 = ctx.ty_vec_uint(2);
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::UConvert,
                Some(uint2),
                Some(id),
                vec![Operand::IdRef(args[4])],
            ));
            id
        };
        Some([extract(ctx, out, wide, 0), extract(ctx, out, wide, 1)])
    } else {
        None
    };
    let mut region = implicit;
    if let Some(explicit) = explicit {
        match has_size_flag {
            Some(Op::ConstantTrue) => region = explicit,
            Some(Op::ConstantFalse) => {}
            _ => {
                for axis in 0..2 {
                    let id = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Select,
                        Some(uint),
                        Some(id),
                        vec![
                            Operand::IdRef(args[2]),
                            Operand::IdRef(explicit[axis]),
                            Operand::IdRef(implicit[axis]),
                        ],
                    ));
                    region[axis] = id;
                }
            }
        }
    }
    let zero = ctx.const_uint(0);
    let mut fits: Option<Word> = None;
    let mut empty: Option<Word> = None;
    for axis in 0..spatial.min(2) {
        let origin = if ncomp == 1 {
            coord32
        } else {
            extract(ctx, out, coord32, axis)
        };
        let dim_axis = if ncomp == 1 {
            dims
        } else {
            extract(ctx, out, dims, axis)
        };
        let end = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IAdd,
            Some(uint),
            Some(end),
            vec![
                Operand::IdRef(origin),
                Operand::IdRef(region[axis as usize]),
            ],
        ));
        let axis_empty = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(axis_empty),
            vec![Operand::IdRef(region[axis as usize]), Operand::IdRef(zero)],
        ));
        let axis_fits = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ULessThanEqual,
            Some(bool_ty),
            Some(axis_fits),
            vec![Operand::IdRef(end), Operand::IdRef(dim_axis)],
        ));
        fits = Some(match fits {
            None => axis_fits,
            Some(prev) => {
                let both = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::LogicalAnd,
                    Some(bool_ty),
                    Some(both),
                    vec![Operand::IdRef(prev), Operand::IdRef(axis_fits)],
                ));
                both
            }
        });
        empty = Some(match empty {
            None => axis_empty,
            Some(prev) => {
                let either = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::LogicalOr,
                    Some(bool_ty),
                    Some(either),
                    vec![Operand::IdRef(prev), Operand::IdRef(axis_empty)],
                ));
                either
            }
        });
    }
    let fits = fits.ok_or("gate_imageblock_region_in_bounds: at least one spatial axis")?;
    let empty = empty.ok_or("gate_imageblock_region_in_bounds: at least one spatial axis")?;
    let all_ones = ctx.const_uint(u32::MAX);
    let mask = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(mask),
        vec![
            Operand::IdRef(fits),
            Operand::IdRef(zero),
            Operand::IdRef(all_ones),
        ],
    ));
    let (mask_vec, coord_ty) = if ncomp == 1 {
        (mask, uint)
    } else {
        let vec_ty = ctx.ty_vec_uint(ncomp);
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(vec_ty),
            Some(id),
            vec![Operand::IdRef(mask); ncomp as usize],
        ));
        (id, vec_ty)
    };
    let gated = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(coord_ty),
        Some(gated),
        vec![Operand::IdRef(coord32), Operand::IdRef(mask_vec)],
    ));
    Ok(ImageblockRegionGate {
        coord: gated,
        empty,
    })
}

pub(in crate::passes) fn imageblock_slice_texel_type(
    ctx: &mut Ctx,
    name: &str,
    v4: Word,
) -> Option<Word> {
    let suffix = name.rsplit('.').next()?;
    let (lanes, elem) = match suffix.strip_prefix('v') {
        Some(rest) => {
            let split = rest.find('f')?;
            (rest[..split].parse::<u32>().ok()?, &rest[split..])
        }
        None => (1, suffix),
    };
    if !(1..=4).contains(&lanes) {
        return None;
    }
    let half = match elem {
        "f16" => true,
        "f32" => false,
        _ => return None,
    };
    Some(match (half, lanes) {
        (true, 1) => ctx.ty_half(),
        (true, n) => ctx.ty_vech(n),
        (false, 1) => ctx.ty_float(),
        (false, 4) => v4,
        (false, n) => ctx.ty_vecf(n),
    })
}

pub(in crate::passes) fn pointer_pointee_type(ctx: &Ctx, ptr_ty: Word) -> Option<Word> {
    let def = type_def_of(ctx, ptr_ty)?;
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    match def.operands.get(1) {
        Some(Operand::IdRef(pointee)) => Some(*pointee),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fast_trig_flush_threshold_is_the_last_evaluated_quadrant() {
        let threshold = 6_588_397.5f32;
        assert_eq!(threshold, std::f32::consts::FRAC_PI_2 * (1u32 << 22) as f32);
        assert_eq!(threshold.to_bits(), 0x4AC9_0FDB);
        assert_eq!(f32::from_bits(0x4AC9_0FDC), 6_588_398.0);
    }
}
