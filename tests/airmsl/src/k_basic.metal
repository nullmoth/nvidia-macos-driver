#include <metal_stdlib>
using namespace metal;
kernel void k_add(device float *a [[buffer(0)]], device const float *b [[buffer(1)]], uint i [[thread_position_in_grid]]) { a[i] += b[i]; }
