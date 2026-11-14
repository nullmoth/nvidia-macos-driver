use super::block_split::{labelled_block, CallSiteSplit};
use super::*;

// Threadgroup memory of a tile function is memory of the tile, not of one dispatch: what a dispatch leaves in it the next
// dispatch of the same render pass finds there, with draws in between. A driver that runs a tile function as a compute
// dispatch keeps that memory in a storage buffer (`tile_mode::TILE_MEMORY_BUFFER_SLOT`, one slice of
// `[[threadgroup]]` bytes per tile, the tile addressed by the dispatch's tile block) and the translated function keeps
// working in workgroup memory, which is fast: it loads each `[[threadgroup(n)]]` argument from its slice when it starts
// and stores it back when it ends.
//
// The copy is by 32-bit words, leaf by leaf, in a fixed order, so what one dispatch stores another dispatch of the same
// element type loads back whatever the element type is. A leaf narrower than 32 bits (half, short, char) takes a word of
// its own, zero-extended; a 64-bit leaf is refused.

/// A threadgroup argument of a tile function, noted when its workgroup variable is made.
#[derive(Clone, Debug)]
pub(in crate::passes) struct TileTgVar {
    pub var: Word,
    pub metal_index: u32,
}

/// Longest run of 32-bit words one element of a threadgroup array may be.
const MAX_LEAVES: usize = 256;

struct Leaf {
    path: Vec<u32>,
    ty: Word,
    bits: u32,
    float: bool,
}

fn collect_leaves(
    ctx: &mut Ctx,
    ty: Word,
    path: &mut Vec<u32>,
    out: &mut Vec<Leaf>,
) -> Result<(), String> {
    let def = type_def_of(ctx, ty)
        .ok_or_else(|| "a tile threadgroup array has an undefined element type".to_string())?;
    match def.class.opcode {
        Op::TypeInt | Op::TypeFloat => {
            let bits = match def.operands.first() {
                Some(Operand::LiteralBit32(bits)) if matches!(*bits, 8 | 16 | 32) => *bits,
                other => {
                    return Err(format!(
                        "tile threadgroup memory holds an element with a {other:?}-bit scalar; only 8, 16 and 32-bit scalars persist across dispatches"
                    ))
                }
            };
            out.push(Leaf { path: path.clone(), ty, bits, float: def.class.opcode == Op::TypeFloat });
        }
        Op::TypeVector => {
            let (Some(Operand::IdRef(elem)), Some(Operand::LiteralBit32(lanes))) =
                (def.operands.first(), def.operands.get(1))
            else {
                return Err("a tile threadgroup array has a malformed vector element".to_string());
            };
            for lane in 0..*lanes {
                path.push(lane);
                collect_leaves(ctx, *elem, path, out)?;
                path.pop();
            }
        }
        Op::TypeArray => {
            let (Some(Operand::IdRef(elem)), Some(Operand::IdRef(len))) =
                (def.operands.first(), def.operands.get(1))
            else {
                return Err("a tile threadgroup array has a malformed array element".to_string());
            };
            let len = match value_def_instruction(ctx, *len).and_then(|inst| inst.operands.first().cloned()) {
                Some(Operand::LiteralBit32(n)) => n,
                _ => return Err("a tile threadgroup array holds an array of unknown length".to_string()),
            };
            for index in 0..len {
                path.push(index);
                collect_leaves(ctx, *elem, path, out)?;
                path.pop();
                if out.len() > MAX_LEAVES {
                    break;
                }
            }
        }
        Op::TypeStruct => {
            let members = def
                .operands
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for (index, member) in members.into_iter().enumerate() {
                path.push(index as u32);
                collect_leaves(ctx, member, path, out)?;
                path.pop();
            }
        }
        other => {
            return Err(format!(
                "tile threadgroup memory holds an element of kind {other:?}, which does not persist across dispatches"
            ))
        }
    }
    if out.len() > MAX_LEAVES {
        return Err(format!(
            "a tile threadgroup array element is more than {MAX_LEAVES} words wide"
        ));
    }
    Ok(())
}

/// Where the tile's slice of tile memory starts, in 32-bit words: tile index times the slice length.
fn tile_word_base(ctx: &mut Ctx, out: &mut Vec<Instruction>) -> Result<Word, String> {
    let uint = ctx.ty_uint();
    let (tile_x, tile_y) = tile_origin::tile_coordinate(ctx, out)?;
    let across = tile_origin::load_tile_member(ctx, out, tile_origin::TILES_ACROSS_MEMBER)?;
    let words = tile_origin::load_tile_member(ctx, out, tile_origin::TILE_MEMORY_WORDS_MEMBER)?;
    let row = ctx.module.fresh_id();
    out.push(Instruction::new(Op::IMul, Some(uint), Some(row), vec![Operand::IdRef(tile_y), Operand::IdRef(across)]));
    let linear = ctx.module.fresh_id();
    out.push(Instruction::new(Op::IAdd, Some(uint), Some(linear), vec![Operand::IdRef(row), Operand::IdRef(tile_x)]));
    let base = ctx.module.fresh_id();
    out.push(Instruction::new(Op::IMul, Some(uint), Some(base), vec![Operand::IdRef(linear), Operand::IdRef(words)]));
    Ok(base)
}

/// This invocation's index in the workgroup and the workgroup's size, from the local invocation id and the local size.
fn thread_stride(ctx: &mut Ctx, out: &mut Vec<Instruction>) -> (Word, Word) {
    let uint = ctx.ty_uint();
    let v3uint = ctx.ty_vec_uint(3);
    let var = air_calls::existing_builtin_input_var(ctx, BuiltIn::LocalInvocationId, v3uint)
        .unwrap_or_else(|| stage_input::bind_kernel_v3uint_builtin(ctx, BuiltIn::LocalInvocationId));
    let id = ctx.module.fresh_id();
    out.push(Instruction::new(Op::Load, Some(v3uint), Some(id), vec![Operand::IdRef(var)]));
    let mut lane = [0; 3];
    for (axis, slot) in lane.iter_mut().enumerate() {
        *slot = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint),
            Some(*slot),
            vec![Operand::IdRef(id), Operand::LiteralBit32(axis as u32)],
        ));
    }
    let [sx, sy, sz] = ctx.kernel_local_size_ids();
    let mul = |ctx: &mut Ctx, out: &mut Vec<Instruction>, a: Word, b: Word| {
        let r = ctx.module.fresh_id();
        out.push(Instruction::new(Op::IMul, Some(uint), Some(r), vec![Operand::IdRef(a), Operand::IdRef(b)]));
        r
    };
    let add = |ctx: &mut Ctx, out: &mut Vec<Instruction>, a: Word, b: Word| {
        let r = ctx.module.fresh_id();
        out.push(Instruction::new(Op::IAdd, Some(uint), Some(r), vec![Operand::IdRef(a), Operand::IdRef(b)]));
        r
    };
    let z_rows = mul(ctx, out, lane[2], sy);
    let rows = add(ctx, out, z_rows, lane[1]);
    let row_words = mul(ctx, out, rows, sx);
    let index = add(ctx, out, row_words, lane[0]);
    let xy = mul(ctx, out, sx, sy);
    let size = mul(ctx, out, xy, sz);
    (index, size)
}

/// The storage buffer tile memory is read and written through, as a runtime array of words.
fn tile_memory_buffer(ctx: &mut Ctx) -> Result<(Word, Word), String> {
    let uint = ctx.ty_uint();
    let array = ctx.get_or_create(Op::TypeRuntimeArray, None, vec![Operand::IdRef(uint)]);
    // the same runtime array of words may already be a device buffer's type, and already carry its stride
    let strided = ctx.module.annotations.iter().any(|inst| {
        inst.class.opcode == Op::Decorate
            && inst.operands.first() == Some(&Operand::IdRef(array))
            && inst.operands.get(1) == Some(&Operand::Decoration(Decoration::ArrayStride))
    });
    if !strided {
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(array),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(4),
            ],
        ));
    }
    let block = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::TypeStruct,
        None,
        Some(block),
        vec![Operand::IdRef(array)],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![Operand::IdRef(block), Operand::Decoration(Decoration::Block)],
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
    let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, block);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(ptr_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::StorageBuffer)],
    ));
    let binding = ctx
        .descriptor_layout
        .buffer_binding(crate::tile_mode::TILE_MEMORY_BUFFER_SLOT)
        .ok_or("the tile memory buffer slot is outside the buffer band")?;
    decorate_binding(&mut ctx.module, var, ctx.descriptor_layout.set, binding);
    ctx.interface_buffer_var(var);
    Ok((var, ptr_ty))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    In,
    Out,
}

fn barrier(ctx: &mut Ctx) -> Instruction {
    let scope = ctx.const_uint(Scope::Workgroup as u32);
    let semantics = ctx
        .const_uint((MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::WORKGROUP_MEMORY).bits());
    Instruction::new(
        Op::ControlBarrier,
        None,
        None,
        vec![
            Operand::IdScope(scope),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
        ],
    )
}

struct Copy {
    var: Word,
    elements: u32,
    leaves: Vec<Leaf>,
    /// 32-bit words from the tile's slice start to this argument's first word.
    offset_words: u32,
}

/// One loop per argument, each invocation taking every `size`-th element. Returns the blocks and, for each, nothing to
/// merge: the last loop's merge block is `after`.
#[allow(clippy::too_many_arguments)]
fn copy_loops(
    ctx: &mut Ctx,
    copies: &[Copy],
    direction: Direction,
    entry_label: Word,
    after: Word,
    base: Word,
    index: Word,
    size: Word,
    buffer: Word,
) -> Result<(Word, Vec<crate::spirv_module::Block>), String> {
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let zero = ctx.const_uint(0);
    let sb_word_ptr = ctx.ty_ptr(StorageClass::StorageBuffer, uint);
    let mut blocks = Vec::new();
    // each loop's header, so that the previous loop's merge block can branch to it
    let headers = copies.iter().map(|_| ctx.module.fresh_id()).collect::<Vec<_>>();
    let merges = copies.iter().map(|_| ctx.module.fresh_id()).collect::<Vec<_>>();
    for (k, copy) in copies.iter().enumerate() {
        let header = headers[k];
        let body = ctx.module.fresh_id();
        let cont = ctx.module.fresh_id();
        let merge = merges[k];
        let previous = if k == 0 { entry_label } else { merges[k - 1] };
        let i = ctx.module.fresh_id();
        let next = ctx.module.fresh_id();
        let elements = ctx.const_uint(copy.elements);
        let width = copy.leaves.len() as u32;
        let width_const = ctx.const_uint(width);
        let offset_const = ctx.const_uint(copy.offset_words);
        let done = ctx.module.fresh_id();
        blocks.push(labelled_block(
            header,
            vec![
                Instruction::new(
                    Op::Phi,
                    Some(uint),
                    Some(i),
                    vec![
                        Operand::IdRef(index),
                        Operand::IdRef(previous),
                        Operand::IdRef(next),
                        Operand::IdRef(cont),
                    ],
                ),
                Instruction::new(
                    Op::ULessThan,
                    Some(bool_ty),
                    Some(done),
                    vec![Operand::IdRef(i), Operand::IdRef(elements)],
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
                    vec![Operand::IdRef(done), Operand::IdRef(body), Operand::IdRef(merge)],
                ),
            ],
        ));
        let mut insts = Vec::new();
        let row = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::IMul, Some(uint), Some(row), vec![Operand::IdRef(i), Operand::IdRef(width_const)]));
        let slice = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::IAdd, Some(uint), Some(slice), vec![Operand::IdRef(base), Operand::IdRef(offset_const)]));
        let first = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::IAdd, Some(uint), Some(first), vec![Operand::IdRef(slice), Operand::IdRef(row)]));
        for (j, leaf) in copy.leaves.iter().enumerate() {
            let word_index = if j == 0 {
                first
            } else {
                let c = ctx.const_uint(j as u32);
                let w = ctx.module.fresh_id();
                insts.push(Instruction::new(Op::IAdd, Some(uint), Some(w), vec![Operand::IdRef(first), Operand::IdRef(c)]));
                w
            };
            let word_ptr = ctx.module.fresh_id();
            insts.push(Instruction::new(
                Op::AccessChain,
                Some(sb_word_ptr),
                Some(word_ptr),
                vec![Operand::IdRef(buffer), Operand::IdRef(zero), Operand::IdRef(word_index)],
            ));
            let leaf_ptr_ty = ctx.ty_ptr(StorageClass::Workgroup, leaf.ty);
            let mut chain = vec![Operand::IdRef(copy.var), Operand::IdRef(i)];
            for step in &leaf.path {
                let c = ctx.const_uint(*step);
                chain.push(Operand::IdRef(c));
            }
            let leaf_ptr = ctx.module.fresh_id();
            insts.push(Instruction::new(Op::AccessChain, Some(leaf_ptr_ty), Some(leaf_ptr), chain));
            match direction {
                Direction::In => {
                    let word = ctx.module.fresh_id();
                    insts.push(Instruction::new(Op::Load, Some(uint), Some(word), vec![Operand::IdRef(word_ptr)]));
                    let value = word_to_leaf(ctx, &mut insts, word, leaf);
                    insts.push(Instruction::new(Op::Store, None, None, vec![Operand::IdRef(leaf_ptr), Operand::IdRef(value)]));
                }
                Direction::Out => {
                    let value = ctx.module.fresh_id();
                    insts.push(Instruction::new(Op::Load, Some(leaf.ty), Some(value), vec![Operand::IdRef(leaf_ptr)]));
                    let word = leaf_to_word(ctx, &mut insts, value, leaf);
                    insts.push(Instruction::new(Op::Store, None, None, vec![Operand::IdRef(word_ptr), Operand::IdRef(word)]));
                }
            }
        }
        insts.push(Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(cont)]));
        blocks.push(labelled_block(body, insts));
        blocks.push(labelled_block(
            cont,
            vec![
                Instruction::new(Op::IAdd, Some(uint), Some(next), vec![Operand::IdRef(i), Operand::IdRef(size)]),
                Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(header)]),
            ],
        ));
        // the merge block falls to the next loop's header, or out of the sequence
        let onward = if k + 1 < copies.len() { headers[k + 1] } else { after };
        blocks.push(labelled_block(
            merge,
            vec![Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(onward)])],
        ));
    }
    Ok((headers[0], blocks))
}

/// The 32-bit word that carries `value`, a loaded leaf.
fn leaf_to_word(ctx: &mut Ctx, insts: &mut Vec<Instruction>, value: Word, leaf: &Leaf) -> Word {
    let uint = ctx.ty_uint();
    if leaf.bits == 32 {
        if leaf.ty == uint {
            return value;
        }
        let word = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::Bitcast, Some(uint), Some(word), vec![Operand::IdRef(value)]));
        return word;
    }
    // narrow: the bits as an unsigned integer of the same width, then widened
    let narrow = ctx.get_or_create(Op::TypeInt, None, vec![Operand::LiteralBit32(leaf.bits), Operand::LiteralBit32(0)]);
    let bits = if leaf.ty == narrow {
        value
    } else {
        let b = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::Bitcast, Some(narrow), Some(b), vec![Operand::IdRef(value)]));
        b
    };
    let word = ctx.module.fresh_id();
    insts.push(Instruction::new(Op::UConvert, Some(uint), Some(word), vec![Operand::IdRef(bits)]));
    word
}

/// The leaf a stored word carries.
fn word_to_leaf(ctx: &mut Ctx, insts: &mut Vec<Instruction>, word: Word, leaf: &Leaf) -> Word {
    let uint = ctx.ty_uint();
    if leaf.bits == 32 {
        if leaf.ty == uint {
            return word;
        }
        let value = ctx.module.fresh_id();
        insts.push(Instruction::new(Op::Bitcast, Some(leaf.ty), Some(value), vec![Operand::IdRef(word)]));
        return value;
    }
    let narrow = ctx.get_or_create(Op::TypeInt, None, vec![Operand::LiteralBit32(leaf.bits), Operand::LiteralBit32(0)]);
    let bits = ctx.module.fresh_id();
    insts.push(Instruction::new(Op::UConvert, Some(narrow), Some(bits), vec![Operand::IdRef(word)]));
    if leaf.ty == narrow {
        return bits;
    }
    let value = ctx.module.fresh_id();
    insts.push(Instruction::new(Op::Bitcast, Some(leaf.ty), Some(value), vec![Operand::IdRef(bits)]));
    value
}

/// A leading `Nop` marks where to split; `CallSiteSplit` removes it.
fn nop() -> Instruction {
    Instruction::new(Op::Nop, None, None, vec![])
}

pub(in crate::passes) fn lower_tile_memory(ctx: &mut Ctx, entry_idx: usize) -> Result<(), String> {
    let vars = std::mem::take(&mut ctx.tile_tg_vars);
    if vars.is_empty() {
        return Ok(());
    }
    let mut copies = Vec::new();
    for noted in &vars {
        // the variable as it stands now: earlier passes may have retyped it
        let Some(def) = value_def_instruction(ctx, noted.var) else { continue };
        let Some(ptr_ty) = def.result_type else { continue };
        let Some(array_ty) = pointer_pointee_type(ctx, ptr_ty) else { continue };
        let Some(array) = type_def_of(ctx, array_ty) else { continue };
        if array.class.opcode != Op::TypeArray {
            return Err(format!(
                "tile threadgroup argument {} is not an array of elements in the translated module",
                noted.metal_index
            ));
        }
        let (Some(Operand::IdRef(elem)), Some(Operand::IdRef(len))) = (array.operands.first(), array.operands.get(1)) else {
            continue;
        };
        let elements = match value_def_instruction(ctx, *len).and_then(|inst| inst.operands.first().cloned()) {
            Some(Operand::LiteralBit32(n)) => n,
            _ => return Err("a tile threadgroup array has no constant length".to_string()),
        };
        if !ctx.module.functions[entry_idx].blocks.iter().any(|block| {
            block.instructions.iter().any(|inst| inst.operands.contains(&Operand::IdRef(noted.var)))
        }) {
            continue; // the function never touches it
        }
        let mut leaves = Vec::new();
        collect_leaves(ctx, *elem, &mut Vec::new(), &mut leaves)?;
        copies.push(Copy {
            var: noted.var,
            elements,
            leaves,
            offset_words: crate::tile_mode::tg_offset(noted.metal_index) / 4,
        });
    }
    if copies.is_empty() {
        return Ok(());
    }

    let function = &ctx.module.functions[entry_idx];
    let returns = function
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.instructions.last().is_some_and(|inst| inst.class.opcode == Op::Return))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let [return_block] = returns[..] else {
        return Err(format!(
            "a tile function that keeps threadgroup memory across dispatches needs one return every invocation reaches, and the entry has {}",
            returns.len()
        ));
    };

    let (buffer, _) = tile_memory_buffer(ctx)?;

    // copy-in: right after the entry block's variables
    {
        let after_variables = ctx.module.functions[entry_idx].blocks[0]
            .instructions
            .iter()
            .take_while(|inst| inst.class.opcode == Op::Variable)
            .count();
        let at = after_variables;
        ctx.module.functions[entry_idx].blocks[0].instructions.insert(at, nop());
        let mut split = CallSiteSplit::open(ctx, entry_idx, 0, at, "tile threadgroup memory load")?;
        let mut pre = Vec::new();
        let base = tile_word_base(ctx, &mut pre)?;
        let (index, size) = thread_stride(ctx, &mut pre);
        split.prefix.extend(pre);
        let entry_label = split.entry_label();
        let after = split.continuation(ctx);
        let (first, blocks) = copy_loops(ctx, &copies, Direction::In, entry_label, after, base, index, size, buffer)?;
        split.branch_prefix_to(first);
        let sync = barrier(ctx);
        split.suffix.insert(0, sync);
        split.finish(ctx, entry_idx, blocks);
    }

    // copy-out: before the one return
    {
        let ret = ctx
            .module
            .functions[entry_idx]
            .blocks
            .iter()
            .position(|block| block.instructions.last().is_some_and(|inst| inst.class.opcode == Op::Return))
            .unwrap_or(return_block);
        let at = ctx.module.functions[entry_idx].blocks[ret].instructions.len() - 1;
        ctx.module.functions[entry_idx].blocks[ret].instructions.insert(at, nop());
        let mut split = CallSiteSplit::open(ctx, entry_idx, ret, at, "tile threadgroup memory store")?;
        let mut pre = Vec::new();
        let base = tile_word_base(ctx, &mut pre)?;
        let (index, size) = thread_stride(ctx, &mut pre);
        let sync = barrier(ctx);
        split.prefix.push(sync);
        split.prefix.extend(pre);
        let entry_label = split.entry_label();
        let after = split.continuation(ctx);
        let (first, blocks) = copy_loops(ctx, &copies, Direction::Out, entry_label, after, base, index, size, buffer)?;
        split.branch_prefix_to(first);
        split.finish(ctx, entry_idx, blocks);
    }
    Ok(())
}
