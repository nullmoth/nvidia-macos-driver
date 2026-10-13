use super::construct_tree::{
    plan_construct_tree, ClaimedBlock, ConstructKind, ConstructNode, ConstructTreePlan,
    OriginalEdge,
};
use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::native::ir::{LlType, LlValue};
use crate::native::tir;

const PREFIX: &str = "%metal2vulkan.ct.oa.";
const OUTER_MERGE: &str = "%metal2vulkan.ct.oa.outer.merge";
const OUTER_PASS: &str = "%metal2vulkan.ct.oa.outer.pass";
const INNER_MERGE: &str = "%metal2vulkan.ct.oa.inner.merge";
const COMMON: &str = "%metal2vulkan.ct.oa.common";
const FINAL: &str = "%metal2vulkan.ct.oa.final";
const OUTER_PC: &str = "%metal2vulkan.ct.oa.outer.pc";
const INNER_PC: &str = "%metal2vulkan.ct.oa.inner.pc";
const COMMON_PC: &str = "%metal2vulkan.ct.oa.common.pc";
const OWN_ARM_MAX_BLOCKS: usize = 300;

#[derive(Clone, Debug)]
struct Witness {
    blocks: Vec<BodyBlock>,
    outer: usize,
    header: usize,
    work: usize,
    shared: usize,
    natural: usize,
    region: HashSet<usize>,
    tree: ConstructTreePlan,
    local_of: HashMap<usize, usize>,
}

fn carrier_with_phis(
    name: &str,
    target: &str,
    phis: &[(String, LlType, Vec<(LlValue, String)>)],
) -> Result<BodyBlock, String> {
    let mut carrier =
        tir::lower_block_carrier(name, &[format!("br label {target}")], &HashMap::default())
            .ok_or_else(|| format!("construct-tree:own-arm-gateway-carrier name={name}"))?;
    for (result, ty, incoming) in phis {
        carrier.push_value_phi(result, ty, incoming);
    }
    Ok(BodyBlock {
        name: name.to_string(),
        role: BlockRole::Normal,
        typed: Some(carrier.into()),
    })
}

fn derive_witness(blocks: &[BodyBlock]) -> Option<Witness> {
    if blocks.is_empty() || blocks.iter().any(|block| block.name.starts_with(PREFIX)) {
        return None;
    }
    let (lblocks, loop_merges) = forest_loop_merges(blocks, false, false);
    let forest = analyze(&lblocks);
    if forest
        .loops
        .iter()
        .any(|info| !loop_merges.contains_key(&info.header))
    {
        return None;
    }
    let (_sblocks, branch, _switch) = unique_selection_merges(&lblocks, &loop_merges, false);
    let forest = analyze(&lblocks);
    let natural_merges = selection_merges(&lblocks, &forest);
    let names: HashMap<&str, usize> = lblocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect();

    let mut found = None;
    for (header, block) in lblocks.iter().enumerate() {
        let Some((t, f)) = conditional_branch_targets(block) else {
            continue;
        };
        if branch.contains_key(&(t.clone(), f.clone())) {
            continue;
        }
        let natural_name = natural_merges.get(&block.name)?;
        if !block_has_phi(&lblocks, natural_name) {
            continue;
        }
        let has_dominated_predecessor = lblocks.iter().any(|predecessor| {
            forest.dominates(&block.name, &predecessor.name)
                && block_successors(predecessor)
                    .iter()
                    .any(|successor| successor == natural_name)
        });
        if has_dominated_predecessor {
            continue;
        }
        let t_dominated = forest.dominates(&block.name, &t);
        let f_dominated = forest.dominates(&block.name, &f);
        if t_dominated == f_dominated {
            continue;
        }
        let work = if t_dominated { &t } else { &f };
        let shared = if t_dominated { &f } else { &t };
        let mut child = block.name.as_str();
        let mut outer = None;
        while let Some(parent) = forest.idom(child) {
            let parent_block = lblocks.get(*names.get(parent)?)?;
            if conditional_branch_targets(parent_block)
                .is_some_and(|(a, b)| a == *shared || b == *shared)
            {
                outer = names.get(parent).copied();
                break;
            }
            child = parent;
        }
        found = Some((
            outer?,
            header,
            *names.get(work.as_str())?,
            *names.get(shared.as_str())?,
            *names.get(natural_name.as_str())?,
        ));
        break;
    }
    let (outer, header, work, shared, natural) = found?;
    let witness_names = [
        lblocks[outer].name.clone(),
        lblocks[header].name.clone(),
        lblocks[work].name.clone(),
        lblocks[shared].name.clone(),
        lblocks[natural].name.clone(),
    ];

    build_source_witness(blocks, witness_names)
}

fn build_source_witness(blocks: &[BodyBlock], witness_names: [String; 5]) -> Option<Witness> {
    let lblocks = blocks.to_vec();
    let forest = analyze(&lblocks);
    let names: HashMap<&str, usize> = lblocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect();
    let [outer_name, header_name, work_name, shared_name, natural_name] = &witness_names;
    let outer = *names.get(outer_name.as_str())?;
    let header = *names.get(header_name.as_str())?;
    let work = *names.get(work_name.as_str())?;
    let shared = *names.get(shared_name.as_str())?;
    let natural = *names.get(natural_name.as_str())?;
    let (outer_t, outer_f) = conditional_branch_targets(&lblocks[outer])?;
    let outer_targets =
        crate::fxhash::set_from([*names.get(outer_t.as_str())?, *names.get(outer_f.as_str())?]);
    if outer_targets != crate::fxhash::set_from([header, shared]) {
        return None;
    }

    let mut region: HashSet<usize> = lblocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (forest.dominates(&lblocks[outer].name, &block.name) && index != natural)
                .then_some(index)
        })
        .collect();
    region.insert(outer);
    if !region.contains(&header) || !region.contains(&work) || !region.contains(&shared) {
        return None;
    }
    let exits: HashSet<usize> = region
        .iter()
        .flat_map(|source| {
            block_successors(&lblocks[*source])
                .into_iter()
                .filter_map(|target| names.get(target.as_str()).copied())
                .filter(|target| !region.contains(target))
        })
        .collect();
    if exits != crate::fxhash::set_from([natural]) {
        return None;
    }

    let mut ordered: Vec<usize> = region.iter().copied().collect();
    ordered.sort_unstable();
    let local_of: HashMap<usize, usize> = ordered
        .iter()
        .copied()
        .enumerate()
        .map(|(local, global)| (global, local))
        .collect();
    let nodes = vec![
        ConstructNode {
            name: "root".to_string(),
            parent: None,
            kind: ConstructKind::Root,
        },
        ConstructNode {
            name: "enclosing-loop".to_string(),
            parent: Some(0),
            kind: ConstructKind::Loop,
        },
        ConstructNode {
            name: "outer-selection".to_string(),
            parent: Some(1),
            kind: ConstructKind::Selection,
        },
        ConstructNode {
            name: "outer-body".to_string(),
            parent: Some(2),
            kind: ConstructKind::Arm,
        },
        ConstructNode {
            name: "inner-selection".to_string(),
            parent: Some(3),
            kind: ConstructKind::Selection,
        },
        ConstructNode {
            name: "work-arm".to_string(),
            parent: Some(4),
            kind: ConstructKind::Arm,
        },
        ConstructNode {
            name: "shared-arm".to_string(),
            parent: Some(2),
            kind: ConstructKind::Arm,
        },
        ConstructNode {
            name: "owner-dispatch".to_string(),
            parent: Some(2),
            kind: ConstructKind::Switch,
        },
    ];
    let claimed = ordered
        .iter()
        .map(|index| {
            let claims = if *index == outer {
                vec![2]
            } else if *index == header {
                vec![3, 4]
            } else if forest.dominates(&lblocks[work].name, &lblocks[*index].name) {
                vec![5]
            } else if forest.dominates(&lblocks[shared].name, &lblocks[*index].name) {
                vec![5, 6]
            } else {
                vec![7]
            };
            ClaimedBlock {
                name: lblocks[*index].name.clone(),
                claims,
            }
        })
        .collect::<Vec<_>>();
    let edges = ordered
        .iter()
        .copied()
        .flat_map(|source| {
            block_successors(&lblocks[source])
                .into_iter()
                .filter_map(|target| names.get(target.as_str()).copied())
                .filter(|target| region.contains(target))
                .map({
                    let local_of = &local_of;
                    move |target| OriginalEdge {
                        from: local_of[&source],
                        to: local_of[&target],
                    }
                })
        })
        .collect::<Vec<_>>();
    let tree = plan_construct_tree(&nodes, &claimed, &edges).ok()?;
    Some(Witness {
        blocks: lblocks,
        outer,
        header,
        work,
        shared,
        natural,
        region,
        tree,
        local_of,
    })
}

fn decision_route(
    source: usize,
    edge: usize,
    steps: usize,
    target: &str,
    out: &mut Vec<BodyBlock>,
) -> String {
    let names = (0..steps.max(1))
        .map(|step| format!("{PREFIX}decision.{source}.{edge}.{step}"))
        .collect::<Vec<_>>();
    for (step, name) in names.iter().enumerate() {
        out.push(passthrough(
            name,
            names.get(step + 1).map(String::as_str).unwrap_or(target),
        ));
    }
    names[0].clone()
}

fn regional_candidate(witness: Witness) -> Result<Vec<BodyBlock>, String> {
    let Witness {
        mut blocks,
        outer,
        header,
        work,
        shared,
        natural,
        region,
        tree,
        local_of,
    } = witness;
    let names: HashMap<String, usize> = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect();
    let natural_name = blocks[natural].name.clone();
    let route_by_edge = tree
        .routes
        .iter()
        .map(|route| ((route.edge.from, route.edge.to), route))
        .collect::<HashMap<_, _>>();
    let source_forest = analyze(&blocks);
    let route_steps = |from: usize, to: usize| {
        route_by_edge
            .get(&(local_of[&from], local_of[&to]))
            .map(|route| route.exits.len() + route.enters.len() + 1)
            .unwrap_or(1)
    };
    let mut synthetic = Vec::new();

    let (outer_t, outer_f) = conditional_branch_targets(&blocks[outer])
        .ok_or_else(|| "construct-tree:own-arm-outer-not-conditional".to_string())?;
    let outer_targets = [outer_t, outer_f];
    let mut outer_incoming = Vec::new();
    for (edge, target_name) in outer_targets.iter().enumerate() {
        let target = names[target_name];
        let state = if target == header {
            0
        } else if target == shared {
            1
        } else {
            return Err("construct-tree:own-arm-outer-target".to_string());
        };
        let first = decision_route(
            outer,
            edge,
            route_steps(outer, target),
            OUTER_MERGE,
            &mut synthetic,
        );
        blocks[outer]
            .typed_mut()
            .expect("carrier")
            .redirect_successor(target_name, &first);
        outer_incoming.push((LlValue::Int(state), synthetic.last().unwrap().name.clone()));
    }
    synthetic.push(carrier_with_phis(
        OUTER_MERGE,
        OUTER_PASS,
        &[(OUTER_PC.to_string(), LlType::Int(32), outer_incoming)],
    )?);
    synthetic
        .last_mut()
        .unwrap()
        .typed_mut()
        .expect("carrier")
        .set_terminator_line(&format!(
            "switch i32 {OUTER_PC}, label {OUTER_PASS} [ i32 0, label {} ]",
            blocks[header].name
        ));
    synthetic.push(passthrough(OUTER_PASS, COMMON));

    let (inner_t, inner_f) = conditional_branch_targets(&blocks[header])
        .ok_or_else(|| "construct-tree:own-arm-inner-not-conditional".to_string())?;
    let inner_targets = [inner_t, inner_f];
    let mut inner_incoming = Vec::new();
    for (edge, target_name) in inner_targets.iter().enumerate() {
        let target = names[target_name];
        let state = if target == work {
            0
        } else if target == shared {
            1
        } else {
            return Err("construct-tree:own-arm-inner-target".to_string());
        };
        let first = decision_route(
            header,
            edge,
            route_steps(header, target),
            INNER_MERGE,
            &mut synthetic,
        );
        blocks[header]
            .typed_mut()
            .expect("carrier")
            .redirect_successor(target_name, &first);
        inner_incoming.push((LlValue::Int(state), synthetic.last().unwrap().name.clone()));
    }
    synthetic.push(carrier_with_phis(
        INNER_MERGE,
        COMMON,
        &[(INNER_PC.to_string(), LlType::Int(32), inner_incoming)],
    )?);

    let mut carry = Vec::<(String, LlType, String)>::new();
    let header_carrier = blocks[header].typed.as_ref().expect("carrier");
    let work_nodes: HashSet<usize> = region
        .iter()
        .copied()
        .filter(|index| source_forest.dominates(&blocks[work].name, &blocks[*index].name))
        .collect();
    let mut work_uses = HashSet::default();
    for index in &work_nodes {
        for inst in &blocks[*index].typed.as_ref().expect("carrier").insts {
            inst.visit_uses(|name| {
                work_uses.insert(name.to_string());
            });
        }
    }
    for inst in &header_carrier.insts {
        let (Some(result), Some(ty)) = (&inst.result, &inst.result_ty) else {
            continue;
        };
        if !work_uses.contains(result) {
            continue;
        }
        if !typed_state_type(ty) {
            return Err(format!(
                "construct-tree:own-arm-illegal-boundary-type value={result} type={ty:?}"
            ));
        }
        carry.push((
            result.clone(),
            ty.clone(),
            format!("{PREFIX}carry.{}", carry.len()),
        ));
    }
    let mut common_phis = vec![(
        COMMON_PC.to_string(),
        LlType::Int(32),
        vec![
            (LlValue::Int(1), OUTER_PASS.to_string()),
            (
                LlValue::Local(INNER_PC.to_string()),
                INNER_MERGE.to_string(),
            ),
        ],
    )];
    for (source, ty, carried) in &carry {
        common_phis.push((
            carried.clone(),
            ty.clone(),
            vec![
                (LlValue::Undef, OUTER_PASS.to_string()),
                (LlValue::Local(source.clone()), INNER_MERGE.to_string()),
            ],
        ));
    }
    synthetic.push(carrier_with_phis(COMMON, FINAL, &common_phis)?);
    synthetic
        .last_mut()
        .unwrap()
        .typed_mut()
        .expect("carrier")
        .set_terminator_line(&format!(
            "switch i32 {COMMON_PC}, label {} [ i32 0, label {} ]",
            blocks[shared].name, blocks[work].name
        ));
    for &index in &work_nodes {
        let map = carry
            .iter()
            .map(|(source, _, carried)| (source.clone(), carried.clone()))
            .collect::<HashMap<_, _>>();
        blocks[index].typed_mut().expect("carrier").rename(&map);
    }

    let mut exit_edges = region
        .iter()
        .copied()
        .flat_map(|source| {
            block_successors(&blocks[source])
                .into_iter()
                .filter_map(|target| names.get(&target).copied())
                .filter(move |target| *target == natural)
                .map(move |target| (source, target))
        })
        .collect::<Vec<_>>();
    exit_edges.sort_unstable();
    if exit_edges.is_empty() {
        return Err("construct-tree:own-arm-no-final-edges".to_string());
    }
    let natural_phis = blocks[natural]
        .typed
        .as_ref()
        .expect("carrier")
        .insts
        .iter()
        .filter_map(|inst| {
            Some((
                inst.result.clone()?,
                inst.phi_incoming().as_ref()?.0.clone(),
                inst.phi_incoming().as_ref()?.1.clone(),
            ))
        })
        .collect::<Vec<_>>();
    let mut final_phis = Vec::new();
    let mut tails = Vec::new();
    for (edge, (source, _)) in exit_edges.iter().copied().enumerate() {
        let tail = format!("{PREFIX}final.route.{edge}");
        blocks[source]
            .typed_mut()
            .expect("carrier")
            .redirect_successor(&natural_name, &tail);
        synthetic.push(passthrough(&tail, FINAL));
        tails.push((source, tail));
    }
    for (phi_index, (result, ty, incoming)) in natural_phis.iter().enumerate() {
        let payload = format!("{PREFIX}final.payload.{phi_index}");
        let by_pred: HashMap<&str, &LlValue> = incoming
            .iter()
            .map(|(value, predecessor)| (predecessor.as_str(), value))
            .collect();
        final_phis.push((
            payload.clone(),
            ty.clone(),
            tails
                .iter()
                .map(|(source, tail)| {
                    by_pred
                        .get(blocks[*source].name.as_str())
                        .map(|value| ((*value).clone(), tail.clone()))
                        .ok_or_else(|| {
                            format!(
                                "construct-tree:own-arm-natural-phi-missing pred={}",
                                blocks[*source].name
                            )
                        })
                })
                .collect::<Result<Vec<_>, String>>()?,
        ));
        let mut kept = incoming
            .iter()
            .filter(|(_, predecessor)| {
                names
                    .get(predecessor)
                    .is_none_or(|source| !region.contains(source))
            })
            .cloned()
            .collect::<Vec<_>>();
        kept.push((LlValue::Local(payload), FINAL.to_string()));
        blocks[natural]
            .typed_mut()
            .expect("carrier")
            .set_phi_incomings(result, &kept);
    }
    synthetic.push(carrier_with_phis(FINAL, &natural_name, &final_phis)?);

    for target in [header, work, shared] {
        if blocks[target]
            .typed
            .as_ref()
            .expect("carrier")
            .insts
            .iter()
            .any(|inst| inst.is_phi())
        {
            return Err(format!(
                "construct-tree:own-arm-entry-phi-unsupported block={}",
                blocks[target].name
            ));
        }
    }

    let original_len = blocks.len();
    if exit_edges.iter().any(|(source, _)| *source >= natural) {
        return Err("construct-tree:own-arm-final-source-after-natural".to_string());
    }
    let outer_decision_prefix = format!("{PREFIX}decision.{outer}.");
    let inner_decision_prefix = format!("{PREFIX}decision.{header}.");
    let mut after_outer = Vec::new();
    let mut after_header = Vec::new();
    let mut before_natural = Vec::new();
    for block in synthetic {
        if block.name.starts_with(&outer_decision_prefix)
            || matches!(block.name.as_str(), OUTER_MERGE | OUTER_PASS)
        {
            after_outer.push(block);
        } else if block.name.starts_with(&inner_decision_prefix)
            || matches!(block.name.as_str(), INNER_MERGE | COMMON)
        {
            after_header.push(block);
        } else {
            before_natural.push(block);
        }
    }
    let mut ordered = Vec::with_capacity(
        blocks.len() + after_outer.len() + after_header.len() + before_natural.len(),
    );
    for (index, block) in blocks.into_iter().enumerate() {
        if index == natural {
            ordered.append(&mut before_natural);
        }
        ordered.push(block);
        if index == outer {
            ordered.append(&mut after_outer);
        }
        if index == header {
            ordered.append(&mut after_header);
        }
    }
    if !after_outer.is_empty() || !after_header.is_empty() || !before_natural.is_empty() {
        return Err("construct-tree:own-arm-gateway-placement".to_string());
    }
    blocks = ordered;
    let outside = original_len.saturating_sub(region.len());
    if blocks.len() > outside.saturating_add(tree.block_bound) {
        return Err("construct-tree:own-arm-materialization-bound".to_string());
    }
    Ok(blocks)
}

pub(in crate::native) fn renest_cond_phi_shared_own_arm(
    blocks: &[BodyBlock],
    reject_reason: Option<&str>,
) -> Result<Option<Vec<BodyBlock>>, String> {
    if blocks.len() > OWN_ARM_MAX_BLOCKS {
        return Ok(None);
    }
    if reject_reason != Some("selection:cond-phi-shared/own-arm") {
        return Ok(None);
    }
    regional_candidate(
        derive_witness(blocks)
            .ok_or_else(|| "construct-tree:own-arm-witness-decline".to_string())?,
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;

    #[test]
    fn r2_own_arm_derivation_is_reject_only() {
        let admitted = vec![
            bb("%entry", &["br i1 %c, label %a, label %b"]),
            bb("%a", &["br label %merge"]),
            bb("%b", &["br label %merge"]),
            bb("%merge", &["ret void"]),
        ];
        assert!(renest_cond_phi_shared_own_arm(
            &admitted,
            structured_reject_reason(&admitted).as_deref()
        )
        .expect("reject gate")
        .is_none());
    }

    fn lazy_own_arm_loop() -> Vec<BodyBlock> {
        vec![
            bb("%entry", &["br label %loop"]),
            bb("%loop", &["br i1 %c4, label %outer, label %natural"]),
            bb("%outer", &["br i1 %c0, label %inner, label %shared"]),
            bb("%inner", &["br i1 %c1, label %work, label %shared"]),
            bb("%work", &["br label %join"]),
            bb("%shared", &["br i1 %c2, label %join, label %natural"]),
            bb("%join", &["br label %natural"]),
            bb(
                "%natural",
                &[
                    "%payload = phi i32 [ 0, %loop ], [ 1, %shared ], [ 2, %join ]",
                    "br label %latch",
                ],
            ),
            bb("%latch", &["br i1 %c3, label %loop, label %exit"]),
            bb("%exit", &["ret void"]),
        ]
    }

    #[test]
    fn r2_own_arm_materializes_once_from_the_source_witness() {
        let source = lazy_own_arm_loop();
        let witness = build_source_witness(
            &source,
            [
                "%outer".to_string(),
                "%inner".to_string(),
                "%work".to_string(),
                "%shared".to_string(),
                "%natural".to_string(),
            ],
        )
        .expect("own-arm source witness");
        let candidate = regional_candidate(witness).expect("own-arm construction");
        assert!(candidate.len() > source.len());
        for original in &source {
            assert_eq!(
                candidate
                    .iter()
                    .filter(|block| block.name == original.name)
                    .count(),
                1,
                "original block {} must be emitted once",
                original.name
            );
        }
        let plan =
            structured_plan_construct_tree(&candidate).expect("construct-tree ownership plan");
        assert!(plan
            .blocks
            .iter()
            .any(|block| block.name == "%metal2vulkan.ct.oa.common"));
    }

    #[test]
    fn r2_own_arm_reads_its_classification_from_the_caller() {
        let source = lazy_own_arm_loop();
        assert_eq!(
            structured_reject_reason(&source),
            None,
            "the fixture has to be admitted for the gate to be the only thing under test"
        );
        assert!(
            renest_cond_phi_shared_own_arm(&source, None)
                .expect("no classification, no work")
                .is_none(),
            "without the caller's classification the recognizer must decline"
        );
        assert!(
            !matches!(
                renest_cond_phi_shared_own_arm(&source, Some("selection:cond-phi-shared/own-arm")),
                Ok(None)
            ),
            "handed the own-arm classification, the gate must open on the same graph"
        );
    }

    #[test]
    fn r2_own_arm_large_input_declines_before_witness_derivation() {
        let mut source = lazy_own_arm_loop();
        while source.len() <= OWN_ARM_MAX_BLOCKS {
            let name = format!("%pad{}", source.len());
            source.push(bb(&name, &["ret void"]));
        }

        assert!(renest_cond_phi_shared_own_arm(
            &source,
            structured_reject_reason(&source).as_deref()
        )
        .expect("large own-arm gate")
        .is_none());
    }
}
