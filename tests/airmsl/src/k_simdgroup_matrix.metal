#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;
kernel void k_sm(device float *a [[buffer(0)]], device float *b [[buffer(1)]], device float *c [[buffer(2)]]) {
  simdgroup_float8x8 A, B, C = simdgroup_float8x8(0);
  simdgroup_load(A, a, 8); simdgroup_load(B, b, 8);
  simdgroup_multiply_accumulate(C, A, B, C);
  simdgroup_store(C, c, 8); }
