use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_mtl_force_not_checked_load_call(
        &mut self,
        call: &LlCall,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if call.callee != "mtl.force_not_checked.load.i64.p1" {
            return Ok(false);
        }
        if call.args.len() != 1 {
            return Err(format!(
                "native emitter: {} expected one pointer argument, got {}",
                call.callee,
                call.args.len()
            ));
        }
        let result_ty = self.resolve_type(&call.ret)?;
        if result_ty != LlType::Int(64) {
            return Err(format!(
                "native emitter: {} returned unsupported type {:?}",
                call.callee, result_ty
            ));
        }
        let arg = &call.args[0];
        let LlType::Ptr(addrspace) = self.resolve_type(&arg.ty)? else {
            return Err(format!(
                "native emitter: {} argument is not a pointer: {:?}",
                call.callee, arg.ty
            ));
        };
        if addrspace != 1 {
            return Err(format!(
                "native emitter: {} expected ptr addrspace(1), got addrspace({addrspace})",
                call.callee
            ));
        }
        let LlValue::Local(arg_name) = &arg.value else {
            return Err(format!(
                "native emitter: {} requires a local device-address pointer",
                call.callee
            ));
        };
        let Some(raw) = self.raw_offsets.get(arg_name).cloned() else {
            return Err(format!(
                "native emitter: {} requires BDA device-address lowering",
                call.callee
            ));
        };
        if raw.device_addr_base.is_none() {
            return Err(format!(
                "native emitter: {} pointer is not backed by a device address",
                call.callee
            ));
        }
        let result = self.result_id(name, &result_ty)?;
        self.emit_device_addr_load(result, &result_ty, &raw, instructions)?;
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_visible_function_table_placeholder_call(
        &mut self,
        call: &LlCall,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if call.callee == "air.get_size_visible_function_table" {
            self.validate_call_args(call, instructions)?;
            let result_ty = self.resolve_type(&call.ret)?;
            if !matches!(result_ty, LlType::Int(_)) {
                return Err(format!(
                    "native emitter: visible function table size returned {result_ty:?}"
                ));
            }
            let zero = self.const_null(&result_ty)?;
            let result_type = self.type_id(&result_ty)?;
            let result = self.result_id(name, &result_ty)?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(zero)],
            ));
            return Ok(true);
        }
        if call.callee == "air.get_function_pointer_visible_function_table" {
            self.validate_call_args(call, instructions)?;
            let result_ty = self.resolve_type(&call.ret)?;
            let LlType::Ptr(addrspace) = result_ty else {
                return Err(format!(
                    "native emitter: visible function table pointer returned {result_ty:?}"
                ));
            };
            self.define_unmodeled_byte_pointer_value(name, addrspace)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub(in crate::native::emitter) fn emit_imageblock_data_call(
        &mut self,
        call: &LlCall,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if call.callee != "air.imageblock_data" {
            return Ok(false);
        }
        self.validate_call_args(call, instructions)?;
        let result_ty = self.resolve_type(&call.ret)?;
        let LlType::Ptr(addrspace) = result_ty else {
            return Err(format!(
                "native emitter: imageblock_data returned non-pointer {result_ty:?}"
            ));
        };
        if addrspace != 4 {
            return Err(format!(
                "native emitter: imageblock_data returned ptr addrspace({addrspace})"
            ));
        }
        let pointee = self
            .ir
            .imageblock_data_pointee
            .clone()
            .unwrap_or_else(|| LlType::Vector(Box::new(LlType::Half), 4));
        if self.ir.imageblock_dimensions.is_none() && !self.ir.imageblock_shared_cells {
            let array_ty = LlType::Array(Box::new(pointee.clone()), 2);
            let slot_ptr_ty = self.ptr_type_id(StorageClass::Private, &pointee)?;
            let storage = if let Some((storage, _)) = self.imageblock_data_scratch.clone() {
                storage
            } else {
                let array_ptr_ty = self.ptr_type_id(StorageClass::Private, &array_ty)?;
                let storage = self.fresh();
                self.module.types_global_values.push(Self::inst(
                    Op::Variable,
                    Some(array_ptr_ty),
                    Some(storage),
                    vec![Operand::StorageClass(StorageClass::Private)],
                ));
                self.imageblock_data_scratch = Some((storage, array_ty.clone()));
                storage
            };
            let result = self.result_id(name, &LlType::Ptr(addrspace))?;
            let zero = self.const_uint(0)?;
            instructions.push(Self::inst(
                Op::InBoundsAccessChain,
                Some(slot_ptr_ty),
                Some(result),
                vec![Operand::IdRef(storage), Operand::IdRef(zero)],
            ));
            self.pointer_storage
                .insert(name.to_string(), StorageClass::Private);
            self.pointer_pointees
                .insert(name.to_string(), self.resolve_type(&pointee)?);
            self.gep_provenance.insert(
                name.to_string(),
                GepProvenance {
                    root: storage,
                    addrspace,
                    source_ty: array_ty,
                    indices: vec![TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Int(0),
                    }],
                    root_indices: None,
                    root_is_indexed_container: true,
                },
            );
            if !self.pointer_phi_values.is_empty() && !self.pointer_nullness.contains_key(name) {
                let is_null = self.const_bool(false)?;
                self.record_pointer_nullness(name.to_string(), is_null);
            }
            return Ok(true);
        }
        if !self.ir.aliased_imageblock_planes.is_empty() && self.ir.imageblock_cell_scale != Some(1)
        {
            return Err(format!(
                "native emitter: an imageblock aliased onto the implicit imageblock stages {:?}                  cells per thread; only one cell per thread, which is one render-target texel, has                  a modelled correspondence to an attachment",
                self.ir.imageblock_cell_scale
            ));
        }
        let cell_scale = self.ir.imageblock_cell_scale.ok_or_else(|| {
            "native emitter: an imageblock cell coordinate is not a linear function of a thread \
             position, so the tile has no row stride that keeps every coordinate inside it and two \
             of its cells would share one linearised index"
                .to_string()
        })?;
        let (cell_count, width_id) = if let Some([width, height]) = self.ir.imageblock_dimensions {
            let cell_count = width
                .checked_mul(height)
                .ok_or_else(|| "native emitter: imageblock cell count overflows".to_string())?;
            (cell_count, self.const_uint(width)?)
        } else {
            let cell_bytes = self.imageblock_cell_allocation_size(&pointee)?;
            let width = self.emit_imageblock_threadgroup_width(instructions)?;
            (
                crate::native::imageblock::cell_capacity(cell_bytes, cell_scale),
                self.emit_scaled_imageblock_width(width, cell_scale, instructions)?,
            )
        };
        let array = LlType::Array(Box::new(pointee.clone()), cell_count);
        let storage = if let Some((storage, _)) = self.imageblock_data_scratch.clone() {
            storage
        } else {
            let array_ptr_ty = self.ptr_type_id(StorageClass::Workgroup, &array)?;
            let storage = self.fresh();
            self.module.types_global_values.push(Self::inst(
                Op::Variable,
                Some(array_ptr_ty),
                Some(storage),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ));
            self.imageblock_data_scratch = Some((storage, pointee.clone()));
            storage
        };
        if !self.ir.aliased_imageblock_planes.is_empty() {
            self.emit_sidecar.aliased_imageblock_staging = Some(storage);
        }

        let coordinate = call
            .args
            .first()
            .ok_or_else(|| "native emitter: imageblock_data has no coordinate".to_string())?;
        let coordinate_ty = self.resolve_type(&coordinate.ty)?;
        let LlType::Vector(component, lanes) = coordinate_ty else {
            return Err("native emitter: imageblock_data coordinate is not a vector".into());
        };
        let component = self.resolve_type(&component)?;
        let LlType::Int(component_bits) = component else {
            return Err("native emitter: imageblock_data coordinate is not integer".into());
        };
        if lanes < 2 {
            return Err(
                "native emitter: imageblock_data coordinate has fewer than two lanes".into(),
            );
        }
        let coordinate_id = self.value_id_in(&coordinate.value, &coordinate.ty, instructions)?;
        let component_ty = self.type_id(&LlType::Int(component_bits))?;
        let mut components = [0; 2];
        for (lane, component_id) in components.iter_mut().enumerate() {
            let extracted = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(component_ty),
                Some(extracted),
                vec![
                    Operand::IdRef(coordinate_id),
                    Operand::LiteralBit32(lane as u32),
                ],
            ));
            *component_id = if component_bits == 32 {
                extracted
            } else {
                let converted = self.fresh();
                let uint_ty = self.type_id(&LlType::Int(32))?;
                instructions.push(Self::inst(
                    Op::UConvert,
                    Some(uint_ty),
                    Some(converted),
                    vec![Operand::IdRef(extracted)],
                ));
                converted
            };
        }
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let row = self.fresh();
        instructions.push(Self::inst(
            Op::IMul,
            Some(uint_ty),
            Some(row),
            vec![Operand::IdRef(components[1]), Operand::IdRef(width_id)],
        ));
        let index = self.fresh();
        instructions.push(Self::inst(
            Op::IAdd,
            Some(uint_ty),
            Some(index),
            vec![Operand::IdRef(row), Operand::IdRef(components[0])],
        ));
        let slot_ptr_ty = self.ptr_type_id(StorageClass::Workgroup, &pointee)?;
        let pointer = self.fresh();
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(slot_ptr_ty),
            Some(pointer),
            vec![Operand::IdRef(storage), Operand::IdRef(index)],
        ));
        self.values
            .insert(name.to_string(), (pointer, LlType::Ptr(addrspace)));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Workgroup);
        self.pointer_pointees
            .insert(name.to_string(), self.resolve_type(&pointee)?);
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root: pointer,
                addrspace,
                source_ty: pointee,
                indices: vec![],
                root_indices: None,
                root_is_indexed_container: false,
            },
        );
        if !self.pointer_phi_values.is_empty() && !self.pointer_nullness.contains_key(name) {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    fn imageblock_cell_allocation_size(&mut self, pointee: &LlType) -> Result<u32, String> {
        let cell_ty = self.type_id(pointee)?;
        let defs = self
            .module
            .types_global_values
            .iter()
            .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
            .collect::<HashMap<_, _>>();
        let (size, align) = crate::layout::spirv_size_align(
            cell_ty,
            &defs,
            crate::layout::SpirvLayout::natural(self.air_data_layout.as_ref()),
        );
        Ok(crate::layout::round_up_u32(size, align.max(1)))
    }

    fn emit_scaled_imageblock_width(
        &mut self,
        width: Word,
        scale: u32,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if scale == 1 {
            return Ok(width);
        }
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let scale_id = self.const_uint(scale)?;
        let scaled = self.fresh();
        instructions.push(Self::inst(
            Op::IMul,
            Some(uint_ty),
            Some(scaled),
            vec![Operand::IdRef(width), Operand::IdRef(scale_id)],
        ));
        Ok(scaled)
    }

    pub(in crate::native::emitter) fn emit_imageblock_threadgroup_width(
        &mut self,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let Some(param_name) = self.ir.imageblock_threads_per_threadgroup_param.clone() else {
            let callee = *self
                .function_ids
                .get(crate::native::ir::IMAGEBLOCK_WIDTH_INTRINSIC)
                .ok_or_else(|| {
                    "native emitter: a cross-coordinate imageblock has no row stride; the entry \
                     declares neither an APV imageblock extent nor a `[[threads_per_threadgroup]]` \
                     parameter, and emitting the module would resolve every tile coordinate to one \
                     per-invocation slot"
                        .to_string()
                })?;
            let uint_ty = self.type_id(&LlType::Int(32))?;
            let width = self.fresh();
            instructions.push(Self::inst(
                Op::FunctionCall,
                Some(uint_ty),
                Some(width),
                vec![Operand::IdRef(callee)],
            ));
            return Ok(width);
        };
        let (param_id, param_ty) = self.values.get(&param_name).cloned().ok_or_else(|| {
            format!(
                "native emitter: imageblock threads_per_threadgroup value {param_name} is unavailable"
            )
        })?;
        let component = match self.resolve_type(&param_ty)? {
            LlType::Vector(component, lanes) if lanes > 0 => {
                let component = self.resolve_type(&component)?;
                let component_ty = self.type_id(&component)?;
                let extracted = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeExtract,
                    Some(component_ty),
                    Some(extracted),
                    vec![Operand::IdRef(param_id), Operand::LiteralBit32(0)],
                ));
                (extracted, component)
            }
            scalar @ LlType::Int(_) => (param_id, scalar),
            other => {
                return Err(format!(
                    "native emitter: threads_per_threadgroup has unsupported imageblock width type {other:?}"
                ));
            }
        };
        let LlType::Int(bits) = component.1 else {
            return Err("native emitter: imageblock row width is not integer".to_string());
        };
        if bits == 32 {
            return Ok(component.0);
        }
        let uint_ty = self.type_id(&LlType::Int(32))?;
        let converted = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(uint_ty),
            Some(converted),
            vec![Operand::IdRef(component.0)],
        ));
        Ok(converted)
    }

    pub(in crate::native::emitter) fn validate_call_args(
        &mut self,
        call: &LlCall,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let callee_params = self
            .ir
            .functions
            .iter()
            .find(|function| function.name == call.callee)
            .map(|function| function.params.clone());
        if let Some(callee_params) = callee_params {
            let args = self.function_call_args_for_params(call, &callee_params)?;
            for (index, arg, (param_name, param_ty)) in args {
                let _ = self.value_id_in(&arg.value, &arg.ty, instructions)?;
                if !matches!(self.resolve_type(&param_ty)?, LlType::Ptr(_)) {
                    continue;
                }
                let Some(pointee) = self.pointer_pointee_for_value(&arg.value)? else {
                    continue;
                };
                let key = (call.callee.clone(), index);
                let declared = self
                    .byte_image_leaf_array(&pointee)
                    .unwrap_or_else(|| pointee.clone());
                match self.function_param_pointees.get(&key) {
                    Some(existing) if !types_compatible(existing, &declared) => {
                        if self.callee_param_accepts_call_pointee(
                            &call.callee,
                            &param_name,
                            &param_ty,
                            &pointee,
                        ) {
                            self.function_param_pointees.insert(key, declared);
                        }
                    }
                    Some(_) => {}
                    None => {
                        if self.callee_param_accepts_call_pointee(
                            &call.callee,
                            &param_name,
                            &param_ty,
                            &pointee,
                        ) {
                            self.function_param_pointees.insert(key, declared);
                        }
                    }
                }
            }
        } else {
            for arg in &call.args {
                let _ = self.value_id_in(&arg.value, &arg.ty, instructions)?;
            }
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn function_call_arg_ids(
        &mut self,
        call: &LlCall,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Vec<Word>, String> {
        let callee_params = self
            .ir
            .functions
            .iter()
            .find(|function| function.name == call.callee)
            .map(|function| function.params.clone())
            .unwrap_or_default();
        if callee_params.is_empty() {
            let mut ids = Vec::with_capacity(call.args.len());
            for arg in &call.args {
                ids.push(self.value_id_in(&arg.value, &arg.ty, instructions)?);
            }
            return Ok(ids);
        }
        let args = self.function_call_args_for_params(call, &callee_params)?;
        let nullness_args = args
            .iter()
            .filter(|(index, _, _)| {
                self.function_param_nullness
                    .contains(&(call.callee.clone(), *index))
            })
            .map(|(_, argument, _)| argument.value.clone())
            .collect::<Vec<_>>();
        let mut ids = Vec::with_capacity(args.len() + nullness_args.len());
        let mut workgroup_cursors = Vec::new();
        for (index, arg, (param_name, param_ty)) in args {
            if let Some(id) = self.data_buffer_array_call_arg_id(
                &call.callee,
                &param_name,
                &param_ty,
                &arg,
                instructions,
            )? {
                ids.push(id);
                continue;
            }
            if let Some(id) = self.raw_workgroup_call_arg_id(
                &call.callee,
                index,
                &param_name,
                &param_ty,
                &arg,
                instructions,
            )? {
                ids.push(id);
                if self.raw_workgroup_cursor_param(&call.callee, index, &param_name, &param_ty) {
                    let raw = match &arg.value {
                        LlValue::Local(name) => self.raw_offsets.get(name).cloned(),
                        _ => None,
                    };
                    workgroup_cursors.push(match raw {
                        Some(raw) => self.emit_raw_byte_index(&raw, 0, instructions)?,
                        None => self.const_uint(0)?,
                    });
                }
                continue;
            }
            if let Some(id) = self.decayed_global_call_arg_id(
                &call.callee,
                index,
                &param_name,
                &param_ty,
                &arg,
                instructions,
            )? {
                ids.push(id);
                continue;
            }
            if let Some(id) = self.byte_image_member_call_arg_id(
                &call.callee,
                index,
                &param_name,
                &param_ty,
                &arg,
                instructions,
            )? {
                ids.push(id);
                continue;
            }
            if let Some(id) = self.raw_device_call_arg_id(
                &call.callee,
                index,
                &param_name,
                &param_ty,
                &arg,
                instructions,
            )? {
                ids.push(id);
                continue;
            }
            if let (LlValue::Local(arg_name), LlType::Ptr(_)) =
                (&arg.value, self.resolve_type(&param_ty)?)
            {
                if self.unmodeled_pointers.contains(arg_name)
                    && self
                        .raw_offsets
                        .get(arg_name)
                        .is_some_and(|raw| !raw.unmodelable)
                    && self.ir.param_is_dereferenced(&call.callee, &param_name)
                {
                    self.cursor_call_sites
                        .insert((call.callee.clone(), arg_name.clone()));
                    return Err(format!(
                        "native emitter: helper @{} parameter {param_name} is passed a \
                         descriptor-backed pointer whose byte cursor cannot cross the call; the \
                         callee would read and write scratch where Metal reaches the buffer",
                        call.callee
                    ));
                }
            }
            ids.push(self.value_id_in(&arg.value, &arg.ty, instructions)?);
        }
        for value in &nullness_args {
            ids.push(self.pointer_call_arg_nullness_id(value)?);
        }
        ids.extend(workgroup_cursors);
        Ok(ids)
    }

    fn data_buffer_array_call_arg_id(
        &mut self,
        callee: &str,
        param_name: &str,
        param_ty: &LlType,
        arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if !self.bda_device_pointers
            || !matches!(self.resolve_type(param_ty)?, LlType::Ptr(0))
            || !self
                .ir
                .metadata_data_buffer_params
                .contains(&(callee.to_string(), param_name.to_string()))
        {
            return Ok(None);
        }
        let LlValue::Local(arg_name) = &arg.value else {
            return Ok(None);
        };
        if let Some(address) = self.bda_direct_addresses.get(arg_name).copied() {
            return Ok(Some(address));
        }
        if !self.data_buffer_params.contains(arg_name)
            || !self.direct_param_indices.contains_key(arg_name)
        {
            return Ok(None);
        }
        let (low, high) = self.emit_direct_buffer_address_payload(arg_name, instructions)?;
        let address = self.combine_pointer_payload_words(low, high, instructions)?;
        self.pointer_payload_words
            .insert(arg_name.to_string(), (low, high));
        self.bda_direct_addresses
            .insert(arg_name.to_string(), address);
        Ok(Some(address))
    }

    fn pointer_call_arg_nullness_id(&mut self, value: &LlValue) -> Result<Word, String> {
        match value {
            LlValue::Zero => self.const_bool(true),
            LlValue::Local(name) => self.pointer_nullness.get(name).copied().ok_or_else(|| {
                format!("native emitter: pointer nullness is not tracked for call argument {name}")
            }),
            LlValue::Global(_) => self.const_bool(false),
            LlValue::Gep(gep) => self.pointer_call_arg_nullness_id(&gep.base.value),
            _ => Err("native emitter: helper pointer nullness requires a pointer value".into()),
        }
    }

    fn function_call_args_for_params(
        &self,
        call: &LlCall,
        callee_params: &[(String, LlType)],
    ) -> Result<Vec<(usize, TypedValue, (String, LlType))>, String> {
        if call.args.len() == callee_params.len() {
            return Ok(call
                .args
                .iter()
                .cloned()
                .zip(callee_params.iter().cloned())
                .enumerate()
                .map(|(index, (arg, param))| (index, arg, param))
                .collect());
        }
        if call.args.len() < callee_params.len() {
            return Err(format!(
                "native emitter: call @{} has {} args for {} params",
                call.callee,
                call.args.len(),
                callee_params.len()
            ));
        }

        let mut out = Vec::with_capacity(callee_params.len());
        let mut arg_index = 0usize;
        for (param_index, param) in callee_params.iter().cloned().enumerate() {
            let mut matched = None;
            for (offset, arg) in call.args[arg_index..].iter().enumerate() {
                if self.call_arg_matches_param(arg, &param.1)? {
                    matched = Some((arg_index + offset, arg.clone()));
                    break;
                }
            }
            let Some((matched_index, arg)) = matched else {
                return Err(format!(
                    "native emitter: call @{} could not align arg {} to param {} type {:?}",
                    call.callee, arg_index, param_index, param.1
                ));
            };
            arg_index = matched_index + 1;
            out.push((param_index, arg, param));
        }
        Ok(out)
    }

    fn call_arg_matches_param(&self, arg: &TypedValue, param_ty: &LlType) -> Result<bool, String> {
        let arg_ty = self.resolve_type(&arg.ty)?;
        let param_ty = self.resolve_type(param_ty)?;
        Ok(types_compatible(&arg_ty, &param_ty))
    }

    pub(in crate::native::emitter) fn decayed_global_call_arg_id(
        &mut self,
        callee: &str,
        param_index: usize,
        param_name: &str,
        param_ty: &LlType,
        arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let LlValue::Global(global_name) = &arg.value else {
            return Ok(None);
        };
        let LlType::Ptr(param_addrspace) = self.resolve_type(param_ty)? else {
            return Ok(None);
        };
        let LlType::Ptr(arg_addrspace) = self.resolve_type(&arg.ty)? else {
            return Ok(None);
        };
        if param_addrspace != arg_addrspace {
            return Ok(None);
        }
        let Some(expected) = self.function_param_concrete_pointee(callee, param_index, param_name)
        else {
            return Ok(None);
        };
        let Some(global_pointee) = self.pointer_pointees.get(global_name).cloned() else {
            return Ok(None);
        };
        let expected = self.resolve_type(&expected)?;
        let global_pointee = self.resolve_type(&global_pointee)?;
        let Some(decay_indices) = decayed_global_call_arg_indices(&global_pointee, &expected)
        else {
            return Ok(None);
        };
        let base = self.value_id(&arg.value, &arg.ty)?;
        if self
            .flat_scalar_reinterpret_globals
            .contains_key(global_name)
            && matches!(&global_pointee, LlType::Array(elem, _) if types_compatible(elem, &expected))
        {
            let ptr_ty = self.ptr_type_id(StorageClass::Private, &expected)?;
            let zero = self.const_int(32, 0)?;
            let result = self.fresh();
            instructions.push(Self::inst(
                Op::InBoundsAccessChain,
                Some(ptr_ty),
                Some(result),
                vec![Operand::IdRef(base), Operand::IdRef(zero)],
            ));
            return Ok(Some(result));
        }
        let storage = if arg_addrspace == 3 {
            StorageClass::Workgroup
        } else {
            StorageClass::Private
        };
        let ptr_ty = self.ptr_type_id(storage, &expected)?;
        let result = self.fresh();
        let mut ops = Vec::with_capacity(decay_indices.len() + 1);
        ops.push(Operand::IdRef(base));
        for index in &decay_indices {
            ops.push(Operand::IdRef(self.value_id(&index.value, &index.ty)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_ty),
            Some(result),
            ops,
        ));
        Ok(Some(result))
    }

    fn byte_image_member_call_arg_id(
        &mut self,
        callee: &str,
        param_index: usize,
        param_name: &str,
        param_ty: &LlType,
        arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let LlValue::Local(arg_name) = &arg.value else {
            return Ok(None);
        };
        if self.raw_offsets.contains_key(arg_name) || self.unmodeled_pointers.contains(arg_name) {
            return Ok(None);
        }
        let (LlType::Ptr(param_addrspace), LlType::Ptr(arg_addrspace)) =
            (self.resolve_type(param_ty)?, self.resolve_type(&arg.ty)?)
        else {
            return Ok(None);
        };
        if param_addrspace != arg_addrspace {
            return Ok(None);
        }
        let Some(expected) = self.function_param_concrete_pointee(callee, param_index, param_name)
        else {
            return Ok(None);
        };
        let expected = self.resolve_type(&expected)?;
        if !is_i8_array_type(&expected) {
            return Ok(None);
        }
        let Some(arg_pointee) = self.pointer_pointees.get(arg_name).cloned() else {
            return Ok(None);
        };
        let mut current = self.resolve_type(&arg_pointee)?;
        let mut depth = 0usize;
        while let LlType::Struct(fields) = &current {
            if fields.len() != 1 {
                return Ok(None);
            }
            let next = self.resolve_type(&fields[0])?;
            current = next;
            depth += 1;
        }
        if depth == 0 || current != expected {
            return Ok(None);
        }
        let Some(storage) = self.pointer_storage.get(arg_name).copied() else {
            return Ok(None);
        };
        let base = self.value_id(&arg.value, &arg.ty)?;
        let ptr_ty = self.ptr_type_id(storage, &expected)?;
        let zero = self.const_int(32, 0)?;
        let result = self.fresh();
        let mut ops = vec![Operand::IdRef(base)];
        ops.extend(std::iter::repeat_n(Operand::IdRef(zero), depth));
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_ty),
            Some(result),
            ops,
        ));
        Ok(Some(result))
    }

    pub(in crate::native::emitter) fn raw_device_call_arg_id(
        &mut self,
        callee: &str,
        param_index: usize,
        param_name: &str,
        param_ty: &LlType,
        arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if !self
            .ir
            .raw_buffer_params
            .contains(&(callee.to_string(), param_name.to_string()))
        {
            return Ok(None);
        }
        let LlType::Ptr(param_addrspace) = self.resolve_type(param_ty)? else {
            return Ok(None);
        };
        if !matches!(param_addrspace, 1 | 2) {
            return Ok(None);
        }
        if param_addrspace == 2
            && !self.bda_device_pointers
            && !self.constant_call_cursor_cannot_conflict(callee, param_index)
        {
            return Ok(None);
        }
        let LlValue::Local(arg_name) = &arg.value else {
            return Ok(None);
        };
        let Some(raw) = self.raw_offsets.get(arg_name).cloned() else {
            return Ok(None);
        };
        if self.bda_device_pointers && matches!(raw.addrspace, 1 | 2) && !raw.unmodelable {
            let mut address_raw = raw.clone();
            if address_raw.device_addr_base.is_none() {
                let Some(base) = self.bda_direct_addresses.get(&raw.root).copied() else {
                    return Ok(None);
                };
                address_raw.device_addr_base = Some(base);
            }
            return self
                .materialize_device_address(&address_raw, instructions)
                .map(Some);
        }
        if !matches!(raw.addrspace, 1 | 2)
            || !raw.dyn_terms.is_empty()
            || raw.unmodelable
            || raw.device_addr_base.is_some()
        {
            return Ok(None);
        }
        let root = raw.root.clone();
        if !self.param_values.contains(&root) || !self.is_raw_buffer_param(&root) {
            return Ok(None);
        }
        let key = (callee.to_string(), param_name.to_string());
        if let Some(previous) = self.raw_call_param_offsets.get(&key) {
            if previous.const_off != raw.const_off || previous.addrspace != raw.addrspace {
                return Err(format!(
                    "native emitter: raw helper parameter @{callee} {param_name} is called with \
                     conflicting constant byte offsets {} and {}",
                    previous.const_off, raw.const_off
                ));
            }
        } else {
            let mut parameter_raw = raw;
            parameter_raw.root = param_name.to_string();
            self.raw_call_param_offsets.insert(key, parameter_raw);
        }
        let id = self.value_id_in(&LlValue::Local(root), &arg.ty, instructions)?;
        Ok(Some(id))
    }

    fn constant_call_cursor_cannot_conflict(&mut self, callee: &str, param_index: usize) -> bool {
        if self.agreeing_constant_call_cursors.is_none() {
            self.agreeing_constant_call_cursors = Some(agreeing_constant_call_cursors(&self.ir));
        }
        self.agreeing_constant_call_cursors
            .as_ref()
            .expect("just set")
            .contains(&(callee.to_string(), param_index))
    }

    pub(in crate::native::emitter) fn raw_workgroup_call_arg_id(
        &mut self,
        callee: &str,
        param_index: usize,
        param_name: &str,
        param_ty: &LlType,
        arg: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if !self
            .ir
            .raw_buffer_params
            .contains(&(callee.to_string(), param_name.to_string()))
        {
            return Ok(None);
        }
        let LlType::Ptr(addrspace) = self.resolve_type(param_ty)? else {
            return Ok(None);
        };
        if addrspace != 3 {
            return Ok(None);
        }
        if self
            .concrete_vector_workgroup_raw_param_pointee(callee, param_index, param_name)
            .is_some()
        {
            return Ok(None);
        }
        let LlType::Ptr(arg_addrspace) = self.resolve_type(&arg.ty)? else {
            return Ok(None);
        };
        if arg_addrspace != 3 {
            return Ok(None);
        }
        let raw = match &arg.value {
            LlValue::Local(arg_name) => self.raw_offsets.get(arg_name).cloned(),
            _ => None,
        };
        let storage = if let Some(raw) = &raw {
            self.raw_access_storage(raw)?
        } else {
            self.pointer_storage_for(&arg.value, arg_addrspace)?
        };
        if storage != StorageClass::Workgroup {
            return Ok(None);
        }
        let raw_ty = raw_workgroup_array_type();
        if let Some(raw) = &raw {
            if raw.unmodelable {
                return Err("native emitter: unmodelable workgroup helper cursor".into());
            }
            let root_id = self.raw_root_value_id(raw)?;
            if self
                .pointer_pointees
                .get(&raw.root)
                .is_some_and(|pointee| types_compatible(pointee, &raw_ty))
            {
                return Ok(Some(root_id));
            }
        } else {
            let arg_id = self.value_id_in(&arg.value, &arg.ty, instructions)?;
            if self
                .pointer_pointee_for_value(&arg.value)?
                .is_some_and(|pointee| types_compatible(&pointee, &raw_ty))
            {
                return Ok(Some(arg_id));
            }
        }
        Err(format!(
            "native emitter: cannot reinterpret workgroup pointer arg {param_name} to raw word view \
             without a logical-pointer bitcast (callee {callee})"
        ))
    }

    pub(in crate::native::emitter) fn shuffled_lane_id(
        &mut self,
        a: &TypedValue,
        b: &TypedValue,
        lane: u32,
        elem: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        if lane == u32::MAX {
            return self.undef_id(elem);
        }
        let a_lanes = self.vector_lane_count(&a.ty)?;
        let (source, source_lane) = if lane < a_lanes {
            (a, lane)
        } else {
            (b, lane - a_lanes)
        };
        if self.one_lane_vector_elem(&source.ty)?.is_some() {
            if source_lane != 0 {
                return Err(format!(
                    "native emitter: one-lane shufflevector source index {source_lane} is out of range"
                ));
            }
            return self.value_id_in(&source.value, &source.ty, instructions);
        }
        let source_id = self.value_id_in(&source.value, &source.ty, instructions)?;
        let result_type = self.type_id(elem)?;
        let result = self.fresh();
        instructions.push(Self::inst(
            Op::CompositeExtract,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(source_id),
                Operand::LiteralBit32(source_lane),
            ],
        ));
        Ok(result)
    }
}

fn decayed_global_call_arg_indices(
    global_pointee: &LlType,
    expected: &LlType,
) -> Option<Vec<TypedValue>> {
    let zero = TypedValue {
        ty: LlType::Int(32),
        value: LlValue::Int(0),
    };
    match global_pointee {
        LlType::Array(elem, _) if types_compatible(elem, expected) => Some(vec![zero]),
        LlType::Struct(fields) => {
            let first = fields.first()?;
            if types_compatible(first, expected) {
                Some(vec![zero])
            } else if matches!(first, LlType::Array(elem, _) if types_compatible(elem, expected)) {
                Some(vec![zero.clone(), zero])
            } else {
                None
            }
        }
        _ => None,
    }
}

fn agreeing_constant_call_cursors(ir: &LlModule) -> HashSet<(String, usize)> {
    let mut sites: HashMap<&str, Vec<(&str, &LlCall)>> = HashMap::default();
    for function in &ir.functions {
        for instruction in function.carrier_insts() {
            if let Some(call) = instruction.alias_call() {
                sites
                    .entry(call.callee.as_str())
                    .or_default()
                    .push((function.name.as_str(), call));
            }
        }
    }
    let mut agreeing = HashSet::default();
    for (callee, calls) in sites {
        let arity = calls
            .iter()
            .map(|(_, call)| call.args.len())
            .max()
            .unwrap_or(0);
        if calls.len() == 1 {
            agreeing.extend((0..arity).map(|index| (callee.to_string(), index)));
            continue;
        }
        let (first_caller, _) = calls[0];
        if calls.iter().any(|(caller, _)| *caller != first_caller) {
            continue;
        }
        let Some(caller) = ir.functions.iter().find(|f| f.name == first_caller) else {
            continue;
        };
        let mut definitions: HashMap<&str, &LlGep> = HashMap::default();
        for instruction in caller.carrier_insts() {
            if let (Some(result), Some(gep)) = (&instruction.result, instruction.gep().as_deref()) {
                definitions.insert(result.as_str(), gep);
            }
        }
        for index in 0..arity {
            let mut keys = calls.iter().map(|(_, call)| {
                call.args.get(index).and_then(|arg| match &arg.value {
                    LlValue::Local(name) => Some(constant_cursor_key(name, &definitions)),
                    _ => None,
                })
            });
            let Some(Some(first)) = keys.next() else {
                continue;
            };
            if keys.all(|key| key.as_ref() == Some(&first)) {
                agreeing.insert((callee.to_string(), index));
            }
        }
    }
    agreeing
}

fn constant_cursor_key(name: &str, definitions: &HashMap<&str, &LlGep>) -> String {
    let Some(gep) = definitions.get(name) else {
        return name.to_string();
    };
    let LlValue::Local(base) = &gep.base.value else {
        return name.to_string();
    };
    if !gep.indices.iter().all(|index| {
        matches!(
            index.value,
            LlValue::Int(_) | LlValue::Hex(_) | LlValue::SignedInt(_) | LlValue::Zero
        )
    }) {
        return name.to_string();
    }
    format!(
        "gep {base} {:?} {:?}",
        gep.source_ty,
        gep.indices
            .iter()
            .map(|index| &index.value)
            .collect::<Vec<_>>()
    )
}
