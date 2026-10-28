use crate::float16::f16_bits_to_f32;
use crate::meta::{
    texture_shape_from_name, AirType, BufferAccess, FragMeta, FragRole, FunctionConstant, KernMeta,
    KernRole, TextureComponent, TextureDimension, TextureShape, VertMeta, VertOutRole, VertRole,
};
use crate::spirv_module::Module;

mod footprint;
mod nonwritable;

pub(crate) fn decorate_unwritten_descriptors(module: &mut Module) {
    nonwritable::decorate_unwritten_descriptors(module);
}

pub const REFLECTION_VERSION: u32 = 56;

pub const RAY_INSTANCE_USER_ID_TABLE_BINDING: u32 = 31;

pub const KERNEL_DISPATCH_PUSH_CONSTANT_SIZE: u32 = 48;

pub const KERNEL_LOCAL_SIZE_SPEC_IDS: [u32; 3] = [0, 1, 2];

pub const DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET: u32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KernelDispatchPushConstantRange {
    pub offset: u32,
    pub size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum KernelDispatch {
    Workgroups,
    ThreadsFixed { threads_per_grid: [u32; 3] },
    ThreadsDynamic { offset: u32 },
}

impl KernelDispatch {
    pub const fn safe_default() -> Self {
        Self::ThreadsDynamic {
            offset: DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
        }
    }
}

impl KernelDispatch {
    pub fn validate(self) -> Result<(), String> {
        let offset = match self {
            Self::Workgroups => return Ok(()),
            Self::ThreadsFixed { .. } => DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
            Self::ThreadsDynamic { offset } => offset,
        };
        if offset % 4 != 0 {
            return Err(format!(
                "kernel grid push-constant offset {offset} is not 4-byte aligned"
            ));
        }
        offset
            .checked_add(KERNEL_DISPATCH_PUSH_CONSTANT_SIZE)
            .ok_or_else(|| "kernel grid push-constant range overflows u32".to_string())?;
        Ok(())
    }

    pub const fn push_constant_range(self) -> Option<KernelDispatchPushConstantRange> {
        match self {
            Self::ThreadsDynamic { offset }
                if offset
                    .checked_add(KERNEL_DISPATCH_PUSH_CONSTANT_SIZE)
                    .is_some() =>
            {
                Some(KernelDispatchPushConstantRange {
                    offset,
                    size: KERNEL_DISPATCH_PUSH_CONSTANT_SIZE,
                })
            }
            Self::ThreadsFixed { .. } => Some(KernelDispatchPushConstantRange {
                offset: DEFAULT_KERNEL_DISPATCH_PUSH_CONSTANT_OFFSET,
                size: KERNEL_DISPATCH_PUSH_CONSTANT_SIZE,
            }),
            Self::Workgroups => None,
            Self::ThreadsDynamic { .. } => None,
        }
    }

    pub fn plan(
        self,
        nominal_local_size: [u32; 3],
        dynamic_threads_per_grid: Option<[u32; 3]>,
    ) -> Result<KernelDispatchPlan, String> {
        if nominal_local_size.contains(&0) {
            return Err("kernel local-size dimensions must be non-zero".to_string());
        }
        let threads_per_grid = match self {
            Self::Workgroups => {
                return Err("whole-workgroup dispatches do not use an exact-thread plan".to_string())
            }
            Self::ThreadsFixed { threads_per_grid } => {
                if dynamic_threads_per_grid.is_some_and(|grid| grid != threads_per_grid) {
                    return Err(
                        "runtime thread grid does not match fixed kernel dispatch".to_string()
                    );
                }
                threads_per_grid
            }
            Self::ThreadsDynamic { .. } => dynamic_threads_per_grid
                .ok_or_else(|| "dynamic kernel dispatch requires a thread grid".to_string())?,
        };
        KernelDispatchPlan::new(threads_per_grid, nominal_local_size)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KernelDispatchRegion {
    pub local_size: [u32; 3],
    pub group_count: [u32; 3],
    pub thread_base: [u32; 3],
    pub threadgroup_base: [u32; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KernelDispatchPlan {
    pub threads_per_grid: [u32; 3],
    pub threadgroups_per_grid: [u32; 3],
    pub regions: Vec<KernelDispatchRegion>,
}

impl KernelDispatchPlan {
    pub fn push_constants(&self, region: KernelDispatchRegion) -> [u32; 12] {
        let mut words = [0; 12];
        words[0..3].copy_from_slice(&self.threads_per_grid);
        words[3..6].copy_from_slice(&region.thread_base);
        words[6..9].copy_from_slice(&region.threadgroup_base);
        words[9..12].copy_from_slice(&self.threadgroups_per_grid);
        words
    }

    fn new(threads_per_grid: [u32; 3], nominal_local_size: [u32; 3]) -> Result<Self, String> {
        let threadgroups_per_grid = std::array::from_fn(|dimension| {
            threads_per_grid[dimension].div_ceil(nominal_local_size[dimension])
        });
        let mut regions = Vec::with_capacity(8);
        for mask in 0_u8..8 {
            let mut local_size = nominal_local_size;
            let mut group_count = [0; 3];
            let mut thread_base = [0; 3];
            let mut threadgroup_base = [0; 3];
            let mut nonempty = true;
            for dimension in 0..3 {
                let full = threads_per_grid[dimension] / nominal_local_size[dimension];
                let tail = threads_per_grid[dimension] % nominal_local_size[dimension];
                if mask & (1 << dimension) == 0 {
                    group_count[dimension] = full;
                } else if tail == 0 {
                    nonempty = false;
                } else {
                    local_size[dimension] = tail;
                    group_count[dimension] = 1;
                    thread_base[dimension] = full * nominal_local_size[dimension];
                    threadgroup_base[dimension] = full;
                }
                nonempty &= group_count[dimension] != 0;
            }
            if nonempty {
                regions.push(KernelDispatchRegion {
                    local_size,
                    group_count,
                    thread_base,
                    threadgroup_base,
                });
            }
        }
        Ok(Self {
            threads_per_grid,
            threadgroups_per_grid,
            regions,
        })
    }
}

pub const DESCRIPTOR_LAYOUT_VERSION: u32 = 1;

pub const RESOURCE_DESCRIPTOR_SET: u32 = 0;
pub const BINDLESS_HEAP_SET: u32 = 2;
pub const BINDLESS_HEAP_BINDING: u32 = 0;
pub const BINDLESS_SAMPLER_BINDING: u32 = 1;
pub const BINDLESS_SAMPLER_SLOTS: u32 = 2048;
/// Descriptor set of [[color(n)]] framebuffer-fetch input attachments (binding 192 + n), apart from the resource sets.
pub const INPUT_ATTACHMENT_SET: u32 = 3;
pub const BINDLESS_STORAGE_BINDING: u32 = 2;
pub const BINDLESS_UTEXEL_BINDING: u32 = 3;
pub const BINDLESS_STEXEL_BINDING: u32 = 4;
pub const BINDLESS_HEAP_SLOTS: u32 = 131072;
pub fn bindless_fixed_fields_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("NVMTL_NO_TIER2_FIXED").as_deref(), Ok(v) if !v.is_empty() && v != "0"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DescriptorBindingRange {
    pub start: u32,
    pub end: u32,
}

impl DescriptorBindingRange {
    pub const fn from_base_count(base: u32, count: u32) -> Result<Self, DescriptorLayoutError> {
        match base.checked_add(count) {
            Some(end) => Ok(Self { start: base, end }),
            None => Err(DescriptorLayoutError::RangeOverflow { base, count }),
        }
    }

    pub const fn binding(self, index: u32) -> Option<u32> {
        let Some(width) = self.end.checked_sub(self.start) else {
            return None;
        };
        if index < width {
            Some(self.start + index)
        } else {
            None
        }
    }

    pub const fn contains(self, binding: u32) -> bool {
        binding >= self.start && binding < self.end
    }

    pub const fn len(self) -> Option<u32> {
        self.end.checked_sub(self.start)
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

pub const BUFFER_BINDING_BASE: u32 = 0;
pub const BUFFER_BINDING_RANGE: DescriptorBindingRange =
    DescriptorBindingRange { start: 0, end: 32 };
pub const TEXTURE_BINDING_BASE: u32 = 32;
pub const TEXTURE_ARGUMENT_COUNT: u32 = 128;
pub const TEXTURE_ARGUMENT_COUNT_USIZE: usize = TEXTURE_ARGUMENT_COUNT as usize;
pub const TEXTURE_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: TEXTURE_BINDING_BASE,
    end: TEXTURE_BINDING_BASE + TEXTURE_ARGUMENT_COUNT,
};
pub const SAMPLER_BINDING_BASE: u32 = TEXTURE_BINDING_RANGE.end;
pub const SAMPLER_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: SAMPLER_BINDING_BASE,
    end: 192,
};
pub const SAMPLER_ARGUMENT_COUNT: u32 = 16;
pub const SAMPLER_ARGUMENT_COUNT_USIZE: usize = SAMPLER_ARGUMENT_COUNT as usize;
pub const COLOR_INPUT_BINDING_BASE: u32 = SAMPLER_BINDING_RANGE.end;
pub const COLOR_INPUT_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: COLOR_INPUT_BINDING_BASE,
    end: 200,
};
pub const IMAGEBLOCK_BINDING_BASE: u32 = COLOR_INPUT_BINDING_RANGE.end;
pub const IMAGEBLOCK_DATA_RATE_STRIDE: u32 = 3;
pub const IMAGEBLOCK_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: IMAGEBLOCK_BINDING_BASE,
    end: 224,
};
pub const FRAGMENT_IMAGEBLOCK_BINDING_BASE: u32 = IMAGEBLOCK_BINDING_RANGE.end;
pub const FRAGMENT_IMAGEBLOCK_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: FRAGMENT_IMAGEBLOCK_BINDING_BASE,
    end: 480,
};
pub const STORAGE_TEXTURE_BINDING_BASE: u32 = FRAGMENT_IMAGEBLOCK_BINDING_RANGE.end;
pub const STORAGE_TEXTURE_BINDING_RANGE: DescriptorBindingRange = DescriptorBindingRange {
    start: STORAGE_TEXTURE_BINDING_BASE,
    end: STORAGE_TEXTURE_BINDING_BASE + TEXTURE_ARGUMENT_COUNT,
};
pub const SYNTHETIC_BINDING_BASE: u32 = 640;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DescriptorLayout {
    pub version: u32,
    pub set: u32,
    pub buffers: DescriptorBindingRange,
    pub sampled_textures: DescriptorBindingRange,
    pub samplers: DescriptorBindingRange,
    pub color_inputs: DescriptorBindingRange,
    pub imageblocks: DescriptorBindingRange,
    pub fragment_imageblocks: DescriptorBindingRange,
    pub storage_textures: DescriptorBindingRange,
    pub synthetic: DescriptorBindingRange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorLayoutError {
    UnsupportedVersion {
        actual: u32,
        expected: u32,
    },
    ReversedRange {
        class: &'static str,
        start: u32,
        end: u32,
    },
    OverlappingRanges {
        left: &'static str,
        left_range: DescriptorBindingRange,
        right: &'static str,
        right_range: DescriptorBindingRange,
    },
    RangeOverflow {
        base: u32,
        count: u32,
    },
}

impl std::fmt::Display for DescriptorLayoutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion { actual, expected } => write!(
                formatter,
                "descriptor layout version {actual} is unsupported; expected {expected}"
            ),
            Self::ReversedRange { class, start, end } => write!(
                formatter,
                "descriptor layout {class} range [{start},{end}) is reversed"
            ),
            Self::OverlappingRanges {
                left,
                left_range,
                right,
                right_range,
            } => write!(
                formatter,
                "descriptor layout ranges {left} [{},{}) and {right} [{},{}) overlap",
                left_range.start, left_range.end, right_range.start, right_range.end
            ),
            Self::RangeOverflow { base, count } => write!(
                formatter,
                "descriptor binding range base {base} plus count {count} overflows u32"
            ),
        }
    }
}

impl std::error::Error for DescriptorLayoutError {}

pub const DEFAULT_DESCRIPTOR_LAYOUT: DescriptorLayout = DescriptorLayout {
    version: DESCRIPTOR_LAYOUT_VERSION,
    set: RESOURCE_DESCRIPTOR_SET,
    buffers: BUFFER_BINDING_RANGE,
    sampled_textures: TEXTURE_BINDING_RANGE,
    samplers: SAMPLER_BINDING_RANGE,
    color_inputs: COLOR_INPUT_BINDING_RANGE,
    imageblocks: IMAGEBLOCK_BINDING_RANGE,
    fragment_imageblocks: FRAGMENT_IMAGEBLOCK_BINDING_RANGE,
    storage_textures: STORAGE_TEXTURE_BINDING_RANGE,
    synthetic: DescriptorBindingRange {
        start: SYNTHETIC_BINDING_BASE,
        end: SYNTHETIC_BINDING_BASE + 32,
    },
};

impl Default for DescriptorLayout {
    fn default() -> Self {
        DEFAULT_DESCRIPTOR_LAYOUT
    }
}

impl DescriptorLayout {
    pub fn validate(self) -> Result<(), DescriptorLayoutError> {
        if self.version != DESCRIPTOR_LAYOUT_VERSION {
            return Err(DescriptorLayoutError::UnsupportedVersion {
                actual: self.version,
                expected: DESCRIPTOR_LAYOUT_VERSION,
            });
        }
        let ranges = [
            ("buffers", self.buffers),
            ("sampled textures", self.sampled_textures),
            ("samplers", self.samplers),
            ("color inputs", self.color_inputs),
            ("imageblocks", self.imageblocks),
            ("fragment imageblocks", self.fragment_imageblocks),
            ("storage textures", self.storage_textures),
            ("synthetic descriptors", self.synthetic),
        ];
        for (name, range) in ranges {
            if range.start > range.end {
                return Err(DescriptorLayoutError::ReversedRange {
                    class: name,
                    start: range.start,
                    end: range.end,
                });
            }
        }
        for (index, (left_name, left)) in ranges.iter().copied().enumerate() {
            if left.is_empty() {
                continue;
            }
            for (right_name, right) in ranges.iter().copied().skip(index + 1) {
                if !right.is_empty() && left.start < right.end && right.start < left.end {
                    return Err(DescriptorLayoutError::OverlappingRanges {
                        left: left_name,
                        left_range: left,
                        right: right_name,
                        right_range: right,
                    });
                }
            }
        }
        Ok(())
    }

    pub const fn buffer_binding(self, index: u32) -> Option<u32> {
        self.buffers.binding(index)
    }

    pub const fn sampled_texture_binding(self, index: u32) -> Option<u32> {
        self.sampled_textures.binding(index)
    }

    pub const fn storage_texture_binding(self, index: u32) -> Option<u32> {
        self.storage_textures.binding(index)
    }

    pub const fn sampler_binding(self, index: u32) -> Option<u32> {
        if index < SAMPLER_ARGUMENT_COUNT {
            self.samplers.binding(index)
        } else {
            None
        }
    }

    pub const fn color_input_binding(self, index: u32) -> Option<u32> {
        self.color_inputs.binding(index)
    }

    pub const fn imageblock_binding(self, attachment: u32, data_rate: u32) -> Option<u32> {
        if data_rate >= IMAGEBLOCK_DATA_RATE_STRIDE {
            return None;
        }
        let Some(offset) = attachment.checked_mul(IMAGEBLOCK_DATA_RATE_STRIDE) else {
            return None;
        };
        let Some(offset) = offset.checked_add(data_rate) else {
            return None;
        };
        self.imageblocks.binding(offset)
    }

    pub const fn fragment_imageblock_binding(self, member: u32) -> Option<u32> {
        self.fragment_imageblocks.binding(member)
    }
}

pub const fn buffer_resource_binding(index: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.buffer_binding(index)
}

pub const fn texture_resource_binding(index: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.sampled_texture_binding(index)
}

pub const fn storage_texture_resource_binding(index: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.storage_texture_binding(index)
}

pub const fn sampler_resource_binding(index: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.sampler_binding(index)
}

pub const fn color_input_resource_binding(index: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.color_input_binding(index)
}

pub const fn imageblock_resource_binding(attachment: u32, data_rate: u32) -> Option<u32> {
    DEFAULT_DESCRIPTOR_LAYOUT.imageblock_binding(attachment, data_rate)
}

pub const fn fragment_imageblock_resource_binding(master_member: u32) -> Option<u32> {
    FRAGMENT_IMAGEBLOCK_BINDING_RANGE.binding(master_member)
}

pub const ADDRESS_SPACE_DEVICE: u32 = 1;
pub const ADDRESS_SPACE_CONSTANT: u32 = 2;
pub const ADDRESS_SPACE_THREADGROUP: u32 = 3;
pub const THREADGROUP_BUFFER_ARGUMENT_COUNT: u32 = 32;
pub const THREADGROUP_BUFFER_ARGUMENT_COUNT_USIZE: usize =
    THREADGROUP_BUFFER_ARGUMENT_COUNT as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ShaderStage {
    Vertex,
    TessellationEvaluation,
    Fragment,
    Kernel,
}

impl From<crate::passes::Stage> for ShaderStage {
    fn from(stage: crate::passes::Stage) -> Self {
        match stage {
            crate::passes::Stage::Vertex => ShaderStage::Vertex,
            crate::passes::Stage::Fragment => ShaderStage::Fragment,
            crate::passes::Stage::Kernel => ShaderStage::Kernel,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResourceKind {
    Buffer,
    ThreadgroupBuffer,
    KernelStageInput,
    Texture,
    TextureArray,
    StorageImage,
    Sampler,
    StaticSampler,
    ColorInput,
    AccelerationStructureShadow,
    PrimitiveAccelerationStructure,
    VisibleFunctionTable,
    IntersectionFunctionTable,
    EmbeddedArgBufferTexture,
    EmbeddedArgBufferSampler,
    EmbeddedArgBufferBuffer,
    BufferAddressTable,
    RayInstanceUserIdTable,
    SynthesizedNullTexture,
    SynthesizedReadSampler,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DescriptorLocation {
    pub set: u32,
    pub binding: u32,
    pub count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResourceAccess {
    Unused,
    ReadOnly,
    WriteOnly,
    ReadWrite,
    Sampled,
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BufferExtent {
    Object { bytes: u32 },
    Unbounded,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BufferFootprint {
    pub static_ranges: Vec<BufferByteRange>,
    pub strided_accesses: Vec<BufferStridedAccess>,
    pub has_unbounded_access: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BufferByteRange {
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BufferStridedAccess {
    pub base_offset: u64,
    pub access_size: u64,
    pub terms: Vec<BufferStrideTerm>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BufferStrideTerm {
    pub source: BufferIndexSource,
    pub stride: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BufferIndexSource {
    VertexIndex,
    InstanceIndex,
    GlobalInvocationIdX,
    GlobalInvocationIdY,
    GlobalInvocationIdZ,
    LocalInvocationIdX,
    LocalInvocationIdY,
    LocalInvocationIdZ,
    WorkgroupIdX,
    WorkgroupIdY,
    WorkgroupIdZ,
    LocalInvocationIndex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerFilter {
    Nearest,
    Linear,
    Bicubic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerMipFilter {
    None,
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerAddressMode {
    ClampToZero,
    ClampToEdge,
    Repeat,
    MirroredRepeat,
    ClampToBorder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerCoordinates {
    Normalized,
    Pixel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerCompareFunction {
    None,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerBorderColor {
    TransparentBlack,
    OpaqueBlack,
    OpaqueWhite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SamplerReduction {
    WeightedAverage,
    Minimum,
    Maximum,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StaticSamplerState {
    pub min_filter: SamplerFilter,
    pub mag_filter: SamplerFilter,
    pub mip_filter: SamplerMipFilter,
    pub address_mode_s: SamplerAddressMode,
    pub address_mode_t: SamplerAddressMode,
    pub address_mode_r: SamplerAddressMode,
    pub coordinates: SamplerCoordinates,
    pub compare_function: SamplerCompareFunction,
    pub max_anisotropy: u32,
    pub lod_min_clamp: f32,
    pub lod_max_clamp: f32,
    pub border_color: SamplerBorderColor,
    pub reduction: SamplerReduction,
    pub lod_bias: f32,
    pub raw_words: [u64; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RuntimeSamplerState {
    pub min_filter: SamplerFilter,
    pub mag_filter: SamplerFilter,
    pub mip_filter: SamplerMipFilter,
    pub address_mode_s: SamplerAddressMode,
    pub address_mode_t: SamplerAddressMode,
    pub address_mode_r: SamplerAddressMode,
    pub coordinates: SamplerCoordinates,
    pub compare_function: SamplerCompareFunction,
    pub max_anisotropy: u32,
    pub lod_min_clamp: f32,
    pub lod_max_clamp: f32,
    pub border_color: SamplerBorderColor,
    pub reduction: SamplerReduction,
    pub lod_bias: f32,
}

impl RuntimeSamplerState {
    pub(crate) fn validate(self) -> Result<(), String> {
        if self.max_anisotropy == 0 {
            return Err("runtime sampler max_anisotropy must be at least 1".into());
        }
        if !self.lod_min_clamp.is_finite()
            || !self.lod_max_clamp.is_finite()
            || !self.lod_bias.is_finite()
        {
            return Err("runtime sampler LOD bounds and bias must be finite".into());
        }
        if self.lod_min_clamp > self.lod_max_clamp {
            return Err(format!(
                "runtime sampler minimum LOD {} exceeds maximum LOD {}",
                self.lod_min_clamp, self.lod_max_clamp
            ));
        }
        self.lowering_state().validate_lowering()
    }

    pub fn lowering_state(self) -> StaticSamplerState {
        StaticSamplerState {
            min_filter: self.min_filter,
            mag_filter: self.mag_filter,
            mip_filter: self.mip_filter,
            address_mode_s: self.address_mode_s,
            address_mode_t: self.address_mode_t,
            address_mode_r: self.address_mode_r,
            coordinates: self.coordinates,
            compare_function: self.compare_function,
            max_anisotropy: self.max_anisotropy,
            lod_min_clamp: self.lod_min_clamp,
            lod_max_clamp: self.lod_max_clamp,
            border_color: self.border_color,
            reduction: self.reduction,
            lod_bias: self.lod_bias,
            raw_words: [0; 2],
        }
    }
}

impl StaticSamplerState {
    pub const fn synthesized_read_sampler() -> Self {
        Self {
            min_filter: SamplerFilter::Nearest,
            mag_filter: SamplerFilter::Nearest,
            mip_filter: SamplerMipFilter::Nearest,
            address_mode_s: SamplerAddressMode::ClampToEdge,
            address_mode_t: SamplerAddressMode::ClampToEdge,
            address_mode_r: SamplerAddressMode::ClampToEdge,
            coordinates: SamplerCoordinates::Normalized,
            compare_function: SamplerCompareFunction::Never,
            max_anisotropy: 1,
            lod_min_clamp: 0.0,
            lod_max_clamp: 65504.0,
            border_color: SamplerBorderColor::TransparentBlack,
            reduction: SamplerReduction::WeightedAverage,
            lod_bias: 0.0,
            raw_words: [0; 2],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RuntimeSamplerSpecialization {
    pub metal_index: u32,
    pub state: RuntimeSamplerState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RuntimeStorageImageFormat {
    R8Unorm,
    Rgba8Unorm,
    Bgra8Unorm,
    R16Float,
    Rg16Float,
    Rg32Float,
    Rgba16Float,
    R32Float,
    Rgba32Float,
    R16Uint,
    R32Uint,
    Rgba8Uint,
    Rgba16Uint,
    Rgba32Uint,
    R32Sint,
    Rgba8Sint,
    Rgba16Sint,
    Rgba32Sint,
}

impl RuntimeStorageImageFormat {
    pub(crate) fn component(self) -> crate::meta::TextureComponent {
        use crate::meta::TextureComponent;
        match self {
            Self::R8Unorm
            | Self::Rgba8Unorm
            | Self::Bgra8Unorm
            | Self::R16Float
            | Self::Rg16Float
            | Self::Rg32Float
            | Self::Rgba16Float
            | Self::R32Float
            | Self::Rgba32Float => TextureComponent::Float,
            Self::R16Uint
            | Self::R32Uint
            | Self::Rgba8Uint
            | Self::Rgba16Uint
            | Self::Rgba32Uint => TextureComponent::Uint,
            Self::R32Sint | Self::Rgba8Sint | Self::Rgba16Sint | Self::Rgba32Sint => {
                TextureComponent::Sint
            }
        }
    }

    pub(crate) fn explicit_format(self) -> Option<crate::meta::TextureFormat> {
        use crate::meta::TextureFormat;
        match self {
            Self::R8Unorm => Some(TextureFormat::R8),
            Self::Rgba8Unorm => Some(TextureFormat::Rgba8),
            Self::R16Float => Some(TextureFormat::R16f),
            Self::Rg16Float => Some(TextureFormat::Rg16f),
            Self::Rg32Float => Some(TextureFormat::Rg32f),
            Self::Rgba16Float => Some(TextureFormat::Rgba16f),
            Self::R32Float => Some(TextureFormat::R32f),
            Self::Rgba32Float => Some(TextureFormat::Rgba32f),
            Self::R16Uint => Some(TextureFormat::R16ui),
            Self::R32Uint => Some(TextureFormat::R32ui),
            Self::Rgba8Uint => Some(TextureFormat::Rgba8ui),
            Self::Rgba16Uint => Some(TextureFormat::Rgba16ui),
            Self::Rgba32Uint => Some(TextureFormat::Rgba32ui),
            Self::R32Sint => Some(TextureFormat::R32i),
            Self::Rgba8Sint => Some(TextureFormat::Rgba8i),
            Self::Rgba16Sint => Some(TextureFormat::Rgba16i),
            Self::Rgba32Sint => Some(TextureFormat::Rgba32i),
            Self::Bgra8Unorm => None,
        }
    }

    pub(crate) fn supports_atomics(self) -> bool {
        matches!(self, Self::R32Uint | Self::R32Sint)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RuntimeStorageImageCapabilities {
    pub storage_image: bool,
    pub storage_image_atomic: bool,
    pub read_without_format: bool,
    pub write_without_format: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RuntimeStorageImageState {
    pub format: RuntimeStorageImageFormat,
    pub capabilities: RuntimeStorageImageCapabilities,
}

impl RuntimeStorageImageState {
    pub(crate) fn validate(self) -> Result<(), String> {
        if !self.capabilities.storage_image {
            return Err(format!(
                "runtime format {:?} lacks storage-image format support",
                self.format
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RuntimeStorageImageSpecialization {
    pub metal_index: u32,
    pub state: RuntimeStorageImageState,
    pub spirv_format: Option<crate::meta::TextureFormat>,
}

impl StaticSamplerState {
    pub(crate) fn from_air_words(words: [u64; 2]) -> Result<Self, String> {
        let word = words[0];
        let border_color = match (word >> 56) & 0x3 {
            0 => SamplerBorderColor::TransparentBlack,
            1 => SamplerBorderColor::OpaqueBlack,
            2 => SamplerBorderColor::OpaqueWhite,
            value => return Err(format!("unsupported AIR sampler border-color code {value}")),
        };
        let address = |shift: u32| match (word >> shift) & 0x7_u64 {
            0 if border_color != SamplerBorderColor::TransparentBlack => {
                Ok(SamplerAddressMode::ClampToBorder)
            }
            0 => Ok(SamplerAddressMode::ClampToZero),
            1 => Ok(SamplerAddressMode::ClampToEdge),
            2 => Ok(SamplerAddressMode::Repeat),
            3 => Ok(SamplerAddressMode::MirroredRepeat),
            value => Err(format!("unsupported AIR sampler address code {value}")),
        };
        let filter = |shift: u32| match (word >> shift) & 0x3_u64 {
            0 => Ok(SamplerFilter::Nearest),
            1 => Ok(SamplerFilter::Linear),
            2 => Ok(SamplerFilter::Bicubic),
            value => Err(format!("unsupported AIR sampler filter code {value}")),
        };
        let mip_filter = match (word >> 13) & 0x3 {
            0 => SamplerMipFilter::None,
            1 => SamplerMipFilter::Nearest,
            2 => SamplerMipFilter::Linear,
            value => return Err(format!("unsupported AIR sampler mip-filter code {value}")),
        };
        let coordinates = match (word >> 15) & 0x1 {
            0 => SamplerCoordinates::Normalized,
            _ => SamplerCoordinates::Pixel,
        };
        let compare_function = match (word >> 16) & 0xf {
            0 => SamplerCompareFunction::None,
            1 => SamplerCompareFunction::Less,
            2 => SamplerCompareFunction::LessEqual,
            3 => SamplerCompareFunction::Greater,
            4 => SamplerCompareFunction::GreaterEqual,
            5 => SamplerCompareFunction::Equal,
            6 => SamplerCompareFunction::NotEqual,
            7 => SamplerCompareFunction::Always,
            8 => SamplerCompareFunction::Never,
            value => {
                return Err(format!(
                    "unsupported AIR sampler compare-function code {value}"
                ))
            }
        };
        let reduction = match (word >> 58) & 0x3 {
            0 => SamplerReduction::WeightedAverage,
            1 => SamplerReduction::Minimum,
            2 => SamplerReduction::Maximum,
            value => return Err(format!("unsupported AIR sampler reduction code {value}")),
        };
        let min_half = ((word >> 24) & 0xffff) as u16;
        let max_half = ((word >> 40) & 0xffff) as u16;
        let bias_half = (words[1] & 0xffff) as u16;
        Ok(Self {
            min_filter: filter(11)?,
            mag_filter: filter(9)?,
            mip_filter,
            address_mode_s: address(0)?,
            address_mode_t: address(3)?,
            address_mode_r: address(6)?,
            coordinates,
            compare_function,
            max_anisotropy: (((word >> 20) & 0xf) as u32) + 1,
            lod_min_clamp: f16_bits_to_f32(min_half),
            lod_max_clamp: f16_bits_to_f32(max_half),
            border_color,
            reduction,
            lod_bias: f16_bits_to_f32(bias_half),
            raw_words: words,
        })
    }

    pub(crate) fn validate_lowering(self) -> Result<(), String> {
        if self.coordinates != SamplerCoordinates::Pixel {
            return Ok(());
        }
        if self.min_filter != self.mag_filter {
            return Err(
                "pixel-coordinate samplers with mixed min/mag filters are unsupported because AIR does not expose the pipeline derivative state needed to select the filter"
                    .into(),
            );
        }
        if self.mip_filter == SamplerMipFilter::Linear {
            return Err(
                "pixel-coordinate samplers with linear mip filtering are unsupported".into(),
            );
        }
        if self.max_anisotropy > 1 {
            return Err("pixel-coordinate sampler anisotropy cannot be emulated exactly".into());
        }
        if self.lod_bias != 0.0 {
            return Err("pixel-coordinate sampler LOD bias cannot be emulated exactly".into());
        }
        if self.lod_min_clamp != 0.0 {
            return Err(format!(
                "pixel-coordinate sampler minimum LOD clamp {} excludes the level zero the emulation reads",
                self.lod_min_clamp
            ));
        }
        if self.reduction != SamplerReduction::WeightedAverage {
            return Err("pixel-coordinate sampler min/max reduction is unsupported".into());
        }
        if self.border_color != SamplerBorderColor::TransparentBlack
            && [
                self.address_mode_s,
                self.address_mode_t,
                self.address_mode_r,
            ]
            .contains(&SamplerAddressMode::ClampToBorder)
        {
            return Err(
                "pixel-coordinate samplers support only transparent-black border emulation".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn uses_pixel_nearest(self) -> bool {
        self.uses_pixel_coordinates()
            && self.min_filter == SamplerFilter::Nearest
            && self.mag_filter == SamplerFilter::Nearest
    }

    pub(crate) fn uses_linear_filter(self) -> bool {
        self.min_filter == SamplerFilter::Linear && self.mag_filter == SamplerFilter::Linear
    }

    pub(crate) fn uses_bicubic_filter(self) -> bool {
        self.min_filter == SamplerFilter::Bicubic && self.mag_filter == SamplerFilter::Bicubic
    }

    pub(crate) fn uses_pixel_coordinates(self) -> bool {
        self.coordinates == SamplerCoordinates::Pixel
    }

    pub(crate) fn spatial_clamps_to_zero(self, dimension: usize) -> bool {
        match self.spatial_address_mode(dimension) {
            Some(SamplerAddressMode::ClampToZero) => true,
            Some(SamplerAddressMode::ClampToBorder) => {
                self.border_color == SamplerBorderColor::TransparentBlack
            }
            _ => false,
        }
    }

    pub(crate) fn spatial_address_mode(self, dimension: usize) -> Option<SamplerAddressMode> {
        [
            self.address_mode_s,
            self.address_mode_t,
            self.address_mode_r,
        ]
        .get(dimension)
        .copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct EmbeddedArgBuffer {
    pub buffer_param_index: u32,
    pub buffer_index: u32,
    pub field_offset: u32,
    pub field_ordinal: u32,
    pub argument_index: u32,
    pub resource_buffer_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ResourceBinding {
    pub kind: ResourceKind,
    pub metal_index: u32,
    pub descriptor: Option<DescriptorLocation>,
    pub param_index: Option<u32>,
    pub stage_input_location: Option<u32>,
    pub address_space: Option<u32>,
    pub declared_size: Option<u32>,
    pub extent: Option<BufferExtent>,
    pub footprint: Option<BufferFootprint>,
    pub type_layout: Option<AirType>,
    pub type_name: Option<String>,
    pub texture_shape: Option<TextureShape>,
    pub embedded_source: Option<EmbeddedArgBuffer>,
    pub access: Option<ResourceAccess>,
    pub static_sampler: Option<StaticSamplerState>,
}

impl ResourceBinding {
    fn descriptor_at(binding: Option<u32>) -> Option<DescriptorLocation> {
        Some(DescriptorLocation {
            set: RESOURCE_DESCRIPTOR_SET,
            binding: binding?,
            count: 1,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VertexAttribute {
    pub location: u32,
    pub type_name: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Varying {
    pub location: u32,
    pub type_name: Option<String>,
    pub name: Option<String>,
    pub user_semantic: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VertexBuiltins {
    pub uses_vertex_index: bool,
    pub uses_instance_index: bool,
    pub writes_position: bool,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RenderTarget {
    pub member_index: u32,
    pub location: u32,
    pub type_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImageblockLayout {
    pub param_index: u32,
    pub type_layout: AirType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImplicitImageblockAttachment {
    pub attachment: u32,
    pub data_rate: u32,
    pub max_index: Option<u32>,
    pub binding: u32,
    pub format: crate::meta::TextureFormat,
    pub access: ResourceAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblockMember {
    pub offset: u32,
    pub size: u32,
    pub type_name: String,
    pub semantic: String,
    pub raster_order_group: u32,
    pub binding: Option<u32>,
    pub access: ResourceAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblock {
    pub sample_size: u32,
    pub members: Vec<FragmentImageblockMember>,
    pub inputs: Vec<crate::meta::FragmentImageblockProjection>,
    pub outputs: Vec<crate::meta::FragmentImageblockProjection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TessellationInterface {
    pub domain: crate::meta::PatchDomain,
    pub control_point_count: u32,
    pub control_point_locations: Vec<u32>,
    pub patch_input_locations: Vec<u32>,
    pub control_point_attributes: Vec<TessellationAttribute>,
    pub patch_attributes: Vec<TessellationAttribute>,
    pub instance_id: Option<TessellationAttribute>,
    pub amplification_id: Option<TessellationAttribute>,
    pub amplification_count: Option<TessellationAttribute>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TessellationAttribute {
    pub location: u32,
    pub type_name: Option<String>,
}

fn tessellation_system_attribute(
    meta: &VertMeta,
    expected_role: &VertRole,
) -> Option<TessellationAttribute> {
    let (parameter, role) = meta
        .roles
        .iter()
        .find(|(_, role)| *role == *expected_role)?;
    Some(TessellationAttribute {
        location: meta.tessellation_system_input_location(role)?,
        type_name: meta.parameter_type_names.get(parameter).cloned(),
    })
}

fn implicit_imageblock_planes(
    attachments: &[crate::meta::ImplicitImageblockAttachment],
) -> Vec<ImplicitImageblockAttachment> {
    attachments
        .iter()
        .map(|attachment| ImplicitImageblockAttachment {
            attachment: attachment.attachment,
            data_rate: attachment.data_rate,
            max_index: attachment.max_index,
            binding: imageblock_resource_binding(attachment.attachment, attachment.data_rate)
                .unwrap_or(IMAGEBLOCK_BINDING_RANGE.end),
            format: attachment.format,
            access: match (attachment.reads, attachment.writes) {
                (true, true) => ResourceAccess::ReadWrite,
                (true, false) => ResourceAccess::ReadOnly,
                (false, true) => ResourceAccess::WriteOnly,
                (false, false) => ResourceAccess::Unused,
            },
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct EmittedImage {
    dimension: TextureDimension,
    arrayed: bool,
    multisampled: bool,
    component: TextureComponent,
    writable: bool,
    storage_format: Option<crate::meta::TextureFormat>,
}

impl EmittedImage {
    fn of(
        types: &crate::fxhash::HashMap<spirv::Word, &crate::spirv_module::Instruction>,
        image: spirv::Word,
    ) -> Option<Self> {
        let instruction = types.get(&image)?;
        if instruction.class.opcode != spirv::Op::TypeImage {
            return None;
        }
        let literal = |index: usize| match instruction.operands.get(index) {
            Some(crate::spirv_module::Operand::LiteralBit32(value)) => Some(*value),
            _ => None,
        };
        let dimension = match instruction.operands.get(1) {
            Some(crate::spirv_module::Operand::Dim(dim)) => TextureDimension::from_spirv_dim(*dim),
            _ => return None,
        };
        let sampled_type = match instruction.operands.first() {
            Some(crate::spirv_module::Operand::IdRef(scalar)) => types.get(scalar)?,
            _ => return None,
        };
        let component = match (sampled_type.class.opcode, sampled_type.operands.get(1)) {
            (spirv::Op::TypeFloat, _) => TextureComponent::Float,
            (spirv::Op::TypeInt, Some(crate::spirv_module::Operand::LiteralBit32(1))) => {
                TextureComponent::Sint
            }
            (spirv::Op::TypeInt, _) => TextureComponent::Uint,
            _ => return None,
        };
        let storage_format = match instruction.operands.get(6) {
            Some(crate::spirv_module::Operand::ImageFormat(format)) => {
                crate::meta::TextureFormat::from_spirv_format(*format)
            }
            _ => None,
        };
        Some(Self {
            dimension,
            arrayed: literal(3)? != 0,
            multisampled: literal(4)? != 0,
            component,
            writable: literal(5)? == 2,
            storage_format,
        })
    }
}

fn pointee_of(
    types: &crate::fxhash::HashMap<spirv::Word, &crate::spirv_module::Instruction>,
    pointer: spirv::Word,
) -> Option<spirv::Word> {
    let instruction = types.get(&pointer)?;
    if instruction.class.opcode != spirv::Op::TypePointer {
        return None;
    }
    let mut pointee = match instruction.operands.get(1) {
        Some(crate::spirv_module::Operand::IdRef(pointee)) => *pointee,
        _ => return None,
    };
    while let Some(element) = types.get(&pointee).and_then(|instruction| {
        matches!(
            instruction.class.opcode,
            spirv::Op::TypeArray | spirv::Op::TypeRuntimeArray
        )
        .then(|| match instruction.operands.first() {
            Some(crate::spirv_module::Operand::IdRef(element)) => Some(*element),
            _ => None,
        })
        .flatten()
    }) {
        pointee = element;
    }
    Some(pointee)
}

impl EmittedImage {
    fn texture_shape(self) -> TextureShape {
        TextureShape {
            dimension: self.dimension,
            arrayed: self.arrayed,
            multisampled: self.multisampled,
            component: self.component,
            writable: self.writable,
            array_ref: false,
            array_length: None,
            storage_format: self.storage_format,
        }
    }
}

fn descriptor_binding_of(module: &Module, variable: spirv::Word) -> Option<u32> {
    module.annotations.iter().find_map(|annotation| {
        match (
            annotation.class.opcode,
            annotation.operands.first(),
            annotation.operands.get(1),
            annotation.operands.get(2),
        ) {
            (
                spirv::Op::Decorate,
                Some(crate::spirv_module::Operand::IdRef(target)),
                Some(crate::spirv_module::Operand::Decoration(spirv::Decoration::Binding)),
                Some(crate::spirv_module::Operand::LiteralBit32(binding)),
            ) if *target == variable => Some(*binding),
            _ => None,
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SamplePositionPushConstantRange {
    pub offset: u32,
    pub size: u32,
    pub positions: u32,
    pub stride: u32,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ShaderReflection {
    pub reflection_version: u32,
    #[cfg_attr(feature = "serde", serde(default))]
    pub descriptor_layout: DescriptorLayout,
    pub stage: ShaderStage,
    pub entry_point: Option<String>,
    pub bindings: Vec<ResourceBinding>,
    pub argument_buffer_fields: Vec<EmbeddedArgBuffer>,
    pub vertex_attributes: Vec<VertexAttribute>,
    pub varyings: Vec<Varying>,
    pub render_targets: Vec<RenderTarget>,
    pub depth_members: Vec<u32>,
    pub depth_qualifier: Option<crate::meta::DepthQualifier>,
    pub stencil_members: Vec<u32>,
    pub local_size: Option<[u32; 3]>,
    pub max_work_group_size: Option<u32>,
    pub kernel_dispatch: Option<KernelDispatch>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub fragment_sample_positions: Option<SamplePositionPushConstantRange>,
    pub vertex_builtins: Option<VertexBuiltins>,
    pub tessellation: Option<TessellationInterface>,
    pub imageblock_layouts: Vec<ImageblockLayout>,
    pub implicit_imageblock_attachments: Vec<ImplicitImageblockAttachment>,
    pub fragment_imageblock: Option<FragmentImageblock>,
    pub datalayout: Option<String>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub runtime_sampler_specializations: Vec<RuntimeSamplerSpecialization>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub runtime_storage_image_specializations: Vec<RuntimeStorageImageSpecialization>,
    pub function_constants: Vec<FunctionConstant>,
}

pub(crate) fn has_sample_position_payload(module: &Module) -> bool {
    use crate::spirv_module::Operand;
    use spirv::{Decoration, Op, StorageClass};
    let defs = module
        .types_global_values
        .iter()
        .filter_map(|i| i.result_id.map(|id| (id, i)))
        .collect::<crate::fxhash::HashMap<_, _>>();
    let id = |operand: &Operand| match operand {
        Operand::IdRef(id) => Some(*id),
        _ => None,
    };
    module.types_global_values.iter().any(|var| {
        let Some(ptr) = var.result_type.and_then(|t| defs.get(&t)) else {
            return false;
        };
        if var.class.opcode != Op::Variable
            || var.operands.first() != Some(&Operand::StorageClass(StorageClass::PushConstant))
        {
            return false;
        }
        let Some(block_id) = ptr.operands.get(1).and_then(id) else {
            return false;
        };
        let Some(block) = defs.get(&block_id) else {
            return false;
        };
        if block.class.opcode != Op::TypeStruct || block.operands.len() != 1 {
            return false;
        }
        let Some(array_id) = block.operands.first().and_then(id) else {
            return false;
        };
        let Some(array) = defs.get(&array_id) else {
            return false;
        };
        if array.class.opcode != Op::TypeArray {
            return false;
        }
        let Some(length) = array
            .operands
            .get(1)
            .and_then(id)
            .and_then(|i| defs.get(&i))
        else {
            return false;
        };
        if length.class.opcode != Op::Constant
            || length.operands.first() != Some(&Operand::LiteralBit32(8))
        {
            return false;
        }
        let Some(vec) = array
            .operands
            .first()
            .and_then(id)
            .and_then(|i| defs.get(&i))
        else {
            return false;
        };
        if vec.class.opcode != Op::TypeVector
            || vec.operands.get(1) != Some(&Operand::LiteralBit32(2))
        {
            return false;
        }
        let Some(float) = vec.operands.first().and_then(id).and_then(|i| defs.get(&i)) else {
            return false;
        };
        if float.class.opcode != Op::TypeFloat
            || float.operands.first() != Some(&Operand::LiteralBit32(32))
        {
            return false;
        }
        let offset = module.annotations.iter().any(|i| {
            i.class.opcode == Op::MemberDecorate
                && i.operands
                    == vec![
                        Operand::IdRef(block_id),
                        Operand::LiteralBit32(0),
                        Operand::Decoration(Decoration::Offset),
                        Operand::LiteralBit32(96),
                    ]
        });
        let stride = module.annotations.iter().any(|i| {
            i.class.opcode == Op::Decorate
                && i.operands
                    == vec![
                        Operand::IdRef(array_id),
                        Operand::Decoration(Decoration::ArrayStride),
                        Operand::LiteralBit32(8),
                    ]
        });
        offset && stride
    })
}

impl ShaderReflection {
    pub(crate) fn apply_descriptor_layout(
        &mut self,
        layout: DescriptorLayout,
    ) -> Result<(), String> {
        layout.validate().map_err(|error| error.to_string())?;
        for resource in &mut self.bindings {
            let Some(descriptor) = resource.descriptor.as_mut() else {
                continue;
            };
            let storage_texture = matches!(resource.kind, ResourceKind::StorageImage)
                || matches!(
                    resource.kind,
                    ResourceKind::TextureArray | ResourceKind::EmbeddedArgBufferTexture
                ) && resource.access == Some(ResourceAccess::Storage);
            let binding = match resource.kind {
                ResourceKind::Buffer
                | ResourceKind::KernelStageInput
                | ResourceKind::AccelerationStructureShadow
                | ResourceKind::PrimitiveAccelerationStructure => {
                    layout.buffer_binding(resource.metal_index)
                }
                ResourceKind::Texture
                | ResourceKind::TextureArray
                | ResourceKind::StorageImage
                | ResourceKind::EmbeddedArgBufferTexture => {
                    if storage_texture {
                        layout.storage_texture_binding(resource.metal_index)
                    } else {
                        layout.sampled_texture_binding(resource.metal_index)
                    }
                }
                ResourceKind::Sampler => layout.sampler_binding(resource.metal_index),
                ResourceKind::EmbeddedArgBufferSampler => {
                    layout.samplers.binding(resource.metal_index)
                }
                ResourceKind::ColorInput => layout.color_input_binding(resource.metal_index),
                ResourceKind::StaticSampler
                | ResourceKind::BufferAddressTable
                | ResourceKind::RayInstanceUserIdTable
                | ResourceKind::SynthesizedNullTexture
                | ResourceKind::SynthesizedReadSampler => {
                    return Err(format!(
                        "cannot reconfigure reflection after synthesized {:?} resources were added",
                        resource.kind
                    ));
                }
                ResourceKind::ThreadgroupBuffer
                | ResourceKind::VisibleFunctionTable
                | ResourceKind::IntersectionFunctionTable
                | ResourceKind::EmbeddedArgBufferBuffer => None,
            }
            .ok_or_else(|| {
                format!(
                    "{:?} resource {} exceeds the selected descriptor layout",
                    resource.kind, resource.metal_index
                )
            })?;
            descriptor.set = layout.set;
            descriptor.binding = binding;
        }
        for attachment in &mut self.implicit_imageblock_attachments {
            attachment.binding = layout
                .imageblock_binding(attachment.attachment, attachment.data_rate)
                .ok_or_else(|| {
                    format!(
                        "implicit imageblock attachment {} rate {} exceeds the selected descriptor layout",
                        attachment.attachment, attachment.data_rate
                    )
                })?;
        }
        if let Some(imageblock) = &mut self.fragment_imageblock {
            for (index, member) in imageblock.members.iter_mut().enumerate() {
                if member.binding.is_some() {
                    member.binding = Some(
                        layout
                            .fragment_imageblock_binding(index as u32)
                            .ok_or_else(|| {
                                format!(
                                    "fragment imageblock member {index} exceeds the selected descriptor layout"
                                )
                            })?,
                    );
                }
            }
        }
        self.descriptor_layout = layout;
        Ok(())
    }

    pub fn validate_descriptor_abi(&self) -> Result<(), String> {
        self.descriptor_layout
            .validate()
            .map_err(|error| error.to_string())?;
        match (self.stage, self.kernel_dispatch) {
            (ShaderStage::Kernel, Some(dispatch)) => dispatch.validate()?,
            (ShaderStage::Kernel, None) => {
                return Err("kernel reflection is missing its dispatch-grid contract".to_string())
            }
            (_, Some(_)) => {
                return Err(
                    "non-kernel reflection carries a kernel dispatch-grid contract".to_string(),
                )
            }
            (_, None) => {}
        }
        for resource in &self.bindings {
            let (Some(layout), Some(declared)) = (&resource.type_layout, resource.declared_size)
            else {
                continue;
            };
            let Some(extent) = crate::layout::air_metadata_extent(layout) else {
                continue;
            };
            if extent > u64::from(declared) {
                return Err(format!(
                    "reflection reports a {extent}-byte member layout for the {declared}-byte argument of {:?}({})",
                    resource.kind, resource.metal_index
                ));
            }
        }
        for (index, resource) in self.bindings.iter().enumerate() {
            if let Some(first) = self.bindings[..index]
                .iter()
                .position(|other| other == resource)
            {
                return Err(format!(
                    "reflection reports {:?}({}) twice, identically, at bindings[{first}] and bindings[{index}]",
                    resource.kind, resource.metal_index
                ));
            }
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum DescriptorClass {
            StorageBuffer,
            SampledImage,
            UniformTexelBuffer,
            StorageImage,
            StorageTexelBuffer,
            Sampler,
            InputAttachment,
        }

        let mut occupied =
            std::collections::BTreeMap::<(u32, u32), (String, DescriptorClass, u32)>::new();
        let mut record = |location: DescriptorLocation,
                          owner: String,
                          class: DescriptorClass|
         -> Result<(), String> {
            if location.set != self.descriptor_layout.set {
                return Err(format!(
                    "{owner} uses descriptor set {}, expected {}",
                    location.set, self.descriptor_layout.set
                ));
            }
            if location.count == 0 {
                return Err(format!("{owner} has zero descriptor count"));
            }
            if let Some((previous, previous_class, previous_count)) = occupied.insert(
                (location.set, location.binding),
                (owner.clone(), class, location.count),
            ) {
                if previous_class != class {
                    return Err(format!(
                        "descriptor set {} binding {} is shared incompatibly by {previous} ({previous_class:?}, count {previous_count}) and {owner} ({class:?}, count {})",
                        location.set, location.binding, location.count
                    ));
                }
            }
            Ok(())
        };

        for resource in &self.bindings {
            let storage_texture = matches!(resource.kind, ResourceKind::StorageImage)
                || matches!(
                    resource.kind,
                    ResourceKind::TextureArray | ResourceKind::EmbeddedArgBufferTexture
                ) && resource.access == Some(ResourceAccess::Storage);
            let texel_buffer = resource
                .texture_shape
                .is_some_and(|shape| shape.dimension == TextureDimension::Buffer);
            let class = match resource.kind {
                ResourceKind::Buffer
                | ResourceKind::KernelStageInput
                | ResourceKind::AccelerationStructureShadow
                | ResourceKind::PrimitiveAccelerationStructure
                | ResourceKind::BufferAddressTable
                | ResourceKind::RayInstanceUserIdTable => DescriptorClass::StorageBuffer,
                ResourceKind::Texture
                | ResourceKind::TextureArray
                | ResourceKind::StorageImage
                | ResourceKind::EmbeddedArgBufferTexture => {
                    if storage_texture && texel_buffer {
                        DescriptorClass::StorageTexelBuffer
                    } else if storage_texture {
                        DescriptorClass::StorageImage
                    } else if texel_buffer {
                        DescriptorClass::UniformTexelBuffer
                    } else {
                        DescriptorClass::SampledImage
                    }
                }
                ResourceKind::Sampler
                | ResourceKind::StaticSampler
                | ResourceKind::EmbeddedArgBufferSampler
                | ResourceKind::SynthesizedReadSampler => DescriptorClass::Sampler,
                ResourceKind::SynthesizedNullTexture => DescriptorClass::SampledImage,
                ResourceKind::ColorInput => DescriptorClass::InputAttachment,
                ResourceKind::ThreadgroupBuffer
                | ResourceKind::VisibleFunctionTable
                | ResourceKind::IntersectionFunctionTable
                | ResourceKind::EmbeddedArgBufferBuffer => DescriptorClass::StorageBuffer,
            };
            let expected = match resource.kind {
                ResourceKind::Buffer
                | ResourceKind::KernelStageInput
                | ResourceKind::AccelerationStructureShadow => {
                    Some(self.descriptor_layout.buffer_binding(resource.metal_index))
                }
                ResourceKind::PrimitiveAccelerationStructure if resource.descriptor.is_some() => {
                    Some(self.descriptor_layout.buffer_binding(resource.metal_index))
                }
                ResourceKind::Texture
                | ResourceKind::TextureArray
                | ResourceKind::StorageImage
                | ResourceKind::EmbeddedArgBufferTexture => Some(if storage_texture {
                    self.descriptor_layout
                        .storage_texture_binding(resource.metal_index)
                } else {
                    self.descriptor_layout
                        .sampled_texture_binding(resource.metal_index)
                }),
                ResourceKind::Sampler => {
                    Some(self.descriptor_layout.sampler_binding(resource.metal_index))
                }
                ResourceKind::StaticSampler | ResourceKind::EmbeddedArgBufferSampler => Some(
                    self.descriptor_layout
                        .samplers
                        .binding(resource.metal_index),
                ),
                ResourceKind::ColorInput => Some(
                    self.descriptor_layout
                        .color_input_binding(resource.metal_index),
                ),
                ResourceKind::BufferAddressTable
                | ResourceKind::RayInstanceUserIdTable
                | ResourceKind::SynthesizedNullTexture
                | ResourceKind::SynthesizedReadSampler => None,
                ResourceKind::ThreadgroupBuffer
                | ResourceKind::PrimitiveAccelerationStructure
                | ResourceKind::VisibleFunctionTable
                | ResourceKind::IntersectionFunctionTable
                | ResourceKind::EmbeddedArgBufferBuffer => {
                    if resource.descriptor.is_some() {
                        return Err(format!(
                            "{:?} resource {} unexpectedly consumes a descriptor",
                            resource.kind, resource.metal_index
                        ));
                    }
                    continue;
                }
            };
            let owner = format!("{:?}({})", resource.kind, resource.metal_index);
            if let Some(band) = match resource.kind {
                ResourceKind::SynthesizedNullTexture => {
                    Some(self.descriptor_layout.sampled_textures)
                }
                ResourceKind::SynthesizedReadSampler => Some(self.descriptor_layout.samplers),
                _ => None,
            } {
                let location = resource
                    .descriptor
                    .ok_or_else(|| format!("{owner} is missing its descriptor"))?;
                if !band.contains(location.binding) {
                    return Err(format!(
                        "{owner} uses binding {}, outside its descriptor band [{},{})",
                        location.binding, band.start, band.end
                    ));
                }
                record(location, owner, class)?;
                continue;
            }
            if resource.kind == ResourceKind::RayInstanceUserIdTable {
                let location = resource
                    .descriptor
                    .ok_or_else(|| format!("{owner} is missing its descriptor"))?;
                if location.binding != RAY_INSTANCE_USER_ID_TABLE_BINDING {
                    return Err(format!(
                        "{owner} does not use reserved ray instance user-ID binding"
                    ));
                }
                if self.bindings.iter().any(|other| {
                    other.kind != ResourceKind::RayInstanceUserIdTable
                        && other
                            .descriptor
                            .is_some_and(|d| d.set == location.set && d.binding == location.binding)
                }) {
                    return Err(format!("reserved ray instance user-ID table binding {} collides with another resource", location.binding));
                }
                record(location, owner, class)?;
                continue;
            }
            if resource.kind == ResourceKind::BufferAddressTable {
                let location = resource
                    .descriptor
                    .ok_or_else(|| format!("{owner} is missing its descriptor"))?;
                if !self.descriptor_layout.synthetic.contains(location.binding) {
                    return Err(format!(
                        "{owner} uses binding {}, outside synthetic descriptor range [{},{})",
                        location.binding,
                        self.descriptor_layout.synthetic.start,
                        self.descriptor_layout.synthetic.end
                    ));
                }
                record(location, owner, class)?;
                continue;
            }
            let expected = expected
                .flatten()
                .ok_or_else(|| format!("{owner} exceeds its descriptor ABI band"))?;
            let location = resource
                .descriptor
                .ok_or_else(|| format!("{owner} is missing its descriptor"))?;
            if location.binding != expected {
                return Err(format!(
                    "{owner} uses binding {}, expected {expected}",
                    location.binding
                ));
            }
            record(location, owner, class)?;
        }

        for attachment in &self.implicit_imageblock_attachments {
            let owner = format!(
                "implicit imageblock attachment {} rate {}",
                attachment.attachment, attachment.data_rate
            );
            let expected = self
                .descriptor_layout
                .imageblock_binding(attachment.attachment, attachment.data_rate)
                .ok_or_else(|| format!("{owner} exceeds its descriptor ABI band"))?;
            if attachment.binding != expected {
                return Err(format!(
                    "{owner} uses binding {}, expected {expected}",
                    attachment.binding
                ));
            }
            record(
                DescriptorLocation {
                    set: self.descriptor_layout.set,
                    binding: attachment.binding,
                    count: 1,
                },
                owner,
                DescriptorClass::StorageImage,
            )?;
        }
        if let Some(imageblock) = &self.fragment_imageblock {
            for (index, member) in imageblock.members.iter().enumerate() {
                let Some(binding) = member.binding else {
                    continue;
                };
                let owner = format!("fragment imageblock member {index}");
                let expected = self
                    .descriptor_layout
                    .fragment_imageblock_binding(index as u32)
                    .ok_or_else(|| format!("{owner} exceeds its descriptor ABI band"))?;
                if binding != expected {
                    return Err(format!(
                        "{owner} uses binding {binding}, expected {expected}"
                    ));
                }
                record(
                    DescriptorLocation {
                        set: self.descriptor_layout.set,
                        binding,
                        count: 1,
                    },
                    owner,
                    DescriptorClass::StorageImage,
                )?;
            }
        }
        let mut specialized_sampler_indices = std::collections::BTreeSet::new();
        for specialization in &self.runtime_sampler_specializations {
            specialization.state.validate().map_err(|error| {
                format!(
                    "runtime sampler {} specialization is invalid: {error}",
                    specialization.metal_index
                )
            })?;
            if specialization.metal_index >= SAMPLER_ARGUMENT_COUNT {
                return Err(format!(
                    "runtime sampler specialization index {} exceeds Metal sampler range 0..{SAMPLER_ARGUMENT_COUNT}",
                    specialization.metal_index
                ));
            }
            if !specialized_sampler_indices.insert(specialization.metal_index) {
                return Err(format!(
                    "runtime sampler {} is specialized more than once",
                    specialization.metal_index
                ));
            }
            if !self.bindings.iter().any(|binding| {
                binding.kind == ResourceKind::Sampler
                    && binding.metal_index == specialization.metal_index
            }) {
                return Err(format!(
                    "runtime sampler {} specialization has no matching AIR sampler binding",
                    specialization.metal_index
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn add_buffer_footprints(&mut self, module: &Module) -> Result<(), String> {
        footprint::attach_buffer_footprints(self, module)
    }

    pub(crate) fn reconcile_texture_shapes(&mut self, module: &Module) {
        let types = module
            .types_global_values
            .iter()
            .filter_map(|instruction| Some((instruction.result_id?, instruction)))
            .collect::<crate::fxhash::HashMap<_, _>>();
        let mut declared = crate::fxhash::HashMap::<u32, Option<EmittedImage>>::default();
        for instruction in module
            .types_global_values
            .iter()
            .filter(|instruction| instruction.class.opcode == spirv::Op::Variable)
        {
            let Some(variable) = instruction.result_id else {
                continue;
            };
            let Some(binding) = descriptor_binding_of(module, variable) else {
                continue;
            };
            let Some(image) = instruction
                .result_type
                .and_then(|pointer| pointee_of(&types, pointer))
                .and_then(|pointee| EmittedImage::of(&types, pointee))
            else {
                continue;
            };
            match declared.entry(binding) {
                std::collections::hash_map::Entry::Occupied(mut seen) => {
                    if seen.get() != &Some(image) {
                        seen.insert(None);
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some(image));
                }
            }
        }
        for resource in &mut self.bindings {
            let Some(shape) = resource.texture_shape.as_mut() else {
                continue;
            };
            let Some(binding) = resource.descriptor.map(|location| location.binding) else {
                continue;
            };
            let Some(Some(image)) = declared.get(&binding) else {
                continue;
            };
            shape.dimension = image.dimension;
            shape.arrayed = image.arrayed;
            shape.multisampled = image.multisampled;
            shape.component = image.component;
            shape.writable = image.writable;
            shape.storage_format = image.storage_format;
        }
    }

    pub(crate) fn retract_unbound_static_samplers(&mut self, module: &Module) {
        let types = module
            .types_global_values
            .iter()
            .filter_map(|instruction| Some((instruction.result_id?, instruction)))
            .collect::<crate::fxhash::HashMap<_, _>>();
        let bound = module
            .types_global_values
            .iter()
            .filter(|instruction| instruction.class.opcode == spirv::Op::Variable)
            .filter_map(|instruction| {
                let variable = instruction.result_id?;
                let pointee = pointee_of(&types, instruction.result_type?)?;
                (types.get(&pointee)?.class.opcode == spirv::Op::TypeSampler)
                    .then(|| descriptor_binding_of(module, variable))
                    .flatten()
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.bindings.retain(|resource| {
            resource.kind != ResourceKind::StaticSampler
                || resource
                    .descriptor
                    .is_none_or(|location| bound.contains(&location.binding))
        });
    }

    pub(crate) fn report_synthesized_placeholders(&mut self, module: &Module, bindings: &[u32]) {
        self.bindings.retain(|resource| {
            !matches!(
                resource.kind,
                ResourceKind::SynthesizedNullTexture | ResourceKind::SynthesizedReadSampler
            )
        });
        let types = module
            .types_global_values
            .iter()
            .filter_map(|instruction| Some((instruction.result_id?, instruction)))
            .collect::<crate::fxhash::HashMap<_, _>>();
        let mut null_textures = 0;
        let mut read_samplers = 0;
        for &binding in bindings {
            let Some(pointee) = module
                .types_global_values
                .iter()
                .filter(|instruction| instruction.class.opcode == spirv::Op::Variable)
                .filter(|instruction| {
                    instruction
                        .result_id
                        .map(|variable| descriptor_binding_of(module, variable) == Some(binding))
                        == Some(true)
                })
                .find_map(|instruction| pointee_of(&types, instruction.result_type?))
            else {
                continue;
            };
            let (kind, metal_index, texture_shape, access) = match types
                .get(&pointee)
                .map(|instruction| instruction.class.opcode)
            {
                Some(spirv::Op::TypeImage) => {
                    let index = null_textures;
                    null_textures += 1;
                    (
                        ResourceKind::SynthesizedNullTexture,
                        index,
                        EmittedImage::of(&types, pointee).map(EmittedImage::texture_shape),
                        Some(ResourceAccess::Sampled),
                    )
                }
                Some(spirv::Op::TypeSampler) => {
                    let index = read_samplers;
                    read_samplers += 1;
                    (ResourceKind::SynthesizedReadSampler, index, None, None)
                }
                _ => continue,
            };
            let static_sampler = (kind == ResourceKind::SynthesizedReadSampler)
                .then(StaticSamplerState::synthesized_read_sampler);
            self.bindings.push(ResourceBinding {
                kind,
                metal_index,
                descriptor: Some(DescriptorLocation {
                    set: self.descriptor_layout.set,
                    binding,
                    count: 1,
                }),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: None,
                footprint: None,
                type_layout: None,
                type_name: None,
                texture_shape,
                embedded_source: None,
                access,
                static_sampler,
            });
        }
    }

    pub(crate) fn report_ray_instance_user_id_table(
        &mut self,
        binding: Option<u32>,
    ) -> Result<(), String> {
        self.bindings
            .retain(|r| r.kind != ResourceKind::RayInstanceUserIdTable);
        if let Some(binding) = binding {
            self.bindings.push(ResourceBinding {
                kind: ResourceKind::RayInstanceUserIdTable,
                metal_index: 0,
                descriptor: Some(DescriptorLocation {
                    set: self.descriptor_layout.set,
                    binding,
                    count: 1,
                }),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: Some(BufferExtent::Unbounded),
                footprint: None,
                type_layout: None,
                type_name: Some("uint[]".into()),
                texture_shape: None,
                embedded_source: None,
                access: Some(ResourceAccess::ReadOnly),
                static_sampler: None,
            });
        }
        self.validate_descriptor_abi()
    }

    pub(crate) fn reconcile_buffer_address_table(&mut self, module: &Module) {
        let mut declared = module
            .types_global_values
            .iter()
            .filter(|instruction| instruction.class.opcode == spirv::Op::Variable)
            .filter_map(|instruction| instruction.result_id)
            .filter_map(|variable| descriptor_binding_of(module, variable))
            .filter(|binding| self.descriptor_layout.synthetic.contains(*binding))
            .filter(|binding| {
                !self.bindings.iter().any(|r| {
                    r.kind == ResourceKind::RayInstanceUserIdTable
                        && r.descriptor.is_some_and(|d| d.binding == *binding)
                })
            })
            .collect::<Vec<_>>();
        declared.sort_unstable();
        declared.dedup();
        self.bindings
            .retain(|resource| resource.kind != ResourceKind::BufferAddressTable);
        for (index, binding) in declared.into_iter().enumerate() {
            self.bindings.push(ResourceBinding {
                kind: ResourceKind::BufferAddressTable,
                metal_index: index as u32,
                descriptor: Some(DescriptorLocation {
                    set: self.descriptor_layout.set,
                    binding,
                    count: 1,
                }),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: None,
                footprint: None,
                type_layout: None,
                type_name: None,
                texture_shape: None,
                embedded_source: None,
                access: Some(ResourceAccess::ReadOnly),
                static_sampler: None,
            });
        }
    }

    pub(crate) fn add_buffer_address_table(&mut self) -> Result<(), String> {
        if self
            .bindings
            .iter()
            .any(|binding| binding.kind == ResourceKind::BufferAddressTable)
        {
            return Ok(());
        }
        let occupied = self
            .bindings
            .iter()
            .filter_map(|resource| resource.descriptor.map(|location| location.binding))
            .chain(
                self.implicit_imageblock_attachments
                    .iter()
                    .map(|attachment| attachment.binding),
            )
            .chain(
                self.fragment_imageblock
                    .iter()
                    .flat_map(|imageblock| &imageblock.members)
                    .filter_map(|member| member.binding),
            )
            .collect::<std::collections::BTreeSet<_>>();
        let binding = (self.descriptor_layout.synthetic.start
            ..self.descriptor_layout.synthetic.end)
            .find(|binding| !occupied.contains(binding))
            .ok_or_else(|| {
                "descriptor binding space exhausted for buffer-address table".to_string()
            })?;
        self.bindings.push(ResourceBinding {
            kind: ResourceKind::BufferAddressTable,
            metal_index: 0,
            descriptor: Some(DescriptorLocation {
                set: self.descriptor_layout.set,
                binding,
                count: 1,
            }),
            param_index: None,
            stage_input_location: None,
            address_space: None,
            declared_size: None,
            extent: None,
            footprint: None,
            type_layout: None,
            type_name: None,
            texture_shape: None,
            embedded_source: None,
            access: Some(ResourceAccess::ReadOnly),
            static_sampler: None,
        });
        Ok(())
    }

    pub fn from_fragment(meta: &FragMeta, entry_point: Option<&str>) -> Self {
        let mut bindings = Vec::new();
        for (idx, role) in &meta.roles {
            let idx = *idx;
            let binding = match role {
                FragRole::Buffer(n) => ResourceBinding {
                    kind: ResourceKind::Buffer,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: meta.buffer_address_spaces.get(&idx).copied(),
                    declared_size: meta.buffer_type_sizes.get(&idx).copied(),
                    extent: Some(buffer_extent(
                        meta.buffer_object_sizes.get(&idx).copied(),
                        meta.buffer_type_sizes.get(&idx).copied(),
                        meta.buffer_type_names.get(&idx),
                    )),
                    footprint: None,
                    type_layout: meta.buffer_layouts.get(&idx).cloned(),
                    type_name: meta.buffer_type_names.get(&idx).cloned(),
                    texture_shape: None,
                    embedded_source: None,
                    access: buffer_access(
                        meta.buffer_accesses.get(&idx).copied(),
                        meta.buffer_address_spaces.get(&idx).copied(),
                    ),
                    static_sampler: None,
                },
                FragRole::Texture(n) => texture_binding(*n, Some(idx), &meta.texture_type_names),
                FragRole::Sampler(n) => sampler_binding(*n, Some(idx)),
                FragRole::AccelerationStructureShadow(n) => ResourceBinding {
                    kind: ResourceKind::AccelerationStructureShadow,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: None,
                    footprint: None,
                    type_layout: None,
                    type_name: None,
                    texture_shape: None,
                    embedded_source: None,
                    access: None,
                    static_sampler: None,
                },
                FragRole::VisibleFunctionTable(n) => {
                    function_table_binding(ResourceKind::VisibleFunctionTable, *n, idx)
                }
                FragRole::IntersectionFunctionTable(n) => {
                    function_table_binding(ResourceKind::IntersectionFunctionTable, *n, idx)
                }
                FragRole::ColorInput(n) => ResourceBinding {
                    kind: ResourceKind::ColorInput,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(color_input_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: None,
                    footprint: None,
                    type_layout: None,
                    type_name: meta.color_input_type_names.get(n).cloned(),
                    texture_shape: None,
                    embedded_source: None,
                    access: None,
                    static_sampler: None,
                },
                FragRole::Position
                | FragRole::PointCoord
                | FragRole::FrontFacing
                | FragRole::BarycentricCoord { .. }
                | FragRole::PrimitiveId
                | FragRole::SampleId
                | FragRole::SampleMaskIn
                | FragRole::ViewportArrayIndex
                | FragRole::RenderTargetArrayIndex
                | FragRole::AmplificationId
                | FragRole::AmplificationCount
                | FragRole::Varying(_)
                | FragRole::ImageblockData
                | FragRole::ExecutionGroup { .. }
                | FragRole::VariantAbsentTexture
                | FragRole::Other => {
                    continue;
                }
            };
            bindings.push(binding);
        }
        append_embedded_resources(
            &mut bindings,
            &meta.embedded_textures,
            &meta.embedded_samplers,
            &meta.embedded_arguments,
        );
        let varyings = meta
            .varying_types
            .keys()
            .chain(meta.varying_names.keys())
            .chain(meta.varying_user_semantics.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|loc| Varying {
                location: loc,
                type_name: meta.varying_types.get(&loc).cloned(),
                name: meta.varying_names.get(&loc).cloned(),
                user_semantic: meta.varying_user_semantics.get(&loc).cloned(),
            })
            .collect();
        let render_targets = meta
            .render_target_members
            .iter()
            .map(|(member, location)| RenderTarget {
                member_index: *member,
                location: *location,
                type_name: meta.render_target_type_names.get(member).cloned(),
            })
            .collect();
        ShaderReflection {
            reflection_version: REFLECTION_VERSION,
            descriptor_layout: DescriptorLayout::default(),
            stage: ShaderStage::Fragment,
            entry_point: entry_point.map(str::to_string),
            bindings,
            argument_buffer_fields: embedded_argument_fields(&meta.embedded_arguments),
            vertex_attributes: Vec::new(),
            varyings,
            render_targets,
            depth_members: meta.depth_members.clone(),
            depth_qualifier: meta.depth_qualifier,
            stencil_members: meta.stencil_members.clone(),
            local_size: None,
            max_work_group_size: None,
            kernel_dispatch: None,
            fragment_sample_positions: None,
            vertex_builtins: None,
            tessellation: None,
            imageblock_layouts: Vec::new(),
            implicit_imageblock_attachments: implicit_imageblock_planes(
                &meta.implicit_imageblock_attachments,
            ),
            fragment_imageblock: meta.fragment_imageblock.as_ref().map(|imageblock| {
                FragmentImageblock {
                    sample_size: imageblock.sample_size,
                    members: imageblock
                        .members
                        .iter()
                        .enumerate()
                        .map(|(index, member)| {
                            let index = index as u32;
                            let reads = imageblock.inputs.iter().any(|projection| {
                                projection
                                    .members
                                    .iter()
                                    .any(|projected| projected.master_member == index)
                            });
                            let writes = imageblock.outputs.iter().any(|projection| {
                                projection
                                    .members
                                    .iter()
                                    .any(|projected| projected.master_member == index)
                            });
                            FragmentImageblockMember {
                                offset: member.offset,
                                size: member.size,
                                type_name: member.type_name.clone(),
                                semantic: member.semantic.clone(),
                                raster_order_group: member.raster_order_group,
                                binding: (reads || writes)
                                    .then(|| fragment_imageblock_resource_binding(index))
                                    .flatten(),
                                access: match (reads, writes) {
                                    (true, true) => ResourceAccess::ReadWrite,
                                    (true, false) => ResourceAccess::ReadOnly,
                                    (false, true) => ResourceAccess::WriteOnly,
                                    (false, false) => ResourceAccess::Unused,
                                },
                            }
                        })
                        .collect(),
                    inputs: imageblock.inputs.clone(),
                    outputs: imageblock.outputs.clone(),
                }
            }),
            datalayout: None,
            runtime_sampler_specializations: Vec::new(),
            runtime_storage_image_specializations: Vec::new(),
            function_constants: Vec::new(),
        }
    }

    pub fn from_vertex(meta: &VertMeta, entry_point: Option<&str>) -> Self {
        let is_tessellation = meta.is_tessellation_evaluation();
        let vertex_builtins = VertexBuiltins {
            uses_vertex_index: meta.roles.iter().any(|(_, r)| *r == VertRole::VertexId),
            uses_instance_index: !is_tessellation
                && meta.roles.iter().any(|(_, r)| *r == VertRole::InstanceId),
            writes_position: meta.output_roles.contains(&VertOutRole::Position),
        };
        let mut bindings = Vec::new();
        for (idx, role) in &meta.roles {
            let idx = *idx;
            let binding = match role {
                VertRole::Buffer(n) => ResourceBinding {
                    kind: ResourceKind::Buffer,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: meta.buffer_address_spaces.get(&idx).copied(),
                    declared_size: meta.buffer_type_sizes.get(&idx).copied(),
                    extent: Some(buffer_extent(
                        meta.buffer_object_sizes.get(&idx).copied(),
                        meta.buffer_type_sizes.get(&idx).copied(),
                        meta.buffer_type_names.get(&idx),
                    )),
                    footprint: None,
                    type_layout: meta.buffer_layouts.get(&idx).cloned(),
                    type_name: meta.buffer_type_names.get(&idx).cloned(),
                    texture_shape: None,
                    embedded_source: None,
                    access: buffer_access(
                        meta.buffer_accesses.get(&idx).copied(),
                        meta.buffer_address_spaces.get(&idx).copied(),
                    ),
                    static_sampler: None,
                },
                VertRole::Texture(n) => texture_binding(*n, Some(idx), &meta.texture_type_names),
                VertRole::Sampler(n) => sampler_binding(*n, Some(idx)),
                VertRole::AccelerationStructureShadow(n) => ResourceBinding {
                    kind: ResourceKind::AccelerationStructureShadow,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: None,
                    footprint: None,
                    type_layout: None,
                    type_name: None,
                    texture_shape: None,
                    embedded_source: None,
                    access: None,
                    static_sampler: None,
                },
                VertRole::VisibleFunctionTable(n) => {
                    function_table_binding(ResourceKind::VisibleFunctionTable, *n, idx)
                }
                VertRole::IntersectionFunctionTable(n) => {
                    function_table_binding(ResourceKind::IntersectionFunctionTable, *n, idx)
                }
                VertRole::VertexInput(_)
                | VertRole::VertexId
                | VertRole::InstanceId
                | VertRole::BaseVertex
                | VertRole::BaseInstance
                | VertRole::PatchControlPoints
                | VertRole::PatchInput(_)
                | VertRole::PositionInPatch
                | VertRole::PatchId
                | VertRole::AmplificationId
                | VertRole::AmplificationCount
                | VertRole::ExecutionGroup { .. }
                | VertRole::VariantAbsentTexture
                | VertRole::Other => continue,
            };
            bindings.push(binding);
        }
        append_embedded_resources(
            &mut bindings,
            &meta.embedded_textures,
            &meta.embedded_samplers,
            &meta.embedded_arguments,
        );
        let vertex_attributes = meta
            .vertex_input_types
            .keys()
            .chain(meta.vertex_input_names.keys())
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|loc| VertexAttribute {
                location: loc,
                type_name: meta.vertex_input_types.get(&loc).cloned(),
                name: meta.vertex_input_names.get(&loc).cloned(),
            })
            .collect();
        let varyings = meta
            .output_roles
            .iter()
            .filter_map(|role| match role {
                VertOutRole::Varying(loc) => Some(Varying {
                    location: *loc,
                    type_name: meta.output_varying_types.get(loc).cloned(),
                    name: meta.output_varying_names.get(loc).cloned(),
                    user_semantic: meta.output_varying_user_semantics.get(loc).cloned(),
                }),
                _ => None,
            })
            .collect();
        ShaderReflection {
            reflection_version: REFLECTION_VERSION,
            descriptor_layout: DescriptorLayout::default(),
            stage: if is_tessellation {
                ShaderStage::TessellationEvaluation
            } else {
                ShaderStage::Vertex
            },
            entry_point: entry_point.map(str::to_string),
            bindings,
            argument_buffer_fields: embedded_argument_fields(&meta.embedded_arguments),
            vertex_attributes,
            varyings,
            render_targets: Vec::new(),
            depth_members: Vec::new(),
            depth_qualifier: None,
            stencil_members: Vec::new(),
            local_size: None,
            max_work_group_size: None,
            kernel_dispatch: None,
            fragment_sample_positions: None,
            vertex_builtins: Some(vertex_builtins),
            tessellation: meta.tessellation.as_ref().map(|tessellation| {
                let mut patch_input_locations = meta
                    .roles
                    .iter()
                    .filter_map(|(_, role)| match role {
                        VertRole::PatchInput(location) => Some(*location),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                patch_input_locations.sort_unstable();
                let patch_attributes = patch_input_locations
                    .iter()
                    .map(|location| TessellationAttribute {
                        location: *location,
                        type_name: meta.patch_input_types.get(location).cloned(),
                    })
                    .collect();
                TessellationInterface {
                    domain: tessellation.domain,
                    control_point_count: tessellation.control_point_count,
                    control_point_locations: tessellation
                        .control_point_fields
                        .iter()
                        .map(|field| field.location)
                        .collect(),
                    patch_input_locations,
                    control_point_attributes: tessellation
                        .control_point_fields
                        .iter()
                        .map(|field| TessellationAttribute {
                            location: field.location,
                            type_name: field.type_name.clone(),
                        })
                        .collect(),
                    patch_attributes,
                    instance_id: tessellation_system_attribute(meta, &VertRole::InstanceId),
                    amplification_id: tessellation_system_attribute(
                        meta,
                        &VertRole::AmplificationId,
                    ),
                    amplification_count: tessellation_system_attribute(
                        meta,
                        &VertRole::AmplificationCount,
                    ),
                }
            }),
            imageblock_layouts: Vec::new(),
            implicit_imageblock_attachments: implicit_imageblock_planes(
                &meta.implicit_imageblock_attachments,
            ),
            fragment_imageblock: None,
            datalayout: None,
            runtime_sampler_specializations: Vec::new(),
            runtime_storage_image_specializations: Vec::new(),
            function_constants: Vec::new(),
        }
    }

    pub fn from_kernel(meta: &KernMeta, entry_point: Option<&str>, local_size: [u32; 3]) -> Self {
        let mut bindings = Vec::new();
        let stage_input_bindings = meta.stage_input_bindings();
        for (idx, role) in &meta.roles {
            let idx = *idx;
            let binding = match role {
                KernRole::Buffer(n) => {
                    let address_space = meta.buffer_address_spaces.get(&idx).copied();
                    let threadgroup = address_space == Some(ADDRESS_SPACE_THREADGROUP);
                    let kind = if threadgroup {
                        ResourceKind::ThreadgroupBuffer
                    } else {
                        ResourceKind::Buffer
                    };
                    ResourceBinding {
                        kind,
                        metal_index: *n,
                        descriptor: if threadgroup {
                            None
                        } else {
                            ResourceBinding::descriptor_at(buffer_resource_binding(*n))
                        },
                        param_index: Some(idx),
                        stage_input_location: None,
                        address_space,
                        declared_size: meta.buffer_type_sizes.get(&idx).copied(),
                        extent: Some(buffer_extent(
                            meta.buffer_object_sizes.get(&idx).copied(),
                            meta.buffer_type_sizes.get(&idx).copied(),
                            meta.buffer_type_names.get(&idx),
                        )),
                        footprint: None,
                        type_layout: meta.buffer_layouts.get(&idx).cloned(),
                        type_name: meta.buffer_type_names.get(&idx).cloned(),
                        texture_shape: None,
                        embedded_source: None,
                        access: buffer_access(
                            meta.buffer_accesses.get(&idx).copied(),
                            address_space,
                        ),
                        static_sampler: None,
                    }
                }
                KernRole::Texture(n) => texture_binding(*n, Some(idx), &meta.texture_type_names),
                KernRole::Sampler(n) => sampler_binding(*n, Some(idx)),
                KernRole::StageInput(location) => {
                    let metal_index = stage_input_bindings
                        .get(&idx)
                        .copied()
                        .expect("stage-input binding was allocated");
                    ResourceBinding {
                        kind: ResourceKind::KernelStageInput,
                        metal_index,
                        descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(
                            metal_index,
                        )),
                        param_index: Some(idx),
                        stage_input_location: Some(*location),
                        address_space: None,
                        declared_size: None,
                        extent: Some(BufferExtent::Unbounded),
                        footprint: None,
                        type_layout: None,
                        type_name: meta.stage_input_type_names.get(&idx).cloned(),
                        texture_shape: None,
                        embedded_source: None,
                        access: Some(ResourceAccess::ReadOnly),
                        static_sampler: None,
                    }
                }
                KernRole::AccelerationStructureShadow(n) => ResourceBinding {
                    kind: ResourceKind::AccelerationStructureShadow,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: None,
                    footprint: None,
                    type_layout: None,
                    type_name: None,
                    texture_shape: None,
                    embedded_source: None,
                    access: None,
                    static_sampler: None,
                },
                KernRole::PrimitiveAccelerationStructure(n) => ResourceBinding {
                    kind: ResourceKind::PrimitiveAccelerationStructure,
                    metal_index: *n,
                    descriptor: None,
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: None,
                    footprint: None,
                    type_layout: None,
                    type_name: Some("acceleration_structure<>".into()),
                    texture_shape: None,
                    embedded_source: None,
                    access: Some(ResourceAccess::ReadOnly),
                    static_sampler: None,
                },
                KernRole::PrimitiveAccelerationStructureShadow(n) => ResourceBinding {
                    kind: ResourceKind::PrimitiveAccelerationStructure,
                    metal_index: *n,
                    descriptor: ResourceBinding::descriptor_at(buffer_resource_binding(*n)),
                    param_index: Some(idx),
                    stage_input_location: None,
                    address_space: None,
                    declared_size: None,
                    extent: Some(BufferExtent::Unbounded),
                    footprint: None,
                    type_layout: None,
                    type_name: Some("acceleration_structure<>".into()),
                    texture_shape: None,
                    embedded_source: None,
                    access: Some(ResourceAccess::ReadOnly),
                    static_sampler: None,
                },
                KernRole::VisibleFunctionTable(n) => {
                    function_table_binding(ResourceKind::VisibleFunctionTable, *n, idx)
                }
                KernRole::IntersectionFunctionTable(n) => {
                    function_table_binding(ResourceKind::IntersectionFunctionTable, *n, idx)
                }
                _ => continue,
            };
            bindings.push(binding);
        }
        append_embedded_resources(
            &mut bindings,
            &meta.embedded_textures,
            &meta.embedded_samplers,
            &meta.embedded_arguments,
        );
        ShaderReflection {
            reflection_version: REFLECTION_VERSION,
            descriptor_layout: DescriptorLayout::default(),
            stage: ShaderStage::Kernel,
            entry_point: entry_point.map(str::to_string),
            bindings,
            argument_buffer_fields: embedded_argument_fields(&meta.embedded_arguments),
            vertex_attributes: Vec::new(),
            varyings: Vec::new(),
            render_targets: Vec::new(),
            depth_members: Vec::new(),
            depth_qualifier: None,
            stencil_members: Vec::new(),
            local_size: Some(local_size),
            max_work_group_size: meta.max_work_group_size,
            kernel_dispatch: Some(KernelDispatch::safe_default()),
            fragment_sample_positions: None,
            vertex_builtins: None,
            tessellation: None,
            imageblock_layouts: {
                let mut ibs: Vec<ImageblockLayout> = meta
                    .imageblock_layouts
                    .iter()
                    .map(|(idx, ty)| ImageblockLayout {
                        param_index: *idx,
                        type_layout: ty.clone(),
                    })
                    .collect();
                ibs.sort_by_key(|ib| ib.param_index);
                ibs
            },
            implicit_imageblock_attachments: implicit_imageblock_planes(
                &meta.implicit_imageblock_attachments,
            ),
            fragment_imageblock: None,
            datalayout: None,
            runtime_sampler_specializations: Vec::new(),
            runtime_storage_image_specializations: Vec::new(),
            function_constants: Vec::new(),
        }
    }

    pub fn binding_at(&self, kind: ResourceKind, metal_index: u32) -> Option<&ResourceBinding> {
        self.bindings
            .iter()
            .find(|b| b.kind == kind && b.metal_index == metal_index)
    }

    pub(crate) fn refine_buffer_access_from_entry(&mut self, ll: &str) {
        let Some(entry) = self.entry_point.as_deref() else {
            return;
        };
        let Some((args, body)) = llvm_entry_args_and_body(ll, entry) else {
            return;
        };
        for binding in &mut self.bindings {
            if !matches!(
                binding.kind,
                ResourceKind::Buffer | ResourceKind::ThreadgroupBuffer
            ) {
                continue;
            }
            let Some(param_index) = binding
                .param_index
                .and_then(|idx| usize::try_from(idx).ok())
            else {
                continue;
            };
            let Some(arg) = args.get(param_index) else {
                continue;
            };
            let Some(name) = percent_tokens(arg).last().copied() else {
                continue;
            };
            if !ssa_token_occurs(body, name) || llvm_arg_has_attribute(arg, "readnone") {
                binding.access = Some(ResourceAccess::Unused);
            } else if llvm_arg_has_attribute(arg, "readonly") {
                binding.access = Some(ResourceAccess::ReadOnly);
            } else if llvm_arg_has_attribute(arg, "writeonly") {
                binding.access = Some(ResourceAccess::WriteOnly);
            }
        }
    }

    pub(crate) fn add_static_samplers(&mut self, ll: &str) -> Result<(), String> {
        let constants = parse_static_sampler_constants(ll)?;
        if constants.is_empty() {
            return Ok(());
        }
        let mut occupied = self
            .bindings
            .iter()
            .filter_map(|binding| binding.descriptor.map(|descriptor| descriptor.binding))
            .collect::<std::collections::BTreeSet<_>>();
        for words in constants {
            let binding = (self.descriptor_layout.samplers.start
                ..self.descriptor_layout.samplers.end)
                .find(|binding| !occupied.contains(binding))
                .ok_or_else(|| {
                    format!(
                        "AIR constexpr sampler count exceeds descriptor band \
                         [{},{})",
                        self.descriptor_layout.samplers.start, self.descriptor_layout.samplers.end
                    )
                })?;
            occupied.insert(binding);
            self.bindings.push(ResourceBinding {
                kind: ResourceKind::StaticSampler,
                metal_index: binding - self.descriptor_layout.samplers.start,
                descriptor: Some(DescriptorLocation {
                    set: self.descriptor_layout.set,
                    binding,
                    count: 1,
                }),
                param_index: None,
                stage_input_location: None,
                address_space: None,
                declared_size: None,
                extent: None,
                footprint: None,
                type_layout: None,
                type_name: None,
                texture_shape: None,
                embedded_source: None,
                access: None,
                static_sampler: Some(StaticSamplerState::from_air_words(words)?),
            });
        }
        Ok(())
    }
}

fn llvm_entry_args_and_body<'a>(ll: &'a str, entry: &str) -> Option<(Vec<&'a str>, &'a str)> {
    let plain = format!("@{entry}(");
    let quoted = format!("@\"{entry}\"(");
    let start = ll.lines().position(|line| {
        line.trim_start().starts_with("define ")
            && (line.contains(&plain) || line.contains(&quoted))
    })?;
    let byte_start = ll
        .lines()
        .take(start)
        .map(|line| line.len() + 1)
        .sum::<usize>();
    let function = &ll[byte_start..];
    let symbol = function.find(&plain).or_else(|| function.find(&quoted))?;
    let open = function[symbol..].find('(')? + symbol;
    let mut depth = 0u32;
    let mut close = None;
    for (offset, character) in function[open..].char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    close = Some(open + offset);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let header_end = function[close + 1..].find('{')? + close + 1;
    let args = split_top_level_llvm(&function[open + 1..close]);
    let tail = &function[header_end + 1..];
    let body_end = tail.find("\n}").unwrap_or(tail.len());
    Some((args, &tail[..body_end]))
}

fn split_top_level_llvm(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    for (index, character) in text.char_indices() {
        match character {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        out.push(text[start..].trim());
    }
    out
}

fn percent_tokens(text: &str) -> Vec<&str> {
    text.split('%')
        .skip(1)
        .filter_map(|tail| {
            let token = tail
                .split(|character: char| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '.')
                })
                .next()?;
            (!token.is_empty()).then_some(token)
        })
        .collect()
}

fn ssa_token_occurs(text: &str, name: &str) -> bool {
    percent_tokens(text).contains(&name)
}

fn llvm_arg_has_attribute(arg: &str, attribute: &str) -> bool {
    arg.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|token| token == attribute)
}

fn parse_static_sampler_constants(ll: &str) -> Result<Vec<[u64; 2]>, String> {
    let mut globals = crate::fxhash::HashMap::<String, [u64; 2]>::default();
    let mut nodes = crate::fxhash::HashMap::<u32, String>::default();
    let mut root = None;

    for raw in ll.lines() {
        let line = raw.trim();
        if line.starts_with('@') && line.contains(" constant ") {
            let Some((name, _)) = line.split_once(" = ") else {
                continue;
            };
            let mut values = line.split("i64 ").skip(1).filter_map(|tail| {
                tail.split(|ch: char| ch == ',' || ch == ']' || ch.is_whitespace())
                    .find(|token| !token.is_empty())
                    .and_then(|token| token.parse::<i64>().ok())
                    .map(|value| value as u64)
            });
            if let Some(first) = values.next() {
                globals.insert(name.to_string(), [first, values.next().unwrap_or(0)]);
            }
            continue;
        }
        if let Some(body) = line.strip_prefix("!air.sampler_states = !{") {
            root = Some(metadata_refs(body));
            continue;
        }
        let Some(rest) = line.strip_prefix('!') else {
            continue;
        };
        let Some((id, body)) = rest.split_once(" = !{") else {
            continue;
        };
        if let Ok(id) = id.parse::<u32>() {
            nodes.insert(id, body.trim_end_matches('}').to_string());
        }
    }

    let Some(root) = root else {
        return Ok(Vec::new());
    };
    let mut constants = Vec::with_capacity(root.len());
    for node_id in root {
        let body = nodes
            .get(&node_id)
            .ok_or_else(|| format!("AIR sampler-state metadata node !{node_id} is missing"))?;
        if !body.contains("!\"air.sampler_state\"") {
            return Err(format!(
                "AIR sampler-state root references non-sampler node !{node_id}"
            ));
        }
        let name = global_name(body)
            .ok_or_else(|| format!("AIR sampler-state node !{node_id} has no global"))?;
        let words = globals
            .get(name)
            .copied()
            .ok_or_else(|| format!("AIR sampler-state global {name} has no i64 initializer"))?;
        constants.push((name.to_string(), words));
    }
    constants.sort_by_key(|(name, _)| crate::meta::static_sampler_name_order(name));
    Ok(constants.into_iter().map(|(_, words)| words).collect())
}

fn metadata_refs(body: &str) -> Vec<u32> {
    body.split('!')
        .skip(1)
        .filter_map(|tail| {
            let digits = tail
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .collect::<String>();
            (!digits.is_empty())
                .then(|| digits.parse::<u32>().ok())
                .flatten()
        })
        .collect()
}

fn global_name(body: &str) -> Option<&str> {
    let start = body.find('@')?;
    let rest = &body[start..];
    let end = rest
        .find(|character: char| {
            character.is_whitespace() || matches!(character, ',' | ')' | ']' | '}')
        })
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn texture_binding(
    n: u32,
    param_index: Option<u32>,
    type_names: &crate::fxhash::HashMap<u32, String>,
) -> ResourceBinding {
    let type_name = param_index.and_then(|idx| type_names.get(&idx).cloned());
    let texture_shape = type_name.as_deref().map(texture_shape_from_name);
    let (kind, access) = classify_texture(texture_shape.as_ref());
    let descriptor_binding = if access == ResourceAccess::Storage {
        storage_texture_resource_binding(n)
    } else {
        texture_resource_binding(n)
    };
    let mut descriptor = ResourceBinding::descriptor_at(descriptor_binding);
    if let (Some(location), Some(shape)) = (descriptor.as_mut(), texture_shape.as_ref()) {
        location.count = shape.descriptor_count();
    }
    ResourceBinding {
        kind,
        metal_index: n,
        descriptor,
        param_index,
        stage_input_location: None,
        address_space: None,
        declared_size: None,
        extent: None,
        footprint: None,
        type_layout: None,
        type_name,
        texture_shape,
        embedded_source: None,
        access: Some(access),
        static_sampler: None,
    }
}

fn classify_texture(shape: Option<&TextureShape>) -> (ResourceKind, ResourceAccess) {
    let Some(shape) = shape else {
        return (ResourceKind::Texture, ResourceAccess::Sampled);
    };
    if shape.array_ref {
        (
            ResourceKind::TextureArray,
            if shape.writable {
                ResourceAccess::Storage
            } else {
                ResourceAccess::Sampled
            },
        )
    } else if shape.writable {
        (ResourceKind::StorageImage, ResourceAccess::Storage)
    } else {
        (ResourceKind::Texture, ResourceAccess::Sampled)
    }
}

fn buffer_extent(
    object_size: Option<u32>,
    declared_size: Option<u32>,
    type_name: Option<&String>,
) -> BufferExtent {
    match (object_size, declared_size, type_name) {
        (Some(bytes), _, _) => BufferExtent::Object { bytes },
        (None, Some(_), _) | (None, None, Some(_)) => BufferExtent::Unbounded,
        (None, None, None) => BufferExtent::Unknown,
    }
}

fn buffer_access(
    declared: Option<BufferAccess>,
    address_space: Option<u32>,
) -> Option<ResourceAccess> {
    match declared {
        Some(BufferAccess::ReadOnly) => Some(ResourceAccess::ReadOnly),
        Some(BufferAccess::WriteOnly) => Some(ResourceAccess::WriteOnly),
        Some(BufferAccess::ReadWrite) => Some(ResourceAccess::ReadWrite),
        None if address_space == Some(ADDRESS_SPACE_CONSTANT) => Some(ResourceAccess::ReadOnly),
        None => None,
    }
}

fn append_embedded_resources(
    bindings: &mut Vec<ResourceBinding>,
    textures: &[crate::meta::EmbeddedTexture],
    samplers: &[crate::meta::EmbeddedSampler],
    arguments: &[crate::meta::EmbeddedArgument],
) {
    for embedded in textures {
        let descriptor_binding = if embedded.storage_format.is_some() {
            storage_texture_resource_binding(embedded.synthetic_texture_index)
                .unwrap_or(STORAGE_TEXTURE_BINDING_RANGE.end)
        } else {
            texture_resource_binding(embedded.synthetic_texture_index)
                .unwrap_or(TEXTURE_BINDING_RANGE.end)
        };
        bindings.push(ResourceBinding {
            kind: ResourceKind::EmbeddedArgBufferTexture,
            metal_index: embedded.synthetic_texture_index,
            descriptor: Some(DescriptorLocation {
                set: RESOURCE_DESCRIPTOR_SET,
                binding: descriptor_binding,
                count: embedded.array_length.unwrap_or(1),
            }),
            param_index: None,
            stage_input_location: None,
            address_space: None,
            declared_size: None,
            extent: None,
            footprint: None,
            type_layout: None,
            type_name: None,
            texture_shape: Some(TextureShape {
                dimension: TextureDimension::from_spirv_dim(embedded.dim),
                arrayed: embedded.arrayed,
                multisampled: false,
                component: TextureComponent::from_image_comp(embedded.comp),
                writable: embedded.storage_format.is_some(),
                array_ref: embedded.array_length.is_some(),
                array_length: embedded.array_length,
                storage_format: embedded.storage_format,
            }),
            embedded_source: Some(EmbeddedArgBuffer {
                buffer_param_index: embedded.buffer_param_index,
                buffer_index: embedded.buffer_index,
                field_offset: embedded.field_offset,
                field_ordinal: embedded.field_ordinal,
                argument_index: embedded.argument_index,
                resource_buffer_index: None,
            }),
            access: Some(if embedded.storage_format.is_some() {
                ResourceAccess::Storage
            } else {
                ResourceAccess::Sampled
            }),
            static_sampler: None,
        });
    }
    for embedded in samplers {
        bindings.push(ResourceBinding {
            kind: ResourceKind::EmbeddedArgBufferSampler,
            metal_index: embedded.synthetic_sampler_index,
            descriptor: Some(DescriptorLocation {
                set: RESOURCE_DESCRIPTOR_SET,
                binding: DEFAULT_DESCRIPTOR_LAYOUT
                    .samplers
                    .binding(embedded.synthetic_sampler_index)
                    .unwrap_or(SAMPLER_BINDING_RANGE.end),
                count: 1,
            }),
            param_index: None,
            stage_input_location: None,
            address_space: None,
            declared_size: None,
            extent: None,
            footprint: None,
            type_layout: None,
            type_name: None,
            texture_shape: None,
            embedded_source: Some(EmbeddedArgBuffer {
                buffer_param_index: embedded.buffer_param_index,
                buffer_index: embedded.buffer_index,
                field_offset: embedded.field_offset,
                field_ordinal: embedded.field_ordinal,
                argument_index: embedded.argument_index,
                resource_buffer_index: None,
            }),
            access: None,
            static_sampler: None,
        });
    }
    for argument in arguments {
        let Some(resource_index) = argument.resource_buffer_index else {
            continue;
        };
        bindings.push(ResourceBinding {
            kind: ResourceKind::EmbeddedArgBufferBuffer,
            metal_index: resource_index,
            descriptor: None,
            param_index: None,
            stage_input_location: None,
            address_space: argument.resource_address_space,
            declared_size: argument.resource_declared_size,
            extent: Some(buffer_extent(None, argument.resource_declared_size, None)),
            footprint: None,
            type_layout: None,
            type_name: None,
            texture_shape: None,
            embedded_source: Some(EmbeddedArgBuffer {
                buffer_param_index: argument.buffer_param_index,
                buffer_index: argument.buffer_index,
                field_offset: argument.field_offset,
                field_ordinal: argument.field_ordinal,
                argument_index: argument.argument_index,
                resource_buffer_index: Some(resource_index),
            }),
            access: buffer_access(argument.resource_access, argument.resource_address_space),
            static_sampler: None,
        });
    }
}

fn embedded_argument_fields(arguments: &[crate::meta::EmbeddedArgument]) -> Vec<EmbeddedArgBuffer> {
    arguments
        .iter()
        .map(|argument| EmbeddedArgBuffer {
            buffer_param_index: argument.buffer_param_index,
            buffer_index: argument.buffer_index,
            field_offset: argument.field_offset,
            field_ordinal: argument.field_ordinal,
            argument_index: argument.argument_index,
            resource_buffer_index: argument.resource_buffer_index,
        })
        .collect()
}

fn sampler_binding(n: u32, param_index: Option<u32>) -> ResourceBinding {
    ResourceBinding {
        kind: ResourceKind::Sampler,
        metal_index: n,
        descriptor: ResourceBinding::descriptor_at(sampler_resource_binding(n)),
        param_index,
        stage_input_location: None,
        address_space: None,
        declared_size: None,
        extent: None,
        footprint: None,
        type_layout: None,
        type_name: None,
        texture_shape: None,
        embedded_source: None,
        access: None,
        static_sampler: None,
    }
}

fn function_table_binding(
    kind: ResourceKind,
    metal_index: u32,
    param_index: u32,
) -> ResourceBinding {
    let type_name = match kind {
        ResourceKind::VisibleFunctionTable => "visible_function_table",
        ResourceKind::IntersectionFunctionTable => "intersection_function_table",
        _ => unreachable!("function-table helper requires a function-table kind"),
    };
    ResourceBinding {
        kind,
        metal_index,
        descriptor: None,
        param_index: Some(param_index),
        stage_input_location: None,
        address_space: None,
        declared_size: None,
        extent: None,
        footprint: None,
        type_layout: None,
        type_name: Some(type_name.into()),
        texture_shape: None,
        embedded_source: None,
        access: Some(ResourceAccess::ReadOnly),
        static_sampler: None,
    }
}

#[cfg(test)]
mod tests;
