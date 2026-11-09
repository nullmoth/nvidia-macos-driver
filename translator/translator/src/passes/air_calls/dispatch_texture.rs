use super::*;

pub(in crate::passes) fn lower_air_calls(ctx: &mut Ctx, entry_idx: usize) -> Result<(), String> {
    lower_agx_emask_memory_calls(ctx, entry_idx)?;
    lower_imageblock_block_copies(ctx, entry_idx)?;

    let names = air_names(&ctx.module);
    let v4 = ctx.ty_vecf(4);
    record_read_sampler_values(ctx, entry_idx, &names);

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    ctx.air_call_body = Some(crate::passes::AirCallBody {
        function: entry_idx,
        dominance: crate::passes::spirv_cfg::BlockDominance::of(
            &ctx.module.functions[entry_idx].blocks,
        ),
    });
    plan_mat8_nv_phis(ctx, entry_idx, &names);
    plan_mat8_fusion(ctx, entry_idx, &names);
    let sampler_selects = sampler_select_defs(ctx, entry_idx);
    for bi in 0..n_blocks {
        ctx.air_call_block = bi;
        let mut new_insts: Vec<Instruction> = vec![];
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        for inst in insts {
            if inst.class.opcode != Op::FunctionCall {
                new_insts.push(inst);
                continue;
            }
            let callee = match inst.operands.first() {
                Some(Operand::IdRef(c)) => *c,
                _ => {
                    new_insts.push(inst);
                    continue;
                }
            };
            let Some(name) = names.get(&callee) else {
                new_insts.push(inst);
                continue;
            };
            let args: Vec<Word> = inst.operands[1..]
                .iter()
                .filter_map(|o| match o {
                    Operand::IdRef(r) => Some(*r),
                    _ => None,
                })
                .collect();
            let res = inst.result_id;
            let rty = inst.result_type;

            if let Err(error) = refuse_texture_from_unforwarded_field(ctx, name, &args) {
                ctx.air_call_body = None;
                return Err(error);
            }
            let lowered = match lower_sampling_call(ctx, name, res, rty, &args, v4, &sampler_selects) {
                Ok(lowered) => lowered,
                Err(error) => {
                    ctx.air_call_body = None;
                    return Err(error);
                }
            };
            new_insts.extend(lowered);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = new_insts;
    }
    if let Err(error) = emit_mat8_nv_phis(ctx, entry_idx) {
        ctx.air_call_body = None;
        return Err(error);
    }
    if let Err(error) = fuse_mat8_coopmat_phis(ctx, entry_idx) {
        ctx.air_call_body = None;
        return Err(error);
    }
    ctx.air_call_body = None;
    Ok(())
}

/// `select cond, samplerA, samplerB` results of the function, by result id. A shader that picks one of two runtime
/// samplers with a condition (a filtering mode held in a constant) leaves the choice as a select of sampler values, which
/// Vulkan cannot sample through.
fn sampler_select_defs(ctx: &Ctx, entry_idx: usize) -> HashMap<Word, (Word, Word, Word)> {
    let mut defs = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if inst.class.opcode != Op::Select {
                continue;
            }
            let ids: Vec<Word> = inst
                .operands
                .iter()
                .filter_map(|o| match o {
                    Operand::IdRef(r) => Some(*r),
                    _ => None,
                })
                .collect();
            if let (Some(result), [cond, a, b]) = (inst.result_id, ids.as_slice()) {
                defs.insert(result, (*cond, *a, *b));
            }
        }
    }
    defs
}

/// A call that samples or gathers through a sampler chosen by a select runs once per candidate sampler and keeps the
/// result of the chosen one: both reads are side-effect free, and the choice is the original condition.
fn lower_sampling_call(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    v4: Word,
    selects: &HashMap<Word, (Word, Word, Word)>,
) -> Result<Vec<Instruction>, String> {
    let samples = name.starts_with("air.sample_") || name.starts_with("air.gather_");
    if let (true, Some(res), Some(rty), Some(&sampler)) = (samples, res, rty, args.get(1)) {
        if let Some(&(cond, when_true, when_false)) = selects.get(&sampler) {
            let sty = ctx.ty_sampler();
            let is_sampler = |ctx: &Ctx, v: Word| {
                value_result_type(ctx, v) == Some(sty) || selects.contains_key(&v)
            };
            if is_sampler(ctx, when_true) && is_sampler(ctx, when_false) {
                let (first, second) = (ctx.module.fresh_id(), ctx.module.fresh_id());
                let mut with_true = args.to_vec();
                with_true[1] = when_true;
                let mut with_false = args.to_vec();
                with_false[1] = when_false;
                let mut out = lower_sampling_call(ctx, name, Some(first), Some(rty), &with_true, v4, selects)?;
                out.extend(lower_sampling_call(ctx, name, Some(second), Some(rty), &with_false, v4, selects)?);
                out.push(Instruction::new(
                    Op::Select,
                    Some(rty),
                    Some(res),
                    vec![Operand::IdRef(cond), Operand::IdRef(first), Operand::IdRef(second)],
                ));
                return Ok(out);
            }
        }
    }
    lower_one(ctx, name, res, rty, args, v4)
}

/// A texture a call samples, gathers, reads or writes that came out of a field of a local object as a placeholder.
///
/// An operand that resolves to no image is read as a texture the shader never bound, and the call folds to zero (a read) or
/// to nothing (a write). That is true of a handle Metal itself left null, and false of a handle the emitter could not see
/// through: a helper that was not inlined reads the texture address out of the object its caller filled, and when the
/// caller's stores were not matched to the helper's loads the same fold hides a texture that is bound. The whole shader then
/// draws nothing and no error says why (a terminal with its background and no text). Facts recorded for such a load say
/// where the placeholder came from, so a call whose texture operand is one is refused, with the words the inline retry
/// looks for; the retry inlines the helper and forwards the fields, and the call is lowered again from the text it makes.
fn refuse_texture_from_unforwarded_field(
    ctx: &Ctx,
    name: &str,
    args: &[Word],
) -> Result<(), String> {
    let loses_data_when_absent = name.starts_with("air.sample_texture")
        || name.starts_with("air.sample_depth")
        || name.starts_with("air.sample_compare_depth")
        || name.starts_with("air.gather_")
        || name.starts_with("air.read_texture")
        || name.starts_with("air.read_depth")
        || name.starts_with("air.write_texture");
    let Some(&handle) = args.first() else {
        return Ok(());
    };
    if !loses_data_when_absent
        || !ctx
            .emit_sidecar
            .local_pointer_field_loads
            .iter()
            .any(|fact| fact.id == handle)
    {
        return Ok(());
    }
    let img = resolve_image_value(ctx, handle);
    if !texture_operand_is_absent(ctx, img) || operand_is_variant_absent_texture(ctx, img) {
        return Ok(());
    }
    let recoverable = [
        ImageOperandUse::Sampled,
        ImageOperandUse::Storage,
        ImageOperandUse::Query,
    ]
    .into_iter()
    .any(|use_as| recovered_image_for_private_operand(ctx, img, name, use_as).is_some());
    if recoverable {
        return Ok(());
    }
    Err(format!(
        "{name}: the texture operand is a pointer placeholder for a field of a local object whose store was not \
         forwarded to the load; folding the call to zero would hide a texture that may be bound"
    ))
}

fn record_read_sampler_values(ctx: &mut Ctx, entry_idx: usize, names: &HashMap<Word, String>) {
    ctx.read_sampler_values = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::FunctionCall)
        .filter(|inst| {
            matches!(inst.operands.first(), Some(Operand::IdRef(callee))
                if names.get(callee).is_some_and(|name| name == "air.get_read_sampler"))
        })
        .filter_map(|inst| inst.result_id)
        .collect();
}

pub(in crate::passes) fn lower_fence_texture(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects 1 operand"));
    }
    if res.is_some() || rty.is_some() {
        return Err(format!("{name} unexpectedly has a result"));
    }
    let scope = ctx.const_uint(Scope::Device as u32);
    let semantics =
        ctx.const_uint((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::IMAGE_MEMORY).bits());
    Ok(vec![Instruction::new(
        Op::MemoryBarrier,
        None,
        None,
        vec![
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
        ],
    )])
}

pub(in crate::passes) fn lower_atomic_global_store_i32(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 5 {
        return Err(format!("{name} expects 5 operands"));
    }
    if res.is_some() || rty.is_some() {
        return Err(format!("{name} unexpectedly has a result"));
    }
    let mut out = Vec::new();
    let ptr = atomic_i32_pointer(ctx, args[0], &mut out);
    let scope = ctx.const_uint(Scope::Device as u32);
    let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
    out.push(Instruction::new(
        Op::AtomicStore,
        None,
        None,
        vec![
            Operand::IdRef(ptr),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
            Operand::IdRef(args[1]),
        ],
    ));
    Ok(out)
}

pub(in crate::passes) fn lower_atomic_integer_rmw(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 5 {
        return Err(format!("{name} expects 5 operands"));
    }
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let scope_kind = if name.starts_with("air.atomic.local.") {
        Scope::Workgroup
    } else {
        Scope::Device
    };
    let op = match name {
        "air.atomic.local.max.s.i32" => Op::AtomicSMax,
        "air.atomic.global.max.u.i32" | "air.atomic.local.max.u.i32" => Op::AtomicUMax,
        "air.atomic.local.min.s.i32" => Op::AtomicSMin,
        "air.atomic.local.min.u.i32" => Op::AtomicUMin,
        "air.atomic.global.and.u.i32"
        | "air.atomic.local.and.u.i32"
        | "air.atomic.global.and.s.i32" => Op::AtomicAnd,
        "air.atomic.global.or.u.i32"
        | "air.atomic.local.or.u.i32"
        | "air.atomic.global.or.s.i32" => Op::AtomicOr,
        "air.atomic.global.xor.u.i32"
        | "air.atomic.local.xor.u.i32"
        | "air.atomic.global.xor.s.i32" => Op::AtomicXor,
        "air.atomic.global.xchg.i32" | "air.atomic.local.xchg.i32" => Op::AtomicExchange,
        _ => return Err(format!("unhandled integer atomic call {name}")),
    };
    let mut out = Vec::new();
    let ptr = atomic_i32_pointer(ctx, args[0], &mut out);
    let scope = ctx.const_uint(scope_kind as u32);
    let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
    out.push(Instruction::new(
        op,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(ptr),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
            Operand::IdRef(args[1]),
        ],
    ));
    Ok(out)
}

fn is_srgb_packed_format(name: &str) -> bool {
    name.contains(".srgb")
}

fn packed_format(name: &str) -> Option<(GLSLstd450, GLSLstd450, u32)> {
    use GLSLstd450 as G;

    if name.contains("unorm4x8") {
        Some((G::PackUnorm4x8, G::UnpackUnorm4x8, 4))
    } else if name.contains("snorm4x8") {
        Some((G::PackSnorm4x8, G::UnpackSnorm4x8, 4))
    } else if name.contains("unorm2x16") {
        Some((G::PackUnorm2x16, G::UnpackUnorm2x16, 2))
    } else if name.contains("snorm2x16") {
        Some((G::PackSnorm2x16, G::UnpackSnorm2x16, 2))
    } else if name.contains("half2x16") {
        Some((G::PackHalf2x16, G::UnpackHalf2x16, 2))
    } else {
        None
    }
}

pub(in crate::passes) fn lower_pack(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if name.starts_with("air.pack.unorm.rgb565") {
        return lower_pack_rgb565(ctx, name, res, rty, args);
    }
    if name.starts_with("air.pack.unorm.rgb10a2") {
        return lower_pack_rgb10a2(ctx, name, res, rty, args);
    }
    if is_srgb_packed_format(name) {
        return lower_pack_srgb4x8(ctx, name, res, rty, args);
    }
    let (pack_op, _, _) =
        packed_format(name).ok_or_else(|| format!("unhandled pack intrinsic: {name}"))?;
    let mut out = Vec::new();
    let mut pack_arg = args[0];
    if let Some(arg_ty) = value_result_type(ctx, args[0]) {
        let float_ty = float_equivalent(ctx, arg_ty);
        if float_ty != arg_ty {
            pack_arg = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::FConvert,
                Some(float_ty),
                Some(pack_arg),
                vec![Operand::IdRef(args[0])],
            ));
        }
    }
    out.push(Instruction::new(
        Op::ExtInst,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(ctx.glsl()),
            Operand::LiteralExtInstInteger(pack_op as u32),
            Operand::IdRef(pack_arg),
        ],
    ));
    Ok(out)
}

fn srgb_transfer(ctx: &mut Ctx, out: &mut Vec<Instruction>, value: Word, encode: bool) -> Word {
    let v4 = ctx.ty_vecf(4);
    let v4bool = ctx.ty_vec_bool(4);
    let ext = ctx.glsl();
    let splat = |ctx: &mut Ctx, out: &mut Vec<Instruction>, x: f32| {
        let scalar = ctx.const_float(x);
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeConstruct,
            Some(v4),
            Some(id),
            vec![Operand::IdRef(scalar); 4],
        ));
        id
    };
    let (edge, linear_scale, exponent, offset, gain) = if encode {
        (0.0031308, 12.92, 1.0 / 2.4, 0.055, 1.055)
    } else {
        (0.04045, 12.92, 2.4, 0.055, 1.055)
    };
    let edge = splat(ctx, out, edge);
    let scale = splat(ctx, out, linear_scale);
    let exponent = splat(ctx, out, exponent);
    let offset = splat(ctx, out, offset);
    let gain = splat(ctx, out, gain);

    let emit = |ctx: &mut Ctx, out: &mut Vec<Instruction>, op: Op, a: Word, b: Word| {
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            op,
            Some(v4),
            Some(id),
            vec![Operand::IdRef(a), Operand::IdRef(b)],
        ));
        id
    };
    let low = if encode {
        emit(ctx, out, Op::FMul, value, scale)
    } else {
        emit(ctx, out, Op::FDiv, value, scale)
    };
    let high = if encode {
        let zero = splat(ctx, out, 0.0);
        let base = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(v4),
            Some(base),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::FMax as u32),
                Operand::IdRef(value),
                Operand::IdRef(zero),
            ],
        ));
        let powed = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(v4),
            Some(powed),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Pow as u32),
                Operand::IdRef(base),
                Operand::IdRef(exponent),
            ],
        ));
        let scaled = emit(ctx, out, Op::FMul, powed, gain);
        emit(ctx, out, Op::FSub, scaled, offset)
    } else {
        let shifted = emit(ctx, out, Op::FAdd, value, offset);
        let base = emit(ctx, out, Op::FDiv, shifted, gain);
        let powed = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(v4),
            Some(powed),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::Pow as u32),
                Operand::IdRef(base),
                Operand::IdRef(exponent),
            ],
        ));
        powed
    };
    let is_low = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FOrdLessThanEqual,
        Some(v4bool),
        Some(is_low),
        vec![Operand::IdRef(value), Operand::IdRef(edge)],
    ));
    let selected = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(v4),
        Some(selected),
        vec![
            Operand::IdRef(is_low),
            Operand::IdRef(low),
            Operand::IdRef(high),
        ],
    ));
    let rebuilt = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::VectorShuffle,
        Some(v4),
        Some(rebuilt),
        vec![
            Operand::IdRef(selected),
            Operand::IdRef(value),
            Operand::LiteralBit32(0),
            Operand::LiteralBit32(1),
            Operand::LiteralBit32(2),
            Operand::LiteralBit32(7),
        ],
    ));
    rebuilt
}

fn srgb_decode_quantize(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    decoded: Word,
    raw: Word,
) -> Word {
    let v4 = ctx.ty_vecf(4);
    let ext = ctx.glsl();
    let steps = ctx.const_float(4095.0);
    let steps_v = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v4),
        Some(steps_v),
        vec![Operand::IdRef(steps); 4],
    ));
    let scaled = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FMul,
        Some(v4),
        Some(scaled),
        vec![Operand::IdRef(decoded), Operand::IdRef(steps_v)],
    ));
    let rounded = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(v4),
        Some(rounded),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::RoundEven as u32),
            Operand::IdRef(scaled),
        ],
    ));
    let stepped = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FDiv,
        Some(v4),
        Some(stepped),
        vec![Operand::IdRef(rounded), Operand::IdRef(steps_v)],
    ));
    let rebuilt = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::VectorShuffle,
        Some(v4),
        Some(rebuilt),
        vec![
            Operand::IdRef(stepped),
            Operand::IdRef(raw),
            Operand::LiteralBit32(0),
            Operand::LiteralBit32(1),
            Operand::LiteralBit32(2),
            Operand::LiteralBit32(7),
        ],
    ));
    rebuilt
}

fn lower_pack_srgb4x8(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects 1 operand"));
    }
    let arg_ty =
        value_result_type(ctx, args[0]).ok_or_else(|| format!("{name} operand has no type"))?;
    let v4 = ctx.ty_vecf(4);
    if float_equivalent(ctx, arg_ty) != v4 {
        return Err(format!(
            "{name} operand is not a four-component float vector"
        ));
    }
    let mut out = Vec::new();
    let mut value = args[0];
    if arg_ty != v4 {
        let widened = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(v4),
            Some(widened),
            vec![Operand::IdRef(value)],
        ));
        value = widened;
    }
    let encoded = srgb_transfer(ctx, &mut out, value, true);
    out.push(Instruction::new(
        Op::ExtInst,
        Some(rty),
        Some(res),
        vec![
            Operand::IdRef(ctx.glsl()),
            Operand::LiteralExtInstInteger(GLSLstd450::PackUnorm4x8 as u32),
            Operand::IdRef(encoded),
        ],
    ));
    Ok(out)
}

fn lower_unpack_srgb4x8(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects 1 operand"));
    }
    let v4 = ctx.ty_vecf(4);
    let mut out = Vec::new();
    let raw = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(v4),
        Some(raw),
        vec![
            Operand::IdRef(ctx.glsl()),
            Operand::LiteralExtInstInteger(GLSLstd450::UnpackUnorm4x8 as u32),
            Operand::IdRef(args[0]),
        ],
    ));
    let decoded = srgb_transfer(ctx, &mut out, raw, false);
    let quantized = srgb_decode_quantize(ctx, &mut out, decoded, raw);
    let (wide, narrow) = narrowed_unpack_result(ctx, res, rty);
    out.push(Instruction::new(
        Op::CopyObject,
        Some(v4),
        Some(wide),
        vec![Operand::IdRef(quantized)],
    ));
    out.extend(narrow);
    Ok(out)
}

fn lower_pack_rgb10a2(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects 1 operand"));
    }
    if int_scalar_width(ctx, rty) != Some(32) {
        return Err(format!("{name} result is not a 32-bit integer"));
    }
    let arg_ty = value_result_type(ctx, args[0])
        .ok_or_else(|| format!("{name} operand has no result type"))?;
    let v4float = ctx.ty_vecf(4);
    if float_equivalent(ctx, arg_ty) != v4float {
        return Err(format!(
            "{name} operand is not a four-component float vector"
        ));
    }

    let float = ctx.ty_float();
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let mut out = Vec::new();
    let vector = if arg_ty == v4float {
        args[0]
    } else {
        let widened = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(v4float),
            Some(widened),
            vec![Operand::IdRef(args[0])],
        ));
        widened
    };

    let zero = ctx.const_float(0.0);
    let one = ctx.const_float(1.0);
    let half = ctx.const_float(0.5);
    let uint_zero = ctx.const_uint(0);
    let uint_one = ctx.const_uint(1);
    let mut fields = Vec::with_capacity(4);
    for (component, maximum, shift) in [
        (0u32, 1023.0f32, 0u32),
        (1, 1023.0, 10),
        (2, 1023.0, 20),
        (3, 3.0, 30),
    ] {
        let scale = ctx.const_float(maximum);
        let extracted = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(float),
            Some(extracted),
            vec![Operand::IdRef(vector), Operand::LiteralBit32(component)],
        ));

        let clamped = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float),
            Some(clamped),
            vec![
                Operand::IdRef(ctx.glsl()),
                Operand::LiteralExtInstInteger(GLSLstd450::NClamp as u32),
                Operand::IdRef(extracted),
                Operand::IdRef(zero),
                Operand::IdRef(one),
            ],
        ));
        let product = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FMul,
            Some(float),
            Some(product),
            vec![Operand::IdRef(clamped), Operand::IdRef(scale)],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(product),
                Operand::Decoration(Decoration::NoContraction),
            ],
        ));
        let negated = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FNegate,
            Some(float),
            Some(negated),
            vec![Operand::IdRef(product)],
        ));
        let residual = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float),
            Some(residual),
            vec![
                Operand::IdRef(ctx.glsl()),
                Operand::LiteralExtInstInteger(GLSLstd450::Fma as u32),
                Operand::IdRef(clamped),
                Operand::IdRef(scale),
                Operand::IdRef(negated),
            ],
        ));
        let floored = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float),
            Some(floored),
            vec![
                Operand::IdRef(ctx.glsl()),
                Operand::LiteralExtInstInteger(GLSLstd450::Floor as u32),
                Operand::IdRef(product),
            ],
        ));
        let fraction = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FSub,
            Some(float),
            Some(fraction),
            vec![Operand::IdRef(product), Operand::IdRef(floored)],
        ));
        let base = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertFToU,
            Some(uint),
            Some(base),
            vec![Operand::IdRef(floored)],
        ));
        let above = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdGreaterThan,
            Some(bool_ty),
            Some(above),
            vec![Operand::IdRef(fraction), Operand::IdRef(half)],
        ));
        let on_half = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(on_half),
            vec![Operand::IdRef(fraction), Operand::IdRef(half)],
        ));
        let residual_positive = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdGreaterThan,
            Some(bool_ty),
            Some(residual_positive),
            vec![Operand::IdRef(residual), Operand::IdRef(zero)],
        ));
        let residual_zero = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FOrdEqual,
            Some(bool_ty),
            Some(residual_zero),
            vec![Operand::IdRef(residual), Operand::IdRef(zero)],
        ));
        let low_bit = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint),
            Some(low_bit),
            vec![Operand::IdRef(base), Operand::IdRef(uint_one)],
        ));
        let floor_is_odd = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::INotEqual,
            Some(bool_ty),
            Some(floor_is_odd),
            vec![Operand::IdRef(low_bit), Operand::IdRef(uint_zero)],
        ));
        let tie_to_even_up = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(tie_to_even_up),
            vec![Operand::IdRef(residual_zero), Operand::IdRef(floor_is_odd)],
        ));
        let tie_up = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::LogicalOr,
            Some(bool_ty),
            Some(tie_up),
            vec![
                Operand::IdRef(residual_positive),
                Operand::IdRef(tie_to_even_up),
            ],
        ));
        let on_half_up = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(on_half_up),
            vec![Operand::IdRef(on_half), Operand::IdRef(tie_up)],
        ));
        let round_up = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::LogicalOr,
            Some(bool_ty),
            Some(round_up),
            vec![Operand::IdRef(above), Operand::IdRef(on_half_up)],
        ));
        let carry = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(uint),
            Some(carry),
            vec![
                Operand::IdRef(round_up),
                Operand::IdRef(uint_one),
                Operand::IdRef(uint_zero),
            ],
        ));
        let integer = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IAdd,
            Some(uint),
            Some(integer),
            vec![Operand::IdRef(base), Operand::IdRef(carry)],
        ));
        if shift == 0 {
            fields.push(integer);
        } else {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftLeftLogical,
                Some(uint),
                Some(shifted),
                vec![
                    Operand::IdRef(integer),
                    Operand::IdRef(ctx.const_uint(shift)),
                ],
            ));
            fields.push(shifted);
        }
    }

    let mut accumulated = fields[0];
    for field in &fields[1..] {
        let combined = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseOr,
            Some(uint),
            Some(combined),
            vec![Operand::IdRef(accumulated), Operand::IdRef(*field)],
        ));
        accumulated = combined;
    }
    out.push(Instruction::new(
        Op::CopyObject,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(accumulated)],
    ));
    Ok(out)
}

fn lower_pack_rgb565(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!("{name} expects 1 operand"));
    }
    if int_scalar_width(ctx, rty) != Some(16) {
        return Err(format!("{name} result is not a 16-bit integer"));
    }
    let arg_ty = value_result_type(ctx, args[0])
        .ok_or_else(|| format!("{name} operand has no result type"))?;
    let v3float = ctx.ty_vecf(3);
    if float_equivalent(ctx, arg_ty) != v3float {
        return Err(format!(
            "{name} operand is not a three-component float vector"
        ));
    }

    let float = ctx.ty_float();
    let uint = ctx.ty_uint();
    let mut out = Vec::new();
    let vector = if arg_ty == v3float {
        args[0]
    } else {
        let widened = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FConvert,
            Some(v3float),
            Some(widened),
            vec![Operand::IdRef(args[0])],
        ));
        widened
    };

    let zero = ctx.const_float(0.0);
    let one = ctx.const_float(1.0);
    let mut fields = Vec::with_capacity(3);
    for (component, maximum, shift) in [(0u32, 31.0f32, 0u32), (1, 63.0, 5), (2, 31.0, 11)] {
        let extracted = ctx.module.fresh_id();
        let clamped = ctx.module.fresh_id();
        let scaled = ctx.module.fresh_id();
        let rounded = ctx.module.fresh_id();
        let integer = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(float),
            Some(extracted),
            vec![Operand::IdRef(vector), Operand::LiteralBit32(component)],
        ));

        out.push(Instruction::new(
            Op::ExtInst,
            Some(float),
            Some(clamped),
            vec![
                Operand::IdRef(ctx.glsl()),
                Operand::LiteralExtInstInteger(GLSLstd450::NClamp as u32),
                Operand::IdRef(extracted),
                Operand::IdRef(zero),
                Operand::IdRef(one),
            ],
        ));
        out.push(Instruction::new(
            Op::FMul,
            Some(float),
            Some(scaled),
            vec![
                Operand::IdRef(clamped),
                Operand::IdRef(ctx.const_float(maximum)),
            ],
        ));
        out.push(Instruction::new(
            Op::ExtInst,
            Some(float),
            Some(rounded),
            vec![
                Operand::IdRef(ctx.glsl()),
                Operand::LiteralExtInstInteger(GLSLstd450::RoundEven as u32),
                Operand::IdRef(scaled),
            ],
        ));
        out.push(Instruction::new(
            Op::ConvertFToU,
            Some(uint),
            Some(integer),
            vec![Operand::IdRef(rounded)],
        ));
        if shift == 0 {
            fields.push(integer);
        } else {
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftLeftLogical,
                Some(uint),
                Some(shifted),
                vec![
                    Operand::IdRef(integer),
                    Operand::IdRef(ctx.const_uint(shift)),
                ],
            ));
            fields.push(shifted);
        }
    }

    let rg = ctx.module.fresh_id();
    let packed = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(uint),
        Some(rg),
        vec![Operand::IdRef(fields[0]), Operand::IdRef(fields[1])],
    ));
    out.push(Instruction::new(
        Op::BitwiseOr,
        Some(uint),
        Some(packed),
        vec![Operand::IdRef(rg), Operand::IdRef(fields[2])],
    ));
    out.push(Instruction::new(
        Op::UConvert,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(packed)],
    ));
    Ok(out)
}

fn narrowed_unpack_result(ctx: &mut Ctx, res: Word, rty: Word) -> (Word, Option<Instruction>) {
    if !is_half_vector(ctx, rty) {
        return (res, None);
    }
    let wide = ctx.module.fresh_id();
    let narrow = Instruction::new(
        Op::FConvert,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(wide)],
    );
    (wide, Some(narrow))
}

pub(in crate::passes) fn lower_unpack_rgb10a2(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let uint_ty = ctx.ty_uint();
    let float_ty = ctx.ty_float();
    let v_float = ctx.ty_vecf(4);
    let c1023 = ctx.const_float(1023.0);
    let c3 = ctx.const_float(3.0);
    let fields = [
        (0u32, 0x3FFu32, c1023),
        (10, 0x3FF, c1023),
        (20, 0x3FF, c1023),
        (30, 0x3, c3),
    ];
    let mut out = Vec::new();
    let mut comps = Vec::with_capacity(4);
    for (shift, mask, div) in fields {
        let mut val = args[0];
        if shift != 0 {
            let sh = ctx.const_uint(shift);
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(uint_ty),
                Some(shifted),
                vec![Operand::IdRef(val), Operand::IdRef(sh)],
            ));
            val = shifted;
        }
        let maskc = ctx.const_uint(mask);
        let masked = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(masked),
            vec![Operand::IdRef(val), Operand::IdRef(maskc)],
        ));
        let asf = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertUToF,
            Some(float_ty),
            Some(asf),
            vec![Operand::IdRef(masked)],
        ));
        let comp = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FDiv,
            Some(float_ty),
            Some(comp),
            vec![Operand::IdRef(asf), Operand::IdRef(div)],
        ));
        comps.push(comp);
    }
    let (vec_res, narrow) = narrowed_unpack_result(ctx, res, rty);
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v_float),
        Some(vec_res),
        comps.iter().map(|c| Operand::IdRef(*c)).collect(),
    ));
    out.extend(narrow);
    Ok(out)
}

pub(in crate::passes) fn lower_unpack_rg11b10f(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let uint_ty = ctx.ty_uint();
    let float_ty = ctx.ty_float();
    let v2_float = ctx.ty_vecf(2);
    let v_float = ctx.ty_vecf(3);
    let ext = ctx.glsl();
    let fields = [
        (0u32, 0x7FFu32, 6u32, 0x3Fu32, 4u32),
        (11, 0x7FF, 6, 0x3F, 4),
        (22, 0x3FF, 5, 0x1F, 5),
    ];
    let mut out = Vec::new();
    let mut comps = Vec::with_capacity(3);
    for (fshift, fmask, eshift, mmask, mls) in fields {
        let mut fv = args[0];
        if fshift != 0 {
            let sh = ctx.const_uint(fshift);
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(uint_ty),
                Some(shifted),
                vec![Operand::IdRef(fv), Operand::IdRef(sh)],
            ));
            fv = shifted;
        }
        let fmaskc = ctx.const_uint(fmask);
        let field = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(field),
            vec![Operand::IdRef(fv), Operand::IdRef(fmaskc)],
        ));
        let eshiftc = ctx.const_uint(eshift);
        let exp_raw = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ShiftRightLogical,
            Some(uint_ty),
            Some(exp_raw),
            vec![Operand::IdRef(field), Operand::IdRef(eshiftc)],
        ));
        let c10 = ctx.const_uint(10);
        let exp_hi = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ShiftLeftLogical,
            Some(uint_ty),
            Some(exp_hi),
            vec![Operand::IdRef(exp_raw), Operand::IdRef(c10)],
        ));
        let mmaskc = ctx.const_uint(mmask);
        let mant_raw = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(mant_raw),
            vec![Operand::IdRef(field), Operand::IdRef(mmaskc)],
        ));
        let mlsc = ctx.const_uint(mls);
        let mant_hi = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ShiftLeftLogical,
            Some(uint_ty),
            Some(mant_hi),
            vec![Operand::IdRef(mant_raw), Operand::IdRef(mlsc)],
        ));
        let half_bits = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseOr,
            Some(uint_ty),
            Some(half_bits),
            vec![Operand::IdRef(exp_hi), Operand::IdRef(mant_hi)],
        ));
        let unpacked = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(v2_float),
            Some(unpacked),
            vec![
                Operand::IdRef(ext),
                Operand::LiteralExtInstInteger(GLSLstd450::UnpackHalf2x16 as u32),
                Operand::IdRef(half_bits),
            ],
        ));
        let comp = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(float_ty),
            Some(comp),
            vec![Operand::IdRef(unpacked), Operand::LiteralBit32(0)],
        ));
        comps.push(comp);
    }
    let (vec_res, narrow) = narrowed_unpack_result(ctx, res, rty);
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v_float),
        Some(vec_res),
        comps.iter().map(|c| Operand::IdRef(*c)).collect(),
    ));
    out.extend(narrow);
    Ok(out)
}

pub(in crate::passes) fn lower_unpack_rgb9e5(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let uint_ty = ctx.ty_uint();
    let float_ty = ctx.ty_float();
    let v_float = ctx.ty_vecf(3);
    let ext = ctx.glsl();
    let mut out = Vec::new();
    let c27 = ctx.const_uint(27);
    let exp_sh = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(uint_ty),
        Some(exp_sh),
        vec![Operand::IdRef(args[0]), Operand::IdRef(c27)],
    ));
    let c1f = ctx.const_uint(0x1F);
    let exp_m = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint_ty),
        Some(exp_m),
        vec![Operand::IdRef(exp_sh), Operand::IdRef(c1f)],
    ));
    let exp_f = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ConvertUToF,
        Some(float_ty),
        Some(exp_f),
        vec![Operand::IdRef(exp_m)],
    ));
    let c24 = ctx.const_float(24.0);
    let exp_adj = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::FSub,
        Some(float_ty),
        Some(exp_adj),
        vec![Operand::IdRef(exp_f), Operand::IdRef(c24)],
    ));
    let scale = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float_ty),
        Some(scale),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Exp2 as u32),
            Operand::IdRef(exp_adj),
        ],
    ));
    let mut comps = Vec::with_capacity(3);
    for shift in [0u32, 9, 18] {
        let mut mv = args[0];
        if shift != 0 {
            let sh = ctx.const_uint(shift);
            let shifted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(uint_ty),
                Some(shifted),
                vec![Operand::IdRef(mv), Operand::IdRef(sh)],
            ));
            mv = shifted;
        }
        let c1ff = ctx.const_uint(0x1FF);
        let mant = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(mant),
            vec![Operand::IdRef(mv), Operand::IdRef(c1ff)],
        ));
        let mant_f = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ConvertUToF,
            Some(float_ty),
            Some(mant_f),
            vec![Operand::IdRef(mant)],
        ));
        let comp = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::FMul,
            Some(float_ty),
            Some(comp),
            vec![Operand::IdRef(mant_f), Operand::IdRef(scale)],
        ));
        comps.push(comp);
    }
    let (vec_res, narrow) = narrowed_unpack_result(ctx, res, rty);
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v_float),
        Some(vec_res),
        comps.iter().map(|c| Operand::IdRef(*c)).collect(),
    ));
    out.extend(narrow);
    Ok(out)
}

pub(in crate::passes) fn lower_unpack(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if is_srgb_packed_format(name) {
        return lower_unpack_srgb4x8(ctx, name, res, rty, args);
    }
    let (_, unpack_op, n) =
        packed_format(name).ok_or_else(|| format!("unhandled unpack intrinsic: {name}"))?;
    let ext = ctx.glsl();
    let v_float = ctx.ty_vecf(n);
    let (unpacked, narrow) = narrowed_unpack_result(ctx, res, rty);
    let mut out = vec![Instruction::new(
        Op::ExtInst,
        Some(v_float),
        Some(unpacked),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(unpack_op as u32),
            Operand::IdRef(args[0]),
        ],
    )];
    out.extend(narrow);
    Ok(out)
}

pub(in crate::passes) fn lower_image_size_query(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    if args.is_empty() {
        return Err(format!("{name} missing texture"));
    }
    let mut img = resolve_image_value(ctx, args[0]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(query_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Query)
        {
            img = query_img;
        } else {
            let zero = ctx.const_int_of(rty, 0);
            return Ok(vec![Instruction::new(
                Op::CopyObject,
                Some(rty),
                Some(res),
                vec![Operand::IdRef(zero)],
            )]);
        }
    }
    let (dim, arrayed) = ctx
        .image_dims
        .get(&img)
        .copied()
        .unwrap_or((Dim::Dim2D, false));
    let spatial = match dim {
        Dim::Dim1D | Dim::DimBuffer => 1,
        Dim::Dim3D => 3,
        _ => 2,
    };
    let ncomp = spatial + if arrayed { 1 } else { 0 };
    let is_array_size_query = name.starts_with("air.get_array_size_texture");
    if is_array_size_query && !arrayed {
        return Err(format!(
            "{name} used on non-array texture id {img} ({})",
            describe_value(ctx, img)
        ));
    }
    let comp = if is_array_size_query {
        spatial
    } else if name.starts_with("air.get_height_") {
        1
    } else if name.starts_with("air.get_depth_") {
        2
    } else {
        0
    };
    let lod = args.get(1).copied();
    let uint = ctx.ty_uint();
    let size_ty = if ncomp == 1 {
        uint
    } else {
        ctx.ty_vec_uint(ncomp)
    };
    let size = ctx.module.fresh_id();
    let mut out = vec![];
    let query_op = image_size_query_op(ctx, img);
    let mut ops = vec![Operand::IdRef(img)];
    if query_op == Op::ImageQuerySizeLod {
        let lod = lod.unwrap_or_else(|| ctx.const_uint(0));
        ops.push(Operand::IdRef(lod));
    }
    out.push(Instruction::new(query_op, Some(size_ty), Some(size), ops));
    let comp_u = if ncomp == 1 {
        size
    } else {
        let c = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(c),
            vec![
                Operand::IdRef(size),
                Operand::LiteralBit32(comp.min(ncomp - 1)),
            ],
        ));
        c
    };
    out.push(copy_or_bitcast_result(rty, res, uint, comp_u));
    Ok(out)
}

pub(in crate::passes) fn lower_is_null_texture(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let img = args
        .first()
        .copied()
        .ok_or_else(|| format!("{name} missing texture"))?;
    let is_null = ctx.null_image_values.contains(&img);
    let c = ctx.const_bool_of(rty, is_null);
    Ok(vec![Instruction::new(
        Op::CopyObject,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(c)],
    )])
}

pub(in crate::passes) fn lower_get_num_mip_levels(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let mut img = args
        .first()
        .copied()
        .ok_or_else(|| format!("{name} missing texture"))?;
    img = resolve_image_value(ctx, img);
    if texture_operand_is_absent(ctx, img) {
        if let Some(query_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Query)
        {
            img = query_img;
        } else {
            let uint = ctx.ty_uint();
            let one = ctx.const_uint(1);
            return Ok(vec![copy_or_bitcast_result(rty, res, uint, one)]);
        }
    }
    if image_is_storage(ctx, img) {
        let uint = ctx.ty_uint();
        let one = ctx.const_uint(1);
        return Ok(vec![copy_or_bitcast_result(rty, res, uint, one)]);
    }
    let uint = ctx.ty_uint();
    let levels = ctx.module.fresh_id();
    Ok(vec![
        Instruction::new(
            Op::ImageQueryLevels,
            Some(uint),
            Some(levels),
            vec![Operand::IdRef(img)],
        ),
        copy_or_bitcast_result(rty, res, uint, levels),
    ])
}

pub(in crate::passes) fn lower_get_num_samples_texture(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    if args.len() != 1 {
        return Err(format!("{name} expects exactly one image operand"));
    }
    let result_definition = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .find(|i| i.result_id == Some(rty));
    if !result_definition.is_some_and(|i| {
        i.class.opcode == Op::TypeInt && i.operands.first() == Some(&Operand::LiteralBit32(32))
    }) {
        return Err(format!("{name} requires a 32-bit integer result"));
    }
    let depth_query = name.starts_with("air.get_num_samples_depth_");
    let uint = ctx.ty_uint();
    let single_sample = |ctx: &mut Ctx| {
        let one = ctx.const_uint(1);
        vec![copy_or_bitcast_result(rty, res, uint, one)]
    };
    let Some(mut img) = args.first().copied() else {
        return Err(format!("{name} missing texture"));
    };
    img = resolve_image_value(ctx, img);
    if texture_operand_is_absent(ctx, img) {
        match recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Query) {
            Some(query_img) => img = query_img,
            None if depth_query => {
                return Err(format!("{name} lacks a resolved multisample image"))
            }
            None => return Ok(single_sample(ctx)),
        }
    }
    let mut out = Vec::new();
    let img = load_image_if_pointer(ctx, img, &mut out);
    if depth_query {
        let Some((_, dim, arrayed, _)) = image_type_from_value_or_pointer(ctx, img) else {
            return Err(format!("{name} requires an actual typed image operand"));
        };
        if dim != Dim::Dim2D
            || arrayed != name.ends_with("_array")
            || !image_value_is_multisampled(ctx, img)
        {
            return Err(format!(
                "{name} image must match the 2D multisample/array ABI"
            ));
        }
    }
    if !image_value_is_multisampled(ctx, img) {
        return Ok(single_sample(ctx));
    }
    let (dim, _, _) = image_shape_or_recorded(ctx, img);
    if dim != Dim::Dim2D {
        return Err(format!(
            "{name} requires a 2D multisample texture; OpImageQuerySamples is undefined for {dim:?}"
        ));
    }
    let samples = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ImageQuerySamples,
        Some(uint),
        Some(samples),
        vec![Operand::IdRef(img)],
    ));
    out.push(copy_or_bitcast_result(rty, res, uint, samples));
    Ok(out)
}

pub(in crate::passes) fn lower_raster_sample_count(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    if ctx.stage != Stage::Fragment {
        return Err(format!("{name} requires fragment stage"));
    }
    let samples = ctx.raster_sample_count.ok_or_else(|| {
        format!(
            "{name} requires TransformOptions::raster_sample_count to match the graphics pipeline"
        )
    })?;
    if !matches!(samples, 1 | 2 | 4 | 8 | 16 | 32 | 64) {
        return Err(format!(
            "{name} raster sample count {samples} is not a Vulkan sample-count value"
        ));
    }
    let uint = ctx.ty_uint();
    let value = ctx.const_uint(samples);
    Ok(vec![copy_or_bitcast_result(rty, res, uint, value)])
}

pub(in crate::passes) fn lower_calculate_lod_texture_2d(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
    args: &[Word],
    component: u32,
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    if args.len() < 3 {
        return Err(format!("{name} missing texture/sampler/coord"));
    }
    if ctx.stage != Stage::Fragment {
        return Err(format!("{name} requires fragment implicit derivatives"));
    }

    let mut img = resolve_image_value(ctx, args[0]);
    if texture_operand_is_absent(ctx, img) {
        if let Some(sampled_img) =
            recovered_image_for_private_operand(ctx, img, name, ImageOperandUse::Sampled)
        {
            img = sampled_img;
        }
    }
    if image_is_storage(ctx, img) {
        return Err(format!("{name} requires a sampled texture"));
    }

    let mut out = Vec::new();
    img = load_image_if_pointer(ctx, img, &mut out);
    let (dim, arrayed, comp) = image_shape_or_recorded(ctx, img);
    if dim != Dim::Dim2D || arrayed {
        return Err(format!("{name} requires a non-array 2D texture"));
    }
    let (img_ty, _, _, _) = sampled_operand_image_info(ctx, img, dim, false, comp);
    let si_ty = ctx.ty_sampled_image(img_ty);
    validate_runtime_sampler_specialization(ctx, args[1])?;
    if ctx
        .sampler_states
        .get(&args[1])
        .is_some_and(|state| state.uses_pixel_coordinates())
    {
        return Err(format!(
            "{name} with a pixel-coordinate sampler is unsupported because Vulkan LOD queries require normalized sampling coordinates"
        ));
    }
    let samp = if comp == crate::passes::ImageComp::Float {
        valid_sampler_value(ctx, args[1], &mut out)?
    } else {
        let var = ctx.default_read_sampler()?;
        let sty = ctx.ty_sampler();
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(sty),
            Some(id),
            vec![Operand::IdRef(var)],
        ));
        id
    };
    let si = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::SampledImage,
        Some(si_ty),
        Some(si),
        vec![Operand::IdRef(img), Operand::IdRef(samp)],
    ));
    let lod_pair_ty = ctx.ty_vecf(2);
    let lod_pair = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::ImageQueryLod,
        Some(lod_pair_ty),
        Some(lod_pair),
        vec![Operand::IdRef(si), Operand::IdRef(args[2])],
    ));

    let float_ty = ctx.ty_float();
    let lod = if rty == float_ty {
        res
    } else {
        ctx.module.fresh_id()
    };
    out.push(Instruction::new(
        Op::CompositeExtract,
        Some(float_ty),
        Some(lod),
        vec![Operand::IdRef(lod_pair), Operand::LiteralBit32(component)],
    ));
    if lod != res {
        out.push(Instruction::new(
            Op::FConvert,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(lod)],
        ));
    }
    Ok(out)
}

pub(in crate::passes) fn lower_get_null_texture(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let (dim, arrayed) = intrinsic_texture_shape(name).unwrap_or((Dim::Dim2D, false));
    let var = ctx.default_null_image_of(dim, arrayed)?;
    let img_ty = ctx.ty_image(dim, arrayed, crate::passes::ImageComp::Float);
    ctx.image_dims.insert(res, (dim, arrayed));
    ctx.image_comp.insert(res, crate::passes::ImageComp::Float);
    ctx.null_image_values.insert(res);
    Ok(vec![Instruction::new(
        Op::Load,
        Some(img_ty),
        Some(res),
        vec![Operand::IdRef(var)],
    )])
}

pub(in crate::passes) fn lower_get_imageblock_extent(
    ctx: &mut Ctx,
    name: &str,
    res: Option<Word>,
    rty: Option<Word>,
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| format!("{name} has no result"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let axis = usize::from(name == "air.get_imageblock_height");
    let extent = ctx.kernel_local_size_ids()[axis];
    let uint_ty = ctx.ty_uint();
    Ok(vec![Instruction::new(
        if rty == uint_ty {
            Op::CopyObject
        } else {
            Op::UConvert
        },
        Some(rty),
        Some(res),
        vec![Operand::IdRef(extent)],
    )])
}

pub(in crate::passes) fn lower_get_read_sampler(
    ctx: &mut Ctx,
    res: Option<Word>,
) -> Result<Vec<Instruction>, String> {
    let res = res.ok_or_else(|| "air.get_read_sampler has no result".to_string())?;
    let var = ctx.default_read_sampler()?;
    let sty = ctx.ty_sampler();
    Ok(vec![Instruction::new(
        Op::Load,
        Some(sty),
        Some(res),
        vec![Operand::IdRef(var)],
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Module;

    fn ext_inst_number(instructions: &[Instruction]) -> u32 {
        instructions
            .iter()
            .find(|inst| inst.class.opcode == Op::ExtInst)
            .and_then(|inst| inst.operands.get(1))
            .and_then(|operand| match operand {
                Operand::LiteralExtInstInteger(number) => Some(*number),
                _ => None,
            })
            .expect("GLSL.std.450 instruction number")
    }

    #[test]
    fn packed_formats_pair_inverse_operations_and_widths() {
        let cases = [
            (
                "air.pack.unorm4x8.v4f32",
                GLSLstd450::PackUnorm4x8,
                GLSLstd450::UnpackUnorm4x8,
                4,
            ),
            (
                "air.unpack.snorm4x8.v4f32",
                GLSLstd450::PackSnorm4x8,
                GLSLstd450::UnpackSnorm4x8,
                4,
            ),
            (
                "air.pack.unorm2x16.v2f32",
                GLSLstd450::PackUnorm2x16,
                GLSLstd450::UnpackUnorm2x16,
                2,
            ),
            (
                "air.unpack.snorm2x16.v2f32",
                GLSLstd450::PackSnorm2x16,
                GLSLstd450::UnpackSnorm2x16,
                2,
            ),
            (
                "air.pack.half2x16.v2f32",
                GLSLstd450::PackHalf2x16,
                GLSLstd450::UnpackHalf2x16,
                2,
            ),
        ];

        for (name, pack_op, unpack_op, component_count) in cases {
            let (actual_pack, actual_unpack, actual_count) =
                packed_format(name).unwrap_or_else(|| panic!("classify {name}"));
            assert_eq!(actual_pack as u32, pack_op as u32, "{name}");
            assert_eq!(actual_unpack as u32, unpack_op as u32, "{name}");
            assert_eq!(actual_count, component_count, "{name}");
        }
        assert!(packed_format("air.unpack.unorm.rgb10a2.v4f32").is_none());
    }

    #[test]
    fn pack_and_unpack_lowering_use_the_paired_format_table() {
        let mut ctx = Ctx::new(Module::new());
        let vec_ty = ctx.ty_vecf(4);
        let scalar = ctx.const_float(0.25);
        let pack_arg = splat(&mut ctx, vec_ty, scalar, 4);
        let uint_ty = ctx.ty_uint();
        let pack_res = ctx.module.fresh_id();
        let packed = lower_pack(
            &mut ctx,
            "air.pack.unorm4x8.v4f32",
            pack_res,
            uint_ty,
            &[pack_arg],
        )
        .expect("lower pack");
        assert_eq!(packed.len(), 1);
        assert_eq!(ext_inst_number(&packed), GLSLstd450::PackUnorm4x8 as u32);

        let unpack_res = ctx.module.fresh_id();
        let unpacked = lower_unpack(
            &mut ctx,
            "air.unpack.unorm4x8.v4f32",
            unpack_res,
            vec_ty,
            &[pack_res],
        )
        .expect("lower unpack");
        assert_eq!(unpacked.len(), 1);
        assert_eq!(
            ext_inst_number(&unpacked),
            GLSLstd450::UnpackUnorm4x8 as u32
        );
    }
}
