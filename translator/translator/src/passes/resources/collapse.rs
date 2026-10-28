use super::rewrites::*;
use super::*;
use crate::passes::access::{is_unsigned_byte_scalar, single_member_array_scalar_elem};
use crate::passes::stage_input::{const_ivec, BufWrap, ParamBinding};

pub(in crate::passes) fn apply_bindings(
    ctx: &mut Ctx,
    entry_idx: usize,
    bindings: Vec<(Word, ParamBinding)>,
    buffer_structs: &[(Word, Word)],
    defs: &HashMap<Word, Instruction>,
) -> Result<(), String> {
    let mut loads: Vec<Instruction> = vec![];
    let mut splices: Vec<(Word, Word)> = vec![];
    let mut resource_values: HashSet<Word> = HashSet::default();
    let mut buffer_param_ids: Vec<Word> = vec![];
    let mut buffer_root_splices: Vec<(Word, Word)> = vec![];
    let mut collapsed_buffers: Vec<(Word, Word, Word, bool, Vec<(Word, Word)>)> = vec![];
    let mut record_array_buffers: Vec<(Word, Word, Word, Word)> = vec![];
    let mut nested_air_ordinal_roots = HashSet::default();
    let mut zero_pointer_vars: Vec<Word> = vec![];
    let mut workgroup_vars: Vec<Word> = vec![];

    for (pid, b) in bindings {
        match b {
            ParamBinding::LoadVar { var, ty } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                splices.push((pid, lid));
            }
            ParamBinding::LoadVarBoolFromUint { var, bool_ty } => {
                let uint_ty = ctx.ty_uint();
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(uint_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let converted = ctx.module.fresh_id();
                let zero = ctx.const_uint(0);
                loads.push(Instruction::new(
                    Op::INotEqual,
                    Some(bool_ty),
                    Some(converted),
                    vec![Operand::IdRef(lid), Operand::IdRef(zero)],
                ));
                splices.push((pid, converted));
            }
            ParamBinding::LoadVarBoolToInt {
                var,
                bool_ty,
                int_ty,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(bool_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let one = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Constant,
                    Some(int_ty),
                    Some(one),
                    vec![Operand::LiteralBit32(1)],
                ));
                let zero = ctx.module.fresh_id();
                ctx.new_globals.push(Instruction::new(
                    Op::Constant,
                    Some(int_ty),
                    Some(zero),
                    vec![Operand::LiteralBit32(0)],
                ));
                let converted = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Select,
                    Some(int_ty),
                    Some(converted),
                    vec![
                        Operand::IdRef(lid),
                        Operand::IdRef(one),
                        Operand::IdRef(zero),
                    ],
                ));
                splices.push((pid, converted));
            }
            ParamBinding::LoadVarConverted {
                var,
                load_ty,
                param_ty,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(load_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let convert_op = if param_is_float(ctx, param_ty) {
                    Op::FConvert
                } else {
                    Op::UConvert
                };
                let cid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    convert_op,
                    Some(param_ty),
                    Some(cid),
                    vec![Operand::IdRef(lid)],
                ));
                splices.push((pid, cid));
            }
            ParamBinding::LoadVarBitcast {
                var,
                load_ty,
                param_ty,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(load_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let bid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Bitcast,
                    Some(param_ty),
                    Some(bid),
                    vec![Operand::IdRef(lid)],
                ));
                splices.push((pid, bid));
            }
            ParamBinding::LoadVarBitAnd {
                var,
                load_ty,
                param_ty,
                mask,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(load_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let masked = ctx.module.fresh_id();
                let mask = ctx.const_uint(mask);
                loads.push(Instruction::new(
                    Op::BitwiseAnd,
                    Some(load_ty),
                    Some(masked),
                    vec![Operand::IdRef(lid), Operand::IdRef(mask)],
                ));
                if param_ty == load_ty {
                    splices.push((pid, masked));
                } else {
                    let cid = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(param_ty),
                        Some(cid),
                        vec![Operand::IdRef(masked)],
                    ));
                    splices.push((pid, cid));
                }
            }
            ParamBinding::LoadVarShiftRight {
                var,
                load_ty,
                param_ty,
                shift,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(load_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                let shifted = ctx.module.fresh_id();
                let shift = ctx.const_uint(shift);
                loads.push(Instruction::new(
                    Op::ShiftRightLogical,
                    Some(load_ty),
                    Some(shifted),
                    vec![Operand::IdRef(lid), Operand::IdRef(shift)],
                ));
                if param_ty == load_ty {
                    splices.push((pid, shifted));
                } else {
                    let cid = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(param_ty),
                        Some(cid),
                        vec![Operand::IdRef(shifted)],
                    ));
                    splices.push((pid, cid));
                }
            }
            ParamBinding::LoadVarComponent {
                var,
                vec_ty,
                scalar_ty,
                out_ty,
                comp,
            } => {
                let vid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(vec_ty),
                    Some(vid),
                    vec![Operand::IdRef(var)],
                ));
                let cid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(scalar_ty),
                    Some(cid),
                    vec![Operand::IdRef(vid), Operand::LiteralBit32(comp)],
                ));
                if scalar_ty == out_ty {
                    splices.push((pid, cid));
                } else {
                    let converted = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(out_ty),
                        Some(converted),
                        vec![Operand::IdRef(cid)],
                    ));
                    splices.push((pid, converted));
                }
            }
            ParamBinding::LoadVarVectorPrefix {
                var,
                vec_ty,
                scalar_ty,
                prefix_ty,
                out_ty,
                lanes,
            } => {
                let vid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(vec_ty),
                    Some(vid),
                    vec![Operand::IdRef(var)],
                ));
                let mut components = Vec::with_capacity(lanes as usize);
                for lane in 0..lanes {
                    let cid = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(scalar_ty),
                        Some(cid),
                        vec![Operand::IdRef(vid), Operand::LiteralBit32(lane)],
                    ));
                    components.push(Operand::IdRef(cid));
                }
                let vector = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeConstruct,
                    Some(prefix_ty),
                    Some(vector),
                    components,
                ));
                if prefix_ty == out_ty {
                    splices.push((pid, vector));
                } else {
                    let converted = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(out_ty),
                        Some(converted),
                        vec![Operand::IdRef(vector)],
                    ));
                    splices.push((pid, converted));
                }
            }
            ParamBinding::LoadThreadsPerGrid {
                var,
                vec_ty,
                out_ty,
                lanes,
            } => {
                let vid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(vec_ty),
                    Some(vid),
                    vec![Operand::IdRef(var)],
                ));
                let uint_ty = ctx.ty_uint();
                let local_size = ctx.kernel_local_size;
                let mut components = Vec::with_capacity(lanes as usize);
                for lane in 0..lanes {
                    let workgroups = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(uint_ty),
                        Some(workgroups),
                        vec![Operand::IdRef(vid), Operand::LiteralBit32(lane)],
                    ));
                    let product = ctx.module.fresh_id();
                    let local = ctx.const_uint(local_size[lane as usize]);
                    loads.push(Instruction::new(
                        Op::IMul,
                        Some(uint_ty),
                        Some(product),
                        vec![Operand::IdRef(workgroups), Operand::IdRef(local)],
                    ));
                    components.push(Operand::IdRef(product));
                }
                if lanes == 1 {
                    let product = match &components[0] {
                        Operand::IdRef(product) => *product,
                        _ => {
                            return Err(
                                "apply_bindings: dispatch-index component is not an id".to_string()
                            )
                        }
                    };
                    if out_ty == uint_ty {
                        splices.push((pid, product));
                    } else {
                        let converted = ctx.module.fresh_id();
                        loads.push(Instruction::new(
                            Op::UConvert,
                            Some(out_ty),
                            Some(converted),
                            vec![Operand::IdRef(product)],
                        ));
                        splices.push((pid, converted));
                    }
                } else {
                    let prefix_ty = ctx.ty_vec_uint(lanes);
                    let vector = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(prefix_ty),
                        Some(vector),
                        components,
                    ));
                    if prefix_ty == out_ty {
                        splices.push((pid, vector));
                    } else {
                        let converted = ctx.module.fresh_id();
                        loads.push(Instruction::new(
                            Op::UConvert,
                            Some(out_ty),
                            Some(converted),
                            vec![Operand::IdRef(vector)],
                        ));
                        splices.push((pid, converted));
                    }
                }
            }
            ParamBinding::LoadKernelDispatchField {
                var,
                first_member,
                out_ty,
                lanes,
            } => {
                let value = materialize_kernel_dispatch_field(
                    ctx,
                    &mut loads,
                    var,
                    first_member,
                    out_ty,
                    lanes,
                )?;
                splices.push((pid, value));
            }
            ParamBinding::LoadBuiltinPlusKernelDispatchField {
                builtin_var,
                dispatch_var,
                first_member,
                out_ty,
                lanes,
            } => {
                let uint_ty = ctx.ty_uint();
                let v3uint_ty = ctx.ty_vec_uint(3);
                let builtin = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(v3uint_ty),
                    Some(builtin),
                    vec![Operand::IdRef(builtin_var)],
                ));
                let mut components = Vec::with_capacity(lanes as usize);
                for lane in 0..lanes {
                    let relative = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(uint_ty),
                        Some(relative),
                        vec![Operand::IdRef(builtin), Operand::LiteralBit32(lane)],
                    ));
                    let base = load_kernel_dispatch_component(
                        ctx,
                        &mut loads,
                        dispatch_var,
                        first_member + lane,
                    );
                    let absolute = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::IAdd,
                        Some(uint_ty),
                        Some(absolute),
                        vec![Operand::IdRef(relative), Operand::IdRef(base)],
                    ));
                    components.push(absolute);
                }
                let value = if lanes == 1 {
                    components[0]
                } else {
                    let vector_ty = ctx.ty_vec_uint(lanes);
                    let vector = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(vector_ty),
                        Some(vector),
                        components.into_iter().map(Operand::IdRef).collect(),
                    ));
                    vector
                };
                let value_ty = if lanes == 1 {
                    uint_ty
                } else {
                    ctx.ty_vec_uint(lanes)
                };
                if value_ty == out_ty {
                    splices.push((pid, value));
                } else {
                    let converted = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(out_ty),
                        Some(converted),
                        vec![Operand::IdRef(value)],
                    ));
                    splices.push((pid, converted));
                }
            }
            ParamBinding::LoadKernelLocalSize { out_ty, lanes }
            | ParamBinding::LoadKernelRequestedLocalSize { out_ty, lanes } => {
                let ids = match b {
                    ParamBinding::LoadKernelRequestedLocalSize { .. } => {
                        ctx.kernel_requested_local_size_ids()
                    }
                    _ => ctx.kernel_local_size_ids(),
                };
                let uint_ty = ctx.ty_uint();
                let value = if lanes == 1 {
                    ids[0]
                } else {
                    let vector_ty = ctx.ty_vec_uint(lanes);
                    let vector = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(vector_ty),
                        Some(vector),
                        ids[..lanes as usize]
                            .iter()
                            .copied()
                            .map(Operand::IdRef)
                            .collect(),
                    ));
                    vector
                };
                let value_ty = if lanes == 1 {
                    uint_ty
                } else {
                    ctx.ty_vec_uint(lanes)
                };
                if value_ty == out_ty {
                    splices.push((pid, value));
                } else {
                    let converted = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(out_ty),
                        Some(converted),
                        vec![Operand::IdRef(value)],
                    ));
                    splices.push((pid, converted));
                }
            }
            ParamBinding::LoadKernelGroupsPerThreadgroup { out_ty, lanes } => {
                let uint_ty = ctx.ty_uint();
                let [local_x, local_y, local_z] = ctx.kernel_local_size_ids();
                let local_xy = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::IMul,
                    Some(uint_ty),
                    Some(local_xy),
                    vec![Operand::IdRef(local_x), Operand::IdRef(local_y)],
                ));
                let local_threads = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::IMul,
                    Some(uint_ty),
                    Some(local_threads),
                    vec![Operand::IdRef(local_xy), Operand::IdRef(local_z)],
                ));
                let rounded = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::IAdd,
                    Some(uint_ty),
                    Some(rounded),
                    vec![
                        Operand::IdRef(local_threads),
                        Operand::IdRef(ctx.const_uint(lanes - 1)),
                    ],
                ));
                let groups = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::UDiv,
                    Some(uint_ty),
                    Some(groups),
                    vec![
                        Operand::IdRef(rounded),
                        Operand::IdRef(ctx.const_uint(lanes)),
                    ],
                ));
                if out_ty == uint_ty {
                    splices.push((pid, groups));
                } else {
                    let converted = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(out_ty),
                        Some(converted),
                        vec![Operand::IdRef(groups)],
                    ));
                    splices.push((pid, converted));
                }
            }
            ParamBinding::FragmentImageblockProjection {
                coord_var,
                param_ty,
                members,
            } => {
                let v4float = ctx.ty_vecf(4);
                let float_ty = ctx.ty_float();
                let sint_ty = ctx.ty_sint();
                let v2sint = ctx.ty_vec_sint(2);
                let coord_value = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(v4float),
                    Some(coord_value),
                    vec![Operand::IdRef(coord_var)],
                ));
                let mut coord_components = Vec::with_capacity(2);
                for component in 0..2 {
                    let float_component = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(float_ty),
                        Some(float_component),
                        vec![
                            Operand::IdRef(coord_value),
                            Operand::LiteralBit32(component),
                        ],
                    ));
                    let int_component = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::ConvertFToS,
                        Some(sint_ty),
                        Some(int_component),
                        vec![Operand::IdRef(float_component)],
                    ));
                    coord_components.push(Operand::IdRef(int_component));
                }
                let coord = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeConstruct,
                    Some(v2sint),
                    Some(coord),
                    coord_components,
                ));
                let mut projected_values = Vec::with_capacity(members.len());
                for (image_var, image_ty, member_ty, format) in members {
                    let image = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::Load,
                        Some(image_ty),
                        Some(image),
                        vec![Operand::IdRef(image_var)],
                    ));
                    let texel_ty = match format.component {
                        ImageComp::Float => ctx.ty_vecf(4),
                        ImageComp::Uint => ctx.ty_vec_uint(4),
                        ImageComp::Sint => ctx.ty_vec_sint(4),
                    };
                    let texel = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::ImageRead,
                        Some(texel_ty),
                        Some(texel),
                        vec![Operand::IdRef(image), Operand::IdRef(coord)],
                    ));
                    let wide_ty = if format.lanes == 1 {
                        match format.component {
                            ImageComp::Float => float_ty,
                            ImageComp::Uint => ctx.ty_uint(),
                            ImageComp::Sint => ctx.ty_sint(),
                        }
                    } else {
                        texel_ty
                    };
                    let wide = if format.lanes == 1 {
                        let component = ctx.module.fresh_id();
                        loads.push(Instruction::new(
                            Op::CompositeExtract,
                            Some(wide_ty),
                            Some(component),
                            vec![Operand::IdRef(texel), Operand::LiteralBit32(0)],
                        ));
                        component
                    } else {
                        texel
                    };
                    let projected = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        match format.component {
                            ImageComp::Float => Op::FConvert,
                            ImageComp::Uint => Op::UConvert,
                            ImageComp::Sint => Op::SConvert,
                        },
                        Some(member_ty),
                        Some(projected),
                        vec![Operand::IdRef(wide)],
                    ));
                    projected_values.push(Operand::IdRef(projected));
                }
                let projection = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeConstruct,
                    Some(param_ty),
                    Some(projection),
                    projected_values,
                ));
                splices.push((pid, projection));
            }
            ParamBinding::Sampler {
                var,
                specialized_state,
            } => {
                let sty = ctx.ty_sampler();
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(sty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                if let Some(state) = specialized_state {
                    ctx.sampler_states.insert(lid, state);
                    ctx.specialized_runtime_sampler_values.insert(lid);
                }
                resource_values.insert(lid);
                splices.push((pid, lid));
            }
            ParamBinding::HeapImage {
                slot,
                image_ty,
                dim,
                comp,
                multisampled,
            } => {
                let (table, word_ptr_ty, wide) = ensure_address_table(ctx)?;
                let uint_ty = ctx.ty_uint();
                let c0 = ctx.const_uint(0);
                let cel = ctx.const_uint(TEXTURE_INDEX_BASE + slot);
                let wptr = ctx.module.fresh_id();
                let word = ctx.module.fresh_id();
                if let Some(u64_ty) = wide {
                    loads.push(Instruction::new(
                        Op::AccessChain,
                        Some(word_ptr_ty),
                        Some(wptr),
                        vec![
                            Operand::IdRef(table),
                            Operand::IdRef(c0),
                            Operand::IdRef(cel),
                        ],
                    ));
                    let wide_v = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::Load,
                        Some(u64_ty),
                        Some(wide_v),
                        vec![Operand::IdRef(wptr)],
                    ));
                    loads.push(Instruction::new(
                        Op::UConvert,
                        Some(uint_ty),
                        Some(word),
                        vec![Operand::IdRef(wide_v)],
                    ));
                } else {
                    loads.push(Instruction::new(
                        Op::AccessChain,
                        Some(word_ptr_ty),
                        Some(wptr),
                        vec![
                            Operand::IdRef(table),
                            Operand::IdRef(c0),
                            Operand::IdRef(cel),
                            Operand::IdRef(c0),
                        ],
                    ));
                    loads.push(Instruction::new(
                        Op::Load,
                        Some(uint_ty),
                        Some(word),
                        vec![Operand::IdRef(wptr)],
                    ));
                }
                let mask = ctx.const_uint(crate::reflect::BINDLESS_HEAP_SLOTS - 1);
                let idx = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::BitwiseAnd,
                    Some(uint_ty),
                    Some(idx),
                    vec![Operand::IdRef(word), Operand::IdRef(mask)],
                ));
                let heap = crate::passes::stage_input::bindless_heap_var(ctx, image_ty, dim, comp);
                let iptr_ty = ctx.ty_ptr(StorageClass::UniformConstant, image_ty);
                let iptr = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::AccessChain,
                    Some(iptr_ty),
                    Some(iptr),
                    vec![Operand::IdRef(heap), Operand::IdRef(idx)],
                ));
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(image_ty),
                    Some(lid),
                    vec![Operand::IdRef(iptr)],
                ));
                ctx.image_dims.insert(lid, dim);
                ctx.image_comp.insert(lid, comp);
                if multisampled {
                    ctx.image_multisampled.insert(lid);
                }
                resource_values.insert(lid);
                splices.push((pid, lid));
            }
            ParamBinding::Image {
                var,
                image_ty,
                dim,
                comp,
                multisampled,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(image_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                ctx.image_dims.insert(lid, dim);
                ctx.image_comp.insert(lid, comp);
                if multisampled {
                    ctx.image_multisampled.insert(lid);
                }
                resource_values.insert(lid);
                splices.push((pid, lid));
            }
            ParamBinding::ImageArray {
                var,
                elem_image_ty,
                dim,
                comp,
                multisampled,
                runtime_specialization,
            } => {
                ctx.image_array_vars
                    .insert(var, (elem_image_ty, dim, comp, multisampled));
                if let Some((metal_index, state)) = runtime_specialization {
                    ctx.register_runtime_storage_image_value(var, metal_index, Some(state));
                }
                remove_dead_local_image_array_memcpy(ctx, entry_idx, pid);
                resource_values.insert(var);
                splices.push((pid, var));
            }
            ParamBinding::StorageImage {
                var,
                image_ty,
                dim,
                comp,
                runtime_specialization,
            } => {
                let lid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(image_ty),
                    Some(lid),
                    vec![Operand::IdRef(var)],
                ));
                ctx.image_dims.insert(lid, dim);
                ctx.image_comp.insert(lid, comp);
                ctx.image_storage.insert(lid);
                if let Some((metal_index, state)) = runtime_specialization {
                    ctx.register_runtime_storage_image_value(var, metal_index, Some(state));
                    ctx.register_runtime_storage_image_value(lid, metal_index, Some(state));
                }
                resource_values.insert(lid);
                splices.push((pid, lid));
            }
            ParamBinding::InputAttachment {
                var,
                image_ty,
                read_ty,
                param_ty,
            } => {
                let image = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(image_ty),
                    Some(image),
                    vec![Operand::IdRef(var)],
                ));
                let coord_ty = ctx.ty_vec_sint(2);
                let coord = const_ivec(ctx, coord_ty, &[0, 0]);
                let read = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::ImageRead,
                    Some(read_ty),
                    Some(read),
                    vec![Operand::IdRef(image), Operand::IdRef(coord)],
                ));
                let value = adapt_input_attachment_read(ctx, &mut loads, read, read_ty, param_ty)?;
                splices.push((pid, value));
            }
            ParamBinding::Buffer { var, wrap } => {
                ctx.bound_buffer_vars.insert(var);
                buffer_root_splices.push((pid, var));
                let raw_block_ty = match wrap {
                    BufWrap::Direct => None,
                    BufWrap::Collapsed { block_ty, .. } | BufWrap::RecordArray { block_ty, .. } => {
                        Some(block_ty)
                    }
                };
                if let Some(source_pointee) = raw_block_ty
                    .filter(|block_ty| {
                        single_member_array_scalar_elem(ctx, *block_ty)
                            .is_some_and(|element| is_unsigned_byte_scalar(ctx, element))
                    })
                    .and_then(|_| value_result_type(ctx, pid))
                    .and_then(|pointer_ty| ptr_pointee(defs, pointer_ty))
                {
                    ctx.emit_sidecar
                        .buffer_root_source_types
                        .entry(var)
                        .or_insert(source_pointee);
                }
                match wrap {
                    BufWrap::Direct => {
                        splices.push((pid, var));
                        buffer_param_ids.push(var);
                    }
                    BufWrap::Collapsed {
                        block_ty,
                        prepend_member0,
                        typed_aliases,
                    } => {
                        resource_values.insert(pid);
                        collapsed_buffers.push((
                            pid,
                            var,
                            block_ty,
                            prepend_member0,
                            typed_aliases,
                        ));
                    }
                    BufWrap::RecordArray { block_ty, elem_ty } => {
                        resource_values.insert(pid);
                        record_array_buffers.push((pid, var, block_ty, elem_ty));
                    }
                }
            }
            ParamBinding::StageInput {
                var,
                value_ty,
                index_var,
                dispatch_var,
            } => {
                let v3u = ctx.ty_vec_uint(3);
                let uint_ty = ctx.ty_uint();
                let gid = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(v3u),
                    Some(gid),
                    vec![Operand::IdRef(index_var)],
                ));
                let relative_x = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(uint_ty),
                    Some(relative_x),
                    vec![Operand::IdRef(gid), Operand::LiteralBit32(0)],
                ));
                let x = if let Some(dispatch_var) = dispatch_var {
                    let base = load_kernel_dispatch_component(ctx, &mut loads, dispatch_var, 3);
                    let absolute = ctx.module.fresh_id();
                    loads.push(Instruction::new(
                        Op::IAdd,
                        Some(uint_ty),
                        Some(absolute),
                        vec![Operand::IdRef(relative_x), Operand::IdRef(base)],
                    ));
                    absolute
                } else {
                    relative_x
                };
                let zero = ctx.const_uint(0);
                let ptr_ty = ctx.ty_ptr(StorageClass::StorageBuffer, value_ty);
                let ptr = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::AccessChain,
                    Some(ptr_ty),
                    Some(ptr),
                    vec![Operand::IdRef(var), Operand::IdRef(zero), Operand::IdRef(x)],
                ));
                let value = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::Load,
                    Some(value_ty),
                    Some(value),
                    vec![Operand::IdRef(ptr)],
                ));
                splices.push((pid, value));
            }
            ParamBinding::WorkgroupMemory { var } => {
                splices.push((pid, var));
                workgroup_vars.push(var);
            }
            ParamBinding::Value { val } => {
                splices.push((pid, val));
            }
            ParamBinding::ZeroValue { val } => {
                splices.push((pid, val));
            }
            ParamBinding::ZeroPointer { var } => {
                splices.push((pid, var));
                zero_pointer_vars.push(var);
            }
        }
    }

    ctx.module.functions[entry_idx].parameters.clear();

    if !loads.is_empty() {
        let func = &mut ctx.module.functions[entry_idx];
        if let Some(first) = func.blocks.first_mut() {
            let at = first
                .instructions
                .iter()
                .position(|i| i.class.opcode != Op::Variable)
                .unwrap_or(first.instructions.len());
            for (k, ld) in loads.into_iter().enumerate() {
                first.instructions.insert(at + k, ld);
            }
        }
    }

    ctx.emit_sidecar
        .remap_ids(&splices.iter().copied().collect());

    {
        let func = &mut ctx.module.functions[entry_idx];
        for (from, to) in splices {
            replace_id_in_function(func, from, to);
        }
    }
    let _ = collapse_resource_wrappers(ctx, entry_idx, &resource_values);
    ctx.emit_sidecar
        .remap_ids(&buffer_root_splices.iter().copied().collect());

    for (pid, var, block_ty, prepend_member0, typed_aliases) in collapsed_buffers {
        rewrite_collapsed_buffer(
            ctx,
            entry_idx,
            pid,
            var,
            block_ty,
            prepend_member0,
            &typed_aliases,
            defs,
        );
    }
    for (pid, var, block_ty, elem_ty) in record_array_buffers {
        nested_air_ordinal_roots.extend(rewrite_record_array_buffer(
            ctx, entry_idx, pid, var, block_ty, elem_ty, defs,
        ));
    }
    if !nested_air_ordinal_roots.is_empty() {
        remap_nested_air_struct_accesses(ctx, entry_idx, &nested_air_ordinal_roots, defs);
    }

    let _ = &buffer_param_ids;
    if !buffer_structs.is_empty() {
        let roots: Vec<Word> = buffer_structs.iter().map(|(v, _)| *v).collect();
        rewrite_pointer_storage(ctx, entry_idx, &roots, StorageClass::StorageBuffer, defs)?;
        rewrite_raw_word_alias_chains(ctx, entry_idx, buffer_structs, defs)?;
    }
    if !zero_pointer_vars.is_empty() {
        rewrite_private_zero_root_loads(ctx, &zero_pointer_vars);
        rewrite_pointer_storage(
            ctx,
            entry_idx,
            &zero_pointer_vars,
            StorageClass::Private,
            defs,
        )?;
        rewrite_private_pointer_atomics(ctx);
    }
    if !workgroup_vars.is_empty() {
        for var in &workgroup_vars {
            rewrite_workgroup_root_access(ctx, entry_idx, *var, defs);
        }
        rewrite_pointer_storage(
            ctx,
            entry_idx,
            &workgroup_vars,
            StorageClass::Workgroup,
            defs,
        )?;
        rewrite_flattened_workgroup_leaf_accesses(ctx, entry_idx, &workgroup_vars, defs);
    }
    rewrite_structural_result_types(ctx, entry_idx, defs);
    rewrite_ulong_uint2_memory_reinterprets(ctx, entry_idx, defs);
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    let _ = materialize_interface_selected_loads(&mut ctx.module);
    let _ = crate::native::construct_interface_cross_binding_pointer_values_module(&mut ctx.module);
    if let Some(address_table) =
        crate::native::construct_interface_cross_binding_pointer_phis_module(
            &mut ctx.module,
            ctx.descriptor_layout,
        )
    {
        ctx.interface_buffer_var(address_table);
    }
    Ok(())
}

fn remove_dead_local_image_array_memcpy(ctx: &mut Ctx, entry_idx: usize, param: Word) {
    let memcpy_ids = ctx
        .module
        .debug_names
        .iter()
        .filter_map(|instruction| match instruction.operands.as_slice() {
            [Operand::IdRef(id), Operand::LiteralString(name), ..]
                if instruction.class.opcode == Op::Name && name.starts_with("llvm.memcpy.") =>
            {
                Some(*id)
            }
            _ => None,
        })
        .collect::<HashSet<_>>();
    let mut dead = Vec::new();
    for (block_index, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let (
                Some(Operand::IdRef(callee)),
                Some(Operand::IdRef(destination)),
                Some(Operand::IdRef(source)),
            ) = (
                instruction.operands.first(),
                instruction.operands.get(1),
                instruction.operands.get(2),
            )
            else {
                continue;
            };
            if instruction.class.opcode == Op::FunctionCall
                && *source == param
                && memcpy_ids.contains(callee)
                && local_copy_destination_is_write_only(
                    &ctx.module.functions[entry_idx],
                    *destination,
                    *callee,
                )
            {
                dead.push((block_index, instruction_index));
            }
        }
    }
    for (block_index, instruction_index) in dead.into_iter().rev() {
        ctx.module.functions[entry_idx].blocks[block_index]
            .instructions
            .remove(instruction_index);
    }
}

fn local_copy_destination_is_write_only(
    function: &crate::spirv_module::Function,
    destination: Word,
    memcpy: Word,
) -> bool {
    let mut region = crate::fxhash::set_from([destination]);
    let mut changed = true;
    while changed {
        changed = false;
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let derived = matches!(
                instruction.class.opcode,
                Op::AccessChain
                    | Op::InBoundsAccessChain
                    | Op::PtrAccessChain
                    | Op::Bitcast
                    | Op::CopyObject
            ) && instruction.operands.first().is_some_and(
                |operand| matches!(operand, Operand::IdRef(id) if region.contains(id)),
            );
            if derived {
                if let Some(result) = instruction.result_id {
                    changed |= region.insert(result);
                }
            }
        }
    }

    for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
        for (operand_index, operand) in instruction.operands.iter().enumerate() {
            let Operand::IdRef(id) = operand else {
                continue;
            };
            if !region.contains(id) {
                continue;
            }
            let allowed_derivation = operand_index == 0
                && matches!(
                    instruction.class.opcode,
                    Op::AccessChain
                        | Op::InBoundsAccessChain
                        | Op::PtrAccessChain
                        | Op::Bitcast
                        | Op::CopyObject
                );
            let allowed_store = instruction.class.opcode == Op::Store && operand_index == 0;
            let allowed_memcpy_destination = instruction.class.opcode == Op::FunctionCall
                && operand_index == 1
                && matches!(instruction.operands.first(), Some(Operand::IdRef(id)) if *id == memcpy);
            if !(allowed_derivation || allowed_store || allowed_memcpy_destination) {
                return false;
            }
        }
    }
    true
}

fn adapt_input_attachment_read(
    ctx: &mut Ctx,
    loads: &mut Vec<Instruction>,
    read: Word,
    read_ty: Word,
    param_ty: Word,
) -> Result<Word, String> {
    if read_ty == param_ty {
        return Ok(read);
    }

    let Some((read_component, Some(read_lanes))) = scalar_or_vector_component_live(ctx, read_ty)
    else {
        return Err(format!(
            "input attachment read type {read_ty} is not a vector"
        ));
    };
    let (param_component, param_lanes) = scalar_or_vector_component_live(ctx, param_ty)
        .ok_or_else(|| format!("input attachment param type {param_ty} is not scalar/vector"))?;
    let needed_lanes = param_lanes.unwrap_or(1);
    if needed_lanes > read_lanes {
        return Err(format!(
            "input attachment param type {param_ty} needs {needed_lanes} components, read type {read_ty} has {read_lanes}"
        ));
    }

    let (shaped, shaped_ty) = match param_lanes {
        None => {
            let extracted = ctx.module.fresh_id();
            loads.push(Instruction::new(
                Op::CompositeExtract,
                Some(read_component),
                Some(extracted),
                vec![Operand::IdRef(read), Operand::LiteralBit32(0)],
            ));
            (extracted, read_component)
        }
        Some(lanes) if lanes == read_lanes => (read, read_ty),
        Some(lanes) => {
            let prefix_ty = input_attachment_vector_type(ctx, read_component, lanes);
            let mut components = Vec::with_capacity(lanes as usize);
            for lane in 0..lanes {
                let extracted = ctx.module.fresh_id();
                loads.push(Instruction::new(
                    Op::CompositeExtract,
                    Some(read_component),
                    Some(extracted),
                    vec![Operand::IdRef(read), Operand::LiteralBit32(lane)],
                ));
                components.push(Operand::IdRef(extracted));
            }
            let vector = ctx.module.fresh_id();
            loads.push(Instruction::new(
                Op::CompositeConstruct,
                Some(prefix_ty),
                Some(vector),
                components,
            ));
            (vector, prefix_ty)
        }
    };
    if shaped_ty == param_ty {
        return Ok(shaped);
    }

    if !is_float_type_live(ctx, read_component) || !is_float_type_live(ctx, param_component) {
        return Err(format!(
            "input attachment read type {read_ty} cannot be converted to param type {param_ty}"
        ));
    }

    let converted = ctx.module.fresh_id();
    loads.push(Instruction::new(
        Op::FConvert,
        Some(param_ty),
        Some(converted),
        vec![Operand::IdRef(shaped)],
    ));
    Ok(converted)
}

fn scalar_or_vector_component_live(ctx: &Ctx, ty: Word) -> Option<(Word, Option<u32>)> {
    let def = type_def_of(ctx, ty)?;
    match def.class.opcode {
        Op::TypeVector => {
            let component = match def.operands.first()? {
                Operand::IdRef(component) => *component,
                _ => return None,
            };
            let lanes = match def.operands.get(1)? {
                Operand::LiteralBit32(lanes) => *lanes,
                _ => return None,
            };
            Some((component, Some(lanes)))
        }
        Op::TypeFloat | Op::TypeInt | Op::TypeBool => Some((ty, None)),
        _ => None,
    }
}

fn is_float_type_live(ctx: &Ctx, ty: Word) -> bool {
    type_def_of(ctx, ty).is_some_and(|def| def.class.opcode == Op::TypeFloat)
}

use crate::passes::stage_input::input_attachment_vector_type;

pub(in crate::passes) fn rewrite_ulong_uint2_memory_reinterprets(
    ctx: &mut Ctx,
    entry_idx: usize,
    defs: &HashMap<Word, Instruction>,
) {
    let types = combined_type_defs(ctx, defs);
    let mut value_types = combined_value_types(ctx, entry_idx);
    let uint = ctx.ty_uint();
    let ulong = ctx.ty_ulong();
    let v2uint = ctx.ty_vec_uint(2);
    let shift = ctx.const_uint(32);

    let block_count = ctx.module.functions[entry_idx].blocks.len();
    for block_idx in 0..block_count {
        let instructions = ctx.module.functions[entry_idx].blocks[block_idx]
            .instructions
            .clone();
        let mut rewritten = Vec::with_capacity(instructions.len());
        for mut inst in instructions {
            match inst.class.opcode {
                Op::Load if inst.result_type == Some(v2uint) => {
                    let Some(result) = inst.result_id else {
                        rewritten.push(inst);
                        continue;
                    };
                    let Some(Operand::IdRef(ptr)) = inst.operands.first() else {
                        rewritten.push(inst);
                        continue;
                    };
                    if pointer_value_pointee(&types, &value_types, *ptr) != Some(ulong) {
                        rewritten.push(inst);
                        continue;
                    }
                    let loaded = ctx.module.fresh_id();
                    inst.result_type = Some(ulong);
                    inst.result_id = Some(loaded);
                    rewritten.push(inst);
                    let low = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(uint),
                        Some(low),
                        vec![Operand::IdRef(loaded)],
                    ));
                    let shifted = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::ShiftRightLogical,
                        Some(ulong),
                        Some(shifted),
                        vec![Operand::IdRef(loaded), Operand::IdRef(shift)],
                    ));
                    let high = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(uint),
                        Some(high),
                        vec![Operand::IdRef(shifted)],
                    ));
                    rewritten.push(Instruction::new(
                        Op::CompositeConstruct,
                        Some(v2uint),
                        Some(result),
                        vec![Operand::IdRef(low), Operand::IdRef(high)],
                    ));
                    value_types.insert(loaded, ulong);
                    value_types.insert(low, uint);
                    value_types.insert(shifted, ulong);
                    value_types.insert(high, uint);
                    value_types.insert(result, v2uint);
                }
                Op::Store => {
                    let (Some(Operand::IdRef(ptr)), Some(Operand::IdRef(object))) =
                        (inst.operands.first(), inst.operands.get(1))
                    else {
                        rewritten.push(inst);
                        continue;
                    };
                    if pointer_value_pointee(&types, &value_types, *ptr) != Some(ulong)
                        || value_types.get(object).copied() != Some(v2uint)
                    {
                        rewritten.push(inst);
                        continue;
                    }
                    let low = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(uint),
                        Some(low),
                        vec![Operand::IdRef(*object), Operand::LiteralBit32(0)],
                    ));
                    let high = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::CompositeExtract,
                        Some(uint),
                        Some(high),
                        vec![Operand::IdRef(*object), Operand::LiteralBit32(1)],
                    ));
                    let low64 = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(ulong),
                        Some(low64),
                        vec![Operand::IdRef(low)],
                    ));
                    let high64 = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::UConvert,
                        Some(ulong),
                        Some(high64),
                        vec![Operand::IdRef(high)],
                    ));
                    let shifted = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::ShiftLeftLogical,
                        Some(ulong),
                        Some(shifted),
                        vec![Operand::IdRef(high64), Operand::IdRef(shift)],
                    ));
                    let joined = ctx.module.fresh_id();
                    rewritten.push(Instruction::new(
                        Op::BitwiseOr,
                        Some(ulong),
                        Some(joined),
                        vec![Operand::IdRef(low64), Operand::IdRef(shifted)],
                    ));
                    inst.operands[1] = Operand::IdRef(joined);
                    rewritten.push(inst);
                    value_types.insert(low, uint);
                    value_types.insert(high, uint);
                    value_types.insert(low64, ulong);
                    value_types.insert(high64, ulong);
                    value_types.insert(shifted, ulong);
                    value_types.insert(joined, ulong);
                }
                _ => rewritten.push(inst),
            }
        }
        ctx.module.functions[entry_idx].blocks[block_idx].instructions = rewritten;
    }
}

pub(in crate::passes) fn pointer_value_pointee(
    types: &HashMap<Word, Instruction>,
    value_types: &HashMap<Word, Word>,
    pointer: Word,
) -> Option<Word> {
    value_types
        .get(&pointer)
        .and_then(|ptr_ty| ptr_pointee(types, *ptr_ty))
}

pub(in crate::passes) fn collapse_resource_wrappers(
    ctx: &mut Ctx,
    entry_idx: usize,
    resource_values: &HashSet<Word>,
) -> Vec<Word> {
    if resource_values.is_empty() {
        return vec![];
    }

    let func = &ctx.module.functions[entry_idx];
    let mut paths: HashMap<Word, HashMap<Vec<u32>, Word>> = HashMap::default();
    let mut inserts: HashMap<Word, (Word, Vec<u32>)> = HashMap::default();
    let mut extract_bases: Vec<(Word, Word)> = vec![];
    let mut replacements: Vec<(Word, Word)> = vec![];
    let mut direct_resource_inserts: Vec<(Word, Word, Word)> = vec![];

    for fact in &ctx.emit_sidecar.aggregate_pointer_values {
        if resource_values.contains(&fact.source) {
            paths
                .entry(fact.aggregate)
                .or_default()
                .insert(fact.indices.clone(), fact.source);
        }
    }

    for blk in &func.blocks {
        for inst in &blk.instructions {
            let Some(result) = inst.result_id else {
                continue;
            };
            match inst.class.opcode {
                Op::CompositeInsert => {
                    let Some(Operand::IdRef(object)) = inst.operands.first() else {
                        continue;
                    };
                    let Some(Operand::IdRef(base)) = inst.operands.get(1) else {
                        continue;
                    };
                    let path = literal_path(&inst.operands[2..]);
                    inserts.insert(result, (*base, path.clone()));
                    if resource_values.contains(object) {
                        direct_resource_inserts.push((result, *base, *object));
                    }
                    let mut result_paths = paths.get(base).cloned().unwrap_or_default();
                    insert_resource_path(
                        &mut result_paths,
                        &path,
                        *object,
                        &paths,
                        resource_values,
                    );
                    if !result_paths.is_empty() {
                        paths.insert(result, result_paths);
                    }
                }
                Op::CompositeConstruct => {
                    let mut result_paths = HashMap::default();
                    for (idx, operand) in inst.operands.iter().enumerate() {
                        let Operand::IdRef(object) = operand else {
                            continue;
                        };
                        insert_resource_path(
                            &mut result_paths,
                            &[idx as u32],
                            *object,
                            &paths,
                            resource_values,
                        );
                    }
                    if !result_paths.is_empty() {
                        paths.insert(result, result_paths);
                    }
                }
                Op::CopyObject => {
                    let Some(Operand::IdRef(source)) = inst.operands.first() else {
                        continue;
                    };
                    if let Some(source_paths) = paths.get(source).cloned() {
                        paths.insert(result, source_paths);
                    } else if resource_values.contains(source) {
                        let mut result_paths = HashMap::default();
                        result_paths.insert(vec![], *source);
                        paths.insert(result, result_paths);
                    }
                }
                Op::CompositeExtract => {
                    let Some(Operand::IdRef(composite)) = inst.operands.first() else {
                        continue;
                    };
                    let path = literal_path(&inst.operands[1..]);
                    let original_composite = *composite;
                    let mut composite = original_composite;
                    while paths.contains_key(&composite) {
                        let Some((base, inserted_path)) = inserts.get(&composite) else {
                            break;
                        };
                        if !composite_paths_are_disjoint(inserted_path, &path) {
                            break;
                        }
                        composite = *base;
                    }
                    if composite != original_composite {
                        extract_bases.push((result, composite));
                    }
                    let Some(composite_paths) = paths.get(&composite) else {
                        continue;
                    };
                    if let Some(resource) = composite_paths.get(&path).copied() {
                        replacements.push((result, resource));
                        continue;
                    }
                    let mut result_paths = HashMap::default();
                    for (resource_path, resource) in composite_paths {
                        if let Some(suffix) = path_suffix(resource_path, &path) {
                            result_paths.insert(suffix.to_vec(), *resource);
                        }
                    }
                    if !result_paths.is_empty() {
                        paths.insert(result, result_paths);
                    }
                }
                _ => {}
            }
        }
    }

    if !extract_bases.is_empty() {
        let extract_bases = extract_bases.into_iter().collect::<HashMap<_, _>>();
        for inst in ctx.module.functions[entry_idx]
            .blocks
            .iter_mut()
            .flat_map(|block| block.instructions.iter_mut())
        {
            if inst.class.opcode != Op::CompositeExtract {
                continue;
            }
            let Some(base) = inst
                .result_id
                .and_then(|result| extract_bases.get(&result))
                .copied()
            else {
                continue;
            };
            inst.operands[0] = Operand::IdRef(base);
        }
    }

    let replacement_roots = replacements.iter().map(|(_, to)| *to).collect::<Vec<_>>();
    if !replacements.is_empty() {
        let replacement_map = replacements.iter().copied().collect();
        ctx.emit_sidecar.remap_ids(&replacement_map);
        {
            let func = &mut ctx.module.functions[entry_idx];
            for (from, to) in replacements {
                replace_id_in_function(func, from, to);
            }
        }
    }
    let wrapper_bypasses = direct_resource_inserts
        .into_iter()
        .map(|(result, base, _)| (result, base))
        .collect::<Vec<_>>();
    if !wrapper_bypasses.is_empty() {
        let replacement_map = wrapper_bypasses.iter().copied().collect();
        ctx.emit_sidecar.remap_ids(&replacement_map);
        let func = &mut ctx.module.functions[entry_idx];
        for (from, to) in wrapper_bypasses {
            replace_id_in_function(func, from, to);
        }
    }
    remove_dead_resource_wrapper_ops(ctx, entry_idx, resource_values);
    replacement_roots
}

pub(in crate::passes) fn collapse_late_pointer_and_opaque_wrappers(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    let type_defs = combined_type_defs(ctx, &HashMap::default());
    let resource_values = ctx
        .new_globals
        .iter()
        .chain(ctx.module.types_global_values.iter())
        .chain(
            ctx.module.functions[entry_idx]
                .blocks
                .iter()
                .flat_map(|block| block.instructions.iter()),
        )
        .filter_map(|inst| {
            let result = inst.result_id?;
            let ty = type_defs.get(&inst.result_type?)?;
            matches!(
                ty.class.opcode,
                Op::TypePointer | Op::TypeImage | Op::TypeSampler | Op::TypeSampledImage
            )
            .then_some(result)
        })
        .collect::<HashSet<_>>();
    let replacement_roots = collapse_resource_wrappers(ctx, entry_idx, &resource_values);

    let value_types = combined_value_types(ctx, entry_idx);
    let mut roots_by_storage: Vec<(StorageClass, Vec<Word>)> = vec![];
    let mut pointer_roots = replacement_roots;
    for inst in ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .filter(|inst| {
            matches!(
                inst.class.opcode,
                Op::AccessChain | Op::InBoundsAccessChain | Op::PtrAccessChain
            )
        })
    {
        let Some(base) = inst.operands.first().and_then(|operand| match operand {
            Operand::IdRef(id) => Some(*id),
            _ => None,
        }) else {
            continue;
        };
        let Some(base_pointer_type) = value_types.get(&base).copied() else {
            continue;
        };
        let Some(base_storage) = ptr_storage_from_type(&type_defs, base_pointer_type) else {
            continue;
        };
        let Some(result_storage) = inst
            .result_type
            .and_then(|pointer_type| ptr_storage_from_type(&type_defs, pointer_type))
        else {
            continue;
        };
        if base_storage != result_storage {
            pointer_roots.push(base);
        }
    }
    for root in pointer_roots {
        let Some(pointer_type) = value_types.get(&root).copied() else {
            continue;
        };
        let Some(pointer_def) = type_def_of(ctx, pointer_type) else {
            continue;
        };
        if pointer_def.class.opcode != Op::TypePointer {
            continue;
        }
        let Some(Operand::StorageClass(storage)) = pointer_def.operands.first() else {
            continue;
        };
        if let Some((_, roots)) = roots_by_storage
            .iter_mut()
            .find(|(candidate, _)| candidate == storage)
        {
            roots.push(root);
        } else {
            roots_by_storage.push((*storage, vec![root]));
        }
    }
    let defs = ctx
        .module
        .types_global_values
        .iter()
        .filter_map(|inst| inst.result_id.map(|id| (id, inst.clone())))
        .collect();
    for (storage, mut roots) in roots_by_storage {
        roots.sort_unstable();
        roots.dedup();
        rewrite_pointer_storage(ctx, entry_idx, &roots, storage, &defs)?;
    }
    Ok(())
}

fn ptr_storage_from_type(
    type_defs: &HashMap<Word, Instruction>,
    pointer_type: Word,
) -> Option<StorageClass> {
    let pointer_def = type_defs.get(&pointer_type)?;
    if pointer_def.class.opcode != Op::TypePointer {
        return None;
    }
    pointer_def
        .operands
        .first()
        .and_then(|operand| match operand {
            Operand::StorageClass(storage) => Some(*storage),
            _ => None,
        })
}

fn composite_paths_are_disjoint(left: &[u32], right: &[u32]) -> bool {
    !left.starts_with(right) && !right.starts_with(left)
}

pub(in crate::passes) fn insert_resource_path(
    out: &mut HashMap<Vec<u32>, Word>,
    prefix: &[u32],
    value: Word,
    paths: &HashMap<Word, HashMap<Vec<u32>, Word>>,
    resource_values: &HashSet<Word>,
) {
    if resource_values.contains(&value) {
        out.insert(prefix.to_vec(), value);
    }
    if let Some(value_paths) = paths.get(&value) {
        for (suffix, resource) in value_paths {
            let mut full = prefix.to_vec();
            full.extend(suffix);
            out.insert(full, *resource);
        }
    }
}

pub(in crate::passes) fn literal_path(operands: &[Operand]) -> Vec<u32> {
    operands
        .iter()
        .filter_map(|operand| match operand {
            Operand::LiteralBit32(value) => Some(*value),
            _ => None,
        })
        .collect()
}

pub(in crate::passes) fn path_suffix<'a>(path: &'a [u32], prefix: &[u32]) -> Option<&'a [u32]> {
    if path.len() < prefix.len() || &path[..prefix.len()] != prefix {
        return None;
    }
    Some(&path[prefix.len()..])
}

pub(in crate::passes) fn remove_dead_resource_wrapper_ops(
    ctx: &mut Ctx,
    entry_idx: usize,
    resource_values: &HashSet<Word>,
) {
    let value_types = function_value_types(ctx, entry_idx);
    let type_defs = combined_type_defs(ctx, &HashMap::default());
    let resource_projection_roots = resource_values
        .iter()
        .copied()
        .filter(|value| {
            value_types
                .get(value)
                .copied()
                .is_some_and(|ty| opaque_resource_type(&type_defs, ty, &mut HashSet::default()))
        })
        .collect::<HashSet<_>>();
    loop {
        let used = function_used_ids(&ctx.module.functions[entry_idx]);
        let mut resource_projections = resource_projection_roots.clone();
        loop {
            let additions = ctx.module.functions[entry_idx]
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        instruction.class.opcode,
                        Op::AccessChain
                            | Op::InBoundsAccessChain
                            | Op::PtrAccessChain
                            | Op::InBoundsPtrAccessChain
                            | Op::Bitcast
                            | Op::CopyObject
                    )
                })
                .filter(|instruction| {
                    instruction.operands.first().is_some_and(
                        |operand| matches!(operand, Operand::IdRef(id) if resource_projections.contains(id)),
                    )
                })
                .filter_map(|instruction| instruction.result_id)
                .filter(|result| !resource_projections.contains(result))
                .collect::<Vec<_>>();
            if additions.is_empty() {
                break;
            }
            resource_projections.extend(additions);
        }
        let mut changed = false;
        for blk in &mut ctx.module.functions[entry_idx].blocks {
            let before = blk.instructions.len();
            blk.instructions.retain(|inst| {
                let dead = inst
                    .result_id
                    .map(|id| !used.contains(&id))
                    .unwrap_or(false);
                let replaced_wrapper = matches!(
                    inst.class.opcode,
                    Op::CompositeInsert
                        | Op::CompositeExtract
                        | Op::CompositeConstruct
                        | Op::CopyObject
                        | Op::Undef
                );
                let replaced_pointer_projection = inst.result_id.is_some_and(|result| {
                    resource_projections.contains(&result)
                        && matches!(
                            inst.class.opcode,
                            Op::AccessChain
                                | Op::InBoundsAccessChain
                                | Op::PtrAccessChain
                                | Op::InBoundsPtrAccessChain
                                | Op::Bitcast
                        )
                });
                !(dead && (replaced_wrapper || replaced_pointer_projection))
            });
            changed |= blk.instructions.len() != before;
        }
        if !changed {
            break;
        }
    }
}

fn opaque_resource_type(
    type_defs: &HashMap<Word, Instruction>,
    ty: Word,
    seen: &mut HashSet<Word>,
) -> bool {
    if !seen.insert(ty) {
        return false;
    }
    let Some(definition) = type_defs.get(&ty) else {
        return false;
    };
    if matches!(
        definition.class.opcode,
        Op::TypeImage | Op::TypeSampler | Op::TypeSampledImage
    ) {
        return true;
    }
    if !matches!(
        definition.class.opcode,
        Op::TypePointer | Op::TypeArray | Op::TypeRuntimeArray
    ) {
        return false;
    }
    definition.operands.iter().any(|operand| {
        matches!(operand, Operand::IdRef(child) if opaque_resource_type(type_defs, *child, seen))
    })
}

pub(in crate::passes) fn function_used_ids(func: &Function) -> HashSet<Word> {
    let mut used = HashSet::default();
    for param in &func.parameters {
        for operand in &param.operands {
            collect_operand_id_refs(operand, &mut used);
        }
    }
    for blk in &func.blocks {
        if let Some(label) = &blk.label {
            for operand in &label.operands {
                collect_operand_id_refs(operand, &mut used);
            }
        }
        for inst in &blk.instructions {
            for operand in &inst.operands {
                collect_operand_id_refs(operand, &mut used);
            }
        }
    }
    used
}

pub(in crate::passes) fn collect_operand_id_refs(operand: &Operand, used: &mut HashSet<Word>) {
    if let Operand::IdRef(id) = operand {
        used.insert(*id);
    }
}

fn param_is_float(ctx: &Ctx, ty: Word) -> bool {
    let Some(def) = crate::passes::value_queries::type_def_of(ctx, ty) else {
        return false;
    };
    let scalar_ty = if def.class.opcode == Op::TypeVector {
        match def.operands.first() {
            Some(Operand::IdRef(component)) => *component,
            _ => return false,
        }
    } else {
        ty
    };
    crate::passes::value_queries::type_def_of(ctx, scalar_ty)
        .is_some_and(|definition| definition.class.opcode == Op::TypeFloat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;

    #[test]
    fn resource_wrapper_collapse_remaps_dynamic_field_fact_root() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        let aggregate = 10;
        let wrapped = 11;
        let extracted = 12;
        let resource = 20;
        let handle = 30;
        let index = 31;
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CompositeInsert,
                        Some(1),
                        Some(wrapped),
                        vec![
                            Operand::IdRef(resource),
                            Operand::IdRef(aggregate),
                            Operand::LiteralBit32(0),
                        ],
                    ),
                    Instruction::new(
                        Op::CompositeExtract,
                        Some(2),
                        Some(extracted),
                        vec![Operand::IdRef(wrapped), Operand::LiteralBit32(0)],
                    ),
                ],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);
        ctx.emit_sidecar.local_pointer_dynamic_field_loads.push(
            crate::emit_sidecar::LocalPointerDynamicFieldLoad {
                id: handle,
                root: extracted,
                prefix: vec![],
                index,
                suffix: vec![0],
            },
        );

        let _ = collapse_resource_wrappers(&mut ctx, 0, &crate::fxhash::set_from([resource]));

        assert_eq!(
            ctx.emit_sidecar.local_pointer_dynamic_field_loads[0].root,
            resource
        );
        assert!(ctx.module.functions[0].blocks[0].instructions.is_empty());
    }

    #[test]
    fn binding_does_not_retain_dead_pointer_projections_of_image_values() {
        let float_ty = 1;
        let image_ty = 2;
        let image_pointer_ty = 3;
        let image_variable = 10;
        let image = 20;
        let dead_projection = 21;
        let unrelated_projection = 22;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(float_ty),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeImage,
                None,
                Some(image_ty),
                vec![
                    Operand::IdRef(float_ty),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(image_pointer_ty),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(image_ty),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(image_pointer_ty),
                Some(image_variable),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ),
        ]);
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::Load,
                        Some(image_ty),
                        Some(image),
                        vec![Operand::IdRef(image_variable)],
                    ),
                    Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(3),
                        Some(dead_projection),
                        vec![Operand::IdRef(image), Operand::IdRef(30)],
                    ),
                    Instruction::new(
                        Op::InBoundsAccessChain,
                        Some(3),
                        Some(unrelated_projection),
                        vec![Operand::IdRef(31), Operand::IdRef(30)],
                    ),
                ],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);

        remove_dead_resource_wrapper_ops(&mut ctx, 0, &crate::fxhash::set_from([image]));

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        assert!(instructions
            .iter()
            .all(|instruction| instruction.result_id != Some(dead_projection)));
        assert!(instructions
            .iter()
            .any(|instruction| instruction.result_id == Some(unrelated_projection)));
    }

    #[test]
    fn binding_does_not_retain_dead_pointer_projections_of_image_arrays() {
        let float_ty = 1;
        let uint_ty = 2;
        let image_ty = 3;
        let array_length = 4;
        let image_array_ty = 5;
        let image_array_pointer_ty = 6;
        let stale_pointer_ty = 7;
        let image_array_variable = 10;
        let stale_projection = 20;
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(float_ty),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint_ty),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypeImage,
                None,
                Some(image_ty),
                vec![
                    Operand::IdRef(float_ty),
                    Operand::Dim(spirv::Dim::Dim2D),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(0),
                    Operand::LiteralBit32(1),
                    Operand::ImageFormat(spirv::ImageFormat::Unknown),
                ],
            ),
            Instruction::new(
                Op::Constant,
                Some(uint_ty),
                Some(array_length),
                vec![Operand::LiteralBit32(2)],
            ),
            Instruction::new(
                Op::TypeArray,
                None,
                Some(image_array_ty),
                vec![Operand::IdRef(image_ty), Operand::IdRef(array_length)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(image_array_pointer_ty),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(image_array_ty),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(stale_pointer_ty),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(uint_ty),
                ],
            ),
            Instruction::new(
                Op::Variable,
                Some(image_array_pointer_ty),
                Some(image_array_variable),
                vec![Operand::StorageClass(StorageClass::UniformConstant)],
            ),
        ]);
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![Instruction::new(
                    Op::InBoundsAccessChain,
                    Some(stale_pointer_ty),
                    Some(stale_projection),
                    vec![
                        Operand::IdRef(image_array_variable),
                        Operand::IdRef(array_length),
                        Operand::IdRef(array_length),
                    ],
                )],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);

        remove_dead_resource_wrapper_ops(
            &mut ctx,
            0,
            &crate::fxhash::set_from([image_array_variable]),
        );

        assert!(ctx.module.functions[0].blocks[0]
            .instructions
            .iter()
            .all(|instruction| instruction.result_id != Some(stale_projection)));
    }

    #[test]
    fn resource_insert_is_removed_when_only_a_disjoint_field_is_extracted() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        let aggregate = 10;
        let resource = 20;
        let resource_copy = 21;
        let wrapped = 22;
        let extracted = 23;
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CopyObject,
                        Some(2),
                        Some(resource_copy),
                        vec![Operand::IdRef(resource)],
                    ),
                    Instruction::new(
                        Op::CompositeInsert,
                        Some(1),
                        Some(wrapped),
                        vec![
                            Operand::IdRef(resource_copy),
                            Operand::IdRef(aggregate),
                            Operand::LiteralBit32(3),
                            Operand::LiteralBit32(0),
                        ],
                    ),
                    Instruction::new(
                        Op::CompositeExtract,
                        Some(3),
                        Some(extracted),
                        vec![
                            Operand::IdRef(wrapped),
                            Operand::LiteralBit32(0),
                            Operand::LiteralBit32(0),
                        ],
                    ),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(50), Operand::IdRef(extracted)],
                    ),
                ],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);

        let _ = collapse_resource_wrappers(&mut ctx, 0, &crate::fxhash::set_from([resource]));

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        assert!(!instructions.iter().any(|inst| {
            matches!(inst.result_id, Some(id) if id == resource_copy || id == wrapped)
        }));
        let extract = instructions
            .iter()
            .find(|inst| inst.result_id == Some(extracted))
            .unwrap();
        assert_eq!(extract.operands.first(), Some(&Operand::IdRef(aggregate)));
    }

    #[test]
    fn late_pointer_wrapper_forwards_the_concrete_inserted_pointer() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(2),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::UniformConstant),
                    Operand::IdRef(1),
                ],
            ),
        ]);
        let aggregate = 10;
        let concrete_pointer = 20;
        let wrapped = 21;
        let extracted = 22;
        let derived = 23;
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![
                    Instruction::new(
                        Op::CompositeInsert,
                        Some(4),
                        Some(wrapped),
                        vec![
                            Operand::IdRef(concrete_pointer),
                            Operand::IdRef(aggregate),
                            Operand::LiteralBit32(0),
                        ],
                    ),
                    Instruction::new(
                        Op::CompositeExtract,
                        Some(3),
                        Some(extracted),
                        vec![Operand::IdRef(wrapped), Operand::LiteralBit32(0)],
                    ),
                    Instruction::new(
                        Op::AccessChain,
                        Some(3),
                        Some(derived),
                        vec![Operand::IdRef(extracted)],
                    ),
                    Instruction::new(
                        Op::Store,
                        None,
                        None,
                        vec![Operand::IdRef(derived), Operand::IdRef(50)],
                    ),
                ],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(2),
            Some(concrete_pointer),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ));

        collapse_late_pointer_and_opaque_wrappers(&mut ctx, 0).unwrap();

        let instructions = &ctx.module.functions[0].blocks[0].instructions;
        assert!(!instructions
            .iter()
            .any(|inst| inst.result_id == Some(wrapped)));
        let store = instructions
            .iter()
            .find(|inst| inst.class.opcode == Op::Store)
            .unwrap();
        assert_eq!(store.operands.first(), Some(&Operand::IdRef(derived)));
        let access = instructions
            .iter()
            .find(|inst| inst.result_id == Some(derived))
            .unwrap();
        assert_eq!(
            access.operands.first(),
            Some(&Operand::IdRef(concrete_pointer))
        );
        assert_eq!(access.result_type, Some(2));
    }

    #[test]
    fn late_pointer_collapse_repairs_direct_base_storage_mismatch() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.types_global_values.extend([
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(2),
                vec![
                    Operand::StorageClass(StorageClass::StorageBuffer),
                    Operand::IdRef(1),
                ],
            ),
            Instruction::new(
                Op::TypePointer,
                None,
                Some(3),
                vec![
                    Operand::StorageClass(StorageClass::Private),
                    Operand::IdRef(1),
                ],
            ),
        ]);
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(40), vec![])),
                instructions: vec![Instruction::new(
                    Op::InBoundsAccessChain,
                    Some(3),
                    Some(21),
                    vec![Operand::IdRef(20)],
                )],
            }],
            end: None,
        });
        let mut ctx = Ctx::new(module);
        ctx.new_globals.push(Instruction::new(
            Op::Variable,
            Some(2),
            Some(20),
            vec![Operand::StorageClass(StorageClass::StorageBuffer)],
        ));

        collapse_late_pointer_and_opaque_wrappers(&mut ctx, 0).unwrap();

        assert_eq!(
            ctx.module.functions[0].blocks[0].instructions[0].result_type,
            Some(2)
        );
    }
}
