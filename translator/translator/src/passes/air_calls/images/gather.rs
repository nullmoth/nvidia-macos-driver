use super::*;

pub(in crate::passes) fn lower_gather(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.gather_texture has no result".into()),
    };
    if args.len() < 6 {
        return Err("air.gather_texture missing texture/sampler/coord/offset/component".into());
    }
    let samp = args[1];
    let mut img = resolve_image_value(ctx, args[0]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(sampled_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Sampled)
        {
            img = sampled_img;
        } else {
            return lower_null_texture_result(ctx, res, rty);
        }
    }
    let (dim, _, _) = image_shape_or_recorded(ctx, img);
    let arrayed = name.starts_with("air.gather_texture_2d_array.");
    let (layer, offset, component) = if arrayed {
        if args.len() < 7 {
            return Err("air.gather_texture array form missing layer/offset/component".into());
        }
        (Some(args[3]), args[5], args[6])
    } else {
        (None, args[4], args[5])
    };
    lower_gather_2d(
        ctx, res, rty, img, dim, arrayed, samp, args[2], layer, offset, component, v4,
    )
}

pub(in crate::passes) fn lower_gather_depth(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.gather_depth has no result".into()),
    };
    if args.len() < 7 {
        return Err("air.gather_depth missing texture/sampler/coord/offset".into());
    }
    let samp = args[1];
    let mut img = resolve_image_value(ctx, args[0]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(sampled_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Sampled)
        {
            img = sampled_img;
        } else {
            return lower_null_texture_result(ctx, res, rty);
        }
    }
    let (dim, _, _) = image_shape_or_recorded(ctx, img);
    let arrayed = name.starts_with("air.gather_depth_2d_array.");
    let coord = args[3];
    let (layer, offset_index) = if arrayed {
        if args.len() < 8 {
            return Err("air.gather_depth array form missing layer/offset".into());
        }
        (Some(args[4]), 6)
    } else {
        (None, 5)
    };
    let offset = args[offset_index];
    let component = ctx.const_uint(0);
    lower_gather_2d(
        ctx, res, rty, img, dim, arrayed, samp, coord, layer, offset, component, v4,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn lower_gather_2d(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    img: Word,
    dim: Dim,
    arrayed: bool,
    samp: Word,
    coord: Word,
    layer: Option<Word>,
    offset: Word,
    component: Word,
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    if dim != Dim::Dim2D {
        return Err("air.gather_texture currently supports 2D textures only".into());
    }
    let mut out = vec![];
    let img = load_image_if_pointer(ctx, img, &mut out);
    let (_, _, fallback_comp) = image_shape_or_recorded(ctx, img);
    let (img_ty, _dim, arrayed, _) =
        sampled_operand_image_info(ctx, img, dim, arrayed, fallback_comp);
    let gather_v4 = image_fetch_v4(ctx, img, v4);

    if let Some(sampler_state) = ctx
        .sampler_states
        .get(&samp)
        .copied()
        .filter(|state| state.uses_pixel_coordinates())
    {
        return lower_pixel_gather_2d(
            ctx,
            res,
            rty,
            img,
            arrayed,
            sampler_state,
            coord,
            layer,
            offset,
            component,
            gather_v4,
            out,
        );
    }

    let si_ty = ctx.ty_sampled_image(img_ty);
    let samp = valid_sampler_value(ctx, samp, &mut out)?;
    let si = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::SampledImage,
        Some(si_ty),
        Some(si),
        vec![Operand::IdRef(img), Operand::IdRef(samp)],
    ));

    let mut coord_for_gather = build_gather_coord_2d(ctx, arrayed, coord, layer, &mut out)?;
    let (const_offset, dynamic_offset) = gather_const_or_dynamic_offset(ctx, offset)?;
    if let Some(offset) = dynamic_offset {
        coord_for_gather =
            apply_dynamic_sample_offset(ctx, img, arrayed, coord_for_gather, offset, 2, &mut out)?;
    }

    let color = ctx.module.fresh_id();
    let mut ops = vec![
        Operand::IdRef(si),
        Operand::IdRef(coord_for_gather),
        Operand::IdRef(component),
    ];
    if let Some(offset) = const_offset {
        if offset != [0, 0] {
            let offset_id = const_sint_vec(ctx, &offset);
            ops.push(Operand::ImageOperands(spirv::ImageOperands::CONST_OFFSET));
            ops.push(Operand::IdRef(offset_id));
        }
    }
    out.push(Instruction::new(
        Op::ImageGather,
        Some(gather_v4),
        Some(color),
        ops,
    ));

    finish_sample_result(ctx, res, rty, color, gather_v4, out)
}

pub(in crate::passes) fn build_arrayed_sample_coord(
    ctx: &mut Ctx,
    spatial: u32,
    coord: Word,
    layer: Word,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let layer_f = sample_layer_to_float(ctx, layer, out)?;
    let mut comps = sample_coord_components(ctx, coord, spatial, out)?;
    comps.push(Operand::IdRef(layer_f));
    let combined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(ctx.ty_vecf(spatial + 1)),
        Some(combined),
        comps,
    ));
    Ok(combined)
}

pub(in crate::passes) fn build_gather_coord_2d(
    ctx: &mut Ctx,
    arrayed: bool,
    coord: Word,
    layer: Option<Word>,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    if !arrayed {
        return Ok(coord);
    }
    let layer = layer.ok_or("air.gather_texture array texture missing layer")?;
    build_arrayed_sample_coord(ctx, 2, coord, layer, out)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn lower_pixel_gather_2d(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    img: Word,
    arrayed: bool,
    sampler_state: StaticSamplerState,
    coord: Word,
    layer: Option<Word>,
    offset: Word,
    component: Word,
    v4: Word,
    mut out: Vec<Instruction>,
) -> Result<Vec<Instruction>, String> {
    let component = const_i32_scalar(ctx, component)
        .ok_or("air.gather_texture pixel component must be a constant scalar")?;
    if !(0..4).contains(&component) {
        return Err("air.gather_texture pixel component is out of range".into());
    }
    let selected_ty = vector_element_type(ctx, v4).unwrap_or_else(|| ctx.ty_float());
    let (const_offset, dynamic_offset) = gather_const_or_dynamic_offset(ctx, offset)?;
    let sample_v4 = image_fetch_v4(ctx, img, v4);
    let lod = ctx.const_uint(0);
    let coord = subtract_half_from_gather_coord(ctx, coord, &mut out);
    let mut values = Vec::with_capacity(4);
    for texel_offset in [[0, 1], [1, 1], [1, 0], [0, 0]] {
        let (fetch_offset, dynamic_fetch_offset) = pixel_gather_fetch_offset(
            ctx,
            const_offset,
            dynamic_offset,
            texel_offset,
            false,
            &mut out,
        )?;
        let fetch = build_pixel_fetch_coord_from_parts(
            ctx,
            sampler_state,
            img,
            Dim::Dim2D,
            arrayed,
            coord,
            layer,
            fetch_offset,
            dynamic_fetch_offset,
            lod,
            &mut out,
        )?;
        let color = ctx.module.fresh_id();
        push_image_read_or_fetch(ctx, &mut out, img, fetch.coord, Some(lod), sample_v4, color)?;
        let mut selected = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(selected_ty),
            Some(selected),
            vec![
                Operand::IdRef(color),
                Operand::LiteralBit32(component as u32),
            ],
        ));
        if let Some(in_bounds) = fetch.in_bounds {
            let guarded = ctx.module.fresh_id();
            let zero = const_null_of(ctx, selected_ty);
            out.push(Instruction::new(
                Op::Select,
                Some(selected_ty),
                Some(guarded),
                vec![
                    Operand::IdRef(in_bounds),
                    Operand::IdRef(selected),
                    Operand::IdRef(zero),
                ],
            ));
            selected = guarded;
        }
        values.push(Operand::IdRef(selected));
    }
    let color = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v4),
        Some(color),
        values,
    ));
    finish_sample_result(ctx, res, rty, color, v4, out)
}

pub(in crate::passes) fn subtract_half_from_gather_coord(
    ctx: &mut Ctx,
    coord: Word,
    out: &mut Vec<Instruction>,
) -> Word {
    let vec2f = ctx.ty_vecf(2);
    if value_result_type(ctx, coord) != Some(vec2f) {
        return coord;
    }
    let half = ctx.const_float(0.5);
    let half_splat = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(vec2f),
        Some(half_splat),
        vec![Operand::IdRef(half), Operand::IdRef(half)],
    ));
    let shifted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FSub,
        Some(vec2f),
        Some(shifted),
        vec![Operand::IdRef(coord), Operand::IdRef(half_splat)],
    ));
    shifted
}

pub(in crate::passes) fn pixel_gather_fetch_offset(
    ctx: &mut Ctx,
    const_offset: Option<[i32; 2]>,
    dynamic_offset: Option<Word>,
    mut texel_offset: [i32; 2],
    footprint_bias: bool,
    out: &mut Vec<Instruction>,
) -> Result<(Option<Vec<i32>>, Option<Vec<Word>>), String> {
    if footprint_bias {
        texel_offset = [texel_offset[0] - 1, texel_offset[1] - 1];
    }
    if let Some(offset) = const_offset {
        return Ok((
            Some(vec![
                offset[0] + texel_offset[0],
                offset[1] + texel_offset[1],
            ]),
            None,
        ));
    }
    let Some(offset) = dynamic_offset else {
        return Ok((Some(texel_offset.to_vec()), None));
    };
    let sint = ctx.ty_sint();
    let components = dynamic_i32_integer_offset_components(ctx, offset, 2, out)?;
    let mut shifted = Vec::with_capacity(2);
    for (component, delta) in components.into_iter().zip(texel_offset) {
        if delta == 0 {
            shifted.push(component);
            continue;
        }
        let value = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IAdd,
            Some(sint),
            Some(value),
            vec![
                Operand::IdRef(component),
                Operand::IdRef(ctx.const_int_of(sint, delta as i64)),
            ],
        ));
        shifted.push(value);
    }
    Ok((None, Some(shifted)))
}

pub(in crate::passes) fn valid_sampler_value(
    ctx: &mut Ctx,
    samp: Word,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    if let Some(slot) = ctx
        .emit_sidecar
        .device_handle_heap_loads
        .iter()
        .find(|fact| fact.sampler && fact.id == samp)
        .map(|fact| fact.slot)
    {
        let sty = ctx.ty_sampler();
        let heap = crate::passes::stage_input::bindless_sampler_heap_var(ctx);
        let ptr = ctx.ty_ptr(spirv::StorageClass::UniformConstant, sty);
        let chain = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::AccessChain,
            Some(ptr),
            Some(chain),
            vec![Operand::IdRef(heap), Operand::IdRef(slot)],
        ));
        let value = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(sty),
            Some(value),
            vec![Operand::IdRef(chain)],
        ));
        return Ok(value);
    }
    validate_runtime_sampler_specialization(ctx, samp)?;
    let sty = ctx.ty_sampler();
    if value_result_type(ctx, samp) == Some(sty) {
        return Ok(samp);
    }
    if !value_is_pointer(ctx, samp) {
        return Err("air.sample_texture sampler operand is not a sampler".into());
    }

    if sampler_join_of_sampler_values(ctx, samp, sty) {
        // A phi/select of samplers is carried as is; the sample is replayed per arm later.
        return Ok(samp);
    }
    if !ctx.read_sampler_values.contains(&samp) {
        return Err(
            "sampler operand is a pointer whose exact sampler state was not recovered".into(),
        );
    }
    let var = ctx.default_read_sampler()?;
    let loaded = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(sty),
        Some(loaded),
        vec![Operand::IdRef(var)],
    ));
    Ok(loaded)
}

fn sampler_join_of_sampler_values(ctx: &Ctx, samp: Word, sty: Word) -> bool {
    let Some(inst) = value_inst(ctx, samp) else {
        return false;
    };
    let (skip, step) = match inst.class.opcode {
        Op::Phi => (0, 2),
        Op::Select => (1, 1),
        _ => return false,
    };
    let mut arms = inst.operands.iter().skip(skip).step_by(step).peekable();
    arms.peek().is_some()
        && arms.all(
            |arm| matches!(arm, Operand::IdRef(id) if value_result_type(ctx, *id) == Some(sty)),
        )
}

pub(in crate::passes) fn validate_runtime_sampler_specialization(
    ctx: &Ctx,
    samp: Word,
) -> Result<(), String> {
    if ctx.ambiguous_sampler_states.contains(&samp) {
        return Err(
            "runtime sampler specialization is ambiguous after an SSA select/phi join".into(),
        );
    }
    if value_is_pointer(ctx, samp) && !ctx.specialized_runtime_sampler_values.is_empty() {
        return Err(
            "runtime sampler specialization cannot recover an exact state after pointer selection"
                .into(),
        );
    }
    Ok(())
}
