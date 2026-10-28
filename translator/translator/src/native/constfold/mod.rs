mod prune;
pub(in crate::native) use prune::*;
mod constants;
pub(in crate::native) use constants::*;
mod nonzero;
pub(in crate::native) use nonzero::*;
mod eval;
pub(in crate::native) use eval::*;
mod simplify;
pub(in crate::native) use simplify::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fxhash::HashMap;
    use crate::spirv_module::Operand;
    use crate::spirv_module::{Block, Function, Instruction, Module, ModuleHeader};
    use spirv::{Op, StorageClass, Word};

    fn inst(op: Op, ty: Option<Word>, res: Option<Word>, ops: Vec<Operand>) -> Instruction {
        Instruction::new(op, ty, res, ops)
    }

    #[test]
    fn prunes_dead_arm_gated_by_bitwise_fc_predicate() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
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
                Some(1),
                Some(11),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(Op::ConstantNull, Some(1), Some(12), vec![]),
            inst(
                Op::Variable,
                Some(3),
                Some(20),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(12),
                ],
            ),
        ];
        let entry = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![
                inst(Op::Load, Some(1), Some(31), vec![Operand::IdRef(20)]),
                inst(
                    Op::ShiftRightLogical,
                    Some(1),
                    Some(32),
                    vec![Operand::IdRef(31), Operand::IdRef(10)],
                ),
                inst(
                    Op::BitwiseAnd,
                    Some(1),
                    Some(33),
                    vec![Operand::IdRef(32), Operand::IdRef(11)],
                ),
                inst(
                    Op::IEqual,
                    Some(2),
                    Some(38),
                    vec![Operand::IdRef(33), Operand::IdRef(10)],
                ),
                inst(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(36),
                        Operand::SelectionControl(spirv::SelectionControl::NONE),
                    ],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(38), Operand::IdRef(34), Operand::IdRef(35)],
                ),
            ],
        };
        let then_b = Block {
            label: Some(inst(Op::Label, None, Some(34), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
        };
        let else_b = Block {
            label: Some(inst(Op::Label, None, Some(35), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
        };
        let merge_b = Block {
            label: Some(inst(Op::Label, None, Some(36), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        };
        let mut func = Function::new();
        func.blocks = vec![entry, then_b, else_b, merge_b];
        m.functions = vec![func];
        m.debug_names = vec![
            inst(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(35),
                    Operand::LiteralString("dead-arm".into()),
                ],
            ),
            inst(
                Op::Name,
                None,
                None,
                vec![Operand::IdRef(39), Operand::LiteralString("unowned".into())],
            ),
        ];

        crate::native::rewrites::prune_constant_branches_module(&mut m).expect("expected a fold");
        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            !labels.contains(&35),
            "dead else-arm (%35) should be pruned: {labels:?}"
        );
        assert!(
            labels.contains(&34),
            "taken then-arm (%34) should survive: {labels:?}"
        );
        assert_eq!(m.debug_names.len(), 1);
        assert_eq!(m.debug_names[0].operands[0], Operand::IdRef(39));
    }

    #[test]
    fn collapses_an_identical_arm_select_and_prunes_the_arm_it_gates() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(Op::ConstantTrue, Some(2), Some(11), vec![]),
            inst(Op::ConstantNull, Some(1), Some(12), vec![]),
            inst(
                Op::Variable,
                Some(3),
                Some(20),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(12),
                ],
            ),
        ];
        let mut function = Function::new();
        function.parameters = vec![inst(Op::FunctionParameter, Some(1), Some(21), vec![])];
        function.blocks = vec![
            Block {
                label: Some(inst(Op::Label, None, Some(30), vec![])),
                instructions: vec![
                    inst(
                        Op::INotEqual,
                        Some(2),
                        Some(31),
                        vec![Operand::IdRef(21), Operand::IdRef(10)],
                    ),
                    inst(
                        Op::Select,
                        Some(2),
                        Some(32),
                        vec![Operand::IdRef(31), Operand::IdRef(11), Operand::IdRef(11)],
                    ),
                    inst(Op::SelectionMerge, None, None, vec![Operand::IdRef(36)]),
                    inst(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![Operand::IdRef(32), Operand::IdRef(34), Operand::IdRef(35)],
                    ),
                ],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(34), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(35), vec![])),
                instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
            },
            Block {
                label: Some(inst(Op::Label, None, Some(36), vec![])),
                instructions: vec![inst(Op::Return, None, None, vec![])],
            },
        ];
        m.functions.push(function);

        crate::native::rewrites::prune_constant_branches_module(&mut m).expect("expected a fold");
        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            !labels.contains(&35),
            "the arm the collapsed select gated is dead: {labels:?}"
        );
        assert!(labels.contains(&34), "the taken arm survives: {labels:?}");
        assert!(
            !m.functions[0]
                .blocks
                .iter()
                .flat_map(|b| b.instructions.iter())
                .any(|i| i.class.opcode == Op::Select),
            "the identical-arm select itself is gone"
        );
    }

    #[test]
    fn collapse_constant_selects_refuses_a_per_lane_condition() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::LiteralBit32(2)],
            ),
            inst(
                Op::TypeVector,
                None,
                Some(4),
                vec![Operand::IdRef(2), Operand::LiteralBit32(2)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(7)],
            ),
        ];
        let select = |result: Word, ty: Word, cond: Word, a: Word, b: Word| {
            inst(
                Op::Select,
                Some(ty),
                Some(result),
                vec![Operand::IdRef(cond), Operand::IdRef(a), Operand::IdRef(b)],
            )
        };
        let mut function = Function::new();
        function.parameters = vec![
            inst(Op::FunctionParameter, Some(3), Some(20), vec![]),
            inst(Op::FunctionParameter, Some(3), Some(21), vec![]),
            inst(Op::FunctionParameter, Some(1), Some(22), vec![]),
            inst(Op::FunctionParameter, Some(1), Some(23), vec![]),
        ];
        function.blocks = vec![Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![
                inst(
                    Op::INotEqual,
                    Some(4),
                    Some(31),
                    vec![Operand::IdRef(20), Operand::IdRef(21)],
                ),
                inst(
                    Op::INotEqual,
                    Some(2),
                    Some(32),
                    vec![Operand::IdRef(22), Operand::IdRef(23)],
                ),
                select(33, 3, 31, 20, 21),
                select(34, 1, 32, 22, 23),
                inst(Op::Return, None, None, vec![]),
            ],
        }];
        m.functions.push(function);

        let vals: HashMap<Word, i128> = [(31, 1), (32, 1)].into_iter().collect();
        let lane_conditions = bool_vector_valued_ids(&m);
        assert!(
            lane_conditions.contains(&31) && !lane_conditions.contains(&32),
            "only the v2bool comparison is a per-lane condition"
        );
        assert!(collapse_constant_selects(
            &mut m.functions[0],
            &vals,
            &lane_conditions
        ));

        let body = &m.functions[0].blocks[0].instructions;
        assert!(
            body.iter().any(|i| i.result_id == Some(33)),
            "the per-lane select survives"
        );
        assert!(
            !body.iter().any(|i| i.result_id == Some(34)),
            "the scalar-condition select folds to its true arm"
        );
        assert_eq!(
            body.last().expect("a terminator").class.opcode,
            Op::Return,
            "the terminator is untouched"
        );
    }

    #[test]
    fn keeps_loop_exit_gated_by_loop_carried_phi() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(70));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(11),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(12),
                vec![Operand::LiteralBit32(48)],
            ),
        ];
        let entry = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(31)])],
        };
        let header = Block {
            label: Some(inst(Op::Label, None, Some(31), vec![])),
            instructions: vec![
                inst(
                    Op::Phi,
                    Some(1),
                    Some(50),
                    vec![
                        Operand::IdRef(10),
                        Operand::IdRef(30),
                        Operand::IdRef(51),
                        Operand::IdRef(32),
                    ],
                ),
                inst(
                    Op::IAdd,
                    Some(1),
                    Some(51),
                    vec![Operand::IdRef(50), Operand::IdRef(11)],
                ),
                inst(
                    Op::ULessThan,
                    Some(2),
                    Some(52),
                    vec![Operand::IdRef(51), Operand::IdRef(12)],
                ),
                inst(
                    Op::LoopMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(33),
                        Operand::IdRef(32),
                        Operand::LoopControl(spirv::LoopControl::NONE),
                    ],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(52), Operand::IdRef(32), Operand::IdRef(33)],
                ),
            ],
        };
        let latch = Block {
            label: Some(inst(Op::Label, None, Some(32), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(31)])],
        };
        let exit = Block {
            label: Some(inst(Op::Label, None, Some(33), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        };
        let mut func = Function::new();
        func.blocks = vec![entry, header, latch, exit];
        m.functions = vec![func];

        prune_constant_branches(&mut m);

        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            labels.contains(&33),
            "loop exit must not be pruned from an unknown induction phi: {labels:?}"
        );
        let header_term = m.functions[0].blocks[1]
            .instructions
            .last()
            .expect("header terminator");
        assert_eq!(header_term.class.opcode, Op::BranchConditional);
    }

    fn module_with_ptr_cycle(extra_use: bool) -> Module {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(70));
        m.types_global_values = vec![
            inst(
                Op::TypeFloat,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(2),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(11),
                vec![Operand::LiteralBit32(1)],
            ),
            inst(Op::ConstantNull, Some(2), Some(12), vec![]),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::Variable,
                Some(3),
                Some(20),
                vec![Operand::StorageClass(StorageClass::Private)],
            ),
        ];
        let pre = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(31)])],
        };
        let hdr = Block {
            label: Some(inst(Op::Label, None, Some(31), vec![])),
            instructions: vec![
                inst(
                    Op::Phi,
                    Some(2),
                    Some(50),
                    vec![
                        Operand::IdRef(51),
                        Operand::IdRef(32),
                        Operand::IdRef(12),
                        Operand::IdRef(30),
                    ],
                ),
                inst(Op::Branch, None, None, vec![Operand::IdRef(32)]),
            ],
        };
        let mut latch_insts = vec![inst(
            Op::PtrAccessChain,
            Some(2),
            Some(51),
            vec![Operand::IdRef(50), Operand::IdRef(11)],
        )];
        if extra_use {
            latch_insts.push(inst(Op::Load, Some(1), Some(60), vec![Operand::IdRef(50)]));
            latch_insts.push(inst(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(20), Operand::IdRef(60)],
            ));
        }
        latch_insts.push(inst(Op::Branch, None, None, vec![Operand::IdRef(31)]));
        let latch = Block {
            label: Some(inst(Op::Label, None, Some(32), vec![])),
            instructions: latch_insts,
        };
        let mut func = Function::new();
        func.blocks = vec![pre, hdr, latch];
        m.functions = vec![func];
        m
    }

    #[test]
    fn dce_collects_dead_pointer_induction_cycle() {
        let mut m = module_with_ptr_cycle(false);
        assert!(dce(&mut m), "expected the dead cycle to be removed");
        let results: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .flat_map(|b| b.instructions.iter().filter_map(|i| i.result_id))
            .collect();
        assert!(
            !results.contains(&50) && !results.contains(&51),
            "dead self-referential pointer cycle (%50/%51) should be gone: {results:?}"
        );
    }

    #[test]
    fn dce_keeps_live_pointer_induction_cycle() {
        let mut m = module_with_ptr_cycle(true);
        dce(&mut m);
        let results: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .flat_map(|b| b.instructions.iter().filter_map(|i| i.result_id))
            .collect();
        assert!(
            results.contains(&50) && results.contains(&51),
            "a cycle with an external consumer (%60) must survive: {results:?}"
        );
    }

    #[test]
    fn prunes_dead_arm_gated_by_vector_fc_element() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(3),
                ],
            ),
            inst(Op::ConstantNull, Some(1), Some(10), vec![]),
            inst(Op::ConstantNull, Some(3), Some(11), vec![]),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(11),
                ],
            ),
        ];
        let entry = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![
                inst(Op::Load, Some(3), Some(31), vec![Operand::IdRef(20)]),
                inst(
                    Op::CompositeExtract,
                    Some(1),
                    Some(32),
                    vec![Operand::IdRef(31), Operand::LiteralBit32(0)],
                ),
                inst(
                    Op::IEqual,
                    Some(2),
                    Some(33),
                    vec![Operand::IdRef(32), Operand::IdRef(10)],
                ),
                inst(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(36),
                        Operand::SelectionControl(spirv::SelectionControl::NONE),
                    ],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(33), Operand::IdRef(34), Operand::IdRef(35)],
                ),
            ],
        };
        let then_b = Block {
            label: Some(inst(Op::Label, None, Some(34), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
        };
        let else_b = Block {
            label: Some(inst(Op::Label, None, Some(35), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(36)])],
        };
        let merge_b = Block {
            label: Some(inst(Op::Label, None, Some(36), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        };
        let mut func = Function::new();
        func.blocks = vec![entry, then_b, else_b, merge_b];
        m.functions = vec![func];

        assert!(prune_constant_branches(&mut m), "expected a fold");
        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            !labels.contains(&35),
            "dead else-arm should be pruned: {labels:?}"
        );
        assert!(
            labels.contains(&34),
            "taken then-arm should survive: {labels:?}"
        );
    }

    #[test]
    fn folds_grid_stride_early_return_guard() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Input),
                    Operand::IdRef(3),
                ],
            ),
            inst(
                Op::Constant,
                Some(1),
                Some(10),
                vec![Operand::LiteralBit32(0xFFFF_FFFF)],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(20),
                vec![Operand::StorageClass(StorageClass::Input)],
            ),
        ];
        m.annotations = vec![inst(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(20),
                Operand::Decoration(spirv::Decoration::BuiltIn),
                Operand::BuiltIn(spirv::BuiltIn::NumWorkgroups),
            ],
        )];
        let entry = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![
                inst(Op::Load, Some(3), Some(31), vec![Operand::IdRef(20)]),
                inst(
                    Op::CompositeExtract,
                    Some(1),
                    Some(32),
                    vec![Operand::IdRef(31), Operand::LiteralBit32(0)],
                ),
                inst(
                    Op::IAdd,
                    Some(1),
                    Some(33),
                    vec![Operand::IdRef(32), Operand::IdRef(10)],
                ),
                inst(
                    Op::UGreaterThan,
                    Some(2),
                    Some(38),
                    vec![Operand::IdRef(32), Operand::IdRef(33)],
                ),
                inst(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(34),
                        Operand::SelectionControl(spirv::SelectionControl::NONE),
                    ],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(38), Operand::IdRef(35), Operand::IdRef(34)],
                ),
            ],
        };
        let body_b = Block {
            label: Some(inst(Op::Label, None, Some(34), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(35)])],
        };
        let ret_b = Block {
            label: Some(inst(Op::Label, None, Some(35), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        };
        let mut func = Function::new();
        func.blocks = vec![entry, body_b, ret_b];
        m.functions = vec![func];

        assert!(
            prune_constant_branches(&mut m),
            "expected the guard to fold"
        );
        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            !labels.contains(&34),
            "dead compute arm (%34) should be pruned: {labels:?}"
        );
    }

    #[test]
    fn folds_unsigned_less_than_zero_bounds_guard() {
        let mut m = Module::new();
        m.header = Some(ModuleHeader::new(40));
        m.types_global_values = vec![
            inst(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            inst(Op::TypeBool, None, Some(2), vec![]),
            inst(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
            inst(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::Input),
                    Operand::IdRef(1),
                ],
            ),
            inst(Op::ConstantNull, Some(1), Some(10), vec![]),
            inst(
                Op::Variable,
                Some(3),
                Some(20),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(10),
                ],
            ),
            inst(
                Op::Variable,
                Some(3),
                Some(21),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(10),
                ],
            ),
            inst(
                Op::Variable,
                Some(4),
                Some(22),
                vec![Operand::StorageClass(StorageClass::Input)],
            ),
        ];
        let entry = Block {
            label: Some(inst(Op::Label, None, Some(30), vec![])),
            instructions: vec![
                inst(Op::Load, Some(1), Some(31), vec![Operand::IdRef(20)]),
                inst(Op::Load, Some(1), Some(32), vec![Operand::IdRef(21)]),
                inst(
                    Op::IMul,
                    Some(1),
                    Some(33),
                    vec![Operand::IdRef(31), Operand::IdRef(32)],
                ),
                inst(Op::Load, Some(1), Some(36), vec![Operand::IdRef(22)]),
                inst(
                    Op::ULessThan,
                    Some(2),
                    Some(38),
                    vec![Operand::IdRef(36), Operand::IdRef(33)],
                ),
                inst(
                    Op::SelectionMerge,
                    None,
                    None,
                    vec![
                        Operand::IdRef(35),
                        Operand::SelectionControl(spirv::SelectionControl::NONE),
                    ],
                ),
                inst(
                    Op::BranchConditional,
                    None,
                    None,
                    vec![Operand::IdRef(38), Operand::IdRef(34), Operand::IdRef(35)],
                ),
            ],
        };
        let compute_b = Block {
            label: Some(inst(Op::Label, None, Some(34), vec![])),
            instructions: vec![inst(Op::Branch, None, None, vec![Operand::IdRef(35)])],
        };
        let skip_b = Block {
            label: Some(inst(Op::Label, None, Some(35), vec![])),
            instructions: vec![inst(Op::Return, None, None, vec![])],
        };
        let mut func = Function::new();
        func.blocks = vec![entry, compute_b, skip_b];
        m.functions = vec![func];

        assert!(
            prune_constant_branches(&mut m),
            "expected the guard to fold"
        );
        let labels: Vec<Word> = m.functions[0]
            .blocks
            .iter()
            .filter_map(|b| b.label.as_ref().and_then(|l| l.result_id))
            .collect();
        assert!(
            !labels.contains(&34),
            "dead compute arm (%34) should be pruned: {labels:?}"
        );
    }
}
