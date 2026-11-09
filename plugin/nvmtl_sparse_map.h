/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_sparse_map.h - the arithmetic of sparse texture mapping (NVMTL_SPARSE_TEXTURES=1): which tile blocks a region covers, which
 * mip levels live in the mip tail, and a page allocator over a sparse heap. Pure, so tools/test_sparse_map.c proves it offline. */
#ifndef NVMTL_SPARSE_MAP_H
#define NVMTL_SPARSE_MAP_H
#include <stdint.h>
#include <string.h>
#include "nvmtl_sparse_query.h"

typedef struct { uint32_t tx, ty, tz;     /* tile index at the level */
                 uint32_t x, y, z;        /* texel offset of the block */
                 uint32_t w, h, d; } nvmtl_sparse_tile;   /* texel extent, clipped at the level's edge */

static inline uint32_t nvmtl_sparse_dim(uint32_t base, uint32_t level) { const uint32_t v = level < 32 ? base >> level : 0u; return v ? v : 1u; }

/* The first mip level in the mip tail: the first whose extent is smaller than one block in any dimension (Vulkan's
   imageMipTailFirstLod for the standard block shapes). `levels` when none is. */
static inline uint32_t nvmtl_sparse_first_tail(uint32_t w, uint32_t h, uint32_t d, uint32_t levels, const uint32_t gran[3])
{
    for (uint32_t l = 0; l < levels; l++)
        if (nvmtl_sparse_dim(w, l) < gran[0] || nvmtl_sparse_dim(h, l) < gran[1] || nvmtl_sparse_dim(d, l) < gran[2]) return l;
    return levels;
}

/* Metal's rule for a mapping region: the origin is a multiple of the tile size, and the size is too unless the region runs to
   the level's edge. Returns NULL when valid, else the reason. */
static inline const char *nvmtl_sparse_region_refusal(uint32_t w, uint32_t h, uint32_t d, uint32_t level, uint32_t first_tail, const uint32_t gran[3],
                                                      const uint32_t org[3], const uint32_t size[3])
{
    const uint32_t ext[3] = { nvmtl_sparse_dim(w, level), nvmtl_sparse_dim(h, level), nvmtl_sparse_dim(d, level) };
    if (level >= first_tail) return "the level is in the mip tail, which is not mapped region by region";
    for (int i = 0; i < 3; i++) {
        if (!gran[i] || !size[i]) return "empty region or no tile size";
        if (org[i] % gran[i]) return "the region origin is not a multiple of the tile size";
        if (org[i] >= ext[i] || size[i] > ext[i] - org[i]) return "the region is outside the level";
        if ((size[i] % gran[i]) && org[i] + size[i] != ext[i]) return "the region size is not a multiple of the tile size and does not reach the level's edge";
    }
    return NULL;
}

/* the tile blocks of a valid region, x fastest; returns the count, or -1 when `cap` is too small */
static inline int nvmtl_sparse_tiles(uint32_t w, uint32_t h, uint32_t d, uint32_t level, const uint32_t gran[3], const uint32_t org[3], const uint32_t size[3],
                                     nvmtl_sparse_tile *out, int cap)
{
    const uint32_t ext[3] = { nvmtl_sparse_dim(w, level), nvmtl_sparse_dim(h, level), nvmtl_sparse_dim(d, level) };
    int n = 0;
    for (uint32_t z = org[2]; z < org[2] + size[2]; z += gran[2])
        for (uint32_t y = org[1]; y < org[1] + size[1]; y += gran[1])
            for (uint32_t x = org[0]; x < org[0] + size[0]; x += gran[0]) {
                if (n >= cap) return -1;
                nvmtl_sparse_tile *t = &out[n++];
                t->tx = x / gran[0]; t->ty = y / gran[1]; t->tz = z / gran[2]; t->x = x; t->y = y; t->z = z;
                t->w = gran[0] < ext[0] - x ? gran[0] : ext[0] - x; t->h = gran[1] < ext[1] - y ? gran[1] : ext[1] - y; t->d = gran[2] < ext[2] - z ? gran[2] : ext[2] - z;
            }
    return n;
}

/* A page allocator over a sparse heap: one bit per 64 KiB page, first fit from a rotating hint. */
typedef struct { uint64_t *bits; uint32_t npages, used, hint; } nvmtl_page_pool;
static inline uint32_t nvmtl_page_pool_words(uint32_t npages) { return (npages + 63u) / 64u; }
static inline void nvmtl_page_pool_init(nvmtl_page_pool *p, uint64_t *storage, uint32_t npages)
{ p->bits = storage; p->npages = npages; p->used = 0; p->hint = 0; memset(storage, 0, (size_t)nvmtl_page_pool_words(npages) * 8u); }
static inline int nvmtl_page_alloc(nvmtl_page_pool *p)
{
    if (p->used >= p->npages) return -1;
    for (uint32_t k = 0; k < p->npages; k++) {
        const uint32_t i = (p->hint + k) % p->npages;
        if (!(p->bits[i >> 6] & (1ull << (i & 63)))) { p->bits[i >> 6] |= 1ull << (i & 63); p->used++; p->hint = i + 1; return (int)i; }
    }
    return -1;
}
/* n consecutive free pages (a mip tail is one contiguous range); the first page index, or -1 */
static inline int nvmtl_page_alloc_run(nvmtl_page_pool *p, uint32_t n)
{
    if (!n || n > p->npages - p->used) return -1;
    uint32_t run = 0;
    for (uint32_t i = 0; i < p->npages; i++) {
        if (p->bits[i >> 6] & (1ull << (i & 63))) { run = 0; continue; }
        if (++run == n) {
            const uint32_t first = i + 1 - n;
            for (uint32_t k = first; k <= i; k++) p->bits[k >> 6] |= 1ull << (k & 63);
            p->used += n; return (int)first;
        }
    }
    return -1;
}
/* 1 when the page was in use and is free now */
static inline int nvmtl_page_free(nvmtl_page_pool *p, uint32_t i)
{
    if (i >= p->npages || !(p->bits[i >> 6] & (1ull << (i & 63)))) return 0;
    p->bits[i >> 6] &= ~(1ull << (i & 63)); p->used--; return 1;
}
/* the pages one tile block takes: a standard block is exactly one page */
#define NVMTL_SPARSE_PAGES_PER_BLOCK 1u

/* sparseTextureTier: Tier2 reads a non-resident tile as zero and can report residency to the shader; Vulkan's
   residencyNonResidentStrict gives the zero read, and shader residency (sparseResidency in the SPIR-V sense) is not exposed through
   this translator, so the tier is 1 whenever the strict zero read holds and 0 (not sparse) otherwise. */
static inline uint32_t nvmtl_sparse_tier(int non_resident_strict) { return non_resident_strict ? 1u : 0u; }
#endif
