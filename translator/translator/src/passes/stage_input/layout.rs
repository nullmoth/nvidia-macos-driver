use super::*;

pub(super) fn include_existing_private_globals(ctx: &mut Ctx) {
    let vars: Vec<Word> = ctx
        .module
        .types_global_values
        .iter()
        .filter(|inst| {
            inst.class.opcode == Op::Variable
                && inst.operands.first() == Some(&Operand::StorageClass(StorageClass::Private))
        })
        .filter_map(|inst| inst.result_id)
        .collect();
    for var in vars {
        ctx.interface_buffer_var(var);
    }
}

impl Ctx {
    pub(in crate::passes) fn interface_buffer_var(&mut self, var: Word) {
        let v = self.module.header.as_ref().map(|h| h.version).unwrap_or(0);
        if v >= 0x0001_0400 {
            self.interface.push(var);
        }
    }

    pub(in crate::passes) fn descriptor_variable(
        &mut self,
        pointee_ty: Word,
        binding: u32,
    ) -> Word {
        let set = self.descriptor_layout.set;
        let pointer_ty = self.ty_ptr(StorageClass::UniformConstant, pointee_ty);
        let var = self.module.fresh_id();
        self.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pointer_ty),
            Some(var),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        decorate_binding(&mut self.module, var, set, binding);
        self.interface_buffer_var(var);
        var
    }

    fn placeholder_descriptor(&mut self, pointee_ty: Word, binding: u32) -> Word {
        let var = self.descriptor_variable(pointee_ty, binding);
        self.placeholder_descriptor_vars.insert(var, binding);
        var
    }

    pub(in crate::passes) fn default_read_sampler(&mut self) -> Result<Word, String> {
        if let Some(v) = self.default_sampler_var {
            return Ok(v);
        }
        let layout = self.descriptor_layout;
        let binding = allocate_static_sampler_binding(&self.module, layout)
            .ok_or_else(|| "no free sampler binding for synthesized read sampler".to_string())?;
        let sty = self.ty_sampler();
        let var = self.placeholder_descriptor(sty, binding);
        self.default_sampler_var = Some(var);
        Ok(var)
    }

    pub(in crate::passes) fn default_null_image_of(
        &mut self,
        dim: Dim,
        arrayed: bool,
    ) -> Result<Word, String> {
        if let Some(&v) = self.default_null_image_vars.get(&(dim, arrayed)) {
            return Ok(v);
        }
        let layout = self.descriptor_layout;
        let binding = allocate_default_texture_binding(&self.module, layout)
            .ok_or_else(|| "no free texture binding for synthesized null image".to_string())?;
        let img_ty = self.ty_image(dim, arrayed, ImageComp::Float);
        let var = self.placeholder_descriptor(img_ty, binding);
        self.default_null_image_vars.insert((dim, arrayed), var);
        Ok(var)
    }

    pub(in crate::passes) fn implicit_imageblock_var(
        &mut self,
        attachment: u32,
        data_rate: u32,
        format: ImageFormat,
        comp: ImageComp,
    ) -> Result<(Word, Word), String> {
        if data_rate > 2 {
            return Err(format!(
                "implicit imageblock attachment {attachment} has unknown data rate {data_rate}"
            ));
        }
        if let Some(&(var, image_ty, existing_format)) =
            self.implicit_imageblock_vars.get(&(attachment, data_rate))
        {
            if existing_format != format {
                return Err(format!(
                    "implicit imageblock attachment {attachment} rate {data_rate} is used with conflicting formats {existing_format:?} and {format:?}"
                ));
            }
            return Ok((var, image_ty));
        }
        let layout = self.descriptor_layout;
        let binding = layout.imageblock_binding(attachment, data_rate)
            .ok_or_else(|| {
                format!(
                    "implicit imageblock attachment {attachment} rate {data_rate} exceeds the descriptor ABI band"
                )
            })?;
        let arrayed = !crate::tile_mode::has(crate::tile_mode::BAND);
        let image_ty = self.ty_storage_image(Dim::Dim2D, arrayed, format, comp);
        let var = self.descriptor_variable(image_ty, binding);
        self.implicit_imageblock_vars
            .insert((attachment, data_rate), (var, image_ty, format));
        Ok((var, image_ty))
    }

    pub(in crate::passes) fn fragment_imageblock_var(
        &mut self,
        master_member: u32,
        type_name: &str,
    ) -> Result<(Word, Word), String> {
        let format = super::super::fragment_imageblock_format(type_name).ok_or_else(|| {
            format!(
                "fragment imageblock master member {master_member} has unsupported type {type_name}"
            )
        })?;
        if let Some(binding) = self.fragment_imageblock_vars.get(&master_member) {
            return Ok(*binding);
        }
        let capability = spirv::Capability::StorageImageExtendedFormats;
        if !self
            .module
            .capabilities
            .iter()
            .any(|instruction| instruction.operands.as_slice() == [Operand::Capability(capability)])
        {
            self.module.capabilities.push(Instruction::new(
                Op::Capability,
                None,
                None,
                vec![Operand::Capability(capability)],
            ));
        }
        let binding = self.descriptor_layout.fragment_imageblock_binding(master_member)
            .ok_or_else(|| {
                format!(
                    "fragment imageblock master member {master_member} exceeds the descriptor ABI band"
                )
            })?;
        let image_ty =
            self.ty_storage_image(Dim::Dim2D, false, format.image_format, format.component);
        let var = self.descriptor_variable(image_ty, binding);
        self.fragment_imageblock_vars
            .insert(master_member, (var, image_ty));
        Ok((var, image_ty))
    }

    pub(super) fn const_zero(&mut self, ty: Word, _defs: &HashMap<Word, Instruction>) -> Word {
        let id = self.module.fresh_id();
        self.new_globals
            .push(Instruction::new(Op::Undef, Some(ty), Some(id), vec![]));
        id
    }

    pub(in crate::passes) fn const_null(&mut self, ty: Word) -> Word {
        let id = self.module.fresh_id();
        self.new_globals.push(Instruction::new(
            Op::ConstantNull,
            Some(ty),
            Some(id),
            vec![],
        ));
        id
    }

    pub(super) fn zero_private_var(&mut self, pointee: Word) -> Word {
        let pointee = self.private_safe_type(pointee);
        let init = self.const_null(pointee);
        let pptr = self.ty_ptr(StorageClass::Private, pointee);
        let var = self.module.fresh_id();
        self.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pptr),
            Some(var),
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(init),
            ],
        ));
        self.interface_buffer_var(var);
        var
    }

    fn private_safe_type(&mut self, ty: Word) -> Word {
        const ABSENT_BUFFER_ARRAY_LEN: u32 = 1024;
        let def = self
            .module
            .types_global_values
            .iter()
            .chain(self.new_globals.iter())
            .find(|inst| inst.result_id == Some(ty))
            .cloned();
        let Some(def) = def else { return ty };
        match def.class.opcode {
            Op::TypeRuntimeArray => match def.operands.first() {
                Some(Operand::IdRef(elem)) => self.ty_array(*elem, ABSENT_BUFFER_ARRAY_LEN),
                _ => ty,
            },
            Op::TypeStruct => {
                let members: Vec<Word> = def
                    .operands
                    .iter()
                    .filter_map(|o| match o {
                        Operand::IdRef(m) => Some(*m),
                        _ => None,
                    })
                    .collect();
                let fixed: Vec<Word> = members.iter().map(|m| self.private_safe_type(*m)).collect();
                if fixed == members {
                    return ty;
                }
                let st = self.module.fresh_id();
                self.new_globals.push(crate::passes::type_inst(
                    Op::TypeStruct,
                    st,
                    fixed.into_iter().map(Operand::IdRef).collect(),
                ));
                st
            }
            _ => ty,
        }
    }
}

pub(in crate::passes) fn decorate_block_struct(
    ctx: &mut Ctx,
    struct_ty: Word,
    defs: &HashMap<Word, Instruction>,
) {
    ctx.module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(struct_ty),
            Operand::Decoration(Decoration::Block),
        ],
    ));
    decorate_layout_recursive(ctx, struct_ty, defs);
}

fn decorate_layout_recursive(ctx: &mut Ctx, ty: Word, defs: &HashMap<Word, Instruction>) {
    if !ctx.laid_out.insert(ty) {
        return;
    }
    let Some(def) = defs.get(&ty).cloned() else {
        return;
    };
    match def.class.opcode {
        Op::TypeArray | Op::TypeRuntimeArray => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(e)) => *e,
                _ => return,
            };
            let (es, ea) = layout_ty_size_align(ctx, elem, defs);
            let stride = round_up(es, ea);
            ctx.module.annotations.push(Instruction::new(
                Op::Decorate,
                None,
                None,
                vec![
                    Operand::IdRef(ty),
                    Operand::Decoration(Decoration::ArrayStride),
                    Operand::LiteralBit32(stride),
                ],
            ));
            decorate_layout_recursive(ctx, elem, defs);
        }
        Op::TypeStruct => {
            let mut off = 0u32;
            let explicit_offsets = ctx.air_struct_offsets.get(&ty).cloned();
            for (mi, op) in def.operands.clone().iter().enumerate() {
                let Operand::IdRef(mty) = op else { continue };
                let (s, a) = layout_ty_size_align(ctx, *mty, defs);
                off = explicit_offsets
                    .as_ref()
                    .and_then(|offsets| offsets.get(mi).copied())
                    .unwrap_or_else(|| round_up(off, a));
                ctx.module.annotations.push(Instruction::new(
                    Op::MemberDecorate,
                    None,
                    None,
                    vec![
                        Operand::IdRef(ty),
                        Operand::LiteralBit32(mi as u32),
                        Operand::Decoration(Decoration::Offset),
                        Operand::LiteralBit32(off),
                    ],
                ));
                decorate_layout_recursive(ctx, *mty, defs);
                off += round_up(s, a);
            }
        }
        _ => {}
    }
}

pub(in crate::passes) fn drop_unconsumed_placeholder_descriptor_loads(ctx: &mut Ctx) {
    if ctx.placeholder_descriptor_vars.is_empty() {
        return;
    }
    let referenced = crate::passes::module_cleanup::function_referenced_ids(&ctx.module);
    let placeholders = std::mem::take(&mut ctx.placeholder_descriptor_vars);
    let unconsumed_load = |instruction: &Instruction| {
        instruction.class.opcode == Op::Load
            && matches!(
                instruction.operands.first(),
                Some(Operand::IdRef(variable)) if placeholders.contains_key(variable)
            )
            && instruction
                .result_id
                .is_some_and(|id| !referenced.contains(&id))
    };
    for function in &mut ctx.module.functions {
        for block in &mut function.blocks {
            block
                .instructions
                .retain(|instruction| !unconsumed_load(instruction));
        }
    }
    ctx.placeholder_descriptor_vars = placeholders;
}

pub(in crate::passes) fn layout_ty_size_align(
    ctx: &Ctx,
    ty: Word,
    defs: &HashMap<Word, Instruction>,
) -> (u32, u32) {
    crate::layout::spirv_size_align(
        ty,
        defs,
        crate::layout::SpirvLayout::air_offsets(
            &ctx.air_struct_offsets,
            ctx.air_data_layout.as_ref(),
        ),
    )
}

pub(in crate::passes) use crate::layout::round_up_u32 as round_up;
