#!/usr/bin/env python3
"""Compile extracted backlight persistence paths against a small I/O Registry fixture."""
from pathlib import Path
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
HEADER = (ROOT / "kexts/NVRMFB/nvrm_off.h").read_text(encoding="utf-8")
SOURCE = (ROOT / "kexts/NVRMFB/fb/nvrm-fb.cpp").read_text(encoding="utf-8")


def between(source, start, end):
    return source[source.index(start):source.index(end, source.index(start))]


fixture = r'''#include <cassert>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>
using SInt32 = int32_t;
using UInt32 = uint32_t;
using UInt64 = uint64_t;
using NvU32 = uint32_t;
using IOReturn = int;
using thread_call_param_t = void *;
static constexpr IOReturn kIOReturnSuccess = 0;
static int gIODTPlane;
static bool gOptionsAvailable = true, gWritesFail = false;
static unsigned gWrites;
#define NVRM_BACKLIGHT_VAR "nullmoth-backlight"
struct OSObject {
    unsigned refs = 1;
    virtual ~OSObject() = default;
    virtual void retain() { ++refs; }
    virtual void release() { if (!--refs) delete this; }
};
struct OSData : OSObject {
    std::vector<unsigned char> bytes;
    static OSData *withBytes(const void *data, unsigned length) {
        auto result = new OSData;
        result->bytes.assign((const unsigned char *)data, (const unsigned char *)data + length);
        return result;
    }
    unsigned getLength() const { return (unsigned)bytes.size(); }
    const void *getBytesNoCopy() const { return bytes.data(); }
};
template <class T> T *castObject(OSObject *object) { return dynamic_cast<T *>(object); }
#define OSDynamicCast(type, object) castObject<type>(object)
struct OSSymbol : OSObject {
    static OSSymbol *withCStringNoCopy(const char *) { return new OSSymbol; }
};
struct IORegistryEntry : OSObject {
    std::map<std::string, std::vector<unsigned char>> values;
    static IORegistryEntry *options;
    static IORegistryEntry *fromPath(const char *, int) { return gOptionsAvailable ? options : nullptr; }
    void release() override {}
    OSObject *copyProperty(const char *key) {
        auto it = values.find(key);
        return it == values.end() ? nullptr : OSData::withBytes(it->second.data(), (unsigned)it->second.size());
    }
    bool setProperty(const char *key, OSData *data) {
        if (gWritesFail) return false;
        values[key] = data->bytes; ++gWrites; return true;
    }
};
IORegistryEntry *IORegistryEntry::options = new IORegistryEntry;
static uint64_t gNow;
static constexpr UInt32 kSecondScale = 1, kNVRMBacklightSaveDelaySeconds = 2;
static constexpr int THREAD_CALL_PRIORITY_LOW = 0, THREAD_CALL_OPTIONS_ONCE = 1;
static uint64_t mach_absolute_time() { return gNow; }
static void clock_interval_to_deadline(UInt32 seconds, UInt32, uint64_t *deadline) { *deadline = gNow + seconds; }
struct Call { unsigned enters = 0; void (*function)(void *, void *) = nullptr; void *parameter = nullptr; uint64_t deadline = 0; };
using thread_call_t = Call *;
static thread_call_t nvrmfbCall(void (*function)(void *, void *), void *parameter) {
    return new Call{0, function, parameter};
}
static void thread_call_enter(thread_call_t call) { ++call->enters; }
static thread_call_t thread_call_allocate_with_options(void (*function)(void *, void *), void *parameter, int, int) {
    return nvrmfbCall(function, parameter);
}
static void thread_call_enter_delayed(thread_call_t call, uint64_t deadline) { ++call->enters; call->deadline = deadline; }
static SInt32 OSAddAtomic(SInt32 delta, volatile SInt32 *value) {
    return __atomic_add_fetch(value, delta, __ATOMIC_SEQ_CST) - delta;
}
static bool OSCompareAndSwap(UInt32 oldValue, UInt32 newValue, volatile UInt32 *value) {
    return __atomic_compare_exchange_n(value, &oldValue, newValue, false, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}
#define OSSafeReleaseNULL(value) do { if (value) { (value)->release(); (value) = nullptr; } } while (0)
#define OSTypeAlloc(type) new type
#define OSDeclareDefaultStructors(type)
#define OSDefineMetaClassAndStructors(type, base)
#define BLLOG(...) do {} while (0)
struct IOService : IORegistryEntry {
    IOReturn result = kIOReturnSuccess;
    NvU32 lastPercent = 0;
    void retain() { ++refs; }
    IOReturn callPlatformFunction(const OSSymbol *, bool, void *arg0, void *, void *, void *) {
        lastPercent = *(NvU32 *)arg0; return result;
    }
};
struct OSDictionary : OSObject {
    bool userValue = false;
    std::map<const OSSymbol *, std::pair<int, int>> ranges;
    std::map<const OSSymbol *, UInt32> values;
    static OSDictionary *withCapacity(unsigned) { return new OSDictionary; }
    static OSDictionary *withDictionary(OSDictionary *other) {
        auto result = new OSDictionary; result->values = other->values; return result;
    }
    OSObject *getObject(const OSSymbol *) { return userValue ? (OSObject *)this : nullptr; }
};
struct IODisplay : IOService {
    OSDictionary *parameters = nullptr;
    ~IODisplay() override { if (parameters) parameters->release(); }
    OSObject *copyProperty(const OSSymbol *) { return nullptr; }
    bool setProperty(const OSSymbol *, OSDictionary *value) { value->retain(); parameters = value; return true; }
    const char *getName() { return "display"; }
    static void addParameter(OSDictionary *dict, const OSSymbol *key, int min, int max) { dict->ranges[key] = {min, max}; }
    static bool setParameter(OSDictionary *dict, const OSSymbol *key, SInt32 value) { dict->values[key] = (UInt32)value; return true; }
};
struct IODisplayParameterHandler : OSObject { virtual bool setDisplay(IODisplay *) { return false; } virtual void free() {} };
using thread_call_t = Call *;
static const OSSymbol parametersKey, brightnessKey, linearKey, valueKey;
static const OSSymbol *gIODisplayParametersKey = &parametersKey;
static const OSSymbol *gIODisplayBrightnessKey = &brightnessKey;
static const OSSymbol *gIODisplayLinearBrightnessKey = &linearKey;
static const OSSymbol *gIODisplayValueKey = &valueKey;
static constexpr SInt32 kAppleBacklightDefaultMax = 255;
static unsigned char setSaved(unsigned char value) {
    auto data = OSData::withBytes(&value, 1);
    IORegistryEntry::options->setProperty("nullmoth-backlight", data); data->release(); return value;
}
static UInt32 savedValue();
static void nvrm_backlight_dummy();
struct NVRMBacklightHandler : IODisplayParameterHandler {
    IOService *fRM = nullptr;
    const OSSymbol *fFn = nullptr;
    thread_call_t fApply = nullptr;
    thread_call_t fPersist = nullptr;
    volatile SInt32 fPending = -1;
    SInt32 fLevel = 100;
    volatile SInt32 fUserLevel = 100;
    volatile SInt32 fPersistPending = -1;
    volatile UInt64 fPersistDeadline = 0;
    SInt32 fRestoreLevel = -1;
    OSDictionary *fParameters = nullptr;
    static const OSSymbol *sBrightnessProbe, *sLinearBrightnessProbe;
    bool init() { return true; }
    void release() override { delete this; }
    static NVRMBacklightHandler *withRM(IOService *, NvU32);
    bool setDisplay(IODisplay *) override;
    bool doIntegerSet(OSDictionary *, const OSSymbol *, UInt32);
    static void applyFire(thread_call_param_t, thread_call_param_t);
    static void persistFire(thread_call_param_t, thread_call_param_t);
};
const OSSymbol *NVRMBacklightHandler::sBrightnessProbe = nullptr;
const OSSymbol *NVRMBacklightHandler::sLinearBrightnessProbe = nullptr;
'''


checks = r'''
static UInt32 savedValue() {
    UInt32 value;
    assert(nvrm_backlight_read(IORegistryEntry::options, &value));
    return value;
}
static void fireSave(NVRMBacklightHandler *handler) {
    auto call = handler->fPersist;
    assert(call->enters > 0);
    gNow = call->deadline;
    call->function(call->parameter, nullptr);
}
int main() {
    IOService rm;
    OSDictionary userForProbe;
    IODisplay display;
    setSaved(37); gWrites = 0;
    auto restored = NVRMBacklightHandler::withRM(&rm, 92);
    assert(restored->fLevel == (SInt32)nvrm_backlight_from_percent(92));
    assert(restored->fUserLevel == (SInt32)nvrm_backlight_from_percent(37));
    assert(restored->setDisplay(&display));
    for (auto key : {gIODisplayBrightnessKey, gIODisplayLinearBrightnessKey,
                    NVRMBacklightHandler::sBrightnessProbe, NVRMBacklightHandler::sLinearBrightnessProbe}) {
        auto range = display.parameters->ranges[key];
        assert(range.first == 0 && range.second == 65535);
    }
    // IODisplay's documented float normalization must not round-trip through whole percent.
    for (unsigned i = 0; i <= 97; ++i) {
        float normalized = i / 97.0f;
        UInt32 logical = (UInt32)roundf(normalized * 65535.0f);
        assert(restored->doIntegerSet(&userForProbe, NVRMBacklightHandler::sBrightnessProbe, logical));
        float result = display.parameters->values[NVRMBacklightHandler::sLinearBrightnessProbe] / 65535.0f;
        assert(fabsf(result - normalized) <= 0.5f / 65535.0f + 1e-7f);
    }
    assert(display.parameters->values[gIODisplayBrightnessKey] == nvrm_backlight_from_percent(37));
    assert(display.parameters->values[gIODisplayLinearBrightnessKey] == nvrm_backlight_from_percent(37));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(rm.lastPercent == 37 && gWrites == 0);

    OSDictionary user; user.userValue = true;
    OSDictionary power;
    // The target is intentionally between whole hardware-percent levels.
    constexpr UInt32 target = 20777;
    assert(restored->doIntegerSet(&user, gIODisplayLinearBrightnessKey, target));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(rm.lastPercent == 35 && savedValue() == nvrm_backlight_from_percent(37) && gWrites == 0);
    NVRMBacklightHandler::persistFire(restored, nullptr);
    assert(gWrites == 0);
    fireSave(restored);
    assert(rm.lastPercent == 35 && savedValue() == target && gWrites == 1);
    const auto &bytes = IORegistryEntry::options->values[NVRM_BACKLIGHT_VAR];
    assert(bytes == std::vector<unsigned char>({2, 0x29, 0x51}));
    assert(display.parameters->values[gIODisplayBrightnessKey] == target);
    assert(restored->fLevel == (SInt32)target && restored->fUserLevel == (SInt32)target);
    auto restarted = NVRMBacklightHandler::withRM(&rm, 100);
    IODisplay restartDisplay;
    assert(restarted->setDisplay(&restartDisplay));
    assert(restartDisplay.parameters->values[gIODisplayLinearBrightnessKey] == target);
    NVRMBacklightHandler::applyFire(restarted, nullptr);
    assert(rm.lastPercent == 35 && savedValue() == target && gWrites == 1);

    assert(restored->doIntegerSet(&power, gIODisplayBrightnessKey, nvrm_backlight_from_percent(12)));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(rm.lastPercent == 12 && savedValue() == target && gWrites == 1);
    assert(display.parameters->values[gIODisplayBrightnessKey] == target);
    IODisplay reboundDisplay;
    assert(restored->setDisplay(&reboundDisplay));
    assert(reboundDisplay.parameters->values[gIODisplayBrightnessKey] == target);
    assert(reboundDisplay.parameters->values[gIODisplayLinearBrightnessKey] == target);
    assert(restored->fLevel == (SInt32)nvrm_backlight_from_percent(12));
    unsigned enters = restored->fApply->enters;
    assert(restored->doIntegerSet(&user, NVRMBacklightHandler::sLinearBrightnessProbe, 4096));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(reboundDisplay.parameters->values[NVRMBacklightHandler::sBrightnessProbe] == 4096);
    assert(restored->fApply->enters == enters && rm.lastPercent == 12 && savedValue() == target);
    assert(restored->doIntegerSet(&user, NVRMBacklightHandler::sBrightnessProbe, 32768));
    assert(reboundDisplay.parameters->values[NVRMBacklightHandler::sLinearBrightnessProbe] == 32768);
    assert(restored->fUserLevel == (SInt32)target && gWrites == 1);

    rm.result = -1;
    assert(restored->doIntegerSet(&user, gIODisplayBrightnessKey, 30001));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(savedValue() == target && restored->fPersistPending == -1 && gWrites == 1);
    rm.result = kIOReturnSuccess;
    gWritesFail = true;
    assert(restored->doIntegerSet(&user, gIODisplayBrightnessKey, 30001));
    NVRMBacklightHandler::applyFire(restored, nullptr);
    fireSave(restored);
    assert(savedValue() == target && restored->fPersistPending == 30001 && gWrites == 1);
    gWritesFail = false;
    fireSave(restored);
    assert(savedValue() == 30001 && restored->fPersistPending == -1 && gWrites == 2);

    for (UInt32 level : {0U, 1U, 32768U, 65534U, 65535U}) {
        assert(restored->doIntegerSet(&user, gIODisplayBrightnessKey, level));
        NVRMBacklightHandler::applyFire(restored, nullptr);
        fireSave(restored);
        assert(savedValue() == level && restored->fLevel == (SInt32)level);
        auto next = NVRMBacklightHandler::withRM(&rm, 80);
        assert(next->fUserLevel == (SInt32)level && next->fRestoreLevel == (SInt32)level);
        if (level <= 1) assert(rm.lastPercent == 5);
        if (level == 32768) assert(rm.lastPercent == 53);
        if (level >= 65534) assert(rm.lastPercent == 100);
    }
    unsigned writes = gWrites;
    assert(nvrm_backlight_write(IORegistryEntry::options, 65535) && gWrites == writes);
    assert(!nvrm_backlight_write(IORegistryEntry::options, 65536) && gWrites == writes);
    for (unsigned percent = 0; percent <= 100; ++percent) {
        setSaved((unsigned char)percent);
        assert(savedValue() == nvrm_backlight_from_percent(percent));
        assert(nvrm_backlight_to_percent(savedValue()) == (percent < 5 ? 5 : percent));
    }
    for (UInt32 legacy : {0U, 1U, 3277U, 20777U, 32768U, 65535U}) {
        IORegistryEntry::options->values[NVRM_BACKLIGHT_VAR] = {1, (unsigned char)legacy, (unsigned char)(legacy >> 8)};
        UInt32 percent = (legacy * 100 + 32767) / 65535;
        assert(nvrm_backlight_to_percent(savedValue()) == (percent < 5 ? 5 : percent));
    }
    assert(nvrm_backlight_to_percent(0) == 5);
    assert(nvrm_backlight_to_percent(3277) == 10);
    assert(nvrm_backlight_to_percent(65535) == 100);
    for (const auto &malformed : std::vector<std::vector<unsigned char>>{{}, {101}, {45, 0}, {3, 1, 0}, {1, 1, 0, 0}}) {
        IORegistryEntry::options->values[NVRM_BACKLIGHT_VAR] = malformed;
        auto invalid = NVRMBacklightHandler::withRM(&rm, 63);
        assert(invalid->fUserLevel == (SInt32)nvrm_backlight_from_percent(63) && invalid->fRestoreLevel == -1);
    }
    IORegistryEntry::options->values.erase(NVRM_BACKLIGHT_VAR);
    auto missing = NVRMBacklightHandler::withRM(&rm, 71);
    assert(missing->fUserLevel == (SInt32)nvrm_backlight_from_percent(71) && missing->fRestoreLevel == -1);
    puts("PASS: fractional targets survive probes, hardware-percent quantization, power requests and restart; legacy storage, endpoints and failed writes are covered.");
}
'''


code = fixture
code += between(HEADER, "enum { kNVRMBacklightMax", "static inline unsigned nvrm_guard_starts")
code += between(SOURCE, "NVRMBacklightHandler *NVRMBacklightHandler::withRM", "void NVRMBacklightHandler::free()")
code += between(SOURCE, "bool NVRMBacklightHandler::setDisplay", "// User changes arrive")
code += between(SOURCE, "bool NVRMBacklightHandler::doIntegerSet", "void NVRMBacklightHandler::applyFire")
code += between(SOURCE, "void NVRMBacklightHandler::applyFire", "// 1 once this boot's guard clear")
code += checks

with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    test = path / "check.cpp"
    test.write_text(code, encoding="utf-8")
    subprocess.run(["clang++", "-std=c++17", "-Wall", "-Wextra", "-Wno-unused-function", str(test), "-o", str(path / "check")], check=True)
    subprocess.run([str(path / "check")], check=True)
