#include <metal_stdlib>
using namespace metal;
kernel void k_coop(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  o[i] = clz(o[i]) + popcount(o[i]) + reverse_bits(o[i]) + extract_bits(o[i], 1, 4) + insert_bits(o[i], 3u, 2, 3); }
