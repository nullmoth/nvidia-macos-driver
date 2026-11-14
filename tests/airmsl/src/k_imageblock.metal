#include <metal_stdlib>
using namespace metal;
fragment float4 f_ib(float4 in [[color(0)]], float4 p [[position]]) { return in * 0.5 + p * 0.0; }
