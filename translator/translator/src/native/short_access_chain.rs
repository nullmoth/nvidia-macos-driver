//! An access chain that stops one or more composite levels short of the type its result points to.
//!
//! LLVM's opaque pointers let a load of `i64` go straight through a pointer to `{ i64, i64 }`, because the first
//! member sits at offset 0. SPIR-V requires an access chain's index path to land exactly on its result pointer's
//! pointee, so such a chain arrives here selecting the aggregate while its result type names the first member.
//! Appending a zero index for each level down to that member addresses the same bytes with the type SPIR-V
//! requires. It is the address the raw-buffer fallback already uses for storage buffers; this lets workgroup,
//! private and function memory, which have no raw fallback, carry the same chain.
//!
//! Only the first member is ever chosen. A chain whose result type cannot be reached through member 0 is left as
//! it is for the module validator to reject: choosing any other member would read the wrong field.

use super::*;

/// Deepest descent attempted. Real chains need one or two levels; the bound keeps a malformed type graph from
/// looping.
const MAX_DESCENT: usize = 8;

fn first_child(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<Word> {
    let def = defs.get(&ty)?;
    match def.class.opcode {
        Op::TypeStruct | Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
            match def.operands.first() {
                Some(Operand::IdRef(child)) => Some(*child),
                _ => None,
            }
        }
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

/// The type an index path selects, or None when the path cannot be walked (a non-constant struct index, or an
/// index into a non-composite) - those are the validator's to report, not ours to guess at.
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
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => match def.operands.first() {
                Some(Operand::IdRef(ty)) => *ty,
                _ => return None,
            },
            _ => return None,
        };
    }
    Some(current)
}

/// How many zero indices take `selected` down to `want` through first members only, or None.
fn first_member_depth(defs: &HashMap<Word, Instruction>, selected: Word, want: Word) -> Option<usize> {
    let mut ty = selected;
    for depth in 0..=MAX_DESCENT {
        if ty == want {
            return (depth > 0).then_some(depth);
        }
        ty = first_child(defs, ty)?;
    }
    None
}

fn u32_zero(module: &mut Module) -> Word {
    let uint = module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == Op::TypeInt
            && i.operands.first() == Some(&Operand::LiteralBit32(32))
            && i.operands.get(1) == Some(&Operand::LiteralBit32(0)))
        .then_some(i.result_id)
        .flatten()
    });
    let uint = match uint {
        Some(id) => id,
        None => {
            let id = module.fresh_id();
            module.types_global_values.push(Instruction::new(
                Op::TypeInt,
                None,
                Some(id),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ));
            id
        }
    };
    if let Some(existing) = module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == Op::Constant
            && i.result_type == Some(uint)
            && i.operands.first() == Some(&Operand::LiteralBit32(0)))
        .then_some(i.result_id)
        .flatten()
    }) {
        return existing;
    }
    let id = module.fresh_id();
    module
        .types_global_values
        .push(Instruction::new(Op::Constant, Some(uint), Some(id), vec![Operand::LiteralBit32(0)]));
    id
}

/// Repairs every access chain that is short only by first-member descent; returns how many it repaired.
pub(super) fn decay_short_access_chains(module: &mut Module) -> usize {
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

    let mut plans: HashMap<Word, usize> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                // The index path starts after the base, and after the element operand of a Ptr chain, which
                // steps across the base's pointee rather than into it.
                let first_index = match instruction.class.opcode {
                    Op::AccessChain | Op::InBoundsAccessChain => 1,
                    Op::PtrAccessChain | Op::InBoundsPtrAccessChain => 2,
                    _ => continue,
                };
                let (Some(result), Some(result_ty)) = (instruction.result_id, instruction.result_type) else {
                    continue;
                };
                if instruction.operands.len() < first_index {
                    continue;
                }
                let Some(Operand::IdRef(base)) = instruction.operands.first() else { continue };
                let Some(base_ty) = value_types.get(base) else { continue };
                let (Some(base_pointee), Some(want)) = (pointee(&defs, *base_ty), pointee(&defs, result_ty)) else {
                    continue;
                };
                let Some(selected) = selected_type(&defs, base_pointee, &instruction.operands[first_index..]) else {
                    continue;
                };
                if let Some(depth) = first_member_depth(&defs, selected, want) {
                    plans.insert(result, depth);
                }
            }
        }
    }
    if plans.is_empty() {
        return 0;
    }

    let zero = u32_zero(module);
    let mut repaired = 0;
    for function in &mut module.functions {
        for block in &mut function.blocks {
            for instruction in &mut block.instructions {
                if let Some(depth) = instruction.result_id.and_then(|id| plans.get(&id)) {
                    instruction.operands.extend(std::iter::repeat(Operand::IdRef(zero)).take(*depth));
                    repaired += 1;
                }
            }
        }
    }
    repaired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ty(op: Op, id: Word, operands: Vec<Operand>) -> Instruction {
        Instruction::new(op, None, Some(id), operands)
    }

    /// The type graph of the failing kernel in miniature: u64 = %1, struct { u64, u64 } = %2,
    /// struct { u32, u64 } = %6, pointers in Workgroup memory.
    fn defs() -> HashMap<Word, Instruction> {
        let wg = Operand::StorageClass(StorageClass::Workgroup);
        [
            ty(Op::TypeInt, 1, vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)]),
            ty(Op::TypeStruct, 2, vec![Operand::IdRef(1), Operand::IdRef(1)]),
            ty(Op::TypePointer, 3, vec![wg.clone(), Operand::IdRef(2)]),
            ty(Op::TypePointer, 4, vec![wg.clone(), Operand::IdRef(1)]),
            ty(Op::TypeInt, 5, vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]),
            ty(Op::TypeStruct, 6, vec![Operand::IdRef(5), Operand::IdRef(1)]),
            ty(Op::TypeArray, 7, vec![Operand::IdRef(2), Operand::IdRef(9)]),
        ]
        .into_iter()
        .map(|i| (i.result_id.unwrap(), i))
        .collect()
    }

    #[test]
    fn a_chain_stopping_at_a_pair_of_u64_reaches_its_first_member_with_one_zero() {
        assert_eq!(first_member_depth(&defs(), 2, 1), Some(1));
    }

    #[test]
    fn a_chain_two_levels_short_through_an_array_of_pairs_takes_two_zeros() {
        assert_eq!(first_member_depth(&defs(), 7, 1), Some(2));
    }

    #[test]
    fn a_struct_whose_first_member_is_not_the_wanted_type_is_left_for_the_validator() {
        // struct { u32, u64 } wanting u64: member 1 has the type, member 0 does not. Choosing member 1 would
        // read a different field, so the chain must stay invalid rather than be guessed at.
        assert_eq!(first_member_depth(&defs(), 6, 1), None);
    }

    #[test]
    fn a_chain_that_already_matches_is_not_touched() {
        assert_eq!(first_member_depth(&defs(), 1, 1), None);
    }

    #[test]
    fn the_pointee_of_the_result_pointer_is_read_from_the_pointer_type() {
        assert_eq!(pointee(&defs(), 4), Some(1));
        assert_eq!(selected_type(&defs(), 2, &[]), Some(2));
    }
}
