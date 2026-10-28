use super::*;

impl LlModule {
    pub(in crate::native) fn infer_pointer_pointees(&mut self) {
        for f in &self.functions {
            for ((name, ty), pointee) in f.params.iter().zip(&f.byval_param_pointees) {
                if matches!(ty, LlType::Ptr(_)) {
                    if let Some(pointee) = pointee {
                        self.ptr_pointees
                            .insert((f.name.clone(), name.clone()), pointee.clone());
                    }
                }
            }
            let params = f
                .params
                .iter()
                .map(|(param, _)| param.clone())
                .collect::<HashSet<_>>();
            let pointer_params = f
                .params
                .iter()
                .filter(|&(_param, ty)| matches!(ty, LlType::Ptr(_)))
                .map(|(param, _ty)| param.clone())
                .collect::<HashSet<_>>();
            let mut pointer_select_arms: HashMap<String, Vec<String>> = HashMap::default();
            let mut pointer_identity_aliases: HashMap<String, String> = HashMap::default();
            for inst in f.carrier_insts() {
                if let Some((result, base)) = inst.identity_ptr_bitcast() {
                    pointer_identity_aliases.insert(result.to_string(), base.to_string());
                    continue;
                }
                let Some(result) = &inst.result else {
                    continue;
                };
                let Some((true_value, false_value)) = inst.select_arms().as_deref() else {
                    continue;
                };
                if !matches!(true_value.ty, LlType::Ptr(_))
                    || !matches!(false_value.ty, LlType::Ptr(_))
                {
                    continue;
                }
                let (LlValue::Local(true_name), LlValue::Local(false_name)) =
                    (&true_value.value, &false_value.value)
                else {
                    continue;
                };
                pointer_select_arms
                    .insert(result.clone(), vec![true_name.clone(), false_name.clone()]);
            }

            let merge_arm_params = |arms: &[String]| -> Vec<String> {
                let mut pending = arms.to_vec();
                let mut visited = HashSet::default();
                let mut roots = Vec::new();
                while let Some(arm) = pending.pop() {
                    if !visited.insert(arm.clone()) {
                        continue;
                    }
                    if params.contains(&arm) {
                        roots.push(arm);
                    } else if let Some(nested) = pointer_select_arms.get(&arm) {
                        pending.extend(nested.iter().cloned());
                    } else if let Some(base) = pointer_identity_aliases.get(&arm) {
                        pending.push(base.clone());
                    }
                }
                roots
            };

            for inst in f.carrier_insts() {
                if let Some(gep) = &inst.gep() {
                    if let LlValue::Local(name) = &gep.base.value {
                        let mut voters = Vec::new();
                        if params.contains(name) {
                            voters.push(name.clone());
                        }
                        if let Some(arms) = pointer_select_arms.get(name) {
                            voters.extend(merge_arm_params(arms));
                        }
                        for voter in voters {
                            let key = (f.name.clone(), voter);
                            if self.gep_source_should_override(&key, &gep.source_ty) {
                                self.metadata_pointee_params.remove(&key);
                                self.ptr_pointees.insert(key, gep.source_ty.clone());
                            } else {
                                self.ptr_pointees
                                    .entry(key)
                                    .or_insert_with(|| gep.source_ty.clone());
                            }
                        }
                    }
                    continue;
                }
                if let Some(load) = &inst.load() {
                    if let LlValue::Local(name) = &load.ptr.value {
                        if params.contains(name) {
                            self.ptr_pointees
                                .entry((f.name.clone(), name.clone()))
                                .or_insert_with(|| load.result_ty.clone());
                        }
                    }
                    continue;
                }
                if let Some((object, pointer)) = inst.store().as_deref() {
                    if let LlValue::Local(name) = &pointer.value {
                        if params.contains(name) {
                            self.ptr_pointees
                                .entry((f.name.clone(), name.clone()))
                                .or_insert_with(|| object.ty.clone());
                        }
                    }
                }
            }

            let mut sources = self.infer_local_pointer_table_param_pointees(f, &pointer_params);
            let mut roots = pointer_params
                .iter()
                .map(|param| (param.clone(), param.clone()))
                .collect::<HashMap<_, _>>();
            let mut changed = true;
            while changed {
                changed = false;
                for inst in f.carrier_insts() {
                    if let Some((res, base)) = inst.identity_ptr_bitcast() {
                        if let Some(root) = roots.get(base).cloned() {
                            if roots.insert(res.to_string(), root).is_none() {
                                changed = true;
                            }
                        }
                        continue;
                    }

                    if let Some((object, pointer)) = inst.store().as_deref() {
                        if let LlValue::Local(name) = &pointer.value {
                            if let Some(root) = roots.get(name).cloned() {
                                sources
                                    .entry(root)
                                    .or_default()
                                    .insert(self.resolve_known_type(&object.ty));
                            }
                        }
                    }

                    if let Some((element, pointers)) = simdgroup_matrix_block_element(inst) {
                        let element = self.resolve_known_type(&element);
                        for pointer in pointers {
                            if let Some(root) = roots.get(&pointer).cloned() {
                                sources.entry(root).or_default().insert(element.clone());
                            }
                        }
                    }

                    let Some(res) = &inst.result else {
                        continue;
                    };
                    if let Some(gep) = &inst.gep() {
                        let LlValue::Local(base) = &gep.base.value else {
                            continue;
                        };
                        let Some(root) = roots.get(base).cloned() else {
                            continue;
                        };
                        sources
                            .entry(root.clone())
                            .or_default()
                            .insert(self.resolve_known_type(&gep.source_ty));
                        if roots.insert(res.clone(), root).is_none() {
                            changed = true;
                        }
                        continue;
                    }
                    if let Some(incoming) = inst.phi_values() {
                        let mut root: Option<String> = None;
                        for value in incoming {
                            let LlValue::Local(name) = value else {
                                continue;
                            };
                            let Some(candidate) = roots.get(name).cloned() else {
                                continue;
                            };
                            match &root {
                                Some(existing) if existing != &candidate => {
                                    root = None;
                                    break;
                                }
                                None => root = Some(candidate),
                                _ => {}
                            }
                        }
                        if let Some(root) = root {
                            if roots.insert(res.clone(), root).is_none() {
                                changed = true;
                            }
                        }
                        continue;
                    }
                    if let Some((true_value, false_value)) = inst.select_arms().as_deref() {
                        let (LlValue::Local(true_name), LlValue::Local(false_name)) =
                            (&true_value.value, &false_value.value)
                        else {
                            continue;
                        };
                        let (Some(true_root), Some(false_root)) = (
                            roots.get(true_name).cloned(),
                            roots.get(false_name).cloned(),
                        ) else {
                            continue;
                        };
                        if true_root == false_root && roots.insert(res.clone(), true_root).is_none()
                        {
                            changed = true;
                        }
                    }
                }
            }

            for (param, seen) in sources {
                let mut seen = seen.into_iter().collect::<Vec<_>>();
                seen.sort_by_key(|ty| format!("{ty:?}"));
                seen.dedup();
                let [pointee] = seen.as_slice() else {
                    continue;
                };
                let key = (f.name.clone(), param);
                if self.gep_source_should_override(&key, pointee) {
                    self.metadata_pointee_params.remove(&key);
                    self.ptr_pointees.insert(key, pointee.clone());
                } else {
                    self.ptr_pointees
                        .entry(key)
                        .or_insert_with(|| pointee.clone());
                }
            }
        }

        let call_edges = self.param_call_edges();
        let mut changed = true;
        while changed {
            changed = false;
            let mut candidates: HashMap<(String, String), HashSet<LlType>> = HashMap::default();
            for edge in &call_edges {
                let Some(pointee) = self
                    .ptr_pointees
                    .get(&(edge.callee_func.clone(), edge.callee_param.clone()))
                    .cloned()
                else {
                    continue;
                };
                let key = (edge.caller_func.clone(), edge.caller_param.clone());
                candidates.entry(key).or_default().insert(pointee);
            }
            for (key, pointees) in candidates {
                let pointees = pointees.into_iter().collect::<Vec<_>>();
                let [pointee] = pointees.as_slice() else {
                    continue;
                };
                match self.ptr_pointees.get(&key).cloned() {
                    Some(existing)
                        if self.metadata_pointee_params.contains(&key)
                            && self.metadata_pointee_can_yield_to_call(&existing, pointee) =>
                    {
                        self.ptr_pointees.insert(key.clone(), pointee.clone());
                        self.metadata_pointee_params.remove(&key);
                        changed = true;
                    }
                    Some(_) => {}
                    None => {
                        self.ptr_pointees.insert(key, pointee.clone());
                        changed = true;
                    }
                }
            }
        }
    }

    pub(in crate::native) fn gep_source_should_override(
        &self,
        key: &(String, String),
        source_ty: &LlType,
    ) -> bool {
        if !self.metadata_pointee_params.contains(key) {
            return false;
        }
        let resolved = self.resolve_known_type(source_ty);
        if !matches!(resolved, LlType::Struct(_) | LlType::Array(_, _))
            || self.type_contains_pointer(&resolved)
        {
            return false;
        }
        let Some(&declared) = self.metadata_pointee_sizes.get(key) else {
            return false;
        };
        self.native_memcpy_type_size_align(&resolved)
            .is_some_and(|(size, _)| size == declared)
    }

    pub(in crate::native) fn metadata_pointee_can_yield_to_call(
        &self,
        metadata_pointee: &LlType,
        candidate: &LlType,
    ) -> bool {
        let metadata_pointee = self.resolve_known_type(metadata_pointee);
        let candidate = self.resolve_known_type(candidate);
        if self.type_contains_pointer(&metadata_pointee) || self.type_contains_pointer(&candidate) {
            return false;
        }
        if !matches!(metadata_pointee, LlType::Array(_, _) | LlType::Struct(_))
            || !matches!(candidate, LlType::Array(_, _) | LlType::Struct(_))
        {
            return false;
        }
        let Some((metadata_size, _)) = self.native_memcpy_type_size_align(&metadata_pointee) else {
            return false;
        };
        let Some((candidate_size, _)) = self.native_memcpy_type_size_align(&candidate) else {
            return false;
        };
        metadata_size == candidate_size
    }

    pub(in crate::native) fn infer_local_pointer_table_param_pointees(
        &self,
        f: &LlFunction,
        pointer_params: &HashSet<String>,
    ) -> HashMap<String, HashSet<LlType>> {
        type Path = Vec<(LlType, Vec<u64>)>;
        type Slot = (String, Path);
        let mut table_slots: HashMap<String, (String, Option<Path>)> = HashMap::default();
        let mut slot_params: HashMap<Slot, HashSet<String>> = HashMap::default();
        let mut table_params: HashMap<String, HashSet<String>> = HashMap::default();
        let mut loaded_params: HashMap<String, HashSet<String>> = HashMap::default();
        let mut sources: HashMap<String, HashSet<LlType>> = HashMap::default();

        for inst in f.carrier_insts() {
            if let Some(res) = &inst.result {
                if let Some(ty) = &inst.alloca_ty() {
                    if self.type_contains_pointer(ty) {
                        table_slots.insert(res.clone(), (res.clone(), Some(Vec::new())));
                    }
                    continue;
                }
                if let Some((bres, base)) = inst.identity_ptr_bitcast() {
                    if let Some(slot) = table_slots.get(base).cloned() {
                        table_slots.insert(bres.to_string(), slot);
                    }
                    continue;
                }
                if let Some(gep) = &inst.gep() {
                    let LlValue::Local(base) = &gep.base.value else {
                        continue;
                    };
                    if let Some((root, path)) = table_slots.get(base).cloned() {
                        let indices = gep
                            .indices
                            .iter()
                            .map(typed_value_u64)
                            .collect::<Option<Vec<_>>>();
                        let path = match (path, indices) {
                            (Some(mut path), Some(indices)) => {
                                path.push((self.resolve_known_type(&gep.source_ty), indices));
                                Some(path)
                            }
                            _ => None,
                        };
                        table_slots.insert(res.clone(), (root, path));
                    } else if let Some(params) = loaded_params.get(base) {
                        for param in params {
                            sources
                                .entry(param.clone())
                                .or_default()
                                .insert(self.resolve_known_type(&gep.source_ty));
                        }
                    }
                    continue;
                }
                if let Some(load) = &inst.load() {
                    if !matches!(load.result_ty, LlType::Ptr(_)) {
                        continue;
                    }
                    let LlValue::Local(ptr_name) = &load.ptr.value else {
                        continue;
                    };
                    let Some((root, path)) = table_slots.get(ptr_name) else {
                        continue;
                    };
                    let params = match path {
                        Some(path) => slot_params.get(&(root.clone(), path.clone())),
                        None => table_params.get(root),
                    };
                    if let Some(params) = params {
                        loaded_params.insert(res.clone(), params.clone());
                    }
                }
                continue;
            }

            let Some((object, ptr)) = inst.store().as_deref() else {
                continue;
            };
            let LlValue::Local(param) = &object.value else {
                continue;
            };
            if !pointer_params.contains(param) {
                continue;
            }
            let LlValue::Local(ptr_name) = &ptr.value else {
                continue;
            };
            let Some((root, path)) = table_slots.get(ptr_name).cloned() else {
                continue;
            };
            table_params
                .entry(root.clone())
                .or_default()
                .insert(param.clone());
            if let Some(path) = path {
                slot_params
                    .entry((root, path))
                    .or_default()
                    .insert(param.clone());
            }
        }

        sources
    }
}

fn simdgroup_matrix_block_element(
    inst: &crate::native::tir::TirInst,
) -> Option<(LlType, Vec<String>)> {
    let call = inst.call();
    let call = call.as_deref()?;
    if !(call.callee.starts_with("air.simdgroup_matrix_8x8_load.")
        || call.callee.starts_with("air.simdgroup_matrix_8x8_store."))
    {
        return None;
    }
    let composite = match &inst.result_ty {
        Some(ty) => ty.clone(),
        None => call
            .args
            .iter()
            .find(|arg| !matches!(arg.ty, LlType::Ptr(_)))?
            .ty
            .clone(),
    };
    let LlType::Vector(element, 64) = composite else {
        return None;
    };
    let pointers = call
        .args
        .iter()
        .filter_map(|arg| match (&arg.ty, &arg.value) {
            (LlType::Ptr(_), LlValue::Local(name)) => Some(name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!pointers.is_empty()).then_some((*element, pointers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_table_slot_types_only_the_parameter_stored_in_it() {
        let ll = r#"
%Holder = type { ptr addrspace(2), ptr addrspace(2) }
%Payload = type { [2 x <4 x float>] }

define void @k(ptr addrspace(2) %first, ptr addrspace(2) %second) {
entry:
  %holder = alloca %Holder, align 8
  %store0 = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 0
  store ptr addrspace(2) %first, ptr %store0, align 8
  %store1 = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 1
  store ptr addrspace(2) %second, ptr %store1, align 8
  %load0 = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 0
  %loaded = load ptr addrspace(2), ptr %load0, align 8
  %element = getelementptr inbounds %Payload, ptr addrspace(2) %loaded, i64 0, i32 0, i64 1
  %value = load <4 x float>, ptr addrspace(2) %element, align 16
  ret void
}
"#;
        let module = LlModule::parse(ll).expect("parse pointer table");
        let payload = LlType::Struct(vec![LlType::Array(
            Box::new(LlType::Vector(Box::new(LlType::Float), 4)),
            2,
        )]);
        assert_eq!(
            module
                .ptr_pointees
                .get(&("k".to_string(), "%first".to_string())),
            Some(&payload)
        );
        assert_eq!(
            module
                .ptr_pointees
                .get(&("k".to_string(), "%second".to_string())),
            None
        );
    }

    #[test]
    fn local_pointer_field_access_replaces_equal_size_metadata_placeholder() {
        let ll = r#"
%Holder = type { ptr addrspace(2), ptr addrspace(2) }
%Payload = type { [2 x <4 x float>] }
%Other = type { [8 x i32] }

define void @k(ptr addrspace(2) %buffer, ptr addrspace(2) %other) {
entry:
  %holder = alloca %Holder, align 8
  %field = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 0
  store ptr addrspace(2) %buffer, ptr %field, align 8
  %other_field = getelementptr inbounds %Holder, ptr %holder, i64 0, i32 1
  store ptr addrspace(2) %other, ptr %other_field, align 8
  %loaded = load ptr addrspace(2), ptr %field, align 8
  %element = getelementptr inbounds %Payload, ptr addrspace(2) %loaded, i64 0, i32 0, i64 1
  %value = load <4 x float>, ptr addrspace(2) %element, align 16
  %other_loaded = load ptr addrspace(2), ptr %other_field, align 8
  %other_element = getelementptr inbounds %Other, ptr addrspace(2) %other_loaded, i64 0, i32 0, i64 1
  %other_value = load i32, ptr addrspace(2) %other_element, align 4
  ret void
}
"#;
        let mut module = LlModule::parse(ll).expect("parse pointer field closure");
        let key = ("k".to_string(), "%buffer".to_string());
        module
            .ptr_pointees
            .insert(key.clone(), LlType::Array(Box::new(LlType::Int(8)), 32));
        module.metadata_pointee_params.insert(key.clone());
        module.metadata_pointee_sizes.insert(key.clone(), 32);

        module.infer_pointer_pointees();

        assert_eq!(
            module.ptr_pointees.get(&key),
            Some(&LlType::Struct(vec![LlType::Array(
                Box::new(LlType::Vector(Box::new(LlType::Float), 4)),
                2,
            )]))
        );
        assert!(!module.metadata_pointee_params.contains(&key));
    }
}
