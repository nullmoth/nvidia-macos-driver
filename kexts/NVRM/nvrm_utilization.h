/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#include "nvrm_utilization_abi.h"

// The caller serialises queries and teardown. All RM objects and the large
// sample ring belong to the opaque state, allocated only on the first query.
extern "C" bool nvrm_query_utilization(unsigned int gpuId, void **state, struct NVRMUtilization *sample);
extern "C" void nvrm_free_utilization(void **state);

// Kept independent of kernel and RM headers so ring selection can be exercised
// by a host test with the same layout or NVIDIA's public sample definitions.
template <typename Params>
static bool nvrmUtilizationLatestSample(const Params &p, struct NVRMUtilization *out)
{
    const unsigned int capacity = sizeof(p.samples) / sizeof(p.samples[0]);
    if (!out || p.count > capacity || p.bufSize != sizeof(p.samples)) return false;
    const bool slotTracker = p.tracker < capacity;
    const bool byteTracker = p.tracker < sizeof(p.samples) &&
                             p.tracker % sizeof(p.samples[0]) == 0;
    if (!slotTracker && !byteTracker) return false;

    // PERFMON returns a circular queue, not a latest-value array. A fixed slot
    // is refreshed only once per complete ring. Compare timestamps so a query
    // also works before the ring has filled, when the unused entries are zero.
    // GSP on the tested RTX 3080 leaves count at zero and reports tracker as a
    // byte offset, although the public structure describes an entry offset.
    // Scan the bounded array; zero timestamps distinguish an empty ring.
    unsigned int latest = 0;
    unsigned long long timestamp = 0;
    for (unsigned int i = 0; i < capacity; i++) {
        if (p.samples[i].base.timeStamp > timestamp) {
            latest = i;
            timestamp = p.samples[i].base.timeStamp;
        }
    }
    if (!timestamp) return false;
    const auto &s = p.samples[latest];
    if (s.gr.util > 10000u || s.fb.util > 10000u ||
        s.nvenc.util > 10000u || s.nvdec.util > 10000u) return false;
    out->gr = s.gr.util;
    out->fb = s.fb.util;
    out->nvenc = s.nvenc.util;
    out->nvdec = s.nvdec.util;
    out->timestamp = timestamp;
    return true;
}
