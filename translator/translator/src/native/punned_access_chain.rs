//! An access chain whose index path lands on a member of a different scalar TYPE than its result points to.
//!
//! The ray-tracing payload is one blob of memory shared by every shader in a pipeline, and each shader was
//! compiled with its OWN view of it: measured on the game's five real pipelines, `%struct.Payload` has three
//! different layouts across the linked modules (`{i32,i32,float,float}` in 31, `{float}` in 8,
//! `{<3 x float>,float,i32}` in 7). When a helper from one module is inlined into an entry carrying another
//! view, its chain walks the caller's `{header, payload}` type and asks for a member the caller declares with
//! a different scalar type:
//!
//! ```text
//! index path = [1, 0];  base points at { header, { i32, i32, float, float } }
//! selects TypeInt 32 0, but the result pointer points to TypeFloat 32
//! ```
//!
//! A pointer of the wrong type is never legal in Logical SPIR-V and no capability admits one, so the pun has
//! to be resolved at the chain's USES, not at its construction: the chain takes the pointer type the member
//! actually has, each load of it loads that type and bitcasts the VALUE, and each store bitcasts before
//! storing. That is the same move as phi-ing loaded values rather than pointers.
//!
//! Deliberately narrow:
//! - only a scalar-for-scalar pun of IDENTICAL bit width. A different width is a different datum, not another
//!   view of one, and is left for the validator to reject.
//! - every use of the chain must be a plain load or store through it. A chain stacked on top, a pointer handed
//!   to a function, or any other mention keeps its meaning only with the original type, so the plan is dropped
//!   and the refusal reads exactly as it did before.
//!
//! THE ASSUMPTION THIS RESTS ON, SAID OUT LOUD: that member k of one shader's payload view is the same
//! datum as member k of the pipeline's. The index path is the only mapping available — byte offset is
//! provably NOT it, because the views also pad their headers differently (`[4 x i8]` against `[20 x i8]`, so
//! the payload starts at 32 bytes in one and 48 in another, and a byte-preserving map would land in padding.)
//! Translating is therefore not the same as being correct here: only a rendered frame settles whether the
//! datum is the right one.

use super::*;

fn pointee(defs: &HashMap<Word, Instruction>, pointer_ty: Word) -> Option<Word> {
    let def = defs.get(&pointer_ty)?;
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    match def.operands.get(1) {
        Some(Operand::IdRef(pointee)) => Some(*pointee),
        _ => None,
    }
}

fn storage_class(defs: &HashMap<Word, Instruction>, pointer_ty: Word) -> Option<Operand> {
    let def = defs.get(&pointer_ty)?;
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    def.operands.first().cloned()
}

/// The bit width of a scalar type, or None for anything that is not one.
fn scalar_width(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let def = defs.get(&ty)?;
    match def.class.opcode {
        Op::TypeInt | Op::TypeFloat => match def.operands.first() {
            Some(Operand::LiteralBit32(width)) => Some(*width),
            _ => None,
        },
        _ => None,
    }
}

fn constant_u32(defs: &HashMap<Word, Instruction>, id: Word) -> Option<u32> {
    let def = defs.get(&id)?;
    if def.class.opcode != Op::Constant {
        return None;
    }
    match def.operands.first() {
        Some(Operand::LiteralBit32(value)) => Some(*value),
        _ => None,
    }
}

/// The type an index path selects, or None when the path cannot be walked - the validator's to report.
fn selected_type(defs: &HashMap<Word, Instruction>, mut current: Word, indices: &[Operand]) -> Option<Word> {
    for index in indices {
        let Operand::IdRef(index) = index else { return None };
        let def = defs.get(&current)?;
        current = match def.class.opcode {
            Op::TypeStruct => {
                let member = constant_u32(defs, *index)? as usize;
                match def.operands.get(member) {
                    Some(Operand::IdRef(ty)) => *ty,
                    _ => return None,
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                match def.operands.first() {
                    Some(Operand::IdRef(ty)) => *ty,
                    _ => return None,
                }
            }
            _ => return None,
        };
    }
    Some(current)
}

fn find_or_create_pointer(module: &mut Module, storage: Operand, pointee_ty: Word) -> Word {
    let wanted = vec![storage, Operand::IdRef(pointee_ty)];
    if let Some(existing) = module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == Op::TypePointer && i.operands == wanted)
            .then_some(i.result_id)
            .flatten()
    }) {
        return existing;
    }
    let id = module.fresh_id();
    module
        .types_global_values
        .push(Instruction::new(Op::TypePointer, None, Some(id), wanted));
    id
}

/// Retypes every access chain that is punned only by a same-width scalar; returns how many it retyped.
pub(super) fn retype_punned_access_chains(module: &mut Module) -> usize {
    let defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    let mut value_types: HashMap<Word, Word> = HashMap::default();
    for instruction in module.all_inst_iter() {
        if let (Some(id), Some(ty)) = (instruction.result_id, instruction.result_type) {
            value_types.insert(id, ty);
        }
    }

    // chain result -> (the type the result pointer claims, the type the member actually has, storage class)
    let mut candidates: HashMap<Word, (Word, Word, Operand)> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let first_index = match instruction.class.opcode {
                    Op::AccessChain | Op::InBoundsAccessChain => 1,
                    Op::PtrAccessChain | Op::InBoundsPtrAccessChain => 2,
                    _ => continue,
                };
                let (Some(result), Some(result_ty)) = (instruction.result_id, instruction.result_type)
                else {
                    continue;
                };
                if instruction.operands.len() <= first_index {
                    continue;
                }
                let Some(Operand::IdRef(base)) = instruction.operands.first() else { continue };
                let Some(base_ty) = value_types.get(base) else { continue };
                let (Some(base_pointee), Some(want)) =
                    (pointee(&defs, *base_ty), pointee(&defs, result_ty))
                else {
                    continue;
                };
                let Some(selected) =
                    selected_type(&defs, base_pointee, &instruction.operands[first_index..])
                else {
                    continue;
                };
                if selected == want {
                    continue;
                }
                let (Some(selected_width), Some(want_width)) =
                    (scalar_width(&defs, selected), scalar_width(&defs, want))
                else {
                    continue;
                };
                if selected_width != want_width {
                    continue;
                }
                let Some(storage) = storage_class(&defs, result_ty) else { continue };
                candidates.insert(result, (want, selected, storage));
            }
        }
    }
    if candidates.is_empty() {
        return 0;
    }

    // Drop any chain that is used as anything but the pointer of a plain load or store.
    let mut usable: std::collections::HashSet<Word> = candidates.keys().copied().collect();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let pointer_operand = match instruction.class.opcode {
                    Op::Load | Op::Store => match instruction.operands.first() {
                        Some(Operand::IdRef(id)) if candidates.contains_key(id) => Some(*id),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(pointer) = pointer_operand {
                    // a load must ask for exactly the punned type; otherwise this is a shape we have not seen
                    if instruction.class.opcode == Op::Load
                        && instruction.result_type != Some(candidates[&pointer].0)
                    {
                        usable.remove(&pointer);
                    }
                    // the rest of a load's/store's operands still get the general check below
                    for operand in instruction.operands.iter().skip(1) {
                        if let Operand::IdRef(id) = operand {
                            if candidates.contains_key(id) {
                                usable.remove(id);
                            }
                        }
                    }
                    continue;
                }
                for operand in &instruction.operands {
                    if let Operand::IdRef(id) = operand {
                        if candidates.contains_key(id) {
                            usable.remove(id);
                        }
                    }
                }
            }
        }
    }
    candidates.retain(|id, _| usable.contains(id));
    if candidates.is_empty() {
        return 0;
    }

    // Every fresh id is taken BEFORE the rewrite: the blocks are borrowed mutably during it.
    let mut load_sites: Vec<Word> = Vec::new();
    let mut store_sites: usize = 0;
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let Some(Operand::IdRef(pointer)) = instruction.operands.first() else { continue };
                if !candidates.contains_key(pointer) {
                    continue;
                }
                match instruction.class.opcode {
                    Op::Load => {
                        if let Some(id) = instruction.result_id {
                            load_sites.push(id);
                        }
                    }
                    Op::Store => store_sites += 1,
                    _ => {}
                }
            }
        }
    }
    let raw_load_id: HashMap<Word, Word> = load_sites
        .iter()
        .map(|&id| (id, module.fresh_id()))
        .collect();
    let mut store_cast_ids: Vec<Word> = (0..store_sites).map(|_| module.fresh_id()).collect();
    store_cast_ids.reverse(); // popped from the back, so they are handed out in order

    let mut retyped: HashMap<Word, (Word, Word, Word)> = HashMap::default(); // chain -> (ptr ty, selected, want)
    // Sorted, because `candidates` is a HashMap and find_or_create_pointer allocates ids as it
    // goes: iterating it in hash order gave the same module a different set of OpTypePointer ids on
    // every run, so the output bytes were not reproducible. That breaks a hash-keyed translation
    // cache and makes a byte-for-byte A/B of two builds meaningless.
    let mut plans: Vec<(Word, (Word, Word, Operand))> = candidates.into_iter().collect();
    plans.sort_by_key(|&(id, _)| id);
    for (id, (want, selected, storage)) in plans {
        let pointer_ty = find_or_create_pointer(module, storage, selected);
        retyped.insert(id, (pointer_ty, selected, want));
    }

    let mut repaired = 0;
    for function in &mut module.functions {
        for block in &mut function.blocks {
            let original = std::mem::take(&mut block.instructions);
            let mut rewritten: Vec<Instruction> = Vec::with_capacity(original.len() + 1);
            for instruction in original {
                // 1 · the chain itself takes the pointer type the member actually has
                if let Some((pointer_ty, _, _)) =
                    instruction.result_id.and_then(|id| retyped.get(&id))
                {
                    let mut chain = instruction;
                    chain.result_type = Some(*pointer_ty);
                    rewritten.push(chain);
                    repaired += 1;
                    continue;
                }
                let plan = match instruction.operands.first() {
                    Some(Operand::IdRef(pointer)) => retyped.get(pointer).copied(),
                    _ => None,
                };
                match (instruction.class.opcode, plan) {
                    // 2 · a load reads the declared type, then the VALUE is bitcast to what the shader wanted
                    (Op::Load, Some((_, selected, want))) => {
                        // Step 1 has already retyped the chain, so pushing the original load here
                        // emits a load of `want` through a pointer now typed `selected` - invalid
                        // SPIR-V, and silently. The id sets are built to match so neither arm should
                        // be reachable; say so loudly if one ever is, rather than shipping bad bytes.
                        let Some(load_id) = instruction.result_id else {
                            eprintln!("PUNNED-CHAIN-NO-RESULT a punned load has no result id; its pointer was already retyped");
                            rewritten.push(instruction);
                            continue;
                        };
                        let Some(&raw) = raw_load_id.get(&load_id) else {
                            eprintln!("PUNNED-CHAIN-NO-RAW no raw id for punned load %{load_id}; its pointer was already retyped");
                            rewritten.push(instruction);
                            continue;
                        };
                        let mut raw_load = instruction;
                        raw_load.result_type = Some(selected);
                        raw_load.result_id = Some(raw);
                        rewritten.push(raw_load);
                        rewritten.push(Instruction::new(
                            Op::Bitcast,
                            Some(want),
                            Some(load_id),
                            vec![Operand::IdRef(raw)],
                        ));
                    }
                    // 3 · a store bitcasts the value first, so the pointer and the value agree
                    (Op::Store, Some((_, selected, _))) => {
                        // Same hazard as the load arm above: the pointer is already retyped, so a
                        // plain push stores the wrong type through it without a word of complaint.
                        let Some(Operand::IdRef(value)) = instruction.operands.get(1).cloned() else {
                            eprintln!("PUNNED-CHAIN-NO-VALUE a punned store has no value operand; its pointer was already retyped");
                            rewritten.push(instruction);
                            continue;
                        };
                        let Some(cast) = store_cast_ids.pop() else {
                            eprintln!("PUNNED-CHAIN-NO-CAST ran out of bitcast ids for a punned store; its pointer was already retyped");
                            rewritten.push(instruction);
                            continue;
                        };
                        rewritten.push(Instruction::new(
                            Op::Bitcast,
                            Some(selected),
                            Some(cast),
                            vec![Operand::IdRef(value)],
                        ));
                        let mut store = instruction;
                        store.operands[1] = Operand::IdRef(cast);
                        rewritten.push(store);
                    }
                    _ => rewritten.push(instruction),
                }
            }
            block.instructions = rewritten;
        }
    }
    repaired
}
