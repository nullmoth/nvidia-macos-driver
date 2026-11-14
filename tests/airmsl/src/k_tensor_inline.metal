#include <metal_stdlib>
#include <metal_tensor>
using namespace metal;
kernel void k_ti(tensor<device float, dextents<int, 2>> t [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  int e = t.get_extent(0);
  t[(int)(i % e), 0] = (float)e; }
