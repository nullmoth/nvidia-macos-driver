use super::*;
mod air_calls;
mod atomics;
use atomics::{atomic_requested_scope, atomic_requested_semantics, AtomicRole};
mod binary_ops;
mod conversions;
mod freeze_unary;
mod intrinsics;
mod operands;

fn is_coherent_air_store(callee: &str) -> bool {
    callee.starts_with("air.store.device_coherent.")
        || callee.starts_with("air.store.system_coherent.")
}

fn is_coherent_air_load(callee: &str) -> bool {
    callee.starts_with("air.load.device_coherent.")
        || callee.starts_with("air.load.system_coherent.")
}

fn air_i32_literal(value: &LlValue) -> Option<i64> {
    match value {
        LlValue::Int(value) => Some(*value as i64),
        LlValue::SignedInt(value) => Some(*value),
        _ => None,
    }
}

pub(in crate::native::emitter) fn bfloat_lanes(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::BFloat => Some(1),
        LlType::Vector(elem, n) if **elem == LlType::BFloat => Some(*n),
        _ => None,
    }
}

fn float_lanes(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Float => Some(1),
        LlType::Vector(elem, n) if **elem == LlType::Float => Some(*n),
        _ => None,
    }
}

pub(in crate::native::emitter) fn shaped_type(elem: LlType, n: u32) -> LlType {
    if n <= 1 {
        elem
    } else {
        LlType::Vector(Box::new(elem), n)
    }
}
