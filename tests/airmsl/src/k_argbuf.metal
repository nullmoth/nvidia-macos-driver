#include <metal_stdlib>
using namespace metal;
struct Args { device float *a; constant float4 *b; texture2d<float> t; sampler s; };
kernel void k_ab(constant Args &args [[buffer(0)]], uint i [[thread_position_in_grid]]) {
  args.a[i] = args.b[0].x + args.t.sample(args.s, float2(0.5)).x; }
