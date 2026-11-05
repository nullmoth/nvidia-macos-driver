#include "../kexts/NVRM/accel/nvrm_smc_plan.h"
#include <cassert>
#include <cstdio>

int main() {
    struct Entry { unsigned key, value; } entries[23];
    auto compare = [](unsigned a, unsigned b) { return a < b ? -1 : a > b ? 1 : 0; };
    for (unsigned i = 0; i < 23; ++i) entries[i] = {23 - i, (23 - i) * 7};
    nvrmSMCSort(entries, 23, compare);
    for (unsigned i = 0; i < 23; ++i)
        assert(entries[i].key == i + 1 && entries[i].value == (i + 1) * 7);
    nvrmSMCSort(entries, 23, compare); // already sorted
    nvrmSMCSort(entries, 1, compare);
    nvrmSMCSort<Entry>(nullptr, 0, compare);

    // Seven motherboard channels, including stopped fans, all remain reserved.
    NVRMSMCInventory board {110, 7, 0x7f, 0};
    auto plan = nvrmSMCPlan(board, 3);
    assert(plan.fanBase == 7 && plan.fanCount == 2 && plan.totalFans == 9);
    assert(plan.controller[0] == 0 && plan.controller[1] == 1 && plan.gpuIndex == 0);

    // An unreadable/missing Ac within FNum is not permission to reuse its slot.
    board.occupiedFans = 0x41;
    plan = nvrmSMCPlan(board, 3);
    assert(plan.fanBase == 7 && plan.totalFans == 9);

    // An out-of-range ID/target key still reserves that entire fan slot.
    board.occupiedFans = 0xff;
    plan = nvrmSMCPlan(board, (1u << 2) | (1u << 15));
    assert(plan.fanBase == 8 && plan.totalFans == 10);
    assert(plan.controller[0] == 2 && plan.controller[1] == 15);
    board.occupiedFans |= 1u << 9;
    plan = nvrmSMCPlan(board, 3);
    assert(plan.fanCount == 0 && plan.totalFans == 7);

    // Never wrap a malformed count or create F10Ac (which is not a fourcc).
    board.fanCount = 255;
    plan = nvrmSMCPlan(board, 3);
    assert(plan.fanCount == 0 && plan.totalFans == 255);
    board = {10, 0, 0, 0x3ff};
    plan = nvrmSMCPlan(board, 0);
    assert(plan.fanCount == 0 && plan.gpuIndex == 10);
    board.occupiedGPUs = 0x1b;
    assert(nvrmSMCPlan(board, 3).gpuIndex == 2);

    NVRMSMCInventory same = board;
    assert(nvrmSMCSameInventory(board, same));
    ++same.keyCount; assert(!nvrmSMCSameInventory(board, same));
    same = board; ++same.fanCount; assert(!nvrmSMCSameInventory(board, same));
    same = board; same.occupiedFans |= 4; assert(!nvrmSMCSameInventory(board, same));
    same = board; same.occupiedGPUs |= 4; assert(!nvrmSMCSameInventory(board, same));

    constexpr unsigned long long second = 1000000000ULL;
    unsigned rpm = 999;
    const auto stopped = nvrmSMCPack(0, second);
    assert(stopped && nvrmSMCUnpack(stopped, second, 16383, rpm) && rpm == 0);
    const auto spinning = nvrmSMCPack(1186, second);
    assert(nvrmSMCUnpack(spinning, 6 * second, 16383, rpm) && rpm == 1186);
    assert(!nvrmSMCUnpack(spinning, 6 * second + 1000000, 16383, rpm));
    assert(!nvrmSMCUnpack(spinning, second - 1000000, 16383, rpm));
    assert(!nvrmSMCUnpack(0, second, 16383, rpm));
    assert(!nvrmSMCUnpack(nvrmSMCPack(16384, second), second, 16383, rpm));
    assert(!nvrmSMCPack(65536, second));
    assert(!nvrmSMCPack(100, 0));
    assert(nvrmSMCUnpack(nvrmSMCPack(55, second), second, 125, rpm) && rpm == 55);
    assert(!nvrmSMCUnpack(nvrmSMCPack(126, second), second, 125, rpm));
    std::puts("SMC slot preservation, sparse-controller mapping, capacity, inventory change and freshness checks passed");
}
