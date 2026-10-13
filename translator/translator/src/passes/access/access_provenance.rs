use super::*;
use crate::passes::rewrites::combined_type_defs;

#[derive(Clone, Debug)]
pub(in crate::passes) struct AccessChainProvenance {
    pub(in crate::passes) root: Word,
    pub(in crate::passes) indices: Vec<Word>,
    pub(in crate::passes) ptr_ty: Word,
    pub(in crate::passes) op: Op,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::passes) struct LocalPointerFieldKey {
    pub(in crate::passes) root: Word,
    pub(in crate::passes) indices: Vec<u32>,
}

pub(in crate::passes) fn recover_inlined_local_pointer_fields(ctx: &mut Ctx, entry_idx: usize) {
    let store_markers = local_pointer_field_store_markers(ctx);
    let store_sources = local_pointer_field_store_sources(ctx);
    let store_keys = local_pointer_field_store_keys(ctx);
    let load_markers = local_pointer_field_load_markers(ctx);
    if store_markers.is_empty() || load_markers.is_empty() {
        return;
    }
    let type_defs = combined_type_defs(ctx, &HashMap::default());

    let mut access_keys: HashMap<Word, LocalPointerFieldKey> = HashMap::default();
    let mut stored_fields: HashMap<LocalPointerFieldKey, Word> = HashMap::default();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if let Some((result, key)) = local_pointer_field_access_key(ctx, &access_keys, inst) {
                access_keys.insert(result, key);
            }
            if inst.class.opcode != Op::Store {
                continue;
            }
            let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(value))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let key = access_keys.get(ptr).or_else(|| store_keys.get(value));
            let (Some(key), Some(source)) = (
                key,
                key.and_then(|key| keyed_store_source(&store_sources, &store_markers, *value, key)),
            ) else {
                continue;
            };
            stored_fields.insert(key.clone(), *source);
        }
    }
    if stored_fields.is_empty() {
        return;
    }

    let value_types = function_value_types(ctx, entry_idx);
    let is_handle = |id: Word| {
        value_types
            .get(&id)
            .and_then(|ty| type_defs.get(ty))
            .is_some_and(|definition| {
                matches!(
                    definition.class.opcode,
                    Op::TypePointer
                        | Op::TypeImage
                        | Op::TypeSampler
                        | Op::TypeSampledImage
                        | Op::TypeAccelerationStructureKHR
                )
            })
    };

    let mut replacements = Vec::new();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if !matches!(inst.class.opcode, Op::Load | Op::CopyObject | Op::Undef) {
                continue;
            }
            if !inst.result_type.is_some_and(|result_type| {
                type_defs.get(&result_type).is_some_and(|definition| {
                    matches!(
                        definition.class.opcode,
                        Op::TypePointer
                            | Op::TypeImage
                            | Op::TypeSampler
                            | Op::TypeSampledImage
                            | Op::TypeAccelerationStructureKHR
                    )
                })
            }) {
                continue;
            }
            let Some(result) = inst.result_id else {
                continue;
            };
            let Some(marker_key) = load_markers.get(&result) else {
                continue;
            };
            let key = if let Some(root) = access_keys.get(&marker_key.root) {
                let mut indices = root.indices.clone();
                indices.extend_from_slice(&marker_key.indices);
                LocalPointerFieldKey {
                    root: root.root,
                    indices,
                }
            } else {
                marker_key.clone()
            };
            let Some(source) = stored_fields.get(&key).copied() else {
                continue;
            };
            if !is_handle(source) {
                continue;
            }
            replacements.push((result, source));
        }
    }
    if !replacements.is_empty() {
        let replacement_map = replacements.iter().copied().collect();
        ctx.emit_sidecar.remap_ids(&replacement_map);
        let func = &mut ctx.module.functions[entry_idx];
        for (from, to) in replacements {
            replace_id_in_function(func, from, to);
        }
    }
}

pub(in crate::passes) fn materialize_inlined_local_pointer_field_stores(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
    let mut sources = HashMap::<(Word, LocalPointerFieldKey), Option<Word>>::default();
    for fact in &ctx.emit_sidecar.local_pointer_field_stores {
        let key = (
            fact.id,
            LocalPointerFieldKey {
                root: fact.root,
                indices: fact.indices.clone(),
            },
        );
        sources
            .entry(key)
            .and_modify(|source| {
                if *source != Some(fact.source) {
                    *source = None;
                }
            })
            .or_insert(Some(fact.source));
    }
    if sources.is_empty() {
        return;
    }

    let value_types = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .chain(ctx.module.functions.iter().flat_map(|function| {
            function
                .parameters
                .iter()
                .chain(function.blocks.iter().flat_map(|block| &block.instructions))
        }))
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    let mut access_keys = HashMap::<Word, LocalPointerFieldKey>::default();
    let mut replacements = Vec::new();
    for (block_index, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            if let Some((result, key)) =
                local_pointer_field_access_key(ctx, &access_keys, instruction)
            {
                access_keys.insert(result, key);
            }
            if instruction.class.opcode != Op::Store {
                continue;
            }
            let (Some(Operand::IdRef(pointer)), Some(Operand::IdRef(marker))) =
                (instruction.operands.first(), instruction.operands.get(1))
            else {
                continue;
            };
            let Some(key) = access_keys.get(pointer) else {
                continue;
            };
            let Some(source) = sources.get(&(*marker, key.clone())).copied().flatten() else {
                continue;
            };
            if source != *marker && value_types.get(&source) == value_types.get(marker) {
                replacements.push((block_index, instruction_index, source));
            }
        }
    }
    for (block_index, instruction_index, source) in replacements {
        ctx.module.functions[entry_idx].blocks[block_index].instructions[instruction_index]
            .operands[1] = Operand::IdRef(source);
    }
}

pub(in crate::passes) fn recover_inlined_local_dynamic_pointer_fields(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut facts = ctx.emit_sidecar.local_pointer_dynamic_field_loads.clone();
    let store_markers = local_pointer_field_store_markers(ctx);
    let store_sources = local_pointer_field_store_sources(ctx);
    let store_keys = local_pointer_field_store_keys(ctx);
    let local_table_roots = store_keys
        .values()
        .map(|key| key.root)
        .collect::<HashSet<_>>();
    if store_markers.is_empty() {
        return Ok(());
    }

    let mut access_keys = HashMap::<Word, LocalPointerFieldKey>::default();
    let mut stored_fields = HashMap::<LocalPointerFieldKey, Word>::default();
    for inst in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
    {
        if let Some((result, key)) = local_pointer_field_access_key(ctx, &access_keys, inst) {
            access_keys.insert(result, key);
        }
        if inst.class.opcode != Op::Store {
            continue;
        }
        let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(value))) =
            (inst.operands.first(), inst.operands.get(1))
        else {
            continue;
        };
        let key = access_keys.get(ptr).or_else(|| store_keys.get(value));
        let (Some(key), Some(source)) = (
            key,
            key.and_then(|key| keyed_store_source(&store_sources, &store_markers, *value, key)),
        ) else {
            continue;
        };
        stored_fields.insert(key.clone(), *source);
    }

    let defs = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|inst| inst.result_id.map(|result| (result, inst.clone())))
        .collect::<HashMap<_, _>>();
    let known = facts.iter().map(|fact| fact.id).collect::<HashSet<_>>();
    for inst in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
    {
        if inst.class.opcode != Op::Load {
            continue;
        }
        let (Some(id), Some(result_ty), Some(Operand::IdRef(ptr))) =
            (inst.result_id, inst.result_type, inst.operands.first())
        else {
            continue;
        };
        if known.contains(&id)
            || type_def_of(ctx, result_ty).is_none_or(|ty| ty.class.opcode != Op::TypePointer)
        {
            continue;
        }
        let Some(ptr_ty) = value_result_type(ctx, *ptr) else {
            continue;
        };
        if pointer_pointee(ctx, ptr_ty) == Some(result_ty) {
            continue;
        }
        let Some(access) = defs.get(ptr).filter(|access| {
            matches!(
                access.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            )
        }) else {
            continue;
        };
        let Some(Operand::IdRef(root)) = access.operands.first() else {
            continue;
        };
        let indices = &access.operands[1..];
        let dynamic = indices
            .iter()
            .enumerate()
            .filter(|(_, operand)| access_index_u32(ctx, operand).is_none())
            .collect::<Vec<_>>();
        let [(dynamic_position, Operand::IdRef(index))] = dynamic.as_slice() else {
            continue;
        };
        let Some(prefix) = indices[..*dynamic_position]
            .iter()
            .map(|operand| access_index_u32(ctx, operand))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let Some(suffix) = indices[*dynamic_position + 1..]
            .iter()
            .map(|operand| access_index_u32(ctx, operand))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        facts.push(crate::emit_sidecar::LocalPointerDynamicFieldLoad {
            id,
            root: *root,
            prefix,
            index: *index,
            suffix,
        });
    }
    if facts.is_empty() {
        return Ok(());
    }

    let mut replacements = HashMap::<Word, Vec<Instruction>>::default();
    let mut pointer_roots = Vec::<(StorageClass, Word)>::new();
    for fact in facts {
        if replacements.contains_key(&fact.id) {
            continue;
        }
        let root = access_keys
            .get(&fact.root)
            .cloned()
            .unwrap_or(LocalPointerFieldKey {
                root: fact.root,
                indices: Vec::new(),
            });
        let mut entries = dynamic_pointer_field_entries(&root, &fact, &stored_fields);
        entries.sort_unstable_by_key(|(index, _)| *index);
        entries.dedup_by_key(|(index, _)| *index);
        let Some((_, first)) = entries.first().copied() else {
            continue;
        };
        let Some(value_ty) = value_result_type(ctx, first) else {
            continue;
        };
        if !entries
            .iter()
            .all(|(_, source)| value_result_type(ctx, *source) == Some(value_ty))
        {
            continue;
        }
        if let Some(pointer_type) = type_def_of(ctx, value_ty) {
            if pointer_type.class.opcode == Op::TypePointer {
                if let Some(Operand::StorageClass(storage)) = pointer_type.operands.first() {
                    pointer_roots.push((*storage, fact.id));
                }
            }
        }
        let Some(index_ty) = value_result_type(ctx, fact.index) else {
            continue;
        };
        if type_def_of(ctx, index_ty).is_none_or(|ty| ty.class.opcode != Op::TypeInt) {
            continue;
        }

        let mut emitted = Vec::new();
        let mut current = first;
        if entries.len() == 1 {
            emitted.push(Instruction::new(
                Op::CopyObject,
                Some(value_ty),
                Some(fact.id),
                vec![Operand::IdRef(first)],
            ));
        } else {
            let bool_ty = ctx.ty_bool();
            for (position, (entry, source)) in entries.iter().copied().enumerate().skip(1) {
                let expected = ctx.const_int_of(index_ty, entry as i64);
                let matches = ctx.module.fresh_id();
                emitted.push(Instruction::new(
                    Op::IEqual,
                    Some(bool_ty),
                    Some(matches),
                    vec![Operand::IdRef(fact.index), Operand::IdRef(expected)],
                ));
                let selected = if position + 1 == entries.len() {
                    fact.id
                } else {
                    ctx.module.fresh_id()
                };
                emitted.push(Instruction::new(
                    Op::Select,
                    Some(value_ty),
                    Some(selected),
                    vec![
                        Operand::IdRef(matches),
                        Operand::IdRef(source),
                        Operand::IdRef(current),
                    ],
                ));
                current = selected;
            }
        }
        replacements.insert(fact.id, emitted);
    }
    if replacements.is_empty() {
        return Ok(());
    }
    let replacement_ids = replacements.keys().copied().collect::<HashSet<_>>();
    let retired_load_pointers = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.class.opcode == Op::Load
                && instruction
                    .result_id
                    .is_some_and(|result| replacement_ids.contains(&result))
        })
        .filter_map(|instruction| match instruction.operands.first() {
            Some(Operand::IdRef(pointer)) => Some(*pointer),
            _ => None,
        })
        .collect::<Vec<_>>();

    for block in &mut ctx.module.functions[entry_idx].blocks {
        let old = block.instructions.clone();
        let mut rebuilt = Vec::with_capacity(old.len());
        for inst in old {
            if let Some(replacement) = inst
                .result_id
                .and_then(|result| replacements.remove(&result))
            {
                rebuilt.extend(replacement);
            } else {
                rebuilt.push(inst);
            }
        }
        block.instructions = rebuilt;
    }
    let defs = ctx
        .module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            instruction
                .result_id
                .map(|result| (result, instruction.clone()))
        })
        .collect::<HashMap<_, _>>();
    for storage in pointer_roots
        .iter()
        .map(|(storage, _)| *storage)
        .collect::<HashSet<_>>()
    {
        let roots = pointer_roots
            .iter()
            .filter_map(|(candidate, root)| (*candidate == storage).then_some(*root))
            .collect::<Vec<_>>();
        crate::passes::workgroup::rewrite_pointer_storage(ctx, entry_idx, &roots, storage, &defs)?;
    }
    crate::passes::resources::retire_dead_pointer_projections(
        ctx,
        entry_idx,
        retired_load_pointers.into_iter().chain(local_table_roots),
    );
    Ok(())
}

pub(in crate::passes) fn recover_unique_local_pointer_field_loads(ctx: &mut Ctx, entry_idx: usize) {
    let store_markers = local_pointer_field_store_markers(ctx);
    let store_sources = local_pointer_field_store_sources(ctx);
    let local_table_roots = local_pointer_field_store_keys(ctx)
        .into_values()
        .map(|key| key.root)
        .collect::<HashSet<_>>();
    if store_markers.is_empty() {
        return;
    }
    let mut access_keys = HashMap::<Word, LocalPointerFieldKey>::default();
    for inst in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
    {
        if let Some((result, key)) = local_pointer_field_access_key(ctx, &access_keys, inst) {
            access_keys.insert(result, key);
        }
    }

    let mut unique_stores =
        HashMap::<LocalPointerFieldKey, Option<(Word, usize, usize)>>::default();
    for (block_index, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (instruction_index, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode == Op::Store {
                let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(value))) =
                    (inst.operands.first(), inst.operands.get(1))
                else {
                    continue;
                };
                if let Some(key) = access_keys.get(ptr) {
                    let source = keyed_store_source(&store_sources, &store_markers, *value, key)
                        .copied()
                        .map(|source| (source, block_index, instruction_index));
                    unique_stores
                        .entry(key.clone())
                        .and_modify(|stored| *stored = None)
                        .or_insert(source);
                }
            }
        }
    }

    let mut replacements = Vec::<(Word, Word)>::new();
    for (block_index, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (instruction_index, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Load {
                continue;
            }
            let (Some(result), Some(result_ty), Some(Operand::IdRef(ptr))) =
                (inst.result_id, inst.result_type, inst.operands.first())
            else {
                continue;
            };
            let Some(key) = access_keys.get(ptr) else {
                continue;
            };
            let Some((source, store_block, store_instruction)) =
                unique_stores.get(key).copied().flatten()
            else {
                continue;
            };
            if (block_index == store_block && instruction_index <= store_instruction)
                || (block_index != store_block && store_block != 0)
            {
                continue;
            }
            let Some(source_ty) = value_result_type(ctx, source) else {
                continue;
            };
            if type_def_of(ctx, result_ty).is_none_or(|ty| ty.class.opcode != Op::TypePointer)
                || !type_def_of(ctx, source_ty).is_some_and(|ty| {
                    matches!(
                        ty.class.opcode,
                        Op::TypePointer | Op::TypeImage | Op::TypeSampler | Op::TypeSampledImage
                    )
                })
            {
                continue;
            }
            replacements.push((result, source));
        }
    }
    if replacements.is_empty() {
        return;
    }
    let dead = replacements
        .iter()
        .map(|(result, _)| *result)
        .collect::<HashSet<_>>();
    let retired_load_pointers = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            instruction.class.opcode == Op::Load
                && instruction
                    .result_id
                    .is_some_and(|result| dead.contains(&result))
        })
        .filter_map(|instruction| match instruction.operands.first() {
            Some(Operand::IdRef(pointer)) => Some(*pointer),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (from, to) in replacements {
        replace_id_in_function(&mut ctx.module.functions[entry_idx], from, to);
    }
    for block in &mut ctx.module.functions[entry_idx].blocks {
        block.instructions.retain(|inst| {
            !(inst.class.opcode == Op::Load
                && inst.result_id.is_some_and(|result| dead.contains(&result)))
        });
    }
    crate::passes::resources::retire_dead_pointer_projections(
        ctx,
        entry_idx,
        retired_load_pointers.into_iter().chain(local_table_roots),
    );
}

fn dynamic_pointer_field_entries(
    root: &LocalPointerFieldKey,
    fact: &crate::emit_sidecar::LocalPointerDynamicFieldLoad,
    stored_fields: &HashMap<LocalPointerFieldKey, Word>,
) -> Vec<(u32, Word)> {
    let mut entries = Vec::new();
    for (key, source) in stored_fields {
        if key.root != root.root {
            continue;
        }

        if key.indices.starts_with(&root.indices) {
            let relative = &key.indices[root.indices.len()..];
            if relative.len() == fact.prefix.len() + 1 + fact.suffix.len()
                && relative.starts_with(&fact.prefix)
                && relative[fact.prefix.len() + 1..] == fact.suffix
            {
                entries.push((relative[fact.prefix.len()], *source));
                continue;
            }
        }

        if fact.prefix.is_empty() && !root.indices.is_empty() {
            let dynamic_position = root.indices.len() - 1;
            if key.indices.len() == root.indices.len() + fact.suffix.len()
                && key.indices[..dynamic_position] == root.indices[..dynamic_position]
                && key.indices[root.indices.len()..] == fact.suffix
            {
                let base = root.indices[dynamic_position];
                let candidate = key.indices[dynamic_position];
                if let Some(entry) = candidate.checked_sub(base) {
                    entries.push((entry, *source));
                }
            }
        }
    }
    entries
}

fn local_pointer_field_store_sources(
    ctx: &Ctx,
) -> HashMap<(Word, LocalPointerFieldKey), Option<Word>> {
    let mut sources = HashMap::<(Word, LocalPointerFieldKey), Option<Word>>::default();
    for fact in &ctx.emit_sidecar.local_pointer_field_stores {
        let key = LocalPointerFieldKey {
            root: fact.root,
            indices: fact.indices.clone(),
        };
        sources
            .entry((fact.id, key))
            .and_modify(|source| {
                if *source != Some(fact.source) {
                    *source = None;
                }
            })
            .or_insert(Some(fact.source));
    }
    sources
}

fn keyed_store_source<'a>(
    sources: &'a HashMap<(Word, LocalPointerFieldKey), Option<Word>>,
    markers: &'a HashMap<Word, Word>,
    marker: Word,
    key: &LocalPointerFieldKey,
) -> Option<&'a Word> {
    if std::env::var_os("NVMTL_NO_B78_DW3D").is_none() {
        if let Some(source) = sources.get(&(marker, key.clone())) {
            return source.as_ref();
        }
    }
    markers.get(&marker)
}

pub(in crate::passes) fn local_pointer_field_store_markers(ctx: &Ctx) -> HashMap<Word, Word> {
    ctx.emit_sidecar
        .local_pointer_field_stores
        .iter()
        .map(|fact| (fact.id, fact.source))
        .collect()
}

fn local_pointer_field_store_keys(ctx: &Ctx) -> HashMap<Word, LocalPointerFieldKey> {
    ctx.emit_sidecar
        .local_pointer_field_stores
        .iter()
        .map(|fact| {
            (
                fact.id,
                LocalPointerFieldKey {
                    root: fact.root,
                    indices: fact.indices.clone(),
                },
            )
        })
        .collect()
}

pub(in crate::passes) fn local_pointer_field_load_markers(
    ctx: &Ctx,
) -> HashMap<Word, LocalPointerFieldKey> {
    ctx.emit_sidecar
        .local_pointer_field_loads
        .iter()
        .map(|fact| {
            (
                fact.id,
                LocalPointerFieldKey {
                    root: fact.root,
                    indices: fact.indices.clone(),
                },
            )
        })
        .collect()
}

pub(in crate::passes) fn local_pointer_field_access_key(
    ctx: &Ctx,
    access_keys: &HashMap<Word, LocalPointerFieldKey>,
    inst: &Instruction,
) -> Option<(Word, LocalPointerFieldKey)> {
    if !matches!(
        inst.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        return None;
    }
    let result = inst.result_id?;
    let Some(Operand::IdRef(base)) = inst.operands.first() else {
        return None;
    };
    let mut key = access_keys
        .get(base)
        .cloned()
        .unwrap_or(LocalPointerFieldKey {
            root: *base,
            indices: Vec::new(),
        });
    for operand in &inst.operands[1..] {
        key.indices.push(access_index_u32(ctx, operand)?);
    }
    Some((result, key))
}

pub(in crate::passes) fn access_index_u32(ctx: &Ctx, operand: &Operand) -> Option<u32> {
    match operand {
        Operand::LiteralBit32(value) => Some(*value),
        Operand::IdRef(id) => const_u32(ctx, *id),
        _ => None,
    }
}

pub(in crate::passes) fn const_u32(ctx: &Ctx, id: Word) -> Option<u32> {
    ctx.module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .find(|inst| inst.class.opcode == Op::Constant && inst.result_id == Some(id))
        .and_then(|inst| match inst.operands.first() {
            Some(Operand::LiteralBit32(value)) => Some(*value),
            Some(Operand::LiteralBit64(value)) => u32::try_from(*value).ok(),
            _ => None,
        })
}

pub(in crate::passes) fn compose_derived_access_chains(ctx: &mut Ctx, entry_idx: usize) {
    let mut provenance: HashMap<Word, AccessChainProvenance> = HashMap::default();
    let mut value_types = function_value_types(ctx, entry_idx);
    let n_blocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..n_blocks {
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut new_insts = Vec::with_capacity(insts.len());
        for mut inst in insts {
            if inst.class.opcode == Op::Select {
                rewrite_pointer_select_constant_operands(ctx, &mut inst);
                if let Some(result) = inst.result_id {
                    if let Some(prov) = selected_pointer_provenance(ctx, &provenance, &inst) {
                        provenance.insert(result, prov);
                    }
                }
                new_insts.push(inst);
                continue;
            }
            if !matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) {
                new_insts.push(inst);
                continue;
            }

            let mut pre = Vec::new();
            if let (Some(result), Some(ptr_ty), Some(Operand::IdRef(base))) =
                (inst.result_id, inst.result_type, inst.operands.first())
            {
                if let Some(prev) = provenance.get(base).cloned() {
                    let mut rewrote = false;
                    if let Some(ops) = compose_array_pointer_access(
                        ctx,
                        &prev,
                        ptr_ty,
                        &inst.operands[1..],
                        &mut pre,
                    ) {
                        inst = Instruction::new(prev.op, inst.result_type, inst.result_id, ops);
                        rewrote = true;
                    }
                    if !rewrote && pointer_pointee(ctx, prev.ptr_ty) == pointer_pointee(ctx, ptr_ty)
                    {
                        if let Some(offset) = derived_access_offset(ctx, &inst.operands[1..]) {
                            if let Some(last) = prev.indices.last().copied() {
                                if let Some(composed) =
                                    compose_access_index(ctx, last, offset, &mut pre)
                                {
                                    let mut indices = prev.indices.clone();
                                    if let Some(slot) = indices.last_mut() {
                                        *slot = composed;
                                    }
                                    let mut ops = vec![Operand::IdRef(prev.root)];
                                    ops.extend(indices.iter().copied().map(Operand::IdRef));
                                    inst = Instruction::new(
                                        prev.op,
                                        inst.result_type,
                                        inst.result_id,
                                        ops,
                                    );
                                }
                            }
                        }
                    }
                }

                if let Some(prov) = provenance_for_access_chain_with_types(ctx, &value_types, &inst)
                {
                    provenance.insert(result, prov);
                }
            }
            if let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) {
                value_types.insert(result, result_type);
            }
            new_insts.extend(pre);
            new_insts.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = new_insts;
    }
}

pub(in crate::passes) fn rewrite_pointer_select_constant_operands(
    ctx: &mut Ctx,
    inst: &mut Instruction,
) {
    let Some(result_type) = inst.result_type else {
        return;
    };
    if pointer_pointee(ctx, result_type).is_none() {
        return;
    }
    for operand in inst.operands.iter_mut().skip(1) {
        let Operand::IdRef(id) = operand else {
            continue;
        };
        let Some(opcode) = pointer_constant_opcode(ctx, *id) else {
            continue;
        };
        if value_result_type(ctx, *id) == Some(result_type) {
            continue;
        }
        *operand = Operand::IdRef(pointer_constant_for_type(ctx, opcode, result_type));
    }
}

pub(in crate::passes) fn pointer_constant_opcode(ctx: &Ctx, id: Word) -> Option<Op> {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .find(|inst| inst.result_id == Some(id))
        .and_then(|inst| match inst.class.opcode {
            Op::ConstantNull | Op::Undef => Some(inst.class.opcode),
            _ => None,
        })
}

pub(in crate::passes) fn pointer_constant_for_type(ctx: &mut Ctx, opcode: Op, ty: Word) -> Word {
    if let Some(existing) = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .find(|inst| inst.class.opcode == opcode && inst.result_type == Some(ty))
        .and_then(|inst| inst.result_id)
    {
        return existing;
    }
    let id = ctx.module.fresh_id();
    ctx.new_globals
        .push(Instruction::new(opcode, Some(ty), Some(id), vec![]));
    id
}

pub(in crate::passes) fn selected_pointer_provenance(
    ctx: &Ctx,
    provenance: &HashMap<Word, AccessChainProvenance>,
    inst: &Instruction,
) -> Option<AccessChainProvenance> {
    let result_type = inst.result_type?;
    pointer_pointee(ctx, result_type)?;
    let [_, Operand::IdRef(true_value), Operand::IdRef(false_value)] = inst.operands.as_slice()
    else {
        return None;
    };
    match (
        provenance.get(true_value).cloned(),
        is_constant_null_id(ctx, *true_value),
        provenance.get(false_value).cloned(),
        is_constant_null_id(ctx, *false_value),
    ) {
        (Some(mut prov), _, None, true) | (None, true, Some(mut prov), _) => {
            prov.ptr_ty = result_type;
            Some(prov)
        }
        _ => None,
    }
}

pub(in crate::passes) fn compose_array_pointer_access(
    ctx: &mut Ctx,
    prev: &AccessChainProvenance,
    ptr_ty: Word,
    operands: &[Operand],
    pre: &mut Vec<Instruction>,
) -> Option<Vec<Operand>> {
    let array_ty = pointer_pointee(ctx, prev.ptr_ty)?;
    let elem_ty = array_element_type(ctx, array_ty)?;
    if pointer_pointee(ctx, ptr_ty)? != elem_ty || prev.indices.is_empty() {
        return None;
    }
    match operands {
        [Operand::IdRef(member)] => {
            let mut ops = vec![Operand::IdRef(prev.root)];
            ops.extend(prev.indices.iter().copied().map(Operand::IdRef));
            ops.push(Operand::IdRef(*member));
            Some(ops)
        }
        [Operand::IdRef(offset), Operand::IdRef(member)] => {
            let last = prev.indices.last().copied()?;
            let composed = compose_access_index(ctx, last, *offset, pre)?;
            let mut indices = prev.indices.clone();
            if let Some(slot) = indices.last_mut() {
                *slot = composed;
            }
            let mut ops = vec![Operand::IdRef(prev.root)];
            ops.extend(indices.into_iter().map(Operand::IdRef));
            ops.push(Operand::IdRef(*member));
            Some(ops)
        }
        _ => None,
    }
}

pub(in crate::passes) fn array_element_type(ctx: &Ctx, ty: Word) -> Option<Word> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeArray | Op::TypeRuntimeArray => match def.operands.first() {
            Some(Operand::IdRef(elem)) => Some(*elem),
            _ => None,
        },
        _ => None,
    }
}

pub(in crate::passes) fn int64_signedness(ctx: &Ctx, ty: Word) -> Option<bool> {
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeInt {
        return None;
    }
    let width = match def.operands.first() {
        Some(Operand::LiteralBit32(w)) => *w,
        _ => return None,
    };
    if width != 64 {
        return None;
    }
    let signed = matches!(def.operands.get(1), Some(Operand::LiteralBit32(1)));
    Some(signed)
}

pub(in crate::passes) fn const_i64_value(ctx: &Ctx, id: Word) -> Option<u64> {
    let def = type_def_of(ctx, id)?;
    if def.class.opcode != Op::Constant {
        return None;
    }
    match def.operands.first() {
        Some(Operand::LiteralBit64(w)) => Some(*w),
        Some(Operand::LiteralBit32(w)) => Some(*w as u64),
        _ => None,
    }
}

pub(in crate::passes) fn pointer_pointee(ctx: &Ctx, ptr_ty: Word) -> Option<Word> {
    let def = type_def_of(ctx, ptr_ty)?;
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    match def.operands.get(1) {
        Some(Operand::IdRef(pointee)) => Some(*pointee),
        _ => None,
    }
}

pub(in crate::passes) fn derived_access_offset(ctx: &Ctx, operands: &[Operand]) -> Option<Word> {
    if operands.len() == 1 {
        return match operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        };
    }
    if operands.len() != 2 {
        return None;
    }
    let first = match operands.first() {
        Some(Operand::IdRef(id)) => *id,
        _ => return None,
    };
    let offset = match operands.get(1) {
        Some(Operand::IdRef(id)) => *id,
        _ => return None,
    };
    if first == offset || const_i64_value(ctx, first) == Some(0) {
        Some(offset)
    } else {
        None
    }
}

fn provenance_for_access_chain_with_types(
    ctx: &Ctx,
    value_types: &HashMap<Word, Word>,
    inst: &Instruction,
) -> Option<AccessChainProvenance> {
    let root = match inst.operands.first() {
        Some(Operand::IdRef(root)) => *root,
        _ => return None,
    };
    let root_type = value_types.get(&root).copied()?;
    provenance_for_access_chain_with_root_type(ctx, inst, root, root_type)
}

fn provenance_for_access_chain_with_root_type(
    ctx: &Ctx,
    inst: &Instruction,
    root: Word,
    root_type: Word,
) -> Option<AccessChainProvenance> {
    let ptr_ty = inst.result_type?;
    pointer_pointee(ctx, root_type)?;
    let indices = inst.operands[1..]
        .iter()
        .map(|op| match op {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    pointer_pointee(ctx, ptr_ty)?;
    Some(AccessChainProvenance {
        root,
        indices,
        ptr_ty,
        op: inst.class.opcode,
    })
}

pub(in crate::passes) fn compose_access_index(
    ctx: &mut Ctx,
    base_index: Word,
    offset: Word,
    pre: &mut Vec<Instruction>,
) -> Option<Word> {
    if const_i64_value(ctx, offset) == Some(0) {
        return Some(base_index);
    }
    match (
        const_i64_value(ctx, base_index),
        const_i64_value(ctx, offset),
    ) {
        (Some(a), Some(b)) => return Some(const_int_like(ctx, base_index, a + b)),
        (_, Some(0)) => return Some(base_index),
        _ => {}
    }
    let base_ty = value_result_type(ctx, base_index)?;
    let offset_ty = value_result_type(ctx, offset)?;
    if base_ty != offset_ty {
        return None;
    }
    let result = ctx.module.fresh_id();
    pre.push(Instruction::new(
        Op::IAdd,
        Some(base_ty),
        Some(result),
        vec![Operand::IdRef(base_index), Operand::IdRef(offset)],
    ));
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, Module, ModuleHeader};

    #[test]
    fn materializes_type_correct_local_pointer_field_store_source() {
        let ulong = 1;
        let uint = 2;
        let ptr_function_ulong = 3;
        let zero = 4;
        let marker = 5;
        let root = 10;
        let source = 11;
        let slot = 12;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(ulong),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
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
                Some(ptr_function_ulong),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(ulong),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(uint),
                Some(zero),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::ConstantNull, Some(ulong), Some(marker), vec![]),
        ];
        module.functions.push(Function {
            def: None,
            parameters: vec![
                Instruction::new(
                    Op::FunctionParameter,
                    Some(ptr_function_ulong),
                    Some(root),
                    vec![],
                ),
                Instruction::new(Op::FunctionParameter, Some(ulong), Some(source), vec![]),
            ],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::AccessChain,
                        Some(ptr_function_ulong),
                        Some(slot),
                        vec![Operand::IdRef(root), Operand::IdRef(zero)],
                    ),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(slot), Operand::IdRef(marker)],
                    ),
                ],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar.local_pointer_field_stores.push(
            crate::emit_sidecar::LocalPointerFieldStore {
                id: marker,
                source,
                root,
                indices: vec![0],
            },
        );

        materialize_inlined_local_pointer_field_stores(&mut ctx, 0);

        assert_eq!(
            ctx.module.functions[0].blocks[0].instructions[1].operands,
            vec![Operand::IdRef(slot), Operand::IdRef(source)]
        );
    }

    #[test]
    fn unique_entry_marker_store_repairs_successor_pointer_load() {
        let ulong = 1;
        let uint = 2;
        let ptr_function_ulong = 3;
        let ptr_uniform_uint = 4;
        let zero = 5;
        let marker = 6;
        let root = 10;
        let source = 11;
        let slot = 12;
        let loaded = 13;
        let consumer = 14;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(ulong),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
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
                Some(ptr_function_ulong),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(ulong),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(ptr_uniform_uint),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(uint),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(uint),
                Some(zero),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::ConstantNull, Some(ulong), Some(marker), vec![]),
        ];
        module.functions.push(Function {
            def: None,
            parameters: vec![
                Instruction::new(
                    Op::FunctionParameter,
                    Some(ptr_function_ulong),
                    Some(root),
                    vec![],
                ),
                Instruction::new(
                    Op::FunctionParameter,
                    Some(ptr_uniform_uint),
                    Some(source),
                    vec![],
                ),
            ],
            blocks: vec![
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                    instructions: vec![
                        Instruction::new(
                            Op::AccessChain,
                            Some(ptr_function_ulong),
                            Some(slot),
                            vec![Operand::IdRef(root), Operand::IdRef(zero)],
                        ),
                        Instruction::new(
                            Op::Store,
                            None,
                            None,
                            vec![Operand::IdRef(slot), Operand::IdRef(marker)],
                        ),
                        Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(21)]),
                    ],
                },
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(21), vec![])),
                    instructions: vec![
                        Instruction::new(
                            Op::Load,
                            Some(ptr_uniform_uint),
                            Some(loaded),
                            vec![Operand::IdRef(slot)],
                        ),
                        Instruction::new(
                            Op::CopyObject,
                            Some(ptr_uniform_uint),
                            Some(consumer),
                            vec![Operand::IdRef(loaded)],
                        ),
                    ],
                },
            ],
            end: None,
        });
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar.local_pointer_field_stores.push(
            crate::emit_sidecar::LocalPointerFieldStore {
                id: marker,
                source,
                root,
                indices: vec![0],
            },
        );

        recover_unique_local_pointer_field_loads(&mut ctx, 0);

        let body = &ctx.module.functions[0].blocks[1].instructions;
        assert!(!body.iter().any(|inst| inst.result_id == Some(loaded)));
        assert_eq!(
            body.iter()
                .find(|inst| inst.result_id == Some(consumer))
                .unwrap()
                .operands,
            vec![Operand::IdRef(source)]
        );
    }
}
