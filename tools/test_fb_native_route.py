from pathlib import Path
import subprocess
import tempfile

source = (Path(__file__).resolve().parents[1] / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
def function(signature):
    start = source.index(signature)
    return source[start:source.index('\n}\n', start) + 3]

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstring>
#include <cstdio>
using UInt32 = uint32_t;
using SInt32 = int32_t;
struct OSSymbol {};
static OSSymbol brightness, other;
static const OSSymbol *gIODisplayBrightnessKey = &brightness;
static const OSSymbol *gIODisplayParametersKey = &other;
static const char *kIODisplayCapabilityStringKey = "IODisplayCapabilityString";
struct OSObject { void release() {} };
struct OSDictionary : OSObject {
    OSObject *range = nullptr;
    OSObject *getObject(const OSSymbol *key) { return key == &brightness ? range : nullptr; }
    static OSDictionary *withCapacity(int) { return new OSDictionary; }
    static OSDictionary *withDictionary(OSDictionary *old) { return new OSDictionary(*old); }
};
struct OSData : OSObject {
    static OSData *withBytes(const char *text, size_t length) {
        assert(length == 10 && std::memcmp(text, "model(EDM)", length) == 0);
        return new OSData;
    }
};
#define OSDynamicCast(T, ptr) static_cast<T*>(ptr)
#define OSSafeReleaseNULL(ptr) do { if (ptr) ptr->release(); ptr = nullptr; } while (0)
#define BLLOG(...) do {} while (0)
struct IODisplay {
    OSDictionary *params = nullptr;
    OSObject *capability = nullptr;
    bool allowParams = true;
    static bool addParameter(OSDictionary *p, const OSSymbol *k, SInt32 min, SInt32 max) {
        assert(k == &brightness && min == 0 && max == 100);
        p->range = new OSDictionary;
        return true;
    }
    static bool setParameter(OSDictionary *p, const OSSymbol *k, SInt32 value) {
        assert(p->range && k == &brightness && value == 30);
        return true;
    }
    OSObject *copyProperty(const OSSymbol *) { return params; }
    OSObject *getProperty(const char *) { return capability; }
    bool setProperty(const OSSymbol *, OSDictionary *p) { if (allowParams) params = p; return allowParams; }
    bool setProperty(const char *, OSObject *p) { capability = p; return true; }
    void removeProperty(const char *) { capability = nullptr; }
};
struct FB {
    bool reply = true;
    unsigned writes = 0;
    bool readDdcBrightness(UInt32 &value, UInt32 &max, UInt32 &gen, unsigned) {
        value = 30; max = 100; gen = 3;
        return reply;
    }
    bool queueDdcBrightness(UInt32 value, UInt32 gen) {
        assert(value == 38 && gen == 3);
        ++writes;
        return true;
    }
};
struct NVRMExternalBrightness {
    FB *fFramebuffer;
    IODisplay *fRetryDisplay = nullptr;
    OSDictionary *fParameters = nullptr;
    UInt32 fGeneration = 0, fMaximum = 0;
    unsigned fRetriesLeft = 0, fRetryDelayMs = 0;
    bool publishParameters(IODisplay *, bool &);
    bool doIntegerSet(OSDictionary *, const OSSymbol *, UInt32);
};
'''
checks = r'''
int main() {
    FB fb;
    NVRMExternalBrightness handler{&fb};
    IODisplay display;
    bool retry = false;
    fb.reply = false;
    assert(!handler.publishParameters(&display, retry) && retry && !display.params && !display.capability);
    fb.reply = true;
    display.allowParams = false;
    assert(!handler.publishParameters(&display, retry) && !display.capability);
    display.allowParams = true;
    assert(handler.publishParameters(&display, retry) && display.params && display.capability);
    auto *range = static_cast<OSDictionary*>(display.params->range);
    OSDictionary unrelated;
    assert(!handler.doIntegerSet(&unrelated, &brightness, 38));
    assert(!handler.doIntegerSet(range, &other, 38));
    assert(!handler.doIntegerSet(range, &brightness, 101));
    assert(handler.doIntegerSet(range, &brightness, 38));
    assert(handler.doIntegerSet(display.params, &brightness, 38));
    assert(fb.writes == 2);
    IODisplay another;
    OSObject existing;
    another.capability = &existing;
    assert(handler.publishParameters(&another, retry) && another.capability == &existing);
    puts("PASS: EDM capability follows verified hardware, existing capability survives, only owned brightness requests queue");
}
'''
with tempfile.TemporaryDirectory() as name:
    cpp = Path(name) / 'route.cpp'
    exe = Path(name) / 'route'
    constants = '\n'.join(line for line in source.splitlines() if line.startswith('static constexpr unsigned kNVRMDdcProbe') and 'ReplyMs' in line)
    cpp.write_text(fixture + constants + function('bool NVRMExternalBrightness::publishParameters') +
        function('bool NVRMExternalBrightness::doIntegerSet') + checks)
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Werror', str(cpp), '-o', str(exe)], check=True)
    subprocess.run([str(exe)], check=True)
