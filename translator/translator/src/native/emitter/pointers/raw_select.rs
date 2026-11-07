use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_selected_pointer_direct_load(
        &mut self,
        result: Word,
        result_ty: &LlType,
        selected: &SelectedPointer,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&selected.ty)? else {
            return Err("native emitter: selected load source is not a pointer".to_string());
        };
        let true_is_null = matches!(selected.true_value, LlValue::Zero);
        let false_is_null = matches!(selected.false_value, LlValue::Zero);
        let true_storage = (!true_is_null)
            .then(|| self.pointer_storage_for(&selected.true_value, addrspace))
            .transpose()?;
        let false_storage = (!false_is_null)
            .then(|| self.pointer_storage_for(&selected.false_value, addrspace))
            .transpose()?;
        let Some(storage) = true_storage.or(false_storage) else {
            return Err("native emitter: selected load has no concrete pointer arm".to_string());
        };
        let result_ty = self.resolve_type(result_ty)?;
        let result_type = self.type_id(&result_ty)?;
        self.emit_selected_load_choice(result, result_type, selected.cond, instructions,
            |this, when_true, instructions| {
                let (value, is_null, arm_storage) = if when_true {
                    (&selected.true_value, true_is_null, true_storage)
                } else {
                    (&selected.false_value, false_is_null, false_storage)
                };
                if is_null { this.const_null(&result_ty) }
                else { this.emit_selected_direct_load_arm_value(value, &selected.ty,
                    arm_storage.unwrap_or(storage), &result_ty, access_align, instructions) }
            })
    }

    // Load only the selected operand; materialize deferred GEP arms inside their branch.
    fn emit_selected_load_choice(
        &mut self,
        result: Word,
        result_type: Word,
        condition: Word,
        instructions: &mut Vec<Instruction>,
        mut arm: impl FnMut(&mut Self, bool, &mut Vec<Instruction>) -> Result<Word, String>,
    ) -> Result<(), String> {
        let true_label = self.fresh();
        let false_label = self.fresh();
        let merge_label = self.fresh();
        instructions.push(Self::inst(Op::SelectionMerge, None, None,
            vec![Operand::IdRef(merge_label), Operand::SelectionControl(SelectionControl::NONE)]));
        instructions.push(Self::inst(Op::BranchConditional, None, None,
            vec![Operand::IdRef(condition), Operand::IdRef(true_label), Operand::IdRef(false_label)]));
        instructions.push(Self::inst(Op::Label, None, Some(true_label), vec![]));
        let true_value = arm(self, true, instructions)?;
        let true_end = instructions.iter().rev().find(|i| i.class.opcode == Op::Label)
            .and_then(|i| i.result_id).expect("selected load true block");
        instructions.push(Self::inst(Op::Branch, None, None, vec![Operand::IdRef(merge_label)]));
        instructions.push(Self::inst(Op::Label, None, Some(false_label), vec![]));
        let false_value = arm(self, false, instructions)?;
        let false_end = instructions.iter().rev().find(|i| i.class.opcode == Op::Label)
            .and_then(|i| i.result_id).expect("selected load false block");
        instructions.push(Self::inst(Op::Branch, None, None, vec![Operand::IdRef(merge_label)]));
        instructions.push(Self::inst(Op::Label, None, Some(merge_label), vec![]));
        instructions.push(Self::inst(Op::Phi, Some(result_type), Some(result),
            vec![Operand::IdRef(true_value), Operand::IdRef(true_end),
                 Operand::IdRef(false_value), Operand::IdRef(false_end)]));
        Ok(())
    }

    fn emit_selected_direct_load_arm_value(
        &mut self,
        value: &LlValue,
        pointer_ty: &LlType,
        storage: StorageClass,
        load_ty: &LlType,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if let LlValue::Local(name) = value {
            if self.null_rooted_pointer_values.contains(name) {
                return self.undef_id(load_ty);
            }
            if let Some(selected) = self.selected_pointers.get(name).cloned() {
                let loaded = self.fresh();
                self.emit_selected_pointer_direct_load(
                    loaded,
                    load_ty,
                    &selected,
                    access_align,
                    instructions,
                )?;
                return Ok(loaded);
            }
            if let Some(raw) = self.raw_offsets.get(name).cloned() {
                let loaded = self.fresh();
                self.emit_raw_load(loaded, load_ty, &raw, access_align, instructions)?;
                return Ok(loaded);
            }
        }
        if let Some(pointee) = self.pointer_pointee_for_value(value)? {
            let pointee = self.resolve_type(&pointee)?;
            if let (LlType::Int(source_bits), LlType::Int(result_bits)) = (&pointee, load_ty) {
                if source_bits > result_bits {
                    let pointer = self.value_id(value, pointer_ty)?;
                    let wide = self.fresh();
                    instructions.push(Self::inst(
                        Op::Load,
                        Some(self.type_id(&pointee)?),
                        Some(wide),
                        vec![Operand::IdRef(pointer)],
                    ));
                    let narrowed = self.fresh();
                    instructions.push(Self::inst(
                        Op::UConvert,
                        Some(self.type_id(load_ty)?),
                        Some(narrowed),
                        vec![Operand::IdRef(wide)],
                    ));
                    return Ok(narrowed);
                }
            }
        }
        let ptr = self.selected_direct_load_arm_pointer(
            value,
            pointer_ty,
            storage,
            load_ty,
            instructions,
        )?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(self.type_id(load_ty)?),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));
        Ok(loaded)
    }

    fn selected_direct_load_arm_pointer(
        &mut self,
        value: &LlValue,
        pointer_ty: &LlType,
        storage: StorageClass,
        load_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if let Some(pointee) = self.pointer_pointee_for_value(value)? {
            if types_compatible(&self.resolve_type(&pointee)?, load_ty) {
                return self.value_id(value, pointer_ty);
            }
        }
        if let Some((element, levels)) = self.aggregate_pointer_arm_scalar_element(value)? {
            if types_compatible(&self.resolve_type(&element)?, load_ty) {
                let LlType::Ptr(addrspace) = self.resolve_type(pointer_ty)? else {
                    return Err("native emitter: selected load arm is not a pointer".to_string());
                };
                return self.decay_pointer_arm_to_element(
                    value,
                    addrspace,
                    storage,
                    &element,
                    levels,
                    instructions,
                );
            }
        }
        let pointee = self.pointer_pointee_for_value(value)?;
        Err(format!(
            "native emitter: selected direct load arm {value:?} with pointee {pointee:?} cannot address {load_ty:?}"
        ))
    }

    fn emit_guarded(
        &mut self,
        condition: Word,
        instructions: &mut Vec<Instruction>,
        body: impl FnOnce(&mut Self, &mut Vec<Instruction>) -> Result<(), String>,
    ) -> Result<(), String> {
        let body_label = self.fresh();
        let merge_label = self.fresh();
        instructions.push(Self::inst(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(merge_label),
                Operand::SelectionControl(SelectionControl::NONE),
            ],
        ));
        instructions.push(Self::inst(
            Op::BranchConditional,
            None,
            None,
            vec![
                Operand::IdRef(condition),
                Operand::IdRef(body_label),
                Operand::IdRef(merge_label),
            ],
        ));
        instructions.push(Self::inst(Op::Label, None, Some(body_label), vec![]));
        body(self, instructions)?;
        instructions.push(Self::inst(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(merge_label)],
        ));
        instructions.push(Self::inst(Op::Label, None, Some(merge_label), vec![]));
        Ok(())
    }

    fn store_arm_condition(
        &mut self,
        condition: Word,
        when_true: bool,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if when_true {
            return Ok(condition);
        }
        let bool_ty = self.type_id(&LlType::Bool)?;
        let negated = self.fresh();
        instructions.push(Self::inst(
            Op::LogicalNot,
            Some(bool_ty),
            Some(negated),
            vec![Operand::IdRef(condition)],
        ));
        Ok(negated)
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_direct_store(
        &mut self,
        object_ty: &LlType,
        object_id: Word,
        selected: &SelectedPointer,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&selected.ty)? else {
            return Err("native emitter: selected store target is not a pointer".to_string());
        };
        let true_is_null = matches!(selected.true_value, LlValue::Zero);
        let false_is_null = matches!(selected.false_value, LlValue::Zero);
        let true_storage = (!true_is_null)
            .then(|| self.pointer_storage_for(&selected.true_value, addrspace))
            .transpose()?;
        let false_storage = (!false_is_null)
            .then(|| self.pointer_storage_for(&selected.false_value, addrspace))
            .transpose()?;
        let Some(storage) = true_storage.or(false_storage) else {
            return Err("native emitter: selected store has no concrete pointer arm".to_string());
        };
        if !matches!(
            storage,
            StorageClass::StorageBuffer | StorageClass::UniformConstant
        ) || true_storage.is_some_and(|s| s != storage)
            || false_storage.is_some_and(|s| s != storage)
        {
            return Err(
                "native emitter: cannot store through reinterpreted pointer select (non-uniform arms)"
                    .to_string(),
            );
        }
        let pointee = if true_is_null {
            self.pointer_pointee_for_value(&selected.false_value)?
        } else {
            self.pointer_pointee_for_value(&selected.true_value)?
        }
        .ok_or_else(|| {
            "native emitter: cannot store through reinterpreted pointer select (missing pointee)"
                .to_string()
        })?;
        let object_ty = self.resolve_type(object_ty)?;
        if !types_compatible(&self.resolve_type(&pointee)?, &object_ty) {
            return Err(format!(
                "native emitter: selected store type mismatch {pointee:?} vs {object_ty:?}"
            ));
        }
        for (value, selected_when_true) in
            [(&selected.true_value, true), (&selected.false_value, false)]
        {
            if matches!(value, LlValue::Zero) {
                continue;
            }
            let ptr = self.value_id(value, &selected.ty)?;
            let arm = self.store_arm_condition(selected.cond, selected_when_true, instructions)?;
            self.emit_guarded(arm, instructions, |_, instructions| {
                instructions.push(Self::inst(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(ptr), Operand::IdRef(object_id)],
                ));
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_selected_access_tree_load(
        &mut self,
        result: Word,
        result_ty: &LlType,
        tree: &SelectedAccessTree,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(result_ty)?;
        let value = self.emit_selected_access_tree_load_value(
            tree,
            &result_ty,
            access_align,
            instructions,
        )?;
        if value != result {
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(self.type_id(&result_ty)?),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
        }
        Ok(())
    }

    fn emit_selected_access_tree_load_value(
        &mut self,
        tree: &SelectedAccessTree,
        result_ty: &LlType,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if !types_compatible(&self.resolve_type(&tree.pointee)?, result_ty) {
            return Err(format!(
                "native emitter: selected access tree load type mismatch {:?} vs {result_ty:?}",
                tree.pointee
            ));
        }
        let value = self.fresh();
        let result_type = self.type_id(result_ty)?;
        self.emit_selected_load_choice(value, result_type, tree.cond, instructions,
            |this, when_true, instructions| this.emit_selected_access_arm_load_value(
                if when_true { &tree.true_arm } else { &tree.false_arm },
                result_ty, access_align, instructions))?;
        Ok(value)
    }

    fn emit_selected_access_arm_load_value(
        &mut self,
        arm: &SelectedAccessArm,
        result_ty: &LlType,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        match arm {
            SelectedAccessArm::Typed { ptr, .. } => {
                let value = self.fresh();
                instructions.push(Self::inst(
                    Op::Load,
                    Some(self.type_id(result_ty)?),
                    Some(value),
                    vec![Operand::IdRef(*ptr)],
                ));
                Ok(value)
            }
            SelectedAccessArm::DeferredTyped { base, storage, geps } => {
                let ptr = self.materialize_selected_typed_arm(base, *storage, geps, instructions)?;
                let value = self.fresh();
                instructions.push(Self::inst(Op::Load, Some(self.type_id(result_ty)?), Some(value),
                    vec![Operand::IdRef(ptr)]));
                Ok(value)
            }
            SelectedAccessArm::Raw(raw) => {
                let value = self.fresh();
                self.emit_raw_load(value, result_ty, raw, access_align, instructions)?;
                Ok(value)
            }
            SelectedAccessArm::Nested(nested) => self.emit_selected_access_tree_load_value(
                nested,
                result_ty,
                access_align,
                instructions,
            ),
            SelectedAccessArm::Null => self.const_null(result_ty),
        }
    }

    pub(in crate::native::emitter) fn emit_selected_access_tree_store(
        &mut self,
        object_ty: &LlType,
        object: Word,
        tree: &SelectedAccessTree,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let object_ty = self.resolve_type(object_ty)?;
        if !types_compatible(&self.resolve_type(&tree.pointee)?, &object_ty) {
            return Err(format!(
                "native emitter: selected access tree store type mismatch {:?} vs {object_ty:?}",
                tree.pointee
            ));
        }
        self.emit_selected_access_tree_store_with_parent(
            &object_ty,
            object,
            tree,
            None,
            access_align,
            instructions,
        )
    }

    fn emit_selected_access_tree_store_with_parent(
        &mut self,
        object_ty: &LlType,
        object: Word,
        tree: &SelectedAccessTree,
        parent_condition: Option<Word>,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let true_condition =
            self.selected_access_branch_condition(parent_condition, tree.cond, true, instructions)?;
        let false_condition = self.selected_access_branch_condition(
            parent_condition,
            tree.cond,
            false,
            instructions,
        )?;
        self.emit_selected_access_arm_store(
            object_ty,
            object,
            &tree.true_arm,
            true_condition,
            access_align,
            instructions,
        )?;
        self.emit_selected_access_arm_store(
            object_ty,
            object,
            &tree.false_arm,
            false_condition,
            access_align,
            instructions,
        )
    }

    fn selected_access_branch_condition(
        &mut self,
        parent: Option<Word>,
        condition: Word,
        true_branch: bool,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let bool_ty = self.type_id(&LlType::Bool)?;
        let branch = if true_branch {
            condition
        } else {
            let negated = self.fresh();
            instructions.push(Self::inst(
                Op::LogicalNot,
                Some(bool_ty),
                Some(negated),
                vec![Operand::IdRef(condition)],
            ));
            negated
        };
        let Some(parent) = parent else {
            return Ok(branch);
        };
        let combined = self.fresh();
        instructions.push(Self::inst(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(combined),
            vec![Operand::IdRef(parent), Operand::IdRef(branch)],
        ));
        Ok(combined)
    }

    fn emit_selected_access_arm_store(
        &mut self,
        object_ty: &LlType,
        object: Word,
        arm: &SelectedAccessArm,
        condition: Word,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        match arm {
            SelectedAccessArm::Nested(nested) => self.emit_selected_access_tree_store_with_parent(
                object_ty,
                object,
                nested,
                Some(condition),
                access_align,
                instructions,
            ),
            SelectedAccessArm::Typed { ptr, storage } => {
                if !matches!(
                    storage,
                    StorageClass::StorageBuffer | StorageClass::UniformConstant
                ) {
                    return Err(format!(
                        "native emitter: selected access store has unsupported storage {storage:?}"
                    ));
                }
                let ptr = *ptr;
                self.emit_guarded(condition, instructions, |_, instructions| {
                    instructions.push(Self::inst(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(ptr), Operand::IdRef(object)],
                    ));
                    Ok(())
                })
            }
            SelectedAccessArm::DeferredTyped { base, storage, geps } => {
                if !matches!(storage, StorageClass::StorageBuffer | StorageClass::UniformConstant) {
                    return Err(format!("native emitter: selected access store has unsupported storage {storage:?}"));
                }
                self.emit_guarded(condition, instructions, |this, instructions| {
                    let ptr = this.materialize_selected_typed_arm(base, *storage, geps, instructions)?;
                    instructions.push(Self::inst(Op::Store, None, None,
                        vec![Operand::IdRef(ptr), Operand::IdRef(object)]));
                    Ok(())
                })
            }
            SelectedAccessArm::Raw(raw) => {
                let raw = raw.clone();
                let object_ty = object_ty.clone();
                self.emit_guarded(condition, instructions, move |emitter, instructions| {
                    emitter.emit_raw_store(&object_ty, object, &raw, access_align, instructions)
                })
            }
            SelectedAccessArm::Null => Ok(()),
        }
    }

    pub(in crate::native::emitter) fn materialize_selected_pointer_value(
        &mut self,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let selected = self
            .selected_pointers
            .get(name)
            .cloned()
            .ok_or_else(|| format!("native emitter: unknown selected pointer {name}"))?;
        let LlType::Ptr(addrspace) = self.resolve_type(&selected.ty)? else {
            return Err(format!(
                "native emitter: selected pointer {name} has non-pointer type {:?}",
                selected.ty
            ));
        };
        let meta = self
            .pointer_meta_for_value(&LlValue::Local(name.to_string()), addrspace)?
            .ok_or_else(|| format!("native emitter: selected pointer {name} missing metadata"))?;
        let result_type = self.pointer_aware_type_id(&selected.ty, Some(&meta))?;
        let result = self.result_id(name, &selected.ty)?;
        let true_id =
            self.pointer_select_arm_id(&selected.true_value, &selected.ty, &meta, instructions)?;
        let false_id =
            self.pointer_select_arm_id(&selected.false_value, &selected.ty, &meta, instructions)?;
        instructions.push(Self::inst(
            Op::Select,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(selected.cond),
                Operand::IdRef(true_id),
                Operand::IdRef(false_id),
            ],
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_load(
        &mut self,
        result: Word,
        result_ty: &LlType,
        selected: &SelectedLoadPointer,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(result_ty)?;
        let pointee = self.resolve_type(&selected.pointee)?;
        let reinterpret_raw = pointee == LlType::Int(8) && pointee != result_ty;
        if reinterpret_raw {
            let pointee_bits = bitcast_width(&pointee).ok_or_else(|| {
                format!("native emitter: cannot reinterpret selected load from {pointee:?}")
            })?;
            let result_bits = bitcast_width(&result_ty).ok_or_else(|| {
                format!("native emitter: cannot reinterpret selected load to {result_ty:?}")
            })?;
            if pointee_bits > result_bits || result_bits % pointee_bits != 0 {
                return Err(format!(
                    "native emitter: selected pointer load bit width mismatch {pointee:?} ({pointee_bits}) vs {result_ty:?} ({result_bits})"
                ));
            }
        }
        if !types_compatible(&pointee, &result_ty)
            && (selected.true_ptr.is_some() || selected.false_ptr.is_some())
        {
            return Err(format!(
                "native emitter: selected pointer load type mismatch {pointee:?} vs {result_ty:?}"
            ));
        }
        let result_type = self.type_id(&result_ty)?;
        self.emit_selected_load_choice(result, result_type, selected.cond, instructions,
            |this, when_true, instructions| {
                let (ptr, raw) = if when_true {
                    (selected.true_ptr, &selected.true_raw)
                } else { (selected.false_ptr, &selected.false_raw) };
                let value = this.fresh();
                if let Some(ptr) = ptr {
                    instructions.push(Self::inst(Op::Load, Some(result_type), Some(value),
                        vec![Operand::IdRef(ptr)]));
                } else if let Some(raw) = raw {
                    this.emit_raw_load(value, &result_ty, raw, access_align, instructions)?;
                } else { return Err("native emitter: selected pointer load missing arm".to_string()); }
                Ok(value)
            })
    }

    pub(in crate::native::emitter) fn emit_selected_pointer_store(
        &mut self,
        object_ty: &LlType,
        object: Word,
        selected: &SelectedLoadPointer,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let object_ty = self.resolve_type(object_ty)?;
        let pointee = self.resolve_type(&selected.pointee)?;
        if let (Some(true_raw), Some(false_raw)) = (&selected.true_raw, &selected.false_raw) {
            return self.emit_selected_raw_store(
                &object_ty,
                object,
                true_raw,
                false_raw,
                selected.cond,
                access_align,
                instructions,
            );
        }
        if !types_compatible(&pointee, &object_ty) {
            return Err(format!(
                "native emitter: selected pointer store type mismatch {pointee:?} vs {object_ty:?}"
            ));
        }
        let true_ptr = selected
            .true_ptr
            .ok_or_else(|| "native emitter: selected pointer store missing true arm".to_string())?;
        let false_ptr = selected.false_ptr.ok_or_else(|| {
            "native emitter: selected pointer store missing false arm".to_string()
        })?;
        for (ptr, selected_when_true) in [(true_ptr, true), (false_ptr, false)] {
            let arm = self.store_arm_condition(selected.cond, selected_when_true, instructions)?;
            self.emit_guarded(arm, instructions, |_, instructions| {
                instructions.push(Self::inst(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(ptr), Operand::IdRef(object)],
                ));
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_raw_pointer_select_index(
        &mut self,
        name: &str,
        result_ty: &LlType,
        true_value: &LlValue,
        false_value: &LlValue,
        cond_id: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = result_ty else {
            return Ok(false);
        };
        let (LlValue::Local(true_name), LlValue::Local(false_name)) = (true_value, false_value)
        else {
            return Ok(false);
        };
        let (Some(true_raw), Some(false_raw)) = (
            self.raw_offsets.get(true_name).cloned(),
            self.raw_offsets.get(false_name).cloned(),
        ) else {
            return Ok(false);
        };
        if true_raw.root != false_raw.root || true_raw.addrspace != false_raw.addrspace {
            return Ok(false);
        }
        if !self.raw_pointer_word_aligned(&true_raw) || !self.raw_pointer_word_aligned(&false_raw) {
            return self.emit_raw_pointer_select_byte(
                name,
                *addrspace,
                &true_raw,
                &false_raw,
                cond_id,
                instructions,
            );
        }

        let uint_ty = self.type_id(&LlType::Int(32))?;
        let true_word = self.emit_raw_word_index(&true_raw, 0, instructions)?;
        let false_word = self.emit_raw_word_index(&false_raw, 0, instructions)?;
        let word_name = raw_word_index_name(name);
        let result = self.result_id(&word_name, &LlType::Int(32))?;
        instructions.push(Self::inst(
            Op::Select,
            Some(uint_ty),
            Some(result),
            vec![
                Operand::IdRef(cond_id),
                Operand::IdRef(true_word),
                Operand::IdRef(false_word),
            ],
        ));
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
                        ty: LlType::Int(32),
                        value: LlValue::Local(word_name),
                    },
                    4,
                )],
                root: true_raw.root,
                addrspace: true_raw.addrspace,
                unmodelable: false,
                device_addr_base: true_raw.device_addr_base,
            },
        );
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_raw_pointer_select_byte(
        &mut self,
        name: &str,
        addrspace: u32,
        true_raw: &RawBufferOffset,
        false_raw: &RawBufferOffset,
        cond_id: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let true_byte = self.emit_raw_byte_index(true_raw, 0, instructions)?;
        let false_byte = self.emit_raw_byte_index(false_raw, 0, instructions)?;
        let byte_name = raw_byte_index_name(name);
        let result = self.result_id(&byte_name, &LlType::Int(32))?;
        instructions.push(Self::inst(
            Op::Select,
            Some(uint_ty),
            Some(result),
            vec![
                Operand::IdRef(cond_id),
                Operand::IdRef(true_byte),
                Operand::IdRef(false_byte),
            ],
        ));
        self.pointer_storage
            .insert(name.to_string(), llvm_pointer_storage(addrspace)?);
        self.pointer_pointees
            .insert(name.to_string(), raw_buffer_block_type());
        self.raw_offsets.insert(
            name.to_string(),
            RawBufferOffset {
                const_off: 0,
                dyn_terms: vec![(
                    TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Local(byte_name),
                    },
                    1,
                )],
                root: true_raw.root.clone(),
                addrspace: true_raw.addrspace,
                unmodelable: false,
                device_addr_base: true_raw.device_addr_base,
            },
        );
        Ok(true)
    }
}
