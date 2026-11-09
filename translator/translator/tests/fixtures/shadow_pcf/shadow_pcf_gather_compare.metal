// Four-tap shadow filter written for this test: depth2d::gather_compare with constant offsets, averaged.
// Exercises the translator path for AIR air.gather_compare_depth_2d.f32 (a runtime sampler, constant texel offsets).
// Regenerate the IR next to this file with:
//   xcrun metal -std=metal3.1 -O2 -S -emit-llvm -target air64-apple-macos26.0 shadow_pcf_gather_compare.metal -o shadow_pcf_gather_compare.ll
//   (then drop the build directory from the source_file_name metadata line)
#include <metal_stdlib>
using namespace metal;

struct In {
    float4 position [[position]];
    float4 shadow_uv_ref;      // xy = shadow map coordinate, z = reference depth
};

fragment float4 fs_main_gather_compare(In in [[stage_in]],
                                       depth2d<float> shadow_map [[texture(9)]],
                                       sampler shadow_sampler [[sampler(8)]])
{
    float2 uv = in.shadow_uv_ref.xy;
    float ref = in.shadow_uv_ref.z;
    float4 a = shadow_map.gather_compare(shadow_sampler, uv, ref, int2(-1, -1));
    float4 b = shadow_map.gather_compare(shadow_sampler, uv, ref, int2(1, -1));
    float4 c = shadow_map.gather_compare(shadow_sampler, uv, ref, int2(-1, 1));
    float4 d = shadow_map.gather_compare(shadow_sampler, uv, ref, int2(1, 1));
    return (a + b + c + d) * 0.0625;
}
