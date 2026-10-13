use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Function, Instruction, Module, Operand};
use spirv::{Decoration, MemoryAccess, Op, StorageClass, Word};

#[derive(Clone)]
enum Ty {
    Int(u32),
    Array(Word, u64),
    Struct(Vec<Word>),
}

#[derive(Clone, Default)]
struct Offset {
    konst: u64,
    terms: Vec<(Word, u64, u32)>,
}

struct Types {
    ty: HashMap<Word, Ty>,
    ptr: HashMap<Word, (StorageClass, Word)>,
    konst: HashMap<Word, u64>,
}

impl Types {
    fn read(module: &Module) -> Self {
        let mut konst = HashMap::default();
        for i in &module.types_global_values {
            if let (Op::Constant, Some(id)) = (i.class.opcode, i.result_id) {
                match i.operands.as_slice() {
                    [Operand::LiteralBit32(v)] => {
                        konst.insert(id, u64::from(*v));
                    }
                    [Operand::LiteralBit64(v)] => {
                        konst.insert(id, *v);
                    }
                    _ => {}
                }
            }
        }
        let mut ty = HashMap::default();
        let mut ptr = HashMap::default();
        for i in &module.types_global_values {
            let Some(id) = i.result_id else { continue };
            match (i.class.opcode, i.operands.as_slice()) {
                (Op::TypeInt, [Operand::LiteralBit32(w), _]) => {
                    ty.insert(id, Ty::Int(*w));
                }
                (Op::TypeArray, [Operand::IdRef(e), Operand::IdRef(n)]) => {
                    if let Some(&n) = konst.get(n) {
                        ty.insert(id, Ty::Array(*e, n));
                    }
                }
                (Op::TypeStruct, members) => {
                    let ids = members
                        .iter()
                        .map(|m| match m {
                            Operand::IdRef(m) => Some(*m),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(ids) = ids {
                        ty.insert(id, Ty::Struct(ids));
                    }
                }
                (Op::TypePointer, [Operand::StorageClass(s), Operand::IdRef(p)]) => {
                    ptr.insert(id, (*s, *p));
                }
                _ => {}
            }
        }
        Types { ty, ptr, konst }
    }

    fn byte_size(&self, t: Word) -> Option<u64> {
        match self.ty.get(&t)? {
            Ty::Int(8) => Some(1),
            Ty::Int(_) => None,
            Ty::Array(e, n) => Some(self.byte_size(*e)? * n),
            Ty::Struct(ms) => ms.iter().map(|m| self.byte_size(*m)).sum(),
        }
    }

    fn is_byte(&self, t: Word) -> bool {
        matches!(self.ty.get(&t), Some(Ty::Int(8)))
    }

    fn walk(
        &self,
        mut pointee: Word,
        indices: &[Operand],
        off: &mut Offset,
        int_width: &dyn Fn(Word) -> Option<u32>,
    ) -> Option<Word> {
        for index in indices {
            let Operand::IdRef(index) = index else {
                return None;
            };
            match self.ty.get(&pointee)? {
                Ty::Array(e, _) => {
                    let stride = self.byte_size(*e)?;
                    if let Some(c) = self.konst.get(index) {
                        off.konst += c * stride;
                    } else {
                        let width = int_width(*index)?;
                        if stride % 4 != 0 || !(width == 32 || width == 64) {
                            return None;
                        }
                        off.terms.push((*index, stride, width));
                    }
                    pointee = *e;
                }
                Ty::Struct(ms) => {
                    let c = usize::try_from(*self.konst.get(index)?).ok()?;
                    for m in ms.get(..c)? {
                        off.konst += self.byte_size(*m)?;
                    }
                    pointee = *ms.get(c)?;
                }
                Ty::Int(_) => return None,
            }
        }
        Some(pointee)
    }
}

pub(crate) fn word_back_function_byte_arrays(module: &mut Module) -> usize {
    let types = Types::read(module);
    let Some(uint) = module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == Op::TypeInt
            && i.operands == [Operand::LiteralBit32(32), Operand::LiteralBit32(0)])
        .then_some(i.result_id)
        .flatten()
    }) else {
        return 0;
    };
    let mut rewritten = 0;
    for fi in 0..module.functions.len() {
        let mut function = std::mem::take(&mut module.functions[fi]);
        rewritten += rewrite_function(module, &types, uint, &mut function);
        module.functions[fi] = function;
    }
    rewritten
}

fn is_chain(op: Op) -> bool {
    matches!(op, Op::AccessChain | Op::InBoundsAccessChain)
}

fn plain_access(extra: &[Operand]) -> bool {
    match extra {
        [] => true,
        [Operand::MemoryAccess(m), ..] => m
            .difference(MemoryAccess::ALIGNED | MemoryAccess::NONTEMPORAL)
            .is_empty(),
        _ => false,
    }
}

fn rewrite_function(
    module: &mut Module,
    types: &Types,
    uint: Word,
    function: &mut Function,
) -> usize {
    let Some(entry) = function.blocks.first() else {
        return 0;
    };
    let mut vars = HashMap::<Word, u64>::default();
    for i in &entry.instructions {
        if i.class.opcode != Op::Variable
            || i.operands != [Operand::StorageClass(StorageClass::Function)]
        {
            continue;
        }
        let (Some(id), Some(pty)) = (i.result_id, i.result_type) else {
            continue;
        };
        let Some(&(StorageClass::Function, pointee)) = types.ptr.get(&pty) else {
            continue;
        };
        if types.is_byte(pointee) {
            continue;
        }
        match types.byte_size(pointee) {
            Some(size) if size >= 4 && size % 4 == 0 => {
                vars.insert(id, size);
            }
            _ => {}
        }
    }
    if vars.is_empty() {
        return 0;
    }
    let value_types: HashMap<Word, Word> = function
        .parameters
        .iter()
        .chain(function.blocks.iter().flat_map(|b| b.instructions.iter()))
        .chain(module.types_global_values.iter())
        .filter_map(|i| Some((i.result_id?, i.result_type?)))
        .collect();
    let int_width = |id: Word| match types.ty.get(value_types.get(&id)?)? {
        Ty::Int(w) => Some(*w),
        _ => None,
    };
    let mut derived = HashMap::<Word, (Word, Offset, Word)>::default();
    for &var in vars.keys() {
        let pty = value_types[&var];
        derived.insert(var, (var, Offset::default(), types.ptr[&pty].1));
    }
    let mut bad = HashSet::<Word>::default();
    for i in function.blocks.iter().flat_map(|b| b.instructions.iter()) {
        let op = i.class.opcode;
        if op == Op::Variable && i.result_id.is_some_and(|id| vars.contains_key(&id)) {
            continue;
        }
        if is_chain(op) {
            if let Some(Operand::IdRef(base)) = i.operands.first() {
                if let Some((var, off, pointee)) = derived.get(base).cloned() {
                    let mut off = off;
                    let walked = types.walk(pointee, &i.operands[1..], &mut off, &int_width);
                    let result_ok = i
                        .result_type
                        .and_then(|t| types.ptr.get(&t))
                        .map(|(_, p)| Some(*p) == walked)
                        .unwrap_or(false);
                    match (walked, i.result_id) {
                        (Some(p), Some(id)) if result_ok => {
                            derived.insert(id, (var, off, p));
                        }
                        _ => {
                            bad.insert(var);
                        }
                    }
                    for o in &i.operands[1..] {
                        if let Operand::IdRef(x) = o {
                            if let Some((v, _, _)) = derived.get(x) {
                                bad.insert(*v);
                            }
                        }
                    }
                    continue;
                }
            }
        }
        let leaf_access = |p: &Word| derived.get(p).filter(|(_, _, t)| types.is_byte(*t));
        match (op, i.operands.as_slice()) {
            (Op::Load, [Operand::IdRef(p), extra @ ..]) if leaf_access(p).is_some() => {
                if !plain_access(extra) {
                    bad.insert(derived[p].0);
                }
                continue;
            }
            (Op::Store, [Operand::IdRef(p), Operand::IdRef(v), extra @ ..])
                if leaf_access(p).is_some() =>
            {
                if !plain_access(extra) {
                    bad.insert(derived[p].0);
                }
                if let Some((var, _, _)) = derived.get(v) {
                    bad.insert(*var);
                }
                continue;
            }
            _ => {}
        }
        for o in &i.operands {
            if let Operand::IdRef(x) = o {
                if let Some((v, _, _)) = derived.get(x) {
                    bad.insert(*v);
                }
            }
        }
    }
    vars.retain(|v, _| !bad.contains(v));
    if vars.is_empty() {
        return 0;
    }
    derived.retain(|_, (v, _, _)| vars.contains_key(v));

    let ptr_uint = pointer(module, types, StorageClass::Function, uint);
    let mut var_ptr = HashMap::<Word, Word>::default();
    for (&var, &size) in &vars {
        let arr = word_array(module, uint, size / 4);
        var_ptr.insert(var, pointer(module, types, StorageClass::Function, arr));
    }
    let byte_ty = |p: &Word| {
        derived
            .get(p)
            .filter(|(_, _, t)| types.is_byte(*t))
            .map(|d| d.2)
    };

    let mut removed = HashSet::<Word>::default();
    for block in &mut function.blocks {
        let old = std::mem::take(&mut block.instructions);
        let mut out = Vec::with_capacity(old.len());
        for mut i in old {
            let op = i.class.opcode;
            if op == Op::Variable {
                if let Some(p) = i.result_id.and_then(|id| var_ptr.get(&id)) {
                    i.result_type = Some(*p);
                }
                out.push(i);
                continue;
            }
            let Some(id) = i
                .result_id
                .filter(|id| is_chain(op) && derived.contains_key(id))
            else {
                match (op, i.operands.as_slice()) {
                    (Op::Load, [Operand::IdRef(p), ..]) if byte_ty(p).is_some() => {
                        let shift = (derived[p].1.konst % 4) as u32 * 8;
                        let word = module.fresh_id();
                        out.push(Instruction::new(
                            Op::Load,
                            Some(uint),
                            Some(word),
                            vec![Operand::IdRef(*p)],
                        ));
                        let mut value = word;
                        if shift > 0 {
                            let c = constant(module, uint, shift);
                            value = module.fresh_id();
                            out.push(Instruction::new(
                                Op::ShiftRightLogical,
                                Some(uint),
                                Some(value),
                                vec![Operand::IdRef(word), Operand::IdRef(c)],
                            ));
                        }
                        out.push(Instruction::new(
                            Op::UConvert,
                            i.result_type,
                            i.result_id,
                            vec![Operand::IdRef(value)],
                        ));
                    }
                    (Op::Store, [Operand::IdRef(p), Operand::IdRef(v), ..])
                        if byte_ty(p).is_some() =>
                    {
                        let (p, v) = (*p, *v);
                        let shift = (derived[&p].1.konst % 4) as u32 * 8;
                        let keep = constant(module, uint, !(0xffu32 << shift));
                        let (word, kept, wide) =
                            (module.fresh_id(), module.fresh_id(), module.fresh_id());
                        out.push(Instruction::new(
                            Op::Load,
                            Some(uint),
                            Some(word),
                            vec![Operand::IdRef(p)],
                        ));
                        out.push(Instruction::new(
                            Op::BitwiseAnd,
                            Some(uint),
                            Some(kept),
                            vec![Operand::IdRef(word), Operand::IdRef(keep)],
                        ));
                        out.push(Instruction::new(
                            Op::UConvert,
                            Some(uint),
                            Some(wide),
                            vec![Operand::IdRef(v)],
                        ));
                        let mut placed = wide;
                        if shift > 0 {
                            let c = constant(module, uint, shift);
                            placed = module.fresh_id();
                            out.push(Instruction::new(
                                Op::ShiftLeftLogical,
                                Some(uint),
                                Some(placed),
                                vec![Operand::IdRef(wide), Operand::IdRef(c)],
                            ));
                        }
                        let merged = module.fresh_id();
                        out.push(Instruction::new(
                            Op::BitwiseOr,
                            Some(uint),
                            Some(merged),
                            vec![Operand::IdRef(kept), Operand::IdRef(placed)],
                        ));
                        out.push(Instruction::new(
                            Op::Store,
                            None,
                            None,
                            vec![Operand::IdRef(p), Operand::IdRef(merged)],
                        ));
                    }
                    _ => out.push(i),
                }
                continue;
            };
            let (var, off, pointee) = derived[&id].clone();
            if !types.is_byte(pointee) {
                removed.insert(id);
                continue;
            }
            let mut index: Option<Word> = None;
            for (d, stride, width) in &off.terms {
                let mut term = *d;
                if *width == 64 {
                    term = module.fresh_id();
                    out.push(Instruction::new(
                        Op::UConvert,
                        Some(uint),
                        Some(term),
                        vec![Operand::IdRef(*d)],
                    ));
                }
                if stride / 4 != 1 {
                    let m = constant(module, uint, (stride / 4) as u32);
                    let narrow = term;
                    term = module.fresh_id();
                    out.push(Instruction::new(
                        Op::IMul,
                        Some(uint),
                        Some(term),
                        vec![Operand::IdRef(narrow), Operand::IdRef(m)],
                    ));
                }
                index = Some(match index {
                    None => term,
                    Some(acc) => {
                        let sum = module.fresh_id();
                        out.push(Instruction::new(
                            Op::IAdd,
                            Some(uint),
                            Some(sum),
                            vec![Operand::IdRef(acc), Operand::IdRef(term)],
                        ));
                        sum
                    }
                });
            }
            let word = (off.konst / 4) as u32;
            let index = match index {
                None => constant(module, uint, word),
                Some(acc) if word == 0 => acc,
                Some(acc) => {
                    let c = constant(module, uint, word);
                    let sum = module.fresh_id();
                    out.push(Instruction::new(
                        Op::IAdd,
                        Some(uint),
                        Some(sum),
                        vec![Operand::IdRef(acc), Operand::IdRef(c)],
                    ));
                    sum
                }
            };
            out.push(Instruction::new(
                op,
                Some(ptr_uint),
                Some(id),
                vec![Operand::IdRef(var), Operand::IdRef(index)],
            ));
        }
        block.instructions = out;
    }
    if !removed.is_empty() {
        let targets = |i: &Instruction| matches!(i.operands.first(), Some(Operand::IdRef(t)) if removed.contains(t));
        module.debug_names.retain(|i| !targets(i));
        module.annotations.retain(|i| !targets(i));
    }
    vars.len()
}

fn pointer(module: &mut Module, types: &Types, storage: StorageClass, pointee: Word) -> Word {
    if let Some((&id, _)) = types
        .ptr
        .iter()
        .find(|(_, &(s, p))| s == storage && p == pointee)
    {
        return id;
    }
    let found = module.types_global_values.iter().find(|i| {
        i.class.opcode == Op::TypePointer
            && i.operands == [Operand::StorageClass(storage), Operand::IdRef(pointee)]
    });
    if let Some(id) = found.and_then(|i| i.result_id) {
        return id;
    }
    let id = module.fresh_id();
    module.types_global_values.push(Instruction::new(
        Op::TypePointer,
        None,
        Some(id),
        vec![Operand::StorageClass(storage), Operand::IdRef(pointee)],
    ));
    id
}

fn word_array(module: &mut Module, uint: Word, words: u64) -> Word {
    let n = constant(module, uint, words as u32);
    let strided: HashSet<Word> = module
        .annotations
        .iter()
        .filter(|i| i.operands.get(1) == Some(&Operand::Decoration(Decoration::ArrayStride)))
        .filter_map(|i| match i.operands.first() {
            Some(Operand::IdRef(t)) => Some(*t),
            _ => None,
        })
        .collect();
    let found = module.types_global_values.iter().find(|i| {
        i.class.opcode == Op::TypeArray
            && i.operands == [Operand::IdRef(uint), Operand::IdRef(n)]
            && i.result_id.is_some_and(|id| !strided.contains(&id))
    });
    if let Some(id) = found.and_then(|i| i.result_id) {
        return id;
    }
    let id = module.fresh_id();
    module.types_global_values.push(Instruction::new(
        Op::TypeArray,
        None,
        Some(id),
        vec![Operand::IdRef(uint), Operand::IdRef(n)],
    ));
    id
}

fn constant(module: &mut Module, ty: Word, value: u32) -> Word {
    let found = module.types_global_values.iter().find(|i| {
        i.class.opcode == Op::Constant
            && i.result_type == Some(ty)
            && i.operands == [Operand::LiteralBit32(value)]
    });
    if let Some(id) = found.and_then(|i| i.result_id) {
        return id;
    }
    let id = module.fresh_id();
    module.types_global_values.push(Instruction::new(
        Op::Constant,
        Some(ty),
        Some(id),
        vec![Operand::LiteralBit32(value)],
    ));
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Block;

    const UCHAR: Word = 1;
    const UINT: Word = 2;
    const C0: Word = 3;
    const C5: Word = 4;
    const C8: Word = 5;
    const ARR8: Word = 6;
    const PVAR: Word = 7;
    const PBYTE: Word = 8;
    const VAR: Word = 20;
    const CHAIN: Word = 21;
    const X: Word = 22;
    const Y: Word = 23;
    const B0: Word = 9;

    fn ins(op: Op, ty: Option<Word>, id: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, ops)
    }

    fn module_with(escape: bool) -> Module {
        let mut m = Module::new();
        let lit = |v| vec![Operand::LiteralBit32(v)];
        m.types_global_values = vec![
            ins(
                Op::TypeInt,
                None,
                Some(UCHAR),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            ins(
                Op::TypeInt,
                None,
                Some(UINT),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            ins(Op::Constant, Some(UINT), Some(C0), lit(0)),
            ins(Op::Constant, Some(UINT), Some(C5), lit(5)),
            ins(Op::Constant, Some(UINT), Some(C8), lit(8)),
            ins(Op::Constant, Some(UCHAR), Some(B0), lit(0)),
            ins(
                Op::TypeArray,
                None,
                Some(ARR8),
                vec![Operand::IdRef(UCHAR), Operand::IdRef(C8)],
            ),
            ins(
                Op::TypePointer,
                None,
                Some(PVAR),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(ARR8),
                ],
            ),
            ins(
                Op::TypePointer,
                None,
                Some(PBYTE),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(UCHAR),
                ],
            ),
        ];
        let mut block = Block::new();
        block.instructions = vec![
            ins(
                Op::Variable,
                Some(PVAR),
                Some(VAR),
                vec![Operand::StorageClass(StorageClass::Function)],
            ),
            ins(
                Op::InBoundsAccessChain,
                Some(PBYTE),
                Some(CHAIN),
                vec![Operand::IdRef(VAR), Operand::IdRef(C5)],
            ),
            ins(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(CHAIN), Operand::IdRef(B0)],
            ),
            ins(Op::Load, Some(UCHAR), Some(X), vec![Operand::IdRef(CHAIN)]),
        ];
        if escape {
            block.instructions.push(ins(
                Op::CopyObject,
                Some(PBYTE),
                Some(Y),
                vec![Operand::IdRef(CHAIN)],
            ));
        }
        let mut f = Function::new();
        f.blocks = vec![block];
        m.functions = vec![f];
        m.set_id_bound(100);
        m
    }

    #[test]
    fn a_byte_array_reached_only_through_byte_chains_becomes_two_words() {
        let mut m = module_with(false);
        assert_eq!(word_back_function_byte_arrays(&mut m), 1);
        let body = &m.functions[0].blocks[0].instructions;
        let var_ty = body[0].result_type.unwrap();
        let arr = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(var_ty))
            .unwrap()
            .operands[1]
            .clone();
        let Operand::IdRef(arr) = arr else {
            panic!("pointer to an id")
        };
        let arr = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(arr))
            .unwrap();
        let Operand::IdRef(n) = arr.operands[1] else {
            panic!("array length id")
        };
        let n = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(n))
            .unwrap();
        assert_eq!(
            (arr.operands[0].clone(), n.operands[0].clone()),
            (Operand::IdRef(UINT), Operand::LiteralBit32(2)),
            "an 8-byte box must be [2 x uint]"
        );
        let chain = body.iter().find(|i| i.result_id == Some(CHAIN)).unwrap();
        let Operand::IdRef(w) = chain.operands[1] else {
            panic!("word index")
        };
        let w = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(w))
            .unwrap();
        assert_eq!(
            w.operands,
            [Operand::LiteralBit32(1)],
            "byte 5 lives in word 1"
        );
        let x = body.iter().find(|i| i.result_id == Some(X)).unwrap();
        assert_eq!(
            (x.class.opcode, x.result_type),
            (Op::UConvert, Some(UCHAR)),
            "the load keeps its id and its uchar type"
        );
        assert_eq!(
            body.iter()
                .filter(|i| i.class.opcode == Op::ShiftRightLogical)
                .count(),
            1,
            "byte 5 = word 1 >> 8"
        );
        assert!(
            body.iter().any(|i| i.class.opcode == Op::BitwiseOr),
            "the byte store merges into its word"
        );
    }

    #[test]
    fn a_byte_pointer_that_escapes_keeps_its_variable_byte_typed() {
        let mut m = module_with(true);
        let before = m.functions[0].blocks[0].instructions.clone();
        assert_eq!(word_back_function_byte_arrays(&mut m), 0);
        assert_eq!(
            m.functions[0].blocks[0].instructions, before,
            "nothing may change"
        );
    }
}
