#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
[[intersection(triangle, triangle_data, instancing)]]
bool tri_test(float2 bary [[barycentric_coord]], uint prim [[primitive_id]], device float *buf [[buffer(2)]]) { return (prim & 1) == 0 && bary.x > 0.0f; }
kernel void k_ift(instance_acceleration_structure as [[buffer(0)]], intersection_function_table<triangle_data, instancing> ift [[buffer(1)]], device float *o [[buffer(3)]], uint i [[thread_position_in_grid]]) {
  intersector<triangle_data, instancing> isect;
  auto r = isect.intersect(ray(float3(0), float3(0, 0, 1), 0, 100), as, ift);
  o[i] = r.type == intersection_type::triangle ? r.distance : -1.0f; }
