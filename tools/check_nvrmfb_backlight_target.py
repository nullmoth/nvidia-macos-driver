#!/usr/bin/env python3
"""Compile the production backlight request method against an asynchronous-call fixture."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text(encoding='utf-8')
start = source.index('bool NVRMBacklightHandler::doIntegerSet(')
end = source.index('\n}\n', start) + 3
method = source[start:end]
apply_start = source.index('void NVRMBacklightHandler::applyFire(')
apply_end = source.index('\n}\n', apply_start) + 3
apply = source[apply_start:apply_end]

fixture = r'''
#include <cassert>
#include <cstdint>
#include <map>
using SInt32 = int32_t;
using UInt32 = uint32_t;
using UInt64 = uint64_t;
enum { kNVRMBacklightMax = 0xFFFF };
static constexpr SInt32 kAppleBacklightDefaultMax = 255;
static constexpr UInt32 kNVRMBacklightSaveDelaySeconds = 2, kSecondScale = 1;
static void clock_interval_to_deadline(UInt32 seconds, UInt32, uint64_t *deadline) { *deadline = seconds; }
struct OSSymbol {};
struct Call { unsigned entered = 0; };
using thread_call_t = Call *;
struct OSDictionary {
    bool hasValue = false;
    std::map<const OSSymbol *, UInt32> values;
    const OSSymbol *getObject(const OSSymbol *) { return hasValue ? (const OSSymbol *)this : nullptr; }
};
struct IODisplay {
    static bool setParameter(OSDictionary *params, const OSSymbol *key, SInt32 value) {
        params->values[key] = (UInt32)value; return true;
    }
};
static OSSymbol brightnessKey, linearKey, otherKey, valueKey;
static const OSSymbol *gIODisplayBrightnessKey = &brightnessKey;
static const OSSymbol *gIODisplayLinearBrightnessKey = &linearKey;
static const OSSymbol *gIODisplayValueKey = &valueKey;
static void thread_call_enter(thread_call_t call) { ++call->entered; }
static SInt32 OSAddAtomic(SInt32 delta, volatile SInt32 *value) { return __atomic_add_fetch(value, delta, __ATOMIC_SEQ_CST) - delta; }
static OSSymbol probeKey, linearProbeKey;
struct NVRMBacklightHandler {
    static const OSSymbol *sBrightnessProbe, *sLinearBrightnessProbe;
    Call call;
    thread_call_t fApply = &call;
    volatile SInt32 fPending = -1;
    volatile SInt32 fUserLevel = 100;
    volatile SInt32 fPersistPending = -1;
    volatile UInt64 fPersistDeadline = 0;
    OSDictionary *fParameters = nullptr;
    bool doIntegerSet(OSDictionary *, const OSSymbol *, UInt32);
};
const OSSymbol *NVRMBacklightHandler::sBrightnessProbe = &probeKey;
const OSSymbol *NVRMBacklightHandler::sLinearBrightnessProbe = &linearProbeKey;
'''
checks = r'''
int main() {
    NVRMBacklightHandler h;
    OSDictionary user, power;
    user.hasValue = true;
    OSDictionary parameters;
    h.fParameters = &parameters;
    assert(h.doIntegerSet(&user, gIODisplayLinearBrightnessKey, 35));
    assert(h.fUserLevel == 35 && h.fPersistPending == -1 && h.fPending == 35 && h.call.entered == 1);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, 100));
    assert(h.fUserLevel == 35 && h.fPersistPending == -1 && h.fPending == 35 && h.call.entered == 2);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, kAppleBacklightDefaultMax));
    assert(h.fPending == 35 && h.fUserLevel == 35);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, 12));
    assert(h.fUserLevel == 35 && h.fPersistPending == -1 && h.fPending == 12 && h.call.entered == 3);
    assert(h.doIntegerSet(&user, gIODisplayBrightnessKey, 64));
    assert(h.fUserLevel == 64 && h.fPersistPending == -1 && h.fPending == 64 && h.call.entered == 4);
    assert(parameters.values[gIODisplayLinearBrightnessKey] == 64);
    assert(!h.doIntegerSet(&user, &otherKey, 80) && h.fPending == 64 && h.call.entered == 4);
    // GetUserBrightness writes a conversion probe with a potentially stale value (18 on the affected laptop).
    assert(h.doIntegerSet(&user, NVRMBacklightHandler::sLinearBrightnessProbe, 18));
    assert(parameters.values[NVRMBacklightHandler::sBrightnessProbe] == 18);
    assert(h.fUserLevel == 64 && h.fPersistPending == -1 && h.fPending == 64 && h.call.entered == 4);
    // RefreshUserBrightness converts in the other direction before sending the actual linear request.
    assert(h.doIntegerSet(&user, NVRMBacklightHandler::sBrightnessProbe, 83));
    assert(parameters.values[NVRMBacklightHandler::sLinearBrightnessProbe] == 83);
    assert(h.fUserLevel == 64 && h.fPending == 64 && h.call.entered == 4);
    assert(h.doIntegerSet(&user, gIODisplayLinearBrightnessKey, 83));
    assert(h.fUserLevel == 83 && h.fPersistPending == -1 && h.fPending == 83 && h.call.entered == 5);
    assert(h.doIntegerSet(&user, NVRMBacklightHandler::sLinearBrightnessProbe, 18));
    assert(h.fUserLevel == 83 && h.fPersistPending == -1 && h.fPending == 83 && h.call.entered == 5);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, 100) && h.fPending == 83);
    assert(h.doIntegerSet(&user, NVRMBacklightHandler::sBrightnessProbe, 0xFFFFFFFFU));
    assert(parameters.values[NVRMBacklightHandler::sLinearBrightnessProbe] == kNVRMBacklightMax && h.fUserLevel == 83);
    // Fractional normalized requests retain their logical value through both conversion directions.
    for (UInt32 target : {0U, 1U, 4096U, 20777U, 32768U, 65534U, 65535U}) {
        unsigned entered = h.call.entered;
        assert(h.doIntegerSet(&user, NVRMBacklightHandler::sBrightnessProbe, target));
        assert(parameters.values[NVRMBacklightHandler::sLinearBrightnessProbe] == target);
        assert(h.doIntegerSet(&user, NVRMBacklightHandler::sLinearBrightnessProbe, target));
        assert(parameters.values[NVRMBacklightHandler::sBrightnessProbe] == target && h.call.entered == entered);
        assert(h.doIntegerSet(&user, gIODisplayLinearBrightnessKey, target));
        assert(parameters.values[gIODisplayBrightnessKey] == target);
        assert(h.fUserLevel == (SInt32)target && h.fPending == (SInt32)target);
        assert(h.doIntegerSet(&user, gIODisplayBrightnessKey, target));
        assert(parameters.values[gIODisplayLinearBrightnessKey] == target);
        assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, kNVRMBacklightMax));
        assert(h.fPending == (SInt32)target && h.fUserLevel == (SInt32)target);
    }
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    test = path / 'check.cpp'
    test.write_text(fixture + method + checks, encoding='utf-8')
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Wextra', str(test), '-o', str(path / 'check')], check=True)
    subprocess.run([str(path / 'check')], check=True)
print('PASS: conversion probes update their counterpart without changing the pending/user/persisted target; real requests retain logical precision and synchronize both public values and power requests bounded; 5% floor unchanged')
