/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once

#include "nvrm_telemetry_abi.h"

#define NVRM_TELEMETRY_CACHE_DEFAULT_MAX_AGE_NS 5000000000ULL
#define NVRM_TELEMETRY_CACHE_RUSD_SEQ_START 0xFF00000000000000ULL
#define NVRM_TELEMETRY_CACHE_RUSD_WRITE_IN_PROGRESS (~0ULL)

enum { NVRM_TELEMETRY_CACHE_AVERAGE_POWER, NVRM_TELEMETRY_CACHE_INSTANT_POWER,
       NVRM_TELEMETRY_CACHE_CLOCKS, NVRM_TELEMETRY_CACHE_FANS,
       NVRM_TELEMETRY_CACHE_GROUP_COUNT };

/* All groups are polled; zero and values >= RUSD_SEQ_START are invalid. */
struct NVRMTelemetryCacheGroup {
    unsigned long long sourceTimestamp;
    unsigned long long arrivalNs;
    unsigned int hasSourceTimestamp;
    unsigned int valid;
    unsigned int needsNewTimestamp;
};

struct NVRMTelemetryCache {
    struct NVRMTelemetry snapshot;
    struct NVRMTelemetryCacheGroup groups[NVRM_TELEMETRY_CACHE_GROUP_COUNT];
    unsigned long long pmGeneration;
    unsigned int hasPmGeneration;
};

static inline unsigned int nvrmTelemetryCacheValidBit(unsigned int group)
{
    return group < NVRM_TELEMETRY_CACHE_GROUP_COUNT ? (1u << group) : 0;
}

static inline unsigned long long nvrmTelemetryCacheSourceTimestamp(const struct NVRMTelemetry *sample,
                                                                   unsigned int group)
{
    switch (group) {
        case NVRM_TELEMETRY_CACHE_AVERAGE_POWER: return sample->averagePowerTimestamp;
        case NVRM_TELEMETRY_CACHE_INSTANT_POWER: return sample->instantPowerTimestamp;
        case NVRM_TELEMETRY_CACHE_CLOCKS: return sample->clocksTimestamp;
        case NVRM_TELEMETRY_CACHE_FANS: return sample->fansTimestamp;
        default: return 0;
    }
}

static inline void nvrmTelemetryCacheCopyPayload(struct NVRMTelemetry *dst,
                                                  const struct NVRMTelemetry *src, unsigned int group)
{
    switch (group) {
        case NVRM_TELEMETRY_CACHE_AVERAGE_POWER:
            dst->averagePowerTimestamp = src->averagePowerTimestamp;
            dst->averagePowerMilliwatts = src->averagePowerMilliwatts;
            break;
        case NVRM_TELEMETRY_CACHE_INSTANT_POWER:
            dst->instantPowerTimestamp = src->instantPowerTimestamp;
            dst->instantPowerMilliwatts = src->instantPowerMilliwatts;
            break;
        case NVRM_TELEMETRY_CACHE_CLOCKS:
            dst->clocksTimestamp = src->clocksTimestamp; dst->coreClockKHz = src->coreClockKHz;
            dst->memoryClockKHz = src->memoryClockKHz;
            break;
        case NVRM_TELEMETRY_CACHE_FANS:
            dst->fansTimestamp = src->fansTimestamp; dst->fanValidMask = src->fanValidMask & 0xffffu;
            for (unsigned int i = 0; i < NVRM_TELEMETRY_FAN_COUNT; i++)
                dst->fanRPM[i] = (dst->fanValidMask & (1u << i)) ? src->fanRPM[i] : 0;
            break;
    }
}

static inline void nvrmTelemetryCacheClearPayload(struct NVRMTelemetry *s, unsigned int group)
{
    const struct NVRMTelemetry zero = {};
    nvrmTelemetryCacheCopyPayload(s, &zero, group);
    s->validMask &= ~nvrmTelemetryCacheValidBit(group);
}

static inline int nvrmTelemetryCacheTimestampUsable(unsigned long long timestamp)
{ return timestamp != 0 && timestamp < NVRM_TELEMETRY_CACHE_RUSD_SEQ_START; }

static inline void nvrmTelemetryCacheInvalidateGroup(struct NVRMTelemetryCacheGroup *group)
{ group->valid = group->arrivalNs = 0; group->needsNewTimestamp = 1; }

/* 2=new, 1=unchanged, 0=invalid. Consume rejected timestamps to prevent replay. */
static inline int nvrmTelemetryCacheObserveGroup(struct NVRMTelemetryCacheGroup *group,
                                                  int sampleValid, unsigned long long timestamp,
                                                  unsigned long long nowNs)
{
    const int usable = nvrmTelemetryCacheTimestampUsable(timestamp);
    if (!sampleValid || !usable) {
        nvrmTelemetryCacheInvalidateGroup(group);
        if (usable && (!group->hasSourceTimestamp || timestamp > group->sourceTimestamp)) {
            group->sourceTimestamp = timestamp; group->hasSourceTimestamp = 1;
        }
        return 0;
    }

    if (!group->hasSourceTimestamp) {
        group->sourceTimestamp = timestamp; group->hasSourceTimestamp = 1;
        if (group->needsNewTimestamp) return 0;
        group->arrivalNs = nowNs; group->valid = 1;
        return 2;
    }
    if (timestamp < group->sourceTimestamp) {
        nvrmTelemetryCacheInvalidateGroup(group);
        return 0;
    }
    if (timestamp == group->sourceTimestamp)
        return group->valid ? 1 : 0;

    group->sourceTimestamp = timestamp; group->arrivalNs = nowNs;
    group->valid = 1; group->needsNewTimestamp = 0;
    return 2;
}

static inline void nvrmTelemetryCacheUpdate(struct NVRMTelemetryCache *cache,
                                             const struct NVRMTelemetry *sample, unsigned long long nowNs)
{
    if (!cache) return;
    if (!sample || sample->version != NVRM_TELEMETRY_ABI_VERSION) {
        for (unsigned int i = 0; i < NVRM_TELEMETRY_CACHE_GROUP_COUNT; i++) {
            nvrmTelemetryCacheInvalidateGroup(&cache->groups[i]);
            nvrmTelemetryCacheClearPayload(&cache->snapshot, i);
        }
        return;
    }

    const int newEpoch = !cache->hasPmGeneration || cache->pmGeneration != sample->pmGeneration;
    if (newEpoch) {
        /* The first provider record and each post-PM record establish a
         * baseline; a later source timestamp proves polling produced new data. */
        for (unsigned int i = 0; i < NVRM_TELEMETRY_CACHE_GROUP_COUNT; i++) {
            cache->groups[i] = {};
            cache->groups[i].needsNewTimestamp = 1;
            nvrmTelemetryCacheClearPayload(&cache->snapshot, i);
        }
    }
    cache->pmGeneration = sample->pmGeneration;
    cache->hasPmGeneration = 1;

    for (unsigned int i = 0; i < NVRM_TELEMETRY_CACHE_GROUP_COUNT; i++) {
        const unsigned int bit = nvrmTelemetryCacheValidBit(i);
        const int result = nvrmTelemetryCacheObserveGroup(&cache->groups[i],
            !newEpoch && (sample->validMask & bit), nvrmTelemetryCacheSourceTimestamp(sample, i), nowNs);
        if (result == 2)
            nvrmTelemetryCacheCopyPayload(&cache->snapshot, sample, i);
        if (cache->groups[i].valid)
            cache->snapshot.validMask |= bit;
        else
            nvrmTelemetryCacheClearPayload(&cache->snapshot, i);
    }
    cache->snapshot.version = NVRM_TELEMETRY_ABI_VERSION;
    cache->snapshot.pmGeneration = cache->pmGeneration;
}

static inline struct NVRMTelemetry nvrmTelemetryCacheSnapshot(
    const struct NVRMTelemetryCache *cache, unsigned long long nowNs, unsigned long long maxAgeNs)
{
    struct NVRMTelemetry out = {};
    out.version = NVRM_TELEMETRY_ABI_VERSION;
    if (!cache) return out;
    out.pmGeneration = cache->pmGeneration;

    for (unsigned int i = 0; i < NVRM_TELEMETRY_CACHE_GROUP_COUNT; i++) {
        const struct NVRMTelemetryCacheGroup *group = &cache->groups[i];
        if (group->valid && nowNs >= group->arrivalNs && nowNs - group->arrivalNs <= maxAgeNs) {
            out.validMask |= nvrmTelemetryCacheValidBit(i);
            nvrmTelemetryCacheCopyPayload(&out, &cache->snapshot, i);
        }
    }
    return out;
}
