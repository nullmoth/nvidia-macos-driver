#include <metal_stdlib>
using namespace metal;
struct Payload { float scale; uint count; };
struct V { float4 pos [[position]]; };
using M = mesh<V, void, 3, 1, topology::triangle>;
[[mesh, max_total_threads_per_threadgroup(3)]]
void mmain(M m, const object_data Payload &p [[payload]], uint t [[thread_index_in_threadgroup]]) {
  m.set_primitive_count(p.count); V v; v.pos = float4(t * p.scale, 0, 0, 1); m.set_vertex(t, v); m.set_index(t, t); }
