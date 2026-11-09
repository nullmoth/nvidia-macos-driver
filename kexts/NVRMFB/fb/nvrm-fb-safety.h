/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#pragma once
static inline bool nvrmCheckedPitch(unsigned width, unsigned alignment, unsigned *out)
{
    if (!out || !width || !alignment || (alignment & (alignment - 1))) return false;
    const unsigned long long pitch = ((unsigned long long)width * 4 + alignment - 1) &
                                      ~((unsigned long long)alignment - 1);
    if (pitch > 0xffffffffULL) return false;
    *out = (unsigned)pitch;
    return true;
}
static inline bool nvrmKmsRasterValid(unsigned width, unsigned height)
{
    return width && height && width <= 0xffffu && height <= 0xffffu;
}
static inline unsigned long long nvrmApertureBytes(unsigned pitch, unsigned height,
                                                  unsigned long long owned)
{
    if (!pitch || !height) return 0;
    const unsigned long long need = (unsigned long long)pitch * height;
    if (need > owned) return 0;
    const unsigned long long spare = owned - need;
    return need + (spare < 128 ? spare : 128);
}
// Which of `count` timings macOS proposed to keep when more than `budget` arrive. macOS lists every scaled (HiDPI)
// desktop for every refresh rate, so a 180 Hz panel can offer 500+ timings and the first 180 Hz scaled mode sits past
// index 300. Keeping the first `budget` in arrival order dropped the highest rates. Instead the highest refresh rates
// are kept first (all timings of one rate, in arrival order, until the budget is full), then lower ones; timings the
// caller marked `must` (the one on the head) are always kept. Arrival order is preserved among the kept ones.
// Returns the number kept; keep[i] is set for each timing.
static inline unsigned nvrmPickTimings(const unsigned *hz, const unsigned char *must, unsigned count, unsigned budget,
                                       unsigned char *keep)
{
    unsigned kept = 0;
    for (unsigned i = 0; i < count; i++) keep[i] = 0;
    for (unsigned i = 0; i < count && kept < budget; i++) if (must && must[i]) { keep[i] = 1; kept++; }
    while (kept < budget) {
        unsigned best = 0; bool any = false;
        for (unsigned i = 0; i < count; i++) if (!keep[i] && (!any || hz[i] > best)) { best = hz[i]; any = true; }
        if (!any) break;
        for (unsigned i = 0; i < count && kept < budget; i++) if (!keep[i] && hz[i] == best) { keep[i] = 1; kept++; }
    }
    return kept;
}
