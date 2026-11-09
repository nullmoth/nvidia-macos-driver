use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_vector_as_scalar_array_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let object_ty = self.resolve_type(&object.ty)?;
        let (LlType::Vector(vector_elem, vector_len), LlType::Array(array_elem, array_len)) =
            (&object_ty, pointee)
        else {
            return Ok(false);
        };
        if vector_len != array_len {
            return Ok(false);
        }
        let elements_are_compatible = types_compatible(vector_elem, array_elem);
        if !elements_are_compatible {
            if !is_scalar_storage_type(vector_elem) || !is_scalar_storage_type(array_elem) {
                return Ok(false);
            }
            let (Some(vector_elem_bits), Some(array_elem_bits)) =
                (bitcast_width(vector_elem), bitcast_width(array_elem))
            else {
                return Ok(false);
            };
            if vector_elem_bits != array_elem_bits {
                return Ok(false);
            }
        }

        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let elem_type = self.type_id(vector_elem)?;
        let array_elem_type = self.type_id(array_elem)?;
        let elements_need_bitcast = !elements_are_compatible && elem_type != array_elem_type;
        let mut elements = Vec::with_capacity(*vector_len as usize);
        for lane in 0..*vector_len {
            let value = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(elem_type),
                Some(value),
                vec![Operand::IdRef(object_id), Operand::LiteralBit32(lane)],
            ));
            let value = if elements_need_bitcast {
                let reinterpreted = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(array_elem_type),
                    Some(reinterpreted),
                    vec![Operand::IdRef(value)],
                ));
                reinterpreted
            } else {
                value
            };
            elements.push(Operand::IdRef(value));
        }
        let array_type = self.type_id(pointee)?;
        let array = self.fresh();
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(array_type),
            Some(array),
            elements,
        ));
        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(array)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_array_as_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let (LlType::Array(array_elem, array_len), LlType::Vector(vector_elem, vector_len)) =
            (pointee, result_ty)
        else {
            return Ok(false);
        };
        if array_len != vector_len
            || !is_scalar_storage_type(array_elem)
            || !is_scalar_storage_type(vector_elem)
        {
            return Ok(false);
        }
        let elements_are_compatible = types_compatible(array_elem, vector_elem);
        if !elements_are_compatible {
            let (Some(array_elem_bits), Some(vector_elem_bits)) =
                (bitcast_width(array_elem), bitcast_width(vector_elem))
            else {
                return Ok(false);
            };
            if array_elem_bits != vector_elem_bits {
                return Ok(false);
            }
        }

        let array_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(array_type),
            Some(loaded),
            vec![Operand::IdRef(ptr)],
        ));
        let array_elem_type = self.type_id(array_elem)?;
        let vector_elem_type = self.type_id(vector_elem)?;
        let elements_need_bitcast = !elements_are_compatible && array_elem_type != vector_elem_type;
        let mut lanes = Vec::with_capacity(*vector_len as usize);
        for lane in 0..*vector_len {
            let value = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(array_elem_type),
                Some(value),
                vec![Operand::IdRef(loaded), Operand::LiteralBit32(lane)],
            ));
            let value = if elements_need_bitcast {
                let reinterpreted = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(vector_elem_type),
                    Some(reinterpreted),
                    vec![Operand::IdRef(value)],
                ));
                reinterpreted
            } else {
                value
            };
            lanes.push(Operand::IdRef(value));
        }
        let vector_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(vector_type),
            Some(result),
            lanes,
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_narrowing_vector_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let object_ty = self.resolve_type(&object.ty)?;
        let Some((target_ty, access_indices)) = narrowing_vector_store_target(pointee, &object_ty)
        else {
            return Ok(false);
        };
        let LlType::Vector(_, target_lanes) = target_ty else {
            return Ok(false);
        };

        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let target_type = self.type_id(&target_ty)?;
        let narrowed = self.fresh();
        let mut shuffle_ops = vec![Operand::IdRef(object_id), Operand::IdRef(object_id)];
        shuffle_ops.extend((0..target_lanes).map(Operand::LiteralBit32));
        instructions.push(Self::inst(
            Op::VectorShuffle,
            Some(target_type),
            Some(narrowed),
            shuffle_ops,
        ));

        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        let storage = match self.resolve_type(&ptr.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: store pointer is not a pointer: {other:?}"
                ))
            }
        };
        let store_ptr = if access_indices.is_empty() {
            ptr_id
        } else {
            let ptr_ty = self.ptr_type_id(storage, &target_ty)?;
            let result = self.fresh();
            let mut ops = vec![Operand::IdRef(ptr_id)];
            for idx in access_indices {
                ops.push(Operand::IdRef(self.const_uint(idx)?));
            }
            instructions.push(Self::inst(
                Op::InBoundsAccessChain,
                Some(ptr_ty),
                Some(result),
                ops,
            ));
            result
        };
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(store_ptr), Operand::IdRef(narrowed)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_widening_vector_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let object_ty = self.resolve_type(&object.ty)?;
        let (
            LlType::Vector(object_elem, object_lanes),
            LlType::Vector(pointee_elem, pointee_lanes),
        ) = (&object_ty, pointee)
        else {
            return Ok(false);
        };
        if object_lanes >= pointee_lanes || !types_compatible(object_elem, pointee_elem) {
            return Ok(false);
        }

        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let elem_type = self.type_id(object_elem)?;
        let mut lanes = Vec::with_capacity(*pointee_lanes as usize);
        for lane in 0..*object_lanes {
            let extracted = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(elem_type),
                Some(extracted),
                vec![Operand::IdRef(object_id), Operand::LiteralBit32(lane)],
            ));
            lanes.push(Operand::IdRef(extracted));
        }
        for _ in *object_lanes..*pointee_lanes {
            lanes.push(Operand::IdRef(self.undef_id(object_elem)?));
        }

        let widened_ty = self.type_id(pointee)?;
        let widened = self.fresh();
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(widened_ty),
            Some(widened),
            lanes,
        ));

        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(widened)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_vector_to_scalar_stores(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let object_ty = self.resolve_type(&object.ty)?;
        let LlType::Vector(elem, lanes) = object_ty else {
            return Ok(false);
        };
        let elem = self.resolve_type(&elem)?;
        if lanes == 0 {
            return Ok(false);
        }
        let store_ty = if types_compatible(pointee, &elem) {
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
        if lanes > 1 && !self.can_emit_gep_provenance_lane_ptrs(ptr, &store_ty)? {
            return Ok(false);
        }

        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        let elem_ty = self.type_id(&elem)?;
        let store_ty_id = self.type_id(&store_ty)?;
        for lane in 0..lanes {
            let lane_value = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(elem_ty),
                Some(lane_value),
                vec![Operand::IdRef(object_id), Operand::LiteralBit32(lane)],
            ));
            let store_value = if types_compatible(&elem, &store_ty) {
                lane_value
            } else {
                let converted = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(store_ty_id),
                    Some(converted),
                    vec![Operand::IdRef(lane_value)],
                ));
                converted
            };
            let lane_ptr = if lane == 0 {
                ptr_id
            } else {
                self.emit_gep_provenance_lane_ptr(ptr, &store_ty, lane, instructions)?
                    .ok_or_else(|| {
                        "native emitter: vector-to-scalar store lost pointer provenance".to_string()
                    })?
            };
            instructions.push(Self::inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(lane_ptr), Operand::IdRef(store_value)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_workgroup_vector_chunk_stores(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Vector(object_elem, object_lanes) = object_ty else {
            return Ok(false);
        };
        let LlType::Vector(_, _) = pointee else {
            return Ok(false);
        };
        let storage = match self.resolve_type(&ptr.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: store pointer is not a pointer: {other:?}"
                ))
            }
        };
        if storage != StorageClass::Workgroup {
            return Ok(false);
        }
        let (Some(object_bits), Some(object_elem_bits), Some(pointee_bits)) = (
            bitcast_width(object_ty),
            bitcast_width(object_elem),
            bitcast_width(pointee),
        ) else {
            return Ok(false);
        };
        if pointee_bits == 0
            || object_elem_bits == 0
            || object_bits % pointee_bits != 0
            || pointee_bits % object_elem_bits != 0
        {
            return Ok(false);
        }
        let chunk_count = object_bits / pointee_bits;
        let lanes_per_chunk = pointee_bits / object_elem_bits;
        if chunk_count == 0
            || lanes_per_chunk == 0
            || *object_lanes != chunk_count * lanes_per_chunk
        {
            return Ok(false);
        }

        let chunk_ty = if lanes_per_chunk == 1 {
            object_elem.as_ref().clone()
        } else {
            LlType::Vector(object_elem.clone(), lanes_per_chunk)
        };
        if bitcast_width(&chunk_ty) != Some(pointee_bits) {
            return Ok(false);
        }

        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let pointee_ty = self.type_id(pointee)?;
        let ptr_ty = self.ptr_type_id(StorageClass::Workgroup, pointee)?;
        let object_elem_ty = self.type_id(object_elem)?;
        let chunk_ty_id = self.type_id(&chunk_ty)?;
        for chunk in 0..chunk_count {
            let chunk_value = if chunk_count == 1 && lanes_per_chunk == *object_lanes {
                object_id
            } else if lanes_per_chunk == 1 {
                let extracted = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeExtract,
                    Some(object_elem_ty),
                    Some(extracted),
                    vec![
                        Operand::IdRef(object_id),
                        Operand::LiteralBit32(chunk * lanes_per_chunk),
                    ],
                ));
                extracted
            } else {
                let mut lanes = Vec::with_capacity(lanes_per_chunk as usize);
                for lane in 0..lanes_per_chunk {
                    let extracted = self.fresh();
                    instructions.push(Self::inst(
                        Op::CompositeExtract,
                        Some(object_elem_ty),
                        Some(extracted),
                        vec![
                            Operand::IdRef(object_id),
                            Operand::LiteralBit32(chunk * lanes_per_chunk + lane),
                        ],
                    ));
                    lanes.push(Operand::IdRef(extracted));
                }
                let chunk_value = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeConstruct,
                    Some(chunk_ty_id),
                    Some(chunk_value),
                    lanes,
                ));
                chunk_value
            };
            let store_value = if types_compatible(&chunk_ty, pointee) {
                chunk_value
            } else {
                let cast = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(pointee_ty),
                    Some(cast),
                    vec![Operand::IdRef(chunk_value)],
                ));
                cast
            };
            let store_ptr = if chunk == 0 {
                ptr_id
            } else {
                let index = self.const_uint(chunk)?;
                let chunk_ptr = self.fresh();
                instructions.push(Self::inst(
                    Op::PtrAccessChain,
                    Some(ptr_ty),
                    Some(chunk_ptr),
                    vec![Operand::IdRef(ptr_id), Operand::IdRef(index)],
                ));
                chunk_ptr
            };
            instructions.push(Self::inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(store_ptr), Operand::IdRef(store_value)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_same_width_reinterpret_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let (Some(object_bits), Some(pointee_bits)) =
            (bitcast_width(object_ty), bitcast_width(pointee))
        else {
            return Ok(false);
        };
        if object_bits == 0 || object_bits != pointee_bits {
            return Ok(false);
        }

        let ptr_id = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let pointee_ty = self.type_id(pointee)?;
        let stored = self.fresh();
        instructions.push(Self::inst(
            Op::Bitcast,
            Some(pointee_ty),
            Some(stored),
            vec![Operand::IdRef(object_id)],
        ));
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(stored)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_scalar_narrowing_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if matches!(object_ty, LlType::Vector(..)) || matches!(pointee, LlType::Vector(..)) {
            return Ok(false);
        }
        let (Some(object_bits), Some(pointee_bits)) =
            (bitcast_width(object_ty), bitcast_width(pointee))
        else {
            return Ok(false);
        };
        if object_bits == 0 || pointee_bits == 0 || object_bits >= pointee_bits {
            return Ok(false);
        }
        let storage = match self.resolve_type(&ptr.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
            _ => return Ok(false),
        };
        if !matches!(storage, StorageClass::Function | StorageClass::Private) {
            return Ok(false);
        }

        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let pointee_uint = LlType::Int(pointee_bits);
        let pointee_uint_type = self.type_id(&pointee_uint)?;

        let pointee_type = self.type_id(pointee)?;
        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(pointee_type),
            Some(loaded),
            vec![Operand::IdRef(ptr_id)],
        ));
        let old_uint = if *pointee == pointee_uint {
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
        let shift = self.const_int(pointee_bits, object_bits as u64)?;
        let shifted_down = self.fresh();
        instructions.push(Self::inst(
            Op::ShiftRightLogical,
            Some(pointee_uint_type),
            Some(shifted_down),
            vec![Operand::IdRef(old_uint), Operand::IdRef(shift)],
        ));
        let high_kept = self.fresh();
        instructions.push(Self::inst(
            Op::ShiftLeftLogical,
            Some(pointee_uint_type),
            Some(high_kept),
            vec![Operand::IdRef(shifted_down), Operand::IdRef(shift)],
        ));

        let object_uint = LlType::Int(object_bits);
        let object_as_uint = if *object_ty == object_uint {
            object_id
        } else {
            let object_uint_type = self.type_id(&object_uint)?;
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(object_uint_type),
                Some(id),
                vec![Operand::IdRef(object_id)],
            ));
            id
        };
        let object_wide = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(pointee_uint_type),
            Some(object_wide),
            vec![Operand::IdRef(object_as_uint)],
        ));

        let combined = self.fresh();
        instructions.push(Self::inst(
            Op::BitwiseOr,
            Some(pointee_uint_type),
            Some(combined),
            vec![Operand::IdRef(high_kept), Operand::IdRef(object_wide)],
        ));
        let stored = if *pointee == pointee_uint {
            combined
        } else {
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(pointee_type),
                Some(id),
                vec![Operand::IdRef(combined)],
            ));
            id
        };
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(stored)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_mismatched_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if types_compatible(pointee, object_ty) {
            return Ok(false);
        }
        Ok(
            self.emit_i64_to_i32_pair_struct_store(object, ptr, pointee, instructions)?
                || self.emit_aggregate_prefix_integer_reinterpret_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_first_vector_aggregate_reinterpret_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_first_scalar_aggregate_reinterpret_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_zero_scalar_to_aggregate_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_vector_as_scalar_array_store(object, ptr, pointee, instructions)?
                || self.emit_vector_to_scalar_stores(object, ptr, pointee, instructions)?
                || self.emit_workgroup_vector_chunk_stores(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_widening_vector_store(object, ptr, pointee, instructions)?
                || self.emit_narrowing_vector_store(object, ptr, pointee, instructions)?
                || self.emit_same_width_reinterpret_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_scalar_narrowing_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?
                || self.emit_single_member_struct_store(
                    object,
                    ptr,
                    pointee,
                    align,
                    instructions,
                )?
                || self.emit_leading_byte_reinterpret_store(
                    object,
                    ptr,
                    object_ty,
                    pointee,
                    instructions,
                )?,
        )
    }

    pub(in crate::native::emitter) fn emit_single_member_struct_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        pointee: &LlType,
        align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        let LlType::Struct(fields) = pointee else {
            return Ok(false);
        };
        let [field] = fields.as_slice() else {
            return Ok(false);
        };
        let leaf_pointee = self.resolve_type(field)?;
        let storage = match self.resolve_type(&ptr.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
            _ => return Ok(false),
        };

        let ptr_id = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
        let leaf_ptr_type = self.ptr_type_id(storage, &leaf_pointee)?;
        let index = self.const_uint(0)?;
        let leaf = self.fresh();
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(leaf_ptr_type),
            Some(leaf),
            vec![Operand::IdRef(ptr_id), Operand::IdRef(index)],
        ));

        let name = format!("metal2vulkan.union.member.{leaf}");
        self.values.insert(name.clone(), (leaf, ptr.ty.clone()));
        self.pointer_pointees.insert(name.clone(), leaf_pointee);
        self.pointer_storage.insert(name.clone(), storage);
        self.emit_store_resolved(
            object.clone(),
            TypedValue {
                ty: ptr.ty.clone(),
                value: LlValue::Local(name),
            },
            align,
            instructions,
        )?;
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_zero_scalar_to_aggregate_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !typed_value_is_zero(object) || !matches!(object_ty, LlType::Int(_)) {
            return Ok(false);
        }
        if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }

        let storage_pointee = function_storage_local_type(pointee);
        let storage_object = function_storage_local_type(object_ty);
        let (pointee_size, _) = self.raw_type_size_align(&storage_pointee)?;
        let (object_size, _) = self.raw_type_size_align(&storage_object)?;
        if pointee_size != object_size {
            return Ok(false);
        }

        let ptr_id = self.value_id(&ptr.value, &ptr.ty)?;
        let zero = self.const_null(&storage_pointee)?;
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(zero)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_first_vector_aggregate_reinterpret_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }
        let Some((vector_ty, access_path)) = leading_vector_store_target(pointee) else {
            return Ok(false);
        };
        let needs_bitcast = if types_compatible(&vector_ty, object_ty) {
            false
        } else {
            let vw = self.vector_total_bits(&vector_ty);
            let ow = self.vector_total_bits(object_ty);
            match (vw, ow) {
                (Some(v), Some(o)) if v != 0 && v == o => true,
                _ => return Ok(false),
            }
        };

        let storage = match self.resolve_type(&ptr.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: store pointer is not a pointer: {other:?}"
                ))
            }
        };
        let raw_object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        let object_id = if needs_bitcast {
            let vector_ty_id = self.type_id(&vector_ty)?;
            let id = self.fresh();
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(vector_ty_id),
                Some(id),
                vec![Operand::IdRef(raw_object_id)],
            ));
            id
        } else {
            raw_object_id
        };
        let ptr_id = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
        let vector_ptr_ty = self.ptr_type_id(storage, &vector_ty)?;
        let vector_ptr = self.fresh();
        let mut ops = vec![Operand::IdRef(ptr_id)];
        for idx in access_path {
            ops.push(Operand::IdRef(self.const_uint(idx)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(vector_ptr_ty),
            Some(vector_ptr),
            ops,
        ));
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(vector_ptr), Operand::IdRef(object_id)],
        ));
        Ok(true)
    }

    pub(in crate::native::emitter) fn vector_total_bits(&self, ty: &LlType) -> Option<u32> {
        match self.resolve_type(ty).ok()? {
            LlType::Vector(elem, lanes) => {
                let elem = self.resolve_type(&elem).ok()?;
                Some(bitcast_width(&elem)? * lanes)
            }
            _ => None,
        }
    }

    pub(in crate::native::emitter) fn emit_first_scalar_aggregate_reinterpret_store(
        &mut self,
        object: &TypedValue,
        ptr: &TypedValue,
        object_ty: &LlType,
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        staged_emit(instructions, |instructions| {
            if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
                return Ok(false);
            }
            if matches!(object_ty, LlType::Vector(_, _)) {
                return Ok(false);
            }
            let storage = match self.resolve_type(&ptr.ty)? {
                LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr.value, addrspace)?,
                other => {
                    return Err(format!(
                        "native emitter: store pointer is not a pointer: {other:?}"
                    ))
                }
            };
            if !matches!(
                storage,
                StorageClass::Function | StorageClass::Workgroup | StorageClass::Private
            ) {
                return Ok(false);
            }
            let Some((scalar_ty, access_path)) = first_scalar_access_path(pointee) else {
                return Ok(false);
            };
            let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
            let store_value = if types_compatible(object_ty, &scalar_ty) {
                object_id
            } else {
                let Some(object_bits) = bitcast_width(object_ty) else {
                    return Ok(false);
                };
                let Some(scalar_bits) = bitcast_width(&scalar_ty) else {
                    return Ok(false);
                };
                if object_bits != scalar_bits {
                    return Ok(false);
                }
                let scalar_type = self.type_id(&scalar_ty)?;
                let cast = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(scalar_type),
                    Some(cast),
                    vec![Operand::IdRef(object_id)],
                ));
                cast
            };

            let ptr_id = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
            let scalar_ptr_ty = self.ptr_type_id(storage, &scalar_ty)?;
            let scalar_ptr = self.fresh();
            let mut ops = vec![Operand::IdRef(ptr_id)];
            for idx in access_path {
                ops.push(Operand::IdRef(self.const_uint(idx)?));
            }
            instructions.push(Self::inst(
                Op::InBoundsAccessChain,
                Some(scalar_ptr_ty),
                Some(scalar_ptr),
                ops,
            ));
            instructions.push(Self::inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(scalar_ptr), Operand::IdRef(store_value)],
            ));
            Ok(true)
        })
    }

    fn emit_aggregate_field_pointer(
        &mut self,
        storage: StorageClass,
        field_ty: &LlType,
        ptr: Word,
        access_path: &[u32],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let field_ptr_ty = self.ptr_type_id(storage, field_ty)?;
        let field_ptr = self.fresh();
        let mut ops = vec![Operand::IdRef(ptr)];
        for idx in access_path {
            ops.push(Operand::IdRef(self.const_uint(*idx)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(field_ptr_ty),
            Some(field_ptr),
            ops,
        ));
        Ok(field_ptr)
    }

    pub(in crate::native::emitter) fn emit_first_pointer_aggregate_reinterpret_load(
        &mut self,
        result_name: &str,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }
        let Some((field_ty, access_path)) = first_pointer_access_path(pointee) else {
            return Ok(false);
        };
        if &field_ty != result_ty {
            return Ok(false);
        }

        let storage = match self.resolve_type(&ptr_value.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr_value.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: load pointer is not a pointer: {other:?}"
                ))
            }
        };
        let key = LocalPointerField {
            root: ptr,
            indices: access_path.clone(),
        };
        let pointer_table = self.bda_device_pointers
            && matches!(result_ty, LlType::Ptr(0))
            && self.bda_pointer_tables.contains(result_name)
            && !self.local_pointer_fields.contains_key(&key);
        if pointer_table || self.bda_forwards_local_pointer_field(result_name, result_ty) {
            let field_ptr = self.emit_aggregate_field_pointer(
                storage,
                &LlType::Int(64),
                ptr,
                &access_path,
                instructions,
            )?;
            self.emit_bda_address_from_field(result_name, field_ptr, instructions)?;
            return Ok(true);
        }
        if storage == StorageClass::Function
            || (storage == StorageClass::Private && type_contains_pointer(pointee))
            || self.local_pointer_fields.contains_key(&key)
        {
            self.emit_pointer_from_local_field_key(
                result_name,
                result,
                result_ty,
                &key,
                instructions,
            )?;
            return Ok(true);
        }
        let field_ptr =
            self.emit_aggregate_field_pointer(storage, &field_ty, ptr, &access_path, instructions)?;

        let result_type = self.type_id(result_ty)?;
        instructions.push(Self::inst(
            Op::Load,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(field_ptr)],
        ));
        if let LlType::Ptr(addrspace) = result_ty {
            self.pointer_storage
                .insert(result_name.to_string(), llvm_pointer_storage(*addrspace)?);
        }
        Ok(true)
    }

    /// A vector loaded through a pointer to an aggregate that starts with a scalar array of the vector's size (a buffer of
    /// `struct { packed_uint4 header; }` slots, `{ { [4 x i32] } }`, read as float4): the load is of that array, reached by
    /// an access chain, and each element becomes a lane.
    pub(in crate::native::emitter) fn emit_leading_array_vector_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pointee, LlType::Struct(_)) {
            return Ok(false);
        }
        let Some((array_ty, path)) = leading_array_covered_by_vector(pointee, result_ty) else {
            return Ok(false);
        };
        let LlType::Ptr(addrspace) = self.resolve_type(&ptr_value.ty)? else {
            return Ok(false);
        };
        let storage = self.pointer_storage_for(&ptr_value.value, addrspace)?;
        let array_ptr = self.emit_aggregate_field_pointer(storage, &array_ty, ptr, &path, instructions)?;
        self.emit_scalar_array_as_vector_load(result, &array_ty, result_ty, array_ptr, instructions)
    }

    pub(in crate::native::emitter) fn emit_first_vector_aggregate_reinterpret_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }
        let Some((vector_ty, access_path)) = leading_vector_store_target(pointee) else {
            return Ok(false);
        };
        let direct = types_compatible(&vector_ty, result_ty);
        let bitcast = !direct
            && matches!(
                (&vector_ty, result_ty),
                (LlType::Vector(..), LlType::Vector(..))
                    if bitcast_width(&vector_ty).is_some()
                        && bitcast_width(&vector_ty) == bitcast_width(result_ty)
            );
        let widen = !direct
            && !bitcast
            && matches!(
                (&vector_ty, result_ty),
                (LlType::Vector(v_elem, v_lanes), LlType::Vector(r_elem, r_lanes))
                    if types_compatible(v_elem, r_elem) && r_lanes > v_lanes
            );
        if !direct && !bitcast && !widen {
            return Ok(false);
        }

        let storage = match self.resolve_type(&ptr_value.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr_value.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: load pointer is not a pointer: {other:?}"
                ))
            }
        };
        let vector_ptr_ty = self.ptr_type_id(storage, &vector_ty)?;
        let vector_ptr = self.fresh();
        let mut ops = vec![Operand::IdRef(ptr)];
        for idx in access_path {
            ops.push(Operand::IdRef(self.const_uint(idx)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(vector_ptr_ty),
            Some(vector_ptr),
            ops,
        ));

        if widen {
            return self.emit_widening_vector_load(
                result,
                &vector_ty,
                result_ty,
                vector_ptr,
                instructions,
            );
        }
        let result_type = self.type_id(result_ty)?;
        if direct {
            instructions.push(Self::inst(
                Op::Load,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(vector_ptr)],
            ));
        } else {
            let vector_type = self.type_id(&vector_ty)?;
            let loaded = self.fresh();
            instructions.push(Self::inst(
                Op::Load,
                Some(vector_type),
                Some(loaded),
                vec![Operand::IdRef(vector_ptr)],
            ));
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(loaded)],
            ));
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_first_scalar_aggregate_reinterpret_load(
        &mut self,
        result: Word,
        pointee: &LlType,
        result_ty: &LlType,
        ptr_value: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pointee, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }
        let Some((scalar_ty, access_path)) = first_scalar_access_path(pointee) else {
            return Ok(false);
        };
        let Some(scalar_bits) = bitcast_width(&scalar_ty) else {
            return Ok(false);
        };
        let Some(result_bits) = bitcast_width(result_ty) else {
            return Ok(false);
        };
        let int_narrowing_load =
            matches!((&scalar_ty, result_ty), (LlType::Int(src), LlType::Int(dst)) if src > dst);
        if scalar_bits != result_bits && !int_narrowing_load {
            return Ok(false);
        }

        let storage = match self.resolve_type(&ptr_value.ty)? {
            LlType::Ptr(addrspace) => self.pointer_storage_for(&ptr_value.value, addrspace)?,
            other => {
                return Err(format!(
                    "native emitter: load pointer is not a pointer: {other:?}"
                ))
            }
        };
        let scalar_ptr_ty = self.ptr_type_id(storage, &scalar_ty)?;
        let scalar_ptr = self.fresh();
        let mut ops = vec![Operand::IdRef(ptr)];
        for idx in access_path {
            ops.push(Operand::IdRef(self.const_uint(idx)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(scalar_ptr_ty),
            Some(scalar_ptr),
            ops,
        ));

        let scalar_type = self.type_id(&scalar_ty)?;
        if types_compatible(&scalar_ty, result_ty) {
            instructions.push(Self::inst(
                Op::Load,
                Some(scalar_type),
                Some(result),
                vec![Operand::IdRef(scalar_ptr)],
            ));
            return Ok(true);
        }

        let loaded = self.fresh();
        instructions.push(Self::inst(
            Op::Load,
            Some(scalar_type),
            Some(loaded),
            vec![Operand::IdRef(scalar_ptr)],
        ));
        let result_type = self.type_id(result_ty)?;
        if int_narrowing_load {
            instructions.push(Self::inst(
                Op::UConvert,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(loaded)],
            ));
        } else {
            instructions.push(Self::inst(
                Op::Bitcast,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(loaded)],
            ));
        }
        Ok(true)
    }
}
