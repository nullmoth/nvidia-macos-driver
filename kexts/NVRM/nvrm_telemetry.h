/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#include <nv.h>
#include "nvrm_telemetry_abi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* The caller serializes query and teardown and holds the NVRM PM lease. */
NV_STATUS nvrm_query_telemetry(nv_state_t *nv, NvU64 pmGeneration,
                               void **state, struct NVRMTelemetry *sample);
void nvrm_free_telemetry(void **state);

#ifdef __cplusplus
}
#endif
