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
// recovered") and the vertex shader fails: Apple's GL then runs it on the CPU. The selector is a constant stored into
// a function variable (once, or the same constant before each call of the helper: Dome Keeper's Godot
// `make_outline(vec2, sampler2D, vec2)` is called twice with unit 3); this pass finds the edges that run when every
// branch on a known condition takes its one target, and replaces a phi with a single live incoming value by that
// value. The emitter may lower the switch to a chain of compares with one header per case and the last cases
// joined by an inner phi first; reachability covers that as well as the plain switch. Phis of other types, and
// branches whose condition is not a constant, are left alone.
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

    // The edges that can run: from the entry block, a terminator whose condition is known follows only its taken
    // target, any other follows all of them. The emitter may have turned the switch into a chain of compares, each
    // case leaving from a header of its own, so a phi's taken edge is found by reachability rather than by one header.
    let Some(entry) = function.blocks.first().and_then(label) else {
        return 0;
    };
    let mut executable_blocks: HashSet<Word> = HashSet::from([entry]);
    let mut executable_edges: HashSet<(Word, Word)> = HashSet::new();
    let mut work = vec![entry];
    while let Some(block) = work.pop() {
        let Some(term) = terminators.get(&block) else {
            continue;
        };
        let targets = match eval.taken_target(term) {
            Some(target) => vec![target],
            None => branch_targets(term),
        };
        for target in targets {
            executable_edges.insert((block, target));
            if executable_blocks.insert(target) {
                work.push(target);
            }
        }
    }

    // Resource phis: every incoming value is an image/sampler, or another resource phi (a switch lowered to nested
    // selections joins its last cases first).
    let phis: Vec<(usize, usize, Word, Word, Vec<(Word, Word)>)> = function
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(b, block)| Some((b, label(block)?, block)))
        .flat_map(|(b, merge, block)| {
            block
                .instructions
                .iter()
                .enumerate()
                .take_while(|(_, i)| i.class.opcode == Op::Phi)
                .filter_map(move |(p, phi)| {
                    phi.result_type?;
                    let incoming = phi
                        .operands
                        .chunks(2)
                        .filter_map(|pair| match pair {
                            [Operand::IdRef(v), Operand::IdRef(b)] => Some((*v, *b)),
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    Some((b, p, merge, phi.result_id?, incoming))
                })
        })
        .collect();
    let mut resource_phis: HashSet<Word> = HashSet::new();
    loop {
        let before = resource_phis.len();
        for (_, _, _, result, incoming) in &phis {
            if incoming.len() >= 2
                && incoming.iter().all(|(v, _)| {
                    resource_phis.contains(v)
                        || defs
                            .get(v)
                            .and_then(|d| d.result_type)
                            .is_some_and(|t| opaque_types.contains(&t))
                })
            {
                resource_phis.insert(*result);
            }
        }
        if resource_phis.len() == before {
            break;
        }
    }

    let mut chosen: HashMap<Word, (Word, (usize, usize))> = HashMap::new();
    for (block_index, phi_index, merge, result, incoming) in &phis {
        if !resource_phis.contains(result) || !executable_blocks.contains(merge) {
            continue;
        }
        let live: HashSet<Word> = incoming
            .iter()
            .filter(|(_, pred)| executable_edges.contains(&(*pred, *merge)))
            .map(|(v, _)| *v)
            .collect();
        if let [value] = live.into_iter().collect::<Vec<_>>()[..] {
            chosen.insert(*result, (value, (*block_index, *phi_index)));
        }
    }
    let resolve = |mut id: Word| {
        let mut steps = 0;
        while let Some((next, _)) = chosen.get(&id) {
            id = *next;
            steps += 1;
            if steps > chosen.len() {
                break;
            }
        }
        id
    };
    let mut replace: HashMap<Word, Word> = HashMap::new();
    for (&result, &(_, at)) in &chosen {
        let value = resolve(result);
        // The branches are not removed. A value defined only in the chosen arm still does not dominate the merge in
        // SPIR-V's CFG, even when that arm's condition is constant. Keep that phi intact.
        if value != result && flow.value_dominates(value, at) {
            replace.insert(result, value);
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
                if values.is_empty() || !is_function_var(ptr) || escaped.contains(ptr) {
                    return false;
                }
                // Every store writes the same literal constant (a GLSL helper taking a sampler, called from several
                // places with the same texture unit, stores that unit before each call) and every load runs after one
                // of them on all paths to it. A store after a load, or on only one path to it, proves nothing.
                // Literal constants only, so cyclic stores/loads cannot recurse during evaluation.
                let value = values[0].0;
                values.iter().all(|(v, _)| *v == value)
                    && defs.get(&value).is_some_and(|d| {
                        matches!(
                            d.class.opcode,
                            Op::Constant | Op::ConstantNull | Op::ConstantTrue | Op::ConstantFalse
                        )
                    })
                    && loads.get(ptr).is_some_and(|uses| {
                        uses.iter()
                            .all(|&at| values.iter().any(|&(_, position)| flow.dominates(position, at)))
                    })
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
    // The switch as the emitter lowers it: a chain of compares, one header per case, the last two cases joined by an
    // inner phi before the outer one.
    fn chain_fixture(selector: Word) -> Ctx {
        let mut ctx = fixture(selector);
        ctx.module
            .types_global_values
            .push(inst(Op::TypeBool, None, Some(4), vec![]));
        let body = &mut ctx.module.functions[0].blocks;
        body[0].instructions.pop(); // the switch
        body[0]
            .instructions
            .push(inst(Op::Load, Some(1), Some(52), vec![Operand::IdRef(72)]));
        body[0].instructions.push(inst(
            Op::IEqual,
            Some(4),
            Some(43),
            vec![Operand::IdRef(41), Operand::IdRef(5)],
        ));
        body[0].instructions.push(inst(
            Op::BranchConditional,
            None,
            None,
            vec![Operand::IdRef(43), Operand::IdRef(11), Operand::IdRef(13)],
        ));
        body.insert(
            2,
            block(
                13,
                vec![
                    inst(
                        Op::IEqual,
                        Some(4),
                        Some(44),
                        vec![Operand::IdRef(41), Operand::IdRef(6)],
                    ),
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(44), Operand::IdRef(14), Operand::IdRef(15)],
                    ),
                ],
            ),
        );
        body.insert(
            3,
            block(
                14,
                vec![inst(Op::Branch, None, None, vec![Operand::IdRef(15)])],
            ),
        );
        body.insert(
            4,
            block(
                15,
                vec![
                    inst(
                        Op::Phi,
                        Some(1),
                        Some(62),
                        vec![
                            Operand::IdRef(51),
                            Operand::IdRef(13),
                            Operand::IdRef(52),
                            Operand::IdRef(14),
                        ],
                    ),
                    inst(Op::Branch, None, None, vec![Operand::IdRef(12)]),
                ],
            ),
        );
        let phi = &mut body[5].instructions[0];
        phi.operands = vec![
            Operand::IdRef(50),
            Operand::IdRef(11),
            Operand::IdRef(62),
            Operand::IdRef(15),
        ];
        ctx
    }
    #[test]
    fn a_switch_lowered_to_a_compare_chain_folds_through_the_inner_phi() {
        for (selector, value) in [(5, 50), (6, 52)] {
            let mut ctx = chain_fixture(selector);
            assert!(fold_constant_resource_phis(&mut ctx, 0) >= 1);
            assert_eq!(source(&ctx), value);
        }
    }
    #[test]
    fn a_compare_chain_on_an_unknown_selector_is_left_alone() {
        let mut ctx = chain_fixture(5);
        ctx.module.functions[0].blocks[0].instructions.remove(1); // the store
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 0);
        assert_eq!(source(&ctx), 60);
    }
    #[test]
    fn the_same_constant_stored_before_each_load_still_folds() {
        let mut ctx = fixture(6);
        let store = ctx.module.functions[0].blocks[0].instructions[1].clone();
        ctx.module.functions[0].blocks[0].instructions.insert(3, store);
        assert_eq!(fold_constant_resource_phis(&mut ctx, 0), 1);
        assert_eq!(source(&ctx), 51);
    }
    #[test]
    fn different_constants_stored_into_the_selector_do_not_fold() {
        let mut ctx = fixture(6);
        let mut store = ctx.module.functions[0].blocks[0].instructions[1].clone();
        store.operands[1] = Operand::IdRef(5);
        ctx.module.functions[0].blocks[0].instructions.insert(3, store);
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
