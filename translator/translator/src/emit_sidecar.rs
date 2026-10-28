use crate::fxhash::{HashMap, HashSet};
use spirv::Word;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EmitSidecar {
    pub(crate) all_device_buffers_raw: bool,
    pub(crate) construct_cross_binding_addresses: bool,
    pub(crate) ordinary_plan_rejected_functions: HashSet<String>,
    pub(crate) ownership_plan_rejected_functions: HashSet<String>,
    pub(crate) post_lowering_cfg_construction_functions: HashSet<String>,
    pub(crate) air_data_layout: Option<crate::layout::AirDataLayout>,
    pub(crate) air_struct_offsets: HashMap<Word, Vec<u32>>,
    pub(crate) air_struct_layout_mappings: Vec<AirStructLayoutMapping>,
    pub(crate) flat_raw_buffer_params: HashSet<u32>,
    pub(crate) buffer_address_words: Vec<BufferAddressWord>,
    pub(crate) buffer_access_offsets: Vec<BufferAccessOffset>,
    pub(crate) buffer_access_affine_offsets: Vec<BufferAccessAffineOffset>,
    pub(crate) buffer_root_source_types: HashMap<Word, Word>,
    pub(crate) buffer_pointer_field_loads: Vec<BufferPointerFieldLoad>,
    pub(crate) buffer_pointer_dynamic_field_loads: Vec<BufferPointerDynamicFieldLoad>,
    pub(crate) buffer_pointer_heap_loads: Vec<BufferPointerHeapLoad>,
    pub(crate) device_handle_heap_loads: Vec<DeviceHandleHeapLoad>,
    pub(crate) local_pointer_field_stores: Vec<LocalPointerFieldStore>,
    pub(crate) local_pointer_field_loads: Vec<LocalPointerFieldLoad>,
    pub(crate) local_pointer_dynamic_field_loads: Vec<LocalPointerDynamicFieldLoad>,
    pub(crate) aggregate_pointer_values: Vec<AggregatePointerValue>,
    pub(crate) aliased_imageblock_staging: Option<Word>,
    pub(crate) air_access_aligns: HashMap<Word, (u32, u32)>,
}

#[derive(Debug)]
pub(crate) struct EmissionFailure {
    pub(crate) error: String,
    pub(crate) rejected: Box<EmissionRejections>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EmissionRejections {
    pub(crate) raw_buffer_layout_required: bool,
    pub(crate) ordinary_plan_functions: HashSet<String>,
    pub(crate) ownership_plan_functions: HashSet<String>,
    pub(crate) cursor_call_sites: HashSet<(String, String)>,
}

impl EmissionFailure {
    pub(crate) fn from_error(error: String) -> Self {
        EmissionFailure {
            error,
            rejected: Box::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AirStructLayoutMapping {
    pub(crate) param_index: u32,
    pub(crate) struct_ty: Option<Word>,
    pub(crate) status: AirStructLayoutMappingStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AirStructLayoutMappingStatus {
    MappedNatural,
    MappedExplicit,
    ParameterMissing,
    ParameterIsNotPointer,
    MetadataIsNotStruct,
    EmittedIsUntypedBuffer,
    EmittedShapeMismatch,
    NonIncreasingOffsets,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferAddressWord {
    pub(crate) id: Word,
    pub(crate) param_index: u32,
    pub(crate) component: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferAccessOffset {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) byte_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferAccessAffineOffset {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) constant: u64,
    pub(crate) terms: Vec<(Word, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferPointerFieldLoad {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) byte_offset: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceHandleHeapLoad {
    pub(crate) id: Word,
    pub(crate) slot: Word,
    pub(crate) sampler: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferPointerHeapLoad {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) byte_offset: u64,
    pub(crate) slot: Word,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferPointerDynamicFieldLoad {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) byte_offset: u64,
    pub(crate) index: Word,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalPointerFieldStore {
    pub(crate) id: Word,
    pub(crate) source: Word,
    pub(crate) root: Word,
    pub(crate) indices: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalPointerFieldLoad {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) indices: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocalPointerDynamicFieldLoad {
    pub(crate) id: Word,
    pub(crate) root: Word,
    pub(crate) prefix: Vec<u32>,
    pub(crate) index: Word,
    pub(crate) suffix: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AggregatePointerValue {
    pub(crate) aggregate: Word,
    pub(crate) source: Word,
    pub(crate) indices: Vec<u32>,
}

impl EmitSidecar {
    pub(crate) fn referenced_ids(&self) -> HashSet<Word> {
        let mut ids = self
            .buffer_address_words
            .iter()
            .map(|fact| fact.id)
            .collect::<HashSet<_>>();
        for fact in &self.buffer_access_offsets {
            ids.extend([fact.id, fact.root]);
        }
        for fact in &self.buffer_access_affine_offsets {
            ids.extend([fact.id, fact.root]);
            ids.extend(fact.terms.iter().map(|(index, _)| *index));
        }
        for fact in &self.buffer_pointer_field_loads {
            ids.extend([fact.id, fact.root]);
        }
        for fact in &self.buffer_pointer_dynamic_field_loads {
            ids.extend([fact.id, fact.root, fact.index]);
        }
        for fact in &self.buffer_pointer_heap_loads {
            ids.extend([fact.id, fact.root, fact.slot]);
        }
        for fact in &self.device_handle_heap_loads {
            ids.extend([fact.id, fact.slot]);
        }
        for fact in &self.local_pointer_field_stores {
            ids.extend([fact.id, fact.source, fact.root]);
        }
        for fact in &self.local_pointer_field_loads {
            ids.extend([fact.id, fact.root]);
        }
        for fact in &self.local_pointer_dynamic_field_loads {
            ids.extend([fact.id, fact.root, fact.index]);
        }
        for fact in &self.aggregate_pointer_values {
            ids.extend([fact.aggregate, fact.source]);
        }
        ids.extend(self.aliased_imageblock_staging);
        ids.extend(self.air_struct_offsets.keys().copied());
        ids.extend(
            self.air_struct_layout_mappings
                .iter()
                .filter_map(|mapping| mapping.struct_ty),
        );
        for (&root, &source_ty) in &self.buffer_root_source_types {
            ids.extend([root, source_ty]);
        }
        ids
    }

    pub(crate) fn remap_ids(&mut self, remap: &HashMap<Word, Word>) {
        self.b78_remap_air_access_aligns(remap, false);
        let replace = |id: &mut Word| {
            if let Some(replacement) = remap.get(id) {
                *id = *replacement;
            }
        };
        for fact in &mut self.buffer_address_words {
            replace(&mut fact.id);
        }
        for fact in &mut self.buffer_access_offsets {
            replace(&mut fact.id);
            replace(&mut fact.root);
        }
        for fact in &mut self.buffer_access_affine_offsets {
            replace(&mut fact.id);
            replace(&mut fact.root);
            for (index, _) in &mut fact.terms {
                replace(index);
            }
        }
        for fact in &mut self.buffer_pointer_field_loads {
            replace(&mut fact.id);
            replace(&mut fact.root);
        }
        for fact in &mut self.buffer_pointer_dynamic_field_loads {
            replace(&mut fact.id);
            replace(&mut fact.root);
            replace(&mut fact.index);
        }
        for fact in &mut self.buffer_pointer_heap_loads {
            replace(&mut fact.id);
            replace(&mut fact.root);
            replace(&mut fact.slot);
        }
        for fact in &mut self.device_handle_heap_loads {
            replace(&mut fact.id);
            replace(&mut fact.slot);
        }
        for fact in &mut self.local_pointer_field_stores {
            replace(&mut fact.id);
            replace(&mut fact.source);
            replace(&mut fact.root);
        }
        for fact in &mut self.local_pointer_field_loads {
            replace(&mut fact.id);
            replace(&mut fact.root);
        }
        for fact in &mut self.local_pointer_dynamic_field_loads {
            replace(&mut fact.id);
            replace(&mut fact.root);
            replace(&mut fact.index);
        }
        for fact in &mut self.aggregate_pointer_values {
            replace(&mut fact.aggregate);
            replace(&mut fact.source);
        }
        if let Some(id) = &mut self.aliased_imageblock_staging {
            replace(id);
        }
        for mapping in &mut self.air_struct_layout_mappings {
            if let Some(struct_ty) = &mut mapping.struct_ty {
                replace(struct_ty);
            }
        }
        self.air_struct_offsets = self
            .air_struct_offsets
            .iter()
            .map(|(&id, offsets)| (remap.get(&id).copied().unwrap_or(id), offsets.clone()))
            .collect();
        self.buffer_root_source_types = self
            .buffer_root_source_types
            .iter()
            .map(|(&root, &source_ty)| {
                (
                    remap.get(&root).copied().unwrap_or(root),
                    remap.get(&source_ty).copied().unwrap_or(source_ty),
                )
            })
            .collect();
    }

    fn b78_remap_air_access_aligns(&mut self, remap: &HashMap<Word, Word>, clone: bool) {
        if self.air_access_aligns.is_empty() || remap.is_empty() {
            return;
        }
        let moved: Vec<(Word, Word, (u32, u32))> = self
            .air_access_aligns
            .iter()
            .filter_map(|(k, h)| Some((*k, *remap.get(k)?, *h)))
            .collect();
        if !clone {
            for (k, _, _) in &moved {
                self.air_access_aligns.remove(k);
            }
        }
        for (_, to, h) in moved {
            let merged = match self.air_access_aligns.get(&to) {
                None => h,
                Some(&(c, b)) if c != 0 && h.0 != 0 && b == h.1 => (c.min(h.0), b),
                Some(_) => (0, 0),
            };
            self.air_access_aligns.insert(to, merged);
        }
    }

    pub(crate) fn clone_inlined_facts(&mut self, remap: &HashMap<Word, Word>) {
        self.b78_remap_air_access_aligns(remap, true);
        let clones = self
            .buffer_access_offsets
            .iter()
            .filter_map(|fact| {
                Some(BufferAccessOffset {
                    id: remap.get(&fact.id).copied()?,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    byte_offset: fact.byte_offset,
                })
            })
            .collect::<Vec<_>>();
        self.buffer_access_offsets.extend(clones);
        let clones = self
            .buffer_access_affine_offsets
            .iter()
            .filter_map(|fact| {
                Some(BufferAccessAffineOffset {
                    id: remap.get(&fact.id).copied()?,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    constant: fact.constant,
                    terms: fact
                        .terms
                        .iter()
                        .map(|(index, stride)| {
                            (remap.get(index).copied().unwrap_or(*index), *stride)
                        })
                        .collect(),
                })
            })
            .collect::<Vec<_>>();
        self.buffer_access_affine_offsets.extend(clones);
        let clones = self
            .local_pointer_field_stores
            .iter()
            .filter_map(|fact| {
                Some(LocalPointerFieldStore {
                    id: remap.get(&fact.id).copied().unwrap_or(fact.id),
                    source: remap.get(&fact.source).copied().unwrap_or(fact.source),
                    root: remap.get(&fact.root).copied()?,
                    indices: fact.indices.clone(),
                })
            })
            .collect::<Vec<_>>();
        self.local_pointer_field_stores.extend(clones);
        let clones = self
            .buffer_pointer_field_loads
            .iter()
            .filter_map(|fact| {
                Some(BufferPointerFieldLoad {
                    id: remap.get(&fact.id).copied()?,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    byte_offset: fact.byte_offset,
                })
            })
            .collect::<Vec<_>>();
        self.buffer_pointer_field_loads.extend(clones);
        let clones = self
            .buffer_pointer_dynamic_field_loads
            .iter()
            .filter_map(|fact| {
                Some(BufferPointerDynamicFieldLoad {
                    id: remap.get(&fact.id).copied()?,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    byte_offset: fact.byte_offset,
                    index: remap.get(&fact.index).copied().unwrap_or(fact.index),
                })
            })
            .collect::<Vec<_>>();
        self.buffer_pointer_dynamic_field_loads.extend(clones);
        let clones = self
            .local_pointer_field_loads
            .iter()
            .filter_map(|fact| {
                let id = remap.get(&fact.id).copied()?;
                Some(LocalPointerFieldLoad {
                    id,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    indices: fact.indices.clone(),
                })
            })
            .collect::<Vec<_>>();
        self.local_pointer_field_loads.extend(clones);
        let clones = self
            .local_pointer_dynamic_field_loads
            .iter()
            .filter_map(|fact| {
                let id = remap.get(&fact.id).copied()?;
                Some(LocalPointerDynamicFieldLoad {
                    id,
                    root: remap.get(&fact.root).copied().unwrap_or(fact.root),
                    prefix: fact.prefix.clone(),
                    index: remap.get(&fact.index).copied().unwrap_or(fact.index),
                    suffix: fact.suffix.clone(),
                })
            })
            .collect::<Vec<_>>();
        self.local_pointer_dynamic_field_loads.extend(clones);
        let clones = self
            .aggregate_pointer_values
            .iter()
            .filter_map(|fact| {
                Some(AggregatePointerValue {
                    aggregate: remap.get(&fact.aggregate).copied()?,
                    source: remap.get(&fact.source).copied().unwrap_or(fact.source),
                    indices: fact.indices.clone(),
                })
            })
            .collect::<Vec<_>>();
        self.aggregate_pointer_values.extend(clones);
    }

    pub(crate) fn remap_instantiated_local_pointer_field_store_sources(
        &mut self,
        remap: &HashMap<Word, Word>,
    ) {
        if std::env::var_os("NVMTL_NO_B78_DW3D").is_some() {
            return self.remap_local_pointer_field_store_sources(remap);
        }
        for fact in &mut self.local_pointer_field_stores {
            if remap.contains_key(&fact.root) {
                continue;
            }
            if let Some(source) = remap.get(&fact.source) {
                fact.source = *source;
            }
        }
    }

    pub(crate) fn remap_local_pointer_field_store_sources(&mut self, remap: &HashMap<Word, Word>) {
        for fact in &mut self.local_pointer_field_stores {
            if let Some(source) = remap.get(&fact.source) {
                fact.source = *source;
            }
        }
    }
}

#[cfg(test)]
mod b78_tcg_dw3d_tests {
    use super::*;

    fn two_sites(old: bool) -> Vec<(Word, Word)> {
        let mut sidecar = EmitSidecar {
            local_pointer_field_stores: vec![LocalPointerFieldStore {
                id: 7,
                source: 10,
                root: 20,
                indices: vec![1],
            }],
            ..EmitSidecar::default()
        };
        for (arg, root) in [(101, 120), (201, 220)] {
            let remap = crate::fxhash::map_from([(10, arg), (20, root)]);
            sidecar.clone_inlined_facts(&remap);
            if old {
                sidecar.remap_local_pointer_field_store_sources(&remap);
            } else {
                sidecar.remap_instantiated_local_pointer_field_store_sources(&remap);
            }
        }
        sidecar
            .local_pointer_field_stores
            .iter()
            .filter(|f| f.root != 20)
            .map(|f| (f.root, f.source))
            .collect()
    }

    #[test]
    fn b78_dw3d_the_second_call_site_stores_its_own_pointer() {
        assert_eq!(two_sites(false), vec![(120, 101), (220, 201)]);
    }

    #[test]
    fn b78_dw3d_the_old_remap_stores_the_first_sites_pointer_at_the_second() {
        assert_eq!(two_sites(true), vec![(120, 101), (220, 101)]);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EmittedSpirv {
    pub(crate) module: crate::spirv_module::Module,
    pub(crate) sidecar: EmitSidecar,
}

impl EmittedSpirv {
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.module
            .assemble()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inlining_clones_load_facts_and_remaps_store_sources() {
        let mut sidecar = EmitSidecar {
            ordinary_plan_rejected_functions: HashSet::default(),
            ownership_plan_rejected_functions: HashSet::default(),
            post_lowering_cfg_construction_functions: HashSet::default(),
            buffer_access_offsets: vec![BufferAccessOffset {
                id: 82,
                root: 92,
                byte_offset: 28,
            }],
            buffer_pointer_field_loads: vec![BufferPointerFieldLoad {
                id: 80,
                root: 90,
                byte_offset: 16,
            }],
            buffer_pointer_dynamic_field_loads: vec![BufferPointerDynamicFieldLoad {
                id: 81,
                root: 91,
                byte_offset: 8,
                index: 71,
            }],
            local_pointer_field_stores: vec![LocalPointerFieldStore {
                id: 10,
                source: 20,
                root: 40,
                indices: vec![1, 2],
            }],
            local_pointer_field_loads: vec![LocalPointerFieldLoad {
                id: 30,
                root: 40,
                indices: vec![1, 2],
            }],
            local_pointer_dynamic_field_loads: vec![LocalPointerDynamicFieldLoad {
                id: 50,
                root: 60,
                prefix: vec![3],
                index: 70,
                suffix: vec![4],
            }],
            ..EmitSidecar::default()
        };
        let remap = crate::fxhash::map_from([
            (20, 120),
            (30, 130),
            (40, 140),
            (50, 150),
            (60, 160),
            (80, 180),
            (90, 190),
            (81, 181),
            (91, 191),
            (82, 182),
            (92, 192),
        ]);

        sidecar.clone_inlined_facts(&remap);
        sidecar.remap_local_pointer_field_store_sources(&remap);

        assert_eq!(
            sidecar.buffer_access_offsets,
            vec![
                BufferAccessOffset {
                    id: 82,
                    root: 92,
                    byte_offset: 28,
                },
                BufferAccessOffset {
                    id: 182,
                    root: 192,
                    byte_offset: 28,
                },
            ]
        );

        assert_eq!(
            sidecar.buffer_pointer_field_loads,
            vec![
                BufferPointerFieldLoad {
                    id: 80,
                    root: 90,
                    byte_offset: 16,
                },
                BufferPointerFieldLoad {
                    id: 180,
                    root: 190,
                    byte_offset: 16,
                },
            ]
        );
        assert_eq!(
            sidecar.buffer_pointer_dynamic_field_loads,
            vec![
                BufferPointerDynamicFieldLoad {
                    id: 81,
                    root: 91,
                    byte_offset: 8,
                    index: 71,
                },
                BufferPointerDynamicFieldLoad {
                    id: 181,
                    root: 191,
                    byte_offset: 8,
                    index: 71,
                },
            ]
        );

        assert_eq!(
            sidecar.local_pointer_field_stores,
            vec![
                LocalPointerFieldStore {
                    id: 10,
                    source: 120,
                    root: 40,
                    indices: vec![1, 2],
                },
                LocalPointerFieldStore {
                    id: 10,
                    source: 120,
                    root: 140,
                    indices: vec![1, 2],
                },
            ]
        );
        assert_eq!(
            sidecar.local_pointer_field_loads,
            vec![
                LocalPointerFieldLoad {
                    id: 30,
                    root: 40,
                    indices: vec![1, 2],
                },
                LocalPointerFieldLoad {
                    id: 130,
                    root: 140,
                    indices: vec![1, 2],
                },
            ]
        );
        assert_eq!(
            sidecar.local_pointer_dynamic_field_loads,
            vec![
                LocalPointerDynamicFieldLoad {
                    id: 50,
                    root: 60,
                    prefix: vec![3],
                    index: 70,
                    suffix: vec![4],
                },
                LocalPointerDynamicFieldLoad {
                    id: 150,
                    root: 160,
                    prefix: vec![3],
                    index: 70,
                    suffix: vec![4],
                },
            ]
        );
    }

    #[test]
    fn whole_module_substitution_remaps_every_sidecar_id_field() {
        let mut sidecar = EmitSidecar {
            all_device_buffers_raw: false,
            construct_cross_binding_addresses: false,
            ordinary_plan_rejected_functions: HashSet::default(),
            ownership_plan_rejected_functions: HashSet::default(),
            post_lowering_cfg_construction_functions: HashSet::default(),
            air_data_layout: None,
            air_struct_offsets: crate::fxhash::map_from([(5, vec![0, 16])]),
            air_struct_layout_mappings: vec![AirStructLayoutMapping {
                param_index: 2,
                struct_ty: Some(5),
                status: AirStructLayoutMappingStatus::MappedNatural,
            }],
            flat_raw_buffer_params: crate::fxhash::set_from([2]),
            buffer_address_words: vec![BufferAddressWord {
                id: 10,
                param_index: 2,
                component: 1,
            }],
            buffer_access_offsets: vec![BufferAccessOffset {
                id: 16,
                root: 17,
                byte_offset: 28,
            }],
            buffer_access_affine_offsets: vec![BufferAccessAffineOffset {
                id: 23,
                root: 24,
                constant: 32,
                terms: vec![(25, 48)],
            }],
            buffer_root_source_types: crate::fxhash::map_from([(18, 19)]),
            buffer_pointer_field_loads: vec![BufferPointerFieldLoad {
                id: 11,
                root: 12,
                byte_offset: 24,
            }],
            buffer_pointer_dynamic_field_loads: vec![BufferPointerDynamicFieldLoad {
                id: 13,
                root: 14,
                byte_offset: 16,
                index: 15,
            }],
            buffer_pointer_heap_loads: vec![BufferPointerHeapLoad {
                id: 60,
                root: 61,
                byte_offset: 0,
                slot: 62,
            }],
            device_handle_heap_loads: vec![DeviceHandleHeapLoad {
                id: 63,
                slot: 64,
                sampler: true,
            }],
            local_pointer_field_stores: vec![LocalPointerFieldStore {
                id: 20,
                source: 21,
                root: 22,
                indices: vec![3],
            }],
            local_pointer_field_loads: vec![LocalPointerFieldLoad {
                id: 30,
                root: 31,
                indices: vec![4],
            }],
            local_pointer_dynamic_field_loads: vec![LocalPointerDynamicFieldLoad {
                id: 40,
                root: 41,
                prefix: vec![5],
                index: 42,
                suffix: vec![6],
            }],
            aggregate_pointer_values: vec![AggregatePointerValue {
                aggregate: 43,
                source: 44,
                indices: vec![1, 0],
            }],
            aliased_imageblock_staging: Some(51),
            air_access_aligns: HashMap::default(),
        };
        let remap = crate::fxhash::map_from([
            (10, 110),
            (11, 111),
            (12, 112),
            (13, 113),
            (14, 114),
            (15, 115),
            (16, 116),
            (17, 117),
            (18, 118),
            (19, 119),
            (20, 120),
            (21, 121),
            (22, 122),
            (23, 123),
            (24, 124),
            (25, 125),
            (30, 130),
            (31, 131),
            (40, 140),
            (41, 141),
            (42, 142),
            (43, 143),
            (44, 144),
            (5, 105),
            (50, 150),
            (51, 151),
            (60, 160),
            (61, 161),
            (62, 162),
        ]);

        sidecar.remap_ids(&remap);
        assert_eq!(
            sidecar.aggregate_pointer_values,
            vec![AggregatePointerValue {
                aggregate: 143,
                source: 144,
                indices: vec![1, 0],
            }]
        );
        assert_eq!(
            sidecar.buffer_root_source_types,
            crate::fxhash::map_from([(118, 119)])
        );

        assert_eq!(sidecar.air_struct_offsets.get(&105), Some(&vec![0, 16]));
        assert!(!sidecar.air_struct_offsets.contains_key(&5));
        assert_eq!(sidecar.air_struct_layout_mappings[0].struct_ty, Some(105));
        assert_eq!(sidecar.buffer_address_words[0].id, 110);
        assert_eq!(sidecar.buffer_access_offsets[0].id, 116);
        assert_eq!(sidecar.buffer_access_offsets[0].root, 117);
        assert_eq!(sidecar.buffer_access_affine_offsets[0].id, 123);
        assert_eq!(sidecar.buffer_access_affine_offsets[0].root, 124);
        assert_eq!(
            sidecar.buffer_access_affine_offsets[0].terms,
            vec![(125, 48)]
        );
        assert_eq!(sidecar.buffer_pointer_field_loads[0].id, 111);
        assert_eq!(sidecar.buffer_pointer_field_loads[0].root, 112);
        assert_eq!(sidecar.buffer_pointer_dynamic_field_loads[0].id, 113);
        assert_eq!(sidecar.buffer_pointer_dynamic_field_loads[0].root, 114);
        assert_eq!(sidecar.buffer_pointer_dynamic_field_loads[0].index, 115);
        assert_eq!(sidecar.buffer_pointer_heap_loads[0].id, 160);
        assert_eq!(sidecar.buffer_pointer_heap_loads[0].root, 161);
        assert_eq!(sidecar.buffer_pointer_heap_loads[0].slot, 162);
        assert_eq!(sidecar.local_pointer_field_stores[0].id, 120);
        assert_eq!(sidecar.local_pointer_field_stores[0].source, 121);
        assert_eq!(sidecar.local_pointer_field_stores[0].root, 122);
        assert_eq!(sidecar.local_pointer_field_loads[0].id, 130);
        assert_eq!(sidecar.local_pointer_field_loads[0].root, 131);
        assert_eq!(sidecar.local_pointer_dynamic_field_loads[0].id, 140);
        assert_eq!(sidecar.local_pointer_dynamic_field_loads[0].root, 141);
        assert_eq!(sidecar.local_pointer_dynamic_field_loads[0].index, 142);
        assert_eq!(sidecar.aliased_imageblock_staging, Some(151));
    }
}
