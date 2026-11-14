#include <metal_stdlib>
using namespace metal;

// Spins long enough for a GPU timestamp pair around it to differ, and writes one value so it cannot be removed.
kernel void spin(device uint *out [[buffer(0)]], uint tid [[thread_position_in_grid]])
{
    uint x = tid + 1;
    for (uint i = 0; i < 20000; i++) x = x * 1664525u + 1013904223u;
    out[tid] = x;
}

struct VOut { float4 pos [[position]]; };

vertex VOut vs_tri(uint vid [[vertex_id]])
{
    float2 p = float2(float((vid << 1) & 2), float(vid & 2));
    VOut o; o.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    return o;
}

// Two logical outputs with different constants, so a swap of the physical attachments is visible in the pixels.
struct FOut { float4 a [[color(0)]]; float4 b [[color(1)]]; };

fragment FOut fs_two(VOut in [[stage_in]])
{
    FOut o; o.a = float4(1, 0, 0, 1); o.b = float4(0, 1, 0, 1);
    return o;
}
