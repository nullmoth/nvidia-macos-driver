/*
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
// How much scanout memory a head reserves when it starts, and whether a monitor identified later fits in it.
//
// The reservation used to follow the mode list of whatever EDID the display link returned first. After a warm restart a
// DisplayPort monitor can still be waking up at that moment: the link then reports a placeholder 1920x1080 display, the
// head reserved memory for 1920x1200, and when the real EDID arrived five seconds later (3440x1440 in the report) its
// modes no longer fitted, so the desktop stayed at 1920x1080 until the next cold start.
// The reservation now has a floor that does not depend on the first EDID: a 3840x2160 desktop (about 32 MB per
// connected head). A monitor identified later keeps its own ceilings whenever its largest desktop fits in the memory
// that was really allocated. Pure, so tools/test_scanout_reserve.py runs it on the host.
#pragma once
enum { kNvrmFbReserveFloorW = 3840, kNvrmFbReserveFloorH = 2160 };
// Bytes a w x h desktop of 32-bit pixels needs with rows aligned to `align` (a power of two), rounded up to 64 KB as the
// allocation is. 0 when a dimension is 0 or the alignment is not a power of two.
static inline unsigned long long nvrmfb_scanout_bytes(unsigned w, unsigned h, unsigned align)
{
    if (!w || !h || !align || (align & (align - 1))) return 0;
    const unsigned long long pitch = ((unsigned long long)w * 4 + align - 1) & ~((unsigned long long)align - 1);
    return (pitch * h + 0xffffull) & ~0xffffull;
}
// What to reserve: the larger of what the mode list asks for, what the boot raster needs, and the floor.
static inline unsigned long long nvrmfb_scanout_reserve(unsigned long long modes, unsigned long long boot, unsigned align)
{
    unsigned long long r = modes > boot ? modes : boot;
    const unsigned long long floor_ = nvrmfb_scanout_bytes(kNvrmFbReserveFloorW, kNvrmFbReserveFloorH, align);
    return r > floor_ ? r : floor_;
}
// Whether a monitor whose largest desktop is w x h can be scanned out of `allocated` bytes.
static inline int nvrmfb_scanout_fits(unsigned w, unsigned h, unsigned align, unsigned long long allocated)
{
    const unsigned long long need = nvrmfb_scanout_bytes(w, h, align);
    return need && need <= allocated;
}
