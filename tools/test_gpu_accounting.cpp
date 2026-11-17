/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#include "../plugin/nvmtl_gpu_accounting.h"
#include "../kexts/NVRM/nvrm_gpu_accounting_abi.h"
#include <algorithm>
#include <array>
#include <cassert>
#include <cstdio>
#include <cstring>
#include <random>
#include <vector>

static void invariants(const nvmtl_gpu_accounting_state &state)
{
    assert(state.count <= NVMTL_GPU_ACCOUNTING_RANGES);
    for (unsigned i = 0; i < state.count; ++i) {
        assert(state.ranges[i].start >= state.floor);
        assert(state.ranges[i].end > state.ranges[i].start);
        if (i) assert(state.ranges[i - 1].end < state.ranges[i].start);
    }
}

int main()
{
    static_assert(sizeof(nv_gpu_accounting_report) == 32, "versioned report ABI");
    nvmtl_gpu_accounting_state state = {};
    assert(!nvmtl_gpu_accounting_add(&state, 1, 2, 0));
    assert(!nvmtl_gpu_accounting_add(&state, 0, 2, 1));
    assert(!nvmtl_gpu_accounting_add(&state, 2, 2, 1));
    assert(!nvmtl_gpu_accounting_add(&state, 3, 2, 1));
    assert(state.busy_ns == 0 && state.count == 0);
    assert(nvmtl_gpu_accounting_add(&state, 30, 50, 3));
    assert(nvmtl_gpu_accounting_add(&state, 10, 20, 1)); // earlier completion
    assert(nvmtl_gpu_accounting_add(&state, 15, 40, 2)); // bridges both intervals
    assert(state.busy_ns == 40 && state.count == 1);
    assert(!nvmtl_gpu_accounting_add(&state, 15, 40, 2)); // duplicate
    assert(!nvmtl_gpu_accounting_add(&state, 20, 30, 2)); // fully covered
    assert(nvmtl_gpu_accounting_add(&state, 50, 60, 4)); // touching
    assert(state.busy_ns == 50 && state.last_submit_absolute == 4);

    // Compare arbitrarily reordered and overlapping completions against an
    // independent occupancy oracle, including duplicate callback delivery.
    std::mt19937 random(0x475055);
    for (unsigned trial = 0; trial < 100; ++trial) {
        state = {};
        std::array<bool, 4096> occupied = {};
        uint64_t previous = 0;
        for (unsigned i = 1; i <= 2000; ++i) {
            unsigned a = 1 + random() % 4000, b = std::min(4096u, a + 1 + random() % 96);
            nvmtl_gpu_accounting_add(&state, a, b, i);
            nvmtl_gpu_accounting_add(&state, a, b, i);
            for (unsigned j = a; j < b; ++j) occupied[j] = true;
            assert(state.busy_ns == (uint64_t)std::count(occupied.begin(), occupied.end(), true));
            assert(state.busy_ns >= previous); previous = state.busy_ns;
            invariants(state);
        }
    }

    state = {};
    for (unsigned i = 0; i < 3 * NVMTL_GPU_ACCOUNTING_RANGES; ++i) {
        assert(nvmtl_gpu_accounting_add(&state, 10 * i + 1, 10 * i + 4, i + 1));
        invariants(state);
    }
    const uint64_t counted = state.busy_ns, floor = state.floor;
    assert(!nvmtl_gpu_accounting_add(&state, 1, 4, 1)); // retired history cannot replay
    assert(state.busy_ns == counted && state.floor == floor);
    assert(nvmtl_gpu_accounting_add(&state, floor + 1, floor + 2, 1000));
    assert(!nvmtl_gpu_accounting_add(&state, floor + 1, floor + 2, 1000));
    invariants(state);

    // Saturation fails without wrapping counters or modifying the union.
    state = {}; state.busy_ns = UINT64_MAX - 2;
    const auto before = state;
    assert(!nvmtl_gpu_accounting_add(&state, 10, 20, 1));
    assert(!std::memcmp(&before, &state, sizeof state));

    nv_gpu_accounting_counter counter = {};
    nv_gpu_accounting_report report = {NV_GPU_ACCOUNTING_VERSION, 0, 1, 500, 100};
    assert(nv_gpu_accounting_accept(&counter, &report, 100, 1100, 1000));
    const auto accepted = counter;
    auto rejects = [&](nv_gpu_accounting_report bad) {
        assert(!nv_gpu_accounting_accept(&counter, &bad, 100, 1100, 1000));
        assert(!std::memcmp(&counter, &accepted, sizeof counter));
    };
    rejects(report);                                      // replay
    report.sequence = 2;
    auto bad = report; bad.version++; rejects(bad);        // unknown ABI
    bad = report; bad.reserved = 1; rejects(bad);
    bad = report; bad.busy_ns = 499; rejects(bad);          // counter regression
    bad = report; bad.busy_ns = 1001; rejects(bad);         // exceeds connection lifetime
    bad = report; bad.last_submit_absolute = 1101; rejects(bad); // future
    bad = report; bad.last_submit_absolute = 99; rejects(bad);   // predates owner
    report.busy_ns = 1000; report.last_submit_absolute = 1100;
    assert(nv_gpu_accounting_accept(&counter, &report, 100, 1100, 1000));
    report.sequence = 3; report.last_submit_absolute = 1099;
    const auto last = counter;
    assert(!nv_gpu_accounting_accept(&counter, &report, 100, 1200, 1100));
    assert(!std::memcmp(&counter, &last, sizeof counter));
    std::puts("GPU accounting: interval union, reordering, replay, bounds and ABI PASS");
}
