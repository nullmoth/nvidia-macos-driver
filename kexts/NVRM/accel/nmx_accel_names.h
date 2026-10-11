/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 *
 * NMXAccel: nvrm-accel.cpp built with -DNMX_ACCEL is the Metal accelerator for the AMD and Intel GPUs NMXGPU drives.
 * Its classes take their own names so it can be loaded beside the NVIDIA accelerator (a PC with an NVIDIA card and an
 * AMD or Intel GPU): IOKit class names are global.
 */
#ifndef NMX_ACCEL_NAMES_H
#define NMX_ACCEL_NAMES_H
#define NVAccel            NMXAccel
#define NVEventMachine     NMXEventMachine
#define NVDisplayMachine   NMXDisplayMachine
#define NVTask             NMXTask
#define NVMemoryMap        NMXMemoryMap
#define IOAccelNVRMCarrier IOAccelNMXCarrier
#define NVResource         NMXResource
#define NVSysMemory        NMXSysMemory
#define NVVidMemory        NMXVidMemory
#define NVSurface          NMXSurface
#define NVDisplayPipe      NMXDisplayPipe
#define NVGLContext        NMXGLContext
#define NV2DContext        NMX2DContext
#define NVCLContext        NMXCLContext
#define NVVideoContext     NMXVideoContext
#endif
