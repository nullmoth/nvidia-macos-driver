/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * The geometry check follows PrimeBridge's BootFramebuffer::attach (https://github.com/myvoow/PrimeBridge),
 * MIT License, Copyright (c) 2026 myvoow. The MIT notice is kept in NOTICE and in gopfb.cpp.
 */

/*
 * NMGopFB decisions in plain C with no kernel headers, so tools/test_gopfb.c runs the same code on the host.
 *
 *  1. which panel driver takes a GPU (nmgop_select): NMIntelFB for the Intel display versions it knows, NMGopFB for
 *     the AMD integrated and RDNA 3/4 GPUs macOS has no driver for, nobody for anything else;
 *  2. whether the framebuffer the firmware left behind may be written (nmgop_geometry): 32 bits per pixel, a sane
 *     pitch, and wholly inside one of the GPU's memory BARs. Anything else is refused and the firmware keeps the screen.
 */
#ifndef NM_GOPFB_H
#define NM_GOPFB_H

#include <stdint.h>

/* ---------------------------------------------------------------------------------------------------------------------
 * selection
 */
enum nmgop_sel { NMGOP_DECLINE = 0, NMGOP_USE_GOPFB = 1, NMGOP_USE_INTELFB = 2 };

#define NMGOP_VENDOR_AMD   0x1002u
#define NMGOP_VENDOR_INTEL 0x8086u

/* AMD GPUs macOS has no driver for: every integrated GPU of the Ryzen generations with RDNA 2 or later graphics, and the
 * discrete RDNA 3 and RDNA 4 cards (ranges as in the 1401 scan classification, p1401/amd_gpu.py, which takes them from the
 * Linux amdgpu tables). Vega and older integrated GPUs, Polaris, Vega and Navi 1x/2x cards have an Apple driver and are
 * never listed. */
static inline int nmgop_amd_has_no_apple_driver(uint16_t dev)
{
    switch (dev) {
    case 0x163F: case 0x1506: case 0x1681: case 0x164E: case 0x13C0:   /* Van Gogh, Mendocino, Rembrandt, Raphael, Granite Ridge */
    case 0x15BF: case 0x15C8: case 0x1900:                             /* Phoenix, Phoenix 2, Hawk Point */
    case 0x150E: case 0x1586:                                          /* Strix Point, Strix Halo */
        return 1;
    default: break;
    }
    if (dev >= 0x7448 && dev <= 0x745F) return 1;   /* Navi 31 */
    if (dev >= 0x7470 && dev <= 0x749F) return 1;   /* Navi 32, Navi 33 */
    if (dev >= 0x7550 && dev <= 0x756F) return 1;   /* Navi 48 */
    if (dev >= 0x7590 && dev <= 0x75AF) return 1;   /* Navi 44 */
    return 0;
}

/* intel_display_version: the display version NMIntelFB knows for this device ID (12 to 20), or 0.
 * nmxgpu_present: the AMD / Intel test driver (NMXGPU) is loaded and owns the GPU, so no second driver may touch it.
 * override_dev: boot-arg nmgopid=0xVVVVDDDD, a tester naming a GPU this table does not know; 0 = none. */
static inline int nmgop_select(uint16_t vendor, uint16_t dev, unsigned intel_display_version, int nmxgpu_present,
                               uint32_t override_dev)
{
    if (nmxgpu_present) return NMGOP_DECLINE;
    if (vendor == NMGOP_VENDOR_INTEL && intel_display_version >= 12 && intel_display_version <= 20) return NMGOP_USE_INTELFB;
    if (vendor == NMGOP_VENDOR_AMD && nmgop_amd_has_no_apple_driver(dev)) return NMGOP_USE_GOPFB;
    if (override_dev && (override_dev >> 16) == vendor && (override_dev & 0xFFFFu) == dev && vendor != 0x10DEu)
        return NMGOP_USE_GOPFB;
    return NMGOP_DECLINE;
}

/* ---------------------------------------------------------------------------------------------------------------------
 * geometry of the firmware framebuffer (PE_Video: v_baseAddr, v_width, v_height, v_rowBytes, v_depth)
 */
enum nmgop_why {
    NMGOP_OK = 0,
    NMGOP_E_DEPTH,     /* not 32 bits per pixel: every copy path writes 8 bits per channel */
    NMGOP_E_EMPTY,     /* no base address or a zero dimension */
    NMGOP_E_PITCH,     /* rowBytes below width * 4, or not a multiple of 4 */
    NMGOP_E_SIZE,      /* absurd size (over 16384 in either dimension, or over 1 GiB in all) */
    NMGOP_E_OUTSIDE    /* not wholly inside any memory BAR of this GPU */
};

static inline const char *nmgop_why_name(int w)
{
    switch (w) {
    case NMGOP_OK:      return "ok";
    case NMGOP_E_DEPTH: return "firmware framebuffer is not 32 bits per pixel";
    case NMGOP_E_EMPTY: return "no firmware framebuffer";
    case NMGOP_E_PITCH: return "row pitch is below width * 4 or not a multiple of 4";
    case NMGOP_E_SIZE:  return "framebuffer size out of range";
    default:            return "framebuffer is not inside a memory BAR of this GPU";
    }
}

struct nmgop_bar { uint64_t phys, len; };

struct nmgop_geom {
    uint64_t phys;        /* base address with the XNU flag bits removed */
    uint64_t flags;       /* those bits (b0 marks a physical address) */
    uint64_t bytes;       /* rowBytes * height */
    unsigned bar;         /* index of the BAR that holds it */
    uint64_t bar_offset;  /* phys - bar.phys */
    uint32_t width, height, pitch;
};

/* v_baseAddr bit 0 is a flag and bits 1:0 are removed exactly as XNU does (v_baseAddr & ~3). Masking more (a page mask)
 * moves the base by up to 4 KiB and shifts every pixel's colour channels; masking less moves it by a byte. */
static inline int nmgop_geometry(uint64_t base_raw, uint64_t width, uint64_t height, uint64_t row_bytes, uint64_t depth,
                                 const struct nmgop_bar *bars, unsigned nbars, struct nmgop_geom *out)
{
    if (depth != 32) return NMGOP_E_DEPTH;
    if (!base_raw || !width || !height) return NMGOP_E_EMPTY;
    if (width > 16384 || height > 16384) return NMGOP_E_SIZE;
    if (row_bytes > (1ull << 30)) return NMGOP_E_SIZE;  /* before the product below can wrap */
    if (row_bytes < width * 4 || (row_bytes & 3)) return NMGOP_E_PITCH;
    const uint64_t bytes = row_bytes * height;          /* both fit in 32 bits here, so no overflow in 64 */
    if (bytes > (1ull << 30)) return NMGOP_E_SIZE;
    const uint64_t phys = base_raw & ~3ull;
    for (unsigned i = 0; i < nbars; i++) {
        const uint64_t b = bars[i].phys, l = bars[i].len;
        if (!l || phys < b) continue;
        const uint64_t off = phys - b;
        if (off > l || bytes > l - off) continue;       /* written so that b + l and phys + bytes never wrap */
        out->phys = phys; out->flags = base_raw & 3ull; out->bytes = bytes; out->bar = i; out->bar_offset = off;
        out->width = (uint32_t)width; out->height = (uint32_t)height; out->pitch = (uint32_t)row_bytes;
        return NMGOP_OK;
    }
    return NMGOP_E_OUTSIDE;
}

#endif
