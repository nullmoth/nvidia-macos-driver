// Shaders for the macOS 26 Metal 3 gaps oracle (gaps26.m).
#include <metal_stdlib>
using namespace metal;

struct VOut { float4 pos [[position]]; };

// Full-screen triangle at a depth chosen by the buffer at index 0 (one float).
vertex VOut fs_v(uint vid [[vertex_id]], constant float &z [[buffer(0)]]) {
    float2 p = float2((vid << 1) & 2, vid & 2);
    VOut o; o.pos = float4(p * 2.0 - 1.0, z, 1.0); return o;
}
fragment float4 fs_red() { return float4(1, 0, 0, 1); }

// Two colour outputs: red on logical 0, green on logical 1.
struct Two { float4 a [[color(0)]]; float4 b [[color(1)]]; };
fragment Two fs_two() { Two t; t.a = float4(1, 0, 0, 1); t.b = float4(0, 1, 0, 1); return t; }

// Writes thread position so a dispatch with a required threadgroup size can be checked.
kernel void k_fill(device uint *out [[buffer(0)]], uint tid [[thread_position_in_grid]]) { out[tid] = tid + 1; }

// Reads texel (0, 0) of a texture (or a view of one) as float so format and swizzle of the view show in the result.
kernel void k_read(texture2d<float, access::read> t [[texture(0)]], device float4 *out [[buffer(0)]]) { out[0] = t.read(uint2(0, 0)); }
