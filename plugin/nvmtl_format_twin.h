/* nvmtl_format_twin.h - colour formats that differ only in channel order.
   Metal draws through the attachment's format, so a pipeline declared in one format renders correctly into an attachment of
   its twin (red stays red). The pairs are the 8-bit RGBA/BGRA ones, with and without sRGB, and the two 10-bit packings.
   Vulkan formats and Metal pixel formats are both given, so tools/test_format_twin.c can check they describe the same pairs. */
#ifndef NVMTL_FORMAT_TWIN_H
#define NVMTL_FORMAT_TWIN_H
#include <stdint.h>
typedef struct { uint32_t mtl, vk; } nvmtl_format_pair;
/* MTLPixelFormat / VkFormat: RGBA8Unorm 70 / R8G8B8A8_UNORM 37, BGRA8Unorm 80 / B8G8R8A8_UNORM 44, the sRGB pair 71 / 43 and
   81 / 50, RGB10A2Unorm 90 / A2B10G10R10_UNORM_PACK32 64, BGR10A2Unorm 94 / A2R10G10B10_UNORM_PACK32 58. */
static const nvmtl_format_pair nvmtl_format_twins[][2] = {
    { { 70, 37 }, { 80, 44 } }, { { 71, 43 }, { 81, 50 } }, { { 90, 64 }, { 94, 58 } },
};
static inline uint32_t nvmtl_format_twin_vk(uint32_t f)
{
    for (unsigned i = 0; i < sizeof nvmtl_format_twins / sizeof nvmtl_format_twins[0]; i++)
        for (int s = 0; s < 2; s++) if (nvmtl_format_twins[i][s].vk == f) return nvmtl_format_twins[i][1 - s].vk;
    return 0;
}
static inline uint32_t nvmtl_format_twin_mtl(uint32_t f)
{
    for (unsigned i = 0; i < sizeof nvmtl_format_twins / sizeof nvmtl_format_twins[0]; i++)
        for (int s = 0; s < 2; s++) if (nvmtl_format_twins[i][s].mtl == f) return nvmtl_format_twins[i][1 - s].mtl;
    return 0;
}
#endif
