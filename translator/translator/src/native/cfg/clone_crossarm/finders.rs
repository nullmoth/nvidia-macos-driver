use super::*;

pub(in crate::native) fn find_deep_shared_continuations(
    blocks: &[BodyBlock],
) -> Vec<(String, String)> {
    let forest = analyze(blocks);
    let selection = selection_merges(blocks, &forest);
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();

    let depth = |name: &str| {
        let mut depth = 0usize;
        let mut cur = name;
        while let Some(parent) = forest.idom(cur) {
            depth += 1;
            cur = parent;
        }
        depth
    };
    let mut headers: Vec<&BodyBlock> = blocks
        .iter()
        .filter(|b| {
            !loop_headers.contains(b.name.as_str())
                && conditional_branch_targets(b).is_some()
                && selection.contains_key(&b.name)
        })
        .collect();
    headers.sort_by_cached_key(|b| std::cmp::Reverse(depth(&b.name)));

    let mut found = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::default();
    for header in headers {
        let Some(natural) = selection.get(&header.name) else {
            continue;
        };
        if forest.dominates(&header.name, natural) {
            continue;
        }
        let reaches_natural = reverse_reachable(blocks, natural, &names);
        for block in blocks {
            if !forest.dominates(&header.name, &block.name) {
                continue;
            }
            for continuation in block_successors(block) {
                if continuation == *natural
                    || !names.contains(continuation.as_str())
                    || forest.dominates(&header.name, &continuation)
                    || !reaches_natural.contains(&continuation)
                {
                    continue;
                }
                let pair = (header.name.clone(), continuation);
                if seen.insert(pair.clone()) {
                    found.push(pair);
                }
            }
        }
    }
    found
}

pub(in crate::native) fn find_switch_case_shared_continuations(
    blocks: &[BodyBlock],
) -> Vec<(String, String)> {
    let forest = analyze(blocks);
    let selection = selection_merges(blocks, &forest);
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let loop_latches: HashSet<&str> = forest
        .loops
        .iter()
        .flat_map(|l| l.latches.iter().map(String::as_str))
        .collect();
    let loop_exits: HashSet<&str> = forest
        .loops
        .iter()
        .flat_map(|l| l.exits.iter().map(String::as_str))
        .collect();

    let depth = |name: &str| {
        let mut depth = 0usize;
        let mut cur = name;
        while let Some(parent) = forest.idom(cur) {
            depth += 1;
            cur = parent;
        }
        depth
    };
    let mut switches: Vec<&BodyBlock> = blocks
        .iter()
        .filter(|b| {
            !loop_headers.contains(b.name.as_str())
                && crate::native::cfg::structured_emit::is_switch_block(b)
                && selection.contains_key(&b.name)
        })
        .collect();
    switches.sort_by_cached_key(|b| std::cmp::Reverse(depth(&b.name)));

    let mut found = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::default();
    for switch in switches {
        let Some(natural) = selection.get(&switch.name) else {
            continue;
        };
        let adjacent_cases = match &switch.typed.as_ref().expect("typed switch").terminator {
            crate::native::tir::TirTerminator::Switch { cases, .. } => {
                let mut ordered = Vec::new();
                let mut unique = HashSet::default();
                for (_, target) in cases {
                    if unique.insert(target.as_str()) {
                        ordered.push(target.as_str());
                    }
                }
                ordered
                    .windows(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect::<HashSet<_>>()
            }
            _ => unreachable!("switch filter guarantees a switch terminator"),
        };
        let roots = block_successors(switch);
        let reaches_natural = reverse_reachable(blocks, natural, &names);
        for case_root in &roots {
            if case_root == natural
                || !names.contains(case_root.as_str())
                || !forest.dominates(&switch.name, case_root)
            {
                continue;
            }
            for block in blocks {
                if !forest.dominates(case_root, &block.name) {
                    continue;
                }
                for continuation in block_successors(block) {
                    let is_legal_adjacent_fallthrough = adjacent_cases
                        .contains(&(case_root.as_str(), continuation.as_str()))
                        && matches!(
                            block.typed.as_ref().map(|typed| &typed.terminator),
                            Some(crate::native::tir::TirTerminator::Br(target))
                                if target == &continuation
                        );
                    if continuation == *natural
                        || is_legal_adjacent_fallthrough
                        || !names.contains(continuation.as_str())
                        || forest.dominates(case_root, &continuation)
                        || !reaches_natural.contains(&continuation)
                        || !shared_clone_is_loop_local(
                            blocks,
                            &forest,
                            case_root,
                            &continuation,
                            &loop_headers,
                            &loop_latches,
                            &loop_exits,
                        )
                    {
                        continue;
                    }
                    let pair = (case_root.clone(), continuation);
                    if seen.insert(pair.clone()) {
                        found.push(pair);
                    }
                }
            }
        }
    }
    found
}

pub(in crate::native) fn shared_clone_is_loop_local(
    blocks: &[BodyBlock],
    forest: &crate::native::cfg::loopforest::LoopForest,
    owner: &str,
    continuation: &str,
    loop_headers: &HashSet<&str>,
    loop_latches: &HashSet<&str>,
    loop_exits: &HashSet<&str>,
) -> bool {
    let membership = |name: &str| {
        forest
            .loops
            .iter()
            .filter(|natural_loop| natural_loop.body.iter().any(|node| node == name))
            .map(|natural_loop| natural_loop.header.as_str())
            .collect::<HashSet<_>>()
    };
    let owner_membership = membership(owner);
    if membership(continuation) != owner_membership {
        return false;
    }
    if blocks.iter().any(|block| {
        forest.dominates(owner, &block.name)
            && block_successors(block)
                .iter()
                .any(|successor| successor == continuation)
            && membership(&block.name) != owner_membership
    }) {
        return false;
    }

    let names: HashSet<&str> = blocks.iter().map(|block| block.name.as_str()).collect();
    let mut region = HashSet::default();
    let mut stack = vec![continuation.to_string()];
    while let Some(name) = stack.pop() {
        if !region.insert(name.clone()) {
            continue;
        }
        if membership(&name) != owner_membership
            || loop_headers.contains(name.as_str())
            || loop_latches.contains(name.as_str())
            || loop_exits.contains(name.as_str())
        {
            return false;
        }
        let Some(block) = blocks.iter().find(|block| block.name == name) else {
            continue;
        };
        for successor in block_successors(block) {
            if !names.contains(successor.as_str()) {
                continue;
            }
            if forest.dominates(continuation, &successor) {
                stack.push(successor);
            } else if membership(&successor) != owner_membership
                || loop_headers.contains(successor.as_str())
                || loop_latches.contains(successor.as_str())
                || loop_exits.contains(successor.as_str())
            {
                return false;
            }
        }
    }
    true
}

pub(in crate::native) fn reverse_reachable(
    blocks: &[BodyBlock],
    target: &str,
    names: &HashSet<&str>,
) -> HashSet<String> {
    let preds = predecessors(blocks);
    let mut seen = HashSet::default();
    let mut stack = vec![target.to_string()];
    while let Some(block) = stack.pop() {
        if !seen.insert(block.clone()) {
            continue;
        }
        for pred in preds.get(&block).into_iter().flatten() {
            if names.contains(pred.as_str()) && !seen.contains(pred) {
                stack.push(pred.clone());
            }
        }
    }
    seen
}

pub(in crate::native) fn find_cross_arm_edge(blocks: &[BodyBlock]) -> Option<(String, String)> {
    let forest = analyze(blocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let loop_latches: HashSet<&str> = forest
        .loops
        .iter()
        .flat_map(|natural_loop| natural_loop.latches.iter().map(String::as_str))
        .collect();
    let loop_exits: HashSet<&str> = forest
        .loops
        .iter()
        .flat_map(|natural_loop| natural_loop.exits.iter().map(String::as_str))
        .collect();
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let targets: HashMap<&str, (String, String)> = blocks
        .iter()
        .filter(|h| !loop_headers.contains(h.name.as_str()))
        .filter_map(|h| {
            let (t0, t1) = conditional_branch_targets(h)?;
            if t0 == t1 || !names.contains(t0.as_str()) || !names.contains(t1.as_str()) {
                return None;
            }
            Some((h.name.as_str(), (t0, t1)))
        })
        .collect();
    for b in blocks {
        for s in block_successors(b) {
            if !names.contains(s.as_str())
                || loop_headers.contains(s.as_str())
                || forest.dominates(&b.name, &s)
            {
                continue;
            }
            let mut child: &str = &b.name;
            while let Some(cur) = forest.idom(child) {
                if let Some((x, y)) = targets.get(cur) {
                    let sibling = if child == x {
                        Some(y)
                    } else if child == y {
                        Some(x)
                    } else {
                        None
                    };
                    if let Some(sib) = sibling {
                        if forest.dominates(sib, &s)
                            && shared_clone_is_loop_local(
                                blocks,
                                &forest,
                                child,
                                &s,
                                &loop_headers,
                                &loop_latches,
                                &loop_exits,
                            )
                        {
                            return Some((child.to_string(), s));
                        }
                    }
                }
                child = cur;
            }
        }
    }
    None
}
