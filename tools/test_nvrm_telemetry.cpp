/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <string>

#include "../kexts/NVRM/nvrm_telemetry_cache.h"
#include "../kexts/NVRM/nvrm_telemetry_decode.h"
#include "../kexts/NVRM/nvrm_telemetry_properties.h"

static unsigned int checks;
static unsigned int maskChecks;

static struct NVRMTelemetry makeSample(unsigned long long generation,
                                       unsigned int validMask,
                                       unsigned long long baseTimestamp)
{
    struct NVRMTelemetry s = {};
    s.version = NVRM_TELEMETRY_ABI_VERSION;
    s.validMask = validMask;
    s.fanValidMask = 0x3u;
    s.pmGeneration = generation;
    s.averagePowerTimestamp = baseTimestamp;
    s.instantPowerTimestamp = baseTimestamp + 1;
    s.clocksTimestamp = baseTimestamp + 2;
    s.fansTimestamp = baseTimestamp + 3;
    s.averagePowerMilliwatts = 125000;
    s.instantPowerMilliwatts = 130000;
    s.coreClockKHz = 1500000;
    s.memoryClockKHz = 9500000;
    s.fanRPM[0] = 0;       // stopped fans are real zero-RPM samples
    s.fanRPM[1] = 1200;
    s.fanRPM[2] = 9000;    // not included in fanValidMask, so must be ignored
    return s;
}

static struct NVRMTelemetry allGroups(unsigned long long generation,
                                     unsigned long long baseTimestamp)
{
    return makeSample(generation,
                      NVRM_TELEMETRY_VALID_AVERAGE_POWER |
                      NVRM_TELEMETRY_VALID_INSTANT_POWER |
                      NVRM_TELEMETRY_VALID_CLOCKS |
                      NVRM_TELEMETRY_VALID_FANS,
                      baseTimestamp);
}

static struct NVRMTelemetry primeCache(struct NVRMTelemetryCache *cache,
                                       struct NVRMTelemetry sample,
                                       unsigned long long nowNs)
{
    nvrmTelemetryCacheUpdate(cache, &sample, nowNs); // first provider record is only a baseline
    sample.averagePowerTimestamp++;
    sample.instantPowerTimestamp++;
    sample.clocksTimestamp++;
    sample.fansTimestamp++;
    nvrmTelemetryCacheUpdate(cache, &sample, nowNs);
    return sample;
}

static void expectMask(struct NVRMTelemetry sample, unsigned int mask)
{
    maskChecks++;
    if (sample.validMask != mask)
        std::fprintf(stderr, "validMask check %u mismatch: got 0x%x expected 0x%x\n", maskChecks, sample.validMask, mask);
    assert(sample.validMask == mask);
    checks++;
}

static void expect(bool condition)
{
    checks++;
    if (!condition) {
        std::fprintf(stderr, "telemetry decode check %u failed\n", checks);
        std::abort();
    }
}

static void testInitialAndZeroSamples()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry sample = allGroups(7, 100);
    sample.averagePowerMilliwatts = 0;
    sample.instantPowerMilliwatts = 0;
    sample.coreClockKHz = 0;
    sample.memoryClockKHz = 0;
    sample.fanValidMask = 0x1u;
    sample.fanRPM[0] = 0;

    nvrmTelemetryCacheUpdate(&cache, &sample, 1000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 1000, 5000000000ULL);
    expectMask(got, 0); // the initial source record may predate this provider
    sample.averagePowerTimestamp++;
    sample.instantPowerTimestamp++;
    sample.clocksTimestamp++;
    sample.fansTimestamp++;
    nvrmTelemetryCacheUpdate(&cache, &sample, 1000);
    got = nvrmTelemetryCacheSnapshot(&cache, 1000, 5000000000ULL);
    expectMask(got, sample.validMask);
    assert(got.averagePowerMilliwatts == 0 && got.instantPowerMilliwatts == 0);
    assert(got.coreClockKHz == 0 && got.memoryClockKHz == 0);
    assert(got.fanValidMask == 0x1u && got.fanRPM[0] == 0);
    assert(got.fanRPM[1] == 0 && got.fanRPM[2] == 0);
    assert(got.pmGeneration == 7);
    checks += 5;
}

static void testRepeatedTimestampDoesNotRefresh()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry first = allGroups(1, 1000);
    first = primeCache(&cache, first, 1000000000ULL);

    struct NVRMTelemetry duplicate = first;
    duplicate.averagePowerMilliwatts = 999999;
    duplicate.coreClockKHz = 7770000;
    duplicate.fanRPM[1] = 4000;
    nvrmTelemetryCacheUpdate(&cache, &duplicate, 5000000000ULL);

    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 6000000000ULL, 5000000000ULL);
    expectMask(got, first.validMask); // exactly five seconds from initial arrival remains fresh
    assert(got.averagePowerMilliwatts == first.averagePowerMilliwatts);
    assert(got.coreClockKHz == first.coreClockKHz);
    assert(got.fanRPM[1] == first.fanRPM[1]);
    checks += 3;

    got = nvrmTelemetryCacheSnapshot(&cache, 6000000001ULL, 5000000000ULL);
    expectMask(got, 0); // repeated source timestamps did not move arrivalNs
    assert(got.averagePowerMilliwatts == 0 && got.coreClockKHz == 0 && got.fanValidMask == 0);
    checks++;

    /* Expiry cannot be undone by presenting the same source record again. */
    nvrmTelemetryCacheUpdate(&cache, &duplicate, 7000000000ULL);
    got = nvrmTelemetryCacheSnapshot(&cache, 7000000000ULL, 5000000000ULL);
    expectMask(got, 0);

    struct NVRMTelemetry newer = allGroups(1, 1010);
    nvrmTelemetryCacheUpdate(&cache, &newer, 7100000000ULL);
    got = nvrmTelemetryCacheSnapshot(&cache, 7100000000ULL, 5000000000ULL);
    expectMask(got, newer.validMask);
    checks += 2;
}

static void testPartialValidityAndFanMask()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry first = allGroups(2, 2000);
    first = primeCache(&cache, first, 1000);

    struct NVRMTelemetry partial = first;
    partial.validMask = NVRM_TELEMETRY_VALID_AVERAGE_POWER | NVRM_TELEMETRY_VALID_FANS;
    partial.fansTimestamp++;
    partial.fanValidMask = 0x5u;
    partial.fanRPM[0] = 0;
    partial.fanRPM[2] = 500;
    partial.fanRPM[3] = 42000;
    nvrmTelemetryCacheUpdate(&cache, &partial, 2000);

    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, NVRM_TELEMETRY_VALID_AVERAGE_POWER | NVRM_TELEMETRY_VALID_FANS);
    assert(got.averagePowerMilliwatts == first.averagePowerMilliwatts);
    assert(got.instantPowerMilliwatts == 0 && got.coreClockKHz == 0 && got.memoryClockKHz == 0);
    assert(got.fanValidMask == 0x5u);
    assert(got.fanRPM[0] == 0 && got.fanRPM[1] == 0 && got.fanRPM[2] == 500);
    assert(got.fanRPM[3] == 0); // a value without its per-fan valid bit is not published
    checks += 5;
}

static void testFailureAndRejectedTimestamp()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry first = allGroups(3, 3000);
    first = primeCache(&cache, first, 1000);

    nvrmTelemetryCacheUpdate(&cache, nullptr, 2000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, 0);

    nvrmTelemetryCacheUpdate(&cache, &first, 3000);
    got = nvrmTelemetryCacheSnapshot(&cache, 3000, 5000000000ULL);
    expectMask(got, 0); // a successful call with the prior timestamp is not new data

    struct NVRMTelemetry newer = allGroups(3, 3010);
    nvrmTelemetryCacheUpdate(&cache, &newer, 4000);
    got = nvrmTelemetryCacheSnapshot(&cache, 4000, 5000000000ULL);
    expectMask(got, newer.validMask);
    checks += 2;

    /* A stable timestamp whose value was rejected consumes that timestamp too. */
    struct NVRMTelemetry rejected = newer;
    rejected.validMask &= ~NVRM_TELEMETRY_VALID_CLOCKS;
    rejected.clocksTimestamp = 3015;
    nvrmTelemetryCacheUpdate(&cache, &rejected, 5000);
    got = nvrmTelemetryCacheSnapshot(&cache, 5000, 5000000000ULL);
    expectMask(got, newer.validMask & ~NVRM_TELEMETRY_VALID_CLOCKS);

    rejected.validMask |= NVRM_TELEMETRY_VALID_CLOCKS;
    rejected.coreClockKHz = 2500000;
    nvrmTelemetryCacheUpdate(&cache, &rejected, 6000);
    got = nvrmTelemetryCacheSnapshot(&cache, 6000, 5000000000ULL);
    expectMask(got, newer.validMask & ~NVRM_TELEMETRY_VALID_CLOCKS);

    rejected.clocksTimestamp = 3016;
    nvrmTelemetryCacheUpdate(&cache, &rejected, 7000);
    got = nvrmTelemetryCacheSnapshot(&cache, 7000, 5000000000ULL);
    expectMask(got, newer.validMask);
    assert(got.coreClockKHz == 2500000);
    checks += 2;
}

static void testGenerationAndTimestampEpochs()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry first = allGroups(10, 4000);
    first = primeCache(&cache, first, 1000);

    struct NVRMTelemetry newGeneration = allGroups(11, 5000);
    nvrmTelemetryCacheUpdate(&cache, &newGeneration, 2000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, 0); // first post-resume sample is only the epoch baseline

    newGeneration.averagePowerMilliwatts++;
    nvrmTelemetryCacheUpdate(&cache, &newGeneration, 3000);
    got = nvrmTelemetryCacheSnapshot(&cache, 3000, 5000000000ULL);
    expectMask(got, 0); // re-reading the exact first post-resume sample cannot revive it

    struct NVRMTelemetry resumed = allGroups(11, 5010);
    resumed.averagePowerMilliwatts = 0;
    nvrmTelemetryCacheUpdate(&cache, &resumed, 4000);
    got = nvrmTelemetryCacheSnapshot(&cache, 4000, 5000000000ULL);
    expectMask(got, resumed.validMask);
    assert(got.averagePowerMilliwatts == 0);
    checks += 2;

    /* A regression invalidates data but keeps each group's high watermark;
     * neither the lower timestamp nor the former high sample can revive it. */
    struct NVRMTelemetry resetClock = allGroups(11, 100);
    nvrmTelemetryCacheUpdate(&cache, &resetClock, 5000);
    got = nvrmTelemetryCacheSnapshot(&cache, 5000, 5000000000ULL);
    expectMask(got, 0);
    nvrmTelemetryCacheUpdate(&cache, &resetClock, 6000);
    got = nvrmTelemetryCacheSnapshot(&cache, 6000, 5000000000ULL);
    expectMask(got, 0);
    struct NVRMTelemetry oldTimestamp = allGroups(11, 5010);
    nvrmTelemetryCacheUpdate(&cache, &oldTimestamp, 6500);
    got = nvrmTelemetryCacheSnapshot(&cache, 6500, 5000000000ULL);
    expectMask(got, 0);
    struct NVRMTelemetry afterReset = allGroups(11, 5011);
    nvrmTelemetryCacheUpdate(&cache, &afterReset, 7000);
    got = nvrmTelemetryCacheSnapshot(&cache, 7000, 5000000000ULL);
    expectMask(got, afterReset.validMask);
    checks += 2;
}

static void testInvalidTimestampsAndPerGroupArrival()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry sample = allGroups(1, 6000);
    sample.averagePowerTimestamp = 0;
    sample.instantPowerTimestamp = NVRM_TELEMETRY_CACHE_RUSD_WRITE_IN_PROGRESS;
    sample.clocksTimestamp = NVRM_TELEMETRY_CACHE_RUSD_SEQ_START | 1ULL;
    nvrmTelemetryCacheUpdate(&cache, &sample, 1000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 1000, 5000000000ULL);
    expectMask(got, 0); // first provider record is baseline even if its data bits are set

    /* The first usable timestamp after an invalid initial record is a baseline;
     * only a later one establishes a fresh sample. */
    sample.averagePowerTimestamp = 6100;
    sample.instantPowerTimestamp = 6101;
    sample.clocksTimestamp = 6102;
    nvrmTelemetryCacheUpdate(&cache, &sample, 2000);
    got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, 0); // newly usable fields establish baselines; fan timestamp is unchanged
    sample.averagePowerTimestamp++;
    sample.instantPowerTimestamp++;
    sample.clocksTimestamp++;
    sample.fansTimestamp++;
    nvrmTelemetryCacheUpdate(&cache, &sample, 3000);
    got = nvrmTelemetryCacheSnapshot(&cache, 3000, 5000000000ULL);
    expectMask(got, sample.validMask);
    checks += 2;

    got = nvrmTelemetryCacheSnapshot(&cache, 2500, 5000000000ULL);
    expectMask(got, 0); // the source arrivals are later than this snapshot time
    got = nvrmTelemetryCacheSnapshot(&cache, 3100, 5000000000ULL);
    expectMask(got, sample.validMask); // snapshotting is read-only; mach_continuous_time is monotonic
    nvrmTelemetryCacheUpdate(&cache, &sample, 3100);
    got = nvrmTelemetryCacheSnapshot(&cache, 3100, 5000000000ULL);
    expectMask(got, sample.validMask);
    sample.averagePowerTimestamp++;
    sample.instantPowerTimestamp++;
    sample.clocksTimestamp++;
    sample.fansTimestamp++;
    nvrmTelemetryCacheUpdate(&cache, &sample, 3200);
    got = nvrmTelemetryCacheSnapshot(&cache, 3200, 5000000000ULL);
    expectMask(got, sample.validMask);
    checks += 4;
}

static void testProviderResetAndNullArguments()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry sample = allGroups(1, 7000);
    nvrmTelemetryCacheUpdate(&cache, &sample, 1000);
    cache = {}; // a different provider has no prior source history
    sample.pmGeneration = 90;
    sample.averagePowerMilliwatts = 0;
    nvrmTelemetryCacheUpdate(&cache, &sample, 2000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, 0); // a provider change discards all previous source history
    sample.averagePowerTimestamp++;
    sample.instantPowerTimestamp++;
    sample.clocksTimestamp++;
    sample.fansTimestamp++;
    nvrmTelemetryCacheUpdate(&cache, &sample, 2000);
    got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, sample.validMask);
    assert(got.pmGeneration == 90 && got.averagePowerMilliwatts == 0);
    nvrmTelemetryCacheUpdate(nullptr, &sample, 3000);
    got = nvrmTelemetryCacheSnapshot(nullptr, 3000, 5000000000ULL);
    expectMask(got, 0);
    assert(got.version == NVRM_TELEMETRY_ABI_VERSION);
    checks += 2;
}

static void testScalarAndClockDecoders()
{
    unsigned int value = 99;
    unsigned long long timestamp = 99;
    expect(nvrmTelemetryDecodeU32(1, 1234, 0, &value, &timestamp));
    expect(value == 0 && timestamp == 1234);

    value = 99; timestamp = 99;
    expect(!nvrmTelemetryDecodeU32(0, 1235, 10, &value, &timestamp));
    expect(value == 0 && timestamp == 0);
    value = 99; timestamp = 99;
    expect(!nvrmTelemetryDecodeU32(1, 0, 10, &value, &timestamp));
    expect(value == 0 && timestamp == 0);
    value = 99; timestamp = 99;
    expect(!nvrmTelemetryDecodeU32(1, 1236, NVRM_TELEMETRY_INVALID_U32, &value, &timestamp));
    expect(value == 0 && timestamp == 1236); // consume stable source TS despite invalid raw value

    unsigned int coreKHz = 99, memoryKHz = 99;
    timestamp = 99;
    expect(nvrmTelemetryDecodeClockPair(1, 2345, 0, 0,
                                       &coreKHz, &memoryKHz, &timestamp));
    expect(coreKHz == 0 && memoryKHz == 0 && timestamp == 2345);
    coreKHz = memoryKHz = 99; timestamp = 99;
    expect(!nvrmTelemetryDecodeClockPair(1, 2346, 1000000,
                                          NVRM_TELEMETRY_INVALID_U32,
                                          &coreKHz, &memoryKHz, &timestamp));
    expect(coreKHz == 0 && memoryKHz == 0 && timestamp == 2346);
}

static void testFanDecoder()
{
    unsigned int raw[NVRM_TELEMETRY_FAN_COUNT] = {};
    unsigned int output[NVRM_TELEMETRY_FAN_COUNT];
    for (unsigned int i = 0; i < NVRM_TELEMETRY_FAN_COUNT; i++)
        output[i] = 77;
    raw[1] = NVRM_TELEMETRY_INVALID_U32;
    raw[4] = 1200;
    raw[10] = 900;
    unsigned int knownMask = 0x80000000u;
    unsigned int validMask = 0xffffffffu;
    unsigned long long timestamp = 99;

    expect(nvrmTelemetryDecodeFans(1, 3456, raw, &knownMask, output, &validMask, &timestamp));
    expect(timestamp == 3456);
    expect(knownMask == ((1u << 4) | (1u << 10)));
    expect(validMask == knownMask); // unknown zero slots and sentinels stay absent
    expect(output[0] == 0 && output[1] == 0 && output[4] == 1200 && output[10] == 900);
    expect(output[5] == 0 && output[9] == 0); // sparse slots are not filled

    raw[4] = 0; // a previously observed fan may legitimately stop
    raw[10] = NVRM_TELEMETRY_INVALID_U32;
    expect(nvrmTelemetryDecodeFans(1, 3457, raw, &knownMask, output, &validMask, &timestamp));
    expect(timestamp == 3457 && validMask == (1u << 4));
    expect(output[4] == 0 && output[10] == 0);

    for (unsigned int i = 0; i < NVRM_TELEMETRY_FAN_COUNT; i++)
        output[i] = 77;
    validMask = 0xffffffffu; timestamp = 99;
    expect(!nvrmTelemetryDecodeFans(0, 3458, raw, &knownMask, output, &validMask, &timestamp));
    expect(validMask == 0 && timestamp == 0 && output[4] == 0);
    validMask = 0xffffffffu; timestamp = 99;
    expect(!nvrmTelemetryDecodeFans(1, 0, raw, &knownMask, output, &validMask, &timestamp));
    expect(validMask == 0 && timestamp == 0);
}

static void testInvalidRawTimestampCannotBeReplayed()
{
    struct NVRMTelemetryCache cache = {};
    struct NVRMTelemetry sample = {};
    sample.version = NVRM_TELEMETRY_ABI_VERSION;
    sample.pmGeneration = 3;
    sample.validMask = NVRM_TELEMETRY_VALID_AVERAGE_POWER;

    sample.averagePowerTimestamp = 100;
    expect(nvrmTelemetryDecodeU32(1, sample.averagePowerTimestamp, 90000,
                                  &sample.averagePowerMilliwatts,
                                  &sample.averagePowerTimestamp));
    nvrmTelemetryCacheUpdate(&cache, &sample, 1000); // initial provider baseline
    expectMask(nvrmTelemetryCacheSnapshot(&cache, 1000, 5000000000ULL), 0);

    sample.averagePowerTimestamp = 101;
    expect(nvrmTelemetryDecodeU32(1, sample.averagePowerTimestamp, 100000,
                                  &sample.averagePowerMilliwatts,
                                  &sample.averagePowerTimestamp));
    nvrmTelemetryCacheUpdate(&cache, &sample, 2000);
    struct NVRMTelemetry got = nvrmTelemetryCacheSnapshot(&cache, 2000, 5000000000ULL);
    expectMask(got, NVRM_TELEMETRY_VALID_AVERAGE_POWER);
    expect(got.averagePowerTimestamp == 101 && got.averagePowerMilliwatts == 100000);

    sample.validMask = 0;
    expect(!nvrmTelemetryDecodeU32(1, 102, NVRM_TELEMETRY_INVALID_U32,
                                  &sample.averagePowerMilliwatts,
                                  &sample.averagePowerTimestamp));
    expect(sample.averagePowerMilliwatts == 0 && sample.averagePowerTimestamp == 102);
    nvrmTelemetryCacheUpdate(&cache, &sample, 3000);
    expectMask(nvrmTelemetryCacheSnapshot(&cache, 3000, 5000000000ULL), 0);

    sample.validMask = NVRM_TELEMETRY_VALID_AVERAGE_POWER;
    expect(nvrmTelemetryDecodeU32(1, 102, 200000,
                                  &sample.averagePowerMilliwatts,
                                  &sample.averagePowerTimestamp));
    nvrmTelemetryCacheUpdate(&cache, &sample, 4000);
    expectMask(nvrmTelemetryCacheSnapshot(&cache, 4000, 5000000000ULL), 0);

    expect(nvrmTelemetryDecodeU32(1, 103, 210000,
                                  &sample.averagePowerMilliwatts,
                                  &sample.averagePowerTimestamp));
    nvrmTelemetryCacheUpdate(&cache, &sample, 5000);
    got = nvrmTelemetryCacheSnapshot(&cache, 5000, 5000000000ULL);
    expectMask(got, NVRM_TELEMETRY_VALID_AVERAGE_POWER);
    expect(got.averagePowerTimestamp == 103 && got.averagePowerMilliwatts == 210000);
}

using PublishedProperties = std::map<std::string, unsigned long long>;

static void publishProperty(void *context, const char *key, int valid, unsigned long long value)
{
    auto &properties = *static_cast<PublishedProperties *>(context);
    if (valid) properties[key] = value;
    else properties.erase(key);
}

static void testConsumerProperties()
{
    PublishedProperties p;
    NVRMTelemetryCache cache = {};
    auto sample = allGroups(1, 100);
    sample.averagePowerMilliwatts = 125500;
    sample.coreClockKHz = 1905999;
    sample.memoryClockKHz = 9501999;
    sample.fanValidMask = (1u << 0) | (1u << 3);
    sample.fanRPM[0] = 1187;
    sample.fanRPM[3] = 1022;
    sample.fanRPM[1] = 99999; // excluded controller must never affect max RPM
    primeCache(&cache, sample, 1000);
    auto got = nvrmTelemetryCacheSnapshot(&cache, 1000, 5000);
    nvrmTelemetryPublishProperties(&got, &p, publishProperty);
    expect(p.at("Core Clock(MHz)") == 1905); // Stats reads integer MHz
    expect(p.at("Memory Clock(MHz)") == 9501);
    expect(p.at("GPU Core Clock(MHz)") == p.at("Core Clock(MHz)"));
    expect(p.at("GPU Memory Clock(MHz)") == p.at("Memory Clock(MHz)"));
    expect(p.at("Total Power(W)") == 126 && p.at("GPU Power(W)") == 126);
    expect(p.at("RUSD Power(mW)") == 125500 && p.at("GPU Average Power(mW)") == 125500);
    expect(p.at("Fan Speed(RPM)") == 1187 && p.at("fan-rpm") == 1187);
    expect(p.at("Fan Count") == 2 && p.at("FanCount") == 2);
    expect(p.count("Fan Speed(%)") == 0 && p.count("fan-percentage") == 0);
    expect(p.count("FanMaxRPM") == 0);

    got.validMask &= ~NVRM_TELEMETRY_VALID_AVERAGE_POWER;
    nvrmTelemetryPublishProperties(&got, &p, publishProperty);
    expect(!p.count("Total Power(W)") && !p.count("GPU Power(W)") && !p.count("RUSD Power(mW)"));
    expect(p.at("Core Clock(MHz)") == 1905 && p.at("fan-rpm") == 1187);

    got = nvrmTelemetryCacheSnapshot(&cache, 6001, 5000);
    nvrmTelemetryPublishProperties(&got, &p, publishProperty);
    expect(p.empty()); // expiry removes every compatibility alias too

    got = allGroups(1, 200);
    got.averagePowerMilliwatts = got.instantPowerMilliwatts = 0;
    got.coreClockKHz = got.memoryClockKHz = 0;
    got.fanValidMask = 1;
    got.fanRPM[0] = 0;
    nvrmTelemetryPublishProperties(&got, &p, publishProperty);
    expect(p.at("Total Power(W)") == 0 && p.at("RUSD Power(mW)") == 0);
    expect(p.at("Core Clock(MHz)") == 0 && p.at("GPU Memory Clock(MHz)") == 0);
    expect(p.at("Fan Speed(RPM)") == 0 && p.at("GPU Fan RPM") == 0 && p.at("FanCount") == 1);

    nvrmTelemetryPublishMemory(6ULL << 30, 1, 10ULL << 30, 1, &p, publishProperty);
    expect(p.at("vramUsedBytes") == (6ULL << 30));
    expect(p.at("vramTotalBytes") == (10ULL << 30));
    expect(p.at("GPU Memory Used(MB)") == 6144 && p.at("GPU Memory Total(MB)") == 10240);
    nvrmTelemetryPublishMemory(0, 1, 10ULL << 30, 1, &p, publishProperty);
    expect(p.at("vramUsedBytes") == 0 && p.at("GPU Memory Used(MB)") == 0);
    nvrmTelemetryPublishMemory(0, 0, 0, 0, &p, publishProperty);
    expect(!p.count("vramUsedBytes") && !p.count("vramTotalBytes") && !p.count("GPU Memory Used(MB)"));
}

int main()
{
    testInitialAndZeroSamples();
    testRepeatedTimestampDoesNotRefresh();
    testPartialValidityAndFanMask();
    testFailureAndRejectedTimestamp();
    testGenerationAndTimestampEpochs();
    testInvalidTimestampsAndPerGroupArrival();
    testProviderResetAndNullArguments();
    testScalarAndClockDecoders();
    testFanDecoder();
    testInvalidRawTimestampCannotBeReplayed();
    testConsumerProperties();
    std::printf("NVRM telemetry cache: %u checks passed\n", checks);
}
