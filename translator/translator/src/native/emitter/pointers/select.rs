use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_select_resolved(
        &mut self,
        cond: TypedValue,
        true_value: TypedValue,
        false_value: TypedValue,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(&true_value.ty)?;
        let false_ty = self.resolve_type(&false_value.ty)?;
        if !types_compatible(&result_ty, &false_ty) {
            return Err(format!(
                "native emitter: select arm type mismatch {result_ty:?} vs {false_ty:?}"
            ));
        }
        let cond_id = self.value_id_in(&cond.value, &cond.ty, instructions)?;
        if let LlType::Ptr(_) = result_ty {
            let arm_is_deferred = |value: &LlValue| {
                matches!(value, LlValue::Local(local)
                    if self.selected_pointers.contains_key(local)
                        || self.selected_access_trees.contains_key(local))
            };
            if arm_is_deferred(&true_value.value) || arm_is_deferred(&false_value.value) {
                self.selected_pointers.insert(
                    name.clone(),
                    SelectedPointer {
                        cond: cond_id,
                        true_value: true_value.value.clone(),
                        false_value: false_value.value.clone(),
                        ty: result_ty.clone(),
                    },
                );
                return Ok(());
            }
            if self.emit_raw_pointer_select_index(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
                instructions,
            )? {
                return Ok(());
            }
            if self.record_cross_storage_pointer_select(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
            )? {
                return Ok(());
            }
            if self.record_incompatible_pointee_load_select(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
            )? {
                return Ok(());
            }
        }
        let pointer_meta =
            self.pointer_merge_meta(&[&true_value.value, &false_value.value], &result_ty)?;
        let mut pointer_provenance = None;
        let nullable_data_arm = match (&true_value.value, &false_value.value) {
            (LlValue::Zero, LlValue::Local(local)) | (LlValue::Local(local), LlValue::Zero) => {
                self.data_buffer_params.contains(local) || self.raw_offsets.contains_key(local)
            }
            _ => false,
        };
        if nullable_data_arm && !self.pointer_phi_incoming_values.contains(&name) {
            let concrete = if matches!(true_value.value, LlValue::Zero) {
                &false_value.value
            } else {
                &true_value.value
            };
            if let LlType::Ptr(addrspace) = result_ty {
                let storage = self.pointer_storage_for(concrete, addrspace)?;
                if matches!(
                    storage,
                    StorageClass::StorageBuffer | StorageClass::UniformConstant
                ) {
                    self.selected_pointers.insert(
                        name,
                        SelectedPointer {
                            cond: cond_id,
                            true_value: true_value.value,
                            false_value: false_value.value,
                            ty: result_ty,
                        },
                    );
                    return Ok(());
                }
            }
        }
        if let LlType::Ptr(addrspace) = result_ty {
            if let Some(meta) = pointer_meta.as_ref() {
                let arm_storage_mismatch = |emitter: &Self,
                                            value: &LlValue|
                 -> Result<bool, String> {
                    if matches!(value, LlValue::Zero | LlValue::Undef) {
                        return Ok(false);
                    }
                    if let Some(storage) = emitter.pointer_value_actual_storage(value, instructions)
                    {
                        return Ok(storage != meta.storage);
                    }
                    if let LlValue::Local(name) = value {
                        if emitter.forward_gep_base_is_unmodeled(name) {
                            return Ok(true);
                        }
                    }
                    Ok(emitter
                        .pointer_meta_for_value(value, addrspace)?
                        .is_some_and(|arm_meta| arm_meta.storage != meta.storage))
                };
                if arm_storage_mismatch(self, &true_value.value)?
                    || arm_storage_mismatch(self, &false_value.value)?
                {
                    let pointee = meta.pointee.as_ref().unwrap_or(&LlType::Int(8)).clone();
                    self.define_unmodeled_pointer_value(&name, addrspace, &pointee)?;
                    return Ok(());
                }
            }
            if self.record_selected_storage_pointer(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
                pointer_meta.as_ref(),
                instructions,
            )? {
                return Ok(());
            }
            if self.record_deferred_buffer_pointer_select(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
                pointer_meta.as_ref(),
            )? {
                return Ok(());
            }
            let can_select_pointer = pointer_meta.as_ref().is_some_and(|meta| {
                matches!(
                    meta.storage,
                    StorageClass::StorageBuffer
                        | StorageClass::UniformConstant
                        | StorageClass::Workgroup
                        | StorageClass::PhysicalStorageBuffer
                )
            });
            if !can_select_pointer {
                pointer_provenance = self.emit_pointer_select_provenance(
                    &name,
                    &result_ty,
                    &true_value.value,
                    &false_value.value,
                    cond_id,
                    instructions,
                )?;
                if let Some(provenance) = pointer_provenance.clone() {
                    self.gep_provenance.insert(name.clone(), provenance);
                    if let Some(meta) = pointer_meta {
                        self.record_pointer_meta(name.clone(), meta);
                    }
                    if self.tracks_pointer_nullness() && !self.pointer_nullness.contains_key(&name)
                    {
                        let is_null = self.const_bool(false)?;
                        self.record_pointer_nullness(name, is_null);
                    }
                    return Ok(());
                }
                self.define_unmodeled_byte_pointer_value(&name, addrspace)?;
                return Ok(());
            }
            if self.try_emit_decayed_aggregate_pointer_select(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
                pointer_meta.as_ref(),
                instructions,
            )? {
                return Ok(());
            }
            pointer_provenance = self.emit_pointer_select_provenance(
                &name,
                &result_ty,
                &true_value.value,
                &false_value.value,
                cond_id,
                instructions,
            )?;
            if let (
                Some(
                    meta @ PointerMeta {
                        storage,
                        pointee: Some(pointee),
                    },
                ),
                Some(provenance),
            ) = (pointer_meta.as_ref(), pointer_provenance.as_ref())
            {
                let result_type = self.ptr_type_id(*storage, pointee)?;
                let result = self.result_id(&name, &result_ty)?;
                let op = pointer_arithmetic_access_chain_op_for_storage(
                    *storage,
                    provenance.root_is_indexed_container,
                    pointee,
                    &provenance.indices,
                );
                let mut operands = vec![Operand::IdRef(provenance.root)];
                for index in gep_spirv_indices(&provenance.indices)? {
                    operands.push(Operand::IdRef(self.value_id(&index.value, &index.ty)?));
                }
                instructions.push(Self::inst(op, Some(result_type), Some(result), operands));
                self.record_pointer_meta(name.clone(), meta.clone());
                self.gep_provenance.insert(name, provenance.clone());
                return Ok(());
            }
        }
        let result_type = self.pointer_aware_type_id(&result_ty, pointer_meta.as_ref())?;
        let result = self.result_id(&name, &result_ty)?;
        let true_id = if let Some(meta) = pointer_meta.as_ref() {
            self.pointer_select_arm_id(&true_value.value, &true_value.ty, meta, instructions)?
        } else {
            self.value_id_in(&true_value.value, &true_value.ty, instructions)?
        };
        let false_id = if let Some(meta) = pointer_meta.as_ref() {
            self.pointer_select_arm_id(&false_value.value, &false_value.ty, meta, instructions)?
        } else {
            self.value_id_in(&false_value.value, &false_value.ty, instructions)?
        };
        instructions.push(Self::inst(
            Op::Select,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(cond_id),
                Operand::IdRef(true_id),
                Operand::IdRef(false_id),
            ],
        ));
        if let Some(meta) = pointer_meta {
            let selected = SelectedPointer {
                ty: result_ty.clone(),
                cond: cond_id,
                true_value: true_value.value.clone(),
                false_value: false_value.value.clone(),
            };
            if !self.pointer_nullness.contains_key(&name) {
                if let Some(is_null) = self.selected_pointer_nullness_id(&selected, instructions)? {
                    self.record_pointer_nullness(name.clone(), is_null);
                }
            }
            self.record_pointer_meta(name.clone(), meta);
        }
        if let Some(provenance) = pointer_provenance {
            self.gep_provenance.insert(name.clone(), provenance);
        }
        let _ = self.emit_raw_pointer_select_index(
            &name,
            &result_ty,
            &true_value.value,
            &false_value.value,
            cond_id,
            instructions,
        )?;
        if matches!(result_ty, LlType::Int(_)) {
            self.record_int_alignment(
                &name,
                &result_ty,
                add_int_alignment(
                    self.int_value_alignment(&true_value.value),
                    self.int_value_alignment(&false_value.value),
                ),
            );
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn record_cross_storage_pointer_select(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        if matches!(true_value, LlValue::Zero | LlValue::Undef)
            || matches!(false_value, LlValue::Zero | LlValue::Undef)
            || self.pointer_phi_incoming_values.contains(name)
        {
            return Ok(false);
        }
        let true_storage = self.pointer_storage_for(true_value, *addrspace)?;
        let false_storage = self.pointer_storage_for(false_value, *addrspace)?;
        if true_storage == false_storage {
            return Ok(false);
        }
        let value_readable = |storage| {
            matches!(
                storage,
                StorageClass::Private
                    | StorageClass::UniformConstant
                    | StorageClass::StorageBuffer
                    | StorageClass::PhysicalStorageBuffer
                    | StorageClass::Workgroup
            )
        };
        if !value_readable(true_storage) || !value_readable(false_storage) {
            return Ok(false);
        }
        let opaque_parameter_arm = |value: &LlValue| {
            matches!(value, LlValue::Local(local)
                if self.param_values.contains(local) && !self.data_buffer_params.contains(local))
        };
        let selected_has_data_pointee = self.tir_use_pointees.contains_key(name);
        if *addrspace == 2
            && !selected_has_data_pointee
            && (opaque_parameter_arm(true_value) || opaque_parameter_arm(false_value))
        {
            self.define_unmodeled_byte_pointer_value(name, *addrspace)?;
            return Ok(true);
        }
        self.selected_pointers.insert(
            name.to_string(),
            SelectedPointer {
                cond: cond_id,
                true_value: true_value.clone(),
                false_value: false_value.clone(),
                ty: result_ty.clone(),
            },
        );
        Ok(true)
    }

    fn record_incompatible_pointee_load_select(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        if !self.tir_direct_load_pointers.contains(name)
            || matches!(true_value, LlValue::Zero | LlValue::Undef)
            || matches!(false_value, LlValue::Zero | LlValue::Undef)
            || self.pointer_phi_incoming_values.contains(name)
        {
            return Ok(false);
        }
        let Some(true_pointee) = self.pointer_pointee_for_value(true_value)? else {
            return Ok(false);
        };
        let Some(false_pointee) = self.pointer_pointee_for_value(false_value)? else {
            return Ok(false);
        };
        if types_compatible(
            &self.resolve_type(&true_pointee)?,
            &self.resolve_type(&false_pointee)?,
        ) {
            return Ok(false);
        }
        let value_readable = |storage| {
            matches!(
                storage,
                StorageClass::Private
                    | StorageClass::UniformConstant
                    | StorageClass::StorageBuffer
                    | StorageClass::PhysicalStorageBuffer
                    | StorageClass::Workgroup
            )
        };
        if !value_readable(self.pointer_storage_for(true_value, *addrspace)?)
            || !value_readable(self.pointer_storage_for(false_value, *addrspace)?)
        {
            return Ok(false);
        }
        self.selected_pointers.insert(
            name.to_string(),
            SelectedPointer {
                cond: cond_id,
                true_value: true_value.clone(),
                false_value: false_value.clone(),
                ty: result_ty.clone(),
            },
        );
        Ok(true)
    }

    pub(in crate::native::emitter) fn pointer_select_arm_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
        meta: &PointerMeta,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let Some(pointee) = meta.pointee.as_ref() else {
            return self.value_id_in(value, ty, instructions);
        };
        match self.typed_null_or_undef_pointer_id(value, meta.storage, pointee, instructions)? {
            Some(id) => Ok(id),
            None => self.value_id_in(value, ty, instructions),
        }
    }

    pub(in crate::native::emitter) fn try_emit_decayed_aggregate_pointer_select(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
        meta: Option<&PointerMeta>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        if meta.and_then(|m| m.pointee.as_ref()).is_some() {
            return Ok(false);
        }
        let storage = llvm_pointer_storage(*addrspace)?;
        let (Some((elem_true, levels_true)), Some((elem_false, levels_false))) = (
            self.aggregate_pointer_arm_scalar_element(true_value)?,
            self.aggregate_pointer_arm_scalar_element(false_value)?,
        ) else {
            return Ok(false);
        };
        if elem_true != elem_false {
            return Ok(false);
        }
        let elem = elem_true;
        let result_type = self.ptr_type_id(storage, &elem)?;
        let true_id = self.decay_pointer_arm_to_element(
            true_value,
            *addrspace,
            storage,
            &elem,
            levels_true,
            instructions,
        )?;
        let false_id = self.decay_pointer_arm_to_element(
            false_value,
            *addrspace,
            storage,
            &elem,
            levels_false,
            instructions,
        )?;
        let result = self.result_id(name, result_ty)?;
        instructions.push(Self::inst(
            Op::Select,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(cond_id),
                Operand::IdRef(true_id),
                Operand::IdRef(false_id),
            ],
        ));
        self.record_pointer_meta(
            name.to_string(),
            PointerMeta {
                storage,
                pointee: Some(elem),
            },
        );
        Ok(true)
    }

    pub(in crate::native::emitter) fn aggregate_pointer_arm_scalar_element(
        &self,
        value: &LlValue,
    ) -> Result<Option<(LlType, usize)>, String> {
        if !matches!(value, LlValue::Local(_) | LlValue::Global(_)) {
            return Ok(None);
        }
        let Some(pointee) = self.pointer_pointee_for_value(value)? else {
            return Ok(None);
        };
        let mut ty = self.resolve_type(&pointee)?;
        let mut levels = 0usize;
        while let LlType::Array(elem, _) = ty {
            ty = self.resolve_type(&elem)?;
            levels += 1;
        }
        if levels == 0 {
            return Ok(None);
        }
        match ty {
            LlType::Int(_)
            | LlType::Float
            | LlType::Half
            | LlType::BFloat
            | LlType::Vector(_, _) => Ok(Some((ty, levels))),
            _ => Ok(None),
        }
    }

    pub(in crate::native::emitter) fn decay_pointer_arm_to_element(
        &mut self,
        value: &LlValue,
        addrspace: u32,
        storage: StorageClass,
        elem: &LlType,
        levels: usize,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let base = self.value_id_in(value, &LlType::Ptr(addrspace), instructions)?;
        let zero = self.const_uint(0)?;
        let mut ops = vec![Operand::IdRef(base)];
        for _ in 0..levels {
            ops.push(Operand::IdRef(zero));
        }
        let ptr_type = self.ptr_type_id(storage, elem)?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::AccessChain,
            Some(ptr_type),
            Some(result),
            ops,
        ));
        Ok(result)
    }

    pub(in crate::native::emitter) fn typed_null_or_undef_pointer_id(
        &mut self,
        value: &LlValue,
        storage: StorageClass,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if !matches!(value, LlValue::Zero | LlValue::Undef) {
            return Ok(None);
        }
        let ptr_type = self.ptr_type_id(storage, pointee)?;
        let id = self.fresh();
        if storage == StorageClass::PhysicalStorageBuffer {
            let zero = self.const_signed_int(64, 0)?;
            instructions.push(Self::inst(
                Op::ConvertUToPtr,
                Some(ptr_type),
                Some(id),
                vec![Operand::IdRef(zero)],
            ));
            return Ok(Some(id));
        }
        self.module.types_global_values.push(Self::inst(
            Op::ConstantNull,
            Some(ptr_type),
            Some(id),
            vec![],
        ));
        Ok(Some(id))
    }

    pub(in crate::native::emitter) fn record_selected_storage_pointer(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
        pointer_meta: Option<&PointerMeta>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        let Some(meta) = pointer_meta else {
            return Ok(false);
        };
        if self.pointer_phi_incoming_values.contains(name) {
            return Ok(false);
        }
        if !matches!(
            meta.storage,
            StorageClass::StorageBuffer | StorageClass::UniformConstant
        ) {
            return Ok(false);
        }
        let true_is_null = matches!(true_value, LlValue::Zero);
        let false_is_null = matches!(false_value, LlValue::Zero);
        if meta.pointee.is_none() {
            let concrete_data = match (true_value, false_value) {
                (LlValue::Zero, LlValue::Local(local)) | (LlValue::Local(local), LlValue::Zero) => {
                    self.data_buffer_params.contains(local) || self.raw_offsets.contains_key(local)
                }
                _ => false,
            };
            if !concrete_data {
                return Ok(false);
            }
        }
        if !(matches!(true_value, LlValue::Local(_)) || true_is_null)
            || !(matches!(false_value, LlValue::Local(_)) || false_is_null)
            || (true_is_null && false_is_null)
        {
            return Ok(false);
        }
        if !true_is_null
            && self
                .pointer_meta_for_value(true_value, *addrspace)?
                .as_ref()
                != Some(meta)
        {
            return Ok(false);
        }
        if !false_is_null
            && self
                .pointer_meta_for_value(false_value, *addrspace)?
                .as_ref()
                != Some(meta)
        {
            return Ok(false);
        }
        self.selected_pointers.insert(
            name.to_string(),
            SelectedPointer {
                cond: cond_id,
                true_value: true_value.clone(),
                false_value: false_value.clone(),
                ty: result_ty.clone(),
            },
        );
        self.record_pointer_meta(name.to_string(), meta.clone());
        if let Some(provenance) = self.emit_pointer_select_provenance(
            name,
            result_ty,
            true_value,
            false_value,
            cond_id,
            instructions,
        )? {
            self.gep_provenance.insert(name.to_string(), provenance);
        }
        if true_is_null || false_is_null {
            let is_null = if true_is_null {
                cond_id
            } else {
                let bool_ty = self.type_id(&LlType::Bool)?;
                let is_null = self.fresh();
                instructions.push(Self::inst(
                    Op::LogicalNot,
                    Some(bool_ty),
                    Some(is_null),
                    vec![Operand::IdRef(cond_id)],
                ));
                is_null
            };
            self.record_pointer_nullness(name.to_string(), is_null);
        } else if self.tracks_pointer_nullness() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn record_deferred_buffer_pointer_select(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
        pointer_meta: Option<&PointerMeta>,
    ) -> Result<bool, String> {
        let engage = pointer_meta.is_some_and(|meta| {
            meta.pointee.is_none()
                && matches!(
                    meta.storage,
                    StorageClass::StorageBuffer | StorageClass::UniformConstant
                )
        });
        if !engage {
            return Ok(false);
        }
        let (LlValue::Local(true_name), LlValue::Local(false_name)) = (true_value, false_value)
        else {
            return Ok(false);
        };
        let param_arm_is_opaque = |name: &String| {
            self.param_values.contains(name) && !self.data_buffer_params.contains(name)
        };
        if param_arm_is_opaque(true_name) || param_arm_is_opaque(false_name) {
            return Ok(false);
        }
        if self.pointer_phi_incoming_values.contains(name) || !self.pointer_phi_values.is_empty() {
            return Ok(false);
        }
        self.selected_pointers.insert(
            name.to_string(),
            SelectedPointer {
                cond: cond_id,
                true_value: true_value.clone(),
                false_value: false_value.clone(),
                ty: result_ty.clone(),
            },
        );
        let is_null = self.const_bool(false)?;
        self.record_pointer_nullness(name.to_string(), is_null);
        Ok(true)
    }
}
