use super::*;

pub(in crate::passes) fn lower_atomic_texture(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if name == "air.atomic_fetch_max_explicit_texture_2d.i16.u.v4i32" {
        return lower_atomic_texture_fetch_max(ctx, name, res, rty, args);
    }
    let op = name
        .strip_prefix("air.atomic_")
        .and_then(|rest| rest.split("_explicit_texture_").next())
        .ok_or_else(|| format!("unsupported texture atomic intrinsic: {name}"))?
        .to_string();
    let (dim, arrayed) = intrinsic_texture_shape(name)
        .ok_or_else(|| format!("{name}: no texture shape in the intrinsic name"))?;
    if !matches!(dim, Dim::Dim1D | Dim::Dim2D | Dim::Dim3D | Dim::DimBuffer)
        || (arrayed && dim != Dim::Dim2D)
    {
        return Err(format!(
            "{name}: texture atomics exist on 1d, 2d, 2d_array and 3d only"
        ));
    }
    let layer_n = usize::from(arrayed);
    let (value_at, coord_at) = if op == "store" {
        (Some(1usize), 2usize)
    } else {
        (None, 1usize)
    };
    let after_coord = coord_at + 1 + layer_n;
    let value_at = match op.as_str() {
        "store" => value_at,
        "load" => None,
        "compare_exchange_weak" => Some(after_coord + 1),
        _ => Some(after_coord),
    };
    let min_args = match op.as_str() {
        "load" => after_coord + 2,
        "store" => after_coord + 2,
        "compare_exchange_weak" => after_coord + 5,
        _ => after_coord + 3,
    };
    if args.len() < min_args {
        return Err(format!(
            "{name} expects at least {min_args} operands, got {}",
            args.len()
        ));
    }
    // AIR fetch/exchange calls can carry a spatial offset after the coordinate
    // and optional array layer. For example, Eevee's 2d_array fetch_min has
    // seven operands: image, xy, layer, offset_xy, value4, order, scope.
    // Preserve the older offset-free form; never treat offset_xy as value4,
    // and apply the offset before appending the array layer.
    let offset_at = (matches!(op.as_str(),
        "fetch_add" | "fetch_sub" | "fetch_and" | "fetch_or" | "fetch_xor"
        | "fetch_min" | "fetch_max" | "exchange") && args.len() == min_args + 1)
        .then_some(after_coord);
    let value_at = value_at.map(|index| index + usize::from(offset_at.is_some()));
    let mut image = resolve_image_value(ctx, args[0]);
    if !image_is_storage(ctx, image) {
        image = recovered_image_for_private_operand(ctx, image, name, ImageOperandUse::Storage)
            .ok_or_else(|| format!("{name} on non-storage image id {image}"))?;
    }
    let (_, _, comp) = image_shape_or_recorded(ctx, image);
    let signed = match comp {
        crate::passes::ImageComp::Uint => false,
        crate::passes::ImageComp::Sint => true,
        crate::passes::ImageComp::Float => {
            return Err(format!("{name} requires an integer storage image"))
        }
    };
    ctx.require_runtime_storage_image_use(image, RuntimeStorageImageUse::Atomic)?;
    let image_pointer = if value_is_pointer(ctx, image) {
        image
    } else {
        value_inst(ctx, image)
            .filter(|instruction| instruction.class.opcode == Op::Load)
            .and_then(|instruction| instruction.operands.first())
            .and_then(|operand| match operand {
                Operand::IdRef(pointer) => Some(*pointer),
                _ => None,
            })
            .ok_or_else(|| format!("{name} image has no descriptor pointer"))?
    };
    let mut out = Vec::new();
    let layer = arrayed.then(|| args[coord_at + 1]);
    let spatial_coord = if let Some(offset_at) = offset_at {
        let (coord, coord_ty) =
            coerce_image_coord32_typed(ctx, args[coord_at], &mut out, "texture atomic coordinate")?;
        let (offset, offset_ty) =
            coerce_image_coord32_typed(ctx, args[offset_at], &mut out, "texture atomic offset")?;
        if coord_ty != offset_ty {
            return Err(format!("{name} coordinate and offset shapes differ"));
        }
        let sum = ctx.module.fresh_id();
        out.push(Instruction::new(Op::IAdd, Some(coord_ty), Some(sum),
            vec![Operand::IdRef(coord), Operand::IdRef(offset)]));
        sum
    } else {
        args[coord_at]
    };
    let coord = build_fetch_coord(ctx, dim, arrayed, spatial_coord, layer, &mut out)?;
    let uint = ctx.ty_uint();
    let elem = if signed { ctx.ty_sint() } else { uint };
    let lane0t =
        |ctx: &mut Ctx, out: &mut Vec<Instruction>, v4: Word, vty: Word| -> Result<Word, String> {
            if resolve::integer_shape(ctx, vty) != Some((32, 4)) {
                return Err(format!("{name} value must be a four-lane i32 vector"));
            }
            let lane_ty = vector_element_type(ctx, vty).unwrap_or(uint);
            let x = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::CompositeExtract,
                Some(lane_ty),
                Some(x),
                vec![Operand::IdRef(v4), Operand::LiteralBit32(0)],
            ));
            if lane_ty == elem {
                return Ok(x);
            }
            let c = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Bitcast,
                Some(elem),
                Some(c),
                vec![Operand::IdRef(x)],
            ));
            Ok(c)
        };
    let lane0 = |ctx: &mut Ctx, out: &mut Vec<Instruction>, v4: Word| -> Result<Word, String> {
        let vty = value_result_type(ctx, v4).ok_or_else(|| format!("{name} value untyped"))?;
        lane0t(ctx, out, v4, vty)
    };
    let pointer_ty = ctx.ty_ptr(StorageClass::Image, elem);
    let pointer = ctx.module.fresh_id();
    let sample = ctx.const_uint(0);
    out.push(Instruction::new(
        Op::ImageTexelPointer,
        Some(pointer_ty),
        Some(pointer),
        vec![
            Operand::IdRef(image_pointer),
            Operand::IdRef(coord),
            Operand::IdRef(sample),
        ],
    ));
    let scope = ctx.const_uint(Scope::Device as u32);
    let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
    let widen = |ctx: &mut Ctx, out: &mut Vec<Instruction>, found: Word, rty: Word, res: Word| {
        let lane_ty = vector_element_type(ctx, rty).unwrap_or(uint);
        let v = if lane_ty == elem {
            found
        } else {
            let c = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Bitcast,
                Some(lane_ty),
                Some(c),
                vec![Operand::IdRef(found)],
            ));
            c
        };
        let undefined = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Undef,
            Some(rty),
            Some(undefined),
            vec![],
        ));
        out.push(Instruction::new(
            Op::CompositeInsert,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(v),
                Operand::IdRef(undefined),
                Operand::LiteralBit32(0),
            ],
        ));
    };
    match op.as_str() {
        "store" => {
            let value = lane0(ctx, &mut out, args[value_at.unwrap_or(1)])?;
            out.push(Instruction::new(
                Op::AtomicStore,
                None,
                None,
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdScope(scope),
                    Operand::IdMemorySemantics(semantics),
                    Operand::IdRef(value),
                ],
            ));
        }
        "load" => {
            let res = res.ok_or_else(|| format!("{name} has no result"))?;
            let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
            let found = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::AtomicLoad,
                Some(elem),
                Some(found),
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdScope(scope),
                    Operand::IdMemorySemantics(semantics),
                ],
            ));
            widen(ctx, &mut out, found, rty, res);
        }
        "compare_exchange_weak" => {
            let res = res.ok_or_else(|| format!("{name} has no result"))?;
            let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
            let expected_ptr = args[after_coord];
            let expected_ty = value_result_type(ctx, args[value_at.unwrap_or(after_coord + 1)])
                .ok_or_else(|| format!("{name} desired value untyped"))?;
            let loaded = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Load,
                Some(expected_ty),
                Some(loaded),
                vec![Operand::IdRef(expected_ptr)],
            ));
            let comparator = lane0t(ctx, &mut out, loaded, expected_ty)?;
            let desired = lane0(ctx, &mut out, args[value_at.unwrap_or(after_coord + 1)])?;
            let found = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::AtomicCompareExchange,
                Some(elem),
                Some(found),
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdScope(scope),
                    Operand::IdMemorySemantics(semantics),
                    Operand::IdMemorySemantics(semantics),
                    Operand::IdRef(desired),
                    Operand::IdRef(comparator),
                ],
            ));
            let lane_ty = vector_element_type(ctx, expected_ty).unwrap_or(uint);
            let back = if lane_ty == elem {
                found
            } else {
                let c = ctx.module.fresh_id();
                out.push(Instruction::new(
                    Op::Bitcast,
                    Some(lane_ty),
                    Some(c),
                    vec![Operand::IdRef(found)],
                ));
                c
            };
            let updated = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::CompositeInsert,
                Some(expected_ty),
                Some(updated),
                vec![
                    Operand::IdRef(back),
                    Operand::IdRef(loaded),
                    Operand::LiteralBit32(0),
                ],
            ));
            out.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(expected_ptr), Operand::IdRef(updated)],
            ));
            out.push(Instruction::new(
                Op::IEqual,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(found), Operand::IdRef(comparator)],
            ));
        }
        _ => {
            let res = res.ok_or_else(|| format!("{name} has no result"))?;
            let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
            let opcode = match (op.as_str(), signed) {
                ("fetch_add", _) => Op::AtomicIAdd,
                ("fetch_sub", _) => Op::AtomicISub,
                ("fetch_and", _) => Op::AtomicAnd,
                ("fetch_or", _) => Op::AtomicOr,
                ("fetch_xor", _) => Op::AtomicXor,
                ("fetch_min", true) => Op::AtomicSMin,
                ("fetch_min", false) => Op::AtomicUMin,
                ("fetch_max", true) => Op::AtomicSMax,
                ("fetch_max", false) => Op::AtomicUMax,
                ("exchange", _) => Op::AtomicExchange,
                _ => {
                    return Err(format!(
                        "unsupported texture atomic operation `{op}`: {name}"
                    ))
                }
            };
            let value = lane0(ctx, &mut out, args[value_at.unwrap_or(after_coord)])?;
            let found = ctx.module.fresh_id();
            out.push(Instruction::new(
                opcode,
                Some(elem),
                Some(found),
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdScope(scope),
                    Operand::IdMemorySemantics(semantics),
                    Operand::IdRef(value),
                ],
            ));
            widen(ctx, &mut out, found, rty, res);
        }
    }
    Ok(out)
}

pub(in crate::passes) fn lower_atomic_texture_fetch_max(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if name != "air.atomic_fetch_max_explicit_texture_2d.i16.u.v4i32" {
        return Err(format!("unsupported texture atomic intrinsic: {name}"));
    }
    if args.len() != 6 {
        return Err(format!("{name} expects 6 operands"));
    }
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let mut image = resolve_image_value(ctx, args[0]);
    if !image_is_storage(ctx, image) {
        image = recovered_image_for_private_operand(ctx, image, name, ImageOperandUse::Storage)
            .ok_or_else(|| format!("{name} on non-storage image id {image}"))?;
    }
    let (_, _, comp) = image_shape_or_recorded(ctx, image);
    if comp != crate::passes::ImageComp::Uint {
        return Err(format!("{name} requires an unsigned integer storage image"));
    }
    ctx.require_runtime_storage_image_use(image, RuntimeStorageImageUse::Atomic)?;
    let image_pointer = if value_is_pointer(ctx, image) {
        image
    } else {
        value_inst(ctx, image)
            .filter(|instruction| instruction.class.opcode == Op::Load)
            .and_then(|instruction| instruction.operands.first())
            .and_then(|operand| match operand {
                Operand::IdRef(pointer) => Some(*pointer),
                _ => None,
            })
            .ok_or_else(|| format!("{name} image has no descriptor pointer"))?
    };

    let mut out = Vec::new();
    let (coord, coord_ty) =
        coerce_image_coord32_typed(ctx, args[1], &mut out, "texture atomic coordinate")?;
    let (offset, offset_ty) =
        coerce_image_coord32_typed(ctx, args[2], &mut out, "texture atomic offset")?;
    if offset_ty != coord_ty {
        return Err(format!("{name} coordinate and offset shapes differ"));
    }
    let offset_coord = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(coord_ty),
        Some(offset_coord),
        vec![Operand::IdRef(coord), Operand::IdRef(offset)],
    ));

    let uint = ctx.ty_uint();
    let value_ty =
        value_result_type(ctx, args[3]).ok_or_else(|| format!("{name} value untyped"))?;
    if resolve::integer_shape(ctx, value_ty) != Some((32, 4)) {
        return Err(format!("{name} value must be a four-lane i32 vector"));
    }
    let value = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeExtract,
        Some(uint),
        Some(value),
        vec![Operand::IdRef(args[3]), Operand::LiteralBit32(0)],
    ));
    let pointer_ty = ctx.ty_ptr(StorageClass::Image, uint);
    let pointer = ctx.module.fresh_id();
    let sample = ctx.const_uint(0);
    out.push(Instruction::new(
        Op::ImageTexelPointer,
        Some(pointer_ty),
        Some(pointer),
        vec![
            Operand::IdRef(image_pointer),
            Operand::IdRef(offset_coord),
            Operand::IdRef(sample),
        ],
    ));
    let previous = ctx.module.fresh_id();
    let scope = ctx.const_uint(Scope::Device as u32);
    let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
    out.push(Instruction::new(
        Op::AtomicUMax,
        Some(uint),
        Some(previous),
        vec![
            Operand::IdRef(pointer),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
            Operand::IdRef(value),
        ],
    ));
    let undefined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Undef,
        Some(rty),
        Some(undefined),
        vec![],
    ));
    out.push(Instruction::new(
        Op::CompositeInsert,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(previous),
            Operand::IdRef(undefined),
            Operand::LiteralBit32(0),
        ],
    ));
    Ok(out)
}
