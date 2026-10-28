use super::*;
use crate::fxhash::HashMap;
use crate::native::ir::{LlGep, LlType, LlValue};
use spirv::StorageClass;

pub(in crate::native) fn addrspace_default_storage(addrspace: u32) -> Option<StorageClass> {
    match addrspace {
        0 | 4 => Some(StorageClass::Private),
        1 | 2 => Some(StorageClass::UniformConstant),
        3 => Some(StorageClass::Workgroup),
        _ => None,
    }
}

pub(in crate::native) fn derive_pointer_storage(
    tir: &TirFunction,
    params: &[(String, LlType)],
    named_types: &HashMap<String, LlType>,
) -> HashMap<String, StorageClass> {
    derive_pointer_storage_from(tir, params, named_types, &HashMap::default())
}

pub(in crate::native) fn derive_pointer_storage_from(
    tir: &TirFunction,
    params: &[(String, LlType)],
    named_types: &HashMap<String, LlType>,
    seeds: &HashMap<String, StorageClass>,
) -> HashMap<String, StorageClass> {
    let mut storage = seeds.clone();
    for (name, ty) in params {
        if let LlType::Ptr(addrspace) = ty {
            if let Some(s) = addrspace_default_storage(*addrspace) {
                storage.entry(name.clone()).or_insert(s);
            }
        }
    }
    loop {
        let mut changed = false;
        for block in &tir.blocks {
            for inst in &block.insts {
                let Some(result) = &inst.result else {
                    continue;
                };
                if storage.contains_key(result) {
                    continue;
                }
                let Some(LlType::Ptr(addrspace)) = tir.value_types.get(result) else {
                    continue;
                };
                let opcode = inst.opcode.as_str();
                let resolved = match opcode {
                    "alloca" => Some(StorageClass::Function),
                    "getelementptr" => inst
                        .gep()
                        .as_ref()
                        .and_then(|gep| gep_base_storage(gep, &storage, named_types)),
                    "bitcast" | "addrspacecast" => cast_source_storage(&inst.operands, &storage),
                    "freeze" => freeze_source_storage(&inst.operands, &storage),
                    "select" => select_arm_storage(&inst.operands, &storage),
                    "phi" => inst
                        .phi_values()
                        .and_then(|values| phi_incoming_storage(values, &storage)),
                    _ => addrspace_default_storage(*addrspace),
                };
                if let Some(s) = resolved {
                    storage.insert(result.clone(), s);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    storage
}

pub(in crate::native) fn gep_base_storage(
    gep: &LlGep,
    storage: &HashMap<String, StorageClass>,
    named_types: &HashMap<String, LlType>,
) -> Option<StorageClass> {
    let base = local_storage(&gep.base.value, storage);
    let source_ty = resolve_named(&gep.source_ty, named_types);
    if base == Some(StorageClass::Private)
        && matches!(source_ty, LlType::Array(_, _) | LlType::Struct(_))
        && type_contains_pointer(&source_ty, named_types)
    {
        return Some(StorageClass::Function);
    }
    base
}

pub(in crate::native) fn resolve_named(
    ty: &LlType,
    named_types: &HashMap<String, LlType>,
) -> LlType {
    match ty {
        LlType::Named(name) => named_types.get(name).cloned().unwrap_or_else(|| ty.clone()),
        _ => ty.clone(),
    }
}

pub(in crate::native) fn type_contains_pointer(
    ty: &LlType,
    named_types: &HashMap<String, LlType>,
) -> bool {
    match ty {
        LlType::Ptr(_) => true,
        LlType::Vector(elem, _) | LlType::Array(elem, _) => {
            type_contains_pointer(elem, named_types)
        }
        LlType::Struct(fields) => fields.iter().any(|f| type_contains_pointer(f, named_types)),
        LlType::Named(name) => named_types
            .get(name)
            .is_some_and(|t| type_contains_pointer(t, named_types)),
        _ => false,
    }
}

pub(in crate::native) fn cast_source_storage(
    operands: &[TirOperand],
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    operand_storage(operands.first(), storage)
}

pub(in crate::native) fn freeze_source_storage(
    operands: &[TirOperand],
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    operand_storage(operands.first(), storage)
}

pub(in crate::native) fn select_arm_storage(
    operands: &[TirOperand],
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    if operands.len() < 3 {
        return None;
    }
    merge_storage(
        operand_storage(operands.get(1), storage),
        operand_storage(operands.get(2), storage),
    )
}

pub(in crate::native) fn phi_incoming_storage<'a>(
    values: impl Iterator<Item = &'a LlValue>,
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    let mut acc: Option<StorageClass> = None;
    let mut any = false;
    for value in values {
        if let Some(s) = local_storage(value, storage) {
            acc = if any {
                merge_storage(acc, Some(s))
            } else {
                Some(s)
            };
            any = true;
        }
    }
    acc
}

pub(in crate::native) fn operand_storage(
    operand: Option<&TirOperand>,
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    operand
        .and_then(TirOperand::as_typed_value)
        .and_then(|tv| local_storage(&tv.value, storage))
}

pub(in crate::native) fn local_storage(
    value: &LlValue,
    storage: &HashMap<String, StorageClass>,
) -> Option<StorageClass> {
    match value {
        LlValue::Local(name) | LlValue::Global(name) => storage.get(name).copied(),
        _ => None,
    }
}

pub(in crate::native) fn merge_storage(
    a: Option<StorageClass>,
    b: Option<StorageClass>,
) -> Option<StorageClass> {
    match (a, b) {
        (Some(x), Some(y)) if x == y => Some(x),
        (Some(_), Some(_)) => None,
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

pub(in crate::native) fn deref_implied_pointee(inst: &TirInst) -> Option<(&str, LlType)> {
    match inst.opcode.as_str() {
        "load" => {
            let pointee = inst.result_ty.clone()?;
            let ptr = operand_name(inst.operands.first()?)?;
            Some((ptr, pointee))
        }
        "store" => {
            let pointee = operand_type(inst.operands.first()?)?.clone();
            let ptr = operand_name(inst.operands.get(1)?)?;
            Some((ptr, pointee))
        }
        "getelementptr" => {
            let srcty = inst.gep_source_ty()?.clone();
            let ptr = operand_name(inst.operands.first()?)?;
            Some((ptr, srcty))
        }
        _ => None,
    }
}

pub(in crate::native) fn operand_name(operand: &TirOperand) -> Option<&str> {
    match operand {
        TirOperand::Value { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

pub(in crate::native) fn operand_type(operand: &TirOperand) -> Option<&LlType> {
    match operand {
        TirOperand::Value { ty, .. } | TirOperand::Const { ty, .. } => Some(ty),
        TirOperand::Unresolved => None,
    }
}

pub(in crate::native) fn record_use_pointee(
    map: &mut HashMap<String, LlType>,
    conflicts: &mut usize,
    ptr: &str,
    pointee: LlType,
) {
    match map.get(ptr) {
        None => {
            map.insert(ptr.to_string(), pointee);
        }
        Some(existing) if *existing == pointee => {}
        Some(existing) => {
            *conflicts += 1;
            if pointee_richness(&pointee) > pointee_richness(existing) {
                map.insert(ptr.to_string(), pointee);
            }
        }
    }
}

pub(in crate::native) fn pointee_richness(ty: &LlType) -> u8 {
    match ty {
        LlType::Struct(_) | LlType::Array(_, _) | LlType::Vector(_, _) => 3,
        LlType::Int(8) => 1,
        _ => 2,
    }
}
