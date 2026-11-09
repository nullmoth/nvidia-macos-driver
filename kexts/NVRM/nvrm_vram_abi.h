/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#define NVRM_VRAM_ABI_VERSION 2u
#define NVRM_VRAM_FN_ALLOC    "nvAllocVram"
#define NVRM_VRAM_FN_FREE     "nvFreeVram"

#define NVRM_VRAM_FN_SCANOUT  "nvScanoutInfo"
// Reply flag of nvScanoutInfo (NVRMVramRequest::flags): the scanout is 30-bit X2B10G10R10, not 8-bit X8R8G8B8. A framebuffer that
// never sets it leaves the 8-bit layout, which is what every accelerator assumed before.
#define NVRM_VRAM_SCANOUT_30BIT 0x1u

#define NVRM_VRAM_BAR1_BUDGET (192ull * 1024ull * 1024ull)

struct NVRMVramRequest {
    unsigned int       version;
    unsigned int       flags;
    unsigned long long size;
    unsigned long long actualSize;
    unsigned long long phys;
    void              *kva;
    void              *handle;
    unsigned long long mappedTotal;
    unsigned int width, height, pitch;
    unsigned long long allocationCookie;
};
