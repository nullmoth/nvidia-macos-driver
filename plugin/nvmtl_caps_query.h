/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* nvmtl_caps_query.h - MTLDevice capability answers decided from the Vulkan bits the device reported, never fixed.
 * Each function takes the raw bits (VkFormatFeatureFlags, VkSampleCountFlags, a feature boolean) so tools/test_caps_query.c can
 * prove that a device without the bit answers NO. */
#ifndef NVMTL_CAPS_QUERY_H
#define NVMTL_CAPS_QUERY_H
#include <stdint.h>

#define NVMTL_FF_SAMPLED_FILTER_LINEAR 0x00001000u   /* VK_FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR_BIT */
#define NVMTL_FF_COLOR_ATTACHMENT      0x00000080u   /* VK_FORMAT_FEATURE_COLOR_ATTACHMENT_BIT */
#define NVMTL_SC_4                     0x00000004u   /* VK_SAMPLE_COUNT_4_BIT */

/* the three 32-bit float formats Metal's query covers: R32Float, RG32Float, RGBA32Float */
typedef struct { uint32_t r32, rg32, rgba32; } nvmtl_format_bits;

/* supports32BitFloatFiltering: every one of the three filters linearly when sampled */
static inline int nvmtl_cap_float32_filtering(const nvmtl_format_bits *optimal_features)
{
    return optimal_features && (optimal_features->r32 & optimal_features->rg32 & optimal_features->rgba32 & NVMTL_FF_SAMPLED_FILTER_LINEAR) != 0;
}
/* supports32BitMSAA: each is a colour attachment and the device offers 4 samples for it (the smallest count Metal's query is about) */
static inline int nvmtl_cap_float32_msaa(const nvmtl_format_bits *features, const nvmtl_format_bits *sample_counts)
{
    if (!features || !sample_counts) return 0;
    const uint32_t f = features->r32 & features->rg32 & features->rgba32;
    const uint32_t s = sample_counts->r32 & sample_counts->rg32 & sample_counts->rgba32;
    return (f & NVMTL_FF_COLOR_ATTACHMENT) != 0 && (s & NVMTL_SC_4) != 0;
}
/* supportsPullModelInterpolation: SPIR-V's InterpolationFunction capability (interpolateAtOffset / Sample / Centroid) needs sampleRateShading */
static inline int nvmtl_cap_pull_model_interpolation(int sample_rate_shading) { return sample_rate_shading != 0; }
/* supportsQueryTextureLOD: OpImageQueryLod is core Vulkan with no feature bit; it needs a working device and the translator's lowering */
static inline int nvmtl_cap_query_texture_lod(int vulkan_up, int translator_lowers_lod_query) { return vulkan_up && translator_lowers_lod_query; }
/* supportsVertexAmplificationCount: the multiview view count the device reported with the feature on (0 = off or absent) */
static inline int nvmtl_cap_vertex_amplification(uint32_t count, uint32_t device_views) { return count <= 1 || (device_views >= 2 && count <= device_views); }
#endif
