//! Fragment stencil export (`[[stencil]]`) lowered for GPUs that cannot export stencil.
//!
//! Metal guarantees a fragment shader can write the stencil reference; NVIDIA hardware cannot (its own drivers expose
//! neither GL_ARB_ nor VK_EXT_shader_stencil_export), and the Vulkan driver drops the write. Blender's Metal backend
//! assumes the export and Eevee classifies its G-buffer with it, so every lit surface rendered black. This rewrite does
//! what Blender itself does where the export is missing: the shader keeps the value it would export, and at each return
//! discards unless that value has the bit the driver pushes (a push-constant block, member at offset
//! `STENCIL_EXPORT_PUSH_OFFSET`). The driver replays the draw once with bit 0 (no discard; reference 0 clears the
//! write-masked bits) and once per write-mask bit with only that bit writable, which rebuilds the exported value.

use crate::spirv_module::{Block, Instruction, Module, Operand};
use spirv::{BuiltIn, Capability, Decoration, Op, StorageClass, Word};
use std::collections::HashMap;

pub(crate) const STENCIL_EXPORT_PUSH_OFFSET: u32 = 160;

fn find_type(module: &Module, op: Op, operands: &[Operand]) -> Option<Word> {
    module
        .types_global_values
        .iter()
        .find(|i| i.class.opcode == op && i.operands == operands)
        .and_then(|i| i.result_id)
}

pub(super) fn lower_stencil_export(module: &mut Module) -> bool {
    // the stencil output variable
    let Some(var) = module.annotations.iter().find_map(|a| {
        (a.class.opcode == Op::Decorate
            && a.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn))
            && a.operands.get(2) == Some(&Operand::BuiltIn(BuiltIn::FragStencilRefEXT)))
        .then(|| match a.operands.first() {
            Some(Operand::IdRef(id)) => Some(*id),
            _ => None,
        })
        .flatten()
    }) else {
        return false;
    };
    // a module that already has push constants keeps the export (one push-constant block per entry point)
    if module.types_global_values.iter().any(|i| {
        i.class.opcode == Op::Variable && i.operands.first() == Some(&Operand::StorageClass(StorageClass::PushConstant))
    }) {
        return false;
    }
    let defs: HashMap<Word, Instruction> =
        module.types_global_values.iter().filter_map(|i| i.result_id.map(|id| (id, i.clone()))).collect();
    let Some(var_ptr_ty) = defs.get(&var).and_then(|v| v.result_type) else { return false };
    let Some(Operand::IdRef(value_ty)) = defs.get(&var_ptr_ty).and_then(|p| p.operands.get(1)).cloned() else { return false };
    let Some(value_def) = defs.get(&value_ty).cloned() else { return false };
    if value_def.class.opcode != Op::TypeInt {
        return false;
    }
    let signed = value_def.operands.get(1) == Some(&Operand::LiteralBit32(1));
    // the fragment entry point
    let Some(entry_fn) = module.entry_points.iter().find_map(|e| match (e.operands.first(), e.operands.get(1)) {
        (Some(Operand::ExecutionModel(spirv::ExecutionModel::Fragment)), Some(Operand::IdRef(f))) => Some(*f),
        _ => None,
    }) else {
        return false;
    };

    let mut new_types: Vec<Instruction> = Vec::new();
    let fresh_type = |module: &mut Module, new_types: &mut Vec<Instruction>, op: Op, operands: Vec<Operand>| -> Word {
        if let Some(id) = find_type(module, op, &operands).or_else(|| {
            new_types.iter().find(|i| i.class.opcode == op && i.operands == operands).and_then(|i| i.result_id)
        }) {
            return id;
        }
        let id = module.fresh_id();
        new_types.push(Instruction::new(op, None, Some(id), operands));
        id
    };
    let uint_ty = fresh_type(module, &mut new_types, Op::TypeInt, vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)]);
    let bool_ty = fresh_type(module, &mut new_types, Op::TypeBool, vec![]);
    // the push-constant block gets its own struct type (Block-decorated), never shared
    let block_ty = module.fresh_id();
    new_types.push(Instruction::new(Op::TypeStruct, None, Some(block_ty), vec![Operand::IdRef(uint_ty)]));
    let block_ptr = fresh_type(module, &mut new_types, Op::TypePointer, vec![Operand::StorageClass(StorageClass::PushConstant), Operand::IdRef(block_ty)]);
    let member_ptr = fresh_type(module, &mut new_types, Op::TypePointer, vec![Operand::StorageClass(StorageClass::PushConstant), Operand::IdRef(uint_ty)]);
    let private_ptr = fresh_type(module, &mut new_types, Op::TypePointer, vec![Operand::StorageClass(StorageClass::Private), Operand::IdRef(value_ty)]);
    let pc_var = module.fresh_id();
    let zero = module.fresh_id();
    new_types.push(Instruction::new(Op::Constant, Some(uint_ty), Some(zero), vec![Operand::LiteralBit32(0)]));
    // constants and variables after the types they use: append, then the pc variable
    module.types_global_values.extend(new_types);
    module.types_global_values.push(Instruction::new(
        Op::Variable,
        Some(block_ptr),
        Some(pc_var),
        vec![Operand::StorageClass(StorageClass::PushConstant)],
    ));
    module.annotations.push(Instruction::new(Op::Decorate, None, None, vec![Operand::IdRef(block_ty), Operand::Decoration(Decoration::Block)]));
    module.annotations.push(Instruction::new(
        Op::MemberDecorate,
        None,
        None,
        vec![Operand::IdRef(block_ty), Operand::LiteralBit32(0), Operand::Decoration(Decoration::Offset), Operand::LiteralBit32(STENCIL_EXPORT_PUSH_OFFSET)],
    ));
    // the output becomes a Private variable holding the value
    module.annotations.retain(|a| {
        !(a.class.opcode == Op::Decorate
            && a.operands.first() == Some(&Operand::IdRef(var))
            && a.operands.get(1) == Some(&Operand::Decoration(Decoration::BuiltIn)))
    });
    // a Private global must follow its pointer type: move the variable to the end of the globals
    if let Some(pos) = module.types_global_values.iter().position(|i| i.result_id == Some(var)) {
        let mut v = module.types_global_values.remove(pos);
        v.result_type = Some(private_ptr);
        v.operands = vec![Operand::StorageClass(StorageClass::Private)];
        module.types_global_values.push(v);
    }
    for e in module.entry_points.iter_mut() {
        if matches!(e.operands.get(1), Some(Operand::IdRef(f)) if *f == entry_fn) {
            e.operands.push(Operand::IdRef(pc_var));
        }
    }
    module.capabilities.retain(|c| c.operands.first() != Some(&Operand::Capability(Capability::StencilExportEXT)));
    module.execution_modes.retain(|m| m.operands.get(1) != Some(&Operand::ExecutionMode(spirv::ExecutionMode::StencilRefReplacingEXT)));
    module.extensions.retain(|x| !matches!(x.operands.first(), Some(Operand::LiteralString(s)) if s == "SPV_EXT_shader_stencil_export"));

    // at each return of the entry function: discard unless (value & bit), when a bit is pushed
    let Some(fi) = module.functions.iter().position(|f| f.def.as_ref().and_then(|d| d.result_id) == Some(entry_fn)) else {
        return true;
    };
    let mut blocks: Vec<Block> = Vec::new();
    for mut block in std::mem::take(&mut module.functions[fi].blocks) {
        let ends_in_return = block.instructions.last().is_some_and(|i| i.class.opcode == Op::Return);
        if !ends_in_return {
            blocks.push(block);
            continue;
        }
        block.instructions.pop();
        let v = module.fresh_id();
        block.instructions.push(Instruction::new(Op::Load, Some(value_ty), Some(v), vec![Operand::IdRef(var)]));
        let vu = if signed {
            let b = module.fresh_id();
            block.instructions.push(Instruction::new(Op::Bitcast, Some(uint_ty), Some(b), vec![Operand::IdRef(v)]));
            b
        } else {
            v
        };
        let chain = module.fresh_id();
        block.instructions.push(Instruction::new(Op::AccessChain, Some(member_ptr), Some(chain), vec![Operand::IdRef(pc_var), Operand::IdRef(zero)]));
        let bit = module.fresh_id();
        block.instructions.push(Instruction::new(Op::Load, Some(uint_ty), Some(bit), vec![Operand::IdRef(chain)]));
        let anded = module.fresh_id();
        block.instructions.push(Instruction::new(Op::BitwiseAnd, Some(uint_ty), Some(anded), vec![Operand::IdRef(vu), Operand::IdRef(bit)]));
        let missing = module.fresh_id();
        block.instructions.push(Instruction::new(Op::IEqual, Some(bool_ty), Some(missing), vec![Operand::IdRef(anded), Operand::IdRef(zero)]));
        let pushed = module.fresh_id();
        block.instructions.push(Instruction::new(Op::INotEqual, Some(bool_ty), Some(pushed), vec![Operand::IdRef(bit), Operand::IdRef(zero)]));
        let cond = module.fresh_id();
        block.instructions.push(Instruction::new(Op::LogicalAnd, Some(bool_ty), Some(cond), vec![Operand::IdRef(missing), Operand::IdRef(pushed)]));
        let kill_label = module.fresh_id();
        let ret_label = module.fresh_id();
        block.instructions.push(Instruction::new(Op::SelectionMerge, None, None, vec![Operand::IdRef(ret_label), Operand::SelectionControl(spirv::SelectionControl::NONE)]));
        block.instructions.push(Instruction::new(Op::BranchConditional, None, None, vec![Operand::IdRef(cond), Operand::IdRef(kill_label), Operand::IdRef(ret_label)]));
        blocks.push(block);
        let mut kill = Block::new();
        kill.label = Some(Instruction::new(Op::Label, None, Some(kill_label), vec![]));
        kill.instructions.push(Instruction::new(Op::Kill, None, None, vec![]));
        blocks.push(kill);
        let mut ret = Block::new();
        ret.label = Some(Instruction::new(Op::Label, None, Some(ret_label), vec![]));
        ret.instructions.push(Instruction::new(Op::Return, None, None, vec![]));
        blocks.push(ret);
    }
    module.functions[fi].blocks = blocks;
    true
}
