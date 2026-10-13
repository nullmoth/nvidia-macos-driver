use super::*;

const LOOP_HEADER_SELECTION_PREFIX: &str = "%metal2vulkan.lhsel.";

pub(in crate::native) fn refine_loop_exit_selection_merges(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    selection_merges: &mut HashMap<String, String>,
) {
    if blocks.len() > LOOP_EXIT_SELECTION_MAX_BLOCKS {
        return;
    }
    let headers: Vec<(String, String)> = selection_merges
        .iter()
        .map(|(header, merge)| (header.clone(), merge.clone()))
        .collect();
    for (header, natural) in headers {
        let mut enclosing: Vec<_> = forest
            .loops
            .iter()
            .filter(|loop_info| loop_info.header != header)
            .filter(|loop_info| loop_info.body.iter().any(|node| node == &header))
            .filter_map(|loop_info| {
                loop_merges
                    .get(&loop_info.header)
                    .filter(|info| info.merge == natural)
                    .map(|info| (loop_info, info))
            })
            .collect();
        enclosing.sort_by_key(|(loop_info, _)| loop_info.body.len());
        for (loop_info, info) in enclosing {
            if let Some(convergence) = loop_exit_selection_convergence(
                blocks,
                forest,
                loop_merges,
                loop_info,
                info,
                &header,
            ) {
                selection_merges.insert(header.clone(), convergence);
                break;
            }
        }
    }
}

pub(in crate::native) fn refine_loop_entry_terminal_selection_merges(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    selection_merges: &mut HashMap<String, String>,
) {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    for loop_info in &forest.loops {
        let Some(info) = loop_merges.get(&loop_info.header) else {
            continue;
        };
        let Some(loop_header) = by_name.get(loop_info.header.as_str()) else {
            continue;
        };
        let successors = block_successors(loop_header);
        let [selection_header] = successors.as_slice() else {
            continue;
        };
        if selection_merges.contains_key(selection_header) {
            continue;
        }
        let Some(selection_block) = by_name.get(selection_header.as_str()) else {
            continue;
        };
        let Some((left, right)) = conditional_branch_targets(selection_block) else {
            continue;
        };
        let loop_body = loop_info
            .body
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        for (candidate, other) in [(&left, &right), (&right, &left)] {
            if !loop_body.contains(candidate.as_str())
                || candidate == &loop_info.header
                || candidate == &info.merge
                || candidate == &info.continue_target
            {
                continue;
            }
            if loop_entry_arm_reaches_merge_or_structured_exit(
                blocks, &by_name, &loop_body, loop_info, info, other, candidate,
            ) {
                selection_merges.insert(selection_header.clone(), candidate.clone());
                if crate::env_vars::spi_why() {
                    eprintln!(
                        "[spi-why]   loop-entry-terminal header={} owner={} merge={}",
                        selection_header, loop_info.header, candidate,
                    );
                }
                break;
            }
        }
        if !selection_merges.contains_key(selection_header) {
            if let Some(candidate) = loop_exit_selection_convergence(
                blocks,
                forest,
                loop_merges,
                loop_info,
                info,
                selection_header,
            ) {
                selection_merges.insert(selection_header.clone(), candidate.clone());
                if crate::env_vars::spi_why() {
                    eprintln!(
                        "[spi-why]   loop-entry-terminal header={} owner={} merge={}",
                        selection_header, loop_info.header, candidate,
                    );
                }
            }
        }
    }
}

pub(in crate::native) fn refine_nested_terminal_selection_merges(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    forced_terminal_merges: &HashMap<String, String>,
    selection_merges: &mut HashMap<String, String>,
) {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut headers = blocks
        .iter()
        .filter(|block| conditional_branch_targets(block).is_some() || is_switch_block(block))
        .filter(|block| !selection_merges.contains_key(&block.name))
        .filter(|block| !forced_terminal_merges.contains_key(&block.name))
        .map(|block| block.name.clone())
        .collect::<Vec<_>>();
    headers.sort();
    for header in headers {
        let direct_enclosing_loop_exit = |target: &str| {
            forest.loops.iter().any(|candidate_loop| {
                candidate_loop.header != header
                    && candidate_loop.body.iter().any(|node| node == &header)
                    && (candidate_loop.header == target
                        || loop_merges.get(&candidate_loop.header).is_some_and(|role| {
                            role.merge == target || role.continue_target == target
                        }))
            })
        };
        let mut enclosing = forest
            .loops
            .iter()
            .filter(|loop_info| loop_info.header != header)
            .filter(|loop_info| loop_info.body.iter().any(|node| node == &header))
            .filter_map(|loop_info| {
                loop_merges
                    .get(&loop_info.header)
                    .map(|info| (loop_info, info))
            })
            .collect::<Vec<_>>();
        enclosing.sort_by_key(|(loop_info, _)| loop_info.body.len());
        if enclosing.is_empty() {
            if let Some(candidate) = terminal_exit_convergence(blocks, forest, &header) {
                if !terminal_arm_enters_loop_with_shared_exit(blocks, forest, &header, &candidate) {
                    selection_merges.insert(header.clone(), candidate.clone());
                    if crate::env_vars::spi_why() {
                        eprintln!(
                            "[spi-why]   terminal-convergence header={} merge={}",
                            header, candidate,
                        );
                    }
                    continue;
                }
            }
        }
        for (loop_info, info) in enclosing {
            let direct = by_name
                .get(header.as_str())
                .and_then(|block| conditional_branch_targets(block))
                .and_then(|(left, right)| {
                    let loop_body = loop_info
                        .body
                        .iter()
                        .map(String::as_str)
                        .collect::<HashSet<_>>();
                    [(&left, &right), (&right, &left)]
                        .into_iter()
                        .find_map(|(candidate, other)| {
                            (loop_body.contains(candidate.as_str())
                                && candidate != &loop_info.header
                                && candidate != &info.merge
                                && candidate != &info.continue_target
                                && (direct_enclosing_loop_exit(other)
                                    || loop_entry_arm_reaches_merge_or_structured_exit(
                                        blocks, &by_name, &loop_body, loop_info, info, other,
                                        candidate,
                                    )))
                            .then(|| candidate.clone())
                        })
                });
            let candidate = direct.or_else(|| {
                loop_exit_selection_convergence(
                    blocks,
                    forest,
                    loop_merges,
                    loop_info,
                    info,
                    &header,
                )
            });
            if let Some(candidate) = candidate {
                selection_merges.insert(header.clone(), candidate.clone());
                if crate::env_vars::spi_why() {
                    eprintln!(
                        "[spi-why]   nested-loop-terminal header={} owner={} merge={}",
                        header, loop_info.header, candidate,
                    );
                }
                break;
            }
        }
        if crate::env_vars::spi_why() && !selection_merges.contains_key(&header) {
            let arms = by_name
                .get(header.as_str())
                .map(|block| block_successors(block))
                .unwrap_or_default();
            let roles = forest
                .loops
                .iter()
                .filter(|loop_info| loop_info.body.iter().any(|node| node == &header))
                .map(|loop_info| {
                    let role = loop_merges.get(&loop_info.header);
                    (
                        loop_info.header.as_str(),
                        role.map(|role| role.merge.as_str()),
                        role.map(|role| role.continue_target.as_str()),
                    )
                })
                .collect::<Vec<_>>();
            eprintln!(
                "[spi-why]   nested-loop-terminal-decline header={} arms={:?} roles={:?}",
                header, arms, roles,
            );
        }
    }
}

fn terminal_arm_enters_loop_with_shared_exit(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
    live_convergence: &str,
) -> bool {
    let Some((left, right)) = blocks
        .iter()
        .find(|block| block.name == header)
        .and_then(conditional_branch_targets)
    else {
        return false;
    };
    [left, right].into_iter().any(|arm| {
        terminal_exit_arm(blocks, forest, header, &arm).is_some_and(|terminal| {
            forest.loops.iter().any(|loop_info| {
                loop_info
                    .exits
                    .iter()
                    .any(|exit| exit == &terminal.return_block)
                    && block_reaches(blocks, live_convergence, &loop_info.header)
            })
        })
    })
}

fn block_reaches(blocks: &[BodyBlock], start: &str, target: &str) -> bool {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::default();
    let mut pending = vec![start.to_string()];
    while let Some(current) = pending.pop() {
        if current == target {
            return true;
        }
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(block) = by_name.get(current.as_str()) {
            pending.extend(block_successors(block));
        }
    }
    false
}

#[cfg(test)]
pub(in crate::native) fn complete_construct_tree_terminal_convergences(
    blocks: &[BodyBlock],
    loop_merges: &HashMap<String, LoopMergeInfo>,
    forced_terminal_merges: &HashMap<String, String>,
    source_selection_merges: &HashMap<String, String>,
    header_merges: &mut HashMap<String, String>,
) -> bool {
    let forest = analyze(blocks);
    let mut proposed = header_merges.clone();
    refine_nested_terminal_selection_merges(
        blocks,
        &forest,
        loop_merges,
        forced_terminal_merges,
        &mut proposed,
    );

    let loop_roles = loop_role_targets_with_passthroughs(blocks, loop_merges);
    let mut claimed = header_merges.values().cloned().collect::<HashSet<_>>();
    let mut additions = proposed
        .into_iter()
        .filter(|(header, _)| {
            !header_merges.contains_key(header) && !source_selection_merges.contains_key(header)
        })
        .collect::<Vec<_>>();
    additions.sort();

    let mut changed = false;
    for (header, merge) in additions {
        if loop_roles.contains(&merge)
            || claimed.contains(&merge)
            || !forest.dominates(&header, &merge)
        {
            if crate::env_vars::spi_why() {
                eprintln!(
                    "[spi-why]   late-terminal-convergence-decline header={} merge={} loop-role={} claimed={} dominated={}",
                    header,
                    merge,
                    loop_roles.contains(&merge),
                    claimed.contains(&merge),
                    forest.dominates(&header, &merge),
                );
            }
            continue;
        }
        if crate::env_vars::spi_why() {
            eprintln!(
                "[spi-why]   late-terminal-convergence header={} merge={}",
                header, merge,
            );
        }
        claimed.insert(merge.clone());
        header_merges.insert(header, merge);
        changed = true;
    }
    changed
}

#[allow(clippy::too_many_arguments)]
fn loop_entry_arm_reaches_merge_or_structured_exit(
    blocks: &[BodyBlock],
    by_name: &HashMap<&str, &BodyBlock>,
    loop_body: &HashSet<&str>,
    loop_info: &super::loopforest::NaturalLoop,
    info: &LoopMergeInfo,
    start: &str,
    candidate: &str,
) -> bool {
    let mut reached = false;
    let direct_structured_exit = start == loop_info.header
        || start == info.merge
        || start == info.continue_target
        || block_ends_in_void_return(blocks, start)
        || block_ends_in_unreachable(blocks, start);
    let mut seen = HashSet::default();
    let mut stack = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if node == candidate {
            reached = true;
            continue;
        }
        if node == loop_info.header || node == info.merge || node == info.continue_target {
            continue;
        }
        if block_ends_in_void_return(blocks, &node) || block_ends_in_unreachable(blocks, &node) {
            continue;
        }
        if !loop_body.contains(node.as_str()) {
            return false;
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        let Some(block) = by_name.get(node.as_str()) else {
            return false;
        };
        let successors = block_successors(block);
        if successors.is_empty() {
            return false;
        }
        stack.extend(successors);
    }
    reached || direct_structured_exit
}

pub(in crate::native) fn loop_exit_selection_convergence(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    loop_info: &super::loopforest::NaturalLoop,
    info: &LoopMergeInfo,
    header: &str,
) -> Option<String> {
    let header_block = blocks.iter().find(|block| block.name == header)?;
    let arms = block_successors(header_block);
    if arms.len() < 2 {
        return None;
    }
    let loop_body: HashSet<&str> = loop_info.body.iter().map(String::as_str).collect();
    let nested_loop_nodes: HashSet<&str> = forest
        .loops
        .iter()
        .filter(|other| other.header != loop_info.header)
        .filter(|other| loop_body.contains(other.header.as_str()))
        .flat_map(|other| other.body.iter().map(String::as_str))
        .collect();
    let all_loop_roles: HashSet<&str> = loop_merges
        .values()
        .flat_map(|role| [role.merge.as_str(), role.continue_target.as_str()])
        .collect();
    let candidate_depth = |name: &str| {
        let mut depth = 0usize;
        let mut cur = name;
        while let Some(parent) = forest.idom(cur) {
            depth += 1;
            cur = parent;
        }
        depth
    };
    let mut candidates: Vec<(&str, usize)> = blocks
        .iter()
        .map(|block| block.name.as_str())
        .filter(|candidate| {
            *candidate != header
                && *candidate != loop_info.header
                && *candidate != info.merge
                && *candidate != info.continue_target
                && loop_body.contains(candidate)
                && !nested_loop_nodes.contains(candidate)
                && !all_loop_roles.contains(candidate)
        })
        .map(|candidate| (candidate, candidate_depth(candidate)))
        .collect();
    candidates.sort_by_key(|(candidate, depth)| (*depth, *candidate));

    let mut best: Option<(usize, usize, &str)> = None;
    for (candidate, depth) in candidates {
        let mut valid = true;
        let mut reaching_arms = 0usize;
        for arm in &arms {
            let mut reached = false;
            let mut seen: HashSet<String> = HashSet::default();
            let mut stack = vec![arm.clone()];
            while let Some(node) = stack.pop() {
                if node == candidate {
                    reached = true;
                    continue;
                }
                if node == info.merge || node == info.continue_target || node == loop_info.header {
                    continue;
                }
                if block_ends_in_void_return(blocks, &node)
                    || block_ends_in_unreachable(blocks, &node)
                {
                    continue;
                }
                if !seen.insert(node.clone()) {
                    continue;
                }
                if !loop_body.contains(node.as_str()) || nested_loop_nodes.contains(node.as_str()) {
                    valid = false;
                    break;
                }
                let Some(block) = blocks.iter().find(|block| block.name == node) else {
                    valid = false;
                    break;
                };
                let successors = block_successors(block);
                if successors.is_empty() {
                    valid = false;
                    break;
                }
                stack.extend(successors);
            }
            if !valid {
                valid = false;
                break;
            }
            reaching_arms += reached as usize;
        }
        if valid && reaching_arms > 0 {
            let replace = best.as_ref().is_none_or(|current| {
                reaching_arms > current.0
                    || (reaching_arms == current.0 && depth < current.1)
                    || (reaching_arms == current.0 && depth == current.1 && candidate < current.2)
            });
            if replace {
                best = Some((reaching_arms, depth, candidate));
            }
        }
    }
    best.map(|(_, _, candidate)| candidate.to_string())
}

pub(in crate::native) fn synth_unique_selection_merge(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    natural: &str,
    counter: &mut usize,
) -> Option<String> {
    let preds: HashSet<String> = header_owned_merge_predecessors(blocks, forest, header, natural)
        .into_iter()
        .collect();
    if preds.is_empty() {
        return None;
    }
    let new_name = format!("{SPLIT_PREFIX}{SEL_TOKEN}{counter}");
    *counter += 1;
    for b in blocks.iter_mut() {
        if preds.contains(&b.name) {
            if let Some(t) = b.typed_mut() {
                t.redirect_successor(natural, &new_name);
            }
        }
    }
    let at = blocks
        .iter()
        .position(|b| b.name == natural)
        .unwrap_or(blocks.len());
    blocks.insert(
        at,
        synthetic_block(
            new_name.clone(),
            vec![format!("br label {natural}")],
            role_for_name(&new_name),
        ),
    );
    Some(new_name)
}

pub(in crate::native) fn synth_unique_selection_merge_phi(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    natural: &str,
    counter: &mut usize,
) -> Option<String> {
    let preds = header_owned_merge_predecessors(blocks, forest, header, natural);
    synth_unique_selection_merge_phi_explicit(blocks, &preds, natural, &HashSet::default(), counter)
}

pub(in crate::native) fn header_owned_merge_predecessors(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
    natural: &str,
) -> Vec<String> {
    blocks
        .iter()
        .filter(|block| {
            block.role != BlockRole::ConstructTreeRoute
                && block_successors(block)
                    .iter()
                    .any(|target| target == natural)
                && forest.dominates(header, &block.name)
        })
        .map(|block| block.name.clone())
        .collect()
}

pub(in crate::native) fn synth_unique_selection_merge_phi_explicit(
    blocks: &mut Vec<BodyBlock>,
    preds: &[String],
    natural: &str,
    routes_into_natural: &HashSet<String>,
    counter: &mut usize,
) -> Option<String> {
    atomic_rewrite(blocks, |blocks| {
        let pred_set = preds.iter().map(String::as_str).collect::<HashSet<_>>();
        let mut redirects = Vec::<(String, String)>::new();
        for block in blocks.iter() {
            if !pred_set.contains(block.name.as_str()) {
                continue;
            }
            for target in block_successors(block) {
                if target == natural || routes_into_natural.contains(&target) {
                    redirects.push((block.name.clone(), target));
                }
            }
        }
        if redirects.is_empty() {
            return None;
        }

        let direct_preds = redirects
            .iter()
            .filter(|(_, target)| target == natural)
            .map(|(pred, _)| pred.as_str())
            .collect::<HashSet<_>>();
        let route_preds = redirects
            .iter()
            .filter(|(_, target)| target != natural)
            .fold(
                HashMap::<&str, Vec<&str>>::default(),
                |mut map, (pred, route)| {
                    map.entry(route.as_str()).or_default().push(pred.as_str());
                    map
                },
            );
        let route_has_unredirected_predecessor = |route: &str| {
            blocks.iter().any(|block| {
                !pred_set.contains(block.name.as_str())
                    && block_successors(block).iter().any(|target| target == route)
            })
        };

        let new_name = format!("{SPLIT_PREFIX}{SEL_TOKEN}{counter}");
        *counter += 1;

        let nat_idx = blocks.iter().position(|b| b.name == natural)?;
        type TypedIncomings = Vec<(crate::native::ir::LlValue, String)>;
        let mut passthrough_merges: Vec<(String, crate::native::ir::LlType, TypedIncomings)> =
            Vec::new();
        let mut nat_rewrites: Vec<(String, TypedIncomings)> = Vec::new();
        if let Some(t) = &blocks[nat_idx].typed {
            for inst in &t.insts {
                let (Some(dst), Some((ty, inc))) =
                    (inst.result.clone(), inst.phi_incoming().clone())
                else {
                    continue;
                };
                let mut typed_red = Vec::new();
                let mut kept_plus = Vec::new();
                for (value, predecessor) in inc {
                    if direct_preds.contains(predecessor.as_str()) {
                        typed_red.push((value, predecessor));
                        continue;
                    }
                    if let Some(owned_preds) = route_preds.get(predecessor.as_str()) {
                        typed_red.extend(
                            owned_preds
                                .iter()
                                .map(|owned| (value.clone(), (*owned).to_string())),
                        );
                        if route_has_unredirected_predecessor(&predecessor) {
                            kept_plus.push((value, predecessor));
                        }
                        continue;
                    }
                    kept_plus.push((value, predecessor));
                }
                if typed_red.is_empty() {
                    continue;
                }
                let merged = format!("{new_name}.phi{}", passthrough_merges.len());
                kept_plus.push((
                    crate::native::ir::LlValue::Local(merged.clone()),
                    new_name.clone(),
                ));
                passthrough_merges.push((merged, ty, typed_red));
                nat_rewrites.push((dst, kept_plus));
            }
        }
        if let Some(t) = blocks[nat_idx].typed_mut() {
            for (dst, kept_plus) in &nat_rewrites {
                t.set_phi_incomings(dst, kept_plus);
            }
        }

        for (predecessor, old_target) in &redirects {
            if let Some(block) = blocks.iter_mut().find(|block| block.name == *predecessor) {
                block.typed_mut()?.redirect_successor(old_target, &new_name);
            }
        }

        let at = blocks
            .iter()
            .position(|b| b.name == natural)
            .unwrap_or(blocks.len());
        let mut blk = crate::native::tir::lower_block_carrier(
            &new_name,
            &[format!("br label {natural}")],
            &crate::fxhash::HashMap::default(),
        )?;
        for (merged, ty, typed_red) in &passthrough_merges {
            blk.push_value_phi(merged, ty, typed_red);
        }
        blocks.insert(
            at,
            BodyBlock {
                name: new_name.clone(),
                role: role_for_name(&new_name),
                typed: Some(blk.into()),
            },
        );

        Some(new_name)
    })
}

pub(in crate::native) fn merge_collides_with_outer_selection_from(
    forest: &LoopForest,
    selection_merges: &HashMap<String, String>,
    header: &str,
    merge: &str,
    converge_inloop: bool,
) -> bool {
    let Some(l) = forest.loop_for_header(header) else {
        return false;
    };
    let body: HashSet<&str> = l.body.iter().map(String::as_str).collect();
    if selection_merges
        .iter()
        .any(|(h, m)| m == merge && !body.contains(h.as_str()))
    {
        return true;
    }
    if converge_inloop {
        return selection_merges
            .iter()
            .any(|(h, m)| m == merge && body.contains(h.as_str()) && h.as_str() != header);
    }
    false
}

pub(in crate::native) fn split_no_phi_overlap(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    exit: &str,
    counter: &mut usize,
) -> Option<String> {
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

    let new_name = format!("{SPLIT_PREFIX}{counter}");
    *counter += 1;

    for b in blocks.iter_mut() {
        if preds.iter().any(|p| p == &b.name) {
            if let Some(t) = b.typed_mut() {
                t.redirect_successor(exit, &new_name);
            }
        }
    }

    let pass_through = synthetic_block(
        new_name.clone(),
        vec![format!("br label {exit}")],
        role_for_name(&new_name),
    );
    let insert_at = blocks
        .iter()
        .position(|b| b.name == exit)
        .unwrap_or(blocks.len());
    blocks.insert(insert_at, pass_through);

    Some(new_name)
}

pub(in crate::native) fn synth_dowhile_continue(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    latch: &str,
    merge: &str,
    counter: &mut usize,
) -> Option<String> {
    let (t, f) = {
        let lb = blocks.iter().find(|b| b.name == latch)?;
        conditional_branch_targets(lb)?
    };
    let arms = [t.as_str(), f.as_str()];
    if !(arms.contains(&header) && arms.contains(&merge)) {
        return None;
    }

    let cont = format!("{CONT_PREFIX}{counter}");
    *counter += 1;

    if let Some(lb) = blocks.iter_mut().find(|b| b.name == latch) {
        if let Some(t) = lb.typed_mut() {
            t.redirect_successor(header, &cont);
        }
    }
    if let Some(hb) = blocks.iter_mut().find(|b| b.name == header) {
        if let Some(t) = hb.typed_mut() {
            t.rewrite_phi_predecessor(latch, &cont);
        }
    }
    let at = blocks
        .iter()
        .position(|b| b.name == header)
        .unwrap_or(blocks.len());
    blocks.insert(
        at,
        synthetic_block(
            cont.clone(),
            vec![format!("br label {header}")],
            role_for_name(&cont),
        ),
    );
    Some(cont)
}

pub(in crate::native) fn split_loop_header_selection(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    merge: &str,
    continue_target: &str,
    loop_body: &HashSet<String>,
    counter: &mut usize,
) -> Option<String> {
    let (t, f) = {
        let hb = blocks.iter().find(|b| b.name == header)?;
        conditional_branch_targets(hb)?
    };
    let roles = [merge, continue_target, header];
    if roles.contains(&t.as_str())
        || roles.contains(&f.as_str())
        || !loop_body.contains(&t)
        || !loop_body.contains(&f)
    {
        return None;
    }

    let sel = format!("{LOOP_HEADER_SELECTION_PREFIX}{counter}");
    *counter += 1;

    let sel_carrier = {
        let hb = blocks.iter_mut().find(|b| b.name == header)?;
        let sel_carrier = hb.typed.as_ref()?.terminator_only_block(&sel);
        if let Some(t) = hb.typed_mut() {
            t.set_unconditional_branch(&sel);
        }
        sel_carrier
    };
    for arm in [&t, &f] {
        if let Some(ab) = blocks.iter_mut().find(|b| &b.name == arm) {
            if let Some(t) = ab.typed_mut() {
                t.rewrite_phi_predecessor(header, &sel);
            }
        }
    }
    let at = blocks
        .iter()
        .position(|b| b.name == header)
        .map(|i| i + 1)
        .unwrap_or(blocks.len());
    blocks.insert(
        at,
        BodyBlock {
            name: sel.clone(),
            role: role_for_name(&sel),
            typed: Some(sel_carrier.into()),
        },
    );
    Some(sel)
}

pub(in crate::native) fn split_loop_header_switch(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    merge: &str,
    continue_target: &str,
    loop_body: &HashSet<String>,
    counter: &mut usize,
) -> Option<String> {
    let targets = {
        let hb = blocks.iter().find(|b| b.name == header)?;
        let is_switch = is_switch_block(hb);
        if !is_switch {
            return None;
        }
        block_successors(hb)
    };
    let roles = [merge, continue_target, header];
    if targets
        .iter()
        .any(|t| roles.contains(&t.as_str()) || !loop_body.contains(t))
    {
        return None;
    }

    let sel = format!("%metal2vulkan.lhsw.{counter}");
    *counter += 1;

    let sel_carrier = {
        let hb = blocks.iter_mut().find(|b| b.name == header)?;
        let sel_carrier = hb.typed.as_ref()?.terminator_only_block(&sel);
        if let Some(t) = hb.typed_mut() {
            t.set_unconditional_branch(&sel);
        }
        sel_carrier
    };
    for arm in &targets {
        if let Some(ab) = blocks.iter_mut().find(|b| &b.name == arm) {
            if let Some(t) = ab.typed_mut() {
                t.rewrite_phi_predecessor(header, &sel);
            }
        }
    }
    let at = blocks
        .iter()
        .position(|b| b.name == header)
        .map(|i| i + 1)
        .unwrap_or(blocks.len());
    blocks.insert(
        at,
        BodyBlock {
            name: sel.clone(),
            role: role_for_name(&sel),
            typed: Some(sel_carrier.into()),
        },
    );
    Some(sel)
}

pub(in crate::native) fn loop_break_continue_merge(
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    b: &str,
    t: &str,
    f: &str,
) -> Option<String> {
    for l in &forest.loops {
        if !l.body.iter().any(|n| n == b) {
            continue;
        }
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let arms = [t, f];
        if !arms.contains(&info.merge.as_str()) {
            continue;
        }
        let other = if t == info.merge { f } else { t };
        if other == info.continue_target
            && info.merge != info.continue_target
            && info.continue_target != b
        {
            return Some(info.continue_target.clone());
        }
    }
    None
}

pub(in crate::native) fn bare_loop_exit_branch(
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    b: &str,
    t: &str,
    f: &str,
) -> bool {
    for l in &forest.loops {
        if l.header == b || !l.body.iter().any(|n| n == b) {
            continue;
        }
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let is_role = |n: &str| n == info.merge || n == info.continue_target;
        if (is_role(t) || is_role(f)) && t != l.header && f != l.header {
            return true;
        }
    }
    false
}

pub(in crate::native) fn bare_loop_exit_branch_with_passthroughs(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    b: &str,
    t: &str,
    f: &str,
) -> bool {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    for l in &forest.loops {
        if l.header == b || !l.body.iter().any(|n| n == b) {
            continue;
        }
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        let is_role = |n: &str| loop_role_or_passthrough(&by_name, n, info);
        if (is_role(t) || is_role(f)) && t != l.header && f != l.header {
            return true;
        }
    }
    false
}

pub(in crate::native) fn bare_natural_loop_exit_branch(
    forest: &LoopForest,
    block: &str,
    true_target: &str,
    false_target: &str,
) -> bool {
    forest.loops.iter().any(|loop_| {
        loop_.header != block
            && loop_.body.iter().any(|name| name == block)
            && ((loop_.body.iter().any(|name| name == true_target)
                && loop_.exits.iter().any(|name| name == false_target))
                || (loop_.body.iter().any(|name| name == false_target)
                    && loop_.exits.iter().any(|name| name == true_target)))
    })
}

pub(in crate::native) fn enclosing_selection_region_exit_target(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    selection_merges: &HashMap<String, String>,
    b: &str,
    t: &str,
    f: &str,
    merge: Option<&str>,
) -> Option<String> {
    if b.starts_with(LOOP_HEADER_SELECTION_PREFIX) {
        return None;
    }
    for target in [t, f] {
        if merge.is_some_and(|merge| target == merge) {
            continue;
        }
        if !forest.dominates(b, target) {
            continue;
        }
        if let Some(exit) = first_enclosing_selection_region_exit(
            blocks,
            forest,
            loop_merges,
            selection_merges,
            b,
            target,
            merge,
        ) {
            return Some(exit);
        }
    }
    None
}

fn first_enclosing_selection_region_exit(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    selection_merges: &HashMap<String, String>,
    b: &str,
    target: &str,
    merge: Option<&str>,
) -> Option<String> {
    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::default();
    let mut stack = vec![target.to_string()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if merge.is_some_and(|merge| node == merge) {
            continue;
        }
        let Some(block) = by_name.get(node.as_str()).map(|index| &blocks[*index]) else {
            continue;
        };
        for successor in block_successors(block) {
            if merge.is_some_and(|merge| successor == merge) {
                continue;
            }
            if legal_enclosing_loop_exit(forest, loop_merges, &node, &successor) {
                continue;
            }
            if forest.dominates(b, &successor) {
                stack.push(successor);
            } else if exits_to_enclosing_selection_region(
                blocks,
                forest,
                selection_merges,
                &by_name,
                b,
                &successor,
            ) {
                return Some(successor);
            }
        }
    }
    None
}

fn legal_enclosing_loop_exit(
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    source: &str,
    target: &str,
) -> bool {
    forest.loops.iter().any(|loop_info| {
        let encloses =
            loop_info.header == source || loop_info.body.iter().any(|name| name == source);
        encloses
            && (loop_info.header == target
                || loop_merges
                    .get(&loop_info.header)
                    .is_some_and(|info| info.merge == target || info.continue_target == target))
    })
}

pub(in crate::native) fn ordinary_selection_enclosing_boundary_target(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    source_selection_merges: &HashMap<String, String>,
    header: &str,
    merge: &str,
) -> Option<String> {
    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let header_block = &blocks[*by_name.get(header)?];
    let (true_target, false_target) = conditional_branch_targets(header_block)?;
    if bare_loop_exit_branch_with_passthroughs(
        blocks,
        forest,
        loop_merges,
        header,
        &true_target,
        &false_target,
    ) {
        return None;
    }
    let header_successors = block_successors(header_block);
    if forest.loops.iter().any(|loop_info| {
        loop_info.latches.iter().any(|latch| latch == header)
            && header_successors
                .iter()
                .any(|successor| successor == &loop_info.header)
    }) {
        return None;
    }
    if by_name
        .get(merge)
        .map(|index| &blocks[*index])
        .is_some_and(is_bare_unreachable)
    {
        return None;
    }

    let mut seen = HashSet::default();
    let mut stack = header_successors
        .into_iter()
        .filter(|target| target != merge)
        .collect::<Vec<_>>();
    let mut boundary = None;
    while let Some(node) = stack.pop() {
        if node == merge || !seen.insert(node.clone()) {
            continue;
        }
        let Some(block) = by_name.get(node.as_str()).map(|index| &blocks[*index]) else {
            continue;
        };
        if !forest.dominates(header, &node) {
            continue;
        }
        for successor in block_successors(block) {
            if successor == merge
                || legal_enclosing_loop_exit(forest, loop_merges, &node, &successor)
            {
                continue;
            }
            if forest.dominates(header, &successor) {
                stack.push(successor);
                continue;
            }
            if exits_to_enclosing_selection_region(
                blocks,
                forest,
                source_selection_merges,
                &by_name,
                &node,
                &successor,
            ) {
                if boundary
                    .as_ref()
                    .is_some_and(|current: &String| current != &successor)
                {
                    return None;
                }
                boundary = Some(successor);
            }
        }
    }
    boundary
}

fn dominated_region_exits(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
    merge: Option<&str>,
) -> HashSet<String> {
    blocks
        .iter()
        .filter(|block| forest.dominates(header, &block.name))
        .filter(|block| merge.is_none_or(|merge| block.name != merge))
        .flat_map(block_successors)
        .filter(|successor| merge.is_none_or(|merge| successor != merge))
        .filter(|successor| !forest.dominates(header, successor))
        .collect()
}

#[derive(Default)]
pub(in crate::native) struct PureEnclosingSelectionOwners {
    dependents_by_owner: HashMap<String, HashSet<String>>,
}

pub(in crate::native) fn pure_enclosing_selection_owners(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    source_selection_merges: &HashMap<String, String>,
) -> PureEnclosingSelectionOwners {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut dependents_by_owner: HashMap<String, HashSet<String>> = HashMap::default();
    for (header, merge) in source_selection_merges {
        let Some((left, right)) = by_name
            .get(header.as_str())
            .and_then(|block| conditional_branch_targets(block))
        else {
            continue;
        };
        if (left == *merge) == (right == *merge) {
            continue;
        }
        let mut current = header.as_str();
        while let Some(parent) = forest.idom(current) {
            if source_selection_merges.contains_key(parent) {
                dependents_by_owner
                    .entry(parent.to_string())
                    .or_default()
                    .insert(header.clone());
            }
            current = parent;
        }
    }
    PureEnclosingSelectionOwners {
        dependents_by_owner,
    }
}

pub(in crate::native) fn materialize_pure_enclosing_selection_routes_for_owner(
    blocks: &mut Vec<BodyBlock>,
    loop_merges: &HashMap<String, LoopMergeInfo>,
    header_merges: &mut HashMap<String, String>,
    indexed_owners: &PureEnclosingSelectionOwners,
    owner: &str,
    counter: &mut usize,
) -> bool {
    let Some(dependents) = indexed_owners.dependents_by_owner.get(owner) else {
        return false;
    };
    let mut dependents = dependents.clone();
    let mut changed = false;
    let mut declined = HashSet::default();
    loop {
        let forest = analyze(blocks);
        let by_name = blocks
            .iter()
            .enumerate()
            .map(|(index, block)| (block.name.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut headers = header_merges
            .iter()
            .filter_map(|(header, merge)| {
                dependents.contains(header).then_some(())?;
                (!declined.contains(header)).then_some(())?;
                let block = &blocks[*by_name.get(header)?];
                conditional_branch_targets(block)?;
                Some((
                    header.clone(),
                    merge.clone(),
                    depth_from_forest(&forest, header),
                ))
            })
            .collect::<Vec<_>>();
        headers.sort_by(|left, right| right.2.cmp(&left.2).then(left.0.cmp(&right.0)));

        let mut candidate = None;
        for (header, merge, _) in headers {
            let block = &blocks[by_name[&header]];
            let Some((left, right)) = conditional_branch_targets(block) else {
                continue;
            };
            let other = match (left == merge, right == merge) {
                (true, false) => right,
                (false, true) => left,
                _ => continue,
            };
            let Some(merge_block) = by_name.get(&merge).map(|index| &blocks[*index]) else {
                continue;
            };
            let merge_successors = block_successors(merge_block);
            let [merge_successor] = merge_successors.as_slice() else {
                continue;
            };
            let merge_routes_out = !forest.dominates(&header, merge_successor)
                && exits_to_enclosing_selection_region(
                    blocks,
                    &forest,
                    header_merges,
                    &by_name,
                    &header,
                    merge_successor,
                );
            let other_routes_out = !forest.dominates(&header, &other)
                && exits_to_enclosing_selection_region(
                    blocks,
                    &forest,
                    header_merges,
                    &by_name,
                    &header,
                    &other,
                );
            if merge_routes_out && other_routes_out {
                candidate = Some((header, other));
                break;
            }
        }
        let Some((header, other)) = candidate else {
            break;
        };

        let mut next_counter = *counter;
        let Some(cloned) =
            crate::native::cfg::clone_crossarm::privatize_dominated_region_with_renames(
                blocks,
                &header,
                &other,
                &mut next_counter,
            )
        else {
            declined.insert(header);
            continue;
        };
        let mut next_blocks = cloned.blocks;
        let cloned_merges = header_merges
            .iter()
            .filter_map(|(owner, merge)| {
                Some((
                    owner.clone(),
                    cloned.renamed.get(owner)?.clone(),
                    cloned
                        .renamed
                        .get(merge)
                        .cloned()
                        .unwrap_or_else(|| merge.clone()),
                ))
            })
            .collect::<Vec<_>>();

        let next_forest = analyze(&next_blocks);
        let Some(natural) = selection_merges(&next_blocks, &next_forest).remove(&header) else {
            declined.insert(header);
            continue;
        };
        let claims = header_merges
            .iter()
            .filter(|(owner, _)| owner.as_str() != header)
            .map(|(_, merge)| merge.clone())
            .chain(cloned_merges.iter().map(|(_, _, merge)| merge.clone()))
            .collect::<HashSet<_>>();
        let loop_roles = loop_role_targets_with_passthroughs(&next_blocks, loop_merges);
        let private = if next_forest.dominates(&header, &natural)
            && !claims.contains(&natural)
            && !loop_roles.contains(&natural)
        {
            Some(natural)
        } else if block_has_phi(&next_blocks, &natural) {
            synth_unique_selection_merge_phi(
                &mut next_blocks,
                &next_forest,
                &header,
                &natural,
                &mut next_counter,
            )
        } else {
            synth_unique_selection_merge(
                &mut next_blocks,
                &next_forest,
                &header,
                &natural,
                &mut next_counter,
            )
        };
        let Some(private) = private else {
            declined.insert(header);
            continue;
        };
        *blocks = next_blocks;
        for (_, cloned_owner, _) in &cloned_merges {
            dependents.insert(cloned_owner.clone());
        }
        let mut new_owners = cloned_merges
            .into_iter()
            .map(|(_, owner, merge)| (owner, merge))
            .collect::<Vec<_>>();
        new_owners.push((header.clone(), private.clone()));
        let next_forest = analyze(blocks);
        header_merges.extend(new_owners.iter().cloned());
        let terminal_links = terminal_parent_links(blocks, &next_forest);
        let mut composition_owners = new_owners
            .iter()
            .map(|(owner, _)| owner.clone())
            .collect::<Vec<_>>();
        composition_owners.sort_by(|left, right| {
            depth_from_forest(&next_forest, right)
                .cmp(&depth_from_forest(&next_forest, left))
                .then(left.cmp(right))
        });
        for owner in composition_owners {
            let completed = complete_terminal_parent_ownership(
                blocks,
                header_merges,
                &terminal_links,
                &owner,
                &mut next_counter,
            );
            for completed_owner in completed {
                if let Some(completed_dependents) =
                    indexed_owners.dependents_by_owner.get(&completed_owner)
                {
                    dependents.extend(completed_dependents.iter().cloned());
                }
                if blocks
                    .iter()
                    .find(|block| block.name == completed_owner)
                    .is_some_and(is_switch_block)
                {
                    finalize_fully_terminal_switch(
                        blocks,
                        header_merges,
                        &completed_owner,
                        &mut next_counter,
                    );
                }
            }
        }
        *counter = next_counter;
        declined.clear();
        changed = true;
    }
    changed
}

fn depth_from_forest(forest: &LoopForest, name: &str) -> usize {
    let mut depth = 0usize;
    let mut current = name;
    while let Some(parent) = forest.idom(current) {
        depth += 1;
        current = parent;
    }
    depth
}

pub(in crate::native) fn bare_enclosing_selection_region_escape(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    selection_merges: &HashMap<String, String>,
    source: &str,
    target: &str,
) -> bool {
    if forest.dominates(source, target) {
        return false;
    }
    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut current = Some(source);
    while let Some(candidate) = current {
        if let Some(block) = by_name.get(candidate).map(|index| &blocks[*index]) {
            if !selection_merges.contains_key(candidate)
                && conditional_branch_targets(block).is_some()
                && !dominated_region_exits(blocks, forest, candidate, None).is_empty()
                && forest.dominates(candidate, source)
                && !forest.dominates(candidate, target)
                && exits_to_enclosing_selection_region(
                    blocks,
                    forest,
                    selection_merges,
                    &by_name,
                    candidate,
                    target,
                )
            {
                return true;
            }
        }
        current = forest.idom(candidate);
    }
    false
}

fn exits_to_enclosing_selection_region(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    selection_merges: &HashMap<String, String>,
    by_name: &HashMap<String, usize>,
    b: &str,
    target: &str,
) -> bool {
    let mut current = b;
    while let Some(parent) = forest.idom(current) {
        if let Some(merge) = selection_merges.get(parent) {
            if let Some(header) = by_name.get(parent).map(|index| &blocks[*index]) {
                if let Some((left, right)) = conditional_branch_targets(header) {
                    let in_left = forest.dominates(&left, b);
                    let in_right = forest.dominates(&right, b);
                    if in_left != in_right && (target == merge || forest.dominates(parent, target))
                    {
                        return true;
                    }
                }
            }
        }
        current = parent;
    }
    false
}

fn loop_role_or_passthrough(
    by_name: &HashMap<&str, &BodyBlock>,
    start: &str,
    info: &LoopMergeInfo,
) -> bool {
    let mut current = start.to_string();
    for _ in 0..=by_name.len() {
        if current == info.merge || current == info.continue_target {
            return true;
        }
        let Some(block) = by_name.get(current.as_str()) else {
            return false;
        };
        if !matches!(
            block.role,
            BlockRole::LMerge | BlockRole::ConstructTreeRoute
        ) {
            return false;
        }
        let successors = block_successors(block);
        if successors.len() != 1 || successors[0] == current {
            return false;
        }
        current = successors[0].clone();
    }
    false
}

pub(in crate::native) fn bare_exit_escape_reason(
    ordered: &[BodyBlock],
    header_merge: &HashMap<String, String>,
    loop_merges: &HashMap<String, LoopMergeInfo>,
) -> Option<&'static str> {
    let forest = analyze(ordered);
    let names: HashSet<&str> = ordered.iter().map(|b| b.name.as_str()).collect();
    let switch_targets: HashMap<&str, HashSet<String>> = ordered
        .iter()
        .filter(|b| is_switch_block(b))
        .map(|b| (b.name.as_str(), block_successors(b).into_iter().collect()))
        .collect();
    for b in ordered {
        for s in block_successors(b) {
            if s == b.name || !names.contains(s.as_str()) {
                continue;
            }
            if forest.dominates(&b.name, &s) || header_merge.get(&b.name).is_some_and(|m| m == &s) {
                continue;
            }
            let loop_ok = forest.loops.iter().any(|l| {
                let encloses = l.header == b.name || l.body.iter().any(|n| n == &b.name);
                encloses
                    && (l.header == s
                        || loop_merges
                            .get(&l.header)
                            .is_some_and(|i| i.merge == s || i.continue_target == s))
            });
            let sel_break = header_merge
                .iter()
                .any(|(h, m)| m == &s && h != &b.name && forest.dominates(h, &b.name));
            let case_ok = switch_targets.iter().any(|(h, tg)| {
                *h != b.name.as_str() && forest.dominates(h, &b.name) && tg.contains(&s)
            });
            let enclosing_selection_region_ok =
                bare_enclosing_selection_region_escape(ordered, &forest, header_merge, &b.name, &s);
            if loop_ok || sel_break || case_ok || enclosing_selection_region_ok {
                continue;
            }
            if crate::env_vars::exit_why() {
                eprintln!(
                    "[exit-why] source={} target={} selection-merge={:?} enclosing-loops={:?}",
                    b.name,
                    s,
                    header_merge.get(&b.name),
                    forest
                        .loops
                        .iter()
                        .filter(|natural_loop| {
                            natural_loop.header == b.name
                                || natural_loop.body.iter().any(|node| node == &b.name)
                        })
                        .map(|natural_loop| (
                            &natural_loop.header,
                            loop_merges.get(&natural_loop.header)
                        ))
                        .collect::<Vec<_>>(),
                );
            }
            return Some("bare-exit:unstructured-escape");
        }
    }
    None
}

pub(in crate::native) fn dominance_loop_exit_escape_reason(
    blocks: &[BodyBlock],
    loop_merges: &HashMap<String, LoopMergeInfo>,
) -> Option<&'static str> {
    let forest = analyze(blocks);
    let names = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    for block in blocks {
        for successor in block_successors(block) {
            if successor == block.name || !names.contains(successor.as_str()) {
                continue;
            }
            for natural_loop in &forest.loops {
                let Some(info) = loop_merges.get(&natural_loop.header) else {
                    continue;
                };
                let source_inside = natural_loop.header == block.name
                    || (forest.dominates(&natural_loop.header, &block.name)
                        && !forest.dominates(&info.merge, &block.name));
                if !source_inside {
                    continue;
                }
                let target_inside = forest.dominates(&natural_loop.header, &successor)
                    && !forest.dominates(&info.merge, &successor);
                let target_is_role = natural_loop.header == successor
                    || info.merge == successor
                    || info.continue_target == successor;
                if target_inside || target_is_role {
                    continue;
                }
                if crate::env_vars::exit_why() {
                    eprintln!(
                        "[exit-why] source={} target={} loop={} merge={} continue={}",
                        block.name,
                        successor,
                        natural_loop.header,
                        info.merge,
                        info.continue_target,
                    );
                }
                return Some("loop-exit:dominance-owned-bypass");
            }
        }
    }
    None
}
