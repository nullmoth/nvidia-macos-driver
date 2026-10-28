use super::super::ops::{bfloat_lanes, shaped_type};
use super::*;

impl Emitter {
    pub(in crate::native::emitter) fn emit_phi_resolved(
        &mut self,
        name: String,
        phi_ty: &LlType,
        parsed_incoming: Vec<(LlValue, String)>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_ty = self.resolve_type(phi_ty)?;
        let incoming = if matches!(result_ty, LlType::Ptr(_)) {
            parsed_incoming
                .into_iter()
                .map(|(value, label)| (self.forwarded_pointer_value(&value), label))
                .collect()
        } else {
            parsed_incoming
        };
        if self.emit_selected_access_tree_phi(&name, &incoming, &result_ty, instructions)? {
            return Ok(());
        }
        if self.emit_bda_address_phi(&name, &incoming, &result_ty, instructions)? {
            return Ok(());
        }
        if self.emit_raw_pointer_phi(&name, &incoming, &result_ty, instructions)? {
            return Ok(());
        }
        if self.emit_unmodeled_pointer_phi(&name, &incoming, &result_ty, instructions)? {
            return Ok(());
        }
        let mut pointer_meta = self.pointer_merge_meta(
            &incoming.iter().map(|(value, _)| value).collect::<Vec<_>>(),
            &result_ty,
        )?;
        if self.null_rooted_pointer_values.contains(&name) {
            if let Some(meta) = pointer_meta.as_mut() {
                if meta.pointee.is_none() {
                    let direct = self
                        .tir_use_pointees
                        .get(&name)
                        .map(|pointee| self.resolve_type(pointee))
                        .transpose()?;
                    let peers = self
                        .null_rooted_pointer_peers
                        .get(&name)
                        .cloned()
                        .unwrap_or_default();
                    let mut recurrence_pointees = Vec::new();
                    for peer in peers {
                        let Some(gep) = self.forward_geps.get(&peer).cloned() else {
                            continue;
                        };
                        let source_ty = self.resolve_type(&gep.source_ty)?;
                        let pointee = gep_pointee(&source_ty, &gep.indices)?;
                        if !recurrence_pointees.contains(&pointee) {
                            recurrence_pointees.push(pointee);
                        }
                    }
                    meta.pointee = match (direct, recurrence_pointees.as_slice()) {
                        (Some(direct), []) => Some(direct),
                        (Some(direct), [recurrence]) if direct == *recurrence => Some(direct),
                        (None, [recurrence]) => Some(recurrence.clone()),
                        (None, []) => Some(LlType::Int(8)),
                        _ => {
                            return Err(format!(
                                "native emitter: null-rooted pointer component has conflicting pointee contracts {recurrence_pointees:?}"
                            ));
                        }
                    };
                }
            }
        }
        let int_alignment = matches!(result_ty, LlType::Int(_))
            .then(|| self.merged_int_alignment(incoming.iter().map(|(value, _)| value.clone())));
        let pointer_provenance = if self.null_rooted_pointer_values.contains(&name) {
            None
        } else {
            self.emit_pointer_phi_provenance(&name, &incoming, instructions)?
        };
        self.emit_pointer_nullness_phi(&name, &incoming, &result_ty, instructions)?;
        if self.null_rooted_pointer_values.contains(&name) {
            let Some(meta) = pointer_meta.as_ref() else {
                return Err(
                    "native emitter: null-rooted pointer phi has no storage contract".into(),
                );
            };
            let Some(pointee) = meta.pointee.as_ref() else {
                return Err(
                    "native emitter: null-rooted pointer phi has no pointee contract".into(),
                );
            };
            let result_type = self.ptr_type_id(meta.storage, pointee)?;
            let result = self.result_id(&name, &result_ty)?;
            self.module.types_global_values.push(Self::inst(
                Op::Undef,
                Some(result_type),
                Some(result),
                vec![],
            ));
            self.record_pointer_meta(name, meta.clone());
            return Ok(());
        }
        if let Some(meta) = pointer_meta.as_ref().filter(|meta| {
            pointer_provenance.is_none()
                && matches!(meta.storage, StorageClass::Private | StorageClass::Function)
        }) {
            let Some((first, _)) = incoming.first() else {
                return Err(format!(
                    "native emitter: pointer phi {name} has no incoming values"
                ));
            };
            if incoming.iter().all(|(value, _)| value == first) {
                let result = match first {
                    LlValue::Zero | LlValue::Undef => {
                        let pointee = meta.pointee.as_ref().ok_or_else(|| {
                            format!(
                                "native emitter: collapsed pointer phi {name} has no pointee contract"
                            )
                        })?;
                        let result_type = self.ptr_type_id(meta.storage, pointee)?;
                        let result = self.fresh();
                        self.module.types_global_values.push(Self::inst(
                            Op::Undef,
                            Some(result_type),
                            Some(result),
                            vec![],
                        ));
                        result
                    }
                    _ => self.value_id(first, &result_ty)?,
                };
                self.values
                    .insert(name.clone(), (result, result_ty.clone()));
                self.record_pointer_meta(name, meta.clone());
                return Ok(());
            }
            return Err(format!(
                "native emitter: {:?} pointer phi {name} has differing structural sources",
                meta.storage
            ));
        }
        if let (
            Some(
                meta @ PointerMeta {
                    storage,
                    pointee: Some(pointee),
                },
            ),
            Some(provenance),
        ) = (pointer_meta.as_ref(), pointer_provenance.as_ref())
        {
            let result_type = self.ptr_type_id(*storage, pointee)?;
            let result = self.result_id(&name, &result_ty)?;
            let op = pointer_arithmetic_access_chain_op_for_storage(
                *storage,
                provenance.root_is_indexed_container,
                pointee,
                &provenance.indices,
            );
            let mut operands = vec![Operand::IdRef(provenance.root)];
            for index in gep_spirv_indices(&provenance.indices)? {
                operands.push(Operand::IdRef(self.value_id(&index.value, &index.ty)?));
            }
            self.phi_result_instructions.push(Self::inst(
                op,
                Some(result_type),
                Some(result),
                operands,
            ));
            self.record_pointer_meta(name.clone(), meta.clone());
            self.gep_provenance.insert(name.clone(), provenance.clone());
            return Ok(());
        }
        if matches!(
            pointer_meta,
            Some(PointerMeta {
                storage: StorageClass::Function,
                ..
            })
        ) {
            return Err(format!(
                "native emitter: Function pointer phi {name} has no exact index-domain representation from {incoming:?}"
            ));
        }
        let result_type = self.pointer_aware_type_id(&result_ty, pointer_meta.as_ref())?;
        let result = self.result_id(&name, &result_ty)?;
        let mut ops = Vec::new();
        let mut seen_incoming: HashMap<Word, Word> = HashMap::default();
        for (value, label) in incoming {
            let mut edge_instructions = Vec::new();
            let value_id = if let Some(meta) = pointer_meta.as_ref() {
                self.pointer_phi_value_id(
                    &value,
                    &result_ty,
                    meta,
                    pointer_provenance.as_ref(),
                    &mut edge_instructions,
                )?
            } else {
                self.phi_value_id(&value, &result_ty, &mut edge_instructions)?
            };
            let label_id = self.label_id(&label)?;
            self.record_phi_edge_instructions(label_id, edge_instructions);
            if let Some(existing) = seen_incoming.insert(label_id, value_id) {
                if existing != value_id {
                    return Err(format!(
                        "native emitter: phi {name} has multiple values from predecessor {label}"
                    ));
                }
                continue;
            }
            ops.push(Operand::IdRef(value_id));
            ops.push(Operand::IdRef(label_id));
        }
        instructions.push(Self::inst(Op::Phi, Some(result_type), Some(result), ops));
        if let Some(meta) = pointer_meta {
            self.record_pointer_meta(name.clone(), meta);
        }
        if let Some(provenance) = pointer_provenance {
            self.gep_provenance.insert(name.clone(), provenance);
        }
        if let Some(alignment) = int_alignment {
            self.record_int_alignment(&name, &result_ty, alignment);
        }
        Ok(())
    }

    fn forwarded_pointer_value(&self, value: &LlValue) -> LlValue {
        let mut value = value.clone();
        let mut visited = HashSet::default();
        while let LlValue::Local(name) = &value {
            if !visited.insert(name.clone()) {
                break;
            }
            let Some(source) = self.pointer_forward_values.get(name) else {
                break;
            };
            value = source.value.clone();
        }
        value
    }

    fn emit_selected_access_tree_phi(
        &mut self,
        name: &str,
        incoming: &[(LlValue, String)],
        result_ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        staged_emit(instructions, |instructions| {
            let LlType::Ptr(_) = result_ty else {
                return Ok(false);
            };
            let pointee = self
                .tir_use_pointees
                .get(name)
                .cloned()
                .or_else(|| self.pointer_pointees.get(name).cloned());
            let Some(pointee) = pointee else {
                return Ok(false);
            };
            let mut trees = Vec::with_capacity(incoming.len());
            for (value, label) in incoming {
                let LlValue::Local(local) = value else {
                    return Ok(false);
                };
                let tree = if let Some(tree) = self.selected_access_trees.get(local).cloned() {
                    if !types_compatible(&self.resolve_type(&tree.pointee)?, &pointee) {
                        return Ok(false);
                    }
                    tree
                } else if let Some(selected) = self.selected_pointers.get(local).cloned() {
                    self.build_selected_access_tree(
                        &selected,
                        &pointee,
                        &[],
                        &mut HashSet::default(),
                        instructions,
                    )?
                } else {
                    return Ok(false);
                };
                trees.push((tree, self.label_id(label)?));
            }
            let Some(tree) = self.merge_selected_access_tree_phi(&trees, instructions)? else {
                return Ok(false);
            };
            self.pointer_pointees
                .insert(name.to_string(), tree.pointee.clone());
            self.selected_access_trees.insert(name.to_string(), tree);
            Ok(true)
        })
    }

    fn merge_selected_access_tree_phi(
        &mut self,
        trees: &[(SelectedAccessTree, Word)],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<SelectedAccessTree>, String> {
        let Some((first, _)) = trees.first() else {
            return Ok(None);
        };
        if trees
            .iter()
            .any(|(tree, _)| !types_compatible(&tree.pointee, &first.pointee))
        {
            return Ok(None);
        }
        let cond = self.merge_selected_phi_ids(
            trees.iter().map(|(tree, label)| (tree.cond, *label)),
            &LlType::Bool,
            instructions,
        )?;
        let true_arms = trees
            .iter()
            .map(|(tree, label)| (&tree.true_arm, *label))
            .collect::<Vec<_>>();
        let false_arms = trees
            .iter()
            .map(|(tree, label)| (&tree.false_arm, *label))
            .collect::<Vec<_>>();
        let Some(true_arm) =
            self.merge_selected_access_arm_phi(&true_arms, &first.pointee, instructions)?
        else {
            return Ok(None);
        };
        let Some(false_arm) =
            self.merge_selected_access_arm_phi(&false_arms, &first.pointee, instructions)?
        else {
            return Ok(None);
        };
        Ok(Some(SelectedAccessTree {
            cond,
            true_arm,
            false_arm,
            pointee: first.pointee.clone(),
        }))
    }

    fn merge_selected_access_arm_phi(
        &mut self,
        arms: &[(&SelectedAccessArm, Word)],
        pointee: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<SelectedAccessArm>, String> {
        let Some((first, _)) = arms.first() else {
            return Ok(None);
        };
        match first {
            SelectedAccessArm::Typed { storage, .. }
                if arms.iter().all(|(arm, _)| {
                    matches!(arm, SelectedAccessArm::Typed { storage: arm_storage, .. } if arm_storage == storage)
                }) =>
            {
                let ptr_type = self.ptr_type_id(*storage, pointee)?;
                let ids = arms.iter().map(|(arm, label)| {
                    let SelectedAccessArm::Typed { ptr, .. } = arm else {
                        unreachable!("shape checked above");
                    };
                    (*ptr, *label)
                });
                let ptr = self.merge_selected_phi_ids_with_type(ids, ptr_type, instructions);
                Ok(Some(SelectedAccessArm::Typed {
                    ptr,
                    storage: *storage,
                }))
            }
            SelectedAccessArm::Nested(_) if arms.iter().all(|(arm, _)| {
                matches!(arm, SelectedAccessArm::Nested(_))
            }) => {
                let nested = arms
                    .iter()
                    .map(|(arm, label)| {
                        let SelectedAccessArm::Nested(tree) = arm else {
                            unreachable!("shape checked above");
                        };
                        ((**tree).clone(), *label)
                    })
                    .collect::<Vec<_>>();
                Ok(self
                    .merge_selected_access_tree_phi(&nested, instructions)?
                    .map(|tree| SelectedAccessArm::Nested(Box::new(tree))))
            }
            SelectedAccessArm::Null
                if arms
                    .iter()
                    .all(|(arm, _)| matches!(arm, SelectedAccessArm::Null)) =>
            {
                Ok(Some(SelectedAccessArm::Null))
            }
            _ => Ok(None),
        }
    }

    fn merge_selected_phi_ids(
        &mut self,
        ids: impl Iterator<Item = (Word, Word)>,
        ty: &LlType,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let result_type = self.type_id(ty)?;
        Ok(self.merge_selected_phi_ids_with_type(ids, result_type, instructions))
    }

    fn merge_selected_phi_ids_with_type(
        &mut self,
        ids: impl Iterator<Item = (Word, Word)>,
        result_type: Word,
        instructions: &mut Vec<Instruction>,
    ) -> Word {
        let ids = ids.collect::<Vec<_>>();
        if ids.iter().all(|(id, _)| *id == ids[0].0) {
            return ids[0].0;
        }
        let result = self.fresh();
        let operands = ids
            .into_iter()
            .flat_map(|(id, label)| [Operand::IdRef(id), Operand::IdRef(label)])
            .collect();
        instructions.push(Self::inst(
            Op::Phi,
            Some(result_type),
            Some(result),
            operands,
        ));
        result
    }

    pub(in crate::native) fn record_phi_edge_instructions(
        &mut self,
        predecessor: Word,
        instructions: Vec<Instruction>,
    ) {
        if instructions.is_empty() {
            return;
        }
        self.phi_edge_instructions
            .entry(predecessor)
            .or_default()
            .extend(instructions);
    }

    pub(in crate::native::emitter) fn pointer_phi_value_id(
        &mut self,
        value: &LlValue,
        result_ty: &LlType,
        meta: &PointerMeta,
        result_provenance: Option<&GepProvenance>,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Word, String> {
        let Some(pointee) = meta.pointee.as_ref() else {
            return self.phi_value_id(value, result_ty, instructions);
        };
        if let Some(id) =
            self.typed_null_or_undef_pointer_id(value, meta.storage, pointee, instructions)?
        {
            return Ok(id);
        }
        let Some(template) = result_provenance else {
            if let LlValue::Local(name) = value {
                if meta.storage == StorageClass::Workgroup && self.param_values.contains(name) {
                    let ptr_ty = self.ptr_type_id(meta.storage, pointee)?;
                    let base = self.value_id(value, result_ty)?;
                    let zero = self.const_uint(0)?;
                    let result = self.fresh();
                    instructions.push(Self::inst(
                        Op::InBoundsAccessChain,
                        Some(ptr_ty),
                        Some(result),
                        vec![Operand::IdRef(base), Operand::IdRef(zero)],
                    ));
                    return Ok(result);
                }
            }
            return self.phi_value_id(value, result_ty, instructions);
        };
        let LlValue::Local(name) = value else {
            return self.phi_value_id(value, result_ty, instructions);
        };
        if self
            .values
            .get(name)
            .is_none_or(|(id, _)| *id != template.root)
        {
            return self.phi_value_id(value, result_ty, instructions);
        }
        let Some(index_ty) = template.indices.first().map(|index| index.ty.clone()) else {
            return self.phi_value_id(value, result_ty, instructions);
        };
        let Some(provenance) =
            self.provenance_for_pointer_value(value, Some(template), Some(&index_ty))?
        else {
            return self.phi_value_id(value, result_ty, instructions);
        };
        if !compatible_pointer_provenance(template, &provenance) {
            return self.phi_value_id(value, result_ty, instructions);
        }
        let ptr_ty = self.ptr_type_id(meta.storage, pointee)?;
        let result = self.fresh();
        let op = pointer_arithmetic_access_chain_op_for_storage(
            meta.storage,
            provenance.root_is_indexed_container,
            pointee,
            &provenance.indices,
        );
        let mut ops = vec![Operand::IdRef(provenance.root)];
        let mut indices = gep_spirv_indices(&provenance.indices)?;
        if indices.is_empty() && !provenance.indices.is_empty() {
            indices = provenance.indices.clone();
        }
        for idx in indices {
            ops.push(Operand::IdRef(self.value_id(&idx.value, &idx.ty)?));
        }
        instructions.push(Self::inst(op, Some(ptr_ty), Some(result), ops));
        Ok(result)
    }

    pub(in crate::native::emitter) fn emit_fcmp_resolved(
        &mut self,
        pred: Op,
        lhs: TypedValue,
        rhs: TypedValue,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let operand_ty = self.resolve_type(&lhs.ty)?;
        let rhs_ty = self.resolve_type(&rhs.ty)?;
        if operand_ty != rhs_ty {
            return Err(format!(
                "native emitter: fcmp operand type mismatch {operand_ty:?} vs {rhs_ty:?}"
            ));
        }
        if let Some(lanes) = bfloat_lanes(&operand_ty) {
            let result_ty = float_compare_result_type(&shaped_type(LlType::Float, lanes))?;
            let result_type = self.type_id(&result_ty)?;
            let result = self.result_id(&name, &result_ty)?;
            let lhs_bits = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
            let rhs_bits = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
            if lanes <= 4 {
                let lhs_f32 = self.bfloat_bits_to_float_shaped_id(lhs_bits, lanes, instructions)?;
                let rhs_f32 = self.bfloat_bits_to_float_shaped_id(rhs_bits, lanes, instructions)?;
                instructions.push(Self::inst(
                    pred,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(lhs_f32), Operand::IdRef(rhs_f32)],
                ));
                return Ok(());
            }
            let bits_type = self.type_id(&LlType::BFloat)?;
            let bool_type = self.type_id(&LlType::Bool)?;
            let mut comparisons = Vec::with_capacity(lanes as usize);
            for lane in 0..lanes {
                let lhs_lane = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeExtract,
                    Some(bits_type),
                    Some(lhs_lane),
                    vec![Operand::IdRef(lhs_bits), Operand::LiteralBit32(lane)],
                ));
                let rhs_lane = self.fresh();
                instructions.push(Self::inst(
                    Op::CompositeExtract,
                    Some(bits_type),
                    Some(rhs_lane),
                    vec![Operand::IdRef(rhs_bits), Operand::LiteralBit32(lane)],
                ));
                let lhs_f32 = self.bfloat_bits_to_float_shaped_id(lhs_lane, 1, instructions)?;
                let rhs_f32 = self.bfloat_bits_to_float_shaped_id(rhs_lane, 1, instructions)?;
                let comparison = self.fresh();
                instructions.push(Self::inst(
                    pred,
                    Some(bool_type),
                    Some(comparison),
                    vec![Operand::IdRef(lhs_f32), Operand::IdRef(rhs_f32)],
                ));
                comparisons.push(Operand::IdRef(comparison));
            }
            instructions.push(Self::inst(
                Op::CompositeConstruct,
                Some(result_type),
                Some(result),
                comparisons,
            ));
            return Ok(());
        }
        let result_ty = float_compare_result_type(&operand_ty)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let lhs_id = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
        let rhs_id = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
        instructions.push(Self::inst(
            pred,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(lhs_id), Operand::IdRef(rhs_id)],
        ));
        Ok(())
    }

    /// `fcmp ord a, b` is "neither operand is NaN"; `fcmp uno` is "either is NaN". Both are built
    /// from OpIsNan on each operand (lane-wise for vectors), since Vulkan has no OpOrdered/OpUnordered.
    pub(in crate::native::emitter) fn emit_fcmp_ordering_resolved(
        &mut self,
        ordered: bool,
        lhs: TypedValue,
        rhs: TypedValue,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let operand_ty = self.resolve_type(&lhs.ty)?;
        let rhs_ty = self.resolve_type(&rhs.ty)?;
        if operand_ty != rhs_ty {
            return Err(format!(
                "native emitter: fcmp operand type mismatch {operand_ty:?} vs {rhs_ty:?}"
            ));
        }
        if bfloat_lanes(&operand_ty).is_some() {
            return Err("native emitter: fcmp ord/uno on bfloat operands is unsupported".into());
        }
        let result_ty = float_compare_result_type(&operand_ty)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let lhs_id = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
        let rhs_id = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
        let lhs_nan = self.fresh();
        instructions.push(Self::inst(
            Op::IsNan,
            Some(result_type),
            Some(lhs_nan),
            vec![Operand::IdRef(lhs_id)],
        ));
        let rhs_nan = self.fresh();
        instructions.push(Self::inst(
            Op::IsNan,
            Some(result_type),
            Some(rhs_nan),
            vec![Operand::IdRef(rhs_id)],
        ));
        if ordered {
            let any_nan = self.fresh();
            instructions.push(Self::inst(
                Op::LogicalOr,
                Some(result_type),
                Some(any_nan),
                vec![Operand::IdRef(lhs_nan), Operand::IdRef(rhs_nan)],
            ));
            instructions.push(Self::inst(
                Op::LogicalNot,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(any_nan)],
            ));
        } else {
            instructions.push(Self::inst(
                Op::LogicalOr,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(lhs_nan), Operand::IdRef(rhs_nan)],
            ));
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_icmp_ptr_resolved(
        &mut self,
        pred: Op,
        lhs: TypedValue,
        rhs: TypedValue,
        name: String,
        rest: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let is_equal = match pred {
            Op::IEqual => true,
            Op::INotEqual => false,
            _ => {
                return Err(format!(
                    "native emitter: ordered pointer icmp is not supported: {rest}"
                ))
            }
        };
        if matches!((&lhs.value, &rhs.value), (LlValue::Zero, LlValue::Zero)) {
            let value = self.const_bool(is_equal)?;
            let result_ty = LlType::Bool;
            let result_type = self.type_id(&result_ty)?;
            let result = self.result_id(&name, &result_ty)?;
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
            return Ok(());
        }
        if let (LlValue::Local(lhs_name), LlValue::Local(rhs_name)) = (&lhs.value, &rhs.value) {
            if self.direct_param_values.contains(lhs_name)
                && self.direct_param_values.contains(rhs_name)
            {
                let equal = lhs_name == rhs_name;
                let value = self.const_bool(if is_equal { equal } else { !equal })?;
                let result_ty = LlType::Bool;
                let result_type = self.type_id(&result_ty)?;
                let result = self.result_id(&name, &result_ty)?;
                instructions.push(Self::inst(
                    Op::CopyObject,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(value)],
                ));
                return Ok(());
            }
        }
        if self.emit_raw_pointer_icmp(pred, &name, &lhs.value, &rhs.value, instructions)? {
            return Ok(());
        }
        if self.emit_provenance_pointer_icmp(pred, &name, &lhs.value, &rhs.value, instructions)? {
            return Ok(());
        }
        if let (LlValue::Local(lhs_name), LlValue::Local(rhs_name)) = (&lhs.value, &rhs.value) {
            if let (Some((lhs_low, lhs_high)), Some((rhs_low, rhs_high))) = (
                self.pointer_payload_words.get(lhs_name).copied(),
                self.pointer_payload_words.get(rhs_name).copied(),
            ) {
                let result_ty = LlType::Bool;
                let result_type = self.type_id(&result_ty)?;
                let low_equal = self.fresh();
                instructions.push(Self::inst(
                    Op::IEqual,
                    Some(result_type),
                    Some(low_equal),
                    vec![Operand::IdRef(lhs_low), Operand::IdRef(rhs_low)],
                ));
                let high_equal = self.fresh();
                instructions.push(Self::inst(
                    Op::IEqual,
                    Some(result_type),
                    Some(high_equal),
                    vec![Operand::IdRef(lhs_high), Operand::IdRef(rhs_high)],
                ));
                let result = self.result_id(&name, &result_ty)?;
                if is_equal {
                    instructions.push(Self::inst(
                        Op::LogicalAnd,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(low_equal), Operand::IdRef(high_equal)],
                    ));
                } else {
                    let equal = self.fresh();
                    instructions.push(Self::inst(
                        Op::LogicalAnd,
                        Some(result_type),
                        Some(equal),
                        vec![Operand::IdRef(low_equal), Operand::IdRef(high_equal)],
                    ));
                    instructions.push(Self::inst(
                        Op::LogicalNot,
                        Some(result_type),
                        Some(result),
                        vec![Operand::IdRef(equal)],
                    ));
                }
                return Ok(());
            }
        }
        let nullness = match (&lhs.value, &rhs.value) {
            (LlValue::Zero, value) | (value, LlValue::Zero) => {
                self.pointer_nullness_for_compare(value)?
            }
            _ => {
                return Err(format!(
                    "native emitter: pointer icmp is only supported against null: {rest}"
                ))
            }
        };
        let result_ty = LlType::Bool;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        if is_equal {
            instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(nullness)],
            ));
        } else {
            instructions.push(Self::inst(
                Op::LogicalNot,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(nullness)],
            ));
        }
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_icmp_int_resolved(
        &mut self,
        pred: Op,
        lhs: TypedValue,
        rhs: TypedValue,
        operand_ty: LlType,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let is_bool = match &operand_ty {
            LlType::Bool => true,
            LlType::Vector(elem, _) => matches!(elem.as_ref(), LlType::Bool),
            _ => false,
        };
        if is_bool {
            return self.emit_icmp_bool(pred, lhs, rhs, operand_ty, name, instructions);
        }
        let result_ty = int_compare_result_type(&operand_ty)?;
        let result_type = self.type_id(&result_ty)?;
        let result = self.result_id(&name, &result_ty)?;
        let lhs_id = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
        let rhs_id = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
        instructions.push(Self::inst(
            pred,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(lhs_id), Operand::IdRef(rhs_id)],
        ));
        Ok(())
    }

    /// `icmp` on `i1` or `<N x i1>` (Apple's GL compiler emits it for GLSL `bvec` equality, e.g. in WebRender's
    /// fragment shaders). SPIR-V has no integer compare on booleans, so equality maps to OpLogical(Not)Equal and the
    /// ordered predicates to their boolean truth tables (i1 is unsigned 0/1, or signed 0/-1).
    fn emit_icmp_bool(
        &mut self,
        pred: Op,
        lhs: TypedValue,
        rhs: TypedValue,
        operand_ty: LlType,
        name: String,
        instructions: &mut Vec<Instruction>,
    ) -> Result<(), String> {
        let result_type = self.type_id(&operand_ty)?;
        let result = self.result_id(&name, &operand_ty)?;
        let lhs_id = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
        let rhs_id = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
        let direct = match pred {
            Op::IEqual => Some(Op::LogicalEqual),
            Op::INotEqual => Some(Op::LogicalNotEqual),
            _ => None,
        };
        if let Some(op) = direct {
            instructions.push(Self::inst(
                op,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(lhs_id), Operand::IdRef(rhs_id)],
            ));
            return Ok(());
        }
        // (negate lhs, negate rhs, combine): unsigned 1 > 0, signed -1 < 0
        let (neg_lhs, neg_rhs, combine) = match pred {
            Op::UGreaterThan | Op::SLessThan => (false, true, Op::LogicalAnd),
            Op::UGreaterThanEqual | Op::SLessThanEqual => (false, true, Op::LogicalOr),
            Op::ULessThan | Op::SGreaterThan => (true, false, Op::LogicalAnd),
            Op::ULessThanEqual | Op::SGreaterThanEqual => (true, false, Op::LogicalOr),
            other => {
                return Err(format!(
                    "native emitter: icmp predicate {other:?} on booleans is not carried"
                ))
            }
        };
        let not = |emitter: &mut Self, id, instructions: &mut Vec<Instruction>| {
            let out = emitter.fresh();
            instructions.push(Self::inst(
                Op::LogicalNot,
                Some(result_type),
                Some(out),
                vec![Operand::IdRef(id)],
            ));
            out
        };
        let a = if neg_lhs { not(self, lhs_id, instructions) } else { lhs_id };
        let b = if neg_rhs { not(self, rhs_id, instructions) } else { rhs_id };
        instructions.push(Self::inst(
            combine,
            Some(result_type),
            Some(result),
            vec![Operand::IdRef(a), Operand::IdRef(b)],
        ));
        Ok(())
    }

    pub(in crate::native::emitter) fn emit_raw_pointer_icmp(
        &mut self,
        pred: Op,
        name: &str,
        lhs: &LlValue,
        rhs: &LlValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        if !matches!(pred, Op::IEqual | Op::INotEqual) {
            return Ok(false);
        }
        let (LlValue::Local(lhs_name), LlValue::Local(rhs_name)) = (lhs, rhs) else {
            return Ok(false);
        };
        let (Some(lhs_raw), Some(rhs_raw)) = (
            self.raw_offsets.get(lhs_name).cloned(),
            self.raw_offsets.get(rhs_name).cloned(),
        ) else {
            return Ok(false);
        };
        if lhs_raw.unmodelable || rhs_raw.unmodelable {
            return Ok(false);
        }

        let result_ty = LlType::Bool;
        let result_type = self.type_id(&result_ty)?;
        let equal = self.fresh();
        let (lhs_index, rhs_index) = match (
            lhs_raw.device_addr_base.is_some(),
            rhs_raw.device_addr_base.is_some(),
        ) {
            (true, true) => (
                self.materialize_device_address(&lhs_raw, instructions)?,
                self.materialize_device_address(&rhs_raw, instructions)?,
            ),
            (false, false)
                if lhs_raw.root == rhs_raw.root && lhs_raw.addrspace == rhs_raw.addrspace =>
            {
                (
                    self.emit_raw_byte_index(&lhs_raw, 0, instructions)?,
                    self.emit_raw_byte_index(&rhs_raw, 0, instructions)?,
                )
            }
            _ => return Ok(false),
        };
        instructions.push(Self::inst(
            Op::IEqual,
            Some(result_type),
            Some(equal),
            vec![Operand::IdRef(lhs_index), Operand::IdRef(rhs_index)],
        ));
        let result = self.result_id(name, &result_ty)?;
        match pred {
            Op::IEqual => instructions.push(Self::inst(
                Op::CopyObject,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(equal)],
            )),
            Op::INotEqual => instructions.push(Self::inst(
                Op::LogicalNot,
                Some(result_type),
                Some(result),
                vec![Operand::IdRef(equal)],
            )),
            _ => unreachable!(),
        }
        Ok(true)
    }

    pub(in crate::native::emitter) fn emit_provenance_pointer_icmp(
        &mut self,
        pred: Op,
        name: &str,
        lhs: &LlValue,
        rhs: &LlValue,
        instructions: &mut Vec<Instruction>,
    ) -> Result<bool, String> {
        staged_emit(instructions, |instructions| {
            let Some(lhs_provenance) =
                self.normalized_pointer_icmp_provenance(lhs, name, instructions)?
            else {
                return Ok(false);
            };
            let Some(rhs_provenance) =
                self.normalized_pointer_icmp_provenance(rhs, name, instructions)?
            else {
                return Ok(false);
            };
            if !compatible_pointer_provenance(&lhs_provenance, &rhs_provenance) {
                return Ok(false);
            }
            let Some(equal) = self.emit_pointer_index_equality(
                &lhs_provenance.indices,
                &rhs_provenance.indices,
                instructions,
            )?
            else {
                return Ok(false);
            };

            let result_ty = LlType::Bool;
            let result_type = self.type_id(&result_ty)?;
            let result = self.result_id(name, &result_ty)?;
            match pred {
                Op::IEqual => instructions.push(Self::inst(
                    Op::CopyObject,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(equal)],
                )),
                Op::INotEqual => instructions.push(Self::inst(
                    Op::LogicalNot,
                    Some(result_type),
                    Some(result),
                    vec![Operand::IdRef(equal)],
                )),
                _ => return Ok(false),
            }
            Ok(true)
        })
    }

    pub(in crate::native::emitter) fn normalized_pointer_icmp_provenance(
        &mut self,
        value: &LlValue,
        name: &str,
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<GepProvenance>, String> {
        let LlValue::Local(local) = value else {
            return Ok(None);
        };
        let Some(provenance) = self.gep_provenance.get(local).cloned() else {
            return Ok(None);
        };
        self.flatten_pointer_icmp_provenance(provenance, name, instructions, 0)
            .map(Some)
    }

    pub(in crate::native::emitter) fn flatten_pointer_icmp_provenance(
        &mut self,
        provenance: GepProvenance,
        name: &str,
        instructions: &mut Vec<Instruction>,
        depth: usize,
    ) -> Result<GepProvenance, String> {
        if depth >= 8 {
            return Ok(provenance);
        }
        let Some(root_name) = self.provenance_root_local_name(provenance.root) else {
            return Ok(provenance);
        };
        let Some(root_provenance) = self.gep_provenance.get(&root_name).cloned() else {
            return Ok(provenance);
        };
        let root_provenance =
            self.flatten_pointer_icmp_provenance(root_provenance, name, instructions, depth + 1)?;
        let Some(indices) = self.compose_followup_gep(
            name,
            &root_provenance,
            &provenance.source_ty,
            &provenance.indices,
            instructions,
        )?
        else {
            return Ok(provenance);
        };
        Ok(GepProvenance {
            root: root_provenance.root,
            addrspace: root_provenance.addrspace,
            source_ty: root_provenance.source_ty,
            indices,
            root_indices: None,
            root_is_indexed_container: root_provenance.root_is_indexed_container,
        })
    }

    pub(in crate::native::emitter) fn provenance_root_local_name(
        &self,
        root: Word,
    ) -> Option<String> {
        self.values.iter().find_map(|(name, (id, _))| {
            (*id == root && self.gep_provenance.contains_key(name)).then(|| name.clone())
        })
    }

    pub(in crate::native::emitter) fn emit_pointer_index_equality(
        &mut self,
        lhs: &[TypedValue],
        rhs: &[TypedValue],
        instructions: &mut Vec<Instruction>,
    ) -> Result<Option<Word>, String> {
        if lhs.len() != rhs.len() {
            return Ok(None);
        }
        let result_type = self.type_id(&LlType::Bool)?;
        let mut equal = None;
        for (lhs, rhs) in lhs.iter().zip(rhs) {
            let lhs_ty = self.resolve_type(&lhs.ty)?;
            let rhs_ty = self.resolve_type(&rhs.ty)?;
            if lhs_ty != rhs_ty || !matches!(lhs_ty, LlType::Int(_)) {
                return Ok(None);
            }
            let lhs_id = self.value_id_in(&lhs.value, &lhs.ty, instructions)?;
            let rhs_id = self.value_id_in(&rhs.value, &rhs.ty, instructions)?;
            let index_equal = if lhs_id == rhs_id {
                self.const_bool(true)?
            } else {
                let index_equal = self.fresh();
                instructions.push(Self::inst(
                    Op::IEqual,
                    Some(result_type),
                    Some(index_equal),
                    vec![Operand::IdRef(lhs_id), Operand::IdRef(rhs_id)],
                ));
                index_equal
            };
            equal = Some(if let Some(prev_equal) = equal {
                if index_equal == self.const_bool(true)? {
                    prev_equal
                } else {
                    let combined = self.fresh();
                    instructions.push(Self::inst(
                        Op::LogicalAnd,
                        Some(result_type),
                        Some(combined),
                        vec![Operand::IdRef(prev_equal), Operand::IdRef(index_equal)],
                    ));
                    combined
                }
            } else {
                index_equal
            });
        }
        Ok(Some(equal.unwrap_or(self.const_bool(true)?)))
    }
}
