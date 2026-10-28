//! Materializes the element loads of an `array<sampler, N>` entry parameter.
//!
//! Apple's AIR passes `array<sampler, N>` byval, as `[N x ptr addrspace(2)]`, and the shader
//! selects one element with a constant index before handing it to `air.sample_*`:
//!
//! ```text
//!   %slot = getelementptr inbounds [2 x ptr addrspace(2)], ptr %samps, i32 0, i32 1
//!   %s = load ptr addrspace(2), ptr %slot
//! ```
//!
//! The descriptor ABI for that parameter is a single Vulkan descriptor array of `OpTypeSampler`,
//! because every element is its own binding. The emitter does not model a loaded sampler-state
//! pointer, so it hands the value over as an opaque placeholder and records what it was: a
//! `LocalPointerFieldLoad` naming the array root and the constant element index.
//!
//! This pass turns each of those facts into the load Vulkan wants: an `OpAccessChain` into the
//! descriptor array followed by an `OpLoad` of `OpTypeSampler`. The placeholder keeps its result
//! id, so every existing use stays valid and the sampling intrinsic sees a sampler value.
//!
//! Only a constant element index is lowered. Metal emits one for every sampler array it can
//! express (WebGL2 forbids anything else: "array index for samplers must be constant integral
//! expressions"), and a dynamic index would need a bounds-checked path this pass does not have.

use crate::spirv_module::Instruction;
use crate::spirv_module::Operand;
use spirv::{Op, StorageClass, Word};

use crate::fxhash::HashMap;

use super::retire_dead_pointer_projections;
use super::super::Ctx;

pub(crate) fn materialize_sampler_array_loads(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.sampler_array_vars.is_empty() {
        return;
    }

    let mut selected: Vec<(Word, Word, u32)> = Vec::new();
    for fact in &ctx.emit_sidecar.local_pointer_field_loads {
        if !ctx.sampler_array_vars.contains_key(&fact.root) {
            continue;
        }
        let Some(element) = fixed_array_element(&fact.indices) else {
            continue;
        };
        selected.push((fact.id, fact.root, element));
    }
    if selected.is_empty() {
        return;
    }

    let sty = ctx.ty_sampler();
    let ptr_sty = ctx.ty_ptr(StorageClass::UniformConstant, sty);
    let mut rewrites: HashMap<Word, (Instruction, Word)> = HashMap::default();
    for (id, arrayvar, element) in selected {
        let idx = ctx.const_uint(element);
        let p = ctx.module.fresh_id();
        rewrites.insert(
            id,
            (
                Instruction::new(
                    Op::AccessChain,
                    Some(ptr_sty),
                    Some(p),
                    vec![Operand::IdRef(arrayvar), Operand::IdRef(idx)],
                ),
                p,
            ),
        );
    }
    let consumed: Vec<Word> = rewrites.keys().copied().collect();

    let func = &mut ctx.module.functions[entry_idx];
    for blk in func.blocks.iter_mut() {
        let mut rebuilt: Vec<Instruction> =
            Vec::with_capacity(blk.instructions.len() + rewrites.len());
        for inst in blk.instructions.drain(..) {
            if let Some(id) = inst.result_id {
                if let Some((chain, p)) = rewrites.remove(&id) {
                    rebuilt.push(chain);
                    let mut load = inst;
                    load.class.opcode = Op::Load;
                    load.result_type = Some(sty);
                    load.operands = vec![Operand::IdRef(p)];
                    rebuilt.push(load);
                    continue;
                }
            }
            rebuilt.push(inst);
        }
        blk.instructions = rebuilt;
    }

    ctx.emit_sidecar
        .local_pointer_field_loads
        .retain(|fact| !consumed.contains(&fact.id));

    let roots = ctx.sampler_array_vars.keys().copied().collect::<Vec<_>>();
    retire_dead_pointer_projections(ctx, entry_idx, roots);
}

/// The element a byval `[N x ptr addrspace(2)]` projection selects, as the emitter reduces it.
fn fixed_array_element(indices: &[u32]) -> Option<u32> {
    match indices {
        [element] | [element, 0] => Some(*element),
        _ => None,
    }
}
