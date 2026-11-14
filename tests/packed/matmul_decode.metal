#include <metal_stdlib>
#include <metal_tensor>
#include <MetalPerformancePrimitives/MetalPerformancePrimitives.h>
using namespace metal; using namespace mpp::tensor_ops;
// C(m, n) = sum_k A(m, k) B(k, n) with A an identity selection: C reads back B decoded. m 16, n 32, k 32.
#define MM(NAME, BT) kernel void NAME(tensor<device half, dextents<int, 2>> a [[buffer(0)]], tensor<device BT, dextents<int, 2>> b [[buffer(1)]], \
    tensor<device float, dextents<int, 2>> c [[buffer(2)]]) { \
    constexpr auto d = matmul2d_descriptor(16, 32, 32, false, false, false); matmul2d<d, execution_simdgroups<1>> op; op.run(a, b, c); }
MM(mm_i4, int4b_format)
MM(mm_u4, uint4b_format)
MM(mm_i2, int2b_format)
MM(mm_u2, uint2b_format)
MM(mm_fp4, metal_fp4_e2m1_format)
MM(mm_e4m3, metal_fp8_e4m3_format)
MM(mm_e5m2, metal_fp8_e5m2_format)
MM(mm_i8, int8_t)
MM(mm_u8, uint8_t)
MM(mm_b16, bfloat)
