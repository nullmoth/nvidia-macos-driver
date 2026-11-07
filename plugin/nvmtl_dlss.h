/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * DLSS Super Resolution behind the MetalFX temporal scaler.
 *
 * NVIDIA's NGX core and DLSS runtime are x86-64 Linux objects. They are loaded from a directory the user
 * installs (never shipped in the plugin) and run inside the process through the ELF host in dlss/. This file is the
 * Vulkan-level layer: it knows Vulkan handles and NGX, nothing about Metal.
 *
 * Everything here is opt-in by the files being present: with no files installed, or NVMTL_DLSS=0, every entry
 * point reports "not available" and the caller keeps its existing path.
 */
#pragma once
#include <stdint.h>
#include <vulkan/vulkan_core.h>

/* One place that names where the user-installed files live. */
#define NVMTL_DLSS_ROOT         "/Library/NullMoth/dlss"
#define NVMTL_DLSS_NGX_DIR      NVMTL_DLSS_ROOT "/ngx"        /* holds libnvidia-ngx.so.<version> */
#define NVMTL_DLSS_SNIPPET_DIR  NVMTL_DLSS_ROOT "/snippets"   /* holds the DLSS snippet files */

/* The extensions the NGX core needs from the Vulkan device. */
#define NVMTL_DLSS_EXT_BINARY_IMPORT "VK_NVX_binary_import"
#define NVMTL_DLSS_EXT_IMAGE_HANDLE  "VK_NVX_image_view_handle"

/* Scale range reported to MetalFX clients while DLSS is serving them (input size / output size). */
#define NVMTL_DLSS_MIN_INPUT_SCALE (1.0f / 3.0f)
#define NVMTL_DLSS_MAX_INPUT_SCALE 1.0f

/* 1 when NVMTL_DLSS is not "0" and the core and snippet directories exist. Cheap, cached. Decides whether the
 * device should be created with the extensions above. */
int nvmtl_dlss_wanted(void);

/* Load the core and initialise it for this device, once. 0 on success; any other value is final for the process
 * (the reason was logged). Safe to call repeatedly and from several threads. */
int nvmtl_dlss_init(VkInstance inst, VkPhysicalDevice pd, VkDevice dev,
                    PFN_vkGetInstanceProcAddr gipa, PFN_vkGetDeviceProcAddr gdpa,
                    uint32_t pci_vendor, uint32_t pci_device);
/* 1 once nvmtl_dlss_init succeeded and the core reported Super Resolution available. */
int nvmtl_dlss_ready(void);

/* An image as NGX takes it. aspect is COLOR for colour images, DEPTH for a depth buffer. */
typedef struct { VkImageView view; VkImage image; VkImageAspectFlags aspect; VkFormat format; uint32_t w, h; } nvmtl_dlss_image;

/* What a feature is created for; a change in any field needs a new feature. */
typedef struct {
    uint32_t in_w, in_h, out_w, out_h;
    int hdr;              /* colour input is linear high dynamic range */
    int mv_lowres;        /* motion vectors are at input resolution (otherwise output resolution) */
    int mv_jittered;      /* motion vectors include the jitter offset */
    int depth_inverted;   /* depth 1 is near */
    int auto_exposure;    /* no exposure texture: the runtime estimates exposure */
} nvmtl_dlss_cfg;

typedef struct nvmtl_dlss_feature nvmtl_dlss_feature;

/* NGX quality mode (0 max performance, 1 balanced, 2 max quality, 3 ultra performance, 4 ultra quality, 5 DLAA)
 * for an output/input size ratio. */
int nvmtl_dlss_quality_for_ratio(float out_over_in);

/* Create a feature, recording its setup into cmd. NULL on failure (logged). */
nvmtl_dlss_feature *nvmtl_dlss_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_cfg *cfg);

typedef struct {
    nvmtl_dlss_image color, depth, motion, output;
    const nvmtl_dlss_image *exposure;   /* NULL when absent */
    const nvmtl_dlss_image *reactive;   /* NULL when absent */
    float jitter_x, jitter_y;           /* sub-pixel jitter in input pixels */
    float mv_scale_x, mv_scale_y;
    float pre_exposure;                 /* 0 = not supplied */
    int reset;
    uint32_t render_w, render_h;        /* the used part of the input */
    uint32_t color_x, color_y, depth_x, depth_y, motion_x, motion_y;   /* input content offsets */
} nvmtl_dlss_eval;

/* Record the upscale into cmd. All images must be in VK_IMAGE_LAYOUT_GENERAL and visible to compute. 0 on success. */
int nvmtl_dlss_evaluate(VkCommandBuffer cmd, nvmtl_dlss_feature *f, const nvmtl_dlss_eval *e);
/* The cfg a feature was created with. */
const nvmtl_dlss_cfg *nvmtl_dlss_feature_cfg(const nvmtl_dlss_feature *f);

/* ---- Ray Reconstruction behind the MetalFX temporal denoised scaler ---- */
/* 1 once the core is ready, reported Ray Reconstruction available, and NVMTL_DLSS_RR is not "0". */
int nvmtl_dlss_rr_ready(void);
typedef struct {
    uint32_t in_w, in_h, out_w, out_h;
    int hdr, mv_lowres, mv_jittered, depth_inverted, auto_exposure;
} nvmtl_dlss_rr_cfg;
typedef struct nvmtl_dlss_rr_feature nvmtl_dlss_rr_feature;
nvmtl_dlss_rr_feature *nvmtl_dlss_rr_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_rr_cfg *cfg);
const nvmtl_dlss_rr_cfg *nvmtl_dlss_rr_feature_cfg(const nvmtl_dlss_rr_feature *f);
typedef struct {
    nvmtl_dlss_image color, depth, motion, output, diffuse_albedo, specular_albedo, normals, roughness;
    const nvmtl_dlss_image *hit_distance;   /* NULL when absent */
    const nvmtl_dlss_image *exposure;       /* NULL when absent */
    const nvmtl_dlss_image *reactive;       /* NULL when absent */
    float world_to_view[16], view_to_clip[16];   /* as the app gave them (column-major), copied by the evaluate call */
    float jitter_x, jitter_y, mv_scale_x, mv_scale_y, pre_exposure;
    int reset;
    uint32_t render_w, render_h;
} nvmtl_dlss_rr_eval;
/* Record the denoise + upscale into cmd. All images must be in VK_IMAGE_LAYOUT_GENERAL. 0 on success. */
int nvmtl_dlss_rr_evaluate(VkCommandBuffer cmd, nvmtl_dlss_rr_feature *f, const nvmtl_dlss_rr_eval *e);

/* ---- Frame Generation behind the MetalFX frame interpolator ---- */
/* 1 once the core is ready, reported Frame Generation available, and NVMTL_DLSS_FG is not "0". */
int nvmtl_dlss_fg_ready(void);
typedef struct {
    uint32_t out_w, out_h;      /* the colour / output size (the "backbuffer") */
    uint32_t in_w, in_h;        /* the depth and motion size */
    uint32_t color_format;      /* VkFormat of the colour image */
    int hdr, depth_inverted;
} nvmtl_dlss_fg_cfg;
typedef struct nvmtl_dlss_fg_feature nvmtl_dlss_fg_feature;
nvmtl_dlss_fg_feature *nvmtl_dlss_fg_feature_create(VkCommandBuffer cmd, const nvmtl_dlss_fg_cfg *cfg);
const nvmtl_dlss_fg_cfg *nvmtl_dlss_fg_feature_cfg(const nvmtl_dlss_fg_feature *f);
typedef struct {
    nvmtl_dlss_image color, depth, motion, output;   /* colour = the newer real frame; output = the in-between frame */
    const nvmtl_dlss_image *ui;                       /* NULL when absent */
    float near_z, far_z, fov_y, aspect;               /* fov in radians */
    float cam_pos[3], cam_right[3], cam_up[3], cam_fwd[3];
    float clip_to_prev[16], prev_to_clip[16], view_to_clip[16];
    float mv_scale_x, mv_scale_y, jitter_x, jitter_y;
    int reset, camera_motion_included;
    uint32_t frame_id, multi_index, multi_count;
} nvmtl_dlss_fg_eval;
/* Record one evaluation into cmd; images must be in VK_IMAGE_LAYOUT_GENERAL. 0 on success. */
int nvmtl_dlss_fg_evaluate(VkCommandBuffer cmd, nvmtl_dlss_fg_feature *f, const nvmtl_dlss_fg_eval *e);

/* Local check of the worker thread every call above runs on (stack size, one thread, environment kept). 0 = pass. */
int nvmtl_dlss_selftest(void);
