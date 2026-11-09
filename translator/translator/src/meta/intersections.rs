#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AirIntersectionInstancing {
    None,
    SingleLevel,
    MultiLevel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AirIntersectionResultField {
    IntersectionType,
    Distance,
    PrimitiveId,
    GeometryId,
    OpaquePointer,
    InstanceId,
    UserInstanceId,
    InstanceLevel,
    Barycentrics,
    FrontFacing,
    CurveParameter,
    WorldSpaceVector(u8),
}

impl AirIntersectionResultField {
    pub fn llvm_type(self) -> &'static str {
        match self {
            Self::IntersectionType | Self::PrimitiveId | Self::GeometryId => "i32",
            Self::Distance => "float",
            Self::OpaquePointer => "ptr addrspace(1)",
            Self::InstanceId | Self::UserInstanceId => "i32",
            Self::InstanceLevel => "i8",
            Self::Barycentrics => "<2 x float>",
            Self::FrontFacing => "i1",
            Self::CurveParameter => "float",
            Self::WorldSpaceVector(_) => "<3 x float>",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AirIntersectionFamily {
    pub intersection_function_buffer: bool,
    pub instancing: AirIntersectionInstancing,
    pub triangle_data: bool,
    pub curve_data: bool,
    pub world_space_data: bool,
    pub user_data: bool,
    pub primitive_motion: bool,
    pub instance_motion: bool,
    pub extended_limits: bool,
}

impl AirIntersectionFamily {
    pub fn parse(callee: &str) -> Result<Option<Self>, String> {
        let Some(suffix) = callee.strip_prefix("air.intersect.") else {
            return Ok(None);
        };
        if suffix.is_empty() {
            return Err("AIR intersection symbol has an empty family suffix".into());
        }
        let mut family = Self {
            intersection_function_buffer: false,
            instancing: AirIntersectionInstancing::None,
            triangle_data: false,
            curve_data: false,
            world_space_data: false,
            user_data: false,
            primitive_motion: false,
            instance_motion: false,
            extended_limits: false,
        };
        for token in suffix.split('.') {
            match token {
                "intersection_function_buffer" if !family.intersection_function_buffer => {
                    family.intersection_function_buffer = true;
                }
                "instancing" if family.instancing == AirIntersectionInstancing::None => {
                    family.instancing = AirIntersectionInstancing::SingleLevel;
                }
                "multi_level_instancing"
                    if family.instancing == AirIntersectionInstancing::None =>
                {
                    family.instancing = AirIntersectionInstancing::MultiLevel;
                }
                "triangle_data" if !family.triangle_data => family.triangle_data = true,
                "curve_data" if !family.curve_data => family.curve_data = true,
                "world_space_data" if !family.world_space_data => {
                    family.world_space_data = true;
                }
                "user_data" if !family.user_data => family.user_data = true,
                "primitive_motion" if !family.primitive_motion => family.primitive_motion = true,
                "instance_motion" if !family.instance_motion => family.instance_motion = true,
                "extended_limits" if !family.extended_limits => family.extended_limits = true,
                _ => {
                    return Err(format!(
                        "AIR intersection symbol {callee} has an unknown or duplicate token {token}"
                    ));
                }
            }
        }
        if family.user_data && !family.intersection_function_buffer {
            return Err(format!(
                "AIR intersection symbol {callee} requests user_data without an intersection function buffer"
            ));
        }
        if family.instance_motion && family.instancing == AirIntersectionInstancing::None {
            return Err(format!(
                "AIR intersection symbol {callee} requests instance_motion without instancing"
            ));
        }
        if family.world_space_data && family.instancing == AirIntersectionInstancing::None {
            return Err(format!(
                "AIR intersection symbol {callee} requests world_space_data without instancing"
            ));
        }
        Ok(Some(family))
    }

    pub fn result_fields(&self) -> Vec<AirIntersectionResultField> {
        use AirIntersectionResultField as Field;
        let mut fields = vec![
            Field::IntersectionType,
            Field::Distance,
            Field::PrimitiveId,
            Field::GeometryId,
            Field::OpaquePointer,
        ];
        match self.instancing {
            AirIntersectionInstancing::None => {}
            AirIntersectionInstancing::SingleLevel => {
                fields.extend([Field::InstanceId, Field::UserInstanceId]);
            }
            AirIntersectionInstancing::MultiLevel => fields.push(Field::InstanceLevel),
        }
        if self.triangle_data {
            fields.extend([Field::Barycentrics, Field::FrontFacing]);
        }
        if self.curve_data {
            // measured from the compiler: the curve parameter follows the instance ids
            fields.push(Field::CurveParameter);
        }
        if self.world_space_data {
            fields.extend((0..8).map(Field::WorldSpaceVector));
        }
        fields
    }

    pub fn llvm_result_type(&self) -> String {
        format!(
            "{{ {} }}",
            self.result_fields()
                .into_iter()
                .map(AirIntersectionResultField::llvm_type)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    pub fn argument_count(&self) -> usize {
        18 + match self.instancing {
            AirIntersectionInstancing::None => 0,
            AirIntersectionInstancing::SingleLevel => 2,
            AirIntersectionInstancing::MultiLevel => 5,
        } + usize::from(self.intersection_function_buffer) * 4
            + usize::from(self.primitive_motion || self.instance_motion)
            + usize::from(self.user_data)
    }

    pub fn intersection_table_argument_index(&self) -> usize {
        5 + usize::from(self.instancing != AirIntersectionInstancing::None)
            + usize::from(self.primitive_motion || self.instance_motion)
    }

    pub fn opaque_triangle_removed_argument_indices(&self) -> Option<Vec<usize>> {
        self.intersection_function_buffer.then(|| {
            let table = self.intersection_table_argument_index();
            let mut removed = vec![table + 1, table + 2];
            if self.user_data {
                removed.push(table + 3);
            }
            removed.extend([self.argument_count() - 2, self.argument_count() - 1]);
            removed
        })
    }

    pub fn result_field(&self, index: usize) -> Option<AirIntersectionResultField> {
        self.result_fields().get(index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_data_adds_one_float_after_the_instance_ids() {
        let family = AirIntersectionFamily::parse("air.intersect.instancing.curve_data").unwrap().unwrap();
        assert!(family.curve_data && !family.triangle_data);
        assert_eq!(family.argument_count(), 20);
        assert_eq!(
            family.llvm_result_type(),
            "{ i32, float, i32, i32, ptr addrspace(1), i32, i32, float }"
        );
        assert_eq!(family.result_field(7), Some(AirIntersectionResultField::CurveParameter));
        let both = AirIntersectionFamily::parse("air.intersect.triangle_data.curve_data").unwrap().unwrap();
        assert_eq!(both.result_fields().len(), 8);
        assert!(AirIntersectionFamily::parse("air.intersect.curve_data.curve_data").is_err());
    }

    #[test]
    fn parses_composed_multi_level_callback_family() {
        let family = AirIntersectionFamily::parse(
            "air.intersect.intersection_function_buffer.multi_level_instancing.triangle_data.world_space_data.user_data.primitive_motion.instance_motion",
        )
        .unwrap()
        .unwrap();
        assert_eq!(family.instancing, AirIntersectionInstancing::MultiLevel);
        assert_eq!(family.argument_count(), 29);
        assert_eq!(family.intersection_table_argument_index(), 7);
        assert_eq!(
            family.opaque_triangle_removed_argument_indices(),
            Some(vec![8, 9, 10, 27, 28])
        );
        assert_eq!(family.result_fields().len(), 16);
        assert_eq!(
            family.llvm_result_type(),
            "{ i32, float, i32, i32, ptr addrspace(1), i8, <2 x float>, i1, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float>, <3 x float> }"
        );
    }

    #[test]
    fn parses_single_level_base_and_extended_limits_without_shape_changes() {
        let base = AirIntersectionFamily::parse("air.intersect.instancing")
            .unwrap()
            .unwrap();
        let extended = AirIntersectionFamily::parse("air.intersect.instancing.extended_limits")
            .unwrap()
            .unwrap();
        assert_eq!(base.argument_count(), 20);
        assert_eq!(base.intersection_table_argument_index(), 6);
        assert_eq!(base.result_fields(), extended.result_fields());
        assert_eq!(extended.argument_count(), 20);
    }

    #[test]
    fn rejects_contradictory_or_unknown_families() {
        assert!(AirIntersectionFamily::parse("air.intersect.user_data").is_err());
        assert!(AirIntersectionFamily::parse("air.intersect.world_space_data").is_err());
        assert!(AirIntersectionFamily::parse("air.intersect.triangle_data.mystery").is_err());
        assert_eq!(AirIntersectionFamily::parse("air.foo").unwrap(), None);
    }
}
