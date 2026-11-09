/* BAR address limits for IOKit physical memory descriptors. */
#ifndef NVRM_BAR_LIMITS_H
#define NVRM_BAR_LIMITS_H

// XNU IOGeneralMemoryDescriptor::initWithOptions rejects a non-remote physical
// range when ceil((address + length) / PAGE_SIZE) exceeds UINT_MAX. This is
// independent of CPUID MAXPHYADDR. Leave the final page outside the aperture.
static inline unsigned long long nvrmBarAddressMax(unsigned cpuBits, unsigned pageShift)
{
    if (!cpuBits || pageShift >= 32) return 0;
    const unsigned long long cpuMax = cpuBits >= 64 ? ~0ULL : (1ULL << cpuBits) - 1;
    const unsigned long long ioMax = (0xffffffffULL << pageShift) - 1;
    return cpuMax < ioMax ? cpuMax : ioMax;
}

static inline bool nvrmClipBarWindow(unsigned long long minimum,
                                     unsigned long long maximum,
                                     unsigned long long addressMax,
                                     unsigned long long *clippedMax)
{
    if (maximum < minimum || minimum > addressMax) return false;
    *clippedMax = maximum < addressMax ? maximum : addressMax;
    return *clippedMax > minimum;
}

#endif
