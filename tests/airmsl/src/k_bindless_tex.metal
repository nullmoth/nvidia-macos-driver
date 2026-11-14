#include <metal_stdlib>
using namespace metal;
kernel void k_bt(array<texture2d<float>, 8> heap [[texture(0)]], array<sampler, 2> smp [[sampler(0)]], device float *out [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  out[i] = heap[i & 7].sample(smp[i & 1], float2(0.5)).x + heap[(i + 1) & 7].read(uint2(0)).y; }
