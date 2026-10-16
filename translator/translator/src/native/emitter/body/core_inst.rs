use super::*;
use crate::native::emitter::ops::bfloat_lanes;

impl Emitter {
    pub(in crate::native::emitter) fn emit_body_inst(
        &mut self,
        inst: &crate::native::tir::TirInst,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if inst.opcode == "metal2vulkan.inline_parameter" {
            let (Some(name), Some(argument)) = (
                inst.result.as_deref(),
                inst.operands
                    .first()
                    .and_then(crate::native::tir::TirOperand::as_typed_value),
            ) else {
                return Err("native emitter: malformed typed inline parameter \
                     (reason=inline_parameter_missing_operand)"
                    .to_string());
            };
            return self.bind_inline_parameter(name, argument, instructions);
        }
        if let Some(name) = &inst.result {
            if let Some((op, kind)) = binary_op_dispatch(&inst.opcode) {
                if let Some(operands) = self.tir_inst_typed_operands(inst) {
                    if let [lhs, rhs] = operands.as_slice() {
                        let (lhs, rhs, name) = (lhs.clone(), rhs.clone(), name.clone());
                        return match kind {
                            BinaryKind::Int => {
                                self.emit_binary_int_op_resolved(op, lhs, rhs, name, instructions)
                            }
                            BinaryKind::Float => {
                                if let Some(mode) = inst.float_math_mode() {
                                    return self.emit_float_op_granting(
                                        op,
                                        lhs,
                                        rhs,
                                        name,
                                        mode,
                                        instructions,
                                    );
                                }
                                if inst.fast_math()
                                    && self.try_emit_fast_fma(
                                        op,
                                        &lhs,
                                        &rhs,
                                        &name,
                                        instructions,
                                    )?
                                {
                                    return Ok(());
                                }
                                self.emit_binary_float_op_resolved(op, lhs, rhs, name, instructions)
                            }
                            BinaryKind::Signed => self.emit_signed_binary_int_op_resolved(
                                op,
                                lhs,
                                rhs,
                                name,
                                instructions,
                            ),
                        };
                    }
                }
            } else if let Some(kind) = unary_op_dispatch(&inst.opcode) {
                if let Some(operands) = self.tir_inst_typed_operands(inst) {
                    if let [value] = operands.as_slice() {
                        let (value, name) = (value.clone(), name.clone());
                        return match kind {
                            UnaryKind::Fneg => self.emit_unary_float_op_resolved(
                                Op::FNegate,
                                value,
                                name,
                                instructions,
                            ),
                            UnaryKind::Freeze => {
                                self.emit_freeze_resolved(value, name, instructions)
                            }
                        };
                    }
                }
            } else if let Some(kind) = convert_op_dispatch(&inst.opcode) {
                if let (Some(operands), Some(result_ty)) =
                    (self.tir_inst_typed_operands(inst), inst.result_ty.as_ref())
                {
                    if let [src] = operands.as_slice() {
                        let dst_ty = self.resolve_type(result_ty)?;
                        let (src, name) = (src.clone(), name.clone());
                        return match kind {
                            ConvertKind::Int(op) => {
                                self.emit_int_convert_resolved(op, src, dst_ty, name, instructions)
                            }
                            ConvertKind::Float => {
                                self.emit_float_convert_resolved(src, dst_ty, name, instructions)
                            }
                            ConvertKind::IntToFloat(op) => self.emit_int_to_float_convert_resolved(
                                op,
                                src,
                                dst_ty,
                                name,
                                instructions,
                            ),
                            ConvertKind::FloatToInt(op) => self.emit_float_to_int_convert_resolved(
                                op,
                                src,
                                dst_ty,
                                name,
                                instructions,
                            ),
                        };
                    }
                }
            } else if inst.opcode == "select" {
                if let Some(operands) = self.tir_inst_typed_operands(inst) {
                    if let [cond, t, f] = operands.as_slice() {
                        let (cond, t, f, name) = (cond.clone(), t.clone(), f.clone(), name.clone());
                        return self.emit_select_resolved(cond, t, f, name, instructions);
                    }
                }
            } else if inst.opcode == "fcmp" {
                if let (Some(tok), Some(operands)) = (
                    inst.cmp_predicate().as_deref(),
                    self.tir_inst_typed_operands(inst),
                ) {
                    if let [lhs, rhs] = operands.as_slice() {
                        if let Some(pred) = fcmp_predicate(tok) {
                            let (lhs, rhs, name) = (lhs.clone(), rhs.clone(), name.clone());
                            return self.emit_fcmp_resolved(pred, lhs, rhs, name, instructions);
                        }
                        if let Some(ordered) = fcmp_ordering_predicate(tok) {
                            let (lhs, rhs, name) = (lhs.clone(), rhs.clone(), name.clone());
                            return self.emit_fcmp_ordering_resolved(
                                ordered,
                                lhs,
                                rhs,
                                name,
                                instructions,
                            );
                        }
                    }
                }
            } else if inst.opcode == "icmp" {
                if let (Some(tok), Some(operands)) = (
                    inst.cmp_predicate().as_deref(),
                    self.tir_inst_typed_operands(inst),
                ) {
                    if let [lhs, rhs] = operands.as_slice() {
                        if let Some(pred) = icmp_predicate(tok) {
                            let operand_ty = self.resolve_type(&lhs.ty)?;
                            if !matches!(operand_ty, LlType::Ptr(_)) {
                                let (lhs, rhs, name) = (lhs.clone(), rhs.clone(), name.clone());
                                return self.emit_icmp_int_resolved(
                                    pred,
                                    lhs,
                                    rhs,
                                    operand_ty,
                                    name,
                                    instructions,
                                );
                            } else if let Some(rest) = &inst.icmp_rest() {
                                let (lhs, rhs, name, rest) =
                                    (lhs.clone(), rhs.clone(), name.clone(), rest.clone());
                                return self.emit_icmp_ptr_resolved(
                                    pred,
                                    lhs,
                                    rhs,
                                    name,
                                    &rest,
                                    instructions,
                                );
                            }
                        }
                    }
                }
            } else if inst.opcode == "inttoptr" || inst.opcode == "ptrtoint" {
                if let (Some(operands), Some(result_ty)) =
                    (self.tir_inst_typed_operands(inst), inst.result_ty.as_ref())
                {
                    if let [src] = operands.as_slice() {
                        let dst_ty = self.resolve_type(result_ty)?;
                        let (src, name) = (src.clone(), name.clone());
                        return if inst.opcode == "inttoptr" {
                            self.emit_inttoptr_resolved(src, dst_ty, name, instructions)
                        } else {
                            self.emit_ptrtoint_resolved(src, dst_ty, name, instructions)
                        };
                    }
                }
            } else if inst.opcode == "getelementptr" {
                if let (Some(source_ty), Some(ops)) =
                    (inst.gep_source_ty(), self.tir_inst_typed_operands(inst))
                {
                    if !ops.is_empty() {
                        let gep = LlGep {
                            inbounds: inst.gep().as_ref().is_some_and(|gep| gep.inbounds),
                            source_ty: source_ty.clone(),
                            base: ops[0].clone(),
                            indices: ops[1..].to_vec(),
                        };
                        let name = name.clone();
                        self.emit_gep_result(&name, &gep, instructions)?;
                        return Ok(());
                    }
                }
            } else if inst.opcode == "load" {
                if let (Some(operands), Some(result_ty)) =
                    (self.tir_inst_typed_operands(inst), inst.result_ty.as_ref())
                {
                    if let [ptr] = operands.as_slice() {
                        let result_ty = self.resolve_type(result_ty)?;
                        let load = LlLoad {
                            ptr: ptr.clone(),
                            result_ty: result_ty.clone(),
                            align: inst.mem_align(),
                        };
                        let name = name.clone();
                        return self.emit_load_resolved(name, load, result_ty, instructions);
                    }
                }
            } else if inst.opcode == "extractelement" {
                if let (Some(operands), Some(line)) =
                    (self.tir_inst_typed_operands(inst), &inst.diag_line())
                {
                    if let [vector, idx] = operands.as_slice() {
                        let (vector, idx, name, line) =
                            (vector.clone(), idx.clone(), name.clone(), line.clone());
                        return self.emit_extractelement_resolved(
                            vector,
                            idx,
                            name,
                            &line,
                            instructions,
                        );
                    }
                }
            } else if inst.opcode == "insertelement" {
                if let (Some(operands), Some(line)) =
                    (self.tir_inst_typed_operands(inst), &inst.diag_line())
                {
                    if let [composite, object, idx] = operands.as_slice() {
                        let (composite, object, idx, name, line) = (
                            composite.clone(),
                            object.clone(),
                            idx.clone(),
                            name.clone(),
                            line.clone(),
                        );
                        return self.emit_insertelement_resolved(
                            composite,
                            object,
                            idx,
                            name,
                            &line,
                            instructions,
                        );
                    }
                }
            } else if inst.opcode == "shufflevector" {
                if let (Some(a), Some(b), Some((declared, lanes)), Some(line)) = (
                    inst.operands
                        .first()
                        .and_then(crate::native::tir::TirOperand::as_typed_value),
                    inst.operands
                        .get(1)
                        .and_then(crate::native::tir::TirOperand::as_typed_value),
                    &inst.shuffle_mask(),
                    &inst.diag_line(),
                ) {
                    let (name, line, lanes) = (name.clone(), line.clone(), lanes.clone());
                    return self.emit_shufflevector_from_mask(
                        a,
                        b,
                        *declared,
                        lanes,
                        &line,
                        name,
                        instructions,
                    );
                }
            } else if inst.opcode == "extractvalue" {
                if let (Some(operands), Some(indices)) = (
                    self.tir_inst_typed_operands(inst),
                    &inst.aggregate_indices(),
                ) {
                    if let [composite] = operands.as_slice() {
                        let (composite, name, indices) =
                            (composite.clone(), name.clone(), indices.clone());
                        return self.emit_extractvalue_typed(
                            composite,
                            &indices,
                            name,
                            instructions,
                        );
                    }
                }
            } else if inst.opcode == "insertvalue" {
                if let (Some(operands), Some(indices)) = (
                    self.tir_inst_typed_operands(inst),
                    &inst.aggregate_indices(),
                ) {
                    if let [composite, object] = operands.as_slice() {
                        let (composite, object, name, indices) = (
                            composite.clone(),
                            object.clone(),
                            name.clone(),
                            indices.clone(),
                        );
                        return self.emit_insertvalue_typed(
                            composite,
                            object,
                            &indices,
                            name,
                            instructions,
                        );
                    }
                }
            } else if inst.opcode == "alloca" {
                if let Some(alloca_ty) = &inst.alloca_ty() {
                    let name = name.clone();
                    return self.emit_alloca_typed(name, alloca_ty, instructions);
                }
            } else if inst.opcode == "phi" {
                if let Some((phi_ty, parsed_incoming)) = &inst.phi_incoming() {
                    let (name, phi_ty, parsed_incoming) =
                        (name.clone(), phi_ty.clone(), parsed_incoming.clone());
                    return self.emit_phi_resolved(name, &phi_ty, parsed_incoming, instructions);
                }
            } else if inst.opcode == "bitcast" {
                if let Some((src, dst_text)) = inst.bitcast() {
                    let (name, src) = (name.clone(), src.clone());
                    return self.emit_bitcast_resolved(src, dst_text, name, instructions);
                }
            } else if matches!(inst.opcode.as_str(), "call" | "tail") {
                if let Some(call) = &inst.call() {
                    let (name, mut call) = (name.clone(), (**call).clone());
                    self.apply_tir_inst_call_args(inst, &name, &mut call);
                    return self.emit_value_call_resolved(name, call, instructions);
                }
                if let Some(err) = &inst.value_call_error() {
                    return Err(err.clone());
                }
            }
        } else if inst.opcode == "store" {
            if let Some(operands) = self.tir_inst_typed_operands(inst) {
                if let [object, ptr] = operands.as_slice() {
                    let (object, ptr) = (object.clone(), ptr.clone());
                    return self.emit_store_resolved(object, ptr, inst.mem_align(), instructions);
                }
            }
            if let Some((object, ptr)) = inst.store().as_deref() {
                let (object, ptr) = (object.clone(), ptr.clone());
                return self.emit_store_resolved(object, ptr, inst.mem_align(), instructions);
            }
        } else if let Some(line) = &inst.void_call_line() {
            if is_ignored_call_line(line) {
                return Ok(());
            }
            if let Some(rest) = strip_call_prefix(line) {
                if self.drop_indirect_function_group_call(rest, instructions)? {
                    return Ok(());
                }
            }
            if let Some(call) = &inst.call() {
                let (mut call, line) = ((**call).clone(), line.clone());
                self.apply_tir_inst_call_args(inst, "void", &mut call);
                return self.emit_void_call_body(call, &line, instructions);
            }
        }
        Err(format!(
            "native emitter: instruction not handled by the typed graph walk \
             (reason=graph_walk_unmigrated_opcode, opcode={}, phi_incoming={}, phi_parse_error={:?}, operands={:?}, result={:?})",
            inst.opcode,
            inst.phi_incoming().is_some(),
            inst.phi_parse_error(),
            inst.operands,
            inst.result
        ))
    }

    fn try_emit_fast_fma(
        &mut self,
        op: Op,
        lhs: &TypedValue,
        rhs: &TypedValue,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !self.fast_contract_adds.contains(name) || self.fast_uncontracted_sums.contains(name) {
            return Ok(false);
        }
        let lhs_product = match &lhs.value {
            LlValue::Local(value) => self.fast_float_products.get(value).cloned(),
            _ => None,
        };
        let rhs_product = match &rhs.value {
            LlValue::Local(value) => self.fast_float_products.get(value).cloned(),
            _ => None,
        };
        if op != Op::FAdd {
            return Ok(false);
        }
        let (product_lhs, product_rhs, addend) = match (lhs_product, rhs_product) {
            (_, Some((a, b))) => (a, b, lhs),
            (Some((a, b)), None) => (a, b, rhs),
            (None, None) => return Ok(false),
        };
        let result_ty = self.resolve_type(&lhs.ty)?;
        if !is_float_type(&result_ty)
            || !types_compatible(&result_ty, &self.resolve_type(&product_lhs.ty)?)
            || !types_compatible(&result_ty, &self.resolve_type(&product_rhs.ty)?)
            || !types_compatible(&result_ty, &self.resolve_type(&addend.ty)?)
            || matches!(result_ty, LlType::Vector(_, lanes) if lanes > 4)
            || bfloat_lanes(&result_ty).is_some()
        {
            return Ok(false);
        }

        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(name, &result_ty)?;
        let a = self.value_id_in(&product_lhs.value, &product_lhs.ty, instructions)?;
        let b = self.value_id_in(&product_rhs.value, &product_rhs.ty, instructions)?;
        let c = self.value_id_in(&addend.value, &addend.ty, instructions)?;
        let glsl = self.glsl_ext_inst_import();
        instructions.push(Self::inst(
            Op::ExtInst,
            Some(result_type),
            Some(result),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(GlslStd450Op::Fma as u32),
                Operand::IdRef(a),
                Operand::IdRef(b),
                Operand::IdRef(c),
            ],
        ));
        Ok(true)
    }

    fn bind_inline_parameter(
        &mut self,
        name: &str,
        argument: TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let resolved_ty = self.resolve_type(&argument.ty)?;
        if let LlValue::Local(source) = &argument.value {
            if let Some(pointer_values) = self.aggregate_pointer_values.get(source).cloned() {
                self.aggregate_pointer_values
                    .insert(name.to_string(), pointer_values);
            }
        }
        if let (LlType::Ptr(_), LlValue::Local(source)) = (&resolved_ty, &argument.value) {
            if let Some(selected) = self.selected_pointers.get(source).cloned() {
                self.selected_pointers.insert(name.to_string(), selected);
                self.direct_param_values.insert(name.to_string());
                self.param_values.insert(name.to_string());
                return Ok(());
            }
            if let Some(tree) = self.selected_access_trees.get(source).cloned() {
                self.pointer_pointees
                    .insert(name.to_string(), tree.pointee.clone());
                self.selected_access_trees.insert(name.to_string(), tree);
                self.direct_param_values.insert(name.to_string());
                self.param_values.insert(name.to_string());
                return Ok(());
            }
            if let Some(selected) = self.selected_load_pointers.get(source).cloned() {
                self.pointer_pointees
                    .insert(name.to_string(), selected.pointee.clone());
                self.selected_load_pointers
                    .insert(name.to_string(), selected);
                self.direct_param_values.insert(name.to_string());
                self.param_values.insert(name.to_string());
                return Ok(());
            }
        }
        let inline_raw = match (&argument.value, &resolved_ty) {
            (LlValue::Local(source), LlType::Ptr(addrspace)) => self
                .raw_offsets
                .get(source)
                .cloned()
                .filter(|raw| {
                    !raw.unmodelable
                        && ((self.is_raw_buffer_param(name) && matches!(raw.addrspace, 1 | 2))
                            || (self.bda_device_pointers && raw.device_addr_base.is_some()))
                })
                .or_else(|| {
                    (self.bda_device_pointers)
                        .then(|| self.bda_direct_addresses.get(source).copied())
                        .flatten()
                        .map(|address| {
                            let mut raw =
                                RawBufferOffset::root(format!(".bda_inline_{address}"), *addrspace);
                            raw.device_addr_base = Some(address);
                            raw
                        })
                }),
            _ => None,
        };
        let inline_raw_storage = inline_raw.as_ref().map(|raw| {
            self.pointer_storage
                .get(&raw.root)
                .copied()
                .unwrap_or(StorageClass::StorageBuffer)
        });
        let pointer_facts = if let LlType::Ptr(addrspace) = resolved_ty {
            let storage = self.pointer_storage_for(&argument.value, addrspace)?;
            let pointee = self.pointer_pointee_for_value(&argument.value)?;
            let nullness = match &argument.value {
                LlValue::Local(source) => self.pointer_nullness.get(source).copied(),
                LlValue::Global(_) | LlValue::Gep(_) => Some(self.const_bool(false)?),
                _ => None,
            };
            Some((addrspace, storage, pointee, nullness))
        } else {
            None
        };
        let bda_argument_address = inline_raw
            .as_ref()
            .filter(|raw| self.bda_device_pointers && raw.device_addr_base.is_some())
            .map(|raw| self.materialize_device_address(raw, instructions))
            .transpose()?;
        let argument_id = if let Some(address) = bda_argument_address {
            address
        } else if let Some(raw) = inline_raw.as_ref() {
            self.value_id_in(
                &LlValue::Local(raw.root.clone()),
                &argument.ty,
                instructions,
            )?
        } else {
            self.value_id_in(&argument.value, &argument.ty, instructions)?
        };
        let placeholder_id = self.fresh();
        self.values
            .insert(name.to_string(), (placeholder_id, resolved_ty.clone()));
        self.inline_parameter_substitutions
            .push((placeholder_id, argument_id));
        self.direct_param_values.insert(name.to_string());
        self.param_values.insert(name.to_string());
        if self.bda_device_pointers {
            if let LlValue::Local(source) = &argument.value {
                if let Some(addresses) = self.bda_aggregate_addresses.get(source).cloned() {
                    self.bda_aggregate_addresses
                        .insert(name.to_string(), addresses);
                }
            }
        }

        if let Some((addrspace, storage, pointee, nullness)) = pointer_facts {
            if self.bda_device_pointers {
                if let LlValue::Local(source) = &argument.value {
                    let address = self.bda_direct_addresses.get(source).copied().or_else(|| {
                        self.raw_offsets.get(source).and_then(|raw| {
                            raw.device_addr_base
                                .or_else(|| self.bda_direct_addresses.get(&raw.root).copied())
                        })
                    });
                    if let Some(address) = address {
                        self.bda_direct_addresses.insert(name.to_string(), address);
                    }
                }
            }
            self.pointer_storage.insert(name.to_string(), storage);
            if let Some(pointee) = pointee {
                self.pointer_pointees.insert(name.to_string(), pointee);
            }
            let argument_provenance = self.values.iter().find_map(|(candidate, (id, _))| {
                (*id == argument_id)
                    .then(|| self.gep_provenance.get(candidate).cloned())
                    .flatten()
            });
            if let Some(provenance) = argument_provenance {
                self.gep_provenance.insert(name.to_string(), provenance);
            } else if let LlValue::Gep(gep) = &argument.value {
                let base_id = self.value_id_in(&gep.base.value, &gep.base.ty, instructions)?;
                let source_ty = self.resolve_type(&gep.source_ty)?;
                self.gep_provenance.insert(
                    name.to_string(),
                    GepProvenance {
                        root: base_id,
                        addrspace,
                        source_ty,
                        indices: gep.indices.clone(),
                        root_indices: None,
                        root_is_indexed_container: self
                            .is_indexed_container_root(base_id, Some(storage)),
                    },
                );
            } else if let LlValue::Local(source) = &argument.value {
                if let Some(provenance) = self.gep_provenance.get(source).cloned() {
                    self.gep_provenance.insert(name.to_string(), provenance);
                }
            }
            if let Some(nullness) = nullness {
                self.record_pointer_nullness(name.to_string(), nullness);
            }
            if let Some(mut raw) = inline_raw
                .filter(|raw| self.is_raw_buffer_param(name) || raw.device_addr_base.is_some())
            {
                raw.root = name.to_string();
                self.raw_offsets.insert(name.to_string(), raw);
                if let Some(storage) = inline_raw_storage {
                    self.pointer_storage.insert(name.to_string(), storage);
                }
            } else if self.is_raw_buffer_param(name) {
                self.raw_offsets.insert(
                    name.to_string(),
                    RawBufferOffset::root(name.to_string(), addrspace),
                );
            }
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_alloca_typed(
        &mut self,
        name: String,
        alloca_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let pointee = self.resolve_type(alloca_ty)?;
        let effective_pointee = self
            .local_alloca_pointees
            .get(&name)
            .cloned()
            .map(|ty| self.resolve_type(&ty))
            .transpose()?
            .filter(|candidate| self.local_alloca_storage_compatible(&pointee, candidate))
            .unwrap_or_else(|| pointee.clone());
        let storage_pointee = function_storage_local_type(&effective_pointee);
        let ptr_type = self.ptr_type_id(StorageClass::Function, &storage_pointee)?;
        let result = self.result_id(&name, &LlType::Ptr(0))?;
        instructions.push(Self::inst(
            Op::Variable,
            Some(ptr_type),
            Some(result),
            vec![Operand::StorageClass(StorageClass::Function)],
        ));
        self.pointer_storage
            .insert(name.clone(), StorageClass::Function);
        self.pointer_pointees
            .insert(name.clone(), effective_pointee);
        let is_null = self.const_bool(false)?;
        self.record_pointer_nullness(name, is_null);
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_shufflevector_from_mask(
        &mut self,
        a: TypedValue,
        b: TypedValue,
        declared_lanes: u32,
        lanes: Vec<u32>,
        line: &str,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let LlType::Vector(elem, _) = &a.ty else {
            return Err(format!(
                "native emitter: shufflevector first operand is not a vector: {:?}",
                a.ty
            ));
        };
        let result_ty = LlType::Vector(elem.clone(), declared_lanes);
        self.emit_shufflevector_with(a, b, result_ty, lanes, line, name, instructions)
    }

    pub(in crate::native::emitter) fn emit_shufflevector_with(
        &mut self,
        a: TypedValue,
        b: TypedValue,
        result_ty: LlType,
        lanes: Vec<u32>,
        line: &str,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if let Some(elem) = self.one_lane_vector_elem(&result_ty)? {
            let Some(lane) = lanes.first().copied() else {
                return Err(format!("native emitter: empty one-lane shuffle: {line}"));
            };
            let result_type = self.type_id(&elem)?;
            let result = self.result_id(&name, &result_ty)?;
            let value = self.shuffled_lane_id(&a, &b, lane, &elem, instructions)?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
            return Ok(());
        }
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let a_lanes = self.vector_lane_count(&a.ty)?;
        let b_lanes = self.vector_lane_count(&b.ty)?;
        if lanes.len() > 4 || a_lanes > 4 || b_lanes > 4 {
            let LlType::Vector(elem, _) = self.resolve_type(&result_ty)? else {
                return Err(format!(
                    "native emitter: shufflevector result is not a vector: {result_ty:?}"
                ));
            };
            let mut ops = Vec::with_capacity(lanes.len());
            for lane in lanes {
                ops.push(Operand::IdRef(self.shuffled_lane_id(
                    &a,
                    &b,
                    lane,
                    &elem,
                    instructions,
                )?));
            }
            instructions.push(Self::inst(
                Op::CompositeConstruct,
                Some(result_type),
                Some(result),
                ops,
            ));
            return Ok(());
        }
        let mut ops = vec![
            Operand::IdRef(self.value_id_in(&a.value, &a.ty, instructions)?),
            Operand::IdRef(self.value_id_in(&b.value, &b.ty, instructions)?),
        ];
        ops.extend(lanes.into_iter().map(Operand::LiteralBit32));
        instructions.push(Self::inst(
            Op::VectorShuffle,
            Some(result_type),
            Some(result),
            ops,
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_insertvalue_typed(
        &mut self,
        composite: TypedValue,
        object: TypedValue,
        indices: &[u32],
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(&composite.ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let mut pointer_values = match &composite.value {
            LlValue::Local(composite_name) => self
                .aggregate_pointer_values
                .get(composite_name)
                .cloned()
                .unwrap_or_default(),
            _ => HashMap::new(),
        };
        pointer_values.retain(|path, _| !path.starts_with(indices));
        if matches!(object.ty, LlType::Ptr(_)) {
            pointer_values.insert(indices.to_vec(), object.clone());
        }
        if let LlValue::Local(object_name) = &object.value {
            if let Some(nested) = self.aggregate_pointer_values.get(object_name) {
                for (path, pointer) in nested {
                    let mut aggregate_path = indices.to_vec();
                    aggregate_path.extend(path);
                    pointer_values.insert(aggregate_path, pointer.clone());
                }
            }
        }
        if !self.bda_device_pointers {
            for (path, pointer) in &pointer_values {
                let source = self.value_id_in(&pointer.value, &pointer.ty, instructions)?;
                self.emit_sidecar.aggregate_pointer_values.push(
                    crate::emit_sidecar::AggregatePointerValue {
                        aggregate: result,
                        source,
                        indices: path.clone(),
                    },
                );
            }
        }
        if !pointer_values.is_empty() {
            self.aggregate_pointer_values
                .insert(name.clone(), pointer_values);
        }
        if self.bda_device_pointers {
            let mut addresses = match &composite.value {
                LlValue::Local(composite_name) => self
                    .bda_aggregate_addresses
                    .get(composite_name)
                    .cloned()
                    .unwrap_or_default(),
                _ => HashMap::new(),
            };
            addresses.retain(|path, _| !path.starts_with(indices));
            if let LlValue::Local(object_name) = &object.value {
                if let Some(address) = self.bda_direct_addresses.get(object_name).copied() {
                    addresses.insert(indices.to_vec(), address);
                }
                if let Some(nested) = self.bda_aggregate_addresses.get(object_name) {
                    for (path, address) in nested {
                        let mut aggregate_path = indices.to_vec();
                        aggregate_path.extend(path);
                        addresses.insert(aggregate_path, *address);
                    }
                }
            }
            if !addresses.is_empty() {
                self.bda_aggregate_addresses.insert(name.clone(), addresses);
            }
        }
        let result_type = self.type_id(&result_ty)?;
        let object_id = if matches!(object.ty, LlType::Ptr(_)) {
            if self.bda_device_pointers {
                match &object.value {
                    LlValue::Local(object_name) => self
                        .bda_direct_addresses
                        .get(object_name)
                        .copied()
                        .unwrap_or(self.const_signed_int(64, 0)?),
                    _ => self.const_signed_int(64, 0)?,
                }
            } else {
                self.const_signed_int(64, 0)?
            }
        } else {
            self.value_id_in(&object.value, &object.ty, instructions)?
        };
        let mut ops = vec![
            Operand::IdRef(object_id),
            Operand::IdRef(self.value_id_in(&composite.value, &composite.ty, instructions)?),
        ];
        ops.extend(indices.iter().copied().map(Operand::LiteralBit32));
        instructions.push(Self::inst(
            Op::CompositeInsert,
            Some(result_type),
            Some(result),
            ops,
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_extractvalue_typed(
        &mut self,
        composite: TypedValue,
        indices: &[u32],
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = extract_value_type(&self.resolve_type(&composite.ty)?, indices)?;
        if matches!(result_ty, LlType::Ptr(_)) {
            if let LlValue::Local(composite_name) = &composite.value {
                if let Some(pointer) = self
                    .aggregate_pointer_values
                    .get(composite_name)
                    .and_then(|pointers| pointers.get(indices))
                    .cloned()
                {
                    self.bind_aggregate_pointer_extract(&name, &result_ty, &pointer, instructions)?;
                    return Ok(());
                }
            }
        }
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let composite_id = self.value_id_in(&composite.value, &composite.ty, instructions)?;
        if let LlValue::Local(composite_name) = &composite.value {
            if let Some(pointers) = self.aggregate_pointer_values.get(composite_name) {
                let nested = pointers
                    .iter()
                    .filter_map(|(path, pointer)| {
                        path.strip_prefix(indices)
                            .filter(|suffix| !suffix.is_empty())
                            .map(|suffix| (suffix.to_vec(), pointer.clone()))
                    })
                    .collect::<HashMap<_, _>>();
                if !nested.is_empty() {
                    if !self.bda_device_pointers {
                        for (path, pointer) in &nested {
                            let source =
                                self.value_id_in(&pointer.value, &pointer.ty, instructions)?;
                            self.emit_sidecar.aggregate_pointer_values.push(
                                crate::emit_sidecar::AggregatePointerValue {
                                    aggregate: result,
                                    source,
                                    indices: path.clone(),
                                },
                            );
                        }
                    }
                    self.aggregate_pointer_values.insert(name.clone(), nested);
                }
            }
        }
        if self.bda_device_pointers {
            if let LlValue::Local(composite_name) = &composite.value {
                if let Some(addresses) = self.bda_aggregate_addresses.get(composite_name).cloned() {
                    if let Some(address) = addresses.get(indices).copied() {
                        self.bda_direct_addresses.insert(name.clone(), address);
                    }
                    let nested = addresses
                        .into_iter()
                        .filter_map(|(path, address)| {
                            path.strip_prefix(indices)
                                .filter(|suffix| !suffix.is_empty())
                                .map(|suffix| (suffix.to_vec(), address))
                        })
                        .collect::<HashMap<_, _>>();
                    if !nested.is_empty() {
                        self.bda_aggregate_addresses.insert(name.clone(), nested);
                    }
                }
            }
        }
        let result_type = if self.bda_direct_addresses.contains_key(&name) {
            self.type_id(&LlType::Int(64))?
        } else {
            result_type
        };
        let mut ops = vec![Operand::IdRef(composite_id)];
        ops.extend(indices.iter().copied().map(Operand::LiteralBit32));
        instructions.push(Self::inst(
            Op::CompositeExtract,
            Some(result_type),
            Some(result),
            ops,
        ));
        if let LlType::Ptr(addrspace) = result_ty {
            if self.bda_direct_addresses.contains_key(&name) {
                let mut raw = RawBufferOffset::root(format!(".bda_{result}"), addrspace);
                raw.device_addr_base = Some(result);
                self.raw_offsets.insert(name.clone(), raw);
                self.pointer_storage
                    .insert(name, StorageClass::PhysicalStorageBuffer);
            } else {
                self.pointer_storage
                    .insert(name, llvm_pointer_storage(addrspace)?);
            }
        }
        Ok(())
    }

    fn bind_aggregate_pointer_extract(
        &mut self,
        name: &str,
        result_ty: &LlType,
        pointer: &TypedValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let source = self.value_id_in(&pointer.value, &pointer.ty, instructions)?;
        self.values
            .insert(name.to_string(), (source, result_ty.clone()));
        if let LlValue::Local(source_name) = &pointer.value {
            if let Some(raw) = self.raw_offsets.get(source_name).cloned() {
                self.raw_offsets.insert(name.to_string(), raw);
            }
            if let Some(storage) = self.pointer_storage.get(source_name).copied() {
                self.pointer_storage.insert(name.to_string(), storage);
            }
            if let Some(pointee) = self.pointer_pointees.get(source_name).cloned() {
                self.pointer_pointees.insert(name.to_string(), pointee);
            }
            if let Some(nullness) = self.pointer_nullness.get(source_name).copied() {
                self.record_pointer_nullness(name.to_string(), nullness);
            }
            if let Some(provenance) = self.gep_provenance.get(source_name).cloned() {
                self.gep_provenance.insert(name.to_string(), provenance);
            }
            if let Some(address) = self.bda_direct_addresses.get(source_name).copied() {
                self.bda_direct_addresses.insert(name.to_string(), address);
            }
            if self.unmodeled_pointers.contains(source_name) {
                self.unmodeled_pointers.insert(name.to_string());
            }
            if self.byte_view_pointers.contains(source_name) {
                self.byte_view_pointers.insert(name.to_string());
            }
            if self.param_values.contains(source_name) {
                self.param_values.insert(name.to_string());
            }
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_extractelement_resolved(
        &mut self,
        vector: TypedValue,
        idx: TypedValue,
        name: String,
        line: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let LlType::Vector(elem, lanes) = self.resolve_type(&vector.ty)? else {
            if let Some(elem) = self.one_lane_vector_elem(&vector.ty)? {
                if const_index(Some(&idx)).is_some_and(|idx| idx != 0) {
                    return Err(format!(
                        "native emitter: one-lane extractelement index is not zero: {line}"
                    ));
                }
                let result_type = self.type_id(&elem)?;
                let result = self.result_id(&name, &elem)?;
                let vector_id = self.value_id_in(&vector.value, &vector.ty, instructions)?;
                instructions.push(Self::inst(
                    Op::CopyObject,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(vector_id)],
                ));
                return Ok(());
            } else {
                return Err(format!(
                    "native emitter: extractelement from non-vector: {line}"
                ));
            }
        };
        let result_ty = *elem;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let vector_id = self.value_id_in(&vector.value, &vector.ty, instructions)?;
        if let Some(idx) = const_index(Some(&idx)) {
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(vector_id), Operand::LiteralBit32(idx)],
            ));
        } else {
            let idx_id = self.vector_index_id(&idx, instructions)?;
            if lanes <= 4 {
                instructions.push(Self::inst(
                    Op::VectorExtractDynamic,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(vector_id), Operand::IdRef(idx_id)],
                ));
            } else {
                self.emit_large_vector_dynamic_extract(
                    vector_id,
                    idx_id,
                    &idx.ty,
                    lanes,
                    result_type,
                    result,
                    instructions,
                )?;
            }
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_insertelement_resolved(
        &mut self,
        composite: TypedValue,
        object: TypedValue,
        idx: TypedValue,
        name: String,
        line: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(&composite.ty)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        if self.one_lane_vector_elem(&composite.ty)?.is_some() {
            if const_index(Some(&idx)).is_some_and(|idx| idx != 0) {
                return Err(format!(
                    "native emitter: one-lane insertelement index is not zero: {line}"
                ));
            }
            let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(object_id)],
            ));
            return Ok(());
        }
        let vector_id = self.value_id_in(&composite.value, &composite.ty, instructions)?;
        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        if let Some(idx) = const_index(Some(&idx)) {
            instructions.push(Self::inst(
                Op::CompositeInsert,
                Some(result_type),
                Some(result),
                vec![
                    Operand::IdRef(object_id),
                    Operand::IdRef(vector_id),
                    Operand::LiteralBit32(idx),
                ],
            ));
        } else {
            let idx_id = self.vector_index_id(&idx, instructions)?;
            if let LlType::Vector(_, lanes) = &result_ty {
                if *lanes > 4 {
                    self.emit_large_vector_dynamic_insert(
                        vector_id,
                        object_id,
                        idx_id,
                        &idx.ty,
                        *lanes,
                        result_type,
                        result,
                        instructions,
                    )?;
                } else {
                    instructions.push(Self::inst(
                        Op::VectorInsertDynamic,
                        Some(result_type),
                        Some(result),
                        vec![
                            Operand::IdRef(vector_id),
                            Operand::IdRef(object_id),
                            Operand::IdRef(idx_id),
                        ],
                    ));
                }
            } else {
                instructions.push(Self::inst(
                    Op::VectorInsertDynamic,
                    Some(result_type),
                    Some(result),
                    vec![
                        Operand::IdRef(vector_id),
                        Operand::IdRef(object_id),
                        Operand::IdRef(idx_id),
                    ],
                ));
            }
        }
        Ok(())
    }

    fn dynamic_index_const(&mut self, idx_ty: &LlType, value: u32) -> Result<Word, String> {
        match self.resolve_type(idx_ty)? {
            LlType::Bool => self.const_uint(value),
            LlType::Int(bits) => self.const_int(bits, u64::from(value)),
            other => Err(format!(
                "native emitter: dynamic vector index has unsupported type {other:?}"
            )),
        }
    }

    fn emit_large_vector_dynamic_extract(
        &mut self,
        vector_id: Word,
        idx_id: Word,
        idx_ty: &LlType,
        lanes: u32,
        result_type: Word,
        result: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let bool_ty = self.type_id(&LlType::Bool)?;
        let mut selected = None;
        for lane in 0..lanes {
            let extracted = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeExtract,
                Some(result_type),
                Some(extracted),
                vec![Operand::IdRef(vector_id), Operand::LiteralBit32(lane)],
            ));
            let Some(prev) = selected else {
                selected = Some(extracted);
                continue;
            };
            let lane_id = self.dynamic_index_const(idx_ty, lane)?;
            let cmp = self.fresh();
            instructions.push(Self::inst(
                Op::IEqual,
                Some(bool_ty),
                Some(cmp),
                vec![Operand::IdRef(idx_id), Operand::IdRef(lane_id)],
            ));
            let select = if lane + 1 == lanes {
                result
            } else {
                self.fresh()
            };
            instructions.push(Self::inst(
                Op::Select,
                Some(result_type),
                Some(select),
                vec![
                    Operand::IdRef(cmp),
                    Operand::IdRef(extracted),
                    Operand::IdRef(prev),
                ],
            ));
            selected = Some(select);
        }
        if lanes == 1 {
            let selected = selected.ok_or("native emitter: empty large vector extract")?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(selected)],
            ));
        }
        Ok(())
    }

    fn emit_large_vector_dynamic_insert(
        &mut self,
        vector_id: Word,
        object_id: Word,
        idx_id: Word,
        idx_ty: &LlType,
        lanes: u32,
        result_type: Word,
        result: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let bool_ty = self.type_id(&LlType::Bool)?;
        let mut selected = None;
        for lane in 0..lanes {
            let candidate = self.fresh();
            instructions.push(Self::inst(
                Op::CompositeInsert,
                Some(result_type),
                Some(candidate),
                vec![
                    Operand::IdRef(object_id),
                    Operand::IdRef(vector_id),
                    Operand::LiteralBit32(lane),
                ],
            ));
            let Some(prev) = selected else {
                selected = Some(candidate);
                continue;
            };
            let lane_id = self.dynamic_index_const(idx_ty, lane)?;
            let cmp = self.fresh();
            instructions.push(Self::inst(
                Op::IEqual,
                Some(bool_ty),
                Some(cmp),
                vec![Operand::IdRef(idx_id), Operand::IdRef(lane_id)],
            ));
            let select = if lane + 1 == lanes {
                result
            } else {
                self.fresh()
            };
            instructions.push(Self::inst(
                Op::Select,
                Some(result_type),
                Some(select),
                vec![
                    Operand::IdRef(cmp),
                    Operand::IdRef(candidate),
                    Operand::IdRef(prev),
                ],
            ));
            selected = Some(select);
        }
        if lanes == 1 {
            let selected = selected.ok_or("native emitter: empty large vector insert")?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(selected)],
            ));
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_inttoptr_resolved(
        &mut self,
        src: TypedValue,
        dst_ty: LlType,
        name: String,
        _instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if self.opaque_resource_pointers.contains(&name) && self.values.contains_key(&name) {
            return Ok(());
        }
        let src_ty = self.resolve_type(&src.ty)?;
        let LlType::Int(_) = src_ty else {
            return Err(format!(
                "native emitter: inttoptr source is not integer: {src_ty:?}"
            ));
        };
        let LlType::Ptr(addrspace) = dst_ty else {
            return Err(format!(
                "native emitter: inttoptr destination is not pointer: {dst_ty:?}"
            ));
        };
        let src_id = self.value_id(&src.value, &src.ty)?;
        if self.bda_device_pointers && addrspace == 1 {
            self.used_device_address = true;
            self.emit_device_address_nullness(&name, src_id, _instructions)?;
            let mut dev = RawBufferOffset::root(format!(".bda_inttoptr_{src_id}"), 1);
            dev.device_addr_base = Some(src_id);
            self.raw_offsets.insert(name.clone(), dev);
            self.pointer_storage
                .insert(name.clone(), StorageClass::PhysicalStorageBuffer);
            self.pointer_pointees.insert(name, LlType::Int(8));
            return Ok(());
        }
        if let LlValue::Local(src_name) = &src.value {
            if let Some(base) = self.symbolic_buffer_addresses.get(src_name).cloned() {
                if base.addrspace == addrspace {
                    if !self.root_is_word_addressable(&base.root) {
                        return Err(format!(
                            "native emitter: buffer address round trip through `{name}` on the \
                             untyped byte root `{}`",
                            base.root
                        ));
                    }
                    let storage = self.raw_access_storage(&base)?;
                    self.define_unmodeled_pointer_value(&name, addrspace, &LlType::Int(8))?;
                    self.raw_offsets.insert(name.clone(), base);
                    self.pointer_storage.insert(name.clone(), storage);
                    return Ok(());
                }
            }
        }
        self.define_unmodeled_pointer_value(&name, addrspace, &LlType::Int(8))?;
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_device_address_nullness(
        &mut self,
        name: &str,
        address: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let bool_ty = self.type_id(&LlType::Bool)?;
        let zero = self.const_signed_int(64, 0)?;
        let is_null = self.result_id(&pointer_null_name(name), &LlType::Bool)?;
        instructions.push(Self::inst(
            Op::IEqual,
            Some(bool_ty),
            Some(is_null),
            vec![Operand::IdRef(address), Operand::IdRef(zero)],
        ));
        self.record_pointer_nullness(name.to_string(), is_null);
        Ok(())
    }

    pub(in crate::native::emitter) fn combine_pointer_payload_words(
        &mut self,
        low: Word,
        high: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let i64_ty = LlType::Int(64);
        let result_type = self.type_id(&i64_ty)?;
        let low64 = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(result_type),
            Some(low64),
            vec![Operand::IdRef(low)],
        ));
        let high64 = self.fresh();
        instructions.push(Self::inst(
            Op::UConvert,
            Some(result_type),
            Some(high64),
            vec![Operand::IdRef(high)],
        ));
        let shifted_high = self.fresh();
        let shift = self.const_signed_int(64, 32)?;
        instructions.push(Self::inst(
            Op::ShiftLeftLogical,
            Some(result_type),
            Some(shifted_high),
            vec![Operand::IdRef(high64), Operand::IdRef(shift)],
        ));
        let address = self.fresh();
        instructions.push(Self::inst(
            Op::BitwiseOr,
            Some(result_type),
            Some(address),
            vec![Operand::IdRef(low64), Operand::IdRef(shifted_high)],
        ));
        Ok(address)
    }

    pub(in crate::native::emitter) fn emit_ptrtoint_resolved(
        &mut self,
        src: TypedValue,
        dst_ty: LlType,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let src_ty = self.resolve_type(&src.ty)?;
        let LlType::Ptr(_) = src_ty else {
            return Err(format!(
                "native emitter: ptrtoint source is not pointer: {src_ty:?}"
            ));
        };
        let LlType::Int(_) = dst_ty else {
            return Err(format!(
                "native emitter: ptrtoint destination is not integer: {dst_ty:?}"
            ));
        };
        if let LlValue::Local(src_name) = &src.value {
            if let Some((low, high)) = self.pointer_payload_words.get(src_name).copied() {
                let LlType::Int(bits) = dst_ty else {
                    unreachable!()
                };
                let result_type = self.type_id(&dst_ty)?;
                let result = self.result_id(&name, &dst_ty)?;
                if bits == 32 {
                    instructions.push(Self::inst(
                        Op::CopyObject,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(low)],
                    ));
                    return Ok(());
                }
                if bits == 64 {
                    let low64 = self.fresh();
                    instructions.push(Self::inst(
                        Op::UConvert,
                        Some(result_type),
                        Some(low64),
                        vec![Operand::IdRef(low)],
                    ));
                    let high64 = self.fresh();
                    instructions.push(Self::inst(
                        Op::UConvert,
                        Some(result_type),
                        Some(high64),
                        vec![Operand::IdRef(high)],
                    ));
                    let shifted_high = self.fresh();
                    let shift = self.const_signed_int(64, 32)?;
                    instructions.push(Self::inst(
                        Op::ShiftLeftLogical,
                        Some(result_type),
                        Some(shifted_high),
                        vec![Operand::IdRef(high64), Operand::IdRef(shift)],
                    ));
                    instructions.push(Self::inst(
                        Op::BitwiseOr,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(low64), Operand::IdRef(shifted_high)],
                    ));
                    return Ok(());
                }
                return Err(format!(
                    "native emitter: serialized pointer payload cannot convert to i{bits}"
                ));
            }
        }
        if let LlValue::Local(src_name) = &src.value {
            if let Some(base) = self.symbolic_buffer_address_for_pointer(src_name) {
                self.symbolic_buffer_addresses.insert(name.clone(), base);
            }
        }
        let _ = self.value_id(&src.value, &src.ty)?;
        let zero = self.const_null(&dst_ty)?;
        let result = self.result_id(&name, &dst_ty)?;
        instructions.push(Self::inst(
            Op::CopyObject,
            Some(self.type_id(&dst_ty)?),
            Some(result),
            vec![Operand::IdRef(zero)],
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_store_resolved(
        &mut self,
        object: TypedValue,
        ptr: TypedValue,
        align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let b78_hint = b78_air_access_hint(&ptr.ty, align, &object.ty);
        let b78_start = instructions.len();
        let emitted = self.emit_store_resolved_b78_inner(object, ptr, align, instructions);
        if emitted.is_ok() {
            self.b78_record_access_align(b78_hint, instructions.get(b78_start..).unwrap_or(&[]));
        }
        emitted
    }

    fn emit_store_resolved_b78_inner(
        &mut self,
        object: TypedValue,
        ptr: TypedValue,
        align: Option<u64>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if let LlValue::Local(ptr_name) = &ptr.value {
            if let Some(padding) = self.workgroup_padding_byte_pointers.get(ptr_name).cloned() {
                let object_ty = self.resolve_type(&object.ty)?;
                let (store_size, _) = self.raw_type_size_align(&object_ty)?;
                if !typed_value_is_zero(&object) {
                    return Err(
                        "native emitter: non-zero store through a symbolic Workgroup padding pointer"
                            .to_string(),
                    );
                }
                if !self.struct_range_is_padding(
                    &padding.struct_ty,
                    padding.byte_offset,
                    store_size,
                )? {
                    return Err(format!(
                        "native emitter: zero store from Workgroup struct padding offset {} spans non-padding bytes",
                        padding.byte_offset
                    ));
                }
                return Ok(());
            }
            if let Some(vector_word) = self.vector_word_pointers.get(ptr_name).cloned() {
                let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
                self.emit_vector_word_store(&vector_word, &object.ty, object_id, instructions)?;
                return Ok(());
            }
            if let Some(tree) = self.selected_access_trees.get(ptr_name).cloned() {
                let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
                self.emit_selected_access_tree_store(
                    &object.ty,
                    object_id,
                    &tree,
                    align,
                    instructions,
                )?;
                return Ok(());
            }
            if let Some(selected) = self.selected_load_pointers.get(ptr_name).cloned() {
                let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
                self.emit_selected_pointer_store(
                    &object.ty,
                    object_id,
                    &selected,
                    align,
                    instructions,
                )?;
                return Ok(());
            }
            if let Some(raw) = self.raw_offsets.get(ptr_name).cloned() {
                if matches!(object.ty, LlType::Ptr(_)) {
                    if let LlValue::Local(obj_name) = &object.value {
                        if !self.pointer_payload_words.contains_key(obj_name)
                            && self.direct_param_indices.contains_key(obj_name)
                        {
                            let payload =
                                self.emit_direct_buffer_address_payload(obj_name, instructions)?;
                            self.pointer_payload_words.insert(obj_name.clone(), payload);
                        }
                        if let Some((low, high)) = self.pointer_payload_words.get(obj_name).copied()
                        {
                            self.emit_raw_word_store_for_access(&raw, 0, low, align, instructions)?;
                            self.emit_raw_word_store_for_access(
                                &raw,
                                4,
                                high,
                                align,
                                instructions,
                            )?;
                            return Ok(());
                        }
                    }
                }
                if self.bda_device_pointers && matches!(object.ty, LlType::Ptr(1)) {
                    if let LlValue::Local(obj_name) = &object.value {
                        if let Some(src) = self.raw_offsets.get(obj_name).cloned() {
                            if src.device_addr_base.is_some() {
                                let addr = self.materialize_device_address(&src, instructions)?;
                                self.emit_raw_store(
                                    &LlType::Int(64),
                                    addr,
                                    &raw,
                                    align,
                                    instructions,
                                )?;
                                return Ok(());
                            }
                        }
                    }
                }
                let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
                self.emit_raw_store(&object.ty, object_id, &raw, align, instructions)?;
                return Ok(());
            }
            if let Some(selected) = self.selected_pointers.get(ptr_name).cloned() {
                let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
                self.emit_selected_pointer_direct_store(
                    &object.ty,
                    object_id,
                    &selected,
                    instructions,
                )?;
                return Ok(());
            }
        }
        if let Some(pointee) = self.pointer_pointee_for_value(&ptr.value)? {
            let pointee = self.resolve_type(&pointee)?;
            if self.emit_pointer_to_local_field_store(&object, &ptr, &pointee, instructions)? {
                return Ok(());
            }
        }
        if let Some(raw) = self.byte_array_reinterpret_raw_pointer(&ptr.value)? {
            let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
            self.emit_raw_store(&object.ty, object_id, &raw, align, instructions)?;
            return Ok(());
        }
        if self.emit_vector_root_store(&ptr, &object, instructions)? {
            return Ok(());
        }
        if let LlValue::Local(name) = &ptr.value {
            if self.unmodeled_pointers.contains(name) {
                return Err(format!(
                    "native emitter: store through {name}, an unmodeled pointer placeholder that                      addresses nothing; the write would be lost"
                ));
            }
        }
        if let Some(pointee) = self.pointer_pointee_for_value(&ptr.value)? {
            let pointee = self.resolve_type(&pointee)?;
            let object_ty = self.resolve_type(&object.ty)?;
            if self.bda_device_pointers && pointee == LlType::Int(64) && object_ty == LlType::Ptr(1)
            {
                if let LlValue::Local(object_name) = &object.value {
                    let address = if let Some(address) =
                        self.bda_direct_addresses.get(object_name).copied()
                    {
                        Some(address)
                    } else if let Some(raw) = self.raw_offsets.get(object_name).cloned() {
                        raw.device_addr_base
                            .map(|_| self.materialize_device_address(&raw, instructions))
                            .transpose()?
                    } else if self.direct_param_indices.contains_key(object_name) {
                        let (low, high) =
                            self.emit_direct_buffer_address_payload(object_name, instructions)?;
                        self.pointer_payload_words
                            .insert(object_name.clone(), (low, high));
                        Some(self.combine_pointer_payload_words(low, high, instructions)?)
                    } else {
                        None
                    };
                    if let Some(address) = address {
                        let pointer = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
                        instructions.push(Self::inst(
                            Op::Store,
                            None,
                            None,
                            vec![Operand::IdRef(pointer), Operand::IdRef(address)],
                        ));
                        return Ok(());
                    }
                }
            }
            if self.emit_mismatched_store(
                &object,
                &ptr,
                &object_ty,
                &pointee,
                align,
                instructions,
            )? {
                return Ok(());
            }
        }
        let resolved_object_ty = self.resolve_type(&object.ty)?;
        if self.emit_trailing_byte_array_value_store(
            &object,
            &ptr,
            &resolved_object_ty,
            instructions,
        )? {
            return Ok(());
        }
        let ptr_id = self.value_id_in(&ptr.value, &ptr.ty, instructions)?;
        let object_id = self.value_id_in(&object.value, &object.ty, instructions)?;
        instructions.push(Self::inst(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(ptr_id), Operand::IdRef(object_id)],
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_void_call_body(
        &mut self,
        call: LlCall,
        line: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if self.emit_zero_memset(&call, instructions)? {
            return Ok(());
        }
        if self.emit_raw_memcpy(&call, instructions)? {
            return Ok(());
        }
        if self.emit_typed_memcpy(&call, instructions)? {
            return Ok(());
        }
        if self.drop_unmodeled_memcpy(&call) {
            return Ok(());
        }
        let result_ty = self.resolve_type(&call.ret)?;
        if result_ty != LlType::Void {
            return Err(format!(
                "native emitter: non-void call without result is not covered yet: {line}"
            ));
        }
        if self.emit_void_air_call(&call, instructions)? {
            return Ok(());
        }
        self.validate_call_args(&call, instructions)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.fresh();
        if let Some(symbol) = call
            .callee
            .strip_suffix(crate::linked_functions::UNRESOLVED_VISIBLE_SUFFIX)
        {
            return Err(format!(
                "native emitter: visible function {symbol:?} is called on a live path but no linked function supplies it"
            ));
        }
        let callee = *self
            .function_ids
            .get(&call.callee)
            .ok_or_else(|| format!("native emitter: unknown callee @{}", call.callee))?;
        let mut ops = vec![Operand::IdRef(callee)];
        for arg in self.function_call_arg_ids(&call, instructions)? {
            ops.push(Operand::IdRef(arg));
        }
        instructions.push(Self::inst(
            Op::FunctionCall,
            Some(result_type),
            Some(result),
            ops,
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_value_call_resolved(
        &mut self,
        name: String,
        call: LlCall,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        if self.emit_mtl_force_not_checked_load_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_visible_function_table_placeholder_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_fshl_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_cttz_i32_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_ctpop_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_bitreverse_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_abs_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_usub_sat_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_air_saturating_add_sub_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_air_halving_add_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_llvm_int_minmax_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_imageblock_data_call(&call, &name, instructions)? {
            return Ok(());
        }
        if self.emit_value_air_call(&call, &name, instructions)? {
            return Ok(());
        }
        let result_ty = self.resolve_type(&call.ret)?;
        self.validate_call_args(&call, instructions)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        if let Some(symbol) = call
            .callee
            .strip_suffix(crate::linked_functions::UNRESOLVED_VISIBLE_SUFFIX)
        {
            return Err(format!(
                "native emitter: visible function {symbol:?} is called on a live path but no linked function supplies it"
            ));
        }
        let callee = *self
            .function_ids
            .get(&call.callee)
            .ok_or_else(|| format!("native emitter: unknown callee @{}", call.callee))?;
        let mut ops = vec![Operand::IdRef(callee)];
        for arg in self.function_call_arg_ids(&call, instructions)? {
            ops.push(Operand::IdRef(arg));
        }
        instructions.push(Self::inst(
            Op::FunctionCall,
            Some(result_type),
            Some(result),
            ops,
        ));
        if let LlType::Ptr(addrspace) = result_ty {
            self.pointer_storage
                .insert(name, llvm_pointer_storage(addrspace)?);
        }
        Ok(())
    }

    fn alias_pointer_facts(&mut self, src_name: &str, name: &str) {
        if let Some(storage) = self.pointer_storage.get(src_name).copied() {
            self.pointer_storage.insert(name.to_string(), storage);
        }
        if let Some(is_null) = self.pointer_nullness.get(src_name).copied() {
            self.record_pointer_nullness(name.to_string(), is_null);
        }
        if let Some(pointee) = self.pointer_pointees.get(src_name).cloned() {
            self.pointer_pointees.insert(name.to_string(), pointee);
        }
        if self.param_values.contains(src_name) {
            self.param_values.insert(name.to_string());
        }
    }

    pub(in crate::native::emitter) fn emit_bitcast_resolved(
        &mut self,
        src: TypedValue,
        dst_text: &str,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let dst_ty = self.convert_dst_type(&name, dst_text)?;
        let src_ty = self.resolve_type(&src.ty)?;
        if matches!((&src_ty, &dst_ty), (LlType::Ptr(_), LlType::Ptr(_))) {
            if let LlValue::Local(src_name) = &src.value {
                if let Some(padding) = self.workgroup_padding_byte_pointers.get(src_name).cloned() {
                    self.workgroup_padding_byte_pointers
                        .insert(name.clone(), padding);
                    self.pointer_storage
                        .insert(name.clone(), StorageClass::Workgroup);
                    self.pointer_pointees.insert(name, LlType::Int(8));
                    return Ok(());
                }
                if let Some(tree) = self.selected_access_trees.get(src_name).cloned() {
                    self.selected_access_trees.insert(name.clone(), tree);
                    if let Some(pointee) = self.pointer_pointees.get(src_name).cloned() {
                        self.pointer_pointees.insert(name.clone(), pointee);
                    }
                    if self.param_values.contains(src_name) {
                        self.param_values.insert(name.clone());
                    }
                    return Ok(());
                }
                if let Some(selected) = self.selected_load_pointers.get(src_name).cloned() {
                    self.selected_load_pointers.insert(name.clone(), selected);
                    self.alias_pointer_facts(src_name, &name);
                    return Ok(());
                }
                if let Some(selected) = self.selected_pointers.get(src_name).cloned() {
                    self.selected_pointers.insert(name.clone(), selected);
                    self.alias_pointer_facts(src_name, &name);
                    return Ok(());
                }
                if let Some(raw) = self.raw_offsets.get(src_name).cloned() {
                    self.alias_pointer_facts(src_name, &name);
                    if !self.pointer_phi_values.is_empty() {
                        self.materialize_raw_byte_index(&name, &raw, true, instructions)?;
                        if self.raw_pointer_word_aligned(&raw) {
                            self.materialize_raw_word_index(&name, &raw, true, instructions)?;
                        }
                    } else {
                        self.materialize_reserved_raw_byte_index(&name, &raw, instructions)?;
                        if self.raw_pointer_word_aligned(&raw) {
                            self.materialize_reserved_raw_word_index(&name, &raw, instructions)?;
                        }
                    }
                    if raw.device_addr_base.is_some() {
                        self.materialize_reserved_bda_address(&name, &raw, instructions)?;
                    }
                    self.raw_offsets.insert(name.clone(), raw);
                    let addrspace = match dst_ty {
                        LlType::Ptr(addrspace) => addrspace,
                        _ => {
                            return Err(
                                "native emitter: unmodeled-byte bitcast destination is not a \
                                 pointer"
                                    .into(),
                            )
                        }
                    };
                    self.define_unmodeled_byte_pointer_value(&name, addrspace)?;
                    return Ok(());
                }
            }
        }
        let src_id = self.value_id(&src.value, &src.ty)?;
        if let (LlType::Ptr(src_addrspace), LlType::Ptr(dst_addrspace)) = (&src_ty, &dst_ty) {
            if src_addrspace == dst_addrspace {
                self.values.insert(name.clone(), (src_id, dst_ty.clone()));
                if let LlValue::Local(src_name) = &src.value {
                    if let Some(address) = self.bda_direct_addresses.get(src_name).copied() {
                        self.bda_direct_addresses.insert(name.clone(), address);
                    }
                    if let Some(storage) = self.pointer_storage.get(src_name).copied() {
                        self.pointer_storage.insert(name.clone(), storage);
                    }
                    if let Some(is_null) = self.pointer_nullness.get(src_name).copied() {
                        self.record_pointer_nullness(name.clone(), is_null);
                    }
                    if let Some(pointee) = self.pointer_pointees.get(src_name).cloned() {
                        self.pointer_pointees.insert(name.clone(), pointee);
                    }
                    if let Some(raw) = self.raw_offsets.get(src_name).cloned() {
                        self.raw_offsets.insert(name.clone(), raw);
                    }
                    if let Some(provenance) = self.gep_provenance.get(src_name).cloned() {
                        self.gep_provenance.insert(name.clone(), provenance);
                    }
                    if self.unmodeled_pointers.contains(src_name) {
                        self.unmodeled_pointers.insert(name.clone());
                    }
                    if self.param_values.contains(src_name) {
                        self.param_values.insert(name);
                    }
                }
                return Ok(());
            }
        }
        let aggregate_result = self.result_id(&name, &dst_ty)?;
        if self.emit_i8_array_integer_bitcast(
            src_id,
            &src_ty,
            &dst_ty,
            aggregate_result,
            instructions,
        )? {
            return Ok(());
        }
        let result = match (&src_ty, &dst_ty) {
            (LlType::Int(32), LlType::Vector(elem, 4)) if **elem == LlType::Int(8) => {
                self.emit_i32_to_v4i8(src_id, instructions)?
            }
            (a, b) if a == b && !self.values.contains_key(&name) => src_id,
            (a, b) if a == b => {
                let result_type = self.type_id(&dst_ty)?;
                let result = self.result_id(&name, &dst_ty)?;
                instructions.push(Self::inst(
                    Op::CopyObject,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(src_id)],
                ));
                result
            }
            (LlType::Ptr(_), LlType::Ptr(_)) => {
                return Err(format!(
                    "native emitter: cannot reinterpret pointer {name} across address spaces \
                     without a logical-pointer bitcast"
                ));
            }
            (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes))
                if *src_lanes > 4 && src_lanes == dst_lanes =>
            {
                let src_elem_ty = self.resolve_type(src_elem)?;
                let dst_elem_ty = self.resolve_type(dst_elem)?;
                let src_elem_type = self.type_id(&src_elem_ty)?;
                let dst_elem_type = self.type_id(&dst_elem_ty)?;
                let lanes = *src_lanes;
                let result_type = self.type_id(&dst_ty)?;
                let result = self.result_id(&name, &dst_ty)?;
                if src_elem_type == dst_elem_type {
                    instructions.push(Self::inst(
                        Op::CopyObject,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(src_id)],
                    ));
                    result
                } else {
                    let mut values = Vec::with_capacity(lanes as usize);
                    for lane in 0..lanes {
                        let component = self.fresh();
                        instructions.push(Self::inst(
                            Op::CompositeExtract,
                            Some(src_elem_type),
                            Some(component),
                            vec![Operand::IdRef(src_id), Operand::LiteralBit32(lane)],
                        ));
                        let cast = self.fresh();
                        instructions.push(Self::inst(
                            Op::Bitcast,
                            Some(dst_elem_type),
                            Some(cast),
                            vec![Operand::IdRef(component)],
                        ));
                        values.push(Operand::IdRef(cast));
                    }
                    instructions.push(Self::inst(
                        Op::CompositeConstruct,
                        Some(result_type),
                        Some(result),
                        values,
                    ));
                    result
                }
            }
            _ => {
                let source_type = self.type_id(&src_ty)?;
                let result_type = self.type_id(&dst_ty)?;
                let result = self.result_id(&name, &dst_ty)?;
                instructions.push(Self::inst(
                    if source_type == result_type {
                        Op::CopyObject
                    } else {
                        Op::Bitcast
                    },
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(src_id)],
                ));
                result
            }
        };
        if matches!(dst_ty, LlType::Ptr(_)) {
            if let LlValue::Local(src_name) = &src.value {
                if let Some(storage) = self.pointer_storage.get(src_name).copied() {
                    self.pointer_storage.insert(name.clone(), storage);
                }
                if let Some(is_null) = self.pointer_nullness.get(src_name).copied() {
                    self.record_pointer_nullness(name.clone(), is_null);
                }
                if let Some(pointee) = self.pointer_pointees.get(src_name).cloned() {
                    self.pointer_pointees.insert(name.clone(), pointee);
                }
                if let Some(raw) = self.raw_offsets.get(src_name).cloned() {
                    self.raw_offsets.insert(name.clone(), raw);
                }
                if let Some(provenance) = self.gep_provenance.get(src_name).cloned() {
                    self.gep_provenance.insert(name.clone(), provenance);
                }
                if self.unmodeled_pointers.contains(src_name) {
                    self.unmodeled_pointers.insert(name.clone());
                }
                if self.byte_view_pointers.contains(src_name) {
                    self.byte_view_pointers.insert(name.clone());
                }
                if self.param_values.contains(src_name) {
                    self.param_values.insert(name.clone());
                }
            }
        }
        self.values.insert(name, (result, dst_ty));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_load_resolved(
        &mut self,
        name: String,
        load: LlLoad,
        result_ty: LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let constant = matches!(load.ptr.ty, LlType::Ptr(2));
        let saved = std::mem::replace(&mut self.constant_space_load, constant);
        let b78_hint = b78_air_access_hint(&load.ptr.ty, load.align, &result_ty);
        let b78_start = instructions.len();
        let emitted = self.emit_load_resolved_inner(name, load, result_ty, instructions);
        self.constant_space_load = saved;
        if emitted.is_ok() {
            self.b78_record_access_align(b78_hint, instructions.get(b78_start..).unwrap_or(&[]));
        }
        emitted
    }

    fn emit_load_resolved_inner(
        &mut self,
        name: String,
        load: LlLoad,
        result_ty: LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        if let LlValue::Local(ptr_name) = &load.ptr.value {
            if let Some(tree) = self.selected_access_trees.get(ptr_name).cloned() {
                self.emit_selected_access_tree_load(
                    result,
                    &result_ty,
                    &tree,
                    load.align,
                    instructions,
                )?;
                return Ok(());
            }
            if let Some(selected) = self.selected_load_pointers.get(ptr_name).cloned() {
                self.emit_selected_pointer_load(
                    result,
                    &result_ty,
                    &selected,
                    load.align,
                    instructions,
                )?;
                return Ok(());
            }
            if let Some(selected) = self.selected_pointers.get(ptr_name).cloned() {
                self.emit_selected_pointer_direct_load(
                    result,
                    &result_ty,
                    &selected,
                    load.align,
                    instructions,
                )?;
                return Ok(());
            }
            if let Some(vector_word) = self.vector_word_pointers.get(ptr_name).cloned() {
                self.emit_vector_word_load(result, &result_ty, &vector_word, instructions)?;
                return Ok(());
            }
            if let Some(raw) = self.raw_offsets.get(ptr_name).cloned() {
                // A sampler an argument buffer holds is its gpuResourceID, whose low word is the sampler-heap slot, whether
                // the buffer is reached through a device address or bound directly (Blender binds its 17-sampler argument
                // buffer directly; refusing it left the pipeline unbuilt and crashed Blender). Textures keep the
                // device-address requirement: a directly bound buffer's texture fields have their own bindings.
                let sampler_field = matches!(result_ty, LlType::Ptr(2)) && self.opaque_sampler_pointers.contains(&name)
                    && crate::argument_buffer_samplers_on();
                if (raw.device_addr_base.is_some() || sampler_field)
                    && crate::reflect::bindless_fixed_fields_on()
                    && ((matches!(result_ty, LlType::Ptr(1))
                        && self.opaque_resource_pointers.contains(&name))
                        || sampler_field)
                {
                    let sampler = matches!(result_ty, LlType::Ptr(2));
                    let low =
                        self.emit_raw_word_load_for_access(&raw, 0, load.align, instructions)?;
                    let slots = if sampler {
                        crate::reflect::BINDLESS_SAMPLER_SLOTS
                    } else {
                        crate::reflect::BINDLESS_HEAP_SLOTS
                    };
                    let mask = self.const_uint(slots - 1)?;
                    let u32_ty = self.type_id(&LlType::Int(32))?;
                    let slot = self.fresh();
                    instructions.push(Self::inst(
                        Op::BitwiseAnd,
                        Some(u32_ty),
                        Some(slot),
                        vec![Operand::IdRef(low), Operand::IdRef(mask)],
                    ));
                    self.emit_sidecar.device_handle_heap_loads.push(
                        crate::emit_sidecar::DeviceHandleHeapLoad {
                            id: result,
                            slot,
                            sampler,
                        },
                    );
                    return Ok(());
                }
                if self.bda_device_pointers && !self.opaque_resource_pointers.contains(&name) {
                    if let LlType::Ptr(1 | 2) = result_ty {
                        let addr = self.result_id(&bda_address_name(&name), &LlType::Int(64))?;
                        self.emit_raw_load(addr, &LlType::Int(64), &raw, load.align, instructions)?;
                        self.bda_address_values.insert(addr);
                        self.emit_device_address_nullness(&name, addr, instructions)?;
                        self.used_device_address = true;
                        let mut dev = RawBufferOffset::root(format!(".bda_{addr}"), 1);
                        dev.device_addr_base = Some(addr);
                        self.raw_offsets.insert(name.clone(), dev);
                        self.pointer_storage
                            .insert(name.clone(), StorageClass::PhysicalStorageBuffer);
                        return Ok(());
                    }
                }
                self.emit_raw_load(result, &result_ty, &raw, load.align, instructions)?;
                if let LlType::Ptr(_) = result_ty {
                    let parameter_root = self
                        .direct_param_values
                        .contains(&raw.root)
                        .then(|| self.values.get(&raw.root).map(|(id, _)| *id))
                        .flatten();
                    if raw.device_addr_base.is_none()
                        && raw.dyn_terms.is_empty()
                        && raw.const_off >= 0
                    {
                        if let Some(root) = parameter_root {
                            self.emit_sidecar.buffer_pointer_field_loads.push(
                                crate::emit_sidecar::BufferPointerFieldLoad {
                                    id: result,
                                    root,
                                    byte_offset: raw.const_off as u64,
                                },
                            );
                            if crate::reflect::bindless_fixed_fields_on() {
                                if let Ok(low) = self.emit_raw_word_load_for_access(
                                    &raw,
                                    0,
                                    load.align,
                                    instructions,
                                ) {
                                    let mask =
                                        self.const_uint(crate::reflect::BINDLESS_HEAP_SLOTS - 1)?;
                                    let u32_ty = self.type_id(&LlType::Int(32))?;
                                    let slot = self.fresh();
                                    instructions.push(Self::inst(
                                        Op::BitwiseAnd,
                                        Some(u32_ty),
                                        Some(slot),
                                        vec![Operand::IdRef(low), Operand::IdRef(mask)],
                                    ));
                                    self.emit_sidecar.buffer_pointer_heap_loads.push(
                                        crate::emit_sidecar::BufferPointerHeapLoad {
                                            id: result,
                                            root,
                                            byte_offset: raw.const_off as u64,
                                            slot,
                                        },
                                    );
                                }
                            }
                        }
                    } else if raw.device_addr_base.is_none() && raw.const_off >= 0 {
                        if let (Some(root), false) = (parameter_root, raw.dyn_terms.is_empty()) {
                            let low = self.emit_raw_word_load_for_access(
                                &raw,
                                0,
                                load.align,
                                instructions,
                            )?;
                            let mask = self.const_uint(crate::reflect::BINDLESS_HEAP_SLOTS - 1)?;
                            let u32_ty = self.type_id(&LlType::Int(32))?;
                            let slot = self.fresh();
                            instructions.push(Self::inst(
                                Op::BitwiseAnd,
                                Some(u32_ty),
                                Some(slot),
                                vec![Operand::IdRef(low), Operand::IdRef(mask)],
                            ));
                            self.emit_sidecar.buffer_pointer_heap_loads.push(
                                crate::emit_sidecar::BufferPointerHeapLoad {
                                    id: result,
                                    root,
                                    byte_offset: raw.const_off as u64,
                                    slot,
                                },
                            );
                        }
                        if let (Some(root), [(index, 8)]) =
                            (parameter_root, raw.dyn_terms.as_slice())
                        {
                            let index = self.value_id_in(&index.value, &index.ty, instructions)?;
                            self.emit_sidecar.buffer_pointer_dynamic_field_loads.push(
                                crate::emit_sidecar::BufferPointerDynamicFieldLoad {
                                    id: result,
                                    root,
                                    byte_offset: raw.const_off as u64,
                                    index,
                                },
                            );
                        }
                    }
                    self.pointer_storage
                        .insert(name.clone(), StorageClass::Private);
                    self.pointer_pointees.insert(name.clone(), LlType::Int(8));
                    self.unmodeled_pointers.insert(name.clone());
                    let needs_payload = self.pointer_payload_values.contains(&name);
                    let is_null = if self.raw_pointer_word_aligned(&raw) || needs_payload {
                        let (payload, is_null) =
                            self.emit_raw_pointer_payload(&raw, 0, load.align, instructions)?;
                        self.pointer_payload_words.insert(name.clone(), payload);
                        is_null
                    } else {
                        self.const_bool(false)?
                    };
                    self.record_pointer_nullness(name.clone(), is_null);
                }
                return Ok(());
            }
            if let Some(raw) = self.byte_array_reinterpret_raw_pointer(&load.ptr.value)? {
                self.emit_raw_load(result, &result_ty, &raw, load.align, instructions)?;
                return Ok(());
            }
            if self.unmodeled_pointers.contains(ptr_name) {
                if let LlType::Ptr(addrspace) = result_ty {
                    self.define_unmodeled_byte_pointer_value(&name, addrspace)?;
                } else {
                    let zero = self.const_null(&result_ty)?;
                    instructions.push(Self::inst(
                        Op::CopyObject,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(zero)],
                    ));
                }
                return Ok(());
            }
        }
        if self.emit_pointer_from_local_dynamic_field_load(
            &name,
            result,
            &result_ty,
            &load.ptr,
            instructions,
        )? {
            return Ok(());
        }
        if let Some(pointee) = self.pointer_pointee_for_value(&load.ptr.value)? {
            let pointee = self.resolve_type(&pointee)?;
            if self.emit_pointer_from_local_field_load(
                &name,
                result,
                &result_ty,
                &load.ptr,
                &pointee,
                instructions,
            )? {
                return Ok(());
            }
        }
        if self.emit_vector_root_load(result, &result_ty, &load.ptr, instructions)? {
            return Ok(());
        }
        let ptr = self.value_id_in(&load.ptr.value, &load.ptr.ty, instructions)?;
        let pointer_is_undefined = self.module.types_global_values.iter().any(|instruction| {
            instruction.result_id == Some(ptr) && instruction.class.opcode == Op::Undef
        });
        if pointer_is_undefined {
            let undefined = self.undef_id(&result_ty)?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(undefined)],
            ));
            return Ok(());
        }
        if self.emit_trailing_byte_array_integer_load(
            result,
            &result_ty,
            &load.ptr,
            instructions,
        )? {
            return Ok(());
        }
        if self.emit_trailing_byte_array_value_load(result, &result_ty, &load.ptr, instructions)? {
            return Ok(());
        }
        if result_ty == LlType::Int(64)
            && self
                .pointer_pointee_for_value(&load.ptr.value)?
                .is_some_and(|pointee| matches!(pointee, LlType::Ptr(1)))
        {
            if let Some(pointer_name) = self.opaque_resource_payload_loads.get(&name).cloned() {
                let pointer_ty = LlType::Ptr(1);
                let pointer_type = self.type_id(&pointer_ty)?;
                let pointer = self.result_id(&pointer_name, &pointer_ty)?;
                instructions.push(Self::inst(
                    Op::Load,
                    Some(pointer_type),
                    Some(pointer),
                    vec![Operand::IdRef(ptr)],
                ));
                return Ok(());
            }
        }
        if let Some(pointee) = self.pointer_pointee_for_value(&load.ptr.value)? {
            let pointee = self.resolve_type(&pointee)?;
            if !types_compatible(&pointee, &result_ty) {
                if self.emit_i32_pair_struct_to_i64_load(
                    result,
                    &pointee,
                    &result_ty,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_aggregate_prefix_integer_reinterpret_load(
                    result,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_aggregate_prefix_pointer_reinterpret_load(
                    &name,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_byte_array_integer_reinterpret_load(
                    result,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_scalar_array_as_vector_load(
                    result,
                    &pointee,
                    &result_ty,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_first_pointer_aggregate_reinterpret_load(
                    &name,
                    result,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_first_vector_aggregate_reinterpret_load(
                    result,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if self.emit_first_scalar_aggregate_reinterpret_load(
                    result,
                    &pointee,
                    &result_ty,
                    &load.ptr,
                    ptr,
                    instructions,
                )? {
                    return Ok(());
                }
                if bitcast_width(&pointee).is_none()
                    && self.emit_leading_byte_reinterpret_load(
                        result,
                        &pointee,
                        &result_ty,
                        &load.ptr,
                        ptr,
                        instructions,
                    )?
                {
                    return Ok(());
                }
                let pointee_bits = bitcast_width(&pointee).ok_or_else(|| {
                    format!(
                        "native emitter: cannot reinterpret load `{name}` from {:?} with non-bitcastable pointee {pointee:?} to {result_ty:?}",
                        load.ptr.value
                    )
                })?;
                let result_bits = bitcast_width(&result_ty).ok_or_else(|| {
                    format!(
                        "native emitter: cannot reinterpret load to non-bitcastable result {result_ty:?}"
                    )
                })?;
                if pointee == LlType::Int(8) {
                    let asp = match self.resolve_type(&load.ptr.ty)? {
                        LlType::Ptr(asp) => asp,
                        _ => 1,
                    };
                    let storage = self.pointer_storage_for(&load.ptr.value, asp)?;
                    if storage == StorageClass::Private
                        && self.emit_private_scalar_load_from_byte_pointer(
                            result,
                            &result_ty,
                            &load.ptr.value,
                            instructions,
                        )?
                    {
                        return Ok(());
                    }
                    if self.emit_scalar_load_from_byte_pointer(
                        result,
                        &result_ty,
                        storage,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                }
                if pointee_bits != result_bits {
                    if self.emit_narrowing_vector_load(
                        result,
                        &pointee,
                        &result_ty,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_widening_vector_load(
                        result,
                        &pointee,
                        &result_ty,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_scalar_to_vector_load(
                        result,
                        &pointee,
                        &result_ty,
                        &load.ptr,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_scalar_word_to_subword_vector_load(
                        result,
                        &pointee,
                        &result_ty,
                        &load.ptr,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_scalar_slots_to_wider_load(
                        result,
                        &pointee,
                        &result_ty,
                        &load.ptr,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_scalar_from_vector_load(
                        result,
                        &pointee,
                        &result_ty,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if self.emit_scalar_narrowing_load(
                        result,
                        &pointee,
                        &result_ty,
                        ptr,
                        instructions,
                    )? {
                        return Ok(());
                    }
                    if pointee == LlType::Int(8) {
                        return Err(format!(
                            "native emitter: cannot reinterpret load of byte pointer to {result_ty:?} without a logical-pointer bitcast"
                        ));
                    }
                    return Err(format!(
                        "native emitter: reinterpret load bit width mismatch {pointee:?} ({pointee_bits}) vs {result_ty:?} ({result_bits})"
                    ));
                }
                let pointee_type = self.type_id(&pointee)?;
                let loaded = self.fresh();
                instructions.push(Self::inst(
                    Op::Load,
                    Some(pointee_type),
                    Some(loaded),
                    vec![Operand::IdRef(ptr)],
                ));
                instructions.push(Self::inst(
                    Op::Bitcast,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(loaded)],
                ));
                return Ok(());
            }
        }
        instructions.push(Self::inst(
            Op::Load,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(ptr)],
        ));
        if let LlType::Ptr(addrspace) = result_ty {
            self.pointer_storage
                .insert(name.clone(), llvm_pointer_storage(addrspace)?);
        }
        Ok(())
    }
}

fn b78_air_access_hint(ptr: &LlType, align: Option<u64>, value: &LlType) -> Option<(u32, u32)> {
    if !matches!(ptr, LlType::Ptr(1 | 2)) {
        return None;
    }
    let claim = align
        .filter(|a| a.is_power_of_two() && *a >= 2)
        .map(|a| a.min(16) as u32);
    Some(match (claim, b78_store_bytes(value)) {
        (Some(c), Some(b)) => (c, b),
        _ => (0, 0),
    })
}

fn b78_store_bytes(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Int(w) if *w >= 8 && w % 8 == 0 => Some(w / 8),
        LlType::Float => Some(4),
        LlType::Half | LlType::BFloat => Some(2),
        LlType::Vector(elem, lanes) => b78_store_bytes(elem)?.checked_mul(*lanes),
        _ => None,
    }
}

impl Emitter {
    fn b78_record_access_align(&mut self, hint: Option<(u32, u32)>, emitted: &[Instruction]) {
        let Some(hint) = hint else { return };
        let ptrs: Vec<spirv::Word> = emitted
            .iter()
            .filter(|i| matches!(i.class.opcode, spirv::Op::Load | spirv::Op::Store))
            .filter_map(|i| match i.operands.first() {
                Some(crate::spirv_module::Operand::IdRef(p)) => Some(*p),
                _ => None,
            })
            .collect();
        let Some((&last, rest)) = ptrs.split_last() else {
            return;
        };
        let hints = &mut self.emit_sidecar.air_access_aligns;
        for p in rest {
            hints.insert(*p, (0, 0));
        }
        let merged = match hints.get(&last) {
            None => hint,
            Some(&(c, b)) if c != 0 && hint.0 != 0 && b == hint.1 => (c.min(hint.0), b),
            Some(_) => (0, 0),
        };
        hints.insert(last, merged);
    }
}
