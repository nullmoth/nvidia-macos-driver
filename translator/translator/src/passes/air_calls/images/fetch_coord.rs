use super::*;

pub(in crate::passes) fn coerce_image_coord32(
    ctx: &mut Ctx,
    coord: Word,
    out: &mut Vec<Instruction>,
    what: &str,
) -> Result<Word, String> {
    coerce_image_coord32_typed(ctx, coord, out, what).map(|(id, _)| id)
}

pub(in crate::passes) fn coerce_image_coord32_typed(
    ctx: &mut Ctx,
    coord: Word,
    out: &mut Vec<Instruction>,
    what: &str,
) -> Result<(Word, Word), String> {
    // The coordinate may be a value this lowering just emitted (a cube array's combined layer), so look
    // through the pending instructions before the module.
    let pending_ty = out
        .iter()
        .rev()
        .find(|inst| inst.result_id == Some(coord))
        .and_then(|inst| inst.result_type);
    let Some(ty) = pending_ty.or_else(|| value_result_type(ctx, coord)) else {
        return Err(format!("{what}: coord has no type"));
    };
    let Some(def) = type_def_of(ctx, ty) else {
        return Err(format!("{what}: coord type is undefined"));
    };
    let dst = match def.class.opcode {
        Op::TypeInt => ctx.ty_uint(),
        Op::TypeVector => {
            let Some(Operand::IdRef(elem)) = def.operands.first() else {
                return Err(format!("{what}: vector coord missing element type"));
            };
            let Some(Operand::LiteralBit32(n)) = def.operands.get(1) else {
                return Err(format!("{what}: vector coord missing length"));
            };
            let is_int_elem = type_def_of(ctx, *elem)
                .map(|e| e.class.opcode == Op::TypeInt)
                .unwrap_or(false);
            if !is_int_elem {
                if crate::env_vars::tex_dbg() {
                    eprintln!(
                        "TEX-COORD {what}: operand id {coord} ty id {ty} = vector len {n} elem id {elem} (non-int)"
                    );
                }
                return Err(format!("{what}: non-integer vector coord"));
            }
            ctx.ty_vec_uint(*n)
        }
        _ => return Err(format!("{what}: non-integer coord")),
    };
    if scalar_bit_width(ctx, ty) == 32 {
        return Ok((coord, ty));
    }
    let c = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(dst),
        Some(c),
        vec![Operand::IdRef(coord)],
    ));
    Ok((c, dst))
}

/// Metal addresses a `texturecube_array` texel by (xy, face, slice). SPIR-V's Cube + Arrayed storage
/// coordinate is `(x, y, layer)` with one combined layer-face index, so the slice and face fold into
/// `slice * 6 + face` before `build_fetch_coord` appends it as the array layer.
pub(in crate::passes) fn combine_cube_array_layer(
    ctx: &mut Ctx,
    face: Word,
    slice: Word,
    out: &mut Vec<Instruction>,
    what: &str,
) -> Result<Word, String> {
    let face32 = coerce_image_coord32(ctx, face, out, what)?;
    let slice32 = coerce_image_coord32(ctx, slice, out, what)?;
    let uint = ctx.ty_uint();
    let six = ctx.const_uint(6);
    let scaled = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(scaled),
        vec![Operand::IdRef(slice32), Operand::IdRef(six)],
    ));
    let combined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(combined),
        vec![Operand::IdRef(scaled), Operand::IdRef(face32)],
    ));
    Ok(combined)
}

pub(in crate::passes) fn build_fetch_coord(
    ctx: &mut Ctx,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    layer: Option<Word>,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let (coord32, coord_ty) = coerce_image_coord32_typed(ctx, coord, out, "air.read_texture")?;
    if dim == Dim::DimCube && !arrayed && layer.is_some() {
        let face = layer.ok_or("air.read_texture: cube face layer missing")?;
        let face32 = coerce_image_coord32(ctx, face, out, "air.write_texture cube face")?;
        let coord_def =
            type_def_of(ctx, coord_ty).ok_or("air.write_texture cube coord type is undefined")?;
        let mut comps = Vec::new();
        match coord_def.class.opcode {
            Op::TypeVector => {
                let Some(Operand::LiteralBit32(n)) = coord_def.operands.get(1) else {
                    return Err("air.write_texture cube vector coord missing length".into());
                };
                if *n != 2 {
                    return Err("air.write_texture cube coord must be uint2".into());
                }
                for c in 0..*n {
                    let id = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(ctx.ty_uint()),
                        Some(id),
                        vec![Operand::IdRef(coord32), Operand::LiteralBit32(c)],
                    ));
                    comps.push(Operand::IdRef(id));
                }
            }
            _ => return Err("air.write_texture cube coord must be an integer vector".into()),
        }
        comps.push(Operand::IdRef(face32));
        let combined = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(ctx.ty_vec_uint(3)),
            Some(combined),
            comps,
        ));
        return Ok(combined);
    }
    if !arrayed {
        return Ok(coord32);
    }
    let layer = layer.ok_or("air.read_texture array texture missing layer")?;
    let layer32 = coerce_image_coord32(ctx, layer, out, "air.read_texture layer")?;
    let coord_def = type_def_of(ctx, coord_ty).ok_or("air.read_texture coord type is undefined")?;
    let out_ty = match dim {
        Dim::Dim1D => ctx.ty_vec_uint(2),
        // A cube array's layer operand is the combined `slice * 6 + face` index.
        Dim::Dim2D | Dim::DimCube => ctx.ty_vec_uint(3),
        _ => return Err("air.read_texture unsupported arrayed dimension".into()),
    };
    let mut comps = Vec::new();
    match coord_def.class.opcode {
        Op::TypeInt => comps.push(Operand::IdRef(coord32)),
        Op::TypeVector => {
            let Some(Operand::LiteralBit32(n)) = coord_def.operands.get(1) else {
                return Err("air.read_texture vector coord missing length".into());
            };
            for c in 0..*n {
                let id = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(ctx.ty_uint()),
                    Some(id),
                    vec![Operand::IdRef(coord32), Operand::LiteralBit32(c)],
                ));
                comps.push(Operand::IdRef(id));
            }
        }
        _ => return Err("air.read_texture non-integer array coord".into()),
    }
    comps.push(Operand::IdRef(layer32));
    let combined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(out_ty),
        Some(combined),
        comps,
    ));
    Ok(combined)
}

pub(in crate::passes) struct PixelFetchCoord {
    pub(in crate::passes) coord: Word,
    pub(in crate::passes) in_bounds: Option<Word>,
}

pub(in crate::passes) fn build_pixel_fetch_coord(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    args: &[Word],
    lod: Word,
    out: &mut Vec<Instruction>,
) -> Result<PixelFetchCoord, String> {
    let spatial: usize = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        Dim::Dim3D => 3,
        _ => return Err("air.sample_texture pixel fetch unsupported image dimension".into()),
    };
    let (offset, dynamic_offset) =
        sample_const_or_dynamic_offset(ctx, arrayed, args, spatial as u32)?;
    let dynamic_offset = dynamic_offset
        .map(|offset| dynamic_i32_integer_offset_components(ctx, offset, spatial, out))
        .transpose()?;
    let layer = if arrayed {
        Some(
            args.get(3)
                .copied()
                .ok_or("air.sample_texture array texture missing layer")?,
        )
    } else {
        None
    };
    build_pixel_fetch_coord_from_parts(
        ctx,
        sampler_state,
        img,
        dim,
        arrayed,
        coord,
        layer,
        offset,
        dynamic_offset,
        lod,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn build_pixel_fetch_coord_from_parts(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    layer: Option<Word>,
    offset: Option<Vec<i32>>,
    dynamic_offset: Option<Vec<Word>>,
    lod: Word,
    out: &mut Vec<Instruction>,
) -> Result<PixelFetchCoord, String> {
    let spatial: usize = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        Dim::Dim3D => 3,
        _ => return Err("air.sample_texture pixel fetch unsupported image dimension".into()),
    };
    let size = query_image_size(ctx, img, spatial, arrayed, lod, out);
    let sint = ctx.ty_sint();
    let float_ty = ctx.ty_float();
    let glsl = ctx.glsl();
    let mut signed_components = Vec::with_capacity(spatial);
    for (idx, comp) in sample_coord_components(ctx, coord, spatial as u32, out)?
        .into_iter()
        .enumerate()
    {
        let Operand::IdRef(comp) = comp else {
            return Err("air.sample_texture pixel coord component is not an id".into());
        };
        let comp = clamp_pixel_coord_component_finite(
            ctx,
            sampler_state,
            comp,
            size,
            spatial > 1 || arrayed,
            idx as u32,
            out,
        );
        let floored = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(floored),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(8),
                Operand::IdRef(comp),
            ],
        ));
        let mut converted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertFToS,
            Some(sint),
            Some(converted),
            vec![Operand::IdRef(floored)],
        ));
        if let Some(offset) = &offset {
            let delta = offset[idx];
            if delta != 0 {
                let shifted = ctx.module.fresh_id();
                let delta = ctx.const_int_of(sint, delta as i64);
                out.push(Instruction::new(
                    Op::IAdd,
                    Some(sint),
                    Some(shifted),
                    vec![Operand::IdRef(converted), Operand::IdRef(delta)],
                ));
                converted = shifted;
            }
        } else if let Some(offset) = &dynamic_offset {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IAdd,
                Some(sint),
                Some(shifted),
                vec![Operand::IdRef(converted), Operand::IdRef(offset[idx])],
            ));
            converted = shifted;
        }
        signed_components.push(converted);
    }
    build_pixel_fetch_coord_from_signed_components(
        ctx,
        sampler_state,
        dim,
        arrayed,
        &signed_components,
        layer,
        size,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn build_normalized_nearest_fetch_coord(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    args: &[Word],
    lod: Word,
    out: &mut Vec<Instruction>,
) -> Result<PixelFetchCoord, String> {
    let spatial: usize = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        Dim::Dim3D => 3,
        _ => {
            return Err("air.sample_texture normalized integer fetch unsupported dimension".into())
        }
    };
    let (offset, dynamic_offset) =
        sample_const_or_dynamic_offset(ctx, arrayed, args, spatial as u32)?;
    let dynamic_offset = dynamic_offset
        .map(|offset| dynamic_i32_integer_offset_components(ctx, offset, spatial, out))
        .transpose()?;
    let layer = if arrayed {
        Some(
            args.get(3)
                .copied()
                .ok_or("air.sample_texture array texture missing layer")?,
        )
    } else {
        None
    };
    let size = query_image_size(ctx, img, spatial, arrayed, lod, out);
    let float_ty = ctx.ty_float();
    let sint = ctx.ty_sint();
    let glsl = ctx.glsl();
    let mut signed_components = Vec::with_capacity(spatial);
    for (idx, comp) in sample_coord_components(ctx, coord, spatial as u32, out)?
        .into_iter()
        .enumerate()
    {
        let Operand::IdRef(comp) = comp else {
            return Err("air.sample_texture normalized coord component is not an id".into());
        };
        let size_component = image_size_component(ctx, size, idx, spatial, arrayed, out)?;
        let size_f = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertUToF,
            Some(float_ty),
            Some(size_f),
            vec![Operand::IdRef(size_component)],
        ));
        let scaled = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FMul,
            Some(float_ty),
            Some(scaled),
            vec![Operand::IdRef(comp), Operand::IdRef(size_f)],
        ));
        let floored = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(floored),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(8),
                Operand::IdRef(scaled),
            ],
        ));
        let mut converted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertFToS,
            Some(sint),
            Some(converted),
            vec![Operand::IdRef(floored)],
        ));
        if let Some(offset) = &offset {
            let delta = offset[idx];
            if delta != 0 {
                let shifted = ctx.module.fresh_id();
                let delta = ctx.const_int_of(sint, delta as i64);
                out.push(Instruction::new(
                    Op::IAdd,
                    Some(sint),
                    Some(shifted),
                    vec![Operand::IdRef(converted), Operand::IdRef(delta)],
                ));
                converted = shifted;
            }
        } else if let Some(offset) = &dynamic_offset {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IAdd,
                Some(sint),
                Some(shifted),
                vec![Operand::IdRef(converted), Operand::IdRef(offset[idx])],
            ));
            converted = shifted;
        }
        signed_components.push(converted);
    }
    build_pixel_fetch_coord_from_signed_components(
        ctx,
        sampler_state,
        dim,
        arrayed,
        &signed_components,
        layer,
        size,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn pixel_linear_tap_coord(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    dim: Dim,
    arrayed: bool,
    base: &[Word],
    tap_offset: &[i32],
    layer: Option<Word>,
    size: Word,
    out: &mut Vec<Instruction>,
) -> Result<PixelFetchCoord, String> {
    let sint = ctx.ty_sint();
    let mut signed_components = Vec::with_capacity(base.len());
    for (idx, base_component) in base.iter().copied().enumerate() {
        let offset = tap_offset[idx];
        let coord = if offset == 0 {
            base_component
        } else {
            let shifted = ctx.module.fresh_id();
            let offset = ctx.const_int_of(sint, offset as i64);
            out.push(Instruction::new(
                Op::IAdd,
                Some(sint),
                Some(shifted),
                vec![Operand::IdRef(base_component), Operand::IdRef(offset)],
            ));
            shifted
        };
        signed_components.push(coord);
    }
    build_pixel_fetch_coord_from_signed_components(
        ctx,
        sampler_state,
        dim,
        arrayed,
        &signed_components,
        layer,
        size,
        out,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn build_pixel_fetch_coord_from_signed_components(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    dim: Dim,
    arrayed: bool,
    signed_components: &[Word],
    layer: Option<Word>,
    size: Word,
    out: &mut Vec<Instruction>,
) -> Result<PixelFetchCoord, String> {
    let spatial: usize = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        Dim::Dim3D => 3,
        _ => return Err("air.sample_texture pixel fetch unsupported image dimension".into()),
    };
    if signed_components.len() != spatial {
        return Err("air.sample_texture pixel fetch coord has unexpected component count".into());
    }
    let uint = ctx.ty_uint();
    let sint = ctx.ty_sint();
    let bool_ty = ctx.ty_bool();
    let mut comps = Vec::new();
    let mut in_bounds = None;
    for (idx, converted) in signed_components.iter().copied().enumerate() {
        let zero = ctx.const_int_of(sint, 0);
        let below_zero = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::SLessThan,
            Some(bool_ty),
            Some(below_zero),
            vec![Operand::IdRef(converted), Operand::IdRef(zero)],
        ));
        let nonnegative = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(sint),
            Some(nonnegative),
            vec![
                Operand::IdRef(below_zero),
                Operand::IdRef(zero),
                Operand::IdRef(converted),
            ],
        ));
        let max_coord_u = image_size_component(ctx, size, idx, spatial, arrayed, out)?;
        let one = ctx.const_uint(1);
        let max_coord_u_minus_one = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ISub,
            Some(uint),
            Some(max_coord_u_minus_one),
            vec![Operand::IdRef(max_coord_u), Operand::IdRef(one)],
        ));
        let max_coord = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Bitcast,
            Some(sint),
            Some(max_coord),
            vec![Operand::IdRef(max_coord_u_minus_one)],
        ));
        let above_max = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::SGreaterThan,
            Some(bool_ty),
            Some(above_max),
            vec![Operand::IdRef(nonnegative), Operand::IdRef(max_coord)],
        ));
        let clamped = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(sint),
            Some(clamped),
            vec![
                Operand::IdRef(above_max),
                Operand::IdRef(max_coord),
                Operand::IdRef(nonnegative),
            ],
        ));
        let addressed = match sampler_state
            .spatial_address_mode(idx)
            .ok_or("pixel sampler address dimension is missing")?
        {
            crate::reflect::SamplerAddressMode::ClampToZero
            | crate::reflect::SamplerAddressMode::ClampToEdge
            | crate::reflect::SamplerAddressMode::ClampToBorder => clamped,
            crate::reflect::SamplerAddressMode::Repeat => {
                let size_s = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Bitcast,
                    Some(sint),
                    Some(size_s),
                    vec![Operand::IdRef(max_coord_u)],
                ));
                let wrapped = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::SMod,
                    Some(sint),
                    Some(wrapped),
                    vec![Operand::IdRef(converted), Operand::IdRef(size_s)],
                ));
                wrapped
            }
            crate::reflect::SamplerAddressMode::MirroredRepeat => {
                let size_s = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Bitcast,
                    Some(sint),
                    Some(size_s),
                    vec![Operand::IdRef(max_coord_u)],
                ));
                let two = ctx.const_int_of(sint, 2);
                let period = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::IMul,
                    Some(sint),
                    Some(period),
                    vec![Operand::IdRef(size_s), Operand::IdRef(two)],
                ));
                let phase = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::SMod,
                    Some(sint),
                    Some(phase),
                    vec![Operand::IdRef(converted), Operand::IdRef(period)],
                ));
                let one_s = ctx.const_int_of(sint, 1);
                let period_minus_one = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::ISub,
                    Some(sint),
                    Some(period_minus_one),
                    vec![Operand::IdRef(period), Operand::IdRef(one_s)],
                ));
                let mirrored = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::ISub,
                    Some(sint),
                    Some(mirrored),
                    vec![Operand::IdRef(period_minus_one), Operand::IdRef(phase)],
                ));
                let second_half = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::SGreaterThanEqual,
                    Some(bool_ty),
                    Some(second_half),
                    vec![Operand::IdRef(phase), Operand::IdRef(size_s)],
                ));
                let wrapped = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Select,
                    Some(sint),
                    Some(wrapped),
                    vec![
                        Operand::IdRef(second_half),
                        Operand::IdRef(mirrored),
                        Operand::IdRef(phase),
                    ],
                ));
                wrapped
            }
        };
        let fetch_comp = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Bitcast,
            Some(uint),
            Some(fetch_comp),
            vec![Operand::IdRef(addressed)],
        ));
        comps.push(Operand::IdRef(fetch_comp));
        if sampler_state.spatial_clamps_to_zero(idx) {
            let not_below_zero = logical_not(ctx, below_zero, out);
            let not_above_max = logical_not(ctx, above_max, out);
            let component_in_bounds = logical_and(ctx, not_below_zero, not_above_max, out);
            in_bounds = Some(match in_bounds {
                Some(existing) => logical_and(ctx, existing, component_in_bounds, out),
                None => component_in_bounds,
            });
        }
    }
    if arrayed {
        let layer = layer.ok_or("air.sample_texture array texture missing layer")?;
        let layer = sample_layer_to_uint(ctx, layer, out)?;
        let layer_count = image_size_component(ctx, size, spatial, spatial, arrayed, out)?;
        let layer_in_bounds = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ULessThan,
            Some(bool_ty),
            Some(layer_in_bounds),
            vec![Operand::IdRef(layer), Operand::IdRef(layer_count)],
        ));
        let one = ctx.const_uint(1);
        let max_layer = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ISub,
            Some(uint),
            Some(max_layer),
            vec![Operand::IdRef(layer_count), Operand::IdRef(one)],
        ));
        let clamped_layer = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(uint),
            Some(clamped_layer),
            vec![
                Operand::IdRef(layer_in_bounds),
                Operand::IdRef(layer),
                Operand::IdRef(max_layer),
            ],
        ));
        comps.push(Operand::IdRef(clamped_layer));
    }
    if comps.len() == 1 {
        let Operand::IdRef(coord) = comps[0] else {
            return Err("air.read_texture: pixel fetch coord component is not an id".into());
        };
        return Ok(PixelFetchCoord { coord, in_bounds });
    }
    let combined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(ctx.ty_vec_uint(comps.len() as u32)),
        Some(combined),
        comps,
    ));
    Ok(PixelFetchCoord {
        coord: combined,
        in_bounds,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn cube_fetch_as_center_sample(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    img: Word,
    coord: Word,
    face: Word,
    fetch_v4: Word,
    color: Word,
    sampler_arg: Option<Word>,
) -> Result<(), String> {
    if sampler_arg.is_some_and(|sampler| {
        ctx.sampler_states
            .get(&sampler)
            .is_some_and(|state| state.uses_pixel_coordinates())
    }) {
        return Err(
            "air.read_texture cube reads do not support pixel-coordinate runtime samplers".into(),
        );
    }
    let img = load_image_if_pointer(ctx, img, out);
    let float = ctx.ty_float();
    let v3f = ctx.ty_vecf(3);
    let (coord32, _) = coerce_image_coord32_typed(ctx, coord, out, "air.read_texture cube coord")?;
    let face32 = coerce_image_coord32(ctx, face, out, "air.read_texture cube face")?;
    let uint = ctx.ty_uint();
    let extract = |vec: Word, idx: u32, ctx: &mut Ctx, out: &mut Vec<Instruction>| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(id),
            vec![Operand::IdRef(vec), Operand::LiteralBit32(idx)],
        ));
        id
    };
    let x = extract(coord32, 0, ctx, out);
    let y = extract(coord32, 1, ctx, out);
    let lod_zero = ctx.const_uint(0);
    let size = query_image_size(ctx, img, 2, false, lod_zero, out);
    let w = extract(size, 0, ctx, out);
    let h = extract(size, 1, ctx, out);
    let to_float = |v: Word, ctx: &mut Ctx, out: &mut Vec<Instruction>| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertUToF,
            Some(float),
            Some(id),
            vec![Operand::IdRef(v)],
        ));
        id
    };
    let xf = to_float(x, ctx, out);
    let yf = to_float(y, ctx, out);
    let wf = to_float(w, ctx, out);
    let hf = to_float(h, ctx, out);
    let one = ctx.const_float(1.0);
    let two = ctx.const_float(2.0);
    let center_offset =
        |p: Word, extent: Word, ctx: &mut Ctx, out: &mut Vec<Instruction>| -> Word {
            let doubled = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FMul,
                Some(float),
                Some(doubled),
                vec![Operand::IdRef(p), Operand::IdRef(two)],
            ));
            let biased = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FAdd,
                Some(float),
                Some(biased),
                vec![Operand::IdRef(doubled), Operand::IdRef(one)],
            ));
            let scaled = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FDiv,
                Some(float),
                Some(scaled),
                vec![Operand::IdRef(biased), Operand::IdRef(extent)],
            ));
            let centered = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FSub,
                Some(float),
                Some(centered),
                vec![Operand::IdRef(scaled), Operand::IdRef(one)],
            ));
            centered
        };
    let s = center_offset(xf, wf, ctx, out);
    let t = center_offset(yf, hf, ctx, out);
    let negate = |v: Word, ctx: &mut Ctx, out: &mut Vec<Instruction>| -> Word {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FNegate,
            Some(float),
            Some(id),
            vec![Operand::IdRef(v)],
        ));
        id
    };
    let neg_one = negate(one, ctx, out);
    let neg_s = negate(s, ctx, out);
    let neg_t = negate(t, ctx, out);
    let vec3 = |c: [Word; 3], ctx: &mut Ctx, out: &mut Vec<Instruction>| -> Word {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(v3f),
            Some(id),
            c.iter().map(|w| Operand::IdRef(*w)).collect(),
        ));
        id
    };
    let dirs = [
        vec3([one, neg_t, neg_s], ctx, out),
        vec3([neg_one, neg_t, s], ctx, out),
        vec3([s, one, t], ctx, out),
        vec3([s, neg_one, neg_t], ctx, out),
        vec3([s, neg_t, one], ctx, out),
        vec3([neg_s, neg_t, neg_one], ctx, out),
    ];
    let bool_ty = ctx.ty_bool();
    let bvec3 = ctx.ty_vec_bool(3);
    let mut dir = dirs[5];
    for face_idx in (0..5).rev() {
        let face_const = ctx.const_uint(face_idx as u32);
        let is_face = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(is_face),
            vec![Operand::IdRef(face32), Operand::IdRef(face_const)],
        ));
        let is_face_v3 = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(bvec3),
            Some(is_face_v3),
            vec![
                Operand::IdRef(is_face),
                Operand::IdRef(is_face),
                Operand::IdRef(is_face),
            ],
        ));
        let selected = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(v3f),
            Some(selected),
            vec![
                Operand::IdRef(is_face_v3),
                Operand::IdRef(dirs[face_idx]),
                Operand::IdRef(dir),
            ],
        ));
        dir = selected;
    }
    let sampler = match sampler_arg {
        Some(samp) => valid_sampler_value(ctx, samp, out)?,
        None => {
            let sty = ctx.ty_sampler();
            let var = ctx.default_read_sampler()?;
            let loaded = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Load,
                Some(sty),
                Some(loaded),
                vec![Operand::IdRef(var)],
            ));
            loaded
        }
    };
    let (fallback_dim, fallback_arrayed, fallback_comp) = image_shape_or_recorded(ctx, img);
    let (img_ty, _, _, _) =
        sampled_operand_image_info(ctx, img, fallback_dim, fallback_arrayed, fallback_comp);
    let si_ty = ctx.ty_sampled_image(img_ty);
    let si = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::SampledImage,
        Some(si_ty),
        Some(si),
        vec![Operand::IdRef(img), Operand::IdRef(sampler)],
    ));
    let lod0 = ctx.const_float(0.0);
    out.push(Instruction::new(
        Op::ImageSampleExplicitLod,
        Some(fetch_v4),
        Some(color),
        vec![
            Operand::IdRef(si),
            Operand::IdRef(dir),
            Operand::ImageOperands(spirv::ImageOperands::LOD),
            Operand::IdRef(lod0),
        ],
    ));
    Ok(())
}
