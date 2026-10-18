use rspirv::dr::{Instruction, Module, Operand};
use rspirv::spirv::{MemoryAccess, Op, StorageClass};
use std::collections::{BTreeMap, HashMap};

fn id(o: &Operand) -> Option<u32> { if let Operand::IdRef(v) = o { Some(*v) } else { None } }

pub fn hints(m: &Module, defs: &HashMap<u32, Instruction>, sb_types: &HashMap<u32, u32>, value_types: &HashMap<u32, u32>) -> HashMap<u32, u32> {
    let is_u32 = |t: u32| defs.get(&t).is_some_and(|d| d.class.opcode == Op::TypeInt && d.operands.first() == Some(&Operand::LiteralBit32(32)));
    m.functions.iter().flat_map(|f| f.blocks.iter()).flat_map(|b| b.instructions.iter()).filter_map(|i| {
        if i.class.opcode != Op::Load { return None; }
        let pointee = i.operands.first().and_then(id).and_then(|p| value_types.get(&p)).and_then(|t| sb_types.get(t))?;
        match i.operands.as_slice() {
            [_, Operand::MemoryAccess(f), Operand::LiteralBit32(a)] if *f == MemoryAccess::ALIGNED && *a >= 8 && a.is_power_of_two()
                && is_u32(*pointee) && i.result_type == Some(*pointee) => Some((i.result_id?, *a)),
            _ => None,
        }
    }).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Leaf { Value(u32), Convert(u32, u32) }

#[derive(Default, Debug)]
struct Affine { k: u32, terms: BTreeMap<Leaf, u32> }

struct Defs<'a> { local: &'a HashMap<u32, Instruction>, consts: &'a HashMap<u32, (u32, u32)>, uint: u32 }

impl Defs<'_> {
    fn constant(&self, v: u32) -> Option<u32> { self.consts.get(&v).map(|c| c.1) }
    fn affine(&self, v: u32, scale: u32, out: &mut Affine, budget: &mut u32) -> bool {
        if *budget == 0 { return false; }
        *budget -= 1;
        if let Some(&(ty, k)) = self.consts.get(&v) {
            if ty != self.uint { return false; }
            out.k = out.k.wrapping_add(k.wrapping_mul(scale));
            return true;
        }
        let mut leaf = |l: Leaf| { let c = out.terms.entry(l).or_insert(0); *c = c.wrapping_add(scale); true };
        let Some(d) = self.local.get(&v) else { return leaf(Leaf::Value(v)) };
        if d.result_type != Some(self.uint) { return false; }
        let ops: Vec<u32> = d.operands.iter().filter_map(id).collect();
        match (d.class.opcode, ops.as_slice()) {
            (Op::IAdd, [a, b]) => self.affine(*a, scale, out, budget) && self.affine(*b, scale, out, budget),
            (Op::ISub, [a, b]) => self.affine(*a, scale, out, budget) && self.affine(*b, scale.wrapping_neg(), out, budget),
            (Op::IMul, [a, b]) => match (self.constant(*a), self.constant(*b)) {
                (Some(c), _) => self.affine(*b, scale.wrapping_mul(c), out, budget),
                (_, Some(c)) => self.affine(*a, scale.wrapping_mul(c), out, budget),
                _ => leaf(Leaf::Value(v)),
            },
            (Op::ShiftLeftLogical, [a, b]) => match self.constant(*b) {
                Some(c) if c < 32 => self.affine(*a, scale.wrapping_mul(1u32 << c), out, budget),
                _ => leaf(Leaf::Value(v)),
            },
            (Op::UConvert | Op::SConvert | Op::Bitcast, [x]) => leaf(Leaf::Convert(d.class.opcode as u32, *x)),
            _ => leaf(Leaf::Value(v)),
        }
    }
    fn differ_by(&self, lo: u32, hi: u32, delta: u32) -> bool {
        let (mut a, mut b, mut budget) = (Affine::default(), Affine::default(), 256);
        if !self.affine(lo, 1, &mut a, &mut budget) || !self.affine(hi, 1, &mut b, &mut budget) { return false; }
        for (l, c) in a.terms { let e = b.terms.entry(l).or_insert(0); *e = e.wrapping_sub(c); }
        b.terms.values().all(|c| *c == 0) && b.k.wrapping_sub(a.k) == delta
    }
    fn word_ptr(&self, p: u32) -> Option<(u32, u32, u32)> {
        let d = self.local.get(&p)?;
        if !matches!(d.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) { return None; }
        match d.operands.iter().map(id).collect::<Option<Vec<_>>>()?.as_slice() {
            [root, member, w] if self.constant(*member) == Some(0) => Some((*root, *member, *w)),
            _ => None,
        }
    }
}

fn is_pure(op: Op) -> bool {
    matches!(op, Op::IAdd | Op::ISub | Op::IMul | Op::UDiv | Op::SDiv | Op::UMod | Op::SMod | Op::SRem | Op::SNegate | Op::Not
        | Op::ShiftLeftLogical | Op::ShiftRightLogical | Op::ShiftRightArithmetic | Op::BitwiseAnd | Op::BitwiseOr | Op::BitwiseXor
        | Op::UConvert | Op::SConvert | Op::FConvert | Op::ConvertUToF | Op::ConvertSToF | Op::ConvertFToU | Op::ConvertFToS | Op::Bitcast
        | Op::ConvertPtrToU | Op::ConvertUToPtr | Op::CompositeExtract | Op::CompositeConstruct | Op::CompositeInsert | Op::VectorShuffle
        | Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::Load | Op::Select | Op::CopyObject
        | Op::IEqual | Op::INotEqual | Op::ULessThan | Op::ULessThanEqual | Op::UGreaterThan | Op::UGreaterThanEqual
        | Op::SLessThan | Op::SLessThanEqual | Op::SGreaterThan | Op::SGreaterThanEqual | Op::LogicalAnd | Op::LogicalOr | Op::LogicalNot
        | Op::FAdd | Op::FSub | Op::FMul | Op::FDiv | Op::FNegate)
}

enum Route { Byte(u32), Word }

fn find_or_push(types: &mut Vec<Instruction>, op: Op, operands: Vec<Operand>, fresh: &mut impl FnMut() -> u32) -> u32 {
    if let Some(n) = types.iter().find(|t| t.class.opcode == op && t.operands == operands).and_then(|t| t.result_id) { return n; }
    let n = fresh();
    types.push(Instruction::new(op, None, Some(n), operands));
    n
}

pub fn fuse(m: &mut Module, next: &mut u32, uint: u32, ulong: u32, hints: &HashMap<u32, u32>) -> usize {
    if hints.is_empty() { return 0; }
    let mut fresh = || { let n = *next; *next += 1; n };
    let consts: HashMap<u32, (u32, u32)> = m.types_global_values.iter().filter(|i| i.class.opcode == Op::Constant)
        .filter_map(|i| match i.operands.as_slice() { [Operand::LiteralBit32(k)] => Some((i.result_id?, (i.result_type?, *k))), _ => None }).collect();
    let mut vector = None;
    let mut fused = 0;
    for fi in 0..m.functions.len() {
        let local: HashMap<u32, Instruction> = m.functions[fi].blocks.iter().flat_map(|b| b.instructions.iter())
            .filter_map(|i| Some((i.result_id?, i.clone()))).collect();
        let d = Defs { local: &local, consts: &consts, uint };
        for bi in 0..m.functions[fi].blocks.len() {
            let body = &m.functions[fi].blocks[bi].instructions;
            let mut plan: Vec<(usize, usize, Route, u32, u32, u32)> = Vec::new();
            let mut taken = std::collections::HashSet::new();
            for (i, w0) in body.iter().enumerate() {
                if !w0.result_id.is_some_and(|r| hints.contains_key(&r)) || taken.contains(&i) || w0.result_type != Some(uint) { continue; }
                let Some(p0) = w0.operands.first().and_then(id) else { continue };
                let Some((root, member, x0)) = d.word_ptr(p0) else { continue };
                for (j, w1) in body.iter().enumerate().skip(i + 1).take(64) {
                    if !is_pure(w1.class.opcode) { break; }
                    if w1.class.opcode != Op::Load || w1.result_type != Some(uint) || taken.contains(&j) { continue; }
                    match w1.operands.as_slice() {
                        [_] => {}
                        [_, Operand::MemoryAccess(f), Operand::LiteralBit32(_)] if *f == MemoryAccess::ALIGNED => {}
                        _ => continue,
                    }
                    let Some((root1, _, x1)) = w1.operands.first().and_then(id).and_then(|p| d.word_ptr(p)) else { continue };
                    if root1 != root { continue; }
                    let div4 = |x: u32| d.local.get(&x).filter(|u| u.class.opcode == Op::UDiv && u.result_type == Some(uint))
                        .and_then(|u| match u.operands.as_slice() { [Operand::IdRef(b), Operand::IdRef(c)] if d.constant(*c) == Some(4) => Some(*b), _ => None });
                    let route = match (div4(x0), div4(x1)) {
                        (Some(b0), Some(b1)) if d.differ_by(b0, b1, 4) => Route::Byte(b0),
                        _ if d.differ_by(x0, x1, 1) => Route::Word,
                        _ => continue,
                    };
                    taken.insert(i); taken.insert(j);
                    plan.push((i, j, route, root, member, p0));
                    break;
                }
            }
            if plan.is_empty() { continue; }
            let (v2, pv2) = *vector.get_or_insert_with(|| {
                let v2 = find_or_push(&mut m.types_global_values, Op::TypeVector, vec![Operand::IdRef(uint), Operand::LiteralBit32(2)], &mut fresh);
                let pv2 = find_or_push(&mut m.types_global_values, Op::TypePointer,
                    vec![Operand::StorageClass(StorageClass::PhysicalStorageBuffer), Operand::IdRef(v2)], &mut fresh);
                (v2, pv2)
            });
            let body = std::mem::take(&mut m.functions[fi].blocks[bi].instructions);
            let seconds: std::collections::HashSet<usize> = plan.iter().map(|p| p.1).collect();
            let mut out = Vec::with_capacity(body.len() + 6 * plan.len());
            for (i, inst) in body.iter().enumerate() {
                if seconds.contains(&i) { continue; }
                let Some((_, j, route, root, member, p0)) = plan.iter().find(|p| p.0 == i) else { out.push(inst.clone()); continue };
                let address = match route {
                    Route::Byte(b0) => {
                        let (p, u, o, a) = (fresh(), fresh(), fresh(), fresh());
                        out.push(Instruction::new(Op::InBoundsAccessChain, local[p0].result_type, Some(p), vec![Operand::IdRef(*root), Operand::IdRef(*member), Operand::IdRef(*member)]));
                        out.push(Instruction::new(Op::ConvertPtrToU, Some(ulong), Some(u), vec![Operand::IdRef(p)]));
                        out.push(Instruction::new(Op::UConvert, Some(ulong), Some(o), vec![Operand::IdRef(*b0)]));
                        out.push(Instruction::new(Op::IAdd, Some(ulong), Some(a), vec![Operand::IdRef(u), Operand::IdRef(o)]));
                        a
                    }
                    Route::Word => {
                        let u = fresh();
                        out.push(Instruction::new(Op::ConvertPtrToU, Some(ulong), Some(u), vec![Operand::IdRef(*p0)]));
                        u
                    }
                };
                let (q, v) = (fresh(), fresh());
                let align = hints[&inst.result_id.unwrap()];
                out.push(Instruction::new(Op::ConvertUToPtr, Some(pv2), Some(q), vec![Operand::IdRef(address)]));
                out.push(Instruction::new(Op::Load, Some(v2), Some(v), vec![Operand::IdRef(q), Operand::MemoryAccess(MemoryAccess::ALIGNED), Operand::LiteralBit32(align)]));
                out.push(Instruction::new(Op::CompositeExtract, Some(uint), inst.result_id, vec![Operand::IdRef(v), Operand::LiteralBit32(0)]));
                out.push(Instruction::new(Op::CompositeExtract, Some(uint), body[*j].result_id, vec![Operand::IdRef(v), Operand::LiteralBit32(1)]));
                fused += 1;
            }
            m.functions[fi].blocks[bi].instructions = out;
        }
    }
    fused
}

#[cfg(test)]
mod tests {
    use crate::buffer_addresses::lower;
    use rspirv::dr::{Module, Operand};
    use rspirv::spirv::{MemoryAccess, Op};
    use spirv_tools::{assembler::Assembler, val::Validator};
    const PAIR: &str = r#"
OpCapability Shader
OpCapability Int64
OpMemoryModel Logical GLSL450
OpEntryPoint GLCompute %main "main" %buf %out %gid
OpExecutionMode %main LocalSize 1 1 1
OpDecorate %gid BuiltIn GlobalInvocationId
OpDecorate %arr ArrayStride 4
OpDecorate %block Block
OpMemberDecorate %block 0 Offset 0
OpDecorate %buf DescriptorSet 0
OpDecorate %buf Binding 0
OpDecorate %out DescriptorSet 0
OpDecorate %out Binding 1
%void = OpTypeVoid
%uint = OpTypeInt 32 0
%ulong = OpTypeInt 64 0
%v3 = OpTypeVector %uint 3
%pin = OpTypePointer Input %v3
%gid = OpVariable %pin Input
%zero = OpConstant %uint 0
%one = OpConstant %uint 1
%two = OpConstant %uint 2
%four = OpConstant %uint 4
%eight = OpConstant %uint 8
%arr = OpTypeRuntimeArray %uint
%block = OpTypeStruct %arr
%pb = OpTypePointer StorageBuffer %block
%pu = OpTypePointer StorageBuffer %uint
%buf = OpVariable %pb StorageBuffer
%out = OpVariable %pb StorageBuffer
%fn = OpTypeFunction %void
%main = OpFunction %void None %fn
%label = OpLabel
%g3 = OpLoad %v3 %gid
%g = OpCompositeExtract %uint %g3 0
%h = OpCompositeExtract %uint %g3 1
%gl = OpUConvert %ulong %g
%c0 = OpUConvert %uint %gl
%m0 = OpIMul %uint %c0 %eight
%b0 = OpIAdd %uint %zero %m0
%w0 = OpUDiv %uint %b0 %four
%p0 = OpInBoundsAccessChain %pu %buf %zero %w0
%x0 = OpLoad %uint %p0 Aligned 8
%c1 = OpUConvert %uint %gl
%m1 = OpIMul %uint %c1 %eight
%b1 = OpIAdd %uint %four %m1
%w1 = OpUDiv %uint %b1 %four
%p1 = OpInBoundsAccessChain %pu %buf %zero %w1
%x1 = OpLoad %uint %p1
%s = OpIAdd %uint %x0 %x1
%po = OpAccessChain %pu %out %zero %g
OpStore %po %s
OpReturn
OpFunctionEnd
"#;
    fn source(text: &str) -> Vec<u8> {
        spirv_tools::assembler::create(Some(spirv_tools::TargetEnv::Vulkan_1_2)).assemble(text,Default::default()).unwrap().as_bytes().to_vec()
    }
    fn validate(b: &[u8]) {
        let w:Vec<u32>=b.chunks_exact(4).map(|w|u32::from_le_bytes(w.try_into().unwrap())).collect();
        spirv_tools::val::create(Some(spirv_tools::TargetEnv::Vulkan_1_2)).validate(&w,None).unwrap();
    }
    fn census(text: &str) -> (Vec<u32>, usize) {
        let input = source(text); validate(&input);
        let output = lower(&input).unwrap(); validate(&output);
        let m: Module = rspirv::dr::load_bytes(&output).unwrap();
        let types: std::collections::HashMap<u32, rspirv::dr::Instruction> =
            m.types_global_values.iter().filter_map(|i| Some((i.result_id?, i.clone()))).collect();
        let vector = |t: Option<u32>| t.and_then(|t| types.get(&t)).is_some_and(|d| d.class.opcode == Op::TypeVector);
        let loads: Vec<_> = m.functions.iter().flat_map(|f| f.blocks.iter()).flat_map(|b| b.instructions.iter())
            .filter(|i| i.class.opcode == Op::Load).collect();
        let wide = loads.iter().filter(|i| vector(i.result_type) && i.operands.get(1) == Some(&Operand::MemoryAccess(MemoryAccess::ALIGNED)))
            .filter_map(|i| match i.operands.get(2) { Some(Operand::LiteralBit32(a)) => Some(*a), _ => None }).filter(|a| *a >= 8).collect();
        // A scalar word may keep the first load's stated alignment of 8. Count scalar uint loads by type, not by
        // alignment 4; otherwise every correctly unmerged hinted pair is misreported as a single load.
        let words = loads.iter().filter(|i| i.result_type.and_then(|t| types.get(&t)).is_some_and(|t|
            t.class.opcode == Op::TypeInt && t.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)])).count();
        (wide, words)
    }
    #[test] fn a_hinted_pair_whose_offsets_differ_by_four_is_one_aligned_8_load() {
        assert_eq!(census(PAIR), (vec![8], 0));
    }
    #[test] fn the_byte_route_leaves_no_divide_feeding_the_wide_load() {
        let m: Module = rspirv::dr::load_bytes(&lower(&source(PAIR)).unwrap()).unwrap();
        let body: Vec<_> = m.functions[0].blocks.iter().flat_map(|b| b.instructions.iter()).collect();
        let at = body.iter().position(|i| i.class.opcode == Op::ConvertUToPtr && body.iter().any(|l| l.class.opcode == Op::Load
            && l.operands.first() == Some(&Operand::IdRef(i.result_id.unwrap())) && l.operands.get(2) == Some(&Operand::LiteralBit32(8)))).unwrap();
        let add = body.iter().find(|i| Some(i.result_id) == body[at].operands.first().map(|o| if let Operand::IdRef(v) = o { Some(*v) } else { None })).unwrap();
        assert_eq!(add.class.opcode, Op::IAdd, "base + u64(byte), not a word pointer");
    }
    #[test] fn a_word_index_pair_one_apart_is_fused_through_the_first_words_pointer() {
        let t = PAIR.replace("%w0 = OpUDiv %uint %b0 %four", "%w0 = OpIAdd %uint %m0 %two")
            .replace("%w1 = OpUDiv %uint %b1 %four", "%w1a = OpIAdd %uint %m1 %two\n%w1 = OpIAdd %uint %w1a %one");
        assert_eq!(census(&t), (vec![8], 0));
    }
    #[test] fn an_unhinted_pair_is_left_as_two_aligned_4_words() {
        assert_eq!(census(&PAIR.replace("%x0 = OpLoad %uint %p0 Aligned 8", "%x0 = OpLoad %uint %p0")), (vec![], 2));
    }
    #[test] fn a_hint_of_4_is_not_a_hint() {
        assert_eq!(census(&PAIR.replace("%p0 Aligned 8", "%p0 Aligned 4")), (vec![], 2));
    }
    #[test] fn offsets_eight_apart_are_not_adjacent_words() {
        assert_eq!(census(&PAIR.replace("%b1 = OpIAdd %uint %four %m1", "%b1 = OpIAdd %uint %eight %m1")), (vec![], 2));
    }
    #[test] fn offsets_over_different_sources_are_not_proved_adjacent() {
        assert_eq!(census(&PAIR.replace("%c1 = OpUConvert %uint %gl", "%c1 = OpCopyObject %uint %h")), (vec![], 2));
    }
    #[test] fn a_store_between_the_words_blocks_the_merge() {
        assert_eq!(census(&PAIR.replace("%c1 = OpUConvert %uint %gl", "OpStore %p0 %zero\n%c1 = OpUConvert %uint %gl")), (vec![], 2));
    }
    #[test] fn words_from_two_buffers_are_not_one_access() {
        assert_eq!(census(&PAIR.replace("%p1 = OpInBoundsAccessChain %pu %buf", "%p1 = OpInBoundsAccessChain %pu %out")), (vec![], 2));
    }
    #[test] fn a_volatile_first_word_is_never_merged() {
        assert_eq!(census(&PAIR.replace("%p0 Aligned 8", "%p0 Volatile|Aligned 8")), (vec![], 2));
    }
    #[test] fn the_affine_prover_is_exact_mod_2_32_for_wrapping_offsets() {
        let t = PAIR.replace("%b0 = OpIAdd %uint %zero %m0", "%b0 = OpISub %uint %m0 %four")
            .replace("%b1 = OpIAdd %uint %four %m1", "%b1 = OpIAdd %uint %zero %m1");
        assert_eq!(census(&t), (vec![8], 0));
    }
}
