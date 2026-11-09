/* test_copy_compat.c - which formats one texture-to-texture copy may join (plugin/nvmtl_copy_compat.h).
   cc -I plugin tools/test_copy_compat.c -o /tmp/test_copy_compat && /tmp/test_copy_compat */
#include <stdio.h>
#include "nvmtl_copy_compat.h"
#include "nvmtl_format_twin.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    arm("the same format always copies (RGBA16F 97 -> 97, D32 126 -> 126, ASTC 157 -> 157)",
        nvmtl_copy_formats_ok(97, 97) && nvmtl_copy_formats_ok(126, 126) && nvmtl_copy_formats_ok(157, 157));
    arm("an sRGB texture and its linear twin copy (RGBA8 37 <-> 43, BGRA8 44 <-> 50)",
        nvmtl_copy_formats_ok(37, 43) && nvmtl_copy_formats_ok(43, 37) && nvmtl_copy_formats_ok(44, 50));
    int twins = 1;
    for (unsigned i = 0; i < sizeof nvmtl_format_twins / sizeof nvmtl_format_twins[0]; i++)
        if (!nvmtl_copy_formats_ok(nvmtl_format_twins[i][0].vk, nvmtl_format_twins[i][1].vk)) twins = 0;
    arm("every channel-order twin the driver substitutes copies into the other", twins);
    arm("a 32-bit view of a 32-bit texture copies (R32_UINT 98 <-> B10G11R11 122, RGBA8 37 <-> R32_SFLOAT 100)",
        nvmtl_copy_formats_ok(98, 122) && nvmtl_copy_formats_ok(37, 100));
    arm("BC7 UNORM 145 and BC7 SRGB 146 copy; BC1 RGB 131 and BC1 RGBA 133 do NOT (different classes)",
        nvmtl_copy_formats_ok(145, 146) && !nvmtl_copy_formats_ok(131, 133));
    arm("NEGATIVE: different texel sizes never copy (RGBA8 37 -> RGBA16F 97, R16 70 -> R32 98)",
        !nvmtl_copy_formats_ok(37, 97) && !nvmtl_copy_formats_ok(70, 98));
    arm("NEGATIVE: depth/stencil never joins another format (D32 126 -> R32_SFLOAT 100, D24S8 129 -> D32S8 130)",
        !nvmtl_copy_formats_ok(126, 100) && !nvmtl_copy_formats_ok(100, 126) && !nvmtl_copy_formats_ok(129, 130));
    arm("NEGATIVE: compressed never joins uncompressed of the same block size (BC7 145 -> RGBA32UI 107)",
        !nvmtl_copy_formats_ok(145, 107));
    arm("NEGATIVE: an unknown format (0, ASTC 157 vs 158) only matches itself", !nvmtl_copy_formats_ok(0, 37) && !nvmtl_copy_formats_ok(157, 158));
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
