use crate::spirv_module::load_bytes;

const FC_DEFINED_NAME_PREFIX: &str = "__metal2vulkan.MTL_FC_DEFINED_";

pub(crate) fn fc_init_index(name: &str) -> Option<u32> {
    let rest = name.split("MTL_FC_INIT_").nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().ok()
}

pub(crate) fn fc_defined_name(index: u32) -> String {
    format!("{FC_DEFINED_NAME_PREFIX}{index}")
}

fn fc_defined_index(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(FC_DEFINED_NAME_PREFIX)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse::<u32>().ok()
}

fn fc_defined_marker_indices(
    module: &crate::spirv_module::Module,
) -> crate::fxhash::HashMap<u32, u32> {
    use crate::spirv_module::Operand;
    use spirv::Op;

    let mut out = crate::fxhash::HashMap::default();
    for inst in &module.debug_names {
        if inst.class.opcode != Op::Name {
            continue;
        }
        if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(name))) =
            (inst.operands.first(), inst.operands.get(1))
        {
            if let Some(index) = fc_defined_index(name) {
                out.insert(*id, index);
            }
        }
    }
    out
}

fn fc_init_indices(module: &crate::spirv_module::Module) -> crate::fxhash::HashSet<u32> {
    use crate::spirv_module::Operand;
    use spirv::Op;

    let mut out = crate::fxhash::HashSet::default();
    for inst in &module.debug_names {
        if inst.class.opcode != Op::Name {
            continue;
        }
        if let Some(Operand::LiteralString(name)) = inst.operands.get(1) {
            if let Some(index) = fc_init_index(name) {
                out.insert(index);
            }
        }
    }
    out
}

fn bool_constant_for(
    module: &mut crate::spirv_module::Module,
    bool_ty: u32,
    value: bool,
    const_for: &mut crate::fxhash::HashMap<(u32, bool), u32>,
) -> u32 {
    use crate::spirv_module::Instruction;
    use spirv::Op;

    if let Some(&id) = const_for.get(&(bool_ty, value)) {
        return id;
    }
    let op = if value {
        Op::ConstantTrue
    } else {
        Op::ConstantFalse
    };
    if let Some(id) = module
        .types_global_values
        .iter()
        .find(|inst| inst.class.opcode == op && inst.result_type == Some(bool_ty))
        .and_then(|inst| inst.result_id)
    {
        const_for.insert((bool_ty, value), id);
        return id;
    }
    let id = module.fresh_id();
    module
        .types_global_values
        .push(Instruction::new(op, Some(bool_ty), Some(id), vec![]));
    const_for.insert((bool_ty, value), id);
    id
}

fn specialize_fc_definedness(
    module: &mut crate::spirv_module::Module,
    defined_indices: &crate::fxhash::HashSet<u32>,
) -> bool {
    use crate::spirv_module::Operand;
    use spirv::Op;

    let marker_indices = fc_defined_marker_indices(module);
    if marker_indices.is_empty() {
        return false;
    }

    let mut edits = Vec::new();
    for (function_idx, function) in module.functions.iter().enumerate() {
        for (block_idx, block) in function.blocks.iter().enumerate() {
            for (inst_idx, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::CopyObject {
                    continue;
                }
                let Some(result_id) = inst.result_id else {
                    continue;
                };
                let Some(&fc_index) = marker_indices.get(&result_id) else {
                    continue;
                };
                let Some(bool_ty) = inst.result_type else {
                    continue;
                };
                edits.push((
                    function_idx,
                    block_idx,
                    inst_idx,
                    bool_ty,
                    defined_indices.contains(&fc_index),
                ));
            }
        }
    }

    if edits.is_empty() {
        return false;
    }

    let mut const_for = crate::fxhash::HashMap::default();
    let mut changed = false;
    for (function_idx, block_idx, inst_idx, bool_ty, value) in edits {
        let const_id = bool_constant_for(module, bool_ty, value, &mut const_for);
        let inst = &mut module.functions[function_idx].blocks[block_idx].instructions[inst_idx];
        if inst.operands.first() != Some(&Operand::IdRef(const_id)) {
            inst.operands = vec![Operand::IdRef(const_id)];
            changed = true;
        }
    }
    changed
}

fn materialize_zero_fc_initializers(module: &mut crate::spirv_module::Module) -> bool {
    use crate::spirv_module::{Instruction, Operand};
    use spirv::Op;

    let mut fc_init_vars: crate::fxhash::HashSet<u32> = crate::fxhash::HashSet::default();
    for inst in &module.debug_names {
        if inst.class.opcode != Op::Name {
            continue;
        }
        if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(name))) =
            (inst.operands.first(), inst.operands.get(1))
        {
            if fc_init_index(name).is_some() {
                fc_init_vars.insert(*id);
            }
        }
    }

    let mut pointee: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    let mut int_types: crate::fxhash::HashSet<u32> = crate::fxhash::HashSet::default();
    let mut int_vectors: crate::fxhash::HashMap<u32, (u32, u32)> =
        crate::fxhash::HashMap::default();
    for inst in &module.types_global_values {
        match inst.class.opcode {
            Op::TypePointer => {
                if let (Some(id), Some(Operand::IdRef(pointee_id))) =
                    (inst.result_id, inst.operands.get(1))
                {
                    pointee.insert(id, *pointee_id);
                }
            }
            Op::TypeInt => {
                if let Some(id) = inst.result_id {
                    int_types.insert(id);
                }
            }
            Op::TypeVector => {
                if let (
                    Some(id),
                    Some(Operand::IdRef(component_ty)),
                    Some(Operand::LiteralBit32(count)),
                ) = (inst.result_id, inst.operands.first(), inst.operands.get(1))
                {
                    int_vectors.insert(id, (*component_ty, *count));
                }
            }
            _ => {}
        }
    }
    int_vectors.retain(|_, (component_ty, _)| int_types.contains(component_ty));

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(1);
    let mut scalar_zero_for: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    let mut vector_zero_for: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    let mut new_consts: crate::fxhash::HashMap<u32, Instruction> =
        crate::fxhash::HashMap::default();
    let mut edits: Vec<(usize, Vec<u32>, u32)> = vec![];

    for (vi, inst) in module.types_global_values.iter().enumerate() {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let Some(var_id) = inst.result_id else {
            continue;
        };
        if !fc_init_vars.contains(&var_id) {
            continue;
        }
        let Some(ptr_ty) = inst.result_type else {
            continue;
        };
        let Some(&value_ty) = pointee.get(&ptr_ty) else {
            continue;
        };

        if int_types.contains(&value_ty) {
            let const_id = *scalar_zero_for.entry(value_ty).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                new_consts.insert(
                    id,
                    Instruction::new(
                        Op::Constant,
                        Some(value_ty),
                        Some(id),
                        vec![Operand::LiteralBit32(0)],
                    ),
                );
                id
            });
            edits.push((vi, vec![const_id], const_id));
            continue;
        }

        if let Some(&(component_ty, count)) = int_vectors.get(&value_ty) {
            let scalar_id = *scalar_zero_for.entry(component_ty).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                new_consts.insert(
                    id,
                    Instruction::new(
                        Op::Constant,
                        Some(component_ty),
                        Some(id),
                        vec![Operand::LiteralBit32(0)],
                    ),
                );
                id
            });
            let composite_id = *vector_zero_for.entry(value_ty).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                new_consts.insert(
                    id,
                    Instruction::new(
                        Op::ConstantComposite,
                        Some(value_ty),
                        Some(id),
                        (0..count).map(|_| Operand::IdRef(scalar_id)).collect(),
                    ),
                );
                id
            });
            edits.push((vi, vec![scalar_id, composite_id], composite_id));
        }
    }

    if edits.is_empty() {
        return false;
    }

    let var_to_consts: crate::fxhash::HashMap<u32, (Vec<u32>, u32)> = edits
        .iter()
        .filter_map(|(vi, consts, initializer)| {
            module.types_global_values[*vi]
                .result_id
                .map(|var_id| (var_id, (consts.clone(), *initializer)))
        })
        .collect();
    let mut rebuilt = Vec::with_capacity(module.types_global_values.len() + new_consts.len());
    let mut emitted = crate::fxhash::HashSet::default();
    for mut inst in module.types_global_values.clone() {
        if inst.class.opcode == Op::Variable {
            if let Some((consts, initializer)) =
                inst.result_id.as_ref().and_then(|id| var_to_consts.get(id))
            {
                for const_id in consts {
                    if emitted.insert(*const_id) {
                        if let Some(new_const) = new_consts.remove(const_id) {
                            rebuilt.push(new_const);
                        }
                    }
                }
                if inst.operands.len() >= 2 {
                    inst.operands[1] = Operand::IdRef(*initializer);
                } else {
                    inst.operands.push(Operand::IdRef(*initializer));
                }
            }
        }
        rebuilt.push(inst);
    }
    module.types_global_values = rebuilt;
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    true
}

fn fc_lanes_from_le_bytes(bytes: &[u8], width: u32, count: u32) -> Vec<u64> {
    let width_bytes = (width as usize).div_ceil(8).min(8);
    (0..count)
        .map(|lane| {
            let start = lane as usize * width_bytes;
            if start >= bytes.len() {
                return 0;
            }
            let available = (bytes.len() - start).min(width_bytes);
            let mut lane = [0u8; 8];
            lane[..available].copy_from_slice(&bytes[start..start + available]);
            u64::from_le_bytes(lane)
        })
        .collect()
}

fn fc_scalar_from_le_bytes(bytes: &[u8], width: u32) -> u64 {
    fc_lanes_from_le_bytes(bytes, width, 1)[0]
}

fn int_constant_id(
    scalar_ty: u32,
    width: u32,
    value: u64,
    next_id: &mut u32,
    const_for: &mut crate::fxhash::HashMap<(u32, u64), u32>,
    new_consts: &mut crate::fxhash::HashMap<u32, crate::spirv_module::Instruction>,
) -> u32 {
    use crate::spirv_module::{Instruction, Operand};
    use spirv::Op;

    *const_for.entry((scalar_ty, value)).or_insert_with(|| {
        let id = *next_id;
        *next_id += 1;
        let operand = if width > 32 {
            Operand::LiteralBit64(value)
        } else {
            Operand::LiteralBit32(value as u32)
        };
        new_consts.insert(
            id,
            Instruction::new(Op::Constant, Some(scalar_ty), Some(id), vec![operand]),
        );
        id
    })
}

pub fn specialize_function_constants_zero(spv: &[u8]) -> Result<Vec<u8>, String> {
    let mut module = load_bytes(spv).map_err(|e| format!("SPIR-V load: {e:?}"))?;
    let mut defined_indices = fc_init_indices(&module);
    defined_indices.extend(fc_defined_marker_indices(&module).values().copied());
    let materialized = materialize_zero_fc_initializers(&mut module);
    let definedness = specialize_fc_definedness(&mut module, &defined_indices);
    if !materialized && !definedness {
        let (specialized, pruned) = prune_static_fc_branches_and_rebuild_interface(module);
        if pruned {
            return Ok(specialized
                .assemble()
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect());
        }
        return Ok(spv.to_vec());
    }
    (module, _) = prune_static_fc_branches_and_rebuild_interface(module);
    Ok(module
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect())
}

pub fn specialize_function_constants(spv: &[u8], values: &[(u32, u64)]) -> Result<Vec<u8>, String> {
    let bytes = values
        .iter()
        .map(|(index, value)| (*index, value.to_le_bytes().to_vec()))
        .collect::<Vec<_>>();
    specialize_function_constant_bytes_impl(spv, &bytes, false)
}

pub fn specialize_function_constant_bytes(
    spv: &[u8],
    values: &[(u32, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    specialize_function_constant_bytes_impl(spv, values, true)
}

fn specialize_function_constant_bytes_impl(
    spv: &[u8],
    values: &[(u32, Vec<u8>)],
    exact_payload_size: bool,
) -> Result<Vec<u8>, String> {
    use crate::spirv_module::Instruction;
    use crate::spirv_module::Operand;
    use spirv::Op;
    if values.is_empty() {
        return Ok(spv.to_vec());
    }
    let want: crate::fxhash::HashMap<u32, &[u8]> = values
        .iter()
        .map(|(index, bytes)| (*index, bytes.as_slice()))
        .collect();
    let mut module = load_bytes(spv).map_err(|e| format!("SPIR-V load: {e:?}"))?;

    let mut var_index: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    for inst in &module.debug_names {
        if inst.class.opcode != Op::Name {
            continue;
        }
        if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(s))) =
            (inst.operands.first(), inst.operands.get(1))
        {
            if let Some(idx) = fc_init_index(s) {
                var_index.insert(*id, idx);
            }
        }
    }

    let mut pointee: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    let mut scalar_width: crate::fxhash::HashMap<u32, u32> = crate::fxhash::HashMap::default();
    let mut numeric_vectors: crate::fxhash::HashMap<u32, (u32, u32)> =
        crate::fxhash::HashMap::default();
    for inst in &module.types_global_values {
        match inst.class.opcode {
            Op::TypePointer => {
                if let (Some(rid), Some(Operand::IdRef(p))) = (inst.result_id, inst.operands.get(1))
                {
                    pointee.insert(rid, *p);
                }
            }
            Op::TypeInt | Op::TypeFloat => {
                if let (Some(rid), Some(Operand::LiteralBit32(w))) =
                    (inst.result_id, inst.operands.first())
                {
                    scalar_width.insert(rid, *w);
                }
            }
            Op::TypeVector => {
                if let (
                    Some(rid),
                    Some(Operand::IdRef(component_ty)),
                    Some(Operand::LiteralBit32(count)),
                ) = (inst.result_id, inst.operands.first(), inst.operands.get(1))
                {
                    numeric_vectors.insert(rid, (*component_ty, *count));
                }
            }
            _ => {}
        }
    }
    numeric_vectors.retain(|_, (component_ty, _)| scalar_width.contains_key(component_ty));

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(1);
    let mut const_for: crate::fxhash::HashMap<(u32, u64), u32> = crate::fxhash::HashMap::default();
    let mut composite_for: crate::fxhash::HashMap<(u32, Vec<u64>), u32> =
        crate::fxhash::HashMap::default();
    let mut new_consts: crate::fxhash::HashMap<u32, Instruction> =
        crate::fxhash::HashMap::default();
    let mut edits: Vec<(usize, Vec<u32>, u32)> = vec![];
    for (vi, inst) in module.types_global_values.iter().enumerate() {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let Some(vid) = inst.result_id else { continue };
        let Some(&idx) = var_index.get(&vid) else {
            continue;
        };
        let Some(&bytes) = want.get(&idx) else {
            continue;
        };
        let ptr_ty = inst
            .result_type
            .ok_or_else(|| format!("FC var %{vid} has no result type"))?;
        let scalar_ty = *pointee
            .get(&ptr_ty)
            .ok_or_else(|| format!("FC var %{vid}: pointer type %{ptr_ty} has no pointee"))?;
        if let Some(&width) = scalar_width.get(&scalar_ty) {
            let required = (width as usize).div_ceil(8);
            if exact_payload_size && bytes.len() != required {
                return Err(format!(
                    "FC index {idx}: payload has {} bytes, scalar type requires {required}",
                    bytes.len()
                ));
            }
            let val = fc_scalar_from_le_bytes(bytes, width);
            let cid = int_constant_id(
                scalar_ty,
                width,
                val,
                &mut next_id,
                &mut const_for,
                &mut new_consts,
            );
            edits.push((vi, vec![cid], cid));
            continue;
        }
        let Some(&(component_ty, count)) = numeric_vectors.get(&scalar_ty) else {
            return Err(format!(
                "FC index {idx}: pointee %{scalar_ty} is not a scalar or vector numeric type"
            ));
        };
        let width = scalar_width[&component_ty];
        let required = (width as usize).div_ceil(8) * count as usize;
        if exact_payload_size && bytes.len() != required {
            return Err(format!(
                "FC index {idx}: payload has {} bytes, vector type requires {required}",
                bytes.len()
            ));
        }
        let lanes = fc_lanes_from_le_bytes(bytes, width, count);
        let mut const_ids = Vec::with_capacity(lanes.len() + 1);
        for lane in &lanes {
            const_ids.push(int_constant_id(
                component_ty,
                width,
                *lane,
                &mut next_id,
                &mut const_for,
                &mut new_consts,
            ));
        }
        let composite_id = *composite_for
            .entry((scalar_ty, lanes.clone()))
            .or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                new_consts.insert(
                    id,
                    Instruction::new(
                        Op::ConstantComposite,
                        Some(scalar_ty),
                        Some(id),
                        const_ids.iter().copied().map(Operand::IdRef).collect(),
                    ),
                );
                id
            });
        const_ids.push(composite_id);
        edits.push((vi, const_ids, composite_id));
    }

    let requested: crate::fxhash::HashSet<u32> = want.keys().copied().collect();
    let mut applied: crate::fxhash::HashSet<u32> = edits
        .iter()
        .filter_map(|(vi, _, _)| module.types_global_values[*vi].result_id)
        .filter_map(|vid| var_index.get(&vid).copied())
        .collect();
    applied.extend(fc_defined_marker_indices(&module).values().copied());
    let missing: Vec<u32> = requested
        .difference(&applied)
        .copied()
        .filter(|index| {
            want.get(index)
                .is_some_and(|bytes| bytes.iter().any(|byte| *byte != 0))
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "specialize_function_constants: no MTL_FC_INIT global for FC index(es) {missing:?} \
             (module has indices {:?})",
            var_index.values().collect::<crate::fxhash::HashSet<_>>()
        ));
    }

    let var_to_consts: crate::fxhash::HashMap<u32, (Vec<u32>, u32)> = edits
        .iter()
        .filter_map(|(vi, consts, initializer)| {
            module.types_global_values[*vi]
                .result_id
                .map(|v| (v, (consts.clone(), *initializer)))
        })
        .collect();

    let mut rebuilt: Vec<Instruction> = Vec::with_capacity(module.types_global_values.len() + 1);
    let mut emitted: crate::fxhash::HashSet<u32> = crate::fxhash::HashSet::default();
    for mut inst in module.types_global_values.clone() {
        if inst.class.opcode == Op::Variable {
            if let Some((consts, initializer)) =
                inst.result_id.as_ref().and_then(|v| var_to_consts.get(v))
            {
                for const_id in consts {
                    if emitted.insert(*const_id) {
                        if let Some(c) = new_consts.remove(const_id) {
                            rebuilt.push(c);
                        }
                    }
                }
                if inst.operands.len() >= 2 {
                    inst.operands[1] = Operand::IdRef(*initializer);
                } else {
                    inst.operands.push(Operand::IdRef(*initializer));
                }
            }
        }
        rebuilt.push(inst);
    }
    module.types_global_values = rebuilt;
    if let Some(h) = module.header.as_mut() {
        h.bound = next_id;
    }
    let defined_indices = requested;
    specialize_fc_definedness(&mut module, &defined_indices);

    (module, _) = prune_static_fc_branches_and_rebuild_interface(module);
    rewrite_thread_local_scalar_byte_subslot_stores(&mut module);

    Ok(module
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect())
}

fn prune_static_fc_branches_and_rebuild_interface(
    mut module: crate::spirv_module::Module,
) -> (crate::spirv_module::Module, bool) {
    if module.entry_points.is_empty() {
        return (module, false);
    }
    let before_prune = module.clone();
    if crate::native::prune_constant_branches_module(&mut module).is_err() {
        return (module, false);
    }
    restore_loop_merges_removed_by_fc_prune(&before_prune, &mut module);
    drop_unreferenced_entry_interface_globals(&mut module);
    module = crate::passes::lower_specialized_workgroup_ptr_access_chains(module);
    (module, true)
}

fn rewrite_thread_local_scalar_byte_subslot_stores(
    module: &mut crate::spirv_module::Module,
) -> bool {
    use crate::spirv_module::{Instruction, Operand};
    use spirv::{Op, StorageClass, Word};

    let mut type_defs: crate::fxhash::HashMap<Word, Instruction> =
        crate::fxhash::HashMap::default();
    let mut ptr_info: crate::fxhash::HashMap<Word, (StorageClass, Word)> =
        crate::fxhash::HashMap::default();
    let mut result_type: crate::fxhash::HashMap<Word, Word> = crate::fxhash::HashMap::default();
    for inst in module.all_inst_iter() {
        if let Some(id) = inst.result_id {
            if let Some(ty) = inst.result_type {
                result_type.insert(id, ty);
            }
            if matches!(
                inst.class.opcode,
                Op::TypeInt | Op::TypeFloat | Op::TypePointer | Op::TypeVector
            ) {
                type_defs.insert(id, inst.clone());
            }
            if inst.class.opcode == Op::TypePointer {
                if let (Some(Operand::StorageClass(sc)), Some(Operand::IdRef(pointee))) =
                    (inst.operands.first(), inst.operands.get(1))
                {
                    ptr_info.insert(id, (*sc, *pointee));
                }
            }
        }
    }

    let scalar_width = |ty: Word| -> Option<u32> {
        let def = type_defs.get(&ty)?;
        match def.class.opcode {
            Op::TypeInt | Op::TypeFloat => match def.operands.first()? {
                Operand::LiteralBit32(width) => Some(*width),
                _ => None,
            },
            _ => None,
        }
    };
    let is_unsigned_int = |ty: Word, width: u32| -> bool {
        matches!(
            type_defs.get(&ty).map(|i| (i.class.opcode, i.operands.as_slice())),
            Some((
                Op::TypeInt,
                [Operand::LiteralBit32(w), Operand::LiteralBit32(0)]
            )) if *w == width
        )
    };
    let const_u32 = |id: Word| -> Option<u32> {
        module.types_global_values.iter().find_map(|inst| {
            if inst.class.opcode == Op::Constant && inst.result_id == Some(id) {
                match inst.operands.first()? {
                    Operand::LiteralBit32(v) => Some(*v),
                    Operand::LiteralBit64(v) => u32::try_from(*v).ok(),
                    _ => None,
                }
            } else {
                None
            }
        })
    };

    #[derive(Clone, Copy)]
    struct Plan {
        chain_fi: usize,
        chain_bi: usize,
        chain_ii: usize,
        store_fi: usize,
        store_bi: usize,
        store_ii: usize,
        base: Word,
        base_ty: Word,
        base_bits: u32,
        object: Word,
        object_ty: Word,
        object_bits: u32,
        offset_bits: u32,
    }

    let mut chain_base: crate::fxhash::HashMap<Word, (usize, usize, usize, Word, Word, u32, u32)> =
        crate::fxhash::HashMap::default();
    for (fi, function) in module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::PtrAccessChain || inst.operands.len() != 2 {
                    continue;
                }
                let (Some(chain), Some(result_ptr_ty)) = (inst.result_id, inst.result_type) else {
                    continue;
                };
                let Some(&(sc, result_pointee)) = ptr_info.get(&result_ptr_ty) else {
                    continue;
                };
                if !matches!(sc, StorageClass::Function | StorageClass::Private)
                    || scalar_width(result_pointee) != Some(8)
                {
                    continue;
                }
                let (Some(Operand::IdRef(base)), Some(Operand::IdRef(offset))) =
                    (inst.operands.first(), inst.operands.get(1))
                else {
                    continue;
                };
                let Some(offset_bytes) = const_u32(*offset) else {
                    continue;
                };
                let Some(base_ptr_ty) = result_type.get(base).copied() else {
                    continue;
                };
                let Some(&(base_sc, base_ty)) = ptr_info.get(&base_ptr_ty) else {
                    continue;
                };
                if base_sc != sc {
                    continue;
                }
                let Some(base_bits) = scalar_width(base_ty) else {
                    continue;
                };
                chain_base.insert(
                    chain,
                    (fi, bi, ii, *base, base_ty, base_bits, offset_bytes * 8),
                );
            }
        }
    }
    if chain_base.is_empty() {
        return false;
    }

    let mut uses: crate::fxhash::HashMap<Word, Vec<(usize, usize, usize)>> =
        crate::fxhash::HashMap::default();
    let mut disqualified = crate::fxhash::HashSet::default();
    for (fi, function) in module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst
                    .result_id
                    .is_some_and(|id| chain_base.contains_key(&id))
                    && inst.class.opcode == Op::PtrAccessChain
                {
                    continue;
                }
                for (oi, op) in inst.operands.iter().enumerate() {
                    let Operand::IdRef(id) = op else { continue };
                    if !chain_base.contains_key(id) {
                        continue;
                    }
                    if inst.class.opcode == Op::Store && oi == 0 {
                        uses.entry(*id).or_default().push((fi, bi, ii));
                    } else {
                        disqualified.insert(*id);
                    }
                }
            }
        }
    }

    let mut plans = Vec::new();
    for (chain, (chain_fi, chain_bi, chain_ii, base, base_ty, base_bits, offset_bits)) in chain_base
    {
        if disqualified.contains(&chain) {
            continue;
        }
        let Some(sites) = uses.get(&chain) else {
            continue;
        };
        for &(store_fi, store_bi, store_ii) in sites {
            let store = &module.functions[store_fi].blocks[store_bi].instructions[store_ii];
            let Some(Operand::IdRef(object)) = store.operands.get(1) else {
                continue;
            };
            let Some(object_ty) = result_type.get(object).copied() else {
                continue;
            };
            let Some(object_bits) = scalar_width(object_ty) else {
                continue;
            };
            if offset_bits + object_bits > base_bits {
                continue;
            }
            plans.push(Plan {
                chain_fi,
                chain_bi,
                chain_ii,
                store_fi,
                store_bi,
                store_ii,
                base,
                base_ty,
                base_bits,
                object: *object,
                object_ty,
                object_bits,
                offset_bits,
            });
        }
    }
    if plans.is_empty() {
        return false;
    }

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(1);
    let mut unsigned_int_for_width: crate::fxhash::HashMap<u32, Word> = type_defs
        .iter()
        .filter_map(|(&id, inst)| {
            if inst.class.opcode == Op::TypeInt {
                if let (Some(Operand::LiteralBit32(width)), Some(Operand::LiteralBit32(0))) =
                    (inst.operands.first(), inst.operands.get(1))
                {
                    return Some((*width, id));
                }
            }
            None
        })
        .collect();
    let mut const_for: crate::fxhash::HashMap<(Word, u64), Word> =
        crate::fxhash::HashMap::default();
    for inst in &module.types_global_values {
        if inst.class.opcode != Op::Constant {
            continue;
        }
        let (Some(ty), Some(id)) = (inst.result_type, inst.result_id) else {
            continue;
        };
        let value = match inst.operands.first() {
            Some(Operand::LiteralBit32(v)) => u64::from(*v),
            Some(Operand::LiteralBit64(v)) => *v,
            _ => continue,
        };
        const_for.insert((ty, value), id);
    }
    let mut new_defs = crate::fxhash::HashMap::default();

    for plan in &plans {
        for width in [plan.base_bits, plan.object_bits] {
            unsigned_int_for_width.entry(width).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                module.types_global_values.push(Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(id),
                    vec![Operand::LiteralBit32(width), Operand::LiteralBit32(0)],
                ));
                id
            });
        }
    }

    let mut chain_sites: crate::fxhash::HashSet<(usize, usize, usize)> =
        crate::fxhash::HashSet::default();
    let mut store_plan: crate::fxhash::HashMap<(usize, usize, usize), Plan> =
        crate::fxhash::HashMap::default();
    for plan in plans {
        chain_sites.insert((plan.chain_fi, plan.chain_bi, plan.chain_ii));
        store_plan.insert((plan.store_fi, plan.store_bi, plan.store_ii), plan);
    }

    let mut changed = false;
    for (fi, function) in module.functions.iter_mut().enumerate() {
        for (bi, block) in function.blocks.iter_mut().enumerate() {
            let old = block.instructions.clone();
            let mut out = Vec::with_capacity(old.len() + 16);
            for (ii, inst) in old.into_iter().enumerate() {
                if chain_sites.contains(&(fi, bi, ii)) {
                    changed = true;
                    continue;
                }
                let Some(plan) = store_plan.get(&(fi, bi, ii)).copied() else {
                    out.push(inst);
                    continue;
                };
                let base_uint_ty = unsigned_int_for_width[&plan.base_bits];
                let object_uint_ty = unsigned_int_for_width[&plan.object_bits];
                let whole = next_id;
                next_id += 1;
                out.push(Instruction::new(
                    Op::Load,
                    Some(plan.base_ty),
                    Some(whole),
                    vec![Operand::IdRef(plan.base)],
                ));
                let whole_u = if is_unsigned_int(plan.base_ty, plan.base_bits) {
                    whole
                } else {
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(base_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(whole)],
                    ));
                    id
                };
                let low_mask = if plan.object_bits == 64 {
                    u64::MAX
                } else {
                    (1u64 << plan.object_bits) - 1
                };
                let low_mask = int_constant_id(
                    base_uint_ty,
                    plan.base_bits,
                    low_mask,
                    &mut next_id,
                    &mut const_for,
                    &mut new_defs,
                );
                let mask = if plan.offset_bits == 0 {
                    low_mask
                } else {
                    let shift = int_constant_id(
                        base_uint_ty,
                        plan.base_bits,
                        u64::from(plan.offset_bits),
                        &mut next_id,
                        &mut const_for,
                        &mut new_defs,
                    );
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::ShiftLeftLogical,
                        Some(base_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(low_mask), Operand::IdRef(shift)],
                    ));
                    id
                };
                let not_mask = next_id;
                next_id += 1;
                out.push(Instruction::new(
                    Op::Not,
                    Some(base_uint_ty),
                    Some(not_mask),
                    vec![Operand::IdRef(mask)],
                ));
                let keep = next_id;
                next_id += 1;
                out.push(Instruction::new(
                    Op::BitwiseAnd,
                    Some(base_uint_ty),
                    Some(keep),
                    vec![Operand::IdRef(whole_u), Operand::IdRef(not_mask)],
                ));
                let object_u = if is_unsigned_int(plan.object_ty, plan.object_bits) {
                    plan.object
                } else {
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(object_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(plan.object)],
                    ));
                    id
                };
                let object_wide = if plan.object_bits == plan.base_bits {
                    object_u
                } else {
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::UConvert,
                        Some(base_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(object_u)],
                    ));
                    id
                };
                let object_shifted = if plan.offset_bits == 0 {
                    object_wide
                } else {
                    let shift = int_constant_id(
                        base_uint_ty,
                        plan.base_bits,
                        u64::from(plan.offset_bits),
                        &mut next_id,
                        &mut const_for,
                        &mut new_defs,
                    );
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::ShiftLeftLogical,
                        Some(base_uint_ty),
                        Some(id),
                        vec![Operand::IdRef(object_wide), Operand::IdRef(shift)],
                    ));
                    id
                };
                let combined = next_id;
                next_id += 1;
                out.push(Instruction::new(
                    Op::BitwiseOr,
                    Some(base_uint_ty),
                    Some(combined),
                    vec![Operand::IdRef(keep), Operand::IdRef(object_shifted)],
                ));
                let stored = if is_unsigned_int(plan.base_ty, plan.base_bits) {
                    combined
                } else {
                    let id = next_id;
                    next_id += 1;
                    out.push(Instruction::new(
                        Op::Bitcast,
                        Some(plan.base_ty),
                        Some(id),
                        vec![Operand::IdRef(combined)],
                    ));
                    id
                };
                let mut operands = vec![Operand::IdRef(plan.base), Operand::IdRef(stored)];
                operands.extend(inst.operands.iter().skip(2).cloned());
                out.push(Instruction::new(Op::Store, None, None, operands));
                changed = true;
            }
            block.instructions = out;
        }
    }
    if !new_defs.is_empty() {
        let mut ids = new_defs.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        for id in ids {
            if let Some(inst) = new_defs.remove(&id) {
                module.types_global_values.push(inst);
            }
        }
    }
    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    changed
}

fn restore_loop_merges_removed_by_fc_prune(
    before: &crate::spirv_module::Module,
    after: &mut crate::spirv_module::Module,
) {
    use crate::fxhash::{HashMap, HashSet};
    use crate::spirv_module::{Block, Function, Instruction, Operand};
    use spirv::Op;

    fn function_id(function: &Function) -> Option<u32> {
        function.def.as_ref().and_then(|def| def.result_id)
    }

    fn block_id(block: &Block) -> Option<u32> {
        block.label.as_ref().and_then(|label| label.result_id)
    }

    fn loop_merge(block: &Block) -> Option<&Instruction> {
        let n = block.instructions.len();
        if n >= 2 && block.instructions[n - 2].class.opcode == Op::LoopMerge {
            Some(&block.instructions[n - 2])
        } else {
            None
        }
    }

    let before_functions = before
        .functions
        .iter()
        .filter_map(|function| function_id(function).map(|id| (id, function)))
        .collect::<HashMap<_, _>>();

    for function in &mut after.functions {
        let Some(fid) = function_id(function) else {
            continue;
        };
        let Some(before_function) = before_functions.get(&fid) else {
            continue;
        };
        let before_blocks = before_function
            .blocks
            .iter()
            .filter_map(|block| block_id(block).map(|id| (id, block)))
            .collect::<HashMap<_, _>>();
        let mut alive = function
            .blocks
            .iter()
            .filter_map(block_id)
            .collect::<HashSet<_>>();
        let mut synthesized_merges = HashSet::default();
        for block in &mut function.blocks {
            if loop_merge(block).is_some() {
                continue;
            }
            let Some(id) = block_id(block) else { continue };
            let Some(before_block) = before_blocks.get(&id) else {
                continue;
            };
            let Some(original_merge) = loop_merge(before_block).cloned() else {
                continue;
            };
            let (Some(Operand::IdRef(merge)), Some(Operand::IdRef(cont))) = (
                original_merge.operands.first(),
                original_merge.operands.get(1),
            ) else {
                continue;
            };
            let merge = *merge;
            let cont = *cont;
            if !alive.contains(&cont) {
                continue;
            }
            if block
                .instructions
                .iter()
                .any(|inst| inst.class.opcode == Op::SelectionMerge)
            {
                continue;
            }
            let insert_at = block.instructions.len().saturating_sub(1);
            block.instructions.insert(insert_at, original_merge);
            if !alive.contains(&merge) && synthesized_merges.insert(merge) {
                alive.insert(merge);
            }
        }
        if synthesized_merges.is_empty() {
            continue;
        }
        let mut new_blocks = synthesized_merges.into_iter().collect::<Vec<_>>();
        new_blocks.sort_unstable();
        for merge in new_blocks {
            function.blocks.push(Block {
                label: Some(Instruction::new(Op::Label, None, Some(merge), vec![])),
                instructions: vec![Instruction::new(Op::Unreachable, None, None, vec![])],
            });
        }
    }
}

fn drop_unreferenced_entry_interface_globals(module: &mut crate::spirv_module::Module) {
    crate::passes::drop_unreferenced_global_variables(module);
    crate::passes::drop_dangling_debug(module);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, Instruction, Module, ModuleHeader, Operand};
    use spirv::{
        AddressingModel, Capability, ExecutionModel, FunctionControl, MemoryModel, Op,
        SelectionControl, StorageClass,
    };

    fn fixture_bytes(bound: u32, globals: Vec<Instruction>, names: Vec<Instruction>) -> Vec<u8> {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(bound);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.types_global_values = globals;
        module.debug_names = names;
        module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    fn name(id: u32, value: &str) -> Instruction {
        Instruction::new(
            Op::Name,
            None,
            None,
            vec![Operand::IdRef(id), Operand::from(value)],
        )
    }

    fn block(label: u32, instructions: Vec<Instruction>) -> Block {
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions,
        }
    }

    fn definedness_fixture_bytes() -> Vec<u8> {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(21);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.types_global_values = vec![
            Instruction::new(Op::TypeBool, None, Some(1), vec![]),
            Instruction::new(Op::ConstantFalse, Some(1), Some(2), vec![]),
            Instruction::new(Op::TypeVoid, None, Some(3), vec![]),
            Instruction::new(Op::TypeFunction, None, Some(4), vec![Operand::IdRef(3)]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(5),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(Op::ConstantNull, Some(5), Some(7), vec![]),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(7),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(7),
                ],
            ),
        ];
        module.debug_names = vec![
            name(8, "_Z1x.MTL_FC_INIT_3_b"),
            name(9, "_Z1y.MTL_FC_INIT_5_b"),
            name(11, &fc_defined_name(3)),
            name(12, &fc_defined_name(5)),
        ];

        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(3),
            Some(10),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(4),
            ],
        ));
        function.blocks = vec![block(
            20,
            vec![
                Instruction::new(Op::CopyObject, Some(1), Some(11), vec![Operand::IdRef(2)]),
                Instruction::new(Op::CopyObject, Some(1), Some(12), vec![Operand::IdRef(2)]),
                Instruction::new(Op::Return, None, None, vec![]),
            ],
        )];
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        module.functions.push(function);

        module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }

    fn copy_object_operand_const_opcode(module: &Module, result_id: u32) -> Op {
        let const_id = module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
            .find(|inst| inst.class.opcode == Op::CopyObject && inst.result_id == Some(result_id))
            .and_then(|inst| inst.operands.first())
            .and_then(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .expect("copy object constant operand");
        module
            .types_global_values
            .iter()
            .find(|inst| inst.result_id == Some(const_id))
            .map(|inst| inst.class.opcode)
            .expect("constant definition")
    }

    #[test]
    fn fc_prune_restores_loop_merge_when_merge_block_was_pruned() {
        let header = 10;
        let body = 11;
        let cont = 12;
        let merge = 13;
        let mut before = Module::new();
        before.functions.push(Function {
            def: Some(Instruction::new(Op::Function, Some(1), Some(50), vec![])),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![
                block(
                    header,
                    vec![
                        Instruction::new(
                            Op::LoopMerge,
                            None,
                            None,
                            vec![Operand::IdRef(merge), Operand::IdRef(cont)],
                        ),
                        Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(body)]),
                    ],
                ),
                block(
                    body,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(cont)],
                    )],
                ),
                block(
                    cont,
                    vec![Instruction::new(
                        Op::Branch,
                        None,
                        None,
                        vec![Operand::IdRef(header)],
                    )],
                ),
                block(
                    merge,
                    vec![Instruction::new(Op::Unreachable, None, None, vec![])],
                ),
            ],
        });
        let mut after = before.clone();
        after.functions[0].blocks[0].instructions.remove(0);
        after.functions[0].blocks.retain(|b| {
            b.label
                .as_ref()
                .and_then(|label| label.result_id)
                .is_some_and(|id| id != merge)
        });

        restore_loop_merges_removed_by_fc_prune(&before, &mut after);

        let fixed_header = &after.functions[0].blocks[0];
        assert_eq!(fixed_header.instructions[0].class.opcode, Op::LoopMerge);
        assert_eq!(
            fixed_header.instructions[0].operands,
            vec![Operand::IdRef(merge), Operand::IdRef(cont)]
        );
        let labels = after.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|label| label.result_id))
            .collect::<Vec<_>>();
        assert_eq!(labels, vec![header, body, cont, merge]);
    }

    #[test]
    fn specialize_prunes_dead_fc_interface_globals() {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(22);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.memory_model = Some(Instruction::new(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.entry_points.push(Instruction::new(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(ExecutionModel::GLCompute),
                Operand::IdRef(12),
                Operand::LiteralString("main".into()),
                Operand::IdRef(9),
                Operand::IdRef(10),
            ],
        ));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(Op::TypeBool, None, Some(2), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(Op::ConstantNull, Some(3), Some(6), vec![]),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(Op::TypeFunction, None, Some(11), vec![Operand::IdRef(1)]),
        ];
        module.debug_names = vec![
            name(8, "_Z1x.MTL_FC_INIT_0_b"),
            name(9, "live_global"),
            name(10, "dead_global"),
        ];

        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(1),
            Some(12),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(11),
            ],
        ));
        function.blocks = vec![
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(13), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::Variable,
                        Some(5),
                        Some(19),
                        vec![Operand::StorageClass(StorageClass::Function)],
                    ),
                    Instruction::new(Op::Load, Some(3), Some(14), vec![Operand::IdRef(8)]),
                    Instruction::new(
                        Op::IEqual,
                        Some(2),
                        Some(15),
                        vec![Operand::IdRef(14), Operand::IdRef(6)],
                    ),
                    Instruction::new(
                        Op::SelectionMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(18),
                            Operand::SelectionControl(SelectionControl::NONE),
                        ],
                    ),
                    Instruction::new(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(15), Operand::IdRef(16), Operand::IdRef(17)],
                    ),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(16), vec![])),
                instructions: vec![
                    Instruction::new(Op::Load, Some(3), Some(20), vec![Operand::IdRef(9)]),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(19), Operand::IdRef(20)],
                    ),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(18)]),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(17), vec![])),
                instructions: vec![
                    Instruction::new(Op::Load, Some(3), Some(21), vec![Operand::IdRef(10)]),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(19), Operand::IdRef(21)],
                    ),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(18)]),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(18), vec![])),
                instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
            },
        ];
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        module.functions.push(function);

        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let out = specialize_function_constants(&bytes, &[(0, 0)]).expect("specialize");
        let m = load_bytes(&out).expect("reload");
        let entry_interface = m.entry_points[0]
            .operands
            .iter()
            .skip(3)
            .filter_map(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect::<crate::fxhash::HashSet<_>>();
        let variables = m
            .types_global_values
            .iter()
            .filter(|inst| inst.class.opcode == Op::Variable)
            .filter_map(|inst| inst.result_id)
            .collect::<crate::fxhash::HashSet<_>>();

        assert!(
            entry_interface.contains(&9),
            "live global stays in interface"
        );
        assert!(
            !entry_interface.contains(&10),
            "dead FC-arm global leaves interface"
        );
        assert!(variables.contains(&9), "live global variable stays");
        assert!(!variables.contains(&10), "dead FC-arm global is dropped");
    }

    #[test]
    fn specialize_repoints_init_initializer() {
        let bytes = fixture_bytes(
            6,
            vec![
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(2),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(1),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(1), Some(3), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(2),
                    Some(4),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(3),
                    ],
                ),
                Instruction::new(
                    Op::Variable,
                    Some(2),
                    Some(5),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(3),
                    ],
                ),
            ],
            vec![
                name(4, "_ZN3app11fc_channelsE.MTL_FC_INIT_0_j"),
                name(5, "_ZN3app11fc_channelsE.13"),
            ],
        );

        let out = specialize_function_constants(&bytes, &[(0, 7)]).expect("specialize");
        let m = load_bytes(&out).expect("reload");

        let init_name = m
            .debug_names
            .iter()
            .find(|i| {
                matches!(i.operands.get(1), Some(Operand::LiteralString(s)) if s.contains("MTL_FC_INIT_0"))
            })
            .and_then(|i| match i.operands.first() {
                Some(Operand::IdRef(id)) => Some(*id),
                _ => None,
            })
            .expect("init name");
        let init_var = m
            .types_global_values
            .iter()
            .find(|i| i.class.opcode == Op::Variable && i.result_id == Some(init_name))
            .expect("init var");
        let init_id = match init_var.operands.get(1) {
            Some(Operand::IdRef(id)) => *id,
            other => panic!("init var has no initializer: {other:?}"),
        };
        let init_const = m
            .types_global_values
            .iter()
            .find(|i| i.class.opcode == Op::Constant && i.result_id == Some(init_id))
            .expect("init constant def");
        assert_eq!(
            init_const.operands.first(),
            Some(&Operand::LiteralBit32(7)),
            "INIT initializer should be OpConstant uint 7"
        );

        assert!(specialize_function_constants(&bytes, &[(9, 1)]).is_err());
    }

    #[test]
    fn value_specialization_materializes_float_fc_initializer_bits() {
        let bytes = fixture_bytes(
            5,
            vec![
                Instruction::new(
                    Op::TypeFloat,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(32)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(2),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(1),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(1), Some(3), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(2),
                    Some(4),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(3),
                    ],
                ),
            ],
            vec![name(4, "_Z5scale.MTL_FC_INIT_9_f")],
        );

        let bits = f32::to_bits(1.5) as u64;
        let out = specialize_function_constants(&bytes, &[(9, bits)]).expect("specialize float");
        let module = load_bytes(&out).expect("reload");
        let initializer = module
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(4))
            .and_then(|inst| inst.operands.get(1))
            .and_then(|operand| match operand {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .expect("float initializer");
        let constant = module
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Constant && inst.result_id == Some(initializer))
            .expect("float constant");
        assert_eq!(constant.result_type, Some(1));
        assert_eq!(constant.operands, vec![Operand::LiteralBit32(bits as u32)]);
    }

    #[test]
    fn erased_zero_fc_override_is_a_safe_noop() {
        let bytes = fixture_bytes(
            2,
            vec![Instruction::new(Op::TypeVoid, None, Some(1), vec![])],
            vec![],
        );

        assert_eq!(
            specialize_function_constants(&bytes, &[(7, 0)]).expect("zero no-op"),
            bytes
        );
        assert!(specialize_function_constants(&bytes, &[(7, 1)]).is_err());
    }

    #[test]
    fn value_specialization_materializes_vector_int_fc_initializer() {
        let bytes = fixture_bytes(
            7,
            vec![
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
                ),
                Instruction::new(
                    Op::TypeVector,
                    None,
                    Some(2),
                    vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(3),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(2),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(2), Some(4), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(3),
                    Some(5),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(4),
                    ],
                ),
            ],
            vec![name(5, "_ZN3app12shader_stateE.MTL_FC_INIT_0_Dv4_j")],
        );

        let payload = [1u32, 2, 3, 4]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let out = specialize_function_constant_bytes(&bytes, &[(0, payload)])
            .expect("specialize full vector");
        let m = load_bytes(&out).expect("reload");
        let vector_init = m
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(5))
            .and_then(|inst| inst.operands.get(1))
            .and_then(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .expect("vector initializer");
        let vector_const = m
            .types_global_values
            .iter()
            .find(|inst| {
                inst.class.opcode == Op::ConstantComposite && inst.result_id == Some(vector_init)
            })
            .expect("vector value composite");
        assert_eq!(vector_const.result_type, Some(2));

        let constants = m
            .types_global_values
            .iter()
            .filter(|inst| inst.class.opcode == Op::Constant)
            .filter_map(|inst| {
                let id = inst.result_id?;
                let Some(Operand::LiteralBit32(value)) = inst.operands.first() else {
                    return None;
                };
                Some((id, *value))
            })
            .collect::<crate::fxhash::HashMap<_, _>>();
        let lane_values = vector_const
            .operands
            .iter()
            .map(|op| match op {
                Operand::IdRef(id) => constants[id],
                other => panic!("unexpected composite operand: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(lane_values, vec![1, 2, 3, 4]);
    }

    #[test]
    fn value_specialization_rewrites_thread_local_byte_subslot_store() {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(40);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(Op::TypeFunction, None, Some(2), vec![Operand::IdRef(1)]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(5),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(6),
                vec![Operand::LiteralBit32(16)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(7),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(8),
                vec![Operand::IdRef(3), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(8),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(7),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(4),
                ],
            ),
            Instruction::new(Op::ConstantNull, Some(8), Some(12), vec![]),
            Instruction::new(
                Op::Variable,
                Some(9),
                Some(13),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(12),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(14),
                vec![Operand::LiteralBit32(2)],
            ),
        ];
        module.debug_names = vec![name(13, "_ZN3app12shader_stateE.MTL_FC_INIT_0_Dv4_j")];
        let function = Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(1),
                Some(20),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(2),
                ],
            )),
            blocks: vec![block(
                21,
                vec![
                    Instruction::new(
                        Op::Variable,
                        Some(10),
                        Some(15),
                        vec![Operand::StorageClass(StorageClass::Function)],
                    ),
                    Instruction::new(Op::Undef, Some(6), Some(16), vec![]),
                    Instruction::new(
                        Op::PtrAccessChain,
                        Some(11),
                        Some(17),
                        vec![Operand::IdRef(15), Operand::IdRef(14)],
                    ),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(17), Operand::IdRef(16)],
                    ),
                    Instruction::new(Op::Return, None, None, vec![]),
                ],
            )],
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            ..Default::default()
        };
        module.functions.push(function);

        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let out = specialize_function_constants(&bytes, &[(0, 1)]).expect("specialize");
        let m = load_bytes(&out).expect("reload");
        let insts = &m.functions[0].blocks[0].instructions;

        assert!(
            !insts
                .iter()
                .any(|inst| inst.class.opcode == Op::PtrAccessChain),
            "byte subslot PtrAccessChain should be removed"
        );
        assert!(
            insts.iter().any(|inst| inst.class.opcode == Op::BitwiseOr),
            "store should become a read-modify-write pack"
        );
        assert!(
            insts.iter().any(|inst| {
                inst.class.opcode == Op::Store
                    && matches!(inst.operands.first(), Some(Operand::IdRef(15)))
            }),
            "rewritten store should target the original scalar slot"
        );
    }

    #[test]
    fn zero_specialization_discovers_fc_initializers() {
        let bytes = fixture_bytes(
            5,
            vec![
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(2),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(1),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(1), Some(3), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(2),
                    Some(4),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(3),
                    ],
                ),
            ],
            vec![name(4, "_ZN3app7enabledE.MTL_FC_INIT_7_b")],
        );

        let out = specialize_function_constants_zero(&bytes).expect("zero specialize");
        let m = load_bytes(&out).expect("reload");
        let init_var = m
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(4))
            .expect("init var");
        let init_id = match init_var.operands.get(1) {
            Some(Operand::IdRef(id)) => *id,
            other => panic!("init var has no initializer: {other:?}"),
        };
        let init_const = m
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Constant && inst.result_id == Some(init_id))
            .expect("zero constant def");
        assert_eq!(init_const.operands, vec![Operand::LiteralBit32(0)]);
    }

    #[test]
    fn zero_specialization_materializes_vector_int_fc_initializers() {
        let bytes = fixture_bytes(
            9,
            vec![
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(2),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(1),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(1), Some(3), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(2),
                    Some(4),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(3),
                    ],
                ),
                Instruction::new(
                    Op::TypeVector,
                    None,
                    Some(5),
                    vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
                ),
                Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(6),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(5),
                    ],
                ),
                Instruction::new(Op::ConstantNull, Some(5), Some(7), vec![]),
                Instruction::new(
                    Op::Variable,
                    Some(6),
                    Some(8),
                    vec![
                        Operand::StorageClass(StorageClass::Private),
                        Operand::IdRef(7),
                    ],
                ),
            ],
            vec![
                name(4, "_ZN3app7enabledE.MTL_FC_INIT_7_b"),
                name(8, "_ZN3app9thresholdE.MTL_FC_INIT_9_Dv4_h"),
            ],
        );

        let out = specialize_function_constants_zero(&bytes).expect("zero specialize");
        let m = load_bytes(&out).expect("reload");
        let scalar_init = m
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(4))
            .and_then(|inst| inst.operands.get(1))
            .and_then(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .expect("scalar initializer");
        assert!(
            m.types_global_values
                .iter()
                .any(|inst| inst.class.opcode == Op::Constant
                    && inst.result_id == Some(scalar_init)
                    && inst.result_type == Some(1)),
            "scalar FC initializer should be materialized as an OpConstant"
        );

        let vector_init = m
            .types_global_values
            .iter()
            .find(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(8))
            .and_then(|inst| inst.operands.get(1))
            .and_then(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .expect("vector initializer");
        let vector_const = m
            .types_global_values
            .iter()
            .find(|inst| {
                inst.class.opcode == Op::ConstantComposite && inst.result_id == Some(vector_init)
            })
            .expect("vector zero composite");
        assert_eq!(vector_const.result_type, Some(5));
        assert_eq!(
            vector_const.operands,
            vec![
                Operand::IdRef(scalar_init),
                Operand::IdRef(scalar_init),
                Operand::IdRef(scalar_init),
                Operand::IdRef(scalar_init),
            ],
            "vector FC initializer should be an explicit zero composite"
        );
    }

    #[test]
    fn zero_specialization_marks_all_fc_definedness_predicates_true() {
        let out =
            specialize_function_constants_zero(&definedness_fixture_bytes()).expect("specialize");
        let module = load_bytes(&out).expect("reload");

        assert_eq!(
            copy_object_operand_const_opcode(&module, 11),
            Op::ConstantTrue
        );
        assert_eq!(
            copy_object_operand_const_opcode(&module, 12),
            Op::ConstantTrue
        );
    }

    #[test]
    fn value_specialization_marks_only_listed_fc_definedness_predicates_true() {
        let out = specialize_function_constants(&definedness_fixture_bytes(), &[(3, 1)])
            .expect("specialize");
        let module = load_bytes(&out).expect("reload");

        assert_eq!(
            copy_object_operand_const_opcode(&module, 11),
            Op::ConstantTrue
        );
        assert_eq!(
            copy_object_operand_const_opcode(&module, 12),
            Op::ConstantFalse
        );
    }

    #[test]
    fn zero_specialization_prunes_static_fc_predicate_without_init_global() {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(40);
        header.set_version(1, 4);
        module.header = Some(header);
        module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(Capability::Shader)],
        ));
        module.memory_model = Some(Instruction::new(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.entry_points.push(Instruction::new(
            Op::EntryPoint,
            None,
            None,
            vec![
                Operand::ExecutionModel(ExecutionModel::GLCompute),
                Operand::IdRef(13),
                Operand::LiteralString("main".into()),
                Operand::IdRef(9),
                Operand::IdRef(10),
            ],
        ));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(Op::TypeBool, None, Some(2), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(Op::ConstantNull, Some(3), Some(6), vec![]),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(7),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(Op::ConstantFalse, Some(2), Some(8), vec![]),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(4),
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(Op::TypeFunction, None, Some(12), vec![Operand::IdRef(1)]),
        ];
        module.debug_names = vec![name(9, "live_global"), name(10, "dead_global")];

        let mut function = Function::new();
        function.def = Some(Instruction::new(
            Op::Function,
            Some(1),
            Some(13),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(12),
            ],
        ));
        function.blocks = vec![
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(14), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::Variable,
                        Some(5),
                        Some(19),
                        vec![Operand::StorageClass(StorageClass::Function)],
                    ),
                    Instruction::new(
                        Op::Select,
                        Some(3),
                        Some(20),
                        vec![Operand::IdRef(8), Operand::IdRef(7), Operand::IdRef(6)],
                    ),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(11), Operand::IdRef(20)],
                    ),
                    Instruction::new(Op::Load, Some(3), Some(21), vec![Operand::IdRef(11)]),
                    Instruction::new(
                        Op::IEqual,
                        Some(2),
                        Some(22),
                        vec![Operand::IdRef(21), Operand::IdRef(6)],
                    ),
                    Instruction::new(
                        Op::SelectionMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(25),
                            Operand::SelectionControl(SelectionControl::NONE),
                        ],
                    ),
                    Instruction::new(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(22), Operand::IdRef(23), Operand::IdRef(24)],
                    ),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(23), vec![])),
                instructions: vec![
                    Instruction::new(Op::Load, Some(3), Some(26), vec![Operand::IdRef(9)]),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(19), Operand::IdRef(26)],
                    ),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(25)]),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(24), vec![])),
                instructions: vec![
                    Instruction::new(Op::Load, Some(3), Some(27), vec![Operand::IdRef(10)]),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(19), Operand::IdRef(27)],
                    ),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(25)]),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(25), vec![])),
                instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
            },
        ];
        function.end = Some(Instruction::new(Op::FunctionEnd, None, None, vec![]));
        module.functions.push(function);

        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        let out = specialize_function_constants_zero(&bytes).expect("zero specialize");
        let m = load_bytes(&out).expect("reload");
        let entry_interface = m.entry_points[0]
            .operands
            .iter()
            .skip(3)
            .filter_map(|op| match op {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect::<crate::fxhash::HashSet<_>>();
        let variables = m
            .types_global_values
            .iter()
            .filter(|inst| inst.class.opcode == Op::Variable)
            .filter_map(|inst| inst.result_id)
            .collect::<crate::fxhash::HashSet<_>>();

        assert!(
            entry_interface.contains(&9),
            "live global stays in interface"
        );
        assert!(
            !entry_interface.contains(&10),
            "dead static-FC arm leaves interface"
        );
        assert!(variables.contains(&9), "live global variable stays");
        assert!(
            !variables.contains(&10),
            "dead static-FC arm global is dropped"
        );
    }

    #[test]
    fn specialize_handles_interleaved_types_and_vars() {
        let private_pointer = |id, pointee| {
            Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(pointee),
                ],
            )
        };
        let variable = |id, pointer, initializer| {
            Instruction::new(
                Op::Variable,
                Some(pointer),
                Some(id),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(initializer),
                ],
            )
        };
        let bytes = fixture_bytes(
            9,
            vec![
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(1),
                    vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
                ),
                private_pointer(2, 1),
                Instruction::new(Op::ConstantNull, Some(1), Some(3), vec![]),
                variable(4, 2, 3),
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(5),
                    vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
                ),
                private_pointer(6, 5),
                Instruction::new(Op::ConstantNull, Some(5), Some(7), vec![]),
                variable(8, 6, 7),
            ],
            vec![
                name(4, "_Z3fooE.MTL_FC_INIT_9_b"),
                name(8, "_Z3barE.MTL_FC_INIT_8_t"),
            ],
        );

        let out = specialize_function_constants(&bytes, &[(8, 32), (9, 1)]).expect("specialize");
        let m = load_bytes(&out).expect("reload");
        use spirv::Op;
        let pos = |id: u32| {
            m.types_global_values
                .iter()
                .position(|i| i.result_id == Some(id))
        };
        for var in m
            .types_global_values
            .iter()
            .filter(|i| i.class.opcode == Op::Variable)
        {
            let init = match var.operands.get(1) {
                Some(Operand::IdRef(id)) => *id,
                other => panic!("var missing initializer: {other:?}"),
            };
            let cinst = m
                .types_global_values
                .iter()
                .find(|i| i.class.opcode == Op::Constant && i.result_id == Some(init))
                .expect("constant def present");
            let cty = cinst.result_type.expect("const has type");
            let (pc, pv, pt) = (
                pos(init).unwrap(),
                pos(var.result_id.unwrap()).unwrap(),
                pos(cty).unwrap(),
            );
            assert!(pt < pc, "constant type must precede the constant");
            assert!(
                pc < pv,
                "constant must precede the variable that initializes with it"
            );
        }
        let val_of = |marker: &str| -> u64 {
            let vid = m
                .debug_names
                .iter()
                .find(|i| matches!(i.operands.get(1), Some(Operand::LiteralString(s)) if s.contains(marker)))
                .and_then(|i| match i.operands.first() { Some(Operand::IdRef(id)) => Some(*id), _ => None })
                .unwrap();
            let init = match m
                .types_global_values
                .iter()
                .find(|i| i.result_id == Some(vid))
                .unwrap()
                .operands
                .get(1)
            {
                Some(Operand::IdRef(id)) => *id,
                _ => panic!(),
            };
            match m
                .types_global_values
                .iter()
                .find(|i| i.result_id == Some(init))
                .unwrap()
                .operands
                .first()
            {
                Some(Operand::LiteralBit32(v)) => *v as u64,
                Some(Operand::LiteralBit64(v)) => *v,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(val_of("MTL_FC_INIT_8"), 32);
        assert_eq!(val_of("MTL_FC_INIT_9"), 1);
    }
}
