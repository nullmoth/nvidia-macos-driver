/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#include <cassert>
#include <cstdio>
#include <cstring>
#include "../kexts/NVRM/nvrm_utilization.h"

// Quick test: clang++ -std=c++17 tools/test_nvrm_utilization.cpp -o /tmp/util-test
// Add -DNVRM_TEST_NVIDIA_HEADERS and OGKM's sdk/nvidia/inc + common/inc include
// paths to test NVIDIA's actual definitions. Optional arguments replay raw RM
// parameter buffers captured by a read-only userspace probe.

#ifdef NVRM_TEST_NVIDIA_HEADERS
extern "C" {
#include "ctrl/ctrl2080/ctrl2080perf.h"
}
using Params = NV2080_CTRL_PERF_GET_GPUMON_PERFMON_UTIL_SAMPLES_V2_PARAMS;
#else
// Match the public PERFMON layout while allowing a quick test without OGKM.
struct Engine {
    unsigned int util, vgpuScale, procId, subProcessID;
    char subProcessName[100];
    unsigned long long pOsPidInfo;
};
struct Sample {
    struct { unsigned long long timeStamp; } base;
    Engine fb, gr, nvenc, nvdec, nvjpg, nvofa;
};
struct Params {
    unsigned char type;
    unsigned int bufSize, count, tracker;
    Sample samples[72];
};
static_assert(sizeof(Engine) == 128, "engine ABI");
#endif
static_assert(sizeof(Params::samples[0]) == 776, "sample ABI");
static_assert(sizeof(Params) == 55888, "parameters ABI");

static Params p;
static unsigned int checks;

static void reset()
{
    std::memset(&p, 0, sizeof p);
    p.type = 2;
    p.bufSize = sizeof(p.samples);
}

static void put(unsigned int slot, unsigned long long timestamp, unsigned int gr = 1234)
{
    p.samples[slot].base.timeStamp = timestamp;
    p.samples[slot].gr.util = gr;
    p.samples[slot].fb.util = 2345;
    p.samples[slot].nvenc.util = 3456;
    p.samples[slot].nvdec.util = 4567;
}

static void succeeds(unsigned long long timestamp, unsigned int gr = 1234)
{
    NVRMUtilization out = { NVRM_UTILIZATION_ABI_VERSION, 0, 0, 0, 0, 0 };
    assert(nvrmUtilizationLatestSample(p, &out));
    assert(out.version == NVRM_UTILIZATION_ABI_VERSION);
    assert(out.timestamp == timestamp && out.gr == gr);
    assert(out.fb == 2345 && out.nvenc == 3456 && out.nvdec == 4567);
    checks++;
}

static void fails()
{
    NVRMUtilization out, before;
    std::memset(&out, 0xa5, sizeof out);
    std::memcpy(&before, &out, sizeof out);
    assert(!nvrmUtilizationLatestSample(p, &out));
    assert(std::memcmp(&before, &out, sizeof out) == 0);
    checks++;
}

int main(int argc, char **argv)
{
    reset(); fails();                                      // empty ring
    reset(); p.count = 4; p.tracker = 4;
    for (unsigned int i = 0; i < 4; i++) put(i, 100 + i);
    succeeds(103);                                        // partially filled, slot tracker

    reset(); p.count = 72; p.tracker = 0;
    for (unsigned int i = 0; i < 72; i++) put(i, 100 + i);
    succeeds(171);                                        // full ring, tail wraps to zero

    reset(); p.count = 0; p.tracker = 38 * sizeof(p.samples[0]);
    for (unsigned int i = 0; i < 72; i++) put(i, 100 + i);
    put(37, 999); succeeds(999);                            // measured GSP byte tracker/count zero
    put(38, 1000); succeeds(1000);                          // tracker and payload snapshot can differ

    p.tracker = 20; succeeds(1000);                         // public slot tracker is accepted
    p.tracker = 0; succeeds(1000);                          // wrapped byte tracker is accepted
    put(38, 1000, 0); succeeds(1000, 0);                    // real idle is valid
    put(38, 1000, 10000); succeeds(1000, 10000);            // inclusive 100 percent

    p.count = 73; fails(); p.count = 0;                     // corrupt count
    p.tracker = 72; fails();                               // neither entry nor aligned byte tracker
    p.tracker = sizeof(p.samples); fails();                // byte tracker outside ring
    p.tracker = 0;
    p.bufSize--; fails(); p.bufSize++;                     // malformed buffer size
    put(38, 1000, 10001); fails();                         // latest invalid: never fall back to old data
    put(38, 1000); p.samples[38].fb.util = 10001; fails();
    put(38, 1000); p.samples[38].nvenc.util = 10001; fails();
    put(38, 1000); p.samples[38].nvdec.util = 10001; fails();
    assert(!nvrmUtilizationLatestSample(p, nullptr)); checks++;
    std::printf("PERFMON parser: %u checks passed\n", checks);
    for (int i = 1; i < argc; i++) {
        FILE *fixture = std::fopen(argv[i], "rb");
        assert(fixture);
        assert(std::fread(&p, sizeof p, 1, fixture) == 1);
        assert(std::fgetc(fixture) == EOF);
        std::fclose(fixture);
        NVRMUtilization out = { NVRM_UTILIZATION_ABI_VERSION, 0, 0, 0, 0, 0 };
        assert(nvrmUtilizationLatestSample(p, &out));
        std::printf("%s: timestamp %llu GR %u.%02u%% FB %u.%02u%% ENC %u.%02u%% DEC %u.%02u%%\n",
                    argv[i], out.timestamp, out.gr / 100, out.gr % 100,
                    out.fb / 100, out.fb % 100, out.nvenc / 100, out.nvenc % 100,
                    out.nvdec / 100, out.nvdec % 100);
    }
}
