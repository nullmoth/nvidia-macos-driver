use super::blocks::block_successors;
use super::BodyBlock;
use crate::fxhash::{HashMap, HashSet};

#[cfg(test)]
thread_local! {
    static CFG_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn cfg_builds_bump() {
    CFG_BUILDS.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
pub(in crate::native) fn cfg_builds_during<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = CFG_BUILDS.with(std::cell::Cell::get);
    let value = work();
    (value, CFG_BUILDS.with(std::cell::Cell::get) - before)
}

pub(in crate::native) struct Cfg {
    pub(in crate::native) entry: String,
    pub(in crate::native) successors: HashMap<String, Vec<String>>,
    pub(in crate::native) predecessors: HashMap<String, Vec<String>>,
    names: HashSet<String>,
    order: Vec<String>,
}

impl Cfg {
    pub(in crate::native) fn from_blocks(blocks: &[BodyBlock]) -> Option<Cfg> {
        #[cfg(test)]
        cfg_builds_bump();
        let entry = blocks.first()?.name.clone();
        let names: HashSet<String> = blocks.iter().map(|b| b.name.clone()).collect();
        let successors: HashMap<String, Vec<String>> = blocks
            .iter()
            .map(|b| {
                let s = block_successors(b)
                    .into_iter()
                    .filter(|t| names.contains(t.as_str()))
                    .collect();
                (b.name.clone(), s)
            })
            .collect();
        let mut predecessors: HashMap<String, Vec<String>> = HashMap::default();
        for b in blocks {
            for t in successors.get(&b.name).into_iter().flatten() {
                predecessors
                    .entry(t.clone())
                    .or_default()
                    .push(b.name.clone());
            }
        }
        Some(Cfg {
            entry,
            successors,
            predecessors,
            names,
            order: blocks.iter().map(|b| b.name.clone()).collect(),
        })
    }

    pub(in crate::native) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub(in crate::native) fn reachable_from(&self, start: &str) -> HashSet<String> {
        let mut reachable: HashSet<String> = HashSet::default();
        let mut stack = vec![start.to_string()];
        reachable.insert(start.to_string());
        while let Some(n) = stack.pop() {
            for t in self.successors.get(&n).into_iter().flatten() {
                if reachable.insert(t.clone()) {
                    stack.push(t.clone());
                }
            }
        }
        reachable
    }

    pub(in crate::native) fn dominators(&self) -> Dominators {
        let (idom, intervals) = named_dominators(&self.order, |name| {
            self.successors.get(name).map(Vec::as_slice).unwrap_or(&[])
        });
        Dominators {
            idom,
            intervals,
            pass_throughs: HashMap::default(),
        }
    }
}

pub(in crate::native) fn block_dominators(blocks: &[BodyBlock]) -> Dominators {
    Cfg::from_blocks(blocks)
        .map(|cfg| cfg.dominators())
        .unwrap_or_default()
}

pub(super) fn named_dominators<'a>(
    order: &'a [String],
    successors: impl Fn(&str) -> &'a [String],
) -> (HashMap<String, String>, HashMap<String, (usize, usize)>) {
    let index: HashMap<&str, usize> = order
        .iter()
        .enumerate()
        .map(|(position, name)| (name.as_str(), position))
        .collect();
    let dense: Vec<Vec<usize>> = order
        .iter()
        .map(|name| {
            successors(name)
                .iter()
                .filter_map(|target| index.get(target.as_str()).copied())
                .collect()
        })
        .collect();
    let predecessors = crate::dominators::build_predecessors(&dense);
    let (_, intervals, parents) = crate::dominators::dominance(&dense, &predecessors);
    let named_idom = parents
        .iter()
        .enumerate()
        .filter_map(|(block, parent)| Some((order[block].clone(), order[(*parent)?].clone())))
        .collect();
    let named_intervals = intervals
        .iter()
        .enumerate()
        .filter_map(|(block, interval)| Some((order[block].clone(), (*interval)?)))
        .collect();
    (named_idom, named_intervals)
}

#[derive(Clone, Debug, Default)]
pub(in crate::native) struct Dominators {
    idom: HashMap<String, String>,
    intervals: HashMap<String, (usize, usize)>,
    pass_throughs: HashMap<String, Vec<String>>,
}

impl Dominators {
    pub(in crate::native) fn dominates(&self, dominator: &str, node: &str) -> bool {
        if dominator == node {
            return true;
        }
        if !self.pass_throughs.is_empty() {
            debug_assert!(
                !self.pass_throughs.contains_key(dominator),
                "a pass-through is never a split target, so nothing asks what it dominates"
            );
            if let Some(originals) = self.pass_throughs.get(node) {
                return !originals.is_empty()
                    && originals
                        .iter()
                        .all(|original| self.dominates_derived(dominator, original));
            }
        }
        self.dominates_derived(dominator, node)
    }

    fn dominates_derived(&self, dominator: &str, node: &str) -> bool {
        if dominator == node {
            return true;
        }
        let (Some(&(dom_in, dom_out)), Some(&(node_in, node_out))) =
            (self.intervals.get(dominator), self.intervals.get(node))
        else {
            return false;
        };
        dom_in <= node_in && node_out <= dom_out
    }

    pub(in crate::native) fn record_pass_through(&mut self, name: &str, predecessors: &[String]) {
        let mut derived = Vec::new();
        for predecessor in predecessors {
            match self.pass_throughs.get(predecessor) {
                Some(resolved) => derived.extend(resolved.iter().cloned()),
                None => derived.push(predecessor.clone()),
            }
        }
        derived.sort();
        derived.dedup();
        self.pass_throughs.insert(name.to_string(), derived);
    }

    pub(in crate::native) fn idom(&self, node: &str) -> Option<&str> {
        self.idom
            .get(node)
            .map(String::as_str)
            .filter(|d| *d != node)
    }
}

pub(super) fn reachable_from(
    start: &str,
    successors: &HashMap<String, Vec<String>>,
) -> HashSet<String> {
    let mut seen: HashSet<&str> = HashSet::default();
    let mut stack: Vec<&str> = vec![start];
    while let Some(block) = stack.pop() {
        if !seen.insert(block) {
            continue;
        }
        if let Some(next) = successors.get(block) {
            stack.extend(next.iter().map(String::as_str));
        }
    }
    seen.into_iter().map(str::to_string).collect()
}

pub(in crate::native) use crate::spirv_module::block_successors_by_label as spirv_block_successors_by_label;

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

    fn oracle_dominates(cfg: &Cfg, dominator: &str, node: &str) -> bool {
        if dominator == node {
            return true;
        }
        let mut seen: HashSet<&str> = HashSet::default();
        let mut stack: Vec<&str> = vec![cfg.entry.as_str()];
        if cfg.entry == dominator {
            return true;
        }
        seen.insert(cfg.entry.as_str());
        while let Some(cur) = stack.pop() {
            for t in cfg.successors.get(cur).into_iter().flatten() {
                if !cfg.contains(t) || t == dominator {
                    continue;
                }
                if seen.insert(t.as_str()) {
                    stack.push(t.as_str());
                }
            }
        }
        !seen.contains(node)
    }

    fn reachable_names(cfg: &Cfg) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::default();
        let mut stack = vec![cfg.entry.as_str()];
        seen.insert(cfg.entry.as_str());
        while let Some(cur) = stack.pop() {
            for t in cfg.successors.get(cur).into_iter().flatten() {
                if cfg.contains(t) && seen.insert(t.as_str()) {
                    stack.push(t.as_str());
                }
            }
        }
        let mut v: Vec<String> = seen.into_iter().map(str::to_string).collect();
        v.sort();
        v
    }

    fn assert_dominance_matches_oracle(blocks: &[BodyBlock]) {
        let cfg = Cfg::from_blocks(blocks).expect("non-empty");
        let doms = cfg.dominators();
        let reachable = reachable_names(&cfg);
        for d in &reachable {
            for n in &reachable {
                assert_eq!(
                    doms.dominates(d, n),
                    oracle_dominates(&cfg, d, n),
                    "dominance mismatch: does {d} dominate {n}?"
                );
            }
        }
    }

    #[test]
    fn straight_line_chain() {
        let blocks = vec![
            blk("0", "br label %a"),
            blk("a", "br label %b"),
            blk("b", "ret void"),
        ];
        let cfg = Cfg::from_blocks(&blocks).unwrap();
        let doms = cfg.dominators();
        assert!(doms.dominates("%0", "%b"));
        assert!(doms.dominates("%a", "%b"));
        assert!(!doms.dominates("%b", "%a"));
        assert_eq!(doms.idom("%b"), Some("%a"));
        assert_eq!(doms.idom("%0"), None);
        assert_dominance_matches_oracle(&blocks);
    }

    #[test]
    fn diamond_merge_dominance() {
        let blocks = vec![
            blk("0", "br i1 %c, label %t, label %f"),
            blk("t", "br label %m"),
            blk("f", "br label %m"),
            blk("m", "ret void"),
        ];
        let cfg = Cfg::from_blocks(&blocks).unwrap();
        let doms = cfg.dominators();
        assert!(doms.dominates("%0", "%m"));
        assert!(!doms.dominates("%t", "%m"));
        assert!(!doms.dominates("%f", "%m"));
        assert_eq!(doms.idom("%m"), Some("%0"));
        assert_dominance_matches_oracle(&blocks);
    }

    #[test]
    fn self_loop_and_nested_loop_dominance() {
        let blocks = vec![
            blk("0", "br label %o"),
            blk("o", "br i1 %c0, label %i, label %x"),
            blk("i", "br i1 %c1, label %i, label %ol"),
            blk("ol", "br i1 %c2, label %o, label %x"),
            blk("x", "ret void"),
        ];
        let cfg = Cfg::from_blocks(&blocks).unwrap();
        let doms = cfg.dominators();
        assert!(doms.dominates("%o", "%i"));
        assert!(doms.dominates("%o", "%ol"));
        assert!(doms.dominates("%o", "%x"));
        assert!(doms.dominates("%i", "%ol"));
        assert!(!doms.dominates("%ol", "%i"));
        assert_dominance_matches_oracle(&blocks);
    }

    #[test]
    fn edges_to_missing_labels_are_dropped() {
        let blocks = vec![
            blk("0", "br i1 %c, label %a, label %ghost"),
            blk("a", "ret void"),
        ];
        let cfg = Cfg::from_blocks(&blocks).unwrap();
        assert!(cfg.contains("%a"));
        assert!(!cfg.contains("%ghost"));
        assert_eq!(
            cfg.successors.get("%0").map(Vec::as_slice),
            Some(&["%a".to_string()][..])
        );
        assert!(!cfg.predecessors.contains_key("%ghost"));
        assert_dominance_matches_oracle(&blocks);
    }

    #[test]
    fn reachable_from_covers_forward_edges_only() {
        let blocks = vec![
            blk("0", "br i1 %c, label %t, label %f"),
            blk("t", "br label %m"),
            blk("f", "br label %m"),
            blk("m", "ret void"),
            blk("dead", "br label %m"),
        ];
        let cfg = Cfg::from_blocks(&blocks).unwrap();
        let r = cfg.reachable_from("%0");
        assert!(r.contains("%0"));
        assert!(r.contains("%t") && r.contains("%f") && r.contains("%m"));
        assert!(!r.contains("%dead"));
        assert_eq!(
            cfg.reachable_from("%m"),
            crate::fxhash::set_from(["%m".to_string()])
        );
    }

    #[test]
    fn empty_function_has_no_cfg() {
        assert!(Cfg::from_blocks(&[]).is_none());
    }

    fn splice_pass_through(
        blocks: &mut Vec<BodyBlock>,
        name: &str,
        target: &str,
        predecessors: &[&str],
    ) {
        for block in blocks.iter_mut() {
            if !predecessors.contains(&block.name.as_str()) {
                continue;
            }
            let typed = block.typed_mut().expect("carrier");
            typed.redirect_successor(target, name);
        }
        let at = blocks
            .iter()
            .position(|block| block.name == target)
            .expect("target block");
        let mut spliced = blk(name.trim_start_matches('%'), &format!("br label {target}"));
        spliced.name = name.to_string();
        blocks.insert(at, spliced);
    }

    #[test]
    fn a_recorded_pass_through_answers_what_a_fresh_analysis_answers() {
        let mut blocks = vec![
            blk("0", "br i1 %c, label %pre, label %h"),
            blk("pre", "br label %m"),
            blk("h", "br i1 %c1, label %body, label %m"),
            blk("body", "br i1 %c2, label %h, label %m"),
            blk("m", "ret void"),
        ];
        let mut dominance = block_dominators(&blocks);

        splice_pass_through(&mut blocks, "%s0", "%m", &["%h", "%body"]);
        dominance.record_pass_through("%s0", &["%h".to_string(), "%body".to_string()]);
        splice_pass_through(&mut blocks, "%s1", "%m", &["%s0", "%pre"]);
        dominance.record_pass_through("%s1", &["%s0".to_string(), "%pre".to_string()]);

        let cfg = Cfg::from_blocks(&blocks).expect("non-empty");
        let names = reachable_names(&cfg);
        assert!(names.contains(&"%s0".to_string()) && names.contains(&"%s1".to_string()));
        let originals = ["%0", "%pre", "%h", "%body", "%m"];
        for dominator in originals {
            for node in &names {
                assert_eq!(
                    dominance.dominates(dominator, node),
                    oracle_dominates(&cfg, dominator, node),
                    "recorded splits disagree with the split graph: does {dominator} dominate {node}?"
                );
            }
        }
    }

    #[test]
    fn a_pass_through_with_no_predecessors_is_dominated_by_nothing() {
        let blocks = vec![blk("0", "br label %a"), blk("a", "ret void")];
        let mut dominance = block_dominators(&blocks);
        dominance.record_pass_through("%s0", &[]);
        assert!(!dominance.dominates("%0", "%s0"));
        assert!(dominance.dominates("%s0", "%s0"));
    }
}
