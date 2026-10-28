use super::*;

pub(in crate::passes) struct PrivAggAccess {
    pub(in crate::passes) remove: Vec<(usize, usize)>,
    pub(in crate::passes) anchor: (usize, usize),
    pub(in crate::passes) is_store: bool,
    pub(in crate::passes) store_val: Word,
    pub(in crate::passes) load_result: Word,
    pub(in crate::passes) load_result_ty: Word,
    pub(in crate::passes) var: Word,
    pub(in crate::passes) elem_ty: Word,
    pub(in crate::passes) ptr_elem_ty: Word,
    pub(in crate::passes) start_elem: u32,
    pub(in crate::passes) lanes: u32,
}

pub(in crate::passes) fn lower_private_byte_aggregate_reinterpret(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(r) = inst.result_id {
            defs.insert(r, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut def_pos: HashMap<Word, (usize, usize)> = HashMap::default();
    let mut users: HashMap<Word, Vec<(usize, usize)>> = HashMap::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if let Some(r) = inst.result_id {
                def_pos.insert(r, (bi, ii));
            }
            for op in &inst.operands {
                if let Operand::IdRef(id) = op {
                    users.entry(*id).or_default().push((bi, ii));
                }
            }
        }
    }

    let mut accesses: Vec<PrivAggAccess> = Vec::new();
    let mut need_len: HashMap<Word, (u32, Word)> = HashMap::default();

    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::PtrAccessChain {
                continue;
            }
            let Some(rp) = inst.result_id else {
                continue;
            };
            let Some(rpt) = inst.result_type else {
                continue;
            };
            let Some(&(storage, pac_pointee)) = ptr_info.get(&rpt) else {
                continue;
            };
            if storage != StorageClass::Private {
                continue;
            }
            let Some(pw) = direct_scalar_width(ctx, pac_pointee) else {
                continue;
            };
            if inst.operands.len() != 2 {
                continue;
            }
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let Some(Operand::IdRef(c_id)) = inst.operands.get(1) else {
                continue;
            };
            let Some(c) = const_u32(ctx, *c_id) else {
                continue;
            };
            let Some(&(bbi, bii)) = def_pos.get(base) else {
                continue;
            };
            let bdef = &ctx.module.functions[entry_idx].blocks[bbi].instructions[bii];
            if !matches!(bdef.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(bpt) = bdef.result_type else {
                continue;
            };
            let Some(&(bstorage, elem_ty)) = ptr_info.get(&bpt) else {
                continue;
            };
            if bstorage != StorageClass::Private {
                continue;
            }
            let Some(elem_bits) = direct_scalar_width(ctx, elem_ty) else {
                continue;
            };
            if pw == 0 || pw % 8 != 0 || elem_bits == 0 || pw >= elem_bits {
                continue;
            }
            if bdef.operands.len() != 2 {
                continue;
            }
            let Some(Operand::IdRef(var)) = bdef.operands.first() else {
                continue;
            };
            let Some(Operand::IdRef(e0_id)) = bdef.operands.get(1) else {
                continue;
            };
            let Some(e0) = const_u32(ctx, *e0_id) else {
                continue;
            };
            let Some(vdef) = defs.get(var) else {
                continue;
            };
            if vdef.class.opcode != Op::Variable {
                continue;
            }
            let Some(vpt) = vdef.result_type else {
                continue;
            };
            let Some(&(vstorage, varr)) = ptr_info.get(&vpt) else {
                continue;
            };
            if vstorage != StorageClass::Private {
                continue;
            }
            let Some(arr_def) = defs.get(&varr) else {
                continue;
            };
            if arr_def.class.opcode != Op::TypeArray {
                continue;
            }
            let (Some(Operand::IdRef(arr_elem)), Some(Operand::IdRef(_len_c))) =
                (arr_def.operands.first(), arr_def.operands.get(1))
            else {
                continue;
            };
            if *arr_elem != elem_ty {
                continue;
            }
            let elem_bytes = elem_bits / 8;
            let byteoff = c * (pw / 8);
            if byteoff % elem_bytes != 0 {
                continue;
            }
            let start_elem = e0 + byteoff / elem_bytes;
            let Some(rp_users) = users.get(&rp) else {
                continue;
            };
            if rp_users.len() != 1 {
                continue;
            }
            let (ubi, uii) = rp_users[0];
            let udef = &ctx.module.functions[entry_idx].blocks[ubi].instructions[uii];
            if udef.class.opcode == Op::Store {
                let Some(Operand::IdRef(sptr)) = udef.operands.first() else {
                    continue;
                };
                if *sptr != rp {
                    continue;
                }
                let Some(Operand::IdRef(val)) = udef.operands.get(1) else {
                    continue;
                };
                let Some(vty) = value_result_type(ctx, *val) else {
                    continue;
                };
                let Some(lanes) = vector_lanes_of(ctx, vty, elem_ty) else {
                    continue;
                };
                let entry = need_len.entry(*var).or_insert((0, elem_ty));
                entry.0 = entry.0.max(start_elem + lanes);
                accesses.push(PrivAggAccess {
                    remove: vec![(bi, ii)],
                    anchor: (ubi, uii),
                    is_store: true,
                    store_val: *val,
                    load_result: 0,
                    load_result_ty: 0,
                    var: *var,
                    elem_ty,
                    ptr_elem_ty: bpt,
                    start_elem,
                    lanes,
                });
            } else if udef.class.opcode == Op::Bitcast {
                let Some(vp) = udef.result_id else {
                    continue;
                };
                let Some(vpt2) = udef.result_type else {
                    continue;
                };
                let Some(&(vp_storage, vec_pointee)) = ptr_info.get(&vpt2) else {
                    continue;
                };
                if vp_storage != StorageClass::Private {
                    continue;
                }
                let Some(lanes) = vector_lanes_of(ctx, vec_pointee, elem_ty) else {
                    continue;
                };
                let Some(vp_users) = users.get(&vp) else {
                    continue;
                };
                if vp_users.len() != 1 {
                    continue;
                }
                let (lbi, lii) = vp_users[0];
                let ldef = &ctx.module.functions[entry_idx].blocks[lbi].instructions[lii];
                if ldef.class.opcode != Op::Load {
                    continue;
                }
                let Some(Operand::IdRef(lptr)) = ldef.operands.first() else {
                    continue;
                };
                if *lptr != vp {
                    continue;
                }
                let (Some(lres), Some(lty)) = (ldef.result_id, ldef.result_type) else {
                    continue;
                };
                if lty != vec_pointee {
                    continue;
                }
                let entry = need_len.entry(*var).or_insert((0, elem_ty));
                entry.0 = entry.0.max(start_elem + lanes);
                accesses.push(PrivAggAccess {
                    remove: vec![(bi, ii), (ubi, uii)],
                    anchor: (lbi, lii),
                    is_store: false,
                    store_val: 0,
                    load_result: lres,
                    load_result_ty: lty,
                    var: *var,
                    elem_ty,
                    ptr_elem_ty: bpt,
                    start_elem,
                    lanes,
                });
            }
        }
    }

    if accesses.is_empty() {
        return Ok(());
    }

    for (&var, &(needed, elem_ty)) in &need_len {
        let Some(vdef) = defs.get(&var) else {
            continue;
        };
        let Some(vpt) = vdef.result_type else {
            continue;
        };
        let Some(&(_, varr)) = ptr_info.get(&vpt) else {
            continue;
        };
        let Some(arr_def) = defs.get(&varr) else {
            continue;
        };
        let Some(Operand::IdRef(len_c)) = arr_def.operands.get(1) else {
            continue;
        };
        let Some(n) = const_u32(ctx, *len_c) else {
            continue;
        };
        if needed <= n {
            continue;
        }
        let new_glob_start = ctx.new_globals.len();
        let len_needed_c = ctx.const_uint(needed);
        let arr_ty = ctx.ty_array(elem_ty, needed);
        let new_var_ptr = ctx.ty_ptr(StorageClass::Private, arr_ty);
        let mut synthesized: Vec<Instruction> = ctx.new_globals.drain(new_glob_start..).collect();
        if let Some(p) = ctx
            .module
            .types_global_values
            .iter()
            .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
        {
            if let Some(q) = ctx
                .module
                .types_global_values
                .iter()
                .position(|i| i.result_id == Some(len_needed_c))
            {
                if q >= p {
                    let len_inst = ctx.module.types_global_values.remove(q);
                    synthesized.insert(0, len_inst);
                }
            }
            let p = ctx
                .module
                .types_global_values
                .iter()
                .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
                .ok_or("private byte aggregate: variable not found in globals")?;
            ctx.module.types_global_values[p].result_type = Some(new_var_ptr);
            let tail = ctx.module.types_global_values.split_off(p);
            ctx.module.types_global_values.extend(synthesized);
            ctx.module.types_global_values.extend(tail);
        } else if let Some(p) = ctx
            .new_globals
            .iter()
            .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
        {
            if let Some(q) = ctx
                .new_globals
                .iter()
                .position(|i| i.result_id == Some(len_needed_c))
            {
                if q >= p {
                    let len_inst = ctx.new_globals.remove(q);
                    synthesized.insert(0, len_inst);
                }
            }
            let p = ctx
                .new_globals
                .iter()
                .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
                .ok_or("private byte aggregate: variable not found in globals")?;
            ctx.new_globals[p].result_type = Some(new_var_ptr);
            let tail = ctx.new_globals.split_off(p);
            ctx.new_globals.extend(synthesized);
            ctx.new_globals.extend(tail);
        } else {
            ctx.new_globals.extend(synthesized);
        }
    }

    let mut remove: HashSet<(usize, usize)> = HashSet::default();
    let mut store_anchor: HashMap<(usize, usize), usize> = HashMap::default();
    let mut load_anchor: HashMap<(usize, usize), usize> = HashMap::default();
    for (ai, acc) in accesses.iter().enumerate() {
        for &r in &acc.remove {
            remove.insert(r);
        }
        if acc.is_store {
            store_anchor.insert(acc.anchor, ai);
        } else {
            load_anchor.insert(acc.anchor, ai);
        }
    }

    let nblocks = ctx.module.functions[entry_idx].blocks.len();
    for bi in 0..nblocks {
        let touched = (0..ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .len())
            .any(|ii| {
                remove.contains(&(bi, ii))
                    || store_anchor.contains_key(&(bi, ii))
                    || load_anchor.contains_key(&(bi, ii))
            });
        if !touched {
            continue;
        }
        let insts = ctx.module.functions[entry_idx].blocks[bi]
            .instructions
            .clone();
        let mut out: Vec<Instruction> = Vec::with_capacity(insts.len());
        for (ii, inst) in insts.into_iter().enumerate() {
            if remove.contains(&(bi, ii)) {
                continue;
            }
            if let Some(&ai) = store_anchor.get(&(bi, ii)) {
                let acc = &accesses[ai];
                for k in 0..acc.lanes {
                    let idx_c = ctx.const_uint(acc.start_elem + k);
                    let pk = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(acc.ptr_elem_ty),
                        Some(pk),
                        vec![Operand::IdRef(acc.var), Operand::IdRef(idx_c)],
                    ));
                    let ek = if acc.lanes == 1 {
                        acc.store_val
                    } else {
                        let id = ctx.module.fresh_id();
                        out.push(Instruction::new(
                            Op::CompositeExtract,
                            Some(acc.elem_ty),
                            Some(id),
                            vec![Operand::IdRef(acc.store_val), Operand::LiteralBit32(k)],
                        ));
                        id
                    };
                    out.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(pk), Operand::IdRef(ek)],
                    ));
                }
                continue;
            }
            if let Some(&ai) = load_anchor.get(&(bi, ii)) {
                let acc = &accesses[ai];
                let mut elems: Vec<Operand> = Vec::with_capacity(acc.lanes as usize);
                for k in 0..acc.lanes {
                    let idx_c = ctx.const_uint(acc.start_elem + k);
                    let pk = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(acc.ptr_elem_ty),
                        Some(pk),
                        vec![Operand::IdRef(acc.var), Operand::IdRef(idx_c)],
                    ));
                    let lk = ctx.module.fresh_id();
                    out.push(Instruction::new(
                        Op::Load,
                        Some(acc.elem_ty),
                        Some(lk),
                        vec![Operand::IdRef(pk)],
                    ));
                    elems.push(Operand::IdRef(lk));
                }
                if acc.lanes == 1 {
                    out.push(Instruction::new(
                        Op::CopyObject,
                        Some(acc.load_result_ty),
                        Some(acc.load_result),
                        vec![elems[0].clone()],
                    ));
                } else {
                    out.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(acc.load_result_ty),
                        Some(acc.load_result),
                        elems,
                    ));
                }
                continue;
            }
            out.push(inst);
        }
        ctx.module.functions[entry_idx].blocks[bi].instructions = out;
    }
    Ok(())
}

pub(in crate::passes) fn retype_demoted_copymemory_placeholder(ctx: &mut Ctx, entry_idx: usize) {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    let mut defs: HashMap<Word, Instruction> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if let Some(r) = inst.result_id {
            defs.insert(r, inst.clone());
        }
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut use_count: HashMap<Word, usize> = HashMap::default();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            for op in &inst.operands {
                if let Operand::IdRef(id) = op {
                    *use_count.entry(*id).or_default() += 1;
                }
            }
        }
    }

    let mut plans: Vec<(Word, Word)> = Vec::new();
    for block in &ctx.module.functions[entry_idx].blocks {
        for inst in &block.instructions {
            if inst.class.opcode != Op::CopyMemory {
                continue;
            }
            let (Some(Operand::IdRef(target)), Some(Operand::IdRef(source))) =
                (inst.operands.first(), inst.operands.get(1))
            else {
                continue;
            };
            let (Some(tpt), Some(spt)) = (
                value_result_type(ctx, *target),
                value_result_type(ctx, *source),
            ) else {
                continue;
            };
            let (Some(&(t_storage, t_pointee)), Some(&(_, s_pointee))) =
                (ptr_info.get(&tpt), ptr_info.get(&spt))
            else {
                continue;
            };
            if t_pointee == s_pointee {
                continue;
            }
            if t_storage != StorageClass::Private {
                continue;
            }
            let Some(tdef) = defs.get(target) else {
                continue;
            };
            if tdef.class.opcode != Op::Variable {
                continue;
            }
            if use_count.get(target).copied().unwrap_or(0) != 1 {
                continue;
            }
            plans.push((*target, s_pointee));
        }
    }
    if plans.is_empty() {
        return;
    }

    for (var, new_pointee) in plans {
        let new_glob_start = ctx.new_globals.len();
        let new_ptr = ctx.ty_ptr(StorageClass::Private, new_pointee);
        let synthesized: Vec<Instruction> = ctx.new_globals.drain(new_glob_start..).collect();
        if let Some(p) = ctx
            .module
            .types_global_values
            .iter()
            .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
        {
            ctx.module.types_global_values[p].result_type = Some(new_ptr);
            ctx.module.types_global_values[p].operands =
                vec![Operand::StorageClass(StorageClass::Private)];
            let tail = ctx.module.types_global_values.split_off(p);
            ctx.module.types_global_values.extend(synthesized);
            ctx.module.types_global_values.extend(tail);
        } else if let Some(p) = ctx
            .new_globals
            .iter()
            .position(|i| i.class.opcode == Op::Variable && i.result_id == Some(var))
        {
            ctx.new_globals[p].result_type = Some(new_ptr);
            ctx.new_globals[p].operands = vec![Operand::StorageClass(StorageClass::Private)];
            let tail = ctx.new_globals.split_off(p);
            ctx.new_globals.extend(synthesized);
            ctx.new_globals.extend(tail);
        } else {
            ctx.new_globals.extend(synthesized);
        }
    }
}

pub(in crate::passes) fn retype_private_direct_memory_placeholders(ctx: &mut Ctx) {
    let definitions = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .filter_map(|instruction| {
            instruction
                .result_id
                .map(|result| (result, instruction.clone()))
        })
        .collect::<HashMap<_, _>>();
    let value_types = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .chain(ctx.module.functions.iter().flat_map(|function| {
            function
                .parameters
                .iter()
                .chain(function.blocks.iter().flat_map(|block| &block.instructions))
        }))
        .filter_map(|instruction| Some((instruction.result_id?, instruction.result_type?)))
        .collect::<HashMap<_, _>>();
    let variables = ctx
        .module
        .types_global_values
        .iter()
        .filter(|instruction| {
            instruction.class.opcode == Op::Variable
                && instruction.operands.first()
                    == Some(&Operand::StorageClass(StorageClass::Private))
        })
        .cloned()
        .collect::<Vec<_>>();
    let variable_ids = variables
        .iter()
        .filter_map(|variable| variable.result_id)
        .collect::<HashSet<_>>();
    let mut uses = HashMap::<Word, Vec<(Instruction, usize)>>::default();
    for instruction in ctx
        .module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
    {
        let mut references = HashMap::<Word, usize>::default();
        for id in instruction
            .operands
            .iter()
            .filter_map(|operand| match operand {
                Operand::IdRef(id) if variable_ids.contains(id) => Some(*id),
                _ => None,
            })
        {
            *references.entry(id).or_default() += 1;
        }
        for (variable, count) in references {
            uses.entry(variable)
                .or_default()
                .push((instruction.clone(), count));
        }
    }
    let mut plans = Vec::new();
    for variable in &variables {
        let (Some(variable_id), Some(pointer_ty), Some(Operand::IdRef(initializer))) = (
            variable.result_id,
            variable.result_type,
            variable.operands.get(1),
        ) else {
            continue;
        };
        let Some(old_pointee) = ptr_pointee(&definitions, pointer_ty) else {
            continue;
        };
        if !definitions.get(initializer).is_some_and(|instruction| {
            instruction.class.opcode == Op::ConstantNull
                && instruction.result_type == Some(old_pointee)
        }) {
            continue;
        }

        let mut object_ty = None;
        let mut saw_use = false;
        let mut compatible = true;
        for (instruction, references) in uses.get(&variable_id).into_iter().flatten() {
            saw_use = true;
            let inferred = match instruction.class.opcode {
                Op::Load
                    if *references == 1
                        && instruction.operands.len() == 1
                        && instruction.operands.first() == Some(&Operand::IdRef(variable_id)) =>
                {
                    instruction.result_type
                }
                Op::Store
                    if *references == 1
                        && instruction.operands.len() == 2
                        && instruction.operands.first() == Some(&Operand::IdRef(variable_id)) =>
                {
                    instruction
                        .operands
                        .get(1)
                        .and_then(|operand| match operand {
                            Operand::IdRef(object) => value_types.get(object).copied(),
                            _ => None,
                        })
                }
                _ => None,
            };
            let Some(inferred) = inferred else {
                compatible = false;
                break;
            };
            if object_ty.is_some_and(|known| known != inferred) {
                compatible = false;
                break;
            }
            object_ty = Some(inferred);
        }
        if compatible && saw_use {
            if let Some(object_ty) = object_ty.filter(|object_ty| *object_ty != old_pointee) {
                plans.push((variable_id, object_ty));
            }
        }
    }

    for (variable_id, object_ty) in plans {
        let Some(mut variable_pos) = ctx
            .module
            .types_global_values
            .iter()
            .position(|instruction| instruction.result_id == Some(variable_id))
        else {
            continue;
        };
        let pointer_ty = ctx
            .module
            .types_global_values
            .iter()
            .take(variable_pos)
            .find(|instruction| {
                instruction.class.opcode == Op::TypePointer
                    && instruction.operands
                        == [
                            Operand::StorageClass(StorageClass::Private),
                            Operand::IdRef(object_ty),
                        ]
            })
            .and_then(|instruction| instruction.result_id)
            .unwrap_or_else(|| {
                let pointer_ty = ctx.module.fresh_id();
                ctx.module.types_global_values.insert(
                    variable_pos,
                    type_inst(
                        Op::TypePointer,
                        pointer_ty,
                        vec![
                            Operand::StorageClass(StorageClass::Private),
                            Operand::IdRef(object_ty),
                        ],
                    ),
                );
                variable_pos += 1;
                ctx.struct_cache.insert(
                    (
                        Op::TypePointer,
                        None,
                        vec![
                            Operand::StorageClass(StorageClass::Private),
                            Operand::IdRef(object_ty),
                        ],
                    ),
                    pointer_ty,
                );
                pointer_ty
            });
        let initializer = ctx
            .module
            .types_global_values
            .iter()
            .take(variable_pos)
            .find(|instruction| {
                instruction.class.opcode == Op::ConstantNull
                    && instruction.result_type == Some(object_ty)
            })
            .and_then(|instruction| instruction.result_id)
            .unwrap_or_else(|| {
                let initializer = ctx.module.fresh_id();
                ctx.module.types_global_values.insert(
                    variable_pos,
                    Instruction::new(Op::ConstantNull, Some(object_ty), Some(initializer), vec![]),
                );
                variable_pos += 1;
                ctx.struct_cache
                    .insert((Op::ConstantNull, Some(object_ty), vec![]), initializer);
                initializer
            });
        let variable = &mut ctx.module.types_global_values[variable_pos];
        variable.result_type = Some(pointer_ty);
        variable.operands = vec![
            Operand::StorageClass(StorageClass::Private),
            Operand::IdRef(initializer),
        ];
    }
}

pub(in crate::passes) fn rewrite_strided_descent_access_chains(ctx: &mut Ctx, entry_idx: usize) {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(s)), Some(Operand::IdRef(p))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*s, *p));
            }
        }
    }

    let mut rewrites: Vec<(usize, usize)> = Vec::new();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if !matches!(inst.class.opcode, Op::InBoundsAccessChain | Op::AccessChain) {
                continue;
            }
            let Some(result_type) = inst.result_type else {
                continue;
            };
            let Some((storage, result_pointee)) = ptr_info.get(&result_type).copied() else {
                continue;
            };
            if !ptr_access_chain_allowed_storage(storage) {
                continue;
            }
            if inst.operands.len() < 3 {
                continue;
            }
            let Some(Operand::IdRef(base)) = inst.operands.first() else {
                continue;
            };
            let Some(base_ptr_ty) = value_result_type(ctx, *base) else {
                continue;
            };
            let Some(&(_, base_pointee)) = ptr_info.get(&base_ptr_ty) else {
                continue;
            };
            let indices = &inst.operands[1..];
            if walk_into_type(ctx, base_pointee, indices).is_some() {
                continue;
            }
            if walk_into_type(ctx, base_pointee, &indices[1..]) != Some(result_pointee) {
                continue;
            }
            rewrites.push((bi, ii));
        }
    }

    for (bi, ii) in rewrites {
        let inst = &mut ctx.module.functions[entry_idx].blocks[bi].instructions[ii];
        *inst = Instruction::new(
            Op::PtrAccessChain,
            inst.result_type,
            inst.result_id,
            inst.operands.clone(),
        );
    }
}

pub(in crate::passes) fn split_workgroup_ptr_access_chain_descent(
    ctx: &mut Ctx,
    _entry_idx: usize,
) {
    let mut ptr_info: HashMap<Word, (StorageClass, Word)> = HashMap::default();
    for inst in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if inst.class.opcode == Op::TypePointer {
            if let (Some(id), Some(Operand::StorageClass(storage)), Some(Operand::IdRef(pointee))) =
                (inst.result_id, inst.operands.first(), inst.operands.get(1))
            {
                ptr_info.insert(id, (*storage, *pointee));
            }
        }
    }

    enum Rewrite {
        ReorderSingletonArray {
            function_idx: usize,
            block_idx: usize,
            inst_idx: usize,
        },
        SplitStride {
            function_idx: usize,
            block_idx: usize,
            inst_idx: usize,
            base_ptr_type: Word,
            extra_zero_indices: usize,
            zero: Option<Operand>,
        },
    }

    let mut rewrites = Vec::new();
    for (function_idx, function) in ctx.module.functions.iter().enumerate() {
        for (block_idx, block) in function.blocks.iter().enumerate() {
            for (inst_idx, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::PtrAccessChain || inst.operands.len() < 3 {
                    continue;
                }
                let (Some(result_type), Some(Operand::IdRef(base))) =
                    (inst.result_type, inst.operands.first())
                else {
                    continue;
                };
                let Some((StorageClass::Workgroup, result_pointee)) =
                    ptr_info.get(&result_type).copied()
                else {
                    continue;
                };
                let Some(base_ptr_type) = value_result_type(ctx, *base) else {
                    continue;
                };
                let Some((StorageClass::Workgroup, base_pointee)) =
                    ptr_info.get(&base_ptr_type).copied()
                else {
                    continue;
                };
                if base_pointee == result_pointee {
                    continue;
                }
                if inst.operands.len() == 3
                    && is_module_variable(ctx, *base)
                    && singleton_array_element(ctx, base_pointee) == Some(result_pointee)
                    && matches!(
                        inst.operands.get(2),
                        Some(Operand::IdRef(id)) if const_u32(ctx, *id) == Some(0)
                    )
                    && walk_into_type(
                        ctx,
                        base_pointee,
                        &[inst.operands[2].clone(), inst.operands[1].clone()],
                    ) == Some(result_pointee)
                {
                    rewrites.push(Rewrite::ReorderSingletonArray {
                        function_idx,
                        block_idx,
                        inst_idx,
                    });
                    continue;
                }
                let Some(walked) = walk_into_type(ctx, base_pointee, &inst.operands[2..]) else {
                    continue;
                };
                let Some(extra_zero_indices) =
                    zero_descent_steps_to_type(ctx, walked, result_pointee)
                else {
                    continue;
                };
                let zero = inst.operands[2..]
                    .iter()
                    .find(|operand| match operand {
                        Operand::IdRef(id) => const_u32(ctx, *id) == Some(0),
                        _ => false,
                    })
                    .cloned();
                if extra_zero_indices > 0 && zero.is_none() {
                    continue;
                }
                rewrites.push(Rewrite::SplitStride {
                    function_idx,
                    block_idx,
                    inst_idx,
                    base_ptr_type,
                    extra_zero_indices,
                    zero,
                });
            }
        }
    }

    for rewrite in rewrites.into_iter().rev() {
        match rewrite {
            Rewrite::ReorderSingletonArray {
                function_idx,
                block_idx,
                inst_idx,
            } => {
                let inst = &mut ctx.module.functions[function_idx].blocks[block_idx].instructions
                    [inst_idx];
                inst.class.opcode = Op::InBoundsAccessChain;
                inst.operands.swap(1, 2);
            }
            Rewrite::SplitStride {
                function_idx,
                block_idx,
                inst_idx,
                base_ptr_type,
                extra_zero_indices,
                zero,
            } => {
                let original = ctx.module.functions[function_idx].blocks[block_idx].instructions
                    [inst_idx]
                    .clone();
                let strided = ctx.module.fresh_id();
                let stride = Instruction::new(
                    Op::PtrAccessChain,
                    Some(base_ptr_type),
                    Some(strided),
                    original.operands[..2].to_vec(),
                );
                let mut descent_operands = vec![Operand::IdRef(strided)];
                descent_operands.extend_from_slice(&original.operands[2..]);
                if let Some(zero) = zero {
                    descent_operands.extend(std::iter::repeat_n(zero, extra_zero_indices));
                }
                let descent = Instruction::new(
                    Op::InBoundsAccessChain,
                    original.result_type,
                    original.result_id,
                    descent_operands,
                );
                let block = &mut ctx.module.functions[function_idx].blocks[block_idx];
                block.instructions[inst_idx] = descent;
                block.instructions.insert(inst_idx, stride);
            }
        }
    }
}

fn is_module_variable(ctx: &Ctx, id: Word) -> bool {
    ctx.new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .any(|inst| inst.class.opcode == Op::Variable && inst.result_id == Some(id))
}

fn singleton_array_element(ctx: &Ctx, ty: Word) -> Option<Word> {
    let structure = type_def_of(ctx, ty)?;
    if structure.class.opcode != Op::TypeStruct || structure.operands.len() != 1 {
        return None;
    }
    let Operand::IdRef(member) = structure.operands[0] else {
        return None;
    };
    let array = type_def_of(ctx, member)?;
    if !matches!(array.class.opcode, Op::TypeArray | Op::TypeRuntimeArray) {
        return None;
    }
    match array.operands.first()? {
        Operand::IdRef(element) => Some(*element),
        _ => None,
    }
}

fn zero_descent_steps_to_type(ctx: &Ctx, mut current: Word, target: Word) -> Option<usize> {
    for steps in 0..=8 {
        if current == target {
            return Some(steps);
        }
        let definition = type_def_of(ctx, current)?;
        current = match definition.class.opcode {
            Op::TypeStruct
            | Op::TypeArray
            | Op::TypeRuntimeArray
            | Op::TypeVector
            | Op::TypeMatrix => match definition.operands.first()? {
                Operand::IdRef(element) => *element,
                _ => return None,
            },
            _ => return None,
        };
    }
    None
}

pub(in crate::passes) fn direct_scalar_width(ctx: &Ctx, ty: Word) -> Option<u32> {
    type_def_of(ctx, ty).and_then(|d| match d.class.opcode {
        Op::TypeInt | Op::TypeFloat => match d.operands.first() {
            Some(Operand::LiteralBit32(b)) => Some(*b),
            _ => None,
        },
        _ => None,
    })
}

pub(in crate::passes) fn rewrite_thread_local_aggregate_prefix_stores(
    ctx: &mut Ctx,
    function_idx: usize,
) {
    let value_types = function_value_types(ctx, function_idx);
    let mut pointer_types = HashMap::default();
    for instruction in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if instruction.class.opcode != Op::TypePointer {
            continue;
        }
        if let (
            Some(pointer_ty),
            Some(Operand::StorageClass(storage)),
            Some(Operand::IdRef(pointee)),
        ) = (
            instruction.result_id,
            instruction.operands.first(),
            instruction.operands.get(1),
        ) {
            pointer_types.insert(pointer_ty, (*storage, *pointee));
        }
    }

    #[derive(Clone)]
    struct Plan {
        pointer: Word,
        object: Word,
        object_ty: Word,
        leaf_ty: Word,
        storage: StorageClass,
        depth: usize,
        trailing_operands: Vec<Operand>,
    }

    let first_scalar = |mut ty: Word| {
        for depth in 0..=8 {
            if direct_scalar_width(ctx, ty).is_some() {
                return Some((ty, depth));
            }
            let definition = type_def_of(ctx, ty)?;
            if !matches!(
                definition.class.opcode,
                Op::TypeStruct
                    | Op::TypeArray
                    | Op::TypeRuntimeArray
                    | Op::TypeVector
                    | Op::TypeMatrix
            ) {
                return None;
            }
            let Operand::IdRef(first) = definition.operands.first()? else {
                return None;
            };
            ty = *first;
        }
        None
    };

    let mut plans = HashMap::default();
    for (block_idx, block) in ctx.module.functions[function_idx].blocks.iter().enumerate() {
        for (instruction_idx, instruction) in block.instructions.iter().enumerate() {
            if instruction.class.opcode != Op::Store || instruction.operands.len() < 2 {
                continue;
            }
            let (Operand::IdRef(pointer), Operand::IdRef(object)) =
                (&instruction.operands[0], &instruction.operands[1])
            else {
                continue;
            };
            let Some(pointer_ty) = value_types.get(pointer).copied() else {
                continue;
            };
            let Some(&(storage, pointee)) = pointer_types.get(&pointer_ty) else {
                continue;
            };
            if !matches!(storage, StorageClass::Private | StorageClass::Function) {
                continue;
            }
            let Some(object_ty) = value_types.get(object).copied() else {
                continue;
            };
            let Some((leaf_ty, depth)) = first_scalar(pointee) else {
                continue;
            };
            if depth == 0
                || direct_scalar_width(ctx, object_ty) != direct_scalar_width(ctx, leaf_ty)
            {
                continue;
            }
            plans.insert(
                (block_idx, instruction_idx),
                Plan {
                    pointer: *pointer,
                    object: *object,
                    object_ty,
                    leaf_ty,
                    storage,
                    depth,
                    trailing_operands: instruction.operands[2..].to_vec(),
                },
            );
        }
    }

    let zero = ctx.const_uint(0);
    for block_idx in 0..ctx.module.functions[function_idx].blocks.len() {
        let old = ctx.module.functions[function_idx].blocks[block_idx]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(old.len());
        for (instruction_idx, instruction) in old.into_iter().enumerate() {
            let Some(plan) = plans.get(&(block_idx, instruction_idx)) else {
                rewritten.push(instruction);
                continue;
            };
            let pointer_ty = ctx.ty_ptr(plan.storage, plan.leaf_ty);
            let leaf_pointer = ctx.module.fresh_id();
            let mut operands = vec![Operand::IdRef(plan.pointer)];
            operands.extend((0..plan.depth).map(|_| Operand::IdRef(zero)));
            rewritten.push(Instruction::new(
                Op::InBoundsAccessChain,
                Some(pointer_ty),
                Some(leaf_pointer),
                operands,
            ));
            let value = if plan.object_ty == plan.leaf_ty {
                plan.object
            } else {
                let value = ctx.module.fresh_id();
                rewritten.push(Instruction::new(
                    Op::Bitcast,
                    Some(plan.leaf_ty),
                    Some(value),
                    vec![Operand::IdRef(plan.object)],
                ));
                value
            };
            let mut operands = vec![Operand::IdRef(leaf_pointer), Operand::IdRef(value)];
            operands.extend(plan.trailing_operands.iter().cloned());
            rewritten.push(Instruction::new(Op::Store, None, None, operands));
        }
        ctx.module.functions[function_idx].blocks[block_idx].instructions = rewritten;
    }
}
