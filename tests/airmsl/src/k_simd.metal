#include <metal_stdlib>
using namespace metal;
kernel void k_simd(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]], uint lane [[thread_index_in_simdgroup]]) {
  float v = o[i];
  v = simd_sum(v) + simd_max(v) + simd_prefix_inclusive_sum(v) + simd_shuffle_down(v, 1) + simd_broadcast_first(v);
  v += (float)(simd_all(lane < 40) ? 1 : 0) + (float)(simd_any(lane & 1) ? 1 : 0);
  o[i] = v; }
