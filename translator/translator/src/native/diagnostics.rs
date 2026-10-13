use super::*;

pub fn structured_reject_reasons(san_ll: &str) -> Result<Vec<Option<String>>, String> {
    let parsed = LlModule::parse(san_ll)?;
    Ok(parsed
        .functions
        .iter()
        .map(|f| {
            let blocks = cfg::lower_unstructured_switches(&f.blocks);
            cfg::structured_reject_reason(&blocks)
        })
        .collect())
}

pub fn cond_other_witness_report(san_ll: &str) -> Result<Vec<String>, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut out = Vec::new();
    for (index, f) in parsed.functions.iter().enumerate() {
        let blocks = cfg::lower_unstructured_switches(&f.blocks);
        let witnesses = cfg::cond_other_witness_lines(&blocks);
        for witness in witnesses {
            out.push(format!("fn={index} name={} {witness}", f.name));
        }
    }
    Ok(out)
}

pub fn straddle_witness_report(san_ll: &str) -> Result<Vec<String>, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut out = Vec::new();
    for (index, f) in parsed.functions.iter().enumerate() {
        let blocks = cfg::lower_unstructured_switches(&f.blocks);
        let witnesses = cfg::straddle_witness_lines(&blocks);
        for witness in witnesses {
            out.push(format!("fn={index} name={} {witness}", f.name));
        }
    }
    Ok(out)
}

pub fn straddle_region_report(san_ll: &str) -> Result<Vec<String>, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut out = Vec::new();
    for (index, f) in parsed.functions.iter().enumerate() {
        let blocks = cfg::lower_unstructured_switches(&f.blocks);
        let reject_reason = cfg::structured_reject_reason(&blocks);
        let source_reason = reject_reason.clone().unwrap_or_else(|| "ADMIT".to_string());
        match cfg::renest_straddle_loop_merge(&blocks, reject_reason.as_deref()) {
            Ok(Some(candidate)) => {
                let candidate_status = cfg::construct_tree_reject_reason(&candidate)
                    .unwrap_or_else(|| "ADMIT".to_string());
                out.push(format!(
                    "fn={index} name={} source={} candidate=some blocks={} status={}",
                    f.name,
                    source_reason,
                    candidate.len(),
                    candidate_status
                ));
                for witness in cfg::construct_tree_gate_witness_lines(&candidate) {
                    out.push(format!(
                        "fn={index} name={} candidate-gate {witness}",
                        f.name
                    ));
                }
                for witness in cfg::cond_other_witness_lines(&candidate) {
                    out.push(format!(
                        "fn={index} name={} candidate-followup {witness}",
                        f.name
                    ));
                }
                for witness in cfg::cond_phi_shared_witness_lines(&candidate) {
                    out.push(format!(
                        "fn={index} name={} candidate-followup {witness}",
                        f.name
                    ));
                }
            }
            Ok(None) => out.push(format!(
                "fn={index} name={} source={} candidate=none",
                f.name, source_reason
            )),
            Err(error) => out.push(format!(
                "fn={index} name={} source={} candidate=decline reason={error}",
                f.name, source_reason
            )),
        }
    }
    Ok(out)
}

pub fn structured_reject_loop_classes(
    san_ll: &str,
) -> Result<Vec<Option<(String, &'static str)>>, String> {
    let parsed = LlModule::parse(san_ll)?;
    Ok(parsed
        .functions
        .iter()
        .map(|f| {
            let blocks = cfg::lower_unstructured_switches(&f.blocks);
            let reason = cfg::structured_reject_reason(&blocks)?;
            let forest = cfg::loopforest::analyze(&blocks);
            let parent_of: HashMap<&str, Option<&str>> = forest
                .loops
                .iter()
                .map(|l| (l.header.as_str(), l.parent.as_deref()))
                .collect();
            let max_depth = parent_of
                .keys()
                .map(|h| {
                    let mut d = 1usize;
                    let mut cur = *h;
                    while let Some(Some(p)) = parent_of.get(cur) {
                        d += 1;
                        cur = p;
                    }
                    d
                })
                .max()
                .unwrap_or(0);
            let category = match max_depth {
                0 => "loop-free",
                1 => "flat-loops",
                2 => "nested-loops-d2",
                _ => "nested-loops-d3+",
            };
            Some((reason, category))
        })
        .collect())
}

pub fn tir_gep_pointee_report(
    san_ll: &str,
) -> Result<Vec<(String, String, Option<String>)>, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut out = Vec::new();
    for f in &parsed.functions {
        let split = f.blocks.clone();
        let Ok(tir) = tir::build_from_blocks(&split) else {
            continue;
        };
        for tb in &tir.blocks {
            for inst in &tb.insts {
                let Some(result) = &inst.result else { continue };
                let is_gep = inst.opcode == "getelementptr";
                if is_gep {
                    let pointee = tir.pointer_pointees.get(result).map(|p| format!("{p:?}"));
                    out.push((f.name.clone(), result.clone(), pointee));
                }
            }
        }
    }
    Ok(out)
}

pub fn irreducible_region_report(
    san_ll: &str,
) -> Result<Vec<(String, Vec<(usize, usize)>)>, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut out = Vec::new();
    for f in &parsed.functions {
        let blocks = cfg::lower_unstructured_switches(&f.blocks);
        let regions = cfg::loopforest::irreducible_regions(&blocks);
        if !regions.is_empty() {
            out.push((
                f.name.clone(),
                regions
                    .iter()
                    .map(|r| (r.nodes.len(), r.entries.len()))
                    .collect(),
            ));
        }
    }
    Ok(out)
}

pub fn tir_self_check(san_ll: &str) -> Result<TirCheckStats, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut stats = TirCheckStats::default();
    for f in &parsed.functions {
        stats.functions += 1;
        let split = f.blocks.clone();
        let tir = match tir::build_from_blocks(&split) {
            Ok(t) => t,
            Err(_) => {
                stats.build_errors += 1;
                continue;
            }
        };
        accumulate_tir_soundness(&tir, f, &parsed.types, &mut stats);
    }
    Ok(stats)
}

fn operand_type_compatible(def: &ir::LlType, used: &ir::LlType) -> bool {
    use ir::LlType::{Bool, Int, Ptr, Vector};
    match (def, used) {
        _ if def == used => true,
        (Bool, Int(1)) | (Int(1), Bool) => true,
        (Ptr(_), Ptr(_)) => true,
        (Vector(d, dn), Vector(u, un)) if dn == un => operand_type_compatible(d, u),
        _ => false,
    }
}

fn accumulate_tir_soundness(
    tir: &tir::TirFunction,
    f: &ir::LlFunction,
    parsed_types: &HashMap<String, ir::LlType>,
    stats: &mut TirCheckStats,
) {
    stats.values_typed += tir.value_types.len();
    let (use_resolved, use_beyond_gep, use_conflicts) = tir::use_pointee_coverage(tir);
    stats.use_pointees_resolved += use_resolved;
    stats.use_pointee_beyond_gep += use_beyond_gep;
    stats.use_pointee_conflicts += use_conflicts;
    let mut defined: HashSet<&str> = parsed_types.keys().map(String::as_str).collect();
    for (p, _) in &f.params {
        defined.insert(p.as_str());
    }
    for tb in &tir.blocks {
        for inst in &tb.insts {
            if let Some(r) = &inst.result {
                defined.insert(r.as_str());
            }
        }
    }
    for tb in &tir.blocks {
        for inst in &tb.insts {
            inst.visit_uses(|u| {
                if !defined.contains(u) {
                    stats.dangling_uses += 1;
                }
            });
        }
    }
    let param_types: HashMap<&str, &ir::LlType> =
        f.params.iter().map(|(n, t)| (n.as_str(), t)).collect();
    for tb in &tir.blocks {
        for inst in &tb.insts {
            for op in &inst.operands {
                stats.operands_total += 1;
                match op {
                    tir::TirOperand::Unresolved => {
                        if crate::env_vars::tir_dbg() {
                            eprintln!("TIR-UNRESOLVED-OP {}", inst.opcode);
                        }
                    }
                    tir::TirOperand::Const { .. } => stats.operands_resolved += 1,
                    tir::TirOperand::Value { name, ty } => {
                        stats.operands_resolved += 1;
                        let def_ty = param_types
                            .get(name.as_str())
                            .copied()
                            .or_else(|| tir.value_types.get(name.as_str()));
                        if let Some(def_ty) = def_ty {
                            stats.operand_value_defs_checked += 1;
                            if !operand_type_compatible(def_ty, ty) {
                                stats.operand_type_mismatches += 1;
                                if crate::env_vars::tir_dbg() {
                                    eprintln!("TIR-OPTYPE {name} def={def_ty:?} use={ty:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for tb in &tir.blocks {
        for inst in &tb.insts {
            if let Some(result) = &inst.result {
                stats.values_total += 1;
                if inst.result_ty.is_none() && crate::env_vars::tir_dbg() {
                    eprintln!("TIR-UNTYPED {}", inst.opcode);
                }
                let is_gep = inst.opcode == "getelementptr";
                if is_gep {
                    stats.gep_results += 1;
                    if tir.pointer_pointees.contains_key(result) {
                        stats.gep_pointees_resolved += 1;
                    }
                }
            }
        }
    }
}

pub fn tir_structured_self_check(san_ll: &str) -> Result<TirCheckStats, String> {
    let parsed = LlModule::parse(san_ll)?;
    let mut stats = TirCheckStats::default();
    for f in &parsed.functions {
        stats.functions += 1;
        let split = f.blocks.clone();
        let mut body_blocks = cfg::lower_unstructured_switches(&split);
        if let Some(plan) = cfg::structured_plan(&body_blocks) {
            body_blocks = plan.blocks;
        }
        let tir = match tir::build_from_blocks(&body_blocks) {
            Ok(t) => t,
            Err(_) => {
                stats.build_errors += 1;
                continue;
            }
        };
        accumulate_tir_soundness(&tir, f, &parsed.types, &mut stats);
    }
    Ok(stats)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TirCheckStats {
    pub functions: usize,
    pub values_typed: usize,
    pub values_total: usize,
    pub term_mismatches: usize,
    pub build_errors: usize,
    pub dangling_uses: usize,
    pub gep_results: usize,
    pub gep_pointees_resolved: usize,
    pub operands_total: usize,
    pub operands_resolved: usize,
    pub operand_type_mismatches: usize,
    pub operand_value_defs_checked: usize,
    pub use_pointees_resolved: usize,
    pub use_pointee_beyond_gep: usize,
    pub use_pointee_conflicts: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StorageCheckStats {
    pub functions: usize,
    pub emitter_values: usize,
    pub agree: usize,
    pub diverge: usize,
    pub tir_missing: usize,
    pub logical_values: usize,
    pub logical_agree: usize,
    pub private_values: usize,
}

pub fn tir_storage_check(san_ll: &str) -> Result<StorageCheckStats, String> {
    let parsed = LlModule::parse(san_ll)?;
    let snapshots = Emitter::new(parsed.clone()).emit_collecting_storage()?;
    let snap_by_fn: HashMap<&str, &HashMap<String, StorageClass>> =
        snapshots.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let mut stats = StorageCheckStats::default();
    for f in &parsed.functions {
        let Some(emitter_map) = snap_by_fn.get(f.name.as_str()) else {
            continue;
        };
        let split = f.blocks.clone();
        let mut body_blocks = cfg::lower_unstructured_switches(&split);
        if let Some(plan) = cfg::structured_plan(&body_blocks) {
            body_blocks = plan.blocks;
        }
        let tir = match tir::build_from_blocks(&body_blocks) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let derived = tir::derive_pointer_storage(&tir, &f.params, &parsed.types);
        stats.functions += 1;
        for (name, emitter_storage) in emitter_map.iter() {
            stats.emitter_values += 1;
            let is_logical = *emitter_storage != StorageClass::Private;
            if is_logical {
                stats.logical_values += 1;
            } else {
                stats.private_values += 1;
            }
            match derived.get(name) {
                Some(d) if d == emitter_storage => {
                    stats.agree += 1;
                    if is_logical {
                        stats.logical_agree += 1;
                    }
                }
                Some(d) => {
                    stats.diverge += 1;
                    if crate::env_vars::storage_dbg() {
                        eprintln!(
                            "STORAGE-DIVERGE {} {name} emitter={emitter_storage:?} tir={d:?}",
                            f.name
                        );
                    }
                }
                None => {
                    stats.tir_missing += 1;
                    if crate::env_vars::storage_dbg() {
                        eprintln!(
                            "STORAGE-MISSING {} {name} emitter={emitter_storage:?}",
                            f.name
                        );
                    }
                }
            }
        }
    }
    Ok(stats)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PointeeCheckStats {
    pub functions: usize,
    pub emitter_values: usize,
    pub agree: usize,
    pub diverge: usize,
    pub carrier_missing: usize,
    pub byte_placeholder: usize,
    pub carrier_upgrades: usize,
}

pub fn tir_pointee_check(san_ll: &str) -> Result<PointeeCheckStats, String> {
    let parsed = LlModule::parse(san_ll)?;
    let snapshots = Emitter::new(parsed.clone()).emit_collecting_pointees()?;
    let snap_by_fn: HashMap<&str, &HashMap<String, ir::LlType>> =
        snapshots.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let mut stats = PointeeCheckStats::default();
    for f in &parsed.functions {
        let Some(emitter_map) = snap_by_fn.get(f.name.as_str()) else {
            continue;
        };
        let split = f.blocks.clone();
        let mut body_blocks = cfg::lower_unstructured_switches(&split);
        if let Some(plan) = cfg::structured_plan(&body_blocks) {
            body_blocks = plan.blocks;
        }
        let tir = match tir::build_from_blocks(&body_blocks) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let carrier = &tir.use_pointees;
        stats.functions += 1;
        for (name, emitter_pointee) in emitter_map.iter() {
            stats.emitter_values += 1;
            let emitter_pointee = parsed.resolve_known_type(emitter_pointee);
            let is_byte = emitter_pointee == ir::LlType::Int(8);
            if is_byte {
                stats.byte_placeholder += 1;
            }
            match carrier.get(name).map(|c| parsed.resolve_known_type(c)) {
                Some(c) if operand_type_compatible(&emitter_pointee, &c) => stats.agree += 1,
                Some(c) => {
                    stats.diverge += 1;
                    if is_byte && c != ir::LlType::Int(8) {
                        stats.carrier_upgrades += 1;
                    }
                    if crate::env_vars::pointee_dbg() {
                        eprintln!(
                            "POINTEE-DIVERGE {} {name} emitter={emitter_pointee:?} carrier={c:?}",
                            f.name
                        );
                    }
                }
                None => {
                    stats.carrier_missing += 1;
                    if crate::env_vars::pointee_dbg() {
                        eprintln!(
                            "POINTEE-MISSING {} {name} emitter={emitter_pointee:?}",
                            f.name
                        );
                    }
                }
            }
        }
    }
    Ok(stats)
}

#[derive(Default)]
pub struct ParamPointeeStats {
    pub functions: usize,
    pub sidecar_values: usize,
    pub agree: usize,
    pub diverge: usize,
    pub use_missing: usize,
}

pub fn param_pointee_check(san_ll: &str) -> Result<ParamPointeeStats, String> {
    let parsed = LlModule::parse(san_ll)?;
    let use_site = &parsed.ptr_pointees;
    let sidecar = Emitter::new(parsed.clone()).emit_collecting_param_pointees()?;
    let mut stats = ParamPointeeStats::default();
    let mut fns_counted: HashSet<&str> = HashSet::default();
    for ((fn_name, idx), call_pointee) in &sidecar {
        let Some(func) = parsed.functions.iter().find(|f| &f.name == fn_name) else {
            continue;
        };
        let Some((param_name, _)) = func.params.get(*idx) else {
            continue;
        };
        stats.sidecar_values += 1;
        fns_counted.insert(fn_name.as_str());
        match use_site.get(&(fn_name.clone(), param_name.clone())) {
            Some(use_pointee) if use_pointee == call_pointee => stats.agree += 1,
            Some(use_pointee) => {
                stats.diverge += 1;
                if crate::env_vars::param_pointee_dbg() {
                    eprintln!(
                        "PARAM-POINTEE-DIVERGE {fn_name} #{idx} {param_name} call={call_pointee:?} use={use_pointee:?}"
                    );
                }
            }
            None => {
                stats.use_missing += 1;
                if crate::env_vars::param_pointee_dbg() {
                    eprintln!(
                        "PARAM-POINTEE-USE-MISSING {fn_name} #{idx} {param_name} call={call_pointee:?}"
                    );
                }
            }
        }
    }
    stats.functions = fns_counted.len();
    Ok(stats)
}
