#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
kernel void k_rti(instance_acceleration_structure as [[buffer(0)]], device float *o [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  intersector<triangle_data, instancing, max_levels<2>> isect;
  isect.assume_geometry_type(geometry_type::triangle);
  isect.force_opacity(forced_opacity::opaque);
  isect.accept_any_intersection(false);
  auto r = isect.intersect(ray(float3(0), float3(0, 0, 1), 0, 100), as, 0xff);
  o[i] = r.distance + (float)r.instance_id[0] + (float)r.primitive_id + r.triangle_barycentric_coord.x; }
