#include <metal_stdlib>
#include <metal_tensor>
#include <MetalPerformancePrimitives/MetalPerformancePrimitives.h>
using namespace metal;
using namespace mpp;
kernel void k_mm(tensor<device half, dextents<int, 2>> A [[buffer(0)]], tensor<device half, dextents<int, 2>> B [[buffer(1)]], tensor<device float, dextents<int, 2>> C [[buffer(2)]]) {
  constexpr tensor_ops::matmul2d_descriptor d(16, 16, 16);
  tensor_ops::matmul2d<d, execution_simdgroup> op;
  op.run(A, B, C); }
