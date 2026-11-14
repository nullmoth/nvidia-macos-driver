Rulers for packed tensor element types, measured on an Apple GPU (references *_m1.txt):

- packed.m / packed.metal: an element access on a packed tensor (int2/int4/fp4/fp8) reads and writes the byte that holds
  the element (reference_m1.txt).
- matmul_decode.m / .metal: matmul2d with an identity left operand reads back the right operand decoded, for every byte
  value of int2, uint2, int4, uint4, fp4 e2m1, fp8 e4m3, fp8 e5m2, int8, uint8 and bfloat16 (matmul_decode_m1.txt);
  matmul_decode_model.py is the decoding the translator implements, and must report 0 differences against it.
- matmul_scaled.m / .metal: block-scaled right operands (ue8m0, one scale per 32 elements along k), with scale rows
  strides[1] / 32 apart (matmul_scaled_m1.txt; a scaled int8 operand reads as zeros on Apple GPUs and stays refused).

Build: xcrun metal -std=metal4.1 -c X.metal -o X.air && xcrun metallib X.air -o X.metallib;
clang -fobjc-arc X.m -framework Metal -framework Foundation -o X; run X X.metallib > out.txt and diff with the reference.
- reflection.m / .metal: pipeline reflection of tensor arguments (MTLTensorBinding: binding type 37, tensorDataType incl.
  packed formats, indexType, extents with -1 for dynamic ones, access 1) against reflection_m1.txt.
