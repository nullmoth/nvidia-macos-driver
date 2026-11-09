/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_counter_heap.h - Metal 4 counter heaps over Vulkan timestamp queries.
 * A timestamp heap entry is one 64-bit value (MTL4TimestampHeapEntry). The device counts in units of timestampPeriod nanoseconds,
 * so the frequency Metal asks for is 1e9 / period; a device that reports no usable period or no timestamp bits has no counters
 * and answers 0, and a heap is then refused. */
#ifndef NVMTL_COUNTER_HEAP_H
#define NVMTL_COUNTER_HEAP_H
#include <math.h>
#include <stdint.h>

#define NVMTL_HEAP_TYPE_INVALID   0
#define NVMTL_HEAP_TYPE_TIMESTAMP 1
#define NVMTL_HEAP_MAX_ENTRIES    32768u   /* the legacy sample buffer's limit, which backs the heap */

static inline uint64_t nvmtl_ts_frequency(double period_ns, uint32_t valid_bits)
{
    if (!valid_bits || !isfinite(period_ns) || period_ns <= 0.0) return 0;
    const double hz = 1e9 / period_ns;
    return hz < 1.0 || hz > 1.8e19 ? 0 : (uint64_t)(hz + 0.5);
}
static inline uint64_t nvmtl_heap_entry_size(int type) { return type == NVMTL_HEAP_TYPE_TIMESTAMP ? 8u : 0u; }
/* NULL when a heap of this type and count can be made, else why not */
static inline const char *nvmtl_heap_refusal(int type, uint64_t count, uint64_t frequency)
{
    if (type != NVMTL_HEAP_TYPE_TIMESTAMP) return "only timestamp heaps exist";
    if (!count || count > NVMTL_HEAP_MAX_ENTRIES) return "an entry count outside 1..32768";
    if (!frequency) return "this device has no usable GPU timestamps";
    return 0;
}
/* a resolve of `range` entries into a destination of `dest_bytes` bytes at `dest_off`: does it fit */
static inline int nvmtl_heap_resolve_fits(uint64_t entries, uint64_t dest_off, uint64_t dest_bytes)
{ return entries && dest_off <= dest_bytes && entries <= (dest_bytes - dest_off) / 8u; }
#endif
