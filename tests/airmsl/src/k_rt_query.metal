#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
kernel void k_rq(primitive_acceleration_structure as [[buffer(0)]], device float *o [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  intersection_query<triangle_data> q;
  q.reset(ray(float3(0), float3(0,0,1), 0, 10), as);
  while (q.next()) { q.commit_triangle_intersection(); }
  o[i] = q.get_committed_distance(); }
