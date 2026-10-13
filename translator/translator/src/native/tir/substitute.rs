use super::*;
use crate::fxhash::HashMap;
use crate::native::ir::{LlGep, LlValue, TypedValue};
use crate::native::parse::{LlCall, LlSwitch};

type Substitutions = HashMap<String, TypedValue>;

fn lookup<'a>(substitutions: &'a Substitutions, name: &str) -> Option<&'a TypedValue> {
    if substitutions.len() == 1 {
        let (key, value) = substitutions.iter().next()?;
        return (key == name).then_some(value);
    }
    substitutions.get(name)
}

fn mentions_key(value: &LlValue, substitutions: &Substitutions) -> bool {
    match value {
        LlValue::Local(name) => lookup(substitutions, name).is_some(),
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => values
            .iter()
            .any(|value| mentions_key(&value.value, substitutions)),
        LlValue::Splat(value) => mentions_key(&value.value, substitutions),
        LlValue::Gep(gep) => {
            mentions_key(&gep.base.value, substitutions)
                || gep
                    .indices
                    .iter()
                    .any(|index| mentions_key(&index.value, substitutions))
        }
        LlValue::IntToPtr { source, .. } => mentions_key(&source.value, substitutions),
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
        | LlValue::Undef => false,
    }
}

fn rename_in_place(line: &mut String, tokens: &HashMap<String, String>) {
    if line.is_ascii() {
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut hit = false;
        while let Some(relative) = bytes[i..].iter().position(|&b| b == b'%') {
            let at = i + relative;
            let mut j = at + 1;
            while j < bytes.len() && crate::native::cfg::clone_crossarm::is_ident_byte(bytes[j]) {
                j += 1;
            }
            if tokens.contains_key(&line[at..j]) {
                hit = true;
                break;
            }
            i = j;
        }
        if !hit {
            return;
        }
    }
    *line = crate::native::cfg::rename_tokens(line, tokens);
}

fn substitute_typed_value(value: &mut TypedValue, substitutions: &Substitutions) {
    if let LlValue::Local(name) = &value.value {
        if let Some(replacement) = lookup(substitutions, name) {
            *value = replacement.clone();
            return;
        }
    }
    substitute_value(&mut value.value, substitutions);
}

fn substitute_value(value: &mut LlValue, substitutions: &Substitutions) {
    match value {
        LlValue::Local(name) => {
            if let Some(replacement) = lookup(substitutions, name) {
                *value = replacement.value.clone();
            }
        }
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                substitute_typed_value(value, substitutions);
            }
        }
        LlValue::Splat(value) => substitute_typed_value(value, substitutions),
        LlValue::Gep(gep) => substitute_gep(gep, substitutions),
        LlValue::IntToPtr { source, .. } => substitute_typed_value(source, substitutions),
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

fn substitute_gep(gep: &mut LlGep, substitutions: &Substitutions) {
    substitute_typed_value(&mut gep.base, substitutions);
    for index in &mut gep.indices {
        substitute_typed_value(index, substitutions);
    }
}

fn substitute_call(call: &mut LlCall, substitutions: &Substitutions) {
    for argument in &mut call.args {
        substitute_typed_value(argument, substitutions);
    }
}

fn substitute_operand(operand: &mut TirOperand, substitutions: &Substitutions) {
    match &*operand {
        TirOperand::Value { name, .. } if lookup(substitutions, name).is_none() => return,
        TirOperand::Const { value, .. }
            if !matches!(value, LlValue::Local(_)) && !mentions_key(value, substitutions) =>
        {
            return;
        }
        _ => {}
    }
    let Some(mut value) = operand.as_typed_value() else {
        return;
    };
    substitute_typed_value(&mut value, substitutions);
    *operand = operand_from_typed_value(&value);
}

fn local_uses(value: &LlValue, uses: &mut Vec<String>) {
    match value {
        LlValue::Local(name) => uses.push(name.clone()),
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                local_uses(&value.value, uses);
            }
        }
        LlValue::Splat(value) => local_uses(&value.value, uses),
        LlValue::Gep(gep) => {
            local_uses(&gep.base.value, uses);
            for index in &gep.indices {
                local_uses(&index.value, uses);
            }
        }
        LlValue::IntToPtr { source, .. } => local_uses(&source.value, uses),
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

fn replacement_token(value: &TypedValue) -> String {
    if let Some(rendered) = crate::native::render::render_value(&value.value) {
        return rendered;
    }
    match &value.value {
        LlValue::Hex(bits) => format!("0x{bits:016X}"),
        LlValue::Float(number) => format!("{number:e}"),
        LlValue::Float32Bits(bits) => format!("f0x{bits:08X}"),
        LlValue::HalfBits(bits) => format!("0xH{bits:04X}"),
        LlValue::BFloatBits(bits) => format!("0xR{bits:04X}"),
        LlValue::Zero if matches!(value.ty, LlType::Ptr(_)) => "null".to_string(),
        LlValue::Zero => "zeroinitializer".to_string(),
        LlValue::Undef => "undef".to_string(),
        LlValue::Vector(_) | LlValue::Array(_) | LlValue::Struct(_) | LlValue::Splat(_) => {
            "zeroinitializer".to_string()
        }
        LlValue::Gep(_) => "null".to_string(),
        LlValue::IntToPtr { .. } => "null".to_string(),
        LlValue::Local(_)
        | LlValue::Global(_)
        | LlValue::Bool(_)
        | LlValue::Int(_)
        | LlValue::SignedInt(_) => unreachable!("injectively rendered above"),
    }
}

fn token_map(substitutions: &Substitutions) -> HashMap<String, String> {
    substitutions
        .iter()
        .map(|(name, value)| (name.clone(), replacement_token(value)))
        .collect()
}

fn substitute_inst(
    inst: &mut TirInst,
    substitutions: &Substitutions,
    tokens: &HashMap<String, String>,
) {
    if let Some(stored_uses) = &mut inst.uses {
        if stored_uses
            .iter()
            .all(|name| lookup(substitutions, name).is_none())
        {
            stored_uses.dedup();
        } else {
            let mut uses = Vec::new();
            for name in &*stored_uses {
                match lookup(substitutions, name) {
                    Some(replacement) => local_uses(&replacement.value, &mut uses),
                    None => uses.push(name.clone()),
                }
            }
            uses.dedup();
            *stored_uses = uses;
        }
    }

    for operand in &mut inst.operands {
        substitute_operand(operand, substitutions);
    }
    match &mut inst.data.payload {
        TirInstData::Compare { rest, .. } => {
            if let Some(rest) = rest {
                rename_in_place(rest, tokens);
            }
        }
        TirInstData::Memory { load, store, .. } => {
            if let Some(load) = load {
                substitute_typed_value(&mut load.ptr, substitutions);
            }
            if let Some((object, pointer)) = store.as_deref_mut() {
                substitute_typed_value(object, substitutions);
                substitute_typed_value(pointer, substitutions);
            }
        }
        TirInstData::Gep { parsed, .. } => {
            if let Some(gep) = parsed {
                substitute_gep(gep, substitutions);
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
                substitute_call(call, substitutions);
            }
            if let Some(call) = alias_override {
                substitute_call(call, substitutions);
            }
            if let EmitScanData::Owned(result) = emit_scan {
                if let Ok(call) = result.as_mut() {
                    substitute_call(call, substitutions);
                }
            }
            for text in [void_line, value_error].into_iter().flatten() {
                rename_in_place(text, tokens);
            }
        }
        TirInstData::Phi {
            incoming,
            incoming_values,
            ..
        } => {
            if let Some((_, incoming)) = incoming {
                for (value, _) in incoming {
                    substitute_value(value, substitutions);
                }
            }
            if let Some(values) = incoming_values {
                for value in values {
                    substitute_value(value, substitutions);
                }
            }
        }
        TirInstData::Element { diag_line, .. } => {
            if let Some(line) = diag_line {
                rename_in_place(line, tokens);
            }
        }
        TirInstData::Bitcast { .. } => {}
        TirInstData::Select(arms) => {
            if let Some((true_value, false_value)) = arms.as_deref_mut() {
                substitute_typed_value(true_value, substitutions);
                substitute_typed_value(false_value, substitutions);
            }
        }
        TirInstData::Plain | TirInstData::Alloca(_) | TirInstData::Aggregate(_) => {}
    }
}

fn substitute_switch(switch: &mut LlSwitch, substitutions: &Substitutions) {
    substitute_typed_value(&mut switch.selector, substitutions);
    for (value, _) in &mut switch.cases {
        substitute_value(value, substitutions);
    }
}

impl TirBlock {
    fn substitute_values_impl(&mut self, substitutions: &Substitutions, include_phis: bool) {
        if substitutions.is_empty() {
            return;
        }
        let tokens = token_map(substitutions);
        for inst in &mut self.insts {
            if include_phis || !inst.is_phi() {
                substitute_inst(inst, substitutions, &tokens);
            }
        }
        match &mut self.terminator {
            TirTerminator::Br(_) | TirTerminator::Ret(None) | TirTerminator::Unreachable => {}
            TirTerminator::BrCond { cond, .. } => {
                if let Some(replacement) = tokens.get(cond) {
                    *cond = replacement.clone();
                }
            }
            TirTerminator::Switch { selector, .. } => {
                if let Some(replacement) = tokens.get(selector) {
                    *selector = replacement.clone();
                }
            }
            TirTerminator::Ret(Some(value)) => {
                if let Some(replacement) = tokens.get(value) {
                    *value = replacement.clone();
                }
            }
        }
        if let RetEmit::Value(value) = &mut self.ret {
            substitute_typed_value(value, substitutions);
        }
        if let Some(switch) = &mut self.switch {
            substitute_switch(switch, substitutions);
        }
    }

    pub(in crate::native) fn substitute_values(&mut self, substitutions: &Substitutions) {
        self.substitute_values_impl(substitutions, true);
    }

    pub(in crate::native) fn substitute_non_phi_values(&mut self, substitutions: &Substitutions) {
        self.substitute_values_impl(substitutions, false);
    }
}

#[cfg(test)]
mod b52_equiv_tests {
    use super::*;

    fn int(value: LlValue) -> TypedValue {
        TypedValue {
            ty: LlType::Int(32),
            value,
        }
    }

    fn full_operand(operand: &TirOperand, substitutions: &Substitutions) -> TirOperand {
        let Some(mut value) = operand.as_typed_value() else {
            return operand.clone();
        };
        substitute_typed_value(&mut value, substitutions);
        operand_from_typed_value(&value)
    }

    #[test]
    fn lookup_answers_what_get_answers_for_zero_one_and_two_entries() {
        let empty = Substitutions::default();
        let one = crate::fxhash::map_from([("%a".to_string(), int(LlValue::Int(1)))]);
        let mut two = one.clone();
        two.insert("%b".to_string(), int(LlValue::Int(2)));
        for map in [&empty, &one, &two] {
            for key in ["%a", "%b", "%c", "", "%"] {
                assert_eq!(
                    format!("{:?}", lookup(map, key)),
                    format!("{:?}", map.get(key)),
                    "key {key:?}"
                );
            }
        }
    }

    #[test]
    fn operand_fast_path_equals_the_round_trip_and_const_local_still_becomes_a_value() {
        let one = crate::fxhash::map_from([("%a".to_string(), int(LlValue::Int(7)))]);
        let mut two = one.clone();
        two.insert("%b".to_string(), int(LlValue::Local("%c".to_string())));
        let i32t = LlType::Int(32);
        let v2 = LlType::Vector(Box::new(i32t.clone()), 2);
        let cases = vec![
            TirOperand::Value {
                name: "%a".into(),
                ty: i32t.clone(),
            },
            TirOperand::Value {
                name: "%z".into(),
                ty: i32t.clone(),
            },
            TirOperand::Const {
                value: LlValue::Local("%z".into()),
                ty: i32t.clone(),
            },
            TirOperand::Const {
                value: LlValue::Int(3),
                ty: i32t.clone(),
            },
            TirOperand::Const {
                value: LlValue::Vector(vec![
                    int(LlValue::Local("%a".into())),
                    int(LlValue::Int(1)),
                ]),
                ty: v2.clone(),
            },
            TirOperand::Const {
                value: LlValue::Vector(vec![int(LlValue::Local("%q".into())), int(LlValue::Undef)]),
                ty: v2.clone(),
            },
            TirOperand::Const {
                value: LlValue::Struct(vec![int(LlValue::Local("%b".into()))]),
                ty: i32t.clone(),
            },
            TirOperand::Unresolved,
        ];
        let (mut unchanged, mut changed) = (0, 0);
        for map in [&one, &two] {
            for case in &cases {
                let want = format!("{:?}", full_operand(case, map));
                let mut got = case.clone();
                substitute_operand(&mut got, map);
                assert_eq!(format!("{got:?}"), want, "operand {case:?}");
                if want == format!("{case:?}") {
                    unchanged += 1;
                } else {
                    changed += 1;
                }
            }
        }
        assert!(
            unchanged >= 6 && changed >= 6,
            "unchanged {unchanged}, changed {changed}"
        );
    }

    #[test]
    fn mentions_key_is_exactly_whether_substitute_value_changes_the_value() {
        let map = crate::fxhash::map_from([("%a".to_string(), int(LlValue::Int(9)))]);
        let values = vec![
            LlValue::Local("%a".into()),
            LlValue::Local("%b".into()),
            LlValue::Global("@a".into()),
            LlValue::Int(1),
            LlValue::Undef,
            LlValue::Zero,
            LlValue::Struct(vec![int(LlValue::Int(1)), int(LlValue::Local("%a".into()))]),
            LlValue::Array(vec![int(LlValue::Local("%b".into()))]),
            LlValue::Vector(vec![int(LlValue::Struct(vec![int(LlValue::Local(
                "%a".into(),
            ))]))]),
        ];
        let (mut yes, mut no) = (0, 0);
        for value in values {
            let mut after = value.clone();
            substitute_value(&mut after, &map);
            let would_change = format!("{after:?}") != format!("{value:?}");
            assert_eq!(mentions_key(&value, &map), would_change, "value {value:?}");
            if would_change {
                yes += 1;
            } else {
                no += 1;
            }
        }
        assert!(yes >= 3 && no >= 5, "yes {yes}, no {no}");
    }

    #[test]
    fn rename_in_place_equals_rename_tokens_and_a_non_ascii_line_still_takes_the_full_path() {
        let tokens = crate::fxhash::map_from([("%a".to_string(), "7".to_string())]);
        let lines = [
            "icmp eq i32 %a, 1",
            "icmp eq i32 %ab, 1",
            "no percent here",
            "%",
            "%%a",
            "trailing %a",
            "%a.b %a_c %a",
            "caf\u{e9} %z",
            "caf\u{e9} %a",
            "",
        ];
        for line in lines {
            let want = crate::native::cfg::rename_tokens(line, &tokens);
            let mut got = line.to_string();
            rename_in_place(&mut got, &tokens);
            assert_eq!(got, want, "line {line:?}");
        }
        assert_ne!(
            crate::native::cfg::rename_tokens("caf\u{e9} %z", &tokens),
            "caf\u{e9} %z"
        );
        assert_eq!(
            crate::native::cfg::rename_tokens("icmp eq i32 %ab, 1", &tokens),
            "icmp eq i32 %ab, 1"
        );
    }
}
