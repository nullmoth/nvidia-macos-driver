/* Formats NVIDIA's user-shared-data block (class 0xde, public cl00de.h) as text. Pure: the caller hands it a snapshot.
 * A field whose timestamp is invalid or mid-write prints "n/a" instead of a stale number. */
#pragma once
#include <stdio.h>
#include "class/cl00de.h"

static inline int rusd_ok(NvU64 ts) { return ts != RUSD_TIMESTAMP_INVALID && ts != RUSD_TIMESTAMP_WRITE_IN_PROGRESS; }

static inline void rusd_format(const NV00DE_SHARED_DATA *d, FILE *f)
{
    const RUSD_TEMPERATURE *t = &d->temperatures[RUSD_TEMPERATURE_TYPE_GPU];
    if (rusd_ok(t->lastModifiedTimestamp)) fprintf(f, "temperature_c %.1f\n", (double)t->temperature / 256.0);
    else fprintf(f, "temperature_c n/a\n");
    if (rusd_ok(d->instPowerUsage.lastModifiedTimestamp)) fprintf(f, "power_w %.3f\n", d->instPowerUsage.info.instGpuPower / 1000.0);
    else fprintf(f, "power_w n/a\n");
    if (rusd_ok(d->powerLimitGpu.lastModifiedTimestamp))
        fprintf(f, "power_limit_w requested %.1f enforced %.1f\n", d->powerLimitGpu.info.requestedmW / 1000.0, d->powerLimitGpu.info.enforcedmW / 1000.0);
    else fprintf(f, "power_limit_w n/a\n");
    static const char *dom[] = { "graphics", "memory", "video", "sm" };
    for (int i = 0; i < RUSD_CLK_PUBLIC_DOMAIN_MAX_TYPE; i++) {
        if (rusd_ok(d->clkPublicDomainInfos.lastModifiedTimestamp))
            fprintf(f, "clock_mhz_%s %.0f target %u\n", dom[i], d->clkPublicDomainInfos.info[i].actualClkKHz / 1000.0, d->clkPublicDomainInfos.info[i].targetClkMHz);
        else fprintf(f, "clock_mhz_%s n/a\n", dom[i]);
    }
    if (rusd_ok(d->perfCurrentPstate.lastModifiedTimestamp)) fprintf(f, "pstate P%u\n", d->perfCurrentPstate.currentPstate);
    else fprintf(f, "pstate n/a\n");
    if (rusd_ok(d->perfDevUtil.lastModifiedTimestamp)) {
        fprintf(f, "util_gpu_pct %u\nutil_memory_pct %u\n", d->perfDevUtil.info.gpuPercentBusy, d->perfDevUtil.info.memoryPercentBusy);
        fprintf(f, "util_nvenc_pct %u\nutil_nvdec_pct %u\n", d->perfDevUtil.info.engUtil[RUSD_ENG_UTILIZATION_VID_ENG_NVENC].clkPercentBusy,
                d->perfDevUtil.info.engUtil[RUSD_ENG_UTILIZATION_VID_ENG_NVDEC].clkPercentBusy);
    } else fprintf(f, "util n/a\n");
    if (rusd_ok(d->clkThrottleInfo.lastModifiedTimestamp)) fprintf(f, "throttle_reasons 0x%x\n", d->clkThrottleInfo.reasonMask);
    else fprintf(f, "throttle_reasons n/a\n");
    if (rusd_ok(d->fanCoolerStatus.lastModifiedTimestamp)) fprintf(f, "fan_rpm %u\n", d->fanCoolerStatus.info.rpmCurr[0]);
    else fprintf(f, "fan_rpm n/a\n");
}
