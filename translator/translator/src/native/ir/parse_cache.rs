//! Parsed modules that the tiers of one translation share (`LlModule::parse_inner`).
//!
//! The cache lives only while a `ParseScope` is held on this thread (`crate::with_parse_cache`, entered once per translation
//! by the C entry points); outside one it is off, and dropping the outermost scope empties it, so no module outlives the
//! translation that parsed it. An entry keeps its whole key, the text included, so a lookup can never return a module parsed
//! from other input. At most `MAX_ENTRIES` modules are kept, the oldest replaced first.
use super::LlModule;
use std::cell::RefCell;
use std::marker::PhantomData;

/// The texts one translation parses more than once: the module as written, its lowered form and the raw tiers' form.
const MAX_ENTRIES: usize = 4;

struct Entry {
    ll: String,
    primitive_phi_metadata: bool,
    kern: Option<String>,
    entry_name: Option<String>,
    module: LlModule,
}

#[derive(Default)]
struct Cache {
    depth: usize,
    entries: Vec<Entry>,
    /// `text_pass` results: (pass, input, output or `None` for an unchanged input).
    texts: Vec<(&'static str, String, Option<String>)>,
}

/// Text passes kept per translation: each pass sees one or two distinct inputs (the primary and the raw tiers' text).
const MAX_TEXTS: usize = 6;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Held for one translation; `!Send`, because the cache behind it is this thread's.
pub(crate) struct ParseScope {
    _not_send: PhantomData<*const ()>,
}

impl ParseScope {
    pub(crate) fn enter() -> ParseScope {
        CACHE.with(|cache| cache.borrow_mut().depth += 1);
        ParseScope {
            _not_send: PhantomData,
        }
    }
}

impl Drop for ParseScope {
    fn drop(&mut self) {
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.depth -= 1;
            if cache.depth == 0 {
                cache.entries.clear();
                cache.texts.clear();
            }
        });
    }
}

pub(super) fn active() -> bool {
    is_active()
}

/// Whether a translation's caches are on (a `ParseScope` is held on this thread).
pub(crate) fn is_active() -> bool {
    CACHE.with(|cache| cache.borrow().depth > 0)
}

pub(super) fn get(
    ll: &str,
    primitive_phi_metadata: bool,
    kern: Option<&str>,
    entry_name: Option<&str>,
) -> Option<LlModule> {
    CACHE.with(|cache| {
        cache
            .borrow()
            .entries
            .iter()
            .find(|e| {
                e.primitive_phi_metadata == primitive_phi_metadata
                    && e.kern.as_deref() == kern
                    && e.entry_name.as_deref() == entry_name
                    && e.ll == ll
            })
            .map(|e| e.module.clone())
    })
}

pub(super) fn put(
    ll: &str,
    primitive_phi_metadata: bool,
    kern: Option<String>,
    entry_name: Option<&str>,
    module: &LlModule,
) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.depth == 0 {
            return;
        }
        if cache.entries.len() == MAX_ENTRIES {
            cache.entries.remove(0);
        }
        cache.entries.push(Entry {
            ll: ll.to_owned(),
            primitive_phi_metadata,
            kern,
            entry_name: entry_name.map(str::to_owned),
            module: module.clone(),
        });
    });
}

/// The result of the pure text pass `pass` on `input` (`run`, whose `None` means the text is unchanged), computed once per
/// translation while a `ParseScope` is held and recomputed otherwise. Keyed by the pass name and the whole input.
pub(crate) fn text_pass(
    pass: &'static str,
    input: &str,
    run: impl FnOnce(&str) -> Option<String>,
) -> Option<String> {
    if !is_active() {
        return run(input);
    }
    let hit = CACHE.with(|cache| {
        cache
            .borrow()
            .texts
            .iter()
            .find(|(p, i, _)| *p == pass && i == input)
            .map(|(_, _, out)| out.clone())
    });
    if let Some(out) = hit {
        return out;
    }
    let out = run(input);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.depth > 0 {
            if cache.texts.len() == MAX_TEXTS {
                cache.texts.remove(0);
            }
            cache.texts.push((pass, input.to_owned(), out.clone()));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "define void @k() {\nentry:\n  ret void\n}\n";
    const B: &str = "define void @j() {\nentry:\n  ret void\n}\n";

    #[test]
    fn a_cached_parse_is_the_parse_of_the_same_text_and_never_of_another() {
        let _scope = ParseScope::enter();
        let a = LlModule::parse(A).unwrap();
        let again = LlModule::parse(A).unwrap();
        assert_eq!(format!("{a:?}"), format!("{again:?}"));
        let b = LlModule::parse(B).unwrap();
        assert_ne!(format!("{a:?}"), format!("{b:?}"), "a different text must not be served the cached module");
        assert_eq!(format!("{b:?}"), format!("{:?}", LlModule::parse_inner_uncached(
                B,
                false,
                crate::meta::parse_air_kernel_meta(B).as_ref(),
                crate::meta::entry_name(B, "kernel").as_deref()
            )
            .unwrap()));
    }

    #[test]
    fn nothing_is_cached_outside_a_scope_and_a_scope_leaves_nothing_behind() {
        assert!(!is_active());
        LlModule::parse(A).unwrap();
        assert!(get(A, false, None, None).is_none(), "outside a scope nothing is stored");
        {
            let _scope = ParseScope::enter();
            LlModule::parse(A).unwrap();
            assert!(CACHE.with(|c| !c.borrow().entries.is_empty()), "inside a scope the parse is stored");
        }
        assert!(CACHE.with(|c| c.borrow().entries.is_empty()), "the scope's end empties the cache");
        assert_eq!(
            text_pass("t", "x", |_| Some("y".into())),
            Some("y".into()),
            "outside a scope a text pass runs"
        );
    }

    #[test]
    fn a_text_pass_runs_once_per_input_in_a_scope() {
        let _scope = ParseScope::enter();
        let mut runs = 0;
        for _ in 0..3 {
            text_pass("t", "x", |_| {
                runs += 1;
                None
            });
        }
        assert_eq!(runs, 1, "the same input ran {runs} times");
        let out = text_pass("t", "z", |input| Some(format!("{input}!")));
        assert_eq!(out.as_deref(), Some("z!"), "another input is run, not served");
    }
}
