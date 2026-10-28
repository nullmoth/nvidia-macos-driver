use super::*;

pub(in crate::passes) fn buffer_pointer_aliases(func: &Function, pid: Word) -> HashSet<Word> {
    let mut aliases = crate::fxhash::set_from([pid]);
    let mut paths: HashMap<Word, HashMap<Vec<u32>, Word>> = HashMap::default();
    let mut changed = true;
    while changed {
        changed = false;
        for inst in func
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter())
        {
            let Some(result) = inst.result_id else {
                continue;
            };
            match inst.class.opcode {
                Op::CompositeInsert => {
                    let (Some(Operand::IdRef(object)), Some(Operand::IdRef(base))) =
                        (inst.operands.first(), inst.operands.get(1))
                    else {
                        continue;
                    };
                    let mut result_paths = paths.get(base).cloned().unwrap_or_default();
                    let path = literal_path(&inst.operands[2..]);
                    if aliases.contains(object) {
                        result_paths.insert(path.clone(), *object);
                    }
                    if let Some(object_paths) = paths.get(object) {
                        for (suffix, alias) in object_paths {
                            let mut full = path.clone();
                            full.extend(suffix);
                            result_paths.insert(full, *alias);
                        }
                    }
                    if !result_paths.is_empty() && paths.get(&result) != Some(&result_paths) {
                        paths.insert(result, result_paths);
                        changed = true;
                    }
                }
                Op::CompositeConstruct => {
                    let mut result_paths = HashMap::default();
                    for (index, operand) in inst.operands.iter().enumerate() {
                        let Operand::IdRef(object) = operand else {
                            continue;
                        };
                        let prefix = vec![index as u32];
                        if aliases.contains(object) {
                            result_paths.insert(prefix.clone(), *object);
                        }
                        if let Some(object_paths) = paths.get(object) {
                            for (suffix, alias) in object_paths {
                                let mut full = prefix.clone();
                                full.extend(suffix);
                                result_paths.insert(full, *alias);
                            }
                        }
                    }
                    if !result_paths.is_empty() && paths.get(&result) != Some(&result_paths) {
                        paths.insert(result, result_paths);
                        changed = true;
                    }
                }
                Op::CompositeExtract => {
                    let Some(Operand::IdRef(composite)) = inst.operands.first() else {
                        continue;
                    };
                    let path = literal_path(&inst.operands[1..]);
                    let Some(composite_paths) = paths.get(composite) else {
                        continue;
                    };
                    if composite_paths.contains_key(&path) && aliases.insert(result) {
                        changed = true;
                    }
                    let mut result_paths = HashMap::default();
                    for (alias_path, alias) in composite_paths {
                        if let Some(suffix) = path_suffix(alias_path, &path) {
                            if !suffix.is_empty() {
                                result_paths.insert(suffix.to_vec(), *alias);
                            }
                        }
                    }
                    if !result_paths.is_empty() && paths.get(&result) != Some(&result_paths) {
                        paths.insert(result, result_paths);
                        changed = true;
                    }
                }
                Op::CopyObject | Op::Bitcast => {
                    let Some(Operand::IdRef(source)) = inst.operands.first() else {
                        continue;
                    };
                    if aliases.contains(source) && aliases.insert(result) {
                        changed = true;
                    }
                    if let Some(source_paths) = paths.get(source).cloned() {
                        if paths.get(&result) != Some(&source_paths) {
                            paths.insert(result, source_paths);
                            changed = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    aliases
}

pub(in crate::passes) fn data_pointer_pointee(
    defs: &HashMap<Word, Instruction>,
    pty: Word,
) -> Option<Word> {
    let pointee = ptr_pointee(defs, pty)?;
    let opaque = matches!(
        defs.get(&pointee).map(|d| d.class.opcode),
        Some(
            Op::TypeImage
                | Op::TypeSampler
                | Op::TypeSampledImage
                | Op::TypeAccelerationStructureKHR
        )
    );
    if opaque {
        None
    } else {
        Some(pointee)
    }
}

pub(in crate::passes) fn access_chains_include_wrapper_member0(
    defs: &HashMap<Word, Instruction>,
    func: &Function,
    pid: Word,
) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    let mut saw_chain = false;
    for blk in &func.blocks {
        for inst in &blk.instructions {
            if matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) {
                let indices = &inst.operands[1..];
                if indices.len() < 2 {
                    return false;
                }
                saw_chain = true;
                let Some(Operand::IdRef(first_index)) = indices.first() else {
                    return false;
                };
                if const_int_literal(defs, *first_index) != Some(0) {
                    return false;
                }
            }
        }
    }
    saw_chain
}

fn const_int_literal(defs: &HashMap<Word, Instruction>, id: Word) -> Option<u64> {
    let inst = defs.get(&id)?;
    if inst.class.opcode != Op::Constant {
        return None;
    }
    match inst.operands.first() {
        Some(Operand::LiteralBit64(v)) => Some(*v),
        Some(Operand::LiteralBit32(v)) => Some(*v as u64),
        _ => None,
    }
}

fn type_after_access_indices(
    defs: &HashMap<Word, Instruction>,
    root_ty: Word,
    indices: &[Operand],
) -> Option<Word> {
    let mut cur = root_ty;
    for idx in indices {
        let def = defs.get(&cur)?;
        cur = match def.class.opcode {
            Op::TypeStruct => {
                let Operand::IdRef(idx_id) = idx else {
                    return None;
                };
                let member = const_int_literal(defs, *idx_id)? as usize;
                match def.operands.get(member) {
                    Some(Operand::IdRef(member_ty)) => *member_ty,
                    _ => return None,
                }
            }
            Op::TypeArray | Op::TypeRuntimeArray | Op::TypeVector => match def.operands.first() {
                Some(Operand::IdRef(elem_ty)) => *elem_ty,
                _ => return None,
            },
            _ => return None,
        };
    }
    Some(cur)
}

fn value_result_type(ctx: &Ctx, func: &Function, value: Word) -> Option<Word> {
    for global in ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
    {
        if global.result_id == Some(value) {
            return global.result_type;
        }
    }
    for param in &func.parameters {
        if param.result_id == Some(value) {
            return param.result_type;
        }
    }
    for block in &func.blocks {
        for inst in &block.instructions {
            if inst.result_id == Some(value) {
                return inst.result_type;
            }
        }
    }
    None
}

pub(in crate::passes) fn access_chain_matches_direct_struct_path(
    defs: &HashMap<Word, Instruction>,
    struct_ty: Word,
    indices: &[Operand],
    result_ptr_ty: Option<Word>,
) -> bool {
    let Some(result_ptr_ty) = result_ptr_ty else {
        return false;
    };
    let Some(result_pointee) = ptr_pointee(defs, result_ptr_ty) else {
        return false;
    };
    type_after_access_indices(defs, struct_ty, indices) == Some(result_pointee)
}

pub(in crate::passes) fn struct_buffer_needs_record_array(
    defs: &HashMap<Word, Instruction>,
    func: &Function,
    pid: Word,
    struct_ty: Word,
) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    for blk in &func.blocks {
        for inst in &blk.instructions {
            if matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) && !access_chain_matches_direct_struct_path(
                defs,
                struct_ty,
                &inst.operands[1..],
                inst.result_type,
            ) {
                return true;
            }
        }
    }
    false
}

pub(in crate::passes) fn buffer_has_access_chains(func: &Function, pid: Word) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    func.blocks.iter().any(|blk| {
        blk.instructions.iter().any(|inst| {
            matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            )
        })
    })
}

pub(in crate::passes) fn buffer_has_multi_index_access_chains(func: &Function, pid: Word) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    func.blocks.iter().any(|blk| {
        blk.instructions.iter().any(|inst| {
            matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) && inst.operands.len() > 2
        })
    })
}

pub(in crate::passes) fn buffer_access_chains_match_struct_path(
    defs: &HashMap<Word, Instruction>,
    func: &Function,
    pid: Word,
    struct_ty: Word,
) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    for blk in &func.blocks {
        for inst in &blk.instructions {
            if matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) && !access_chain_matches_direct_struct_path(
                defs,
                struct_ty,
                &inst.operands[1..],
                inst.result_type,
            ) {
                return false;
            }
        }
    }
    true
}

pub(in crate::passes) fn buffer_has_struct_path_access_chain(
    defs: &HashMap<Word, Instruction>,
    func: &Function,
    pid: Word,
    struct_ty: Word,
) -> bool {
    let aliases = buffer_pointer_aliases(func, pid);
    func.blocks.iter().any(|blk| {
        blk.instructions.iter().any(|inst| {
            matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) && access_chain_matches_direct_struct_path(
                defs,
                struct_ty,
                &inst.operands[1..],
                inst.result_type,
            )
        })
    })
}

pub(in crate::passes) fn body_buf_elem_types(ctx: &Ctx, func: &Function, pid: Word) -> Vec<Word> {
    let defs = type_defs(&ctx.module);
    let aliases = buffer_pointer_aliases(func, pid);
    let mut elements = Vec::new();
    for blk in &func.blocks {
        for inst in &blk.instructions {
            if matches!(
                inst.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::InBoundsPtrAccessChain
            ) && inst.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
            ) && inst.operands.len() == 2
            {
                if let Some(rt) = inst.result_type {
                    if let Some(elem) = ptr_pointee(&defs, rt) {
                        if !elements.contains(&elem) {
                            elements.push(elem);
                        }
                    }
                }
            }
            if inst.class.opcode == Op::Load
                && inst.operands.first().is_some_and(
                    |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
                )
            {
                if let Some(result_type) = inst.result_type {
                    if !elements.contains(&result_type) {
                        elements.push(result_type);
                    }
                }
            }
            if inst.class.opcode == Op::Store
                && inst.operands.first().is_some_and(
                    |operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)),
                )
            {
                if let Some(Operand::IdRef(value)) = inst.operands.get(1) {
                    if let Some(result_type) = value_result_type(ctx, func, *value) {
                        if !elements.contains(&result_type) {
                            elements.push(result_type);
                        }
                    }
                }
            }
        }
    }
    elements
}

pub(in crate::passes) fn body_buf_elem_type(ctx: &Ctx, func: &Function, pid: Word) -> Option<Word> {
    let mut elements = body_buf_elem_types(ctx, func, pid).into_iter();
    let element = elements.next()?;
    elements.next().is_none().then_some(element)
}

pub(in crate::passes) fn body_buf_flat_scalar_element_type(
    ctx: &Ctx,
    func: &Function,
    pid: Word,
) -> Option<Word> {
    let defs = type_defs(&ctx.module);
    let aliases = buffer_pointer_aliases(func, pid);
    let mut element = None;
    let mut saw_access = false;
    for instruction in func.blocks.iter().flat_map(|block| &block.instructions) {
        if !matches!(
            instruction.class.opcode,
            Op::AccessChain
                | Op::InBoundsAccessChain
                | Op::PtrAccessChain
                | Op::InBoundsPtrAccessChain
        ) || !instruction
            .operands
            .first()
            .is_some_and(|operand| matches!(operand, Operand::IdRef(id) if aliases.contains(id)))
        {
            continue;
        }
        let indices = instruction.operands.get(1..)?;
        let [Operand::IdRef(wrapper), _] = indices else {
            return None;
        };
        if const_int_literal(&defs, *wrapper) != Some(0) {
            return None;
        }
        let pointee = instruction
            .result_type
            .and_then(|ty| ptr_pointee(&defs, ty))?;
        if !defs.get(&pointee).is_some_and(|definition| {
            matches!(
                definition.class.opcode,
                Op::TypeInt | Op::TypeFloat | Op::TypeBool
            )
        }) {
            return None;
        }
        if element.is_some_and(|known| known != pointee) {
            return None;
        }
        element = Some(pointee);
        saw_access = true;
    }
    saw_access.then_some(element).flatten()
}

pub(in crate::passes) fn texture_dims(
    ctx: &Ctx,
    entry_idx: usize,
    type_hints: &HashMap<Word, ImageShape>,
) -> HashMap<Word, ImageShape> {
    let names = air_names(&ctx.module);
    let write_textures = write_texture_operands(ctx, entry_idx, &names);
    let mut out = HashMap::default();
    let mut direction_sampled: HashSet<Word> = HashSet::default();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                continue;
            };
            let Some(name) = names.get(callee) else {
                continue;
            };
            if name.starts_with("air.sample_") || name.starts_with("air.gather_") {
                if let Some(Operand::IdRef(tex)) = inst.operands.get(1) {
                    direction_sampled.insert(*tex);
                }
            }
            if !(name.starts_with("air.sample_texture")
                || name.starts_with("air.sample_depth")
                || name.starts_with("air.read_texture")
                || name.starts_with("air.read_depth")
                || name.starts_with("air.is_null_texture")
                || name.starts_with("air.get_width_texture")
                || name.starts_with("air.get_height_texture")
                || name.starts_with("air.get_depth_texture")
                || name.starts_with("air.get_array_size_texture")
                || name.starts_with("air.get_width_depth")
                || name.starts_with("air.get_height_depth")
                || name.starts_with("air.get_depth_depth")
                || name.starts_with("air.get_num_mip_levels_texture")
                || name.starts_with("air.get_num_mip_levels_depth")
                || crate::air_intrinsics::is_image_sample_count_query(name))
            {
                continue;
            }
            let Some(Operand::IdRef(tex)) = inst.operands.get(1) else {
                continue;
            };
            if ((name.starts_with("air.read_texture") || name.starts_with("air.read_depth"))
                || is_size_query(name)
                || name.starts_with("air.is_null_texture"))
                && write_textures.contains(tex)
            {
                continue;
            }

            if name.starts_with("air.get_num_samples_depth_") {
                if let Some(shape) = type_hints.get(tex) {
                    out.insert(*tex, *shape);
                    continue;
                }
            }
            let (dim, arrayed) = sample_dim(name);
            let comp = texture_comp(name)
                .or_else(|| type_hints.get(tex).map(|shape| shape.comp))
                .unwrap_or(ImageComp::Float);
            let multisampled = name.contains("_ms")
                || type_hints
                    .get(tex)
                    .map(|shape| shape.multisampled)
                    .unwrap_or(false);
            let shape = ImageShape {
                dim,
                arrayed,
                comp,
                multisampled,
            };
            out.entry(*tex)
                .and_modify(|e: &mut ImageShape| {
                    if e.comp == ImageComp::Float {
                        e.comp = comp;
                    }
                    e.multisampled |= multisampled;
                })
                .or_insert(shape);
        }
    }
    for (tex, entry) in out.iter_mut() {
        if entry.dim == Dim::DimCube && !entry.arrayed && !direction_sampled.contains(tex) {
            entry.dim = Dim::Dim2D;
            entry.arrayed = true;
        }
    }
    out
}

fn write_texture_operands(
    ctx: &Ctx,
    entry_idx: usize,
    names: &HashMap<Word, String>,
) -> HashSet<Word> {
    let mut out = HashSet::default();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                continue;
            };
            if !names
                .get(callee)
                .map(|name| is_write_texture_name(name))
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(Operand::IdRef(tex)) = inst.operands.get(1) {
                out.insert(*tex);
            }
        }
    }
    out
}

pub(in crate::passes) fn sampled_binding_required_operands(
    ctx: &Ctx,
    entry_idx: usize,
) -> HashSet<Word> {
    let names = air_names(&ctx.module);
    let mut required = HashSet::default();
    for instruction in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter(|instruction| instruction.class.opcode == Op::FunctionCall)
    {
        let callee = match instruction.operands.first() {
            Some(Operand::IdRef(callee)) => *callee,
            _ => continue,
        };
        let Some(name) = names.get(&callee) else {
            continue;
        };
        let Some(Operand::IdRef(texture)) = instruction.operands.get(1) else {
            continue;
        };
        if name.starts_with("air.sample_texture") || name.starts_with("air.sample_depth") {
            required.insert(*texture);
        }
    }
    required
}

fn is_size_query(name: &str) -> bool {
    name.starts_with("air.get_width_texture")
        || name.starts_with("air.get_height_texture")
        || name.starts_with("air.get_depth_texture")
        || name.starts_with("air.get_array_size_texture")
        || name.starts_with("air.get_width_depth")
        || name.starts_with("air.get_height_depth")
        || name.starts_with("air.get_depth_depth")
        || name.starts_with("air.get_num_mip_levels_texture")
        || name.starts_with("air.get_num_mip_levels_depth")
        || crate::air_intrinsics::is_image_sample_count_query(name)
}

pub(in crate::passes) fn write_texture_dims(
    ctx: &Ctx,
    entry_idx: usize,
) -> HashMap<Word, (Dim, bool, ImageFormat, ImageComp)> {
    let names = air_names(&ctx.module);
    let mut out = HashMap::default();
    for blk in &ctx.module.functions[entry_idx].blocks {
        for inst in &blk.instructions {
            if inst.class.opcode != Op::FunctionCall {
                continue;
            }
            let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                continue;
            };
            let Some(name) = names.get(callee) else {
                continue;
            };
            if !is_write_texture_name(name) && !crate::air_intrinsics::is_texture_atomic(name) {
                continue;
            }
            let Some(Operand::IdRef(tex)) = inst.operands.get(1) else {
                continue;
            };
            let (dim, arrayed) = sample_dim(name);
            let (fmt, comp) =
                if crate::air_intrinsics::is_texture_atomic(name) && name.contains(".u.") {
                    (ImageFormat::R32ui, ImageComp::Uint)
                } else if crate::air_intrinsics::is_texture_atomic(name) {
                    (ImageFormat::R32i, ImageComp::Sint)
                } else if name.contains(".u.") && name.contains(".v4i16") {
                    (ImageFormat::Rgba16ui, ImageComp::Uint)
                } else if name.contains(".u.") {
                    (ImageFormat::Rgba8ui, ImageComp::Uint)
                } else if name.contains(".s.") && name.contains(".v4i16") {
                    (ImageFormat::Rgba16i, ImageComp::Sint)
                } else if name.contains(".s.") {
                    (ImageFormat::Rgba8i, ImageComp::Sint)
                } else if name.ends_with(".v4f16") || name.contains(".v4f16") {
                    (ImageFormat::Rgba16f, ImageComp::Float)
                } else {
                    (ImageFormat::Rgba32f, ImageComp::Float)
                };
            if crate::air_intrinsics::is_texture_atomic(name) {
                out.insert(*tex, (dim, arrayed, fmt, comp));
            } else {
                out.entry(*tex).or_insert((dim, arrayed, fmt, comp));
            }
        }
    }
    out
}

fn is_write_texture_name(name: &str) -> bool {
    name.starts_with("air.write_texture")
        || name.starts_with("air.write_imageblock_slice_to_texture")
}

pub(in crate::passes) fn texture_storage_hints(
    params: &[(Word, Word)],
    stage: &Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
) -> HashMap<Word, (Dim, bool, ImageFormat, ImageComp)> {
    params
        .iter()
        .enumerate()
        .filter_map(|(i, (pid, _))| {
            let idx = i as u32;
            let name = match stage {
                Stage::Fragment => frag.and_then(|m| m.texture_type_name(idx)),
                Stage::Vertex => vert.and_then(|m| m.texture_type_name(idx)),
                Stage::Kernel => kern.and_then(|m| m.texture_type_name(idx)),
            }?;
            texture_arg_storage(name).map(|shape| (*pid, shape))
        })
        .collect()
}

fn texture_comp(name: &str) -> Option<ImageComp> {
    if name.contains(".u.") {
        Some(ImageComp::Uint)
    } else if name.contains(".s.") {
        Some(ImageComp::Sint)
    } else if is_size_query(name) || name.starts_with("air.is_null_texture") {
        None
    } else {
        Some(ImageComp::Float)
    }
}

pub(in crate::passes) fn texture_arg_shape(name: &str) -> ImageShape {
    let shape = crate::meta::texture_shape_from_name(name);
    ImageShape {
        dim: shape.dimension.to_spirv_dim(),
        arrayed: shape.arrayed,
        comp: shape.component.to_image_comp(),
        multisampled: shape.multisampled,
    }
}

fn texture_arg_storage(name: &str) -> Option<(Dim, bool, ImageFormat, ImageComp)> {
    // Apple's OpenCL declares every image as texture2d<unknown, ...>: the element type is only in the write itself
    // (air.write_texture_2d.u.v4i32 for write_imageui). Reading "unknown" as float made the storage image Rgba32f and
    // refused every uint/int write ("unsupported texel shape": Geekbench 6 OpenCL Face Detection), so it is no hint
    // and write_texture_dims decides.
    if is_unknown_element(name) {
        return None;
    }
    let shape = crate::meta::texture_shape_from_name(name);
    let fmt = shape.storage_format?.to_spirv_format();
    Some((
        shape.dimension.to_spirv_dim(),
        shape.arrayed,
        fmt,
        shape.component.to_image_comp(),
    ))
}

fn is_unknown_element(name: &str) -> bool {
    name.split_once('<')
        .and_then(|(_, rest)| rest.split(|c: char| c == ',' || c == '>').next())
        .is_some_and(|elem| elem.trim() == "unknown")
}

fn sample_dim(name: &str) -> (Dim, bool) {
    if name.contains("texture_buffer") {
        (Dim::DimBuffer, false)
    } else if name.contains("_1d_array") {
        (Dim::Dim1D, true)
    } else if name.contains("_2d_ms_array") {
        (Dim::Dim2D, true)
    } else if name.contains("_1d") {
        (Dim::Dim1D, false)
    } else if name.contains("_3d") {
        (Dim::Dim3D, false)
    } else if name.contains("_cube_array") {
        (Dim::DimCube, true)
    } else if name.contains("_cube") {
        (Dim::DimCube, false)
    } else if name.contains("_2d_array") {
        (Dim::Dim2D, true)
    } else {
        (Dim::Dim2D, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_discovery_follows_exact_by_value_wrapper_field() {
        let parameter = 10;
        let aggregate = 11;
        let wrapped = 12;
        let extracted = 13;
        let chain = 14;
        let function = Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CompositeInsert,
                        Some(30),
                        Some(wrapped),
                        vec![
                            Operand::IdRef(parameter),
                            Operand::IdRef(aggregate),
                            Operand::LiteralBit32(2),
                            Operand::LiteralBit32(1),
                        ],
                    ),
                    Instruction::new(
                        Op::CompositeExtract,
                        Some(31),
                        Some(extracted),
                        vec![
                            Operand::IdRef(wrapped),
                            Operand::LiteralBit32(2),
                            Operand::LiteralBit32(1),
                        ],
                    ),
                    Instruction::new(
                        Op::AccessChain,
                        Some(32),
                        Some(chain),
                        vec![Operand::IdRef(extracted), Operand::IdRef(40)],
                    ),
                ],
            }],
        };

        let aliases = buffer_pointer_aliases(&function, parameter);
        assert!(aliases.contains(&parameter));
        assert!(aliases.contains(&extracted));
        assert!(buffer_has_access_chains(&function, parameter));
    }

    #[test]
    fn buffer_discovery_follows_transparent_pointer_bitcasts() {
        let parameter = 10;
        let copied = 11;
        let bitcast = 12;
        let chain = 13;
        let function = Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CopyObject,
                        Some(30),
                        Some(copied),
                        vec![Operand::IdRef(parameter)],
                    ),
                    Instruction::new(
                        Op::Bitcast,
                        Some(31),
                        Some(bitcast),
                        vec![Operand::IdRef(copied)],
                    ),
                    Instruction::new(
                        Op::PtrAccessChain,
                        Some(32),
                        Some(chain),
                        vec![Operand::IdRef(bitcast), Operand::IdRef(40)],
                    ),
                ],
            }],
        };

        let aliases = buffer_pointer_aliases(&function, parameter);
        assert!(aliases.contains(&parameter));
        assert!(aliases.contains(&copied));
        assert!(aliases.contains(&bitcast));
        assert!(buffer_has_access_chains(&function, parameter));
    }

    #[test]
    fn buffer_element_discovery_includes_pointer_arithmetic_views() {
        let mut module = Module::new();
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
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
            Instruction::new(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(3),
                ],
            ),
        ];
        module.functions.push(Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                instructions: vec![
                    Instruction::new(Op::CopyObject, Some(4), Some(11), vec![Operand::IdRef(10)]),
                    Instruction::new(
                        Op::PtrAccessChain,
                        Some(4),
                        Some(12),
                        vec![Operand::IdRef(11), Operand::IdRef(30)],
                    ),
                    Instruction::new(
                        Op::AccessChain,
                        Some(5),
                        Some(13),
                        vec![Operand::IdRef(11), Operand::IdRef(31)],
                    ),
                ],
            }],
        });
        let ctx = Ctx::new(module);

        assert_eq!(
            body_buf_elem_types(&ctx, &ctx.module.functions[0], 10),
            vec![1, 3]
        );
    }

    #[test]
    fn buffer_flat_element_view_is_none_when_the_body_reads_several_types() {
        let mut module = Module::new();
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::LiteralBit32(4)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(3),
                vec![Operand::IdRef(1), Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(4),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(2),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(5),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(3),
                ],
            ),
        ];
        let member_chains = vec![
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(4),
                Some(11),
                vec![Operand::IdRef(10), Operand::IdRef(30)],
            ),
            Instruction::new(
                Op::InBoundsAccessChain,
                Some(5),
                Some(12),
                vec![Operand::IdRef(10), Operand::IdRef(31)],
            ),
        ];
        module.functions.push(Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(20), vec![])),
                instructions: member_chains.clone(),
            }],
        });
        module.functions.push(Function {
            def: None,
            end: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(21), vec![])),
                instructions: member_chains[..1].to_vec(),
            }],
        });
        let ctx = Ctx::new(module);

        assert_eq!(body_buf_elem_type(&ctx, &ctx.module.functions[0], 10), None);
        assert_eq!(
            body_buf_elem_type(&ctx, &ctx.module.functions[1], 10),
            Some(2)
        );
    }

    #[test]
    fn texture_arg_comp_reads_nested_texture_array_scalar() {
        assert_eq!(
            texture_arg_shape("array<texture2d<uint, sample>, 2>").comp,
            ImageComp::Uint
        );
        assert_eq!(
            texture_arg_shape("array<texture2d<int, sample>, 2>").comp,
            ImageComp::Sint
        );
        assert_eq!(
            texture_arg_shape("array<texture2d<half, sample>, 2>").comp,
            ImageComp::Float
        );
    }
}
