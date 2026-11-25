use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn materialize_reserved_raw_word_index(
        &mut self,
        name: &str,
        raw: &RawBufferOffset,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        self.materialize_raw_word_index(name, raw, false, instructions)
    }

    pub(in crate::native::emitter) fn materialize_raw_word_index(
        &mut self,
        name: &str,
        raw: &RawBufferOffset,
        force: bool,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let word_name = raw_word_index_name(name);
        if !force && !self.values.contains_key(&word_name) {
            return Ok(());
        }
        if !self.raw_pointer_word_aligned(raw) {
            return Err(format!(
                "native emitter: raw pointer phi offset for {name} is not word-aligned: {raw:?}"
            ));
        }
        let value = self.emit_raw_word_index(raw, 0, instructions)?;
        let result_ty = self.type_id(&LlType::Int(32))?;
        let result = self.result_id(&word_name, &LlType::Int(32))?;
        if value != result {
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_ty),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn materialize_reserved_raw_byte_index(
        &mut self,
        name: &str,
        raw: &RawBufferOffset,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        self.materialize_raw_byte_index(name, raw, false, instructions)
    }

    pub(in crate::native::emitter) fn materialize_raw_byte_index(
        &mut self,
        name: &str,
        raw: &RawBufferOffset,
        force: bool,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let byte_name = raw_byte_index_name(name);
        if !force && !self.values.contains_key(&byte_name) {
            return Ok(());
        }
        let value = self.emit_raw_byte_index(raw, 0, instructions)?;
        let result_ty = self.type_id(&LlType::Int(32))?;
        let result = self.result_id(&byte_name, &LlType::Int(32))?;
        if value != result {
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_ty),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn raw_index_u32(
        &mut self,
        index: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let ty = self.resolve_type(&index.ty)?;
        let id = self.value_id(&index.value, &index.ty)?;
        match ty {
            LlType::Int(32) => Ok(id),
            LlType::Int(_) => {
                let uint_ty = self.type_id(&LlType::Int(32))?;
                let out = self.fresh();
                instructions.push(Self::inst(
                    Op::UConvert,
                    Some(uint_ty),
                    Some(out),
                    vec![Operand::IdRef(id)],
                ));
                Ok(out)
            }
            other => Err(format!(
                "native emitter: raw dynamic index must be integer, got {other:?}"
            )),
        }
    }

    pub(in crate::native::emitter) fn emit_private_zero_pointer_value_at(
        &mut self,
        result: Word,
        pointee: &LlType,
        site: &str,
    ) -> Result<(), String> {
        if crate::env_vars::unmodeled_why() {
            eprintln!(
                "[unmodeled-why] result=%{result} site={site} pointee={:?}",
                self.resolve_type(pointee)
                    .unwrap_or_else(|_| pointee.clone())
            );
        }
        let pointee = function_storage_local_type(&self.resolve_type(pointee)?);
        let ptr_ty = self.ptr_type_id(StorageClass::Private, &pointee)?;
        let init = self.const_null(&pointee)?;
        self.module.types_global_values.push(Self::inst(
            Op::Variable,
            Some(ptr_ty),
            Some(result),
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(init),
            ],
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn define_unmodeled_pointer_value(
        &mut self,
        name: &str,
        addrspace: u32,
        pointee: &LlType,
    ) -> Result<(), String> {
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        if crate::env_vars::spi_why() {
            self.module.debug_names.push(Self::inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(result),
                    Operand::LiteralString(format!("unmodeled pointer {name}")),
                ],
            ));
        }
        self.emit_private_zero_pointer_value_at(
            result,
            pointee,
            &format!("define_unmodeled_pointer name={name}"),
        )?;
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Private);
        self.pointer_pointees
            .insert(name.to_string(), self.resolve_type(pointee)?);
        self.unmodeled_pointers.insert(name.to_string());
        if self.tracks_pointer_nullness() && !self.pointer_nullness.contains_key(name) {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn symbolic_buffer_address_for_pointer(
        &self,
        name: &str,
    ) -> Option<RawBufferOffset> {
        if let Some(raw) = self.raw_offsets.get(name) {
            return (raw.device_addr_base.is_none() && !raw.unmodelable).then(|| raw.clone());
        }
        if self.unmodeled_pointers.contains(name) {
            return None;
        }
        let storage = self.pointer_storage.get(name).copied()?;
        if matches!(
            storage,
            StorageClass::Private | StorageClass::PhysicalStorageBuffer
        ) {
            return None;
        }
        let ty = self
            .values
            .get(name)
            .or_else(|| self.global_values.get(name))
            .map(|(_, ty)| ty.clone())?;
        let LlType::Ptr(addrspace) = self.resolve_type(&ty).ok()? else {
            return None;
        };
        Some(RawBufferOffset::root(name.to_string(), addrspace))
    }

    pub(in crate::native::emitter) fn is_raw_buffer_param(&self, name: &str) -> bool {
        let mut name = name.to_string();
        loop {
            if self.raw_buffer_params.contains(&name) {
                return true;
            }
            match crate::native::cfg::clone_crossarm::clone_source_name(&name) {
                Some(source) => name = source,
                None => return false,
            }
        }
    }

    pub(in crate::native::emitter) fn root_is_word_addressable(&self, root: &str) -> bool {
        self.pointer_pointees
            .get(root)
            .is_some_and(|pointee| !matches!(self.resolve_type(pointee), Ok(LlType::Int(8))))
    }

    pub(in crate::native::emitter) fn extend_symbolic_buffer_address(
        &mut self,
        name: &str,
        lhs: &TypedValue,
        rhs: &TypedValue,
    ) {
        let symbolic = |value: &LlValue| match value {
            LlValue::Local(local) => self.symbolic_buffer_addresses.get(local).cloned(),
            _ => None,
        };
        let (mut base, offset) = match (symbolic(&lhs.value), symbolic(&rhs.value)) {
            (Some(base), None) => (base, rhs.clone()),
            (None, Some(base)) => (base, lhs.clone()),
            _ => return,
        };
        match offset.value {
            LlValue::Int(value) | LlValue::Hex(value) => {
                let Ok(value) = i64::try_from(value) else {
                    return;
                };
                base.const_off += value;
            }
            LlValue::SignedInt(value) => base.const_off += value,
            LlValue::Zero => {}
            _ => base.dyn_terms.push((offset, 1)),
        }
        self.symbolic_buffer_addresses
            .insert(name.to_string(), base);
    }

    pub(in crate::native::emitter) fn define_unmodeled_byte_pointer_value(
        &mut self,
        name: &str,
        addrspace: u32,
    ) -> Result<(), String> {
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        if crate::env_vars::spi_why() {
            self.module.debug_names.push(Self::inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(result),
                    Operand::LiteralString(format!("unmodeled byte pointer {name}")),
                ],
            ));
        }
        self.emit_private_zero_pointer_value_at(
            result,
            &LlType::Int(8),
            &format!("define_unmodeled_byte_pointer name={name}"),
        )?;
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Private);
        self.unmodeled_pointers.insert(name.to_string());
        if self.tracks_pointer_nullness() && !self.pointer_nullness.contains_key(name) {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_unmodeled_byte_pointer_copy(
        &mut self,
        name: &str,
        result: Word,
        addrspace: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let placeholder_name = format!("{name}.metal2vulkan.unmodeled_byte_pointer");
        self.define_unmodeled_byte_pointer_value(&placeholder_name, addrspace)?;
        let placeholder = self.value_id(
            &LlValue::Local(placeholder_name.clone()),
            &LlType::Ptr(addrspace),
        )?;
        let result_type = self.ptr_type_id(StorageClass::Private, &LlType::Int(8))?;
        instructions.push(Self::inst(
            Op::CopyObject,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(placeholder)],
        ));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Private);
        self.unmodeled_pointers.insert(name.to_string());
        if let Some(is_null) = self.pointer_nullness.get(&placeholder_name).copied() {
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn raw_type_size_align(
        &self,
        ty: &LlType,
    ) -> Result<(u64, u64), String> {
        crate::layout::raw_size_align(ty, &|t| self.resolve_type(t), self.air_data_layout.as_ref())
    }

    pub(in crate::native::emitter) fn emit_scalar_load_from_byte_pointer(
        &mut self,
        result: Word,
        result_ty: &LlType,
        storage: StorageClass,
        base_ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if matches!(storage, StorageClass::Private | StorageClass::Function) {
            return Ok(false);
        }
        let ty = self.resolve_type(result_ty)?;
        if let LlType::Vector(elem, lanes) = &ty {
            let elem = self.resolve_type(elem)?;
            let (elem_size, _) = self.raw_type_size_align(&elem)?;
            let ptr_ty = self.ptr_type_id(storage, &LlType::Int(8))?;
            let mut lane_ids = Vec::with_capacity(*lanes as usize);
            for lane in 0..*lanes {
                let lane_ptr = if lane == 0 {
                    base_ptr
                } else {
                    let p = self.fresh();
                    let idx = self.const_uint(lane * elem_size as u32)?;
                    instructions.push(Self::inst(
                        Op::PtrAccessChain,
                        Some(ptr_ty),
                        Some(p),
                        vec![Operand::IdRef(base_ptr), Operand::IdRef(idx)],
                    ));
                    p
                };
                let lane_id = self.fresh();
                if !self.emit_scalar_load_from_byte_pointer(
                    lane_id,
                    &elem,
                    storage,
                    lane_ptr,
                    instructions,
                )? {
                    return Ok(false);
                }
                lane_ids.push(lane_id);
            }
            let result_type = self.type_id(&ty)?;
            instructions.push(Self::inst(
                Op::CompositeConstruct,
                Some(result_type),
                Some(result),
                lane_ids.into_iter().map(Operand::IdRef).collect(),
            ));
            return Ok(true);
        }
        if !matches!(
            ty,
            LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_)
        ) {
            return Ok(false);
        }
        let Some(bits) = bitcast_width(&ty) else {
            return Ok(false);
        };
        if !(bits == 16 || bits == 32 || bits == 64) {
            return Ok(false);
        }
        let nbytes = bits / 8;
        let uchar_ty = self.type_id(&LlType::Int(8))?;
        let ptr_ty = self.ptr_type_id(storage, &LlType::Int(8))?;
        let acc_ty_ll = LlType::Int(bits);
        let acc_ty = self.type_id(&acc_ty_ll)?;
        let mut acc: Option<Word> = None;
        for k in 0..nbytes {
            let byte_ptr = if k == 0 {
                base_ptr
            } else {
                let p = self.fresh();
                let idx = self.const_uint(k)?;
                instructions.push(Self::inst(
                    Op::PtrAccessChain,
                    Some(ptr_ty),
                    Some(p),
                    vec![Operand::IdRef(base_ptr), Operand::IdRef(idx)],
                ));
                p
            };
            let byte = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(uchar_ty),
                Some(byte),
                vec![Operand::IdRef(byte_ptr)],
            ));
            let widened = self.fresh();
            instructions.push(Self::inst(
                Op::UConvert,
                Some(acc_ty),
                Some(widened),
                vec![Operand::IdRef(byte)],
            ));
            let shifted = if k == 0 {
                widened
            } else {
                let s = self.const_uint(k * 8)?;
                let sh = self.fresh();
                instructions.push(Self::inst(
                    Op::ShiftLeftLogical,
                    Some(acc_ty),
                    Some(sh),
                    vec![Operand::IdRef(widened), Operand::IdRef(s)],
                ));
                sh
            };
            acc = Some(match acc {
                None => shifted,
                Some(a) => {
                    let o = self.fresh();
                    instructions.push(Self::inst(
                        Op::BitwiseOr,
                        Some(acc_ty),
                        Some(o),
                        vec![Operand::IdRef(a), Operand::IdRef(shifted)],
                    ));
                    o
                }
            });
        }
        let word = acc.ok_or_else(|| {
            "native emitter: byte-pointer scalar load produced no accumulator \
             (nbytes must be >= 1 for a 16/32/64-bit scalar)"
                .to_string()
        })?;
        let result_type = self.type_id(&ty)?;
        let op = if ty == acc_ty_ll {
            Op::CopyObject
        } else {
            Op::Bitcast
        };
        instructions.push(Self::inst(
            op,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(word)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_private_scalar_load_from_byte_pointer(
        &mut self,
        result: Word,
        result_ty: &LlType,
        pointer: &LlValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let ty = self.resolve_type(result_ty)?;
        if !matches!(
            ty,
            LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_)
        ) {
            return Ok(false);
        }
        let LlValue::Local(pointer_name) = pointer else {
            return Ok(false);
        };
        let Some(provenance) = self.gep_provenance.get(pointer_name).cloned() else {
            return Ok(false);
        };
        if provenance.source_ty != LlType::Int(8)
            || !provenance.root_is_indexed_container
            || provenance.indices.len() != 1
        {
            return Ok(false);
        }
        let Some(byte_offset) = const_index_i64(&provenance.indices[0]) else {
            return Ok(false);
        };
        if byte_offset < 0 {
            return Ok(false);
        }
        let (size, _) = self.raw_type_size_align(&ty)?;
        if size == 0 || !(byte_offset as u64).is_multiple_of(size) {
            return Ok(false);
        }
        let root_has_matching_pointee = self.values.iter().any(|(name, (id, value_ty))| {
            *id == provenance.root
                && matches!(value_ty, LlType::Ptr(_))
                && self
                    .pointer_pointees
                    .get(name)
                    .is_some_and(|pointee| self.resolve_type(pointee).is_ok_and(|p| p == ty))
        });
        if !root_has_matching_pointee {
            return Ok(false);
        }
        let element_index = byte_offset as u64 / size;
        let ptr = if element_index == 0 {
            provenance.root
        } else {
            let ptr_ty = self.ptr_type_id(StorageClass::Private, &ty)?;
            let ptr = self.fresh();
            let index = self.const_uint(u32::try_from(element_index).map_err(|_| {
                format!(
                    "native emitter: private byte-view element index {element_index} overflows u32"
                )
            })?)?;
            instructions.push(Self::inst(
                Op::PtrAccessChain,
                Some(ptr_ty),
                Some(ptr),
                vec![Operand::IdRef(provenance.root), Operand::IdRef(index)],
            ));
            ptr
        };
        let result_type = self.type_id(&ty)?;
        instructions.push(Self::inst(
            Op::Load,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(ptr)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_direct_buffer_address_payload(
        &mut self,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(Word, Word), String> {
        let param_index = self.direct_param_indices.get(name).copied().ok_or_else(|| {
            format!("native emitter: direct pointer payload source {name} is not an entry parameter")
        })?;
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let zero = self.const_uint(0)?;
        let mut words = [0; 2];
        for (component, word) in words.iter_mut().enumerate() {
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(uint_ty),
                Some(id),
                vec![Operand::IdRef(zero)],
            ));
            self.emit_sidecar
                .buffer_address_words
                .push(crate::emit_sidecar::BufferAddressWord {
                    id,
                    param_index,
                    component: component as u32,
                });
            *word = id;
        }
        Ok((words[0], words[1]))
    }

    pub(in crate::native::emitter) fn raw_struct_member(
        &self,
        fields: &[LlType],
        index: u64,
    ) -> Result<(u64, LlType), String> {
        let mut off = 0;
        for (field_index, field) in fields.iter().enumerate() {
            let (size, align) = self.raw_type_size_align(field)?;
            off = round_up_u64(off, align);
            if field_index as u64 == index {
                return Ok((off, self.resolve_type(field)?));
            }
            off += size;
        }
        Err(format!(
            "native emitter: raw struct member index {index} out of range"
        ))
    }
}
