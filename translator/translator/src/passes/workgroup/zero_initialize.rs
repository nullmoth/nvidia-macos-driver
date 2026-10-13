use super::*;

pub(in crate::passes) fn zero_initialize_workgroup_memory(ctx: &mut Ctx, entry_idx: usize) {
    let vars: Vec<(Word, Word)> = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .filter(|inst| {
            inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup))
        })
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect();
    if vars.is_empty() {
        return;
    }

    let shader_initialized_atomic_vars =
        shader_initialized_atomic_workgroup_vars(ctx, entry_idx, &vars);
    let mut null_by_pointee: HashMap<Word, Word> = HashMap::default();
    let mut prologue = Vec::new();
    let cooperative =
        std::env::var_os("NVMTL_WG_ZERO_ALL").is_none() && entry_label(ctx, entry_idx).is_some();
    let mut strided: Vec<StridedFill> = Vec::new();
    for (var, ptr_ty) in vars {
        if shader_initialized_atomic_vars.contains(&var) {
            continue;
        }
        let Some(ptr_def) = type_def_of(ctx, ptr_ty) else {
            continue;
        };
        if ptr_def.class.opcode != Op::TypePointer {
            continue;
        }
        let Some(&Operand::IdRef(pointee)) = ptr_def.operands.get(1) else {
            continue;
        };
        if cooperative {
            if let Some(fill) = strided_fill_of(ctx, var, pointee, &mut null_by_pointee) {
                strided.push(fill);
                continue;
            }
        }
        let null_id = *null_by_pointee.entry(pointee).or_insert_with(|| {
            let id = ctx.module.fresh_id();
            ctx.new_globals.push(Instruction::new(
                Op::ConstantNull,
                Some(pointee),
                Some(id),
                vec![],
            ));
            id
        });
        prologue.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(var), Operand::IdRef(null_id)],
        ));
    }
    if prologue.is_empty() && strided.is_empty() {
        return;
    }
    let scope = ctx.const_uint(Scope::Workgroup as u32);
    let semantics = ctx
        .const_uint((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY).bits());
    prologue.push(Instruction::new(
        Op::ControlBarrier,
        None,
        None,
        vec![
            Operand::IdScope(scope),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
        ],
    ));

    if !strided.is_empty() {
        let barrier = prologue.pop().expect("the fill barrier was pushed last");
        emit_strided_fills(ctx, entry_idx, prologue, strided, barrier);
        return;
    }

    let Some(block) = ctx.module.functions[entry_idx].blocks.first_mut() else {
        return;
    };
    let insert_at = block
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(block.instructions.len());
    block.instructions.splice(insert_at..insert_at, prologue);
}

struct StridedFill {
    var: Word,
    elem_ptr: Word,
    elem_null: Word,
    chunks: u32,
    chunk: u32,
}

fn entry_label(ctx: &Ctx, entry_idx: usize) -> Option<Word> {
    ctx.module.functions[entry_idx]
        .blocks
        .first()
        .and_then(|block| block.label.as_ref())
        .and_then(|label| label.result_id)
}

fn strided_fill_of(
    ctx: &mut Ctx,
    var: Word,
    pointee: Word,
    null_by_pointee: &mut HashMap<Word, Word>,
) -> Option<StridedFill> {
    let array = type_def_of(ctx, pointee)?;
    if array.class.opcode != Op::TypeArray {
        return None;
    }
    let (Some(&Operand::IdRef(elem)), Some(&Operand::IdRef(length_id))) =
        (array.operands.first(), array.operands.get(1))
    else {
        return None;
    };
    let length = literal_u32_constant(ctx, length_id)?;
    if length < 2 {
        return None;
    }
    let mut chunk = match scalar_or_vector_bytes(ctx, elem) {
        Some(bytes) if bytes > 0 && 16 % bytes == 0 => 16 / bytes,
        _ => 1,
    };
    while length % chunk != 0 {
        chunk /= 2;
    }
    let elem_ptr = ctx.ty_ptr(StorageClass::Workgroup, elem);
    let elem_null = *null_by_pointee.entry(elem).or_insert_with(|| {
        let id = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::ConstantNull,
            Some(elem),
            Some(id),
            vec![],
        ));
        id
    });
    Some(StridedFill {
        var,
        elem_ptr,
        elem_null,
        chunks: length / chunk,
        chunk,
    })
}

fn literal_u32_constant(ctx: &Ctx, id: Word) -> Option<u32> {
    let def = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .find(|inst| inst.result_id == Some(id))?;
    if def.class.opcode != Op::Constant {
        return None;
    }
    let ty = type_def_of(ctx, def.result_type?)?;
    if ty.class.opcode != Op::TypeInt || ty.operands.first() != Some(&Operand::LiteralBit32(32)) {
        return None;
    }
    match def.operands.first() {
        Some(&Operand::LiteralBit32(value)) => Some(value),
        _ => None,
    }
}

fn scalar_or_vector_bytes(ctx: &Ctx, ty: Word) -> Option<u32> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeInt | Op::TypeFloat => match def.operands.first() {
            Some(&Operand::LiteralBit32(width)) if width % 8 == 0 => Some(width / 8),
            _ => None,
        },
        Op::TypeVector => {
            let (Some(&Operand::IdRef(component)), Some(&Operand::LiteralBit32(count))) =
                (def.operands.first(), def.operands.get(1))
            else {
                return None;
            };
            let component_def = type_def_of(ctx, component)?;
            if !matches!(component_def.class.opcode, Op::TypeInt | Op::TypeFloat) {
                return None;
            }
            Some(scalar_or_vector_bytes(ctx, component)? * count)
        }
        _ => None,
    }
}

fn emit_strided_fills(
    ctx: &mut Ctx,
    entry_idx: usize,
    whole_stores: Vec<Instruction>,
    fills: Vec<StridedFill>,
    barrier: Instruction,
) {
    let Some(entry) = entry_label(ctx, entry_idx) else {
        return;
    };
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let v3uint = ctx.ty_vec_uint(3);
    let local_id_var = crate::passes::air_calls::existing_builtin_input_var(
        ctx,
        spirv::BuiltIn::LocalInvocationId,
        v3uint,
    )
    .unwrap_or_else(|| {
        crate::passes::stage_input::bind_kernel_v3uint_builtin(
            ctx,
            spirv::BuiltIn::LocalInvocationId,
        )
    });
    let [size_x, size_y, size_z] = ctx.kernel_local_size_ids();
    let negative = std::env::var_os("NVMTL_WG_ZERO_NEG").is_some();

    let binary = |op: Op, result: Word, a: Word, b: Word| {
        Instruction::new(
            op,
            Some(uint),
            Some(result),
            vec![Operand::IdRef(a), Operand::IdRef(b)],
        )
    };
    let branch =
        |target: Word| Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(target)]);
    let label = |id: Word| Some(Instruction::new(Op::Label, None, Some(id), vec![]));

    let [local_id, x, y, z, row, plane_size, plane, lane_xy, lane, wg] =
        std::array::from_fn(|_| ctx.module.fresh_id());
    let mut entry_tail = whole_stores;
    entry_tail.push(Instruction::new(
        Op::Load,
        Some(v3uint),
        Some(local_id),
        vec![Operand::IdRef(local_id_var)],
    ));
    for (component, id) in [x, y, z].into_iter().enumerate() {
        entry_tail.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(id),
            vec![
                Operand::IdRef(local_id),
                Operand::LiteralBit32(component as u32),
            ],
        ));
    }
    entry_tail.push(binary(Op::IMul, row, y, size_x));
    entry_tail.push(binary(Op::IMul, plane_size, size_x, size_y));
    entry_tail.push(binary(Op::IMul, plane, z, plane_size));
    entry_tail.push(binary(Op::IAdd, lane_xy, x, row));
    entry_tail.push(binary(Op::IAdd, lane, lane_xy, plane));
    entry_tail.push(binary(Op::IMul, wg, plane_size, size_z));
    let stride = if negative {
        let doubled = ctx.module.fresh_id();
        let two = ctx.const_uint(2);
        entry_tail.push(binary(Op::IMul, doubled, wg, two));
        doubled
    } else {
        wg
    };

    let mut blocks: Vec<Block> = Vec::new();
    let mut pred = entry;
    for fill in &fills {
        let [header, body, cont, merge, chunk_index, more, next] =
            std::array::from_fn(|_| ctx.module.fresh_id());
        let chunks = ctx.const_uint(fill.chunks);
        let into_header = branch(header);
        match blocks.last_mut() {
            Some(previous_merge) => previous_merge.instructions.push(into_header),
            None => entry_tail.push(into_header),
        }
        blocks.push(Block {
            label: label(header),
            instructions: vec![
                Instruction::new(
                    Op::Phi,
                    Some(uint),
                    Some(chunk_index),
                    vec![
                        Operand::IdRef(lane),
                        Operand::IdRef(pred),
                        Operand::IdRef(next),
                        Operand::IdRef(cont),
                    ],
                ),
                Instruction::new(
                    Op::ULessThan,
                    Some(bool_ty),
                    Some(more),
                    vec![Operand::IdRef(chunk_index), Operand::IdRef(chunks)],
                ),
                Instruction::new(
                    Op::LoopMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(merge),
                        Operand::IdRef(cont),
                        Operand::LoopControl(spirv::LoopControl::NONE),
                    ],
                ),
                Instruction::new(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![
                        Operand::IdRef(more),
                        Operand::IdRef(body),
                        Operand::IdRef(merge),
                    ],
                ),
            ],
        });
        let mut stores = Vec::new();
        let first = if fill.chunk == 1 {
            chunk_index
        } else {
            let first = ctx.module.fresh_id();
            let chunk = ctx.const_uint(fill.chunk);
            stores.push(binary(Op::IMul, first, chunk_index, chunk));
            first
        };
        for element in 0..fill.chunk {
            let index = if element == 0 {
                first
            } else {
                let index = ctx.module.fresh_id();
                let offset = ctx.const_uint(element);
                stores.push(binary(Op::IAdd, index, first, offset));
                index
            };
            let pointer = ctx.module.fresh_id();
            stores.push(Instruction::new(
                Op::AccessChain,
                Some(fill.elem_ptr),
                Some(pointer),
                vec![Operand::IdRef(fill.var), Operand::IdRef(index)],
            ));
            stores.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(pointer), Operand::IdRef(fill.elem_null)],
            ));
        }
        stores.push(branch(cont));
        blocks.push(Block {
            label: label(body),
            instructions: stores,
        });
        blocks.push(Block {
            label: label(cont),
            instructions: vec![binary(Op::IAdd, next, chunk_index, stride), branch(header)],
        });
        blocks.push(Block {
            label: label(merge),
            instructions: Vec::new(),
        });
        pred = merge;
    }

    let function = &mut ctx.module.functions[entry_idx];
    for block in function.blocks.iter_mut().skip(1) {
        for inst in block.instructions.iter_mut() {
            if inst.class.opcode != Op::Phi {
                continue;
            }
            for parent in inst.operands.iter_mut().skip(1).step_by(2) {
                if *parent == Operand::IdRef(entry) {
                    *parent = Operand::IdRef(pred);
                }
            }
        }
    }
    let entry_block = &mut function.blocks[0];
    let insert_at = entry_block
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(entry_block.instructions.len());
    let original_tail = entry_block.instructions.split_off(insert_at);
    entry_block.instructions.extend(entry_tail);
    let last_merge = blocks.last_mut().expect("at least one strided fill");
    last_merge.instructions.push(barrier);
    last_merge.instructions.extend(original_tail);
    function.blocks.splice(1..1, blocks);
}

fn shader_initialized_atomic_workgroup_vars(
    ctx: &Ctx,
    entry_idx: usize,
    vars: &[(Word, Word)],
) -> HashSet<Word> {
    let workgroup_vars: HashSet<Word> = vars.iter().map(|(var, _)| *var).collect();
    let pointer_sources = pointer_source_map(ctx, entry_idx);
    let mut prebarrier_stores = HashSet::default();
    let mut atomic_uses = HashSet::default();
    let mut before_first_barrier = true;

    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if inst.class.opcode == Op::ControlBarrier {
                before_first_barrier = false;
            }
            if before_first_barrier && inst.class.opcode == Op::Store {
                if let Some(root) =
                    id_ref_at(inst, 0).and_then(|ptr| pointer_root(ptr, &pointer_sources))
                {
                    if workgroup_vars.contains(&root) {
                        prebarrier_stores.insert(root);
                    }
                }
            }
            if is_atomic_op(inst.class.opcode) {
                if let Some(root) =
                    id_ref_at(inst, 0).and_then(|ptr| pointer_root(ptr, &pointer_sources))
                {
                    if workgroup_vars.contains(&root) {
                        atomic_uses.insert(root);
                    }
                }
            }
        }
    }

    prebarrier_stores
        .intersection(&atomic_uses)
        .copied()
        .collect()
}

fn pointer_source_map(ctx: &Ctx, entry_idx: usize) -> HashMap<Word, Word> {
    let mut sources = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if matches!(
                inst.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::Bitcast
                    | Op::CopyObject
            ) {
                if let (Some(result), Some(source)) = (inst.result_id, id_ref_at(inst, 0)) {
                    sources.insert(result, source);
                }
            }
        }
    }
    sources
}

fn pointer_root(ptr: Word, sources: &HashMap<Word, Word>) -> Option<Word> {
    let mut cur = ptr;
    let mut seen = HashSet::default();
    while seen.insert(cur) {
        match sources.get(&cur).copied() {
            Some(next) => cur = next,
            None => return Some(cur),
        }
    }
    None
}

use super::atomic_loop::id_ref_at;

fn is_atomic_op(op: Op) -> bool {
    matches!(
        op,
        Op::AtomicLoad
            | Op::AtomicStore
            | Op::AtomicExchange
            | Op::AtomicCompareExchange
            | Op::AtomicCompareExchangeWeak
            | Op::AtomicIIncrement
            | Op::AtomicIDecrement
            | Op::AtomicIAdd
            | Op::AtomicISub
            | Op::AtomicSMin
            | Op::AtomicUMin
            | Op::AtomicSMax
            | Op::AtomicUMax
            | Op::AtomicAnd
            | Op::AtomicOr
            | Op::AtomicXor
    )
}
