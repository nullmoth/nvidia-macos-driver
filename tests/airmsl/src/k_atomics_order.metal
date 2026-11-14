#include <metal_stdlib>
using namespace metal;
kernel void kr(device atomic<uint, thread_scope_device> *d [[buffer(0)]],
               device atomic<uint, thread_scope_threadgroup> *g [[buffer(1)]],
               device atomic<uint, thread_scope_simdgroup> *s [[buffer(2)]],
               threadgroup atomic<uint, thread_scope_threadgroup> *tg [[threadgroup(0)]],
               threadgroup atomic<uint, thread_scope_device> *td [[threadgroup(1)]],
               device uint *out [[buffer(3)]]) {
  uint r = 0;
  r += atomic_fetch_add_explicit(d, 1u, memory_order_relaxed);
  r += atomic_fetch_add_explicit(d, 2u, memory_order_acquire, mem_flags::mem_device);
  r += atomic_fetch_add_explicit(d, 3u, memory_order_release, mem_flags::mem_device);
  r += atomic_fetch_add_explicit(d, 4u, memory_order_acq_rel, mem_flags::mem_device);
  r += atomic_fetch_add_explicit(d, 5u, memory_order_seq_cst, mem_flags::mem_device);
  r += atomic_fetch_add_explicit(d, 6u, memory_order_acq_rel, mem_flags::mem_threadgroup);
  r += atomic_fetch_add_explicit(d, 7u, memory_order_acq_rel, mem_flags::mem_device | mem_flags::mem_threadgroup);
  r += atomic_fetch_add_explicit(d, 8u, memory_order_acq_rel, mem_flags::mem_texture);
  r += atomic_fetch_add_explicit(d, 9u, memory_order_acq_rel, mem_flags::mem_none);
  r += atomic_fetch_add_explicit(g, 10u, memory_order_relaxed);
  r += atomic_fetch_add_explicit(s, 11u, memory_order_relaxed);
  r += atomic_fetch_add_explicit(tg, 12u, memory_order_relaxed);
  r += atomic_fetch_add_explicit(td, 13u, memory_order_acq_rel, mem_flags::mem_threadgroup);
  r += atomic_load_explicit(d, memory_order_acquire, mem_flags::mem_device);
  atomic_store_explicit(d, 14u, memory_order_release, mem_flags::mem_device);
  uint e = 1; r += atomic_compare_exchange_weak_explicit(d, &e, 15u, memory_order_acq_rel, memory_order_acquire, mem_flags::mem_device);
  r += atomic_exchange_explicit(d, 16u, memory_order_seq_cst, mem_flags::mem_device);
  atomic_thread_fence(mem_flags::mem_device, memory_order_seq_cst, thread_scope_device);
  atomic_thread_fence(mem_flags::mem_threadgroup, memory_order_acq_rel, thread_scope_threadgroup);
  atomic_thread_fence(mem_flags::mem_device, memory_order_release, thread_scope_simdgroup);
  atomic_thread_fence(mem_flags::mem_device | mem_flags::mem_texture, memory_order_acquire, thread_scope_device);
  out[0] = r; }
