use super::*;

mod bindings;
mod buffer_addresses;
mod collapse;
mod discovery;
mod query_select;
pub(in crate::passes) mod rewrites;
mod sampler_array;
mod selected_load;
mod texture_array;

pub(in crate::passes) use bindings::*;
pub(in crate::passes) use buffer_addresses::*;
pub(in crate::passes) use collapse::*;
pub(in crate::passes) use discovery::*;
pub(in crate::passes) use query_select::rewrite_resource_query_selects;
pub(in crate::passes) use sampler_array::materialize_sampler_array_loads;
pub(in crate::passes) use selected_load::materialize_interface_selected_loads;
pub(in crate::passes) use texture_array::{
    materialize_texture_array_loads, retire_dead_pointer_projections,
    route_texture_array_elements_through_heap, sink_loop_header_texture_array_loads,
};
