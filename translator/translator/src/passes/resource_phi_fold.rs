// Apple's GL compiles a GLSL function that takes a sampler2D (Minecraft's `sample_lightmap(sampler2D, ivec2)`) by
// passing every texture/sampler pair the shader binds plus the index of the one meant, and choosing with a switch:
//
//     store i32 0, ptr %which                        ; the caller: Sampler2 is pair 0
//     %i = load i32, ptr %which                      ; the inlined helper
//     switch i32 %i, label %merge [ i32 1, label %case ]
//     merge: %tex = phi ptr addrspace(1) [ %t0, %block ], [ %t1, %case ]
//            %smp = phi ptr addrspace(2) [ %s0, %block ], [ %s1, %case ]
//
// The emitter turns each candidate into a loaded image/sampler and the phis keep LLVM's pointer type, so the sample
// lowering finds a sampler operand it cannot recover ("sampler operand is a pointer whose exact sampler state was not
// recovered") and the vertex shader fails: Apple's GL then runs it on the CPU. The selector is a constant, stored
// once into a function variable; this pass evaluates the branch that feeds such a phi and replaces the phi by the
// value on the edge that is taken. Phis of other types, and branches whose condition is not a constant, are left
// alone.
use crate::spirv_module::{Instruction, Operand};
use spirv::{Op, StorageClass, Word};
use std::collections::{HashMap, HashSet};

use super::Ctx;

pub(super) fn fold_constant_resource_phis(ctx: &mut Ctx, entry_idx: usize) -> usize {
    // Most shaders have no resource phi. Do not build another CFG and value map for their ordinary draw path.
    if !ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .any(|b| b.instructions.iter().any(|i| i.class.opcode == Op::Phi))
    {
        return 0;
    }
    let globals: Vec<&Instruction> = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .collect();
    let opaque_types: HashSet<Word> = globals
        .iter()
        .filter(|i| {
            matches!(
                i.class.opcode,
                Op::TypeImage | Op::TypeSampler | Op::TypeSampledImage
            )
        })
        .filter_map(|i| i.result_id)
        .collect();
    if opaque_types.is_empty() {
        return 0;
    }
    let function = &ctx.module.functions[entry_idx];
    let defs: HashMap<Word, &Instruction> = globals
        .iter()
        .copied()
        .chain(function.blocks.iter().flat_map(|b| b.instructions.iter()))
        .filter_map(|i| Some((i.result_id?, i)))
        .collect();
    let flow = Flow::new(function);
    let eval = Eval::new(function, &defs, &flow);
    let label = |b: &crate::spirv_module::Block| b.label.as_ref().and_then(|l| l.result_id);
    let terminators: HashMap<Word, &Instruction> = function
        .blocks
        .iter()
        .filter_map(|b| Some((label(b)?, b.instructions.last()?)))
        .collect();
    let mut preds: HashMap<Word, Vec<Word>> = HashMap::new();
    for (&block, term) in &terminators {
        for target in branch_targets(term) {
            preds.entry(target).or_default().push(block);
        }
    }

    let mut replace: HashMap<Word, Word> = HashMap::new();
    for (block_index, block) in function.blocks.iter().enumerate() {
        let Some(merge) = label(block) else { continue };
        for (phi_index, phi) in block
            .instructions
            .iter()
            .enumerate()
            .take_while(|(_, i)| i.class.opcode == Op::Phi)
        {
            let (Some(result), Some(_)) = (phi.result_id, phi.result_type) else {
                continue;
            };
            let incoming: Vec<(Word, Word)> = phi
                .operands
                .chunks(2)
                .filter_map(|pair| match pair {
                    [Operand::IdRef(v), Operand::IdRef(b)] => Some((*v, *b)),
                    _ => None,
                })
                .collect();
            if incoming.len() < 2
                || !incoming.iter().all(|(v, _)| {
                    defs.get(v)
                        .and_then(|d| d.result_type)
                        .is_some_and(|t| opaque_types.contains(&t))
                })
            {
                continue;
            }
            // every edge leaves the same header, directly or through a block that only branches on to the merge
            let header_of = |pred: Word| -> Option<Word> {
                let term = terminators.get(&pred)?;
                if term.class.opcode == Op::Branch {
                    match preds.get(&pred).map(Vec::as_slice) {
                        Some([only]) => Some(*only),
                        _ => None,
                    }
                } else {
                    Some(pred)
                }
            };
            let headers: HashSet<Option<Word>> =
                incoming.iter().map(|(_, p)| header_of(*p)).collect();
            let [Some(header)] = headers.into_iter().collect::<Vec<_>>()[..] else {
                continue;
            };
            let Some(taken) = terminators.get(&header).and_then(|t| eval.taken_target(t)) else {
                continue;
            };
            let chosen = incoming
                .iter()
                .find(|(_, pred)| {
                    if taken == merge {
                        *pred == header
                    } else {
                        *pred == taken
                    }
                })
                .map(|(v, _)| *v);
            if let Some(value) = chosen {
                // The branch is not removed. A value defined only in the chosen arm still does not dominate the
                // merge in SPIR-V's CFG, even when that arm's condition is constant. Keep that phi intact.
                if flow.value_dominates(value, (block_index, phi_index)) {
                    replace.insert(result, value);
                }
            }
        }
    }
    if replace.is_empty() {
        return 0;
    }
    let function = &mut ctx.module.functions[entry_idx];
    for block in &mut function.blocks {
        block
            .instructions
            .retain(|i| !i.result_id.is_some_and(|r| replace.contains_key(&r)));
        for inst in &mut block.instructions {
            for operand in &mut inst.operands {
                if let Operand::IdRef(id) = operand {
                    let mut to = *id;
                    while let Some(next) = replace.get(&to) {
                        to = *next;
                    }
                    *id = to;
                }
            }
        }
    }
    replace.len()
}

// Instruction order matters for a store and load in one block; across blocks use the shared dominator analysis.
struct Flow {
    positions: HashMap<Word, (usize, usize)>,
    dominance: Vec<Option<(usize, usize)>>,
}

impl Flow {
    fn new(function: &crate::spirv_module::Function) -> Self {
        let labels: HashMap<Word, usize> = function
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(b, block)| Some((block.label.as_ref()?.result_id?, b)))
            .collect();
        let successors: Vec<Vec<usize>> = function
            .blocks
            .iter()
            .map(|block| {
                block
                    .instructions
                    .last()
                    .map(branch_targets)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|label| labels.get(&label).copied())
                    .collect()
            })
            .collect();
        let predecessors = crate::dominators::build_predecessors(&successors);
        let (_, dominance, _) = crate::dominators::dominance(&successors, &predecessors);
        let positions = function
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(b, block)| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter_map(move |(i, inst)| Some((inst.result_id?, (b, i))))
            })
            .collect();
        Self {
            positions,
            dominance,
        }
    }

    fn dominates(&self, from: (usize, usize), to: (usize, usize)) -> bool {
        if from.0 == to.0 {
            from.1 < to.1
        } else {
            crate::dominators::dominates_interval(&self.dominance, from.0, to.0)
        }
    }

    fn value_dominates(&self, value: Word, at: (usize, usize)) -> bool {
        self.positions
            .get(&value)
            .is_none_or(|&position| self.dominates(position, at))
    }
}

fn branch_targets(term: &Instruction) -> Vec<Word> {
    let ids = |range: &[Operand]| {
        range
            .iter()
            .filter_map(|o| match o {
                Operand::IdRef(id) => Some(*id),
                _ => None,
            })
            .collect()
    };
    match term.class.opcode {
        Op::Branch => ids(&term.operands),
        Op::BranchConditional => ids(term.operands.get(1..3).unwrap_or(&[])),
        Op::Switch => ids(term.operands.get(1..).unwrap_or(&[])),
        _ => vec![],
    }
}

// Integer and boolean values known at translation time: constants, and loads of a function variable that holds one
// stored constant and is otherwise only loaded (an alloca whose address never escapes).
struct Eval<'a> {
    defs: &'a HashMap<Word, &'a Instruction>,
    stored: HashMap<Word, Word>,
}

impl<'a> Eval<'a> {
    fn new(
        function: &'a crate::spirv_module::Function,
        defs: &'a HashMap<Word, &'a Instruction>,
        flow: &Flow,
    ) -> Self {
        let mut stores: HashMap<Word, Vec<(Word, (usize, usize))>> = HashMap::new();
        let mut loads: HashMap<Word, Vec<(usize, usize)>> = HashMap::new();
        let mut escaped: HashSet<Word> = HashSet::new();
        for (block, body) in function.blocks.iter().enumerate() {
            for (index, inst) in body.instructions.iter().enumerate() {
                match (inst.class.opcode, inst.operands.as_slice()) {
                    (Op::Store, [Operand::IdRef(ptr), Operand::IdRef(value), ..]) => {
                        stores
                            .entry(*ptr)
                            .or_default()
                            .push((*value, (block, index)));
                        escaped.insert(*value);
                    }
                    (Op::Load, [Operand::IdRef(ptr), ..]) => {
                        loads.entry(*ptr).or_default().push((block, index));
                    }
                    _ => escaped.extend(inst.operands.iter().filter_map(|o| match o {
                        Operand::IdRef(id) => Some(*id),
                        _ => None,
                    })),
                }
            }
        }
        let is_function_var = |ptr: &Word| {
            defs.get(ptr).is_some_and(|d| {
                d.class.opcode == Op::Variable
                    && d.operands == [Operand::StorageClass(StorageClass::Function)]
            })
        };
        let stored = stores
            .into_iter()
            .filter(|(ptr, values)| {
                if values.len() != 1 || !is_function_var(ptr) || escaped.contains(ptr) {
                    return false;
                }
                let (value, position) = values[0];
                // A single store is insufficient if it runs after a load or on only one path to it. Restrict its
                // source to literal constants as well, so cyclic stores/loads cannot recurse during evaluation.
                defs.get(&value).is_some_and(|d| {
                    matches!(
                        d.class.opcode,
                        Op::Constant | Op::ConstantNull | Op::ConstantTrue | Op::ConstantFalse
                    )
                }) && loads
                    .get(ptr)
                    .is_some_and(|uses| uses.iter().all(|&at| flow.dominates(position, at)))
            })
            .map(|(ptr, values)| (ptr, values[0].0))
            .collect();
        Eval { defs, stored }
    }

    fn int(&self, id: Word) -> Option<u64> {
        let d = self.defs.get(&id)?;
        match (d.class.opcode, d.operands.as_slice()) {
            (Op::Constant, [Operand::LiteralBit32(v)]) => Some(u64::from(*v)),
            (Op::Constant, [Operand::LiteralBit64(v)]) => Some(*v),
            (Op::ConstantNull, []) => Some(0),
            (Op::Load, [Operand::IdRef(ptr), ..]) => self.int(*self.stored.get(ptr)?),
            _ => None,
        }
    }

    fn bool(&self, id: Word) -> Option<bool> {
        self.bool_depth(id, 0)
    }

    fn bool_depth(&self, id: Word, depth: usize) -> Option<bool> {
        if depth > 32 {
            return None;
        }
        let d = self.defs.get(&id)?;
        match (d.class.opcode, d.operands.as_slice()) {
            (Op::ConstantTrue, []) => Some(true),
            (Op::ConstantFalse, []) => Some(false),
            (Op::IEqual, [Operand::IdRef(a), Operand::IdRef(b)]) => {
                Some(self.int(*a)? == self.int(*b)?)
            }
            (Op::INotEqual, [Operand::IdRef(a), Operand::IdRef(b)]) => {
                Some(self.int(*a)? != self.int(*b)?)
            }
            (Op::LogicalNot, [Operand::IdRef(a)]) => Some(!self.bool_depth(*a, depth + 1)?),
            (Op::Load, [Operand::IdRef(ptr), ..]) => {
                self.bool_depth(*self.stored.get(ptr)?, depth + 1)
            }
            _ => None,
        }
    }

    fn taken_target(&self, term: &Instruction) -> Option<Word> {
        match (term.class.opcode, term.operands.as_slice()) {
            (
                Op::BranchConditional,
                [Operand::IdRef(cond), Operand::IdRef(t), Operand::IdRef(f), ..],
            ) => Some(if self.bool(*cond)? { *t } else { *f }),
            (Op::Switch, [Operand::IdRef(selector), Operand::IdRef(default), cases @ ..]) => {
                let value = self.int(*selector)?;
                let mut target = *default;
                for case in cases.chunks(2) {
                    let literal = match case.first()? {
                        Operand::LiteralBit32(v) => u64::from(*v),
                        Operand::LiteralBit64(v) => *v,
                        _ => return None,
                    };
                    if literal == value {
                        let Operand::IdRef(t) = case.get(1)? else {
                            return None;
                        };
                        target = *t;
                    }
                }
                Some(target)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::{Block, Function, Module};

    fn inst(op: Op, ty: Option<Word>, id: Option<Word>, operands: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, id, operands)
    }
    fn block(id: Word, instructions: Vec<Instruction>) -> Block {
        Block {
            label: Some(inst(Op::Label, None, Some(id), vec![])),
            instructions,
        }
    }
    fn fixture(selector: Word) -> Ctx {
        let mut module = Module::new();
        module.types_global_values = vec![
            inst(Op::TypeSampler, None, Some(1), vec![]),
            inst(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Function),
                    Operand::IdRef(2),
                ],
            ),
            inst(
                Op::Constant,
                Some(2),
                Some(5),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(1)],
            ),
        ];
        let mut function = Function::new();
        function.blocks = vec![
            block(
                10,
                vec![
                    inst(
                        Op::Variable,
                        Some(3),
                        Some(40),
                        vec![Operand::StorageClass(StorageClass::Function)],
                    ),
                    inst(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(40), Operand::IdRef(selector)],
                    ),
                    inst(Op::Load, Some(2), Some(41), vec![Operand::IdRef(40)]),
                    inst(Op::Load, Some(1), Some(50), vec![Operand::IdRef(70)]),
                    inst(Op::Load, Some(1), Some(51), vec![Operand::IdRef(71)]),
                    inst(
                        Op::Switch,
                        None,
                        None,
                        vec![
                            Operand::IdRef(41),
                            Operand::IdRef(12),
                            Operand::LiteralBit32(1),
                            Operand::IdRef(11),
                        ],
                    ),
                ],
            ),
            block(
                11,
                vec![inst(Op::Branch, None, None, vec![Operand::IdRef(12)])],
            ),
            block(
                12,
                vec![
                    inst(
                        Op::Phi,
                        Some(1),
                        Some(60),
                        vec![
                            Operand::IdRef(50),
                            Operand::IdRef(10),
                            Operand::IdRef(51),
                            Operand::IdRef(11),
                        ],
                    ),
                    inst(Op::CopyObject, Some(1), Some(61), vec![Operand::IdRef(60)]),
                    inst(Op::Return, None, None, vec![]),
                ],
            ),
        ];
        module.functions.push(function);
        Ctx::new(module)
    }
    fn source(ctx: &Ctx) -> Word {
        ctx.module.functions[0]
            .blocks
            .last()
            .unwrap()
            .instructions
            .iter()
            .find(|i| i.result_id == Some(61))
            .unwrap()
            .operands
            .first()
            .and_then(|o| {
                if let Operand::IdRef(id) = o {
                    Some(*id)
                } else {
                    None
                }
            })
            .unwrap()
    }

    #[test]
    fn literal_selector_and_dominating_resources_fold_each_switch_arm() {
        for (selector, value) in [(5, 50), (6, 51)] {
            let mut ctx = fixture(selector);
            assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 1);
            assert_eq!(source(&ctx), value);
            assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
        }
    }
    #[test]
    fn a_resource_defined_only_in_the_taken_arm_keeps_its_phi() {
        let mut ctx = fixture(6);
        let resource = ctx.module.functions[0].blocks[0].instructions.remove(4);
        ctx.module.functions[0].blocks[1]
            .instructions
            .insert(0, resource);
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
        assert_eq!(source(&ctx), 60);
    }
    #[test]
    fn a_store_after_the_selector_load_does_not_prove_a_constant() {
        let mut ctx = fixture(5);
        ctx.module.functions[0].blocks[0].instructions.swap(1, 2);
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
    }
    #[test]
    fn a_conditional_store_does_not_prove_a_constant_on_other_paths() {
        let mut ctx = fixture(5);
        let body = &mut ctx.module.functions[0].blocks;
        let var = body[0].instructions.remove(0);
        let store = body[0].instructions.remove(0);
        body.insert(
            0,
            block(
                8,
                vec![
                    var,
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(80), Operand::IdRef(9), Operand::IdRef(10)],
                    ),
                ],
            ),
        );
        body.insert(
            1,
            block(
                9,
                vec![
                    store,
                    inst(Op::Branch, None, None, vec![Operand::IdRef(10)]),
                ],
            ),
        );
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
    }
    #[test]
    fn a_store_in_a_dominating_predecessor_still_folds() {
        let mut ctx = fixture(5);
        let body = &mut ctx.module.functions[0].blocks;
        let var = body[0].instructions.remove(0);
        let store = body[0].instructions.remove(0);
        body.insert(
            0,
            block(
                9,
                vec![
                    var,
                    store,
                    inst(Op::Branch, None, None, vec![Operand::IdRef(10)]),
                ],
            ),
        );
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 1);
        assert_eq!(source(&ctx), 50);
    }
    #[test]
    fn initialized_or_escaped_selector_storage_is_left_alone() {
        let mut ctx = fixture(5);
        ctx.module.functions[0].blocks[0].instructions[0]
            .operands
            .push(Operand::IdRef(6));
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
        let mut ctx = fixture(5);
        ctx.module.functions[0].blocks[0].instructions.insert(
            1,
            inst(Op::CopyObject, Some(3), Some(42), vec![Operand::IdRef(40)]),
        );
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
    }
    #[test]
    fn cyclic_selector_loads_are_not_evaluated_recursively() {
        let mut ctx = fixture(41);
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
    }
    #[test]
    fn ordinary_integer_phis_are_not_changed() {
        let mut ctx = fixture(5);
        let phi = &mut ctx.module.functions[0].blocks[2].instructions[0];
        phi.result_type = Some(2);
        phi.operands[0] = Operand::IdRef(5);
        phi.operands[2] = Operand::IdRef(6);
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
        assert_eq!(source(&ctx), 60);
    }
}
