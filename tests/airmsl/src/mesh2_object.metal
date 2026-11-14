#include <metal_stdlib>
using namespace metal;
struct Payload { float scale; uint count; };
[[object, max_total_threads_per_threadgroup(32)]]
void omain(object_data Payload &p [[payload]], mesh_grid_properties g, uint t [[thread_index_in_threadgroup]], constant float *in [[buffer(0)]]) {
  if (t == 0) { p.scale = in[0]; p.count = 1; g.set_threadgroups_per_grid(uint3(1, 1, 1)); } }
