#include <metal_stdlib>
using namespace metal;
kernel void k_s4(device float *o [[buffer(0)]], device uint *u [[buffer(1)]], uint i [[thread_position_in_grid]], uint lane [[thread_index_in_simdgroup]]) {
  float v = o[i];
  v += simd_shuffle_and_fill_down(v, 1.0f, 1) + simd_shuffle_and_fill_up(v, 2.0f, 1) + simd_shuffle_rotate_down(v, 1) + simd_shuffle_rotate_up(v, 1);
  v += simd_shuffle_xor(v, 2) + simd_prefix_exclusive_product(v) + simd_product(v) + simd_min(v);
  uint w = u[i];
  w += simd_and(w) + simd_or(w) + simd_xor(w) + (uint)simd_is_first() + popcount((uint)(ulong)simd_ballot(lane < 7) ) + (simd_all(w > 1) ? 1u : 0u);
  v += quad_shuffle_xor(v, 1) + quad_shuffle_up(v, 1) + quad_shuffle_rotate_up(v, 1) + quad_shuffle_rotate_down(v, 1) + quad_product(v) + (quad_all(v > 0) ? 1 : 0);
  o[i] = v; u[i] = w; }
