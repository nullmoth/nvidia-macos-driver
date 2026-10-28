use super::construct_tree::renest::{materialize_construct_tree_roles, renest_construct_tree};
use super::construct_tree::{
    materialize_construct_routes, plan_construct_tree, ClaimedBlock, ConstructKind, ConstructNode,
    ConstructTreePlan, OriginalEdge as Edge,
};
use super::{BlockRole, BodyBlock};
use crate::fxhash::HashMap;
use crate::native::tir;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    LazyPhiOwnArm,
    TwoBoundarySharedArm,
    InLoopSwitchMultiLevelBreak,
    LoopMergeStraddle,
    CrossArmContinuation,
    ThreeLatchThreeExitNestedLoop,
    TwoLatchTwoExitPhiLoop,
    MergeInLoop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Requirements(u16);

impl Requirements {
    const OWNERSHIP: Self = Self(1 << 0);
    const BOUNDARY_ROUTES: Self = Self(1 << 1);
    const WHOLE_LOOP_FOREST: Self = Self(1 << 2);
    const SWITCH_AWARE: Self = Self(1 << 3);
    const PHI_PAYLOAD: Self = Self(1 << 4);
    const LAZY_EXECUTION: Self = Self(1 << 5);
    const NO_BLOCK_CLONE: Self = Self(1 << 6);
    const PROVED_NESTING: Self = Self(1 << 7);

    const fn of(bits: u16) -> Self {
        Self(bits)
    }

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DisprovenLever {
    PairwiseLatchExitFunnels,
    GeneralizedExitSelector,
    RegionCloneOrLargerBudget,
    PostClonePlannerReplay,
    ExistingStraddleSplit,
    PhiFunnel,
    CrossArmClone,
    LocalMergeSplit,
    TransformReordering,
    ConvergeFixpoint,
    LatchProtection,
    ForcedAdmission,
    FlatWholeFunctionRelooper,
}

impl DisprovenLever {
    fn supplies(self) -> Requirements {
        use DisprovenLever::*;
        match self {
            PairwiseLatchExitFunnels | GeneralizedExitSelector => Requirements::of(
                Requirements::PHI_PAYLOAD.0
                    | Requirements::BOUNDARY_ROUTES.0
                    | Requirements::NO_BLOCK_CLONE.0,
            ),
            RegionCloneOrLargerBudget | CrossArmClone => {
                Requirements::of(Requirements::BOUNDARY_ROUTES.0)
            }
            PostClonePlannerReplay | TransformReordering => Requirements::of(0),
            ExistingStraddleSplit | LocalMergeSplit => {
                Requirements::of(Requirements::BOUNDARY_ROUTES.0 | Requirements::NO_BLOCK_CLONE.0)
            }
            PhiFunnel => {
                Requirements::of(Requirements::PHI_PAYLOAD.0 | Requirements::NO_BLOCK_CLONE.0)
            }
            ConvergeFixpoint => {
                Requirements::of(Requirements::BOUNDARY_ROUTES.0 | Requirements::NO_BLOCK_CLONE.0)
            }
            LatchProtection => Requirements::of(
                Requirements::BOUNDARY_ROUTES.0
                    | Requirements::WHOLE_LOOP_FOREST.0
                    | Requirements::NO_BLOCK_CLONE.0,
            ),
            ForcedAdmission => Requirements::NO_BLOCK_CLONE,
            FlatWholeFunctionRelooper => Requirements::of(
                Requirements::BOUNDARY_ROUTES.0
                    | Requirements::WHOLE_LOOP_FOREST.0
                    | Requirements::SWITCH_AWARE.0
                    | Requirements::NO_BLOCK_CLONE.0,
            ),
        }
    }
}

#[derive(Debug)]
struct Fixture {
    class: &'static str,
    shape: Shape,
    constructs: Vec<ConstructNode>,
    blocks: Vec<ClaimedBlock>,
    edges: Vec<Edge>,
    requirements: Requirements,
    disproven: Vec<DisprovenLever>,
}

fn root() -> ConstructNode {
    ConstructNode {
        name: "root".to_string(),
        parent: None,
        kind: ConstructKind::Root,
    }
}

fn child(name: &str, parent: usize, kind: ConstructKind) -> ConstructNode {
    ConstructNode {
        name: name.to_string(),
        parent: Some(parent),
        kind,
    }
}

fn block(name: &str, claims: &[usize]) -> ClaimedBlock {
    ClaimedBlock {
        name: name.to_string(),
        claims: claims.to_vec(),
    }
}

fn plan(fixture: &Fixture) -> ConstructTreePlan {
    plan_construct_tree(&fixture.constructs, &fixture.blocks, &fixture.edges)
        .expect("R0 fixture must satisfy the production R1 planner")
}

fn typed_blocks(fixture: &Fixture) -> Vec<BodyBlock> {
    let labels: Vec<String> = (0..fixture.blocks.len())
        .map(|index| format!("%ct.b{index}"))
        .collect();
    let mut predecessors: Vec<Vec<usize>> = vec![Vec::new(); fixture.blocks.len()];
    let mut successors: Vec<Vec<usize>> = vec![Vec::new(); fixture.blocks.len()];
    for edge in &fixture.edges {
        if !predecessors[edge.to].contains(&edge.from) {
            predecessors[edge.to].push(edge.from);
        }
        if !successors[edge.from].contains(&edge.to) {
            successors[edge.from].push(edge.to);
        }
    }

    (0..fixture.blocks.len())
        .map(|index| {
            let mut lines = Vec::new();
            if fixture.blocks[index].name.contains("phi") && !predecessors[index].is_empty() {
                let incoming = predecessors[index]
                    .iter()
                    .enumerate()
                    .map(|(value, pred)| format!("[ {value}, {} ]", labels[*pred]))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("%ct.phi{index} = phi i32 {incoming}"));
            }
            match successors[index].as_slice() {
                [] => lines.push("ret void".to_string()),
                [target] => lines.push(format!("br label {}", labels[*target])),
                [left, right] => lines.push(format!(
                    "br i1 %ct.cond{index}, label {}, label {}",
                    labels[*left], labels[*right]
                )),
                many => {
                    let default = labels[many[0]].clone();
                    let cases = many
                        .iter()
                        .enumerate()
                        .skip(1)
                        .map(|(literal, target)| {
                            format!("i32 {literal}, label {}", labels[*target])
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    lines.push(format!(
                        "switch i32 %ct.switch{index}, label {default} [ {cases} ]"
                    ));
                }
            }
            BodyBlock {
                name: labels[index].clone(),
                role: BlockRole::Normal,
                typed: tir::lower_block_carrier(&labels[index], &lines, &HashMap::default())
                    .map(Into::into),
            }
        })
        .collect()
}

fn llvm_fixture_shell(block_count: usize) -> String {
    let mut params = Vec::new();
    for index in 0..block_count {
        params.push(format!("i1 %ct.cond{index}"));
        params.push(format!("i32 %ct.switch{index}"));
    }
    format!(
        "target triple = \"spirv-unknown-vulkan1.2\"\n\
         define void @construct_tree_fixture({}) {{\n\
         entry:\n\
           ret void\n\
         }}\n",
        params.join(", ")
    )
}

fn fixtures() -> Vec<Fixture> {
    let common = Requirements::OWNERSHIP.0
        | Requirements::BOUNDARY_ROUTES.0
        | Requirements::NO_BLOCK_CLONE.0
        | Requirements::PROVED_NESTING.0;

    vec![
        Fixture {
            class: "selection:cond-phi-shared/own-arm",
            shape: Shape::LazyPhiOwnArm,
            constructs: vec![
                root(),
                child("loop", 0, ConstructKind::Loop),
                child("outer-if", 1, ConstructKind::Selection),
                child("outer-then", 2, ConstructKind::Arm),
                child("lazy-if", 3, ConstructKind::Selection),
                child("lazy-work", 4, ConstructKind::Arm),
            ],
            blocks: vec![
                block("lazy-header", &[4]),
                block("expensive-work", &[5]),
                block("phi-arm", &[3, 5]),
                block("loop-merge", &[0, 1]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 0, to: 2 },
                Edge { from: 1, to: 2 },
                Edge { from: 2, to: 3 },
            ],
            requirements: Requirements::of(
                common
                    | Requirements::PHI_PAYLOAD.0
                    | Requirements::LAZY_EXECUTION.0
                    | Requirements::WHOLE_LOOP_FOREST.0,
            ),
            disproven: vec![
                DisprovenLever::PhiFunnel,
                DisprovenLever::ExistingStraddleSplit,
                DisprovenLever::CrossArmClone,
                DisprovenLever::TransformReordering,
            ],
        },
        Fixture {
            class: "selection:cond-shared-arm",
            shape: Shape::TwoBoundarySharedArm,
            constructs: vec![
                root(),
                child("outer-if", 0, ConstructKind::Selection),
                child("outer-arm", 1, ConstructKind::Arm),
                child("inner-if", 2, ConstructKind::Selection),
                child("inner-arm", 3, ConstructKind::Arm),
            ],
            blocks: vec![
                block("outer-entry", &[2]),
                block("inner-entry", &[3]),
                block("shared-region", &[2, 4]),
                block("boundary-a", &[0]),
                block("boundary-b", &[0]),
            ],
            edges: vec![
                Edge { from: 0, to: 2 },
                Edge { from: 1, to: 2 },
                Edge { from: 2, to: 3 },
                Edge { from: 2, to: 4 },
            ],
            requirements: Requirements::of(common | Requirements::PHI_PAYLOAD.0),
            disproven: vec![
                DisprovenLever::RegionCloneOrLargerBudget,
                DisprovenLever::PostClonePlannerReplay,
                DisprovenLever::ExistingStraddleSplit,
            ],
        },
        Fixture {
            class: "selection:cond-other",
            shape: Shape::InLoopSwitchMultiLevelBreak,
            constructs: vec![
                root(),
                child("outer-loop", 0, ConstructKind::Loop),
                child("loop-if", 1, ConstructKind::Selection),
                child("switch", 2, ConstructKind::Switch),
                child("switch-case", 3, ConstructKind::Arm),
            ],
            blocks: vec![
                block("switch-header", &[3]),
                block("case-body", &[4]),
                block("loop-continue", &[1]),
                block("loop-exit", &[0, 1, 3]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 1, to: 2 },
                Edge { from: 1, to: 3 },
            ],
            requirements: Requirements::of(
                common
                    | Requirements::WHOLE_LOOP_FOREST.0
                    | Requirements::SWITCH_AWARE.0
                    | Requirements::PHI_PAYLOAD.0,
            ),
            disproven: vec![
                DisprovenLever::LocalMergeSplit,
                DisprovenLever::TransformReordering,
                DisprovenLever::ExistingStraddleSplit,
            ],
        },
        Fixture {
            class: "selection:straddle-loop-merge",
            shape: Shape::LoopMergeStraddle,
            constructs: vec![
                root(),
                child("guard", 0, ConstructKind::Selection),
                child("guard-body", 1, ConstructKind::Arm),
                child("inner-loop", 2, ConstructKind::Loop),
            ],
            blocks: vec![
                block("loop-header", &[3]),
                block("loop-body", &[3]),
                block("shared-selection-merge", &[0, 1, 3]),
                block("guard-continuation", &[0]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 1, to: 0 },
                Edge { from: 1, to: 2 },
                Edge { from: 2, to: 3 },
            ],
            requirements: Requirements::of(common | Requirements::WHOLE_LOOP_FOREST.0),
            disproven: vec![
                DisprovenLever::ExistingStraddleSplit,
                DisprovenLever::TransformReordering,
                DisprovenLever::RegionCloneOrLargerBudget,
            ],
        },
        Fixture {
            class: "selection:cross-arm-shared",
            shape: Shape::CrossArmContinuation,
            constructs: vec![
                root(),
                child("outer-if", 0, ConstructKind::Selection),
                child("left-arm", 1, ConstructKind::Arm),
                child("right-arm", 1, ConstructKind::Arm),
                child("nested-if", 3, ConstructKind::Selection),
            ],
            blocks: vec![
                block("left-entry", &[2]),
                block("nested-entry", &[4]),
                block("shared-continuation", &[2, 3, 4]),
                block("outer-merge", &[0]),
            ],
            edges: vec![
                Edge { from: 0, to: 2 },
                Edge { from: 1, to: 2 },
                Edge { from: 2, to: 3 },
            ],
            requirements: Requirements::of(common | Requirements::PHI_PAYLOAD.0),
            disproven: vec![
                DisprovenLever::RegionCloneOrLargerBudget,
                DisprovenLever::CrossArmClone,
                DisprovenLever::PostClonePlannerReplay,
            ],
        },
        Fixture {
            class: "loop:MultipleExits+MultipleLatches[k=3,phi=1]",
            shape: Shape::ThreeLatchThreeExitNestedLoop,
            constructs: vec![
                root(),
                child("outer-loop", 0, ConstructKind::Loop),
                child("inner-loop", 1, ConstructKind::Loop),
            ],
            blocks: vec![
                block("header-phi", &[2]),
                block("latch-a", &[2]),
                block("latch-b", &[2]),
                block("latch-c", &[2]),
                block("exit-a", &[1, 2]),
                block("exit-b", &[0]),
                block("exit-c", &[0]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 1, to: 0 },
                Edge { from: 2, to: 0 },
                Edge { from: 3, to: 0 },
                Edge { from: 1, to: 4 },
                Edge { from: 2, to: 5 },
                Edge { from: 3, to: 6 },
            ],
            requirements: Requirements::of(
                common | Requirements::WHOLE_LOOP_FOREST.0 | Requirements::PHI_PAYLOAD.0,
            ),
            disproven: vec![
                DisprovenLever::PairwiseLatchExitFunnels,
                DisprovenLever::GeneralizedExitSelector,
                DisprovenLever::TransformReordering,
                DisprovenLever::FlatWholeFunctionRelooper,
            ],
        },
        Fixture {
            class: "loop:MultipleExits+MultipleLatches[k=2,phi=1]",
            shape: Shape::TwoLatchTwoExitPhiLoop,
            constructs: vec![
                root(),
                child("loop", 0, ConstructKind::Loop),
                child("post-loop-if", 0, ConstructKind::Selection),
            ],
            blocks: vec![
                block("header-phi", &[1]),
                block("latch-a", &[1]),
                block("latch-b", &[1]),
                block("exit-a", &[0, 2]),
                block("exit-b", &[0, 2]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 1, to: 0 },
                Edge { from: 2, to: 0 },
                Edge { from: 1, to: 3 },
                Edge { from: 2, to: 4 },
            ],
            requirements: Requirements::of(
                common | Requirements::WHOLE_LOOP_FOREST.0 | Requirements::PHI_PAYLOAD.0,
            ),
            disproven: vec![
                DisprovenLever::PairwiseLatchExitFunnels,
                DisprovenLever::PostClonePlannerReplay,
                DisprovenLever::TransformReordering,
            ],
        },
        Fixture {
            class: "selection:cond-phi-shared/loop-role/merge-inloop",
            shape: Shape::MergeInLoop,
            constructs: vec![
                root(),
                child("loop", 0, ConstructKind::Loop),
                child("guard", 1, ConstructKind::Selection),
                child("guard-arm", 2, ConstructKind::Arm),
            ],
            blocks: vec![
                block("guard-header", &[2]),
                block("guard-body", &[3]),
                block("phi-loop-merge", &[0, 1, 2]),
                block("loop-continue", &[1]),
            ],
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 0, to: 2 },
                Edge { from: 1, to: 3 },
                Edge { from: 3, to: 0 },
            ],
            requirements: Requirements::of(
                common | Requirements::WHOLE_LOOP_FOREST.0 | Requirements::PHI_PAYLOAD.0,
            ),
            disproven: vec![
                DisprovenLever::ConvergeFixpoint,
                DisprovenLever::LatchProtection,
                DisprovenLever::ForcedAdmission,
            ],
        },
    ]
}

#[test]
fn r0_fixture_battery_covers_exact_reject_census_classes() {
    let got: Vec<&str> = fixtures()
        .into_iter()
        .map(|fixture| fixture.class)
        .collect();
    assert_eq!(
        got,
        vec![
            "selection:cond-phi-shared/own-arm",
            "selection:cond-shared-arm",
            "selection:cond-other",
            "selection:straddle-loop-merge",
            "selection:cross-arm-shared",
            "loop:MultipleExits+MultipleLatches[k=3,phi=1]",
            "loop:MultipleExits+MultipleLatches[k=2,phi=1]",
            "selection:cond-phi-shared/loop-role/merge-inloop",
        ]
    );
}

#[test]
fn r0_fixture_shapes_are_one_to_one_with_classes() {
    let got: Vec<Shape> = fixtures()
        .into_iter()
        .map(|fixture| fixture.shape)
        .collect();
    assert_eq!(
        got,
        vec![
            Shape::LazyPhiOwnArm,
            Shape::TwoBoundarySharedArm,
            Shape::InLoopSwitchMultiLevelBreak,
            Shape::LoopMergeStraddle,
            Shape::CrossArmContinuation,
            Shape::ThreeLatchThreeExitNestedLoop,
            Shape::TwoLatchTwoExitPhiLoop,
            Shape::MergeInLoop,
        ]
    );
}

#[test]
fn r0_fixture_ownership_lifts_shared_blocks_to_one_lca() {
    for fixture in fixtures() {
        let planned = plan(&fixture);
        assert_eq!(fixture.constructs[0].kind, ConstructKind::Root);
        assert!(fixture.constructs[0].parent.is_none());
        assert!(
            fixture.blocks.iter().any(|block| block.claims.len() > 1),
            "{} must retain its recorded ownership conflict",
            fixture.class
        );
        for (index, construct) in fixture.constructs.iter().enumerate().skip(1) {
            assert!(
                construct.parent.is_some_and(|parent| parent < index),
                "{} construct {} must form a finite parent-first tree",
                fixture.class,
                construct.name
            );
        }
        assert_eq!(planned.owners.len(), fixture.blocks.len());
        assert!(
            planned.ownership_lifts > 0,
            "{} must force at least one claim to lift toward an LCA",
            fixture.class
        );
        for (index, block) in fixture.blocks.iter().enumerate() {
            assert!(!block.claims.is_empty(), "{} has no claim", block.name);
            assert!(
                planned.owners[index] < fixture.constructs.len(),
                "{} block {} has an invalid planned owner",
                fixture.class,
                block.name
            );
        }
    }
}

#[test]
fn r0_routes_are_derived_once_from_original_edges_and_are_bounded() {
    for fixture in fixtures() {
        let planned = plan(&fixture);
        assert!(
            planned
                .routes
                .iter()
                .any(|route| !route.exits.is_empty() || !route.enters.is_empty()),
            "{} must exercise a construct boundary",
            fixture.class
        );
        let route_steps: usize = planned
            .routes
            .iter()
            .map(|route| route.exits.len() + route.enters.len())
            .sum();
        assert!(
            route_steps <= fixture.edges.len() * 2 * planned.max_depth,
            "{} exceeds the edge × construct-depth construction bound",
            fixture.class
        );
        assert_eq!(
            planned.block_bound,
            fixture.blocks.len()
                + 3 * fixture.constructs.len()
                + fixture.edges.len() * (2 * planned.max_depth + 1)
        );
    }
}

#[test]
fn r1_all_r0_fixtures_materialize_typed_routes_within_the_bound() {
    for fixture in fixtures() {
        let planned = plan(&fixture);
        let blocks = typed_blocks(&fixture);
        let out = materialize_construct_routes(&blocks, &planned)
            .unwrap_or_else(|error| panic!("{} did not materialize: {error}", fixture.class));
        let expected_gateways: usize = planned
            .routes
            .iter()
            .map(|route| route.exits.len() + route.enters.len())
            .sum();
        assert_eq!(
            out.len(),
            blocks.len() + expected_gateways,
            "{} must materialize exactly its finite route steps",
            fixture.class
        );
        assert!(
            out.len() <= planned.block_bound,
            "{} must stay within its source-derived block bound",
            fixture.class
        );
        assert!(
            out.iter().all(|block| block.typed.is_some()),
            "{} must retain a typed carrier on every original and gateway block",
            fixture.class
        );
    }
}

#[test]
fn r1_all_r0_fixtures_renest_to_an_ordinary_structured_plan() {
    for fixture in fixtures() {
        let planned = plan(&fixture);
        let blocks = typed_blocks(&fixture);
        let materialized = materialize_construct_tree_roles(&blocks, &planned)
            .unwrap_or_else(|error| panic!("{} did not re-nest: {error}", fixture.class));
        for (route_index, route) in planned.routes.iter().enumerate() {
            let source_targets = fixture
                .edges
                .iter()
                .filter(|edge| edge.from == route.edge.from)
                .collect::<Vec<_>>();
            let edge_index = source_targets
                .iter()
                .position(|edge| **edge == route.edge)
                .expect("route corresponds to one source edge");
            let prefix = format!("%metal2vulkan.ct.route.{}.{edge_index}.", route.edge.from);
            let route_blocks = materialized
                .iter()
                .filter(|block| block.name.starts_with(&prefix))
                .count();
            assert_eq!(
                route_blocks,
                route.exits.len() + route.enters.len() + 1,
                "{} route {route_index} must materialize every ownership step plus its selector",
                fixture.class
            );
        }
        let original_names = blocks
            .iter()
            .map(|block| block.name.as_str())
            .collect::<Vec<_>>();
        for name in original_names {
            assert_eq!(
                materialized
                    .iter()
                    .filter(|block| block.name == name)
                    .count(),
                1,
                "{} must retain original body {name} exactly once",
                fixture.class
            );
        }
        assert!(
            materialized.len() <= planned.block_bound,
            "{} must stay within its source-derived bound",
            fixture.class
        );
        let structured = renest_construct_tree(&blocks, &planned)
            .unwrap_or_else(|error| panic!("{} did not plan: {error}", fixture.class));
        assert!(
            !structured.blocks.is_empty(),
            "{} must produce a non-empty structured plan",
            fixture.class
        );
    }
}

#[test]
fn r1_all_r0_fixtures_emit_and_pass_spirv_val() {
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_construct_tree_r1_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&tmp).expect("construct-tree temp directory");
    for fixture in fixtures() {
        let planned = plan(&fixture);
        let blocks = typed_blocks(&fixture);
        let materialized = materialize_construct_tree_roles(&blocks, &planned)
            .unwrap_or_else(|error| panic!("{} did not materialize roles: {error}", fixture.class));
        let llvm = llvm_fixture_shell(fixture.blocks.len());
        let spv = crate::native::emit_vulkan_spirv_from_typed_blocks(&llvm, materialized)
            .unwrap_or_else(|error| panic!("{} did not emit: {error}", fixture.class));
        let module = crate::spirv_module::load_bytes(spv)
            .unwrap_or_else(|error| panic!("{} did not reload: {error}", fixture.class));
        let spv = crate::passes::transform(
            module,
            crate::passes::Stage::Kernel,
            None,
            None,
            None,
            Some("construct_tree_fixture"),
        )
        .unwrap_or_else(|error| panic!("{} did not finalize: {error}", fixture.class))
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
        crate::tools::spirv_val_bytes(&spv, &tmp).unwrap_or_else(|error| {
            let asm = crate::disassemble(&spv).unwrap_or_else(|why| why);
            panic!("{} failed spirv-val: {error}\n{asm}", fixture.class)
        });
    }
}

#[test]
fn r0_c6b_whole_nested_loop_plan_has_no_transform_fixpoint() {
    let fixture = fixtures()
        .into_iter()
        .find(|fixture| fixture.shape == Shape::ThreeLatchThreeExitNestedLoop)
        .unwrap();
    let original_edges = fixture.edges.len();
    let planned = plan(&fixture);

    assert_eq!(planned.routes.len(), original_edges);
    assert_eq!(
        fixture
            .constructs
            .iter()
            .filter(|construct| construct.kind == ConstructKind::Loop)
            .count(),
        2,
        "the whole nested-loop forest is present in one immutable input"
    );
    assert!(
        planned
            .routes
            .iter()
            .filter(|route| !route.exits.is_empty())
            .count()
            >= 2,
        "multiple exits are routed in the same construction, not pairwise"
    );
}

#[test]
fn r0_recorded_local_levers_are_negative_fixtures() {
    for fixture in fixtures() {
        assert!(
            !fixture.disproven.is_empty(),
            "{} needs at least one recorded disproof",
            fixture.class
        );
        for lever in &fixture.disproven {
            assert!(
                !lever.supplies().contains(fixture.requirements),
                "{lever:?} accidentally satisfies all requirements for {}; R1 would repeat a dead end",
                fixture.class
            );
        }
    }
}
