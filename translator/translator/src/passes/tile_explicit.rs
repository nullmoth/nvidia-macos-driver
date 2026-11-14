use super::*;

// A tile function's explicit-layout imageblock holds what the render pass's fragment shaders wrote into it. A driver
// that emulates tile memory keeps each member of the structure in a storage image the size of the render target (the
// same images the fragment shaders' own imageblock accesses use), and the kernel stages them into and out of its
// threadgroup cells around its body (see `aliased_imageblock`). These are the two accesses it stages with.

fn member_format(plane: &crate::meta::AliasedImageblockPlane) -> Result<FragmentImageblockFormat, String> {
    fragment_imageblock_format(plane.intrinsic_suffix).ok_or_else(|| {
        format!(
            "explicit imageblock member {} has type {}, which no storage-image plane represents",
            plane.attachment, plane.intrinsic_suffix
        )
    })
}

fn texel_ty(ctx: &mut Ctx, comp: ImageComp) -> Word {
    match comp {
        ImageComp::Float => ctx.ty_vecf(4),
        ImageComp::Uint => ctx.ty_vec_uint(4),
        ImageComp::Sint => ctx.ty_vec_sint(4),
    }
}

fn scalar_ty(ctx: &mut Ctx, comp: ImageComp) -> Word {
    match comp {
        ImageComp::Float => ctx.ty_float(),
        ImageComp::Uint => ctx.ty_uint(),
        ImageComp::Sint => ctx.ty_sint(),
    }
}

fn convert_op(comp: ImageComp) -> Op {
    match comp {
        ImageComp::Float => Op::FConvert,
        ImageComp::Uint => Op::UConvert,
        ImageComp::Sint => Op::SConvert,
    }
}

/// `value` of type `from_ty` as a `to_ty`, for the value of an imageblock member and its storage-image texel. A member
/// as wide as its texel component (float, int, uint) differs from it at most in signedness: no conversion, a bit copy.
pub(in crate::passes) fn convert_imageblock_value(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    comp: ImageComp,
    value: Word,
    from_ty: Word,
    to_ty: Word,
    result: Word,
) {
    let op = if from_ty == to_ty {
        Op::CopyObject
    } else if scalar_bit_width(ctx, from_ty) == scalar_bit_width(ctx, to_ty) {
        Op::Bitcast
    } else {
        convert_op(comp)
    };
    out.push(Instruction::new(op, Some(to_ty), Some(result), vec![Operand::IdRef(value)]));
}

/// Read member `plane` for the cell at tile-local `coord` (two unsigned lanes); the value, of type `member_ty`, is `result`.
pub(in crate::passes) fn load_member(
    ctx: &mut Ctx,
    plane: &crate::meta::AliasedImageblockPlane,
    coord: Word,
    member_ty: Word,
    result: Word,
) -> Result<Vec<Instruction>, String> {
    let format = member_format(plane)?;
    let (var, image_ty) = ctx.fragment_imageblock_var(plane.attachment, plane.intrinsic_suffix)?;
    let mut out = Vec::new();
    let pixel = tile_origin::add_tile_origin(ctx, coord, &mut out)?;
    let image = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(image_ty),
        Some(image),
        vec![Operand::IdRef(var)],
    ));
    let texel_type = texel_ty(ctx, format.component);
    let texel = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ImageRead,
        Some(texel_type),
        Some(texel),
        vec![Operand::IdRef(image), Operand::IdRef(pixel)],
    ));
    let wide_ty = if format.lanes == 1 {
        scalar_ty(ctx, format.component)
    } else {
        texel_type
    };
    let wide = if format.lanes == 1 {
        let component = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(wide_ty),
            Some(component),
            vec![Operand::IdRef(texel), Operand::LiteralBit32(0)],
        ));
        component
    } else {
        texel
    };
    convert_imageblock_value(ctx, &mut out, format.component, wide, wide_ty, member_ty, result);
    Ok(out)
}

/// Write `value` (of type `member_ty`) to member `plane` for the cell at tile-local `coord`.
pub(in crate::passes) fn store_member(
    ctx: &mut Ctx,
    plane: &crate::meta::AliasedImageblockPlane,
    coord: Word,
    value: Word,
    member_ty: Word,
) -> Result<Vec<Instruction>, String> {
    let format = member_format(plane)?;
    let (var, image_ty) = ctx.fragment_imageblock_var(plane.attachment, plane.intrinsic_suffix)?;
    let mut out = Vec::new();
    let pixel = tile_origin::add_tile_origin(ctx, coord, &mut out)?;
    let texel_type = texel_ty(ctx, format.component);
    let wide_ty = if format.lanes == 1 {
        scalar_ty(ctx, format.component)
    } else {
        texel_type
    };
    let wide = if wide_ty == member_ty {
        value
    } else {
        let converted = ctx.module.fresh_id();
        convert_imageblock_value(ctx, &mut out, format.component, value, member_ty, wide_ty, converted);
        converted
    };
    let texel = if format.lanes == 1 {
        let splat = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(texel_type),
            Some(splat),
            vec![Operand::IdRef(wide); 4],
        ));
        splat
    } else {
        wide
    };
    let image = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(image_ty),
        Some(image),
        vec![Operand::IdRef(var)],
    ));
    out.push(Instruction::new(
        Op::ImageWrite,
        None,
        None,
        vec![Operand::IdRef(image), Operand::IdRef(pixel), Operand::IdRef(texel)],
    ));
    Ok(out)
}
