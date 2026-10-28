use crate::fxhash::HashMap;
use crate::spirv_module::Module;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Function, Instruction};
use spirv::{Op, StorageClass, Word};

pub const DEFAULT_LOOP_BUDGET: u32 = 1 << 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopBudgetReport {
    pub loops_bounded_in_place: usize,
    pub loops_bounded_via_early_return: usize,
    pub loops_skipped: usize,
}

impl LoopBudgetReport {
    pub fn had_loops(&self) -> bool {
        self.loops_bounded_in_place + self.loops_bounded_via_early_return + self.loops_skipped > 0
    }

    pub fn loops_instrumented(&self) -> usize {
        self.loops_bounded_in_place + self.loops_bounded_via_early_return
    }

    pub fn needs_revalidation(&self) -> bool {
        self.loops_bounded_via_early_return > 0
    }
}

pub fn instrument_loop_budget(module: &mut Module, budget: u32) -> LoopBudgetReport {
    let mut report = LoopBudgetReport::default();
    if !module.functions.iter().any(function_has_loop) {
        return report;
    }

    let mut pool = ConstantPool::intern(module, budget);
    for index in 0..module.functions.len() {
        let mut function = std::mem::take(&mut module.functions[index]);
        instrument_function(&mut function, &mut pool, &mut report);
        module.functions[index] = function;
    }
    pool.flush(module);
    module.sync_id_bound_from_instructions();
    report
}

fn function_has_loop(function: &Function) -> bool {
    function
        .blocks
        .iter()
        .any(|block| loop_merge_index(block).is_some())
}

fn loop_merge_index(block: &Block) -> Option<usize> {
    block
        .instructions
        .iter()
        .position(|inst| inst.class.opcode == Op::LoopMerge)
}

fn block_label(block: &Block) -> Option<Word> {
    block.label.as_ref().and_then(|label| label.result_id)
}

fn terminator_targets(inst: &Instruction) -> Vec<Word> {
    let skip = match inst.class.opcode {
        Op::Branch => 0,
        Op::BranchConditional | Op::Switch => 1,
        _ => return Vec::new(),
    };
    inst.operands
        .iter()
        .skip(skip)
        .filter_map(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitForm {
    ContinueOnTrue,
    ExitOnTrue,
    ReturnFromBranch,
    ReturnViaGuard,
}

struct ConstantPool {
    next_id: Word,
    uint: Word,
    bool_ty: Word,
    ptr_function_uint: Word,
    const_zero: Word,
    const_one: Word,
    const_budget: Word,
    void_ty: Option<Word>,
    undef_by_type: HashMap<Word, Word>,
    additions: Vec<Instruction>,
}

impl ConstantPool {
    fn intern(module: &mut Module, budget: u32) -> Self {
        let mut pool = Self {
            next_id: module.id_bound().max(1),
            uint: 0,
            bool_ty: 0,
            ptr_function_uint: 0,
            const_zero: 0,
            const_one: 0,
            const_budget: 0,
            void_ty: module
                .types_global_values
                .iter()
                .find(|inst| inst.class.opcode == Op::TypeVoid)
                .and_then(|inst| inst.result_id),
            undef_by_type: HashMap::default(),
            additions: Vec::new(),
        };
        pool.uint = pool.find_or_create(module, Op::TypeInt, None, || {
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]
        });
        pool.bool_ty = pool.find_or_create(module, Op::TypeBool, None, Vec::new);
        let uint = pool.uint;
        pool.ptr_function_uint = pool.find_or_create(module, Op::TypePointer, None, || {
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(uint),
            ]
        });
        pool.const_zero = pool.constant(module, 0);
        pool.const_one = pool.constant(module, 1);
        pool.const_budget = pool.constant(module, budget);
        pool
    }

    fn fresh_id(&mut self) -> Word {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn find_or_create(
        &mut self,
        module: &Module,
        opcode: Op,
        result_type: Option<Word>,
        operands: impl FnOnce() -> Vec<Operand>,
    ) -> Word {
        let operands = operands();
        let matches = |inst: &Instruction| {
            inst.class.opcode == opcode
                && inst.result_type == result_type
                && inst.operands == operands
        };
        if let Some(id) = module
            .types_global_values
            .iter()
            .chain(&self.additions)
            .find(|inst| matches(inst))
            .and_then(|inst| inst.result_id)
        {
            return id;
        }
        let id = self.fresh_id();
        self.additions
            .push(Instruction::new(opcode, result_type, Some(id), operands));
        id
    }

    fn constant(&mut self, module: &Module, value: u32) -> Word {
        let uint = self.uint;
        self.find_or_create(module, Op::Constant, Some(uint), || {
            vec![Operand::LiteralBit32(value)]
        })
    }

    fn undef(&mut self, result_type: Word) -> Word {
        if let Some(id) = self.undef_by_type.get(&result_type) {
            return *id;
        }
        let id = self.fresh_id();
        self.additions.push(Instruction::new(
            Op::Undef,
            Some(result_type),
            Some(id),
            Vec::new(),
        ));
        self.undef_by_type.insert(result_type, id);
        id
    }

    fn flush(self, module: &mut Module) {
        module.types_global_values.extend(self.additions);
        module.set_id_bound(self.next_id);
    }
}

struct LoopPlan {
    header_id: Word,
    continue_id: Word,
    counter: Word,
    guard_id: Word,
    check_id: Word,
    exit_block_id: Word,
    exit: ExitForm,
}

fn instrument_function(
    function: &mut Function,
    pool: &mut ConstantPool,
    report: &mut LoopBudgetReport,
) {
    let mut plans = Vec::new();
    for block in &function.blocks {
        let Some(merge_index) = loop_merge_index(block) else {
            continue;
        };
        let Some(header_id) = block_label(block) else {
            continue;
        };
        let loop_merge = &block.instructions[merge_index];
        let (Some(Operand::IdRef(merge_id)), Some(Operand::IdRef(continue_id))) =
            (loop_merge.operands.first(), loop_merge.operands.get(1))
        else {
            continue;
        };
        let Some(terminator) = block.instructions.last() else {
            continue;
        };
        let Some(exit) = classify_exit(terminator, *merge_id) else {
            report.loops_skipped += 1;
            continue;
        };
        plans.push(LoopPlan {
            header_id,
            continue_id: *continue_id,
            counter: pool.fresh_id(),
            guard_id: pool.fresh_id(),
            check_id: pool.fresh_id(),
            exit_block_id: pool.fresh_id(),
            exit,
        });
    }

    for plan in plans {
        apply_plan(function, &plan, pool);
        match plan.exit {
            ExitForm::ContinueOnTrue | ExitForm::ExitOnTrue => report.loops_bounded_in_place += 1,
            ExitForm::ReturnFromBranch | ExitForm::ReturnViaGuard => {
                report.loops_bounded_via_early_return += 1
            }
        }
    }
}

fn classify_exit(terminator: &Instruction, merge_id: Word) -> Option<ExitForm> {
    match terminator.class.opcode {
        Op::BranchConditional => {
            let targets = terminator_targets(terminator);
            match (targets.first(), targets.get(1)) {
                (Some(_), Some(false_target)) if *false_target == merge_id => {
                    Some(ExitForm::ContinueOnTrue)
                }
                (Some(true_target), Some(_)) if *true_target == merge_id => {
                    Some(ExitForm::ExitOnTrue)
                }
                _ => Some(ExitForm::ReturnViaGuard),
            }
        }
        Op::Branch => Some(ExitForm::ReturnFromBranch),
        Op::Switch => Some(ExitForm::ReturnViaGuard),
        _ => None,
    }
}

fn apply_plan(function: &mut Function, plan: &LoopPlan, pool: &mut ConstantPool) {
    declare_counter(function, plan, pool);
    reset_counter_in_preheaders(function, plan, pool);

    let Some(header_index) = function
        .blocks
        .iter()
        .position(|block| block_label(block) == Some(plan.header_id))
    else {
        return;
    };

    let in_budget = bump_counter(&mut function.blocks[header_index], plan, pool);

    match plan.exit {
        ExitForm::ContinueOnTrue | ExitForm::ExitOnTrue => {
            fold_budget_into_condition(&mut function.blocks[header_index], plan, pool, in_budget);
        }
        ExitForm::ReturnFromBranch | ExitForm::ReturnViaGuard => {
            divert_to_return(function, header_index, plan, pool, in_budget);
        }
    }
}

fn bump_counter(header: &mut Block, plan: &LoopPlan, pool: &mut ConstantPool) -> Word {
    let Some(merge_index) = loop_merge_index(header) else {
        return pool.const_zero;
    };
    let current = pool.fresh_id();
    let next = pool.fresh_id();
    let in_budget = pool.fresh_id();
    let emitted = [
        Instruction::new(
            Op::Load,
            Some(pool.uint),
            Some(current),
            vec![Operand::IdRef(plan.counter)],
        ),
        Instruction::new(
            Op::IAdd,
            Some(pool.uint),
            Some(next),
            vec![Operand::IdRef(current), Operand::IdRef(pool.const_one)],
        ),
        Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(plan.counter), Operand::IdRef(next)],
        ),
        Instruction::new(
            Op::ULessThan,
            Some(pool.bool_ty),
            Some(in_budget),
            vec![Operand::IdRef(current), Operand::IdRef(pool.const_budget)],
        ),
    ];
    for (offset, inst) in emitted.into_iter().enumerate() {
        header.instructions.insert(merge_index + offset, inst);
    }
    in_budget
}

fn fold_budget_into_condition(
    header: &mut Block,
    plan: &LoopPlan,
    pool: &mut ConstantPool,
    in_budget: Word,
) {
    let Some(merge_index) = loop_merge_index(header) else {
        return;
    };
    let Some(original) = header
        .instructions
        .last()
        .and_then(|inst| inst.operands.first().cloned())
    else {
        return;
    };
    let Operand::IdRef(original_condition) = original else {
        return;
    };

    let combined = pool.fresh_id();
    let mut emitted = Vec::new();
    match plan.exit {
        ExitForm::ContinueOnTrue => emitted.push(Instruction::new(
            Op::LogicalAnd,
            Some(pool.bool_ty),
            Some(combined),
            vec![
                Operand::IdRef(original_condition),
                Operand::IdRef(in_budget),
            ],
        )),
        ExitForm::ExitOnTrue => {
            let exhausted = pool.fresh_id();
            emitted.push(Instruction::new(
                Op::LogicalNot,
                Some(pool.bool_ty),
                Some(exhausted),
                vec![Operand::IdRef(in_budget)],
            ));
            emitted.push(Instruction::new(
                Op::LogicalOr,
                Some(pool.bool_ty),
                Some(combined),
                vec![
                    Operand::IdRef(original_condition),
                    Operand::IdRef(exhausted),
                ],
            ));
        }
        ExitForm::ReturnFromBranch | ExitForm::ReturnViaGuard => return,
    }
    for (offset, inst) in emitted.into_iter().enumerate() {
        header.instructions.insert(merge_index + offset, inst);
    }
    if let Some(terminator) = header.instructions.last_mut() {
        terminator.operands[0] = Operand::IdRef(combined);
    }
}

fn divert_to_return(
    function: &mut Function,
    header_index: usize,
    plan: &LoopPlan,
    pool: &mut ConstantPool,
    in_budget: Word,
) {
    let guard_terminator = match plan.exit {
        ExitForm::ReturnFromBranch => None,
        _ => function.blocks[header_index].instructions.pop(),
    };

    let stay = match &guard_terminator {
        Some(_) => plan.guard_id,
        None => {
            let Some(body) = function.blocks[header_index]
                .instructions
                .last()
                .and_then(|terminator| terminator_targets(terminator).first().copied())
            else {
                return;
            };
            function.blocks[header_index].instructions.pop();
            body
        }
    };

    function.blocks[header_index]
        .instructions
        .push(Instruction::new(
            Op::Branch,
            None,
            None,
            vec![Operand::IdRef(plan.check_id)],
        ));

    let mut check = Block::new();
    check.label = Some(Instruction::new(
        Op::Label,
        None,
        Some(plan.check_id),
        Vec::new(),
    ));
    check.instructions.push(Instruction::new(
        Op::SelectionMerge,
        None,
        None,
        vec![
            Operand::IdRef(stay),
            Operand::SelectionControl(spirv::SelectionControl::NONE),
        ],
    ));
    check.instructions.push(Instruction::new(
        Op::BranchConditional,
        None,
        None,
        vec![
            Operand::IdRef(in_budget),
            Operand::IdRef(stay),
            Operand::IdRef(plan.exit_block_id),
        ],
    ));

    let exit = build_return_block(function, plan.exit_block_id, pool);

    let mut insert_at = header_index + 1;
    for block in [check, exit] {
        function.blocks.insert(insert_at, block);
        insert_at += 1;
    }

    match guard_terminator {
        Some(original) => {
            let original_targets = terminator_targets(&original);
            let mut guard = Block::new();
            guard.label = Some(Instruction::new(
                Op::Label,
                None,
                Some(plan.guard_id),
                Vec::new(),
            ));
            guard.instructions.push(original);
            function.blocks.insert(insert_at, guard);
            for target in &original_targets {
                repoint_phi_predecessor(function, *target, plan.header_id, plan.guard_id);
            }
        }
        None => repoint_phi_predecessor(function, stay, plan.header_id, plan.check_id),
    }
}

fn build_return_block(function: &Function, label: Word, pool: &mut ConstantPool) -> Block {
    let return_type = function.def.as_ref().and_then(|def| def.result_type);
    let terminator = match return_type {
        Some(ty) if Some(ty) != pool.void_ty => {
            let undef = pool.undef(ty);
            Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(undef)])
        }
        _ => Instruction::new(Op::Return, None, None, Vec::new()),
    };
    let mut block = Block::new();
    block.label = Some(Instruction::new(Op::Label, None, Some(label), Vec::new()));
    block.instructions.push(terminator);
    block
}

fn declare_counter(function: &mut Function, plan: &LoopPlan, pool: &ConstantPool) {
    let Some(entry) = function.blocks.first_mut() else {
        return;
    };
    let insertion = entry
        .instructions
        .iter()
        .position(|inst| inst.class.opcode != Op::Variable)
        .unwrap_or(entry.instructions.len());
    entry.instructions.insert(
        insertion,
        Instruction::new(
            Op::Variable,
            Some(pool.ptr_function_uint),
            Some(plan.counter),
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(pool.const_zero),
            ],
        ),
    );
}

fn reset_counter_in_preheaders(function: &mut Function, plan: &LoopPlan, pool: &ConstantPool) {
    for block in &mut function.blocks {
        let Some(label) = block_label(block) else {
            continue;
        };
        if label == plan.header_id || label == plan.continue_id {
            continue;
        }
        let Some(terminator) = block.instructions.last() else {
            continue;
        };
        if !terminator_targets(terminator).contains(&plan.header_id) {
            continue;
        }
        let mut insertion = block.instructions.len() - 1;
        if insertion > 0
            && matches!(
                block.instructions[insertion - 1].class.opcode,
                Op::SelectionMerge | Op::LoopMerge
            )
        {
            insertion -= 1;
        }
        block.instructions.insert(
            insertion,
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![
                    Operand::IdRef(plan.counter),
                    Operand::IdRef(pool.const_zero),
                ],
            ),
        );
    }
}

fn repoint_phi_predecessor(function: &mut Function, target: Word, from: Word, to: Word) {
    let Some(block) = function
        .blocks
        .iter_mut()
        .find(|block| block_label(block) == Some(target))
    else {
        return;
    };
    for inst in &mut block.instructions {
        if inst.class.opcode != Op::Phi {
            break;
        }
        for pair in inst.operands.chunks_mut(2) {
            if let [_, Operand::IdRef(predecessor)] = pair {
                if *predecessor == from {
                    *predecessor = to;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: Word = 20;
    const CONTINUE: Word = 30;
    const BODY: Word = 40;

    fn terminator(opcode: Op, operands: Vec<Operand>) -> Instruction {
        Instruction::new(opcode, None, None, operands)
    }

    #[test]
    fn a_header_that_exits_on_false_folds_the_budget_with_and() {
        let inst = terminator(
            Op::BranchConditional,
            vec![
                Operand::IdRef(7),
                Operand::IdRef(BODY),
                Operand::IdRef(MERGE),
            ],
        );
        assert_eq!(classify_exit(&inst, MERGE), Some(ExitForm::ContinueOnTrue));
    }

    #[test]
    fn a_header_that_exits_on_true_folds_the_budget_with_or() {
        let inst = terminator(
            Op::BranchConditional,
            vec![
                Operand::IdRef(7),
                Operand::IdRef(MERGE),
                Operand::IdRef(BODY),
            ],
        );
        assert_eq!(classify_exit(&inst, MERGE), Some(ExitForm::ExitOnTrue));
    }

    #[test]
    fn an_unconditional_header_exits_by_returning() {
        let inst = terminator(Op::Branch, vec![Operand::IdRef(BODY)]);
        assert_eq!(
            classify_exit(&inst, MERGE),
            Some(ExitForm::ReturnFromBranch)
        );
    }

    #[test]
    fn a_conditional_header_with_no_exit_returns_through_a_guard() {
        let inst = terminator(
            Op::BranchConditional,
            vec![
                Operand::IdRef(7),
                Operand::IdRef(BODY),
                Operand::IdRef(BODY + 1),
            ],
        );
        assert_eq!(classify_exit(&inst, MERGE), Some(ExitForm::ReturnViaGuard));
    }

    #[test]
    fn a_header_that_returns_cannot_iterate() {
        assert_eq!(
            classify_exit(&terminator(Op::Return, Vec::new()), MERGE),
            None
        );
        assert_eq!(
            classify_exit(&terminator(Op::Unreachable, Vec::new()), MERGE),
            None
        );
    }

    #[test]
    fn branch_conditional_targets_skip_the_condition_operand() {
        let inst = terminator(
            Op::BranchConditional,
            vec![
                Operand::IdRef(7),
                Operand::IdRef(BODY),
                Operand::IdRef(MERGE),
            ],
        );
        assert_eq!(terminator_targets(&inst), vec![BODY, MERGE]);
    }

    #[test]
    fn switch_targets_skip_the_selector_operand() {
        let inst = terminator(
            Op::Switch,
            vec![
                Operand::IdRef(7),
                Operand::IdRef(MERGE),
                Operand::LiteralBit32(1),
                Operand::IdRef(BODY),
            ],
        );
        assert_eq!(terminator_targets(&inst), vec![MERGE, BODY]);
    }

    #[test]
    fn a_non_iterating_header_is_counted_as_skipped() {
        let mut block = Block::new();
        block.label = Some(Instruction::new(Op::Label, None, Some(10), Vec::new()));
        block.instructions.push(terminator(
            Op::LoopMerge,
            vec![
                Operand::IdRef(MERGE),
                Operand::IdRef(CONTINUE),
                Operand::LoopControl(spirv::LoopControl::NONE),
            ],
        ));
        block.instructions.push(terminator(Op::Return, Vec::new()));
        let mut function = Function::new();
        function.blocks.push(block);

        let mut module = Module::new();
        let mut pool = ConstantPool::intern(&mut module, 16);
        let mut report = LoopBudgetReport::default();
        instrument_function(&mut function, &mut pool, &mut report);

        assert_eq!(report.loops_instrumented(), 0);
        assert_eq!(report.loops_skipped, 1);
        assert!(report.had_loops());
        assert!(!report.needs_revalidation());
    }
}
