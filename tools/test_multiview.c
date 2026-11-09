/* test_multiview.c - Metal vertex amplification as a Vulkan multiview pass (plugin/nvmtl_multiview.h).
   cc -I plugin tools/test_multiview.c -o /tmp/test_multiview && /tmp/test_multiview */
#include <stdio.h>
#include <string.h>
#include "nvmtl_multiview.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }

/* a minimal SPIR-V stream: header, then OpDecorate <id> BuiltIn <b>, and OpMemberDecorate <struct> <m> BuiltIn <b> */
static size_t decorate(uint32_t *w, uint32_t builtin, int member)
{
    size_t n = 0;
    w[n++] = 0x07230203u; w[n++] = 0x00010500u; w[n++] = 0; w[n++] = 10; w[n++] = 0;
    if (member) { w[n++] = (5u << 16) | NVMTL_SPV_OP_MEMBER_DECORATE; w[n++] = 7; w[n++] = 1; w[n++] = NVMTL_SPV_DEC_BUILTIN; w[n++] = builtin; }
    else        { w[n++] = (4u << 16) | NVMTL_SPV_OP_DECORATE;        w[n++] = 7;               w[n++] = NVMTL_SPV_DEC_BUILTIN; w[n++] = builtin; }
    return n;
}

int main(void)
{
    arm("count 2 on a device with 6 views gives view mask 0b11", nvmtl_mv_mask_for_count(2, 6) == 3u);
    arm("count 4 gives mask 0xF and four layers are needed", nvmtl_mv_mask_for_count(4, 6) == 15u && nvmtl_mv_layers_needed(15u) == 4u);
    arm("count 1 is an ordinary pass: no view mask, today's behaviour", nvmtl_mv_mask_for_count(1, 6) == 0u && nvmtl_mv_mask_for_count(0, 6) == 0u);
    arm("a count above what the device can multiview gives no mask (the pipeline is refused upstream, not drawn wrong)", nvmtl_mv_mask_for_count(7, 6) == 0u);
    arm("a count above 32 never overflows the mask shift", nvmtl_mv_mask_for_count(33, 64) == 0u && nvmtl_mv_mask_for_count(32, 64) == 0xFFFFFFFFu);
    arm("popcount of a mask is the number of views", nvmtl_mv_popcount(0xFu) == 4u && nvmtl_mv_popcount(0u) == 0u);

    nvmtl_mv_mapping id2[2] = { { 0, 0 }, { 1, 0 } };
    nvmtl_mv_mapping stereo_viewports[2] = { { 0, 0 }, { 0, 1 } };
    nvmtl_mv_mapping shifted[2] = { { 1, 0 }, { 2, 0 } };
    nvmtl_mv_mapping swapped[2] = { { 1, 0 }, { 0, 0 } };
    arm("slice i / viewport 0 mappings are accepted", nvmtl_mv_mappings_refusal(2, id2) == NULL);
    arm("no mapping array is Metal's default and is accepted", nvmtl_mv_mappings_refusal(2, NULL) == NULL);
    arm("two views packed into two viewports of one slice are refused with a reason", nvmtl_mv_mappings_refusal(2, stereo_viewports) != NULL
        && strstr(nvmtl_mv_mappings_refusal(2, stereo_viewports), "viewport"));
    arm("slice offsets that do not start at 0 are refused", nvmtl_mv_mappings_refusal(2, shifted) != NULL);
    arm("slice offsets out of view order are refused", nvmtl_mv_mappings_refusal(2, swapped) != NULL);
    arm("count 1 never looks at mappings", nvmtl_mv_mappings_refusal(1, stereo_viewports) == NULL);

    arm("an amplified pipeline drawn with the count it was built for is allowed", nvmtl_mv_draw_refusal(3u, 2) == NULL);
    arm("an amplified pipeline drawn at count 1 is refused", nvmtl_mv_draw_refusal(3u, 1) != NULL);
    arm("an amplified pipeline drawn at a different count is refused", nvmtl_mv_draw_refusal(3u, 4) != NULL);
    arm("an ordinary pipeline ignores the encoder's count", nvmtl_mv_draw_refusal(0u, 2) == NULL && nvmtl_mv_draw_refusal(0u, 1) == NULL);

    const char *why = NULL;
    arm("an ordinary pass keeps its framebuffer layer count", nvmtl_mv_framebuffer_layers(0, 1, &why) == 1u && nvmtl_mv_framebuffer_layers(0, 6, &why) == 6u);
    arm("a multiview pass has a framebuffer of 1 layer when the views carry enough layers", nvmtl_mv_framebuffer_layers(3u, 2, &why) == 1u && !why);
    arm("a multiview pass over a single-layer attachment is refused with a reason", nvmtl_mv_framebuffer_layers(3u, 1, &why) == 0u && why != NULL);

    uint32_t w[16]; size_t n;
    n = decorate(w, NVMTL_SPV_BI_LAYER, 0);
    arm("a vertex module that writes Layer is refused under multiview", nvmtl_mv_vertex_refusal(w, n) != NULL);
    n = decorate(w, NVMTL_SPV_BI_VIEWPORT_INDEX, 1);
    arm("a ViewportIndex block member is found too", nvmtl_mv_vertex_refusal(w, n) != NULL);
    n = decorate(w, 0 /* Position */, 1);
    arm("a module that only writes Position may run under multiview", nvmtl_mv_vertex_refusal(w, n) == NULL);
    n = decorate(w, NVMTL_SPV_BI_VIEW_INDEX, 0);
    arm("ViewIndex is found as a builtin", nvmtl_mv_spirv_has_builtin(w, n, NVMTL_SPV_BI_VIEW_INDEX) == 1);
    arm("a truncated stream is not guessed at", nvmtl_mv_spirv_has_builtin(w, n - 1, NVMTL_SPV_BI_VIEW_INDEX) == 0);
    w[0] = 0; arm("a stream without the SPIR-V magic is ignored", nvmtl_mv_spirv_has_builtin(w, n, NVMTL_SPV_BI_VIEW_INDEX) == 0);

    printf("%s (%d failed)\n", fails ? "FAILED" : "ALL ARMS HOLD", fails);
    return fails != 0;
}
