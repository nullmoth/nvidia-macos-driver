/* Recovering a BAR that IOPCIFamily published no aperture for (a laptop without Above 4G Decoding or Resizable BAR in
 * its BIOS leaves the device memory list empty while the BARs themselves are programmed). Pure functions, no kernel
 * calls, so tools/test_bar_recover.py can compile and exercise them on a host. */
#ifndef NVRM_BAR_RECOVER_H
#define NVRM_BAR_RECOVER_H

// The "assigned-addresses" property is a list of 5-cell entries: phys.hi, phys.mid, phys.lo, size.hi, size.lo. The low
// byte of phys.hi is the BAR's config-space register offset, bits 24-25 its space (2 = 32-bit memory, 3 = 64-bit memory).
// Returns false when the register has no memory entry with a non-zero address and size.
static inline bool nvrmAssignedLookup(const unsigned *cells, unsigned nCells, unsigned regOff,
                                      unsigned long long *addr, unsigned long long *size)
{
    for (unsigned i = 0; cells && i + 5 <= nCells; i += 5) {
        const unsigned space = (cells[i] >> 24) & 3;
        if ((cells[i] & 0xff) != regOff || space < 2) continue;
        const unsigned long long a = ((unsigned long long)cells[i + 1] << 32) | cells[i + 2];
        const unsigned long long s = ((unsigned long long)cells[i + 3] << 32) | cells[i + 4];
        if (!a || !s) continue;
        *addr = a; *size = s;
        return true;
    }
    return false;
}

// The size a BAR reports after all ones were written to it (PCI 6.2.5.1). lo and hi are the read-back values; hi is
// ignored for a 32-bit BAR. A BAR that reads back no size bits, or an I/O BAR, has size 0.
static inline unsigned long long nvrmBarSizeFromProbe(unsigned lo, unsigned hi, bool is64)
{
    if (lo & 1) return 0;
    unsigned long long mask = lo & ~0xFULL;
    if (is64) mask |= (unsigned long long)hi << 32;
    else mask |= 0xffffffff00000000ULL;
    if (!(mask & ~0xFULL) || mask == ~0ULL) return 0;
    return ~mask + 1;
}

#endif
