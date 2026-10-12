#!/usr/bin/env python3
"""Exercise the production display power transitions without a display change."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
start = source.index('bool NVRMNVDAFramebuffer::setHeadPower(')
methods = source[start:source.index('IOReturn NVRMNVDAFramebuffer::setAttribute(', start)]
head_off = source[source.index('bool NVRMNVDAFramebuffer::headOff()'):start]
assert 'submitConfig(cfg, rep, NV_TRUE)' in head_off
assert 'fKms->applyModeSetConfig' not in head_off
connection = source[source.index('IOReturn NVRMNVDAFramebuffer::setAttributeForConnection('):]
assert 'setHeadPower(value != 0)' in connection
fixture = r'''
#include <cassert>
#include <cstdio>
#include <mutex>
using IOReturn=int;
struct IOService {};
enum { kIOPMAckImplied=0, kIOFBNotifyDidPowerOn=1, kIOFBNotifyWillPowerOff=2 };
static bool disabled=false, lockAvailable=true;
static std::recursive_mutex displayLock;
struct NVRMDisplayGate {
    NVRMDisplayGate() { displayLock.lock(); }
    ~NVRMDisplayGate() { displayLock.unlock(); }
    bool valid() { return lockAvailable; }
};
static bool PE_parse_boot_argn(const char*,char*,unsigned long) { return disabled; }
static int OSAddAtomic(int n,int *value) { int old=*value;*value+=n;return old; }
#define FBLOG(...) ((void)0)
struct NVRMNVDAFramebuffer {
    bool fHeadSlept=false, fModeSet=true, sleepOK=true, wakeOK=true;
    void *fSurf=this;
    unsigned fHead=0, sleeps=0, wakes=0, notifications=0;
    int fStopping=0, fReflipRc=0;
    bool headOff() { sleeps++; return sleepOK; }
    bool reflip() { wakes++; return wakeOK; }
    void handleEvent(int event) { if(event==kIOFBNotifyDidPowerOn) notifications++; }
    bool setHeadPower(bool awake);
    IOReturn setPowerState(unsigned long ordinal,IOService *device);
};
'''
checks = r'''
int main() {
    NVRMNVDAFramebuffer f;
    assert(f.setHeadPower(true) && !f.wakes);
    f.sleepOK=false; assert(!f.setHeadPower(false) && !f.fHeadSlept);
    f.sleepOK=true; assert(f.setHeadPower(false) && f.fHeadSlept);
    unsigned sleeps=f.sleeps; assert(f.setHeadPower(false) && f.sleeps==sleeps);
    f.wakeOK=false; f.setPowerState(1,nullptr);
    assert(f.fHeadSlept && f.wakes==1 && !f.notifications);
    f.wakeOK=true; f.setPowerState(1,nullptr);
    assert(!f.fHeadSlept && f.wakes==2 && f.notifications==1);
    f.setPowerState(1,nullptr); assert(f.wakes==2);
    f.setPowerState(0,nullptr); assert(f.fHeadSlept);
    disabled=true; unsigned wakes=f.wakes;
    assert(f.setHeadPower(true) && f.fHeadSlept && f.wakes==wakes);
    disabled=false; f.fStopping=1; assert(!f.setHeadPower(true)); f.fStopping=0;
    lockAvailable=false; assert(!f.setHeadPower(true)); lockAvailable=true;
    assert(f.setHeadPower(true) && !f.fHeadSlept);
    f.fModeSet=false; assert(f.setHeadPower(false) && !f.fHeadSlept);
    puts("PASS: failed sleep, failed wake retry, notifications, repeated requests, opt-out, shutdown, unavailable gate");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + methods + checks)
    subprocess.run(['clang++', '-std=c++17', '-pthread', str(path / 'test.cpp'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
