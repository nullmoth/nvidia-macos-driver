#include <metal_stdlib>
#include <metal_tensor>
using namespace metal;
kernel void k(tensor<device half, dextents<int, 2>> a [[buffer(0)]], tensor<device int4b_format, dextents<short, 3>> b [[buffer(1)]],
              tensor<device float, extents<int, 4, 8>> c [[buffer(3)]], device float *out [[buffer(2)]], uint i [[thread_position_in_grid]]) {
    out[i] = float(a[i, 0]) + float(b[i, 0, 0]) + c[i % 4, 0];
}
