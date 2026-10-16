/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#define NVRM_ESC_SURF_VRAM     0xE2u
#define NVRM_ESC_SURF_DIRTY    0xE3u
#define NVRM_SURFSHARE_VERSION 2u
/* NVRM_ESC_SURF_DIRTY with this plane bit is a WindowServer completion mark (bit 0: 1 pending, 0 ready), not a dirty plane */
#define NVRM_SS_PLANE_GPU_MARK 0x80000000u
#define NVRM_SS_BAD_VERSION   0x80000001u
#define NVRM_SS_OFF           0x80000002u
#define NVRM_SS_NO_SURFACE    0x80000003u
#define NVRM_SS_NO_CACHE      0x80000004u
#define NVRM_SS_NO_VRAM       0x80000005u
#define NVRM_SS_TOO_SMALL     0x80000006u
struct NVRMSurfShareEsc {
    unsigned int version;
    unsigned int surfaceID;
    unsigned int plane;
    unsigned int hClient;
    unsigned int hSrcClient;
    unsigned int hSrcMemory;
    unsigned int status;
    unsigned int pad;
    unsigned long long size;
};

#ifdef NVRM_SS_KERNEL_ABI
#define NVACCEL_SS_FN   "nvSurfVram"
#define NVRM_SS_FN_PAGEOFF "nvSurfPageoff"
#define NVRM_SS_FN_RM   "nvSurfShareRm"
#define NVRM_SS_RM_VERSION 2u
enum { NVRM_SS_RM_SHARE_KAPI = 5 };
struct NVRMSurfShareRm {
    unsigned int version, op;
    void *kapiDevice, *kapiMemory;
    unsigned int target;
    unsigned int hClient, hObject;
    unsigned int status;
};
struct NVRMSurfPageoff {
    unsigned int version;
    unsigned int status;
    void *kapiDevice, *kapiMemory;
    const unsigned long long *pages;
    unsigned long long pageCount;
    unsigned long long length;
    unsigned long long usec;
};
#endif
