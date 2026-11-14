#include <metal_stdlib>
#include <metal_stdlib>
using namespace metal;
[[visible]] float addone(float x) { return x + 1.0; }
[[visible]] float twice(float x) { return x * 2.0; }
kernel void k_fp(device float *o [[buffer(0)]], visible_function_table<float(float)> tbl [[buffer(1)]], uint i [[thread_position_in_grid]]) {
  o[i] = tbl[i & 1](o[i]); }
