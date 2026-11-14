#include <metal_stdlib>
using namespace metal;
kernel void kq(device atomic_uint *o [[buffer(0)]], device atomic_float *fo [[buffer(1)]], device atomic<ulong> *lo [[buffer(2)]], device float *f [[buffer(3)]], uint t [[thread_index_in_threadgroup]]) {
  atomic_fetch_or_explicit(o, 1u, memory_order_relaxed);
  atomic_fetch_add_explicit(fo, 1.0f, memory_order_relaxed);
}
