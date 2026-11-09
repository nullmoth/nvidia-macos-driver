/* Startup timing decisions shared by NVRM (auto-go) and NVAccel (headless check). Pure functions, no kernel calls,
 * so tools/test_startup_policy.py can compile and exercise them on a host. */
#ifndef NVRM_STARTUP_POLICY_H
#define NVRM_STARTUP_POLICY_H

// NVAccel starts as soon as NVRM claims the card, but the display heads only publish after auto-go has waited its
// settle delay (100 s on a card whose BAR1 could not be moved) and then run the bring-up. A headless deadline counted
// from NVAccel's start therefore fired before any head could exist, the Metal plugin name came back without arming,
// and WindowServer aborted every ten seconds. The deadline is the base wait added to the settle delay NVRM published.
// A settle value above five minutes is a typo in a boot argument, not a reason to withhold Metal indefinitely.
static inline unsigned nvrmHeadlessDeadlineSec(unsigned baseSec, unsigned settleMs)
{
    const unsigned kMaxSettleSec = 300;
    unsigned settleSec = (settleMs + 999u) / 1000u;
    if (settleSec > kMaxSettleSec) settleSec = kMaxSettleSec;
    return baseSec + settleSec;
}

// True when the display enumeration has finished on at least one connector list and none of them carries a display.
// counts[i] is the connected-displays value of nub i, or -1 when that nub has not published one yet. A nub that has
// not published is not evidence of an empty connector list, so it keeps the caller waiting.
static inline bool nvrmNoDisplayOnAnyConnector(const int *counts, unsigned n)
{
    bool seen = false;
    for (unsigned i = 0; i < n; i++) {
        if (counts[i] < 0) return false;
        seen = true;
        if (counts[i] > 0) return false;
    }
    return seen;
}

// Settle delay before auto-go brings the display up. nvrmsettle (milliseconds) overrides the default when present.
static inline unsigned nvrmAutoGoSettleMs(bool bar1Placed, bool turing, bool haveArg, unsigned argMs)
{
    unsigned ms = bar1Placed ? (turing ? 10000u : 500u) : 100000u;
    if (haveArg) ms = argMs;
    return ms;
}

// A firmware that sizes BAR1 above 8 GB (16 GB on an RTX 4060 Ti 16 GB, 32 GB on the largest cards) leaves a window
// the bring-up has not run on: an RTX 3060 and an RTX 5080 resized to 16 GB both stopped during bring-up, and 8 GB is
// the size proven on hardware. Returns the size to resize to (8 GB), or 0 to leave BAR1 as it is: already 8 GB or
// smaller, the card does not offer 8 GB (bit 13 of the supported-sizes mask, 2^(20+13) bytes), or the user asked to
// keep the firmware's size.
static inline unsigned long long nvrmBar1ShrinkTarget(unsigned long long currentSize, unsigned supportedMask, bool keepFirmwareSize)
{
    const unsigned long long k8GB = 8ULL << 30;
    if (keepFirmwareSize || currentSize <= k8GB) return 0;
    return ((supportedMask >> 13) & 1) ? k8GB : 0;
}

// auto-go holds the registry until the display stack is armed so WindowServer does not start on a half-armed GPU. With
// no head published and no display on any connector, WindowServer runs on another GPU, so the hold is released before
// arming and the arming still runs, which keeps a display plugged in later working. Any head published: arm, then release.
static inline bool nvrmReleaseBeforeArming(unsigned headMask, bool noDisplayAnywhere)
{
    return headMask == 0 && noDisplayAnywhere;
}

// GSP stall handling in the RM (kernel_gsp.c, _kgspIsRpcTimeoutFatal and _kgspCheckAndLogTimeout), modelled here so the
// decision can be tested. An RPC that times out is classed by the GSP's heartbeat: a dead heartbeat is GSP_ERROR (the GSP
// is wedged or crashed), a live one is GSP_RPC_TIMEOUT (GSP-RM is slow). A crash report from CrashCat is always fatal and
// is looked at on every poll. With timeout classification off, which is the RM default, a dead heartbeat is not fatal
// until the same RPC has timed out `threshold` times, so a wedged GSP kept WindowServer polling for 6 s per RPC until
// then (#454). With it on, a dead heartbeat is fatal at the first timeout and a slow GSP keeps the old count.
static inline bool nvrmGspTimeoutFatal(bool crashReport, bool classificationOn, bool heartbeatAlive, unsigned timeoutCount, unsigned threshold)
{
    if (crashReport) return true;
    if (timeoutCount == threshold) return true;
    if (!heartbeatAlive) return classificationOn;
    return false;
}

// With no 64-bit memory window in the host bridge's ACPI _CRS, BAR1 is placed from a fixed fallback range only when
// something above 4 GB is known to be routed. A BAR1 the firmware left below 4 GB on a host that decodes nothing above
// 4 GB (Above 4G Decoding off) is fine where it is, and moving it made a board start; but a card whose BAR1 must grow
// to 4 GB or more (a 64 MB laptop BAR1 resized to 8 GB) can only live above 4 GB, so it needs the fallback range when
// other PCI devices already sit up there. 2.6.3 refused every unplaced BAR1 below 4 GB, and a 64 MB BAR1 that 2.6.2
// had resized stayed at 64 MB: the desktop froze on a stale frame.
static inline bool nvrmFallbackWindowAllowed(unsigned long long firmwareBar1, unsigned long long resizeTo, bool above4GRouted)
{
    const unsigned long long k4GB = 4ULL << 30;
    if (firmwareBar1 >= k4GB) return true;
    return resizeTo >= k4GB && above4GRouted;
}

// The name the registry carries when the RM's released-chip table has no entry for a PCI id (a newer board revision):
// the generation and chip the RM itself reports, never a made-up retail name. snprintf semantics; returns the length.
#ifdef KERNEL
#include <libkern/libkern.h>
#else
#include <stdio.h>
#endif
static inline int nvrmFallbackModelName(char *out, unsigned long outLen, const char *gen, const char *chip, unsigned deviceId)
{
    return snprintf(out, outLen, "NVIDIA %s GPU (%s, 10DE:%04X)", gen, chip, deviceId & 0xffff);
}

#endif
