use crate::fxhash::HashMap;
use crate::passes::Stage;

mod embedded;
mod function_constants;
mod globals;
mod intersections;
mod samplers;
mod textures;
mod types;
use embedded::{
    body_uses_texture_intrinsic, detect_embedded_arguments, detect_embedded_samplers,
    detect_embedded_textures, unsurfaced_embedded_resources, ArgumentBuffers,
};
pub use embedded::{
    embedded_synthetic_sampler_index, embedded_synthetic_texture_index, EmbeddedArgument,
    EmbeddedSampler, EmbeddedTexture,
};
pub(crate) use function_constants::function_constants_without_a_supplied_value;
pub use function_constants::{parse_function_constants, FunctionConstant};
use globals::{location_index_with_static, static_init_int_global_values};
pub(crate) use globals::{static_init_foldable_global_values, StaticIntValue};
pub use intersections::{
    AirIntersectionFamily, AirIntersectionInstancing, AirIntersectionResultField,
};
pub use samplers::{is_static_sampler_global, static_sampler_name_order};
pub use textures::{
    texture_shape_from_name, TextureComponent, TextureDimension, TextureFormat, TextureShape,
    TEXTURE_HANDLE_ARRAY_DESCRIPTOR_COUNT,
};
pub(crate) use types::storage_air_type_for_size;
use types::{parse_struct_info, struct_info_ref, tokenize, Tok};
pub use types::{primitive_air_type_from_name, AirMember, AirScalar, AirType};

pub fn is_device_buffer_array_type_name(name: &str) -> bool {
    name.chars()
        .filter(|character| !character.is_whitespace())
        .eq("array_ref<void>".chars())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FragRole {
    Position,
    PointCoord,
    FrontFacing,
    BarycentricCoord { no_perspective: bool },
    PrimitiveId,
    SampleId,
    SampleMaskIn,
    ViewportArrayIndex,
    RenderTargetArrayIndex,
    AmplificationId,
    AmplificationCount,
    Varying(u32),
    Texture(u32),
    Sampler(u32),
    VisibleFunctionTable(u32),
    IntersectionFunctionTable(u32),
    Buffer(u32),
    AccelerationStructureShadow(u32),
    ColorInput(u32),
    ImageblockData,
    ExecutionGroup {
        fact: ExecutionGroupFact,
        lanes: u32,
    },
    VariantAbsentTexture,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferAccess {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VaryingSampling {
    #[default]
    Center,
    Centroid,
    Sample,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VaryingInterpolation {
    pub flat: bool,
    pub no_perspective: bool,
    pub sampling: VaryingSampling,
}

impl VaryingInterpolation {
    fn from_role_strings(strs: &[String]) -> Self {
        let has = |marker: &str| strs.iter().any(|s| s == marker);
        Self {
            flat: has("flat"),
            no_perspective: has("no_perspective"),
            sampling: if has("centroid") {
                VaryingSampling::Centroid
            } else if has("sample") {
                VaryingSampling::Sample
            } else {
                VaryingSampling::Center
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FragMeta {
    pub input_type_names: HashMap<u32, String>,
    pub roles: Vec<(u32, FragRole)>,
    pub unmodelled_input_params: Vec<(u32, String)>,
    pub unmodelled_stage_attributes: Vec<String>,
    pub early_fragment_tests: bool,
    pub implicit_imageblock_attachments: Vec<ImplicitImageblockAttachment>,
    pub varying_types: HashMap<u32, String>,
    pub varying_names: HashMap<u32, String>,
    pub varying_user_semantics: HashMap<u32, String>,
    pub varying_interpolation: HashMap<u32, VaryingInterpolation>,
    pub n_render_targets: u32,
    pub render_target_members: Vec<(u32, u32)>,
    pub render_target_type_names: HashMap<u32, String>,
    pub render_target_dual_members: Vec<u32>,
    pub depth_members: Vec<u32>,
    pub depth_qualifier: Option<DepthQualifier>,
    pub stencil_members: Vec<u32>,
    pub sample_mask_members: Vec<u32>,
    pub unmodelled_output_members: Vec<(u32, String)>,
    pub fragment_imageblock: Option<FragmentImageblock>,
    pub render_target_indices: Vec<u32>,
    pub buffer_layouts: HashMap<u32, AirType>,
    pub buffer_address_spaces: HashMap<u32, u32>,
    pub buffer_type_sizes: HashMap<u32, u32>,
    pub buffer_object_sizes: HashMap<u32, u32>,
    pub buffer_type_names: HashMap<u32, String>,
    pub buffer_accesses: HashMap<u32, BufferAccess>,
    pub texture_type_names: HashMap<u32, String>,
    pub declared_descriptor_counts: HashMap<u32, u32>,
    pub color_input_type_names: HashMap<u32, String>,
    pub embedded_textures: Vec<EmbeddedTexture>,
    pub embedded_samplers: Vec<EmbeddedSampler>,
    pub embedded_arguments: Vec<EmbeddedArgument>,
    pub unsurfaced_embedded_resources: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblockMember {
    pub offset: u32,
    pub size: u32,
    pub type_name: String,
    pub semantic: String,
    pub raster_order_group: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblockProjectionMember {
    pub projection_member: u32,
    pub master_member: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblockProjection {
    pub interface_index: u32,
    pub members: Vec<FragmentImageblockProjectionMember>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FragmentImageblock {
    pub sample_size: u32,
    pub members: Vec<FragmentImageblockMember>,
    pub inputs: Vec<FragmentImageblockProjection>,
    pub outputs: Vec<FragmentImageblockProjection>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DepthQualifier {
    Any,
    Less,
    Greater,
}

impl FragMeta {
    pub fn role_of(&self, idx: u32) -> Option<&FragRole> {
        self.roles.iter().find(|(i, _)| *i == idx).map(|(_, r)| r)
    }
    pub fn layout_of(&self, idx: u32) -> Option<&AirType> {
        self.buffer_layouts.get(&idx)
    }
    pub fn texture_type_name(&self, idx: u32) -> Option<&str> {
        self.texture_type_names.get(&idx).map(String::as_str)
    }
    pub fn declared_descriptor_count(&self, idx: u32) -> Option<u32> {
        self.declared_descriptor_counts.get(&idx).copied()
    }
    pub fn color_input_type_name(&self, location: u32) -> Option<&str> {
        self.color_input_type_names
            .get(&location)
            .map(String::as_str)
    }
    pub fn varying_type(&self, loc: u32) -> Option<&str> {
        self.varying_types.get(&loc).map(String::as_str)
    }
    pub fn varying_name(&self, loc: u32) -> Option<&str> {
        self.varying_names.get(&loc).map(String::as_str)
    }
    pub fn varying_user_semantic(&self, loc: u32) -> Option<&str> {
        self.varying_user_semantics.get(&loc).map(String::as_str)
    }
    pub fn varying_interpolation(&self, loc: u32) -> VaryingInterpolation {
        self.varying_interpolation
            .get(&loc)
            .copied()
            .unwrap_or_default()
    }
    pub fn varying_is_flat(&self, loc: u32) -> bool {
        self.varying_interpolation(loc).flat
    }
    pub fn render_target_location_for_member(&self, member_idx: u32) -> Option<u32> {
        self.render_target_members
            .iter()
            .find_map(|(member, location)| (*member == member_idx).then_some(*location))
    }
    pub fn is_dual_source_member(&self, member_idx: u32) -> bool {
        self.render_target_dual_members.contains(&member_idx)
    }
    pub fn render_target_type_name(&self, member_idx: u32) -> Option<&str> {
        self.render_target_type_names
            .get(&member_idx)
            .map(String::as_str)
    }
    pub fn is_depth_member(&self, member_idx: u32) -> bool {
        self.depth_members.contains(&member_idx)
    }
    pub fn is_stencil_member(&self, member_idx: u32) -> bool {
        self.stencil_members.contains(&member_idx)
    }
    pub fn is_sample_mask_member(&self, member_idx: u32) -> bool {
        self.sample_mask_members.contains(&member_idx)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VertRole {
    VertexInput(u32),
    Buffer(u32),
    AccelerationStructureShadow(u32),
    Texture(u32),
    Sampler(u32),
    VisibleFunctionTable(u32),
    IntersectionFunctionTable(u32),
    VertexId,
    InstanceId,
    // [[base_vertex]] / [[base_instance]]: Vulkan BaseVertex / BaseInstance (DrawParameters)
    BaseVertex,
    BaseInstance,
    PatchControlPoints,
    PatchInput(u32),
    PositionInPatch,
    PatchId,
    AmplificationId,
    AmplificationCount,
    ExecutionGroup {
        fact: ExecutionGroupFact,
        lanes: u32,
    },
    VariantAbsentTexture,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum PatchDomain {
    Triangle,
    Quad,
    Isoline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchControlPointField {
    pub location: u32,
    pub type_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TessellationMeta {
    pub domain: PatchDomain,
    pub control_point_count: u32,
    pub control_point_function: Option<String>,
    pub control_point_fields: Vec<PatchControlPointField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VertOutRole {
    Position,
    PointSize,
    ClipDistance,
    ViewportArrayIndex,
    RenderTargetArrayIndex,
    Varying(u32),
    FunctionConstantDisabled,
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct VertMeta {
    pub roles: Vec<(u32, VertRole)>,
    pub unmodelled_input_params: Vec<(u32, String)>,
    pub implicit_imageblock_attachments: Vec<ImplicitImageblockAttachment>,
    pub parameter_type_names: HashMap<u32, String>,
    pub output_roles: Vec<VertOutRole>,
    pub unmodelled_output_members: Vec<(u32, String)>,
    pub invariant_outputs: Vec<u32>,
    pub output_varying_types: HashMap<u32, String>,
    pub output_varying_names: HashMap<u32, String>,
    pub output_varying_user_semantics: HashMap<u32, String>,
    pub vertex_input_types: HashMap<u32, String>,
    pub vertex_input_names: HashMap<u32, String>,
    pub patch_input_types: HashMap<u32, String>,
    pub patch_input_names: HashMap<u32, String>,
    pub buffer_layouts: HashMap<u32, AirType>,
    pub buffer_address_spaces: HashMap<u32, u32>,
    pub buffer_type_sizes: HashMap<u32, u32>,
    pub buffer_object_sizes: HashMap<u32, u32>,
    pub buffer_type_names: HashMap<u32, String>,
    pub buffer_accesses: HashMap<u32, BufferAccess>,
    pub texture_type_names: HashMap<u32, String>,
    pub declared_descriptor_counts: HashMap<u32, u32>,
    pub embedded_textures: Vec<EmbeddedTexture>,
    pub embedded_samplers: Vec<EmbeddedSampler>,
    pub embedded_arguments: Vec<EmbeddedArgument>,
    pub unsurfaced_embedded_resources: Vec<String>,
    pub tessellation: Option<TessellationMeta>,
    pub undecoded_patch_shape: Option<String>,
    pub unmodelled_stage_attributes: Vec<String>,
}

impl VertMeta {
    pub fn is_tessellation_evaluation(&self) -> bool {
        self.tessellation.is_some()
    }

    pub fn tessellation_system_input_location(&self, role: &VertRole) -> Option<u32> {
        let base = self
            .roles
            .iter()
            .filter_map(|(_, role)| match role {
                VertRole::PatchInput(location) => Some(*location),
                _ => None,
            })
            .chain(
                self.tessellation
                    .iter()
                    .flat_map(|meta| meta.control_point_fields.iter().map(|field| field.location)),
            )
            .max()
            .map_or(0, |location| location + 1);
        match role {
            VertRole::InstanceId => Some(base),
            VertRole::AmplificationId => Some(base + 1),
            VertRole::AmplificationCount => Some(base + 2),
            _ => None,
        }
    }
}

impl VertMeta {
    pub fn role_of(&self, idx: u32) -> Option<&VertRole> {
        self.roles.iter().find(|(i, _)| *i == idx).map(|(_, r)| r)
    }
    pub fn layout_of(&self, idx: u32) -> Option<&AirType> {
        self.buffer_layouts.get(&idx)
    }
    pub fn texture_type_name(&self, idx: u32) -> Option<&str> {
        self.texture_type_names.get(&idx).map(String::as_str)
    }
    pub fn declared_descriptor_count(&self, idx: u32) -> Option<u32> {
        self.declared_descriptor_counts.get(&idx).copied()
    }
    pub fn output_role_of(&self, idx: u32) -> Option<&VertOutRole> {
        self.output_roles.get(idx as usize)
    }
    pub fn output_is_invariant(&self, idx: u32) -> bool {
        self.invariant_outputs.contains(&idx)
    }
    pub fn output_varying_type(&self, loc: u32) -> Option<&str> {
        self.output_varying_types.get(&loc).map(String::as_str)
    }
    pub fn output_varying_name(&self, loc: u32) -> Option<&str> {
        self.output_varying_names.get(&loc).map(String::as_str)
    }
    pub fn output_varying_user_semantic(&self, loc: u32) -> Option<&str> {
        self.output_varying_user_semantics
            .get(&loc)
            .map(String::as_str)
    }
    pub fn vertex_input_type(&self, loc: u32) -> Option<&str> {
        self.vertex_input_types.get(&loc).map(String::as_str)
    }
    pub fn vertex_input_name(&self, loc: u32) -> Option<&str> {
        self.vertex_input_names.get(&loc).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KernRole {
    Buffer(u32),
    Texture(u32),
    Sampler(u32),
    AccelerationStructureShadow(u32),
    PrimitiveAccelerationStructure(u32),
    PrimitiveAccelerationStructureShadow(u32),
    VisibleFunctionTable(u32),
    IntersectionFunctionTable(u32),
    ThreadsPerThreadgroup,
    DispatchThreadsPerThreadgroup,
    ThreadPositionInThreadgroup,
    ThreadgroupsPerGrid,
    ThreadsPerGrid,
    ThreadgroupPositionInGrid,
    ThreadIndexInThreadgroup,
    ExecutionGroup {
        fact: ExecutionGroupFact,
        lanes: u32,
    },
    ThreadPositionInGrid,
    StageInput(u32),
    VariantAbsentTexture,
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct KernMeta {
    pub roles: Vec<(u32, KernRole)>,
    pub unmodelled_input_params: Vec<(u32, String)>,
    pub aliased_implicit_imageblock_params: Vec<u32>,
    pub aliased_implicit_imageblock_planes: HashMap<u32, Vec<AliasedImageblockPlane>>,
    pub unmodelled_stage_attributes: Vec<String>,
    pub max_work_group_size: Option<u32>,
    pub function_constant_buffer_locations: HashMap<u32, u32>,
    pub buffer_layouts: HashMap<u32, AirType>,
    pub imageblock_layouts: HashMap<u32, AirType>,
    pub implicit_imageblock_attachments: Vec<ImplicitImageblockAttachment>,
    pub buffer_address_spaces: HashMap<u32, u32>,
    pub buffer_type_sizes: HashMap<u32, u32>,
    pub buffer_object_sizes: HashMap<u32, u32>,
    pub buffer_type_names: HashMap<u32, String>,
    pub buffer_accesses: HashMap<u32, BufferAccess>,
    pub texture_type_names: HashMap<u32, String>,
    pub declared_descriptor_counts: HashMap<u32, u32>,
    pub stage_input_type_names: HashMap<u32, String>,
    pub embedded_textures: Vec<EmbeddedTexture>,
    pub embedded_samplers: Vec<EmbeddedSampler>,
    pub embedded_arguments: Vec<EmbeddedArgument>,
    pub unsurfaced_embedded_resources: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImplicitImageblockAttachment {
    pub attachment: u32,
    pub data_rate: u32,
    pub max_index: Option<u32>,
    pub format: TextureFormat,
    pub reads: bool,
    pub writes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AliasedImageblockPlane {
    pub attachment: u32,
    pub offset: u32,
    pub format: TextureFormat,
    pub intrinsic_suffix: &'static str,
}

pub fn aliased_imageblock_planes(
    layout: &AirType,
) -> Result<Option<Vec<AliasedImageblockPlane>>, String> {
    let AirType::Struct(members) = layout else {
        return Ok(None);
    };
    if members.is_empty() {
        return Ok(None);
    }
    let mut planes = Vec::with_capacity(members.len());
    for (index, member) in members.iter().enumerate() {
        let (format, intrinsic_suffix) = match &member.ty {
            AirType::Scalar(AirScalar::Half) => (TextureFormat::R16f, "f16"),
            AirType::Vec {
                scalar: AirScalar::Half,
                lanes: 2,
            } => (TextureFormat::Rg16f, "v2f16"),
            AirType::Vec {
                scalar: AirScalar::Half,
                lanes: 4,
            } => (TextureFormat::Rgba16f, "v4f16"),
            AirType::Scalar(AirScalar::Float) => (TextureFormat::R32f, "f32"),
            AirType::Vec {
                scalar: AirScalar::Float,
                lanes: 4,
            } => (TextureFormat::Rgba32f, "v4f32"),
            AirType::Scalar(AirScalar::UInt) => (TextureFormat::R32ui, "i32"),
            other => {
                return Err(format!(
                    "aliased imageblock member {index} has type {other:?}, which no implicit \
                     imageblock plane format represents"
                ))
            }
        };
        planes.push(AliasedImageblockPlane {
            attachment: index as u32,
            offset: member.offset,
            format,
            intrinsic_suffix,
        });
    }
    Ok(Some(planes))
}

impl KernRole {
    pub fn buffer_table_slot(&self) -> Option<u32> {
        match self {
            Self::Buffer(slot)
            | Self::AccelerationStructureShadow(slot)
            | Self::PrimitiveAccelerationStructure(slot)
            | Self::PrimitiveAccelerationStructureShadow(slot)
            | Self::VisibleFunctionTable(slot)
            | Self::IntersectionFunctionTable(slot) => Some(*slot),
            Self::Texture(_)
            | Self::Sampler(_)
            | Self::StageInput(_)
            | Self::ThreadsPerThreadgroup
            | Self::DispatchThreadsPerThreadgroup
            | Self::ThreadPositionInThreadgroup
            | Self::ThreadgroupsPerGrid
            | Self::ThreadsPerGrid
            | Self::ThreadgroupPositionInGrid
            | Self::ThreadIndexInThreadgroup
            | Self::ExecutionGroup { .. }
            | Self::ThreadPositionInGrid
            | Self::VariantAbsentTexture
            | Self::Other => None,
        }
    }
}

impl KernMeta {
    pub fn role_of(&self, idx: u32) -> Option<&KernRole> {
        self.roles.iter().find(|(i, _)| *i == idx).map(|(_, r)| r)
    }
    pub fn layout_of(&self, idx: u32) -> Option<&AirType> {
        self.buffer_layouts.get(&idx)
    }
    pub fn imageblock_layout_of(&self, idx: u32) -> Option<&AirType> {
        self.imageblock_layouts.get(&idx)
    }
    pub fn buffer_address_space(&self, idx: u32) -> Option<u32> {
        self.buffer_address_spaces.get(&idx).copied()
    }
    pub fn buffer_type_size(&self, idx: u32) -> Option<u32> {
        self.buffer_type_sizes.get(&idx).copied()
    }
    pub fn buffer_type_name(&self, idx: u32) -> Option<&str> {
        self.buffer_type_names.get(&idx).map(String::as_str)
    }
    pub fn texture_type_name(&self, idx: u32) -> Option<&str> {
        self.texture_type_names.get(&idx).map(String::as_str)
    }
    pub fn declared_descriptor_count(&self, idx: u32) -> Option<u32> {
        self.declared_descriptor_counts.get(&idx).copied()
    }

    pub fn stage_input_bindings(&self) -> HashMap<u32, u32> {
        let mut occupied = self
            .roles
            .iter()
            .filter_map(|(_, role)| role.buffer_table_slot())
            .collect::<crate::fxhash::HashSet<_>>();
        let mut stage_inputs = self
            .roles
            .iter()
            .filter(|(_, role)| matches!(role, KernRole::StageInput(_)))
            .map(|(param_index, _)| *param_index)
            .collect::<Vec<_>>();
        stage_inputs.sort_unstable();
        let mut next = 0u32;
        let mut bindings = HashMap::default();
        for param_index in stage_inputs {
            while occupied.contains(&next) {
                next = next.saturating_add(1);
            }
            occupied.insert(next);
            bindings.insert(param_index, next);
        }
        bindings
    }
}

pub fn parse_air_kernel_meta(ll: &str) -> Option<KernMeta> {
    parse_air_kernel_meta_with(ll, false)
}

pub fn parse_air_kernel_meta_with(ll: &str, promote_fc_buffers: bool) -> Option<KernMeta> {
    let nodes = collect_nodes(ll);
    let entry = entry_name_from_nodes(ll, "kernel", &nodes);
    parse_air_kernel_meta_with_nodes(ll, promote_fc_buffers, &nodes, entry.as_deref())
}

pub(crate) fn parse_air_kernel_meta_variants(
    ll: &str,
) -> (Option<KernMeta>, Option<KernMeta>, Option<String>) {
    let nodes = collect_nodes(ll);
    let entry = entry_name_from_nodes(ll, "kernel", &nodes);
    let default = parse_air_kernel_meta_with_nodes(ll, false, &nodes, entry.as_deref());
    let promoted = parse_air_kernel_meta_with_nodes(ll, true, &nodes, entry.as_deref());
    (default, promoted, entry)
}

fn parse_air_kernel_meta_with_nodes(
    ll: &str,
    promote_fc_buffers: bool,
    nodes: &HashMap<u32, String>,
    entry: Option<&str>,
) -> Option<KernMeta> {
    let root = stage_root(ll, "kernel")?;
    let rootc = nodes.get(&root)?;
    let static_int_globals = static_init_int_global_values(ll);
    let resource_location =
        |node: &str, fallback: u32| location_index_with_static(node, fallback, &static_int_globals);
    let param_address_spaces = entry
        .and_then(|name| function_param_pointer_address_spaces(ll, name))
        .unwrap_or_default();
    let refs = refs_in(rootc);
    let in_ref = *refs.get(1)?;
    let mut max_work_group_size = None;
    let mut unmodelled_stage_attributes = vec![];
    for attribute in stage_root_attributes(rootc, nodes) {
        match attribute {
            StageAttribute::MaxWorkGroupSize(size) => max_work_group_size = Some(size),
            other => unmodelled_stage_attributes.push(other.describe()),
        }
    }

    let mut roles = vec![];
    let mut unmodelled_input_params: Vec<(u32, String)> = vec![];
    let mut aliased_implicit_imageblock_params: Vec<u32> = vec![];
    let mut function_constant_buffer_locations = HashMap::default();
    let mut buffer_layouts = HashMap::default();
    let mut imageblock_layouts = HashMap::default();
    let mut buffer_address_spaces = HashMap::default();
    let mut buffer_type_sizes = HashMap::default();
    let mut buffer_object_sizes = HashMap::default();
    let mut buffer_type_names = HashMap::default();
    let mut buffer_accesses = HashMap::default();
    let mut texture_type_names = HashMap::default();
    let mut declared_descriptor_counts = HashMap::default();
    let mut stage_input_type_names = HashMap::default();
    let mut top_level_texture_locations: Vec<u32> = vec![];
    let mut indirect_buffer_struct_refs: Vec<(u32, u32, u32)> = vec![];
    for r in refs_in(nodes.get(&in_ref)?) {
        let Some(node) = nodes.get(&r) else { continue };
        let Some(idx) = first_i32(node) else { continue };
        let layout = struct_info_ref(node).and_then(|sref| parse_struct_info(nodes, sref, 0));
        let strs = role_strings(node);
        if let Some(count) = declared_descriptor_count(node) {
            declared_descriptor_counts.insert(idx, count);
        }
        if strs.first().map(String::as_str) == Some("function_constant")
            && primary_role(&strs) == Some("buffer")
        {
            function_constant_buffer_locations.insert(idx, resource_location(node, idx));
        }
        let Some(mut first) = fc_promoted_role(&strs, promote_fc_buffers) else {
            continue;
        };
        let texture_slot = variant_texture_slot(node, idx, nodes, &static_int_globals);
        if primary_role(&strs) == Some("texture") && texture_slot.is_none() {
            first = VARIANT_ABSENT_TEXTURE_ROLE;
        }
        let first = present_gated_buffer_role(first, &strs, node, nodes, &static_int_globals);
        let first = present_system_value_role(first, node, nodes, &static_int_globals);
        if let Some(declared) = unmodelled_declared_role(
            |role| air_input_role_is_modelled(Stage::Kernel, role),
            node,
            nodes,
            &static_int_globals,
        ) {
            unmodelled_input_params.push((idx, declared));
        }
        let role = match first {
            "buffer" | "indirect_buffer" => {
                if first == "indirect_buffer" {
                    if let Some(sref) = struct_info_ref(node) {
                        indirect_buffer_struct_refs.push((idx, resource_location(node, idx), sref));
                    }
                }
                if let Some(t) = layout.clone() {
                    buffer_layouts.insert(idx, t);
                }
                buffer_address_spaces.insert(
                    idx,
                    address_space(node)
                        .or_else(|| param_address_spaces.get(&idx).copied())
                        .unwrap_or(1),
                );
                if let Some(name) = arg_type_name(node) {
                    buffer_type_names.insert(idx, name);
                }
                if let Some(size) = i32_after_marker(node, "air.arg_type_size")
                    .or_else(|| i32_after_marker(node, "air.buffer_size"))
                {
                    buffer_type_sizes.insert(idx, size);
                }
                if let Some(size) = i32_after_marker(node, "air.buffer_size") {
                    buffer_object_sizes.insert(idx, size);
                }
                if let Some(access) = declared_buffer_access(node) {
                    buffer_accesses.insert(idx, access);
                }
                KernRole::Buffer(location_index_with_static(node, idx, &static_int_globals))
            }
            "texture" => {
                if let Some(name) = arg_type_name(node) {
                    texture_type_names.insert(idx, name);
                }
                let loc = texture_slot.unwrap_or_else(|| resource_location(node, idx));
                top_level_texture_locations.push(loc);
                KernRole::Texture(loc)
            }
            "instance_acceleration_structure" | "primitive_acceleration_structure"
                if ll.contains("_intersection_query.") || ll.contains("@air.intersect.") =>
            {
                KernRole::AccelerationStructureShadow(resource_location(node, idx))
            }
            "instance_acceleration_structure" if body_uses_acceleration_structure_shadow(ll) => {
                KernRole::AccelerationStructureShadow(resource_location(node, idx))
            }
            "primitive_acceleration_structure" => {
                let binding = resource_location(node, idx);
                if ll.contains("@air.intersect.") {
                    KernRole::PrimitiveAccelerationStructureShadow(binding)
                } else {
                    KernRole::PrimitiveAccelerationStructure(binding)
                }
            }
            "visible_function_table" => {
                KernRole::VisibleFunctionTable(resource_location(node, idx))
            }
            "intersection_function_table" => {
                KernRole::IntersectionFunctionTable(resource_location(node, idx))
            }
            "imageblock" => {
                if let Some(t) = layout {
                    imageblock_layouts.insert(idx, t);
                }
                if strs.iter().any(|s| s == "alias_implicit_imageblock") {
                    aliased_implicit_imageblock_params.push(idx);
                }
                KernRole::Other
            }
            "sampler" => KernRole::Sampler(resource_location(node, idx)),
            "threads_per_threadgroup" => KernRole::ThreadsPerThreadgroup,
            "dispatch_threads_per_threadgroup" => KernRole::DispatchThreadsPerThreadgroup,
            "thread_position_in_threadgroup" => KernRole::ThreadPositionInThreadgroup,
            "threadgroups_per_grid" => KernRole::ThreadgroupsPerGrid,
            "threads_per_grid" => KernRole::ThreadsPerGrid,
            "threadgroup_position_in_grid" => KernRole::ThreadgroupPositionInGrid,
            "thread_index_in_threadgroup" => KernRole::ThreadIndexInThreadgroup,
            "thread_position_in_grid" => KernRole::ThreadPositionInGrid,
            "stage_in" => {
                if let Some(name) = arg_type_name(node) {
                    stage_input_type_names.insert(idx, name);
                }
                KernRole::StageInput(resource_location(node, idx))
            }
            VARIANT_ABSENT_TEXTURE_ROLE => KernRole::VariantAbsentTexture,
            _ if declares_disabled_texture(&strs) => KernRole::VariantAbsentTexture,
            other => stage_execution_group_role(Stage::Kernel, other).map_or(
                KernRole::Other,
                |(fact, lanes)| KernRole::ExecutionGroup { fact, lanes },
            ),
        };
        roles.push((idx, role));
    }
    let argument_buffers = ArgumentBuffers::new(indirect_buffer_struct_refs);
    let embedded_textures = if body_uses_texture_intrinsic(ll) {
        detect_embedded_textures(nodes, &argument_buffers, &top_level_texture_locations)
    } else {
        vec![]
    };
    let top_level_sampler_locations = roles
        .iter()
        .filter_map(|(_, role)| match role {
            KernRole::Sampler(location) => Some(*location),
            _ => None,
        })
        .collect::<Vec<_>>();
    let embedded_samplers = if body_uses_texture_intrinsic(ll) {
        detect_embedded_samplers(nodes, &argument_buffers, &top_level_sampler_locations)
    } else {
        vec![]
    };
    let embedded_arguments = detect_embedded_arguments(nodes, &argument_buffers);
    let unsurfaced = unsurfaced_embedded_resources(nodes, &argument_buffers);
    let mut implicit_imageblock_attachments = detect_implicit_imageblock_attachments(ll)?;
    let mut aliased_implicit_imageblock_planes = HashMap::default();
    for param in &aliased_implicit_imageblock_params {
        let Some(layout) = imageblock_layouts.get(param) else {
            continue;
        };
        let Some(planes) = aliased_imageblock_planes(layout).ok().flatten() else {
            continue;
        };
        for plane in &planes {
            implicit_imageblock_attachments.push(ImplicitImageblockAttachment {
                attachment: plane.attachment,
                data_rate: 0,
                max_index: Some(0),
                format: plane.format,
                reads: true,
                writes: true,
            });
        }
        aliased_implicit_imageblock_planes.insert(*param, planes);
    }
    Some(KernMeta {
        roles,
        unmodelled_input_params,
        aliased_implicit_imageblock_params,
        aliased_implicit_imageblock_planes,
        unmodelled_stage_attributes,
        max_work_group_size,
        function_constant_buffer_locations,
        buffer_layouts,
        imageblock_layouts,
        implicit_imageblock_attachments,
        buffer_address_spaces,
        buffer_type_sizes,
        buffer_object_sizes,
        buffer_type_names,
        buffer_accesses,
        texture_type_names,
        declared_descriptor_counts,
        stage_input_type_names,
        embedded_textures,
        embedded_samplers,
        embedded_arguments,
        unsurfaced_embedded_resources: unsurfaced,
    })
}

pub fn implicit_imageblock_texture_format(name: &str) -> Result<Option<TextureFormat>, String> {
    let suffix = name
        .strip_prefix("air.load.implicit_imageblock.")
        .or_else(|| name.strip_prefix("air.store.implicit_imageblock."));
    let Some(suffix) = suffix else {
        return Ok(None);
    };
    let format = match suffix {
        "f16" => TextureFormat::R16f,
        "v2f16" => TextureFormat::Rg16f,
        "v4f16" => TextureFormat::Rgba16f,
        "f32" => TextureFormat::R32f,
        "v4f32" => TextureFormat::Rgba32f,
        "i32" => TextureFormat::R32ui,
        _ => {
            return Err(format!(
                "{name} has unsupported implicit imageblock texel type"
            ))
        }
    };
    Ok(Some(format))
}

fn detect_implicit_imageblock_attachments(ll: &str) -> Option<Vec<ImplicitImageblockAttachment>> {
    let mut attachments =
        std::collections::BTreeMap::<(u32, u32, TextureFormat), ImplicitImageblockAttachment>::new(
        );
    for line in ll.lines() {
        let Some(at) = line.find("@air.") else {
            continue;
        };
        let call = &line[at + 1..];
        let Some(open) = call.find('(') else { continue };
        let name = &call[..open];
        let (reads, writes, value_prefix) = if name.starts_with("air.load.implicit_imageblock.") {
            (true, false, 0usize)
        } else if name.starts_with("air.store.implicit_imageblock.") {
            (false, true, 1usize)
        } else {
            continue;
        };
        let Some(close) = call[open + 1..].find(')') else {
            continue;
        };
        let args = split_top_level_commas(&call[open + 1..open + 1 + close]);
        let Some(attachment) = args
            .get(value_prefix)
            .and_then(|arg| typed_u32_constant(arg))
        else {
            continue;
        };
        let index = args
            .get(value_prefix + 2)
            .and_then(|arg| typed_u32_constant(arg));
        let Some(data_rate) = args
            .get(value_prefix + 3)
            .and_then(|arg| typed_u32_constant(arg))
        else {
            continue;
        };
        let format = implicit_imageblock_texture_format(name).ok().flatten()?;
        let entry = attachments
            .entry((attachment, data_rate, format))
            .or_insert(ImplicitImageblockAttachment {
                attachment,
                data_rate,
                max_index: index,
                format,
                reads: false,
                writes: false,
            });
        entry.reads |= reads;
        entry.writes |= writes;
        entry.max_index = match (entry.max_index, index) {
            (Some(left), Some(right)) => Some(left.max(right)),
            _ => None,
        };
    }
    Some(attachments.into_values().collect())
}

fn typed_u32_constant(value: &str) -> Option<u32> {
    value.split_whitespace().last()?.parse().ok()
}

fn queried_acceleration_structure_role(ll: &str, role: &str) -> bool {
    matches!(
        role,
        "instance_acceleration_structure" | "primitive_acceleration_structure"
    ) && (ll.contains("_intersection_query.") || ll.contains("@air.intersect."))
}

fn body_uses_acceleration_structure_shadow(ll: &str) -> bool {
    ll.contains("@air.get_instance_count_instance_acceleration_structure")
        || ll.contains("@air.get_primitive_acceleration_structure_instance_acceleration_structure")
        || ll.lines().any(|line| {
            let Some(start) = line.find("@air.intersect.") else {
                return false;
            };
            let Some(end) = line[start + 1..].find('(') else {
                return false;
            };
            let callee = &line[start + 1..start + 1 + end];
            AirIntersectionFamily::parse(callee)
                .ok()
                .flatten()
                .is_some_and(|family| family.instancing != AirIntersectionInstancing::None)
        })
}

pub fn air_intersection_calls_are_supported(ll: &str) -> bool {
    crate::native::ray_intersection::all_air_intersection_calls_are_lowerable(ll)
}

fn collect_nodes(ll: &str) -> HashMap<u32, String> {
    #[cfg(test)]
    AIR_META_PARSE_COUNT.with(|count| count.set(count.get() + 1));
    let mut nodes = HashMap::default();
    for l in ll.lines() {
        let l = l.trim();
        let Some(rest) = l.strip_prefix('!') else {
            continue;
        };
        let Some((eq, prefix_len)) =
            rest.find(" = !{")
                .map(|eq| (eq, " = !{".len()))
                .or_else(|| {
                    rest.find(" = distinct !{")
                        .map(|eq| (eq, " = distinct !{".len()))
                })
        else {
            continue;
        };
        let Ok(id) = rest[..eq].parse::<u32>() else {
            continue;
        };
        let body = &rest[eq + prefix_len..];
        let body = body.strip_suffix('}').unwrap_or(body);
        nodes.insert(id, body.to_string());
    }
    nodes
}

#[cfg(test)]
thread_local! {
    static AIR_META_PARSE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_air_meta_parse_count() {
    AIR_META_PARSE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn air_meta_parse_count() -> usize {
    AIR_META_PARSE_COUNT.with(std::cell::Cell::get)
}

pub fn entry_name(ll: &str, stage: &str) -> Option<String> {
    let nodes = collect_nodes(ll);
    entry_name_from_nodes(ll, stage, &nodes)
}

fn entry_name_from_nodes(ll: &str, stage: &str, nodes: &HashMap<u32, String>) -> Option<String> {
    let root = stage_root(ll, stage)?;
    let body = nodes.get(&root)?;
    let at = body.find('@')?;
    let after = &body[at + 1..];
    let name = if let Some(quoted) = after.strip_prefix('"') {
        quoted_symbol_name(quoted)?
    } else {
        after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '$')
            .collect()
    };
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn pointer_symbol(body: &str) -> Option<String> {
    let at = body.find('@')?;
    let after = &body[at + 1..];
    let name = if let Some(quoted) = after.strip_prefix('"') {
        quoted_symbol_name(quoted)?
    } else {
        after
            .chars()
            .take_while(|c| c.is_alphanumeric() || matches!(*c, '_' | '.' | '$'))
            .collect()
    };
    (!name.is_empty()).then_some(name)
}

fn quoted_symbol_name(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => {
                let hi = chars.peek().copied();
                let mut clone = chars.clone();
                let lo = {
                    clone.next();
                    clone.peek().copied()
                };
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    if hi.is_ascii_hexdigit() && lo.is_ascii_hexdigit() {
                        chars.next();
                        chars.next();
                        let byte = u8::from_str_radix(&format!("{hi}{lo}"), 16).ok()?;
                        out.push(byte as char);
                        continue;
                    }
                }
                out.push(chars.next().unwrap_or('\\'));
            }
            _ => out.push(ch),
        }
    }
    None
}

fn function_param_pointer_address_spaces(ll: &str, name: &str) -> Option<HashMap<u32, u32>> {
    let params = function_param_list(ll, name)?;
    let mut out = HashMap::default();
    for (idx, param) in split_top_level_commas(&params).into_iter().enumerate() {
        let param = param.trim_start();
        if !param.starts_with("ptr") {
            continue;
        }
        if let Some(addrspace) = param.find("addrspace(").and_then(|pos| {
            let after = &param[pos + "addrspace(".len()..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        }) {
            out.insert(idx as u32, addrspace);
        }
    }
    Some(out)
}

fn function_param_list(ll: &str, name: &str) -> Option<String> {
    let unquoted = format!("@{name}(");
    let quoted = format!("@\"{name}\"(");
    let (start, needle_len) = ll
        .find(&unquoted)
        .map(|start| (start, unquoted.len()))
        .or_else(|| ll.find(&quoted).map(|start| (start, quoted.len())))?;
    let start = start + needle_len;
    let mut depth = 1u32;
    let mut end = start;
    for (off, ch) in ll[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = start + off;
                    break;
                }
            }
            _ => {}
        }
    }
    (depth == 0).then(|| ll[start..end].to_string())
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut angle_depth = 0i32;
    let mut paren_depth = 0i32;
    let mut brace_depth = 0i32;
    for (idx, ch) in s.char_indices() {
        match ch {
            '<' => angle_depth += 1,
            '>' => angle_depth -= 1,
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '{' => brace_depth += 1,
            '}' => brace_depth -= 1,
            ',' if angle_depth == 0 && paren_depth == 0 && brace_depth == 0 => {
                items.push(s[start..idx].trim().to_string());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        items.push(tail.to_string());
    }
    items
}

fn stage_root(ll: &str, stage: &str) -> Option<u32> {
    let needle = format!("!air.{stage} = !{{!");
    for l in ll.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix(&needle) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return digits.parse().ok();
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StageAttribute {
    Patch(String),
    EarlyFragmentTests,
    MaxWorkGroupSize(u32),
    Unrecognized(String),
}

impl StageAttribute {
    pub(crate) fn describe(&self) -> String {
        match self {
            StageAttribute::Patch(_) => "air.patch".to_string(),
            StageAttribute::EarlyFragmentTests => "early_fragment_tests".to_string(),
            StageAttribute::MaxWorkGroupSize(_) => "air.max_work_group_size".to_string(),
            StageAttribute::Unrecognized(text) => text.clone(),
        }
    }
}

fn stage_root_attributes(rootc: &str, nodes: &HashMap<u32, String>) -> Vec<StageAttribute> {
    let operands = split_top_level_commas(rootc);
    operands
        .iter()
        .skip(3)
        .filter_map(|operand| {
            let referenced = operand
                .strip_prefix('!')
                .and_then(|digits| digits.parse::<u32>().ok())
                .and_then(|id| nodes.get(&id));
            Some(match referenced {
                Some(node) if node.contains("!\"air.patch\"") => {
                    StageAttribute::Patch(node.clone())
                }
                Some(node) if node.contains("!\"air.max_work_group_size\"") => {
                    match i32_after_marker(node, "air.max_work_group_size") {
                        Some(size) => StageAttribute::MaxWorkGroupSize(size),
                        None => StageAttribute::Unrecognized(
                            "air.max_work_group_size states no thread count".to_string(),
                        ),
                    }
                }
                Some(node) if node.trim().is_empty() => return None,
                Some(node) => StageAttribute::Unrecognized(format!("!{{{node}}}")),
                None if operand == "!\"early_fragment_tests\"" => {
                    StageAttribute::EarlyFragmentTests
                }
                None => StageAttribute::Unrecognized(operand.clone()),
            })
        })
        .collect()
}

fn refs_in(body: &str) -> Vec<u32> {
    body.split(',')
        .filter_map(|s| {
            s.trim()
                .strip_prefix('!')
                .and_then(|x| x.parse::<u32>().ok())
        })
        .collect()
}

fn first_i32(body: &str) -> Option<u32> {
    let mut it = body.split_whitespace().peekable();
    while let Some(tok) = it.next() {
        if tok == "i32" {
            if let Some(n) = it.peek() {
                let n = n.trim_end_matches(',');
                if let Ok(v) = n.parse::<u32>() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn i32_after_marker(body: &str, marker: &str) -> Option<u32> {
    let marker = format!("!\"{marker}\"");
    let pos = body.find(&marker)?;
    first_i32(&body[pos + marker.len()..])
}

fn location_index(body: &str, fallback: u32) -> u32 {
    match location_operands(body).map(|operands| operands.index) {
        Some(LocationOperand::Literal(index)) => index,
        _ => fallback,
    }
}

fn declared_descriptor_count(body: &str) -> Option<u32> {
    match location_operands(body)?.count? {
        LocationOperand::Literal(count) if count > 1 => Some(count),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocationOperand {
    Literal(u32),
    Global(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LocationOperands {
    pub index: LocationOperand,
    pub count: Option<LocationOperand>,
}

fn marker_operands(body: &str, marker: &str) -> Vec<Option<LocationOperand>> {
    let marker = format!("!\"{marker}\"");
    let Some(position) = body.find(&marker) else {
        return vec![];
    };
    split_metadata_operands(&body[position + marker.len()..])
        .into_iter()
        .map(|operand| {
            if let Some(literal) = operand.strip_prefix("i32 ") {
                return literal.trim().parse().ok().map(LocationOperand::Literal);
            }
            let at = operand.find('@')?;
            let name = operand[at..]
                .chars()
                .take_while(|character| {
                    !character.is_whitespace() && !matches!(*character, ',' | ')' | '(' | '[' | ']')
                })
                .collect::<String>();
            (name.len() > 1).then_some(LocationOperand::Global(name))
        })
        .collect()
}

pub(crate) fn location_operands(body: &str) -> Option<LocationOperands> {
    let mut operands = marker_operands(body, "air.location_index").into_iter();
    Some(LocationOperands {
        index: operands.next().flatten()?,
        count: operands.next().flatten(),
    })
}

pub(crate) fn declared_slot(
    body: &str,
    marker: &str,
    fallback: u32,
    static_int_globals: &HashMap<String, u32>,
) -> u32 {
    match marker_operands(body, marker).into_iter().next().flatten() {
        Some(LocationOperand::Literal(slot)) => slot,
        Some(LocationOperand::Global(global)) => {
            static_int_globals.get(&global).copied().unwrap_or(fallback)
        }
        None => fallback,
    }
}

fn split_metadata_operands(tail: &str) -> Vec<String> {
    let mut operands = vec![];
    let mut current = String::new();
    let mut quoted = false;
    for character in tail.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ',' if !quoted => {
                operands.push(std::mem::take(&mut current).trim().to_string());
            }
            _ => current.push(character),
        }
    }
    operands.push(current.trim().to_string());
    operands.retain(|operand| !operand.is_empty());
    operands
}

fn render_target_location(
    body: &str,
    fallback: u32,
    static_int_globals: &HashMap<String, u32>,
) -> u32 {
    declared_slot(body, "air.render_target", fallback, static_int_globals)
}

fn address_space(body: &str) -> Option<u32> {
    i32_after_marker(body, "air.address_space")
}

fn role_strings(body: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = body;
    while let Some(p) = rest.find("!\"air.") {
        let after = &rest[p + 6..];
        let end = after.find('"').unwrap_or(after.len());
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

fn primary_role(strs: &[String]) -> Option<&str> {
    strs.iter()
        .map(String::as_str)
        .find(|s| *s != "function_constant")
}

fn declared_role(body: &str) -> Option<String> {
    let role_name = |operand: &str| {
        operand
            .strip_prefix("!\"air.")
            .and_then(|rest| rest.strip_suffix('"'))
            .map(str::to_string)
    };
    let mut operands = split_metadata_operands(body).into_iter();
    let first = operands.find_map(|operand| role_name(&operand))?;
    if first != "function_constant" {
        return (first != "function_constant_disabled").then_some(first);
    }
    operands.next().filter(|operand| {
        operand.strip_prefix('!').is_some_and(|rest| {
            !rest.is_empty() && rest.chars().all(|character| character.is_ascii_digit())
        })
    })?;
    operands
        .next()
        .as_deref()
        .and_then(role_name)
        .filter(|role| !ARGUMENT_IDENTITY_MARKERS.contains(&role.as_str()))
}

const ARGUMENT_IDENTITY_MARKERS: &[&str] = &["arg_name", "arg_type_name"];

fn function_constant_gate_global(body: &str, nodes: &HashMap<u32, String>) -> Option<String> {
    if role_strings(body).first().map(String::as_str) != Some("function_constant") {
        return None;
    }
    refs_in(body)
        .into_iter()
        .filter_map(|r| nodes.get(&r))
        .find_map(|node| {
            let at = node.find('@')?;
            let name = node[at..]
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(*c, ',' | ')' | '(' | '[' | ']'))
                .collect::<String>();
            (name.len() > 1).then_some(name)
        })
}

fn function_constant_gate_disabled(
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> bool {
    function_constant_gate_global(body, nodes)
        .and_then(|global| static_int_globals.get(&global).copied())
        == Some(0)
}

fn function_constant_gate_enabled(
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> bool {
    function_constant_gate_global(body, nodes)
        .and_then(|global| static_int_globals.get(&global).copied())
        .is_some_and(|value| value != 0)
}

fn present_gated_buffer_role<'a>(
    role: &'a str,
    strs: &'a [String],
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> &'a str {
    if role != "function_constant" || primary_role(strs) != Some("buffer") {
        return role;
    }
    if function_constant_gate_enabled(body, nodes, static_int_globals) {
        "buffer"
    } else {
        role
    }
}

fn variant_texture_slot(
    body: &str,
    fallback: u32,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> Option<u32> {
    let slot = location_index_with_static(body, fallback, static_int_globals);
    if slot == u32::MAX {
        return None;
    }
    let summed_by_function_constant = matches!(
        location_operands(body).map(|operands| operands.index),
        Some(LocationOperand::Global(_))
    );
    if summed_by_function_constant
        && function_constant_gate_disabled(body, nodes, static_int_globals)
    {
        return None;
    }
    Some(slot)
}

fn declares_disabled_texture(strs: &[String]) -> bool {
    strs.first().map(String::as_str) == Some("function_constant_disabled")
        && strs.get(1).map(String::as_str) == Some("texture")
}

const VARIANT_ABSENT_TEXTURE_ROLE: &str = "variant_absent_texture";

fn unmodelled_declared_role(
    is_modelled: impl Fn(&str) -> bool,
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> Option<String> {
    let declared = declared_role(body)?;
    (!is_modelled(&declared) && !function_constant_gate_disabled(body, nodes, static_int_globals))
        .then_some(declared)
}

fn metadata_enabled_by_default(
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> bool {
    function_constant_gate_global(body, nodes)
        .map(|global| {
            static_int_globals
                .get(&global)
                .is_some_and(|value| *value != 0)
        })
        .unwrap_or(true)
}

pub(crate) fn specialize_function_constant_metadata(ll: &str) -> String {
    let nodes = collect_nodes(ll);
    let static_int_globals = static_init_int_global_values(ll);
    let marker = "!\"air.function_constant\"";
    let mut output = String::with_capacity(ll.len());

    for line in ll.split_inclusive('\n') {
        let value = function_constant_gate_global(line, &nodes)
            .and_then(|global| static_int_globals.get(&global).copied());
        match value {
            Some(0) => {
                output.push_str(&line.replacen(marker, "!\"air.function_constant_disabled\"", 1))
            }
            Some(_) => {
                let rewritten = line.find(marker).and_then(|start| {
                    let after_marker = &line[start + marker.len()..];
                    let next_role = after_marker.find("!\"air.")?;
                    Some(format!("{}{}", &line[..start], &after_marker[next_role..]))
                });
                if let Some(rewritten) = rewritten {
                    output.push_str(&rewritten);
                } else {
                    output.push_str(line);
                }
            }
            None => output.push_str(line),
        }
    }
    output
}

const FC_PROMOTED_RESOURCE_ROLES: &[&str] = &[
    "imageblock",
    "intersection_function_table",
    "sampler",
    "stage_in",
    "texture",
    "visible_function_table",
];

fn fc_promoted_role(strs: &[String], promote_buffers: bool) -> Option<&str> {
    match gated_role(strs, |role| {
        FC_PROMOTED_RESOURCE_ROLES.contains(&role) || air_role_is_system_value(role)
    }) {
        Some("function_constant") if promote_buffers => gated_role(strs, |role| role == "buffer"),
        other => other,
    }
}

fn gated_role(strs: &[String], modelled: impl Fn(&str) -> bool) -> Option<&str> {
    let first = strs.first().map(String::as_str)?;
    if first != "function_constant" {
        return Some(first);
    }
    Some(match primary_role(strs) {
        Some(role) if modelled(role) => role,
        _ => first,
    })
}

fn string_after_marker(body: &str, marker: &str) -> Option<String> {
    let marker = format!("!\"{marker}\"");
    let pos = body.find(&marker)?;
    let after = &body[pos + marker.len()..];
    let pos = after.find("!\"")?;
    let value = &after[pos + 2..];
    let end = value.find('"')?;
    Some(value[..end].to_string())
}

fn arg_type_name(body: &str) -> Option<String> {
    string_after_marker(body, "air.arg_type_name")
}

fn declared_buffer_access(body: &str) -> Option<BufferAccess> {
    if body.contains("air.read_write") {
        Some(BufferAccess::ReadWrite)
    } else if body.contains("air.write") {
        Some(BufferAccess::WriteOnly)
    } else if body.contains("air.read") {
        Some(BufferAccess::ReadOnly)
    } else {
        None
    }
}

fn arg_name(body: &str) -> Option<String> {
    string_after_marker(body, "air.arg_name")
}

fn ref_after_marker(body: &str, marker: &str) -> Option<u32> {
    let marker = format!("!\"{marker}\"");
    let after = body.get(body.find(&marker)? + marker.len()..)?;
    let bang = after.find('!')?;
    let digits = after[bang + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn parse_fragment_imageblock_master(body: &str) -> Option<Vec<FragmentImageblockMember>> {
    let toks = tokenize(body);
    let mut members = Vec::new();
    let mut i = 0;
    while i + 4 < toks.len() {
        let (offset, size, type_name, semantic) = match (
            toks.get(i),
            toks.get(i + 1),
            toks.get(i + 2),
            toks.get(i + 3),
            toks.get(i + 4),
        ) {
            (
                Some(Tok::Int(offset)),
                Some(Tok::Int(size)),
                Some(Tok::Int(_array_len)),
                Some(Tok::Str(type_name)),
                Some(Tok::Str(semantic)),
            ) => (*offset, *size, type_name.clone(), semantic.clone()),
            _ => return None,
        };
        i += 5;
        let raster_order_group = match (toks.get(i), toks.get(i + 1)) {
            (Some(Tok::Str(marker)), Some(Tok::Int(group)))
                if marker == "air.raster_order_group" =>
            {
                i += 2;
                *group
            }
            _ => return None,
        };
        members.push(FragmentImageblockMember {
            offset,
            size,
            type_name,
            semantic,
            raster_order_group,
        });
    }
    (!members.is_empty() && i == toks.len()).then_some(members)
}

fn parse_fragment_imageblock_projection(
    nodes: &HashMap<u32, String>,
    node: &str,
    interface_index: u32,
    master_members: &[FragmentImageblockMember],
) -> Option<FragmentImageblockProjection> {
    let projection_ref = struct_info_ref(node)?;
    let projection = nodes.get(&projection_ref)?;
    let toks = tokenize(projection);
    let mut members = Vec::new();
    let mut i = 0;
    let mut projection_member = 0;
    while i + 4 < toks.len() {
        let semantic = match (
            toks.get(i),
            toks.get(i + 1),
            toks.get(i + 2),
            toks.get(i + 3),
            toks.get(i + 4),
        ) {
            (
                Some(Tok::Int(_)),
                Some(Tok::Int(_)),
                Some(Tok::Int(_)),
                Some(Tok::Str(_)),
                Some(Tok::Str(semantic)),
            ) => semantic,
            _ => return None,
        };
        let master_member = master_members
            .iter()
            .position(|member| member.semantic == *semantic)? as u32;
        members.push(FragmentImageblockProjectionMember {
            projection_member,
            master_member,
        });
        projection_member += 1;
        i += 5;
        if matches!(toks.get(i), Some(Tok::Str(marker)) if marker == "air.raster_order_group")
            && matches!(toks.get(i + 1), Some(Tok::Int(_)))
        {
            i += 2;
        }
    }
    (!members.is_empty() && i == toks.len()).then_some(FragmentImageblockProjection {
        interface_index,
        members,
    })
}

fn parse_fragment_imageblock(
    nodes: &HashMap<u32, String>,
    out_ref: u32,
    in_ref: u32,
) -> Option<FragmentImageblock> {
    let output_nodes = nodes
        .get(&out_ref)
        .map(|body| refs_in(body))
        .unwrap_or_default();
    let input_nodes = nodes
        .get(&in_ref)
        .map(|body| refs_in(body))
        .unwrap_or_default();
    let imageblock_node = output_nodes
        .iter()
        .chain(input_nodes.iter())
        .filter_map(|id| nodes.get(id))
        .find(|body| primary_role(&role_strings(body)) == Some("imageblock_data"))?;
    let master_ref = ref_after_marker(imageblock_node, "air.imageblock_master")
        .or_else(|| struct_info_ref(imageblock_node))?;
    let master_members = parse_fragment_imageblock_master(nodes.get(&master_ref)?)?;
    let sample_size = i32_after_marker(imageblock_node, "air.imageblock_data_size")?;

    let outputs = output_nodes
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            let node = nodes.get(id)?;
            (primary_role(&role_strings(node)) == Some("imageblock_data"))
                .then(|| {
                    parse_fragment_imageblock_projection(nodes, node, index as u32, &master_members)
                })
                .flatten()
        })
        .collect();
    let inputs = input_nodes
        .iter()
        .filter_map(|id| {
            let node = nodes.get(id)?;
            let index = first_i32(node)?;
            (primary_role(&role_strings(node)) == Some("imageblock_data"))
                .then(|| parse_fragment_imageblock_projection(nodes, node, index, &master_members))
                .flatten()
        })
        .collect();
    Some(FragmentImageblock {
        sample_size,
        members: master_members,
        inputs,
        outputs,
    })
}

pub fn parse_air_fragment_meta(ll: &str) -> Option<FragMeta> {
    parse_air_fragment_meta_with_entry(ll).0
}

pub(crate) fn parse_air_fragment_meta_with_entry(ll: &str) -> (Option<FragMeta>, Option<String>) {
    let nodes = collect_nodes(ll);
    let entry = entry_name_from_nodes(ll, "fragment", &nodes);
    let meta = parse_air_fragment_meta_with_nodes(ll, &nodes, entry.as_deref());
    (meta, entry)
}

pub const AIR_SYSTEM_VALUE_ROLES: &[&str] = &[
    "amplification_count",
    "amplification_id",
    "barycentric_coord",
    "base_instance",
    "base_vertex",
    "dispatch_threads_per_threadgroup",
    "front_facing",
    "instance_id",
    "patch_id",
    "point_coord",
    "position",
    "position_in_patch",
    "primitive_id",
    "render_target_array_index",
    "sample_id",
    "sample_mask_in",
    "thread_index_in_threadgroup",
    "thread_position_in_grid",
    "thread_position_in_threadgroup",
    "threadgroup_position_in_grid",
    "threadgroups_per_grid",
    "threads_per_grid",
    "threads_per_threadgroup",
    "vertex_id",
    "viewport_array_index",
];

pub fn air_role_is_system_value(role: &str) -> bool {
    AIR_SYSTEM_VALUE_ROLES.contains(&role) || execution_group_role(role).is_some()
}

fn present_system_value_role<'a>(
    role: &'a str,
    body: &str,
    nodes: &HashMap<u32, String>,
    static_int_globals: &HashMap<String, u32>,
) -> &'a str {
    if air_role_is_system_value(role)
        && !metadata_enabled_by_default(body, nodes, static_int_globals)
    {
        return "";
    }
    role
}

pub const FRAGMENT_INPUT_ROLES: &[&str] = &[
    "amplification_count",
    "amplification_id",
    "barycentric_coord",
    "buffer",
    "fragment_input",
    "front_facing",
    "imageblock_data",
    "indirect_buffer",
    "intersection_function_table",
    "point_coord",
    "position",
    "primitive_id",
    "render_target",
    "render_target_array_index",
    "sample_id",
    "sample_mask_in",
    "sampler",
    "texture",
    "viewport_array_index",
    "visible_function_table",
];

pub const VERTEX_INPUT_ROLES: &[&str] = &[
    "amplification_count",
    "amplification_id",
    "base_instance",
    "base_vertex",
    "buffer",
    "indirect_buffer",
    "instance_id",
    "intersection_function_table",
    "patch_control_point_input",
    "patch_id",
    "patch_input",
    "position_in_patch",
    "sampler",
    "texture",
    "vertex_id",
    "vertex_input",
    "visible_function_table",
];

pub const KERNEL_INPUT_ROLES: &[&str] = &[
    "buffer",
    "dispatch_threads_per_threadgroup",
    "imageblock",
    "indirect_buffer",
    "instance_acceleration_structure",
    "intersection_function_table",
    "primitive_acceleration_structure",
    "sampler",
    "stage_in",
    "texture",
    "thread_index_in_threadgroup",
    "thread_position_in_grid",
    "thread_position_in_threadgroup",
    "threadgroup_position_in_grid",
    "threadgroups_per_grid",
    "threads_per_grid",
    "threads_per_threadgroup",
    "visible_function_table",
];

pub fn air_input_role_is_modelled(stage: Stage, role: &str) -> bool {
    let inventory = match stage {
        Stage::Fragment => FRAGMENT_INPUT_ROLES,
        Stage::Vertex => VERTEX_INPUT_ROLES,
        Stage::Kernel => KERNEL_INPUT_ROLES,
    };
    inventory.contains(&role) || stage_execution_group_role(stage, role).is_some()
}

pub const AIR_EXECUTION_GROUPS: &[(&str, u32)] = &[("quadgroup", 4), ("simdgroup", 32)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionGroupFact {
    ThreadIndexInGroup,
    GroupIndexInThreadgroup,
    GroupsPerThreadgroup,
    ThreadsPerGroup,
}

impl ExecutionGroupFact {
    pub fn needs_a_threadgroup(self) -> bool {
        match self {
            Self::ThreadIndexInGroup | Self::ThreadsPerGroup => false,
            Self::GroupIndexInThreadgroup | Self::GroupsPerThreadgroup => true,
        }
    }
}

pub fn execution_group_role(marker: &str) -> Option<(ExecutionGroupFact, u32)> {
    let marker = marker.strip_prefix("dispatch_").unwrap_or(marker);
    let (fact, group) = if let Some(group) = marker.strip_prefix("thread_index_in_") {
        (ExecutionGroupFact::ThreadIndexInGroup, group)
    } else if let Some(group) = marker.strip_suffix("_index_in_threadgroup") {
        (ExecutionGroupFact::GroupIndexInThreadgroup, group)
    } else if let Some(group) = marker.strip_suffix("s_per_threadgroup") {
        (ExecutionGroupFact::GroupsPerThreadgroup, group)
    } else {
        let group = marker.strip_prefix("threads_per_")?;
        (ExecutionGroupFact::ThreadsPerGroup, group)
    };
    let lanes = AIR_EXECUTION_GROUPS
        .iter()
        .find_map(|&(name, lanes)| (name == group).then_some(lanes))?;
    Some((fact, lanes))
}

fn stage_execution_group_role(stage: Stage, marker: &str) -> Option<(ExecutionGroupFact, u32)> {
    execution_group_role(marker)
        .filter(|(fact, _)| stage == Stage::Kernel || !fact.needs_a_threadgroup())
}

pub const FRAGMENT_OUTPUT_ROLES: &[&str] = &[
    "render_target",
    "depth",
    "stencil",
    "sample_mask",
    "imageblock_data",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum VertexOutputKind {
    Position,
    PointSize,
    ClipDistance,
    ViewportArrayIndex,
    RenderTargetArrayIndex,
    Varying,
}

impl VertexOutputKind {
    fn is_system_value(self) -> bool {
        !matches!(self, Self::Varying | Self::Position)
    }
}

const VERTEX_OUTPUT_ROLES: &[(&str, VertexOutputKind)] = &[
    ("clip_distance", VertexOutputKind::ClipDistance),
    ("point_size", VertexOutputKind::PointSize),
    ("position", VertexOutputKind::Position),
    (
        "render_target_array_index",
        VertexOutputKind::RenderTargetArrayIndex,
    ),
    ("vertex_output", VertexOutputKind::Varying),
    ("viewport_array_index", VertexOutputKind::ViewportArrayIndex),
];

fn vertex_input_gated_role(strs: &[String]) -> Option<&str> {
    gated_role(strs, |role| {
        FC_PROMOTED_RESOURCE_ROLES.contains(&role)
            || air_role_is_system_value(role)
            || matches!(role, "patch_input" | "vertex_input")
    })
}

fn vertex_output_kind(role: &str) -> Option<VertexOutputKind> {
    VERTEX_OUTPUT_ROLES
        .iter()
        .find(|(name, _)| *name == role)
        .map(|(_, kind)| *kind)
}

fn parse_air_fragment_meta_with_nodes(
    ll: &str,
    nodes: &HashMap<u32, String>,
    entry: Option<&str>,
) -> Option<FragMeta> {
    let static_int_globals = static_init_int_global_values(ll);
    let root = stage_root(ll, "fragment")?;
    let rootc = nodes.get(&root)?;
    let refs = refs_in(rootc);
    let (out_ref, in_ref) = (*refs.first()?, *refs.get(1)?);
    let mut early_fragment_tests = false;
    let mut unmodelled_stage_attributes = vec![];
    for attribute in stage_root_attributes(rootc, nodes) {
        match attribute {
            StageAttribute::EarlyFragmentTests => early_fragment_tests = true,
            other => unmodelled_stage_attributes.push(other.describe()),
        }
    }
    let fragment_imageblock = parse_fragment_imageblock(nodes, out_ref, in_ref);
    let render_target_members: Vec<(u32, u32)> = nodes
        .get(&out_ref)
        .map(|c| {
            refs_in(c)
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let node = nodes.get(&r)?;
                    let roles = role_strings(node);
                    let is_render_target = primary_role(&roles) == Some("render_target");
                    (is_render_target
                        && metadata_enabled_by_default(node, nodes, &static_int_globals))
                    .then(|| {
                        (
                            i as u32,
                            render_target_location(node, i as u32, &static_int_globals),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let output_members_with_role = |role: &str| -> Vec<u32> {
        nodes
            .get(&out_ref)
            .map(|c| {
                refs_in(c)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        let node = nodes.get(&r)?;
                        (primary_role(&role_strings(node)) == Some(role)
                            && metadata_enabled_by_default(node, nodes, &static_int_globals))
                        .then_some(i as u32)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let depth_members = output_members_with_role("depth");
    let depth_qualifier = nodes.get(&out_ref).and_then(|c| {
        refs_in(c).into_iter().find_map(|r| {
            let node = nodes.get(&r)?;
            (primary_role(&role_strings(node)) == Some("depth"))
                .then(|| string_after_marker(node, "air.depth_qualifier"))
                .flatten()
                .and_then(|qualifier| match qualifier.as_str() {
                    "air.any" => Some(DepthQualifier::Any),
                    "air.less" => Some(DepthQualifier::Less),
                    "air.greater" => Some(DepthQualifier::Greater),
                    _ => None,
                })
        })
    });
    let stencil_members = output_members_with_role("stencil");
    let sample_mask_members = output_members_with_role("sample_mask");
    let unmodelled_output_members: Vec<(u32, String)> = nodes
        .get(&out_ref)
        .map(|c| {
            refs_in(c)
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let node = nodes.get(&r)?;
                    let role = unmodelled_declared_role(
                        |role| FRAGMENT_OUTPUT_ROLES.contains(&role),
                        node,
                        nodes,
                        &static_int_globals,
                    )?;
                    Some((i as u32, role))
                })
                .collect()
        })
        .unwrap_or_default();
    let render_target_dual_members: Vec<u32> = nodes
        .get(&out_ref)
        .map(|c| {
            refs_in(c)
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let node = nodes.get(&r)?;
                    (primary_role(&role_strings(node)) == Some("render_target")
                        && matches!(
                            marker_operands(node, "air.render_target").get(1),
                            Some(Some(LocationOperand::Literal(1)))
                        ))
                    .then_some(i as u32)
                })
                .collect()
        })
        .unwrap_or_default();
    let render_target_indices: Vec<u32> = render_target_members
        .iter()
        .map(|(_, location)| *location)
        .collect();
    let n_render_targets = render_target_indices.len() as u32;
    let render_target_type_names: HashMap<u32, String> = nodes
        .get(&out_ref)
        .map(|c| {
            refs_in(c)
                .into_iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let node = nodes.get(&r)?;
                    let roles = role_strings(node);
                    let is_render_target = primary_role(&roles) == Some("render_target");
                    if is_render_target
                        && metadata_enabled_by_default(node, nodes, &static_int_globals)
                    {
                        arg_type_name(node).map(|name| (i as u32, name))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut roles = vec![];
    let mut input_type_names = HashMap::default();
    let mut unmodelled_input_params: Vec<(u32, String)> = vec![];
    let mut buffer_layouts = HashMap::default();
    let mut varying_types = HashMap::default();
    let mut varying_names = HashMap::default();
    let mut varying_user_semantics = HashMap::default();
    let mut varying_interpolation = HashMap::default();
    let mut texture_type_names = HashMap::default();
    let mut declared_descriptor_counts = HashMap::default();
    let mut color_input_type_names = HashMap::default();
    let mut varying_loc = 0u32;
    let mut buffer_address_spaces = HashMap::default();
    let mut buffer_type_sizes = HashMap::default();
    let mut buffer_object_sizes = HashMap::default();
    let mut buffer_type_names = HashMap::default();
    let mut buffer_accesses = HashMap::default();
    let mut indirect_buffer_struct_refs: Vec<(u32, u32, u32)> = Vec::new();
    let param_address_spaces = entry
        .and_then(|name| function_param_pointer_address_spaces(ll, name))
        .unwrap_or_default();
    for r in refs_in(nodes.get(&in_ref)?) {
        let Some(node) = nodes.get(&r) else { continue };
        let Some(idx) = first_i32(node) else { continue };
        if let Some(name) = arg_type_name(node) {
            input_type_names.insert(idx, name);
        }
        if let Some(sref) = struct_info_ref(node) {
            if let Some(t) = parse_struct_info(nodes, sref, 0) {
                buffer_layouts.insert(idx, t);
            }
        }
        let strs = role_strings(node);
        if let Some(count) = declared_descriptor_count(node) {
            declared_descriptor_counts.insert(idx, count);
        }
        let Some(role_str) = primary_role(&strs) else {
            continue;
        };
        let texture_slot = variant_texture_slot(node, idx, nodes, &static_int_globals);
        let role_str = present_system_value_role(role_str, node, nodes, &static_int_globals);
        if let Some(declared) = unmodelled_declared_role(
            |role| {
                air_input_role_is_modelled(Stage::Fragment, role)
                    || queried_acceleration_structure_role(ll, role)
            },
            node,
            nodes,
            &static_int_globals,
        ) {
            unmodelled_input_params.push((idx, declared));
        }
        let role =
            match role_str {
                "position" => FragRole::Position,
                "point_coord" => FragRole::PointCoord,
                "front_facing" => FragRole::FrontFacing,
                "barycentric_coord" => FragRole::BarycentricCoord {
                    no_perspective: VaryingInterpolation::from_role_strings(&strs).no_perspective,
                },
                "primitive_id" => FragRole::PrimitiveId,
                "sample_id" => FragRole::SampleId,
                "sample_mask_in" => FragRole::SampleMaskIn,
                "viewport_array_index" => FragRole::ViewportArrayIndex,
                "render_target_array_index" => FragRole::RenderTargetArrayIndex,
                "amplification_id" => FragRole::AmplificationId,
                "amplification_count" => FragRole::AmplificationCount,
                "fragment_input" => {
                    let l = varying_loc;
                    varying_loc += 1;
                    if let Some(name) = arg_type_name(node) {
                        varying_types.insert(l, name);
                    }
                    if let Some(name) = arg_name(node) {
                        varying_names.insert(l, name);
                    }
                    if let Some(semantic) = string_after_marker(node, "air.fragment_input") {
                        varying_user_semantics.insert(l, semantic);
                    }
                    varying_interpolation.insert(l, VaryingInterpolation::from_role_strings(&strs));
                    FragRole::Varying(l)
                }
                "texture" => match texture_slot {
                    Some(slot) => {
                        if let Some(name) = arg_type_name(node) {
                            texture_type_names.insert(idx, name);
                        }
                        FragRole::Texture(slot)
                    }
                    None => FragRole::VariantAbsentTexture,
                },
                "sampler" => {
                    FragRole::Sampler(location_index_with_static(node, idx, &static_int_globals))
                }
                "visible_function_table" => FragRole::VisibleFunctionTable(
                    location_index_with_static(node, idx, &static_int_globals),
                ),
                "intersection_function_table" => FragRole::IntersectionFunctionTable(
                    location_index_with_static(node, idx, &static_int_globals),
                ),
                "buffer" | "indirect_buffer" => {
                    if role_str == "indirect_buffer" {
                        if let Some(sref) = struct_info_ref(node) {
                            indirect_buffer_struct_refs.push((
                                idx,
                                location_index_with_static(node, idx, &static_int_globals),
                                sref,
                            ));
                        }
                    }
                    if let Some(space) =
                        address_space(node).or_else(|| param_address_spaces.get(&idx).copied())
                    {
                        buffer_address_spaces.insert(idx, space);
                    }
                    if let Some(size) = i32_after_marker(node, "air.arg_type_size")
                        .or_else(|| i32_after_marker(node, "air.buffer_size"))
                    {
                        buffer_type_sizes.insert(idx, size);
                    }
                    if let Some(size) = i32_after_marker(node, "air.buffer_size") {
                        buffer_object_sizes.insert(idx, size);
                    }
                    if let Some(name) = arg_type_name(node) {
                        buffer_type_names.insert(idx, name);
                    }
                    if let Some(access) = declared_buffer_access(node) {
                        buffer_accesses.insert(idx, access);
                    }
                    FragRole::Buffer(location_index_with_static(node, idx, &static_int_globals))
                }
                "render_target" => {
                    let location = render_target_location(node, idx, &static_int_globals);
                    if let Some(name) = arg_type_name(node) {
                        color_input_type_names.insert(location, name);
                    }
                    FragRole::ColorInput(location)
                }
                "instance_acceleration_structure" | "primitive_acceleration_structure"
                    if queried_acceleration_structure_role(ll, role_str) =>
                {
                    FragRole::AccelerationStructureShadow(location_index_with_static(
                        node,
                        idx,
                        &static_int_globals,
                    ))
                }
                "imageblock_data" => FragRole::ImageblockData,
                _ if declares_disabled_texture(&strs) => FragRole::VariantAbsentTexture,
                other => stage_execution_group_role(Stage::Fragment, other).map_or(
                    FragRole::Other,
                    |(fact, lanes)| FragRole::ExecutionGroup { fact, lanes },
                ),
            };
        roles.push((idx, role));
    }
    let top_level_texture_locations = roles
        .iter()
        .filter_map(|(_, role)| match role {
            FragRole::Texture(location) => Some(*location),
            _ => None,
        })
        .collect::<Vec<_>>();
    let top_level_sampler_locations = roles
        .iter()
        .filter_map(|(_, role)| match role {
            FragRole::Sampler(location) => Some(*location),
            _ => None,
        })
        .collect::<Vec<_>>();
    let argument_buffers = ArgumentBuffers::new(indirect_buffer_struct_refs);
    Some(FragMeta {
        input_type_names,
        roles,
        unmodelled_stage_attributes,
        early_fragment_tests,
        unmodelled_input_params,
        implicit_imageblock_attachments: detect_implicit_imageblock_attachments(ll)?,
        varying_types,
        varying_names,
        varying_user_semantics,
        varying_interpolation,
        n_render_targets,
        render_target_members,
        render_target_type_names,
        render_target_dual_members,
        depth_members,
        depth_qualifier,
        stencil_members,
        sample_mask_members,
        unmodelled_output_members,
        fragment_imageblock,
        render_target_indices,
        buffer_layouts,
        buffer_address_spaces,
        buffer_type_sizes,
        buffer_object_sizes,
        buffer_type_names,
        buffer_accesses,
        texture_type_names,
        declared_descriptor_counts,
        color_input_type_names,
        embedded_textures: if body_uses_texture_intrinsic(ll) {
            detect_embedded_textures(nodes, &argument_buffers, &top_level_texture_locations)
        } else {
            Vec::new()
        },
        embedded_samplers: if body_uses_texture_intrinsic(ll) {
            detect_embedded_samplers(nodes, &argument_buffers, &top_level_sampler_locations)
        } else {
            Vec::new()
        },
        embedded_arguments: detect_embedded_arguments(nodes, &argument_buffers),
        unsurfaced_embedded_resources: unsurfaced_embedded_resources(nodes, &argument_buffers),
    })
}

pub fn parse_air_vertex_meta(ll: &str) -> Option<VertMeta> {
    parse_air_vertex_meta_with_entry(ll).0
}

pub(crate) fn parse_air_vertex_meta_with_entry(ll: &str) -> (Option<VertMeta>, Option<String>) {
    let nodes = collect_nodes(ll);
    let entry = entry_name_from_nodes(ll, "vertex", &nodes);
    let meta = parse_air_vertex_meta_with_nodes(ll, &nodes, entry.as_deref());
    (meta, entry)
}

fn parse_air_vertex_meta_with_nodes(
    ll: &str,
    nodes: &HashMap<u32, String>,
    entry: Option<&str>,
) -> Option<VertMeta> {
    let static_int_globals = static_init_int_global_values(ll);
    let root = stage_root(ll, "vertex")?;
    let rootc = nodes.get(&root)?;
    let refs = refs_in(rootc);
    let out_ref = *refs.first()?;
    let in_ref = *refs.get(1)?;
    let mut patch_node = None;
    let mut unmodelled_stage_attributes = vec![];
    for attribute in stage_root_attributes(rootc, nodes) {
        match attribute {
            StageAttribute::Patch(node) => patch_node = Some(node),
            other => unmodelled_stage_attributes.push(other.describe()),
        }
    }
    let mut undecoded_patch_shape = None;
    let patch_shape = patch_node.as_deref().and_then(|node| {
        let domain = if node.contains("!\"quad\"") {
            PatchDomain::Quad
        } else if node.contains("!\"triangle\"") {
            PatchDomain::Triangle
        } else if node.contains("!\"isoline\"") {
            PatchDomain::Isoline
        } else {
            undecoded_patch_shape = Some("air.patch names no tessellation domain".to_string());
            return None;
        };
        match i32_after_marker(node, "air.patch_control_point") {
            Some(count) => Some((domain, count)),
            None => {
                undecoded_patch_shape =
                    Some("air.patch states no air.patch_control_point count".to_string());
                None
            }
        }
    });

    let mut output_roles = vec![];
    let mut invariant_outputs = vec![];
    let mut unmodelled_output_members = vec![];
    let mut output_varying_types = HashMap::default();
    let mut output_varying_names = HashMap::default();
    let mut output_varying_user_semantics = HashMap::default();
    let mut out_loc = 0u32;
    for r in refs_in(nodes.get(&out_ref)?) {
        let Some(node) = nodes.get(&r) else { continue };
        let strs = role_strings(node);
        let Some(first) = gated_role(&strs, |role| vertex_output_kind(role).is_some()) else {
            continue;
        };
        let kind = vertex_output_kind(first).filter(|kind| {
            !kind.is_system_value() || metadata_enabled_by_default(node, nodes, &static_int_globals)
        });
        let role = match kind {
            Some(VertexOutputKind::Position) => VertOutRole::Position,
            Some(VertexOutputKind::PointSize) => VertOutRole::PointSize,
            Some(VertexOutputKind::ClipDistance) => VertOutRole::ClipDistance,
            Some(VertexOutputKind::ViewportArrayIndex) => VertOutRole::ViewportArrayIndex,
            Some(VertexOutputKind::RenderTargetArrayIndex) => VertOutRole::RenderTargetArrayIndex,
            Some(VertexOutputKind::Varying) => {
                let l = location_index_with_static(node, out_loc, &static_int_globals);
                out_loc += 1;
                if let Some(name) = arg_type_name(node) {
                    output_varying_types.insert(l, name);
                }
                if let Some(name) = arg_name(node) {
                    output_varying_names.insert(l, name);
                }
                if let Some(semantic) = string_after_marker(node, "air.vertex_output") {
                    output_varying_user_semantics.insert(l, semantic);
                }
                VertOutRole::Varying(l)
            }
            None if matches!(first, "function_constant" | "function_constant_disabled")
                || vertex_output_kind(first).is_some() =>
            {
                VertOutRole::FunctionConstantDisabled
            }
            None => VertOutRole::Other,
        };
        if strs.iter().any(|s| s == "invariant") {
            invariant_outputs.push(output_roles.len() as u32);
        }
        if matches!(role, VertOutRole::Other) {
            unmodelled_output_members.push((output_roles.len() as u32, first.to_string()));
        }
        output_roles.push(role);
    }

    let mut roles = vec![];
    let mut unmodelled_input_params: Vec<(u32, String)> = vec![];
    let mut parameter_type_names = HashMap::default();
    let mut vertex_input_types = HashMap::default();
    let mut vertex_input_names = HashMap::default();
    let mut patch_input_types = HashMap::default();
    let mut patch_input_names = HashMap::default();
    let mut buffer_layouts = HashMap::default();
    let mut buffer_address_spaces = HashMap::default();
    let mut buffer_type_sizes = HashMap::default();
    let mut buffer_object_sizes = HashMap::default();
    let mut buffer_type_names = HashMap::default();
    let mut buffer_accesses = HashMap::default();
    let mut texture_type_names = HashMap::default();
    let mut declared_descriptor_counts = HashMap::default();
    let mut indirect_buffer_struct_refs = Vec::new();
    let mut patch_control_point = None;
    let param_address_spaces = entry
        .and_then(|name| function_param_pointer_address_spaces(ll, name))
        .unwrap_or_default();
    let mut vin_loc = 0u32;
    for r in refs_in(nodes.get(&in_ref)?) {
        let Some(node) = nodes.get(&r) else { continue };
        let Some(idx) = first_i32(node) else { continue };
        if let Some(name) = arg_type_name(node) {
            parameter_type_names.insert(idx, name);
        }
        if let Some(sref) = struct_info_ref(node) {
            if let Some(t) = parse_struct_info(nodes, sref, 0) {
                buffer_layouts.insert(idx, t);
            }
        }
        let strs = role_strings(node);
        if let Some(count) = declared_descriptor_count(node) {
            declared_descriptor_counts.insert(idx, count);
        }
        let Some(mut first) = vertex_input_gated_role(&strs) else {
            continue;
        };
        let texture_slot = variant_texture_slot(node, idx, nodes, &static_int_globals);
        if primary_role(&strs) == Some("texture") && texture_slot.is_none() {
            first = VARIANT_ABSENT_TEXTURE_ROLE;
        }
        let first = present_gated_buffer_role(first, &strs, node, nodes, &static_int_globals);
        let first = present_system_value_role(first, node, nodes, &static_int_globals);
        if let Some(declared) = unmodelled_declared_role(
            |role| {
                air_input_role_is_modelled(Stage::Vertex, role)
                    || queried_acceleration_structure_role(ll, role)
            },
            node,
            nodes,
            &static_int_globals,
        ) {
            unmodelled_input_params.push((idx, declared));
        }
        let role =
            match first {
                "vertex_input" => {
                    let l = location_index_with_static(node, vin_loc, &static_int_globals);
                    vin_loc += 1;
                    if let Some(name) = arg_type_name(node) {
                        vertex_input_types.insert(l, name);
                    }
                    if let Some(name) = arg_name(node) {
                        vertex_input_names.insert(l, name);
                    }
                    VertRole::VertexInput(l)
                }
                "buffer" | "indirect_buffer" => {
                    if first == "indirect_buffer" {
                        if let Some(sref) = struct_info_ref(node) {
                            indirect_buffer_struct_refs.push((
                                idx,
                                location_index_with_static(node, idx, &static_int_globals),
                                sref,
                            ));
                        }
                    }
                    if let Some(space) =
                        address_space(node).or_else(|| param_address_spaces.get(&idx).copied())
                    {
                        buffer_address_spaces.insert(idx, space);
                    }
                    if let Some(size) = i32_after_marker(node, "air.arg_type_size")
                        .or_else(|| i32_after_marker(node, "air.buffer_size"))
                    {
                        buffer_type_sizes.insert(idx, size);
                    }
                    if let Some(size) = i32_after_marker(node, "air.buffer_size") {
                        buffer_object_sizes.insert(idx, size);
                    }
                    if let Some(name) = arg_type_name(node) {
                        buffer_type_names.insert(idx, name);
                    }
                    if let Some(access) = declared_buffer_access(node) {
                        buffer_accesses.insert(idx, access);
                    }
                    VertRole::Buffer(location_index_with_static(node, idx, &static_int_globals))
                }
                "texture" => {
                    if let Some(name) = arg_type_name(node) {
                        texture_type_names.insert(idx, name);
                    }
                    VertRole::Texture(texture_slot.unwrap_or_else(|| {
                        location_index_with_static(node, idx, &static_int_globals)
                    }))
                }
                "sampler" => {
                    VertRole::Sampler(location_index_with_static(node, idx, &static_int_globals))
                }
                "visible_function_table" => VertRole::VisibleFunctionTable(
                    location_index_with_static(node, idx, &static_int_globals),
                ),
                "intersection_function_table" => VertRole::IntersectionFunctionTable(
                    location_index_with_static(node, idx, &static_int_globals),
                ),
                "instance_acceleration_structure" | "primitive_acceleration_structure"
                    if queried_acceleration_structure_role(ll, first) =>
                {
                    VertRole::AccelerationStructureShadow(location_index_with_static(
                        node,
                        idx,
                        &static_int_globals,
                    ))
                }
                "vertex_id" => VertRole::VertexId,
                "instance_id" => VertRole::InstanceId,
                "base_vertex" => VertRole::BaseVertex,
                "base_instance" => VertRole::BaseInstance,
                "patch_control_point_input" => {
                    let refs = refs_in(node);
                    let function = refs
                        .first()
                        .and_then(|reference| nodes.get(reference))
                        .and_then(|body| pointer_symbol(body));
                    let fields = refs
                        .iter()
                        .skip(1)
                        .filter_map(|reference| nodes.get(reference))
                        .map(|field| PatchControlPointField {
                            location: location_index_with_static(field, 0, &static_int_globals),
                            type_name: arg_type_name(field),
                        })
                        .collect::<Vec<_>>();
                    if let Some(function) = function {
                        patch_control_point = Some((function, fields));
                    }
                    VertRole::PatchControlPoints
                }
                "patch_input" => {
                    let location = location_index_with_static(node, idx, &static_int_globals);
                    if let Some(name) = arg_type_name(node) {
                        patch_input_types.insert(location, name);
                    }
                    if let Some(name) = arg_name(node) {
                        patch_input_names.insert(location, name);
                    }
                    VertRole::PatchInput(location)
                }
                "position_in_patch" => VertRole::PositionInPatch,
                "patch_id" => VertRole::PatchId,
                "amplification_id" => VertRole::AmplificationId,
                "amplification_count" => VertRole::AmplificationCount,
                VARIANT_ABSENT_TEXTURE_ROLE => VertRole::VariantAbsentTexture,
                _ if declares_disabled_texture(&strs) => VertRole::VariantAbsentTexture,
                other => stage_execution_group_role(Stage::Vertex, other).map_or(
                    VertRole::Other,
                    |(fact, lanes)| VertRole::ExecutionGroup { fact, lanes },
                ),
            };
        roles.push((idx, role));
    }
    let top_level_texture_locations = roles
        .iter()
        .filter_map(|(_, role)| match role {
            VertRole::Texture(location) => Some(*location),
            _ => None,
        })
        .collect::<Vec<_>>();
    let top_level_sampler_locations = roles
        .iter()
        .filter_map(|(_, role)| match role {
            VertRole::Sampler(location) => Some(*location),
            _ => None,
        })
        .collect::<Vec<_>>();
    let argument_buffers = ArgumentBuffers::new(indirect_buffer_struct_refs);
    Some(VertMeta {
        roles,
        unmodelled_input_params,
        implicit_imageblock_attachments: detect_implicit_imageblock_attachments(ll)?,
        parameter_type_names,
        output_roles,
        invariant_outputs,
        unmodelled_output_members,
        output_varying_types,
        output_varying_names,
        output_varying_user_semantics,
        vertex_input_types,
        vertex_input_names,
        patch_input_types,
        patch_input_names,
        buffer_layouts,
        buffer_address_spaces,
        buffer_type_sizes,
        buffer_object_sizes,
        buffer_type_names,
        buffer_accesses,
        texture_type_names,
        declared_descriptor_counts,
        embedded_textures: if body_uses_texture_intrinsic(ll) {
            detect_embedded_textures(nodes, &argument_buffers, &top_level_texture_locations)
        } else {
            Vec::new()
        },
        embedded_samplers: if body_uses_texture_intrinsic(ll) {
            detect_embedded_samplers(nodes, &argument_buffers, &top_level_sampler_locations)
        } else {
            Vec::new()
        },
        embedded_arguments: detect_embedded_arguments(nodes, &argument_buffers),
        unsurfaced_embedded_resources: unsurfaced_embedded_resources(nodes, &argument_buffers),
        undecoded_patch_shape,
        unmodelled_stage_attributes,
        tessellation: patch_shape.map(|(domain, control_point_count)| {
            let (control_point_function, control_point_fields) = patch_control_point
                .map(|(function, fields)| (Some(function), fields))
                .unwrap_or_default();
            TessellationMeta {
                domain,
                control_point_count,
                control_point_function,
                control_point_fields,
            }
        }),
    })
}

#[cfg(test)]
mod tests;
