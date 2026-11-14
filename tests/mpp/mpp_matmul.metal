// Shader-side Metal 4 tensors and MetalPerformancePrimitives matmul. Each kernel computes C = A * B (or C += A * B).
#include <metal_stdlib>
#include <metal_tensor>
#include <MetalPerformancePrimitives/MetalPerformancePrimitives.h>
using namespace metal;
using namespace mpp;

// one tile: the tensors are exactly 16x16x16
kernel void mm_tile(tensor<device half, dextents<int, 2>> A [[buffer(0)]],
                    tensor<device half, dextents<int, 2>> B [[buffer(1)]],
                    tensor<device float, dextents<int, 2>> C [[buffer(2)]]) {
    constexpr tensor_ops::matmul2d_descriptor d(16, 16, 16);
    tensor_ops::matmul2d<d, execution_simdgroup> op;
    op.run(A, B, C);
}

// whole matrices cut into 16x16 tiles of C, one simdgroup per tile; K is read from the tensors
kernel void mm_sliced(tensor<device half, dextents<int, 2>> A [[buffer(0)]],
                      tensor<device half, dextents<int, 2>> B [[buffer(1)]],
                      tensor<device float, dextents<int, 2>> C [[buffer(2)]],
                      uint2 tg [[threadgroup_position_in_grid]]) {
    constexpr tensor_ops::matmul2d_descriptor d(16, 16, static_cast<int>(dynamic_extent));
    tensor_ops::matmul2d<d, execution_simdgroup> op;
    auto mA = A.slice(0, tg.y * 16);
    auto mB = B.slice(tg.x * 16, 0);
    auto mC = C.slice(tg.x * 16, tg.y * 16);
    op.run(mA, mB, mC);
}

// C += A * B, with B transposed
kernel void mm_acc_tr(tensor<device half, dextents<int, 2>> A [[buffer(0)]],
                      tensor<device half, dextents<int, 2>> B [[buffer(1)]],
                      tensor<device float, dextents<int, 2>> C [[buffer(2)]]) {
    constexpr tensor_ops::matmul2d_descriptor d(16, 16, 16, false, true, false, tensor_ops::matmul2d_descriptor::mode::multiply_accumulate);
    tensor_ops::matmul2d<d, execution_simdgroup> op;
    op.run(A, B, C);
}

// element access through the handle
kernel void t_access(tensor<device float, dextents<int, 2>> t [[buffer(0)]],
                     device float *out [[buffer(1)]],
                     uint i [[thread_position_in_grid]]) {
    int e0 = t.get_extent(0), e1 = t.get_extent(1);
    if (i == 0) { out[0] = (float)e0; out[1] = (float)e1; }
    if (i < (uint)(e0 * e1)) out[2 + i] = t[(int)(i % e0), (int)(i / e0)] * 2.0f;
}
