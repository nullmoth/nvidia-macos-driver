#include <metal_stdlib>
using namespace metal;
struct FIn { float4 pos [[position]]; float2 uv; };
fragment float4 f_bary(FIn in [[stage_in]], float3 bc [[barycentric_coord]], texture2d<float> t [[texture(0)]]) {
  constexpr sampler s(filter::linear);
  return t.sample(s, in.uv, gradient2d(dfdx(in.uv), dfdy(in.uv))) * bc.x + t.sample(s, in.uv, level(2.0)) * bc.y; }
