//! Split a vector access through a PhysicalStorageBuffer pointer whose `Aligned` is smaller than the vector.
//!
//! NVK's compiler requires every memory access to be aligned to its own size (nak/from_nir.rs: `size_B <= align`)
//! and panics in vkCreateComputePipelines otherwise. A device-address load of a float4 is emitted with the element's
//! alignment (4) when the true alignment of the address is not known, which is a 16-byte access promised only 4-byte
//! alignment: MPS NDArrayVectorMatrixMultiply_1_4 stopped pipeline creation this way. Each such access becomes one
//! access per component (an access chain into the vector, aligned to the component), recombined with
//! OpCompositeConstruct or split with OpCompositeExtract. Accesses already aligned to their size are untouched.

use crate::fxhash::HashMap;
use crate::spirv_module::{load_bytes, Instruction, Operand, SpirvModule};
use spirv::{MemoryAccess, Op, StorageClass, Word};

/// The SPIR-V bytes with every under-aligned physical vector access split, or the input unchanged when there is
/// none (or the bytes do not parse, which the caller's own validation reports).
pub fn split_underaligned_physical_vector_accesses(spv: &[u8]) -> Vec<u8> {
    let Ok(mut module) = load_bytes(spv) else { return spv.to_vec() };
    if split_module(&mut module) == 0 {
        return spv.to_vec();
    }
    module.assemble().iter().flat_map(|w| w.to_le_bytes()).collect()
}

struct Types {
    /// result id -> instruction for every type and constant
    defs: HashMap<Word, Instruction>,
}

impl Types {
    fn scalar_bytes(&self, ty: Word) -> Option<u32> {
        let d = self.defs.get(&ty)?;
        match (d.class.opcode, d.operands.first()) {
            (Op::TypeInt | Op::TypeFloat, Some(Operand::LiteralBit32(bits))) => Some(bits.div_ceil(8)),
            _ => None,
        }
    }
    /// (component type, component count) of a vector type
    fn vector(&self, ty: Word) -> Option<(Word, u32)> {
        let d = self.defs.get(&ty)?;
        match (d.class.opcode, d.operands.first(), d.operands.get(1)) {
            (Op::TypeVector, Some(Operand::IdRef(c)), Some(Operand::LiteralBit32(n))) => Some((*c, *n)),
            _ => None,
        }
    }
    /// pointee of a PhysicalStorageBuffer pointer type
    fn psb_pointee(&self, ty: Word) -> Option<Word> {
        let d = self.defs.get(&ty)?;
        match (d.class.opcode, d.operands.first(), d.operands.get(1)) {
            (Op::TypePointer, Some(Operand::StorageClass(StorageClass::PhysicalStorageBuffer)), Some(Operand::IdRef(p))) => Some(*p),
            _ => None,
        }
    }
}

fn aligned_of(operands: &[Operand], from: usize) -> Option<(usize, u32)> {
    let index = operands.iter().skip(from).position(|o| matches!(o, Operand::MemoryAccess(_)))? + from;
    let Operand::MemoryAccess(mask) = operands[index] else { return None };
    if !mask.contains(MemoryAccess::ALIGNED) {
        return None;
    }
    match operands.get(index + 1) {
        Some(Operand::LiteralBit32(a)) => Some((index, *a)),
        _ => None,
    }
}

fn split_module(module: &mut SpirvModule) -> usize {
    let mut types = Types { defs: HashMap::default() };
    for i in &module.types_global_values {
        if let Some(id) = i.result_id { types.defs.insert(id, i.clone()); }
    }
    // the result type of every value, to find each pointer operand's type
    let mut value_type: HashMap<Word, Word> = HashMap::default();
    for i in module.types_global_values.iter().chain(module.functions.iter().flat_map(|f| f.all_inst_iter())) {
        if let (Some(id), Some(ty)) = (i.result_id, i.result_type) { value_type.insert(id, ty); }
    }
    let uint = types.defs.iter().find_map(|(id, d)| {
        (d.class.opcode == Op::TypeInt && d.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]).then_some(*id)
    });
    let mut new_globals: Vec<Instruction> = Vec::new();
    let mut uint = uint.unwrap_or_else(|| {
        let id = module.fresh_id();
        new_globals.push(Instruction::new(Op::TypeInt, None, Some(id), vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]));
        id
    });
    let _ = &mut uint;
    let mut constants: HashMap<u32, Word> = HashMap::default();
    for (id, d) in &types.defs {
        if d.class.opcode == Op::Constant && d.result_type == Some(uint) {
            if let Some(Operand::LiteralBit32(v)) = d.operands.first() { constants.entry(*v).or_insert(*id); }
        }
    }
    let mut psb_ptr: HashMap<Word, Word> = HashMap::default();
    for (id, d) in &types.defs {
        if let Some(p) = types.psb_pointee(*id) { if d.class.opcode == Op::TypePointer { psb_ptr.entry(p).or_insert(*id); } }
    }
    let mut changed = 0usize;
    let mut fresh = |module: &mut SpirvModule| module.fresh_id();
    let function_count = module.functions.len();
    for fi in 0..function_count {
        let block_count = module.functions[fi].blocks.len();
        for bi in 0..block_count {
            let old = std::mem::take(&mut module.functions[fi].blocks[bi].instructions);
            let mut out = Vec::with_capacity(old.len());
            for inst in old {
                let (is_load, ptr, align_from) = match inst.class.opcode {
                    Op::Load => (true, inst.operands.first(), 1),
                    Op::Store => (false, inst.operands.first(), 2),
                    _ => { out.push(inst); continue }
                };
                let Some(Operand::IdRef(ptr)) = ptr.cloned() else { out.push(inst); continue };
                let Some(pointee) = value_type.get(&ptr).and_then(|t| types.psb_pointee(*t)) else { out.push(inst); continue };
                let Some((component, count)) = types.vector(pointee) else { out.push(inst); continue };
                let Some(elem) = types.scalar_bytes(component) else { out.push(inst); continue };
                let declared = aligned_of(&inst.operands, align_from).map_or(0, |(_, a)| a);
                if declared >= elem * count { out.push(inst); continue }
                // each component is aligned to itself, unless the access promised even less
                let piece_align = if declared == 0 { elem } else { elem.min(declared) };
                let elem_ptr = *psb_ptr.entry(component).or_insert_with(|| {
                    let id = fresh(module);
                    new_globals.push(Instruction::new(Op::TypePointer, None, Some(id),
                        vec![Operand::StorageClass(StorageClass::PhysicalStorageBuffer), Operand::IdRef(component)]));
                    id
                });
                let mut index_ids = Vec::new();
                for c in 0..count {
                    let id = *constants.entry(c).or_insert_with(|| {
                        let id = fresh(module);
                        new_globals.push(Instruction::new(Op::Constant, Some(uint), Some(id), vec![Operand::LiteralBit32(c)]));
                        id
                    });
                    index_ids.push(id);
                }
                let access = vec![Operand::MemoryAccess(MemoryAccess::ALIGNED), Operand::LiteralBit32(piece_align)];
                if is_load {
                    let mut parts = Vec::new();
                    for idx in &index_ids {
                        let p = fresh(module);
                        out.push(Instruction::new(Op::InBoundsAccessChain, Some(elem_ptr), Some(p), vec![Operand::IdRef(ptr), Operand::IdRef(*idx)]));
                        let v = fresh(module);
                        let mut ops = vec![Operand::IdRef(p)];
                        ops.extend(access.iter().cloned());
                        out.push(Instruction::new(Op::Load, Some(component), Some(v), ops));
                        parts.push(Operand::IdRef(v));
                    }
                    out.push(Instruction::new(Op::CompositeConstruct, inst.result_type, inst.result_id, parts));
                } else {
                    let Some(Operand::IdRef(value)) = inst.operands.get(1).cloned() else { out.push(inst); continue };
                    for (c, idx) in index_ids.iter().enumerate() {
                        let e = fresh(module);
                        out.push(Instruction::new(Op::CompositeExtract, Some(component), Some(e), vec![Operand::IdRef(value), Operand::LiteralBit32(c as u32)]));
                        let p = fresh(module);
                        out.push(Instruction::new(Op::InBoundsAccessChain, Some(elem_ptr), Some(p), vec![Operand::IdRef(ptr), Operand::IdRef(*idx)]));
                        let mut ops = vec![Operand::IdRef(p), Operand::IdRef(e)];
                        ops.extend(access.iter().cloned());
                        out.push(Instruction::new(Op::Store, None, None, ops));
                    }
                }
                changed += 1;
            }
            module.functions[fi].blocks[bi].instructions = out;
        }
    }
    // new types and constants go after the existing ones, which every use follows
    let insert_at = module.types_global_values.iter().position(|i| i.class.opcode == Op::Variable).unwrap_or(module.types_global_values.len());
    for (k, g) in new_globals.into_iter().enumerate() {
        module.types_global_values.insert(insert_at + k, g);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_float4_load_promised_4_bytes_becomes_four_aligned_float_loads() {
        let asm = "OpCapability Shader\nOpCapability Int64\nOpCapability PhysicalStorageBufferAddresses\nOpExtension \"SPV_KHR_physical_storage_buffer\"\nOpMemoryModel PhysicalStorageBuffer64 GLSL450\nOpEntryPoint GLCompute %main \"main\"\nOpExecutionMode %main LocalSize 1 1 1\n%void = OpTypeVoid\n%fn = OpTypeFunction %void\n%float = OpTypeFloat 32\n%v4 = OpTypeVector %float 4\n%ulong = OpTypeInt 64 0\n%pv4 = OpTypePointer PhysicalStorageBuffer %v4\n%addr = OpConstant %ulong 4096\n%main = OpFunction %void None %fn\n%l = OpLabel\n%p = OpConvertUToPtr %pv4 %addr\n%x = OpLoad %v4 %p Aligned 4\nOpStore %p %x Aligned 4\nOpReturn\nOpFunctionEnd\n";
        let dir = std::env::temp_dir().join(format!("psb_align_split_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("in.spvasm");
        let bin = dir.join("in.spv");
        std::fs::write(&src, asm).unwrap();
        let ok = std::process::Command::new("spirv-as").args(["--target-env", "vulkan1.2"]).arg(&src).arg("-o").arg(&bin)
            .status().expect("spirv-as");
        assert!(ok.success());
        let spv = std::fs::read(&bin).unwrap();
        let out = split_underaligned_physical_vector_accesses(&spv);
        let text = crate::disassemble(&out).expect("disassemble");
        let float_loads = text.lines().filter(|l| l.contains("OpLoad") && l.contains("Aligned 4") && !l.contains("v4")).count();
        let float_stores = text.lines().filter(|l| l.trim_start().starts_with("OpStore") && l.contains("Aligned 4")).count();
        assert_eq!((float_loads, float_stores), (4, 4), "{text}");
        assert!(text.contains("OpCompositeConstruct") && text.contains("OpCompositeExtract"), "{text}");
        crate::tools::spirv_val_bytes(&out, &dir).expect("spirv-val");
        // an access already aligned to its size is left as it is
        assert_eq!(split_underaligned_physical_vector_accesses(&spirv_aligned(&dir, asm)), spirv_aligned(&dir, asm));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn spirv_aligned(dir: &std::path::Path, asm: &str) -> Vec<u8> {
        let src = dir.join("al.spvasm");
        let bin = dir.join("al.spv");
        std::fs::write(&src, asm.replace("Aligned 4", "Aligned 16")).unwrap();
        assert!(std::process::Command::new("spirv-as").args(["--target-env", "vulkan1.2"]).arg(&src).arg("-o").arg(&bin)
            .status().expect("spirv-as").success());
        std::fs::read(&bin).unwrap()
    }
}
