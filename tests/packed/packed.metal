// Kernels for packed.m: read every element of a [32 x 2] tensor of each packed format, and write elements.
#include <metal_stdlib>
#include <metal_tensor>
using namespace metal;
#define RD(NAME, FMT, T) kernel void NAME(tensor<device FMT, dextents<int, 2>> a [[buffer(0)]], device T *out [[buffer(1)]], uint2 i [[thread_position_in_grid]]) { out[i.y * 32 + i.x] = T(a[i.x, i.y]); }
RD(rd_int4, int4b_format, int)
RD(rd_uint4, uint4b_format, int)
RD(rd_int2, int2b_format, int)
RD(rd_uint2, uint2b_format, int)
RD(rd_fp8e4m3, metal_fp8_e4m3_format, float)
RD(rd_fp8e5m2, metal_fp8_e5m2_format, float)
RD(rd_fp8ue8m0, metal_fp8_ue8m0_format, float)
RD(rd_fp4, metal_fp4_e2m1_format, float)
// write: element (x, y) = x - 16 (wraps per format), then the raw bytes show the packing
#define WR(NAME, FMT, T) kernel void NAME(tensor<device FMT, dextents<int, 2>> a [[buffer(0)]], device T *out [[buffer(1)]], uint2 i [[thread_position_in_grid]]) { a[i.x, i.y] = T(int(i.x) - 16 + int(i.y)); }
WR(wr_int4, int4b_format, int)
WR(wr_uint4, uint4b_format, int)
WR(wr_int2, int2b_format, int)
WR(wr_fp8e4m3, metal_fp8_e4m3_format, float)
WR(wr_fp4, metal_fp4_e2m1_format, float)
