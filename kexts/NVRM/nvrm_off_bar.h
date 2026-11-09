/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// Driver off on a Resizable BAR card: BAR1 lost. IOPCIFamily can fail to size a large 64-bit BAR (RTX 5060 test: BAR1's upper
// half was written as a 16-byte BAR of its own and the firmware's screen range went to another device), so the firmware
// framebuffer address no longer lies in any aperture of the card and Apple's own framebuffer driver takes the card with the
// stale address. Switch boot-arg nvoffbar=1 repairs it: NVRM, even with the driver off, runs only its BAR1 placement (the
// code every driver start runs), publishes "nvrm-bar1-placed" on the card's PCI nub and asks for matching again; NVRMBootFB then
// finds the screen at the start of the placed BAR1. Nothing in matching ever waits on anything: both sides only read properties.
// One copy lives in kexts/NVRM and one in kexts/NVRMFB; tools/test_off_bar.py checks they match.
#pragma once
// A 64-bit memory BAR whose low and high config dwords name an address that no aperture of the nub covers is lost.
// ap[i] = {base, length} of the nub's apertures; apertures of 16 bytes or less are the stray BAR IOPCIFamily wrote.
struct NvrmApertureSpan { unsigned long long base, length; };
static inline bool nvrmBar64Lost(unsigned lo, unsigned hi, const NvrmApertureSpan *ap, unsigned n)
{
    if ((lo & 1) || ((lo >> 1) & 3) != 2) return false;          // I/O or 32-bit: not this failure
    const unsigned long long a = ((unsigned long long)hi << 32) | (lo & ~0xFull);
    if (!a) return true;                                         // a 64-bit BAR with no address at all
    for (unsigned i = 0; i < n; i++) if (ap[i].base == a && ap[i].length > 16) return false;
    return true;
}
