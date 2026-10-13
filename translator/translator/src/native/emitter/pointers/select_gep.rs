use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_selected_pointer_gep(
        &mut self,
        name: &str,
        selected: &SelectedPointer,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&selected.ty)? else {
            return Ok(false);
        };
        let pointee = gep_pointee(source_ty, indices)?;
        let nullable_raw = match (&selected.true_value, &selected.false_value) {
            (LlValue::Zero, LlValue::Local(local)) | (LlValue::Local(local), LlValue::Zero) => {
                self.raw_offsets.contains_key(local)
            }
            _ => false,
        };
        if nullable_raw
            || self.selected_value_has_access_tree(&selected.true_value)
            || self.selected_value_has_access_tree(&selected.false_value)
        {
            let tree = self.build_selected_access_tree(
                selected,
                source_ty,
                indices,
                &mut HashSet::default(),
                instructions,
            )?;
            self.selected_access_trees.insert(name.to_string(), tree);
            self.pointer_pointees
                .insert(name.to_string(), pointee.clone());
            return Ok(true);
        }
        let true_is_null = matches!(selected.true_value, LlValue::Zero);
        let false_is_null = matches!(selected.false_value, LlValue::Zero);
        let true_storage = (!true_is_null)
            .then(|| self.pointer_storage_for(&selected.true_value, addrspace))
            .transpose()?;
        let false_storage = (!false_is_null)
            .then(|| self.pointer_storage_for(&selected.false_value, addrspace))
            .transpose()?;
        let Some(storage) = true_storage.or(false_storage) else {
            return Ok(false);
        };
        if let (Some(true_storage), Some(false_storage)) = (true_storage, false_storage) {
            if true_storage != false_storage {
                let raw_arm = |emitter: &Self, value: &LlValue| match value {
                    LlValue::Local(local) => emitter.raw_offsets.get(local).cloned(),
                    _ => None,
                };
                let true_raw = raw_arm(self, &selected.true_value)
                    .map(|raw| self.apply_raw_gep(raw, source_ty, indices))
                    .transpose()?;
                let false_raw = raw_arm(self, &selected.false_value)
                    .map(|raw| self.apply_raw_gep(raw, source_ty, indices))
                    .transpose()?;
                let true_ptr = if true_raw.is_none() {
                    let ptr_type = self.ptr_type_id(true_storage, &pointee)?;
                    Some(self.emit_selected_pointer_access_chain(
                        ptr_type,
                        &selected.true_value,
                        &selected.ty,
                        source_ty,
                        &pointee,
                        true_storage,
                        indices,
                        instructions,
                    )?)
                } else {
                    None
                };
                let false_ptr = if false_raw.is_none() {
                    let ptr_type = self.ptr_type_id(false_storage, &pointee)?;
                    Some(self.emit_selected_pointer_access_chain(
                        ptr_type,
                        &selected.false_value,
                        &selected.ty,
                        source_ty,
                        &pointee,
                        false_storage,
                        indices,
                        instructions,
                    )?)
                } else {
                    None
                };
                self.selected_load_pointers.insert(
                    name.to_string(),
                    SelectedLoadPointer {
                        cond: selected.cond,
                        true_ptr,
                        false_ptr,
                        true_storage,
                        false_storage,
                        pointee: pointee.clone(),
                        true_raw,
                        false_raw,
                    },
                );
                self.pointer_pointees.insert(name.to_string(), pointee);
                return Ok(true);
            }
        }
        if !matches!(
            storage,
            StorageClass::StorageBuffer | StorageClass::UniformConstant
        ) {
            return Ok(false);
        }
        if true_is_null || false_is_null {
            return self.emit_nullable_selected_pointer_gep(
                name,
                selected,
                storage,
                &pointee,
                source_ty,
                indices,
                instructions,
            );
        }
        let (true_raw, false_raw) = match (&selected.true_value, &selected.false_value) {
            (LlValue::Local(true_name), LlValue::Local(false_name)) => match (
                self.raw_offsets.get(true_name).cloned(),
                self.raw_offsets.get(false_name).cloned(),
            ) {
                (Some(true_raw), Some(false_raw)) => (
                    Some(self.apply_raw_gep(true_raw, source_ty, indices)?),
                    Some(self.apply_raw_gep(false_raw, source_ty, indices)?),
                ),
                _ => (None, None),
            },
            _ => (None, None),
        };
        if let (Some(true_raw), Some(false_raw)) = (true_raw.clone(), false_raw.clone()) {
            if self.emit_selected_raw_pointer_gep(
                name,
                selected.cond,
                addrspace,
                storage,
                &pointee,
                true_raw,
                false_raw,
                instructions,
            )? {
                return Ok(true);
            }
        }
        let use_raw_arms = pointee == LlType::Int(8) && true_raw.is_some() && false_raw.is_some();
        let ptr_type = (!use_raw_arms)
            .then(|| self.ptr_type_id(storage, &pointee))
            .transpose()?;
        let true_ptr = if let Some(ptr_type) = ptr_type {
            Some(self.emit_selected_pointer_access_chain(
                ptr_type,
                &selected.true_value,
                &selected.ty,
                source_ty,
                &pointee,
                storage,
                indices,
                instructions,
            )?)
        } else {
            None
        };
        let false_ptr = if let Some(ptr_type) = ptr_type {
            Some(self.emit_selected_pointer_access_chain(
                ptr_type,
                &selected.false_value,
                &selected.ty,
                source_ty,
                &pointee,
                storage,
                indices,
                instructions,
            )?)
        } else {
            None
        };
        self.selected_load_pointers.insert(
            name.to_string(),
            SelectedLoadPointer {
                cond: selected.cond,
                true_ptr,
                false_ptr,
                true_storage: storage,
                false_storage: storage,
                pointee: pointee.clone(),
                true_raw,
                false_raw,
            },
        );
        self.pointer_storage.insert(name.to_string(), storage);
        self.pointer_pointees.insert(name.to_string(), pointee);
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    fn selected_value_has_access_tree(&self, value: &LlValue) -> bool {
        matches!(value, LlValue::Local(name) if self.selected_pointers.contains_key(name) || self.selected_access_trees.contains_key(name))
    }

    pub(in crate::native::emitter) fn build_selected_access_tree(
        &mut self,
        selected: &SelectedPointer,
        source_ty: &LlType,
        indices: &[TypedValue],
        visiting: &mut HashSet<String>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<SelectedAccessTree, String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&selected.ty)? else {
            return Err("native emitter: selected access tree source is not a pointer".to_string());
        };
        let pointee = gep_pointee(source_ty, indices)?;
        let shape = SelectedGepShape {
            source_ty,
            pointee: &pointee,
            indices,
        };
        let true_arm = self.build_selected_access_arm(
            &selected.true_value,
            &selected.ty,
            addrspace,
            &shape,
            visiting,
            instructions,
        )?;
        let false_arm = self.build_selected_access_arm(
            &selected.false_value,
            &selected.ty,
            addrspace,
            &shape,
            visiting,
            instructions,
        )?;
        Ok(SelectedAccessTree {
            cond: selected.cond,
            true_arm,
            false_arm,
            pointee,
        })
    }

    fn build_selected_access_arm(
        &mut self,
        value: &LlValue,
        pointer_ty: &LlType,
        addrspace: u32,
        shape: &SelectedGepShape<'_>,
        visiting: &mut HashSet<String>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<SelectedAccessArm, String> {
        if matches!(value, LlValue::Zero) {
            return Ok(SelectedAccessArm::Null);
        }
        if let LlValue::Local(name) = value {
            if let Some(nested) = self.selected_pointers.get(name).cloned() {
                if !visiting.insert(name.clone()) {
                    return Err("native emitter: cyclic selected access tree".to_string());
                }
                let tree = self.build_selected_access_tree(
                    &nested,
                    shape.source_ty,
                    shape.indices,
                    visiting,
                    instructions,
                )?;
                visiting.remove(name);
                return Ok(SelectedAccessArm::Nested(Box::new(tree)));
            }
            if let Some(tree) = self.selected_access_trees.get(name).cloned() {
                let tree = self.apply_selected_access_tree_gep(
                    &tree,
                    shape.source_ty,
                    shape.indices,
                    instructions,
                )?;
                return Ok(SelectedAccessArm::Nested(Box::new(tree)));
            }
            if let Some(raw) = self.raw_offsets.get(name).cloned() {
                return Ok(SelectedAccessArm::Raw(self.apply_raw_gep(
                    raw,
                    shape.source_ty,
                    shape.indices,
                )?));
            }
        }
        let storage = self.pointer_storage_for(value, addrspace)?;
        let ptr_type = self.ptr_type_id(storage, shape.pointee)?;
        let ptr = self.emit_selected_pointer_access_chain(
            ptr_type,
            value,
            pointer_ty,
            shape.source_ty,
            shape.pointee,
            storage,
            shape.indices,
            instructions,
        )?;
        Ok(SelectedAccessArm::Typed { ptr, storage })
    }

    pub(in crate::native::emitter) fn emit_selected_access_tree_gep(
        &mut self,
        name: &str,
        tree: &SelectedAccessTree,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !types_compatible(&self.resolve_type(&tree.pointee)?, source_ty) {
            return Ok(false);
        }
        let tree = self.apply_selected_access_tree_gep(tree, source_ty, indices, instructions)?;
        self.pointer_pointees
            .insert(name.to_string(), tree.pointee.clone());
        self.selected_access_trees.insert(name.to_string(), tree);
        Ok(true)
    }

    fn apply_selected_access_tree_gep(
        &mut self,
        tree: &SelectedAccessTree,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<SelectedAccessTree, String> {
        let pointee = gep_pointee(source_ty, indices)?;
        let true_arm = self.apply_selected_access_arm_gep(
            &tree.true_arm,
            source_ty,
            &pointee,
            indices,
            instructions,
        )?;
        let false_arm = self.apply_selected_access_arm_gep(
            &tree.false_arm,
            source_ty,
            &pointee,
            indices,
            instructions,
        )?;
        Ok(SelectedAccessTree {
            cond: tree.cond,
            true_arm,
            false_arm,
            pointee,
        })
    }

    fn apply_selected_access_arm_gep(
        &mut self,
        arm: &SelectedAccessArm,
        source_ty: &LlType,
        pointee: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<SelectedAccessArm, String> {
        match arm {
            SelectedAccessArm::Typed { ptr, storage } => {
                let ptr_type = self.ptr_type_id(*storage, pointee)?;
                let ptr = self.emit_selected_pointer_access_chain_from_id(
                    ptr_type,
                    *ptr,
                    *storage,
                    source_ty,
                    pointee,
                    indices,
                    instructions,
                )?;
                Ok(SelectedAccessArm::Typed {
                    ptr,
                    storage: *storage,
                })
            }
            SelectedAccessArm::Raw(raw) => Ok(SelectedAccessArm::Raw(self.apply_raw_gep(
                raw.clone(),
                source_ty,
                indices,
            )?)),
            SelectedAccessArm::Nested(nested) => Ok(SelectedAccessArm::Nested(Box::new(
                self.apply_selected_access_tree_gep(nested, source_ty, indices, instructions)?,
            ))),
            SelectedAccessArm::Null => Ok(SelectedAccessArm::Null),
        }
    }

    pub(in crate::native::emitter) fn emit_selected_load_pointer_gep(
        &mut self,
        name: &str,
        selected: &SelectedLoadPointer,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        staged_emit(instructions, |instructions| {
            if !types_compatible(&self.resolve_type(&selected.pointee)?, source_ty) {
                return Ok(false);
            }
            let pointee = gep_pointee(source_ty, indices)?;
            let true_raw = selected
                .true_raw
                .clone()
                .map(|raw| self.apply_raw_gep(raw, source_ty, indices))
                .transpose()?;
            let false_raw = selected
                .false_raw
                .clone()
                .map(|raw| self.apply_raw_gep(raw, source_ty, indices))
                .transpose()?;
            let true_ptr_type = selected
                .true_ptr
                .is_some()
                .then(|| self.ptr_type_id(selected.true_storage, &pointee))
                .transpose()?;
            let false_ptr_type = selected
                .false_ptr
                .is_some()
                .then(|| self.ptr_type_id(selected.false_storage, &pointee))
                .transpose()?;
            let true_ptr = match (selected.true_ptr, true_ptr_type) {
                (Some(ptr), Some(ptr_type)) => {
                    Some(self.emit_selected_pointer_access_chain_from_id(
                        ptr_type,
                        ptr,
                        selected.true_storage,
                        source_ty,
                        &pointee,
                        indices,
                        instructions,
                    )?)
                }
                (None, _) => None,
                (Some(_), None) => return Ok(false),
            };
            let false_ptr = match (selected.false_ptr, false_ptr_type) {
                (Some(ptr), Some(ptr_type)) => {
                    Some(self.emit_selected_pointer_access_chain_from_id(
                        ptr_type,
                        ptr,
                        selected.false_storage,
                        source_ty,
                        &pointee,
                        indices,
                        instructions,
                    )?)
                }
                (None, _) => None,
                (Some(_), None) => return Ok(false),
            };
            if (true_ptr.is_none() && true_raw.is_none())
                || (false_ptr.is_none() && false_raw.is_none())
            {
                return Ok(false);
            }
            self.selected_load_pointers.insert(
                name.to_string(),
                SelectedLoadPointer {
                    cond: selected.cond,
                    true_ptr,
                    false_ptr,
                    true_storage: selected.true_storage,
                    false_storage: selected.false_storage,
                    pointee: pointee.clone(),
                    true_raw,
                    false_raw,
                },
            );
            if selected.true_storage == selected.false_storage {
                self.pointer_storage
                    .insert(name.to_string(), selected.true_storage);
            }
            self.pointer_pointees.insert(name.to_string(), pointee);
            if !self.pointer_phi_values.is_empty() {
                let is_null = self.const_bool(false)?;
                self.record_pointer_nullness(name.to_string(), is_null);
            }
            Ok(true)
        })
    }

    pub(in crate::native::emitter) fn emit_selected_raw_pointer_gep(
        &mut self,
        name: &str,
        cond: Word,
        addrspace: u32,
        storage: StorageClass,
        pointee: &LlType,
        true_raw: RawBufferOffset,
        false_raw: RawBufferOffset,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if true_raw.root != false_raw.root
            || true_raw.addrspace != false_raw.addrspace
            || true_raw.unmodelable
            || false_raw.unmodelable
        {
            return Ok(false);
        }

        let word_indexed =
            self.raw_pointer_word_aligned(&true_raw) && self.raw_pointer_word_aligned(&false_raw);
        let index_name = if word_indexed {
            raw_word_index_name(name)
        } else {
            raw_byte_index_name(name)
        };
        let true_index = if word_indexed {
            self.emit_raw_word_index(&true_raw, 0, instructions)?
        } else {
            self.emit_raw_byte_index(&true_raw, 0, instructions)?
        };
        let false_index = if word_indexed {
            self.emit_raw_word_index(&false_raw, 0, instructions)?
        } else {
            self.emit_raw_byte_index(&false_raw, 0, instructions)?
        };
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let index = self.result_id(&index_name, &LlType::Int(32))?;
        instructions.push(Self::inst(
            Op::Select,
            Some(uint_ty),
            Some(index),
            vec![
                Operand::IdRef(cond),
                Operand::IdRef(true_index),
                Operand::IdRef(false_index),
            ],
        ));

        self.raw_offsets.insert(
            name.to_string(),
            RawBufferOffset {
                const_off: 0,
                dyn_terms: vec![(
                    TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Local(index_name),
                    },
                    if word_indexed { 4 } else { 1 },
                )],
                root: true_raw.root,
                addrspace: true_raw.addrspace,
                unmodelable: false,
                device_addr_base: true_raw.device_addr_base,
            },
        );
        let placeholder = self.result_id(name, &LlType::Ptr(addrspace))?;
        self.emit_private_zero_pointer_value_at(
            placeholder,
            pointee,
            &format!("select_raw_pointer_placeholder name={name}"),
        )?;
        self.pointer_storage.insert(name.to_string(), storage);
        self.pointer_pointees
            .insert(name.to_string(), pointee.clone());
        self.unmodeled_pointers.insert(name.to_string());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_nullable_selected_pointer_gep(
        &mut self,
        name: &str,
        selected: &SelectedPointer,
        storage: StorageClass,
        pointee: &LlType,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let ptr_type = self.ptr_type_id(storage, pointee)?;
        let result = self.result_id(name, &selected.ty)?;
        let concrete_value = if matches!(selected.true_value, LlValue::Zero) {
            &selected.false_value
        } else {
            &selected.true_value
        };
        let base_id = self.value_id(concrete_value, &selected.ty)?;
        let source_type_id = self.type_id(source_ty)?;
        let source_compatible = self
            .word_result_type(base_id, instructions)
            .or_else(|| self.current_parameter_types.get(&base_id).copied())
            .and_then(|ty| self.type_pointer_pointee_id(ty))
            == Some(source_type_id);
        if !source_compatible {
            self.raw_buffer_layout_required = true;
            return Err("native emitter: nullable selected GEP requires a proven source layout; use raw byte-offset construction".into());
        }
        self.emit_selected_pointer_access_chain_from_concrete_arm(
            result,
            ptr_type,
            concrete_value,
            &selected.ty,
            source_ty,
            indices,
            instructions,
        )?;
        self.pointer_storage.insert(name.to_string(), storage);
        self.pointer_pointees
            .insert(name.to_string(), pointee.clone());
        if let Some(is_null) = self.selected_pointer_nullness_id(selected, instructions)? {
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn selected_pointer_nullness_id(
        &mut self,
        selected: &SelectedPointer,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let true_is_null = matches!(selected.true_value, LlValue::Zero);
        let false_is_null = matches!(selected.false_value, LlValue::Zero);
        if true_is_null == false_is_null {
            return Ok(None);
        }
        if true_is_null {
            return Ok(Some(selected.cond));
        }
        let bool_ty = self.type_id(&LlType::Bool)?;
        let is_null = self.fresh();
        instructions.push(Self::inst(
            Op::LogicalNot,
            Some(bool_ty),
            Some(is_null),
            vec![Operand::IdRef(selected.cond)],
        ));
        Ok(Some(is_null))
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_access_chain_from_concrete_arm(
        &mut self,
        result: Word,
        ptr_type: Word,
        base_value: &LlValue,
        base_ty: &LlType,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if let LlValue::Local(base_name) = base_value {
            if let Some(prev) = self.gep_provenance.get(base_name).cloned() {
                let name = format!("%air.selgep.{result}");
                if let Some(indices) =
                    self.compose_followup_gep(&name, &prev, source_ty, indices, instructions)?
                {
                    let pointee = gep_pointee(&prev.source_ty, &indices)?;
                    let op = self.pointer_arithmetic_access_chain_op(
                        base_value, base_ty, &pointee, &indices,
                    )?;
                    let mut ops = vec![Operand::IdRef(prev.root)];
                    for idx in gep_spirv_indices(&indices)? {
                        ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
                    }
                    instructions.push(Self::inst(op, Some(ptr_type), Some(result), ops));
                    return Ok(());
                }
            }
        }
        let base = self.value_id(base_value, base_ty)?;
        let pointee = gep_pointee(source_ty, indices)?;
        let op = self.pointer_arithmetic_access_chain_op(base_value, base_ty, &pointee, indices)?;
        let mut ops = vec![Operand::IdRef(base)];
        for idx in gep_spirv_indices(indices)? {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        instructions.push(Self::inst(op, Some(ptr_type), Some(result), ops));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_access_chain_from_id(
        &mut self,
        ptr_type: Word,
        base: Word,
        storage: StorageClass,
        source_ty: &LlType,
        pointee: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let pointee_resolved = self.resolve_type(pointee)?;
        if indices.len() > 1
            && self.pointer_id_already_at_scalar(
                base,
                &LlValue::Undef,
                &pointee_resolved,
                ptr_type,
                instructions,
                None,
            )
        {
            if let Some(result) = self.emit_flattened_scalar_arm_access_chain(
                ptr_type,
                base,
                source_ty,
                pointee,
                indices,
                instructions,
            )? {
                return Ok(result);
            }
        }
        let spirv_indices = gep_spirv_indices(indices)?;
        if spirv_indices.is_empty() {
            return Ok(base);
        }
        let op = pointer_arithmetic_access_chain_op_for_storage(storage, false, pointee, indices);
        let mut ops = vec![Operand::IdRef(base)];
        for idx in spirv_indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        let result = self.fresh();
        instructions.push(Self::inst(op, Some(ptr_type), Some(result), ops));
        Ok(result)
    }

    pub(in crate::native::emitter) fn pointer_arithmetic_access_chain_op(
        &self,
        base_value: &LlValue,
        base_ty: &LlType,
        pointee: &LlType,
        indices: &[TypedValue],
    ) -> Result<Op, String> {
        let LlType::Ptr(addrspace) = self.resolve_type(base_ty)? else {
            return Ok(Op::InBoundsAccessChain);
        };
        let storage = self.pointer_storage_for(base_value, addrspace)?;
        let base_is_indexed_container = match base_value {
            LlValue::Local(name) if self.selected_pointers.contains_key(name) => false,
            LlValue::Local(name) => self
                .values
                .get(name)
                .is_some_and(|(id, _)| self.is_indexed_container_root(*id, Some(storage))),
            LlValue::Global(name) => self
                .global_values
                .get(name)
                .is_some_and(|(id, _)| self.is_indexed_container_root(*id, Some(storage))),
            _ => false,
        };
        Ok(pointer_arithmetic_access_chain_op_for_storage(
            storage,
            base_is_indexed_container,
            pointee,
            indices,
        ))
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_access_chain(
        &mut self,
        ptr_type: Word,
        base_value: &LlValue,
        base_ty: &LlType,
        source_ty: &LlType,
        pointee: &LlType,
        storage: StorageClass,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let base = self.value_id(base_value, base_ty)?;
        let pointee_resolved = self.resolve_type(pointee)?;
        let base_pointee = match base_value {
            LlValue::Local(n) => self
                .pointer_pointees
                .get(n)
                .and_then(|p| self.resolve_type(p).ok()),
            _ => None,
        };
        let already_at_scalar = is_scalar_pointee(&pointee_resolved)
            && indices.len() > 1
            && self.pointer_id_already_at_scalar(
                base,
                base_value,
                &pointee_resolved,
                ptr_type,
                instructions,
                base_pointee.as_ref(),
            );
        if already_at_scalar {
            if let Some(result) = self.emit_flattened_scalar_arm_access_chain(
                ptr_type,
                base,
                source_ty,
                pointee,
                indices,
                instructions,
            )? {
                return Ok(result);
            }
        }
        let spirv_indices = gep_spirv_indices(indices)?;
        if spirv_indices.is_empty() {
            return Ok(base);
        }
        let op = pointer_arithmetic_access_chain_op_for_storage(storage, false, pointee, indices);
        let mut ops = vec![Operand::IdRef(base)];
        for idx in spirv_indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        let result = self.fresh();
        instructions.push(Self::inst(op, Some(ptr_type), Some(result), ops));
        Ok(result)
    }
}
