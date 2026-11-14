use super::*;

// A tile function addresses its imageblock with tile-local coordinates. A driver that emulates tile memory binds the
// whole render target as the plane, so the translated kernel adds the tile's origin: the tile coordinate (the
// threadgroup position in the grid) times the tile size. The driver pushes the tile size after the twelve words of the
// dispatch region (see `tile_mode`); the tile coordinate is read exactly as `[[threadgroup_position_in_grid]]` is.

/// Push-constant block members after the dispatch region, in the order the driver writes them.
pub(in crate::passes) const TILE_WIDTH_MEMBER: u32 = 12;
pub(in crate::passes) const TILE_HEIGHT_MEMBER: u32 = 13;
pub(in crate::passes) const TILES_ACROSS_MEMBER: u32 = 14;
pub(in crate::passes) const TILE_MEMORY_WORDS_MEMBER: u32 = 15;
/// Words the block holds in a tile function: the dispatch region plus the four tile members.
pub(in crate::passes) const TILE_BLOCK_MEMBERS: u32 = 16;

/// The variable `bind_kernel_grid_push_constant_once` made, found by the name it gave it.
fn dispatch_region_var(ctx: &Ctx) -> Option<Word> {
    let name = Operand::LiteralString("metal2vulkan_dispatch_region".into());
    ctx.module
        .debug_names
        .iter()
        .find(|inst| inst.class.opcode == Op::Name && inst.operands.get(1) == Some(&name))
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
}

/// One word of the tile block.
pub(in crate::passes) fn load_tile_member(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    member: u32,
) -> Result<Word, String> {
    let var = dispatch_region_var(ctx).ok_or_else(|| {
        "a tile function needs the dispatch-region push constants, and this kernel's dispatch has none".to_string()
    })?;
    Ok(stage_input::load_kernel_dispatch_component(ctx, out, var, member))
}

/// The tile coordinate: WorkgroupId plus the group base the driver pushes when it splits a dispatch.
pub(in crate::passes) fn tile_coordinate(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
) -> Result<(Word, Word), String> {
    let var = dispatch_region_var(ctx).ok_or_else(|| {
        "a tile function needs the dispatch-region push constants, and this kernel's dispatch has none".to_string()
    })?;
    let uint = ctx.ty_uint();
    let v3uint = ctx.ty_vec_uint(3);
    let workgroup_var = air_calls::existing_builtin_input_var(ctx, BuiltIn::WorkgroupId, v3uint)
        .unwrap_or_else(|| stage_input::bind_kernel_v3uint_builtin(ctx, BuiltIn::WorkgroupId));
    let workgroup = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Load,
        Some(v3uint),
        Some(workgroup),
        vec![Operand::IdRef(workgroup_var)],
    ));
    let mut axes = [0; 2];
    for axis in 0..2u32 {
        let lane = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(lane),
            vec![Operand::IdRef(workgroup), Operand::LiteralBit32(axis)],
        ));
        let base = stage_input::load_kernel_dispatch_component(ctx, out, var, 6 + axis);
        let sum = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IAdd,
            Some(uint),
            Some(sum),
            vec![Operand::IdRef(lane), Operand::IdRef(base)],
        ));
        axes[axis as usize] = sum;
    }
    Ok((axes[0], axes[1]))
}

/// The pixel of the render target that tile-local (0, 0) of this tile is: tile coordinate times tile size.
pub(in crate::passes) fn tile_origin(ctx: &mut Ctx, out: &mut Vec<Instruction>) -> Result<Word, String> {
    let uint = ctx.ty_uint();
    let v2uint = ctx.ty_vec_uint(2);
    let (tile_x, tile_y) = tile_coordinate(ctx, out)?;
    let width = load_tile_member(ctx, out, TILE_WIDTH_MEMBER)?;
    let height = load_tile_member(ctx, out, TILE_HEIGHT_MEMBER)?;
    let origin_x = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(origin_x),
        vec![Operand::IdRef(tile_x), Operand::IdRef(width)],
    ));
    let origin_y = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IMul,
        Some(uint),
        Some(origin_y),
        vec![Operand::IdRef(tile_y), Operand::IdRef(height)],
    ));
    let origin = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v2uint),
        Some(origin),
        vec![Operand::IdRef(origin_x), Operand::IdRef(origin_y)],
    ));
    Ok(origin)
}

/// `coord + origin` for a tile-local two-lane unsigned coordinate.
pub(in crate::passes) fn add_tile_origin(
    ctx: &mut Ctx,
    coord: Word,
    out: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let v2uint = ctx.ty_vec_uint(2);
    let origin = tile_origin(ctx, out)?;
    let sum = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::IAdd,
        Some(v2uint),
        Some(sum),
        vec![Operand::IdRef(coord), Operand::IdRef(origin)],
    ));
    Ok(sum)
}
