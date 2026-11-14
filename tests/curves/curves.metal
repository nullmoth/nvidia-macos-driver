#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace metal::raytracing;
// One ray per thread, from rays[i] (origin xyz, then direction xyz). Writes distance, curve parameter and hit type.
struct Ray6 { packed_float3 o; packed_float3 d; };
struct Hit { float t; float u; uint type; uint prim; };
kernel void trace_curves(primitive_acceleration_structure as [[buffer(0)]], const device Ray6 *rays [[buffer(1)]],
                         device Hit *out [[buffer(2)]], uint i [[thread_position_in_grid]]) {
    intersector<curve_data> isect;
    isect.assume_geometry_type(geometry_type::curve);
    ray r(float3(rays[i].o), float3(rays[i].d), 0.0f, 1000.0f);
    auto h = isect.intersect(r, as);
    out[i].t = h.type == intersection_type::curve ? h.distance : -1.0f;
    out[i].u = h.type == intersection_type::curve ? h.curve_parameter : -1.0f;
    out[i].type = (uint)h.type; out[i].prim = h.primitive_id;
}
// The same trace through an intersection query: every curve candidate is committed, so the committed hit is the
// nearest one, as the intersector reports it.
kernel void trace_curves_query(primitive_acceleration_structure as [[buffer(0)]], const device Ray6 *rays [[buffer(1)]],
                               device Hit *out [[buffer(2)]], uint i [[thread_position_in_grid]]) {
    ray r(float3(rays[i].o), float3(rays[i].d), 0.0f, 1000.0f);
    intersection_params p; p.assume_geometry_type(geometry_type::curve);
    intersection_query<curve_data> q(r, as, p);
    while (q.next())
        if (q.get_candidate_intersection_type() == intersection_type::curve) q.commit_curve_intersection();
    const bool hit = q.get_committed_intersection_type() == intersection_type::curve;
    out[i].t = hit ? q.get_committed_distance() : -1.0f;
    out[i].u = hit ? q.get_committed_curve_parameter() : -1.0f;
    out[i].type = (uint)q.get_committed_intersection_type(); out[i].prim = q.get_committed_primitive_id();
}
