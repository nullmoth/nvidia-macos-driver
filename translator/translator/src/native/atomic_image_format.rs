//! Storage images used by an atomic get an atomic-capable format.
//!
//! Metal allows texture atomics only on R32Uint / R32Sint (and R32Float for exchange), so a texture an atomic touches is
//! one of those formats whatever its declaration says. Vulkan requires the `OpTypeImage` an `OpImageTexelPointer`
//! addresses to be declared R32ui, R32i, R32f, R64ui or R64i. The storage format of a texture parameter is inferred from
//! the calls in the entry function; an atomic inside a helper the texture reaches through a struct (Blender's Eevee
//! shadow pass: `MTLShaderFragmentImpl::main` doing `atomic_fetch_min` on `shadow_atlas_img`) is not seen there, so the
//! image kept a write-inferred Rgba8ui and the module was refused, which left Blender with no pipeline and crashed it.
//!
//! This rewrite runs on the finished module: for every `OpImageTexelPointer` whose image is declared in a format atomics
//! cannot use, the variable it addresses (through access chains and copies) gets its own image type in the 32-bit format
//! matching the image's sampled type, and every pointer and loaded value derived from that variable is retyped. Other
//! variables that shared the old image type keep it.

use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{ImageFormat, Op, StorageClass, Word};
use std::collections::{HashMap, HashSet};

fn atomic_capable(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::R32ui | ImageFormat::R32i | ImageFormat::R32f | ImageFormat::R64ui | ImageFormat::R64i
    )
}

/// The atomic format for an image whose sampled type is `sampled`: unsigned/signed 32 or 64-bit integer, or 32-bit float.
fn atomic_format_for(defs: &HashMap<Word, Instruction>, sampled: Word) -> Option<ImageFormat> {
    let def = defs.get(&sampled)?;
    match (def.class.opcode, def.operands.as_slice()) {
        (Op::TypeInt, [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]) => Some(ImageFormat::R32ui),
        (Op::TypeInt, [Operand::LiteralBit32(32), Operand::LiteralBit32(1)]) => Some(ImageFormat::R32i),
        (Op::TypeInt, [Operand::LiteralBit32(64), Operand::LiteralBit32(0)]) => Some(ImageFormat::R64ui),
        (Op::TypeInt, [Operand::LiteralBit32(64), Operand::LiteralBit32(1)]) => Some(ImageFormat::R64i),
        (Op::TypeFloat, [Operand::LiteralBit32(32)]) => Some(ImageFormat::R32f),
        _ => None,
    }
}

fn first_id(inst: &Instruction) -> Option<Word> {
    match inst.operands.first() {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    }
}

pub(super) fn construct_atomic_storage_image_formats(module: &mut Module) -> bool {
    let defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    // every function-level instruction by result id, to walk an image pointer back to its variable
    let mut producers: HashMap<Word, Instruction> = HashMap::new();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if let Some(id) = inst.result_id {
                    producers.insert(id, inst.clone());
                }
            }
        }
    }
    let pointee_of = |ptr_ty: Word| -> Option<(StorageClass, Word)> {
        let def = defs.get(&ptr_ty)?;
        match (def.class.opcode, def.operands.as_slice()) {
            (Op::TypePointer, [Operand::StorageClass(sc), Operand::IdRef(p)]) => Some((*sc, *p)),
            _ => None,
        }
    };

    // variables addressed by an OpImageTexelPointer whose image format atomics cannot use -> the format to give them
    let mut targets: Vec<(Word, ImageFormat)> = Vec::new();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if inst.class.opcode != Op::ImageTexelPointer {
                    continue;
                }
                let Some(mut cur) = first_id(inst) else { continue };
                let mut var = None;
                for _ in 0..16 {
                    if let Some(p) = producers.get(&cur) {
                        match p.class.opcode {
                            Op::AccessChain | Op::InBoundsAccessChain | Op::CopyObject => {
                                let Some(base) = first_id(p) else { break };
                                cur = base;
                                continue;
                            }
                            Op::Variable => {
                                var = Some(cur);
                            }
                            _ => {}
                        }
                        break;
                    }
                    if defs.get(&cur).is_some_and(|d| d.class.opcode == Op::Variable) {
                        var = Some(cur);
                    }
                    break;
                }
                let Some(var) = var else { continue };
                let Some(var_ty) = defs.get(&var).and_then(|d| d.result_type).or_else(|| producers.get(&var).and_then(|d| d.result_type)) else {
                    continue;
                };
                let Some((_, mut image)) = pointee_of(var_ty) else { continue };
                while let Some(d) = defs.get(&image) {
                    if matches!(d.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
                        let Some(elem) = first_id(d) else { break };
                        image = elem;
                    } else {
                        break;
                    }
                }
                let Some(img) = defs.get(&image) else { continue };
                if img.class.opcode != Op::TypeImage {
                    continue;
                }
                let (Some(Operand::IdRef(sampled)), Some(Operand::ImageFormat(fmt))) = (img.operands.first(), img.operands.get(6)) else {
                    continue;
                };
                if atomic_capable(*fmt) {
                    continue;
                }
                let Some(want) = atomic_format_for(&defs, *sampled) else { continue };
                if !targets.iter().any(|(v, _)| *v == var) {
                    targets.push((var, want));
                }
            }
        }
    }
    if targets.is_empty() {
        return false;
    }

    let mut changed = false;
    for (var, want) in targets {
        // the variable's type chain: pointer -> (array ->)* image
        let Some(var_def_ty) = defs.get(&var).and_then(|d| d.result_type) else { continue };
        let Some((sc, top)) = pointee_of(var_def_ty) else { continue };
        let mut chain = vec![top];
        while let Some(d) = defs.get(&chain.last().copied().unwrap_or(0)) {
            if matches!(d.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
                if let Some(elem) = first_id(d) {
                    chain.push(elem);
                    continue;
                }
            }
            break;
        }
        let image = *chain.last().unwrap_or(&0);
        let Some(img) = defs.get(&image) else { continue };
        let mut new_types: Vec<Instruction> = Vec::new();
        // remap old type id -> new type id, innermost first
        let mut remap: HashMap<Word, Word> = HashMap::new();
        let new_image = module.fresh_id();
        let mut ops = img.operands.clone();
        ops[6] = Operand::ImageFormat(want);
        new_types.push(Instruction::new(Op::TypeImage, None, Some(new_image), ops));
        remap.insert(image, new_image);
        for &outer in chain.iter().rev().skip(1) {
            let d = &defs[&outer];
            let mut ops = d.operands.clone();
            if let Some(Operand::IdRef(e)) = ops.first_mut() {
                *e = remap[e];
            }
            let id = module.fresh_id();
            new_types.push(Instruction::new(d.class.opcode, None, Some(id), ops));
            remap.insert(outer, id);
        }
        // a pointer of the variable's storage class to every remapped type (access chains may stop at any level)
        let mut ptr_to: HashMap<Word, Word> = HashMap::new();
        for (&old, &new) in &remap {
            let id = module.fresh_id();
            new_types.push(Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![Operand::StorageClass(sc), Operand::IdRef(new)],
            ));
            ptr_to.insert(old, id);
        }
        // every id derived from the variable: pointers keep their level, loads become the new value type
        let mut derived: HashSet<Word> = HashSet::from([var]);
        loop {
            let before = derived.len();
            for p in producers.values() {
                if matches!(p.class.opcode, Op::AccessChain | Op::InBoundsAccessChain | Op::CopyObject | Op::Load)
                    && first_id(p).is_some_and(|b| derived.contains(&b))
                {
                    if let Some(id) = p.result_id {
                        derived.insert(id);
                    }
                }
            }
            if derived.len() == before {
                break;
            }
        }
        let Some(var_pos) = module
            .types_global_values
            .iter()
            .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
        else {
            continue;
        };
        module.types_global_values[var_pos].result_type = Some(ptr_to[&top]);
        let tail = module.types_global_values.split_off(var_pos);
        module.types_global_values.extend(new_types);
        module.types_global_values.extend(tail);
        for function in module.functions.iter_mut() {
            for block in function.blocks.iter_mut() {
                for inst in block.instructions.iter_mut() {
                    let Some(id) = inst.result_id else { continue };
                    if id == var || !derived.contains(&id) {
                        continue;
                    }
                    let Some(old_ty) = inst.result_type else { continue };
                    if inst.class.opcode == Op::Load {
                        if let Some(&n) = remap.get(&old_ty) {
                            inst.result_type = Some(n);
                        }
                    } else if let Some((_, old_pointee)) = pointee_of(old_ty) {
                        if let Some(&n) = ptr_to.get(&old_pointee) {
                            inst.result_type = Some(n);
                        }
                    }
                }
            }
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};
    use spirv::Dim;

    fn ins(op: Op, ty: Option<Word>, id: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, ops)
    }

    // %1 uint, %2 image(uint, 2D, arrayed, Rgba8ui), %3 ptr UniformConstant %2, %4 var, %5 uint3 coord, %6 ptr Image uint,
    // a second variable %9 of the same image type that no atomic touches.
    fn module(fmt: ImageFormat) -> Module {
        let mut m = Module::default();
        m.header = Some(ModuleHeader::new(20));
        m.types_global_values = vec![
            ins(Op::TypeInt, None, Some(1), vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]),
            ins(Op::TypeImage, None, Some(2), vec![
                Operand::IdRef(1), Operand::Dim(Dim::Dim2D), Operand::LiteralBit32(0), Operand::LiteralBit32(1),
                Operand::LiteralBit32(0), Operand::LiteralBit32(2), Operand::ImageFormat(fmt)]),
            ins(Op::TypePointer, None, Some(3), vec![Operand::StorageClass(StorageClass::UniformConstant), Operand::IdRef(2)]),
            ins(Op::Variable, Some(3), Some(4), vec![Operand::StorageClass(StorageClass::UniformConstant)]),
            ins(Op::Variable, Some(3), Some(9), vec![Operand::StorageClass(StorageClass::UniformConstant)]),
            ins(Op::TypePointer, None, Some(6), vec![Operand::StorageClass(StorageClass::Image), Operand::IdRef(1)]),
        ];
        let mut block = Block::new();
        block.instructions = vec![
                ins(Op::CopyObject, Some(3), Some(7), vec![Operand::IdRef(4)]),
                ins(Op::ImageTexelPointer, Some(6), Some(8), vec![Operand::IdRef(7), Operand::IdRef(5), Operand::IdRef(5)]),
                ins(Op::Load, Some(2), Some(10), vec![Operand::IdRef(9)]),
        ];
        let mut f = Function::new();
        f.blocks = vec![block];
        m.functions = vec![f];
        m
    }

    fn format_of_var(m: &Module, var: Word) -> Option<ImageFormat> {
        let defs: HashMap<Word, &Instruction> =
            m.types_global_values.iter().filter_map(|i| i.result_id.map(|id| (id, i))).collect();
        let ptr = defs.get(&var)?.result_type?;
        let Some(Operand::IdRef(img)) = defs.get(&ptr)?.operands.get(1) else { return None };
        match defs.get(img)?.operands.get(6) {
            Some(Operand::ImageFormat(f)) => Some(*f),
            _ => None,
        }
    }

    #[test]
    fn an_image_an_atomic_addresses_becomes_r32ui_and_its_sibling_keeps_rgba8ui() {
        let mut m = module(ImageFormat::Rgba8ui);
        assert!(construct_atomic_storage_image_formats(&mut m), "the rewrite must fire on a Rgba8ui atomic image");
        assert_eq!(format_of_var(&m, 4), Some(ImageFormat::R32ui));
        assert_eq!(format_of_var(&m, 9), Some(ImageFormat::Rgba8ui), "a variable no atomic touches keeps its type");
        let copy = &m.functions[0].blocks[0].instructions[0];
        assert_eq!(copy.result_type, m.types_global_values.iter().find(|i| i.result_id == Some(4)).and_then(|v| v.result_type),
                   "the copied pointer follows the variable's new type");
    }

    #[test]
    fn an_image_already_r32ui_is_left_alone() {
        let mut m = module(ImageFormat::R32ui);
        assert!(!construct_atomic_storage_image_formats(&mut m));
        assert_eq!(format_of_var(&m, 4), Some(ImageFormat::R32ui));
    }
}
