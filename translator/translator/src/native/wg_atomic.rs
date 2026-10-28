use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Op, StorageClass, Word};
use std::collections::BTreeMap;

pub(super) fn construct_workgroup_atomic_floats(module: &mut Module) -> bool {
    let float_ty = match scalar_type(module, Op::TypeFloat, 32) {
        Some(t) => t,
        None => return false,
    };
    let int32_types: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| {
            i.class.opcode == Op::TypeInt && i.operands.first() == Some(&Operand::LiteralBit32(32))
        })
        .filter_map(|i| i.result_id)
        .collect();
    if int32_types.is_empty() {
        return false;
    }

    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut type_defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in &module.types_global_values {
        if let Some(result) = inst.result_id {
            type_defs.insert(result, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut cands: Vec<(Word, Word)> = Vec::new();
    for inst in &module.types_global_values {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let (Some(var), Some(ptr_ty)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let Some(&(StorageClass::Workgroup, pointee)) = ptr_info.get(&ptr_ty) else {
            continue;
        };
        if tree_contains_float(&type_defs, pointee, float_ty, &mut HashSet::default()) {
            cands.push((var, pointee));
        }
    }
    if cands.is_empty() {
        return false;
    }

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut changed = false;
    for (var, pointee) in cands {
        if remodel_one(module, var, pointee, float_ty, &int32_types, &mut next_id) {
            changed = true;
        }
    }
    if changed {
        if let Some(header) = module.header.as_mut() {
            header.bound = next_id;
        }
    }
    changed
}

fn tree_contains_float(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    float_ty: Word,
    seen: &mut HashSet<Word>,
) -> bool {
    if ty == float_ty {
        return true;
    }
    if !seen.insert(ty) {
        return false;
    }
    let Some(def) = defs.get(&ty) else {
        return false;
    };
    match def.class.opcode {
        Op::TypeArray => operand_id(def, 0)
            .map(|elem| tree_contains_float(defs, elem, float_ty, seen))
            .unwrap_or(false),
        Op::TypeStruct => def.operands.iter().any(
            |o| matches!(o, Operand::IdRef(f) if tree_contains_float(defs, *f, float_ty, seen)),
        ),
        _ => false,
    }
}

struct RemodelPlan {
    int_ty: Word,
    chain_pointee: BTreeMap<Word, Word>,
    float_chain_ids: HashSet<Word>,
    bitcast_ids: HashSet<Word>,
    bitcast_to_chain: HashMap<Word, Word>,
    var: Word,
    null_init: bool,
}

fn remodel_one(
    module: &mut Module,
    var: Word,
    pointee: Word,
    float_ty: Word,
    int32_types: &HashSet<Word>,
    next_id: &mut Word,
) -> bool {
    let Some(plan) = validate(module, var, pointee, float_ty, int32_types) else {
        return false;
    };

    let mut fresh = || {
        let id = *next_id;
        *next_id += 1;
        id
    };
    let mut memo: HashMap<Word, Word> = HashMap::default();
    let mut new_types: Vec<Instruction> = Vec::new();
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    let Some(new_pointee) = clone_f2i(
        &type_defs,
        pointee,
        float_ty,
        plan.int_ty,
        &mut memo,
        &mut new_types,
        &mut fresh,
    ) else {
        return false;
    };

    let new_null = plan.null_init.then(|| {
        let id = fresh();
        new_types.push(Instruction::new(
            Op::ConstantNull,
            Some(new_pointee),
            Some(id),
            vec![],
        ));
        id
    });

    let cloned_of = |p: Word| -> Word {
        if p == float_ty {
            plan.int_ty
        } else {
            *memo.get(&p).unwrap_or(&p)
        }
    };

    let mut wg_ptr_cache: HashMap<Word, Word> = HashMap::default();
    let mut wg_ptr_to = |pointee: Word,
                         module: &Module,
                         new_types: &mut Vec<Instruction>,
                         fresh: &mut dyn FnMut() -> Word|
     -> Word {
        if let Some(&id) = wg_ptr_cache.get(&pointee) {
            return id;
        }
        let id = find_ptr(module, StorageClass::Workgroup, pointee).unwrap_or_else(|| {
            let id = fresh();
            new_types.push(Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(pointee),
                ],
            ));
            id
        });
        wg_ptr_cache.insert(pointee, id);
        id
    };

    let new_var_ptr = wg_ptr_to(new_pointee, module, &mut new_types, &mut fresh);
    let mut chain_new_ptr: HashMap<Word, Word> = HashMap::default();
    for (&chain_id, &orig_pointee) in &plan.chain_pointee {
        let new_pointee = cloned_of(orig_pointee);
        let ptr = wg_ptr_to(new_pointee, module, &mut new_types, &mut fresh);
        chain_new_ptr.insert(chain_id, ptr);
    }

    let Some(var_pos) = module
        .types_global_values
        .iter()
        .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
    else {
        return false;
    };
    module.types_global_values[var_pos].result_type = Some(new_var_ptr);
    let tail = module.types_global_values.split_off(var_pos);
    module.types_global_values.extend(new_types);
    module.types_global_values.extend(tail);

    for func in module.functions.iter_mut() {
        for block in func.blocks.iter_mut() {
            for inst in block.instructions.iter_mut() {
                if let Some(&ptr) = chain_new_ptr.get(&inst.result_id.unwrap_or(0)) {
                    inst.result_type = Some(ptr);
                }
            }
        }
    }

    rewrite_bodies(module, &plan, new_null, next_id);
    true
}

fn validate(
    module: &Module,
    var: Word,
    pointee: Word,
    float_ty: Word,
    int32_types: &HashSet<Word>,
) -> Option<RemodelPlan> {
    let pointee_null_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::ConstantNull && i.result_type == Some(pointee))
        .filter_map(|i| i.result_id)
        .collect();
    let ptr_info: HashMap<Word, (StorageClass, Word)> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::TypePointer)
        .filter_map(|i| {
            let id = i.result_id?;
            match (i.operands.first()?, i.operands.get(1)?) {
                (Operand::StorageClass(s), Operand::IdRef(p)) => Some((id, (*s, *p))),
                _ => None,
            }
        })
        .collect();

    let mut chain_pointee: BTreeMap<Word, Word> = BTreeMap::new();
    let mut float_chain_ids: HashSet<Word> = HashSet::default();
    let mut roots: HashSet<Word> = HashSet::default();
    roots.insert(var);
    loop {
        let mut added = false;
        for func in &module.functions {
            for block in &func.blocks {
                for inst in &block.instructions {
                    let Some(result_id) = inst.result_id else {
                        continue;
                    };
                    if chain_pointee.contains_key(&result_id) {
                        continue;
                    }
                    if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                        continue;
                    }
                    let Some(base) = operand_id(inst, 0) else {
                        continue;
                    };
                    if !roots.contains(&base) {
                        continue;
                    }
                    let result_ty = inst.result_type?;
                    let &(sc, pointee) = ptr_info.get(&result_ty)?;
                    if sc != StorageClass::Workgroup {
                        return None;
                    }
                    chain_pointee.insert(result_id, pointee);
                    roots.insert(result_id);
                    if pointee == float_ty {
                        float_chain_ids.insert(result_id);
                    }
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    let all_chain_ids: HashSet<Word> = chain_pointee.keys().copied().collect();
    if float_chain_ids.is_empty() {
        return None;
    }

    let mut null_init = false;
    for func in &module.functions {
        for block in &func.blocks {
            for inst in &block.instructions {
                let is_chain_base =
                    matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                        && operand_id(inst, 0) == Some(var);
                if is_chain_base {
                    continue;
                }
                if inst.class.opcode == Op::Store
                    && operand_id(inst, 0) == Some(var)
                    && operand_id(inst, 1).is_some_and(|v| pointee_null_ids.contains(&v))
                {
                    null_init = true;
                    continue;
                }
                if inst
                    .operands
                    .iter()
                    .any(|o| matches!(o, Operand::IdRef(id) if *id == var))
                {
                    return None;
                }
            }
        }
    }

    let mut bitcast_ids: HashSet<Word> = HashSet::default();
    let mut bitcast_to_chain: HashMap<Word, Word> = HashMap::default();
    let mut int_ty: Option<Word> = None;
    for func in &module.functions {
        for block in &func.blocks {
            for inst in &block.instructions {
                if inst.class.opcode != Op::Bitcast {
                    continue;
                }
                let Some(src) = operand_id(inst, 0) else {
                    continue;
                };
                if !all_chain_ids.contains(&src) {
                    continue;
                }
                let result_ty = inst.result_type?;
                let &(sc, leaf) = ptr_info.get(&result_ty)?;
                if sc != StorageClass::Workgroup || !int32_types.contains(&leaf) {
                    return None;
                }
                match int_ty {
                    Some(t) if t != leaf => return None,
                    _ => int_ty = Some(leaf),
                }
                bitcast_ids.insert(inst.result_id?);
                bitcast_to_chain.insert(inst.result_id?, src);
            }
        }
    }
    let int_ty = match int_ty {
        Some(t) => t,
        None => {
            return None;
        }
    };

    for func in &module.functions {
        for block in &func.blocks {
            for inst in &block.instructions {
                for (oi, op) in inst.operands.iter().enumerate() {
                    let Operand::IdRef(id) = op else { continue };
                    if all_chain_ids.contains(id) {
                        let ok = match inst.class.opcode {
                            Op::InBoundsAccessChain | Op::AccessChain => oi == 0,
                            Op::Bitcast => bitcast_ids.contains(&inst.result_id.unwrap_or(0)),
                            Op::Load => oi == 0,
                            Op::Store => oi == 0,
                            op if is_atomic(op) => oi == 0,
                            _ => false,
                        };
                        if !ok {
                            return None;
                        }
                    }
                    if bitcast_ids.contains(id) && !(is_atomic(inst.class.opcode) && oi == 0) {
                        return None;
                    }
                }
            }
        }
    }

    Some(RemodelPlan {
        int_ty,
        chain_pointee,
        float_chain_ids,
        bitcast_ids,
        bitcast_to_chain,
        var,
        null_init,
    })
}

fn rewrite_bodies(
    module: &mut Module,
    plan: &RemodelPlan,
    new_null: Option<Word>,
    next_id: &mut Word,
) {
    let int_ty = plan.int_ty;
    for func in module.functions.iter_mut() {
        for block in func.blocks.iter_mut() {
            let insts = block.instructions.clone();
            let mut out = Vec::with_capacity(insts.len());
            for mut inst in insts {
                if let (Op::Store, Some(new_null)) = (inst.class.opcode, new_null) {
                    if operand_id(&inst, 0) == Some(plan.var) {
                        inst.operands[1] = Operand::IdRef(new_null);
                        out.push(inst);
                        continue;
                    }
                }
                if inst.class.opcode == Op::Bitcast
                    && inst
                        .result_id
                        .map(|r| plan.bitcast_ids.contains(&r))
                        .unwrap_or(false)
                {
                    continue;
                }
                if inst.class.opcode == Op::Load {
                    if let Some(ptr) = operand_id(&inst, 0) {
                        if plan.float_chain_ids.contains(&ptr) && inst.result_type != Some(int_ty) {
                            let (rt, rid) = (inst.result_type.unwrap(), inst.result_id.unwrap());
                            let tmp = *next_id;
                            *next_id += 1;
                            out.push(Instruction::new(
                                Op::Load,
                                Some(int_ty),
                                Some(tmp),
                                vec![Operand::IdRef(ptr)],
                            ));
                            out.push(Instruction::new(
                                Op::Bitcast,
                                Some(rt),
                                Some(rid),
                                vec![Operand::IdRef(tmp)],
                            ));
                            continue;
                        }
                    }
                }
                if inst.class.opcode == Op::Store {
                    if let Some(ptr) = operand_id(&inst, 0) {
                        if plan.float_chain_ids.contains(&ptr) {
                            let fval = operand_id(&inst, 1).unwrap();
                            let tmp = *next_id;
                            *next_id += 1;
                            out.push(Instruction::new(
                                Op::Bitcast,
                                Some(int_ty),
                                Some(tmp),
                                vec![Operand::IdRef(fval)],
                            ));
                            out.push(Instruction::new(
                                Op::Store,
                                None,
                                None,
                                vec![Operand::IdRef(ptr), Operand::IdRef(tmp)],
                            ));
                            continue;
                        }
                    }
                }
                for op in inst.operands.iter_mut() {
                    if let Operand::IdRef(id) = op {
                        if let Some(&c) = plan.bitcast_to_chain.get(id) {
                            *op = Operand::IdRef(c);
                        }
                    }
                }
                out.push(inst);
            }
            block.instructions = out;
        }
    }
}

fn clone_f2i(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    float_ty: Word,
    int_ty: Word,
    memo: &mut HashMap<Word, Word>,
    new_types: &mut Vec<Instruction>,
    fresh: &mut dyn FnMut() -> Word,
) -> Option<Word> {
    if ty == float_ty {
        return Some(int_ty);
    }
    if let Some(&m) = memo.get(&ty) {
        return Some(m);
    }
    let def = defs.get(&ty)?;
    match def.class.opcode {
        Op::TypeArray => {
            let elem = operand_id(def, 0)?;
            let len = match def.operands.get(1)? {
                Operand::IdRef(c) => *c,
                _ => return None,
            };
            let new_elem = clone_f2i(defs, elem, float_ty, int_ty, memo, new_types, fresh)?;
            if new_elem == elem {
                memo.insert(ty, ty);
                return Some(ty);
            }
            let id = fresh();
            new_types.push(Instruction::new(
                Op::TypeArray,
                None,
                Some(id),
                vec![Operand::IdRef(new_elem), Operand::IdRef(len)],
            ));
            memo.insert(ty, id);
            Some(id)
        }
        Op::TypeStruct => {
            let mut fields: Vec<Word> = Vec::new();
            let mut any_changed = false;
            for o in &def.operands {
                let Operand::IdRef(f) = o else { return None };
                let nf = clone_f2i(defs, *f, float_ty, int_ty, memo, new_types, fresh)?;
                any_changed |= nf != *f;
                fields.push(nf);
            }
            if !any_changed {
                memo.insert(ty, ty);
                return Some(ty);
            }
            let id = fresh();
            new_types.push(Instruction::new(
                Op::TypeStruct,
                None,
                Some(id),
                fields.into_iter().map(Operand::IdRef).collect(),
            ));
            memo.insert(ty, id);
            Some(id)
        }
        _ => {
            memo.insert(ty, ty);
            Some(ty)
        }
    }
}

fn scalar_type(module: &Module, op: Op, bits: u32) -> Option<Word> {
    module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == op && i.operands.first() == Some(&Operand::LiteralBit32(bits)))
            .then_some(i.result_id)
            .flatten()
    })
}

fn find_ptr(module: &Module, sc: StorageClass, pointee: Word) -> Option<Word> {
    module.types_global_values.iter().find_map(|i| {
        (i.class.opcode == Op::TypePointer
            && i.operands.first() == Some(&Operand::StorageClass(sc))
            && i.operands.get(1) == Some(&Operand::IdRef(pointee)))
        .then_some(i.result_id)
        .flatten()
    })
}

fn operand_id(inst: &Instruction, idx: usize) -> Option<Word> {
    match inst.operands.get(idx) {
        Some(Operand::IdRef(id)) => Some(*id),
        _ => None,
    }
}

fn is_atomic(op: Op) -> bool {
    matches!(
        op,
        Op::AtomicSMin
            | Op::AtomicSMax
            | Op::AtomicUMin
            | Op::AtomicUMax
            | Op::AtomicIAdd
            | Op::AtomicISub
            | Op::AtomicAnd
            | Op::AtomicOr
            | Op::AtomicXor
            | Op::AtomicExchange
            | Op::AtomicCompareExchange
            | Op::AtomicLoad
            | Op::AtomicStore
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};

    fn i(op: Op, ty: Option<Word>, res: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, res, ops)
    }

    #[test]
    fn nested_workgroup_float_atomic_remodels_to_uint() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            i(
                Op::TypeFloat,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32)],
            ),
            i(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            i(
                Op::Constant,
                Some(2),
                Some(3),
                vec![Operand::LiteralBit32(2)],
            ),
            i(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(1)]),
            i(
                Op::TypeArray,
                None,
                Some(6),
                vec![Operand::IdRef(5), Operand::IdRef(3)],
            ),
            i(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(6),
                ],
            ),
            i(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(5),
                ],
            ),
            i(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(1),
                ],
            ),
            i(
                Op::TypePointer,
                None,
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(2),
                ],
            ),
            i(
                Op::Constant,
                Some(2),
                Some(11),
                vec![Operand::LiteralBit32(0)],
            ),
            i(
                Op::Constant,
                Some(2),
                Some(12),
                vec![Operand::LiteralBit32(1)],
            ),
            i(
                Op::Constant,
                Some(2),
                Some(13),
                vec![Operand::LiteralBit32(7)],
            ),
            i(
                Op::Variable,
                Some(7),
                Some(20),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            i(Op::Undef, Some(9), Some(14), vec![]),
        ];
        let mut block = Block::new();
        block.label = Some(i(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            i(
                Op::InBoundsAccessChain,
                Some(8),
                Some(31),
                vec![Operand::IdRef(20), Operand::IdRef(11)],
            ),
            i(
                Op::InBoundsAccessChain,
                Some(9),
                Some(32),
                vec![Operand::IdRef(31), Operand::IdRef(11)],
            ),
            i(Op::Bitcast, Some(10), Some(33), vec![Operand::IdRef(32)]),
            i(
                Op::AtomicSMin,
                Some(2),
                Some(34),
                vec![
                    Operand::IdRef(33),
                    Operand::IdRef(12),
                    Operand::IdRef(11),
                    Operand::IdRef(13),
                ],
            ),
            i(
                Op::InBoundsAccessChain,
                Some(9),
                Some(35),
                vec![Operand::IdRef(20), Operand::IdRef(11), Operand::IdRef(11)],
            ),
            i(
                Op::Phi,
                Some(9),
                Some(36),
                vec![
                    Operand::IdRef(14),
                    Operand::IdRef(30),
                    Operand::IdRef(35),
                    Operand::IdRef(30),
                ],
            ),
            i(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.blocks = vec![block];
        m.functions = vec![func];

        assert!(
            !construct_workgroup_atomic_floats(&mut m),
            "a dead pointer escape must not weaken the constructor's all-uses gate"
        );
        assert!(crate::native::eliminate_dead_pointer_values_module(
            &mut m,
            &HashSet::default()
        ));
        assert!(construct_workgroup_atomic_floats(&mut m));
        assert!(
            !construct_workgroup_atomic_floats(&mut m),
            "construction must close the complete Workgroup float-as-int atomic graph"
        );

        let body = &m.functions[0].blocks[0].instructions;
        assert!(!body.iter().any(|x| x.result_id == Some(33)));
        let atomic = body.iter().find(|x| x.result_id == Some(34)).unwrap();
        assert_eq!(atomic.operands.first(), Some(&Operand::IdRef(32)));
        let leaf = body.iter().find(|x| x.result_id == Some(32)).unwrap();
        let leaf_ptr = leaf.result_type.unwrap();
        let pointee = m
            .types_global_values
            .iter()
            .find(|x| x.result_id == Some(leaf_ptr))
            .and_then(|x| x.operands.get(1));
        assert_eq!(pointee, Some(&Operand::IdRef(2)));
        let var = m
            .types_global_values
            .iter()
            .find(|x| x.result_id == Some(20))
            .unwrap();
        assert_ne!(var.result_type, Some(7));
        assert!(!m.functions[0].blocks[0]
            .instructions
            .iter()
            .any(|x| x.class.opcode == Op::Bitcast));
    }

    #[test]
    fn workgroup_float_with_foreign_use_is_left_untouched() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(30));
        m.types_global_values = vec![
            i(
                Op::TypeFloat,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32)],
            ),
            i(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            i(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(1),
                ],
            ),
            i(
                Op::Variable,
                Some(7),
                Some(20),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(i(Op::Label, None, Some(25), vec![]));
        block.instructions = vec![
            i(Op::Load, Some(1), Some(26), vec![Operand::IdRef(20)]),
            i(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.blocks = vec![block];
        m.functions = vec![func];

        assert!(!construct_workgroup_atomic_floats(&mut m));
    }
}
