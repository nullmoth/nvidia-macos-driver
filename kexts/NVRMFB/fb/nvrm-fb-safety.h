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
// EDID overrides. A replacement EDID for one display is read from
//   /Library/NullMoth/edid-override/<vendor>-<product>.bin
// (the display's own manufacturer id and product code, four upper-case hex digits each). It is accepted only when it is a
// whole number of 128-byte blocks, carries the EDID header, its block count byte matches, every block sums to zero, and
// its manufacturer id and product code are the display's own. The same checks gate a boot raster or mode list built from
// it, so a file for another monitor, or a truncated one, is ignored rather than driven.
static inline bool nvrmEdidValid(const unsigned char *e, unsigned n)
{
    static const unsigned char hdr[8] = { 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0 };
    if (!e || n < 128 || n % 128) return false;
    for (unsigned i = 0; i < 8; i++) if (e[i] != hdr[i]) return false;
    if ((n / 128) != (unsigned)e[126] + 1u) return false;
    for (unsigned b = 0; b < n; b += 128) {
        unsigned char sum = 0;
        for (unsigned i = 0; i < 128; i++) sum = (unsigned char)(sum + e[b + i]);
        if (sum) return false;
    }
    return true;
}
static inline bool nvrmEdidSameDisplay(const unsigned char *a, const unsigned char *b)
{
    return a && b && a[8] == b[8] && a[9] == b[9] && a[10] == b[10] && a[11] == b[11];
}
// "<VVVV>-<PPPP>.bin": vendor = bytes 8..9 as stored (big-endian), product = bytes 11,10 (stored little-endian).
static inline void nvrmEdidOverrideName(const unsigned char *real, char *out, unsigned size)
{
    static const char hex[] = "0123456789ABCDEF";
    if (!real || !out || size < 16) { if (out && size) out[0] = 0; return; }
    const unsigned char b[4] = { real[8], real[9], real[11], real[10] };
    unsigned k = 0;
    for (unsigned i = 0; i < 4; i++) {
        out[k++] = hex[b[i] >> 4]; out[k++] = hex[b[i] & 15];
        if (i == 1) out[k++] = '-';
    }
    out[k++] = '.'; out[k++] = 'b'; out[k++] = 'i'; out[k++] = 'n'; out[k] = 0;
}
// Smaller-than-boot rasters are offered only when asked for: nvfbsmall=1 on the boot line, or the file
// /Library/NullMoth/nvfb-small (root-owned). A boot-arg of 0 turns it off even when the file exists.
static inline bool nvrmSmallRastersWanted(bool haveArg, unsigned argValue, bool fileTrusted)
{
    return haveArg ? argValue != 0 : fileTrusted;
}
// Root-owned and not writable by group or others: the trust rule for files the kext reads from /Library/NullMoth.
static inline bool nvrmFileTrusted(unsigned uid, unsigned mode)
{
    return uid == 0 && !(mode & 022);
}
// The service whose children are the display nubs. A framebuffer that refused to start (no display connected at boot) is
// detached from its nub by IOKit, so getProvider() on it returns nothing and a later hot-plug could not reach the idle
// nubs to start again. The parent remembered when a framebuffer first started is the fallback. (Diagnosed in issue 288.)
template <class T> static inline T *nvrmPickNubParent(T *viaProvider, T *remembered)
{
    return viaProvider ? viaProvider : remembered;
}
// ---- Variable refresh rate (adaptive sync) ----
// IOGraphics' model, from its public types: a display's IODisplayTimingRange carries kIORangeSupportsVRR, each detailed timing
// carries verticalBlankingExtension (the most blanking lines the display can stretch a frame by; 0 = none), and the
// kIOFBVariableRefreshRate attribute switches it on and off for the connection.
// The panel's vertical rate range comes from the EDID range-limits descriptor (tag 0xFD): min and max vertical rate in Hz,
// with the offset flags in byte 4 adding 255 Hz to the two when set. 0 when the EDID gives no range.
static inline bool nvrmEdidVerticalRange(const unsigned char *e, unsigned n, unsigned *minHz, unsigned *maxHz)
{
    if (!e || n < 128 || !minHz || !maxHz) return false;
    for (unsigned off = 54; off + 18 <= 126; off += 18) {
        const unsigned char *d = e + off;
        if (d[0] || d[1] || d[2] != 0 || d[3] != 0xFD) continue;
        unsigned lo = d[5], hi = d[6];
        if (d[4] & 2) hi += 255;
        if (d[4] & 1) lo += 255;
        if (!lo || hi < lo) return false;
        *minHz = lo; *maxHz = hi; return true;
    }
    return false;
}
// Lines a frame of `vTotal` lines at `hz` can be stretched by before it drops under the display's minimum rate.
static inline unsigned nvrmVrrBlankExtension(unsigned vTotal, unsigned hz, unsigned minHz, unsigned maxHz)
{
    if (!vTotal || !hz || !minHz || hz > maxHz || hz < minHz) return 0;
    const unsigned long long stretched = ((unsigned long long)vTotal * hz + minHz / 2) / minHz;
    return stretched > vTotal ? (unsigned)(stretched - vTotal) : 0;
}
