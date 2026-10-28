use super::{
    analyze, block_successors, collect_value_locals, conditional_branch_targets, passthrough,
    route_passthrough, substitute_cross_slot_value, synthetic_block, BlockRole, BodyBlock,
    StructuredPlan, ValueSlot, CROSS_ARM_EDGE_MAX_BLOCKS,
};
use crate::fxhash::{HashMap, HashSet};

pub(in crate::native) mod renest;

const ROUTE_PREFIX: &str = "%metal2vulkan.ctroute.";

pub(in crate::native) fn requires_loop_exit_sibling_dispatch(blocks: &[BodyBlock]) -> bool {
    let forest = analyze(blocks);
    let post_dominators = super::loopforest::post_idom(blocks);
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    blocks.iter().any(|header| {
        let Some((left, right)) = conditional_branch_targets(header) else {
            return false;
        };
        let requires_dispatch =
            [(&left, &right), (&right, &left)]
                .into_iter()
                .any(|(loop_entry, sibling)| {
                    post_dominators.get(&header.name) != Some(sibling)
                        && forest.loops.iter().any(|natural_loop| {
                            natural_loop.body.iter().any(|block| block == loop_entry)
                                && !natural_loop.body.iter().any(|block| block == &header.name)
                                && !natural_loop.body.iter().any(|block| block == sibling)
                                && natural_loop.exits.iter().any(|exit| {
                                    let mut current = exit.clone();
                                    let mut seen = HashSet::default();
                                    loop {
                                        if &current == sibling {
                                            break true;
                                        }
                                        if !seen.insert(current.clone())
                                            || natural_loop
                                                .body
                                                .iter()
                                                .any(|block| block == &current)
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
                                })
                        })
                });
        requires_dispatch
    })
}

pub(in crate::native) fn renest_whole_cfg_dispatch(
    blocks: &[BodyBlock],
) -> Result<StructuredPlan, String> {
    if blocks.is_empty() {
        return Err("construct-tree:whole-cfg-empty".to_string());
    }
    if blocks.len() > CROSS_ARM_EDGE_MAX_BLOCKS {
        return Err(format!(
            "construct-tree:whole-cfg-block-limit blocks={} limit={CROSS_ARM_EDGE_MAX_BLOCKS}",
            blocks.len()
        ));
    }

    let mut nodes = vec![
        ConstructNode {
            name: "whole-cfg-root".to_string(),
            parent: None,
            kind: ConstructKind::Root,
        },
        ConstructNode {
            name: "whole-cfg-loop".to_string(),
            parent: Some(0),
            kind: ConstructKind::Loop,
        },
        ConstructNode {
            name: "whole-cfg-switch".to_string(),
            parent: Some(1),
            kind: ConstructKind::Switch,
        },
    ];
    let mut claimed = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let arm = nodes.len();
        nodes.push(ConstructNode {
            name: format!("whole-cfg-arm-{index}"),
            parent: Some(2),
            kind: ConstructKind::Arm,
        });
        claimed.push(ClaimedBlock {
            name: block.name.clone(),
            claims: vec![arm],
        });
    }

    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut edges = Vec::new();
    let mut seen = HashSet::default();
    for (from, block) in blocks.iter().enumerate() {
        for target in block_successors(block) {
            let Some(&to) = by_name.get(target.as_str()) else {
                return Err(format!(
                    "construct-tree:whole-cfg-unknown-successor from={} to={target}",
                    block.name
                ));
            };
            if seen.insert((from, to)) {
                edges.push(OriginalEdge { from, to });
            }
        }
    }
    let plan = plan_construct_tree(&nodes, &claimed, &edges)?;
    renest::renest_construct_tree(blocks, &plan)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native) enum ConstructKind {
    Root,
    Loop,
    Selection,
    Switch,
    Arm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct ConstructNode {
    pub(in crate::native) name: String,
    pub(in crate::native) parent: Option<usize>,
    pub(in crate::native) kind: ConstructKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct ClaimedBlock {
    pub(in crate::native) name: String,
    pub(in crate::native) claims: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::native) struct OriginalEdge {
    pub(in crate::native) from: usize,
    pub(in crate::native) to: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct ConstructRoute {
    pub(in crate::native) edge: OriginalEdge,
    pub(in crate::native) exits: Vec<usize>,
    pub(in crate::native) enters: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::native) struct ConstructTreePlan {
    pub(in crate::native) owners: Vec<usize>,
    pub(in crate::native) routes: Vec<ConstructRoute>,
    pub(in crate::native) max_depth: usize,
    pub(in crate::native) ownership_lifts: usize,
    pub(in crate::native) block_bound: usize,
}

fn ancestry(nodes: &[ConstructNode], mut node: usize) -> Vec<usize> {
    let mut out = vec![node];
    while let Some(parent) = nodes[node].parent {
        out.push(parent);
        node = parent;
    }
    out
}

fn least_common_ancestor(nodes: &[ConstructNode], claims: &[usize]) -> usize {
    let first = ancestry(nodes, claims[0]);
    first
        .into_iter()
        .find(|candidate| {
            claims
                .iter()
                .all(|claim| ancestry(nodes, *claim).contains(candidate))
        })
        .expect("validated tree always has root as a common ancestor")
}

pub(in crate::native) fn plan_construct_tree(
    nodes: &[ConstructNode],
    blocks: &[ClaimedBlock],
    edges: &[OriginalEdge],
) -> Result<ConstructTreePlan, String> {
    let Some(root) = nodes.first() else {
        return Err("construct-tree:no-root".to_string());
    };
    if root.parent.is_some() || root.kind != ConstructKind::Root {
        return Err("construct-tree:invalid-root".to_string());
    }

    let mut depths = vec![0usize; nodes.len()];
    for (index, node) in nodes.iter().enumerate().skip(1) {
        let Some(parent) = node.parent else {
            return Err(format!(
                "construct-tree:detached-construct name={}",
                node.name
            ));
        };
        if parent >= index {
            return Err(format!(
                "construct-tree:not-parent-first name={} parent={parent} index={index}",
                node.name
            ));
        }
        depths[index] = depths[parent]
            .checked_add(1)
            .ok_or_else(|| "construct-tree:depth-overflow".to_string())?;
    }
    let max_depth = depths.iter().copied().max().unwrap_or(0);

    let mut owners = Vec::with_capacity(blocks.len());
    let mut ownership_lifts = 0usize;
    for block in blocks {
        if block.claims.is_empty() {
            return Err(format!("construct-tree:no-claim block={}", block.name));
        }
        if let Some(claim) = block
            .claims
            .iter()
            .copied()
            .find(|claim| *claim >= nodes.len())
        {
            return Err(format!(
                "construct-tree:claim-out-of-range block={} claim={claim} constructs={}",
                block.name,
                nodes.len()
            ));
        }
        let owner = least_common_ancestor(nodes, &block.claims);
        let deepest_claim = block
            .claims
            .iter()
            .map(|claim| depths[*claim])
            .max()
            .unwrap_or(depths[owner]);
        ownership_lifts = ownership_lifts
            .checked_add(deepest_claim.saturating_sub(depths[owner]))
            .ok_or_else(|| "construct-tree:lift-overflow".to_string())?;
        owners.push(owner);
    }

    let mut routes = Vec::with_capacity(edges.len());
    for &edge in edges {
        if edge.from >= blocks.len() || edge.to >= blocks.len() {
            return Err(format!(
                "construct-tree:edge-out-of-range from={} to={} blocks={}",
                edge.from,
                edge.to,
                blocks.len()
            ));
        }
        let from_path = ancestry(nodes, owners[edge.from]);
        let to_path = ancestry(nodes, owners[edge.to]);
        let lca = *from_path
            .iter()
            .find(|node| to_path.contains(node))
            .expect("validated tree always shares root");
        let exits = from_path
            .into_iter()
            .take_while(|node| *node != lca)
            .collect();
        let mut enters: Vec<usize> = to_path
            .into_iter()
            .take_while(|node| *node != lca)
            .collect();
        enters.reverse();
        routes.push(ConstructRoute {
            edge,
            exits,
            enters,
        });
    }

    let edge_factor = max_depth
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| "construct-tree:bound-overflow".to_string())?;
    let block_bound = blocks
        .len()
        .checked_add(
            nodes
                .len()
                .checked_mul(3)
                .ok_or_else(|| "construct-tree:bound-overflow".to_string())?,
        )
        .and_then(|value| {
            edges
                .len()
                .checked_mul(edge_factor)
                .and_then(|edge_blocks| value.checked_add(edge_blocks))
        })
        .ok_or_else(|| "construct-tree:bound-overflow".to_string())?;

    debug_assert!(ownership_lifts <= blocks.len().saturating_mul(max_depth));
    debug_assert_eq!(routes.len(), edges.len());

    Ok(ConstructTreePlan {
        owners,
        routes,
        max_depth,
        ownership_lifts,
        block_bound,
    })
}

pub(in crate::native) fn materialize_construct_routes(
    blocks: &[BodyBlock],
    plan: &ConstructTreePlan,
) -> Result<Vec<BodyBlock>, String> {
    if blocks.len() != plan.owners.len() {
        return Err(format!(
            "construct-tree:block-plan-mismatch blocks={} owners={}",
            blocks.len(),
            plan.owners.len()
        ));
    }
    let mut names = HashSet::with_capacity_and_hasher(blocks.len(), Default::default());
    for block in blocks {
        if !names.insert(block.name.as_str()) {
            return Err(format!(
                "construct-tree:duplicate-block-name block={}",
                block.name
            ));
        }
        if block.typed.is_none() {
            return Err(format!(
                "construct-tree:missing-carrier block={}",
                block.name
            ));
        }
    }

    let mut seen_edges = HashSet::with_capacity_and_hasher(plan.routes.len(), Default::default());
    for route in &plan.routes {
        let edge = route.edge;
        if edge.from >= blocks.len() || edge.to >= blocks.len() {
            return Err(format!(
                "construct-tree:planned-edge-out-of-range from={} to={} blocks={}",
                edge.from,
                edge.to,
                blocks.len()
            ));
        }
        if !seen_edges.insert((edge.from, edge.to)) {
            return Err(format!(
                "construct-tree:duplicate-edge from={} to={}",
                blocks[edge.from].name, blocks[edge.to].name
            ));
        }
        let source = blocks[edge.from]
            .typed
            .as_ref()
            .expect("carrier checked above");
        if !source
            .terminator
            .successors()
            .iter()
            .any(|successor| *successor == blocks[edge.to].name)
        {
            return Err(format!(
                "construct-tree:not-a-source-edge from={} to={}",
                blocks[edge.from].name, blocks[edge.to].name
            ));
        }
    }

    let mut out = blocks.to_vec();
    let mut gateways = Vec::new();
    for (route_index, route) in plan.routes.iter().enumerate() {
        let step_count = route.exits.len() + route.enters.len();
        if step_count == 0 {
            continue;
        }
        let source_name = blocks[route.edge.from].name.clone();
        let target_name = blocks[route.edge.to].name.clone();
        let route_names: Vec<String> = (0..step_count)
            .map(|step| format!("{ROUTE_PREFIX}{route_index}.{step}"))
            .collect();
        if let Some(source) = out[route.edge.from].typed.as_mut() {
            let source = std::sync::Arc::make_mut(source);
            source.redirect_successor(&target_name, &route_names[0]);
        }
        if let Some(target) = out[route.edge.to].typed.as_mut() {
            let target = std::sync::Arc::make_mut(target);
            target.rewrite_phi_predecessor(
                &source_name,
                route_names.last().expect("non-empty route"),
            );
        }
        for (step, name) in route_names.iter().enumerate() {
            let target = route_names
                .get(step + 1)
                .cloned()
                .unwrap_or_else(|| target_name.clone());
            gateways.push(synthetic_block(
                name.clone(),
                vec![format!("br label {target}")],
                BlockRole::ConstructTreeRoute,
            ));
        }
    }
    out.extend(gateways);
    if out.len() > plan.block_bound {
        return Err(format!(
            "construct-tree:materialization-bound blocks={} bound={}",
            out.len(),
            plan.block_bound
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;
    use crate::native::ir::{LlType, LlValue, TypedValue};
    use crate::native::tir;

    fn deep_single_owner_nodes() -> Vec<ConstructNode> {
        vec![
            ConstructNode {
                name: "root".to_string(),
                parent: None,
                kind: ConstructKind::Root,
            },
            ConstructNode {
                name: "selection".to_string(),
                parent: Some(0),
                kind: ConstructKind::Selection,
            },
            ConstructNode {
                name: "arm".to_string(),
                parent: Some(1),
                kind: ConstructKind::Arm,
            },
            ConstructNode {
                name: "inner".to_string(),
                parent: Some(2),
                kind: ConstructKind::Selection,
            },
        ]
    }

    #[test]
    fn whole_cfg_dispatch_owns_loop_exit_into_selection_sibling() {
        let blocks = vec![
            bb("%entry", &["br label %outer"]),
            bb("%outer", &["br i1 %direct, label %sibling, label %loop"]),
            bb(
                "%loop",
                &[
                    "%i = phi i32 [ 0, %outer ], [ %next, %body ]",
                    "br label %body",
                ],
            ),
            bb(
                "%body",
                &[
                    "%next = add i32 %i, 1",
                    "br i1 %leave, label %sibling, label %loop",
                ],
            ),
            bb(
                "%sibling",
                &[
                    "%value = phi i32 [ 7, %outer ], [ %next, %body ]",
                    "ret void",
                ],
            ),
        ];
        assert!(
            !requires_loop_exit_sibling_dispatch(&blocks),
            "a loop exiting into its guard's join must keep the ordinary plan"
        );
        let plan = renest_whole_cfg_dispatch(&blocks).expect("whole-CFG ownership plan");
        assert!(plan
            .blocks
            .iter()
            .any(|block| block.name == "%metal2vulkan.ct.dispatch"));
        assert!(plan
            .blocks
            .iter()
            .any(|block| block.name == "%metal2vulkan.ct.header"));
        assert!(super::super::structured_plan_construct_tree(&plan.blocks).is_some());
    }

    #[test]
    fn whole_cfg_dispatch_carries_nonvoid_returns_through_typed_state() {
        let blocks = vec![
            bb("%entry", &["br i1 %condition, label %left, label %right"]),
            bb(
                "%left",
                &["%left.value = add i32 6, 1", "ret i32 %left.value"],
            ),
            bb("%right", &["ret i32 9"]),
        ];

        let plan = renest_whole_cfg_dispatch(&blocks).expect("whole-CFG ownership plan");
        let header = plan
            .blocks
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher header");
        let merge = plan
            .blocks
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.merge")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher merge");
        let exit = plan
            .blocks
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.exit")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher exit");

        assert!(header
            .insts
            .iter()
            .any(|inst| inst.result.as_deref() == Some("%metal2vulkan.ct.return")));
        assert!(merge
            .insts
            .iter()
            .any(|inst| inst.result.as_deref() == Some("%metal2vulkan.ct.nextreturn")));
        assert!(matches!(
            &exit.ret,
            tir::RetEmit::Value(TypedValue {
                ty: LlType::Int(32),
                value: LlValue::Local(value),
            }) if value == "%metal2vulkan.ct.return"
        ));
        assert!(super::super::structured_plan_construct_tree(&plan.blocks).is_some());
    }

    fn linear_claims() -> Vec<ClaimedBlock> {
        vec![
            ClaimedBlock {
                name: "%entry".to_string(),
                claims: vec![3],
            },
            ClaimedBlock {
                name: "%def".to_string(),
                claims: vec![3],
            },
            ClaimedBlock {
                name: "%use".to_string(),
                claims: vec![3],
            },
        ]
    }

    #[test]
    fn r1_core_rejects_non_parent_first_constructs() {
        let nodes = vec![
            ConstructNode {
                name: "root".to_string(),
                parent: None,
                kind: ConstructKind::Root,
            },
            ConstructNode {
                name: "bad".to_string(),
                parent: Some(1),
                kind: ConstructKind::Selection,
            },
        ];
        assert_eq!(
            plan_construct_tree(&nodes, &[], &[]),
            Err("construct-tree:not-parent-first name=bad parent=1 index=1".to_string())
        );
    }

    #[test]
    fn r1_core_lifts_sibling_claim_to_parent_and_routes_once() {
        let nodes = vec![
            ConstructNode {
                name: "root".to_string(),
                parent: None,
                kind: ConstructKind::Root,
            },
            ConstructNode {
                name: "selection".to_string(),
                parent: Some(0),
                kind: ConstructKind::Selection,
            },
            ConstructNode {
                name: "left".to_string(),
                parent: Some(1),
                kind: ConstructKind::Arm,
            },
            ConstructNode {
                name: "right".to_string(),
                parent: Some(1),
                kind: ConstructKind::Arm,
            },
        ];
        let blocks = vec![
            ClaimedBlock {
                name: "left-block".to_string(),
                claims: vec![2],
            },
            ClaimedBlock {
                name: "shared".to_string(),
                claims: vec![2, 3],
            },
        ];
        let edges = vec![OriginalEdge { from: 0, to: 1 }];
        let plan = plan_construct_tree(&nodes, &blocks, &edges).unwrap();
        assert_eq!(plan.owners, vec![2, 1]);
        assert_eq!(plan.routes.len(), 1);
        assert_eq!(plan.routes[0].exits, vec![2]);
        assert!(plan.routes[0].enters.is_empty());
        assert_eq!(plan.ownership_lifts, 1);
    }

    #[test]
    fn r1_materializer_splits_boundary_edges_and_rewrites_phi_predecessors() {
        let nodes = vec![
            ConstructNode {
                name: "root".to_string(),
                parent: None,
                kind: ConstructKind::Root,
            },
            ConstructNode {
                name: "selection".to_string(),
                parent: Some(0),
                kind: ConstructKind::Selection,
            },
            ConstructNode {
                name: "left".to_string(),
                parent: Some(1),
                kind: ConstructKind::Arm,
            },
            ConstructNode {
                name: "right".to_string(),
                parent: Some(1),
                kind: ConstructKind::Arm,
            },
        ];
        let blocks = vec![
            bb("%entry", &["br i1 %c, label %left, label %right"]),
            bb("%left", &["br label %merge"]),
            bb("%right", &["br label %merge"]),
            bb(
                "%merge",
                &["%value = phi i32 [ 7, %left ], [ 9, %right ]", "ret void"],
            ),
        ];
        let claims = vec![
            ClaimedBlock {
                name: "%entry".to_string(),
                claims: vec![1],
            },
            ClaimedBlock {
                name: "%left".to_string(),
                claims: vec![2],
            },
            ClaimedBlock {
                name: "%right".to_string(),
                claims: vec![3],
            },
            ClaimedBlock {
                name: "%merge".to_string(),
                claims: vec![2, 3],
            },
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 0, to: 2 },
            OriginalEdge { from: 1, to: 3 },
            OriginalEdge { from: 2, to: 3 },
        ];
        let plan = plan_construct_tree(&nodes, &claims, &edges).unwrap();
        let out = materialize_construct_routes(&blocks, &plan).unwrap();

        assert_eq!(
            out.iter()
                .filter(|block| block.name.starts_with(ROUTE_PREFIX))
                .count(),
            4,
            "one typed gateway for each single-level boundary edge"
        );
        assert_eq!(
            out.iter()
                .filter(|block| !block.name.starts_with(ROUTE_PREFIX))
                .map(|block| block.name.as_str())
                .collect::<Vec<_>>(),
            ["%entry", "%left", "%right", "%merge"],
            "every original body is retained exactly once"
        );
        assert!(out.iter().all(|block| block.typed.is_some()));

        let merge = out.iter().find(|block| block.name == "%merge").unwrap();
        let phi = merge
            .typed
            .as_ref()
            .unwrap()
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%value"))
            .and_then(|inst| inst.phi_incoming().as_ref())
            .map(|(_, incoming)| incoming)
            .unwrap();
        assert_eq!(phi[0].1, format!("{ROUTE_PREFIX}2.0"));
        assert_eq!(phi[1].1, format!("{ROUTE_PREFIX}3.0"));
        assert!(
            super::super::structured_plan(&out).is_some(),
            "typed gateway insertion must preserve an already-structured diamond"
        );
    }

    #[test]
    fn r1_materializer_rejects_an_edge_absent_from_the_typed_cfg() {
        let nodes = vec![ConstructNode {
            name: "root".to_string(),
            parent: None,
            kind: ConstructKind::Root,
        }];
        let blocks = vec![bb("%a", &["ret void"]), bb("%b", &["ret void"])];
        let claims = vec![
            ClaimedBlock {
                name: "%a".to_string(),
                claims: vec![0],
            },
            ClaimedBlock {
                name: "%b".to_string(),
                claims: vec![0],
            },
        ];
        let plan =
            plan_construct_tree(&nodes, &claims, &[OriginalEdge { from: 0, to: 1 }]).unwrap();
        assert_eq!(
            materialize_construct_routes(&blocks, &plan).unwrap_err(),
            "construct-tree:not-a-source-edge from=%a to=%b"
        );
    }

    #[test]
    fn r1_renester_carries_cross_case_scalar_values_through_typed_state_slots() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb("%def", &["%x = add i32 1, 2", "br label %use"]),
            bb("%use", &["%y = add i32 %x, 3", "ret void"]),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();
        let out = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap();

        let header = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .unwrap()
            .typed
            .as_ref()
            .unwrap();
        assert!(
            header
                .insts
                .iter()
                .any(|inst| inst.result.as_deref() == Some("%metal2vulkan.ct.vslot.0")),
            "the cross-case scalar def must have a current state phi"
        );

        let use_block = out
            .iter()
            .find(|block| block.name == "%use")
            .unwrap()
            .typed
            .as_ref()
            .unwrap();
        let y = use_block
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%y"))
            .unwrap();
        let mut y_uses = Vec::new();
        y.visit_uses(|name| y_uses.push(name.to_string()));
        assert_eq!(
            y_uses,
            vec!["%metal2vulkan.ct.vslot.0".to_string()],
            "later cases read the current state slot, not the out-of-scope source SSA name"
        );

        let def_block = out
            .iter()
            .find(|block| block.name == "%def")
            .unwrap()
            .typed
            .as_ref()
            .unwrap();
        assert!(
            def_block
                .insts
                .iter()
                .any(|inst| inst.result.as_deref() == Some("%x")),
            "the defining case must keep the original SSA result instead of renaming the def"
        );

        let merge = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.merge")
            .unwrap()
            .typed
            .as_ref()
            .unwrap();
        let next_slot = merge
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%metal2vulkan.ct.nextvslot.0"))
            .and_then(|inst| inst.phi_incoming().as_ref())
            .map(|(_, incoming)| incoming)
            .unwrap();
        assert!(
            next_slot
                .iter()
                .any(|(value, pred)| matches!(value, crate::native::ir::LlValue::Local(name) if name == "%x")
                    && pred == "%metal2vulkan.ct.casejoin.1"),
            "the defining case must publish its local result into the next state slot"
        );
    }

    #[test]
    fn r1_renester_carries_cross_case_pointer_values_as_typed_phis() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb("%def", &["%p = freeze ptr null", "br label %use"]),
            bb("%use", &["%v = load i32, ptr %p, align 4", "ret void"]),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();
        let out = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap();
        let header = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher header");
        let pointer_slot = header
            .insts
            .iter()
            .find(|inst| inst.result.as_deref() == Some("%metal2vulkan.ct.vslot.0"))
            .expect("pointer state slot");
        assert_eq!(pointer_slot.result_ty, Some(LlType::Ptr(0)));
        assert!(super::super::structured_plan_construct_tree(&out).is_some());
    }

    #[test]
    fn r1_renester_keeps_function_variables_out_of_pointer_state() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb("%def", &["%p = alloca i32, align 4", "br label %use"]),
            bb("%use", &["%v = load i32, ptr %p, align 4", "ret void"]),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();
        let out = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap();
        let header = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher header");

        assert!(!header.insts.iter().any(|inst| {
            inst.result
                .as_deref()
                .is_some_and(|result| result.starts_with("%metal2vulkan.ct.vslot."))
        }));
        let use_block = out
            .iter()
            .find(|block| block.name == "%use")
            .and_then(|block| block.typed.as_ref())
            .expect("typed use block");
        assert!(use_block.insts.iter().any(|inst| {
            inst.operands.iter().any(|operand| {
                matches!(operand, tir::TirOperand::Value { name, ty: LlType::Ptr(0) } if name == "%p")
            })
        }));
    }

    #[test]
    fn r1_renester_hoists_function_pointer_derivations_out_of_state() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb(
                "%def",
                &[
                    "%p = alloca [2 x i32], align 4",
                    "%alias = bitcast ptr %p to ptr",
                    "%field = getelementptr [2 x i32], ptr %alias, i64 0, i64 1",
                    "%device.field = getelementptr i32, ptr addrspace(1) %device, i64 %index",
                    "br label %use",
                ],
            ),
            bb(
                "%use",
                &[
                    "%v = load i32, ptr %field, align 4",
                    "%w = load i32, ptr addrspace(1) %device.field, align 4",
                    "ret void",
                ],
            ),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();
        let out = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap();
        let preheader = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.pre")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher preheader");
        let preheader_results = preheader
            .insts
            .iter()
            .filter_map(|inst| inst.result.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            preheader_results,
            ["%p", "%alias", "%field", "%device.field"]
        );

        let header = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher header");
        assert!(!header.insts.iter().any(|inst| {
            inst.result
                .as_deref()
                .is_some_and(|result| result.starts_with("%metal2vulkan.ct.vslot."))
        }));
        assert_eq!(
            out.iter()
                .filter_map(|block| block.typed.as_ref())
                .flat_map(|block| &block.insts)
                .filter(|inst| inst.result.as_deref() == Some("%alias"))
                .count(),
            1
        );
        assert_eq!(
            out.iter()
                .filter_map(|block| block.typed.as_ref())
                .flat_map(|block| &block.insts)
                .filter(|inst| inst.result.as_deref() == Some("%field"))
                .count(),
            1
        );
        assert!(super::super::structured_plan_construct_tree(&out).is_some());
    }

    #[test]
    fn r1_renester_carries_dynamic_function_pointer_indices() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb(
                "%def",
                &[
                    "%p = alloca [4 x i32], align 4",
                    "%index = add i64 %argument, 1",
                    "%field = getelementptr [4 x i32], ptr %p, i64 0, i64 %index",
                    "%same = load i32, ptr %field, align 4",
                    "br label %use",
                ],
            ),
            bb(
                "%use",
                &["store i32 %same, ptr %field, align 4", "ret void"],
            ),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();
        let out = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap();
        let header = out
            .iter()
            .find(|block| block.name == "%metal2vulkan.ct.header")
            .and_then(|block| block.typed.as_ref())
            .expect("typed dispatcher header");
        assert!(header.insts.iter().any(|inst| {
            inst.result.as_deref() == Some("%metal2vulkan.ct.vslot.0")
                && inst.result_ty == Some(LlType::Int(64))
        }));
        assert!(header.insts.iter().any(|inst| {
            inst.result.as_deref() == Some("%metal2vulkan.ct.ptr.0")
                && inst.opcode == "getelementptr"
        }));
        assert!(!header
            .insts
            .iter()
            .any(|inst| { inst.result_ty == Some(LlType::Ptr(0)) && inst.opcode == "phi" }));

        let defining_case = out
            .iter()
            .find(|block| block.name == "%def")
            .and_then(|block| block.typed.as_ref())
            .expect("typed defining case");
        assert!(defining_case
            .insts
            .iter()
            .any(|inst| inst.result.as_deref() == Some("%field")));
        let later_case = out
            .iter()
            .find(|block| block.name == "%use")
            .and_then(|block| block.typed.as_ref())
            .expect("typed later case");
        assert!(later_case.insts.iter().any(|inst| {
            inst.operands.iter().any(|operand| {
                matches!(operand, tir::TirOperand::Value { name, ty: LlType::Ptr(0) }
                    if name == "%metal2vulkan.ct.ptr.0")
            })
        }));
        assert!(super::super::structured_plan_construct_tree(&out).is_some());
    }

    #[test]
    fn r1_renester_rejects_opaque_images_as_generic_pointer_state() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb("%def", &["%table = freeze ptr null", "br label %use"]),
            bb(
                "%use",
                &[
                    "%image = load ptr addrspace(1), ptr %table",
                    "call void @air.write_texture_2d(ptr addrspace(1) %image)",
                    "ret void",
                ],
            ),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();

        let error = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap_err();
        assert_eq!(
            error,
            "construct-tree:cross-case-opaque-image owner=%def value=%table"
        );
    }

    #[test]
    fn r1_renester_rejects_opaque_image_phis_as_generic_pointer_state() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb(
                "%def",
                &[
                    "%image = phi ptr addrspace(1) [ null, %entry ]",
                    "br label %use",
                ],
            ),
            bb(
                "%use",
                &[
                    "call void @air.write_texture_2d(ptr addrspace(1) %image)",
                    "ret void",
                ],
            ),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();

        let error = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap_err();
        assert_eq!(
            error,
            "construct-tree:cross-case-opaque-image owner=%def value=%image"
        );
    }

    #[test]
    fn r1_renester_rejects_unresolved_cross_case_composites() {
        let nodes = deep_single_owner_nodes();
        let blocks = vec![
            bb("%entry", &["br label %def"]),
            bb(
                "%def",
                &[
                    "%aggregate = insertvalue %\"wrapper\" undef, i32 7, 0",
                    "br label %use",
                ],
            ),
            bb(
                "%use",
                &[
                    "%value = extractvalue %\"wrapper\" %aggregate, 0",
                    "ret void",
                ],
            ),
        ];
        let edges = vec![
            OriginalEdge { from: 0, to: 1 },
            OriginalEdge { from: 1, to: 2 },
        ];
        let plan = plan_construct_tree(&nodes, &linear_claims(), &edges).unwrap();

        let error = renest::materialize_construct_tree_roles(&blocks, &plan).unwrap_err();
        assert_eq!(
            error,
            "construct-tree:cross-case-unresolved-composite owner=%def value=%aggregate"
        );
    }
}
