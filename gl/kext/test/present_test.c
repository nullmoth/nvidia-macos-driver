/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * Host test of nm_present.h. The scanout sits between two guard areas; every arm checks the guards afterwards,
 * so a copy that leaves the scanout fails the arm that caused it. NM_PRESENT_NO_CLIP builds the control: the
 * same test against a copy with the clipping removed must fail.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../nm_present.h"

#define DW 16u
#define DH 8u
#define DPITCH 80u            /* wider than DW * 4: the padding must stay untouched */
#define GUARD 4096u
static uint8_t *area, *scan;
static int failed, arms;

#ifdef NM_PRESENT_NO_CLIP
static uint32_t copy(const uint8_t *src, uint32_t sw, uint32_t sh, uint32_t srow, int32_t sx, int32_t sy, int deep)
{
    (void)deep;
    for (uint32_t y = 0; y < sh; y++)
        memcpy(scan + (int64_t)(sy + (int32_t)y) * DPITCH + (int64_t)sx * 4, src + (uint64_t)y * srow, (size_t)sw * 4u);
    return sh;
}
#else
static uint32_t copy(const uint8_t *src, uint32_t sw, uint32_t sh, uint32_t srow, int32_t sx, int32_t sy, int deep)
{
    return nm_present_copy(src, sw, sh, srow, sx, sy, scan, DW, DH, DPITCH, (uint64_t)DPITCH * DH, deep);
}
#endif

static void reset(void) { memset(area, 0xEE, GUARD * 2 + DPITCH * DH); memset(scan, 0, DPITCH * DH); }
static int guards_ok(void)
{
    for (uint32_t i = 0; i < GUARD; i++) if (area[i] != 0xEE || scan[DPITCH * DH + i] != 0xEE) return 0;
    return 1;
}
static uint32_t px(uint32_t x, uint32_t y) { uint32_t v; memcpy(&v, scan + y * DPITCH + x * 4, 4); return v; }
static uint32_t count_set(void) { uint32_t n = 0; for (uint32_t y = 0; y < DH; y++) for (uint32_t x = 0; x < DPITCH / 4; x++) if (px(x, y)) n++; return n; }
static void arm(const char *name, int ok, long a, long b)
{
    arms++; if (!ok) failed++;
    printf("%s  %s  [%ld, %ld]\n", ok ? "PASS" : "FAIL", name, a, b);
}

int main(void)
{
    area = malloc(GUARD * 2 + DPITCH * DH); scan = area + GUARD;
    uint32_t sw = 24, sh = 12, srow = 100;
    uint8_t *src = malloc((size_t)srow * sh);
    for (uint32_t y = 0; y < sh; y++) for (uint32_t x = 0; x < sw; x++) { uint32_t v = 0xFF000000u | ((y + 1) << 8) | (x + 1); memcpy(src + y * srow + x * 4, &v, 4); }

    reset(); uint32_t n = copy(src, 4, 3, srow, 2, 1, 0);
    arm("a surface inside the scanout lands at its position, pixel for pixel", n == 3 && px(2, 1) == 0xFF000101u && px(5, 3) == 0xFF000304u && count_set() == 12 && guards_ok(), n, count_set());

    reset(); n = copy(src, sw, sh, srow, 0, 0, 0);
    arm("a surface larger than the scanout is cut at the right and bottom edges", n == DH && count_set() == DW * DH && px(DW - 1, DH - 1) == (0xFF000000u | (DH << 8) | DW) && guards_ok(), n, count_set());

    reset(); n = copy(src, sw, sh, srow, -5, -4, 0);
    arm("a surface that starts left of and above the scanout shows its inner part at 0,0", n == DH && px(0, 0) == 0xFF000506u && guards_ok(), n, px(0, 0));

    reset(); n = copy(src, sw, sh, srow, 12, 6, 0);
    arm("a surface hanging over the bottom-right corner writes only the overlap", n == 2 && count_set() == 4 * 2 && guards_ok(), n, count_set());

    reset(); n = copy(src, sw, sh, srow, 16, 0, 0); uint32_t m = copy(src, sw, sh, srow, 0, 8, 0);
    arm("a surface entirely to the right or below writes nothing", n == 0 && m == 0 && count_set() == 0 && guards_ok(), n, m);

    reset(); n = copy(src, sw, sh, srow, -24, 0, 0); m = copy(src, sw, sh, srow, 0, -12, 0);
    arm("a surface entirely to the left or above writes nothing", n == 0 && m == 0 && count_set() == 0 && guards_ok(), n, m);

    reset(); n = copy(src, sw, sh, srow, 2147483640, 2147483640, 0); m = copy(src, sw, sh, srow, -2147483647 - 1, -2147483647 - 1, 0);
    arm("positions at the ends of the integer range write nothing", n == 0 && m == 0 && count_set() == 0 && guards_ok(), n, m);

    reset(); n = copy(src, 4, 2, srow, 0, 0, 0);
    arm("row padding of the scanout is never written", n == 2 && px(DW, 0) == 0 && px(DPITCH / 4 - 1, 1) == 0 && guards_ok(), n, px(DW, 0));

#ifndef NM_PRESENT_NO_CLIP
    reset(); n = nm_present_copy(src, sw, sh, srow, 0, 0, scan, DW, DH, DPITCH, (uint64_t)DPITCH * DH - 1, 0);
    arm("a scanout whose rows do not fit its mapping is refused", n == 0 && count_set() == 0 && guards_ok(), n, 0);

    reset(); n = nm_present_copy(src, sw, sh, sw * 4 - 4, 0, 0, scan, DW, DH, DPITCH, (uint64_t)DPITCH * DH, 0);
    arm("a surface whose row is shorter than its width is refused", n == 0 && count_set() == 0 && guards_ok(), n, 0);

    reset(); uint32_t one = 0xFF80FF01u; n = nm_present_copy((const uint8_t *)&one, 1, 1, 4, 0, 0, scan, DW, DH, DPITCH, (uint64_t)DPITCH * DH, 1);
    uint32_t want = ((0x01u << 2 | 0) << 20) | (0x3FFu << 10) | ((0x80u << 2) | 2);
    arm("ten-bit scanout: full green stays full, blue takes the high field, red the low", n == 1 && px(0, 0) == want && guards_ok(), px(0, 0), want);
#endif
    printf("%d arms, %d failed\n", arms, failed);
    return failed ? 1 : 0;
}
