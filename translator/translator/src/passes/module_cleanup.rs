use super::*;

fn require_extension(ctx: &mut Ctx, ext: &str) {
    let have = ctx
        .module
        .extensions
        .iter()
        .any(|e| matches!(e.operands.first(), Some(Operand::LiteralString(s)) if s == ext));
    if !have {
        ctx.module.extensions.push(Instruction::new(
            Op::Extension,
            None,
            None,
            vec![Operand::LiteralString(ext.into())],
        ));
    }
}

fn needed_variable_pointer_capabilities(ctx: &Ctx) -> (bool, bool) {
    crate::spirv_variable_ptr::variable_pointer_requirement(&ctx.module)
}

fn defined_ids(module: &Module) -> HashSet<Word> {
    let mut s = HashSet::default();
    for inst in &module.types_global_values {
        if let Some(id) = inst.result_id {
            s.insert(id);
        }
    }
    for inst in &module.ext_inst_imports {
        if let Some(id) = inst.result_id {
            s.insert(id);
        }
    }
    for f in &module.functions {
        if let Some(id) = f.def.as_ref().and_then(|d| d.result_id) {
            s.insert(id);
        }
        for p in &f.parameters {
            if let Some(id) = p.result_id {
                s.insert(id);
            }
        }
        for b in &f.blocks {
            if let Some(id) = b.label.as_ref().and_then(|l| l.result_id) {
                s.insert(id);
            }
            for inst in &b.instructions {
                if let Some(id) = inst.result_id {
                    s.insert(id);
                }
            }
        }
    }
    s
}

pub(super) fn function_referenced_ids(module: &Module) -> HashSet<Word> {
    let mut s = HashSet::default();
    for f in &module.functions {
        for b in &f.blocks {
            for inst in &b.instructions {
                for op in &inst.operands {
                    if let Operand::IdRef(r) | Operand::IdScope(r) | Operand::IdMemorySemantics(r) =
                        op
                    {
                        s.insert(*r);
                    }
                }
            }
        }
    }
    s
}

pub(super) fn drop_dead_unreferenced_variables(
    ctx: &mut Ctx,
    function_refs: &HashSet<Word>,
    interface_ids: &HashSet<Word>,
) {
    ctx.module.types_global_values.retain(|inst| {
        if inst.class.opcode != Op::Variable {
            return true;
        }
        let Some(id) = inst.result_id else {
            return true;
        };
        function_refs.contains(&id) || interface_ids.contains(&id)
    });
}

fn targets_any(instruction: &Instruction, removed: &HashSet<Word>) -> bool {
    matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if removed.contains(id))
}

pub(crate) fn drop_unreferenced_global_variables(module: &mut Module) -> bool {
    const INTERFACE_START: usize = 3;

    let referenced = function_referenced_ids(module);
    let mut changed = false;
    for entry_point in &mut module.entry_points {
        let before = entry_point.operands.len();
        entry_point.operands = std::mem::take(&mut entry_point.operands)
            .into_iter()
            .enumerate()
            .filter(|(index, operand)| match operand {
                Operand::IdRef(id) if *index >= INTERFACE_START => referenced.contains(id),
                _ => true,
            })
            .map(|(_, operand)| operand)
            .collect();
        changed |= entry_point.operands.len() != before;
    }
    let removed = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
        .filter_map(|instruction| instruction.result_id)
        .filter(|id| !referenced.contains(id))
        .collect::<HashSet<_>>();
    if removed.is_empty() {
        return changed;
    }
    module.types_global_values.retain(|instruction| {
        instruction
            .result_id
            .is_none_or(|id| !removed.contains(&id))
    });
    module
        .annotations
        .retain(|instruction| !targets_any(instruction, &removed));
    module
        .debug_names
        .retain(|instruction| !targets_any(instruction, &removed));
    true
}

pub(crate) fn drop_dangling_debug(module: &mut Module) {
    let defined = defined_ids(module);
    let keep = |inst: &Instruction| -> bool {
        match inst.operands.first() {
            Some(Operand::IdRef(id)) => defined.contains(id),
            _ => true,
        }
    };
    module.debug_names.retain(&keep);
    module.annotations.retain(&keep);
}

const LAYOUT_DECORATIONS: &[spirv::Decoration] = &[
    spirv::Decoration::ArrayStride,
    spirv::Decoration::Block,
    spirv::Decoration::BufferBlock,
    spirv::Decoration::ColMajor,
    spirv::Decoration::MatrixStride,
    spirv::Decoration::Offset,
    spirv::Decoration::RowMajor,
];

pub(super) fn gc_dead_globals(ctx: &mut Ctx) {
    let collect = |inst: &Instruction, live: &mut HashSet<Word>| {
        if let Some(ty) = inst.result_type {
            live.insert(ty);
        }
        for operand in &inst.operands {
            if let Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) =
                operand
            {
                live.insert(*id);
            }
        }
    };

    let mut live = HashSet::default();
    live.extend(
        ctx.emit_sidecar
            .local_pointer_field_stores
            .iter()
            .map(|fact| fact.id),
    );
    live.extend(ctx.emit_sidecar.buffer_root_source_types.values().copied());
    for section in [&ctx.module.entry_points, &ctx.module.execution_modes] {
        for instruction in section {
            collect(instruction, &mut live);
        }
    }
    for instruction in &ctx.module.annotations {
        if instruction.operands.iter().any(|operand| {
            matches!(operand, Operand::Decoration(decoration) if LAYOUT_DECORATIONS.contains(decoration))
        }) {
            continue;
        }
        collect(instruction, &mut live);
    }
    for function in &ctx.module.functions {
        if let Some(definition) = &function.def {
            collect(definition, &mut live);
        }
        for parameter in &function.parameters {
            collect(parameter, &mut live);
        }
        for block in &function.blocks {
            if let Some(label) = &block.label {
                collect(label, &mut live);
            }
            for instruction in &block.instructions {
                collect(instruction, &mut live);
            }
        }
    }

    let definitions = ctx
        .module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction)))
        .collect::<HashMap<_, _>>();
    let mut pending = live.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let Some(definition) = definitions.get(&id) else {
            continue;
        };
        let mut dependencies = HashSet::default();
        collect(definition, &mut dependencies);
        for dependency in dependencies {
            if live.insert(dependency) {
                pending.push(dependency);
            }
        }
    }

    ctx.module
        .types_global_values
        .retain(|instruction| instruction.result_id.is_none_or(|id| live.contains(&id)));
    drop_dangling_debug(&mut ctx.module);
}

pub(crate) fn drop_unused_scalar_width_capabilities(module: &mut Module) {
    let used: HashSet<spirv::Capability> = module
        .types_global_values
        .iter()
        .filter(|i| matches!(i.class.opcode, Op::TypeInt | Op::TypeFloat))
        .filter_map(|i| match i.operands.first() {
            Some(&Operand::LiteralBit32(width)) => {
                crate::native::scalar_width_capability(i.class.opcode, width)
            }
            _ => None,
        })
        .collect();
    module.capabilities.retain(|c| match c.operands.first() {
        Some(&Operand::Capability(capability)) => {
            !matches!(
                capability,
                spirv::Capability::Int8
                    | spirv::Capability::Int16
                    | spirv::Capability::Int64
                    | spirv::Capability::Float16
                    | spirv::Capability::Float64
            ) || used.contains(&capability)
        }
        _ => true,
    });
}

const GRAMMAR_COMPLETE_CAPABILITIES: &[spirv::Capability] = &[
    spirv::Capability::DemoteToHelperInvocation,
    spirv::Capability::FloatControls2,
    spirv::Capability::GroupNonUniformArithmetic,
    spirv::Capability::GroupNonUniformBallot,
    spirv::Capability::GroupNonUniformClustered,
    spirv::Capability::GroupNonUniformQuad,
    spirv::Capability::GroupNonUniformShuffle,
    spirv::Capability::GroupNonUniformShuffleRelative,
    spirv::Capability::GroupNonUniformVote,
    spirv::Capability::ImageQuery,
    spirv::Capability::StorageImageExtendedFormats,
];

pub(crate) fn drop_unreferenced_scalar_types(module: &mut Module) {
    let scalars: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|instruction| matches!(instruction.class.opcode, Op::TypeInt | Op::TypeFloat))
        .filter_map(|instruction| instruction.result_id)
        .collect();
    if scalars.is_empty() {
        return;
    }
    let mut referenced: HashSet<Word> = HashSet::default();
    for instruction in module.all_inst_iter() {
        if matches!(
            instruction.class.opcode,
            Op::Name | Op::MemberName | Op::Decorate | Op::MemberDecorate | Op::DecorateId
        ) {
            continue;
        }
        referenced.extend(instruction.result_type.filter(|ty| scalars.contains(ty)));
        for operand in &instruction.operands {
            if let Operand::IdRef(id) = operand {
                if scalars.contains(id) && instruction.result_id != Some(*id) {
                    referenced.insert(*id);
                }
            }
        }
    }
    module.types_global_values.retain(|instruction| {
        instruction
            .result_id
            .is_none_or(|id| !scalars.contains(&id) || referenced.contains(&id))
    });
    drop_dangling_debug(module);
}

pub(crate) fn drop_unrequired_capabilities(module: &mut Module) {
    let mut required: HashSet<spirv::Capability> = HashSet::default();
    let mut required_extensions: HashSet<&'static str> = HashSet::default();
    for instruction in module.all_inst_iter() {
        if matches!(instruction.class.opcode, Op::Capability | Op::Extension) {
            continue;
        }
        if let Some((capabilities, extensions)) =
            crate::spirv_binary::instruction_declaration_requirements(instruction.class.opcode)
        {
            required.extend(capabilities.iter().copied());
            required_extensions.extend(extensions.iter().copied());
        }
        for operand in &instruction.operands {
            for requirement in crate::spirv_binary::operand_declaration_requirements(operand) {
                required.extend(requirement.capabilities.iter().copied());
                required_extensions.extend(requirement.extensions.iter().copied());
            }
        }
        if let (Op::TypeInt | Op::TypeFloat, Some(&Operand::LiteralBit32(width))) =
            (instruction.class.opcode, instruction.operands.first())
        {
            required.extend(crate::native::scalar_width_capability(
                instruction.class.opcode,
                width,
            ));
        }
    }
    let mut frontier: Vec<spirv::Capability> = module
        .capabilities
        .iter()
        .filter_map(|instruction| match instruction.operands.first() {
            Some(&Operand::Capability(capability)) => Some(capability),
            _ => None,
        })
        .filter(|capability| {
            !GRAMMAR_COMPLETE_CAPABILITIES.contains(capability) || required.contains(capability)
        })
        .collect();
    while let Some(capability) = frontier.pop() {
        for requirement in
            crate::spirv_binary::operand_declaration_requirements(&Operand::Capability(capability))
        {
            if let [prerequisite] = requirement.capabilities {
                if required.insert(*prerequisite) {
                    frontier.push(*prerequisite);
                }
            }
            required_extensions.extend(requirement.extensions.iter().copied());
        }
    }
    let (needs_storage_buffer_pointers, needs_other_pointers) =
        crate::spirv_variable_ptr::variable_pointer_requirement(module);
    module
        .capabilities
        .retain(|instruction| match instruction.operands.first() {
            Some(&Operand::Capability(spirv::Capability::VariablePointersStorageBuffer)) => {
                needs_storage_buffer_pointers || needs_other_pointers
            }
            Some(&Operand::Capability(spirv::Capability::VariablePointers)) => needs_other_pointers,
            Some(&Operand::Capability(capability)) => {
                !GRAMMAR_COMPLETE_CAPABILITIES.contains(&capability)
                    || required.contains(&capability)
            }
            _ => true,
        });
    let droppable_extensions: HashSet<&'static str> = GRAMMAR_COMPLETE_CAPABILITIES
        .iter()
        .flat_map(|capability| {
            crate::spirv_binary::operand_declaration_requirements(&Operand::Capability(*capability))
        })
        .flat_map(|requirement| requirement.extensions.iter().copied())
        .collect();
    module
        .extensions
        .retain(|instruction| match instruction.operands.first() {
            Some(Operand::LiteralString(extension)) => {
                !droppable_extensions.contains(extension.as_str())
                    || required_extensions.contains(extension.as_str())
            }
            _ => true,
        });
}

pub(super) fn drop_unused_variable_pointer_capabilities(ctx: &mut Ctx) -> (bool, bool) {
    crate::spirv_variable_ptr::lower_storage_buffer_pointer_phis(&mut ctx.module);
    crate::spirv_variable_ptr::lower_zero_base_storage_buffer_ptr_access_chains(&mut ctx.module);
    crate::spirv_variable_ptr::rewrite_storage_buffer_atomic_scopes(&mut ctx.module);
    let (needs_storage_buffer, needs_other) = needed_variable_pointer_capabilities(ctx);
    ctx.module
        .capabilities
        .retain(|c| match c.operands.first() {
            Some(Operand::Capability(spirv::Capability::VariablePointersStorageBuffer)) => {
                needs_storage_buffer
            }
            Some(Operand::Capability(spirv::Capability::VariablePointers)) => needs_other,
            _ => true,
        });
    (needs_storage_buffer, needs_other)
}

pub(super) fn add_needed_capabilities(ctx: &mut Ctx, variable_pointer_requirements: (bool, bool)) {
    use spirv::Capability;
    let mut want: Vec<Capability> = vec![];
    let formatless_storage_types: HashSet<Word> = ctx.module.types_global_values.iter()
        .filter(|i| i.class.opcode == Op::TypeImage
            && i.operands.get(5) == Some(&Operand::LiteralBit32(2))
            && i.operands.get(6) == Some(&Operand::ImageFormat(spirv::ImageFormat::Unknown)))
        .filter_map(|i| i.result_id)
        .collect();
    let value_types: HashMap<Word, Word> = ctx.module.all_inst_iter()
        .filter_map(|i| Some((i.result_id?, i.result_type?)))
        .collect();
    for instruction in ctx.module.functions.iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.instructions)
    {
        let capability = match instruction.class.opcode {
            Op::ImageRead => Capability::StorageImageReadWithoutFormat,
            Op::ImageWrite => Capability::StorageImageWriteWithoutFormat,
            _ => continue,
        };
        let Some(Operand::IdRef(image)) = instruction.operands.first() else { continue; };
        if value_types.get(image).is_some_and(|ty| formatless_storage_types.contains(ty))
            && !want.contains(&capability)
        {
            want.push(capability);
        }
    }
    if ctx.module.entry_points.iter().any(|instruction| {
        instruction.operands.first()
            == Some(&Operand::ExecutionModel(
                spirv::ExecutionModel::TessellationEvaluation,
            ))
    }) {
        want.push(Capability::Tessellation);
    }
    let has_demote = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::DemoteToHelperInvocation | Op::IsHelperInvocationEXT
            )
        });
    if has_demote {
        want.push(Capability::DemoteToHelperInvocation);
    }
    let image_dim = |dim: Dim, storage: bool| {
        ctx.module.types_global_values.iter().any(|i| {
            i.class.opcode == Op::TypeImage
                && i.operands.get(1) == Some(&Operand::Dim(dim))
                && (i.operands.get(5) == Some(&Operand::LiteralBit32(2))) == storage
        })
    };
    if image_dim(Dim::Dim1D, false) {
        want.push(Capability::Sampled1D);
    }
    if image_dim(Dim::Dim1D, true) {
        want.push(Capability::Image1D);
    }
    if image_dim(Dim::DimBuffer, false) {
        want.push(Capability::SampledBuffer);
    }
    if image_dim(Dim::DimBuffer, true) {
        want.push(Capability::ImageBuffer);
    }
    let has_input_attachment_type = ctx.module.types_global_values.iter().any(|i| {
        i.class.opcode == Op::TypeImage
            && i.operands.get(1) == Some(&Operand::Dim(Dim::DimSubpassData))
    });
    let has_input_attachment_decor = ctx.module.annotations.iter().any(|i| {
        i.class.opcode == Op::Decorate
            && i.operands.get(1) == Some(&Operand::Decoration(Decoration::InputAttachmentIndex))
    });
    if has_input_attachment_type || has_input_attachment_decor {
        want.push(Capability::InputAttachment);
    }
    let has_viewport_index = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::ViewportIndex))
    });
    if has_viewport_index {
        want.push(Capability::ShaderViewportIndex);
        if let Some(header) = ctx.module.header.as_mut() {
            let version = header.version();
            if version < (1, 5) {
                header.set_version(1, 5);
            }
        }
    }
    let has_layer = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::Layer))
    });
    if has_layer {
        want.push(Capability::ShaderLayer);
        if let Some(header) = ctx.module.header.as_mut() {
            let version = header.version();
            if version < (1, 5) {
                header.set_version(1, 5);
            }
        }
    }
    let has_view_index = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::ViewIndex))
    });
    if has_view_index {
        want.push(Capability::MultiView);
    }
    let has_clip_distance = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::ClipDistance))
    });
    if has_clip_distance {
        want.push(Capability::ClipDistance);
    }
    let has_primitive_id = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::PrimitiveId))
    });
    if has_primitive_id
        && !want.iter().any(|capability| {
            matches!(
                capability,
                Capability::Geometry
                    | Capability::Tessellation
                    | Capability::RayTracingKHR
                    | Capability::MeshShadingEXT
            )
        })
    {
        want.push(Capability::Tessellation);
    }
    let has_sample_id = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::SampleId))
    });
    let has_sample_interpolation = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::Sample))
    });
    if has_sample_id || has_sample_interpolation {
        want.push(Capability::SampleRateShading);
    }
    let has_barycentric = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && matches!(
                instruction.operands.get(2),
                Some(&Operand::BuiltIn(BuiltIn::BaryCoordKHR))
                    | Some(&Operand::BuiltIn(BuiltIn::BaryCoordNoPerspKHR))
            )
    });
    if has_barycentric {
        want.push(Capability::FragmentBarycentricKHR);
    }
    let has_stencil_export = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && instruction.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::FragStencilRefEXT))
    });
    if has_stencil_export {
        want.push(Capability::StencilExportEXT);
    }
    let has_draw_parameters = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && matches!(
                instruction.operands.get(2),
                Some(&Operand::BuiltIn(BuiltIn::BaseVertex)) | Some(&Operand::BuiltIn(BuiltIn::BaseInstance))
            )
    });
    if has_draw_parameters {
        want.push(Capability::DrawParameters);
    }
    let has_query = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::ImageQuerySize
                    | Op::ImageQuerySizeLod
                    | Op::ImageQueryLod
                    | Op::ImageQueryLevels
                    | Op::ImageQuerySamples
            )
        });
    if has_query {
        want.push(Capability::ImageQuery);
    }
    let has_group_shuffle = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformShuffle | Op::GroupNonUniformShuffleXor
            )
        });
    if has_group_shuffle {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformShuffle);
    }
    let has_group_shuffle_relative = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformShuffleUp | Op::GroupNonUniformShuffleDown
            )
        });
    if has_group_shuffle_relative {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformShuffleRelative);
    }
    let has_group_elect = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| i.class.opcode == Op::GroupNonUniformElect);
    if has_group_elect {
        want.push(Capability::GroupNonUniform);
    }
    let has_group_vote = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformAll | Op::GroupNonUniformAny | Op::GroupNonUniformAllEqual
            )
        });
    if has_group_vote {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformVote);
    }
    let has_group_ballot = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformBallot
                    | Op::GroupNonUniformBallotBitExtract
                    | Op::GroupNonUniformBallotBitCount
                    | Op::GroupNonUniformBallotFindLSB
                    | Op::GroupNonUniformBallotFindMSB
            )
        });
    if has_group_ballot {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformBallot);
    }
    let has_group_broadcast = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformBroadcast | Op::GroupNonUniformBroadcastFirst
            )
        });
    if has_group_broadcast {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformBallot);
    }
    let has_subgroup_invocation_builtin = ctx.module.annotations.iter().any(|instruction| {
        instruction.class.opcode == Op::Decorate
            && instruction.operands.iter().any(|operand| {
                matches!(
                    operand,
                    Operand::BuiltIn(spirv::BuiltIn::SubgroupLocalInvocationId)
                )
            })
    });
    if has_subgroup_invocation_builtin {
        want.push(Capability::GroupNonUniform);
    }
    let has_group_arithmetic = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            matches!(
                i.class.opcode,
                Op::GroupNonUniformIAdd
                    | Op::GroupNonUniformFAdd
                    | Op::GroupNonUniformSMin
                    | Op::GroupNonUniformUMin
                    | Op::GroupNonUniformFMin
                    | Op::GroupNonUniformSMax
                    | Op::GroupNonUniformUMax
                    | Op::GroupNonUniformFMax
                    | Op::GroupNonUniformBitwiseAnd
                    | Op::GroupNonUniformBitwiseOr
                    | Op::GroupNonUniformBitwiseXor
            )
            && !i.operands.iter().any(|o| {
                matches!(
                    o,
                    Operand::GroupOperation(spirv::GroupOperation::ClusteredReduce)
                )
            })
        });
    if has_group_arithmetic {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformArithmetic);
    }
    let has_clustered_reduce = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| {
            i.operands.iter().any(|o| {
                matches!(
                    o,
                    Operand::GroupOperation(spirv::GroupOperation::ClusteredReduce)
                )
            })
        });
    if has_clustered_reduce {
        want.push(Capability::GroupNonUniform);
        want.push(Capability::GroupNonUniformClustered);
    }
    let has_atomic_fadd = ctx
        .module
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|b| b.instructions.iter())
        .any(|i| i.class.opcode == Op::AtomicFAddEXT);
    if has_atomic_fadd {
        want.push(Capability::AtomicFloat32AddEXT);
    }
    let int_width = |w: u32| {
        ctx.module.types_global_values.iter().any(|i| {
            i.class.opcode == Op::TypeInt && i.operands.first() == Some(&Operand::LiteralBit32(w))
        })
    };
    let has_half = ctx.module.types_global_values.iter().any(|i| {
        i.class.opcode == Op::TypeFloat && i.operands.first() == Some(&Operand::LiteralBit32(16))
    });
    if has_half {
        want.push(Capability::Float16);
    }
    if int_width(8) {
        want.push(Capability::Int8);
    }
    if int_width(16) {
        want.push(Capability::Int16);
    }
    if int_width(64) {
        want.push(Capability::Int64);
    }
    let (has_storage_buffer_pointer_merge, has_other_pointer_merge) = variable_pointer_requirements;
    if has_storage_buffer_pointer_merge {
        want.push(Capability::VariablePointersStorageBuffer);
    }
    if has_other_pointer_merge {
        want.push(Capability::VariablePointers);
    }
    for cap in &want {
        let already = ctx.module.capabilities.iter().any(|c| {
            matches!(c.operands.first(), Some(Operand::Capability(existing)) if existing == cap)
        });
        if !already {
            ctx.module.capabilities.push(Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(*cap)],
            ));
        }
    }
    if want.contains(&Capability::DemoteToHelperInvocation) {
        let ext = "SPV_EXT_demote_to_helper_invocation";
        require_extension(ctx, ext);
    }
    if want.contains(&Capability::AtomicFloat32AddEXT) {
        let ext = "SPV_EXT_shader_atomic_float_add";
        require_extension(ctx, ext);
    }
    if want.contains(&Capability::FragmentBarycentricKHR) {
        let ext = "SPV_KHR_fragment_shader_barycentric";
        require_extension(ctx, ext);
    }
    if want.contains(&Capability::DrawParameters) {
        require_extension(ctx, "SPV_KHR_shader_draw_parameters");
    }
    if want.contains(&Capability::StencilExportEXT) {
        let ext = "SPV_EXT_shader_stencil_export";
        require_extension(ctx, ext);
        let entry_points: Vec<Word> = ctx
            .module
            .entry_points
            .iter()
            .filter(|instruction| {
                instruction.operands.first()
                    == Some(&Operand::ExecutionModel(spirv::ExecutionModel::Fragment))
            })
            .filter_map(|instruction| match instruction.operands.get(1) {
                Some(&Operand::IdRef(entry)) => Some(entry),
                _ => None,
            })
            .collect();
        for entry in entry_points {
            let declared = ctx.module.execution_modes.iter().any(|instruction| {
                instruction.operands.as_slice()
                    == [
                        Operand::IdRef(entry),
                        Operand::ExecutionMode(spirv::ExecutionMode::StencilRefReplacingEXT),
                    ]
            });
            if !declared {
                ctx.module.execution_modes.push(Instruction::new(
                    Op::ExecutionMode,
                    None,
                    None,
                    vec![
                        Operand::IdRef(entry),
                        Operand::ExecutionMode(spirv::ExecutionMode::StencilRefReplacingEXT),
                    ],
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::Module;
    use crate::spirv_module::ModuleHeader;

    fn module_with_demote_declared(body: Vec<Instruction>) -> Module {
        use crate::spirv_module::{Block, Function};
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(10));
        module.capabilities = vec![
            Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(spirv::Capability::Shader)],
            ),
            Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(
                    spirv::Capability::DemoteToHelperInvocation,
                )],
            ),
        ];
        module.extensions = vec![Instruction::new(
            Op::Extension,
            None,
            None,
            vec![Operand::LiteralString(
                "SPV_EXT_demote_to_helper_invocation".to_string(),
            )],
        )];
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(Op::TypeFunction, None, Some(2), vec![Operand::IdRef(1)]),
            Instruction::new(Op::TypeBool, None, Some(3), vec![]),
        ];
        let mut instructions = body;
        instructions.push(Instruction::new(Op::Return, None, None, vec![]));
        module.functions = vec![Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(1),
                Some(4),
                vec![
                    Operand::FunctionControl(spirv::FunctionControl::NONE),
                    Operand::IdRef(2),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(5), vec![])),
                instructions,
            }],
        }];
        module
    }

    fn declares(module: &Module, capability: spirv::Capability) -> bool {
        module.capabilities.iter().any(|instruction| {
            instruction.operands.first() == Some(&Operand::Capability(capability))
        })
    }

    fn declares_extension(module: &Module, extension: &str) -> bool {
        module.extensions.iter().any(|instruction| {
            matches!(instruction.operands.first(), Some(Operand::LiteralString(name)) if name == extension)
        })
    }

    fn module_with_int64_declared(body: Vec<Instruction>) -> Module {
        use crate::spirv_module::{Block, Function};
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(10));
        module.capabilities = vec![
            Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(spirv::Capability::Shader)],
            ),
            Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(spirv::Capability::Int64)],
            ),
        ];
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(Op::TypeFunction, None, Some(2), vec![Operand::IdRef(1)]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
        ];
        module.debug_names = vec![Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(3),
                Operand::LiteralString("ulong".to_string()),
            ],
        )];
        let mut instructions = body;
        instructions.push(Instruction::new(Op::Return, None, None, vec![]));
        module.functions = vec![Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(1),
                Some(4),
                vec![
                    Operand::FunctionControl(spirv::FunctionControl::NONE),
                    Operand::IdRef(2),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(5), vec![])),
                instructions,
            }],
        }];
        module
    }

    fn declares_type(module: &Module, opcode: Op, width: u32) -> bool {
        module.types_global_values.iter().any(|instruction| {
            instruction.class.opcode == opcode
                && instruction.operands.first() == Some(&Operand::LiteralBit32(width))
        })
    }

    #[test]
    fn a_scalar_type_nothing_uses_takes_its_capability_with_it() {
        let mut module = module_with_int64_declared(vec![]);

        drop_unreferenced_scalar_types(&mut module);
        drop_unused_scalar_width_capabilities(&mut module);

        assert!(!declares_type(&module, Op::TypeInt, 64));
        assert!(!declares(&module, spirv::Capability::Int64));
        assert!(
            module.debug_names.is_empty(),
            "the name that described it goes too"
        );
    }

    #[test]
    fn a_scalar_type_an_instruction_results_in_is_kept() {
        let mut module =
            module_with_int64_declared(vec![Instruction::new(Op::Undef, Some(3), Some(6), vec![])]);

        drop_unreferenced_scalar_types(&mut module);
        drop_unused_scalar_width_capabilities(&mut module);

        assert!(declares_type(&module, Op::TypeInt, 64));
        assert!(declares(&module, spirv::Capability::Int64));
    }

    #[test]
    fn a_capability_no_instruction_requires_is_dropped_with_its_extension() {
        let mut module = module_with_demote_declared(vec![]);

        drop_unrequired_capabilities(&mut module);

        assert!(!declares(
            &module,
            spirv::Capability::DemoteToHelperInvocation
        ));
        assert!(!declares_extension(
            &module,
            "SPV_EXT_demote_to_helper_invocation"
        ));
        assert!(
            declares(&module, spirv::Capability::Shader),
            "a capability outside the allowlist is never a candidate"
        );
    }

    #[test]
    fn the_other_instruction_that_needs_a_capability_keeps_it() {
        let mut module = module_with_demote_declared(vec![Instruction::new(
            Op::IsHelperInvocationEXT,
            Some(3),
            Some(6),
            vec![],
        )]);

        drop_unrequired_capabilities(&mut module);

        assert!(declares(
            &module,
            spirv::Capability::DemoteToHelperInvocation
        ));
        assert!(declares_extension(
            &module,
            "SPV_EXT_demote_to_helper_invocation"
        ));
    }

    fn module_with_variable_pointers_declared(body: Vec<Instruction>) -> Module {
        use crate::spirv_module::{Block, Function};
        let (void, fn_ty, uint, pointer, a, b, cond) = (1, 2, 3, 4, 5, 6, 7);
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.capabilities = [
            spirv::Capability::Shader,
            spirv::Capability::VariablePointersStorageBuffer,
            spirv::Capability::VariablePointers,
        ]
        .into_iter()
        .map(|capability| {
            Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(capability)],
            )
        })
        .collect();
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(void), vec![]),
            Instruction::new(
                Op::TypeFunction,
                None,
                Some(fn_ty),
                vec![Operand::IdRef(void)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(pointer),
                vec![
                    Operand::StorageClass(spirv::StorageClass::Workgroup),
                    Operand::IdRef(uint),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(pointer),
                Some(a),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::Variable,
                Some(pointer),
                Some(b),
                vec![Operand::StorageClass(spirv::StorageClass::Workgroup)],
            ),
            Instruction::new(Op::TypeBool, None, Some(cond), vec![]),
            Instruction::new(Op::Undef, Some(cond), Some(8), vec![]),
        ];
        let mut instructions = body;
        instructions.push(Instruction::new(Op::Return, None, None, vec![]));
        module.functions = vec![Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(void),
                Some(9),
                vec![
                    Operand::FunctionControl(spirv::FunctionControl::NONE),
                    Operand::IdRef(fn_ty),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(10), vec![])),
                instructions,
            }],
        }];
        module
    }

    #[test]
    fn variable_pointers_goes_when_the_last_pointer_merge_does() {
        let mut module = module_with_variable_pointers_declared(vec![Instruction::new(
            Op::Select,
            Some(3),
            Some(11),
            vec![Operand::IdRef(8), Operand::IdRef(3), Operand::IdRef(3)],
        )]);

        drop_unrequired_capabilities(&mut module);

        assert!(!declares(&module, spirv::Capability::VariablePointers));
        assert!(!declares(
            &module,
            spirv::Capability::VariablePointersStorageBuffer
        ));
    }

    #[test]
    fn a_workgroup_pointer_select_keeps_variable_pointers() {
        let mut module = module_with_variable_pointers_declared(vec![Instruction::new(
            Op::Select,
            Some(4),
            Some(11),
            vec![Operand::IdRef(8), Operand::IdRef(5), Operand::IdRef(6)],
        )]);

        drop_unrequired_capabilities(&mut module);

        assert!(declares(&module, spirv::Capability::VariablePointers));
        assert!(declares(
            &module,
            spirv::Capability::VariablePointersStorageBuffer
        ));
    }

    fn module_with_one_stranded_descriptor() -> Module {
        use crate::spirv_module::{Block, Function};
        let (uint, pointer, live, stranded, entry, loaded) = (1, 2, 3, 4, 5, 6);
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(7));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(pointer),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(uint),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(pointer),
                Some(live),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Variable,
                Some(pointer),
                Some(stranded),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        for (variable, binding) in [(live, 0), (stranded, 1)] {
            module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(variable),
                    Operand::Decoration(spirv::Decoration::Binding),
                    Operand::LiteralBit32(binding),
                ],
            ));
            module.debug_names.push(Instruction::new(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(variable),
                    Operand::LiteralString(format!("buffer{binding}")),
                ],
            ));
        }
        module.entry_points.push(Instruction::new(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(spirv::ExecutionModel::GLCompute),
                Operand::IdRef(entry),
                Operand::LiteralString("main".to_string()),
                Operand::IdRef(live),
                Operand::IdRef(stranded),
            ],
        ));
        let mut function = Function::new();
        let mut block = Block::new();
        block.instructions.push(Instruction::new(
            Op::Load,
            Some(uint),
            Some(loaded),
            vec![Operand::IdRef(live)],
        ));
        function.blocks.push(block);
        module.functions.push(function);
        module
    }

    const LIVE_VARIABLE: Word = 3;
    const STRANDED_VARIABLE: Word = 4;

    fn variable_ids(module: &Module) -> HashSet<Word> {
        module
            .types_global_values
            .iter()
            .filter(|instruction| instruction.class.opcode == Op::Variable)
            .filter_map(|instruction| instruction.result_id)
            .collect()
    }

    fn ids_named_by(records: &[Instruction]) -> HashSet<Word> {
        records
            .iter()
            .filter_map(|instruction| match instruction.operands.first() {
                Some(Operand::IdRef(id)) => Some(*id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_variable_no_instruction_references_leaves_with_its_interface_entry_and_records() {
        let mut module = module_with_one_stranded_descriptor();

        assert!(drop_unreferenced_global_variables(&mut module));

        assert_eq!(variable_ids(&module), crate::fxhash::set_from([LIVE_VARIABLE]));
        let interface = module.entry_points[0].operands[3..]
            .iter()
            .filter_map(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        assert_eq!(interface, crate::fxhash::set_from([LIVE_VARIABLE]));
        assert!(!ids_named_by(&module.annotations).contains(&STRANDED_VARIABLE));
        assert!(!ids_named_by(&module.debug_names).contains(&STRANDED_VARIABLE));
        assert!(ids_named_by(&module.annotations).contains(&LIVE_VARIABLE));
        assert!(ids_named_by(&module.debug_names).contains(&LIVE_VARIABLE));
    }

    #[test]
    fn the_sweep_reaches_a_fixed_point_in_one_pass() {
        let mut module = module_with_one_stranded_descriptor();
        assert!(drop_unreferenced_global_variables(&mut module));
        assert!(!drop_unreferenced_global_variables(&mut module));
    }

    #[test]
    fn gc_keeps_local_pointer_store_sentinel_rooted_by_typed_sidecar() {
        let ulong = 1;
        let sentinel = 2;
        let dead = 3;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(4));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(ulong),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::ConstantNull, Some(ulong), Some(sentinel), vec![]),
            Instruction::new(Op::ConstantNull, Some(ulong), Some(dead), vec![]),
        ];
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar.local_pointer_field_stores.push(
            crate::emit_sidecar::LocalPointerFieldStore {
                id: sentinel,
                source: 99,
                root: 98,
                indices: Vec::new(),
            },
        );

        gc_dead_globals(&mut ctx);

        let ids = ctx
            .module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id)
            .collect::<HashSet<_>>();
        assert!(ids.contains(&ulong));
        assert!(ids.contains(&sentinel));
        assert!(!ids.contains(&dead));
    }

    #[test]
    fn gc_drops_dead_global_even_when_debug_metadata_names_it() {
        let byte = 1;
        let pointer = 2;
        let null = 3;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(4));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(byte),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(pointer),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(byte),
                ],
            ),
            Instruction::new(Op::ConstantNull, Some(pointer), Some(null), vec![]),
        ];
        module.debug_names.push(Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(null),
                Operand::LiteralString("dead pointer".to_string()),
            ],
        ));
        let mut ctx = Ctx::new(module);

        gc_dead_globals(&mut ctx);

        assert!(ctx.module.types_global_values.is_empty());
        assert!(ctx.module.debug_names.is_empty());
    }

    #[test]
    fn gc_drops_dead_struct_owned_only_by_its_block_and_offset_layout() {
        let word = 1;
        let structure = 2;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(3));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(word),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(structure),
                vec![Operand::IdRef(word), Operand::IdRef(word)],
            ),
        ];
        module.annotations = vec![
            Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(structure),
                    Operand::Decoration(spirv::Decoration::Block),
                ],
            ),
            Instruction::new(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(structure),
                    Operand::LiteralBit32(1),
                    Operand::Decoration(spirv::Decoration::Offset),
                    Operand::LiteralBit32(4),
                ],
            ),
        ];
        let mut ctx = Ctx::new(module);

        gc_dead_globals(&mut ctx);

        assert!(ctx.module.types_global_values.is_empty());
        assert!(ctx.module.annotations.is_empty());
    }

    #[test]
    fn gc_keeps_the_constant_a_workgroup_size_decoration_names() {
        let word = 1;
        let vector = 2;
        let one = 3;
        let size = 4;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(5));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(word),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(vector),
                vec![Operand::IdRef(word), Operand::LiteralBit32(3)],
            ),
            Instruction::new(
                Op::Constant,
                Some(word),
                Some(one),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::ConstantComposite,
                Some(vector),
                Some(size),
                vec![
                    Operand::IdRef(one),
                    Operand::IdRef(one),
                    Operand::IdRef(one),
                ],
            ),
        ];
        module.annotations = vec![Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(size),
                Operand::Decoration(spirv::Decoration::BuiltIn),
                Operand::BuiltIn(spirv::BuiltIn::WorkgroupSize),
            ],
        )];
        let mut ctx = Ctx::new(module);

        gc_dead_globals(&mut ctx);

        assert_eq!(ctx.module.types_global_values.len(), 4);
        assert_eq!(ctx.module.annotations.len(), 1);
    }

    #[test]
    fn gc_drops_dead_pointer_type_owned_only_by_array_stride() {
        let byte = 1;
        let pointer = 2;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(3));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(byte),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(pointer),
                vec![
                    Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                    Operand::IdRef(byte),
                ],
            ),
        ];
        module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(pointer),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(1),
            ],
        ));
        let mut ctx = Ctx::new(module);

        gc_dead_globals(&mut ctx);

        assert!(ctx.module.types_global_values.is_empty());
        assert!(ctx.module.annotations.is_empty());
    }
}
