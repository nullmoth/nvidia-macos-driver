//! Optional promotion of one AIR-identified GL default-uniform buffer.
//! Accept only read-only 32-bit word blocks with statically bounded loads. All
//! other pointers and buffers keep their original types and address lowering.
use rspirv::{binary::Assemble, dr::{Instruction, Operand}, spirv::*};
use std::collections::{BTreeSet, HashMap};

pub const BINDING: u32 = 641;
fn id(o: &Operand) -> Option<u32> { if let Operand::IdRef(n) = o { Some(*n) } else { None } }
fn lit(o: &Operand) -> Option<u32> { if let Operand::LiteralBit32(n) = o { Some(*n) } else { None } }
fn inst(op: Op, ty: Option<u32>, result: Option<u32>, args: Vec<Operand>) -> Instruction {
    Instruction::new(op, ty, result, args)
}

#[derive(Clone, Copy)]
struct ConstantInt {
    ty: u32,
    width: u32,
    signed: bool,
    bits: u64,
}
impl ConstantInt {
    fn mask(self) -> u64 {
        if self.width == 64 { u64::MAX } else { (1u64 << self.width) - 1 }
    }
    fn signed_value(self) -> i128 {
        let value = self.bits as i128;
        if self.bits & (1u64 << (self.width - 1)) != 0 { value - (1i128 << self.width) } else { value }
    }
    fn index(self) -> Option<u32> {
        if self.signed && self.signed_value() < 0 { return None; }
        u32::try_from(self.bits).ok()
    }
}

// Prove offsets from scalar integer constants only. Loads, parameters, phi
// nodes, specialization constants and unsupported operations remain unknown.
// Index expressions commonly contain function-local arithmetic and conversions,
// so the definition map must include instructions outside the global section.
struct ConstantInts<'a> {
    defs: HashMap<u32, &'a Instruction>,
    cache: HashMap<u32, Option<ConstantInt>>,
    no_wrap: HashMap<u32, u8>,
}
impl<'a> ConstantInts<'a> {
    fn new(module: &'a rspirv::dr::Module) -> Self {
        let defs = module.all_inst_iter().filter_map(|i| Some((i.result_id?, i))).collect();
        let mut no_wrap = HashMap::new();
        for annotation in &module.annotations {
            if annotation.class.opcode != Op::Decorate { continue; }
            if let [Operand::IdRef(target), Operand::Decoration(decoration)] = annotation.operands.as_slice() {
                let flag = match decoration {
                    Decoration::NoUnsignedWrap => 1,
                    Decoration::NoSignedWrap => 2,
                    _ => continue,
                };
                *no_wrap.entry(*target).or_insert(0) |= flag;
            }
        }
        Self { defs, cache: HashMap::new(), no_wrap }
    }
    fn constant(&mut self, n: u32, depth: u32) -> Option<ConstantInt> {
        if let Some(value) = self.cache.get(&n) { return *value; }
        if depth >= 128 { return None; }
        let instruction = *self.defs.get(&n)?;
        // An in-progress definition stays unknown if an invalid cyclic graph
        // reaches it again; do not recurse indefinitely on untrusted modules.
        self.cache.insert(n, None);
        let result = self.evaluate(instruction, depth);
        self.cache.insert(n, result);
        result
    }
    fn evaluate(&mut self, instruction: &Instruction, depth: u32) -> Option<ConstantInt> {
        let ty = instruction.result_type?;
        let definition = *self.defs.get(&ty)?;
        let [Operand::LiteralBit32(width), Operand::LiteralBit32(signed)] = definition.operands.as_slice() else { return None; };
        if definition.class.opcode != Op::TypeInt || !matches!(*width, 8 | 16 | 32 | 64) || *signed > 1 { return None; }
        let mut result = ConstantInt { ty, width: *width, signed: *signed != 0, bits: 0 };
        let op = instruction.class.opcode;
        result.bits = match (op, instruction.operands.as_slice()) {
            (Op::Constant, [Operand::LiteralBit32(value)]) if result.width <= 32 => *value as u64,
            (Op::Constant, [Operand::LiteralBit64(value)]) if result.width == 64 => *value,
            (Op::ConstantNull, []) => 0,
            (Op::CopyObject | Op::UConvert | Op::SConvert | Op::Bitcast | Op::SNegate | Op::Not, [Operand::IdRef(value)]) => {
                let value = self.constant(*value, depth + 1)?;
                match op {
                    Op::CopyObject if value.ty == ty => value.bits,
                    Op::UConvert => value.bits,
                    Op::SConvert => value.signed_value() as u64,
                    Op::Bitcast if value.width == result.width => value.bits,
                    Op::SNegate if value.width == result.width => {
                        if !self.arithmetic_defined(instruction.result_id?, op, value, value) { return None; }
                        0u64.wrapping_sub(value.bits)
                    }
                    Op::Not if value.width == result.width => !value.bits,
                    _ => return None,
                }
            }
            (Op::IAdd | Op::ISub | Op::IMul | Op::UDiv | Op::UMod
                | Op::ShiftLeftLogical | Op::ShiftRightLogical | Op::ShiftRightArithmetic
                | Op::BitwiseAnd | Op::BitwiseOr | Op::BitwiseXor,
                [Operand::IdRef(left), Operand::IdRef(right)]) => {
                let left = self.constant(*left, depth + 1)?;
                let right = self.constant(*right, depth + 1)?;
                if left.width != result.width { return None; }
                if matches!(op, Op::ShiftLeftLogical | Op::ShiftRightLogical | Op::ShiftRightArithmetic) {
                    // SPIR-V permits a different integer type for the shift.
                    // Negative or oversized shift counts have no proven value.
                    if right.bits >= result.width as u64 { return None; }
                    let shift = right.bits as u32;
                    match op {
                        Op::ShiftLeftLogical => {
                            if !self.arithmetic_defined(instruction.result_id?, op, left, right) { return None; }
                            left.bits << shift
                        }
                        Op::ShiftRightLogical => left.bits >> shift,
                        _ => (left.signed_value() >> shift) as u64,
                    }
                } else {
                    if right.width != result.width { return None; }
                    if matches!(op, Op::IAdd | Op::ISub | Op::IMul)
                        && !self.arithmetic_defined(instruction.result_id?, op, left, right) { return None; }
                    match op {
                        Op::IAdd => left.bits.wrapping_add(right.bits),
                        Op::ISub => left.bits.wrapping_sub(right.bits),
                        Op::IMul => left.bits.wrapping_mul(right.bits),
                        Op::UDiv if right.bits != 0 => left.bits / right.bits,
                        Op::UMod if right.bits != 0 => left.bits % right.bits,
                        Op::BitwiseAnd => left.bits & right.bits,
                        Op::BitwiseOr => left.bits | right.bits,
                        Op::BitwiseXor => left.bits ^ right.bits,
                        _ => return None,
                    }
                }
            }
            _ => return None,
        } & result.mask();
        Some(result)
    }
    fn arithmetic_defined(&self, n: u32, op: Op, left: ConstantInt, right: ConstantInt) -> bool {
        let flags = self.no_wrap.get(&n).copied().unwrap_or(0);
        if flags & 1 != 0 {
            let (a, b) = (left.bits as u128, right.bits as u128);
            let value = match op {
                Op::IAdd => Some(a + b), Op::ISub => a.checked_sub(b), Op::IMul => Some(a * b),
                Op::ShiftLeftLogical => Some(a << right.bits as u32),
                Op::SNegate => 0u128.checked_sub(a), _ => return false,
            };
            if !value.is_some_and(|v| v <= left.mask() as u128) { return false; }
        }
        if flags & 2 != 0 {
            let (a, b) = (left.signed_value(), right.signed_value());
            let value = match op {
                Op::IAdd => a + b, Op::ISub => a - b, Op::IMul => a * b,
                Op::ShiftLeftLogical => a * (1i128 << right.bits as u32),
                Op::SNegate => -a, _ => return false,
            };
            let high = 1i128 << (left.width - 1);
            if value < -high || value >= high { return false; }
        }
        true
    }
}

pub fn promote(bytes: &[u8], binding: u32, limit: u32) -> Result<(Vec<u8>, u32), String> {
    if binding >= 32 || limit < 4 { return Err("invalid GL uniform slot/range".into()); }
    let mut m = rspirv::dr::load_bytes(bytes).map_err(|e| e.to_string())?;
    let defs: HashMap<_, _> = m.types_global_values.iter()
        .filter_map(|i| Some((i.result_id?, i.clone()))).collect();
    let deco = |n, d| m.annotations.iter().find_map(|a| match a.operands.as_slice() {
        [Operand::IdRef(t), Operand::Decoration(k), rest @ ..] if *t == n && *k == d => Some(rest),
        _ => None,
    });
    if m.annotations.iter().any(|a| matches!(a.operands.as_slice(),
        [_, Operand::Decoration(Decoration::Binding), Operand::LiteralBit32(BINDING)])) {
        return Err("constant-buffer binding already occupied".into());
    }
    let roots: Vec<_> = defs.values().filter(|v| v.class.opcode == Op::Variable
        && v.operands.first() == Some(&Operand::StorageClass(StorageClass::StorageBuffer))
        && deco(v.result_id.unwrap(), Decoration::DescriptorSet) == Some(&[Operand::LiteralBit32(0)])
        && deco(v.result_id.unwrap(), Decoration::Binding) == Some(&[Operand::LiteralBit32(binding)])).collect();
    if roots.len() != 1 { return Err("GL uniform is not a single storage-buffer variable".into()); }
    let var = roots[0].clone();
    let root = var.result_id.unwrap();
    if deco(root, Decoration::NonWritable).is_none() { return Err("GL uniform lacks read-only metadata".into()); }
    let block = var.result_type.and_then(|p| defs.get(&p)).and_then(|p| p.operands.get(1)).and_then(id)
        .ok_or("GL uniform has no block type")?;
    let structure = defs.get(&block).filter(|s| s.class.opcode == Op::TypeStruct && s.operands.len() == 1)
        .ok_or("GL uniform is not a one-member block")?;
    if deco(block, Decoration::Block).is_none()
        || !m.annotations.iter().any(|a| a.operands == [
            Operand::IdRef(block), Operand::LiteralBit32(0), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(0)]) {
        return Err("GL uniform has an unsupported block layout".into());
    }
    let array = id(&structure.operands[0]).ok_or("GL uniform array missing")?;
    let arr = defs.get(&array).filter(|a| a.class.opcode == Op::TypeRuntimeArray).ok_or("GL uniform is not a word array")?;
    // Apple's GL compiler declares the words as uint or float. Both are 4-byte
    // scalars at stride 4, so word offsets and range checks are the same.
    let elem = arr.operands.first().and_then(id).ok_or("GL uniform element missing")?;
    let is_uint32 = |w: &Instruction| w.class.opcode == Op::TypeInt
        && w.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)];
    if !defs.get(&elem).is_some_and(|w| is_uint32(w)
        || (w.class.opcode == Op::TypeFloat && w.operands == [Operand::LiteralBit32(32)]))
        || deco(array, Decoration::ArrayStride) != Some(&[Operand::LiteralBit32(4)]) {
        return Err("GL uniform is not an array of 32-bit words".into());
    }
    // The array length and promoted indices are uint32 constants. They are
    // inserted before the variable, so a reused uint32 type must precede it.
    let root_position = m.types_global_values.iter().position(|i| i.result_id == Some(root)).unwrap();
    let existing_word = m.types_global_values.iter().position(|i| is_uint32(i));
    if existing_word.is_some_and(|at| at > root_position) {
        return Err("GL uniform precedes its index type".into());
    }
    let existing_word = existing_word.and_then(|at| m.types_global_values[at].result_id);
    let mut constants = ConstantInts::new(&m);
    let mut folded_indices = HashMap::new();
    let mut needed_indices = BTreeSet::new();
    // Kind 0: block, 1: word array, 2: scalar at a known word offset.
    let mut derived = HashMap::from([(root, (0u32, 0u32))]);
    loop {
        let mut grew = false;
        for i in m.functions.iter().flat_map(|f| &f.blocks).flat_map(|b| &b.instructions) {
            let Some(n) = i.result_id else { continue };
            if derived.contains_key(&n) { continue; }
            let Some(&(kind, offset)) = i.operands.first().and_then(id).and_then(|v| derived.get(&v)) else { continue };
            let state = match i.class.opcode {
                Op::CopyObject if i.operands.len() == 1 => Some((kind, offset)),
                Op::AccessChain | Op::InBoundsAccessChain => {
                    let mut k = kind; let mut off = offset;
                    for operand in &i.operands[1..] {
                        let index_id = id(operand).ok_or("invalid GL uniform index")?;
                        let value = constants.constant(index_id, 0).ok_or("dynamically indexed GL uniform")?;
                        let index = value.index().ok_or("negative or oversized GL uniform index")?;
                        folded_indices.insert(index_id, index);
                        needed_indices.insert(index);
                        match k {
                            0 if index == 0 => k = 1,
                            1 => { k = 2; off = index; }
                            _ => return Err("unsupported GL uniform access chain".into()),
                        }
                    }
                    Some((k, off))
                }
                _ => None,
            };
            if let Some(state) = state {
                let pointee = i.result_type.and_then(|t| defs.get(&t))
                    .filter(|p| p.class.opcode == Op::TypePointer && p.operands.first() == Some(&Operand::StorageClass(StorageClass::StorageBuffer)))
                    .and_then(|p| p.operands.get(1)).and_then(id);
                let expected = match state.0 { 0 => block, 1 => array, _ => elem };
                if pointee != Some(expected) { return Err("unexpected GL uniform pointer type".into()); }
                derived.insert(n, state); grew = true;
            }
        }
        if !grew { break; }
    }
    let mut extent = 0u32;
    // Check every use before changing a pointer's storage class. A global
    // initializer can capture the root too; rewriting only function accesses
    // would leave that alias with the old StorageBuffer pointer type.
    for i in m.all_inst_iter() {
        for (at, operand) in i.operands.iter().enumerate() {
            let Some(&(kind, off)) = id(operand).and_then(|v| derived.get(&v)) else { continue };
            match i.class.opcode {
                Op::Name | Op::Decorate | Op::DecorateId if at == 0 => {}
                Op::EntryPoint if at >= 3 => {}
                Op::GroupDecorate if at >= 1 => {}
                Op::AccessChain | Op::InBoundsAccessChain | Op::CopyObject
                    if at == 0 && i.result_id.is_some_and(|n| derived.contains_key(&n)) => {}
                Op::Load if at == 0 && kind == 2 && i.result_type == Some(elem) => {
                    // Volatile/non-private/availability operands require a separate memory-model review.
                    if i.operands.len() > 1 && !matches!(i.operands.get(1), Some(Operand::MemoryAccess(a)) if *a == MemoryAccess::ALIGNED) {
                        return Err("unsupported GL uniform load memory operands".into());
                    }
                    let end = off.checked_add(1).and_then(|n| n.checked_mul(4)).ok_or("GL uniform offset overflow")?;
                    if end > limit { return Err("GL uniform exceeds constant-buffer range".into()); }
                    extent = extent.max(end);
                }
                _ => return Err(format!("GL uniform pointer escapes through {:?}", i.class.opcode)),
            }
        }
    }
    if extent == 0 { return Err("GL uniform has no supported loads".into()); }
    drop(constants);
    let mut next = m.header.as_ref().ok_or("missing SPIR-V header")?.bound;
    let after_types = next.checked_add(6).ok_or("GL uniform ID space exhausted")?;
    let (count, sized, new_block, pblock, parray, pword) = (next, next+1, next+2, next+3, next+4, next+5);
    next = after_types;
    let mut types = Vec::new();
    // A float-word module may have no uint32 type; SPIR-V forbids a duplicate one.
    let word = match existing_word {
        Some(n) => n,
        None => {
            let n = next;
            next = next.checked_add(1).ok_or("GL uniform ID space exhausted")?;
            types.push(inst(Op::TypeInt, None, Some(n), vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]));
            n
        }
    };
    types.extend([
        inst(Op::Constant, Some(word), Some(count), vec![Operand::LiteralBit32(extent/4)]),
        inst(Op::TypeArray, None, Some(sized), vec![Operand::IdRef(elem), Operand::IdRef(count)]),
        inst(Op::TypeStruct, None, Some(new_block), vec![Operand::IdRef(sized)]),
        inst(Op::TypePointer, None, Some(pblock), vec![Operand::StorageClass(StorageClass::Uniform), Operand::IdRef(new_block)]),
        inst(Op::TypePointer, None, Some(parray), vec![Operand::StorageClass(StorageClass::Uniform), Operand::IdRef(sized)]),
        inst(Op::TypePointer, None, Some(pword), vec![Operand::StorageClass(StorageClass::Uniform), Operand::IdRef(elem)]),
    ]);
    // Materialize the proof as ordinary uint32 constants in the promoted
    // access chains. Keep the original expressions for all other uses.
    let mut index_constants = HashMap::new();
    for i in &m.types_global_values {
        if i.class.opcode == Op::Constant && i.result_type == Some(word) {
            if let Some(value) = i.operands.first().and_then(lit) {
                if needed_indices.contains(&value) { index_constants.entry(value).or_insert(i.result_id.unwrap()); }
            }
        }
    }
    for value in needed_indices {
        if index_constants.contains_key(&value) { continue; }
        let n = next;
        next = next.checked_add(1).ok_or("GL uniform ID space exhausted")?;
        index_constants.insert(value, n);
        types.push(inst(Op::Constant, Some(word), Some(n), vec![Operand::LiteralBit32(value)]));
    }
    let position = m.types_global_values.iter().position(|i| i.result_id == Some(root)).unwrap();
    m.types_global_values.splice(position..position, types);
    let v = m.types_global_values.iter_mut().find(|i| i.result_id == Some(root)).unwrap();
    v.result_type = Some(pblock); v.operands[0] = Operand::StorageClass(StorageClass::Uniform);
    for i in m.functions.iter_mut().flat_map(|f| &mut f.blocks).flat_map(|b| &mut b.instructions) {
        if let Some(&(kind, _)) = i.result_id.and_then(|n| derived.get(&n)) {
            i.result_type = Some(match kind { 0 => pblock, 1 => parray, _ => pword });
            if matches!(i.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                for operand in &mut i.operands[1..] {
                    let value = folded_indices[&id(operand).ok_or("invalid folded GL uniform index")?];
                    *operand = Operand::IdRef(index_constants[&value]);
                }
            }
        }
    }
    m.annotations.retain(|a| !(a.class.opcode == Op::Decorate
        && a.operands.first() == Some(&Operand::IdRef(root))
        && matches!(a.operands.get(1), Some(Operand::Decoration(Decoration::NonWritable | Decoration::Restrict | Decoration::Aliased)))));
    for a in &mut m.annotations {
        if a.class.opcode == Op::Decorate && a.operands.get(..2) == Some(&[Operand::IdRef(root), Operand::Decoration(Decoration::Binding)]) {
            a.operands[2] = Operand::LiteralBit32(BINDING);
        }
    }
    m.annotations.extend([
        inst(Op::Decorate, None, None, vec![Operand::IdRef(sized), Operand::Decoration(Decoration::ArrayStride), Operand::LiteralBit32(4)]),
        inst(Op::Decorate, None, None, vec![Operand::IdRef(new_block), Operand::Decoration(Decoration::Block)]),
        inst(Op::MemberDecorate, None, None, vec![Operand::IdRef(new_block), Operand::LiteralBit32(0), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(0)]),
    ]);
    m.header.as_mut().unwrap().bound = next;
    Ok((m.assemble().iter().flat_map(|w| w.to_le_bytes()).collect(), extent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rspirv::dr::{Block, Function, Module, ModuleHeader};

    fn module(float_words: bool) -> Module {
        let elem = if float_words { 3 } else { 2 };
        let mut m = Module::default();
        m.header = Some(ModuleHeader { magic_number: 0x07230203, version: 0x00010300,
            generator: 0, bound: 64, reserved_word: 0 });
        m.capabilities.push(inst(Op::Capability, None, None, vec![Operand::Capability(Capability::Shader)]));
        m.memory_model = Some(inst(Op::MemoryModel, None, None, vec![
            Operand::AddressingModel(AddressingModel::Logical), Operand::MemoryModel(MemoryModel::GLSL450)]));
        m.entry_points.push(inst(Op::EntryPoint, None, None, vec![
            Operand::ExecutionModel(ExecutionModel::GLCompute), Operand::IdRef(18), Operand::LiteralString("main".into())]));
        m.execution_modes.push(inst(Op::ExecutionMode, None, None, vec![Operand::IdRef(18),
            Operand::ExecutionMode(ExecutionMode::LocalSize), Operand::LiteralBit32(1), Operand::LiteralBit32(1), Operand::LiteralBit32(1)]));
        for (target, decoration, value) in [(4, Decoration::ArrayStride, Some(4)),
            (5, Decoration::Block, None), (15, Decoration::NonWritable, None),
            (15, Decoration::DescriptorSet, Some(0)), (15, Decoration::Binding, Some(7))] {
            let mut operands = vec![Operand::IdRef(target), Operand::Decoration(decoration)];
            if let Some(n) = value { operands.push(Operand::LiteralBit32(n)); }
            m.annotations.push(inst(Op::Decorate, None, None, operands));
        }
        m.annotations.push(inst(Op::MemberDecorate, None, None, vec![Operand::IdRef(5),
            Operand::LiteralBit32(0), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(0)]));
        m.types_global_values = vec![
            inst(Op::TypeVoid, None, Some(1), vec![]),
            inst(Op::TypeInt, None, Some(2), vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]),
            inst(Op::TypeFloat, None, Some(3), vec![Operand::LiteralBit32(32)]),
            inst(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(elem)]),
            inst(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            inst(Op::TypePointer, None, Some(6), vec![Operand::StorageClass(StorageClass::StorageBuffer), Operand::IdRef(5)]),
            inst(Op::TypePointer, None, Some(7), vec![Operand::StorageClass(StorageClass::StorageBuffer), Operand::IdRef(elem)]),
            inst(Op::Constant, Some(2), Some(8), vec![Operand::LiteralBit32(0)]),
            inst(Op::Constant, Some(2), Some(9), vec![Operand::LiteralBit32(3)]),
            inst(Op::TypeFunction, None, Some(10), vec![Operand::IdRef(1)]),
            inst(Op::Variable, Some(6), Some(15), vec![Operand::StorageClass(StorageClass::StorageBuffer)]),
        ];
        m.functions.push(Function {
            def: Some(inst(Op::Function, Some(1), Some(18), vec![Operand::FunctionControl(FunctionControl::NONE), Operand::IdRef(10)])),
            end: Some(inst(Op::FunctionEnd, None, None, vec![])), parameters: vec![],
            blocks: vec![Block { label: Some(inst(Op::Label, None, Some(19), vec![])), instructions: vec![
                inst(Op::AccessChain, Some(7), Some(20), vec![Operand::IdRef(15), Operand::IdRef(8), Operand::IdRef(9)]),
                inst(Op::Load, Some(elem), Some(21), vec![Operand::IdRef(20)]),
                inst(Op::Return, None, None, vec![]),
            ] }],
        });
        m
    }

    fn bytes(m: &Module) -> Vec<u8> { m.assemble().iter().flat_map(|w| w.to_le_bytes()).collect() }

    #[test]
    fn uint_and_float_words_preserve_the_load_type() {
        for float_words in [false, true] {
            let (output, extent) = promote(&bytes(&module(float_words)), 7, 65536).unwrap();
            assert_eq!(extent, 16);
            let m = rspirv::dr::load_bytes(output).unwrap();
            let variable = m.types_global_values.iter().find(|i| i.result_id == Some(15)).unwrap();
            assert_eq!(variable.operands[0], Operand::StorageClass(StorageClass::Uniform));
            let load = m.functions[0].blocks[0].instructions.iter().find(|i| i.class.opcode == Op::Load).unwrap();
            assert_eq!(load.result_type, Some(if float_words { 3 } else { 2 }));
            assert!(m.annotations.iter().any(|i| i.operands == [Operand::IdRef(15),
                Operand::Decoration(Decoration::Binding), Operand::LiteralBit32(BINDING)]));
        }
    }

    #[test]
    fn function_local_constant_arithmetic_proves_the_word_offset() {
        let mut m = module(false);
        m.types_global_values.push(inst(Op::Constant, Some(2), Some(22), vec![Operand::LiteralBit32(1352)]));
        m.types_global_values.push(inst(Op::Constant, Some(2), Some(23), vec![Operand::LiteralBit32(4)]));
        let instructions = &mut m.functions[0].blocks[0].instructions;
        instructions[0].operands[2] = Operand::IdRef(25);
        instructions.splice(0..0, [
            inst(Op::IAdd, Some(2), Some(24), vec![Operand::IdRef(22), Operand::IdRef(8)]),
            inst(Op::UDiv, Some(2), Some(25), vec![Operand::IdRef(24), Operand::IdRef(23)]),
        ]);
        let (output, extent) = promote(&bytes(&m), 7, 65536).unwrap();
        assert_eq!(extent, 1356);
        let m = rspirv::dr::load_bytes(output).unwrap();
        let access = m.functions[0].blocks[0].instructions.iter().find(|i| i.class.opcode == Op::AccessChain).unwrap();
        let index = id(&access.operands[2]).unwrap();
        assert!(m.types_global_values.iter().any(|i| i.result_id == Some(index)
            && i.class.opcode == Op::Constant && i.operands == [Operand::LiteralBit32(338)]));
    }

    #[test]
    fn float_words_can_introduce_an_unsigned_index_type() {
        let mut m = module(true);
        m.types_global_values[1].operands[1] = Operand::LiteralBit32(1);
        let (output, extent) = promote(&bytes(&m), 7, 65536).unwrap();
        assert_eq!(extent, 16);
        let m = rspirv::dr::load_bytes(output).unwrap();
        assert_eq!(m.types_global_values.iter().filter(|i| i.class.opcode == Op::TypeInt
            && i.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)]).count(), 1);
    }

    #[test]
    fn unknown_indices_keep_the_original_path() {
        let mut m = module(false);
        m.types_global_values.push(inst(Op::Undef, Some(2), Some(22), vec![]));
        m.functions[0].blocks[0].instructions[0].operands[2] = Operand::IdRef(22);
        assert!(promote(&bytes(&m), 7, 65536).unwrap_err().contains("dynamically indexed"));
    }

    #[test]
    fn global_pointer_initializers_keep_the_original_path() {
        let mut m = module(false);
        m.capabilities.push(inst(Op::Capability, None, None,
            vec![Operand::Capability(Capability::VariablePointers)]));
        m.types_global_values.push(inst(Op::TypePointer, None, Some(22),
            vec![Operand::StorageClass(StorageClass::Private), Operand::IdRef(6)]));
        m.types_global_values.push(inst(Op::Variable, Some(22), Some(23),
            vec![Operand::StorageClass(StorageClass::Private), Operand::IdRef(15)]));
        assert!(promote(&bytes(&m), 7, 65536).unwrap_err().contains("pointer escapes through Variable"));
    }

    #[test]
    fn names_and_entry_point_interfaces_do_not_escape_the_pointer() {
        let mut m = module(false);
        m.header.as_mut().unwrap().version = 0x00010500;
        m.debug_names.push(inst(Op::Name, None, None,
            vec![Operand::IdRef(15), Operand::LiteralString("gl_uniforms".into())]));
        m.entry_points[0].operands.push(Operand::IdRef(15));
        assert_eq!(promote(&bytes(&m), 7, 65536).unwrap().1, 16);
    }

    #[test]
    fn short_ranges_are_refused() {
        assert!(promote(&bytes(&module(false)), 7, 12).unwrap_err().contains("range"));
    }

    #[test]
    fn writes_and_volatile_loads_are_refused() {
        let mut m = module(false);
        m.functions[0].blocks[0].instructions.insert(2, inst(Op::Store, None, None,
            vec![Operand::IdRef(20), Operand::IdRef(21)]));
        assert!(promote(&bytes(&m), 7, 65536).unwrap_err().contains("Store"));
        let mut m = module(false);
        m.functions[0].blocks[0].instructions[1].operands.push(Operand::MemoryAccess(MemoryAccess::VOLATILE));
        assert!(promote(&bytes(&m), 7, 65536).unwrap_err().contains("memory operands"));
    }

    #[test]
    fn readonly_metadata_is_required() {
        let mut m = module(false);
        m.annotations.retain(|i| i.operands != [Operand::IdRef(15), Operand::Decoration(Decoration::NonWritable)]);
        assert!(promote(&bytes(&m), 7, 65536).unwrap_err().contains("read-only"));
    }

    #[test]
    fn invalid_slots_are_refused() {
        assert!(promote(&bytes(&module(false)), 32, 65536).is_err());
        assert!(promote(&bytes(&module(false)), 8, 65536).is_err());
    }
}
