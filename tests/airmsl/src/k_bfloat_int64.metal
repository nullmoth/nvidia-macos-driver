#include <metal_stdlib>
using namespace metal;
kernel void k_bf(device bfloat *o [[buffer(0)]], device long *l [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  o[i] = o[i] * bfloat(2.0f); l[i] = l[i] * 3 + 1; }
