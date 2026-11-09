/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_yuv422.h - Metal's packed 4:2:2 pixel formats MTLPixelFormatGBGR422 (240) and MTLPixelFormatBGRG422 (241).
 *
 * Two pixels share one chroma pair and occupy four bytes:
 *   GBGR422: bytes G0 B G1 R      BGRG422: bytes B G0 R G1
 * Vulkan offers the same packings only behind a sampler Y'CbCr conversion, which cannot live in this driver's bindless
 * sampler array. The texture is instead kept as a plain RGBA8 image at full width, holding what Metal's sampler returns
 * (R, G, B = the packed red, green and blue values, alpha 1), and the exact packed bytes are kept beside it so
 * getBytes returns what the application wrote.
 *
 * The reconstruction below was measured on an Apple GPU (a compute kernel sampling and read()ing a 8x1 texture, nearest
 * and linear filtering: all three agree at texel centres). A pair's chroma sits halfway between its two pixels, so the
 * pixel x has chroma  3/4 of its own pair + 1/4 of the neighbouring pair on its side (the previous pair for an even x,
 * the next pair for an odd x), clamped at the row ends. Luma is the pixel's own byte. Values round half up.
 */
#ifndef NVMTL_YUV422_H
#define NVMTL_YUV422_H
#include <stddef.h>
#include <stdint.h>

#define NVMTL_PF_GBGR422 240
#define NVMTL_PF_BGRG422 241

static inline int nvmtl_y422_is_format(uint32_t mtl_format) { return mtl_format == NVMTL_PF_GBGR422 || mtl_format == NVMTL_PF_BGRG422; }

/* byte offsets inside a 4-byte pair */
static inline void nvmtl_y422_layout(uint32_t fmt, int *g0, int *g1, int *b, int *r)
{
    if (fmt == NVMTL_PF_GBGR422) { *g0 = 0; *b = 1; *g1 = 2; *r = 3; }
    else                         { *b = 0; *g0 = 1; *r = 2; *g1 = 3; }
}

static inline uint8_t nvmtl_y422_mix(uint8_t own, uint8_t other) { return (uint8_t)((3u * own + other + 2u) >> 2); }

/* Decodes one row of `w` pixels (w even) from packed bytes into RGBA8. */
static inline void nvmtl_y422_decode_row(uint32_t fmt, const uint8_t *src, uint32_t w, uint8_t *rgba)
{
    int g0, g1, b, r; nvmtl_y422_layout(fmt, &g0, &g1, &b, &r);
    const uint32_t pairs = w / 2;
    for (uint32_t x = 0; x < w; x++) {
        const uint32_t k = x / 2, odd = x & 1u;
        const uint8_t *own = src + 4u * k;
        const uint8_t *nb = src + 4u * (odd ? (k + 1 < pairs ? k + 1 : k) : (k > 0 ? k - 1 : k));
        uint8_t *o = rgba + 4u * x;
        o[0] = nvmtl_y422_mix(own[r], nb[r]);
        o[1] = own[odd ? g1 : g0];
        o[2] = nvmtl_y422_mix(own[b], nb[b]);
        o[3] = 255;
    }
}

/* the bytes of a packed row of w pixels */
static inline size_t nvmtl_y422_row_bytes(uint32_t w) { return (size_t)w * 2u; }

/* 1 when a region can be written in place: Metal's 4:2:2 regions are whole pairs */
static inline int nvmtl_y422_region_ok(uint32_t x, uint32_t w) { return (x & 1u) == 0 && (w & 1u) == 0; }
#endif
