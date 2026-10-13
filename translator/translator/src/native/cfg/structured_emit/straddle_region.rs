use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::native::ir::{LlType, LlValue, TypedValue};
use crate::native::tir;
use std::collections::BTreeSet;

const PREFIX: &str = "%metal2vulkan.ct.sl.";
const PRE: &str = "%metal2vulkan.ct.sl.pre";
const HEADER: &str = "%metal2vulkan.ct.sl.header";
const TEST: &str = "%metal2vulkan.ct.sl.test";
const DISPATCH: &str = "%metal2vulkan.ct.sl.dispatch";
const INVALID: &str = "%metal2vulkan.ct.sl.invalid";
const MERGE: &str = "%metal2vulkan.ct.sl.merge";
const CONTINUE: &str = "%metal2vulkan.ct.sl.continue";
const EXIT: &str = "%metal2vulkan.ct.sl.exit";
const PC: &str = "%metal2vulkan.ct.sl.pc";
const NEXT_PC: &str = "%metal2vulkan.ct.sl.nextpc";
const DONE: &str = "%metal2vulkan.ct.sl.done";
const EXIT_ID: &str = "%metal2vulkan.ct.sl.exit.id";
const NEXT_EXIT_ID: &str = "%metal2vulkan.ct.sl.next.exit.id";
const ROUTE: &str = "%metal2vulkan.ct.sl.route";

const MAX_REGION_BLOCKS: usize = 128;

#[derive(Clone, Debug)]
struct Witness {
    blocks: Vec<BodyBlock>,
    closure: HashSet<usize>,
    ordered: Vec<usize>,
    local_of: HashMap<usize, usize>,
    entry_from: usize,
    entry_to: usize,
    exits: Vec<(usize, usize)>,
}

#[derive(Clone, Debug)]
struct PhiSlot {
    ty: LlType,
    host: usize,
    incoming: HashMap<usize, LlValue>,
    initial: LlValue,
    current: String,
    next: String,
}

#[derive(Clone, Debug)]
struct ExitPayloadSlot {
    ty: LlType,
    target: usize,
    phi_result: String,
    exit_index: usize,
    value: LlValue,
    current: String,
    next: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    Internal(usize),
    Exit(usize),
    Reentry(usize),
}

#[derive(Debug)]
struct CaseUpdate {
    join: String,
    pc: LlValue,
    exit_id: LlValue,
    phi_slots: Vec<LlValue>,
    value_slots: Vec<LlValue>,
    exit_payload_slots: Vec<LlValue>,
}

fn carrier_with_phis(
    name: &str,
    target: &str,
    phis: &[(String, LlType, Vec<(LlValue, String)>)],
    role: BlockRole,
) -> Result<BodyBlock, String> {
    let mut carrier =
        tir::lower_block_carrier(name, &[format!("br label {target}")], &HashMap::default())
            .ok_or_else(|| format!("construct-tree:straddle-gateway-carrier name={name}"))?;
    for (result, ty, incoming) in phis {
        carrier.push_value_phi(result, ty, incoming);
    }
    Ok(BodyBlock {
        name: name.to_string(),
        role,
        typed: Some(carrier.into()),
    })
}

fn derive_witness(blocks: &[BodyBlock], reject_reason: Option<&str>) -> Option<Witness> {
    if reject_reason != Some("selection:straddle-loop-merge") {
        return None;
    }
    for (converge, break_aware) in [(false, false), (true, false), (true, true)] {
        if let Some(witness) = derive_witness_in_mode(blocks, converge, break_aware) {
            return Some(witness);
        }
    }
    None
}

fn derive_loop_exit_sibling_witness(blocks: &[BodyBlock]) -> Option<Witness> {
    let forest = analyze(blocks);
    let post_idom = super::super::loopforest::post_idom(blocks);
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let post_chain = |start: &str| {
        let mut chain = vec![start.to_string()];
        let mut current = start;
        while let Some(next) = post_idom.get(current) {
            if chain.contains(next) {
                break;
            }
            chain.push(next.clone());
            current = next;
        }
        chain
    };
    for owner in blocks {
        let Some((left, right)) = conditional_branch_targets(owner) else {
            continue;
        };
        let Some(owner_merge) = post_idom.get(&owner.name) else {
            continue;
        };
        for (loop_entry, sibling) in [(&left, &right), (&right, &left)] {
            for natural_loop in &forest.loops {
                let exit_chains = natural_loop
                    .exits
                    .iter()
                    .map(|exit| post_chain(exit))
                    .collect::<Vec<_>>();
                let Some(loop_merge) = exit_chains.first().and_then(|first| {
                    first
                        .iter()
                        .find(|candidate| {
                            exit_chains.iter().all(|chain| chain.contains(*candidate))
                        })
                        .cloned()
                }) else {
                    continue;
                };
                if !natural_loop.body.iter().any(|block| block == loop_entry)
                    || natural_loop.body.iter().any(|block| block == &owner.name)
                    || natural_loop.body.iter().any(|block| block == sibling)
                    || !forest.dominates(&owner.name, &natural_loop.header)
                    || !forest.dominates(&loop_merge, owner_merge)
                {
                    continue;
                }
                let exits_to_sibling = natural_loop.exits.iter().any(|exit| {
                    let mut current = exit.clone();
                    let mut seen = HashSet::default();
                    loop {
                        if &current == sibling {
                            break true;
                        }
                        if !seen.insert(current.clone())
                            || natural_loop.body.iter().any(|block| block == &current)
                        {
                            break false;
                        }
                        let Some(block) = by_name.get(current.as_str()) else {
                            break false;
                        };
                        let successors = block_successors(block);
                        let [next] = successors.as_slice() else {
                            break false;
                        };
                        current = next.clone();
                    }
                });
                if !exits_to_sibling {
                    continue;
                }
                if let Some(witness) =
                    build_selection_arm_witness(blocks, &forest, &owner.name, owner_merge)
                {
                    return Some(witness);
                }
            }
        }
    }
    None
}

fn build_selection_arm_witness(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    owner: &str,
    owner_merge: &str,
) -> Option<Witness> {
    let names = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let name_set = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    let mut predecessors: HashMap<String, Vec<String>> = HashMap::default();
    for block in blocks {
        for successor in block_successors(block) {
            if name_set.contains(successor.as_str()) {
                predecessors
                    .entry(successor)
                    .or_default()
                    .push(block.name.clone());
            }
        }
    }
    let mut reaches_merge = HashSet::default();
    let mut stack = vec![owner_merge.to_string()];
    while let Some(node) = stack.pop() {
        if !reaches_merge.insert(node.clone()) {
            continue;
        }
        if let Some(incoming) = predecessors.get(&node) {
            stack.extend(incoming.iter().cloned());
        }
    }
    let closure = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.name != owner_merge
                && forest.dominates(owner, &block.name)
                && reaches_merge.contains(&block.name))
            .then_some(index)
        })
        .collect::<HashSet<_>>();
    if closure.is_empty() || closure.len() > MAX_REGION_BLOCKS {
        return None;
    }
    let mut entries = Vec::new();
    let mut exits = Vec::new();
    for (source, block) in blocks.iter().enumerate() {
        for successor in block_successors(block) {
            let target = *names.get(successor.as_str())?;
            match (closure.contains(&source), closure.contains(&target)) {
                (false, true) => entries.push((source, target)),
                (true, false) => exits.push((source, target)),
                _ => {}
            }
        }
    }
    entries.sort_unstable();
    entries.dedup();
    exits.sort_unstable();
    exits.dedup();
    if entries.len() != 1 || exits.is_empty() {
        return None;
    }
    let mut ordered = closure.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let local_of = ordered
        .iter()
        .copied()
        .enumerate()
        .map(|(local, global)| (global, local))
        .collect::<HashMap<_, _>>();
    Some(Witness {
        blocks: blocks.to_vec(),
        closure,
        ordered,
        local_of,
        entry_from: entries[0].0,
        entry_to: entries[0].1,
        exits,
    })
}

fn derive_witness_in_mode(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    break_aware: bool,
) -> Option<Witness> {
    let (lblocks, loop_merges) = forest_loop_merges(blocks, converge_inloop, false);
    let lforest = analyze(&lblocks);
    for loop_info in &lforest.loops {
        if !loop_merges.contains_key(&loop_info.header) {
            return None;
        }
    }
    let (sblocks, branch, switch) = unique_selection_merges(&lblocks, &loop_merges, break_aware);
    if sblocks.iter().any(|block| block.name.starts_with(PREFIX)) {
        return None;
    }
    let forest = analyze(&sblocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let name_set: HashSet<&str> = sblocks.iter().map(|block| block.name.as_str()).collect();
    let mut header_merge: HashMap<String, String> = HashMap::default();
    for (header, info) in &loop_merges {
        header_merge.insert(header.clone(), info.merge.clone());
    }
    for block in &sblocks {
        if loop_headers.contains(block.name.as_str()) {
            continue;
        }
        if is_switch_block(block) {
            if let Some(merge) = switch.get(&block.name) {
                header_merge.insert(block.name.clone(), merge.clone());
            }
            continue;
        }
        let successors = block_successors(block);
        let distinct: HashSet<&str> = successors
            .iter()
            .map(String::as_str)
            .filter(|target| name_set.contains(target))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        if let Some((t, f)) = conditional_branch_targets(block) {
            if let Some(merge) = branch.get(&(t, f)) {
                header_merge.insert(block.name.clone(), merge.clone());
            }
        }
    }

    for loop_info in &forest.loops {
        let info = loop_merges.get(&loop_info.header)?;
        for (owner, owner_merge) in &header_merge {
            if owner == &loop_info.header {
                continue;
            }
            let inside = forest.dominates(owner, &loop_info.header)
                && !forest.dominates(owner_merge, &loop_info.header);
            if !inside || !forest.dominates(owner_merge, &info.merge) {
                continue;
            }
            return build_witness(
                &sblocks,
                &forest,
                &loop_info.body,
                &info.merge,
                owner,
                owner_merge,
            );
        }
    }
    None
}

fn build_witness(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_body: &[String],
    loop_merge: &str,
    owner: &str,
    owner_merge: &str,
) -> Option<Witness> {
    let names: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect();
    let name_set: HashSet<&str> = blocks.iter().map(|block| block.name.as_str()).collect();
    let mut preds: HashMap<String, Vec<String>> = HashMap::default();
    for block in blocks {
        for succ in block_successors(block) {
            if name_set.contains(succ.as_str()) {
                preds.entry(succ).or_default().push(block.name.clone());
            }
        }
    }
    let mut reaches_loop_merge = HashSet::default();
    let mut stack = vec![loop_merge.to_string()];
    while let Some(node) = stack.pop() {
        if !reaches_loop_merge.insert(node.clone()) {
            continue;
        }
        if let Some(predecessors) = preds.get(&node) {
            stack.extend(predecessors.iter().cloned());
        }
    }
    let loop_body: HashSet<&str> = loop_body.iter().map(String::as_str).collect();
    let mut closure_names = HashSet::default();
    for block in blocks {
        let name = block.name.as_str();
        let in_owner_arm = forest.dominates(owner, name)
            && !forest.dominates(owner_merge, name)
            && reaches_loop_merge.contains(name);
        let in_merge_tail =
            forest.dominates(owner_merge, name) && reaches_loop_merge.contains(name);
        if in_owner_arm || in_merge_tail || loop_body.contains(name) || name == loop_merge {
            closure_names.insert(name);
        }
    }
    if closure_names.len() > MAX_REGION_BLOCKS {
        return None;
    }
    let closure = closure_names
        .iter()
        .filter_map(|name| names.get(name).copied())
        .collect::<HashSet<_>>();
    let mut entries = Vec::new();
    let mut exits = Vec::new();
    for (source, block) in blocks.iter().enumerate() {
        for succ in block_successors(block) {
            let target = *names.get(succ.as_str())?;
            match (closure.contains(&source), closure.contains(&target)) {
                (false, true) => entries.push((source, target)),
                (true, false) => exits.push((source, target)),
                _ => {}
            }
        }
    }
    entries.sort_unstable();
    entries.dedup();
    exits.sort_unstable();
    exits.dedup();
    if entries.len() != 1 || exits.len() != 2 {
        return None;
    }
    let mut ordered = closure.iter().copied().collect::<Vec<_>>();
    ordered.sort_unstable();
    let local_of = ordered
        .iter()
        .copied()
        .enumerate()
        .map(|(local, global)| (global, local))
        .collect::<HashMap<_, _>>();
    Some(Witness {
        blocks: blocks.to_vec(),
        closure,
        ordered,
        local_of,
        entry_from: entries[0].0,
        entry_to: entries[0].1,
        exits,
    })
}

fn regional_candidate(
    witness: Witness,
    require_exit_payload: bool,
) -> Result<Vec<BodyBlock>, String> {
    let Witness {
        mut blocks,
        closure,
        ordered,
        local_of,
        entry_from,
        entry_to,
        exits,
    } = witness;
    let names: HashMap<String, usize> = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect();
    let entry_to_local = *local_of
        .get(&entry_to)
        .ok_or_else(|| "construct-tree:straddle-entry-target-outside".to_string())?;
    let done_state = ordered.len() as u64;
    let exit_gateway = (0..exits.len())
        .map(|index| format!("{PREFIX}exit.gateway.{index}"))
        .collect::<Vec<_>>();
    let exit_by_pair = exits
        .iter()
        .copied()
        .enumerate()
        .map(|(index, edge)| (edge, index))
        .collect::<HashMap<_, _>>();

    for block in &blocks {
        if block.typed.is_none() {
            return Err(format!(
                "construct-tree:straddle-missing-carrier block={}",
                block.name
            ));
        }
    }

    let mut successors = Vec::with_capacity(ordered.len());
    for &global in &ordered {
        let mut block_successors_local = Vec::new();
        for target_name in block_successors(&blocks[global]) {
            let Some(&target) = names.get(&target_name) else {
                continue;
            };
            if let Some(&local) = local_of.get(&target) {
                block_successors_local.push((target_name, Target::Internal(local)));
            } else if target == entry_from {
                let Some(&exit_index) = exit_by_pair.get(&(global, target)) else {
                    return Err(format!(
                        "construct-tree:straddle-unplanned-reentry from={} to={}",
                        blocks[global].name, blocks[target].name
                    ));
                };
                block_successors_local.push((target_name, Target::Reentry(exit_index)));
            } else {
                let Some(&exit_index) = exit_by_pair.get(&(global, target)) else {
                    return Err(format!(
                        "construct-tree:straddle-unplanned-exit from={} to={}",
                        blocks[global].name, blocks[target].name
                    ));
                };
                block_successors_local.push((target_name, Target::Exit(exit_index)));
            }
        }
        if block_successors_local.is_empty() {
            return Err(format!(
                "construct-tree:straddle-terminal-inside block={}",
                blocks[global].name
            ));
        }
        successors.push(block_successors_local);
    }

    let mut rename = HashMap::default();
    let mut phi_slots = Vec::new();
    for (host_local, &global) in ordered.iter().enumerate() {
        let carrier = blocks[global].typed.as_ref().expect("checked above");
        for inst in &carrier.insts {
            let Some((ty, incoming)) = &inst.phi_incoming() else {
                continue;
            };
            let result = inst.result.as_ref().ok_or_else(|| {
                format!(
                    "construct-tree:straddle-phi-without-result block={}",
                    blocks[global].name
                )
            })?;
            let slot_index = phi_slots.len();
            let current = format!("{PREFIX}slot.{slot_index}");
            let next = format!("{PREFIX}nextslot.{slot_index}");
            rename.insert(result.clone(), current.clone());
            let mut routed = HashMap::default();
            let mut initial = None;
            for (value, predecessor) in incoming {
                let Some(&source) = names.get(predecessor) else {
                    return Err(format!(
                        "construct-tree:straddle-phi-unknown-predecessor block={} pred={predecessor}",
                        blocks[global].name
                    ));
                };
                if let Some(&source_local) = local_of.get(&source) {
                    routed.insert(source_local, value.clone());
                } else if source == entry_from && global == entry_to {
                    initial = Some(value.clone());
                } else {
                    return Err(format!(
                        "construct-tree:straddle-phi-external-predecessor block={} pred={predecessor}",
                        blocks[global].name
                    ));
                }
            }
            phi_slots.push(PhiSlot {
                ty: ty.clone(),
                host: host_local,
                incoming: routed,
                initial: initial.unwrap_or(LlValue::Undef),
                current,
                next,
            });
        }
    }
    for slot in &mut phi_slots {
        slot.initial = tir::renamed_llvalue(&slot.initial, &rename);
        for value in slot.incoming.values_mut() {
            *value = tir::renamed_llvalue(value, &rename);
        }
    }

    let mut def_block = HashMap::default();
    for (local, &global) in ordered.iter().enumerate() {
        for inst in &blocks[global].typed.as_ref().expect("checked above").insts {
            if !inst.is_phi() {
                if let Some(result) = &inst.result {
                    def_block.insert(result.clone(), (local, inst.result_ty.clone()));
                }
            }
        }
    }

    let mut exit_payload_slots = collect_exit_payload_slots(
        &blocks,
        &closure,
        &local_of,
        &names,
        &exits,
        &exit_by_pair,
        &def_block,
    )?;
    if require_exit_payload && exit_payload_slots.is_empty() {
        return Err("construct-tree:straddle-no-exit-payload".to_string());
    }

    let mut cross_values = BTreeSet::new();
    for (local, &global) in ordered.iter().enumerate() {
        let carrier = blocks[global].typed.as_ref().expect("checked above");
        for inst in &carrier.insts {
            if inst.is_phi() {
                continue;
            }
            inst.visit_uses(|name| {
                if def_block
                    .get(name)
                    .is_some_and(|(owner, _)| *owner != local)
                {
                    cross_values.insert(name.to_string());
                }
            });
        }
        for name in terminator_uses(carrier) {
            if def_block
                .get(name.as_str())
                .is_some_and(|(owner, _)| *owner != local)
            {
                cross_values.insert(name);
            }
        }
    }
    for slot in &phi_slots {
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
    for slot in &exit_payload_slots {
        let source = local_of[&exits[slot.exit_index].0];
        let mut names = Vec::new();
        collect_value_locals(&slot.value, &mut names);
        for name in names {
            if def_block
                .get(name.as_str())
                .is_some_and(|(owner, _)| *owner != source)
            {
                cross_values.insert(name);
            }
        }
    }

    let mut value_slots = Vec::new();
    for original in cross_values {
        let Some((owner, ty)) = def_block.get(&original) else {
            continue;
        };
        let Some(ty) = ty else {
            return Err(format!(
                "construct-tree:straddle-cross-case-untyped-value owner={} value={original}",
                blocks[ordered[*owner]].name
            ));
        };
        if matches!(ty, LlType::Ptr(_)) || !typed_state_type(ty) {
            return Err(format!(
                "construct-tree:straddle-cross-case-illegal-value owner={} value={original} type={ty:?}",
                blocks[ordered[*owner]].name
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
    for slot in &mut phi_slots {
        for (&source, value) in &mut slot.incoming {
            substitute_cross_slot_value(value, &value_slot_by_name, source);
        }
    }
    for slot in &mut exit_payload_slots {
        let source = local_of[&exits[slot.exit_index].0];
        slot.value = tir::renamed_llvalue(&slot.value, &rename);
        substitute_cross_slot_value(&mut slot.value, &value_slot_by_name, source);
    }

    rewrite_entry_and_exit_phis(
        &mut blocks,
        &closure,
        entry_from,
        entry_to,
        &exit_gateway,
        &exit_payload_slots,
    )?;

    let mut wrapper = Vec::new();
    wrapper.push(passthrough(PRE, HEADER));
    let mut header_phis = vec![
        (
            PC.to_string(),
            LlType::Int(32),
            vec![
                (LlValue::Int(entry_to_local as u64), PRE.to_string()),
                (LlValue::Local(NEXT_PC.to_string()), CONTINUE.to_string()),
            ],
        ),
        (
            EXIT_ID.to_string(),
            LlType::Int(32),
            vec![
                (LlValue::Int(0), PRE.to_string()),
                (
                    LlValue::Local(NEXT_EXIT_ID.to_string()),
                    CONTINUE.to_string(),
                ),
            ],
        ),
    ];
    for slot in &phi_slots {
        header_phis.push((
            slot.current.clone(),
            slot.ty.clone(),
            vec![
                (slot.initial.clone(), PRE.to_string()),
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
    for slot in &exit_payload_slots {
        header_phis.push((
            slot.current.clone(),
            slot.ty.clone(),
            vec![
                (LlValue::Undef, PRE.to_string()),
                (LlValue::Local(slot.next.clone()), CONTINUE.to_string()),
            ],
        ));
    }
    wrapper.push(carrier_with_phis(
        HEADER,
        TEST,
        &header_phis,
        BlockRole::Normal,
    )?);
    wrapper.push(synthetic_block(
        TEST.to_string(),
        vec![
            format!("{DONE} = icmp eq i32 {PC}, {done_state}"),
            format!("br i1 {DONE}, label {EXIT}, label {DISPATCH}"),
        ],
        BlockRole::Normal,
    ));
    let switch_cases = ordered
        .iter()
        .enumerate()
        .map(|(state, &global)| format!("i32 {state}, label {}", blocks[global].name))
        .collect::<Vec<_>>()
        .join(" ");
    wrapper.push(synthetic_block(
        DISPATCH.to_string(),
        vec![format!(
            "switch i32 {PC}, label {INVALID} [ {switch_cases} ]"
        )],
        BlockRole::ConstructTreeRoute,
    ));

    let mut updates = Vec::new();
    for (case_index, &global) in ordered.iter().enumerate() {
        let mut case = blocks[global].clone();
        let carrier = std::sync::Arc::make_mut(case.typed.as_mut().expect("checked above"));
        carrier.rename(&rename);
        let substitutions = value_slots
            .iter()
            .filter(|slot| slot.owner != case_index)
            .map(|slot| {
                (
                    slot.original.clone(),
                    TypedValue {
                        ty: slot.ty.clone(),
                        value: LlValue::Local(slot.current.clone()),
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        carrier.substitute_values(&substitutions);
        carrier.insts.retain(|inst| !inst.is_phi());

        let targets = &successors[case_index];
        let join = format!("{PREFIX}casejoin.{case_index}");
        let mut route_tails = Vec::new();
        let mut route_blocks = Vec::new();
        for (edge_index, (target_name, _)) in targets.iter().enumerate() {
            let route = format!("{ROUTE}.{case_index}.{edge_index}.select");
            carrier.redirect_successor(target_name, &route);
            route_blocks.push(route_passthrough(&route, &join));
            route_tails.push(route);
        }
        wrapper.push(case);
        wrapper.extend(route_blocks);

        let mut join_phis = Vec::new();
        let per_target = |target: Target, route_tail: &str| -> Result<CaseUpdate, String> {
            let pc = match target {
                Target::Internal(local) => LlValue::Int(local as u64),
                Target::Exit(_) => LlValue::Int(done_state),
                Target::Reentry(_) => LlValue::Int(entry_to_local as u64),
            };
            let exit_id = match target {
                Target::Internal(_) => LlValue::Local(EXIT_ID.to_string()),
                Target::Exit(exit_index) => LlValue::Int(exit_index as u64),
                Target::Reentry(_) => LlValue::Local(EXIT_ID.to_string()),
            };
            let phi_slot_values = phi_slots
                .iter()
                .map(|slot| match target {
                    Target::Internal(local) if local == slot.host => {
                        slot.incoming.get(&case_index).cloned().ok_or_else(|| {
                            format!(
                                "construct-tree:straddle-phi-missing-source block={} pred={}",
                                blocks[ordered[slot.host]].name, blocks[global].name
                            )
                        })
                    }
                    Target::Reentry(exit_index) if slot.host == entry_to_local => Ok(
                        reentry_initial_value(&slot.initial, exit_index, &exit_payload_slots),
                    ),
                    _ => Ok(LlValue::Local(slot.current.clone())),
                })
                .collect::<Result<Vec<_>, String>>()?;
            let value_slot_values = value_slots
                .iter()
                .map(|slot| {
                    if slot.owner == case_index {
                        LlValue::Local(slot.original.clone())
                    } else {
                        LlValue::Local(slot.current.clone())
                    }
                })
                .collect::<Vec<_>>();
            let exit_payload_values = exit_payload_slots
                .iter()
                .map(|slot| match target {
                    Target::Exit(exit_index) if exit_index == slot.exit_index => slot.value.clone(),
                    Target::Reentry(exit_index) if exit_index == slot.exit_index => {
                        slot.value.clone()
                    }
                    _ => LlValue::Local(slot.current.clone()),
                })
                .collect::<Vec<_>>();
            Ok(CaseUpdate {
                join: route_tail.to_string(),
                pc,
                exit_id,
                phi_slots: phi_slot_values,
                value_slots: value_slot_values,
                exit_payload_slots: exit_payload_values,
            })
        };

        let update = if targets.len() == 1 {
            let target_update = per_target(targets[0].1, &route_tails[0])?;
            CaseUpdate {
                join: join.clone(),
                pc: target_update.pc,
                exit_id: target_update.exit_id,
                phi_slots: target_update.phi_slots,
                value_slots: target_update.value_slots,
                exit_payload_slots: target_update.exit_payload_slots,
            }
        } else {
            let mut target_updates = Vec::new();
            for ((_, target), route_tail) in targets.iter().zip(route_tails.iter()) {
                target_updates.push(per_target(*target, route_tail)?);
            }
            join_phis.push((
                format!("{PREFIX}casepc.{case_index}"),
                LlType::Int(32),
                target_updates
                    .iter()
                    .map(|update| (update.pc.clone(), update.join.clone()))
                    .collect(),
            ));
            join_phis.push((
                format!("{PREFIX}caseexit.{case_index}"),
                LlType::Int(32),
                target_updates
                    .iter()
                    .map(|update| (update.exit_id.clone(), update.join.clone()))
                    .collect(),
            ));
            let mut phi_values = Vec::new();
            for (slot_index, slot) in phi_slots.iter().enumerate() {
                let result = format!("{PREFIX}caseslot.{case_index}.{slot_index}");
                join_phis.push((
                    result.clone(),
                    slot.ty.clone(),
                    target_updates
                        .iter()
                        .map(|update| (update.phi_slots[slot_index].clone(), update.join.clone()))
                        .collect(),
                ));
                phi_values.push(LlValue::Local(result));
            }
            let mut exit_payload_values = Vec::new();
            for (slot_index, slot) in exit_payload_slots.iter().enumerate() {
                let result = format!("{PREFIX}casepayload.{case_index}.{slot_index}");
                join_phis.push((
                    result.clone(),
                    slot.ty.clone(),
                    target_updates
                        .iter()
                        .map(|update| {
                            (
                                update.exit_payload_slots[slot_index].clone(),
                                update.join.clone(),
                            )
                        })
                        .collect(),
                ));
                exit_payload_values.push(LlValue::Local(result));
            }
            CaseUpdate {
                join: join.clone(),
                pc: LlValue::Local(format!("{PREFIX}casepc.{case_index}")),
                exit_id: LlValue::Local(format!("{PREFIX}caseexit.{case_index}")),
                phi_slots: phi_values,
                value_slots: target_updates
                    .first()
                    .map(|update| update.value_slots.clone())
                    .unwrap_or_default(),
                exit_payload_slots: exit_payload_values,
            }
        };
        wrapper.push(carrier_with_phis(
            &join,
            MERGE,
            &join_phis,
            BlockRole::Normal,
        )?);
        updates.push(update);
    }

    wrapper.push(passthrough(INVALID, MERGE));
    let mut merge_phis = vec![
        (
            NEXT_PC.to_string(),
            LlType::Int(32),
            updates
                .iter()
                .map(|update| (update.pc.clone(), update.join.clone()))
                .chain(std::iter::once((
                    LlValue::Int(done_state),
                    INVALID.to_string(),
                )))
                .collect(),
        ),
        (
            NEXT_EXIT_ID.to_string(),
            LlType::Int(32),
            updates
                .iter()
                .map(|update| (update.exit_id.clone(), update.join.clone()))
                .chain(std::iter::once((
                    LlValue::Local(EXIT_ID.to_string()),
                    INVALID.to_string(),
                )))
                .collect(),
        ),
    ];
    for (slot_index, slot) in phi_slots.iter().enumerate() {
        merge_phis.push((
            slot.next.clone(),
            slot.ty.clone(),
            updates
                .iter()
                .map(|update| (update.phi_slots[slot_index].clone(), update.join.clone()))
                .chain(std::iter::once((
                    LlValue::Local(slot.current.clone()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    for (slot_index, slot) in value_slots.iter().enumerate() {
        merge_phis.push((
            slot.next.clone(),
            slot.ty.clone(),
            updates
                .iter()
                .map(|update| (update.value_slots[slot_index].clone(), update.join.clone()))
                .chain(std::iter::once((
                    LlValue::Local(slot.current.clone()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    for (slot_index, slot) in exit_payload_slots.iter().enumerate() {
        merge_phis.push((
            slot.next.clone(),
            slot.ty.clone(),
            updates
                .iter()
                .map(|update| {
                    (
                        update.exit_payload_slots[slot_index].clone(),
                        update.join.clone(),
                    )
                })
                .chain(std::iter::once((
                    LlValue::Local(slot.current.clone()),
                    INVALID.to_string(),
                )))
                .collect(),
        ));
    }
    wrapper.push(carrier_with_phis(
        MERGE,
        CONTINUE,
        &merge_phis,
        BlockRole::LMerge,
    )?);
    wrapper.push(passthrough(CONTINUE, HEADER));
    let external_exit_indices = exits
        .iter()
        .enumerate()
        .filter_map(|(index, (_, target))| (*target != entry_from).then_some(index))
        .collect::<Vec<_>>();
    if external_exit_indices.is_empty() {
        return Err("construct-tree:straddle-no-external-exit".to_string());
    }
    if external_exit_indices.len() == 1 {
        wrapper.push(route_passthrough(
            EXIT,
            &exit_gateway[external_exit_indices[0]],
        ));
    } else {
        let switch_targets = external_exit_indices
            .iter()
            .map(|index| format!("i32 {index}, label {}", exit_gateway[*index]))
            .collect::<Vec<_>>()
            .join(" ");
        wrapper.push(synthetic_block(
            EXIT.to_string(),
            vec![format!(
                "switch i32 {EXIT_ID}, label {} [ {switch_targets} ]",
                exit_gateway[external_exit_indices[0]]
            )],
            BlockRole::Normal,
        ));
    }
    for exit_index in external_exit_indices {
        let (_, target) = exits[exit_index];
        wrapper.push(route_passthrough(
            &exit_gateway[exit_index],
            &blocks[target].name,
        ));
    }

    let original_len = blocks.len();
    let insert_at = ordered
        .iter()
        .copied()
        .min()
        .ok_or_else(|| "construct-tree:straddle-empty-region".to_string())?;
    let mut out = Vec::with_capacity(blocks.len() - closure.len() + wrapper.len());
    let mut wrapper = Some(wrapper);
    for (index, block) in blocks.into_iter().enumerate() {
        if index == insert_at {
            out.extend(wrapper.take().expect("wrapper inserted once"));
        }
        if !closure.contains(&index) {
            out.push(block);
        }
    }
    if let Some(wrapper) = wrapper {
        out.extend(wrapper);
    }
    let outside = original_len.saturating_sub(closure.len());
    if out.len() > outside.saturating_add(MAX_REGION_BLOCKS.saturating_mul(8)) {
        return Err("construct-tree:straddle-materialization-bound".to_string());
    }
    Ok(out)
}

type DefBlock = HashMap<String, (usize, Option<LlType>)>;

#[allow(clippy::too_many_arguments)]
fn collect_exit_payload_slots(
    blocks: &[BodyBlock],
    closure: &HashSet<usize>,
    local_of: &HashMap<usize, usize>,
    names: &HashMap<String, usize>,
    exits: &[(usize, usize)],
    exit_by_pair: &HashMap<(usize, usize), usize>,
    def_block: &DefBlock,
) -> Result<Vec<ExitPayloadSlot>, String> {
    let mut slots = Vec::new();
    let is_pointer_def = |name: &str| {
        def_block
            .get(name)
            .and_then(|(_, ty)| ty.as_ref())
            .is_some_and(|ty| matches!(ty, LlType::Ptr(_)))
    };
    for (target, block) in blocks.iter().enumerate() {
        if closure.contains(&target) {
            continue;
        }
        let Some(carrier) = &block.typed else {
            continue;
        };
        for inst in &carrier.insts {
            if let Some((ty, incoming)) = &inst.phi_incoming() {
                let result = inst.result.clone().ok_or_else(|| {
                    format!(
                        "construct-tree:straddle-exit-phi-without-result block={}",
                        block.name
                    )
                })?;
                for (value, predecessor) in incoming {
                    let Some(&source) = names.get(predecessor) else {
                        continue;
                    };
                    if !closure.contains(&source) {
                        continue;
                    }
                    let exit_index = *exit_by_pair.get(&(source, target)).ok_or_else(|| {
                        format!(
                            "construct-tree:straddle-exit-phi-nonedge block={} pred={predecessor}",
                            block.name
                        )
                    })?;
                    let mut locals = Vec::new();
                    collect_value_locals(value, &mut locals);
                    if locals.iter().any(|name| is_pointer_def(name)) {
                        return Err(format!(
                            "construct-tree:straddle-exit-pointer-payload block={} phi={result}",
                            block.name
                        ));
                    }
                    if !typed_state_type(ty) {
                        return Err(format!(
                            "construct-tree:straddle-exit-illegal-payload block={} phi={result} type={ty:?}",
                            block.name
                        ));
                    }
                    let slot_index = slots.len();
                    slots.push(ExitPayloadSlot {
                        ty: ty.clone(),
                        target,
                        phi_result: result.clone(),
                        exit_index,
                        value: value.clone(),
                        current: format!("{PREFIX}payload.{slot_index}"),
                        next: format!("{PREFIX}nextpayload.{slot_index}"),
                    });
                }
                continue;
            }
            let mut escaping = None;
            inst.visit_uses(|used| {
                if escaping.is_none() && def_block.contains_key(used) {
                    escaping = Some(used.to_string());
                }
            });
            if let Some(used) = escaping {
                return Err(format!(
                    "construct-tree:straddle-nonphi-escape block={} value={used}",
                    block.name
                ));
            }
        }
        for used in terminator_uses(carrier) {
            if def_block.contains_key(&used) {
                return Err(format!(
                    "construct-tree:straddle-nonphi-escape block={} value={used}",
                    block.name
                ));
            }
        }
    }
    for (source, target) in exits {
        if !blocks[*target].typed.as_ref().is_some_and(|carrier| {
            carrier.insts.iter().any(|inst| {
                inst.phi_incoming().as_ref().is_some_and(|(_, incoming)| {
                    incoming
                        .iter()
                        .any(|(_, predecessor)| names.get(predecessor) == Some(source))
                })
            })
        }) {
            let _ = local_of[source];
        }
    }
    Ok(slots)
}

fn reentry_initial_value(
    value: &LlValue,
    exit_index: usize,
    exit_payload_slots: &[ExitPayloadSlot],
) -> LlValue {
    let LlValue::Local(name) = value else {
        return value.clone();
    };
    exit_payload_slots
        .iter()
        .find(|slot| slot.exit_index == exit_index && slot.phi_result == *name)
        .map(|slot| slot.value.clone())
        .unwrap_or_else(|| value.clone())
}

fn rewrite_entry_and_exit_phis(
    blocks: &mut [BodyBlock],
    closure: &HashSet<usize>,
    entry_from: usize,
    entry_to: usize,
    exit_gateway: &[String],
    exit_payload_slots: &[ExitPayloadSlot],
) -> Result<(), String> {
    let entry_to_name = blocks[entry_to].name.clone();
    let index_by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let block_names = blocks
        .iter()
        .map(|block| block.name.clone())
        .collect::<Vec<_>>();
    blocks[entry_from]
        .typed_mut()
        .expect("checked above")
        .redirect_successor(&entry_to_name, PRE);

    let mut payload_by_target_phi: HashMap<(usize, &str), Vec<&ExitPayloadSlot>> =
        HashMap::default();
    for slot in exit_payload_slots {
        payload_by_target_phi
            .entry((slot.target, slot.phi_result.as_str()))
            .or_default()
            .push(slot);
    }
    for (&(target, phi), slots) in &payload_by_target_phi {
        let target_name = block_names[target].clone();
        let carrier = blocks[target].typed_mut().expect("checked above");
        let inst = carrier
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some(phi) && inst.is_phi())
            .ok_or_else(|| {
                format!(
                    "construct-tree:straddle-target-phi-missing block={} phi={phi}",
                    target_name
                )
            })?;
        let incoming = inst
            .phi_incoming()
            .as_ref()
            .map(|(_, incoming)| incoming.clone())
            .ok_or_else(|| {
                format!(
                    "construct-tree:straddle-target-phi-untyped block={} phi={phi}",
                    target_name
                )
            })?;
        let mut kept = incoming
            .into_iter()
            .filter(|(_, predecessor)| {
                index_by_name
                    .get(predecessor)
                    .copied()
                    .is_none_or(|source| !closure.contains(&source))
            })
            .collect::<Vec<_>>();
        for slot in slots {
            if slot.target != entry_from {
                kept.push((
                    LlValue::Local(slot.current.clone()),
                    exit_gateway[slot.exit_index].clone(),
                ));
            }
        }
        carrier.set_phi_incomings(phi, &kept);
    }

    Ok(())
}

pub(in crate::native) fn renest_straddle_loop_merge(
    blocks: &[BodyBlock],
    reject_reason: Option<&str>,
) -> Result<Option<Vec<BodyBlock>>, String> {
    if reject_reason != Some("selection:straddle-loop-merge") {
        return Ok(None);
    }
    regional_candidate(
        derive_witness(blocks, reject_reason)
            .ok_or_else(|| "construct-tree:straddle-witness-decline".to_string())?,
        true,
    )
    .map(Some)
}

pub(in crate::native) fn renest_loop_exit_sibling(
    blocks: &[BodyBlock],
) -> Result<Option<Vec<BodyBlock>>, String> {
    if !requires_loop_exit_sibling_dispatch(blocks) {
        return Ok(None);
    }
    regional_candidate(
        derive_loop_exit_sibling_witness(blocks)
            .ok_or_else(|| "construct-tree:loop-exit-sibling-witness-decline".to_string())?,
        false,
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;

    #[test]
    fn r2_straddle_derivation_is_reject_only() {
        let admitted = vec![
            bb("%entry", &["br i1 %c, label %a, label %b"]),
            bb("%a", &["br label %merge"]),
            bb("%b", &["br label %merge"]),
            bb("%merge", &["ret void"]),
        ];
        assert!(renest_straddle_loop_merge(
            &admitted,
            structured_reject_reason(&admitted).as_deref()
        )
        .expect("reject gate")
        .is_none());
    }

    #[test]
    fn r2_straddle_reads_its_classification_from_the_caller() {
        let admitted = vec![
            bb("%entry", &["br i1 %c, label %a, label %b"]),
            bb("%a", &["br label %merge"]),
            bb("%b", &["br label %merge"]),
            bb("%merge", &["ret void"]),
        ];
        assert_eq!(
            structured_reject_reason(&admitted),
            None,
            "the fixture has to be admitted for the gate to be the only thing under test"
        );
        assert_eq!(
            renest_straddle_loop_merge(&admitted, Some("selection:straddle-loop-merge")).err(),
            Some("construct-tree:straddle-witness-decline".to_string()),
            "the gate must open on the caller's classification and decline at the witness"
        );
    }

    #[test]
    fn r2_straddle_region_wraps_two_exit_scalar_payloads() {
        let source = vec![
            bb("%entry", &["br label %owner"]),
            bb("%owner", &["br label %loop"]),
            bb(
                "%loop",
                &[
                    "%i = phi i32 [ 0, %owner ], [ %inc, %body ]",
                    "br i1 %c0, label %body, label %lmerge",
                ],
            ),
            bb(
                "%body",
                &[
                    "%inc = add i32 %i, 1",
                    "br i1 %c1, label %loop, label %lmerge",
                ],
            ),
            bb(
                "%lmerge",
                &["%v = add i32 %i, 2", "br i1 %c2, label %out0, label %out1"],
            ),
            bb("%out0", &["%x = phi i32 [ %v, %lmerge ]", "br label %done"]),
            bb("%out1", &["%y = phi i32 [ %i, %lmerge ]", "br label %done"]),
            bb("%done", &["ret void"]),
        ];
        let forest = analyze(&source);
        let loop_body = forest
            .loop_for_header("%loop")
            .expect("synthetic loop")
            .body
            .clone();
        let witness = build_witness(&source, &forest, &loop_body, "%lmerge", "%owner", "%out0")
            .expect("bounded straddle witness");
        let candidate = regional_candidate(witness, true).expect("regional wrapper");
        assert!(candidate.iter().any(|block| block.name == PRE));
        assert!(candidate.iter().any(|block| block.name == EXIT));
        for original in ["%owner", "%loop", "%body", "%lmerge"] {
            assert_eq!(
                candidate
                    .iter()
                    .filter(|block| block.name == original)
                    .count(),
                1,
                "regional original block {original} must appear once"
            );
        }
        let out0 = candidate
            .iter()
            .find(|block| block.name == "%out0")
            .expect("out0 remains outside");
        let x_incomings = out0
            .typed
            .as_ref()
            .expect("carrier")
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%x"))
            .and_then(|inst| inst.phi_incoming().as_ref())
            .map(|(_, incoming)| incoming.clone())
            .expect("rewritten out0 phi");
        assert!(
            x_incomings
                .iter()
                .any(|(_, pred)| pred.starts_with("%metal2vulkan.ct.sl.exit.gateway.")),
            "exit phi must receive a gateway payload"
        );
        assert!(
            x_incomings.iter().all(|(_, pred)| pred != "%lmerge"),
            "direct closure predecessor must be removed from outside phi"
        );
        structured_plan_construct_tree(&candidate).unwrap_or_else(|| {
            panic!(
                "regional wrapper should structure, reason={:?}",
                structured_reject_reason(&candidate)
            )
        });
        let merge_preds = candidate
            .iter()
            .filter(|block| block_successors(block).iter().any(|target| target == MERGE))
            .map(|block| block.name.clone())
            .collect::<HashSet<_>>();
        let merge = candidate
            .iter()
            .find(|block| block.name == MERGE)
            .expect("wrapper merge");
        for inst in &merge.typed.as_ref().expect("carrier").insts {
            let Some((_, incoming)) = &inst.phi_incoming() else {
                continue;
            };
            for (_, predecessor) in incoming {
                assert!(
                    merge_preds.contains(predecessor),
                    "merge phi predecessor {predecessor} must branch to {MERGE}"
                );
            }
        }
    }

    #[test]
    fn loop_exit_sibling_wrapper_owns_the_complete_selection_tail() {
        let source = vec![
            bb("%entry", &["br label %owner"]),
            bb("%owner", &["br i1 %direct, label %sibling, label %loop"]),
            bb(
                "%loop",
                &[
                    "%i = phi i32 [ 0, %owner ], [ %next, %loop_tail ]",
                    "br label %body",
                ],
            ),
            bb(
                "%body",
                &[
                    "%next = add i32 %i, 1",
                    "br i1 %leave, label %sibling, label %loop_tail",
                ],
            ),
            bb("%loop_tail", &["br i1 %again, label %loop, label %tail"]),
            bb(
                "%sibling",
                &[
                    "%value = phi i32 [ 7, %owner ], [ %next, %body ]",
                    "br label %post",
                ],
            ),
            bb("%tail", &["br label %post"]),
            bb("%post", &["br label %merge"]),
            bb("%merge", &["ret void"]),
        ];
        assert!(requires_loop_exit_sibling_dispatch(&source));
        let candidate = renest_loop_exit_sibling(&source)
            .expect("regional ownership construction")
            .expect("loop-exit sibling witness");
        assert!(candidate.iter().any(|block| block.name == PRE));
        assert!(candidate.iter().any(|block| block.name == "%post"));
        assert!(candidate.iter().any(|block| block.name == "%merge"));
        structured_plan_construct_tree(&candidate).unwrap_or_else(|| {
            panic!(
                "regional loop-exit sibling candidate should structure, reason={:?}",
                structured_reject_reason(&candidate)
            )
        });
    }
}
