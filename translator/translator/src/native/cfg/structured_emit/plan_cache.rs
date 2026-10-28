//! Results that the attempts of one `structured_plan` call share.
//!
//! `structured_plan` tries one plan after another (`structured_plan_ladder` with and without a bare exit, on the original blocks
//! and on privatized copies, again after `finalize_loop_role_switches`), and `structured_reject_reason` walks the same ladder
//! once more. Each attempt needs `forest_loop_merges` and the selection synthesis, and both depend only on the blocks and a few
//! of the attempt's flags, so attempts that differ in the other flags compute the same thing again. This module caches those two
//! results for the duration of one planner call and returns a clone of a stored value for an equal input.
//!
//! An entry remembers its whole input: the blocks, the flags that affect the step, and the merge tables the selection synthesis
//! reads. Blocks hold their body in an `Arc` that is never changed in place while it is shared, so two blocks with the same name,
//! role and `Arc` are equal; blocks with different `Arc`s (a privatized copy made again) are compared by their debug text, as
//! `block_body_changed` does. The key keeps full values rather than a hash, so a collision cannot serve a stale result.
//!
//! The cache lives only while a `Scope` is held on this thread; outside one it is off. Each table is capped at `MAX_ENTRIES`: a
//! lookup that misses while the table is full is computed but not stored, which bounds the linear scan. A later `Scope` starts
//! empty because dropping the outermost one clears both tables.
use super::*;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::Arc;

pub(super) type LoopMerges = (Vec<BodyBlock>, HashMap<String, LoopMergeInfo>);
pub(super) type Selection = (
    Vec<BodyBlock>,
    HashMap<(String, String), String>,
    HashMap<String, String>,
    HashMap<String, String>,
);

/// Exact structural key of the two merge tables: sorted `(key, value)` terminal merges and sorted
/// `(header, merge, continue_target)` loop merges. Every field is kept, so distinct tables never compare equal.
type MapsKey = (Vec<(String, String)>, Vec<(String, String, String)>);

const MAX_ENTRIES: usize = 24;

#[cfg(test)]
thread_local! {
    static HITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct LoopMergesEntry {
    converge_inloop: bool,
    multi_exit_clone: bool,
    input: Vec<BodyBlock>,
    value: LoopMerges,
}

struct SelectionEntry {
    flags: [bool; 3],
    maps: MapsKey,
    input: Vec<BodyBlock>,
    value: Selection,
}

#[derive(Default)]
struct Cache {
    depth: usize,
    loop_merges: Vec<LoopMergesEntry>,
    selection: Vec<SelectionEntry>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Held by a caller of the planner for as long as it runs; the cache is emptied when the outermost one is dropped.
///
/// The cache behind it is thread-local, so a `Scope` must stay on the thread that entered it. The `PhantomData` of a raw
/// pointer makes the type `!Send`/`!Sync`; nothing ever reads the field.
pub(in crate::native) struct Scope {
    _not_send: PhantomData<*const ()>,
}

impl Scope {
    pub(in crate::native) fn enter() -> Scope {
        CACHE.with(|cache| cache.borrow_mut().depth += 1);
        Scope {
            _not_send: PhantomData,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.depth -= 1;
            if cache.depth == 0 {
                cache.loop_merges.clear();
                cache.selection.clear();
            }
        });
    }
}

fn active() -> bool {
    CACHE.with(|cache| cache.borrow().depth > 0)
}

fn same_blocks(left: &[BodyBlock], right: &[BodyBlock]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(l, r)| {
            l.name == r.name
                && l.role == r.role
                && match (&l.typed, &r.typed) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b) || format!("{a:?}") == format!("{b:?}"),
                    (None, None) => true,
                    _ => false,
                }
        })
}

/// The part of the key that is not the blocks: the merge tables, in a fixed order.
fn maps_key(
    loop_merges: &HashMap<String, LoopMergeInfo>,
    terminal_merges: &HashMap<String, String>,
) -> MapsKey {
    let mut terminal: Vec<(String, String)> = terminal_merges
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    terminal.sort();
    let mut loops: Vec<(String, String, String)> = loop_merges
        .iter()
        .map(|(header, info)| {
            (
                header.clone(),
                info.merge.clone(),
                info.continue_target.clone(),
            )
        })
        .collect();
    loops.sort();
    (terminal, loops)
}

pub(super) fn forest_loop_merges_cached(
    blocks: &[BodyBlock],
    converge_inloop: bool,
    multi_exit_clone: bool,
) -> LoopMerges {
    if !active() {
        return forest_loop_merges(blocks, converge_inloop, multi_exit_clone);
    }
    let hit = CACHE.with(|cache| {
        cache
            .borrow()
            .loop_merges
            .iter()
            .find(|e| {
                e.converge_inloop == converge_inloop
                    && e.multi_exit_clone == multi_exit_clone
                    && same_blocks(&e.input, blocks)
            })
            .map(|e| e.value.clone())
    });
    if let Some(value) = hit {
        #[cfg(test)]
        HITS.with(|hits| hits.set(hits.get() + 1));
        return value;
    }
    let value = forest_loop_merges(blocks, converge_inloop, multi_exit_clone);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.loop_merges.len() < MAX_ENTRIES {
            cache.loop_merges.push(LoopMergesEntry {
                converge_inloop,
                multi_exit_clone,
                input: blocks.to_vec(),
                value: value.clone(),
            });
        }
    });
    value
}

/// The selection synthesis of `structured_plan_inner8`, from `lblocks` to the normalized `sblocks` and their merge tables.
#[allow(clippy::too_many_arguments)]
pub(super) fn selection_cached(
    lblocks: &[BodyBlock],
    loop_merges: &HashMap<String, LoopMergeInfo>,
    terminal_merges: &HashMap<String, String>,
    break_aware: bool,
    loop_exit_selection: bool,
    construct_tree_owned: bool,
    compute: impl FnOnce() -> Selection,
) -> Selection {
    if !active() {
        return compute();
    }
    let flags = [break_aware, loop_exit_selection, construct_tree_owned];
    let maps = maps_key(loop_merges, terminal_merges);
    let hit = CACHE.with(|cache| {
        cache
            .borrow()
            .selection
            .iter()
            .find(|e| e.flags == flags && e.maps == maps && same_blocks(&e.input, lblocks))
            .map(|e| e.value.clone())
    });
    if let Some(value) = hit {
        #[cfg(test)]
        HITS.with(|hits| hits.set(hits.get() + 1));
        return value;
    }
    let value = compute();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.selection.len() < MAX_ENTRIES {
            cache.selection.push(SelectionEntry {
                flags,
                maps,
                input: lblocks.to_vec(),
                value: value.clone(),
            });
        }
    });
    value
}

#[cfg(test)]
mod tests {
    use super::super::test_support::bb;
    use super::*;

    fn hits() -> usize {
        HITS.with(std::cell::Cell::get)
    }

    fn reset_hits() {
        HITS.with(|count| count.set(0));
    }

    fn self_loop(step: i32) -> Vec<BodyBlock> {
        let next = format!("%next = add i32 %x, {step}");
        vec![
            bb("%entry", &["br label %loop"]),
            bb(
                "%loop",
                &[
                    "%x = phi i32 [ 0, %entry ], [ %next, %loop ]",
                    next.as_str(),
                    "br label %loop",
                ],
            ),
        ]
    }

    fn plan_text(plan: &StructuredPlan) -> String {
        let mut loops: Vec<_> = plan
            .loop_merges
            .iter()
            .map(|(header, info)| format!("{header}=>{}|{}", info.merge, info.continue_target))
            .collect();
        loops.sort();
        format!("{:?}\n{loops:?}", plan.blocks)
    }

    /// A selection whose terminal-merges entry carries `tag`, so a caller can tell which compute produced it.
    fn dummy_selection(tag: &str) -> Selection {
        let mut terminal = HashMap::default();
        terminal.insert("tag".to_string(), tag.to_string());
        (Vec::new(), HashMap::default(), terminal, HashMap::default())
    }

    fn tag_of(selection: &Selection) -> Option<&str> {
        selection.2.get("tag").map(String::as_str)
    }

    #[test]
    fn selection_key_distinguishes_different_inputs() {
        reset_hits();
        let _scope = Scope::enter();
        let lblocks = self_loop(1);
        let mut loop_merges = HashMap::default();
        loop_merges.insert(
            "%loop".to_string(),
            LoopMergeInfo {
                merge: "%merge".to_string(),
                continue_target: "%loop".to_string(),
            },
        );
        let mut other_loop_merges = HashMap::default();
        other_loop_merges.insert(
            "%loop".to_string(),
            LoopMergeInfo {
                merge: "%other".to_string(),
                continue_target: "%loop".to_string(),
            },
        );
        let terminal_merges = HashMap::default();
        let mut other_terminal_merges = HashMap::default();
        other_terminal_merges.insert("%merge".to_string(), "%exit".to_string());

        let first = selection_cached(
            &lblocks,
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("first"),
        );
        assert_eq!(hits(), 0);
        assert_eq!(tag_of(&first), Some("first"));

        // The same key is found and the body is not computed again.
        let second = selection_cached(
            &lblocks,
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || panic!("an identical key must not compute"),
        );
        assert_eq!(hits(), 1);
        assert_eq!(tag_of(&second), Some("first"));

        // A different flag, a different loop-merge table and a different terminal-merge table all miss.
        let by_flag = selection_cached(
            &lblocks,
            &loop_merges,
            &terminal_merges,
            true,
            false,
            false,
            || dummy_selection("flag"),
        );
        let by_loop = selection_cached(
            &lblocks,
            &other_loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("loops"),
        );
        let by_terminal = selection_cached(
            &lblocks,
            &loop_merges,
            &other_terminal_merges,
            false,
            false,
            false,
            || dummy_selection("terminal"),
        );
        assert_eq!(tag_of(&by_flag), Some("flag"));
        assert_eq!(tag_of(&by_loop), Some("loops"));
        assert_eq!(tag_of(&by_terminal), Some("terminal"));
        assert_eq!(hits(), 1, "only the identical lookup hit");
    }

    #[test]
    fn equal_body_text_in_different_arcs_still_hits() {
        reset_hits();
        let _scope = Scope::enter();
        let first_input = self_loop(1);
        let second_input = self_loop(1);
        // Distinct allocations with identical text: `same_blocks` must fall back to the text comparison.
        assert!(!Arc::ptr_eq(
            first_input[1].typed.as_ref().expect("typed block"),
            second_input[1].typed.as_ref().expect("typed block"),
        ));
        assert_eq!(
            format!("{:?}", first_input[1].typed),
            format!("{:?}", second_input[1].typed),
        );

        let loop_merges = HashMap::default();
        let terminal_merges = HashMap::default();
        let first = selection_cached(
            &first_input,
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("first"),
        );
        assert_eq!(hits(), 0);
        let second = selection_cached(
            &second_input,
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || panic!("equal text in a fresh `Arc` must hit"),
        );
        assert_eq!(hits(), 1);
        assert_eq!(tag_of(&second), Some("first"));
        assert_eq!(format!("{:?}", first.0), format!("{:?}", second.0));
    }

    #[test]
    fn a_changed_block_set_misses() {
        reset_hits();
        let _scope = Scope::enter();
        let loop_merges = HashMap::default();
        let terminal_merges = HashMap::default();
        let first = selection_cached(
            &self_loop(1),
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("first"),
        );
        assert_eq!(hits(), 0);
        assert_eq!(tag_of(&first), Some("first"));

        // One block fewer.
        let shorter = self_loop(1);
        let by_length = selection_cached(
            &shorter[..1],
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("shorter"),
        );
        // Same length, one body line different.
        let by_body = selection_cached(
            &self_loop(2),
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("body"),
        );
        // Same length, one block name different.
        let mut renamed = self_loop(1);
        renamed[1].name = "%other".to_string();
        let by_name = selection_cached(
            &renamed,
            &loop_merges,
            &terminal_merges,
            false,
            false,
            false,
            || dummy_selection("name"),
        );

        assert_eq!(tag_of(&by_length), Some("shorter"));
        assert_eq!(tag_of(&by_body), Some("body"));
        assert_eq!(tag_of(&by_name), Some("name"));
        assert_eq!(hits(), 0, "none of the changed block sets hit");
    }

    #[test]
    fn equal_blocks_built_separately_hit_and_anything_else_misses() {
        reset_hits();
        let _scope = Scope::enter();
        let first_input = self_loop(1);
        let first = forest_loop_merges_cached(&first_input, false, false);
        assert_eq!(hits(), 0);

        // The same blocks lowered again own different `Arc`s and are still found.
        let second_input = self_loop(1);
        let second = forest_loop_merges_cached(&second_input, false, false);
        assert_eq!(hits(), 1);
        assert_eq!(format!("{:?}", first.0), format!("{:?}", second.0));
        assert_eq!(first.1.len(), second.1.len());

        // A different flag, and a block whose body differs, are not.
        forest_loop_merges_cached(&second_input, true, false);
        forest_loop_merges_cached(&second_input, false, true);
        forest_loop_merges_cached(&self_loop(2), false, false);
        assert_eq!(hits(), 1);
    }

    #[test]
    fn nothing_is_kept_outside_a_scope_or_after_it_ends() {
        reset_hits();
        let blocks = self_loop(1);
        forest_loop_merges_cached(&blocks, false, false);
        forest_loop_merges_cached(&blocks, false, false);
        assert_eq!(hits(), 0, "the cache is off outside a scope");

        {
            let _scope = Scope::enter();
            forest_loop_merges_cached(&blocks, false, false);
        }
        {
            let _scope = Scope::enter();
            forest_loop_merges_cached(&blocks, false, false);
            assert_eq!(hits(), 0, "a new scope starts empty");
            forest_loop_merges_cached(&blocks, false, false);
            assert_eq!(hits(), 1);
        }
    }

    #[test]
    fn a_plan_is_the_same_with_the_cache_and_without_it() {
        let blocks = self_loop(1);
        // Called directly, outside a scope, the attempt runs without the cache.
        let without = structured_plan_inner(&blocks, false, false).expect("plan");
        reset_hits();
        let _scope = Scope::enter();
        let first = structured_plan_inner(&blocks, false, false).expect("plan");
        let second = structured_plan_inner(&blocks, false, false).expect("plan");
        assert!(hits() >= 2, "the second attempt reuses both steps");
        assert_eq!(plan_text(&without), plan_text(&first));
        assert_eq!(plan_text(&without), plan_text(&second));
    }
}
