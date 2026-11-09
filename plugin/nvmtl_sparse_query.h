/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_sparse_query.h - sparse texture tile sizes from the device's Vulkan sparse image granularity.
 * Vulkan's standard sparse block is 64 KiB; the device reports the texel shape of one block per format, image type and sample
 * count (imageGranularity). Metal's sparseTileSize is that shape, and sparseTileSizeInBytes is the block size. A device that
 * reports no sparse properties for a format has no tile size for it: the answer is (0, 0, 0), not a guess. */
#ifndef NVMTL_SPARSE_QUERY_H
#define NVMTL_SPARSE_QUERY_H
#include <stdint.h>

#define NVMTL_SPARSE_BLOCK_BYTES 65536u

/* 1 when a granularity of w x h x d texels of `bpp` bytes with `samples` samples is a whole standard 64 KiB block */
static inline int nvmtl_sparse_block_is_standard(uint32_t w, uint32_t h, uint32_t d, uint32_t bpp, uint32_t samples)
{
    if (!w || !h || !d || !bpp || !samples) return 0;
    const uint64_t bytes = (uint64_t)w * h * d * bpp * samples;
    return bytes == NVMTL_SPARSE_BLOCK_BYTES;
}
/* the sparse tile size Metal reports: the device's granularity when it reported one, else zero */
static inline void nvmtl_sparse_tile_size(int reported, const uint32_t gran[3], uint32_t out[3])
{
    for (int i = 0; i < 3; i++) out[i] = reported && gran ? gran[i] : 0u;
}
/* sparseTileSizeInBytes: the block size when the device reports sparse properties for anything */
static inline uint64_t nvmtl_sparse_tile_bytes(int device_reports_sparse) { return device_reports_sparse ? NVMTL_SPARSE_BLOCK_BYTES : 0u; }
#endif
