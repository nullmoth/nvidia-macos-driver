use super::blocks::{block_successors, conditional_branch_targets};
use super::loopforest::{analyze, post_idom, selection_merges};
use super::structured_emit::role_for_name;
use super::BodyBlock;
use crate::fxhash::{HashMap, HashSet};

mod detect;
pub(in crate::native) use detect::*;
mod privatize;
pub(in crate::native) use privatize::*;
mod finders;
pub(in crate::native) use finders::*;
mod cross_arm;
pub(in crate::native) use cross_arm::*;
mod normalize;
pub(in crate::native) use normalize::*;

const MAX_REGION_BLOCKS: usize = 96;
const MAX_REGION_BOUNDARIES: usize = 4;
const MAX_SINGLE_BOUNDARY_REGION_BLOCKS: usize = 1152;
const MAX_REGION_CLONE_GROWTH: usize = MAX_SINGLE_BOUNDARY_REGION_BLOCKS * 2;
const MAX_ROUNDS: usize = 64;
const MAX_DEEP_SHARED_CONTINUATION_BLOCKS: usize = 300;
const DEEP_SHARED_COUNTER_START: usize = 4_000_000;
const SWITCH_CASE_COUNTER_START: usize = 5_000_000;
const SHARED_EXIT_COUNTER_START: usize = 6_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(name: &str, lines: &[&str]) -> BodyBlock {
        let name = format!("%{name}");
        let lines: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let typed = crate::native::tir::lower_block_carrier(&name, &lines, &HashMap::default());
        BodyBlock {
            name,
            role: crate::native::cfg::BlockRole::Normal,
            typed: typed.map(Into::into),
        }
    }

    #[test]
    fn clone_source_name_inverts_fresh() {
        for original in [
            "%1",
            "%cont",
            "%metal2vulkan.helper.5.3.param.1",
            "%xa7_already_a_clone",
        ] {
            for id in [
                0usize,
                7,
                DEEP_SHARED_COUNTER_START,
                SWITCH_CASE_COUNTER_START,
            ] {
                assert_eq!(
                    clone_source_name(&fresh(original, id)).as_deref(),
                    Some(original),
                    "fresh({original}, {id}) does not invert"
                );
            }
        }
        for ordinary in ["%1", "%xa_1", "%xax7_1", "%xa7", "%metal2vulkan.uret", "%x"] {
            assert_eq!(
                clone_source_name(ordinary),
                None,
                "{ordinary} is not a clone"
            );
        }
    }

    #[test]
    fn rename_is_boundary_aware() {
        let mut map = HashMap::default();
        map.insert("%1".to_string(), "%xa0_1".to_string());
        assert_eq!(
            rename_tokens("  %r = add i32 %1, %10", &map),
            "  %r = add i32 %xa0_1, %10"
        );
    }

    #[test]
    fn rebuild_phi_keeps_selected_preds() {
        let line = "  %d = phi i32 [ %a, %p1 ], [ %b, %p2 ]";
        let kept = rebuild_phi(line, |p| p == "%p1").unwrap();
        assert_eq!(kept, "  %d = phi i32 [ %a, %p1 ]");
        let dropped = rebuild_phi(line, |p| p == "%nope");
        assert!(dropped.is_none());
    }

    #[test]
    fn line_def_extracts_result() {
        assert_eq!(line_def("  %5 = add i32 %1, %2"), Some("%5".to_string()));
        assert_eq!(line_def("  store i32 %1, ptr %2"), None);
        assert_eq!(line_def("  br label %3"), None);
    }

    #[test]
    fn detects_and_clones_inner_header_cross_arm() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %taken, label %checkb"]),
            blk("checkb", &["br i1 %cb, label %taken, label %elseb"]),
            blk("taken", &["ret void"]),
            blk("elseb", &["ret void"]),
        ];
        assert_eq!(
            find_cross_arm(&blocks),
            Some(("%checkb".to_string(), "%taken".to_string()))
        );
        let mut counter = 0;
        let out = privatize_region(&blocks, "%checkb", "%taken", &mut counter)
            .expect("should apply a clone");
        let clone = out
            .iter()
            .find(|b| b.name.starts_with("%xa"))
            .expect("clone block present");
        assert_eq!(clone.lines(), vec!["ret void".to_string()]);
        let checkb = out.iter().find(|b| b.name == "%checkb").unwrap();
        assert!(checkb.lines().last().unwrap().contains(&clone.name));
        assert!(
            !checkb.lines().last().unwrap().contains("%taken,")
                || checkb.lines().last().unwrap().contains(&clone.name)
        );
        let entry = out.iter().find(|b| b.name == "%entry").unwrap();
        assert!(entry.lines().last().unwrap().contains("%taken"));
    }

    #[test]
    fn find_cross_arm_preserves_source_terminator_arm_order() {
        let blocks = vec![
            blk("entry", &["br i1 %outer, label %shared_a, label %gate"]),
            blk("gate", &["br i1 %middle, label %shared_b, label %inner"]),
            blk(
                "inner",
                &["br i1 %nested, label %shared_b, label %shared_a"],
            ),
            blk("shared_a", &["ret void"]),
            blk("shared_b", &["ret void"]),
        ];

        assert_eq!(
            find_cross_arm(&blocks),
            Some(("%inner".to_string(), "%shared_b".to_string())),
            "when both arms are shared, choose the first source terminator arm"
        );
    }

    #[test]
    fn clones_shared_merge_via_full_closure() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %taken, label %checkb"]),
            blk("checkb", &["br i1 %cb, label %taken, label %elseb"]),
            blk("taken", &["br label %end"]),
            blk("elseb", &["br label %end"]),
            blk("end", &["ret void"]),
        ];
        assert_eq!(
            find_cross_arm(&blocks),
            Some(("%checkb".to_string(), "%taken".to_string()))
        );
        let mut counter = 0;
        let out = privatize_region(&blocks, "%checkb", "%taken", &mut counter)
            .expect("full-closure clone applies");
        assert_eq!(out.iter().filter(|b| b.name.starts_with("%xa")).count(), 2);
        let checkb = out.iter().find(|b| b.name == "%checkb").unwrap();
        let term = checkb.lines().last().unwrap().clone();
        assert!(term.contains("%xa"));
        let end = out.iter().find(|b| b.name == "%end").unwrap();
        assert_eq!(end.lines(), vec!["ret void".to_string()]);
    }

    #[test]
    fn dominated_region_clone_witness_reports_cloneable_shape() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %taken, label %checkb"]),
            blk("checkb", &["br i1 %cb, label %taken, label %elseb"]),
            blk("taken", &["br label %end"]),
            blk("elseb", &["br label %end"]),
            blk("end", &["ret void"]),
        ];

        let witness = dominated_region_clone_witness(&blocks, "%checkb", "%taken");
        assert_eq!(witness.reason, "cloneable");
        assert_eq!(witness.region_blocks, 1);
        assert_eq!(witness.region_cap, MAX_SINGLE_BOUNDARY_REGION_BLOCKS);
        assert_eq!(witness.boundary_count, 1);
        assert_eq!(witness.boundary_cap, MAX_REGION_BOUNDARIES);
        assert_eq!(witness.redirect_count, 1);
        assert_eq!(witness.external_pred_count, 1);
        assert_eq!(witness.arm_cycle_pred_count, 0);
    }

    #[test]
    fn region_fixpoint_witness_reports_stop_after_successful_clone() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %taken, label %checkb"]),
            blk("checkb", &["br i1 %cb, label %taken, label %elseb"]),
            blk("taken", &["br label %end"]),
            blk("elseb", &["br label %end"]),
            blk("end", &["ret void"]),
        ];

        let (out, witness) = privatize_region_cross_arm_with_witness(&blocks);
        assert_eq!(out.len(), 6);
        assert_eq!(witness.input_blocks, 5);
        assert_eq!(witness.output_blocks, 6);
        assert_eq!(witness.rounds, 1);
        assert_eq!(witness.stop_reason, "no_cross_arm");
        assert!(witness.next_blocks.is_none());
        assert!(witness.stop_candidate.is_none());
    }

    #[test]
    fn privatize_trivial_clones_shared_passthrough_arm() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert_eq!(
            find_trivial_cross_arm(&blocks),
            Some(("%inner".to_string(), "%shared".to_string()))
        );
        let out = privatize_trivial_cross_arm(&blocks);
        let clones: Vec<&BodyBlock> = out.iter().filter(|b| b.name.starts_with("%xa")).collect();
        assert_eq!(clones.len(), 1);
        assert_eq!(clones[0].lines(), vec!["br label %merge".to_string()]);
        let inner = out.iter().find(|b| b.name == "%inner").unwrap();
        assert!(inner.lines().last().unwrap().contains(&clones[0].name));
        assert!(out.iter().any(|b| b.name == "%shared"));
        assert!(super::super::structured_emit::structured_plan(&blocks).is_some());
    }

    #[test]
    fn privatize_trivial_mirrors_successor_phi_incoming() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk(
                "merge",
                &["%p = phi i32 [ %v, %shared ], [ %w, %elseb ]", "ret void"],
            ),
        ];
        let out = privatize_trivial_cross_arm(&blocks);
        let clone_name = out
            .iter()
            .find(|b| b.name.starts_with("%xa"))
            .unwrap()
            .name
            .clone();
        let merge = out.iter().find(|b| b.name == "%merge").unwrap();
        let phi = merge.lines()[0].clone();
        assert!(phi.contains("[ %v, %shared ]"), "phi = {phi}");
        assert!(
            phi.contains(&format!("[ %v, {clone_name} ]")),
            "phi = {phi}"
        );
        assert!(phi.contains("[ %w, %elseb ]"), "phi = {phi}");
    }

    #[test]
    fn privatize_trivial_clones_arm_with_dead_def() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk(
                "shared",
                &[
                    "%d = add i32 %x, 1",
                    "%u = mul i32 %d, 2",
                    "store i32 %u, ptr %p",
                    "br label %merge",
                ],
            ),
            blk("elseb", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert_eq!(
            find_trivial_cross_arm(&blocks),
            Some(("%inner".to_string(), "%shared".to_string()))
        );
        let out = privatize_trivial_cross_arm(&blocks);
        let clone = out.iter().find(|b| b.name.starts_with("%xa")).unwrap();
        assert!(clone
            .lines()
            .iter()
            .any(|l| l.contains("%xa") && l.contains("add")));
        assert!(clone.lines().iter().any(|l| l.contains("store")));
        assert!(!clone
            .lines()
            .iter()
            .any(|l| line_def(l) == Some("%d".to_string())));
    }

    #[test]
    fn privatize_trivial_clones_arm_def_used_only_in_successor_phi() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["%d = add i32 %x, 1", "br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk(
                "merge",
                &["%p = phi i32 [ %d, %shared ], [ %w, %elseb ]", "ret void"],
            ),
        ];
        assert_eq!(
            find_trivial_cross_arm(&blocks),
            Some(("%inner".to_string(), "%shared".to_string()))
        );
        let out = privatize_trivial_cross_arm(&blocks);
        let clone = out.iter().find(|b| b.name.starts_with("%xa")).unwrap();
        let clone_name = clone.name.clone();
        let renamed = clone.lines().iter().find_map(|l| line_def(l)).unwrap();
        assert_ne!(renamed, "%d");
        let merge = out.iter().find(|b| b.name == "%merge").unwrap();
        let phi = merge.lines()[0].clone();
        assert!(phi.contains("[ %d, %shared ]"), "phi = {phi}");
        assert!(
            phi.contains(&format!("[ {renamed}, {clone_name} ]")),
            "phi = {phi}"
        );
    }

    #[test]
    fn clone_declines_label_that_is_also_an_ssa_operand() {
        let blocks = vec![
            blk(
                "%entry",
                &["%value = add i32 %shared, 1", "br label %shared"],
            ),
            blk("%shared", &["ret void"]),
        ];
        assert!(cloned_labels_overlap_ssa_values(
            &blocks,
            &crate::fxhash::set_from(["%shared".to_string()])
        ));
    }

    #[test]
    fn privatize_trivial_skips_arm_def_used_in_successor_body() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["%d = add i32 %x, 1", "br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk("merge", &["%q = mul i32 %d, 3", "ret void"]),
        ];
        assert_eq!(find_trivial_cross_arm(&blocks), None);
        assert_eq!(privatize_trivial_cross_arm(&blocks).len(), blocks.len());
    }

    #[test]
    fn privatize_trivial_clones_arm_with_phi_partitions_incomings() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk(
                "shared",
                &[
                    "%p = phi i32 [ %a, %entry ], [ %b, %inner ]",
                    "br label %merge",
                ],
            ),
            blk("elseb", &["br label %merge"]),
            blk(
                "merge",
                &["%m = phi i32 [ %p, %shared ], [ %w, %elseb ]", "ret void"],
            ),
        ];
        assert_eq!(
            find_trivial_cross_arm(&blocks),
            Some(("%inner".to_string(), "%shared".to_string()))
        );
        let out = privatize_trivial_cross_arm(&blocks);
        let shared = out.iter().find(|b| b.name == "%shared").unwrap();
        assert!(
            shared.lines()[0].contains("[ %a, %entry ]"),
            "{}",
            shared.lines()[0]
        );
        assert!(
            !shared.lines()[0].contains("%inner"),
            "{}",
            shared.lines()[0]
        );
        let clone = out.iter().find(|b| b.name.starts_with("%xa")).unwrap();
        let renamed = clone.lines().iter().find_map(|l| line_def(l)).unwrap();
        assert_ne!(renamed, "%p");
        assert!(
            clone.lines()[0].contains("[ %b, %inner ]"),
            "{}",
            clone.lines()[0]
        );
        assert!(!clone.lines()[0].contains("%entry"), "{}", clone.lines()[0]);
        let merge = out.iter().find(|b| b.name == "%merge").unwrap();
        assert!(
            merge.lines()[0].contains("[ %p, %shared ]"),
            "{}",
            merge.lines()[0]
        );
        assert!(
            merge.lines()[0].contains(&format!("[ {renamed}, {} ]", clone.name)),
            "{}",
            merge.lines()[0]
        );
    }

    #[test]
    fn unify_returns_void_makes_divergent_selection_structurable() {
        let blocks = vec![
            blk("h", &["br i1 %c, label %a, label %b"]),
            blk("a", &["ret void"]),
            blk("b", &["ret void"]),
        ];
        assert!(super::super::structured_emit::structured_plan_ladder(&blocks, false).is_some());
        let out = unify_returns(&blocks).expect("two rets to unify");
        let a = out.iter().find(|x| x.name == "%a").unwrap();
        let b = out.iter().find(|x| x.name == "%b").unwrap();
        assert_eq!(a.lines().last(), b.lines().last());
        assert!(a
            .lines()
            .last()
            .unwrap()
            .starts_with(format!("br label {URET_PREFIX}").as_str()));
        assert!(super::super::structured_emit::structured_plan(&out).is_some());
        let plan = super::super::structured_emit::structured_plan(&out)
            .expect("the explicitly unified graph must admit");
        assert!(
            plan.blocks
                .iter()
                .any(|block| block.name.starts_with(URET_PREFIX)),
            "admitted plan must contain the synthesized shared exit"
        );
    }

    #[test]
    fn divergent_unreachable_and_return_gain_shared_exit() {
        let blocks = vec![
            blk("h", &["br i1 %c, label %a, label %b"]),
            blk("a", &["unreachable"]),
            blk("b", &["ret void"]),
        ];
        let candidate = separate_divergent_selection_exits(&blocks)
            .expect("return-like divergent exits must normalize");
        assert_eq!(
            candidate
                .iter()
                .filter(|block| block.name.starts_with(URET_PREFIX))
                .count(),
            1
        );
        assert!(super::super::structured_emit::structured_plan(&candidate).is_some());
    }

    #[test]
    fn shared_phi_exit_predecessor_is_privatized_for_nested_selection() {
        let blocks = vec![
            blk("entry", &["br i1 %outer, label %ret, label %h"]),
            blk("h", &["br i1 %c, label %a, label %b"]),
            blk("a", &["br label %ret"]),
            blk("b", &["unreachable"]),
            blk(
                "ret",
                &["%rv = phi i32 [ 1, %entry ], [ 2, %a ]", "ret i32 %rv"],
            ),
        ];
        assert!(super::super::structured_emit::structured_plan_ladder(&blocks, false).is_some());
        let unified =
            separate_divergent_selection_exits(&blocks).expect("return-like exits must unify");
        let private = privatize_shared_phi_exit_predecessors(&unified);
        assert!(
            private.len() > unified.len(),
            "the shared return predecessor must gain a private clone"
        );
        assert!(
            super::super::structured_emit::structured_plan_ladder(&private, false).is_some(),
            "the ordinary ladder must admit after shared-exit separation"
        );
        assert!(
            super::super::structured_emit::structured_plan(&blocks).is_some(),
            "the reject-only C1 construction must compose both transforms"
        );
    }

    #[test]
    fn unify_returns_value_builds_phi() {
        let blocks = vec![
            blk("h", &["br i1 %c, label %a, label %b"]),
            blk("a", &["ret i32 %va"]),
            blk("b", &["ret i32 %vb"]),
        ];
        let out = unify_returns(&blocks).expect("two rets to unify");
        let exit = out
            .iter()
            .find(|x| x.name.starts_with(URET_PREFIX))
            .unwrap();
        assert_eq!(
            exit.lines()[0],
            format!("{URET_PREFIX}.v = phi i32 [ %va, %a ], [ %vb, %b ]").as_str()
        );
        assert_eq!(exit.lines()[1], format!("ret i32 {URET_PREFIX}.v").as_str());
        assert!(super::super::structured_emit::structured_plan(&out).is_some());
    }

    #[test]
    fn privatize_dominated_region_clones_multi_successor_arm() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["br i1 %cc, label %left, label %right"]),
            blk("left", &["br label %merge"]),
            blk("right", &["br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        let out = privatize_region_cross_arm(&blocks);
        let clones: Vec<&BodyBlock> = out.iter().filter(|b| b.name.starts_with("%xa")).collect();
        assert_eq!(clones.len(), 3, "shared+left+right cloned, merge preserved");
        assert_eq!(
            clones
                .iter()
                .map(|block| block.name.as_str())
                .collect::<Vec<_>>(),
            ["%xa0_shared", "%xa0_left", "%xa0_right"],
            "cloned blocks preserve their source block order"
        );
        assert_eq!(out.iter().filter(|b| b.name == "%merge").count(), 1);
        assert!(super::super::structured_emit::structured_plan(&out).is_some());
    }

    #[test]
    fn privatize_dominated_region_mirrors_boundary_phi() {
        let blocks = vec![
            blk("entry", &["br i1 %ca, label %shared, label %inner"]),
            blk("inner", &["br i1 %cb, label %shared, label %elseb"]),
            blk("shared", &["br i1 %cc, label %left, label %right"]),
            blk("left", &["%vl = add i32 1, 1", "br label %merge"]),
            blk("right", &["%vr = add i32 2, 2", "br label %merge"]),
            blk("elseb", &["br label %merge"]),
            blk(
                "merge",
                &[
                    "%p = phi i32 [ %vl, %left ], [ %vr, %right ], [ 0, %elseb ]",
                    "ret void",
                ],
            ),
        ];
        let out = privatize_region_cross_arm(&blocks);
        let merge = out.iter().find(|b| b.name == "%merge").unwrap();
        let phi = merge
            .lines()
            .iter()
            .find(|l| l.contains("phi"))
            .unwrap()
            .clone();
        assert_eq!(
            phi.matches('[').count(),
            5,
            "3 original + 2 mirrored incomings"
        );
        assert!(phi.contains("%elseb"));
        assert!(super::super::structured_emit::structured_plan(&out).is_some());
    }

    #[test]
    fn privatize_deep_shared_continuations_clones_nested_shared_tail() {
        let blocks = vec![
            blk("entry", &["br i1 %co, label %inner, label %outer"]),
            blk("inner", &["br i1 %ci, label %direct, label %inner_else"]),
            blk("direct", &["br label %shared"]),
            blk(
                "inner_else",
                &["br i1 %ce, label %outer_merge, label %tail"],
            ),
            blk("outer", &["br label %tail"]),
            blk("tail", &["br label %shared"]),
            blk("shared", &["br label %outer_merge"]),
            blk("outer_merge", &["ret void"]),
        ];
        assert!(
            find_deep_shared_continuations(&blocks)
                .contains(&("%inner".to_string(), "%tail".to_string())),
            "the outer nested header sees its externally reachable continuation"
        );
        let out = privatize_deep_shared_continuations(&blocks);
        let clones: Vec<&BodyBlock> = out.iter().filter(|b| b.name.starts_with("%xa")).collect();
        assert_eq!(
            clones.len(),
            3,
            "the two nested selections each receive a private shared continuation"
        );
        let direct = out.iter().find(|b| b.name == "%direct").unwrap();
        assert!(
            direct.lines().last().unwrap().contains("%xa"),
            "inner true arm must enter the private downstream continuation"
        );
        let outer = out.iter().find(|b| b.name == "%outer").unwrap();
        assert_eq!(outer.lines().last(), Some(&"br label %tail".to_string()));
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the cloned graph remains structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_clones_shared_case_tails() {
        let blocks = vec![
            blk("entry", &["br i1 %co, label %sw, label %merge"]),
            blk(
                "sw",
                &[
                    "switch i32 %x, label %merge [ i32 4, label %four i32 3, label %three i32 2, label %two i32 1, label %one ]",
                ],
            ),
            blk("one", &["br label %tail2"]),
            blk("two", &["br label %tail1"]),
            blk("three", &["br label %tail1"]),
            blk("four", &["br label %merge"]),
            blk("tail1", &["br label %tail2"]),
            blk("tail2", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert!(
            find_switch_case_shared_continuations(&blocks)
                .contains(&("%one".to_string(), "%tail2".to_string())),
            "the first case sees its shared tail"
        );
        let out = privatize_switch_case_continuations(&blocks);
        let clones: Vec<&BodyBlock> = out.iter().filter(|b| b.name.starts_with("%xa")).collect();
        assert_eq!(clones.len(), 3, "each overlapping case tail is privatized");
        assert_eq!(out.iter().filter(|b| b.name == "%merge").count(), 1);
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the privatized switch graph remains structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_preserves_adjacent_case_chain() {
        let blocks = vec![
            blk("entry", &["br label %sw"]),
            blk(
                "sw",
                &["switch i32 %x, label %merge [ i32 3, label %a i32 2, label %b i32 1, label %c ]"],
            ),
            blk("a", &["br label %b"]),
            blk("b", &["br label %c"]),
            blk("c", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert!(find_switch_case_shared_continuations(&blocks).is_empty());
        let out = privatize_switch_case_continuations(&blocks);
        assert_eq!(out.len(), blocks.len());
        assert!(out
            .iter()
            .zip(&blocks)
            .all(|(actual, expected)| actual.name == expected.name
                && actual.lines() == expected.lines()));
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the legal compact switch remains directly structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_rewrites_conditional_adjacent_entry() {
        let blocks = vec![
            blk("entry", &["br label %sw"]),
            blk(
                "sw",
                &["switch i32 %x, label %merge [ i32 2, label %a i32 1, label %b ]"],
            ),
            blk("a", &["br i1 %take, label %b, label %tail"]),
            blk("tail", &["br label %merge"]),
            blk("b", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert_eq!(
            find_switch_case_shared_continuations(&blocks),
            vec![("%a".into(), "%b".into())]
        );
        let out = privatize_switch_case_continuations(&blocks);
        let a = out.iter().find(|block| block.name == "%a").unwrap();
        assert!(!block_successors(a).iter().any(|target| target == "%b"));
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the conditional side entry is privatized before planning"
        );
    }

    #[test]
    fn shared_clone_rejects_cross_loop_redirected_predecessor() {
        let blocks = vec![
            blk("entry", &["br label %outer"]),
            blk("outer", &["br i1 %direct, label %shared, label %head"]),
            blk(
                "head",
                &[
                    "%i = phi i32 [ 0, %outer ], [ %next, %body ]",
                    "br label %body",
                ],
            ),
            blk(
                "body",
                &[
                    "%edge = add i32 %i, 1",
                    "%next = add i32 %i, 1",
                    "br i1 %leave, label %shared, label %head",
                ],
            ),
            blk(
                "shared",
                &[
                    "%value = phi i32 [ 0, %outer ], [ %edge, %body ]",
                    "ret void",
                ],
            ),
        ];
        let forest = analyze(&blocks);
        let loop_headers = forest
            .loops
            .iter()
            .map(|natural_loop| natural_loop.header.as_str())
            .collect::<HashSet<_>>();
        let loop_latches = forest
            .loops
            .iter()
            .flat_map(|natural_loop| natural_loop.latches.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        let loop_exits = forest
            .loops
            .iter()
            .flat_map(|natural_loop| natural_loop.exits.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        assert!(!shared_clone_is_loop_local(
            &blocks,
            &forest,
            "%outer",
            "%shared",
            &loop_headers,
            &loop_latches,
            &loop_exits,
        ));
    }

    #[test]
    fn privatize_switch_case_continuations_splits_case_to_case_entry() {
        let blocks = vec![
            blk("entry", &["br label %sw"]),
            blk(
                "sw",
                &["switch i32 %x, label %default [ i32 0, label %a i32 1, label %b i32 2, label %c ]"],
            ),
            blk("a", &["br i1 %ca, label %default, label %a_tail"]),
            blk("a_tail", &["br label %default"]),
            blk("b", &["br label %default"]),
            blk("c", &["br label %merge"]),
            blk("default", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert!(
            find_switch_case_shared_continuations(&blocks)
                .contains(&("%a".to_string(), "%default".to_string())),
            "a case-to-case edge is recognized even with a switch-dominated merge"
        );
        let out = privatize_switch_case_continuations(&blocks);
        assert!(
            out.len() > blocks.len(),
            "case entry received a private copy"
        );
        assert_eq!(out.iter().filter(|b| b.name == "%default").count(), 1);
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the split case-entry graph remains structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_splits_subset_before_dominated_merge() {
        let blocks = vec![
            blk("entry", &["br label %sw"]),
            blk(
                "sw",
                &["switch i32 %x, label %default [ i32 0, label %a i32 1, label %b i32 2, label %c ]"],
            ),
            blk("a", &["br label %shared"]),
            blk("b", &["br label %shared"]),
            blk("c", &["br label %merge"]),
            blk("default", &["br label %merge"]),
            blk("shared", &["br label %merge"]),
            blk("merge", &["ret void"]),
        ];
        assert!(
            find_switch_case_shared_continuations(&blocks)
                .contains(&("%a".to_string(), "%shared".to_string())),
            "the intermediate subset reconvergence is not the switch merge"
        );
        let out = privatize_switch_case_continuations(&blocks);
        assert!(out.len() > blocks.len(), "the shared suffix is privatized");
        assert_eq!(out.iter().filter(|b| b.name == "%merge").count(), 1);
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the split subset remains structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_clones_loop_local_tail() {
        let blocks = vec![
            blk("entry", &["br label %loop"]),
            blk("loop", &["br i1 %run, label %sw, label %join"]),
            blk(
                "sw",
                &["switch i32 %x, label %join [ i32 0, label %a i32 1, label %b ]"],
            ),
            blk("a", &["br label %shared"]),
            blk("b", &["br label %shared"]),
            blk("shared", &["br label %join"]),
            blk("join", &["br label %latch"]),
            blk("latch", &["br i1 %again, label %loop, label %exit"]),
            blk("exit", &["ret void"]),
        ];
        assert!(
            find_switch_case_shared_continuations(&blocks)
                .contains(&("%a".to_string(), "%shared".to_string())),
            "a loop-local shared case suffix is eligible"
        );
        let out = privatize_switch_case_continuations(&blocks);
        assert!(out.len() > blocks.len(), "the shared suffix is privatized");
        assert_eq!(out.iter().filter(|b| b.name == "%join").count(), 1);
        assert_eq!(out.iter().filter(|b| b.name == "%latch").count(), 1);
        assert!(
            super::super::structured_emit::structured_plan(&out).is_some(),
            "the loop and privatized switch remain structurable"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_skips_loop_latch() {
        let blocks = vec![
            blk("entry", &["br label %loop"]),
            blk("loop", &["br i1 %again, label %sw, label %exit"]),
            blk(
                "sw",
                &["switch i32 %x, label %exit [ i32 0, label %a i32 1, label %b ]"],
            ),
            blk("a", &["br label %shared"]),
            blk("b", &["br label %shared"]),
            blk("shared", &["br label %loop"]),
            blk("exit", &["ret void"]),
        ];
        assert_eq!(
            privatize_switch_case_continuations(&blocks).len(),
            blocks.len(),
            "a shared loop latch is left to the loop-aware structurizer"
        );
    }

    #[test]
    fn privatize_switch_case_continuations_skips_nested_loop_exit() {
        let blocks = vec![
            blk("entry", &["br label %outer"]),
            blk("outer", &["br i1 %run, label %sw, label %exit"]),
            blk(
                "sw",
                &["switch i32 %x, label %join [ i32 0, label %a i32 1, label %b i32 2, label %inner ]"],
            ),
            blk("a", &["br label %shared"]),
            blk("b", &["br label %shared"]),
            blk(
                "inner",
                &["br i1 %inner_again, label %inner_body, label %shared"],
            ),
            blk("inner_body", &["br label %inner"]),
            blk("shared", &["br label %join"]),
            blk("join", &["br label %outer_latch"]),
            blk("outer_latch", &["br label %outer"]),
            blk("exit", &["ret void"]),
        ];
        assert!(
            !find_switch_case_shared_continuations(&blocks)
                .contains(&("%a".to_string(), "%shared".to_string())),
            "a nested loop exit is retained as that loop's structural merge"
        );
        assert_eq!(
            privatize_switch_case_continuations(&blocks).len(),
            blocks.len(),
            "the nested loop exit is not cloned as a case suffix"
        );
    }

    #[test]
    fn privatize_cross_arm_edge_clones_ancestor_sibling_escape() {
        let blocks = vec![
            blk("entry", &["br i1 %c0, label %A, label %B"]),
            blk("A", &["br label %M"]),
            blk("B", &["br i1 %c1, label %C, label %D"]),
            blk("C", &["br label %A"]),
            blk("D", &["br label %M"]),
            blk("M", &["ret void"]),
        ];
        assert!(
            find_cross_arm_edge(&blocks).is_some(),
            "the %C->%A sibling-arm escape is detected"
        );
        let out = privatize_cross_arm_edge(&blocks);
        assert!(out.len() > blocks.len(), "a private clone of %A was added");
        assert!(
            find_cross_arm_edge(&out).is_none(),
            "the cross-arm edge is gone after privatization"
        );
    }
}
