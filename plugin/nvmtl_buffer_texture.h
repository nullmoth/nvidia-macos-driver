/* Bounds of the bytes actually read by a linear, uncompressed texture. */
#pragma once
#include <stddef.h>
#include <stdint.h>

static inline int nvmtl_buffer_texture_fits(size_t length, size_t offset, size_t width,
                                           size_t height, size_t bpp, size_t pitch)
{
    if (!width || !height || !bpp || offset > length || width > SIZE_MAX / bpp) return 0;
    const size_t row = width * bpp, available = length - offset;
    // The last row has no required trailing padding. Division avoids pitch * height overflow.
    return pitch >= row && row <= available && height - 1 <= (available - row) / pitch;
}
