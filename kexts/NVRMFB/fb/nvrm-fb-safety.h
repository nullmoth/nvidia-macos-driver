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
// ---- Output colour controls (colour depth, format, range, vibrance) ----
// Settings that reach the head: bpc 0 = automatic, else 6/8/10/12; format 0 RGB, 1 YCbCr 4:2:2, 2 YCbCr 4:4:4, 3 automatic;
// range 0 automatic, 1 full, 2 limited; vibrance -100..100 percent (0 = off).
static inline bool nvrmColorBpcValid(unsigned v) { return v == 0 || v == 6 || v == 8 || v == 10 || v == 12; }
static inline bool nvrmColorFormatValid(unsigned v) { return v <= 3; }
static inline bool nvrmColorRangeValid(unsigned v) { return v <= 2; }
// Vibrance is a saturation gain in the layer's colour matrix, 16.16 fixed point: each channel becomes
//   luma + (1 + v/100) * (channel - luma), luma from the Rec.709 weights, so greys are untouched and v = -100 is monochrome.
// v is clamped to -100..100. Rows sum to 1.0 exactly (greys stay grey).
static inline void nvrmSaturationMatrix(int vibrance, int m[3][3])
{
    if (vibrance < -100) vibrance = -100;
    if (vibrance > 100) vibrance = 100;
    static const long long w[3] = { 13933, 46871, 4732 };    // 0.2126, 0.7152, 0.0722 in 16.16 (sum 65536)
    const long long gain = 65536 + (long long)vibrance * 65536 / 100;   // 1 + v/100
    for (int r = 0; r < 3; r++) {
        long long sum = 0;
        for (int c = 0; c < 3; c++) {
            long long v = w[c] * (65536 - gain) / 65536 + (r == c ? gain : 0);
            if (c < 2) { m[r][c] = (int)v; sum += v; }
        }
        m[r][2] = (int)(65536 - sum);
    }
}
// ---- HDR (HDR10) ----
// The display's HDR static-metadata data block (CTA-861 extension block, extended tag 6): which transfer functions it takes
// (bit 2 of `eotfMask` is ST 2084 / PQ) and its luminance codes. Returns true when a PQ-capable block is present.
static inline bool nvrmEdidHdrPq(const unsigned char *e, unsigned n, unsigned *maxLumCode)
{
    if (!e || n < 256) return false;
    for (unsigned blk = 128; blk + 128 <= n; blk += 128) {
        const unsigned char *x = e + blk;
        if (x[0] != 0x02) continue;
        const unsigned end = x[2] < 4 ? 4 : (x[2] > 127 ? 127 : x[2]);
        for (unsigned i = 4; i < end;) {
            const unsigned tag = x[i] >> 5, len = x[i] & 31;
            if (i + 1 + len > end) break;
            if (tag == 7 && len >= 3 && x[i + 1] == 6 && (x[i + 2] & 4)) {
                if (maxLumCode) *maxLumCode = len >= 5 ? x[i + 4] : 0;
                return true;
            }
            i += 1 + len;
        }
    }
    return false;
}
// The EDID luminance code to cd/m2: 50 * 2^(code / 32), 0 = unknown.
static inline unsigned nvrmHdrLuminanceFromCode(unsigned code)
{
    if (!code) return 0;
    unsigned long long v = 50ull << (code / 32);
    static const unsigned frac[32] = { 1000, 1022, 1044, 1067, 1091, 1114, 1139, 1164, 1189, 1215, 1242, 1269, 1297, 1325, 1354, 1384, 1414, 1445, 1477, 1509, 1542, 1576, 1610, 1646, 1682, 1719, 1756, 1795, 1834, 1874, 1915, 1957 };
    v = v * frac[code % 32] / 1000;
    return v > 65535 ? 65535 : (unsigned)v;
}
// ---- Bounded waits ----
// Try `tryFn()` until it succeeds or `totalMs` has passed, sleeping `stepMs` between tries. Returns whether it succeeded.
// A WindowServer thread that waits forever on a lock someone else holds is killed by the watchdog after two minutes
// (taking the session with it); a wait that gives up lets the mode change fail and be retried.
template <class TryFn, class SleepFn>
static inline bool nvrmTryUntil(TryFn tryFn, SleepFn sleepFn, unsigned totalMs, unsigned stepMs)
{
    if (tryFn()) return true;
    if (!stepMs) stepMs = 1;
    for (unsigned waited = 0; waited < totalMs; waited += stepMs) {
        sleepFn(stepMs);
        if (tryFn()) return true;
    }
    return false;
}
// ---- Head scaler throughput ----
// A scaled (HiDPI or downscaled) mode is refused at the mode set when the head's scaler cannot move that many pixels per
// second; the limit is ~1.1-1.5 Gpixel/s but differs per card and per DSC link, so it is learned: from the refusals, and
// optionally by a binary search of test commits. The load of a timing is the larger of the raster and the desktop, times the
// refresh rate.
static inline unsigned long long nvrmScalerLoad(unsigned rasterW, unsigned rasterH, unsigned deskW, unsigned deskH, unsigned hz)
{
    const unsigned long long r = (unsigned long long)rasterW * rasterH, d = (unsigned long long)deskW * deskH;
    return (r > d ? r : d) * hz;
}
// A refusal at `load` means everything at or above it is too much; keeps the smallest refused load minus one. 0 = unknown.
static inline unsigned long long nvrmScalerLearn(unsigned long long limit, unsigned long long refusedLoad)
{
    if (!refusedLoad) return limit;
    const unsigned long long l = refusedLoad - 1;
    return (!limit || l < limit) ? l : limit;
}
static inline bool nvrmScalerAllows(unsigned long long limit, unsigned long long load) { return !limit || load <= limit; }
// Largest passing load among `loads` (ascending), assuming monotonic pass-then-fail; test(load) runs a non-committing trial.
// Returns the limit (the largest passing load, or 0 if the smallest already fails; ~0 if all pass = no limit).
template <class T>
static inline unsigned long long nvrmScalerSearch(const unsigned long long *loads, unsigned n, T test, unsigned *trials)
{
    unsigned lo = 0, hi = n, t = 0;           // answer in [lo, hi): loads[lo-1] passes, loads[hi] fails
    while (lo < hi) {
        const unsigned mid = lo + (hi - lo) / 2;
        t++;
        if (test(loads[mid])) lo = mid + 1; else hi = mid;
    }
    if (trials) *trials = t;
    if (lo == n) return ~0ull;
    return lo ? loads[lo - 1] : 0;
}
// ---- DSR-style supersampled desktops ----
// NVIDIA's driver offers desktops larger than the panel (DSR: 1.20x to 4.00x the pixel count) that the display engine scales
// down to the native raster. Per-axis factors are the square roots of 1.20, 1.50, 1.78, 2.00, 2.25, 3.00, 4.00, in 1/1000.
enum { kNvrmDsrFactors = 7 };
static inline unsigned nvrmDsrAxisFactorMilli(unsigned i)
{
    static const unsigned f[kNvrmDsrFactors] = { 1095, 1225, 1333, 1414, 1500, 1732, 2000 };
    return i < kNvrmDsrFactors ? f[i] : 0;
}
// The desktop size for factor i on a native raster, each axis rounded to an even number; false when it exceeds the scaler cap.
static inline bool nvrmDsrSize(unsigned nativeW, unsigned nativeH, unsigned i, unsigned maxW, unsigned maxH, unsigned *w, unsigned *h)
{
    const unsigned f = nvrmDsrAxisFactorMilli(i);
    if (!f || !nativeW || !nativeH) return false;
    const unsigned dw = (unsigned)(((unsigned long long)nativeW * f + 500) / 1000 + 1) & ~1u, dh = (unsigned)(((unsigned long long)nativeH * f + 500) / 1000 + 1) & ~1u;
    if (dw > maxW || dh > maxH || dw <= nativeW || dh <= nativeH) return false;
    *w = dw; *h = dh; return true;
}
// Highest pixel clock published in IOFBTimingRange. 1.2 GHz excluded the very modes NVKMS lists for fast panels (a 2560x1600
// 300 Hz eDP panel needs 1.37 GHz, a 4K 240 Hz DSC mode 2.1 GHz); macOS then validated them against this range rather than
// as the panel's own timing (issue 429: the mode is enumerated as valid, the panel goes black). NVKMS still validates every
// commit, so a higher bound only stops macOS from refusing or rewriting timings the hardware takes.
enum { kNvrmMaxPixelClockMHz = 2700 };
// nvfbhotplugpoll=<seconds>: valid periods are 1 to 60.
static inline bool nvrmHotplugPollValid(unsigned sec) { return sec >= 1 && sec <= 60; }
// After a mode change the head must keep showing the frame WindowServer last presented. That frame lives in a surface of the
// accelerator's (zero-copy flips), not in the scanout memory; a mode set that attaches the empty scanout surface shows black
// until WindowServer presents again, and a static desktop does not (issues 153, 161: cursor-only desktop after a refresh-rate
// change, fixed by anything that makes WindowServer redraw). The front surface is carried through when the raster, the desktop
// size and the rotation are unchanged, as for a refresh-rate-only change.
static inline bool nvrmKeepFrontSurface(bool enabled, bool haveFront, bool frontIsScanout, unsigned oldW, unsigned oldH, unsigned newW, unsigned newH,
                                        unsigned oldTransform, unsigned newTransform)
{
    return enabled && haveFront && !frontIsScanout && oldW == newW && oldH == newH && oldTransform == newTransform;
}
// ---- Rotated desktops of other sizes (issue 138: no resolution change once rotated) ----
// The refusal of rotated scaled desktops is not a hardware limit: WindowServer composites the turned desktop into raster-oriented
// frames in its GPU pass (the rotation commit says so) and CoreDisplay's own scaled-mode generator makes only mirror-only rotated
// products, so a rotated head offers just the driver's own modes. NVKMS confirms the display engine is not involved: its layer
// code (nvkms-evo3.c, rotation loop) skips 90 and 270 degrees because SCAN_COLUMN rotation "hasn't been added", so the engine only
// ever scans an unrotated raster-sized frame (0 or 180 degrees) and any scaling is the ordinary head scaler. nvfbrotscale=1 makes the driver offer, next to each rotated
// native-raster mode, turned desktops at 80%, 2/3 and 50% of the raster.
enum { kNvrmRotFactors = 3 };
static inline bool nvrmRotatedDesktop(unsigned turnedW, unsigned turnedH, unsigned i, unsigned *w, unsigned *h)
{
    static const unsigned f[kNvrmRotFactors] = { 800, 667, 500 };
    if (i >= kNvrmRotFactors || !turnedW || !turnedH) return false;
    const unsigned dw = ((unsigned)((unsigned long long)turnedW * f[i] / 1000)) & ~1u, dh = ((unsigned)((unsigned long long)turnedH * f[i] / 1000)) & ~1u;
    if (dw < 640 || dh < 480) return false;
    *w = dw; *h = dh; return true;
}
// A rotated scaled desktop is acceptable when it is no larger than the turned raster (nvfbrotscale=1).
static inline bool nvrmRotatedDesktopOk(bool enabled, unsigned deskW, unsigned deskH, unsigned turnedW, unsigned turnedH)
{
    return deskW == turnedW && deskH == turnedH ? true : (enabled && deskW && deskH && deskW <= turnedW && deskH <= turnedH && !(deskW & 1) && !(deskH & 1));
}
// ---- Surround: several heads, one logical display (nvfbsurround) ----
// Tiled 5K/6K panels work this way on a Mac: one framebuffer with a raster as wide as the tiles together, each head scanning its
// slice of the same surface. Members sit side by side in index order and must run the same height and the same refresh rate.
struct NvrmSurMode { unsigned w, h, hz; };
enum { kNvrmSurMaxMembers = 4, kNvrmSurMaxModes = 64 };
// Choose one mode per member: the tallest height every member has, then the highest refresh rate every member has at that height
// (within 1 Hz), and for each member the widest mode at that height and rate. Returns false when the members share nothing.
static inline bool nvrmSurroundPlan(const NvrmSurMode modes[][kNvrmSurMaxModes], const unsigned *count, unsigned members,
                                    unsigned *pick, unsigned *totalW, unsigned *height, unsigned *hz)
{
    if (members < 2 || members > kNvrmSurMaxMembers) return false;
    unsigned bestH = 0, bestHz = 0;
    for (unsigned a = 0; a < count[0]; a++) {
        const unsigned h = modes[0][a].h, r = modes[0][a].hz;
        if (h < bestH || (h == bestH && r <= bestHz)) continue;
        bool all = true;
        for (unsigned m = 1; m < members && all; m++) {
            bool has = false;
            for (unsigned b = 0; b < count[m] && !has; b++) has = modes[m][b].h == h && modes[m][b].hz + 1 >= r && modes[m][b].hz <= r + 1;
            all = has;
        }
        if (all) { bestH = h; bestHz = r; }
    }
    if (!bestH) return false;
    unsigned sum = 0;
    for (unsigned m = 0; m < members; m++) {
        int at = -1; unsigned bw = 0;
        for (unsigned b = 0; b < count[m]; b++)
            if (modes[m][b].h == bestH && modes[m][b].hz + 1 >= bestHz && modes[m][b].hz <= bestHz + 1 && modes[m][b].w > bw) { bw = modes[m][b].w; at = (int)b; }
        if (at < 0) return false;
        pick[m] = (unsigned)at; sum += bw;
    }
    *totalW = sum; *height = bestH; *hz = bestHz;
    return true;
}
// Members from a bit mask of framebuffer indices: the master is the lowest index, the rest are members that do not start as their
// own framebuffer. Only indices below `connected` count.
static inline unsigned nvrmSurroundMembers(unsigned mask, unsigned connected, unsigned *idx)
{
    unsigned n = 0;
    for (unsigned i = 0; i < 8 && i < connected && n < kNvrmSurMaxMembers; i++) if (mask & (1u << i)) idx[n++] = i;
    return n;
}
// The slice offset of member m: the widths of the members before it.
static inline unsigned nvrmSurroundOffset(const unsigned *widths, unsigned m) { unsigned o = 0; for (unsigned i = 0; i < m; i++) o += widths[i]; return o; }
// Boot with nothing connected (issue 88: a Dell on DisplayPort, every AUX read of the first query timed out within 14 ms of the
// firmware coming up; the same card on HDMI was fine): ask again a few times, 3 s apart, through the hot-plug scan (which
// starts a framebuffer for a display that appears). 6 tries, then it stays quiet like before. 0 = stop.
enum { kNvrmBootRetries = 6 };
static inline unsigned nvrmBootRetryDelayMs(unsigned attempt) { return attempt < kNvrmBootRetries ? 3000u : 0u; }
// The master of a surround mask is its lowest connected index; a member is any other connected index in the mask.
static inline unsigned nvrmSurroundMaster(unsigned mask, unsigned connected) { for (unsigned i = 0; i < 8 && i < connected; i++) if (mask & (1u << i)) return i; return 8; }
static inline bool nvrmSurroundIsMember(unsigned mask, unsigned index, unsigned connected)
{
    unsigned idx[kNvrmSurMaxMembers];
    return nvrmSurroundMembers(mask, connected, idx) >= 2 && index != nvrmSurroundMaster(mask, connected) && index < 8 && (mask & (1u << index));
}
// What a hot-plug scan does to a group. `total` = members in the mask, `up` = how many of them are connected now.
//   Formed and a member went away      -> Degrade: every display is its own again (members un-hidden).
//   Degraded and all members are back  -> Reform: members hidden, the master offers the combined mode again.
enum NvrmSurAction { kSurNone = 0, kSurDegrade = 1, kSurReform = 2 };
static inline NvrmSurAction nvrmSurroundNext(bool formed, bool degraded, unsigned total, unsigned up)
{
    if (formed && up < total) return kSurDegrade;
    if (degraded && total >= 2 && up == total) return kSurReform;
    return kSurNone;
}
