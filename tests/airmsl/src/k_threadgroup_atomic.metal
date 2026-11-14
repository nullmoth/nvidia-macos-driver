#include <metal_stdlib>
using namespace metal;
kernel void k_tg(device atomic_uint *o [[buffer(0)]], threadgroup uint *sh [[threadgroup(0)]], uint t [[thread_index_in_threadgroup]], uint i [[thread_position_in_grid]]) {
  sh[t] = i; threadgroup_barrier(mem_flags::mem_threadgroup);
  atomic_fetch_add_explicit(o, sh[(t + 1) & 31], memory_order_relaxed); }
