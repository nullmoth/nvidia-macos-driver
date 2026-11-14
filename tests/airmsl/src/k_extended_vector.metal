#include <metal_stdlib>
using namespace metal;
kernel void k_ev(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  vec<float, 8> a = vec<float, 8>(o[i]);
  a = a * a + a; o[i] = a[0] + a[7]; }
