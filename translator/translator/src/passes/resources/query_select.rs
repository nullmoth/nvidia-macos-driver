use super::super::*;

#[derive(Clone, Copy)]
struct ResourceSelect {
    cond: Word,
    true_value: Word,
    false_value: Word,
}

pub(in crate::passes) fn rewrite_resource_query_selects(ctx: &mut Ctx) -> Result<(), String> {
    let value_types = value_result_types(&ctx.module);
    let pointer_types = pointer_type_storage_classes(&ctx.module);
    let opaque_types: HashSet<Word> = ctx
        .module
        .types_global_values
        .iter()
        .filter(|inst| {
            matches!(
                inst.class.opcode,
                Op::TypeImage | Op::TypeSampledImage | Op::TypeSampler
            )
        })
        .filter_map(|inst| inst.result_id)
        .collect();

    let mut seeds: Vec<(Word, ResourceSelect)> = vec![];
    for function in &ctx.module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if inst.class.opcode != Op::Select {
                    continue;
                }
                let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) else {
                    continue;
                };
                if pointer_types.get(&result_type) != Some(&StorageClass::UniformConstant) {
                    continue;
                }
                let (
                    Some(Operand::IdRef(cond)),
                    Some(Operand::IdRef(true_id)),
                    Some(Operand::IdRef(false_id)),
                ) = (
                    inst.operands.first(),
                    inst.operands.get(1),
                    inst.operands.get(2),
                )
                else {
                    continue;
                };
                let (Some(&true_type), Some(&false_type)) =
                    (value_types.get(true_id), value_types.get(false_id))
                else {
                    continue;
                };
                if true_type != false_type || pointer_types.contains_key(&true_type) {
                    continue;
                }
                seeds.push((
                    result,
                    ResourceSelect {
                        cond: *cond,
                        true_value: *true_id,
                        false_value: *false_id,
                    },
                ));
            }
        }
    }
    if seeds.is_empty() {
        return Ok(());
    }

    let mut candidates: HashMap<Word, ResourceSelect> = HashMap::default();
    'seed: for &(seed_id, sel) in &seeds {
        let mut closure: HashMap<Word, ResourceSelect> = HashMap::default();
        closure.insert(seed_id, sel);
        let mut work = vec![seed_id];
        while let Some(id) = work.pop() {
            let cond = closure[&id].cond;
            for function in &ctx.module.functions {
                for block in &function.blocks {
                    for inst in &block.instructions {
                        if inst.result_id == Some(id) {
                            continue;
                        }
                        if !inst.operands.iter().any(|o| o == &Operand::IdRef(id)) {
                            continue;
                        }
                        if !is_duplicable_resource_use(inst, id) {
                            continue 'seed;
                        }
                        let result = inst.result_id.ok_or(
                            "rewrite_resource_query_selects: duplicable use has no result",
                        )?;
                        let result_type = inst.result_type.ok_or(
                            "rewrite_resource_query_selects: duplicable use has no result type",
                        )?;
                        if opaque_types.contains(&result_type)
                            && closure
                                .insert(
                                    result,
                                    ResourceSelect {
                                        cond,
                                        true_value: 0,
                                        false_value: 0,
                                    },
                                )
                                .is_none()
                        {
                            work.push(result);
                        }
                    }
                }
            }
        }
        candidates.extend(closure);
    }
    if candidates.is_empty() {
        return Ok(());
    }

    let mut rewritten_value_types: HashMap<Word, Word> = HashMap::default();
    for function_idx in 0..ctx.module.functions.len() {
        for block_idx in 0..ctx.module.functions[function_idx].blocks.len() {
            let old = ctx.module.functions[function_idx].blocks[block_idx]
                .instructions
                .clone();
            let mut new_insts = Vec::with_capacity(old.len());
            for inst in old {
                let result = inst.result_id;
                if inst.class.opcode == Op::Select
                    && result.is_some_and(|id| candidates.contains_key(&id))
                {
                    continue;
                }
                let used = inst.operands.iter().find_map(|o| match o {
                    Operand::IdRef(r) if candidates.contains_key(r) => Some(*r),
                    _ => None,
                });
                let Some(used_id) = used else {
                    new_insts.push(inst);
                    continue;
                };
                let sel = candidates[&used_id];
                let result =
                    result.ok_or("rewrite_resource_query_selects: duplicable use has no result")?;
                let result_type = inst
                    .result_type
                    .ok_or("rewrite_resource_query_selects: duplicable use has no result type")?;
                let true_use = ctx.module.fresh_id();
                let false_use = ctx.module.fresh_id();
                let mut true_ops = inst.operands.clone();
                let mut false_ops = inst.operands.clone();
                for (t_op, f_op) in true_ops.iter_mut().zip(false_ops.iter_mut()) {
                    if *t_op == Operand::IdRef(used_id) {
                        *t_op = Operand::IdRef(sel.true_value);
                        *f_op = Operand::IdRef(sel.false_value);
                    }
                }
                let true_result_type = duplicated_resource_use_result_type(
                    ctx,
                    &rewritten_value_types,
                    &inst,
                    result_type,
                    used_id,
                    sel.true_value,
                );
                let false_result_type = duplicated_resource_use_result_type(
                    ctx,
                    &rewritten_value_types,
                    &inst,
                    result_type,
                    used_id,
                    sel.false_value,
                );
                new_insts.push(Instruction::new(
                    inst.class.opcode,
                    Some(true_result_type),
                    Some(true_use),
                    true_ops,
                ));
                new_insts.push(Instruction::new(
                    inst.class.opcode,
                    Some(false_result_type),
                    Some(false_use),
                    false_ops,
                ));
                rewritten_value_types.insert(true_use, true_result_type);
                rewritten_value_types.insert(false_use, false_result_type);
                if let Some(cascade) = candidates.get_mut(&result) {
                    cascade.true_value = true_use;
                    cascade.false_value = false_use;
                } else {
                    new_insts.push(Instruction::new(
                        Op::Select,
                        Some(result_type),
                        Some(result),
                        vec![
                            Operand::IdRef(sel.cond),
                            Operand::IdRef(true_use),
                            Operand::IdRef(false_use),
                        ],
                    ));
                }
            }
            ctx.module.functions[function_idx].blocks[block_idx].instructions = new_insts;
        }
    }
    Ok(())
}

fn duplicated_resource_use_result_type(
    ctx: &mut Ctx,
    local_value_types: &HashMap<Word, Word>,
    inst: &Instruction,
    fallback: Word,
    used_id: Word,
    replacement: Word,
) -> Word {
    if inst.class.opcode != Op::SampledImage
        || inst.operands.first() != Some(&Operand::IdRef(used_id))
    {
        return fallback;
    }
    image_type_for_sampled_operand(ctx, local_value_types, replacement)
        .map(|image_ty| ctx.ty_sampled_image(image_ty))
        .unwrap_or(fallback)
}

fn image_type_for_sampled_operand(
    ctx: &Ctx,
    local_value_types: &HashMap<Word, Word>,
    image: Word,
) -> Option<Word> {
    let ty = local_value_types
        .get(&image)
        .copied()
        .or_else(|| value_result_type(ctx, image))?;
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode == Op::TypeImage {
        return Some(ty);
    }
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    let pointee = match def.operands.get(1)? {
        Operand::IdRef(pointee) => *pointee,
        _ => return None,
    };
    type_def_of(ctx, pointee)
        .filter(|pointee_def| pointee_def.class.opcode == Op::TypeImage)
        .map(|_| pointee)
}

fn is_duplicable_resource_use(inst: &Instruction, id: Word) -> bool {
    matches!(
        inst.class.opcode,
        Op::ImageQuerySize
            | Op::ImageQuerySizeLod
            | Op::ImageQueryLevels
            | Op::ImageQuerySamples
            | Op::SampledImage
            | Op::Image
            | Op::ImageFetch
            | Op::ImageGather
            | Op::ImageDrefGather
            | Op::ImageSampleImplicitLod
            | Op::ImageSampleExplicitLod
            | Op::ImageSampleDrefImplicitLod
            | Op::ImageSampleDrefExplicitLod
            | Op::ImageRead
    ) && inst.operands.first() == Some(&Operand::IdRef(id))
        && inst
            .operands
            .iter()
            .skip(1)
            .all(|o| o != &Operand::IdRef(id))
        && inst.result_id.is_some()
        && inst.result_type.is_some()
}

fn value_result_types(module: &Module) -> HashMap<Word, Word> {
    let mut types = HashMap::default();
    for inst in &module.types_global_values {
        if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
            types.insert(result, result_type);
        }
    }
    for function in &module.functions {
        if let Some(def) = &function.def {
            if let (Some(result), Some(result_type)) = (def.result_id, def.result_type) {
                types.insert(result, result_type);
            }
        }
        for param in &function.parameters {
            if let (Some(result), Some(result_type)) = (param.result_id, param.result_type) {
                types.insert(result, result_type);
            }
        }
        for block in &function.blocks {
            if let Some(label) = &block.label {
                if let (Some(result), Some(result_type)) = (label.result_id, label.result_type) {
                    types.insert(result, result_type);
                }
            }
            for inst in &block.instructions {
                if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                    types.insert(result, result_type);
                }
            }
        }
    }
    types
}

fn pointer_type_storage_classes(module: &Module) -> HashMap<Word, StorageClass> {
    module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::TypePointer {
                return None;
            }
            let id = inst.result_id?;
            let storage = match inst.operands.first()? {
                Operand::StorageClass(storage) => *storage,
                _ => return None,
            };
            Some((id, storage))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;

    fn type_instruction(opcode: Op, result: Word, operands: Vec<Operand>) -> Instruction {
        Instruction::new(opcode, None, Some(result), operands)
    }

    #[test]
    fn duplicated_sampled_images_are_typed_from_their_branch_images_at_construction() {
        let bool_ty = 1;
        let float_ty = 2;
        let image_2d_ty = 3;
        let image_3d_ty = 4;
        let sampler_ty = 5;
        let sampled_2d_ty = 6;
        let sampled_3d_ty = 7;
        let selected_image_ptr_ty = 8;
        let condition = 20;
        let true_image = 21;
        let false_image = 22;
        let sampler = 23;
        let selected_image = 30;
        let sampled_image = 31;

        let image_type = |result, dim| {
            type_instruction(
                Op::TypeImage,
                result,
                vec![
                    Operand::IdRef(float_ty),
                    Operand::Dim(dim),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            )
        };
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            type_instruction(Op::TypeBool, bool_ty, vec![]),
            type_instruction(Op::TypeFloat, float_ty, vec![Operand::LiteralBit32(32)]),
            image_type(image_2d_ty, spirv::Dim::Dim2D),
            image_type(image_3d_ty, spirv::Dim::Dim3D),
            type_instruction(Op::TypeSampler, sampler_ty, vec![]),
            type_instruction(
                Op::TypeSampledImage,
                sampled_2d_ty,
                vec![Operand::IdRef(image_2d_ty)],
            ),
            type_instruction(
                Op::TypeSampledImage,
                sampled_3d_ty,
                vec![Operand::IdRef(image_3d_ty)],
            ),
            type_instruction(
                Op::TypePointer,
                selected_image_ptr_ty,
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(image_2d_ty),
                ],
            ),
        ];
        module.functions = vec![Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![
                    Instruction::new(Op::Undef, Some(bool_ty), Some(condition), vec![]),
                    Instruction::new(Op::Undef, Some(image_2d_ty), Some(true_image), vec![]),
                    Instruction::new(Op::Undef, Some(image_2d_ty), Some(false_image), vec![]),
                    Instruction::new(Op::Undef, Some(sampler_ty), Some(sampler), vec![]),
                    Instruction::new(
                        Op::Select,
                        Some(selected_image_ptr_ty),
                        Some(selected_image),
                        vec![
                            Operand::IdRef(condition),
                            Operand::IdRef(true_image),
                            Operand::IdRef(false_image),
                        ],
                    ),
                    Instruction::new(
                        Op::SampledImage,
                        Some(sampled_3d_ty),
                        Some(sampled_image),
                        vec![Operand::IdRef(selected_image), Operand::IdRef(sampler)],
                    ),
                ],
            }],
        }];

        let mut ctx = Ctx::new(module);
        rewrite_resource_query_selects(&mut ctx).expect("resource select lowering");

        let sampled_images = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .filter(|instruction| instruction.class.opcode == Op::SampledImage)
            .collect::<Vec<_>>();
        assert_eq!(sampled_images.len(), 2);
        assert!(sampled_images
            .iter()
            .all(|instruction| instruction.result_type == Some(sampled_2d_ty)));
        assert!(ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::Select));
    }
}
