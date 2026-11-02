use super::*;

pub(super) fn gep_spirv_indices(indices: &[TypedValue]) -> Result<Vec<TypedValue>, String> {
    if const_index(indices.first()) == Some(0) {
        return Ok(indices[1..].to_vec());
    }
    Ok(indices.to_vec())
}

pub(super) fn gep_pointee(source: &LlType, indices: &[TypedValue]) -> Result<LlType, String> {
    let mut cur = source.clone();
    for (i, tv) in indices.iter().enumerate() {
        if i == 0 {
            continue;
        }
        match cur {
            LlType::Struct(fields) => {
                let idx = const_index(Some(tv)).ok_or_else(|| {
                    "native emitter: dynamic struct getelementptr indices are not covered yet"
                        .to_string()
                })?;
                cur = fields.get(idx as usize).cloned().ok_or_else(|| {
                    format!("native emitter: struct GEP index {idx} out of range")
                })?;
            }
            LlType::Array(elem, _) => {
                cur = *elem;
            }
            LlType::Vector(elem, _) => {
                cur = *elem;
            }
            other => {
                return Err(format!(
                    "native emitter: GEP through {other:?} is not covered yet"
                ));
            }
        }
    }
    Ok(cur)
}

pub(super) fn extract_value_type(source: &LlType, indices: &[u32]) -> Result<LlType, String> {
    let mut cur = source.clone();
    for idx in indices {
        match cur {
            LlType::Struct(fields) => {
                cur = fields.get(*idx as usize).cloned().ok_or_else(|| {
                    format!("native emitter: extractvalue struct index {idx} out of range")
                })?;
            }
            LlType::Array(elem, _) => {
                cur = *elem;
            }
            other => {
                return Err(format!(
                    "native emitter: extractvalue through {other:?} is not covered yet"
                ));
            }
        }
    }
    Ok(cur)
}

pub(super) fn types_compatible(have: &LlType, want: &LlType) -> bool {
    have == want
        || matches!(
            (have, want),
            (LlType::Bool, LlType::Int(1)) | (LlType::Int(1), LlType::Bool)
        )
        || match (have, want) {
            (LlType::Vector(h_elem, h_lanes), LlType::Vector(w_elem, w_lanes))
                if h_lanes == w_lanes =>
            {
                types_compatible(h_elem, w_elem)
            }
            // Two address spaces that become the same SPIR-V storage class describe the same pointer, and the
            // number they carry in the AIR no longer distinguishes them. A ray payload arrives this way: the
            // kernel holds it in its own space and an intersection function declares the parameter in AIR's
            // ray-data space, both Private. A pointer crossing INTO another storage class is still refused.
            (LlType::Ptr(have_space), LlType::Ptr(want_space)) => {
                matches!(
                    (llvm_pointer_storage(*have_space), llvm_pointer_storage(*want_space)),
                    (Ok(h), Ok(w)) if h == w
                )
            }
            _ => false,
        }
}

pub(super) fn pointer_index_name(name: &str) -> String {
    format!("{name}.air.ptridx")
}

pub(super) fn pointer_null_name(name: &str) -> String {
    format!("{name}.air.isnull")
}

pub(super) fn raw_word_index_name(name: &str) -> String {
    format!("{name}.air.rawword")
}

pub(super) fn raw_byte_index_name(name: &str) -> String {
    format!("{name}.air.rawbyte")
}

pub(super) fn compatible_pointer_provenance(a: &GepProvenance, b: &GepProvenance) -> bool {
    a.root == b.root
        && a.addrspace == b.addrspace
        && a.source_ty == b.source_ty
        && a.indices.len() == b.indices.len()
}

pub(super) fn is_bool_type(ty: &LlType) -> bool {
    match ty {
        LlType::Bool => true,
        LlType::Vector(elem, _) => is_bool_type(elem),
        _ => false,
    }
}

pub(super) fn is_integer_type(ty: &LlType) -> bool {
    match ty {
        LlType::Int(_) => true,
        LlType::Vector(elem, _) => is_integer_type(elem),
        _ => false,
    }
}

pub(super) fn spirv_int_width(bits: u32) -> Result<u32, String> {
    match bits {
        0 => Err("native emitter: integer width i0 is not covered".into()),
        1 => Ok(1),
        2..=8 => Ok(8),
        9..=16 => Ok(16),
        17..=32 => Ok(32),
        33..=64 => Ok(64),
        other => Err(format!(
            "native emitter: unsupported integer constant width i{other}"
        )),
    }
}

pub(super) fn is_spirv_legal_int_width(bits: u32) -> bool {
    matches!(bits, 8 | 16 | 32 | 64)
}

pub(super) fn nonstandard_scalar_int_bits(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Int(bits) if !is_spirv_legal_int_width(*bits) && *bits > 1 => Some(*bits),
        _ => None,
    }
}

pub(super) fn is_float_type(ty: &LlType) -> bool {
    match ty {
        LlType::Float | LlType::Half => true,
        LlType::Vector(elem, _) => is_float_type(elem),
        _ => false,
    }
}

pub(super) fn logical_op_for_bitwise(op: Op, ty: &LlType) -> Option<Op> {
    if !is_bool_type(ty) {
        return None;
    }
    match op {
        Op::BitwiseAnd => Some(Op::LogicalAnd),
        Op::BitwiseOr => Some(Op::LogicalOr),
        Op::BitwiseXor => Some(Op::LogicalNotEqual),
        _ => None,
    }
}

pub(super) fn float_convert_supported(src: &LlType, dst: &LlType) -> bool {
    match (src, dst) {
        (LlType::Float | LlType::Half, LlType::Float | LlType::Half) => true,
        (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes))
            if src_lanes == dst_lanes =>
        {
            float_convert_supported(src_elem, dst_elem)
        }
        _ => false,
    }
}

pub(super) fn int_to_float_convert_supported(src: &LlType, dst: &LlType) -> bool {
    match (src, dst) {
        (LlType::Int(_), LlType::Float | LlType::Half) => true,
        (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes))
            if src_lanes == dst_lanes =>
        {
            int_to_float_convert_supported(src_elem, dst_elem)
        }
        _ => false,
    }
}

pub(super) fn int_convert_supported(src: &LlType, dst: &LlType) -> bool {
    match (src, dst) {
        (LlType::Int(_), LlType::Int(_)) => true,
        (LlType::Vector(src_elem, src_lanes), LlType::Vector(dst_elem, dst_lanes))
            if src_lanes == dst_lanes =>
        {
            int_convert_supported(src_elem, dst_elem)
        }
        _ => false,
    }
}

pub(super) fn is_byte_view_scalar_or_vector_source(ty: &LlType) -> bool {
    fn is_wide_scalar(ty: &LlType) -> bool {
        match ty {
            LlType::Float | LlType::Half | LlType::BFloat => true,
            LlType::Int(bits) => *bits >= 16,
            _ => false,
        }
    }
    match ty {
        LlType::Vector(elem, lanes) => *lanes > 0 && is_wide_scalar(elem),
        other => is_wide_scalar(other),
    }
}

pub(super) fn bitcast_width(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Float | LlType::Int(32) => Some(32),
        LlType::Half | LlType::BFloat | LlType::Int(16) => Some(16),
        LlType::Int(bits) => Some(*bits),
        LlType::Vector(elem, lanes) => bitcast_width(elem).map(|bits| bits * *lanes),
        _ => None,
    }
}

pub(super) fn first_aggregate_leaf(
    ty: &LlType,
    is_leaf: &impl Fn(&LlType) -> bool,
    descend_vectors: bool,
) -> Option<(LlType, Vec<u32>)> {
    if is_leaf(ty) {
        return Some((ty.clone(), vec![]));
    }
    let inner: &LlType = match ty {
        LlType::Vector(elem, lanes) if descend_vectors && *lanes > 0 => elem,
        LlType::Array(elem, len) if *len > 0 => elem,
        LlType::Struct(fields) => fields.first()?,
        _ => return None,
    };
    let (leaf, mut path) = first_aggregate_leaf(inner, is_leaf, descend_vectors)?;
    path.insert(0, 0);
    Some((leaf, path))
}

pub(super) fn first_scalar_access_path(ty: &LlType) -> Option<(LlType, Vec<u32>)> {
    first_aggregate_leaf(
        ty,
        &|t| {
            matches!(
                t,
                LlType::Float | LlType::Half | LlType::BFloat | LlType::Int(_) | LlType::Bool
            )
        },
        true,
    )
}

pub(super) fn llvm_pointer_storage(addrspace: u32) -> Result<StorageClass, String> {
    match addrspace {
        0 => Ok(StorageClass::Private),
        1 | 2 => Ok(StorageClass::UniformConstant),
        3 => Ok(StorageClass::Workgroup),
        4 => Ok(StorageClass::Private),
        // AIR's ray-data space, which a [[payload]] parameter is declared in. An inline ray query has no
        // payload storage of its own: the caller allocates the payload and the intersection functions write
        // through the pointer, so it is the caller's own storage under another number.
        5 => Ok(StorageClass::Private),
        _ => Err(format!(
            "native emitter: unsupported LLVM pointer addrspace({addrspace})"
        )),
    }
}

pub(super) fn function_storage_local_type(ty: &LlType) -> LlType {
    match ty {
        LlType::Ptr(_) => LlType::Int(64),
        LlType::Vector(elem, lanes) => {
            LlType::Vector(Box::new(function_storage_local_type(elem)), *lanes)
        }
        LlType::Array(elem, len) => {
            LlType::Array(Box::new(function_storage_local_type(elem)), *len)
        }
        LlType::Struct(fields) => {
            LlType::Struct(fields.iter().map(function_storage_local_type).collect())
        }
        _ => ty.clone(),
    }
}

pub(super) fn i8_leaf_byte_size(ty: &LlType) -> Option<u32> {
    match ty {
        LlType::Int(8) => Some(1),
        LlType::Array(elem, len) => i8_leaf_byte_size(elem)?.checked_mul(*len),
        LlType::Struct(fields) => {
            let mut total: u32 = 0;
            for field in fields {
                total = total.checked_add(i8_leaf_byte_size(field)?)?;
            }
            Some(total)
        }
        _ => None,
    }
}

pub(super) fn flat_scalar_leaf_count(ty: &LlType) -> Option<(LlType, u32)> {
    match ty {
        LlType::Bool | LlType::Half | LlType::BFloat | LlType::Float | LlType::Int(_) => {
            Some((ty.clone(), 1))
        }
        LlType::Array(elem, len) => {
            let (leaf, count) = flat_scalar_leaf_count(elem)?;
            Some((leaf, count.checked_mul(*len)?))
        }
        LlType::Struct(fields) => {
            let mut leaf = None;
            let mut count = 0u32;
            for field in fields {
                let (field_leaf, field_count) = flat_scalar_leaf_count(field)?;
                if leaf
                    .as_ref()
                    .is_some_and(|existing| !types_compatible(existing, &field_leaf))
                {
                    return None;
                }
                leaf.get_or_insert(field_leaf);
                count = count.checked_add(field_count)?;
            }
            leaf.map(|leaf| (leaf, count))
        }
        _ => None,
    }
}

pub(super) fn raw_buffer_block_type() -> LlType {
    LlType::Struct(vec![LlType::Array(Box::new(LlType::Int(32)), 0)])
}

pub(super) fn raw_workgroup_array_type() -> LlType {
    LlType::Array(Box::new(LlType::Int(32)), 2048)
}

pub(super) fn is_zero_wrapper_identity_gep(source: &LlType, indices: &[TypedValue]) -> bool {
    if indices.iter().any(|idx| const_index(Some(idx)) != Some(0)) {
        return false;
    }
    matches!(
        source,
        LlType::Struct(fields)
            if matches!(fields.as_slice(), [LlType::Array(_, 0)])
    )
}

pub(super) fn is_zero_wrapper_source(source: &LlType) -> bool {
    matches!(
        source,
        LlType::Struct(fields)
            if matches!(fields.as_slice(), [LlType::Array(_, 0)])
    )
}

pub(super) fn wrapper_gep_index(indices: &[TypedValue]) -> Option<&TypedValue> {
    if indices.len() != 3 {
        return None;
    }
    if const_index(indices.first()) != Some(0) || const_index(indices.get(1)) != Some(0) {
        return None;
    }
    indices.get(2)
}

pub(super) fn gep_can_offset_element_pointer(
    source: &LlType,
    indices: &[TypedValue],
    pointee: &LlType,
) -> bool {
    if indices.len() == 1 && source == pointee {
        return true;
    }

    let Some(parent) = gep_parent_before_last(source, indices) else {
        return false;
    };
    match parent {
        LlType::Array(elem, _) | LlType::Vector(elem, _) => elem.as_ref() == pointee,
        _ => false,
    }
}

pub(super) fn gep_parent_before_last(source: &LlType, indices: &[TypedValue]) -> Option<LlType> {
    if indices.len() <= 1 {
        return None;
    }

    let mut cur = source.clone();
    for (i, tv) in indices.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if i + 1 == indices.len() {
            return Some(cur);
        }
        match cur {
            LlType::Struct(fields) => {
                let idx = const_index(Some(tv))? as usize;
                cur = fields.get(idx)?.clone();
            }
            LlType::Array(elem, _) | LlType::Vector(elem, _) => {
                cur = *elem;
            }
            _ => return None,
        }
    }
    None
}

pub(super) fn const_index(value: Option<&TypedValue>) -> Option<u32> {
    let value = value?;
    match &value.value {
        LlValue::Int(v) | LlValue::Hex(v) => (*v <= u32::MAX as u64).then_some(*v as u32),
        LlValue::SignedInt(v) if *v >= 0 => (*v <= u32::MAX as i64).then_some(*v as u32),
        _ => None,
    }
}

pub(super) fn const_index_i64(value: &TypedValue) -> Option<i64> {
    match value.value {
        LlValue::Int(v) => Some(v as i64),
        LlValue::SignedInt(v) => Some(v),
        _ => None,
    }
}

pub(super) fn round_up_u64(value: u64, align: u64) -> u64 {
    if align == 0 {
        value
    } else {
        value.div_ceil(align) * align
    }
}

impl Emitter {
    pub(super) fn record_int_alignment(&mut self, name: &str, ty: &LlType, alignment: u64) {
        if matches!(self.resolve_type(ty), Ok(LlType::Int(_))) {
            self.int_alignments
                .insert(name.to_string(), alignment.clamp(1, 4));
        }
    }

    pub(super) fn int_value_alignment(&self, value: &LlValue) -> u64 {
        match value {
            LlValue::Local(name) => self.int_alignments.get(name).copied().unwrap_or(1),
            LlValue::Int(value) | LlValue::Hex(value) => word_alignment_factor(*value),
            LlValue::SignedInt(value) => signed_word_alignment_factor(*value),
            LlValue::Zero => 4,
            _ => 1,
        }
    }

    pub(super) fn merged_int_alignment(&self, values: impl IntoIterator<Item = LlValue>) -> u64 {
        values
            .into_iter()
            .map(|value| self.int_value_alignment(&value))
            .reduce(gcd_u64)
            .unwrap_or(1)
    }

    pub(super) fn raw_pointer_word_aligned(&self, raw: &RawBufferOffset) -> bool {
        !raw.unmodelable
            && raw.const_off % 4 == 0
            && raw
                .dyn_terms
                .iter()
                .all(|(index, stride)| self.raw_dynamic_term_word_aligned(index, *stride))
    }

    pub(super) fn raw_dynamic_term_word_aligned(&self, index: &TypedValue, stride: i64) -> bool {
        self.raw_dynamic_term_aligned_to(index, stride, 4)
    }

    pub(super) fn raw_offset_aligned_to(
        &self,
        raw: &RawBufferOffset,
        extra_byte: u64,
        align: u64,
    ) -> bool {
        if align <= 1 {
            return !raw.unmodelable;
        }
        let off = raw.const_off + extra_byte as i64;
        !raw.unmodelable
            && off % align as i64 == 0
            && raw
                .dyn_terms
                .iter()
                .all(|(index, stride)| self.raw_dynamic_term_aligned_to(index, *stride, align))
    }

    fn raw_dynamic_term_aligned_to(&self, index: &TypedValue, stride: i64, align: u64) -> bool {
        raw_stride_aligned_to(self.int_value_alignment(&index.value), stride, align)
    }
}

pub(super) fn add_int_alignment(lhs: u64, rhs: u64) -> u64 {
    gcd_u64(lhs, rhs).max(1)
}

pub(super) fn mul_int_alignment(lhs: u64, rhs: u64) -> u64 {
    lhs.saturating_mul(rhs).clamp(1, 4)
}

pub(super) fn shift_left_int_alignment(lhs: u64, rhs: &LlValue) -> u64 {
    let factor = match rhs {
        LlValue::Int(value) | LlValue::Hex(value) => {
            if *value >= 2 {
                4
            } else if *value == 1 {
                2
            } else {
                1
            }
        }
        LlValue::SignedInt(value) => {
            if *value >= 2 {
                4
            } else if *value == 1 {
                2
            } else {
                1
            }
        }
        _ => 1,
    };
    mul_int_alignment(lhs, factor)
}

pub(super) fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

fn word_alignment_factor(value: u64) -> u64 {
    if value.is_multiple_of(4) {
        4
    } else if value.is_multiple_of(2) {
        2
    } else {
        1
    }
}

fn signed_word_alignment_factor(value: i64) -> u64 {
    if value.rem_euclid(4) == 0 {
        4
    } else if value.rem_euclid(2) == 0 {
        2
    } else {
        1
    }
}

fn abs_i64_as_u64(value: i64) -> u64 {
    if value == i64::MIN {
        1u64 << 63
    } else {
        value.unsigned_abs()
    }
}

fn raw_stride_aligned_to(index_alignment: u64, stride: i64, align: u64) -> bool {
    let required_index_alignment = align / gcd_u64(abs_i64_as_u64(stride), align);
    index_alignment.is_multiple_of(required_index_alignment)
}

#[cfg(test)]
mod atomic_alignment_tests {
    use super::raw_stride_aligned_to;

    #[test]
    fn raw_stride_alignment_matches_concrete_offsets_for_word_and_wide_units() {
        for align in [4, 8, 12, 24] {
            for known_index_alignment in [1, 2, 4] {
                for stride in -24..=24 {
                    let concrete_offsets_are_aligned = (0..=96i64)
                        .filter(|index| (*index as u64).is_multiple_of(known_index_alignment))
                        .all(|index| (index * stride).rem_euclid(align as i64) == 0);
                    assert_eq!(raw_stride_aligned_to(known_index_alignment, stride, align), concrete_offsets_are_aligned,
                        "index alignment {known_index_alignment}, stride {stride}, access unit {align}");
                }
            }
        }
    }
}
