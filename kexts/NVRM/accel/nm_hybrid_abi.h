/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Level 2 of the hybrid-laptop path (nm_hybrid.h): the call NVAccel makes on another GPU's framebuffer to scan out a
 * composited surface without copying it. The framebuffer owns the display engine and its translation table; NVAccel owns
 * the surface's memory and keeps it wired for as long as the framebuffer holds pages of it.
 *
 * Called through IOService::callPlatformFunction(NMH_FN_IMPORT, false, &request, ...) on the framebuffer. A framebuffer
 * that does not know the symbol returns an error and NVAccel copies the frame instead.
 *
 * One frame is: QUERY (may it be imported, which slot), MAP when the slot needs the surface's pages, FLIP. HOME is sent
 * before the first copied frame after any imported one, because the copy path writes into the boot surface.
 */
#pragma once

#define NMH_FN_IMPORT      "nmHybridImport"
#define NMH_ABI_VERSION    1u

#define NMH_OP_QUERY 1u
#define NMH_OP_MAP   2u
#define NMH_OP_FLIP  3u
#define NMH_OP_HOME  4u

struct NMHybridReq {
    unsigned int       version;     /* NMH_ABI_VERSION */
    unsigned int       op;
    unsigned int       surfaceId;
    unsigned int       format;      /* pixel format fourcc */
    unsigned int       width, height, pitch;
    int                slot;        /* QUERY out; MAP and FLIP in */
    unsigned int       needMap;     /* QUERY out: 1 when the slot must be mapped before it is flipped to */
    unsigned int       evictedId;   /* QUERY out: the surface that lost the slot (release what was held for it), or 0 */
    unsigned int       npages;      /* MAP in: number of 4 KiB pages */
    const unsigned long long *pages;/* MAP in: bus address of each page */
    int                result;      /* out: enum nmh_why, 0 = ok */
};
