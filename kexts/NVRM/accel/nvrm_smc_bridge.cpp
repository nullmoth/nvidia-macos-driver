/*
 * Optional SMC publication inside NVAccel. Collection remains in NVRM.
 * VirtualSMC's public plugin ABI is used without a load-time dependency.
 */
#include "nvrm_smc_bridge.h"
#include "nvrm_smc_plan.h"
#include "smc/nvrm_vsmc_sdk.hpp"
#include <IOKit/IOUserClient.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSDictionary.h>
#include <mach/task.h>
#include <pexpert/pexpert.h>

extern "C" uint64_t mach_continuous_time(void);
extern "C" void absolutetime_to_nanoseconds(uint64_t, uint64_t *);
static uint64_t nowNs() {
    uint64_t ns = 0;
    absolutetime_to_nanoseconds(mach_continuous_time(), &ns);
    return ns;
}

static unsigned gSubmissionAttempted;
static IOLock *gRegistrationLock;

struct NVRMSMCBridge {
    IOService *owner;
    alignas(8) uint64_t fan[16] = {};
    alignas(8) uint64_t temperature = 0;
    unsigned active = 1;
    unsigned ticks = 0, stableMask = 0, stableTicks = 0;
    bool submitted = false, attempted = false;
    NVRMSMCPlan plan = {};
    NVRMVirtualSMCAPI::Plugin plugin {"NullMothNativeSensors", 1, NVRMVirtualSMCAPI::Version, {}, {}};
};

static_assert(NVRMVirtualSMCAPI::Version == 1, "VirtualSMC ABI changed");
static_assert(sizeof(NVRMVirtualSMCValue) == 144, "VirtualSMC value ABI changed");
static_assert(sizeof(NVRMVirtualSMCKeyValue) == 24, "VirtualSMC key ABI changed");
static_assert(sizeof(NVRMVirtualSMCAPI::Plugin) == 72, "VirtualSMC plugin ABI changed");

class SensorValue final : public NVRMVirtualSMCValue {
    NVRMSMCBridge *bridge;
    unsigned controller;
    bool thermal;
public:
    SensorValue(NVRMSMCBridge *b, unsigned index, bool temp) : bridge(b), controller(index), thermal(temp) {
        size = 2;
        type = temp ? SmcKeyTypeSp78 : SmcKeyTypeFpe2;
        attr = SMC_KEY_ATTRIBUTE_READ;
        serializeLevel = NVRMSerializeLevel::None;
    }
    SMC_RESULT update(const SMC_DATA *) override { return SmcNotWritable; }
protected:
    SMC_RESULT writeAccess() override { return SmcNotWritable; }
    SMC_RESULT readAccess() override {
        // VirtualSMC calls this with a spinlock held: no allocations, RM calls,
        // IORegistry access, or sleeping locks are permitted here.
        if (!__atomic_load_n(&bridge->active, __ATOMIC_ACQUIRE)) return SmcNotReadable;
        uint64_t *source = thermal ? &bridge->temperature : &bridge->fan[controller];
        const uint64_t packed = __atomic_load_n(source, __ATOMIC_ACQUIRE);
        unsigned value = 0;
        if (!nvrmSMCUnpack(packed, nowNs(), thermal ? 125 : 16383, value) || (thermal && !value))
            return SmcNotReadable;
        unsigned encoded = value << (thermal ? 8 : 2);
        data[0] = static_cast<SMC_DATA>(encoded >> 8);
        data[1] = static_cast<SMC_DATA>(encoded);
        return SmcSuccess;
    }
};

class ConstantValue final : public NVRMVirtualSMCValue {
public:
    ConstantValue(const uint8_t *bytes, unsigned length, SMC_KEY_TYPE valueType) {
        size = static_cast<SMC_DATA_SIZE>(length);
        type = valueType;
        attr = SMC_KEY_ATTRIBUTE_READ;
        serializeLevel = NVRMSerializeLevel::None;
        for (unsigned i = 0; i < length; ++i) data[i] = bytes[i];
    }
    SMC_RESULT update(const SMC_DATA *) override { return SmcNotWritable; }
protected:
    SMC_RESULT writeAccess() override { return SmcNotWritable; }
};

// AppleSMC's established read-only client protocol. An unsupported connection,
// malformed reply or unreadable inventory disables registration, never implies
// an empty key. There is no SMC write command in this implementation.
struct SMCRequest {
    uint32_t key;
    uint8_t version[8], limits[16];
    uint32_t size, type;
    uint8_t attributes[4], result, status, command, reserved;
    uint32_t index;
    uint8_t bytes[32];
};
static_assert(sizeof(SMCRequest) == 80, "AppleSMC client layout");
static_assert(__builtin_offsetof(SMCRequest, bytes) == 48, "AppleSMC payload offset");
static constexpr uint32_t key(char a, char b, char c, char d) {
    return (uint32_t(uint8_t(a)) << 24) | (uint32_t(uint8_t(b)) << 16) |
        (uint32_t(uint8_t(c)) << 8) | uint8_t(d);
}

class SMCReader {
    IOUserClient *client = nullptr;
    bool call(const SMCRequest &in, SMCRequest &out) {
        IOExternalMethodArguments args = {};
        args.version = kIOExternalMethodArgumentsCurrentVersion;
        args.selector = 2;
        args.structureInput = &in; args.structureInputSize = sizeof in;
        args.structureOutput = &out; args.structureOutputSize = sizeof out;
        return client && client->externalMethod(2, &args, nullptr, nullptr, nullptr) == kIOReturnSuccess &&
            args.structureOutputSize == sizeof out;
    }
public:
    ~SMCReader() { if (client) { client->clientClose(); client->release(); } }
    bool open(IOService *service) {
        return service && service->newUserClient(kernel_task, nullptr, 0, nullptr, &client) == kIOReturnSuccess && client;
    }
    // -1 means a protocol/permission error, not an unused key.
    int info(uint32_t name, SMCRequest &out) {
        SMCRequest in = {}; in.key = name; in.command = 9; out = {};
        if (!call(in, out)) return -1;
        if (out.result == SmcNotFound) return 0;
        if (out.result != SmcSuccess || !out.size || out.size > 32 || !out.type) return -1;
        return 1;
    }
    bool read(uint32_t name, uint32_t type, unsigned size, uint8_t *bytes) {
        SMCRequest metadata = {};
        if (info(name, metadata) != 1 || metadata.size != size || metadata.type != type) return false;
        SMCRequest in = {}, out = {}; in.key = name; in.size = size; in.command = 5;
        if (!call(in, out) || out.result != SmcSuccess) return false;
        for (unsigned i = 0; i < size; ++i) bytes[i] = out.bytes[i];
        return true;
    }
    bool inventory(NVRMSMCInventory &result) {
        result = {};
        uint8_t count[4] = {}, fans = 0;
        if (!read(key('#','K','E','Y'), key('u','i','3','2'), 4, count) ||
            !read(key('F','N','u','m'), key('u','i','8',' '), 1, &fans)) return false;
        result.keyCount = uint32_t(count[0]) << 24 | uint32_t(count[1]) << 16 | uint32_t(count[2]) << 8 | count[3];
        result.fanCount = fans;
        if (!result.keyCount || result.keyCount > 16384) return false;
        for (unsigned i = 0; i < result.keyCount; ++i) {
            SMCRequest in = {}, out = {}; in.command = 8; in.index = i;
            if (!call(in, out) || out.result != SmcSuccess || !out.key) return false;
            const unsigned a = out.key >> 24, b = (out.key >> 16) & 255, c = (out.key >> 8) & 255;
            if (a == 'F' && b >= '0' && b <= '9') result.occupiedFans |= 1u << (b - '0');
            if (a == 'T' && b == 'G' && c >= '0' && c <= '9') result.occupiedGPUs |= 1u << (c - '0');
        }
        return true;
    }
};

static IOService *findService(const char *className) {
    OSDictionary *matching = IOService::serviceMatching(className);
    if (!matching) return nullptr;
    OSIterator *iterator = IOService::getMatchingServices(matching);
    matching->release();
    IOService *found = nullptr;
    if (iterator) {
        while (OSObject *object = iterator->getNextObject()) {
            IOService *service = OSDynamicCast(IOService, object);
            if (service && !service->isInactive()) { service->retain(); found = service; break; }
        }
        iterator->release();
    }
    return found;
}

static void status(NVRMSMCBridge *b, const char *message) { b->owner->setProperty("NVRMSMCStatus", message); }

static bool addValue(NVRMSMCBridge *b, SMC_KEY name, NVRMVirtualSMCValue *value) {
    if (!value) return false;
    auto item = NVRMVirtualSMCKeyValue::create(name, value);
    if (b->plugin.data.push_back(item)) return true;
    delete value;
    return false;
}

static bool targetsAbsent(SMCReader &reader, const NVRMSMCPlan &plan) {
    SMCRequest metadata = {};
    for (unsigned i = 0; i < plan.fanCount; ++i) {
        const char index = static_cast<char>('0' + plan.fanBase + i);
        if (reader.info(key('F', index, 'A', 'c'), metadata) != 0 ||
            reader.info(key('F', index, 'I', 'D'), metadata) != 0) return false;
    }
    if (plan.gpuIndex < 10) {
        const char index = static_cast<char>('0' + plan.gpuIndex);
        if (reader.info(key('T','G',index,'D'), metadata) != 0 ||
            reader.info(key('T','G',index,'P'), metadata) != 0) return false;
    }
    return true;
}

static void registerSensors(NVRMSMCBridge *b, unsigned fanMask) {
    IOService *vsmc = findService("VirtualSMC");
    IOService *apple = findService("AppleSMC");
    if (!vsmc || !apple) {
        status(b, "waiting for VirtualSMC and AppleSMC");
        if (vsmc) vsmc->release(); if (apple) apple->release();
        return;
    }
    SMCReader reader;
    NVRMSMCInventory before = {}, confirmation = {};
    if (!reader.open(apple) || !reader.inventory(before)) {
        status(b, "SMC inventory unavailable; no keys submitted");
        vsmc->release(); apple->release(); return;
    }
    b->plan = nvrmSMCPlan(before, fanMask);
    if ((!b->plan.fanCount && b->plan.gpuIndex == 10) || !targetsAbsent(reader, b->plan)) {
        status(b, "no unused SMC slots; no keys submitted");
        vsmc->release(); apple->release(); return;
    }
    bool ok = true;
    for (unsigned i = 0; ok && i < b->plan.fanCount; ++i) {
        const char slot = static_cast<char>('0' + b->plan.fanBase + i);
        ok = addValue(b, SMC_MAKE_IDENTIFIER('F',slot,'A','c'), new SensorValue(b, b->plan.controller[i], false));
        uint8_t name[16] = {}; // {fds: four descriptor bytes, then 12 label bytes.
        char label[24];
        snprintf(label, sizeof label, "GPU %u Fan %u", b->plan.gpuIndex < 10 ? b->plan.gpuIndex : 0, b->plan.controller[i] + 1);
        for (unsigned n = 0; n < 12; ++n) name[4+n] = n < strlen(label) ? label[n] : ' ';
        if (ok) ok = addValue(b, SMC_MAKE_IDENTIFIER('F',slot,'I','D'),
            new ConstantValue(name, sizeof name, SMC_MAKE_IDENTIFIER('{','f','d','s')));
    }
    if (ok && b->plan.fanCount) {
        uint8_t count = static_cast<uint8_t>(b->plan.totalFans);
        ok = addValue(b, SMC_MAKE_IDENTIFIER('F','N','u','m'), new ConstantValue(&count, 1, SmcKeyTypeUint8));
    }
    if (ok && b->plan.gpuIndex < 10) {
        const char index = static_cast<char>('0' + b->plan.gpuIndex);
        ok = addValue(b, SMC_MAKE_IDENTIFIER('T','G',index,'D'), new SensorValue(b, 0, true)) &&
            addValue(b, SMC_MAKE_IDENTIFIER('T','G',index,'P'), new SensorValue(b, 0, true));
    }
    if (!ok || !reader.inventory(confirmation) || !nvrmSMCSameInventory(before, confirmation) ||
        !targetsAbsent(reader, b->plan)) {
        b->plugin.data.deinit();
        status(b, "SMC inventory changed or allocation failed; no keys submitted");
        vsmc->release(); apple->release(); return;
    }
    nvrmSMCSort(b->plugin.data.data(), b->plugin.data.size(), [](SMC_KEY a, SMC_KEY c) {
        return NVRMVirtualSMCKeyValue::compare(a, c);
    });
    // The SDK retains pointers even after partial submission failure. From this
    // point the descriptor, values and containing code must live until reboot.
    b->attempted = true;
    __atomic_store_n(&gSubmissionAttempted, 1u, __ATOMIC_RELEASE);
    const OSSymbol *function = OSSymbol::withCStringNoCopy(NVRMVirtualSMCAPI::SubmitPlugin);
    const IOReturn result = function ? vsmc->callPlatformFunction(function, false, b->owner, &b->plugin, nullptr, nullptr) : kIOReturnNoMemory;
    if (function) function->release();
    b->submitted = result == kIOReturnSuccess;
    b->owner->setProperty("NVRMSMCSubmitResult", static_cast<unsigned long long>(result), 32);
    b->owner->setProperty("NVRMSMCOriginalFanCount", static_cast<unsigned long long>(before.fanCount), 32);
    if (b->submitted) {
        OSDictionary *map = OSDictionary::withCapacity(b->plan.fanCount);
        for (unsigned i = 0; map && i < b->plan.fanCount; ++i) {
            char slot[4]; snprintf(slot, sizeof slot, "%u", b->plan.fanBase + i);
            OSNumber *controller = OSNumber::withNumber(b->plan.controller[i], 32);
            if (controller) { map->setObject(slot, controller); controller->release(); }
        }
        if (map) { b->owner->setProperty("NVRMSMCFanMap", map); map->release(); }
        if (b->plan.gpuIndex < 10)
            b->owner->setProperty("NVRMSMCGPUIndex", static_cast<unsigned long long>(b->plan.gpuIndex), 32);
        status(b, b->plan.fanCount ? "SMC sensors published" : "SMC temperature published; no free fan slots");
        IOLog("NVAccel: SMC published %u GPU fans after %u existing slots, temperature index %u\n",
              b->plan.fanCount, b->plan.fanBase, b->plan.gpuIndex);
    } else {
        __atomic_store_n(&b->active, 0u, __ATOMIC_RELEASE);
        status(b, "VirtualSMC rejected plugin; sensors disabled");
    }
    vsmc->release(); apple->release();
}

NVRMSMCBridge *nvrmSMCCreate(IOService *owner) {
    int disabled = 0;
    if (!owner || (PE_parse_boot_argn("nvsmcoff", &disabled, sizeof disabled) && disabled)) return nullptr;
    if (!__atomic_load_n(&gRegistrationLock, __ATOMIC_ACQUIRE)) {
        IOLock *lock = IOLockAlloc();
        if (!lock) return nullptr;
        IOLock *expected = nullptr;
        if (!__atomic_compare_exchange_n(&gRegistrationLock, &expected, lock, false, __ATOMIC_RELEASE, __ATOMIC_RELAXED))
            IOLockFree(lock);
    }
    NVRMSMCBridge *b = new NVRMSMCBridge;
    if (b) { b->owner = owner; status(b, "waiting for native samples"); }
    return b;
}

void nvrmSMCTemperature(NVRMSMCBridge *b, unsigned celsius) {
    if (b) __atomic_store_n(&b->temperature, celsius >= 1 && celsius <= 125 ? nvrmSMCPack(celsius, nowNs()) : 0, __ATOMIC_RELEASE);
}

void nvrmSMCTick(NVRMSMCBridge *b, const NVRMTelemetry &sample, unsigned long long arrivalNs) {
    if (!b || !__atomic_load_n(&b->active, __ATOMIC_ACQUIRE)) return;
    const unsigned mask = sample.validMask & NVRM_TELEMETRY_VALID_FANS ? sample.fanValidMask & 0xffffu : 0;
    for (unsigned i = 0; i < 16; ++i) {
        uint64_t packed = (mask & (1u << i)) && sample.fanRPM[i] <= 16383 ? nvrmSMCPack(sample.fanRPM[i], arrivalNs) : 0;
        __atomic_store_n(&b->fan[i], packed, __ATOMIC_RELEASE);
    }
    ++b->ticks;
    b->stableTicks = mask && mask == b->stableMask ? b->stableTicks + 1 : 0;
    b->stableMask = mask;
    // Give boot-time fan providers and NVIDIA controller discovery time to settle.
    if (b->attempted || b->ticks < 10 || (b->stableTicks < 3 && b->ticks < 60) || b->ticks % 10) return;
    IOLock *lock = __atomic_load_n(&gRegistrationLock, __ATOMIC_ACQUIRE);
    IOLockLock(lock);
    registerSensors(b, mask);
    IOLockUnlock(lock);
}

void nvrmSMCStop(NVRMSMCBridge *b) {
    if (!b) return;
    __atomic_store_n(&b->active, 0u, __ATOMIC_RELEASE);
    if (!b->attempted) { b->plugin.data.deinit(); b->plugin.dataHidden.deinit(); delete b; }
}

bool nvrmSMCCanUnload() {
    if (__atomic_load_n(&gSubmissionAttempted, __ATOMIC_ACQUIRE)) return false;
    // Called only by the module stop entry, after its services have stopped.
    IOLock *lock = __atomic_exchange_n(&gRegistrationLock, nullptr, __ATOMIC_ACQ_REL);
    if (lock) IOLockFree(lock);
    return true;
}
