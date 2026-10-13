use super::*;
use crate::passes::stage_input::layout_ty_size_align;

pub(in crate::passes) struct WgFlatwordCand {
    pub(in crate::passes) var: Word,
    pub(in crate::passes) elem_words: u32,
    pub(in crate::passes) total_words: u32,
}

pub(in crate::passes) fn validate_wg_flatword(
    ctx: &Ctx,
    func: &Function,
    cand: &WgFlatwordCand,
    uint_ty: Word,
) -> Option<Vec<(usize, usize)>> {
    let mut chains: Vec<(usize, usize)> = Vec::new();
    let mut chain_ids: HashSet<Word> = HashSet::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            let mentions = inst
                .operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(id) if *id == cand.var));
            if !mentions {
                continue;
            }
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                || inst.operands.len() != 2
                || operand_id(inst, 0) != Some(cand.var)
            {
                return None;
            }
            let cid = inst.result_id?;
            let idx = operand_id(inst, 1)?;
            if !index_is_word_addressed(ctx, func, idx, cand.elem_words) {
                return None;
            }
            chains.push((bi, ii));
            chain_ids.insert(cid);
        }
    }
    if chains.is_empty() {
        return None;
    }
    for block in &func.blocks {
        for inst in &block.instructions {
            if inst
                .result_id
                .map(|r| chain_ids.contains(&r))
                .unwrap_or(false)
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if let Some(ptr) = operand_id(inst, 0) {
                        if chain_ids.contains(&ptr) && inst.result_type != Some(uint_ty) {
                            return None;
                        }
                    }
                }
                Op::Store => {
                    if let Some(ptr) = operand_id(inst, 0) {
                        if chain_ids.contains(&ptr) {
                            let obj_ty =
                                operand_id(inst, 1).and_then(|o| value_result_type(ctx, o));
                            if obj_ty != Some(uint_ty) {
                                return None;
                            }
                        }
                    }
                }
                _ => {
                    if inst
                        .operands
                        .iter()
                        .any(|o| matches!(o, Operand::IdRef(id) if chain_ids.contains(id)))
                    {
                        return None;
                    }
                }
            }
        }
    }
    Some(chains)
}

pub(in crate::passes) fn remodel_workgroup_flatword_aggregate(ctx: &mut Ctx, entry_idx: usize) {
    let uint_ty = ctx.ty_uint();

    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(result) = inst.result_id {
            defs.insert(result, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut cands: Vec<WgFlatwordCand> = Vec::new();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let (Some(var), Some(ptr_ty)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let Some(&(StorageClass::Workgroup, pointee)) = ptr_info.get(&ptr_ty) else {
            continue;
        };
        let Some(arr_def) = defs.get(&pointee) else {
            continue;
        };
        if arr_def.class.opcode != Op::TypeArray {
            continue;
        }
        let (Some(Operand::IdRef(elem_ty)), Some(Operand::IdRef(len_c))) =
            (arr_def.operands.first(), arr_def.operands.get(1))
        else {
            continue;
        };
        let Some(k) = const_u32(ctx, *len_c) else {
            continue;
        };
        if direct_scalar_width(ctx, *elem_ty).is_some() {
            continue;
        }
        let (elem_bytes, _) = layout_ty_size_align(ctx, *elem_ty, &defs);
        if elem_bytes == 0 || elem_bytes % 4 != 0 {
            continue;
        }
        let elem_words = elem_bytes / 4;
        let Some(total_words) = k.checked_mul(elem_words) else {
            continue;
        };
        cands.push(WgFlatwordCand {
            var,
            elem_words,
            total_words,
        });
    }

    for cand in cands {
        let Some(chains) =
            validate_wg_flatword(ctx, &ctx.module.functions[entry_idx], &cand, uint_ty)
        else {
            continue;
        };
        let new_glob_start = ctx.new_globals.len();
        let arr_ty = ctx.ty_array(uint_ty, cand.total_words);
        let new_var_ptr = ctx.ty_ptr(StorageClass::Workgroup, arr_ty);
        let new_chain_ptr = ctx.ty_ptr(StorageClass::Workgroup, uint_ty);
        let synthesized: Vec<Instruction> = ctx.new_globals.drain(new_glob_start..).collect();
        let is_var =
            |i: &Instruction| i.class.opcode == Op::Variable && i.result_id == Some(cand.var);
        if let Some(p) = ctx.module.types_global_values.iter().position(is_var) {
            ctx.module.types_global_values[p].result_type = Some(new_var_ptr);
            let tail = ctx.module.types_global_values.split_off(p);
            ctx.module.types_global_values.extend(synthesized);
            ctx.module.types_global_values.extend(tail);
        } else if let Some(p) = ctx.new_globals.iter().position(is_var) {
            ctx.new_globals[p].result_type = Some(new_var_ptr);
            let tail = ctx.new_globals.split_off(p);
            ctx.new_globals.extend(synthesized);
            ctx.new_globals.extend(tail);
        } else {
            ctx.new_globals.extend(synthesized);
        }
        for &(bi, ii) in &chains {
            ctx.module.functions[entry_idx].blocks[bi].instructions[ii].result_type =
                Some(new_chain_ptr);
        }
    }
}

pub(in crate::passes) fn remodel_workgroup_single_field_struct_array(
    ctx: &mut Ctx,
    entry_idx: usize,
) {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(result) = inst.result_id {
            defs.insert(result, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(sc)), Some(Operand::IdRef(pointee))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*sc, *pointee));
            }
        }
    }

    let mut cands = Vec::new();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let (Some(var), Some(ptr_ty)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let Some(&(StorageClass::Workgroup, arr_ty)) = ptr_info.get(&ptr_ty) else {
            continue;
        };
        let Some(arr_def) = defs.get(&arr_ty) else {
            continue;
        };
        if arr_def.class.opcode != Op::TypeArray {
            continue;
        }
        let (Some(Operand::IdRef(struct_ty)), Some(Operand::IdRef(len_c))) =
            (arr_def.operands.first(), arr_def.operands.get(1))
        else {
            continue;
        };
        let Some(struct_def) = defs.get(struct_ty) else {
            continue;
        };
        if struct_def.class.opcode != Op::TypeStruct || struct_def.operands.len() != 1 {
            continue;
        }
        let Some(Operand::IdRef(field_ty)) = struct_def.operands.first() else {
            continue;
        };
        if direct_scalar_width(ctx, *field_ty).is_none() || const_u32(ctx, *len_c).is_none() {
            continue;
        }
        cands.push((var, *len_c, *field_ty));
    }

    for (var, len_c, field_ty) in cands {
        let Some(chains) = validate_single_field_struct_array_uses(
            ctx,
            &ctx.module.functions[entry_idx],
            var,
            field_ty,
            &ptr_info,
        ) else {
            continue;
        };

        let arr_ty = ctx.module.fresh_id();
        let new_var_ptr = ctx.module.fresh_id();
        let synthesized = vec![
            Instruction::new(
                Op::TypeArray,
                None,
                Some(arr_ty),
                vec![Operand::IdRef(field_ty), Operand::IdRef(len_c)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(new_var_ptr),
                vec![
                    Operand::StorageClass(StorageClass::Workgroup),
                    Operand::IdRef(arr_ty),
                ],
            ),
        ];
        let chain_ptr = ctx.ty_ptr(StorageClass::Workgroup, field_ty);
        let is_var = |i: &Instruction| i.class.opcode == Op::Variable && i.result_id == Some(var);
        if let Some(p) = ctx.module.types_global_values.iter().position(is_var) {
            ctx.module.types_global_values[p].result_type = Some(new_var_ptr);
            let tail = ctx.module.types_global_values.split_off(p);
            ctx.module.types_global_values.extend(synthesized);
            ctx.module.types_global_values.extend(tail);
        } else if let Some(p) = ctx.new_globals.iter().position(is_var) {
            ctx.new_globals[p].result_type = Some(new_var_ptr);
            let tail = ctx.new_globals.split_off(p);
            ctx.new_globals.extend(synthesized);
            ctx.new_globals.extend(tail);
        } else {
            continue;
        }

        for &(bi, ii) in &chains {
            let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
            inst.result_type = Some(chain_ptr);
            inst.operands.truncate(2);
        }
    }
}

fn validate_single_field_struct_array_uses(
    ctx: &Ctx,
    func: &Function,
    var: Word,
    field_ty: Word,
    ptr_info: &HashMap<Word, (StorageClass, Word)>,
) -> Option<Vec<(usize, usize)>> {
    let mut chains = Vec::new();
    let mut chain_ids = HashSet::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            let mentions_var = inst
                .operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(id) if *id == var));
            if !mentions_var {
                continue;
            }
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                || inst.operands.len() != 3
                || operand_id(inst, 0) != Some(var)
                || operand_id(inst, 2).and_then(|id| const_u32(ctx, id)) != Some(0)
                || inst.result_type.and_then(|ty| ptr_info.get(&ty).copied())
                    != Some((StorageClass::Workgroup, field_ty))
            {
                return None;
            }
            chains.push((bi, ii));
            chain_ids.insert(inst.result_id?);
        }
    }
    if chains.is_empty() {
        return None;
    }

    for block in &func.blocks {
        for inst in &block.instructions {
            if inst
                .result_id
                .is_some_and(|result| chain_ids.contains(&result))
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if operand_id(inst, 0).is_some_and(|ptr| chain_ids.contains(&ptr))
                        && inst.result_type != Some(field_ty)
                    {
                        return None;
                    }
                }
                Op::Store => {
                    if operand_id(inst, 0).is_some_and(|ptr| chain_ids.contains(&ptr))
                        && operand_id(inst, 1).and_then(|value| value_result_type(ctx, value))
                            != Some(field_ty)
                    {
                        return None;
                    }
                }
                op if is_atomic_pointer_op(op) => {
                    if !operand_id(inst, 0).is_some_and(|ptr| chain_ids.contains(&ptr))
                        && inst
                            .operands
                            .iter()
                            .any(|o| matches!(o, Operand::IdRef(id) if chain_ids.contains(id)))
                    {
                        return None;
                    }
                }
                _ => {
                    if inst
                        .operands
                        .iter()
                        .any(|o| matches!(o, Operand::IdRef(id) if chain_ids.contains(id)))
                    {
                        return None;
                    }
                }
            }
        }
    }
    Some(chains)
}

pub(in crate::passes) fn is_atomic_pointer_op(op: Op) -> bool {
    matches!(
        op,
        Op::AtomicLoad
            | Op::AtomicStore
            | Op::AtomicExchange
            | Op::AtomicCompareExchange
            | Op::AtomicCompareExchangeWeak
            | Op::AtomicIIncrement
            | Op::AtomicIDecrement
            | Op::AtomicIAdd
            | Op::AtomicISub
            | Op::AtomicSMin
            | Op::AtomicUMin
            | Op::AtomicSMax
            | Op::AtomicUMax
            | Op::AtomicAnd
            | Op::AtomicOr
            | Op::AtomicXor
    )
}

pub(in crate::passes) fn existing_scalar_type(ctx: &Ctx, opcode: Op) -> Option<Word> {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .find(|inst| {
            inst.class.opcode == opcode && inst.operands.first() == Some(&Operand::LiteralBit32(32))
        })
        .and_then(|inst| inst.result_id)
}

pub(in crate::passes) struct WgFloatAtomicPlan {
    pub(in crate::passes) var: Word,
    pub(in crate::passes) k: u32,
    pub(in crate::passes) int_ty: Word,
    pub(in crate::passes) chain_ids: HashSet<Word>,
    pub(in crate::passes) chains: Vec<(usize, usize)>,
    pub(in crate::passes) bitcast_ids: HashSet<Word>,
    pub(in crate::passes) bitcast_to_chain: HashMap<Word, Word>,
}

pub(in crate::passes) fn remodel_workgroup_floatarray_atomic_as_uint(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let Some(float_ty) = existing_scalar_type(ctx, Op::TypeFloat) else {
        return Ok(());
    };

    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(result) = inst.result_id {
            defs.insert(result, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let int32_types: HashSet<Word> = defs
        .values()
        .filter(|inst| {
            inst.class.opcode == Op::TypeInt
                && inst.operands.first() == Some(&Operand::LiteralBit32(32))
        })
        .filter_map(|inst| inst.result_id)
        .collect();

    let mut cands: Vec<(Word, u32)> = Vec::new();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode != Op::Variable {
            continue;
        }
        let (Some(var), Some(ptr_ty)) = (inst.result_id, inst.result_type) else {
            continue;
        };
        let Some(&(StorageClass::Workgroup, pointee)) = ptr_info.get(&ptr_ty) else {
            continue;
        };
        let Some(arr_def) = defs.get(&pointee) else {
            continue;
        };
        if arr_def.class.opcode != Op::TypeArray {
            continue;
        }
        let (Some(Operand::IdRef(elem_ty)), Some(Operand::IdRef(len_c))) =
            (arr_def.operands.first(), arr_def.operands.get(1))
        else {
            continue;
        };
        if *elem_ty != float_ty {
            continue;
        }
        let Some(k) = const_u32(ctx, *len_c) else {
            continue;
        };
        cands.push((var, k));
    }

    for (var, k) in cands {
        let Some(plan) = validate_wg_float_atomic(
            ctx,
            &ctx.module.functions[entry_idx],
            var,
            k,
            float_ty,
            &int32_types,
            &ptr_info,
        ) else {
            continue;
        };

        let int_ty = plan.int_ty;
        let new_glob_start = ctx.new_globals.len();
        let arr_ty = ctx.ty_array(int_ty, plan.k);
        let new_var_ptr = ctx.ty_ptr(StorageClass::Workgroup, arr_ty);
        let chain_ptr_uint = ctx.ty_ptr(StorageClass::Workgroup, int_ty);
        let synthesized: Vec<Instruction> = ctx.new_globals.drain(new_glob_start..).collect();
        let is_var =
            |i: &Instruction| i.class.opcode == Op::Variable && i.result_id == Some(plan.var);
        if let Some(p) = ctx.module.types_global_values.iter().position(is_var) {
            ctx.module.types_global_values[p].result_type = Some(new_var_ptr);
            let tail = ctx.module.types_global_values.split_off(p);
            ctx.module.types_global_values.extend(synthesized);
            ctx.module.types_global_values.extend(tail);
        } else if let Some(p) = ctx.new_globals.iter().position(is_var) {
            ctx.new_globals[p].result_type = Some(new_var_ptr);
            let tail = ctx.new_globals.split_off(p);
            ctx.new_globals.extend(synthesized);
            ctx.new_globals.extend(tail);
        } else {
            ctx.new_globals.extend(synthesized);
            continue;
        }

        for &(bi, ii) in &plan.chains {
            ctx.module.functions[entry_idx].blocks[bi].instructions[ii].result_type =
                Some(chain_ptr_uint);
        }

        let n_blocks = ctx.module.functions[entry_idx].blocks.len();
        for bi in 0..n_blocks {
            let insts = ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .clone();
            let mut out = Vec::with_capacity(insts.len());
            for mut inst in insts {
                if inst.class.opcode == Op::Bitcast
                    && inst
                        .result_id
                        .map(|r| plan.bitcast_ids.contains(&r))
                        .unwrap_or(false)
                {
                    continue;
                }
                if inst.class.opcode == Op::Load {
                    if let Some(ptr) = operand_id(&inst, 0) {
                        if plan.chain_ids.contains(&ptr) && inst.result_type != Some(int_ty) {
                            let (rt, rid) = (
                                inst.result_type
                                    .ok_or("wg float atomic: load missing result type")?,
                                inst.result_id
                                    .ok_or("wg float atomic: load missing result id")?,
                            );
                            let tmp = ctx.module.fresh_id();
                            out.push(Instruction::new(
                                Op::Load,
                                Some(int_ty),
                                Some(tmp),
                                vec![Operand::IdRef(ptr)],
                            ));
                            out.push(Instruction::new(
                                Op::Bitcast,
                                Some(rt),
                                Some(rid),
                                vec![Operand::IdRef(tmp)],
                            ));
                            continue;
                        }
                    }
                }
                if inst.class.opcode == Op::Store {
                    if let Some(ptr) = operand_id(&inst, 0) {
                        if plan.chain_ids.contains(&ptr) {
                            let fval = operand_id(&inst, 1)
                                .ok_or("wg float atomic: store missing value operand")?;
                            let stored_int = value_result_type(ctx, fval) == Some(int_ty);
                            if !stored_int {
                                let tmp = ctx.module.fresh_id();
                                out.push(Instruction::new(
                                    Op::Bitcast,
                                    Some(int_ty),
                                    Some(tmp),
                                    vec![Operand::IdRef(fval)],
                                ));
                                out.push(Instruction::new(
                                    Op::Store,
                                    None,
                                    None,
                                    vec![Operand::IdRef(ptr), Operand::IdRef(tmp)],
                                ));
                                continue;
                            }
                        }
                    }
                }
                for op in inst.operands.iter_mut() {
                    if let Operand::IdRef(id) = op {
                        if let Some(&c) = plan.bitcast_to_chain.get(id) {
                            *op = Operand::IdRef(c);
                        }
                    }
                }
                out.push(inst);
            }
            ctx.module.functions[entry_idx].blocks[bi].instructions = out;
        }
    }
    Ok(())
}

pub(in crate::passes) fn validate_wg_float_atomic(
    ctx: &Ctx,
    func: &Function,
    var: Word,
    k: u32,
    float_ty: Word,
    int32_types: &HashSet<Word>,
    ptr_info: &HashMap<Word, (StorageClass, Word)>,
) -> Option<WgFloatAtomicPlan> {
    let mut chains: Vec<(usize, usize)> = Vec::new();
    let mut chain_ids: HashSet<Word> = HashSet::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            let mentions_var = inst
                .operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(id) if *id == var));
            if !mentions_var {
                continue;
            }
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
                || inst.operands.len() != 2
                || operand_id(inst, 0) != Some(var)
            {
                return None;
            }
            let result_ty = inst.result_type?;
            if ptr_info.get(&result_ty) != Some(&(StorageClass::Workgroup, float_ty)) {
                return None;
            }
            chains.push((bi, ii));
            chain_ids.insert(inst.result_id?);
        }
    }
    if chains.is_empty() {
        return None;
    }

    let mut bitcast_ids: HashSet<Word> = HashSet::default();
    let mut bitcast_to_chain: HashMap<Word, Word> = HashMap::default();
    let mut int_ty: Option<Word> = None;
    for block in &func.blocks {
        for inst in &block.instructions {
            if inst.class.opcode != Op::Bitcast {
                continue;
            }
            let Some(src) = operand_id(inst, 0) else {
                continue;
            };
            if !chain_ids.contains(&src) {
                continue;
            }
            let rt = inst.result_type?;
            let &(sc, pointee) = ptr_info.get(&rt)?;
            if sc != StorageClass::Workgroup || !int32_types.contains(&pointee) {
                return None;
            }
            if *int_ty.get_or_insert(pointee) != pointee {
                return None;
            }
            let cu = inst.result_id?;
            bitcast_ids.insert(cu);
            bitcast_to_chain.insert(cu, src);
        }
    }
    let int_ty = int_ty?;
    if bitcast_ids.is_empty() {
        return None;
    }

    for block in &func.blocks {
        for inst in &block.instructions {
            if inst
                .result_id
                .map(|r| chain_ids.contains(&r))
                .unwrap_or(false)
                && matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain)
            {
                continue;
            }
            if inst.class.opcode == Op::Bitcast
                && inst
                    .result_id
                    .map(|r| bitcast_ids.contains(&r))
                    .unwrap_or(false)
            {
                continue;
            }
            match inst.class.opcode {
                Op::Load => {
                    if let Some(ptr) = operand_id(inst, 0) {
                        if chain_ids.contains(&ptr)
                            && inst.result_type != Some(float_ty)
                            && inst.result_type != Some(int_ty)
                        {
                            return None;
                        }
                    }
                }
                Op::Store => {
                    if let Some(ptr) = operand_id(inst, 0) {
                        if chain_ids.contains(&ptr) {
                            let obj_ty =
                                operand_id(inst, 1).and_then(|o| value_result_type(ctx, o));
                            if obj_ty != Some(float_ty) && obj_ty != Some(int_ty) {
                                return None;
                            }
                        }
                    }
                }
                _ => {
                    if inst
                        .operands
                        .iter()
                        .any(|o| matches!(o, Operand::IdRef(id) if chain_ids.contains(id)))
                    {
                        return None;
                    }
                }
            }
        }
    }

    for block in &func.blocks {
        for inst in &block.instructions {
            if inst.class.opcode == Op::Bitcast
                && inst
                    .result_id
                    .map(|r| bitcast_ids.contains(&r))
                    .unwrap_or(false)
            {
                continue;
            }
            let references_bitcast = inst
                .operands
                .iter()
                .any(|o| matches!(o, Operand::IdRef(id) if bitcast_ids.contains(id)));
            if !references_bitcast {
                continue;
            }
            let pointer_is_bitcast =
                matches!(operand_id(inst, 0), Some(id) if bitcast_ids.contains(&id));
            let ok = match inst.class.opcode {
                op if is_atomic_pointer_op(op) => pointer_is_bitcast,
                Op::Load => pointer_is_bitcast && inst.result_type == Some(int_ty),
                Op::Store => {
                    pointer_is_bitcast
                        && operand_id(inst, 1).and_then(|o| value_result_type(ctx, o))
                            == Some(int_ty)
                }
                _ => false,
            };
            if !ok {
                return None;
            }
        }
    }

    Some(WgFloatAtomicPlan {
        var,
        k,
        int_ty,
        chain_ids,
        chains,
        bitcast_ids,
        bitcast_to_chain,
    })
}

pub(in crate::passes) fn vector_lanes_of(ctx: &Ctx, ty: Word, elem: Word) -> Option<u32> {
    if ty == elem {
        return Some(1);
    }
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeVector {
        return None;
    }
    let Some(Operand::IdRef(e)) = def.operands.first() else {
        return None;
    };
    if *e != elem {
        return None;
    }
    match def.operands.get(1) {
        Some(Operand::LiteralBit32(n)) => Some(*n),
        _ => None,
    }
}
