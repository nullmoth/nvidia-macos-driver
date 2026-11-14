#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
[[intersection(triangle, triangle_data, instancing, intersection_function_buffer)]]
bool tri_ifb(float2 bary [[barycentric_coord]], uint prim [[primitive_id]]) { return (prim & 1) == 0 && bary.x > 0.1f; }
kernel void k_ifb(instance_acceleration_structure as [[buffer(0)]], device float *o [[buffer(3)]], const device void *ifb [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  intersector<triangle_data, instancing, intersection_function_buffer> isect;
  intersection_function_buffer_arguments args;
  args.intersection_function_buffer = ifb; args.intersection_function_buffer_size = 4096; args.intersection_function_stride = 64;
  auto r = isect.intersect(ray(float3(0), float3(0, 0, 1), 0, 100), as, args);
  o[i] = r.type == intersection_type::triangle ? r.distance : -1.0f; }
