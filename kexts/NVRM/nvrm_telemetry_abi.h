/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#define NVRM_TELEMETRY_ABI_VERSION 1u
#define NVRM_TELEMETRY_FN "NVRMGpuTelemetry"
#define NVRM_TELEMETRY_FAN_COUNT 16u

#define NVRM_TELEMETRY_VALID_AVERAGE_POWER  (1u << 0)
#define NVRM_TELEMETRY_VALID_INSTANT_POWER  (1u << 1)
#define NVRM_TELEMETRY_VALID_CLOCKS         (1u << 2)
#define NVRM_TELEMETRY_VALID_FANS           (1u << 3)

/*
 * Kernel-only platform-function payload. Each group timestamp is copied from
 * its RUSD record and is opaque to consumers; it is not host wall-clock time.
 * Power values are milliwatts, clocks are kHz, and fan speeds are RPM.
 */
struct NVRMTelemetry {
    unsigned int version;
    unsigned int validMask;
    unsigned int fanValidMask;
    unsigned int reserved;
    unsigned long long pmGeneration;
    unsigned long long averagePowerTimestamp;
    unsigned long long instantPowerTimestamp;
    unsigned long long clocksTimestamp;
    unsigned long long fansTimestamp;
    unsigned int averagePowerMilliwatts;
    unsigned int instantPowerMilliwatts;
    unsigned int coreClockKHz;
    unsigned int memoryClockKHz;
    unsigned int fanRPM[NVRM_TELEMETRY_FAN_COUNT];
};
