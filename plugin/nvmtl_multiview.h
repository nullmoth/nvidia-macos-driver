/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_multiview.h - Metal vertex amplification expressed as a Vulkan multiview pass.
 *
 * Metal draws one primitive stream once per amplified view. setVertexAmplificationCount:viewMappings: names, for each
 * view, a render-target-array index offset and a viewport-array index offset. Vulkan's multiview (core 1.1) runs the
 * subpass once per set bit of the render pass view mask and routes view i to layer i of every attachment view.
 *
 * What maps exactly:
 *   - count N with view i mapped to render target slice i and viewport 0 -> view mask (1<<N)-1.
 * What does not, and is refused with a reason instead of drawn wrongly:
 *   - a view mapped to a viewport other than 0 (needs per-view viewport selection, which core multiview lacks);
 *   - render target offsets that are not 0,1,2,... in view order;
 *   - a vertex shader that itself writes the layer or the viewport index (Vulkan forbids that under multiview);
 *   - an encoder count that differs from the count the pipeline was built for (the pass view mask is part of the
 *     pipeline's render pass, so one pipeline serves one count).
 *
 * Everything here is pure, so tools/test_multiview.c proves it without a GPU or the plugin.
 */
#ifndef NVMTL_MULTIVIEW_H
#define NVMTL_MULTIVIEW_H
#include <stddef.h>
#include <stdint.h>

#define NVMTL_MV_HARD_MAX 32u   /* a Vulkan view mask is 32 bits */

typedef struct { uint32_t render_target_offset, viewport_offset; } nvmtl_mv_mapping;

/* the view mask for an amplification count, 0 when the count is 1 (an ordinary pass) or unusable */
static inline uint32_t nvmtl_mv_mask_for_count(uint32_t count, uint32_t device_max_views)
{
    if (count < 2 || count > NVMTL_MV_HARD_MAX || count > device_max_views) return 0;
    return count == 32u ? 0xFFFFFFFFu : ((1u << count) - 1u);
}

static inline uint32_t nvmtl_mv_popcount(uint32_t mask)
{
    uint32_t n = 0;
    for (; mask; mask &= mask - 1u) n++;
    return n;
}

/* one past the highest set bit: the attachment views must have at least this many layers */
static inline uint32_t nvmtl_mv_layers_needed(uint32_t mask)
{
    uint32_t n = 0;
    for (; mask; mask >>= 1) n++;
    return n;
}

/* 0 when the mappings are the identity layer routing Vulkan multiview provides; otherwise a reason string.
   A NULL mapping array is Metal's default for the call: view i -> slice i, viewport 0. */
static inline const char *nvmtl_mv_mappings_refusal(uint32_t count, const nvmtl_mv_mapping *m)
{
    if (count < 2) return NULL;
    if (!m) return NULL;
    for (uint32_t i = 0; i < count; i++) {
        if (m[i].viewport_offset != 0) return "a view is mapped to a viewport other than 0 (multiview cannot select a viewport per view)";
        if (m[i].render_target_offset != i) return "render target offsets are not 0,1,2,... in view order (multiview routes view i to layer i)";
    }
    return NULL;
}

/* A draw with an amplified pipeline: the encoder's count must equal the pipeline's. NULL when the draw may go ahead. */
static inline const char *nvmtl_mv_draw_refusal(uint32_t pipeline_mask, uint32_t encoder_count)
{
    if (!pipeline_mask) return NULL;   /* an ordinary pipeline: the encoder's count is ignored, as before */
    if (nvmtl_mv_popcount(pipeline_mask) != encoder_count)
        return encoder_count < 2 ? "the pipeline renders several views and the encoder's amplification count is 1"
                                 : "the encoder's amplification count differs from the one the pipeline was built for";
    return NULL;
}

/* The pass the views draw into: with a view mask Vulkan wants the framebuffer's layer count to be 1 and the attachment
   views to carry the layers. Returns the framebuffer layer count, or 0 with *why set when the attachments are too short. */
static inline uint32_t nvmtl_mv_framebuffer_layers(uint32_t mask, uint32_t pass_layers, const char **why)
{
    if (why) *why = NULL;
    if (!mask) return pass_layers > 1 ? pass_layers : 1u;
    if (pass_layers < nvmtl_mv_layers_needed(mask)) {
        if (why) *why = "the render pass has fewer array layers than the pipeline has views";
        return 0;
    }
    return 1u;
}

/* ---- SPIR-V scans (word streams, little-endian host words) ---- */
#define NVMTL_SPV_OP_DECORATE        71u
#define NVMTL_SPV_OP_MEMBER_DECORATE 72u
#define NVMTL_SPV_DEC_BUILTIN        11u
#define NVMTL_SPV_BI_LAYER           9u
#define NVMTL_SPV_BI_VIEWPORT_INDEX  10u
#define NVMTL_SPV_BI_VIEW_INDEX      4440u

/* 1 when the module decorates anything with the given BuiltIn (a variable or a block member) */
static inline int nvmtl_mv_spirv_has_builtin(const uint32_t *w, size_t nwords, uint32_t builtin)
{
    if (!w || nwords < 5 || w[0] != 0x07230203u) return 0;
    for (size_t i = 5; i < nwords;) {
        const uint32_t op = w[i] & 0xFFFFu, len = w[i] >> 16;
        if (!len || i + len > nwords) return 0;   /* a malformed stream: say nothing rather than guess */
        if (op == NVMTL_SPV_OP_DECORATE && len >= 4 && w[i + 2] == NVMTL_SPV_DEC_BUILTIN && w[i + 3] == builtin) return 1;
        if (op == NVMTL_SPV_OP_MEMBER_DECORATE && len >= 5 && w[i + 3] == NVMTL_SPV_DEC_BUILTIN && w[i + 4] == builtin) return 1;
        i += len;
    }
    return 0;
}

/* NULL when a vertex module may run under multiview, otherwise why not */
static inline const char *nvmtl_mv_vertex_refusal(const uint32_t *w, size_t nwords)
{
    if (nvmtl_mv_spirv_has_builtin(w, nwords, NVMTL_SPV_BI_LAYER)) return "the vertex shader writes the render target array index, which Vulkan forbids under multiview";
    if (nvmtl_mv_spirv_has_builtin(w, nwords, NVMTL_SPV_BI_VIEWPORT_INDEX)) return "the vertex shader writes the viewport array index, which Vulkan forbids under multiview";
    return NULL;
}
#endif
