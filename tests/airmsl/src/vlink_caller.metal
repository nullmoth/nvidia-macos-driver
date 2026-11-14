#include <metal_stdlib>
using namespace metal;
[[visible]] float addone(float x);
kernel void k_vcaller(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { o[i] = addone(o[i]) * 2.0f; }
