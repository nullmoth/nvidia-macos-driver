use super::*;

use crate::reflect::SamplerAddressMode;

fn is_gradient_family(name: &str) -> bool {
    name.split('.').any(|token| token.ends_with("_grad"))
}

fn sample_gradient_pair(
    ctx: &Ctx,
    name: &str,
    arrayed: bool,
    args: &[Word],
    spatial: usize,
) -> Result<Option<(Word, Word, Option<Word>)>, String> {
    if !is_gradient_family(name) {
        return Ok(None);
    }
    let start = if arrayed { 4 } else { 3 };
    let is_derivative = |arg: Word| {
        value_result_type(ctx, arg)
            .and_then(|ty| vector_type_shape(ctx, ty))
            .is_some_and(|(elem, lanes)| lanes as usize == spatial && is_f32_scalar(ctx, elem))
    };
    let (Some(&dx), Some(&dy)) = (args.get(start), args.get(start + 1)) else {
        return Err(format!("{name} has no gradient operands"));
    };
    if !is_derivative(dx) || !is_derivative(dy) {
        let shape = |arg: Word| {
            value_result_type(ctx, arg).map(|ty| {
                (
                    ty,
                    vector_type_shape(ctx, ty).map(|(e, l)| (l, is_f32_scalar(ctx, e))),
                )
            })
        };
        return Err(format!(
            "{name} does not carry two {spatial}-component float derivatives after its coordinate \
             [args {} ; dx %{dx} {:?} dy %{dy} {:?}]",
            args.len(),
            shape(dx),
            shape(dy)
        ));
    }
    let mut min_lod = None;
    if let Some(&clamp) = args.get(start + 2) {
        let is_zero = value_def_instruction(ctx, clamp).is_some_and(|def| {
            matches!(def.class.opcode, Op::ConstantNull)
                || (def.class.opcode == Op::Constant
                    && def.operands.first() == Some(&Operand::LiteralBit32(0)))
        });
        if !is_zero {
            let clamp_ty = value_result_type(ctx, clamp);
            let is_float = clamp_ty.is_some_and(|ty| is_f32_scalar(ctx, ty));
            let is_bool = clamp_ty.is_some_and(|ty| {
                type_def_of(ctx, ty).is_some_and(|def| def.class.opcode == Op::TypeBool)
            });
            if is_float {
                min_lod = Some(clamp);
            } else if !is_bool {
                return Err(format!(
                    "{name} minimum-LOD clamp is neither a float scalar nor absent"
                ));
            }
        }
    }
    Ok(Some((dx, dy, min_lod)))
}

// Cube direction -> (face coordinates vec2, face int) in Metal's face order (+X -X +Y -Y +Z -Z) with the major axis chosen as
// gld.cube_vector_to_uv does (x on ties with y or z, then y over z): u = sc/|ma|/2 + 1/2, v = tc/|ma|/2 + 1/2.
fn cube_direction_to_face_uv(ctx: &mut Ctx, coord: Word, out: &mut Vec<Instruction>) -> Result<(Word, Word), String> {
    let is_vec3f = value_result_type(ctx, coord)
        .and_then(|ty| vector_type_shape(ctx, ty))
        .is_some_and(|(elem, lanes)| lanes == 3 && is_f32_scalar(ctx, elem));
    if !is_vec3f {
        return Err("air.sample_texture_cube on a 2D-array view: the direction is not a float3".into());
    }
    let (f, b, i) = (ctx.ty_float(), ctx.ty_bool(), ctx.ty_sint());
    let glsl = ctx.glsl();
    let mut emit = |ctx: &mut Ctx, op: Op, ty: Word, ops: Vec<Operand>| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(op, Some(ty), Some(id), ops));
        // later lowering (the 2D-array sample's layer and coordinate) asks for these values' types before they are placed
        ctx.phase_value_types.get_or_insert_with(Default::default).insert(id, ty);
        id
    };
    let comp = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, c: u32| {
        emit(ctx, Op::CompositeExtract, f, vec![Operand::IdRef(coord), Operand::LiteralBit32(c)])
    };
    let (x, y, z) = (comp(ctx, &mut emit, 0), comp(ctx, &mut emit, 1), comp(ctx, &mut emit, 2));
    let abs = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, v: Word| {
        emit(ctx, Op::ExtInst, f, vec![Operand::IdRef(glsl), Operand::LiteralExtInstInteger(GLSLstd450::FAbs as u32), Operand::IdRef(v)])
    };
    let (ax, ay, az) = (abs(ctx, &mut emit, x), abs(ctx, &mut emit, y), abs(ctx, &mut emit, z));
    let ge_xy = emit(ctx, Op::FOrdGreaterThanEqual, b, vec![Operand::IdRef(ax), Operand::IdRef(ay)]);
    let ge_xz = emit(ctx, Op::FOrdGreaterThanEqual, b, vec![Operand::IdRef(ax), Operand::IdRef(az)]);
    let xmaj = emit(ctx, Op::LogicalAnd, b, vec![Operand::IdRef(ge_xy), Operand::IdRef(ge_xz)]);
    let ymaj = emit(ctx, Op::FOrdGreaterThanEqual, b, vec![Operand::IdRef(ay), Operand::IdRef(az)]);
    let zero = ctx.const_float(0.0);
    let neg = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, v: Word| {
        emit(ctx, Op::FOrdLessThan, b, vec![Operand::IdRef(v), Operand::IdRef(zero)])
    };
    let (negx, negy, negz) = (neg(ctx, &mut emit, x), neg(ctx, &mut emit, y), neg(ctx, &mut emit, z));
    let minus = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, v: Word| {
        emit(ctx, Op::FNegate, f, vec![Operand::IdRef(v)])
    };
    let (mx, my, mz) = (minus(ctx, &mut emit, x), minus(ctx, &mut emit, y), minus(ctx, &mut emit, z));
    let sel = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, ty: Word, c: Word, t: Word, e: Word| {
        emit(ctx, Op::Select, ty, vec![Operand::IdRef(c), Operand::IdRef(t), Operand::IdRef(e)])
    };
    // x major: sc = -z (+X) / z (-X), tc = -y; y major: sc = x, tc = z (+Y) / -z (-Y); z major: sc = x (+Z) / -x (-Z), tc = -y
    let sc_x = sel(ctx, &mut emit, f, negx, z, mz);
    let sc_z = sel(ctx, &mut emit, f, negz, mx, x);
    let sc_yz = sel(ctx, &mut emit, f, ymaj, x, sc_z);
    let sc = sel(ctx, &mut emit, f, xmaj, sc_x, sc_yz);
    let tc_y = sel(ctx, &mut emit, f, negy, mz, z);
    let tc_yz = sel(ctx, &mut emit, f, ymaj, tc_y, my);
    let tc = sel(ctx, &mut emit, f, xmaj, my, tc_yz);
    let ma_yz = sel(ctx, &mut emit, f, ymaj, ay, az);
    let ma = sel(ctx, &mut emit, f, xmaj, ax, ma_yz);
    let face_of = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, negative: Word, pos: i64| {
        let (p, n) = (ctx.const_int_of(i, pos), ctx.const_int_of(i, pos + 1));
        emit(ctx, Op::Select, i, vec![Operand::IdRef(negative), Operand::IdRef(n), Operand::IdRef(p)])
    };
    let (fx, fy, fz) = (face_of(ctx, &mut emit, negx, 0), face_of(ctx, &mut emit, negy, 2), face_of(ctx, &mut emit, negz, 4));
    let face_yz = sel(ctx, &mut emit, i, ymaj, fy, fz);
    let face = sel(ctx, &mut emit, i, xmaj, fx, face_yz);
    let half = ctx.const_float(0.5);
    let to_unit = |ctx: &mut Ctx, emit: &mut dyn FnMut(&mut Ctx, Op, Word, Vec<Operand>) -> Word, s: Word| {
        let q = emit(ctx, Op::FDiv, f, vec![Operand::IdRef(s), Operand::IdRef(ma)]);
        let h = emit(ctx, Op::FMul, f, vec![Operand::IdRef(q), Operand::IdRef(half)]);
        emit(ctx, Op::FAdd, f, vec![Operand::IdRef(h), Operand::IdRef(half)])
    };
    let (u, v) = (to_unit(ctx, &mut emit, sc), to_unit(ctx, &mut emit, tc));
    let v2 = ctx.ty_vecf(2);
    let uv = emit(ctx, Op::CompositeConstruct, v2, vec![Operand::IdRef(u), Operand::IdRef(v)]);
    Ok((uv, face))
}

pub(in crate::passes) fn lower_sample(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.sample has no result".into()),
    };
    if args.len() < 3 {
        return Err("air.sample missing texture/sampler/coord".into());
    }
    let (mut img, samp, coord) = (resolve_image_value(ctx, args[0]), args[1], args[2]);
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
    // Apple's OpenGL samples a cube map one face at a time, with no filtering across faces, through a 2D-array view of it ("cubeAsArray": face and
    // face coordinates from gld.cube_vector_to_uv, layer = face), and keeps the real cube sample on a branch chosen by a
    // per-unit mask. A helper that takes the sampler as an argument carries both calls on the same texture, so the texture
    // is typed as a 2D array and its cube call reached the array path, reading the cube's LOD flag as a layer ("layer is not
    // scalar int/float"; Stellaris's planet shaders). The cube call becomes the same 2D-array sample of the face it hits.
    if name.starts_with("air.sample_texture_cube.") && dim == Dim::Dim2D && arrayed && !is_gradient_family(name)
        && args.len() >= 7
    {
        let mut pre = vec![];
        let (uv, face) = cube_direction_to_face_uv(ctx, coord, &mut pre)?;
        let bool_ty = ctx.ty_bool();
        let no_offset = ctx.const_bool_of(bool_ty, false);
        let offset = const_sint_vec(ctx, &[0, 0]);
        let array_args = [args[0], args[1], uv, face, no_offset, offset, args[3], args[4], args[5], args[6]];
        let array_name = name.replacen("sample_texture_cube", "sample_texture_2d_array", 1);
        pre.extend(lower_sample(ctx, &array_name, Some(res), Some(rty), &array_args, v4)?);
        return Ok(pre);
    }
    let sample_v4 = image_fetch_v4(ctx, img, v4);
    let is_int_tex = comp != crate::passes::ImageComp::Float;

    if let Some(sampler_state) = ctx
        .sampler_states
        .get(&samp)
        .copied()
        .filter(|state| state.uses_pixel_coordinates())
    {
        let pixel_linear = comp == crate::passes::ImageComp::Float
            && sampler_state.uses_linear_filter()
            && matches!(dim, Dim::Dim1D | Dim::Dim2D | Dim::Dim3D);
        let pixel_bicubic = comp == crate::passes::ImageComp::Float
            && sampler_state.uses_bicubic_filter()
            && matches!(dim, Dim::Dim1D | Dim::Dim2D);
        let pixel_fetch = comp != crate::passes::ImageComp::Float
            || sampler_state.uses_pixel_nearest()
            || arrayed;
        if pixel_linear || pixel_bicubic || pixel_fetch {
            if is_gradient_family(name) {
                return Err(format!(
                    "{name} samples through a pixel-coordinate sampler, whose emulation has no \
                     place for the gradient the call carries"
                ));
            }
            let lod = match find_sample_lod(ctx, arrayed, args, &mut out) {
                Some(lod) => sample_lod_to_fetch_lod(ctx, lod, &mut out)?,
                None => ctx.const_uint(0),
            };
            if pixel_linear {
                let color = lower_pixel_linear_sample(
                    ctx,
                    sampler_state,
                    img,
                    dim,
                    arrayed,
                    coord,
                    args,
                    lod,
                    sample_v4,
                    &mut out,
                )?;
                return finish_sample_result(ctx, res, rty, color, sample_v4, out);
            }
            if pixel_bicubic {
                let color = lower_pixel_bicubic_sample(
                    ctx,
                    sampler_state,
                    img,
                    dim,
                    arrayed,
                    coord,
                    args,
                    lod,
                    sample_v4,
                    false,
                    &mut out,
                )?;
                return finish_sample_result(ctx, res, rty, color, sample_v4, out);
            }
            let fetch = build_pixel_fetch_coord(
                ctx,
                sampler_state,
                img,
                dim,
                arrayed,
                coord,
                args,
                lod,
                &mut out,
            )?;
            let mut color = ctx.module.fresh_id();
            push_image_read_or_fetch(ctx, &mut out, img, fetch.coord, Some(lod), sample_v4, color)?;
            if let Some(in_bounds) = fetch.in_bounds {
                let guarded = ctx.module.fresh_id();
                let zero_color = const_null_of(ctx, sample_v4);
                out.push(Instruction::new(
                    Op::Select,
                    Some(sample_v4),
                    Some(guarded),
                    vec![
                        Operand::IdRef(in_bounds),
                        Operand::IdRef(color),
                        Operand::IdRef(zero_color),
                    ],
                ));
                color = guarded;
            }
            return finish_sample_result(ctx, res, rty, color, sample_v4, out);
        }
        return Err(format!(
            "pixel-coordinate sampling does not support {dim:?} with {:?} filtering",
            sampler_state.min_filter
        ));
    }
    if let Some(sampler_state) = ctx
        .sampler_states
        .get(&samp)
        .copied()
        .filter(|state| state.uses_bicubic_filter())
    {
        if comp == crate::passes::ImageComp::Float && matches!(dim, Dim::Dim1D | Dim::Dim2D) {
            let lod = match find_sample_lod(ctx, arrayed, args, &mut out) {
                Some(lod) => sample_lod_to_fetch_lod(ctx, lod, &mut out)?,
                None => ctx.const_uint(0),
            };
            let color = lower_pixel_bicubic_sample(
                ctx,
                sampler_state,
                img,
                dim,
                arrayed,
                coord,
                args,
                lod,
                sample_v4,
                true,
                &mut out,
            )?;
            return finish_sample_result(ctx, res, rty, color, sample_v4, out);
        }
        return Err(
            "air.sample_texture bicubic filtering is supported only for 1D/2D float textures"
                .into(),
        );
    }
    if is_int_tex && is_gradient_family(name) {
        return Err(format!(
            "{name} samples an integer texture, whose nearest-fetch emulation has no place for \
             the gradient the call carries"
        ));
    }
    if is_int_tex {
        if let Some(sampler_state) = ctx.sampler_states.get(&samp).copied() {
            let lod = match find_sample_lod(ctx, arrayed, args, &mut out) {
                Some(lod) => sample_lod_to_fetch_lod(ctx, lod, &mut out)?,
                None => ctx.const_uint(0),
            };
            let fetch = build_normalized_nearest_fetch_coord(
                ctx,
                sampler_state,
                img,
                dim,
                arrayed,
                coord,
                args,
                lod,
                &mut out,
            )?;
            let mut color = ctx.module.fresh_id();
            push_image_read_or_fetch(ctx, &mut out, img, fetch.coord, Some(lod), sample_v4, color)?;
            if let Some(in_bounds) = fetch.in_bounds {
                let guarded = ctx.module.fresh_id();
                let zero_color = const_null_of(ctx, sample_v4);
                out.push(Instruction::new(
                    Op::Select,
                    Some(sample_v4),
                    Some(guarded),
                    vec![
                        Operand::IdRef(in_bounds),
                        Operand::IdRef(color),
                        Operand::IdRef(zero_color),
                    ],
                ));
                color = guarded;
            }
            return finish_sample_result(ctx, res, rty, color, sample_v4, out);
        }
    }

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

    let mut coord_for_sample = build_sample_coord(ctx, dim, arrayed, coord, args, &mut out)?;
    let spatial = sample_spatial_dims(dim);
    // `sample_spatial_dims` leaves cube maps out because they take no texel offset, but their explicit
    // gradients are 3-component direction vectors (SPIR-V Grad operands on a Cube image carry 3 lanes).
    let gradient_lanes = match dim {
        Dim::DimCube => 3,
        _ => spatial.unwrap_or(0),
    };
    let grad3 = sample_gradient_pair(ctx, name, arrayed, args, gradient_lanes)?;
    let min_lod = grad3.and_then(|(_, _, clamp)| clamp);
    let grad = grad3.map(|(dx, dy, _)| (dx, dy));
    if min_lod.is_some() {
        let has = ctx.module.capabilities.iter().any(|inst| {
            matches!(
                inst.operands.first(),
                Some(Operand::Capability(spirv::Capability::MinLod))
            )
        });
        if !has {
            ctx.module.capabilities.push(Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(spirv::Capability::MinLod)],
            ));
        }
    }
    let level = if grad.is_some() {
        None
    } else {
        find_sample_level(ctx, arrayed, args, &mut out)
    };
    let (const_offset, dynamic_offset) = if let Some(spatial) = spatial {
        let (const_offset, dynamic_offset) =
            sample_const_or_dynamic_offset(ctx, arrayed, args, spatial as u32)?;
        let const_offset = const_offset.and_then(|offset| {
            if offset.iter().all(|delta| *delta == 0) {
                None
            } else {
                Some(const_sint_vec(ctx, &offset))
            }
        });
        (const_offset, dynamic_offset)
    } else {
        (None, None)
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
    push_image_sample_min_lod(
        ctx,
        &mut out,
        sample_v4,
        color,
        si,
        coord_for_sample,
        level,
        is_int_tex,
        const_offset,
        grad,
        min_lod,
    );

    finish_sample_result(ctx, res, rty, color, sample_v4, out)
}

#[allow(clippy::too_many_arguments)]
fn lower_pixel_bicubic_sample(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    args: &[Word],
    lod: Word,
    sample_v4: Word,
    normalized_coordinates: bool,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let spatial = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        _ => return Err("air.sample_texture bicubic sample unsupported dimension".into()),
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
                .ok_or("air.sample_texture bicubic array texture missing layer")?,
        )
    } else {
        None
    };
    let size = query_image_size(ctx, img, spatial, arrayed, lod, out);
    let float_ty = ctx.ty_float();
    let sint = ctx.ty_sint();
    let bool_ty = ctx.ty_bool();
    let glsl = ctx.glsl();
    let zero_f = ctx.const_float(0.0);
    let half_f = ctx.const_float(0.5);
    let mut base = Vec::with_capacity(spatial);
    let mut axis_weights = Vec::with_capacity(spatial);
    for (axis, component) in sample_coord_components(ctx, coord, spatial as u32, out)?
        .into_iter()
        .enumerate()
    {
        let Operand::IdRef(component) = component else {
            return Err("air.sample_texture bicubic coord component is not an id".into());
        };
        let component = if normalized_coordinates {
            let extent = if spatial == 1 {
                size
            } else {
                let extent = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(ctx.ty_uint()),
                    Some(extent),
                    vec![Operand::IdRef(size), Operand::LiteralBit32(axis as u32)],
                ));
                extent
            };
            let extent_f = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ConvertUToF,
                Some(float_ty),
                Some(extent_f),
                vec![Operand::IdRef(extent)],
            ));
            let pixel_component = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FMul,
                Some(float_ty),
                Some(pixel_component),
                vec![Operand::IdRef(component), Operand::IdRef(extent_f)],
            ));
            pixel_component
        } else {
            component
        };
        let component = clamp_pixel_coord_component_finite(
            ctx,
            sampler_state,
            component,
            size,
            spatial > 1,
            axis as u32,
            out,
        );
        let biased = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(biased),
            vec![Operand::IdRef(component), Operand::IdRef(half_f)],
        ));
        let floor = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(floor),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(8),
                Operand::IdRef(biased),
            ],
        ));
        let mut base_i = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertFToS,
            Some(sint),
            Some(base_i),
            vec![Operand::IdRef(floor)],
        ));
        if let Some(offset) = &offset {
            if offset[axis] != 0 {
                let shifted = ctx.module.fresh_id();
                let delta = ctx.const_int_of(sint, offset[axis] as i64);
                out.push(Instruction::new(
                    Op::IAdd,
                    Some(sint),
                    Some(shifted),
                    vec![Operand::IdRef(base_i), Operand::IdRef(delta)],
                ));
                base_i = shifted;
            }
        } else if let Some(offset) = &dynamic_offset {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IAdd,
                Some(sint),
                Some(shifted),
                vec![Operand::IdRef(base_i), Operand::IdRef(offset[axis])],
            ));
            base_i = shifted;
        }
        let frac = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(frac),
            vec![Operand::IdRef(biased), Operand::IdRef(floor)],
        ));
        base.push(base_i);
        axis_weights.push(catmull_rom_weights(ctx, frac, out));
    }

    let zero_color = const_null_of(ctx, sample_v4);
    let mut acc = zero_color;
    for tap in 0..4usize.pow(spatial as u32) {
        let tap_indices = (0..spatial)
            .map(|axis| (tap / 4usize.pow(axis as u32)) % 4)
            .collect::<Vec<_>>();
        let tap_offsets = tap_indices
            .iter()
            .map(|index| *index as i32 - 1)
            .collect::<Vec<_>>();
        let tap_coord = pixel_linear_tap_coord(
            ctx,
            sampler_state,
            dim,
            arrayed,
            &base,
            &tap_offsets,
            layer,
            size,
            out,
        )?;
        let fetched = ctx.module.fresh_id();
        push_image_read_or_fetch(
            ctx,
            out,
            img,
            tap_coord.coord,
            Some(lod),
            sample_v4,
            fetched,
        )?;
        let color = if let Some(in_bounds) = tap_coord.in_bounds {
            let guarded = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Select,
                Some(sample_v4),
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
        };
        let mut weight = axis_weights[0][tap_indices[0]];
        for (axis, weights) in axis_weights.iter().enumerate().skip(1) {
            let combined = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FMul,
                Some(float_ty),
                Some(combined),
                vec![
                    Operand::IdRef(weight),
                    Operand::IdRef(weights[tap_indices[axis]]),
                ],
            ));
            weight = combined;
        }
        let weighted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::VectorTimesScalar,
            Some(sample_v4),
            Some(weighted),
            vec![Operand::IdRef(color), Operand::IdRef(weight)],
        ));
        let weight_is_zero = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(weight_is_zero),
            vec![Operand::IdRef(weight), Operand::IdRef(zero_f)],
        ));
        let contribution = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(sample_v4),
            Some(contribution),
            vec![
                Operand::IdRef(weight_is_zero),
                Operand::IdRef(zero_color),
                Operand::IdRef(weighted),
            ],
        ));
        let sum = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FAdd,
            Some(sample_v4),
            Some(sum),
            vec![Operand::IdRef(acc), Operand::IdRef(contribution)],
        ));
        acc = sum;
    }
    Ok(acc)
}

fn catmull_rom_weights(ctx: &mut Ctx, t: Word, out: &mut Vec<Instruction>) -> [Word; 4] {
    let float_ty = ctx.ty_float();
    let c_half = ctx.const_float(0.5);
    let c_neg_half = ctx.const_float(-0.5);
    let c_one = ctx.const_float(1.0);
    let c_one_half = ctx.const_float(1.5);
    let c_two = ctx.const_float(2.0);
    let c_two_half = ctx.const_float(2.5);
    let mul = |ctx: &mut Ctx, a, b, out: &mut Vec<Instruction>| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FMul,
            Some(float_ty),
            Some(id),
            vec![Operand::IdRef(a), Operand::IdRef(b)],
        ));
        id
    };
    let add = |ctx: &mut Ctx, op, a, b, out: &mut Vec<Instruction>| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            op,
            Some(float_ty),
            Some(id),
            vec![Operand::IdRef(a), Operand::IdRef(b)],
        ));
        id
    };
    let t2 = mul(ctx, t, t, out);
    let t3 = mul(ctx, t2, t, out);
    let w0a = mul(ctx, c_neg_half, t, out);
    let w0b = mul(ctx, c_one, t2, out);
    let w0c = mul(ctx, c_neg_half, t3, out);
    let w0ab = add(ctx, Op::FAdd, w0a, w0b, out);
    let w0 = add(ctx, Op::FAdd, w0ab, w0c, out);
    let w1a = mul(ctx, c_two_half, t2, out);
    let w1b = mul(ctx, c_one_half, t3, out);
    let w1a = add(ctx, Op::FSub, c_one, w1a, out);
    let w1 = add(ctx, Op::FAdd, w1a, w1b, out);
    let w2a = mul(ctx, c_half, t, out);
    let w2b = mul(ctx, c_two, t2, out);
    let w2c = mul(ctx, c_one_half, t3, out);
    let w2ab = add(ctx, Op::FAdd, w2a, w2b, out);
    let w2 = add(ctx, Op::FSub, w2ab, w2c, out);
    let w3a = mul(ctx, c_neg_half, t2, out);
    let w3b = mul(ctx, c_half, t3, out);
    let w3 = add(ctx, Op::FAdd, w3a, w3b, out);
    [w0, w1, w2, w3]
}

pub(in crate::passes) fn clamp_pixel_coord_component_finite(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    comp: Word,
    size: Word,
    size_is_vector: bool,
    axis: u32,
    out: &mut Vec<Instruction>,
) -> Word {
    let float_ty = ctx.ty_float();
    let uint = ctx.ty_uint();
    let glsl = ctx.glsl();
    let size_axis = if size_is_vector {
        let c = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(c),
            vec![Operand::IdRef(size), Operand::LiteralBit32(axis)],
        ));
        c
    } else {
        size
    };
    let size_f = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ConvertUToF,
        Some(float_ty),
        Some(size_f),
        vec![Operand::IdRef(size_axis)],
    ));
    let period = match sampler_state.spatial_address_mode(axis as usize) {
        Some(SamplerAddressMode::Repeat) => Some(size_f),
        Some(SamplerAddressMode::MirroredRepeat) => {
            let two = ctx.const_float(2.0);
            let period = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FMul,
                Some(float_ty),
                Some(period),
                vec![Operand::IdRef(size_f), Operand::IdRef(two)],
            ));
            Some(period)
        }
        _ => None,
    };
    let Some(period) = period else {
        let slack = ctx.const_float(9.0);
        let hi = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FAdd,
            Some(float_ty),
            Some(hi),
            vec![Operand::IdRef(size_f), Operand::IdRef(slack)],
        ));
        let lo = ctx.const_float(-9.0);
        return nclamp_float(ctx, comp, lo, hi, out);
    };
    let hi = ctx.const_float(4_194_304.0);
    let lo = ctx.const_float(-4_194_304.0);
    let finite = nclamp_float(ctx, comp, lo, hi, out);
    let quotient = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FDiv,
        Some(float_ty),
        Some(quotient),
        vec![Operand::IdRef(finite), Operand::IdRef(period)],
    ));
    let whole = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(whole),
        vec![
            Operand::IdRef(glsl),
            Operand::LiteralExtInstInteger(8),
            Operand::IdRef(quotient),
        ],
    ));
    let periods = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FMul,
        Some(float_ty),
        Some(periods),
        vec![Operand::IdRef(period), Operand::IdRef(whole)],
    ));
    let wrapped = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FSub,
        Some(float_ty),
        Some(wrapped),
        vec![Operand::IdRef(finite), Operand::IdRef(periods)],
    ));
    wrapped
}

fn nclamp_float(ctx: &mut Ctx, x: Word, lo: Word, hi: Word, out: &mut Vec<Instruction>) -> Word {
    let float_ty = ctx.ty_float();
    let glsl = ctx.glsl();
    let clamped = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(clamped),
        vec![
            Operand::IdRef(glsl),
            Operand::LiteralExtInstInteger(81),
            Operand::IdRef(x),
            Operand::IdRef(lo),
            Operand::IdRef(hi),
        ],
    ));
    clamped
}

#[allow(clippy::too_many_arguments)]
pub(in crate::passes) fn lower_pixel_linear_sample(
    ctx: &mut Ctx,
    sampler_state: StaticSamplerState,
    img: Word,
    dim: Dim,
    arrayed: bool,
    coord: Word,
    args: &[Word],
    lod: Word,
    sample_v4: Word,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let spatial: usize = match dim {
        Dim::Dim1D => 1,
        Dim::Dim2D => 2,
        Dim::Dim3D => 3,
        _ => return Err("air.sample_texture pixel linear sample unsupported dimension".into()),
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
    let bool_ty = ctx.ty_bool();
    let glsl = ctx.glsl();
    let zero_f = ctx.const_float(0.0);
    let one_f = ctx.const_float(1.0);
    let half_f = ctx.const_float(0.5);
    let mut base = Vec::with_capacity(spatial);
    let mut frac = Vec::with_capacity(spatial);
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
        let biased_coord = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(biased_coord),
            vec![Operand::IdRef(comp), Operand::IdRef(half_f)],
        ));
        let floor = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(floor),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(8),
                Operand::IdRef(biased_coord),
            ],
        ));
        let mut base_i = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertFToS,
            Some(sint),
            Some(base_i),
            vec![Operand::IdRef(floor)],
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
                    vec![Operand::IdRef(base_i), Operand::IdRef(delta)],
                ));
                base_i = shifted;
            }
        } else if let Some(offset) = &dynamic_offset {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IAdd,
                Some(sint),
                Some(shifted),
                vec![Operand::IdRef(base_i), Operand::IdRef(offset[idx])],
            ));
            base_i = shifted;
        }
        let frac_i = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(frac_i),
            vec![Operand::IdRef(biased_coord), Operand::IdRef(floor)],
        ));
        base.push(base_i);
        frac.push(frac_i);
    }

    let mut axis_weights = Vec::with_capacity(spatial);
    for &frac_axis in frac.iter().take(spatial) {
        let one_minus = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float_ty),
            Some(one_minus),
            vec![Operand::IdRef(one_f), Operand::IdRef(frac_axis)],
        ));
        axis_weights.push([one_minus, frac_axis]);
    }
    let zero_color = const_null_of(ctx, sample_v4);
    let mut acc = zero_color;
    for tap in 0..(1usize << spatial) {
        let tap_offset: Vec<i32> = (0..spatial)
            .map(|axis| ((tap >> axis) & 1) as i32)
            .collect();
        let tap_coord = pixel_linear_tap_coord(
            ctx,
            sampler_state,
            dim,
            arrayed,
            &base,
            &tap_offset,
            layer,
            size,
            out,
        )?;
        let fetched = ctx.module.fresh_id();
        push_image_read_or_fetch(
            ctx,
            out,
            img,
            tap_coord.coord,
            Some(lod),
            sample_v4,
            fetched,
        )?;
        let color = if let Some(in_bounds) = tap_coord.in_bounds {
            let guarded = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Select,
                Some(sample_v4),
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
        };
        let mut weight = axis_weights[0][tap & 1];
        for (axis, weights) in axis_weights.iter().enumerate().skip(1) {
            let combined = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FMul,
                Some(float_ty),
                Some(combined),
                vec![
                    Operand::IdRef(weight),
                    Operand::IdRef(weights[(tap >> axis) & 1]),
                ],
            ));
            weight = combined;
        }
        let weighted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::VectorTimesScalar,
            Some(sample_v4),
            Some(weighted),
            vec![Operand::IdRef(color), Operand::IdRef(weight)],
        ));
        let weight_is_zero = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(weight_is_zero),
            vec![Operand::IdRef(weight), Operand::IdRef(zero_f)],
        ));
        let contribution = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(sample_v4),
            Some(contribution),
            vec![
                Operand::IdRef(weight_is_zero),
                Operand::IdRef(zero_color),
                Operand::IdRef(weighted),
            ],
        ));
        let sum = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FAdd,
            Some(sample_v4),
            Some(sum),
            vec![Operand::IdRef(acc), Operand::IdRef(contribution)],
        ));
        acc = sum;
    }
    Ok(acc)
}

pub(in crate::passes) fn finish_sample_result(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    color: Word,
    color_ty: Word,
    mut out: Vec<Instruction>,
) -> Result<Vec<Instruction>, String> {
    let rdef = type_def_of(ctx, rty);
    let is_struct = rdef
        .as_ref()
        .map(|d| d.class.opcode == Op::TypeStruct)
        .unwrap_or(false);
    if is_struct {
        let member0 = rdef
            .as_ref()
            .and_then(|d| d.operands.first())
            .and_then(|o| match o {
                Operand::IdRef(m) => Some(*m),
                _ => None,
            });
        let color_for_struct = match member0 {
            Some(m) if m != color_ty && is_half_vector(ctx, m) => {
                let c = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::FConvert,
                    Some(m),
                    Some(c),
                    vec![Operand::IdRef(color)],
                ));
                c
            }
            Some(m) => coerce_same_shape_integer(ctx, &mut out, color, color_ty, m)?,
            _ => color,
        };
        let i8u = ctx.ty_int8();
        let undef8 = ctx.module.fresh_id();
        out.push(Instruction::new(Op::Undef, Some(i8u), Some(undef8), vec![]));
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(color_for_struct), Operand::IdRef(undef8)],
        ));
    } else if rty != color_ty && is_half_vector(ctx, rty) {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(color)],
        ));
    } else if resolve::integer_shape(ctx, rty).is_some() {
        let c = coerce_same_shape_integer(ctx, &mut out, color, color_ty, rty)?;
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(c)],
        ));
    } else {
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(color)],
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Module;

    #[test]
    fn normalized_bicubic_scales_coordinates_and_emits_sixteen_fetches() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim2D, false, crate::passes::ImageComp::Float);
        let image = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Undef,
            Some(image_ty),
            Some(image),
            vec![],
        ));
        let coord_ty = ctx.ty_vecf(2);
        let x = ctx.const_float(0.25);
        let y = ctx.const_float(0.75);
        let coord = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::ConstantComposite,
            Some(coord_ty),
            Some(coord),
            vec![Operand::IdRef(x), Operand::IdRef(y)],
        ));
        let sample_v4 = ctx.ty_vech(4);
        let lod = ctx.const_uint(0);
        let sampler_state = StaticSamplerState::from_air_words([34901797601023049, 0])
            .expect("normalized bicubic AIR sampler state");
        assert!(sampler_state.uses_bicubic_filter());
        assert!(!sampler_state.uses_pixel_coordinates());

        let mut out = Vec::new();
        lower_pixel_bicubic_sample(
            &mut ctx,
            sampler_state,
            image,
            Dim::Dim2D,
            false,
            coord,
            &[image, 0, coord],
            lod,
            sample_v4,
            true,
            &mut out,
        )
        .expect("normalized bicubic lowering");

        assert_eq!(
            out.iter()
                .filter(|inst| inst.class.opcode == Op::ImageFetch)
                .count(),
            16
        );
        let size_ids = out
            .iter()
            .filter(|inst| inst.class.opcode == Op::ImageQuerySizeLod)
            .filter_map(|inst| inst.result_id)
            .collect::<HashSet<_>>();
        let extent_float_ids = out
            .iter()
            .filter(|inst| inst.class.opcode == Op::ConvertUToF)
            .filter(|inst| {
                let Some(Operand::IdRef(source)) = inst.operands.first() else {
                    return false;
                };
                out.iter().any(|candidate| {
                    candidate.result_id == Some(*source)
                        && candidate.class.opcode == Op::CompositeExtract
                        && matches!(candidate.operands.first(), Some(Operand::IdRef(id)) if size_ids.contains(id))
                })
            })
            .filter_map(|inst| inst.result_id)
            .collect::<HashSet<_>>();
        let scaled_extent_ids = extent_float_ids
            .iter()
            .filter(|extent| {
                out.iter().any(|inst| {
                    inst.class.opcode == Op::FMul
                        && inst
                            .operands
                            .iter()
                            .any(|operand| operand == &Operand::IdRef(**extent))
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(scaled_extent_ids.len(), 2);
    }

    #[test]
    fn a_repeating_bicubic_wraps_the_coordinate_rather_than_clamping_it() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim2D, false, crate::passes::ImageComp::Float);
        let image = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Undef,
            Some(image_ty),
            Some(image),
            vec![],
        ));
        let coord_ty = ctx.ty_vecf(2);
        let x = ctx.const_float(3.0625);
        let y = ctx.const_float(0.4375);
        let coord = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::ConstantComposite,
            Some(coord_ty),
            Some(coord),
            vec![Operand::IdRef(x), Operand::IdRef(y)],
        ));
        let sample_v4 = ctx.ty_vecf(4);
        let lod = ctx.const_uint(0);
        let sampler_state = StaticSamplerState::from_air_words([34901797601023122, 0])
            .expect("bicubic repeat AIR sampler state");
        assert!(sampler_state.uses_bicubic_filter());
        assert!(!sampler_state.uses_pixel_coordinates());
        for axis in 0..2 {
            assert_eq!(
                sampler_state.spatial_address_mode(axis),
                Some(SamplerAddressMode::Repeat)
            );
        }

        let mut out = Vec::new();
        lower_pixel_bicubic_sample(
            &mut ctx,
            sampler_state,
            image,
            Dim::Dim2D,
            false,
            coord,
            &[image, 0, coord],
            lod,
            sample_v4,
            true,
            &mut out,
        )
        .expect("bicubic repeat lowering");

        let float_constant = |value: f32, ctx: &Ctx| {
            ctx.new_globals.iter().any(|inst| {
                inst.class.opcode == Op::Constant
                    && inst.operands.first() == Some(&Operand::LiteralBit32(value.to_bits()))
            })
        };
        for slack in [9.0f32, -9.0f32] {
            assert!(
                !float_constant(slack, &ctx),
                "a repeating axis must not saturate to size {slack:+}"
            );
        }

        let floored_quotients = out
            .iter()
            .filter(|inst| {
                inst.class.opcode == Op::ExtInst
                    && inst.operands.get(1) == Some(&Operand::LiteralExtInstInteger(8))
            })
            .filter(|inst| {
                let Some(Operand::IdRef(source)) = inst.operands.get(2) else {
                    return false;
                };
                out.iter().any(|candidate| {
                    candidate.result_id == Some(*source) && candidate.class.opcode == Op::FDiv
                })
            })
            .filter_map(|inst| inst.result_id)
            .collect::<HashSet<_>>();
        assert_eq!(floored_quotients.len(), 2);
        let wraps = out
            .iter()
            .filter(|inst| inst.class.opcode == Op::FSub)
            .filter(|inst| {
                let Some(Operand::IdRef(subtrahend)) = inst.operands.get(1) else {
                    return false;
                };
                out.iter().any(|candidate| {
                    candidate.result_id == Some(*subtrahend)
                        && candidate.class.opcode == Op::FMul
                        && candidate.operands.iter().any(|operand| {
                            matches!(operand, Operand::IdRef(id) if floored_quotients.contains(id))
                        })
                })
            })
            .count();
        assert_eq!(wraps, 2);
    }

    #[test]
    fn normalized_bicubic_1d_emits_four_fetches() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim1D, false, crate::passes::ImageComp::Float);
        let image = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Undef,
            Some(image_ty),
            Some(image),
            vec![],
        ));
        let coord = ctx.const_float(0.25);
        let sample_v4 = ctx.ty_vech(4);
        let lod = ctx.const_uint(0);
        let sampler_state = StaticSamplerState::from_air_words([34901797601023049, 0])
            .expect("normalized bicubic AIR sampler state");
        let mut out = Vec::new();
        lower_pixel_bicubic_sample(
            &mut ctx,
            sampler_state,
            image,
            Dim::Dim1D,
            false,
            coord,
            &[image, 0, coord],
            lod,
            sample_v4,
            true,
            &mut out,
        )
        .expect("normalized 1D bicubic lowering");
        assert_eq!(
            out.iter()
                .filter(|inst| inst.class.opcode == Op::ImageFetch)
                .count(),
            4
        );
    }
}
