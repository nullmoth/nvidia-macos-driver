/* test_format_twin.c - the channel-order twin table (plugin/nvmtl_format_twin.h).
   cc -I plugin tools/test_format_twin.c -o /tmp/test_format_twin && /tmp/test_format_twin */
#include <stdio.h>
#include "nvmtl_format_twin.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    arm("the pipeline format Cyberpunk builds (BGRA8, Vulkan 44) has the RGBA8 back buffer's format (37) as its twin", nvmtl_format_twin_vk(44) == 37 && nvmtl_format_twin_vk(37) == 44);
    arm("the Metal formats agree: BGRA8Unorm 80 <-> RGBA8Unorm 70", nvmtl_format_twin_mtl(80) == 70 && nvmtl_format_twin_mtl(70) == 80);
    int ok = 1;
    for (unsigned i = 0; i < sizeof nvmtl_format_twins / sizeof nvmtl_format_twins[0]; i++)
        for (int s = 0; s < 2; s++) {
            const nvmtl_format_pair a = nvmtl_format_twins[i][s], b = nvmtl_format_twins[i][1 - s];
            if (nvmtl_format_twin_vk(a.vk) != b.vk || nvmtl_format_twin_mtl(a.mtl) != b.mtl) ok = 0;
            if (nvmtl_format_twin_vk(nvmtl_format_twin_vk(a.vk)) != a.vk) ok = 0;   /* a twin of a twin is the original */
        }
    arm("every pair maps both ways, in both numberings, and twice is the identity", ok);
    arm("sRGB stays sRGB: 43 <-> 50 and 71 <-> 81", nvmtl_format_twin_vk(43) == 50 && nvmtl_format_twin_mtl(71) == 81);
    arm("a format with no twin (RGBA16F 115 / VK 97, R8 10 / 9) maps to none", !nvmtl_format_twin_vk(97) && !nvmtl_format_twin_mtl(115) && !nvmtl_format_twin_vk(9) && !nvmtl_format_twin_mtl(10));
    arm("an sRGB format is never the twin of a linear one", nvmtl_format_twin_vk(37) != 50 && nvmtl_format_twin_vk(43) != 44);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
