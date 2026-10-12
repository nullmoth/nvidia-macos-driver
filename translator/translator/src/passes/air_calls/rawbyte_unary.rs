use super::*;

pub(in crate::passes) fn atomic_i32_pointer(
    ctx: &mut Ctx,
    ptr: Word,
    out: &mut Vec<Instruction>,
) -> Word {
    let Some(ptr_ty) = value_result_type(ctx, ptr) else {
        return ptr;
    };
    let Some(pointee) = pointer_pointee_type(ctx, ptr_ty) else {
        return ptr;
    };
    if is_uint_scalar_width(ctx, pointee, 32) {
        return ptr;
    }
    if !is_uint_scalar_width(ctx, pointee, 8) {
        return ptr;
    }
    let Some((root, byte_index)) = raw_byte_access_root_and_index(ctx, ptr) else {
        return ptr;
    };
    let Some(binding) = descriptor_binding(ctx, root) else {
        return ptr;
    };
    let alias = raw_uint_alias_buffer(ctx, binding);
    let Some(word_index) = raw_byte_index_to_word_index(ctx, byte_index, out) else {
        return ptr;
    };
    let uint = ctx.ty_uint();
    let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, uint);
    let fixed = ctx.module.fresh_id();
    let zero = ctx.const_uint(0);
    out.push(Instruction::new(
        Op::AccessChain,
        Some(ptr_ty),
        Some(fixed),
        vec![
            Operand::IdRef(alias),
            Operand::IdRef(zero),
            Operand::IdRef(word_index),
        ],
    ));
    fixed
}

pub(in crate::passes) fn raw_byte_access_root_and_index(
    ctx: &Ctx,
    ptr: Word,
) -> Option<(Word, Word)> {
    let inst = value_def_instruction(ctx, ptr)?;
    if !matches!(
        inst.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        return None;
    }
    let Some(Operand::IdRef(root)) = inst.operands.first() else {
        return None;
    };
    let indices = inst.operands[1..]
        .iter()
        .map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if let Some(byte_index) = raw_byte_buffer_index(ctx, *root, &indices) {
        return Some((*root, byte_index));
    }
    let (root, mut base_indices) = raw_byte_access_path(ctx, *root)?;
    base_indices.extend(indices);
    raw_byte_buffer_index(ctx, root, &base_indices).map(|byte_index| (root, byte_index))
}

pub(in crate::passes) fn raw_byte_access_path(ctx: &Ctx, ptr: Word) -> Option<(Word, Vec<Word>)> {
    let inst = value_def_instruction(ctx, ptr)?;
    if !matches!(
        inst.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        return None;
    }
    let Some(Operand::IdRef(base)) = inst.operands.first() else {
        return None;
    };
    let indices = inst.operands[1..]
        .iter()
        .map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if raw_byte_buffer_index(ctx, *base, &indices).is_some() {
        return Some((*base, indices));
    }
    let (root, mut base_indices) = raw_byte_access_path(ctx, *base)?;
    base_indices.extend(indices);
    Some((root, base_indices))
}

pub(in crate::passes) fn raw_byte_buffer_index(
    ctx: &Ctx,
    root: Word,
    indices: &[Word],
) -> Option<Word> {
    let [member, byte_index] = indices else {
        return None;
    };
    if constant_u32(ctx, *member) != Some(0) {
        return None;
    }
    let root_ty = value_result_type(ctx, root)?;
    let block_ty = pointer_pointee_type(ctx, root_ty)?;
    let block_def = type_def_of(ctx, block_ty)?;
    if block_def.class.opcode != Op::TypeStruct {
        return None;
    }
    let runtime = match block_def.operands.first() {
        Some(Operand::IdRef(runtime)) => *runtime,
        _ => return None,
    };
    let runtime_def = type_def_of(ctx, runtime)?;
    if runtime_def.class.opcode != Op::TypeRuntimeArray {
        return None;
    }
    let elem = match runtime_def.operands.first() {
        Some(Operand::IdRef(elem)) => *elem,
        _ => return None,
    };
    is_uint_scalar_width(ctx, elem, 8).then_some(*byte_index)
}

pub(in crate::passes) fn raw_byte_index_to_word_index(
    ctx: &mut Ctx,
    byte_index: Word,
    out: &mut Vec<Instruction>,
) -> Option<Word> {
    if let Some(byte_index) = constant_u32(ctx, byte_index) {
        return (byte_index % 4 == 0).then(|| ctx.const_uint(byte_index / 4));
    }
    let byte_ty = value_result_type(ctx, byte_index)?;
    let byte_index = if is_uint_scalar_width(ctx, byte_ty, 32) {
        byte_index
    } else if int_scalar_width(ctx, byte_ty) == Some(64) {
        let converted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::UConvert,
            Some(ctx.ty_uint()),
            Some(converted),
            vec![Operand::IdRef(byte_index)],
        ));
        converted
    } else {
        return None;
    };
    let word = ctx.module.fresh_id();
    let divisor = ctx.const_uint(4);
    out.push(Instruction::new(
        Op::UDiv,
        Some(ctx.ty_uint()),
        Some(word),
        vec![Operand::IdRef(byte_index), Operand::IdRef(divisor)],
    ));
    Some(word)
}

pub(in crate::passes) fn raw_uint_alias_buffer(ctx: &mut Ctx, binding: u32) -> Word {
    if let Some(var) = find_raw_uint_alias_buffer(ctx, binding) {
        return var;
    }
    let uint = ctx.ty_uint();
    let runtime = ctx.ty_runtime_array(uint);
    let block = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeStruct,
        block,
        vec![Operand::IdRef(runtime)],
    ));
    decorate_raw_uint_block(ctx, block, runtime);

    let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, block);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(ptr_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::StorageBuffer)],
    ));
    decorate_descriptor_binding(ctx, var, binding);
    ctx.interface.push(var);
    var
}

pub(in crate::passes) fn find_raw_uint_alias_buffer(ctx: &Ctx, binding: u32) -> Option<Word> {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .filter(|inst| inst.class.opcode == Op::Variable)
        .filter_map(|inst| inst.result_id)
        .find(|var| {
            descriptor_binding(ctx, *var) == Some(binding) && is_raw_uint_buffer_var(ctx, *var)
        })
}

pub(in crate::passes) fn is_raw_uint_buffer_var(ctx: &Ctx, var: Word) -> bool {
    let Some(var_ty) = value_result_type(ctx, var) else {
        return false;
    };
    let Some(block_ty) = pointer_pointee_type(ctx, var_ty) else {
        return false;
    };
    let Some(block_def) = type_def_of(ctx, block_ty) else {
        return false;
    };
    if block_def.class.opcode != Op::TypeStruct {
        return false;
    }
    let Some(Operand::IdRef(runtime)) = block_def.operands.first() else {
        return false;
    };
    let Some(runtime_def) = type_def_of(ctx, *runtime) else {
        return false;
    };
    if runtime_def.class.opcode != Op::TypeRuntimeArray {
        return false;
    }
    let Some(Operand::IdRef(elem)) = runtime_def.operands.first() else {
        return false;
    };
    is_uint_scalar_width(ctx, *elem, 32)
}

pub(in crate::passes) fn decorate_raw_uint_block(ctx: &mut Ctx, block: Word, runtime: Word) {
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
            Operand::LiteralBit32(0),
        ],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(runtime),
            Operand::Decoration(Decoration::ArrayStride),
            Operand::LiteralBit32(4),
        ],
    ));
}

pub(in crate::passes) fn decorate_descriptor_binding(ctx: &mut Ctx, var: Word, binding: u32) {
    let set = ctx.descriptor_layout.set;
    decorate_binding(&mut ctx.module, var, set, binding);
}

pub(in crate::passes) fn descriptor_binding(ctx: &Ctx, var: Word) -> Option<u32> {
    ctx.module.annotations.iter().find_map(|inst| {
        if inst.class.opcode == Op::Decorate
            && inst.operands.first() == Some(&Operand::IdRef(var))
            && inst.operands.get(1) == Some(&Operand::Decoration(Decoration::Binding))
        {
            match inst.operands.get(2) {
                Some(Operand::LiteralBit32(binding)) => Some(*binding),
                _ => None,
            }
        } else {
            None
        }
    })
}

pub(in crate::passes) fn vector_type_shape(ctx: &Ctx, ty: Word) -> Option<(Word, u32)> {
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeVector {
        return None;
    }
    let elem = match def.operands.first() {
        Some(Operand::IdRef(elem)) => *elem,
        _ => return None,
    };
    let lanes = match def.operands.get(1) {
        Some(Operand::LiteralBit32(lanes)) => *lanes,
        _ => return None,
    };
    Some((elem, lanes))
}

pub(in crate::passes) fn is_image_size_query(name: &str) -> bool {
    name.starts_with("air.get_width_texture")
        || name.starts_with("air.get_height_texture")
        || name.starts_with("air.get_depth_texture")
        || name.starts_with("air.get_array_size_texture")
        || name.starts_with("air.get_width_depth")
        || name.starts_with("air.get_height_depth")
        || name.starts_with("air.get_depth_depth")
}

pub(in crate::passes) fn half_deriv(
    ctx: &mut Ctx,
    op: Op,
    res: Word,
    rty: Word,
    arg: Word,
) -> Result<Vec<Instruction>, String> {
    if !is_f32_scalar_or_vector(ctx, rty) && !is_half_scalar_or_vector(ctx, rty) {
        return Err(format!(
            "{op:?} AIR result is not a half/float scalar or vector"
        ));
    }
    if value_result_type(ctx, arg) != Some(rty) {
        return Err(format!("{op:?} AIR operand does not match its result type"));
    }
    let float_ty = float_equivalent(ctx, rty);
    if float_ty == rty {
        return Ok(vec![Instruction::new(
            op,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(arg)],
        )]);
    }
    let argf = ctx.module.fresh_id();
    let derivf = ctx.module.fresh_id();
    Ok(vec![
        Instruction::new(
            Op::FConvert,
            Some(float_ty),
            Some(argf),
            vec![Operand::IdRef(arg)],
        ),
        Instruction::new(op, Some(float_ty), Some(derivf), vec![Operand::IdRef(argf)]),
        Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(derivf)],
        ),
    ])
}

pub(in crate::passes) fn lower_post_scaled_glsl_unary(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    arg: Word,
    scale: f32,
    op: GLSLstd450,
) -> Result<Vec<Instruction>, String> {
    let float_ty = float_equivalent(ctx, rty);
    if !is_f32_scalar_or_vector(ctx, float_ty) {
        return Err(
            "scaled transcendental result is not a half/float scalar or vector".to_string(),
        );
    }
    let ext = ctx.glsl();
    let n = vector_len(ctx, float_ty);
    let scale_c = splat_or_scalar(ctx, float_ty, scale, n);
    let mut out = Vec::new();
    let argf = if float_ty == rty {
        arg
    } else {
        let f = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(float_ty),
            Some(f),
            vec![Operand::IdRef(arg)],
        ));
        f
    };
    let scaled = if float_ty == rty {
        res
    } else {
        ctx.module.fresh_id()
    };
    let transcendental = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(transcendental),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(op as u32),
            Operand::IdRef(argf),
        ],
    ));
    out.push(Instruction::new(
        Op::FMul,
        Some(float_ty),
        Some(scaled),
        vec![Operand::IdRef(transcendental), Operand::IdRef(scale_c)],
    ));
    if float_ty != rty {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(scaled)],
        ));
    }
    Ok(out)
}

pub(in crate::passes) fn half_glsl_op(
    ctx: &mut Ctx,
    op: GLSLstd450,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Vec<Instruction> {
    let float_ty = float_equivalent(ctx, rty);
    let ext = ctx.glsl();
    let extinst = |result_ty: Word, result: Word, operands: &[Word]| {
        let mut ops = vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(op as u32),
        ];
        ops.extend(operands.iter().map(|arg| Operand::IdRef(*arg)));
        Instruction::new(Op::ExtInst, Some(result_ty), Some(result), ops)
    };
    if float_ty == rty {
        return vec![extinst(rty, res, args)];
    }
    let mut out = Vec::with_capacity(args.len() + 2);
    let widened = args
        .iter()
        .map(|arg| {
            let argf = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FConvert,
                Some(float_ty),
                Some(argf),
                vec![Operand::IdRef(*arg)],
            ));
            argf
        })
        .collect::<Vec<_>>();
    let outf = ctx.module.fresh_id();
    out.push(extinst(float_ty, outf, &widened));
    out.push(Instruction::new(
        Op::FConvert,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(outf)],
    ));
    out
}

pub(in crate::passes) fn lower_metal_pow(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    base: Word,
    exponent: Word,
) -> Vec<Instruction> {
    let float_ty = float_equivalent(ctx, rty);
    let n = vector_len(ctx, float_ty);
    let bool_ty = if n > 1 {
        ctx.ty_vec_bool(n)
    } else {
        ctx.ty_bool()
    };
    let ext = ctx.glsl();
    let zero = splat_or_scalar(ctx, float_ty, 0.0, n);
    let one = splat_or_scalar(ctx, float_ty, 1.0, n);
    let two = splat_or_scalar(ctx, float_ty, 2.0, n);
    let one_half = splat_or_scalar(ctx, float_ty, 0.5, n);
    let not_a_number = splat_or_scalar(ctx, float_ty, f32::NAN, n);
    let infinity = splat_or_scalar(ctx, float_ty, f32::INFINITY, n);
    let int_scalar_ty = ctx.ty_sint();
    let int_ty = if n > 1 {
        ctx.ty_vec_sint(n)
    } else {
        int_scalar_ty
    };
    let zero_int_scalar = ctx.const_int_of(int_scalar_ty, 0);
    let zero_int = if n > 1 {
        splat(ctx, int_ty, zero_int_scalar, n)
    } else {
        zero_int_scalar
    };
    let mut out = Vec::new();
    let widen = |ctx: &mut Ctx, out: &mut Vec<Instruction>, value: Word| {
        if float_ty == rty {
            return value;
        }
        let wide = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(float_ty),
            Some(wide),
            vec![Operand::IdRef(value)],
        ));
        wide
    };
    let basef = widen(ctx, &mut out, base);
    let exponentf = widen(ctx, &mut out, exponent);
    let magnitude_base = ctx.module.fresh_id();
    let magnitude = ctx.module.fresh_id();
    let base_is_unit = ctx.module.fresh_id();
    let exponent_magnitude = ctx.module.fresh_id();
    let exponent_is_finite = ctx.module.fresh_id();
    let exponent_is_not_finite = ctx.module.fresh_id();
    let answers_unit = ctx.module.fresh_id();
    let unit_or_signed = ctx.module.fresh_id();
    let integral_exponent = ctx.module.fresh_id();
    let is_integral = ctx.module.fresh_id();
    let halved = ctx.module.fresh_id();
    let halved_integral = ctx.module.fresh_id();
    let doubled = ctx.module.fresh_id();
    let is_odd = ctx.module.fresh_id();
    let odd_integer = ctx.module.fresh_id();
    let negated = ctx.module.fresh_id();
    let base_is_zero = ctx.module.fresh_id();
    let base_bits = ctx.module.fresh_id();
    let base_sign_bit = ctx.module.fresh_id();
    let base_is_negative_zero = ctx.module.fresh_id();
    let base_is_negative = ctx.module.fresh_id();
    let base_sign_is_negative = ctx.module.fresh_id();
    let apply_sign = ctx.module.fresh_id();
    let signed = ctx.module.fresh_id();
    let base_is_finite = ctx.module.fresh_id();
    let is_not_integral = ctx.module.fresh_id();
    let base_is_finite_negative = ctx.module.fresh_id();
    let out_of_domain = ctx.module.fresh_id();
    let powered = ctx.module.fresh_id();
    let exponent_is_zero = ctx.module.fresh_id();
    let guarded = if float_ty == rty {
        res
    } else {
        ctx.module.fresh_id()
    };
    out.extend([
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(magnitude_base),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::FAbs as u32),
                Operand::IdRef(basef),
            ],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(magnitude),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Pow as u32),
                Operand::IdRef(magnitude_base),
                Operand::IdRef(exponentf),
            ],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(integral_exponent),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Trunc as u32),
                Operand::IdRef(exponentf),
            ],
        ),
        Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(is_integral),
            vec![Operand::IdRef(exponentf), Operand::IdRef(integral_exponent)],
        ),
        Instruction::new(
            Op::FMul,
            Some(float_ty),
            Some(halved),
            vec![Operand::IdRef(exponentf), Operand::IdRef(one_half)],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(halved_integral),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Trunc as u32),
                Operand::IdRef(halved),
            ],
        ),
        Instruction::new(
            Op::FMul,
            Some(float_ty),
            Some(doubled),
            vec![Operand::IdRef(halved_integral), Operand::IdRef(two)],
        ),
        Instruction::new(
            Op::FOrdNotEqual,
            Some(bool_ty),
            Some(is_odd),
            vec![Operand::IdRef(doubled), Operand::IdRef(exponentf)],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(odd_integer),
            vec![Operand::IdRef(is_integral), Operand::IdRef(is_odd)],
        ),
        Instruction::new(
            Op::FNegate,
            Some(float_ty),
            Some(negated),
            vec![Operand::IdRef(magnitude)],
        ),
        Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(base_is_zero),
            vec![Operand::IdRef(magnitude_base), Operand::IdRef(zero)],
        ),
        Instruction::new(
            Op::Bitcast,
            Some(int_ty),
            Some(base_bits),
            vec![Operand::IdRef(basef)],
        ),
        Instruction::new(
            Op::SLessThan,
            Some(bool_ty),
            Some(base_sign_bit),
            vec![Operand::IdRef(base_bits), Operand::IdRef(zero_int)],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(base_is_negative_zero),
            vec![Operand::IdRef(base_is_zero), Operand::IdRef(base_sign_bit)],
        ),
        Instruction::new(
            Op::FOrdLessThan,
            Some(bool_ty),
            Some(base_is_negative),
            vec![Operand::IdRef(basef), Operand::IdRef(zero)],
        ),
        Instruction::new(
            Op::LogicalOr,
            Some(bool_ty),
            Some(base_sign_is_negative),
            vec![
                Operand::IdRef(base_is_negative),
                Operand::IdRef(base_is_negative_zero),
            ],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(apply_sign),
            vec![
                Operand::IdRef(base_sign_is_negative),
                Operand::IdRef(odd_integer),
            ],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(signed),
            vec![
                Operand::IdRef(apply_sign),
                Operand::IdRef(negated),
                Operand::IdRef(magnitude),
            ],
        ),
        Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(base_is_unit),
            vec![Operand::IdRef(magnitude_base), Operand::IdRef(one)],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(float_ty),
            Some(exponent_magnitude),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::FAbs as u32),
                Operand::IdRef(exponentf),
            ],
        ),
        Instruction::new(
            Op::FOrdLessThan,
            Some(bool_ty),
            Some(exponent_is_finite),
            vec![Operand::IdRef(exponent_magnitude), Operand::IdRef(infinity)],
        ),
        Instruction::new(
            Op::LogicalNot,
            Some(bool_ty),
            Some(exponent_is_not_finite),
            vec![Operand::IdRef(exponent_is_finite)],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(answers_unit),
            vec![
                Operand::IdRef(base_is_unit),
                Operand::IdRef(exponent_is_not_finite),
            ],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(unit_or_signed),
            vec![
                Operand::IdRef(answers_unit),
                Operand::IdRef(one),
                Operand::IdRef(signed),
            ],
        ),
        Instruction::new(
            Op::FOrdNotEqual,
            Some(bool_ty),
            Some(base_is_finite),
            vec![Operand::IdRef(magnitude_base), Operand::IdRef(infinity)],
        ),
        Instruction::new(
            Op::LogicalNot,
            Some(bool_ty),
            Some(is_not_integral),
            vec![Operand::IdRef(is_integral)],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(base_is_finite_negative),
            vec![
                Operand::IdRef(base_is_negative),
                Operand::IdRef(base_is_finite),
            ],
        ),
        Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(out_of_domain),
            vec![
                Operand::IdRef(base_is_finite_negative),
                Operand::IdRef(is_not_integral),
            ],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(powered),
            vec![
                Operand::IdRef(out_of_domain),
                Operand::IdRef(not_a_number),
                Operand::IdRef(unit_or_signed),
            ],
        ),
        Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(exponent_is_zero),
            vec![Operand::IdRef(exponentf), Operand::IdRef(zero)],
        ),
        Instruction::new(
            Op::Select,
            Some(float_ty),
            Some(guarded),
            vec![
                Operand::IdRef(exponent_is_zero),
                Operand::IdRef(one),
                Operand::IdRef(powered),
            ],
        ),
    ]);
    if float_ty != rty {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(guarded)],
        ));
    }
    out
}

pub(in crate::passes) fn lower_fast_ldexp(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if !is_f32_scalar_or_vector(ctx, rty) {
        return Err(format!("{name} currently supports f32 scalar or vector results"));
    }
    let lanes = vector_type_shape(ctx, rty).map(|(_, lanes)| lanes);
    let mantissa_ty =
        value_result_type(ctx, args[0]).ok_or_else(|| format!("{name} mantissa has no type"))?;
    if mantissa_ty != rty {
        return Err(format!("{name} mantissa/result type mismatch"));
    }
    let exponent_ty =
        value_result_type(ctx, args[1]).ok_or_else(|| format!("{name} exponent has no type"))?;
    let mut out = Vec::new();
    // GLSL Ldexp wants the exponent shaped like the mantissa: a scalar i32 for a scalar, a
    // 32-bit integer vector of the same lane count for a vector (a scalar exponent is splatted).
    let exponent = match (lanes, int_vector_width(ctx, exponent_ty)) {
        (None, None) if is_int_scalar_width(ctx, exponent_ty, 32) => args[1],
        (Some(n), Some((lanes, 32))) if lanes == n => args[1],
        (Some(n), None) if is_int_scalar_width(ctx, exponent_ty, 32) => {
            let vector_ty = ctx.get_or_create(
                Op::TypeVector,
                None,
                vec![Operand::IdRef(exponent_ty), Operand::LiteralBit32(n)],
            );
            let splat = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::CompositeConstruct,
                Some(vector_ty),
                Some(splat),
                (0..n).map(|_| Operand::IdRef(args[1])).collect(),
            ));
            splat
        }
        _ => return Err(format!("{name} exponent is not a 32-bit integer shaped like the mantissa")),
    };
    let ext = ctx.glsl();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Ldexp as u32),
            Operand::IdRef(args[0]),
            Operand::IdRef(exponent),
        ],
    ));
    Ok(out)
}

pub(in crate::passes) fn float_equivalent(ctx: &mut Ctx, ty: Word) -> Word {
    if let Some(def) = type_def_of(ctx, ty) {
        match def.class.opcode {
            Op::TypeFloat => {
                if def.operands.first() == Some(&Operand::LiteralBit32(16)) {
                    return ctx.ty_float();
                }
                return ty;
            }
            Op::TypeVector => {
                if let (Some(Operand::IdRef(elem)), Some(Operand::LiteralBit32(n))) =
                    (def.operands.first(), def.operands.get(1))
                {
                    let n = *n;
                    if is_half_scalar(ctx, *elem) {
                        return ctx.ty_vecf(n);
                    }
                }
                return ty;
            }
            _ => {}
        }
    }
    ty
}

pub(in crate::passes) fn lower_fast_tanh(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    x: Word,
) -> Result<Vec<Instruction>, String> {
    let elem = element_type(ctx, rty);
    let (scale_scalar, one_scalar) = if is_half_scalar(ctx, elem) {
        (ctx.const_half(2.885_39), ctx.const_half(1.0))
    } else {
        (ctx.const_float(2.885_39), ctx.const_float(1.0))
    };
    let (scale, one) = clamp_edges(ctx, rty, scale_scalar, one_scalar);
    let ext = ctx.glsl();
    let scaled = ctx.module.fresh_id();
    let t = ctx.module.fresh_id();
    let num = ctx.module.fresh_id();
    let den = ctx.module.fresh_id();
    Ok(vec![
        Instruction::new(
            Op::FMul,
            Some(rty),
            Some(scaled),
            vec![Operand::IdRef(x), Operand::IdRef(scale)],
        ),
        Instruction::new(
            Op::ExtInst,
            Some(rty),
            Some(t),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Exp2 as u32),
                Operand::IdRef(scaled),
            ],
        ),
        Instruction::new(
            Op::FSub,
            Some(rty),
            Some(num),
            vec![Operand::IdRef(t), Operand::IdRef(one)],
        ),
        Instruction::new(
            Op::FAdd,
            Some(rty),
            Some(den),
            vec![Operand::IdRef(t), Operand::IdRef(one)],
        ),
        Instruction::new(
            Op::FDiv,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(num), Operand::IdRef(den)],
        ),
    ])
}

pub(in crate::passes) fn scalar_zero_one(ctx: &mut Ctx, rty: Word) -> (Word, Word) {
    let elem = element_type(ctx, rty);
    if is_half_scalar(ctx, elem) {
        (ctx.const_half(0.0), ctx.const_half(1.0))
    } else {
        (ctx.const_float(0.0), ctx.const_float(1.0))
    }
}

#[cfg(test)]
mod tests {
    fn powr(a: f32, y: f32) -> f32 {
        if y == 0.0 && (a == 0.0 || a.is_infinite() || a.is_nan()) {
            return f32::NAN;
        }
        if a == 1.0 && !y.is_finite() {
            return f32::NAN;
        }
        a.powf(y)
    }

    fn emitted_pow(x: f32, y: f32) -> f32 {
        let magnitude_base = x.abs();
        let magnitude = powr(magnitude_base, y);
        let is_integral = y == y.trunc();
        let is_odd = (y * 0.5).trunc() * 2.0 != y;
        let base_is_negative = x < 0.0;
        let sign_is_negative =
            base_is_negative || (magnitude_base == 0.0 && (x.to_bits() as i32) < 0);
        let signed = if sign_is_negative && is_integral && is_odd {
            -magnitude
        } else {
            magnitude
        };
        let unit_or_signed = if magnitude_base == 1.0 && !y.is_finite() {
            1.0
        } else {
            signed
        };
        if y == 0.0 {
            1.0
        } else if base_is_negative && magnitude_base != f32::INFINITY && !is_integral {
            f32::NAN
        } else {
            unit_or_signed
        }
    }

    #[test]
    fn emitted_pow_reproduces_metal_on_every_edge() {
        const NAN: f32 = f32::NAN;
        const INF: f32 = f32::INFINITY;
        let measured: &[(f32, f32, f32)] = &[
            (0.0, 3.0, 0.0),
            (0.0, -3.0, INF),
            (0.0, 2.0, 0.0),
            (0.0, -2.0, INF),
            (0.0, 0.5, 0.0),
            (0.0, 2.6, 0.0),
            (0.0, 0.0, 1.0),
            (0.0, -0.0, 1.0),
            (0.0, INF, 0.0),
            (0.0, -INF, INF),
            (0.0, NAN, NAN),
            (0.0, 1.0, 0.0),
            (-0.0, 3.0, -0.0),
            (-0.0, -3.0, -INF),
            (-0.0, 2.0, 0.0),
            (-0.0, -2.0, INF),
            (-0.0, 0.5, 0.0),
            (-0.0, 2.6, 0.0),
            (-0.0, 0.0, 1.0),
            (-0.0, -0.0, 1.0),
            (-0.0, INF, 0.0),
            (-0.0, -INF, INF),
            (-0.0, NAN, NAN),
            (-0.0, 1.0, -0.0),
            (-2.0, 3.0, -8.0),
            (-2.0, -3.0, -0.125),
            (-2.0, 2.0, 4.0),
            (-2.0, -2.0, 0.25),
            (-2.0, 0.5, NAN),
            (-2.0, 2.6, NAN),
            (-2.0, 0.0, 1.0),
            (-2.0, -0.0, 1.0),
            (-2.0, INF, INF),
            (-2.0, -INF, 0.0),
            (-2.0, NAN, NAN),
            (-2.0, 1.0, -2.0),
            (-1.0, 3.0, -1.0),
            (-1.0, -3.0, -1.0),
            (-1.0, 2.0, 1.0),
            (-1.0, -2.0, 1.0),
            (-1.0, 0.5, NAN),
            (-1.0, 2.6, NAN),
            (-1.0, 0.0, 1.0),
            (-1.0, -0.0, 1.0),
            (-1.0, INF, 1.0),
            (-1.0, -INF, 1.0),
            (-1.0, NAN, NAN),
            (-1.0, 1.0, -1.0),
            (-INF, 3.0, -INF),
            (-INF, -3.0, -0.0),
            (-INF, 2.0, INF),
            (-INF, -2.0, 0.0),
            (-INF, 0.5, INF),
            (-INF, 2.6, INF),
            (-INF, 0.0, 1.0),
            (-INF, -0.0, 1.0),
            (-INF, INF, INF),
            (-INF, -INF, 0.0),
            (-INF, NAN, NAN),
            (-INF, 1.0, -INF),
            (INF, 3.0, INF),
            (INF, -3.0, 0.0),
            (INF, 2.0, INF),
            (INF, -2.0, 0.0),
            (INF, 0.5, INF),
            (INF, 2.6, INF),
            (INF, 0.0, 1.0),
            (INF, -0.0, 1.0),
            (INF, INF, INF),
            (INF, -INF, 0.0),
            (INF, NAN, NAN),
            (INF, 1.0, INF),
            (2.0, 3.0, 8.0),
            (2.0, -3.0, 0.125),
            (2.0, 2.0, 4.0),
            (2.0, -2.0, 0.25),
            (2.0, 4.0, 16.0),
            (2.0, -1.0, 0.5),
            (2.0, 0.0, 1.0),
            (2.0, -0.0, 1.0),
            (2.0, INF, INF),
            (2.0, -INF, 0.0),
            (2.0, NAN, NAN),
            (2.0, 1.0, 2.0),
        ];
        assert_eq!(measured.len(), 7 * 12);
        for &(x, y, expected) in measured {
            let got = emitted_pow(x, y);
            if expected.is_nan() {
                assert!(got.is_nan(), "pow({x}, {y}) = {got}, expected NaN");
            } else {
                assert_eq!(got.to_bits(), expected.to_bits(), "pow({x}, {y})");
            }
        }
    }

    #[test]
    fn the_two_powr_guards_are_what_separate_pow_from_powr() {
        assert!(powr(f32::INFINITY, 0.0).is_nan());
        assert!(powr(0.0, 0.0).is_nan());
        assert!(powr(1.0, f32::INFINITY).is_nan());
        assert_eq!(emitted_pow(f32::INFINITY, 0.0).to_bits(), 1.0f32.to_bits());
        assert_eq!(emitted_pow(0.0, 0.0).to_bits(), 1.0f32.to_bits());
        assert_eq!(emitted_pow(1.0, f32::INFINITY).to_bits(), 1.0f32.to_bits());
    }

    #[test]
    fn the_unit_base_override_cannot_fire_at_a_finite_exponent() {
        for &y in &[0.416_666_66f32, 2.4, 1.0, -3.0, 1e30, f32::MIN_POSITIVE] {
            assert!(y.is_finite());
            for &x in &[1.0f32, -1.0, 0.0, 2.0, f32::INFINITY] {
                let answers_unit = x.abs() == 1.0 && !y.is_finite();
                assert!(!answers_unit, "pow({x}, {y})");
            }
        }
    }
}
