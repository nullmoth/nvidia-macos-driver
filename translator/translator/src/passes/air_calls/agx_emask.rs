use super::block_split::{labelled_block as block, CallSiteSplit};
use super::*;

const AGX_LOAD_EMASK_PREFIX: &str = "llvm.agx3.load.with.emask.global.";
const AGX_STORE_EMASK_PREFIX: &str = "llvm.agx3.store.with.emask.global.";
const AGX_EMASK_LANES: u32 = 4;

pub(in crate::passes) fn lower_agx3_edgecheck(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 3 {
        return Err(format!("{name} expects 3 operands"));
    }
    let mut out = Vec::new();
    let base = ensure_u32(ctx, &mut out, args[0], "llvm.agx3.edgecheck lane base")?;
    let low = ensure_u32(ctx, &mut out, args[1], "llvm.agx3.edgecheck range start")?;
    let count = ensure_u32(ctx, &mut out, args[2], "llvm.agx3.edgecheck range count")?;
    let uint_ty = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let zero = ctx.const_uint(0);
    let mut acc = zero;
    for lane in 0..AGX_EMASK_LANES {
        let lane_index = if lane == 0 {
            base
        } else {
            let lane_const = ctx.const_uint(lane);
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::IAdd,
                Some(uint_ty),
                Some(id),
                vec![Operand::IdRef(base), Operand::IdRef(lane_const)],
            ));
            id
        };
        let negated_lane = ctx.const_uint((-(lane as i32)) as u32);
        let non_negative = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::SGreaterThanEqual,
            Some(bool_ty),
            Some(non_negative),
            vec![Operand::IdRef(base), Operand::IdRef(negated_lane)],
        ));
        let relative = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ISub,
            Some(uint_ty),
            Some(relative),
            vec![Operand::IdRef(lane_index), Operand::IdRef(low)],
        ));
        let within = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::ULessThan,
            Some(bool_ty),
            Some(within),
            vec![Operand::IdRef(relative), Operand::IdRef(count)],
        ));
        let active = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(active),
            vec![Operand::IdRef(non_negative), Operand::IdRef(within)],
        ));
        let bit = ctx.const_uint(1u32 << lane);
        let lane_bits = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Select,
            Some(uint_ty),
            Some(lane_bits),
            vec![
                Operand::IdRef(active),
                Operand::IdRef(bit),
                Operand::IdRef(zero),
            ],
        ));
        if lane == 0 {
            acc = lane_bits;
        } else {
            let next = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::BitwiseOr,
                Some(uint_ty),
                Some(next),
                vec![Operand::IdRef(acc), Operand::IdRef(lane_bits)],
            ));
            acc = next;
        }
    }
    let result = coerce_u32_to_result(ctx, &mut out, acc, rty, res)?;
    if result != res {
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(result)],
        ));
    }
    Ok(out)
}

pub(in crate::passes) fn lower_agx_emask_memory_calls(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    loop {
        let names = air_names(&ctx.module);
        let Some(site) = find_next_agx_emask_memory_call(ctx, entry_idx, &names) else {
            return Ok(());
        };
        split_agx_emask_memory_call(ctx, entry_idx, site)?;
    }
}

#[derive(Clone)]
struct AgxMemorySite {
    block: usize,
    inst: usize,
    name: String,
    call: Instruction,
}

fn find_next_agx_emask_memory_call(
    ctx: &Ctx,
    entry_idx: usize,
    names: &crate::fxhash::HashMap<Word, String>,
) -> Option<AgxMemorySite> {
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                continue;
            };
            let Some(name) = names.get(callee) else {
                continue;
            };
            if name.starts_with(AGX_LOAD_EMASK_PREFIX) || name.starts_with(AGX_STORE_EMASK_PREFIX) {
                return Some(AgxMemorySite {
                    block: bi,
                    inst: ii,
                    name: name.clone(),
                    call: inst.clone(),
                });
            }
        }
    }
    None
}

fn split_agx_emask_memory_call(
    ctx: &mut Ctx,
    entry_idx: usize,
    site: AgxMemorySite,
) -> Result<(), String> {
    let args = idref_args(&site.call);
    let is_load = site.name.starts_with(AGX_LOAD_EMASK_PREFIX);
    let mut split = CallSiteSplit::open(ctx, entry_idx, site.block, site.inst, &site.name)?;

    let replacement = if is_load {
        plan_load(
            ctx,
            entry_idx,
            &site.name,
            &args,
            site.call.result_id,
            site.call.result_type,
        )?
    } else {
        plan_store(ctx, &site.name, &args)?
    };
    let cont_label = split.continuation(ctx);
    let lanes = replacement.lanes;
    let test_labels: Vec<Word> = (0..lanes).map(|_| ctx.module.fresh_id()).collect();
    let body_labels: Vec<Word> = (0..lanes).map(|_| ctx.module.fresh_id()).collect();

    let mask0 = ensure_u32(ctx, &mut split.prefix, replacement.mask0, "AGX emask mask0")?;
    let mask1 = ensure_u32(ctx, &mut split.prefix, replacement.mask1, "AGX emask mask1")?;
    let stride = ensure_u32(
        ctx,
        &mut split.prefix,
        replacement.stride,
        "AGX emask stride",
    )?;
    if let Some((scratch, zero)) = replacement.load_scratch {
        split.prefix.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(scratch), Operand::IdRef(zero)],
        ));
    }
    split.branch_prefix_to(test_labels[0]);

    let mut blocks = Vec::with_capacity((lanes as usize) * 2 + 1);
    for lane in 0..lanes {
        let lane_idx = lane as usize;
        let next = if lane + 1 == lanes {
            cont_label
        } else {
            test_labels[lane_idx + 1]
        };
        let mut test = Vec::new();
        let active = append_mask_bit_test(ctx, &mut test, mask0, mask1, lane)?;
        test.push(Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(next),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        ));
        test.push(Instruction::new(
            Op::BranchConditional,
            None,
            None,
            vec![
                Operand::IdRef(active),
                Operand::IdRef(body_labels[lane_idx]),
                Operand::IdRef(next),
            ],
        ));
        blocks.push(block(test_labels[lane_idx], test));

        let mut body = Vec::new();
        append_lane_memory_op(ctx, &mut body, &replacement, stride, lane)?;
        body.push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(next)],
        ));
        blocks.push(block(body_labels[lane_idx], body));
    }

    if let Some((scratch, _, result, rty)) = replacement.load_scratch_result {
        split.suffix.insert(
            0,
            Instruction::new(
                Op::Load,
                Some(rty),
                Some(result),
                vec![Operand::IdRef(scratch)],
            ),
        );
    }
    split.finish(ctx, entry_idx, blocks);
    Ok(())
}

struct MemoryReplacement {
    base: Word,
    value: Option<Word>,
    elem_ty: Word,
    ptr_pointee: Word,
    mask0: Word,
    mask1: Word,
    stride: Word,
    ptr_ty: Word,
    load_scratch: Option<(Word, Word)>,
    load_scratch_result: Option<(Word, Word, Word, Word)>,
    lanes: u32,
}

fn emask_value_shape(ctx: &Ctx, ty: Word) -> Option<(Word, u32)> {
    composite_shape(ctx, ty).or_else(|| direct_scalar_bit_width(ctx, ty).map(|_| (ty, 1)))
}

fn plan_load(
    ctx: &mut Ctx,
    entry_idx: usize,
    name: &str,
    args: &[Word],
    res: Option<Word>,
    rty: Option<Word>,
) -> Result<MemoryReplacement, String> {
    if args.len() != 4 {
        return Err(format!("{name} expects 4 operands"));
    }
    let res = res.ok_or_else(|| format!("{name} has no result id"))?;
    let rty = rty.ok_or_else(|| format!("{name} has no result type"))?;
    let (elem_ty, lanes) =
        emask_value_shape(ctx, rty).ok_or_else(|| format!("{name} result has no scalar shape"))?;
    if !(1..=AGX_EMASK_LANES).contains(&lanes) {
        return Err(format!("{name} result is not a scalar or 2-4 lane value"));
    }
    if !matches!(scalar_bit_width(ctx, elem_ty), 8 | 16 | 32) {
        return Err(format!("{name} result element is not 8-, 16-, or 32-bit"));
    }
    let ptr_ty = value_result_type(ctx, args[0])
        .ok_or_else(|| format!("{name} pointer operand has no type"))?;
    let ptr_pointee = pointer_pointee_type(ctx, ptr_ty)
        .ok_or_else(|| format!("{name} pointer operand is not a pointer"))?;
    let scratch = insert_function_scratch(ctx, entry_idx, rty);
    let zero = const_null_of(ctx, rty);
    Ok(MemoryReplacement {
        base: args[0],
        value: None,
        elem_ty,
        ptr_pointee,
        mask0: args[1],
        mask1: args[2],
        stride: args[3],
        ptr_ty,
        load_scratch: Some((scratch, zero)),
        load_scratch_result: Some((scratch, zero, res, rty)),
        lanes,
    })
}

fn plan_store(ctx: &mut Ctx, name: &str, args: &[Word]) -> Result<MemoryReplacement, String> {
    if args.len() != 5 {
        return Err(format!("{name} expects 5 operands"));
    }
    let value_ty =
        value_result_type(ctx, args[1]).ok_or_else(|| format!("{name} value has no type"))?;
    let (elem_ty, lanes) = emask_value_shape(ctx, value_ty)
        .ok_or_else(|| format!("{name} value has no scalar shape"))?;
    if !(1..=AGX_EMASK_LANES).contains(&lanes) {
        return Err(format!("{name} value is not a scalar or 2-4 lane value"));
    }
    if !matches!(scalar_bit_width(ctx, elem_ty), 8 | 16 | 32) {
        return Err(format!("{name} value element is not 8-, 16-, or 32-bit"));
    }
    let ptr_ty = value_result_type(ctx, args[0])
        .ok_or_else(|| format!("{name} pointer operand has no type"))?;
    let ptr_pointee = pointer_pointee_type(ctx, ptr_ty)
        .ok_or_else(|| format!("{name} pointer operand is not a pointer"))?;
    Ok(MemoryReplacement {
        base: args[0],
        value: Some(args[1]),
        elem_ty,
        ptr_pointee,
        mask0: args[2],
        mask1: args[3],
        stride: args[4],
        ptr_ty,
        load_scratch: None,
        load_scratch_result: None,
        lanes,
    })
}

fn append_lane_memory_op(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    replacement: &MemoryReplacement,
    stride: Word,
    lane: u32,
) -> Result<(), String> {
    let ptr = append_lane_ptr(
        ctx,
        out,
        replacement.ptr_ty,
        replacement.ptr_pointee,
        replacement.base,
        stride,
        lane,
    )?;
    if let Some(value) = replacement.value {
        let lane_value = if replacement.lanes == 1 {
            value
        } else {
            composite_extract(ctx, out, replacement.elem_ty, value, lane)
        };
        let lane_value = coerce_store_lane_value(
            ctx,
            out,
            lane_value,
            replacement.elem_ty,
            replacement.ptr_pointee,
        );
        out.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr), Operand::IdRef(lane_value)],
        ));
    } else {
        let (scratch, _) = replacement
            .load_scratch
            .ok_or("AGX emask load missing scratch")?;
        let rty = replacement
            .load_scratch_result
            .map(|(_, _, _, rty)| rty)
            .ok_or("AGX emask load missing result type")?;
        let load_ty = lane_load_type(ctx, replacement.elem_ty, replacement.ptr_pointee);
        let loaded_lane = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::Load,
            Some(load_ty),
            Some(loaded_lane),
            vec![Operand::IdRef(ptr)],
        ));
        let lane_value =
            coerce_loaded_lane_value(ctx, out, loaded_lane, load_ty, replacement.elem_ty);
        let stored = if replacement.lanes == 1 {
            lane_value
        } else {
            let current = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Load,
                Some(rty),
                Some(current),
                vec![Operand::IdRef(scratch)],
            ));
            let inserted = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::CompositeInsert,
                Some(rty),
                Some(inserted),
                vec![
                    Operand::IdRef(lane_value),
                    Operand::IdRef(current),
                    Operand::LiteralBit32(lane),
                ],
            ));
            inserted
        };
        out.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(scratch), Operand::IdRef(stored)],
        ));
    }
    Ok(())
}

fn lane_load_type(ctx: &Ctx, elem_ty: Word, ptr_pointee: Word) -> Word {
    if elem_ty == ptr_pointee {
        return elem_ty;
    }
    match (
        direct_scalar_bit_width(ctx, elem_ty),
        direct_scalar_bit_width(ctx, ptr_pointee),
    ) {
        (Some(a), Some(b)) if a == b => ptr_pointee,
        _ => elem_ty,
    }
}

fn coerce_loaded_lane_value(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    value_ty: Word,
    elem_ty: Word,
) -> Word {
    if value_ty == elem_ty {
        return value;
    }
    match (
        direct_scalar_bit_width(ctx, value_ty),
        direct_scalar_bit_width(ctx, elem_ty),
    ) {
        (Some(a), Some(b)) if a == b => {
            let coerced = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Bitcast,
                Some(elem_ty),
                Some(coerced),
                vec![Operand::IdRef(value)],
            ));
            coerced
        }
        _ => value,
    }
}

fn coerce_store_lane_value(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    value_ty: Word,
    ptr_pointee: Word,
) -> Word {
    if value_ty == ptr_pointee {
        return value;
    }
    match (
        direct_scalar_bit_width(ctx, value_ty),
        direct_scalar_bit_width(ctx, ptr_pointee),
    ) {
        (Some(a), Some(b)) if a == b => {
            let coerced = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Bitcast,
                Some(ptr_pointee),
                Some(coerced),
                vec![Operand::IdRef(value)],
            ));
            coerced
        }
        _ => value,
    }
}

fn append_lane_ptr(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    ptr_ty: Word,
    ptr_pointee: Word,
    base: Word,
    byte_stride: Word,
    lane: u32,
) -> Result<Word, String> {
    let offset = if lane == 0 {
        ctx.const_uint(0)
    } else {
        let elem_bits = direct_scalar_bit_width(ctx, ptr_pointee)
            .ok_or("AGX emask pointer pointee is not scalar-sized")?;
        let elem_bytes = elem_bits
            .checked_div(8)
            .filter(|bytes| *bytes != 0 && elem_bits % 8 == 0)
            .ok_or("AGX emask pointer pointee size is not byte-addressable")?;
        let elem_stride = if elem_bytes == 1 {
            byte_stride
        } else {
            let id = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::UDiv,
                Some(ctx.ty_uint()),
                Some(id),
                vec![
                    Operand::IdRef(byte_stride),
                    Operand::IdRef(ctx.const_uint(elem_bytes)),
                ],
            ));
            id
        };
        let lane_const = ctx.const_uint(lane);
        let id = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IMul,
            Some(ctx.ty_uint()),
            Some(id),
            vec![Operand::IdRef(elem_stride), Operand::IdRef(lane_const)],
        ));
        id
    };
    let ptr = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::PtrAccessChain,
        Some(ptr_ty),
        Some(ptr),
        vec![Operand::IdRef(base), Operand::IdRef(offset)],
    ));
    Ok(ptr)
}

fn append_mask_bit_test(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    mask0: Word,
    mask1: Word,
    lane: u32,
) -> Result<Word, String> {
    let uint_ty = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let combined = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint_ty),
        Some(combined),
        vec![Operand::IdRef(mask0), Operand::IdRef(mask1)],
    ));
    let bit = ctx.const_uint(1u32 << lane);
    let lane_bits = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint_ty),
        Some(lane_bits),
        vec![Operand::IdRef(combined), Operand::IdRef(bit)],
    ));
    let zero = ctx.const_uint(0);
    let active = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::INotEqual,
        Some(bool_ty),
        Some(active),
        vec![Operand::IdRef(lane_bits), Operand::IdRef(zero)],
    ));
    Ok(active)
}

fn ensure_u32(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    what: &str,
) -> Result<Word, String> {
    let Some(ty) = value_result_type(ctx, value) else {
        return Err(format!("{what} has no type"));
    };
    let Some(def) = type_def_of(ctx, ty) else {
        return Err(format!("{what} type is undefined"));
    };
    if def.class.opcode != Op::TypeInt {
        return Err(format!("{what} is not an integer"));
    }
    if def.operands.first() == Some(&Operand::LiteralBit32(32)) {
        return Ok(value);
    }
    let uint = ctx.ty_uint();
    let converted = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(converted),
        vec![Operand::IdRef(value)],
    ));
    Ok(converted)
}

fn coerce_u32_to_result(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    rty: Word,
    res: Word,
) -> Result<Word, String> {
    let Some(def) = type_def_of(ctx, rty) else {
        return Err("llvm.agx3.edgecheck result type is undefined".to_string());
    };
    if def.class.opcode != Op::TypeInt {
        return Err("llvm.agx3.edgecheck result type is not an integer".to_string());
    }
    if def.operands.first() == Some(&Operand::LiteralBit32(32)) {
        out.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(res),
            vec![Operand::IdRef(value)],
        ));
        return Ok(res);
    }
    out.push(Instruction::new(
        Op::UConvert,
        Some(rty),
        Some(res),
        vec![Operand::IdRef(value)],
    ));
    Ok(res)
}

fn direct_scalar_bit_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeInt | Op::TypeFloat => match def.operands.first() {
            Some(Operand::LiteralBit32(bits)) => Some(*bits),
            _ => None,
        },
        _ => None,
    }
}

fn insert_function_scratch(ctx: &mut Ctx, entry_idx: usize, pointee: Word) -> Word {
    let ptr_ty = ctx.ty_ptr(StorageClass::Function, pointee);
    let var = ctx.module.fresh_id();
    let entry = &mut ctx.module.functions[entry_idx].blocks[0];
    let at = entry
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(entry.instructions.len());
    entry.instructions.insert(
        at,
        Instruction::new(
            Op::Variable,
            Some(ptr_ty),
            Some(var),
            vec![Operand::StorageClass(StorageClass::Function)],
        ),
    );
    var
}

fn idref_args(inst: &Instruction) -> Vec<Word> {
    inst.operands[1..]
        .iter()
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Function, Module, ModuleHeader};

    fn inst(op: Op, ty: Option<Word>, id: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, ops)
    }

    #[test]
    fn edgecheck_lowers_to_four_lane_mask() {
        let mut ctx = Ctx::new(Module::new());
        let rty = ctx.ty_int16();
        let base = ctx.const_uint(8);
        let low = ctx.const_uint(0);
        let high = ctx.const_uint(10);
        let res = ctx.module.fresh_id();

        let insts = lower_agx3_edgecheck(
            &mut ctx,
            "llvm.agx3.edgecheck",
            res,
            rty,
            &[base, low, high],
        )
        .expect("edgecheck lowers");

        assert_eq!(insts.last().and_then(|inst| inst.result_id), Some(res));
        assert_eq!(
            insts.last().map(|inst| inst.class.opcode),
            Some(Op::UConvert)
        );
        for (op, count) in [
            (Op::SGreaterThanEqual, 4),
            (Op::ULessThan, 4),
            (Op::ISub, 4),
            (Op::UGreaterThanEqual, 0),
        ] {
            assert_eq!(
                insts.iter().filter(|inst| inst.class.opcode == op).count(),
                count,
                "{op:?}"
            );
        }
    }

    #[test]
    fn emask_store_split_preserves_phi_and_loop_ownership() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(300));
        module.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(1), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(5), vec![]),
            inst(
                Op::TypeVector,
                None,
                Some(6),
                vec![Operand::IdRef(4), Operand::LiteralBit32(4)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            inst(
                Op::TypeFunction,
                None,
                Some(8),
                vec![Operand::IdRef(1), Operand::IdRef(7), Operand::IdRef(6)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(30),
                vec![Operand::LiteralBit32(15)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(31),
                vec![Operand::LiteralBit32(4)],
            ),
            inst(
                Op::Constant,
                Some(4),
                Some(32),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        module.debug_names = vec![inst(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(200),
                Operand::LiteralString("llvm.agx3.store.with.emask.global.v4i32".to_string()),
            ],
        )];
        module.functions = vec![Function {
            def: Some(inst(
                Op::Function,
                Some(1),
                Some(100),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(8),
                ],
            )),
            parameters: vec![
                inst(Op::FunctionParameter, Some(7), Some(20), vec![]),
                inst(Op::FunctionParameter, Some(6), Some(21), vec![]),
            ],
            blocks: vec![
                block(
                    10,
                    vec![
                        inst(
                            Op::FunctionCall,
                            Some(1),
                            Some(40),
                            vec![
                                Operand::IdRef(200),
                                Operand::IdRef(20),
                                Operand::IdRef(21),
                                Operand::IdRef(30),
                                Operand::IdRef(30),
                                Operand::IdRef(31),
                            ],
                        ),
                        inst(Op::Branch, None, None, vec![Operand::IdRef(11)]),
                    ],
                ),
                block(
                    11,
                    vec![
                        inst(
                            Op::Phi,
                            Some(4),
                            Some(50),
                            vec![Operand::IdRef(32), Operand::IdRef(10)],
                        ),
                        inst(Op::Return, None, None, vec![]),
                    ],
                ),
            ],
            end: Some(inst(Op::FunctionEnd, None, None, vec![])),
        }];
        let mut loop_module = module.clone();
        loop_module
            .types_global_values
            .push(inst(Op::ConstantTrue, Some(5), Some(33), vec![]));
        let call = loop_module.functions[0].blocks[0].instructions[0].clone();
        loop_module.functions[0].blocks = vec![
            block(
                9,
                vec![inst(Op::Branch, None, None, vec![Operand::IdRef(10)])],
            ),
            block(
                10,
                vec![
                    call,
                    inst(
                        Op::LoopMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(20),
                            Operand::IdRef(12),
                            Operand::LoopControl(spirv::LoopControl::NONE),
                        ],
                    ),
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(33), Operand::IdRef(12), Operand::IdRef(20)],
                    ),
                ],
            ),
            block(
                12,
                vec![inst(Op::Branch, None, None, vec![Operand::IdRef(10)])],
            ),
            block(
                20,
                vec![
                    inst(
                        Op::Phi,
                        Some(4),
                        Some(50),
                        vec![Operand::IdRef(32), Operand::IdRef(10)],
                    ),
                    inst(Op::Return, None, None, vec![]),
                ],
            ),
        ];
        let mut ctx = Ctx::new(module);

        lower_agx_emask_memory_calls(&mut ctx, 0).expect("emask store splits");
        let function = &ctx.module.functions[0];
        assert!(function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .all(|inst| inst.class.opcode != Op::FunctionCall));
        assert_eq!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|inst| inst.class.opcode == Op::SelectionMerge)
                .count(),
            4
        );
        let phi = function.blocks.iter().find_map(|block| {
            block
                .instructions
                .iter()
                .find(|inst| inst.class.opcode == Op::Phi)
        });
        let phi = phi.expect("successor phi remains");
        assert_ne!(phi.operands.get(1), Some(&Operand::IdRef(10)));
        assert!(matches!(phi.operands.get(1), Some(Operand::IdRef(_))));

        let mut loop_ctx = Ctx::new(loop_module);
        lower_agx_emask_memory_calls(&mut loop_ctx, 0).expect("loop-header emask store splits");
        let function = &loop_ctx.module.functions[0];
        let header = function
            .blocks
            .iter()
            .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(10))
            .expect("original loop header");
        assert_eq!(
            header
                .instructions
                .iter()
                .filter(|inst| inst.class.opcode == Op::LoopMerge)
                .count(),
            1
        );
        assert_eq!(
            header.instructions.last().map(|inst| inst.class.opcode),
            Some(Op::Branch)
        );
        assert_eq!(
            function
                .blocks
                .iter()
                .filter(|block| {
                    block
                        .instructions
                        .iter()
                        .any(|inst| inst.class.opcode == Op::LoopMerge)
                })
                .count(),
            1,
            "the backedge target must remain the sole loop header"
        );
        let exit_test = function
            .blocks
            .iter()
            .find(|block| {
                block.instructions.last().is_some_and(|inst| {
                    inst.class.opcode == Op::BranchConditional
                        && inst.operands.first() == Some(&Operand::IdRef(33))
                })
            })
            .expect("lowered loop exit test");
        let selection = &exit_test.instructions[exit_test.instructions.len() - 2];
        assert_eq!(selection.class.opcode, Op::SelectionMerge);
        let private_merge = match selection.operands.first() {
            Some(Operand::IdRef(label)) => *label,
            other => panic!("selection has no private merge: {other:?}"),
        };
        assert_ne!(private_merge, 20);
        let passthrough = function
            .blocks
            .iter()
            .find(|block| {
                block.label.as_ref().and_then(|label| label.result_id) == Some(private_merge)
            })
            .expect("private loop-exit merge");
        assert!(matches!(
            passthrough.instructions.as_slice(),
            [instruction]
                if instruction.class.opcode == Op::Branch
                    && instruction.operands == [Operand::IdRef(20)]
        ));
        let merge_phi = function
            .blocks
            .iter()
            .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(20))
            .and_then(|block| block.instructions.first())
            .expect("loop merge phi");
        assert_eq!(
            merge_phi.operands.get(1),
            Some(&Operand::IdRef(private_merge))
        );
    }

    #[test]
    fn emask_load_in_the_entry_block_keeps_its_scratch_variable() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(300));
        module.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(1), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(6),
                vec![Operand::IdRef(4), Operand::LiteralBit32(4)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            inst(
                Op::TypeFunction,
                None,
                Some(8),
                vec![Operand::IdRef(1), Operand::IdRef(7)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(30),
                vec![Operand::LiteralBit32(15)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(31),
                vec![Operand::LiteralBit32(4)],
            ),
        ];
        module.debug_names = vec![inst(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(201),
                Operand::LiteralString("llvm.agx3.load.with.emask.global.v4i32".to_string()),
            ],
        )];
        module.functions = vec![Function {
            def: Some(inst(
                Op::Function,
                Some(1),
                Some(100),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(8),
                ],
            )),
            parameters: vec![inst(Op::FunctionParameter, Some(7), Some(20), vec![])],
            blocks: vec![block(
                10,
                vec![
                    inst(
                        Op::FunctionCall,
                        Some(6),
                        Some(40),
                        vec![
                            Operand::IdRef(201),
                            Operand::IdRef(20),
                            Operand::IdRef(30),
                            Operand::IdRef(30),
                            Operand::IdRef(31),
                        ],
                    ),
                    inst(Op::Return, None, None, vec![]),
                ],
            )],
            end: Some(inst(Op::FunctionEnd, None, None, vec![])),
        }];
        let mut ctx = Ctx::new(module);

        lower_agx_emask_memory_calls(&mut ctx, 0).expect("emask load splits");
        let function = &ctx.module.functions[0];
        let scratch: Vec<Word> = function.blocks[0]
            .instructions
            .iter()
            .take_while(|inst| inst.class.opcode == Op::Variable)
            .filter_map(|inst| inst.result_id)
            .collect();
        assert_eq!(
            scratch.len(),
            1,
            "the load allocates one entry-block scratch"
        );
        let reads = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find(|inst| inst.class.opcode == Op::Load && inst.result_id == Some(40))
            .expect("the call's result is now a load of the scratch");
        assert_eq!(reads.operands.first(), Some(&Operand::IdRef(scratch[0])));
        assert!(function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.class.opcode == Op::Store)
            .all(|inst| inst.operands.first() == Some(&Operand::IdRef(scratch[0]))));
    }
}
