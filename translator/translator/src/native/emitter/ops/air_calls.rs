use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_void_air_call(
        &mut self,
        call: &LlCall,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        match call.callee.as_str() {
            "air.wg.barrier" | "air.simdgroup.barrier" => {
                if call.args.len() != 2 && call.args.len() != 3 {
                    return Err(format!(
                        "native emitter: {} expects 2 or 3 operands, got {}",
                        call.callee,
                        call.args.len()
                    ));
                }
                // The callee names the execution scope; the last operand is Metal's thread_scope argument, which
                // is the MEMORY scope (simdgroup_barrier(flags, thread_scope_threadgroup) emits
                // air.simdgroup.barrier(flags, 1)). Reading that operand as the execution scope turned simdgroup
                // barriers into workgroup barriers (BAR.SYNC) and threadgroup barriers with a simdgroup memory scope
                // into subgroup-only barriers.
                let execution = air_barrier_execution_scope(call);
                let scope = self.const_uint(execution as u32)?;
                let memory_scope =
                    self.const_uint(air_barrier_memory_scope(call, air_barrier_operand_scope(call)?) as u32)?;
                let semantics = self.const_uint(air_barrier_memory_semantics(call).bits())?;
                instructions.push(Self::inst(
                    Op::ControlBarrier,
                    None,
                    None,
                    vec![
                        Operand::IdScope(scope),
                        Operand::IdScope(memory_scope),
                        Operand::IdMemorySemantics(semantics),
                    ],
                ));
                Ok(true)
            }
            "air.atomic.fence" => {
                if call.args.len() != 3 {
                    return Err("native emitter: air.atomic.fence expects 3 operands".to_string());
                }
                let scope_kind = air_fence_memory_scope(call);
                let scope = self.const_uint(scope_kind as u32)?;
                let semantics = self.const_uint(air_barrier_memory_semantics(call).bits())?;
                instructions.push(Self::inst(
                    Op::MemoryBarrier,
                    None,
                    None,
                    vec![
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                    ],
                ));
                Ok(true)
            }
            callee if callee.starts_with("air.fence_texture") => {
                if call.args.len() != 1 {
                    return Err(format!("native emitter: {} expects 1 operand", call.callee));
                }
                let scope = self.const_uint(Scope::Device as u32)?;
                let semantics = self.const_uint(
                    (MemorySemantics::ACQUIRE_RELEASE | MemorySemantics::IMAGE_MEMORY).bits(),
                )?;
                instructions.push(Self::inst(
                    Op::MemoryBarrier,
                    None,
                    None,
                    vec![
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                    ],
                ));
                Ok(true)
            }
            callee if is_coherent_air_store(callee) => {
                if call.args.len() != 2 {
                    return Err(format!(
                        "native emitter: {} expects 2 operands",
                        call.callee
                    ));
                }
                let value =
                    self.value_id_in(&call.args[0].value, &call.args[0].ty, instructions)?;
                let ptr_arg = &call.args[1];
                if let LlValue::Local(name) = &ptr_arg.value {
                    if let Some(raw) = self.raw_offsets.get(name).cloned() {
                        self.emit_raw_store(&call.args[0].ty, value, &raw, None, instructions)?;
                        return Ok(true);
                    }
                    if self.unmodeled_pointers.contains(name) {
                        return Ok(true);
                    }
                }
                let ptr = self.value_id_in(&ptr_arg.value, &ptr_arg.ty, instructions)?;
                instructions.push(Self::inst(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(ptr), Operand::IdRef(value)],
                ));
                Ok(true)
            }
            "air.atomic.local.store.i32" | "air.atomic.global.store.i32" => {
                if !Self::atomic_arity_ok(call, 5) {
                    return Err(format!(
                        "native emitter: {} expects 5 operands (or 6 from the newer toolchain)",
                        call.callee
                    ));
                }
                let ptr = self.atomic_i32_pointer_id(&call.args[0], instructions)?;
                let value =
                    self.value_id_in(&call.args[1].value, &call.args[1].ty, instructions)?;
                let scope_kind = self.atomic_i32_scope_for_arg(&call.args[0])?;
                let scope = self.const_uint(scope_kind as u32)?;
                let semantics_kind =
                    Self::atomic_i32_memory_semantics(scope_kind, MemorySemantics::RELEASE);
                let semantics = self.const_uint(semantics_kind.bits())?;
                instructions.push(Self::inst(
                    Op::AtomicStore,
                    None,
                    None,
                    vec![
                        Operand::IdRef(ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                        Operand::IdRef(value),
                    ],
                ));
                Ok(true)
            }
            _ if crate::air_intrinsics::integer_atomic_rmw(&call.callee).is_some() => {
                self.emit_integer_atomic_rmw(call, None, instructions)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(in crate::native::emitter) fn emit_value_air_call(
        &mut self,
        call: &LlCall,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        match call.callee.as_str() {
            "llvm.agx2.cluster.num" => {
                if !call.args.is_empty() {
                    return Err(format!(
                        "native emitter: {} expects no operands",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Int(32) {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}, expected i32",
                        call.callee
                    ));
                }
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let zero = self.const_uint(0)?;
                instructions.push(Self::inst(
                    Op::CopyObject,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(zero)],
                ));
                Ok(true)
            }
            callee if callee.starts_with("air.is_null_texture") => {
                if call.args.len() == 1 {
                    if let LlValue::Local(arg_name) = &call.args[0].value {
                        if self.null_texture_values.contains(arg_name) {
                            let result_ty = self.resolve_type(&call.ret)?;
                            let result_type = self.type_id(&result_ty)?;
                            let result = self.result_id(name, &result_ty)?;
                            let c = self.const_bool(true)?;
                            instructions.push(Self::inst(
                                Op::CopyObject,
                                Some(result_type),
                                Some(result),
                                vec![Operand::IdRef(c)],
                            ));
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            }
            "air.get_null_intersection_function_table" => {
                if !call.args.is_empty() {
                    return Err(format!(
                        "native emitter: {} expects no operands",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                let LlType::Ptr(addrspace) = result_ty else {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}, expected pointer",
                        call.callee
                    ));
                };
                self.define_unmodeled_byte_pointer_value(name, addrspace)?;
                let is_null = self.const_bool(true)?;
                self.record_pointer_nullness(name.to_string(), is_null);
                Ok(true)
            }
            "air.get_instance_count_instance_acceleration_structure" => {
                if call.args.len() != 1 {
                    return Err(format!("native emitter: {} expects 1 operand", call.callee));
                }
                let LlValue::Local(shadow_name) = &call.args[0].value else {
                    return Err(format!(
                        "native emitter: {} shadow operand is not SSA",
                        call.callee
                    ));
                };
                let Some(mut raw) = self.raw_offsets.get(shadow_name).cloned() else {
                    return Ok(false);
                };
                raw.const_off += crate::as_shadow::INSTANCE_COUNT_BYTE_OFFSET as i64;
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Int(32) {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}, expected i32",
                        call.callee
                    ));
                }
                let result = self.result_id(name, &result_ty)?;
                self.emit_raw_load(result, &result_ty, &raw, Some(4), instructions)?;
                Ok(true)
            }
            "air.get_primitive_acceleration_structure_instance_acceleration_structure" => {
                if call.args.len() != 2 {
                    return Err(format!(
                        "native emitter: {} expects 2 operands",
                        call.callee
                    ));
                }
                let LlValue::Local(shadow_name) = &call.args[0].value else {
                    return Err(format!(
                        "native emitter: {} shadow operand is not SSA",
                        call.callee
                    ));
                };
                let Some(mut raw) = self.raw_offsets.get(shadow_name).cloned() else {
                    return Ok(false);
                };
                let result_ty = self.resolve_type(&call.ret)?;
                let LlType::Ptr(addrspace) = result_ty else {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}, expected pointer",
                        call.callee
                    ));
                };
                raw.const_off += crate::as_shadow::CHILD_REFERENCES_BYTE_OFFSET as i64;
                raw.dyn_terms.push((
                    call.args[1].clone(),
                    crate::as_shadow::CHILD_REFERENCE_BYTE_STRIDE as i64,
                ));
                self.define_unmodeled_byte_pointer_value(name, addrspace)?;
                let (payload, is_null) =
                    self.emit_raw_pointer_payload(&raw, 0, Some(8), instructions)?;
                self.pointer_payload_words.insert(name.to_string(), payload);
                self.record_pointer_nullness(name.to_string(), is_null);
                Ok(true)
            }
            "air.get_data_pointer_instance_acceleration_structure" => {
                if self.bda_device_pointers && call.args.len() == 1 {
                    if let LlValue::Local(arg_name) = &call.args[0].value {
                        if let Some(raw) = self.raw_offsets.get(arg_name).cloned() {
                            if raw.device_addr_base.is_some() {
                                self.used_device_address = true;
                                self.raw_offsets.insert(name.to_string(), raw);
                                self.pointer_storage
                                    .insert(name.to_string(), StorageClass::PhysicalStorageBuffer);
                                return Ok(true);
                            }
                        }
                    }
                }

                Ok(false)
            }
            callee if is_coherent_air_load(callee) => {
                if call.args.len() != 1 {
                    return Err(format!("native emitter: {} expects 1 operand", call.callee));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let ptr_arg = &call.args[0];
                if let LlValue::Local(ptr_name) = &ptr_arg.value {
                    if let Some(raw) = self.raw_offsets.get(ptr_name).cloned() {
                        self.emit_raw_load(result, &result_ty, &raw, None, instructions)?;
                        return Ok(true);
                    }
                    if self.unmodeled_pointers.contains(ptr_name) {
                        let zero = self.const_null(&result_ty)?;
                        instructions.push(Self::inst(
                            Op::CopyObject,
                            Some(result_type),
                            Some(result),
                            vec![Operand::IdRef(zero)],
                        ));
                        return Ok(true);
                    }
                }
                let ptr = self.value_id_in(&ptr_arg.value, &ptr_arg.ty, instructions)?;
                instructions.push(Self::inst(
                    Op::Load,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(ptr)],
                ));
                Ok(true)
            }
            "air.atomic.local.load.i32" | "air.atomic.global.load.i32" => {
                if !Self::atomic_arity_ok(call, 4) {
                    return Err(format!(
                        "native emitter: {} expects 4 operands (or 5 from the newer toolchain)",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Int(32) {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}",
                        call.callee
                    ));
                }
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let ptr = self.atomic_i32_pointer_id(&call.args[0], instructions)?;
                let scope_kind = self.atomic_i32_scope_for_arg(&call.args[0])?;
                let scope = self.const_uint(scope_kind as u32)?;
                let semantics_kind =
                    Self::atomic_i32_memory_semantics(scope_kind, MemorySemantics::ACQUIRE);
                let semantics = self.const_uint(semantics_kind.bits())?;
                instructions.push(Self::inst(
                    Op::AtomicLoad,
                    Some(result_type),
                    Some(result),
                    vec![
                        Operand::IdRef(ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                    ],
                ));
                Ok(true)
            }
            "air.atomic.global.add.f32" => {
                if !Self::atomic_arity_ok(call, 5) {
                    return Err(format!(
                        "native emitter: {} expects 5 operands (or 6 from the newer toolchain)",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Float {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}",
                        call.callee
                    ));
                }
                self.require_capability(Capability::AtomicFloat32AddEXT);
                self.require_extension("SPV_EXT_shader_atomic_float_add");
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let ptr = self.atomic_f32_pointer_id(&call.args[0], instructions)?;
                let value =
                    self.value_id_in(&call.args[1].value, &call.args[1].ty, instructions)?;
                let scope = self.const_uint(Scope::Device as u32)?;
                let semantics = self.const_uint(MemorySemantics::RELAXED.bits())?;
                instructions.push(Self::inst(
                    Op::AtomicFAddEXT,
                    Some(result_type),
                    Some(result),
                    vec![
                        Operand::IdRef(ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                        Operand::IdRef(value),
                    ],
                ));
                Ok(true)
            }
            "air.atomic.global.sub.f32" => {
                if !Self::atomic_arity_ok(call, 5) {
                    return Err(format!(
                        "native emitter: {} expects 5 operands (or 6 from the newer toolchain)",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Float {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}",
                        call.callee
                    ));
                }
                self.require_capability(Capability::AtomicFloat32AddEXT);
                self.require_extension("SPV_EXT_shader_atomic_float_add");
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let ptr = self.atomic_f32_pointer_id(&call.args[0], instructions)?;
                let value =
                    self.value_id_in(&call.args[1].value, &call.args[1].ty, instructions)?;
                let negated = self.fresh();
                instructions.push(Self::inst(
                    Op::FNegate,
                    Some(result_type),
                    Some(negated),
                    vec![Operand::IdRef(value)],
                ));
                let scope = self.const_uint(Scope::Device as u32)?;
                let semantics = self.const_uint(MemorySemantics::RELAXED.bits())?;
                instructions.push(Self::inst(
                    Op::AtomicFAddEXT,
                    Some(result_type),
                    Some(result),
                    vec![
                        Operand::IdRef(ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(semantics),
                        Operand::IdRef(negated),
                    ],
                ));
                Ok(true)
            }
            "air.atomic.local.cmpxchg.weak.i32" | "air.atomic.global.cmpxchg.weak.i32" => {
                if !Self::atomic_arity_ok(call, 7) {
                    return Err(format!(
                        "native emitter: {} expects 7 operands (or 8 from the newer toolchain)",
                        call.callee
                    ));
                }
                let result_ty = self.resolve_type(&call.ret)?;
                if result_ty != LlType::Int(32) {
                    return Err(format!(
                        "native emitter: {} returned {result_ty:?}",
                        call.callee
                    ));
                }
                let compare_ptr_ty = self.resolve_type(&call.args[1].ty)?;
                if !matches!(compare_ptr_ty, LlType::Ptr(_)) {
                    return Err(format!(
                        "native emitter: {} compare operand is {compare_ptr_ty:?}",
                        call.callee
                    ));
                }
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(name, &result_ty)?;
                let ptr = self.atomic_i32_pointer_id(&call.args[0], instructions)?;
                let compare_ptr = self.value_id(&call.args[1].value, &call.args[1].ty)?;
                let compare = self.fresh();
                instructions.push(Self::inst(
                    Op::Load,
                    Some(result_type),
                    Some(compare),
                    vec![Operand::IdRef(compare_ptr)],
                ));
                let value =
                    self.value_id_in(&call.args[2].value, &call.args[2].ty, instructions)?;
                let scope_kind = self.atomic_i32_scope_for_arg(&call.args[0])?;
                let scope = self.const_uint(scope_kind as u32)?;
                let success_semantics_kind =
                    Self::atomic_i32_memory_semantics(scope_kind, MemorySemantics::ACQUIRE_RELEASE);
                let failure_semantics_kind =
                    Self::atomic_i32_memory_semantics(scope_kind, MemorySemantics::ACQUIRE);
                let success_semantics = self.const_uint(success_semantics_kind.bits())?;
                let failure_semantics = self.const_uint(failure_semantics_kind.bits())?;
                instructions.push(Self::inst(
                    Op::AtomicCompareExchange,
                    Some(result_type),
                    Some(result),
                    vec![
                        Operand::IdRef(ptr),
                        Operand::IdScope(scope),
                        Operand::IdMemorySemantics(success_semantics),
                        Operand::IdMemorySemantics(failure_semantics),
                        Operand::IdRef(value),
                        Operand::IdRef(compare),
                    ],
                ));
                instructions.push(Self::inst(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(compare_ptr), Operand::IdRef(result)],
                ));
                Ok(true)
            }
            _ if crate::air_intrinsics::integer_atomic_rmw(&call.callee).is_some() => {
                self.emit_integer_atomic_rmw(call, Some(name), instructions)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    /// Atomic intrinsics take `base` operands in AIR up to 2.8. The AIR 2.9 toolchain inserts one more i32 ahead of the
    /// trailing volatile flag; everything the lowering reads (pointer, values) keeps its position, so both lengths lower
    /// the same way as long as the extra operand really is an i32.
    fn atomic_arity_ok(call: &LlCall, base: usize) -> bool {
        match call.args.len() {
            n if n == base => true,
            n if n == base + 1 => {
                matches!(call.args[n - 2].ty, LlType::Int(32)) && matches!(call.args[n - 1].ty, LlType::Int(1))
            }
            _ => false,
        }
    }

    fn emit_integer_atomic_rmw(
        &mut self,
        call: &LlCall,
        name: Option<&str>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let (op, width) = crate::air_intrinsics::integer_atomic_rmw(&call.callee)
            .ok_or("native emitter: missing integer atomic ABI")?;
        if !Self::atomic_arity_ok(call, 5) {
            return Err(format!(
                "native emitter: {} expects 5 operands (or 6 from the newer toolchain)",
                call.callee
            ));
        }
        let result_ty = LlType::Int(width);
        if name.is_some() && self.resolve_type(&call.ret)? != result_ty {
            return Err(format!(
                "native emitter: {} result differs from ABI width",
                call.callee
            ));
        }
        let result_type = self.type_id(&result_ty)?;
        let result = match name {
            Some(name) => self.result_id(name, &result_ty)?,
            None => self.fresh(),
        };
        let ptr = self.atomic_integer_pointer_id(&call.args[0], width, instructions)?;
        let value = self.value_id_in(&call.args[1].value, &call.args[1].ty, instructions)?;
        if self.resolve_type(&call.args[1].ty)? != result_ty {
            return Err(format!(
                "native emitter: {} value width differs from result",
                call.callee
            ));
        }
        if width == 64 {
            self.require_capability(Capability::Int64Atomics);
        }
        let scope_kind = self.atomic_i32_scope_for_arg(&call.args[0])?;
        let scope = self.const_uint(scope_kind as u32)?;
        let semantics_kind =
            Self::atomic_i32_memory_semantics(scope_kind, MemorySemantics::ACQUIRE_RELEASE);
        let semantics = self.const_uint(semantics_kind.bits())?;
        instructions.push(Self::inst(
            op,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(ptr),
                Operand::IdScope(scope),
                Operand::IdMemorySemantics(semantics),
                Operand::IdRef(value),
            ],
        ));
        Ok(())
    }
}

fn air_barrier_memory_semantics(call: &LlCall) -> MemorySemantics {
    let flags = call
        .args
        .first()
        .and_then(|arg| air_i32_literal(&arg.value))
        .unwrap_or(0);
    let mut semantics = MemorySemantics::ACQUIRE_RELEASE;
    if flags & 1 != 0 {
        semantics |= MemorySemantics::UNIFORM_MEMORY | MemorySemantics::CROSS_WORKGROUP_MEMORY;
    }
    if flags & (2 | 8) != 0 {
        semantics |= MemorySemantics::WORKGROUP_MEMORY;
    }
    if flags & 4 != 0 {
        semantics |= MemorySemantics::IMAGE_MEMORY;
    }
    if semantics == MemorySemantics::ACQUIRE_RELEASE {
        semantics |= MemorySemantics::WORKGROUP_MEMORY;
    }
    semantics
}

fn air_barrier_execution_scope(call: &LlCall) -> Scope {
    if call.callee == "air.simdgroup.barrier" {
        Scope::Subgroup
    } else {
        Scope::Workgroup
    }
}

// Metal thread_scope values as AIR spells them: 1 threadgroup, 2 device, 4 simdgroup (0, thread, has no barrier meaning).
fn air_barrier_operand_scope(call: &LlCall) -> Result<Scope, String> {
    let scope_index = call.args.len().saturating_sub(1);
    match call
        .args
        .get(scope_index)
        .and_then(|arg| air_i32_literal(&arg.value))
    {
        Some(1) => Ok(Scope::Workgroup),
        Some(2) => Ok(Scope::Device),
        Some(4) => Ok(Scope::Subgroup),
        Some(other) => Err(format!(
            "native emitter: {} states memory scope {other}, which is none of AIR's threadgroup (1), \
             device (2) or simdgroup (4)",
            call.callee
        )),
        None => Err(format!(
            "native emitter: {} has no constant memory scope operand",
            call.callee
        )),
    }
}

fn air_fence_memory_scope(call: &LlCall) -> Scope {
    match call.args.get(2).and_then(|arg| air_i32_literal(&arg.value)) {
        Some(0) => Scope::Invocation,
        Some(1) => Scope::Workgroup,
        Some(4) => Scope::Subgroup,
        _ => Scope::Device,
    }
}

fn air_barrier_memory_scope(call: &LlCall, default_scope: Scope) -> Scope {
    let flags = call
        .args
        .first()
        .and_then(|arg| air_i32_literal(&arg.value))
        .unwrap_or(0);
    if flags & (1 | 4) != 0 {
        Scope::Device
    } else {
        default_scope
    }
}
