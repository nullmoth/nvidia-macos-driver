/* PCIe ASPM L1 for the GPU's link. Pure functions, no kernel calls, so tools/test_aspm.py can compile and exercise them
 * on a host.
 *
 * Linux leaves ASPM to the PCI core (pcie_aspm.c): L1 is enabled on a link only when both ends advertise it and the
 * exit latency the path adds is within what the endpoint says it can accept. macOS applies ASPM from a device property
 * at attach time and hackintosh boards often pass nothing, so the GPU's link idled in L0 for ever and the card drew
 * its full idle power. This is the same decision, taken once the card is up.
 *
 * Link Capabilities (offset 0x0c in the PCIe capability): bits 11:10 ASPM support (1 = L0s, 2 = L1, 3 = both),
 * bits 17:15 L1 exit latency. Device Capabilities (0x04): bits 11:9 acceptable L1 latency, 7 = no limit.
 * Link Control (0x10): bits 1:0 ASPM control (1 = L0s, 2 = L1). Latency codes 0..7 mean <1, 1-2, 2-4, 4-8, 8-16, 16-32,
 * 32-64 and >64 microseconds. */
#ifndef NVRM_ASPM_H
#define NVRM_ASPM_H

struct NvrmAspmEnd { bool present; unsigned devCap, lnkCap, lnkCtl; };

enum NvrmAspmWhy { kAspmEnabled = 0, kAspmNoCapability, kAspmEndpointNoL1, kAspmUpstreamNoL1, kAspmLatency, kAspmAlready };

// Decides whether to set the L1 bit on both ends. On kAspmEnabled the new Link Control values are in *epCtl and *upCtl;
// the other bits of each register are kept as they are (L0s is never turned on here).
static inline NvrmAspmWhy nvrmAspmPlan(const NvrmAspmEnd *ep, const NvrmAspmEnd *up, unsigned *epCtl, unsigned *upCtl)
{
    if (!ep || !up || !ep->present || !up->present) return kAspmNoCapability;
    if (!(((ep->lnkCap >> 10) & 3) & 2)) return kAspmEndpointNoL1;
    if (!(((up->lnkCap >> 10) & 3) & 2)) return kAspmUpstreamNoL1;
    const unsigned epExit = (ep->lnkCap >> 15) & 7, upExit = (up->lnkCap >> 15) & 7;
    const unsigned worst = epExit > upExit ? epExit : upExit;
    const unsigned accept = (ep->devCap >> 9) & 7;
    if (accept != 7 && worst > accept) return kAspmLatency;
    if ((ep->lnkCtl & 2) && (up->lnkCtl & 2)) return kAspmAlready;
    *epCtl = ep->lnkCtl | 2u; *upCtl = up->lnkCtl | 2u;
    return kAspmEnabled;
}

static inline const char *nvrmAspmWhyString(NvrmAspmWhy w)
{
    switch (w) {
        case kAspmEnabled: return "L1 enabled";
        case kAspmNoCapability: return "no PCIe capability on the card or its upstream port";
        case kAspmEndpointNoL1: return "the card does not support L1";
        case kAspmUpstreamNoL1: return "the upstream port does not support L1";
        case kAspmLatency: return "L1 exit latency is more than the card accepts";
        case kAspmAlready: return "L1 was already on (firmware)";
    }
    return "?";
}

// "none" without the capability, "on" when the firmware sized BAR1 above 256 MB, "off" otherwise
static inline const char *nvrmRebarState(bool capability, unsigned long long firmwareBytes)
{
    if (!capability) return "none";
    return firmwareBytes > (256ULL << 20) ? "on" : "off";
}

#endif
