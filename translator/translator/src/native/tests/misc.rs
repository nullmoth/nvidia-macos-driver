#![allow(unused_imports)]
use super::super::cfg::{
    id_ref_operand, infer_branch_merges, infer_loop_merges, infer_switch_merges,
    lower_unstructured_switches, split_body_blocks, BodyBlock,
};
use super::super::emit_vulkan_spirv;
use super::super::emitter::Emitter;
use super::super::ir::{LlType, LlValue};
use super::super::parse::{parse_type, parse_typed_value};
use super::*;
use crate::fxhash::{HashMap, HashSet};
use crate::passes::{self, Stage};
use crate::spirv_module::load_bytes;
use crate::spirv_module::Operand;
use crate::spirv_module::{Block, Function, Instruction};
use crate::{disassemble, meta, tools};
use spirv::{Capability, Decoration, Op, Scope, SelectionControl, StorageClass, Word};

fn cap(capability: Capability) -> Instruction {
    Instruction::new(
        Op::Capability,
        None,
        None,
        vec![Operand::Capability(capability)],
    )
}

fn has_cap(module: &Module, capability: Capability) -> bool {
    module.capabilities.iter().any(
        |inst| matches!(inst.operands.as_slice(), [Operand::Capability(c)] if *c == capability),
    )
}

fn module_with_variable_pointer_caps(result_type: Word) -> Module {
    let uint = 1;
    let storage_buffer_uint_ptr = 2;
    let mut module = Module::new();
    module.capabilities = vec![
        cap(Capability::Shader),
        cap(Capability::VariablePointers),
        cap(Capability::VariablePointersStorageBuffer),
    ];
    module.types_global_values = vec![
        Instruction::new(
            Op::TypeInt,
            None,
            Some(uint),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_uint_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(uint),
            ],
        ),
    ];
    module.functions.push(Function {
        def: None,
        end: None,
        parameters: vec![],
        blocks: vec![Block {
            label: None,
            instructions: vec![Instruction::new(
                Op::Select,
                Some(result_type),
                Some(10),
                vec![Operand::IdRef(3), Operand::IdRef(4), Operand::IdRef(5)],
            )],
        }],
    });
    module
}

#[test]
fn native_capability_closure_drops_stale_variable_pointer_caps() {
    let uint = 1;
    let mut module = module_with_variable_pointer_caps(uint);

    super::super::add_native_module_capabilities(&mut module);

    assert!(!has_cap(&module, Capability::VariablePointers));
    assert!(!has_cap(&module, Capability::VariablePointersStorageBuffer));
}

#[test]
fn native_capability_closure_keeps_storage_buffer_pointer_requirement() {
    let storage_buffer_uint_ptr = 2;
    let mut module = module_with_variable_pointer_caps(storage_buffer_uint_ptr);

    super::super::add_native_module_capabilities(&mut module);

    assert!(!has_cap(&module, Capability::VariablePointers));
    assert!(has_cap(&module, Capability::VariablePointersStorageBuffer));
}

#[test]
fn native_capability_closure_lowers_zero_base_storage_buffer_ptr_access_chain() {
    let uint = 1;
    let runtime_array = 2;
    let block_ty = 3;
    let storage_buffer_block_ptr = 4;
    let storage_buffer_uint_ptr = 5;
    let zero = 6;
    let dynamic_index = 7;
    let buffer = 8;
    let base = 9;
    let ptr = 10;
    let mut module = Module::new();
    module.capabilities = vec![
        cap(Capability::Shader),
        cap(Capability::VariablePointersStorageBuffer),
    ];
    module.types_global_values = vec![
        Instruction::new(
            Op::TypeInt,
            None,
            Some(uint),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(zero),
            vec![Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(dynamic_index),
            vec![Operand::LiteralBit32(11)],
        ),
        Instruction::new(
            Op::TypeRuntimeArray,
            None,
            Some(runtime_array),
            vec![Operand::IdRef(uint)],
        ),
        Instruction::new(
            Op::TypeStruct,
            None,
            Some(block_ty),
            vec![Operand::IdRef(runtime_array)],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_block_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(block_ty),
            ],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_uint_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(uint),
            ],
        ),
        Instruction::new(
            Op::Variable,
            Some(storage_buffer_block_ptr),
            Some(buffer),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ),
    ];
    module.functions.push(Function {
        def: None,
        end: None,
        parameters: vec![],
        blocks: vec![Block {
            label: None,
            instructions: vec![
                Instruction::new(
                    Op::AccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(base),
                    vec![
                        Operand::IdRef(buffer),
                        Operand::IdRef(zero),
                        Operand::IdRef(zero),
                    ],
                ),
                Instruction::new(
                    Op::PtrAccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(ptr),
                    vec![Operand::IdRef(base), Operand::IdRef(dynamic_index)],
                ),
            ],
        }],
    });

    super::super::add_native_module_capabilities(&mut module);

    let rewritten = module.functions[0].blocks[0]
        .instructions
        .iter()
        .find(|inst| inst.result_id == Some(ptr))
        .expect("rewritten pointer");
    assert_eq!(rewritten.class.opcode, Op::AccessChain);
    assert_eq!(
        rewritten.operands,
        vec![
            Operand::IdRef(buffer),
            Operand::IdRef(zero),
            Operand::IdRef(dynamic_index),
        ]
    );
    assert!(!has_cap(&module, Capability::VariablePointersStorageBuffer));
}

#[test]
fn native_capability_closure_lowers_dynamic_base_storage_buffer_ptr_access_chain() {
    let uint = 1;
    let runtime_array = 2;
    let block_ty = 3;
    let storage_buffer_block_ptr = 4;
    let storage_buffer_uint_ptr = 5;
    let zero = 6;
    let base_index = 7;
    let ptr_index = 8;
    let buffer = 9;
    let base = 10;
    let ptr = 11;
    let mut module = Module::new();
    module.header = Some(crate::spirv_module::ModuleHeader::new(20));
    module.capabilities = vec![
        cap(Capability::Shader),
        cap(Capability::VariablePointersStorageBuffer),
    ];
    module.types_global_values = vec![
        Instruction::new(
            Op::TypeInt,
            None,
            Some(uint),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(zero),
            vec![Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(base_index),
            vec![Operand::LiteralBit32(5)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(ptr_index),
            vec![Operand::LiteralBit32(11)],
        ),
        Instruction::new(
            Op::TypeRuntimeArray,
            None,
            Some(runtime_array),
            vec![Operand::IdRef(uint)],
        ),
        Instruction::new(
            Op::TypeStruct,
            None,
            Some(block_ty),
            vec![Operand::IdRef(runtime_array)],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_block_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(block_ty),
            ],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_uint_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(uint),
            ],
        ),
        Instruction::new(
            Op::Variable,
            Some(storage_buffer_block_ptr),
            Some(buffer),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ),
    ];
    module.functions.push(Function {
        def: None,
        end: None,
        parameters: vec![],
        blocks: vec![Block {
            label: None,
            instructions: vec![
                Instruction::new(
                    Op::AccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(base),
                    vec![
                        Operand::IdRef(buffer),
                        Operand::IdRef(zero),
                        Operand::IdRef(base_index),
                    ],
                ),
                Instruction::new(
                    Op::PtrAccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(ptr),
                    vec![Operand::IdRef(base), Operand::IdRef(ptr_index)],
                ),
            ],
        }],
    });

    super::super::add_native_module_capabilities(&mut module);

    let block = &module.functions[0].blocks[0];
    let add = block
        .instructions
        .iter()
        .find(|inst| inst.class.opcode == Op::IAdd)
        .expect("base index + pointer offset");
    assert_eq!(add.result_type, Some(uint));
    assert_eq!(
        add.operands,
        vec![Operand::IdRef(base_index), Operand::IdRef(ptr_index)]
    );
    let rewritten = block
        .instructions
        .iter()
        .find(|inst| inst.result_id == Some(ptr))
        .expect("rewritten pointer");
    assert_eq!(rewritten.class.opcode, Op::AccessChain);
    assert_eq!(
        rewritten.operands,
        vec![
            Operand::IdRef(buffer),
            Operand::IdRef(zero),
            Operand::IdRef(add.result_id.unwrap()),
        ]
    );
    assert!(!has_cap(&module, Capability::VariablePointersStorageBuffer));
}

#[test]
fn native_capability_closure_keeps_struct_member_ptr_access_chain() {
    let uint = 1;
    let block_ty = 2;
    let storage_buffer_block_ptr = 3;
    let storage_buffer_uint_ptr = 4;
    let zero = 5;
    let dynamic_index = 6;
    let buffer = 7;
    let base = 8;
    let ptr = 9;
    let mut module = Module::new();
    module.capabilities = vec![
        cap(Capability::Shader),
        cap(Capability::VariablePointersStorageBuffer),
    ];
    module.types_global_values = vec![
        Instruction::new(
            Op::TypeInt,
            None,
            Some(uint),
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(zero),
            vec![Operand::LiteralBit32(0)],
        ),
        Instruction::new(
            Op::Constant,
            Some(uint),
            Some(dynamic_index),
            vec![Operand::LiteralBit32(11)],
        ),
        Instruction::new(
            Op::TypeStruct,
            None,
            Some(block_ty),
            vec![Operand::IdRef(uint)],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_block_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(block_ty),
            ],
        ),
        Instruction::new(
            Op::TypePointer,
            None,
            Some(storage_buffer_uint_ptr),
            vec![
                Operand::StorageClass(StorageClass::StorageBuffer),
                Operand::IdRef(uint),
            ],
        ),
        Instruction::new(
            Op::Variable,
            Some(storage_buffer_block_ptr),
            Some(buffer),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ),
    ];
    module.functions.push(Function {
        def: None,
        end: None,
        parameters: vec![],
        blocks: vec![Block {
            label: None,
            instructions: vec![
                Instruction::new(
                    Op::AccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(base),
                    vec![Operand::IdRef(buffer), Operand::IdRef(zero)],
                ),
                Instruction::new(
                    Op::PtrAccessChain,
                    Some(storage_buffer_uint_ptr),
                    Some(ptr),
                    vec![Operand::IdRef(base), Operand::IdRef(dynamic_index)],
                ),
            ],
        }],
    });

    super::super::add_native_module_capabilities(&mut module);

    let retained = module.functions[0].blocks[0]
        .instructions
        .iter()
        .find(|inst| inst.result_id == Some(ptr))
        .expect("retained pointer");
    assert_eq!(retained.class.opcode, Op::PtrAccessChain);
    assert!(has_cap(&module, Capability::VariablePointersStorageBuffer));
}

#[test]
fn native_bda_off_leaves_logical_addressing() {
    let ll = r#"
define void @k(ptr addrspace(1) %out, ptr addrspace(1) %in) {
entry:
  %v = load i32, ptr addrspace(1) %in, align 4
  store i32 %v, ptr addrspace(1) %out, align 4
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 0, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"out"}
!4 = !{i32 1, !"air.buffer", !"air.buffer_size", i32 4, !"air.location_index", i32 1, i32 1, !"air.read", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"uint", !"air.arg_name", !"in"}
"#;
    let spv = crate::native::emit_vulkan_spirv_all_buffers_raw_bda(ll).expect("bda emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(
        !asm.contains("PhysicalStorageBuffer64"),
        "no device pointer → must stay Logical:\n{asm}"
    );
    assert!(!asm.contains("OpConvertUToPtr"), "{asm}");
}

#[test]
fn native_i64_division_guard_uses_64_bit_constants() {
    let ll = r#"
target triple = "air64-apple-macosx14.0.0"
define void @k(ptr addrspace(2) %src, ptr addrspace(1) %out) {
entry:
  %a = load i64, ptr addrspace(2) %src, align 8
  %bptr = getelementptr inbounds i64, ptr addrspace(2) %src, i64 1
  %b = load i64, ptr addrspace(2) %bptr, align 8
  %q = udiv i64 %a, %b
  store i64 %q, ptr addrspace(1) %out, align 8
  ret void
}

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4}
!3 = !{i32 0, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.read", !"air.address_space", i32 2, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"src"}
!4 = !{i32 1, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_align_size", i32 8, !"air.arg_type_name", !"ulong", !"air.arg_name", !"out"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_i64_division_guard_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpUDiv"), "{asm}");
    assert!(asm.contains("OpSelect"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_function_constant_defined_lowers_to_false_default() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@_Z1x.MTL_FC_INIT_1_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1

define void @main() {
entry:
  %defined = tail call i1 @air.is_function_constant_defined(ptr addrspace(2) @_Z1x.MTL_FC_INIT_1_b)
  br i1 %defined, label %enabled, label %disabled

enabled:
  br label %merge

disabled:
  br label %merge

merge:
  ret void
}

declare i1 @air.is_function_constant_defined(ptr addrspace(2))
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_fc_defined_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let fc_var = module
        .debug_names
        .iter()
        .find_map(|inst| match inst.operands.as_slice() {
            [Operand::IdRef(id), Operand::LiteralString(name)] if name.contains("MTL_FC_INIT") => {
                Some(*id)
            }
            _ => None,
        })
        .expect("native emit retains the function-constant global");
    let fc_storage = variable_storage_class(&module, fc_var);
    assert_eq!(
        fc_storage,
        Some(StorageClass::Private),
        "native globals are materialized as Private before the interface passes"
    );
    assert!(
        !module.types_global_values.iter().any(|inst| {
            inst.class.opcode == Op::Variable
                && matches!(
                    inst.operands.first(),
                    Some(Operand::StorageClass(StorageClass::UniformConstant))
                )
        }),
        "the retired post-emit fold matched only module-scope UniformConstant variables"
    );
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpConstantFalse"), "{asm}");
    assert!(
        asm.contains("__metal2vulkan.MTL_FC_DEFINED_1"),
        "definedness specialization marker should survive lowering: {asm}"
    );
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn a_supplied_function_constant_reaches_the_implicit_predicate_initializer() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@__metal_implicit_fc_pred_0 = internal addrspace(2) global i8 0, align 1
@_Z8CLIPPING.MTL_FC_INIT_1_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %defined = tail call i1 @air.is_function_constant_defined(ptr addrspace(2) @_Z8CLIPPING.MTL_FC_INIT_1_b)
  %widened = zext i1 %defined to i8
  %normalized = tail call i8 @air.normalize_function_constant_predicate.i8(i8 %widened)
  store i8 %normalized, ptr addrspace(2) @__metal_implicit_fc_pred_0
  ret void
}

define i32 @frag() {
entry:
  ret i32 7
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"int"}

declare i1 @air.is_function_constant_defined(ptr addrspace(2))
declare i8 @air.normalize_function_constant_predicate.i8(i8)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_fc_implicit_predicate_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let specialized = crate::translate_sanitized_native_specialized_with_options(
        ll,
        Stage::Fragment,
        &tmp,
        passes::TransformOptions::default(),
        &[(1, vec![1])],
    )
    .expect("a supplied function constant must not break the implicit predicate initializer");
    tools::spirv_val_bytes(&specialized, &tmp).expect("spirv-val");
    let asm = disassemble(&specialized).expect("disassemble specialized");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
}

#[test]
fn air_level_function_constant_specialization_selects_cfg_before_emission() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@enabled.MTL_FC_INIT_0_b = internal addrspace(2) externally_initialized constant i8 undef, section "air.fc_initializer", align 1
@enabled_copy = internal addrspace(2) global i8 undef, align 1

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %value = load i8, ptr addrspace(2) @enabled.MTL_FC_INIT_0_b
  %defined = call i1 @air.is_function_constant_defined(ptr addrspace(2) @enabled.MTL_FC_INIT_0_b)
  %selected = select i1 %defined, i8 %value, i8 0
  store i8 %selected, ptr addrspace(2) @enabled_copy
  ret void
}

define i32 @frag() {
entry:
  %value = load i8, ptr addrspace(2) @enabled_copy
  %disabled = icmp eq i8 %value, 0
  %result = select i1 %disabled, i32 7, i32 9
  ret i32 %result
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"int"}

declare i1 @air.is_function_constant_defined(ptr addrspace(2))
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_air_fc_specialization_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let default = crate::translate_sanitized_native(ll, Stage::Fragment, &tmp).expect("default");
    let enabled = crate::translate_sanitized_native_specialized_with_options(
        ll,
        Stage::Fragment,
        &tmp,
        passes::TransformOptions::default(),
        &[(0, vec![1])],
    )
    .expect("specialized");

    let constants = |bytes: &[u8]| {
        load_bytes(bytes)
            .expect("load")
            .types_global_values
            .into_iter()
            .filter(|inst| inst.class.opcode == Op::Constant)
            .filter_map(|inst| match inst.operands.as_slice() {
                [Operand::LiteralBit32(value)] => Some(*value),
                _ => None,
            })
            .collect::<HashSet<_>>()
    };
    let default_constants = constants(&default);
    let enabled_constants = constants(&enabled);
    assert!(default_constants.contains(&7), "{default_constants:?}");
    assert!(!default_constants.contains(&9), "{default_constants:?}");
    assert!(enabled_constants.contains(&9), "{enabled_constants:?}");
    assert!(!enabled_constants.contains(&7), "{enabled_constants:?}");
    tools::spirv_val_bytes(&enabled, &tmp).expect("spirv-val");
}

#[test]
fn air_level_vector_function_constant_remains_typed_through_lane_extraction() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@lanes.MTL_FC_INIT_0_Dv4_j = internal addrspace(2) externally_initialized constant <4 x i32> undef, section "air.fc_initializer", align 16

define i32 @frag() {
entry:
  %value = load <4 x i32>, ptr addrspace(2) @lanes.MTL_FC_INIT_0_Dv4_j
  %x = extractelement <4 x i32> %value, i64 0
  %y = extractelement <4 x i32> %value, i64 1
  %z = extractelement <4 x i32> %value, i64 2
  %w = extractelement <4 x i32> %value, i64 3
  %xy = add i32 %x, %y
  %zw = add i32 %z, %w
  %sum = add i32 %xy, %zw
  ret i32 %sum
}

!air.fragment = !{!0}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"uint"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_air_vector_fc_specialization_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let values = [1u32, 2, 3, 4]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();

    crate::translate_sanitized_native_specialized_with_options(
        ll,
        Stage::Fragment,
        &tmp,
        passes::TransformOptions::default(),
        &[(0, values)],
    )
    .expect("translate a specialized vector through all lane extractions");
}

#[test]
fn native_zero_memset_of_typed_alloca_stores_null_object() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%struct.Lighting = type { <3 x float>, <3 x float>, <3 x float>, <3 x float> }

define void @main() {
entry:
  %lighting = alloca %struct.Lighting, align 16
  %raw = bitcast ptr %lighting to ptr
  call void @llvm.memset.p0.i64(ptr %raw, i8 0, i64 64, i1 false)
  ret void
}

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_zero_memset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpConstantNull"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_workgroup_struct_padding_zero_writes_are_valid_by_construction() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Padded = type { i32, i8 }
@scratch = internal addrspace(3) global %Padded zeroinitializer, align 4

define void @main() {
entry:
  %padding = getelementptr i8, ptr addrspace(3) @scratch, i64 5
  %tail = getelementptr i8, ptr addrspace(3) %padding, i64 1
  call void @llvm.memset.p3.i64(ptr addrspace(3) %tail, i8 0, i64 2, i1 false)
  %padding.word = bitcast ptr addrspace(3) %tail to ptr addrspace(3)
  store i16 0, ptr addrspace(3) %padding.word, align 2
  ret void
}

declare void @llvm.memset.p3.i64(ptr addrspace(3), i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_workgroup_padding_memset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let emitted_asm = disassemble(
        &module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .expect("disassemble native module");
    assert!(!emitted_asm.contains("OpPtrAccessChain"), "{emitted_asm}");
    assert!(!emitted_asm.contains("llvm.memset"), "{emitted_asm}");

    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_partial_zero_memset_of_array_clears_leading_elements() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main() {
entry:
  %buf = alloca [15 x float], align 4
  call void @llvm.memset.p0.i64(ptr %buf, i8 0, i64 56, i1 false)
  ret void
}

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_partial_memset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert!(asm.contains("OpConstantNull"), "{asm}");
    assert_eq!(asm.matches("OpStore").count(), 14, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_zero_memset_past_its_pointee_clears_the_whole_element_run() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main() {
entry:
  %buf = alloca [8 x float], align 4
  %tail = getelementptr inbounds [8 x float], ptr %buf, i64 0, i64 2
  call void @llvm.memset.p0.i64(ptr %tail, i8 0, i64 24, i1 false)
  ret void
}

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_memset_run_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpStore").count(), 6, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_memcpy_past_its_pointee_copies_the_whole_element_run() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main() {
entry:
  %dst = alloca [12 x i8], align 4
  %src = alloca [12 x i8], align 4
  %d = getelementptr inbounds [12 x i8], ptr %dst, i64 0, i64 0
  %s = getelementptr inbounds [12 x i8], ptr %src, i64 0, i64 0
  call void @llvm.memcpy.p0.p0.i64(ptr %d, ptr %s, i64 12, i1 false)
  ret void
}

declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_memcpy_run_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpCopyMemory").count(), 12, "{asm}");
    assert!(!asm.contains("llvm.memcpy"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_zero_memset_of_a_device_pointer_table_keeps_its_single_store() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"

define void @main() {
entry:
  %table = alloca [4 x ptr addrspace(1)], align 8
  %tail = getelementptr inbounds [4 x ptr addrspace(1)], ptr %table, i64 0, i64 1
  call void @llvm.memset.p0.i64(ptr %tail, i8 0, i64 24, i1 false)
  ret void
}

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_memset_ptr_table_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpStore").count(), 1, "{asm}");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_partial_zero_memset_of_struct_clears_complete_prefix_fields() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%Prefix = type { i32, [3 x i32], i32 }

define void @main() {
entry:
  %value = alloca %Prefix, align 4
  call void @llvm.memset.p0.i64(ptr %value, i8 0, i64 16, i1 false)
  ret void
}

declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_partial_struct_memset_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let out = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect("interface transform")
        .assemble()
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let asm = disassemble(&out).expect("disassemble transformed");
    assert_eq!(asm.matches("OpStore").count(), 2, "{asm}");
    assert!(!asm.contains("OpFunctionCall"), "{asm}");
    assert!(!asm.contains("llvm.memset"), "{asm}");
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&out, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_command_encoder_helpers_are_refused_rather_than_dropped() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @main() {
entry:
  tail call void @air.set_object_buffer_render_command.p1i8(ptr addrspace(1) null, i32 0, ptr addrspace(1) null, i32 2)
  tail call void @air.draw_primitives_render_command(ptr addrspace(1) null, i32 0, i32 1, i32 2, i32 3, i32 4, i32 5)
  ret void
}

declare void @air.set_object_buffer_render_command.p1i8(ptr addrspace(1), i32, ptr addrspace(1), i32)
declare void @air.draw_primitives_render_command(ptr addrspace(1), i32, i32, i32, i32, i32, i32)
"#;
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let error = passes::transform(module, Stage::Kernel, None, None, None, Some("main"))
        .expect_err("an indirect-command-buffer encoder has no Vulkan lowering");
    assert!(
        error.contains("air.set_object_buffer_render_command")
            || error.contains("air.draw_primitives_render_command"),
        "the refusal must name the family it could not lower: {error}"
    );
    assert!(
        error.contains("no Vulkan equivalent"),
        "the refusal must say the family is recognised, not unknown: {error}"
    );
}

#[test]
fn a_command_encoder_family_is_recognised_rather_than_unknown() {
    use crate::air_intrinsics::{air_intrinsic_disposition, AirIntrinsicDisposition};
    for name in [
        "air.set_object_buffer_render_command.p1i8",
        "air.draw_primitives_render_command",
        "air.set_kernel_buffer_compute_command.p2i8",
        "air.set_threadgroup_memory_length_compute_command",
    ] {
        assert_eq!(
            air_intrinsic_disposition(name),
            Some(AirIntrinsicDisposition::NoVulkanEquivalent),
            "{name}"
        );
    }
    assert_eq!(air_intrinsic_disposition("air.future_command"), None);
}

#[test]
fn native_read_depth_2d_array_combines_layer_into_fetch_coord() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define void @k(ptr addrspace(1) %depth, ptr addrspace(2) %samp, ptr addrspace(1) %out, <2 x i32> %gid) {
entry:
  %layer = extractelement <2 x i32> %gid, i32 0
  %read = tail call { float, i8 } @air.read_depth_2d_array.f32(ptr addrspace(1) %depth, ptr addrspace(2) %samp, i32 1, <2 x i32> %gid, i32 %layer, <2 x i32> zeroinitializer, i32 0, i32 1)
  %value = extractvalue { float, i8 } %read, 0
  store float %value, ptr addrspace(1) %out, align 4
  ret void
}

declare { float, i8 } @air.read_depth_2d_array.f32(ptr addrspace(1), ptr addrspace(2), i32, <2 x i32>, i32, <2 x i32>, i32, i32)

!air.kernel = !{!0}
!0 = !{ptr @k, !1, !2}
!1 = !{}
!2 = !{!3, !4, !5, !6}
!3 = !{i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"depth2d_array<float, read>", !"air.arg_name", !"depth"}
!4 = !{i32 1, !"air.sampler", !"air.location_index", i32 0, i32 1, !"air.arg_type_name", !"sampler", !"air.arg_name", !"samp"}
!5 = !{i32 2, !"air.buffer", !"air.location_index", i32 0, i32 1, !"air.write", !"air.address_space", i32 1, !"air.arg_type_size", i32 4, !"air.arg_type_align_size", i32 4, !"air.arg_type_name", !"float*", !"air.arg_name", !"out"}
!6 = !{i32 3, !"air.thread_position_in_grid", !"air.arg_type_name", !"uint2", !"air.arg_name", !"gid"}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_read_depth_array_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let spv = crate::translate_sanitized_native(ll, Stage::Kernel, &tmp).expect("translate");
    let asm = disassemble(&spv).expect("disassemble");
    let module = load_bytes(&spv).expect("load spv");
    let fetch = module
        .all_inst_iter()
        .find(|inst| inst.class.opcode == Op::ImageFetch)
        .expect("image fetch");
    let coord = match fetch.operands.get(1) {
        Some(Operand::IdRef(id)) => *id,
        other => panic!("unexpected image fetch coordinate operand: {other:?}"),
    };
    let coord_ty = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord))
        .and_then(|inst| inst.result_type)
        .expect("coordinate type");
    let coord_ty_inst = module
        .all_inst_iter()
        .find(|inst| inst.result_id == Some(coord_ty))
        .expect("coordinate type definition");
    assert_eq!(coord_ty_inst.class.opcode, Op::TypeVector, "{asm}");
    assert!(
        matches!(
            coord_ty_inst.operands.get(1),
            Some(Operand::LiteralBit32(3))
        ),
        "arrayed 2D depth fetch coord must be a 3-component vector (x, y, layer)\n{asm}"
    );
    if std::process::Command::new("spirv-val")
        .arg("--version")
        .output()
        .is_ok()
    {
        tools::spirv_val_bytes(&spv, &tmp).expect("spirv-val");
    }
}

#[test]
fn native_alloca_lowers_to_function_variable() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define i32 @local_slot(i32 %x) {
entry:
  %slot = alloca i32, align 4
  store i32 %x, ptr %slot
  %v = load i32, ptr %slot
  ret i32 %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVariable"), "{asm}");
    assert!(asm.contains("Function"), "{asm}");
    assert!(asm.contains("OpStore"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_freeze_lowers_to_copy_object() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @freeze_scalar(float %x) {
entry:
  %f = freeze float %x
  ret float %f
}

define <2 x i32> @freeze_vector(<2 x i32> %v) {
entry:
  %f = freeze <2 x i32> %v
  ret <2 x i32> %f
}

define float @freeze_undef() {
entry:
  %f = freeze float undef
  ret float %f
}
"#;
    let spv = emit_vulkan_spirv(ll).expect("native emit");
    let asm = disassemble(&spv).expect("disassemble");
    assert!(asm.contains("OpCopyObject"), "{asm}");
}

#[test]
fn native_dedupes_matching_function_type_declarations() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define float @main(float %x) {
entry:
  %a = tail call fast float @helper_a(float %x, float 1.000000e+00)
  %b = tail call fast float @helper_b(float %a, float 2.000000e+00)
  ret float %b
}

declare float @helper_a(float, float)
declare float @helper_b(float, float)
"#;
    let module = load_bytes(emit_vulkan_spirv(ll).expect("native emit")).expect("load native spv");
    let float_ty = module
        .types_global_values
        .iter()
        .find(|inst| {
            inst.class.opcode == Op::TypeFloat && inst.operands == [Operand::LiteralBit32(32)]
        })
        .and_then(|inst| inst.result_id)
        .expect("float32 type");
    let duplicate_signature_types = module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::TypeFunction
                && inst.operands
                    == [
                        Operand::IdRef(float_ty),
                        Operand::IdRef(float_ty),
                        Operand::IdRef(float_ty),
                    ]
        })
        .count();
    assert_eq!(duplicate_signature_types, 1);
}

#[test]
fn native_shuffle_mask_poison_lanes_become_undef_sentinel() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @shuffle(<2 x float> %v) {
entry:
  %out = shufflevector <2 x float> %v, <2 x float> poison, <4 x i32> <i32 0, i32 1, i32 poison, i32 poison>
  ret <4 x float> %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVectorShuffle"), "{asm}");
    assert!(asm.contains("4294967295"), "{asm}");
}

#[test]
fn native_quoted_named_types_resolve() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
%"struct.metal::matrix.21" = type { [1 x <4 x float>] }
define <4 x float> @matrix(ptr addrspace(2) %m) {
entry:
  %p = getelementptr inbounds %"struct.metal::matrix.21", ptr addrspace(2) %m, i64 0, i32 0, i64 0
  %v = load <4 x float>, ptr addrspace(2) %p
  ret <4 x float> %v
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpInBoundsAccessChain"), "{asm}");
    assert!(asm.contains("OpLoad"), "{asm}");
}

#[test]
fn native_shuffle_mask_zeroinitializer_expands_to_zero_lanes() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <4 x float> @splat(<2 x float> %v) {
entry:
  %out = shufflevector <2 x float> %v, <2 x float> undef, <4 x i32> zeroinitializer
  ret <4 x float> %out
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpVectorShuffle"), "{asm}");
    assert!(asm.contains(" 0 0 0 0"), "{asm}");
}

#[test]
fn native_typed_zeroinitializer_lowers_to_constant_null() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
define <2 x i32> @zero() {
entry:
  ret <2 x i32> zeroinitializer
}
"#;
    let asm = disassemble(&emit_vulkan_spirv(ll).expect("native emit")).expect("disassemble");
    assert!(asm.contains("OpConstantNull"), "{asm}");
}

#[test]
fn native_parser_accepts_byte_string_constant_array() {
    let value = parse_typed_value(r#"[8 x i8] c"\03\06\0B\10\17 )@""#).expect("parse byte string");
    assert_eq!(value.ty, LlType::Array(Box::new(LlType::Int(8)), 8));
    let LlValue::Array(lanes) = value.value else {
        panic!("expected byte array");
    };
    let bytes = lanes
        .into_iter()
        .map(|lane| {
            assert_eq!(lane.ty, LlType::Int(8));
            match lane.value {
                LlValue::Int(byte) => byte as u8,
                other => panic!("expected byte lane, got {other:?}"),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(bytes, [3, 6, 11, 16, 23, 32, 41, 64]);
}

#[test]
fn a_function_constant_loaded_at_a_narrower_float_type_carries_its_bits() {
    let ll = r#"
target triple = "spirv-unknown-vulkan1.2"
@bias = internal unnamed_addr addrspace(2) global i16 undef, align 2
@bias.MTL_FC_INIT_0_t = internal unnamed_addr addrspace(2) externally_initialized constant i16 undef, section "air.fc_initializer", align 2
@scale = internal unnamed_addr addrspace(2) global i32 undef, align 4
@scale.MTL_FC_INIT_1_t = internal unnamed_addr addrspace(2) externally_initialized constant i32 undef, section "air.fc_initializer", align 4

define internal void @_GLOBAL__sub_I_fc() section "air.static_init" {
entry:
  %0 = load i16, ptr addrspace(2) @bias.MTL_FC_INIT_0_t, align 2
  store i16 %0, ptr addrspace(2) @bias, align 2
  %1 = load i32, ptr addrspace(2) @scale.MTL_FC_INIT_1_t, align 4
  store i32 %1, ptr addrspace(2) @scale, align 4
  ret void
}

define float @frag() {
entry:
  %bias = load half, ptr addrspace(2) @bias, align 2
  %scale = load float, ptr addrspace(2) @scale, align 4
  %widened = fpext half %bias to float
  %sum = fadd float %widened, %scale
  ret float %sum
}

!air.fragment = !{!0}
!air.function_constants = !{!4, !5}
!0 = !{ptr @frag, !1, !2}
!1 = !{!3}
!2 = !{}
!3 = !{!"air.render_target", i32 0, i32 0, !"air.arg_type_name", !"float"}
!4 = !{ptr addrspace(2) @bias.MTL_FC_INIT_0_t, !"ushort", !"bias", i32 0, i1 true}
!5 = !{ptr addrspace(2) @scale.MTL_FC_INIT_1_t, !"uint", !"scale", i32 1, i1 true}
"#;
    let tmp = std::env::temp_dir().join(format!(
        "metal2vulkan_native_fc_float_bits_{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let specialized = crate::translate_sanitized_native_specialized_with_options(
        ll,
        Stage::Fragment,
        &tmp,
        passes::TransformOptions::default(),
        &[(0, vec![0x00, 0xbc]), (1, vec![0x00, 0x00, 0x40, 0x40])],
    )
    .expect("a function constant loaded at half must reinterpret its bits");
    tools::spirv_val_bytes(&specialized, &tmp).expect("spirv-val");
    let asm = disassemble(&specialized).expect("disassemble specialized");
    assert!(
        asm.lines()
            .any(|line| line.contains("OpConstant") && line.trim_end().ends_with(" 3")),
        "the supplied uint bits must reinterpret to 3.0: {asm}"
    );
    let widened = asm
        .lines()
        .find_map(|line| line.split("OpFConvert").nth(1))
        .and_then(|rest| rest.split_whitespace().nth(1))
        .expect("the half load must be widened to the render target type")
        .to_string();
    assert!(
        asm.lines()
            .any(|line| line.starts_with(&format!("{widened} = OpConstant "))),
        "the supplied ushort bits must reach the widening as a half literal: {asm}"
    );
}
