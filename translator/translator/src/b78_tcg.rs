use crate::fxhash::{HashMap, HashSet};
use crate::meta::{BufferAccess, KernMeta, KernRole};
use crate::reflect::{DescriptorLayout, SYNTHETIC_BINDING_BASE};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Decoration, MemoryAccess, Op, StorageClass, Word};

const TABLE_SELF_BASE: u64 = 32;

fn id_of(o: &Operand) -> Option<Word> {
    if let Operand::IdRef(v) = o {
        Some(*v)
    } else {
        None
    }
}

fn global_defs(module: &Module) -> HashMap<Word, Instruction> {
    module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|n| (n, i.clone())))
        .collect()
}

fn byte_size(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let t = defs.get(&ty)?;
    match t.class.opcode {
        Op::TypeInt | Op::TypeFloat => match t.operands.first()? {
            Operand::LiteralBit32(w) if *w >= 8 && w % 8 == 0 => Some(w / 8),
            _ => None,
        },
        Op::TypeVector => {
            let n = match t.operands.get(1)? {
                Operand::LiteralBit32(n) => *n,
                _ => return None,
            };
            byte_size(defs, id_of(t.operands.first()?)?)?.checked_mul(n)
        }
        _ => None,
    }
}

fn pointer_type(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<(StorageClass, Word)> {
    let t = defs.get(&ty)?;
    if t.class.opcode != Op::TypePointer {
        return None;
    }
    match (t.operands.first()?, t.operands.get(1)?) {
        (Operand::StorageClass(sc), Operand::IdRef(p)) => Some((*sc, *p)),
        _ => None,
    }
}

fn raise_aligned(operands: &mut Vec<Operand>, at: usize, claim: u32) -> bool {
    match operands.get(at).cloned() {
        Some(Operand::MemoryAccess(flags)) if flags.contains(MemoryAccess::ALIGNED) => {
            match operands.get(at + 1) {
                Some(Operand::LiteralBit32(a)) if *a < claim => {
                    operands[at + 1] = Operand::LiteralBit32(claim);
                    true
                }
                _ => false,
            }
        }
        Some(Operand::MemoryAccess(flags)) => {
            operands[at] = Operand::MemoryAccess(flags | MemoryAccess::ALIGNED);
            operands.insert(at + 1, Operand::LiteralBit32(claim));
            true
        }
        None if operands.len() == at => {
            operands.extend([
                Operand::MemoryAccess(MemoryAccess::ALIGNED),
                Operand::LiteralBit32(claim),
            ]);
            true
        }
        _ => false,
    }
}

fn provable_address_aligns(
    module: &Module,
    defs: &HashMap<Word, Instruction>,
) -> HashMap<Word, u8> {
    const TOP: u8 = 4;
    let tz = |v: u64| {
        if v == 0 {
            TOP
        } else {
            (v.trailing_zeros() as u8).min(TOP)
        }
    };
    let consts = int_constants(module);
    let strides = decorations_of(module, Decoration::ArrayStride);
    let mut member_offsets: HashMap<(Word, u32), u32> = HashMap::default();
    for a in &module.annotations {
        if a.class.opcode != Op::MemberDecorate {
            continue;
        }
        if let (
            Some(Operand::IdRef(s)),
            Some(Operand::LiteralBit32(m)),
            Some(Operand::Decoration(Decoration::Offset)),
            Some(Operand::LiteralBit32(o)),
        ) = (
            a.operands.first(),
            a.operands.get(1),
            a.operands.get(2),
            a.operands.get(3),
        ) {
            member_offsets.insert((*s, *m), *o);
        }
    }
    let mut types: HashMap<Word, Word> = HashMap::default();
    let mut al: HashMap<Word, u8> = HashMap::default();
    for i in module.all_inst_iter() {
        if let (Some(n), Some(t)) = (i.result_id, i.result_type) {
            types.insert(n, t);
        }
    }
    for i in &module.types_global_values {
        let Some(n) = i.result_id else { continue };
        let e = match i.class.opcode {
            Op::Constant => consts.get(&n).map_or(0, |v| tz(*v)),
            Op::ConstantNull | Op::Undef | Op::Variable => TOP,
            _ => 0,
        };
        al.insert(n, e);
    }
    let local_sc = |p: Word| -> bool {
        types
            .get(&p)
            .and_then(|t| pointer_type(defs, *t))
            .is_some_and(|(sc, _)| {
                matches!(
                    sc,
                    StorageClass::Function | StorageClass::Private | StorageClass::Workgroup
                )
            })
    };
    let mut root: HashMap<Word, (Word, Vec<u32>)> = HashMap::default();
    let mut params: HashMap<Word, Vec<Word>> = HashMap::default();
    let mut calls: Vec<(Word, Vec<Word>)> = Vec::new();
    for f in &module.functions {
        if let Some(fid) = f.def.as_ref().and_then(|d| d.result_id) {
            params.insert(
                fid,
                f.parameters.iter().filter_map(|p| p.result_id).collect(),
            );
        }
        for i in f.blocks.iter().flat_map(|b| &b.instructions) {
            let Some(n) = i.result_id else { continue };
            al.insert(n, TOP);
            match i.class.opcode {
                Op::Variable if local_sc(n) => {
                    root.insert(n, (n, Vec::new()));
                }
                op if is_chain(op) || matches!(op, Op::CopyObject | Op::Bitcast) => {
                    if let Some((v, mut path)) = i
                        .operands
                        .first()
                        .and_then(id_of)
                        .and_then(|b| root.get(&b).cloned())
                    {
                        if is_chain(op) {
                            let first =
                                if matches!(op, Op::PtrAccessChain | Op::InBoundsPtrAccessChain) {
                                    2
                                } else {
                                    1
                                };
                            let whole = first == 1
                                || i.operands
                                    .get(1)
                                    .and_then(id_of)
                                    .and_then(|c| consts.get(&c))
                                    == Some(&0);
                            if whole {
                                for o in &i.operands[first.min(i.operands.len())..] {
                                    match id_of(o).and_then(|c| consts.get(&c)) {
                                        Some(c) => path.push(*c as u32),
                                        None => break,
                                    }
                                }
                            }
                        }
                        root.insert(n, (v, path));
                    }
                }
                Op::FunctionCall => {
                    let mut ops = i.operands.iter().filter_map(id_of);
                    if let Some(callee) = ops.next() {
                        calls.push((callee, ops.collect()));
                    }
                }
                _ => {}
            }
        }
        for p in &f.parameters {
            if let Some(n) = p.result_id {
                al.insert(n, TOP);
            }
        }
    }
    let mut escaped: HashSet<Word> = HashSet::default();
    for i in module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
    {
        for (k, o) in i.operands.iter().enumerate() {
            let Some((r, _)) = id_of(o).and_then(|v| root.get(&v)) else {
                continue;
            };
            let ok = k == 0
                && (matches!(
                    i.class.opcode,
                    Op::Load | Op::Store | Op::CopyObject | Op::Bitcast
                ) || is_chain(i.class.opcode));
            if !ok {
                escaped.insert(*r);
            }
        }
    }
    let mut stores: HashMap<Word, Vec<(Vec<u32>, Word)>> = HashMap::default();
    for i in module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
    {
        if i.class.opcode != Op::Store {
            continue;
        }
        if let (Some((v, path)), Some(val)) = (
            i.operands
                .first()
                .and_then(id_of)
                .and_then(|p| root.get(&p)),
            i.operands.get(1).and_then(id_of),
        ) {
            stores.entry(*v).or_default().push((path.clone(), val));
        }
    }
    let overlaps = |a: &[u32], b: &[u32]| a.iter().zip(b).all(|(x, y)| x == y);
    let fdefs: HashMap<Word, &Instruction> = module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
        .filter_map(|i| Some((i.result_id?, i)))
        .collect();
    let def_of = |o: Option<&Operand>| o.and_then(id_of).and_then(|v| fdefs.get(&v).copied());
    let is_hi = |i: &Instruction| {
        i.class.opcode == Op::ShiftLeftLogical
            && i.operands
                .get(1)
                .and_then(id_of)
                .and_then(|c| consts.get(&c))
                == Some(&32)
    };
    let is_lo = |i: &Instruction| {
        i.class.opcode == Op::UConvert
            && def_of(i.operands.first()).is_some_and(|l| {
                l.class.opcode == Op::Load
                    && l.operands
                        .first()
                        .and_then(id_of)
                        .is_some_and(|p| !root.contains_key(&p) && !local_sc(p))
            })
    };
    let addr_pairs: HashSet<Word> = fdefs
        .values()
        .filter(|i| i.class.opcode == Op::BitwiseOr)
        .filter(
            |i| match (def_of(i.operands.first()), def_of(i.operands.get(1))) {
                (Some(x), Some(y)) => (is_lo(x) && is_hi(y)) || (is_hi(x) && is_lo(y)),
                _ => false,
            },
        )
        .filter_map(|i| i.result_id)
        .collect();
    let elem_step = |ty: Word| -> Option<u8> {
        if let Some(s) = strides.get(&ty) {
            return Some(tz(*s as u64));
        }
        let t = defs.get(&ty)?;
        let elem = match t.class.opcode {
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector => id_of(t.operands.first()?)?,
            _ => ty,
        };
        byte_size(defs, elem).map(|b| tz(b as u64))
    };
    loop {
        let mut changed = false;
        for (callee, args) in &calls {
            for (p, a) in params.get(callee).into_iter().flatten().zip(args) {
                let e = al.get(a).copied().unwrap_or(0);
                if e < al[p] {
                    al.insert(*p, e);
                    changed = true;
                }
            }
        }
        for i in module
            .functions
            .iter()
            .flat_map(|f| f.blocks.iter().flat_map(|b| &b.instructions))
        {
            let a = |k: usize| {
                i.operands
                    .get(k)
                    .and_then(id_of)
                    .and_then(|v| al.get(&v).copied())
                    .unwrap_or(0)
            };
            let Some(n) = i.result_id else { continue };
            let e = match i.class.opcode {
                Op::Load => {
                    let p = i.operands.first().and_then(id_of);
                    if let Some((v, path)) = p.and_then(|p| root.get(&p)) {
                        if escaped.contains(v) {
                            0
                        } else {
                            stores
                                .get(v)
                                .into_iter()
                                .flatten()
                                .filter(|(sp, _)| overlaps(sp, path))
                                .map(|(_, val)| al.get(val).copied().unwrap_or(0))
                                .min()
                                .unwrap_or(TOP)
                        }
                    } else if p.is_some_and(&local_sc) {
                        0
                    } else {
                        let t = i.result_type.and_then(|t| defs.get(&t));
                        let base = t.is_some_and(|t| {
                            t.class.opcode == Op::TypePointer
                                || (t.class.opcode == Op::TypeInt
                                    && t.operands.first() == Some(&Operand::LiteralBit32(64)))
                        });
                        if base {
                            TOP
                        } else {
                            0
                        }
                    }
                }
                Op::Variable => TOP,
                Op::BitwiseOr if addr_pairs.contains(&n) => TOP,
                Op::IAdd | Op::ISub | Op::BitwiseOr | Op::BitwiseXor => a(0).min(a(1)),
                Op::IMul => (a(0) + a(1)).min(TOP),
                Op::BitwiseAnd => a(0).max(a(1)),
                Op::ShiftLeftLogical => match i
                    .operands
                    .get(1)
                    .and_then(id_of)
                    .and_then(|s| consts.get(&s))
                {
                    Some(s) => (a(0) as u64 + s).min(TOP as u64) as u8,
                    None => a(0),
                },
                Op::UConvert
                | Op::SConvert
                | Op::Bitcast
                | Op::CopyObject
                | Op::ConvertUToPtr
                | Op::ConvertPtrToU
                | Op::SNegate
                | Op::CopyLogical
                | Op::CompositeExtract
                | Op::VectorExtractDynamic => a(0),
                Op::Select => a(1).min(a(2)),
                Op::CompositeInsert | Op::VectorShuffle => a(0).min(a(1)),
                Op::CompositeConstruct | Op::Phi => {
                    let step = if i.class.opcode == Op::Phi { 2 } else { 1 };
                    (0..i.operands.len())
                        .step_by(step)
                        .map(a)
                        .min()
                        .unwrap_or(0)
                }
                op if is_chain(op) => {
                    let base = i.operands.first().and_then(id_of);
                    let mut e = a(0);
                    let mut ty = base
                        .and_then(|b| types.get(&b))
                        .and_then(|t| pointer_type(defs, *t))
                        .map(|(_, p)| p);
                    let mut k = 1;
                    if matches!(op, Op::PtrAccessChain | Op::InBoundsPtrAccessChain) {
                        let step = base
                            .and_then(|b| types.get(&b))
                            .and_then(|pt| strides.get(pt).map(|s| tz(*s as u64)))
                            .or_else(|| ty.and_then(elem_step));
                        e = e.min(step.map_or(0, |s| (s + a(1)).min(TOP)));
                        k = 2;
                    }
                    while k < i.operands.len() {
                        let Some(t) = ty.and_then(|t| defs.get(&t)) else {
                            e = 0;
                            break;
                        };
                        match t.class.opcode {
                            Op::TypeStruct => {
                                let m = i
                                    .operands
                                    .get(k)
                                    .and_then(id_of)
                                    .and_then(|c| consts.get(&c))
                                    .map(|m| *m as u32);
                                let off = m.and_then(|m| {
                                    member_offsets
                                        .get(&(ty.unwrap(), m))
                                        .copied()
                                        .or((m == 0).then_some(0))
                                });
                                match (m, off) {
                                    (Some(m), Some(off)) => {
                                        e = e.min(tz(off as u64));
                                        ty = t.operands.get(m as usize).and_then(id_of);
                                    }
                                    _ => {
                                        e = 0;
                                        break;
                                    }
                                }
                            }
                            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector => {
                                e = e
                                    .min(ty.and_then(elem_step).map_or(0, |s| (s + a(k)).min(TOP)));
                                ty = t.operands.first().and_then(id_of);
                            }
                            _ => {
                                e = 0;
                                break;
                            }
                        }
                        k += 1;
                    }
                    e
                }
                _ => 0,
            };
            if e < al[&n] {
                al.insert(n, e);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    al
}

pub(crate) fn apply_air_access_aligns(
    module: &mut Module,
    hints: &HashMap<Word, (u32, u32)>,
) -> usize {
    if hints.is_empty() || std::env::var_os("NVMTL_NO_AIR_ALIGN").is_some() {
        return 0;
    }
    let defs = global_defs(module);
    let proven = provable_address_aligns(module, &defs);
    let mut capped = 0;
    let mut value_types = HashMap::default();
    for i in module.all_inst_iter() {
        if let (Some(n), Some(t)) = (i.result_id, i.result_type) {
            value_types.insert(n, t);
        }
    }
    let mut changed = 0;
    for f in &mut module.functions {
        for b in &mut f.blocks {
            for i in &mut b.instructions {
                let (at, value_ty) = match i.class.opcode {
                    Op::Load => (1, i.result_type),
                    Op::Store => (
                        2,
                        i.operands
                            .get(1)
                            .and_then(id_of)
                            .and_then(|v| value_types.get(&v).copied()),
                    ),
                    _ => continue,
                };
                let Some(ptr) = i.operands.first().and_then(id_of) else {
                    continue;
                };
                let Some(&(claim, bytes)) = hints.get(&ptr) else {
                    continue;
                };
                if claim < 2 {
                    continue;
                }
                let Some((storage, pointee)) =
                    value_types.get(&ptr).and_then(|t| pointer_type(&defs, *t))
                else {
                    continue;
                };
                if !matches!(
                    storage,
                    StorageClass::StorageBuffer | StorageClass::PhysicalStorageBuffer
                ) {
                    continue;
                }
                if byte_size(&defs, pointee) != Some(bytes)
                    || value_ty.and_then(|t| byte_size(&defs, t)) != Some(bytes)
                {
                    continue;
                }
                let proof = 1u32 << proven.get(&ptr).copied().unwrap_or(0);
                if proof < claim {
                    capped += 1;
                }
                let claim = claim.min(proof);
                if claim >= 2 && raise_aligned(&mut i.operands, at, claim) {
                    changed += 1;
                }
            }
        }
    }
    if std::env::var_os("NVMTL_B78_DEBUG").is_some() {
        let usable = hints.values().filter(|h| h.0 >= 2).count();
        eprintln!("tcg A: {} hints ({usable} usable), {changed} accesses raised, {capped} claims capped by A2", hints.len());
        for (p, h) in hints.iter().filter(|(_, h)| h.0 >= 2) {
            eprintln!(
                "tcg A:   %{p} claim {} bytes {} ptr-type {:?}",
                h.0,
                h.1,
                value_types.get(p)
            );
        }
    }
    changed
}

fn decorations_of(module: &Module, deco: Decoration) -> HashMap<Word, u32> {
    module
        .annotations
        .iter()
        .filter(|a| {
            a.class.opcode == Op::Decorate && a.operands.get(1) == Some(&Operand::Decoration(deco))
        })
        .filter_map(|a| match (a.operands.first(), a.operands.get(2)) {
            (Some(Operand::IdRef(n)), Some(Operand::LiteralBit32(v))) => Some((*n, *v)),
            (Some(Operand::IdRef(n)), None) => Some((*n, 0)),
            _ => None,
        })
        .collect()
}

fn is_chain(op: Op) -> bool {
    matches!(
        op,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain | Op::InBoundsPtrAccessChain
    )
}

fn written_or_escaped(module: &Module, vars: &HashSet<Word>) -> HashSet<Word> {
    let mut root: HashMap<Word, Word> = vars.iter().map(|v| (*v, *v)).collect();
    for _ in 0..64 {
        let before = root.len();
        for f in &module.functions {
            for b in &f.blocks {
                for i in &b.instructions {
                    if !(is_chain(i.class.opcode) || i.class.opcode == Op::CopyObject) {
                        continue;
                    }
                    let base_root = i
                        .operands
                        .first()
                        .and_then(id_of)
                        .and_then(|b| root.get(&b).copied());
                    if let (Some(n), Some(r)) = (i.result_id, base_root) {
                        root.insert(n, r);
                    }
                }
            }
        }
        if root.len() == before {
            break;
        }
    }
    let mut refused = HashSet::default();
    for f in &module.functions {
        for b in &f.blocks {
            for i in &b.instructions {
                for (k, o) in i.operands.iter().enumerate() {
                    let Some(r) = id_of(o).and_then(|n| root.get(&n)) else {
                        continue;
                    };
                    let fine = match i.class.opcode {
                        Op::Load | Op::ArrayLength => k == 0,
                        op if is_chain(op) || op == Op::CopyObject => k == 0,
                        _ => false,
                    };
                    if !fine {
                        refused.insert(*r);
                    }
                }
            }
        }
    }
    refused
}

pub(crate) fn state_readonly_psb(
    module: &mut Module,
    kern: Option<&KernMeta>,
    layout: DescriptorLayout,
) -> (usize, usize, usize) {
    let logical = module
        .memory_model
        .as_ref()
        .and_then(|i| i.operands.first())
        .is_some_and(|o| matches!(o, Operand::AddressingModel(spirv::AddressingModel::Logical)));
    if std::env::var_os("NVMTL_NO_B78_READONLY").is_some() || logical {
        return (0, 0, 0);
    }
    let Some(kern) = kern else { return (0, 0, 0) };
    let sets = decorations_of(module, Decoration::DescriptorSet);
    let bindings = decorations_of(module, Decoration::Binding);
    let already = decorations_of(module, Decoration::NonWritable);
    let sb_vars: Vec<(Word, u32)> = module
        .types_global_values
        .iter()
        .filter(|i| {
            i.class.opcode == Op::Variable
                && i.operands.first() == Some(&Operand::StorageClass(StorageClass::StorageBuffer))
        })
        .filter_map(|i| {
            let n = i.result_id?;
            (sets.get(&n) == Some(&layout.set)).then_some(())?;
            Some((n, *bindings.get(&n)?))
        })
        .collect();
    let constant_bindings: HashSet<u32> = kern
        .roles
        .iter()
        .filter_map(|(idx, role)| match role {
            KernRole::Buffer(loc)
                if kern.buffer_address_space(*idx) == Some(2)
                    && kern.buffer_accesses.get(idx) == Some(&BufferAccess::ReadOnly) =>
            {
                layout.buffer_binding(*loc)
            }
            _ => None,
        })
        .collect();
    let table_vars: HashSet<Word> = sb_vars
        .iter()
        .filter(|(_, b)| *b == SYNTHETIC_BINDING_BASE)
        .map(|(n, _)| *n)
        .collect();
    let refused = written_or_escaped(module, &sb_vars.iter().map(|(n, _)| *n).collect());
    let refused_bindings: HashSet<u32> = sb_vars
        .iter()
        .filter(|(n, _)| refused.contains(n))
        .map(|(_, b)| *b)
        .collect();
    let mut decorate = Vec::new();
    let mut d1 = 0;
    for (n, b) in &sb_vars {
        if constant_bindings.contains(b)
            && !refused_bindings.contains(b)
            && !already.contains_key(n)
        {
            decorate.push(*n);
            d1 += 1;
        }
    }
    let (mut d2, mut ml3) = (0, 0);
    if !table_vars.is_empty() && !refused_bindings.contains(&SYNTHETIC_BINDING_BASE) {
        match table_self_derived(module, &table_vars) {
            Ok(wrap) => {
                for n in &table_vars {
                    if !already.contains_key(n) {
                        decorate.push(*n);
                        d2 += 1;
                    }
                }
                ml3 = wrap_nonwritable(module, &wrap);
            }
            Err(reason) => {
                if std::env::var_os("NVMTL_B78_DEBUG").is_some() {
                    eprintln!("tcg: table read-only refused: {reason}");
                }
            }
        }
    }
    for n in decorate {
        module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(n),
                Operand::Decoration(Decoration::NonWritable),
            ],
        ));
    }
    (d1, d2, ml3)
}

fn int_constants(module: &Module) -> HashMap<Word, u64> {
    module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Constant)
        .filter_map(|i| {
            let v = match i.operands.first()? {
                Operand::LiteralBit32(v) => *v as u64,
                Operand::LiteralBit64(v) => *v,
                _ => return None,
            };
            Some((i.result_id?, v))
        })
        .collect()
}

fn table_self_derived(
    module: &Module,
    table_vars: &HashSet<Word>,
) -> Result<HashSet<Word>, String> {
    let consts = int_constants(module);
    let mut defs: HashMap<Word, &Instruction> = HashMap::default();
    let mut uses: HashMap<Word, Vec<(&Instruction, usize)>> = HashMap::default();
    let mut fn_vars = HashSet::default();
    for f in &module.functions {
        for b in &f.blocks {
            for i in &b.instructions {
                if let Some(n) = i.result_id {
                    defs.insert(n, i);
                    if i.class.opcode == Op::Variable
                        && i.operands.first()
                            == Some(&Operand::StorageClass(StorageClass::Function))
                    {
                        fn_vars.insert(n);
                    }
                }
                for (k, o) in i.operands.iter().enumerate() {
                    if let Some(n) = id_of(o) {
                        uses.entry(n).or_default().push((i, k));
                    }
                }
            }
        }
    }
    let path_of = |mut p: Word| -> Option<(Word, Vec<u64>)> {
        let mut rev = Vec::new();
        for _ in 0..16 {
            if fn_vars.contains(&p) {
                rev.reverse();
                return Some((p, rev.concat()));
            }
            let d = defs.get(&p)?;
            if d.class.opcode == Op::CopyObject {
                p = id_of(d.operands.first()?)?;
                continue;
            }
            if !matches!(d.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                return None;
            }
            let idx = d.operands[1..]
                .iter()
                .map(|o| id_of(o).and_then(|c| consts.get(&c).copied()))
                .collect::<Option<Vec<_>>>()?;
            rev.push(idx);
            p = id_of(d.operands.first()?)?;
        }
        None
    };
    let mut poisoned: HashSet<Word> = HashSet::default();
    let mut stores: HashMap<(Word, Vec<u64>), Vec<Word>> = HashMap::default();
    let mut field_loads: HashMap<Word, (Word, Vec<u64>)> = HashMap::default();
    let mut paths: HashMap<Word, HashSet<Vec<u64>>> = HashMap::default();
    for (&v, &d) in &defs {
        if fn_vars.contains(&v) && d.operands.len() > 1 {
            poisoned.insert(v);
        }
    }
    for (&n, &d) in &defs {
        if d.class.opcode == Op::Load {
            if let Some(p) = d.operands.first().and_then(id_of) {
                if let Some(fp) = path_of(p) {
                    paths.entry(fp.0).or_default().insert(fp.1.clone());
                    field_loads.insert(n, fp);
                }
            }
        }
    }
    let root_var = |mut p: Word| -> Option<Word> {
        for _ in 0..16 {
            if fn_vars.contains(&p) {
                return Some(p);
            }
            let d = defs.get(&p)?;
            if !is_chain(d.class.opcode) && d.class.opcode != Op::CopyObject {
                return None;
            }
            p = id_of(d.operands.first()?)?;
        }
        None
    };
    for f in &module.functions {
        for b in &f.blocks {
            for i in &b.instructions {
                for (k, o) in i.operands.iter().enumerate() {
                    let Some(n) = id_of(o) else { continue };
                    let Some(v) = root_var(n) else { continue };
                    let Some((_, path)) = path_of(n) else {
                        poisoned.insert(v);
                        continue;
                    };
                    match (i.class.opcode, k) {
                        (Op::Load, 0) => {}
                        (Op::AccessChain | Op::InBoundsAccessChain | Op::CopyObject, 0) => {}
                        (Op::Store, 0) => {
                            paths.entry(v).or_default().insert(path.clone());
                            if let Some(val) = i.operands.get(1).and_then(id_of) {
                                stores.entry((v, path)).or_default().push(val);
                            }
                        }
                        _ => {
                            poisoned.insert(v);
                        }
                    }
                }
            }
        }
    }
    for (v, ps) in &paths {
        if ps
            .iter()
            .any(|a| ps.iter().any(|b| a.len() < b.len() && b.starts_with(a)))
        {
            poisoned.insert(*v);
        }
    }
    let table_source = |d: &Instruction| -> bool {
        if d.class.opcode != Op::Load {
            return false;
        }
        let Some(c) = d
            .operands
            .first()
            .and_then(id_of)
            .and_then(|p| defs.get(&p))
        else {
            return false;
        };
        is_chain(c.class.opcode)
            && c.operands
                .first()
                .and_then(id_of)
                .is_some_and(|b| table_vars.contains(&b))
            && c.operands
                .get(1)
                .and_then(id_of)
                .and_then(|x| consts.get(&x))
                == Some(&0)
            && c.operands
                .get(2)
                .and_then(id_of)
                .and_then(|x| consts.get(&x))
                .is_some_and(|k| *k >= TABLE_SELF_BASE)
    };
    let propagates = |op: Op| {
        matches!(
            op,
            Op::UConvert
                | Op::SConvert
                | Op::Bitcast
                | Op::CopyObject
                | Op::CompositeExtract
                | Op::ShiftLeftLogical
                | Op::ShiftRightLogical
                | Op::BitwiseOr
                | Op::IAdd
                | Op::ISub
                | Op::Phi
                | Op::Select
        )
    };
    let mut derived: HashSet<Word> = defs
        .iter()
        .filter(|(n, d)| {
            table_source(d)
                || field_loads
                    .get(n)
                    .is_some_and(|fp| !poisoned.contains(&fp.0) && stores.contains_key(fp))
                || propagates(d.class.opcode)
        })
        .map(|(n, _)| *n)
        .collect();
    let rule = |n: Word, set: &HashSet<Word>, grounded: bool| -> bool {
        let d = defs[&n];
        if table_source(d) {
            return true;
        }
        let ids = |r: std::ops::Range<usize>| {
            d.operands
                .get(r)
                .unwrap_or(&[])
                .iter()
                .filter_map(id_of)
                .collect::<Vec<_>>()
        };
        let all = |v: &[Word]| !v.is_empty() && v.iter().all(|x| set.contains(x));
        let any = |v: &[Word]| v.iter().any(|x| set.contains(x));
        if let Some(fp) = field_loads.get(&n) {
            let vals = stores.get(fp).map(|v| v.as_slice()).unwrap_or(&[]);
            return if grounded { any(vals) } else { all(vals) };
        }
        match d.class.opcode {
            Op::UConvert
            | Op::SConvert
            | Op::Bitcast
            | Op::CopyObject
            | Op::CompositeExtract
            | Op::ShiftLeftLogical
            | Op::ShiftRightLogical
            | Op::ISub => all(&ids(0..1)),
            Op::BitwiseOr => {
                if grounded {
                    any(&ids(0..2))
                } else {
                    all(&ids(0..2))
                }
            }
            Op::IAdd => any(&ids(0..2)),
            Op::Phi => {
                let inc: Vec<Word> = d.operands.iter().step_by(2).filter_map(id_of).collect();
                if grounded {
                    any(&inc)
                } else {
                    all(&inc)
                }
            }
            Op::Select => {
                if grounded {
                    any(&ids(1..3))
                } else {
                    all(&ids(1..3))
                }
            }
            _ => false,
        }
    };
    loop {
        let drop: Vec<Word> = derived
            .iter()
            .copied()
            .filter(|n| !rule(*n, &derived, false))
            .collect();
        if drop.is_empty() {
            break;
        }
        for n in drop {
            derived.remove(&n);
        }
    }
    let mut grounded: HashSet<Word> = HashSet::default();
    loop {
        let add: Vec<Word> = derived
            .iter()
            .copied()
            .filter(|n| !grounded.contains(n) && rule(*n, &grounded, true))
            .collect();
        if add.is_empty() {
            break;
        }
        grounded.extend(add);
    }
    let derived = grounded;
    let mut wrap = HashSet::default();
    for n in &derived {
        for (u, k) in uses.get(n).map(|v| v.as_slice()).unwrap_or(&[]) {
            let op = u.class.opcode;
            let ok = match op {
                _ if u.result_id.is_some_and(|r| derived.contains(&r)) => true,
                Op::IEqual
                | Op::INotEqual
                | Op::ULessThan
                | Op::ULessThanEqual
                | Op::UGreaterThan
                | Op::UGreaterThanEqual => true,
                Op::Store if *k == 1 => u
                    .operands
                    .first()
                    .and_then(id_of)
                    .and_then(&path_of)
                    .is_some_and(|fp| !poisoned.contains(&fp.0)),
                Op::ConvertUToPtr => {
                    let p = u.result_id.unwrap_or(0);
                    let only_loaded = uses
                        .get(&p)
                        .map(|v| v.as_slice())
                        .unwrap_or(&[])
                        .iter()
                        .all(|(l, j)| l.class.opcode == Op::Load && *j == 0);
                    if !only_loaded {
                        return Err(format!(
                            "table address %{n} becomes pointer %{p} that is not only loaded"
                        ));
                    }
                    wrap.insert(p);
                    true
                }
                _ => false,
            };
            if !ok {
                return Err(format!("table address %{n} used by {op:?} operand {k}"));
            }
        }
    }
    Ok(wrap)
}

fn wrap_nonwritable(module: &mut Module, wrap: &HashSet<Word>) -> usize {
    if wrap.is_empty() {
        return 0;
    }
    let defs = global_defs(module);
    let plain = |t: Word| {
        defs.get(&t).is_some_and(|d| match d.class.opcode {
            Op::TypeInt | Op::TypeFloat => true,
            Op::TypeVector => {
                matches!(d.operands.get(1), Some(Operand::LiteralBit32(2..=4)))
                    && d.operands
                        .first()
                        .and_then(id_of)
                        .and_then(|e| defs.get(&e))
                        .is_some_and(|e| matches!(e.class.opcode, Op::TypeInt | Op::TypeFloat))
            }
            _ => false,
        })
    };
    let uint = defs.iter().find_map(|(&n, i)| {
        (i.class.opcode == Op::TypeInt
            && i.operands == vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)])
        .then_some(n)
    });
    let zero = uint.and_then(|u| {
        defs.iter().find_map(|(&n, i)| {
            (i.class.opcode == Op::Constant
                && i.result_type == Some(u)
                && i.operands == vec![Operand::LiteralBit32(0)])
            .then_some(n)
        })
    });
    let uint = uint.unwrap_or_else(|| {
        let n = module.fresh_id();
        module.types_global_values.push(Instruction::new(
            Op::TypeInt,
            None,
            Some(n),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ));
        n
    });
    let zero = zero.unwrap_or_else(|| {
        let n = module.fresh_id();
        module.types_global_values.push(Instruction::new(
            Op::Constant,
            Some(uint),
            Some(n),
            vec![Operand::LiteralBit32(0)],
        ));
        n
    });
    let mut wrappers: HashMap<Word, Word> = HashMap::default();
    let mut changed = 0;
    for fi in 0..module.functions.len() {
        for bi in 0..module.functions[fi].blocks.len() {
            let mut out = Vec::with_capacity(module.functions[fi].blocks[bi].instructions.len());
            let insts = std::mem::take(&mut module.functions[fi].blocks[bi].instructions);
            for i in insts {
                let hit = i.class.opcode == Op::ConvertUToPtr
                    && i.result_id.is_some_and(|n| wrap.contains(&n));
                let pointee = i
                    .result_type
                    .and_then(|t| pointer_type(&defs, t))
                    .filter(|(sc, p)| *sc == StorageClass::PhysicalStorageBuffer && plain(*p));
                let (true, Some((_, t))) = (hit, pointee) else {
                    out.push(i);
                    continue;
                };
                let pw = match wrappers.get(&t) {
                    Some(p) => *p,
                    None => {
                        let (w, pw) = (module.fresh_id(), module.fresh_id());
                        module.types_global_values.push(Instruction::new(
                            Op::TypeStruct,
                            None,
                            Some(w),
                            vec![Operand::IdRef(t)],
                        ));
                        module.types_global_values.push(Instruction::new(
                            Op::TypePointer,
                            None,
                            Some(pw),
                            vec![
                                Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                                Operand::IdRef(w),
                            ],
                        ));
                        module.annotations.extend([
                            Instruction::new(
                                Op::Decorate,
                                None,
                                None,
                                vec![Operand::IdRef(w), Operand::Decoration(Decoration::Block)],
                            ),
                            Instruction::new(
                                Op::MemberDecorate,
                                None,
                                None,
                                vec![
                                    Operand::IdRef(w),
                                    Operand::LiteralBit32(0),
                                    Operand::Decoration(Decoration::Offset),
                                    Operand::LiteralBit32(0),
                                ],
                            ),
                            Instruction::new(
                                Op::MemberDecorate,
                                None,
                                None,
                                vec![
                                    Operand::IdRef(w),
                                    Operand::LiteralBit32(0),
                                    Operand::Decoration(Decoration::NonWritable),
                                ],
                            ),
                        ]);
                        wrappers.insert(t, pw);
                        pw
                    }
                };
                let c = module.fresh_id();
                out.push(Instruction::new(
                    Op::ConvertUToPtr,
                    Some(pw),
                    Some(c),
                    i.operands.clone(),
                ));
                out.push(Instruction::new(
                    Op::AccessChain,
                    i.result_type,
                    i.result_id,
                    vec![Operand::IdRef(c), Operand::IdRef(zero)],
                ));
                changed += 1;
            }
            module.functions[fi].blocks[bi].instructions = out;
        }
    }
    changed
}
