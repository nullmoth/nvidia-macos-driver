/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#include "nvrm_telemetry_abi.h"

// RUSD stores these measurements as NvU32 values. Reject the all-ones value
// as an unavailable/impossible measurement; zero remains valid for power and
// clocks. NVIDIA's public RUSD definition does not specify a numeric RPM cap.
#define NVRM_TELEMETRY_INVALID_U32 0xffffffffu

// Pure helpers shared by the RM snapshot reader and host-side decoder tests.
// sourceValid must include both a successful RUSD seqlock copy and a valid
// RUSD source timestamp.
static inline int
nvrmTelemetryDecodeU32(int sourceValid, unsigned long long sourceTimestamp,
                       unsigned int rawValue, unsigned int *value,
                       unsigned long long *timestamp)
{
    if (value != 0) *value = 0;
    if (timestamp != 0) *timestamp = 0;
    if (!sourceValid || sourceTimestamp == 0 || value == 0 || timestamp == 0)
        return 0;

    *timestamp = sourceTimestamp;
    if (rawValue == NVRM_TELEMETRY_INVALID_U32)
        return 0;

    *value = rawValue;
    return 1;
}

static inline int
nvrmTelemetryDecodeClockPair(int sourceValid, unsigned long long sourceTimestamp,
                             unsigned int rawCoreKHz, unsigned int rawMemoryKHz,
                             unsigned int *coreKHz, unsigned int *memoryKHz,
                             unsigned long long *timestamp)
{
    if (coreKHz != 0) *coreKHz = 0;
    if (memoryKHz != 0) *memoryKHz = 0;
    if (timestamp != 0) *timestamp = 0;
    if (coreKHz == 0 || memoryKHz == 0 || timestamp == 0 ||
        !sourceValid || sourceTimestamp == 0)
        return 0;

    *timestamp = sourceTimestamp;
    if (rawCoreKHz == NVRM_TELEMETRY_INVALID_U32 ||
        rawMemoryKHz == NVRM_TELEMETRY_INVALID_U32)
        return 0;

    *coreKHz = rawCoreKHz;
    *memoryKHz = rawMemoryKHz;
    return 1;
}

static inline int
nvrmTelemetryDecodeFans(int sourceValid, unsigned long long sourceTimestamp,
                        const unsigned int rawRpm[NVRM_TELEMETRY_FAN_COUNT],
                        unsigned int *knownFanMask,
                        unsigned int outRpm[NVRM_TELEMETRY_FAN_COUNT],
                        unsigned int *outValidMask,
                        unsigned long long *timestamp)
{
    unsigned int i;

    if (outValidMask != 0) *outValidMask = 0;
    if (timestamp != 0) *timestamp = 0;
    if (outRpm != 0)
        for (i = 0; i < NVRM_TELEMETRY_FAN_COUNT; ++i)
            outRpm[i] = 0;

    if (!sourceValid || sourceTimestamp == 0 || rawRpm == 0 ||
        knownFanMask == 0 || outRpm == 0 || outValidMask == 0 || timestamp == 0)
        return 0;

    *knownFanMask &= (1u << NVRM_TELEMETRY_FAN_COUNT) - 1u;
    *timestamp = sourceTimestamp;
    for (i = 0; i < NVRM_TELEMETRY_FAN_COUNT; ++i)
    {
        unsigned int bit = 1u << i;
        unsigned int rpm = rawRpm[i];

        if (rpm != 0 && rpm != NVRM_TELEMETRY_INVALID_U32)
            *knownFanMask |= bit;

        if ((*knownFanMask & bit) != 0 && rpm != NVRM_TELEMETRY_INVALID_U32)
        {
            outRpm[i] = rpm;
            *outValidMask |= bit;
        }
    }
    return 1;
}
