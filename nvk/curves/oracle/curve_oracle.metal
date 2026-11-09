#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace raytracing;

// float4 on purpose: a float3 member occupies 16 bytes in a Metal struct, which would not match the host's 32-byte ray
struct RayIn { float4 o; float4 d; };
struct HitOut { float t; float u; uint prim; uint hit; };

kernel void curve_oracle(primitive_acceleration_structure as [[buffer(0)]], device const RayIn *rays [[buffer(1)]],
                         device HitOut *out [[buffer(2)]], uint i [[thread_position_in_grid]])
{
   intersector<curve_data> isect;
   isect.assume_geometry_type(geometry_type::curve);
   ray r(rays[i].o.xyz, rays[i].d.xyz, rays[i].o.w, rays[i].d.w);
   auto h = isect.intersect(r, as);
   HitOut o;
   // hit carries the reported intersection type: 0 none, 1 triangle, 2 bounding box, 3 curve
   o.hit = (uint)h.type;
   o.t = o.hit ? h.distance : 0.0f;
   o.u = o.hit ? h.curve_parameter : 0.0f;
   o.prim = o.hit ? h.primitive_id : 0u;
   out[i] = o;
}
