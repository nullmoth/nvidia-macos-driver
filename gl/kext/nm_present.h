/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * The copy of a surface into a scanout, kept free of kernel types so the same lines are compiled into the kext
 * and into a host test (test/present_test.c). Every bound is checked here: a wrong one is a write outside the
 * scanout mapping.
 */
#pragma once
#include <stdint.h>

/* Copies the part of a BGRA surface (sw x sh, srow bytes per row, placed at sx, sy) that lies inside a scanout
 * (dw x dh, dpitch bytes per row, dsize bytes mapped) and returns the number of rows written; 0 when nothing
 * overlaps or the scanout's own numbers do not fit its mapping. deep: the scanout holds ten bits per channel
 * with blue highest; each 8-bit channel is widened by repeating its top bits. */
static inline uint32_t nm_present_copy(const uint8_t *src, uint32_t sw, uint32_t sh, uint32_t srow, int32_t sx, int32_t sy,
                                       uint8_t *dst, uint32_t dw, uint32_t dh, uint32_t dpitch, uint64_t dsize, int deep)
{
    if (!src || !dst || sw == 0 || sh == 0 || dw == 0 || dh == 0) return 0;
    if ((uint64_t)sw * 4u > srow || (uint64_t)dw * 4u > dpitch || (uint64_t)dpitch * dh > dsize) return 0;
    int64_t x0 = sx < 0 ? 0 : sx, y0 = sy < 0 ? 0 : sy;
    int64_t x1 = (int64_t)sx + sw, y1 = (int64_t)sy + sh;
    if (x1 > (int64_t)dw) x1 = dw;
    if (y1 > (int64_t)dh) y1 = dh;
    if (x0 >= x1 || y0 >= y1) return 0;
    for (int64_t y = y0; y < y1; y++) {
        const uint8_t *from = src + (uint64_t)(y - sy) * srow + (uint64_t)(x0 - sx) * 4u;
        uint8_t *to = dst + (uint64_t)y * dpitch + (uint64_t)x0 * 4u;
        const uint32_t *f = (const uint32_t *)(const void *)from;
        uint32_t *t = (uint32_t *)(void *)to;
        for (int64_t x = 0; x < x1 - x0; x++) {
            uint32_t p = f[x];
            if (deep) {
                uint32_t b = p & 0xff, g = (p >> 8) & 0xff, r = (p >> 16) & 0xff;
                p = (((b << 2) | (b >> 6)) << 20) | (((g << 2) | (g >> 6)) << 10) | ((r << 2) | (r >> 6));
            }
            t[x] = p;
        }
    }
    return (uint32_t)(y1 - y0);
}
