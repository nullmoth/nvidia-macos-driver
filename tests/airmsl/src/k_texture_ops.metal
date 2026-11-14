#include <metal_stdlib>
using namespace metal;
kernel void k_tex(texture2d<float, access::read_write> t [[texture(0)]], texture2d_array<half> a [[texture(1)]], depth2d<float> d [[texture(2)]], uint2 gid [[thread_position_in_grid]]) {
  constexpr sampler s(address::clamp_to_edge, filter::linear);
  float4 v = t.read(gid) + float4(a.gather(s, float2(gid) / 64.0, 0)) + d.sample_compare(s, float2(gid) / 64.0, 0.5);
  t.write(v, gid); }
