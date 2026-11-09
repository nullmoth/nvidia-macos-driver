/* Request sizes for the zero-copy surface path on a small BAR1.
 *
 * On a 256 MB BAR1 every surface is a mapping in a window RM allocates from, and surfaces of many different sizes come
 * and go: a request of 8 MB failed at 217 MB of 243 MB in use with nothing parked (issues #364, #462) because the holes
 * left by freed surfaces no longer fit. Rounding each request up to a power of two between 512 KB and 16 MB leaves few
 * hole sizes, so a freed block fits the next request of its class. An offline model of those surface sizes
 * (tools/test_vram_class.py) refuses 43% fewer allocations with the bytes free but no contiguous hole. Larger requests
 * keep their size. A cache of freed blocks was modelled too and made it worse, so there is none. */
#ifndef NVRM_VRAM_CLASS_H
#define NVRM_VRAM_CLASS_H

static inline unsigned long long nvrmVramSizeClass(unsigned long long size)
{
    const unsigned long long kMin = 512ULL << 10, kMax = 16ULL << 20;
    if (size > kMax) return size;
    unsigned long long c = kMin;
    while (c < size) c <<= 1;
    return c;
}

#endif
