use crate::fxhash::{HashMap, HashSet};
use crate::spirv_module::Operand;
use crate::spirv_module::{load_bytes, Module};
use spirv::{Decoration, Op, StorageClass, Word};

mod bounded_work;
mod buffers;
mod byte_image;
mod control_flow;
mod interface;
mod intrinsics;
mod misc;
mod textures;
mod threadgroup;
mod types;

fn asm_has_line(asm: &str, needle: &str) -> bool {
    asm.lines().any(|line| line.trim() == needle)
}

fn whole_workgroup_options() -> crate::passes::TransformOptions {
    crate::passes::TransformOptions {
        kernel_dispatch: Some(crate::reflect::KernelDispatch::Workgroups),
        ..crate::passes::TransformOptions::default()
    }
}

fn assert_no_pointer_bitcasts(spv: &[u8]) {
    let module = load_bytes(spv).expect("load spv");
    let pointer_types = module
        .types_global_values
        .iter()
        .filter_map(|inst| {
            (inst.class.opcode == Op::TypePointer)
                .then_some(inst.result_id)
                .flatten()
        })
        .collect::<HashSet<_>>();
    let pointer_bitcast = module.all_inst_iter().find(|inst| {
        inst.class.opcode == Op::Bitcast
            && inst
                .result_type
                .is_some_and(|ty| pointer_types.contains(&ty))
    });
    assert!(pointer_bitcast.is_none(), "{pointer_bitcast:?}");
}

fn uint32_type_id(asm: &str) -> String {
    asm.lines()
        .find(|line| line.trim_end().ends_with("OpTypeInt 32 0"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("%uint")
        .to_string()
}

fn interface_variable_at_location(
    module: &Module,
    storage: StorageClass,
    location: u32,
) -> Option<Word> {
    module.annotations.iter().find_map(|inst| {
        if inst.class.opcode != Op::Decorate {
            return None;
        }
        let [
            Operand::IdRef(id),
            Operand::Decoration(Decoration::Location),
            Operand::LiteralBit32(loc),
        ] = inst.operands.as_slice()
        else {
            return None;
        };
        if *loc != location || variable_storage_class(module, *id) != Some(storage) {
            return None;
        }
        Some(*id)
    })
}

fn variable_storage_class(module: &Module, var: Word) -> Option<StorageClass> {
    module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::Variable && inst.result_id == Some(var) {
            if let Some(Operand::StorageClass(storage)) = inst.operands.first() {
                return Some(*storage);
            }
        }
        None
    })
}

fn pointer_type_storage_class(module: &Module, ptr_ty: Word) -> Option<StorageClass> {
    module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::TypePointer && inst.result_id == Some(ptr_ty) {
            if let Some(Operand::StorageClass(storage)) = inst.operands.first() {
                return Some(*storage);
            }
        }
        None
    })
}

fn variable_pointee_type(module: &Module, var: Word) -> Option<Word> {
    let ptr_ty = module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::Variable && inst.result_id == Some(var) {
            inst.result_type
        } else {
            None
        }
    })?;
    module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::TypePointer && inst.result_id == Some(ptr_ty) {
            if let Some(Operand::IdRef(pointee)) = inst.operands.get(1) {
                return Some(*pointee);
            }
        }
        None
    })
}

fn assert_phi_operand_types_match(module: &Module, asm: &str) {
    let result_types = module
        .all_inst_iter()
        .filter_map(|inst| Some((inst.result_id?, inst.result_type?)))
        .collect::<HashMap<_, _>>();
    for inst in module.all_inst_iter() {
        if inst.class.opcode != Op::Phi {
            continue;
        }
        let Some(result_type) = inst.result_type else {
            continue;
        };
        for incoming in inst.operands.chunks(2) {
            let [Operand::IdRef(value), Operand::IdRef(_label)] = incoming else {
                continue;
            };
            assert_eq!(
                result_types.get(value),
                Some(&result_type),
                "phi {:?} incoming value {value} has mismatched type\n{asm}",
                inst.result_id
            );
        }
    }
}

fn is_unsigned_int_vector(module: &Module, ty: Word, bits: u32, lanes: u32) -> bool {
    let Some(elem) = module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::TypeVector
            && inst.result_id == Some(ty)
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(lanes))
        {
            if let Some(Operand::IdRef(elem)) = inst.operands.first() {
                return Some(*elem);
            }
        }
        None
    }) else {
        return false;
    };
    module.types_global_values.iter().any(|inst| {
        inst.class.opcode == Op::TypeInt
            && inst.result_id == Some(elem)
            && inst.operands.first() == Some(&Operand::LiteralBit32(bits))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(0))
    })
}

fn is_float_vector(module: &Module, ty: Word, lanes: u32) -> bool {
    let Some(elem) = module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::TypeVector
            && inst.result_id == Some(ty)
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(lanes))
        {
            if let Some(Operand::IdRef(elem)) = inst.operands.first() {
                return Some(*elem);
            }
        }
        None
    }) else {
        return false;
    };
    module.types_global_values.iter().any(|inst| {
        inst.class.opcode == Op::TypeFloat
            && inst.result_id == Some(elem)
            && inst.operands.first() == Some(&Operand::LiteralBit32(32))
    })
}

fn is_signed_i32_vector(module: &Module, ty: Word, lanes: u32) -> bool {
    let Some(elem) = module.types_global_values.iter().find_map(|inst| {
        if inst.class.opcode == Op::TypeVector
            && inst.result_id == Some(ty)
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(lanes))
        {
            if let Some(Operand::IdRef(elem)) = inst.operands.first() {
                return Some(*elem);
            }
        }
        None
    }) else {
        return false;
    };
    module.types_global_values.iter().any(|inst| {
        inst.class.opcode == Op::TypeInt
            && inst.result_id == Some(elem)
            && inst.operands.first() == Some(&Operand::LiteralBit32(32))
            && inst.operands.get(1) == Some(&Operand::LiteralBit32(1))
    })
}
