//! `OpCopyMemory` between two pointee types that are the same aggregate laid out differently becomes load + `OpCopyLogical`
//! + store.
//!
//! A storage-buffer struct carries explicit layout decorations and so is a different SPIR-V type from the function-local
//! struct of the same shape, and `OpCopyMemory` requires one pointee type on both sides. Element-wise copies of a buffer
//! struct into a local (Blender's Eevee compute shaders: a light or shadow record copied out of a buffer array) emitted it
//! anyway and the module was refused. SPIR-V 1.4 added `OpCopyLogical` for exactly this: types whose members match one for
//! one, ignoring decorations. A pair that does not match logically is left alone and still refused by the module check.

use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Op, Word};
use std::collections::HashMap;

fn logical_match(defs: &HashMap<Word, Instruction>, a: Word, b: Word, depth: u32) -> bool {
    if a == b {
        return true;
    }
    if depth > 32 {
        return false;
    }
    let (Some(da), Some(db)) = (defs.get(&a), defs.get(&b)) else { return false };
    if da.class.opcode != db.class.opcode {
        return false;
    }
    match da.class.opcode {
        Op::TypeStruct => {
            da.operands.len() == db.operands.len()
                && da.operands.iter().zip(&db.operands).all(|(x, y)| match (x, y) {
                    (Operand::IdRef(x), Operand::IdRef(y)) => logical_match(defs, *x, *y, depth + 1),
                    _ => false,
                })
        }
        Op::TypeArray => match (da.operands.as_slice(), db.operands.as_slice()) {
            ([Operand::IdRef(ea), Operand::IdRef(la)], [Operand::IdRef(eb), Operand::IdRef(lb)]) => {
                let len = |id: &Word| defs.get(id).map(|d| (d.class.opcode, d.operands.clone()));
                len(la).is_some() && len(la) == len(lb) && logical_match(defs, *ea, *eb, depth + 1)
            }
            _ => false,
        },
        // scalars, vectors and matrices: the same declaration under two ids
        Op::TypeInt | Op::TypeFloat | Op::TypeBool | Op::TypeVector | Op::TypeMatrix => da.operands == db.operands,
        _ => false,
    }
}

pub(super) fn split_mismatched_copy_memory(module: &mut Module) -> bool {
    let defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    let mut value_type: HashMap<Word, Word> = HashMap::new();
    for inst in module.types_global_values.iter().chain(module.functions.iter().flat_map(|f| f.all_inst_iter())) {
        if let (Some(id), Some(ty)) = (inst.result_id, inst.result_type) {
            value_type.insert(id, ty);
        }
    }
    let pointee = |ptr: Word| -> Option<Word> {
        let d = defs.get(value_type.get(&ptr)?)?;
        match (d.class.opcode, d.operands.get(1)) {
            (Op::TypePointer, Some(Operand::IdRef(p))) => Some(*p),
            _ => None,
        }
    };
    // (function, block, index) -> (target, source, target pointee, source pointee)
    let mut sites = Vec::new();
    for (fi, f) in module.functions.iter().enumerate() {
        for (bi, b) in f.blocks.iter().enumerate() {
            for (ii, inst) in b.instructions.iter().enumerate() {
                if inst.class.opcode != Op::CopyMemory {
                    continue;
                }
                let (Some(Operand::IdRef(dst)), Some(Operand::IdRef(src))) = (inst.operands.first(), inst.operands.get(1)) else {
                    continue;
                };
                let (Some(dt), Some(st)) = (pointee(*dst), pointee(*src)) else { continue };
                if dt != st && logical_match(&defs, dt, st, 0) {
                    sites.push((fi, bi, ii, *dst, *src, dt, st));
                }
            }
        }
    }
    if sites.is_empty() {
        return false;
    }
    // back to front, so the indices of earlier sites in the same block stay valid
    for &(fi, bi, ii, dst, src, dt, st) in sites.iter().rev() {
        let loaded = module.fresh_id();
        let copied = module.fresh_id();
        let replacement = [
            Instruction::new(Op::Load, Some(st), Some(loaded), vec![Operand::IdRef(src)]),
            Instruction::new(Op::CopyLogical, Some(dt), Some(copied), vec![Operand::IdRef(loaded)]),
            Instruction::new(Op::Store, None, None, vec![Operand::IdRef(dst), Operand::IdRef(copied)]),
        ];
        let instructions = &mut module.functions[fi].blocks[bi].instructions;
        instructions.splice(ii..ii + 1, replacement);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};
    use spirv::StorageClass;

    fn ins(op: Op, ty: Option<Word>, id: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, ops)
    }

    // %1 uint, %2 struct{uint} (buffer), %3 struct{uint} (local), %4 ptr SB %2, %5 ptr Function %3, %6/%7 vars,
    // %8 struct{uint, uint} (a different shape), %9 ptr Function %8, %10 var
    fn module() -> Module {
        let mut m = Module::default();
        m.header = Some(ModuleHeader::new(30));
        m.types_global_values = vec![
            ins(Op::TypeInt, None, Some(1), vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]),
            ins(Op::TypeStruct, None, Some(2), vec![Operand::IdRef(1)]),
            ins(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(1)]),
            ins(Op::TypePointer, None, Some(4), vec![Operand::StorageClass(StorageClass::StorageBuffer), Operand::IdRef(2)]),
            ins(Op::TypePointer, None, Some(5), vec![Operand::StorageClass(StorageClass::Function), Operand::IdRef(3)]),
            ins(Op::Variable, Some(4), Some(6), vec![Operand::StorageClass(StorageClass::StorageBuffer)]),
            ins(Op::TypeStruct, None, Some(8), vec![Operand::IdRef(1), Operand::IdRef(1)]),
            ins(Op::TypePointer, None, Some(9), vec![Operand::StorageClass(StorageClass::Function), Operand::IdRef(8)]),
        ];
        let mut b = Block::new();
        b.instructions = vec![
            ins(Op::Variable, Some(5), Some(7), vec![Operand::StorageClass(StorageClass::Function)]),
            ins(Op::Variable, Some(9), Some(10), vec![Operand::StorageClass(StorageClass::Function)]),
            ins(Op::CopyMemory, None, None, vec![Operand::IdRef(7), Operand::IdRef(6)]),
            ins(Op::CopyMemory, None, None, vec![Operand::IdRef(10), Operand::IdRef(6)]),
        ];
        let mut f = Function::new();
        f.blocks = vec![b];
        m.functions = vec![f];
        m
    }

    #[test]
    fn a_buffer_struct_copied_to_a_local_struct_of_the_same_shape_becomes_copy_logical() {
        let mut m = module();
        assert!(split_mismatched_copy_memory(&mut m));
        let ops: Vec<Op> = m.functions[0].blocks[0].instructions.iter().map(|i| i.class.opcode).collect();
        assert_eq!(ops, vec![Op::Variable, Op::Variable, Op::Load, Op::CopyLogical, Op::Store, Op::CopyMemory],
                   "the matching copy is split; the copy into a two-member struct is left for the module check to refuse");
    }

    #[test]
    fn a_copy_between_the_same_type_is_left_alone() {
        let mut m = module();
        m.functions[0].blocks[0].instructions.truncate(2);
        m.functions[0].blocks[0].instructions.push(ins(Op::CopyMemory, None, None, vec![Operand::IdRef(7), Operand::IdRef(7)]));
        assert!(!split_mismatched_copy_memory(&mut m));
    }
}
