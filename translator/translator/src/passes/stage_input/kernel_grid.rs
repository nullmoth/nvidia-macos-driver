use super::*;

pub(in crate::passes) fn bind_kernel_grid_push_constant_once(
    ctx: &mut Ctx,
    slot: &mut Option<Word>,
    offset: u32,
) -> Word {
    if let Some(var) = *slot {
        return var;
    }
    let uint_ty = ctx.ty_uint();
    let block_ty = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeStruct,
        block_ty,
        (0..12).map(|_| Operand::IdRef(uint_ty)).collect(),
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(block_ty),
            Operand::Decoration(Decoration::Block),
        ],
    ));
    for member in 0..12 {
        ctx.module.annotations.push(Instruction::new(
            Op::MemberDecorate,
            None,
            None,
            vec![
                Operand::IdRef(block_ty),
                Operand::LiteralBit32(member),
                Operand::Decoration(Decoration::Offset),
                Operand::LiteralBit32(offset + member * 4),
            ],
        ));
    }
    let pointer_ty = ctx.ty_ptr(StorageClass::PushConstant, block_ty);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pointer_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::PushConstant)],
    ));
    ctx.module.debug_names.push(Instruction::new(
        Op::Name,
        None,
        None,
        vec![
            Operand::IdRef(var),
            Operand::LiteralString("metal2vulkan_dispatch_region".into()),
        ],
    ));
    ctx.interface.push(var);
    *slot = Some(var);
    var
}

pub(in crate::passes) fn load_kernel_dispatch_component(
    ctx: &mut Ctx,
    instructions: &mut Vec<Instruction>,
    var: Word,
    member: u32,
) -> Word {
    let uint_ty = ctx.ty_uint();
    let pointer_ty = ctx.ty_ptr(StorageClass::PushConstant, uint_ty);
    let member_index = ctx.const_uint(member);
    let pointer = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::AccessChain,
        Some(pointer_ty),
        Some(pointer),
        vec![Operand::IdRef(var), Operand::IdRef(member_index)],
    ));
    let value = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Load,
        Some(uint_ty),
        Some(value),
        vec![Operand::IdRef(pointer)],
    ));
    // An indirect dispatch's grid is written by the GPU, so the driver cannot push it: it pushes 0x80000000 | the
    // threadgroup size per axis for threads_per_grid (members 0-2) and 0x80000000 for threadgroups_per_grid (9-11), and
    // the shader takes them from NumWorkgroups, which is what Metal defines for dispatchThreadgroups(indirectBuffer:).
    // Eevee's light culling sizes its dispatches on the GPU; its threads_per_grid read 0 and every surface lit black.
    // a shape compiled for one fixed grid is never dispatched indirectly, and stays without the fallback
    if matches!(ctx.kernel_dispatch, crate::reflect::KernelDispatch::ThreadsFixed { .. }) {
        return value;
    }
    let lane = match member {
        0..=2 => member,
        9..=11 => member - 9,
        _ => return value,
    };
    let flag_bit = ctx.const_uint(0x8000_0000);
    let low_mask = ctx.const_uint(0x7fff_ffff);
    let zero = ctx.const_uint(0);
    let bool_ty = ctx.ty_bool();
    let v3u = ctx.ty_vec_uint(3);
    let nw_var = num_workgroups_var(ctx);
    let flag = ctx.module.fresh_id();
    instructions.push(Instruction::new(Op::BitwiseAnd, Some(uint_ty), Some(flag), vec![Operand::IdRef(value), Operand::IdRef(flag_bit)]));
    let indirect = ctx.module.fresh_id();
    instructions.push(Instruction::new(Op::INotEqual, Some(bool_ty), Some(indirect), vec![Operand::IdRef(flag), Operand::IdRef(zero)]));
    let nw = ctx.module.fresh_id();
    instructions.push(Instruction::new(Op::Load, Some(v3u), Some(nw), vec![Operand::IdRef(nw_var)]));
    let groups = ctx.module.fresh_id();
    instructions.push(Instruction::new(Op::CompositeExtract, Some(uint_ty), Some(groups), vec![Operand::IdRef(nw), Operand::LiteralBit32(lane)]));
    let from_gpu = if member <= 2 {
        let size = ctx.module.fresh_id();
        instructions.push(Instruction::new(Op::BitwiseAnd, Some(uint_ty), Some(size), vec![Operand::IdRef(value), Operand::IdRef(low_mask)]));
        let threads = ctx.module.fresh_id();
        instructions.push(Instruction::new(Op::IMul, Some(uint_ty), Some(threads), vec![Operand::IdRef(groups), Operand::IdRef(size)]));
        threads
    } else {
        groups
    };
    let chosen = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Select,
        Some(uint_ty),
        Some(chosen),
        vec![Operand::IdRef(indirect), Operand::IdRef(from_gpu), Operand::IdRef(value)],
    ));
    chosen
}

/// The module's NumWorkgroups input, made once: two variables with one BuiltIn are invalid.
fn num_workgroups_var(ctx: &mut Ctx) -> Word {
    let existing = ctx.module.annotations.iter().find_map(|inst| {
        (inst.class.opcode == Op::Decorate
            && inst.operands.get(1) == Some(&Operand::Decoration(spirv::Decoration::BuiltIn))
            && inst.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::NumWorkgroups)))
        .then(|| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .flatten()
    });
    if let Some(var) = existing {
        if !ctx.interface.contains(&var) {
            ctx.interface.push(var);
        }
        return var;
    }
    bind_kernel_v3uint_builtin(ctx, BuiltIn::NumWorkgroups)
}

pub(in crate::passes) fn materialize_kernel_dispatch_field(
    ctx: &mut Ctx,
    instructions: &mut Vec<Instruction>,
    var: Word,
    first_member: u32,
    out_ty: Word,
    lanes: u32,
) -> Result<Word, String> {
    if !(1..=3).contains(&lanes) || first_member > 9 {
        return Err(format!(
            "kernel dispatch field has unsupported member {first_member} and lane count {lanes}"
        ));
    }
    let uint_ty = ctx.ty_uint();
    let components = (0..lanes)
        .map(|lane| load_kernel_dispatch_component(ctx, instructions, var, first_member + lane))
        .collect::<Vec<_>>();
    let value = if lanes == 1 {
        components[0]
    } else {
        let vector_ty = ctx.ty_vec_uint(lanes);
        let vector = ctx.module.fresh_id();
        instructions.push(Instruction::new(
            Op::CompositeConstruct,
            Some(vector_ty),
            Some(vector),
            components.into_iter().map(Operand::IdRef).collect(),
        ));
        vector
    };
    let value_ty = if lanes == 1 {
        uint_ty
    } else {
        ctx.ty_vec_uint(lanes)
    };
    if value_ty == out_ty {
        return Ok(value);
    }
    let converted = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::UConvert,
        Some(out_ty),
        Some(converted),
        vec![Operand::IdRef(value)],
    ));
    Ok(converted)
}
