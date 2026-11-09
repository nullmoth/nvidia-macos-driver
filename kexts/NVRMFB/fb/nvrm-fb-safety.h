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
// The uncompressed payload a link carries, in bits per second, above which a mode is driven with DSC. DP 1.4 is
// HBR3 x4 (25.92 Gbit/s); an HDMI 2.1 FRL link (48 Gbit/s, 16b/18b) carries 42.67 Gbit/s. Other connectors (TMDS DVI,
// analog, unknown) never get DSC hints: the commit would be refused for bandwidth there, not compressed. An internal
// panel is eDP, which follows the DP limit. 0 = no DSC hint for this connector.
static inline unsigned long long nvrmDscThresholdBps(unsigned hdaConnectorType, bool internal)
{
    if (internal || hdaConnectorType == 0x400) return 25920000000ull;
    if (hdaConnectorType == 0x800) return 42670000000ull;
    return 0;
}
// Registry location for framebuffer instance `index`. Every instance is named NVRMFramebuffer, so two heads shared the
// path NVRMFramebuffer/display0 and macOS merged their display preferences (colour profiles, identity). Apple
// framebuffers carry a unit location (@0, @1 ...). Instance 0 keeps its old, unnumbered path so the preferences already
// stored for a single display stay valid; every other instance gets "@<index>". Returns false when no location is set.
static inline bool nvrmFbLocation(unsigned index, char *out, unsigned size)
{
    if (!index || !out || size < 12) return false;
    char tmp[12]; unsigned n = 0;
    for (unsigned v = index; v && n < sizeof tmp; v /= 10) tmp[n++] = (char)('0' + v % 10);
    for (unsigned i = 0; i < n; i++) out[i] = tmp[n - 1 - i];
    out[n] = 0;
    return true;
}
// Highest frame rate published in IOFBTimingRange. macOS drops EDID timings above it, and 500 hid 540 Hz panels' own rate.
enum { kNvrmMaxFrameRate = 1000 };
// The flip latch mode in effect. `setting` is the debug.nvrmfb_flip_latch value (1 = wait for every flip). At 357 Hz and
// above the wait costs more than a third of the frame (a 540 Hz frame is 1.85 ms, the wait was measured at 1.15 ms
// average), so with `autoDefer` set a setting of 1 becomes 2: the flip is verified at the head of the next submit
// instead, still before its predecessor's surface can be reused. An explicit 0 or 2 is never changed.
static inline int nvrmFlipLatchMode(int setting, int autoDefer, unsigned long long periodNs)
{
    if (setting == 1 && autoDefer && periodNs && periodNs <= 2800000ull) return 2;
    return setting;
}
// Native refresh rate from which half and quarter rates are derived, and the lowest derived rate offered.
enum { kNvrmDerivedRateMinHz = 240, kNvrmDerivedRateFloorHz = 48 };
