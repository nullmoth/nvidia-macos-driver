use super::*;

pub(in crate::passes) fn lower_sample_depth(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.sample_depth has no result".into()),
    };
    if args.len() < 4 {
        return Err("air.sample_depth missing texture/sampler/coord".into());
    }
    let (mut img, samp, coord) = (resolve_image_value(ctx, args[0]), args[1], args[3]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(sampled_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Sampled)
        {
            img = sampled_img;
        } else {
            return lower_null_texture_result(ctx, res, rty);
        }
    }
    let mut out = vec![];
    img = load_image_if_pointer(ctx, img, &mut out);
    let (fallback_dim, fallback_arrayed, fallback_comp) = image_shape_or_recorded(ctx, img);
    let (img_ty, dim, arrayed, comp) =
        sampled_operand_image_info(ctx, img, fallback_dim, fallback_arrayed, fallback_comp);
    if comp != crate::passes::ImageComp::Float {
        return Err("air.sample_depth on non-float texture".into());
    }
    let mut sample_args = vec![args[0], args[1], coord];
    if arrayed {
        sample_args.push(
            args.get(4)
                .copied()
                .ok_or("air.sample_depth array texture missing layer")?,
        );
        sample_args.extend_from_slice(&args[5..]);
    } else {
        sample_args.extend_from_slice(&args[4..]);
    }
    let pixel_state = ctx
        .sampler_states
        .get(&samp)
        .copied()
        .filter(|state| state.uses_pixel_coordinates());
    let color = if let Some(state) = pixel_state {
        let lod = match find_sample_lod(ctx, arrayed, &sample_args, &mut out) {
            Some(lod) => sample_lod_to_fetch_lod(ctx, lod, &mut out)?,
            None => ctx.const_uint(0),
        };
        if state.uses_linear_filter() && matches!(dim, Dim::Dim1D | Dim::Dim2D | Dim::Dim3D) {
            lower_pixel_linear_sample(
                ctx,
                state,
                img,
                dim,
                arrayed,
                coord,
                &sample_args,
                lod,
                v4,
                &mut out,
            )?
        } else if state.uses_pixel_nearest() {
            let fetch = build_pixel_fetch_coord(
                ctx,
                state,
                img,
                dim,
                arrayed,
                coord,
                &sample_args,
                lod,
                &mut out,
            )?;
            let fetched = ctx.module.fresh_id();
            push_image_read_or_fetch(ctx, &mut out, img, fetch.coord, Some(lod), v4, fetched)?;
            if let Some(in_bounds) = fetch.in_bounds {
                let guarded = ctx.module.fresh_id();
                let zero = const_null_of(ctx, v4);
                out.push(Instruction::new(
                    Op::Select,
                    Some(v4),
                    Some(guarded),
                    vec![
                        Operand::IdRef(in_bounds),
                        Operand::IdRef(fetched),
                        Operand::IdRef(zero),
                    ],
                ));
                guarded
            } else {
                fetched
            }
        } else {
            return Err(format!(
                "pixel-coordinate depth sampling does not support {dim:?} with {:?} filtering",
                state.min_filter
            ));
        }
    } else {
        let si_ty = ctx.ty_sampled_image(img_ty);
        let si = ctx.module.fresh_id();
        let color = ctx.module.fresh_id();
        let samp = valid_sampler_value(ctx, samp, &mut out)?;
        out.push(Instruction::new(
            Op::SampledImage,
            Some(si_ty),
            Some(si),
            vec![Operand::IdRef(img), Operand::IdRef(samp)],
        ));
        let mut coord_for_sample = if arrayed {
            let layer = args[4];
            let spatial = match dim {
                Dim::Dim1D => 1,
                Dim::Dim2D => 2,
                Dim::DimCube | Dim::Dim3D => 3,
                _ => return Err("air.sample_depth unsupported arrayed dimension".into()),
            };
            build_arrayed_sample_coord(ctx, spatial, coord, layer, &mut out)?
        } else {
            coord
        };
        let level = find_sample_level(ctx, arrayed, &sample_args, &mut out);
        let spatial = sample_spatial_dims(dim);
        let (const_offset, dynamic_offset) = match spatial {
            Some(spatial) => {
                let (const_offset, dynamic_offset) =
                    sample_const_or_dynamic_offset(ctx, arrayed, &sample_args, spatial as u32)?;
                let const_offset = const_offset.and_then(|offset| {
                    if offset.iter().all(|delta| *delta == 0) {
                        None
                    } else {
                        Some(const_sint_vec(ctx, &offset))
                    }
                });
                (const_offset, dynamic_offset)
            }
            None => (None, None),
        };
        if let (Some(spatial), Some(offset)) = (spatial, dynamic_offset) {
            coord_for_sample = apply_dynamic_sample_offset(
                ctx,
                img,
                arrayed,
                coord_for_sample,
                offset,
                spatial,
                &mut out,
            )?;
        }
        push_image_sample(
            ctx,
            &mut out,
            v4,
            color,
            si,
            coord_for_sample,
            level,
            false,
            const_offset,
            None,
        );
        color
    };
    let depth = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeExtract,
        Some(ctx.ty_float()),
        Some(depth),
        vec![Operand::IdRef(color), Operand::LiteralBit32(0)],
    ));

    let rdef = type_def_of(ctx, rty);
    let is_struct = rdef
        .as_ref()
        .map(|d| d.class.opcode == Op::TypeStruct)
        .unwrap_or(false);
    if is_struct {
        let i8u = ctx.ty_int8();
        let undef8 = ctx.module.fresh_id();
        out.push(Instruction::new(Op::Undef, Some(i8u), Some(undef8), vec![]));
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(depth), Operand::IdRef(undef8)],
        ));
    } else {
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(depth)],
        ));
    }
    Ok(out)
}

pub(in crate::passes) fn lower_sample_compare_depth(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.sample_compare_depth has no result".into()),
    };
    if args.len() < 5 {
        return Err("air.sample_compare_depth missing texture/sampler/coord/reference".into());
    }
    let (mut img, samp, coord) = (resolve_image_value(ctx, args[0]), args[1], args[3]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(sampled_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Sampled)
        {
            img = sampled_img;
        } else {
            return lower_null_texture_result(ctx, res, rty);
        }
    }
    let mut out = vec![];
    img = load_image_if_pointer(ctx, img, &mut out);
    let (fallback_dim, fallback_arrayed, fallback_comp) = image_shape_or_recorded(ctx, img);
    let (img_ty, dim, arrayed, comp) =
        sampled_operand_image_info(ctx, img, fallback_dim, fallback_arrayed, fallback_comp);
    if comp != crate::passes::ImageComp::Float {
        return Err("air.sample_compare_depth on non-float texture".into());
    }
    let (mut coord_for_sample, reference) = if arrayed {
        let layer = args
            .get(4)
            .copied()
            .ok_or("air.sample_compare_depth array texture missing layer")?;
        let reference = args
            .get(5)
            .copied()
            .ok_or("air.sample_compare_depth array texture missing reference")?;
        let spatial = match dim {
            Dim::Dim1D => 1,
            Dim::Dim2D => 2,
            Dim::DimCube | Dim::Dim3D => 3,
            _ => return Err("air.sample_compare_depth unsupported arrayed dimension".into()),
        };
        (
            build_arrayed_sample_coord(ctx, spatial, coord, layer, &mut out)?,
            reference,
        )
    } else {
        (coord, args[4])
    };
    let float_ty = ctx.ty_float();
    let bool_ty = ctx.ty_bool();
    let depth = ctx.module.fresh_id();
    let shadow = ctx.module.fresh_id();
    let one = ctx.const_float(1.0);
    let zero = ctx.const_float(0.0);
    let sampler_state = ctx.sampler_states.get(&samp).copied();
    if sampler_state
        .map(|state| state.compare_function == crate::reflect::SamplerCompareFunction::None)
        .unwrap_or(false)
    {
        return Err("air.sample_compare_depth requires a sampler with comparison enabled".into());
    }
    let mut sample_args = vec![args[0], args[1], coord];
    let trailing = if arrayed {
        sample_args.push(args[4]);
        args.get(6..).unwrap_or_default()
    } else {
        args.get(5..).unwrap_or_default()
    };
    sample_args.extend_from_slice(trailing);
    let hw_dref = sampler_state.is_none();
    let color = if let Some(state) = sampler_state.filter(|state| state.uses_pixel_coordinates()) {
        let lod = match find_sample_lod(ctx, arrayed, &sample_args, &mut out) {
            Some(lod) => sample_lod_to_fetch_lod(ctx, lod, &mut out)?,
            None => ctx.const_uint(0),
        };
        if state.uses_linear_filter() && matches!(dim, Dim::Dim2D | Dim::Dim3D) {
            lower_pixel_linear_sample(
                ctx,
                state,
                img,
                dim,
                arrayed,
                coord,
                &sample_args,
                lod,
                v4,
                &mut out,
            )?
        } else if state.uses_pixel_nearest() {
            let fetch = build_pixel_fetch_coord(
                ctx,
                state,
                img,
                dim,
                arrayed,
                coord,
                &sample_args,
                lod,
                &mut out,
            )?;
            let fetched = ctx.module.fresh_id();
            push_image_read_or_fetch(ctx, &mut out, img, fetch.coord, Some(lod), v4, fetched)?;
            if let Some(in_bounds) = fetch.in_bounds {
                let guarded = ctx.module.fresh_id();
                let zero_color = const_null_of(ctx, v4);
                out.push(Instruction::new(
                    Op::Select,
                    Some(v4),
                    Some(guarded),
                    vec![
                        Operand::IdRef(in_bounds),
                        Operand::IdRef(fetched),
                        Operand::IdRef(zero_color),
                    ],
                ));
                guarded
            } else {
                fetched
            }
        } else {
            return Err(format!(
                "pixel-coordinate depth comparison does not support {dim:?} with {:?} filtering",
                state.min_filter
            ));
        }
    } else {
        let si_ty = ctx.ty_sampled_image(img_ty);
        let si = ctx.module.fresh_id();
        let color = ctx.module.fresh_id();
        let valid_sampler = valid_sampler_value(ctx, samp, &mut out)?;
        out.push(Instruction::new(
            Op::SampledImage,
            Some(si_ty),
            Some(si),
            vec![Operand::IdRef(img), Operand::IdRef(valid_sampler)],
        ));
        let level = find_sample_level(ctx, arrayed, &sample_args, &mut out);
        let spatial = sample_spatial_dims(dim);
        let (const_offset, dynamic_offset) = match spatial {
            Some(spatial) => {
                let (const_offset, dynamic_offset) =
                    sample_const_or_dynamic_offset(ctx, arrayed, &sample_args, spatial as u32)?;
                let const_offset = const_offset.and_then(|offset| {
                    if offset.iter().all(|delta| *delta == 0) {
                        None
                    } else {
                        Some(const_sint_vec(ctx, &offset))
                    }
                });
                (const_offset, dynamic_offset)
            }
            None => (None, None),
        };
        if let (Some(spatial), Some(offset)) = (spatial, dynamic_offset) {
            coord_for_sample = apply_dynamic_sample_offset(
                ctx,
                img,
                arrayed,
                coord_for_sample,
                offset,
                spatial,
                &mut out,
            )?;
        }
        if hw_dref {
            push_image_sample_dref(
                ctx,
                &mut out,
                float_ty,
                shadow,
                si,
                coord_for_sample,
                reference,
                level,
                const_offset,
            );
        } else {
            push_image_sample(
                ctx,
                &mut out,
                v4,
                color,
                si,
                coord_for_sample,
                level,
                false,
                const_offset,
                None,
            );
        }
        color
    };
    if !hw_dref {
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(float_ty),
            Some(depth),
            vec![Operand::IdRef(color), Operand::LiteralBit32(0)],
        ));
        let compare = sampler_state
            .map(|state| state.compare_function)
            .unwrap_or(crate::reflect::SamplerCompareFunction::GreaterEqual);
        let passed = match compare {
            crate::reflect::SamplerCompareFunction::None
            | crate::reflect::SamplerCompareFunction::Never => ctx.const_bool_of(bool_ty, false),
            crate::reflect::SamplerCompareFunction::Always => ctx.const_bool_of(bool_ty, true),
            compare => {
                let passed = ctx.module.fresh_id();
                let opcode = match compare {
                    crate::reflect::SamplerCompareFunction::Less => Op::FOrdLessThan,
                    crate::reflect::SamplerCompareFunction::LessEqual => Op::FOrdLessThanEqual,
                    crate::reflect::SamplerCompareFunction::Greater => Op::FOrdGreaterThan,
                    crate::reflect::SamplerCompareFunction::GreaterEqual => {
                        Op::FOrdGreaterThanEqual
                    }
                    crate::reflect::SamplerCompareFunction::Equal => Op::FOrdEqual,
                    crate::reflect::SamplerCompareFunction::NotEqual => Op::FOrdNotEqual,
                    crate::reflect::SamplerCompareFunction::None
                    | crate::reflect::SamplerCompareFunction::Always
                    | crate::reflect::SamplerCompareFunction::Never => unreachable!(),
                };
                out.push(Instruction::new(
                    opcode,
                    Some(bool_ty),
                    Some(passed),
                    vec![Operand::IdRef(reference), Operand::IdRef(depth)],
                ));
                passed
            }
        };
        out.push(Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(shadow),
            vec![
                Operand::IdRef(passed),
                Operand::IdRef(one),
                Operand::IdRef(zero),
            ],
        ));
    }

    let rdef = type_def_of(ctx, rty);
    let is_struct = rdef
        .as_ref()
        .map(|d| d.class.opcode == Op::TypeStruct)
        .unwrap_or(false);
    if is_struct {
        let i8u = ctx.ty_int8();
        let undef8 = ctx.module.fresh_id();
        out.push(Instruction::new(Op::Undef, Some(i8u), Some(undef8), vec![]));
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(shadow), Operand::IdRef(undef8)],
        ));
    } else {
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(shadow)],
        ));
    }
    Ok(out)
}

#[derive(Clone, Copy)]
pub(in crate::passes) enum SampleLevel {
    Explicit(Word),
    Bias(Word),
}

pub(in crate::passes) fn classify_sample_level(ctx: &Ctx, arrayed: bool, args: &[Word]) -> Option<SampleLevel> {
    let start = if arrayed { 4 } else { 3 };
    let rest = args.get(start..)?;
    let (offset, &slot) = rest
        .iter()
        .enumerate()
        .find(|(_, arg)| scalar_float_width(ctx, **arg).is_some())?;
    if !sample_level_flag(ctx, args, start + offset)? {
        if float_operand_is_constant_zero(ctx, slot) {
            return None;
        }
        return Some(SampleLevel::Bias(slot));
    }
    Some(SampleLevel::Explicit(slot))
}

pub(in crate::passes) fn find_sample_level(
    ctx: &mut Ctx,
    arrayed: bool,
    args: &[Word],
    out: &mut Vec<Instruction>,
) -> Option<SampleLevel> {
    match classify_sample_level(ctx, arrayed, args)? {
        SampleLevel::Explicit(lod) => sample_lod_as_f32(ctx, lod, out).map(SampleLevel::Explicit),
        SampleLevel::Bias(bias) => sample_lod_as_f32(ctx, bias, out).map(SampleLevel::Bias),
    }
}

pub(in crate::passes) fn find_sample_lod(
    ctx: &mut Ctx,
    arrayed: bool,
    args: &[Word],
    out: &mut Vec<Instruction>,
) -> Option<Word> {
    match classify_sample_level(ctx, arrayed, args)? {
        SampleLevel::Explicit(lod) => sample_lod_as_f32(ctx, lod, out),
        SampleLevel::Bias(_) => None,
    }
}

pub(in crate::passes) fn float_operand_is_constant_zero(ctx: &Ctx, value: Word) -> bool {
    let Some(def) = value_def_instruction(ctx, value) else {
        return false;
    };
    match def.class.opcode {
        Op::ConstantNull => true,
        Op::Constant => matches!(def.operands.first(), Some(Operand::LiteralBit32(0))),
        _ => false,
    }
}

fn sample_level_flag(ctx: &Ctx, args: &[Word], index: usize) -> Option<bool> {
    let &flag = index
        .checked_sub(1)
        .and_then(|previous| args.get(previous))?;
    let ty = value_result_type(ctx, flag)?;
    if type_def_of(ctx, ty).is_none_or(|def| def.class.opcode != Op::TypeBool) {
        return None;
    }
    Some(
        value_def_instruction(ctx, flag)
            .is_none_or(|def| !matches!(def.class.opcode, Op::ConstantFalse)),
    )
}

fn scalar_float_width(ctx: &Ctx, value: Word) -> Option<u32> {
    let ty = value_result_type(ctx, value)?;
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeFloat {
        return None;
    }
    match def.operands.first() {
        Some(Operand::LiteralBit32(width)) => Some(*width),
        _ => None,
    }
}

pub(in crate::passes) fn sample_spatial_dims(dim: Dim) -> Option<usize> {
    match dim {
        Dim::Dim1D => Some(1),
        Dim::Dim2D => Some(2),
        Dim::Dim3D => Some(3),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn push_image_sample(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    result_ty: Word,
    result: Word,
    sampled_image: Word,
    coord: Word,
    level: Option<SampleLevel>,
    force_lod0: bool,
    const_offset: Option<Word>,
    grad: Option<(Word, Word)>,
) {
    push_image_sample_min_lod(
        ctx,
        out,
        result_ty,
        result,
        sampled_image,
        coord,
        level,
        force_lod0,
        const_offset,
        grad,
        None,
    )
}
#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn push_image_sample_dref(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    result_ty: Word,
    result: Word,
    sampled_image: Word,
    coord: Word,
    dref: Word,
    level: Option<SampleLevel>,
    const_offset: Option<Word>,
) {
    let lod = match level {
        Some(SampleLevel::Explicit(lod)) => Some(lod),
        _ => (!matches!(ctx.stage, Stage::Fragment)).then(|| ctx.const_float(0.0)),
    };
    let bias = match level {
        Some(SampleLevel::Bias(bias)) if lod.is_none() => Some(bias),
        _ => None,
    };
    let mut operands = vec![
        Operand::IdRef(sampled_image),
        Operand::IdRef(coord),
        Operand::IdRef(dref),
    ];
    let mut image_operands = spirv::ImageOperands::empty();
    if bias.is_some() {
        image_operands |= spirv::ImageOperands::BIAS;
    }
    if lod.is_some() {
        image_operands |= spirv::ImageOperands::LOD;
    }
    if const_offset.is_some() {
        image_operands |= spirv::ImageOperands::CONST_OFFSET;
    }
    if !image_operands.is_empty() {
        operands.push(Operand::ImageOperands(image_operands));
        if let Some(bias) = bias {
            operands.push(Operand::IdRef(bias));
        }
        if let Some(lod) = lod {
            operands.push(Operand::IdRef(lod));
        }
        if let Some(offset) = const_offset {
            operands.push(Operand::IdRef(offset));
        }
    }
    out.push(Instruction::new(
        if lod.is_some() {
            Op::ImageSampleDrefExplicitLod
        } else {
            Op::ImageSampleDrefImplicitLod
        },
        Some(result_ty),
        Some(result),
        operands,
    ));
}
#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn push_image_sample_min_lod(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    result_ty: Word,
    result: Word,
    sampled_image: Word,
    coord: Word,
    level: Option<SampleLevel>,
    force_lod0: bool,
    const_offset: Option<Word>,
    grad: Option<(Word, Word)>,
    min_lod: Option<Word>,
) {
    let lod = if grad.is_some() {
        None
    } else {
        match level {
            Some(SampleLevel::Explicit(lod)) => Some(lod),
            _ => {
                (force_lod0 || !matches!(ctx.stage, Stage::Fragment)).then(|| ctx.const_float(0.0))
            }
        }
    };
    let bias = match level {
        Some(SampleLevel::Bias(bias)) if lod.is_none() && grad.is_none() => Some(bias),
        _ => None,
    };
    let mut operands = vec![Operand::IdRef(sampled_image), Operand::IdRef(coord)];
    let mut image_operands = spirv::ImageOperands::empty();
    if bias.is_some() {
        image_operands |= spirv::ImageOperands::BIAS;
    }
    if lod.is_some() {
        image_operands |= spirv::ImageOperands::LOD;
    }
    if grad.is_some() {
        image_operands |= spirv::ImageOperands::GRAD;
    }
    if const_offset.is_some() {
        image_operands |= spirv::ImageOperands::CONST_OFFSET;
    }
    if min_lod.is_some() {
        image_operands |= spirv::ImageOperands::MIN_LOD;
    }
    if !image_operands.is_empty() {
        operands.push(Operand::ImageOperands(image_operands));
        if let Some(bias) = bias {
            operands.push(Operand::IdRef(bias));
        }
        if let Some(lod) = lod {
            operands.push(Operand::IdRef(lod));
        }
        if let Some((dx, dy)) = grad {
            operands.push(Operand::IdRef(dx));
            operands.push(Operand::IdRef(dy));
        }
        if let Some(offset) = const_offset {
            operands.push(Operand::IdRef(offset));
        }
        if let Some(min_lod) = min_lod {
            operands.push(Operand::IdRef(min_lod));
        }
    }
    out.push(Instruction::new(
        if lod.is_some() || grad.is_some() {
            Op::ImageSampleExplicitLod
        } else {
            Op::ImageSampleImplicitLod
        },
        Some(result_ty),
        Some(result),
        operands,
    ));
}

pub(in crate::passes) fn sample_lod_as_f32(
    ctx: &mut Ctx,
    lod: Word,
    out: &mut Vec<Instruction>,
) -> Option<Word> {
    let ty = value_result_type(ctx, lod)?;
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeFloat {
        return None;
    }
    let width = match def.operands.first() {
        Some(Operand::LiteralBit32(w)) => *w,
        _ => return None,
    };
    if width == 32 {
        return Some(lod);
    }
    if width == 16 {
        let widened = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(ctx.ty_float()),
            Some(widened),
            vec![Operand::IdRef(lod)],
        ));
        return Some(widened);
    }
    None
}
