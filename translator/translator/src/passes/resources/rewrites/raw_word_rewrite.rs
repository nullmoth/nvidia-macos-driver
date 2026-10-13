use super::*;
use crate::passes::access::{flatten_affine_offset_roots, inherited_affine_byte_offset};

#[derive(Clone, Debug)]
pub(in crate::passes) struct BufferAccessPath {
    pub(in crate::passes) root: Word,
    pub(in crate::passes) indices: Vec<Word>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::passes) enum RawStoreObject {
    Float32,
    Uint32,
}

pub(in crate::passes) fn rewrite_raw_word_alias_chains(
    ctx: &mut Ctx,
    entry_idx: usize,
    buffer_structs: &[(Word, Word)],
    defs: &HashMap<Word, Instruction>,
) -> Result<(), String> {
    let buffer_types: HashMap<Word, Word> = buffer_structs.iter().copied().collect();
    if buffer_types.is_empty() {
        return Ok(());
    }
    let mut types = combined_type_defs(ctx, defs);
    let mut exact_raw_facts = HashMap::default();
    for fact in &ctx.emit_sidecar.buffer_access_offsets {
        let Some(block) = buffer_types.get(&fact.root) else {
            continue;
        };
        if is_raw_uint_word_block(&types, *block) {
            let Some(byte_offset) = u32::try_from(fact.byte_offset).ok() else {
                continue;
            };
            exact_raw_facts
                .entry(fact.id)
                .or_insert((fact.root, byte_offset));
        }
    }
    let fact_parents = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            if !matches!(
                instruction.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::CopyObject
            ) || instruction.operands.iter().skip(1).any(
                |operand| !matches!(operand, Operand::IdRef(id) if const_u32(&types, *id) == Some(0)),
            ) {
                return None;
            }
            let result = instruction.result_id?;
            let Operand::IdRef(base) = instruction.operands.first()? else {
                return None;
            };
            Some((result, *base))
        })
        .collect::<HashMap<_, _>>();

    let mut raw_alias_vars: HashMap<Word, Word> = HashMap::default();
    let mut paths: HashMap<Word, BufferAccessPath> = HashMap::default();
    let mut value_types = combined_value_types(ctx, entry_idx);

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let n_insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .len();
        for ii in 0..n_insts {
            let snapshot = ctx.module.functions[entry_idx].blocks[bi].instructions[ii].clone();
            let Some(result) = snapshot.result_id else {
                continue;
            };
            if matches!(snapshot.class.opcode, Op::CopyObject | Op::Bitcast) {
                let Some(Operand::IdRef(source)) = snapshot.operands.first() else {
                    continue;
                };
                if let Some(path) = paths.get(source).cloned() {
                    if ptr_pointee(&types, snapshot.result_type.unwrap_or(0)).is_some() {
                        paths.insert(result, path);
                    }
                }
                continue;
            }
            if !matches!(
                snapshot.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) {
                continue;
            }
            let Some(Operand::IdRef(base)) = snapshot.operands.first() else {
                continue;
            };
            if ptr_pointee(&types, snapshot.result_type.unwrap_or(0)).is_none() {
                continue;
            }
            let indices = snapshot.operands[1..]
                .iter()
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if indices.len() != snapshot.operands.len().saturating_sub(1) {
                continue;
            }
            if buffer_types.contains_key(base) {
                paths.insert(
                    result,
                    BufferAccessPath {
                        root: *base,
                        indices,
                    },
                );
            } else if let Some(base_path) = paths.get(base).cloned() {
                let mut combined = base_path.indices;
                combined.extend(indices);
                paths.insert(
                    result,
                    BufferAccessPath {
                        root: base_path.root,
                        indices: combined,
                    },
                );
            }
        }
    }

    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let mut ii = 0;
        while ii
            < ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .len()
        {
            let snapshot = ctx.module.functions[entry_idx].blocks[bi].instructions[ii].clone();

            if matches!(
                snapshot.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) {
                if let Some(Operand::IdRef(base)) = snapshot.operands.first() {
                    let result_pointee = snapshot
                        .result_type
                        .and_then(|result_type| ptr_pointee(&types, result_type));
                    if buffer_types.contains_key(base)
                        && result_pointee.is_some_and(|ty| is_uint_type(&types, ty))
                    {
                        if buffer_types
                            .get(base)
                            .is_some_and(|ty| is_raw_uint_word_block(&types, *ty))
                        {
                            if let Some(word) =
                                direct_raw_root_word_index(&types, &snapshot.operands[1..])
                            {
                                let Some(binding) = descriptor_binding(&ctx.module, *base) else {
                                    ii += 1;
                                    continue;
                                };
                                let raw_var = match raw_alias_vars.get(base).copied() {
                                    Some(var) => var,
                                    None => {
                                        let var =
                                            create_raw_alias_buffer(ctx, binding, defs, &mut types);
                                        raw_alias_vars.insert(*base, var);
                                        var
                                    }
                                };
                                let ptr_uint = ctx.ty_ptr(
                                    StorageClass::StorageBuffer,
                                    result_pointee
                                        .ok_or("raw word alias: missing result pointee")?,
                                );
                                let zero = ctx.const_uint(0);
                                let inst = &mut ctx.module.functions[entry_idx].blocks[bi]
                                    .instructions[ii];
                                inst.result_type = Some(ptr_uint);
                                inst.operands = vec![
                                    Operand::IdRef(raw_var),
                                    Operand::IdRef(zero),
                                    Operand::IdRef(word),
                                ];
                                ii += 1;
                                continue;
                            }
                        }
                        let index_ids = snapshot.operands[1..]
                            .iter()
                            .map(|operand| match operand {
                                Operand::IdRef(id) => Some(*id),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>();
                        if let (Some(indices), Some(block_ty)) =
                            (index_ids, buffer_types.get(base).copied())
                        {
                            if let Some((byte_offset, leaf_ty)) =
                                access_path_byte_offset_and_leaf_type(
                                    ctx, &types, block_ty, &indices,
                                )
                            {
                                if byte_offset % 4 == 0
                                    && result_pointee.is_some_and(|pointee| pointee != leaf_ty)
                                {
                                    let Some(binding) = descriptor_binding(&ctx.module, *base)
                                    else {
                                        ii += 1;
                                        continue;
                                    };
                                    let raw_var = match raw_alias_vars.get(base).copied() {
                                        Some(var) => var,
                                        None => {
                                            let var = create_raw_alias_buffer(
                                                ctx, binding, defs, &mut types,
                                            );
                                            raw_alias_vars.insert(*base, var);
                                            var
                                        }
                                    };
                                    let ptr_uint = ctx.ty_ptr(
                                        StorageClass::StorageBuffer,
                                        result_pointee
                                            .ok_or("raw word alias: missing result pointee")?,
                                    );
                                    let zero = ctx.const_uint(0);
                                    let word = ctx.const_uint(byte_offset / 4);
                                    let inst = &mut ctx.module.functions[entry_idx].blocks[bi]
                                        .instructions[ii];
                                    inst.result_type = Some(ptr_uint);
                                    inst.operands = vec![
                                        Operand::IdRef(raw_var),
                                        Operand::IdRef(zero),
                                        Operand::IdRef(word),
                                    ];
                                    ii += 1;
                                    continue;
                                }
                            }
                        }
                    }
                    if let Some(base_path) = paths.get(base).cloned() {
                        if result_pointee.is_some_and(|ty| is_uint_type(&types, ty)) {
                            if let Some(local_word) =
                                raw_word_chain_offset(&types, &snapshot.operands[1..])
                            {
                                if let Some(block_ty) = buffer_types.get(&base_path.root).copied() {
                                    if let Some(base_bytes) = access_path_byte_offset(
                                        ctx,
                                        &types,
                                        block_ty,
                                        &base_path.indices,
                                    ) {
                                        let combined_indices = snapshot.operands[1..]
                                            .iter()
                                            .map(|operand| match operand {
                                                Operand::IdRef(id) => Some(*id),
                                                _ => None,
                                            })
                                            .collect::<Option<Vec<_>>>()
                                            .map(|mut indices| {
                                                let mut combined = base_path.indices.clone();
                                                combined.append(&mut indices);
                                                combined
                                            });
                                        if let (Some(combined_indices), Some(pointee)) =
                                            (combined_indices, result_pointee)
                                        {
                                            if access_path_byte_offset_and_leaf_type(
                                                ctx,
                                                &types,
                                                block_ty,
                                                &combined_indices,
                                            )
                                            .is_some_and(|(_, leaf_ty)| leaf_ty == pointee)
                                            {
                                                ii += 1;
                                                continue;
                                            }
                                        }
                                        if base_bytes % 4 == 0 {
                                            let Some(binding) =
                                                descriptor_binding(&ctx.module, base_path.root)
                                            else {
                                                ii += 1;
                                                continue;
                                            };
                                            let raw_var = match raw_alias_vars
                                                .get(&base_path.root)
                                                .copied()
                                            {
                                                Some(var) => var,
                                                None => {
                                                    let var = create_raw_alias_buffer(
                                                        ctx, binding, defs, &mut types,
                                                    );
                                                    raw_alias_vars.insert(base_path.root, var);
                                                    var
                                                }
                                            };
                                            let word = ctx.const_uint(base_bytes / 4 + local_word);
                                            let ptr_uint = ctx.ty_ptr(
                                                StorageClass::StorageBuffer,
                                                result_pointee.ok_or(
                                                    "raw word alias: missing result pointee",
                                                )?,
                                            );
                                            let zero = ctx.const_uint(0);
                                            let inst = &mut ctx.module.functions[entry_idx].blocks
                                                [bi]
                                                .instructions[ii];
                                            inst.result_type = Some(ptr_uint);
                                            inst.operands = vec![
                                                Operand::IdRef(raw_var),
                                                Operand::IdRef(zero),
                                                Operand::IdRef(word),
                                            ];
                                            ii += 1;
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if snapshot.class.opcode == Op::Load {
                if let Some(rewrite) = plan_structured_raw_vector_load_rewrite(
                    ctx,
                    &mut value_types,
                    &mut raw_alias_vars,
                    StructuredRawInputs {
                        types: &types,
                        defs,
                        buffer_types: &buffer_types,
                        paths: &paths,
                        exact_raw_facts: &exact_raw_facts,
                        fact_parents: &fact_parents,
                    },
                    &snapshot,
                ) {
                    let inserted = rewrite.prefix.len();
                    let insts = &mut ctx.module.functions[entry_idx].blocks[bi].instructions;
                    for (offset, inst) in rewrite.prefix.into_iter().enumerate() {
                        insts.insert(ii + offset, inst);
                    }
                    insts[ii + inserted] = rewrite.replacement;
                    ii += inserted + 1;
                    continue;
                }
                if let Some(rewrite) = plan_structured_raw_subword_load_rewrite(
                    ctx,
                    &mut value_types,
                    &mut raw_alias_vars,
                    StructuredRawInputs {
                        types: &types,
                        defs,
                        buffer_types: &buffer_types,
                        paths: &paths,
                        exact_raw_facts: &exact_raw_facts,
                        fact_parents: &fact_parents,
                    },
                    &snapshot,
                ) {
                    let inserted = rewrite.prefix.len();
                    let insts = &mut ctx.module.functions[entry_idx].blocks[bi].instructions;
                    for (offset, inst) in rewrite.prefix.into_iter().enumerate() {
                        insts.insert(ii + offset, inst);
                    }
                    insts[ii + inserted] = rewrite.replacement;
                    ii += inserted + 1;
                    continue;
                }
                if let Some(rewrite) = plan_mismatched_raw_word_load_rewrite(
                    ctx,
                    &types,
                    &mut value_types,
                    &mut raw_alias_vars,
                    defs,
                    &buffer_types,
                    &paths,
                    &exact_raw_facts,
                    &fact_parents,
                    &snapshot,
                ) {
                    let inserted = rewrite.prefix.len();
                    let insts = &mut ctx.module.functions[entry_idx].blocks[bi].instructions;
                    for (offset, inst) in rewrite.prefix.into_iter().enumerate() {
                        insts.insert(ii + offset, inst);
                    }
                    insts[ii + inserted] = rewrite.replacement;
                    ii += inserted + 1;
                    continue;
                }
            }
            if snapshot.class.opcode == Op::Store {
                if let Some(rewrite) = plan_raw_byte_store_rewrite(
                    ctx,
                    &types,
                    &mut value_types,
                    &mut raw_alias_vars,
                    defs,
                    &buffer_types,
                    &paths,
                    &exact_raw_facts,
                    &fact_parents,
                    &snapshot,
                ) {
                    let inserted = rewrite.prefix.len();
                    let insts = &mut ctx.module.functions[entry_idx].blocks[bi].instructions;
                    for (offset, inst) in rewrite.prefix.into_iter().enumerate() {
                        insts.insert(ii + offset, inst);
                    }
                    insts[ii + inserted] = rewrite.replacement;
                    ii += inserted + 1;
                    continue;
                }
            }
            if let Some(rewrite) = plan_raw_byte_atomic_rewrite(
                ctx,
                &types,
                &mut value_types,
                &mut raw_alias_vars,
                defs,
                &buffer_types,
                &paths,
                &snapshot,
            ) {
                let inserted = rewrite.prefix.len();
                let insts = &mut ctx.module.functions[entry_idx].blocks[bi].instructions;
                for (offset, inst) in rewrite.prefix.into_iter().enumerate() {
                    insts.insert(ii + offset, inst);
                }
                insts[ii + inserted].operands[0] = Operand::IdRef(rewrite.ptr);
                ii += inserted + 1;
                continue;
            }
            ii += 1;
        }
    }

    let used = function_used_ids(&ctx.module.functions[entry_idx]);
    for block in &mut ctx.module.functions[entry_idx].blocks {
        block.instructions.retain(|inst| {
            if inst.class.opcode != Op::Bitcast {
                return true;
            }
            let Some(result) = inst.result_id else {
                return true;
            };
            if used.contains(&result) || !paths.contains_key(&result) {
                return true;
            }
            inst.result_type
                .and_then(|ty| ptr_pointee(&types, ty))
                .is_none()
        });
    }
    Ok(())
}

pub(in crate::passes) struct RawPointerRewrite {
    pub(in crate::passes) prefix: Vec<Instruction>,
    pub(in crate::passes) ptr: Word,
}

pub(in crate::passes) struct RawStoreRewrite {
    pub(in crate::passes) prefix: Vec<Instruction>,
    pub(in crate::passes) replacement: Instruction,
}

pub(in crate::passes) struct RawLoadRewrite {
    pub(in crate::passes) prefix: Vec<Instruction>,
    pub(in crate::passes) replacement: Instruction,
}

#[derive(Clone, Copy)]
struct StructuredRawInputs<'a> {
    types: &'a HashMap<Word, Instruction>,
    defs: &'a HashMap<Word, Instruction>,
    buffer_types: &'a HashMap<Word, Word>,
    paths: &'a HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &'a HashMap<Word, (Word, u32)>,
    fact_parents: &'a HashMap<Word, Word>,
}

fn plan_structured_raw_vector_load_rewrite(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    inputs: StructuredRawInputs<'_>,
    load: &Instruction,
) -> Option<RawLoadRewrite> {
    if load.operands.len() != 1 {
        return None;
    }
    let result = load.result_id?;
    let result_ty = load.result_type?;
    let vector = inputs.types.get(&result_ty)?;
    let (Some(Operand::IdRef(component_ty)), Some(Operand::LiteralBit32(lanes))) =
        (vector.operands.first(), vector.operands.get(1))
    else {
        return None;
    };
    if vector.class.opcode != Op::TypeVector
        || !(is_integer_type_with_width(inputs.types, *component_ty, 32)
            || inputs.types.get(component_ty).is_some_and(|ty| {
                ty.class.opcode == Op::TypeFloat && ty.operands == [Operand::LiteralBit32(32)]
            }))
    {
        return None;
    }
    let Operand::IdRef(ptr) = load.operands[0] else {
        return None;
    };
    exact_raw_word_fact(inputs.exact_raw_facts, inputs.fact_parents, ptr)?;
    let access = structured_raw_word_access(
        ctx,
        inputs.types,
        raw_alias_vars,
        inputs.defs,
        inputs.buffer_types,
        inputs.paths,
        inputs.exact_raw_facts,
        inputs.fact_parents,
        ptr,
    )?;
    let uint = ctx.ty_uint();
    let mut prefix = Vec::new();
    let mut components = Vec::with_capacity(*lanes as usize);
    for lane in 0..*lanes {
        let byte_offset = access.byte_offset.checked_add(lane.checked_mul(4)?)?;
        let raw = raw_u32_at_const_byte_offset(
            ctx,
            value_types,
            access.raw_var,
            StorageClass::StorageBuffer,
            byte_offset,
            &mut prefix,
        );
        let component = if *component_ty == uint {
            raw
        } else {
            let converted = ctx.module.fresh_id();
            prefix.push(Instruction::new(
                Op::Bitcast,
                Some(*component_ty),
                Some(converted),
                vec![Operand::IdRef(raw)],
            ));
            value_types.insert(converted, *component_ty);
            converted
        };
        components.push(Operand::IdRef(component));
    }
    Some(RawLoadRewrite {
        prefix,
        replacement: Instruction::new(
            Op::CompositeConstruct,
            Some(result_ty),
            Some(result),
            components,
        ),
    })
}

fn raw_u32_at_const_byte_offset(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    storage: StorageClass,
    byte_offset: u32,
    prefix: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let low =
        raw_word_load_at_const_index(ctx, value_types, raw_var, storage, byte_offset / 4, prefix);
    let byte_lane = byte_offset % 4;
    if byte_lane == 0 {
        return low;
    }
    let shifted_low = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(uint),
        Some(shifted_low),
        vec![
            Operand::IdRef(low),
            Operand::IdRef(ctx.const_uint(byte_lane * 8)),
        ],
    ));
    value_types.insert(shifted_low, uint);
    let high = raw_word_load_at_const_index(
        ctx,
        value_types,
        raw_var,
        storage,
        byte_offset / 4 + 1,
        prefix,
    );
    let shifted_high = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(uint),
        Some(shifted_high),
        vec![
            Operand::IdRef(high),
            Operand::IdRef(ctx.const_uint(32 - byte_lane * 8)),
        ],
    ));
    value_types.insert(shifted_high, uint);
    let assembled = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::BitwiseOr,
        Some(uint),
        Some(assembled),
        vec![Operand::IdRef(shifted_low), Operand::IdRef(shifted_high)],
    ));
    value_types.insert(assembled, uint);
    assembled
}

fn plan_structured_raw_subword_load_rewrite(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    inputs: StructuredRawInputs<'_>,
    load: &Instruction,
) -> Option<RawLoadRewrite> {
    if load.operands.len() != 1 {
        return None;
    }
    let result = load.result_id?;
    let result_ty = load.result_type?;
    let bits = if is_integer_type_with_width(inputs.types, result_ty, 8) {
        8
    } else if is_integer_type_with_width(inputs.types, result_ty, 16) {
        16
    } else {
        return None;
    };
    let Operand::IdRef(ptr) = load.operands[0] else {
        return None;
    };
    exact_raw_word_fact(inputs.exact_raw_facts, inputs.fact_parents, ptr)?;
    let access = structured_raw_word_access(
        ctx,
        inputs.types,
        raw_alias_vars,
        inputs.defs,
        inputs.buffer_types,
        inputs.paths,
        inputs.exact_raw_facts,
        inputs.fact_parents,
        ptr,
    )?;
    let uint = ctx.ty_uint();
    let word_index = access.byte_offset / 4;
    let byte_lane = access.byte_offset % 4;
    let mut prefix = Vec::new();
    let low = raw_word_load_at_const_index(
        ctx,
        value_types,
        access.raw_var,
        StorageClass::StorageBuffer,
        word_index,
        &mut prefix,
    );
    let shifted_low = if byte_lane == 0 {
        low
    } else {
        let shifted = ctx.module.fresh_id();
        prefix.push(Instruction::new(
            Op::ShiftRightLogical,
            Some(uint),
            Some(shifted),
            vec![
                Operand::IdRef(low),
                Operand::IdRef(ctx.const_uint(byte_lane * 8)),
            ],
        ));
        value_types.insert(shifted, uint);
        shifted
    };
    let assembled = if byte_lane * 8 + bits <= 32 {
        shifted_low
    } else {
        let high = raw_word_load_at_const_index(
            ctx,
            value_types,
            access.raw_var,
            StorageClass::StorageBuffer,
            word_index + 1,
            &mut prefix,
        );
        let shifted_high = ctx.module.fresh_id();
        prefix.push(Instruction::new(
            Op::ShiftLeftLogical,
            Some(uint),
            Some(shifted_high),
            vec![
                Operand::IdRef(high),
                Operand::IdRef(ctx.const_uint(32 - byte_lane * 8)),
            ],
        ));
        value_types.insert(shifted_high, uint);
        let assembled = ctx.module.fresh_id();
        prefix.push(Instruction::new(
            Op::BitwiseOr,
            Some(uint),
            Some(assembled),
            vec![Operand::IdRef(shifted_low), Operand::IdRef(shifted_high)],
        ));
        value_types.insert(assembled, uint);
        assembled
    };
    let masked = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(masked),
        vec![
            Operand::IdRef(assembled),
            Operand::IdRef(ctx.const_uint((1u32 << bits) - 1)),
        ],
    ));
    value_types.insert(masked, uint);
    Some(RawLoadRewrite {
        prefix,
        replacement: Instruction::new(
            Op::UConvert,
            Some(result_ty),
            Some(result),
            vec![Operand::IdRef(masked)],
        ),
    })
}

pub(in crate::passes) fn plan_mismatched_raw_word_load_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    defs: &HashMap<Word, Instruction>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    load: &Instruction,
) -> Option<RawLoadRewrite> {
    let result = load.result_id?;
    let result_ty = load.result_type?;
    let result_kind = raw_store_object_kind(types, result_ty)?;
    let [Operand::IdRef(ptr), ..] = load.operands.as_slice() else {
        return None;
    };
    let ptr_ty = value_types.get(ptr).copied()?;
    let ptr_pointee = ptr_pointee(types, ptr_ty)?;
    if raw_store_object_kind(types, ptr_pointee) == Some(result_kind) {
        if let Some(rewrite) = plan_aligned_structured_leaf_load_rewrite(
            ctx,
            types,
            value_types,
            buffer_types,
            paths,
            *ptr,
            load,
            result,
            result_ty,
            result_kind,
        ) {
            return Some(rewrite);
        }
        return plan_structured_raw_word_load_rewrite(
            ctx,
            types,
            value_types,
            raw_alias_vars,
            defs,
            buffer_types,
            paths,
            exact_raw_facts,
            fact_parents,
            *ptr,
            result,
            result_ty,
            result_kind,
        );
    }
    if let Some(rewrite) = plan_aligned_structured_leaf_load_rewrite(
        ctx,
        types,
        value_types,
        buffer_types,
        paths,
        *ptr,
        load,
        result,
        result_ty,
        result_kind,
    ) {
        return Some(rewrite);
    }
    if let Some(rewrite) = plan_structured_raw_word_load_rewrite(
        ctx,
        types,
        value_types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        exact_raw_facts,
        fact_parents,
        *ptr,
        result,
        result_ty,
        result_kind,
    ) {
        return Some(rewrite);
    }
    let pointer = plan_raw_word_pointer_rewrite(
        ctx,
        types,
        value_types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        *ptr,
    )
    .or_else(|| {
        plan_structured_raw_word_pointer_rewrite(
            ctx,
            types,
            value_types,
            raw_alias_vars,
            defs,
            buffer_types,
            paths,
            *ptr,
        )
    })?;

    match result_kind {
        RawStoreObject::Uint32 => {
            let mut replacement = load.clone();
            replacement.operands = vec![Operand::IdRef(pointer.ptr)];
            Some(RawLoadRewrite {
                prefix: pointer.prefix,
                replacement,
            })
        }
        RawStoreObject::Float32 => {
            let uint = ctx.ty_uint();
            let raw = ctx.module.fresh_id();
            let mut prefix = pointer.prefix;
            prefix.push(Instruction::new(
                Op::Load,
                Some(uint),
                Some(raw),
                vec![Operand::IdRef(pointer.ptr)],
            ));
            value_types.insert(raw, uint);
            Some(RawLoadRewrite {
                prefix,
                replacement: Instruction::new(
                    Op::Bitcast,
                    Some(result_ty),
                    Some(result),
                    vec![Operand::IdRef(raw)],
                ),
            })
        }
    }
}

pub(in crate::passes) fn plan_raw_byte_store_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    defs: &HashMap<Word, Instruction>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    store: &Instruction,
) -> Option<RawStoreRewrite> {
    let [Operand::IdRef(ptr), Operand::IdRef(object), ..] = store.operands.as_slice() else {
        return None;
    };
    let object_ty = value_types.get(object).copied()?;
    let object_kind = raw_store_object_kind(types, object_ty)?;
    let ptr_ty = value_types.get(ptr).copied()?;
    let ptr_pointee = ptr_pointee(types, ptr_ty)?;
    if raw_store_object_kind(types, ptr_pointee) == Some(object_kind) {
        return None;
    }

    let mut prelude = Vec::new();
    let object = match object_kind {
        RawStoreObject::Uint32 => *object,
        RawStoreObject::Float32 => {
            let converted = ctx.module.fresh_id();
            prelude.push(Instruction::new(
                Op::Bitcast,
                Some(ctx.ty_uint()),
                Some(converted),
                vec![Operand::IdRef(*object)],
            ));
            value_types.insert(converted, ctx.ty_uint());
            converted
        }
    };

    if let Some(mut rewrite) = plan_structured_raw_word_store_rewrite(
        ctx,
        types,
        value_types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        exact_raw_facts,
        fact_parents,
        *ptr,
        object,
    ) {
        prelude.append(&mut rewrite.prefix);
        return Some(RawStoreRewrite {
            prefix: prelude,
            replacement: rewrite.replacement,
        });
    }

    let pointer = plan_raw_word_pointer_rewrite(
        ctx,
        types,
        value_types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        *ptr,
    )
    .or_else(|| {
        plan_structured_raw_word_pointer_rewrite(
            ctx,
            types,
            value_types,
            raw_alias_vars,
            defs,
            buffer_types,
            paths,
            *ptr,
        )
    })?;
    prelude.extend(pointer.prefix);

    let mut replacement = store.clone();
    replacement.operands = vec![Operand::IdRef(pointer.ptr), Operand::IdRef(object)];

    Some(RawStoreRewrite {
        prefix: prelude,
        replacement,
    })
}

fn plan_aligned_structured_leaf_load_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    ptr: Word,
    load: &Instruction,
    result: Word,
    result_ty: Word,
    result_kind: RawStoreObject,
) -> Option<RawLoadRewrite> {
    let path = paths.get(&ptr)?;
    let block_ty = buffer_types.get(&path.root).copied()?;
    let (byte_offset, leaf_ty) =
        access_path_byte_offset_and_leaf_type(ctx, types, block_ty, &path.indices)?;
    if byte_offset % 4 != 0 {
        return None;
    }
    if raw_store_object_kind(types, leaf_ty) == Some(result_kind) {
        return None;
    }

    if let Some(rewrite) = plan_structured_leaf_load(
        ctx,
        types,
        value_types,
        path,
        leaf_ty,
        load,
        result,
        result_ty,
        result_ty,
        false,
    ) {
        return Some(rewrite);
    }

    if result_kind == RawStoreObject::Float32 {
        let uint = ctx.ty_uint();
        if uint != result_ty {
            return plan_structured_leaf_load(
                ctx,
                types,
                value_types,
                path,
                leaf_ty,
                load,
                result,
                result_ty,
                uint,
                true,
            );
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn plan_structured_leaf_load(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    path: &BufferAccessPath,
    leaf_ty: Word,
    load: &Instruction,
    result: Word,
    result_ty: Word,
    load_ty: Word,
    bitcast_result: bool,
) -> Option<RawLoadRewrite> {
    let suffix = path_to_leaf(types, leaf_ty, load_ty)?;
    let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, load_ty);
    let leaf_ptr = ctx.module.fresh_id();
    let mut operands = Vec::with_capacity(1 + path.indices.len() + suffix.len());
    operands.push(Operand::IdRef(path.root));
    operands.extend(path.indices.iter().copied().map(Operand::IdRef));
    operands.extend(
        suffix
            .into_iter()
            .map(|index| Operand::IdRef(ctx.const_uint(index))),
    );

    let mut prefix = vec![Instruction::new(
        Op::AccessChain,
        Some(ptr_ty),
        Some(leaf_ptr),
        operands,
    )];
    value_types.insert(leaf_ptr, ptr_ty);

    let mut load_operands = vec![Operand::IdRef(leaf_ptr)];
    load_operands.extend(load.operands.iter().skip(1).cloned());
    if !bitcast_result {
        return Some(RawLoadRewrite {
            prefix,
            replacement: Instruction::new(Op::Load, Some(result_ty), Some(result), load_operands),
        });
    }

    let loaded = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::Load,
        Some(load_ty),
        Some(loaded),
        load_operands,
    ));
    value_types.insert(loaded, load_ty);
    Some(RawLoadRewrite {
        prefix,
        replacement: Instruction::new(
            Op::Bitcast,
            Some(result_ty),
            Some(result),
            vec![Operand::IdRef(loaded)],
        ),
    })
}

#[derive(Clone, Copy, Debug)]
pub(in crate::passes) struct StructuredRawWordAccess {
    pub(in crate::passes) raw_var: Word,
    pub(in crate::passes) byte_offset: u32,
}

pub(in crate::passes) fn plan_structured_raw_word_load_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    defs: &HashMap<Word, Instruction>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    ptr: Word,
    result: Word,
    result_ty: Word,
    result_kind: RawStoreObject,
) -> Option<RawLoadRewrite> {
    let has_exact_raw_fact = exact_raw_word_fact(exact_raw_facts, fact_parents, ptr).is_some();
    let access = structured_raw_word_access(
        ctx,
        types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        exact_raw_facts,
        fact_parents,
        ptr,
    )?;

    let uint = ctx.ty_uint();
    let mut prefix = Vec::new();
    let low_word = raw_word_load_at_const_index(
        ctx,
        value_types,
        access.raw_var,
        StorageClass::StorageBuffer,
        access.byte_offset / 4,
        &mut prefix,
    );
    if access.byte_offset % 4 == 0 {
        if !has_exact_raw_fact {
            return None;
        }
        return match result_kind {
            RawStoreObject::Uint32 => Some(RawLoadRewrite {
                prefix,
                replacement: Instruction::new(
                    Op::CopyObject,
                    Some(result_ty),
                    Some(result),
                    vec![Operand::IdRef(low_word)],
                ),
            }),
            RawStoreObject::Float32 => Some(RawLoadRewrite {
                prefix,
                replacement: Instruction::new(
                    Op::Bitcast,
                    Some(result_ty),
                    Some(result),
                    vec![Operand::IdRef(low_word)],
                ),
            }),
        };
    }
    let high_word = raw_word_load_at_const_index(
        ctx,
        value_types,
        access.raw_var,
        StorageClass::StorageBuffer,
        access.byte_offset / 4 + 1,
        &mut prefix,
    );

    let low_shift = ctx.const_uint((access.byte_offset % 4) * 8);
    let shifted_low = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::ShiftRightLogical,
        Some(uint),
        Some(shifted_low),
        vec![Operand::IdRef(low_word), Operand::IdRef(low_shift)],
    ));
    value_types.insert(shifted_low, uint);

    let high_shift = ctx.const_uint(32 - (access.byte_offset % 4) * 8);
    let shifted_high = ctx.module.fresh_id();
    prefix.push(Instruction::new(
        Op::ShiftLeftLogical,
        Some(uint),
        Some(shifted_high),
        vec![Operand::IdRef(high_word), Operand::IdRef(high_shift)],
    ));
    value_types.insert(shifted_high, uint);

    match result_kind {
        RawStoreObject::Uint32 => Some(RawLoadRewrite {
            prefix,
            replacement: Instruction::new(
                Op::BitwiseOr,
                Some(uint),
                Some(result),
                vec![Operand::IdRef(shifted_low), Operand::IdRef(shifted_high)],
            ),
        }),
        RawStoreObject::Float32 => {
            let assembled = ctx.module.fresh_id();
            prefix.push(Instruction::new(
                Op::BitwiseOr,
                Some(uint),
                Some(assembled),
                vec![Operand::IdRef(shifted_low), Operand::IdRef(shifted_high)],
            ));
            value_types.insert(assembled, uint);
            Some(RawLoadRewrite {
                prefix,
                replacement: Instruction::new(
                    Op::Bitcast,
                    Some(result_ty),
                    Some(result),
                    vec![Operand::IdRef(assembled)],
                ),
            })
        }
    }
}

pub(in crate::passes) fn plan_structured_raw_word_store_rewrite(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    defs: &HashMap<Word, Instruction>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    ptr: Word,
    object: Word,
) -> Option<RawStoreRewrite> {
    let path = paths.get(&ptr)?;
    let block_ty = buffer_types.get(&path.root).copied()?;
    let byte_offset = access_path_byte_offset(ctx, types, block_ty, &path.indices)?;
    if byte_offset % 4 == 0 {
        return None;
    }
    let access = structured_raw_word_access(
        ctx,
        types,
        raw_alias_vars,
        defs,
        buffer_types,
        paths,
        exact_raw_facts,
        fact_parents,
        ptr,
    )?;

    let uint = ctx.ty_uint();
    let mut instructions = Vec::new();
    for byte in 0..4 {
        let source = if byte == 0 {
            object
        } else {
            let shifted = ctx.module.fresh_id();
            let shift = ctx.const_uint(byte * 8);
            instructions.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(uint),
                Some(shifted),
                vec![Operand::IdRef(object), Operand::IdRef(shift)],
            ));
            value_types.insert(shifted, uint);
            shifted
        };
        raw_byte_store_from_u32_at_const_offset(
            ctx,
            value_types,
            access.raw_var,
            access.byte_offset + byte,
            source,
            &mut instructions,
        );
    }

    let replacement = instructions.pop()?;
    Some(RawStoreRewrite {
        prefix: instructions,
        replacement,
    })
}

pub(in crate::passes) fn structured_raw_word_access(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    raw_alias_vars: &mut HashMap<Word, Word>,
    defs: &HashMap<Word, Instruction>,
    buffer_types: &HashMap<Word, Word>,
    paths: &HashMap<Word, BufferAccessPath>,
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    ptr: Word,
) -> Option<StructuredRawWordAccess> {
    if let Some((root, byte_offset)) = exact_raw_word_fact(exact_raw_facts, fact_parents, ptr) {
        let binding = descriptor_binding(&ctx.module, root)?;
        let raw_var = match raw_alias_vars.get(&root).copied() {
            Some(var) => var,
            None => {
                let mut type_defs = combined_type_defs(ctx, defs);
                let var = create_raw_alias_buffer(ctx, binding, defs, &mut type_defs);
                raw_alias_vars.insert(root, var);
                var
            }
        };
        return Some(StructuredRawWordAccess {
            raw_var,
            byte_offset,
        });
    }
    let path = paths.get(&ptr)?;
    let block_ty = buffer_types.get(&path.root).copied()?;
    let byte_offset = access_path_byte_offset(ctx, types, block_ty, &path.indices)?;
    let binding = descriptor_binding(&ctx.module, path.root)?;
    let raw_var = match raw_alias_vars.get(&path.root).copied() {
        Some(var) => var,
        None => {
            let mut type_defs = combined_type_defs(ctx, defs);
            let var = create_raw_alias_buffer(ctx, binding, defs, &mut type_defs);
            raw_alias_vars.insert(path.root, var);
            var
        }
    };
    Some(StructuredRawWordAccess {
        raw_var,
        byte_offset,
    })
}

fn exact_raw_word_fact(
    exact_raw_facts: &HashMap<Word, (Word, u32)>,
    fact_parents: &HashMap<Word, Word>,
    ptr: Word,
) -> Option<(Word, u32)> {
    let mut fact_ptr = ptr;
    let mut seen = HashSet::default();
    loop {
        if !seen.insert(fact_ptr) {
            return None;
        }
        if let Some(fact) = exact_raw_facts.get(&fact_ptr).copied() {
            return Some(fact);
        }
        fact_ptr = fact_parents.get(&fact_ptr).copied()?;
    }
}

fn is_raw_uint_word_block(types: &HashMap<Word, Instruction>, block_ty: Word) -> bool {
    let Some(block) = types.get(&block_ty) else {
        return false;
    };
    let [Operand::IdRef(array_ty)] = block.operands.as_slice() else {
        return false;
    };
    if block.class.opcode != Op::TypeStruct {
        return false;
    }
    let Some(array) = types.get(array_ty) else {
        return false;
    };
    if !matches!(array.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
        return false;
    }
    matches!(array.operands.first(), Some(Operand::IdRef(elem)) if is_uint_type(types, *elem))
}

fn pointer_storage(
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    pointer: Word,
) -> Option<StorageClass> {
    let pointer_ty = value_types.get(&pointer)?;
    let definition = types.get(pointer_ty)?;
    if definition.class.opcode != Op::TypePointer {
        return None;
    }
    match definition.operands.first()? {
        Operand::StorageClass(storage) => Some(*storage),
        _ => None,
    }
}

pub(in crate::passes) fn raw_word_pointer_at_const_index(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    storage: StorageClass,
    word_index: u32,
    instructions: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let ptr_uint_ty = ctx.ty_ptr(storage, uint);
    let raw_ptr = ctx.module.fresh_id();
    let zero = ctx.const_uint(0);
    let word = ctx.const_uint(word_index);
    instructions.push(Instruction::new(
        Op::AccessChain,
        Some(ptr_uint_ty),
        Some(raw_ptr),
        vec![
            Operand::IdRef(raw_var),
            Operand::IdRef(zero),
            Operand::IdRef(word),
        ],
    ));
    value_types.insert(raw_ptr, ptr_uint_ty);
    raw_ptr
}

pub(in crate::passes) fn raw_word_load_at_const_index(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    storage: StorageClass,
    word_index: u32,
    instructions: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let ptr = raw_word_pointer_at_const_index(
        ctx,
        value_types,
        raw_var,
        storage,
        word_index,
        instructions,
    );
    let word = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Load,
        Some(uint),
        Some(word),
        vec![Operand::IdRef(ptr)],
    ));
    value_types.insert(word, uint);
    word
}

pub(in crate::passes) fn rewrite_exact_raw_word_loads(ctx: &mut Ctx, entry_idx: usize) {
    let byte_ty = ctx.ty_int8();
    let uint = ctx.ty_uint();
    let types = combined_type_defs(ctx, &HashMap::default());
    let mut value_types = combined_value_types(ctx, entry_idx);
    let definitions = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
        .collect::<HashMap<_, _>>();
    let facts = ctx
        .emit_sidecar
        .buffer_access_offsets
        .iter()
        .map(|fact| {
            let root =
                zero_offset_raw_word_root(ctx, &types, &value_types, &definitions, fact.root)
                    .unwrap_or(fact.root);
            (fact.id, (root, fact.byte_offset))
        })
        .collect::<HashMap<_, _>>();
    let mut replaced_pointers = HashSet::default();

    for bi in 0..ctx.module.functions[entry_idx].blocks.len() {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for inst in old {
            let Some((result, result_ty, pointer)) = (|| {
                if inst.class.opcode != Op::Load || inst.operands.len() != 1 {
                    return None;
                }
                let result = inst.result_id?;
                let result_ty = inst.result_type?;
                let Operand::IdRef(pointer) = inst.operands[0] else {
                    return None;
                };
                Some((result, result_ty, pointer))
            })() else {
                rewritten.push(inst);
                continue;
            };
            let Some(&(root, byte_offset)) = facts.get(&pointer) else {
                rewritten.push(inst);
                continue;
            };
            let Some(root_ty) = value_types.get(&root).copied() else {
                rewritten.push(inst);
                continue;
            };
            let Some(root_pointee) = ptr_pointee(&types, root_ty) else {
                rewritten.push(inst);
                continue;
            };
            let Some(root_storage) = pointer_storage(&types, &value_types, root) else {
                rewritten.push(inst);
                continue;
            };
            if !is_raw_uint_word_block(&types, root_pointee) {
                rewritten.push(inst);
                continue;
            }
            let Ok(byte_offset) = u32::try_from(byte_offset) else {
                rewritten.push(inst);
                continue;
            };
            if result_ty != byte_ty {
                let Some(rewrite) = plan_exact_raw_word_typed_load(
                    ctx,
                    &types,
                    &mut value_types,
                    root,
                    root_storage,
                    byte_offset,
                    result,
                    result_ty,
                ) else {
                    rewritten.push(inst);
                    continue;
                };
                rewritten.extend(rewrite.prefix);
                rewritten.push(rewrite.replacement);
                replaced_pointers.extend([pointer, root]);
                continue;
            }
            let word = raw_word_load_at_const_index(
                ctx,
                &mut value_types,
                root,
                root_storage,
                byte_offset / 4,
                &mut rewritten,
            );
            let shifted = if byte_offset % 4 == 0 {
                word
            } else {
                let shifted = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::ShiftRightLogical,
                    Some(uint),
                    Some(shifted),
                    vec![
                        Operand::IdRef(word),
                        Operand::IdRef(ctx.const_uint((byte_offset % 4) * 8)),
                    ],
                ));
                value_types.insert(shifted, uint);
                shifted
            };
            let masked = ctx.module.fresh_id();
            rewritten.push(Instruction::new(
                Op::BitwiseAnd,
                Some(uint),
                Some(masked),
                vec![
                    Operand::IdRef(shifted),
                    Operand::IdRef(ctx.const_uint(0xff)),
                ],
            ));
            rewritten.push(Instruction::new(
                Op::UConvert,
                Some(byte_ty),
                Some(result),
                vec![Operand::IdRef(masked)],
            ));
            value_types.insert(masked, uint);
            value_types.insert(result, byte_ty);
            replaced_pointers.extend([pointer, root]);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
    if !replaced_pointers.is_empty() {
        crate::passes::resources::retire_dead_pointer_projections(
            ctx,
            entry_idx,
            replaced_pointers,
        );
    }
}

pub(in crate::passes) fn rewrite_affine_raw_word_loads(ctx: &mut Ctx, entry_idx: usize) {
    let uint = ctx.ty_uint();
    let types = combined_type_defs(ctx, &HashMap::default());
    let mut value_types = combined_value_types(ctx, entry_idx);
    let definitions = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
        .collect::<HashMap<_, _>>();
    let mut affine_offsets = ctx
        .emit_sidecar
        .buffer_access_affine_offsets
        .iter()
        .filter_map(|fact| {
            Some((
                fact.id,
                (
                    fact.root,
                    u32::try_from(fact.constant).ok()?,
                    fact.terms
                        .iter()
                        .map(|(index, stride)| Some((*index, u32::try_from(*stride).ok()?)))
                        .collect::<Option<Vec<_>>>()?,
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    let exact_offsets = ctx
        .emit_sidecar
        .buffer_access_offsets
        .iter()
        .map(|fact| (fact.id, (fact.root, fact.byte_offset)))
        .collect::<HashMap<_, _>>();
    flatten_affine_offset_roots(&mut affine_offsets, &exact_offsets);
    if affine_offsets.is_empty() {
        return;
    }
    let mut replaced_pointers = HashSet::default();
    let mut replaced_fact_roots = HashSet::default();

    for bi in 0..ctx.module.functions[entry_idx].blocks.len() {
        let old = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for instruction in old {
            let Some((result, result_ty, pointer)) = (|| {
                if instruction.class.opcode != Op::Load || instruction.operands.len() != 1 {
                    return None;
                }
                let Operand::IdRef(pointer) = instruction.operands[0] else {
                    return None;
                };
                Some((instruction.result_id?, instruction.result_type?, pointer))
            })() else {
                rewritten.push(instruction);
                continue;
            };
            let Some((fact_root, constant, terms)) = inherited_affine_byte_offset(
                ctx,
                pointer,
                &affine_offsets,
                &definitions,
                &types,
                &value_types,
                &mut HashSet::default(),
            ) else {
                rewritten.push(instruction);
                continue;
            };
            if terms.iter().any(|(_, stride)| stride % 4 != 0) {
                rewritten.push(instruction);
                continue;
            }
            let Some(root) =
                zero_offset_raw_word_root(ctx, &types, &value_types, &definitions, fact_root)
            else {
                rewritten.push(instruction);
                continue;
            };
            let Some(root_storage) = pointer_storage(&types, &value_types, root) else {
                rewritten.push(instruction);
                continue;
            };
            let byte_result = result_ty == ctx.ty_int8();
            let word_shape = raw_32bit_load_shape(&types, result_ty);
            if !byte_result && (constant % 4 != 0 || word_shape.is_none()) {
                rewritten.push(instruction);
                continue;
            }
            let constant_words = constant / 4;
            let terms = terms
                .into_iter()
                .map(|(index, stride)| (index, stride / 4))
                .collect::<Vec<_>>();
            let Some(index_ty) = terms
                .first()
                .and_then(|(index, _)| value_types.get(index).copied())
            else {
                rewritten.push(instruction);
                continue;
            };
            if terms
                .iter()
                .any(|(index, _)| value_types.get(index).copied() != Some(index_ty))
            {
                rewritten.push(instruction);
                continue;
            }
            replaced_pointers.extend([pointer, fact_root, root]);
            replaced_fact_roots.insert(fact_root);
            let mut word_index = ctx.const_int_of(index_ty, i64::from(constant_words));
            for (index, stride) in terms {
                let term = if stride == 1 {
                    index
                } else {
                    let product = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::IMul,
                        Some(index_ty),
                        Some(product),
                        vec![
                            Operand::IdRef(index),
                            Operand::IdRef(ctx.const_int_of(index_ty, i64::from(stride))),
                        ],
                    ));
                    value_types.insert(product, index_ty);
                    product
                };
                let sum = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::IAdd,
                    Some(index_ty),
                    Some(sum),
                    vec![Operand::IdRef(word_index), Operand::IdRef(term)],
                ));
                value_types.insert(sum, index_ty);
                word_index = sum;
            }
            if byte_result {
                let raw = raw_word_load_at_index(
                    ctx,
                    &mut value_types,
                    root,
                    root_storage,
                    word_index,
                    &mut rewritten,
                );
                let shifted = if constant % 4 == 0 {
                    raw
                } else {
                    let shifted = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::ShiftRightLogical,
                        Some(uint),
                        Some(shifted),
                        vec![
                            Operand::IdRef(raw),
                            Operand::IdRef(ctx.const_uint((constant % 4) * 8)),
                        ],
                    ));
                    value_types.insert(shifted, uint);
                    shifted
                };
                let masked = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::BitwiseAnd,
                    Some(uint),
                    Some(masked),
                    vec![
                        Operand::IdRef(shifted),
                        Operand::IdRef(ctx.const_uint(0xff)),
                    ],
                ));
                rewritten.push(Instruction::new(
                    Op::UConvert,
                    Some(result_ty),
                    Some(result),
                    vec![Operand::IdRef(masked)],
                ));
                value_types.insert(masked, uint);
                value_types.insert(result, result_ty);
                continue;
            }
            let (component_ty, lanes) = word_shape.expect("32-bit load shape checked above");
            let mut components = Vec::with_capacity(lanes as usize);
            for lane in 0..lanes {
                let lane_index = if lane == 0 {
                    word_index
                } else {
                    let lane_index = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::IAdd,
                        Some(index_ty),
                        Some(lane_index),
                        vec![
                            Operand::IdRef(word_index),
                            Operand::IdRef(ctx.const_int_of(index_ty, i64::from(lane))),
                        ],
                    ));
                    value_types.insert(lane_index, index_ty);
                    lane_index
                };
                let raw = raw_word_load_at_index(
                    ctx,
                    &mut value_types,
                    root,
                    root_storage,
                    lane_index,
                    &mut rewritten,
                );
                let component = if component_ty == uint {
                    raw
                } else {
                    let component = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::Bitcast,
                        Some(component_ty),
                        Some(component),
                        vec![Operand::IdRef(raw)],
                    ));
                    value_types.insert(component, component_ty);
                    component
                };
                components.push(Operand::IdRef(component));
            }
            rewritten.push(if lanes == 1 {
                Instruction::new(Op::CopyObject, Some(result_ty), Some(result), components)
            } else {
                Instruction::new(
                    Op::CompositeConstruct,
                    Some(result_ty),
                    Some(result),
                    components,
                )
            });
            value_types.insert(result, result_ty);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = rewritten;
    }
    replaced_pointers.extend(
        affine_offsets
            .iter()
            .filter(|(_, (root, _, _))| replaced_fact_roots.contains(root))
            .map(|(id, _)| *id),
    );
    if !replaced_pointers.is_empty() {
        crate::passes::resources::retire_dead_pointer_projections(
            ctx,
            entry_idx,
            replaced_pointers,
        );
    }
}

fn raw_32bit_load_shape(
    types: &HashMap<Word, Instruction>,
    result_ty: Word,
) -> Option<(Word, u32)> {
    if is_integer_type_with_width(types, result_ty, 32) || is_float32_type(types, result_ty) {
        return Some((result_ty, 1));
    }
    let vector = types.get(&result_ty)?;
    let (Some(Operand::IdRef(component)), Some(Operand::LiteralBit32(lanes))) =
        (vector.operands.first(), vector.operands.get(1))
    else {
        return None;
    };
    (vector.class.opcode == Op::TypeVector
        && (is_integer_type_with_width(types, *component, 32)
            || is_float32_type(types, *component)))
    .then_some((*component, *lanes))
}

fn raw_word_load_at_index(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    storage: StorageClass,
    word_index: Word,
    instructions: &mut Vec<Instruction>,
) -> Word {
    let uint = ctx.ty_uint();
    let pointer_ty = ctx.ty_ptr(storage, uint);
    let pointer = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::AccessChain,
        Some(pointer_ty),
        Some(pointer),
        vec![
            Operand::IdRef(raw_var),
            Operand::IdRef(ctx.const_uint(0)),
            Operand::IdRef(word_index),
        ],
    ));
    value_types.insert(pointer, pointer_ty);
    let loaded = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Load,
        Some(uint),
        Some(loaded),
        vec![Operand::IdRef(pointer)],
    ));
    value_types.insert(loaded, uint);
    loaded
}

fn plan_exact_raw_word_typed_load(
    ctx: &mut Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    storage: StorageClass,
    byte_offset: u32,
    result: Word,
    result_ty: Word,
) -> Option<RawLoadRewrite> {
    let uint = ctx.ty_uint();
    if is_integer_type_with_width(types, result_ty, 32) || is_float32_type(types, result_ty) {
        let mut prefix = Vec::new();
        let raw = raw_u32_at_const_byte_offset(
            ctx,
            value_types,
            raw_var,
            storage,
            byte_offset,
            &mut prefix,
        );
        let replacement = if result_ty == uint {
            Instruction::new(
                Op::CopyObject,
                Some(result_ty),
                Some(result),
                vec![Operand::IdRef(raw)],
            )
        } else {
            Instruction::new(
                Op::Bitcast,
                Some(result_ty),
                Some(result),
                vec![Operand::IdRef(raw)],
            )
        };
        return Some(RawLoadRewrite {
            prefix,
            replacement,
        });
    }
    let vector = types.get(&result_ty)?;
    let (Some(Operand::IdRef(component_ty)), Some(Operand::LiteralBit32(lanes))) =
        (vector.operands.first(), vector.operands.get(1))
    else {
        return None;
    };
    if vector.class.opcode != Op::TypeVector
        || !(is_integer_type_with_width(types, *component_ty, 32)
            || types.get(component_ty).is_some_and(|ty| {
                ty.class.opcode == Op::TypeFloat && ty.operands == [Operand::LiteralBit32(32)]
            }))
    {
        return None;
    }
    let mut prefix = Vec::new();
    let mut components = Vec::with_capacity(*lanes as usize);
    for lane in 0..*lanes {
        let lane_offset = byte_offset.checked_add(lane.checked_mul(4)?)?;
        let raw = raw_u32_at_const_byte_offset(
            ctx,
            value_types,
            raw_var,
            storage,
            lane_offset,
            &mut prefix,
        );
        let component = if *component_ty == uint {
            raw
        } else {
            let converted = ctx.module.fresh_id();
            prefix.push(Instruction::new(
                Op::Bitcast,
                Some(*component_ty),
                Some(converted),
                vec![Operand::IdRef(raw)],
            ));
            value_types.insert(converted, *component_ty);
            converted
        };
        components.push(Operand::IdRef(component));
    }
    Some(RawLoadRewrite {
        prefix,
        replacement: Instruction::new(
            Op::CompositeConstruct,
            Some(result_ty),
            Some(result),
            components,
        ),
    })
}

fn zero_offset_raw_word_root(
    ctx: &Ctx,
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    definitions: &HashMap<Word, Instruction>,
    root: Word,
) -> Option<Word> {
    let mut current = root;
    let mut seen = HashSet::default();
    loop {
        if !seen.insert(current) {
            return None;
        }
        let pointer_ty = value_types.get(&current).copied()?;
        let pointee = ptr_pointee(types, pointer_ty)?;
        if is_raw_uint_word_block(types, pointee) {
            return Some(current);
        }
        let definition = definitions.get(&current)?;
        if definition.class.opcode == Op::CopyObject {
            let Operand::IdRef(base) = definition.operands.first()? else {
                return None;
            };
            current = *base;
            continue;
        }
        if !matches!(
            definition.class.opcode,
            Op::AccessChain | Op::InBoundsAccessChain
        ) {
            return None;
        }
        let Operand::IdRef(base) = definition.operands.first()? else {
            return None;
        };
        let base_ty = value_types.get(base).copied()?;
        let base_pointee = ptr_pointee(types, base_ty)?;
        let indices = definition.operands[1..]
            .iter()
            .map(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if access_path_byte_offset(ctx, types, base_pointee, &indices) != Some(0) {
            return None;
        }
        current = *base;
    }
}

pub(in crate::passes) fn raw_byte_store_from_u32_at_const_offset(
    ctx: &mut Ctx,
    value_types: &mut HashMap<Word, Word>,
    raw_var: Word,
    byte_offset: u32,
    value: Word,
    instructions: &mut Vec<Instruction>,
) {
    let uint = ctx.ty_uint();
    let ptr = raw_word_pointer_at_const_index(
        ctx,
        value_types,
        raw_var,
        StorageClass::StorageBuffer,
        byte_offset / 4,
        instructions,
    );
    let lane_shift = (byte_offset % 4) * 8;
    let byte_mask = ctx.const_uint(0xff);
    let byte = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::BitwiseAnd,
        Some(uint),
        Some(byte),
        vec![Operand::IdRef(value), Operand::IdRef(byte_mask)],
    ));
    value_types.insert(byte, uint);

    let shifted_byte = if lane_shift == 0 {
        byte
    } else {
        let shifted = ctx.module.fresh_id();
        let shift = ctx.const_uint(lane_shift);
        instructions.push(Instruction::new(
            Op::ShiftLeftLogical,
            Some(uint),
            Some(shifted),
            vec![Operand::IdRef(byte), Operand::IdRef(shift)],
        ));
        value_types.insert(shifted, uint);
        shifted
    };

    let shifted_mask = 0xffu32 << lane_shift;
    let inverted_mask = ctx.const_uint(!shifted_mask);
    let scope = ctx.const_uint(Scope::Device as u32);
    let semantics = ctx.const_uint(MemorySemantics::RELAXED.bits());
    let old = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::AtomicAnd,
        Some(uint),
        Some(old),
        vec![
            Operand::IdRef(ptr),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
            Operand::IdRef(inverted_mask),
        ],
    ));
    value_types.insert(old, uint);
    let old = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::AtomicOr,
        Some(uint),
        Some(old),
        vec![
            Operand::IdRef(ptr),
            Operand::IdScope(scope),
            Operand::IdMemorySemantics(semantics),
            Operand::IdRef(shifted_byte),
        ],
    ));
    value_types.insert(old, uint);
}
