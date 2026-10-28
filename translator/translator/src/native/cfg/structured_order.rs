use super::loopforest::LoopForest;
use super::BodyBlock;
use crate::fxhash::{HashMap, HashSet};

pub(in crate::native) fn structured_order(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    merge_of: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    structured_order_with_loop_merges_last(blocks, forest, merge_of, false)
}

pub(in crate::native) fn structured_order_terminal(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    merge_of: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    structured_order_with_loop_merges_last(blocks, forest, merge_of, true)
}

fn structured_order_with_loop_merges_last(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    merge_of: impl Fn(&str) -> Option<String>,
    defer_loop_merges_last: bool,
) -> Vec<String> {
    let pos: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();

    let mut children: HashMap<String, Vec<String>> = HashMap::default();
    for b in blocks {
        if let Some(d) = forest.idom(&b.name) {
            children
                .entry(d.to_string())
                .or_default()
                .push(b.name.clone());
        }
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|n| pos.get(n.as_str()).copied().unwrap_or(usize::MAX));
    }

    let mut defer_at: HashMap<String, Vec<String>> = HashMap::default();
    let mut loop_merges = HashSet::default();
    for b in blocks {
        if let Some(m) = merge_of(&b.name) {
            if forest.loop_for_header(&b.name).is_some() {
                loop_merges.insert(m.clone());
            }
            if let Some(d) = forest.idom(&m) {
                let slot = defer_at.entry(d.to_string()).or_default();
                if !slot.contains(&m) {
                    slot.push(m);
                }
            }
        }
    }
    for ms in defer_at.values_mut() {
        if defer_loop_merges_last {
            ms.sort_by_key(|n| {
                (
                    loop_merges.contains(n) as u8,
                    pos.get(n.as_str()).copied().unwrap_or(usize::MAX),
                )
            });
        } else {
            ms.sort_by_key(|n| pos.get(n.as_str()).copied().unwrap_or(usize::MAX));
        }
    }

    let Some(entry) = blocks.first().map(|b| b.name.clone()) else {
        return Vec::new();
    };

    let mut order = Vec::with_capacity(blocks.len());
    let mut visited = HashSet::default();
    let mut stack = vec![entry];
    while let Some(b) = stack.pop() {
        if !visited.insert(b.clone()) {
            continue;
        }
        order.push(b.clone());

        let deferred = defer_at.get(&b).cloned().unwrap_or_default();
        let mut kids = children.get(&b).cloned().unwrap_or_default();
        if !deferred.is_empty() {
            kids.retain(|c| !deferred.contains(c));
        }
        for m in deferred.into_iter().rev() {
            stack.push(m);
        }
        for c in kids.into_iter().rev() {
            stack.push(c);
        }
    }

    for b in blocks {
        if !visited.contains(&b.name) {
            order.push(b.name.clone());
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::super::loopforest::analyze;
    use super::*;

    fn bb(name: &str, term: &str) -> BodyBlock {
        let name = name.to_string();
        let typed = crate::native::tir::lower_block_carrier(
            &name,
            &[term.to_string()],
            &crate::fxhash::HashMap::default(),
        );
        BodyBlock {
            name,
            role: crate::native::cfg::BlockRole::Normal,
            typed: typed.map(Into::into),
        }
    }

    fn order_of(blocks: &[BodyBlock], merges: &[(&str, &str)]) -> Vec<String> {
        let forest = analyze(blocks);
        let map: HashMap<String, String> = merges
            .iter()
            .map(|(h, m)| (h.to_string(), m.to_string()))
            .collect();
        structured_order(blocks, &forest, |h| map.get(h).cloned())
    }

    fn terminal_order_of(blocks: &[BodyBlock], merges: &[(&str, &str)]) -> Vec<String> {
        let forest = analyze(blocks);
        let map: HashMap<String, String> = merges
            .iter()
            .map(|(h, m)| (h.to_string(), m.to_string()))
            .collect();
        structured_order_terminal(blocks, &forest, |h| map.get(h).cloned())
    }

    #[test]
    fn if_diamond_emits_merge_last() {
        let blocks = vec![
            bb("%entry", "br i1 %c, label %a, label %b"),
            bb("%a", "br label %m"),
            bb("%b", "br label %m"),
            bb("%m", "ret void"),
        ];
        let order = order_of(&blocks, &[("%entry", "%m")]);
        assert_eq!(order, vec!["%entry", "%a", "%b", "%m"]);
    }

    #[test]
    fn merge_before_arms_is_reordered_after() {
        let blocks = vec![
            bb("%entry", "br i1 %c, label %a, label %b"),
            bb("%m", "ret void"),
            bb("%a", "br label %m"),
            bb("%b", "br label %m"),
        ];
        let order = order_of(&blocks, &[("%entry", "%m")]);
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(pos("%a") < pos("%m") && pos("%b") < pos("%m"));
        assert_eq!(order[0], "%entry");
    }

    #[test]
    fn loop_emits_exit_after_body() {
        let blocks = vec![
            bb("%entry", "br label %h"),
            bb("%h", "br i1 %c, label %body, label %exit"),
            bb("%body", "br label %h"),
            bb("%exit", "ret void"),
        ];
        let order = order_of(&blocks, &[("%h", "%exit")]);
        assert_eq!(order, vec!["%entry", "%h", "%body", "%exit"]);
    }

    #[test]
    fn nested_if_orders_inner_before_outer_merge() {
        let blocks = vec![
            bb("%entry", "br i1 %c0, label %outer_then, label %outer_merge"),
            bb(
                "%outer_then",
                "br i1 %c1, label %inner_then, label %inner_merge",
            ),
            bb("%inner_then", "br label %inner_merge"),
            bb("%inner_merge", "br label %outer_merge"),
            bb("%outer_merge", "ret void"),
        ];
        let order = order_of(
            &blocks,
            &[("%entry", "%outer_merge"), ("%outer_then", "%inner_merge")],
        );
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert_eq!(order[0], "%entry");
        assert!(pos("%inner_then") < pos("%inner_merge"));
        assert!(pos("%inner_merge") < pos("%outer_merge"));
        assert!(pos("%outer_then") < pos("%inner_merge"));
    }

    #[test]
    fn loop_exit_dominated_by_body_guard_emits_after_body() {
        let blocks = vec![
            bb("%entry", "br label %h"),
            bb("%h", "br label %b1"),
            bb("%b1", "br i1 %c, label %b2, label %exit"),
            bb("%exit", "ret void"),
            bb("%b2", "br label %h"),
        ];
        let order = order_of(&blocks, &[("%h", "%exit")]);
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap();
        assert!(
            pos("%exit") > pos("%b2"),
            "loop exit must follow the latch (whole body) — got {order:?}"
        );
        assert!(
            pos("%exit") > pos("%b1"),
            "loop exit must follow its dominating guard — got {order:?}"
        );
        assert_eq!(order[0], "%entry");
    }

    #[test]
    fn terminal_order_closes_selection_before_shared_deferred_loop_exit() {
        let blocks = vec![
            bb("%entry", "br label %loop.header"),
            bb("%loop.header", "br label %guard"),
            bb(
                "%guard",
                "br i1 %break_now, label %loop.exit, label %selection.merge",
            ),
            bb("%loop.exit", "ret void"),
            bb("%selection.merge", "br label %loop.latch"),
            bb("%loop.latch", "br label %loop.header"),
        ];
        let merges = [
            ("%loop.header", "%loop.exit"),
            ("%guard", "%selection.merge"),
        ];
        let ordinary = order_of(&blocks, &merges);
        let terminal = terminal_order_of(&blocks, &merges);
        let pos =
            |order: &[String], name: &str| order.iter().position(|block| block == name).unwrap();
        assert!(pos(&ordinary, "%loop.exit") < pos(&ordinary, "%selection.merge"));
        assert!(pos(&terminal, "%selection.merge") < pos(&terminal, "%loop.exit"));
    }

    #[test]
    fn order_is_a_permutation() {
        let blocks = vec![
            bb("%entry", "br i1 %c, label %a, label %b"),
            bb("%a", "br label %m"),
            bb("%b", "br label %m"),
            bb("%m", "ret void"),
        ];
        let order = order_of(&blocks, &[("%entry", "%m")]);
        let mut sorted = order.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), blocks.len());
        assert_eq!(order[0], "%entry");
    }
}
