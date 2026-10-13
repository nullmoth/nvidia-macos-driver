use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Function, Instruction};
use spirv::{Op, StorageClass, Word};

pub(super) struct ReloopDecline {
    pub(super) function: Option<Word>,
    pub(super) blocks: usize,
    pub(super) reason: &'static str,
}

pub(super) const TOO_MANY_BLOCKS: &str = "too-many-blocks";

pub(super) fn relooper_block_ceiling(max_blocks: usize) -> usize {
    effective_relooper_block_cap(max_blocks).saturating_mul(MAX_RELOOPER_GROUPS)
}

pub(super) fn rewrite_to_relooper(module: &mut Module, max_blocks: usize) -> bool {
    rewrite_to_relooper_if(module, max_blocks, |_| true).0
}

pub(super) fn rewrite_selected_to_relooper(
    module: &mut Module,
    max_blocks: usize,
    selected: &HashSet<Word>,
) -> (bool, Vec<ReloopDecline>) {
    rewrite_to_relooper_if(module, max_blocks, |function| {
        function
            .def
            .as_ref()
            .and_then(|def| def.result_id)
            .is_some_and(|id| selected.contains(&id))
    })
}

fn rewrite_to_relooper_if(
    module: &mut Module,
    max_blocks: usize,
    mut selected: impl FnMut(&Function) -> bool,
) -> (bool, Vec<ReloopDecline>) {
    let max_blocks = effective_relooper_block_cap(max_blocks);
    let mut next_id = module.header.as_ref().map(|h| h.bound).unwrap_or(1);
    let mut tc = TypeCtx::new(module, &mut next_id);

    let convergent = convergent_functions(module);
    let mut changed = false;
    let mut declines = Vec::new();
    if crate::env_vars::reloop_why() {
        eprintln!("RELOOP-ENTER functions={}", module.functions.len());
    }
    for function in &mut module.functions {
        if !selected(function) {
            continue;
        }
        let id = function.def.as_ref().and_then(|def| def.result_id);
        let blocks = function.blocks.len();
        match rewrite_function(function, &mut tc, max_blocks, &convergent) {
            Ok(()) => changed = true,
            Err(reason) => declines.push(ReloopDecline {
                function: id,
                blocks,
                reason,
            }),
        }
    }
    tc.flush(module);
    if let Some(h) = module.header.as_mut() {
        h.bound = next_id;
    }
    (changed, declines)
}

pub(super) struct TypeCtx<'a> {
    next_id: &'a mut Word,
    int_types: HashMap<(u32, u32), Word>,
    int_widths: HashMap<Word, u32>,
    bool_type: Option<Word>,
    pointer_types: HashMap<(StorageClass, Word), Word>,
    int_consts: HashMap<(Word, u64), Word>,
    type_op: HashMap<Word, Op>,
    type_defs: HashMap<Word, Instruction>,
    value_types: HashMap<Word, Word>,
    const_values: HashMap<Word, u64>,
    explicitly_laid_out: HashSet<Word>,
    array_strides: HashMap<Word, u32>,
    pending: Vec<Instruction>,
    pending_annotations: Vec<Instruction>,
}

impl<'a> TypeCtx<'a> {
    pub(super) fn new(module: &Module, next_id: &'a mut Word) -> Self {
        let mut int_types = HashMap::default();
        let mut int_widths = HashMap::default();
        let mut bool_type = None;
        let mut pointer_types = HashMap::default();
        let mut int_consts = HashMap::default();
        let mut type_op = HashMap::default();
        let mut type_defs = HashMap::default();
        let mut value_types = HashMap::default();
        let mut const_values = HashMap::default();
        for inst in &module.types_global_values {
            let Some(rid) = inst.result_id else { continue };
            type_op.insert(rid, inst.class.opcode);
            type_defs.insert(rid, inst.clone());
            if let Some(result_type) = inst.result_type {
                value_types.insert(rid, result_type);
            }
            match inst.class.opcode {
                Op::TypeInt => {
                    if let (Some(Operand::LiteralBit32(w)), Some(Operand::LiteralBit32(s))) =
                        (inst.operands.first(), inst.operands.get(1))
                    {
                        int_types.insert((*w, *s), rid);
                        int_widths.insert(rid, *w);
                    }
                }
                Op::TypeBool => bool_type = Some(rid),
                Op::TypePointer => {
                    if let (Some(Operand::StorageClass(storage)), Some(Operand::IdRef(pointee))) =
                        (inst.operands.first(), inst.operands.get(1))
                    {
                        pointer_types.insert((*storage, *pointee), rid);
                    }
                }
                Op::Constant => {
                    let value = match inst.operands.first() {
                        Some(Operand::LiteralBit32(value)) => Some(*value as u64),
                        Some(Operand::LiteralBit64(value)) => Some(*value),
                        _ => None,
                    };
                    if let (Some(ty), Some(value)) = (inst.result_type, value) {
                        int_consts.insert((ty, value), rid);
                        const_values.insert(rid, value);
                    }
                }
                _ => {}
            }
        }
        let array_strides = module
            .annotations
            .iter()
            .filter_map(|inst| {
                if inst.class.opcode != Op::Decorate
                    || inst.operands.get(1)
                        != Some(&Operand::Decoration(spirv::Decoration::ArrayStride))
                {
                    return None;
                }
                let (Some(Operand::IdRef(pointer)), Some(Operand::LiteralBit32(stride))) =
                    (inst.operands.first(), inst.operands.get(2))
                else {
                    return None;
                };
                Some((*pointer, *stride))
            })
            .collect();
        TypeCtx {
            next_id,
            int_types,
            int_widths,
            bool_type,
            pointer_types,
            int_consts,
            type_op,
            type_defs,
            value_types,
            const_values,
            explicitly_laid_out: module
                .annotations
                .iter()
                .filter_map(|inst| {
                    let explicit = match inst.class.opcode {
                        Op::Decorate => matches!(
                            inst.operands.get(1),
                            Some(Operand::Decoration(
                                spirv::Decoration::ArrayStride
                                    | spirv::Decoration::Block
                                    | spirv::Decoration::BufferBlock
                            ))
                        ),
                        Op::MemberDecorate => matches!(
                            inst.operands.get(2),
                            Some(Operand::Decoration(
                                spirv::Decoration::Offset
                                    | spirv::Decoration::MatrixStride
                                    | spirv::Decoration::RowMajor
                                    | spirv::Decoration::ColMajor
                            ))
                        ),
                        _ => false,
                    };
                    explicit.then(|| inst.operands.first()).flatten()
                })
                .filter_map(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                })
                .collect(),
            array_strides,
            pending: Vec::new(),
            pending_annotations: Vec::new(),
        }
    }

    pub(super) fn fresh(&mut self) -> Word {
        let id = *self.next_id;
        *self.next_id += 1;
        id
    }

    pub(super) fn type_opcode(&self, ty: Word) -> Option<Op> {
        self.type_op.get(&ty).copied()
    }

    pub(super) fn value_type(&self, value: Word) -> Option<Word> {
        self.value_types.get(&value).copied()
    }

    fn composite_members(&self, ty: Word) -> Option<Vec<Word>> {
        let def = self.type_defs.get(&ty)?;
        match def.class.opcode {
            Op::TypeStruct => Some(
                def.operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Operand::IdRef(member) => Some(*member),
                        _ => None,
                    })
                    .collect(),
            ),
            Op::TypeArray => {
                let Operand::IdRef(element) = def.operands.first()? else {
                    return None;
                };
                let Operand::IdRef(length) = def.operands.get(1)? else {
                    return None;
                };
                let length = usize::try_from(*self.const_values.get(length)?).ok()?;
                Some(vec![*element; length])
            }
            _ => None,
        }
    }

    fn index_path_selection(&self, indices: &[Operand], base_pointee: Word) -> Option<Word> {
        let mut selected = base_pointee;
        for operand in indices {
            let &Operand::IdRef(index) = operand else {
                return None;
            };
            selected = match self.type_opcode(selected)? {
                Op::TypeStruct => {
                    let member = usize::try_from(*self.const_values.get(&index)?).ok()?;
                    self.composite_members(selected)?.get(member).copied()?
                }
                Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector | Op::TypeMatrix => {
                    match self.type_defs.get(&selected)?.operands.first()? {
                        Operand::IdRef(element) => *element,
                        _ => return None,
                    }
                }
                _ => return None,
            };
        }
        Some(selected)
    }

    fn has_explicit_layout_reachable(&self, ty: Word) -> bool {
        let mut pending = vec![ty];
        let mut seen = HashSet::default();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            if self.explicitly_laid_out.contains(&current) {
                return true;
            }
            if let Some(members) = self.composite_members(current) {
                pending.extend(members);
            }
        }
        false
    }

    fn int_ty(&mut self, width: u32, signed: u32) -> Word {
        if let Some(&id) = self.int_types.get(&(width, signed)) {
            return id;
        }
        let id = self.fresh();
        self.pending.push(Instruction::new(
            Op::TypeInt,
            None,
            Some(id),
            vec![Operand::LiteralBit32(width), Operand::LiteralBit32(signed)],
        ));
        self.type_op.insert(id, Op::TypeInt);
        self.int_types.insert((width, signed), id);
        self.int_widths.insert(id, width);
        id
    }

    pub(super) fn i32_ty(&mut self) -> Word {
        self.int_ty(32, 0)
    }

    fn bool_ty(&mut self) -> Word {
        if let Some(id) = self.bool_type {
            return id;
        }
        let id = self.fresh();
        self.pending
            .push(Instruction::new(Op::TypeBool, None, Some(id), vec![]));
        self.type_op.insert(id, Op::TypeBool);
        self.bool_type = Some(id);
        id
    }

    pub(super) fn ptr_function(&mut self, pointee: Word) -> Word {
        self.ptr(StorageClass::Function, pointee)
    }

    fn pointer_shape(&self, ty: Word) -> Option<(StorageClass, Word)> {
        let definition = self.type_defs.get(&ty)?;
        let (Some(Operand::StorageClass(storage)), Some(Operand::IdRef(pointee))) =
            (definition.operands.first(), definition.operands.get(1))
        else {
            return None;
        };
        Some((*storage, *pointee))
    }

    fn ptr(&mut self, storage: StorageClass, pointee: Word) -> Word {
        if let Some(&id) = self.pointer_types.get(&(storage, pointee)) {
            return id;
        }
        let id = self.fresh();
        self.pending.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(id),
            vec![Operand::StorageClass(storage), Operand::IdRef(pointee)],
        ));
        self.type_op.insert(id, Op::TypePointer);
        self.type_defs.insert(
            id,
            Instruction::new(
                Op::TypePointer,
                None,
                Some(id),
                vec![Operand::StorageClass(storage), Operand::IdRef(pointee)],
            ),
        );
        self.pointer_types.insert((storage, pointee), id);
        id
    }

    fn ensure_array_stride(&mut self, pointer: Word, stride: u32) {
        if self.array_strides.get(&pointer) == Some(&stride) {
            return;
        }
        self.pending_annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(pointer),
                Operand::Decoration(spirv::Decoration::ArrayStride),
                Operand::LiteralBit32(stride),
            ],
        ));
        self.array_strides.insert(pointer, stride);
    }

    pub(super) fn int_const(&mut self, ty: Word, value: u64) -> Word {
        if let Some(&id) = self.int_consts.get(&(ty, value)) {
            return id;
        }
        let id = self.fresh();
        self.pending.push(Instruction::new(
            Op::Constant,
            Some(ty),
            Some(id),
            vec![if self.int_widths.get(&ty) == Some(&64) {
                Operand::LiteralBit64(value)
            } else {
                Operand::LiteralBit32(value as u32)
            }],
        ));
        self.int_consts.insert((ty, value), id);
        id
    }

    pub(super) fn flush(self, module: &mut Module) {
        module.types_global_values.extend(self.pending);
        module.annotations.extend(self.pending_annotations);
    }
}

#[derive(Clone)]
pub(super) enum Term {
    Branch(Word),
    BranchCond(Word, Word, Word),
    Switch(Word, Word, Vec<(u64, Word)>),
    Return,
    ReturnValue(Word),
    Unreachable,
    Kill(Instruction),
}

const MAX_SCALARIZED_SPILL_LEAVES: usize = 64;
const MAX_SCALARIZED_SPILL_LEAVES_PER_FUNCTION: usize = 512;

enum Spill {
    Direct { var: Word, ty: Word },
    Composite { ty: Word, members: Vec<Spill> },
}

fn scalarized_spill_leaf_count(tc: &TypeCtx<'_>, ty: Word) -> Option<usize> {
    if !tc.has_explicit_layout_reachable(ty) {
        return Some(1);
    }
    let members = tc.composite_members(ty)?;
    let mut leaves = 0usize;
    for member in members {
        leaves = leaves.checked_add(scalarized_spill_leaf_count(tc, member)?)?;
        if leaves > MAX_SCALARIZED_SPILL_LEAVES {
            return None;
        }
    }
    Some(leaves)
}

fn build_spill(tc: &mut TypeCtx<'_>, ty: Word, variables: &mut Vec<Instruction>) -> Spill {
    if tc.has_explicit_layout_reachable(ty) {
        if let Some(member_types) = tc.composite_members(ty) {
            return Spill::Composite {
                ty,
                members: member_types
                    .into_iter()
                    .map(|member| build_spill(tc, member, variables))
                    .collect(),
            };
        }
    }
    let ptr_ty = tc.ptr_function(ty);
    let var = tc.fresh();
    variables.push(Instruction::new(
        Op::Variable,
        Some(ptr_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::Function)],
    ));
    Spill::Direct { var, ty }
}

fn store_spill(
    tc: &mut TypeCtx<'_>,
    spill: &Spill,
    value: Word,
    instructions: &mut Vec<Instruction>,
) {
    match spill {
        Spill::Direct { var, .. } => instructions.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(*var), Operand::IdRef(value)],
        )),
        Spill::Composite { members, .. } => {
            for (index, member) in members.iter().enumerate() {
                let member_ty = match member {
                    Spill::Direct { ty, .. } | Spill::Composite { ty, .. } => *ty,
                };
                let extracted = tc.fresh();
                instructions.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(member_ty),
                    Some(extracted),
                    vec![Operand::IdRef(value), Operand::LiteralBit32(index as u32)],
                ));
                store_spill(tc, member, extracted, instructions);
            }
        }
    }
}

fn load_spill(tc: &mut TypeCtx<'_>, spill: &Spill, instructions: &mut Vec<Instruction>) -> Word {
    match spill {
        Spill::Direct { var, ty } => {
            let loaded = tc.fresh();
            instructions.push(Instruction::new(
                Op::Load,
                Some(*ty),
                Some(loaded),
                vec![Operand::IdRef(*var)],
            ));
            loaded
        }
        Spill::Composite { ty, members } => {
            let values = members
                .iter()
                .map(|member| Operand::IdRef(load_spill(tc, member, instructions)))
                .collect();
            let reconstructed = tc.fresh();
            instructions.push(Instruction::new(
                Op::CompositeConstruct,
                Some(*ty),
                Some(reconstructed),
                values,
            ));
            reconstructed
        }
    }
}

pub(super) fn decode_term(inst: &Instruction) -> Option<Term> {
    match inst.class.opcode {
        Op::Branch => match inst.operands.first()? {
            Operand::IdRef(t) => Some(Term::Branch(*t)),
            _ => None,
        },
        Op::BranchConditional => {
            let (Operand::IdRef(c), Operand::IdRef(t), Operand::IdRef(f)) = (
                inst.operands.first()?,
                inst.operands.get(1)?,
                inst.operands.get(2)?,
            ) else {
                return None;
            };
            Some(Term::BranchCond(*c, *t, *f))
        }
        Op::Switch => {
            let Operand::IdRef(sel) = inst.operands.first()? else {
                return None;
            };
            let Operand::IdRef(def) = inst.operands.get(1)? else {
                return None;
            };
            let mut cases = Vec::new();
            let mut i = 2;
            while i + 1 < inst.operands.len() {
                let lit = match &inst.operands[i] {
                    Operand::LiteralBit32(v) => *v as u64,
                    Operand::LiteralBit64(v) => *v,
                    _ => return None,
                };
                let Operand::IdRef(lbl) = &inst.operands[i + 1] else {
                    return None;
                };
                cases.push((lit, *lbl));
                i += 2;
            }
            Some(Term::Switch(*sel, *def, cases))
        }
        Op::Return => Some(Term::Return),
        Op::ReturnValue => match inst.operands.first()? {
            Operand::IdRef(v) => Some(Term::ReturnValue(*v)),
            _ => None,
        },
        Op::Unreachable => Some(Term::Unreachable),
        Op::Kill | Op::TerminateInvocation => Some(Term::Kill(inst.clone())),
        _ => None,
    }
}

pub(super) fn block_label(block: &Block) -> Option<Word> {
    block.label.as_ref().and_then(|l| l.result_id)
}

const MAX_RELOOPER_BLOCKS: usize = 1024;

const MAX_DRIVER_SAFE_RELOOPER_BLOCKS: usize = MAX_RELOOPER_BLOCKS;
const MAX_RELOOPER_GROUPS: usize = 8;

fn effective_relooper_block_cap(requested: usize) -> usize {
    requested.min(MAX_DRIVER_SAFE_RELOOPER_BLOCKS)
}

pub(super) fn default_max_relooper_blocks() -> usize {
    effective_relooper_block_cap(crate::env_vars::relooper_max_blocks(MAX_RELOOPER_BLOCKS))
}

fn rewrite_function(
    function: &mut Function,
    tc: &mut TypeCtx,
    max_blocks: usize,
    convergent: &HashSet<Word>,
) -> Result<(), &'static str> {
    let bail = |reason: &'static str| -> Result<(), &'static str> {
        if crate::env_vars::reloop_why() {
            eprintln!("RELOOP-BAIL {reason} (blocks={})", function.blocks.len());
        }
        Err(reason)
    };

    if crate::env_vars::reloop_why() {
        eprintln!(
            "RELOOP-FN blocks={} max={}",
            function.blocks.len(),
            max_blocks
        );
    }
    if function.blocks.len() < 2 {
        return bail("too-few-blocks");
    }
    if function.blocks.len() > max_blocks.saturating_mul(MAX_RELOOPER_GROUPS) {
        return bail(TOO_MANY_BLOCKS);
    }

    let mut terms: Vec<Term> = Vec::with_capacity(function.blocks.len());
    for block in &function.blocks {
        let Some(last) = block.instructions.last() else {
            return bail("empty-block");
        };
        match decode_term(last) {
            Some(t) => terms.push(t),
            None => {
                if crate::env_vars::reloop_why() {
                    eprintln!(
                        "RELOOP-UNHANDLED-TERMINATOR opcode={:?} operands={:?}",
                        last.class.opcode, last.operands
                    );
                }
                return bail("unhandled-terminator");
            }
        }
    }

    let labels: Vec<Word> = match function
        .blocks
        .iter()
        .map(block_label)
        .collect::<Option<_>>()
    {
        Some(v) => v,
        None => return bail("block-without-label"),
    };
    let label_index: HashMap<Word, usize> =
        labels.iter().enumerate().map(|(i, l)| (*l, i)).collect();

    let entry_term = &terms[0];
    let entry_has_successor = matches!(
        entry_term,
        Term::Branch(_) | Term::BranchCond(..) | Term::Switch(..)
    );
    let mut all_targets: HashSet<Word> = HashSet::default();
    for t in &terms {
        match t {
            Term::Branch(x) => {
                all_targets.insert(*x);
            }
            Term::BranchCond(_, a, b) => {
                all_targets.insert(*a);
                all_targets.insert(*b);
            }
            Term::Switch(_, d, cs) => {
                all_targets.insert(*d);
                for (_, l) in cs {
                    all_targets.insert(*l);
                }
            }
            _ => {}
        }
    }
    if !entry_has_successor {
        return bail("entry-no-successor");
    }
    if !all_targets
        .iter()
        .all(|target| label_index.contains_key(target))
    {
        return bail("missing-target");
    }
    if all_targets.contains(&labels[0]) {
        return bail("entry-is-branch-target");
    }
    if function.blocks[0]
        .instructions
        .iter()
        .any(|i| i.class.opcode == Op::Phi)
    {
        return bail("entry-has-phi");
    }

    let mut def_block: HashMap<Word, usize> = HashMap::default();
    let mut phis: HashMap<Word, (Word, Vec<(Word, Word)>)> = HashMap::default();
    let mut value_type: HashMap<Word, Word> = HashMap::default();
    let mut variables: Vec<Instruction> = Vec::new();
    let mut variable_ids: HashSet<Word> = HashSet::default();
    let mut ptr_def: HashMap<Word, Instruction> = HashMap::default();

    for (bi, block) in function.blocks.iter().enumerate() {
        for inst in &block.instructions {
            if let Some(rid) = inst.result_id {
                if let Some(rty) = inst.result_type {
                    value_type.insert(rid, rty);
                }
                def_block.insert(rid, bi);
                let rematerializable_shape = matches!(
                    inst.class.opcode,
                    Op::AccessChain
                        | Op::InBoundsAccessChain
                        | Op::PtrAccessChain
                        | Op::Select
                        | Op::CopyObject
                        | Op::ConvertUToPtr
                ) || (inst.class.opcode == Op::Load
                    && matches!(
                        inst.result_type.and_then(|ty| tc.type_opcode(ty)),
                        Some(
                            Op::TypeImage
                                | Op::TypeSampler
                                | Op::TypeSampledImage
                                | Op::TypeAccelerationStructureKHR
                        )
                    ));
                if rematerializable_shape {
                    ptr_def.insert(rid, inst.clone());
                }
            }
            match inst.class.opcode {
                Op::Phi => {
                    let rid = inst.result_id.unwrap();
                    let rty = match inst.result_type {
                        Some(t) => t,
                        None => return bail("phi-without-result-type"),
                    };
                    let mut incoming = Vec::new();
                    let mut k = 0;
                    while k + 1 < inst.operands.len() {
                        let (Operand::IdRef(v), Operand::IdRef(p)) =
                            (&inst.operands[k], &inst.operands[k + 1])
                        else {
                            return bail("malformed-phi-operands");
                        };
                        incoming.push((*v, *p));
                        k += 2;
                    }
                    phis.insert(rid, (rty, incoming));
                }
                Op::Variable => {
                    variables.push(inst.clone());
                    variable_ids.insert(inst.result_id.unwrap_or(0));
                }
                _ => {}
            }
        }
    }

    let block_phis: Vec<Vec<Word>> = function
        .blocks
        .iter()
        .map(|b| {
            b.instructions
                .iter()
                .filter(|i| i.class.opcode == Op::Phi)
                .filter_map(|i| i.result_id)
                .collect()
        })
        .collect();

    let param_ids: HashSet<Word> = function
        .parameters
        .iter()
        .filter_map(|p| p.result_id)
        .collect();

    let mut demote: HashSet<Word> = phis.keys().copied().collect();
    for (bi, block) in function.blocks.iter().enumerate() {
        for inst in &block.instructions {
            for op in &inst.operands {
                if let Operand::IdRef(id) = op {
                    if param_ids.contains(id) || variable_ids.contains(id) {
                        continue;
                    }
                    if let Some(&db) = def_block.get(id) {
                        if db != bi && db != 0 {
                            demote.insert(*id);
                        }
                    }
                }
            }
        }
    }

    for (_, incoming) in phis.values() {
        for (val, pred) in incoming {
            if param_ids.contains(val) || variable_ids.contains(val) {
                continue;
            }
            let Some(&pred_bi) = label_index.get(pred) else {
                continue;
            };
            if let Some(&db) = def_block.get(val) {
                if db != pred_bi && db != 0 {
                    demote.insert(*val);
                }
            }
        }
    }

    let dominates_all_cases = |id: &Word| -> bool {
        !def_block.contains_key(id)
            || def_block.get(id) == Some(&0)
            || param_ids.contains(id)
            || variable_ids.contains(id)
    };

    let mut remat: HashMap<Word, Instruction> = HashMap::default();
    let mut remat_scalar_demote: HashSet<Word> = HashSet::default();
    loop {
        let mut newly: Vec<(Word, Instruction, Vec<Word>)> = Vec::new();
        for (&v, inst) in &ptr_def {
            if remat.contains_key(&v) {
                continue;
            }
            let Some(&ty) = value_type.get(&v) else {
                continue;
            };
            let ty_op = tc.type_opcode(ty);
            if !matches!(
                ty_op,
                Some(
                    Op::TypePointer
                        | Op::TypeImage
                        | Op::TypeSampler
                        | Op::TypeSampledImage
                        | Op::TypeAccelerationStructureKHR
                )
            ) {
                continue;
            }
            let ids: Option<Vec<Word>> = inst
                .operands
                .iter()
                .map(|o| match o {
                    Operand::IdRef(i) => Some(*i),
                    _ => None,
                })
                .collect();
            let Some(ids) = ids else { continue };
            let (ptr_ops, scalar_ops): (&[Word], &[Word]) = match inst.class.opcode {
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
                    if ids.len() >= 2 =>
                {
                    (&ids[..1], &ids[1..])
                }
                Op::Select if ids.len() == 3 => (&ids[1..3], &ids[..1]),
                Op::CopyObject if ids.len() == 1 => (&ids[..1], &ids[..0]),
                Op::ConvertUToPtr if ids.len() == 1 => (&ids[..0], &ids[..1]),
                Op::Load
                    if ids.len() == 1
                        && matches!(
                            ty_op,
                            Some(
                                Op::TypeImage
                                    | Op::TypeSampler
                                    | Op::TypeSampledImage
                                    | Op::TypeAccelerationStructureKHR
                            )
                        ) =>
                {
                    (&ids[..1], &ids[..0])
                }
                _ => continue,
            };
            if !ptr_ops
                .iter()
                .all(|p| dominates_all_cases(p) || remat.contains_key(p))
            {
                continue;
            }
            let mut pending: Vec<Word> = Vec::new();
            let mut ok = true;
            for s in scalar_ops {
                if dominates_all_cases(s) {
                    continue;
                }
                match value_type.get(s).and_then(|t| tc.type_opcode(*t)) {
                    Some(Op::TypeInt | Op::TypeFloat | Op::TypeVector | Op::TypeBool) => {
                        pending.push(*s)
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            newly.push((v, inst.clone(), pending));
        }
        if newly.is_empty() {
            break;
        }
        for (v, inst, pending) in newly {
            remat.insert(v, inst);
            remat_scalar_demote.extend(pending);
        }
    }
    for v in remat.keys() {
        demote.remove(v);
    }
    demote.extend(remat_scalar_demote);

    let mut remat_phi: HashMap<Word, (Word, Vec<(Word, Word)>)> = HashMap::default();
    loop {
        let mut added = false;
        for (&pid, (pty, incoming)) in &phis {
            if remat_phi.contains_key(&pid)
                || !demote.contains(&pid)
                || tc.type_opcode(*pty) != Some(Op::TypePointer)
            {
                continue;
            }
            if incoming.iter().all(|(v, _)| {
                *v != pid
                    && (dominates_all_cases(v)
                        || remat.contains_key(v)
                        || remat_phi.contains_key(v))
            }) {
                remat_phi.insert(pid, (*pty, incoming.clone()));
                added = true;
            }
        }
        if !added {
            break;
        }
    }
    for pid in remat_phi.keys() {
        demote.remove(pid);
    }

    let mut remat_phi_invariant: HashSet<Word> = HashSet::default();
    for (&pid, (_, incoming)) in &remat_phi {
        let key_of = |v: Word| -> (u32, Vec<Operand>) {
            match remat.get(&v) {
                Some(inst) => (inst.class.opcode as u32, inst.operands.clone()),
                None => (u32::MAX, vec![Operand::IdRef(v)]),
            }
        };
        let mut keys = incoming.iter().map(|(v, _)| key_of(*v));
        if let Some(first) = keys.next() {
            if keys.all(|k| k == first) {
                remat_phi_invariant.insert(pid);
            }
        }
    }

    let mut scalarized_spill_leaves = 0usize;
    for v in &demote {
        let Some(&ty) = value_type.get(v) else {
            return bail("non-spillable-demote");
        };
        match tc.type_opcode(ty) {
            Some(Op::TypePointer)
            | Some(Op::TypeImage)
            | Some(Op::TypeSampler)
            | Some(Op::TypeSampledImage)
            | Some(Op::TypeAccelerationStructureKHR)
            | Some(Op::TypeRuntimeArray)
            | None => {
                if crate::env_vars::reloop_why() {
                    let def_op = def_block.get(v).and_then(|&bi| {
                        function.blocks[bi]
                            .instructions
                            .iter()
                            .find(|inst| inst.result_id == Some(*v))
                            .map(|inst| inst.class.opcode)
                    });
                    let def = def_block.get(v).and_then(|&bi| {
                        function.blocks[bi]
                            .instructions
                            .iter()
                            .find(|inst| inst.result_id == Some(*v))
                    });
                    eprintln!(
                        "RELOOP-BAIL-VALUE id=%{v} type=%{ty} type_op={:?} def_op={:?} operands={:?}",
                        tc.type_opcode(ty),
                        def_op,
                        def.map(|inst| &inst.operands)
                    );
                    if let Some((_, incoming)) = phis.get(v) {
                        for (incoming_value, predecessor) in incoming {
                            let incoming_def = def_block.get(incoming_value).and_then(|&bi| {
                                function.blocks[bi]
                                    .instructions
                                    .iter()
                                    .find(|inst| inst.result_id == Some(*incoming_value))
                            });
                            eprintln!(
                                "RELOOP-BAIL-PHI-ARM value=%{incoming_value} predecessor=%{predecessor} def_op={:?} operands={:?}",
                                incoming_def.map(|inst| inst.class.opcode),
                                incoming_def.map(|inst| &inst.operands)
                            );
                        }
                    }
                    if let Some(definition) = def {
                        for operand in &definition.operands {
                            let Operand::IdRef(dependency) = operand else {
                                continue;
                            };
                            if let Some((_, incoming)) = phis.get(dependency) {
                                eprintln!(
                                    "RELOOP-BAIL-DEPENDENCY-PHI id=%{dependency} operands={:?}",
                                    definition.operands
                                );
                                for (incoming_value, predecessor) in incoming {
                                    let incoming_def =
                                        def_block.get(incoming_value).and_then(|&bi| {
                                            function.blocks[bi].instructions.iter().find(|inst| {
                                                inst.result_id == Some(*incoming_value)
                                            })
                                        });
                                    eprintln!(
                                        "RELOOP-BAIL-DEPENDENCY-PHI-ARM value=%{incoming_value} predecessor=%{predecessor} def_op={:?} operands={:?}",
                                        incoming_def.map(|inst| inst.class.opcode),
                                        incoming_def.map(|inst| &inst.operands)
                                    );
                                }
                            }
                        }
                    }
                }
                return bail("non-spillable-demote");
            }
            _ => {}
        }
        if tc.has_explicit_layout_reachable(ty) {
            let Some(leaves) = scalarized_spill_leaf_count(tc, ty) else {
                return bail("explicit-layout-spill-too-large");
            };
            let Some(total) = scalarized_spill_leaves.checked_add(leaves) else {
                return bail("explicit-layout-spill-total-too-large");
            };
            scalarized_spill_leaves = total;
            if scalarized_spill_leaves > MAX_SCALARIZED_SPILL_LEAVES_PER_FUNCTION {
                return bail("explicit-layout-spill-total-too-large");
            }
        }
    }

    let i32_ty = tc.i32_ty();
    let ptr_i32 = tc.ptr_function(i32_ty);
    let bool_ty = tc.bool_ty();

    let mut demote_order: Vec<Word> = demote.iter().copied().collect();
    demote_order.sort_unstable();
    let mut spill: HashMap<Word, Spill> = HashMap::default();
    let mut spill_vars: Vec<Instruction> = Vec::new();
    for &v in &demote_order {
        let ty = value_type[&v];
        spill.insert(v, build_spill(tc, ty, &mut spill_vars));
    }

    let mut remat_phi_order: Vec<Word> = remat_phi.keys().copied().collect();
    remat_phi_order.sort_unstable();
    let mut phi_tag: HashMap<Word, Word> = HashMap::default();
    for &pid in &remat_phi_order {
        let var = tc.fresh();
        spill_vars.push(Instruction::new(
            Op::Variable,
            Some(ptr_i32),
            Some(var),
            vec![Operand::StorageClass(StorageClass::Function)],
        ));
        phi_tag.insert(pid, var);
    }

    let state_var = tc.fresh();
    spill_vars.push(Instruction::new(
        Op::Variable,
        Some(ptr_i32),
        Some(state_var),
        vec![Operand::StorageClass(StorageClass::Function)],
    ));

    let case_const: Vec<Word> = (0..function.blocks.len())
        .map(|i| tc.int_const(i32_ty, i as u64))
        .collect();

    let new_entry = tc.fresh();
    let loop_header = tc.fresh();
    let dispatch = tc.fresh();
    let switch_default_break = tc.fresh();
    let sel_merge = tc.fresh();
    let loop_continue = tc.fresh();
    let loop_merge = tc.fresh();
    let group_count = function.blocks.len().div_ceil(max_blocks);
    let group_dispatches = (0..group_count).map(|_| tc.fresh()).collect::<Vec<_>>();
    let group_merges = (0..group_count).map(|_| tc.fresh()).collect::<Vec<_>>();

    let park_states: Vec<usize> = function
        .blocks
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, block)| block_is_convergent(block, convergent))
        .map(|(index, _)| index)
        .collect();
    let park_consts: Vec<Word> = park_states.iter().map(|&index| case_const[index]).collect();
    let park = (!park_states.is_empty()).then(|| Park {
        outer_header: tc.fresh(),
        outer_continue: tc.fresh(),
        outer_merge: tc.fresh(),
        gate: tc.fresh(),
        armed: tc.fresh(),
    });
    if crate::env_vars::reloop_why() && park.is_some() {
        eprintln!(
            "RELOOP-PARK barrier-states={} blocks={}",
            park_states.len(),
            function.blocks.len()
        );
    }

    let phi_ids: HashSet<Word> = phis
        .keys()
        .copied()
        .filter(|p| !remat_phi.contains_key(p))
        .collect();

    let demo = Demo {
        spill: &spill,
        demote: &demote,
        def_block: &def_block,
        value_type: &value_type,
        phi: &phi_ids,
        remat: &remat,
        remat_phi: &remat_phi,
        phi_tag: &phi_tag,
        remat_phi_invariant: &remat_phi_invariant,
    };

    let mut case_blocks: Vec<(usize, Block)> = Vec::with_capacity(function.blocks.len());
    let mut entry_processed: Vec<Instruction> = Vec::new();

    for (bi, block) in function.blocks.iter().enumerate() {
        let this_label = labels[bi];
        let exit_target = if bi == 0 {
            park.as_ref().map_or(loop_header, |park| park.outer_header)
        } else if group_count == 1 {
            sel_merge
        } else {
            group_merges[bi / max_blocks]
        };
        let mut prelude: Vec<Instruction> = Vec::new();
        let mut local_load: HashMap<Word, Word> = HashMap::default();
        let mut local_remat: HashMap<Word, Word> = HashMap::default();

        let mut body: Vec<Instruction> = Vec::new();
        let n = block.instructions.len();
        for (ii, inst) in block.instructions.iter().enumerate() {
            if matches!(
                inst.class.opcode,
                Op::Phi | Op::Variable | Op::LoopMerge | Op::SelectionMerge
            ) || ii == n - 1
            {
                continue;
            }
            let mut inst = inst.clone();
            for op in &mut inst.operands {
                if let Operand::IdRef(id) = op {
                    if remat_phi.contains_key(id)
                        || (remat.contains_key(id) && def_block.get(id) != Some(&bi))
                    {
                        let r = rematerialize(
                            tc,
                            &mut prelude,
                            &mut local_remat,
                            &mut local_load,
                            &demo,
                            bi,
                            *id,
                        );
                        *op = Operand::IdRef(r);
                    } else if demote.contains(id)
                        && (phi_ids.contains(id) || def_block.get(id) != Some(&bi))
                    {
                        let l = load_demoted(tc, &mut prelude, &mut local_load, &spill, *id);
                        *op = Operand::IdRef(l);
                    }
                }
            }
            if matches!(
                inst.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::InBoundsPtrAccessChain
            ) {
                let base = inst.operands.first().and_then(|operand| match operand {
                    Operand::IdRef(id) => Some(*id),
                    _ => None,
                });
                let base_shape = base
                    .and_then(|id| tc.value_type(id).or_else(|| value_type.get(&id).copied()))
                    .and_then(|ty| tc.pointer_shape(ty));
                let result_pointee = inst
                    .result_type
                    .and_then(|ty| tc.pointer_shape(ty))
                    .map(|(_, pointee)| pointee);
                if let (Some((storage, base_pointee)), Some(pointee)) = (base_shape, result_pointee)
                {
                    let path_reaches_pointee =
                        tc.index_path_selection(&inst.operands[1..], base_pointee) == Some(pointee);
                    if storage == StorageClass::PhysicalStorageBuffer
                        && base_pointee != pointee
                        && !path_reaches_pointee
                    {
                        let address_type = tc.int_ty(64, 0);
                        let address = tc.fresh();
                        body.push(Instruction::new(
                            Op::ConvertPtrToU,
                            Some(address_type),
                            Some(address),
                            vec![Operand::IdRef(base.expect("access chain has a base"))],
                        ));
                        let pointer_type = tc.ptr(storage, pointee);
                        if tc.int_widths.get(&pointee) == Some(&8) {
                            tc.ensure_array_stride(pointer_type, 1);
                        }
                        let reinterpreted = tc.fresh();
                        body.push(Instruction::new(
                            Op::ConvertUToPtr,
                            Some(pointer_type),
                            Some(reinterpreted),
                            vec![Operand::IdRef(address)],
                        ));
                        inst.operands[0] = Operand::IdRef(reinterpreted);
                        tc.value_types.insert(reinterpreted, pointer_type);
                    }
                    inst.result_type = Some(tc.ptr(storage, pointee));
                }
            }
            let spill_after = inst.result_id.filter(|r| demote.contains(r));
            body.push(inst);
            if let Some(r) = spill_after {
                store_spill(tc, &spill[&r], r, &mut body);
            }
        }

        let mut tail: Vec<Instruction> = Vec::new();
        match &terms[bi] {
            Term::Branch(t) => {
                store_phi_edges(
                    tc,
                    &mut prelude,
                    &mut tail,
                    &mut local_load,
                    &demo,
                    bi,
                    this_label,
                    *t,
                    &label_index,
                    &block_phis,
                    &phis,
                );
                let next = case_const[label_index[t]];
                tail.push(store_state(state_var, next));
                tail.push(branch(exit_target));
            }
            Term::BranchCond(c, t, f) => {
                store_phi_edges(
                    tc,
                    &mut prelude,
                    &mut tail,
                    &mut local_load,
                    &demo,
                    bi,
                    this_label,
                    *t,
                    &label_index,
                    &block_phis,
                    &phis,
                );
                store_phi_edges(
                    tc,
                    &mut prelude,
                    &mut tail,
                    &mut local_load,
                    &demo,
                    bi,
                    this_label,
                    *f,
                    &label_index,
                    &block_phis,
                    &phis,
                );
                if t == f {
                    let next = case_const[label_index[t]];
                    tail.push(store_state(state_var, next));
                } else {
                    let cond = resolve(tc, &mut prelude, &mut local_load, &demo, bi, *c);
                    let tc_const = case_const[label_index[t]];
                    let fc_const = case_const[label_index[f]];
                    let sel = tc.fresh();
                    tail.push(Instruction::new(
                        Op::Select,
                        Some(i32_ty),
                        Some(sel),
                        vec![
                            Operand::IdRef(cond),
                            Operand::IdRef(tc_const),
                            Operand::IdRef(fc_const),
                        ],
                    ));
                    tail.push(store_state(state_var, sel));
                }
                tail.push(branch(exit_target));
            }
            Term::Switch(selv, def, cases) => {
                store_phi_edges(
                    tc,
                    &mut prelude,
                    &mut tail,
                    &mut local_load,
                    &demo,
                    bi,
                    this_label,
                    *def,
                    &label_index,
                    &block_phis,
                    &phis,
                );
                for (_, lbl) in cases {
                    store_phi_edges(
                        tc,
                        &mut prelude,
                        &mut tail,
                        &mut local_load,
                        &demo,
                        bi,
                        this_label,
                        *lbl,
                        &label_index,
                        &block_phis,
                        &phis,
                    );
                }
                let selector = resolve(tc, &mut prelude, &mut local_load, &demo, bi, *selv);
                let sel_ty = value_type
                    .get(selv)
                    .copied()
                    .or_else(|| tc.value_type(*selv))
                    .unwrap_or(i32_ty);
                let mut cur = case_const[label_index[def]];
                for (lit, lbl) in cases {
                    let lit_const = tc.int_const(sel_ty, *lit);
                    let eq = tc.fresh();
                    tail.push(Instruction::new(
                        Op::IEqual,
                        Some(bool_ty),
                        Some(eq),
                        vec![Operand::IdRef(selector), Operand::IdRef(lit_const)],
                    ));
                    let picked = case_const[label_index[lbl]];
                    let nxt = tc.fresh();
                    tail.push(Instruction::new(
                        Op::Select,
                        Some(i32_ty),
                        Some(nxt),
                        vec![
                            Operand::IdRef(eq),
                            Operand::IdRef(picked),
                            Operand::IdRef(cur),
                        ],
                    ));
                    cur = nxt;
                }
                tail.push(store_state(state_var, cur));
                tail.push(branch(exit_target));
            }
            Term::Return => tail.push(Instruction::new(Op::Return, None, None, vec![])),
            Term::ReturnValue(v) => {
                let rv = resolve(tc, &mut prelude, &mut local_load, &demo, bi, *v);
                tail.push(Instruction::new(
                    Op::ReturnValue,
                    None,
                    None,
                    vec![Operand::IdRef(rv)],
                ));
            }
            Term::Unreachable => tail.push(Instruction::new(Op::Unreachable, None, None, vec![])),
            Term::Kill(inst) => tail.push(inst.clone()),
        }

        let mut instructions = Vec::new();
        instructions.extend(prelude);
        instructions.extend(body);
        instructions.extend(tail);
        if bi == 0 {
            entry_processed = instructions;
        } else {
            case_blocks.push((
                bi,
                Block {
                    label: Some(Instruction::new(Op::Label, None, Some(this_label), vec![])),
                    instructions,
                },
            ));
        }
    }

    let mut entry_insts: Vec<Instruction> = Vec::new();
    entry_insts.extend(variables.iter().cloned());
    entry_insts.extend(spill_vars);
    if let Some(park) = &park {
        let zero = tc.int_const(i32_ty, 0);
        entry_insts.push(Instruction::new(
            Op::Variable,
            Some(ptr_i32),
            Some(park.armed),
            vec![Operand::StorageClass(StorageClass::Function)],
        ));
        entry_insts.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(park.armed), Operand::IdRef(zero)],
        ));
    }
    entry_insts.extend(entry_processed);
    let entry_block = Block {
        label: Some(Instruction::new(Op::Label, None, Some(new_entry), vec![])),
        instructions: entry_insts,
    };

    let header_block = Block {
        label: Some(Instruction::new(Op::Label, None, Some(loop_header), vec![])),
        instructions: vec![
            Instruction::new(
                Op::LoopMerge,
                None,
                None,
                vec![
                    Operand::IdRef(loop_merge),
                    Operand::IdRef(loop_continue),
                    Operand::LoopControl(spirv::LoopControl::NONE),
                ],
            ),
            Instruction::new(
                Op::Branch,
                None,
                None,
                vec![Operand::IdRef(
                    park.as_ref().map_or(dispatch, |park| park.gate),
                )],
            ),
        ],
    };

    let state_load = tc.fresh();
    let mut dispatch_instructions = vec![Instruction::new(
        Op::Load,
        Some(i32_ty),
        Some(state_load),
        vec![Operand::IdRef(state_var)],
    )];
    let switch_selector;
    let mut switch_ops = vec![Operand::IdRef(0), Operand::IdRef(switch_default_break)];
    if group_count == 1 {
        switch_selector = state_load;
        for (i, lbl) in labels.iter().enumerate().skip(1) {
            switch_ops.push(Operand::LiteralBit32(i as u32));
            switch_ops.push(Operand::IdRef(*lbl));
        }
    } else {
        let divisor = tc.int_const(i32_ty, max_blocks as u64);
        switch_selector = tc.fresh();
        dispatch_instructions.push(Instruction::new(
            Op::UDiv,
            Some(i32_ty),
            Some(switch_selector),
            vec![Operand::IdRef(state_load), Operand::IdRef(divisor)],
        ));
        for (group, label) in group_dispatches.iter().enumerate() {
            switch_ops.push(Operand::LiteralBit32(group as u32));
            switch_ops.push(Operand::IdRef(*label));
        }
    }
    switch_ops[0] = Operand::IdRef(switch_selector);
    dispatch_instructions.extend([
        Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(sel_merge),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        ),
        Instruction::new(Op::Switch, None, None, switch_ops),
    ]);
    if let Some(park) = &park {
        let zero = tc.int_const(i32_ty, 0);
        dispatch_instructions.insert(
            0,
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(park.armed), Operand::IdRef(zero)],
            ),
        );
    }
    let dispatch_block = Block {
        label: Some(Instruction::new(Op::Label, None, Some(dispatch), vec![])),
        instructions: dispatch_instructions,
    };
    let gate_block = park.as_ref().map(|park| {
        let mut instructions = Vec::new();
        let parked = load_parked(tc, state_var, &park_consts, &mut instructions);
        let armed = tc.fresh();
        instructions.push(Instruction::new(
            Op::Load,
            Some(i32_ty),
            Some(armed),
            vec![Operand::IdRef(park.armed)],
        ));
        let zero = tc.int_const(i32_ty, 0);
        let unarmed = tc.fresh();
        instructions.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(unarmed),
            vec![Operand::IdRef(armed), Operand::IdRef(zero)],
        ));
        let wait = tc.fresh();
        instructions.push(Instruction::new(
            Op::LogicalAnd,
            Some(bool_ty),
            Some(wait),
            vec![Operand::IdRef(parked), Operand::IdRef(unarmed)],
        ));
        instructions.push(Instruction::new(
            Op::BranchConditional,
            None,
            None,
            vec![
                Operand::IdRef(wait),
                Operand::IdRef(loop_merge),
                Operand::IdRef(dispatch),
            ],
        ));
        Block {
            label: Some(Instruction::new(Op::Label, None, Some(park.gate), vec![])),
            instructions,
        }
    });
    let switch_default_block = Block {
        label: Some(Instruction::new(
            Op::Label,
            None,
            Some(switch_default_break),
            vec![],
        )),
        instructions: vec![Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(loop_merge)],
        )],
    };

    let sel_merge_block = Block {
        label: Some(Instruction::new(Op::Label, None, Some(sel_merge), vec![])),
        instructions: vec![Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(loop_continue)],
        )],
    };
    let continue_block = Block {
        label: Some(Instruction::new(
            Op::Label,
            None,
            Some(loop_continue),
            vec![],
        )),
        instructions: vec![Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(loop_header)],
        )],
    };
    let merge_terminal = if function
        .def
        .as_ref()
        .and_then(|def| def.result_type)
        .is_some_and(|ty| tc.type_opcode(ty) == Some(Op::TypeVoid))
    {
        Instruction::new(Op::Return, None, None, vec![])
    } else {
        Instruction::new(Op::Unreachable, None, None, vec![])
    };
    let mut park_blocks = Vec::new();
    let merge_block = match &park {
        None => Block {
            label: Some(Instruction::new(Op::Label, None, Some(loop_merge), vec![])),
            instructions: vec![merge_terminal],
        },
        Some(park) => {
            let one = tc.int_const(i32_ty, 1);
            let mut instructions = vec![Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(park.armed), Operand::IdRef(one)],
            )];
            let parked = load_parked(tc, state_var, &park_consts, &mut instructions);
            instructions.push(Instruction::new(
                Op::BranchConditional,
                None,
                None,
                vec![
                    Operand::IdRef(parked),
                    Operand::IdRef(park.outer_continue),
                    Operand::IdRef(park.outer_merge),
                ],
            ));
            park_blocks.push(Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(park.outer_continue),
                    vec![],
                )),
                instructions: vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(park.outer_header)],
                )],
            });
            park_blocks.push(Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(park.outer_merge),
                    vec![],
                )),
                instructions: vec![merge_terminal],
            });
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(loop_merge), vec![])),
                instructions,
            }
        }
    };

    let mut new_blocks = Vec::with_capacity(case_blocks.len() + 7 + group_count * 2);
    new_blocks.push(entry_block);
    if let Some(park) = &park {
        new_blocks.push(Block {
            label: Some(Instruction::new(
                Op::Label,
                None,
                Some(park.outer_header),
                vec![],
            )),
            instructions: vec![
                Instruction::new(
                    Op::LoopMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(park.outer_merge),
                        Operand::IdRef(park.outer_continue),
                        Operand::LoopControl(spirv::LoopControl::NONE),
                    ],
                ),
                Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(loop_header)]),
            ],
        });
    }
    new_blocks.push(header_block);
    new_blocks.extend(gate_block);
    new_blocks.push(dispatch_block);
    new_blocks.push(switch_default_block);
    if group_count == 1 {
        new_blocks.extend(case_blocks.into_iter().map(|(_, block)| block));
    } else {
        let mut cases = case_blocks.into_iter().peekable();
        for group in 0..group_count {
            let group_start = group * max_blocks;
            let group_end = (group_start + max_blocks).min(function.blocks.len());
            let mut inner_ops = vec![
                Operand::IdRef(state_load),
                Operand::IdRef(group_merges[group]),
            ];
            for (index, label) in labels
                .iter()
                .enumerate()
                .take(group_end)
                .skip(group_start.max(1))
            {
                inner_ops.push(Operand::LiteralBit32(index as u32));
                inner_ops.push(Operand::IdRef(*label));
            }
            new_blocks.push(Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(group_dispatches[group]),
                    vec![],
                )),
                instructions: vec![
                    Instruction::new(
                        Op::SelectionMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(group_merges[group]),
                            Operand::SelectionControl(spirv::SelectionControl::NONE),
                        ],
                    ),
                    Instruction::new(Op::Switch, None, None, inner_ops),
                ],
            });
            while cases.peek().is_some_and(|(index, _)| *index < group_end) {
                let (_, block) = cases.next().expect("peeked case");
                new_blocks.push(block);
            }
            new_blocks.push(Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(group_merges[group]),
                    vec![],
                )),
                instructions: vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(sel_merge)],
                )],
            });
        }
    }
    new_blocks.push(sel_merge_block);
    new_blocks.push(continue_block);
    new_blocks.push(merge_block);
    new_blocks.extend(park_blocks);
    function.blocks = new_blocks;
    Ok(())
}

struct Park {
    outer_header: Word,
    outer_continue: Word,
    outer_merge: Word,
    gate: Word,
    armed: Word,
}

fn load_parked(
    tc: &mut TypeCtx<'_>,
    state_var: Word,
    park_consts: &[Word],
    instructions: &mut Vec<Instruction>,
) -> Word {
    let i32_ty = tc.i32_ty();
    let bool_ty = tc.bool_ty();
    let state = tc.fresh();
    instructions.push(Instruction::new(
        Op::Load,
        Some(i32_ty),
        Some(state),
        vec![Operand::IdRef(state_var)],
    ));
    let mut parked = None;
    for &constant in park_consts {
        let equal = tc.fresh();
        instructions.push(Instruction::new(
            Op::IEqual,
            Some(bool_ty),
            Some(equal),
            vec![Operand::IdRef(state), Operand::IdRef(constant)],
        ));
        parked = Some(match parked {
            None => equal,
            Some(previous) => {
                let either = tc.fresh();
                instructions.push(Instruction::new(
                    Op::LogicalOr,
                    Some(bool_ty),
                    Some(either),
                    vec![Operand::IdRef(previous), Operand::IdRef(equal)],
                ));
                either
            }
        });
    }
    parked.expect("a parking state machine has at least one barrier state")
}

fn block_is_convergent(block: &Block, convergent: &HashSet<Word>) -> bool {
    block
        .instructions
        .iter()
        .any(|instruction| match instruction.class.opcode {
            Op::ControlBarrier => true,
            Op::FunctionCall => matches!(
                instruction.operands.first(),
                Some(Operand::IdRef(callee)) if convergent.contains(callee)
            ),
            _ => false,
        })
}

fn convergent_functions(module: &Module) -> HashSet<Word> {
    let mut convergent = HashSet::default();
    loop {
        let before = convergent.len();
        for function in &module.functions {
            let Some(id) = function.def.as_ref().and_then(|def| def.result_id) else {
                continue;
            };
            if !convergent.contains(&id)
                && function
                    .blocks
                    .iter()
                    .any(|block| block_is_convergent(block, &convergent))
            {
                convergent.insert(id);
            }
        }
        if convergent.len() == before {
            return convergent;
        }
    }
}

struct Demo<'a> {
    spill: &'a HashMap<Word, Spill>,
    demote: &'a HashSet<Word>,
    def_block: &'a HashMap<Word, usize>,
    value_type: &'a HashMap<Word, Word>,
    phi: &'a HashSet<Word>,
    remat: &'a HashMap<Word, Instruction>,
    remat_phi: &'a HashMap<Word, (Word, Vec<(Word, Word)>)>,
    phi_tag: &'a HashMap<Word, Word>,
    remat_phi_invariant: &'a HashSet<Word>,
}

fn store_state(state_var: Word, value: Word) -> Instruction {
    Instruction::new(
        Op::Store,
        None,
        None,
        vec![Operand::IdRef(state_var), Operand::IdRef(value)],
    )
}

fn branch(target: Word) -> Instruction {
    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(target)])
}

fn load_demoted(
    tc: &mut TypeCtx,
    prelude: &mut Vec<Instruction>,
    local_load: &mut HashMap<Word, Word>,
    spill: &HashMap<Word, Spill>,
    v: Word,
) -> Word {
    if let Some(&l) = local_load.get(&v) {
        return l;
    }
    let l = load_spill(tc, &spill[&v], prelude);
    local_load.insert(v, l);
    l
}

fn resolve(
    tc: &mut TypeCtx,
    prelude: &mut Vec<Instruction>,
    local_load: &mut HashMap<Word, Word>,
    demo: &Demo,
    bi: usize,
    v: Word,
) -> Word {
    if demo.demote.contains(&v) && (demo.phi.contains(&v) || demo.def_block.get(&v) != Some(&bi)) {
        load_demoted(tc, prelude, local_load, demo.spill, v)
    } else {
        v
    }
}

fn rematerialize(
    tc: &mut TypeCtx,
    prelude: &mut Vec<Instruction>,
    local_remat: &mut HashMap<Word, Word>,
    local_load: &mut HashMap<Word, Word>,
    demo: &Demo,
    bi: usize,
    v: Word,
) -> Word {
    if let Some(&r) = local_remat.get(&v) {
        return r;
    }
    if let Some((pty, incoming)) = demo.remat_phi.get(&v) {
        if demo.remat_phi_invariant.contains(&v) {
            let first = incoming.first().expect("phi has >=1 incoming").0;
            let r = if demo.remat.contains_key(&first) || demo.remat_phi.contains_key(&first) {
                rematerialize(tc, prelude, local_remat, local_load, demo, bi, first)
            } else {
                resolve(tc, prelude, local_load, demo, bi, first)
            };
            local_remat.insert(v, r);
            return r;
        }
        let i32_ty = tc.i32_ty();
        let bool_ty = tc.bool_ty();
        let tag_var = demo.phi_tag[&v];
        let tag = tc.fresh();
        prelude.push(Instruction::new(
            Op::Load,
            Some(i32_ty),
            Some(tag),
            vec![Operand::IdRef(tag_var)],
        ));
        let arm = |tc: &mut TypeCtx,
                   prelude: &mut Vec<Instruction>,
                   local_remat: &mut HashMap<Word, Word>,
                   local_load: &mut HashMap<Word, Word>,
                   id: Word|
         -> Word {
            if demo.remat.contains_key(&id) || demo.remat_phi.contains_key(&id) {
                rematerialize(tc, prelude, local_remat, local_load, demo, bi, id)
            } else {
                resolve(tc, prelude, local_load, demo, bi, id)
            }
        };
        let last = incoming.last().expect("phi has at least one incoming").0;
        let mut acc = arm(tc, prelude, local_remat, local_load, last);
        for (i, (val, _)) in incoming.iter().enumerate().rev().skip(1) {
            let armv = arm(tc, prelude, local_remat, local_load, *val);
            let tagc = tc.int_const(i32_ty, i as u64);
            let cmp = tc.fresh();
            prelude.push(Instruction::new(
                Op::IEqual,
                Some(bool_ty),
                Some(cmp),
                vec![Operand::IdRef(tag), Operand::IdRef(tagc)],
            ));
            let sel = tc.fresh();
            prelude.push(Instruction::new(
                Op::Select,
                Some(*pty),
                Some(sel),
                vec![
                    Operand::IdRef(cmp),
                    Operand::IdRef(armv),
                    Operand::IdRef(acc),
                ],
            ));
            acc = sel;
        }
        local_remat.insert(v, acc);
        return acc;
    }
    let inst = demo.remat[&v].clone();
    let mut new_ops = Vec::with_capacity(inst.operands.len());
    for op in inst.operands.iter() {
        match op {
            Operand::IdRef(id) => {
                let r = if demo.remat.contains_key(id) {
                    rematerialize(tc, prelude, local_remat, local_load, demo, bi, *id)
                } else {
                    resolve(tc, prelude, local_load, demo, bi, *id)
                };
                new_ops.push(Operand::IdRef(r));
            }
            _ => new_ops.push(op.clone()),
        }
    }
    let result = tc.fresh();
    let mut result_type = inst.result_type;
    if matches!(
        inst.class.opcode,
        Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
    ) {
        let base = new_ops.first().and_then(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        });
        let base_storage = base
            .and_then(|id| {
                tc.value_type(id)
                    .or_else(|| demo.value_type.get(&id).copied())
            })
            .and_then(|ty| tc.pointer_shape(ty))
            .map(|(storage, _)| storage);
        let result_pointee = result_type
            .and_then(|ty| tc.pointer_shape(ty))
            .map(|(_, pointee)| pointee);
        if let (Some(storage), Some(pointee)) = (base_storage, result_pointee) {
            result_type = Some(tc.ptr(storage, pointee));
        }
    }
    prelude.push(Instruction::new(
        inst.class.opcode,
        result_type,
        Some(result),
        new_ops,
    ));
    if let Some(result_type) = result_type {
        tc.value_types.insert(result, result_type);
    }
    local_remat.insert(v, result);
    result
}

#[allow(clippy::too_many_arguments)]
fn store_phi_edges(
    tc: &mut TypeCtx,
    prelude: &mut Vec<Instruction>,
    tail: &mut Vec<Instruction>,
    local_load: &mut HashMap<Word, Word>,
    demo: &Demo,
    bi: usize,
    this_label: Word,
    target: Word,
    label_index: &HashMap<Word, usize>,
    block_phis: &[Vec<Word>],
    phis: &HashMap<Word, (Word, Vec<(Word, Word)>)>,
) {
    let Some(&ti) = label_index.get(&target) else {
        return;
    };
    for &rid in &block_phis[ti] {
        let (_, incoming) = &phis[&rid];
        if let Some(&tag_var) = demo.phi_tag.get(&rid) {
            for (idx, (_, pred)) in incoming.iter().enumerate() {
                if *pred == this_label {
                    let i32_ty = tc.i32_ty();
                    let tagc = tc.int_const(i32_ty, idx as u64);
                    tail.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(tag_var), Operand::IdRef(tagc)],
                    ));
                }
            }
            continue;
        }
        for (val, pred) in incoming {
            if *pred == this_label {
                let rv = resolve(tc, prelude, local_load, demo, bi, *val);
                store_spill(tc, &demo.spill[&rid], rv, tail);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "metal2vulkan_relooper_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn assemble(spvasm: &str) -> Option<Vec<u8>> {
        if std::process::Command::new("spirv-as")
            .arg("--version")
            .output()
            .is_err()
        {
            return None;
        }
        let dir = scratch();
        let src = dir.join("in.spvasm");
        let out = dir.join("in.spv");
        std::fs::write(&src, spvasm).unwrap();
        let st = std::process::Command::new("spirv-as")
            .args(["--target-env", crate::tools::VULKAN_TARGET_ENV])
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "spirv-as: {}",
            String::from_utf8_lossy(&st.stderr)
        );
        Some(std::fs::read(&out).unwrap())
    }

    fn validates(spv: &[u8]) -> bool {
        let dir = scratch();
        let p = dir.join("m.spv");
        std::fs::write(&p, spv).unwrap();
        let st = std::process::Command::new("spirv-val")
            .args(["--target-env", crate::tools::VULKAN_TARGET_ENV])
            .arg(&p)
            .output()
            .unwrap();
        if !st.status.success() {
            eprintln!("spirv-val: {}", String::from_utf8_lossy(&st.stderr));
        }
        st.status.success()
    }

    fn relooper_bytes(spv: &[u8]) -> Vec<u8> {
        let mut module = crate::spirv_module::load_bytes(spv).expect("load");
        assert!(
            rewrite_to_relooper(&mut module, MAX_RELOOPER_BLOCKS),
            "expected a rewrite"
        );
        module
            .assemble()
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect()
    }

    fn module_with_block_count(blocks: usize) -> Module {
        let mut function = Function::new();
        function.def = Some(Instruction::new(Op::Function, Some(2), Some(1), vec![]));
        function.blocks = (0..blocks)
            .map(|index| Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(index as Word + 16),
                    vec![],
                )),
                instructions: Vec::new(),
            })
            .collect();
        let mut module = Module::new();
        module.functions.push(function);
        module
    }

    #[test]
    fn a_function_over_the_state_machine_ceiling_declines_by_name() {
        let ceiling = relooper_block_ceiling(MAX_RELOOPER_BLOCKS);
        assert_eq!(ceiling, MAX_RELOOPER_BLOCKS * MAX_RELOOPER_GROUPS);

        let mut module = module_with_block_count(ceiling + 1);
        let (rewrote, declines) = rewrite_selected_to_relooper(
            &mut module,
            MAX_RELOOPER_BLOCKS,
            &crate::fxhash::set_from([1]),
        );
        assert!(!rewrote);
        assert_eq!(declines.len(), 1);
        assert_eq!(declines[0].reason, TOO_MANY_BLOCKS);
        assert_eq!(declines[0].blocks, ceiling + 1);
        assert_eq!(declines[0].function, Some(1));

        let mut module = module_with_block_count(ceiling);
        let (_, declines) = rewrite_selected_to_relooper(
            &mut module,
            MAX_RELOOPER_BLOCKS,
            &crate::fxhash::set_from([1]),
        );
        assert_eq!(declines.len(), 1);
        assert_ne!(declines[0].reason, TOO_MANY_BLOCKS);
    }

    #[test]
    fn whole_function_relooper_clamps_requested_cap_to_driver_safe_limit() {
        assert_eq!(
            effective_relooper_block_cap(8192),
            MAX_DRIVER_SAFE_RELOOPER_BLOCKS
        );
        assert_eq!(
            effective_relooper_block_cap(MAX_RELOOPER_BLOCKS),
            MAX_RELOOPER_BLOCKS
        );
    }

    #[test]
    fn selected_relooper_leaves_unselected_function_cfg_unchanged() {
        let Some(spv) = assemble(
            r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main"
OpExecutionMode %main OriginUpperLeft
%void = OpTypeVoid
%fn = OpTypeFunction %void
%helper = OpFunction %void None %fn
%h0 = OpLabel
OpBranch %h1
%h1 = OpLabel
OpReturn
OpFunctionEnd
%main = OpFunction %void None %fn
%m0 = OpLabel
OpBranch %m1
%m1 = OpLabel
OpReturn
OpFunctionEnd
"#,
        ) else {
            return;
        };
        let mut module = crate::spirv_module::load_bytes(&spv).expect("load");
        let helper_id = module.functions[0]
            .def
            .as_ref()
            .and_then(|def| def.result_id)
            .expect("helper id");
        let (rewrote, declines) = rewrite_selected_to_relooper(
            &mut module,
            MAX_RELOOPER_BLOCKS,
            &crate::fxhash::set_from([helper_id]),
        );
        assert!(rewrote);
        assert!(declines.is_empty());
        assert_ne!(module.functions[0].blocks.len(), 2);
        assert_eq!(module.functions[1].blocks.len(), 2);
    }

    #[test]
    fn whole_function_relooper_partitions_1025_block_state_machine() {
        let mut spvasm = String::from(
            r#"OpCapability Shader
OpMemoryModel Logical GLSL450
OpEntryPoint Fragment %main "main"
OpExecutionMode %main OriginUpperLeft
%void = OpTypeVoid
%fn = OpTypeFunction %void
%main = OpFunction %void None %fn
"#,
        );
        for index in 0..MAX_DRIVER_SAFE_RELOOPER_BLOCKS + 1 {
            spvasm.push_str(&format!("%b{index} = OpLabel\n"));
            if index == MAX_DRIVER_SAFE_RELOOPER_BLOCKS {
                spvasm.push_str("OpReturn\n");
            } else {
                spvasm.push_str(&format!("OpBranch %b{}\n", index + 1));
            }
        }
        spvasm.push_str("OpFunctionEnd\n");

        let Some(spv) = assemble(&spvasm) else {
            return;
        };
        let mut module = crate::spirv_module::load_bytes(&spv).expect("load");
        assert!(rewrite_to_relooper(&mut module, 8192));
        let switch_case_counts = module.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| instruction.class.opcode == Op::Switch)
            .map(|instruction| instruction.operands.len().saturating_sub(2) / 2)
            .collect::<Vec<_>>();
        assert_eq!(switch_case_counts.len(), 3, "{switch_case_counts:?}");
        assert!(
            switch_case_counts
                .iter()
                .all(|count| *count <= MAX_DRIVER_SAFE_RELOOPER_BLOCKS),
            "{switch_case_counts:?}"
        );
        let bytes = module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        assert!(validates(&bytes));
    }

    fn block_id(block: &Block) -> Word {
        block
            .label
            .as_ref()
            .and_then(|label| label.result_id)
            .expect("block label id")
    }

    fn id_ref(operand: &Operand) -> Word {
        let Operand::IdRef(id) = operand else {
            panic!("expected IdRef operand, got {operand:?}");
        };
        *id
    }

    fn single_loop_dispatch_default(spv: &[u8]) -> (Word, Word, Word, Op) {
        let module = crate::spirv_module::load_bytes(spv).expect("load relooped module");
        let function = module
            .functions
            .iter()
            .find(|function| {
                function.blocks.iter().any(|block| {
                    block
                        .instructions
                        .iter()
                        .any(|instruction| instruction.class.opcode == Op::LoopMerge)
                })
            })
            .expect("function with a relooped dispatcher");
        let loop_header = function
            .blocks
            .iter()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.class.opcode == Op::LoopMerge)
            })
            .expect("loop header");
        let loop_merge = loop_header
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::LoopMerge)
            .and_then(|instruction| instruction.operands.first())
            .map(id_ref)
            .expect("loop merge target");
        let dispatch = loop_header
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::Branch)
            .and_then(|instruction| instruction.operands.first())
            .map(id_ref)
            .expect("dispatch branch");
        let dispatch_block = function
            .blocks
            .iter()
            .find(|block| block_id(block) == dispatch)
            .expect("dispatch block");
        let switch_default = dispatch_block
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::Switch)
            .and_then(|instruction| instruction.operands.get(1))
            .map(id_ref)
            .expect("switch default target");
        let default_block = function
            .blocks
            .iter()
            .find(|block| block_id(block) == switch_default)
            .expect("switch default block");
        let default_branch = default_block
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::Branch)
            .and_then(|instruction| instruction.operands.first())
            .map(id_ref)
            .expect("default block branch");
        let merge_terminal = function
            .blocks
            .iter()
            .find(|block| block_id(block) == loop_merge)
            .and_then(|block| block.instructions.last())
            .map(|instruction| instruction.class.opcode)
            .expect("loop merge terminal");
        (loop_merge, switch_default, default_branch, merge_terminal)
    }

    #[test]
    fn relooper_bails_instead_of_panicking_on_missing_target() {
        let mut module = Module::new();
        let mut function = Function::new();
        function.blocks = vec![
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(1), vec![])),
                instructions: vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(2)],
                )],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(2), vec![])),
                instructions: vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(99)],
                )],
            },
        ];
        module.functions.push(function);

        assert!(
            !rewrite_to_relooper(&mut module, MAX_RELOOPER_BLOCKS),
            "missing branch target should decline, not panic"
        );
    }

    fn barrier_iterations(module: &Module, input: u64) -> Vec<Vec<u32>> {
        let mut values: HashMap<Word, u64> = HashMap::default();
        for instruction in &module.types_global_values {
            let Some(id) = instruction.result_id else {
                continue;
            };
            match (instruction.class.opcode, instruction.operands.first()) {
                (Op::Constant, Some(Operand::LiteralBit32(value))) => {
                    values.insert(id, u64::from(*value));
                }
                (Op::ConstantTrue, _) => {
                    values.insert(id, 1);
                }
                (Op::ConstantFalse, _) => {
                    values.insert(id, 0);
                }
                _ => {}
            }
        }
        let function = &module.functions[0];
        let blocks: HashMap<Word, &Block> = function
            .blocks
            .iter()
            .map(|block| (block_id(block), block))
            .collect();
        let function_variables: HashSet<Word> = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| instruction.class.opcode == Op::Variable)
            .filter_map(|instruction| instruction.result_id)
            .collect();
        let mut memory: HashMap<Word, u64> = HashMap::default();
        let mut loops: Vec<(Word, Word, u32)> = Vec::new();
        let mut barriers = Vec::new();
        let mut current = block_id(&function.blocks[0]);
        for _ in 0..10_000 {
            if let Some(position) = loops.iter().rposition(|(_, merge, _)| *merge == current) {
                loops.truncate(position);
            }
            let mut next = None;
            for instruction in &blocks[&current].instructions {
                let ops = &instruction.operands;
                let value =
                    |values: &HashMap<Word, u64>, index: usize| values[&id_ref(&ops[index])];
                let result = match instruction.class.opcode {
                    Op::LoopMerge => {
                        if loops
                            .last()
                            .is_some_and(|(header, _, _)| *header == current)
                        {
                            loops.last_mut().expect("checked").2 += 1;
                        } else {
                            loops.push((current, id_ref(&ops[0]), 1));
                        }
                        None
                    }
                    Op::Load => {
                        let pointer = id_ref(&ops[0]);
                        Some(if function_variables.contains(&pointer) {
                            memory[&pointer]
                        } else {
                            input
                        })
                    }
                    Op::Store => {
                        memory.insert(id_ref(&ops[0]), value(&values, 1));
                        None
                    }
                    Op::IEqual => Some(u64::from(value(&values, 0) == value(&values, 1))),
                    Op::ULessThan => Some(u64::from(value(&values, 0) < value(&values, 1))),
                    Op::LogicalOr => {
                        Some(u64::from(value(&values, 0) != 0 || value(&values, 1) != 0))
                    }
                    Op::LogicalAnd => {
                        Some(u64::from(value(&values, 0) != 0 && value(&values, 1) != 0))
                    }
                    Op::Select => Some(if value(&values, 0) != 0 {
                        value(&values, 1)
                    } else {
                        value(&values, 2)
                    }),
                    Op::UDiv => Some(value(&values, 0) / value(&values, 1)),
                    Op::ControlBarrier => {
                        barriers.push(loops.iter().map(|(_, _, iteration)| *iteration).collect());
                        None
                    }
                    Op::Branch => {
                        next = Some(id_ref(&ops[0]));
                        None
                    }
                    Op::BranchConditional => {
                        next = Some(id_ref(&ops[if value(&values, 0) != 0 { 1 } else { 2 }]));
                        None
                    }
                    Op::Switch => {
                        let selector = value(&values, 0);
                        let mut target = id_ref(&ops[1]);
                        for pair in ops[2..].chunks(2) {
                            if let [Operand::LiteralBit32(literal), label] = pair {
                                if u64::from(*literal) == selector {
                                    target = id_ref(label);
                                }
                            }
                        }
                        next = Some(target);
                        None
                    }
                    Op::Return | Op::ReturnValue | Op::Unreachable => return barriers,
                    Op::Variable | Op::SelectionMerge => None,
                    other => panic!("barrier_iterations does not model {other:?}"),
                };
                if let (Some(id), Some(result)) = (instruction.result_id, result) {
                    values.insert(id, result);
                }
            }
            current = next.expect("every block ends in a branch or an exit");
        }
        panic!("the invocation did not finish within 10000 blocks");
    }

    const GUARDED_LOAD_THEN_BARRIER: &str = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %lid
                       OpExecutionMode %main LocalSize 64 1 1
                       OpDecorate %lid BuiltIn LocalInvocationIndex
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %bool = OpTypeBool
               %uint = OpTypeInt 32 0
             %uint_2 = OpConstant %uint 2
            %uint_16 = OpConstant %uint 16
           %uint_264 = OpConstant %uint 264
             %ptr_in = OpTypePointer Input %uint
                %lid = OpVariable %ptr_in Input
               %main = OpFunction %void None %fn
              %entry = OpLabel
                  %i = OpLoad %uint %lid
                  %c = OpULessThan %bool %i %uint_16
                       OpSelectionMerge %join None
                       OpBranchConditional %c %load %join
               %load = OpLabel
                       OpBranch %load2
              %load2 = OpLabel
                       OpBranch %join
               %join = OpLabel
                       OpControlBarrier %uint_2 %uint_2 %uint_264
                       OpReturn
                       OpFunctionEnd
    "#;

    #[test]
    fn relooper_barrier_executes_in_one_iteration_on_every_path() {
        let Some(spv) = assemble(GUARDED_LOAD_THEN_BARRIER) else {
            return;
        };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
        let module = crate::spirv_module::load_bytes(&out).expect("load");
        let guarded = barrier_iterations(&module, 0);
        let skipping = barrier_iterations(&module, 40);
        assert_eq!(
            guarded.len(),
            1,
            "one barrier on the guarded path: {guarded:?}"
        );
        assert_eq!(
            guarded, skipping,
            "a lane that took the guarded load ({guarded:?}) and one that skipped it ({skipping:?}) \
             must execute the barrier in the same iteration of every loop around it"
        );
    }

    #[test]
    fn barrier_iterations_sees_the_unparked_state_machine_diverge() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %lid
                       OpExecutionMode %main LocalSize 64 1 1
                       OpDecorate %lid BuiltIn LocalInvocationIndex
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %bool = OpTypeBool
               %uint = OpTypeInt 32 0
             %uint_1 = OpConstant %uint 1
             %uint_2 = OpConstant %uint 2
             %uint_3 = OpConstant %uint 3
            %uint_16 = OpConstant %uint 16
           %uint_264 = OpConstant %uint 264
             %ptr_in = OpTypePointer Input %uint
             %ptr_fn = OpTypePointer Function %uint
                %lid = OpVariable %ptr_in Input
               %main = OpFunction %void None %fn
              %entry = OpLabel
              %state = OpVariable %ptr_fn Function
                  %i = OpLoad %uint %lid
                  %c = OpULessThan %bool %i %uint_16
                 %s0 = OpSelect %uint %c %uint_1 %uint_3
                       OpStore %state %s0
                       OpBranch %head
               %head = OpLabel
                       OpLoopMerge %merge %cont None
                       OpBranch %dispatch
           %dispatch = OpLabel
                 %sv = OpLoad %uint %state
                       OpSelectionMerge %sel None
                       OpSwitch %sv %default 1 %load 2 %load2 3 %join
            %default = OpLabel
                       OpBranch %merge
               %load = OpLabel
                       OpStore %state %uint_2
                       OpBranch %sel
              %load2 = OpLabel
                       OpStore %state %uint_3
                       OpBranch %sel
               %join = OpLabel
                       OpControlBarrier %uint_2 %uint_2 %uint_264
                       OpReturn
                %sel = OpLabel
                       OpBranch %cont
               %cont = OpLabel
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else {
            return;
        };
        assert!(
            validates(&spv),
            "the unparked state machine is valid SPIR-V — that is the trap"
        );
        let module = crate::spirv_module::load_bytes(&spv).expect("load");
        assert_eq!(barrier_iterations(&module, 0), vec![vec![3]]);
        assert_eq!(barrier_iterations(&module, 40), vec![vec![1]]);
    }

    #[test]
    fn relooper_prologue_barrier_keeps_the_single_loop() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %lid
                       OpExecutionMode %main LocalSize 64 1 1
                       OpDecorate %lid BuiltIn LocalInvocationIndex
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %bool = OpTypeBool
               %uint = OpTypeInt 32 0
             %uint_2 = OpConstant %uint 2
            %uint_16 = OpConstant %uint 16
           %uint_264 = OpConstant %uint 264
             %ptr_in = OpTypePointer Input %uint
                %lid = OpVariable %ptr_in Input
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpControlBarrier %uint_2 %uint_2 %uint_264
                  %i = OpLoad %uint %lid
                  %c = OpULessThan %bool %i %uint_16
                       OpSelectionMerge %join None
                       OpBranchConditional %c %load %join
               %load = OpLabel
                       OpBranch %join
               %join = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else {
            return;
        };
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
        let module = crate::spirv_module::load_bytes(&out).expect("load");
        let loops = module.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| instruction.class.opcode == Op::LoopMerge)
            .count();
        assert_eq!(loops, 1, "a prologue barrier must not add the parking loop");
        assert_eq!(barrier_iterations(&module, 0), vec![Vec::<u32>::new()]);
    }

    #[test]
    fn relooper_dispatch_default_breaks_to_loop_merge() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %bool = OpTypeBool
               %true = OpConstantTrue %bool
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpBranch %head
               %head = OpLabel
                       OpLoopMerge %merge %cont None
                       OpBranchConditional %true %body %merge
               %body = OpLabel
                       OpBranch %cont
               %cont = OpLabel
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
        let (loop_merge, switch_default, default_branch, merge_terminal) =
            single_loop_dispatch_default(&out);
        assert_ne!(
            switch_default, loop_merge,
            "SPIR-V requires a dominated switch default block, not a direct merge target"
        );
        assert_eq!(
            default_branch, loop_merge,
            "relooper switch default block must statically break to the loop merge"
        );
        assert_eq!(
            merge_terminal,
            Op::Return,
            "void-function dispatch merge represents normal function exit"
        );
    }

    #[test]
    fn relooper_loop_with_phi_validates() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                %u10 = OpConstant %uint 10
              %ptr_u = OpTypePointer Function %uint
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %acc = OpVariable %ptr_u Function
                       OpStore %acc %u0
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %body
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %body None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
              %inext = OpIAdd %uint %i %u1
                       OpStore %acc %inext
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
    }

    #[test]
    fn relooper_switch_in_loop_validates() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                 %u2 = OpConstant %uint 2
                 %u5 = OpConstant %uint 5
              %ptr_u = OpTypePointer Function %uint
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %acc = OpVariable %ptr_u Function
                       OpStore %acc %u0
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %cont
                %cmp = OpULessThan %bool %i %u5
                       OpLoopMerge %merge %cont None
                       OpBranchConditional %cmp %sw %merge
                 %sw = OpLabel
                %dbl = OpIAdd %uint %i %i
                       OpSelectionMerge %swm None
                       OpSwitch %i %def 0 %c0 1 %c1
                 %c0 = OpLabel
                       OpStore %acc %dbl
                       OpBranch %swm
                 %c1 = OpLabel
                       OpStore %acc %i
                       OpBranch %swm
                %def = OpLabel
                       OpStore %acc %u2
                       OpBranch %swm
                %swm = OpLabel
                       OpBranch %cont
               %cont = OpLabel
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
    }

    #[test]
    fn relooper_preserves_64_bit_switch_literals() {
        let spvasm = r#"
                       OpCapability Shader
                       OpCapability Int64
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
              %ulong = OpTypeInt 64 0
                 %u1 = OpConstant %ulong 1
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpBranch %switch
             %switch = OpLabel
                       OpSelectionMerge %merge None
                       OpSwitch %u1 %default 0 %zero 1 %one
               %zero = OpLabel
                       OpBranch %merge
                %one = OpLabel
                       OpBranch %merge
            %default = OpLabel
                       OpBranch %merge
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
    }

    #[test]
    fn relooper_entry_pointer_used_in_loop_validates() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                %u10 = OpConstant %uint 10
              %ptr_u = OpTypePointer Function %uint
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %acc = OpVariable %ptr_u Function
                  %p = OpAccessChain %ptr_u %acc
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %body
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %body None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
                       OpStore %p %i
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "relooper output must validate");
    }

    #[test]
    fn relooper_rematerializes_nonentry_pointer_access_chain() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                %u10 = OpConstant %uint 10
                %u4 = OpConstant %uint 4
              %ptr_u = OpTypePointer Function %uint
                %arr_t = OpTypeArray %uint %u4
            %ptr_arr = OpTypePointer Function %arr_t
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %arr = OpVariable %ptr_arr Function
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %use
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %use None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
                  %p = OpAccessChain %ptr_u %arr %u0
                       OpBranch %use
                %use = OpLabel
                       OpStore %p %i
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(
            validates(&out),
            "relooper output must validate (rematerialized pointer)"
        );
    }

    #[test]
    fn relooper_rematerializes_nonentry_image_load() {
        let spvasm = r#"
                       OpCapability Shader
                       OpCapability ImageQuery
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %img_var
                       OpExecutionMode %main LocalSize 1 1 1
                       OpDecorate %img_var DescriptorSet 0
                       OpDecorate %img_var Binding 0
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
              %float = OpTypeFloat 32
               %uint = OpTypeInt 32 0
             %v2uint = OpTypeVector %uint 2
                 %u0 = OpConstant %uint 0
              %image = OpTypeImage %float 2D 0 0 0 1 Unknown
            %ptr_img = OpTypePointer UniformConstant %image
             %img_var = OpVariable %ptr_img UniformConstant
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpBranch %body
               %body = OpLabel
                %img = OpLoad %image %img_var
                       OpBranch %use
                %use = OpLabel
               %size = OpImageQuerySizeLod %v2uint %img %u0
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(
            validates(&out),
            "relooper output must validate (rematerialized image load)"
        );
    }

    #[test]
    fn relooper_scalarizes_explicit_layout_aggregate_spills() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %buf
                       OpExecutionMode %main LocalSize 1 1 1
                       OpDecorate %arr ArrayStride 4
                       OpMemberDecorate %block 0 Offset 0
                       OpDecorate %block Block
                       OpDecorate %buf DescriptorSet 0
                       OpDecorate %buf Binding 0
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                 %u3 = OpConstant %uint 3
                %arr = OpTypeArray %uint %u3
              %block = OpTypeStruct %arr
          %ptr_block = OpTypePointer StorageBuffer %block
            %ptr_arr = OpTypePointer StorageBuffer %arr
              %ptr_u = OpTypePointer StorageBuffer %uint
                %buf = OpVariable %ptr_block StorageBuffer
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpBranch %load
               %load = OpLabel
           %array_ptr = OpAccessChain %ptr_arr %buf %u0
             %values = OpLoad %arr %array_ptr
                       OpBranch %use
                %use = OpLabel
              %value = OpCompositeExtract %uint %values 1
              %field = OpAccessChain %ptr_u %buf %u0 %u0
                       OpStore %field %value
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(validates(&out), "scalarized relooper output must validate");
    }

    #[test]
    fn relooper_rematerializes_pointer_select_and_phi() {
        let spvasm = r#"
                       OpCapability Shader
                       OpCapability VariablePointersStorageBuffer
                       OpExtension "SPV_KHR_variable_pointers"
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main" %buf
                       OpExecutionMode %main LocalSize 1 1 1
                       OpDecorate %arr_t ArrayStride 4
                       OpMemberDecorate %buf_t 0 Offset 0
                       OpDecorate %buf_t Block
                       OpDecorate %buf DescriptorSet 0
                       OpDecorate %buf Binding 0
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                 %u2 = OpConstant %uint 2
                 %u4 = OpConstant %uint 4
                %u10 = OpConstant %uint 10
              %arr_t = OpTypeArray %uint %u4
              %buf_t = OpTypeStruct %arr_t
            %ptr_buf = OpTypePointer StorageBuffer %buf_t
                %buf = OpVariable %ptr_buf StorageBuffer
              %ptr_u = OpTypePointer StorageBuffer %uint
               %main = OpFunction %void None %fn
              %entry = OpLabel
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %cont
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %cont None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
                  %c = OpULessThan %bool %i %u2
                 %pa = OpAccessChain %ptr_u %buf %u0 %u0
                 %pb = OpAccessChain %ptr_u %buf %u0 %u1
               %psel = OpSelect %ptr_u %c %pa %pb
                       OpSelectionMerge %join None
                       OpBranchConditional %c %t %f
                 %t = OpLabel
                 %pt = OpAccessChain %ptr_u %buf %u0 %u2
                       OpBranch %join
                 %f = OpLabel
                       OpBranch %join
               %join = OpLabel
                  %p = OpPhi %ptr_u %pt %t %psel %f
                       OpStore %p %i
                       OpBranch %cont
               %cont = OpLabel
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(
            validates(&out),
            "relooper output must validate (rematerialized pointer select + phi)"
        );
    }

    #[test]
    fn relooper_rematerializes_pointer_copy_object() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                %u10 = OpConstant %uint 10
                 %u4 = OpConstant %uint 4
              %ptr_u = OpTypePointer Function %uint
              %arr_t = OpTypeArray %uint %u4
            %ptr_arr = OpTypePointer Function %arr_t
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %arr = OpVariable %ptr_arr Function
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %use
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %use None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
                  %p = OpAccessChain %ptr_u %arr %u0
                 %pc = OpCopyObject %ptr_u %p
                       OpBranch %use
                %use = OpLabel
                       OpStore %pc %i
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        assert!(validates(&spv), "input must validate");
        let out = relooper_bytes(&spv);
        assert!(
            validates(&out),
            "relooper output must validate (rematerialized pointer CopyObject)"
        );
    }

    #[test]
    fn relooper_rematerializes_invariant_function_pointer_phi() {
        let spvasm = r#"
                       OpCapability Shader
                       OpMemoryModel Logical GLSL450
                       OpEntryPoint GLCompute %main "main"
                       OpExecutionMode %main LocalSize 1 1 1
               %void = OpTypeVoid
                 %fn = OpTypeFunction %void
               %uint = OpTypeInt 32 0
               %bool = OpTypeBool
                 %u0 = OpConstant %uint 0
                 %u1 = OpConstant %uint 1
                 %u2 = OpConstant %uint 2
                 %u4 = OpConstant %uint 4
                %u10 = OpConstant %uint 10
              %arr_t = OpTypeArray %uint %u4
            %ptr_arr = OpTypePointer Function %arr_t
              %ptr_u = OpTypePointer Function %uint
               %main = OpFunction %void None %fn
              %entry = OpLabel
                %arr = OpVariable %ptr_arr Function
                       OpBranch %head
               %head = OpLabel
                  %i = OpPhi %uint %u0 %entry %inext %cont
                %cmp = OpULessThan %bool %i %u10
                       OpLoopMerge %merge %cont None
                       OpBranchConditional %cmp %body %merge
               %body = OpLabel
                  %c = OpULessThan %bool %i %u2
                       OpSelectionMerge %join None
                       OpBranchConditional %c %t %f
                 %t = OpLabel
                 %pt = OpAccessChain %ptr_u %arr %u2
                       OpBranch %join
                 %f = OpLabel
                 %pf = OpAccessChain %ptr_u %arr %u2
                       OpBranch %join
               %join = OpLabel
                  %p = OpPhi %ptr_u %pt %t %pf %f
                       OpStore %p %i
                       OpBranch %cont
               %cont = OpLabel
              %inext = OpIAdd %uint %i %u1
                       OpBranch %head
              %merge = OpLabel
                       OpReturn
                       OpFunctionEnd
        "#;
        let Some(spv) = assemble(spvasm) else { return };
        let out = relooper_bytes(&spv);
        assert!(
            validates(&out),
            "relooper output must validate (invariant Function pointer phi rematerialized without a select)"
        );
    }
}
