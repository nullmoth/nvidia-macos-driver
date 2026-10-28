use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_bda_address_phi(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        result_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let Some(result) = self.reserve_bda_address_phi(name, incoming, result_ty)? else {
            return Ok(false);
        };
        let address_ty = LlType::Int(64);
        let result_type = self.type_id(&address_ty)?;
        let mut ops = Vec::new();
        let mut seen_incoming: HashMap<Word, Word> = HashMap::default();
        for (value, label) in incoming {
            let label_id = self.label_id(label)?;
            let mut edge_instructions = Vec::new();
            let value_id = self.bda_phi_address_id(value, &mut edge_instructions)?;
            self.record_phi_edge_instructions(label_id, edge_instructions);
            if let Some(existing) = seen_incoming.insert(label_id, value_id) {
                if existing != value_id {
                    return Err(format!(
                        "native emitter: BDA address phi has multiple values from predecessor {label}"
                    ));
                }
                continue;
            }
            ops.push(Operand::IdRef(value_id));
            ops.push(Operand::IdRef(label_id));
        }
        instructions.push(Self::inst(Op::Phi, Some(result_type), Some(result), ops));
        self.emit_pointer_nullness_phi(name, incoming, result_ty, instructions)?;
        self.used_device_address = true;
        Ok(true)
    }

    pub(in crate::native::emitter) fn reserve_bda_address_phi(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        result_ty: &LlType,
    ) -> Result<Option<Word>, String> {
        let LlType::Ptr(addrspace @ 0..=2) = result_ty else {
            return Ok(None);
        };
        if !self.bda_device_pointers
            || !incoming
                .iter()
                .all(|(value, _)| self.bda_phi_value_is_addressable(value))
        {
            return Ok(None);
        }

        let result = self.result_id(&bda_address_name(name), &LlType::Int(64))?;
        if let Some((logical_result, _)) = self.values.get(name).cloned() {
            self.phi_result_instructions
                .retain(|instruction| instruction.result_id != Some(logical_result));
            self.emit_sidecar
                .remap_ids(&crate::fxhash::map_from([(logical_result, result)]));
        }
        let mut raw = RawBufferOffset::root(format!(".bda_phi_{name}"), *addrspace);
        raw.device_addr_base = Some(result);
        self.raw_offsets.insert(name.to_string(), raw);
        self.pointer_storage
            .insert(name.to_string(), StorageClass::PhysicalStorageBuffer);
        self.pointer_pointees
            .insert(name.to_string(), LlType::Int(8));
        self.gep_provenance.remove(name);
        Ok(Some(result))
    }

    pub(in crate::native::emitter) fn bda_phi_value_is_addressable(&self, value: &LlValue) -> bool {
        self.bda_phi_value_is_addressable_inner(value, &mut HashSet::default())
    }

    fn bda_phi_value_is_addressable_inner(
        &self,
        value: &LlValue,
        visiting: &mut HashSet<String>,
    ) -> bool {
        match value {
            LlValue::Zero | LlValue::Undef => true,
            LlValue::Local(name) => {
                if self.bda_inttoptr_sources.contains_key(name)
                    || self.bda_forward_addresses.contains(name)
                    || self.bda_direct_addresses.contains_key(name)
                    || self.raw_offsets.get(name).is_some_and(|raw| {
                        raw.const_off == 0
                            && raw.dyn_terms.is_empty()
                            && (raw.device_addr_base.is_some()
                                || self.bda_direct_addresses.contains_key(&raw.root))
                    })
                {
                    return true;
                }
                let Some(incoming) = self.tir_phi_incomings.get(name) else {
                    return false;
                };
                if !visiting.insert(name.clone()) {
                    return true;
                }
                let addressable = incoming
                    .iter()
                    .all(|(value, _)| self.bda_phi_value_is_addressable_inner(value, visiting));
                visiting.remove(name);
                addressable
            }
            _ => false,
        }
    }

    fn bda_phi_address_id(
        &mut self,
        value: &LlValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        match value {
            LlValue::Zero => self.const_signed_int(64, 0),
            LlValue::Undef => self.undef_id(&LlType::Int(64)),
            LlValue::Local(name) => {
                if let Some(address) = self.bda_direct_addresses.get(name).copied() {
                    return Ok(address);
                }
                if let Some(raw) = self.raw_offsets.get(name) {
                    if raw.const_off == 0 && raw.dyn_terms.is_empty() {
                        if let Some(address) = raw.device_addr_base {
                            return Ok(address);
                        }
                        if let Some(address) = self.bda_direct_addresses.get(&raw.root).copied() {
                            return Ok(address);
                        }
                    }
                }
                if let Some(raw) = self
                    .raw_offsets
                    .get(name)
                    .filter(|raw| raw.device_addr_base.is_some())
                    .cloned()
                {
                    let address = self.materialize_device_address(&raw, instructions)?;
                    self.bda_address_values.insert(address);
                    return Ok(address);
                }
                if self.bda_address_loads.contains(name) {
                    if !self.values.contains_key(name) {
                        let address = self.result_id(&bda_address_name(name), &LlType::Int(64))?;
                        self.bda_address_values.insert(address);
                        return Ok(address);
                    }
                    let result_ty = self.tir_result_types.get(name).cloned().ok_or_else(|| {
                        format!("native emitter: BDA pointer load {name} has no result type")
                    })?;
                    let address = self.result_id(name, &result_ty)?;
                    self.bda_address_values.insert(address);
                    return Ok(address);
                }
                if let Some(source) = self.bda_forward_sources.get(name).cloned() {
                    return self.bda_phi_address_id(&source.value, instructions);
                }
                if let (Some((true_value, false_value)), Some(condition)) = (
                    self.forward_pointer_selects.get(name).cloned(),
                    self.forward_pointer_select_conditions.get(name).cloned(),
                ) {
                    let true_address = self.bda_phi_address_id(&true_value.value, instructions)?;
                    let false_address =
                        self.bda_phi_address_id(&false_value.value, instructions)?;
                    let condition =
                        self.value_id_in(&condition.value, &condition.ty, instructions)?;
                    let address_type = self.type_id(&LlType::Int(64))?;
                    let address = self.fresh();
                    instructions.push(Self::inst(
                        Op::Select,
                        Some(address_type),
                        Some(address),
                        vec![
                            Operand::IdRef(condition),
                            Operand::IdRef(true_address),
                            Operand::IdRef(false_address),
                        ],
                    ));
                    self.bda_address_values.insert(address);
                    self.bda_direct_addresses.insert(name.clone(), address);
                    return Ok(address);
                }
                if let Some(gep) = self.forward_geps.get(name).cloned() {
                    let root = match &gep.base.value {
                        LlValue::Local(base_name) => base_name.clone(),
                        LlValue::Zero | LlValue::Undef => format!(".bda_forward_gep_{name}"),
                        other => {
                            return Err(if crate::env_vars::retry_debug() {
                                format!(
                                    "native emitter: forward BDA GEP {name} has an unsupported base {other:?} in {gep:?}"
                                )
                            } else {
                                "native emitter: forward BDA GEP has an unsupported base"
                                    .to_string()
                            });
                        }
                    };
                    let base_address = self.bda_phi_address_id(&gep.base.value, instructions)?;
                    let LlType::Ptr(addrspace) = self.resolve_type(&gep.base.ty)? else {
                        return Err(
                            "native emitter: forward BDA GEP base is not a pointer".to_string()
                        );
                    };
                    let source_type = self.resolve_type(&gep.source_ty)?;
                    let mut raw = RawBufferOffset::root(root, addrspace);
                    raw.device_addr_base = Some(base_address);
                    let raw = self.apply_raw_gep(raw, &source_type, &gep.indices)?;
                    if !raw.unmodelable {
                        let address = self.materialize_device_address(&raw, instructions)?;
                        self.bda_address_values.insert(address);
                        return Ok(address);
                    }
                }
                if let Some((pointer, LlType::Ptr(_))) = self.values.get(name).cloned() {
                    if self.pointer_storage.get(name) == Some(&StorageClass::PhysicalStorageBuffer)
                    {
                        let address_type = self.type_id(&LlType::Int(64))?;
                        let address = self.fresh();
                        instructions.push(Self::inst(
                            Op::ConvertPtrToU,
                            Some(address_type),
                            Some(address),
                            vec![Operand::IdRef(pointer)],
                        ));
                        self.bda_address_values.insert(address);
                        return Ok(address);
                    }
                }
                if let Some(source) = self.bda_inttoptr_sources.get(name).cloned() {
                    return self.phi_value_id(&source.value, &source.ty, &mut Vec::new());
                }
                if let (Some(incoming), Some(result_ty)) = (
                    self.tir_phi_incomings.get(name).cloned(),
                    self.tir_result_types.get(name).cloned(),
                ) {
                    if self
                        .reserve_bda_address_phi(name, &incoming, &result_ty)?
                        .is_some()
                    {
                        return self
                            .raw_offsets
                            .get(name)
                            .and_then(|raw| raw.device_addr_base)
                            .ok_or_else(|| {
                                format!(
                                    "native emitter: reserved BDA pointer {name} has no address result"
                                )
                            });
                    }
                }
                Err(format!(
                    "native emitter: pointer {name} has no exact BDA address representation"
                ))
            }
            _ => Err("native emitter: unsupported BDA address phi value".to_string()),
        }
    }

    pub(in crate::native::emitter) fn pointer_aware_type_id(
        &mut self,
        ty: &LlType,
        meta: Option<&PointerMeta>,
    ) -> Result<Word, String> {
        if let Some(PointerMeta {
            storage,
            pointee: Some(pointee),
        }) = meta
        {
            self.ptr_type_id(*storage, pointee)
        } else {
            self.type_id(ty)
        }
    }

    pub(in crate::native::emitter) fn pointer_merge_meta(
        &self,
        values: &[&LlValue],
        ty: &LlType,
    ) -> Result<Option<PointerMeta>, String> {
        let LlType::Ptr(addrspace) = ty else {
            return Ok(None);
        };
        let mut merged = PointerMeta {
            storage: llvm_pointer_storage(*addrspace)?,
            pointee: None,
        };
        let mut saw_meta = false;
        for value in values {
            let Some(meta) = self.pointer_meta_for_value(value, *addrspace)? else {
                continue;
            };
            if saw_meta && merged.storage != meta.storage {
                return Err(format!(
                    "native emitter: pointer merge storage mismatch {:?} vs {:?} at {value:?} across {values:?}",
                    merged.storage, meta.storage
                ));
            }
            saw_meta = true;
            merged.storage = meta.storage;
            if let Some(pointee) = meta.pointee {
                match &merged.pointee {
                    Some(existing) if existing != &pointee => {
                        return Err(format!(
                            "native emitter: pointer merge pointee mismatch {existing:?} vs {pointee:?} across {values:?}"
                        ));
                    }
                    None => merged.pointee = Some(pointee),
                    _ => {}
                }
            }
        }
        Ok(Some(merged))
    }

    pub(in crate::native::emitter) fn pointer_meta_for_value(
        &self,
        value: &LlValue,
        addrspace: u32,
    ) -> Result<Option<PointerMeta>, String> {
        match value {
            LlValue::Local(name) => {
                let network_pointee = self.network_pointees.get(name);
                Ok(self
                    .pointer_storage
                    .get(name)
                    .copied()
                    .map(|storage| PointerMeta {
                        storage,
                        pointee: network_pointee
                            .or_else(|| self.pointer_pointees.get(name))
                            .cloned(),
                    }))
            }
            LlValue::Global(name) => {
                let storage = match self.global_values.get(name) {
                    Some((_, LlType::Ptr(3))) => StorageClass::Workgroup,
                    _ => StorageClass::Private,
                };
                Ok(Some(PointerMeta {
                    storage,
                    pointee: None,
                }))
            }
            LlValue::Gep(gep) => {
                let LlType::Ptr(base_addrspace) = self.resolve_type(&gep.base.ty)? else {
                    return Err(format!(
                        "native emitter: getelementptr base is not a pointer: {:?}",
                        gep.base.ty
                    ));
                };
                Ok(Some(PointerMeta {
                    storage: self.pointer_storage_for(&gep.base.value, base_addrspace)?,
                    pointee: Some(gep_pointee(
                        &self.resolve_type(&gep.source_ty)?,
                        &gep.indices,
                    )?),
                }))
            }
            LlValue::Undef | LlValue::Zero => Ok(None),
            _ => Ok(Some(PointerMeta {
                storage: llvm_pointer_storage(addrspace)?,
                pointee: None,
            })),
        }
    }

    pub(in crate::native::emitter) fn pointer_in_pointer_merge(&self, name: &str) -> bool {
        if self.pointer_phi_values.contains(name)
            || self.pointer_phi_incoming_values.contains(name)
            || self.selected_pointers.contains_key(name)
        {
            return true;
        }
        self.selected_pointers.values().any(|sp| {
            matches!(&sp.true_value, LlValue::Local(n) if n == name)
                || matches!(&sp.false_value, LlValue::Local(n) if n == name)
        })
    }

    pub(in crate::native::emitter) fn pointer_pointee_for_value(
        &self,
        value: &LlValue,
    ) -> Result<Option<LlType>, String> {
        match value {
            LlValue::Local(name) => {
                if let Some(pointee) = self.pointer_pointees.get(name) {
                    if *pointee == LlType::Int(8)
                        && !self.raw_offsets.contains_key(name)
                        && !self.unmodeled_pointers.contains(name)
                        && !self.byte_view_pointers.contains(name)
                    {
                        if let Some(carrier) = self.tir_use_pointees.get(name) {
                            let carrier = self.resolve_type(carrier)?;
                            if carrier != LlType::Int(8) {
                                return Ok(Some(carrier));
                            }
                        }
                    }
                    if crate::env_vars::whole_part()
                        && is_scalar_pointee(pointee)
                        && !self.raw_offsets.contains_key(name)
                        && !self.unmodeled_pointers.contains(name)
                        && !self.byte_view_pointers.contains(name)
                        && !self.pointer_in_pointer_merge(name)
                    {
                        if let Some(carrier) = self.tir_use_pointees.get(name) {
                            let carrier = self.resolve_type(carrier)?;
                            if whole_part_widens(&carrier, pointee) {
                                return Ok(Some(carrier));
                            }
                        }
                    }
                    if crate::env_vars::reinterp_real()
                        && is_scalar_pointee(pointee)
                        && !self.raw_offsets.contains_key(name)
                        && !self.unmodeled_pointers.contains(name)
                        && !self.byte_view_pointers.contains(name)
                        && !self.pointer_in_pointer_merge(name)
                    {
                        if let Some(carrier) = self.tir_use_pointees.get(name) {
                            let carrier = self.resolve_type(carrier)?;
                            if reinterp_compatible(&carrier, pointee) {
                                return Ok(Some(carrier));
                            }
                        }
                    }
                    return Ok(Some(pointee.clone()));
                }
                Ok(self.tir_use_pointees.get(name).cloned())
            }
            LlValue::Global(name) => {
                if let Some(pointee) = self.pointer_pointees.get(name) {
                    return Ok(Some(pointee.clone()));
                }
                self.global_values
                    .get(name)
                    .map(|(_, ty)| match ty {
                        LlType::Ptr(_) => None,
                        other => Some(other.clone()),
                    })
                    .ok_or_else(|| format!("native emitter: unknown global value {name}"))
            }
            LlValue::Gep(gep) => Ok(Some(gep_pointee(
                &self.resolve_type(&gep.source_ty)?,
                &gep.indices,
            )?)),
            _ => Ok(None),
        }
    }

    pub(in crate::native::emitter) fn raw_only_induction_phi(
        &self,
        name: &str,
        incoming: &[(LlValue, String)],
        template: &RawBufferOffset,
    ) -> bool {
        let mut proving = crate::fxhash::set_from([name.to_string()]);
        incoming
            .iter()
            .all(|(value, _)| self.raw_induction_arm(name, value, template, &mut proving))
    }

    fn raw_induction_arm(
        &self,
        phi: &str,
        value: &LlValue,
        template: &RawBufferOffset,
        proving: &mut HashSet<String>,
    ) -> bool {
        if matches!(value, LlValue::Zero) {
            return true;
        }
        let LlValue::Local(incoming_name) = value else {
            return false;
        };
        if let Some(raw) = self.raw_offsets.get(incoming_name) {
            return raw.root == template.root
                && raw.addrspace == template.addrspace
                && !raw.unmodelable;
        }
        if !proving.insert(incoming_name.clone()) {
            return true;
        }
        if let Some(gep) = self.forward_geps.get(incoming_name) {
            return self.raw_induction_arm(phi, &gep.base.value, template, proving);
        }
        if self
            .forward_select_recurrence_gep(incoming_name, phi)
            .is_some()
        {
            return true;
        }
        self.tir_phi_incomings
            .get(incoming_name)
            .is_some_and(|incoming| {
                incoming
                    .iter()
                    .all(|(value, _)| self.raw_induction_arm(phi, value, template, proving))
            })
    }

    pub(in crate::native::emitter) fn emit_raw_pointer_phi(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        result_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        let Some(template) = incoming.iter().find_map(|(value, _)| match value {
            LlValue::Local(name) => self.raw_offsets.get(name).cloned(),
            _ => None,
        }) else {
            return Ok(false);
        };
        if template.unmodelable {
            return Ok(false);
        }
        if self.network_pointees.contains_key(name)
            && !self.raw_only_induction_phi(name, incoming, &template)
        {
            return Ok(false);
        }
        let all_incoming_raw = incoming.iter().all(|(value, _)| match value {
            LlValue::Local(name) => self.raw_offsets.contains_key(name),
            LlValue::Zero => true,
            _ => false,
        });
        let word_indexed = all_incoming_raw
            && incoming.iter().all(|(value, _)| {
                matches!(value, LlValue::Zero)
                    || matches!(value, LlValue::Local(name) if self
                        .raw_offsets
                        .get(name)
                        .is_some_and(|raw| self.raw_pointer_word_aligned(raw)))
            });

        let index_ty = LlType::Int(32);
        let index_name = if word_indexed {
            raw_word_index_name(name)
        } else {
            raw_byte_index_name(name)
        };
        let result = self.result_id(&index_name, &index_ty)?;
        let result_type = self.type_id(&index_ty)?;
        let mut ops = Vec::new();
        let mut seen_incoming: HashMap<Word, Word> = HashMap::default();
        let mut pending_edges = Vec::new();
        for (value, label) in incoming {
            let mut edge_instructions = Vec::new();
            let value_id = match value {
                LlValue::Local(incoming_name) => {
                    if let Some(raw) = self.raw_offsets.get(incoming_name).cloned() {
                        if raw.root != template.root
                            || raw.addrspace != template.addrspace
                            || raw.unmodelable
                            || (word_indexed && !self.raw_pointer_word_aligned(&raw))
                        {
                            return Ok(false);
                        }
                        let incoming_index_name = if word_indexed {
                            raw_word_index_name(incoming_name)
                        } else {
                            raw_byte_index_name(incoming_name)
                        };
                        if self.values.contains_key(&incoming_index_name) {
                            self.value_id(&LlValue::Local(incoming_index_name), &LlType::Int(32))?
                        } else if word_indexed && raw.dyn_terms.is_empty() {
                            self.emit_raw_word_index(&raw, 0, &mut edge_instructions)?
                        } else if !word_indexed {
                            self.emit_raw_byte_index(&raw, 0, &mut edge_instructions)?
                        } else {
                            return Ok(false);
                        }
                    } else if self.values.contains_key(incoming_name) {
                        return Ok(false);
                    } else {
                        self.phi_value_id(
                            &LlValue::Local(if word_indexed {
                                raw_word_index_name(incoming_name)
                            } else {
                                raw_byte_index_name(incoming_name)
                            }),
                            &index_ty,
                            &mut edge_instructions,
                        )?
                    }
                }
                LlValue::Zero => self.const_uint(0)?,
                _ => return Ok(false),
            };
            let label_id = self.label_id(label)?;
            pending_edges.push((label_id, edge_instructions));
            if let Some(existing) = seen_incoming.insert(label_id, value_id) {
                if existing != value_id {
                    return Err(format!(
                        "native emitter: raw pointer phi has multiple offsets from predecessor {label}"
                    ));
                }
                continue;
            }
            ops.push(Operand::IdRef(value_id));
            ops.push(Operand::IdRef(label_id));
        }
        for (predecessor, edge_instructions) in pending_edges {
            self.record_phi_edge_instructions(predecessor, edge_instructions);
        }
        instructions.push(Self::inst(Op::Phi, Some(result_type), Some(result), ops));
        if incoming
            .iter()
            .any(|(value, _)| matches!(value, LlValue::Zero))
        {
            self.emit_pointer_nullness_phi(name, incoming, result_ty, instructions)?;
        }
        self.pointer_storage
            .insert(name.to_string(), llvm_pointer_storage(*addrspace)?);
        self.pointer_pointees
            .insert(name.to_string(), raw_buffer_block_type());
        self.raw_offsets.insert(
            name.to_string(),
            RawBufferOffset {
                const_off: 0,
                dyn_terms: vec![(
                    TypedValue {
                        ty: index_ty,
                        value: LlValue::Local(index_name),
                    },
                    if word_indexed { 4 } else { 1 },
                )],
                root: template.root.clone(),
                addrspace: template.addrspace,
                unmodelable: false,
                device_addr_base: template.device_addr_base,
            },
        );
        self.define_unmodeled_pointer_value(name, *addrspace, &LlType::Int(8))?;
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_unmodeled_pointer_phi(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        result_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        let has_unmodeled = incoming.iter().any(|(value, _)| match value {
            LlValue::Local(name) => {
                self.unmodeled_pointers.contains(name) || self.forward_gep_base_is_unmodeled(name)
            }
            _ => false,
        });
        if !has_unmodeled {
            return Ok(false);
        }
        self.emit_pointer_nullness_phi(name, incoming, result_ty, instructions)?;
        self.define_unmodeled_pointer_value(name, *addrspace, &LlType::Int(8))?;
        Ok(true)
    }

    pub(in crate::native::emitter) fn forward_gep_base_is_unmodeled(&self, name: &str) -> bool {
        let Some(gep) = self.forward_geps.get(name) else {
            return false;
        };
        let LlValue::Local(base) = &gep.base.value else {
            return false;
        };
        self.unmodeled_pointers.contains(base)
    }

    pub(in crate::native::emitter) fn pointer_value_actual_storage(
        &self,
        value: &LlValue,
        instructions: &[Instruction],
    ) -> Option<StorageClass> {
        let id = match value {
            LlValue::Local(name) => self.values.get(name).map(|(id, _)| *id),
            LlValue::Global(name) => self.global_values.get(name).map(|(id, _)| *id),
            _ => None,
        }?;
        let result_type = self
            .module
            .types_global_values
            .iter()
            .chain(instructions.iter())
            .find(|inst| inst.result_id == Some(id))?
            .result_type?;
        self.module.types_global_values.iter().find_map(|inst| {
            if inst.class.opcode != Op::TypePointer || inst.result_id != Some(result_type) {
                return None;
            }
            match inst.operands.first() {
                Some(Operand::StorageClass(storage)) => Some(*storage),
                _ => None,
            }
        })
    }

    pub(in crate::native::emitter) fn emit_pointer_phi_provenance(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<GepProvenance>, String> {
        let Some(template) = self.pointer_phi_template_provenance(name, incoming)? else {
            return Ok(None);
        };
        let first_index_is_pointer_arithmetic = template.indices.len() == 1
            && incoming.iter().any(|(value, _)| {
                let LlValue::Local(incoming_name) = value else {
                    return false;
                };
                self.forward_geps.get(incoming_name).is_some_and(
                    |gep| matches!(&gep.base.value, LlValue::Local(base) if base == name),
                ) || self
                    .forward_select_recurrence_gep(incoming_name, name)
                    .is_some()
            });
        let forward_index_ty = (template.indices.len() == 1).then(|| &template.indices[0].ty);
        let mut provenances = vec![None; incoming.len()];
        for (position, (value, _)) in incoming.iter().enumerate() {
            let LlValue::Local(value_name) = value else {
                continue;
            };
            if !self.values.contains_key(value_name) {
                continue;
            }
            let Some(provenance) =
                self.provenance_for_pointer_value(value, Some(&template), forward_index_ty)?
            else {
                return Ok(None);
            };
            if !compatible_pointer_provenance(&template, &provenance)
                || provenance
                    .indices
                    .iter()
                    .zip(&template.indices)
                    .any(|(index, template_index)| index.ty != template_index.ty)
            {
                return Ok(None);
            }
            provenances[position] = Some(provenance);
        }
        let prior_provenance = self.gep_provenance.get(name).cloned();
        let index_names = if template.indices.len() == 1 {
            vec![pointer_index_name(name)]
        } else {
            (0..template.indices.len())
                .map(|position| format!("{}.{position}", pointer_index_name(name)))
                .collect::<Vec<_>>()
        };
        let prior_index_values = index_names
            .iter()
            .map(|index_name| (index_name.clone(), self.values.get(index_name).cloned()))
            .collect::<Vec<_>>();
        if self
            .reserve_pointer_provenance_from_template(
                name,
                &template,
                first_index_is_pointer_arithmetic,
            )?
            .is_none()
        {
            return Ok(None);
        }
        let mut resolved_provenances = Vec::with_capacity(incoming.len());
        for ((value, label), preflight) in incoming.iter().zip(provenances) {
            let provenance = if let Some(provenance) = preflight {
                provenance
            } else if let Some(provenance) =
                self.provenance_for_pointer_value(value, Some(&template), forward_index_ty)?
            {
                provenance
            } else {
                self.restore_pointer_phi_reservation(name, prior_provenance, &prior_index_values);
                return Ok(None);
            };
            if !compatible_pointer_provenance(&template, &provenance)
                || provenance
                    .indices
                    .iter()
                    .zip(&template.indices)
                    .any(|(index, template_index)| index.ty != template_index.ty)
            {
                self.restore_pointer_phi_reservation(name, prior_provenance, &prior_index_values);
                return Ok(None);
            }
            resolved_provenances.push((provenance, label));
        }
        let mut merged_indices = Vec::with_capacity(template.indices.len());
        let mut selected_ty = template.source_ty.clone();
        for (position, template_index) in template.indices.iter().enumerate() {
            let Some(structural_literal) = structural_pointer_index(
                position,
                &mut selected_ty,
                template_index,
                first_index_is_pointer_arithmetic,
            ) else {
                self.restore_pointer_phi_reservation(name, prior_provenance, &prior_index_values);
                return Ok(None);
            };
            if structural_literal {
                if resolved_provenances.iter().any(|(provenance, _)| {
                    provenance.indices[position].value != template_index.value
                }) {
                    self.restore_pointer_phi_reservation(
                        name,
                        prior_provenance,
                        &prior_index_values,
                    );
                    return Ok(None);
                }
                merged_indices.push(template_index.clone());
                continue;
            }
            let index_name = if template.indices.len() == 1 {
                pointer_index_name(name)
            } else {
                format!("{}.{position}", pointer_index_name(name))
            };
            if resolved_provenances
                .iter()
                .all(|(provenance, _)| provenance.indices[position].value == template_index.value)
                && !self.values.contains_key(&index_name)
            {
                merged_indices.push(template_index.clone());
                continue;
            }
            let result = self.result_id(&index_name, &template_index.ty)?;
            let result_type = self.type_id(&template_index.ty)?;
            let mut ops = Vec::new();
            let mut seen_incoming: HashMap<Word, Word> = HashMap::default();
            let mut pending_edges = Vec::new();
            for (provenance, label) in &resolved_provenances {
                let index = &provenance.indices[position];
                let mut edge_instructions = Vec::new();
                let value_id =
                    self.phi_value_id(&index.value, &index.ty, &mut edge_instructions)?;
                let label_id = self.label_id(label)?;
                pending_edges.push((label_id, edge_instructions));
                if let Some(existing) = seen_incoming.insert(label_id, value_id) {
                    if existing != value_id {
                        return Err(format!(
                            "native emitter: pointer index phi has multiple values from predecessor {label}"
                        ));
                    }
                    continue;
                }
                ops.push(Operand::IdRef(value_id));
                ops.push(Operand::IdRef(label_id));
            }
            for (predecessor, edge_instructions) in pending_edges {
                self.record_phi_edge_instructions(predecessor, edge_instructions);
            }
            instructions.push(Self::inst(Op::Phi, Some(result_type), Some(result), ops));
            merged_indices.push(TypedValue {
                ty: template_index.ty.clone(),
                value: LlValue::Local(index_name),
            });
        }
        Ok(Some(GepProvenance {
            root: template.root,
            addrspace: template.addrspace,
            source_ty: template.source_ty,
            indices: merged_indices,
            root_indices: None,
            root_is_indexed_container: template.root_is_indexed_container,
        }))
    }

    fn restore_pointer_phi_reservation(
        &mut self,
        name: &str,
        prior_provenance: Option<GepProvenance>,
        prior_index_values: &[(String, Option<(Word, LlType)>)],
    ) {
        if let Some(provenance) = prior_provenance {
            self.gep_provenance.insert(name.to_string(), provenance);
        } else {
            self.gep_provenance.remove(name);
        }
        for (index_name, prior_value) in prior_index_values {
            if let Some(value) = prior_value {
                self.values.insert(index_name.clone(), value.clone());
            } else {
                self.values.remove(index_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incompatible_defined_phi_arm_does_not_leave_reserved_index() {
        let ir = LlModule::parse("define void @k() {\nentry:\n  ret void\n}\n")
            .expect("minimal module parses");
        let mut emitter = Emitter::new(ir);
        let pointer_ty = LlType::Ptr(1);
        emitter
            .values
            .insert("%modeled".into(), (10, pointer_ty.clone()));
        emitter.values.insert("%unmodeled".into(), (11, pointer_ty));
        emitter.gep_provenance.insert(
            "%modeled".into(),
            GepProvenance {
                root: 12,
                addrspace: 1,
                source_ty: LlType::Int(16),
                indices: vec![TypedValue {
                    ty: LlType::Int(64),
                    value: LlValue::Int(0),
                }],
                root_indices: None,
                root_is_indexed_container: false,
            },
        );

        let result = emitter
            .emit_pointer_phi_provenance(
                "%merged",
                &[
                    (LlValue::Local("%modeled".into()), "left".into()),
                    (LlValue::Local("%unmodeled".into()), "right".into()),
                ],
                &mut Vec::new(),
            )
            .expect("unsupported provenance declines cleanly");

        assert!(result.is_none());
        assert!(!emitter.values.contains_key(&pointer_index_name("%merged")));
        assert!(!emitter.gep_provenance.contains_key("%merged"));
    }
}
