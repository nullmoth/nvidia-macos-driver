use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

pub(in crate::native) fn scalar_int_bool_types(module: &Module) -> HashSet<Word> {
    module
        .types_global_values
        .iter()
        .filter(|i| matches!(i.class.opcode, Op::TypeInt | Op::TypeBool))
        .filter_map(|i| i.result_id)
        .collect()
}

pub(in crate::native) fn module_scalar_constants(
    module: &Module,
    int_types: &HashSet<Word>,
) -> HashMap<Word, i128> {
    let mut out = HashMap::default();
    for inst in &module.types_global_values {
        let Some(rid) = inst.result_id else { continue };
        match inst.class.opcode {
            Op::ConstantTrue => {
                out.insert(rid, 1);
            }
            Op::ConstantFalse => {
                out.insert(rid, 0);
            }
            Op::ConstantNull => {
                if inst.result_type.is_some_and(|t| int_types.contains(&t)) {
                    out.insert(rid, 0);
                }
            }
            Op::Constant => {
                if !inst.result_type.is_some_and(|t| int_types.contains(&t)) {
                    continue;
                }
                if let (Some(Operand::LiteralBit32(v)), None) =
                    (inst.operands.first(), inst.operands.get(1))
                {
                    out.insert(rid, *v as i128);
                }
            }
            _ => {}
        }
    }
    out
}

pub(in crate::native) fn value_int_widths(module: &Module) -> HashMap<Word, u32> {
    let mut type_width: HashMap<Word, u32> = HashMap::default();
    for inst in &module.types_global_values {
        if inst.class.opcode == Op::TypeInt {
            if let (Some(rid), Some(Operand::LiteralBit32(w))) =
                (inst.result_id, inst.operands.first())
            {
                type_width.insert(rid, *w);
            }
        }
    }
    let mut out: HashMap<Word, u32> = HashMap::default();
    for inst in module.types_global_values.iter().chain(
        module
            .functions
            .iter()
            .flat_map(|f| f.blocks.iter())
            .flat_map(|b| b.instructions.iter()),
    ) {
        if let (Some(rid), Some(t)) = (inst.result_id, inst.result_type) {
            if let Some(w) = type_width.get(&t) {
                out.insert(rid, *w);
            }
        }
    }
    out
}

pub(in crate::native) fn direct_global(
    inst: &Instruction,
    global_vars: &HashSet<Word>,
) -> Option<Word> {
    match inst.operands.first() {
        Some(Operand::IdRef(p)) if global_vars.contains(p) => Some(*p),
        _ => None,
    }
}

pub(in crate::native) fn compute_global_consts(
    module: &Module,
    consts: &HashMap<Word, i128>,
    widths: &HashMap<Word, u32>,
    vec_globals: &HashMap<Word, Vec<i128>>,
) -> HashMap<Word, i128> {
    let mut global_vars: HashSet<Word> = HashSet::default();
    let mut initializer: HashMap<Word, Word> = HashMap::default();
    for inst in &module.types_global_values {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let Some(rid) = inst.result_id else { continue };
        global_vars.insert(rid);
        if let Some(Operand::IdRef(init)) = inst.operands.get(1) {
            initializer.insert(rid, *init);
        }
    }

    let mut store_count: HashMap<Word, usize> = HashMap::default();
    let mut single_store: HashMap<Word, (usize, usize, usize, Word)> = HashMap::default();
    let mut load_functions: HashMap<Word, HashSet<usize>> = HashMap::default();
    for (fi, f) in module.functions.iter().enumerate() {
        for (bi, blk) in f.blocks.iter().enumerate() {
            for (ii, inst) in blk.instructions.iter().enumerate() {
                match inst.class.opcode {
                    Op::Store => {
                        if let Some(g) = direct_global(inst, &global_vars) {
                            *store_count.entry(g).or_default() += 1;
                            if let Some(Operand::IdRef(v)) = inst.operands.get(1) {
                                single_store.insert(g, (fi, bi, ii, *v));
                            }
                        }
                    }
                    Op::Load => {
                        if let Some(g) = direct_global(inst, &global_vars) {
                            load_functions.entry(g).or_default().insert(fi);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let mut gc: HashMap<Word, i128> = HashMap::default();
    for g in &global_vars {
        if store_count.get(g).copied().unwrap_or(0) == 0 {
            if let Some(init) = initializer.get(g) {
                if let Some(c) = consts.get(init) {
                    gc.insert(*g, *c);
                }
            }
        }
    }

    loop {
        let mut candidates = Vec::new();
        for (&g, &(fi, bi, ii, vid)) in &single_store {
            if gc.contains_key(&g) || store_count.get(&g).copied().unwrap_or(0) != 1 {
                continue;
            }
            let f = &module.functions[fi];
            if bi != 0 {
                continue;
            }
            let entry = &f.blocks[0];
            let load_before = entry.instructions[..ii]
                .iter()
                .any(|i| i.class.opcode == Op::Load && direct_global(i, &global_vars) == Some(g));
            if load_before {
                continue;
            }
            let loaded_elsewhere = load_functions
                .get(&g)
                .is_some_and(|functions| functions.iter().any(|&other| other != fi));
            if loaded_elsewhere {
                continue;
            }
            candidates.push((g, fi, vid));
        }
        if candidates.is_empty() {
            break;
        }
        let function_indices = candidates
            .iter()
            .map(|(_, fi, _)| *fi)
            .collect::<HashSet<_>>();
        let evaluations = function_indices
            .into_iter()
            .map(|fi| {
                (
                    fi,
                    forward_eval(
                        &module.functions[fi],
                        consts,
                        &gc,
                        widths,
                        &HashMap::default(),
                        vec_globals,
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut changed = false;
        for (g, fi, vid) in candidates {
            if let Some(c) = evaluations.get(&fi).and_then(|values| values.get(&vid)) {
                gc.insert(g, *c);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    gc
}

pub(in crate::native) fn module_composite_constants(
    module: &Module,
    int_types: &HashSet<Word>,
    consts: &HashMap<Word, i128>,
) -> HashMap<Word, Vec<i128>> {
    let mut vec_len: HashMap<Word, u32> = HashMap::default();
    for inst in &module.types_global_values {
        if inst.class.opcode == Op::TypeVector {
            if let (Some(rid), Some(Operand::IdRef(elem)), Some(Operand::LiteralBit32(n))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                if int_types.contains(elem) {
                    vec_len.insert(rid, *n);
                }
            }
        }
    }
    let mut out: HashMap<Word, Vec<i128>> = HashMap::default();
    for inst in &module.types_global_values {
        let Some(rid) = inst.result_id else { continue };
        let Some(ty) = inst.result_type else { continue };
        let Some(&n) = vec_len.get(&ty) else { continue };
        match inst.class.opcode {
            Op::ConstantNull => {
                out.insert(rid, vec![0; n as usize]);
            }
            Op::ConstantComposite => {
                let comps: Option<Vec<i128>> = inst
                    .operands
                    .iter()
                    .map(|op| match op {
                        Operand::IdRef(c) => consts.get(c).copied(),
                        _ => None,
                    })
                    .collect();
                if let Some(comps) = comps {
                    if comps.len() == n as usize {
                        out.insert(rid, comps);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(in crate::native) fn compute_vector_global_consts(
    module: &Module,
    composites: &HashMap<Word, Vec<i128>>,
) -> HashMap<Word, Vec<i128>> {
    let mut global_vars: HashSet<Word> = HashSet::default();
    let mut initializer: HashMap<Word, Word> = HashMap::default();
    for inst in &module.types_global_values {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let Some(rid) = inst.result_id else { continue };
        global_vars.insert(rid);
        if let Some(Operand::IdRef(init)) = inst.operands.get(1) {
            initializer.insert(rid, *init);
        }
    }

    let mut store_count: HashMap<Word, usize> = HashMap::default();
    let mut single_store: HashMap<Word, (usize, usize, usize, Word)> = HashMap::default();
    for (fi, f) in module.functions.iter().enumerate() {
        for (bi, blk) in f.blocks.iter().enumerate() {
            for (ii, inst) in blk.instructions.iter().enumerate() {
                if inst.class.opcode != Op::Store {
                    continue;
                }
                if let Some(g) = direct_global(inst, &global_vars) {
                    *store_count.entry(g).or_default() += 1;
                    if let Some(Operand::IdRef(v)) = inst.operands.get(1) {
                        single_store.insert(g, (fi, bi, ii, *v));
                    }
                }
            }
        }
    }

    let mut gc: HashMap<Word, Vec<i128>> = HashMap::default();
    for g in &global_vars {
        if store_count.get(g).copied().unwrap_or(0) == 0 {
            if let Some(init) = initializer.get(g) {
                if let Some(c) = composites.get(init) {
                    gc.insert(*g, c.clone());
                }
            }
        }
    }

    let mut def: HashMap<Word, &Instruction> = HashMap::default();
    for f in &module.functions {
        for b in &f.blocks {
            for inst in &b.instructions {
                if let Some(r) = inst.result_id {
                    def.insert(r, inst);
                }
            }
        }
    }

    let resolve = |vid: Word, gc: &HashMap<Word, Vec<i128>>| -> Option<Vec<i128>> {
        if let Some(c) = composites.get(&vid) {
            return Some(c.clone());
        }
        let inst = def.get(&vid)?;
        match inst.class.opcode {
            Op::Load | Op::CopyObject => match inst.operands.first() {
                Some(Operand::IdRef(p)) => gc.get(p).cloned(),
                _ => None,
            },
            _ => None,
        }
    };

    loop {
        let mut changed = false;
        for (&g, &(fi, bi, ii, vid)) in &single_store {
            if gc.contains_key(&g) || store_count.get(&g).copied().unwrap_or(0) != 1 || bi != 0 {
                continue;
            }
            let entry = &module.functions[fi].blocks[0];
            let load_before = entry.instructions[..ii]
                .iter()
                .any(|i| i.class.opcode == Op::Load && direct_global(i, &global_vars) == Some(g));
            if load_before {
                continue;
            }
            let loaded_elsewhere = module.functions.iter().enumerate().any(|(other, of)| {
                other != fi
                    && of.blocks.iter().flat_map(|b| &b.instructions).any(|i| {
                        i.class.opcode == Op::Load && direct_global(i, &global_vars) == Some(g)
                    })
            });
            if loaded_elsewhere {
                continue;
            }
            if let Some(c) = resolve(vid, &gc) {
                gc.insert(g, c);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    gc
}

pub(in crate::native) fn numworkgroups_vars(module: &Module) -> HashSet<Word> {
    module
        .annotations
        .iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::Decorate {
                return None;
            }
            let target = match inst.operands.first() {
                Some(Operand::IdRef(t)) => *t,
                _ => return None,
            };
            match inst.operands.get(1) {
                Some(Operand::Decoration(spirv::Decoration::BuiltIn)) => {
                    match inst.operands.get(2) {
                        Some(Operand::BuiltIn(spirv::BuiltIn::NumWorkgroups)) => Some(target),
                        _ => None,
                    }
                }
                _ => None,
            }
        })
        .collect()
}

pub(in crate::native) fn bool_vector_valued_ids(module: &Module) -> HashSet<Word> {
    let bool_types: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypeBool)
        .filter_map(|inst| inst.result_id)
        .collect();
    let bool_vectors: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|inst| inst.class.opcode == Op::TypeVector)
        .filter(|inst| match inst.operands.first() {
            Some(Operand::IdRef(element)) => bool_types.contains(element),
            _ => false,
        })
        .filter_map(|inst| inst.result_id)
        .collect();
    module
        .types_global_values
        .iter()
        .chain(
            module
                .functions
                .iter()
                .flat_map(|function| function.blocks.iter())
                .flat_map(|block| block.instructions.iter()),
        )
        .filter(|inst| {
            inst.result_type
                .is_some_and(|ty| bool_vectors.contains(&ty))
        })
        .filter_map(|inst| inst.result_id)
        .collect()
}
