use super::*;
use crate::spirv_module::ModuleHeader;
use spirv::{FunctionControl, LoopControl};

fn mkfunc(id: Word, ret: Word, label: Word, insts: Vec<Instruction>) -> Function {
    Function {
        def: Some(Instruction::new(
            Op::Function,
            Some(ret),
            Some(id),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(3),
            ],
        )),
        end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
        parameters: vec![],
        blocks: vec![Block {
            label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
            instructions: insts,
        }],
    }
}

fn module_with(funcs: Vec<Function>) -> Module {
    let mut m = Module::new();
    m.header = Some(ModuleHeader::new(1000));
    m.functions = funcs;
    m
}

fn call_count(m: &Module, fi: usize) -> usize {
    m.functions[fi]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .filter(|i| i.class.opcode == Op::FunctionCall)
        .count()
}

#[test]
fn inlines_single_block_helper_and_drops_it() {
    let helper = mkfunc(
        20,
        2,
        21,
        vec![Instruction::new(
            Op::ReturnValue,
            None,
            None,
            vec![Operand::IdRef(5)],
        )],
    );
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(12),
                vec![Operand::IdRef(20)],
            ),
            Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(12)]),
        ],
    );
    let mut ctx = Ctx::new(module_with(vec![entry, helper]));
    let stats = inline_helpers(&mut ctx, 0).expect("inline ok");
    assert_eq!(
        stats,
        InlineStats {
            splices: 1,
            helper_instances: 1,
        }
    );
    assert_eq!(ctx.module.functions.len(), 1, "helper inlined + dropped");
    assert_eq!(call_count(&ctx.module, 0), 0, "call removed");
    let returns_5 = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|i| {
            i.class.opcode == Op::ReturnValue && i.operands.first() == Some(&Operand::IdRef(5))
        });
    assert!(returns_5, "call result rewired to the helper return value");
}

#[test]
fn inliner_selects_the_body_when_a_linked_declaration_precedes_it() {
    let helper = mkfunc(
        20,
        2,
        21,
        vec![Instruction::new(
            Op::ReturnValue,
            None,
            None,
            vec![Operand::IdRef(5)],
        )],
    );
    let mut declaration = helper.clone();
    declaration.blocks.clear();
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(12),
                vec![Operand::IdRef(20)],
            ),
            Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(12)]),
        ],
    );
    let mut ctx = Ctx::new(module_with(vec![entry, declaration, helper]));

    let stats = inline_helpers(&mut ctx, 0).expect("inline bodied linked definition");

    assert_eq!(stats.splices, 1);
    assert_eq!(call_count(&ctx.module, 0), 0);
}

#[test]
fn inliner_stats_count_unique_helpers_separately_from_splices() {
    let helper = mkfunc(
        20,
        2,
        21,
        vec![Instruction::new(
            Op::ReturnValue,
            None,
            None,
            vec![Operand::IdRef(5)],
        )],
    );
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(12),
                vec![Operand::IdRef(20)],
            ),
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(13),
                vec![Operand::IdRef(20)],
            ),
            Instruction::new(Op::ReturnValue, None, None, vec![Operand::IdRef(13)]),
        ],
    );
    let mut ctx = Ctx::new(module_with(vec![entry, helper]));

    let stats = inline_helpers(&mut ctx, 0).expect("inline ok");

    assert_eq!(
        stats,
        InlineStats {
            splices: 2,
            helper_instances: 1,
        }
    );
}

#[test]
fn selected_multiblock_splice_preserves_chained_call_order_without_pruning() {
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
            Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(30)]),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    let mut first = mkfunc(
        20,
        2,
        21,
        vec![
            Instruction::new(
                Op::IAdd,
                Some(5),
                Some(24),
                vec![Operand::IdRef(6), Operand::IdRef(7)],
            ),
            Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(22)]),
        ],
    );
    first.blocks.push(Block {
        label: Some(Instruction::new(Op::Label, None, Some(22), vec![])),
        instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
    });
    let mut second = mkfunc(
        30,
        2,
        31,
        vec![
            Instruction::new(
                Op::IMul,
                Some(5),
                Some(34),
                vec![Operand::IdRef(6), Operand::IdRef(7)],
            ),
            Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(32)]),
        ],
    );
    second.blocks.push(Block {
        label: Some(Instruction::new(Op::Label, None, Some(32), vec![])),
        instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
    });
    let mut ctx = Ctx::new(module_with(vec![entry, first, second]));

    let stats = inline_selected_helpers(&mut ctx, 0, &crate::fxhash::set_from([20, 30]))
        .expect("selected emitted splice");

    assert_eq!(
        stats,
        InlineStats {
            splices: 2,
            helper_instances: 2,
        }
    );
    assert_eq!(
        ctx.module.functions.len(),
        3,
        "producer-side selection defers dead-function pruning to the residual closure"
    );
    assert_eq!(call_count(&ctx.module, 0), 0);
    let opcode_block = |opcode| {
        ctx.module.functions[0]
            .blocks
            .iter()
            .position(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.class.opcode == opcode)
            })
            .expect("marker opcode")
    };
    assert!(
        opcode_block(Op::IAdd) < opcode_block(Op::IMul),
        "the first constructor/helper CFG must complete before the second starts"
    );
}

#[test]
fn inlined_helper_retargets_pointer_results_to_call_argument_storage() {
    let void = 100;
    let float = 101;
    let v4float = 102;
    let vertex_struct = 103;
    let uint = 104;
    let uint_0 = 105;
    let ptr_function_struct = 106;
    let ptr_private_struct = 107;
    let ptr_private_v4float = 108;
    let wrapper_array = 109;
    let ptr_function_wrapper = 110;
    let uint_1 = 111;
    let helper_param = 25;
    let local_struct = 30;
    let access_result = 31;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(
                Op::Variable,
                Some(ptr_function_wrapper),
                Some(local_struct),
                vec![Operand::StorageClass(StorageClass::Function)],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![Operand::IdRef(20), Operand::IdRef(local_struct)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.clear();

    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(ptr_private_v4float),
                Some(access_result),
                vec![Operand::IdRef(helper_param), Operand::IdRef(uint_0)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.push(Instruction::new(
        Op::FunctionParameter,
        Some(ptr_private_struct),
        Some(helper_param),
        vec![],
    ));

    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(Op::TypeFloat, float, vec![Operand::LiteralBit32(32)]),
        type_inst(
            Op::TypeVector,
            v4float,
            vec![Operand::IdRef(float), Operand::LiteralBit32(4)],
        ),
        type_inst(
            Op::TypeArray,
            vertex_struct,
            vec![Operand::IdRef(v4float), Operand::IdRef(uint_1)],
        ),
        type_inst(
            Op::TypeInt,
            uint,
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(uint_0),
            vec![Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(uint_1),
            vec![Operand::LiteralBit32(1)],
        ),
        type_inst(
            Op::TypeArray,
            wrapper_array,
            vec![Operand::IdRef(vertex_struct), Operand::IdRef(uint_1)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_function_struct,
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(vertex_struct),
            ],
        ),
        type_inst(
            Op::TypePointer,
            ptr_private_struct,
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(vertex_struct),
            ],
        ),
        type_inst(
            Op::TypePointer,
            ptr_private_v4float,
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(v4float),
            ],
        ),
        type_inst(
            Op::TypePointer,
            ptr_function_wrapper,
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(wrapper_array),
            ],
        ),
    ];

    let mut ctx = Ctx::new(module);
    inline_helpers(&mut ctx, 0).expect("inline ok");
    assert_eq!(ctx.module.functions.len(), 1, "helper should be dropped");

    let access = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .find(|i| i.class.opcode == Op::InBoundsAccessChain)
        .expect("inlined access chain");
    assert_ne!(
        access.result_id,
        Some(access_result),
        "helper result ids should still be freshened while retargeting the pointer type"
    );
    assert_eq!(
        access.operands.first(),
        Some(&Operand::IdRef(local_struct)),
        "helper param should be remapped to the caller's Function alloca"
    );
    assert_eq!(
        access.operands,
        vec![
            Operand::IdRef(local_struct),
            Operand::IdRef(uint_0),
            Operand::IdRef(uint_0),
        ],
        "the inline splice must complete zero-offset wrapper descent while constructing the chain"
    );
    let defs = type_defs_with_new_globals(&ctx);
    let result_type = access.result_type.expect("access result type");
    assert_eq!(
        ptr_storage(&defs, result_type),
        Some(StorageClass::Function),
        "access-chain result storage must follow the remapped base pointer"
    );
}

#[test]
fn inlined_helper_extracts_inserted_pointer_from_aggregate_carrier() {
    let void = 100;
    let uint = 101;
    let ptr_buffer_uint = 102;
    let inner = 103;
    let outer = 104;
    let buffer = 12;
    let undef = 30;
    let aggregate = 31;
    let helper_param = 25;
    let extracted = 26;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(Op::Undef, Some(outer), Some(undef), vec![]),
            Instruction::new(
                Op::CompositeInsert,
                Some(outer),
                Some(aggregate),
                vec![
                    Operand::IdRef(buffer),
                    Operand::IdRef(undef),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![Operand::IdRef(20), Operand::IdRef(aggregate)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.push(Instruction::new(
        Op::FunctionParameter,
        Some(ptr_buffer_uint),
        Some(buffer),
        vec![],
    ));
    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::CompositeExtract,
                Some(ptr_buffer_uint),
                Some(extracted),
                vec![
                    Operand::IdRef(helper_param),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::Load,
                Some(uint),
                Some(27),
                vec![Operand::IdRef(extracted)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.push(Instruction::new(
        Op::FunctionParameter,
        Some(outer),
        Some(helper_param),
        vec![],
    ));
    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(
            Op::TypeInt,
            uint,
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_buffer_uint,
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(uint),
            ],
        ),
        type_inst(Op::TypeStruct, inner, vec![Operand::IdRef(ptr_buffer_uint)]),
        type_inst(
            Op::TypeStruct,
            outer,
            vec![Operand::IdRef(uint), Operand::IdRef(inner)],
        ),
    ];

    let mut ctx = Ctx::new(module);
    inline_helpers(&mut ctx, 0).expect("inline aggregate pointer carrier");

    let instructions = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(
        instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::CompositeExtract),
        "the helper extract is replaced by the inserted pointer identity"
    );
    let load = instructions
        .iter()
        .find(|instruction| instruction.class.opcode == Op::Load)
        .expect("inlined load");
    assert_eq!(load.operands.first(), Some(&Operand::IdRef(buffer)));
}

#[test]
fn inlined_helper_extracts_inserted_device_address_payload_from_aggregate_carrier() {
    let void = 100;
    let ulong = 101;
    let ptr_function_ulong = 102;
    let inner = 103;
    let outer = 104;
    let address = 12;
    let output = 13;
    let undef = 30;
    let aggregate = 31;
    let helper_param = 25;
    let helper_output = 26;
    let extracted = 27;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(Op::Undef, Some(outer), Some(undef), vec![]),
            Instruction::new(
                Op::CompositeInsert,
                Some(outer),
                Some(aggregate),
                vec![
                    Operand::IdRef(address),
                    Operand::IdRef(undef),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::IdRef(aggregate),
                    Operand::IdRef(output),
                ],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.extend([
        Instruction::new(Op::FunctionParameter, Some(ulong), Some(address), vec![]),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_function_ulong),
            Some(output),
            vec![],
        ),
    ]);
    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::CompositeExtract,
                Some(ulong),
                Some(extracted),
                vec![
                    Operand::IdRef(helper_param),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(helper_output), Operand::IdRef(extracted)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.extend([
        Instruction::new(
            Op::FunctionParameter,
            Some(outer),
            Some(helper_param),
            vec![],
        ),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_function_ulong),
            Some(helper_output),
            vec![],
        ),
    ]);
    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(
            Op::TypeInt,
            ulong,
            vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_function_ulong,
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(ulong),
            ],
        ),
        type_inst(Op::TypeStruct, inner, vec![Operand::IdRef(ulong)]),
        type_inst(
            Op::TypeStruct,
            outer,
            vec![Operand::IdRef(ulong), Operand::IdRef(inner)],
        ),
    ];

    let mut ctx = Ctx::new(module);
    inline_helpers(&mut ctx, 0).expect("inline aggregate device-address carrier");

    let instructions = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(
        instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::CompositeExtract),
        "the helper extract is replaced by the exact inserted payload"
    );
    let store = instructions
        .iter()
        .find(|instruction| instruction.class.opcode == Op::Store)
        .expect("inlined store");
    assert_eq!(
        store.operands,
        vec![Operand::IdRef(output), Operand::IdRef(address)]
    );
}

#[test]
fn inlined_helper_elides_type_mismatched_pointer_extract_by_exact_identity() {
    let void = 100;
    let ulong = 101;
    let ptr_buffer_ulong = 102;
    let inner = 103;
    let outer = 104;
    let pointer = 12;
    let output = 13;
    let undef = 30;
    let aggregate = 31;
    let helper_param = 25;
    let helper_output = 26;
    let extracted = 27;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(Op::Undef, Some(outer), Some(undef), vec![]),
            Instruction::new(
                Op::CompositeInsert,
                Some(outer),
                Some(aggregate),
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdRef(undef),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::IdRef(aggregate),
                    Operand::IdRef(output),
                ],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.extend([
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(pointer),
            vec![],
        ),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(output),
            vec![],
        ),
    ]);
    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::CompositeExtract,
                Some(ptr_buffer_ulong),
                Some(extracted),
                vec![
                    Operand::IdRef(helper_param),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(helper_output), Operand::IdRef(extracted)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.extend([
        Instruction::new(
            Op::FunctionParameter,
            Some(outer),
            Some(helper_param),
            vec![],
        ),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(helper_output),
            vec![],
        ),
    ]);
    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(
            Op::TypeInt,
            ulong,
            vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_buffer_ulong,
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(ulong),
            ],
        ),
        type_inst(Op::TypeStruct, inner, vec![Operand::IdRef(ulong)]),
        type_inst(
            Op::TypeStruct,
            outer,
            vec![Operand::IdRef(ulong), Operand::IdRef(inner)],
        ),
    ];

    let mut ctx = Ctx::new(module);
    inline_helpers(&mut ctx, 0).expect("inline exact pointer aggregate carrier");

    let instructions = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(
        instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::CompositeExtract),
        "the exact pointer identity does not depend on the aggregate's temporary payload type"
    );
}

#[test]
fn inlined_helper_preserves_type_mismatched_pointer_payload_identity() {
    let void = 100;
    let ulong = 101;
    let ptr_buffer_ulong = 102;
    let inner = 103;
    let outer = 104;
    let pointer = 12;
    let output = 13;
    let undef = 30;
    let aggregate = 31;
    let helper_param = 25;
    let helper_output = 26;
    let extracted = 27;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(Op::Undef, Some(outer), Some(undef), vec![]),
            Instruction::new(
                Op::CompositeInsert,
                Some(outer),
                Some(aggregate),
                vec![
                    Operand::IdRef(pointer),
                    Operand::IdRef(undef),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::IdRef(aggregate),
                    Operand::IdRef(output),
                ],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.extend([
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(pointer),
            vec![],
        ),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(output),
            vec![],
        ),
    ]);
    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::CompositeExtract,
                Some(ptr_buffer_ulong),
                Some(extracted),
                vec![
                    Operand::IdRef(helper_param),
                    Operand::LiteralBit32(1),
                    Operand::LiteralBit32(0),
                ],
            ),
            Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(helper_output), Operand::IdRef(extracted)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.extend([
        Instruction::new(
            Op::FunctionParameter,
            Some(outer),
            Some(helper_param),
            vec![],
        ),
        Instruction::new(
            Op::FunctionParameter,
            Some(ptr_buffer_ulong),
            Some(helper_output),
            vec![],
        ),
    ]);
    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(
            Op::TypeInt,
            ulong,
            vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_buffer_ulong,
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(ulong),
            ],
        ),
        type_inst(Op::TypeStruct, inner, vec![Operand::IdRef(ulong)]),
        type_inst(
            Op::TypeStruct,
            outer,
            vec![Operand::IdRef(ulong), Operand::IdRef(inner)],
        ),
    ];

    let mut ctx = Ctx::new(module);
    inline_helpers(&mut ctx, 0).expect("inline type-mismatched pointer aggregate carrier");

    let instructions = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .collect::<Vec<_>>();
    assert!(
        instructions
            .iter()
            .all(|instruction| instruction.class.opcode != Op::CompositeExtract),
        "the exact pointer extract is replaced by the inserted pointer identity"
    );
    let store = instructions
        .iter()
        .find(|instruction| instruction.class.opcode == Op::Store)
        .expect("inlined store");
    assert_eq!(
        store.operands,
        vec![Operand::IdRef(output), Operand::IdRef(pointer)]
    );
}

#[test]
fn leaves_air_intrinsic_calls_for_the_lowering_pass() {
    let helper = mkfunc(
        20,
        2,
        21,
        vec![Instruction::new(
            Op::ReturnValue,
            None,
            None,
            vec![Operand::IdRef(5)],
        )],
    );
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(12),
                vec![Operand::IdRef(20)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    let mut m = module_with(vec![entry, helper]);
    m.debug_names.push(Instruction::new(
        Op::Name,
        None,
        None,
        vec![Operand::IdRef(20), Operand::LiteralString("air.foo".into())],
    ));
    let mut ctx = Ctx::new(m);
    inline_helpers(&mut ctx, 0).expect("ok");
    assert_eq!(call_count(&ctx.module, 0), 1, "air.* call left intact");
}

#[test]
fn inlining_remaps_local_pointer_field_store_source() {
    let void = 100;
    let float = 101;
    let v4float = 102;
    let sentinel = 35;
    let object = 30;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.clear();
    let helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(Op::Undef, Some(v4float), Some(object), vec![]),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    let mut m = module_with(vec![entry, helper]);
    m.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(Op::TypeFloat, float, vec![Operand::LiteralBit32(32)]),
        type_inst(
            Op::TypeVector,
            v4float,
            vec![Operand::IdRef(float), Operand::LiteralBit32(4)],
        ),
        Instruction::new(Op::ConstantNull, Some(v4float), Some(sentinel), vec![]),
    ];
    let mut ctx = Ctx::new(m);
    ctx.emit_sidecar
        .local_pointer_field_stores
        .push(crate::emit_sidecar::LocalPointerFieldStore {
            id: sentinel,
            source: object,
            root: object,
            indices: vec![0],
        });
    inline_helpers(&mut ctx, 0).expect("inline ok");

    let undef_id = ctx.module.functions[0]
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .find(|i| i.class.opcode == Op::Undef)
        .and_then(|i| i.result_id)
        .expect("inlined undef");
    assert_ne!(
        undef_id, object,
        "the stored object id should be freshened by inlining"
    );
    let source = ctx
        .emit_sidecar
        .local_pointer_field_stores
        .iter()
        .find(|fact| fact.id == sentinel && fact.root == undef_id)
        .map(|fact| fact.source)
        .expect("store fact present");
    assert!(ctx
        .emit_sidecar
        .local_pointer_field_stores
        .iter()
        .any(|fact| fact.id == sentinel && fact.root == object && fact.source == object));
    assert_eq!(
        source, undef_id,
        "the store fact's source id must be remapped to the freshened object"
    );
}

#[test]
fn multiblock_inline_keeps_loop_merge_on_split_header() {
    let void = 100;
    let entry_id = 10;
    let helper_id = 20;
    let entry_label = 11;
    let helper_entry_label = 21;
    let helper_return_label = 22;
    let loop_merge_label = 30;
    let loop_continue_label = 31;
    let loop_body_label = 32;
    let cond = 40;

    let entry = mkfunc(
        entry_id,
        void,
        entry_label,
        vec![
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![Operand::IdRef(helper_id)],
            ),
            Instruction::new(
                Op::LoopMerge,
                None,
                None,
                vec![
                    Operand::IdRef(loop_merge_label),
                    Operand::IdRef(loop_continue_label),
                    Operand::LoopControl(LoopControl::NONE),
                ],
            ),
            Instruction::new(
                Op::BranchConditional,
                None,
                None,
                vec![
                    Operand::IdRef(cond),
                    Operand::IdRef(loop_body_label),
                    Operand::IdRef(loop_continue_label),
                ],
            ),
        ],
    );
    let helper = Function {
        def: Some(Instruction::new(
            Op::Function,
            Some(void),
            Some(helper_id),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(3),
            ],
        )),
        end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
        parameters: vec![],
        blocks: vec![
            Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(helper_entry_label),
                    vec![],
                )),
                instructions: vec![Instruction::new(
                    Op::Branch,
                    None,
                    None,
                    vec![Operand::IdRef(helper_return_label)],
                )],
            },
            Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(helper_return_label),
                    vec![],
                )),
                instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
            },
        ],
    };
    let mut ctx = Ctx::new(module_with(vec![entry, helper]));
    inline_helpers(&mut ctx, 0).expect("inline ok");
    let blocks = &ctx.module.functions[0].blocks;
    let header = blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(entry_label))
        .expect("entry block");
    assert!(
        header
            .instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::LoopMerge),
        "split caller header lost OpLoopMerge"
    );
    assert!(
        header
            .instructions
            .last()
            .is_some_and(|inst| inst.class.opcode == Op::Branch),
        "split caller header should branch into the inlined callee"
    );

    let continuation = blocks
        .iter()
        .find(|block| {
            block
                .instructions
                .last()
                .is_some_and(|inst| inst.class.opcode == Op::BranchConditional)
        })
        .expect("continuation block with original conditional");
    assert!(
        continuation
            .instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::SelectionMerge),
        "continuation conditional needs a SelectionMerge after loop merge is lifted"
    );
    assert!(
        !continuation
            .instructions
            .iter()
            .any(|inst| inst.class.opcode == Op::LoopMerge),
        "continuation must not keep the lifted OpLoopMerge"
    );
}

#[test]
fn lifted_loop_privatizes_existing_selection_at_continue_boundary() {
    let loop_merge_label = 30;
    let loop_continue_label = 31;
    let true_label = 32;
    let cond = 40;
    let mut post = vec![
        Instruction::new(
            Op::LoopMerge,
            None,
            None,
            vec![
                Operand::IdRef(loop_merge_label),
                Operand::IdRef(loop_continue_label),
                Operand::LoopControl(LoopControl::NONE),
            ],
        ),
        Instruction::new(
            Op::SelectionMerge,
            None,
            None,
            vec![
                Operand::IdRef(loop_continue_label),
                Operand::SelectionControl(spirv::SelectionControl::NONE),
            ],
        ),
        Instruction::new(
            Op::BranchConditional,
            None,
            None,
            vec![
                Operand::IdRef(cond),
                Operand::IdRef(true_label),
                Operand::IdRef(loop_continue_label),
            ],
        ),
    ];

    let (lifted, split) = lift_loop_merge_from_split_tail(&mut post);

    assert_eq!(
        lifted.and_then(|inst| inst.operands.get(1).cloned()),
        Some(Operand::IdRef(loop_continue_label))
    );
    assert_eq!(split, Some(loop_continue_label));
    assert_eq!(
        post.iter()
            .filter(|inst| inst.class.opcode == Op::SelectionMerge)
            .count(),
        1,
        "the existing selection marker is retained for the phi-aware split"
    );
}

#[test]
fn split_inlined_selection_merge_with_backward_merge_inserts_after_construct() {
    let void = 100;
    let function_id = 10;
    let merge_label = 20;
    let header_label = 30;
    let exit_label = 40;
    let cond = 50;

    let function = Function {
        def: Some(Instruction::new(
            Op::Function,
            Some(void),
            Some(function_id),
            vec![
                Operand::FunctionControl(FunctionControl::NONE),
                Operand::IdRef(3),
            ],
        )),
        end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
        parameters: vec![],
        blocks: vec![
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(merge_label), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::LoopMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(exit_label),
                            Operand::IdRef(merge_label),
                            Operand::LoopControl(LoopControl::NONE),
                        ],
                    ),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(header_label)]),
                ],
            },
            Block {
                label: Some(Instruction::new(
                    Op::Label,
                    None,
                    Some(header_label),
                    vec![],
                )),
                instructions: vec![
                    Instruction::new(
                        Op::SelectionMerge,
                        None,
                        None,
                        vec![
                            Operand::IdRef(merge_label),
                            Operand::SelectionControl(spirv::SelectionControl::NONE),
                        ],
                    ),
                    Instruction::new(
                        Op::BranchConditional,
                        None,
                        None,
                        vec![
                            Operand::IdRef(cond),
                            Operand::IdRef(merge_label),
                            Operand::IdRef(merge_label),
                        ],
                    ),
                ],
            },
            Block {
                label: Some(Instruction::new(Op::Label, None, Some(exit_label), vec![])),
                instructions: vec![Instruction::new(Op::Return, None, None, vec![])],
            },
        ],
    };
    let mut ctx = Ctx::new(module_with(vec![function]));
    split_inlined_selection_merge(&mut ctx, 0, header_label, merge_label);

    let blocks = &ctx.module.functions[0].blocks;
    let label_positions = blocks
        .iter()
        .enumerate()
        .filter_map(|(idx, block)| Some((block.label.as_ref()?.result_id?, idx)))
        .collect::<HashMap<_, _>>();
    let synthetic_label = 1000;
    assert!(
        label_positions[&synthetic_label] > label_positions[&header_label],
        "synthetic merge must be laid out after its in-construct predecessor"
    );

    let loop_header = blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(merge_label))
        .expect("loop header block");
    let loop_merge = loop_header
        .instructions
        .iter()
        .find(|inst| inst.class.opcode == Op::LoopMerge)
        .expect("loop merge");
    assert_eq!(
        loop_merge.operands.get(1),
        Some(&Operand::IdRef(merge_label)),
        "the enclosing loop retains its continue while the nested selection gets a private merge"
    );

    let header = blocks
        .iter()
        .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(header_label))
        .expect("header block");
    assert_eq!(
        header.instructions[0].operands.first(),
        Some(&Operand::IdRef(synthetic_label))
    );
    assert_eq!(
        header.instructions[1].operands[1],
        Operand::IdRef(synthetic_label)
    );

    let synthetic = blocks
        .iter()
        .find(|block| {
            block.label.as_ref().and_then(|label| label.result_id) == Some(synthetic_label)
        })
        .expect("synthetic block");
    assert_eq!(
        synthetic
            .instructions
            .last()
            .and_then(|inst| inst.operands.first()),
        Some(&Operand::IdRef(merge_label))
    );
}

#[test]
fn self_recursion_terminates_without_inlining() {
    let entry = mkfunc(
        10,
        2,
        11,
        vec![
            Instruction::new(
                Op::FunctionCall,
                Some(2),
                Some(12),
                vec![Operand::IdRef(10)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    let mut ctx = Ctx::new(module_with(vec![entry]));
    let stats = inline_helpers(&mut ctx, 0).expect("terminates without inlining self");
    assert_eq!(stats, InlineStats::default());
    assert_eq!(ctx.module.functions.len(), 1);
    assert_eq!(call_count(&ctx.module, 0), 1, "self-call retained");
}

#[test]
fn byte_view_reinterpret_recognized_and_reassembled_little_endian() {
    let void = 100;
    let uint = 104;
    let uint_0 = 105;
    let uchar = 110;
    let arr8uchar = 111;
    let struct_l0 = 112;
    let struct_l1 = 113;
    let struct_outer = 114;
    let ptr_func_arr = 115;
    let ptr_priv_struct = 116;
    let ptr_priv_uint = 117;
    let uint_1 = 118;
    let len8 = 119;
    let helper_param = 25;
    let chain = 31;
    let load = 32;
    let arr = 30;

    let mut entry = mkfunc(
        10,
        void,
        11,
        vec![
            Instruction::new(
                Op::Variable,
                Some(ptr_func_arr),
                Some(arr),
                vec![Operand::StorageClass(StorageClass::Function)],
            ),
            Instruction::new(
                Op::FunctionCall,
                None,
                None,
                vec![Operand::IdRef(20), Operand::IdRef(arr)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    entry.parameters.clear();

    let mut helper = mkfunc(
        20,
        void,
        21,
        vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(ptr_priv_uint),
                Some(chain),
                vec![
                    Operand::IdRef(helper_param),
                    Operand::IdRef(uint_1),
                    Operand::IdRef(uint_0),
                    Operand::IdRef(uint_0),
                ],
            ),
            Instruction::new(
                Op::Load,
                Some(uint),
                Some(load),
                vec![Operand::IdRef(chain)],
            ),
            Instruction::new(Op::Return, None, None, vec![]),
        ],
    );
    helper.parameters.push(Instruction::new(
        Op::FunctionParameter,
        Some(ptr_priv_struct),
        Some(helper_param),
        vec![],
    ));

    let mut module = module_with(vec![entry, helper]);
    module.types_global_values = vec![
        type_inst(Op::TypeVoid, void, vec![]),
        type_inst(
            Op::TypeInt,
            uint,
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(uint_0),
            vec![Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(uint_1),
            vec![Operand::LiteralBit32(1)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(len8),
            vec![Operand::LiteralBit32(8)],
        ),
        type_inst(
            Op::TypeInt,
            uchar,
            vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
        ),
        type_inst(
            Op::TypeArray,
            arr8uchar,
            vec![Operand::IdRef(uchar), Operand::IdRef(len8)],
        ),
        type_inst(Op::TypeStruct, struct_l0, vec![Operand::IdRef(uint)]),
        type_inst(Op::TypeStruct, struct_l1, vec![Operand::IdRef(struct_l0)]),
        type_inst(
            Op::TypeStruct,
            struct_outer,
            vec![Operand::IdRef(struct_l1), Operand::IdRef(struct_l1)],
        ),
        type_inst(
            Op::TypePointer,
            ptr_func_arr,
            vec![
                Operand::StorageClass(StorageClass::Function),
                Operand::IdRef(arr8uchar),
            ],
        ),
        type_inst(
            Op::TypePointer,
            ptr_priv_struct,
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(struct_outer),
            ],
        ),
        type_inst(
            Op::TypePointer,
            ptr_priv_uint,
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(uint),
            ],
        ),
    ];

    let mut ctx = Ctx::new(module);
    let result_types = collect_result_types(&ctx);
    let mut remap: HashMap<Word, Word> = HashMap::default();
    remap.insert(helper_param, arr);
    let callee = ctx.module.functions[1].clone();
    let plan = plan_byte_view_reinterprets(&ctx, &callee, &remap, &result_types);
    let bv = plan.get(&chain).expect("byte-view reinterpret recognized");
    assert_eq!(bv.arg, arr, "byte reassembly roots at the caller array");
    assert_eq!(bv.offset, 4, "second nested uint sits at byte offset 4");
    assert_eq!(bv.width, 4, "uint is 4 bytes");
    assert_eq!(bv.scalar_ty, uint);

    let out_id = 900;
    let insts = emit_byte_view_load(&mut ctx, bv, out_id, uint);
    let count = |op: Op| insts.iter().filter(|i| i.class.opcode == op).count();
    assert_eq!(
        count(Op::InBoundsAccessChain),
        4,
        "one byte pointer per byte"
    );
    assert_eq!(count(Op::Load), 4, "one byte load per byte");
    assert_eq!(count(Op::UConvert), 4, "each byte widened to uint");
    assert_eq!(count(Op::ShiftLeftLogical), 3, "bytes 1..3 shifted");
    assert_eq!(count(Op::BitwiseOr), 3, "3 ORs fold 4 bytes");
    let defs = type_defs_with_new_globals(&ctx);
    let mut byte_offsets: Vec<u64> = insts
        .iter()
        .filter(|i| i.class.opcode == Op::InBoundsAccessChain)
        .filter_map(|i| match i.operands.get(1) {
            Some(Operand::IdRef(c)) => const_int_value(&defs, *c),
            _ => None,
        })
        .collect();
    byte_offsets.sort_unstable();
    assert_eq!(byte_offsets, vec![4, 5, 6, 7], "byte offsets 4..7");
    let last = insts.last().expect("terminal instruction");
    assert_eq!(last.class.opcode, Op::CopyObject);
    assert_eq!(last.result_id, Some(out_id));
    assert_eq!(last.result_type, Some(uint));
}

#[test]
fn half_byte_view_uses_a_16_bit_accumulator_for_load_and_store() {
    let uchar = 10;
    let half = 11;
    let mut module = Module::new();
    module.header = Some(ModuleHeader::new(100));
    module.types_global_values = vec![
        type_inst(
            Op::TypeInt,
            uchar,
            vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
        ),
        type_inst(Op::TypeFloat, half, vec![Operand::LiteralBit32(16)]),
    ];
    let mut ctx = Ctx::new(module);
    let integer_width = |ctx: &Ctx, ty| {
        ctx.module
            .types_global_values
            .iter()
            .chain(ctx.new_globals.iter())
            .find(|instruction| instruction.result_id == Some(ty))
            .and_then(|instruction| match instruction.operands.first() {
                Some(Operand::LiteralBit32(width)) => Some(*width),
                _ => None,
            })
    };
    let plan = ByteViewPlan {
        arg: 20,
        storage: StorageClass::Function,
        elem_uchar_ty: Some(uchar),
        scalar_backing_ty: None,
        offset: 0,
        width: 2,
        scalar_ty: half,
    };

    let load = emit_byte_view_load(&mut ctx, &plan, 30, half);
    let load_bitcast = load.last().expect("load ends in its scalar bitcast");
    assert_eq!(load_bitcast.class.opcode, Op::Bitcast);
    assert_eq!(load_bitcast.result_type, Some(half));
    let Operand::IdRef(load_bits) = load_bitcast.operands[0] else {
        panic!("load bitcast has an id operand");
    };
    let load_bits_ty = load
        .iter()
        .find(|instruction| instruction.result_id == Some(load_bits))
        .and_then(|instruction| instruction.result_type)
        .expect("assembled load has a type");
    assert_eq!(integer_width(&ctx, load_bits_ty), Some(16));

    let store = emit_byte_view_store(&mut ctx, &plan, 31);
    let store_bitcast = store.first().expect("store starts with its scalar bitcast");
    assert_eq!(store_bitcast.class.opcode, Op::Bitcast);
    let store_bits_ty = store_bitcast
        .result_type
        .expect("store bitcast has a result type");
    assert_eq!(integer_width(&ctx, store_bits_ty), Some(16));
}

#[test]
fn byte_view_struct_offset_uses_source_vector_allocation_stride() {
    let mut module = Module::new();
    module.header = Some(ModuleHeader::new(10));
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
    ];
    let defs = type_defs(&module);
    let mut ctx = Ctx::new(module);
    ctx.air_data_layout = Some(
        crate::layout::AirDataLayout::parse("e-v24:64:64")
            .expect("parse vector alignment override"),
    );

    assert_eq!(struct_member_byte_offset(&ctx, &defs, 3, 1), Some(8));
}
