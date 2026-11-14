#include <metal_stdlib>
using namespace metal;
kernel void k_quad(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  float v = o[i]; v = quad_sum(v) + quad_shuffle(v, 1) + quad_broadcast(v, 2); o[i] = v; }
