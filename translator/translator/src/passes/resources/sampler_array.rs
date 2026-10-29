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
//! Every element is its own descriptor at its own sampler index, so the parameter is declared as
//! one `OpTypeSampler` variable per element at consecutive bindings, matching the plugin's layout.
//! The emitter does not model a loaded sampler-state pointer, so it hands the value over as an
//! opaque placeholder and records what it was: a `LocalPointerFieldLoad` naming the array root
//! (element 0's variable) and the constant element index.
//!
//! This pass turns each of those facts into the load Vulkan wants: an `OpLoad` of that element's
//! sampler variable. The placeholder keeps its result id, so every existing use stays valid and
//! the sampling intrinsic sees a sampler value.
//!
//! Only a constant element index is lowered. Metal emits one for every sampler array it can
//! express (WebGL2 forbids anything else: "array index for samplers must be constant integral
//! expressions"), and a dynamic index would need a bounds-checked path this pass does not have.

use crate::spirv_module::Instruction;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

use crate::fxhash::HashMap;

use super::retire_dead_pointer_projections;
use super::super::Ctx;

pub(crate) fn materialize_sampler_array_loads(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.sampler_array_vars.is_empty() {
        return;
    }

    // element load -> the sampler variable of that element; an index past the declared length is
    // left alone, so the placeholder fails the module loudly instead of sampling another binding
    let mut rewrites: HashMap<Word, Word> = HashMap::default();
    for fact in &ctx.emit_sidecar.local_pointer_field_loads {
        let Some(elements) = ctx.sampler_array_vars.get(&fact.root) else {
            continue;
        };
        let Some(element) = fixed_array_element(&fact.indices) else {
            continue;
        };
        if let Some(var) = elements.get(element as usize) {
            rewrites.insert(fact.id, *var);
        }
    }
    if rewrites.is_empty() {
        return;
    }

    let sty = ctx.ty_sampler();
    let consumed: Vec<Word> = rewrites.keys().copied().collect();

    let func = &mut ctx.module.functions[entry_idx];
    for blk in func.blocks.iter_mut() {
        let mut rebuilt: Vec<Instruction> =
            Vec::with_capacity(blk.instructions.len() + rewrites.len());
        for inst in blk.instructions.drain(..) {
            if let Some(id) = inst.result_id {
                if let Some(var) = rewrites.remove(&id) {
                    let mut load = inst;
                    load.class.opcode = Op::Load;
                    load.result_type = Some(sty);
                    load.operands = vec![Operand::IdRef(var)];
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
