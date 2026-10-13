use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Instruction;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use spirv::{Capability, Decoration, MemoryModel, Op, StorageClass, Word};
use std::collections::VecDeque;

struct Leaf {
    id: Word,
    var: Word,
    pointee: Word,
    element: Option<Word>,
}

struct PsbDiscovery {
    type_defs: HashMap<Word, Instruction>,
    var_storage: HashMap<Word, StorageClass>,
    var_pointee: HashMap<Word, Word>,
    value_type: HashMap<Word, Word>,
    cross_binding_merges: Vec<Word>,
    has_cross_binding_phi: bool,
    closure: HashSet<Word>,
    closure_values: Vec<Word>,
    leaves: Vec<Leaf>,
    buffer_slot: HashMap<Word, u32>,
    const_ids: HashSet<Word>,
}

fn element_allocation_stride(
    module: &Module,
    type_defs: &HashMap<Word, Instruction>,
    ty: Word,
) -> Option<u32> {
    let mut declared = None;
    for annotation in &module.annotations {
        if annotation.class.opcode != Op::Decorate {
            continue;
        }
        let [Operand::IdRef(target), Operand::Decoration(Decoration::ArrayStride), Operand::LiteralBit32(stride)] =
            annotation.operands.as_slice()
        else {
            continue;
        };
        let Some(definition) = type_defs.get(target) else {
            continue;
        };
        let carries_ty = match definition.class.opcode {
            Op::TypePointer => definition.operands.get(1) == Some(&Operand::IdRef(ty)),
            Op::TypeArray | Op::TypeRuntimeArray => {
                definition.operands.first() == Some(&Operand::IdRef(ty))
            }
            _ => false,
        };
        if !carries_ty {
            continue;
        }
        match declared {
            Some(existing) if existing != *stride => return None,
            _ => declared = Some(*stride),
        }
    }
    if declared.is_some() {
        return declared;
    }

    let definition = type_defs.get(&ty)?;
    let store_size = match definition.class.opcode {
        Op::TypeInt | Op::TypeFloat => match definition.operands.first()? {
            Operand::LiteralBit32(bits) => bits.div_ceil(8),
            _ => return None,
        },
        Op::TypeVector => {
            let (Operand::IdRef(component), Operand::LiteralBit32(count)) =
                (definition.operands.first()?, definition.operands.get(1)?)
            else {
                return None;
            };
            let component = type_defs.get(component)?;
            let bits = match component.class.opcode {
                Op::TypeInt | Op::TypeFloat => match component.operands.first()? {
                    Operand::LiteralBit32(bits) => *bits,
                    _ => return None,
                },
                _ => return None,
            };
            bits.div_ceil(8).checked_mul(*count)?
        }
        _ => return None,
    };
    store_size.max(1).checked_next_power_of_two()
}

fn ensure_physical_memory_alignment(instruction: &mut Instruction, required: u32) {
    let Some(memory_access_index) = instruction
        .operands
        .iter()
        .position(|operand| matches!(operand, Operand::MemoryAccess(_)))
    else {
        instruction
            .operands
            .push(Operand::MemoryAccess(spirv::MemoryAccess::ALIGNED));
        instruction.operands.push(Operand::LiteralBit32(required));
        return;
    };
    let Operand::MemoryAccess(memory_access) = instruction.operands[memory_access_index] else {
        unreachable!();
    };
    if memory_access.contains(spirv::MemoryAccess::ALIGNED) {
        if let Some(Operand::LiteralBit32(alignment)) =
            instruction.operands.get_mut(memory_access_index + 1)
        {
            *alignment = (*alignment).max(required);
        }
    } else {
        instruction.operands[memory_access_index] =
            Operand::MemoryAccess(memory_access | spirv::MemoryAccess::ALIGNED);
        instruction
            .operands
            .insert(memory_access_index + 1, Operand::LiteralBit32(required));
    }
}

fn discover_cross_binding_psb(module: &Module) -> Option<PsbDiscovery> {
    let type_defs: HashMap<Word, Instruction> = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i.clone())))
        .collect();
    let var_storage: HashMap<Word, StorageClass> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Variable)
        .filter_map(|i| {
            let id = i.result_id?;
            match i.operands.first()? {
                Operand::StorageClass(s) => Some((id, *s)),
                _ => None,
            }
        })
        .collect();
    let var_binding: HashMap<Word, u32> = module
        .annotations
        .iter()
        .filter(|i| {
            i.class.opcode == Op::Decorate
                && matches!(
                    i.operands.get(1),
                    Some(Operand::Decoration(Decoration::Binding))
                )
        })
        .filter_map(|i| match (i.operands.first()?, i.operands.get(2)?) {
            (Operand::IdRef(id), Operand::LiteralBit32(b)) => Some((*id, *b)),
            _ => None,
        })
        .collect();

    let ptr_info = |ty: Word| -> Option<(StorageClass, Word)> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypePointer {
            return None;
        }
        match (inst.operands.first()?, inst.operands.get(1)?) {
            (Operand::StorageClass(s), Operand::IdRef(pointee)) => Some((*s, *pointee)),
            _ => None,
        }
    };

    let var_pointee: HashMap<Word, Word> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Variable)
        .filter_map(|i| {
            let id = i.result_id?;
            let ty = i.result_type?;
            let (_, pointee) = ptr_info(ty)?;
            Some((id, pointee))
        })
        .collect();
    let is_buffer_var =
        |id: Word| -> bool { var_storage.get(&id) == Some(&StorageClass::StorageBuffer) };

    let mut value_def: HashMap<Word, Instruction> = HashMap::default();
    let mut value_type: HashMap<Word, Word> = HashMap::default();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                if let Some(rid) = inst.result_id {
                    if let Some(rty) = inst.result_type {
                        if ptr_info(rty)
                            .is_some_and(|(storage, _)| storage == StorageClass::StorageBuffer)
                        {
                            value_type.insert(rid, rty);
                            value_def.insert(rid, inst.clone());
                        }
                    }
                }
            }
        }
    }

    let is_sb_pointer = |id: Word| -> bool {
        value_type
            .get(&id)
            .and_then(|t| ptr_info(*t))
            .map(|(sc, _)| sc == StorageClass::StorageBuffer)
            .unwrap_or(false)
    };
    let pointer_operands = |inst: &Instruction| -> Vec<Word> {
        match inst.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain => inst
                .operands
                .first()
                .and_then(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .into_iter()
                .collect(),
            Op::Select => inst.operands[1..]
                .iter()
                .filter_map(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .collect(),
            Op::Phi => inst
                .operands
                .chunks(2)
                .filter_map(|c| match c.first() {
                    Some(Operand::IdRef(v)) => Some(*v),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .collect(),
            Op::CopyObject => inst
                .operands
                .first()
                .and_then(|o| match o {
                    Operand::IdRef(b) => Some(*b),
                    _ => None,
                })
                .filter(|id| is_sb_pointer(*id) || is_buffer_var(*id))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RootState {
        None,
        One(Word),
        Multiple,
    }
    impl RootState {
        fn union(self, other: Self) -> Self {
            match (self, other) {
                (Self::Multiple, _) | (_, Self::Multiple) => Self::Multiple,
                (Self::None, state) | (state, Self::None) => state,
                (Self::One(left), Self::One(right)) if left == right => Self::One(left),
                (Self::One(_), Self::One(_)) => Self::Multiple,
            }
        }
    }
    let dependencies = value_def
        .iter()
        .filter(|(id, _)| is_sb_pointer(**id))
        .map(|(id, definition)| (*id, pointer_operands(definition)))
        .collect::<HashMap<_, _>>();
    let mut dependents = HashMap::<Word, Vec<Word>>::default();
    for (&value, operands) in &dependencies {
        for &operand in operands {
            dependents.entry(operand).or_default().push(value);
        }
    }
    let mut root_states = var_storage
        .iter()
        .filter_map(|(&id, &storage)| {
            (storage == StorageClass::StorageBuffer).then_some((id, RootState::One(id)))
        })
        .collect::<HashMap<_, _>>();
    root_states.extend(dependencies.keys().map(|&id| (id, RootState::None)));
    let mut root_worklist = dependencies.keys().copied().collect::<VecDeque<_>>();
    while let Some(value) = root_worklist.pop_front() {
        let next = dependencies.get(&value).into_iter().flatten().fold(
            RootState::None,
            |state, operand| {
                state.union(root_states.get(operand).copied().unwrap_or(RootState::None))
            },
        );
        if root_states.get(&value).copied() != Some(next) {
            root_states.insert(value, next);
            root_worklist.extend(dependents.get(&value).into_iter().flatten().copied());
        }
    }

    let mut cross_binding_merges: Vec<Word> = Vec::new();
    for (id, def) in &value_def {
        if !matches!(def.class.opcode, Op::Select | Op::Phi) {
            continue;
        }
        if !is_sb_pointer(*id) {
            continue;
        }
        if root_states.get(id) == Some(&RootState::Multiple) {
            cross_binding_merges.push(*id);
        }
    }
    cross_binding_merges.sort_unstable();
    if cross_binding_merges.is_empty() {
        return None;
    }

    let mut children: HashMap<Word, Vec<Word>> = HashMap::default();
    let mut parents: HashMap<Word, Vec<Word>> = HashMap::default();
    for (id, def) in &value_def {
        if !is_sb_pointer(*id) {
            continue;
        }
        let is_merge = matches!(def.class.opcode, Op::Select | Op::Phi | Op::CopyObject);
        let ops = pointer_operands(def);
        for p in &ops {
            if is_sb_pointer(*p) || (is_merge && is_buffer_var(*p)) {
                children.entry(*id).or_default().push(*p);
                parents.entry(*p).or_default().push(*id);
            }
        }
    }
    let mut closure: HashSet<Word> = HashSet::default();
    let mut stack: Vec<Word> = cross_binding_merges.clone();
    while let Some(v) = stack.pop() {
        if !closure.insert(v) {
            continue;
        }
        for n in children.get(&v).into_iter().flatten() {
            stack.push(*n);
        }
        for n in parents.get(&v).into_iter().flatten() {
            stack.push(*n);
        }
    }
    let mut closure_values: Vec<Word> = closure.iter().copied().collect();
    closure_values.sort_unstable();

    let const_zero_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| i.class.opcode == Op::Constant)
        .filter(|i| matches!(i.operands.first(), Some(Operand::LiteralBit32(0))))
        .filter_map(|i| i.result_id)
        .collect();
    let const_ids: HashSet<Word> = module
        .types_global_values
        .iter()
        .filter(|i| matches!(i.class.opcode, Op::Constant | Op::ConstantNull))
        .filter_map(|i| i.result_id)
        .collect();

    let mut leaves: Vec<Leaf> = Vec::new();
    let mut buffer_slot: HashMap<Word, u32> = HashMap::default();
    for &v in &closure_values {
        if is_buffer_var(v) {
            let &pointee = var_pointee.get(&v)?;
            if let std::collections::hash_map::Entry::Vacant(e) = buffer_slot.entry(v) {
                let &binding = var_binding.get(&v)?;
                e.insert(binding);
            }
            leaves.push(Leaf {
                id: v,
                var: v,
                pointee,
                element: None,
            });
            continue;
        }
        let def = value_def.get(&v).unwrap();
        match def.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain if matches!(def.operands.first(), Some(Operand::IdRef(b)) if is_buffer_var(*b)) =>
            {
                let Some(Operand::IdRef(base)) = def.operands.first() else {
                    return None;
                };
                if var_storage.get(base) != Some(&StorageClass::StorageBuffer) {
                    return None;
                }
                let indices = &def.operands[1..];
                if indices.is_empty() {
                    return None;
                }
                let prefix_zero = indices[..indices.len() - 1].iter().all(|o| match o {
                    Operand::IdRef(idx) => const_zero_ids.contains(idx),
                    _ => false,
                });
                if !prefix_zero {
                    return None;
                }
                let Operand::IdRef(last) = indices[indices.len() - 1] else {
                    return None;
                };
                let element = (!const_zero_ids.contains(&last)).then_some(last);
                let pointee = match value_type.get(&v).and_then(|t| ptr_info(*t)) {
                    Some((_, p)) => p,
                    None => return None,
                };
                element_allocation_stride(module, &type_defs, pointee)?;
                if !buffer_slot.contains_key(base) {
                    let &binding = var_binding.get(base)?;
                    buffer_slot.insert(*base, binding);
                }
                leaves.push(Leaf {
                    id: v,
                    var: *base,
                    pointee,
                    element,
                });
            }
            Op::AccessChain | Op::InBoundsAccessChain => {}
            Op::Select | Op::Phi | Op::PtrAccessChain | Op::CopyObject | Op::Undef => {}
            _ => {
                return None;
            }
        }
    }
    if leaves.is_empty() {
        return None;
    }

    let is_atomic_ptr_op = |op: Op| -> bool {
        matches!(
            op,
            Op::AtomicLoad
                | Op::AtomicStore
                | Op::AtomicExchange
                | Op::AtomicCompareExchange
                | Op::AtomicCompareExchangeWeak
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
                | Op::AtomicFAddEXT
                | Op::AtomicFMinEXT
                | Op::AtomicFMaxEXT
        )
    };

    let mut load_store_ptr_uses: HashSet<Word> = HashSet::default();
    for function in &module.functions {
        for block in &function.blocks {
            for inst in &block.instructions {
                let in_closure_def = inst
                    .result_id
                    .map(|r| closure.contains(&r))
                    .unwrap_or(false);
                match inst.class.opcode {
                    Op::Load => {
                        if let Some(Operand::IdRef(p)) = inst.operands.first() {
                            if closure.contains(p) {
                                load_store_ptr_uses.insert(inst.result_id.unwrap_or(0));
                                continue;
                            }
                        }
                    }
                    Op::Store => {
                        if let Some(Operand::IdRef(p)) = inst.operands.first() {
                            if closure.contains(p) {
                                continue;
                            }
                        }
                    }
                    op if is_atomic_ptr_op(op) => {
                        if let Some(Operand::IdRef(p)) = inst.operands.first() {
                            if closure.contains(p) {
                                continue;
                            }
                        }
                    }
                    _ => {}
                }
                if !in_closure_def {
                    for op in &inst.operands {
                        if let Operand::IdRef(id) = op {
                            if closure.contains(id) && !is_buffer_var(*id) {
                                return None;
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = load_store_ptr_uses;

    let has_cross_binding_phi = cross_binding_merges.iter().any(|id| {
        value_def
            .get(id)
            .is_some_and(|inst| inst.class.opcode == Op::Phi)
    });

    Some(PsbDiscovery {
        type_defs,
        var_storage,
        var_pointee,
        value_type,
        cross_binding_merges,
        has_cross_binding_phi,
        closure,
        closure_values,
        leaves,
        buffer_slot,
        const_ids,
    })
}

#[cfg(test)]
pub(super) fn rewrite_cross_binding_pointer_merges(module: &mut Module) -> bool {
    construct_cross_binding_pointer_merges_with_layout(
        module,
        crate::reflect::DescriptorLayout::default(),
    )
    .is_some()
}

pub(super) fn construct_cross_binding_pointer_merges_with_layout(
    module: &mut Module,
    layout: crate::reflect::DescriptorLayout,
) -> Option<Word> {
    rewrite_cross_binding_pointer_merges_inner(module, false, layout)
}

#[cfg(test)]
pub(super) fn rewrite_cross_binding_pointer_phis(module: &mut Module) -> bool {
    construct_cross_binding_pointer_phis_with_layout(
        module,
        crate::reflect::DescriptorLayout::default(),
    )
    .is_some()
}

pub(super) fn construct_cross_binding_pointer_phis_with_layout(
    module: &mut Module,
    layout: crate::reflect::DescriptorLayout,
) -> Option<Word> {
    rewrite_cross_binding_pointer_merges_inner(module, true, layout)
}

#[cfg(test)]
pub(super) fn has_cross_binding_pointer_phi(module: &Module) -> bool {
    discover_cross_binding_psb(module).is_some_and(|discovery| discovery.has_cross_binding_phi)
}

fn rewrite_cross_binding_pointer_merges_inner(
    module: &mut Module,
    require_cross_binding_phi: bool,
    layout: crate::reflect::DescriptorLayout,
) -> Option<Word> {
    let storage_buffer_pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|instruction| {
            (instruction.class.opcode == Op::TypePointer
                && instruction.operands.first()
                    == Some(&Operand::StorageClass(StorageClass::StorageBuffer)))
            .then_some(instruction.result_id?)
        })
        .collect::<HashSet<_>>();
    let has_pointer_merge = module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(instruction.class.opcode, Op::Phi | Op::Select)
                && instruction
                    .result_type
                    .is_some_and(|ty| storage_buffer_pointer_types.contains(&ty))
        });
    if !has_pointer_merge {
        return None;
    }
    let discovery = discover_cross_binding_psb(module)?;
    if require_cross_binding_phi && !discovery.has_cross_binding_phi {
        return None;
    }
    let occupied = crate::spirv_module::descriptor_bindings_in_set(module, layout.set);
    let address_table_binding = (layout.synthetic.start..layout.synthetic.end)
        .find(|binding| !occupied.contains(binding))?;
    let PsbDiscovery {
        type_defs,
        var_storage,
        var_pointee,
        value_type,
        cross_binding_merges,
        closure,
        closure_values,
        leaves,
        buffer_slot,
        const_ids,
        ..
    } = discovery;
    let ptr_info = |ty: Word| -> Option<(StorageClass, Word)> {
        let inst = type_defs.get(&ty)?;
        if inst.class.opcode != Op::TypePointer {
            return None;
        }
        match (inst.operands.first()?, inst.operands.get(1)?) {
            (Operand::StorageClass(s), Operand::IdRef(pointee)) => Some((*s, *pointee)),
            _ => None,
        }
    };
    let is_buffer_var =
        |id: Word| -> bool { var_storage.get(&id) == Some(&StorageClass::StorageBuffer) };
    let element_strides = type_defs
        .keys()
        .filter_map(|ty| {
            element_allocation_stride(module, &type_defs, *ty).map(|stride| (*ty, stride))
        })
        .collect::<HashMap<_, _>>();
    let scalar_align = |ty: Word| -> Option<u32> {
        let inst = type_defs.get(&ty)?;
        let scalar = match inst.class.opcode {
            Op::TypeInt | Op::TypeFloat => ty,
            Op::TypeVector => match inst.operands.first()? {
                Operand::IdRef(elem) => *elem,
                _ => return None,
            },
            _ => return None,
        };
        match type_defs.get(&scalar)?.operands.first()? {
            Operand::LiteralBit32(bits) => Some(bits / 8),
            _ => None,
        }
    };

    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(0);
    let mut fresh = || {
        let id = next_id;
        next_id += 1;
        id
    };

    fn find_int_ty(module: &Module, bits: u32) -> Option<Word> {
        module.types_global_values.iter().find_map(|i| {
            if i.class.opcode == Op::TypeInt
                && matches!(i.operands.first(), Some(Operand::LiteralBit32(b)) if *b == bits)
                && matches!(i.operands.get(1), Some(Operand::LiteralBit32(0)))
            {
                i.result_id
            } else {
                None
            }
        })
    }
    fn find_ptr_ty(module: &Module, storage: StorageClass, pointee: Word) -> Option<Word> {
        module.types_global_values.iter().find_map(|i| {
            (i.class.opcode == Op::TypePointer
                && i.operands.first() == Some(&Operand::StorageClass(storage))
                && i.operands.get(1) == Some(&Operand::IdRef(pointee)))
            .then_some(i.result_id)
            .flatten()
        })
    }
    let uint_ty = match find_int_ty(module, 32) {
        Some(id) => id,
        None => {
            let id = fresh();
            module.types_global_values.push(Instruction::new(
                Op::TypeInt,
                None,
                Some(id),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ));
            id
        }
    };
    let ulong_ty = match find_int_ty(module, 64) {
        Some(id) => id,
        None => {
            let id = fresh();
            module.types_global_values.push(Instruction::new(
                Op::TypeInt,
                None,
                Some(id),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ));
            id
        }
    };
    let uint_const = |module: &mut Module, fresh: &mut dyn FnMut() -> Word, v: u32| -> Word {
        if let Some(existing) = module.types_global_values.iter().find_map(|i| {
            (i.class.opcode == Op::Constant
                && i.result_type == Some(uint_ty)
                && matches!(i.operands.first(), Some(Operand::LiteralBit32(b)) if *b == v))
            .then_some(i.result_id)
            .flatten()
        }) {
            return existing;
        }
        let id = fresh();
        module.types_global_values.push(Instruction::new(
            Op::Constant,
            Some(uint_ty),
            Some(id),
            vec![Operand::LiteralBit32(v)],
        ));
        id
    };

    let mut psb_ptr: HashMap<Word, Word> = HashMap::default();
    for leaf in &leaves {
        if psb_ptr.contains_key(&leaf.pointee) {
            continue;
        }
        let id = fresh();
        module.types_global_values.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(id),
            vec![
                Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                Operand::IdRef(leaf.pointee),
            ],
        ));
        if let Some(stride) = element_strides.get(&leaf.pointee).copied() {
            module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(id),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(stride),
                ],
            ));
        }
        psb_ptr.insert(leaf.pointee, id);
    }
    let mut retype: HashMap<Word, Word> = HashMap::default();
    for &v in &closure_values {
        if is_buffer_var(v) {
            continue;
        }
        let pointee = match value_type.get(&v).and_then(|t| ptr_info(*t)) {
            Some((_, p)) => p,
            None => return None,
        };
        let psb = match psb_ptr.get(&pointee) {
            Some(p) => *p,
            None => {
                let id = fresh();
                module.types_global_values.push(Instruction::new(
                    Op::TypePointer,
                    None,
                    Some(id),
                    vec![
                        Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                        Operand::IdRef(pointee),
                    ],
                ));
                if let Some(stride) = element_strides.get(&pointee).copied() {
                    module.annotations.push(Instruction::new(
                        Op::Decorate,
                        None,
                        None,
                        vec![
                            Operand::IdRef(id),
                            Operand::Decoration(Decoration::ArrayStride),
                            Operand::LiteralBit32(stride),
                        ],
                    ));
                }
                psb_ptr.insert(pointee, id);
                id
            }
        };
        retype.insert(v, psb);
    }

    let all_value_types = module
        .all_inst_iter()
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    let all_value_defs = module
        .all_inst_iter()
        .filter_map(|instruction| Some((instruction.result_id?, instruction.clone())))
        .collect::<HashMap<_, _>>();
    let memory_view_pointees = crate::emission_order::dedup_in_encounter_order(
        module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| {
                let Some(Operand::IdRef(pointer)) = instruction.operands.first() else {
                    return None;
                };
                if !closure.contains(pointer) {
                    return None;
                }
                let accessed_type = match instruction.class.opcode {
                    Op::Load => instruction.result_type,
                    Op::Store => instruction
                        .operands
                        .get(1)
                        .and_then(|operand| match operand {
                            Operand::IdRef(object) => all_value_types.get(object).copied(),
                            _ => None,
                        }),
                    _ => None,
                };
                let pointer_pointee = value_type
                    .get(pointer)
                    .and_then(|ty| ptr_info(*ty))
                    .map(|(_, pointee)| pointee);
                (accessed_type != pointer_pointee)
                    .then_some(accessed_type)
                    .flatten()
            }),
    );
    for pointee in memory_view_pointees {
        if psb_ptr.contains_key(&pointee) {
            continue;
        }
        let id = fresh();
        module.types_global_values.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(id),
            vec![
                Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                Operand::IdRef(pointee),
            ],
        ));
        psb_ptr.insert(pointee, id);
    }

    let mut nullish_retype_requests = Vec::new();
    for (function_index, function) in module.functions.iter().enumerate() {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let Some(result) = instruction.result_id else {
                    continue;
                };
                let Some(&new_type) = retype.get(&result) else {
                    continue;
                };
                let pointer_arm_indices = match instruction.class.opcode {
                    Op::Select => (1..instruction.operands.len()).collect::<Vec<_>>(),
                    Op::Phi => (0..instruction.operands.len()).step_by(2).collect(),
                    Op::CopyObject => vec![0],
                    _ => continue,
                };
                for index in pointer_arm_indices {
                    let Some(Operand::IdRef(source)) = instruction.operands.get(index) else {
                        continue;
                    };
                    let Some(definition) = all_value_defs.get(source) else {
                        continue;
                    };
                    if matches!(definition.class.opcode, Op::Undef | Op::ConstantNull)
                        && definition.result_type != Some(new_type)
                    {
                        nullish_retype_requests.push((function_index, *source, new_type));
                    }
                }
            }
        }
    }
    nullish_retype_requests
        .sort_unstable_by_key(|(function, source, ty)| (*function, *source, *ty));
    nullish_retype_requests.dedup();
    let uint_zero = uint_const(module, &mut fresh, 0);
    let mut nullish_retypes = HashMap::default();
    let mut nullish_conversions = vec![Vec::new(); module.functions.len()];
    let mut nullish_zero_addresses = vec![None; module.functions.len()];
    for (function_index, source, new_type) in nullish_retype_requests {
        let zero_address = match nullish_zero_addresses[function_index] {
            Some(id) => id,
            None => {
                let id = fresh();
                nullish_conversions[function_index].push(Instruction::new(
                    Op::UConvert,
                    Some(ulong_ty),
                    Some(id),
                    vec![Operand::IdRef(uint_zero)],
                ));
                nullish_zero_addresses[function_index] = Some(id);
                id
            }
        };
        let replacement = fresh();
        nullish_conversions[function_index].push(Instruction::new(
            Op::ConvertUToPtr,
            Some(new_type),
            Some(replacement),
            vec![Operand::IdRef(zero_address)],
        ));
        nullish_retypes.insert((function_index, source, new_type), replacement);
    }
    for (function, conversions) in module.functions.iter_mut().zip(nullish_conversions) {
        if conversions.is_empty() {
            continue;
        }
        let entry = function.blocks.first_mut()?;
        let insertion = entry
            .instructions
            .iter()
            .position(|instruction| {
                !matches!(
                    instruction.class.opcode,
                    Op::Variable | Op::Line | Op::NoLine
                )
            })
            .unwrap_or(entry.instructions.len());
        entry.instructions.splice(insertion..insertion, conversions);
    }

    let addr_rt = fresh();
    module.types_global_values.push(Instruction::new(
        Op::TypeRuntimeArray,
        None,
        Some(addr_rt),
        vec![Operand::IdRef(ulong_ty)],
    ));
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(addr_rt),
            Operand::Decoration(Decoration::ArrayStride),
            Operand::LiteralBit32(8),
        ],
    ));
    let addr_struct = fresh();
    module.types_global_values.push(Instruction::new(
        Op::TypeStruct,
        None,
        Some(addr_struct),
        vec![Operand::IdRef(addr_rt)],
    ));
    module.annotations.push(Instruction::new(
        Op::MemberDecorate,
        None,
        None,
        vec![
            Operand::IdRef(addr_struct),
            Operand::LiteralBit32(0),
            Operand::Decoration(Decoration::Offset),
            Operand::LiteralBit32(0),
        ],
    ));
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(addr_struct),
            Operand::Decoration(Decoration::Block),
        ],
    ));
    let ptr_sb_struct = fresh();
    module.types_global_values.push(Instruction::new(
        Op::TypePointer,
        None,
        Some(ptr_sb_struct),
        vec![
            Operand::StorageClass(StorageClass::StorageBuffer),
            Operand::IdRef(addr_struct),
        ],
    ));
    let ptr_sb_u64 = match find_ptr_ty(module, StorageClass::StorageBuffer, ulong_ty) {
        Some(id) => id,
        None => {
            let id = fresh();
            module.types_global_values.push(Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(ulong_ty),
                ],
            ));
            id
        }
    };
    let addr_var = fresh();
    module.types_global_values.push(Instruction::new(
        Op::Variable,
        Some(ptr_sb_struct),
        Some(addr_var),
        vec![Operand::StorageClass(StorageClass::StorageBuffer)],
    ));
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(addr_var),
            Operand::Decoration(Decoration::DescriptorSet),
            Operand::LiteralBit32(layout.set),
        ],
    ));
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(addr_var),
            Operand::Decoration(Decoration::Binding),
            Operand::LiteralBit32(address_table_binding),
        ],
    ));
    let uses_full_interface = module
        .header
        .as_ref()
        .map(|h| h.version() >= (1, 4))
        .unwrap_or(false);
    if uses_full_interface {
        for ep in &mut module.entry_points {
            ep.operands.push(Operand::IdRef(addr_var));
        }
    }

    let uint_zero = uint_const(module, &mut fresh, 0);
    let mut slots: Vec<u32> = buffer_slot.values().copied().collect();
    slots.sort_unstable();
    slots.dedup();
    let mut slot_consts: HashMap<u32, Word> = HashMap::default();
    for slot in slots {
        slot_consts
            .entry(slot)
            .or_insert_with(|| uint_const(module, &mut fresh, slot));
    }
    let leaf_map: HashMap<Word, &Leaf> = leaves.iter().map(|l| (l.id, l)).collect();

    let mut var_base: HashMap<Word, Word> = HashMap::default();
    let whole_buffer_leaves: Vec<Word> = leaves
        .iter()
        .filter(|l| is_buffer_var(l.id))
        .map(|l| l.id)
        .collect();
    if !whole_buffer_leaves.is_empty() {
        let mut prelude: Vec<Instruction> = Vec::new();
        for &var in &whole_buffer_leaves {
            let slot = *buffer_slot.get(&var).unwrap();
            let slot_c = *slot_consts.get(&slot).unwrap();
            let pointee = *var_pointee.get(&var).unwrap();
            let psb_struct = *psb_ptr.get(&pointee).unwrap();
            let ac = fresh();
            prelude.push(Instruction::new(
                Op::AccessChain,
                Some(ptr_sb_u64),
                Some(ac),
                vec![
                    Operand::IdRef(addr_var),
                    Operand::IdRef(uint_zero),
                    Operand::IdRef(slot_c),
                ],
            ));
            let addr = fresh();
            prelude.push(Instruction::new(
                Op::Load,
                Some(ulong_ty),
                Some(addr),
                vec![Operand::IdRef(ac)],
            ));
            let base = fresh();
            prelude.push(Instruction::new(
                Op::ConvertUToPtr,
                Some(psb_struct),
                Some(base),
                vec![Operand::IdRef(addr)],
            ));
            var_base.insert(var, base);
        }
        let merge_fn = module.functions.iter_mut().find(|f| {
            f.blocks.iter().any(|b| {
                b.instructions.iter().any(|i| {
                    i.result_id
                        .is_some_and(|r| cross_binding_merges.contains(&r))
                })
            })
        });
        if let Some(func) = merge_fn {
            if let Some(block) = func.blocks.first_mut() {
                let at = block
                    .instructions
                    .iter()
                    .position(|i| i.class.opcode != Op::Variable)
                    .unwrap_or(0);
                for (k, inst) in prelude.into_iter().enumerate() {
                    block.instructions.insert(at + k, inst);
                }
            }
        }
    }

    let closure_align: HashMap<Word, u32> = closure_values
        .iter()
        .filter_map(|p| {
            let pointee = value_type
                .get(p)
                .and_then(|t| ptr_info(*t))
                .map(|(_, pe)| pe)?;
            scalar_align(pointee).map(|a| (*p, a))
        })
        .collect();

    for (function_index, function) in module.functions.iter_mut().enumerate() {
        for block in &mut function.blocks {
            let mut new_insts: Vec<Instruction> = Vec::with_capacity(block.instructions.len());
            for inst in block.instructions.clone() {
                let rid = inst.result_id;
                if let Some(r) = rid {
                    if let Some(leaf) = leaf_map.get(&r) {
                        let slot = *buffer_slot.get(&leaf.var).unwrap();
                        let slot_c = *slot_consts.get(&slot).unwrap();
                        let ac = fresh();
                        new_insts.push(Instruction::new(
                            Op::AccessChain,
                            Some(ptr_sb_u64),
                            Some(ac),
                            vec![
                                Operand::IdRef(addr_var),
                                Operand::IdRef(uint_zero),
                                Operand::IdRef(slot_c),
                            ],
                        ));
                        let addr = fresh();
                        new_insts.push(Instruction::new(
                            Op::Load,
                            Some(ulong_ty),
                            Some(addr),
                            vec![Operand::IdRef(ac)],
                        ));
                        let psb_pointee_ptr = *psb_ptr.get(&leaf.pointee).unwrap();
                        match leaf.element {
                            None => {
                                new_insts.push(Instruction::new(
                                    Op::ConvertUToPtr,
                                    Some(psb_pointee_ptr),
                                    Some(r),
                                    vec![Operand::IdRef(addr)],
                                ));
                            }
                            Some(element) => {
                                let base = fresh();
                                new_insts.push(Instruction::new(
                                    Op::ConvertUToPtr,
                                    Some(psb_pointee_ptr),
                                    Some(base),
                                    vec![Operand::IdRef(addr)],
                                ));
                                new_insts.push(Instruction::new(
                                    Op::PtrAccessChain,
                                    Some(psb_pointee_ptr),
                                    Some(r),
                                    vec![Operand::IdRef(base), Operand::IdRef(element)],
                                ));
                            }
                        }
                        continue;
                    }
                }
                let mut inst = inst;
                if !var_base.is_empty()
                    && matches!(inst.class.opcode, Op::Select | Op::Phi | Op::CopyObject)
                    && rid.is_some_and(|r| closure.contains(&r))
                {
                    for op in inst.operands.iter_mut() {
                        if let Operand::IdRef(id) = op {
                            if let Some(&base) = var_base.get(id) {
                                *op = Operand::IdRef(base);
                            }
                        }
                    }
                }
                if let Some(r) = rid {
                    if matches!(inst.class.opcode, Op::AccessChain | Op::InBoundsAccessChain) {
                        let base = match inst.operands.first() {
                            Some(Operand::IdRef(b)) => Some(*b),
                            _ => None,
                        };
                        let merged_pointee_is_composite = base
                            .and_then(|base| retype.get(&base).or_else(|| value_type.get(&base)))
                            .and_then(|ty| ptr_info(*ty))
                            .and_then(|(_, pointee)| type_defs.get(&pointee))
                            .is_some_and(|pointee| {
                                matches!(
                                    pointee.class.opcode,
                                    Op::TypeStruct
                                        | Op::TypeArray
                                        | Op::TypeRuntimeArray
                                        | Op::TypeVector
                                        | Op::TypeMatrix
                                )
                            });
                        let single_index = (inst.operands.len() == 2)
                            .then(|| match inst.operands[1] {
                                Operand::IdRef(idx)
                                    if !const_ids.contains(&idx)
                                        || !merged_pointee_is_composite =>
                                {
                                    Some(idx)
                                }
                                _ => None,
                            })
                            .flatten();
                        if let (Some(base), Some(idx), Some(&elem_ptr)) =
                            (base, single_index, retype.get(&r))
                        {
                            if closure.contains(&base) {
                                let addr = fresh();
                                new_insts.push(Instruction::new(
                                    Op::ConvertPtrToU,
                                    Some(ulong_ty),
                                    Some(addr),
                                    vec![Operand::IdRef(base)],
                                ));
                                let elem_base = fresh();
                                new_insts.push(Instruction::new(
                                    Op::ConvertUToPtr,
                                    Some(elem_ptr),
                                    Some(elem_base),
                                    vec![Operand::IdRef(addr)],
                                ));
                                new_insts.push(Instruction::new(
                                    Op::PtrAccessChain,
                                    Some(elem_ptr),
                                    Some(r),
                                    vec![Operand::IdRef(elem_base), Operand::IdRef(idx)],
                                ));
                                continue;
                            }
                        }
                    }
                }
                if let Some(r) = rid {
                    if let Some(new_ty) = retype.get(&r) {
                        if matches!(
                            inst.class.opcode,
                            Op::Select
                                | Op::Phi
                                | Op::PtrAccessChain
                                | Op::AccessChain
                                | Op::InBoundsAccessChain
                                | Op::CopyObject
                        ) {
                            inst.result_type = Some(*new_ty);
                            let pointer_arm_indices = match inst.class.opcode {
                                Op::Select => (1..inst.operands.len()).collect::<Vec<_>>(),
                                Op::Phi => (0..inst.operands.len()).step_by(2).collect(),
                                Op::CopyObject => vec![0],
                                _ => Vec::new(),
                            };
                            for index in pointer_arm_indices {
                                let Some(Operand::IdRef(source)) = inst.operands.get_mut(index)
                                else {
                                    continue;
                                };
                                if let Some(&replacement) =
                                    nullish_retypes.get(&(function_index, *source, *new_ty))
                                {
                                    *source = replacement;
                                }
                            }
                        }
                    }
                }
                let closure_memory_pointer = match inst.class.opcode {
                    Op::Load | Op::Store => {
                        inst.operands.first().and_then(|operand| match operand {
                            Operand::IdRef(pointer) if closure.contains(pointer) => Some(*pointer),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                let accessed_type = match inst.class.opcode {
                    Op::Load => inst.result_type,
                    Op::Store => inst.operands.get(1).and_then(|operand| match operand {
                        Operand::IdRef(object) => all_value_types.get(object).copied(),
                        _ => None,
                    }),
                    _ => None,
                };
                if let (Some(pointer), Some(accessed_type)) =
                    (closure_memory_pointer, accessed_type)
                {
                    let pointer_pointee = value_type
                        .get(&pointer)
                        .and_then(|ty| ptr_info(*ty))
                        .map(|(_, pointee)| pointee);
                    if closure.contains(&pointer) && pointer_pointee != Some(accessed_type) {
                        let byte_step = byte_element_step(
                            pointer,
                            &all_value_defs,
                            &value_type,
                            &all_value_types,
                            &type_defs,
                            ulong_ty,
                        );
                        let address = fresh();
                        if let Some((base, index)) = byte_step {
                            let base_address = fresh();
                            new_insts.push(Instruction::new(
                                Op::ConvertPtrToU,
                                Some(ulong_ty),
                                Some(base_address),
                                vec![Operand::IdRef(base)],
                            ));
                            new_insts.push(Instruction::new(
                                Op::IAdd,
                                Some(ulong_ty),
                                Some(address),
                                vec![Operand::IdRef(base_address), Operand::IdRef(index)],
                            ));
                        } else {
                            new_insts.push(Instruction::new(
                                Op::ConvertPtrToU,
                                Some(ulong_ty),
                                Some(address),
                                vec![Operand::IdRef(pointer)],
                            ));
                        }
                        let typed_pointer = fresh();
                        new_insts.push(Instruction::new(
                            Op::ConvertUToPtr,
                            Some(*psb_ptr.get(&accessed_type).unwrap()),
                            Some(typed_pointer),
                            vec![Operand::IdRef(address)],
                        ));
                        inst.operands[0] = Operand::IdRef(typed_pointer);
                    }
                }
                match inst.class.opcode {
                    Op::Load => {
                        if let Some(pointer) = closure_memory_pointer {
                            let align = inst
                                .result_type
                                .and_then(scalar_align)
                                .or_else(|| closure_align.get(&pointer).copied())
                                .unwrap_or(4);
                            ensure_physical_memory_alignment(&mut inst, align);
                        }
                    }
                    Op::Store => {
                        if let Some(pointer) = closure_memory_pointer {
                            let align = accessed_type
                                .and_then(scalar_align)
                                .or_else(|| closure_align.get(&pointer).copied())
                                .unwrap_or(4);
                            ensure_physical_memory_alignment(&mut inst, align);
                        }
                    }
                    _ => {}
                }
                new_insts.push(inst);
            }
            block.instructions = new_insts;
        }
    }

    if let Some(mm) = module.memory_model.as_mut() {
        if let Some(op) = mm.operands.first_mut() {
            *op = Operand::AddressingModel(spirv::AddressingModel::PhysicalStorageBuffer64);
        }
    }
    let has_cap = |module: &Module, c: Capability| {
        module
            .capabilities
            .iter()
            .any(|i| matches!(i.operands.first(), Some(Operand::Capability(x)) if *x == c))
    };
    for cap in [
        Capability::PhysicalStorageBufferAddresses,
        Capability::Int64,
    ] {
        if !has_cap(module, cap) {
            module.capabilities.push(Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(cap)],
            ));
        }
    }
    let has_ext = module.extensions.iter().any(|i| {
        matches!(i.operands.first(), Some(Operand::LiteralString(s)) if s == "SPV_KHR_physical_storage_buffer")
    });
    if !has_ext {
        module.extensions.push(Instruction::new(
            Op::Extension,
            None,
            None,
            vec![Operand::LiteralString(
                "SPV_KHR_physical_storage_buffer".to_string(),
            )],
        ));
    }
    let _ = MemoryModel::GLSL450;

    if let Some(header) = module.header.as_mut() {
        header.bound = next_id;
    }
    Some(addr_var)
}

fn byte_element_step(
    pointer: Word,
    value_defs: &HashMap<Word, Instruction>,
    value_type: &HashMap<Word, Word>,
    all_value_types: &HashMap<Word, Word>,
    type_defs: &HashMap<Word, Instruction>,
    ulong_ty: Word,
) -> Option<(Word, Word)> {
    let def = value_defs.get(&pointer)?;
    if !matches!(
        def.class.opcode,
        Op::PtrAccessChain | Op::InBoundsPtrAccessChain
    ) {
        return None;
    }
    let [Operand::IdRef(base), Operand::IdRef(index)] = def.operands.as_slice() else {
        return None;
    };
    if all_value_types.get(index).copied() != Some(ulong_ty) {
        return None;
    }
    let base_pointee = match type_defs.get(value_type.get(base)?)?.operands.as_slice() {
        [Operand::StorageClass(_), Operand::IdRef(pointee)] => *pointee,
        _ => return None,
    };
    let pointee = type_defs.get(&base_pointee)?;
    if pointee.class.opcode != Op::TypeInt {
        return None;
    }
    match pointee.operands.first()? {
        Operand::LiteralBit32(8) => Some((*base, *index)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, ModuleHeader};

    fn inst(op: Op, ty: Option<Word>, res: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, res, ops)
    }

    #[test]
    fn a_byte_element_step_is_recognized_and_a_wider_one_is_not() {
        let uchar = 1;
        let ulong = 2;
        let uint = 3;
        let ptr_uchar = 4;
        let ptr_ushort = 5;
        let ushort = 6;
        let type_defs = crate::fxhash::map_from([
            (
                uchar,
                inst(
                    Op::TypeInt,
                    None,
                    Some(uchar),
                    vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
                ),
            ),
            (
                ushort,
                inst(
                    Op::TypeInt,
                    None,
                    Some(ushort),
                    vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
                ),
            ),
            (
                ptr_uchar,
                inst(
                    Op::TypePointer,
                    None,
                    Some(ptr_uchar),
                    vec![
                        Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                        Operand::IdRef(uchar),
                    ],
                ),
            ),
            (
                ptr_ushort,
                inst(
                    Op::TypePointer,
                    None,
                    Some(ptr_ushort),
                    vec![
                        Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                        Operand::IdRef(ushort),
                    ],
                ),
            ),
        ]);
        let (base, index, chain) = (10, 11, 12);
        let chain_inst = inst(
            Op::PtrAccessChain,
            Some(ptr_uchar),
            Some(chain),
            vec![Operand::IdRef(base), Operand::IdRef(index)],
        );
        let value_defs = crate::fxhash::map_from([(chain, chain_inst.clone())]);
        let value_type = crate::fxhash::map_from([(base, ptr_uchar), (chain, ptr_uchar)]);
        let all_value_types = crate::fxhash::map_from([(index, ulong)]);
        assert_eq!(
            byte_element_step(
                chain,
                &value_defs,
                &value_type,
                &all_value_types,
                &type_defs,
                ulong,
            ),
            Some((base, index))
        );

        let wide_type = crate::fxhash::map_from([(base, ptr_ushort), (chain, ptr_uchar)]);
        assert_eq!(
            byte_element_step(
                chain,
                &value_defs,
                &wide_type,
                &all_value_types,
                &type_defs,
                ulong,
            ),
            None
        );
        let narrow_index = crate::fxhash::map_from([(index, uint)]);
        assert_eq!(
            byte_element_step(
                chain,
                &value_defs,
                &value_type,
                &narrow_index,
                &type_defs,
                ulong,
            ),
            None
        );

        let load = crate::fxhash::map_from([(
            chain,
            inst(
                Op::Load,
                Some(ptr_uchar),
                Some(chain),
                vec![Operand::IdRef(base)],
            ),
        )]);
        assert_eq!(
            byte_element_step(
                chain,
                &load,
                &value_type,
                &all_value_types,
                &type_defs,
                ulong,
            ),
            None
        );
    }

    #[test]
    fn a_byte_chain_address_is_taken_from_the_base_and_offset() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(40));
        module.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(2), vec![Operand::IdRef(1)]),
            inst(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(2)]),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::TypeBool, None, Some(6), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(7),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeInt,
                None,
                Some(8),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(7),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(6), Some(12), vec![]),
            inst(
                Op::Constant,
                Some(8),
                Some(13),
                vec![Operand::LiteralBit32(4), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        module.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(2),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(1),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(3), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(3),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::AccessChain,
                Some(5),
                Some(31),
                vec![Operand::IdRef(20), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(
                Op::AccessChain,
                Some(5),
                Some(32),
                vec![Operand::IdRef(21), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(
                Op::Select,
                Some(5),
                Some(33),
                vec![Operand::IdRef(12), Operand::IdRef(31), Operand::IdRef(32)],
            ),
            inst(
                Op::PtrAccessChain,
                Some(5),
                Some(34),
                vec![Operand::IdRef(33), Operand::IdRef(13)],
            ),
            inst(Op::Load, Some(7), Some(35), vec![Operand::IdRef(34)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut function = Function::new();
        function.blocks = vec![block];
        module.functions = vec![function];

        assert!(rewrite_cross_binding_pointer_merges(&mut module));
        let instructions = &module.functions[0].blocks[0].instructions;
        let converted = instructions
            .iter()
            .filter(|instruction| instruction.class.opcode == Op::ConvertPtrToU)
            .map(|instruction| instruction.operands.clone())
            .collect::<Vec<_>>();
        assert!(!converted.is_empty(), "no address was materialized");
        assert!(
            !converted.contains(&vec![Operand::IdRef(34)]),
            "the byte access chain is still converted directly: {converted:?}"
        );
        let base_address = instructions
            .iter()
            .find(|instruction| {
                instruction.class.opcode == Op::ConvertPtrToU
                    && instruction.operands == [Operand::IdRef(33)]
            })
            .and_then(|instruction| instruction.result_id)
            .expect("the select's address");
        assert!(
            instructions.iter().any(|instruction| {
                instruction.class.opcode == Op::IAdd
                    && instruction.operands == [Operand::IdRef(base_address), Operand::IdRef(13)]
            }),
            "the byte index is not added to the base address"
        );
    }

    #[test]
    fn the_address_table_reuses_the_modules_storage_buffer_ulong_pointer() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(40));
        module.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(2), vec![Operand::IdRef(1)]),
            inst(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(2)]),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::TypeBool, None, Some(6), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(7),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(7),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(6), Some(12), vec![]),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        module.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(2),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(8),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(3), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(3),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::AccessChain,
                Some(5),
                Some(31),
                vec![Operand::IdRef(20), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(
                Op::AccessChain,
                Some(5),
                Some(32),
                vec![Operand::IdRef(21), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(
                Op::Select,
                Some(5),
                Some(33),
                vec![Operand::IdRef(12), Operand::IdRef(31), Operand::IdRef(32)],
            ),
            inst(Op::Load, Some(1), Some(35), vec![Operand::IdRef(33)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut function = Function::new();
        function.blocks = vec![block];
        module.functions = vec![function];

        assert!(rewrite_cross_binding_pointer_merges(&mut module));
        let declarations = module
            .types_global_values
            .iter()
            .filter(|instruction| {
                instruction.class.opcode == Op::TypePointer
                    && instruction.operands
                        == [
                            Operand::StorageClass(StorageClass::StorageBuffer),
                            Operand::IdRef(1),
                        ]
            })
            .filter_map(|instruction| instruction.result_id)
            .collect::<Vec<_>>();
        assert_eq!(
            declarations,
            vec![5],
            "the address table declared its own StorageBuffer ulong pointer"
        );
    }

    #[test]
    fn physical_vector_stride_prefers_explicit_layout_and_rejects_conflicts() {
        let mut module = Module::new();
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            inst(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(2)]),
        ];
        let type_defs = module
            .types_global_values
            .iter()
            .filter_map(|instruction| {
                instruction
                    .result_id
                    .map(|result_id| (result_id, instruction.clone()))
            })
            .collect::<HashMap<_, _>>();

        assert_eq!(element_allocation_stride(&module, &type_defs, 2), Some(4));

        module.annotations.push(inst(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(3),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(8),
            ],
        ));
        assert_eq!(element_allocation_stride(&module, &type_defs, 2), Some(8));

        module.annotations.push(inst(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(4),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(4),
            ],
        ));
        assert_eq!(element_allocation_stride(&module, &type_defs, 2), None);
    }

    #[test]
    fn rewrite_cross_binding_vector_elements_preserves_source_array_stride() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(40));
        module.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        module.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(3), vec![Operand::IdRef(2)]),
            inst(Op::TypeStruct, None, Some(4), vec![Operand::IdRef(3)]),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            inst(Op::TypeBool, None, Some(7), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(8),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(8),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(8),
                Some(11),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(Op::ConstantTrue, Some(7), Some(12), vec![]),
            inst(
                Op::Variable,
                Some(5),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(5),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        module.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(3),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(8),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(4), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(4),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::AccessChain,
                Some(6),
                Some(31),
                vec![Operand::IdRef(20), Operand::IdRef(10), Operand::IdRef(11)],
            ),
            inst(
                Op::AccessChain,
                Some(6),
                Some(32),
                vec![Operand::IdRef(21), Operand::IdRef(10), Operand::IdRef(11)],
            ),
            inst(
                Op::Select,
                Some(6),
                Some(33),
                vec![Operand::IdRef(12), Operand::IdRef(31), Operand::IdRef(32)],
            ),
            inst(Op::Load, Some(2), Some(34), vec![Operand::IdRef(33)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut function = Function::new();
        function.blocks = vec![block];
        module.functions = vec![function];

        assert!(rewrite_cross_binding_pointer_merges(&mut module));
        let physical_vector_pointer = module
            .types_global_values
            .iter()
            .find_map(|instruction| {
                (instruction.class.opcode == Op::TypePointer
                    && instruction.operands
                        == [
                            Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                            Operand::IdRef(2),
                        ])
                .then_some(instruction.result_id)
                .flatten()
            })
            .expect("physical uchar3 pointer");
        assert!(module.annotations.iter().any(|annotation| {
            annotation.class.opcode == Op::Decorate
                && annotation.operands
                    == [
                        Operand::IdRef(physical_vector_pointer),
                        Operand::Decoration(Decoration::ArrayStride),
                        Operand::LiteralBit32(8),
                    ]
        }));
    }

    #[test]
    fn rewrite_whole_buffer_cross_binding_select_lowers_to_physical() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(2), vec![Operand::IdRef(1)]),
            inst(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(2)]),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::TypeBool, None, Some(6), vec![]),
            inst(
                Op::TypeFloat,
                None,
                Some(7),
                vec![Operand::LiteralBit32(32)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(8),
                vec![Operand::IdRef(7), Operand::LiteralBit32(4)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(6), Some(11), vec![]),
            inst(Op::Undef, Some(4), Some(12), vec![]),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        m.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(2),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(4),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(3), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(3),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::Select,
                Some(4),
                Some(31),
                vec![Operand::IdRef(11), Operand::IdRef(20), Operand::IdRef(21)],
            ),
            inst(
                Op::InBoundsAccessChain,
                Some(5),
                Some(32),
                vec![Operand::IdRef(31), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(Op::Load, Some(8), Some(33), vec![Operand::IdRef(32)]),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.blocks = vec![block];
        m.functions = vec![func];

        let mut select_only = m.clone();
        assert!(!has_cross_binding_pointer_phi(&select_only));
        let mut pointer_invalid = select_only.clone();
        pointer_invalid.functions[0].blocks[0].instructions.insert(
            1,
            inst(Op::Bitcast, Some(4), Some(39), vec![Operand::IdRef(20)]),
        );
        let pointer_invalid_before = pointer_invalid.assemble();
        assert_eq!(
            super::super::rewrites::construct_interface_cross_binding_pointer_merges_module(
                &mut pointer_invalid,
                crate::reflect::DescriptorLayout::default(),
            ),
            None
        );
        assert_eq!(pointer_invalid.assemble(), pointer_invalid_before);
        let mut memory_invalid = select_only.clone();
        memory_invalid.functions[0].blocks[0].instructions.insert(
            1,
            inst(Op::Load, Some(1), Some(38), vec![Operand::IdRef(7)]),
        );
        let memory_invalid_before = memory_invalid.assemble();
        assert_eq!(
            super::super::rewrites::construct_interface_cross_binding_pointer_merges_module(
                &mut memory_invalid,
                crate::reflect::DescriptorLayout::default(),
            ),
            None
        );
        assert_eq!(memory_invalid.assemble(), memory_invalid_before);
        let mut phi = m.clone();
        phi.functions[0].blocks[0].instructions[0] = inst(
            Op::Phi,
            Some(4),
            Some(31),
            vec![
                Operand::IdRef(20),
                Operand::IdRef(30),
                Operand::IdRef(21),
                Operand::IdRef(30),
                Operand::IdRef(12),
                Operand::IdRef(30),
            ],
        );
        assert!(has_cross_binding_pointer_phi(&phi));
        let mut nested_phi = m.clone();
        nested_phi.functions[0].blocks[0].instructions[0] = inst(
            Op::Phi,
            Some(4),
            Some(31),
            vec![
                Operand::IdRef(20),
                Operand::IdRef(30),
                Operand::IdRef(34),
                Operand::IdRef(30),
            ],
        );
        nested_phi.functions[0].blocks[0].instructions.insert(
            0,
            inst(
                Op::Phi,
                Some(4),
                Some(34),
                vec![
                    Operand::IdRef(21),
                    Operand::IdRef(30),
                    Operand::IdRef(12),
                    Operand::IdRef(30),
                ],
            ),
        );
        assert!(has_cross_binding_pointer_phi(&nested_phi));
        assert!(rewrite_cross_binding_pointer_phis(&mut nested_phi));
        assert!(!rewrite_cross_binding_pointer_phis(&mut select_only));
        let before_exhaustion = phi.assemble();
        let exhausted_layout = crate::reflect::DescriptorLayout {
            synthetic: crate::reflect::DescriptorBindingRange {
                start: crate::reflect::SYNTHETIC_BINDING_BASE,
                end: crate::reflect::SYNTHETIC_BINDING_BASE,
            },
            ..crate::reflect::DescriptorLayout::default()
        };
        assert_eq!(
            construct_cross_binding_pointer_phis_with_layout(&mut phi, exhausted_layout),
            None
        );
        assert_eq!(phi.assemble(), before_exhaustion);
        assert!(rewrite_cross_binding_pointer_phis(&mut phi));
        assert!(rewrite_cross_binding_pointer_merges(&mut m));

        assert!(matches!(
            m.memory_model.as_ref().unwrap().operands.first(),
            Some(Operand::AddressingModel(
                spirv::AddressingModel::PhysicalStorageBuffer64
            ))
        ));
        let select = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(31))
            .unwrap();
        assert!(!select
            .operands
            .iter()
            .any(|o| matches!(o, Operand::IdRef(20) | Operand::IdRef(21))));
        let n_convert = m.functions[0].blocks[0]
            .instructions
            .iter()
            .filter(|i| i.class.opcode == Op::ConvertUToPtr)
            .count();
        assert_eq!(n_convert, 3);
        let chain = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(32))
            .unwrap();
        let chain_ty = chain.result_type.unwrap();
        let sc = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(chain_ty))
            .and_then(|i| i.operands.first());
        assert!(matches!(
            sc,
            Some(Operand::StorageClass(StorageClass::PhysicalStorageBuffer))
        ));
        let load = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(33))
            .unwrap();
        let Operand::IdRef(load_pointer) = load.operands[0] else {
            panic!("load pointer is not an id");
        };
        let load_pointer_type = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(load_pointer))
            .and_then(|i| i.result_type)
            .unwrap();
        let load_pointer_definition = m
            .types_global_values
            .iter()
            .find(|i| i.result_id == Some(load_pointer_type))
            .unwrap();
        assert_eq!(
            load_pointer_definition.operands,
            [
                Operand::StorageClass(StorageClass::PhysicalStorageBuffer),
                Operand::IdRef(8)
            ]
        );
        let rewritten_phi = phi.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(31))
            .unwrap();
        let Operand::IdRef(retyped_undef) = rewritten_phi.operands[4] else {
            panic!("phi undef arm is not an id");
        };
        assert_ne!(retyped_undef, 12);
        let retyped_undef_definition = phi.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(retyped_undef))
            .unwrap();
        assert_eq!(retyped_undef_definition.class.opcode, Op::ConvertUToPtr);
        assert_eq!(
            retyped_undef_definition.result_type,
            rewritten_phi.result_type
        );
        let Operand::IdRef(zero_address) = retyped_undef_definition.operands[0] else {
            panic!("physical null address is not an id");
        };
        let zero_address_definition = phi.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|instruction| instruction.result_id == Some(zero_address))
            .expect("zero address conversion");
        assert_eq!(zero_address_definition.class.opcode, Op::UConvert);
    }

    #[test]
    fn rewrite_whole_buffer_cross_binding_select_allows_atomic_consumer() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.memory_model = Some(inst(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(MemoryModel::GLSL450),
            ],
        ));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeRuntimeArray, None, Some(2), vec![Operand::IdRef(1)]),
            inst(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(2)]),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::TypeBool, None, Some(6), vec![]),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(6), Some(11), vec![]),
            inst(
                Op::Constant,
                Some(1),
                Some(12),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(13),
                vec![Operand::LiteralBit32(7)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(21),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
        ];
        m.annotations = vec![
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(2),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(4),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![Operand::IdRef(3), Operand::Decoration(Decoration::Block)],
            ),
            inst(
                Op::MemberDecorate,
                None,
                None,
                vec![
                    Operand::IdRef(3),
                    Operand::LiteralBit32(0),
                    Operand::Decoration(Decoration::Offset),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(0),
                ],
            ),
            inst(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(21),
                    Operand::Decoration(Decoration::Binding),
                    Operand::LiteralBit32(1),
                ],
            ),
        ];
        let mut block = Block::new();
        block.label = Some(inst(Op::Label, None, Some(30), vec![]));
        block.instructions = vec![
            inst(
                Op::Select,
                Some(4),
                Some(31),
                vec![Operand::IdRef(11), Operand::IdRef(20), Operand::IdRef(21)],
            ),
            inst(
                Op::InBoundsAccessChain,
                Some(5),
                Some(32),
                vec![Operand::IdRef(31), Operand::IdRef(10), Operand::IdRef(10)],
            ),
            inst(
                Op::AtomicIAdd,
                Some(1),
                Some(33),
                vec![
                    Operand::IdRef(32),
                    Operand::IdRef(12),
                    Operand::IdRef(10),
                    Operand::IdRef(13),
                ],
            ),
            inst(Op::Return, None, None, vec![]),
        ];
        let mut func = Function::new();
        func.blocks = vec![block];
        m.functions = vec![func];

        assert!(rewrite_cross_binding_pointer_merges(&mut m));
        assert!(matches!(
            m.memory_model.as_ref().unwrap().operands.first(),
            Some(Operand::AddressingModel(
                spirv::AddressingModel::PhysicalStorageBuffer64
            ))
        ));
        let select = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(31))
            .unwrap();
        assert!(!select
            .operands
            .iter()
            .any(|o| matches!(o, Operand::IdRef(20) | Operand::IdRef(21))));
        let atomic = m.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(33))
            .unwrap();
        assert_eq!(atomic.class.opcode, Op::AtomicIAdd);
        assert!(matches!(atomic.operands.first(), Some(Operand::IdRef(32))));
    }
}
