/* nvmtl_copy_compat.h - which two Vulkan formats one image-to-image copy may join.
   vkCmdCopyImage copies bits, and it accepts two different single-plane colour formats when they are size-compatible:
   the same texel size for uncompressed formats, the same compatibility class for block-compressed ones. Depth and stencil
   formats must match exactly. The texture copy used to demand an identical VkFormat, so a copy Metal performs (an sRGB
   texture and its linear twin, a texture and a view that reads it in another format, the BGRA/RGBA twins) was dropped with
   one log line and the destination kept whatever the memory held before - in a recycled heap placement, the previous
   resource's texels. tools/test_copy_compat.c checks the table. Values are VkFormat numbers from the Vulkan core table. */
#ifndef NVMTL_COPY_COMPAT_H
#define NVMTL_COPY_COMPAT_H
#include <stdint.h>
/* The compatibility class of a colour format: uncompressed formats are keyed by texel bytes (1..32), block-compressed BC
   formats by 100 + their class, anything else (depth, stencil, ETC, ASTC, multi-planar, unknown) by 0 = exact match only. */
static inline uint32_t nvmtl_copy_class(uint32_t f)
{
    if (f == 1) return 1;                           /* R4G4_UNORM_PACK8 */
    if (f >= 2 && f <= 8) return 2;                 /* 4444, 565, 5551 packings */
    if (f >= 9 && f <= 15) return 1;                /* R8 */
    if (f >= 16 && f <= 22) return 2;               /* R8G8 */
    if (f >= 23 && f <= 36) return 3;               /* R8G8B8, B8G8R8 */
    if (f >= 37 && f <= 69) return 4;               /* RGBA8, BGRA8, ABGR8 packed, A2R10G10B10, A2B10G10R10 */
    if (f >= 70 && f <= 76) return 2;               /* R16 */
    if (f >= 77 && f <= 83) return 4;               /* R16G16 */
    if (f >= 84 && f <= 90) return 6;               /* R16G16B16 */
    if (f >= 91 && f <= 97) return 8;               /* R16G16B16A16 */
    if (f >= 98 && f <= 100) return 4;              /* R32 */
    if (f >= 101 && f <= 103) return 8;             /* R32G32 */
    if (f >= 104 && f <= 106) return 12;            /* R32G32B32 */
    if (f >= 107 && f <= 109) return 16;            /* R32G32B32A32 */
    if (f >= 110 && f <= 112) return 8;             /* R64 */
    if (f >= 113 && f <= 115) return 16;            /* R64G64 */
    if (f >= 116 && f <= 118) return 24;            /* R64G64B64 */
    if (f >= 119 && f <= 121) return 32;            /* R64G64B64A64 */
    if (f == 122 || f == 123) return 4;             /* B10G11R11_UFLOAT, E5B9G9R9_UFLOAT */
    /* 124-130 are depth/stencil: exact match only */
    if (f >= 131 && f <= 146) return 100 + (f - 131) / 2;   /* BC1 RGB, BC1 RGBA, BC2, BC3, BC4, BC5, BC6H, BC7 - each a UNORM/SRGB or UNORM/SNORM pair */
    return 0;
}
/* 1 when one vkCmdCopyImage may copy from a src-format image into a dst-format image. */
static inline int nvmtl_copy_formats_ok(uint32_t src, uint32_t dst)
{
    if (src == dst) return 1;
    const uint32_t a = nvmtl_copy_class(src), b = nvmtl_copy_class(dst);
    return a && a == b;
}
#endif
