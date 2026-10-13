use super::*;

pub(in crate::native) const TERMINAL_EXIT_SELECTION_MAX_BLOCKS: usize = 300;

pub(in crate::native) const TERMINAL_EXIT_COUNTER_START: usize = 3_000_000;

pub(in crate::native) struct TerminalExitSelectionPlan {
    pub(in crate::native) blocks: Vec<BodyBlock>,
    pub(in crate::native) merges: HashMap<String, String>,
}

pub(in crate::native) fn prepare_terminal_exit_selection(
    blocks: &[BodyBlock],
) -> Option<TerminalExitSelectionPlan> {
    let seed = privatize_single_loop_return_exit(blocks);
    terminal_exit_selection_merges(seed.as_deref().unwrap_or(blocks))
}

pub(in crate::native) fn privatize_single_loop_return_exit(
    blocks: &[BodyBlock],
) -> Option<Vec<BodyBlock>> {
    if blocks.len() > TERMINAL_EXIT_SELECTION_MAX_BLOCKS {
        return None;
    }
    let forest = analyze(blocks);
    let [loop_info] = forest.loops.as_slice() else {
        return None;
    };
    if loop_info.latches.len() != 1 || !(1..=2).contains(&loop_info.exits.len()) {
        return None;
    }
    let return_exits: Vec<&String> = loop_info
        .exits
        .iter()
        .filter(|exit| {
            blocks
                .iter()
                .find(|block| block.name == exit.as_str())
                .is_some_and(is_bare_ret_void)
        })
        .collect();
    let [exit] = return_exits.as_slice() else {
        return None;
    };
    let exit = exit.as_str();
    if loop_info.exits.len() == 2
        && !loop_info
            .exits
            .iter()
            .any(|candidate| block_ends_in_unreachable(blocks, candidate))
    {
        return None;
    }
    let exit_block = blocks.iter().find(|block| block.name == exit)?;
    debug_assert!(exit_block
        .typed
        .as_ref()
        .is_some_and(|t| t.insts.is_empty()));

    let loop_nodes: HashSet<&str> = loop_info.body.iter().map(String::as_str).collect();
    let mut loop_predecessors = Vec::new();
    let mut has_outer_predecessor = false;
    for block in blocks {
        if !block_successors(block).iter().any(|target| target == exit) {
            continue;
        }
        if loop_nodes.contains(block.name.as_str()) {
            loop_predecessors.push(block.name.clone());
        } else {
            has_outer_predecessor = true;
        }
    }
    if loop_predecessors.is_empty() || !has_outer_predecessor {
        return None;
    }

    let names: HashSet<&str> = blocks.iter().map(|block| block.name.as_str()).collect();
    let mut id = TERMINAL_EXIT_COUNTER_START;
    let private_return = loop {
        let candidate = format!("{SPLIT_PREFIX}{TLOOPRET_TOKEN}{id}");
        if !names.contains(candidate.as_str()) {
            break candidate;
        }
        id += 1;
    };

    let mut out = blocks.to_vec();
    for predecessor in &loop_predecessors {
        let idx = out.iter().position(|block| block.name == *predecessor)?;
        if !block_successors(&out[idx]).iter().any(|s| s == exit) {
            return None;
        }
        out[idx]
            .typed_mut()?
            .redirect_successor(exit, &private_return);
    }
    let role = role_for_name(&private_return);
    out.push(synthetic_block(
        private_return,
        vec!["ret void".to_string()],
        role,
    ));
    Some(out)
}

pub(in crate::native) fn terminal_exit_selection_merges(
    blocks: &[BodyBlock],
) -> Option<TerminalExitSelectionPlan> {
    let mut out = blocks.to_vec();
    let natural_loops = analyze(&out).loops;
    let mut merges = HashMap::default();
    let mut counter = TERMINAL_EXIT_COUNTER_START;
    let mut changed = false;
    loop {
        let forest =
            crate::native::cfg::loopforest::analyze_reusing_natural_loops(&out, &natural_loops);
        let loop_nodes: HashSet<&str> = forest
            .loops
            .iter()
            .flat_map(|loop_info| loop_info.body.iter().map(String::as_str))
            .collect();
        let header_depth = |name: &str| {
            let mut depth = 0usize;
            let mut current = name;
            while let Some(parent) = forest.idom(current) {
                depth += 1;
                current = parent;
            }
            depth
        };
        let mut headers: Vec<(String, usize)> = out
            .iter()
            .filter(|block| !merges.contains_key(&block.name))
            .filter(|block| !loop_nodes.contains(block.name.as_str()))
            .filter(|block| conditional_branch_targets(block).is_some())
            .map(|block| (block.name.clone(), header_depth(&block.name)))
            .collect();

        let mut shared_headers = headers.clone();
        shared_headers.sort_by(|(left, left_depth), (right, right_depth)| {
            left_depth.cmp(right_depth).then(left.cmp(right))
        });
        let mut applied = false;
        for (header, _) in shared_headers {
            if let Some(private_return) =
                synth_shared_void_return_selection_merge(&mut out, &forest, &header, &mut counter)
            {
                merges.insert(header, private_return);
                changed = true;
                applied = true;
            }
        }
        if applied {
            continue;
        }

        headers.sort_by(|(left, left_depth), (right, right_depth)| {
            right_depth.cmp(left_depth).then(left.cmp(right))
        });

        'headers: for (header, _) in headers {
            if let Some(terminal) = terminal_exit_continuation(&out, &forest, &header) {
                let mut candidate = out.clone();
                let mut candidate_counter = counter;
                let Some(merge) = synth_terminal_selection_merge(
                    &mut candidate,
                    &forest,
                    &header,
                    &terminal.continuation,
                    &mut candidate_counter,
                ) else {
                    continue;
                };
                let Some(_) = synth_terminal_return_clone(
                    &mut candidate,
                    &header,
                    &terminal.exit_arm,
                    &terminal.return_arm,
                    &mut candidate_counter,
                ) else {
                    continue;
                };
                out = candidate;
                counter = candidate_counter;
                merges.insert(header, merge);
                changed = true;
                applied = true;
                break 'headers;
            }

            if let Some(continuation) = terminal_owned_arm_continuation(&out, &merges, &header) {
                let mut candidate = out.clone();
                let mut candidate_counter = counter;
                let Some(merge) = synth_terminal_selection_merge(
                    &mut candidate,
                    &forest,
                    &header,
                    &continuation,
                    &mut candidate_counter,
                ) else {
                    continue;
                };
                out = candidate;
                counter = candidate_counter;
                merges.insert(header, merge);
                changed = true;
                applied = true;
                break 'headers;
            }

            if let Some(merge) = terminal_exit_bridge_merge(&out, &forest, &merges, &header) {
                merges.insert(header, merge);
                changed = true;
                applied = true;
                break 'headers;
            }

            if blocks.len() > TERMINAL_EXIT_SELECTION_MAX_BLOCKS {
                continue;
            }

            if let Some(convergence) = terminal_exit_convergence(&out, &forest, &header) {
                let mut candidate = out.clone();
                let mut candidate_counter = counter;
                let Some(merge) = synth_terminal_selection_merge(
                    &mut candidate,
                    &forest,
                    &header,
                    &convergence,
                    &mut candidate_counter,
                ) else {
                    continue;
                };
                out = candidate;
                counter = candidate_counter;
                merges.insert(header, merge);
                changed = true;
                applied = true;
                break 'headers;
            }
        }
        if !applied {
            break;
        }
    }

    changed.then_some(TerminalExitSelectionPlan {
        blocks: out,
        merges,
    })
}

pub(in crate::native) fn direct_terminal_exit_selection_merges(
    blocks: &[BodyBlock],
    already_owned: &HashMap<String, String>,
) -> Option<TerminalExitSelectionPlan> {
    let forest = analyze(blocks);
    let loop_nodes = forest
        .loops
        .iter()
        .flat_map(|loop_info| loop_info.body.iter().cloned())
        .collect::<HashSet<_>>();
    let depth = |name: &str| {
        let mut depth = 0usize;
        let mut current = name;
        while let Some(parent) = forest.idom(current) {
            depth += 1;
            current = parent;
        }
        depth
    };
    let mut guards = blocks
        .iter()
        .filter(|block| {
            !already_owned.contains_key(&block.name) && !loop_nodes.contains(&block.name)
        })
        .filter_map(|block| {
            let (left, right) = conditional_branch_targets(block)?;
            let left_returns = blocks
                .iter()
                .find(|candidate| candidate.name == left)
                .is_some_and(is_bare_ret_void);
            let right_returns = blocks
                .iter()
                .find(|candidate| candidate.name == right)
                .is_some_and(is_bare_ret_void);
            let (continuation, exit_arm, return_arm) = match (left_returns, right_returns) {
                (true, false) => (
                    right,
                    left.clone(),
                    TerminalReturnArm {
                        return_block: left,
                        predecessor: None,
                    },
                ),
                (false, true) => (
                    left,
                    right.clone(),
                    TerminalReturnArm {
                        return_block: right,
                        predecessor: None,
                    },
                ),
                _ => {
                    let terminal = terminal_exit_continuation(blocks, &forest, &block.name)?;
                    (
                        terminal.continuation,
                        terminal.exit_arm,
                        terminal.return_arm,
                    )
                }
            };
            Some((
                block.name.clone(),
                continuation,
                exit_arm,
                return_arm,
                depth(&block.name),
            ))
        })
        .collect::<Vec<_>>();
    guards.sort_by(|left, right| right.4.cmp(&left.4).then(left.0.cmp(&right.0)));

    let mut out = blocks.to_vec();
    let mut merges = HashMap::default();
    let mut counter = TERMINAL_EXIT_COUNTER_START;
    for (header, continuation, exit_arm, return_arm, _) in guards {
        let mut candidate = out.clone();
        let Some(header_index) = candidate.iter().position(|block| block.name == header) else {
            continue;
        };
        let Some(continuation_index) = candidate
            .iter()
            .position(|block| block.name == continuation)
        else {
            continue;
        };
        let names = candidate
            .iter()
            .map(|block| block.name.as_str())
            .collect::<HashSet<_>>();
        let private_merge = loop {
            let candidate_name = format!("{SPLIT_PREFIX}{SEL_TOKEN}{counter}");
            counter += 1;
            if !names.contains(candidate_name.as_str()) {
                break candidate_name;
            }
        };
        let Some(typed) = candidate[header_index].typed_mut() else {
            continue;
        };
        typed.redirect_successor(&continuation, &private_merge);
        let Some(typed) = candidate[continuation_index].typed_mut() else {
            continue;
        };
        typed.rewrite_phi_predecessor(&header, &private_merge);
        candidate.insert(
            continuation_index,
            synthetic_block(
                private_merge.clone(),
                vec![format!("br label {continuation}")],
                BlockRole::LMerge,
            ),
        );
        if synth_terminal_return_clone(
            &mut candidate,
            &header,
            &exit_arm,
            &return_arm,
            &mut counter,
        )
        .is_none()
        {
            continue;
        }
        out = candidate;
        merges.insert(header, private_merge);
    }
    (!merges.is_empty()).then_some(TerminalExitSelectionPlan {
        blocks: out,
        merges,
    })
}

pub(in crate::native) fn coalesce_sibling_conditional_dispatches(
    blocks: &mut Vec<BodyBlock>,
) -> bool {
    let mut changed = false;
    loop {
        let forest = analyze(blocks);
        let loop_nodes = forest
            .loops
            .iter()
            .flat_map(|loop_info| loop_info.body.iter().map(String::as_str))
            .collect::<HashSet<_>>();
        let mut predecessors = HashMap::<String, Vec<String>>::default();
        for block in blocks.iter() {
            for successor in block_successors(block) {
                predecessors
                    .entry(successor)
                    .or_default()
                    .push(block.name.clone());
            }
        }

        type ValuePhi = (
            String,
            crate::native::ir::LlType,
            Vec<(crate::native::ir::LlValue, String)>,
        );
        type TargetRewrite = (String, String, Vec<(crate::native::ir::LlValue, String)>);
        let mut candidate = None;
        for header in blocks.iter() {
            let Some((left, right)) = conditional_branch_targets(header) else {
                continue;
            };
            if loop_nodes.contains(header.name.as_str())
                || loop_nodes.contains(left.as_str())
                || loop_nodes.contains(right.as_str())
                || predecessors
                    .get(&left)
                    .is_none_or(|preds| preds != std::slice::from_ref(&header.name))
                || predecessors
                    .get(&right)
                    .is_none_or(|preds| preds != std::slice::from_ref(&header.name))
            {
                continue;
            }
            let Some(left_block) = blocks.iter().find(|block| block.name == left) else {
                continue;
            };
            let Some(right_block) = blocks.iter().find(|block| block.name == right) else {
                continue;
            };
            let (Some(left_typed), Some(right_typed)) =
                (left_block.typed.as_ref(), right_block.typed.as_ref())
            else {
                continue;
            };
            let (
                crate::native::tir::TirTerminator::BrCond {
                    cond: left_cond,
                    t: left_true,
                    f: left_false,
                },
                crate::native::tir::TirTerminator::BrCond {
                    cond: right_cond,
                    t: right_true,
                    f: right_false,
                },
            ) = (&left_typed.terminator, &right_typed.terminator)
            else {
                continue;
            };
            if left_true != right_true || left_false != right_false || left_true == left_false {
                continue;
            }
            let bool_value = |value: &str| match value {
                "true" => Some(crate::native::ir::LlValue::Bool(true)),
                "false" => Some(crate::native::ir::LlValue::Bool(false)),
                local if local.starts_with('%') => {
                    Some(crate::native::ir::LlValue::Local(local.to_string()))
                }
                _ => None,
            };
            let (Some(left_value), Some(right_value)) =
                (bool_value(left_cond), bool_value(right_cond))
            else {
                continue;
            };

            let names = blocks
                .iter()
                .map(|block| block.name.as_str())
                .collect::<HashSet<_>>();
            let mut suffix = TERMINAL_EXIT_COUNTER_START;
            let merge = loop {
                let name = format!("{SPLIT_PREFIX}{SEL_TOKEN}{suffix}");
                suffix += 1;
                if !names.contains(name.as_str()) {
                    break name;
                }
            };
            let mut value_phis = Vec::<ValuePhi>::new();
            let mut rewrites = Vec::<TargetRewrite>::new();
            let mut supported = true;
            for target in [left_true, left_false] {
                let Some(target_block) = blocks.iter().find(|block| block.name == *target) else {
                    supported = false;
                    break;
                };
                let Some(typed) = target_block.typed.as_ref() else {
                    supported = false;
                    break;
                };
                for inst in &typed.insts {
                    if !inst.is_phi() {
                        continue;
                    }
                    let (Some(result), Some((ty, incoming))) =
                        (inst.result.clone(), inst.phi_incoming().clone())
                    else {
                        supported = false;
                        break;
                    };
                    let left_incoming = incoming
                        .iter()
                        .find(|(_, predecessor)| predecessor == &left)
                        .map(|(value, _)| value.clone());
                    let right_incoming = incoming
                        .iter()
                        .find(|(_, predecessor)| predecessor == &right)
                        .map(|(value, _)| value.clone());
                    match (left_incoming, right_incoming) {
                        (None, None) => continue,
                        (Some(left_value), Some(right_value)) => {
                            let merged = format!("{merge}.value{}", value_phis.len());
                            let mut kept = incoming
                                .into_iter()
                                .filter(|(_, predecessor)| {
                                    predecessor != &left && predecessor != &right
                                })
                                .collect::<Vec<_>>();
                            kept.push((
                                crate::native::ir::LlValue::Local(merged.clone()),
                                merge.clone(),
                            ));
                            value_phis.push((
                                merged,
                                ty,
                                vec![(left_value, left.clone()), (right_value, right.clone())],
                            ));
                            rewrites.push((target.to_string(), result, kept));
                        }
                        _ => {
                            supported = false;
                            break;
                        }
                    }
                }
                if !supported {
                    break;
                }
            }
            if supported {
                candidate = Some((
                    left,
                    right,
                    left_true.clone(),
                    left_false.clone(),
                    left_value,
                    right_value,
                    merge,
                    value_phis,
                    rewrites,
                ));
                break;
            }
        }

        let Some((
            left,
            right,
            true_target,
            false_target,
            left_value,
            right_value,
            merge,
            value_phis,
            rewrites,
        )) = candidate
        else {
            break;
        };
        for arm in [&left, &right] {
            let Some(block) = blocks.iter_mut().find(|block| block.name == *arm) else {
                return changed;
            };
            block
                .typed_mut()
                .expect("candidate required a typed sibling arm")
                .set_terminator_line(&format!("br label {merge}"));
        }
        for (target, result, incoming) in &rewrites {
            let Some(block) = blocks.iter_mut().find(|block| block.name == *target) else {
                return changed;
            };
            block
                .typed_mut()
                .expect("candidate required a typed target")
                .set_phi_incomings(result, incoming);
        }
        let selector = format!("{merge}.selector");
        let mut carrier = crate::native::tir::lower_block_carrier(
            &merge,
            &[format!(
                "br i1 {selector}, label {true_target}, label {false_target}"
            )],
            &HashMap::default(),
        )
        .expect("synthetic conditional carrier is always parseable");
        for (result, ty, incoming) in &value_phis {
            carrier.push_value_phi(result, ty, incoming);
        }
        carrier.push_value_phi(
            &selector,
            &crate::native::ir::LlType::Int(1),
            &[(left_value, left), (right_value, right)],
        );
        let insert_at = blocks
            .iter()
            .position(|block| block.name == true_target || block.name == false_target)
            .unwrap_or(blocks.len());
        blocks.insert(
            insert_at,
            BodyBlock {
                name: merge.clone(),
                role: role_for_name(&merge),
                typed: Some(carrier.into()),
            },
        );
        changed = true;
    }
    changed
}

#[derive(Default)]
pub(in crate::native) struct TerminalParentLinks {
    child_by_parent: HashMap<String, String>,
    parents_by_child: HashMap<String, Vec<String>>,
}

pub(in crate::native) fn terminal_parent_links(
    blocks: &[BodyBlock],
    forest: &LoopForest,
) -> TerminalParentLinks {
    let mut links = TerminalParentLinks::default();
    for block in blocks {
        let Some(terminal) = terminal_exit_continuation(blocks, forest, &block.name) else {
            continue;
        };
        links
            .child_by_parent
            .insert(block.name.clone(), terminal.continuation.clone());
        links
            .parents_by_child
            .entry(terminal.continuation)
            .or_default()
            .push(block.name.clone());
    }
    for parents in links.parents_by_child.values_mut() {
        parents.sort();
    }
    links
}

pub(in crate::native) fn compose_terminal_parent_nested_merge(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    parent: &str,
    counter: &mut usize,
) -> bool {
    let forest = analyze(blocks);
    let Some(terminal) = terminal_exit_continuation(blocks, &forest, parent) else {
        return false;
    };
    let child = terminal.continuation;
    let Some(child_merge) = header_merges.get(&child).cloned() else {
        return false;
    };
    if child == parent || !forest.dominates(parent, &child_merge) {
        return false;
    }
    let Some(merge_block) = blocks.iter().find(|block| block.name == child_merge) else {
        return false;
    };
    if merge_block.role != BlockRole::LMerge {
        return false;
    }
    let successors = block_successors(merge_block);
    let [successor] = successors.as_slice() else {
        return false;
    };
    if header_merges.get(parent) == Some(successor) {
        return false;
    }
    let successor = successor.clone();
    let private = format!("{SPLIT_PREFIX}{SEL_TOKEN}{counter}");
    *counter += 1;
    let Some(child) = blocks.iter_mut().find(|block| block.name == child_merge) else {
        return false;
    };
    let Some(typed) = child.typed_mut() else {
        return false;
    };
    typed.redirect_successor(&successor, &private);
    if let Some(target) = blocks.iter_mut().find(|block| block.name == successor) {
        if let Some(typed) = target.typed_mut() {
            typed.rewrite_phi_predecessor(&child_merge, &private);
        }
    }
    let insert_at = blocks
        .iter()
        .position(|block| block.name == successor)
        .unwrap_or(blocks.len());
    blocks.insert(
        insert_at,
        synthetic_block(
            private.clone(),
            vec![format!("br label {successor}")],
            BlockRole::LMerge,
        ),
    );
    if crate::env_vars::spi_why() {
        eprintln!(
            "[spi-why]   terminal-parent-compose header={} child_merge={} merge={} successor={}",
            parent, child_merge, private, successor,
        );
    }
    header_merges.insert(parent.to_string(), private);
    true
}

pub(in crate::native) fn complete_terminal_parent_ownership(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    links: &TerminalParentLinks,
    owner: &str,
    counter: &mut usize,
) -> Vec<String> {
    let mut pending = vec![owner.to_string()];
    let mut visited = HashSet::default();
    let mut completed = Vec::new();
    while let Some(child) = pending.pop() {
        if !visited.insert(child.clone()) {
            continue;
        }
        if links
            .child_by_parent
            .get(&child)
            .is_some_and(|owned_child| header_merges.contains_key(owned_child))
        {
            compose_terminal_parent_nested_merge(blocks, header_merges, &child, counter);
        }
        if let Some(merge) = header_merges.get(&child).cloned() {
            privatize_direct_arm_terminal_return(blocks, &child, &merge, counter);
        }
        completed.push(child.clone());
        pending.extend(
            links
                .parents_by_child
                .get(&child)
                .into_iter()
                .flatten()
                .filter(|parent| header_merges.contains_key(*parent))
                .rev()
                .cloned(),
        );
    }
    completed
}

#[cfg(test)]
pub(in crate::native) fn compose_terminal_parent_nested_merges(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    counter: &mut usize,
) -> bool {
    let links = terminal_parent_links(blocks, &analyze(blocks));
    let parents = header_merges.keys().cloned().collect::<Vec<_>>();
    let before = blocks.len();
    for parent in parents {
        complete_terminal_parent_ownership(blocks, header_merges, &links, &parent, counter);
    }
    blocks.len() != before
}

#[cfg(test)]
pub(in crate::native) fn finalize_direct_terminal_guard_merges(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    counter: &mut usize,
) -> bool {
    let mut changed = false;
    loop {
        let forest = analyze(blocks);
        let loop_headers = forest
            .loops
            .iter()
            .map(|loop_info| loop_info.header.as_str())
            .collect::<HashSet<_>>();
        let depth = |name: &str| {
            let mut depth = 0usize;
            let mut current = name;
            while let Some(parent) = forest.idom(current) {
                depth += 1;
                current = parent;
            }
            depth
        };
        let mut headers = header_merges.keys().cloned().collect::<Vec<_>>();
        headers.sort_by(|left, right| depth(right).cmp(&depth(left)).then_with(|| left.cmp(right)));

        let mut applied = false;
        for header in headers {
            if loop_headers.contains(header.as_str()) {
                continue;
            }
            let Some(terminal) = terminal_exit_continuation(blocks, &forest, &header) else {
                continue;
            };
            let current_is_local = header_merges.get(&header).is_some_and(|merge| {
                let header_targets_merge = blocks
                    .iter()
                    .find(|block| block.name == header)
                    .is_some_and(|block| block_successors(block).contains(merge));
                let merge_reaches_continuation = blocks
                    .iter()
                    .find(|block| block.name == *merge)
                    .is_some_and(|block| {
                        block_successors(block) == [terminal.continuation.clone()]
                    });
                header_targets_merge && merge_reaches_continuation
            });
            if header_merges.get(&header) == Some(&terminal.continuation) || current_is_local {
                continue;
            }
            let Some(private) = synth_terminal_selection_merge(
                blocks,
                &forest,
                &header,
                &terminal.continuation,
                counter,
            ) else {
                continue;
            };
            if crate::env_vars::spi_why() {
                eprintln!(
                    "[spi-why]   terminal-direct-finalize header={} continuation={} merge={}",
                    header, terminal.continuation, private,
                );
            }
            header_merges.insert(header, private);
            changed = true;
            applied = true;
            break;
        }
        if !applied {
            break;
        }
    }
    changed
}

pub(in crate::native) fn finalize_fully_terminal_switch(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    header: &str,
    counter: &mut usize,
) -> bool {
    #[derive(Clone, Copy)]
    enum TerminalKind {
        Return,
        Unreachable,
    }

    let Some(header_block) = blocks.iter().find(|block| block.name == header) else {
        return false;
    };
    if !is_switch_block(header_block) {
        return false;
    }
    let forest = analyze(blocks);
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let header_block = by_name[header];
    let terminal_chain = |start: &str| {
        let mut current = start.to_string();
        let mut seen = HashSet::default();
        while seen.insert(current.clone()) {
            let block = *by_name.get(current.as_str())?;
            if is_bare_ret_void(block) {
                return Some(TerminalKind::Return);
            }
            if is_bare_unreachable(block) {
                return Some(TerminalKind::Unreachable);
            }
            if block.role != BlockRole::LMerge
                || block
                    .typed
                    .as_ref()
                    .is_none_or(|typed| !typed.insts.is_empty())
            {
                return None;
            }
            let successors = block_successors(block);
            let [successor] = successors.as_slice() else {
                return None;
            };
            current = successor.clone();
        }
        None
    };
    let current_is_terminal_merge = header_merges
        .get(header)
        .and_then(|merge| by_name.get(merge.as_str()))
        .is_some_and(|block| is_bare_unreachable(block));
    let current_is_owned_reconvergence = header_merges.get(header).is_some_and(|merge| {
        !current_is_terminal_merge
            && merge != header
            && by_name.contains_key(merge.as_str())
            && forest.dominates(header, merge)
    });
    if current_is_owned_reconvergence {
        return false;
    }
    let mut seen = HashSet::default();
    let mut stack = block_successors(header_block)
        .into_iter()
        .map(|target| (header.to_string(), target))
        .collect::<Vec<_>>();
    let mut redirects = Vec::<(String, String, TerminalKind)>::new();
    let mut valid = true;
    while let Some((predecessor, node)) = stack.pop() {
        let Some(block) = by_name.get(node.as_str()) else {
            valid = false;
            break;
        };
        if is_bare_ret_void(block) || is_bare_unreachable(block) {
            continue;
        }
        if predecessor != header && block.role == BlockRole::LMerge {
            if let Some(kind) = terminal_chain(&node) {
                let Some(predecessor_block) = by_name.get(predecessor.as_str()) else {
                    valid = false;
                    break;
                };
                if block_successors(predecessor_block) != [node.clone()] {
                    valid = false;
                    break;
                }
                redirects.push((predecessor, node, kind));
                continue;
            }
        }
        if !forest.dominates(header, &node) {
            let Some(kind) = terminal_chain(&node) else {
                valid = false;
                break;
            };
            let Some(predecessor_block) = by_name.get(predecessor.as_str()) else {
                valid = false;
                break;
            };
            if block_successors(predecessor_block) != [node.clone()] {
                valid = false;
                break;
            }
            redirects.push((predecessor, node, kind));
            continue;
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        let successors = block_successors(block);
        if successors.is_empty() {
            valid = false;
            break;
        }
        stack.extend(
            successors
                .into_iter()
                .map(|successor| (node.clone(), successor)),
        );
    }
    if !valid || redirects.is_empty() && current_is_terminal_merge {
        return false;
    }
    for (predecessor, old_target, kind) in redirects {
        let Some(block) = blocks.iter_mut().find(|block| block.name == predecessor) else {
            continue;
        };
        if !block_successors(block)
            .iter()
            .any(|successor| successor == &old_target)
        {
            continue;
        }
        if let Some(typed) = block.typed_mut() {
            typed.set_terminator_line(match kind {
                TerminalKind::Return => "ret void",
                TerminalKind::Unreachable => "unreachable",
            });
        }
    }
    let names = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    let merge = loop {
        let candidate = format!("{SPLIT_PREFIX}{SEL_TOKEN}{counter}");
        *counter += 1;
        if !names.contains(candidate.as_str()) {
            break candidate;
        }
    };
    blocks.push(synthetic_block(
        merge.clone(),
        vec!["unreachable".to_string()],
        BlockRole::LMerge,
    ));
    if crate::env_vars::spi_why() {
        eprintln!(
            "[spi-why]   terminal-switch-finalize header={} merge={}",
            header, merge,
        );
    }
    header_merges.insert(header.to_string(), merge);
    true
}

#[cfg(test)]
pub(in crate::native) fn finalize_fully_terminal_switches(
    blocks: &mut Vec<BodyBlock>,
    header_merges: &mut HashMap<String, String>,
    counter: &mut usize,
) -> bool {
    let headers = header_merges.keys().cloned().collect::<Vec<_>>();
    let mut changed = false;
    for header in headers {
        changed |= finalize_fully_terminal_switch(blocks, header_merges, &header, counter);
    }
    changed
}

pub(in crate::native) fn privatize_direct_arm_terminal_return(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    merge: &str,
    counter: &mut usize,
) -> bool {
    let Some(block) = blocks.iter().find(|block| block.name == header) else {
        return false;
    };
    let Some((left, right)) = conditional_branch_targets(block) else {
        return false;
    };
    let terminal_arm = match (left == merge, right == merge) {
        (true, false) => right,
        (false, true) => left,
        _ => return false,
    };
    let forest = analyze(blocks);
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut region = HashSet::default();
    let mut returns = HashSet::default();
    let mut stack = vec![terminal_arm.clone()];
    let mut valid = true;
    while let Some(node) = stack.pop() {
        let Some(block) = by_name.get(node.as_str()) else {
            valid = false;
            break;
        };
        if is_bare_ret_void(block) {
            returns.insert(node);
            continue;
        }
        if is_bare_unreachable(block) {
            continue;
        }
        if !forest.dominates(header, &node) || !region.insert(node.clone()) {
            valid &= forest.dominates(header, &node);
            continue;
        }
        let successors = block_successors(block);
        if successors.is_empty() {
            valid = false;
            break;
        }
        stack.extend(successors);
    }
    if !valid || returns.is_empty() {
        return false;
    }

    let mut redirects = Vec::<(String, String)>::new();
    for return_target in &returns {
        let mut owned = Vec::new();
        let mut outside = false;
        for block in blocks.iter().filter(|block| {
            block_successors(block)
                .iter()
                .any(|successor| successor == return_target)
        }) {
            if region.contains(&block.name)
                || (block.name == header && return_target == &terminal_arm)
            {
                owned.push(block.name.clone());
            } else {
                outside = true;
            }
        }
        if outside {
            redirects.extend(
                owned
                    .into_iter()
                    .map(|predecessor| (predecessor, return_target.clone())),
            );
        }
    }
    if redirects.is_empty() {
        return false;
    }

    let names = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    let private_return = loop {
        let candidate = format!("{SPLIT_PREFIX}{TEXITRET_TOKEN}{counter}");
        *counter += 1;
        if !names.contains(candidate.as_str()) {
            break candidate;
        }
    };
    for (predecessor, return_target) in &redirects {
        let Some(block) = blocks.iter_mut().find(|block| block.name == *predecessor) else {
            continue;
        };
        let Some(typed) = block.typed_mut() else {
            continue;
        };
        typed.redirect_successor(return_target, &private_return);
    }
    blocks.push(synthetic_block(
        private_return,
        vec!["ret void".to_string()],
        BlockRole::TerminalExitReturn,
    ));
    true
}

fn terminal_owned_arm_continuation(
    blocks: &[BodyBlock],
    terminal_merges: &HashMap<String, String>,
    header: &str,
) -> Option<String> {
    let block = blocks.iter().find(|block| block.name == header)?;
    let (left, right) = conditional_branch_targets(block)?;
    for (continuation, terminal_arm) in [(&left, &right), (&right, &left)] {
        let Some(terminal_merge) = terminal_merges.get(terminal_arm) else {
            continue;
        };
        let Some(merge_block) = blocks.iter().find(|block| block.name == *terminal_merge) else {
            continue;
        };
        if merge_block.role == BlockRole::TerminalExitReturn && is_bare_ret_void(merge_block) {
            return Some(continuation.clone());
        }
    }
    None
}

pub(in crate::native) fn synth_terminal_selection_merge(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    continuation: &str,
    counter: &mut usize,
) -> Option<String> {
    if block_has_phi(blocks, continuation) {
        synth_unique_selection_merge_phi(blocks, forest, header, continuation, counter)
    } else {
        synth_unique_selection_merge(blocks, forest, header, continuation, counter)
    }
}

pub(in crate::native) fn terminal_unreachable_selection_merges(
    blocks: &[BodyBlock],
) -> Option<TerminalExitSelectionPlan> {
    if blocks.len() > TERMINAL_EXIT_SELECTION_MAX_BLOCKS {
        return None;
    }
    let mut out = blocks.to_vec();
    let mut merges = HashMap::default();
    let mut counter = TERMINAL_EXIT_COUNTER_START;
    let mut changed = false;
    loop {
        let forest = analyze(&out);
        let loop_nodes: HashSet<&str> = forest
            .loops
            .iter()
            .flat_map(|loop_info| loop_info.body.iter().map(String::as_str))
            .collect();
        let depth = |name: &str| {
            let mut depth = 0usize;
            let mut current = name;
            while let Some(parent) = forest.idom(current) {
                depth += 1;
                current = parent;
            }
            depth
        };
        let mut headers: Vec<String> = out
            .iter()
            .filter(|block| !merges.contains_key(&block.name))
            .filter(|block| !loop_nodes.contains(block.name.as_str()))
            .filter(|block| conditional_branch_targets(block).is_some())
            .map(|block| block.name.clone())
            .collect();
        headers.sort_by(|left, right| depth(right).cmp(&depth(left)).then(left.cmp(right)));

        let mut applied = false;
        for header in headers {
            let Some((merge, exit_arm, return_arm)) =
                terminal_unreachable_selection(&out, &forest, &header)
            else {
                continue;
            };
            let mut candidate = out.clone();
            let mut candidate_counter = counter;
            if synth_terminal_return_clone(
                &mut candidate,
                &header,
                &exit_arm,
                &return_arm,
                &mut candidate_counter,
            )
            .is_none()
            {
                continue;
            }
            out = candidate;
            counter = candidate_counter;
            merges.insert(header, merge);
            changed = true;
            applied = true;
            break;
        }
        if !applied {
            break;
        }
    }
    changed.then_some(TerminalExitSelectionPlan {
        blocks: out,
        merges,
    })
}

pub(in crate::native) struct TerminalExitContinuation {
    pub(in crate::native) continuation: String,
    pub(in crate::native) exit_arm: String,
    pub(in crate::native) return_arm: TerminalReturnArm,
}

pub(in crate::native) struct TerminalReturnArm {
    pub(in crate::native) return_block: String,
    pub(in crate::native) predecessor: Option<String>,
}

pub(in crate::native) fn terminal_exit_continuation(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
) -> Option<TerminalExitContinuation> {
    let block = blocks.iter().find(|block| block.name == header)?;
    let (left, right) = conditional_branch_targets(block)?;
    let left_exit = terminal_exit_arm(blocks, forest, header, &left);
    let right_exit = terminal_exit_arm(blocks, forest, header, &right);
    match (left_exit, right_exit) {
        (None, Some(return_arm)) => Some(TerminalExitContinuation {
            continuation: left,
            exit_arm: right,
            return_arm,
        }),
        (Some(return_arm), None) => Some(TerminalExitContinuation {
            continuation: right,
            exit_arm: left,
            return_arm,
        }),
        _ => None,
    }
}

pub(in crate::native) fn terminal_unreachable_selection(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
) -> Option<(String, String, TerminalReturnArm)> {
    let block = blocks.iter().find(|block| block.name == header)?;
    let (left, right) = conditional_branch_targets(block)?;
    let left_return = terminal_exit_arm(blocks, forest, header, &left);
    let right_return = terminal_exit_arm(blocks, forest, header, &right);
    let left_unreachable = block_ends_in_unreachable(blocks, &left);
    let right_unreachable = block_ends_in_unreachable(blocks, &right);
    let (merge, exit_arm, return_arm) = match (
        left_return,
        right_return,
        left_unreachable,
        right_unreachable,
    ) {
        (Some(return_arm), None, false, true) => (right, left, return_arm),
        (None, Some(return_arm), true, false) => (left, right, return_arm),
        _ => return None,
    };
    forest
        .dominates(header, &merge)
        .then_some((merge, exit_arm, return_arm))
}

pub(in crate::native) fn block_ends_in_unreachable(blocks: &[BodyBlock], name: &str) -> bool {
    blocks
        .iter()
        .find(|block| block.name == name)
        .is_some_and(is_bare_unreachable)
}

fn is_bare_ret_void(block: &BodyBlock) -> bool {
    block
        .typed
        .as_ref()
        .is_some_and(|t| t.insts.is_empty() && matches!(t.ret, crate::native::tir::RetEmit::Void))
}

pub(in crate::native) fn is_bare_unreachable(block: &BodyBlock) -> bool {
    block.typed.as_ref().is_some_and(|t| {
        t.insts.is_empty() && matches!(t.terminator, crate::native::tir::TirTerminator::Unreachable)
    })
}

pub(in crate::native) fn terminal_exit_arm(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
    arm: &str,
) -> Option<TerminalReturnArm> {
    if blocks
        .iter()
        .find(|block| block.name == arm)
        .is_some_and(is_bare_ret_void)
    {
        return Some(TerminalReturnArm {
            return_block: arm.to_string(),
            predecessor: None,
        });
    }
    if !forest.dominates(header, arm) {
        return None;
    }
    let mut current = arm.to_string();
    let mut seen = HashSet::default();
    while seen.insert(current.clone()) {
        let block = blocks.iter().find(|block| block.name == current)?;
        if is_bare_ret_void(block) {
            return Some(TerminalReturnArm {
                return_block: current,
                predecessor: None,
            });
        }
        let successors = block_successors(block);
        if successors.len() != 1 {
            return None;
        }
        let next = successors.into_iter().next()?;
        if blocks
            .iter()
            .find(|block| block.name == next)
            .is_some_and(is_bare_ret_void)
        {
            return Some(TerminalReturnArm {
                return_block: next,
                predecessor: Some(current),
            });
        }
        if !forest.dominates(header, &next) {
            return None;
        }
        current = next;
    }
    None
}

pub(in crate::native) fn synth_terminal_return_clone(
    blocks: &mut Vec<BodyBlock>,
    header: &str,
    exit_arm: &str,
    return_arm: &TerminalReturnArm,
    counter: &mut usize,
) -> Option<String> {
    if !blocks
        .iter()
        .find(|block| block.name == return_arm.return_block)
        .is_some_and(is_bare_ret_void)
    {
        return None;
    }
    let (redirect_block, redirect_from) = match &return_arm.predecessor {
        Some(predecessor) => (predecessor.as_str(), return_arm.return_block.as_str()),
        None => (header, exit_arm),
    };
    let redirect_index = blocks
        .iter()
        .position(|block| block.name == redirect_block)?;
    if !block_successors(blocks.get(redirect_index)?)
        .iter()
        .any(|s| s == redirect_from)
    {
        return None;
    }

    let names: HashSet<&str> = blocks.iter().map(|block| block.name.as_str()).collect();
    let mut id = *counter;
    let return_name = loop {
        let candidate = format!("{SPLIT_PREFIX}{TEXITRET_TOKEN}{id}");
        if !names.contains(candidate.as_str()) {
            break candidate;
        }
        id += 1;
    };
    *counter = id + 1;
    blocks
        .get_mut(redirect_index)?
        .typed_mut()?
        .redirect_successor(redirect_from, &return_name);
    blocks.push(synthetic_block(
        return_name.clone(),
        vec!["ret void".to_string()],
        BlockRole::TerminalExitReturn,
    ));
    Some(return_name)
}

pub(in crate::native) fn synth_shared_void_return_selection_merge(
    blocks: &mut Vec<BodyBlock>,
    forest: &LoopForest,
    header: &str,
    counter: &mut usize,
) -> Option<String> {
    let header_block = blocks.iter().find(|block| block.name == header)?;
    let (left, right) = conditional_branch_targets(header_block)?;
    let left_returns = arm_void_return_targets(blocks, forest, header, &left)?;
    let right_returns = arm_void_return_targets(blocks, forest, header, &right)?;
    if left_returns.is_empty() || right_returns.is_empty() {
        return None;
    }
    let return_targets = left_returns
        .into_iter()
        .chain(right_returns)
        .collect::<HashSet<_>>();

    let names = blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect::<HashSet<_>>();
    let mut id = *counter;
    let private_merge = loop {
        let candidate = format!("{SPLIT_PREFIX}{SEL_TOKEN}{id}");
        if !names.contains(candidate.as_str()) {
            break candidate;
        }
        id += 1;
    };
    *counter = id + 1;
    blocks.push(synthetic_block(
        private_merge.clone(),
        vec!["unreachable".to_string()],
        BlockRole::LMerge,
    ));
    if crate::env_vars::spi_why() {
        eprintln!(
            "[spi-why]   terminal-return-merge header={} returns={:?} merge={}",
            header, return_targets, private_merge,
        );
    }
    Some(private_merge)
}

pub(in crate::native) fn fully_terminal_void_return_selection(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
) -> bool {
    let Some((left, right)) = blocks
        .iter()
        .find(|block| block.name == header)
        .and_then(conditional_branch_targets)
    else {
        return false;
    };
    [left, right].into_iter().all(|arm| {
        arm_void_return_targets(blocks, forest, header, &arm)
            .is_some_and(|returns| !returns.is_empty())
    })
}

fn arm_void_return_targets(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
    arm: &str,
) -> Option<Vec<String>> {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut returns = HashSet::default();
    let mut seen = HashSet::default();
    let mut stack = vec![arm.to_string()];
    while let Some(node) = stack.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        let block = by_name.get(node.as_str())?;
        if is_bare_ret_void(block) {
            returns.insert(node);
            continue;
        }
        if is_bare_unreachable(block) {
            continue;
        }
        if !forest.dominates(header, &node) {
            return None;
        }
        let successors = block_successors(block);
        if successors.is_empty() {
            return None;
        }
        stack.extend(successors);
    }
    let mut returns = returns.into_iter().collect::<Vec<_>>();
    returns.sort();
    Some(returns)
}

pub(in crate::native) fn terminal_exit_bridge_merge(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    terminal_merges: &HashMap<String, String>,
    header: &str,
) -> Option<String> {
    let block = blocks.iter().find(|block| block.name == header)?;
    let (left, right) = conditional_branch_targets(block)?;
    for (continuation, terminal_arm) in [(&left, &right), (&right, &left)] {
        let Some(terminal_merge) = terminal_merges.get(terminal_arm) else {
            continue;
        };
        let Some(merge_block) = blocks.iter().find(|block| block.name == *terminal_merge) else {
            continue;
        };
        let successors = block_successors(merge_block);
        if forest.dominates(header, continuation)
            && successors.len() == 1
            && successors.first() == Some(continuation)
        {
            return Some(continuation.clone());
        }
    }
    None
}

pub(in crate::native) fn terminal_exit_convergence(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    header: &str,
) -> Option<String> {
    let header_block = blocks.iter().find(|block| block.name == header)?;
    let arms = block_successors(header_block);
    if arms.len() < 2 {
        return None;
    }
    let loop_nodes: HashSet<&str> = forest
        .loops
        .iter()
        .flat_map(|loop_info| loop_info.body.iter().map(String::as_str))
        .collect();
    if loop_nodes.contains(header) {
        return None;
    }
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut predecessors = HashMap::<String, Vec<&str>>::default();
    for block in blocks {
        for successor in block_successors(block) {
            predecessors
                .entry(successor)
                .or_default()
                .push(block.name.as_str());
        }
    }
    let terminal = |name: &str| {
        by_name
            .get(name)
            .is_some_and(|block| is_bare_ret_void(block) || is_bare_unreachable(block))
    };
    let candidate_depth = |name: &str| {
        let mut depth = 0usize;
        let mut current = name;
        while let Some(parent) = forest.idom(current) {
            depth += 1;
            current = parent;
        }
        depth
    };

    let reachable = |arm: &str| -> Option<(HashSet<String>, bool)> {
        let mut nodes = HashSet::default();
        let mut saw_terminal = false;
        let mut stack = vec![arm.to_string()];
        while let Some(node) = stack.pop() {
            if terminal(&node) {
                saw_terminal = true;
                continue;
            }
            if !forest.dominates(header, &node) {
                nodes.insert(node);
                continue;
            }
            if !nodes.insert(node.clone()) {
                continue;
            }
            let successors = block_successors(by_name.get(node.as_str())?);
            if successors.is_empty() {
                return None;
            }
            stack.extend(successors);
        }
        Some((nodes, saw_terminal))
    };
    let arm_regions = arms
        .iter()
        .map(|arm| reachable(arm))
        .collect::<Option<Vec<_>>>()?;
    let mut coverage = HashMap::<&str, usize>::default();
    for (region, _) in &arm_regions {
        for node in region {
            *coverage.entry(node.as_str()).or_default() += 1;
        }
    }
    let mut candidates: Vec<(&str, usize, usize)> = coverage
        .into_iter()
        .filter_map(|(name, arm_coverage)| {
            let block = by_name.get(name)?;
            (!matches!(
                block.role,
                BlockRole::LMerge | BlockRole::TerminalExitReturn
            ) && !loop_nodes.contains(name))
            .then(|| (name, arm_coverage, candidate_depth(name)))
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then(left.2.cmp(&right.2))
            .then(left.0.cmp(right.0))
    });
    if crate::env_vars::spi_why() {
        eprintln!(
            "[spi-why]   terminal-convergence-candidates header={} arms={:?} regions={:?} candidates={:?}",
            header,
            arms,
            arm_regions
                .iter()
                .map(|(nodes, terminal)| (nodes.len(), *terminal))
                .collect::<Vec<_>>(),
            candidates
                .iter()
                .take(12)
                .map(|(name, coverage, depth)| (*name, *coverage, *depth))
                .collect::<Vec<_>>(),
        );
    }

    for (candidate, _, _) in candidates {
        let mut valid = true;
        let mut owned_region = HashSet::default();
        for arm in &arms {
            let mut reached = false;
            let mut saw_terminal = false;
            let mut seen = HashSet::default();
            let mut stack = vec![arm.clone()];
            while let Some(node) = stack.pop() {
                if node == candidate {
                    reached = true;
                    continue;
                }
                if terminal(&node) {
                    saw_terminal = true;
                    continue;
                }
                if !forest.dominates(header, &node) {
                    valid = false;
                    break;
                }
                if !seen.insert(node.clone()) {
                    continue;
                }
                owned_region.insert(node.clone());
                let Some(block) = by_name.get(node.as_str()) else {
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
            if !valid || (!reached && !saw_terminal) {
                valid = false;
                break;
            }
        }
        if valid {
            valid = owned_region.iter().all(|node| {
                predecessors.get(node.as_str()).is_none_or(|incoming| {
                    incoming.iter().all(|predecessor| {
                        *predecessor == header || owned_region.contains(*predecessor)
                    })
                })
            });
        }
        if valid {
            return Some(candidate.to_string());
        }
    }
    None
}
