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
#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>
using SInt32 = int32_t;
using UInt32 = uint32_t;
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
struct Call { unsigned enters = 0; void (*function)(void *, void *) = nullptr; void *parameter = nullptr; };
using thread_call_t = Call *;
static thread_call_t nvrmfbCall(void (*function)(void *, void *), void *parameter) {
    return new Call{0, function, parameter};
}
static void thread_call_enter(thread_call_t call) { ++call->enters; }
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
    static void addParameter(OSDictionary *, const OSSymbol *, int, int) {}
    static void setParameter(OSDictionary *dict, const OSSymbol *key, SInt32 value) { dict->values[key] = (UInt32)value; }
};
struct IODisplayParameterHandler : OSObject { virtual bool setDisplay(IODisplay *) { return false; } virtual void free() {} };
using thread_call_t = Call *;
static const OSSymbol parametersKey, brightnessKey, linearKey, valueKey;
static const OSSymbol *gIODisplayParametersKey = &parametersKey;
static const OSSymbol *gIODisplayBrightnessKey = &brightnessKey;
static const OSSymbol *gIODisplayLinearBrightnessKey = &linearKey;
static const OSSymbol *gIODisplayValueKey = &valueKey;
static constexpr SInt32 kNVRMMinBacklight = 5;
static unsigned char setSaved(unsigned char value) {
    auto data = OSData::withBytes(&value, 1);
    IORegistryEntry::options->setProperty("nullmoth-backlight", data); data->release(); return value;
}
static unsigned char savedValue() {
    auto data = OSDynamicCast(OSData, IORegistryEntry::options->copyProperty("nullmoth-backlight"));
    if (!data || data->getLength() != 1) { if (data) data->release(); return 255; }
    auto value = *(const unsigned char *)data->getBytesNoCopy(); data->release(); return value;
}
static void nvrm_backlight_dummy();
struct NVRMBacklightHandler : IODisplayParameterHandler {
    IOService *fRM = nullptr;
    const OSSymbol *fFn = nullptr;
    thread_call_t fApply = nullptr;
    volatile SInt32 fPending = -1;
    SInt32 fLevel = 100;
    volatile SInt32 fUserLevel = 100;
    volatile SInt32 fPersistPending = -1;
    SInt32 fRestoreLevel = -1;
    bool init() { return true; }
    void release() override { delete this; }
    static NVRMBacklightHandler *withRM(IOService *, NvU32);
    bool setDisplay(IODisplay *) override;
    bool doIntegerSet(OSDictionary *, const OSSymbol *, UInt32);
    static void applyFire(thread_call_param_t, thread_call_param_t);
};
'''


checks = r'''static void resetWrites() { gWrites = 0; }
int main() {
    IOService rm;
    IODisplay display;
    setSaved(37); resetWrites();
    auto restored = NVRMBacklightHandler::withRM(&rm, 92);
    assert(restored && restored->fLevel == 92 && restored->fUserLevel == 37 && restored->fRestoreLevel == 37);
    assert(restored->setDisplay(&display));
    assert(display.parameters->values[gIODisplayBrightnessKey] == 37);
    assert(display.parameters->values[gIODisplayLinearBrightnessKey] == 37 && restored->fApply->enters == 1);
    NVRMBacklightHandler::applyFire(restored, nullptr);
    assert(rm.lastPercent == 37 && savedValue() == 37 && gWrites == 0);

    OSDictionary user; user.userValue = true;
    auto zero = NVRMBacklightHandler::withRM(&rm, 100);
    assert(zero->doIntegerSet(&user, gIODisplayBrightnessKey, 0));
    NVRMBacklightHandler::applyFire(zero, nullptr);
    assert(rm.lastPercent == 5 && savedValue() == 0 && zero->fUserLevel == 0 && gWrites == 1);
    auto zeroAfterRestart = NVRMBacklightHandler::withRM(&rm, 85);
    assert(zeroAfterRestart->fUserLevel == 0 && zeroAfterRestart->fRestoreLevel == 0);
    IODisplay zeroDisplay;
    assert(zeroAfterRestart->setDisplay(&zeroDisplay) && zeroDisplay.parameters->values[gIODisplayBrightnessKey] == 0);
    NVRMBacklightHandler::applyFire(zeroAfterRestart, nullptr);
    assert(rm.lastPercent == 5 && savedValue() == 0 && gWrites == 1);

    auto hundred = NVRMBacklightHandler::withRM(&rm, 5);
    assert(hundred->doIntegerSet(&user, gIODisplayLinearBrightnessKey, 100));
    NVRMBacklightHandler::applyFire(hundred, nullptr);
    assert(savedValue() == 100 && hundred->fUserLevel == 100);
    auto hundredAfterRestart = NVRMBacklightHandler::withRM(&rm, 5);
    assert(hundredAfterRestart->fUserLevel == 100 && hundredAfterRestart->fRestoreLevel == 100);
    IODisplay hundredDisplay;
    assert(hundredAfterRestart->setDisplay(&hundredDisplay) && hundredDisplay.parameters->values[gIODisplayBrightnessKey] == 100);
    NVRMBacklightHandler::applyFire(hundredAfterRestart, nullptr);
    assert(rm.lastPercent == 100 && savedValue() == 100 && gWrites == 2);

    unsigned char invalid = 101;
    auto invalidData = OSData::withBytes(&invalid, 1);
    IORegistryEntry::options->setProperty("nullmoth-backlight", invalidData); invalidData->release();
    auto invalidValue = NVRMBacklightHandler::withRM(&rm, 63);
    assert(invalidValue->fUserLevel == 63 && invalidValue->fRestoreLevel == -1);
    unsigned char malformed[] = {45, 0};
    auto malformedData = OSData::withBytes(malformed, sizeof(malformed));
    IORegistryEntry::options->setProperty("nullmoth-backlight", malformedData); malformedData->release();
    auto malformedValue = NVRMBacklightHandler::withRM(&rm, 64);
    assert(malformedValue->fUserLevel == 64 && malformedValue->fRestoreLevel == -1);
    IORegistryEntry::options->values.erase("nullmoth-backlight");
    auto missing = NVRMBacklightHandler::withRM(&rm, 71);
    assert(missing->fUserLevel == 71 && missing->fRestoreLevel == -1);

    setSaved(70); resetWrites();
    auto dimmed = NVRMBacklightHandler::withRM(&rm, 70);
    OSDictionary power;
    assert(dimmed->doIntegerSet(&power, gIODisplayBrightnessKey, 22));
    NVRMBacklightHandler::applyFire(dimmed, nullptr);
    assert(rm.lastPercent == 22 && savedValue() == 70 && gWrites == 0);

    auto saveFailure = NVRMBacklightHandler::withRM(&rm, 70);
    assert(saveFailure->doIntegerSet(&user, gIODisplayBrightnessKey, 46));
    gWritesFail = true;
    NVRMBacklightHandler::applyFire(saveFailure, nullptr);
    assert(rm.lastPercent == 46 && savedValue() == 70 && saveFailure->fPersistPending == 46);
    gWritesFail = false;
    assert(nvrm_backlight_write(IORegistryEntry::options, 46) && savedValue() == 46 && gWrites == 1);

    auto hardwareFailure = NVRMBacklightHandler::withRM(&rm, 46);
    assert(hardwareFailure->doIntegerSet(&user, gIODisplayBrightnessKey, 55));
    rm.result = -1;
    NVRMBacklightHandler::applyFire(hardwareFailure, nullptr);
    assert(savedValue() == 46 && hardwareFailure->fPersistPending == 55);
    puts("PASS: extracted withRM/setDisplay/applyFire restore saved targets, preserve temp dim, enforce 0->5 floor, and retain failed writes for retry.");
}
'''


code = fixture
code += between(HEADER, "static inline bool nvrm_backlight_read", "static inline unsigned nvrm_guard_starts")
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
