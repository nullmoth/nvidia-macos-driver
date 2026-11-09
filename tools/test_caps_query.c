/* test_caps_query.c - capability answers come from device bits (plugin/nvmtl_caps_query.h).
   cc -I plugin tools/test_caps_query.c -o /tmp/test_caps_query && /tmp/test_caps_query */
#include <stdio.h>
#include "nvmtl_caps_query.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    const uint32_t F = NVMTL_FF_SAMPLED_FILTER_LINEAR | NVMTL_FF_COLOR_ATTACHMENT | 0x1u;
    nvmtl_format_bits all = { F, F, F }, no_rgba_filter = { F, F, F & ~NVMTL_FF_SAMPLED_FILTER_LINEAR }, no_rg32_attach = { F, F & ~NVMTL_FF_COLOR_ATTACHMENT, F };
    nvmtl_format_bits s4 = { 0x1f, 0x1f, 0x1f }, s4_missing = { 0x1f, 0x1b, 0x1f }, s_none = { 1, 1, 1 };
    arm("32-bit float filtering is YES when all three formats filter linearly", nvmtl_cap_float32_filtering(&all) == 1);
    arm("32-bit float filtering is NO when one format cannot filter (a device without the bit)", nvmtl_cap_float32_filtering(&no_rgba_filter) == 0);
    arm("32-bit float filtering is NO with no format data at all", nvmtl_cap_float32_filtering(0) == 0);
    arm("32-bit MSAA is YES with colour attachment and 4 samples on all three", nvmtl_cap_float32_msaa(&all, &s4) == 1);
    arm("32-bit MSAA is NO when one format is not a colour attachment", nvmtl_cap_float32_msaa(&no_rg32_attach, &s4) == 0);
    arm("32-bit MSAA is NO when one format lacks 4 samples", nvmtl_cap_float32_msaa(&all, &s4_missing) == 0 && nvmtl_cap_float32_msaa(&all, &s_none) == 0);
    arm("pull-model interpolation follows sampleRateShading, both ways", nvmtl_cap_pull_model_interpolation(1) == 1 && nvmtl_cap_pull_model_interpolation(0) == 0);
    arm("texture LOD query needs Vulkan up and the translator lowering", nvmtl_cap_query_texture_lod(1, 1) == 1 && nvmtl_cap_query_texture_lod(0, 1) == 0 && nvmtl_cap_query_texture_lod(1, 0) == 0);
    arm("vertex amplification: count 1 is always fine, counts above the device's multiview views are not", nvmtl_cap_vertex_amplification(1, 0) == 1 && nvmtl_cap_vertex_amplification(2, 6) == 1 && nvmtl_cap_vertex_amplification(7, 6) == 0);
    arm("vertex amplification is NO for count 2 when multiview is off (0 views)", nvmtl_cap_vertex_amplification(2, 0) == 0 && nvmtl_cap_vertex_amplification(2, 1) == 0);
    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
