use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn scale_gep_index(
        &mut self,
        index: &TypedValue,
        scale: u32,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<TypedValue, String> {
        if scale == 1 {
            return Ok(index.clone());
        }
        if let Some(value) = const_index(Some(index)) {
            let mut out = index.clone();
            out.value = LlValue::Int((value * scale) as u64);
            return Ok(out);
        }
        let index_ty = self.resolve_type(&index.ty)?;
        let LlType::Int(bits) = index_ty else {
            return Err(format!(
                "native emitter: vector GEP index is not an integer: {index_ty:?}"
            ));
        };
        let result_type = self.type_id(&index_ty)?;
        let index_id = self.value_id(&index.value, &index.ty)?;
        let scale_id = self.const_int(bits, scale as u64)?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::IMul,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(index_id), Operand::IdRef(scale_id)],
        ));
        let scaled_name = format!("%air.vecidx.{}", name.trim_start_matches('%'));
        self.values
            .insert(scaled_name.clone(), (result, index_ty.clone()));
        self.record_int_alignment(
            &scaled_name,
            &index_ty,
            self.int_value_alignment(&index.value),
        );
        Ok(TypedValue {
            ty: index_ty,
            value: LlValue::Local(scaled_name),
        })
    }

    pub(in crate::native::emitter) fn vector_lane_count(&self, ty: &LlType) -> Result<u32, String> {
        match ty {
            LlType::Named(name) => {
                let aliased = self
                    .ir
                    .types
                    .get(name)
                    .ok_or_else(|| format!("native emitter: unknown named type {name}"))?;
                self.vector_lane_count(aliased)
            }
            LlType::Vector(_, lanes) => Ok(*lanes),
            other => Err(format!(
                "native emitter: expected vector type for shufflevector, got {other:?}"
            )),
        }
    }

    pub(in crate::native::emitter) fn emit_widening_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes)) =
            (pointee, result_ty)
        else {
            return Ok(false);
        };
        if src_lanes >= dst_lanes || !types_compatible(src_elem, dst_elem) {
            return Ok(false);
        }

        let pointee_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(pointee_type),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));

        let elem_type = self.type_id(src_elem)?;
        let mut lanes = Vec::with_capacity(*dst_lanes as usize);
        for lane in 0..*src_lanes {
            let extracted = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(elem_type),
                Some(extracted),
                vec![Operand::IdRef(loaded), Operand::LiteralBit32(lane)],
            ));
            lanes.push(Operand::IdRef(extracted));
        }
        for _ in *src_lanes..*dst_lanes {
            lanes.push(Operand::IdRef(self.undef_id(src_elem)?));
        }

        let result_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            lanes,
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_narrowing_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes)) =
            (pointee, result_ty)
        else {
            return Ok(false);
        };
        if src_lanes <= dst_lanes || !types_compatible(src_elem, dst_elem) {
            return Ok(false);
        }

        let pointee_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(pointee_type),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));

        let elem_type = self.type_id(dst_elem)?;
        let mut lanes = Vec::with_capacity(*dst_lanes as usize);
        for lane in 0..*dst_lanes {
            let extracted = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(elem_type),
                Some(extracted),
                vec![Operand::IdRef(loaded), Operand::LiteralBit32(lane)],
            ));
            lanes.push(Operand::IdRef(extracted));
        }

        let result_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            lanes,
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_to_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Vector(elem, lanes) = result_ty else {
            return Ok(false);
        };
        let elem = self.resolve_type(elem)?;
        if *lanes == 0 {
            return Ok(false);
        }
        let slot_ty = if types_compatible(pointee, &elem) {
            elem.clone()
        } else {
            let (Some(elem_bits), Some(pointee_bits)) =
                (bitcast_width(&elem), bitcast_width(pointee))
            else {
                return Ok(false);
            };
            if elem_bits != pointee_bits {
                return Ok(false);
            }
            pointee.clone()
        };
        if *lanes > 1 && !self.can_emit_gep_provenance_lane_ptrs(ptr_value, &slot_ty)? {
            if self.emit_scalar_pointer_vector_load(
                result,
                &elem,
                *lanes,
                ptr_value,
                ptr,
                instructions,
            )? {
                return Ok(true);
            }
            return self.emit_scalar_to_vector_load_by_lane_geps(
                result,
                &elem,
                &slot_ty,
                *lanes,
                ptr_value,
                ptr,
                instructions,
            );
        }

        let mut lane_ptrs = Vec::with_capacity(*lanes as usize);
        for lane in 0..*lanes {
            let lane_ptr = if lane == 0 {
                ptr
            } else {
                self.emit_gep_provenance_lane_ptr(ptr_value, &slot_ty, lane, instructions)?
                    .ok_or_else(|| {
                        "native emitter: scalar-to-vector load lost pointer provenance".to_string()
                    })?
            };
            lane_ptrs.push(lane_ptr);
        }

        let elem_type = self.type_id(&elem)?;
        let slot_type = self.type_id(&slot_ty)?;
        let lanes_need_bitcast = slot_type != elem_type;
        let mut lane_ids = Vec::with_capacity(*lanes as usize);
        for lane_ptr in lane_ptrs {
            let lane_id = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(slot_type),
                Some(lane_id),
                vec![Operand::IdRef(lane_ptr)],
            ));
            let lane_id = if lanes_need_bitcast {
                let reinterpreted = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(elem_type),
                    Some(reinterpreted),
                    vec![Operand::IdRef(lane_id)],
                ));
                reinterpreted
            } else {
                lane_id
            };
            lane_ids.push(Operand::IdRef(lane_id));
        }

        let result_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            lane_ids,
        ));
        Ok(true)
    }

    /// Lane pointers as explicit GEPs for a vector load whose base has no provenance (an index-0 GEP on a
    /// parameter); lanes are reinterpreted to the element type when the slot type differs.
    #[allow(clippy::too_many_arguments)]
    fn emit_scalar_to_vector_load_by_lane_geps(
        &mut self,
        result: Word,
        elem: &LlType,
        slot_ty: &LlType,
        lanes: u32,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlValue::Local(base_name) = &ptr_value.value else {
            return Ok(false);
        };
        let base_ty = self.resolve_type(&ptr_value.ty)?;
        if !matches!(base_ty, LlType::Ptr(_)) {
            return Ok(false);
        }
        let mut lane_ptrs = vec![ptr];
        for lane in 1..lanes {
            let gep = LlGep {
                inbounds: true,
                source_ty: slot_ty.clone(),
                base: TypedValue {
                    ty: base_ty.clone(),
                    value: ptr_value.value.clone(),
                },
                indices: vec![TypedValue {
                    ty: LlType::Int(32),
                    value: LlValue::Int(lane as u64),
                }],
            };
            let name = format!("{base_name}.m2v_lane{lane}");
            match self.emit_gep_result(&name, &gep, instructions)? {
                Some(id) => lane_ptrs.push(id),
                None => return Ok(false),
            }
        }
        self.emit_vector_from_lane_ptrs(result, elem, slot_ty, &lane_ptrs, instructions)?;
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_word_to_subword_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Vector(elem, lanes) = result_ty else {
            return Ok(false);
        };
        let elem = self.resolve_type(elem)?;
        let Some(pointee_bits) = bitcast_width(pointee) else {
            return Ok(false);
        };
        let Some(elem_bits) = bitcast_width(&elem) else {
            return Ok(false);
        };
        if elem_bits == 0 || pointee_bits <= elem_bits || pointee_bits % elem_bits != 0 {
            return Ok(false);
        }
        let lanes_per_word = pointee_bits / elem_bits;
        if *lanes < lanes_per_word || *lanes % lanes_per_word != 0 {
            return Ok(false);
        }
        let word_count = *lanes / lanes_per_word;
        if word_count > 1 && !self.can_emit_gep_provenance_lane_ptrs(ptr_value, pointee)? {
            return Ok(false);
        }

        let chunk_ty = LlType::Vector(Box::new(elem.clone()), lanes_per_word);
        if bitcast_width(&chunk_ty) != Some(pointee_bits) {
            return Ok(false);
        }
        let pointee_type = self.type_id(pointee)?;
        let chunk_type = self.type_id(&chunk_ty)?;
        let elem_type = self.type_id(&elem)?;
        let mut lane_ids = Vec::with_capacity(*lanes as usize);
        for word in 0..word_count {
            let word_ptr = if word == 0 {
                ptr
            } else {
                self.emit_gep_provenance_lane_ptr(ptr_value, pointee, word, instructions)?
                    .ok_or_else(|| {
                        "native emitter: subword-vector load lost pointer provenance".to_string()
                    })?
            };
            let loaded = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(pointee_type),
                Some(loaded),
                vec![Operand::IdRef(word_ptr)],
            ));
            let chunk = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(chunk_type),
                Some(chunk),
                vec![Operand::IdRef(loaded)],
            ));
            for lane in 0..lanes_per_word {
                let lane_id = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeExtract,
                    Some(elem_type),
                    Some(lane_id),
                    vec![Operand::IdRef(chunk), Operand::LiteralBit32(lane)],
                ));
                lane_ids.push(Operand::IdRef(lane_id));
            }
        }

        let result_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            lane_ids,
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_slots_to_wider_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if matches!(pointee, LlType::Vector(..)) {
            return Ok(false);
        }
        let (result_elem, lanes) = match result_ty {
            LlType::Vector(elem, lanes) => (self.resolve_type(elem)?, *lanes),
            LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_) => {
                (result_ty.clone(), 1)
            }
            _ => return Ok(false),
        };
        let (Some(pointee_bits), Some(result_elem_bits)) =
            (bitcast_width(pointee), bitcast_width(&result_elem))
        else {
            return Ok(false);
        };
        if pointee_bits == 0
            || lanes == 0
            || result_elem_bits <= pointee_bits
            || result_elem_bits % pointee_bits != 0
        {
            return Ok(false);
        }
        let slots_per_elem = result_elem_bits / pointee_bits;
        let total_slots = slots_per_elem * lanes;
        if !self.gep_provenance_strides_contiguous(ptr_value, pointee)?
            || !self.can_emit_gep_provenance_lane_ptrs(ptr_value, pointee)?
        {
            return Ok(false);
        }

        let pointee_type = self.type_id(pointee)?;
        let pointee_uint = LlType::Int(pointee_bits);
        let pointee_uint_type = self.type_id(&pointee_uint)?;
        let elem_uint = LlType::Int(result_elem_bits);
        let elem_uint_type = self.type_id(&elem_uint)?;

        let mut slot_uints = Vec::with_capacity(total_slots as usize);
        for slot in 0..total_slots {
            let slot_ptr = if slot == 0 {
                ptr
            } else {
                self.emit_gep_provenance_lane_ptr(ptr_value, pointee, slot, instructions)?
                    .ok_or_else(|| {
                        "native emitter: scalar-to-wider-vector load lost pointer provenance"
                            .to_string()
                    })?
            };
            let loaded = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(pointee_type),
                Some(loaded),
                vec![Operand::IdRef(slot_ptr)],
            ));
            let as_uint = if *pointee == pointee_uint {
                loaded
            } else {
                let id = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(pointee_uint_type),
                    Some(id),
                    vec![Operand::IdRef(loaded)],
                ));
                id
            };
            slot_uints.push(as_uint);
        }

        let mut elem_uints = Vec::with_capacity(lanes as usize);
        for lane in 0..lanes {
            let mut acc: Option<Word> = None;
            for j in 0..slots_per_elem {
                let slot_uint = slot_uints[(lane * slots_per_elem + j) as usize];
                let widened = if pointee_bits == result_elem_bits {
                    slot_uint
                } else {
                    let id = self.fresh();
                    instructions.push(Self::inst(
                        Op::UConvert,
                        Some(elem_uint_type),
                        Some(id),
                        vec![Operand::IdRef(slot_uint)],
                    ));
                    id
                };
                let shifted = if j == 0 {
                    widened
                } else {
                    let shift = self.const_uint(j * pointee_bits)?;
                    let id = self.fresh();
                    instructions.push(Self::inst(
                        Op::ShiftLeftLogical,
                        Some(elem_uint_type),
                        Some(id),
                        vec![Operand::IdRef(widened), Operand::IdRef(shift)],
                    ));
                    id
                };
                acc = Some(match acc {
                    None => shifted,
                    Some(prev) => {
                        let id = self.fresh();
                        instructions.push(Self::inst(
                            Op::BitwiseOr,
                            Some(elem_uint_type),
                            Some(id),
                            vec![Operand::IdRef(prev), Operand::IdRef(shifted)],
                        ));
                        id
                    }
                });
            }
            elem_uints.push(Operand::IdRef(acc.ok_or_else(|| {
                "native emitter: vector integer load produced no lane accumulator \
                 (slots_per_elem must be >= 1)"
                    .to_string()
            })?));
        }

        let bitcast_needed = !types_compatible(&result_elem, &elem_uint);
        let packed = match result_ty {
            LlType::Vector(..) => {
                let uint_vec_type =
                    self.type_id(&LlType::Vector(Box::new(elem_uint.clone()), lanes))?;
                let uint_vec = if bitcast_needed { self.fresh() } else { result };
                instructions.push(Self::inst(
                    Op::CompositeConstruct,
                    Some(uint_vec_type),
                    Some(uint_vec),
                    elem_uints,
                ));
                uint_vec
            }
            _ => {
                let Operand::IdRef(acc) = elem_uints[0] else {
                    return Ok(false);
                };
                if bitcast_needed {
                    acc
                } else {
                    let result_type = self.type_id(result_ty)?;
                    instructions.push(Self::inst(
                        Op::CopyObject,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(acc)],
                    ));
                    result
                }
            }
        };
        if bitcast_needed {
            let result_type = self.type_id(result_ty)?;
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(packed)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn gep_provenance_strides_contiguous(
        &self,
        ptr: &TypedValue,
        _elem: &LlType,
    ) -> Result<bool, String> {
        let LlValue::Local(name) = &ptr.value else {
            return Ok(false);
        };
        let Some(provenance) = self.gep_provenance.get(name) else {
            return Ok(false);
        };
        if provenance.indices.len() <= 1 {
            return Ok(true);
        }
        Ok(matches!(
            gep_parent_before_last(&provenance.source_ty, &provenance.indices),
            Some(LlType::Array(..)) | Some(LlType::Vector(..))
        ))
    }

    pub(in crate::native::emitter) fn emit_scalar_from_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Vector(elem, _lanes) = pointee else {
            return Ok(false);
        };
        let elem = self.resolve_type(elem)?;
        if matches!(result_ty, LlType::Vector(..)) {
            return Ok(false);
        }
        let (Some(elem_bits), Some(res_bits)) = (bitcast_width(&elem), bitcast_width(result_ty))
        else {
            return Ok(false);
        };
        if elem_bits == 0 || res_bits == 0 || res_bits > elem_bits {
            return Ok(false);
        }

        let pointee_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(pointee_type),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));
        let elem_type = self.type_id(&elem)?;
        if elem == *result_ty {
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(self.type_id(result_ty)?),
                Some(result),
                vec![Operand::IdRef(loaded), Operand::LiteralBit32(0)],
            ));
            return Ok(true);
        }
        let comp0 = self.fresh();
        instructions.push(Self::inst(
            Op::CompositeExtract,
            Some(elem_type),
            Some(comp0),
            vec![Operand::IdRef(loaded), Operand::LiteralBit32(0)],
        ));

        if res_bits == elem_bits {
            let result_type = self.type_id(result_ty)?;
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(comp0)],
            ));
            return Ok(true);
        }

        let elem_uint = LlType::Int(elem_bits);
        let as_uint = if elem == elem_uint {
            comp0
        } else {
            let elem_uint_type = self.type_id(&elem_uint)?;
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(elem_uint_type),
                Some(id),
                vec![Operand::IdRef(comp0)],
            ));
            id
        };
        let res_uint = LlType::Int(res_bits);
        let res_is_uint = *result_ty == res_uint;
        let res_uint_type = self.type_id(&res_uint)?;
        let truncated = if res_is_uint { result } else { self.fresh() };
        instructions.push(Self::inst(
            Op::UConvert,
            Some(res_uint_type),
            Some(truncated),
            vec![Operand::IdRef(as_uint)],
        ));
        if !res_is_uint {
            let result_type = self.type_id(result_ty)?;
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(truncated)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_narrowing_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if matches!(pointee, LlType::Vector(..)) || matches!(result_ty, LlType::Vector(..)) {
            return Ok(false);
        }
        let (Some(pointee_bits), Some(res_bits)) =
            (bitcast_width(pointee), bitcast_width(result_ty))
        else {
            return Ok(false);
        };
        if pointee_bits == 0 || res_bits == 0 || res_bits >= pointee_bits {
            return Ok(false);
        }

        let pointee_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(pointee_type),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));
        let pointee_uint = LlType::Int(pointee_bits);
        let as_uint = if *pointee == pointee_uint {
            loaded
        } else {
            let pointee_uint_type = self.type_id(&pointee_uint)?;
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(pointee_uint_type),
                Some(id),
                vec![Operand::IdRef(loaded)],
            ));
            id
        };
        let res_uint = LlType::Int(res_bits);
        let res_is_uint = *result_ty == res_uint;
        let res_uint_type = self.type_id(&res_uint)?;
        let truncated = if res_is_uint { result } else { self.fresh() };
        instructions.push(Self::inst(
            Op::UConvert,
            Some(res_uint_type),
            Some(truncated),
            vec![Operand::IdRef(as_uint)],
        ));
        if !res_is_uint {
            let result_type = self.type_id(result_ty)?;
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(truncated)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_pointer_vector_load(
        &mut self,
        result: Word,
        elem: &LlType,
        lanes: u32,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&ptr_value.ty)? else {
            return Ok(false);
        };
        let storage = self.pointer_storage_for(&ptr_value.value, addrspace)?;
        if storage != StorageClass::Workgroup {
            return Ok(false);
        }

        let ptr_type = self.ptr_type_id(storage, elem)?;
        let mut lane_ptrs = vec![ptr];
        for lane in 1..lanes {
            let lane_index = self.const_uint(lane)?;
            let lane_ptr = self.fresh();
            instructions.push(Self::inst(
                Op::PtrAccessChain,
                Some(ptr_type),
                Some(lane_ptr),
                vec![Operand::IdRef(ptr), Operand::IdRef(lane_index)],
            ));
            lane_ptrs.push(lane_ptr);
        }
        self.emit_vector_from_lane_ptrs(result, elem, elem, &lane_ptrs, instructions)?;
        Ok(true)
    }

    /// Loads one `slot_ty` value per lane pointer, reinterpreted to `elem` when the types differ, and
    /// assembles the vector.
    fn emit_vector_from_lane_ptrs(
        &mut self,
        result: Word,
        elem: &LlType,
        slot_ty: &LlType,
        lane_ptrs: &[Word],
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let elem_type = self.type_id(elem)?;
        let slot_type = self.type_id(slot_ty)?;
        let mut lane_ids = Vec::with_capacity(lane_ptrs.len());
        for &lane_ptr in lane_ptrs {
            let mut lane_id = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(slot_type),
                Some(lane_id),
                vec![Operand::IdRef(lane_ptr)],
            ));
            if slot_type != elem_type {
                let reinterpreted = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(elem_type),
                    Some(reinterpreted),
                    vec![Operand::IdRef(lane_id)],
                ));
                lane_id = reinterpreted;
            }
            lane_ids.push(Operand::IdRef(lane_id));
        }
        let lanes = lane_ptrs.len() as u32;
        let result_type = self.type_id(&LlType::Vector(Box::new(elem.clone()), lanes))?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            lane_ids,
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn can_emit_gep_provenance_lane_ptrs(
        &self,
        ptr: &TypedValue,
        elem: &LlType,
    ) -> Result<bool, String> {
        let LlValue::Local(name) = &ptr.value else {
            return Ok(false);
        };
        let Some(provenance) = self.gep_provenance.get(name) else {
            return Ok(false);
        };
        if provenance.indices.is_empty() {
            return Ok(false);
        }
        Ok(types_compatible(
            &gep_pointee(&provenance.source_ty, &provenance.indices)?,
            elem,
        ) && self.gep_provenance_strides_contiguous(ptr, elem)?)
    }

    pub(in crate::native::emitter) fn emit_gep_provenance_lane_ptr(
        &mut self,
        ptr: &TypedValue,
        elem: &LlType,
        lane: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let LlValue::Local(name) = &ptr.value else {
            return Ok(None);
        };
        let Some(provenance) = self.gep_provenance.get(name).cloned() else {
            return Ok(None);
        };
        if !types_compatible(
            &gep_pointee(&provenance.source_ty, &provenance.indices)?,
            elem,
        ) {
            return Ok(None);
        }
        let Some(last_index) = provenance.indices.last() else {
            return Ok(None);
        };
        let mut indices = provenance.indices.clone();
        let lane_index = TypedValue {
            ty: last_index.ty.clone(),
            value: LlValue::Int(lane as u64),
        };
        if let Some(last) = indices.last_mut() {
            *last = self.combine_gep_indices(last, &lane_index, instructions)?;
        }
        let pointee = gep_pointee(&provenance.source_ty, &indices)?;
        let storage = self.pointer_storage_for(&ptr.value, provenance.addrspace)?;
        let ptr_ty = self.ptr_type_id(storage, &pointee)?;
        let result = self.fresh();
        let mut ops = vec![Operand::IdRef(provenance.root)];
        for idx in gep_spirv_indices(&indices)? {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        let root_is_param = self.param_values.iter().any(|param| {
            self.values
                .get(param)
                .is_some_and(|(id, _)| *id == provenance.root)
        });
        instructions.push(Self::inst(
            if root_is_param
                || provenance.root_is_indexed_container
                || self.is_indexed_container_root(provenance.root, None)
                || !ptr_access_chain_allowed_storage(storage)
            {
                Op::InBoundsAccessChain
            } else {
                Op::PtrAccessChain
            },
            Some(ptr_ty),
            Some(result),
            ops,
        ));
        Ok(Some(result))
    }

    pub(in crate::native::emitter) fn drop_indirect_function_group_call(
        &mut self,
        rest: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !rest.contains("!air.function_groups") {
            return Ok(false);
        }
        let Some(open) = rest.find('(') else {
            return Ok(false);
        };
        let head = rest[..open].trim();
        let head_parts = split_top_level_whitespace(head);
        let Some(callee_text) = head_parts.last().copied() else {
            return Ok(false);
        };
        if !callee_text.starts_with('%') {
            return Ok(false);
        }
        let ret_text = head
            .strip_suffix(callee_text)
            .map(str::trim)
            .unwrap_or(head);
        let ret_ty = self.resolve_type(&parse_type(ret_text)?)?;
        if ret_ty != LlType::Void {
            return Err(format!(
                "native emitter: indirect function-group call returned {ret_ty:?}"
            ));
        }
        let callee = parse_value(callee_text)?;
        let _ = self.value_id(&callee, &LlType::Ptr(0))?;
        let close = matching_paren(rest, open)
            .ok_or_else(|| format!("native emitter: unmatched indirect call parens: {rest}"))?;
        let args_text = &rest[open + 1..close];
        if !args_text.trim().is_empty() {
            for arg in split_top_level(args_text, ',') {
                let arg = parse_typed_value(arg)?;
                let _ = self.value_id_in(&arg.value, &arg.ty, instructions)?;
            }
        }
        Ok(true)
    }
}
