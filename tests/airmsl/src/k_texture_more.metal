#include <metal_stdlib>
using namespace metal;
kernel void k_tm(texture2d<float> t [[texture(0)]], texturecube<float> c [[texture(1)]], texture3d<float> v3 [[texture(2)]], texture1d<float> t1 [[texture(3)]],
                 texture2d_ms<float> ms [[texture(4)]], depth2d_array<float> da [[texture(5)]], texturecube_array<float> ca [[texture(6)]],
                 texture2d<uint, access::read_write> at [[texture(7)]], sampler s [[sampler(0)]], device float4 *o [[buffer(0)]], uint2 gid [[thread_position_in_grid]]) {
  float2 uv = float2(gid) / 64.0f;
  float4 r = t.sample(s, uv, bias(0.5f)) + t.sample(s, uv, min_lod_clamp(1.0f)) + t.sample(s, uv, int2(1, 1));
  r += c.sample(s, float3(uv, 1.0f)) + v3.sample(s, float3(uv, 0.5f)) + t1.sample(s, uv.x) + ms.read(gid, 1);
  r += da.sample_compare(s, uv, 1, 0.5f) + ca.sample(s, float3(uv, 1.0f), 2);
  r += t.gather(s, uv) + t.gather(s, uv, int2(0, 1), component::y);
  r += float4(t.get_width(1), t.get_height(0), t.get_num_mip_levels(), 0);
  auto sp = t.sparse_sample(s, uv); r += sp.value() * (sp.resident() ? 1.0f : 0.0f);
  o[gid.x] = r; }
