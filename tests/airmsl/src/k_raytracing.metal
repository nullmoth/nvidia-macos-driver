#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
kernel void k_rt(instance_acceleration_structure as [[buffer(0)]], device float *o [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  ray r(float3(0,0,0), float3(0,0,1), 0.0, 100.0);
  intersector<triangle_data, instancing> isect;
  auto res = isect.intersect(r, as);
  o[i] = res.type == intersection_type::triangle ? res.distance : -1.0; }
