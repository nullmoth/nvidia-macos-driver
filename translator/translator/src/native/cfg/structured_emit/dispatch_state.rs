use super::*;
use crate::fxhash::HashMap;
use crate::native::ir::{LlType, LlValue};
use crate::native::tir;

#[derive(Clone, Debug)]
pub(in crate::native) struct ValueSlot {
    pub(in crate::native) ty: LlType,
    pub(in crate::native) owner: usize,
    pub(in crate::native) original: String,
    pub(in crate::native) current: String,
    pub(in crate::native) next: String,
}

pub(in crate::native) fn typed_state_type(ty: &LlType) -> bool {
    match ty {
        LlType::Void | LlType::Ptr(_) | LlType::Named(_) => false,
        LlType::Vector(element, _) | LlType::Array(element, _) => typed_state_type(element),
        LlType::Struct(fields) => fields.iter().all(typed_state_type),
        LlType::Bool | LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_) => true,
    }
}

pub(in crate::native) fn passthrough(name: &str, target: &str) -> BodyBlock {
    synthetic_block(
        name.to_string(),
        vec![format!("br label {target}")],
        BlockRole::Normal,
    )
}

pub(in crate::native) fn route_passthrough(name: &str, target: &str) -> BodyBlock {
    synthetic_block(
        name.to_string(),
        vec![format!("br label {target}")],
        BlockRole::ConstructTreeRoute,
    )
}

pub(in crate::native) fn collect_value_locals(value: &LlValue, out: &mut Vec<String>) {
    match value {
        LlValue::Local(name) => out.push(name.clone()),
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                collect_value_locals(&value.value, out);
            }
        }
        LlValue::Splat(value) => collect_value_locals(&value.value, out),
        LlValue::Gep(gep) => {
            collect_value_locals(&gep.base.value, out);
            for index in &gep.indices {
                collect_value_locals(&index.value, out);
            }
        }
        LlValue::IntToPtr { source, .. } => collect_value_locals(&source.value, out),
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

pub(in crate::native) fn substitute_cross_slot_value(
    value: &mut LlValue,
    value_slots: &HashMap<&str, &ValueSlot>,
    source: usize,
) {
    match value {
        LlValue::Local(name) => {
            let Some(slot) = value_slots.get(name.as_str()) else {
                return;
            };
            if slot.owner != source {
                *value = LlValue::Local(slot.current.clone());
            }
        }
        LlValue::Vector(values) | LlValue::Array(values) | LlValue::Struct(values) => {
            for value in values {
                substitute_cross_slot_value(&mut value.value, value_slots, source);
            }
        }
        LlValue::Splat(value) => {
            substitute_cross_slot_value(&mut value.value, value_slots, source);
        }
        LlValue::Gep(gep) => {
            substitute_cross_slot_value(&mut gep.base.value, value_slots, source);
            for index in &mut gep.indices {
                substitute_cross_slot_value(&mut index.value, value_slots, source);
            }
        }
        LlValue::IntToPtr {
            source: operand, ..
        } => substitute_cross_slot_value(&mut operand.value, value_slots, source),
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

pub(in crate::native) fn terminator_uses(carrier: &tir::TirBlock) -> Vec<String> {
    match &carrier.terminator {
        tir::TirTerminator::Br(_)
        | tir::TirTerminator::Ret(None)
        | tir::TirTerminator::Unreachable => Vec::new(),
        tir::TirTerminator::BrCond { cond, .. }
        | tir::TirTerminator::Switch { selector: cond, .. }
        | tir::TirTerminator::Ret(Some(cond)) => vec![cond.clone()],
    }
}
