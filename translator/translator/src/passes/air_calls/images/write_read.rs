use super::*;

pub(in crate::passes) fn lower_write(
    ctx: &mut Ctx,
    name: &str,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    if args.len() < 3 {
        return Err("air.write_texture missing image/coord/texel".into());
    }
    let (dim, arrayed) = intrinsic_texture_shape(name)
        .ok_or_else(|| format!("unsupported air.write_texture intrinsic shape: {name}"))?;
    let mut img = resolve_image_value(ctx, args[0]);
    if !image_is_storage(ctx, img) {
        if let Some(storage_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Storage)
        {
            img = storage_img;
        } else if texture_operand_is_absent(ctx, img) {
            return lower_absent_texture_write(ctx);
        } else {
            return Err(format!(
                "air.write_texture on non-storage image id {img} ({})",
                describe_value(ctx, img)
            ));
        }
    }
    ctx.require_runtime_storage_image_use(img, RuntimeStorageImageUse::Write)?;
    let mut out = vec![];
    let (_, _, comp) = image_shape_or_recorded(ctx, img);
    if crate::env_vars::tex_dbg() {
        let tys: Vec<String> = args
            .iter()
            .map(|a| {
                value_result_type(ctx, *a)
                    .and_then(|t| type_def_of(ctx, t))
                    .map(|d| format!("{}:{:?}", a, d.class.opcode))
                    .unwrap_or_else(|| format!("{a}:?"))
            })
            .collect();
        eprintln!("WRITE-TEX img {img} dim {dim:?} arrayed {arrayed} args {tys:?}");
    }
    let (coord, layer, texel) = if dim == Dim::DimCube && arrayed {
        if args.len() < 5 {
            return Err("air.write_texture cube array write missing face/slice/texel".into());
        }
        let layer = combine_cube_array_layer(
            ctx,
            args[2],
            args[3],
            &mut out,
            "air.write_texture cube array layer",
        )?;
        (args[1], Some(layer), args[4])
    } else if dim == Dim::DimCube && !arrayed {
        if args.len() < 4 {
            return Err("air.write_texture cube write missing face/texel".into());
        }
        (args[1], Some(args[2]), args[3])
    } else if arrayed {
        if args.len() < 4 {
            return Err("air.write_texture array write missing layer/texel".into());
        }
        (args[1], Some(args[2]), args[3])
    } else {
        (args[1], None, args[2])
    };

    let coord32 = build_fetch_coord(ctx, dim, arrayed, coord, layer, &mut out)?;

    let texel32 = match comp {
        crate::passes::ImageComp::Float => match vector_shape(ctx, texel) {
            Some((elem, 4)) if is_float_width(ctx, elem, 32) => texel,
            Some((elem, 4)) if is_float_width(ctx, elem, 16) => {
                let t = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::FConvert,
                    Some(v4),
                    Some(t),
                    vec![Operand::IdRef(texel)],
                ));
                t
            }
            _ => {
                let ty = value_result_type(ctx, texel);
                let ty_def = ty
                    .and_then(|t| crate::passes::value_queries::type_def_of(ctx, t))
                    .map(|d| format!("{:?} {:?}", d.class.opcode, d.operands));
                let elem_def = vector_shape(ctx, texel)
                    .and_then(|(e, _)| crate::passes::value_queries::type_def_of(ctx, e))
                    .map(|d| format!("{:?} {:?}", d.class.opcode, d.operands));
                return Err(format!(
                    "air.write_texture: unsupported texel shape id {texel} type {ty:?} = {ty_def:?} elem {elem_def:?} ({})",
                    describe_value(ctx, texel)
                ));
            }
        },
        crate::passes::ImageComp::Uint | crate::passes::ImageComp::Sint => {
            let texel_ty = value_result_type(ctx, texel)
                .ok_or("air.write_texture: integer texel has no result type")?;
            if !matches!(resolve::integer_shape(ctx, texel_ty), Some((_bits, 4))) {
                return Err("air.write_texture: unsupported integer texel shape".into());
            }
            let target_ty = match comp {
                crate::passes::ImageComp::Uint => ctx.ty_vec_uint(4),
                crate::passes::ImageComp::Sint => ctx.ty_vec_sint(4),
                crate::passes::ImageComp::Float => {
                    return Err("air.write_texture: Float comp in the integer texel path".into())
                }
            };
            coerce_same_shape_integer(ctx, &mut out, texel, texel_ty, target_ty)?
        }
    };

    let texel32 = preserve_defined_texel_lanes(ctx, &mut out, img, coord32, texel, texel32)?;

    out.push(Instruction::new(
        Op::ImageWrite,
        None,
        None,
        vec![
            Operand::IdRef(img),
            Operand::IdRef(coord32),
            Operand::IdRef(texel32),
        ],
    ));
    Ok(out)
}

fn preserve_defined_texel_lanes(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    img: Word,
    coord32: Word,
    source_texel: Word,
    write_texel: Word,
) -> Result<Word, String> {
    let undefined = (0..4)
        .map(|lane| texel_lane_is_statically_undef(ctx, out, source_texel, lane))
        .collect::<Vec<_>>();
    if undefined.iter().all(|is_undef| !*is_undef) {
        return Ok(write_texel);
    }

    let Some(write_ty) = pending_or_module_result_type(ctx, out, write_texel) else {
        return Ok(write_texel);
    };
    let lane_ty = element_type(ctx, write_ty);
    let current = ctx.module.fresh_id();
    ctx.require_runtime_storage_image_use(img, RuntimeStorageImageUse::Read)?;
    out.push(Instruction::new(
        Op::ImageRead,
        Some(write_ty),
        Some(current),
        vec![Operand::IdRef(img), Operand::IdRef(coord32)],
    ));

    let mut merged = current;
    for (lane, is_undef) in undefined.into_iter().enumerate() {
        if is_undef {
            continue;
        }
        let lane_value = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(lane_ty),
            Some(lane_value),
            vec![
                Operand::IdRef(write_texel),
                Operand::LiteralBit32(lane as u32),
            ],
        ));
        let next = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeInsert,
            Some(write_ty),
            Some(next),
            vec![
                Operand::IdRef(lane_value),
                Operand::IdRef(merged),
                Operand::LiteralBit32(lane as u32),
            ],
        ));
        merged = next;
    }
    Ok(merged)
}

fn pending_or_module_result_type(ctx: &Ctx, pending: &[Instruction], value: Word) -> Option<Word> {
    pending
        .iter()
        .rev()
        .find(|inst| inst.result_id == Some(value))
        .and_then(|inst| inst.result_type)
        .or_else(|| value_result_type(ctx, value))
}

fn texel_lane_is_statically_undef(
    ctx: &Ctx,
    pending: &[Instruction],
    value: Word,
    lane: usize,
) -> bool {
    let Some(inst) = pending
        .iter()
        .rev()
        .find(|inst| inst.result_id == Some(value))
        .or_else(|| value_inst(ctx, value))
    else {
        return false;
    };
    match inst.class.opcode {
        Op::Undef => true,
        Op::CompositeConstruct => inst
            .operands
            .get(lane)
            .and_then(|operand| match operand {
                Operand::IdRef(id) => Some(texel_lane_is_statically_undef(ctx, pending, *id, 0)),
                _ => None,
            })
            .unwrap_or(false),
        Op::CompositeInsert => {
            let inserted = inst.operands.first().and_then(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            });
            let base = inst.operands.get(1).and_then(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            });
            let target_lane = inst.operands.get(2).and_then(|operand| match operand {
                Operand::LiteralBit32(index) => Some(*index as usize),
                _ => None,
            });
            if target_lane == Some(lane) {
                inserted
                    .map(|id| texel_lane_is_statically_undef(ctx, pending, id, 0))
                    .unwrap_or(false)
            } else {
                base.map(|id| texel_lane_is_statically_undef(ctx, pending, id, lane))
                    .unwrap_or(false)
            }
        }
        Op::CopyObject | Op::UConvert | Op::SConvert | Op::FConvert | Op::Bitcast => inst
            .operands
            .first()
            .and_then(|operand| match operand {
                Operand::IdRef(id) => Some(texel_lane_is_statically_undef(ctx, pending, *id, lane)),
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

pub(in crate::passes) fn lower_read(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.read_texture has no result".into()),
    };
    if args.len() < 2 {
        return Err("air.read_texture missing texture/coord".into());
    }
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
    if image_is_storage(ctx, img) {
        ctx.require_runtime_storage_image_use(img, RuntimeStorageImageUse::Read)?;
    }
    let (dim, arrayed) = ctx
        .image_dims
        .get(&img)
        .copied()
        .unwrap_or((Dim::Dim2D, false));
    let cube = dim == Dim::DimCube && !arrayed;
    let cube_array = dim == Dim::DimCube && arrayed;
    if cube_array && !image_is_storage(ctx, img) {
        return Err("air.read_texture on a sampled cube array texture is unsupported".into());
    }
    let (coord, layer, lod, slice) = if cube_array {
        let base = if value_is_int_or_intvec(ctx, args[1]) {
            1
        } else {
            2
        };
        let lod_at = if base == 2 && args.len() >= 8 {
            base + 4
        } else {
            base + 3
        };
        (
            *args.get(base).ok_or("air.read_texture missing coord")?,
            args.get(base + 1).copied(),
            args.get(lod_at).copied(),
            Some(
                *args
                    .get(base + 2)
                    .ok_or("air.read_texture cube array read missing slice")?,
            ),
        )
    } else {
        let (coord, layer, lod) = read_operands(ctx, args, arrayed, cube)?;
        (coord, layer, lod, None)
    };
    let mut out = vec![];
    let layer = match (layer, slice) {
        (Some(face), Some(slice)) => Some(combine_cube_array_layer(
            ctx,
            face,
            slice,
            &mut out,
            "air.read_texture cube array layer",
        )?),
        (layer, _) => layer,
    };
    let fetch_v4 = image_fetch_v4(ctx, img, v4);
    let color = ctx.module.fresh_id();
    if cube && !image_is_storage(ctx, img) {
        let face = layer.ok_or("air.read_texture cube read missing face")?;
        let sampler_arg = (!value_is_int_or_intvec(ctx, args[1])).then_some(args[1]);
        cube_fetch_as_center_sample(
            ctx,
            &mut out,
            img,
            coord,
            face,
            fetch_v4,
            color,
            sampler_arg,
        )?;
    } else {
        let coord32 = build_fetch_coord(ctx, dim, arrayed, coord, layer, &mut out)?;
        read_rest(ctx, &mut out, img, dim, coord32, lod, fetch_v4, color)?;
    }
    finish_read(ctx, out, res, rty, v4, fetch_v4, color)
}

/// The (coord, layer, lod) operands of every non-cube-array `air.read_texture` shape.
fn read_operands(
    ctx: &Ctx,
    args: &[Word],
    arrayed: bool,
    cube: bool,
) -> Result<(Word, Option<Word>, Option<Word>), String> {
    Ok(if value_is_int_or_intvec(ctx, args[1]) {
        if arrayed || cube {
            (args[1], args.get(2).copied(), args.get(3).copied())
        } else {
            (args[1], None, args.get(2).copied())
        }
    } else if arrayed || cube {
        let lod_idx = if args.len() >= 7 { 5 } else { 4 };
        (
            *args.get(2).ok_or("air.read_texture missing coord")?,
            args.get(3).copied(),
            args.get(lod_idx).copied(),
        )
    } else if args.len() == 5 {
        (
            *args.get(2).ok_or("air.read_texture missing coord")?,
            None,
            args.get(3).copied(),
        )
    } else {
        (
            *args.get(2).ok_or("air.read_texture missing coord")?,
            None,
            args.get(4).copied(),
        )
    })
}

/// Emits the OpImageRead / OpImageFetch of a read whose coordinate is already combined.
#[allow(clippy::too_many_arguments)]
fn read_rest(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    img: Word,
    dim: Dim,
    coord32: Word,
    lod: Option<Word>,
    fetch_v4: Word,
    color: Word,
) -> Result<(), String> {
    let lod32 = match lod {
        Some(lod) => Some(coerce_image_coord32(ctx, lod, out, "air.read_texture lod")?),
        None => None,
    };
    let mut ops = vec![Operand::IdRef(img), Operand::IdRef(coord32)];
    let op = if image_is_storage(ctx, img) {
        Op::ImageRead
    } else {
        if image_value_is_multisampled(ctx, img) {
            if let Some(sample) = lod32 {
                ops.push(Operand::ImageOperands(spirv::ImageOperands::SAMPLE));
                ops.push(Operand::IdRef(sample));
            }
        } else if dim != Dim::DimBuffer {
            if let Some(lod) = lod32 {
                ops.push(Operand::ImageOperands(spirv::ImageOperands::LOD));
                ops.push(Operand::IdRef(lod));
            }
        }
        Op::ImageFetch
    };
    out.push(Instruction::new(op, Some(fetch_v4), Some(color), ops));
    Ok(())
}

/// Converts the fetched v4 into the intrinsic result shape (struct, half vector, integer, or v4).
fn finish_read(
    ctx: &mut Ctx,
    mut out: Vec<Instruction>,
    res: Word,
    rty: Word,
    v4: Word,
    fetch_v4: Word,
    color: Word,
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
        let c = match member0 {
            Some(m) if m != v4 && is_half_vector(ctx, m) => {
                let cc = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::FConvert,
                    Some(m),
                    Some(cc),
                    vec![Operand::IdRef(color)],
                ));
                cc
            }
            Some(m) => coerce_same_shape_integer(ctx, &mut out, color, fetch_v4, m)?,
            _ => color,
        };
        let i8u = ctx.ty_int8();
        let undef8 = ctx.module.fresh_id();
        out.push(Instruction::new(Op::Undef, Some(i8u), Some(undef8), vec![]));
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(c), Operand::IdRef(undef8)],
        ));
    } else if rty != v4 && is_half_vector(ctx, rty) {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(color)],
        ));
    } else if resolve::integer_shape(ctx, rty).is_some() {
        let c = coerce_same_shape_integer(ctx, &mut out, color, fetch_v4, rty)?;
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

pub(in crate::passes) fn lower_read_depth(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
) -> Result<Vec<Instruction>, String> {
    let (res, rty) = match (res, rty) {
        (Some(r), Some(t)) => (r, t),
        _ => return Err("air.read_depth has no result".into()),
    };
    if args.len() < 3 {
        return Err("air.read_depth missing texture/coord".into());
    }
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
    let (dim, arrayed) = ctx
        .image_dims
        .get(&img)
        .copied()
        .unwrap_or((Dim::Dim2D, false));
    let comp = ctx
        .image_comp
        .get(&img)
        .copied()
        .unwrap_or(crate::passes::ImageComp::Float);
    if comp != crate::passes::ImageComp::Float {
        return Err("air.read_depth on non-float texture".into());
    }

    let (coord, layer, lod) = if value_is_int_or_intvec(ctx, args[1]) {
        let coord_idx = if vector_shape(ctx, args[1]).is_none()
            && args
                .get(2)
                .is_some_and(|coord| vector_shape(ctx, *coord).is_some())
        {
            2
        } else {
            1
        };
        let coord = *args.get(coord_idx).ok_or("air.read_depth missing coord")?;
        if arrayed {
            (
                coord,
                args.get(coord_idx + 1).copied(),
                args.get(coord_idx + 2).copied(),
            )
        } else {
            (coord, None, args.get(coord_idx + 1).copied())
        }
    } else {
        let coord = *args.get(3).ok_or("air.read_depth missing coord")?;
        if arrayed {
            (
                coord,
                args.get(4).copied(),
                args.get(if args.len() >= 8 { 6 } else { 5 }).copied(),
            )
        } else {
            (
                coord,
                None,
                args.get(if args.len() >= 7 { 5 } else { 4 }).copied(),
            )
        }
    };
    let mut out = vec![];
    let coord32 = build_fetch_coord(ctx, dim, arrayed, coord, layer, &mut out)?;
    let lod32 = match lod {
        Some(lod) => Some(coerce_image_coord32(
            ctx,
            lod,
            &mut out,
            "air.read_depth lod",
        )?),
        None => None,
    };
    let color = ctx.module.fresh_id();
    let mut ops = vec![Operand::IdRef(img), Operand::IdRef(coord32)];
    if image_value_is_multisampled(ctx, img) {
        let sample = lod.ok_or("air.read_depth multisample read missing sample index")?;
        let sample32 = coerce_image_coord32(ctx, sample, &mut out, "air.read_depth sample")?;
        ops.push(Operand::ImageOperands(spirv::ImageOperands::SAMPLE));
        ops.push(Operand::IdRef(sample32));
    } else if let Some(lod) = lod32 {
        ops.push(Operand::ImageOperands(spirv::ImageOperands::LOD));
        ops.push(Operand::IdRef(lod));
    }
    out.push(Instruction::new(Op::ImageFetch, Some(v4), Some(color), ops));
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
