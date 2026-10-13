use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{Op, StorageClass, Word};
use std::cmp::Ordering;

#[derive(Clone)]
struct AccessChainDef {
    root: Word,
    indices: Vec<Word>,
    parent_before_last: Option<Word>,
}

pub(crate) fn lower_zero_base_storage_buffer_ptr_access_chains(module: &mut Module) -> usize {
    if !module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            matches!(
                inst.class.opcode,
                Op::PtrAccessChain | Op::InBoundsPtrAccessChain
            )
        })
    {
        return 0;
    }
    let defs = collect_defs(module);
    let zero_constants = zero_integer_constants(&defs);
    let access_chains = collect_access_chain_defs(module, &defs, &zero_constants);
    let direct_demotions =
        collect_zero_stride_ptr_access_chain_demotions(module, &defs, &zero_constants);
    if access_chains.is_empty() && direct_demotions.is_empty() {
        return 0;
    }

    struct Rewrite {
        operands: Vec<Operand>,
        add: Option<(Word, Word, Word, Word)>,
    }

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut rewrites: HashMap<Word, Rewrite> = HashMap::default();
    for inst in module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
    {
        if !matches!(
            inst.class.opcode,
            Op::PtrAccessChain | Op::InBoundsPtrAccessChain
        ) {
            continue;
        }
        let (Some(result_id), Some(result_type)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let Some((StorageClass::StorageBuffer, result_pointee)) = ptr_info(&defs, result_type)
        else {
            continue;
        };
        let Some(Operand::IdRef(base)) = inst.operands.first() else {
            continue;
        };
        let Some(base_chain) = access_chains.get(base) else {
            continue;
        };
        let Some((StorageClass::StorageBuffer, root_pointee)) =
            value_pointer_info(&defs, base_chain.root)
        else {
            continue;
        };
        if base_chain.indices.is_empty() {
            continue;
        }
        let Some(parent) = base_chain.parent_before_last else {
            continue;
        };
        if !is_array_indexable_type(&defs, parent) {
            continue;
        }
        let Some(ptr_indices) = id_ref_operands(&inst.operands[1..]) else {
            continue;
        };
        if ptr_indices.is_empty() {
            continue;
        }

        let mut new_indices = Vec::with_capacity(base_chain.indices.len() + ptr_indices.len() - 1);
        new_indices.extend_from_slice(&base_chain.indices[..base_chain.indices.len() - 1]);
        let Some(last_base_index) = base_chain.indices.last() else {
            continue;
        };
        let add = if zero_constants.contains(last_base_index) {
            new_indices.push(ptr_indices[0]);
            None
        } else {
            let Some(index_ty) = integer_value_type(&defs, *last_base_index) else {
                continue;
            };
            if integer_value_type(&defs, ptr_indices[0]) != Some(index_ty) {
                continue;
            }
            let sum = next_id;
            next_id += 1;
            new_indices.push(sum);
            Some((sum, index_ty, *last_base_index, ptr_indices[0]))
        };
        new_indices.extend_from_slice(&ptr_indices[1..]);
        if walk_access_chain_pointee(&defs, root_pointee, &new_indices) != Some(result_pointee) {
            continue;
        }

        let mut operands = Vec::with_capacity(1 + new_indices.len());
        operands.push(Operand::IdRef(base_chain.root));
        operands.extend(new_indices.into_iter().map(Operand::IdRef));
        rewrites.insert(result_id, Rewrite { operands, add });
    }

    let mut changed = 0;
    for block in module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
    {
        let mut pos = 0;
        while pos < block.instructions.len() {
            let Some(result_id) = block.instructions[pos].result_id else {
                pos += 1;
                continue;
            };
            if let Some(rewrite) = rewrites.remove(&result_id) {
                if let Some((sum, ty, lhs, rhs)) = rewrite.add {
                    block.instructions.insert(
                        pos,
                        Instruction::new(
                            Op::IAdd,
                            Some(ty),
                            Some(sum),
                            vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
                        ),
                    );
                    pos += 1;
                }
                block.instructions[pos].class.opcode = Op::AccessChain;
                block.instructions[pos].operands = rewrite.operands;
                changed += 1;
                pos += 1;
                continue;
            }
            if direct_demotions.contains(&result_id) {
                block.instructions[pos].class.opcode = Op::AccessChain;
                changed += 1;
            }
            pos += 1;
        }
    }

    if changed != 0 {
        if let Some(header) = module.header.as_mut() {
            header.bound = next_id;
        }
    }
    changed
}

pub(crate) fn rewrite_storage_buffer_atomic_scopes(module: &mut Module) -> usize {
    if !module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| atomic_scope_semantics_indices(inst).is_some())
    {
        return 0;
    }
    let mut defs = collect_defs(module);
    let mut rewrites = Vec::new();
    for (fi, function) in module.functions.iter().enumerate() {
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                let Some((scope_idx, semantics_indices)) = atomic_scope_semantics_indices(inst)
                else {
                    continue;
                };
                let Some(Operand::IdRef(ptr)) = inst.operands.first() else {
                    continue;
                };
                let Some((StorageClass::StorageBuffer, _)) = value_pointer_info(&defs, *ptr) else {
                    continue;
                };
                let Some(scope_operand) = inst.operands.get(scope_idx) else {
                    continue;
                };
                let Some(scope_id) = operand_id(scope_operand) else {
                    continue;
                };
                if constant_u32(&defs, scope_id) != Some(spirv::Scope::Workgroup as u32) {
                    continue;
                }
                let Some(scope_ty) = integer_value_type(&defs, scope_id) else {
                    continue;
                };
                let mut semantics_tys = Vec::new();
                for &semantics_idx in semantics_indices {
                    let Some(semantics_operand) = inst.operands.get(semantics_idx) else {
                        semantics_tys.clear();
                        break;
                    };
                    let Some(semantics_id) = operand_id(semantics_operand) else {
                        semantics_tys.clear();
                        break;
                    };
                    let Some(semantics_ty) = integer_value_type(&defs, semantics_id) else {
                        semantics_tys.clear();
                        break;
                    };
                    semantics_tys.push((semantics_idx, semantics_ty));
                }
                if semantics_tys.len() != semantics_indices.len() {
                    continue;
                }
                rewrites.push((fi, bi, ii, scope_idx, scope_ty, semantics_tys));
            }
        }
    }
    if rewrites.is_empty() {
        return 0;
    }

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut constants = HashMap::default();
    for (_, _, _, _, scope_ty, semantics_tys) in &rewrites {
        let device = get_or_create_integer_constant(module, &mut defs, &mut next_id, *scope_ty, 1);
        constants.insert((*scope_ty, 1), device);
        for (_, semantics_ty) in semantics_tys {
            let relaxed =
                get_or_create_integer_constant(module, &mut defs, &mut next_id, *semantics_ty, 0);
            constants.insert((*semantics_ty, 0), relaxed);
        }
    }

    for (fi, bi, ii, scope_idx, scope_ty, semantics_tys) in rewrites {
        let inst = &mut module.functions[fi].blocks[bi].instructions[ii];
        let device = constants[&(scope_ty, 1)];
        inst.operands[scope_idx] = Operand::IdScope(device);
        for (semantics_idx, semantics_ty) in semantics_tys {
            let relaxed = constants[&(semantics_ty, 0)];
            inst.operands[semantics_idx] = Operand::IdMemorySemantics(relaxed);
        }
    }

    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    constants.len()
}

fn collect_zero_stride_ptr_access_chain_demotions(
    module: &Module,
    defs: &HashMap<Word, Instruction>,
    zero_constants: &HashSet<Word>,
) -> HashSet<Word> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            if !matches!(
                inst.class.opcode,
                Op::PtrAccessChain | Op::InBoundsPtrAccessChain
            ) {
                return None;
            }
            let result_id = inst.result_id?;
            let result_type = inst.result_type?;
            let Some((StorageClass::StorageBuffer, result_pointee)) = ptr_info(defs, result_type)
            else {
                return None;
            };
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                return None;
            };
            let (StorageClass::StorageBuffer, base_pointee) = value_pointer_info(defs, *base)?
            else {
                return None;
            };
            let ptr_indices = id_ref_operands(&inst.operands[1..])?;
            let first = ptr_indices.first()?;
            if !zero_constants.contains(first) {
                return None;
            }
            (walk_access_chain_pointee(defs, base_pointee, &ptr_indices) == Some(result_pointee))
                .then_some(result_id)
        })
        .collect()
}

fn atomic_scope_semantics_indices(inst: &Instruction) -> Option<(usize, &'static [usize])> {
    match inst.class.opcode {
        Op::AtomicLoad => Some((1, &[2])),
        Op::AtomicStore => Some((1, &[2])),
        Op::AtomicExchange
        | Op::AtomicIIncrement
        | Op::AtomicIDecrement
        | Op::AtomicIAdd
        | Op::AtomicISub
        | Op::AtomicSMin
        | Op::AtomicUMin
        | Op::AtomicSMax
        | Op::AtomicUMax
        | Op::AtomicAnd
        | Op::AtomicOr
        | Op::AtomicXor
        | Op::AtomicFAddEXT => Some((1, &[2])),
        Op::AtomicCompareExchange | Op::AtomicCompareExchangeWeak => Some((1, &[2, 3])),
        _ => None,
    }
}

fn operand_id(operand: &Operand) -> Option<Word> {
    match operand {
        Operand::IdRef(id) | Operand::IdScope(id) | Operand::IdMemorySemantics(id) => Some(*id),
        _ => None,
    }
}

fn get_or_create_integer_constant(
    module: &mut Module,
    defs: &mut HashMap<Word, Instruction>,
    next_id: &mut Word,
    ty: Word,
    value: u32,
) -> Word {
    if let Some(id) = defs.iter().find_map(|(&id, inst)| {
        (inst.class.opcode == Op::Constant
            && inst.result_type == Some(ty)
            && constant_u32(defs, id) == Some(value))
        .then_some(id)
    }) {
        return id;
    }
    let id = *next_id;
    *next_id += 1;
    let inst = Instruction::new(
        Op::Constant,
        Some(ty),
        Some(id),
        vec![Operand::LiteralBit32(value)],
    );
    module.types_global_values.push(inst.clone());
    defs.insert(id, inst);
    id
}

pub(crate) fn variable_pointer_requirement(module: &Module) -> (bool, bool) {
    if !module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|inst| {
            matches!(
                inst.class.opcode,
                Op::Phi | Op::Select | Op::PtrAccessChain | Op::InBoundsPtrAccessChain
            )
        })
    {
        return (false, false);
    }
    let pointer_storage = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            if inst.class.opcode != Op::TypePointer {
                return None;
            }
            let Operand::StorageClass(storage) = inst.operands.first()? else {
                return None;
            };
            Some((inst.result_id?, *storage))
        })
        .collect::<HashMap<_, _>>();
    let mut has_storage_buffer_pointer_merge = false;
    let mut has_other_pointer_merge = false;
    for inst in module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
    {
        if !matches!(
            inst.class.opcode,
            Op::Phi | Op::Select | Op::PtrAccessChain | Op::InBoundsPtrAccessChain
        ) {
            continue;
        }
        let Some(result_type) = inst.result_type else {
            continue;
        };
        let Some(&storage) = pointer_storage.get(&result_type) else {
            continue;
        };
        match storage {
            StorageClass::StorageBuffer => has_storage_buffer_pointer_merge = true,
            StorageClass::PhysicalStorageBuffer => {}
            _ => has_other_pointer_merge = true,
        }
    }
    (has_storage_buffer_pointer_merge, has_other_pointer_merge)
}

fn collect_defs(module: &Module) -> HashMap<Word, Instruction> {
    let needed_values = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            matches!(
                inst.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::InBoundsPtrAccessChain
            ) || atomic_scope_semantics_indices(inst).is_some()
        })
        .flat_map(|inst| &inst.operands)
        .filter_map(operand_id)
        .collect::<HashSet<_>>();
    let mut definitions = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect::<HashMap<_, _>>();
    for function in &module.functions {
        for parameter in &function.parameters {
            if let Some(id) = parameter.result_id.filter(|id| needed_values.contains(id)) {
                definitions.insert(
                    id,
                    Instruction::new(
                        parameter.class.opcode,
                        parameter.result_type,
                        Some(id),
                        Vec::new(),
                    ),
                );
            }
        }
        for inst in function.blocks.iter().flat_map(|block| &block.instructions) {
            if let Some(id) = inst.result_id.filter(|id| needed_values.contains(id)) {
                definitions.insert(
                    id,
                    Instruction::new(inst.class.opcode, inst.result_type, Some(id), Vec::new()),
                );
            }
        }
    }
    definitions
}

fn collect_access_chain_defs(
    module: &Module,
    defs: &HashMap<Word, Instruction>,
    zero_constants: &HashSet<Word>,
) -> HashMap<Word, AccessChainDef> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| {
            if !matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                return None;
            }
            let result_id = inst.result_id?;
            let result_type = inst.result_type?;
            if ptr_info(defs, result_type)?.0 != StorageClass::StorageBuffer {
                return None;
            }
            let Operand::IdRef(root) = inst.operands.first()? else {
                return None;
            };
            let indices = id_ref_operands(&inst.operands[1..])?;
            let (root_storage, root_pointee) = value_pointer_info(defs, *root)?;
            if root_storage != StorageClass::StorageBuffer {
                return None;
            }
            let (parent_before_last, selected_pointee) =
                walk_access_chain_with_parent(defs, root_pointee, &indices, zero_constants)?;
            if ptr_info(defs, result_type)?.1 != selected_pointee {
                return None;
            }
            Some((
                result_id,
                AccessChainDef {
                    root: *root,
                    indices,
                    parent_before_last,
                },
            ))
        })
        .collect()
}

fn id_ref_operands(operands: &[Operand]) -> Option<Vec<Word>> {
    operands
        .iter()
        .map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect()
}

fn value_pointer_info(
    defs: &HashMap<Word, Instruction>,
    value: Word,
) -> Option<(StorageClass, Word)> {
    let ptr_ty = defs.get(&value)?.result_type?;
    ptr_info(defs, ptr_ty)
}

fn ptr_info(defs: &HashMap<Word, Instruction>, ptr_ty: Word) -> Option<(StorageClass, Word)> {
    let inst = defs.get(&ptr_ty)?;
    if inst.class.opcode != Op::TypePointer {
        return None;
    }
    match (inst.operands.first()?, inst.operands.get(1)?) {
        (Operand::StorageClass(storage), Operand::IdRef(pointee)) => Some((*storage, *pointee)),
        _ => None,
    }
}

fn zero_integer_constants(defs: &HashMap<Word, Instruction>) -> HashSet<Word> {
    defs.iter()
        .filter_map(|(&id, inst)| is_zero_integer_constant(defs, inst).then_some(id))
        .collect()
}

fn is_zero_integer_constant(defs: &HashMap<Word, Instruction>, inst: &Instruction) -> bool {
    if inst
        .result_type
        .and_then(|ty| defs.get(&ty))
        .is_none_or(|ty| ty.class.opcode != Op::TypeInt)
    {
        return false;
    }
    matches!(
        (inst.class.opcode, inst.operands.as_slice()),
        (Op::ConstantNull, [])
            | (Op::Constant, [Operand::LiteralBit32(0)])
            | (Op::Constant, [Operand::LiteralBit64(0)])
    )
}

fn integer_value_type(defs: &HashMap<Word, Instruction>, id: Word) -> Option<Word> {
    let ty = defs.get(&id)?.result_type?;
    (defs.get(&ty)?.class.opcode == Op::TypeInt).then_some(ty)
}

fn constant_u32(defs: &HashMap<Word, Instruction>, id: Word) -> Option<u32> {
    let inst = defs.get(&id)?;
    if inst
        .result_type
        .and_then(|ty| defs.get(&ty))
        .is_none_or(|ty| ty.class.opcode != Op::TypeInt)
    {
        return None;
    }
    match (inst.class.opcode, inst.operands.as_slice()) {
        (Op::ConstantNull, []) => Some(0),
        (Op::Constant, [Operand::LiteralBit32(value)]) => Some(*value),
        (Op::Constant, [Operand::LiteralBit64(value)]) => u32::try_from(*value).ok(),
        _ => None,
    }
}

fn is_array_indexable_type(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    defs.get(&ty)
        .is_some_and(|inst| matches!(inst.class.opcode, Op::TypeArray | Op::TypeRuntimeArray))
}

fn walk_access_chain_pointee(
    defs: &HashMap<Word, Instruction>,
    root_pointee: Word,
    indices: &[Word],
) -> Option<Word> {
    walk_access_chain_with_parent(defs, root_pointee, indices, &HashSet::default())
        .map(|(_, pointee)| pointee)
}

fn walk_access_chain_with_parent(
    defs: &HashMap<Word, Instruction>,
    root_pointee: Word,
    indices: &[Word],
    zero_constants: &HashSet<Word>,
) -> Option<(Option<Word>, Word)> {
    let mut cur = root_pointee;
    let mut parent_before_last = None;
    for (index_pos, &index_id) in indices.iter().enumerate() {
        if index_pos + 1 == indices.len() {
            parent_before_last = Some(cur);
        }
        cur = walk_member(defs, cur, index_id, zero_constants)?;
    }
    Some((parent_before_last, cur))
}

fn walk_member(
    defs: &HashMap<Word, Instruction>,
    aggregate: Word,
    index: Word,
    zero_constants: &HashSet<Word>,
) -> Option<Word> {
    let inst = defs.get(&aggregate)?;
    match inst.class.opcode {
        Op::TypeVector | Op::TypeMatrix | Op::TypeArray | Op::TypeRuntimeArray => {
            match inst.operands.first()? {
                Operand::IdRef(elem) => Some(*elem),
                _ => None,
            }
        }
        Op::TypeStruct => {
            let member_idx = if zero_constants.contains(&index) {
                0
            } else {
                constant_u32(defs, index)? as usize
            };
            match inst.operands.get(member_idx)? {
                Operand::IdRef(member) => Some(*member),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) fn lower_storage_buffer_pointer_phis(module: &mut Module) -> usize {
    let defs = collect_all_defs(module);
    let mut rewritten = 0;
    for function_index in 0..module.functions.len() {
        rewritten += lower_pointer_phis_in_function(module, function_index, &defs);
    }
    rewritten
}

fn collect_all_defs(module: &Module) -> HashMap<Word, Instruction> {
    let mut definitions = module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect::<HashMap<_, _>>();
    for function in &module.functions {
        for inst in function.blocks.iter().flat_map(|block| &block.instructions) {
            if let Some(id) = inst.result_id {
                definitions.insert(id, inst.clone());
            }
        }
    }
    definitions
}

struct PointerPhiGroup {
    phis: Vec<Word>,
    steps: HashMap<Word, (Word, Word)>,
    entries: HashMap<Word, Word>,
    root: Word,
    leading: Vec<Word>,
    pointer_type: Word,
    index_type: Word,
    narrow_entry_type: Option<Word>,
    narrow_delta_type: Option<Word>,
}

fn lower_pointer_phis_in_function(
    module: &mut Module,
    function_index: usize,
    defs: &HashMap<Word, Instruction>,
) -> usize {
    let Some(groups) = collect_pointer_phi_groups(module, function_index, defs) else {
        return 0;
    };
    let mut rewritten = 0;
    for group in groups {
        if apply_pointer_phi_group(module, function_index, &group) {
            rewritten += 1;
        }
    }
    rewritten
}

fn collect_pointer_phi_groups(
    module: &Module,
    function_index: usize,
    defs: &HashMap<Word, Instruction>,
) -> Option<Vec<PointerPhiGroup>> {
    let function = module.functions.get(function_index)?;
    let phis = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| inst.class.opcode == Op::Phi)
        .filter(|inst| {
            inst.result_type
                .and_then(|ty| ptr_info(defs, ty))
                .is_some_and(|(storage, _)| storage == StorageClass::StorageBuffer)
        })
        .filter_map(|inst| inst.result_id)
        .collect::<Vec<_>>();
    if phis.is_empty() {
        return None;
    }
    let mut component: HashMap<Word, usize> = HashMap::default();
    for (index, phi) in phis.iter().enumerate() {
        component.insert(*phi, index);
    }
    let member = phis.iter().copied().collect::<HashSet<_>>();
    let mut changed = true;
    while changed {
        changed = false;
        for phi in &phis {
            let Some(inst) = defs.get(phi) else { continue };
            for operand in inst.operands.iter().filter_map(operand_id) {
                if !member.contains(&operand) {
                    continue;
                }
                let (a, b) = (component[phi], component[&operand]);
                if a != b {
                    let (low, high) = (a.min(b), a.max(b));
                    for value in component.values_mut() {
                        if *value == high {
                            *value = low;
                        }
                    }
                    changed = true;
                }
            }
        }
    }
    let mut networks: HashMap<usize, Vec<Word>> = HashMap::default();
    for phi in &phis {
        networks.entry(component[phi]).or_default().push(*phi);
    }
    let mut groups = Vec::new();
    for (_, mut network) in networks {
        network.sort_unstable();
        if let Some(group) = build_pointer_phi_group(function, defs, network) {
            groups.push(group);
        }
    }
    groups.sort_by_key(|group| group.phis.first().copied().unwrap_or(0));
    Some(groups)
}

fn build_pointer_phi_group(
    function: &crate::spirv_module::Function,
    defs: &HashMap<Word, Instruction>,
    phis: Vec<Word>,
) -> Option<PointerPhiGroup> {
    let members = phis.iter().copied().collect::<HashSet<_>>();
    let pointer_type = defs.get(phis.first()?)?.result_type?;
    if phis
        .iter()
        .any(|phi| defs.get(phi).and_then(|inst| inst.result_type) != Some(pointer_type))
    {
        return None;
    }

    let mut steps: HashMap<Word, (Word, Word)> = HashMap::default();
    for inst in function.blocks.iter().flat_map(|block| &block.instructions) {
        if !matches!(
            inst.class.opcode,
            Op::PtrAccessChain | Op::InBoundsPtrAccessChain
        ) {
            continue;
        }
        let (Some(result), Some(result_type)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let [Operand::IdRef(base), Operand::IdRef(delta)] = inst.operands.as_slice() else {
            continue;
        };
        if !members.contains(base) {
            continue;
        }
        if result_type != pointer_type {
            return None;
        }
        steps.insert(result, (*base, *delta));
    }

    let mut entries: HashMap<Word, Word> = HashMap::default();
    let mut root_and_leading: Option<(Word, Vec<Word>)> = None;
    for phi in &phis {
        let inst = defs.get(phi)?;
        for (position, operand) in inst.operands.iter().enumerate() {
            if position % 2 != 0 {
                continue;
            }
            let Operand::IdRef(value) = operand else {
                return None;
            };
            if members.contains(value) || steps.contains_key(value) {
                continue;
            }
            let entry = defs.get(value)?;
            if !matches!(
                entry.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain
            ) || entry.result_type != Some(pointer_type)
            {
                return None;
            }
            let ids = id_ref_operands(&entry.operands)?;
            let (root, indices) = ids.split_first()?;
            let (last, leading) = indices.split_last()?;
            match &root_and_leading {
                None => root_and_leading = Some((*root, leading.to_vec())),
                Some((known_root, known_leading)) => {
                    if known_root != root || known_leading != leading {
                        return None;
                    }
                }
            }
            entries.insert(*value, *last);
        }
    }
    let (root, leading) = root_and_leading?;
    if defs
        .get(&root)
        .is_none_or(|inst| inst.class.opcode != Op::Variable)
    {
        return None;
    }
    if leading.iter().any(|index| {
        defs.get(index)
            .is_none_or(|inst| !matches!(inst.class.opcode, Op::Constant | Op::ConstantNull))
    }) {
        return None;
    }

    let index_type = integer_value_type(defs, *entries.values().next()?)?;
    let index_width = integer_type_width(defs, index_type)?;
    if entries
        .values()
        .any(|id| integer_value_type(defs, *id) != Some(index_type))
    {
        return None;
    }
    let delta_type = match steps.values().next() {
        None => index_type,
        Some((_, first)) => {
            let ty = integer_value_type(defs, *first)?;
            if steps
                .values()
                .any(|(_, delta)| integer_value_type(defs, *delta) != Some(ty))
            {
                return None;
            }
            ty
        }
    };
    let delta_width = integer_type_width(defs, delta_type)?;
    let (index_type, narrow_entry_type, narrow_delta_type) = match index_width.cmp(&delta_width) {
        Ordering::Equal => (index_type, None, None),
        Ordering::Less => (delta_type, Some(index_type), None),
        Ordering::Greater => (index_type, None, Some(delta_type)),
    };
    for narrow in [narrow_entry_type, narrow_delta_type].into_iter().flatten() {
        if !integer_type_is_unsigned(defs, narrow) {
            return None;
        }
    }

    let rewritable = members
        .iter()
        .chain(steps.keys())
        .copied()
        .collect::<HashSet<_>>();
    for inst in function.blocks.iter().flat_map(|block| &block.instructions) {
        if inst.result_id.is_some_and(|id| rewritable.contains(&id)) {
            continue;
        }
        let mut named = inst
            .operands
            .iter()
            .enumerate()
            .filter(|(_, operand)| matches!(operand, Operand::IdRef(id) if rewritable.contains(id)))
            .map(|(position, _)| position);
        let Some(first) = named.next() else { continue };
        if first != 0 || named.next().is_some() {
            return None;
        }
        if !matches!(inst.class.opcode, Op::Load | Op::Store) {
            return None;
        }
    }
    Some(PointerPhiGroup {
        phis,
        steps,
        entries,
        root,
        leading,
        pointer_type,
        index_type,
        narrow_entry_type,
        narrow_delta_type,
    })
}

fn integer_type_is_unsigned(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    defs.get(&ty).is_some_and(|def| {
        def.class.opcode == Op::TypeInt
            && matches!(def.operands.get(1), Some(Operand::LiteralBit32(0)))
    })
}

fn apply_pointer_phi_group(
    module: &mut Module,
    function_index: usize,
    group: &PointerPhiGroup,
) -> bool {
    let leaves = module.functions[function_index]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|inst| {
            matches!(inst.operands.first(), Some(Operand::IdRef(id))
                if group.phis.contains(id) || group.steps.contains_key(id))
        })
        .count();
    let widened_entries = group.narrow_entry_type.map_or(0, |_| group.entries.len());
    let widened_deltas = group.narrow_delta_type.map_or(0, |_| group.steps.len());
    let mut ids = module.reserve_ids(
        (group.phis.len() + group.steps.len() + leaves + widened_entries + widened_deltas) as Word,
    );
    let mut fresh = || ids.next().expect("reserved id");
    let mut index_of: HashMap<Word, Word> = HashMap::default();
    for phi in &group.phis {
        index_of.insert(*phi, fresh());
    }
    for step in group.steps.keys() {
        index_of.insert(*step, fresh());
    }
    let widened_entry = group
        .narrow_entry_type
        .map(|_| {
            group
                .entries
                .keys()
                .map(|chain| (*chain, fresh()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let widened_delta = group
        .narrow_delta_type
        .map(|_| {
            group
                .steps
                .keys()
                .map(|step| (*step, fresh()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let index_for = |value: Word| -> Option<Word> {
        index_of.get(&value).copied().or_else(|| {
            group
                .entries
                .get(&value)
                .map(|index| widened_entry.get(&value).copied().unwrap_or(*index))
        })
    };

    let function = &mut module.functions[function_index];
    for block in &mut function.blocks {
        let mut rewritten = Vec::with_capacity(block.instructions.len());
        for inst in block.instructions.drain(..) {
            let result = inst.result_id;
            if result.is_some_and(|id| index_of.contains_key(&id) && group.phis.contains(&id)) {
                let mut operands = Vec::with_capacity(inst.operands.len());
                for (position, operand) in inst.operands.iter().enumerate() {
                    if position % 2 == 1 {
                        operands.push(operand.clone());
                        continue;
                    }
                    let Some(Some(index)) = operand_id(operand).map(index_for) else {
                        return false;
                    };
                    operands.push(Operand::IdRef(index));
                }
                rewritten.push(Instruction::new(
                    Op::Phi,
                    Some(group.index_type),
                    Some(index_of[&result.expect("phi result")]),
                    operands,
                ));
                continue;
            }
            if let Some((base, delta)) = result.and_then(|id| group.steps.get(&id)) {
                let Some(base_index) = index_for(*base) else {
                    return false;
                };
                let step = result.expect("step result");
                let delta = match widened_delta.get(&step) {
                    None => *delta,
                    Some(widened) => {
                        rewritten.push(Instruction::new(
                            Op::UConvert,
                            Some(group.index_type),
                            Some(*widened),
                            vec![Operand::IdRef(*delta)],
                        ));
                        *widened
                    }
                };
                rewritten.push(Instruction::new(
                    Op::IAdd,
                    Some(group.index_type),
                    Some(index_of[&step]),
                    vec![Operand::IdRef(base_index), Operand::IdRef(delta)],
                ));
                continue;
            }
            let pointer = match inst.operands.first() {
                Some(Operand::IdRef(id)) if index_of.contains_key(id) => Some(*id),
                _ => None,
            };
            if let Some(pointer) = pointer {
                let chain = fresh();
                let mut operands = vec![Operand::IdRef(group.root)];
                operands.extend(group.leading.iter().map(|id| Operand::IdRef(*id)));
                operands.push(Operand::IdRef(index_of[&pointer]));
                rewritten.push(Instruction::new(
                    Op::AccessChain,
                    Some(group.pointer_type),
                    Some(chain),
                    operands,
                ));
                let mut inst = inst;
                inst.operands[0] = Operand::IdRef(chain);
                rewritten.push(inst);
                continue;
            }
            if let Some(widened) = result.and_then(|id| widened_entry.get(&id)) {
                let index = group.entries[&result.expect("entry result")];
                rewritten.push(inst);
                rewritten.push(Instruction::new(
                    Op::UConvert,
                    Some(group.index_type),
                    Some(*widened),
                    vec![Operand::IdRef(index)],
                ));
                continue;
            }
            rewritten.push(inst);
        }
        block.instructions = rewritten;
    }
    true
}

fn integer_type_width(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<u32> {
    let def = defs.get(&ty)?;
    if def.class.opcode != Op::TypeInt {
        return None;
    }
    match def.operands.first()? {
        Operand::LiteralBit32(width) => Some(*width),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};

    fn inst(
        op: Op,
        result_type: Option<Word>,
        result_id: Option<Word>,
        operands: Vec<Operand>,
    ) -> Instruction {
        Instruction::new(op, result_type, result_id, operands)
    }

    fn mixed_width_pointer_walk(index_type: Word, delta_type: Word) -> Module {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(40));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(1)],
            ),
            inst(
                Op::TypeFloat,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(5), vec![Operand::IdRef(4)]),
            inst(Op::TypeStruct, None, Some(6), vec![Operand::IdRef(5)]),
            inst(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(6),
                ],
            ),
            inst(
                Op::Variable,
                Some(7),
                Some(8),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(2),
                Some(11),
                vec![Operand::LiteralBit32(1), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(12),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(3),
                Some(13),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        let entry_index = match index_type {
            1 => 10,
            2 => 11,
            _ => 13,
        };
        let delta = if delta_type == 2 { 11 } else { 12 };
        let mut entry = Block::new();
        entry.label = Some(inst(Op::Label, None, Some(20), vec![]));
        entry.instructions = vec![
            inst(
                Op::AccessChain,
                Some(9),
                Some(30),
                vec![
                    Operand::IdRef(8),
                    Operand::IdRef(10),
                    Operand::IdRef(entry_index),
                ],
            ),
            inst(Op::Branch, None, None, vec![Operand::IdRef(21)]),
        ];
        let mut body = Block::new();
        body.label = Some(inst(Op::Label, None, Some(21), vec![]));
        body.instructions = vec![
            inst(
                Op::Phi,
                Some(9),
                Some(31),
                vec![
                    Operand::IdRef(30),
                    Operand::IdRef(20),
                    Operand::IdRef(32),
                    Operand::IdRef(21),
                ],
            ),
            inst(
                Op::PtrAccessChain,
                Some(9),
                Some(32),
                vec![Operand::IdRef(31), Operand::IdRef(delta)],
            ),
            inst(Op::Store, None, None, vec![Operand::IdRef(31)]),
            inst(Op::Branch, None, None, vec![Operand::IdRef(21)]),
        ];
        let mut function = Function::new();
        function.blocks = vec![entry, body];
        module.functions = vec![function];
        module
    }

    fn pointer_phi_count(module: &Module) -> usize {
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|inst| inst.class.opcode == Op::Phi && inst.result_type == Some(9))
            .count()
    }

    fn instructions_of(module: &Module) -> Vec<&Instruction> {
        module.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .collect()
    }

    #[test]
    fn a_narrow_entry_index_widens_to_the_step_delta() {
        let mut module = mixed_width_pointer_walk(1, 2);
        assert_eq!(lower_storage_buffer_pointer_phis(&mut module), 1);
        assert_eq!(pointer_phi_count(&module), 0);
        let instructions = instructions_of(&module);
        assert!(instructions.iter().any(|inst| {
            inst.class.opcode == Op::UConvert
                && inst.result_type == Some(2)
                && inst.operands == [Operand::IdRef(10)]
        }));
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::Phi && inst.result_type == Some(2)));
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::IAdd && inst.result_type == Some(2)));
    }

    #[test]
    fn a_narrow_step_delta_widens_to_the_entry_index() {
        let mut module = mixed_width_pointer_walk(2, 1);
        assert_eq!(lower_storage_buffer_pointer_phis(&mut module), 1);
        assert_eq!(pointer_phi_count(&module), 0);
        let instructions = instructions_of(&module);
        assert!(instructions.iter().any(|inst| {
            inst.class.opcode == Op::UConvert
                && inst.result_type == Some(2)
                && inst.operands == [Operand::IdRef(12)]
        }));
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::IAdd && inst.result_type == Some(2)));
    }

    #[test]
    fn a_signed_narrow_index_is_declined() {
        let mut module = mixed_width_pointer_walk(3, 2);
        let before = module.clone();
        assert_eq!(lower_storage_buffer_pointer_phis(&mut module), 0);
        assert_eq!(pointer_phi_count(&module), 1);
        assert_eq!(
            instructions_of(&module).len(),
            instructions_of(&before).len(),
            "a declined walk must not mutate"
        );
    }

    #[test]
    fn zero_stride_storage_buffer_ptr_access_chain_demotes_to_access_chain() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(2),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(3),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(1)]),
            inst(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            inst(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(5),
                ],
            ),
            inst(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
        ];
        let mut function = Function::new();
        let mut block = Block::new();
        block.instructions.push(inst(
            Op::PtrAccessChain,
            Some(8),
            Some(9),
            vec![Operand::IdRef(7), Operand::IdRef(2), Operand::IdRef(3)],
        ));
        function.blocks.push(block);
        module.functions.push(function);

        assert_eq!(
            lower_zero_base_storage_buffer_ptr_access_chains(&mut module),
            1
        );
        assert_eq!(
            module.functions[0].blocks[0].instructions[0].class.opcode,
            Op::AccessChain
        );
    }

    #[test]
    fn storage_buffer_atomic_workgroup_scope_rewrites_to_device_relaxed() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(2),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(3),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(4),
                vec![Operand::LiteralBit32(2)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(5),
                vec![Operand::LiteralBit32(264)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        let mut function = Function::new();
        let mut block = Block::new();
        block.instructions.push(inst(
            Op::AtomicOr,
            Some(1),
            Some(8),
            vec![
                Operand::IdRef(7),
                Operand::IdScope(4),
                Operand::IdMemorySemantics(5),
                Operand::IdRef(3),
            ],
        ));
        function.blocks.push(block);
        module.functions.push(function);

        assert_eq!(rewrite_storage_buffer_atomic_scopes(&mut module), 2);
        let operands = &module.functions[0].blocks[0].instructions[0].operands;
        assert_eq!(operands[1], Operand::IdScope(3));
        assert_eq!(operands[2], Operand::IdMemorySemantics(2));
    }
}
