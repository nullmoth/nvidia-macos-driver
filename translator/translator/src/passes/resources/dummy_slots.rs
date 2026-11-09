//! A null resource id reads and writes a dummy of the image type the shader declares.
//!
//! A texture handle an argument buffer holds is a gpuResourceID whose low 17 bits the shader masks to a bindless heap
//! slot (`BINDLESS_HEAP_SLOTS`). A handle of zero, which Metal apps write for "no texture", lands on slot 0. One slot
//! holds one descriptor per binding, so slot 0 can match only one image dimensionality, while the kernels of a game
//! declare 2D, 2D array, 3D and cube-array images: an image store whose declared dimensionality differs from the
//! descriptor's header is undefined on NVIDIA and walked off the dummy into unmapped memory. When the plugin reserves
//! slots 1..5 for typed dummies (`BINDLESS_DUMMY_SLOT_*`, `TransformOptions::typed_dummy_slots`), every heap access
//! chain's index becomes `index == 0 ? dummy slot of the declared type : index`: a null handle then reads zeros and
//! discards writes for every type, which is what Apple's null resource does.
use super::super::Ctx;
use crate::spirv_module::{Instruction, Operand};
use spirv::{Dim, Op, Word};

/// The reserved slot whose dummy has the declared dimensionality. A 1D image reads the 2D dummy (no 1D dummy; the
/// games measured declare none). Texel buffers keep slot 0: their bindings hold buffer views, which have no dimension.
pub(in crate::passes) fn dummy_slot_for(dims: (Dim, bool)) -> Option<u32> {
    use crate::reflect::*;
    Some(match dims {
        (Dim::Dim2D, false) | (Dim::Dim1D, _) => BINDLESS_DUMMY_SLOT_2D,
        (Dim::Dim2D, true) => BINDLESS_DUMMY_SLOT_2D_ARRAY,
        (Dim::Dim3D, _) => BINDLESS_DUMMY_SLOT_3D,
        (Dim::DimCube, false) => BINDLESS_DUMMY_SLOT_CUBE,
        (Dim::DimCube, true) => BINDLESS_DUMMY_SLOT_CUBE_ARRAY,
        _ => return None,
    })
}

/// Runs after every pass that builds heap access chains and before the module's globals are closed.
pub(in crate::passes) fn redirect_zero_heap_slots(ctx: &mut Ctx) {
    if !ctx.typed_dummy_slots || ctx.bindless_heap_vars.is_empty() {
        return;
    }
    let heap_vars: Vec<Word> = ctx.bindless_heap_vars.values().copied().collect();
    let mut targets: Vec<(Word, u32)> = Vec::new();
    for var in heap_vars {
        let Some(&(_, dims, _, _)) = ctx.image_array_vars.get(&var) else {
            continue;
        };
        if let Some(slot) = dummy_slot_for(dims) {
            targets.push((var, slot));
        }
    }
    if targets.is_empty() {
        return;
    }
    let uint_ty = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let zero = ctx.const_uint(0);
    let slot_consts: Vec<(Word, Word)> = targets
        .iter()
        .map(|&(var, slot)| (var, ctx.const_uint(slot)))
        .collect();
    let mut redirected = 0usize;
    for function_idx in 0..ctx.module.functions.len() {
        let n_blocks = ctx.module.functions[function_idx].blocks.len();
        for block_idx in 0..n_blocks {
            let insts = ctx.module.functions[function_idx].blocks[block_idx]
                .instructions
                .clone();
            let mut out = Vec::with_capacity(insts.len() + 2);
            for mut inst in insts {
                if matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                    if let [Operand::IdRef(var), Operand::IdRef(index)] = inst.operands.as_slice() {
                        if let Some(&(_, dummy)) = slot_consts.iter().find(|(v, _)| v == var) {
                            let index = *index;
                            let is_zero = ctx.module.fresh_id();
                            out.push(Instruction::new(
                                Op::IEqual,
                                Some(bool_ty),
                                Some(is_zero),
                                vec![Operand::IdRef(index), Operand::IdRef(zero)],
                            ));
                            let safe = ctx.module.fresh_id();
                            out.push(Instruction::new(
                                Op::Select,
                                Some(uint_ty),
                                Some(safe),
                                vec![
                                    Operand::IdRef(is_zero),
                                    Operand::IdRef(dummy),
                                    Operand::IdRef(index),
                                ],
                            ));
                            inst.operands[1] = Operand::IdRef(safe);
                            redirected += 1;
                        }
                    }
                }
                out.push(inst);
            }
            ctx.module.functions[function_idx].blocks[block_idx].instructions = out;
        }
    }
    let _ = redirected;
}
