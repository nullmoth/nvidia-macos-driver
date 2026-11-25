use super::*;
use crate::float16::f32_to_f16_bits;

impl Emitter {
    fn storage_type(ty: &LlType) -> LlType {
        Self::storage_type_at_depth(ty, false)
    }

    fn storage_type_at_depth(ty: &LlType, nested: bool) -> LlType {
        match ty {
            LlType::Ptr(_) if nested => LlType::Int(64),
            LlType::BFloat => LlType::Int(16),
            LlType::Int(bits) => match spirv_int_width(*bits) {
                Ok(legal) if legal != *bits => LlType::Int(legal),
                _ => ty.clone(),
            },
            LlType::Vector(elem, lanes) => {
                LlType::Vector(Box::new(Self::storage_type_at_depth(elem, true)), *lanes)
            }
            LlType::Array(elem, len) => {
                LlType::Array(Box::new(Self::storage_type_at_depth(elem, true)), *len)
            }
            LlType::Struct(fields) => LlType::Struct(
                fields
                    .iter()
                    .map(|field| Self::storage_type_at_depth(field, true))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    pub(super) fn type_id(&mut self, ty: &LlType) -> Result<Word, String> {
        let ty = Self::storage_type(&self.resolve_type(ty)?);
        if let Some(id) = self.interner.types.get(&ty) {
            return Ok(*id);
        }
        if let LlType::Ptr(addrspace) = &ty {
            let storage = llvm_pointer_storage(*addrspace)?;
            let key = (storage, LlType::Int(8));
            if let Some(id) = self.interner.ptr_types.get(&key).copied() {
                self.interner.types.insert(ty, id);
                return Ok(id);
            }
        }
        let id = self.fresh();
        let inst = match &ty {
            LlType::Void => Self::inst(Op::TypeVoid, None, Some(id), vec![]),
            LlType::Bool => Self::inst(Op::TypeBool, None, Some(id), vec![]),
            LlType::Float => Self::inst(
                Op::TypeFloat,
                None,
                Some(id),
                vec![Operand::LiteralBit32(32)],
            ),
            LlType::Half => {
                self.require_capability(Capability::Float16);
                Self::inst(
                    Op::TypeFloat,
                    None,
                    Some(id),
                    vec![Operand::LiteralBit32(16)],
                )
            }
            LlType::BFloat => {
                return Err(
                    "native emitter: BFloat must be normalized to its u16 storage type before \
                     type_id"
                        .into(),
                )
            }
            LlType::Int(bits) => {
                match *bits {
                    8 => self.require_capability(Capability::Int8),
                    16 => self.require_capability(Capability::Int16),
                    64 => self.require_capability(Capability::Int64),
                    _ => {}
                }
                Self::inst(
                    Op::TypeInt,
                    None,
                    Some(id),
                    vec![Operand::LiteralBit32(*bits), Operand::LiteralBit32(0)],
                )
            }
            LlType::Ptr(addrspace) => {
                let storage = llvm_pointer_storage(*addrspace)?;
                let uchar = self.type_id(&LlType::Int(8))?;
                self.interner
                    .ptr_types
                    .insert((storage, LlType::Int(8)), id);
                Self::inst(
                    Op::TypePointer,
                    None,
                    Some(id),
                    vec![Operand::StorageClass(storage), Operand::IdRef(uchar)],
                )
            }
            LlType::Vector(elem, lanes) if *lanes > 4 => {
                let elem = self.type_id(elem)?;
                let len = self.const_uint(*lanes)?;
                Self::inst(
                    Op::TypeArray,
                    None,
                    Some(id),
                    vec![Operand::IdRef(elem), Operand::IdRef(len)],
                )
            }
            LlType::Vector(elem, lanes) => {
                let elem = self.type_id(elem)?;
                Self::inst(
                    Op::TypeVector,
                    None,
                    Some(id),
                    vec![Operand::IdRef(elem), Operand::LiteralBit32(*lanes)],
                )
            }
            LlType::Array(elem, len) => {
                let elem = self.type_id(elem)?;
                if *len == 0 {
                    Self::inst(
                        Op::TypeRuntimeArray,
                        None,
                        Some(id),
                        vec![Operand::IdRef(elem)],
                    )
                } else {
                    let len = self.const_uint(*len)?;
                    Self::inst(
                        Op::TypeArray,
                        None,
                        Some(id),
                        vec![Operand::IdRef(elem), Operand::IdRef(len)],
                    )
                }
            }
            LlType::Struct(fields) => {
                let mut operands = Vec::with_capacity(fields.len());
                for field in fields {
                    operands.push(Operand::IdRef(self.type_id(field)?));
                }
                Self::inst(Op::TypeStruct, None, Some(id), operands)
            }
            LlType::Named(name) => {
                return Err(format!("native emitter: unresolved named type {name}"));
            }
        };
        self.module.types_global_values.push(inst);
        self.interner.types.insert(ty, id);
        Ok(id)
    }

    pub(super) fn signed_int_type_id(&mut self, ty: &LlType) -> Result<Word, String> {
        let ty = Self::storage_type(&self.resolve_type(ty)?);
        if let Some(id) = self.interner.signed_int_types.get(&ty) {
            return Ok(*id);
        }
        let id = self.fresh();
        let inst = match &ty {
            LlType::Int(bits) => {
                match *bits {
                    8 => self.require_capability(Capability::Int8),
                    16 => self.require_capability(Capability::Int16),
                    64 => self.require_capability(Capability::Int64),
                    _ => {}
                }
                Self::inst(
                    Op::TypeInt,
                    None,
                    Some(id),
                    vec![Operand::LiteralBit32(*bits), Operand::LiteralBit32(1)],
                )
            }
            LlType::Vector(elem, lanes) if is_integer_type(elem) => {
                let elem = self.signed_int_type_id(elem)?;
                Self::inst(
                    Op::TypeVector,
                    None,
                    Some(id),
                    vec![Operand::IdRef(elem), Operand::LiteralBit32(*lanes)],
                )
            }
            _ => {
                return Err(format!(
                    "native emitter: signed integer type requested for {ty:?}"
                ))
            }
        };
        self.module.types_global_values.push(inst);
        self.interner.signed_int_types.insert(ty, id);
        Ok(id)
    }

    pub(super) fn ptr_type_id(
        &mut self,
        storage: StorageClass,
        pointee: &LlType,
    ) -> Result<Word, String> {
        let pointee = self.resolve_type(pointee)?;
        let key = (storage, pointee.clone());
        if let Some(id) = self.interner.ptr_types.get(&key) {
            return Ok(*id);
        }
        let pointee_id = self.type_id(&pointee)?;
        if storage == StorageClass::PhysicalStorageBuffer {
            self.ensure_physical_storage_layout(&pointee, pointee_id, true)?;
        }
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::TypePointer,
            None,
            Some(id),
            vec![Operand::StorageClass(storage), Operand::IdRef(pointee_id)],
        ));
        self.interner.ptr_types.insert(key, id);
        Ok(id)
    }

    fn ensure_physical_storage_layout(
        &mut self,
        ty: &LlType,
        type_id: Word,
        root: bool,
    ) -> Result<(), String> {
        let ty = Self::storage_type(&self.resolve_type(ty)?);
        let has_decoration = |annotations: &[Instruction], target, decoration| {
            annotations.iter().any(|instruction| {
                instruction.class.opcode == Op::Decorate
                    && instruction.operands.first() == Some(&Operand::IdRef(target))
                    && instruction.operands.get(1) == Some(&Operand::Decoration(decoration))
            })
        };
        match &ty {
            LlType::Struct(fields) => {
                if root
                    && !has_decoration(&self.module.annotations, type_id, spirv::Decoration::Block)
                {
                    self.module.annotations.push(Self::inst(
                        Op::Decorate,
                        None,
                        None,
                        vec![
                            Operand::IdRef(type_id),
                            Operand::Decoration(spirv::Decoration::Block),
                        ],
                    ));
                }
                let mut offset = 0u64;
                for (member, field) in fields.iter().enumerate() {
                    let (size, align) = self.raw_type_size_align(field)?;
                    offset = offset.div_ceil(align) * align;
                    let already_decorated = self.module.annotations.iter().any(|instruction| {
                        instruction.class.opcode == Op::MemberDecorate
                            && instruction.operands.first() == Some(&Operand::IdRef(type_id))
                            && instruction.operands.get(1)
                                == Some(&Operand::LiteralBit32(member as u32))
                            && instruction.operands.get(2)
                                == Some(&Operand::Decoration(spirv::Decoration::Offset))
                    });
                    if !already_decorated {
                        self.module.annotations.push(Self::inst(
                            Op::MemberDecorate,
                            None,
                            None,
                            vec![
                                Operand::IdRef(type_id),
                                Operand::LiteralBit32(member as u32),
                                Operand::Decoration(spirv::Decoration::Offset),
                                Operand::LiteralBit32(offset as u32),
                            ],
                        ));
                    }
                    let field_id = self.type_id(field)?;
                    self.ensure_physical_storage_layout(field, field_id, false)?;
                    offset += size;
                }
            }
            LlType::Array(element, _) | LlType::Vector(element, 5..) => {
                let (stride, _) = self.raw_type_size_align(element)?;
                if !has_decoration(
                    &self.module.annotations,
                    type_id,
                    spirv::Decoration::ArrayStride,
                ) {
                    self.module.annotations.push(Self::inst(
                        Op::Decorate,
                        None,
                        None,
                        vec![
                            Operand::IdRef(type_id),
                            Operand::Decoration(spirv::Decoration::ArrayStride),
                            Operand::LiteralBit32(stride as u32),
                        ],
                    ));
                }
                let element_id = self.type_id(element)?;
                self.ensure_physical_storage_layout(element, element_id, false)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn const_uint(&mut self, value: u32) -> Result<Word, String> {
        if let Some(id) = self.interner.uint_constants.get(&value) {
            return Ok(*id);
        }
        let uint = self.type_id(&LlType::Int(32))?;
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Constant,
            Some(uint),
            Some(id),
            vec![Operand::LiteralBit32(value)],
        ));
        self.interner.uint_constants.insert(value, id);
        Ok(id)
    }

    pub(super) fn const_uint_shaped(&mut self, value: u32, n: u32) -> Result<Word, String> {
        let scalar = self.const_uint(value)?;
        if n <= 1 {
            return Ok(scalar);
        }
        let vec_ty = LlType::Vector(Box::new(LlType::Int(32)), n);
        self.const_composite_with_constituents(&vec_ty, vec![scalar; n as usize])
    }

    pub(super) fn resolve_type(&self, ty: &LlType) -> Result<LlType, String> {
        match ty {
            LlType::Int(1) => Ok(LlType::Bool),
            LlType::Named(name) => {
                let aliased = self
                    .ir
                    .types
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("native emitter: unknown named type {name}"))?;
                self.resolve_type(&aliased)
            }
            LlType::Vector(elem, 1) => self.resolve_type(elem),
            LlType::Vector(elem, lanes) => {
                Ok(LlType::Vector(Box::new(self.resolve_type(elem)?), *lanes))
            }
            LlType::Array(elem, len) => Ok(LlType::Array(Box::new(self.resolve_type(elem)?), *len)),
            LlType::Struct(fields) => fields
                .iter()
                .map(|f| self.resolve_type(f))
                .collect::<Result<Vec<_>, _>>()
                .map(LlType::Struct),
            _ => Ok(ty.clone()),
        }
    }

    pub(super) fn undef_id(&mut self, ty: &LlType) -> Result<Word, String> {
        let ty = self.resolve_type(ty)?;
        if let Some(id) = self.interner.undefs.get(&ty) {
            return Ok(*id);
        }
        let type_id = self.type_id(&ty)?;
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Undef,
            Some(type_id),
            Some(id),
            vec![],
        ));
        self.interner.undefs.insert(ty, id);
        Ok(id)
    }

    pub(super) fn result_id(&mut self, name: &str, ty: &LlType) -> Result<Word, String> {
        let ty = self.resolve_type(ty)?;
        if let Some((id, have_ty)) = self.values.get(name).cloned() {
            let have = self.resolve_type(&have_ty)?;
            if !types_compatible(&have, &ty) {
                return Err(format!(
                    "native emitter: SSA value {name} was reserved as {have:?}, defined as {ty:?}"
                ));
            }
            return Ok(id);
        }
        let id = self.fresh();
        self.values.insert(name.to_string(), (id, ty));
        Ok(id)
    }

    pub(super) fn phi_value_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if let LlValue::Local(name) = value {
            if !self.values.contains_key(name) {
                return self.result_id(name, ty);
            }
        }
        self.value_id_in(value, ty, instructions)
    }

    pub(super) fn value_id(&mut self, value: &LlValue, ty: &LlType) -> Result<Word, String> {
        match value {
            LlValue::Local(name) => {
                if self.pointer_phi_values.contains(name)
                    && matches!(self.resolve_type(ty)?, LlType::Ptr(_))
                {
                    if let Some(address) = self.raw_offsets.get(name).and_then(|raw| {
                        (raw.const_off == 0 && raw.dyn_terms.is_empty())
                            .then_some(raw.device_addr_base)
                            .flatten()
                    }) {
                        return Ok(address);
                    }
                }
                let (id, have_ty) = if let Some((id, have_ty)) = self.values.get(name).cloned() {
                    (id, have_ty)
                } else if self.construct_tree_active
                    || self.relooper_feed
                    || self
                        .tir_result_types
                        .get(name)
                        .is_some_and(|ty| !matches!(ty, LlType::Ptr(_)))
                {
                    let Some(have_ty) = self.tir_result_types.get(name).cloned() else {
                        return Err(format!("native emitter: unknown SSA value {name}"));
                    };
                    let want = self.resolve_type(ty)?;
                    let have = self.resolve_type(&have_ty)?;
                    if !types_compatible(&have, &want) {
                        return Err(format!(
                            "native emitter: SSA value {name} has type {have:?}, used as {want:?}"
                        ));
                    }
                    (self.result_id(name, &have_ty)?, have_ty)
                } else if self.pointer_phi_values.contains(name) {
                    let Some(have_ty) = self.tir_result_types.get(name).cloned() else {
                        return Err(format!("native emitter: unknown SSA value {name}"));
                    };
                    let want = self.resolve_type(ty)?;
                    let have = self.resolve_type(&have_ty)?;
                    if !types_compatible(&have, &want) {
                        return Err(format!(
                            "native emitter: SSA value {name} has type {have:?}, used as {want:?}"
                        ));
                    }
                    if let LlType::Ptr(addrspace) = have {
                        let phi_incoming = self.tir_phi_incomings.get(name).cloned();
                        let reserved_bda = match phi_incoming.as_ref() {
                            Some(incoming) => self
                                .reserve_bda_address_phi(name, incoming, &have_ty)?
                                .is_some(),
                            None => false,
                        };
                        if !reserved_bda {
                            self.reserve_pointer_phi_provenance(name)?;
                            let merge_meta = phi_incoming
                                .map(|incoming| {
                                    self.pointer_merge_meta(
                                        &incoming
                                            .iter()
                                            .map(|(value, _)| value)
                                            .collect::<Vec<_>>(),
                                        &have_ty,
                                    )
                                })
                                .transpose()?
                                .flatten();
                            self.pointer_storage
                                .entry(name.clone())
                                .or_insert(match &merge_meta {
                                    Some(meta) => meta.storage,
                                    None => llvm_pointer_storage(addrspace)?,
                                });
                            if !self.pointer_pointees.contains_key(name) {
                                if let Some(pointee) = merge_meta
                                    .and_then(|meta| meta.pointee)
                                    .or_else(|| self.tir_use_pointees.get(name).cloned())
                                {
                                    self.pointer_pointees.insert(name.clone(), pointee);
                                }
                            }
                        }
                    }
                    (self.result_id(name, &have_ty)?, have_ty)
                } else if !self.selected_pointers.contains_key(name)
                    && !self.selected_access_trees.contains_key(name)
                    && !self.selected_load_pointers.contains_key(name)
                    && self.tir_result_types.contains_key(name)
                {
                    let have_ty = self.tir_result_types.get(name).cloned().unwrap();
                    let want = self.resolve_type(ty)?;
                    let have = self.resolve_type(&have_ty)?;
                    if !types_compatible(&have, &want) {
                        return Err(format!(
                            "native emitter: SSA value {name} has type {have:?}, used as {want:?}"
                        ));
                    }
                    if let LlType::Ptr(addrspace) = have {
                        self.pointer_storage
                            .entry(name.clone())
                            .or_insert(llvm_pointer_storage(addrspace)?);
                        if !self.pointer_pointees.contains_key(name) {
                            if let Some(pointee) = self.tir_use_pointees.get(name).cloned() {
                                self.pointer_pointees.insert(name.clone(), pointee);
                            }
                        }
                    }
                    (self.result_id(name, &have_ty)?, have_ty)
                } else {
                    return Err(format!("native emitter: unknown SSA value {name}"));
                };
                let want = self.resolve_type(ty)?;
                let have = self.resolve_type(&have_ty)?;
                if !types_compatible(&have, &want) {
                    return Err(format!(
                        "native emitter: SSA value {name} has type {have:?}, used as {want:?}"
                    ));
                }
                Ok(id)
            }
            LlValue::Global(name) => {
                let (id, _have_ty) = self
                    .global_values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("native emitter: unknown global value {name}"))?;
                Ok(id)
            }
            LlValue::Gep(_) => Err(
                "native emitter: getelementptr value requires instruction-local materialization"
                    .to_string(),
            ),
            LlValue::IntToPtr { .. } => Err(
                "native emitter: inttoptr value requires instruction-local materialization"
                    .to_string(),
            ),
            LlValue::Bool(value) => match self.resolve_type(ty)? {
                LlType::Bool | LlType::Int(1) => self.const_bool(*value),
                other => Err(format!(
                    "native emitter: bool literal {value} used as non-bool type {other:?}"
                )),
            },
            LlValue::Int(value) => match self.resolve_type(ty)? {
                LlType::Int(bits) => self.const_int(bits, *value),
                other => self.int_literal_bits_as_float(*value, &other),
            },
            LlValue::SignedInt(value) => match self.resolve_type(ty)? {
                LlType::Int(bits) => self.const_signed_int(bits, *value),
                other => self.int_literal_bits_as_float(*value as u64, &other),
            },
            LlValue::Hex(bits) => match self.resolve_type(ty)? {
                LlType::Int(width) => self.const_int(width, *bits),
                LlType::Float => self.const_float32(f64::from_bits(*bits) as f32),
                other => Err(format!(
                    "native emitter: hex literal 0x{bits:x} used as unsupported type {other:?}"
                )),
            },
            LlValue::Float(value) => match self.resolve_type(ty)? {
                LlType::Float => self.const_float32(*value as f32),
                LlType::Half => self.const_float16_bits(f32_to_f16_bits(*value as f32)),
                other => Err(format!(
                    "native emitter: float literal {value} used as non-float type {other:?}"
                )),
            },
            LlValue::Float32Bits(bits) => match self.resolve_type(ty)? {
                LlType::Float => self.const_float32_bits(*bits),
                other => Err(format!(
                    "native emitter: float32 bit literal f0x{bits:08x} used as non-float type {other:?}"
                )),
            },
            LlValue::HalfBits(bits) => match self.resolve_type(ty)? {
                LlType::Half => self.const_float16_bits(*bits),
                other => Err(format!(
                    "native emitter: half literal 0xH{bits:04x} used as non-half type {other:?}"
                )),
            },
            LlValue::BFloatBits(bits) => match self.resolve_type(ty)? {
                LlType::BFloat => self.const_int(16, *bits as u64),
                other => Err(format!(
                    "native emitter: bfloat literal 0xR{bits:04x} used as non-bfloat type {other:?}"
                )),
            },
            LlValue::Vector(_) | LlValue::Array(_) | LlValue::Struct(_) | LlValue::Splat(_) => Err(
                "native emitter: aggregate literal requires instruction-local materialization"
                    .into(),
            ),
            LlValue::Zero => self.const_null(ty),
            LlValue::Undef => self.undef_id(ty),
        }
    }

    pub(super) fn value_id_in(
        &mut self,
        value: &LlValue,
        ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let result_ty = self.resolve_type(ty)?;
        if matches!(result_ty, LlType::Ptr(_)) {
            if let LlValue::Local(name) = value {
                if self.selected_pointers.contains_key(name) && !self.values.contains_key(name) {
                    self.materialize_selected_pointer_value(name, instructions)?;
                }
            }
        }
        if let LlValue::Gep(gep) = value {
            let name = format!("%air.constgep.{}", self.module.id_bound());
            if let Some(id) = self.emit_gep_result(&name, gep, instructions)? {
                return Ok(id);
            }
            if matches!(gep.base.value, LlValue::Global(_)) {
                if let Some((id, _)) = self.values.get(&name) {
                    return Ok(*id);
                }
            }
            return Err(format!(
                "native emitter: getelementptr value `{name}` did not materialize a pointer"
            ));
        }
        if let LlValue::IntToPtr {
            source,
            destination,
        } = value
        {
            let expected = self.resolve_type(ty)?;
            let destination = self.resolve_type(destination)?;
            if expected != destination {
                return Err(format!(
                    "native emitter: inttoptr constant-expression destination {destination:?} does not match typed operand {expected:?}"
                ));
            }
            let name = format!("%air.const.inttoptr.{}", self.module.id_bound());
            self.emit_inttoptr_resolved(
                source.as_ref().clone(),
                destination,
                name.clone(),
                instructions,
            )?;
            return self.value_id(&LlValue::Local(name), ty);
        }
        if let Some(elem) = self.one_lane_vector_elem(ty)? {
            match value {
                LlValue::Splat(lane) => {
                    let lane_ty = self.resolve_type(&lane.ty)?;
                    if lane_ty != elem {
                        return Err(format!(
                            "native emitter: splat lane type {lane_ty:?} does not match {elem:?}"
                        ));
                    }
                    return self.value_id(&lane.value, &lane.ty);
                }
                LlValue::Vector(lanes) => {
                    let [lane] = lanes.as_slice() else {
                        return Err(format!(
                            "native emitter: vector literal has {} lanes, expected 1",
                            lanes.len()
                        ));
                    };
                    let lane_ty = self.resolve_type(&lane.ty)?;
                    if lane_ty != elem {
                        return Err(format!(
                            "native emitter: vector lane type {lane_ty:?} does not match {elem:?}"
                        ));
                    }
                    return self.value_id(&lane.value, &lane.ty);
                }
                _ => return self.value_id(value, ty),
            }
        }
        if let Some(id) = self.const_composite_id(value, &result_ty)? {
            return Ok(id);
        }
        let LlType::Vector(elem, count) = &result_ty else {
            if let LlValue::Splat(lane) = value {
                let lane_ty = self.resolve_type(&lane.ty)?;
                if lane_ty == result_ty {
                    return self.value_id(&lane.value, &lane.ty);
                }
            } else if let LlValue::Vector(lanes) = value {
                if let [lane] = lanes.as_slice() {
                    let lane_ty = self.resolve_type(&lane.ty)?;
                    if lane_ty == result_ty {
                        return self.value_id(&lane.value, &lane.ty);
                    }
                }
                return Err(format!(
                    "native emitter: vector literal used as non-vector type {result_ty:?}"
                ));
            }
            return self.value_id(value, ty);
        };
        if let Some(id) = self.const_composite_id(value, &result_ty)? {
            return Ok(id);
        }
        if let LlValue::Splat(lane) = value {
            let lane_ty = self.resolve_type(&lane.ty)?;
            if lane_ty != **elem {
                return Err(format!(
                    "native emitter: splat lane type {lane_ty:?} does not match {elem:?}"
                ));
            }
            let lane_id = self.value_id(&lane.value, &lane.ty)?;
            let result_type = self.type_id(&result_ty)?;
            let result = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeConstruct,
                Some(result_type),
                Some(result),
                (0..*count).map(|_| Operand::IdRef(lane_id)).collect(),
            ));
            return Ok(result);
        }
        let LlValue::Vector(lanes) = value else {
            return self.value_id(value, ty);
        };
        if lanes.len() != *count as usize {
            return Err(format!(
                "native emitter: vector literal has {} lanes, expected {count}",
                lanes.len()
            ));
        }
        let result_type = self.type_id(&result_ty)?;
        let mut ops = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let lane_ty = self.resolve_type(&lane.ty)?;
            if lane_ty != **elem {
                return Err(format!(
                    "native emitter: vector lane type {lane_ty:?} does not match {elem:?}"
                ));
            }
            ops.push(Operand::IdRef(self.value_id(&lane.value, &lane.ty)?));
        }
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::CompositeConstruct,
            Some(result_type),
            Some(result),
            ops,
        ));
        Ok(result)
    }

    pub(super) fn one_lane_vector_elem(&self, ty: &LlType) -> Result<Option<LlType>, String> {
        match ty {
            LlType::Named(name) => {
                let aliased = self
                    .ir
                    .types
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("native emitter: unknown named type {name}"))?;
                self.one_lane_vector_elem(&aliased)
            }
            LlType::Vector(elem, 1) => self.resolve_type(elem).map(Some),
            _ => Ok(None),
        }
    }

    pub(super) fn const_composite_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
    ) -> Result<Option<Word>, String> {
        let ty = self.resolve_type(ty)?;
        let (elem, count, is_array) = match &ty {
            LlType::Vector(elem, count) => (elem, count, false),
            LlType::Array(elem, count) => (elem, count, true),
            LlType::Struct(fields) => {
                let LlValue::Struct(values) = value else {
                    if matches!(value, LlValue::Vector(_) | LlValue::Array(_)) {
                        return Err(
                            "native emitter: non-struct aggregate literal used as struct type"
                                .into(),
                        );
                    }
                    return Ok(None);
                };
                if values.len() != fields.len() {
                    return Err(format!(
                        "native emitter: struct literal has {} fields, expected {}",
                        values.len(),
                        fields.len()
                    ));
                }
                let mut constituents = Vec::with_capacity(values.len());
                for (index, (field, expected_ty)) in values.iter().zip(fields).enumerate() {
                    let field_ty = self.resolve_type(&field.ty)?;
                    let expected_ty = self.resolve_type(expected_ty)?;
                    if field_ty != expected_ty {
                        return Err(format!(
                            "native emitter: struct field {index} type {field_ty:?} does not match {expected_ty:?}"
                        ));
                    }
                    let Some(field_id) =
                        self.const_scalar_or_composite_id(&field.value, &field.ty)?
                    else {
                        return Ok(None);
                    };
                    constituents.push(field_id);
                }
                return self
                    .const_composite_with_constituents(&ty, constituents)
                    .map(Some);
            }
            _ => return Ok(None),
        };
        let type_name = if is_array { "array" } else { "vector" };
        let lane_name = if is_array { "element" } else { "lane" };
        let lanes = match value {
            LlValue::Splat(lane) => {
                let lane_ty = self.resolve_type(&lane.ty)?;
                if lane_ty != **elem {
                    return Err(format!(
                        "native emitter: splat {lane_name} type {lane_ty:?} does not match {elem:?}"
                    ));
                }
                let Some(lane_id) = self.const_scalar_or_composite_id(&lane.value, &lane.ty)?
                else {
                    return Ok(None);
                };
                return self
                    .const_composite_with_constituents(&ty, (0..*count).map(|_| lane_id).collect())
                    .map(Some);
            }
            LlValue::Vector(lanes) if !is_array => lanes,
            LlValue::Array(lanes) if is_array => lanes,
            LlValue::Vector(_) => {
                return Err("native emitter: vector literal used as array type".into());
            }
            LlValue::Array(_) => {
                return Err("native emitter: array literal used as vector type".into());
            }
            LlValue::Struct(_) => {
                return Err("native emitter: struct literal used as vector/array type".into());
            }
            _ => return Ok(None),
        };
        if lanes.len() != *count as usize {
            return Err(format!(
                "native emitter: {type_name} literal has {} {lane_name}s, expected {count}",
                lanes.len()
            ));
        }
        let mut constituents = Vec::with_capacity(*count as usize);
        for lane in lanes {
            let lane_ty = self.resolve_type(&lane.ty)?;
            if lane_ty != **elem {
                return Err(format!(
                    "native emitter: {type_name} {lane_name} type {lane_ty:?} does not match {elem:?}"
                ));
            }
            let Some(lane_id) = self.const_scalar_or_composite_id(&lane.value, &lane.ty)? else {
                return Ok(None);
            };
            constituents.push(lane_id);
        }
        self.const_composite_with_constituents(&ty, constituents)
            .map(Some)
    }

    fn const_composite_with_constituents(
        &mut self,
        ty: &LlType,
        constituents: Vec<Word>,
    ) -> Result<Word, String> {
        let ty = self.resolve_type(ty)?;
        let key = (ty.clone(), constituents.clone());
        if let Some(id) = self.interner.composite_constants.get(&key) {
            return Ok(*id);
        }
        let result_type = self.type_id(&ty)?;
        let result = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::ConstantComposite,
            Some(result_type),
            Some(result),
            constituents.into_iter().map(Operand::IdRef).collect(),
        ));
        self.interner.composite_constants.insert(key, result);
        Ok(result)
    }

    fn const_scalar_or_composite_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
    ) -> Result<Option<Word>, String> {
        if let Some(id) = self.const_scalar_id(value, ty)? {
            return Ok(Some(id));
        }
        self.const_composite_id(value, ty)
    }

    pub(super) fn const_initializer_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
    ) -> Result<Word, String> {
        if let Some(id) = self.const_scalar_or_composite_id(value, ty)? {
            return Ok(id);
        }
        Err(format!(
            "native emitter: unsupported global initializer {value:?} for {ty:?}"
        ))
    }

    pub(super) fn const_scalar_id(
        &mut self,
        value: &LlValue,
        ty: &LlType,
    ) -> Result<Option<Word>, String> {
        match value {
            LlValue::Local(_)
            | LlValue::Global(_)
            | LlValue::Vector(_)
            | LlValue::Array(_)
            | LlValue::Struct(_)
            | LlValue::Splat(_) => Ok(None),
            LlValue::Undef => self.const_null(ty).map(Some),
            _ => self.value_id(value, ty).map(Some),
        }
    }

    pub(super) fn const_int(&mut self, bits: u32, value: u64) -> Result<Word, String> {
        let legal = spirv_int_width(bits)?;
        let encoded = if bits >= 64 {
            value
        } else {
            value & ((1u64 << bits) - 1)
        };
        if legal == 32 {
            return self.const_uint(encoded as u32);
        }
        if let Some(id) = self.interner.int_constants.get(&(legal, encoded)) {
            return Ok(*id);
        }
        let ty = self.type_id(&LlType::Int(legal))?;
        let id = self.fresh();
        let operands = if legal <= 32 {
            vec![Operand::LiteralBit32(encoded as u32)]
        } else if legal == 64 {
            vec![Operand::LiteralBit64(encoded)]
        } else {
            return Err(format!(
                "native emitter: unsupported integer constant width i{bits}"
            ));
        };
        self.module.types_global_values.push(Self::inst(
            Op::Constant,
            Some(ty),
            Some(id),
            operands,
        ));
        self.interner.int_constants.insert((legal, encoded), id);
        Ok(id)
    }

    pub(super) fn const_bool(&mut self, value: bool) -> Result<Word, String> {
        if let Some(id) = self.interner.bool_constants.get(&value) {
            return Ok(*id);
        }
        let ty = self.type_id(&LlType::Bool)?;
        let id = self.fresh();
        let op = if value {
            Op::ConstantTrue
        } else {
            Op::ConstantFalse
        };
        self.module
            .types_global_values
            .push(Self::inst(op, Some(ty), Some(id), vec![]));
        self.interner.bool_constants.insert(value, id);
        Ok(id)
    }

    pub(super) fn const_null(&mut self, ty: &LlType) -> Result<Word, String> {
        let ty = self.resolve_type(ty)?;
        if let Some(id) = self.interner.null_constants.get(&ty) {
            return Ok(*id);
        }
        let type_id = self.type_id(&ty)?;
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::ConstantNull,
            Some(type_id),
            Some(id),
            vec![],
        ));
        self.interner.null_constants.insert(ty, id);
        Ok(id)
    }

    pub(super) fn const_signed_int(&mut self, bits: u32, value: i64) -> Result<Word, String> {
        let encoded = if bits >= 64 {
            value as u64
        } else {
            (value as u64) & ((1u64 << bits) - 1)
        };
        self.const_int(bits, encoded)
    }

    pub(super) fn int_constant_like(&mut self, ty: &LlType, value: i64) -> Result<Word, String> {
        match ty {
            LlType::Int(bits) => self.const_signed_int(*bits, value),
            LlType::Vector(elem, lanes) => {
                let LlType::Int(bits) = elem.as_ref() else {
                    return Err(format!(
                        "native emitter: integer constant requested for non-int vector {ty:?}"
                    ));
                };
                let scalar = self.const_signed_int(*bits, value)?;
                let result_type = self.type_id(ty)?;
                let result = self.fresh();
                self.module.types_global_values.push(Self::inst(
                    Op::ConstantComposite,
                    Some(result_type),
                    Some(result),
                    (0..*lanes).map(|_| Operand::IdRef(scalar)).collect(),
                ));
                Ok(result)
            }
            other => Err(format!(
                "native emitter: integer constant requested for non-int type {other:?}"
            )),
        }
    }

    fn int_literal_bits_as_float(&mut self, value: u64, ty: &LlType) -> Result<Word, String> {
        match ty {
            LlType::Float => self.const_float32_bits(value as u32),
            LlType::Half => self.const_float16_bits(value as u16),
            LlType::BFloat => self.const_int(16, value & 0xffff),
            other => Err(format!(
                "native emitter: integer literal {value} used as non-int type {other:?}"
            )),
        }
    }

    pub(super) fn const_float32(&mut self, value: f32) -> Result<Word, String> {
        self.const_float32_bits(value.to_bits())
    }

    pub(super) fn const_float32_bits(&mut self, bits: u32) -> Result<Word, String> {
        if let Some(id) = self.interner.float32_constants.get(&bits) {
            return Ok(*id);
        }
        let ty = self.type_id(&LlType::Float)?;
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Constant,
            Some(ty),
            Some(id),
            vec![Operand::LiteralBit32(bits)],
        ));
        self.interner.float32_constants.insert(bits, id);
        Ok(id)
    }

    pub(super) fn const_float16_bits(&mut self, bits: u16) -> Result<Word, String> {
        if let Some(id) = self.interner.float16_constants.get(&bits) {
            return Ok(*id);
        }
        let ty = self.type_id(&LlType::Half)?;
        let id = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Constant,
            Some(ty),
            Some(id),
            vec![Operand::LiteralBit32(bits as u32)],
        ));
        self.interner.float16_constants.insert(bits, id);
        Ok(id)
    }
}

#[cfg(test)]
mod float_literal_tests {
    use super::f32_to_f16_bits;

    #[test]
    fn decimal_half_conversion_rounds_to_nearest_ties_even() {
        assert_eq!(f32_to_f16_bits(1.0), 0x3c00);
        assert_eq!(f32_to_f16_bits(f32::from_bits(0x3f80_1000)), 0x3c00);
        assert_eq!(f32_to_f16_bits(f32::from_bits(0x3f80_1001)), 0x3c01);
        assert_eq!(f32_to_f16_bits(2.0_f32.powi(-24)), 0x0001);
        assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
    }
}
