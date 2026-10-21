from pathlib import Path
import os
import shlex
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "kexts" / "NMHDA" / "nmhda.cpp"


def between(source, start, end):
    return source[source.index(start):source.index(end, source.index(start))]


HARNESS = r'''#include <cassert>
#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>
#include <cstdio>
using SInt32 = int32_t;
using mach_vm_address_t = uintptr_t;
#define OSDynamicCast(type, value) dynamic_cast<type *>(value)
#define FunctionCast(function, pointer) reinterpret_cast<decltype(&function)>(pointer)
#define arrsize(a) (sizeof(a) / sizeof((a)[0]))
struct OSObject {
    int refs = 1;
    virtual ~OSObject() = default;
    void retain() { ++refs; }
    void release() { if (!--refs) delete this; }
};
struct OSData : OSObject {
    uint32_t value;
    explicit OSData(uint32_t v) : value(v) {}
    unsigned getLength() { return 4; }
    const void *getBytesNoCopy() { return &value; }
};
struct OSNumber : OSObject {
    static bool fail;
    uint32_t value;
    explicit OSNumber(uint32_t v) : value(v) {}
    static OSNumber *withNumber(uint32_t v, unsigned) { return fail ? nullptr : new OSNumber(v); }
    uint32_t unsigned32BitValue() { return value; }
};
bool OSNumber::fail = false;
struct OSDictionary : OSObject {
    std::map<std::string, OSObject *> values;
    static OSDictionary *withCapacity(unsigned) { return new OSDictionary; }
    bool setObject(const char *key, OSObject *value) {
        value->retain();
        if (values.count(key)) values[key]->release();
        values[key] = value;
        return true;
    }
    OSObject *getProperty(const char *key) { return values.count(key) ? values[key] : nullptr; }
    OSObject *getObject(const char *key) { return getProperty(key); }
    ~OSDictionary() { for (auto &item : values) item.second->release(); }
};
struct OSArray : OSObject {
    std::vector<OSObject *> values;
    static OSArray *withCapacity(unsigned) { return new OSArray; }
    static OSArray *withArray(OSArray *other) {
        auto copy = new OSArray;
        for (auto value : other->values) copy->setObject(value);
        return copy;
    }
    bool setObject(OSObject *value) { value->retain(); values.push_back(value); return true; }
    unsigned getCount() { return static_cast<unsigned>(values.size()); }
    OSObject *getObject(unsigned index) { return values[index]; }
    ~OSArray() { for (auto value : values) value->release(); }
};
struct IOService : OSDictionary {
    const char *getName() { return "HDAU"; }
    bool setProperty(const char *key, OSObject *value) { return setObject(key, value); }
};
struct IOPCIDevice : IOService {};
using IORegistryEntry = IOService;
static bool wrapStart(IOService *, IOService *) { return true; }
static void report(const char *, const char *, ...) {}
static mach_vm_address_t orgProbe, orgStart;
static unsigned probeCalls;
static IOService *originalProbe(IOService *self, IOService *, SInt32 *score) {
    ++probeCalls; *score = 1; return self;
}
static bool originalAccepted = true;
static IOService *probeTarget(IOService *self, IOService *, SInt32 *score) {
    ++probeCalls; *score = 1; return originalAccepted ? self : nullptr;
}
static void setData(IOService &service, const char *key, uint32_t value) {
    auto data = new OSData(value); service.setObject(key, data); data->release();
}
static void setTestNumber(OSDictionary *dict, const char *key, uint32_t value) {
    auto number = OSNumber::withNumber(value, 32); assert(number);
    dict->setObject(key, number); number->release();
}

struct KernelPatcher {
    enum Error { NoError = 0, Failed = 1 };
    struct KextInfo { size_t loadIndex = 0; };
    struct RouteRequest {
        template <typename F> RouteRequest(const char *, F, mach_vm_address_t) {}
    };
    struct LookupPatch { void *info; const uint8_t *find; const uint8_t *replace; size_t size; unsigned count; };
    static size_t solvedIndex, routedIndex;
    static mach_vm_address_t solvedAddress;
    static size_t solvedSize;
    static bool solveOk, routeOk;
    static unsigned routeCalls, patchCalls;
    static Error error;
    mach_vm_address_t solveSymbol(size_t index, const char *, mach_vm_address_t address, size_t size) {
        solvedIndex = index; solvedAddress = address; solvedSize = size; return solveOk ? 1 : 0;
    }
    bool routeMultiple(size_t index, RouteRequest *, size_t) {
        ++routeCalls; routedIndex = index;
        if (routeOk) { orgProbe = reinterpret_cast<mach_vm_address_t>(&probeTarget); orgStart = 2; }
        else error = Error::Failed;
        return routeOk;
    }
    void clearError() { error = Error::NoError; }
    Error getError() { return error; }
    void applyLookupPatch(LookupPatch *) { ++patchCalls; error = Error::NoError; }
};
size_t KernelPatcher::solvedIndex, KernelPatcher::routedIndex;
mach_vm_address_t KernelPatcher::solvedAddress;
size_t KernelPatcher::solvedSize;
bool KernelPatcher::solveOk = true, KernelPatcher::routeOk = true;
unsigned KernelPatcher::routeCalls, KernelPatcher::patchCalls;
KernelPatcher::Error KernelPatcher::error = KernelPatcher::Error::NoError;
static KernelPatcher::KextInfo kHda;
static const uint16_t kKepler[] { 0x0E0A, 0x0E0B, 0x0E1B, 0x0E1A, 0x0E08 };
static uint16_t fixtureIds[5];
static size_t fixtureIdCount;
static size_t rematchCalls;
static size_t nvidiaAudioIds(uint16_t *ids, size_t max) {
    size_t count = fixtureIdCount < max ? fixtureIdCount : max;
    for (size_t i = 0; i < count; ++i) ids[i] = fixtureIds[i];
    return count;
}
static void rematchNvidiaAudio() { ++rematchCalls; }
'''


CHECKS = r'''
int main() {
    SInt32 score = 0;
    IOService controller;
    IOPCIDevice nvidia;
    setData(nvidia, "vendor-id", 0x10de); setData(nvidia, "class-code", 0x040300);
    setData(nvidia, "device-id", 0x22e9); setData(nvidia, "layout-id", 7);
    auto prior = OSArray::withCapacity(1);
    auto unrelated = OSDictionary::withCapacity(1); setTestNumber(unrelated, "VID", 0x1002);
    prior->setObject(unrelated); unrelated->release();
    controller.setObject("HwFactoryPrefixTranslation", prior);
    auto original = reinterpret_cast<mach_vm_address_t>(&originalProbe);
    orgProbe = original;
    assert(wrapProbe(&controller, &nvidia, &score) == &controller && probeCalls == 1);
    auto entries = dynamic_cast<OSArray *>(controller.getProperty("HwFactoryPrefixTranslation"));
    assert(entries && entries->getCount() == 2 && entries->getObject(0) == unrelated);
    auto translation = dynamic_cast<OSDictionary *>(entries->getObject(1));
    for (auto pair : std::map<std::string, uint32_t>{{"LayoutID", 7}, {"VID", 0x10de}, {"SourceDID", 0x9d70}, {"StandInDID", 0x22e9}})
        assert(dynamic_cast<OSNumber *>(translation->getProperty(pair.first.c_str()))->value == pair.second);
    assert(entries != prior && prior->getCount() == 1);
    assert(wrapProbe(&controller, &nvidia, &score) == &controller && probeCalls == 2);
    assert(dynamic_cast<OSArray *>(controller.getProperty("HwFactoryPrefixTranslation"))->getCount() == 2);
    prior->release();

    IOService noLayoutController;
    IOPCIDevice noLayout;
    setData(noLayout, "vendor-id", 0x10de); setData(noLayout, "class-code", 0x040300); setData(noLayout, "device-id", 0x22e9);
    assert(wrapProbe(&noLayoutController, &noLayout, &score) == &noLayoutController && probeCalls == 3);
    assert(!noLayoutController.getProperty("HwFactoryPrefixTranslation"));

    IOService intelController;
    IOPCIDevice intel;
    setData(intel, "vendor-id", 0x8086); setData(intel, "class-code", 0x040300);
    assert(wrapProbe(&intelController, &intel, &score) == &intelController && probeCalls == 4);
    assert(!intelController.getProperty("HwFactoryPrefixTranslation"));

    IOService failureController;
    OSNumber::fail = true;
    assert(wrapProbe(&failureController, &nvidia, &score) == nullptr && probeCalls == 4);
    OSNumber::fail = false;

    KernelPatcher patcher;
    KernelPatcher::routeCalls = KernelPatcher::patchCalls = rematchCalls = 0;
    KernelPatcher::solveOk = false; KernelPatcher::routeOk = true; fixtureIdCount = 1; fixtureIds[0] = 0x22e9;
    kHda.loadIndex = 4; orgProbe = 0; orgStart = 0;
    onHdaLoaded(nullptr, patcher, 19, 0x1234, 4096);
    assert(KernelPatcher::solvedIndex == 19 && KernelPatcher::solvedAddress == 0x1234 && KernelPatcher::solvedSize == 4096);
    assert(KernelPatcher::routeCalls == 0 && KernelPatcher::patchCalls == 0 && rematchCalls == 0);

    KernelPatcher::solveOk = true; KernelPatcher::routeOk = false;
    onHdaLoaded(nullptr, patcher, 19, 0x1234, 4096);
    assert(KernelPatcher::routedIndex == 19 && KernelPatcher::patchCalls == 0 && rematchCalls == 0);

    KernelPatcher::routeOk = true;
    onHdaLoaded(nullptr, patcher, 19, 0x1234, 4096);
    assert(kHda.loadIndex == 19 && KernelPatcher::patchCalls == 1 && rematchCalls == 1);
    onHdaLoaded(nullptr, patcher, 19, 0x1234, 4096);
    assert(KernelPatcher::routeCalls == 2 && KernelPatcher::patchCalls == 1 && rematchCalls == 1);
    puts("NMHDA production probes: native-ID conversion, preservation, no-layout pass-through, deduplication, runtime index and hook failure passed.");
}
'''


def main():
    source = SOURCE.read_text(encoding="utf-8")
    config = between(source, "PluginConfiguration ADDPR(config)", "// kmutil")
    assert "KernelVersion::Sequoia, KernelVersion::Tahoe" in config
    code = HARNESS
    code += between(source, "static uint32_t readU32", "// NVIDIA audio functions:")
    code += between(source, "static bool isNvidiaAudio", "static bool wrapStart")
    code += between(source, "static void onHdaLoaded", "static void pluginStart")
    code += CHECKS

    command = os.environ.get("CXX")
    compiler = shlex.split(command) if command else None
    if not compiler:
        compiler_path = shutil.which("clang++") or shutil.which("g++") or shutil.which("c++")
        if not compiler_path:
            raise SystemExit("NMHDA extracted-function test requires clang++ or g++ on PATH")
        compiler = [compiler_path]
    with tempfile.TemporaryDirectory(prefix="nmhda-native-id-") as temp:
        cpp = Path(temp) / "test.cpp"
        exe = Path(temp) / ("test.exe" if os.name == "nt" else "test")
        cpp.write_text(code, encoding="utf-8")
        subprocess.run(compiler + ["-std=c++17", "-Wall", "-Wextra", "-Werror", str(cpp), "-o", str(exe)], check=True)
        subprocess.run([str(exe)], check=True)


if __name__ == "__main__":
    main()
