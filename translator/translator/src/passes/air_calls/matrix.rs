use super::*;
use crate::air_intrinsics::{
    matrix16_intrinsic, Matrix16Element, Matrix16Intrinsic, Matrix8Intrinsic,
};

pub(in crate::passes) fn is_half_scalar(ctx: &Ctx, ty: Word) -> bool {
    type_def_of(ctx, ty)
        .map(|d| {
            d.class.opcode == Op::TypeFloat
                && d.operands.first() == Some(&Operand::LiteralBit32(16))
        })
        .unwrap_or(false)
}

pub(in crate::passes) fn is_half_scalar_or_vector(ctx: &Ctx, ty: Word) -> bool {
    is_half_scalar(ctx, ty) || is_half_vector(ctx, ty)
}

pub(in crate::passes) fn is_bool_type(ctx: &Ctx, ty: Word) -> bool {
    type_def_of(ctx, ty)
        .map(|def| def.class.opcode == Op::TypeBool)
        .unwrap_or(false)
}

struct Matrix8Lane {
    simd_lane: Word,
    simd_base: Word,
    row: Word,
    col0: Word,
}

fn matrix8_lane(ctx: &mut Ctx, out: &mut Vec<Instruction>) -> Matrix8Lane {
    let uint = ctx.ty_uint();
    let subgroup_lane = subgroup_lane_index_u32(ctx, out);
    let simd_lane = metal_simd_lane_local_u32(ctx, subgroup_lane, out);
    let simd_base = metal_simd_lane_base_u32(ctx, subgroup_lane, simd_lane, out);
    let row_lo = shift_with_const(ctx, out, Op::ShiftRightLogical, simd_lane, 1);
    let row_lo = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, row_lo, 3);
    let row_hi = shift_with_const(ctx, out, Op::ShiftRightLogical, simd_lane, 2);
    let row_hi = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, row_hi, 4);
    let row = binary_value(ctx, out, Op::BitwiseOr, uint, row_lo, row_hi);
    let col_lo = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, simd_lane, 1);
    let col_lo = shift_with_const(ctx, out, Op::ShiftLeftLogical, col_lo, 1);
    let col_hi = shift_with_const(ctx, out, Op::ShiftRightLogical, simd_lane, 1);
    let col_hi = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, col_hi, 4);
    let col0 = binary_value(ctx, out, Op::BitwiseOr, uint, col_lo, col_hi);
    Matrix8Lane {
        simd_lane,
        simd_base,
        row,
        col0,
    }
}

fn matrix8_slot_col(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    lane: &Matrix8Lane,
    slot: u32,
) -> Word {
    if slot == 0 {
        return lane.col0;
    }
    let uint = ctx.ty_uint();
    bitwise_with_const(ctx, out, Op::BitwiseOr, uint, lane.col0, 1)
}

fn matrix8_pack(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    elem: Word,
    rty: Word,
    res: Word,
    slots: [Word; 2],
) {
    let undef = ctx.module.fresh_id();
    out.push(Instruction::new(Op::Undef, Some(elem), Some(undef), vec![]));
    let mut lanes = Vec::with_capacity(64);
    lanes.push(Operand::IdRef(slots[0]));
    lanes.push(Operand::IdRef(slots[1]));
    lanes.extend(std::iter::repeat_n(Operand::IdRef(undef), 62));
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(rty),
        Some(res),
        lanes,
    ));
}

pub(in crate::passes) fn lower_simdgroup_matrix_8x8_mac(
    ctx: &mut Ctx,
    name: &str,
    signature: Matrix8Intrinsic,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 3 {
        return Err(format!(
            "air.simdgroup_matrix_8x8_multiply_accumulate expects 3 operands, got {}",
            args.len()
        ));
    }
    let (result_elem, lanes) = composite_shape(ctx, rty)
        .ok_or_else(|| "air.simdgroup_matrix result is not a 64-lane composite".to_string())?;
    if lanes != 64 || !matrix8_storage_matches(ctx, result_elem, signature.result) {
        return Err("air.simdgroup_matrix result is not v64f16 or v64f32".to_string());
    }
    let kinds = [signature.lhs, signature.rhs, signature.accumulator];
    let mut operand_elems = Vec::with_capacity(3);
    for (ordinal, (arg, kind)) in args.iter().zip(kinds).enumerate() {
        let ty = value_result_type(ctx, *arg)
            .ok_or_else(|| format!("air.simdgroup_matrix operand {ordinal} has no type"))?;
        let (elem, arg_lanes) = composite_shape(ctx, ty).ok_or_else(|| {
            format!("air.simdgroup_matrix operand {ordinal} is not a 64-lane composite")
        })?;
        if arg_lanes != 64 || !matrix8_storage_matches(ctx, elem, kind) {
            return Err(format!(
                "{name} operand {ordinal} does not match its ABI element type"
            ));
        }
        operand_elems.push(elem);
    }
    if mat8_mma_eligible(signature) {
        match ctx.mat8_fuse.get(&res).copied() {
            Some(Mat8FuseRole::Skip) => return Ok(Vec::new()),
            Some(Mat8FuseRole::Lead(group)) => {
                return Ok(lower_mat8_fused(
                    ctx,
                    signature,
                    rty,
                    group,
                    operand_elems[2],
                    result_elem,
                ));
            }
            None => {}
        }
        return Ok(lower_simdgroup_matrix_8x8_mac_mma(
            ctx,
            signature,
            res,
            rty,
            args,
            operand_elems[2],
            result_elem,
        ));
    }
    let arithmetic_elem = if kinds
        .iter()
        .chain(std::iter::once(&signature.result))
        .any(|kind| *kind != Matrix16Element::F16)
    {
        ctx.ty_float()
    } else {
        result_elem
    };

    let mut insts = Vec::with_capacity(160);
    let uint = ctx.ty_uint();
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let lane = matrix8_lane(ctx, &mut insts);
    let a_key = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, lane.simd_lane, 0x16);
    let b_key = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, lane.simd_lane, 0x09);

    let mut own = [[0 as Word; 2]; 3];
    for (ordinal, kind) in kinds.iter().enumerate() {
        for slot in 0..2u32 {
            let raw =
                composite_extract(ctx, &mut insts, operand_elems[ordinal], args[ordinal], slot);
            own[ordinal][slot as usize] =
                matrix16_to_accumulator(ctx, &mut insts, raw, *kind, arithmetic_elem);
        }
    }
    let mut accumulators = own[2];
    let ext = ctx.glsl();
    for k in 0..8u32 {
        let a_varying_bits = ((k & 4) << 1) | ((k >> 1) & 1);
        let b_varying_bits = ((k & 3) << 1) | ((k & 4) << 2);
        let a_owner_local =
            bitwise_with_const(ctx, &mut insts, Op::BitwiseOr, uint, a_key, a_varying_bits);
        let b_owner_local =
            bitwise_with_const(ctx, &mut insts, Op::BitwiseOr, uint, b_key, b_varying_bits);
        let a_owner = binary_value(
            ctx,
            &mut insts,
            Op::IAdd,
            uint,
            lane.simd_base,
            a_owner_local,
        );
        let b_owner = binary_value(
            ctx,
            &mut insts,
            Op::IAdd,
            uint,
            lane.simd_base,
            b_owner_local,
        );
        let a = subgroup_shuffle(
            ctx,
            &mut insts,
            arithmetic_elem,
            scope,
            own[0][(k & 1) as usize],
            a_owner,
        );
        for slot in 0..2 {
            let b = subgroup_shuffle(
                ctx,
                &mut insts,
                arithmetic_elem,
                scope,
                own[1][slot],
                b_owner,
            );
            let next = ctx.module.fresh_id();
            insts.push(Instruction::new(
                Op::ExtInst,
                Some(arithmetic_elem),
                Some(next),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(GLSLstd450::Fma as u32),
                    Operand::IdRef(a),
                    Operand::IdRef(b),
                    Operand::IdRef(accumulators[slot]),
                ],
            ));
            accumulators[slot] = next;
        }
    }
    let mut slots = [0 as Word; 2];
    for slot in 0..2 {
        slots[slot] = if signature.result == Matrix16Element::Bf16 {
            let narrowed = ctx.module.fresh_id();
            narrow_f32_to_bf16(
                ctx,
                &mut insts,
                accumulators[slot],
                1,
                result_elem,
                narrowed,
            );
            narrowed
        } else {
            convert_matrix_lane(
                ctx,
                &mut insts,
                accumulators[slot],
                arithmetic_elem,
                result_elem,
            )
        };
    }
    matrix8_pack(ctx, &mut insts, result_elem, rty, res, slots);
    Ok(insts)
}

fn coopmat_type(ctx: &mut Ctx, component: Word, rows: u32, cols: u32, use_: u32) -> Word {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let rows = ctx.const_uint(rows);
    let cols = ctx.const_uint(cols);
    let use_ = ctx.const_uint(use_);
    ctx.get_or_create(
        Op::TypeCooperativeMatrixKHR,
        None,
        vec![
            Operand::IdRef(component),
            Operand::IdScope(scope),
            Operand::IdRef(rows),
            Operand::IdRef(cols),
            Operand::IdRef(use_),
        ],
    )
}

fn coopmat_from_pair(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    ty: Word,
    zero: Word,
    e: [Word; 2],
) -> Word {
    let mut m = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(ty),
        Some(m),
        vec![Operand::IdRef(zero)],
    ));
    for (index, value) in e.iter().enumerate() {
        let next = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeInsert,
            Some(ty),
            Some(next),
            vec![
                Operand::IdRef(*value),
                Operand::IdRef(m),
                Operand::LiteralBit32(index as u32),
            ],
        ));
        m = next;
    }
    m
}

fn lower_simdgroup_matrix_8x8_mac_mma(
    ctx: &mut Ctx,
    signature: Matrix8Intrinsic,
    res: Word,
    rty: Word,
    args: &[Word],
    c_elem: Word,
    result_elem: Word,
) -> Vec<Instruction> {
    mat8_mma_module_setup(ctx);
    let mut insts = Vec::with_capacity(96);
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let half = ctx.ty_half();
    let acc_elem = if signature.accumulator == Matrix16Element::F16
        && signature.result == Matrix16Element::F16
    {
        half
    } else {
        ctx.ty_float()
    };
    let zero_half = ctx.const_half(0.0);
    let zero_acc = if acc_elem == half {
        zero_half
    } else {
        ctx.const_float(0.0)
    };
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let lane = matrix8_lane(ctx, &mut insts);

    let g = shift_with_const(ctx, &mut insts, Op::ShiftRightLogical, lane.simd_lane, 2);
    let t = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, lane.simd_lane, 3);
    let g3 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, g, 3);
    let g3 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, g3, 1);
    let g4 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, g, 4);
    let g4 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, g4, 2);
    let t1 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, t, 1);
    let t2 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, t, 2);
    let t2 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, t2, 2);
    let ac = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, g3, g4);
    let ac = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, ac, t1);
    let ac = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, ac, t2);
    let ac = binary_value(ctx, &mut insts, Op::IAdd, uint, lane.simd_base, ac);
    let bt1 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, t1, 2);
    let bt2 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, t, 2);
    let bt2 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, bt2, 3);
    let bg1 = shift_with_const(ctx, &mut insts, Op::ShiftRightLogical, g, 1);
    let bg1 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, bg1, 1);
    let bg4 = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, g, 4);
    let bg4 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, bg4, 1);
    let b0 = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, bt1, bt2);
    let b0 = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, b0, bg1);
    let b0 = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, b0, bg4);
    let b1 = bitwise_with_const(ctx, &mut insts, Op::BitwiseOr, uint, b0, 2);
    let b0 = binary_value(ctx, &mut insts, Op::IAdd, uint, lane.simd_base, b0);
    let b1 = binary_value(ctx, &mut insts, Op::IAdd, uint, lane.simd_base, b1);
    let g_odd = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, g, 1);
    let zero_u = ctx.const_uint(0);
    let g_even = binary_value(ctx, &mut insts, Op::IEqual, bool_ty, g_odd, zero_u);

    let a_e = match mat8_nv_cached(ctx, args[0], MAT8_A) {
        Some(hit) => hit.e,
        None => {
            let own = [
                composite_extract(ctx, &mut insts, half, args[0], 0),
                composite_extract(ctx, &mut insts, half, args[0], 1),
            ];
            let e = [
                subgroup_shuffle(ctx, &mut insts, half, scope, own[0], ac),
                subgroup_shuffle(ctx, &mut insts, half, scope, own[1], ac),
            ];
            mat8_nv_record(ctx, args[0], MAT8_A, e, half);
            e
        }
    };
    let b_e = match mat8_nv_cached(ctx, args[1], MAT8_B) {
        Some(hit) => hit.e,
        None => {
            let own = [
                composite_extract(ctx, &mut insts, half, args[1], 0),
                composite_extract(ctx, &mut insts, half, args[1], 1),
            ];
            let mut e = [0 as Word; 2];
            for (index, src) in [b0, b1].into_iter().enumerate() {
                let even = subgroup_shuffle(ctx, &mut insts, half, scope, own[0], src);
                let odd = subgroup_shuffle(ctx, &mut insts, half, scope, own[1], src);
                e[index] = select_value(ctx, &mut insts, half, g_even, even, odd);
            }
            mat8_nv_record(ctx, args[1], MAT8_B, e, half);
            e
        }
    };
    let c_e = match mat8_nv_cached(ctx, args[2], MAT8_C) {
        Some(hit) => [
            convert_matrix_lane(ctx, &mut insts, hit.e[0], hit.elem, acc_elem),
            convert_matrix_lane(ctx, &mut insts, hit.e[1], hit.elem, acc_elem),
        ],
        None => {
            let mut own = [
                composite_extract(ctx, &mut insts, c_elem, args[2], 0),
                composite_extract(ctx, &mut insts, c_elem, args[2], 1),
            ];
            for own_lane in &mut own {
                *own_lane = matrix16_to_accumulator(
                    ctx,
                    &mut insts,
                    *own_lane,
                    signature.accumulator,
                    acc_elem,
                );
            }
            let e = [
                subgroup_shuffle(ctx, &mut insts, acc_elem, scope, own[0], ac),
                subgroup_shuffle(ctx, &mut insts, acc_elem, scope, own[1], ac),
            ];
            mat8_nv_record(ctx, args[2], MAT8_C, e, acc_elem);
            e
        }
    };

    let a_ty = coopmat_type(ctx, half, 16, 8, 0);
    let b_ty = coopmat_type(ctx, half, 8, 8, 1);
    let c_ty = coopmat_type(ctx, acc_elem, 16, 8, 2);
    let a = coopmat_from_pair(ctx, &mut insts, a_ty, zero_half, a_e);
    let b = coopmat_from_pair(ctx, &mut insts, b_ty, zero_half, b_e);
    let c = coopmat_from_pair(ctx, &mut insts, c_ty, zero_acc, c_e);
    let d = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::CooperativeMatrixMulAddKHR,
        Some(c_ty),
        Some(d),
        vec![Operand::IdRef(a), Operand::IdRef(b), Operand::IdRef(c)],
    ));
    let d_e = [
        composite_extract(ctx, &mut insts, acc_elem, d, 0),
        composite_extract(ctx, &mut insts, acc_elem, d, 1),
    ];
    mat8_nv_record(ctx, res, MAT8_C, d_e, acc_elem);
    let back = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, lane.row, 2);
    let half_col = shift_with_const(ctx, &mut insts, Op::ShiftRightLogical, lane.col0, 1);
    let back = binary_value(ctx, &mut insts, Op::BitwiseOr, uint, back, half_col);
    let back = binary_value(ctx, &mut insts, Op::IAdd, uint, lane.simd_base, back);
    let mut slots = [0 as Word; 2];
    for slot in 0..2 {
        let v = subgroup_shuffle(ctx, &mut insts, acc_elem, scope, d_e[slot], back);
        slots[slot] = convert_matrix_lane(ctx, &mut insts, v, acc_elem, result_elem);
    }
    matrix8_pack(ctx, &mut insts, result_elem, rty, res, slots);
    insts
}

const MAT8_A: u8 = 0;
const MAT8_B: u8 = 1;
const MAT8_C: u8 = 2;

fn mat8_mma_eligible(signature: Matrix8Intrinsic) -> bool {
    signature.lhs == Matrix16Element::F16
        && signature.rhs == Matrix16Element::F16
        && matches!(
            signature.accumulator,
            Matrix16Element::F16 | Matrix16Element::F32
        )
        && matches!(
            signature.result,
            Matrix16Element::F16 | Matrix16Element::F32
        )
        && std::env::var_os("NVMTL_MATRIX8_SHUFFLE").is_none()
}

fn mat8_acc_is_half(signature: Matrix8Intrinsic) -> bool {
    signature.accumulator == Matrix16Element::F16 && signature.result == Matrix16Element::F16
}

fn mat8_nv_cached(ctx: &Ctx, value: Word, role: u8) -> Option<Mat8Nv> {
    let hit = *ctx.mat8_nv.get(&(value, role))?;
    let body = ctx.air_call_body.as_ref()?;
    body.dominance
        .dominates(hit.block, ctx.air_call_block)
        .then_some(hit)
}

fn mat8_nv_record(ctx: &mut Ctx, value: Word, role: u8, e: [Word; 2], elem: Word) {
    let block = ctx.air_call_block;
    ctx.mat8_nv.insert((value, role), Mat8Nv { e, elem, block });
}

fn mat8_ac_source(ctx: &mut Ctx, insts: &mut Vec<Instruction>, lane: &Matrix8Lane) -> Word {
    let uint = ctx.ty_uint();
    let g = shift_with_const(ctx, insts, Op::ShiftRightLogical, lane.simd_lane, 2);
    let t = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, lane.simd_lane, 3);
    let g3 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, g, 3);
    let g3 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, g3, 1);
    let g4 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, g, 4);
    let g4 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, g4, 2);
    let t1 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, t, 1);
    let t2 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, t, 2);
    let t2 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, t2, 2);
    let ac = binary_value(ctx, insts, Op::BitwiseOr, uint, g3, g4);
    let ac = binary_value(ctx, insts, Op::BitwiseOr, uint, ac, t1);
    let ac = binary_value(ctx, insts, Op::BitwiseOr, uint, ac, t2);
    binary_value(ctx, insts, Op::IAdd, uint, lane.simd_base, ac)
}

fn mat8_c_to_nv(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    value: Word,
    c_elem: Word,
    kind: Matrix16Element,
    acc_elem: Word,
) -> [Word; 2] {
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let lane = matrix8_lane(ctx, insts);
    let ac = mat8_ac_source(ctx, insts, &lane);
    let mut e = [0 as Word; 2];
    for (slot, element) in e.iter_mut().enumerate() {
        let own = composite_extract(ctx, insts, c_elem, value, slot as u32);
        let own = matrix16_to_accumulator(ctx, insts, own, kind, acc_elem);
        *element = subgroup_shuffle(ctx, insts, acc_elem, scope, own, ac);
    }
    e
}

fn mat8_nv_to_apple(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    e: [Word; 2],
    acc_elem: Word,
    result_elem: Word,
    rty: Word,
) -> Word {
    let uint = ctx.ty_uint();
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let lane = matrix8_lane(ctx, insts);
    let back = shift_with_const(ctx, insts, Op::ShiftLeftLogical, lane.row, 2);
    let half_col = shift_with_const(ctx, insts, Op::ShiftRightLogical, lane.col0, 1);
    let back = binary_value(ctx, insts, Op::BitwiseOr, uint, back, half_col);
    let back = binary_value(ctx, insts, Op::IAdd, uint, lane.simd_base, back);
    let mut slots = [0 as Word; 2];
    for slot in 0..2 {
        let v = subgroup_shuffle(ctx, insts, acc_elem, scope, e[slot], back);
        slots[slot] = convert_matrix_lane(ctx, insts, v, acc_elem, result_elem);
    }
    let res = ctx.module.fresh_id();
    matrix8_pack(ctx, insts, result_elem, rty, res, slots);
    res
}

fn mat8_mma_module_setup(ctx: &mut Ctx) {
    ctx.add_capability(spirv::Capability::CooperativeMatrixKHR);
    ctx.add_capability(spirv::Capability::VulkanMemoryModel);
    for extension in ["SPV_KHR_cooperative_matrix", "SPV_KHR_vulkan_memory_model"] {
        if !ctx
            .module
            .extensions
            .iter()
            .any(|e| e.operands.first() == Some(&Operand::LiteralString(extension.to_string())))
        {
            ctx.module.extensions.push(Instruction::new(
                Op::Extension,
                None,
                None,
                vec![Operand::LiteralString(extension.to_string())],
            ));
        }
    }
    if let Some(model) = ctx.module.memory_model.as_mut() {
        if let Some(operand) = model.operands.get_mut(1) {
            *operand = Operand::MemoryModel(spirv::MemoryModel::Vulkan);
        }
    }
}

fn coopmat_from_cells(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    ty: Word,
    zero: Word,
    cells: &[(u32, Word)],
) -> Word {
    let mut m = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(ty),
        Some(m),
        vec![Operand::IdRef(zero)],
    ));
    for (index, value) in cells {
        let next = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::CompositeInsert,
            Some(ty),
            Some(next),
            vec![
                Operand::IdRef(*value),
                Operand::IdRef(m),
                Operand::LiteralBit32(*index),
            ],
        ));
        m = next;
    }
    m
}

fn mat8_b_to_nv(ctx: &mut Ctx, insts: &mut Vec<Instruction>, value: Word) -> [Word; 2] {
    let uint = ctx.ty_uint();
    let bool_ty = ctx.ty_bool();
    let half = ctx.ty_half();
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let lane = matrix8_lane(ctx, insts);
    let g = shift_with_const(ctx, insts, Op::ShiftRightLogical, lane.simd_lane, 2);
    let t = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, lane.simd_lane, 3);
    let t1 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, t, 1);
    let bt1 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, t1, 2);
    let bt2 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, t, 2);
    let bt2 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, bt2, 3);
    let bg1 = shift_with_const(ctx, insts, Op::ShiftRightLogical, g, 1);
    let bg1 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, bg1, 1);
    let bg4 = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, g, 4);
    let bg4 = shift_with_const(ctx, insts, Op::ShiftLeftLogical, bg4, 1);
    let b0 = binary_value(ctx, insts, Op::BitwiseOr, uint, bt1, bt2);
    let b0 = binary_value(ctx, insts, Op::BitwiseOr, uint, b0, bg1);
    let b0 = binary_value(ctx, insts, Op::BitwiseOr, uint, b0, bg4);
    let b1 = bitwise_with_const(ctx, insts, Op::BitwiseOr, uint, b0, 2);
    let b0 = binary_value(ctx, insts, Op::IAdd, uint, lane.simd_base, b0);
    let b1 = binary_value(ctx, insts, Op::IAdd, uint, lane.simd_base, b1);
    let g_odd = bitwise_with_const(ctx, insts, Op::BitwiseAnd, uint, g, 1);
    let zero_u = ctx.const_uint(0);
    let g_even = binary_value(ctx, insts, Op::IEqual, bool_ty, g_odd, zero_u);
    let own = [
        composite_extract(ctx, insts, half, value, 0),
        composite_extract(ctx, insts, half, value, 1),
    ];
    let mut e = [0 as Word; 2];
    for (index, src) in [b0, b1].into_iter().enumerate() {
        let even = subgroup_shuffle(ctx, insts, half, scope, own[0], src);
        let odd = subgroup_shuffle(ctx, insts, half, scope, own[1], src);
        e[index] = select_value(ctx, insts, half, g_even, even, odd);
    }
    e
}

fn mat8_nv_operand(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    value: Word,
    role: u8,
    elem: Word,
    kind: Matrix16Element,
    want: Word,
) -> [Word; 2] {
    if let Some(hit) = mat8_nv_cached(ctx, value, role) {
        return [
            convert_matrix_lane(ctx, insts, hit.e[0], hit.elem, want),
            convert_matrix_lane(ctx, insts, hit.e[1], hit.elem, want),
        ];
    }
    let e = if role == MAT8_B {
        mat8_b_to_nv(ctx, insts, value)
    } else {
        mat8_c_to_nv(ctx, insts, value, elem, kind, want)
    };
    mat8_nv_record(ctx, value, role, e, want);
    e
}

fn lower_mat8_fused(
    ctx: &mut Ctx,
    signature: Matrix8Intrinsic,
    rty: Word,
    g: Mat8Fuse,
    c_elem: Word,
    result_elem: Word,
) -> Vec<Instruction> {
    mat8_mma_module_setup(ctx);
    let mut insts = Vec::with_capacity(64);
    let half = ctx.ty_half();
    let acc_elem = if mat8_acc_is_half(signature) {
        half
    } else {
        ctx.ty_float()
    };
    let zero_half = ctx.const_half(0.0);
    let zero_acc = if acc_elem == half {
        zero_half
    } else {
        ctx.const_float(0.0)
    };
    let (mut a_cells, mut b_cells, mut c_cells) = (Vec::new(), Vec::new(), Vec::new());
    for m in 0..g.rows {
        for k in 0..if g.a_mem.is_some() { 0 } else { g.ks } {
            let e = mat8_nv_operand(
                ctx,
                &mut insts,
                g.a[m][k],
                MAT8_A,
                half,
                signature.lhs,
                half,
            );
            for (s, element) in e.into_iter().enumerate() {
                a_cells.push(((4 * k + 2 * m + s) as u32, element));
            }
        }
        let e = mat8_nv_operand(
            ctx,
            &mut insts,
            g.c[m],
            MAT8_C,
            c_elem,
            signature.accumulator,
            acc_elem,
        );
        for (s, element) in e.into_iter().enumerate() {
            c_cells.push(((2 * m + s) as u32, element));
        }
    }
    for k in 0..if g.b_mem.is_some() { 0 } else { g.ks } {
        let e = mat8_nv_operand(ctx, &mut insts, g.b[k], MAT8_B, half, signature.rhs, half);
        for (s, element) in e.into_iter().enumerate() {
            b_cells.push(((2 * k + s) as u32, element));
        }
    }
    let kdim = 8 * g.ks as u32;
    let a_ty = coopmat_type(ctx, half, 16, kdim, 0);
    let b_ty = coopmat_type(ctx, half, kdim, 8, 1);
    let c_ty = coopmat_type(ctx, acc_elem, 16, 8, 2);
    let a = match g.a_mem {
        Some(mem) => mat8_cm_load(ctx, &mut insts, a_ty, mem),
        None => coopmat_from_cells(ctx, &mut insts, a_ty, zero_half, &a_cells),
    };
    let b = match g.b_mem {
        Some(mem) => mat8_cm_load(ctx, &mut insts, b_ty, mem),
        None => coopmat_from_cells(ctx, &mut insts, b_ty, zero_half, &b_cells),
    };
    let c = coopmat_from_cells(ctx, &mut insts, c_ty, zero_acc, &c_cells);
    let d = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::CooperativeMatrixMulAddKHR,
        Some(c_ty),
        Some(d),
        vec![Operand::IdRef(a), Operand::IdRef(b), Operand::IdRef(c)],
    ));
    for m in 0..g.rows {
        let e = [
            composite_extract(ctx, &mut insts, acc_elem, d, (2 * m) as u32),
            composite_extract(ctx, &mut insts, acc_elem, d, (2 * m + 1) as u32),
        ];
        mat8_nv_record(ctx, g.out[m], MAT8_C, e, acc_elem);
        let apple = mat8_nv_to_apple(ctx, &mut insts, e, acc_elem, result_elem, rty);
        insts.push(Instruction::new(
            Op::CopyObject,
            Some(rty),
            Some(g.out[m]),
            vec![Operand::IdRef(apple)],
        ));
    }
    insts
}

pub(in crate::passes) fn plan_mat8_fusion(
    ctx: &mut Ctx,
    entry_idx: usize,
    names: &HashMap<Word, String>,
) {
    ctx.mat8_fuse.clear();
    ctx.mat8_cm.clear();
    if std::env::var_os("NVMTL_MATRIX8_NO_FUSE").is_some() {
        return;
    }
    #[derive(Clone, Copy)]
    struct Mac {
        pos: usize,
        res: Word,
        a: Word,
        b: Word,
        c: Word,
        sig: Matrix8Intrinsic,
    }
    let blocks = &ctx.module.functions[entry_idx].blocks;
    let cm_env = mat8_cm_env(&ctx.module, &ctx.new_globals, blocks);
    let mut uses: HashMap<Word, usize> = HashMap::default();
    for block in blocks {
        for inst in &block.instructions {
            for op in &inst.operands {
                if let Operand::IdRef(v) = op {
                    *uses.entry(*v).or_insert(0) += 1;
                }
            }
        }
    }
    let mut roles: Vec<(Word, Mat8FuseRole)> = Vec::new();
    for block in blocks {
        let mut macs: Vec<Mac> = Vec::new();
        let mut use_pos: HashMap<Word, Vec<usize>> = HashMap::default();
        let mut loads: HashMap<Word, usize> = HashMap::default();
        let mut hazards: Vec<usize> = Vec::new();
        for (pos, inst) in block.instructions.iter().enumerate() {
            if inst.class.opcode != Op::Phi {
                for op in &inst.operands {
                    if let Operand::IdRef(v) = op {
                        use_pos.entry(*v).or_default().push(pos);
                    }
                }
            }
            if inst.class.opcode == Op::FunctionCall {
                let name = match inst.operands.first() {
                    Some(Operand::IdRef(c)) => names.get(c).map(String::as_str),
                    _ => None,
                };
                match (name, inst.result_id) {
                    (Some(n), Some(res)) if n.starts_with("air.simdgroup_matrix_8x8_load.") => {
                        loads.insert(res, pos);
                    }
                    (Some(n), _)
                        if n.starts_with("air.simdgroup_matrix_8x8_multiply_accumulate.") => {}
                    _ => hazards.push(pos),
                }
            } else if mat8_cm_hazard(inst.class.opcode) {
                hazards.push(pos);
            }
            if inst.class.opcode != Op::FunctionCall || inst.operands.len() != 4 {
                continue;
            }
            let (Some(res), Some(Operand::IdRef(callee))) = (inst.result_id, inst.operands.first())
            else {
                continue;
            };
            let Some(sig) = names
                .get(callee)
                .and_then(|n| crate::air_intrinsics::matrix8_intrinsic(n))
            else {
                continue;
            };
            if !mat8_mma_eligible(sig) {
                continue;
            }
            let (Operand::IdRef(a), Operand::IdRef(b), Operand::IdRef(c)) =
                (&inst.operands[1], &inst.operands[2], &inst.operands[3])
            else {
                continue;
            };
            macs.push(Mac {
                pos,
                res,
                a: *a,
                b: *b,
                c: *c,
                sig,
            });
        }
        if macs.len() < 2 {
            continue;
        }
        let by_res: HashMap<Word, usize> =
            macs.iter().enumerate().map(|(i, m)| (m.res, i)).collect();
        let mut taken = vec![false; macs.len()];
        let mut k_of: HashMap<usize, usize> = HashMap::default();
        for y in 0..macs.len() {
            let Some(&x) = by_res.get(&macs[y].c) else {
                continue;
            };
            if x < y
                && !taken[x]
                && !taken[y]
                && macs[x].sig == macs[y].sig
                && uses.get(&macs[x].res) == Some(&1)
            {
                taken[x] = true;
                taken[y] = true;
                k_of.insert(x, y);
            }
        }
        let units: Vec<(usize, Option<usize>)> = (0..macs.len())
            .filter_map(|i| match k_of.get(&i) {
                Some(&y) => Some((i, Some(y))),
                None if !taken[i] => Some((i, None)),
                None => None,
            })
            .collect();
        let members =
            |u: (usize, Option<usize>)| -> Vec<usize> { std::iter::once(u.0).chain(u.1).collect() };
        let out_of = |u: (usize, Option<usize>)| -> Word { macs[u.1.unwrap_or(u.0)].res };
        let mut merged = vec![false; units.len()];
        for ui in 0..units.len() {
            if merged[ui] {
                continue;
            }
            merged[ui] = true;
            let u = units[ui];
            let mut group = vec![u];
            for vi in ui + 1..units.len() {
                if merged[vi] {
                    continue;
                }
                let v = units[vi];
                if v.1.is_some() != u.1.is_some()
                    || macs[v.0].sig != macs[u.0].sig
                    || macs[v.0].b != macs[u.0].b
                    || v.1.map(|y| macs[y].b) != u.1.map(|y| macs[y].b)
                {
                    continue;
                }
                let results: Vec<Word> = members(u)
                    .into_iter()
                    .chain(members(v))
                    .map(|i| macs[i].res)
                    .collect();
                let reads_other = members(u).into_iter().chain(members(v)).any(|i| {
                    let own = members(if members(u).contains(&i) { u } else { v });
                    [macs[i].a, macs[i].b, macs[i].c]
                        .iter()
                        .any(|o| results.contains(o) && !own.iter().any(|&j| macs[j].res == *o))
                });
                if reads_other {
                    continue;
                }
                let lead = members(u)
                    .into_iter()
                    .chain(members(v))
                    .map(|i| macs[i].pos)
                    .max()
                    .unwrap_or(0);
                let early_reader = [out_of(u), out_of(v)].iter().any(|o| {
                    use_pos
                        .get(o)
                        .is_some_and(|ps| ps.iter().any(|&p| p <= lead))
                });
                if early_reader {
                    continue;
                }
                merged[vi] = true;
                group.push(v);
                break;
            }
            if group.len() == 1 && u.1.is_none() {
                continue;
            }
            let mut fuse = Mat8Fuse {
                rows: group.len(),
                ks: if u.1.is_some() { 2 } else { 1 },
                a: [[0; 2]; 2],
                b: [0; 2],
                c: [0; 2],
                out: [0; 2],
                a_mem: None,
                b_mem: None,
            };
            for (m, unit) in group.iter().enumerate() {
                fuse.a[m][0] = macs[unit.0].a;
                if let Some(y) = unit.1 {
                    fuse.a[m][1] = macs[y].a;
                }
                fuse.c[m] = macs[unit.0].c;
                fuse.out[m] = out_of(*unit);
            }
            fuse.b[0] = macs[u.0].b;
            if let Some(y) = u.1 {
                fuse.b[1] = macs[y].b;
            }
            let all: Vec<usize> = group.iter().flat_map(|unit| members(*unit)).collect();
            let lead = *all.iter().max_by_key(|&&i| macs[i].pos).unwrap();
            mat8_cm_plan(&cm_env, &mut fuse, macs[lead].pos, &loads, &hazards);
            for i in all {
                roles.push((
                    macs[i].res,
                    if i == lead {
                        Mat8FuseRole::Lead(fuse)
                    } else {
                        Mat8FuseRole::Skip
                    },
                ));
            }
        }
    }
    ctx.mat8_fuse.extend(roles);
}

fn first_non_phi(block: &Block) -> usize {
    block
        .instructions
        .iter()
        .position(|i| i.class.opcode != Op::Phi)
        .unwrap_or(block.instructions.len())
}

fn mat8_cm_load(ctx: &mut Ctx, insts: &mut Vec<Instruction>, ty: Word, mem: Mat8Mem) -> Word {
    if let Some(&hit) = ctx.mat8_cm.get(&(mem.key, ty)) {
        return hit;
    }
    let layout = ctx.const_uint(0);
    let stride = ctx.const_uint(mem.stride);
    let m = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::CooperativeMatrixLoadKHR,
        Some(ty),
        Some(m),
        vec![
            Operand::IdRef(mem.ptr),
            Operand::IdRef(layout),
            Operand::IdRef(stride),
        ],
    ));
    ctx.mat8_cm.insert((mem.key, ty), m);
    m
}

struct Mat8CmEnv<'a> {
    defs: HashMap<Word, &'a Instruction>,
    aligned: HashSet<Word>,
    off: bool,
    trace: bool,
}

fn mat8_cm_env<'a>(
    module: &'a Module,
    new_globals: &'a [Instruction],
    blocks: &'a [Block],
) -> Mat8CmEnv<'a> {
    let mut defs: HashMap<Word, &'a Instruction> = HashMap::default();
    for inst in module.types_global_values.iter().chain(new_globals.iter()) {
        if let Some(id) = inst.result_id {
            defs.insert(id, inst);
        }
    }
    for block in blocks {
        for inst in &block.instructions {
            if let Some(id) = inst.result_id {
                defs.insert(id, inst);
            }
        }
    }
    let mut aligned = HashSet::default();
    let mut offset_ok = true;
    for inst in &module.types_global_values {
        if inst.class.opcode != Op::Variable
            || inst.operands.first() != Some(&Operand::StorageClass(StorageClass::Workgroup))
        {
            continue;
        }
        let Some(id) = inst.result_id else { continue };
        if offset_ok {
            aligned.insert(id);
        }
        let bytes = inst
            .result_type
            .and_then(|pty| mat8_cm_pointee(&defs, pty))
            .and_then(|t| mat8_cm_bytes(&defs, t, 0));
        if bytes.is_none_or(|b| b % 16 != 0) {
            offset_ok = false;
        }
    }
    Mat8CmEnv {
        defs,
        aligned,
        off: std::env::var_os("NVMTL_MATRIX8_NO_CMLOAD").is_some(),
        trace: std::env::var_os("NVMTL_MATRIX8_CMLOAD_TRACE").is_some(),
    }
}

fn mat8_cm_pointee(defs: &HashMap<Word, &Instruction>, ptr_ty: Word) -> Option<Word> {
    let def = defs.get(&ptr_ty)?;
    match (def.class.opcode, def.operands.get(1)) {
        (Op::TypePointer, Some(Operand::IdRef(t))) => Some(*t),
        _ => None,
    }
}

fn mat8_cm_bytes(defs: &HashMap<Word, &Instruction>, ty: Word, depth: u32) -> Option<u64> {
    let def = defs.get(&ty)?;
    if depth > 8 {
        return None;
    }
    match (def.class.opcode, def.operands.first(), def.operands.get(1)) {
        (Op::TypeInt | Op::TypeFloat, Some(Operand::LiteralBit32(w)), _) if *w >= 8 => {
            Some(u64::from(*w / 8))
        }
        (Op::TypeVector, Some(Operand::IdRef(c)), Some(Operand::LiteralBit32(n)))
            if *n == 2 || *n == 4 =>
        {
            Some(mat8_cm_bytes(defs, *c, depth + 1)? * u64::from(*n))
        }
        (Op::TypeArray, Some(Operand::IdRef(e)), Some(Operand::IdRef(len))) => {
            let n = u64::try_from(mat8_cm_const(defs, *len)?).ok()?;
            Some(mat8_cm_bytes(defs, *e, depth + 1)? * n)
        }
        _ => None,
    }
}

fn mat8_cm_const(defs: &HashMap<Word, &Instruction>, v: Word) -> Option<i64> {
    let def = defs.get(&v)?;
    match (def.class.opcode, def.operands.first()) {
        (Op::ConstantNull | Op::ConstantFalse, _) => Some(0),
        (Op::Constant, Some(Operand::LiteralBit32(x))) => Some(i64::from(*x as i32)),
        (Op::Constant, Some(Operand::LiteralBit64(x))) => Some(*x as i64),
        _ => None,
    }
}

fn mat8_cm_is_zero(defs: &HashMap<Word, &Instruction>, v: Word) -> bool {
    let Some(def) = defs.get(&v) else {
        return false;
    };
    match def.class.opcode {
        Op::ConstantNull | Op::ConstantFalse => true,
        Op::ConstantComposite => def
            .operands
            .iter()
            .all(|o| matches!(o, Operand::IdRef(c) if mat8_cm_const(defs, *c) == Some(0))),
        _ => false,
    }
}

#[derive(Clone, PartialEq, Debug, Default)]
struct Mat8Aff {
    k: i64,
    t: Vec<(Word, i64)>,
}

impl Mat8Aff {
    fn plus(mut self, o: &Mat8Aff, s: i64) -> Mat8Aff {
        self.k = self.k.wrapping_add(o.k.wrapping_mul(s));
        for &(v, c) in &o.t {
            match self.t.binary_search_by_key(&v, |p| p.0) {
                Ok(i) => self.t[i].1 = self.t[i].1.wrapping_add(c.wrapping_mul(s)),
                Err(i) => self.t.insert(i, (v, c.wrapping_mul(s))),
            }
        }
        self.t.retain(|p| p.1 != 0);
        self
    }
    fn scaled(self, s: i64) -> Mat8Aff {
        Mat8Aff::default().plus(&self, s)
    }
}

fn mat8_cm_int(defs: &HashMap<Word, &Instruction>, v: Word, depth: u32) -> Mat8Aff {
    let leaf = || Mat8Aff {
        k: 0,
        t: vec![(v, 1)],
    };
    if let Some(k) = mat8_cm_const(defs, v) {
        return Mat8Aff { k, t: Vec::new() };
    }
    let Some(def) = defs.get(&v) else {
        return leaf();
    };
    if depth > 32 {
        return leaf();
    }
    let arg = |i: usize| match def.operands.get(i) {
        Some(Operand::IdRef(x)) => Some(*x),
        _ => None,
    };
    match (def.class.opcode, arg(0), arg(1)) {
        (Op::IAdd, Some(a), Some(b)) => {
            mat8_cm_int(defs, a, depth + 1).plus(&mat8_cm_int(defs, b, depth + 1), 1)
        }
        (Op::ISub, Some(a), Some(b)) => {
            mat8_cm_int(defs, a, depth + 1).plus(&mat8_cm_int(defs, b, depth + 1), -1)
        }
        (Op::IMul, Some(a), Some(b)) => {
            let (x, y) = (
                mat8_cm_int(defs, a, depth + 1),
                mat8_cm_int(defs, b, depth + 1),
            );
            if y.t.is_empty() {
                x.scaled(y.k)
            } else if x.t.is_empty() {
                y.scaled(x.k)
            } else {
                leaf()
            }
        }
        (Op::ShiftLeftLogical, Some(a), Some(b)) => match mat8_cm_const(defs, b) {
            Some(s) if (0..31).contains(&s) => mat8_cm_int(defs, a, depth + 1).scaled(1i64 << s),
            _ => leaf(),
        },
        (Op::UConvert | Op::SConvert | Op::CopyObject, Some(a), _) => {
            mat8_cm_int(defs, a, depth + 1)
        }
        _ => leaf(),
    }
}

fn mat8_cm_tz(defs: &HashMap<Word, &Instruction>, v: Word, depth: u32) -> u32 {
    if let Some(k) = mat8_cm_const(defs, v) {
        return if k == 0 { 64 } else { k.trailing_zeros() };
    }
    let Some(def) = defs.get(&v) else { return 0 };
    if depth > 32 {
        return 0;
    }
    let arg = |i: usize| match def.operands.get(i) {
        Some(Operand::IdRef(x)) => Some(*x),
        _ => None,
    };
    let tz = |x: Word| mat8_cm_tz(defs, x, depth + 1);
    match (def.class.opcode, arg(0), arg(1)) {
        (Op::BitwiseAnd, Some(a), Some(b)) => tz(a).max(tz(b)),
        (Op::BitwiseOr | Op::IAdd | Op::ISub, Some(a), Some(b)) => tz(a).min(tz(b)),
        (Op::IMul, Some(a), Some(b)) => (tz(a) + tz(b)).min(64),
        (Op::ShiftLeftLogical, Some(a), Some(b)) => match mat8_cm_const(defs, b) {
            Some(s) if (0..64).contains(&s) => (tz(a) + s as u32).min(64),
            _ => 0,
        },
        (Op::UConvert | Op::SConvert | Op::CopyObject, Some(a), _) => tz(a),
        _ => 0,
    }
}

fn mat8_cm_ptr(defs: &HashMap<Word, &Instruction>, p: Word, depth: u32) -> Option<(Word, Mat8Aff)> {
    let def = defs.get(&p)?;
    if depth > 32 {
        return None;
    }
    let arg = |i: usize| match def.operands.get(i) {
        Some(Operand::IdRef(x)) => Some(*x),
        _ => None,
    };
    match def.class.opcode {
        Op::AccessChain | Op::InBoundsAccessChain if def.operands.len() == 2 => {
            let (var, idx) = (arg(0)?, arg(1)?);
            let vdef = defs.get(&var)?;
            if vdef.class.opcode != Op::Variable
                || vdef.operands.first() != Some(&Operand::StorageClass(StorageClass::Workgroup))
            {
                return None;
            }
            let arr = defs.get(&mat8_cm_pointee(defs, vdef.result_type?)?)?;
            let elem = match (arr.class.opcode, arr.operands.first()) {
                (Op::TypeArray, Some(Operand::IdRef(e))) => *e,
                _ => return None,
            };
            if !matches!(defs.get(&elem)?.class.opcode, Op::TypeFloat | Op::TypeInt) {
                return None;
            }
            Some((var, mat8_cm_int(defs, idx, 0)))
        }
        Op::PtrAccessChain | Op::InBoundsPtrAccessChain if def.operands.len() == 2 => {
            let (base, n) = (arg(0)?, arg(1)?);
            if def.result_type != defs.get(&base)?.result_type {
                return None;
            }
            let (var, a) = mat8_cm_ptr(defs, base, depth + 1)?;
            Some((var, a.plus(&mat8_cm_int(defs, n, 0), 1)))
        }
        Op::CopyObject => mat8_cm_ptr(defs, arg(0)?, depth + 1),
        _ => None,
    }
}

fn mat8_cm_hazard(op: Op) -> bool {
    matches!(
        op,
        Op::Store
            | Op::CopyMemory
            | Op::CopyMemorySized
            | Op::ControlBarrier
            | Op::MemoryBarrier
            | Op::ImageWrite
            | Op::FunctionCall
    ) || format!("{op:?}").starts_with("Atomic")
}

fn mat8_cm_plan(
    env: &Mat8CmEnv,
    fuse: &mut Mat8Fuse,
    lead: usize,
    loads: &HashMap<Word, usize>,
    hazards: &[usize],
) {
    if env.off {
        return;
    }
    if fuse.rows == 2 {
        let parts: Vec<(Word, i64, i64)> = (0..2)
            .flat_map(|m| (0..fuse.ks).map(move |k| (m, k)))
            .map(|(m, k)| (fuse.a[m][k], 8 * m as i64, 8 * k as i64))
            .collect();
        fuse.a_mem = mat8_cm_tile(env, &parts, lead, loads, hazards, "A");
    }
    let parts: Vec<(Word, i64, i64)> = (0..fuse.ks).map(|k| (fuse.b[k], 8 * k as i64, 0)).collect();
    fuse.b_mem = mat8_cm_tile(env, &parts, lead, loads, hazards, "B");
}

fn mat8_cm_tile(
    env: &Mat8CmEnv,
    parts: &[(Word, i64, i64)],
    lead: usize,
    loads: &HashMap<Word, usize>,
    hazards: &[usize],
    what: &str,
) -> Option<Mat8Mem> {
    let r = mat8_cm_tile_why(env, parts, lead, loads, hazards);
    if env.trace {
        match &r {
            Ok(m) => eprintln!(
                "mat8 cmload {what} %{}: tile ptr %{} stride {}",
                parts[0].0, m.ptr, m.stride
            ),
            Err(why) => eprintln!("mat8 cmload {what} %{}: cells ({why})", parts[0].0),
        }
    }
    r.ok()
}

fn mat8_cm_tile_why(
    env: &Mat8CmEnv,
    parts: &[(Word, i64, i64)],
    lead: usize,
    loads: &HashMap<Word, usize>,
    hazards: &[usize],
) -> Result<Mat8Mem, String> {
    let defs = &env.defs;
    let mut first_pos = usize::MAX;
    let mut base: Option<(Word, Word, Mat8Aff, i64)> = None;
    for &(value, row, col) in parts {
        let pos = *loads
            .get(&value)
            .ok_or_else(|| format!("%{value} is not an 8x8 load in this block"))?;
        if pos >= lead {
            return Err(format!("load of %{value} is after the lead"));
        }
        first_pos = first_pos.min(pos);
        let call = defs.get(&value).ok_or("load has no def")?;
        let op = |i: usize| match call.operands.get(i) {
            Some(Operand::IdRef(x)) => Ok(*x),
            _ => Err(format!("load operand {i} missing")),
        };
        if call.operands.len() != 5 {
            return Err(format!(
                "load has {} operands, want ptr/stride/origin/transpose",
                call.operands.len() - 1
            ));
        }
        let result_def = call.result_type.and_then(|t| defs.get(&t));
        let half_result = result_def
            .and_then(|d| match (d.class.opcode, d.operands.first()) {
                (Op::TypeVector | Op::TypeArray, Some(Operand::IdRef(c))) => defs.get(c),
                _ => None,
            })
            .is_some_and(|c| {
                c.class.opcode == Op::TypeFloat
                    && c.operands.first() == Some(&Operand::LiteralBit32(16))
            });
        if !half_result {
            return Err(format!(
                "load is not half ({:?})",
                result_def.map(|d| d.class.opcode)
            ));
        }
        let (ptr, stride_id, origin, transpose) = (op(1)?, op(2)?, op(3)?, op(4)?);
        let stride = mat8_cm_const(defs, stride_id).ok_or("stride is not a constant")?;
        if !mat8_cm_is_zero(defs, origin) || !mat8_cm_is_zero(defs, transpose) {
            return Err("origin or transpose is not constant zero".into());
        }
        let (var, aff) =
            mat8_cm_ptr(defs, ptr, 0).ok_or("pointer is not a Workgroup element chain")?;
        match &base {
            None => {
                if row != 0 || col != 0 {
                    return Err("first part is not the tile origin".into());
                }
                base = Some((ptr, var, aff, stride));
            }
            Some((_, bvar, baff, bstride)) => {
                if var != *bvar || stride != *bstride {
                    return Err("parts differ in variable or stride".into());
                }
                let d = aff.plus(baff, -1);
                if !d.t.is_empty() || d.k != row * stride + col {
                    return Err(format!("part offset {:?} != {}", d, row * stride + col));
                }
            }
        }
    }
    if hazards.iter().any(|&h| h > first_pos && h < lead) {
        return Err("store, barrier or call between the loads and the lead".into());
    }
    let (ptr, var, aff, stride) = base.ok_or("no parts")?;
    if !env.aligned.contains(&var) {
        return Err("variable offset not provably 16 B aligned".into());
    }
    if stride <= 0 || stride % 8 != 0 || stride > i64::from(u32::MAX) {
        return Err(format!(
            "stride {stride} is not a positive multiple of 8 halves"
        ));
    }
    if aff.k.rem_euclid(8) != 0 {
        return Err(format!("constant offset {} is not 16 B aligned", aff.k));
    }
    for &(leaf, c) in &aff.t {
        if c.trailing_zeros().min(64) + mat8_cm_tz(defs, leaf, 0) < 3 {
            return Err(format!("term {c}*%{leaf} is not provably 16 B aligned"));
        }
    }
    Ok(Mat8Mem {
        ptr,
        stride: stride as u32,
        key: parts[0].0,
    })
}

fn before_terminator(block: &Block) -> usize {
    let mut at = block
        .instructions
        .iter()
        .rposition(|i| is_block_terminator(i.class.opcode))
        .unwrap_or(block.instructions.len());
    if at > 0
        && matches!(
            block.instructions[at - 1].class.opcode,
            Op::LoopMerge | Op::SelectionMerge
        )
    {
        at -= 1;
    }
    at
}

pub(in crate::passes) fn plan_mat8_nv_phis(
    ctx: &mut Ctx,
    entry_idx: usize,
    names: &HashMap<Word, String>,
) {
    ctx.mat8_nv.clear();
    ctx.mat8_nv_phis.clear();
    if std::env::var_os("NVMTL_MATRIX8_NO_CARRY").is_some() {
        return;
    }
    let mut c_of: HashMap<Word, Option<(Matrix16Element, bool)>> = HashMap::default();
    let mut read_by_phi: HashSet<Word> = HashSet::default();
    let mut phis: Vec<(usize, Word, Word)> = Vec::new();
    let mut mac_result: HashMap<Word, (Matrix16Element, bool)> = HashMap::default();
    let mut incoming_of: HashMap<Word, Vec<Word>> = HashMap::default();
    for (bi, block) in ctx.module.functions[entry_idx].blocks.iter().enumerate() {
        for inst in &block.instructions {
            match inst.class.opcode {
                Op::Phi => {
                    if let (Some(id), Some(ty)) = (inst.result_id, inst.result_type) {
                        phis.push((bi, id, ty));
                    }
                    for op in &inst.operands {
                        if let Operand::IdRef(v) = op {
                            read_by_phi.insert(*v);
                        }
                    }
                    if let Some(id) = inst.result_id {
                        let values =
                            inst.operands
                                .chunks(2)
                                .filter_map(|pair| match pair.first() {
                                    Some(Operand::IdRef(v)) => Some(*v),
                                    _ => None,
                                });
                        incoming_of.insert(id, values.collect());
                    }
                }
                Op::FunctionCall => {
                    let Some(Operand::IdRef(callee)) = inst.operands.first() else {
                        continue;
                    };
                    let Some(sig) = names
                        .get(callee)
                        .and_then(|n| crate::air_intrinsics::matrix8_intrinsic(n))
                    else {
                        continue;
                    };
                    if !mat8_mma_eligible(sig) {
                        continue;
                    }
                    if let Some(res) = inst.result_id {
                        mac_result.insert(res, (sig.result, mat8_acc_is_half(sig)));
                    }
                    let Some(Operand::IdRef(c)) = inst.operands.get(3) else {
                        continue;
                    };
                    let key = (sig.accumulator, mat8_acc_is_half(sig));
                    c_of.entry(*c)
                        .and_modify(|k| {
                            if *k != Some(key) {
                                *k = None
                            }
                        })
                        .or_insert(Some(key));
                }
                _ => {}
            }
        }
    }
    let mut chosen = Vec::new();
    for (bi, phi, ty) in phis {
        let key = match c_of.get(&phi).copied() {
            Some(k) => k,
            None if std::env::var_os("NVMTL_MATRIX8_NO_EXIT_CARRY").is_none() => {
                let mut keys = incoming_of
                    .get(&phi)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| mac_result.get(v).copied());
                match keys.next() {
                    Some(k) if keys.all(|o| o == k) => Some(k),
                    _ => None,
                }
            }
            None => None,
        };
        let Some((kind, half_acc)) = key else {
            continue;
        };
        if read_by_phi.contains(&phi) {
            continue;
        }
        let Some((c_elem, 64)) = composite_shape(ctx, ty) else {
            continue;
        };
        chosen.push((bi, phi, ty, c_elem, kind, half_acc));
    }
    for (bi, phi, ty, c_elem, kind, half_acc) in chosen {
        let acc_elem = if half_acc {
            ctx.ty_half()
        } else {
            ctx.ty_float()
        };
        let e = [ctx.module.fresh_id(), ctx.module.fresh_id()];
        ctx.mat8_nv.insert(
            (phi, MAT8_C),
            Mat8Nv {
                e,
                elem: acc_elem,
                block: bi,
            },
        );
        ctx.mat8_nv_phis.push(Mat8NvPhi {
            phi,
            ty,
            c_elem,
            kind,
            acc_elem,
            e,
            block: bi,
        });
    }
}

pub(in crate::passes) fn emit_mat8_nv_phis(ctx: &mut Ctx, entry_idx: usize) -> Result<(), String> {
    let plan = std::mem::take(&mut ctx.mat8_nv_phis);
    for p in plan {
        let labels: HashMap<Word, usize> = ctx.module.functions[entry_idx]
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| Some((b.label.as_ref()?.result_id?, i)))
            .collect();
        let phi_inst = ctx.module.functions[entry_idx].blocks[p.block]
            .instructions
            .iter()
            .find(|i| i.result_id == Some(p.phi))
            .cloned()
            .ok_or_else(|| format!("batch 45: planned matrix phi %{} vanished", p.phi))?;
        let mut incoming = Vec::new();
        for pair in phi_inst.operands.chunks(2) {
            let (Some(Operand::IdRef(v)), Some(Operand::IdRef(l))) = (pair.first(), pair.get(1))
            else {
                return Err(format!(
                    "batch 45: matrix phi %{} has a malformed incoming pair",
                    p.phi
                ));
            };
            let pi = *labels
                .get(l)
                .ok_or_else(|| format!("batch 45: phi %{} names unknown block %{l}", p.phi))?;
            let dominance = &ctx
                .air_call_body
                .as_ref()
                .ok_or("batch 45: phi emit outside the walk")?
                .dominance;
            let hit = ctx
                .mat8_nv
                .get(&(*v, MAT8_C))
                .copied()
                .filter(|h| dominance.dominates(h.block, pi));
            incoming.push((*v, *l, pi, hit));
        }
        let mut ops: [Vec<Operand>; 2] = [Vec::new(), Vec::new()];
        for (v, l, pi, hit) in incoming {
            let mut tail = Vec::new();
            let e = match hit {
                Some(h) => [
                    convert_matrix_lane(ctx, &mut tail, h.e[0], h.elem, p.acc_elem),
                    convert_matrix_lane(ctx, &mut tail, h.e[1], h.elem, p.acc_elem),
                ],
                None => mat8_c_to_nv(ctx, &mut tail, v, p.c_elem, p.kind, p.acc_elem),
            };
            if !tail.is_empty() {
                let block = &mut ctx.module.functions[entry_idx].blocks[pi];
                let at = before_terminator(block);
                block.instructions.splice(at..at, tail);
            }
            for k in 0..2 {
                ops[k].push(Operand::IdRef(e[k]));
                ops[k].push(Operand::IdRef(l));
            }
        }
        {
            let block = &mut ctx.module.functions[entry_idx].blocks[p.block];
            let at = first_non_phi(block);
            for k in (0..2).rev() {
                block.instructions.insert(
                    at,
                    Instruction::new(Op::Phi, Some(p.acc_elem), Some(p.e[k]), ops[k].clone()),
                );
            }
        }
        let merge = ctx.module.functions[entry_idx].blocks[p.block]
            .instructions
            .iter()
            .find(|i| i.class.opcode == Op::LoopMerge)
            .and_then(|i| match i.operands.first() {
                Some(Operand::IdRef(m)) => labels.get(m).copied(),
                _ => None,
            });
        let mut site: [Option<Word>; 2] = [None, None];
        let n_blocks = ctx.module.functions[entry_idx].blocks.len();
        for bi in 0..n_blocks {
            let reads = ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .iter()
                .any(|i| i.class.opcode != Op::Phi && i.operands.contains(&Operand::IdRef(p.phi)));
            if !reads {
                continue;
            }
            let dominance = &ctx
                .air_call_body
                .as_ref()
                .ok_or("batch 45: phi emit outside the walk")?
                .dominance;
            let (which, target) = match merge {
                Some(m) if dominance.dominates(m, bi) => (1, m),
                _ if dominance.dominates(p.block, bi) => (0, p.block),
                _ => {
                    return Err(format!(
                        "batch 45: matrix phi %{} is read where it does not dominate",
                        p.phi
                    ))
                }
            };
            let conv = match site[which] {
                Some(id) => id,
                None => {
                    let mut insts = Vec::new();
                    let id = mat8_nv_to_apple(ctx, &mut insts, p.e, p.acc_elem, p.c_elem, p.ty);
                    let block = &mut ctx.module.functions[entry_idx].blocks[target];
                    let at = first_non_phi(block);
                    block.instructions.splice(at..at, insts);
                    site[which] = Some(id);
                    id
                }
            };
            for inst in ctx.module.functions[entry_idx].blocks[bi]
                .instructions
                .iter_mut()
            {
                if inst.class.opcode == Op::Phi {
                    continue;
                }
                for op in inst.operands.iter_mut() {
                    if *op == Operand::IdRef(p.phi) {
                        *op = Operand::IdRef(conv);
                    }
                }
            }
        }
    }
    Ok(())
}

struct CmChain {
    ty: Word,
    zero: Word,
    cells: Vec<Word>,
    ids: Vec<Word>,
}

fn cmphi_cell_count(ctx: &Ctx, ty: Word) -> Option<usize> {
    let def = type_def_of(ctx, ty)?;
    if def.class.opcode != Op::TypeCooperativeMatrixKHR {
        return None;
    }
    let id = |k: usize| match def.operands.get(k) {
        Some(Operand::IdRef(v) | Operand::IdScope(v)) => Some(*v),
        _ => None,
    };
    if constant_u32(ctx, id(1)?)? != Scope::Subgroup as u32 {
        return None;
    }
    let cells = constant_u32(ctx, id(2)?)?.checked_mul(constant_u32(ctx, id(3)?)?)? / 32;
    (cells > 0).then_some(cells as usize)
}

fn cmphi_defs(blocks: &[Block]) -> HashMap<Word, (usize, usize)> {
    let mut defs = HashMap::default();
    for (bi, block) in blocks.iter().enumerate() {
        for (ii, inst) in block.instructions.iter().enumerate() {
            if let Some(id) = inst.result_id {
                defs.insert(id, (bi, ii));
            }
        }
    }
    defs
}

fn cmphi_chain(
    ctx: &Ctx,
    blocks: &[Block],
    defs: &HashMap<Word, (usize, usize)>,
    head: Word,
) -> Option<CmChain> {
    let inst_of = move |id: Word| defs.get(&id).map(|&(b, i)| &blocks[b].instructions[i]);
    let ty = inst_of(head)?.result_type?;
    let n = cmphi_cell_count(ctx, ty)?;
    let mut cells: Vec<Option<Word>> = vec![None; n];
    let mut ids = Vec::new();
    let mut id = head;
    for _ in 0..4 * n + 1 {
        let inst = inst_of(id)?;
        if inst.result_type != Some(ty) {
            return None;
        }
        ids.push(id);
        match (inst.class.opcode, inst.operands.as_slice()) {
            (
                Op::CompositeInsert,
                [Operand::IdRef(value), Operand::IdRef(base), Operand::LiteralBit32(index)],
            ) => {
                let slot = cells.get_mut(*index as usize)?;
                if slot.is_none() {
                    *slot = Some(*value);
                }
                id = *base;
            }
            (Op::CompositeConstruct, [Operand::IdRef(zero)]) => {
                let cells = cells.into_iter().collect::<Option<Vec<_>>>()?;
                return Some(CmChain {
                    ty,
                    zero: *zero,
                    cells,
                    ids,
                });
            }
            _ => return None,
        }
    }
    None
}

fn cmphi_source(
    blocks: &[Block],
    defs: &HashMap<Word, (usize, usize)>,
    ty: Word,
    values: &[Word],
) -> Option<Word> {
    let mut source: Option<Word> = None;
    for (i, value) in values.iter().enumerate() {
        let &(b, ii) = defs.get(value)?;
        let inst = &blocks[b].instructions[ii];
        let (Op::CompositeExtract, [Operand::IdRef(d), Operand::LiteralBit32(at)]) =
            (inst.class.opcode, inst.operands.as_slice())
        else {
            return None;
        };
        if *at as usize != i || source.is_some_and(|s| s != *d) {
            return None;
        }
        source = Some(*d);
    }
    let d = source?;
    let &(b, ii) = defs.get(&d)?;
    (blocks[b].instructions[ii].result_type == Some(ty)).then_some(d)
}

fn cmphi_edges(
    blocks: &[Block],
    defs: &HashMap<Word, (usize, usize)>,
    phis: &[&Instruction],
    ty: Word,
) -> Option<Vec<(Word, Result<Word, Vec<Word>>)>> {
    let incoming = |phi: &Instruction, label: Word| {
        phi.operands.chunks(2).find_map(|pair| match pair {
            [Operand::IdRef(v), Operand::IdRef(l)] if *l == label => Some(*v),
            _ => None,
        })
    };
    let first = phis.first()?;
    let mut edges = Vec::new();
    for pair in first.operands.chunks(2) {
        let [_, Operand::IdRef(label)] = pair else {
            return None;
        };
        let values = phis
            .iter()
            .map(|phi| incoming(phi, *label))
            .collect::<Option<Vec<_>>>()?;
        edges.push((
            *label,
            cmphi_source(blocks, defs, ty, &values).ok_or(values),
        ));
    }
    if phis
        .iter()
        .any(|phi| phi.operands.len() != first.operands.len())
        || !edges.iter().any(|e| e.1.is_ok())
    {
        return None;
    }
    Some(edges)
}

fn cmphi_sweep(blocks: &mut [Block], mut dead: Vec<Word>) -> HashSet<Word> {
    let mut swept = HashSet::default();
    loop {
        let read: HashSet<Word> = blocks
            .iter()
            .flat_map(|b| b.instructions.iter())
            .flat_map(|i| i.operands.iter())
            .filter_map(|o| match o {
                Operand::IdRef(v) => Some(*v),
                _ => None,
            })
            .collect();
        let gone: HashSet<Word> = dead.iter().copied().filter(|d| !read.contains(d)).collect();
        if gone.is_empty() {
            return swept;
        }
        for block in blocks.iter_mut() {
            block
                .instructions
                .retain(|i| !i.result_id.is_some_and(|r| gone.contains(&r)));
        }
        dead.retain(|d| !gone.contains(d));
        swept.extend(gone);
    }
}

pub(in crate::passes) fn fuse_mat8_coopmat_phis(
    ctx: &mut Ctx,
    entry_idx: usize,
) -> Result<(), String> {
    if std::env::var_os("NVMTL_MATRIX8_NO_CMPHI").is_some() {
        return Ok(());
    }
    let neg = std::env::var_os("NVMTL_MATRIX8_CMPHI_NEG").is_some();
    let mut dead: Vec<Word> = Vec::new();

    let forward = {
        let blocks = &ctx.module.functions[entry_idx].blocks;
        let defs = cmphi_defs(blocks);
        let mut forward = Vec::new();
        for (bi, block) in blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::CooperativeMatrixMulAddKHR {
                    continue;
                }
                for k in 0..3 {
                    let Some(&Operand::IdRef(operand)) = inst.operands.get(k) else {
                        continue;
                    };
                    let Some(chain) = cmphi_chain(ctx, blocks, &defs, operand) else {
                        continue;
                    };
                    if let Some(d) = cmphi_source(blocks, &defs, chain.ty, &chain.cells) {
                        forward.push((bi, ii, k, d, chain.ids));
                    }
                }
            }
        }
        forward
    };
    let forwarded = forward.len();
    for (bi, ii, k, d, ids) in forward {
        ctx.module.functions[entry_idx].blocks[bi].instructions[ii].operands[k] = Operand::IdRef(d);
        dead.extend(ids);
    }

    struct Group {
        header: usize,
        phis: Vec<(Word, Word)>,
        ty: Word,
        zero: Word,
        edges: Vec<(Word, Result<Word, Vec<Word>>)>,
    }
    let (groups, sites) = {
        let blocks = &ctx.module.functions[entry_idx].blocks;
        let defs = cmphi_defs(blocks);
        let mut groups: Vec<Group> = Vec::new();
        let mut owner: HashMap<Word, usize> = HashMap::default();
        let mut sites: Vec<(usize, usize, usize, Vec<Word>)> = Vec::new();
        for (bi, block) in blocks.iter().enumerate() {
            for (ii, inst) in block.instructions.iter().enumerate() {
                if inst.class.opcode != Op::CooperativeMatrixMulAddKHR {
                    continue;
                }
                let Some(&Operand::IdRef(c)) = inst.operands.get(2) else {
                    continue;
                };
                let Some(chain) = cmphi_chain(ctx, blocks, &defs, c) else {
                    continue;
                };
                let Some(&(header, _)) = chain.cells.first().and_then(|v| defs.get(v)) else {
                    continue;
                };
                let mut phis: Vec<&Instruction> = Vec::new();
                for v in &chain.cells {
                    let Some(&(b, i)) = defs.get(v) else { break };
                    let phi = &blocks[b].instructions[i];
                    if b != header || phi.class.opcode != Op::Phi || phi.result_type.is_none() {
                        break;
                    }
                    phis.push(phi);
                }
                let distinct: HashSet<Word> = chain.cells.iter().copied().collect();
                if phis.len() != chain.cells.len() || distinct.len() != chain.cells.len() {
                    continue;
                }
                let owners: HashSet<Option<usize>> =
                    chain.cells.iter().map(|v| owner.get(v).copied()).collect();
                let group = match owners.into_iter().collect::<Vec<_>>().as_slice() {
                    [Some(g)]
                        if groups[*g].ty == chain.ty
                            && groups[*g]
                                .phis
                                .iter()
                                .map(|p| p.0)
                                .eq(chain.cells.iter().copied()) =>
                    {
                        *g
                    }
                    [None] => {
                        let Some(edges) = cmphi_edges(blocks, &defs, &phis, chain.ty) else {
                            continue;
                        };
                        if defs.contains_key(&chain.zero) && edges.iter().any(|e| e.1.is_err()) {
                            continue;
                        }
                        groups.push(Group {
                            header,
                            phis: phis
                                .iter()
                                .filter_map(|p| Some((p.result_id?, p.result_type?)))
                                .collect(),
                            ty: chain.ty,
                            zero: chain.zero,
                            edges,
                        });
                        for v in &chain.cells {
                            owner.insert(*v, groups.len() - 1);
                        }
                        groups.len() - 1
                    }
                    _ => continue,
                };
                sites.push((bi, ii, group, chain.ids));
            }
        }
        (groups, sites)
    };
    let p_ids: Vec<Word> = groups.iter().map(|_| ctx.module.fresh_id()).collect();
    for (bi, ii, g, ids) in sites {
        ctx.module.functions[entry_idx].blocks[bi].instructions[ii].operands[2] =
            Operand::IdRef(p_ids[g]);
        dead.extend(ids);
    }
    let labels: HashMap<Word, usize> = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| Some((b.label.as_ref()?.result_id?, i)))
        .collect();
    let mut replace: HashMap<Word, Word> = HashMap::default();
    for (g, group) in groups.iter().enumerate() {
        let p = p_ids[g];
        let mut ops = Vec::with_capacity(2 * group.edges.len());
        for (label, edge) in &group.edges {
            let value = match edge {
                Ok(d) => {
                    if neg {
                        p
                    } else {
                        *d
                    }
                }
                Err(values) => {
                    let pi = *labels
                        .get(label)
                        .ok_or_else(|| format!("tc cmphi: a phi names unknown block %{label}"))?;
                    let cells: Vec<(u32, Word)> = values
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (i as u32, *v))
                        .collect();
                    let mut tail = Vec::new();
                    let m = coopmat_from_cells(ctx, &mut tail, group.ty, group.zero, &cells);
                    let block = &mut ctx.module.functions[entry_idx].blocks[pi];
                    let at = before_terminator(block);
                    block.instructions.splice(at..at, tail);
                    m
                }
            };
            ops.push(Operand::IdRef(value));
            ops.push(Operand::IdRef(*label));
        }
        let mut extracts = Vec::with_capacity(group.phis.len());
        for (i, (phi, elem)) in group.phis.iter().enumerate() {
            let x = composite_extract(ctx, &mut extracts, *elem, p, i as u32);
            replace.insert(*phi, x);
        }
        let block = &mut ctx.module.functions[entry_idx].blocks[group.header];
        block.instructions.retain(|inst| {
            !(inst.class.opcode == Op::Phi
                && inst.result_id.is_some_and(|r| replace.contains_key(&r)))
        });
        block
            .instructions
            .insert(0, Instruction::new(Op::Phi, Some(group.ty), Some(p), ops));
        let at = first_non_phi(block);
        block.instructions.splice(at..at, extracts);
    }
    if !replace.is_empty() {
        for block in ctx.module.functions[entry_idx].blocks.iter_mut() {
            for inst in block.instructions.iter_mut() {
                for op in inst.operands.iter_mut() {
                    if let Operand::IdRef(v) = op {
                        if let Some(x) = replace.get(&*v) {
                            *v = *x;
                        }
                    }
                }
            }
        }
    }
    let swept = cmphi_sweep(&mut ctx.module.functions[entry_idx].blocks, dead);
    let survivor = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .find(|i| {
            i.result_id.is_some_and(|r| replace.contains_key(&r))
                || i.operands
                    .iter()
                    .any(|o| matches!(o, Operand::IdRef(v) if replace.contains_key(v)))
        });
    if let Some(inst) = survivor {
        return Err(format!(
            "tc cmphi: a fused scalar accumulator phi survived in an {:?}",
            inst.class.opcode
        ));
    }
    let gone = |i: &Instruction| matches!(i.operands.first(), Some(Operand::IdRef(t)) if replace.contains_key(t) || swept.contains(t));
    ctx.module.debug_names.retain(|i| !gone(i));
    ctx.module.annotations.retain(|i| !gone(i));
    if std::env::var_os("NVMTL_MATRIX8_CMPHI_TRACE").is_some() {
        eprintln!(
            "mat8 cmphi: {forwarded} operands forwarded, {} scalar phis -> {} coop phis, {} chain insts swept{}",
            replace.len(),
            groups.len(),
            swept.len(),
            if neg { " (NEG: P feeds itself)" } else { "" }
        );
    }
    Ok(())
}

fn matrix8_storage_matches(ctx: &Ctx, elem: Word, kind: Matrix16Element) -> bool {
    match kind {
        Matrix16Element::F32 => is_f32_scalar(ctx, elem),
        Matrix16Element::F16 => is_half_scalar(ctx, elem),
        Matrix16Element::Bf16 => is_int_scalar_width(ctx, elem, 16),
        Matrix16Element::F8E4M3 | Matrix16Element::F8E4M3Fn | Matrix16Element::F8E5M2 => {
            is_int_scalar_width(ctx, elem, 8)
        }
        Matrix16Element::I8 { .. } => false,
    }
}

fn convert_matrix_lane(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    value: Word,
    from: Word,
    to: Word,
) -> Word {
    if from == to {
        return value;
    }
    let converted = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::FConvert,
        Some(to),
        Some(converted),
        vec![Operand::IdRef(value)],
    ));
    converted
}

pub(in crate::passes) fn agx2_matmad_dimension(name: &str) -> Option<u32> {
    match name {
        "llvm.agx2.f16matmad4x4.v2f16" | "llvm.agx2.f32matmad4x4.v2f32" => Some(4),
        "llvm.agx2.f16matmad8x8.v2f16" | "llvm.agx2.f32matmad8x8.v2f32" => Some(8),
        _ => None,
    }
}

pub(in crate::passes) fn lower_agx2_matmad(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
    dimension: u32,
) -> Result<Vec<Instruction>, String> {
    if args.len() != 3 {
        return Err(format!("{name} expects 3 operands, got {}", args.len()));
    }
    if !matches!(dimension, 4 | 8) {
        return Err(format!("{name} has unsupported dimension {dimension}"));
    }
    let (elem, lanes) = composite_shape(ctx, rty)
        .ok_or_else(|| format!("{name} result is not a two-lane float composite"))?;
    if lanes != 2 || (!is_f32_scalar(ctx, elem) && !is_half_scalar(ctx, elem)) {
        return Err(format!("{name} result is not v2f16 or v2f32"));
    }
    for (ordinal, arg) in args.iter().enumerate() {
        let arg_ty = value_result_type(ctx, *arg)
            .ok_or_else(|| format!("{name} operand {ordinal} has no type"))?;
        let (arg_elem, arg_lanes) = composite_shape(ctx, arg_ty)
            .ok_or_else(|| format!("{name} operand {ordinal} is not a two-lane composite"))?;
        if arg_lanes != 2 || arg_elem != elem {
            return Err(format!(
                "{name} operand {ordinal} does not match its v2 result element type"
            ));
        }
    }

    let mut out = Vec::with_capacity(96);
    let uint = ctx.ty_uint();
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut out);
    let simd_lane = metal_simd_lane_local_u32(ctx, subgroup_lane, &mut out);
    let simd_base = metal_simd_lane_base_u32(ctx, subgroup_lane, simd_lane, &mut out);

    let a_preserve_mask = if dimension == 4 { 0x1e } else { 0x16 };
    let b_preserve_mask = if dimension == 4 { 0x19 } else { 0x09 };
    let a_key = bitwise_with_const(
        ctx,
        &mut out,
        Op::BitwiseAnd,
        uint,
        simd_lane,
        a_preserve_mask,
    );
    let b_key = bitwise_with_const(
        ctx,
        &mut out,
        Op::BitwiseAnd,
        uint,
        simd_lane,
        b_preserve_mask,
    );

    let a_components = [
        composite_extract(ctx, &mut out, elem, args[0], 0),
        composite_extract(ctx, &mut out, elem, args[0], 1),
    ];
    let b_components = [
        composite_extract(ctx, &mut out, elem, args[1], 0),
        composite_extract(ctx, &mut out, elem, args[1], 1),
    ];
    let mut accumulators = [
        composite_extract(ctx, &mut out, elem, args[2], 0),
        composite_extract(ctx, &mut out, elem, args[2], 1),
    ];
    let ext = ctx.glsl();

    for k in 0..dimension {
        let a_varying_bits = if dimension == 4 {
            (k >> 1) & 1
        } else {
            ((k & 4) << 1) | ((k >> 1) & 1)
        };
        let b_varying_bits = if dimension == 4 {
            k << 1
        } else {
            ((k & 3) << 1) | ((k & 4) << 2)
        };
        let a_owner_local =
            bitwise_with_const(ctx, &mut out, Op::BitwiseOr, uint, a_key, a_varying_bits);
        let b_owner_local =
            bitwise_with_const(ctx, &mut out, Op::BitwiseOr, uint, b_key, b_varying_bits);
        let a_owner = binary_value(ctx, &mut out, Op::IAdd, uint, simd_base, a_owner_local);
        let b_owner = binary_value(ctx, &mut out, Op::IAdd, uint, simd_base, b_owner_local);
        let a = subgroup_shuffle(
            ctx,
            &mut out,
            elem,
            scope,
            a_components[(k & 1) as usize],
            a_owner,
        );
        for component in 0..2 {
            let b = subgroup_shuffle(ctx, &mut out, elem, scope, b_components[component], b_owner);
            let next = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::ExtInst,
                Some(elem),
                Some(next),
                vec![
                    Operand::IdRef(ext),
                    Operand::LiteralExtInstInteger(GLSLstd450::Fma as u32),
                    Operand::IdRef(a),
                    Operand::IdRef(b),
                    Operand::IdRef(accumulators[component]),
                ],
            ));
            accumulators[component] = next;
        }
    }
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(rty),
        Some(res),
        accumulators.into_iter().map(Operand::IdRef).collect(),
    ));
    Ok(out)
}

#[derive(Clone, Copy)]
enum Matrix16Transpose {
    Constant(bool),
    Dynamic(Word),
}

#[derive(Clone, Copy)]
struct Matrix16Tile {
    components: [Word; 8],
    elem: Word,
    scope: Word,
    simd_base: Word,
}

#[derive(Clone, Copy)]
struct Matrix16OutputMapping {
    row_bits: Word,
    row_as_col: Word,
    col_bits: Word,
    col_as_row: Word,
    simd_lane: Word,
}

pub(in crate::passes) fn lower_simdgroup_matrix_16x16_mac(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    let Some(signature) = matrix16_intrinsic(name) else {
        return Err(format!(
            "{name} has an unsupported 16x16x16 matrix ABI signature"
        ));
    };
    lower_matrix16_mac(ctx, name, signature, res, rty, args, None)
}

pub(in crate::passes) fn lower_agx3_igemm_16x16_mac(
    ctx: &mut Ctx,
    name: &str,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    const ABI_CONSTANTS: [(usize, u32); 7] =
        [(0, 16), (1, 16), (2, 16), (3, 9), (5, 75), (7, 75), (9, 9)];
    if name != "llvm.agx3.igemm.v8i32.i64.i64.v8i32" {
        return Err(format!("{name} has an unsupported AGX3 integer matrix ABI"));
    }
    if args.len() != 10 {
        return Err(format!("{name} expects 10 operands, got {}", args.len()));
    }
    for (ordinal, expected) in ABI_CONSTANTS {
        let actual = constant_u32(ctx, args[ordinal]);
        if actual != Some(expected) {
            return Err(format!(
                "{name} operand {ordinal} must be the ABI constant {expected}, got {actual:?}"
            ));
        }
    }
    let mut packed_types = [0; 2];
    for (index, ordinal) in [4, 6].into_iter().enumerate() {
        let ty = value_result_type(ctx, args[ordinal])
            .ok_or_else(|| format!("{name} packed operand {ordinal} has no type"))?;
        if !is_int_scalar_width(ctx, ty, 64) {
            return Err(format!("{name} packed operand {ordinal} is not i64"));
        }
        packed_types[index] = ty;
    }

    let byte = ctx.ty_int8();
    let fragment = ctx.ty_array(byte, 8);
    let bool_ty = ctx.ty_bool();
    let not_transposed = ctx.const_bool_of(bool_ty, false);
    let mut unpack = Vec::with_capacity(33);
    let a =
        unpack_i64_matrix16_fragment(ctx, &mut unpack, fragment, byte, packed_types[0], args[4]);
    let b =
        unpack_i64_matrix16_fragment(ctx, &mut unpack, fragment, byte, packed_types[1], args[6]);
    let matrix_args = [a, not_transposed, b, not_transposed, args[8]];
    unpack.extend(lower_matrix16_mac(
        ctx,
        name,
        Matrix16Intrinsic {
            lhs: Matrix16Element::I8 { signed: true },
            rhs: Matrix16Element::I8 { signed: true },
            integer: true,
        },
        res,
        rty,
        &matrix_args,
        Some((byte, byte)),
    )?);
    Ok(unpack)
}

fn unpack_i64_matrix16_fragment(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    fragment_ty: Word,
    byte_ty: Word,
    packed_ty: Word,
    packed: Word,
) -> Word {
    let mut bytes = Vec::with_capacity(8);
    for index in 0..8 {
        let shifted = if index == 0 {
            packed
        } else {
            let shifted = ctx.module.fresh_id();
            let shift = ctx.const_int_of(packed_ty, i64::from(index * 8));
            out.push(Instruction::new(
                Op::ShiftRightLogical,
                Some(packed_ty),
                Some(shifted),
                vec![Operand::IdRef(packed), Operand::IdRef(shift)],
            ));
            shifted
        };
        let byte = ctx.module.fresh_id();
        out.push(Instruction::new(
            Op::UConvert,
            Some(byte_ty),
            Some(byte),
            vec![Operand::IdRef(shifted)],
        ));
        bytes.push(Operand::IdRef(byte));
    }
    let fragment = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(fragment_ty),
        Some(fragment),
        bytes,
    ));
    fragment
}

fn lower_matrix16_mac(
    ctx: &mut Ctx,
    name: &str,
    signature: Matrix16Intrinsic,
    res: Word,
    rty: Word,
    args: &[Word],
    known_fragment_elems: Option<(Word, Word)>,
) -> Result<Vec<Instruction>, String> {
    let a_kind = signature.lhs;
    let b_kind = signature.rhs;
    let integer = signature.integer;
    if args.len() != 5 {
        return Err(format!("{name} expects 5 operands, got {}", args.len()));
    }
    let (result_elem, result_lanes) = composite_shape(ctx, rty)
        .ok_or_else(|| format!("{name} result is not an eight-lane composite"))?;
    let result_ok = if integer {
        is_int_scalar_width(ctx, result_elem, 32)
    } else {
        is_f32_scalar(ctx, result_elem)
    };
    if result_lanes != 8 || !result_ok {
        return Err(format!(
            "{name} result must be {}",
            if integer { "v8i32" } else { "v8f32" }
        ));
    }
    let (a_ty, b_ty) = match known_fragment_elems {
        Some(types) => types,
        None => (
            validate_matrix16_fragment(ctx, name, args[0], a_kind, 0)?,
            validate_matrix16_fragment(ctx, name, args[2], b_kind, 2)?,
        ),
    };
    let c_ty = value_result_type(ctx, args[4])
        .ok_or_else(|| format!("{name} accumulator has no result type"))?;
    if c_ty != rty {
        return Err(format!(
            "{name} accumulator type does not match its result type"
        ));
    }
    for (ordinal, flag) in [(1, args[1]), (3, args[3])] {
        let ty = value_result_type(ctx, flag)
            .ok_or_else(|| format!("{name} transpose operand {ordinal} has no type"))?;
        if !is_bool_type(ctx, ty) {
            return Err(format!("{name} transpose operand {ordinal} is not i1"));
        }
    }
    let transpose_a = matrix16_transpose(ctx, args[1]);
    let transpose_b = matrix16_transpose(ctx, args[3]);

    let mut out = Vec::with_capacity(640);
    let uint = ctx.ty_uint();
    let scope = ctx.const_uint(Scope::Subgroup as u32);
    let subgroup_lane = subgroup_lane_index_u32(ctx, &mut out);
    let simd_lane = metal_simd_lane_local_u32(ctx, subgroup_lane, &mut out);
    let simd_base = metal_simd_lane_base_u32(ctx, subgroup_lane, simd_lane, &mut out);
    let row_bits = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, simd_lane, 0x16);
    let col_bits = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, simd_lane, 0x09);

    let row_as_col_lo = shift_with_const(ctx, &mut out, Op::ShiftRightLogical, simd_lane, 2);
    let row_as_col_lo = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, row_as_col_lo, 1);
    let row_as_col_hi = shift_with_const(ctx, &mut out, Op::ShiftRightLogical, simd_lane, 1);
    let row_as_col_hi = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, row_as_col_hi, 8);
    let row_as_col = binary_value(
        ctx,
        &mut out,
        Op::BitwiseOr,
        uint,
        row_as_col_lo,
        row_as_col_hi,
    );
    let col_as_row_lo = shift_with_const(ctx, &mut out, Op::ShiftLeftLogical, simd_lane, 2);
    let col_as_row_lo = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, col_as_row_lo, 4);
    let col_as_row_hi = shift_with_const(ctx, &mut out, Op::ShiftLeftLogical, simd_lane, 1);
    let col_as_row_hi = bitwise_with_const(ctx, &mut out, Op::BitwiseAnd, uint, col_as_row_hi, 16);
    let col_as_row = binary_value(
        ctx,
        &mut out,
        Op::BitwiseOr,
        uint,
        col_as_row_lo,
        col_as_row_hi,
    );

    let a_components = extract_matrix16_components(ctx, &mut out, a_ty, args[0]);
    let a_components =
        convert_matrix16_components(ctx, &mut out, a_components, a_kind, result_elem);
    let b_components = extract_matrix16_components(ctx, &mut out, b_ty, args[2]);
    let b_components =
        convert_matrix16_components(ctx, &mut out, b_components, b_kind, result_elem);
    let c_components = extract_matrix16_components(ctx, &mut out, result_elem, args[4]);
    let a_tile = Matrix16Tile {
        components: a_components,
        elem: result_elem,
        scope,
        simd_base,
    };
    let b_tile = Matrix16Tile {
        components: b_components,
        elem: result_elem,
        scope,
        simd_base,
    };
    let mapping = Matrix16OutputMapping {
        row_bits,
        row_as_col,
        col_bits,
        col_as_row,
        simd_lane,
    };
    let mut results = Vec::with_capacity(8);
    for component in 0..8u32 {
        let row_offset = component / 4;
        let col_offset = component % 4;
        let mut acc = c_components[component as usize];
        for k in 0..16u32 {
            let a = matrix16_a_cell(ctx, &mut out, &a_tile, &mapping, row_offset, k, transpose_a);
            let b = matrix16_b_cell(ctx, &mut out, &b_tile, &mapping, col_offset, k, transpose_b);
            if integer {
                let product = binary_value(ctx, &mut out, Op::IMul, result_elem, a, b);
                acc = binary_value(ctx, &mut out, Op::IAdd, result_elem, acc, product);
            } else {
                let next = ctx.module.fresh_id();
                let ext = ctx.glsl();
                out.push(Instruction::new(
                    Op::ExtInst,
                    Some(result_elem),
                    Some(next),
                    vec![
                        Operand::IdRef(ext),
                        Operand::LiteralExtInstInteger(GLSLstd450::Fma as u32),
                        Operand::IdRef(a),
                        Operand::IdRef(b),
                        Operand::IdRef(acc),
                    ],
                ));
                acc = next;
            }
        }
        results.push(Operand::IdRef(acc));
    }
    out.push(Instruction::new(
        Op::CompositeConstruct,
        Some(rty),
        Some(res),
        results,
    ));
    Ok(out)
}

fn is_int_scalar_width(ctx: &Ctx, ty: Word, width: u32) -> bool {
    type_def_of(ctx, ty)
        .map(|def| {
            def.class.opcode == Op::TypeInt
                && def.operands.first() == Some(&Operand::LiteralBit32(width))
        })
        .unwrap_or(false)
}

fn validate_matrix16_fragment(
    ctx: &Ctx,
    name: &str,
    value: Word,
    kind: Matrix16Element,
    ordinal: usize,
) -> Result<Word, String> {
    let ty = value_result_type(ctx, value)
        .ok_or_else(|| format!("{name} operand {ordinal} has no type"))?;
    let (elem, lanes) = composite_shape(ctx, ty)
        .ok_or_else(|| format!("{name} operand {ordinal} is not an eight-lane composite"))?;
    let valid = match kind {
        Matrix16Element::F32 => is_f32_scalar(ctx, elem),
        Matrix16Element::F16 => is_half_scalar(ctx, elem),
        Matrix16Element::Bf16 => is_int_scalar_width(ctx, elem, 16),
        Matrix16Element::F8E4M3
        | Matrix16Element::F8E4M3Fn
        | Matrix16Element::F8E5M2
        | Matrix16Element::I8 { .. } => is_int_scalar_width(ctx, elem, 8),
    };
    if lanes != 8 || !valid {
        return Err(format!(
            "{name} operand {ordinal} does not match its ABI element type"
        ));
    }
    Ok(elem)
}

fn matrix16_transpose(ctx: &Ctx, flag: Word) -> Matrix16Transpose {
    match const_bool_value(ctx, flag) {
        Some(value) => Matrix16Transpose::Constant(value),
        None => Matrix16Transpose::Dynamic(flag),
    }
}

fn extract_matrix16_components(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    elem: Word,
    vector: Word,
) -> [Word; 8] {
    std::array::from_fn(|index| composite_extract(ctx, out, elem, vector, index as u32))
}

fn convert_matrix16_components(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    components: [Word; 8],
    kind: Matrix16Element,
    accumulator_ty: Word,
) -> [Word; 8] {
    components.map(|value| matrix16_to_accumulator(ctx, out, value, kind, accumulator_ty))
}

fn matrix16_a_cell(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    tile: &Matrix16Tile,
    mapping: &Matrix16OutputMapping,
    row_offset: u32,
    k: u32,
    transpose: Matrix16Transpose,
) -> Word {
    let normal_owner = owner_with_bits(
        ctx,
        out,
        tile.simd_base,
        mapping.row_bits,
        encode_matrix16_col(k),
    );
    let normal = matrix16_shuffle_component(
        ctx,
        out,
        tile.components[(row_offset * 4 + k % 4) as usize],
        tile.elem,
        tile.scope,
        normal_owner,
    );
    if matches!(transpose, Matrix16Transpose::Constant(false)) {
        return normal;
    }
    let transposed_owner = owner_with_bits(
        ctx,
        out,
        tile.simd_base,
        mapping.row_as_col,
        encode_matrix16_row(k),
    );
    let uint = ctx.ty_uint();
    let row_low = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, mapping.simd_lane, 2);
    let component_offset = ctx.const_uint((k % 2) * 4 + row_offset);
    let component = binary_value(ctx, out, Op::IAdd, uint, row_low, component_offset);
    let transposed =
        matrix16_shuffle_dynamic_component(ctx, out, tile, transposed_owner, component);
    matrix16_select_transpose(ctx, out, tile.elem, transpose, normal, transposed)
}

fn matrix16_b_cell(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    tile: &Matrix16Tile,
    mapping: &Matrix16OutputMapping,
    col_offset: u32,
    k: u32,
    transpose: Matrix16Transpose,
) -> Word {
    let normal_owner = owner_with_bits(
        ctx,
        out,
        tile.simd_base,
        mapping.col_bits,
        encode_matrix16_row(k),
    );
    let normal = matrix16_shuffle_component(
        ctx,
        out,
        tile.components[((k % 2) * 4 + col_offset) as usize],
        tile.elem,
        tile.scope,
        normal_owner,
    );
    if matches!(transpose, Matrix16Transpose::Constant(false)) {
        return normal;
    }
    let row_offset_bit = (col_offset / 2) << 1;
    let transposed_owner = owner_with_bits(
        ctx,
        out,
        tile.simd_base,
        mapping.col_as_row,
        encode_matrix16_col(k) | row_offset_bit,
    );
    let transposed = matrix16_shuffle_component(
        ctx,
        out,
        tile.components[((col_offset % 2) * 4 + k % 4) as usize],
        tile.elem,
        tile.scope,
        transposed_owner,
    );
    matrix16_select_transpose(ctx, out, tile.elem, transpose, normal, transposed)
}

fn encode_matrix16_row(row: u32) -> u32 {
    let group = row / 2;
    ((group & 1) << 1) | ((group & 2) << 1) | ((group & 4) << 2)
}

fn encode_matrix16_col(col: u32) -> u32 {
    let group = col / 4;
    (group & 1) | ((group & 2) << 2)
}

fn owner_with_bits(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    simd_base: Word,
    varying: Word,
    fixed: u32,
) -> Word {
    let uint = ctx.ty_uint();
    let local = bitwise_with_const(ctx, out, Op::BitwiseOr, uint, varying, fixed);
    binary_value(ctx, out, Op::IAdd, uint, simd_base, local)
}

fn matrix16_shuffle_component(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    component: Word,
    elem: Word,
    scope: Word,
    owner: Word,
) -> Word {
    subgroup_shuffle(ctx, out, elem, scope, component, owner)
}

fn matrix16_shuffle_dynamic_component(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    tile: &Matrix16Tile,
    owner: Word,
    component: Word,
) -> Word {
    let shuffled: Vec<_> = tile
        .components
        .iter()
        .map(|value| subgroup_shuffle(ctx, out, tile.elem, tile.scope, *value, owner))
        .collect();
    let bool_ty = ctx.ty_bool();
    let mut result = shuffled[0];
    for index in 1..8u32 {
        let index_value = ctx.const_uint(index);
        let matches = binary_value(ctx, out, Op::IEqual, bool_ty, component, index_value);
        result = select_value(
            ctx,
            out,
            tile.elem,
            matches,
            shuffled[index as usize],
            result,
        );
    }
    result
}

fn matrix16_select_transpose(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    elem: Word,
    transpose: Matrix16Transpose,
    normal: Word,
    transposed: Word,
) -> Word {
    match transpose {
        Matrix16Transpose::Constant(true) => transposed,
        Matrix16Transpose::Constant(false) => normal,
        Matrix16Transpose::Dynamic(condition) => {
            let result = ctx.module.fresh_id();
            out.push(Instruction::new(
                Op::Select,
                Some(elem),
                Some(result),
                vec![
                    Operand::IdRef(condition),
                    Operand::IdRef(transposed),
                    Operand::IdRef(normal),
                ],
            ));
            result
        }
    }
}

fn matrix16_to_accumulator(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    kind: Matrix16Element,
    accumulator_ty: Word,
) -> Word {
    match kind {
        Matrix16Element::F32 => value,
        Matrix16Element::F16 => {
            let half = ctx.ty_half();
            convert_matrix_lane(ctx, out, value, half, accumulator_ty)
        }
        Matrix16Element::Bf16 => widen_bf16_to_f32(ctx, out, value, 1),
        Matrix16Element::F8E4M3 => matrix16_float8_to_f32(ctx, out, value, 4, 3, false),
        Matrix16Element::F8E4M3Fn => matrix16_float8_to_f32(ctx, out, value, 4, 3, true),
        Matrix16Element::F8E5M2 => matrix16_float8_to_f32(ctx, out, value, 5, 2, false),
        Matrix16Element::I8 { signed } => {
            let result = ctx.module.fresh_id();
            out.push(Instruction::new(
                if signed { Op::SConvert } else { Op::UConvert },
                Some(accumulator_ty),
                Some(result),
                vec![Operand::IdRef(value)],
            ));
            result
        }
    }
}

fn matrix16_float8_to_f32(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    value: Word,
    exponent_bits: u32,
    mantissa_bits: u32,
    finite_only: bool,
) -> Word {
    let uint = ctx.ty_uint();
    let int = ctx.ty_sint();
    let float = ctx.ty_float();
    let bool_ty = ctx.ty_bool();
    let bits = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::UConvert,
        Some(uint),
        Some(bits),
        vec![Operand::IdRef(value)],
    ));
    let mantissa_mask = (1u32 << mantissa_bits) - 1;
    let exponent_mask = (1u32 << exponent_bits) - 1;
    let mantissa = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, bits, mantissa_mask);
    let shifted_exp = shift_with_const(ctx, out, Op::ShiftRightLogical, bits, mantissa_bits);
    let exponent = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, shifted_exp, exponent_mask);
    let zero = ctx.const_uint(0);
    let exp_zero = binary_value(ctx, out, Op::IEqual, bool_ty, exponent, zero);
    let implicit_bit = ctx.const_uint(1 << mantissa_bits);
    let normal_significand = binary_value(ctx, out, Op::IAdd, uint, mantissa, implicit_bit);
    let significand = select_value(ctx, out, uint, exp_zero, mantissa, normal_significand);
    let significand_f = unary_value(ctx, out, Op::ConvertUToF, float, significand);
    let exponent_i = unary_value(ctx, out, Op::Bitcast, int, exponent);
    let bias = (1i32 << (exponent_bits - 1)) - 1;
    let normal_scale_offset = ctx.const_int_of(int, -(bias + mantissa_bits as i32) as i64);
    let normal_scale = binary_value(ctx, out, Op::IAdd, int, exponent_i, normal_scale_offset);
    let subnormal_scale = ctx.const_int_of(int, (1 - bias - mantissa_bits as i32) as i64);
    let scale = select_value(ctx, out, int, exp_zero, subnormal_scale, normal_scale);
    let magnitude = ctx.module.fresh_id();
    let ext = ctx.glsl();
    out.push(Instruction::new(
        Op::ExtInst,
        Some(float),
        Some(magnitude),
        vec![
            Operand::IdRef(ext),
            Operand::LiteralExtInstInteger(GLSLstd450::Ldexp as u32),
            Operand::IdRef(significand_f),
            Operand::IdRef(scale),
        ],
    ));
    let sign = bitwise_with_const(ctx, out, Op::BitwiseAnd, uint, bits, 0x80);
    let negative = binary_value(ctx, out, Op::INotEqual, bool_ty, sign, zero);
    let negated = unary_value(ctx, out, Op::FNegate, float, magnitude);
    let signed = select_value(ctx, out, float, negative, negated, magnitude);

    let exponent_mask_value = ctx.const_uint(exponent_mask);
    let exp_all_ones = binary_value(ctx, out, Op::IEqual, bool_ty, exponent, exponent_mask_value);
    let mantissa_mask_value = ctx.const_uint(mantissa_mask);
    let mant_all_ones = binary_value(ctx, out, Op::IEqual, bool_ty, mantissa, mantissa_mask_value);
    if finite_only {
        let is_nan = binary_value(
            ctx,
            out,
            Op::LogicalAnd,
            bool_ty,
            exp_all_ones,
            mant_all_ones,
        );
        let nan = ctx.const_float(f32::NAN);
        return select_value(ctx, out, float, is_nan, nan, signed);
    }
    let mant_zero = binary_value(ctx, out, Op::IEqual, bool_ty, mantissa, zero);
    let infinity = ctx.const_float(f32::INFINITY);
    let negative_infinity = ctx.const_float(f32::NEG_INFINITY);
    let signed_infinity = select_value(ctx, out, float, negative, negative_infinity, infinity);
    let nan = ctx.const_float(f32::NAN);
    let special = select_value(ctx, out, float, mant_zero, signed_infinity, nan);
    select_value(ctx, out, float, exp_all_ones, special, signed)
}

fn shift_with_const(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    op: Op,
    value: Word,
    amount: u32,
) -> Word {
    let uint = ctx.ty_uint();
    let amount = ctx.const_uint(amount);
    binary_value(ctx, out, op, uint, value, amount)
}

fn unary_value(ctx: &mut Ctx, out: &mut Vec<Instruction>, op: Op, ty: Word, value: Word) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        op,
        Some(ty),
        Some(result),
        vec![Operand::IdRef(value)],
    ));
    result
}

fn select_value(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    ty: Word,
    condition: Word,
    when_true: Word,
    when_false: Word,
) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::Select,
        Some(ty),
        Some(result),
        vec![
            Operand::IdRef(condition),
            Operand::IdRef(when_true),
            Operand::IdRef(when_false),
        ],
    ));
    result
}

fn bitwise_with_const(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    op: Op,
    ty: Word,
    lhs: Word,
    rhs: u32,
) -> Word {
    let rhs = ctx.const_uint(rhs);
    binary_value(ctx, out, op, ty, lhs, rhs)
}

fn binary_value(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    op: Op,
    ty: Word,
    lhs: Word,
    rhs: Word,
) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        op,
        Some(ty),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn subgroup_shuffle(
    ctx: &mut Ctx,
    out: &mut Vec<Instruction>,
    ty: Word,
    scope: Word,
    value: Word,
    lane: Word,
) -> Word {
    let result = ctx.module.fresh_id();
    out.push(Instruction::new(
        Op::GroupNonUniformShuffle,
        Some(ty),
        Some(result),
        vec![
            Operand::IdScope(scope),
            Operand::IdRef(value),
            Operand::IdRef(lane),
        ],
    ));
    result
}

pub(in crate::passes) fn lower_simdgroup_matrix_8x8_init_diag(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() != 1 {
        return Err(format!(
            "air.simdgroup_matrix_8x8_init_diag expects 1 operand, got {}",
            args.len()
        ));
    }
    let (elem, lanes) = composite_shape(ctx, rty).ok_or_else(|| {
        "air.simdgroup_matrix init_diag result is not a 64-lane composite".to_string()
    })?;
    if lanes != 64 {
        return Err("air.simdgroup_matrix init_diag result is not 64-lane".to_string());
    }
    let zero = if is_half_scalar(ctx, elem) {
        ctx.const_half(0.0)
    } else if is_f32_scalar(ctx, elem) {
        ctx.const_float(0.0)
    } else {
        ctx.const_int_of(elem, 0)
    };
    let mut insts = Vec::with_capacity(24);
    let bool_ty = ctx.ty_bool();
    let lane = matrix8_lane(ctx, &mut insts);
    let mut slots = [0 as Word; 2];
    for slot in 0..2u32 {
        let col = matrix8_slot_col(ctx, &mut insts, &lane, slot);
        let on_diag = binary_value(ctx, &mut insts, Op::IEqual, bool_ty, lane.row, col);
        slots[slot as usize] = select_value(ctx, &mut insts, elem, on_diag, args[0], zero);
    }
    matrix8_pack(ctx, &mut insts, elem, rty, res, slots);
    Ok(insts)
}

enum MatrixBlockBase {
    Element,
    Block(Word),
}

fn simdgroup_matrix_block_base(
    ctx: &mut Ctx,
    ptr_ty: Word,
    elem: Word,
    what: &str,
    whose: &str,
) -> Result<MatrixBlockBase, String> {
    let pointee = pointer_pointee_type(ctx, ptr_ty)
        .ok_or_else(|| format!("air.simdgroup_matrix {what} pointer is not a pointer"))?;
    if pointee == elem {
        return Ok(MatrixBlockBase::Element);
    }
    let is_block_of_elem = type_def_of(ctx, pointee)
        .map(|def| {
            matches!(def.class.opcode, Op::TypeArray | Op::TypeRuntimeArray)
                && def.operands.first() == Some(&Operand::IdRef(elem))
        })
        .unwrap_or(false);
    if is_block_of_elem {
        if let Some(storage) = pointer_storage_class(ctx, ptr_ty) {
            return Ok(MatrixBlockBase::Block(ctx.ty_ptr(storage, elem)));
        }
    }
    let got = type_def_of(ctx, pointee)
        .map(|def| format!("{:?} {:?}", def.class.opcode, def.operands))
        .unwrap_or_else(|| "undefined".to_string());
    let want = type_def_of(ctx, elem)
        .map(|def| format!("{:?} {:?}", def.class.opcode, def.operands))
        .unwrap_or_else(|| "undefined".to_string());
    Err(format!(
        "air.simdgroup_matrix {what} pointee does not match {whose} element type (pointee %{pointee} = {got}; element %{elem} = {want})"
    ))
}

fn pointer_storage_class(ctx: &Ctx, ptr_ty: Word) -> Option<StorageClass> {
    let def = type_def_of(ctx, ptr_ty)?;
    if def.class.opcode != Op::TypePointer {
        return None;
    }
    match def.operands.first() {
        Some(Operand::StorageClass(storage)) => Some(*storage),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Term {
    Known(i64),
    Value(Word),
}

impl Term {
    fn id(self, ctx: &mut Ctx, idx_ty: Word) -> Word {
        match self {
            Term::Known(value) => ctx.const_int_of(idx_ty, value),
            Term::Value(id) => id,
        }
    }
}

struct MatrixBlockAddress {
    idx_ty: Word,
    col_stride: Term,
    row_stride: Term,
    col_origin: Term,
    row_origin: Term,
}

impl MatrixBlockAddress {
    fn axis(
        ctx: &mut Ctx,
        insts: &mut Vec<Instruction>,
        idx_ty: Word,
        coord: Word,
        origin: Term,
        stride: Term,
    ) -> Term {
        let uint = ctx.ty_uint();
        let coord = if idx_ty == uint {
            coord
        } else {
            let op = if is_int_scalar_width(ctx, idx_ty, 32) {
                Op::Bitcast
            } else {
                Op::SConvert
            };
            let widened = ctx.module.fresh_id();
            insts.push(Instruction::new(
                op,
                Some(idx_ty),
                Some(widened),
                vec![Operand::IdRef(coord)],
            ));
            widened
        };
        let offset = match origin {
            Term::Known(0) => coord,
            Term::Known(value) => {
                let origin = ctx.const_int_of(idx_ty, value);
                binary(ctx, insts, Op::IAdd, idx_ty, coord, origin)
            }
            Term::Value(id) => binary(ctx, insts, Op::IAdd, idx_ty, coord, id),
        };
        match stride {
            Term::Known(0) => Term::Known(0),
            Term::Known(1) => Term::Value(offset),
            stride => {
                let stride = stride.id(ctx, idx_ty);
                Term::Value(binary(ctx, insts, Op::IMul, idx_ty, offset, stride))
            }
        }
    }

    fn index(&self, ctx: &mut Ctx, insts: &mut Vec<Instruction>, row: Word, col: Word) -> Word {
        let down = Self::axis(
            ctx,
            insts,
            self.idx_ty,
            row,
            self.row_origin,
            self.row_stride,
        );
        let across = Self::axis(
            ctx,
            insts,
            self.idx_ty,
            col,
            self.col_origin,
            self.col_stride,
        );
        match (down, across) {
            (Term::Known(a), Term::Known(b)) => ctx.const_int_of(self.idx_ty, a + b),
            (Term::Known(0), other) | (other, Term::Known(0)) => other.id(ctx, self.idx_ty),
            (down, across) => {
                let (a, b) = (down.id(ctx, self.idx_ty), across.id(ctx, self.idx_ty));
                binary(ctx, insts, Op::IAdd, self.idx_ty, a, b)
            }
        }
    }
}

fn binary(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    op: Op,
    ty: Word,
    lhs: Word,
    rhs: Word,
) -> Word {
    let result = ctx.module.fresh_id();
    insts.push(Instruction::new(
        op,
        Some(ty),
        Some(result),
        vec![Operand::IdRef(lhs), Operand::IdRef(rhs)],
    ));
    result
}

fn simdgroup_matrix_elem_ptr(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    ptr_ty: Word,
    shape: &MatrixBlockBase,
    address: &MatrixBlockAddress,
    base: Word,
    row: Word,
    col: Word,
) -> Word {
    let index = address.index(ctx, insts, row, col);
    let (op, result_ty) = match shape {
        MatrixBlockBase::Element => (Op::PtrAccessChain, ptr_ty),
        MatrixBlockBase::Block(elem_ptr_ty) => (Op::AccessChain, *elem_ptr_ty),
    };
    let elem_ptr = ctx.module.fresh_id();
    insts.push(Instruction::new(
        op,
        Some(result_ty),
        Some(elem_ptr),
        vec![Operand::IdRef(base), Operand::IdRef(index)],
    ));
    elem_ptr
}

fn descriptor_component(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    elem_ty: Word,
    desc_vec: Word,
    lane: u32,
) -> Term {
    if let Some(value) = constant_vector_component(ctx, desc_vec, lane) {
        return Term::Known(value);
    }
    Term::Value(composite_extract(ctx, insts, elem_ty, desc_vec, lane))
}

fn constant_vector_component(ctx: &Ctx, vector: Word, lane: u32) -> Option<i64> {
    let def = value_def_instruction(ctx, vector)?;
    match def.class.opcode {
        Op::ConstantNull => Some(0),
        Op::ConstantComposite => match def.operands.get(lane as usize)? {
            Operand::IdRef(component) => constant_scalar_value(ctx, *component),
            _ => None,
        },
        _ => None,
    }
}

fn constant_scalar_value(ctx: &Ctx, scalar: Word) -> Option<i64> {
    let def = value_def_instruction(ctx, scalar)?;
    match def.class.opcode {
        Op::ConstantNull => Some(0),
        Op::Constant => match def.operands.first()? {
            Operand::LiteralBit32(value) => Some(i64::from(*value)),
            Operand::LiteralBit64(value) => i64::try_from(*value).ok(),
            _ => None,
        },
        _ => None,
    }
}

fn simdgroup_matrix_address(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    strides: Word,
    origin: Option<Word>,
) -> Result<MatrixBlockAddress, String> {
    let vec_ty = value_result_type(ctx, strides)
        .ok_or_else(|| "simdgroup_matrix descriptor vector has no type".to_string())?;
    let (idx_ty, lanes) = composite_shape(ctx, vec_ty)
        .ok_or_else(|| "simdgroup_matrix descriptor is not a vector".to_string())?;
    if lanes < 2 {
        return Err("simdgroup_matrix stride descriptor is not two-component".to_string());
    }
    let col_stride = descriptor_component(ctx, insts, idx_ty, strides, 0);
    let row_stride = descriptor_component(ctx, insts, idx_ty, strides, 1);
    let (col_origin, row_origin) = match origin {
        Some(origin) => (
            descriptor_component(ctx, insts, idx_ty, origin, 0),
            descriptor_component(ctx, insts, idx_ty, origin, 1),
        ),
        None => (Term::Known(0), Term::Known(0)),
    };
    Ok(MatrixBlockAddress {
        idx_ty,
        col_stride,
        row_stride,
        col_origin,
        row_origin,
    })
}

fn constant_bool_value(ctx: &Ctx, scalar: Word) -> Option<bool> {
    let def = value_def_instruction(ctx, scalar)?;
    match def.class.opcode {
        Op::ConstantTrue => Some(true),
        Op::ConstantFalse | Op::ConstantNull => Some(false),
        _ => None,
    }
}

fn select_term(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    idx_ty: Word,
    cond: Word,
    a: Term,
    b: Term,
) -> Term {
    let (a, b) = (a.id(ctx, idx_ty), b.id(ctx, idx_ty));
    let id = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::Select,
        Some(idx_ty),
        Some(id),
        vec![Operand::IdRef(cond), Operand::IdRef(a), Operand::IdRef(b)],
    ));
    Term::Value(id)
}

fn simdgroup_matrix_address_classic(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    elements_per_row: Word,
    origin: Word,
    transpose: Word,
) -> Result<MatrixBlockAddress, String> {
    let vec_ty = value_result_type(ctx, origin)
        .ok_or_else(|| "simdgroup_matrix origin vector has no type".to_string())?;
    let (idx_ty, lanes) = composite_shape(ctx, vec_ty)
        .ok_or_else(|| "simdgroup_matrix origin is not a vector".to_string())?;
    if lanes < 2 {
        return Err("simdgroup_matrix origin is not two-component".to_string());
    }
    let epr_ty = value_result_type(ctx, elements_per_row)
        .ok_or_else(|| "simdgroup_matrix elements_per_row has no type".to_string())?;
    if epr_ty != idx_ty {
        return Err(
            "simdgroup_matrix elements_per_row is not the origin's integer type".to_string(),
        );
    }
    let row_stride = match constant_scalar_value(ctx, elements_per_row) {
        Some(value) => Term::Known(value),
        None => Term::Value(elements_per_row),
    };
    let col_stride = Term::Known(1);
    let col_origin = descriptor_component(ctx, insts, idx_ty, origin, 0);
    let row_origin = descriptor_component(ctx, insts, idx_ty, origin, 1);
    Ok(match constant_bool_value(ctx, transpose) {
        Some(false) => MatrixBlockAddress {
            idx_ty,
            col_stride,
            row_stride,
            col_origin,
            row_origin,
        },
        Some(true) => MatrixBlockAddress {
            idx_ty,
            col_stride: row_stride,
            row_stride: col_stride,
            col_origin: row_origin,
            row_origin: col_origin,
        },
        None => MatrixBlockAddress {
            idx_ty,
            col_stride: select_term(ctx, insts, idx_ty, transpose, row_stride, col_stride),
            row_stride: select_term(ctx, insts, idx_ty, transpose, col_stride, row_stride),
            col_origin: select_term(ctx, insts, idx_ty, transpose, row_origin, col_origin),
            row_origin: select_term(ctx, insts, idx_ty, transpose, col_origin, row_origin),
        },
    })
}

fn simdgroup_matrix_address_for_call(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    trailing: &[Word],
    what: &str,
) -> Result<MatrixBlockAddress, String> {
    let first = *trailing
        .first()
        .ok_or_else(|| format!("air.simdgroup_matrix_8x8_{what} has no descriptor operands"))?;
    let first_ty = value_result_type(ctx, first)
        .ok_or_else(|| format!("air.simdgroup_matrix_8x8_{what} descriptor operand has no type"))?;
    if composite_shape(ctx, first_ty).is_some() {
        let strides = *trailing.get(1).ok_or_else(|| {
            format!("air.simdgroup_matrix_8x8_{what} descriptor ABI needs a stride vector")
        })?;
        return simdgroup_matrix_address(ctx, insts, strides, trailing.get(2).copied());
    }
    match trailing {
        [elements_per_row, origin, transpose, ..] => {
            simdgroup_matrix_address_classic(ctx, insts, *elements_per_row, *origin, *transpose)
        }
        _ => Err(format!(
            "air.simdgroup_matrix_8x8_{what} classic ABI expects (i64 elements_per_row, <2 x i64> origin, \
             i1 transpose), got {} trailing operands",
            trailing.len()
        )),
    }
}

pub(in crate::passes) fn lower_simdgroup_matrix_8x8_load(
    ctx: &mut Ctx,
    res: Word,
    rty: Word,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() < 3 {
        return Err(format!(
            "air.simdgroup_matrix_8x8_load expects >=3 operands, got {}",
            args.len()
        ));
    }
    let (elem, lanes) = composite_shape(ctx, rty)
        .ok_or_else(|| "air.simdgroup_matrix load result is not a 64-lane composite".to_string())?;
    if lanes != 64 {
        return Err("air.simdgroup_matrix load result is not 64-lane".to_string());
    }
    let base = args[0];
    let ptr_ty = value_result_type(ctx, base)
        .ok_or_else(|| "air.simdgroup_matrix load pointer has no type".to_string())?;
    let shape = simdgroup_matrix_block_base(ctx, ptr_ty, elem, "load", "result")?;
    let mut insts = Vec::new();
    let address = simdgroup_matrix_address_for_call(ctx, &mut insts, &args[1..], "load")?;
    let lane = matrix8_lane(ctx, &mut insts);
    let mut slots = [0 as Word; 2];
    for slot in 0..2u32 {
        let col = matrix8_slot_col(ctx, &mut insts, &lane, slot);
        let elem_ptr = simdgroup_matrix_elem_ptr(
            ctx, &mut insts, ptr_ty, &shape, &address, base, lane.row, col,
        );
        let value = ctx.module.fresh_id();
        insts.push(Instruction::new(
            Op::Load,
            Some(elem),
            Some(value),
            vec![Operand::IdRef(elem_ptr)],
        ));
        slots[slot as usize] = value;
    }
    if elem == ctx.ty_half() && std::env::var_os("NVMTL_MATRIX8_NO_DIRECT").is_none() {
        let uint = ctx.ty_uint();
        let g = shift_with_const(ctx, &mut insts, Op::ShiftRightLogical, lane.simd_lane, 2);
        let t = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, lane.simd_lane, 3);
        let t2 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, t, 1);
        let t2p = bitwise_with_const(ctx, &mut insts, Op::BitwiseOr, uint, t2, 1);
        for (role, cells) in [(MAT8_A, [(g, t2), (g, t2p)]), (MAT8_B, [(t2, g), (t2p, g)])] {
            let mut e = [0 as Word; 2];
            for (slot, (row, col)) in cells.into_iter().enumerate() {
                let elem_ptr = simdgroup_matrix_elem_ptr(
                    ctx, &mut insts, ptr_ty, &shape, &address, base, row, col,
                );
                let value = ctx.module.fresh_id();
                insts.push(Instruction::new(
                    Op::Load,
                    Some(elem),
                    Some(value),
                    vec![Operand::IdRef(elem_ptr)],
                ));
                e[slot] = value;
            }
            mat8_nv_record(ctx, res, role, e, elem);
            if role == MAT8_A {
                mat8_nv_record(ctx, res, MAT8_C, e, elem);
            }
        }
    }
    matrix8_pack(ctx, &mut insts, elem, rty, res, slots);
    Ok(insts)
}

pub(in crate::passes) fn lower_simdgroup_matrix_8x8_store(
    ctx: &mut Ctx,
    args: &[Word],
) -> Result<Vec<Instruction>, String> {
    if args.len() < 4 {
        return Err(format!(
            "air.simdgroup_matrix_8x8_store expects >=4 operands, got {}",
            args.len()
        ));
    }
    let matrix = args[0];
    let base = args[1];
    let mat_ty = value_result_type(ctx, matrix)
        .ok_or_else(|| "air.simdgroup_matrix store value has no type".to_string())?;
    let (elem, lanes) = composite_shape(ctx, mat_ty)
        .ok_or_else(|| "air.simdgroup_matrix store value is not a 64-lane composite".to_string())?;
    if lanes != 64 {
        return Err("air.simdgroup_matrix store value is not 64-lane".to_string());
    }
    let ptr_ty = value_result_type(ctx, base)
        .ok_or_else(|| "air.simdgroup_matrix store pointer has no type".to_string())?;
    let shape = simdgroup_matrix_block_base(ctx, ptr_ty, elem, "store", "value")?;
    let mut insts = Vec::new();
    let address = simdgroup_matrix_address_for_call(ctx, &mut insts, &args[2..], "store")?;
    let lane = matrix8_lane(ctx, &mut insts);
    let (half, float) = (ctx.ty_half(), ctx.ty_float());
    if (elem == half || elem == float) && std::env::var_os("NVMTL_MATRIX8_NO_DIRECT").is_none() {
        if let Some(hit) = mat8_nv_cached(ctx, matrix, MAT8_C) {
            let uint = ctx.ty_uint();
            let g = shift_with_const(ctx, &mut insts, Op::ShiftRightLogical, lane.simd_lane, 2);
            let t = bitwise_with_const(ctx, &mut insts, Op::BitwiseAnd, uint, lane.simd_lane, 3);
            let t2 = shift_with_const(ctx, &mut insts, Op::ShiftLeftLogical, t, 1);
            let t2p = bitwise_with_const(ctx, &mut insts, Op::BitwiseOr, uint, t2, 1);
            for (slot, col) in [t2, t2p].into_iter().enumerate() {
                let value = convert_matrix_lane(ctx, &mut insts, hit.e[slot], hit.elem, elem);
                let elem_ptr = simdgroup_matrix_elem_ptr(
                    ctx, &mut insts, ptr_ty, &shape, &address, base, g, col,
                );
                insts.push(Instruction::new(
                    Op::Store,
                    None,
                    None,
                    vec![Operand::IdRef(elem_ptr), Operand::IdRef(value)],
                ));
            }
            return Ok(insts);
        }
    }
    for slot in 0..2u32 {
        let value = composite_extract(ctx, &mut insts, elem, matrix, slot);
        let col = matrix8_slot_col(ctx, &mut insts, &lane, slot);
        let elem_ptr = simdgroup_matrix_elem_ptr(
            ctx, &mut insts, ptr_ty, &shape, &address, base, lane.row, col,
        );
        insts.push(Instruction::new(
            Op::Store,
            None,
            None,
            vec![Operand::IdRef(elem_ptr), Operand::IdRef(value)],
        ));
    }
    Ok(insts)
}

pub(in crate::passes) fn composite_extract(
    ctx: &mut Ctx,
    insts: &mut Vec<Instruction>,
    elem: Word,
    value: Word,
    lane: u32,
) -> Word {
    let result = ctx.module.fresh_id();
    insts.push(Instruction::new(
        Op::CompositeExtract,
        Some(elem),
        Some(result),
        vec![Operand::IdRef(value), Operand::LiteralBit32(lane)],
    ));
    result
}

#[cfg(test)]
mod matrix16_mapping_tests {
    use super::{encode_matrix16_col, encode_matrix16_row};
    use std::collections::BTreeSet;

    fn coordinate(lane: u32, component: u32) -> (u32, u32) {
        let row_group = ((lane >> 1) & 3) | ((lane >> 2) & 4);
        let col_group = (lane & 1) | ((lane >> 2) & 2);
        (row_group * 2 + component / 4, col_group * 4 + component % 4)
    }

    #[test]
    fn distributed_fragments_cover_every_matrix_cell_once() {
        let cells: BTreeSet<_> = (0..32)
            .flat_map(|lane| (0..8).map(move |component| coordinate(lane, component)))
            .collect();
        assert_eq!(cells.len(), 16 * 16);
        assert_eq!(cells.first(), Some(&(0, 0)));
        assert_eq!(cells.last(), Some(&(15, 15)));
    }

    #[test]
    fn owner_formulas_cover_normal_and_transposed_operands() {
        for lane in 0..32u32 {
            let row_bits = lane & 0x16;
            let col_bits = lane & 0x09;
            let row_as_col = ((lane >> 2) & 1) | ((lane >> 1) & 8);
            let col_as_row = ((lane << 2) & 4) | ((lane << 1) & 16);
            for component in 0..8u32 {
                let (row, col) = coordinate(lane, component);
                let row_offset = component / 4;
                let col_offset = component % 4;
                for k in 0..16u32 {
                    let a = coordinate(row_bits | encode_matrix16_col(k), row_offset * 4 + k % 4);
                    assert_eq!(a, (row, k));
                    let at = coordinate(
                        row_as_col | encode_matrix16_row(k),
                        (k % 2) * 4 + (lane & 2) + row_offset,
                    );
                    assert_eq!(at, (k, row));

                    let b = coordinate(col_bits | encode_matrix16_row(k), (k % 2) * 4 + col_offset);
                    assert_eq!(b, (k, col));
                    let bt = coordinate(
                        col_as_row | encode_matrix16_col(k) | ((col_offset / 2) << 1),
                        (col_offset % 2) * 4 + k % 4,
                    );
                    assert_eq!(bt, (col, k));
                }
            }
        }
    }
}
