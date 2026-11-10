/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#pragma once

#include "nvrm_telemetry_abi.h"

/* One publication path for native names and compatibility aliases. Invalid
 * groups remove every alias together; a measured zero remains a valid value. */
typedef void (*NVRMTelemetryPropertyWriter)(void *, const char *, int, unsigned long long);

static inline void nvrmTelemetryPublishProperties(const struct NVRMTelemetry *s,
                                                  void *context, NVRMTelemetryPropertyWriter write)
{
    const int compatible = s->version == NVRM_TELEMETRY_ABI_VERSION;
    const int average = compatible && (s->validMask & NVRM_TELEMETRY_VALID_AVERAGE_POWER);
    const int instant = compatible && (s->validMask & NVRM_TELEMETRY_VALID_INSTANT_POWER);
    const int clocks = compatible && (s->validMask & NVRM_TELEMETRY_VALID_CLOCKS);
    const unsigned fanMask = s->fanValidMask & 0xffffu;
    const int fans = compatible && (s->validMask & NVRM_TELEMETRY_VALID_FANS) && fanMask;
    const unsigned long long watts = (s->averagePowerMilliwatts + 500ULL) / 1000;
    write(context, "Total Power(W)", average, watts);
    write(context, "GPU Power(W)", average, watts);
    write(context, "GPU Average Power(mW)", average, s->averagePowerMilliwatts);
    write(context, "RUSD Power(mW)", average, s->averagePowerMilliwatts);
    write(context, "Instant Power(W)", instant, (s->instantPowerMilliwatts + 500ULL) / 1000);
    write(context, "GPU Instant Power(mW)", instant, s->instantPowerMilliwatts);
    write(context, "Core Clock(MHz)", clocks, s->coreClockKHz / 1000);
    write(context, "GPU Core Clock(MHz)", clocks, s->coreClockKHz / 1000);
    write(context, "Memory Clock(MHz)", clocks, s->memoryClockKHz / 1000);
    write(context, "GPU Memory Clock(MHz)", clocks, s->memoryClockKHz / 1000);

    unsigned maxRPM = 0, count = 0;
    for (unsigned i = 0; fans && i < NVRM_TELEMETRY_FAN_COUNT; i++) {
        if (!(fanMask & (1u << i))) continue;
        if (s->fanRPM[i] > maxRPM) maxRPM = s->fanRPM[i];
        count++;
    }
    const char *rpmKeys[] = { "Fan Speed(RPM)", "fan-rpm", "GPU Fan Speed(RPM)",
                             "Fan RPM", "GPU Fan RPM" };
    for (unsigned i = 0; i < sizeof rpmKeys / sizeof rpmKeys[0]; i++)
        write(context, rpmKeys[i], fans, maxRPM);
    // Reported controllers, not a claim about physical fan count or maximum RPM.
    write(context, "Fan Count", fans, count);
    write(context, "FanCount", fans, count);
}

static inline void nvrmTelemetryPublishMemory(unsigned long long usedBytes, int usedValid,
                                              unsigned long long totalBytes, int totalValid,
                                              void *context, NVRMTelemetryPropertyWriter write)
{
    write(context, "vramUsedBytes", usedValid, usedBytes);
    write(context, "vramTotalBytes", totalValid, totalBytes);
    // These legacy key names say MB; their established representation is MiB.
    write(context, "GPU Memory Used(MB)", usedValid, usedBytes / 1048576ULL);
    write(context, "GPU Memory Total(MB)", totalValid, totalBytes / 1048576ULL);
}
