#include <metal_stdlib>
#include <metal_tensor>
#include <MetalPerformancePrimitives/MetalPerformancePrimitives.h>
using namespace metal; using namespace mpp::tensor_ops;
using scales_t = tensor_blockwise<tensor_plane_scales, device metal_fp8_ue8m0_format, 32, 1>;
#define SC(NAME, BT) kernel void NAME(tensor<device half, dextents<int, 2>> a [[buffer(0)]], \
    tensor<device BT, dextents<int, 2>, tensor_handle, scales_t> b [[buffer(1)]], tensor<device float, dextents<int, 2>> c [[buffer(2)]]) { \
    constexpr auto d = matmul2d_descriptor(16, 32, 32, false, true, false); matmul2d<d, execution_simdgroups<1>> op; op.run(a, b, c); }
SC(sc_e4m3, metal_fp8_e4m3_format)
SC(sc_fp4, metal_fp4_e2m1_format)
SC(sc_i8, int8_t)
