use super::{blocks::synthetic_block, graph::Cfg, loopforest, BlockRole, BodyBlock};
use crate::fxhash::HashSet;
use crate::native::ir::LlType;
use crate::native::tir::{TirOpcode, TirTerminator};

/// Give a bottom-tested loop with a short-circuit exit one CFG merge.
/// The latch's pure integer operations may execute on the early-exit path,
/// but its condition is selected only when that path formerly ran the latch.
pub(in crate::native) fn converge_loop_exit(
    blocks: &mut Vec<BodyBlock>,
    parameters: &[(String, LlType)],
) -> bool {
    let Some(cfg) = Cfg::from_blocks(blocks) else {
        return false;
    };
    let forest = loopforest::analyze(blocks);
    let block_index = |name: &str| blocks.iter().position(|block| block.name == name);
    let candidate = forest.loops.iter().find_map(|natural_loop| {
        let [latch] = natural_loop.latches.as_slice() else {
            return None;
        };
        let [_, _] = natural_loop.exits.as_slice() else {
            return None;
        };
        let [early] = cfg.predecessors.get(latch)?.as_slice() else {
            return None;
        };
        let header = &natural_loop.header;
        if header == latch || header == early {
            return None;
        }
        let latch_index = block_index(latch)?;
        let early_index = block_index(early)?;
        let latch_block = blocks[latch_index].typed.as_deref()?;
        let early_block = blocks[early_index].typed.as_deref()?;
        let TirTerminator::BrCond {
            cond: latch_cond,
            t: latch_true,
            f: latch_false,
        } = &latch_block.terminator
        else {
            return None;
        };
        let TirTerminator::BrCond {
            cond: early_cond,
            t: early_true,
            f: early_false,
        } = &early_block.terminator
        else {
            return None;
        };
        let latch_exits_on_true = latch_false == header;
        let latch_exit = if latch_exits_on_true { latch_true } else { latch_false };
        let early_exits_on_true = early_false == latch;
        let early_exit = if early_exits_on_true { early_true } else { early_false };
        if (!latch_exits_on_true && latch_true != header)
            || (!early_exits_on_true && early_true != latch)
            || latch_exit == early_exit
            || !natural_loop.exits.contains(latch_exit)
            || !natural_loop.exits.contains(early_exit)
            || blocks[latch_index].role != BlockRole::Normal
            || blocks[early_index].role != BlockRole::Normal
        {
            return None;
        }
        let exits = natural_loop
            .body
            .iter()
            .flat_map(|name| cfg.successors.get(name).into_iter().flatten())
            .filter(|target| !natural_loop.body.contains(target))
            .count();
        if exits != 2 {
            return None;
        }
        // Reject loads, calls, PHIs and any other instruction whose behavior
        // or operands could require execution only on its original path.
        let safe_latch = latch_block.insts.iter().all(|inst| {
            matches!(inst.opcode, TirOpcode::Add | TirOpcode::Sub | TirOpcode::ICmp)
                && matches!(inst.result_ty, Some(LlType::Bool | LlType::Int(_)))
                && inst.operands.len() == 2
                && inst.operands.iter().all(|operand| {
                    operand.as_typed_value().is_some_and(|value| {
                        matches!(value.ty, LlType::Bool | LlType::Int(_))
                    })
                })
        });
        if !safe_latch {
            return None;
        }
        let latch_exit_index = block_index(latch_exit)?;
        let early_exit_index = block_index(early_exit)?;
        for (index, predecessor) in [(latch_exit_index, latch), (early_exit_index, early)] {
            let block = blocks[index].typed.as_deref()?;
            if block.insts.iter().filter(|inst| inst.is_phi()).any(|inst| {
                !inst.phi_incoming().as_ref().is_some_and(|(_, incoming)| {
                    incoming.iter().filter(|(_, parent)| parent == predecessor).count() == 1
                })
            }) {
                return None;
            }
        }
        Some((
            early_index,
            latch_index,
            early_exit_index,
            latch_exit_index,
            header.clone(),
            early_cond.clone(),
            latch_cond.clone(),
            early_exit.clone(),
            early_exits_on_true,
            latch_exits_on_true,
        ))
    });

    let Some((
        early_index,
        latch_index,
        early_exit_index,
        latch_exit_index,
        header,
        early_cond,
        latch_cond,
        early_exit,
        early_exits_on_true,
        latch_exits_on_true,
    )) = candidate
    else {
        return false;
    };
    let occupied = blocks
        .iter()
        .flat_map(|block| {
            std::iter::once(block.name.as_str()).chain(
                block
                    .typed
                    .iter()
                    .flat_map(|typed| &typed.insts)
                    .filter_map(|inst| inst.result.as_deref()),
            )
        })
        .chain(parameters.iter().map(|(name, _)| name.as_str()))
        .collect::<HashSet<_>>();
    let (merge, condition) = (0..)
        .map(|id| {
            (
                format!("%metal2vulkan.converge.exit.{id}"),
                format!("%metal2vulkan.converge.condition.{id}"),
            )
        })
        .find(|(merge, condition)| {
            !occupied.contains(merge.as_str()) && !occupied.contains(condition.as_str())
        })
        .expect("fresh loop exit identifiers");

    let force_exit = if latch_exits_on_true { "true" } else { "false" };
    let (condition_true, condition_false) = if early_exits_on_true {
        (force_exit, latch_cond.as_str())
    } else {
        (latch_cond.as_str(), force_exit)
    };
    let (latch_true, latch_false) = if latch_exits_on_true {
        (merge.as_str(), header.as_str())
    } else {
        (header.as_str(), merge.as_str())
    };
    let Some(new_latch) = crate::native::tir::lower_block_carrier_with_prefix(
        &blocks[latch_index].name,
        blocks[latch_index].typed.as_deref().expect("checked latch"),
        &[
            format!("{condition} = select i1 {early_cond}, i1 {condition_true}, i1 {condition_false}"),
            format!("br i1 {condition}, label {latch_true}, label {latch_false}"),
        ],
    ) else {
        return false;
    };
    let normal_exit = blocks[latch_exit_index].name.clone();
    let (merge_true, merge_false) = if early_exits_on_true {
        (early_exit.as_str(), normal_exit.as_str())
    } else {
        (normal_exit.as_str(), early_exit.as_str())
    };
    let merge_block = synthetic_block(
        merge.clone(),
        vec![format!("br i1 {early_cond}, label {merge_true}, label {merge_false}")],
        BlockRole::Normal,
    );

    if merge_block.typed.is_none() {
        return false;
    }

    let latch = blocks[latch_index].name.clone();
    let early = blocks[early_index].name.clone();
    blocks[early_index]
        .typed_mut()
        .expect("checked early block")
        .set_unconditional_branch(&latch);
    blocks[latch_index].typed = Some(new_latch.into());
    blocks[early_exit_index]
        .typed_mut()
        .expect("checked early exit")
        .rewrite_phi_predecessor(&early, &merge);
    blocks[latch_exit_index]
        .typed_mut()
        .expect("checked latch exit")
        .rewrite_phi_predecessor(&latch, &merge);
    blocks.insert(latch_index + 1, merge_block);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::cfg::split_body_blocks;
    use crate::native::ir::LlValue;

    const LOOP: &str = "
entry:
    br label %header
header:
    %i = phi i32 [0, %entry], [%inc, %latch]
    %found = icmp eq i32 %i, 3
    br label %early
early:
    br i1 %found, label %hit, label %latch
latch:
    %inc = add nuw nsw i32 %i, 1
    %limit = icmp eq i32 %inc, 10
    br i1 %limit, label %miss, label %header
hit:
    %hit_value = phi i32 [%i, %early]
    ret i32 %hit_value
miss:
    %miss_value = phi i32 [%inc, %latch]
    ret i32 %miss_value
";

    fn parse(source: &str) -> Vec<BodyBlock> {
        split_body_blocks(&source.lines().collect::<Vec<_>>(), "%entry".into(), &Default::default())
    }

    fn block<'a>(blocks: &'a [BodyBlock], name: &str) -> &'a crate::native::tir::TirBlock {
        blocks.iter().find(|block| block.name == name).unwrap().typed.as_deref().unwrap()
    }

    fn assert_select(
        latch: &crate::native::tir::TirBlock,
        result: &str,
        true_value: LlValue,
        false_value: LlValue,
    ) {
        let select = latch.insts.last().unwrap();
        assert_eq!(select.opcode, TirOpcode::Select);
        assert_eq!(select.result.as_deref(), Some(result));
        assert_eq!(select.operands[0].as_typed_value().unwrap().value, LlValue::Local("%found".into()));
        let arms = select.select_arms().as_ref().unwrap();
        assert_eq!(arms.0.value, true_value);
        assert_eq!(arms.1.value, false_value);
    }

    #[test]
    fn preserves_distinct_exits_and_repairs_both_exit_phis() {
        let mut blocks = parse(LOOP);
        assert!(converge_loop_exit(&mut blocks, &[]));
        let merge = "%metal2vulkan.converge.exit.0";
        let condition = "%metal2vulkan.converge.condition.0";
        assert_eq!(block(&blocks, "%early").terminator, TirTerminator::Br("%latch".into()));
        let latch = block(&blocks, "%latch");
        assert_eq!(
            latch.terminator,
            TirTerminator::BrCond { cond: condition.into(), t: merge.into(), f: "%header".into() }
        );
        assert_eq!(
            block(&blocks, merge).terminator,
            TirTerminator::BrCond { cond: "%found".into(), t: "%hit".into(), f: "%miss".into() }
        );
        assert_select(latch, condition, LlValue::Bool(true), LlValue::Local("%limit".into()));
        for (exit, value) in [("%hit", "%i"), ("%miss", "%inc")] {
            let (_, incoming) = block(&blocks, exit).insts[0].phi_incoming().as_ref().unwrap();
            assert_eq!(incoming, &[(LlValue::Local(value.into()), merge.into())]);
        }
        let (_, header_incoming) = block(&blocks, "%header").insts[0].phi_incoming().as_ref().unwrap();
        assert_eq!(header_incoming[1], (LlValue::Local("%inc".into()), "%latch".into()));
    }

    #[test]
    fn supports_either_branch_polarity_and_avoids_reserved_names() {
        let source = LOOP
            .replace("label %hit, label %latch", "label %latch, label %hit")
            .replace("label %miss, label %header", "label %header, label %miss");
        let mut blocks = parse(&source);
        assert!(converge_loop_exit(
            &mut blocks,
            &[("%metal2vulkan.converge.exit.0".into(), LlType::Bool)]
        ));
        let merge = "%metal2vulkan.converge.exit.1";
        let latch = block(&blocks, "%latch");
        assert_eq!(
            latch.terminator,
            TirTerminator::BrCond {
                cond: "%metal2vulkan.converge.condition.1".into(),
                t: "%header".into(),
                f: merge.into(),
            }
        );
        assert_eq!(
            block(&blocks, merge).terminator,
            TirTerminator::BrCond { cond: "%found".into(), t: "%miss".into(), f: "%hit".into() }
        );
        assert_select(
            latch,
            "%metal2vulkan.converge.condition.1",
            LlValue::Local("%limit".into()),
            LlValue::Bool(false),
        );
    }

    #[test]
    fn rejects_effectful_latches_and_extra_incoming_edges() {
        let effects = LOOP.replace(
            "    %inc =",
            "    store i32 %i, ptr addrspace(1) %output\n    %inc =",
        );
        let extra_pred = LOOP
            .replace("br label %early", "br i1 %found, label %early, label %latch");
        let shared_exit = LOOP.replace("label %miss, label %header", "label %hit, label %header");
        for source in [effects, extra_pred, shared_exit] {
            let mut blocks = parse(&source);
            let before = blocks.iter().map(BodyBlock::lines).collect::<Vec<_>>();
            assert!(!converge_loop_exit(&mut blocks, &[]));
            assert_eq!(before, blocks.iter().map(BodyBlock::lines).collect::<Vec<_>>());
        }
    }
}
