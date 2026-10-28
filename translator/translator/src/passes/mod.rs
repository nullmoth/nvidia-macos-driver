use crate::fxhash::{HashMap, HashSet};
use crate::meta::{
    AirScalar, AirType, FragMeta, FragRole, KernMeta, KernRole, VertMeta, VertOutRole, VertRole,
};
use crate::reflect::{
    RuntimeSamplerState, RuntimeStorageImageState, StaticSamplerState,
    SAMPLER_ARGUMENT_COUNT_USIZE, TEXTURE_ARGUMENT_COUNT_USIZE,
    THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE,
};
use crate::spirv_module::{is_block_terminator, Block, Function, Instruction, Module, Operand};
use spirv::{
    BuiltIn, Decoration, Dim, FunctionControl, ImageFormat, MemorySemantics, Op, Scope,
    StorageClass, Word,
};
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Vertex,
    Fragment,
    Kernel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformOptions {
    pub descriptor_layout: crate::reflect::DescriptorLayout,
    pub kernel_local_size: [u32; 3],
    pub kernel_dispatch: Option<crate::reflect::KernelDispatch>,
    pub denorm_flush_to_zero_f32: bool,
    pub raster_sample_count: Option<u32>,
    pub runtime_sampler_states: [Option<RuntimeSamplerState>; SAMPLER_ARGUMENT_COUNT_USIZE],
    pub runtime_storage_image_states:
        [Option<RuntimeStorageImageState>; TEXTURE_ARGUMENT_COUNT_USIZE],
    pub vertex_amplification_count: u32,
    pub threadgroup_memory_lengths: [Option<u32>; THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE],
    /// Colour attachments bound as Metal A8Unorm (bit per location). Vulkan has no A8 format, so the plugin backs
    /// them with R8 and the fragment output's alpha is written to every lane; the red lane is what the target keeps.
    pub a8_render_target_mask: u32,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            descriptor_layout: crate::reflect::DescriptorLayout::default(),
            kernel_local_size: [64, 1, 1],
            kernel_dispatch: None,
            denorm_flush_to_zero_f32: false,
            raster_sample_count: None,
            vertex_amplification_count: 1,
            runtime_sampler_states: [None; SAMPLER_ARGUMENT_COUNT_USIZE],
            runtime_storage_image_states: [None; TEXTURE_ARGUMENT_COUNT_USIZE],
            threadgroup_memory_lengths: [None; THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE],
            a8_render_target_mask: 0,
        }
    }
}

impl TransformOptions {
    pub fn with_kernel_dispatch(
        mut self,
        dispatch: crate::reflect::KernelDispatch,
    ) -> Result<Self, String> {
        dispatch.validate()?;
        self.kernel_dispatch = Some(dispatch);
        Ok(self)
    }

    pub fn with_descriptor_layout(
        mut self,
        layout: crate::reflect::DescriptorLayout,
    ) -> Result<Self, crate::reflect::DescriptorLayoutError> {
        layout.validate()?;
        self.descriptor_layout = layout;
        Ok(self)
    }

    pub fn with_a8_render_targets(mut self, mask: u32) -> Self {
        self.a8_render_target_mask = mask;
        self
    }

    pub fn with_runtime_sampler(
        mut self,
        metal_index: u32,
        state: RuntimeSamplerState,
    ) -> Result<Self, String> {
        state.validate()?;
        let slot = usize::try_from(metal_index)
            .ok()
            .filter(|slot| *slot < SAMPLER_ARGUMENT_COUNT_USIZE)
            .ok_or_else(|| {
                format!(
                    "Metal sampler index {metal_index} exceeds runtime specialization range 0..{}",
                    SAMPLER_ARGUMENT_COUNT_USIZE
                )
            })?;
        self.runtime_sampler_states[slot] = Some(state);
        Ok(self)
    }

    pub(crate) fn validate_runtime_samplers(self) -> Result<(), String> {
        for (index, state) in self.runtime_sampler_states.iter().copied().enumerate() {
            if let Some(state) = state {
                state
                    .validate()
                    .map_err(|error| format!("runtime sampler {index}: {error}"))?;
            }
        }
        Ok(())
    }

    pub fn with_runtime_storage_image(
        mut self,
        metal_index: u32,
        state: RuntimeStorageImageState,
    ) -> Result<Self, String> {
        state.validate()?;
        let slot = usize::try_from(metal_index)
            .ok()
            .filter(|slot| *slot < TEXTURE_ARGUMENT_COUNT_USIZE)
            .ok_or_else(|| {
                format!(
                    "storage-image resource index {metal_index} exceeds runtime specialization range 0..{}",
                    TEXTURE_ARGUMENT_COUNT_USIZE
                )
            })?;
        self.runtime_storage_image_states[slot] = Some(state);
        Ok(self)
    }

    pub fn with_threadgroup_memory_length(
        mut self,
        metal_index: u32,
        length: u32,
    ) -> Result<Self, String> {
        let slot = usize::try_from(metal_index)
            .ok()
            .filter(|slot| *slot < THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE)
            .ok_or_else(|| {
                format!(
                    "Metal threadgroup buffer index {metal_index} exceeds the threadgroup argument range 0..{}",
                    THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE
                )
            })?;
        if length == 0 {
            return Err(format!(
                "threadgroup buffer {metal_index} was given a zero byte length"
            ));
        }
        self.threadgroup_memory_lengths[slot] = Some(length);
        Ok(self)
    }

    pub(crate) fn validate_runtime_storage_images(self) -> Result<(), String> {
        for (index, state) in self
            .runtime_storage_image_states
            .iter()
            .copied()
            .enumerate()
        {
            if let Some(state) = state {
                state
                    .validate()
                    .map_err(|error| format!("runtime storage image {index}: {error}"))?;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_kernel_dispatch_options(
    stage: Stage,
    options: TransformOptions,
) -> Result<(), String> {
    if !matches!(stage, Stage::Kernel) {
        return Ok(());
    }
    if options.kernel_local_size.contains(&0) {
        return Err("kernel LocalSize dimensions must be non-zero".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageComp {
    Float,
    Uint,
    Sint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeStorageImageUse {
    Read,
    Write,
    Atomic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FragmentImageblockFormat {
    image_format: ImageFormat,
    component: ImageComp,
    bits: u32,
    lanes: u32,
}

fn fragment_imageblock_format(type_name: &str) -> Option<FragmentImageblockFormat> {
    let (image_format, component, bits, lanes) = match type_name {
        "half" => (ImageFormat::R16f, ImageComp::Float, 16, 1),
        "half4" => (ImageFormat::Rgba16f, ImageComp::Float, 16, 4),
        "uchar4" => (ImageFormat::Rgba8ui, ImageComp::Uint, 8, 4),
        "ushort" => (ImageFormat::R16ui, ImageComp::Uint, 16, 1),
        _ => return None,
    };
    Some(FragmentImageblockFormat {
        image_format,
        component,
        bits,
        lanes,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::passes) enum SingletonType {
    FnVoid,
    Int8,
    Int16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::passes) struct ImageShape {
    pub(in crate::passes) dim: Dim,
    pub(in crate::passes) arrayed: bool,
    pub(in crate::passes) comp: ImageComp,
    pub(in crate::passes) multisampled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::passes) enum SynthCacheKey {
    Array { elem: Word, len_const: Word },
    ConstInt { int_ty: Word, value: i64 },
    ConstFloat { bits: u32 },
    ConstHalf { bits: u16 },
    IntType { bits: u32, signed: bool },
    VecType { elem: Word, lanes: u32 },
    SubgroupLocalInvocationIdInputVar,
}

struct AirCallBody {
    function: usize,
    dominance: spirv_cfg::BlockDominance,
}

#[derive(Clone, Copy)]
struct Mat8Nv {
    e: [Word; 2],
    elem: Word,
    block: usize,
}

struct Mat8NvPhi {
    phi: Word,
    ty: Word,
    c_elem: Word,
    kind: crate::air_intrinsics::Matrix16Element,
    acc_elem: Word,
    e: [Word; 2],
    block: usize,
}

#[derive(Clone, Copy)]
struct Mat8Fuse {
    rows: usize,
    ks: usize,
    a: [[Word; 2]; 2],
    b: [Word; 2],
    c: [Word; 2],
    out: [Word; 2],
    a_mem: Option<Mat8Mem>,
    b_mem: Option<Mat8Mem>,
}

#[derive(Clone, Copy)]
struct Mat8Mem {
    ptr: Word,
    stride: u32,
    key: Word,
}

#[derive(Clone, Copy)]
enum Mat8FuseRole {
    Skip,
    Lead(Mat8Fuse),
}

struct Ctx {
    module: Module,
    emit_sidecar: crate::emit_sidecar::EmitSidecar,
    stage: Stage,
    glsl_ext: Option<Word>,
    interface: Vec<Word>,
    new_globals: Vec<Instruction>,
    synth_cache: HashMap<SynthCacheKey, Word>,
    singleton_types: HashMap<SingletonType, Word>,
    struct_cache: HashMap<(Op, Option<Word>, Vec<Operand>), Word>,
    phase_value_types: Option<HashMap<Word, Word>>,
    phase_type_positions: Option<HashMap<Word, (bool, usize)>>,
    image_dims: HashMap<Word, (Dim, bool)>,
    image_comp: HashMap<Word, ImageComp>,
    image_multisampled: HashSet<Word>,
    image_storage: HashSet<Word>,
    image_array_vars: HashMap<Word, (Word, (Dim, bool), ImageComp, bool)>,
    bindless_heap_vars: HashMap<Word, Word>,
    address_table: Option<(Word, Word, Option<Word>)>,
    bound_buffer_vars: HashSet<Word>,
    null_image_values: HashSet<Word>,
    laid_out: HashSet<Word>,
    air_struct_offsets: HashMap<Word, Vec<u32>>,
    air_data_layout: Option<crate::layout::AirDataLayout>,
    descriptor_layout: crate::reflect::DescriptorLayout,
    kernel_local_size: [u32; 3],
    kernel_local_size_ids: Option<[Word; 3]>,
    kernel_workgroup_size_id: Option<Word>,
    kernel_dispatch: crate::reflect::KernelDispatch,
    raster_sample_count: Option<u32>,
    vertex_amplification_count: u32,
    runtime_sampler_states: [Option<RuntimeSamplerState>; SAMPLER_ARGUMENT_COUNT_USIZE],
    runtime_storage_image_states: [Option<RuntimeStorageImageState>; TEXTURE_ARGUMENT_COUNT_USIZE],
    threadgroup_memory_lengths: [Option<u32>; THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE],
    a8_render_target_mask: u32,
    a8_output_vars: HashSet<Word>,
    runtime_storage_image_values: HashMap<Word, (u32, RuntimeStorageImageState)>,
    applied_runtime_storage_image_indices: HashSet<u32>,
    default_sampler_var: Option<Word>,
    read_sampler_values: HashSet<Word>,
    air_call_body: Option<AirCallBody>,
    air_call_block: usize,
    mat8_nv: HashMap<(Word, u8), Mat8Nv>,
    mat8_nv_phis: Vec<Mat8NvPhi>,
    mat8_fuse: HashMap<Word, Mat8FuseRole>,
    mat8_cm: HashMap<(Word, Word), Word>,
    unsurfaced_embedded_resources: Vec<String>,
    pub(in crate::passes) variant_absent_texture_values: HashSet<Word>,
    placeholder_descriptor_vars: HashMap<Word, u32>,
    ray_instance_user_id_table: Option<Word>,
    sampler_states: HashMap<Word, StaticSamplerState>,
    specialized_runtime_sampler_values: HashSet<Word>,
    ambiguous_sampler_states: HashSet<Word>,
    default_null_image_vars: HashMap<(Dim, bool), Word>,
    implicit_imageblock_vars: HashMap<(u32, u32), (Word, Word, ImageFormat)>,
    fragment_imageblock_vars: HashMap<u32, (Word, Word)>,
    fragment_sample_positions_var: Option<Word>,
    uses_fragment_imageblock: bool,
    fragment_imageblock_coord_var: Option<Word>,
    writes_frag_depth: bool,
}

impl Ctx {
    #[cfg(test)]
    fn new(module: Module) -> Self {
        Self::with_options(module, Stage::Kernel, TransformOptions::default())
    }

    #[cfg(test)]
    fn with_options(module: Module, stage: Stage, options: TransformOptions) -> Self {
        Self::with_options_and_sidecar(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            stage,
            options,
        )
    }

    fn with_options_and_sidecar(
        module: Module,
        emit_sidecar: crate::emit_sidecar::EmitSidecar,
        stage: Stage,
        options: TransformOptions,
    ) -> Self {
        let mut module = module;
        module.sync_id_bound_from_instructions();
        let air_struct_offsets = emit_sidecar.air_struct_offsets.clone();
        let air_data_layout = emit_sidecar.air_data_layout.clone();
        Ctx {
            module,
            emit_sidecar,
            stage,
            glsl_ext: None,
            interface: vec![],
            new_globals: vec![],
            synth_cache: HashMap::default(),
            singleton_types: HashMap::default(),
            struct_cache: HashMap::default(),
            phase_value_types: None,
            phase_type_positions: None,
            image_dims: HashMap::default(),
            image_comp: HashMap::default(),
            image_multisampled: HashSet::default(),
            image_storage: HashSet::default(),
            image_array_vars: HashMap::default(),
            bindless_heap_vars: HashMap::default(),
            address_table: None,
            bound_buffer_vars: HashSet::default(),
            null_image_values: HashSet::default(),
            laid_out: HashSet::default(),
            air_struct_offsets,
            air_data_layout,
            descriptor_layout: options.descriptor_layout,
            kernel_local_size: options.kernel_local_size,
            kernel_local_size_ids: None,
            kernel_workgroup_size_id: None,
            kernel_dispatch: options
                .kernel_dispatch
                .unwrap_or_else(crate::reflect::KernelDispatch::safe_default),
            raster_sample_count: options.raster_sample_count,
            vertex_amplification_count: options.vertex_amplification_count,
            runtime_sampler_states: options.runtime_sampler_states,
            runtime_storage_image_states: options.runtime_storage_image_states,
            threadgroup_memory_lengths: options.threadgroup_memory_lengths,
            a8_render_target_mask: options.a8_render_target_mask,
            a8_output_vars: HashSet::default(),
            runtime_storage_image_values: HashMap::default(),
            applied_runtime_storage_image_indices: HashSet::default(),
            default_sampler_var: None,
            read_sampler_values: HashSet::default(),
            air_call_body: None,
            air_call_block: 0,
            mat8_nv: HashMap::default(),
            mat8_nv_phis: Vec::new(),
            mat8_fuse: HashMap::default(),
            mat8_cm: HashMap::default(),
            unsurfaced_embedded_resources: Vec::new(),
            variant_absent_texture_values: HashSet::default(),
            sampler_states: HashMap::default(),
            specialized_runtime_sampler_values: HashSet::default(),
            ambiguous_sampler_states: HashSet::default(),
            default_null_image_vars: HashMap::default(),
            placeholder_descriptor_vars: HashMap::default(),
            ray_instance_user_id_table: None,
            implicit_imageblock_vars: HashMap::default(),
            fragment_imageblock_vars: HashMap::default(),
            fragment_sample_positions_var: None,
            uses_fragment_imageblock: false,
            fragment_imageblock_coord_var: None,
            writes_frag_depth: false,
        }
    }

    fn add_capability(&mut self, capability: spirv::Capability) {
        if self.module.capabilities.iter().any(|instruction| {
            instruction.operands.first() == Some(&Operand::Capability(capability))
        }) {
            return;
        }
        self.module.capabilities.push(Instruction::new(
            Op::Capability,
            None,
            None,
            vec![Operand::Capability(capability)],
        ));
    }

    fn specialize_storage_image_format(
        &mut self,
        metal_index: u32,
        air_format: ImageFormat,
        component: ImageComp,
    ) -> Result<(ImageFormat, Option<RuntimeStorageImageState>), String> {
        let Some(state) = usize::try_from(metal_index)
            .ok()
            .and_then(|index| self.runtime_storage_image_states.get(index))
            .copied()
            .flatten()
        else {
            return Ok((air_format, None));
        };
        let air_component = crate::meta::TextureComponent::from_image_comp(component);
        let runtime_component = state.format.component();
        if air_component != runtime_component {
            return Err(format!(
                "runtime storage image {metal_index}: AIR texels are {air_component:?}, but runtime format {:?} is {runtime_component:?}",
                state.format
            ));
        }
        self.applied_runtime_storage_image_indices
            .insert(metal_index);
        let format = state
            .format
            .explicit_format()
            .map(crate::meta::TextureFormat::to_spirv_format)
            .unwrap_or(ImageFormat::Unknown);
        if storage_image_format_needs_extended_capability(format) {
            self.add_capability(spirv::Capability::StorageImageExtendedFormats);
        }
        Ok((format, Some(state)))
    }

    fn register_runtime_storage_image_value(
        &mut self,
        value: Word,
        metal_index: u32,
        state: Option<RuntimeStorageImageState>,
    ) {
        if let Some(state) = state {
            self.runtime_storage_image_values
                .insert(value, (metal_index, state));
        }
    }

    fn require_runtime_storage_image_use(
        &mut self,
        image: Word,
        usage: RuntimeStorageImageUse,
    ) -> Result<(), String> {
        let specializations = self.runtime_storage_image_origins(image);
        if specializations.is_empty() {
            return Ok(());
        }
        let first_format = specializations[0].1.format.explicit_format();
        if specializations
            .iter()
            .any(|(_, state)| state.format.explicit_format() != first_format)
        {
            return Err(
                "differently formatted runtime storage images cannot be selected into one image value"
                    .into(),
            );
        }
        for (metal_index, state) in specializations {
            if usage == RuntimeStorageImageUse::Atomic {
                if !state.format.supports_atomics() {
                    return Err(format!(
                        "runtime storage image {metal_index}: format {:?} cannot implement storage-image atomics",
                        state.format
                    ));
                }
                if !state.capabilities.storage_image_atomic {
                    return Err(format!(
                        "runtime storage image {metal_index}: format {:?} lacks storage-image atomic support",
                        state.format
                    ));
                }
            }
            if state.format.explicit_format().is_some() {
                continue;
            }
            let (supported, capability, operation) = match usage {
                RuntimeStorageImageUse::Read => (
                    state.capabilities.read_without_format,
                    spirv::Capability::StorageImageReadWithoutFormat,
                    "read",
                ),
                RuntimeStorageImageUse::Write => (
                    state.capabilities.write_without_format,
                    spirv::Capability::StorageImageWriteWithoutFormat,
                    "write",
                ),
                RuntimeStorageImageUse::Atomic => {
                    return Err(format!(
                        "runtime storage image {metal_index}: formatless storage-image atomics are unsupported"
                    ));
                }
            };
            if !supported {
                return Err(format!(
                    "runtime storage image {metal_index}: format {:?} requires host {operation}-without-format support",
                    state.format
                ));
            }
            self.add_capability(capability);
        }
        Ok(())
    }

    fn runtime_storage_image_origins(&self, image: Word) -> Vec<(u32, RuntimeStorageImageState)> {
        let mut pending = vec![image];
        let mut visited = HashSet::default();
        let mut origins = Vec::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            if let Some(origin) = self.runtime_storage_image_values.get(&value).copied() {
                if !origins.contains(&origin) {
                    origins.push(origin);
                }
                continue;
            }
            let Some(instruction) = self
                .module
                .types_global_values
                .iter()
                .chain(self.new_globals.iter())
                .chain(
                    self.module
                        .functions
                        .iter()
                        .flat_map(|function| function.blocks.iter())
                        .flat_map(|block| block.instructions.iter()),
                )
                .find(|instruction| instruction.result_id == Some(value))
            else {
                continue;
            };
            match instruction.class.opcode {
                Op::CopyObject | Op::Load => {
                    if let Some(Operand::IdRef(source)) = instruction.operands.first() {
                        pending.push(*source);
                    }
                }
                Op::Select => {
                    pending.extend(instruction.operands.iter().skip(1).filter_map(|operand| {
                        match operand {
                            Operand::IdRef(source) => Some(*source),
                            _ => None,
                        }
                    }))
                }
                Op::Phi => pending.extend(instruction.operands.iter().enumerate().filter_map(
                    |(index, operand)| match (index % 2, operand) {
                        (0, Operand::IdRef(source)) => Some(*source),
                        _ => None,
                    },
                )),
                _ => {}
            }
        }
        origins.sort_unstable_by_key(|(metal_index, _)| *metal_index);
        origins
    }

    fn validate_runtime_storage_image_bindings(&self) -> Result<(), String> {
        for (metal_index, state) in self
            .runtime_storage_image_states
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, state)| Some((u32::try_from(index).ok()?, state?)))
        {
            if !self
                .applied_runtime_storage_image_indices
                .contains(&metal_index)
            {
                return Err(format!(
                    "runtime storage image {metal_index}: no storage-image binding exists for runtime format {:?}",
                    state.format
                ));
            }
        }
        Ok(())
    }
}

fn storage_image_format_needs_extended_capability(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Rg32f
            | ImageFormat::Rg16f
            | ImageFormat::R11fG11fB10f
            | ImageFormat::R16f
            | ImageFormat::Rgba16
            | ImageFormat::Rgb10A2
            | ImageFormat::Rg16
            | ImageFormat::Rg8
            | ImageFormat::R16
            | ImageFormat::R8
            | ImageFormat::Rgba16Snorm
            | ImageFormat::Rg16Snorm
            | ImageFormat::Rg8Snorm
            | ImageFormat::R16Snorm
            | ImageFormat::R8Snorm
            | ImageFormat::Rg32i
            | ImageFormat::Rg16i
            | ImageFormat::Rg8i
            | ImageFormat::R16i
            | ImageFormat::R8i
            | ImageFormat::Rgb10a2ui
            | ImageFormat::Rg32ui
            | ImageFormat::Rg16ui
            | ImageFormat::Rg8ui
            | ImageFormat::R16ui
            | ImageFormat::R8ui
    )
}

fn type_inst(op: Op, result: Word, operands: Vec<Operand>) -> Instruction {
    Instruction::new(op, None, Some(result), operands)
}

fn find_entry_index(module: &Module, entry_name: Option<&str>) -> Option<usize> {
    if let Some(name) = entry_name {
        let mut id_for_name = None;
        for inst in &module.debug_names {
            if inst.class.opcode == Op::Name {
                if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(s))) =
                    (inst.operands.first(), inst.operands.get(1))
                {
                    if s == name {
                        id_for_name = Some(*id);
                    }
                }
            }
        }
        if let Some(id) = id_for_name {
            if let Some(p) = module.functions.iter().position(|f| {
                !f.blocks.is_empty() && f.def.as_ref().and_then(|d| d.result_id) == Some(id)
            }) {
                return Some(p);
            }
        }
    }
    module.functions.iter().position(|f| !f.blocks.is_empty())
}

fn propagate_sampler_state_aliases(ctx: &mut Ctx, entry_idx: usize) {
    let sampler_ty = ctx.ty_sampler();
    let aliases = ctx.module.functions[entry_idx]
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            if instruction.result_type != Some(sampler_ty) {
                return None;
            }
            let result = instruction.result_id?;
            let sources = match instruction.class.opcode {
                Op::CopyObject => instruction
                    .operands
                    .first()
                    .and_then(|operand| match operand {
                        Operand::IdRef(source) => Some(vec![*source]),
                        _ => None,
                    })
                    .unwrap_or_default(),
                Op::Select => instruction
                    .operands
                    .iter()
                    .skip(1)
                    .filter_map(|operand| match operand {
                        Operand::IdRef(source) => Some(*source),
                        _ => None,
                    })
                    .collect(),
                Op::Phi => instruction
                    .operands
                    .iter()
                    .step_by(2)
                    .filter_map(|operand| match operand {
                        Operand::IdRef(source) => Some(*source),
                        _ => None,
                    })
                    .collect(),
                _ => return None,
            };
            (!sources.is_empty()).then_some((result, sources))
        })
        .collect::<Vec<_>>();
    let mut dependents = HashMap::<Word, Vec<usize>>::default();
    for (index, (_, sources)) in aliases.iter().enumerate() {
        for source in sources {
            dependents.entry(*source).or_default().push(index);
        }
    }
    let mut queued = vec![false; aliases.len()];
    let mut queue = VecDeque::new();
    for (index, (_, sources)) in aliases.iter().enumerate() {
        if sources.iter().any(|source| {
            ctx.sampler_states.contains_key(source) || ctx.ambiguous_sampler_states.contains(source)
        }) {
            queued[index] = true;
            queue.push_back(index);
        }
    }
    while let Some(index) = queue.pop_front() {
        let (result, sources) = &aliases[index];
        let mut exact = None;
        let mut saw_known = false;
        let mut conflict = false;
        for source in sources {
            if ctx.ambiguous_sampler_states.contains(source) {
                conflict = true;
                saw_known = true;
                continue;
            }
            let Some(state) = ctx.sampler_states.get(source).copied() else {
                conflict = true;
                continue;
            };
            saw_known = true;
            if exact.is_some_and(|existing| existing != state) {
                conflict = true;
            } else {
                exact = Some(state);
            }
        }
        if !saw_known {
            continue;
        }
        if conflict {
            ctx.ambiguous_sampler_states.insert(*result);
        } else if let Some(state) = exact {
            ctx.sampler_states.insert(*result, state);
        }
        for dependent in dependents.get(result).into_iter().flatten() {
            if !queued[*dependent] {
                queued[*dependent] = true;
                queue.push_back(*dependent);
            }
        }
    }
}

#[cfg(test)]
mod sampler_state_alias_tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;

    #[test]
    fn pass_context_allocator_advances_past_definitions_missing_from_the_header_bound() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(2));
        let mut ctx = Ctx::new(module);
        ctx.new_globals.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(7),
            vec![
                Operand::StorageClass(StorageClass::Workgroup),
                Operand::IdRef(1),
            ],
        ));

        assert_eq!(ctx.ty_sampler(), 8);
        assert_eq!(
            ctx.module.header.as_ref().map(|header| header.bound),
            Some(9)
        );
    }

    #[test]
    fn propagation_follows_alias_sources_in_later_blocks() {
        let mut module = Module::new();
        module
            .types_global_values
            .push(Instruction::new(Op::TypeSampler, None, Some(1), vec![]));
        module.functions.push(Function {
            def: None,
            parameters: vec![],
            blocks: vec![
                Block {
                    label: None,
                    instructions: vec![Instruction::new(
                        Op::CopyObject,
                        Some(1),
                        Some(5),
                        vec![Operand::IdRef(4)],
                    )],
                },
                Block {
                    label: None,
                    instructions: vec![Instruction::new(
                        Op::CopyObject,
                        Some(1),
                        Some(4),
                        vec![Operand::IdRef(3)],
                    )],
                },
            ],
            end: None,
        });
        let state = StaticSamplerState::from_air_words([34901797601020416, 0])
            .expect("decode sampler state");
        let mut ctx = Ctx::new(module);
        ctx.sampler_states.insert(3, state);

        propagate_sampler_state_aliases(&mut ctx, 0);

        assert_eq!(ctx.sampler_states.get(&4), Some(&state));
        assert_eq!(ctx.sampler_states.get(&5), Some(&state));
        assert!(ctx.ambiguous_sampler_states.is_empty());
    }
}

fn type_defs(module: &Module) -> HashMap<Word, Instruction> {
    let mut m = HashMap::default();
    for inst in &module.types_global_values {
        if let Some(rid) = inst.result_id {
            m.insert(rid, inst.clone());
        }
    }
    m
}

fn ptr_pointee(defs: &HashMap<Word, Instruction>, ptr: Word) -> Option<Word> {
    let inst = defs.get(&ptr)?;
    if inst.class.opcode == Op::TypePointer {
        if let Operand::IdRef(p) = inst.operands[1] {
            return Some(p);
        }
    }
    None
}

fn ptr_storage(defs: &HashMap<Word, Instruction>, ptr: Word) -> Option<StorageClass> {
    let inst = defs.get(&ptr)?;
    if inst.class.opcode == Op::TypePointer {
        if let Operand::StorageClass(sc) = inst.operands[0] {
            return Some(sc);
        }
    }
    None
}

impl Ctx {
    fn dominates_air_call_site(&self, value: Word) -> bool {
        let Some(body) = self.air_call_body.as_ref() else {
            return false;
        };
        let Some(function) = self.module.functions.get(body.function) else {
            return false;
        };
        match function.blocks.iter().position(|block| {
            block
                .instructions
                .iter()
                .any(|inst| inst.result_id == Some(value))
        }) {
            Some(block) => body.dominance.dominates(block, self.air_call_block),
            None => self
                .module
                .types_global_values
                .iter()
                .chain(self.new_globals.iter())
                .any(|inst| inst.result_id == Some(value)),
        }
    }

    fn get_or_create(&mut self, op: Op, result_type: Option<Word>, operands: Vec<Operand>) -> Word {
        let key = (op, result_type, operands.clone());
        if let Some(&id) = self.struct_cache.get(&key) {
            // A cleanup between passes can drop an unused global this cache still names (a ConstantNull the NaN-to-zero
            // conversion then selected: "owned Select references undefined id", Blender's light-probe kernel). Hand an id
            // out only while it is still defined.
            if self
                .module
                .types_global_values
                .iter()
                .chain(self.new_globals.iter())
                .any(|inst| inst.result_id == Some(id))
            {
                return id;
            }
            self.struct_cache.remove(&key);
        }
        let mut highest_definition = 0;
        let mut existing = None;
        for inst in self
            .module
            .types_global_values
            .iter()
            .chain(self.new_globals.iter())
        {
            highest_definition = highest_definition.max(inst.result_id.unwrap_or(0));
            if inst.class.opcode == op
                && inst.result_type == result_type
                && inst.operands == operands
            {
                if let Some(rid) = inst.result_id {
                    existing.get_or_insert(rid);
                }
            }
        }
        if self.module.id_bound() <= highest_definition {
            self.module
                .set_id_bound(highest_definition.saturating_add(1));
        }
        if let Some(id) = existing {
            self.struct_cache.insert(key, id);
            return id;
        }
        let id = self.module.fresh_id();
        self.new_globals
            .push(Instruction::new(op, result_type, Some(id), operands));
        self.struct_cache.insert(key, id);
        id
    }

    fn ty_float(&mut self) -> Word {
        self.get_or_create(Op::TypeFloat, None, vec![Operand::LiteralBit32(32)])
    }

    fn ty_bool(&mut self) -> Word {
        self.get_or_create(Op::TypeBool, None, vec![])
    }

    fn ty_vec_bool(&mut self, n: u32) -> Word {
        let b = self.ty_bool();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(b), Operand::LiteralBit32(n)],
        )
    }

    fn ty_vecf(&mut self, n: u32) -> Word {
        let f = self.ty_float();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(f), Operand::LiteralBit32(n)],
        )
    }

    fn ty_vech(&mut self, n: u32) -> Word {
        let h = self.ty_half();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(h), Operand::LiteralBit32(n)],
        )
    }

    fn ty_vec_u16(&mut self, n: u32) -> Word {
        let u16_ty = self.ty_int16();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(u16_ty), Operand::LiteralBit32(n)],
        )
    }

    fn ty_ptr(&mut self, sc: StorageClass, pointee: Word) -> Word {
        self.get_or_create(
            Op::TypePointer,
            None,
            vec![Operand::StorageClass(sc), Operand::IdRef(pointee)],
        )
    }

    fn ty_array(&mut self, elem: Word, len: u32) -> Word {
        let len_c = self.const_uint(len);
        let key = SynthCacheKey::Array {
            elem,
            len_const: len_c,
        };
        if let Some(&id) = self.synth_cache.get(&key) {
            return id;
        }
        let id = self.module.fresh_id();
        self.new_globals.push(type_inst(
            Op::TypeArray,
            id,
            vec![Operand::IdRef(elem), Operand::IdRef(len_c)],
        ));
        self.synth_cache.insert(key, id);
        id
    }

    fn build_air_type(&mut self, t: &AirType) -> Word {
        match t {
            AirType::Scalar(scalar) => self.ty_air_scalar(*scalar),
            AirType::Vec { scalar, lanes } => self.ty_air_vec(*scalar, *lanes),
            AirType::PackedVec { scalar, lanes } => {
                let elem = self.ty_air_scalar(*scalar);
                self.ty_array(elem, *lanes)
            }
            AirType::Array { elem, len } => {
                let elem_ty = self.build_air_type(elem);
                self.ty_array(elem_ty, *len)
            }
            AirType::Matrix { scalar, cols, rows } => {
                let col = self.ty_air_vec(*scalar, *rows);
                let arr = self.ty_array(col, *cols);
                let st = self.module.fresh_id();
                self.new_globals
                    .push(type_inst(Op::TypeStruct, st, vec![Operand::IdRef(arr)]));
                st
            }
            AirType::Struct(members) => {
                let mtys: Vec<Word> = members.iter().map(|m| self.build_air_type(&m.ty)).collect();
                let st = self.module.fresh_id();
                let offsets: Vec<u32> = members.iter().map(|m| m.offset).collect();
                if offsets.windows(2).all(|w| w[1] > w[0]) {
                    self.air_struct_offsets.insert(st, offsets);
                }
                self.new_globals.push(type_inst(
                    Op::TypeStruct,
                    st,
                    mtys.into_iter().map(Operand::IdRef).collect(),
                ));
                st
            }
            AirType::Opaque { size } => {
                self.build_air_type(&crate::meta::storage_air_type_for_size(*size))
            }
        }
    }

    fn ty_air_scalar(&mut self, scalar: AirScalar) -> Word {
        match scalar {
            AirScalar::Float => self.ty_float(),
            AirScalar::Half => self.ty_half(),
            AirScalar::UInt => self.ty_uint(),
            AirScalar::ULong | AirScalar::SLong => self.ty_ulong(),
            AirScalar::UShort | AirScalar::SShort => self.ty_int16(),
            AirScalar::SInt => self.ty_uint(),
            AirScalar::UChar | AirScalar::Bool => self.ty_int8(),
        }
    }

    fn ty_air_vec(&mut self, scalar: AirScalar, lanes: u32) -> Word {
        match scalar {
            AirScalar::Float => self.ty_vecf(lanes),
            AirScalar::Half => self.ty_vech(lanes),
            AirScalar::UInt => self.ty_vec_uint(lanes),
            AirScalar::ULong | AirScalar::SLong => self.ty_vec_ulong(lanes),
            AirScalar::UShort | AirScalar::SShort => self.ty_vec_u16(lanes),
            AirScalar::SInt => self.ty_vec_uint(lanes),
            AirScalar::UChar | AirScalar::Bool => {
                let elem = self.ty_int8();
                self.ty_array(elem, lanes)
            }
        }
    }

    fn ty_runtime_array(&mut self, elem: Word) -> Word {
        self.get_or_create(Op::TypeRuntimeArray, None, vec![Operand::IdRef(elem)])
    }

    fn ty_image(&mut self, dim: Dim, arrayed: bool, comp: ImageComp) -> Word {
        self.ty_image_ms(dim, arrayed, comp, false)
    }

    fn ty_image_ms(
        &mut self,
        dim: Dim,
        arrayed: bool,
        comp: ImageComp,
        multisampled: bool,
    ) -> Word {
        let f = match comp {
            ImageComp::Float => self.ty_float(),
            ImageComp::Uint => self.ty_uint(),
            ImageComp::Sint => self.ty_sint(),
        };
        self.get_or_create(
            Op::TypeImage,
            None,
            vec![
                Operand::IdRef(f),
                Operand::Dim(dim),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(arrayed as u32),
                Operand::LiteralBit32(multisampled as u32),
                Operand::LiteralBit32(1),
                Operand::ImageFormat(ImageFormat::Unknown),
            ],
        )
    }

    fn ty_input_attachment(&mut self, sampled: Word) -> Word {
        self.get_or_create(
            Op::TypeImage,
            None,
            vec![
                Operand::IdRef(sampled),
                Operand::Dim(Dim::DimSubpassData),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(2),
                Operand::ImageFormat(ImageFormat::Unknown),
            ],
        )
    }

    fn ty_storage_image(
        &mut self,
        dim: Dim,
        arrayed: bool,
        fmt: ImageFormat,
        comp: ImageComp,
    ) -> Word {
        let sampled = match comp {
            ImageComp::Float => self.ty_float(),
            ImageComp::Uint => self.ty_uint(),
            ImageComp::Sint => self.ty_sint(),
        };
        self.get_or_create(
            Op::TypeImage,
            None,
            vec![
                Operand::IdRef(sampled),
                Operand::Dim(dim),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(arrayed as u32),
                Operand::LiteralBit32(0),
                Operand::LiteralBit32(2),
                Operand::ImageFormat(fmt),
            ],
        )
    }

    fn ty_sampler(&mut self) -> Word {
        self.get_or_create(Op::TypeSampler, None, vec![])
    }

    fn ty_sampled_image(&mut self, image: Word) -> Word {
        self.get_or_create(Op::TypeSampledImage, None, vec![Operand::IdRef(image)])
    }

    fn ty_void(&mut self) -> Word {
        self.get_or_create(Op::TypeVoid, None, vec![])
    }

    fn ty_uint(&mut self) -> Word {
        self.get_or_create(
            Op::TypeInt,
            None,
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
        )
    }

    fn ty_ulong(&mut self) -> Word {
        self.get_or_create(
            Op::TypeInt,
            None,
            vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
        )
    }

    fn ty_sint(&mut self) -> Word {
        self.get_or_create(
            Op::TypeInt,
            None,
            vec![Operand::LiteralBit32(32), Operand::LiteralBit32(1)],
        )
    }

    fn ty_vec_sint(&mut self, n: u32) -> Word {
        let s = self.ty_sint();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(s), Operand::LiteralBit32(n)],
        )
    }

    fn ty_vec_uint(&mut self, n: u32) -> Word {
        let u = self.ty_uint();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(u), Operand::LiteralBit32(n)],
        )
    }

    fn ty_vec_ulong(&mut self, n: u32) -> Word {
        let u = self.ty_ulong();
        self.get_or_create(
            Op::TypeVector,
            None,
            vec![Operand::IdRef(u), Operand::LiteralBit32(n)],
        )
    }

    fn const_uint(&mut self, v: u32) -> Word {
        let uint = self.ty_uint();
        self.get_or_create(Op::Constant, Some(uint), vec![Operand::LiteralBit32(v)])
    }

    fn const_composite(&mut self, ty: Word, constituents: Vec<Word>) -> Word {
        let operands = constituents.into_iter().map(Operand::IdRef).collect();
        self.get_or_create(Op::ConstantComposite, Some(ty), operands)
    }

    fn kernel_requested_local_size_ids(&mut self) -> [Word; 3] {
        self.kernel_local_size.map(|value| self.const_uint(value))
    }

    fn kernel_local_size_ids(&mut self) -> [Word; 3] {
        if let Some(ids) = self.kernel_local_size_ids {
            return ids;
        }
        let ids = if matches!(
            self.kernel_dispatch,
            crate::reflect::KernelDispatch::Workgroups
        ) {
            self.kernel_local_size.map(|value| self.const_uint(value))
        } else {
            let uint_ty = self.ty_uint();
            std::array::from_fn(|dimension| {
                let id = self.module.fresh_id();
                self.new_globals.push(Instruction::new(
                    Op::SpecConstant,
                    Some(uint_ty),
                    Some(id),
                    vec![Operand::LiteralBit32(self.kernel_local_size[dimension])],
                ));
                self.module.annotations.push(Instruction::new(
                    Op::Decorate,
                    None,
                    None,
                    vec![
                        Operand::IdRef(id),
                        Operand::Decoration(Decoration::SpecId),
                        Operand::LiteralBit32(
                            crate::reflect::KERNEL_LOCAL_SIZE_SPEC_IDS[dimension],
                        ),
                    ],
                ));
                id
            })
        };
        self.kernel_local_size_ids = Some(ids);
        ids
    }

    fn kernel_workgroup_size_id(&mut self) -> Word {
        if let Some(id) = self.kernel_workgroup_size_id {
            return id;
        }
        let ids = self.kernel_local_size_ids();
        let vector_ty = self.ty_vec_uint(3);
        let id = self.module.fresh_id();
        self.new_globals.push(Instruction::new(
            Op::SpecConstantComposite,
            Some(vector_ty),
            Some(id),
            ids.into_iter().map(Operand::IdRef).collect(),
        ));
        self.module.annotations.push(Instruction::new(
            Op::Decorate,
            None,
            None,
            vec![
                Operand::IdRef(id),
                Operand::Decoration(Decoration::BuiltIn),
                Operand::BuiltIn(BuiltIn::WorkgroupSize),
            ],
        ));
        self.kernel_workgroup_size_id = Some(id);
        id
    }

    fn const_bool_of(&mut self, bool_ty: Word, value: bool) -> Word {
        let op = if value {
            Op::ConstantTrue
        } else {
            Op::ConstantFalse
        };
        self.get_or_create(op, Some(bool_ty), vec![])
    }

    fn const_int_of(&mut self, int_ty: Word, v: i64) -> Word {
        let key = SynthCacheKey::ConstInt { int_ty, value: v };
        if let Some(&id) = self.synth_cache.get(&key) {
            return id;
        }
        let (bits, signed) = self
            .module
            .types_global_values
            .iter()
            .chain(self.new_globals.iter())
            .find_map(|inst| {
                if inst.class.opcode == Op::TypeInt && inst.result_id == Some(int_ty) {
                    match (inst.operands.first(), inst.operands.get(1)) {
                        (Some(Operand::LiteralBit32(bits)), Some(Operand::LiteralBit32(sign))) => {
                            Some((*bits, *sign == 1))
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .unwrap_or((32, false));
        let lit = match bits {
            64 => Operand::LiteralBit64(v as u64),
            bits if bits < 32 && !signed => {
                Operand::LiteralBit32((v as u32) & ((1u32 << bits) - 1))
            }
            _ => Operand::LiteralBit32(v as u32),
        };
        let id = self.get_or_create(Op::Constant, Some(int_ty), vec![lit]);
        self.synth_cache.insert(key, id);
        id
    }

    fn const_float(&mut self, bits: f32) -> Word {
        let key = SynthCacheKey::ConstFloat {
            bits: bits.to_bits(),
        };
        if let Some(&id) = self.synth_cache.get(&key) {
            return id;
        }
        let f = self.ty_float();
        let id = self.get_or_create(
            Op::Constant,
            Some(f),
            vec![Operand::LiteralBit32(bits.to_bits())],
        );
        self.synth_cache.insert(key, id);
        id
    }

    fn ty_half(&mut self) -> Word {
        self.get_or_create(Op::TypeFloat, None, vec![Operand::LiteralBit32(16)])
    }

    fn const_half(&mut self, v: f32) -> Word {
        let bits = crate::float16::f32_to_f16_bits(v);
        let key = SynthCacheKey::ConstHalf { bits };
        if let Some(&id) = self.synth_cache.get(&key) {
            return id;
        }
        let h = self.ty_half();
        let id = self.get_or_create(
            Op::Constant,
            Some(h),
            vec![Operand::LiteralBit32(bits as u32)],
        );
        self.synth_cache.insert(key, id);
        id
    }

    fn glsl(&mut self) -> Word {
        if let Some(id) = self.glsl_ext {
            return id;
        }
        for inst in &self.module.ext_inst_imports {
            if let Some(Operand::LiteralString(s)) = inst.operands.first() {
                if s == "GLSL.std.450" {
                    let id = inst.result_id.unwrap();
                    self.glsl_ext = Some(id);
                    return id;
                }
            }
        }
        let id = self.module.fresh_id();
        self.module.ext_inst_imports.push(Instruction::new(
            Op::ExtInstImport,
            None,
            Some(id),
            vec![Operand::LiteralString("GLSL.std.450".into())],
        ));
        self.glsl_ext = Some(id);
        id
    }
}

mod access;
mod air_calls;
mod aliased_imageblock;
mod emitted_inline;
mod finalize;
mod latch_trampoline;
mod ldhoist;
pub(crate) mod loop_budget;
#[cfg(test)]
mod lowering_regression_tests;
mod module_cleanup;
mod subgroup_materialize;
pub(crate) use module_cleanup::{
    drop_dangling_debug, drop_unreferenced_global_variables, drop_unreferenced_scalar_types,
    drop_unrequired_capabilities, drop_unused_scalar_width_capabilities,
};
mod byte_words;
mod prune;
mod resource_phi_fold;
mod resources;
mod spirv_cfg;
mod stage_input;
mod stage_output;
mod type_singletons;
mod value_queries;
mod widen_shl;
mod workgroup;

use access::{
    compose_derived_access_chains, decorate_ptr_access_chain_base_strides,
    drop_overindexed_zero_tail, drop_writeonly_dead_local_array_stores,
    expose_nullable_memory_bases, fold_block_view_element_offsets, guard_integer_division_by_zero,
    hoist_function_variables, lower_cross_member_subword_load, lower_cross_member_subword_store,
    lower_private_byte_aggregate_reinterpret, lower_private_low_byte_word_load,
    lower_private_memory_atomics, lower_scalar_i64_arithmetic_to_u32_halves,
    lower_subword_scalar_store, materialize_inlined_local_pointer_field_stores,
    narrow_access_chain_indices, neutralize_null_access_chains,
    neutralize_private_placeholder_access_chains, recover_inlined_local_dynamic_pointer_fields,
    recover_inlined_local_pointer_fields, recover_unique_local_pointer_field_loads,
    remap_dynamic_word_index_to_array_member, remap_dynamic_word_index_to_array_struct_field,
    remap_overflow_word_index_to_outer_member, remap_word_index_to_struct_member,
    remodel_workgroup_flatword_aggregate, remodel_workgroup_floatarray_atomic_as_uint,
    remodel_workgroup_single_field_struct_array, reroot_demoted_array_element_overindex,
    retype_demoted_copymemory_placeholder, retype_private_direct_memory_placeholders,
    rewrite_byte_buffer_chained_reinterpret, rewrite_chained_element_reinterpret,
    rewrite_dynamic_homogeneous_struct_index_load, rewrite_dynamic_struct_index_reinterpret,
    rewrite_dynamic_struct_index_subword_reinterpret,
    rewrite_dynamic_struct_index_vector_reinterpret,
    rewrite_dynamic_struct_index_wide_word_reinterpret, rewrite_exact_raw_byte_block_memory,
    rewrite_flat_scalar_ptr_access_through_vector_array, rewrite_raw_byte_pointer_direct_loads,
    rewrite_raw_byte_pointer_wide_loads, rewrite_raw_byte_pointer_wide_stores,
    rewrite_reinterpret_scalar_loads, rewrite_scalar_pointer_arithmetic_access_chains,
    rewrite_scalar_slot_array_overindex, rewrite_strided_descent_access_chains,
    rewrite_thread_local_aggregate_prefix_stores, split_workgroup_ptr_access_chain_descent,
};
use air_calls::lower_air_calls;
use air_calls::lower_ray_queries;
use emitted_inline::{
    compose_chained_access_chains, inline_selected_helpers, prune_unreferenced_functions,
};
use finalize::finalize;
use prune::prune_unreachable_blocks;
use resources::rewrites::{rewrite_affine_raw_word_loads, rewrite_exact_raw_word_loads};
use resources::*;
use stage_input::{
    build_stage_input, load_kernel_dispatch_component, materialize_kernel_dispatch_field,
};
use stage_output::rewrite_return;
use subgroup_materialize::materialize_selected_subgroup_results;
use value_queries::*;
use workgroup::*;

fn air_names(module: &Module) -> HashMap<Word, String> {
    let mut m = HashMap::default();
    for inst in &module.debug_names {
        if inst.class.opcode == Op::Name {
            if let (Some(Operand::IdRef(id)), Some(Operand::LiteralString(s))) =
                (inst.operands.first(), inst.operands.get(1))
            {
                if s.starts_with("air.")
                    || s.starts_with("llvm.fabs.")
                    || s.starts_with("llvm.fmuladd.")
                    || is_agx2_matmad_symbol(s)
                    || s == "llvm.agx3.edgecheck"
                    || s == "llvm.agx3.yield"
                    || s == "llvm.agx3.igemm.v8i32.i64.i64.v8i32"
                    || s == "llvm.agx2.cluster.num"
                    || s.starts_with("llvm.agx3.load.with.emask.global.")
                    || s.starts_with("llvm.agx3.store.with.emask.global.")
                    || s.starts_with("llvm.bswap.")
                    || s.starts_with("llvm.maxnum.")
                    || s.starts_with("llvm.minnum.")
                    || s == "llvm.assume"
                {
                    m.insert(*id, s.clone());
                }
            }
        }
    }
    m
}

fn is_agx2_matmad_symbol(name: &str) -> bool {
    matches!(
        name,
        "llvm.agx2.f16matmad4x4.v2f16"
            | "llvm.agx2.f32matmad4x4.v2f32"
            | "llvm.agx2.f16matmad8x8.v2f16"
            | "llvm.agx2.f32matmad8x8.v2f32"
    )
}

pub(crate) fn inline_all_emitted_helpers(
    mut module: Module,
    emit_sidecar: crate::emit_sidecar::EmitSidecar,
    entry_name: Option<&str>,
    preserved_leaf_functions: &HashSet<Word>,
) -> Result<(Module, crate::emit_sidecar::EmitSidecar), String> {
    let (partitioned, changed) = partition_embedded_blocks(module);
    module = partitioned;
    let mut ctx = Ctx::with_options_and_sidecar(
        module,
        emit_sidecar,
        Stage::Kernel,
        TransformOptions::default(),
    );
    let entry_idx = find_entry_index(&ctx.module, entry_name)
        .ok_or_else(|| "no entry function with a body found before emitted inlining".to_string())?;
    let entry_id = ctx.module.functions[entry_idx]
        .def
        .as_ref()
        .and_then(|instruction| instruction.result_id);
    let selected_ids = ctx
        .module
        .functions
        .iter()
        .filter(|function| !function.blocks.is_empty())
        .filter_map(|function| {
            let id = function
                .def
                .as_ref()
                .and_then(|instruction| instruction.result_id)?;
            (Some(id) != entry_id && !preserved_leaf_functions.contains(&id)).then_some(id)
        })
        .collect::<HashSet<_>>();
    if !changed || selected_ids.is_empty() {
        emitted_inline::complete_inlined_access_chain_descent(&mut ctx, entry_idx);
        ctx.module.types_global_values.append(&mut ctx.new_globals);
        return Ok((ctx.module, ctx.emit_sidecar));
    }
    inline_selected_helpers(&mut ctx, entry_idx, &selected_ids)?;
    crate::native::close_inlined_bda_pointer_tables_module(&mut ctx.module);
    emitted_inline::complete_inlined_access_chain_descent(&mut ctx, entry_idx);
    if let Some((function, block, terminator, instructions)) =
        first_detached_instruction(&ctx.module)
    {
        return Err(format!(
            "emitted inliner produced instructions after a terminator \
             (reason=emitted_inline_detached_instruction, function={function}, block={block}, \
             first_terminator={terminator}, instructions={instructions})"
        ));
    }
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    Ok((ctx.module, ctx.emit_sidecar))
}

pub(crate) fn lower_specialized_workgroup_ptr_access_chains(module: Module) -> Module {
    let mut ctx = Ctx::with_options_and_sidecar(
        module,
        crate::emit_sidecar::EmitSidecar::default(),
        Stage::Kernel,
        TransformOptions::default(),
    );
    split_workgroup_ptr_access_chain_descent(&mut ctx, 0);
    ctx.module
}

fn partition_embedded_blocks(mut module: Module) -> (Module, bool) {
    let is_terminator = |instruction: &Instruction| is_block_terminator(instruction.class.opcode);
    let can_partition = module.functions.iter().all(|function| {
        function.blocks.iter().all(|block| {
            let mut segment_has_terminator = false;
            for instruction in &block.instructions {
                if instruction.class.opcode == Op::Label {
                    if !segment_has_terminator {
                        return false;
                    }
                    segment_has_terminator = false;
                } else if segment_has_terminator {
                    return false;
                } else if is_terminator(instruction) {
                    segment_has_terminator = true;
                }
            }
            segment_has_terminator
        })
    });
    if !can_partition {
        return (module, false);
    }

    module.functions = module
        .functions
        .into_iter()
        .map(|mut function| {
            let mut blocks = Vec::new();
            for block in function.blocks {
                let mut current = Block {
                    label: block.label,
                    instructions: Vec::new(),
                };
                for instruction in block.instructions {
                    if instruction.class.opcode == Op::Label {
                        blocks.push(current);
                        current = Block {
                            label: Some(instruction),
                            instructions: Vec::new(),
                        };
                    } else {
                        current.instructions.push(instruction);
                    }
                }
                blocks.push(current);
            }
            function.blocks = blocks;
            function
        })
        .collect();
    (module, true)
}

fn first_detached_instruction(module: &Module) -> Option<(usize, usize, usize, usize)> {
    for (function_index, function) in module.functions.iter().enumerate() {
        for (block_index, block) in function.blocks.iter().enumerate() {
            let terminator = block.instructions.iter().position(|instruction| {
                matches!(
                    instruction.class.opcode,
                    Op::Branch
                        | Op::BranchConditional
                        | Op::Switch
                        | Op::Return
                        | Op::ReturnValue
                        | Op::Kill
                        | Op::Unreachable
                )
            });
            if let Some(terminator) =
                terminator.filter(|index| index + 1 != block.instructions.len())
            {
                return Some((
                    function_index,
                    block_index,
                    terminator,
                    block.instructions.len(),
                ));
            }
        }
    }
    None
}

fn replace_id_in_function(func: &mut Function, from: Word, to: Word) {
    for blk in &mut func.blocks {
        for inst in &mut blk.instructions {
            for op in &mut inst.operands {
                if let Operand::IdRef(r) = op {
                    if *r == from {
                        *r = to;
                    }
                }
            }
        }
    }
}

pub(crate) fn canonicalize_ids(module: &mut Module) {
    canonicalize_ids_and_remap(module, &mut []);
}

pub(crate) fn canonicalize_ids_and_remap(module: &mut Module, tracked_ids: &mut [Word]) {
    let _ = canonicalize_ids_and_collect_remap(module, tracked_ids);
}

pub(crate) fn canonicalize_ids_and_remap_sidecar(
    module: &mut Module,
    tracked_ids: &mut [Word],
    sidecar: &mut crate::emit_sidecar::EmitSidecar,
) {
    let remap = canonicalize_ids_and_collect_remap(module, tracked_ids);
    sidecar.remap_ids(&remap);
}

fn canonicalize_ids_and_collect_remap(
    module: &mut Module,
    tracked_ids: &mut [Word],
) -> HashMap<Word, Word> {
    let mut remap: HashMap<Word, Word> = HashMap::default();
    let mut next: Word = 1;
    for inst in module.all_inst_iter() {
        if let Some(result_id) = inst.result_id {
            remap.entry(result_id).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            });
        }
    }
    for inst in module.all_inst_iter() {
        for operand in &inst.operands {
            let (Operand::IdRef(id) | Operand::IdMemorySemantics(id) | Operand::IdScope(id)) =
                operand
            else {
                continue;
            };
            remap.entry(*id).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            });
        }
        for id in inst.result_type.iter() {
            remap.entry(*id).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            });
        }
    }
    let map = |w: Word| remap.get(&w).copied().unwrap_or(w);
    for inst in module.all_inst_iter_mut() {
        if let Some(result_type) = inst.result_type.as_mut() {
            *result_type = map(*result_type);
        }
        if let Some(result_id) = inst.result_id.as_mut() {
            *result_id = map(*result_id);
        }
        for operand in &mut inst.operands {
            match operand {
                Operand::IdRef(w) | Operand::IdMemorySemantics(w) | Operand::IdScope(w) => {
                    *w = map(*w);
                }
                _ => {}
            }
        }
    }
    module.set_id_bound(next);
    for id in tracked_ids {
        *id = map(*id);
    }
    remap
}

#[cfg(test)]
mod canonicalize_tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;

    #[test]
    fn canonicalize_remaps_typed_sidecar_ids_with_the_module() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(81));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(50),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::ConstantNull, Some(50), Some(80), vec![]),
        ];
        let mut tracked = [80];

        canonicalize_ids_and_remap(&mut module, &mut tracked);

        assert_eq!(tracked, [2]);
        assert_eq!(module.types_global_values[1].result_id, Some(2));
        assert_eq!(module.header.as_ref().map(|header| header.bound), Some(3));
    }

    #[test]
    fn canonicalize_does_not_alias_an_undefined_id_onto_a_defined_one() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(81));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(50),
                vec![Operand::LiteralBit32(64), Operand::LiteralBit32(0)],
            ),
            Instruction::new(Op::ConstantNull, Some(50), Some(80), vec![]),
            Instruction::new(
                Op::ConstantComposite,
                Some(50),
                Some(70),
                vec![Operand::IdRef(1)],
            ),
        ];

        canonicalize_ids(&mut module);

        let type_id = module.types_global_values[0].result_id;
        let composite = &module.types_global_values[2];
        let Some(&Operand::IdRef(undefined)) = composite.operands.first() else {
            panic!("the composite kept its operand");
        };
        assert_ne!(
            Some(undefined),
            type_id,
            "an undefined operand was renumbered onto the TypeInt"
        );
        let defined = module
            .all_inst_iter()
            .filter_map(|inst| inst.result_id)
            .collect::<Vec<_>>();
        assert!(
            !defined.contains(&undefined),
            "an undefined operand was renumbered onto a defined id: {undefined} in {defined:?}"
        );
        assert!(
            module
                .header
                .as_ref()
                .is_some_and(|header| header.bound > undefined),
            "the id bound must cover the number the undefined reference was given"
        );
    }
}

#[cfg(test)]
mod emitted_inline_tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;
    use spirv::FunctionControl;

    fn function(id: Word, label: Word, instructions: Vec<Instruction>) -> Function {
        Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(2),
                Some(id),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(3),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: Vec::new(),
            blocks: vec![Block {
                label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
                instructions,
            }],
        }
    }

    #[test]
    fn emitted_selection_skips_a_prepass_intermediate_the_old_inliner_cannot_observe() {
        let entry_instructions = vec![
            Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
            Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(11)]),
            Instruction::new(Op::UConvert, Some(5), Some(12), vec![Operand::IdRef(6)]),
            Instruction::new(Op::Return, None, None, vec![]),
        ];
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.debug_names = vec![
            Instruction::new(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(10),
                    Operand::LiteralString("main".to_string()),
                ],
            ),
            Instruction::new(
                Op::Name,
                None,
                None,
                vec![
                    Operand::IdRef(20),
                    Operand::LiteralString("helper".to_string()),
                ],
            ),
        ];
        module.functions = vec![
            function(10, 11, entry_instructions.clone()),
            function(
                20,
                21,
                vec![Instruction::new(Op::Return, None, None, vec![])],
            ),
        ];

        let (module, _) = inline_all_emitted_helpers(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            Some("main"),
            &HashSet::default(),
        )
        .expect("invalid intermediate follows the historical retry route");

        assert_eq!(
            module.functions[0].blocks[0].instructions,
            entry_instructions
        );
        assert_eq!(
            module.functions.len(),
            2,
            "the selected helper remains untouched"
        );
    }

    #[test]
    fn emitted_closure_partitions_embedded_labels_like_the_loader() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.debug_names = vec![Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(10),
                Operand::LiteralString("main".to_string()),
            ],
        )];
        module.functions = vec![
            function(
                10,
                11,
                vec![
                    Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
                    Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(12)]),
                    Instruction::new(Op::Label, None, Some(12), vec![]),
                    Instruction::new(Op::Return, None, None, vec![]),
                ],
            ),
            function(
                20,
                21,
                vec![Instruction::new(Op::Return, None, None, vec![])],
            ),
        ];

        let (module, _) = inline_all_emitted_helpers(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            Some("main"),
            &HashSet::default(),
        )
        .expect("complete emitted closure");

        assert_eq!(module.functions[0].blocks.len(), 2);
        assert_eq!(
            module.functions[0].blocks[1]
                .label
                .as_ref()
                .and_then(|label| label.result_id),
            Some(12)
        );
        assert!(
            module.functions[0]
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .all(|instruction| instruction.class.opcode != Op::FunctionCall),
            "the helper is spliced after reproducing the serialized block partition"
        );
    }

    #[test]
    fn emitted_closure_selects_bodied_function_ids_without_debug_names() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.debug_names = vec![Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(10),
                Operand::LiteralString("main".to_string()),
            ],
        )];
        module.functions = vec![
            function(
                10,
                11,
                vec![
                    Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
                    Instruction::new(Op::Return, None, None, vec![]),
                ],
            ),
            function(
                20,
                21,
                vec![Instruction::new(Op::Return, None, None, vec![])],
            ),
        ];

        let (module, _) = inline_all_emitted_helpers(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            Some("main"),
            &HashSet::default(),
        )
        .expect("complete emitted closure");

        assert!(
            module.functions[0].blocks[0]
                .instructions
                .iter()
                .all(|instruction| instruction.class.opcode != Op::FunctionCall),
            "all bodied function ids are selected independently of OpName"
        );
    }

    #[test]
    fn emitted_closure_preserves_lowered_leaf_calls_and_their_bodies() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.debug_names = vec![Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(10),
                Operand::LiteralString("main".to_string()),
            ],
        )];
        module.functions = vec![
            function(
                10,
                11,
                vec![
                    Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(20)]),
                    Instruction::new(Op::FunctionCall, None, None, vec![Operand::IdRef(30)]),
                    Instruction::new(Op::Return, None, None, vec![]),
                ],
            ),
            function(
                20,
                21,
                vec![Instruction::new(Op::Return, None, None, vec![])],
            ),
            function(
                30,
                31,
                vec![Instruction::new(Op::Return, None, None, vec![])],
            ),
        ];
        let (module, _) = inline_all_emitted_helpers(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            Some("main"),
            &crate::fxhash::set_from([20]),
        )
        .expect("preserve lowered leaf while splicing AIR helper");
        let calls = module.functions[0]
            .all_inst_iter()
            .filter(|instruction| instruction.class.opcode == Op::FunctionCall)
            .map(|instruction| instruction.operands[0].clone())
            .collect::<Vec<_>>();
        assert_eq!(calls, vec![Operand::IdRef(20)]);
        let leaf = module
            .functions
            .iter()
            .find(|function| {
                function
                    .def
                    .as_ref()
                    .and_then(|definition| definition.result_id)
                    == Some(20)
            })
            .expect("referenced leaf body retained");
        assert_eq!(leaf.blocks.len(), 1);
        assert_eq!(leaf.blocks[0].instructions[0].class.opcode, Op::Return);
    }

    fn blocks_function(id: Word, blocks: Vec<(Word, Vec<Instruction>)>) -> Function {
        Function {
            def: Some(Instruction::new(
                Op::Function,
                Some(2),
                Some(id),
                vec![
                    Operand::FunctionControl(FunctionControl::NONE),
                    Operand::IdRef(3),
                ],
            )),
            end: Some(Instruction::new(Op::FunctionEnd, None, None, vec![])),
            parameters: Vec::new(),
            blocks: blocks
                .into_iter()
                .map(|(label, instructions)| Block {
                    label: Some(Instruction::new(Op::Label, None, Some(label), vec![])),
                    instructions,
                })
                .collect(),
        }
    }

    fn named(id: Word, name: &str) -> Instruction {
        Instruction::new(
            Op::Name,
            None,
            None,
            vec![Operand::IdRef(id), Operand::LiteralString(name.to_string())],
        )
    }

    fn phi_parents_are_predecessors(function: &Function) -> Vec<(Word, Word, bool)> {
        let mut predecessors: HashMap<Word, HashSet<Word>> = HashMap::default();
        for (from, successors) in crate::spirv_module::block_successors_by_label(&function.blocks) {
            for to in successors {
                predecessors.entry(to).or_default().insert(from);
            }
        }
        let mut answers = Vec::new();
        for block in &function.blocks {
            let Some(here) = block.label.as_ref().and_then(|label| label.result_id) else {
                continue;
            };
            let empty = HashSet::default();
            let here_predecessors = predecessors.get(&here).unwrap_or(&empty);
            for instruction in &block.instructions {
                if instruction.class.opcode != Op::Phi {
                    continue;
                }
                for parent in instruction.operands.iter().skip(1).step_by(2) {
                    if let Operand::IdRef(parent) = parent {
                        answers.push((here, *parent, here_predecessors.contains(parent)));
                    }
                }
            }
        }
        answers
    }

    #[test]
    fn a_header_phi_before_the_split_latch_follows_the_back_edge_to_the_continuation() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(100));
        module.debug_names = vec![named(10, "main"), named(20, "helper")];
        module.functions = vec![
            blocks_function(
                10,
                vec![
                    (
                        11,
                        vec![Instruction::new(
                            Op::Branch,
                            None,
                            None,
                            vec![Operand::IdRef(12)],
                        )],
                    ),
                    (
                        12,
                        vec![
                            Instruction::new(
                                Op::Phi,
                                Some(5),
                                Some(30),
                                vec![
                                    Operand::IdRef(40),
                                    Operand::IdRef(11),
                                    Operand::IdRef(41),
                                    Operand::IdRef(13),
                                ],
                            ),
                            Instruction::new(
                                Op::LoopMerge,
                                None,
                                None,
                                vec![
                                    Operand::IdRef(14),
                                    Operand::IdRef(13),
                                    Operand::LoopControl(spirv::LoopControl::NONE),
                                ],
                            ),
                            Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(13)]),
                        ],
                    ),
                    (
                        13,
                        vec![
                            Instruction::new(
                                Op::IAdd,
                                Some(5),
                                Some(41),
                                vec![Operand::IdRef(30), Operand::IdRef(30)],
                            ),
                            Instruction::new(
                                Op::FunctionCall,
                                None,
                                None,
                                vec![Operand::IdRef(20)],
                            ),
                            Instruction::new(Op::Branch, None, None, vec![Operand::IdRef(12)]),
                        ],
                    ),
                    (14, vec![Instruction::new(Op::Return, None, None, vec![])]),
                ],
            ),
            blocks_function(
                20,
                vec![
                    (
                        21,
                        vec![Instruction::new(
                            Op::Branch,
                            None,
                            None,
                            vec![Operand::IdRef(22)],
                        )],
                    ),
                    (22, vec![Instruction::new(Op::Return, None, None, vec![])]),
                ],
            ),
        ];

        let (module, _) = inline_all_emitted_helpers(
            module,
            crate::emit_sidecar::EmitSidecar::default(),
            Some("main"),
            &HashSet::default(),
        )
        .expect("complete emitted closure");

        let entry = &module.functions[0];
        assert!(
            entry
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .all(|instruction| instruction.class.opcode != Op::FunctionCall),
            "the multi-block helper is spliced into the latch"
        );
        let stale = phi_parents_are_predecessors(entry)
            .into_iter()
            .filter(|(_, _, is_predecessor)| !is_predecessor)
            .collect::<Vec<_>>();
        assert!(
            stale.is_empty(),
            "every phi parent must be a real predecessor after the splice, got {stale:?}"
        );
        let header_parents = entry
            .blocks
            .iter()
            .find(|block| block.label.as_ref().and_then(|label| label.result_id) == Some(12))
            .expect("the loop header survives")
            .instructions
            .iter()
            .find(|instruction| instruction.class.opcode == Op::Phi)
            .expect("the loop-carried phi survives")
            .operands
            .iter()
            .skip(1)
            .step_by(2)
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            !header_parents.contains(&Operand::IdRef(13)),
            "the header phi must not keep naming the latch, which now branches into the callee:              {header_parents:?}"
        );
    }
}

#[cfg(test)]
pub(crate) fn transform(
    module: Module,
    stage: Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
    entry_name: Option<&str>,
) -> Result<Module, String> {
    transform_with_options(
        module,
        stage,
        frag,
        vert,
        kern,
        entry_name,
        TransformOptions {
            kernel_dispatch: matches!(stage, Stage::Kernel)
                .then_some(crate::reflect::KernelDispatch::Workgroups),
            ..TransformOptions::default()
        },
    )
}

#[cfg(test)]
pub(crate) fn transform_with_options(
    module: Module,
    stage: Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
    entry_name: Option<&str>,
    options: TransformOptions,
) -> Result<Module, String> {
    transform_with_options_and_sidecar(
        module,
        crate::emit_sidecar::EmitSidecar::default(),
        stage,
        frag,
        vert,
        kern,
        entry_name,
        options,
    )
    .map(
        |Transformed {
             module, sidecar, ..
         }| {
            let mut sidecar = sidecar;
            sidecar.buffer_root_source_types.clear();
            let mut ctx = Ctx::with_options_and_sidecar(module, sidecar, stage, options);
            module_cleanup::gc_dead_globals(&mut ctx);
            ctx.module
        },
    )
}

#[cfg(test)]
pub(crate) fn validate_descriptor_bindings(
    module: &Module,
    layout: crate::reflect::DescriptorLayout,
) -> Result<(), String> {
    resources::validate_descriptor_binding_classes(module, layout)
}

pub(crate) fn validate_descriptor_bindings_with_ray_table(
    module: &Module,
    layout: crate::reflect::DescriptorLayout,
    ray_table_binding: Option<u32>,
) -> Result<(), String> {
    resources::validate_descriptor_binding_classes_with_ray_table(module, layout, ray_table_binding)
}

#[cfg(test)]
mod phase_contract_tests {
    use super::*;
    use crate::spirv_module::ModuleHeader;

    fn ctx_with_staged_pointer_type() -> Ctx {
        let mut module = Module::new();
        let mut header = ModuleHeader::new(10);
        header.set_version(1, 5);
        module.header = Some(header);
        module.capabilities = [spirv::Capability::Shader, spirv::Capability::Linkage]
            .map(|capability| {
                Instruction::new(
                    Op::Capability,
                    None,
                    None,
                    vec![Operand::Capability(capability)],
                )
            })
            .to_vec();
        module.memory_model = Some(Instruction::new(
            Op::MemoryModel,
            None,
            None,
            vec![
                Operand::AddressingModel(spirv::AddressingModel::Logical),
                Operand::MemoryModel(spirv::MemoryModel::GLSL450),
            ],
        ));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Variable,
                Some(2),
                Some(3),
                vec![Operand::StorageClass(StorageClass::Private)],
            ),
        ];
        let mut ctx = Ctx::new(module);
        ctx.new_globals.push(Instruction::new(
            Op::TypePointer,
            None,
            Some(2),
            vec![
                Operand::StorageClass(StorageClass::Private),
                Operand::IdRef(1),
            ],
        ));
        ctx
    }

    #[test]
    fn a_synthesized_constant_reuses_the_one_the_module_already_declares() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(10));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(2),
                vec![Operand::LiteralBit32(1.5f32.to_bits())],
            ),
            Instruction::new(
                Op::TypeFloat,
                None,
                Some(3),
                vec![Operand::LiteralBit32(16)],
            ),
            Instruction::new(
                Op::Constant,
                Some(3),
                Some(4),
                vec![Operand::LiteralBit32(
                    crate::float16::f32_to_f16_bits(0.5) as u32
                )],
            ),
            Instruction::new(
                Op::TypeInt,
                None,
                Some(5),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::Constant,
                Some(5),
                Some(6),
                vec![Operand::LiteralBit32(7)],
            ),
        ];
        let mut ctx = Ctx::new(module);
        assert_eq!(ctx.const_float(1.5), 2, "float constant was declared twice");
        assert_eq!(ctx.const_half(0.5), 4, "half constant was declared twice");
        assert_eq!(ctx.const_int_of(5, 7), 6, "int constant was declared twice");
        assert!(
            ctx.new_globals
                .iter()
                .all(|inst| inst.class.opcode != Op::Constant),
            "a constant was staged even though the module already declared it"
        );
    }

    #[test]
    fn a_composite_constant_is_declared_once_however_many_callers_ask() {
        let mut module = Module::new();
        module.header = Some(ModuleHeader::new(10));
        module.types_global_values = vec![
            Instruction::new(
                Op::TypeInt,
                None,
                Some(1),
                vec![Operand::LiteralBit32(32), Operand::LiteralBit32(1)],
            ),
            Instruction::new(
                Op::TypeVector,
                None,
                Some(2),
                vec![Operand::IdRef(1), Operand::LiteralBit32(3)],
            ),
            Instruction::new(
                Op::Constant,
                Some(1),
                Some(3),
                vec![Operand::LiteralBit32(0)],
            ),
            Instruction::new(
                Op::ConstantComposite,
                Some(2),
                Some(4),
                vec![Operand::IdRef(3), Operand::IdRef(3), Operand::IdRef(3)],
            ),
        ];
        let mut ctx = Ctx::new(module);

        let splat = crate::passes::access::const_composite_splat(&mut ctx, 2, 3, 3);
        assert_eq!(
            splat, 4,
            "the splat the module already declares was declared again"
        );
        assert_eq!(
            ctx.const_composite(2, vec![3, 3, 3]),
            4,
            "a second caller with the same shape got a second id"
        );

        let mixed = ctx.const_int_of(1, 1);
        let first = ctx.const_composite(2, vec![3, mixed, 3]);
        assert_ne!(first, 4, "a different shape reused the zero splat");
        assert_eq!(
            ctx.const_composite(2, vec![3, mixed, 3]),
            first,
            "the new shape was declared twice"
        );
        assert_eq!(
            ctx.new_globals
                .iter()
                .filter(|inst| inst.class.opcode == Op::ConstantComposite)
                .count(),
            1,
            "more composites were staged than there are distinct shapes"
        );
    }

    #[test]
    fn a_phase_dump_is_named_for_its_phase_and_holds_the_module() {
        let ctx = ctx_with_staged_pointer_type();
        let prefix = std::env::temp_dir().join(format!(
            "m2v_phase_dump_test_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        write_phase_dump(&ctx, prefix.as_os_str(), 7, "air lowering start");
        let path = prefix.with_file_name(format!(
            "{}-07-air-lowering-start.spv",
            prefix.file_name().unwrap().to_string_lossy()
        ));
        let bytes = std::fs::read(&path).expect("phase dump written");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            bytes.get(..4),
            Some(&0x0723_0203u32.to_le_bytes()[..]),
            "dump must start with the SPIR-V magic number"
        );
        assert_eq!(
            bytes.len(),
            ctx.module.assemble().len() * 4,
            "dump must be the whole assembled module"
        );
    }

    #[test]
    fn a_phase_dump_that_cannot_be_written_is_not_an_error() {
        let ctx = ctx_with_staged_pointer_type();
        let prefix = std::env::temp_dir()
            .join("m2v_phase_dump_missing_dir")
            .join("x");
        write_phase_dump(&ctx, prefix.as_os_str(), 0, "cleanup start");
    }

    #[test]
    fn a_staged_global_counts_as_declared() {
        assert_eq!(
            phase_contract_verdict(&ctx_with_staged_pointer_type()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_genuinely_missing_global_is_reported_by_id() {
        let mut ctx = ctx_with_staged_pointer_type();
        ctx.new_globals.clear();
        let verdicts = phase_contract_verdict(&ctx);
        assert!(
            verdicts
                .iter()
                .any(|verdict| verdict.contains("references undefined id %2")),
            "verdict must name the missing id, got: {verdicts:?}"
        );
    }

    #[test]
    fn a_transient_debug_name_does_not_hide_the_violation_under_it() {
        let mut ctx = ctx_with_staged_pointer_type();
        ctx.module.debug_names.push(Instruction::new(
            Op::Name,
            None,
            None,
            vec![
                Operand::IdRef(9),
                Operand::LiteralString("erased".to_string()),
            ],
        ));
        ctx.module.types_global_values.push(Instruction::new(
            Op::Variable,
            Some(3),
            Some(4),
            vec![Operand::StorageClass(StorageClass::Private)],
        ));
        let verdicts = phase_contract_verdict(&ctx);
        assert!(
            verdicts
                .iter()
                .any(|verdict| verdict.contains("references undefined id %9")),
            "the dangling debug name must be reported, got: {verdicts:?}"
        );
        assert!(
            verdicts
                .iter()
                .any(|verdict| verdict.contains("result type is not a type declaration")),
            "the violation under it must be reported too, got: {verdicts:?}"
        );
    }
}

fn phase_contract_verdict(ctx: &Ctx) -> Vec<String> {
    let mut module = ctx.module.clone();
    module
        .types_global_values
        .extend(ctx.new_globals.iter().cloned());
    crate::native::owned_module_failures(&module, usize::MAX)
        .into_iter()
        .map(|failure| {
            let (kind, error) = match failure {
                crate::native::OwnedModuleFailure::Invalid(error) => ("invalid", error),
                crate::native::OwnedModuleFailure::TypeConstruction(error) => {
                    ("type-construction", error)
                }
                crate::native::OwnedModuleFailure::CfgConstruction(error) => {
                    ("cfg-construction", error)
                }
                crate::native::OwnedModuleFailure::RawBufferConstruction(error) => {
                    ("raw-buffer-construction", error)
                }
            };
            format!("{kind}: {error}")
        })
        .collect()
}

fn write_phase_dump(ctx: &Ctx, prefix: &std::ffi::OsStr, ordinal: usize, phase: &str) {
    let slug = phase
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let mut path = prefix.to_os_string();
    path.push(format!("-{ordinal:02}-{slug}.spv"));
    let words = ctx.module.assemble();
    let bytes = words
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<u8>>();
    if let Err(err) = std::fs::write(&path, bytes) {
        eprintln!("[phase-dump] {phase}: {err}");
    }
}

fn report_phase_contract(ctx: &Ctx, phase: &str) {
    let verdicts = phase_contract_verdict(ctx);
    if verdicts.is_empty() {
        eprintln!("[pass-contract] {phase}: ok");
    }
    for verdict in verdicts {
        eprintln!("[pass-contract] {phase}: {verdict}");
    }
}

pub(crate) struct Transformed {
    pub(crate) module: Module,
    pub(crate) sidecar: crate::emit_sidecar::EmitSidecar,
    pub(crate) placeholder_descriptor_bindings: Vec<u32>,
    pub(crate) ray_instance_user_id_table_binding: Option<u32>,
    pub(crate) fragment_sample_positions: Option<Word>,
}

pub(crate) fn fit_local_size_to_ceiling(
    stage: Stage,
    kern: Option<&KernMeta>,
    mut options: TransformOptions,
) -> Result<TransformOptions, String> {
    let (Stage::Kernel, Some(ceiling)) = (stage, kern.and_then(|meta| meta.max_work_group_size))
    else {
        return Ok(options);
    };
    let [x, y, z] = options.kernel_local_size;
    let requested = x.saturating_mul(y).saturating_mul(z);
    if requested <= ceiling {
        return Ok(options);
    }
    if ceiling >= 1
        && options.kernel_dispatch.is_none()
        && options.kernel_local_size == TransformOptions::default().kernel_local_size
    {
        options.kernel_local_size = [ceiling, 1, 1];
        return Ok(options);
    }
    Err(format!(
        "kernel LocalSize {x}x{y}x{z} is {requested} threads, past the \
         `air.max_work_group_size` ceiling of {ceiling} this entry was compiled for"
    ))
}

pub(crate) fn transform_with_options_and_sidecar(
    module: Module,
    emit_sidecar: crate::emit_sidecar::EmitSidecar,
    stage: Stage,
    frag: Option<&FragMeta>,
    vert: Option<&VertMeta>,
    kern: Option<&KernMeta>,
    entry_name: Option<&str>,
    options: TransformOptions,
) -> Result<Transformed, String> {
    validate_kernel_dispatch_options(stage, options)?;
    let options = fit_local_size_to_ceiling(stage, kern, options)?;
    if let Some(dispatch) = options.kernel_dispatch {
        dispatch.validate()?;
    }
    if !matches!(stage, Stage::Kernel) && options.kernel_dispatch.is_some() {
        return Err("kernel dispatch bounds are only valid for kernel stages".to_string());
    }
    let mut ctx = Ctx::with_options_and_sidecar(module, emit_sidecar, stage, options);
    ctx.unsurfaced_embedded_resources = match stage {
        Stage::Fragment => frag.map(|m| m.unsurfaced_embedded_resources.clone()),
        Stage::Vertex => vert.map(|m| m.unsurfaced_embedded_resources.clone()),
        Stage::Kernel => kern.map(|m| m.unsurfaced_embedded_resources.clone()),
    }
    .unwrap_or_default();
    let retry_debug = crate::env_vars::retry_debug();
    let pass_contract = crate::env_vars::pass_contract();
    let phase_dump = crate::env_vars::phase_dump();
    let phase_ordinal = std::cell::Cell::new(0usize);
    macro_rules! debug_phase {
        ($phase:expr) => {
            if retry_debug {
                eprintln!("[retry-debug] passes: {}", $phase);
            }
            if pass_contract {
                report_phase_contract(&ctx, $phase);
            }
            if let Some(prefix) = phase_dump.as_deref() {
                write_phase_dump(&ctx, prefix, phase_ordinal.get(), $phase);
            }
            phase_ordinal.set(phase_ordinal.get() + 1);
        };
    }
    let mut entry_idx = find_entry_index(&ctx.module, entry_name)
        .ok_or_else(|| "no entry function with a body found".to_string())?;
    debug_phase!("cleanup start");
    compose_chained_access_chains(&mut ctx, entry_idx);
    prune_unreferenced_functions(&mut ctx, entry_idx);
    entry_idx = find_entry_index(&ctx.module, entry_name)
        .ok_or_else(|| "entry vanished after helper cleanup".to_string())?;
    recover_inlined_local_pointer_fields(&mut ctx, entry_idx);
    compose_derived_access_chains(&mut ctx, entry_idx);
    neutralize_null_access_chains(&mut ctx, entry_idx);

    debug_phase!("interface start");
    let input_defs = build_stage_input(&mut ctx, entry_idx, &stage, frag, vert, kern)?;
    ctx.validate_runtime_storage_image_bindings()?;
    materialize_inlined_local_pointer_field_stores(&mut ctx, entry_idx);
    rewrite_return(&mut ctx, entry_idx, &stage, frag, vert, &input_defs)?;
    resources::materialize_texture_array_loads(&mut ctx, entry_idx);
    hoist_function_variables(&mut ctx, entry_idx);
    recover_inlined_local_pointer_fields(&mut ctx, entry_idx);
    resources::materialize_texture_array_loads(&mut ctx, entry_idx);
    neutralize_private_placeholder_access_chains(&mut ctx, entry_idx)?;
    resource_phi_fold::fold_constant_resource_phis(&mut ctx, entry_idx);
    propagate_sampler_state_aliases(&mut ctx, entry_idx);

    lower_ray_queries(&mut ctx, entry_idx)?;

    debug_phase!("air lowering start");
    ctx.phase_value_types = Some(function_value_types(&ctx, entry_idx));
    let air_lowering = lower_air_calls(&mut ctx, entry_idx);
    ctx.phase_value_types = None;
    air_lowering?;
    aliased_imageblock::lower_aliased_imageblock(
        &mut ctx,
        entry_idx,
        &stage,
        &kern
            .and_then(|meta| {
                meta.aliased_implicit_imageblock_params
                    .first()
                    .and_then(|param| meta.aliased_implicit_imageblock_planes.get(param))
            })
            .cloned()
            .unwrap_or_default(),
    )?;
    resources::collapse_late_pointer_and_opaque_wrappers(&mut ctx, entry_idx)?;
    recover_unique_local_pointer_field_loads(&mut ctx, entry_idx);
    recover_inlined_local_dynamic_pointer_fields(&mut ctx, entry_idx)?;
    lower_private_memory_atomics(&mut ctx, entry_idx);

    guard_integer_division_by_zero(&mut ctx, entry_idx);

    debug_phase!("memory lowering start");
    ctx.phase_type_positions = Some(
        ctx.new_globals
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| Some((instruction.result_id?, (true, index))))
            .chain(
                ctx.module
                    .types_global_values
                    .iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| {
                        Some((instruction.result_id?, (false, index)))
                    }),
            )
            .collect(),
    );
    for function_idx in 0..ctx.module.functions.len() {
        narrow_access_chain_indices(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        expose_nullable_memory_bases(&mut ctx, function_idx);
    }
    compose_derived_access_chains(&mut ctx, entry_idx);
    rewrite_scalar_pointer_arithmetic_access_chains(&mut ctx, entry_idx);
    for function_idx in 0..ctx.module.functions.len() {
        drop_overindexed_zero_tail(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        lower_private_low_byte_word_load(&mut ctx, function_idx);
    }
    reroot_demoted_array_element_overindex(&mut ctx, entry_idx);
    remap_word_index_to_struct_member(&mut ctx, entry_idx);
    remap_overflow_word_index_to_outer_member(&mut ctx, entry_idx);
    for function_idx in 0..ctx.module.functions.len() {
        remap_dynamic_word_index_to_array_member(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        remap_dynamic_word_index_to_array_struct_field(&mut ctx, function_idx);
    }
    drop_writeonly_dead_local_array_stores(&mut ctx, entry_idx);
    lower_cross_member_subword_load(&mut ctx, entry_idx)?;
    lower_cross_member_subword_store(&mut ctx, entry_idx);
    lower_subword_scalar_store(&mut ctx, entry_idx);
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_strided_descent_access_chains(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_struct_index_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_struct_index_subword_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_struct_index_wide_word_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_struct_index_vector_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_homogeneous_struct_index_load(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_chained_element_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_byte_buffer_chained_reinterpret(&mut ctx, function_idx)?;
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_raw_byte_pointer_direct_loads(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_raw_byte_pointer_wide_loads(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_raw_byte_pointer_wide_stores(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        fold_block_view_element_offsets(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_dynamic_struct_index_subword_reinterpret(&mut ctx, function_idx)?;
    }
    decorate_ptr_access_chain_base_strides(&mut ctx);
    rewrite_reinterpret_scalar_loads(&mut ctx, entry_idx);
    rewrite_scalar_slot_array_overindex(&mut ctx, entry_idx)?;
    remodel_workgroup_flatword_aggregate(&mut ctx, entry_idx);
    remodel_workgroup_single_field_struct_array(&mut ctx, entry_idx);
    remodel_workgroup_floatarray_atomic_as_uint(&mut ctx, entry_idx)?;
    lower_private_byte_aggregate_reinterpret(&mut ctx, entry_idx)?;
    retype_private_direct_memory_placeholders(&mut ctx);
    retype_demoted_copymemory_placeholder(&mut ctx, entry_idx);
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_thread_local_aggregate_prefix_stores(&mut ctx, function_idx);
    }
    resources::collapse_late_pointer_and_opaque_wrappers(&mut ctx, entry_idx)?;
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_exact_raw_byte_block_memory(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_raw_byte_pointer_wide_loads(&mut ctx, function_idx);
    }
    rewrite_flat_scalar_ptr_access_through_vector_array(&mut ctx, entry_idx);
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_exact_raw_word_loads(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        rewrite_affine_raw_word_loads(&mut ctx, function_idx);
    }
    let bound_buffer_vars = ctx.bound_buffer_vars.clone();
    resources::retire_dead_pointer_projections(
        &mut ctx,
        entry_idx,
        bound_buffer_vars.iter().copied(),
    );
    decorate_ptr_access_chain_base_strides(&mut ctx);
    ctx.emit_sidecar.buffer_root_source_types.clear();
    neutralize_private_placeholder_access_chains(&mut ctx, entry_idx)?;
    ctx.phase_type_positions = None;
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    let preserved_pointer_facts = ctx
        .emit_sidecar
        .local_pointer_field_stores
        .iter()
        .map(|fact| fact.id)
        .collect::<HashSet<_>>();
    crate::native::eliminate_dead_pointer_values_module(&mut ctx.module, &preserved_pointer_facts);
    crate::native::construct_workgroup_atomic_floats_module(&mut ctx.module);
    crate::native::construct_atomic_storage_image_formats_module(&mut ctx.module);
    crate::native::close_private_vector_word_views_module(&mut ctx.module);
    if let Some(address_table) =
        crate::native::construct_interface_cross_binding_pointer_phis_module(
            &mut ctx.module,
            ctx.descriptor_layout,
        )
    {
        ctx.interface_buffer_var(address_table);
    }
    decorate_ptr_access_chain_base_strides(&mut ctx);

    debug_phase!("integer lowering start");
    if std::env::var_os("NVMTL_I64_HALVES").is_some() {
        lower_scalar_i64_arithmetic_to_u32_halves(&mut ctx);
    }

    debug_phase!("workgroup/finalize start");
    if matches!(stage, Stage::Kernel) {
        workgroup::unroll_small_workgroup_atomic_loops(&mut ctx, entry_idx);
        if std::env::var_os("NVMTL_WG_ZERO").is_some() {
            workgroup::zero_initialize_workgroup_memory(&mut ctx, entry_idx);
        }
    }
    prune_unreachable_blocks(&mut ctx.module);
    materialize_selected_subgroup_results(&mut ctx);

    resources::sink_loop_header_texture_array_loads(&mut ctx, entry_idx);
    ctx.emit_sidecar.local_pointer_field_stores.clear();
    ctx.emit_sidecar.aggregate_pointer_values.clear();
    finalize(&mut ctx, entry_idx, &stage, frag, vert)?;
    for function_idx in 0..ctx.module.functions.len() {
        drop_overindexed_zero_tail(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        lower_private_low_byte_word_load(&mut ctx, function_idx);
    }
    for function_idx in 0..ctx.module.functions.len() {
        narrow_access_chain_indices(&mut ctx, function_idx);
    }
    split_workgroup_ptr_access_chain_descent(&mut ctx, entry_idx);
    decorate_ptr_access_chain_base_strides(&mut ctx);
    ctx.module.types_global_values.append(&mut ctx.new_globals);
    stage_input::drop_unconsumed_placeholder_descriptor_loads(&mut ctx);
    module_cleanup::drop_unreferenced_global_variables(&mut ctx.module);
    module_cleanup::gc_dead_globals(&mut ctx);
    if !crate::env_vars::no_latch_trampoline_fold() {
        let folded = latch_trampoline::fold_latch_break_trampolines(&mut ctx.module);
        if retry_debug && folded > 0 {
            eprintln!("[retry-debug] passes: folded {folded} do-while latch trampoline(s)");
        }
    }
    if !crate::env_vars::no_widen_shl() {
        let widened = widen_shl::widen_masked_shifts(&mut ctx.module);
        if retry_debug && widened > 0 {
            eprintln!("[retry-debug] passes: widened {widened} masked 16-bit shift(s)");
        }
    }
    if !crate::env_vars::no_byte_words() {
        let worded = byte_words::word_back_function_byte_arrays(&mut ctx.module);
        if retry_debug && worded > 0 {
            eprintln!("[retry-debug] passes: word-backed {worded} Function byte variable(s)");
        }
    }
    if !crate::env_vars::no_load_hoist() {
        let hoisted = ldhoist::hoist_device_loads(&mut ctx.module);
        if retry_debug && hoisted > 0 {
            eprintln!(
                "[retry-debug] passes: hoisted {hoisted} device load(s) above threadgroup stores"
            );
        }
    }
    debug_phase!("complete");

    let placeholder_descriptor_bindings = surviving_placeholder_bindings(&ctx);
    let ray_instance_user_id_table_binding = ctx
        .ray_instance_user_id_table
        .filter(|id| {
            ctx.module
                .types_global_values
                .iter()
                .any(|i| i.result_id == Some(*id))
        })
        .map(|_| crate::reflect::RAY_INSTANCE_USER_ID_TABLE_BINDING);
    Ok(Transformed {
        module: ctx.module,
        sidecar: ctx.emit_sidecar,
        placeholder_descriptor_bindings,
        fragment_sample_positions: ctx.fragment_sample_positions_var,
        ray_instance_user_id_table_binding,
    })
}

fn surviving_placeholder_bindings(ctx: &Ctx) -> Vec<u32> {
    let mut bindings = ctx
        .module
        .types_global_values
        .iter()
        .filter(|instruction| instruction.class.opcode == Op::Variable)
        .filter_map(|instruction| instruction.result_id)
        .filter_map(|variable| ctx.placeholder_descriptor_vars.get(&variable).copied())
        .collect::<Vec<_>>();
    bindings.sort_unstable();
    bindings.dedup();
    bindings
}
