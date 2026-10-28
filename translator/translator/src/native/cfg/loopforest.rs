use super::blocks::block_successors;
use super::graph::{Cfg, Dominators};
use super::BodyBlock;
use crate::fxhash::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::native) struct NaturalLoop {
    pub(in crate::native) header: String,
    pub(in crate::native) body: Vec<String>,
    pub(in crate::native) latches: Vec<String>,
    pub(in crate::native) exits: Vec<String>,
    pub(in crate::native) parent: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(in crate::native) struct LoopForest {
    pub(in crate::native) loops: Vec<NaturalLoop>,
    doms: Dominators,
}

impl LoopForest {
    pub(in crate::native) fn dominates(&self, dominator: &str, node: &str) -> bool {
        self.doms.dominates(dominator, node)
    }

    pub(in crate::native) fn record_pass_through(&mut self, name: &str, predecessors: &[String]) {
        self.doms.record_pass_through(name, predecessors);
    }

    pub(in crate::native) fn loop_for_header(&self, header: &str) -> Option<&NaturalLoop> {
        self.loops.iter().find(|l| l.header == header)
    }

    pub(in crate::native) fn idom(&self, node: &str) -> Option<&str> {
        self.doms.idom(node)
    }

    pub(in crate::native) fn structured_plan(&self) -> Vec<LoopPlan> {
        self.structured_plan_ignoring_exits(&HashSet::default())
    }

    pub(in crate::native) fn structured_plan_ignoring_exits(
        &self,
        ignored: &HashSet<String>,
    ) -> Vec<LoopPlan> {
        let all_latches: HashSet<&str> = self
            .loops
            .iter()
            .flat_map(|l| l.latches.iter().map(String::as_str))
            .collect();
        self.loops
            .iter()
            .map(|l| {
                let live_exits = l
                    .exits
                    .iter()
                    .filter(|exit| !ignored.contains(*exit))
                    .cloned()
                    .collect::<Vec<_>>();
                let exits = if live_exits.is_empty() {
                    &l.exits
                } else {
                    &live_exits
                };
                let mut restructure = Vec::new();
                if l.latches.len() > 1 {
                    restructure.push(Restructure::MultipleLatches);
                }
                if exits.len() > 1 {
                    restructure.push(Restructure::MultipleExits);
                }
                if let [exit] = exits.as_slice() {
                    if all_latches.contains(exit.as_str()) {
                        restructure.push(Restructure::MergeIsEnclosingContinue);
                    }
                }
                if exits.is_empty() {
                    restructure.push(Restructure::NoExit);
                }
                LoopPlan {
                    header: l.header.clone(),
                    continue_block: l.latches.first().cloned(),
                    merge_block: exits.first().cloned(),
                    restructure,
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::native) struct LoopPlan {
    pub(in crate::native) header: String,
    pub(in crate::native) continue_block: Option<String>,
    pub(in crate::native) merge_block: Option<String>,
    pub(in crate::native) restructure: Vec<Restructure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native) enum Restructure {
    MultipleLatches,
    MultipleExits,
    MergeIsEnclosingContinue,
    NoExit,
}

pub(in crate::native) fn analyze(blocks: &[BodyBlock]) -> LoopForest {
    let cfg = match Cfg::from_blocks(blocks) {
        Some(cfg) => cfg,
        None => return LoopForest::default(),
    };
    let doms = cfg.dominators();

    let mut forest = LoopForest {
        loops: Vec::new(),
        doms,
    };

    let mut latches_by_header: HashMap<String, Vec<String>> = HashMap::default();
    for b in blocks {
        for t in cfg.successors.get(&b.name).into_iter().flatten() {
            if cfg.contains(t) && forest.dominates(t, &b.name) {
                latches_by_header
                    .entry(t.clone())
                    .or_default()
                    .push(b.name.clone());
            }
        }
    }

    let mut loops: Vec<NaturalLoop> = latches_by_header
        .into_iter()
        .map(|(header, mut latches)| {
            latches.sort();
            latches.dedup();
            let body = natural_loop_body(&header, &latches, &cfg.predecessors);
            let body_set: HashSet<&str> = body.iter().map(String::as_str).collect();
            let mut exits = Vec::new();
            for n in &body {
                for t in cfg.successors.get(n).into_iter().flatten() {
                    if cfg.contains(t) && !body_set.contains(t.as_str()) {
                        exits.push(t.clone());
                    }
                }
            }
            exits.sort();
            exits.dedup();
            NaturalLoop {
                header,
                body,
                latches,
                exits,
                parent: None,
            }
        })
        .collect();

    let bodies: Vec<HashSet<String>> = loops
        .iter()
        .map(|l| l.body.iter().cloned().collect())
        .collect();
    let headers: Vec<String> = loops.iter().map(|l| l.header.clone()).collect();
    let parents: Vec<Option<String>> = (0..loops.len())
        .map(|i| {
            let mut best: Option<usize> = None;
            for j in 0..loops.len() {
                if i == j || headers[i] == headers[j] {
                    continue;
                }
                if bodies[j].contains(&headers[i])
                    && bodies[i].is_subset(&bodies[j])
                    && bodies[i].len() < bodies[j].len()
                {
                    match best {
                        Some(b) if bodies[b].len() <= bodies[j].len() => {}
                        _ => best = Some(j),
                    }
                }
            }
            best.map(|j| headers[j].clone())
        })
        .collect();
    for (i, parent) in parents.into_iter().enumerate() {
        loops[i].parent = parent;
    }

    loops.sort_by(|a, b| a.header.cmp(&b.header));
    forest.loops = loops;
    forest
}

pub(in crate::native) fn analyze_reusing_natural_loops(
    blocks: &[BodyBlock],
    loops: &[NaturalLoop],
) -> LoopForest {
    let Some(cfg) = Cfg::from_blocks(blocks) else {
        return LoopForest::default();
    };
    LoopForest {
        loops: loops.to_vec(),
        doms: cfg.dominators(),
    }
}

fn natural_loop_body(
    header: &str,
    latches: &[String],
    predecessors: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut body: HashSet<String> = crate::fxhash::set_from([header.to_string()]);
    let mut stack: Vec<String> = latches.to_vec();
    for l in latches {
        body.insert(l.clone());
    }
    while let Some(n) = stack.pop() {
        if n == header {
            continue;
        }
        for pred in predecessors.get(&n).into_iter().flatten() {
            if body.insert(pred.clone()) {
                stack.push(pred.clone());
            }
        }
    }
    let mut body: Vec<String> = body.into_iter().collect();
    body.sort();
    body
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::native) struct IrreducibleRegion {
    pub(in crate::native) nodes: Vec<String>,
    pub(in crate::native) entries: Vec<String>,
}

pub(in crate::native) fn irreducible_regions(blocks: &[BodyBlock]) -> Vec<IrreducibleRegion> {
    let cfg = match Cfg::from_blocks(blocks) {
        Some(cfg) => cfg,
        None => return Vec::new(),
    };
    let succ = &cfg.successors;
    let reachable = cfg.reachable_from(&cfg.entry);

    let mut preds: HashMap<String, Vec<String>> = HashMap::default();
    for n in &reachable {
        for t in succ.get(n).into_iter().flatten() {
            if reachable.contains(t) {
                preds.entry(t.clone()).or_default().push(n.clone());
            }
        }
    }

    let mut visited: HashSet<String> = HashSet::default();
    let mut finish_order: Vec<String> = Vec::new();
    for start in blocks
        .iter()
        .map(|b| &b.name)
        .filter(|n| reachable.contains(*n))
    {
        if visited.contains(start) {
            continue;
        }
        let mut dfs: Vec<(String, usize)> = vec![(start.clone(), 0)];
        visited.insert(start.clone());
        while let Some((node, ci)) = dfs.last().cloned() {
            let next = succ
                .get(&node)
                .and_then(|s| s.iter().filter(|t| reachable.contains(*t)).nth(ci));
            match next {
                Some(child) => {
                    dfs.last_mut().unwrap().1 += 1;
                    if visited.insert(child.clone()) {
                        dfs.push((child.clone(), 0));
                    }
                }
                None => {
                    finish_order.push(node);
                    dfs.pop();
                }
            }
        }
    }

    let mut assigned: HashSet<String> = HashSet::default();
    let mut regions: Vec<IrreducibleRegion> = Vec::new();
    for root in finish_order.iter().rev() {
        if assigned.contains(root) {
            continue;
        }
        let mut scc: Vec<String> = Vec::new();
        let mut st = vec![root.clone()];
        assigned.insert(root.clone());
        while let Some(n) = st.pop() {
            scc.push(n.clone());
            for p in preds.get(&n).into_iter().flatten() {
                if assigned.insert(p.clone()) {
                    st.push(p.clone());
                }
            }
        }

        let scc_set: HashSet<&str> = scc.iter().map(String::as_str).collect();
        let is_cycle = scc.len() > 1
            || succ
                .get(&scc[0])
                .is_some_and(|s| s.iter().any(|t| t == &scc[0]));
        if !is_cycle {
            continue;
        }

        let mut entries: Vec<String> = scc
            .iter()
            .filter(|n| {
                **n == cfg.entry
                    || preds
                        .get(*n)
                        .into_iter()
                        .flatten()
                        .any(|p| !scc_set.contains(p.as_str()))
            })
            .cloned()
            .collect();
        if entries.len() > 1 {
            entries.sort();
            let mut nodes = scc;
            nodes.sort();
            regions.push(IrreducibleRegion { nodes, entries });
        }
    }
    regions.sort_by(|a, b| a.nodes.first().cmp(&b.nodes.first()));
    regions
}

const VIRTUAL_EXIT: &str = "@@virtual_exit";

pub(in crate::native) fn post_idom(blocks: &[BodyBlock]) -> HashMap<String, String> {
    post_idom_cut(blocks, &HashSet::default())
}

fn post_idom_cut(blocks: &[BodyBlock], cut: &HashSet<(String, String)>) -> HashMap<String, String> {
    let cfg = match Cfg::from_blocks(blocks) {
        Some(cfg) => cfg,
        None => return HashMap::default(),
    };
    let fsucc: HashMap<String, Vec<String>> = cfg
        .successors
        .iter()
        .map(|(n, ts)| {
            let kept: Vec<String> = ts
                .iter()
                .filter(|t| !cut.contains(&(n.clone(), (*t).clone())))
                .cloned()
                .collect();
            (n.clone(), kept)
        })
        .collect();

    let mut reachable: HashSet<String> = HashSet::default();
    let mut stack = vec![cfg.entry.clone()];
    reachable.insert(cfg.entry.clone());
    while let Some(n) = stack.pop() {
        for t in fsucc.get(&n).into_iter().flatten() {
            if reachable.insert(t.clone()) {
                stack.push(t.clone());
            }
        }
    }

    let exits: Vec<String> = reachable
        .iter()
        .filter(|n| fsucc.get(*n).map(|s| s.is_empty()).unwrap_or(true))
        .cloned()
        .collect();
    if exits.is_empty() {
        return HashMap::default();
    }

    let mut rsucc: HashMap<String, Vec<String>> = HashMap::default();
    rsucc.insert(VIRTUAL_EXIT.to_string(), exits.clone());
    for n in &reachable {
        for t in fsucc.get(n).into_iter().flatten() {
            if reachable.contains(t) {
                rsucc.entry(t.clone()).or_default().push(n.clone());
            }
        }
    }
    for e in &exits {
        rsucc
            .entry(e.clone())
            .or_default()
            .push(VIRTUAL_EXIT.to_string());
    }

    let mut order = vec![VIRTUAL_EXIT.to_string()];
    order.extend(
        reachable
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
    );
    let (ridom, _) = super::graph::named_dominators(&order, |name| {
        rsucc.get(name).map(Vec::as_slice).unwrap_or(&[])
    });

    ridom
        .into_iter()
        .filter(|(n, d)| n != VIRTUAL_EXIT && d != VIRTUAL_EXIT && n != d)
        .collect()
}

pub(in crate::native) fn selection_merges(
    blocks: &[BodyBlock],
    forest: &LoopForest,
) -> HashMap<String, String> {
    let unreachable_targets: HashSet<&str> = blocks
        .iter()
        .filter(|block| {
            block.typed.as_ref().is_some_and(|typed| {
                typed.insts.is_empty()
                    && matches!(
                        typed.terminator,
                        crate::native::tir::TirTerminator::Unreachable
                    )
            })
        })
        .map(|block| block.name.as_str())
        .collect();
    let mut terminal_switch_edges = HashSet::default();
    for block in blocks {
        let is_switch = block.typed.as_ref().is_some_and(|typed| {
            matches!(
                typed.terminator,
                crate::native::tir::TirTerminator::Switch { .. }
            )
        });
        if !is_switch {
            continue;
        }
        for target in block_successors(block) {
            if unreachable_targets.contains(target.as_str()) {
                terminal_switch_edges.insert((block.name.clone(), target));
            }
        }
    }
    let pidom = if terminal_switch_edges.is_empty() {
        post_idom(blocks)
    } else {
        post_idom_cut(blocks, &terminal_switch_edges)
    };
    selection_merges_from_pidom(blocks, forest, &pidom)
}

pub(in crate::native) fn break_aware_selection_merges(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    loop_merges: &HashMap<String, super::LoopMergeInfo>,
) -> HashMap<String, String> {
    let cfg = match Cfg::from_blocks(blocks) {
        Some(cfg) => cfg,
        None => return HashMap::default(),
    };
    let mut cut: HashSet<(String, String)> = HashSet::default();
    for l in &forest.loops {
        let Some(info) = loop_merges.get(&l.header) else {
            continue;
        };
        for n in &l.body {
            if n == &l.header {
                continue;
            }
            let succs: Vec<&str> = cfg
                .successors
                .get(n)
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect();
            if !succs.iter().any(|s| *s == info.merge) {
                continue;
            }
            let is_structural_exit = succs
                .iter()
                .any(|s| *s == info.continue_target || *s == l.header);
            if is_structural_exit {
                continue;
            }
            cut.insert((n.clone(), info.merge.clone()));
        }
    }
    selection_merges_from_pidom(blocks, forest, &post_idom_cut(blocks, &cut))
}

fn selection_merges_from_pidom(
    blocks: &[BodyBlock],
    forest: &LoopForest,
    pidom: &HashMap<String, String>,
) -> HashMap<String, String> {
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let block_names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    let mut merges = HashMap::default();
    for b in blocks {
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let distinct: HashSet<String> = block_successors(b)
            .into_iter()
            .filter(|successor| block_names.contains(successor.as_str()))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        if let Some(merge) = pidom.get(&b.name) {
            merges.insert(b.name.clone(), merge.clone());
        }
    }
    merges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blk(name: &str, term: &str) -> BodyBlock {
        let name = format!("%{name}");
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

    #[test]
    fn single_self_loop() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %h, label %x"),
            blk("x", "ret void"),
        ];
        let f = analyze(&blocks);
        assert_eq!(f.loops.len(), 1);
        let l = &f.loops[0];
        assert_eq!(l.header, "%h");
        assert_eq!(l.latches, vec!["%h".to_string()]);
        assert_eq!(l.exits, vec!["%x".to_string()]);
        assert_eq!(l.parent, None);
    }

    #[test]
    fn simple_loop_with_body() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %b, label %x"),
            blk("b", "br label %h"),
            blk("x", "ret void"),
        ];
        let f = analyze(&blocks);
        assert_eq!(f.loops.len(), 1);
        let l = f.loop_for_header("%h").unwrap();
        assert_eq!(l.latches, vec!["%b".to_string()]);
        assert!(l.body.contains(&"%h".to_string()) && l.body.contains(&"%b".to_string()));
        assert_eq!(l.exits, vec!["%x".to_string()]);
    }

    #[test]
    fn dominance_is_correct() {
        let blocks = vec![
            blk("0", "br i1 %c, label %a, label %b"),
            blk("a", "br label %m"),
            blk("b", "br label %m"),
            blk("m", "ret void"),
        ];
        let f = analyze(&blocks);
        assert!(f.dominates("%0", "%m"));
        assert!(f.dominates("%0", "%a"));
        assert!(!f.dominates("%a", "%m"));
        assert!(f.dominates("%m", "%m"));
    }

    #[test]
    fn nested_loops_form_a_forest() {
        let blocks = vec![
            blk("0", "br label %H"),
            blk("H", "br label %G"),
            blk("G", "br i1 %c, label %body, label %L"),
            blk("body", "br label %G"),
            blk("L", "br i1 %d, label %H, label %X"),
            blk("X", "ret void"),
        ];
        let f = analyze(&blocks);
        assert_eq!(f.loops.len(), 2, "{:?}", f.loops);
        let outer = f.loop_for_header("%H").unwrap();
        let inner = f.loop_for_header("%G").unwrap();
        assert_eq!(outer.parent, None);
        assert_eq!(inner.parent, Some("%H".to_string()));
        let outer_body: HashSet<&str> = outer.body.iter().map(String::as_str).collect();
        for n in &inner.body {
            assert!(
                outer_body.contains(n.as_str()),
                "inner {n} not in outer body"
            );
        }
    }

    #[test]
    fn inner_merge_equals_outer_continue_is_detectable() {
        let blocks = vec![
            blk("0", "br label %H"),
            blk("H", "br label %G"),
            blk("G", "br i1 %c, label %b, label %latch"),
            blk("b", "br label %G"),
            blk("latch", "br i1 %d, label %H, label %X"),
            blk("X", "ret void"),
        ];
        let f = analyze(&blocks);
        let inner = f.loop_for_header("%G").unwrap();
        let outer = f.loop_for_header("%H").unwrap();
        assert!(inner.exits.contains(&"%latch".to_string()));
        assert!(outer.latches.contains(&"%latch".to_string()));
    }

    #[test]
    fn structured_plan_marks_simple_loop_directly_structurable() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %b, label %x"),
            blk("b", "br label %h"),
            blk("x", "ret void"),
        ];
        let plan = analyze(&blocks).structured_plan();
        assert_eq!(plan.len(), 1);
        assert!(plan[0].restructure.is_empty(), "{:?}", plan[0]);
        assert_eq!(plan[0].continue_block.as_deref(), Some("%b"));
        assert_eq!(plan[0].merge_block.as_deref(), Some("%x"));
    }

    #[test]
    fn structured_plan_flags_merge_is_enclosing_continue() {
        let blocks = vec![
            blk("0", "br label %H"),
            blk("H", "br label %G"),
            blk("G", "br i1 %c, label %b, label %latch"),
            blk("b", "br label %G"),
            blk("latch", "br i1 %d, label %H, label %X"),
            blk("X", "ret void"),
        ];
        let plan = analyze(&blocks).structured_plan();
        let inner = plan.iter().find(|p| p.header == "%G").unwrap();
        assert!(
            inner
                .restructure
                .contains(&Restructure::MergeIsEnclosingContinue),
            "inner loop should be flagged for split: {inner:?}"
        );
        let outer = plan.iter().find(|p| p.header == "%H").unwrap();
        assert!(
            outer.restructure.is_empty(),
            "outer loop is directly structurable: {outer:?}"
        );
    }

    #[test]
    fn structured_plan_flags_multiple_exits() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %body, label %x1"),
            blk("body", "br i1 %d, label %h, label %x2"),
            blk("x1", "ret void"),
            blk("x2", "ret void"),
        ];
        let plan = analyze(&blocks).structured_plan();
        let l = plan.iter().find(|p| p.header == "%h").unwrap();
        assert!(
            l.restructure.contains(&Restructure::MultipleExits),
            "two exits should be flagged: {l:?}"
        );
    }

    #[test]
    fn irreducible_back_edge_is_not_a_natural_loop() {
        let blocks = vec![
            blk("0", "br i1 %c, label %a, label %b"),
            blk("a", "br label %b"),
            blk("b", "br i1 %d, label %a, label %x"),
            blk("x", "ret void"),
        ];
        let f = analyze(&blocks);
        assert_eq!(
            f.loops.len(),
            0,
            "irreducible cycle must not be a natural loop: {:?}",
            f.loops
        );
    }

    #[test]
    fn irreducible_regions_empty_for_reducible_loop() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %b, label %x"),
            blk("b", "br label %h"),
            blk("x", "ret void"),
        ];
        assert!(
            irreducible_regions(&blocks).is_empty(),
            "a reducible loop has no irreducible region"
        );
    }

    #[test]
    fn irreducible_regions_empty_for_nested_reducible_loops() {
        let blocks = vec![
            blk("0", "br label %H"),
            blk("H", "br label %G"),
            blk("G", "br i1 %c, label %body, label %L"),
            blk("body", "br label %G"),
            blk("L", "br i1 %d, label %H, label %X"),
            blk("X", "ret void"),
        ];
        assert!(
            irreducible_regions(&blocks).is_empty(),
            "nested reducible loops have no irreducible region"
        );
    }

    #[test]
    fn irreducible_regions_detects_multi_entry_cycle() {
        let blocks = vec![
            blk("0", "br i1 %c, label %a, label %b"),
            blk("a", "br label %b"),
            blk("b", "br i1 %d, label %a, label %x"),
            blk("x", "ret void"),
        ];
        let regions = irreducible_regions(&blocks);
        assert_eq!(regions.len(), 1, "{regions:?}");
        assert_eq!(regions[0].nodes, vec!["%a".to_string(), "%b".to_string()]);
        assert_eq!(
            regions[0].entries,
            vec!["%a".to_string(), "%b".to_string()],
            "both cycle nodes are entered from outside"
        );
    }

    #[test]
    fn irreducible_regions_ignores_single_entry_scc() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br label %b"),
            blk("b", "br i1 %d, label %h, label %x"),
            blk("x", "ret void"),
        ];
        assert!(
            irreducible_regions(&blocks).is_empty(),
            "single-entry SCC is reducible"
        );
    }

    #[test]
    fn selection_merge_is_immediate_post_dominator_of_diamond() {
        let blocks = vec![
            blk("H", "br i1 %c, label %a, label %b"),
            blk("a", "br label %m"),
            blk("b", "br label %m"),
            blk("m", "ret void"),
        ];
        let forest = analyze(&blocks);
        let merges = selection_merges(&blocks, &forest);
        assert_eq!(merges.get("%H").map(String::as_str), Some("%m"));
        assert!(!merges.contains_key("%a"));
    }

    #[test]
    fn selection_merge_nested_if() {
        let blocks = vec![
            blk("H", "br i1 %c, label %G, label %e"),
            blk("G", "br i1 %d, label %g1, label %g2"),
            blk("g1", "br label %gm"),
            blk("g2", "br label %gm"),
            blk("gm", "br label %m"),
            blk("e", "br label %m"),
            blk("m", "ret void"),
        ];
        let forest = analyze(&blocks);
        let merges = selection_merges(&blocks, &forest);
        assert_eq!(merges.get("%H").map(String::as_str), Some("%m"));
        assert_eq!(merges.get("%G").map(String::as_str), Some("%gm"));
    }

    #[test]
    fn switch_merge_ignores_terminal_unreachable_arm() {
        let blocks = vec![
            blk(
                "sw",
                "switch i32 %s, label %dead [ i32 0, label %a i32 1, label %b ]",
            ),
            blk("a", "br label %m"),
            blk("b", "br label %m"),
            blk("dead", "unreachable"),
            blk("m", "ret void"),
        ];
        let forest = analyze(&blocks);
        let merges = selection_merges(&blocks, &forest);
        assert_eq!(merges.get("%sw").map(String::as_str), Some("%m"));
    }

    #[test]
    fn selection_merge_skips_loop_header() {
        let blocks = vec![
            blk("0", "br label %h"),
            blk("h", "br i1 %c, label %b, label %x"),
            blk("b", "br label %h"),
            blk("x", "ret void"),
        ];
        let forest = analyze(&blocks);
        let merges = selection_merges(&blocks, &forest);
        assert!(
            !merges.contains_key("%h"),
            "loop header must not get a selection merge: {merges:?}"
        );
    }

    #[test]
    fn break_aware_selection_merge_moves_guarded_break_off_loop_merge() {
        let blocks = vec![
            blk("entry", "br label %h"),
            blk("h", "br label %S"),
            blk("S", "br i1 %c, label %body, label %m"),
            blk("body", "br label %latch"),
            blk("latch", "br i1 %d, label %h, label %m"),
            blk("m", "ret void"),
        ];
        let forest = analyze(&blocks);
        let plain = selection_merges(&blocks, &forest);
        assert_eq!(
            plain.get("%S").map(String::as_str),
            Some("%m"),
            "plain: {plain:?}"
        );
        let mut loop_merges = HashMap::default();
        loop_merges.insert(
            "%h".to_string(),
            super::super::LoopMergeInfo {
                merge: "%m".to_string(),
                continue_target: "%latch".to_string(),
            },
        );
        let ba = break_aware_selection_merges(&blocks, &forest, &loop_merges);
        assert_eq!(
            ba.get("%S").map(String::as_str),
            Some("%body"),
            "break-aware S should reconverge at its non-break arm: {ba:?}"
        );
        assert_eq!(ba.get("%latch").map(String::as_str), Some("%m"), "{ba:?}");
    }

    #[test]
    fn post_idom_handles_multiple_returns() {
        let blocks = vec![
            blk("H", "br i1 %c, label %a, label %b"),
            blk("a", "ret void"),
            blk("b", "ret void"),
        ];
        let pidom = post_idom(&blocks);
        assert!(!pidom.contains_key("%H"), "{pidom:?}");
        let forest = analyze(&blocks);
        assert!(selection_merges(&blocks, &forest).is_empty());
    }
}
