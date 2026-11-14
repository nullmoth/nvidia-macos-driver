#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
kernel void k_curve(primitive_acceleration_structure as [[buffer(0)]], device float *o [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  intersector<curve_data> isect;
  auto r = isect.intersect(ray(float3(0), float3(0, 0, 1), 0, 100), as);
  o[i] = r.type == intersection_type::curve ? r.distance : -1.0f; }
