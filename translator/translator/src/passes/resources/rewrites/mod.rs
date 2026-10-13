use super::*;
use crate::passes::stage_input::{
    decorate_block_struct, is_backend_padding_array, layout_ty_size_align, round_up,
};

mod raw_word_rewrite;
pub(in crate::passes) use raw_word_rewrite::*;
mod private_atomics;
pub(in crate::passes) use private_atomics::*;
mod air_struct_remap;
pub(in crate::passes) use air_struct_remap::*;
mod structural_load;
pub(in crate::passes) use structural_load::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;
    use spirv::FunctionControl;

    fn defs_from_module(module: &Module) -> HashMap<Word, Instruction> {
        module
            .types_global_values
            .iter()
            .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
            .collect()
    }

    fn one_block_function(instructions: Vec<Instruction>) -> Function {
        Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(1),
                Some(90),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(91),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(92), vec![])),
                instructions,
            }],
        }
    }

    fn id_ref(operand: &Operand) -> Option<Word> {
        match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        }
    }

    #[test]
    fn runtime_array_word_index_respects_source_vector_alignment() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(3)]),
            Instruction::new(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        let defs = defs_from_module(&module);
        let value_types = crate::fxhash::map_from([(7, 2)]);

        let default_ctx = Ctx::new(module.clone());
        assert_eq!(
            runtime_array_word_index(&default_ctx, &defs, &value_types, 5, &[6, 7]),
            Some(7)
        );

        let mut custom_ctx = Ctx::new(module);
        custom_ctx.air_data_layout = Some(
            crate::layout::AirDataLayout::parse("e-v24:64:64")
                .expect("parse vector alignment override"),
        );
        assert_eq!(
            runtime_array_word_index(&custom_ctx, &defs, &value_types, 5, &[6, 7]),
            None,
            "an eight-byte vector allocation cannot be addressed as one raw word"
        );
    }

    #[test]
    fn workgroup_root_select_arm_rewrites_to_element_zero() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(2), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeBool, None, Some(5), vec![]),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(6),
                vec![Operand::LiteralBit32(512)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(7),
                vec![Operand::LiteralBit32(64)],
            ),
            Instruction::new(Op::ConstantTrue, Some(5), Some(8), vec![]),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(10),
                vec![Operand::IdRef(3), Operand::IdRef(6)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(10),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(11),
                Some(12),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(13),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(3),
                ],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(13),
                Some(20),
                vec![Operand::IdRef(12), Operand::IdRef(7)],
            ),
            Instruction::new(
                Op::Select,
                Some(13),
                Some(21),
                vec![Operand::IdRef(8), Operand::IdRef(20), Operand::IdRef(12)],
            ),
            Instruction::new(
                Op::PtrAccessChain,
                Some(13),
                Some(22),
                vec![Operand::IdRef(21), Operand::IdRef(7)],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_workgroup_root_access(&mut ctx, 0, 12, &defs);

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        let select = instructions
            .iter()
            .find(|inst| inst.result_id == Some(21))
            .expect("pointer select");
        assert_eq!(select.operands[1], Operand::IdRef(20));
        let root_leaf = id_ref(&select.operands[2]).expect("rewritten root arm");
        assert_ne!(root_leaf, 12);
        let decay = instructions
            .iter()
            .find(|inst| inst.result_id == Some(root_leaf))
            .expect("element-zero root chain");
        assert_eq!(decay.class.opcode, Op::InBoundsAccessChain);
        assert_eq!(decay.result_type, Some(13));
        assert_eq!(decay.operands.len(), 2);
        assert_eq!(decay.operands[0], Operand::IdRef(12));
    }

    #[test]
    fn raw_workgroup_root_wide_store_rewrites_to_byte_zero_pointer() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(5),
                vec![Operand::LiteralBit32(512)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(6),
                vec![Operand::IdRef(2), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(6),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(7),
                Some(8),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(9),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(8), Operand::IdRef(9)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_workgroup_root_access(&mut ctx, 0, 8, &defs);

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        let store = instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::Store)
            .expect("store");
        let pointer = id_ref(store.operands.first().expect("store pointer")).expect("pointer id");
        assert_ne!(pointer, 8);
        let decay = instructions
            .iter()
            .find(|instruction| instruction.result_id == Some(pointer))
            .expect("byte-zero pointer");
        assert_eq!(decay.class.opcode, Op::InBoundsAccessChain);
        assert_eq!(decay.operands[0], Operand::IdRef(8));
        assert_eq!(decay.operands.len(), 2);
    }

    #[test]
    fn flattened_workgroup_struct_store_rewrites_to_leaf_indices() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(3),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(4),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(5),
                vec![Operand::LiteralBit32(512)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(11),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(6),
                vec![
                    Operand::IdRef(2),
                    Operand::IdRef(2),
                    Operand::IdRef(2),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(6), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(7),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(8),
                Some(9),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(6),
                ],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::IAdd,
                Some(2),
                Some(20),
                vec![Operand::IdRef(3), Operand::IdRef(4)],
            ),
            Instruction::new(
                Op::IMul,
                Some(2),
                Some(21),
                vec![Operand::IdRef(20), Operand::IdRef(11)],
            ),
            Instruction::new(
                Op::IAdd,
                Some(2),
                Some(22),
                vec![Operand::IdRef(21), Operand::IdRef(4)],
            ),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(10),
                Some(23),
                vec![Operand::IdRef(9), Operand::IdRef(22)],
            ),
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(23), Operand::IdRef(20)],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_flattened_workgroup_leaf_accesses(&mut ctx, 0, &[9], &defs);

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        let access = instructions
            .iter()
            .find(|inst| inst.result_id == Some(23))
            .expect("access chain");
        assert_eq!(access.operands.len(), 3);
        assert_eq!(access.operands[0], Operand::IdRef(9));
        assert_eq!(access.operands[1], Operand::IdRef(20));
        let Operand::IdRef(member) = access.operands[2] else {
            panic!("struct member should be constant id");
        };
        assert_eq!(const_u32(&defs, member), Some(1));
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(2)
        );
        assert!(!instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::UMod));
    }

    #[test]
    fn flattened_workgroup_array_atomic_rewrites_to_leaf_indices() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(3),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(4),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(5),
                vec![Operand::LiteralBit32(2048)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(512)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(2), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(8),
                vec![Operand::IdRef(7), Operand::IdRef(6)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(8),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(9),
                Some(10),
                vec![Operand::StorageClass(StorageClass::Workgroup)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(7),
                ],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::IAdd,
                Some(2),
                Some(20),
                vec![Operand::IdRef(3), Operand::IdRef(4)],
            ),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(11),
                Some(21),
                vec![Operand::IdRef(10), Operand::IdRef(20)],
            ),
            Instruction::new(
                Op::AtomicAnd,
                Some(2),
                Some(22),
                vec![
                    Operand::IdRef(21),
                    Operand::IdScope(4),
                    Operand::IdMemorySemantics(3),
                    Operand::IdRef(20),
                ],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_flattened_workgroup_leaf_accesses(&mut ctx, 0, &[10], &defs);

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        let access = instructions
            .iter()
            .find(|inst| inst.result_id == Some(21))
            .expect("access chain");
        assert_eq!(access.operands.len(), 3);
        assert_eq!(access.operands[0], Operand::IdRef(10));
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(2)
        );
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::UDiv));
        assert!(instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::UMod));
    }

    #[test]
    fn collapsed_direct_access_prefers_offset_remap_over_unique_suffix() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::IdRef(1), Operand::IdRef(2)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(4),
                vec![Operand::IdRef(1), Operand::IdRef(2)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(5),
                vec![Operand::IdRef(1), Operand::IdRef(3), Operand::IdRef(4)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(7),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(8),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(9),
                vec![Operand::LiteralBit32(3)],
            ),
        ];

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_struct_offsets.insert(4, vec![0, 8]);
        ctx.air_struct_offsets.insert(5, vec![0, 8, 20]);
        let (indices, pointee) = remap_collapsed_direct_air_struct_access(
            &mut ctx,
            &defs,
            5,
            &[Operand::IdRef(9), Operand::IdRef(8)],
            Some(6),
        )
        .expect("offset-remapped nested member");

        assert_eq!(indices, vec![Operand::IdRef(8), Operand::IdRef(7)]);
        assert_eq!(pointee, 2);
    }

    #[test]
    fn collapsed_direct_access_recovers_unique_compact_root_from_suffix() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(3),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(4),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(5),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(6),
                vec![Operand::LiteralBit32(26)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(2), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(8),
                vec![Operand::IdRef(1), Operand::IdRef(7)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(9),
                vec![Operand::IdRef(1), Operand::IdRef(8)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
        ];

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        let suffix = [Operand::IdRef(4), Operand::IdRef(3)];
        let (indices, pointee) = remap_collapsed_direct_air_struct_access(
            &mut ctx,
            &defs,
            9,
            &[Operand::IdRef(6), suffix[0].clone(), suffix[1].clone()],
            Some(10),
        )
        .expect("unique compact root member");

        assert_eq!(
            indices,
            vec![Operand::IdRef(4), suffix[0].clone(), suffix[1].clone()]
        );
        assert_eq!(pointee, 2);
    }

    #[test]
    fn record_array_direct_member_uses_load_type_to_remap_padded_index() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(200));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(5),
                vec![Operand::IdRef(4), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(5), Operand::IdRef(6)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(8), vec![Operand::IdRef(7)]),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(9),
                vec![
                    Operand::IdRef(2),
                    Operand::IdRef(2),
                    Operand::IdRef(3),
                    Operand::IdRef(3),
                    Operand::IdRef(8),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypeRuntimeArray,
                None,
                Some(10),
                vec![Operand::IdRef(9)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(11), vec![Operand::IdRef(10)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(12),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(11),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(12),
                Some(13),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(14),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(9),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(15),
                vec![Operand::LiteralBit32(5)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(16),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(17), vec![Operand::IdRef(7)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(18),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(8),
                ],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(Op::Undef, Some(2), Some(62), vec![]),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(14),
                Some(60),
                vec![Operand::IdRef(50), Operand::IdRef(15)],
            ),
            Instruction::new(Op::Load, Some(17), Some(61), vec![Operand::IdRef(60)]),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(18),
                Some(63),
                vec![Operand::IdRef(50), Operand::IdRef(62), Operand::IdRef(15)],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_struct_offsets.insert(9, vec![0, 4, 8, 9, 16, 80]);
        rewrite_record_array_buffer(&mut ctx, 0, 50, 13, 11, 9, &defs);
        rewrite_structural_result_types(&mut ctx, 0, &defs);

        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(60))
            .expect("access chain");
        assert_eq!(access.operands.len(), 4);
        assert_eq!(access.operands[0], Operand::IdRef(13));
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[1]).unwrap()),
            Some(0)
        );
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[2]).unwrap()),
            Some(0)
        );
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[3]).unwrap()),
            Some(4)
        );
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(8)
        );
        let load = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(61))
            .expect("load");
        assert_eq!(load.result_type, Some(8));
        let indexed = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(63))
            .expect("record-indexed access chain");
        assert_eq!(indexed.operands[0], Operand::IdRef(13));
        assert_eq!(indexed.operands[2], Operand::IdRef(62));
        assert_eq!(
            const_u32(&defs, id_ref(&indexed.operands[3]).unwrap()),
            Some(4),
            "the source member after an elided padding field must use its compact ordinal"
        );
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, indexed.result_type.unwrap()),
            Some(8)
        );
    }

    #[test]
    fn record_array_member_path_falls_back_to_raw_index_across_natural_gaps() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(5),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(6),
                vec![Operand::IdRef(3), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(7),
                vec![Operand::IdRef(2), Operand::IdRef(6)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(8),
                vec![Operand::IdRef(4), Operand::IdRef(7)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(9), vec![Operand::IdRef(8)]),
            Instruction::new(Op::TypeStruct, None, Some(10), vec![Operand::IdRef(9)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(10),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(11),
                Some(12),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(13),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(14),
                vec![Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(15),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(13),
                Some(60),
                vec![
                    Operand::IdRef(50),
                    Operand::IdRef(14),
                    Operand::IdRef(14),
                    Operand::IdRef(15),
                ],
            ),
            Instruction::new(Op::Load, Some(3), Some(61), vec![Operand::IdRef(60)]),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_struct_offsets.insert(8, vec![0, 8]);
        ctx.air_struct_offsets.insert(7, vec![0, 8]);
        assert_eq!(remap_air_struct_member_index(&ctx, &defs, 8, 14), None);
        assert_eq!(remap_air_struct_member_index(&ctx, &defs, 7, 14), None);

        rewrite_record_array_buffer(&mut ctx, 0, 50, 12, 10, 8, &defs);

        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(60))
            .expect("access chain");
        assert_eq!(access.operands.len(), 6);
        assert_eq!(access.operands[0], Operand::IdRef(12));
        let vals: Vec<Option<u32>> = access.operands[1..]
            .iter()
            .map(|o| id_ref(o).and_then(|id| const_u32(&defs, id)))
            .collect();
        assert_eq!(vals, vec![Some(0), Some(0), Some(1), Some(1), Some(0)]);
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(3)
        );
    }

    #[test]
    fn record_array_flattens_source_dimensions_from_affine_layout() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(3), vec![Operand::IdRef(2)]),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(4),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(5),
                vec![Operand::IdRef(3), Operand::IdRef(4)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(6), vec![Operand::IdRef(5)]),
            Instruction::new(Op::TypeRuntimeArray, None, Some(7), vec![Operand::IdRef(6)]),
            Instruction::new(Op::TypeStruct, None, Some(8), vec![Operand::IdRef(7)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(9),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(8),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(9),
                Some(10),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(12),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(Op::Undef, Some(2), Some(20), vec![]),
            Instruction::new(Op::Undef, Some(2), Some(21), vec![]),
            Instruction::new(Op::Undef, Some(2), Some(22), vec![]),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(11),
                Some(60),
                vec![
                    Operand::IdRef(50),
                    Operand::IdRef(20),
                    Operand::IdRef(12),
                    Operand::IdRef(21),
                    Operand::IdRef(22),
                    Operand::IdRef(12),
                ],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar.buffer_access_affine_offsets.push(
            crate::emit_sidecar::BufferAccessAffineOffset {
                id: 60,
                root: 50,
                constant: 0,
                terms: vec![(20, 16), (21, 8), (22, 4)],
            },
        );
        rewrite_record_array_buffer(&mut ctx, 0, 50, 10, 8, 6, &defs);

        let body = &ctx.module.functions[0].blocks[0].instructions;
        let scale = body
            .iter()
            .find(|instruction| instruction.class.opcode == Op::IMul)
            .unwrap_or_else(|| {
                panic!("first source dimension scaled by its row stride: {body:#?}")
            });
        let sum = body
            .iter()
            .find(|instruction| instruction.class.opcode == Op::IAdd)
            .expect("flattened source dimensions summed");
        assert_eq!(sum.operands[0], Operand::IdRef(scale.result_id.unwrap()));
        assert_eq!(sum.operands[1], Operand::IdRef(22));
        let access = body
            .iter()
            .find(|instruction| instruction.result_id == Some(60))
            .expect("access chain");
        assert_eq!(
            access.operands,
            vec![
                Operand::IdRef(10),
                Operand::IdRef(12),
                Operand::IdRef(20),
                Operand::IdRef(12),
                Operand::IdRef(sum.result_id.unwrap()),
                Operand::IdRef(12),
            ]
        );
    }

    #[test]
    fn collapsed_buffer_direct_member_uses_load_type_to_remap_padded_index() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(200));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(5),
                vec![Operand::IdRef(4), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(6),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(5), Operand::IdRef(6)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(8), vec![Operand::IdRef(7)]),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(9),
                vec![
                    Operand::IdRef(2),
                    Operand::IdRef(2),
                    Operand::IdRef(3),
                    Operand::IdRef(3),
                    Operand::IdRef(8),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypeRuntimeArray,
                None,
                Some(10),
                vec![Operand::IdRef(9)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(11), vec![Operand::IdRef(10)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(12),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(11),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(12),
                Some(13),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(14),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(9),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(15),
                vec![Operand::LiteralBit32(5)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(16),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(17), vec![Operand::IdRef(7)]),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(14),
                Some(60),
                vec![Operand::IdRef(50), Operand::IdRef(15)],
            ),
            Instruction::new(Op::Load, Some(17), Some(61), vec![Operand::IdRef(60)]),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_struct_offsets.insert(9, vec![0, 4, 8, 9, 16, 80]);
        rewrite_collapsed_buffer(&mut ctx, 0, 50, 13, 11, true, &[], &defs);
        rewrite_structural_result_types(&mut ctx, 0, &defs);

        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(60))
            .expect("access chain");
        assert_eq!(access.operands.len(), 4);
        assert_eq!(access.operands[0], Operand::IdRef(13));
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[1]).unwrap()),
            Some(0)
        );
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[2]).unwrap()),
            Some(0)
        );
        assert_eq!(
            const_u32(&defs, id_ref(&access.operands[3]).unwrap()),
            Some(4)
        );
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(8)
        );
        let load = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(61))
            .expect("load");
        assert_eq!(load.result_type, Some(8));
    }

    #[test]
    fn collapsed_scalar_buffer_replays_exact_source_byte_offset() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(3),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(3)]),
            Instruction::new(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(9),
                vec![Operand::LiteralBit32(1)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::InBoundsAccessChain,
                Some(8),
                Some(20),
                vec![Operand::IdRef(50), Operand::IdRef(9), Operand::IdRef(9)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar
            .buffer_access_offsets
            .push(crate::emit_sidecar::BufferAccessOffset {
                id: 20,
                root: 50,
                byte_offset: 16,
            });
        rewrite_collapsed_buffer(&mut ctx, 0, 50, 7, 5, true, &[], &defs);

        let types = combined_type_defs(&ctx, &defs);
        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(20))
            .expect("access chain");
        assert_eq!(access.operands.len(), 3);
        assert_eq!(access.operands[0], Operand::IdRef(7));
        assert_eq!(
            const_u32(&types, id_ref(&access.operands[1]).unwrap()),
            Some(0)
        );
        assert_eq!(
            const_u32(&types, id_ref(&access.operands[2]).unwrap()),
            Some(4)
        );
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(3)
        );
    }

    #[test]
    fn collapsed_struct_buffer_replays_flat_word_offset_as_nested_member_path() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::IdRef(1)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::IdRef(1), Operand::IdRef(1)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(4),
                vec![Operand::IdRef(2), Operand::IdRef(3), Operand::IdRef(1)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(4),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(5),
                Some(6),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(7),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(8),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(9),
                vec![Operand::LiteralBit32(3)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::InBoundsAccessChain,
                Some(7),
                Some(20),
                vec![Operand::IdRef(6), Operand::IdRef(8), Operand::IdRef(9)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar
            .buffer_access_offsets
            .push(crate::emit_sidecar::BufferAccessOffset {
                id: 20,
                root: 6,
                byte_offset: 12,
            });
        assert_eq!(
            body_buf_flat_scalar_element_type(&ctx, &ctx.module.functions[0], 6),
            Some(1)
        );
        rewrite_collapsed_buffer(&mut ctx, 0, 50, 6, 4, false, &[], &defs);

        let types = combined_type_defs(&ctx, &defs);
        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|instruction| instruction.result_id == Some(20))
            .expect("access chain");
        assert_eq!(access.operands[0], Operand::IdRef(6));
        assert_eq!(access.operands.len(), 3);
        assert_eq!(
            const_u32(&types, id_ref(&access.operands[1]).unwrap()),
            Some(1)
        );
        assert_eq!(
            const_u32(&types, id_ref(&access.operands[2]).unwrap()),
            Some(1)
        );
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(1)
        );
    }

    #[test]
    fn collapsed_buffer_routes_each_scalar_view_to_its_typed_descriptor_alias() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(16)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(2)]),
            Instruction::new(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(8), vec![Operand::IdRef(3)]),
            Instruction::new(Op::TypeStruct, None, Some(9), vec![Operand::IdRef(8)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(10),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(9),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(10),
                Some(11),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(12),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(13),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(14),
                vec![Operand::LiteralBit32(3)],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(12),
                Some(20),
                vec![Operand::IdRef(50), Operand::IdRef(14)],
            ),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(13),
                Some(21),
                vec![Operand::IdRef(50), Operand::IdRef(14)],
            ),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_collapsed_buffer(&mut ctx, 0, 50, 7, 5, true, &[(3, 11)], &defs);

        let body = &ctx.module.functions[0].blocks[0].instructions;
        let half_access = body.iter().find(|inst| inst.result_id == Some(20)).unwrap();
        let bfloat_access = body.iter().find(|inst| inst.result_id == Some(21)).unwrap();
        assert_eq!(half_access.operands[0], Operand::IdRef(7));
        assert_eq!(bfloat_access.operands[0], Operand::IdRef(11));
        assert_eq!(half_access.operands.len(), 3);
        assert_eq!(bfloat_access.operands.len(), 3);
        assert_eq!(half_access.result_type, Some(12));
        assert_eq!(bfloat_access.result_type, Some(13));
    }

    #[test]
    fn collapsed_buffer_does_not_confuse_element_zero_with_wrapper_member_zero() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(2), Operand::LiteralBit32(4)],
            ),
            Instruction::new(Op::TypeRuntimeArray, None, Some(4), vec![Operand::IdRef(3)]),
            Instruction::new(Op::TypeStruct, None, Some(5), vec![Operand::IdRef(4)]),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(6),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(8),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(9),
                vec![Operand::LiteralBit32(0)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::InBoundsAccessChain,
                Some(8),
                Some(20),
                vec![Operand::IdRef(50), Operand::IdRef(9)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_collapsed_buffer(&mut ctx, 0, 50, 7, 5, true, &[], &defs);

        let all_defs = combined_type_defs(&ctx, &defs);
        let access = ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .find(|inst| inst.result_id == Some(20))
            .unwrap();
        assert_eq!(access.operands[0], Operand::IdRef(7));
        assert_eq!(access.operands.len(), 3);
        assert!(matches!(
            access.operands.get(1),
            Some(Operand::IdRef(id)) if const_u32(&all_defs, *id) == Some(0)
        ));
        assert_eq!(access.operands[2], Operand::IdRef(9));
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, access.result_type.unwrap()),
            Some(3)
        );
    }

    #[test]
    fn rooted_vector_stride_scales_when_pointer_rewrites_to_scalar_element() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(200));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(16), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(4),
                vec![Operand::IdRef(2), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(4),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(5),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(8),
                vec![Operand::LiteralBit64(9)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::PtrAccessChain,
                Some(6),
                Some(60),
                vec![Operand::IdRef(7), Operand::IdRef(8)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_pointer_storage(&mut ctx, 0, &[7], StorageClass::StorageBuffer, &defs)
            .expect("rewrite pointer storage");

        let body = &ctx.module.functions[0].blocks[0].instructions;
        let chain_pos = body
            .iter()
            .position(|inst| inst.result_id == Some(60))
            .expect("PtrAccessChain");
        let chain = &body[chain_pos];
        assert_eq!(chain.result_type, Some(5), "result becomes scalar pointer");
        let scaled = id_ref(&chain.operands[1]).expect("scaled element index");
        let scale = &body[chain_pos - 1];
        assert_eq!(scale.class.opcode, Op::IMul);
        assert_eq!(scale.result_type, Some(3), "keep the i64 index width");
        assert_eq!(scale.result_id, Some(scaled));
        assert_eq!(scale.operands[0], Operand::IdRef(8));
        let factor = id_ref(&scale.operands[1]).expect("vector lane factor");
        let factor_def = ctx
            .new_globals
            .iter()
            .find(|inst| inst.result_id == Some(factor))
            .expect("factor constant");
        assert_eq!(factor_def.result_type, Some(3));
        assert_eq!(factor_def.operands, vec![Operand::LiteralBit64(4)]);
    }

    #[test]
    fn a_three_lane_vector_strides_by_its_padded_size() {
        assert_eq!(vector_chain_stride_factor(3, 32, 32), Some(4));
    }

    #[test]
    fn a_vector_chain_retyped_to_bytes_strides_by_its_whole_size() {
        assert_eq!(vector_chain_stride_factor(3, 32, 8), Some(16));
        assert_eq!(vector_chain_stride_factor(2, 32, 8), Some(8));
        assert_eq!(vector_chain_stride_factor(4, 32, 8), Some(16));
    }

    #[test]
    fn an_unnarrowed_vector_chain_is_not_rescaled() {
        assert_eq!(vector_chain_stride_factor(4, 32, 128), None);
    }

    fn vector_chain_stride_factor(lanes: u32, elem_bits: u32, root_bits: u32) -> Option<u64> {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(200));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(root_bits), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(9),
                vec![Operand::LiteralBit32(elem_bits), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(4),
                vec![Operand::IdRef(9), Operand::LiteralBit32(lanes)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(4),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(5),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(8),
                vec![Operand::LiteralBit64(9)],
            ),
        ];
        module
            .functions
            .push(one_block_function(vec![Instruction::new(
                Op::PtrAccessChain,
                Some(6),
                Some(60),
                vec![Operand::IdRef(7), Operand::IdRef(8)],
            )]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_pointer_storage(&mut ctx, 0, &[7], StorageClass::StorageBuffer, &defs)
            .expect("rewrite pointer storage");

        let body = &ctx.module.functions[0].blocks[0].instructions;
        let chain = body
            .iter()
            .find(|inst| inst.result_id == Some(60))
            .expect("PtrAccessChain");
        let element = id_ref(&chain.operands[1]).expect("element operand");
        if element == 8 {
            return None;
        }
        let scale = body
            .iter()
            .find(|inst| inst.result_id == Some(element))
            .expect("scale instruction");
        assert_eq!(scale.class.opcode, Op::IMul);
        assert_eq!(scale.operands[0], Operand::IdRef(8));
        let factor = id_ref(&scale.operands[1]).expect("stride factor");
        let factor_def = ctx
            .new_globals
            .iter()
            .find(|inst| inst.result_id == Some(factor))
            .expect("factor constant");
        match factor_def.operands.first() {
            Some(Operand::LiteralBit64(value)) => Some(*value),
            Some(Operand::LiteralBit32(value)) => Some(u64::from(*value)),
            other => panic!("unexpected factor literal {other:?}"),
        }
    }

    #[test]
    fn rooted_vector_load_constructs_vector_stride_pointer() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(2), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(3),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(6),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(5),
                Some(7),
                vec![Operand::StorageClass(StorageClass::StorageBuffer)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(8),
                vec![Operand::LiteralBit32(2)],
            ),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(6),
                Some(20),
                vec![Operand::IdRef(7), Operand::IdRef(8)],
            ),
            Instruction::new(Op::Load, Some(3), Some(21), vec![Operand::IdRef(20)]),
        ]));

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        rewrite_pointer_storage(&mut ctx, 0, &[7], StorageClass::StorageBuffer, &defs)
            .expect("rewrite pointer storage");

        let chain = &ctx.module.functions[0].blocks[0].instructions[0];
        assert_eq!(chain.class.opcode, Op::PtrAccessChain);
        assert_eq!(
            pointer_pointee_including_new(&ctx, &defs, chain.result_type.unwrap()),
            Some(3)
        );
        assert_eq!(chain.operands, [Operand::IdRef(7), Operand::IdRef(8)]);
    }

    #[test]
    fn nested_air_struct_ordinal_remaps_after_padding_elided_bridge() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(200));
        module.types_global_values = vec![
            Instruction::new(Op::TypeVoid, None, Some(1), vec![]),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(3),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(5),
                vec![Operand::IdRef(2), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(6),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(7),
                vec![Operand::IdRef(3), Operand::IdRef(6)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(8),
                vec![Operand::IdRef(2), Operand::IdRef(7), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(9),
                vec![Operand::IdRef(2), Operand::IdRef(5)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(10),
                vec![Operand::IdRef(9), Operand::IdRef(9)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(11),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(9),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(12),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(5),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(13),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(14),
                vec![Operand::LiteralBit32(1)],
            ),
        ];
        let mut function = one_block_function(vec![Instruction::new(
            Op::InBoundsAccessChain,
            Some(12),
            Some(60),
            vec![Operand::IdRef(50), Operand::IdRef(13)],
        )]);
        function.parameters.push(Instruction::new(
            Op::FunctionParameter,
            Some(11),
            Some(50),
            vec![],
        ));
        module.functions.push(function);

        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_struct_offsets.insert(9, vec![0, 8]);
        assert!(types_match_with_elided_air_padding(&ctx, &defs, 9, 8));
        let (direct_indices, direct_pointee) = remap_direct_air_struct_access_to_pointee(
            &mut ctx,
            &defs,
            10,
            &[Operand::IdRef(14)],
            8,
        )
        .expect("compact top member matches padded AIR result");
        assert_eq!(direct_indices, vec![Operand::IdRef(14)]);
        assert_eq!(direct_pointee, 9);

        remap_nested_air_struct_accesses(&mut ctx, 0, &crate::fxhash::set_from([50]), &defs);
        let chain = &ctx.module.functions[0].blocks[0].instructions[0];
        assert_eq!(chain.operands[1], Operand::IdRef(14));
        assert_eq!(chain.result_type, Some(12));
    }

    #[test]
    fn raw_word_access_and_air_ordinal_remap_share_vector_allocation_stride() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(20));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(3),
                vec![Operand::IdRef(2), Operand::IdRef(1)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(4),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(4),
                Some(5),
                vec![Operand::LiteralBit32(1)],
            ),
        ];
        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);
        ctx.air_data_layout = Some(
            crate::layout::AirDataLayout::parse("e-v24:64:64")
                .expect("parse vector alignment override"),
        );

        assert_eq!(access_path_byte_offset(&ctx, &defs, 3, &[5]), Some(8));

        ctx.air_struct_offsets.insert(3, vec![0, 8]);
        assert_eq!(remap_air_struct_member_index(&ctx, &defs, 3, 5), Some(1));
    }

    #[test]
    fn structural_extract_follows_the_bound_composite_alias() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(30));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(2),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(2),
                Some(3),
                vec![Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(4),
                vec![Operand::IdRef(2), Operand::IdRef(3)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(5),
                vec![Operand::IdRef(2), Operand::IdRef(3)],
            ),
            Instruction::new(Op::TypeStruct, None, Some(6), vec![Operand::IdRef(4)]),
        ];
        module.functions.push(one_block_function(vec![
            Instruction::new(Op::Undef, Some(6), Some(10), vec![]),
            Instruction::new(
                Op::CompositeExtract,
                Some(5),
                Some(11),
                vec![Operand::IdRef(10), Operand::LiteralBit32(0)],
            ),
        ]));
        let defs = defs_from_module(&module);
        let mut ctx = Ctx::new(module);

        rewrite_structural_result_types(&mut ctx, 0, &defs);

        let extract = &ctx.module.functions[0].blocks[0].instructions[1];
        assert_eq!(extract.result_type, Some(4));
    }
}
