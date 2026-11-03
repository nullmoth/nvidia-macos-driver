use super::stage_input::kernel_grid::load_kernel_dispatch_component;
use super::*;

// Metal's dispatchThreads:threadsPerThreadgroup: dispatches an EXACT number of threads, which need not be a
// multiple of the threadgroup size, and the threads past that count are never created. Vulkan has no such
// call: vkCmdDispatch runs whole workgroups, so the final workgroup of a non-multiple dispatch carries lanes
// that Metal would not have. Nothing used to discard them. Those lanes ran the kernel with a grid position
// past the end of the grid, and every address derived from that position was out of range.
//
// Cyberpunk's light-cluster kernel is where this first became visible: its tile index is the grid position
// shifted right, so the tail lanes indexed past the tile array and faulted the channel. It was worked around
// for a while by substituting a hand-patched copy of that one kernel, keyed by the hash of its translated
// SPIR-V - which fixed one shader on one machine and silently stopped applying whenever the translator
// changed. This is the general fix, and it needs no substitution.
//
// The guard goes in the ENTRY block, before any other instruction of the body. That position is not an
// accident and it is the whole correctness argument:
//
//   A subgroup collective sees the set of lanes that are active where it executes.
//   OpGroupNonUniformBallot(%true) IS that set, and OpGroupNonUniformShuffle reads a named lane from it.
//   Retiring the tail lanes BEFORE any collective gives every collective exactly the lane set Metal gives
//   it, because in Metal those lanes do not exist at all. Retiring them part-way through the body does not:
//   the collectives before the guard would see the full set and the ones after would see the reduced set,
//   and the in-range lanes then compute from a mask that matches neither driver. A guard inserted deep in
//   that same Cyberpunk kernel - 5 of its 13 collectives above the guard and 8 below - produced precisely
//   that: correct colours with scattered lights missing.
//
// Clamping the position instead of returning was considered and rejected. It keeps every lane alive, so the
// collectives stay identical, but a clamped tail lane then does the work of the last in-range thread: it
// reads the same inputs and writes the same outputs. That is harmless for a kernel that only reads, and
// wrong for any kernel that accumulates, uses an atomic, or writes once per thread, because the last
// element would take the tail's contribution as well. Metal's own behaviour is that the lane is absent, so
// absence is what this emits.
//
// When the guard cannot fire it still costs the comparison, and that is deliberate: dispatchThreadgroups:
// passes a thread count that is the group count times the group size, so the condition is false for every
// lane and the shape of the code stays the same either way. An indirect dispatch reads its count back from
// NumWorkgroups times the group size through the same helper, which is likewise an exact multiple.
//
// Limitation worth stating rather than hiding: this guards the kernels that read thread_position_in_grid,
// which is how the dispatch region gets bound in the first place. A kernel that reads only its
// threadgroup-relative position has no region to compare against here and is left alone; Metal gives such a
// kernel a varying threads_per_threadgroup for the edge groups, which is a separate mechanism.
pub(in crate::passes) fn emit_dispatch_tail_guard(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.stage != Stage::Kernel {
        return;
    }
    if matches!(
        ctx.kernel_dispatch,
        crate::reflect::KernelDispatch::Workgroups
    ) {
        return;
    }
    // Only a kernel that already reads the grid position has a region bound, and only that kernel derives
    // addresses from a position this guard can compare. Finding the variable rather than creating one keeps
    // this pass from inventing a push constant the pipeline layout does not describe.
    let Some(region) = existing_dispatch_region_var(ctx) else {
        return;
    };
    let Some(gid) = existing_global_invocation_id_var(ctx) else {
        return;
    };
    // A kernel that executes a workgroup barrier may NOT have lanes retired ahead of it. Vulkan requires
    // OpControlBarrier to be reached by every invocation in the workgroup, and a fixed-size Vulkan
    // workgroup still contains the tail lanes even when Metal's dispatch would not have created them: if
    // this guard returns first, the survivors wait at a barrier the retired lanes never reach, which is
    // undefined and faults the channel. Measured, not reasoned: 1,024 of the 2,544 kernels this guard
    // reached in Cyberpunk's shader cache contain OpControlBarrier, and the first game run on it died with
    // "NVRM channel faulted / FAULT_PDE compute shader" after translating cleanly.
    // Metal gets away with the same source because on that hardware the tail threads genuinely do not
    // exist, so its barrier is consistent with a smaller workgroup. Ours cannot be.
    if function_has_workgroup_barrier(ctx, entry_idx) {
        return;
    }

    let uint_ty = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let v3uint_ty = ctx.ty_vec_uint(3);

    let mut guard: Vec<Instruction> = Vec::new();
    let builtin = ctx.module.fresh_id();
    guard.push(Instruction::new(
        Op::Load,
        Some(v3uint_ty),
        Some(builtin),
        vec![Operand::IdRef(gid)],
    ));
    let mut out_of_range: Option<Word> = None;
    for lane in 0..3u32 {
        let relative = ctx.module.fresh_id();
        guard.push(Instruction::new(
            Op::CompositeExtract,
            Some(uint_ty),
            Some(relative),
            vec![Operand::IdRef(builtin), Operand::LiteralBit32(lane)],
        ));
        // members 3..5 are the thread base of this dispatch chunk: a large grid is dispatched in pieces and
        // the absolute position is the chunk's base plus the position inside it, so the comparison has to be
        // against the absolute position or the last chunk would discard legitimate lanes.
        let base = load_kernel_dispatch_component(ctx, &mut guard, region, 3 + lane);
        let absolute = ctx.module.fresh_id();
        guard.push(Instruction::new(
            Op::IAdd,
            Some(uint_ty),
            Some(absolute),
            vec![Operand::IdRef(relative), Operand::IdRef(base)],
        ));
        // members 0..2 are threads_per_grid, read RAW rather than through load_kernel_dispatch_component.
        // That helper resolves an indirect dispatch's count from NumWorkgroups, which would make this guard
        // reference an Input builtin that the entry point's interface was already finalised without - the
        // module then fails validation with "OpEntryPoint omits a global used by its static call tree".
        // Reading raw is also the behaviour this guard wants: for an indirect dispatch the driver pushes
        // 0x80000000 | the threadgroup size, so the comparison against it is false for every lane and the
        // guard stays out of the way. That is right, because an indirect dispatch runs whole groups and has
        // no tail to discard in the first place.
        let threads = load_region_member_raw(ctx, &mut guard, region, lane);
        let past = ctx.module.fresh_id();
        guard.push(Instruction::new(
            Op::UGreaterThanEqual,
            Some(bool_ty),
            Some(past),
            vec![Operand::IdRef(absolute), Operand::IdRef(threads)],
        ));
        out_of_range = Some(match out_of_range {
            None => past,
            Some(prev) => {
                let any = ctx.module.fresh_id();
                guard.push(Instruction::new(
                    Op::LogicalOr,
                    Some(bool_ty),
                    Some(any),
                    vec![Operand::IdRef(prev), Operand::IdRef(past)],
                ));
                any
            }
        });
    }
    let Some(out_of_range) = out_of_range else {
        return;
    };

    let return_label = ctx.module.fresh_id();
    let merge_label = ctx.module.fresh_id();
    guard.push(Instruction::new(
        Op::SelectionMerge,
        None,
        None,
        vec![
            Operand::IdRef(merge_label),
            Operand::SelectionControl(spirv::SelectionControl::NONE),
        ],
    ));
    guard.push(Instruction::new(
        Op::BranchConditional,
        None,
        None,
        vec![
            Operand::IdRef(out_of_range),
            Operand::IdRef(return_label),
            Operand::IdRef(merge_label),
        ],
    ));

    // Never index the function list directly: a caller's index can be stale, and a kernel whose body was
    // reduced to nothing has no blocks to split. Either case means there is no tail to guard here.
    let Some(function) = ctx.module.functions.get_mut(entry_idx) else {
        return;
    };
    if function.blocks.is_empty() {
        return;
    }
    let entry_label = function.blocks[0]
        .label
        .as_ref()
        .and_then(|inst| inst.result_id);
    let entry = &mut function.blocks[0];
    // OpVariable must stay at the top of a function's first block, so the guard goes after the last of them
    // and everything below it becomes the merge block. Splitting here rather than inserting a block before
    // the entry keeps the function's first label, which the entry point names.
    let after_variables = entry
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(entry.instructions.len());
    let tail = entry.instructions.split_off(after_variables);
    entry.instructions.extend(guard);

    let merge_block = Block {
        label: Some(Instruction::new(
            Op::Label,
            None,
            Some(merge_label),
            vec![],
        )),
        instructions: tail,
    };
    let return_block = Block {
        label: Some(Instruction::new(
            Op::Label,
            None,
            Some(return_label),
            vec![],
        )),
        instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
    };
    // The merge block has to be the one that follows the entry, because the rest of the body branches into
    // the labels that were already inside it.
    function.blocks.insert(1, merge_block);
    function.blocks.insert(2, return_block);

    // The part that is easy to miss and breaks rendering silently: splitting the entry moved its
    // terminator into the merge block, so every edge that used to leave the entry block now leaves the
    // merge block. An OpPhi naming the entry's label as the parent of one of its values is therefore
    // naming a block that no longer branches to it, and the value is lost - four buffers tests lost exactly
    // the pointer and index phis they assert on, which in a real app is a pointer read from the wrong arm.
    // Every such parent is renamed to the merge block. Only the entry label needs this: no other block's
    // edges changed.
    if let Some(entry_label) = entry_label {
        for block in function.blocks.iter_mut() {
            for inst in block.instructions.iter_mut() {
                if inst.class.opcode != Op::Phi {
                    continue;
                }
                // OpPhi operands are (value, parent) pairs, so the parents are at the odd indices
                for index in (1..inst.operands.len()).step_by(2) {
                    if inst.operands[index] == Operand::IdRef(entry_label) {
                        inst.operands[index] = Operand::IdRef(merge_label);
                    }
                }
            }
        }
    }
}

// A plain read of one member of the dispatch region. load_kernel_dispatch_component is the right call for
// the thread base (members 3..5), which it returns untouched, but not for the thread counts - see the note
// at its use above.
fn load_region_member_raw(
    ctx: &mut Ctx,
    instructions: &mut Vec<Instruction>,
    region: Word,
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
        vec![Operand::IdRef(region), Operand::IdRef(member_index)],
    ));
    let value = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Load,
        Some(uint_ty),
        Some(value),
        vec![Operand::IdRef(pointer)],
    ));
    value
}

// True when the entry point, or anything it calls, executes a barrier that synchronises the workgroup.
// The whole static call tree is walked rather than the entry function alone: a barrier inside a helper is
// just as binding, and a check that looked only at the top level would pass the kernels it must refuse.
// OpControlBarrier is the operation that waits; OpMemoryBarrier only orders memory and retires nobody, so
// it does not constrain this guard.
fn function_has_workgroup_barrier(ctx: &Ctx, entry_idx: usize) -> bool {
    let entry_id = match ctx.module.functions.get(entry_idx).and_then(|f| f.def.as_ref()).and_then(|d| d.result_id) {
        Some(id) => id,
        // The entry cannot be identified, so whether it barriers is UNKNOWN - and unknown is not "no".
        None => return true,
    };
    let mut pending = vec![entry_id];
    let mut seen = crate::fxhash::HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(function) = ctx
            .module
            .functions
            .iter()
            .find(|f| f.def.as_ref().and_then(|d| d.result_id) == Some(id))
        else {
            // A call we cannot resolve might barrier; refuse rather than assume it does not.
            return true;
        };
        for block in &function.blocks {
            for inst in &block.instructions {
                if inst.class.opcode == Op::ControlBarrier {
                    return true;
                }
                if inst.class.opcode == Op::FunctionCall {
                    if let Some(Operand::IdRef(callee)) = inst.operands.first() {
                        pending.push(*callee);
                    }
                }
            }
        }
    }
    false
}

fn existing_dispatch_region_var(ctx: &Ctx) -> Option<Word> {
    ctx.module.debug_names.iter().find_map(|inst| {
        (inst.class.opcode == Op::Name
            && matches!(
                inst.operands.get(1),
                Some(Operand::LiteralString(name)) if name == "metal2vulkan_dispatch_region"
            ))
        .then(|| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .flatten()
    })
}

fn existing_global_invocation_id_var(ctx: &Ctx) -> Option<Word> {
    ctx.module.annotations.iter().find_map(|inst| {
        (inst.class.opcode == Op::Decorate
            && inst.operands.get(1) == Some(&Operand::Decoration(spirv::Decoration::BuiltIn))
            && inst.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::GlobalInvocationId)))
        .then(|| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .flatten()
    })
}
