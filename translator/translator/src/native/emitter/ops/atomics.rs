use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn atomic_i32_scope_for_arg(
        &self,
        ptr_arg: &TypedValue,
    ) -> Result<Scope, String> {
        let storage = if let LlValue::Local(name) = &ptr_arg.value {
            if let Some(raw) = self.raw_offsets.get(name) {
                self.raw_access_storage(raw)?
            } else if self.unmodeled_pointers.contains(name) {
                StorageClass::Workgroup
            } else {
                let LlType::Ptr(addrspace) = self.resolve_type(&ptr_arg.ty)? else {
                    return Err(format!(
                        "native emitter: atomic i32 pointer argument has type {:?}",
                        ptr_arg.ty
                    ));
                };
                self.pointer_storage_for(&ptr_arg.value, addrspace)?
            }
        } else {
            let LlType::Ptr(addrspace) = self.resolve_type(&ptr_arg.ty)? else {
                return Err(format!(
                    "native emitter: atomic i32 pointer argument has type {:?}",
                    ptr_arg.ty
                ));
            };
            self.pointer_storage_for(&ptr_arg.value, addrspace)?
        };
        Ok(if storage == StorageClass::Workgroup {
            Scope::Workgroup
        } else {
            Scope::Device
        })
    }

    pub(in crate::native::emitter) fn atomic_i32_memory_semantics(
        scope: Scope,
        workgroup_semantics: MemorySemantics,
    ) -> MemorySemantics {
        if scope == Scope::Workgroup {
            workgroup_semantics | MemorySemantics::WORKGROUP_MEMORY
        } else {
            MemorySemantics::RELAXED
        }
    }

    pub(in crate::native::emitter) fn atomic_i32_pointer_id(
        &mut self,
        ptr_arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        self.atomic_integer_pointer_id(ptr_arg, 32, instructions)
    }

    pub(in crate::native::emitter) fn atomic_integer_pointer_id(
        &mut self,
        ptr_arg: &TypedValue,
        width: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if width != 32 && width != 64 {
            return Err(format!(
                "native emitter: unsupported atomic integer width {width}"
            ));
        }
        if let LlValue::IntToPtr {
            source,
            destination,
        } = &ptr_arg.value
        {
            if self.resolve_type(destination)? != LlType::Ptr(3) {
                return Err(format!(
                    "native emitter: atomic inttoptr constant expression targets {destination:?}, expected Workgroup pointer"
                ));
            }
            let address = match source.value {
                LlValue::Int(value) | LlValue::Hex(value) => value,
                LlValue::SignedInt(value) if value >= 0 => value as u64,
                _ => {
                    return Err(
                        "native emitter: Workgroup atomic inttoptr address is not a nonnegative integer literal"
                            .into(),
                    )
                }
            };
            return self.workgroup_integer_pointer_for_address(address, width);
        }
        if let LlValue::Local(name) = &ptr_arg.value {
            if let Some(raw) = self.raw_offsets.get(name).cloned() {
                if width == 64 && !self.raw_offset_aligned_to(&raw, 0, 8) {
                    return Err(
                        "native emitter: 64-bit atomic raw offset is not 8-byte aligned".into(),
                    );
                }
                let pointer = self.emit_raw_word_pointer_for_access(
                    &raw,
                    0,
                    Some(u64::from(width / 8)),
                    instructions,
                )?;
                if width == 32 {
                    return Ok(pointer);
                }
                let storage = self.raw_access_storage(&raw)?;
                if storage != StorageClass::PhysicalStorageBuffer {
                    return Err("native emitter: 64-bit atomic requires a typed i64 pointee or physical buffer addressing".into());
                }
                let ptr_type = self.ptr_type_id(storage, &LlType::Int(width))?;
                let result = self.fresh();
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(ptr_type),
                    Some(result),
                    vec![Operand::IdRef(pointer)],
                ));
                return Ok(result);
            }
        }
        if matches!(&ptr_arg.value, LlValue::Local(name) if self.unmodeled_pointers.contains(name))
        {
            if width == 32 {
                return self.unmodeled_atomic_i32_pointer();
            }
            return Err("native emitter: unmodeled 64-bit atomic pointer".into());
        }
        let ptr = self.value_id_in(&ptr_arg.value, &ptr_arg.ty, instructions)?;
        let Some(pointee) = self.pointer_pointee_for_value(&ptr_arg.value)? else {
            if width == 32 {
                return Ok(ptr);
            }
            return Err("native emitter: 64-bit atomic pointer has no modeled pointee".into());
        };
        let pointee = self.resolve_type(&pointee)?;
        if pointee == LlType::Int(width) {
            return Ok(ptr);
        }
        if width == 32 && pointee == LlType::Float {
            return self.bitcast_pointer_to_atomic_i32(ptr_arg, ptr, instructions);
        }

        let LlType::Struct(fields) = pointee else {
            return Err(format!(
                "native emitter: atomic i{width} pointer targets {pointee:?}"
            ));
        };
        let mut field = LlType::Struct(fields);
        let mut depth = 0usize;
        loop {
            field = self.resolve_type(&field)?;
            match field {
                LlType::Int(bits) if bits == width => break,
                LlType::Struct(ref fields) if !fields.is_empty() => {
                    field = fields[0].clone();
                    depth += 1;
                }
                LlType::Array(ref element, count) if count > 0 => {
                    field = element.as_ref().clone();
                    depth += 1;
                }
                _ => {
                    return Err(format!(
                        "native emitter: atomic i{width} pointer first-field chain targets {field:?}"
                    ));
                }
            }
        }
        let LlType::Ptr(addrspace) = self.resolve_type(&ptr_arg.ty)? else {
            return Err(format!(
                "native emitter: atomic i{width} pointer argument has type {:?}",
                ptr_arg.ty
            ));
        };
        let ptr_type = self.ptr_type_id(
            self.pointer_storage_for(&ptr_arg.value, addrspace)?,
            &LlType::Int(width),
        )?;
        let zero = self.const_uint(0)?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_type),
            Some(result),
            std::iter::once(Operand::IdRef(ptr))
                .chain(std::iter::repeat_n(Operand::IdRef(zero), depth))
                .collect(),
        ));
        Ok(result)
    }

    pub(in crate::native::emitter) fn bitcast_pointer_to_atomic_i32(
        &mut self,
        ptr_arg: &TypedValue,
        ptr: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let LlType::Ptr(addrspace) = self.resolve_type(&ptr_arg.ty)? else {
            return Err(format!(
                "native emitter: atomic i32 pointer argument has type {:?}",
                ptr_arg.ty
            ));
        };
        let storage = self.pointer_storage_for(&ptr_arg.value, addrspace)?;
        let ptr_type = self.ptr_type_id(storage, &LlType::Int(32))?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::Bitcast,
            Some(ptr_type),
            Some(result),
            vec![Operand::IdRef(ptr)],
        ));
        Ok(result)
    }

    pub(in crate::native::emitter) fn atomic_f32_pointer_id(
        &mut self,
        ptr_arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if matches!(&ptr_arg.value, LlValue::Local(name) if self.unmodeled_pointers.contains(name))
        {
            return self.unmodeled_atomic_f32_pointer();
        }
        let ptr = self.value_id_in(&ptr_arg.value, &ptr_arg.ty, instructions)?;
        let Some(pointee) = self.pointer_pointee_for_value(&ptr_arg.value)? else {
            return Ok(ptr);
        };
        let pointee = self.resolve_type(&pointee)?;
        if pointee == LlType::Float {
            return Ok(ptr);
        }

        let LlType::Struct(fields) = pointee else {
            return Err(format!(
                "native emitter: atomic f32 pointer targets {pointee:?}"
            ));
        };
        let [field] = fields.as_slice() else {
            return Err(format!(
                "native emitter: atomic f32 pointer targets struct with {} fields",
                fields.len()
            ));
        };
        let field = self.resolve_type(field)?;
        if field != LlType::Float {
            return Err(format!(
                "native emitter: atomic f32 pointer targets struct field {field:?}"
            ));
        }
        let LlType::Ptr(addrspace) = self.resolve_type(&ptr_arg.ty)? else {
            return Err(format!(
                "native emitter: atomic f32 pointer argument has type {:?}",
                ptr_arg.ty
            ));
        };
        let ptr_type = self.ptr_type_id(llvm_pointer_storage(addrspace)?, &LlType::Float)?;
        let zero = self.const_uint(0)?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_type),
            Some(result),
            vec![Operand::IdRef(ptr), Operand::IdRef(zero)],
        ));
        Ok(result)
    }

    pub(in crate::native::emitter) fn unmodeled_atomic_i32_pointer(
        &mut self,
    ) -> Result<Word, String> {
        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, &LlType::Int(32))?;
        let result = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Variable,
            Some(ptr_type),
            Some(result),
            vec![Operand::StorageClass(StorageClass::Workgroup)],
        ));
        Ok(result)
    }

    fn workgroup_integer_pointer_for_address(
        &mut self,
        address: u64,
        width: u32,
    ) -> Result<Word, String> {
        if !address.is_multiple_of(u64::from(width / 8)) {
            return Err("native emitter: misaligned Workgroup atomic integer address".into());
        }
        let end = address
            .checked_add(u64::from(width / 8))
            .ok_or("native emitter: Workgroup atomic integer address overflows")?;
        if self
            .workgroup_i32_addresses
            .keys()
            .any(|(other_address, other_width)| {
                (*other_address, *other_width) != (address, width)
                    && *other_address < end
                    && other_address.saturating_add(u64::from(*other_width / 8)) > address
            })
        {
            return Err("native emitter: overlapping Workgroup atomic integer addresses".into());
        }
        if let Some(pointer) = self.workgroup_i32_addresses.get(&(address, width)) {
            return Ok(*pointer);
        }
        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, &LlType::Int(width))?;
        let pointer = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Variable,
            Some(ptr_type),
            Some(pointer),
            vec![Operand::StorageClass(StorageClass::Workgroup)],
        ));
        self.workgroup_i32_addresses
            .insert((address, width), pointer);
        Ok(pointer)
    }

    pub(in crate::native::emitter) fn unmodeled_atomic_f32_pointer(
        &mut self,
    ) -> Result<Word, String> {
        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, &LlType::Float)?;
        let result = self.fresh();
        self.module.types_global_values.push(Self::inst(
            Op::Variable,
            Some(ptr_type),
            Some(result),
            vec![Operand::StorageClass(StorageClass::Workgroup)],
        ));
        Ok(result)
    }
}

/// Which half of an atomic's ordering a SPIR-V instruction can carry: OpAtomicLoad takes no release bit and
/// OpAtomicStore no acquire bit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::native::emitter) enum AtomicRole {
    Load,
    Store,
    ReadModifyWrite,
}

/// Scope operand of an AIR atomic (thread 0, threadgroup 1, device 2, simdgroup 4) as the SPIR-V scope, or the one
/// implied by the pointer's address space when the operand is not one of those constants.
pub(in crate::native::emitter) fn atomic_requested_scope(
    call: &LlCall,
    scope_index: usize,
    pointer_scope: Scope,
) -> Scope {
    match call.args.get(scope_index).and_then(|a| super::air_i32_literal(&a.value)) {
        Some(0) => Scope::Invocation,
        Some(1) => Scope::Workgroup,
        Some(2) => Scope::Device,
        Some(4) => Scope::Subgroup,
        _ => pointer_scope,
    }
}

/// Memory semantics for one AIR atomic: the semantics the emitter always used (`base`) with the ordering the shader
/// asked for added on top, never removed. The order operand carries memory_order in its low byte and, for AIR 2.9
/// shaders, the mem_flags operand shifted left by 8 (`air_atomic_v29` folds it in). memory_order values are
/// relaxed 0, acquire 2, release 3, acq_rel 4, seq_cst 5. Vulkan forbids sequentially consistent semantics without the
/// Vulkan memory model, so seq_cst becomes acquire-release, which is no weaker for the operations a shader can see.
/// mem_flags device 1, threadgroup 2, texture 4 select the memory classes the ordering applies to; with none given the
/// class of the atomic's own pointer is used.
pub(in crate::native::emitter) fn atomic_requested_semantics(
    call: &LlCall,
    order_index: usize,
    pointer_scope: Scope,
    base: MemorySemantics,
    role: AtomicRole,
) -> MemorySemantics {
    let encoded = call
        .args
        .get(order_index)
        .and_then(|a| super::air_i32_literal(&a.value))
        .unwrap_or(0);
    let (order, flags) = ((encoded & 0xff) as u32, (encoded >> 8) as u32);
    let (acquire, release) = match order {
        2 => (true, false),
        3 => (false, true),
        4 | 5 => (true, true),
        _ => (false, false),
    };
    let acquire = acquire && role != AtomicRole::Store;
    let release = release && role != AtomicRole::Load;
    if !acquire && !release {
        return base;
    }
    let mut semantics = base;
    semantics |= match (acquire, release) {
        (true, true) => MemorySemantics::ACQUIRE_RELEASE,
        (true, false) => MemorySemantics::ACQUIRE,
        _ => MemorySemantics::RELEASE,
    };
    if flags & 1 != 0 {
        semantics |= MemorySemantics::UNIFORM_MEMORY;
    }
    if flags & 2 != 0 {
        semantics |= MemorySemantics::WORKGROUP_MEMORY;
    }
    if flags & 4 != 0 {
        semantics |= MemorySemantics::IMAGE_MEMORY;
    }
    if flags == 0 {
        semantics |= if pointer_scope == Scope::Workgroup {
            MemorySemantics::WORKGROUP_MEMORY
        } else {
            MemorySemantics::UNIFORM_MEMORY
        };
    }
    semantics
}
