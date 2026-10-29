use crate::fxhash::HashMap;
use crate::fxhash::HashSet;

use crate::spirv_module::Instruction;
use crate::spirv_module::Operand;
use spirv::{Op, StorageClass, Word};

use super::super::{air_names, type_def_of, value_result_type, Ctx};

pub(crate) fn materialize_texture_array_loads(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.image_array_vars.is_empty() && ctx.emit_sidecar.device_handle_heap_loads.is_empty() {
        return;
    }
    let names = air_names(&ctx.module);

    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if let Some(rid) = inst.result_id {
                defs.insert(rid, inst.clone());
            }
        }
    }
    let fixed_elements = ctx
        .emit_sidecar
        .local_pointer_field_loads
        .iter()
        .filter_map(|fact| fixed_array_element(ctx, fact).map(|resolved| (fact.id, resolved)))
        .collect::<Vec<_>>();
    let mut fixed_handles: HashMap<Word, (Word, Word)> = fixed_elements
        .into_iter()
        .map(|(id, (arrayvar, element))| (id, (arrayvar, ctx.const_uint(element))))
        .collect();
    let buffer_fixed_elements = ctx
        .emit_sidecar
        .buffer_pointer_field_loads
        .iter()
        .filter(|fact| ctx.image_array_vars.contains_key(&fact.root) && fact.byte_offset % 8 == 0)
        .map(|fact| (fact.id, fact.root, (fact.byte_offset / 8) as u32))
        .collect::<Vec<_>>();
    fixed_handles.extend(
        buffer_fixed_elements
            .into_iter()
            .map(|(id, root, element)| (id, (root, ctx.const_uint(element)))),
    );
    for (id, resolved) in fixed_handles.clone() {
        let mut alias = id;
        for _ in 0..8 {
            let Some(inst) = defs
                .get(&alias)
                .filter(|inst| inst.class.opcode == Op::CopyObject)
            else {
                break;
            };
            let Some(Operand::IdRef(source)) = inst.operands.first() else {
                break;
            };
            fixed_handles.entry(*source).or_insert(resolved);
            alias = *source;
        }
    }
    let mut local_table_handles = HashMap::default();
    for fact in &ctx.emit_sidecar.local_pointer_dynamic_field_loads {
        let resolved = local_table_array_elements(ctx, &defs, fact, &fixed_handles);
        if let Some(resolved) = resolved {
            local_table_handles.insert(fact.id, resolved);
        }
    }
    let mut dynamic_handles: HashMap<Word, (Word, Word)> = ctx
        .emit_sidecar
        .local_pointer_dynamic_field_loads
        .iter()
        .filter_map(|fact| dynamic_array_element(ctx, fact).map(|resolved| (fact.id, resolved)))
        .chain(
            ctx.emit_sidecar
                .buffer_pointer_dynamic_field_loads
                .iter()
                .filter(|fact| ctx.image_array_vars.contains_key(&fact.root))
                .map(|fact| (fact.id, (fact.root, fact.index))),
        )
        .collect();
    for (id, resolved) in dynamic_handles.clone() {
        let mut alias = id;
        for _ in 0..8 {
            let Some(inst) = defs
                .get(&alias)
                .filter(|inst| inst.class.opcode == Op::CopyObject)
            else {
                break;
            };
            let Some(Operand::IdRef(source)) = inst.operands.first() else {
                break;
            };
            dynamic_handles.entry(*source).or_insert(resolved);
            alias = *source;
        }
    }

    let mut candidates: Vec<(Word, Word)> = Vec::new();
    let mut table_candidates = Vec::new();
    let mut handles: HashMap<Word, (Word, Word)> = HashMap::default();
    let mut handle_order: Vec<Word> = Vec::new();
    let mut handle_preludes: HashMap<Word, Vec<Instruction>> = HashMap::default();
    let mut seen: crate::fxhash::HashSet<Word> = crate::fxhash::HashSet::default();
    let device_texture_slots: HashMap<Word, Word> = ctx
        .emit_sidecar
        .device_handle_heap_loads
        .iter()
        .filter(|fact| !fact.sampler)
        .map(|fact| (fact.id, fact.slot))
        .collect();
    let mut device_candidates: Vec<(Word, Word, String)> = Vec::new();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                continue;
            };
            let Some(name) = names.get(callee) else {
                continue;
            };
            if !is_texture_intrinsic(name) {
                continue;
            }
            let Some(Operand::IdRef(handle)) = inst.operands.get(1) else {
                continue;
            };
            if let Some(&slot) = device_texture_slots.get(handle) {
                device_candidates.push((*handle, slot, name.to_string()));
                seen.insert(*handle);
                continue;
            }
            if !seen.insert(*handle) {
                continue;
            }
            if let Some(&(arrayvar, idx)) = dynamic_handles.get(handle) {
                if handles.insert(*handle, (arrayvar, idx)).is_none() {
                    handle_order.push(*handle);
                }
                continue;
            }
            if let Some(&(arrayvar, idx)) = fixed_handles.get(handle) {
                if handles.insert(*handle, (arrayvar, idx)).is_none() {
                    handle_order.push(*handle);
                }
                continue;
            }
            if let Some((arrayvar, selector, entries)) = local_table_handles.get(handle).cloned() {
                table_candidates.push((*handle, arrayvar, selector, entries));
                continue;
            }
            let Some(load) = defs.get(handle) else {
                continue;
            };
            if load.class.opcode != Op::Load {
                continue;
            }
            let Some(Operand::IdRef(ptr)) = load.operands.first() else {
                continue;
            };
            candidates.push((*handle, *ptr));
        }
    }
    for (handle, ptr) in candidates {
        if let Some((arrayvar, idx)) = resolve_array_element(ctx, &defs, ptr) {
            if handles.insert(handle, (arrayvar, idx)).is_none() {
                handle_order.push(handle);
            }
        }
    }
    let mut device_groups: Vec<(Word, Word, Vec<String>)> = Vec::new();
    for (handle, slot, name) in device_candidates {
        match device_groups.iter_mut().find(|group| group.0 == handle) {
            Some(group) => group.2.push(name),
            None => device_groups.push((handle, slot, vec![name])),
        }
    }
    for (handle, slot, names) in device_groups {
        let atomic = names
            .iter()
            .any(|n| crate::air_intrinsics::is_texture_atomic(n));
        let storage = atomic
            || names
                .iter()
                .any(|n| n.starts_with("air.write_texture") || n.contains(".device_coherent"));
        let Some(name) = names
            .iter()
            .find(|n| crate::passes::air_calls::intrinsic_texture_shape(n).is_some())
            .cloned()
        else {
            continue;
        };
        let comp_name = names
            .iter()
            .find(|n| n.contains(".u.") || n.contains(".s."))
            .cloned()
            .unwrap_or_else(|| name.clone());
        let Some((dim, arrayed)) = crate::passes::air_calls::intrinsic_texture_shape(&name) else {
            continue;
        };
        if names.iter().any(|n| n.contains("_ms")) {
            continue;
        }
        let comp = if comp_name.contains(".u.") {
            crate::passes::ImageComp::Uint
        } else if comp_name.contains(".s.") {
            crate::passes::ImageComp::Sint
        } else {
            crate::passes::ImageComp::Float
        };
        let (image_ty, heap) = if storage {
            let format = match (atomic, comp) {
                (false, _) => spirv::ImageFormat::Unknown,
                (true, crate::passes::ImageComp::Uint) => spirv::ImageFormat::R32ui,
                (true, crate::passes::ImageComp::Sint) => spirv::ImageFormat::R32i,
                (true, crate::passes::ImageComp::Float) => spirv::ImageFormat::R32f,
            };
            let ty = ctx.ty_storage_image(dim, arrayed, format, comp);
            (
                ty,
                crate::passes::stage_input::bindless_storage_heap_var(
                    ctx,
                    ty,
                    (dim, arrayed),
                    comp,
                ),
            )
        } else {
            let ty = ctx.ty_image(dim, arrayed, comp);
            (
                ty,
                crate::passes::stage_input::bindless_heap_var(ctx, ty, (dim, arrayed), comp),
            )
        };
        let _ = image_ty;
        if handles.insert(handle, (heap, slot)).is_none() {
            handle_order.push(handle);
        }
    }
    for (handle, arrayvar, selector, entries) in table_candidates {
        if let Some((idx, prelude)) = descriptor_table_index(ctx, selector, &entries) {
            if handles.insert(handle, (arrayvar, idx)).is_none() {
                handle_order.push(handle);
            }
            handle_preludes.insert(handle, prelude);
        }
    }
    if handles.is_empty() {
        return;
    }

    let mut new_access_chains: HashMap<Word, Instruction> = HashMap::default();
    let mut retyped_load_ptr: HashMap<Word, (Word, Word)> = HashMap::default();
    let mut dead_roots: Vec<Word> = Vec::new();
    for &handle in &handle_order {
        let (arrayvar, idx) = handles[&handle];
        if !defs.contains_key(&handle) {
            continue;
        }
        let &(elem_image_ty, dim, comp, multisampled) =
            ctx.image_array_vars.get(&arrayvar).unwrap();
        let ptr_image = ctx.ty_ptr(StorageClass::UniformConstant, elem_image_ty);
        let p = ctx.module.fresh_id();
        new_access_chains.insert(
            handle,
            Instruction::new(
                Op::AccessChain,
                Some(ptr_image),
                Some(p),
                vec![Operand::IdRef(arrayvar), Operand::IdRef(idx)],
            ),
        );
        retyped_load_ptr.insert(handle, (elem_image_ty, p));
        ctx.image_dims.insert(handle, dim);
        ctx.image_comp.insert(handle, comp);
        if multisampled {
            ctx.image_multisampled.insert(handle);
        }
        if image_type_is_storage(ctx, elem_image_ty) {
            ctx.image_storage.insert(handle);
            if let Some((metal_index, state)) =
                ctx.runtime_storage_image_values.get(&arrayvar).copied()
            {
                ctx.register_runtime_storage_image_value(handle, metal_index, Some(state));
            }
        }
        if let Some(load) = defs.get(&handle) {
            if let Some(Operand::IdRef(old_ptr)) = load.operands.first() {
                if *old_ptr != arrayvar {
                    dead_roots.push(*old_ptr);
                }
            }
        }
    }
    dead_roots.extend(handle_order.iter().map(|handle| handles[handle].0));

    let mut placeholder_uses: HashMap<(usize, usize), (Instruction, Instruction, Word)> =
        HashMap::default();
    let mut placeholder_specs = Vec::new();
    for (block_idx, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (inst_idx, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(handle)) = inst.operands.get(1) else {
                continue;
            };
            let Some(&(arrayvar, idx)) = handles.get(handle) else {
                continue;
            };
            if defs.contains_key(handle) {
                continue;
            }
            placeholder_specs.push((block_idx, inst_idx, arrayvar, idx));
        }
    }
    for (block_idx, inst_idx, arrayvar, idx) in placeholder_specs {
        let &(image_ty, dim, comp, multisampled) = ctx.image_array_vars.get(&arrayvar).unwrap();
        let ptr_image = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
        let pointer = ctx.module.fresh_id();
        let image = ctx.module.fresh_id();
        let chain = Instruction::new(
            Op::AccessChain,
            Some(ptr_image),
            Some(pointer),
            vec![Operand::IdRef(arrayvar), Operand::IdRef(idx)],
        );
        let load = Instruction::new(
            Op::Load,
            Some(image_ty),
            Some(image),
            vec![Operand::IdRef(pointer)],
        );
        ctx.image_dims.insert(image, dim);
        ctx.image_comp.insert(image, comp);
        if multisampled {
            ctx.image_multisampled.insert(image);
        }
        if image_type_is_storage(ctx, image_ty) {
            ctx.image_storage.insert(image);
            if let Some((metal_index, state)) =
                ctx.runtime_storage_image_values.get(&arrayvar).copied()
            {
                ctx.register_runtime_storage_image_value(image, metal_index, Some(state));
            }
        }
        placeholder_uses.insert((block_idx, inst_idx), (chain, load, image));
    }

    let func = &mut ctx.module.functions[entry_idx];
    for (block_idx, blk) in func.blocks.iter_mut().enumerate() {
        let mut rebuilt: Vec<Instruction> =
            Vec::with_capacity(blk.instructions.len() + handles.len());
        for (inst_idx, inst) in blk.instructions.clone().into_iter().enumerate() {
            if let Some((chain, load, image)) = placeholder_uses.remove(&(block_idx, inst_idx)) {
                rebuilt.push(chain);
                rebuilt.push(load);
                let mut call = inst;
                call.operands[1] = Operand::IdRef(image);
                rebuilt.push(call);
                continue;
            }
            if let Some(handle) = inst.result_id {
                if let (Some(chain), Some(&(image_ty, p))) = (
                    new_access_chains.get(&handle),
                    retyped_load_ptr.get(&handle),
                ) {
                    if let Some(prelude) = handle_preludes.remove(&handle) {
                        rebuilt.extend(prelude);
                    }
                    rebuilt.push(chain.clone());
                    let mut load = inst;
                    load.class.opcode = Op::Load;
                    load.result_type = Some(image_ty);
                    load.operands = vec![Operand::IdRef(p)];
                    rebuilt.push(load);
                    continue;
                }
            }
            rebuilt.push(inst);
        }
        blk.instructions = rebuilt;
    }
    ctx.emit_sidecar
        .local_pointer_dynamic_field_loads
        .retain(|fact| !handles.contains_key(&fact.id));

    retire_dead_pointer_projections(ctx, entry_idx, dead_roots.iter().copied());
}

/// Element `i` of a sampled `array<texture, N>` entry parameter reads the texture bound at Metal index
/// `base + i` the way a single texture does under bindless-all: its heap slot from the per-draw table,
/// then the heap. The plugin declares every texture index as its own count-1 binding, so the
/// `[N x image]` variable at the first binding read element 1 past that binding: a probe sampling two
/// depth maps through `array<depth2d<float>, 2>` compared element 1 against map 0 (RTX 5060, 10-28;
/// an M1 Pro reads map 1). The table holds a slot for every index the app bound, constant or dynamic
/// `i` alike. Runs after every pass that recognises the array variable's own access chains.
pub(crate) fn route_texture_array_elements_through_heap(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.image_array_base.is_empty()
        || !super::bindless_all_on()
        || !super::address_table_usable(ctx)
    {
        return;
    }
    let mut sites: Vec<(usize, usize, Word, Word, Word, Word, u32, u32)> = Vec::new();
    for (block_idx, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (inst_idx, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                continue;
            }
            let [Operand::IdRef(var), Operand::IdRef(idx)] = inst.operands.as_slice() else {
                continue;
            };
            let (Some(&(base, count)), Some(result), Some(result_ty)) =
                (ctx.image_array_base.get(var), inst.result_id, inst.result_type)
            else {
                continue;
            };
            sites.push((block_idx, inst_idx, result, result_ty, *var, *idx, base, count));
        }
    }
    if sites.is_empty() {
        return;
    }
    let Ok((table, word_ptr_ty, wide)) = super::ensure_address_table(ctx) else {
        return;
    };
    let uint_ty = ctx.ty_uint();
    let c0 = ctx.const_uint(0);
    let mask = ctx.const_uint(crate::reflect::BINDLESS_HEAP_SLOTS - 1);
    let local_types: HashMap<Word, Word> = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect();
    let mut replacements: HashMap<(usize, usize), Vec<Instruction>> = HashMap::default();
    for (block_idx, inst_idx, result, result_ty, var, idx, base, count) in sites {
        let &(elem_ty, dims, comp, _) = ctx.image_array_vars.get(&var).unwrap();
        let mut out = Vec::new();
        // the element index as a 32-bit unsigned value: AIR's dynamic indices are 64-bit (a GEP index), a
        // signed 32-bit one is reinterpreted; an index whose type cannot be read leaves the chain as it was
        let index_ty = local_types.get(&idx).copied().or_else(|| value_result_type(ctx, idx));
        let width = index_ty.and_then(|ty| type_def_of(ctx, ty)).and_then(|def| {
            (def.class.opcode == Op::TypeInt).then(|| match def.operands.first() {
                Some(Operand::LiteralBit32(width)) => Some(*width),
                _ => None,
            })?
        });
        let index = match (index_ty, width) {
            (Some(ty), _) if ty == uint_ty => idx,
            (Some(_), Some(32)) => {
                let cast = ctx.module.fresh_id();
                out.push(Instruction::new(Op::Bitcast, Some(uint_ty), Some(cast), vec![Operand::IdRef(idx)]));
                cast
            }
            (Some(_), Some(64)) => {
                let narrow = ctx.module.fresh_id();
                out.push(Instruction::new(Op::UConvert, Some(uint_ty), Some(narrow), vec![Operand::IdRef(idx)]));
                narrow
            }
            _ => continue,
        };
        // an index past the array reads its last element instead of another table word (a buffer address)
        let last = ctx.const_uint(count.saturating_sub(1));
        let clamped = ctx.module.fresh_id();
        let glsl = ctx.glsl();
        out.push(Instruction::new(
            Op::ExtInst,
            Some(uint_ty),
            Some(clamped),
            vec![
                Operand::IdRef(glsl),
                Operand::LiteralExtInstInteger(spirv::GlslStd450Op::UMin as u32),
                Operand::IdRef(index),
                Operand::IdRef(last),
            ],
        ));
        let first = ctx.const_uint(super::TEXTURE_INDEX_BASE + base);
        let cel = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::IAdd,
            Some(uint_ty),
            Some(cel),
            vec![Operand::IdRef(first), Operand::IdRef(clamped)],
        ));
        let wptr = ctx.module.fresh_id();
        let word = ctx.module.fresh_id();
        if let Some(u64_ty) = wide {
            out.push(Instruction::new(
                Op::AccessChain,
                Some(word_ptr_ty),
                Some(wptr),
                vec![Operand::IdRef(table), Operand::IdRef(c0), Operand::IdRef(cel)],
            ));
            let wide_v = ctx.module.fresh_id();
            out.push(Instruction::new(Op::Load, Some(u64_ty), Some(wide_v), vec![Operand::IdRef(wptr)]));
            out.push(Instruction::new(Op::UConvert, Some(uint_ty), Some(word), vec![Operand::IdRef(wide_v)]));
        } else {
            out.push(Instruction::new(
                Op::AccessChain,
                Some(word_ptr_ty),
                Some(wptr),
                vec![
                    Operand::IdRef(table),
                    Operand::IdRef(c0),
                    Operand::IdRef(cel),
                    Operand::IdRef(c0),
                ],
            ));
            out.push(Instruction::new(Op::Load, Some(uint_ty), Some(word), vec![Operand::IdRef(wptr)]));
        }
        let slot = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::BitwiseAnd,
            Some(uint_ty),
            Some(slot),
            vec![Operand::IdRef(word), Operand::IdRef(mask)],
        ));
        let heap = crate::passes::stage_input::bindless_heap_var(ctx, elem_ty, dims, comp);
        out.push(Instruction::new(
            Op::AccessChain,
            Some(result_ty),
            Some(result),
            vec![Operand::IdRef(heap), Operand::IdRef(slot)],
        ));
        replacements.insert((block_idx, inst_idx), out);
    }
    let function = &mut ctx.module.functions[entry_idx];
    for (block_idx, block) in function.blocks.iter_mut().enumerate() {
        if !replacements.keys().any(|(b, _)| *b == block_idx) {
            continue;
        }
        let mut rebuilt = Vec::with_capacity(block.instructions.len() + 8);
        for (inst_idx, inst) in block.instructions.drain(..).enumerate() {
            match replacements.remove(&(block_idx, inst_idx)) {
                Some(seq) => rebuilt.extend(seq),
                None => rebuilt.push(inst),
            }
        }
        block.instructions = rebuilt;
    }
}

pub(crate) fn sink_loop_header_texture_array_loads(ctx: &mut Ctx, entry_idx: usize) {
    if ctx.image_array_vars.is_empty() {
        return;
    }

    let function = &ctx.module.functions[entry_idx];
    let mut defs: HashMap<Word, (usize, usize, Instruction)> = HashMap::default();
    let mut uses: HashMap<Word, Vec<(usize, usize)>> = HashMap::default();
    for (block_idx, block) in function.blocks.iter().enumerate() {
        for (inst_idx, inst) in block.instructions.iter().enumerate() {
            if let Some(id) = inst.result_id {
                defs.insert(id, (block_idx, inst_idx, inst.clone()));
            }
            for operand in &inst.operands {
                if let Operand::IdRef(id) = operand {
                    uses.entry(*id).or_default().push((block_idx, inst_idx));
                }
            }
        }
    }

    let loop_headers: HashSet<usize> = function
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            block
                .instructions
                .iter()
                .any(|inst| inst.class.opcode == Op::LoopMerge)
        })
        .map(|(block_idx, _)| block_idx)
        .collect();
    let mut insertions: HashMap<(usize, usize), Vec<Instruction>> = HashMap::default();
    let mut removals = HashSet::default();

    for (&load_id, &(load_block, _, ref load)) in &defs {
        if load.class.opcode != Op::Load || load.operands.len() != 1 {
            continue;
        }
        let Some(Operand::IdRef(pointer_id)) = load.operands.first() else {
            continue;
        };
        let Some(&(chain_block, _, ref chain)) = defs.get(pointer_id) else {
            continue;
        };
        if load_block != chain_block || !loop_headers.contains(&load_block) {
            continue;
        }
        if !matches!(
            chain.class.opcode,
            Op::AccessChain | Op::InBoundsAccessChain
        ) {
            continue;
        }
        let Some(Operand::IdRef(root)) = chain.operands.first() else {
            continue;
        };
        if !ctx.image_array_vars.contains_key(root) {
            continue;
        }
        if uses.get(pointer_id).map(Vec::as_slice) != Some(&[(load_block, defs[&load_id].1)][..]) {
            continue;
        }
        let Some([(use_block, use_inst)]) = uses.get(&load_id).map(Vec::as_slice) else {
            continue;
        };
        if *use_block == load_block
            || function.blocks[*use_block].instructions[*use_inst]
                .class
                .opcode
                == Op::Phi
        {
            continue;
        }
        insertions
            .entry((*use_block, *use_inst))
            .or_default()
            .extend([chain.clone(), load.clone()]);
        removals.insert(*pointer_id);
        removals.insert(load_id);
    }

    if removals.is_empty() {
        return;
    }
    let function = &mut ctx.module.functions[entry_idx];
    for (block_idx, block) in function.blocks.iter_mut().enumerate() {
        let old = block.instructions.clone();
        let mut rebuilt = Vec::with_capacity(old.len());
        for (inst_idx, inst) in old.into_iter().enumerate() {
            if let Some(prefix) = insertions.remove(&(block_idx, inst_idx)) {
                rebuilt.extend(prefix);
            }
            if !inst.result_id.is_some_and(|id| removals.contains(&id)) {
                rebuilt.push(inst);
            }
        }
        block.instructions = rebuilt;
    }
}

fn is_texture_intrinsic(name: &str) -> bool {
    name.starts_with("air.sample_texture")
        || name.starts_with("air.sample_depth")
        || name.starts_with("air.sample_compare_depth")
        || name.starts_with("air.gather_compare_depth")
        || crate::air_intrinsics::is_texture_atomic(name)
        || name.starts_with("air.read_texture")
        || name.starts_with("air.read_depth")
        || name.starts_with("air.write_texture")
        || name.starts_with("air.gather_texture")
        || name.starts_with("air.gather_depth")
        || name.starts_with("air.get_width_texture")
        || name.starts_with("air.get_height_texture")
        || name.starts_with("air.get_depth_texture")
        || name.starts_with("air.get_array_size_texture")
        || name.starts_with("air.get_width_depth")
        || name.starts_with("air.get_height_depth")
        || name.starts_with("air.get_depth_depth")
        || name.starts_with("air.get_num_mip_levels_texture")
        || name.starts_with("air.get_num_mip_levels_depth")
        || crate::air_intrinsics::is_image_sample_count_query(name)
        || name.starts_with("air.is_null_texture")
}
fn image_type_is_storage(ctx: &Ctx, image_ty: Word) -> bool {
    type_def_of(ctx, image_ty)
        .filter(|def| def.class.opcode == Op::TypeImage)
        .and_then(|def| match def.operands.get(5) {
            Some(Operand::LiteralBit32(sampled)) => Some(*sampled == 2),
            _ => None,
        })
        .unwrap_or(false)
}

fn dynamic_array_element(
    ctx: &Ctx,
    fact: &crate::emit_sidecar::LocalPointerDynamicFieldLoad,
) -> Option<(Word, Word)> {
    if !ctx.image_array_vars.contains_key(&fact.root) || !fact.prefix.is_empty() {
        return None;
    }
    if !(fact.suffix.is_empty() || fact.suffix.as_slice() == [0]) {
        return None;
    }
    Some((fact.root, fact.index))
}

fn local_table_array_elements(
    ctx: &Ctx,
    defs: &HashMap<Word, Instruction>,
    fact: &crate::emit_sidecar::LocalPointerDynamicFieldLoad,
    fixed_handles: &HashMap<Word, (Word, Word)>,
) -> Option<(Word, Word, Vec<(u32, Word)>)> {
    let mut entries = Vec::new();
    let mut arrayvar = None;
    for store in &ctx.emit_sidecar.local_pointer_field_stores {
        if store.root != fact.root {
            continue;
        }
        let prefix_len = fact.prefix.len();
        if store.indices.len() != prefix_len + 1 + fact.suffix.len()
            || !store.indices.starts_with(&fact.prefix)
            || store.indices[prefix_len + 1..] != fact.suffix
        {
            continue;
        }
        let (source_array, descriptor_index) =
            fixed_descriptor_element(defs, fixed_handles, store.source)?;
        match arrayvar {
            Some(existing) if existing != source_array => return None,
            Some(_) => {}
            None => arrayvar = Some(source_array),
        }
        entries.push((store.indices[prefix_len], descriptor_index));
    }
    entries.sort_unstable_by_key(|(table_index, _)| *table_index);
    if entries
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
    {
        return None;
    }
    entries.dedup_by_key(|(table_index, _)| *table_index);
    (entries.len() >= 2).then_some((arrayvar?, fact.index, entries))
}

fn fixed_descriptor_element(
    defs: &HashMap<Word, Instruction>,
    fixed_handles: &HashMap<Word, (Word, Word)>,
    mut value: Word,
) -> Option<(Word, Word)> {
    for _ in 0..8 {
        if let Some(resolved) = fixed_handles.get(&value) {
            return Some(*resolved);
        }
        let inst = defs.get(&value)?;
        value = match inst.class.opcode {
            Op::CopyObject => match inst.operands.first()? {
                Operand::IdRef(source) => *source,
                _ => return None,
            },
            Op::CompositeExtract => {
                let Operand::IdRef(composite) = inst.operands.first()? else {
                    return None;
                };
                let path = literal_index_path(&inst.operands[1..])?;
                resolve_inserted_value(defs, *composite, &path)?
            }
            _ => return None,
        };
    }
    None
}

fn descriptor_table_index(
    ctx: &mut Ctx,
    selector: Word,
    entries: &[(u32, Word)],
) -> Option<(Word, Vec<Instruction>)> {
    let mut instructions = Vec::new();
    let mut current = entries.first()?.1;
    let selector_ty = value_result_type(ctx, selector)?;
    let bool_ty = ctx.ty_bool();
    let uint_ty = ctx.ty_uint();
    for (table_index, descriptor_index) in entries.iter().copied().skip(1) {
        let expected = ctx.const_int_of(selector_ty, i64::from(table_index));
        let matches = ctx.module.fresh_id();
        instructions.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(matches),
            vec![Operand::IdRef(selector), Operand::IdRef(expected)],
        ));
        let selected = ctx.module.fresh_id();
        instructions.push(Instruction::new(
            Op::Select,
            Some(uint_ty),
            Some(selected),
            vec![
                Operand::IdRef(matches),
                Operand::IdRef(descriptor_index),
                Operand::IdRef(current),
            ],
        ));
        current = selected;
    }
    Some((current, instructions))
}

fn fixed_array_element(
    ctx: &Ctx,
    fact: &crate::emit_sidecar::LocalPointerFieldLoad,
) -> Option<(Word, u32)> {
    if !ctx.image_array_vars.contains_key(&fact.root) {
        return None;
    }
    match fact.indices.as_slice() {
        [element] | [element, 0] => Some((fact.root, *element)),
        _ => None,
    }
}

fn resolve_array_element(
    ctx: &mut Ctx,
    defs: &HashMap<Word, Instruction>,
    ptr: Word,
) -> Option<(Word, Word)> {
    if ctx.image_array_vars.contains_key(&ptr) {
        let zero = ctx.const_uint(0);
        return Some((ptr, zero));
    }
    let inst = defs.get(&ptr)?;
    match inst.class.opcode {
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => {
            let Operand::IdRef(base) = inst.operands.first()? else {
                return None;
            };
            let arrayvar = resolve_ptr_root(ctx, defs, *base)?;
            let Operand::IdRef(idx) = inst.operands.get(1)? else {
                return None;
            };
            Some((arrayvar, *idx))
        }
        Op::Bitcast | Op::CopyObject => {
            let Operand::IdRef(src) = inst.operands.first()? else {
                return None;
            };
            resolve_array_element(ctx, defs, *src)
        }
        _ => None,
    }
}

fn resolve_ptr_root(ctx: &Ctx, defs: &HashMap<Word, Instruction>, mut ptr: Word) -> Option<Word> {
    for _ in 0..8 {
        if ctx.image_array_vars.contains_key(&ptr) {
            return Some(ptr);
        }
        let inst = defs.get(&ptr)?;
        match inst.class.opcode {
            Op::Bitcast | Op::CopyObject => {
                let Operand::IdRef(src) = inst.operands.first()? else {
                    return None;
                };
                ptr = *src;
            }
            Op::CompositeExtract => {
                let Operand::IdRef(composite) = inst.operands.first()? else {
                    return None;
                };
                let path = literal_index_path(&inst.operands[1..])?;
                ptr = resolve_inserted_value(defs, *composite, &path)?;
            }
            _ => return None,
        }
    }
    None
}

fn resolve_inserted_value(
    defs: &HashMap<Word, Instruction>,
    mut composite: Word,
    path: &[u32],
) -> Option<Word> {
    for _ in 0..8 {
        let inst = defs.get(&composite)?;
        match inst.class.opcode {
            Op::CompositeInsert => {
                let Operand::IdRef(inserted) = inst.operands.first()? else {
                    return None;
                };
                let Operand::IdRef(base) = inst.operands.get(1)? else {
                    return None;
                };
                if literal_index_path(&inst.operands[2..])?.as_slice() == path {
                    return Some(*inserted);
                }
                composite = *base;
            }
            Op::CopyObject => {
                let Operand::IdRef(source) = inst.operands.first()? else {
                    return None;
                };
                composite = *source;
            }
            _ => return None,
        }
    }
    None
}

fn literal_index_path(operands: &[Operand]) -> Option<Vec<u32>> {
    operands
        .iter()
        .map(|operand| match operand {
            Operand::LiteralBit32(index) => Some(*index),
            _ => None,
        })
        .collect()
}

pub(in crate::passes) fn retire_dead_pointer_projections(
    ctx: &mut Ctx,
    entry_idx: usize,
    roots: impl IntoIterator<Item = Word>,
) {
    let is_pointer_derivation = |instruction: &Instruction| {
        matches!(
            instruction.class.opcode,
            Op::AccessChain
                | Op::InBoundsAccessChain
                | Op::PtrAccessChain
                | Op::Bitcast
                | Op::CopyObject
        )
    };
    let mut candidates = roots.into_iter().collect::<HashSet<_>>();
    let mut removed = HashSet::default();
    loop {
        let additions = ctx.module.functions[entry_idx]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| is_pointer_derivation(instruction))
            .filter(|instruction| {
                instruction
                    .operands
                    .iter()
                    .any(|operand| matches!(operand, Operand::IdRef(id) if candidates.contains(id)))
            })
            .filter_map(|instruction| instruction.result_id)
            .filter(|result| !candidates.contains(result))
            .collect::<Vec<_>>();
        if additions.is_empty() {
            break;
        }
        candidates.extend(additions);
    }
    loop {
        let mut used: crate::fxhash::HashSet<Word> = crate::fxhash::HashSet::default();
        for f in &ctx.module.functions {
            for blk in &f.blocks {
                for inst in &blk.instructions {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            used.insert(*id);
                        }
                    }
                }
            }
        }
        for inst in ctx
            .module
            .types_global_values
            .iter()
            .chain(ctx.new_globals.iter())
            .chain(ctx.module.entry_points.iter())
        {
            for op in &inst.operands {
                if let Operand::IdRef(id) = op {
                    used.insert(*id);
                }
            }
        }

        let func = &mut ctx.module.functions[entry_idx];
        let mut removed_any = false;
        let mut freed_sources: Vec<Word> = Vec::new();
        for blk in &mut func.blocks {
            blk.instructions.retain(|inst| {
                let Some(rid) = inst.result_id else {
                    return true;
                };
                if is_pointer_derivation(inst) && candidates.contains(&rid) && !used.contains(&rid)
                {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            freed_sources.push(*id);
                        }
                    }
                    removed.insert(rid);
                    removed_any = true;
                    return false;
                }
                true
            });
        }
        if !removed_any {
            break;
        }
        candidates.extend(freed_sources);
    }
    if !removed.is_empty() {
        let references_removed = |instruction: &Instruction| {
            instruction
                .operands
                .iter()
                .any(|operand| matches!(operand, Operand::IdRef(id) if removed.contains(id)))
        };
        ctx.module
            .debug_names
            .retain(|instruction| !references_removed(instruction));
        ctx.module
            .annotations
            .retain(|instruction| !references_removed(instruction));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::ImageComp;
    use crate::spirv_module::{Block, Function, Module};
    use spirv::Dim;

    #[test]
    fn descriptor_materialization_drops_the_dead_derived_pointer_closure() {
        let descriptor_root = 1;
        let stale_projection = 10;
        let alias = 11;
        let unrelated = 12;
        let mut module = Module::new();
        module.debug_names.push(Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(stale_projection),
                Operand::LiteralString("old.texture.pointer".into()),
            ],
        ));
        module.functions.push(Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: None,
                instructions: vec![
                    Instruction::new(
                        Op::InBoundsAccessChain,
                        None,
                        Some(stale_projection),
                        vec![Operand::IdRef(descriptor_root), Operand::IdRef(2)],
                    ),
                    Instruction::new(
                        Op::CopyObject,
                        None,
                        Some(alias),
                        vec![Operand::IdRef(stale_projection)],
                    ),
                    Instruction::new(
                        Op::AccessChain,
                        None,
                        Some(unrelated),
                        vec![Operand::IdRef(3), Operand::IdRef(2)],
                    ),
                ],
            }],
        });
        let mut ctx = Ctx::new(module);

        retire_dead_pointer_projections(&mut ctx, 0, [descriptor_root]);

        let results = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .filter_map(|instruction| instruction.result_id)
            .collect::<HashSet<_>>();
        assert!(!results.contains(&stale_projection));
        assert!(!results.contains(&alias));
        assert!(results.contains(&unrelated));
        assert!(ctx.module.debug_names.is_empty());
    }

    #[test]
    fn descriptor_array_root_survives_insert_extract_wrapper() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim2D, false, ImageComp::Float);
        let root = ctx.module.fresh_id();
        ctx.image_array_vars.insert(
            root,
            (image_ty, (Dim::Dim2D, false), ImageComp::Float, false),
        );
        let alias = ctx.module.fresh_id();
        let poison = ctx.module.fresh_id();
        let aggregate = ctx.module.fresh_id();
        let extracted = ctx.module.fresh_id();
        let defs = crate::fxhash::map_from([
            (
                alias,
                Instruction::new(Op::Bitcast, None, Some(alias), vec![Operand::IdRef(root)]),
            ),
            (
                aggregate,
                Instruction::new(
                    Op::CompositeInsert,
                    None,
                    Some(aggregate),
                    vec![
                        Operand::IdRef(alias),
                        Operand::IdRef(poison),
                        Operand::LiteralBit32(0),
                    ],
                ),
            ),
            (
                extracted,
                Instruction::new(
                    Op::CompositeExtract,
                    None,
                    Some(extracted),
                    vec![Operand::IdRef(aggregate), Operand::LiteralBit32(0)],
                ),
            ),
        ]);

        assert_eq!(resolve_ptr_root(&ctx, &defs, extracted), Some(root));
    }

    #[test]
    fn local_pointer_table_becomes_descriptor_array_load_once() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim2D, false, ImageComp::Float);
        let array_ty = ctx.ty_array(image_ty, 2);
        let ptr_array = ctx.ty_ptr(StorageClass::UniformConstant, array_ty);
        let ptr_image = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
        let uint_ty = ctx.ty_uint();
        let private_byte = ctx.ty_ptr(StorageClass::Private, uint_ty);
        let root = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(ptr_array),
            Some(root),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        ctx.image_array_vars.insert(
            root,
            (image_ty, (Dim::Dim2D, false), ImageComp::Float, false),
        );

        let fixed0 = ctx.module.fresh_id();
        let source0 = ctx.module.fresh_id();
        let fixed1 = ctx.module.fresh_id();
        let source1 = ctx.module.fresh_id();
        let selector = ctx.const_uint(1);
        let table_root = ctx.module.fresh_id();
        let handle = ctx.module.fresh_id();
        let bool_ty = ctx.ty_bool();
        let condition = ctx.const_bool_of(bool_ty, false);
        let callee = ctx.module.fresh_id();
        let call_result = ctx.module.fresh_id();
        ctx.module.debug_names.push(Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(callee),
                Operand::LiteralString("air.get_width_texture_2d".into()),
            ],
        ));
        ctx.emit_sidecar.local_pointer_field_loads.extend([
            crate::emit_sidecar::LocalPointerFieldLoad {
                id: fixed0,
                root,
                indices: vec![0],
            },
            crate::emit_sidecar::LocalPointerFieldLoad {
                id: fixed1,
                root,
                indices: vec![1],
            },
        ]);
        ctx.emit_sidecar.local_pointer_field_stores.extend([
            crate::emit_sidecar::LocalPointerFieldStore {
                id: ctx.module.fresh_id(),
                source: source0,
                root: table_root,
                indices: vec![0, 0, 0],
            },
            crate::emit_sidecar::LocalPointerFieldStore {
                id: ctx.module.fresh_id(),
                source: source1,
                root: table_root,
                indices: vec![0, 1, 0],
            },
        ]);
        ctx.emit_sidecar.local_pointer_dynamic_field_loads.push(
            crate::emit_sidecar::LocalPointerDynamicFieldLoad {
                id: handle,
                root: table_root,
                prefix: vec![0],
                index: selector,
                suffix: vec![0],
            },
        );
        let function_id = ctx.module.fresh_id();
        let label = ctx.module.fresh_id();
        ctx.module.functions.push(Function {
            def: Some(Instruction::new(
                Op::Function,
                None,
                Some(function_id),
                vec![],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CopyObject,
                        Some(private_byte),
                        Some(fixed0),
                        vec![Operand::IdRef(source0)],
                    ),
                    Instruction::new(
                        Op::CopyObject,
                        Some(private_byte),
                        Some(fixed1),
                        vec![Operand::IdRef(source1)],
                    ),
                    Instruction::new(
                        Op::Select,
                        Some(private_byte),
                        Some(handle),
                        vec![
                            Operand::IdRef(condition),
                            Operand::IdRef(source1),
                            Operand::IdRef(source0),
                        ],
                    ),
                    Instruction::new(
                        Op::FunctionCall,
                        Some(uint_ty),
                        Some(call_result),
                        vec![Operand::IdRef(callee), Operand::IdRef(handle)],
                    ),
                ],
            }],
        });

        materialize_texture_array_loads(&mut ctx, 0);

        let body = &ctx.module.functions[0].blocks[0].instructions;
        let load = body
            .iter()
            .find(|inst| inst.result_id == Some(handle))
            .expect("materialized handle");
        assert_eq!(load.class.opcode, Op::Load);
        assert_eq!(load.result_type, Some(image_ty));
        assert!(body.iter().any(|inst| {
            inst.class.opcode == Op::AccessChain
                && inst.result_type == Some(ptr_image)
                && inst.operands.first() == Some(&Operand::IdRef(root))
        }));
        assert!(ctx
            .emit_sidecar
            .local_pointer_dynamic_field_loads
            .is_empty());
    }

    #[test]
    fn single_use_texture_array_load_sinks_out_of_loop_header() {
        let mut ctx = Ctx::new(Module::new());
        let image_ty = ctx.ty_image(Dim::Dim2D, false, ImageComp::Float);
        let array_ty = ctx.ty_array(image_ty, 4);
        let ptr_array = ctx.ty_ptr(StorageClass::UniformConstant, array_ty);
        let ptr_image = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
        let root = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(ptr_array),
            Some(root),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        ctx.image_array_vars.insert(
            root,
            (image_ty, (Dim::Dim2D, false), ImageComp::Float, false),
        );

        let index = ctx.const_uint(1);
        let lod = ctx.const_uint(0);
        let query_ty = ctx.ty_vec_uint(2);
        let chain = ctx.module.fresh_id();
        let image = ctx.module.fresh_id();
        let query = ctx.module.fresh_id();
        let header = ctx.module.fresh_id();
        let body = ctx.module.fresh_id();
        let continue_label = ctx.module.fresh_id();
        let merge = ctx.module.fresh_id();
        let function_id = ctx.module.fresh_id();
        ctx.module.functions.push(Function {
            def: Some(Instruction::new(
                Op::Function,
                None,
                Some(function_id),
                vec![],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(header), vec![])),
                    instructions: vec![
                        Instruction::new(
                            Op::AccessChain,
                            Some(ptr_image),
                            Some(chain),
                            vec![Operand::IdRef(root), Operand::IdRef(index)],
                        ),
                        Instruction::new(
                            Op::Load,
                            Some(image_ty),
                            Some(image),
                            vec![Operand::IdRef(chain)],
                        ),
                        Instruction::new(
                            Op::LoopMerge,
                            None,
                            None,
                            vec![Operand::IdRef(merge), Operand::IdRef(continue_label)],
                        ),
                        Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(body)]),
                    ],
                },
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(body), vec![])),
                    instructions: vec![
                        Instruction::new(
                            Op::ImageQuerySizeLod,
                            Some(query_ty),
                            Some(query),
                            vec![Operand::IdRef(image), Operand::IdRef(lod)],
                        ),
                        Instruction::new(
                            Op::Branch,
                            None,
                            None,
                            vec![Operand::IdRef(continue_label)],
                        ),
                    ],
                },
                Block {
                    label: Some(Instruction::new(
                        Op::Label,
                        None,
                        Some(continue_label),
                        vec![],
                    )),
                    instructions: vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(header)],
                    )],
                },
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(merge), vec![])),
                    instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
                },
            ],
        });

        sink_loop_header_texture_array_loads(&mut ctx, 0);

        let header_insts = &ctx.module.functions[0].blocks[0].instructions;
        assert!(!header_insts
            .iter()
            .any(|inst| matches!(inst.result_id, Some(id) if id == chain || id == image)));
        let body_insts = &ctx.module.functions[0].blocks[1].instructions;
        assert_eq!(body_insts[0].result_id, Some(chain));
        assert_eq!(body_insts[1].result_id, Some(image));
        assert_eq!(body_insts[2].result_id, Some(query));
    }
}
