use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_raw_store(
        &mut self,
        ty: &LlType,
        value: Word,
        raw: &RawBufferOffset,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if raw.unmodelable {
            return Err("native emitter: raw buffer offset is not modelable".into());
        }
        if raw.device_addr_base.is_some() {
            let ty = self.resolve_type(ty)?;
            return self.emit_device_addr_store(&ty, value, raw, instructions);
        }
        let resolved_ty = self.resolve_type(ty)?;
        let (store_size, _) = self.raw_type_size_align(&resolved_ty)?;
        if let Some(addressable) = self.emit_raw_u32_addressable(raw, store_size, instructions)? {
            let store_label = self.fresh();
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
                    Operand::IdRef(addressable),
                    Operand::IdRef(store_label),
                    Operand::IdRef(merge_label),
                ],
            ));
            instructions.push(Self::inst(Op::Label, None, Some(store_label), vec![]));
            self.emit_raw_store_unchecked(&resolved_ty, value, raw, access_align, instructions)?;
            instructions.push(Self::inst(
                Op::Branch,
                None,
                None,
                vec![Operand::IdRef(merge_label)],
            ));
            instructions.push(Self::inst(Op::Label, None, Some(merge_label), vec![]));
            return Ok(());
        }
        self.emit_raw_store_unchecked(&resolved_ty, value, raw, access_align, instructions)
    }

    pub(in crate::native::emitter) fn emit_raw_store_unchecked(
        &mut self,
        ty: &LlType,
        value: Word,
        raw: &RawBufferOffset,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        match ty {
            LlType::Struct(fields) => {
                for i in 0..fields.len() {
                    let (offset, field) = self.raw_struct_member(fields, i as u64)?;
                    let id = self.fresh();
                    let field_ty = self.type_id(&field)?;
                    instructions.push(Self::inst(Op::CompositeExtract, Some(field_ty), Some(id),
                        vec![Operand::IdRef(value), Operand::LiteralBit32(i as u32)]));
                    let mut part=raw.clone();
                    part.const_off=part.const_off.checked_add(offset as i64).ok_or("raw aggregate offset overflow")?;
                    let align=access_align.map(|a| if offset==0 {a} else {a.min(1u64 << offset.trailing_zeros())});
                    self.emit_raw_store_unchecked(&field,id,&part,align,instructions)?;
                }
                Ok(())
            }
            LlType::Array(elem, count) => {
                let elem=self.resolve_type(elem)?;
                let (size,alignment)=self.raw_type_size_align(&elem)?;
                let stride=round_up_u64(size,alignment);
                let elem_ty=self.type_id(&elem)?;
                for i in 0..*count {
                    let id=self.fresh();
                    instructions.push(Self::inst(Op::CompositeExtract,Some(elem_ty),Some(id),
                        vec![Operand::IdRef(value),Operand::LiteralBit32(i)]));
                    let offset=u64::from(i)*stride;
                    let mut part=raw.clone();part.const_off=part.const_off.checked_add(offset as i64).ok_or("raw aggregate offset overflow")?;
                    let align=access_align.map(|a| if offset==0 {a} else {a.min(1u64 << offset.trailing_zeros())});
                    self.emit_raw_store_unchecked(&elem,id,&part,align,instructions)?;
                }
                Ok(())
            }
            LlType::Vector(elem, lanes) => {
                let elem = self.resolve_type(elem)?;
                let (elem_size, _) = self.raw_type_size_align(&elem)?;
                let elem_ty = self.type_id(&elem)?;
                for lane in 0..*lanes {
                    let lane_id = self.fresh();
                    instructions.push(Self::inst(
                        Op::CompositeExtract,
                        Some(elem_ty),
                        Some(lane_id),
                        vec![Operand::IdRef(value), Operand::LiteralBit32(lane)],
                    ));
                    self.emit_raw_scalar_store(
                        &elem,
                        lane_id,
                        raw,
                        lane as u64 * elem_size,
                        access_align,
                        instructions,
                    )?;
                }
                Ok(())
            }
            scalar => self.emit_raw_scalar_store(scalar, value, raw, 0, access_align, instructions),
        }
    }

    pub(in crate::native::emitter) fn emit_raw_u32_addressable(
        &mut self,
        raw: &RawBufferOffset,
        access_size: u64,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let mut terms = Vec::with_capacity(raw.dyn_terms.len());
        let mut has_wide_term = false;
        for (index, stride) in &raw.dyn_terms {
            let ty = self.resolve_type(&index.ty)?;
            let LlType::Int(bits) = ty else {
                return Err(format!(
                    "native emitter: raw dynamic index must be integer, got {ty:?}"
                ));
            };
            has_wide_term |= bits > 32;
            terms.push((index.clone(), *stride, bits));
        }
        if !has_wide_term {
            return Ok(None);
        }
        if access_size == 0 || access_size - 1 > u32::MAX as u64 {
            return Err(format!(
                "native emitter: raw store size {access_size} is not u32-addressable"
            ));
        }

        let i64_ty = self.type_id(&LlType::Int(64))?;
        let mut byte_offset = self.const_signed_int(64, raw.const_off)?;
        for (index, stride, bits) in terms {
            if stride < 0 {
                return Err(format!(
                    "native emitter: raw dynamic byte stride {stride} is not supported"
                ));
            }
            if bits > 64 {
                return Err(format!(
                    "native emitter: raw dynamic i{bits} index is wider than the address model"
                ));
            }
            let index_id = self.value_id(&index.value, &index.ty)?;
            let index64 = if bits == 64 {
                index_id
            } else {
                let widened = self.fresh();
                instructions.push(Self::inst(
                    Op::SConvert,
                    Some(i64_ty),
                    Some(widened),
                    vec![Operand::IdRef(index_id)],
                ));
                widened
            };
            let term = if stride == 1 {
                index64
            } else {
                let scaled = self.fresh();
                let factor = self.const_signed_int(64, stride)?;
                instructions.push(Self::inst(
                    Op::IMul,
                    Some(i64_ty),
                    Some(scaled),
                    vec![Operand::IdRef(index64), Operand::IdRef(factor)],
                ));
                scaled
            };
            let sum = self.fresh();
            instructions.push(Self::inst(
                Op::IAdd,
                Some(i64_ty),
                Some(sum),
                vec![Operand::IdRef(byte_offset), Operand::IdRef(term)],
            ));
            byte_offset = sum;
        }

        let max_start = self.const_int(64, u64::from(u32::MAX) - (access_size - 1))?;
        let bool_ty = self.type_id(&LlType::Bool)?;
        let addressable = self.fresh();
        instructions.push(Self::inst(
            Op::ULessThanEqual,
            Some(bool_ty),
            Some(addressable),
            vec![Operand::IdRef(byte_offset), Operand::IdRef(max_start)],
        ));
        Ok(Some(addressable))
    }

    pub(in crate::native::emitter) fn emit_selected_raw_store(
        &mut self,
        ty: &LlType,
        value: Word,
        true_raw: &RawBufferOffset,
        false_raw: &RawBufferOffset,
        cond: Word,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if true_raw.unmodelable || false_raw.unmodelable {
            return Err("native emitter: selected raw buffer offset is not modelable".into());
        }
        let true_label = self.fresh();
        let false_label = self.fresh();
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
                Operand::IdRef(cond),
                Operand::IdRef(true_label),
                Operand::IdRef(false_label),
            ],
        ));
        instructions.push(Self::inst(Op::Label, None, Some(true_label), vec![]));
        self.emit_raw_store(ty, value, true_raw, access_align, instructions)?;
        instructions.push(Self::inst(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(merge_label)],
        ));
        instructions.push(Self::inst(Op::Label, None, Some(false_label), vec![]));
        self.emit_raw_store(ty, value, false_raw, access_align, instructions)?;
        instructions.push(Self::inst(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(merge_label)],
        ));
        instructions.push(Self::inst(Op::Label, None, Some(merge_label), vec![]));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_raw_scalar_store(
        &mut self,
        ty: &LlType,
        value: Word,
        raw: &RawBufferOffset,
        extra_byte: u64,
        access_align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if self.emit_raw_byte_array_integer_store(ty, value, raw, extra_byte, instructions)? {
            return Ok(());
        }
        match ty {
            LlType::Float => {
                let word_ty = self.type_id(&LlType::Int(32))?;
                let word = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(word_ty),
                    Some(word),
                    vec![Operand::IdRef(value)],
                ));
                self.emit_raw_word_store_for_access(
                    raw,
                    extra_byte,
                    word,
                    access_align,
                    instructions,
                )
            }
            LlType::Int(32) => self.emit_raw_word_store_for_access(
                raw,
                extra_byte,
                value,
                access_align,
                instructions,
            ),
            LlType::Int(64) => {
                let i64_ty = self.type_id(&LlType::Int(64))?;
                let i32_ty = self.type_id(&LlType::Int(32))?;
                let low = self.fresh();
                instructions.push(Self::inst(
                    Op::UConvert,
                    Some(i32_ty),
                    Some(low),
                    vec![Operand::IdRef(value)],
                ));
                let shift = self.const_signed_int(64, 32)?;
                let shifted = self.fresh();
                instructions.push(Self::inst(
                    Op::ShiftRightLogical,
                    Some(i64_ty),
                    Some(shifted),
                    vec![Operand::IdRef(value), Operand::IdRef(shift)],
                ));
                let high = self.fresh();
                instructions.push(Self::inst(
                    Op::UConvert,
                    Some(i32_ty),
                    Some(high),
                    vec![Operand::IdRef(shifted)],
                ));
                self.emit_raw_word_store_for_access(
                    raw,
                    extra_byte,
                    low,
                    access_align,
                    instructions,
                )?;
                self.emit_raw_word_store_for_access(
                    raw,
                    extra_byte + 4,
                    high,
                    access_align,
                    instructions,
                )
            }
            LlType::Half => {
                self.emit_raw_subword_store(ty, value, raw, extra_byte, 2, instructions)
            }
            LlType::Int(16) | LlType::BFloat => {
                self.emit_raw_subword_store(ty, value, raw, extra_byte, 2, instructions)
            }
            LlType::Int(8) => {
                self.emit_raw_subword_store(ty, value, raw, extra_byte, 1, instructions)
            }
            other => Err(format!(
                "native emitter: raw store for {other:?} is not covered yet"
            )),
        }
    }

    pub(in crate::native::emitter) fn emit_raw_subword_store(
        &mut self,
        ty: &LlType,
        value: Word,
        raw: &RawBufferOffset,
        extra_byte: u64,
        element_size: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let bits = self.emit_raw_subword_store_bits(ty, value, element_size, instructions)?;
        self.emit_raw_byte_store_from_u32(bits, raw, extra_byte, instructions)?;
        if element_size == 1 {
            return Ok(());
        }

        let uint_ty = self.type_id(&LlType::Int(32))?;
        let shift = self.const_uint(8)?;
        let high = self.fresh();
        instructions.push(Self::inst(
            Op::ShiftRightLogical,
            Some(uint_ty),
            Some(high),
            vec![Operand::IdRef(bits), Operand::IdRef(shift)],
        ));
        self.emit_raw_byte_store_from_u32(high, raw, extra_byte + 1, instructions)
    }

    pub(in crate::native::emitter) fn emit_raw_subword_store_bits(
        &mut self,
        ty: &LlType,
        value: Word,
        element_size: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if element_size == 1 {
            let uint_ty = self.type_id(&LlType::Int(32))?;
            let bits32 = self.fresh();
            instructions.push(Self::inst(
                Op::UConvert,
                Some(uint_ty),
                Some(bits32),
                vec![Operand::IdRef(value)],
            ));
            return Ok(bits32);
        }

        let u16_ty = self.type_id(&LlType::Int(16))?;
        let bits16 = match ty {
            LlType::Half => {
                let bits = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(u16_ty),
                    Some(bits),
                    vec![Operand::IdRef(value)],
                ));
                bits
            }
            LlType::Int(16) | LlType::BFloat => value,
            other => {
                return Err(format!(
                    "native emitter: raw 16-bit store for {other:?} is not covered yet"
                ));
            }
        };
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let bits32 = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(uint_ty),
            Some(bits32),
            vec![Operand::IdRef(bits16)],
        ));
        Ok(bits32)
    }

    pub(in crate::native::emitter) fn emit_raw_byte_load_as_u32(
        &mut self,
        raw: &RawBufferOffset,
        extra_byte: u64,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let byte = self.fresh();
        self.emit_raw_scalar_load(byte, &LlType::Int(8), raw, extra_byte, None, instructions)?;
        let u32_ty = self.type_id(&LlType::Int(32))?;
        let widened = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(u32_ty),
            Some(widened),
            vec![Operand::IdRef(byte)],
        ));
        Ok(widened)
    }

    pub(in crate::native::emitter) fn emit_raw_byte_store_from_u32(
        &mut self,
        value: Word,
        raw: &RawBufferOffset,
        extra_byte: u64,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let byte_index = self.emit_raw_byte_index(raw, extra_byte, instructions)?;
        let word_index = self.fresh();
        let word_divisor = self.const_uint(4)?;
        instructions.push(Self::inst(
            Op::UDiv,
            Some(uint_ty),
            Some(word_index),
            vec![Operand::IdRef(byte_index), Operand::IdRef(word_divisor)],
        ));
        let (ptr, storage) =
            self.emit_raw_word_pointer_at_index_with_storage(raw, word_index, instructions)?;

        let byte_lane = self.fresh();
        let lane_mask = self.const_uint(3)?;
        instructions.push(Self::inst(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(byte_lane),
            vec![Operand::IdRef(byte_index), Operand::IdRef(lane_mask)],
        ));

        let bits_per_byte = self.const_uint(8)?;
        let shift = self.fresh();
        instructions.push(Self::inst(
            Op::IMul,
            Some(uint_ty),
            Some(shift),
            vec![Operand::IdRef(byte_lane), Operand::IdRef(bits_per_byte)],
        ));

        let byte_mask = self.const_uint(0xff)?;
        let byte = self.fresh();
        instructions.push(Self::inst(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(byte),
            vec![Operand::IdRef(value), Operand::IdRef(byte_mask)],
        ));

        let shifted_byte = self.fresh();
        instructions.push(Self::inst(
            Op::ShiftLeftLogical,
            Some(uint_ty),
            Some(shifted_byte),
            vec![Operand::IdRef(byte), Operand::IdRef(shift)],
        ));

        let shifted_mask = self.fresh();
        instructions.push(Self::inst(
            Op::ShiftLeftLogical,
            Some(uint_ty),
            Some(shifted_mask),
            vec![Operand::IdRef(byte_mask), Operand::IdRef(shift)],
        ));
        let inverted_mask = self.fresh();
        let all_ones = self.const_uint(u32::MAX)?;
        instructions.push(Self::inst(
            Op::BitwiseXor,
            Some(uint_ty),
            Some(inverted_mask),
            vec![Operand::IdRef(shifted_mask), Operand::IdRef(all_ones)],
        ));

        if storage == StorageClass::Private {
            let old_word = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(uint_ty),
                Some(old_word),
                vec![Operand::IdRef(ptr)],
            ));
            let cleared = self.fresh();
            instructions.push(Self::inst(
                Op::BitwiseAnd,
                Some(uint_ty),
                Some(cleared),
                vec![Operand::IdRef(old_word), Operand::IdRef(inverted_mask)],
            ));
            let new_word = self.fresh();
            instructions.push(Self::inst(
                Op::BitwiseOr,
                Some(uint_ty),
                Some(new_word),
                vec![Operand::IdRef(cleared), Operand::IdRef(shifted_byte)],
            ));
            instructions.push(Self::inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(ptr), Operand::IdRef(new_word)],
            ));
            return Ok(());
        }
        let scope_kind = if storage == StorageClass::Workgroup {
            Scope::Workgroup
        } else {
            Scope::Device
        };
        let scope = self.const_uint(scope_kind as u32)?;
        let semantics = self.const_uint(MemorySemantics::RELAXED.bits())?;
        let old = self.fresh();
        instructions.push(Self::inst(
            Op::AtomicAnd,
            Some(uint_ty),
            Some(old),
            vec![
                Operand::IdRef(ptr),
                Operand::IdScope(scope),
                Operand::IdMemorySemantics(semantics),
                Operand::IdRef(inverted_mask),
            ],
        ));
        let old = self.fresh();
        instructions.push(Self::inst(
            Op::AtomicOr,
            Some(uint_ty),
            Some(old),
            vec![
                Operand::IdRef(ptr),
                Operand::IdScope(scope),
                Operand::IdMemorySemantics(semantics),
                Operand::IdRef(shifted_byte),
            ],
        ));
        Ok(())
    }
}
