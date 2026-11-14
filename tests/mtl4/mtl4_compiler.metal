#include <metal_stdlib>
using namespace metal;

constant uint scale [[function_constant(0)]];

// out[i] = i * scale. Needs the constant: Metal refuses to build it unspecialized.
kernel void k_scale(device uint *out [[buffer(0)]], uint tid [[thread_position_in_grid]]) {
    out[tid] = tid * scale;
}

// out[i] = i, no constants.
kernel void k_plain(device uint *out [[buffer(0)]], uint tid [[thread_position_in_grid]]) {
    out[tid] = tid;
}

struct VOut { float4 pos [[position]]; };

vertex VOut v_main(uint vid [[vertex_id]]) {
    float2 p[3] = { float2(-1, -1), float2(3, -1), float2(-1, 3) };
    VOut o; o.pos = float4(p[vid], 0, 1); return o;
}

fragment float4 f_main() { return float4(1, 0, 0, 1); }

// Functions reached through a visible function table, as visible, stitchable and (for binary functions) library functions.
[[visible]] int vis_add(int x) { return x + 100; }
[[stitchable]] int st_add(int x) { return x + 1; }
[[stitchable]] int st_mul(int x, int y) { return x * y; }

kernel void k_call(device int *out [[buffer(0)]], visible_function_table<int(int)> t [[buffer(1)]],
                   uint tid [[thread_position_in_grid]]) {
    out[tid] = t[0](int(tid));
}
