use super::*;

pub(crate) const ADDRESS_TABLE_SELF_BASE: u32 = 32;
pub(crate) const TEXTURE_INDEX_BASE: u32 = 64;
pub(crate) fn bindless_all_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("NVMTL_NO_BINDLESS_ALL").as_deref(), Ok(v) if !v.is_empty() && v != "0"))
}
fn existing_address_table(ctx: &mut Ctx) -> Option<(Word, Word, Option<Word>)> {
    let layout = ctx.descriptor_layout;
    let want = layout.synthetic.start;
    let ann = &ctx.module.annotations;
    let deco = |target: Word, d: Decoration, v: u32| {
        ann.iter().any(|i| {
            i.class.opcode == Op::Decorate
                && i.operands.first() == Some(&Operand::IdRef(target))
                && i.operands.get(1) == Some(&Operand::Decoration(d))
                && i.operands.get(2) == Some(&Operand::LiteralBit32(v))
        })
    };
    let globals: Vec<Instruction> = ctx
        .module
        .types_global_values
        .iter()
        .chain(ctx.new_globals.iter())
        .cloned()
        .collect();
    let def = |id: Word| globals.iter().find(|i| i.result_id == Some(id)).cloned();
    for v in globals.iter().filter(|i| i.class.opcode == Op::Variable) {
        let Some(var) = v.result_id else { continue };
        if !deco(var, Decoration::DescriptorSet, layout.set)
            || !deco(var, Decoration::Binding, want)
        {
            continue;
        }
        let ptr = def(v.result_type?)?;
        let Some(Operand::IdRef(block)) = ptr.operands.get(1) else {
            return None;
        };
        let st = def(*block)?;
        let Some(Operand::IdRef(arr)) = st.operands.first() else {
            return None;
        };
        let ra = def(*arr)?;
        if ra.class.opcode != Op::TypeRuntimeArray {
            return None;
        }
        let Some(Operand::IdRef(elem)) = ra.operands.first() else {
            return None;
        };
        let e = def(*elem)?;
        return match (e.class.opcode, e.operands.as_slice()) {
            (Op::TypeVector, [_, Operand::LiteralBit32(2)]) => {
                let u = ctx.ty_uint();
                Some((var, ctx.ty_ptr(StorageClass::StorageBuffer, u), None))
            }
            (Op::TypeInt, [Operand::LiteralBit32(64), _]) => Some((
                var,
                ctx.ty_ptr(StorageClass::StorageBuffer, *elem),
                Some(*elem),
            )),
            _ => None,
        };
    }
    None
}
pub(in crate::passes) fn address_table_usable(ctx: &mut Ctx) -> bool {
    if ctx.address_table.is_some() {
        return true;
    }
    let layout = ctx.descriptor_layout;
    let occupied = crate::spirv_module::descriptor_bindings_in_set(&ctx.module, layout.set);
    !occupied.contains(&layout.synthetic.start) || existing_address_table(ctx).is_some()
}
pub(in crate::passes) fn ensure_address_table(
    ctx: &mut Ctx,
) -> Result<(Word, Word, Option<Word>), String> {
    if let Some(t) = ctx.address_table {
        return Ok(t);
    }
    if let Some(t) = existing_address_table(ctx) {
        ctx.address_table = Some(t);
        return Ok(t);
    }
    let t = declare_uvec2_address_table(ctx)?;
    ctx.address_table = Some(t);
    Ok(t)
}
fn declare_uvec2_address_table(ctx: &mut Ctx) -> Result<(Word, Word, Option<Word>), String> {
    let vec_ty = ctx.ty_vec_uint(2);
    let uint_ty = ctx.ty_uint();
    let array_ty = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeRuntimeArray,
        array_ty,
        vec![Operand::IdRef(vec_ty)],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(array_ty),
            Operand::Decoration(Decoration::ArrayStride),
            Operand::LiteralBit32(std::mem::size_of::<u64>() as u32),
        ],
    ));
    let block_ty = ctx.module.fresh_id();
    ctx.new_globals.push(type_inst(
        Op::TypeStruct,
        block_ty,
        vec![Operand::IdRef(array_ty)],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::MemberDecorate,
        None,
        None,
        vec![
            Operand::IdRef(block_ty),
            Operand::LiteralBit32(0),
            Operand::Decoration(Decoration::Offset),
            Operand::LiteralBit32(0),
        ],
    ));
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(block_ty),
            Operand::Decoration(Decoration::Block),
        ],
    ));
    let block_ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, block_ty);
    let uint_ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, uint_ty);
    let table = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(block_ptr_ty),
        Some(table),
        vec![Operand::StorageClass(StorageClass::StorageBuffer)],
    ));
    let layout = ctx.descriptor_layout;
    let occupied = crate::spirv_module::descriptor_bindings_in_set(&ctx.module, layout.set);
    let binding = (layout.synthetic.start..layout.synthetic.end)
        .find(|b| !occupied.contains(b))
        .ok_or_else(|| "descriptor binding space exhausted for buffer-address table".to_string())?;
    decorate_binding(&mut ctx.module, table, layout.set, binding);
    ctx.interface_buffer_var(table);
    Ok((table, uint_ptr_ty, None))
}

/// Where a stage entry's parameter is bound, for the buffer-address table: a kernel's buffers and the buffers its
/// function constants locate, and a vertex or fragment function's buffers. Graphics stages reach this table when an
/// entry passes one of its buffers to a helper that takes a device address (VFX particle and Vista splat shaders).
pub(in crate::passes) struct BufferAddressRoles<'a> {
    pub(in crate::passes) kern: Option<&'a KernMeta>,
    pub(in crate::passes) vert: Option<&'a VertMeta>,
    pub(in crate::passes) frag: Option<&'a FragMeta>,
}

impl BufferAddressRoles<'_> {
    fn location(&self, param_index: u32) -> Option<u32> {
        if let Some(kern) = self.kern {
            return match kern.role_of(param_index) {
                Some(
                    KernRole::Buffer(location)
                    | KernRole::AccelerationStructureShadow(location)
                    | KernRole::PrimitiveAccelerationStructureShadow(location),
                ) => Some(*location),
                _ => kern.function_constant_buffer_locations.get(&param_index).copied(),
            };
        }
        if let Some(vert) = self.vert {
            return match vert.role_of(param_index) {
                Some(VertRole::Buffer(location)) => Some(*location),
                _ => None,
            };
        }
        match self.frag?.role_of(param_index) {
            Some(FragRole::Buffer(location)) => Some(*location),
            _ => None,
        }
    }

    fn type_name(&self, param_index: u32) -> Option<&str> {
        self.kern
            .and_then(|meta| meta.buffer_type_names.get(&param_index))
            .or_else(|| self.vert.and_then(|meta| meta.buffer_type_names.get(&param_index)))
            .or_else(|| self.frag.and_then(|meta| meta.buffer_type_names.get(&param_index)))
            .map(String::as_str)
    }
}

pub(in crate::passes) fn lower_buffer_address_facts(
    ctx: &mut Ctx,
    entry_idx: usize,
    roles: BufferAddressRoles<'_>,
) -> Result<(), String> {
    let address_words = ctx
        .emit_sidecar
        .buffer_address_words
        .iter()
        .map(|fact| (fact.id, (fact.param_index, fact.component)))
        .collect::<HashMap<_, _>>();
    if address_words.is_empty() {
        return Ok(());
    }
    if roles.kern.is_none() && roles.vert.is_none() && roles.frag.is_none() {
        return Err("buffer-address facts require stage metadata".to_string());
    }
    let mut locations = HashMap::default();
    for (id, (param_index, component)) in &address_words {
        let Some(location) = roles.location(*param_index) else {
            return Err(format!(
                "buffer-address fact {id} references a parameter without a buffer-backed resource {param_index}"
            ));
        };
        let location = if roles
            .type_name(*param_index)
            .is_some_and(|name| name.starts_with("array_ref<"))
        {
            ADDRESS_TABLE_SELF_BASE + location
        } else {
            location
        };
        locations.insert(*id, (location, *component));
    }

    let uint_ty = ctx.ty_uint();
    let shared = match ctx.address_table {
        Some((t, p, None)) => Some((t, p)),
        _ => None,
    };
    let (table, uint_ptr_ty) = if let Some(s) = shared {
        s
    } else {
        let vec_ty = ctx.ty_vec_uint(2);
        let array_ty = ctx.module.fresh_id();
        ctx.new_globals.push(type_inst(
            Op::TypeRuntimeArray,
            array_ty,
            vec![Operand::IdRef(vec_ty)],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(array_ty),
                Operand::Decoration(Decoration::ArrayStride),
                Operand::LiteralBit32(std::mem::size_of::<u64>() as u32),
            ],
        ));
        let block_ty = ctx.module.fresh_id();
        ctx.new_globals.push(type_inst(
            Op::TypeStruct,
            block_ty,
            vec![Operand::IdRef(array_ty)],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::MemberDecorate,
            None,
            None,
            vec![
                Operand::IdRef(block_ty),
                Operand::LiteralBit32(0),
                Operand::Decoration(Decoration::Offset),
                Operand::LiteralBit32(0),
            ],
        ));
        ctx.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(block_ty),
                Operand::Decoration(Decoration::Block),
            ],
        ));
        let block_ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, block_ty);
        let uint_ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, uint_ty);
        let table = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(block_ptr_ty),
            Some(table),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ));
        let layout = ctx.descriptor_layout;
        let occupied = crate::spirv_module::descriptor_bindings_in_set(&ctx.module, layout.set);
        let binding = (layout.synthetic.start..layout.synthetic.end)
            .find(|binding| !occupied.contains(binding))
            .ok_or_else(|| {
                "descriptor binding space exhausted for buffer-address table".to_string()
            })?;
        decorate_binding(&mut ctx.module, table, layout.set, binding);
        ctx.interface_buffer_var(table);
        if ctx.address_table.is_none() {
            ctx.address_table = Some((table, uint_ptr_ty, None));
        }
        (table, uint_ptr_ty)
    };

    let zero = ctx.const_uint(0);
    let mut distinct_locations = locations
        .values()
        .map(|(location, _)| *location)
        .collect::<Vec<_>>();
    distinct_locations.sort_unstable();
    distinct_locations.dedup();
    let location_constants = distinct_locations
        .into_iter()
        .map(|location| (location, ctx.const_uint(location)))
        .collect::<HashMap<_, _>>();
    let component_constants = [ctx.const_uint(0), ctx.const_uint(1)];
    let block_count = ctx.module.functions[entry_idx].blocks.len();
    for block_index in 0..block_count {
        let instructions = ctx.module.functions[entry_idx].blocks[block_index]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(instructions.len());
        for inst in instructions {
            let Some(result) = inst.result_id else {
                rewritten.push(inst);
                continue;
            };
            let Some((location, component)) = locations.get(&result).copied() else {
                rewritten.push(inst);
                continue;
            };
            let pointer = ctx.module.fresh_id();
            rewritten.push(Instruction::new(
                Op::AccessChain,
                Some(uint_ptr_ty),
                Some(pointer),
                vec![
                    Operand::IdRef(table),
                    Operand::IdRef(zero),
                    Operand::IdRef(location_constants[&location]),
                    Operand::IdRef(component_constants[component as usize]),
                ],
            ));
            rewritten.push(Instruction::new(
                Op::Load,
                Some(uint_ty),
                Some(result),
                vec![Operand::IdRef(pointer)],
            ));
        }
        ctx.module.functions[entry_idx].blocks[block_index].instructions = rewritten;
    }
    ctx.emit_sidecar.buffer_address_words.clear();
    Ok(())
}
