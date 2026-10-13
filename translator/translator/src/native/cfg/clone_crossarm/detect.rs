use super::*;

pub(in crate::native) fn predecessors(blocks: &[BodyBlock]) -> HashMap<String, Vec<String>> {
    let mut preds: HashMap<String, Vec<String>> = HashMap::default();
    for b in blocks {
        for s in block_successors(b) {
            preds.entry(s).or_default().push(b.name.clone());
        }
    }
    preds
}

pub(in crate::native) fn find_cross_arm(blocks: &[BodyBlock]) -> Option<(String, String)> {
    let forest = analyze(blocks);
    let pidom = post_idom(blocks);
    let loop_headers: HashSet<&str> = forest.loops.iter().map(|l| l.header.as_str()).collect();
    let is_enclosing_break = |b: &str, a: &str| -> bool {
        forest.loops.iter().any(|l| {
            l.body.iter().any(|n| n == b) && (l.header == a || l.exits.iter().any(|e| e == a))
        })
    };
    let names: HashSet<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
    for b in blocks {
        if loop_headers.contains(b.name.as_str()) {
            continue;
        }
        let succs = block_successors(b);
        let distinct: HashSet<&str> = succs
            .iter()
            .map(String::as_str)
            .filter(|t| names.contains(t))
            .collect();
        if distinct.len() < 2 {
            continue;
        }
        let merge = pidom.get(&b.name).map(String::as_str);
        for a in succs
            .iter()
            .map(String::as_str)
            .filter(|a| distinct.contains(a))
        {
            if Some(a) == merge || is_enclosing_break(&b.name, a) {
                continue;
            }
            if loop_headers.contains(a) {
                continue;
            }
            if !forest.dominates(&b.name, a) {
                return Some((b.name.clone(), a.to_string()));
            }
        }
    }
    None
}
