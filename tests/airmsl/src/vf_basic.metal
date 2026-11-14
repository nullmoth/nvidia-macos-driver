#include <metal_stdlib>
using namespace metal;
struct VOut { float4 pos [[position]]; float2 uv; };
vertex VOut vmain(uint vid [[vertex_id]], constant float4 *verts [[buffer(0)]]) {
  VOut o; o.pos = verts[vid]; o.uv = verts[vid].xy * 0.5 + 0.5; return o; }
fragment float4 fmain(VOut in [[stage_in]], texture2d<float> t [[texture(0)]], sampler s [[sampler(0)]]) {
  return t.sample(s, in.uv); }
