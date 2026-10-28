use super::*;
use crate::passes::stage_input::{
    byte_varying_interface_type, decorate_builtin, decorate_location, decorate_with,
    scalar_or_vector_component,
};

fn fragment_output_location(frag: Option<&FragMeta>, member_idx: usize) -> Option<u32> {
    match frag {
        Some(meta) => meta.render_target_location_for_member(member_idx as u32),
        None => Some(member_idx as u32),
    }
}

fn fragment_output_is_depth(frag: Option<&FragMeta>, member_idx: usize) -> bool {
    frag.map(|meta| meta.is_depth_member(member_idx as u32))
        .unwrap_or(false)
}

fn fragment_output_is_stencil(frag: Option<&FragMeta>, member_idx: usize) -> bool {
    frag.map(|meta| meta.is_stencil_member(member_idx as u32))
        .unwrap_or(false)
}

fn fragment_output_is_sample_mask(frag: Option<&FragMeta>, member_idx: usize) -> bool {
    frag.map(|meta| meta.is_sample_mask_member(member_idx as u32))
        .unwrap_or(false)
}

fn fragment_output_type(
    ctx: &mut Ctx,
    frag: Option<&FragMeta>,
    member_idx: usize,
    ty: Word,
    defs: &HashMap<Word, Instruction>,
) -> Word {
    let interface_ty = user_location_output_type(ctx, ty);
    if interface_ty != ty {
        return interface_ty;
    }
    let Some(name) = frag.and_then(|meta| meta.render_target_type_name(member_idx as u32)) else {
        return ty;
    };
    if let Some((signed, lanes)) = air_integer_render_target_shape(name) {
        return int32_render_target_interface_type(ctx, signed, lanes, ty, defs).unwrap_or(ty);
    }
    ty
}

fn user_location_output_type(ctx: &mut Ctx, ty: Word) -> Word {
    if type_def_of(ctx, ty).is_some_and(|definition| definition.class.opcode == Op::TypeBool) {
        ctx.ty_uint()
    } else if let Some((bits, lanes)) = float_component_shape_live(ctx, ty) {
        if bits >= 32 {
            ty
        } else if lanes == 1 {
            ctx.ty_float()
        } else {
            ctx.ty_vecf(lanes)
        }
    } else {
        ty
    }
}

fn air_integer_render_target_shape(name: &str) -> Option<(bool, u32)> {
    let raw = name.trim();
    let raw = raw.strip_prefix("packed_").unwrap_or(raw);
    for (prefix, signed) in [
        ("ushort", false),
        ("short", true),
        ("uint", false),
        ("int", true),
        ("uchar", false),
        ("char", true),
    ] {
        let Some(rest) = raw.strip_prefix(prefix) else {
            continue;
        };
        let lanes = if rest.is_empty() {
            1
        } else {
            rest.parse::<u32>().ok()?
        };
        if (1..=4).contains(&lanes) {
            return Some((signed, lanes));
        }
    }
    None
}

fn int32_render_target_interface_type(
    ctx: &mut Ctx,
    signed: bool,
    lanes: u32,
    ty: Word,
    defs: &HashMap<Word, Instruction>,
) -> Option<Word> {
    let (component, actual_lanes) = scalar_or_vector_component(defs, ty)?;
    let def = defs.get(&component)?;
    if def.class.opcode != Op::TypeInt {
        return None;
    }
    let actual_lanes = actual_lanes.unwrap_or(1);
    if actual_lanes != lanes {
        return None;
    }
    Some(match (signed, lanes) {
        (true, 1) => ctx.ty_sint(),
        (false, 1) => ctx.ty_uint(),
        (true, _) => ctx.ty_vec_sint(lanes),
        (false, _) => ctx.ty_vec_uint(lanes),
    })
}

fn bitcast_for_store(
    ctx: &mut Ctx,
    stores: &mut Vec<Instruction>,
    value: Word,
    src_ty: Word,
    dst_ty: Word,
) -> Word {
    if src_ty == dst_ty {
        return value;
    }
    if type_def_of(ctx, src_ty).is_some_and(|ty| ty.class.opcode == Op::TypeBool)
        && int_component_shape_live(ctx, dst_ty) == Some((32, false))
    {
        let selected = ctx.module.fresh_id();
        stores.push(Instruction::new(
            Op::Select,
            Some(dst_ty),
            Some(selected),
            vec![
                Operand::IdRef(value),
                Operand::IdRef(ctx.const_uint(1)),
                Operand::IdRef(ctx.const_uint(0)),
            ],
        ));
        return selected;
    }
    let cast = ctx.module.fresh_id();
    stores.push(Instruction::new(
        Op::Bitcast,
        Some(dst_ty),
        Some(cast),
        vec![Operand::IdRef(value)],
    ));
    cast
}

fn value_for_store(
    ctx: &mut Ctx,
    stores: &mut Vec<Instruction>,
    value: Word,
    src_ty: Word,
    dst_ty: Word,
) -> Word {
    if src_ty == dst_ty {
        return value;
    }
    if let (Some((src_bits, _)), Some((dst_bits, dst_signed))) = (
        int_component_shape_live(ctx, src_ty),
        int_component_shape_live(ctx, dst_ty),
    ) {
        if src_bits != dst_bits {
            let converted = ctx.module.fresh_id();
            stores.push(Instruction::new(
                if dst_signed {
                    Op::SConvert
                } else {
                    Op::UConvert
                },
                Some(dst_ty),
                Some(converted),
                vec![Operand::IdRef(value)],
            ));
            return converted;
        }
    }
    if let (Some((src_bits, src_lanes)), Some((dst_bits, dst_lanes))) = (
        float_component_shape_live(ctx, src_ty),
        float_component_shape_live(ctx, dst_ty),
    ) {
        if src_bits != dst_bits && src_lanes == dst_lanes {
            let converted = ctx.module.fresh_id();
            stores.push(Instruction::new(
                Op::FConvert,
                Some(dst_ty),
                Some(converted),
                vec![Operand::IdRef(value)],
            ));
            return converted;
        }
    }
    bitcast_for_store(ctx, stores, value, src_ty, dst_ty)
}

/// An A8Unorm colour attachment is an R8 image on Vulkan: broadcast the output's alpha to every lane so the red lane,
/// the only one the target keeps, holds the alpha Metal would have stored. Scalar and non-float outputs pass through.
fn a8_splat_alpha(ctx: &mut Ctx, stores: &mut Vec<Instruction>, value: Word, ty: Word) -> Word {
    if float_component_shape_live(ctx, ty) != Some((32, 4)) {
        return value;
    }
    let splat = ctx.module.fresh_id();
    stores.push(Instruction::new(
        Op::VectorShuffle,
        Some(ty),
        Some(splat),
        vec![
            Operand::IdRef(value),
            Operand::IdRef(value),
            Operand::LiteralBit32(3),
            Operand::LiteralBit32(3),
            Operand::LiteralBit32(3),
            Operand::LiteralBit32(3),
        ],
    ));
    splat
}

fn float_component_shape_live(ctx: &Ctx, ty: Word) -> Option<(u32, u32)> {
    let def = type_def_of(ctx, ty)?;
    let (scalar_ty, lanes) = if def.class.opcode == Op::TypeVector {
        match def.operands.as_slice() {
            [Operand::IdRef(component), Operand::LiteralBit32(lanes)] => (*component, *lanes),
            _ => return None,
        }
    } else {
        (ty, 1)
    };
    let def = type_def_of(ctx, scalar_ty)?;
    if def.class.opcode != Op::TypeFloat {
        return None;
    }
    match def.operands.first()? {
        Operand::LiteralBit32(bits) => Some((*bits, lanes)),
        _ => None,
    }
}

fn int_component_shape_live(ctx: &Ctx, ty: Word) -> Option<(u32, bool)> {
    let mut scalar_ty = ty;
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode == Op::TypeVector {
        scalar_ty = match def.operands.first()? {
            Operand::IdRef(component) => *component,
            _ => return None,
        };
    }
    let def = type_def_of(ctx, scalar_ty)?;
    if def.class.opcode != Op::TypeInt {
        return None;
    }
    let bits = match def.operands.first()? {
        Operand::LiteralBit32(bits) => *bits,
        _ => return None,
    };
    let signed = match def.operands.get(1)? {
        Operand::LiteralBit32(signed) => *signed != 0,
        _ => return None,
    };
    Some((bits, signed))
}

fn fragment_imageblock_coord(
    ctx: &mut Ctx,
    coord_var: Word,
    instructions: &mut Vec<Instruction>,
) -> Word {
    let v4float = ctx.ty_vecf(4);
    let float_ty = ctx.ty_float();
    let sint_ty = ctx.ty_sint();
    let v2sint = ctx.ty_vec_sint(2);
    let coord_value = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::Load,
        Some(v4float),
        Some(coord_value),
        vec![Operand::IdRef(coord_var)],
    ));
    let mut components = Vec::with_capacity(2);
    for component in 0..2 {
        let value = ctx.module.fresh_id();
        instructions.push(Instruction::new(
            Op::CompositeExtract,
            Some(float_ty),
            Some(value),
            vec![
                Operand::IdRef(coord_value),
                Operand::LiteralBit32(component),
            ],
        ));
        let converted = ctx.module.fresh_id();
        instructions.push(Instruction::new(
            Op::ConvertFToS,
            Some(sint_ty),
            Some(converted),
            vec![Operand::IdRef(value)],
        ));
        components.push(Operand::IdRef(converted));
    }
    let coord = ctx.module.fresh_id();
    instructions.push(Instruction::new(
        Op::CompositeConstruct,
        Some(v2sint),
        Some(coord),
        components,
    ));
    coord
}

fn ensure_fragment_imageblock_coord_var(ctx: &mut Ctx) -> Word {
    if let Some(var) = ctx.fragment_imageblock_coord_var {
        return var;
    }
    let coord_ty = ctx.ty_vecf(4);
    let pointer_ty = ctx.ty_ptr(StorageClass::Input, coord_ty);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pointer_ty),
        Some(var),
        vec![Operand::StorageClass(StorageClass::Input)],
    ));
    decorate_builtin(&mut ctx.module, var, BuiltIn::FragCoord);
    ctx.interface.push(var);
    ctx.fragment_imageblock_coord_var = Some(var);
    var
}

fn vertex_builtin_output_type(ctx: &mut Ctx, builtin: BuiltIn, member_ty: Word) -> Word {
    match builtin {
        BuiltIn::Layer | BuiltIn::ViewportIndex => ctx.ty_uint(),
        _ => member_ty,
    }
}

enum ClipDistanceOutputType {
    Scalar { array_ty: Word, elem_ty: Word },
    Array { array_ty: Word },
}

impl ClipDistanceOutputType {
    fn array_ty(&self) -> Word {
        match *self {
            ClipDistanceOutputType::Scalar { array_ty, .. }
            | ClipDistanceOutputType::Array { array_ty } => array_ty,
        }
    }
}

fn clip_distance_output_type(
    ctx: &mut Ctx,
    defs: &HashMap<Word, Instruction>,
    member_ty: Word,
) -> Result<ClipDistanceOutputType, String> {
    if let Some(def) = defs
        .get(&member_ty)
        .filter(|def| def.class.opcode == Op::TypeArray)
    {
        let elem = match def.operands.first() {
            Some(Operand::IdRef(elem)) => *elem,
            _ => {
                return Err(format!(
                    "clip_distance output type {member_ty} has no array element type"
                ));
            }
        };
        if !defs.get(&elem).is_some_and(|def| {
            def.class.opcode == Op::TypeFloat
                && def.operands.first() == Some(&Operand::LiteralBit32(32))
        }) {
            return Err(format!(
                "clip_distance output array type {member_ty} is not float[]"
            ));
        }
        return Ok(ClipDistanceOutputType::Array {
            array_ty: member_ty,
        });
    }

    let (component, lanes) = scalar_or_vector_component(defs, member_ty).ok_or_else(|| {
        format!("clip_distance output type {member_ty} is not scalar/vector/array")
    })?;
    if lanes.is_some() {
        return Err("vector clip_distance outputs are not yet supported".to_string());
    }
    if !defs.get(&component).is_some_and(|def| {
        def.class.opcode == Op::TypeFloat
            && def.operands.first() == Some(&Operand::LiteralBit32(32))
    }) {
        return Err(format!(
            "clip_distance output type {member_ty} is not scalar float"
        ));
    }
    Ok(ClipDistanceOutputType::Scalar {
        array_ty: ctx.ty_array(component, 1),
        elem_ty: component,
    })
}

fn value_is_statically_undef(ctx: &Ctx, value: Word) -> bool {
    value_def_instruction(ctx, value)
        .map(|def| def.class.opcode == Op::Undef)
        .unwrap_or(false)
}

fn composite_member_is_statically_undef(ctx: &Ctx, composite: Word, member: u32) -> bool {
    let Some(def) = value_def_instruction(ctx, composite) else {
        return false;
    };
    match def.class.opcode {
        Op::Undef => true,
        Op::CompositeConstruct => def
            .operands
            .get(member as usize)
            .and_then(|operand| match operand {
                Operand::IdRef(id) => Some(value_is_statically_undef(ctx, *id)),
                _ => None,
            })
            .unwrap_or(false),
        Op::CompositeInsert => {
            let Some(Operand::IdRef(inserted)) = def.operands.first() else {
                return false;
            };
            let Some(Operand::IdRef(base)) = def.operands.get(1) else {
                return false;
            };
            let inserted_member = def.operands.get(2).and_then(|operand| match operand {
                Operand::LiteralBit32(index) => Some(*index),
                _ => None,
            });
            match inserted_member {
                Some(index) if index == member => value_is_statically_undef(ctx, *inserted),
                Some(_) => composite_member_is_statically_undef(ctx, *base, member),
                None => false,
            }
        }
        _ => false,
    }
}

pub(in crate::passes) fn rewrite_return(
    ctx: &mut Ctx,
    entry_idx: usize,
    stage: &Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    defs: &HashMap<Word, Instruction>,
) -> Result<(), String> {
    let mut ret_locs: Vec<(usize, usize, Word)> = Vec::new();
    for (bi, blk) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (ii, inst) in blk.instructions.iter().enumerate() {
            if inst.class.opcode == Op::ReturnValue {
                if let Some(Operand::IdRef(v)) = inst.operands.first() {
                    ret_locs.push((bi, ii, *v));
                }
            }
        }
    }
    if ret_locs.is_empty() {
        if ctx.uses_fragment_imageblock {
            return Err(
                "fragment imageblock entry has no value return at which to end pixel interlock"
                    .to_string(),
            );
        }
        return Ok(());
    }

    let ret_ty = ctx.module.functions[entry_idx]
        .def
        .as_ref()
        .and_then(|d| d.result_type)
        .ok_or_else(|| "entry function has no result type".to_string())?;

    let declared_outputs_must_be_written = matches!(stage, Stage::Vertex);

    enum OutputWrite {
        Direct {
            var: Word,
            src_ty: Word,
            dst_ty: Word,
        },
        Extract {
            var: Word,
            member: u32,
            src_ty: Word,
            dst_ty: Word,
        },
        ArrayBuiltinSlot {
            var: Word,
            member: Option<u32>,
            src_ty: Word,
            elem_ty: Word,
        },
        FragmentImageblock {
            image_var: Word,
            image_ty: Word,
            return_member: u32,
            projection_member: u32,
            projection_ty: Word,
            member_ty: Word,
            coord_var: Word,
            format: FragmentImageblockFormat,
        },
    }

    impl OutputWrite {
        fn stores(
            &self,
            ctx: &mut Ctx,
            retval: Word,
            declared_outputs_must_be_written: bool,
        ) -> Vec<Instruction> {
            let mut stores = Vec::new();
            let skip_undef = !declared_outputs_must_be_written;
            let (var, value, src_ty, dst_ty) = match *self {
                OutputWrite::Direct {
                    var,
                    src_ty,
                    dst_ty,
                } => {
                    if skip_undef && value_is_statically_undef(ctx, retval) {
                        return stores;
                    }
                    (var, retval, src_ty, dst_ty)
                }
                OutputWrite::Extract {
                    var,
                    member,
                    src_ty,
                    dst_ty,
                } => {
                    if skip_undef && composite_member_is_statically_undef(ctx, retval, member) {
                        return stores;
                    }
                    let ext = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(src_ty),
                        Some(ext),
                        vec![Operand::IdRef(retval), Operand::LiteralBit32(member)],
                    ));
                    (var, ext, src_ty, dst_ty)
                }
                OutputWrite::ArrayBuiltinSlot {
                    var,
                    member,
                    src_ty,
                    elem_ty,
                } => {
                    let ext = match member {
                        Some(member) => {
                            if skip_undef
                                && composite_member_is_statically_undef(ctx, retval, member)
                            {
                                return stores;
                            }
                            let ext = ctx.module.fresh_id();
                            stores.push(Instruction::new(
                                Op::CompositeExtract,
                                Some(src_ty),
                                Some(ext),
                                vec![Operand::IdRef(retval), Operand::LiteralBit32(member)],
                            ));
                            ext
                        }
                        None => {
                            if skip_undef && value_is_statically_undef(ctx, retval) {
                                return stores;
                            }
                            retval
                        }
                    };
                    let zero = ctx.const_uint(0);
                    let ptr_ty = ctx.ty_ptr(StorageClass::Output, elem_ty);
                    let ptr = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        Op::AccessChain,
                        Some(ptr_ty),
                        Some(ptr),
                        vec![Operand::IdRef(var), Operand::IdRef(zero)],
                    ));
                    let value = value_for_store(ctx, &mut stores, ext, src_ty, elem_ty);
                    stores.push(Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(ptr), Operand::IdRef(value)],
                    ));
                    return stores;
                }
                OutputWrite::FragmentImageblock {
                    image_var,
                    image_ty,
                    return_member,
                    projection_member,
                    projection_ty,
                    member_ty,
                    coord_var,
                    format,
                } => {
                    if composite_member_is_statically_undef(ctx, retval, return_member) {
                        return stores;
                    }
                    let projection = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(projection_ty),
                        Some(projection),
                        vec![Operand::IdRef(retval), Operand::LiteralBit32(return_member)],
                    ));
                    let member = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(member_ty),
                        Some(member),
                        vec![
                            Operand::IdRef(projection),
                            Operand::LiteralBit32(projection_member),
                        ],
                    ));
                    let wide_scalar_ty = match format.component {
                        ImageComp::Float => ctx.ty_float(),
                        ImageComp::Uint => ctx.ty_uint(),
                        ImageComp::Sint => ctx.ty_sint(),
                    };
                    let wide_ty = if format.lanes == 1 {
                        wide_scalar_ty
                    } else {
                        match format.component {
                            ImageComp::Float => ctx.ty_vecf(4),
                            ImageComp::Uint => ctx.ty_vec_uint(4),
                            ImageComp::Sint => ctx.ty_vec_sint(4),
                        }
                    };
                    let converted = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        match format.component {
                            ImageComp::Float => Op::FConvert,
                            ImageComp::Uint => Op::UConvert,
                            ImageComp::Sint => Op::SConvert,
                        },
                        Some(wide_ty),
                        Some(converted),
                        vec![Operand::IdRef(member)],
                    ));
                    let texel = if format.lanes == 1 {
                        let texel = ctx.module.fresh_id();
                        stores.push(Instruction::new(
                            Op::CompositeConstruct,
                            Some(match format.component {
                                ImageComp::Float => ctx.ty_vecf(4),
                                ImageComp::Uint => ctx.ty_vec_uint(4),
                                ImageComp::Sint => ctx.ty_vec_sint(4),
                            }),
                            Some(texel),
                            vec![Operand::IdRef(converted); 4],
                        ));
                        texel
                    } else {
                        converted
                    };
                    let coord = fragment_imageblock_coord(ctx, coord_var, &mut stores);
                    let image = ctx.module.fresh_id();
                    stores.push(Instruction::new(
                        Op::Load,
                        Some(image_ty),
                        Some(image),
                        vec![Operand::IdRef(image_var)],
                    ));
                    stores.push(Instruction::new(
                        Op::ImageWrite,
                        None,
                        None,
                        vec![
                            Operand::IdRef(image),
                            Operand::IdRef(coord),
                            Operand::IdRef(texel),
                        ],
                    ));
                    return stores;
                }
            };
            let value = value_for_store(ctx, &mut stores, value, src_ty, dst_ty);
            let value = if ctx.a8_output_vars.contains(&var) {
                a8_splat_alpha(ctx, &mut stores, value, dst_ty)
            } else {
                value
            };
            stores.push(Instruction::new(
                Op::Store,
                None,
                None,
                vec![Operand::IdRef(var), Operand::IdRef(value)],
            ));
            stores
        }
    }

    // Input layout and local-pointer materialization can clone a buffer record
    // also used by the entry's return aggregate. Resolve the current type IDs,
    // including deferred globals, before constructing the output interface.
    let mut output_defs = defs.clone();
    for instruction in ctx.module.types_global_values.iter().chain(ctx.new_globals.iter()) {
        if let Some(id) = instruction.result_id {
            output_defs.insert(id, instruction.clone());
        }
    }
    let defs = &output_defs;
    let mut outputs: Vec<OutputWrite> = vec![];
    let rdef = defs.get(&ret_ty).cloned();
    if rdef.is_none() && matches!(stage, Stage::Vertex | Stage::Fragment) {
        return Err(format!("stage output return type {ret_ty} has no current definition"));
    }

    let unmodelled = match stage {
        Stage::Fragment => frag.map(|meta| meta.unmodelled_output_members.as_slice()),
        Stage::Vertex => vert.map(|meta| meta.unmodelled_output_members.as_slice()),
        Stage::Kernel => None,
    }
    .unwrap_or_default();
    if let Some((member, role)) = unmodelled.first() {
        return Err(format!(
            "return member {member} declares AIR output role `air.{role}`, which has no lowering; \
             emitting the module would silently drop what the shader wrote to it"
        ));
    }

    match stage {
        Stage::Fragment => {
            if let Some(def) = &rdef {
                if def.class.opcode == Op::TypeStruct {
                    for (mi, op) in def.operands.clone().iter().enumerate() {
                        let Operand::IdRef(mty) = op else { continue };
                        if let Some((imageblock, projection)) = frag
                            .and_then(|meta| meta.fragment_imageblock.as_ref())
                            .and_then(|imageblock| {
                                imageblock
                                    .outputs
                                    .iter()
                                    .find(|projection| projection.interface_index == mi as u32)
                                    .map(|projection| (imageblock, projection))
                            })
                        {
                            let projected_types = defs
                                .get(mty)
                                .filter(|definition| definition.class.opcode == Op::TypeStruct)
                                .ok_or_else(|| {
                                    format!(
                                        "fragment imageblock return member {mi} is not a struct value"
                                    )
                                })?
                                .operands
                                .iter()
                                .filter_map(|operand| match operand {
                                    Operand::IdRef(ty) => Some(*ty),
                                    _ => None,
                                })
                                .collect::<Vec<_>>();
                            if projected_types.len() != projection.members.len() {
                                return Err(format!(
                                    "fragment imageblock return member {mi} exposes {} fields but AIR projects {}",
                                    projected_types.len(),
                                    projection.members.len()
                                ));
                            }
                            let coord_var = ensure_fragment_imageblock_coord_var(ctx);
                            for (projected_ty, projected) in
                                projected_types.into_iter().zip(projection.members.iter())
                            {
                                let master = imageblock
                                    .members
                                    .get(projected.master_member as usize)
                                    .ok_or_else(|| {
                                        format!(
                                            "fragment imageblock return member {mi} references missing master member {}",
                                            projected.master_member
                                        )
                                    })?;
                                let format = fragment_imageblock_format(&master.type_name)
                                    .ok_or_else(|| {
                                        format!(
                                            "fragment imageblock return member {mi} field {} has unsupported master type {}",
                                            projected.projection_member, master.type_name
                                        )
                                    })?;
                                if !super::stage_input::fragment_imageblock_projection_type_matches(
                                    defs,
                                    projected_ty,
                                    format,
                                ) {
                                    return Err(format!(
                                        "fragment imageblock return member {mi} field {} does not match AIR master type {}",
                                        projected.projection_member, master.type_name
                                    ));
                                }
                                let (image_var, image_ty) = ctx.fragment_imageblock_var(
                                    projected.master_member,
                                    &master.type_name,
                                )?;
                                outputs.push(OutputWrite::FragmentImageblock {
                                    image_var,
                                    image_ty,
                                    return_member: mi as u32,
                                    projection_member: projected.projection_member,
                                    projection_ty: *mty,
                                    member_ty: projected_ty,
                                    coord_var,
                                    format,
                                });
                            }
                            continue;
                        }
                        #[derive(Clone, Copy)]
                        enum OutKind {
                            Location(u32),
                            Depth,
                            Stencil,
                            SampleMask,
                        }
                        let kind = if fragment_output_is_depth(frag, mi) {
                            Some(OutKind::Depth)
                        } else if fragment_output_is_stencil(frag, mi) {
                            Some(OutKind::Stencil)
                        } else if fragment_output_is_sample_mask(frag, mi) {
                            Some(OutKind::SampleMask)
                        } else {
                            fragment_output_location(frag, mi).map(OutKind::Location)
                        };
                        let Some(kind) = kind else { continue };
                        let uint_ty = ctx.ty_uint();
                        let output_ty = match kind {
                            OutKind::SampleMask => ctx.ty_array(uint_ty, 1),
                            _ => fragment_output_type(ctx, frag, mi, *mty, defs),
                        };
                        let var = make_output_var(ctx, output_ty);
                        match kind {
                            OutKind::Location(location) => {
                                decorate_location(&mut ctx.module, var, location);
                                if location < 32
                                    && (ctx.a8_render_target_mask >> location) & 1 == 1
                                    && !frag.is_some_and(|meta| meta.is_dual_source_member(mi as u32))
                                {
                                    ctx.a8_output_vars.insert(var);
                                }
                                if frag.is_some_and(|meta| meta.is_dual_source_member(mi as u32)) {
                                    ctx.module.annotations.push(Instruction::new(
                                        Op::Decorate,
                                        None,
                                        None,
                                        vec![
                                            Operand::IdRef(var),
                                            Operand::Decoration(Decoration::Index),
                                            Operand::LiteralBit32(1),
                                        ],
                                    ));
                                }
                            }
                            OutKind::Depth => {
                                decorate_builtin(&mut ctx.module, var, BuiltIn::FragDepth);
                                ctx.writes_frag_depth = true;
                            }
                            OutKind::Stencil => {
                                decorate_builtin(&mut ctx.module, var, BuiltIn::FragStencilRefEXT)
                            }
                            OutKind::SampleMask => {
                                decorate_builtin(&mut ctx.module, var, BuiltIn::SampleMask)
                            }
                        }
                        ctx.interface.push(var);
                        outputs.push(match kind {
                            OutKind::SampleMask => OutputWrite::ArrayBuiltinSlot {
                                var,
                                member: Some(mi as u32),
                                src_ty: *mty,
                                elem_ty: uint_ty,
                            },
                            _ => OutputWrite::Extract {
                                var,
                                member: mi as u32,
                                src_ty: *mty,
                                dst_ty: output_ty,
                            },
                        });
                    }
                } else {
                    if fragment_output_is_depth(frag, 0) {
                        let var = make_output_var(ctx, ret_ty);
                        decorate_builtin(&mut ctx.module, var, BuiltIn::FragDepth);
                        ctx.writes_frag_depth = true;
                        ctx.interface.push(var);
                        outputs.push(OutputWrite::Direct {
                            var,
                            src_ty: ret_ty,
                            dst_ty: ret_ty,
                        });
                    } else if fragment_output_is_stencil(frag, 0) {
                        let var = make_output_var(ctx, ret_ty);
                        decorate_builtin(&mut ctx.module, var, BuiltIn::FragStencilRefEXT);
                        ctx.interface.push(var);
                        outputs.push(OutputWrite::Direct {
                            var,
                            src_ty: ret_ty,
                            dst_ty: ret_ty,
                        });
                    } else if fragment_output_is_sample_mask(frag, 0) {
                        let uint_ty = ctx.ty_uint();
                        let mask_ty = ctx.ty_array(uint_ty, 1);
                        let var = make_output_var(ctx, mask_ty);
                        decorate_builtin(&mut ctx.module, var, BuiltIn::SampleMask);
                        ctx.interface.push(var);
                        outputs.push(OutputWrite::ArrayBuiltinSlot {
                            var,
                            member: None,
                            src_ty: ret_ty,
                            elem_ty: uint_ty,
                        });
                    } else if let Some(location) = fragment_output_location(frag, 0) {
                        let output_ty = fragment_output_type(ctx, frag, 0, ret_ty, defs);
                        let var = make_output_var(ctx, output_ty);
                        decorate_location(&mut ctx.module, var, location);
                        if location < 32 && (ctx.a8_render_target_mask >> location) & 1 == 1 {
                            ctx.a8_output_vars.insert(var);
                        }
                        ctx.interface.push(var);
                        outputs.push(OutputWrite::Direct {
                            var,
                            src_ty: ret_ty,
                            dst_ty: output_ty,
                        });
                    }
                }
            }
        }
        Stage::Vertex => {
            if let Some(def) = &rdef {
                if def.class.opcode == Op::TypeStruct {
                    let mut fallback_loc = 0u32;
                    for (mi, op) in def.operands.clone().iter().enumerate() {
                        let Operand::IdRef(mty) = op else { continue };
                        #[derive(Clone, Copy)]
                        enum OutKind {
                            Builtin(BuiltIn),
                            Location(u32),
                        }
                        let kind = match vert.and_then(|m| m.output_role_of(mi as u32)).cloned() {
                            Some(VertOutRole::Position) => OutKind::Builtin(BuiltIn::Position),
                            Some(VertOutRole::PointSize) => OutKind::Builtin(BuiltIn::PointSize),
                            Some(VertOutRole::ClipDistance) => {
                                OutKind::Builtin(BuiltIn::ClipDistance)
                            }
                            Some(VertOutRole::ViewportArrayIndex) => {
                                OutKind::Builtin(BuiltIn::ViewportIndex)
                            }
                            Some(VertOutRole::RenderTargetArrayIndex) => {
                                OutKind::Builtin(BuiltIn::Layer)
                            }
                            Some(VertOutRole::Varying(l)) => {
                                fallback_loc = fallback_loc.max(l + 1);
                                OutKind::Location(l)
                            }
                            Some(VertOutRole::FunctionConstantDisabled) => continue,
                            _ if mi == 0 => OutKind::Builtin(BuiltIn::Position),
                            _ => {
                                let loc = fallback_loc;
                                fallback_loc += 1;
                                OutKind::Location(loc)
                            }
                        };
                        let clip_distance_ty = match kind {
                            OutKind::Builtin(BuiltIn::ClipDistance) => {
                                Some(clip_distance_output_type(ctx, defs, *mty)?)
                            }
                            _ => None,
                        };
                        let output_ty = match (kind, clip_distance_ty.as_ref()) {
                            (OutKind::Builtin(BuiltIn::ClipDistance), Some(clip_distance_ty)) => {
                                clip_distance_ty.array_ty()
                            }
                            (OutKind::Builtin(builtin), _) => {
                                vertex_builtin_output_type(ctx, builtin, *mty)
                            }
                            (OutKind::Location(_), _) => {
                                let transport_ty = byte_varying_interface_type(ctx, *mty);
                                user_location_output_type(ctx, transport_ty)
                            }
                        };
                        let var = make_output_var(ctx, output_ty);
                        match kind {
                            OutKind::Builtin(builtin) => {
                                decorate_builtin(&mut ctx.module, var, builtin)
                            }
                            OutKind::Location(loc) => decorate_location(&mut ctx.module, var, loc),
                        }
                        if vert.is_some_and(|m| m.output_is_invariant(mi as u32)) {
                            decorate_with(&mut ctx.module, var, Decoration::Invariant);
                        }
                        ctx.interface.push(var);
                        match clip_distance_ty {
                            Some(ClipDistanceOutputType::Scalar { elem_ty, .. }) => {
                                outputs.push(OutputWrite::ArrayBuiltinSlot {
                                    var,
                                    member: Some(mi as u32),
                                    src_ty: *mty,
                                    elem_ty,
                                });
                            }
                            Some(ClipDistanceOutputType::Array { .. }) | None => {
                                outputs.push(OutputWrite::Extract {
                                    var,
                                    member: mi as u32,
                                    src_ty: *mty,
                                    dst_ty: output_ty,
                                });
                            }
                        }
                    }
                } else {
                    let var = make_output_var(ctx, ret_ty);
                    decorate_builtin(&mut ctx.module, var, BuiltIn::Position);
                    if vert.is_some_and(|m| m.output_is_invariant(0)) {
                        decorate_with(&mut ctx.module, var, Decoration::Invariant);
                    }
                    ctx.interface.push(var);
                    outputs.push(OutputWrite::Direct {
                        var,
                        src_ty: ret_ty,
                        dst_ty: ret_ty,
                    });
                }
            }
        }
        Stage::Kernel => {}
    }

    ret_locs.sort_by_key(|(bi, ii, _)| (*bi, *ii));
    for (bi, ii, retval) in ret_locs.into_iter().rev() {
        let mut replacement = Vec::new();
        for output in &outputs {
            replacement.extend(output.stores(ctx, retval, declared_outputs_must_be_written));
        }
        if ctx.uses_fragment_imageblock {
            replacement.push(Instruction::new(
                Op::EndInvocationInterlockEXT,
                None,
                None,
                vec![],
            ));
        }
        replacement.push(Instruction::new(Op::Return, None, None, vec![]));

        let blk = &mut ctx.module.functions[entry_idx].blocks[bi];
        blk.instructions.splice(ii..=ii, replacement);
    }
    Ok(())
}

pub(in crate::passes) fn handle_static_sampler(ctx: &mut Ctx) -> Result<(), String> {
    let mut samp_globals: Vec<(Word, String)> = vec![];
    for inst in &ctx.module.debug_names {
        if inst.class.opcode == Op::Name {
            if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(s))) =
                (inst.operands.first(), inst.operands.get(1))
            {
                if crate::meta::is_static_sampler_global(s) {
                    samp_globals.push((*id, s.trim_start_matches('@').to_string()));
                }
            }
        }
    }
    if samp_globals.is_empty() {
        return Ok(());
    }
    samp_globals.sort_by_key(|(_, name)| crate::meta::static_sampler_name_order(name));

    let sty = ctx.ty_sampler();
    let pptr = ctx.ty_ptr(StorageClass::UniformConstant, sty);

    for (old_var, _) in samp_globals {
        let sampler_state = static_sampler_words(ctx, old_var)
            .map(StaticSamplerState::from_air_words)
            .transpose()?;
        if let Some(state) = sampler_state {
            state.validate_lowering()?;
        }
        let new_var = ctx.module.fresh_id();
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(pptr),
            Some(new_var),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
        let layout = ctx.descriptor_layout;
        let binding = allocate_static_sampler_binding(&ctx.module, layout).ok_or_else(|| {
            format!(
                "AIR constexpr sampler count exceeds descriptor band \
                 [{},{})",
                layout.samplers.start, layout.samplers.end
            )
        })?;
        decorate_binding(&mut ctx.module, new_var, layout.set, binding);
        ctx.interface_buffer_var(new_var);

        ctx.module
            .types_global_values
            .retain(|i| i.result_id != Some(old_var));
        ctx.module
            .debug_names
            .retain(|i| i.operands.first() != Some(&Operand::IdRef(old_var)));
        ctx.module
            .annotations
            .retain(|i| i.operands.first() != Some(&Operand::IdRef(old_var)));

        let load_count = ctx
            .module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                !(instruction.class.opcode == Op::Bitcast
                    && instruction.operands.first() == Some(&Operand::IdRef(old_var)))
                    && instruction.operands.contains(&Operand::IdRef(old_var))
            })
            .count();
        let load_count = Word::try_from(load_count)
            .expect("SPIR-V module cannot reserve more than u32::MAX ids");
        let mut load_ids = ctx.module.reserve_ids(load_count);
        for func in &mut ctx.module.functions {
            for blk in &mut func.blocks {
                let mut ii = 0;
                while ii < blk.instructions.len() {
                    let inst = &mut blk.instructions[ii];
                    if inst.class.opcode == Op::Bitcast
                        && inst.operands.first() == Some(&Operand::IdRef(old_var))
                    {
                        let rid = inst.result_id;
                        *inst = Instruction::new(
                            Op::Load,
                            Some(sty),
                            rid,
                            vec![Operand::IdRef(new_var)],
                        );
                        if let (Some(rid), Some(state)) = (rid, sampler_state) {
                            ctx.sampler_states.insert(rid, state);
                        }
                        ii += 1;
                        continue;
                    }

                    let mut load_id = None;
                    for op in &mut inst.operands {
                        if *op == Operand::IdRef(old_var) {
                            let id = *load_id.get_or_insert_with(|| {
                                load_ids.next().expect(
                                    "reserved one sampler-load id per rewritten instruction",
                                )
                            });
                            *op = Operand::IdRef(id);
                        }
                    }
                    if let Some(id) = load_id {
                        blk.instructions.insert(
                            ii,
                            Instruction::new(
                                Op::Load,
                                Some(sty),
                                Some(id),
                                vec![Operand::IdRef(new_var)],
                            ),
                        );
                        if let Some(state) = sampler_state {
                            ctx.sampler_states.insert(id, state);
                        }
                        ii += 2;
                    } else {
                        ii += 1;
                    }
                }
            }
        }
        debug_assert!(load_ids.next().is_none());
    }
    Ok(())
}

fn static_sampler_words(ctx: &Ctx, var: Word) -> Option<[u64; 2]> {
    let initializer = ctx
        .module
        .types_global_values
        .iter()
        .find(|inst| inst.result_id == Some(var) && inst.class.opcode == Op::Variable)
        .and_then(|inst| inst.operands.get(1))
        .and_then(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        })?;
    static_sampler_words_from_const(ctx, initializer)
}

fn static_sampler_words_from_const(ctx: &Ctx, value: Word) -> Option<[u64; 2]> {
    let inst = ctx
        .module
        .types_global_values
        .iter()
        .find(|inst| inst.result_id == Some(value))?;
    match inst.class.opcode {
        Op::Constant => match inst.operands.first()? {
            Operand::LiteralBit64(value) => Some([*value, 0]),
            Operand::LiteralBit32(value) => Some([*value as u64, 0]),
            _ => None,
        },
        Op::ConstantComposite => {
            let mut words = inst.operands.iter().filter_map(|operand| match operand {
                Operand::IdRef(id) => {
                    static_sampler_words_from_const(ctx, *id).map(|words| words[0])
                }
                _ => None,
            });
            Some([words.next()?, words.next().unwrap_or(0)])
        }
        _ => None,
    }
}

fn make_output_var(ctx: &mut Ctx, ty: Word) -> Word {
    let pptr = ctx.ty_ptr(StorageClass::Output, ty);
    let var = ctx.module.fresh_id();
    ctx.new_globals.push(Instruction::new(
        Op::Variable,
        Some(pptr),
        Some(var),
        vec![Operand::StorageClass(StorageClass::Output)],
    ));
    var
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::resources::rewrites::combined_type_defs;

    #[test]
    fn scalar_bool_fragment_output_uses_uint_interface_and_select() {
        let mut ctx = Ctx::new(Module::new());
        let bool_ty = ctx.ty_bool();
        let defs = combined_type_defs(&ctx, &HashMap::default());

        let output_ty = fragment_output_type(&mut ctx, None, 0, bool_ty, &defs);
        assert_eq!(output_ty, ctx.ty_uint());

        let bool_value = ctx.module.fresh_id();
        let mut stores = Vec::new();
        let converted = value_for_store(&mut ctx, &mut stores, bool_value, bool_ty, output_ty);
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0].class.opcode, Op::Select);
        assert_eq!(stores[0].result_id, Some(converted));
        assert_eq!(stores[0].result_type, Some(output_ty));
    }
}
