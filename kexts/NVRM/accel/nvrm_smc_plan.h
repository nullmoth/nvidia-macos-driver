/* NullMoth native sensor publication. No hardware or kernel dependencies. */
#pragma once

enum { NVRM_SMC_MAX_FANS = 10, NVRM_SMC_MAX_GPUS = 10 };

// The plugin has at most 23 keys. Sort its private array before publication,
// without importing Lilu's qsort implementation or any kernel sort symbol.
template <typename Entry, typename Compare>
static inline void nvrmSMCSort(Entry *entries, unsigned count, Compare compare) {
    for (unsigned i = 1; i < count; ++i) {
        const Entry saved = entries[i];
        unsigned j = i;
        while (j && compare(saved.key, entries[j - 1].key) < 0) {
            entries[j] = entries[j - 1];
            --j;
        }
        entries[j] = saved;
    }
}

struct NVRMSMCInventory {
    unsigned keyCount;
    unsigned fanCount;
    unsigned occupiedFans;
    unsigned occupiedGPUs;
};

struct NVRMSMCPlan {
    unsigned fanBase;
    unsigned fanCount;
    unsigned controller[10];
    unsigned totalFans;
    unsigned gpuIndex;
};

// Never fill a hole inside FNum: an absent or stopped motherboard fan is not
// available for reassignment. Also reserve slots with any F<digit>xx key.
static inline NVRMSMCPlan nvrmSMCPlan(const NVRMSMCInventory &inventory,
                                     unsigned controllerMask) {
    NVRMSMCPlan plan = {};
    plan.gpuIndex = NVRM_SMC_MAX_GPUS;
    plan.fanBase = inventory.fanCount;
    for (unsigned i = 0; i < NVRM_SMC_MAX_FANS; ++i)
        if ((inventory.occupiedFans & (1u << i)) && plan.fanBase <= i)
            plan.fanBase = i + 1;
    unsigned count = 0;
    for (unsigned i = 0; i < 16; ++i)
        if (controllerMask & (1u << i)) ++count;
    if (plan.fanBase <= NVRM_SMC_MAX_FANS && count <= NVRM_SMC_MAX_FANS - plan.fanBase) {
        for (unsigned i = 0; i < 16; ++i)
            if (controllerMask & (1u << i)) plan.controller[plan.fanCount++] = i;
    }
    plan.totalFans = plan.fanCount ? plan.fanBase + plan.fanCount : inventory.fanCount;
    for (unsigned i = 0; i < NVRM_SMC_MAX_GPUS; ++i)
        if (!(inventory.occupiedGPUs & (1u << i))) { plan.gpuIndex = i; break; }
    return plan;
}

static inline bool nvrmSMCSameInventory(const NVRMSMCInventory &a, const NVRMSMCInventory &b) {
    return a.keyCount == b.keyCount && a.fanCount == b.fanCount &&
        a.occupiedFans == b.occupiedFans && a.occupiedGPUs == b.occupiedGPUs;
}

// One naturally aligned atomic word contains both value and source arrival.
// Millisecond timestamps leave 8,900 years before the packed field overflows.
static inline unsigned long long nvrmSMCPack(unsigned value, unsigned long long arrivalNs) {
    const unsigned long long ms = arrivalNs / 1000000ULL;
    if (!ms || ms >= (1ULL << 48) || value > 0xffff) return 0;
    return (ms << 16) | value;
}

static inline bool nvrmSMCUnpack(unsigned long long packed, unsigned long long nowNs,
                                unsigned maximum, unsigned &value) {
    const unsigned long long arrival = packed >> 16, now = nowNs / 1000000ULL;
    value = static_cast<unsigned>(packed & 0xffff);
    return arrival && now >= arrival && now - arrival <= 5000 && value <= maximum;
}
