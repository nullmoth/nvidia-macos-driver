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
assert 'kNVRMMinBacklight = 5' in source
assert 'v < kNVRMMinBacklight ? kNVRMMinBacklight : v' in apply
assert 'h->fUserLevel = h->fLevel;' in source

fixture = r'''
#include <cassert>
#include <cstdint>
using SInt32 = int32_t;
using UInt32 = uint32_t;
struct OSSymbol {};
struct Call { unsigned entered = 0; };
using thread_call_t = Call *;
struct OSDictionary {
    bool hasValue = false;
    const OSSymbol *getObject(const OSSymbol *) { return hasValue ? (const OSSymbol *)this : nullptr; }
};
static OSSymbol brightnessKey, linearKey, otherKey, valueKey;
static const OSSymbol *gIODisplayBrightnessKey = &brightnessKey;
static const OSSymbol *gIODisplayLinearBrightnessKey = &linearKey;
static const OSSymbol *gIODisplayValueKey = &valueKey;
static void thread_call_enter(thread_call_t call) { ++call->entered; }
static SInt32 OSAddAtomic(SInt32 delta, volatile SInt32 *value) { return __atomic_add_fetch(value, delta, __ATOMIC_SEQ_CST) - delta; }
struct NVRMBacklightHandler {
    Call call;
    thread_call_t fApply = &call;
    volatile SInt32 fPending = -1;
    volatile SInt32 fUserLevel = 100;
    bool doIntegerSet(OSDictionary *, const OSSymbol *, UInt32);
};
'''
checks = r'''
int main() {
    NVRMBacklightHandler h;
    OSDictionary user{true}, power{false};
    assert(h.doIntegerSet(&user, gIODisplayLinearBrightnessKey, 35));
    assert(h.fUserLevel == 35 && h.fPending == 35 && h.call.entered == 1);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, 100));
    assert(h.fUserLevel == 35 && h.fPending == 35 && h.call.entered == 2);
    assert(h.doIntegerSet(&power, gIODisplayBrightnessKey, 12));
    assert(h.fUserLevel == 35 && h.fPending == 12 && h.call.entered == 3);
    assert(h.doIntegerSet(&user, gIODisplayBrightnessKey, 64));
    assert(h.fUserLevel == 64 && h.fPending == 64 && h.call.entered == 4);
    assert(!h.doIntegerSet(&user, &otherKey, 80) && h.fPending == 64 && h.call.entered == 4);
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    test = path / 'check.cpp'
    test.write_text(fixture + method + checks, encoding='utf-8')
    subprocess.run(['clang++', '-std=c++17', '-Wall', '-Wextra', str(test), '-o', str(path / 'check')], check=True)
    subprocess.run([str(path / 'check')], check=True)
print('PASS: production doIntegerSet preserves the user target, bounds temporary power requests, and queues asynchronously; the 5% floor remains in applyFire')
