use super::*;
use crate::fxhash::HashSet;
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

pub(in crate::native) fn prune_constant_branches(module: &mut Module) -> bool {
    prune_constant_branches_impl(module, &HashSet::default(), true)
}

pub(in crate::native) fn prune_constant_cfg(module: &mut Module) -> bool {
    prune_constant_branches_impl(module, &HashSet::default(), false)
}

fn prune_constant_branches_impl(
    module: &mut Module,
    preserved_global_ids: &HashSet<Word>,
    sweep_dead_values: bool,
) -> bool {
    let scalar_int_types = scalar_int_bool_types(module);
    let lane_conditions = bool_vector_valued_ids(module);
    let consts = module_scalar_constants(module, &scalar_int_types);
    let widths = value_int_widths(module);
    let composites = module_composite_constants(module, &scalar_int_types, &consts);
    let vec_globals = compute_vector_global_consts(module, &composites);
    let global_consts = compute_global_consts(module, &consts, &widths, &vec_globals);
    let numworkgroups = numworkgroups_vars(module);
    if consts.is_empty() && global_consts.is_empty() && vec_globals.is_empty() {
        return false;
    }

    let mut any = false;
    let mut cfg_was_pruned = false;
    loop {
        let mut changed = false;
        for fi in 0..module.functions.len() {
            let mut vals = forward_eval(
                &module.functions[fi],
                &consts,
                &global_consts,
                &widths,
                &composites,
                &vec_globals,
            );
            let guards = nonzero_self_minus_one_guards(
                &module.functions[fi],
                &vals,
                &widths,
                &numworkgroups,
            );
            for (g, v) in guards {
                vals.entry(g).or_insert(v);
            }
            changed |=
                collapse_constant_selects(&mut module.functions[fi], &vals, &lane_conditions);
            if sweep_dead_values {
                changed |= fold_branches(&mut module.functions[fi], &vals);
                changed |= prune_unreachable(&mut module.functions[fi]);
                changed |= collapse_trivial_phis(&mut module.functions[fi]);
            } else {
                let folded = fold_branches(&mut module.functions[fi], &vals);
                let pruned = prune_unreachable(&mut module.functions[fi]);
                let collapsed =
                    (folded || pruned) && collapse_trivial_phis(&mut module.functions[fi]);
                changed |= folded || pruned || collapsed;
                if folded || pruned {
                    cfg_was_pruned = true;
                }
            }
        }
        if sweep_dead_values {
            changed |= dce_preserving(module, preserved_global_ids);
        }
        if sweep_dead_values || cfg_was_pruned {
            changed |= sweep_uncalled_functions(module);
        }
        any |= changed;
        if !changed {
            break;
        }
    }
    any
}

pub(in crate::native) fn sweep_uncalled_functions(module: &mut Module) -> bool {
    let fn_id = |f: &crate::spirv_module::Function| -> Option<Word> { f.def.as_ref()?.result_id };

    let mut live: HashSet<Word> = HashSet::default();
    for ep in &module.entry_points {
        if let Some(Operand::IdRef(id)) = ep.operands.get(1) {
            live.insert(*id);
        }
    }
    loop {
        let mut added = false;
        for f in &module.functions {
            let Some(id) = fn_id(f) else { continue };
            if !live.contains(&id) {
                continue;
            }
            for b in &f.blocks {
                for inst in &b.instructions {
                    if inst.class.opcode == Op::FunctionCall {
                        if let Some(Operand::IdRef(callee)) = inst.operands.first() {
                            if live.insert(*callee) {
                                added = true;
                            }
                        }
                    }
                }
            }
        }
        if !added {
            break;
        }
    }

    let mut removed_ids: HashSet<Word> = HashSet::default();
    for f in &module.functions {
        let Some(id) = fn_id(f) else { continue };
        if live.contains(&id) {
            continue;
        }
        removed_ids.insert(id);
        for p in &f.parameters {
            if let Some(r) = p.result_id {
                removed_ids.insert(r);
            }
        }
        for b in &f.blocks {
            if let Some(r) = b.label.as_ref().and_then(|l| l.result_id) {
                removed_ids.insert(r);
            }
            for inst in &b.instructions {
                if let Some(r) = inst.result_id {
                    removed_ids.insert(r);
                }
            }
        }
    }
    if removed_ids.is_empty() {
        return false;
    }

    module.functions.retain(|f| match fn_id(f) {
        Some(id) => live.contains(&id),
        None => true,
    });
    let targets_removed = |inst: &Instruction| -> bool {
        matches!(inst.operands.first(), Some(Operand::IdRef(t)) if removed_ids.contains(t))
    };
    module.debug_names.retain(|i| !targets_removed(i));
    module.annotations.retain(|i| !targets_removed(i));
    true
}
