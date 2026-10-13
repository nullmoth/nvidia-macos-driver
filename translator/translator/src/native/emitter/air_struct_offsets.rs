use super::*;
use crate::emit_sidecar::{AirStructLayoutMapping, AirStructLayoutMappingStatus};
use crate::meta::{AirMember, AirScalar, AirType};

impl Emitter {
    pub(super) fn record_air_struct_offsets(
        &mut self,
        buffer_layouts: Option<&HashMap<u32, AirType>>,
        air_data_layout: Option<&crate::layout::AirDataLayout>,
    ) {
        let Some(buffer_layouts) = buffer_layouts else {
            return;
        };
        let entry_id = self
            .ir
            .entry_name
            .as_ref()
            .and_then(|name| self.function_ids.get(name))
            .copied();
        let entry = entry_id
            .and_then(|entry_id| {
                self.module.functions.iter().find(|function| {
                    function
                        .def
                        .as_ref()
                        .and_then(|instruction| instruction.result_id)
                        == Some(entry_id)
                })
            })
            .or_else(|| {
                self.module
                    .functions
                    .iter()
                    .find(|function| !function.blocks.is_empty())
            });
        let Some(entry) = entry else {
            return;
        };
        let param_types = entry
            .parameters
            .iter()
            .map(|param| param.result_type)
            .collect::<Vec<_>>();
        let defs = self
            .module
            .types_global_values
            .iter()
            .filter_map(|instruction| instruction.result_id.map(|id| (id, instruction.clone())))
            .collect::<HashMap<_, _>>();

        let param_count = param_types.len();
        for (index, param_type) in param_types.into_iter().enumerate() {
            let Some(layout) = buffer_layouts.get(&(index as u32)) else {
                continue;
            };
            let Some(struct_ty) = param_type.and_then(|ty| pointer_pointee(&defs, ty)) else {
                self.emit_sidecar
                    .air_struct_layout_mappings
                    .push(AirStructLayoutMapping {
                        param_index: index as u32,
                        struct_ty: None,
                        status: AirStructLayoutMappingStatus::ParameterIsNotPointer,
                    });
                continue;
            };
            let mut status = remember_existing_air_struct_offsets(
                &mut self.emit_sidecar.air_struct_offsets,
                &defs,
                struct_ty,
                layout,
                air_data_layout,
            );
            if status == AirStructLayoutMappingStatus::EmittedShapeMismatch
                && !emitted_carries_members(&defs, struct_ty)
            {
                status = AirStructLayoutMappingStatus::EmittedIsUntypedBuffer;
            }
            self.emit_sidecar
                .air_struct_layout_mappings
                .push(AirStructLayoutMapping {
                    param_index: index as u32,
                    struct_ty: Some(struct_ty),
                    status,
                });
        }
        let mut missing_params = buffer_layouts
            .keys()
            .copied()
            .filter(|index| *index as usize >= param_count)
            .collect::<Vec<_>>();
        missing_params.sort_unstable();
        self.emit_sidecar
            .air_struct_layout_mappings
            .extend(
                missing_params
                    .into_iter()
                    .map(|param_index| AirStructLayoutMapping {
                        param_index,
                        struct_ty: None,
                        status: AirStructLayoutMappingStatus::ParameterMissing,
                    }),
            );
    }
}

fn emitted_carries_members(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    let mut current = ty;
    for _ in 0..8 {
        let Some(def) = defs.get(&current) else {
            return false;
        };
        match def.class.opcode {
            Op::TypeStruct if def.operands.len() != 1 => return true,
            Op::TypeStruct => match def.operands.first() {
                Some(Operand::IdRef(member_ty)) => current = *member_ty,
                _ => return true,
            },
            Op::TypeRuntimeArray => match def.operands.first() {
                Some(Operand::IdRef(elem)) => current = *elem,
                _ => return false,
            },
            Op::TypeArray => match array_type(defs, current) {
                Some((elem, _len)) => current = elem,
                None => return false,
            },
            _ => return false,
        }
    }
    false
}

fn pointer_pointee(defs: &HashMap<Word, Instruction>, ty: Word) -> Option<Word> {
    let def = defs.get(&ty)?;
    (def.class.opcode == Op::TypePointer).then_some(())?;
    match def.operands.get(1)? {
        Operand::IdRef(pointee) => Some(*pointee),
        _ => None,
    }
}

fn remember_existing_air_struct_offsets(
    offsets_by_type: &mut HashMap<Word, Vec<u32>>,
    defs: &HashMap<Word, Instruction>,
    struct_ty: Word,
    layout: &AirType,
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) -> AirStructLayoutMappingStatus {
    let AirType::Struct(members) = layout else {
        return AirStructLayoutMappingStatus::MetadataIsNotStruct;
    };
    remember_existing_air_struct_offsets_inner(
        offsets_by_type,
        defs,
        struct_ty,
        members,
        air_data_layout,
    )
}

fn remember_existing_air_struct_offsets_inner(
    offsets_by_type: &mut HashMap<Word, Vec<u32>>,
    defs: &HashMap<Word, Instruction>,
    struct_ty: Word,
    members: &[AirMember],
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) -> AirStructLayoutMappingStatus {
    let Some(def) = defs.get(&struct_ty) else {
        return AirStructLayoutMappingStatus::EmittedShapeMismatch;
    };
    if def.class.opcode != Op::TypeStruct || def.operands.len() < members.len() {
        return AirStructLayoutMappingStatus::EmittedShapeMismatch;
    }
    let Some(offsets) = map_existing_air_struct_offsets(defs, def, members, air_data_layout) else {
        return AirStructLayoutMappingStatus::EmittedShapeMismatch;
    };
    if !offsets.windows(2).all(|window| window[1] > window[0]) {
        return AirStructLayoutMappingStatus::NonIncreasingOffsets;
    }
    let natural_offsets = (0..def.operands.len())
        .map(|index| {
            crate::layout::spirv_struct_member(
                struct_ty,
                index,
                defs,
                crate::layout::SpirvLayout::natural(air_data_layout),
            )
            .map(|(offset, _)| offset)
        })
        .collect::<Option<Vec<_>>>();
    let status = if natural_offsets.as_ref() == Some(&offsets) {
        AirStructLayoutMappingStatus::MappedNatural
    } else {
        AirStructLayoutMappingStatus::MappedExplicit
    };
    offsets_by_type.insert(struct_ty, offsets);
    remember_nested_existing_air_struct_offsets(
        offsets_by_type,
        defs,
        def,
        members,
        air_data_layout,
    );
    status
}

fn remember_nested_existing_air_struct_offsets(
    offsets_by_type: &mut HashMap<Word, Vec<u32>>,
    defs: &HashMap<Word, Instruction>,
    def: &Instruction,
    members: &[AirMember],
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) {
    let mut air_idx = 0usize;
    for op in &def.operands {
        let Operand::IdRef(member_ty) = op else {
            return;
        };
        if let Some(member) = members.get(air_idx) {
            if air_type_matches_existing(defs, *member_ty, &member.ty, air_data_layout) {
                remember_existing_air_type_offsets(
                    offsets_by_type,
                    defs,
                    *member_ty,
                    &member.ty,
                    air_data_layout,
                );
                air_idx += 1;
                continue;
            }
        }
        if is_backend_padding_array(defs, *member_ty) {
            continue;
        }
        return;
    }
}

fn remember_existing_air_type_offsets(
    offsets_by_type: &mut HashMap<Word, Vec<u32>>,
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    air_ty: &AirType,
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) {
    match air_ty {
        AirType::Struct(members) => {
            let _ = remember_existing_air_struct_offsets_inner(
                offsets_by_type,
                defs,
                ty,
                members,
                air_data_layout,
            );
        }
        AirType::Array { elem, .. } => {
            if let Some((array_elem, _)) = array_type(defs, ty) {
                remember_existing_air_type_offsets(
                    offsets_by_type,
                    defs,
                    array_elem,
                    elem,
                    air_data_layout,
                );
            }
        }
        _ => {}
    }
}

fn map_existing_air_struct_offsets(
    defs: &HashMap<Word, Instruction>,
    def: &Instruction,
    members: &[AirMember],
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) -> Option<Vec<u32>> {
    let mut offsets = Vec::with_capacity(def.operands.len());
    let mut air_idx = 0usize;
    let mut cursor = 0u32;

    for op in &def.operands {
        let Operand::IdRef(member_ty) = op else {
            return None;
        };
        let (size, align) = crate::layout::spirv_size_align(
            *member_ty,
            defs,
            crate::layout::SpirvLayout::natural(air_data_layout),
        );
        let allocation_size = crate::layout::round_up_u32(size, align);
        if let Some(member) = members.get(air_idx) {
            if air_type_matches_existing(defs, *member_ty, &member.ty, air_data_layout) {
                offsets.push(member.offset);
                cursor = member.offset.saturating_add(allocation_size);
                air_idx += 1;
                continue;
            }
        }
        if is_backend_padding_array(defs, *member_ty) {
            let offset = cursor;
            offsets.push(offset);
            cursor = offset.saturating_add(allocation_size);
            continue;
        }
        return None;
    }

    (air_idx == members.len()).then_some(offsets)
}

fn air_type_matches_existing(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    air_ty: &AirType,
    air_data_layout: Option<&crate::layout::AirDataLayout>,
) -> bool {
    match air_ty {
        AirType::Scalar(scalar) => scalar_type_matches(defs, ty, *scalar),
        AirType::Vec { scalar, lanes } => vector_type_matches(defs, ty, *scalar, *lanes),
        AirType::PackedVec { scalar, lanes } => array_type_matches(defs, ty, *scalar, *lanes),
        AirType::Array { elem, len } => {
            let Some((array_elem, array_len)) = array_type(defs, ty) else {
                return false;
            };
            array_len == *len && air_type_matches_existing(defs, array_elem, elem, air_data_layout)
        }
        AirType::Matrix { scalar, cols, rows } => {
            matrix_type_matches(defs, ty, *scalar, *cols, *rows)
        }
        AirType::Struct(members) => {
            let Some(def) = defs.get(&ty) else {
                return false;
            };
            def.class.opcode == Op::TypeStruct
                && map_existing_air_struct_offsets(defs, def, members, air_data_layout)
                    .is_some_and(|offsets| offsets.windows(2).all(|window| window[1] > window[0]))
        }
        AirType::Opaque { size } => {
            let (emitted_size, emitted_align) = crate::layout::spirv_size_align(
                ty,
                defs,
                crate::layout::SpirvLayout::natural(air_data_layout),
            );
            crate::layout::round_up_u32(emitted_size, emitted_align) == *size
        }
    }
}

fn scalar_type_matches(defs: &HashMap<Word, Instruction>, ty: Word, scalar: AirScalar) -> bool {
    match scalar {
        AirScalar::Float => type_float_width(defs, ty).is_some(),
        AirScalar::Half => type_float_width(defs, ty) == Some(16),
        AirScalar::UInt | AirScalar::SInt => type_int_width(defs, ty) == Some(32),
        AirScalar::ULong | AirScalar::SLong => type_int_width(defs, ty) == Some(64),
        AirScalar::UShort | AirScalar::SShort => type_int_width(defs, ty) == Some(16),
        AirScalar::UChar => type_int_width(defs, ty) == Some(8),
        AirScalar::Bool => {
            type_int_width(defs, ty) == Some(8)
                || type_float_width(defs, ty) == Some(32)
                || defs
                    .get(&ty)
                    .is_some_and(|def| def.class.opcode == Op::TypeBool)
        }
    }
}

fn vector_type_matches(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    scalar: AirScalar,
    lanes: u32,
) -> bool {
    let Some(def) = defs.get(&ty) else {
        return false;
    };
    if def.class.opcode != Op::TypeVector {
        return false;
    }
    let Some(Operand::IdRef(elem)) = def.operands.first() else {
        return false;
    };
    let Some(Operand::LiteralBit32(n)) = def.operands.get(1) else {
        return false;
    };
    *n == lanes && scalar_type_matches(defs, *elem, scalar)
}

fn array_type_matches(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    scalar: AirScalar,
    len: u32,
) -> bool {
    let Some((elem, array_len)) = array_type(defs, ty) else {
        return false;
    };
    array_len == len && scalar_type_matches(defs, elem, scalar)
}

fn matrix_type_matches(
    defs: &HashMap<Word, Instruction>,
    ty: Word,
    scalar: AirScalar,
    cols: u32,
    rows: u32,
) -> bool {
    let Some(def) = defs.get(&ty) else {
        return false;
    };
    if def.class.opcode != Op::TypeStruct || def.operands.len() != 1 {
        return false;
    }
    let Some(Operand::IdRef(array_ty)) = def.operands.first() else {
        return false;
    };
    let Some((vec_ty, array_len)) = array_type(defs, *array_ty) else {
        return false;
    };
    array_len == cols && vector_type_matches(defs, vec_ty, scalar, rows)
}

use crate::spirv_module::array_type;

use crate::spirv_module::type_int_width;

use crate::spirv_module::type_float_width;

fn is_backend_padding_array(defs: &HashMap<Word, Instruction>, ty: Word) -> bool {
    if type_int_width(defs, ty) == Some(8) {
        return true;
    }
    let Some((elem, _len)) = array_type(defs, ty) else {
        return false;
    };
    type_int_width(defs, elem) == Some(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_nonincreasing_air_offsets_fail_the_top_level_mapping() {
        let byte_ty = 1;
        let inner_ty = 2;
        let outer_ty = 3;
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
                inner_ty,
                Instruction::new(
                    Op::TypeStruct,
                    None,
                    Some(inner_ty),
                    vec![Operand::IdRef(byte_ty), Operand::IdRef(byte_ty)],
                ),
            ),
            (
                outer_ty,
                Instruction::new(
                    Op::TypeStruct,
                    None,
                    Some(outer_ty),
                    vec![Operand::IdRef(inner_ty)],
                ),
            ),
        ]);
        let air_ty = AirType::Struct(vec![AirMember {
            offset: 0,
            ty: AirType::Struct(vec![
                AirMember {
                    offset: 0,
                    ty: AirType::Scalar(AirScalar::UChar),
                },
                AirMember {
                    offset: 0,
                    ty: AirType::Scalar(AirScalar::UChar),
                },
            ]),
        }]);

        assert!(!air_type_matches_existing(&defs, outer_ty, &air_ty, None));
    }
}
