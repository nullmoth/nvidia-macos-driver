use super::{
    BufferByteRange, BufferFootprint, BufferIndexSource, BufferStrideTerm, BufferStridedAccess,
    DescriptorLocation, ResourceAccess, ResourceBinding, ResourceKind, ShaderReflection,
};
use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::{Instruction, Module, Operand};
use spirv::{BuiltIn, Decoration, Op, Word};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ADDRESS_ALTERNATIVES_PER_POINTER: usize = 4096;
const MAX_FOOTPRINT_RECORDS_PER_BINDING: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct DescriptorKey {
    pub(super) set: u32,
    pub(super) binding: u32,
}

impl From<DescriptorLocation> for DescriptorKey {
    fn from(value: DescriptorLocation) -> Self {
        Self {
            set: value.set,
            binding: value.binding,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct ScalarExpr {
    constant: u64,
    terms: BTreeMap<BufferIndexSource, u64>,
}

impl ScalarExpr {
    fn constant(value: u64) -> Self {
        Self {
            constant: value,
            terms: BTreeMap::new(),
        }
    }

    fn source(source: BufferIndexSource) -> Self {
        Self {
            constant: 0,
            terms: [(source, 1)].into_iter().collect(),
        }
    }

    fn checked_add(&self, rhs: &Self) -> Option<Self> {
        let mut terms = self.terms.clone();
        for (source, stride) in &rhs.terms {
            let value = terms
                .get(source)
                .copied()
                .unwrap_or(0)
                .checked_add(*stride)?;
            if value == 0 {
                terms.remove(source);
            } else {
                terms.insert(*source, value);
            }
        }
        Some(Self {
            constant: self.constant.checked_add(rhs.constant)?,
            terms,
        })
    }

    fn checked_sub(&self, rhs: &Self) -> Option<Self> {
        let mut terms = self.terms.clone();
        for (source, stride) in &rhs.terms {
            let value = terms
                .get(source)
                .copied()
                .unwrap_or(0)
                .checked_sub(*stride)?;
            if value == 0 {
                terms.remove(source);
            } else {
                terms.insert(*source, value);
            }
        }
        Some(Self {
            constant: self.constant.checked_sub(rhs.constant)?,
            terms,
        })
    }

    fn checked_mul(&self, factor: u64) -> Option<Self> {
        Some(Self {
            constant: self.constant.checked_mul(factor)?,
            terms: self
                .terms
                .iter()
                .map(|(source, stride)| Some((*source, stride.checked_mul(factor)?)))
                .collect::<Option<_>>()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Address {
    root: DescriptorKey,
    offset: Option<ScalarExpr>,
}

#[derive(Default)]
struct Decorations {
    descriptor_sets: HashMap<Word, u32>,
    bindings: HashMap<Word, u32>,
    builtins: HashMap<Word, BuiltIn>,
    array_strides: HashMap<Word, u64>,
    member_offsets: HashMap<(Word, u32), u64>,
    member_matrix_strides: HashMap<(Word, u32), u64>,
    row_major_members: HashSet<(Word, u32)>,
}

#[derive(Clone, Copy)]
struct MatrixLayout {
    stride: u64,
    row_major: bool,
}

pub(super) struct Analyzer<'a> {
    module: &'a Module,
    definitions: HashMap<Word, &'a Instruction>,
    value_types: HashMap<Word, Word>,
    constants: HashMap<Word, u64>,
    decorations: Decorations,
    roots: HashMap<Word, DescriptorKey>,
    pointer_addresses: HashMap<Word, Vec<Address>>,
    scalar_memo: HashMap<Word, Option<ScalarExpr>>,
    scalar_visiting: HashSet<Word>,
}

pub(super) fn attach_buffer_footprints(
    reflection: &mut ShaderReflection,
    module: &Module,
) -> Result<(), String> {
    let target_bindings = reflection
        .bindings
        .iter()
        .filter(|binding| binding_supports_footprint(binding))
        .filter_map(|binding| binding.descriptor.map(DescriptorKey::from))
        .collect::<BTreeSet<_>>();
    if target_bindings.is_empty() {
        return Ok(());
    }

    let mut analyzer = Analyzer::new(module, &target_bindings);
    let analysis = analyzer.analyze();
    let logical_addressing = addressing_is_logical(module);

    for binding in &mut reflection.bindings {
        if !binding_supports_footprint(binding) {
            continue;
        }
        let Some(descriptor) = binding.descriptor else {
            continue;
        };
        let key = DescriptorKey::from(descriptor);
        let Some(footprint) = analysis.footprints.get(&key).cloned() else {
            if logical_addressing {
                binding.access = Some(ResourceAccess::Unused);
            }
            binding.footprint = Some(BufferFootprint {
                has_unbounded_access: !logical_addressing
                    && binding.access != Some(ResourceAccess::Unused),
                ..BufferFootprint::default()
            });
            continue;
        };
        binding.footprint = Some(footprint);
        let observed = analysis.observed.get(&key).copied();
        if logical_addressing && !analysis.escaped.contains(&key) {
            binding.access = Some(exact_access(observed.unwrap_or_default()));
        } else if let Some(observed) = observed {
            binding.access = Some(widen_access(binding.access, observed));
        }
    }
    Ok(())
}

fn widen_access(declared: Option<ResourceAccess>, observed: AccessDirection) -> ResourceAccess {
    let (declared_reads, declared_writes) = match declared {
        Some(ResourceAccess::ReadOnly) => (true, false),
        Some(ResourceAccess::WriteOnly) => (false, true),
        Some(ResourceAccess::ReadWrite) => (true, true),
        _ => (false, false),
    };
    match (
        declared_reads || observed.reads,
        declared_writes || observed.writes,
    ) {
        (false, false) => declared.unwrap_or(ResourceAccess::Unused),
        direction => classify(direction),
    }
}

fn exact_access(observed: AccessDirection) -> ResourceAccess {
    classify((observed.reads, observed.writes))
}

fn classify(direction: (bool, bool)) -> ResourceAccess {
    match direction {
        (true, true) => ResourceAccess::ReadWrite,
        (true, false) => ResourceAccess::ReadOnly,
        (false, true) => ResourceAccess::WriteOnly,
        (false, false) => ResourceAccess::Unused,
    }
}

pub(super) fn addressing_is_logical(module: &Module) -> bool {
    module
        .memory_model
        .as_ref()
        .and_then(|instruction| instruction.operands.first())
        .is_some_and(|operand| {
            matches!(
                operand,
                Operand::AddressingModel(spirv::AddressingModel::Logical)
            )
        })
}

pub(super) fn descriptor_keys(module: &Module) -> HashMap<Word, DescriptorKey> {
    let decorations = Decorations::from_module(module);
    decorations
        .descriptor_sets
        .iter()
        .filter_map(|(id, set)| {
            Some((
                *id,
                DescriptorKey {
                    set: *set,
                    binding: *decorations.bindings.get(id)?,
                },
            ))
        })
        .collect()
}

fn binding_supports_footprint(binding: &ResourceBinding) -> bool {
    matches!(
        binding.kind,
        ResourceKind::Buffer
            | ResourceKind::KernelStageInput
            | ResourceKind::AccelerationStructureShadow
    ) && binding.descriptor.is_some()
}

impl<'a> Analyzer<'a> {
    pub(super) fn new(module: &'a Module, targets: &BTreeSet<DescriptorKey>) -> Self {
        let definitions = module
            .all_inst_iter()
            .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction)))
            .collect::<HashMap<_, _>>();
        let value_types = definitions
            .iter()
            .filter_map(|(id, instruction)| instruction.result_type.map(|ty| (*id, ty)))
            .collect::<HashMap<_, _>>();
        let constants = definitions
            .iter()
            .filter_map(|(id, instruction)| constant_value(instruction).map(|value| (*id, value)))
            .collect::<HashMap<_, _>>();
        let decorations = Decorations::from_module(module);
        let roots = definitions
            .iter()
            .filter_map(|(id, instruction)| {
                (instruction.class.opcode == Op::Variable).then_some(())?;
                let key = DescriptorKey {
                    set: *decorations.descriptor_sets.get(id)?,
                    binding: *decorations.bindings.get(id)?,
                };
                targets.contains(&key).then_some((*id, key))
            })
            .collect::<HashMap<_, _>>();
        let pointer_addresses = roots
            .iter()
            .map(|(id, root)| {
                (
                    *id,
                    vec![Address {
                        root: *root,
                        offset: Some(ScalarExpr::default()),
                    }],
                )
            })
            .collect();
        Self {
            module,
            definitions,
            value_types,
            constants,
            decorations,
            roots,
            pointer_addresses,
            scalar_memo: HashMap::default(),
            scalar_visiting: HashSet::default(),
        }
    }

    pub(super) fn analyze(&mut self) -> Analysis {
        self.propagate_pointer_addresses();
        let mut footprints = self
            .roots
            .values()
            .copied()
            .map(|key| (key, BufferFootprint::default()))
            .collect::<BTreeMap<_, _>>();
        let mut observed = BTreeMap::<DescriptorKey, AccessDirection>::new();

        for instruction in self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
        {
            for access in self.memory_accesses(instruction) {
                let Some(addresses) = self.pointer_addresses.get(&access.pointer).cloned() else {
                    continue;
                };
                for address in addresses {
                    let seen = observed.entry(address.root).or_insert(AccessDirection {
                        reads: false,
                        writes: false,
                    });
                    seen.reads |= access.direction.reads;
                    seen.writes |= access.direction.writes;
                    let footprint = footprints.entry(address.root).or_default();
                    let Some(size) = access.size else {
                        footprint.has_unbounded_access = true;
                        continue;
                    };
                    let Some(offset) = address.offset else {
                        footprint.has_unbounded_access = true;
                        continue;
                    };
                    if offset.terms.is_empty() {
                        if offset.constant.checked_add(size).is_some() {
                            if footprint.static_ranges.len() + footprint.strided_accesses.len()
                                < MAX_FOOTPRINT_RECORDS_PER_BINDING
                            {
                                footprint.static_ranges.push(BufferByteRange {
                                    offset: offset.constant,
                                    size,
                                });
                            } else {
                                footprint.has_unbounded_access = true;
                            }
                        } else {
                            footprint.has_unbounded_access = true;
                        }
                    } else {
                        if footprint.static_ranges.len() + footprint.strided_accesses.len()
                            < MAX_FOOTPRINT_RECORDS_PER_BINDING
                        {
                            footprint.strided_accesses.push(BufferStridedAccess {
                                base_offset: offset.constant,
                                access_size: size,
                                terms: offset
                                    .terms
                                    .into_iter()
                                    .map(|(source, stride)| BufferStrideTerm { source, stride })
                                    .collect(),
                            });
                        } else {
                            footprint.has_unbounded_access = true;
                        }
                    }
                }
            }
        }
        let mut escaped = BTreeSet::new();
        self.mark_unmodeled_pointer_escapes(&mut footprints, &mut escaped);

        for footprint in footprints.values_mut() {
            coalesce_static_ranges(&mut footprint.static_ranges);
            footprint.strided_accesses.sort();
            footprint.strided_accesses.dedup();
        }
        Analysis {
            footprints,
            observed,
            escaped,
        }
    }

    fn mark_unmodeled_pointer_escapes(
        &self,
        footprints: &mut BTreeMap<DescriptorKey, BufferFootprint>,
        escaped: &mut BTreeSet<DescriptorKey>,
    ) {
        for instruction in self
            .module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .flat_map(|block| block.instructions.iter())
        {
            for (position, operand) in instruction.operands.iter().enumerate() {
                let Some(pointer) = id_ref(operand) else {
                    continue;
                };
                let Some(addresses) = self.pointer_addresses.get(&pointer) else {
                    continue;
                };
                if pointer_operand_is_modeled(instruction.class.opcode, position) {
                    continue;
                }
                for address in addresses {
                    footprints
                        .entry(address.root)
                        .or_default()
                        .has_unbounded_access = true;
                    escaped.insert(address.root);
                }
            }
        }
    }

    fn propagate_pointer_addresses(&mut self) {
        let limit = self.definitions.len().saturating_add(1);
        for _ in 0..limit {
            let mut changed = false;
            let mut pointer_ids = self
                .value_types
                .iter()
                .filter_map(|(id, ty)| self.pointer_pointee(*ty).map(|_| *id))
                .collect::<Vec<_>>();
            pointer_ids.sort_unstable();
            for id in pointer_ids {
                if self.roots.contains_key(&id) {
                    continue;
                }
                let mut derived = self.derive_pointer_addresses(id);
                if derived.is_empty() {
                    continue;
                }
                normalize_addresses(&mut derived);
                let current = self.pointer_addresses.entry(id).or_default();
                let previous = current.clone();
                current.extend(derived);
                normalize_addresses(current);
                changed |= *current != previous;
            }
            if !changed {
                break;
            }
        }
    }

    fn derive_pointer_addresses(&mut self, id: Word) -> Vec<Address> {
        let Some(instruction) = self.definitions.get(&id).copied() else {
            return Vec::new();
        };
        match instruction.class.opcode {
            Op::AccessChain | Op::InBoundsAccessChain => {
                let Some(Operand::IdRef(base)) = instruction.operands.first() else {
                    return Vec::new();
                };
                let Some(base_ty) = self.value_types.get(base).copied() else {
                    return Vec::new();
                };
                let Some(pointee) = self.pointer_pointee(base_ty) else {
                    return Vec::new();
                };
                let delta = self.access_chain_delta(pointee, &instruction.operands[1..]);
                self.offset_addresses(*base, delta)
            }
            Op::PtrAccessChain | Op::InBoundsPtrAccessChain => {
                let Some(Operand::IdRef(base)) = instruction.operands.first() else {
                    return Vec::new();
                };
                let Some(base_ty) = self.value_types.get(base).copied() else {
                    return Vec::new();
                };
                let Some(pointee) = self.pointer_pointee(base_ty) else {
                    return Vec::new();
                };
                let Some(element) = instruction.operands.get(1).and_then(id_ref) else {
                    return self.unknown_addresses_from(*base);
                };
                let Some(stride) = self
                    .decorations
                    .array_strides
                    .get(&base_ty)
                    .copied()
                    .or_else(|| self.type_span(pointee, &mut HashSet::default()))
                else {
                    return self.unknown_addresses_from(*base);
                };
                let Some(element_delta) = self
                    .scalar_expr(element)
                    .and_then(|v| v.checked_mul(stride))
                else {
                    return self.unknown_addresses_from(*base);
                };
                let suffix = self.access_chain_delta(pointee, &instruction.operands[2..]);
                let delta = suffix.and_then(|value| element_delta.checked_add(&value));
                self.offset_addresses(*base, delta)
            }
            Op::CopyObject | Op::Bitcast => instruction
                .operands
                .first()
                .and_then(id_ref)
                .and_then(|source| self.pointer_addresses.get(&source).cloned())
                .unwrap_or_default(),
            Op::Select => instruction
                .operands
                .get(1..3)
                .unwrap_or_default()
                .iter()
                .filter_map(id_ref)
                .flat_map(|source| {
                    self.pointer_addresses
                        .get(&source)
                        .cloned()
                        .unwrap_or_default()
                })
                .collect(),
            Op::Phi => instruction
                .operands
                .iter()
                .step_by(2)
                .filter_map(id_ref)
                .flat_map(|source| {
                    self.pointer_addresses
                        .get(&source)
                        .cloned()
                        .unwrap_or_default()
                })
                .collect(),
            Op::Load | Op::ConvertUToPtr | Op::FunctionParameter | Op::Variable => Vec::new(),
            _ => self.unknown_pointer_operands(instruction),
        }
    }

    fn offset_addresses(&self, base: Word, delta: Option<ScalarExpr>) -> Vec<Address> {
        self.pointer_addresses
            .get(&base)
            .into_iter()
            .flatten()
            .map(|address| Address {
                root: address.root,
                offset: address
                    .offset
                    .as_ref()
                    .and_then(|offset| delta.as_ref().and_then(|delta| offset.checked_add(delta))),
            })
            .collect()
    }

    fn unknown_addresses_from(&self, base: Word) -> Vec<Address> {
        self.pointer_addresses
            .get(&base)
            .into_iter()
            .flatten()
            .map(|address| Address {
                root: address.root,
                offset: None,
            })
            .collect()
    }

    fn unknown_pointer_operands(&self, instruction: &Instruction) -> Vec<Address> {
        instruction
            .operands
            .iter()
            .filter_map(id_ref)
            .filter(|operand| {
                self.value_types
                    .get(operand)
                    .is_some_and(|ty| self.pointer_pointee(*ty).is_some())
            })
            .flat_map(|operand| self.unknown_addresses_from(operand))
            .collect()
    }

    fn access_chain_delta(&mut self, mut ty: Word, indices: &[Operand]) -> Option<ScalarExpr> {
        let mut offset = ScalarExpr::default();
        let mut matrix_layout = None;
        for operand in indices {
            let index = id_ref(operand).and_then(|id| self.scalar_expr(id))?;
            let definition = self.definitions.get(&ty).copied()?;
            match definition.class.opcode {
                Op::TypeStruct => {
                    if !index.terms.is_empty() {
                        return None;
                    }
                    let member = u32::try_from(index.constant).ok()?;
                    let member_ty = definition.operands.get(member as usize).and_then(id_ref)?;
                    let member_offset = *self.decorations.member_offsets.get(&(ty, member))?;
                    offset = offset.checked_add(&ScalarExpr::constant(member_offset))?;
                    matrix_layout = self.decorations.matrix_layout(ty, member);
                    ty = member_ty;
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    let element = definition.operands.first().and_then(id_ref)?;
                    let stride = *self.decorations.array_strides.get(&ty)?;
                    offset = offset.checked_add(&index.checked_mul(stride)?)?;
                    ty = element;
                }
                Op::TypeVector => {
                    let element = definition.operands.first().and_then(id_ref)?;
                    let stride = self.type_span(element, &mut HashSet::default())?;
                    offset = offset.checked_add(&index.checked_mul(stride)?)?;
                    ty = element;
                }
                Op::TypeMatrix => {
                    let layout = matrix_layout?;
                    if layout.row_major {
                        return None;
                    }
                    let column = definition.operands.first().and_then(id_ref)?;
                    offset = offset.checked_add(&index.checked_mul(layout.stride)?)?;
                    ty = column;
                }
                _ => return None,
            }
        }
        Some(offset)
    }

    fn scalar_expr(&mut self, id: Word) -> Option<ScalarExpr> {
        if let Some(cached) = self.scalar_memo.get(&id) {
            return cached.clone();
        }
        if !self.scalar_visiting.insert(id) {
            return None;
        }
        let value = self.scalar_expr_uncached(id);
        self.scalar_visiting.remove(&id);
        self.scalar_memo.insert(id, value.clone());
        value
    }

    fn scalar_expr_uncached(&mut self, id: Word) -> Option<ScalarExpr> {
        if let Some(value) = self.constants.get(&id).copied() {
            return Some(ScalarExpr::constant(value));
        }
        let instruction = self.definitions.get(&id).copied()?;
        match instruction.class.opcode {
            Op::Load => {
                let pointer = instruction.operands.first().and_then(id_ref)?;
                self.builtin_pointer_source(pointer).map(ScalarExpr::source)
            }
            Op::CompositeExtract => {
                let composite = instruction.operands.first().and_then(id_ref)?;
                let component = instruction.operands.get(1).and_then(literal_u32)?;
                self.builtin_component_expr(composite, component)
            }
            Op::CopyObject | Op::UConvert | Op::SConvert | Op::Bitcast => {
                self.scalar_expr(instruction.operands.first().and_then(id_ref)?)
            }
            Op::IAdd => {
                let lhs_id = instruction.operands.first().and_then(id_ref)?;
                let rhs_id = instruction.operands.get(1).and_then(id_ref)?;
                if let Some(expr) = self.logical_dispatch_index_expr(lhs_id, rhs_id) {
                    return Some(expr);
                }
                if let Some(expr) = self.logical_dispatch_index_expr(rhs_id, lhs_id) {
                    return Some(expr);
                }
                let lhs = self.scalar_expr(lhs_id)?;
                let rhs = self.scalar_expr(rhs_id)?;
                lhs.checked_add(&rhs)
            }
            Op::ISub => {
                let lhs = self.scalar_expr(instruction.operands.first().and_then(id_ref)?)?;
                let rhs = self.scalar_expr(instruction.operands.get(1).and_then(id_ref)?)?;
                lhs.checked_sub(&rhs)
            }
            Op::IMul => {
                let lhs_id = instruction.operands.first().and_then(id_ref)?;
                let rhs_id = instruction.operands.get(1).and_then(id_ref)?;
                if let Some(factor) = self.constants.get(&lhs_id).copied() {
                    self.scalar_expr(rhs_id)?.checked_mul(factor)
                } else if let Some(factor) = self.constants.get(&rhs_id).copied() {
                    self.scalar_expr(lhs_id)?.checked_mul(factor)
                } else {
                    None
                }
            }
            Op::ShiftLeftLogical => {
                let value = self.scalar_expr(instruction.operands.first().and_then(id_ref)?)?;
                let shift = self
                    .constants
                    .get(&instruction.operands.get(1).and_then(id_ref)?)
                    .copied()?;
                value.checked_mul(1_u64.checked_shl(u32::try_from(shift).ok()?)?)
            }
            Op::BitwiseOr => self.lowered_i64_add_sub_expr(instruction),
            _ => None,
        }
    }

    fn logical_dispatch_index_expr(
        &mut self,
        builtin_id: Word,
        base_id: Word,
    ) -> Option<ScalarExpr> {
        let expression = self.scalar_expr(builtin_id)?;
        if expression.constant != 0 || expression.terms.len() != 1 {
            return None;
        }
        let (&source, &stride) = expression.terms.iter().next()?;
        if stride != 1 {
            return None;
        }
        let expected_member = match source {
            BufferIndexSource::GlobalInvocationIdX => 3,
            BufferIndexSource::GlobalInvocationIdY => 4,
            BufferIndexSource::GlobalInvocationIdZ => 5,
            BufferIndexSource::WorkgroupIdX => 6,
            BufferIndexSource::WorkgroupIdY => 7,
            BufferIndexSource::WorkgroupIdZ => 8,
            _ => return None,
        };
        (self.dispatch_payload_member(base_id)? == expected_member).then_some(expression)
    }

    fn dispatch_payload_member(&self, id: Word) -> Option<u32> {
        let load = self.definitions.get(&id).copied()?;
        if load.class.opcode != Op::Load {
            return None;
        }
        let pointer = load.operands.first().and_then(id_ref)?;
        let access = self.definitions.get(&pointer).copied()?;
        if access.class.opcode != Op::AccessChain || access.operands.len() != 2 {
            return None;
        }
        let variable = access.operands.first().and_then(id_ref)?;
        let member_id = access.operands.get(1).and_then(id_ref)?;
        let member = u32::try_from(*self.constants.get(&member_id)?).ok()?;
        let variable = self.definitions.get(&variable).copied()?;
        if variable.class.opcode != Op::Variable
            || variable.operands.first()
                != Some(&Operand::StorageClass(spirv::StorageClass::PushConstant))
        {
            return None;
        }
        let pointer_ty = variable.result_type?;
        let pointer = self.definitions.get(&pointer_ty).copied()?;
        let block_ty = pointer.operands.get(1).and_then(id_ref)?;
        let block = self.definitions.get(&block_ty).copied()?;
        if block.class.opcode != Op::TypeStruct || block.operands.len() != 12 {
            return None;
        }
        let first_offset = *self.decorations.member_offsets.get(&(block_ty, 0))?;
        let exact_layout = first_offset % 4 == 0
            && (0..12).all(|index| {
                self.decorations.member_offsets.get(&(block_ty, index))
                    == Some(&(first_offset + u64::from(index) * 4))
            });
        exact_layout.then_some(member)
    }

    fn lowered_i64_add_sub_expr(&mut self, instruction: &Instruction) -> Option<ScalarExpr> {
        let (low, high) = self.recomposed_u64_halves(instruction)?;
        let low_definition = self.definitions.get(&low).copied()?;
        let operation = low_definition.class.opcode;
        if !matches!(operation, Op::IAdd | Op::ISub) {
            return None;
        }
        let low_lhs = low_definition.operands.first().and_then(id_ref)?;
        let low_rhs = low_definition.operands.get(1).and_then(id_ref)?;
        let original_lhs = self.low_half_source(low_lhs)?;
        let original_rhs = self.low_half_source(low_rhs)?;

        let high_definition = self.definitions.get(&high).copied()?;
        let high_base = match operation {
            Op::IAdd => high_definition
                .operands
                .iter()
                .filter_map(id_ref)
                .find(|candidate| {
                    self.binary_uses_high_halves(*candidate, Op::IAdd, original_lhs, original_rhs)
                })?,
            Op::ISub => {
                if high_definition.class.opcode != Op::ISub {
                    return None;
                }
                high_definition.operands.first().and_then(id_ref)?
            }
            _ => return None,
        };
        if !self.binary_uses_high_halves(high_base, operation, original_lhs, original_rhs) {
            return None;
        }

        let lhs = self.scalar_expr(original_lhs)?;
        let rhs = self.scalar_expr(original_rhs)?;
        match operation {
            Op::IAdd => lhs.checked_add(&rhs),
            Op::ISub => lhs.checked_sub(&rhs),
            _ => None,
        }
    }

    fn recomposed_u64_halves(&self, instruction: &Instruction) -> Option<(Word, Word)> {
        if instruction.class.opcode != Op::BitwiseOr {
            return None;
        }
        let mut low = None;
        let mut high = None;
        for operand in instruction.operands.iter().filter_map(id_ref) {
            let definition = self.definitions.get(&operand).copied()?;
            if definition.class.opcode == Op::UConvert {
                low = definition.operands.first().and_then(id_ref);
                continue;
            }
            if definition.class.opcode != Op::ShiftLeftLogical {
                return None;
            }
            let shift = definition
                .operands
                .get(1)
                .and_then(id_ref)
                .and_then(|id| self.constants.get(&id).copied())?;
            if shift != 32 {
                return None;
            }
            let widened = definition.operands.first().and_then(id_ref)?;
            let widened_definition = self.definitions.get(&widened).copied()?;
            if widened_definition.class.opcode != Op::UConvert {
                return None;
            }
            high = widened_definition.operands.first().and_then(id_ref);
        }
        Some((low?, high?))
    }

    fn low_half_source(&self, value: Word) -> Option<Word> {
        let definition = self.definitions.get(&value).copied()?;
        (definition.class.opcode == Op::UConvert)
            .then(|| definition.operands.first().and_then(id_ref))?
    }

    fn high_half_source(&self, value: Word) -> Option<Word> {
        let definition = self.definitions.get(&value).copied()?;
        if definition.class.opcode != Op::UConvert {
            return None;
        }
        let shifted = definition.operands.first().and_then(id_ref)?;
        let shifted_definition = self.definitions.get(&shifted).copied()?;
        if shifted_definition.class.opcode != Op::ShiftRightLogical {
            return None;
        }
        let shift = shifted_definition
            .operands
            .get(1)
            .and_then(id_ref)
            .and_then(|id| self.constants.get(&id).copied())?;
        (shift == 32).then(|| shifted_definition.operands.first().and_then(id_ref))?
    }

    fn binary_uses_high_halves(
        &self,
        value: Word,
        opcode: Op,
        original_lhs: Word,
        original_rhs: Word,
    ) -> bool {
        let Some(definition) = self.definitions.get(&value).copied() else {
            return false;
        };
        if definition.class.opcode != opcode {
            return false;
        }
        let Some(lhs) = definition.operands.first().and_then(id_ref) else {
            return false;
        };
        let Some(rhs) = definition.operands.get(1).and_then(id_ref) else {
            return false;
        };
        let pair = (self.high_half_source(lhs), self.high_half_source(rhs));
        pair == (Some(original_lhs), Some(original_rhs))
            || (opcode == Op::IAdd && pair == (Some(original_rhs), Some(original_lhs)))
    }

    fn builtin_component_expr(&mut self, value: Word, component: u32) -> Option<ScalarExpr> {
        let instruction = self.definitions.get(&value).copied()?;
        match instruction.class.opcode {
            Op::Load => {
                let pointer = instruction.operands.first().and_then(id_ref)?;
                let builtin = self.decorations.builtins.get(&pointer).copied()?;
                builtin_source(builtin, component).map(ScalarExpr::source)
            }
            Op::CopyObject | Op::Bitcast | Op::UConvert | Op::SConvert => self
                .builtin_component_expr(instruction.operands.first().and_then(id_ref)?, component),
            Op::CompositeConstruct => self.scalar_expr(
                instruction
                    .operands
                    .get(component as usize)
                    .and_then(id_ref)?,
            ),
            _ => None,
        }
    }

    fn builtin_pointer_source(&self, pointer: Word) -> Option<BufferIndexSource> {
        if let Some(builtin) = self.decorations.builtins.get(&pointer).copied() {
            return builtin_source(builtin, 0);
        }
        let instruction = self.definitions.get(&pointer).copied()?;
        if !matches!(
            instruction.class.opcode,
            Op::AccessChain | Op::InBoundsAccessChain
        ) {
            return None;
        }
        let base = instruction.operands.first().and_then(id_ref)?;
        let builtin = self.decorations.builtins.get(&base).copied()?;
        let component = instruction
            .operands
            .last()
            .and_then(id_ref)
            .and_then(|id| self.constants.get(&id).copied())
            .and_then(|value| u32::try_from(value).ok())?;
        builtin_source(builtin, component)
    }

    fn memory_accesses(&mut self, instruction: &Instruction) -> Vec<MemoryAccess> {
        let pointer = |index| instruction.operands.get(index).and_then(id_ref);
        let result_size = || {
            instruction
                .result_type
                .and_then(|ty| pointer(0).and_then(|pointer| self.access_type_span(pointer, ty)))
        };
        let pointee_size = |this: &Self, pointer: Word| {
            this.value_types
                .get(&pointer)
                .and_then(|ty| this.pointer_pointee(*ty))
                .and_then(|ty| this.access_type_span(pointer, ty))
        };
        match instruction.class.opcode {
            Op::Load => pointer(0)
                .map(|pointer| MemoryAccess {
                    pointer,
                    size: result_size(),
                    direction: AccessDirection::READ,
                })
                .into_iter()
                .collect(),
            Op::Store => pointer(0)
                .map(|pointer| MemoryAccess {
                    pointer,
                    size: instruction
                        .operands
                        .get(1)
                        .and_then(id_ref)
                        .and_then(|value| self.value_types.get(&value).copied())
                        .and_then(|ty| self.access_type_span(pointer, ty)),
                    direction: AccessDirection::WRITE,
                })
                .into_iter()
                .collect(),
            Op::CopyMemory => [
                (pointer(0), AccessDirection::WRITE),
                (pointer(1), AccessDirection::READ),
            ]
            .into_iter()
            .filter_map(|(pointer, direction)| Some((pointer?, direction)))
            .map(|(pointer, direction)| MemoryAccess {
                pointer,
                size: pointee_size(self, pointer),
                direction,
            })
            .collect(),
            Op::CopyMemorySized => {
                let size = instruction
                    .operands
                    .get(2)
                    .and_then(id_ref)
                    .and_then(|id| self.constants.get(&id).copied());
                [
                    (pointer(0), AccessDirection::WRITE),
                    (pointer(1), AccessDirection::READ),
                ]
                .into_iter()
                .filter_map(|(pointer, direction)| Some((pointer?, direction)))
                .map(|(pointer, direction)| MemoryAccess {
                    pointer,
                    size,
                    direction,
                })
                .collect()
            }
            opcode if is_atomic(opcode) => pointer(0)
                .map(|pointer| MemoryAccess {
                    pointer,
                    size: result_size().or_else(|| pointee_size(self, pointer)),
                    direction: AccessDirection::READ_WRITE,
                })
                .into_iter()
                .collect(),
            Op::CooperativeMatrixLoadKHR
            | Op::CooperativeMatrixStoreKHR
            | Op::CooperativeMatrixLoadNV
            | Op::CooperativeMatrixStoreNV
            | Op::CooperativeVectorLoadNV
            | Op::CooperativeVectorStoreNV
            | Op::CooperativeMatrixLoadTensorNV
            | Op::CooperativeMatrixStoreTensorNV => pointer(0)
                .map(|pointer| MemoryAccess {
                    pointer,
                    size: None,
                    direction: AccessDirection::READ_WRITE,
                })
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn pointer_pointee(&self, pointer_ty: Word) -> Option<Word> {
        self.definitions
            .get(&pointer_ty)
            .filter(|definition| definition.class.opcode == Op::TypePointer)
            .and_then(|definition| definition.operands.get(1))
            .and_then(id_ref)
    }

    fn access_type_span(&self, pointer: Word, ty: Word) -> Option<u64> {
        self.type_span(ty, &mut HashSet::default()).or_else(|| {
            let layout = self.pointer_matrix_layout(pointer, &mut HashSet::default())?;
            self.type_span_with_matrix_layout(ty, Some(layout), &mut HashSet::default())
        })
    }

    fn pointer_matrix_layout(
        &self,
        pointer: Word,
        visiting: &mut HashSet<Word>,
    ) -> Option<MatrixLayout> {
        if !visiting.insert(pointer) {
            return None;
        }
        let result = (|| {
            let instruction = self.definitions.get(&pointer).copied()?;
            match instruction.class.opcode {
                Op::AccessChain | Op::InBoundsAccessChain => {
                    let base = instruction.operands.first().and_then(id_ref)?;
                    let base_ty = self.value_types.get(&base).copied()?;
                    let pointee = self.pointer_pointee(base_ty)?;
                    self.matrix_layout_after_path(pointee, &instruction.operands[1..])
                }
                Op::CopyObject | Op::Bitcast => self.pointer_matrix_layout(
                    instruction.operands.first().and_then(id_ref)?,
                    visiting,
                ),
                _ => None,
            }
        })();
        visiting.remove(&pointer);
        result
    }

    fn matrix_layout_after_path(&self, mut ty: Word, indices: &[Operand]) -> Option<MatrixLayout> {
        let mut layout = None;
        for operand in indices {
            let definition = self.definitions.get(&ty).copied()?;
            match definition.class.opcode {
                Op::TypeStruct => {
                    let index = id_ref(operand).and_then(|id| self.constants.get(&id).copied())?;
                    let member = u32::try_from(index).ok()?;
                    ty = definition.operands.get(member as usize).and_then(id_ref)?;
                    layout = self
                        .decorations
                        .matrix_layout(definition.result_id?, member);
                }
                Op::TypeArray | Op::TypeRuntimeArray => {
                    ty = definition.operands.first().and_then(id_ref)?;
                }
                Op::TypeMatrix => {
                    ty = definition.operands.first().and_then(id_ref)?;
                }
                Op::TypeVector => {
                    ty = definition.operands.first().and_then(id_ref)?;
                }
                _ => return None,
            }
        }
        layout
    }

    fn type_span(&self, ty: Word, visiting: &mut HashSet<Word>) -> Option<u64> {
        self.type_span_with_matrix_layout(ty, None, visiting)
    }

    fn type_span_with_matrix_layout(
        &self,
        ty: Word,
        matrix_layout: Option<MatrixLayout>,
        visiting: &mut HashSet<Word>,
    ) -> Option<u64> {
        if !visiting.insert(ty) {
            return None;
        }
        let result =
            (|| {
                let definition = self.definitions.get(&ty).copied()?;
                match definition.class.opcode {
                    Op::TypeBool => Some(4),
                    Op::TypeInt | Op::TypeFloat => definition
                        .operands
                        .first()
                        .and_then(literal_u32)
                        .and_then(|bits| u64::from(bits).checked_add(7))
                        .map(|bits| bits / 8),
                    Op::TypeVector => {
                        let element = definition.operands.first().and_then(id_ref)?;
                        let count = definition.operands.get(1).and_then(literal_u32)?;
                        self.type_span_with_matrix_layout(element, matrix_layout, visiting)?
                            .checked_mul(u64::from(count))
                    }
                    Op::TypeArray => {
                        let element = definition.operands.first().and_then(id_ref)?;
                        let count = definition
                            .operands
                            .get(1)
                            .and_then(id_ref)
                            .and_then(|id| self.constants.get(&id).copied())?;
                        if count == 0 {
                            return Some(0);
                        }
                        let stride = *self.decorations.array_strides.get(&ty)?;
                        stride.checked_mul(count - 1)?.checked_add(
                            self.type_span_with_matrix_layout(element, matrix_layout, visiting)?,
                        )
                    }
                    Op::TypeRuntimeArray => None,
                    Op::TypeMatrix => {
                        let layout = matrix_layout?;
                        let column_ty = definition.operands.first().and_then(id_ref)?;
                        let columns = u64::from(definition.operands.get(1).and_then(literal_u32)?);
                        let column_definition = self.definitions.get(&column_ty).copied()?;
                        if column_definition.class.opcode != Op::TypeVector {
                            return None;
                        }
                        let scalar_ty = column_definition.operands.first().and_then(id_ref)?;
                        let rows =
                            u64::from(column_definition.operands.get(1).and_then(literal_u32)?);
                        let scalar_span = self.type_span(scalar_ty, visiting)?;
                        let (major_count, final_vector_span) = if layout.row_major {
                            (rows, columns.checked_mul(scalar_span)?)
                        } else {
                            (
                                columns,
                                self.type_span_with_matrix_layout(column_ty, None, visiting)?,
                            )
                        };
                        if major_count == 0 {
                            Some(0)
                        } else {
                            layout
                                .stride
                                .checked_mul(major_count - 1)?
                                .checked_add(final_vector_span)
                        }
                    }
                    Op::TypeStruct => {
                        let mut end = 0_u64;
                        for (member, operand) in definition.operands.iter().enumerate() {
                            let member_ty = id_ref(operand)?;
                            let offset = *self
                                .decorations
                                .member_offsets
                                .get(&(ty, u32::try_from(member).ok()?))?;
                            let member_layout = self
                                .decorations
                                .matrix_layout(ty, u32::try_from(member).ok()?);
                            end =
                                end.max(offset.checked_add(self.type_span_with_matrix_layout(
                                    member_ty,
                                    member_layout,
                                    visiting,
                                )?)?);
                        }
                        Some(end)
                    }
                    Op::TypePointer => definition.operands.get(1).and_then(id_ref).and_then(|ty| {
                        self.type_span_with_matrix_layout(ty, matrix_layout, visiting)
                    }),
                    _ => None,
                }
            })();
        visiting.remove(&ty);
        result
    }
}

pub(super) struct Analysis {
    footprints: BTreeMap<DescriptorKey, BufferFootprint>,
    pub(super) observed: BTreeMap<DescriptorKey, AccessDirection>,
    pub(super) escaped: BTreeSet<DescriptorKey>,
}

#[derive(Clone, Copy)]
struct MemoryAccess {
    pointer: Word,
    size: Option<u64>,
    direction: AccessDirection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AccessDirection {
    reads: bool,
    pub(super) writes: bool,
}

impl AccessDirection {
    const READ: Self = Self {
        reads: true,
        writes: false,
    };
    const WRITE: Self = Self {
        reads: false,
        writes: true,
    };
    const READ_WRITE: Self = Self {
        reads: true,
        writes: true,
    };
}

impl Decorations {
    fn matrix_layout(&self, struct_ty: Word, member: u32) -> Option<MatrixLayout> {
        self.member_matrix_strides
            .get(&(struct_ty, member))
            .copied()
            .map(|stride| MatrixLayout {
                stride,
                row_major: self.row_major_members.contains(&(struct_ty, member)),
            })
    }

    fn from_module(module: &Module) -> Self {
        let mut result = Self::default();
        for instruction in &module.annotations {
            match instruction.class.opcode {
                Op::Decorate => {
                    let Some(target) = instruction.operands.first().and_then(id_ref) else {
                        continue;
                    };
                    match instruction.operands.get(1) {
                        Some(Operand::Decoration(Decoration::DescriptorSet)) => {
                            if let Some(value) = instruction.operands.get(2).and_then(literal_u32) {
                                result.descriptor_sets.insert(target, value);
                            }
                        }
                        Some(Operand::Decoration(Decoration::Binding)) => {
                            if let Some(value) = instruction.operands.get(2).and_then(literal_u32) {
                                result.bindings.insert(target, value);
                            }
                        }
                        Some(Operand::Decoration(Decoration::BuiltIn)) => {
                            if let Some(Operand::BuiltIn(value)) = instruction.operands.get(2) {
                                result.builtins.insert(target, *value);
                            }
                        }
                        Some(Operand::Decoration(Decoration::ArrayStride)) => {
                            if let Some(value) = instruction.operands.get(2).and_then(literal_u32) {
                                result.array_strides.insert(target, u64::from(value));
                            }
                        }
                        _ => {}
                    }
                }
                Op::MemberDecorate => {
                    let Some(struct_ty) = instruction.operands.first().and_then(id_ref) else {
                        continue;
                    };
                    let Some(member) = instruction.operands.get(1).and_then(literal_u32) else {
                        continue;
                    };
                    match instruction.operands.get(2) {
                        Some(Operand::Decoration(Decoration::Offset)) => {
                            if let Some(offset) = instruction.operands.get(3).and_then(literal_u32)
                            {
                                result
                                    .member_offsets
                                    .insert((struct_ty, member), u64::from(offset));
                            }
                        }
                        Some(Operand::Decoration(Decoration::MatrixStride)) => {
                            if let Some(stride) = instruction.operands.get(3).and_then(literal_u32)
                            {
                                result
                                    .member_matrix_strides
                                    .insert((struct_ty, member), u64::from(stride));
                            }
                        }
                        Some(Operand::Decoration(Decoration::RowMajor)) => {
                            result.row_major_members.insert((struct_ty, member));
                        }
                        Some(Operand::Decoration(Decoration::ColMajor)) => {}
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        result
    }
}

fn constant_value(instruction: &Instruction) -> Option<u64> {
    match instruction.class.opcode {
        Op::Constant => match instruction.operands.first()? {
            Operand::LiteralBit32(value) => Some(u64::from(*value)),
            Operand::LiteralBit64(value) => Some(*value),
            _ => None,
        },
        Op::ConstantNull => Some(0),
        _ => None,
    }
}

fn id_ref(operand: &Operand) -> Option<Word> {
    match operand {
        Operand::IdRef(id) => Some(*id),
        _ => None,
    }
}

fn literal_u32(operand: &Operand) -> Option<u32> {
    match operand {
        Operand::LiteralBit32(value) => Some(*value),
        _ => None,
    }
}

fn builtin_source(builtin: BuiltIn, component: u32) -> Option<BufferIndexSource> {
    use BufferIndexSource::*;
    match (builtin, component) {
        (BuiltIn::VertexIndex, 0) => Some(VertexIndex),
        (BuiltIn::InstanceIndex, 0) => Some(InstanceIndex),
        (BuiltIn::GlobalInvocationId, 0) => Some(GlobalInvocationIdX),
        (BuiltIn::GlobalInvocationId, 1) => Some(GlobalInvocationIdY),
        (BuiltIn::GlobalInvocationId, 2) => Some(GlobalInvocationIdZ),
        (BuiltIn::LocalInvocationId, 0) => Some(LocalInvocationIdX),
        (BuiltIn::LocalInvocationId, 1) => Some(LocalInvocationIdY),
        (BuiltIn::LocalInvocationId, 2) => Some(LocalInvocationIdZ),
        (BuiltIn::WorkgroupId, 0) => Some(WorkgroupIdX),
        (BuiltIn::WorkgroupId, 1) => Some(WorkgroupIdY),
        (BuiltIn::WorkgroupId, 2) => Some(WorkgroupIdZ),
        (BuiltIn::LocalInvocationIndex, 0) => Some(LocalInvocationIndex),
        _ => None,
    }
}

fn is_atomic(opcode: Op) -> bool {
    matches!(
        opcode,
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
}

fn pointer_operand_is_modeled(opcode: Op, position: usize) -> bool {
    match opcode {
        Op::AccessChain
        | Op::InBoundsAccessChain
        | Op::PtrAccessChain
        | Op::InBoundsPtrAccessChain
        | Op::CopyObject
        | Op::Bitcast
        | Op::Load
        | Op::ArrayLength => position == 0,
        Op::Store => position == 0,
        Op::CopyMemory | Op::CopyMemorySized => position <= 1,
        Op::Select => matches!(position, 1 | 2),
        Op::Phi => position.is_multiple_of(2),
        Op::PtrEqual | Op::PtrNotEqual => position <= 1,
        opcode if is_atomic(opcode) => position == 0,
        Op::CooperativeMatrixLoadKHR
        | Op::CooperativeMatrixStoreKHR
        | Op::CooperativeMatrixLoadNV
        | Op::CooperativeMatrixStoreNV
        | Op::CooperativeVectorLoadNV
        | Op::CooperativeVectorStoreNV
        | Op::CooperativeMatrixLoadTensorNV
        | Op::CooperativeMatrixStoreTensorNV => position == 0,
        _ => false,
    }
}

fn coalesce_static_ranges(ranges: &mut Vec<BufferByteRange>) {
    ranges.sort();
    let mut merged = Vec::<BufferByteRange>::with_capacity(ranges.len());
    for range in ranges.iter().cloned() {
        let Some(end) = range.offset.checked_add(range.size) else {
            continue;
        };
        if let Some(previous) = merged.last_mut() {
            let previous_end = previous.offset.saturating_add(previous.size);
            if range.offset <= previous_end {
                previous.size = previous_end.max(end).saturating_sub(previous.offset);
                continue;
            }
        }
        merged.push(range);
    }
    *ranges = merged;
}

fn normalize_addresses(addresses: &mut Vec<Address>) {
    addresses.sort();
    addresses.dedup();
    let unknown_roots = addresses
        .iter()
        .filter(|address| address.offset.is_none())
        .map(|address| address.root)
        .collect::<BTreeSet<_>>();
    if !unknown_roots.is_empty() {
        addresses
            .retain(|address| address.offset.is_none() || !unknown_roots.contains(&address.root));
    }
    if addresses.len() > MAX_ADDRESS_ALTERNATIVES_PER_POINTER {
        *addresses = addresses
            .iter()
            .map(|address| address.root)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|root| Address { root, offset: None })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_sources_cover_draw_and_dispatch_index_domains() {
        use BufferIndexSource::*;
        assert_eq!(builtin_source(BuiltIn::VertexIndex, 0), Some(VertexIndex));
        assert_eq!(
            builtin_source(BuiltIn::InstanceIndex, 0),
            Some(InstanceIndex)
        );
        for (builtin, expected) in [
            (
                BuiltIn::GlobalInvocationId,
                [
                    GlobalInvocationIdX,
                    GlobalInvocationIdY,
                    GlobalInvocationIdZ,
                ],
            ),
            (
                BuiltIn::LocalInvocationId,
                [LocalInvocationIdX, LocalInvocationIdY, LocalInvocationIdZ],
            ),
            (
                BuiltIn::WorkgroupId,
                [WorkgroupIdX, WorkgroupIdY, WorkgroupIdZ],
            ),
        ] {
            for (component, expected) in expected.into_iter().enumerate() {
                assert_eq!(builtin_source(builtin, component as u32), Some(expected));
            }
            assert_eq!(builtin_source(builtin, 3), None);
        }
        assert_eq!(
            builtin_source(BuiltIn::LocalInvocationIndex, 0),
            Some(LocalInvocationIndex)
        );
        assert_eq!(builtin_source(BuiltIn::FragCoord, 0), None);
    }

    #[test]
    fn static_ranges_coalesce_overlap_and_adjacency() {
        let mut ranges = vec![
            BufferByteRange {
                offset: 20,
                size: 4,
            },
            BufferByteRange { offset: 4, size: 8 },
            BufferByteRange { offset: 0, size: 4 },
            BufferByteRange {
                offset: 10,
                size: 12,
            },
        ];
        coalesce_static_ranges(&mut ranges);
        assert_eq!(
            ranges,
            [BufferByteRange {
                offset: 0,
                size: 24
            }]
        );
    }

    #[test]
    fn unknown_pointer_alternative_subsumes_known_offsets_for_its_root() {
        let root = DescriptorKey { set: 0, binding: 2 };
        let mut addresses = vec![
            Address {
                root,
                offset: Some(ScalarExpr::constant(12)),
            },
            Address { root, offset: None },
        ];
        normalize_addresses(&mut addresses);
        assert_eq!(addresses, [Address { root, offset: None }]);
    }

    #[test]
    fn addressing_is_logical_only_for_a_declared_logical_memory_model() {
        use spirv::AddressingModel;
        let with = |addressing| {
            let mut module = Module::new();
            module.memory_model = Some(Instruction::new(
                Op::MemoryModel,
                None,
                None,
                vec![
                    Operand::AddressingModel(addressing),
                    Operand::MemoryModel(spirv::MemoryModel::GLSL450),
                ],
            ));
            addressing_is_logical(&module)
        };
        assert!(with(AddressingModel::Logical));
        assert!(!with(AddressingModel::PhysicalStorageBuffer64));
        assert!(!with(AddressingModel::Physical64));
        assert!(!addressing_is_logical(&Module::new()));
    }
}
