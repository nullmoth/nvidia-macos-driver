use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_gep_result(
        &mut self,
        name: &str,
        gep: &LlGep,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        let addrspace = match self.resolve_type(&gep.base.ty)? {
            LlType::Ptr(addrspace) => addrspace,
            other => {
                return Err(format!(
                    "native emitter: getelementptr base is not a pointer: {other:?}"
                ))
            }
        };
        let parsed_source_ty = self.resolve_type(&gep.source_ty)?;
        if let LlValue::Local(base_name) = &gep.base.value {
            if self.pointer_phi_values.contains(base_name)
                && !self.raw_offsets.contains_key(base_name)
            {
                if let Some(incoming) = self.tir_phi_incomings.get(base_name).cloned() {
                    let base_ty = self.resolve_type(&gep.base.ty)?;
                    self.reserve_bda_address_phi(base_name, &incoming, &base_ty)?;
                }
            }
        }
        let network_base_pointee = match &gep.base.value {
            LlValue::Local(base) => self.network_pointees.get(base),
            _ => None,
        };
        let base_pointee = if let Some(pointee) = network_base_pointee {
            Some(self.resolve_type(pointee)?)
        } else {
            self.pointer_pointee_for_value(&gep.base.value)?
                .as_ref()
                .map(|ty| self.resolve_type(ty))
                .transpose()?
        };
        if self.null_rooted_pointer_values.contains(name)
            && matches!(&gep.base.value, LlValue::Local(base) if self.null_rooted_pointer_values.contains(base))
        {
            let storage = self.pointer_storage_for(&gep.base.value, addrspace)?;
            let pointee = self
                .network_pointees
                .get(name)
                .cloned()
                .unwrap_or(gep_pointee(&parsed_source_ty, &gep.indices)?);
            let result_type = self.ptr_type_id(storage, &pointee)?;
            let result = self.result_id(name, &LlType::Ptr(addrspace))?;
            self.module.types_global_values.push(Self::inst(
                Op::Undef,
                Some(result_type),
                Some(result),
                vec![],
            ));
            self.pointer_storage.insert(name.to_string(), storage);
            self.pointer_pointees.insert(name.to_string(), pointee);
            return Ok(Some(result));
        }
        if parsed_source_ty == LlType::Int(8) && gep.indices.len() == 1 {
            if let Some(relative) = const_index_i64(&gep.indices[0]) {
                let deferred = match &gep.base.value {
                    LlValue::Local(base_name) => self
                        .workgroup_padding_byte_pointers
                        .get(base_name)
                        .cloned()
                        .map(|base| {
                            let byte_offset = i128::from(base.byte_offset) + i128::from(relative);
                            (base.struct_ty, byte_offset)
                        }),
                    _ => None,
                };
                let direct = if deferred.is_none()
                    && self.pointer_storage_for(&gep.base.value, addrspace).ok()
                        == Some(StorageClass::Workgroup)
                    && matches!(base_pointee, Some(LlType::Struct(_)))
                {
                    Some((
                        base_pointee.clone().expect("matched struct pointee"),
                        i128::from(relative),
                    ))
                } else {
                    None
                };
                if let Some((struct_ty, byte_offset)) = deferred.or(direct) {
                    let byte_offset = u64::try_from(byte_offset).map_err(|_| {
                        "native emitter: Workgroup padding byte GEP has a negative offset"
                            .to_string()
                    })?;
                    if self.struct_range_is_padding(&struct_ty, byte_offset, 1)? {
                        self.pointer_storage
                            .insert(name.to_string(), StorageClass::Workgroup);
                        self.pointer_pointees
                            .insert(name.to_string(), LlType::Int(8));
                        self.workgroup_padding_byte_pointers.insert(
                            name.to_string(),
                            WorkgroupPaddingBytePointer {
                                struct_ty,
                                byte_offset,
                            },
                        );
                        return Ok(None);
                    }
                    if matches!(
                        &gep.base.value,
                        LlValue::Local(base_name)
                            if self.workgroup_padding_byte_pointers.contains_key(base_name)
                    ) {
                        return Err(
                            "native emitter: byte GEP escapes a symbolic Workgroup padding range"
                                .to_string(),
                        );
                    }
                }
            }
        }
        if let LlValue::Local(base_name) = &gep.base.value {
            if let Some(raw_base) = self.raw_offsets.get(base_name).cloned() {
                let base_storage = self.pointer_storage_for(&gep.base.value, addrspace)?;
                let raw = self.apply_raw_gep(raw_base, &parsed_source_ty, &gep.indices)?;
                self.pointer_storage.insert(name.to_string(), base_storage);
                let pointee = gep_pointee(&parsed_source_ty, &gep.indices)?;
                self.pointer_pointees
                    .insert(name.to_string(), pointee.clone());
                if !self.pointer_phi_values.is_empty() {
                    let is_null = self.const_bool(false)?;
                    self.record_pointer_nullness(name.to_string(), is_null);
                    self.materialize_raw_byte_index(name, &raw, true, instructions)?;
                    if self.raw_pointer_word_aligned(&raw) {
                        self.materialize_raw_word_index(name, &raw, true, instructions)?;
                    }
                } else {
                    self.materialize_reserved_raw_byte_index(name, &raw, instructions)?;
                    if self.raw_pointer_word_aligned(&raw) {
                        self.materialize_reserved_raw_word_index(name, &raw, instructions)?;
                    }
                }
                if raw.device_addr_base.is_some() {
                    self.materialize_reserved_bda_address(name, &raw, instructions)?;
                }
                self.raw_offsets.insert(name.to_string(), raw);
                if !matches!(pointee, LlType::Ptr(_)) {
                    if let Some(raw) =
                        self.raw_offsets.get(name).cloned().filter(|raw| {
                            self.bda_device_pointers && raw.device_addr_base.is_some()
                        })
                    {
                        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
                        let address = self.materialize_device_address(&raw, instructions)?;
                        let pointer_type =
                            self.ptr_type_id(StorageClass::PhysicalStorageBuffer, &pointee)?;
                        instructions.push(Self::inst(
                            Op::ConvertUToPtr,
                            Some(pointer_type),
                            Some(result),
                            vec![Operand::IdRef(address)],
                        ));
                        self.pointer_storage
                            .insert(name.to_string(), StorageClass::PhysicalStorageBuffer);
                        self.pointer_pointees
                            .insert(name.to_string(), pointee.clone());
                        self.used_device_address = true;
                    } else {
                        self.define_unmodeled_pointer_value(name, addrspace, &pointee)?;
                    }
                }
                return Ok(self.values.get(name).map(|(id, _)| *id));
            }
            if self.unmodeled_pointers.contains(base_name) {
                let recoverable_field_base = self.values.get(base_name).is_some_and(|(id, _)| {
                    self.emit_sidecar
                        .local_pointer_field_loads
                        .iter()
                        .any(|fact| fact.id == *id)
                });
                if !recoverable_field_base {
                    let pointee = gep_pointee(&parsed_source_ty, &gep.indices)?;
                    if matches!(pointee, LlType::Ptr(_)) {
                        self.define_unmodeled_byte_pointer_value(name, addrspace)?;
                    } else {
                        self.define_unmodeled_pointer_value(name, addrspace, &pointee)?;
                    }
                    return Ok(self.values.get(name).map(|(id, _)| *id));
                }
            }
            if let Some(selected) = self.selected_pointers.get(base_name).cloned() {
                if self.emit_selected_pointer_gep(
                    name,
                    &selected,
                    &parsed_source_ty,
                    &gep.indices,
                    instructions,
                )? {
                    return Ok(None);
                }
            }
            if let Some(tree) = self.selected_access_trees.get(base_name).cloned() {
                if self.emit_selected_access_tree_gep(
                    name,
                    &tree,
                    &parsed_source_ty,
                    &gep.indices,
                    instructions,
                )? {
                    return Ok(None);
                }
            }
            if let Some(selected) = self.selected_load_pointers.get(base_name).cloned() {
                if self.emit_selected_load_pointer_gep(
                    name,
                    &selected,
                    &parsed_source_ty,
                    &gep.indices,
                    instructions,
                )? {
                    return Ok(None);
                }
            }
            if let Some(table) = self.dynamic_pointer_tables.get(base_name).cloned() {
                if self.emit_dynamic_pointer_table_gep(
                    name,
                    addrspace,
                    &table,
                    &parsed_source_ty,
                    &gep.indices,
                    instructions,
                )? {
                    return Ok(None);
                }
            }
        }
        let parsed_base = self.value_id_in(&gep.base.value, &gep.base.ty, instructions)?;
        let base_storage = self.pointer_storage_for(&gep.base.value, addrspace)?;
        let base_points_to_aggregate = base_pointee
            .as_ref()
            .is_some_and(|ty| matches!(ty, LlType::Array(_, _) | LlType::Struct(_)));
        let base_is_param = match &gep.base.value {
            LlValue::Local(name) => self.param_values.contains(name),
            _ => false,
        };
        let provenance = match &gep.base.value {
            LlValue::Local(base_name) => self.gep_provenance.get(base_name).cloned(),
            _ => None,
        };
        let composed = if let Some(prev) = &provenance {
            self.compose_followup_gep(name, prev, &parsed_source_ty, &gep.indices, instructions)?
                .map(|indices| (prev, indices))
        } else {
            None
        };
        let composed_root_indices = if let Some((prev, _)) = &composed {
            if let Some(root_indices) = prev.root_indices.clone() {
                let mut combined = root_indices;
                if let Some(first) = gep.indices.first() {
                    if let Some(last) = combined.last_mut() {
                        *last = self.combine_gep_indices(last, first, instructions)?;
                    } else {
                        combined.push(first.clone());
                    }
                }
                combined.extend(gep.indices.iter().skip(1).cloned());
                Some(combined)
            } else {
                None
            }
        } else {
            None
        };
        let was_composed = composed.is_some();
        let previous_root_is_indexed_container = provenance
            .as_ref()
            .is_some_and(|prev| prev.root_is_indexed_container);
        let (base, source_ty, indices, root, provenance_addrspace) =
            if let Some((prev, indices)) = composed {
                (
                    prev.root,
                    prev.source_ty.clone(),
                    indices,
                    prev.root,
                    prev.addrspace,
                )
            } else {
                (
                    parsed_base,
                    parsed_source_ty,
                    gep.indices.clone(),
                    parsed_base,
                    addrspace,
                )
            };
        if base_is_param && indices.len() == 1 && const_index(indices.first()) == Some(0) {
            if let Some(base_pointee) = base_pointee.as_ref() {
                if self.entry_gep_preserves_base_pointee(base_pointee, &source_ty)? {
                    self.values
                        .insert(name.to_string(), (base, LlType::Ptr(addrspace)));
                    self.pointer_storage.insert(name.to_string(), base_storage);
                    self.pointer_pointees
                        .insert(name.to_string(), base_pointee.clone());
                    if let LlValue::Local(base_name) = &gep.base.value {
                        if let Some(is_null) = self.pointer_nullness.get(base_name).copied() {
                            self.record_pointer_nullness(name.to_string(), is_null);
                        }
                    }
                    // The GEP names the parameter itself, but a wider load through it (Apple's GL loads an ivec3
                    // uniform at offset 0 through `getelementptr float, ptr %ubo, i32 0`) needs the slots after it:
                    // keep the GEP as its provenance, as a nonzero first index would have.
                    let prev = provenance.unwrap_or_else(|| GepProvenance {
                        root: base,
                        addrspace,
                        source_ty: source_ty.clone(),
                        indices: indices.clone(),
                        root_indices: None,
                        root_is_indexed_container: self.is_indexed_container_root(base, None),
                    });
                    self.gep_provenance.insert(name.to_string(), prev);
                    return Ok(Some(base));
                }
            }
        }
        if !base_is_param && is_zero_wrapper_identity_gep(&source_ty, &indices) {
            self.values
                .insert(name.to_string(), (base, LlType::Ptr(addrspace)));
            self.pointer_storage.insert(name.to_string(), base_storage);
            self.pointer_pointees
                .insert(name.to_string(), gep_pointee(&source_ty, &indices)?);
            if let LlValue::Local(base_name) = &gep.base.value {
                if let Some(is_null) = self.pointer_nullness.get(base_name).copied() {
                    self.record_pointer_nullness(name.to_string(), is_null);
                }
            }
            if let Some(prev) = provenance {
                self.gep_provenance.insert(name.to_string(), prev);
            }
            return Ok(Some(base));
        }
        if !base_is_param
            && base_storage == StorageClass::Workgroup
            && indices.iter().all(|idx| const_index(Some(idx)) == Some(0))
        {
            if let Some(base_pointee) = base_pointee.as_ref() {
                let pointee = gep_pointee(&source_ty, &indices)?;
                if types_compatible(base_pointee, &pointee) {
                    self.values
                        .insert(name.to_string(), (base, LlType::Ptr(addrspace)));
                    self.pointer_storage.insert(name.to_string(), base_storage);
                    self.pointer_pointees.insert(name.to_string(), pointee);
                    if let LlValue::Local(base_name) = &gep.base.value {
                        if let Some(is_null) = self.pointer_nullness.get(base_name).copied() {
                            self.record_pointer_nullness(name.to_string(), is_null);
                        }
                    }
                    if let Some(prev) = provenance {
                        self.gep_provenance.insert(name.to_string(), prev);
                    }
                    return Ok(Some(base));
                }
            }
        }
        if self.emit_private_struct_byte_gep(
            name,
            addrspace,
            base_storage,
            base_pointee.as_ref(),
            provenance.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        if self.emit_private_vector_byte_gep(
            name,
            addrspace,
            base_storage,
            base_pointee.as_ref(),
            provenance.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        if self.record_vector_word_pointer_gep(
            name,
            base,
            base_storage,
            base_pointee.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        if self.emit_workgroup_vector_stream_gep(
            name,
            addrspace,
            base,
            base_storage,
            base_pointee.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        if self.emit_workgroup_scalar_vector_gep(
            name,
            addrspace,
            base,
            base_storage,
            base_pointee.as_ref(),
            base_is_param,
            provenance.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        if self.emit_workgroup_scalar_aggregate_gep(
            name,
            addrspace,
            base,
            base_storage,
            base_pointee.as_ref(),
            base_is_param,
            provenance.as_ref(),
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(None);
        }
        let entry_metadata_indices = if base_is_param {
            base_pointee.as_ref().map_or(Ok(None), |base_pointee| {
                self.entry_gep_metadata_indices(base_pointee, &source_ty, &indices)
            })?
        } else {
            None
        };
        let pointee = if let (Some(base_pointee), Some(metadata_indices)) =
            (base_pointee.as_ref(), entry_metadata_indices.as_ref())
        {
            gep_pointee(base_pointee, metadata_indices)?
        } else {
            gep_pointee(&source_ty, &indices)?
        };
        let source_points_to_aggregate =
            matches!(source_ty, LlType::Array(_, _) | LlType::Struct(_));
        if base_is_param
            && source_points_to_aggregate
            && type_contains_pointer(&source_ty)
            && matches!(pointee, LlType::Ptr(_))
        {
            let logical_indices = entry_metadata_indices
                .clone()
                .unwrap_or_else(|| indices.clone());
            let storage_source_ty = function_storage_local_type(&source_ty);
            let access_pointee = gep_pointee(&storage_source_ty, &logical_indices)?;
            let ptr_type = self.ptr_type_id(StorageClass::Function, &access_pointee)?;
            let result = self.result_id(name, &LlType::Ptr(addrspace))?;
            let mut ops = vec![Operand::IdRef(root)];
            for idx in gep_spirv_indices(&logical_indices)? {
                ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
            }
            instructions.push(Self::inst(
                Op::InBoundsAccessChain,
                Some(ptr_type),
                Some(result),
                ops,
            ));
            self.pointer_storage
                .insert(name.to_string(), StorageClass::Function);
            self.pointer_pointees
                .insert(name.to_string(), access_pointee);
            if !self.pointer_phi_values.is_empty() {
                let is_null = self.const_bool(false)?;
                self.record_pointer_nullness(name.to_string(), is_null);
            }
            self.gep_provenance.insert(
                name.to_string(),
                GepProvenance {
                    root,
                    addrspace: provenance_addrspace,
                    source_ty: if entry_metadata_indices.is_some() {
                        base_pointee.clone().unwrap_or(source_ty)
                    } else {
                        source_ty
                    },
                    indices: logical_indices,
                    root_indices: None,
                    root_is_indexed_container: previous_root_is_indexed_container
                        || self.is_indexed_container_root(root, None),
                },
            );
            return Ok(Some(result));
        }
        if let Some(raw) = self.byte_array_reinterpret_raw_gep(
            &gep.base.value,
            base,
            addrspace,
            base_storage,
            base_pointee.as_ref(),
            &source_ty,
            &indices,
        )? {
            let result = self.result_id(name, &LlType::Ptr(addrspace))?;
            self.emit_private_zero_pointer_value_at(
                result,
                &pointee,
                &format!("byte_array_reinterpret_raw_gep name={name}"),
            )?;
            self.pointer_storage
                .insert(name.to_string(), StorageClass::Private);
            self.pointer_pointees.insert(name.to_string(), pointee);
            self.raw_offsets.insert(name.to_string(), raw);
            self.unmodeled_pointers.insert(name.to_string());
            if !self.pointer_phi_values.is_empty() {
                let is_null = self.const_bool(false)?;
                self.record_pointer_nullness(name.to_string(), is_null);
            }
            return Ok(Some(result));
        }
        if let Some(result) = self.emit_flat_scalar_reinterpret_gep(
            name,
            &gep.base.value,
            base,
            addrspace,
            base_storage,
            &source_ty,
            &indices,
            instructions,
        )? {
            return Ok(Some(result));
        }
        if !was_composed
            && indices.len() == 1
            && matches!(base_pointee.as_ref(), Some(LlType::Int(8)))
            && is_byte_view_scalar_or_vector_source(&source_ty)
            && !matches!(base_storage, StorageClass::Private | StorageClass::Function)
        {
            let result = self.emit_byte_view_scalar_gep(
                name,
                addrspace,
                base_storage,
                base,
                &source_ty,
                &indices[0],
                instructions,
            )?;
            return Ok(Some(result));
        }
        let access_storage = if base_storage == StorageClass::Private
            && source_points_to_aggregate
            && type_contains_pointer(&source_ty)
        {
            StorageClass::Function
        } else {
            base_storage
        };
        let access_pointee =
            if access_storage == StorageClass::Function && source_points_to_aggregate {
                let storage_source_ty = function_storage_local_type(&source_ty);
                gep_pointee(&storage_source_ty, &indices)?
            } else {
                pointee.clone()
            };
        let ptr_type = self.ptr_type_id(access_storage, &access_pointee)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        if !was_composed
            && indices.len() > 1
            && matches!(
                access_storage,
                StorageClass::StorageBuffer | StorageClass::UniformConstant
            )
            && self.pointer_id_already_at_scalar(
                base,
                &gep.base.value,
                &access_pointee,
                ptr_type,
                instructions,
                base_pointee.as_ref(),
            )
        {
            if let Some(flat_result) = self.emit_flattened_scalar_arm_access_chain(
                ptr_type,
                base,
                &source_ty,
                &access_pointee,
                &indices,
                instructions,
            )? {
                if let Some(entry) = self.values.get_mut(name) {
                    entry.0 = flat_result;
                }
                self.pointer_storage
                    .insert(name.to_string(), access_storage);
                self.pointer_pointees
                    .insert(name.to_string(), access_pointee.clone());
                if !self.pointer_phi_values.is_empty() {
                    let is_null = self.const_bool(false)?;
                    self.record_pointer_nullness(name.to_string(), is_null);
                }
                self.gep_provenance.insert(
                    name.to_string(),
                    GepProvenance {
                        root,
                        addrspace: provenance_addrspace,
                        source_ty: source_ty.clone(),
                        indices: indices.clone(),
                        root_indices: None,
                        root_is_indexed_container: previous_root_is_indexed_container
                            || self.is_indexed_container_root(root, None),
                    },
                );
                return Ok(Some(flat_result));
            }
        }
        let pointee_points_to_aggregate =
            matches!(pointee, LlType::Array(_, _) | LlType::Struct(_));
        let struct_base_stride = matches!(base_pointee.as_ref(), Some(LlType::Struct(_)));
        let array_base_stride = matches!(base_pointee.as_ref(), Some(LlType::Array(_, _)))
            && base_pointee
                .as_ref()
                .is_some_and(|base_ty| types_compatible(base_ty, &source_ty))
            && indices.len() >= 2;
        let first_index_nonzero = indices.first().and_then(|idx| const_index(Some(idx))) != Some(0);
        let base_is_pointer_phi = matches!(
            &gep.base.value,
            LlValue::Local(base_name) if self.pointer_phi_values.contains(base_name)
        );
        let use_ptr_access_chain = decide_ptr_access_chain(PtrAccessChainInputs {
            pointee_points_to_aggregate,
            base_storage: access_storage,
            is_indexed_container_root: self.is_indexed_container_root(base, Some(access_storage)),
            struct_base_stride,
            array_base_stride,
            first_index_nonzero,
            was_composed,
            base_is_param,
            base_points_to_aggregate,
            base_is_pointer_phi,
        });
        let incompatible_aggregate_base = base_pointee.as_ref().is_some_and(|base_ty| {
            base_points_to_aggregate && !types_compatible(base_ty, &source_ty)
        });
        let reinterpreted_workgroup_indices =
            if incompatible_aggregate_base && base_storage == StorageClass::Workgroup {
                base_pointee.as_ref().map_or(Ok(None), |base_ty| {
                    self.reinterpreted_workgroup_aggregate_indices(
                        base_ty, &source_ty, &pointee, &indices,
                    )
                })?
            } else {
                None
            };
        let keep_incompatible_root_index = incompatible_aggregate_base
            && base_pointee
                .as_ref()
                .is_some_and(|base_ty| aggregate_member0_wraps_source(base_ty, &source_ty));
        let member0_array_element_root_indices = if incompatible_aggregate_base {
            base_pointee.as_ref().and_then(|base_ty| {
                aggregate_member0_array_element_wraps_source(base_ty, &source_ty).then(|| {
                    let mut prefixed = Vec::with_capacity(indices.len() + 1);
                    prefixed.push(TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Int(0),
                    });
                    prefixed.extend(indices.iter().cloned());
                    prefixed
                })
            })
        } else {
            None
        };
        let workgroup_param_array_backing = base_is_param
            && base_storage == StorageClass::Workgroup
            && indices.len() >= 2
            && !matches!(&gep.base.value,
                LlValue::Local(base_name) if self.is_raw_buffer_param(base_name));
        let logical_indices = entry_metadata_indices.as_ref().unwrap_or(&indices);
        let access_indices = if use_ptr_access_chain {
            logical_indices.clone()
        } else if let Some(indices) = reinterpreted_workgroup_indices {
            indices
        } else if let Some(indices) = composed_root_indices.clone() {
            indices
        } else if let Some(indices) = member0_array_element_root_indices.clone() {
            indices
        } else if keep_incompatible_root_index || workgroup_param_array_backing {
            logical_indices.clone()
        } else {
            gep_spirv_indices(logical_indices)?
        };
        let mut ops = vec![Operand::IdRef(base)];
        for idx in &access_indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        let opcode = if access_indices.is_empty() {
            Op::CopyObject
        } else if use_ptr_access_chain {
            Op::PtrAccessChain
        } else {
            Op::InBoundsAccessChain
        };
        instructions.push(Self::inst(opcode, Some(ptr_type), Some(result), ops));
        self.pointer_storage
            .insert(name.to_string(), access_storage);
        self.pointer_pointees
            .insert(name.to_string(), access_pointee);
        if let LlValue::Local(base_name) = &gep.base.value {
            if let Some(is_null) = self.pointer_nullness.get(base_name).copied() {
                let known_non_null = self.const_bool(false)?;
                if is_null == known_non_null {
                    self.record_pointer_nullness(name.to_string(), known_non_null);
                }
            }
        }
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        if indices.len() == 1 {
            self.materialize_reserved_pointer_index(name, &indices[0], instructions)?;
        }
        let fact_source_ty = if entry_metadata_indices.is_some() {
            base_pointee.clone().unwrap_or_else(|| source_ty.clone())
        } else {
            source_ty.clone()
        };
        let fact_indices = entry_metadata_indices
            .clone()
            .unwrap_or_else(|| indices.clone());
        let raw = self.apply_raw_gep(
            RawBufferOffset::root(String::new(), provenance_addrspace),
            &fact_source_ty,
            &fact_indices,
        )?;
        if !raw.unmodelable && raw.const_off >= 0 {
            if raw.dyn_terms.is_empty() {
                self.emit_sidecar.buffer_access_offsets.push(
                    crate::emit_sidecar::BufferAccessOffset {
                        id: result,
                        root,
                        byte_offset: raw.const_off as u64,
                    },
                );
            } else if raw.dyn_terms.iter().all(|(_, stride)| *stride >= 0) {
                let terms = raw
                    .dyn_terms
                    .iter()
                    .map(|(index, stride)| {
                        Ok((
                            self.value_id(&index.value, &index.ty)?,
                            u64::try_from(*stride).map_err(|_| {
                                "native emitter: negative affine buffer stride".to_string()
                            })?,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                self.emit_sidecar.buffer_access_affine_offsets.push(
                    crate::emit_sidecar::BufferAccessAffineOffset {
                        id: result,
                        root,
                        constant: raw.const_off as u64,
                        terms,
                    },
                );
            }
        }
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root,
                addrspace: provenance_addrspace,
                source_ty: if entry_metadata_indices.is_some() {
                    base_pointee.clone().unwrap_or(source_ty)
                } else {
                    source_ty
                },
                indices: entry_metadata_indices.unwrap_or(indices),
                root_indices: (composed_root_indices.is_some()
                    || member0_array_element_root_indices.is_some())
                .then(|| access_indices.clone()),
                root_is_indexed_container: previous_root_is_indexed_container
                    || self.is_indexed_container_root(root, None),
            },
        );
        Ok(Some(result))
    }

    pub(in crate::native::emitter) fn emit_byte_view_scalar_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base_storage: StorageClass,
        base: Word,
        source_ty: &LlType,
        index: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let (elem_size, _) = self.raw_type_size_align(source_ty)?;
        let idx_ty = self.resolve_type(&index.ty)?;
        let idx_bits = match idx_ty {
            LlType::Int(bits) => bits,
            _ => 32,
        };
        let int_ty = self.type_id(&LlType::Int(idx_bits))?;
        let idx_id = self.value_id(&index.value, &index.ty)?;
        let byte_index = if elem_size <= 1 {
            idx_id
        } else {
            let factor = if idx_bits == 64 {
                self.const_signed_int(64, elem_size as i64)?
            } else {
                self.const_uint(elem_size as u32)?
            };
            let scaled = self.fresh();
            instructions.push(Self::inst(
                Op::IMul,
                Some(int_ty),
                Some(scaled),
                vec![Operand::IdRef(idx_id), Operand::IdRef(factor)],
            ));
            scaled
        };
        let ptr_ty = self.ptr_type_id(base_storage, &LlType::Int(8))?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        instructions.push(Self::inst(
            Op::PtrAccessChain,
            Some(ptr_ty),
            Some(result),
            vec![Operand::IdRef(base), Operand::IdRef(byte_index)],
        ));
        self.pointer_storage.insert(name.to_string(), base_storage);
        self.pointer_pointees
            .insert(name.to_string(), LlType::Int(8));
        self.byte_view_pointers.insert(name.to_string());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(result)
    }

    pub(in crate::native::emitter) fn entry_gep_preserves_base_pointee(
        &self,
        base_pointee: &LlType,
        source_ty: &LlType,
    ) -> Result<bool, String> {
        if types_compatible(base_pointee, source_ty) {
            return Ok(true);
        }
        let (LlType::Struct(source_fields), LlType::Struct(metadata_fields)) =
            (source_ty, base_pointee)
        else {
            return Ok(false);
        };
        let base_storage = function_storage_local_type(base_pointee);
        let source_storage = function_storage_local_type(source_ty);
        let (base_size, _base_align) = self.raw_type_size_align(&base_storage)?;
        let (source_size, _source_align) = self.raw_type_size_align(&source_storage)?;
        if base_size != source_size {
            return Ok(false);
        }
        self.struct_layout_fields_compatible_by_offset(source_fields, metadata_fields)
    }

    pub(in crate::native::emitter) fn struct_layout_fields_compatible_by_offset(
        &self,
        source_fields: &[LlType],
        metadata_fields: &[LlType],
    ) -> Result<bool, String> {
        for metadata_index in 0..metadata_fields.len() {
            let (metadata_offset, metadata_field) =
                self.raw_struct_member(metadata_fields, metadata_index as u64)?;
            let metadata_field = function_storage_local_type(&metadata_field);
            if !self.source_struct_has_matching_field(
                source_fields,
                metadata_offset,
                &metadata_field,
            )? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn source_struct_has_matching_field(
        &self,
        source_fields: &[LlType],
        metadata_offset: u64,
        metadata_field: &LlType,
    ) -> Result<bool, String> {
        for source_index in 0..source_fields.len() {
            let (source_offset, source_field) =
                self.raw_struct_member(source_fields, source_index as u64)?;
            if source_offset != metadata_offset {
                continue;
            }
            let source_field = function_storage_local_type(&source_field);
            if types_compatible(&source_field, metadata_field) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::native::emitter) fn entry_gep_metadata_indices(
        &self,
        base_pointee: &LlType,
        source_ty: &LlType,
        indices: &[TypedValue],
    ) -> Result<Option<Vec<TypedValue>>, String> {
        if indices.len() == 1 {
            return if self.entry_gep_preserves_base_pointee(base_pointee, source_ty)? {
                Ok(Some(indices.to_vec()))
            } else {
                Ok(None)
            };
        }
        let mut out = Vec::with_capacity(indices.len());
        let mut source_cur = source_ty.clone();
        let mut metadata_cur = base_pointee.clone();
        for (position, index) in indices.iter().enumerate() {
            if position == 0 {
                out.push(index.clone());
                continue;
            }
            match (&source_cur, &metadata_cur) {
                (LlType::Struct(source_fields), LlType::Struct(metadata_fields)) => {
                    let Some(source_index) = const_index(Some(index)) else {
                        return Ok(None);
                    };
                    let (source_offset, source_field) =
                        self.raw_struct_member(source_fields, u64::from(source_index))?;
                    let Some((metadata_index, metadata_field)) = self.matching_metadata_field(
                        metadata_fields,
                        source_offset,
                        &source_field,
                    )?
                    else {
                        return Ok(None);
                    };
                    out.push(TypedValue {
                        ty: index.ty.clone(),
                        value: LlValue::Int(u64::from(metadata_index)),
                    });
                    source_cur = source_field;
                    metadata_cur = metadata_field;
                }
                (LlType::Array(source_elem, _), LlType::Array(metadata_elem, _))
                | (LlType::Vector(source_elem, _), LlType::Vector(metadata_elem, _)) => {
                    out.push(index.clone());
                    source_cur = self.resolve_type(source_elem)?;
                    metadata_cur = self.resolve_type(metadata_elem)?;
                }
                _ => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    pub(in crate::native::emitter) fn matching_metadata_field(
        &self,
        metadata_fields: &[LlType],
        source_offset: u64,
        source_field: &LlType,
    ) -> Result<Option<(u32, LlType)>, String> {
        let source_field = function_storage_local_type(source_field);
        for metadata_index in 0..metadata_fields.len() {
            let (metadata_offset, metadata_field) =
                self.raw_struct_member(metadata_fields, metadata_index as u64)?;
            if metadata_offset != source_offset {
                continue;
            }
            let metadata_storage = function_storage_local_type(&metadata_field);
            if types_compatible(&metadata_storage, &source_field) {
                return Ok(Some((metadata_index as u32, metadata_field)));
            }
        }
        Ok(None)
    }

    pub(in crate::native::emitter) fn emit_private_vector_byte_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        provenance: Option<&GepProvenance>,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if (!matches!(
            base_storage,
            StorageClass::Private | StorageClass::Workgroup
        ) || (base_storage == StorageClass::Workgroup && addrspace != 4))
            || self.resolve_type(source_ty)? != LlType::Int(8)
            || indices.len() != 1
        {
            return Ok(false);
        }
        let Some(base_pointee) = base_pointee else {
            return Ok(false);
        };
        let vector_ty = self.resolve_type(base_pointee)?;
        if !matches!(vector_ty, LlType::Vector(_, _)) {
            return Ok(false);
        }
        let Some(byte_offset) = const_index(indices.first()) else {
            return Ok(false);
        };
        let (vector_size, _) = self.raw_type_size_align(&vector_ty)?;
        if vector_size == 0 || !(byte_offset as u64).is_multiple_of(vector_size) {
            return Ok(false);
        }
        let vector_index = byte_offset as u64 / vector_size;
        if vector_index > u32::MAX as u64 {
            return Ok(false);
        }
        let Some(provenance) = provenance else {
            return Ok(false);
        };
        let provenance_ty = self.resolve_type(&provenance.source_ty)?;
        let LlType::Array(array_elem, array_len) = provenance_ty else {
            return Ok(false);
        };
        if !types_compatible(&array_elem, &vector_ty) {
            return Ok(false);
        }
        let Some(base_index) = provenance
            .indices
            .last()
            .and_then(|idx| const_index(Some(idx)))
        else {
            return Ok(false);
        };
        let target_index = base_index as u64 + vector_index;
        if target_index > u32::MAX as u64 || target_index >= array_len as u64 {
            return Ok(false);
        }
        let ptr_type = self.ptr_type_id(base_storage, &vector_ty)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let index = self.const_uint(target_index as u32)?;
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_type),
            Some(result),
            vec![Operand::IdRef(provenance.root), Operand::IdRef(index)],
        ));
        self.pointer_storage.insert(name.to_string(), base_storage);
        self.pointer_pointees.insert(name.to_string(), vector_ty);
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root: provenance.root,
                addrspace: provenance.addrspace,
                source_ty: provenance.source_ty.clone(),
                indices: vec![TypedValue {
                    ty: LlType::Int(32),
                    value: LlValue::Int(target_index),
                }],
                root_indices: None,
                root_is_indexed_container: provenance.root_is_indexed_container,
            },
        );
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_private_struct_byte_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        provenance: Option<&GepProvenance>,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if (!matches!(
            base_storage,
            StorageClass::Private | StorageClass::Workgroup
        ) || (base_storage == StorageClass::Workgroup && addrspace != 4))
            || self.resolve_type(source_ty)? != LlType::Int(8)
            || indices.len() != 1
        {
            return Ok(false);
        }
        let Some(base_pointee) = base_pointee else {
            return Ok(false);
        };
        let struct_ty = self.resolve_type(base_pointee)?;
        let LlType::Struct(fields) = &struct_ty else {
            return Ok(false);
        };
        let Some(byte_offset) = const_index(indices.first()) else {
            return Ok(false);
        };
        let mut matching_member = None;
        for (member_index, _) in fields.iter().enumerate() {
            let (offset, member_ty) = self.raw_struct_member(fields, member_index as u64)?;
            if offset == byte_offset as u64 {
                matching_member = Some((member_index as u32, member_ty));
                break;
            }
        }
        let Some((member_index, member_ty)) = matching_member else {
            return Ok(false);
        };
        let Some(provenance) = provenance else {
            return Ok(false);
        };
        let Some(provenance_ty) =
            self.provenance_access_chain_pointee(&provenance.source_ty, &provenance.indices)?
        else {
            return Ok(false);
        };
        if !types_compatible(&provenance_ty, &struct_ty) {
            return Ok(false);
        }
        let ptr_type = self.ptr_type_id(base_storage, &member_ty)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let index = self.const_uint(member_index)?;
        let mut operands = Vec::with_capacity(provenance.indices.len() + 2);
        operands.push(Operand::IdRef(provenance.root));
        for provenance_index in &provenance.indices {
            operands.push(Operand::IdRef(
                self.value_id(&provenance_index.value, &provenance_index.ty)?,
            ));
        }
        operands.push(Operand::IdRef(index));
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_type),
            Some(result),
            operands,
        ));
        self.pointer_storage.insert(name.to_string(), base_storage);
        self.pointer_pointees
            .insert(name.to_string(), member_ty.clone());
        let mut member_path = provenance.indices.clone();
        member_path.push(TypedValue {
            ty: LlType::Int(32),
            value: LlValue::Int(member_index as u64),
        });
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root: provenance.root,
                addrspace: provenance.addrspace,
                source_ty: provenance.source_ty.clone(),
                indices: member_path,
                root_indices: None,
                root_is_indexed_container: provenance.root_is_indexed_container,
            },
        );
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn provenance_access_chain_pointee(
        &self,
        source: &LlType,
        indices: &[TypedValue],
    ) -> Result<Option<LlType>, String> {
        let mut current = self.resolve_type(source)?;
        for index in indices {
            current = match current {
                LlType::Struct(fields) => {
                    let Some(index) = const_index(Some(index)) else {
                        return Ok(None);
                    };
                    let Some(field) = fields.get(index as usize) else {
                        return Ok(None);
                    };
                    self.resolve_type(field)?
                }
                LlType::Array(elem, _) | LlType::Vector(elem, _) => self.resolve_type(&elem)?,
                _ => return Ok(None),
            };
        }
        Ok(Some(current))
    }

    pub(in crate::native::emitter) fn byte_array_reinterpret_raw_gep(
        &self,
        base_value: &LlValue,
        base_id: Word,
        addrspace: u32,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        source_ty: &LlType,
        indices: &[TypedValue],
    ) -> Result<Option<RawBufferOffset>, String> {
        if !matches!(base_storage, StorageClass::Function | StorageClass::Private) {
            return Ok(None);
        }
        let Some(root) = self
            .byte_array_reinterpret_root(base_value, base_pointee)
            .or_else(|| self.byte_array_reinterpret_root_for_id(base_id))
        else {
            return Ok(None);
        };
        let Some(root_pointee) = self.pointer_pointees.get(&root) else {
            return Ok(None);
        };
        if types_compatible(root_pointee, source_ty) {
            return Ok(None);
        }
        self.apply_raw_gep(RawBufferOffset::root(root, addrspace), source_ty, indices)
            .map(Some)
    }

    pub(in crate::native::emitter) fn emit_flat_scalar_reinterpret_gep(
        &mut self,
        name: &str,
        base_value: &LlValue,
        base_id: Word,
        addrspace: u32,
        base_storage: StorageClass,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if base_storage != StorageClass::Private
            || !matches!(source_ty, LlType::Array(_, _) | LlType::Struct(_))
        {
            return Ok(None);
        }
        let Some((root_name, root_id, view_ty)) =
            self.flat_scalar_reinterpret_root(base_value, base_id)
        else {
            return Ok(None);
        };
        let pointee = gep_pointee(source_ty, indices)?;
        let Some(access_indices) = self.flat_scalar_reinterpret_access_indices(
            &view_ty,
            source_ty,
            indices,
            &pointee,
            instructions,
        )?
        else {
            return Err(format!(
                "native emitter: cannot remap GEP of flat-scalar global {root_name} from {source_ty:?} with indices {indices:?} into {view_ty:?}"
            ));
        };
        let ptr_ty = self.ptr_type_id(StorageClass::Private, &pointee)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let mut ops = vec![Operand::IdRef(root_id)];
        for index in &access_indices {
            ops.push(Operand::IdRef(self.value_id(&index.value, &index.ty)?));
        }
        instructions.push(Self::inst(
            Op::InBoundsAccessChain,
            Some(ptr_ty),
            Some(result),
            ops,
        ));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Private);
        self.pointer_pointees
            .insert(name.to_string(), pointee.clone());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        if let Some(index) = access_indices.last() {
            self.materialize_reserved_pointer_index(name, index, instructions)?;
        }
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root: root_id,
                addrspace,
                source_ty: source_ty.clone(),
                indices: indices.to_vec(),
                root_indices: Some(access_indices),
                root_is_indexed_container: true,
            },
        );
        Ok(Some(result))
    }

    fn flat_scalar_reinterpret_root(
        &self,
        base_value: &LlValue,
        base_id: Word,
    ) -> Option<(String, Word, LlType)> {
        let candidate = match base_value {
            LlValue::Global(name) if self.flat_scalar_reinterpret_globals.contains_key(name) => {
                self.global_values
                    .get(name)
                    .map(|(id, _)| (name.clone(), *id))
            }
            _ => None,
        }
        .or_else(|| {
            self.global_values.iter().find_map(|(name, (id, _))| {
                (*id == base_id && self.flat_scalar_reinterpret_globals.contains_key(name))
                    .then(|| (name.clone(), *id))
            })
        })?;
        let pointee = self.pointer_pointees.get(&candidate.0)?;
        Some((candidate.0, candidate.1, pointee.clone()))
    }

    fn flat_scalar_reinterpret_access_indices(
        &mut self,
        view_ty: &LlType,
        source_ty: &LlType,
        indices: &[TypedValue],
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Vec<TypedValue>>, String> {
        if types_compatible(view_ty, source_ty) {
            return gep_spirv_indices(indices).map(Some);
        }
        let Some((scalar, _)) = flat_scalar_leaf_count(view_ty) else {
            return Ok(None);
        };
        let scalar = self.resolve_type(&scalar)?;
        let Some(offset) = self.constant_gep_offset_in_scalar_units(source_ty, indices, &scalar)?
        else {
            return Ok(None);
        };
        self.decompose_scalar_offset_indices(view_ty, offset, pointee, instructions)
    }

    fn constant_gep_offset_in_scalar_units(
        &self,
        source_ty: &LlType,
        indices: &[TypedValue],
        scalar: &LlType,
    ) -> Result<Option<u64>, String> {
        if indices.is_empty() {
            return Ok(None);
        }
        let (scalar_size, _) = self.raw_type_size_align(scalar)?;
        if scalar_size == 0 {
            return Ok(None);
        }
        let Some(first) = const_index(indices.first()) else {
            return Ok(None);
        };
        let source_ty = self.resolve_type(source_ty)?;
        let (source_size, _) = self.raw_type_size_align(&source_ty)?;
        if !source_size.is_multiple_of(scalar_size) {
            return Ok(None);
        }
        let mut offset = u64::from(first) * (source_size / scalar_size);
        if indices.len() > 1 {
            let Some((byte_offset, _)) =
                self.constant_aggregate_gep_offset(&source_ty, &indices[1..])?
            else {
                return Ok(None);
            };
            if !byte_offset.is_multiple_of(scalar_size) {
                return Ok(None);
            }
            offset += byte_offset / scalar_size;
        }
        Ok(Some(offset))
    }

    fn decompose_scalar_offset_indices(
        &self,
        ty: &LlType,
        offset: u64,
        pointee: &LlType,
        _instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Vec<TypedValue>>, String> {
        let mut offset = offset;
        let mut ty = self.resolve_type(ty)?;
        let mut out = Vec::new();
        loop {
            if types_compatible(&ty, pointee) {
                return Ok(Some(out));
            }
            match ty {
                LlType::Array(elem, len) => {
                    let elem = self.resolve_type(&elem)?;
                    let Some((_, elem_units)) = flat_scalar_leaf_count(&elem) else {
                        return Ok(None);
                    };
                    if elem_units == 0 {
                        return Ok(None);
                    }
                    let index = offset / u64::from(elem_units);
                    if index >= u64::from(len) {
                        return Ok(None);
                    }
                    out.push(TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Int(index),
                    });
                    offset %= u64::from(elem_units);
                    ty = elem;
                }
                LlType::Struct(fields) => {
                    let mut cursor = 0u64;
                    let mut selected = None;
                    for (field_index, field) in fields.iter().enumerate() {
                        let field = self.resolve_type(field)?;
                        let Some((_, field_units)) = flat_scalar_leaf_count(&field) else {
                            return Ok(None);
                        };
                        let next = cursor + u64::from(field_units);
                        if offset < next {
                            selected = Some((field_index, field, cursor));
                            break;
                        }
                        cursor = next;
                    }
                    let Some((field_index, field, cursor)) = selected else {
                        return Ok(None);
                    };
                    out.push(TypedValue {
                        ty: LlType::Int(32),
                        value: LlValue::Int(field_index as u64),
                    });
                    offset -= cursor;
                    ty = field;
                }
                _ => return Ok(None),
            }
        }
    }

    pub(in crate::native::emitter) fn byte_array_reinterpret_root(
        &self,
        base_value: &LlValue,
        base_pointee: Option<&LlType>,
    ) -> Option<String> {
        let name = match base_value {
            LlValue::Local(name) => name,
            LlValue::Global(name) => {
                return self
                    .pointer_pointees
                    .get(name)
                    .is_some_and(is_i8_array_type)
                    .then(|| name.clone());
            }
            _ => return None,
        };
        if base_pointee.is_some_and(is_i8_array_type) {
            return Some(name.clone());
        }
        let (id, _) = self.values.get(name)?;
        self.values
            .iter()
            .find_map(|(candidate, (candidate_id, _))| {
                (candidate_id == id
                    && self
                        .pointer_pointees
                        .get(candidate)
                        .is_some_and(is_i8_array_type))
                .then(|| candidate.clone())
            })
    }

    pub(in crate::native::emitter) fn byte_array_reinterpret_root_for_id(
        &self,
        id: Word,
    ) -> Option<String> {
        self.values
            .iter()
            .chain(self.global_values.iter())
            .find_map(|(candidate, (candidate_id, _))| {
                (candidate_id == &id
                    && self
                        .pointer_pointees
                        .get(candidate)
                        .is_some_and(is_i8_array_type))
                .then(|| candidate.clone())
            })
    }

    pub(in crate::native::emitter) fn byte_array_reinterpret_raw_pointer(
        &self,
        value: &LlValue,
    ) -> Result<Option<RawBufferOffset>, String> {
        let LlValue::Local(name) = value else {
            return Ok(None);
        };
        if let Some(root) = self.byte_array_reinterpret_root(value, self.pointer_pointees.get(name))
        {
            let addrspace = self
                .values
                .get(name)
                .and_then(|(_, ty)| match ty {
                    LlType::Ptr(addrspace) => Some(*addrspace),
                    _ => None,
                })
                .unwrap_or(0);
            return Ok(Some(RawBufferOffset::root(root, addrspace)));
        }
        let Some(provenance) = self.gep_provenance.get(name) else {
            return Ok(None);
        };
        let Some(root) = self.byte_array_reinterpret_root_for_id(provenance.root) else {
            return Ok(None);
        };
        let Some(root_pointee) = self.pointer_pointees.get(&root) else {
            return Ok(None);
        };
        if types_compatible(root_pointee, &provenance.source_ty) {
            return Ok(None);
        }
        self.apply_raw_gep(
            RawBufferOffset::root(root, provenance.addrspace),
            &provenance.source_ty,
            &provenance.indices,
        )
        .map(Some)
    }

    pub(in crate::native::emitter) fn emit_workgroup_vector_stream_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base: Word,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if base_storage != StorageClass::Workgroup || indices.len() != 1 {
            return Ok(false);
        }
        let source_ty = self.resolve_type(source_ty)?;
        if !matches!(source_ty, LlType::Vector(_, _)) {
            return Ok(false);
        }
        let Some(base_pointee) = base_pointee else {
            return Ok(false);
        };
        let base_pointee = self.resolve_type(base_pointee)?;
        let base_is_vector_pointer = if types_compatible(&base_pointee, &source_ty) {
            true
        } else if let LlType::Array(elem, _) = &base_pointee {
            if types_compatible(elem, &source_ty) {
                false
            } else {
                return Ok(false);
            }
        } else {
            return Ok(false);
        };

        if let Some(root) =
            self.vector_word_root_from_vector_ty(base_storage, &source_ty, base_is_vector_pointer)?
        {
            self.vector_word_roots.entry(base).or_insert(root);
        }

        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, &source_ty)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let index = self.value_id(&indices[0].value, &indices[0].ty)?;
        let use_ptr_access =
            base_is_vector_pointer && !self.is_workgroup_indexed_container_root(base);
        instructions.push(Self::inst(
            if use_ptr_access {
                Op::PtrAccessChain
            } else {
                Op::InBoundsAccessChain
            },
            Some(ptr_type),
            Some(result),
            vec![Operand::IdRef(base), Operand::IdRef(index)],
        ));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Workgroup);
        self.pointer_pointees
            .insert(name.to_string(), source_ty.clone());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        self.materialize_reserved_pointer_index(name, &indices[0], instructions)?;
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root: base,
                addrspace,
                source_ty,
                indices: indices.to_vec(),
                root_indices: None,
                root_is_indexed_container: !base_is_vector_pointer,
            },
        );
        Ok(true)
    }

    pub(in crate::native::emitter) fn reinterpreted_workgroup_aggregate_indices(
        &mut self,
        base_ty: &LlType,
        source_ty: &LlType,
        pointee: &LlType,
        indices: &[TypedValue],
    ) -> Result<Option<Vec<TypedValue>>, String> {
        if const_index(indices.first()) != Some(0) {
            return Ok(None);
        }
        let base_ty = self.resolve_type(base_ty)?;
        let mut base_cur = base_ty.clone();
        let mut source_cur = self.resolve_type(source_ty)?;
        let mut out = Vec::new();
        if !align_reinterpreted_workgroup_type(&mut base_cur, &source_cur, &mut out) {
            return Ok(None);
        }
        for index in &indices[1..] {
            match source_cur.clone() {
                LlType::Array(source_elem, source_len) => {
                    if !align_reinterpreted_workgroup_type(&mut base_cur, &source_cur, &mut out) {
                        return Ok(None);
                    }
                    let LlType::Array(base_elem, base_len) = base_cur else {
                        return Ok(None);
                    };
                    if base_len != source_len {
                        return Ok(None);
                    }
                    out.push(index.clone());
                    source_cur = *source_elem;
                    base_cur = *base_elem;
                }
                LlType::Struct(source_fields) => {
                    if !align_reinterpreted_workgroup_type(&mut base_cur, &source_cur, &mut out) {
                        return Ok(None);
                    }
                    let LlType::Struct(base_fields) = base_cur else {
                        return Ok(None);
                    };
                    let Some(field) = const_index(Some(index)) else {
                        return Ok(None);
                    };
                    let Some(source_field) = source_fields.get(field as usize).cloned() else {
                        return Ok(None);
                    };
                    let Some(base_field) = base_fields.get(field as usize).cloned() else {
                        return Ok(None);
                    };
                    out.push(index.clone());
                    source_cur = source_field;
                    base_cur = base_field;
                }
                LlType::Vector(source_elem, source_lanes) => {
                    if !align_reinterpreted_workgroup_type(&mut base_cur, &source_cur, &mut out) {
                        return Ok(None);
                    }
                    let LlType::Vector(base_elem, base_lanes) = base_cur else {
                        return Ok(None);
                    };
                    if base_lanes != source_lanes {
                        return Ok(None);
                    }
                    out.push(index.clone());
                    source_cur = *source_elem;
                    base_cur = *base_elem;
                }
                _ => return Ok(None),
            }
        }
        if !align_reinterpreted_workgroup_type(&mut base_cur, &source_cur, &mut out) {
            return Ok(None);
        }
        if !types_compatible(&base_cur, pointee) {
            return Ok(None);
        }
        Ok(Some(out))
    }

    pub(in crate::native::emitter) fn emit_workgroup_scalar_aggregate_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base: Word,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        base_is_param: bool,
        provenance: Option<&GepProvenance>,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if base_storage != StorageClass::Workgroup || base_is_param || indices.is_empty() {
            return Ok(false);
        }
        if !matches!(source_ty, LlType::Array(_, _) | LlType::Struct(_)) {
            return Ok(false);
        }
        let Some(base_pointee) = base_pointee else {
            return Ok(false);
        };
        let (access_pointee, scaled) = if indices.len() == 1 {
            let (source_size, _) = self.raw_type_size_align(source_ty)?;
            let (pointee_size, _) = self.raw_type_size_align(base_pointee)?;
            if pointee_size == 0 || source_size == 0 || source_size % pointee_size != 0 {
                return Ok(false);
            }
            let scale = source_size / pointee_size;
            if scale > u32::MAX as u64 {
                return Ok(false);
            }
            (
                base_pointee.to_owned(),
                self.scale_gep_index(&indices[0], scale as u32, name, instructions)?,
            )
        } else {
            let Some((elem, index)) = self.workgroup_scalar_array_record_gep_index(
                base_pointee,
                source_ty,
                indices,
                name,
                instructions,
            )?
            else {
                return Ok(false);
            };
            (elem, index)
        };
        let (access_base, access_indices, root) = if let Some(prev) =
            provenance.filter(|prev| types_compatible(&prev.source_ty, &access_pointee))
        {
            let mut combined = prev.indices.clone();
            if let Some(last) = combined.last_mut() {
                *last = self.combine_gep_indices(last, &scaled, instructions)?;
            } else {
                combined.push(scaled);
            }
            (prev.root, combined, prev.root)
        } else {
            (base, vec![scaled], base)
        };
        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, &access_pointee)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let op = if self.is_workgroup_global_root(access_base) {
            Op::InBoundsAccessChain
        } else {
            Op::PtrAccessChain
        };
        let mut ops = vec![Operand::IdRef(access_base)];
        for idx in &access_indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        instructions.push(Self::inst(op, Some(ptr_type), Some(result), ops));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Workgroup);
        self.pointer_pointees
            .insert(name.to_string(), access_pointee.clone());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        if let Some(index) = access_indices.last() {
            self.materialize_reserved_pointer_index(name, index, instructions)?;
        }
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root,
                addrspace,
                source_ty: access_pointee,
                indices: access_indices,
                root_indices: None,
                root_is_indexed_container: self.is_workgroup_indexed_container_root(root),
            },
        );
        Ok(true)
    }

    pub(in crate::native::emitter) fn workgroup_scalar_array_record_gep_index(
        &mut self,
        base_pointee: &LlType,
        source_ty: &LlType,
        indices: &[TypedValue],
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<(LlType, TypedValue)>, String> {
        let LlType::Array(elem, _) = self.resolve_type(base_pointee)? else {
            return Ok(None);
        };
        let elem = self.resolve_type(&elem)?;
        if !is_scalar_storage_type(&elem) {
            return Ok(None);
        }
        let source_ty = self.resolve_type(source_ty)?;
        let (field_offset, field_ty) =
            match self.constant_aggregate_gep_offset(&source_ty, &indices[1..])? {
                Some(offset) => offset,
                None => return Ok(None),
            };
        if !types_compatible(&elem, &field_ty) {
            return Ok(None);
        }
        let (source_size, _) = self.raw_type_size_align(&source_ty)?;
        let (elem_size, _) = self.raw_type_size_align(&elem)?;
        if elem_size == 0
            || source_size == 0
            || source_size % elem_size != 0
            || field_offset % elem_size != 0
        {
            return Ok(None);
        }
        let scale = source_size / elem_size;
        let field_index = field_offset / elem_size;
        if scale > u32::MAX as u64 || field_index > u32::MAX as u64 {
            return Ok(None);
        }
        let scaled = self.scale_gep_index(&indices[0], scale as u32, name, instructions)?;
        let index = if field_index == 0 {
            scaled
        } else {
            let field_index = TypedValue {
                ty: scaled.ty.clone(),
                value: LlValue::Int(field_index),
            };
            self.combine_gep_indices(&scaled, &field_index, instructions)?
        };
        Ok(Some((elem, index)))
    }

    pub(in crate::native::emitter) fn constant_aggregate_gep_offset(
        &self,
        ty: &LlType,
        indices: &[TypedValue],
    ) -> Result<Option<(u64, LlType)>, String> {
        let mut offset = 0;
        let mut cur = self.resolve_type(ty)?;
        for index in indices {
            let Some(index) = const_index(Some(index)).map(u64::from) else {
                return Ok(None);
            };
            match cur {
                LlType::Struct(fields) => {
                    let (member_offset, member_ty) = self.raw_struct_member(&fields, index)?;
                    offset += member_offset;
                    cur = member_ty;
                }
                LlType::Array(elem, len) => {
                    if index >= u64::from(len) {
                        return Ok(None);
                    }
                    let elem = self.resolve_type(&elem)?;
                    let (elem_size, elem_align) = self.raw_type_size_align(&elem)?;
                    offset += index * round_up_u64(elem_size, elem_align);
                    cur = elem;
                }
                LlType::Vector(elem, lanes) => {
                    if index >= u64::from(lanes) {
                        return Ok(None);
                    }
                    let elem = self.resolve_type(&elem)?;
                    let (elem_size, _) = self.raw_type_size_align(&elem)?;
                    offset += index * elem_size;
                    cur = elem;
                }
                _ => return Ok(None),
            }
        }
        Ok(Some((offset, cur)))
    }

    pub(in crate::native::emitter) fn emit_workgroup_scalar_vector_gep(
        &mut self,
        name: &str,
        addrspace: u32,
        base: Word,
        base_storage: StorageClass,
        base_pointee: Option<&LlType>,
        base_is_param: bool,
        provenance: Option<&GepProvenance>,
        source_ty: &LlType,
        indices: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if base_storage != StorageClass::Workgroup || indices.len() != 1 {
            return Ok(false);
        }
        let LlType::Vector(elem, lanes) = source_ty else {
            return Ok(false);
        };
        if *lanes <= 1 || !base_pointee.is_some_and(|pointee| types_compatible(pointee, elem)) {
            return Ok(false);
        }

        let scaled = self.scale_gep_index(&indices[0], *lanes, name, instructions)?;
        let (access_base, access_indices, root) =
            if let Some(prev) = provenance.filter(|prev| types_compatible(&prev.source_ty, elem)) {
                let mut combined = prev.indices.clone();
                if let Some(last) = combined.last_mut() {
                    *last = self.combine_gep_indices(last, &scaled, instructions)?;
                } else {
                    combined.push(scaled);
                }
                (prev.root, combined, prev.root)
            } else {
                (base, vec![scaled], base)
            };

        let ptr_type = self.ptr_type_id(StorageClass::Workgroup, elem)?;
        let result = self.result_id(name, &LlType::Ptr(addrspace))?;
        let access_base_is_param_root = self.param_values.iter().any(|param| {
            self.values
                .get(param)
                .is_some_and(|(id, _)| *id == access_base)
        });
        let use_ptr_access_chain = !access_base_is_param_root && !base_is_param;
        let spirv_indices = if use_ptr_access_chain {
            access_indices.clone()
        } else {
            gep_spirv_indices(&access_indices)?
        };
        let mut ops = vec![Operand::IdRef(access_base)];
        for idx in &spirv_indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        instructions.push(Self::inst(
            if use_ptr_access_chain {
                Op::PtrAccessChain
            } else {
                Op::InBoundsAccessChain
            },
            Some(ptr_type),
            Some(result),
            ops,
        ));
        self.pointer_storage
            .insert(name.to_string(), StorageClass::Workgroup);
        self.pointer_pointees
            .insert(name.to_string(), elem.as_ref().clone());
        if !self.pointer_phi_values.is_empty() {
            let is_null = self.const_bool(false)?;
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        if let Some(index) = access_indices.last() {
            self.materialize_reserved_pointer_index(name, index, instructions)?;
        }
        self.gep_provenance.insert(
            name.to_string(),
            GepProvenance {
                root,
                addrspace,
                source_ty: elem.as_ref().clone(),
                indices: access_indices,
                root_indices: None,
                root_is_indexed_container: self.is_workgroup_indexed_container_root(root),
            },
        );
        Ok(true)
    }
}
