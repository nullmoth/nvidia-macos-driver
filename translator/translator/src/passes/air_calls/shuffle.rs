use super::*;
pub(in crate::passes) fn subgroup_shuffle_index_u32(
    ctx: &mut Ctx,
    value: Word,
    insts: &mut Vec<Instruction>,
) -> Result<Word, String> {
    let Some(ty) = value_result_type(ctx, value) else {
        return Err("subgroup shuffle index has no type".to_string());
    };
    let Some(def) = type_def_of(ctx, ty) else {
        return Err("subgroup shuffle index type is undefined".to_string());
    };
    if def.class.opcode != Op::TypeInt {
        return Err("subgroup shuffle index is not an integer".to_string());
    }
    if def.operands.first() == Some(&Operand::LiteralBit32(32)) {
        return Ok(value);
    }
    let uint = ctx.ty_uint();
    let converted = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(converted),
        vec![Operand::IdRef(value)],
    ));
    Ok(converted)
}

pub(in crate::passes) fn lower_quad_shuffle(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let requested_lane = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut insts);
    let quad_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(quad_base),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(!3u32)),
        ],
    ));
    let quad_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(quad_local),
        vec![
            Operand::IdRef(requested_lane),
            Operand::IdRef(ctx.const_uint(3)),
        ],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(source_lane),
        vec![Operand::IdRef(quad_base), Operand::IdRef(quad_local)],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_quad_shuffle_rotate_down(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    lower_quad_shuffle_rotate(ctx, result, result_type, args, false)
}

pub(in crate::passes) fn lower_quad_shuffle_rotate_up(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    lower_quad_shuffle_rotate(ctx, result, result_type, args, true)
}

// Rotating up by d reads the lane d below, wrapping within the group: (lane - d) mod size, which the mask after the
// subtraction gives for any d because the group size is a power of two.
fn lower_quad_shuffle_rotate(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
    up: bool,
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut insts);
    let quad_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(quad_base),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(!3u32)),
        ],
    ));
    let local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(local),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(3)),
        ],
    ));
    let local_plus_delta = ctx.module.fresh_id();
    insts.push(Instruction::new(
        if up { Op::ISub } else { Op::IAdd },
        Some(uint),
        Some(local_plus_delta),
        vec![Operand::IdRef(local), Operand::IdRef(delta)],
    ));
    let source_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(source_local),
        vec![
            Operand::IdRef(local_plus_delta),
            Operand::IdRef(ctx.const_uint(3)),
        ],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(source_lane),
        vec![Operand::IdRef(quad_base), Operand::IdRef(source_local)],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_shuffle_rotate_down(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    lower_simd_shuffle_rotate(ctx, result, result_type, args, false)
}

pub(in crate::passes) fn lower_simd_shuffle_rotate_up(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    lower_simd_shuffle_rotate(ctx, result, result_type, args, true)
}

// Rotating up by d reads the lane d below, wrapping within the group: (lane - d) mod size, which the mask after the
// subtraction gives for any d because the group size is a power of two.
fn lower_simd_shuffle_rotate(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
    up: bool,
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut insts);
    let simd_lane = metal_simd_lane_local_u32(ctx, subgroup_lane, &mut insts);
    let simd_base = metal_simd_lane_base_u32(ctx, subgroup_lane, simd_lane, &mut insts);
    let local_plus_delta = ctx.module.fresh_id();
    insts.push(Instruction::new(
        if up { Op::ISub } else { Op::IAdd },
        Some(uint),
        Some(local_plus_delta),
        vec![Operand::IdRef(simd_lane), Operand::IdRef(delta)],
    ));
    let source_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(source_local),
        vec![
            Operand::IdRef(local_plus_delta),
            Operand::IdRef(ctx.const_uint(31)),
        ],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(source_lane),
        vec![Operand::IdRef(simd_base), Operand::IdRef(source_local)],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_shuffle_down(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let simd_lane = metal_simd_lane_local_u32(ctx, lane, &mut insts);
    let simd_base = metal_simd_lane_base_u32(ctx, lane, simd_lane, &mut insts);
    let remaining = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(remaining),
        vec![
            Operand::IdRef(ctx.const_uint(32)),
            Operand::IdRef(simd_lane),
        ],
    ));
    let in_bounds = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ULessThan,
        Some(ctx.ty_bool()),
        Some(in_bounds),
        vec![Operand::IdRef(delta), Operand::IdRef(remaining)],
    ));
    let shifted_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(shifted_lane),
        vec![Operand::IdRef(simd_lane), Operand::IdRef(delta)],
    ));
    let shifted_subgroup_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(shifted_subgroup_lane),
        vec![Operand::IdRef(simd_base), Operand::IdRef(shifted_lane)],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(source_lane),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(shifted_subgroup_lane),
            Operand::IdRef(lane),
        ],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_shuffle_up(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let simd_lane = metal_simd_lane_local_u32(ctx, lane, &mut insts);
    let simd_base = metal_simd_lane_base_u32(ctx, lane, simd_lane, &mut insts);
    let in_bounds = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UGreaterThanEqual,
        Some(ctx.ty_bool()),
        Some(in_bounds),
        vec![Operand::IdRef(simd_lane), Operand::IdRef(delta)],
    ));
    let shifted_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(shifted_lane),
        vec![Operand::IdRef(simd_lane), Operand::IdRef(delta)],
    ));
    let shifted_subgroup_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(shifted_subgroup_lane),
        vec![Operand::IdRef(simd_base), Operand::IdRef(shifted_lane)],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(source_lane),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(shifted_subgroup_lane),
            Operand::IdRef(lane),
        ],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_quad_shuffle_up(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[1], &mut insts)?;
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut insts);
    let quad_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(quad_base),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(!3u32)),
        ],
    ));
    let local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(local),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(3)),
        ],
    ));
    let in_bounds = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UGreaterThanEqual,
        Some(bool_ty),
        Some(in_bounds),
        vec![Operand::IdRef(local), Operand::IdRef(delta)],
    ));
    let lowered = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(lowered),
        vec![Operand::IdRef(local), Operand::IdRef(delta)],
    ));
    let source_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(source_local),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(lowered),
            Operand::IdRef(local),
        ],
    ));
    let source_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(source_lane),
        vec![Operand::IdRef(quad_base), Operand::IdRef(source_local)],
    ));
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(source_lane),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_shuffle_and_fill_down(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[2], &mut insts)?;
    let modulo = subgroup_shuffle_index_u32(ctx, args[3], &mut insts)?;
    let uint = ctx.ty_uint();
    let modulo_is_zero = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IEqual,
        Some(ctx.ty_bool()),
        Some(modulo_is_zero),
        vec![Operand::IdRef(modulo), Operand::IdRef(ctx.const_uint(0))],
    ));
    let safe_modulo = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(safe_modulo),
        vec![
            Operand::IdRef(modulo_is_zero),
            Operand::IdRef(ctx.const_uint(1)),
            Operand::IdRef(modulo),
        ],
    ));
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let local_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(local_lane),
        vec![Operand::IdRef(lane), Operand::IdRef(safe_modulo)],
    ));
    let cluster_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(cluster_base),
        vec![Operand::IdRef(lane), Operand::IdRef(local_lane)],
    ));
    let source_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(source_local),
        vec![Operand::IdRef(local_lane), Operand::IdRef(delta)],
    ));
    let in_bounds = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ULessThan,
        Some(ctx.ty_bool()),
        Some(in_bounds),
        vec![Operand::IdRef(source_local), Operand::IdRef(safe_modulo)],
    ));
    let wrapped_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(wrapped_local),
        vec![Operand::IdRef(source_local), Operand::IdRef(safe_modulo)],
    ));
    let data_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(data_local),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(source_local),
            Operand::IdRef(wrapped_local),
        ],
    ));
    let data_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(data_lane),
        vec![Operand::IdRef(cluster_base), Operand::IdRef(data_local)],
    ));
    let fill_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(fill_lane),
        vec![Operand::IdRef(cluster_base), Operand::IdRef(wrapped_local)],
    ));
    let data = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(data),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(data_lane),
        ],
    ));
    let fill = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(fill),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[1]),
            Operand::IdRef(fill_lane),
        ],
    ));
    insts.push(Instruction::new(
        Op::Select,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(data),
            Operand::IdRef(fill),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn lower_simd_shuffle_and_fill_up(
    ctx: &mut Ctx,
    result: Word,
    result_type: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let mut insts = Vec::new();
    let delta = subgroup_shuffle_index_u32(ctx, args[2], &mut insts)?;
    let modulo = subgroup_shuffle_index_u32(ctx, args[3], &mut insts)?;
    let modulo_is_zero = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IEqual,
        Some(bool_ty),
        Some(modulo_is_zero),
        vec![Operand::IdRef(modulo), Operand::IdRef(ctx.const_uint(0))],
    ));
    let safe_modulo = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(uint),
        Some(safe_modulo),
        vec![
            Operand::IdRef(modulo_is_zero),
            Operand::IdRef(ctx.const_uint(1)),
            Operand::IdRef(modulo),
        ],
    ));
    let lane = subgroup_lane_index_u32(ctx, &mut insts);
    let local_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(local_lane),
        vec![Operand::IdRef(lane), Operand::IdRef(safe_modulo)],
    ));
    let cluster_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(cluster_base),
        vec![Operand::IdRef(lane), Operand::IdRef(local_lane)],
    ));
    let delta_mod = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(delta_mod),
        vec![Operand::IdRef(delta), Operand::IdRef(safe_modulo)],
    ));
    let in_bounds = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UGreaterThanEqual,
        Some(bool_ty),
        Some(in_bounds),
        vec![Operand::IdRef(local_lane), Operand::IdRef(delta)],
    ));
    let lifted_hi = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(lifted_hi),
        vec![Operand::IdRef(local_lane), Operand::IdRef(safe_modulo)],
    ));
    let lifted = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(lifted),
        vec![Operand::IdRef(lifted_hi), Operand::IdRef(delta_mod)],
    ));
    let wrapped_local = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::UMod,
        Some(uint),
        Some(wrapped_local),
        vec![Operand::IdRef(lifted), Operand::IdRef(safe_modulo)],
    ));
    let src_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(src_lane),
        vec![Operand::IdRef(cluster_base), Operand::IdRef(wrapped_local)],
    ));
    let data = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(data),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[0]),
            Operand::IdRef(src_lane),
        ],
    ));
    let fill = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(result_type),
        Some(fill),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(args[1]),
            Operand::IdRef(src_lane),
        ],
    ));
    insts.push(Instruction::new(
        Op::Select,
        Some(result_type),
        Some(result),
        vec![
            Operand::IdRef(in_bounds),
            Operand::IdRef(data),
            Operand::IdRef(fill),
        ],
    ));
    Ok(insts)
}

pub(in crate::passes) fn subgroup_lane_index_u32(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let var = subgroup_local_invocation_id_input_var(ctx, uint);
    let lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Load,
        Some(uint),
        Some(lane),
        vec![Operand::IdRef(var)],
    ));
    lane
}

pub(in crate::passes) fn metal_simd_lane_local_u32(
    ctx: &mut Ctx,
    subgroup_lane: Word,
    insts: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let simd_lane = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(simd_lane),
        vec![
            Operand::IdRef(subgroup_lane),
            Operand::IdRef(ctx.const_uint(31)),
        ],
    ));
    simd_lane
}

pub(in crate::passes) fn metal_simd_lane_base_u32(
    ctx: &mut Ctx,
    subgroup_lane: Word,
    simd_lane: Word,
    insts: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let simd_base = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::ISub,
        Some(uint),
        Some(simd_base),
        vec![Operand::IdRef(subgroup_lane), Operand::IdRef(simd_lane)],
    ));
    simd_base
}

pub(in crate::passes) fn metal_simd_absolute_lane_u32(
    ctx: &mut Ctx,
    index: Word,
    insts: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let lane = subgroup_lane_index_u32(ctx, insts);
    let simd_lane = metal_simd_lane_local_u32(ctx, lane, insts);
    let simd_base = metal_simd_lane_base_u32(ctx, lane, simd_lane, insts);
    let masked = metal_simd_lane_local_u32(ctx, index, insts);
    let absolute = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::IAdd,
        Some(uint),
        Some(absolute),
        vec![Operand::IdRef(simd_base), Operand::IdRef(masked)],
    ));
    absolute
}

pub(in crate::passes) fn subgroup_local_invocation_id_input_var(ctx: &mut Ctx, uint: Word) -> Word {
    let key = SynthCacheKey::SubgroupLocalInvocationIdInputVar;
    if let Some(&var) = ctx.synth_cache.get(&key) {
        return var;
    }
    if let Some(var) = existing_builtin_input_var(ctx, BuiltIn::SubgroupLocalInvocationId, uint) {
        decorate_fragment_integer_input_flat(ctx, var);
        ctx.synth_cache.insert(key, var);
        return var;
    }
    let ptr_ty = ctx.ty_ptr(StorageClass::Input, uint);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(ptr_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::Input)],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(var),
            Operand::Decoration(Decoration::BuiltIn),
            Operand::BuiltIn(BuiltIn::SubgroupLocalInvocationId),
        ],
    ));
    decorate_fragment_integer_input_flat(ctx, var);
    ctx.interface.push(var);
    ctx.synth_cache.insert(key, var);
    var
}

fn decorate_fragment_integer_input_flat(ctx: &mut Ctx, var: Word) {
    if ctx.stage != Stage::Fragment
        || ctx.module.annotations.iter().any(|instruction| {
            instruction.class.opcode == Op::Decorate
                && instruction.operands.first() == Some(&Operand::IdRef(var))
                && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::Flat))
        })
    {
        return;
    }
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![Operand::IdRef(var), Operand::Decoration(Decoration::Flat)],
    ));
}

pub(in crate::passes) fn existing_builtin_input_var(
    ctx: &Ctx,
    builtin: BuiltIn,
    pointee: Word,
) -> Option<Word> {
    ctx.module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .find_map(|inst| {
            if inst.class.opcode != Op::Variable {
                return None;
            }
            if inst.operands.first() != Some(&Operand::StorageClass(StorageClass::Input)) {
                return None;
            }
            let var = inst.result_id?;
            let ptr_ty = inst.result_type?;
            let ptr_def = type_def_of(ctx, ptr_ty)?;
            if ptr_def.class.opcode != Op::TypePointer
                || ptr_def.operands.first() != Some(&Operand::StorageClass(StorageClass::Input))
                || ptr_def.operands.get(1) != Some(&Operand::IdRef(pointee))
            {
                return None;
            }
            has_builtin_decoration(ctx, var, builtin).then_some(var)
        })
}

pub(in crate::passes) fn has_builtin_decoration(ctx: &Ctx, var: Word, builtin: BuiltIn) -> bool {
    ctx.module.annotations.iter().any(|inst| {
        inst.class.opcode == Op::Decorate
            && inst.operands.first() == Some(&Operand::IdRef(var))
            && inst.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && inst.operands.get(2) == Some(&Operand::BuiltIn(builtin))
    })
}
