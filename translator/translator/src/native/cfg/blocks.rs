use super::super::ir::{LlFunction, LlType, LlValue};
use super::super::parse::{strip_comment, LlSwitch};
use super::graph::{reachable_from, Cfg};
use super::loopforest::{analyze, post_idom};
use super::{BlockRole, BodyBlock, LoopMergeInfo};
use crate::fxhash::{HashMap, HashSet};

const SWITCH_BLOCK_PREFIX: &str = "%metal2vulkan_switch_";
pub(in crate::native) const SWITCH_BYPASS_PREFIX: &str = "%metal2vulkan_switch_bypass_";

#[cfg(test)]
pub(in crate::native) fn split_body_blocks<S: AsRef<str>>(
    lines: &[S],
    entry_name: String,
    named_types: &HashMap<String, LlType>,
) -> Vec<BodyBlock> {
    let mut blocks = Vec::new();
    let mut cur_name = entry_name;
    let mut cur_lines: Vec<String> = Vec::new();
    for line in lines {
        let line = line.as_ref();
        let trimmed = strip_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(label) = trimmed.strip_suffix(':') {
            if !cur_lines.is_empty() || !blocks.is_empty() {
                let typed =
                    crate::native::tir::lower_block_carrier(&cur_name, &cur_lines, named_types)
                        .map(Into::into);
                blocks.push(BodyBlock {
                    name: cur_name,
                    role: BlockRole::Normal,
                    typed,
                });
            }
            cur_name = format!("%{label}");
            cur_lines = Vec::new();
        } else {
            cur_lines.push(line.to_string());
        }
    }
    let typed =
        crate::native::tir::lower_block_carrier(&cur_name, &cur_lines, named_types).map(Into::into);
    blocks.push(BodyBlock {
        name: cur_name,
        role: BlockRole::Normal,
        typed,
    });
    blocks
}

pub(in crate::native) fn split_source_body_blocks(
    lines: &[&str],
    entry_name: String,
    named_types: &HashMap<String, LlType>,
) -> Result<Vec<BodyBlock>, String> {
    use std::borrow::Cow;

    let mut blocks = Vec::new();
    let mut cur_name = entry_name;
    let mut cur_lines: Vec<Cow<'_, str>> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let raw = lines[index];
        let trimmed = strip_comment(raw).trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        if let Some(label) = trimmed.strip_suffix(':') {
            if !cur_lines.is_empty() || !blocks.is_empty() {
                let typed =
                    crate::native::tir::lower_block_carrier(&cur_name, &cur_lines, named_types)
                        .map(Into::into);
                blocks.push(BodyBlock {
                    name: cur_name,
                    role: BlockRole::Normal,
                    typed,
                });
            }
            cur_name = format!("%{label}");
            cur_lines.clear();
            index += 1;
            continue;
        }
        if trimmed.starts_with("switch ") {
            let (switch, next) = crate::native::parse::collect_switch(lines, index)?;
            cur_lines.push(Cow::Owned(switch));
            index = next;
            continue;
        }
        cur_lines.push(Cow::Borrowed(raw));
        index += 1;
    }
    let typed =
        crate::native::tir::lower_block_carrier(&cur_name, &cur_lines, named_types).map(Into::into);
    blocks.push(BodyBlock {
        name: cur_name,
        role: BlockRole::Normal,
        typed,
    });
    Ok(blocks)
}

pub(in crate::native) fn synthetic_block(
    name: String,
    lines: Vec<String>,
    role: BlockRole,
) -> BodyBlock {
    let typed =
        crate::native::tir::lower_block_carrier(&name, &lines, &HashMap::default()).map(Into::into);
    BodyBlock { name, role, typed }
}

pub(in crate::native) fn implicit_entry_block_name(f: &LlFunction) -> String {
    let mut next_numeric = 0;
    for (name, _) in &f.params {
        if let Some(n) = name.strip_prefix('%').and_then(|s| s.parse::<usize>().ok()) {
            if n >= next_numeric {
                next_numeric = n + 1;
            }
        }
    }
    format!("%{next_numeric}")
}

struct IndexedReachability {
    index: HashMap<String, usize>,
    names: Vec<String>,
    successors: Vec<Vec<usize>>,
    unreachable: Vec<bool>,
}

impl IndexedReachability {
    fn new(blocks: &[BodyBlock], successors: &HashMap<String, Vec<String>>) -> Self {
        let names = blocks.iter().map(|block| block.name.clone()).collect();
        let index = blocks
            .iter()
            .enumerate()
            .map(|(idx, block)| (block.name.clone(), idx))
            .collect::<HashMap<_, _>>();
        let successors = blocks
            .iter()
            .map(|block| {
                successors
                    .get(&block.name)
                    .into_iter()
                    .flatten()
                    .filter_map(|successor| index.get(successor).copied())
                    .collect()
            })
            .collect();
        let unreachable = blocks
            .iter()
            .map(|block| {
                block.typed.as_ref().is_some_and(|carrier| {
                    matches!(
                        carrier.terminator,
                        crate::native::tir::TirTerminator::Unreachable
                    )
                })
            })
            .collect();
        Self {
            index,
            names,
            successors,
            unreachable,
        }
    }

    fn reachable(&self, start: &str, excluded: Option<&str>) -> Vec<bool> {
        let mut seen = vec![false; self.successors.len()];
        let Some(&start) = self.index.get(start) else {
            return seen;
        };
        let excluded = excluded.and_then(|name| self.index.get(name).copied());
        let mut pending = vec![start];
        while let Some(node) = pending.pop() {
            if Some(node) == excluded || seen[node] {
                continue;
            }
            seen[node] = true;
            pending.extend(self.successors[node].iter().copied());
        }
        seen
    }

    fn contains(&self, reachable: &[bool], name: &str) -> bool {
        self.index
            .get(name)
            .is_some_and(|index| reachable.get(*index) == Some(&true))
    }

    fn all_paths_reach(&self, start: &str, target: &str, allow_unreachable: bool) -> bool {
        let (Some(&start), Some(&target)) = (self.index.get(start), self.index.get(target)) else {
            return false;
        };
        fn walk(
            graph: &IndexedReachability,
            node: usize,
            target: usize,
            allow_unreachable: bool,
            visiting: &mut [bool],
            memo: &mut [Option<bool>],
        ) -> bool {
            if node == target || (allow_unreachable && graph.unreachable[node]) {
                return true;
            }
            if let Some(cached) = memo[node] {
                return cached;
            }
            if visiting[node] {
                return true;
            }
            visiting[node] = true;
            let ok = !graph.successors[node].is_empty()
                && graph.successors[node].iter().copied().all(|successor| {
                    walk(graph, successor, target, allow_unreachable, visiting, memo)
                });
            visiting[node] = false;
            memo[node] = Some(ok);
            ok
        }
        walk(
            self,
            start,
            target,
            allow_unreachable,
            &mut vec![false; self.successors.len()],
            &mut vec![None; self.successors.len()],
        )
    }
}

pub(in crate::native) fn infer_branch_merges(
    blocks: &[BodyBlock],
) -> HashMap<(String, String), String> {
    let by_name: HashMap<String, &BodyBlock> = blocks.iter().map(|b| (b.name.clone(), b)).collect();
    let successors: HashMap<String, Vec<String>> = blocks
        .iter()
        .map(|b| (b.name.clone(), block_successors(b)))
        .collect();
    let reachability = IndexedReachability::new(blocks, &successors);
    let switch_merges = infer_switch_merges(blocks);
    let loop_merges = infer_loop_merges(blocks);
    let mut merges = HashMap::default();
    loop {
        let mut changed = false;
        for block in blocks {
            let Some((true_label, false_label)) = conditional_branch_targets(block) else {
                continue;
            };
            let key = (true_label.clone(), false_label.clone());
            if merges.contains_key(&key) {
                continue;
            }
            let true_exit = unconditional_branch_target(by_name.get(&true_label).copied());
            let false_exit = unconditional_branch_target(by_name.get(&false_label).copied());
            let true_merge = target_structured_merge(
                &true_label,
                &by_name,
                &merges,
                &switch_merges,
                &loop_merges,
            );
            let false_merge = target_structured_merge(
                &false_label,
                &by_name,
                &merges,
                &switch_merges,
                &loop_merges,
            );
            let true_reachable = reachability.reachable(&true_label, None);
            let false_reachable = reachability.reachable(&false_label, None);
            let true_reaches_false = reachability.contains(&true_reachable, &false_label);
            let false_reaches_true = reachability.contains(&false_reachable, &true_label);
            let is_loop_continue =
                is_loop_continue_branch(&block.name, &true_label, &false_label, &loop_merges);
            let merge = match (true_exit, false_exit) {
                (Some(a), Some(b)) if a == b => Some(a),
                (Some(a), _) if a == false_label => Some(a),
                (_, Some(b)) if b == true_label => Some(b),
                (Some(a), _)
                    if false_merge.as_ref() == Some(&a)
                        && reachability.all_paths_reach(&false_label, &a, false) =>
                {
                    Some(a)
                }
                (_, Some(b))
                    if true_merge.as_ref() == Some(&b)
                        && reachability.all_paths_reach(&true_label, &b, false) =>
                {
                    Some(b)
                }
                _ if !loop_merges.contains_key(&false_label)
                    && !switch_merges.contains_key(&false_label)
                    && conditional_merge_through_shared_body(
                        &true_label,
                        &false_label,
                        &by_name,
                    )
                    .is_some_and(|candidate| {
                        reachability.all_paths_reach(&true_label, &candidate, false)
                            && reachability.all_paths_reach(&false_label, &candidate, false)
                    }) =>
                {
                    conditional_merge_through_shared_body(&true_label, &false_label, &by_name)
                }
                _ if !loop_merges.contains_key(&true_label)
                    && !switch_merges.contains_key(&true_label)
                    && conditional_merge_through_shared_body(
                        &false_label,
                        &true_label,
                        &by_name,
                    )
                    .is_some_and(|candidate| {
                        reachability.all_paths_reach(&true_label, &candidate, false)
                            && reachability.all_paths_reach(&false_label, &candidate, false)
                    }) =>
                {
                    conditional_merge_through_shared_body(&false_label, &true_label, &by_name)
                }
                _ if true_merge.as_ref().zip(false_merge.as_ref()).is_some_and(
                    |(true_merge, false_merge)| {
                        true_merge == false_merge
                            && reachability.all_paths_reach(&true_label, true_merge, false)
                            && reachability.all_paths_reach(&false_label, true_merge, false)
                    },
                ) =>
                {
                    true_merge.clone()
                }
                _ if false_reaches_true
                    && !true_reaches_false
                    && !is_loop_continue
                    && reachability.all_paths_reach(&false_label, &true_label, false) =>
                {
                    Some(true_label.clone())
                }
                _ if true_reaches_false
                    && !false_reaches_true
                    && !is_loop_continue
                    && reachability.all_paths_reach(&true_label, &false_label, false) =>
                {
                    Some(false_label.clone())
                }
                _ if switch_merges.get(&true_label) == Some(&false_label) => {
                    Some(false_label.clone())
                }
                _ if switch_merges.get(&false_label) == Some(&true_label) => {
                    Some(true_label.clone())
                }
                _ if block_is_unreachable(&false_label, &by_name) => reachable_merge_after_header(
                    &block.name,
                    &true_label,
                    blocks,
                    &reachability,
                    &true_reachable,
                ),
                _ if block_is_unreachable(&true_label, &by_name) => reachable_merge_after_header(
                    &block.name,
                    &false_label,
                    blocks,
                    &reachability,
                    &false_reachable,
                ),
                _ if loop_merges.get(&true_label).is_some_and(|info| {
                    info.merge == false_label && info.continue_target != block.name
                }) =>
                {
                    Some(false_label.clone())
                }
                _ if loop_merges.get(&false_label).is_some_and(|info| {
                    info.merge == true_label && info.continue_target != block.name
                }) =>
                {
                    Some(true_label.clone())
                }
                (_, Some(b))
                    if b == true_label
                        && block_role_is_switch_bypass(&false_label, &by_name)
                        && switch_merges.values().any(|merge| merge == &true_label) =>
                {
                    Some(false_label.clone())
                }
                (Some(a), _)
                    if a == false_label
                        && block_role_is_switch_bypass(&true_label, &by_name)
                        && switch_merges.values().any(|merge| merge == &false_label) =>
                {
                    Some(true_label.clone())
                }
                _ if !is_loop_continue => common_reachable_merge_after_header(
                    &block.name,
                    &true_label,
                    &false_label,
                    blocks,
                    &reachability,
                    &true_reachable,
                    &false_reachable,
                ),
                _ => None,
            };
            if let Some(merge) = merge {
                let branch_target_is_structured = loop_merges.contains_key(&true_label)
                    || loop_merges.contains_key(&false_label)
                    || switch_merges.contains_key(&true_label)
                    || switch_merges.contains_key(&false_label);
                if !branch_target_is_structured
                    && has_unrepairable_external_predecessor_to_branch_body(
                        &block.name,
                        &true_label,
                        &false_label,
                        &merge,
                        &successors,
                        &reachability,
                        &by_name,
                    )
                {
                    continue;
                }
                if !branch_target_is_structured
                    && branch_merge_path_has_unrepairable_external_entry(
                        &block.name,
                        &true_label,
                        &false_label,
                        &merge,
                        &successors,
                        &reachability,
                        &by_name,
                    )
                {
                    continue;
                }
                merges.insert(key, merge);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    merges
}

pub(in crate::native) fn index_branch_merges_by_header(
    blocks: &[BodyBlock],
    loop_merges: &HashMap<String, LoopMergeInfo>,
    branch_merges: &HashMap<(String, String), String>,
    branch_merges_by_header: &mut HashMap<String, String>,
) {
    for block in blocks {
        if loop_merges.contains_key(&block.name)
            || branch_merges_by_header.contains_key(&block.name)
        {
            continue;
        }
        let Some((on_true, on_false)) = conditional_branch_targets(block) else {
            continue;
        };
        if let Some(merge) = branch_merges.get(&(on_true, on_false)) {
            branch_merges_by_header.insert(block.name.clone(), merge.clone());
        }
    }
}

fn block_role_is_switch_bypass(label: &str, by_name: &HashMap<String, &BodyBlock>) -> bool {
    by_name
        .get(label)
        .is_some_and(|block| block.role == BlockRole::SwitchBypass)
}

fn is_loop_continue_branch(
    block: &str,
    true_label: &str,
    false_label: &str,
    loop_merges: &HashMap<String, LoopMergeInfo>,
) -> bool {
    loop_merges
        .get(true_label)
        .is_some_and(|info| info.merge == false_label && info.continue_target == block)
        || loop_merges
            .get(false_label)
            .is_some_and(|info| info.merge == true_label && info.continue_target == block)
}

fn conditional_merge_through_shared_body(
    body_label: &str,
    conditional_label: &str,
    by_name: &HashMap<String, &BodyBlock>,
) -> Option<String> {
    let (true_label, false_label) = conditional_branch_targets(by_name.get(conditional_label)?)?;
    if true_label == body_label {
        return Some(false_label);
    }
    if false_label == body_label {
        return Some(true_label);
    }
    None
}

fn common_reachable_merge_after_header(
    header: &str,
    true_label: &str,
    false_label: &str,
    blocks: &[BodyBlock],
    reachability: &IndexedReachability,
    true_reachable: &[bool],
    false_reachable: &[bool],
) -> Option<String> {
    let start = blocks
        .iter()
        .position(|block| block.name == header)
        .map(|idx| idx + 1)?;
    let true_without_header;
    let false_without_header;
    let true_reachable = if reachability.contains(true_reachable, header) {
        true_without_header = reachability.reachable(true_label, Some(header));
        &true_without_header
    } else {
        true_reachable
    };
    let false_reachable = if reachability.contains(false_reachable, header) {
        false_without_header = reachability.reachable(false_label, Some(header));
        &false_without_header
    } else {
        false_reachable
    };
    blocks
        .iter()
        .skip(start)
        .map(|block| &block.name)
        .find(|candidate| {
            reachability.contains(true_reachable, candidate)
                && reachability.contains(false_reachable, candidate)
                && reachability.all_paths_reach(true_label, candidate, true)
                && reachability.all_paths_reach(false_label, candidate, true)
        })
        .cloned()
}

fn reachable_merge_after_header(
    header: &str,
    label: &str,
    blocks: &[BodyBlock],
    reachability: &IndexedReachability,
    reachable: &[bool],
) -> Option<String> {
    let start = blocks
        .iter()
        .position(|block| block.name == header)
        .map(|idx| idx + 1)?;
    blocks
        .iter()
        .skip(start)
        .map(|block| &block.name)
        .find(|candidate| {
            reachability.contains(reachable, candidate)
                && reachability.all_paths_reach(label, candidate, true)
        })
        .cloned()
}

fn all_paths_reach_target(
    start: &str,
    target: &str,
    successors: &HashMap<String, Vec<String>>,
) -> bool {
    fn walk(
        node: &str,
        target: &str,
        successors: &HashMap<String, Vec<String>>,
        visiting: &mut HashSet<String>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if node == target {
            return true;
        }
        if let Some(&cached) = memo.get(node) {
            return cached;
        }
        if !visiting.insert(node.to_string()) {
            return true;
        }
        let ok = successors
            .get(node)
            .filter(|next| !next.is_empty())
            .is_some_and(|next| {
                next.iter()
                    .all(|succ| walk(succ, target, successors, visiting, memo))
            });
        visiting.remove(node);
        memo.insert(node.to_string(), ok);
        ok
    }

    walk(
        start,
        target,
        successors,
        &mut HashSet::default(),
        &mut HashMap::default(),
    )
}

fn block_is_unreachable(label: &str, by_name: &HashMap<String, &BodyBlock>) -> bool {
    by_name.get(label).is_some_and(|block| {
        block.typed.as_ref().is_some_and(|carrier| {
            matches!(
                carrier.terminator,
                crate::native::tir::TirTerminator::Unreachable
            )
        })
    })
}

fn has_unrepairable_external_predecessor_to_branch_body(
    header: &str,
    true_label: &str,
    false_label: &str,
    merge: &str,
    successors: &HashMap<String, Vec<String>>,
    reachability: &IndexedReachability,
    by_name: &HashMap<String, &BodyBlock>,
) -> bool {
    let reachable = reachability.reachable(header, None);
    [true_label, false_label]
        .into_iter()
        .filter(|target| **target != *merge)
        .any(|body| {
            let has_external_pred = successors.iter().any(|(pred, targets)| {
                pred != header
                    && pred != true_label
                    && pred != false_label
                    && !reachability.contains(&reachable, pred)
                    && targets.iter().any(|target| target == body)
            });
            has_external_pred
                && !external_entry_is_repairable_selection_target(
                    body,
                    header,
                    merge,
                    &reachable,
                    reachability,
                    successors,
                    by_name,
                )
        })
}

fn branch_merge_path_has_unrepairable_external_entry(
    header: &str,
    true_label: &str,
    false_label: &str,
    merge: &str,
    successors: &HashMap<String, Vec<String>>,
    reachability: &IndexedReachability,
    by_name: &HashMap<String, &BodyBlock>,
) -> bool {
    let header_reachable = reachability.reachable(header, None);
    let true_to_merge = reachability.reachable(true_label, Some(merge));
    let false_to_merge = reachability.reachable(false_label, Some(merge));

    reachability.names.iter().enumerate().any(|(index, node)| {
        if node == header
            || node == merge
            || (!true_to_merge[index] && !false_to_merge[index])
            || !reachability.contains(&reachability.reachable(node, None), merge)
        {
            return false;
        }
        let has_external_pred = successors.iter().any(|(pred, targets)| {
            !reachability.contains(&header_reachable, pred)
                && targets.iter().any(|target| target == node)
        });
        has_external_pred
            && !external_entry_is_repairable_selection_target(
                node,
                header,
                merge,
                &header_reachable,
                reachability,
                successors,
                by_name,
            )
    })
}

fn external_entry_is_repairable_selection_target(
    node: &str,
    header: &str,
    merge: &str,
    header_reachable: &[bool],
    reachability: &IndexedReachability,
    successors: &HashMap<String, Vec<String>>,
    by_name: &HashMap<String, &BodyBlock>,
) -> bool {
    if block_starts_with_phi(node, by_name) {
        return false;
    }
    if unconditional_branch_target(by_name.get(node).copied()) == Some(merge.to_string()) {
        return false;
    }
    std::iter::once(header)
        .chain(
            reachability
                .names
                .iter()
                .filter(|name| reachability.contains(header_reachable, name))
                .map(String::as_str),
        )
        .filter_map(|pred| successors.get(pred).map(|targets| (pred, targets)))
        .any(|(pred, targets)| {
            pred != node && targets.len() == 2 && targets.iter().any(|target| target == node)
        })
}

fn block_starts_with_phi(label: &str, by_name: &HashMap<String, &BodyBlock>) -> bool {
    by_name.get(label).is_some_and(|block| {
        block
            .typed
            .as_ref()
            .and_then(|carrier| carrier.insts.first())
            .is_some_and(|inst| inst.is_phi())
    })
}

fn target_structured_merge(
    label: &str,
    by_name: &HashMap<String, &BodyBlock>,
    branch_merges: &HashMap<(String, String), String>,
    switch_merges: &HashMap<String, String>,
    loop_merges: &HashMap<String, LoopMergeInfo>,
) -> Option<String> {
    if let Some(merge) = switch_merges.get(label) {
        return Some(merge.clone());
    }
    if let Some(info) = loop_merges.get(label) {
        return Some(info.merge.clone());
    }
    let (true_label, false_label) = conditional_branch_targets(by_name.get(label)?)?;
    branch_merges.get(&(true_label, false_label)).cloned()
}

pub(in crate::native) fn infer_loop_merges(blocks: &[BodyBlock]) -> HashMap<String, LoopMergeInfo> {
    let order: HashMap<String, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    let successors: HashMap<String, Vec<String>> = blocks
        .iter()
        .map(|b| (b.name.clone(), block_successors(b)))
        .collect();
    let mut predecessors: HashMap<String, Vec<String>> = HashMap::default();
    for (block, targets) in &successors {
        for target in targets {
            predecessors
                .entry(target.clone())
                .or_default()
                .push(block.clone());
        }
    }

    let doms = Cfg::from_blocks(blocks).map(|cfg| cfg.dominators());
    let mut merges = HashMap::default();
    for header in blocks {
        let Some(header_idx) = order.get(&header.name).copied() else {
            continue;
        };
        let header_reachable = reachable_from(&header.name, &successors);
        let back_preds = predecessors
            .get(&header.name)
            .into_iter()
            .flat_map(|preds| preds.iter())
            .filter(|pred| {
                header_reachable.contains(*pred)
                    && doms
                        .as_ref()
                        .is_some_and(|d| d.dominates(&header.name, pred))
                    && order
                        .get(*pred)
                        .copied()
                        .is_some_and(|idx| idx >= header_idx)
            })
            .cloned()
            .collect::<Vec<_>>();
        if back_preds.is_empty() {
            continue;
        }

        let loop_nodes = natural_loop_nodes(&header.name, &back_preds, &predecessors);
        let Some(continue_target) =
            infer_continue_target(&header.name, &back_preds, &loop_nodes, &successors, &order)
        else {
            continue;
        };
        let Some(merge) = infer_loop_exit(&header.name, &loop_nodes, blocks, &successors, &order)
        else {
            continue;
        };
        merges.insert(
            header.name.clone(),
            LoopMergeInfo {
                merge,
                continue_target,
            },
        );
    }
    merges
}

#[cfg(test)]
pub(in crate::native) fn normalize_loop_continue_targets(
    blocks: &[BodyBlock],
    loop_merges: &mut HashMap<String, LoopMergeInfo>,
) {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.as_str(), block))
        .collect::<HashMap<_, _>>();
    for (header, info) in loop_merges {
        let mut seen = HashSet::default();
        while info.continue_target != *header
            && info.continue_target != info.merge
            && seen.insert(info.continue_target.clone())
        {
            let Some(block) = by_name.get(info.continue_target.as_str()) else {
                break;
            };
            let Some(typed) = block.typed.as_ref() else {
                break;
            };
            if !typed.insts.iter().all(|inst| {
                inst.void_call_line()
                    .as_deref()
                    .is_some_and(crate::native::parse::is_ignored_call_line)
            }) {
                break;
            }
            let crate::native::tir::TirTerminator::Br(target) = &typed.terminator else {
                break;
            };
            if target == header || target == &info.merge {
                break;
            }
            info.continue_target = target.clone();
        }
    }
}

fn natural_loop_nodes(
    header: &str,
    back_preds: &[String],
    predecessors: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut nodes = crate::fxhash::set_from([header.to_string()]);
    let mut stack = back_preds.to_vec();
    while let Some(block) = stack.pop() {
        if !nodes.insert(block.clone()) || block == header {
            continue;
        }
        if let Some(preds) = predecessors.get(&block) {
            stack.extend(preds.iter().cloned());
        }
    }
    nodes
}

fn infer_continue_target(
    header: &str,
    back_preds: &[String],
    loop_nodes: &HashSet<String>,
    successors: &HashMap<String, Vec<String>>,
    order: &HashMap<String, usize>,
) -> Option<String> {
    if back_preds.len() == 1 && back_preds[0] != header {
        return Some(back_preds[0].clone());
    }

    let reachable = successors
        .get(header)?
        .iter()
        .map(|target| (target, reachable_from(target, successors)))
        .collect::<Vec<_>>();
    reachable
        .iter()
        .filter(|(target, _)| loop_nodes.contains(*target))
        .find(|(_, seen)| back_preds.iter().all(|pred| seen.contains(pred)))
        .map(|(target, _)| (*target).clone())
        .or_else(|| {
            back_preds
                .iter()
                .filter(|pred| pred.as_str() != header)
                .min_by_key(|pred| order.get(*pred).copied().unwrap_or(usize::MAX))
                .cloned()
        })
}

fn infer_loop_exit(
    header: &str,
    loop_nodes: &HashSet<String>,
    blocks: &[BodyBlock],
    successors: &HashMap<String, Vec<String>>,
    order: &HashMap<String, usize>,
) -> Option<String> {
    let mut exits = HashSet::default();
    for node in loop_nodes {
        for target in successors.get(node).into_iter().flatten() {
            if !loop_nodes.contains(target) {
                exits.insert(target.clone());
            }
        }
    }
    if exits.len() == 1 {
        return exits.into_iter().next();
    }
    let header_idx = order.get(header).copied()?;
    let reachable = exits
        .iter()
        .map(|exit| reachable_from(exit, successors))
        .collect::<Vec<_>>();
    blocks
        .iter()
        .skip(header_idx + 1)
        .map(|block| &block.name)
        .find(|candidate| reachable.iter().all(|seen| seen.contains(*candidate)))
        .cloned()
}

pub(in crate::native) fn infer_switch_merges(blocks: &[BodyBlock]) -> HashMap<String, String> {
    let by_name: HashMap<String, &BodyBlock> = blocks.iter().map(|b| (b.name.clone(), b)).collect();
    let order: HashMap<String, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    let successors: HashMap<String, Vec<String>> = blocks
        .iter()
        .map(|b| (b.name.clone(), block_successors(b)))
        .collect();
    let mut merges = HashMap::default();
    for block in blocks {
        let Some(targets) = switch_targets(block) else {
            continue;
        };
        let live_targets = targets
            .iter()
            .filter(|target| !block_is_unreachable(target, &by_name))
            .collect::<Vec<_>>();
        if live_targets.is_empty() {
            continue;
        }
        let reachable = live_targets
            .iter()
            .map(|target| (*target, reachable_from(target, &successors)))
            .collect::<Vec<_>>();
        let Some(start) = order.get(&block.name).map(|i| i + 1) else {
            continue;
        };
        for candidate in blocks.iter().skip(start) {
            if reachable
                .iter()
                .all(|(_, seen)| seen.contains(&candidate.name))
                && reachable
                    .iter()
                    .all(|(target, _)| all_paths_reach_target(target, &candidate.name, &successors))
            {
                merges.insert(block.name.clone(), candidate.name.clone());
                break;
            }
        }
    }
    merges
}

pub(in crate::native) fn switch_default_is_inferred_merge(blocks: &[BodyBlock]) -> bool {
    let merges = infer_switch_merges(blocks);
    blocks.iter().any(|block| {
        switch_of(block)
            .is_some_and(|switch| merges.get(&block.name) == Some(&switch.default_label))
    })
}

pub(in crate::native) fn infer_switch_merges_bounded(
    blocks: &[BodyBlock],
) -> HashMap<String, String> {
    if blocks.len() > crate::native::cfg::CROSS_ARM_EDGE_MAX_BLOCKS {
        infer_direct_switch_merges(blocks)
    } else {
        infer_switch_merges(blocks)
    }
}

pub(in crate::native) fn infer_direct_switch_merges(
    blocks: &[BodyBlock],
) -> HashMap<String, String> {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.clone(), block))
        .collect::<HashMap<_, _>>();
    let order = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut merges = HashMap::default();
    for block in blocks {
        let Some(targets) = switch_targets(block) else {
            continue;
        };
        let live_targets = targets
            .into_iter()
            .filter(|target| !block_is_unreachable(target, &by_name))
            .collect::<Vec<_>>();
        let Some(first) = live_targets.first() else {
            continue;
        };
        let mut candidates = Vec::with_capacity(2);
        if let Some(target) = unconditional_branch_target(by_name.get(first).copied()) {
            candidates.push(target);
        }
        candidates.push(first.clone());
        candidates.dedup();
        let Some(header_order) = order.get(block.name.as_str()) else {
            continue;
        };
        let merge = candidates.into_iter().find(|candidate| {
            order
                .get(candidate.as_str())
                .is_some_and(|candidate_order| candidate_order > header_order)
                && live_targets.iter().all(|target| {
                    target == candidate
                        || unconditional_branch_target(by_name.get(target).copied()).as_ref()
                            == Some(candidate)
                })
        });
        if let Some(merge) = merge {
            merges.insert(block.name.clone(), merge);
        }
    }
    merges
}

pub(in crate::native) fn infer_direct_branch_merges(
    blocks: &[BodyBlock],
) -> HashMap<(String, String), String> {
    let by_name = blocks
        .iter()
        .map(|block| (block.name.clone(), block))
        .collect::<HashMap<_, _>>();
    let order = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut merges = HashMap::default();
    for block in blocks {
        let Some((on_true, on_false)) = conditional_branch_targets(block) else {
            continue;
        };
        let Some(header_order) = order.get(block.name.as_str()) else {
            continue;
        };
        let mut candidates = Vec::with_capacity(4);
        for arm in [&on_true, &on_false] {
            if let Some(target) = unconditional_branch_target(by_name.get(arm).copied()) {
                candidates.push(target);
            }
            candidates.push(arm.clone());
        }
        candidates.dedup();
        let merge = candidates.into_iter().find(|candidate| {
            order
                .get(candidate.as_str())
                .is_some_and(|candidate_order| candidate_order > header_order)
                && [&on_true, &on_false].into_iter().all(|arm| {
                    block_is_unreachable(arm, &by_name)
                        || arm == candidate
                        || unconditional_branch_target(by_name.get(arm).copied()).as_ref()
                            == Some(candidate)
                })
        });
        if let Some(merge) = merge {
            merges.insert((on_true, on_false), merge);
        }
    }
    merges
}

pub(in crate::native) fn infer_bounded_branch_merges_by_header(
    blocks: &[BodyBlock],
) -> HashMap<String, String> {
    let direct = infer_direct_branch_merges(blocks);
    let Some(cfg) = Cfg::from_blocks(blocks) else {
        return HashMap::default();
    };
    let dominators = cfg.dominators();
    let post_dominators = post_idom(blocks);
    let order = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut merges = HashMap::default();

    for block in blocks {
        let Some((on_true, on_false)) = conditional_branch_targets(block) else {
            continue;
        };
        if let Some(merge) = direct.get(&(on_true, on_false)) {
            merges.insert(block.name.clone(), merge.clone());
            continue;
        }
        let is_loop_header = cfg
            .predecessors
            .get(&block.name)
            .into_iter()
            .flatten()
            .any(|predecessor| dominators.dominates(&block.name, predecessor));
        if is_loop_header {
            continue;
        }
        let Some(merge) = post_dominators.get(&block.name) else {
            continue;
        };
        let ordered_after_header = order
            .get(block.name.as_str())
            .zip(order.get(merge.as_str()))
            .is_some_and(|(header, merge)| merge > header);
        if ordered_after_header {
            merges.insert(block.name.clone(), merge.clone());
        }
    }
    merges
}

pub(in crate::native) fn funnel_shared_branch_dispatches(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    let mut current = blocks.to_vec();
    let mut counter = 0usize;
    for _ in 0..blocks.len() {
        let Some(next) = funnel_one_shared_branch_dispatch(&current, &mut counter) else {
            break;
        };
        current = next;
    }
    current
}

fn funnel_one_shared_branch_dispatch(
    blocks: &[BodyBlock],
    counter: &mut usize,
) -> Option<Vec<BodyBlock>> {
    let cfg = Cfg::from_blocks(blocks)?;
    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let names = by_name.keys().copied().collect::<HashSet<_>>();

    'header: for header in blocks {
        let Some((left, right)) = conditional_branch_targets(header) else {
            continue;
        };
        if left == right {
            continue;
        }
        let (Some(&left_index), Some(&right_index)) =
            (by_name.get(left.as_str()), by_name.get(right.as_str()))
        else {
            continue;
        };
        let private_arm = |arm: &str| {
            cfg.predecessors
                .get(arm)
                .is_some_and(|preds| preds.len() == 1 && preds[0] == header.name)
        };
        if !private_arm(&left) || !private_arm(&right) {
            continue;
        }
        let Some(left_typed) = blocks[left_index].typed.as_deref() else {
            continue;
        };
        let Some(right_typed) = blocks[right_index].typed.as_deref() else {
            continue;
        };
        let crate::native::tir::TirTerminator::BrCond {
            cond: left_cond,
            t: left_true,
            f: left_false,
        } = &left_typed.terminator
        else {
            continue;
        };
        let crate::native::tir::TirTerminator::BrCond {
            cond: right_cond,
            t: right_true,
            f: right_false,
        } = &right_typed.terminator
        else {
            continue;
        };
        if left_true != right_true
            || left_false != right_false
            || left_true == left_false
            || !names.contains(left_true.as_str())
            || !names.contains(left_false.as_str())
        {
            continue;
        }
        let insertion = left_index.max(right_index) + 1;
        if [left_true, left_false].into_iter().any(|target| {
            by_name
                .get(target.as_str())
                .is_none_or(|target_index| *target_index < insertion)
        }) {
            continue;
        }

        let mut join_name = format!("%metal2vulkan.branch_funnel.{}", *counter);
        while names.contains(join_name.as_str()) {
            *counter += 1;
            join_name = format!("%metal2vulkan.branch_funnel.{}", *counter);
        }
        *counter += 1;
        let predicate = format!("{join_name}.predicate");
        let join_lines = vec![
            format!("{predicate} = phi i1 [ {left_cond}, {left} ], [ {right_cond}, {right} ]"),
            format!("br i1 {predicate}, label {left_true}, label {left_false}"),
        ];
        let Some(join_typed) =
            crate::native::tir::lower_block_carrier(&join_name, &join_lines, &HashMap::default())
        else {
            continue;
        };

        let mut target_rewrites = Vec::new();
        let mut join_typed = join_typed;
        let mut join_phi_index = 0usize;
        for target in [left_true, left_false] {
            let target_index = *by_name.get(target.as_str())?;
            let Some(target_typed) = blocks[target_index].typed.as_deref() else {
                continue 'header;
            };
            let mut rewrites = Vec::new();
            for inst in &target_typed.insts {
                let Some((ty, incoming)) = &inst.phi_incoming() else {
                    continue;
                };
                let from_left = incoming
                    .iter()
                    .filter(|(_, pred)| pred == &left)
                    .collect::<Vec<_>>();
                let from_right = incoming
                    .iter()
                    .filter(|(_, pred)| pred == &right)
                    .collect::<Vec<_>>();
                if from_left.is_empty() && from_right.is_empty() {
                    continue;
                }
                if from_left.len() != 1 || from_right.len() != 1 {
                    continue 'header;
                }
                let result = inst.result.as_ref()?;
                let phi_name = format!("{join_name}.phi.{join_phi_index}");
                join_phi_index += 1;
                join_typed.push_value_phi(
                    &phi_name,
                    ty,
                    &[
                        (from_left[0].0.clone(), left.clone()),
                        (from_right[0].0.clone(), right.clone()),
                    ],
                );
                let mut replacement = Vec::with_capacity(incoming.len() - 1);
                let mut inserted = false;
                for (value, pred) in incoming {
                    if pred == &left || pred == &right {
                        if !inserted {
                            replacement.push((LlValue::Local(phi_name.clone()), join_name.clone()));
                            inserted = true;
                        }
                    } else {
                        replacement.push((value.clone(), pred.clone()));
                    }
                }
                rewrites.push((result.clone(), replacement));
            }
            target_rewrites.push((target_index, rewrites));
        }

        let mut out = blocks.to_vec();
        for arm_index in [left_index, right_index] {
            let typed = std::sync::Arc::make_mut(out[arm_index].typed.as_mut()?);
            typed.set_unconditional_branch(&join_name);
        }
        for (target_index, rewrites) in target_rewrites {
            let typed = std::sync::Arc::make_mut(out[target_index].typed.as_mut()?);
            for (result, incoming) in rewrites {
                typed.set_phi_incomings(&result, &incoming);
            }
        }
        out.insert(
            insertion,
            BodyBlock {
                name: join_name,
                role: BlockRole::Normal,
                typed: Some(join_typed.into()),
            },
        );
        if crate::env_vars::retry_debug() {
            eprintln!(
                "[retry-debug] funnel shared dispatch: header={} arms=({}, {}) targets=({}, {})",
                header.name, left, right, left_true, left_false
            );
        }
        return Some(out);
    }
    None
}

pub(in crate::native) fn refunnel_one_deep_shared_arm(
    blocks: &[BodyBlock],
    counter: &mut usize,
) -> Option<Vec<BodyBlock>> {
    let cfg = Cfg::from_blocks(blocks)?;
    let dominators = cfg.dominators();
    let post_dominators = post_idom(blocks);
    let by_name = blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.name.as_str(), index))
        .collect::<HashMap<_, _>>();
    let names = by_name.keys().copied().collect::<HashSet<_>>();

    'header: for header in blocks {
        let Some((on_true, on_false)) = conditional_branch_targets(header) else {
            continue;
        };
        let Some(merge) = post_dominators.get(&header.name) else {
            continue;
        };
        if cfg
            .predecessors
            .get(&header.name)
            .into_iter()
            .flatten()
            .any(|pred| dominators.dominates(&header.name, pred))
        {
            continue;
        }
        for shared in [&on_true, &on_false] {
            if shared == merge {
                continue;
            }
            let candidate_entries = cfg
                .predecessors
                .get(shared)
                .into_iter()
                .flatten()
                .filter(|pred| dominators.dominates(&header.name, pred))
                .cloned()
                .collect::<Vec<_>>();
            if candidate_entries.len() < 2
                || !candidate_entries.iter().any(|pred| pred == &header.name)
            {
                continue;
            }
            let shared_reachable = cfg.reachable_from(shared);
            let shared_entries = candidate_entries
                .into_iter()
                .filter(|pred| !shared_reachable.contains(pred.as_str()))
                .collect::<Vec<_>>();
            if shared_entries.len() < 2 || !shared_entries.iter().any(|pred| pred == &header.name) {
                continue;
            }
            let bypass_entries = cfg
                .predecessors
                .get(merge)
                .into_iter()
                .flatten()
                .filter(|pred| {
                    dominators.dominates(&header.name, pred)
                        && !shared_reachable.contains(pred.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            if bypass_entries.is_empty() {
                continue;
            }
            let mut all_entries = shared_entries
                .iter()
                .map(|pred| (pred.clone(), true))
                .chain(bypass_entries.iter().map(|pred| (pred.clone(), false)))
                .collect::<Vec<_>>();
            all_entries.sort_by_key(|(pred, _)| by_name.get(pred.as_str()).copied());
            all_entries.dedup_by(|left, right| left.0 == right.0);

            let mut join_name = format!("%metal2vulkan.branch_refunnel.{}", *counter);
            while names.contains(join_name.as_str()) {
                *counter += 1;
                join_name = format!("%metal2vulkan.branch_refunnel.{}", *counter);
            }
            *counter += 1;
            let route = format!("{join_name}.route");
            let incoming_text = all_entries
                .iter()
                .map(|(pred, takes_shared)| format!("[ {takes_shared}, {pred} ]"))
                .collect::<Vec<_>>()
                .join(", ");
            let join_lines = vec![
                format!("{route} = phi i1 {incoming_text}"),
                format!("br i1 {route}, label {shared}, label {merge}"),
            ];
            let Some(mut join_typed) = crate::native::tir::lower_block_carrier(
                &join_name,
                &join_lines,
                &HashMap::default(),
            ) else {
                continue;
            };

            let mut target_rewrites = Vec::new();
            let mut join_phi_index = 0usize;
            for (target, entries) in [
                (shared.as_str(), shared_entries.as_slice()),
                (merge.as_str(), bypass_entries.as_slice()),
            ] {
                let target_index = *by_name.get(target)?;
                let Some(target_typed) = blocks[target_index].typed.as_deref() else {
                    continue 'header;
                };
                let entry_set = entries.iter().map(String::as_str).collect::<HashSet<_>>();
                let mut rewrites = Vec::new();
                for inst in &target_typed.insts {
                    let Some((ty, incoming)) = &inst.phi_incoming() else {
                        continue;
                    };
                    let selected = incoming
                        .iter()
                        .filter(|(_, pred)| entry_set.contains(pred.as_str()))
                        .collect::<Vec<_>>();
                    if selected.is_empty() {
                        continue;
                    }
                    if selected.len() != entry_set.len() {
                        continue 'header;
                    }
                    let result = inst.result.as_ref()?;
                    let phi_name = format!("{join_name}.phi.{join_phi_index}");
                    join_phi_index += 1;
                    let join_incoming = all_entries
                        .iter()
                        .map(|(pred, _)| {
                            let value = selected
                                .iter()
                                .find_map(|(value, selected_pred)| {
                                    (selected_pred == pred).then(|| (*value).clone())
                                })
                                .unwrap_or(LlValue::Undef);
                            (value, pred.clone())
                        })
                        .collect::<Vec<_>>();
                    join_typed.push_value_phi(&phi_name, ty, &join_incoming);
                    let mut replacement = Vec::with_capacity(incoming.len() + 1 - selected.len());
                    let mut inserted = false;
                    for (value, pred) in incoming {
                        if entry_set.contains(pred.as_str()) {
                            if !inserted {
                                replacement
                                    .push((LlValue::Local(phi_name.clone()), join_name.clone()));
                                inserted = true;
                            }
                        } else {
                            replacement.push((value.clone(), pred.clone()));
                        }
                    }
                    rewrites.push((result.clone(), replacement));
                }
                target_rewrites.push((target_index, rewrites));
            }

            let mut out = blocks.to_vec();
            for (pred, takes_shared) in &all_entries {
                let target = if *takes_shared { shared } else { merge };
                let pred_index = *by_name.get(pred.as_str())?;
                let typed = std::sync::Arc::make_mut(out[pred_index].typed.as_mut()?);
                typed.redirect_successor(target, &join_name);
            }
            for (target_index, rewrites) in target_rewrites {
                let typed = std::sync::Arc::make_mut(out[target_index].typed.as_mut()?);
                for (result, incoming) in rewrites {
                    typed.set_phi_incomings(&result, &incoming);
                }
            }
            let insertion = *by_name.get(merge.as_str())?;
            out.insert(
                insertion,
                BodyBlock {
                    name: join_name,
                    role: BlockRole::Normal,
                    typed: Some(join_typed.into()),
                },
            );
            return Some(out);
        }
    }
    None
}

pub(in crate::native) fn lower_unstructured_switches(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    lower_switches(blocks, false)
}

pub(in crate::native) fn lower_loop_exit_switches(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    lower_switches(blocks, true)
}

fn lower_switches(blocks: &[BodyBlock], force_loop_exit_ladders: bool) -> Vec<BodyBlock> {
    let normalized = normalize_switch_bypass_merges(blocks);
    let blocks = normalized.as_slice();
    let switch_merges = infer_switch_merges(blocks);
    let loop_exit_ladders = if force_loop_exit_ladders {
        loop_exit_switch_headers(blocks)
    } else {
        Default::default()
    };
    let successors: HashMap<String, Vec<String>> = blocks
        .iter()
        .map(|b| (b.name.clone(), block_successors(b)))
        .collect();
    let mut lowered = Vec::new();
    let mut phi_rewrites: HashMap<String, HashMap<String, Vec<String>>> = HashMap::default();
    let mut synthetic_index = 0usize;

    for block in normalized {
        let Some(switch) = switch_of(&block) else {
            lowered.push(block);
            continue;
        };
        let needs_ladder = loop_exit_ladders.contains(&block.name)
            || !switch_merges.contains_key(&block.name)
            || switch_merges.get(&block.name) == Some(&switch.default_label)
            || switch_case_order_requires_ladder(&block.name, &switch, &switch_merges, &successors);
        if !needs_ladder {
            lowered.push(block);
            continue;
        }
        let Some(selector_ty) = text_type(&switch.selector.ty) else {
            lowered.push(block);
            continue;
        };
        let Some(selector_value) = text_value(&switch.selector.value) else {
            lowered.push(block);
            continue;
        };
        let cases = switch
            .cases
            .iter()
            .map(|(value, label)| text_value(value).map(|value| (value, label.clone())))
            .collect::<Option<Vec<_>>>();
        let Some(cases) = cases else {
            lowered.push(block.clone());
            continue;
        };

        let block_index = synthetic_index;
        synthetic_index += 1;
        if cases.is_empty() {
            let terminator = format!("br label {}", switch.default_label);
            let typed = block
                .typed
                .as_ref()
                .and_then(|p| {
                    crate::native::tir::lower_block_carrier_with_prefix(
                        &block.name,
                        p,
                        std::slice::from_ref(&terminator),
                    )
                })
                .map(Into::into);
            lowered.push(BodyBlock {
                name: block.name.clone(),
                role: block.role,
                typed,
            });
            continue;
        }

        let mut target_predecessors: HashMap<String, Vec<String>> = HashMap::default();
        for (case_index, (case_value, label)) in cases.iter().enumerate() {
            let name = if case_index == 0 {
                block.name.clone()
            } else {
                format!("{SWITCH_BLOCK_PREFIX}{block_index}_{case_index}")
            };
            add_unique(
                target_predecessors.entry(label.clone()).or_default(),
                name.clone(),
            );
            let next_label = if case_index + 1 == cases.len() {
                switch.default_label.clone()
            } else {
                format!("{SWITCH_BLOCK_PREFIX}{block_index}_{}", case_index + 1)
            };
            let false_label = if case_index == 0 || case_index + 1 == cases.len() {
                next_label.clone()
            } else {
                format!("{SWITCH_BLOCK_PREFIX}{block_index}_{case_index}_merge")
            };
            let cond = format!("{SWITCH_BLOCK_PREFIX}{block_index}_{case_index}_cond");
            let tail = vec![
                format!("{cond} = icmp eq {selector_ty} {selector_value}, {case_value}"),
                format!("br i1 {cond}, label {label}, label {false_label}"),
            ];
            let typed = if case_index == 0 {
                block.typed.as_ref().and_then(|p| {
                    crate::native::tir::lower_block_carrier_with_prefix(&name, p, &tail)
                })
            } else {
                crate::native::tir::lower_block_carrier(&name, &tail, &HashMap::default())
            }
            .map(Into::into);
            let role = if case_index == 0 {
                block.role
            } else {
                BlockRole::Normal
            };
            lowered.push(BodyBlock { name, role, typed });
            if false_label != next_label {
                let merge_lines = vec![format!("br label {next_label}")];
                let typed = crate::native::tir::lower_block_carrier(
                    &false_label,
                    &merge_lines,
                    &HashMap::default(),
                )
                .map(Into::into);
                lowered.push(BodyBlock {
                    name: false_label,
                    role: BlockRole::Normal,
                    typed,
                });
            }
        }
        let default_pred = if cases.len() == 1 {
            block.name.clone()
        } else {
            format!("{SWITCH_BLOCK_PREFIX}{block_index}_{}", cases.len() - 1)
        };
        add_unique(
            target_predecessors
                .entry(switch.default_label.clone())
                .or_default(),
            default_pred,
        );
        record_phi_rewrites(&mut phi_rewrites, &block.name, target_predecessors);
    }

    rewrite_lowered_switch_target_phis(&mut lowered, &phi_rewrites);
    lowered
}

fn loop_exit_switch_headers(blocks: &[BodyBlock]) -> HashSet<String> {
    let forest = analyze(blocks);
    let mut headers = HashSet::default();
    for loop_info in &forest.loops {
        if loop_info.exits.is_empty() {
            continue;
        }
        let exits = loop_info.exits.iter().collect::<HashSet<_>>();
        let body: HashSet<&str> = loop_info.body.iter().map(String::as_str).collect();
        for block in blocks {
            if (block.name != loop_info.header && !body.contains(block.name.as_str()))
                || !block.typed.as_ref().is_some_and(|t| {
                    matches!(
                        t.terminator,
                        crate::native::tir::TirTerminator::Switch { .. }
                    )
                })
            {
                continue;
            }
            let successors = block_successors(block);
            if successors.iter().any(|target| exits.contains(target))
                && successors
                    .iter()
                    .any(|target| body.contains(target.as_str()))
            {
                headers.insert(block.name.clone());
            }
        }
    }
    headers
}

fn switch_of(block: &BodyBlock) -> Option<LlSwitch> {
    block.typed.as_ref()?.switch.clone()
}

fn switch_case_order_requires_ladder(
    block_name: &str,
    switch: &LlSwitch,
    switch_merges: &HashMap<String, String>,
    successors: &HashMap<String, Vec<String>>,
) -> bool {
    let case_targets = switch_case_targets_in_order(switch);
    let merge = switch_merges.get(block_name);
    let order = case_targets
        .iter()
        .enumerate()
        .map(|(idx, target)| (target.clone(), idx))
        .collect::<HashMap<_, _>>();
    if !order.contains_key(&switch.default_label)
        && !first_reachable_case_targets(
            &switch.default_label,
            merge.map(String::as_str),
            Some(block_name),
            &order,
            successors,
        )
        .is_empty()
    {
        return true;
    }
    if case_targets.len() < 2 {
        return false;
    }
    for (idx, target) in case_targets.iter().enumerate() {
        let reached_targets = first_reachable_case_targets(
            target,
            merge.map(String::as_str),
            None,
            &order,
            successors,
        );
        for reached in reached_targets {
            let Some(reached_idx) = order.get(&reached).copied() else {
                continue;
            };
            if idx + 1 != reached_idx {
                return true;
            }
        }
    }
    false
}

fn first_reachable_case_targets(
    start: &str,
    merge: Option<&str>,
    stop: Option<&str>,
    case_order: &HashMap<String, usize>,
    successors: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut reached = HashSet::default();
    let mut seen = HashSet::default();
    let mut stack = successors.get(start).cloned().unwrap_or_default();
    while let Some(node) = stack.pop() {
        let is_boundary = merge == Some(node.as_str()) || stop == Some(node.as_str());
        if is_boundary || !seen.insert(node.clone()) {
            continue;
        }
        if node != start && case_order.contains_key(&node) {
            reached.insert(node);
            continue;
        }
        if let Some(next) = successors.get(&node) {
            stack.extend(next.iter().cloned());
        }
    }
    reached
}

fn switch_case_targets_in_order(switch: &LlSwitch) -> Vec<String> {
    let mut targets = Vec::new();
    let mut seen = HashSet::default();
    for (_, label) in &switch.cases {
        if seen.insert(label.clone()) {
            targets.push(label.clone());
        }
    }
    targets
}

fn record_phi_rewrites(
    rewrites: &mut HashMap<String, HashMap<String, Vec<String>>>,
    old_pred: &str,
    target_predecessors: HashMap<String, Vec<String>>,
) {
    for (target, new_preds) in target_predecessors {
        let pred_rewrites = rewrites.entry(target).or_default();
        let rewritten = pred_rewrites.entry(old_pred.to_string()).or_default();
        for new_pred in new_preds {
            add_unique(rewritten, new_pred);
        }
    }
}

fn rewrite_lowered_switch_target_phis(
    blocks: &mut [BodyBlock],
    rewrites: &HashMap<String, HashMap<String, Vec<String>>>,
) {
    for block in blocks {
        let Some(block_rewrites) = rewrites.get(&block.name) else {
            continue;
        };
        if let Some(t) = block.typed_mut() {
            t.expand_phi_predecessors(block_rewrites);
        }
    }
}

fn add_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn normalize_switch_bypass_merges(blocks: &[BodyBlock]) -> Vec<BodyBlock> {
    let mut normalized = blocks.to_vec();
    let mut synthetic_index = 0usize;
    while normalize_one_switch_bypass_merge(&mut normalized, &mut synthetic_index) {}
    normalized
}

fn normalize_one_switch_bypass_merge(
    blocks: &mut Vec<BodyBlock>,
    synthetic_index: &mut usize,
) -> bool {
    let unreachable = blocks
        .iter()
        .filter(|block| {
            block.typed.as_ref().is_some_and(|t| {
                matches!(t.terminator, crate::native::tir::TirTerminator::Unreachable)
            })
        })
        .map(|block| block.name.clone())
        .collect::<HashSet<_>>();
    let order: HashMap<String, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.clone(), i))
        .collect();
    let successors: HashMap<String, Vec<String>> = blocks
        .iter()
        .map(|b| (b.name.clone(), block_successors(b)))
        .collect();

    for block in blocks.clone() {
        let Some(targets) = switch_targets(&block) else {
            continue;
        };
        let live_targets = targets
            .iter()
            .filter(|target| !unreachable.contains(*target))
            .cloned()
            .collect::<Vec<_>>();
        if live_targets.is_empty() {
            continue;
        }
        let reachable = live_targets
            .iter()
            .map(|target| (target, reachable_from(target, &successors)))
            .collect::<Vec<_>>();
        let Some(start) = order.get(&block.name).map(|i| i + 1) else {
            continue;
        };
        let mut intermediate = None;
        let mut merge = None;
        for candidate in blocks.iter().skip(start) {
            if !reachable
                .iter()
                .all(|(_, seen)| seen.contains(&candidate.name))
            {
                continue;
            }
            if reachable
                .iter()
                .all(|(target, _)| all_paths_reach_target(target, &candidate.name, &successors))
            {
                merge = Some(candidate.name.clone());
                break;
            }
            intermediate.get_or_insert_with(|| candidate.name.clone());
        }
        let (Some(intermediate), Some(merge)) = (intermediate, merge) else {
            continue;
        };
        let intermediate_exit = blocks
            .iter()
            .find(|block| block.name == intermediate)
            .and_then(|block| unconditional_branch_target(Some(block)));
        if intermediate_exit != Some(merge.clone()) {
            continue;
        }
        if rewrite_switch_bypass_merge(
            blocks,
            &live_targets,
            &successors,
            &intermediate,
            &merge,
            *synthetic_index,
        ) {
            *synthetic_index += 1;
            return true;
        }
    }
    false
}

fn rewrite_switch_bypass_merge(
    blocks: &mut Vec<BodyBlock>,
    live_targets: &[String],
    successors: &HashMap<String, Vec<String>>,
    intermediate: &str,
    merge: &str,
    switch_index: usize,
) -> bool {
    let reachable_from_cases = live_targets
        .iter()
        .flat_map(|target| reachable_from(target, successors))
        .collect::<HashSet<_>>();
    let bypass_preds = blocks
        .iter()
        .filter(|block| block.name != intermediate && reachable_from_cases.contains(&block.name))
        .filter(|block| block_successors(block).iter().any(|target| target == merge))
        .map(|block| block.name.clone())
        .collect::<Vec<_>>();
    if bypass_preds.is_empty() {
        return false;
    }

    let Some(intermediate_idx) = blocks.iter().position(|block| block.name == intermediate) else {
        return false;
    };
    let Some(merge_idx) = blocks.iter().position(|block| block.name == merge) else {
        return false;
    };

    let bypass_set = bypass_preds.iter().cloned().collect::<HashSet<_>>();

    let intermediate_phi_results: HashSet<String> = match &blocks[intermediate_idx].typed {
        Some(t) => t
            .insts
            .iter()
            .filter(|inst| inst.is_phi())
            .filter_map(|inst| inst.result.clone())
            .collect(),
        None => return false,
    };

    let typed_additions: HashMap<String, Vec<(LlValue, String)>> = {
        let Some(merge_carrier) = &blocks[merge_idx].typed else {
            return false;
        };
        let merge_phis: Vec<&crate::native::tir::TirInst> = merge_carrier
            .insts
            .iter()
            .filter(|inst| inst.is_phi())
            .collect();
        if merge_phis.is_empty() {
            return false;
        }
        let mut additions: HashMap<String, Vec<(LlValue, String)>> = HashMap::default();
        for merge_phi in merge_phis {
            let Some((_, incoming)) = &merge_phi.phi_incoming() else {
                return false;
            };
            let Some((inner_value, _)) = incoming.iter().find(|(_, pred)| pred == intermediate)
            else {
                return false;
            };
            let LlValue::Local(inner_name) = inner_value else {
                return false;
            };
            if !intermediate_phi_results.contains(inner_name) {
                return false;
            }
            for (value, pred) in incoming
                .iter()
                .filter(|(_, pred)| bypass_set.contains(pred))
            {
                let label = synthetic_bypass_label(switch_index, pred);
                additions
                    .entry(inner_name.clone())
                    .or_default()
                    .push((value.clone(), label));
            }
        }
        additions
    };
    if typed_additions.is_empty() {
        return false;
    }

    let mut redirects = Vec::with_capacity(bypass_preds.len());
    for pred in &bypass_preds {
        let synthetic = synthetic_bypass_label(switch_index, pred);
        let Some(pred_idx) = blocks.iter().position(|block| &block.name == pred) else {
            return false;
        };
        let redirectable = blocks[pred_idx].typed.as_ref().is_some_and(|t| {
            matches!(
                t.terminator,
                crate::native::tir::TirTerminator::Br(_)
                    | crate::native::tir::TirTerminator::BrCond { .. }
            )
        });
        if !redirectable {
            return false;
        }
        redirects.push((pred_idx, pred.clone(), synthetic));
    }

    let mut insert_after = HashMap::default();
    for (pred_idx, pred, synthetic) in redirects {
        if let Some(t) = blocks[pred_idx].typed_mut() {
            t.redirect_successor(merge, &synthetic);
        }
        insert_after.insert(pred, synthetic);
    }
    if let Some(t) = blocks[intermediate_idx].typed_mut() {
        for (result, extra) in &typed_additions {
            for (value, pred) in extra {
                t.append_phi_incoming(result, value.clone(), pred);
            }
        }
    }
    if let Some(t) = blocks[merge_idx].typed_mut() {
        t.rebuild_phi_incomings(|pred| !bypass_set.contains(pred));
    }

    for block_idx in (0..blocks.len()).rev() {
        let Some(synthetic) = insert_after.get(&blocks[block_idx].name).cloned() else {
            continue;
        };
        let lines = vec![format!("br label {intermediate}")];
        let typed =
            crate::native::tir::lower_block_carrier(&synthetic, &lines, &HashMap::default())
                .map(Into::into);
        blocks.insert(
            block_idx + 1,
            BodyBlock {
                name: synthetic,
                role: BlockRole::SwitchBypass,
                typed,
            },
        );
    }
    true
}

fn synthetic_bypass_label(switch_index: usize, pred: &str) -> String {
    format!(
        "{SWITCH_BYPASS_PREFIX}{}_{}",
        switch_index,
        pred.trim_start_matches('%')
    )
}

fn text_type(ty: &LlType) -> Option<String> {
    match ty {
        LlType::Bool => Some("i1".to_string()),
        LlType::Int(bits) => Some(format!("i{bits}")),
        _ => None,
    }
}

fn text_value(value: &LlValue) -> Option<String> {
    match value {
        LlValue::Local(name) | LlValue::Global(name) => Some(name.clone()),
        LlValue::Bool(value) => Some(value.to_string()),
        LlValue::Int(value) | LlValue::Hex(value) => Some(value.to_string()),
        LlValue::SignedInt(value) => Some(value.to_string()),
        _ => None,
    }
}

fn successors_of_terminator(term: &crate::native::tir::TirTerminator) -> Vec<String> {
    let is_switch = matches!(term, crate::native::tir::TirTerminator::Switch { .. });
    let mut succ: Vec<String> = term.successors().into_iter().map(String::from).collect();
    if is_switch {
        succ.sort();
        succ.dedup();
    }
    succ
}

pub(in crate::native) fn conditional_branch_targets(block: &BodyBlock) -> Option<(String, String)> {
    match &block.typed.as_ref()?.terminator {
        crate::native::tir::TirTerminator::BrCond { t, f, .. } => Some((t.clone(), f.clone())),
        _ => None,
    }
}

pub(in crate::native) fn block_successors(block: &BodyBlock) -> Vec<String> {
    match &block.typed {
        Some(carrier) => successors_of_terminator(&carrier.terminator),
        None => Vec::new(),
    }
}

fn switch_targets(block: &BodyBlock) -> Option<Vec<String>> {
    match &block.typed.as_ref()?.terminator {
        crate::native::tir::TirTerminator::Switch { default, cases, .. } => {
            let mut targets = Vec::with_capacity(cases.len() + 1);
            targets.push(default.clone());
            targets.extend(cases.iter().map(|(_, label)| label.clone()));
            targets.sort();
            targets.dedup();
            Some(targets)
        }
        _ => None,
    }
}

fn unconditional_branch_target(block: Option<&BodyBlock>) -> Option<String> {
    match &block?.typed.as_ref()?.terminator {
        crate::native::tir::TirTerminator::Br(t) => Some(t.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(name: &str, terminator: &str) -> BodyBlock {
        synthetic_block(
            name.to_string(),
            vec![terminator.to_string()],
            BlockRole::Normal,
        )
    }

    #[test]
    fn loop_plan_resolves_emission_empty_continue_chain_before_emission() {
        let blocks = vec![
            block("%header", "br label %body"),
            block("%body", "br label %pass0"),
            synthetic_block(
                "%pass0".to_string(),
                vec![
                    "call void @llvm.lifetime.start.p0(i64 4, ptr null)".to_string(),
                    "br label %pass1".to_string(),
                ],
                BlockRole::Normal,
            ),
            block("%pass1", "br label %header"),
            block("%merge", "ret void"),
        ];
        let mut loop_merges = crate::fxhash::map_from([(
            "%header".to_string(),
            LoopMergeInfo {
                merge: "%merge".to_string(),
                continue_target: "%pass0".to_string(),
            },
        )]);

        normalize_loop_continue_targets(&blocks, &mut loop_merges);

        assert_eq!(loop_merges["%header"].continue_target, "%pass1");
    }

    #[test]
    fn pair_keyed_branch_merges_are_indexed_only_for_unclaimed_selection_headers() {
        let blocks = vec![
            block("%first", "br i1 %c0, label %yes, label %no"),
            block("%second", "br i1 %c1, label %yes, label %no"),
            block("%loop", "br i1 %c2, label %yes, label %no"),
        ];
        let branch_merges = crate::fxhash::map_from([(
            ("%yes".to_string(), "%no".to_string()),
            "%shared".to_string(),
        )]);
        let loop_merges = crate::fxhash::map_from([(
            "%loop".to_string(),
            LoopMergeInfo {
                merge: "%exit".to_string(),
                continue_target: "%continue".to_string(),
            },
        )]);
        let mut by_header =
            crate::fxhash::map_from([("%second".to_string(), "%specific".to_string())]);

        index_branch_merges_by_header(&blocks, &loop_merges, &branch_merges, &mut by_header);

        assert_eq!(by_header["%first"], "%shared");
        assert_eq!(by_header["%second"], "%specific");
        assert!(!by_header.contains_key("%loop"));
    }

    #[test]
    fn implicit_entry_block_name_uses_next_numeric_value() {
        let function = LlFunction {
            name: "main".to_string(),
            ret: LlType::Void,
            params: vec![
                ("%0".to_string(), LlType::Int(32)),
                ("%named".to_string(), LlType::Int(32)),
                ("%6".to_string(), LlType::Int(32)),
            ],
            byval_param_pointees: vec![None; 3],
            is_static_initializer: false,
            blocks: Vec::new(),
            loop_controls: HashMap::default(),
        };

        assert_eq!(implicit_entry_block_name(&function), "%7");

        let function = LlFunction {
            name: "main".to_string(),
            ret: LlType::Void,
            params: vec![("%named".to_string(), LlType::Int(32))],
            byval_param_pointees: vec![None],
            is_static_initializer: false,
            blocks: Vec::new(),
            loop_controls: HashMap::default(),
        };

        assert_eq!(implicit_entry_block_name(&function), "%0");
    }

    #[test]
    fn branch_merge_ignores_backedge_reachability_through_header() {
        let blocks = vec![
            block("entry", "br label %header"),
            block("%header", "br i1 %cond, label %latch, label %body"),
            block("%body", "br label %inner"),
            block("%inner", "br i1 %done, label %latch, label %inner"),
            block("%latch", "br i1 %again, label %header, label %exit"),
            block("%exit", "ret void"),
        ];
        let merges = infer_branch_merges(&blocks);

        assert_eq!(
            merges.get(&("%latch".to_string(), "%body".to_string())),
            Some(&"%latch".to_string())
        );
    }

    #[test]
    fn branch_merge_ignores_latch_cycle_through_predecessor_to_header() {
        let blocks = vec![
            block("%95", "br i1 %outer, label %96, label %113"),
            block("%96", "br label %113"),
            block("%113", "br i1 %zero_order, label %122, label %115"),
            block("%115", "br label %125"),
            block("%122", "br i1 %next_y, label %44, label %47"),
            block("%125", "br i1 %inner_done, label %122, label %125"),
            block("%44", "br label %41"),
            block("%47", "br label %95"),
            block("%41", "br label %exit"),
            block("%exit", "ret void"),
        ];
        let merges = infer_branch_merges(&blocks);

        assert_eq!(
            merges.get(&("%122".to_string(), "%115".to_string())),
            Some(&"%122".to_string())
        );
    }

    #[test]
    fn branch_merge_keeps_cubemap_latch_before_accumulation_body() {
        let blocks = vec![
            block("%3", "br label %29"),
            block("%29", "br i1 %empty, label %41, label %33"),
            block("%31", "br i1 %zero_order, label %138, label %139"),
            block("%33", "br label %47"),
            block("%41", "br i1 %face_done, label %31, label %29"),
            block("%44", "br i1 %row_done, label %41, label %33"),
            block(
                "%47",
                "switch i32 %face, label %69 [ i32 0, label %54 i32 1, label %57 i32 2, label %60 i32 3, label %63 i32 4, label %66 ]",
            ),
            block("%54", "br label %73"),
            block("%57", "br label %73"),
            block("%60", "br label %73"),
            block("%63", "br label %73"),
            block("%66", "br label %73"),
            block("%69", "br label %73"),
            block("%73", "br i1 %order1, label %86, label %87"),
            block("%86", "br label %87"),
            block("%87", "br i1 %order2, label %88, label %95"),
            block("%88", "br label %95"),
            block("%95", "br i1 %order3, label %96, label %113"),
            block("%96", "br label %113"),
            block("%113", "br i1 %zero_order, label %122, label %115"),
            block("%115", "br label %125"),
            block("%122", "br i1 %texel_done, label %44, label %47"),
            block("%125", "br i1 %coeff_done, label %122, label %125"),
            block("%138", "ret void"),
            block("%139", "br i1 %out_done, label %138, label %139"),
        ];
        let lowered = lower_unstructured_switches(&blocks);
        let merges = infer_branch_merges(&lowered);

        assert_eq!(
            merges.get(&("%122".to_string(), "%115".to_string())),
            Some(&"%122".to_string()),
            "{merges:?}"
        );
    }

    #[test]
    fn switch_whose_default_is_its_merge_lowers_to_branch_ladder() {
        let blocks = vec![
            block(
                "%switch",
                "switch i32 %tag, label %merge [ i32 0, label %a i32 1, label %b i32 2, label %c ]",
            ),
            block("%a", "br label %merge"),
            block("%b", "br label %merge"),
            block("%c", "br label %merge"),
            block("%merge", "ret void"),
        ];
        assert_eq!(
            infer_switch_merges(&blocks).get("%switch"),
            Some(&"%merge".to_string())
        );

        let lowered = lower_unstructured_switches(&blocks);
        assert!(lowered.iter().all(|block| switch_of(block).is_none()));
        assert!(lowered.len() > blocks.len());
    }

    #[test]
    fn direct_switch_merge_accepts_only_local_common_reconvergence() {
        let blocks = vec![
            block(
                "%switch",
                "switch i32 %tag, label %dead [ i32 0, label %a i32 1, label %b ]",
            ),
            block("%a", "br label %merge"),
            block("%b", "br label %merge"),
            block("%dead", "unreachable"),
            block("%merge", "ret void"),
        ];
        assert_eq!(
            infer_direct_switch_merges(&blocks).get("%switch"),
            Some(&"%merge".to_string())
        );

        let mut divergent = blocks;
        divergent[2] = block("%b", "br label %other");
        divergent.push(block("%other", "ret void"));
        assert!(infer_direct_switch_merges(&divergent).is_empty());
    }

    #[test]
    fn direct_branch_merge_accepts_only_local_common_reconvergence() {
        let blocks = vec![
            block("%if", "br i1 %cond, label %a, label %b"),
            block("%a", "br label %merge"),
            block("%b", "br label %merge"),
            block("%merge", "ret void"),
        ];
        assert_eq!(
            infer_direct_branch_merges(&blocks).get(&("%a".to_string(), "%b".to_string())),
            Some(&"%merge".to_string())
        );

        let mut divergent = blocks;
        divergent[2] = block("%b", "br label %other");
        divergent.push(block("%other", "ret void"));
        assert!(infer_direct_branch_merges(&divergent).is_empty());

        let terminal = vec![
            block("%if", "br i1 %cond, label %live, label %dead"),
            block("%live", "br label %merge"),
            block("%dead", "unreachable"),
            block("%merge", "ret void"),
        ];
        assert_eq!(
            infer_direct_branch_merges(&terminal).get(&("%live".to_string(), "%dead".to_string())),
            Some(&"%merge".to_string())
        );
    }

    #[test]
    fn bounded_branch_merge_uses_post_dominance_but_excludes_loop_headers() {
        let blocks = vec![
            block("%if", "br i1 %cond, label %a, label %b"),
            block("%a", "br label %a_tail"),
            block("%a_tail", "br label %merge"),
            block("%b", "br label %b_tail"),
            block("%b_tail", "br label %merge"),
            block("%merge", "ret void"),
        ];
        assert_eq!(
            infer_bounded_branch_merges_by_header(&blocks).get("%if"),
            Some(&"%merge".to_string())
        );

        let loop_blocks = vec![
            block("%loop", "br i1 %cond, label %body, label %exit"),
            block("%body", "br label %loop"),
            block("%exit", "ret void"),
        ];
        assert!(infer_bounded_branch_merges_by_header(&loop_blocks).is_empty());
    }

    #[test]
    fn shared_branch_dispatch_is_funnelled_with_destination_phis() {
        let blocks = vec![
            block("%header", "br i1 %outer, label %left, label %right"),
            block("%left", "br i1 %left_cond, label %taken, label %done"),
            block("%right", "br i1 %right_cond, label %taken, label %done"),
            synthetic_block(
                "%taken".to_string(),
                vec![
                    "%value = phi i32 [ 1, %left ], [ 2, %right ]".to_string(),
                    "ret void".to_string(),
                ],
                BlockRole::Normal,
            ),
            block("%done", "ret void"),
        ];
        let out = funnel_shared_branch_dispatches(&blocks);
        let join = out
            .iter()
            .find(|block| block.name.starts_with("%metal2vulkan.branch_funnel."))
            .expect("shared dispatch receives a funnel");
        assert_eq!(block_successors(&out[1]), vec![join.name.clone()]);
        assert_eq!(block_successors(&out[2]), vec![join.name.clone()]);
        assert_eq!(
            conditional_branch_targets(join),
            Some(("%taken".to_string(), "%done".to_string()))
        );

        let taken = out.iter().find(|block| block.name == "%taken").unwrap();
        let incoming = &taken.typed.as_ref().unwrap().insts[0]
            .phi_incoming()
            .as_ref()
            .unwrap()
            .1;
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1, join.name);
        assert_eq!(
            join.typed
                .as_ref()
                .unwrap()
                .insts
                .iter()
                .filter(|inst| inst.opcode == "phi")
                .count(),
            2
        );
    }

    #[test]
    fn deep_shared_arm_is_refunnelled_without_cloning_its_region() {
        let blocks = vec![
            block("%header", "br i1 %outer, label %shared, label %inner"),
            block("%inner", "br label %dispatch"),
            block(
                "%dispatch",
                "br i1 %nested, label %via_shared, label %bypass",
            ),
            block("%via_shared", "br label %shared"),
            block("%bypass", "br label %merge"),
            synthetic_block(
                "%shared".to_string(),
                vec![
                    "%shared_value = phi i32 [ 1, %header ], [ 2, %via_shared ]".to_string(),
                    "br label %merge".to_string(),
                ],
                BlockRole::Normal,
            ),
            synthetic_block(
                "%merge".to_string(),
                vec![
                    "%merge_value = phi i32 [ 3, %bypass ], [ %shared_value, %shared ]".to_string(),
                    "ret void".to_string(),
                ],
                BlockRole::Normal,
            ),
        ];
        let out = refunnel_one_deep_shared_arm(&blocks, &mut 0).unwrap();
        let join = out
            .iter()
            .find(|block| block.name.starts_with("%metal2vulkan.branch_refunnel."))
            .expect("deep shared arm receives a refunnel");
        assert_eq!(
            conditional_branch_targets(join),
            Some(("%shared".to_string(), "%merge".to_string()))
        );
        for predecessor in ["%header", "%via_shared", "%bypass"] {
            let block = out.iter().find(|block| block.name == predecessor).unwrap();
            assert!(block_successors(block).contains(&join.name));
        }
        let join_phis = join
            .typed
            .as_ref()
            .unwrap()
            .insts
            .iter()
            .filter_map(|instruction| instruction.phi_incoming().as_ref())
            .collect::<Vec<_>>();
        assert_eq!(join_phis.len(), 3, "route plus both forwarded values");
        assert!(join_phis.iter().all(|(_, incoming)| incoming.len() == 3));
        assert!(join_phis.iter().skip(1).all(|(_, incoming)| incoming
            .iter()
            .any(|(value, _)| matches!(value, LlValue::Undef))));
        assert_eq!(out.len(), blocks.len() + 1);
    }

    #[test]
    fn loop_exit_switch_lowers_to_a_branch_ladder() {
        let blocks = vec![
            block("%entry", "br label %head"),
            block("%head", "br i1 %run, label %sw, label %exit"),
            block(
                "%sw",
                "switch i32 %tag, label %exit [ i32 0, label %latch i32 4, label %latch ]",
            ),
            block("%latch", "br label %head"),
            block("%exit", "ret void"),
        ];
        let lowered = lower_loop_exit_switches(&blocks);
        assert!(
            lowered.len() > blocks.len(),
            "the forced ladder adds a comparison block: {lowered:#?}"
        );
        assert!(
            !lowered.iter().any(|block| {
                block.typed.as_ref().is_some_and(|t| {
                    matches!(
                        t.terminator,
                        crate::native::tir::TirTerminator::Switch { .. }
                    )
                })
            }),
            "loop-exiting switch must be lowered: {lowered:#?}"
        );
        let first = lowered.iter().find(|block| block.name == "%sw").unwrap();
        assert!(
            block_successors(first)
                .iter()
                .any(|target| target == "%latch"),
            "first equality branch still reaches the case target: {first:#?}"
        );
        assert!(
            lowered.iter().any(|block| block_successors(block)
                .iter()
                .any(|target| target == "%exit")),
            "the ladder preserves the default loop-exit target: {lowered:#?}"
        );
    }
}
