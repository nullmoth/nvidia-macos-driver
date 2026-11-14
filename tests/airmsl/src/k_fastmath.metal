#include <metal_stdlib>
using namespace metal;
kernel void k_fm(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  float x = o[i]; o[i] = fma(x, x, precise::sin(x)) + fast::exp(x) + rsqrt(x) + fract(x) + sign(x) + smoothstep(0.f, 1.f, x) + ldexp(x, 2) + trunc(x); }
