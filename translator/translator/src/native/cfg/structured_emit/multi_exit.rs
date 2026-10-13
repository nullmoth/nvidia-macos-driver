use super::*;

pub(in crate::native) fn split_multi_exit_critical_edges(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    exits: &[String],
    counter: &mut usize,
) -> Option<Vec<(String, usize)>> {
    let body = forest
        .loop_for_header(header)?
        .body
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    split_two_target_critical_edges(blocks, &body, exits, counter)
}

pub(in crate::native) fn split_two_target_critical_edges(
    blocks: &mut Vec<BodyBlock>,
    region: &HashSet<String>,
    exits: &[String],
    counter: &mut usize,
) -> Option<Vec<(String, usize)>> {
    atomic_rewrite(blocks, |blocks| {
        if exits.len() != 2 {
            return None;
        }
        let mut splits: Vec<(String, Vec<usize>)> = Vec::new();

        for block in blocks.iter().filter(|block| region.contains(&block.name)) {
            let successors = block_successors(block);
            let hit: Vec<usize> = exits
                .iter()
                .enumerate()
                .filter_map(|(index, exit)| {
                    successors
                        .iter()
                        .any(|target| target == exit)
                        .then_some(index)
                })
                .collect();
            if hit.len() < 2 {
                continue;
            }
            splits.push((block.name.clone(), hit));
        }
        if splits.is_empty() {
            return Some(Vec::new());
        }

        let mut edge_blocks = Vec::new();
        let mut edge_preds = Vec::new();
        for (source, targets) in splits {
            let mut edges = Vec::with_capacity(targets.len());
            for index in targets {
                let edge = format!("{EXIT_EDGE_PREFIX}{counter}");
                *counter += 1;
                edges.push((index, edge));
            }

            let source_block = blocks.iter_mut().find(|b| b.name == source)?;
            if let Some(t) = source_block.typed_mut() {
                for (index, edge) in &edges {
                    t.redirect_successor(&exits[*index], edge);
                }
            }

            for (index, edge) in &edges {
                let exit = blocks.iter_mut().find(|b| b.name == exits[*index])?;
                if let Some(t) = exit.typed_mut() {
                    t.rewrite_phi_predecessor(&source, edge);
                }
                let edge_lines = vec![format!("br label {}", exits[*index])];
                edge_blocks.push(BodyBlock {
                    typed: crate::native::tir::lower_block_carrier(
                        edge,
                        &edge_lines,
                        &crate::fxhash::HashMap::default(),
                    )
                    .map(Into::into),
                    name: edge.clone(),
                    role: role_for_name(edge),
                });
                edge_preds.push((edge.clone(), *index));
            }
        }
        blocks.extend(edge_blocks);
        Some(edge_preds)
    })
}

pub(in crate::native) fn synth_multi_exit_merge(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    exits: &[String],
    counter: &mut usize,
) -> Option<String> {
    atomic_rewrite(blocks, |blocks| {
        if exits.len() != 2 {
            return None;
        }
        let mut exits = exits.to_vec();
        exits.sort();
        let exits = &exits[..];
        let critical_edge_preds =
            split_multi_exit_critical_edges(blocks, forest, header, exits, counter)?;
        let body: HashSet<&str> = forest
            .loop_for_header(header)?
            .body
            .iter()
            .map(String::as_str)
            .collect();

        let mut preds: Vec<(String, usize)> = Vec::new();
        let critical_edge_set: HashSet<&str> = critical_edge_preds
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        for b in blocks.iter() {
            if !body.contains(b.name.as_str()) && !critical_edge_set.contains(b.name.as_str()) {
                continue;
            }
            let mut hit: Option<usize> = None;
            for succ in block_successors(b) {
                if let Some(idx) = exits.iter().position(|e| e == &succ) {
                    match hit {
                        Some(prev) if prev != idx => return None,
                        _ => hit = Some(idx),
                    }
                }
            }
            if let Some(idx) = hit {
                preds.push((b.name.clone(), idx));
            }
        }
        if preds.is_empty() {
            return None;
        }

        synth_two_target_dispatch(blocks, exits, &preds, counter)
    })
}

pub(in crate::native) fn synth_two_target_dispatch(
    blocks: &mut Vec<BodyBlock>,
    exits: &[String],
    preds: &[(String, usize)],
    counter: &mut usize,
) -> Option<String> {
    atomic_rewrite(blocks, |blocks| {
        if exits.len() != 2
            || preds.is_empty()
            || preds.iter().any(|(_, exit)| *exit >= exits.len())
        {
            return None;
        }
        let merge = format!("{SPLIT_PREFIX}{counter}");
        *counter += 1;
        let sel = format!("{EXIT_SEL_PREFIX}{counter}");
        *counter += 1;

        let forest = analyze(blocks);
        let mut definitions = HashMap::default();
        for block in blocks.iter() {
            let carrier = block.typed.as_ref()?;
            for instruction in &carrier.insts {
                if let (Some(result), Some(ty)) = (&instruction.result, &instruction.result_ty) {
                    definitions.insert(result.clone(), (block.name.clone(), ty.clone()));
                }
            }
        }
        let mut dispatch_live_phis: Vec<(
            String,
            crate::native::ir::LlType,
            Vec<(crate::native::ir::LlValue, String)>,
        )> = Vec::new();
        let mut region_substitutions = Vec::new();
        for (exit_index, exit) in exits.iter().enumerate() {
            let region = blocks
                .iter()
                .filter(|block| forest.dominates(exit, &block.name))
                .map(|block| block.name.clone())
                .collect::<HashSet<_>>();
            let mut used = Vec::new();
            let mut used_set = HashSet::default();
            for block in blocks.iter().filter(|block| region.contains(&block.name)) {
                let carrier = block.typed.as_ref()?;
                for instruction in carrier
                    .insts
                    .iter()
                    .filter(|instruction| !instruction.is_phi())
                {
                    instruction.visit_uses(|name| {
                        if used_set.insert(name.to_string()) {
                            used.push(name.to_string());
                        }
                    });
                }
                let terminator_use = match &carrier.terminator {
                    crate::native::tir::TirTerminator::Br(_)
                    | crate::native::tir::TirTerminator::Ret(None)
                    | crate::native::tir::TirTerminator::Unreachable => None,
                    crate::native::tir::TirTerminator::BrCond { cond, .. }
                    | crate::native::tir::TirTerminator::Switch { selector: cond, .. }
                    | crate::native::tir::TirTerminator::Ret(Some(cond)) => Some(cond),
                };
                if let Some(name) = terminator_use {
                    if used_set.insert(name.clone()) {
                        used.push(name.clone());
                    }
                }
            }

            let mut substitutions = HashMap::default();
            for value in used {
                let Some((definition_block, ty)) = definitions.get(&value) else {
                    continue;
                };
                if region.contains(definition_block) {
                    continue;
                }
                if preds
                    .iter()
                    .all(|(predecessor, _)| forest.dominates(definition_block, predecessor))
                {
                    continue;
                }
                if !preds
                    .iter()
                    .filter(|(_, target)| *target == exit_index)
                    .all(|(predecessor, _)| forest.dominates(definition_block, predecessor))
                    || !dispatch_live_value_type(ty)
                {
                    return None;
                }
                let carried = format!("%metal2vulkan.exitlive.{counter}");
                *counter += 1;
                let incoming = preds
                    .iter()
                    .map(|(predecessor, target)| {
                        let incoming = if *target == exit_index {
                            crate::native::ir::LlValue::Local(value.clone())
                        } else {
                            crate::native::ir::LlValue::Undef
                        };
                        (incoming, predecessor.clone())
                    })
                    .collect();
                substitutions.insert(
                    value,
                    crate::native::ir::TypedValue {
                        ty: ty.clone(),
                        value: crate::native::ir::LlValue::Local(carried.clone()),
                    },
                );
                dispatch_live_phis.push((carried, ty.clone(), incoming));
            }
            region_substitutions.push((region, substitutions));
        }
        for (region, substitutions) in region_substitutions {
            for block in blocks
                .iter_mut()
                .filter(|block| region.contains(&block.name))
            {
                block
                    .typed_mut()
                    .expect("dispatch live-in analysis checked every carrier")
                    .substitute_non_phi_values(&substitutions);
            }
        }

        for b in blocks.iter_mut() {
            if let Some((_, idx)) = preds.iter().find(|(n, _)| n == &b.name) {
                if let Some(t) = b.typed_mut() {
                    t.redirect_successor(&exits[*idx], &merge);
                }
            }
        }

        let dispatch_targets = [exits[0].clone(), exits[1].clone()];
        let exit_edge_pred = [merge.clone(), merge.clone()];

        type TypedIncomings = Vec<(crate::native::ir::LlValue, String)>;
        let mut typed_value_phis: Vec<(String, crate::native::ir::LlType, TypedIncomings)> =
            Vec::new();
        for (ei, e) in exits.iter().enumerate() {
            let Some(exit_idx) = blocks.iter().position(|b| &b.name == e) else {
                continue;
            };
            let edge_pred = exit_edge_pred[ei].clone();
            let mut exit_typed_merges: Vec<(String, TypedIncomings)> = Vec::new();
            if let Some(t) = &blocks[exit_idx].typed {
                for inst in &t.insts {
                    let (Some(dst), Some((cty, inc))) =
                        (inst.result.clone(), inst.phi_incoming().clone())
                    else {
                        continue;
                    };
                    let is_this_exit_pred =
                        |p: &str| preds.iter().any(|(name, idx)| name == p && *idx == ei);
                    let (typed_red, mut kept_plus): (Vec<_>, Vec<_>) =
                        inc.into_iter().partition(|(_, p)| is_this_exit_pred(p));
                    if typed_red.is_empty() {
                        continue;
                    }
                    let merged = format!("%metal2vulkan.exitphi.{counter}");
                    *counter += 1;
                    let merged_typed: TypedIncomings = preds
                        .iter()
                        .map(|(pname, pidx)| {
                            let v = if *pidx == ei {
                                typed_red
                                    .iter()
                                    .find(|(_, p)| p == pname)
                                    .map(|(v, _)| v.clone())
                                    .unwrap_or(crate::native::ir::LlValue::Undef)
                            } else {
                                crate::native::ir::LlValue::Undef
                            };
                            (v, pname.clone())
                        })
                        .collect();
                    kept_plus.push((
                        crate::native::ir::LlValue::Local(merged.clone()),
                        edge_pred.clone(),
                    ));
                    exit_typed_merges.push((dst, kept_plus));
                    typed_value_phis.push((merged, cty, merged_typed));
                }
            }
            if let Some(t) = blocks[exit_idx].typed_mut() {
                for (dst, kept_plus) in &exit_typed_merges {
                    t.set_phi_incomings(dst, kept_plus);
                }
            }
        }

        let selector_typed: TypedIncomings = preds
            .iter()
            .map(|(name, idx)| (crate::native::ir::LlValue::Bool(*idx == 0), name.clone()))
            .collect();
        let branch_line = format!(
            "br i1 {sel}, label {}, label {}",
            dispatch_targets[0], dispatch_targets[1]
        );
        let mut blk = crate::native::tir::lower_block_carrier(
            &merge,
            &[branch_line],
            &crate::fxhash::HashMap::default(),
        )?;
        for (mname, ty, merged_typed) in &typed_value_phis {
            blk.push_value_phi(mname, ty, merged_typed);
        }
        for (name, ty, incoming) in &dispatch_live_phis {
            blk.push_value_phi(name, ty, incoming);
        }
        blk.push_value_phi(&sel, &crate::native::ir::LlType::Int(1), &selector_typed);
        let merge_block = BodyBlock {
            name: merge.clone(),
            role: role_for_name(&merge),
            typed: Some(blk.into()),
        };
        let insert_at = blocks
            .iter()
            .position(|b| b.name == exits[0] || b.name == exits[1])
            .unwrap_or(blocks.len());
        blocks.insert(insert_at, merge_block);

        Some(merge)
    })
}

fn dispatch_live_value_type(ty: &crate::native::ir::LlType) -> bool {
    use crate::native::ir::LlType;
    match ty {
        LlType::Void | LlType::Ptr(_) | LlType::Named(_) => false,
        LlType::Vector(element, _) | LlType::Array(element, _) => dispatch_live_value_type(element),
        LlType::Struct(fields) => fields.iter().all(dispatch_live_value_type),
        LlType::Bool | LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_) => true,
    }
}

pub(in crate::native) fn synth_multi_latch_continue(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    latches: &[String],
    counter: &mut usize,
) -> Option<String> {
    atomic_rewrite(blocks, |blocks| {
        if latches.len() < 2 {
            return None;
        }
        let mut latches = latches.to_vec();
        latches.sort();
        latches.dedup();
        let latch_set: HashSet<&str> = latches.iter().map(String::as_str).collect();

        for l in &latches {
            let b = blocks.iter().find(|b| &b.name == l)?;
            if !block_successors(b).iter().any(|s| s == header) {
                return None;
            }
        }

        let hidx = blocks.iter().position(|b| b.name == header)?;
        let new_latch = format!("{SPLIT_PREFIX}{counter}");
        *counter += 1;

        type TypedIncomings = Vec<(crate::native::ir::LlValue, String)>;
        let mut typed_value_phis: Vec<(String, crate::native::ir::LlType, TypedIncomings)> =
            Vec::new();
        let mut header_typed_merges: Vec<(String, TypedIncomings)> = Vec::new();
        if let Some(t) = &blocks[hidx].typed {
            for inst in &t.insts {
                let (Some(dst), Some((cty, inc))) =
                    (inst.result.clone(), inst.phi_incoming().clone())
                else {
                    continue;
                };
                let (typed_red, mut kept_plus): (Vec<_>, Vec<_>) = inc
                    .into_iter()
                    .partition(|(_, p)| latch_set.contains(p.as_str()));
                if typed_red.is_empty() {
                    continue;
                }
                let merged = format!("%metal2vulkan.latchphi.{counter}");
                *counter += 1;
                let merged_typed: TypedIncomings = latches
                    .iter()
                    .map(|lname| {
                        let v = typed_red
                            .iter()
                            .find(|(_, p)| p == lname)
                            .map(|(v, _)| v.clone())
                            .unwrap_or(crate::native::ir::LlValue::Undef);
                        (v, lname.clone())
                    })
                    .collect();
                kept_plus.push((
                    crate::native::ir::LlValue::Local(merged.clone()),
                    new_latch.clone(),
                ));
                header_typed_merges.push((dst, kept_plus));
                typed_value_phis.push((merged, cty, merged_typed));
            }
        }
        if let Some(t) = blocks[hidx].typed_mut() {
            for (dst, kept_plus) in &header_typed_merges {
                t.set_phi_incomings(dst, kept_plus);
            }
        }

        for l in &latches {
            if let Some(b) = blocks.iter_mut().find(|b| &b.name == l) {
                if let Some(t) = b.typed_mut() {
                    t.redirect_successor(header, &new_latch);
                }
            }
        }

        let insert_at = hidx.max(1);
        let latch_block = {
            let mut blk = crate::native::tir::lower_block_carrier(
                &new_latch,
                &[format!("br label {header}")],
                &crate::fxhash::HashMap::default(),
            )?;
            for (merged, ty, merged_typed) in &typed_value_phis {
                blk.push_value_phi(merged, ty, merged_typed);
            }
            BodyBlock {
                name: new_latch.clone(),
                role: role_for_name(&new_latch),
                typed: Some(blk.into()),
            }
        };
        blocks.insert(insert_at, latch_block);

        Some(new_latch)
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;

    fn bb_uncarried(name: &str) -> BodyBlock {
        let block = bb(name, &[]);
        assert!(block.typed.is_none(), "fixture must have no typed carrier");
        block
    }

    #[test]
    fn declined_critical_edge_split_leaves_no_dangling_successor() {
        let mut blocks = vec![
            bb("%entry", &["br i1 %c, label %x, label %y"]),
            bb("%x", &["ret void"]),
        ];
        let before = format!("{blocks:?}");
        let region = crate::fxhash::set_from(["%entry".to_string()]);
        let exits = vec!["%x".to_string(), "%y".to_string()];
        let mut counter = 0;

        assert!(
            split_two_target_critical_edges(&mut blocks, &region, &exits, &mut counter).is_none(),
            "a dispatch target with no block cannot be split"
        );
        let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        let entry = blocks.iter().find(|b| b.name == "%entry").unwrap();
        for successor in block_successors(entry) {
            assert!(
                names.contains(successor.as_str()) || successor == "%y",
                "declined split left %entry branching to {successor}, which is not a block"
            );
        }
        assert_eq!(
            format!("{blocks:?}"),
            before,
            "a declined split must not edit the CFG"
        );
    }

    #[test]
    fn declined_two_exit_funnel_leaves_the_split_edges_out() {
        let mut blocks = vec![
            bb("%entry", &["br label %h"]),
            bb("%h", &["br i1 %c0, label %b, label %latch"]),
            bb(
                "%b",
                &["switch i32 %s, label %latch [ i32 0, label %x i32 1, label %y ]"],
            ),
            bb("%latch", &["br i1 %c1, label %h, label %x"]),
            bb("%x", &["ret void"]),
            bb("%y", &["ret void"]),
            bb_uncarried("%uncarried"),
        ];
        let forest = analyze(&blocks);
        let exits = forest
            .loop_for_header("%h")
            .map(|l| {
                let mut exits = l.exits.clone();
                exits.sort();
                exits
            })
            .expect("the fixture's natural loop");
        assert_eq!(exits, vec!["%x".to_string(), "%y".to_string()]);
        let before = format!("{blocks:?}");
        let mut counter = 0;

        assert!(
            synth_multi_exit_merge(&mut blocks, &forest, "%h", &exits, &mut counter).is_none(),
            "the dispatch proof declines on the uncarried block"
        );
        assert_eq!(
            format!("{blocks:?}"),
            before,
            "a declined funnel must not leave its critical-edge splits behind"
        );
    }
}
