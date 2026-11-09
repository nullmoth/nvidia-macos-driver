#include <metal_stdlib>
#include <metal_raytracing>
using namespace metal;
using namespace raytracing;

struct RayIn { float3 o; float tmin; float3 d; float tmax; };
struct HitOut { float t; float u; uint prim; uint hit; };

kernel void curve_oracle(primitive_acceleration_structure as [[buffer(0)]], device const RayIn *rays [[buffer(1)]],
                         device HitOut *out [[buffer(2)]], uint i [[thread_position_in_grid]])
{
   intersector<curve_data> isect;
   isect.assume_geometry_type(geometry_type::curve);
   ray r(rays[i].o, rays[i].d, rays[i].tmin, rays[i].tmax);
   auto h = isect.intersect(r, as);
   HitOut o;
   o.hit = h.type == intersection_type::curve ? 1u : 0u;
   o.t = o.hit ? h.distance : 0.0f;
   o.u = o.hit ? h.curve_parameter : 0.0f;
   o.prim = o.hit ? h.primitive_id : 0u;
   out[i] = o;
}
