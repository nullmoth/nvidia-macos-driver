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

#endif
