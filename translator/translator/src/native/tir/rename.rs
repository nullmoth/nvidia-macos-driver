use super::*;
use crate::fxhash::HashMap;
use crate::native::ir::{LlGep, LlValue, TypedValue};
use crate::native::parse::{LlCall, LlSwitch};

type Map = HashMap<String, String>;

fn rn(name: &str, map: &Map) -> String {
    map.get(name).cloned().unwrap_or_else(|| name.to_string())
}

fn rn_in_place(name: &mut String, map: &Map) {
    if let Some(repl) = map.get(name.as_str()) {
        *name = repl.clone();
    }
}

fn rename_llvalue(v: &mut LlValue, map: &Map) {
    match v {
        LlValue::Local(n) => rn_in_place(n, map),
        LlValue::Vector(vs) | LlValue::Array(vs) | LlValue::Struct(vs) => {
            for tv in vs {
                rename_typed_value(tv, map);
            }
        }
        LlValue::Splat(b) => rename_typed_value(b, map),
        LlValue::Gep(g) => rename_gep(g, map),
        LlValue::IntToPtr { source, .. } => rename_typed_value(source, map),
        LlValue::Global(_)
        | LlValue::Bool(_)
        | LlValue::Int(_)
        | LlValue::SignedInt(_)
        | LlValue::Hex(_)
        | LlValue::Float(_)
        | LlValue::Float32Bits(_)
        | LlValue::HalfBits(_)
        | LlValue::BFloatBits(_)
        | LlValue::Zero
        | LlValue::Undef => {}
    }
}

pub(in crate::native) fn renamed_llvalue(v: &LlValue, map: &Map) -> LlValue {
    let mut out = v.clone();
    rename_llvalue(&mut out, map);
    out
}

pub(in crate::native) fn renamed_label(name: &str, map: &Map) -> String {
    rn(name, map)
}

fn rename_typed_value(tv: &mut TypedValue, map: &Map) {
    rename_llvalue(&mut tv.value, map);
}

fn rename_gep(g: &mut LlGep, map: &Map) {
    rename_typed_value(&mut g.base, map);
    for idx in &mut g.indices {
        rename_typed_value(idx, map);
    }
}

fn rename_call(c: &mut LlCall, map: &Map) {
    for arg in &mut c.args {
        rename_typed_value(arg, map);
    }
}

fn rename_operand(op: &mut TirOperand, map: &Map) {
    match op {
        TirOperand::Value { name, .. } => rn_in_place(name, map),
        TirOperand::Const { value, .. } => rename_llvalue(value, map),
        TirOperand::Unresolved => {}
    }
}

fn rename_terminator(t: &mut TirTerminator, map: &Map) {
    match t {
        TirTerminator::Br(target) => rn_in_place(target, map),
        TirTerminator::BrCond { cond, t, f } => {
            rn_in_place(cond, map);
            rn_in_place(t, map);
            rn_in_place(f, map);
        }
        TirTerminator::Switch {
            selector,
            default,
            cases,
        } => {
            rn_in_place(selector, map);
            rn_in_place(default, map);
            for (_, label) in cases {
                rn_in_place(label, map);
            }
        }
        TirTerminator::Ret(Some(v)) => rn_in_place(v, map),
        TirTerminator::Ret(None) | TirTerminator::Unreachable => {}
    }
}

fn rename_ret_emit(r: &mut RetEmit, map: &Map) {
    if let RetEmit::Value(tv) = r {
        rename_typed_value(tv, map);
    }
}

fn rename_switch(sw: &mut LlSwitch, map: &Map) {
    rename_typed_value(&mut sw.selector, map);
    rn_in_place(&mut sw.default_label, map);
    for (value, label) in &mut sw.cases {
        rename_llvalue(value, map);
        rn_in_place(label, map);
    }
}

fn rename_inst(inst: &mut TirInst, map: &Map) {
    if let Some(r) = &mut inst.result {
        rn_in_place(r, map);
    }
    if let Some(uses) = &mut inst.uses {
        for u in uses {
            rn_in_place(u, map);
        }
    }
    for op in &mut inst.operands {
        rename_operand(op, map);
    }
    match &mut inst.data.payload {
        TirInstData::Compare { rest, .. } => {
            if let Some(rest) = rest {
                *rest = crate::native::cfg::rename_tokens(rest, map);
            }
        }
        TirInstData::Memory { load, store, .. } => {
            if let Some(load) = load {
                rename_typed_value(&mut load.ptr, map);
            }
            if let Some((object, ptr)) = store.as_deref_mut() {
                rename_typed_value(object, map);
                rename_typed_value(ptr, map);
            }
        }
        TirInstData::Gep { parsed, .. } => {
            if let Some(gep) = parsed {
                rename_gep(gep, map);
            }
        }
        TirInstData::Call {
            parsed,
            void_line,
            value_error,
            alias_override,
            emit_scan,
            ..
        } => {
            if let Some(call) = parsed {
                rename_call(call, map);
            }
            if let Some(call) = alias_override {
                rename_call(call, map);
            }
            match emit_scan {
                EmitScanData::Owned(result) => match result.as_mut() {
                    Ok(call) => rename_call(call, map),
                    Err(message) => *message = crate::native::cfg::rename_tokens(message, map),
                },
                EmitScanData::None | EmitScanData::Parsed => {}
            }
            for text in [void_line, value_error].into_iter().flatten() {
                *text = crate::native::cfg::rename_tokens(text, map);
            }
        }
        TirInstData::Phi {
            incoming,
            incoming_values,
            ..
        } => {
            if let Some((_, incoming)) = incoming {
                for (value, predecessor) in incoming {
                    rename_llvalue(value, map);
                    rn_in_place(predecessor, map);
                }
            }
            if let Some(values) = incoming_values {
                for value in values {
                    rename_llvalue(value, map);
                }
            }
        }
        TirInstData::Element { diag_line, .. } => {
            if let Some(line) = diag_line {
                *line = crate::native::cfg::rename_tokens(line, map);
            }
        }
        TirInstData::Bitcast { destination, .. } => {
            if let Some(destination) = destination {
                *destination = crate::native::cfg::rename_tokens(destination, map);
            }
        }
        TirInstData::Select(arms) => {
            if let Some((true_value, false_value)) = arms.as_deref_mut() {
                rename_typed_value(true_value, map);
                rename_typed_value(false_value, map);
            }
        }
        TirInstData::Plain | TirInstData::Alloca(_) | TirInstData::Aggregate(_) => {}
    }
}

impl TirBlock {
    pub(in crate::native) fn rename(&mut self, map: &Map) {
        rn_in_place(&mut self.label, map);
        for inst in &mut self.insts {
            rename_inst(inst, map);
        }
        rename_terminator(&mut self.terminator, map);
        rename_ret_emit(&mut self.ret, map);
        if let Some(sw) = &mut self.switch {
            rename_switch(sw, map);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::cfg::rename_tokens;

    fn map(pairs: &[(&str, &str)]) -> Map {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn rename_matches_relowered_lines() {
        let types = HashMap::default();
        let m = map(&[
            ("%a", "%a.c"),
            ("%b", "%b.c"),
            ("%r", "%r.c"),
            ("%p", "%p.c"),
            ("%blk", "%blk.c"),
            ("%pred1", "%pred1.c"),
            ("%pred2", "%pred2.c"),
        ]);
        let cases: &[&[&str]] = &[
            &["%r = add i32 %a, %b", "br label %blk"],
            &["%r = icmp eq i32 %a, %b", "br i1 %r, label %blk, label %a"],
            &["%r = getelementptr i8, ptr %a, i32 %b", "ret ptr %r"],
            &[
                "%r = phi i32 [ %a, %pred1 ], [ %b, %pred2 ]",
                "switch i32 %r, label %blk [ i32 0, label %pred1 ]",
            ],
            &["%r = call i32 @f(i32 %a, i32 %b)", "ret i32 %r"],
            &[
                "%r = extractelement <4 x float> %a, i32 %b",
                "br label %blk",
            ],
            &["%r = bitcast i32 %a to float", "ret void"],
        ];
        for case in cases {
            let lines: Vec<String> = case.iter().map(|s| s.to_string()).collect();
            let mut carrier = lower_block_carrier("%blk", &lines, &types).unwrap();
            carrier.rename(&m);
            let renamed: Vec<String> = lines.iter().map(|l| rename_tokens(l, &m)).collect();
            let expected = lower_block_carrier("%blk.c", &renamed, &types).unwrap();
            assert_eq!(
                format!("{carrier:?}"),
                format!("{expected:?}"),
                "typed rename diverged from re-lower for {case:?}"
            );
        }
    }
}
