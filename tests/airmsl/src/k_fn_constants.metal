#include <metal_stdlib>
using namespace metal;
constant bool useA [[function_constant(0)]];
constant int n [[function_constant(1)]];
kernel void k_fc(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  float v = o[i]; if (useA) v += 1; for (int j = 0; j < n; ++j) v *= 2; o[i] = v; }
