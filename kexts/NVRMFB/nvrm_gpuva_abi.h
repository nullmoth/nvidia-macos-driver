/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#define NVRM_GPUVA_ABI_VERSION 1u

#define NVRM_GPUVA_FLAG_UNMAP_ONLY 1u

#define NVRM_GPUVA_FN_ALLOC   "nvGpuVaAlloc"
#define NVRM_GPUVA_FN_MAP     "nvGpuVaMap"
#define NVRM_GPUVA_FN_FREE    "nvGpuVaFree"

// Asks NVRM for the two kernel pointers a display kext needs. They were IORegistry properties, which any process can
// read, and that gave away the kernel's address layout.
#define NVRM_DISPLAY_PTRS_ABI_VERSION 1u
#define NVRM_DISPLAY_PTRS_FN "nvDisplayPointers"
struct NVRMDisplayPointers {
    unsigned int version;
    void        *kapi;
    void        *physForVa;
};

#define NVRM_FLIP_ABI_VERSION 2u

#define NVRM_FLIP_FN  "nvFlipToSurface"

struct NVRMFlipRequest {
    unsigned int version;
    unsigned int flags;
    void        *kapiMemory;
    unsigned int width, height;
    unsigned int pitch;
    int          flipResult;
    unsigned long long allocationCookie;
};
#define NVRM_GPUVA_FN_HANDLES "nvRmHandles"

struct NVRMGpuVaRequest {
    unsigned int       version;
    unsigned int       flags;
    void              *kapiDevice;
    void              *kapiMemory;
    unsigned int       hClient;
    unsigned int       hDevice;
    unsigned int       hMemory;
    unsigned int       hVirt;
    unsigned long long size;
    unsigned long long align;
    unsigned long long gpuva;
    unsigned int       isVidmem;
    int                status;
};

struct NVRMRmHandles {
    unsigned int version;
    void        *kapiDevice;
    unsigned int hClient;
    unsigned int hDevice;
    unsigned int hSubDevice;
};
