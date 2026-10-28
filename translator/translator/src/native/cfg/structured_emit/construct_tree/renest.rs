use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::native::ir::{LlType, LlValue, TypedValue};
use crate::native::tir;
use std::collections::BTreeSet;

const PREFIX: &str = "%metal2vulkan.ct.";
const PRE: &str = "%metal2vulkan.ct.pre";
const HEADER: &str = "%metal2vulkan.ct.header";
const TEST: &str = "%metal2vulkan.ct.test";
const DISPATCH: &str = "%metal2vulkan.ct.dispatch";
const INVALID: &str = "%metal2vulkan.ct.invalid";
const MERGE: &str = "%metal2vulkan.ct.merge";
const CONTINUE: &str = "%metal2vulkan.ct.continue";
const EXIT: &str = "%metal2vulkan.ct.exit";
const PC: &str = "%metal2vulkan.ct.pc";
const NEXT_PC: &str = "%metal2vulkan.ct.nextpc";
const DONE: &str = "%metal2vulkan.ct.done";
const ROUTE: &str = "%metal2vulkan.ct.route";
const RETURN_VALUE: &str = "%metal2vulkan.ct.return";
const NEXT_RETURN_VALUE: &str = "%metal2vulkan.ct.nextreturn";

#[derive(Clone, Debug)]
struct PhiSlot {
    original: String,
    ty: LlType,
    host: usize,
    incoming: HashMap<usize, LlValue>,
    current: String,
    next: String,
}

#[derive(Clone, Debug)]
struct HeaderPointerDerivation {
    owner: usize,
    original: String,
    header: String,
    inst: tir::TirInst,
    scalar_dependencies: Vec<String>,
}

fn cross_case_state_type(ty: &LlType) -> bool {
    match ty {
        LlType::Named(_) => false,
        LlType::Vector(element, _) | LlType::Array(element, _) => {
            pointer_free_composite_component(element)
        }
        LlType::Struct(fields) => fields.iter().all(pointer_free_composite_component),
        _ => true,
    }
}

fn pointer_free_composite_component(ty: &LlType) -> bool {
    match ty {
        LlType::Ptr(_) | LlType::Named(_) => false,
        LlType::Vector(element, _) | LlType::Array(element, _) => {
            pointer_free_composite_component(element)
        }
        LlType::Struct(fields) => fields.iter().all(pointer_free_composite_component),
        _ => true,
    }
}

fn opaque_image_dependencies(blocks: &[BodyBlock]) -> HashSet<String> {
    let mut values = HashSet::default();
    for block in blocks.iter().filter_map(|block| block.typed.as_ref()) {
        for inst in &block.insts {
            let Some(call) = inst.call().as_ref() else {
                continue;
            };
            if !crate::air_intrinsics::air_image_intrinsic(&call.callee) {
                continue;
            }
            for argument in &call.args {
                if matches!(argument.ty, LlType::Ptr(_)) {
                    if let LlValue::Local(name) = &argument.value {
                        values.insert(name.clone());
                    }
                }
            }
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        for block in blocks.iter().filter_map(|block| block.typed.as_ref()) {
            for inst in &block.insts {
                let Some(result) = &inst.result else {
                    continue;
                };
                if !values.contains(result) || !matches!(inst.result_ty, Some(LlType::Ptr(_))) {
                    continue;
                }
                for operand in &inst.operands {
                    if let tir::TirOperand::Value {
                        name,
                        ty: LlType::Ptr(_),
                    } = operand
                    {
                        changed |= values.insert(name.clone());
                    }
                }
                if let Some(phi_values) = inst.phi_values() {
                    for value in phi_values {
                        if let LlValue::Local(name) = value {
                            changed |= values.insert(name.clone());
                        }
                    }
                }
            }
        }
    }
    values
}

fn dominating_pointer_derivations(
    blocks: &[BodyBlock],
    function_variables: &HashSet<String>,
    defined_values: &HashSet<String>,
) -> (HashSet<String>, Vec<tir::TirInst>) {
    let mut names = HashSet::default();
    let mut instructions = Vec::new();
    loop {
        let mut added = false;
        for block in blocks.iter().filter_map(|block| block.typed.as_ref()) {
            for inst in &block.insts {
                let Some(result) = &inst.result else {
                    continue;
                };
                if names.contains(result)
                    || !matches!(inst.result_ty, Some(LlType::Ptr(_)))
                    || !matches!(
                        inst.opcode,
                        tir::TirOpcode::Bitcast
                            | tir::TirOpcode::AddrSpaceCast
                            | tir::TirOpcode::GetElementPtr
                    )
                {
                    continue;
                }

                let mut rooted = match inst.opcode {
                    tir::TirOpcode::GetElementPtr => inst.gep().as_ref().is_some_and(|gep| {
                        matches!(
                            &gep.base.value,
                            LlValue::Local(name) if !defined_values.contains(name)
                        )
                    }),
                    tir::TirOpcode::Bitcast | tir::TirOpcode::AddrSpaceCast => {
                        inst.operands.iter().any(|operand| {
                            matches!(
                                operand,
                                tir::TirOperand::Value {
                                    name,
                                    ty: LlType::Ptr(_)
                                } if !defined_values.contains(name)
                            )
                        })
                    }
                    _ => false,
                };
                let mut operands_dominate = true;
                inst.visit_uses(|name| {
                    if function_variables.contains(name) || names.contains(name) {
                        rooted = true;
                    } else if defined_values.contains(name) {
                        operands_dominate = false;
                    }
                });
                if rooted && operands_dominate {
                    names.insert(result.clone());
                    instructions.push(inst.clone());
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    (names, instructions)
}

fn carrier_with_phis(
    name: &str,
    target: &str,
    phis: &[(String, LlType, Vec<(LlValue, String)>)],
    role: BlockRole,
) -> Result<BodyBlock, String> {
    let mut carrier =
        tir::lower_block_carrier(name, &[format!("br label {target}")], &HashMap::default())
            .ok_or_else(|| format!("construct-tree:gateway-carrier name={name}"))?;
    for (result, ty, incoming) in phis {
        carrier.push_value_phi(result, ty, incoming);
    }
    Ok(BodyBlock {
        name: name.to_string(),
        role,
        typed: Some(carrier.into()),
    })
}

fn renamed_value(value: &LlValue, map: &HashMap<String, String>) -> LlValue {
    tir::renamed_llvalue(value, map)
}

fn terminator_value_uses(terminator: &tir::TirTerminator) -> Vec<String> {
    match terminator {
        tir::TirTerminator::Br(_)
        | tir::TirTerminator::Ret(None)
        | tir::TirTerminator::Unreachable => Vec::new(),
        tir::TirTerminator::BrCond { cond, .. }
        | tir::TirTerminator::Switch { selector: cond, .. }
        | tir::TirTerminator::Ret(Some(cond)) => vec![cond.clone()],
    }
}

pub(in crate::native) fn materialize_construct_tree_roles(
    blocks: &[BodyBlock],
    plan: &ConstructTreePlan,
) -> Result<Vec<BodyBlock>, String> {
    if blocks.is_empty() {
        return Err("construct-tree:empty-region".to_string());
    }
    if blocks.len() != plan.owners.len() {
        return Err(format!(
            "construct-tree:block-plan-mismatch blocks={} owners={}",
            blocks.len(),
            plan.owners.len()
        ));
    }
    if blocks.iter().any(|block| block.name.starts_with(PREFIX)) {
        return Err("construct-tree:reserved-block-prefix".to_string());
    }

    let names: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect();
    if names.len() != blocks.len() {
        return Err("construct-tree:duplicate-block-name".to_string());
    }
    let planned_edges: HashSet<(usize, usize)> = plan
        .routes
        .iter()
        .map(|route| (route.edge.from, route.edge.to))
        .collect();
    if planned_edges.len() != plan.routes.len() {
        return Err("construct-tree:duplicate-edge".to_string());
    }
    let routes: HashMap<(usize, usize), &ConstructRoute> = plan
        .routes
        .iter()
        .map(|route| ((route.edge.from, route.edge.to), route))
        .collect();
    let opaque_image_dependencies = opaque_image_dependencies(blocks);

    let mut return_ty = None;
    let mut saw_void_return = false;
    let mut successors = Vec::with_capacity(blocks.len());
    for (source, block) in blocks.iter().enumerate() {
        let carrier = block
            .typed
            .as_ref()
            .ok_or_else(|| format!("construct-tree:missing-carrier block={}", block.name))?;
        if carrier
            .insts
            .iter()
            .filter_map(|inst| inst.result.as_deref())
            .any(|result| result.starts_with(PREFIX))
        {
            return Err(format!(
                "construct-tree:reserved-value-prefix block={}",
                block.name
            ));
        }
        match &carrier.ret {
            tir::RetEmit::Value(value) => {
                if matches!(value.ty, LlType::Ptr(_)) {
                    return Err(format!(
                        "construct-tree:pointer-return block={}",
                        block.name
                    ));
                }
                if saw_void_return
                    || return_ty
                        .as_ref()
                        .is_some_and(|existing| existing != &value.ty)
                {
                    return Err(format!(
                        "construct-tree:mixed-return-type block={}",
                        block.name
                    ));
                }
                return_ty = Some(value.ty.clone());
            }
            tir::RetEmit::Void => {
                if return_ty.is_some() {
                    return Err(format!(
                        "construct-tree:mixed-return-type block={}",
                        block.name
                    ));
                }
                saw_void_return = true;
            }
            tir::RetEmit::FromText if matches!(carrier.terminator, tir::TirTerminator::Ret(_)) => {
                return Err(format!(
                    "construct-tree:untyped-return block={}",
                    block.name
                ));
            }
            tir::RetEmit::FromText => {}
        }
        let mut block_successors = Vec::new();
        for target in carrier.terminator.successors() {
            let Some(&target_index) = names.get(target) else {
                return Err(format!(
                    "construct-tree:unknown-successor from={} to={target}",
                    block.name
                ));
            };
            if !planned_edges.contains(&(source, target_index)) {
                return Err(format!(
                    "construct-tree:unplanned-source-edge from={} to={target}",
                    block.name
                ));
            }
            if !block_successors.contains(&target_index) {
                block_successors.push(target_index);
            }
        }
        successors.push(block_successors);
    }
    let actual_edges: HashSet<(usize, usize)> = successors
        .iter()
        .enumerate()
        .flat_map(|(source, targets)| targets.iter().map(move |target| (source, *target)))
        .collect();
    if actual_edges != planned_edges {
        return Err(format!(
            "construct-tree:edge-set-mismatch actual={} planned={}",
            actual_edges.len(),
            planned_edges.len()
        ));
    }

    let mut slots = Vec::new();
    let mut rename = HashMap::default();
    for (host, block) in blocks.iter().enumerate() {
        let carrier = block.typed.as_ref().expect("checked above");
        for inst in &carrier.insts {
            let Some((ty, incoming)) = &inst.phi_incoming() else {
                continue;
            };
            let result = inst
                .result
                .as_ref()
                .ok_or_else(|| format!("construct-tree:phi-without-result block={}", block.name))?;
            let slot_index = slots.len();
            let current = format!("{PREFIX}slot.{slot_index}");
            let next = format!("{PREFIX}nextslot.{slot_index}");
            rename.insert(result.clone(), current.clone());
            let mut routed = HashMap::default();
            for (value, predecessor) in incoming {
                let Some(&source) = names.get(predecessor.as_str()) else {
                    return Err(format!(
                        "construct-tree:phi-unknown-predecessor block={} pred={predecessor}",
                        block.name
                    ));
                };
                if !planned_edges.contains(&(source, host)) {
                    return Err(format!(
                        "construct-tree:phi-nonedge block={} pred={predecessor}",
                        block.name
                    ));
                }
                if routed.insert(source, value.clone()).is_some() {
                    return Err(format!(
                        "construct-tree:phi-duplicate-predecessor block={} pred={predecessor}",
                        block.name
                    ));
                }
            }
            slots.push(PhiSlot {
                original: result.clone(),
                ty: ty.clone(),
                host,
                incoming: routed,
                current,
                next,
            });
        }
    }
    for slot in &slots {
        if matches!(slot.ty, LlType::Ptr(_)) && opaque_image_dependencies.contains(&slot.original) {
            return Err(format!(
                "construct-tree:cross-case-opaque-image owner={} value={}",
                blocks[slot.host].name, slot.original
            ));
        }
    }
    for slot in &mut slots {
        for value in slot.incoming.values_mut() {
            *value = renamed_value(value, &rename);
        }
    }

    let mut def_block = HashMap::default();
    let mut defined_values = HashSet::default();
    let mut function_variables = HashSet::default();
    for (index, block) in blocks.iter().enumerate() {
        for inst in &block.typed.as_ref().expect("checked above").insts {
            if let Some(result) = &inst.result {
                defined_values.insert(result.clone());
            }
            if !inst.is_phi() {
                if let Some(result) = &inst.result {
                    def_block.insert(result.clone(), (index, inst.result_ty.clone()));
                    if inst.opcode == "alloca" {
                        function_variables.insert(result.clone());
                    }
                }
            }
        }
    }
    let mut cross_values = BTreeSet::new();
    for (index, block) in blocks.iter().enumerate() {
        let carrier = block.typed.as_ref().expect("checked above");
        for inst in &block.typed.as_ref().expect("checked above").insts {
            if inst.is_phi() {
                continue;
            }
            inst.visit_uses(|name| {
                if def_block
                    .get(name)
                    .is_some_and(|(owner, _)| *owner != index)
                {
                    cross_values.insert(name.to_string());
                }
            });
        }
        for name in terminator_value_uses(&carrier.terminator) {
            if def_block
                .get(name.as_str())
                .is_some_and(|(owner, _)| *owner != index)
            {
                cross_values.insert(name);
            }
        }
    }
    for slot in &slots {
        for (&source, value) in &slot.incoming {
            let mut names = Vec::new();
            collect_value_locals(value, &mut names);
            for name in names {
                if def_block
                    .get(name.as_str())
                    .is_some_and(|(owner, _)| *owner != source)
                    && !rename.values().any(|slot| slot.as_str() == name.as_str())
                {
                    cross_values.insert(name);
                }
            }
        }
    }
    let (dominating_pointer_derivations, pointer_preheader_instructions) =
        dominating_pointer_derivations(blocks, &function_variables, &defined_values);
    cross_values.retain(|name| {
        !function_variables.contains(name) && !dominating_pointer_derivations.contains(name)
    });

    let mut header_pointer_derivations = Vec::new();
    for (owner, block) in blocks.iter().enumerate() {
        for inst in &block.typed.as_ref().expect("checked above").insts {
            let Some(original) = &inst.result else {
                continue;
            };
            if !cross_values.contains(original) || inst.result_ty != Some(LlType::Ptr(0)) {
                continue;
            }
            let Some(gep) = inst.gep().as_ref() else {
                continue;
            };
            let LlValue::Local(base) = &gep.base.value else {
                continue;
            };
            if defined_values.contains(base)
                && !function_variables.contains(base)
                && !dominating_pointer_derivations.contains(base)
            {
                continue;
            }

            let mut scalar_dependencies = Vec::new();
            let mut representable = true;
            for index in &gep.indices {
                let LlValue::Local(name) = &index.value else {
                    continue;
                };
                if rename.contains_key(name) || !defined_values.contains(name) {
                    continue;
                }
                let Some((_, Some(ty))) = def_block.get(name) else {
                    representable = false;
                    break;
                };
                if matches!(ty, LlType::Ptr(_)) || !cross_case_state_type(ty) {
                    representable = false;
                    break;
                }
                scalar_dependencies.push(name.clone());
            }
            if !representable || scalar_dependencies.is_empty() {
                continue;
            }
            for dependency in &scalar_dependencies {
                cross_values.insert(dependency.clone());
            }
            cross_values.remove(original);
            header_pointer_derivations.push(HeaderPointerDerivation {
                owner,
                original: original.clone(),
                header: format!("{PREFIX}ptr.{}", header_pointer_derivations.len()),
                inst: inst.clone(),
                scalar_dependencies,
            });
        }
    }
    let mut value_slots = Vec::new();
    for original in cross_values {
        let Some((owner, ty)) = def_block.get(&original) else {
            continue;
        };
        let Some(ty) = ty else {
            return Err(format!(
                "construct-tree:cross-case-untyped-value owner={} value={original}",
                blocks[*owner].name
            ));
        };
        if !cross_case_state_type(ty) {
            return Err(format!(
                "construct-tree:cross-case-unresolved-composite owner={} value={original}",
                blocks[*owner].name
            ));
        }
        if matches!(ty, LlType::Ptr(_)) && opaque_image_dependencies.contains(&original) {
            return Err(format!(
                "construct-tree:cross-case-opaque-image owner={} value={original}",
                blocks[*owner].name
            ));
        }
        let slot_index = value_slots.len();
        value_slots.push(ValueSlot {
            ty: ty.clone(),
            owner: *owner,
            original,
            current: format!("{PREFIX}vslot.{slot_index}"),
            next: format!("{PREFIX}nextvslot.{slot_index}"),
        });
    }
    let value_slot_by_name: HashMap<&str, &ValueSlot> = value_slots
        .iter()
        .map(|slot| (slot.original.as_str(), slot))
        .collect();
    for slot in &mut slots {
        for (&source, value) in &mut slot.incoming {
            substitute_cross_slot_value(value, &value_slot_by_name, source);
        }
    }

    let done_state = blocks.len() as u64;
    let mut preheader = passthrough(PRE, HEADER);
    let preheader_carrier =
        std::sync::Arc::make_mut(preheader.typed.as_mut().expect("typed preheader"));
    preheader_carrier.insts.extend(
        blocks
            .iter()
            .filter_map(|block| block.typed.as_ref())
            .flat_map(|block| &block.insts)
            .filter(|inst| inst.opcode == "alloca")
            .cloned(),
    );
    preheader_carrier
        .insts
        .extend(pointer_preheader_instructions);
    let mut out = vec![preheader];
    let mut header_phis = vec![(
        PC.to_string(),
        LlType::Int(32),
        vec![
            (LlValue::Int(0), PRE.to_string()),
            (LlValue::Local(NEXT_PC.to_string()), CONTINUE.to_string()),
        ],
    )];
    for slot in &slots {
        header_phis.push((
            slot.current.clone(),
            slot.ty.clone(),
            vec![
                (LlValue::Undef, PRE.to_string()),
                (LlValue::Local(slot.next.clone()), CONTINUE.to_string()),
            ],
        ));
    }
    for slot in &value_slots {
        header_phis.push((
            slot.current.clone(),
            slot.ty.clone(),
            vec![
                (LlValue::Undef, PRE.to_string()),
                (LlValue::Local(slot.next.clone()), CONTINUE.to_string()),
            ],
        ));
    }
    if let Some(return_ty) = &return_ty {
        header_phis.push((
            RETURN_VALUE.to_string(),
            return_ty.clone(),
            vec![
                (LlValue::Undef, PRE.to_string()),
                (
                    LlValue::Local(NEXT_RETURN_VALUE.to_string()),
                    CONTINUE.to_string(),
                ),
            ],
        ));
    }
    let mut header = carrier_with_phis(HEADER, TEST, &header_phis, BlockRole::Normal)?;
    if !header_pointer_derivations.is_empty() {
        let carrier = std::sync::Arc::make_mut(header.typed.as_mut().expect("typed header"));
        carrier.insts.extend(
            header_pointer_derivations
                .iter()
                .map(|derivation| derivation.inst.clone()),
        );
        let mut header_rename = rename.clone();
        for derivation in &header_pointer_derivations {
            header_rename.insert(derivation.original.clone(), derivation.header.clone());
            for dependency in &derivation.scalar_dependencies {
                let slot = value_slot_by_name
                    .get(dependency.as_str())
                    .expect("dynamic pointer scalar has a value slot");
                header_rename.insert(dependency.clone(), slot.current.clone());
            }
        }
        carrier.rename(&header_rename);
    }
    out.push(header);
    out.push(synthetic_block(
        TEST.to_string(),
        vec![
            format!("{DONE} = icmp eq i32 {PC}, {done_state}"),
            format!("br i1 {DONE}, label {EXIT}, label {DISPATCH}"),
        ],
        BlockRole::Normal,
    ));
    let switch_cases = blocks
        .iter()
        .enumerate()
        .map(|(state, block)| format!("i32 {state}, label {}", block.name))
        .collect::<Vec<_>>()
        .join(" ");
    out.push(synthetic_block(
        DISPATCH.to_string(),
        vec![format!(
            "switch i32 {PC}, label {INVALID} [ {switch_cases} ]"
        )],
        BlockRole::ConstructTreeRoute,
    ));

    let mut pc_updates = Vec::with_capacity(blocks.len());
    let mut phi_updates = slots
        .iter()
        .map(|_| Vec::with_capacity(blocks.len()))
        .collect::<Vec<_>>();
    let mut value_update_incoming = value_slots
        .iter()
        .map(|_| Vec::with_capacity(blocks.len()))
        .collect::<Vec<_>>();
    let mut return_update_incoming = return_ty.as_ref().map(|_| Vec::with_capacity(blocks.len()));
    for (case_index, original) in blocks.iter().enumerate() {
        let mut case = original.clone();
        let carrier = std::sync::Arc::make_mut(case.typed.as_mut().expect("checked above"));
        carrier.rename(&rename);
        carrier.insts.retain(|inst| {
            !inst.result.as_ref().is_some_and(|result| {
                function_variables.contains(result)
                    || dominating_pointer_derivations.contains(result)
            })
        });
        let mut used_names = HashSet::default();
        for inst in carrier.insts.iter().filter(|inst| !inst.is_phi()) {
            inst.visit_uses(|name| {
                used_names.insert(name.to_string());
            });
        }
        used_names.extend(terminator_value_uses(&carrier.terminator));
        let mut substitutions = HashMap::default();
        for name in used_names {
            let Some(slot) = value_slot_by_name.get(name.as_str()) else {
                continue;
            };
            if slot.owner == case_index {
                continue;
            }
            substitutions.insert(
                slot.original.clone(),
                TypedValue {
                    ty: slot.ty.clone(),
                    value: LlValue::Local(slot.current.clone()),
                },
            );
        }
        for derivation in &header_pointer_derivations {
            if derivation.owner != case_index {
                substitutions.insert(
                    derivation.original.clone(),
                    TypedValue {
                        ty: LlType::Ptr(0),
                        value: LlValue::Local(derivation.header.clone()),
                    },
                );
            }
        }
        carrier.substitute_values(&substitutions);
        carrier.insts.retain(|inst| !inst.is_phi());
        let case_return = match &carrier.ret {
            tir::RetEmit::Value(value) => Some(value.value.clone()),
            tir::RetEmit::Void | tir::RetEmit::FromText => None,
        };
        let targets = &successors[case_index];
        let join = format!("{PREFIX}casejoin.{case_index}");
        let mut route_tails = Vec::with_capacity(targets.len());
        let mut route_blocks = Vec::new();
        if targets.is_empty() {
            carrier.set_unconditional_branch(&join);
        }
        for (edge_index, target) in targets.iter().enumerate() {
            let route = routes.get(&(case_index, *target)).ok_or_else(|| {
                format!(
                    "construct-tree:missing-route from={} to={}",
                    original.name, blocks[*target].name
                )
            })?;
            let mut route_names = Vec::with_capacity(route.exits.len() + route.enters.len() + 1);
            route_names.extend(route.exits.iter().enumerate().map(|(step, construct)| {
                format!("{ROUTE}.{case_index}.{edge_index}.exit.{step}.{construct}")
            }));
            route_names.extend(route.enters.iter().enumerate().map(|(step, construct)| {
                format!("{ROUTE}.{case_index}.{edge_index}.enter.{step}.{construct}")
            }));
            route_names.push(format!("{ROUTE}.{case_index}.{edge_index}.select"));
            carrier.redirect_successor(&blocks[*target].name, &route_names[0]);
            for (step, name) in route_names.iter().enumerate() {
                let next = route_names
                    .get(step + 1)
                    .map(String::as_str)
                    .unwrap_or(&join);
                route_blocks.push(route_passthrough(name, next));
            }
            route_tails.push(route_names.last().expect("terminal route step").clone());
        }
        out.push(case);
        out.extend(route_blocks);

        let pc_update = match targets.as_slice() {
            [] => LlValue::Int(done_state),
            [target] => LlValue::Int(*target as u64),
            _ => LlValue::Local(format!("{PREFIX}casepc.{case_index}")),
        };
        let mut join_phis = Vec::new();
        if targets.len() > 1 {
            join_phis.push((
                format!("{PREFIX}casepc.{case_index}"),
                LlType::Int(32),
                targets
                    .iter()
                    .enumerate()
                    .map(|(edge_index, target)| {
                        (
                            LlValue::Int(*target as u64),
                            route_tails[edge_index].clone(),
                        )
                    })
                    .collect(),
            ));
        }
        let mut slot_updates = Vec::with_capacity(slots.len());
        for (slot_index, slot) in slots.iter().enumerate() {
            let edge_value = |target: usize| -> Result<LlValue, String> {
                if target != slot.host {
                    return Ok(LlValue::Local(slot.current.clone()));
                }
                slot.incoming.get(&case_index).cloned().ok_or_else(|| {
                    format!(
                        "construct-tree:phi-missing-source block={} pred={}",
                        blocks[slot.host].name, original.name
                    )
                })
            };
            let update = match targets.as_slice() {
                [] => LlValue::Local(slot.current.clone()),
                [target] => edge_value(*target)?,
                _ if targets.contains(&slot.host) => {
                    let result = format!("{PREFIX}caseslot.{case_index}.{slot_index}");
                    join_phis.push((
                        result.clone(),
                        slot.ty.clone(),
                        targets
                            .iter()
                            .enumerate()
                            .map(|(edge_index, target)| {
                                Ok((edge_value(*target)?, route_tails[edge_index].clone()))
                            })
                            .collect::<Result<Vec<_>, String>>()?,
                    ));
                    LlValue::Local(result)
                }
                _ => LlValue::Local(slot.current.clone()),
            };
            slot_updates.push(update);
        }
        let case_value_updates: Vec<_> = value_slots
            .iter()
            .map(|slot| {
                if slot.owner == case_index {
                    LlValue::Local(slot.original.clone())
                } else {
                    LlValue::Local(slot.current.clone())
                }
            })
            .collect();
        out.push(carrier_with_phis(
            &join,
            MERGE,
            &join_phis,
            BlockRole::Normal,
        )?);
        pc_updates.push((pc_update, join.clone()));
        for (incoming, value) in phi_updates.iter_mut().zip(slot_updates) {
            incoming.push((value, join.clone()));
        }
        for (incoming, value) in value_update_incoming.iter_mut().zip(case_value_updates) {
            incoming.push((value, join.clone()));
        }
        if let Some(incoming) = return_update_incoming.as_mut() {
            incoming.push((
                case_return.unwrap_or_else(|| LlValue::Local(RETURN_VALUE.to_string())),
                join,
            ));
        }
    }

    out.push(passthrough(INVALID, MERGE));
    let mut merge_phis = vec![(
        NEXT_PC.to_string(),
        LlType::Int(32),
        pc_updates
            .into_iter()
            .chain(std::iter::once((
                LlValue::Int(done_state),
                INVALID.to_string(),
            )))
            .collect(),
    )];
    for (slot, incoming) in slots.iter().zip(phi_updates) {
        merge_phis.push((
            slot.next.clone(),
            slot.ty.clone(),
            incoming
                .into_iter()
                .chain(std::iter::once((
                    LlValue::Local(slot.current.clone()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    for (slot, incoming) in value_slots.iter().zip(value_update_incoming) {
        merge_phis.push((
            slot.next.clone(),
            slot.ty.clone(),
            incoming
                .into_iter()
                .chain(std::iter::once((
                    LlValue::Local(slot.current.clone()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    if let Some(incoming) = return_update_incoming {
        merge_phis.push((
            NEXT_RETURN_VALUE.to_string(),
            return_ty.clone().expect("return updates require a type"),
            incoming
                .into_iter()
                .chain(std::iter::once((
                    LlValue::Local(RETURN_VALUE.to_string()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    out.push(carrier_with_phis(
        MERGE,
        CONTINUE,
        &merge_phis,
        BlockRole::LMerge,
    )?);
    out.push(passthrough(CONTINUE, HEADER));
    let exit = if let Some(return_ty) = &return_ty {
        let return_ty = crate::native::render::render_type(return_ty)
            .ok_or_else(|| "construct-tree:unrenderable-return-type".to_string())?;
        format!("ret {return_ty} {RETURN_VALUE}")
    } else {
        "ret void".to_string()
    };
    out.push(synthetic_block(
        EXIT.to_string(),
        vec![exit],
        BlockRole::Normal,
    ));

    if out.len() > plan.block_bound {
        return Err(format!(
            "construct-tree:materialization-bound blocks={} bound={}",
            out.len(),
            plan.block_bound
        ));
    }
    if crate::env_vars::why() {
        let instructions = out
            .iter()
            .filter_map(|block| block.typed.as_ref())
            .map(|block| block.insts.len())
            .sum::<usize>();
        let phis = out
            .iter()
            .filter_map(|block| block.typed.as_ref())
            .flat_map(|block| &block.insts)
            .filter(|inst| inst.is_phi())
            .count();
        let phi_incoming = out
            .iter()
            .filter_map(|block| block.typed.as_ref())
            .flat_map(|block| &block.insts)
            .filter_map(|inst| inst.phi_incoming().as_ref())
            .map(|(_, incoming)| incoming.len())
            .sum::<usize>();
        eprintln!(
            "WHY-CONSTRUCT-TREE materialized blocks={} instructions={instructions} phis={phis} phi-incoming={phi_incoming} source-phis={} cross-values={}",
            out.len(),
            slots.len(),
            value_slots.len()
        );
    }
    if finalized_construct_tree_plan(&out).is_none() {
        let reason = super::super::construct_tree_reject_reason(&out)
            .unwrap_or_else(|| "unknown".to_string());
        return Err(format!("construct-tree:role-plan-reject reason={reason}"));
    }
    Ok(out)
}

fn finalized_construct_tree_plan(blocks: &[BodyBlock]) -> Option<super::super::StructuredPlan> {
    super::super::structured_plan(blocks)
        .or_else(|| super::super::structured_plan_construct_tree(blocks))
}

pub(in crate::native) fn renest_construct_tree(
    blocks: &[BodyBlock],
    plan: &ConstructTreePlan,
) -> Result<super::super::StructuredPlan, String> {
    let out = materialize_construct_tree_roles(blocks, plan)?;
    finalized_construct_tree_plan(&out)
        .ok_or_else(|| "construct-tree:role-plan-nondeterministic".to_string())
}
