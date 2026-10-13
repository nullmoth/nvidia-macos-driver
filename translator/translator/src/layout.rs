use crate::fxhash::HashMap;
use crate::meta::AirType;
use crate::native::ir::{ll_type_from_air_type, round_up_u64, LlType};
use crate::spirv_module::Instruction;
use crate::spirv_module::Operand;
use spirv::{Op, Word};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AirDataLayout {
    vector_abi_align_bits: HashMap<u32, u32>,
}

impl AirDataLayout {
    pub(crate) fn parse(layout: &str) -> Result<Self, String> {
        let mut vector_abi_align_bits = HashMap::default();
        for component in layout.split('-') {
            let Some(rest) = component.strip_prefix('v') else {
                continue;
            };
            let mut fields = rest.split(':');
            let size = fields
                .next()
                .and_then(|field| field.parse::<u32>().ok())
                .ok_or_else(|| format!("invalid AIR datalayout vector entry `{component}`"))?;
            let abi_align = fields
                .next()
                .and_then(|field| field.parse::<u32>().ok())
                .ok_or_else(|| format!("invalid AIR datalayout vector entry `{component}`"))?;
            if size == 0
                || abi_align == 0
                || !size.is_multiple_of(8)
                || !abi_align.is_multiple_of(8)
            {
                return Err(format!(
                    "unsupported AIR datalayout vector entry `{component}`: sizes and alignments must be positive whole bytes"
                ));
            }
            vector_abi_align_bits.insert(size, abi_align);
        }
        Ok(Self {
            vector_abi_align_bits,
        })
    }

    pub(crate) fn from_ir(ir: &str) -> Result<Option<Self>, String> {
        let Some(line) = ir
            .lines()
            .map(str::trim_start)
            .find(|line| line.starts_with("target datalayout"))
        else {
            return Ok(None);
        };
        let start = line
            .find('"')
            .ok_or_else(|| "invalid target datalayout: missing opening quote".to_string())?;
        let rest = &line[start + 1..];
        let end = rest
            .find('"')
            .ok_or_else(|| "invalid target datalayout: missing closing quote".to_string())?;
        Self::parse(&rest[..end]).map(Some)
    }

    fn vector_align_bytes(&self, store_bits: u32) -> u32 {
        self.vector_abi_align_bits
            .get(&store_bits)
            .copied()
            .unwrap_or_else(|| store_bits.next_power_of_two())
            / 8
    }
}

pub(super) fn scalar_storage_size(
    ty: &LlType,
    resolve: &impl Fn(&LlType) -> LlType,
) -> Option<u64> {
    match resolve(ty) {
        LlType::Bool | LlType::Int(1) | LlType::Int(8) => Some(1),
        LlType::Half | LlType::BFloat | LlType::Int(16) => Some(2),
        LlType::Float | LlType::Int(32) => Some(4),
        LlType::Int(64) | LlType::Ptr(_) => Some(8),
        _ => None,
    }
}

pub(super) fn native_size_align(
    ty: &LlType,
    resolve: &impl Fn(&LlType) -> LlType,
) -> Option<(u64, u64)> {
    match resolve(ty) {
        LlType::Bool => Some((1, 1)),
        LlType::Int(bits) => {
            let bytes = u64::from(bits).div_ceil(8).max(1);
            Some((bytes, bytes.next_power_of_two().min(8)))
        }
        LlType::Half | LlType::BFloat => Some((2, 2)),
        LlType::Float => Some((4, 4)),
        LlType::Ptr(_) => Some((8, 8)),
        LlType::Vector(elem, lanes) => {
            let (elem_size, elem_align) = native_size_align(&elem, resolve)?;
            Some((elem_size * lanes as u64, elem_align))
        }
        LlType::Array(elem, len) => {
            let (elem_size, elem_align) = native_size_align(&elem, resolve)?;
            Some((round_up_u64(elem_size, elem_align) * len as u64, elem_align))
        }
        LlType::Struct(fields) => {
            let mut offset = 0u64;
            let mut max_align = 1u64;
            for field in fields {
                let (size, align) = native_size_align(&field, resolve)?;
                max_align = max_align.max(align);
                offset = round_up_u64(offset, align);
                offset += size;
            }
            Some((round_up_u64(offset, max_align), max_align))
        }
        LlType::Void | LlType::Named(_) => None,
    }
}

pub(super) fn memcpy_size_align(
    ty: &LlType,
    resolve: &impl Fn(&LlType) -> LlType,
    data_layout: Option<&AirDataLayout>,
) -> Option<(u64, u64)> {
    match resolve(ty) {
        LlType::Bool => Some((1, 1)),
        LlType::Int(bits) => {
            let bytes = u64::from(bits).div_ceil(8).max(1);
            Some((bytes, bytes.next_power_of_two().min(8)))
        }
        LlType::Half | LlType::BFloat => Some((2, 2)),
        LlType::Float => Some((4, 4)),
        LlType::Ptr(_) => Some((8, 8)),
        LlType::Vector(elem, lanes) => {
            let (elem_size, _elem_align) = memcpy_size_align(&elem, resolve, data_layout)?;
            let store_size = elem_size.checked_mul(u64::from(lanes))?;
            let store_bits = u32::try_from(store_size.checked_mul(8)?).ok()?;
            let align = u64::from(
                data_layout
                    .map(|layout| layout.vector_align_bytes(store_bits))
                    .unwrap_or_else(|| store_bits.next_power_of_two() / 8),
            );
            Some((round_up_u64(store_size, align), align))
        }
        LlType::Array(elem, len) => {
            let (elem_size, elem_align) = memcpy_size_align(&elem, resolve, data_layout)?;
            Some((round_up_u64(elem_size, elem_align) * len as u64, elem_align))
        }
        LlType::Struct(fields) => {
            let mut offset = 0u64;
            let mut max_align = 1u64;
            for field in fields {
                let (size, align) = memcpy_size_align(&field, resolve, data_layout)?;
                max_align = max_align.max(align);
                offset = round_up_u64(offset, align);
                offset += size;
            }
            Some((round_up_u64(offset, max_align), max_align))
        }
        LlType::Void | LlType::Named(_) => None,
    }
}

pub(super) fn raw_size_align(
    ty: &LlType,
    resolve: &impl Fn(&LlType) -> Result<LlType, String>,
    data_layout: Option<&AirDataLayout>,
) -> Result<(u64, u64), String> {
    match resolve(ty)? {
        LlType::Bool | LlType::Int(1) | LlType::Int(8) => Ok((1, 1)),
        LlType::Half | LlType::BFloat | LlType::Int(16) => Ok((2, 2)),
        LlType::Float | LlType::Int(32) => Ok((4, 4)),
        LlType::Int(64) => Ok((8, 8)),
        LlType::Ptr(_) => Ok((8, 8)),
        LlType::Vector(elem, lanes) => {
            let (elem_size, _) = raw_size_align(&elem, resolve, data_layout)?;
            let store_size = elem_size * u64::from(lanes);
            let store_bits =
                u32::try_from(store_size.checked_mul(8).ok_or_else(|| {
                    "native emitter: raw vector store width overflow".to_string()
                })?)
                .map_err(|_| "native emitter: raw vector store width overflow".to_string())?;
            let align = u64::from(
                data_layout
                    .map(|layout| layout.vector_align_bytes(store_bits))
                    .unwrap_or_else(|| store_bits.next_power_of_two() / 8),
            );
            Ok((round_up_u64(store_size, align), align))
        }
        LlType::Array(elem, len) => {
            let (elem_size, elem_align) = raw_size_align(&elem, resolve, data_layout)?;
            let stride = round_up_u64(elem_size, elem_align);
            Ok((stride * len as u64, elem_align))
        }
        LlType::Struct(fields) => {
            let mut off = 0;
            let mut max_align = 1;
            for field in fields {
                let (size, align) = raw_size_align(&field, resolve, data_layout)?;
                off = round_up_u64(off, align) + size;
                max_align = max_align.max(align);
            }
            Ok((round_up_u64(off, max_align), max_align))
        }
        other => Err(format!(
            "native emitter: raw size/align for {other:?} is not covered yet"
        )),
    }
}

pub(super) fn air_metadata_size_align(
    ty: &AirType,
    resolve: &impl Fn(&LlType) -> LlType,
    data_layout: Option<&AirDataLayout>,
) -> Option<(u64, u64)> {
    match ty {
        AirType::Struct(members) => {
            let mut end = 0u64;
            let mut max_align = 1u64;
            for member in members {
                let (size, align) = air_metadata_size_align(&member.ty, resolve, data_layout)?;
                max_align = max_align.max(align);
                end = end.max(u64::from(member.offset) + size);
            }
            Some((round_up_u64(end, max_align), max_align))
        }
        _ => memcpy_size_align(&ll_type_from_air_type(ty), resolve, data_layout),
    }
}

pub(crate) fn air_metadata_extent(ty: &AirType) -> Option<u64> {
    match ty {
        AirType::Struct(members) => members
            .iter()
            .map(|member| u64::from(member.offset).checked_add(air_metadata_extent(&member.ty)?))
            .try_fold(0, |reach, end| Some(reach.max(end?))),
        AirType::Array { elem, len } => air_metadata_extent(elem)?.checked_mul(u64::from(*len)),
        _ => memcpy_size_align(&ll_type_from_air_type(ty), &|ty| ty.clone(), None)
            .map(|(size, _align)| size),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SpirvLayout<'a> {
    offsets: Option<&'a HashMap<Word, Vec<u32>>>,
    data_layout: Option<&'a AirDataLayout>,
    runtime_arrays_have_one_element_extent: bool,
}

impl<'a> SpirvLayout<'a> {
    pub(crate) const fn natural(data_layout: Option<&'a AirDataLayout>) -> Self {
        Self {
            offsets: None,
            data_layout,
            runtime_arrays_have_one_element_extent: false,
        }
    }

    pub(crate) const fn air_offsets(
        offsets: &'a HashMap<Word, Vec<u32>>,
        data_layout: Option<&'a AirDataLayout>,
    ) -> Self {
        Self {
            offsets: Some(offsets),
            data_layout,
            runtime_arrays_have_one_element_extent: true,
        }
    }

    fn struct_offsets(self, ty: Word) -> Option<&'a [u32]> {
        self.offsets?.get(&ty).map(Vec::as_slice)
    }

    fn supports_runtime_array(self) -> bool {
        self.runtime_arrays_have_one_element_extent
    }

    fn vector_align(self, store_bits: u32) -> u32 {
        self.data_layout
            .map(|layout| layout.vector_align_bytes(store_bits))
            .unwrap_or_else(|| store_bits.next_power_of_two() / 8)
    }
}

pub(crate) fn spirv_size_align(
    ty: Word,
    defs: &HashMap<Word, Instruction>,
    rule: SpirvLayout<'_>,
) -> (u32, u32) {
    let Some(def) = defs.get(&ty) else {
        return (4, 4);
    };
    match def.class.opcode {
        Op::TypeFloat | Op::TypeInt => {
            let width = match def.operands.first() {
                Some(Operand::LiteralBit32(bits)) => *bits / 8,
                _ => 4,
            };
            (width, width)
        }
        Op::TypeVector => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(elem)) => *elem,
                _ => return (16, 16),
            };
            let lanes = match def.operands.get(1) {
                Some(Operand::LiteralBit32(lanes)) => *lanes,
                _ => 4,
            };
            let (elem_size, _) = spirv_size_align(elem, defs, rule);
            let align = rule.vector_align(elem_size * lanes * 8);
            (elem_size * lanes, align)
        }
        Op::TypeArray | Op::TypeRuntimeArray if rule.supports_runtime_array() => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(elem)) => *elem,
                _ => return (16, 16),
            };
            let len = if def.class.opcode == Op::TypeArray {
                array_len(defs, def).unwrap_or(1)
            } else {
                1
            };
            let (elem_size, elem_align) = spirv_size_align(elem, defs, rule);
            (round_up_u32(elem_size, elem_align) * len, elem_align)
        }
        Op::TypeArray => {
            let elem = match def.operands.first() {
                Some(Operand::IdRef(elem)) => *elem,
                _ => return (16, 16),
            };
            let len = array_len(defs, def).unwrap_or(1);
            let (elem_size, elem_align) = spirv_size_align(elem, defs, rule);
            (round_up_u32(elem_size, elem_align) * len, elem_align)
        }
        Op::TypeStruct => {
            let mut cursor = 0u32;
            let mut end = 0u32;
            let mut max_align = 1u32;
            let explicit_offsets = rule.struct_offsets(ty);
            for (index, operand) in def.operands.iter().enumerate() {
                let Operand::IdRef(member) = operand else {
                    continue;
                };
                let (size, align) = spirv_size_align(*member, defs, rule);
                let alloc = round_up_u32(size, align);
                let member_offset = explicit_offsets
                    .and_then(|offsets| offsets.get(index).copied())
                    .unwrap_or_else(|| round_up_u32(cursor, align));
                end = end.max(member_offset + alloc);
                cursor = member_offset + alloc;
                max_align = max_align.max(align);
            }
            (round_up_u32(end, max_align), max_align)
        }
        _ => (4, 4),
    }
}

pub(crate) fn spirv_struct_member(
    struct_ty: Word,
    member_index: usize,
    defs: &HashMap<Word, Instruction>,
    rule: SpirvLayout<'_>,
) -> Option<(u32, Word)> {
    let def = defs.get(&struct_ty)?;
    if def.class.opcode != Op::TypeStruct {
        return None;
    }
    let explicit_offsets = rule.struct_offsets(struct_ty);
    let mut cursor = 0u32;
    for (index, operand) in def.operands.iter().enumerate() {
        let Operand::IdRef(member_ty) = operand else {
            return None;
        };
        let (size, align) = spirv_size_align(*member_ty, defs, rule);
        let offset = explicit_offsets
            .and_then(|offsets| offsets.get(index).copied())
            .unwrap_or_else(|| round_up_u32(cursor, align));
        if index == member_index {
            return Some((offset, *member_ty));
        }
        cursor = offset.checked_add(round_up_u32(size, align))?;
    }
    None
}

fn array_len(defs: &HashMap<Word, Instruction>, def: &Instruction) -> Option<u32> {
    let Operand::IdRef(constant) = def.operands.get(1)? else {
        return None;
    };
    match defs.get(constant)?.operands.first()? {
        Operand::LiteralBit32(len) => Some(*len),
        _ => None,
    }
}

pub(crate) fn round_up_u32(value: u32, align: u32) -> u32 {
    if align == 0 {
        value
    } else {
        value.div_ceil(align) * align
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_metadata_extent_strides_arrays_tightly_and_takes_the_furthest_reach() {
        use crate::meta::{AirMember, AirScalar};

        let packed = AirType::Array {
            elem: Box::new(AirType::PackedVec {
                scalar: AirScalar::Half,
                lanes: 3,
            }),
            len: 3,
        };
        assert_eq!(air_metadata_extent(&packed), Some(18));
        assert_eq!(
            air_metadata_size_align(&packed, &|ty| ty.clone(), None),
            Some((18, 2)),
        );

        let union = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Scalar(AirScalar::UInt),
            },
            AirMember {
                offset: 4,
                ty: AirType::Scalar(AirScalar::UShort),
            },
            AirMember {
                offset: 4,
                ty: AirType::Scalar(AirScalar::UChar),
            },
        ]);
        assert_eq!(air_metadata_extent(&union), Some(6));

        assert_eq!(air_metadata_extent(&AirType::Struct(vec![])), Some(0));
    }

    #[test]
    fn air_metadata_extent_reaches_an_unmodelled_member_declared_size() {
        use crate::meta::{AirMember, AirScalar};

        let with_opaque_tail = AirType::Struct(vec![
            AirMember {
                offset: 0,
                ty: AirType::Scalar(AirScalar::UInt),
            },
            AirMember {
                offset: 8,
                ty: AirType::Opaque { size: 408 },
            },
        ]);
        assert_eq!(air_metadata_extent(&with_opaque_tail), Some(416));
        assert_eq!(
            air_metadata_size_align(&with_opaque_tail, &|ty| ty.clone(), None),
            Some((416, 4)),
        );

        let array_of_opaque = AirType::Array {
            elem: Box::new(AirType::Opaque { size: 288 }),
            len: 8,
        };
        assert_eq!(air_metadata_extent(&array_of_opaque), Some(2304));
    }

    #[test]
    fn air_datalayout_parses_vector_abi_overrides_and_rejects_partial_bytes() {
        let layout = AirDataLayout::parse("e-p:64:64-v24:64:64-v96:128:128-n8:16:32")
            .expect("parse AIR datalayout");
        assert_eq!(layout.vector_align_bytes(24), 8);
        assert_eq!(layout.vector_align_bytes(96), 16);
        assert_eq!(layout.vector_align_bytes(48), 8);

        let error = AirDataLayout::parse("e-v24:30:32").expect_err("partial-byte alignment");
        assert!(error.contains("whole bytes"), "{error}");
    }

    #[test]
    fn spirv_vector_alignment_comes_from_the_supplied_air_datalayout() {
        let byte_ty = 1;
        let vector_ty = 2;
        let struct_ty = 3;
        let defs = crate::fxhash::map_from([
            (
                byte_ty,
                Instruction::new(
                    Op::TypeInt,
                    None,
                    Some(byte_ty),
                    vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
                ),
            ),
            (
                vector_ty,
                Instruction::new(
                    Op::TypeVector,
                    None,
                    Some(vector_ty),
                    vec![Operand::IdRef(byte_ty), Operand::LiteralBit32(3)],
                ),
            ),
            (
                struct_ty,
                Instruction::new(
                    Op::TypeStruct,
                    None,
                    Some(struct_ty),
                    vec![Operand::IdRef(vector_ty), Operand::IdRef(byte_ty)],
                ),
            ),
        ]);
        let data_layout = AirDataLayout::parse("e-v24:64:64").expect("parse override");
        let rule = SpirvLayout::natural(Some(&data_layout));

        assert_eq!(spirv_size_align(vector_ty, &defs, rule), (3, 8));
        assert_eq!(
            spirv_struct_member(struct_ty, 1, &defs, rule),
            Some((8, byte_ty))
        );
        assert_eq!(spirv_size_align(struct_ty, &defs, rule), (16, 8));
    }

    #[test]
    fn spirv_air_offsets_set_struct_and_runtime_array_extent() {
        let uint_ty = 1;
        let struct_ty = 2;
        let runtime_array_ty = 3;
        let mut defs = HashMap::default();
        defs.insert(
            uint_ty,
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint_ty),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
        );
        defs.insert(
            struct_ty,
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(struct_ty),
                vec![Operand::IdRef(uint_ty), Operand::IdRef(uint_ty)],
            ),
        );
        defs.insert(
            runtime_array_ty,
            Instruction::new(
                Op::TypeRuntimeArray,
                None,
                Some(runtime_array_ty),
                vec![Operand::IdRef(struct_ty)],
            ),
        );

        let offsets = crate::fxhash::map_from([(struct_ty, vec![0, 16])]);
        let rule = SpirvLayout::air_offsets(&offsets, None);

        assert_eq!(spirv_size_align(struct_ty, &defs, rule), (20, 4));
        assert_eq!(
            spirv_struct_member(struct_ty, 1, &defs, rule),
            Some((16, uint_ty))
        );
        assert_eq!(spirv_size_align(runtime_array_ty, &defs, rule), (20, 4));
        assert_eq!(
            spirv_size_align(runtime_array_ty, &defs, SpirvLayout::natural(None)),
            (4, 4)
        );
    }

    #[test]
    fn spirv_natural_vector_and_array_rules() {
        let float_ty = 1u32;
        let vec2_ty = 2u32;
        let vec3_ty = 3u32;
        let vec4_ty = 4u32;
        let len3 = 5u32;
        let float_array_ty = 6u32;
        let vec3_array_ty = 7u32;
        let len2 = 8u32;
        let uint_ty = 9u32;
        let mut defs = HashMap::default();
        defs.insert(
            float_ty,
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(float_ty),
                vec![Operand::LiteralBit32(32)],
            ),
        );
        for (id, lanes) in [(vec2_ty, 2u32), (vec3_ty, 3), (vec4_ty, 4)] {
            defs.insert(
                id,
                Instruction::new(
                    Op::TypeVector,
                    None,
                    Some(id),
                    vec![Operand::IdRef(float_ty), Operand::LiteralBit32(lanes)],
                ),
            );
        }
        defs.insert(
            uint_ty,
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uint_ty),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
        );
        defs.insert(
            len3,
            Instruction::new(
                Op::Constant,
                Some(uint_ty),
                Some(len3),
                vec![Operand::LiteralBit32(3)],
            ),
        );
        defs.insert(
            len2,
            Instruction::new(
                Op::Constant,
                Some(uint_ty),
                Some(len2),
                vec![Operand::LiteralBit32(2)],
            ),
        );
        defs.insert(
            float_array_ty,
            Instruction::new(
                Op::TypeArray,
                None,
                Some(float_array_ty),
                vec![Operand::IdRef(float_ty), Operand::IdRef(len3)],
            ),
        );
        defs.insert(
            vec3_array_ty,
            Instruction::new(
                Op::TypeArray,
                None,
                Some(vec3_array_ty),
                vec![Operand::IdRef(vec3_ty), Operand::IdRef(len2)],
            ),
        );

        let rule = SpirvLayout::natural(None);
        assert_eq!(spirv_size_align(float_ty, &defs, rule), (4, 4));
        assert_eq!(spirv_size_align(vec2_ty, &defs, rule), (8, 8));
        assert_eq!(spirv_size_align(vec3_ty, &defs, rule), (12, 16));
        assert_eq!(spirv_size_align(vec4_ty, &defs, rule), (16, 16));
        assert_eq!(spirv_size_align(float_array_ty, &defs, rule), (12, 4));
        assert_eq!(spirv_size_align(vec3_array_ty, &defs, rule), (32, 16));
        assert_eq!(spirv_size_align(999, &defs, rule), (4, 4));
    }

    #[test]
    fn spirv_natural_struct_consumes_vector_allocation_size() {
        let uchar_ty = 1u32;
        let v3uchar_ty = 2u32;
        let struct_ty = 3u32;
        let mut defs = HashMap::default();
        defs.insert(
            uchar_ty,
            Instruction::new(
                Op::TypeInt,
                None,
                Some(uchar_ty),
                vec![Operand::LiteralBit32(8), Operand::LiteralBit32(0)],
            ),
        );
        defs.insert(
            v3uchar_ty,
            Instruction::new(
                Op::TypeVector,
                None,
                Some(v3uchar_ty),
                vec![Operand::IdRef(uchar_ty), Operand::LiteralBit32(3)],
            ),
        );
        defs.insert(
            struct_ty,
            Instruction::new(
                Op::TypeStruct,
                None,
                Some(struct_ty),
                vec![
                    Operand::IdRef(uchar_ty),
                    Operand::IdRef(v3uchar_ty),
                    Operand::IdRef(uchar_ty),
                ],
            ),
        );

        let rule = SpirvLayout::natural(None);
        assert_eq!(spirv_size_align(v3uchar_ty, &defs, rule), (3, 4));
        assert_eq!(spirv_size_align(struct_ty, &defs, rule), (12, 4));
        assert_eq!(
            spirv_struct_member(struct_ty, 2, &defs, rule),
            Some((8, uchar_ty))
        );
    }

    #[test]
    fn every_three_lane_scalar_family_uses_allocation_stride_for_following_members() {
        for (case, (component_bits, component_is_float)) in
            [(8, false), (16, false), (16, true), (32, true)]
                .into_iter()
                .enumerate()
        {
            for (next_case, next_bits) in [8, 16, 32].into_iter().enumerate() {
                let base = 100 + (case * 20 + next_case * 4) as u32;
                let component_ty = base;
                let vector_ty = base + 1;
                let next_ty = base + 2;
                let struct_ty = base + 3;
                let scalar_op = if component_is_float {
                    Op::TypeFloat
                } else {
                    Op::TypeInt
                };
                let scalar_operands = if component_is_float {
                    vec![Operand::LiteralBit32(component_bits)]
                } else {
                    vec![
                        Operand::LiteralBit32(component_bits),
                        Operand::LiteralBit32(0),
                    ]
                };
                let defs = crate::fxhash::map_from([
                    (
                        component_ty,
                        Instruction::new(scalar_op, None, Some(component_ty), scalar_operands),
                    ),
                    (
                        vector_ty,
                        Instruction::new(
                            Op::TypeVector,
                            None,
                            Some(vector_ty),
                            vec![Operand::IdRef(component_ty), Operand::LiteralBit32(3)],
                        ),
                    ),
                    (
                        next_ty,
                        Instruction::new(
                            Op::TypeInt,
                            None,
                            Some(next_ty),
                            vec![Operand::LiteralBit32(next_bits), Operand::LiteralBit32(0)],
                        ),
                    ),
                    (
                        struct_ty,
                        Instruction::new(
                            Op::TypeStruct,
                            None,
                            Some(struct_ty),
                            vec![Operand::IdRef(vector_ty), Operand::IdRef(next_ty)],
                        ),
                    ),
                ]);
                let component_bytes = component_bits / 8;
                let allocation_size = component_bytes * 4;
                let expected_next = round_up_u32(allocation_size, next_bits / 8);
                assert_eq!(
                    spirv_struct_member(struct_ty, 1, &defs, SpirvLayout::natural(None)),
                    Some((expected_next, next_ty)),
                    "component_bits={component_bits} float={component_is_float} next_bits={next_bits}"
                );
                assert_eq!(
                    spirv_size_align(vector_ty, &defs, SpirvLayout::natural(None)),
                    (component_bytes * 3, allocation_size)
                );
            }
        }
    }
}
