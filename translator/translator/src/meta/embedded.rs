use super::types::{member_tuples, tokenize};
use super::{arg_type_name, role_strings, texture_shape_from_name, BufferAccess, TextureFormat};
use crate::fxhash::HashMap;
use crate::passes::ImageComp;
use spirv::Dim;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ArgumentBuffers(Vec<(u32, u32, u32)>);

impl ArgumentBuffers {
    pub(super) fn new(mut declared: Vec<(u32, u32, u32)>) -> Self {
        declared.sort_by_key(|(param_index, _, _)| *param_index);
        Self(declared)
    }

    fn iter(&self) -> impl Iterator<Item = (u32, u32, u32)> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedTexture {
    pub buffer_param_index: u32,
    pub buffer_index: u32,
    pub field_offset: u32,
    pub field_ordinal: u32,
    pub argument_index: u32,
    pub dim: Dim,
    pub arrayed: bool,
    pub comp: ImageComp,
    pub storage_format: Option<TextureFormat>,
    pub array_length: Option<u32>,
    pub synthetic_texture_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedArgument {
    pub buffer_param_index: u32,
    pub buffer_index: u32,
    pub field_ordinal: u32,
    pub field_offset: u32,
    pub argument_index: u32,
    pub resource_buffer_index: Option<u32>,
    pub resource_address_space: Option<u32>,
    pub resource_declared_size: Option<u32>,
    pub resource_access: Option<BufferAccess>,
}

pub fn embedded_synthetic_texture_index(texture_locations: &[u32]) -> u32 {
    texture_locations.iter().copied().max().map_or(0, |m| m + 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedSampler {
    pub buffer_param_index: u32,
    pub buffer_index: u32,
    pub field_offset: u32,
    pub field_ordinal: u32,
    pub argument_index: u32,
    pub synthetic_sampler_index: u32,
}

pub fn embedded_synthetic_sampler_index(sampler_locations: &[u32]) -> u32 {
    sampler_locations.iter().copied().max().map_or(0, |m| m + 1)
}

fn expressible_embedded_sampler(member: &ArgumentMember, node: &str) -> bool {
    if member.in_array_wrapper {
        return false;
    }
    if member.array_len != 0 {
        return false;
    }
    if let Some(name) = arg_type_name(node) {
        if name != "sampler" {
            return false;
        }
    }
    role_strings(node).iter().any(|s| s == "sampler")
}

pub(super) fn detect_embedded_samplers(
    nodes: &HashMap<u32, String>,
    argument_buffers: &ArgumentBuffers,
    top_level_sampler_locations: &[u32],
) -> Vec<EmbeddedSampler> {
    let mut out = vec![];
    let mut next_s = embedded_synthetic_sampler_index(top_level_sampler_locations);
    for (buffer_param_index, buffer_index, sref) in argument_buffers.iter() {
        for member in embedded_argument_members(nodes, sref) {
            let Some(node) = nodes.get(&member.node_ref) else {
                continue;
            };
            if !expressible_embedded_sampler(&member, node) {
                continue;
            }
            out.push(EmbeddedSampler {
                buffer_param_index,
                buffer_index,
                field_offset: member.field_offset,
                field_ordinal: member.field_ordinal,
                argument_index: member.argument_index(node),
                synthetic_sampler_index: next_s,
            });
            next_s += 1;
        }
    }
    out
}

pub(super) fn body_uses_texture_intrinsic(ll: &str) -> bool {
    ll.contains("@air.sample_")
        || ll.contains("@air.gather_")
        || ll.contains("@air.read_texture")
        || ll.contains("@air.write_texture")
        || ll.contains("@air.write_imageblock_slice_to_texture")
}

pub(super) fn detect_embedded_textures(
    nodes: &HashMap<u32, String>,
    argument_buffers: &ArgumentBuffers,
    top_level_texture_locations: &[u32],
) -> Vec<EmbeddedTexture> {
    let mut out = vec![];
    let mut next_k = embedded_synthetic_texture_index(top_level_texture_locations);
    for (buffer_param_index, buffer_index, sref) in argument_buffers.iter() {
        for member in embedded_argument_members(nodes, sref) {
            let Some(tex_node) = nodes.get(&member.node_ref) else {
                continue;
            };
            let Some(shape) = expressible_embedded_texture(&member, tex_node) else {
                continue;
            };
            out.push(EmbeddedTexture {
                buffer_param_index,
                buffer_index,
                field_offset: member.field_offset,
                field_ordinal: member.field_ordinal,
                argument_index: member.argument_index(tex_node),
                dim: shape.dimension.to_spirv_dim(),
                arrayed: shape.arrayed,
                comp: shape.component.to_image_comp(),
                storage_format: shape.storage_format,
                array_length: shape.array_length,
                synthetic_texture_index: next_k,
            });
            next_k += 1;
        }
    }
    out
}

fn expressible_embedded_texture(
    member: &ArgumentMember,
    node: &str,
) -> Option<super::TextureShape> {
    if member.in_array_wrapper {
        return None;
    }
    let strs = role_strings(node);
    if !strs.iter().any(|s| s == "texture")
        || !strs
            .iter()
            .any(|s| matches!(s.as_str(), "sample" | "read" | "write" | "read_write"))
    {
        return None;
    }
    let mut shape = texture_shape_from_name(&arg_type_name(node).unwrap_or_default());
    if shape.array_length.is_none() && member.array_len != 0 {
        shape.array_ref = true;
        shape.array_length = Some(member.array_len);
    }
    Some(shape)
}

pub(super) fn detect_embedded_arguments(
    nodes: &HashMap<u32, String>,
    argument_buffers: &ArgumentBuffers,
) -> Vec<EmbeddedArgument> {
    argument_buffers
        .iter()
        .flat_map(|(buffer_param_index, buffer_index, sref)| {
            embedded_argument_members(nodes, sref)
                .into_iter()
                .filter(|member| member.depth == 0)
                .flat_map(move |member| {
                    let node = nodes.get(&member.node_ref);
                    let is_buffer = node
                        .is_some_and(|node| role_strings(node).iter().any(|role| role == "buffer"));
                    let elements = match node {
                        Some(node) if expressible_embedded_texture(&member, node).is_some() => 1,
                        Some(node) if member.repeats_a_handle(node) => member.array_len,
                        // a metal::array<sampler, N> member: the struct type info gives no C array length, the
                        // field's own location_index pair carries N (Blender: array<sampler, 17>)
                        Some(node) if member.array_len == 0 && member.repeats_a_handle_by_location(node).is_some() => {
                            member.repeats_a_handle_by_location(node).unwrap_or(1)
                        }
                        _ => 1,
                    };
                    (0..elements).filter_map(move |element| {
                        let node = node?;
                        let argument_index = member.argument_index(node).checked_add(element)?;
                        Some(EmbeddedArgument {
                            buffer_param_index,
                            buffer_index,
                            field_ordinal: member.field_ordinal,
                            field_offset: member
                                .field_offset
                                .checked_add(element.checked_mul(HANDLE_BYTES)?)?,
                            argument_index,
                            resource_buffer_index: is_buffer.then_some(argument_index),
                            resource_address_space: is_buffer
                                .then(|| super::address_space(node))
                                .flatten(),
                            resource_declared_size: is_buffer
                                .then(|| super::i32_after_marker(node, "air.arg_type_size"))
                                .flatten(),
                            resource_access: is_buffer
                                .then(|| super::declared_buffer_access(node))
                                .flatten(),
                        })
                    })
                })
        })
        .collect()
}

const EMBEDDED_RESOURCE_ROLES: &[&str] = &[
    "buffer",
    "command_buffer",
    "compute_pipeline_state",
    "indirect_buffer",
    "intersection_function_table",
    "primitive_acceleration_structure",
    "render_pipeline_state",
    "sampler",
    "texture",
    "visible_function_table",
];

pub(super) fn unsurfaced_embedded_resources(
    nodes: &HashMap<u32, String>,
    argument_buffers: &ArgumentBuffers,
) -> Vec<String> {
    let mut out = vec![];
    for (buffer_param_index, _, sref) in argument_buffers.iter() {
        for member in embedded_argument_members(nodes, sref) {
            let Some(node) = nodes.get(&member.node_ref) else {
                continue;
            };
            if expressible_embedded_texture(&member, node).is_some() {
                continue;
            }
            if expressible_embedded_sampler(&member, node) {
                continue;
            }
            let Some(role) = role_strings(node)
                .into_iter()
                .find(|role| EMBEDDED_RESOURCE_ROLES.contains(&role.as_str()))
            else {
                continue;
            };
            let offset = member.field_offset;
            match (member.depth, role.as_str()) {
                (0, "texture") => out.push(format!(
                    "argument buffer parameter {buffer_param_index} holds an air.texture at byte \
                     offset {offset} the embedded lowering cannot express ({})",
                    arg_type_name(node).unwrap_or_else(|| "unnamed".to_string())
                )),
                (0, "sampler") => out.push(format!(
                    "argument buffer parameter {buffer_param_index} holds an air.sampler at byte \
                     offset {offset} the embedded lowering cannot express (a sampler array)"
                )),
                (0, _) => {}
                (_, _) => out.push(format!(
                    "argument buffer parameter {buffer_param_index} holds an air.{role} at byte \
                     offset {offset} inside a nested struct member"
                )),
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArgumentMember {
    field_ordinal: u32,
    field_offset: u32,
    node_ref: u32,
    argument_base: u32,
    depth: u32,
    in_array_wrapper: bool,
    array_len: u32,
}

impl ArgumentMember {
    fn argument_index(&self, node: &str) -> u32 {
        self.argument_base
            .saturating_add(super::location_index(node, self.field_ordinal))
    }

    fn repeats_a_handle_by_location(&self, node: &str) -> Option<u32> {
        // samplers only for now: texture and buffer arrays in argument buffers keep the layout they shipped with
        let count = super::declared_descriptor_count(node)?;
        role_strings(node).iter().any(|role| role == "sampler").then_some(count)
    }

    fn repeats_a_handle(&self, node: &str) -> bool {
        self.array_len > 1
            && role_strings(node)
                .iter()
                .any(|role| EMBEDDED_RESOURCE_ROLES.contains(&role.as_str()))
    }
}

const HANDLE_BYTES: u32 = 8;

const MAX_ARGUMENT_NESTING: u32 = 8;

fn embedded_argument_members(nodes: &HashMap<u32, String>, sref: u32) -> Vec<ArgumentMember> {
    let mut out = vec![];
    collect_argument_members(nodes, sref, Nesting::default(), &mut out);
    out
}

#[derive(Clone, Copy, Debug, Default)]
struct Nesting {
    field_ordinal: u32,
    field_offset: u32,
    argument_base: u32,
    depth: u32,
    in_array_wrapper: bool,
}

fn collect_argument_members(
    nodes: &HashMap<u32, String>,
    sref: u32,
    enclosing: Nesting,
    out: &mut Vec<ArgumentMember>,
) {
    if enclosing.depth > MAX_ARGUMENT_NESTING {
        return;
    }
    let Some(body) = nodes.get(&sref) else {
        return;
    };
    for (field_ordinal, member) in member_tuples(&tokenize(body)).into_iter().enumerate() {
        let outer_ordinal = if enclosing.depth == 0 {
            field_ordinal as u32
        } else {
            enclosing.field_ordinal
        };
        match (member.nested, member.argument_wrapper_id) {
            (Some(wrapper), Some(id)) => collect_argument_members(
                nodes,
                wrapper,
                Nesting {
                    field_ordinal: outer_ordinal,
                    field_offset: enclosing.field_offset + member.offset,
                    argument_base: enclosing.argument_base + id,
                    depth: enclosing.depth + 1,
                    in_array_wrapper: enclosing.in_array_wrapper || member.array_len != 0,
                },
                out,
            ),
            _ => {
                if let Some(node_ref) = member.argument_node {
                    out.push(ArgumentMember {
                        field_ordinal: outer_ordinal,
                        field_offset: enclosing.field_offset + member.offset,
                        node_ref,
                        argument_base: enclosing.argument_base,
                        depth: enclosing.depth,
                        in_array_wrapper: enclosing.in_array_wrapper,
                        array_len: member.array_len,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::TextureFormat;

    #[test]
    fn embedded_texture_use_gate_includes_sampling_and_gathering() {
        assert!(body_uses_texture_intrinsic(
            "call <4 x float> @air.sample_texture_2d(...)"
        ));
        assert!(body_uses_texture_intrinsic(
            "call <4 x float> @air.gather_texture_2d(...)"
        ));
        assert!(!body_uses_texture_intrinsic(
            "declare void @unrelated_texture_helper()"
        ));
    }

    #[test]
    fn embedded_texture_detection_includes_read_and_write_fields() {
        let mut nodes = HashMap::default();
        nodes.insert(
            10,
            r#"i32 0, i32 8, i32 0, !"texture2d<float, read>", !"input", !"air.indirect_argument", !20, i32 8, i32 8, i32 0, !"texture2d<float, write>", !"output", !"air.indirect_argument", !21"#.to_string(),
        );
        nodes.insert(
            20,
            r#"i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.read", !"air.arg_type_name", !"texture2d<float, read>""#.to_string(),
        );
        nodes.insert(
            21,
            r#"i32 1, !"air.texture", !"air.location_index", i32 1, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>""#.to_string(),
        );

        let textures =
            detect_embedded_textures(&nodes, &ArgumentBuffers::new(vec![(4, 0, 10)]), &[]);
        assert_eq!(textures.len(), 2);
        assert_eq!(textures[0].buffer_param_index, 4);
        assert_eq!(textures[0].field_offset, 0);
        assert_eq!(textures[0].argument_index, 0);
        assert_eq!(textures[0].storage_format, None);
        assert!(!textures[0].arrayed);
        assert_eq!(textures[0].synthetic_texture_index, 0);
        assert_eq!(textures[1].field_offset, 8);
        assert_eq!(textures[1].argument_index, 1);
        assert_eq!(textures[1].storage_format, Some(TextureFormat::R32f));
        assert_eq!(textures[1].array_length, None);
        assert!(!textures[1].arrayed);
        assert_eq!(textures[1].synthetic_texture_index, 1);
    }

    #[test]
    fn embedded_fixed_depth_array_preserves_handle_count_and_image_shape() {
        let mut nodes = HashMap::default();
        nodes.insert(
            10,
            r#"i32 0, i32 16, i32 0, !"array<depth2d_array<float, sample>, 2>", !"depth", !"air.indirect_argument", !20"#.to_string(),
        );
        nodes.insert(
            20,
            r#"i32 0, !"air.texture", !"air.location_index", i32 4, i32 2, !"air.sample", !"air.arg_type_name", !"array<depth2d_array<float, sample>, 2>""#.to_string(),
        );

        let textures =
            detect_embedded_textures(&nodes, &ArgumentBuffers::new(vec![(1, 16, 10)]), &[]);
        assert_eq!(textures.len(), 1);
        assert_eq!(textures[0].argument_index, 4);
        assert_eq!(textures[0].array_length, Some(2));
        assert!(textures[0].arrayed);
        assert_eq!(textures[0].storage_format, None);
    }

    #[test]
    fn embedded_c_array_member_takes_its_length_from_the_member_tuple() {
        let mut nodes = HashMap::default();
        nodes.insert(
            10,
            r#"i32 0, i32 8, i32 32, !"texture2d<float, sample>", !"slices", !"air.indirect_argument", !20"#.to_string(),
        );
        nodes.insert(
            20,
            r#"i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"slices""#.to_string(),
        );

        let textures =
            detect_embedded_textures(&nodes, &ArgumentBuffers::new(vec![(5, 1, 10)]), &[]);
        assert_eq!(textures.len(), 1);
        assert_eq!(textures[0].array_length, Some(32));
        assert_eq!(textures[0].argument_index, 0);
        assert!(!textures[0].arrayed);
        assert!(
            unsurfaced_embedded_resources(&nodes, &ArgumentBuffers::new(vec![(5, 1, 10)]))
                .is_empty()
        );
    }

    #[test]
    fn a_c_array_of_buffer_handles_surfaces_one_resource_per_element() {
        let mut nodes = HashMap::default();
        nodes.insert(
            10,
            r#"i32 0, i32 8, i32 2, !"half4", !"bufs", !"air.indirect_argument", !20, i32 16, i32 4, i32 4, !"uint", !"counts", !"air.indirect_argument", !21"#.to_string(),
        );
        nodes.insert(
            20,
            r#"i32 0, !"air.buffer", !"air.location_index", i32 12, i32 1, !"air.read_write", !"air.address_space", i32 1, !"air.arg_type_size", i32 8, !"air.arg_type_name", !"half4", !"air.arg_name", !"bufs""#.to_string(),
        );
        nodes.insert(
            21,
            r#"i32 1, !"air.indirect_constant", !"air.location_index", i32 14, i32 1, !"air.arg_type_name", !"uint", !"air.arg_name", !"counts""#.to_string(),
        );

        let arguments = detect_embedded_arguments(&nodes, &ArgumentBuffers::new(vec![(0, 3, 10)]));
        let coordinates = arguments
            .iter()
            .map(|argument| {
                (
                    argument.field_offset,
                    argument.argument_index,
                    argument.resource_buffer_index,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            coordinates,
            vec![(0, 12, Some(12)), (8, 13, Some(13)), (16, 14, None)]
        );
    }

    #[test]
    fn embedded_argument_detection_classifies_only_nested_buffers_as_device_resources() {
        let mut nodes = HashMap::default();
        nodes.insert(
            10,
            r#"i32 0, i32 4, i32 0, !"uint", !"count", !"air.indirect_argument", !20, i32 8, i32 8, i32 0, !"float", !"values", !"air.indirect_argument", !21"#.to_string(),
        );
        nodes.insert(
            20,
            r#"i32 0, !"air.indirect_constant", !"air.location_index", i32 2, i32 1"#.to_string(),
        );
        nodes.insert(
            21,
            r#"i32 1, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.address_space", i32 1"#.to_string(),
        );

        let arguments = detect_embedded_arguments(&nodes, &ArgumentBuffers::new(vec![(3, 5, 10)]));
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].resource_buffer_index, None);
        assert_eq!(arguments[1].buffer_param_index, 3);
        assert_eq!(arguments[1].buffer_index, 5);
        assert_eq!(arguments[1].field_offset, 8);
        assert_eq!(arguments[1].argument_index, 7);
        assert_eq!(arguments[1].resource_buffer_index, Some(7));
        assert_eq!(arguments[1].resource_address_space, Some(1));
    }

    #[test]
    fn a_wrapped_texture_binds_at_the_summed_argument_id() {
        let nodes = crate::fxhash::map_from([
            (
                10_u32,
                r#"!"air.struct_type_info", !11, i32 64, i32 8, i32 0, !"deep", !"d", !"air.indirect_argument", i32 30, i32 72, i32 8, i32 0, !"texture2d<float, sample>", !"flat", !"air.indirect_argument", !14"#.to_string(),
            ),
            (
                11_u32,
                r#"!"air.struct_type_info", !12, i32 0, i32 8, i32 0, !"inner", !"mid", !"air.indirect_argument", i32 0"#.to_string(),
            ),
            (
                12_u32,
                r#"i32 8, i32 8, i32 0, !"texture2d<float, sample>", !"y", !"air.indirect_argument", !13"#.to_string(),
            ),
            (
                13_u32,
                r#"i32 0, !"air.texture", !"air.location_index", i32 2, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>""#.to_string(),
            ),
            (
                14_u32,
                r#"i32 1, !"air.texture", !"air.location_index", i32 40, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>""#.to_string(),
            ),
        ]);
        let textures =
            detect_embedded_textures(&nodes, &ArgumentBuffers::new(vec![(0, 0, 10)]), &[]);
        assert_eq!(
            textures
                .iter()
                .map(|t| (t.field_ordinal, t.field_offset, t.argument_index))
                .collect::<Vec<_>>(),
            vec![(0, 72, 32), (1, 72, 40)],
        );
    }

    #[test]
    fn an_arrayed_wrapper_is_refused_rather_than_bound_at_element_zero() {
        let nodes = crate::fxhash::map_from([
            (
                10_u32,
                r#"!"air.struct_type_info", !11, i32 0, i32 48, i32 9, !"MaterialProperty", !"slot", !"air.indirect_argument", i32 0"#.to_string(),
            ),
            (
                11_u32,
                r#"i32 0, i32 8, i32 0, !"texture2d<float, sample>", !"tex", !"air.indirect_argument", !12"#.to_string(),
            ),
            (
                12_u32,
                r#"i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>""#.to_string(),
            ),
        ]);
        let buffers = ArgumentBuffers::new(vec![(0, 0, 10)]);
        assert_eq!(detect_embedded_textures(&nodes, &buffers, &[]), vec![]);
        assert_eq!(
            unsurfaced_embedded_resources(&nodes, &buffers),
            vec![
                "argument buffer parameter 0 holds an air.texture at byte offset 0 inside a nested \
                 struct member"
                    .to_string()
            ],
        );
    }

    #[test]
    fn a_wrapped_buffer_is_named_unsurfaced_and_is_not_an_embedded_argument() {
        let nodes = crate::fxhash::map_from([
            (
                10_u32,
                r#"!"air.struct_type_info", !11, i32 16, i32 8, i32 0, !"wrapper", !"w", !"air.indirect_argument", i32 4, i32 24, i32 8, i32 0, !"float", !"values", !"air.indirect_argument", !13"#.to_string(),
            ),
            (
                11_u32,
                r#"i32 0, i32 8, i32 0, !"float", !"inner", !"air.indirect_argument", !12"#.to_string(),
            ),
            (
                12_u32,
                r#"i32 0, !"air.buffer", !"air.location_index", i32 1, i32 1, !"air.address_space", i32 1"#.to_string(),
            ),
            (
                13_u32,
                r#"i32 1, !"air.buffer", !"air.location_index", i32 7, i32 1, !"air.address_space", i32 1"#.to_string(),
            ),
        ]);
        let buffers = ArgumentBuffers::new(vec![(0, 0, 10)]);
        assert_eq!(
            detect_embedded_arguments(&nodes, &buffers)
                .iter()
                .map(|argument| (argument.field_offset, argument.resource_buffer_index))
                .collect::<Vec<_>>(),
            vec![(24, Some(7))],
        );
        assert_eq!(
            unsurfaced_embedded_resources(&nodes, &buffers),
            vec![
                "argument buffer parameter 0 holds an air.buffer at byte offset 16 inside a nested \
                 struct member"
                    .to_string()
            ],
        );
    }

    #[test]
    fn an_inline_nested_struct_does_not_shift_its_siblings() {
        let nodes = crate::fxhash::map_from([
            (
                10_u32,
                r#"i32 0, i32 4, i32 0, !"uint", !"count", !"air.indirect_argument", !11, !"air.struct_type_info", !12, i32 8, i32 16, i32 0, !"Params", !"params", i32 24, i32 8, i32 0, !"texture2d<float, sample>", !"tex", !"air.indirect_argument", !13"#.to_string(),
            ),
            (
                11_u32,
                r#"i32 0, !"air.indirect_constant", !"air.location_index", i32 0, i32 1"#.to_string(),
            ),
            (
                12_u32,
                r#"i32 0, i32 16, i32 0, !"float4", !"a""#.to_string(),
            ),
            (
                13_u32,
                r#"i32 2, !"air.texture", !"air.location_index", i32 5, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>""#.to_string(),
            ),
        ]);
        let buffers = ArgumentBuffers::new(vec![(0, 0, 10)]);
        assert!(unsurfaced_embedded_resources(&nodes, &buffers).is_empty());
        let textures = detect_embedded_textures(&nodes, &buffers, &[]);
        assert_eq!(textures.len(), 1);
        assert_eq!(textures[0].field_ordinal, 2);
        assert_eq!(textures[0].field_offset, 24);
        assert_eq!(textures[0].argument_index, 5);
    }

    #[test]
    fn the_synthetic_texture_index_does_not_depend_on_the_argument_list_order() {
        let nodes = crate::fxhash::map_from([
            (
                10_u32,
                r#"i32 0, i32 8, i32 0, !"texture2d<float, sample>", !"first", !"air.indirect_argument", !12"#
                    .to_string(),
            ),
            (
                11_u32,
                r#"i32 0, i32 8, i32 0, !"texture2d<float, write>", !"second", !"air.indirect_argument", !13"#
                    .to_string(),
            ),
            (
                12_u32,
                r#"i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.sample", !"air.arg_type_name", !"texture2d<float, sample>", !"air.arg_name", !"first""#
                    .to_string(),
            ),
            (
                13_u32,
                r#"i32 0, !"air.texture", !"air.location_index", i32 0, i32 1, !"air.write", !"air.arg_type_name", !"texture2d<float, write>", !"air.arg_name", !"second""#
                    .to_string(),
            ),
        ]);
        let declared = vec![(0_u32, 7_u32, 10_u32), (1, 2, 11)];
        let forward =
            detect_embedded_textures(&nodes, &ArgumentBuffers::new(declared.clone()), &[]);
        let reversed = detect_embedded_textures(
            &nodes,
            &ArgumentBuffers::new(declared.into_iter().rev().collect()),
            &[],
        );
        assert_eq!(forward, reversed);
        assert_eq!(
            forward
                .iter()
                .map(|texture| (texture.buffer_param_index, texture.synthetic_texture_index))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1)],
        );
    }
}
