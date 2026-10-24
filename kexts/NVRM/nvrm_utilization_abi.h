/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#define NVRM_UTILIZATION_ABI_VERSION 1u
#define NVRM_UTILIZATION_FN "NVRMGpuUtilization"

// Kernel-only platform function. Values are percentages multiplied by 100,
// including zero for a genuinely idle engine. A failed call supplies no sample.
struct NVRMUtilization {
    unsigned int version;
    unsigned int gr, fb, nvenc, nvdec;
    unsigned long long timestamp;
};
