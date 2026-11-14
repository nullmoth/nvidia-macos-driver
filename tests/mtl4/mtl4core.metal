#include <metal_stdlib>
using namespace metal;

struct VOut { float4 pos [[position]]; };

vertex VOut tri_v(uint vid [[vertex_id]], const device float2 *verts [[buffer(0)]]) {
    VOut o;
    o.pos = float4(verts[vid], 0.0, 1.0);
    return o;
}

// Colour from a buffer bound by address, scaled by a texture and sampler bound by resource ID.
fragment float4 tri_f(VOut in [[stage_in]], const device float4 *color [[buffer(0)]],
                      texture2d<float> tex [[texture(0)]], sampler smp [[sampler(0)]]) {
    return *color * tex.sample(smp, float2(0.5, 0.5));
}

kernel void fill_k(device uint *out [[buffer(0)]], constant uint &base [[buffer(1)]], uint t [[thread_position_in_grid]]) {
    out[t] = base + t;
}

kernel void add_k(device uint *d [[buffer(0)]], uint t [[thread_position_in_grid]]) {
    d[t] += 1000u;
}

// Mesh shading: one mesh threadgroup of three threads emits one triangle; no object stage.
struct MV { float4 pos [[position]]; };
using MeshOut = metal::mesh<MV, void, 3, 1, metal::topology::triangle>;

[[mesh]] void mesh_m(MeshOut out, const device float2 *verts [[buffer(0)]], uint tid [[thread_index_in_threadgroup]]) {
    if (tid < 3) {
        MV v; v.pos = float4(verts[tid], 0.0, 1.0);
        out.set_vertex(tid, v);
        out.set_index(tid, tid);
    }
    if (tid == 0) out.set_primitive_count(1);
}

fragment float4 mesh_f(MV in [[stage_in]], const device float4 *color [[buffer(0)]]) {
    return *color;
}

// Ray tracing: one ray per thread along +z from z = -1; the output is the hit distance or -1.
#include <metal_raytracing>
using namespace metal::raytracing;

kernel void rt_k(primitive_acceleration_structure as [[buffer(0)]], const device float2 *rays [[buffer(1)]],
                 device float *out [[buffer(2)]], uint t [[thread_position_in_grid]]) {
    ray r; r.origin = float3(rays[t], -1.0); r.direction = float3(0, 0, 1); r.min_distance = 0; r.max_distance = 100;
    intersector<triangle_data> i;
    intersection_result<triangle_data> res = i.intersect(r, as);
    out[t] = res.type == intersection_type::triangle ? res.distance : -1.0;
}

kernel void rt_inst_k(instance_acceleration_structure as [[buffer(0)]], const device float2 *rays [[buffer(1)]],
                      device float *out [[buffer(2)]], uint t [[thread_position_in_grid]]) {
    ray r; r.origin = float3(rays[t], -1.0); r.direction = float3(0, 0, 1); r.min_distance = 0; r.max_distance = 100;
    intersector<triangle_data, instancing> i;
    intersection_result<triangle_data, instancing> res = i.intersect(r, as);
    out[t] = res.type == intersection_type::triangle ? res.distance : -1.0;
}

// Writes the arguments of an indirect threads dispatch: n threads in a grid, 16 per threadgroup.
kernel void args_k(device uint *a [[buffer(0)]], constant uint &n [[buffer(1)]]) {
    a[0] = n; a[1] = 1; a[2] = 1; a[3] = 16; a[4] = 1; a[5] = 1;
}

// two colour outputs for the colour attachment map arm: logical 0 red, logical 1 green
struct TwoOut { float4 c0 [[color(0)]]; float4 c1 [[color(1)]]; };
fragment TwoOut two_f(VOut in [[stage_in]]) { TwoOut o; o.c0 = float4(1, 0, 0, 1); o.c1 = float4(0, 1, 0, 1); return o; }
