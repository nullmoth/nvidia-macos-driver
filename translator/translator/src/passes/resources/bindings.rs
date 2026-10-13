use super::*;
use crate::reflect::DescriptorLayout;

pub(in crate::passes) fn decorate_binding(module: &mut Module, id: Word, set: u32, binding: u32) {
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(id),
            Operand::Decoration(Decoration::DescriptorSet),
            Operand::LiteralBit32(set),
        ],
    ));
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(id),
            Operand::Decoration(Decoration::Binding),
            Operand::LiteralBit32(binding),
        ],
    ));
}

pub(in crate::passes) fn decorate_input_attachment_index(
    module: &mut Module,
    id: Word,
    index: u32,
) {
    module.annotations.push(Instruction::new(
        Op::Decorate,
        None,
        None,
        vec![
            Operand::IdRef(id),
            Operand::Decoration(Decoration::InputAttachmentIndex),
            Operand::LiteralBit32(index),
        ],
    ));
}

pub(in crate::passes) fn allocate_static_sampler_binding(
    module: &Module,
    layout: DescriptorLayout,
) -> Option<u32> {
    let occupied = crate::spirv_module::descriptor_bindings_in_set(module, layout.set);
    (layout.samplers.start..layout.samplers.end).find(|binding| !occupied.contains(binding))
}

pub(in crate::passes) fn allocate_default_texture_binding(
    module: &Module,
    layout: DescriptorLayout,
) -> Option<u32> {
    let occupied = crate::spirv_module::descriptor_bindings_in_set(module, layout.set);
    (layout.sampled_textures.start..layout.sampled_textures.end)
        .find(|binding| !occupied.contains(binding))
}

pub(in crate::passes) fn texture_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout
        .sampled_texture_binding(index)
        .ok_or_else(|| format!("Metal texture index {index} exceeds the descriptor ABI band"))
}

pub(in crate::passes) fn storage_texture_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout.storage_texture_binding(index).ok_or_else(|| {
        format!("Metal storage-texture index {index} exceeds the descriptor ABI band")
    })
}

pub(in crate::passes) fn buffer_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout
        .buffer_binding(index)
        .ok_or_else(|| format!("Metal buffer index {index} exceeds the descriptor ABI band"))
}

pub(in crate::passes) fn sampler_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout
        .sampler_binding(index)
        .ok_or_else(|| format!("Metal sampler index {index} exceeds the descriptor ABI band"))
}

pub(in crate::passes) fn embedded_sampler_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout.samplers.binding(index).ok_or_else(|| {
        format!(
            "embedded argument-buffer sampler index {index} exceeds the {} sampler descriptors in \
             the ABI band",
            layout.samplers.len().unwrap_or(0)
        )
    })
}

pub(in crate::passes) fn color_input_resource_binding(
    layout: DescriptorLayout,
    index: u32,
) -> Result<u32, String> {
    layout
        .color_input_binding(index)
        .ok_or_else(|| format!("Metal color-input index {index} exceeds the descriptor ABI band"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescriptorClass {
    StorageBuffer,
    UniformBuffer,
    Sampler,
    SampledImage,
    UniformTexelBuffer,
    CombinedImageSampler,
    StorageImage,
    StorageTexelBuffer,
    InputAttachment,
    AccelerationStructure,
}

#[cfg(test)]
pub(in crate::passes) fn validate_descriptor_binding_classes(
    module: &Module,
    layout: DescriptorLayout,
) -> Result<(), String> {
    validate_descriptor_binding_classes_with_ray_table(module, layout, None)
}

pub(in crate::passes) fn validate_descriptor_binding_classes_with_ray_table(
    module: &Module,
    layout: DescriptorLayout,
    ray_table_binding: Option<u32>,
) -> Result<(), String> {
    let names = module
        .debug_names
        .iter()
        .filter_map(|instruction| {
            if instruction.class.opcode != Op::Name {
                return None;
            }
            let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(name))) =
                (instruction.operands.first(), instruction.operands.get(1))
            else {
                return None;
            };
            Some((*id, name.as_str()))
        })
        .collect::<HashMap<_, _>>();
    let label = |id: Word| match names.get(&id) {
        Some(name) => format!("%{id} ({name})"),
        None => format!("%{id}"),
    };
    let definitions = module
        .types_global_values
        .iter()
        .filter_map(|instruction| instruction.result_id.map(|result| (result, instruction)))
        .collect::<HashMap<_, _>>();
    let mut sets = HashMap::<Word, u32>::default();
    let mut bindings = HashMap::<Word, u32>::default();
    for annotation in &module.annotations {
        if annotation.class.opcode != Op::Decorate {
            continue;
        }
        let (
            Some(Operand::IdRef(target)),
            Some(Operand::Decoration(decoration)),
            Some(Operand::LiteralBit32(value)),
        ) = (
            annotation.operands.first(),
            annotation.operands.get(1),
            annotation.operands.get(2),
        )
        else {
            continue;
        };
        match decoration {
            Decoration::DescriptorSet => {
                if let Some(previous) = sets.insert(*target, *value) {
                    if previous != *value {
                        return Err(format!(
                            "descriptor variable {} has conflicting sets {previous} and {value}",
                            label(*target)
                        ));
                    }
                }
            }
            Decoration::Binding => {
                if let Some(previous) = bindings.insert(*target, *value) {
                    if previous != *value {
                        return Err(format!(
                            "descriptor variable {} has conflicting bindings {previous} and {value}",
                            label(*target)
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    fn classify(
        definitions: &HashMap<Word, &Instruction>,
        ty: Word,
        storage: Option<StorageClass>,
    ) -> Option<DescriptorClass> {
        let definition = definitions.get(&ty)?;
        match definition.class.opcode {
            Op::TypePointer => {
                let Operand::StorageClass(pointer_storage) = definition.operands.first()? else {
                    return None;
                };
                let Operand::IdRef(pointee) = definition.operands.get(1)? else {
                    return None;
                };
                classify(definitions, *pointee, Some(*pointer_storage))
            }
            Op::TypeArray | Op::TypeRuntimeArray => {
                let Operand::IdRef(element) = definition.operands.first()? else {
                    return None;
                };
                classify(definitions, *element, storage)
            }
            Op::TypeSampler => Some(DescriptorClass::Sampler),
            Op::TypeSampledImage => Some(DescriptorClass::CombinedImageSampler),
            Op::TypeImage => {
                let dimension = definition.operands.get(1);
                let storage = definition.operands.get(5) == Some(&Operand::LiteralBit32(2));
                if dimension == Some(&Operand::Dim(spirv::Dim::DimSubpassData)) {
                    Some(DescriptorClass::InputAttachment)
                } else if dimension == Some(&Operand::Dim(spirv::Dim::DimBuffer)) && storage {
                    Some(DescriptorClass::StorageTexelBuffer)
                } else if dimension == Some(&Operand::Dim(spirv::Dim::DimBuffer)) {
                    Some(DescriptorClass::UniformTexelBuffer)
                } else if storage {
                    Some(DescriptorClass::StorageImage)
                } else {
                    Some(DescriptorClass::SampledImage)
                }
            }
            Op::TypeAccelerationStructureKHR => Some(DescriptorClass::AccelerationStructure),
            _ if storage == Some(StorageClass::StorageBuffer) => {
                Some(DescriptorClass::StorageBuffer)
            }
            _ if storage == Some(StorageClass::Uniform) => Some(DescriptorClass::UniformBuffer),
            _ => None,
        }
    }

    let mut occupied = std::collections::BTreeMap::<(u32, u32), (Word, DescriptorClass)>::new();
    for variable in module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
    {
        let Some(id) = variable.result_id else {
            continue;
        };
        let Some(binding) = bindings.get(&id).copied() else {
            continue;
        };
        let set = sets
            .get(&id)
            .copied()
            .ok_or_else(|| format!("descriptor variable {} has no descriptor set", label(id)))?;
        let class = variable
            .result_type
            .and_then(|ty| classify(&definitions, ty, None))
            .ok_or_else(|| {
                format!(
                    "descriptor variable {} has an unknown descriptor class",
                    label(id)
                )
            })?;
        let allowed = (class == DescriptorClass::StorageBuffer
            && ray_table_binding == Some(binding))
            || match class {
                DescriptorClass::StorageBuffer
                | DescriptorClass::UniformBuffer
                | DescriptorClass::AccelerationStructure => {
                    layout.buffers.contains(binding) || layout.synthetic.contains(binding)
                }
                DescriptorClass::Sampler => layout.samplers.contains(binding),
                DescriptorClass::SampledImage
                | DescriptorClass::UniformTexelBuffer
                | DescriptorClass::CombinedImageSampler => {
                    layout.sampled_textures.contains(binding)
                }
                DescriptorClass::StorageImage | DescriptorClass::StorageTexelBuffer => {
                    layout.storage_textures.contains(binding)
                        || layout.imageblocks.contains(binding)
                        || layout.fragment_imageblocks.contains(binding)
                }
                DescriptorClass::InputAttachment => layout.color_inputs.contains(binding),
            };
        let bindless_heap = set == crate::reflect::BINDLESS_HEAP_SET
            && ((binding == crate::reflect::BINDLESS_HEAP_BINDING && class == DescriptorClass::SampledImage)
                || (binding == crate::reflect::BINDLESS_SAMPLER_BINDING && class == DescriptorClass::Sampler)
                || (binding == crate::reflect::BINDLESS_STORAGE_BINDING && class == DescriptorClass::StorageImage)
                || (binding == crate::reflect::BINDLESS_UTEXEL_BINDING && class == DescriptorClass::UniformTexelBuffer)
                || (binding == crate::reflect::BINDLESS_STEXEL_BINDING && class == DescriptorClass::StorageTexelBuffer));
        if !bindless_heap && (set != layout.set || !allowed) {
            return Err(format!(
                "descriptor variable {} has class {class:?} at set {set} binding {binding}, outside its ABI band",
                label(id)
            ));
        }
        if let Some((previous, previous_class)) = occupied.insert((set, binding), (id, class)) {
            if previous_class != class {
                return Err(format!(
                    "descriptor set {set} binding {binding} is shared by {} ({previous_class:?}) and {} ({class:?})",
                    label(previous), label(id)
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_variable(module: &mut Module, id: Word, dimension: spirv::Dim, storage: bool) {
        let scalar = id + 100;
        let image = id + 200;
        let pointer = id + 300;
        module.types_global_values.push(type_inst(
            Op::TypeFloat,
            scalar,
            vec![Operand::LiteralBit32(32)],
        ));
        module.types_global_values.push(type_inst(
            Op::TypeImage,
            image,
            vec![
                Operand::IdRef(scalar),
                Operand::Dim(dimension),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(if storage { 2 } else { 1 }),
                Operand::ImageFormat(spirv::ImageFormat::Unknown),
            ],
        ));
        module.types_global_values.push(type_inst(
            Op::TypePointer,
            pointer,
            vec![
                Operand::StorageClass(StorageClass::UniformConstant),
                Operand::IdRef(image),
            ],
        ));
        module.types_global_values.push(Instruction::new(
            Op::Variable,
            Some(pointer),
            Some(id),
            vec![Operand::StorageClass(StorageClass::UniformConstant)],
        ));
    }

    #[test]
    fn static_sampler_allocator_is_bounded_by_its_descriptor_band() {
        let mut module = Module::new();
        let layout = DescriptorLayout::default();
        decorate_binding(&mut module, 1, layout.set, layout.sampled_textures.end - 1);
        decorate_binding(
            &mut module,
            2,
            layout.set,
            crate::reflect::COLOR_INPUT_BINDING_RANGE.start,
        );
        assert_eq!(
            allocate_static_sampler_binding(&module, layout),
            Some(layout.samplers.start)
        );

        for (offset, binding) in (layout.samplers.start..layout.samplers.end).enumerate() {
            decorate_binding(&mut module, 100 + offset as u32, layout.set, binding);
        }
        assert_eq!(allocate_static_sampler_binding(&module, layout), None);
    }

    #[test]
    fn null_texture_allocator_is_bounded_by_its_descriptor_band() {
        let mut module = Module::new();
        let layout = DescriptorLayout::default();
        decorate_binding(&mut module, 1, layout.set, layout.samplers.start);
        assert_eq!(
            allocate_default_texture_binding(&module, layout),
            Some(layout.sampled_textures.start)
        );

        for (offset, binding) in
            (layout.sampled_textures.start..layout.sampled_textures.end).enumerate()
        {
            decorate_binding(&mut module, 100 + offset as u32, layout.set, binding);
        }
        assert_eq!(allocate_default_texture_binding(&module, layout), None);
    }

    #[test]
    fn synthesized_allocators_scope_occupied_bindings_by_descriptor_set() {
        let mut module = Module::new();
        let layout = DescriptorLayout {
            set: 3,
            ..DescriptorLayout::default()
        };
        decorate_binding(&mut module, 1, 0, layout.samplers.start);
        decorate_binding(&mut module, 2, layout.set, layout.samplers.start + 1);
        decorate_binding(&mut module, 3, 0, layout.sampled_textures.start);
        decorate_binding(
            &mut module,
            4,
            layout.set,
            layout.sampled_textures.start + 1,
        );

        assert_eq!(
            allocate_static_sampler_binding(&module, layout),
            Some(layout.samplers.start)
        );
        assert_eq!(
            allocate_default_texture_binding(&module, layout),
            Some(layout.sampled_textures.start)
        );
    }

    #[test]
    fn descriptor_collision_oracle_distinguishes_images_from_texel_buffers() {
        for storage in [false, true] {
            let mut module = Module::new();
            image_variable(&mut module, 1, spirv::Dim::Dim2D, storage);
            image_variable(&mut module, 2, spirv::Dim::DimBuffer, storage);
            let layout = DescriptorLayout::default();
            let binding = if storage {
                layout.storage_textures.start
            } else {
                layout.sampled_textures.start
            };
            decorate_binding(&mut module, 1, layout.set, binding);
            decorate_binding(&mut module, 2, layout.set, binding);

            let error = validate_descriptor_binding_classes(&module, layout)
                .expect_err("image and texel-buffer descriptors cannot alias");
            assert!(error.contains("is shared by"), "{error}");
        }
    }
}
