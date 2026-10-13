use super::*;

pub(in crate::native) fn privatize_dispatch_shared_exits(
    blocks: &mut Vec<BodyBlock>,
    dispatch: &str,
    counter: &mut usize,
) {
    let Some((arm0, arm1)) = blocks
        .iter()
        .find(|b| b.name == dispatch)
        .and_then(conditional_branch_targets)
    else {
        return;
    };
    for arm in [arm0, arm1] {
        if analyze(blocks).dominates(dispatch, &arm) {
            continue;
        }
        if let Some(next) =
            super::clone_crossarm::privatize_dominated_region(blocks, dispatch, &arm, counter)
        {
            *blocks = next;
        }
    }
}

pub(in crate::native) fn split_phi_overlap(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    exit: &str,
    counter: &mut usize,
) -> Option<String> {
    atomic_rewrite(blocks, |blocks| {
        let body: Vec<String> = forest.loop_for_header(header)?.body.clone();
        let preds: Vec<String> = body
            .iter()
            .filter(|name| {
                blocks
                    .iter()
                    .find(|b| &b.name == *name)
                    .map(|b| block_successors(b).iter().any(|s| s == exit))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if preds.is_empty() {
            return None;
        }
        let is_redirected = |pred: &str| preds.iter().any(|p| p == pred);

        let new_name = format!("{SPLIT_PREFIX}{counter}");
        *counter += 1;

        let exit_idx = blocks.iter().position(|b| b.name == exit)?;
        type TypedIncomings = Vec<(crate::native::ir::LlValue, String)>;
        let mut passthrough_merges: Vec<(String, crate::native::ir::LlType, TypedIncomings)> =
            Vec::new();
        let mut exit_rewrites: Vec<(String, TypedIncomings)> = Vec::new();
        if let Some(t) = &blocks[exit_idx].typed {
            for inst in &t.insts {
                let (Some(dst), Some((ty, inc))) =
                    (inst.result.clone(), inst.phi_incoming().clone())
                else {
                    continue;
                };
                let (typed_red, mut kept_plus): (Vec<_>, Vec<_>) =
                    inc.into_iter().partition(|(_, pred)| is_redirected(pred));
                if typed_red.is_empty() {
                    continue;
                }
                let merged = format!("{new_name}.phi{}", passthrough_merges.len());
                kept_plus.push((
                    crate::native::ir::LlValue::Local(merged.clone()),
                    new_name.clone(),
                ));
                passthrough_merges.push((merged, ty, typed_red));
                exit_rewrites.push((dst, kept_plus));
            }
        }
        if let Some(t) = blocks[exit_idx].typed_mut() {
            for (dst, kept_plus) in &exit_rewrites {
                t.set_phi_incomings(dst, kept_plus);
            }
        }

        for b in blocks.iter_mut() {
            if preds.iter().any(|p| p == &b.name) {
                if let Some(t) = b.typed_mut() {
                    t.redirect_successor(exit, &new_name);
                }
            }
        }

        let insert_at = blocks
            .iter()
            .position(|b| b.name == exit)
            .unwrap_or(blocks.len());
        let mut blk = crate::native::tir::lower_block_carrier(
            &new_name,
            &[format!("br label {exit}")],
            &crate::fxhash::HashMap::default(),
        )?;
        for (merged, ty, typed_red) in &passthrough_merges {
            blk.push_value_phi(merged, ty, typed_red);
        }
        blocks.insert(
            insert_at,
            BodyBlock {
                name: new_name.clone(),
                role: role_for_name(&new_name),
                typed: Some(blk.into()),
            },
        );

        Some(new_name)
    })
}

pub(in crate::native) fn block_has_phi(blocks: &[BodyBlock], name: &str) -> bool {
    blocks
        .iter()
        .find(|b| b.name == name)
        .map(|b| {
            b.typed
                .as_ref()
                .is_some_and(|t| t.insts.iter().any(|i| i.is_phi()))
        })
        .unwrap_or(false)
}
