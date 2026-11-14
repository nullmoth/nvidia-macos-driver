#include <metal_stdlib>
using namespace metal;
kernel void k_ul(device atomic_uint *o [[buffer(0)]], threadgroup atomic_uint *sh [[threadgroup(0)]], uint t [[thread_index_in_threadgroup]]) {
  atomic_store_explicit(sh + t, 0u, memory_order_relaxed);
  threadgroup_barrier(mem_flags::mem_threadgroup);
  atomic_fetch_add_explicit(sh + (t ^ 1), 1u, memory_order_relaxed);
  atomic_fetch_min_explicit(o, t, memory_order_relaxed);
  atomic_fetch_and_explicit(o, ~t, memory_order_relaxed);
  atomic_fetch_xor_explicit(o, t, memory_order_relaxed);
  atomic_fetch_sub_explicit(o, 1u, memory_order_relaxed); }
