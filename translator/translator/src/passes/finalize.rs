use super::*;
use module_cleanup::{
    add_needed_capabilities, drop_dead_unreferenced_variables, drop_unrequired_capabilities,
    drop_unused_scalar_width_capabilities, drop_unused_variable_pointer_capabilities,
    function_referenced_ids, gc_dead_globals,
};

pub(in crate::passes) fn finalize(
    ctx: &mut Ctx,
    entry_idx: usize,
    stage: &Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
) -> Result<(), String> {
    let void = ctx.ty_void();
    let fn_void = ctx.ty_fn_void(void);
    if matches!(stage, Stage::Kernel)
        && !matches!(
            ctx.kernel_dispatch,
            crate::reflect::KernelDispatch::Workgroups
        )
    {
        ctx.kernel_workgroup_size_id();
    }
    {
        let def = ctx.module.functions[entry_idx]
            .def
            .as_mut()
            .ok_or("finalize: entry function has no OpFunction def")?;
        def.result_type = Some(void);
        if let Some(Operand::FunctionControl(_)) = def.operands.first() {
            def.operands[0] = Operand::FunctionControl(FunctionControl::NONE);
        }
        if def.operands.get(1).is_some() {
            def.operands[1] = Operand::IdRef(fn_void);
        }
    }

    ctx.module.types_global_values.append(&mut ctx.new_globals);
    rewrite_resource_query_selects(ctx)?;
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    ctx.module.sync_id_bound_from_instructions();
    let _ = crate::native::construct_interface_cross_binding_pointer_values_module(&mut ctx.module);
    if !ctx.emit_sidecar.all_device_buffers_raw
        || ctx.emit_sidecar.construct_cross_binding_addresses
    {
        if let Some(address_table) =
            crate::native::construct_interface_cross_binding_pointer_merges_module(
                &mut ctx.module,
                ctx.descriptor_layout,
            )
        {
            ctx.interface_buffer_var(address_table);
        }
    }
    crate::native::construct_opaque_image_selects_module(&mut ctx.module);

    let entry_id = ctx.module.functions[entry_idx]
        .def
        .as_ref()
        .and_then(|d| d.result_id)
        .ok_or("finalize: entry function def has no result id")?;
    let tessellation = matches!(stage, Stage::Vertex)
        .then(|| vert.and_then(|meta| meta.tessellation.as_ref()))
        .flatten();
    let exec_model = match (stage, tessellation) {
        (Stage::Vertex, Some(_)) => spirv::ExecutionModel::TessellationEvaluation,
        (Stage::Vertex, None) => spirv::ExecutionModel::Vertex,
        (Stage::Fragment, _) => spirv::ExecutionModel::Fragment,
        (Stage::Kernel, _) => spirv::ExecutionModel::GLCompute,
    };
    let mut ep_operands = vec![
        Operand::ExecutionModel(exec_model),
        Operand::IdRef(entry_id),
        Operand::LiteralString("main".into()),
    ];
    let referenced_from_functions = function_referenced_ids(&ctx.module);
    let mut interface = Vec::new();
    let mut interface_ids = HashSet::default();
    for &id in &ctx.interface {
        if referenced_from_functions.contains(&id) && interface_ids.insert(id) {
            interface.push(id);
        }
    }
    for inst in &ctx.module.types_global_values {
        if inst.class.opcode == Op::Variable
            && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Workgroup))
        {
            if let Some(id) = inst.result_id {
                if referenced_from_functions.contains(&id) && interface_ids.insert(id) {
                    interface.push(id);
                }
            }
        }
    }
    for id in interface {
        ep_operands.push(Operand::IdRef(id));
    }
    ctx.module
        .entry_points
        .push(Instruction::new(Op::EntryPoint, None, None, ep_operands));

    if let Some(tessellation) = tessellation {
        use crate::meta::PatchDomain;
        let domain = match tessellation.domain {
            PatchDomain::Triangle => spirv::ExecutionMode::Triangles,
            PatchDomain::Quad => spirv::ExecutionMode::Quads,
            PatchDomain::Isoline => spirv::ExecutionMode::Isolines,
        };
        for mode in [domain, spirv::ExecutionMode::SpacingEqual] {
            ctx.module.execution_modes.push(Instruction::new(
                Op::ExecutionMode,
                None,
                None,
                vec![Operand::IdRef(entry_id), Operand::ExecutionMode(mode)],
            ));
        }
        if !matches!(tessellation.domain, PatchDomain::Isoline) {
            ctx.module.execution_modes.push(Instruction::new(
                Op::ExecutionMode,
                None,
                None,
                vec![
                    Operand::IdRef(entry_id),
                    Operand::ExecutionMode(spirv::ExecutionMode::VertexOrderCcw),
                ],
            ));
        }
    }

    if matches!(stage, Stage::Fragment) {
        ctx.module.execution_modes.push(Instruction::new(
            Op::ExecutionMode,
            None,
            None,
            vec![
                Operand::IdRef(entry_id),
                Operand::ExecutionMode(spirv::ExecutionMode::OriginUpperLeft),
            ],
        ));
        if frag.is_some_and(|meta| meta.early_fragment_tests) {
            ctx.module.execution_modes.push(Instruction::new(
                Op::ExecutionMode,
                None,
                None,
                vec![
                    Operand::IdRef(entry_id),
                    Operand::ExecutionMode(spirv::ExecutionMode::EarlyFragmentTests),
                ],
            ));
        }
        if ctx.writes_frag_depth {
            ctx.module.execution_modes.push(Instruction::new(
                Op::ExecutionMode,
                None,
                None,
                vec![
                    Operand::IdRef(entry_id),
                    Operand::ExecutionMode(spirv::ExecutionMode::DepthReplacing),
                ],
            ));
        }
        if ctx.uses_fragment_imageblock {
            let capability = spirv::Capability::FragmentShaderPixelInterlockEXT;
            if !ctx.module.capabilities.iter().any(|instruction| {
                instruction.operands.as_slice() == [Operand::Capability(capability)]
            }) {
                ctx.module.capabilities.push(Instruction::new(
                    Op::Capability,
                    None,
                    None,
                    vec![Operand::Capability(capability)],
                ));
            }
            if !ctx.module.extensions.iter().any(|instruction| {
                instruction.operands.first()
                    == Some(&Operand::LiteralString(
                        "SPV_EXT_fragment_shader_interlock".to_string(),
                    ))
            }) {
                ctx.module.extensions.push(Instruction::new(
                    Op::Extension,
                    None,
                    None,
                    vec![Operand::LiteralString(
                        "SPV_EXT_fragment_shader_interlock".to_string(),
                    )],
                ));
            }
            ctx.module.execution_modes.push(Instruction::new(
                Op::ExecutionMode,
                None,
                None,
                vec![
                    Operand::IdRef(entry_id),
                    Operand::ExecutionMode(spirv::ExecutionMode::PixelInterlockOrderedEXT),
                ],
            ));
        }
    }
    if matches!(stage, Stage::Kernel)
        && matches!(
            ctx.kernel_dispatch,
            crate::reflect::KernelDispatch::Workgroups
        )
    {
        let [x, y, z] = ctx.kernel_local_size;
        ctx.module.execution_modes.push(Instruction::new(
            Op::ExecutionMode,
            None,
            None,
            vec![
                Operand::IdRef(entry_id),
                Operand::ExecutionMode(spirv::ExecutionMode::LocalSize),
                Operand::LiteralBit32(x),
                Operand::LiteralBit32(y),
                Operand::LiteralBit32(z),
            ],
        ));
    }
    let air_ids: HashSet<Word> = air_names(&ctx.module).keys().copied().collect();
    ctx.module.functions.retain(|function| {
        let is_decl = function.blocks.is_empty();
        let id = function
            .def
            .as_ref()
            .and_then(|definition| definition.result_id);
        !(is_decl && id.is_some_and(|id| air_ids.contains(&id)))
    });
    ctx.module.functions.retain(|function| {
        !function.blocks.is_empty()
            || function
                .def
                .as_ref()
                .and_then(|definition| definition.result_id)
                .is_none_or(|id| !air_ids.contains(&id))
    });
    ctx.module.functions.retain(|function| {
        !function.blocks.is_empty()
            || function
                .def
                .as_ref()
                .and_then(|definition| definition.result_id)
                .is_some_and(|id| referenced_from_functions.contains(&id))
    });
    if !ctx.module.entry_points.is_empty() {
        if let Some(declaration) = ctx.module.functions.iter().find(|f| f.blocks.is_empty()) {
            let id = declaration.def.as_ref().and_then(|d| d.result_id);
            let name = ctx
                .module
                .debug_names
                .iter()
                .find_map(|i| match i.operands.as_slice() {
                    [Operand::IdRef(target), Operand::LiteralString(name)]
                        if Some(*target) == id =>
                    {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .unwrap_or("unnamed");
            return Err(format!("unresolved external shader function: {name}"));
        }
    }
    ctx.module.debug_names.retain(|instruction| {
        !matches!(
            (instruction.class.opcode, instruction.operands.first()),
            (Op::Name, Some(Operand::IdRef(id))) if air_ids.contains(id)
        )
    });

    drop_dead_unreferenced_variables(ctx, &referenced_from_functions, &interface_ids);
    gc_dead_globals(ctx);
    drop_unused_scalar_width_capabilities(&mut ctx.module);
    drop_unrequired_capabilities(&mut ctx.module);
    let variable_pointer_requirements = drop_unused_variable_pointer_capabilities(ctx);
    add_needed_capabilities(ctx, variable_pointer_requirements);
    if !ctx.bindless_heap_vars.is_empty() {
        finalize_bindless_heap(ctx);
    }
    exact_narrowing_float_conversions(ctx);
    metal_nan_to_zero_conversions(ctx);
    order_module_scope_dependencies(&mut ctx.module)?;

    Ok(())
}

fn metal_nan_to_zero_conversions(ctx: &mut Ctx) {
    let mut plan: Vec<(usize, usize, usize, Word, Word)> = Vec::new();
    for (f, function) in ctx.module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if !matches!(inst.class.opcode, Op::ConvertFToU | Op::ConvertFToS) {
                    continue;
                }
                if let (Some(_), Some(result_ty), Some(Operand::IdRef(source))) =
                    (inst.result_id, inst.result_type, inst.operands.first())
                {
                    plan.push((f, bi, ii, *source, result_ty));
                }
            }
        }
    }
    if plan.is_empty() {
        return;
    }
    let mut shapes: Vec<(Word, Word)> = Vec::with_capacity(plan.len());
    for &(_, _, _, source, result_ty) in &plan {
        let lanes = value_result_type(ctx, source)
            .and_then(|ty| type_def_of(ctx, ty))
            .and_then(|def| match (def.class.opcode, def.operands.get(1)) {
                (Op::TypeVector, Some(Operand::LiteralBit32(n))) => Some(*n),
                _ => None,
            });
        let bool_ty = match lanes {
            Some(n) => ctx.ty_vec_bool(n),
            None => ctx.ty_bool(),
        };
        let zero = ctx.get_or_create(Op::ConstantNull, Some(result_ty), vec![]);
        shapes.push((bool_ty, zero));
    }
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    for (k, &(f, bi, ii, source, result_ty)) in plan.iter().enumerate().rev() {
        let (bool_ty, zero) = shapes[k];
        let converted = ctx.module.fresh_id();
        let is_nan = ctx.module.fresh_id();
        let block = &mut ctx.module.functions[f].blocks[bi];
        let mut original = block.instructions[ii].clone();
        let result = original.result_id.expect("planned with a result");
        original.result_id = Some(converted);
        let nan = Instruction::new(
            Op::IsNan,
            Some(bool_ty),
            Some(is_nan),
            vec![Operand::IdRef(source)],
        );
        let select = Instruction::new(
            Op::Select,
            Some(result_ty),
            Some(result),
            vec![
                Operand::IdRef(is_nan),
                Operand::IdRef(zero),
                Operand::IdRef(converted),
            ],
        );
        block.instructions.splice(ii..=ii, [original, nan, select]);
    }
}

fn exact_narrowing_float_conversions(ctx: &mut Ctx) {
    fn float_width(ctx: &Ctx, ty: Word) -> Option<u32> {
        let def = type_def_of(ctx, ty)?;
        match def.class.opcode {
            Op::TypeFloat => match def.operands.first() {
                Some(Operand::LiteralBit32(width)) => Some(*width),
                _ => None,
            },
            Op::TypeVector => match def.operands.first() {
                Some(Operand::IdRef(element)) => float_width(ctx, *element),
                _ => None,
            },
            _ => None,
        }
    }
    let conversions: Vec<(Word, Word, Word)> = ctx
        .module
        .functions
        .iter()
        .flat_map(|function| function.blocks.iter())
        .flat_map(|block| block.instructions.iter())
        .filter(|inst| inst.class.opcode == Op::FConvert)
        .filter_map(
            |inst| match (inst.result_id, inst.result_type, inst.operands.first()) {
                (Some(result), Some(result_ty), Some(Operand::IdRef(source))) => {
                    Some((result, result_ty, *source))
                }
                _ => None,
            },
        )
        .collect();
    let narrowing: crate::fxhash::HashSet<Word> = conversions
        .iter()
        .filter(|(_, result_ty, source)| {
            match (
                float_width(ctx, *result_ty),
                value_result_type(ctx, *source).and_then(|ty| float_width(ctx, ty)),
            ) {
                (Some(dst), Some(src)) => dst < src,
                _ => false,
            }
        })
        .map(|(result, _, _)| *result)
        .collect();
    if narrowing.is_empty() {
        return;
    }
    let already: crate::fxhash::HashSet<Word> = ctx
        .module
        .annotations
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::Decorate
                && inst.operands.get(1) == Some(&Operand::Decoration(Decoration::NoContraction))
        })
        .filter_map(|inst| match inst.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .collect();
    let mut marked: Vec<Word> = conversions
        .iter()
        .filter(|(result, _, source)| narrowing.contains(result) || narrowing.contains(source))
        .map(|(result, _, _)| *result)
        .filter(|result| !already.contains(result))
        .collect();
    marked.sort_unstable();
    marked.dedup();
    for id in marked {
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(id),
                Operand::Decoration(Decoration::NoContraction),
            ],
        ));
    }
}

fn finalize_bindless_heap(ctx: &mut Ctx) {
    use std::collections::BTreeSet;
    for capability in [
        spirv::Capability::RuntimeDescriptorArray,
        spirv::Capability::ShaderNonUniform,
        spirv::Capability::SampledImageArrayNonUniformIndexing,
    ] {
        ctx.add_capability(capability);
    }
    let kinds: Vec<(spirv::Dim, u32)> = ctx
        .bindless_heap_vars
        .keys()
        .filter_map(|ty| type_def_of(ctx, *ty))
        .filter(|def| def.class.opcode == Op::TypeImage)
        .filter_map(|def| match (def.operands.get(1), def.operands.get(5)) {
            (Some(Operand::Dim(dim)), Some(Operand::LiteralBit32(sampled))) => {
                Some((*dim, *sampled))
            }
            _ => None,
        })
        .collect();
    for (dim, sampled) in kinds {
        ctx.add_capability(match (dim == spirv::Dim::DimBuffer, sampled) {
            (false, 2) => spirv::Capability::StorageImageArrayNonUniformIndexing,
            (true, 2) => spirv::Capability::StorageTexelBufferArrayNonUniformIndexing,
            (true, _) => spirv::Capability::UniformTexelBufferArrayNonUniformIndexing,
            _ => spirv::Capability::SampledImageArrayNonUniformIndexing,
        });
    }
    let ext = "SPV_EXT_descriptor_indexing".to_string();
    if !ctx.module.extensions.iter().any(|instruction| {
        instruction.operands.first() == Some(&Operand::LiteralString(ext.clone()))
    }) {
        ctx.module.extensions.push(Instruction::new(
            Op::Extension,
            None,
            None,
            vec![Operand::LiteralString(ext)],
        ));
    }
    let heaps: BTreeSet<Word> = ctx.bindless_heap_vars.values().copied().collect();
    let mut chains: BTreeSet<Word> = BTreeSet::new();
    let mut marked: BTreeSet<Word> = BTreeSet::new();
    loop {
        let before = marked.len() + chains.len();
        for function in &ctx.module.functions {
            for block in &function.blocks {
                for inst in &block.instructions {
                    let Some(result) = inst.result_id else {
                        continue;
                    };
                    let first = match inst.operands.first() {
                        Some(Operand::IdRef(id)) => *id,
                        _ => continue,
                    };
                    match inst.class.opcode {
                        Op::AccessChain | Op::InBoundsAccessChain if heaps.contains(&first) => {
                            chains.insert(result);
                            marked.insert(result);
                            if let Some(Operand::IdRef(index)) = inst.operands.get(1) {
                                marked.insert(*index);
                            }
                        }
                        Op::Load if chains.contains(&first) => {
                            marked.insert(result);
                        }
                        Op::SampledImage | Op::CopyObject | Op::Image
                            if marked.contains(&first) =>
                        {
                            marked.insert(result);
                        }
                        _ => {}
                    }
                }
            }
        }
        if marked.len() + chains.len() == before {
            break;
        }
    }
    for id in marked {
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(id),
                Operand::Decoration(spirv::Decoration::NonUniform),
            ],
        ));
    }
}

fn order_module_scope_dependencies(module: &mut Module) -> Result<(), String> {
    let definitions = module
        .types_global_values
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| instruction.result_id.map(|id| (id, index)))
        .collect::<HashMap<_, _>>();
    let forward_pointers = module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::TypeForwardPointer)
        .filter_map(|instruction| match instruction.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut dependencies = vec![Vec::new(); module.types_global_values.len()];
    for (index, instruction) in module.types_global_values.iter().enumerate() {
        let ids =
            instruction
                .result_type
                .into_iter()
                .chain(
                    instruction
                        .operands
                        .iter()
                        .filter_map(|operand| match operand {
                            Operand::IdRef(id)
                            | Operand::IdMemorySemantics(id)
                            | Operand::IdScope(id) => Some(*id),
                            _ => None,
                        }),
                );
        for id in ids {
            if forward_pointers.contains(&id) {
                continue;
            }
            if let Some(&dependency) = definitions.get(&id) {
                if dependency != index {
                    dependencies[index].push(dependency);
                }
            }
        }
        dependencies[index].sort_unstable();
        dependencies[index].dedup();
    }

    fn visit(
        index: usize,
        dependencies: &[Vec<usize>],
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<(), String> {
        match state[index] {
            2 => return Ok(()),
            1 => {
                return Err(format!(
                    "module-scope definitions contain a dependency cycle at instruction {index}"
                ));
            }
            _ => {}
        }
        state[index] = 1;
        for &dependency in &dependencies[index] {
            visit(dependency, dependencies, state, order)?;
        }
        state[index] = 2;
        order.push(index);
        Ok(())
    }

    let mut state = vec![0u8; dependencies.len()];
    let mut order = Vec::with_capacity(dependencies.len());
    for index in 0..dependencies.len() {
        visit(index, &dependencies, &mut state, &mut order)?;
    }
    let mut ranks = vec![0usize; order.len()];
    for (rank, index) in order.into_iter().enumerate() {
        ranks[index] = rank;
    }
    let original_indices = module
        .types_global_values
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| instruction.result_id.map(|id| (id, index)))
        .collect::<HashMap<_, _>>();
    let forward_ranks = module
        .types_global_values
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            (instruction.class.opcode == Op::TypeForwardPointer)
                .then(|| match instruction.operands.first() {
                    Some(Operand::IdRef(id)) => Some((*id, ranks[index])),
                    _ => None,
                })
                .flatten()
        })
        .collect::<HashMap<_, _>>();
    module.types_global_values.sort_by_key(|instruction| {
        instruction
            .result_id
            .and_then(|id| original_indices.get(&id).copied())
            .map(|index| ranks[index])
            .or_else(|| {
                (instruction.class.opcode == Op::TypeForwardPointer)
                    .then(|| match instruction.operands.first() {
                        Some(Operand::IdRef(id)) => forward_ranks.get(id).copied(),
                        _ => None,
                    })
                    .flatten()
            })
            .unwrap_or(usize::MAX)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalization_orders_late_aggregate_type_dependencies_before_existing_users() {
        let mut module = Module::new();
        module.types_global_values = vec![
            Instruction::new(Op::TypeStruct, None, Some(1), vec![Operand::IdRef(4)]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(3),
                vec![Operand::LiteralBit32(11)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(4),
                vec![Operand::IdRef(2), Operand::IdRef(3)],
            ),
        ];

        order_module_scope_dependencies(&mut module).unwrap();

        assert_eq!(
            module
                .types_global_values
                .iter()
                .filter_map(|instruction| instruction.result_id)
                .collect::<Vec<_>>(),
            vec![2, 3, 4, 1]
        );
    }
}
