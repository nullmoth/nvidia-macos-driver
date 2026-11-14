#include <metal_stdlib>
using namespace metal;
kernel void kb(device atomic_uint *o [[buffer(0)]], device float *f [[buffer(1)]], threadgroup atomic_uint *sh [[threadgroup(0)]], uint t [[thread_index_in_threadgroup]]) {
  threadgroup_barrier(mem_flags::mem_none);
  threadgroup_barrier(mem_flags::mem_device);
  threadgroup_barrier(mem_flags::mem_threadgroup);
  threadgroup_barrier(mem_flags::mem_texture);
  threadgroup_barrier(mem_flags::mem_device | mem_flags::mem_threadgroup);
  simdgroup_barrier(mem_flags::mem_threadgroup);
  simdgroup_barrier(mem_flags::mem_device);
  atomic_thread_fence(mem_flags::mem_device, memory_order_seq_cst, thread_scope_device);
  atomic_thread_fence(mem_flags::mem_threadgroup, memory_order_seq_cst, thread_scope_threadgroup);
  atomic_fetch_add_explicit(o, 1u, memory_order_relaxed);
  atomic_fetch_max_explicit(sh, 1u, memory_order_relaxed);
  atomic_exchange_explicit(o, 2u, memory_order_relaxed);
  uint e = 1; atomic_compare_exchange_weak_explicit(o, &e, 3u, memory_order_relaxed, memory_order_relaxed);
  atomic_store_explicit(o, 5u, memory_order_relaxed);
  f[t] = atomic_load_explicit(o, memory_order_relaxed);
}
