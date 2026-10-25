//! A pointer phi whose only use is a load becomes a phi of the LOADED VALUES.
//!
//! With ray tracing enabled, a shipping game had 15 of its fragment shaders refused; 12 of them failed with
//!
//! ```text
//! native emitter: Function pointer phi %.in has differing structural sources
//! ```
//!
//! None of those 15 modules contains a ray query or an `air.intersect` call — they are ordinary fragment
//! shaders that merely only load when ray tracing is on. The refusal is a logical-SPIR-V problem, and
//! `owned_cfg` already encodes why no capability can rescue it: `VariablePointers` admits pointers in
//! `StorageBuffer` and `Workgroup` only, never `Function`. So a Function-storage pointer phi has no legal
//! representation and the input has to be rewritten instead.
//!
//! The shape such a module actually has:
//!
//! ```text
//! 271:                                    ; preds = %259, %9
//!   %.in = phi ptr [ %270, %259 ], [ %244, %9 ]    ; %270 = getelementptr ... ptr %24, an alloca
//!   %272 = phi float [ %266, %259 ], [ %240, %9 ]  ; value phis ALREADY sit beside it
//!   %275 = load float, ptr %.in, align 4           ; the ONLY use of %.in
//! ```
//!
//! So the load is hoisted into each predecessor — where the address is already computed — and the pointer
//! phi is replaced by a phi of the loaded values, which is exactly what the float phis beside it do. That
//! is safe because the load is unconditional in the successor and each incoming address is live in its own
//! predecessor; hoisting cannot introduce a fault that the successor's own load would not already take.
//!
//! DELIBERATELY NARROW. This fires only on the form above, and anything else keeps the existing hard
//! error rather than being guessed at:
//!   * every use of the phi must be ONE load, in the phi's own block;
//!   * only phi instructions may sit between the phi and that load, so the replacement stays in the block's
//!     phi prologue — a phi after a non-phi instruction is invalid LLVM;
//!   * a predecessor label may not repeat in the incoming list;
//!   * a store through the phi, a use in another block, or a second use refuses, because for those the
//!     loaded value is not a faithful stand-in for the pointer.

/// A block header: `<name>:` optionally followed by a `; preds = ...` comment. Returns the label.
///
/// This deliberately does NOT key on indentation. A first version decided "not indented = a header",
/// which is true of how LLVM prints a module but silently matched nothing when handed IR whose indentation
/// had been stripped — the pass then found no terminators and refused every module without saying why.
/// Ending in a colon is the real signal, and an instruction never does (it carries ` = ` or a keyword).
fn block_label(line: &str) -> Option<&str> {
    let body = line.split(';').next()?.trim();
    let name = body.strip_suffix(':')?;
    if name.is_empty() || name.contains(char::is_whitespace) || name.contains('=') {
        return None;
    }
    Some(name)
}

const TERMINATORS: [&str; 6] = ["br ", "ret ", "switch ", "unreachable", "resume ", "indirectbr "];

/// One function: where it lives, each labelled block's terminator, and the ENTRY block's terminator.
///
/// The entry block is why a first version refused every real module. LLVM prints it with NO label line
/// at all, so a scan that only records labelled blocks never learns its terminator — and the game's phi
/// reads `[ %244, %9 ]`, where `%9` IS that unlabelled entry block. Every one of the refused modules named its entry block this way.
struct Blocks {
    span: (usize, usize),
    terms: std::collections::HashMap<String, usize>,
    entry_term: Option<usize>,
}

impl Blocks {
    /// The line to insert above, for a phi predecessor label.
    ///
    /// The entry fallback applies ONLY to an all-digit label that appears as no block header in this
    /// function. Every non-entry block must carry a header, so an unknown numeric label can only be the
    /// implicit entry block; anything else returns None and the phi is left to the existing hard error,
    /// because inserting a load into the wrong block would be silently wrong code.
    fn terminator_for(&self, label: &str) -> Option<usize> {
        if let Some(at) = self.terms.get(label) {
            return Some(*at);
        }
        if label.bytes().all(|b| b.is_ascii_digit()) {
            return self.entry_term;
        }
        None
    }
}

fn functions(lines: &[&str]) -> Vec<Blocks> {
    let mut out: Vec<Blocks> = Vec::new();
    let mut label: Option<String> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.starts_with("define ") {
            out.push(Blocks { span: (i, lines.len()), terms: Default::default(), entry_term: None });
            label = None;
            continue;
        }
        let Some(f) = out.last_mut() else { continue };
        if line == "}" {
            f.span.1 = i;
            label = None;
            continue;
        }
        if let Some(name) = block_label(raw) {
            label = Some(name.to_string());
            continue;
        }
        if TERMINATORS.iter().any(|t| line.starts_with(t)) {
            match label.as_ref() {
                Some(name) => {
                    f.terms.insert(name.clone(), i);
                }
                // before any header: this is the implicit entry block, which has exactly one terminator
                None => {
                    if f.entry_term.is_none() {
                        f.entry_term = Some(i);
                    }
                }
            }
        }
    }
    out
}

/// `%name` as it appears as an operand, so `%.in` does not match `%.in2`.
fn uses_value(line: &str, name: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        let ok_after = after
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.');
        if ok_after {
            return true;
        }
        rest = &rest[at + name.len()..];
    }
    false
}

struct PtrPhi {
    line: usize,
    name: String,
    incoming: Vec<(String, String)>, // (value, predecessor label)
}

/// `  %x = phi ptr [ %a, %l1 ], [ %b, %l2 ]` -> the pieces, or None if it is not that.
fn parse_ptr_phi(line: &str, index: usize) -> Option<PtrPhi> {
    let trimmed = line.trim();
    let (name, rhs) = trimmed.split_once(" = ")?;
    if !name.starts_with('%') {
        return None;
    }
    // only a plain `phi ptr`: an addrspace pointer is a different storage class and not this bug
    // "phi ptr addrspace(1) [ ... ]" ALSO starts with "phi ptr ", and a first version happily rewrote
    //    it — the incoming list still parsed. The list has to start immediately after the type.
    let list = rhs.strip_prefix("phi ptr ")?.trim_start();
    if !list.starts_with('[') {
        return None;
    }
    let mut incoming = Vec::new();
    for part in list.split('[').skip(1) {
        let inner = part.split(']').next()?;
        let (value, label) = inner.split_once(',')?;
        let label = label.trim().trim_start_matches('%');
        incoming.push((value.trim().to_string(), label.to_string()));
    }
    if incoming.len() < 2 {
        return None;
    }
    // a repeated predecessor label would need one load per edge; refuse rather than guess
    for i in 0..incoming.len() {
        for j in i + 1..incoming.len() {
            if incoming[i].1 == incoming[j].1 {
                return None;
            }
        }
    }
    Some(PtrPhi { line: index, name: name.to_string(), incoming })
}

/// The single load of `name`, if it is in the phi's own phi-prologue: (line, result, type, align suffix).
fn sole_prologue_load(lines: &[&str], phi: &PtrPhi) -> Option<(usize, String, String, String)> {
    let mut found: Option<(usize, String, String, String)> = None;
    let mut prologue = true;
    for (i, raw) in lines.iter().enumerate() {
        if i == phi.line {
            continue;
        }
        if !uses_value(raw, &phi.name) {
            // once a non-phi instruction appears after the phi, the prologue is over
            if i > phi.line && raw.starts_with(' ') {
                let t = raw.trim();
                if !t.is_empty() && !t.starts_with(';') && !t.contains(" = phi ") {
                    prologue = false;
                }
            }
            continue;
        }
        // a use. It must be the one load, and it must still be in the prologue.
        if found.is_some() || i < phi.line || !prologue {
            return None;
        }
        let trimmed = raw.trim();
        let (result, rhs) = trimmed.split_once(" = ")?;
        let body = rhs.strip_prefix("load ")?;
        let (ty, addr) = body.split_once(", ptr ")?;
        // `%.in, align 4` -> the name must be the whole operand, and keep whatever follows it
        let (operand, tail) = match addr.split_once(',') {
            Some((operand, tail)) => (operand.trim(), tail.trim().to_string()),
            None => (addr.trim(), String::new()),
        };
        if operand != phi.name || ty.contains('*') {
            return None;
        }
        found = Some((i, result.to_string(), ty.to_string(), tail));
    }
    found
}

/// Rewrite every pointer phi that matches the measured form. Returns None when nothing applied.
fn rewrite(ll: &str) -> Option<String> {
    if !ll.contains("phi ptr ") {
        return None;
    }
    let lines: Vec<&str> = ll.lines().collect();
    let funcs = functions(&lines);

    // line index -> replacement text (None = drop the line), plus loads to insert above a terminator
    let mut replace: std::collections::HashMap<usize, Option<String>> = std::collections::HashMap::new();
    let mut insert: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
    let mut fresh = 0usize;

    for (i, raw) in lines.iter().enumerate() {
        let Some(phi) = parse_ptr_phi(raw, i) else { continue };
        let Some((load_line, result, ty, tail)) = sole_prologue_load(&lines, &phi) else { continue };
        // the phi's own function decides what its predecessor labels mean
        let Some(f) = funcs.iter().find(|f| i > f.span.0 && i < f.span.1) else { continue };
        // every predecessor must be a block whose terminator we can insert above
        let Some(ats) = phi
            .incoming
            .iter()
            .map(|(_, label)| f.terminator_for(label))
            .collect::<Option<Vec<usize>>>()
        else {
            continue;
        };
        if replace.contains_key(&load_line) {
            continue; // two phis feeding one load: leave both to the existing error
        }
        let mut arms = Vec::new();
        for ((value, label), at) in phi.incoming.iter().zip(ats) {
            let name = format!("%ptrphi.sunk.{fresh}");
            fresh += 1;
            let loaded = if tail.is_empty() {
                format!("  {name} = load {ty}, ptr {value}")
            } else {
                format!("  {name} = load {ty}, ptr {value}, {tail}")
            };
            insert.entry(at).or_default().push(loaded);
            arms.push(format!("[ {name}, %{label} ]"));
        }
        // the value phi takes the LOAD's result name, so no other line needs rewriting, and it sits where
        // the pointer phi sat — still inside the block's phi prologue, which LLVM requires.
        replace.insert(phi.line, Some(format!("  {result} = phi {ty} {}", arms.join(", "))));
        replace.insert(load_line, None);
    }

    if replace.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(ll.len() + 256);
    for (i, raw) in lines.iter().enumerate() {
        if let Some(loads) = insert.get(&i) {
            for loaded in loads {
                out.push_str(loaded);
                out.push('\n');
            }
        }
        match replace.get(&i) {
            Some(None) => {}
            Some(Some(text)) => {
                out.push_str(text);
                out.push('\n');
            }
            None => {
                out.push_str(raw);
                out.push('\n');
            }
        }
    }
    Some(out)
}

pub(crate) fn sink_pointer_phi_loads(ll: &str) -> std::borrow::Cow<'_, str> {
    match rewrite(ll) {
        Some(out) => std::borrow::Cow::Owned(out),
        None => std::borrow::Cow::Borrowed(ll),
    }
}

pub(crate) fn sink_pointer_phi_loads_owned(ll: String) -> String {
    match rewrite(&ll) {
        Some(out) => out,
        None => ll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape a refused module has, reduced: a pointer phi over two alloca-derived GEPs whose
    /// only use is an unconditional load, with value phis already beside it.
    /// `concat!`, NOT a `\`-continued string: the continuation strips the leading whitespace of each
    /// line, which left this fixture with no indentation at all and made the first version of the test
    /// exercise IR that LLVM never produces. The `; preds` comment on the header is real too — the label
    /// scan has to see past it.
    const GAME: &str = concat!(
        "define void @ps() {\n",
        "9:\n",
        "  %24 = alloca [4 x float], align 4\n",
        "  %244 = getelementptr [4 x float], ptr %24, i64 0, i64 0\n",
        "  %240 = fadd float 1.0, 2.0\n",
        "  br label %271\n",
        "259:\n",
        "  %270 = getelementptr [4 x float], ptr %24, i64 0, i64 1\n",
        "  %266 = fadd float 3.0, 4.0\n",
        "  br label %271\n",
        "271:                                              ; preds = %259, %9\n",
        "  %.in = phi ptr [ %270, %259 ], [ %244, %9 ]\n",
        "  %272 = phi float [ %266, %259 ], [ %240, %9 ]\n",
        "  %275 = load float, ptr %.in, align 4\n",
        "  ret void\n",
        "}\n",
    );

    #[test]
    fn the_game_form_becomes_a_value_phi() {
        let out = rewrite(GAME).expect("the measured form must rewrite");
        assert!(!out.contains("phi ptr "), "the pointer phi must be gone:\n{out}");
        assert!(
            out.contains("%275 = phi float [ %ptrphi.sunk.0, %259 ], [ %ptrphi.sunk.1, %9 ]"),
            "the load's result must become the value phi:\n{out}"
        );
        assert!(out.contains("  %ptrphi.sunk.0 = load float, ptr %270, align 4"), "{out}");
        assert!(out.contains("  %ptrphi.sunk.1 = load float, ptr %244, align 4"), "{out}");
        // each hoisted load must sit DIRECTLY above its predecessor's terminator. Inserted after the
        //    branch it would never execute, and the phi would read an undefined value.
        let ls: Vec<&str> = out.lines().collect();
        for name in ["%ptrphi.sunk.0", "%ptrphi.sunk.1"] {
            let i = ls
                .iter()
                .position(|l| l.contains(name) && l.contains("= load"))
                .unwrap_or_else(|| panic!("no hoisted load for {name}:\n{out}"));
            assert!(
                ls[i + 1].trim_start().starts_with("br label %271"),
                "the hoisted load must sit directly above the terminator, got {:?}\n{out}",
                ls[i + 1]
            );
        }
        // the value phi must still be in the phi prologue: a phi after a non-phi is invalid LLVM.
        // Found by this assertion: the header carries "; preds = ..." so it cannot be matched as "271:\n".
        let head = ls
            .iter()
            .position(|l| block_label(l) == Some("271"))
            .unwrap_or_else(|| panic!("no 271 block header:\n{out}"));
        assert!(
            ls[head + 1].contains(" = phi "),
            "the prologue must still start with a phi, got {:?}\n{out}",
            ls[head + 1]
        );
    }

    /// A store through the phi is NOT this transformation: the loaded value cannot stand in for a
    /// pointer that is written through, so it must be left to the existing hard error.
    #[test]
    fn a_store_through_the_phi_refuses() {
        let ll = GAME.replace(
            "  %275 = load float, ptr %.in, align 4\n",
            "  %275 = load float, ptr %.in, align 4\n  store float 0.0, ptr %.in, align 4\n",
        );
        assert!(rewrite(&ll).is_none(), "a stored-through pointer phi must refuse");
    }

    /// Two loads of the same phi refuse: only the single-use form was measured, and a second use may
    /// sit outside the prologue where a phi cannot be placed.
    #[test]
    fn a_second_use_refuses() {
        let ll = GAME.replace(
            "  %275 = load float, ptr %.in, align 4\n",
            "  %275 = load float, ptr %.in, align 4\n  %276 = load float, ptr %.in, align 4\n",
        );
        assert!(rewrite(&ll).is_none(), "two uses must refuse");
    }

    /// A load that is NOT in the phi prologue refuses: the replacement phi would land after a non-phi
    /// instruction, which is invalid LLVM.
    #[test]
    fn a_load_after_a_non_phi_refuses() {
        let ll = GAME.replace(
            "  %272 = phi float [ %266, %259 ], [ %240, %9 ]\n",
            "  %272 = phi float [ %266, %259 ], [ %240, %9 ]\n  %273 = fadd float 1.0, 1.0\n",
        );
        assert!(rewrite(&ll).is_none(), "a load outside the prologue must refuse");
    }

    /// A module with no pointer phi is returned untouched, and borrows rather than copying.
    #[test]
    fn nothing_to_do_is_borrowed() {
        let ll = "define void @f() {\n  ret void\n}\n";
        assert!(matches!(sink_pointer_phi_loads(ll), std::borrow::Cow::Borrowed(_)));
    }

    /// An addrspace pointer phi is a different storage class and not this defect: leave it alone.
    #[test]
    fn an_addrspace_pointer_phi_is_left_alone() {
        let ll = GAME.replace(
            "  %.in = phi ptr [ %270, %259 ], [ %244, %9 ]\n",
            "  %.in = phi ptr addrspace(1) [ %270, %259 ], [ %244, %9 ]\n",
        );
        assert!(rewrite(&ll).is_none(), "an addrspace pointer phi must be left to the emitter");
    }
}
