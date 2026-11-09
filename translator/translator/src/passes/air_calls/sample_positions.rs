use super::*;

pub(in crate::passes) fn lower_sample_position(
    ctx: &mut Ctx,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if !matches!(ctx.stage, Stage::Fragment) {
        return Err("air.get_sample_position requires a fragment render-pass payload".into());
    }
    if args.len() != 2 {
        return Err("air.get_sample_position expects index and constant flags".into());
    }
    if crate::passes::access::const_u32(ctx, args[1]) != Some(0) {
        return Err("air.get_sample_position supports only the authored flags=0 ABI".into());
    }
    let res = res.ok_or("air.get_sample_position has no result")?;
    let rty = rty.ok_or("air.get_sample_position has no result type")?;
    let vec2 = ctx.ty_vecf(2);
    if rty != vec2 {
        return Err("air.get_sample_position result must be float2".into());
    }
    let mut index_ty = None;
    for (arg, label) in [(args[0], "index"), (args[1], "flags")] {
        let ty = ctx
            .module
            .types_global_values
            .iter()
            .chain(ctx.new_globals.iter())
            .chain(ctx.module.functions.iter().flat_map(|f| {
                f.parameters
                    .iter()
                    .chain(f.blocks.iter().flat_map(|b| b.instructions.iter()))
            }))
            .find(|i| i.result_id == Some(arg))
            .and_then(|i| i.result_type);
        if label == "index" {
            index_ty = ty;
        }
        let definition = ty.and_then(|t| {
            ctx.module
                .types_global_values
                .iter()
                .chain(ctx.new_globals.iter())
                .find(|i| i.result_id == Some(t))
        });
        if !definition.is_some_and(|i| {
            i.class.opcode == Op::TypeInt && i.operands.first() == Some(&Operand::LiteralBit32(32))
        }) {
            return Err(format!(
                "air.get_sample_position {label} must be a 32-bit integer"
            ));
        }
    }
    if crate::passes::access::const_u32(ctx, args[0]).is_some_and(|i| i >= 8) {
        return Err("air.get_sample_position index exceeds the eight-position payload".into());
    }
    let var = if let Some(var) = ctx.fragment_sample_positions_var {
        var
    } else {
        let length = ctx.const_uint(8);
        let array = ctx.module.fresh_id();
        ctx.new_globals.push(type_inst(
            Op::TypeArray,
            array,
            vec![Operand::IdRef(vec2), Operand::IdRef(length)],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(array),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(8),
            ],
        ));
        let block = ctx.module.fresh_id();
        ctx.new_globals.push(type_inst(
            Op::TypeStruct,
            block,
            vec![Operand::IdRef(array)],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(block),
                Operand::Decoration(Decoration::Block),
            ],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::MemberDecorate,
            None,
            None,
            vec![
                Operand::IdRef(block),
                Operand::LiteralBit32(0),
                Operand::Decoration(Decoration::Offset),
                Operand::LiteralBit32(96),
            ],
        ));
        let ptr = ctx.ty_ptr(StorageClass::PushConstant, block);
        let var = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(ptr),
            Some(var),
            vec![Operand::StorageClass(StorageClass::PushConstant)],
        ));
        ctx.interface.push(var);
        ctx.fragment_sample_positions_var = Some(var);
        var
    };
    let pointer_ty = ctx.ty_ptr(StorageClass::PushConstant, vec2);
    let index_ty = index_ty.ok_or("air.get_sample_position index has no type")?;
    let bool_ty = ctx.ty_bool();
    let bool_vec2 = ctx.ty_vec_bool(2);
    let bound = ctx.const_int_of(index_ty, 8);
    let index_zero = ctx.const_int_of(index_ty, 0);
    let zero = ctx.const_uint(0);
    let float_zero = ctx.const_float(0.0);
    let vector_zero = ctx.const_composite(vec2, vec![float_zero, float_zero]);
    let within = ctx.module.fresh_id();
    let safe_index = ctx.module.fresh_id();
    let pointer = ctx.module.fresh_id();
    let loaded = ctx.module.fresh_id();
    let vector_condition = ctx.module.fresh_id();
    Ok(vec![
        Instruction::new(
            Op::ULessThan,
            Some(bool_ty),
            Some(within),
            vec![Operand::IdRef(args[0]), Operand::IdRef(bound)],
        ),
        Instruction::new(
            Op::Select,
            Some(index_ty),
            Some(safe_index),
            vec![
                Operand::IdRef(within),
                Operand::IdRef(args[0]),
                Operand::IdRef(index_zero),
            ],
        ),
        Instruction::new(
            Op::AccessChain,
            Some(pointer_ty),
            Some(pointer),
            vec![
                Operand::IdRef(var),
                Operand::IdRef(zero),
                Operand::IdRef(safe_index),
            ],
        ),
        Instruction::new(
            Op::Load,
            Some(rty),
            Some(loaded),
            vec![Operand::IdRef(pointer)],
        ),
        Instruction::new(
            Op::CompositeConstruct,
            Some(bool_vec2),
            Some(vector_condition),
            vec![Operand::IdRef(within), Operand::IdRef(within)],
        ),
        Instruction::new(
            Op::Select,
            Some(rty),
            Some(res),
            vec![
                Operand::IdRef(vector_condition),
                Operand::IdRef(loaded),
                Operand::IdRef(vector_zero),
            ],
        ),
    ])
}

/// `air.interpolate_{offset,sample,centroid}_{perspective,no_perspective}.<type>(interpolant, ...)`: the value of a
/// fragment input at a pixel offset, at a sample index, or at the centroid. The interpolant is the Input variable
/// itself (see stage_input); its perspective setting is the decoration on that variable.
pub(in crate::passes) fn lower_pull_interpolation(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let (Some(res), Some(rty)) = (res, rty) else {
        return Err(format!("{name} has no result"));
    };
    if name.contains("f16") {
        return Err(format!("{name}: half-precision interpolants are not supported"));
    }
    let (op, expected_args) = if name.starts_with("air.interpolate_offset_") {
        (GLSLstd450::InterpolateAtOffset, 2)
    } else if name.starts_with("air.interpolate_sample_") {
        (GLSLstd450::InterpolateAtSample, 2)
    } else if name.starts_with("air.interpolate_centroid_") {
        (GLSLstd450::InterpolateAtCentroid, 1)
    } else {
        return Err(format!("{name}: not a pull-model interpolation"));
    };
    if args.len() != expected_args {
        return Err(format!("{name} expects {expected_args} operand(s), got {}", args.len()));
    }
    let ext = ctx.glsl();
    let mut operands = vec![Operand::IdRef(ext), Operand::LiteralExtInstInteger(op as u32), Operand::IdRef(args[0])];
    if let Some(&extra) = args.get(1) {
        operands.push(Operand::IdRef(extra));
    }
    Ok(vec![Instruction::new(Op::ExtInst, Some(rty), Some(res), operands)])
}
