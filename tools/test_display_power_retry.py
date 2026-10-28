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
# per-connection power stays the 1.14.7 no-op success: head power follows setPowerState only
connection_power = connection[:connection.index('switch (attribute)')]
assert 'return kIOReturnSuccess' in connection_power and 'setHeadPower' not in connection_power
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
    bool fPowerOn=true, pagingState=true, fSysSlept=false, gpuReady=true;
    unsigned familyWakes=0;
    void *fSurf=this;
    unsigned fHead=0, sleeps=0, wakes=0, notifications=0, audioPublishes=0;
    void publishDisplayAudioProperties() { audioPublishes++; }
    int fStopping=0, fReflipRc=0;
    bool headOff() { sleeps++; return sleepOK; }
    bool reflip() { wakes++; return wakeOK; }
    void handleEvent(int event) { if(event==kIOFBNotifyDidPowerOn) notifications++; }
    bool rmGpuReady() { return gpuReady; }
    void familyDidWake() { familyWakes++; notifications++; pagingState=true; }
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
    // a failed wake stays retryable, and WindowServer is still told the display is on (1.14.7 behaviour)
    assert(f.fHeadSlept && f.wakes==1 && f.notifications==1);
    // the display's audio is published again only once the head really is back (PR #366)
    assert(f.audioPublishes==0);
    f.wakeOK=true; f.setPowerState(1,nullptr);
    assert(!f.fHeadSlept && f.wakes==2 && f.notifications==2 && f.audioPublishes==1);
    f.setPowerState(1,nullptr); assert(f.wakes==2);
    f.setPowerState(0,nullptr); assert(f.fHeadSlept);
    // -nvrmnodpms: display sleep leaves the head on, but a head that is off (GPU standby) is still lit again
    disabled=true; unsigned wakes=f.wakes;
    assert(f.setHeadPower(true) && !f.fHeadSlept && f.wakes==wakes+1);
    sleeps=f.sleeps; assert(f.setHeadPower(false) && !f.fHeadSlept && f.sleeps==sleeps);
    wakes=f.wakes;
    disabled=false; f.fStopping=1; assert(!f.setHeadPower(true)); f.fStopping=0;
    lockAvailable=false; assert(!f.setHeadPower(true)); lockAvailable=true;
    assert(f.setHeadPower(true) && !f.fHeadSlept);
    f.fModeSet=false; assert(f.setHeadPower(false) && !f.fHeadSlept);
    f.fModeSet=true;
    // system sleep: NVRM already put the GPU in standby, so the head counts as off without a push to NVKMS
    f.gpuReady=false; sleeps=f.sleeps; f.setPowerState(0,nullptr);
    assert(f.fHeadSlept && f.sleeps==sleeps && !f.fPowerOn);
    // a display wake before NVRM resumed the GPU pushes nothing and leaves the head for NVRM's resume to light
    wakes=f.wakes; unsigned notes=f.notifications; f.setPowerState(1,nullptr);
    assert(f.fHeadSlept && f.wakes==wakes && f.fPowerOn && f.notifications==notes+1 && !f.familyWakes);
    // the first power-on after a system sleep lights the head and does the family's wake work once
    f.setPowerState(0,nullptr); f.gpuReady=true; f.fSysSlept=true; f.pagingState=false;
    notes=f.notifications; f.setPowerState(1,nullptr);
    assert(!f.fHeadSlept && f.wakes==wakes+1 && f.familyWakes==1 && !f.fSysSlept && f.pagingState);
    assert(f.notifications==notes+1);
    f.setPowerState(1,nullptr); assert(f.familyWakes==1 && f.notifications==notes+2);
    puts("PASS: failed sleep, failed wake retry with notification, audio republished only on a real wake, connection power no-op, repeated requests, opt-out, shutdown, unavailable gate, GPU standby, wake before resume, system wake");
}
'''
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.cpp').write_text(fixture + methods + checks)
    subprocess.run(['clang++', '-std=c++17', '-pthread', str(path / 'test.cpp'),
                    '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)
