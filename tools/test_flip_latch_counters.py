#!/usr/bin/env python3
"""The debug.nvrmfb_flip_latch* sysctls must report the real flip completion wait.

The counters existed but nothing wrote them, so an operator debugging flicker read zeros. The wait
region is lifted from submitConfig in nvrm-fb.cpp and exercised: a completed flip bumps the wait count
and records its duration, a flip whose completion never arrives bumps the timeout count and faults,
and the gFlipLatch knob at 0 bypasses the wait. The negative control is the pre-fix region (no
counters, no gate) and leaves the counters at zero."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()

start = source.index('if (bootstrap || !gFlipLatch) return NV_TRUE;')
end = source.index('fSubmissionFaulted = true; return NV_FALSE;', start)
end = source.index('\n', end)
region = source[start:end]
# the event-woken wait the region calls, lifted whole so the test runs the shipped loop
w0 = source.index('static IOLock *sFlipWaitLock;')
w1 = source.index('\n}\n', source.index('bool NVRMNVDAFramebuffer::waitFlipDone(unsigned limitMs)')) + 3
waiter = source[w0:w1]
statics = waiter[:waiter.index('void NVRMNVDAFramebuffer::flipWake')]
body = waiter[waiter.index('bool NVRMNVDAFramebuffer::waitFlipDone'):].replace('NVRMNVDAFramebuffer::', '')
assert 'gFlipLatchWaits' in region and 'gFlipLatchTimeouts' in region, \
    'the completion wait no longer updates the flip-latch counters'

control = r'''    if (bootstrap) return NV_TRUE;
    for (unsigned i = 0; i < 100; ++i) {
        NvBool pending = NV_TRUE;
        if (!fKms->getFlipPendingStatus(fDev, fHead, NVKMS_KAPI_LAYER_PRIMARY_IDX, &pending)) break;
        if (!pending) return NV_TRUE;
        IOSleep(1);
    }
    FBLOG("presentation blocked: submitted completion unavailable on head %u", fHead);
    fSubmissionFaulted = true; return NV_FALSE;'''

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
using NvU32=uint32_t; using SInt32=int32_t; using NvBool=bool;
enum { NV_TRUE=1, NV_FALSE=0, NVKMS_KAPI_LAYER_PRIMARY_IDX=0 };
#define FBLOG(...) ((void)0)
static int64_t gClock = 0;   // nanoseconds, the absolute-time unit here
static uint64_t mach_absolute_time() { gClock += 1000; return (uint64_t)gClock; } // +1 us per call
static void IOSleep(unsigned) { gClock += 1000000; }
static SInt32 OSIncrementAtomic(volatile SInt32 *p) { return (*p)++; }
static void absolutetime_to_nanoseconds(uint64_t t, uint64_t *ns) { *ns = t; }
enum { kMicrosecondScale = 1000, kMillisecondScale = 1000000, THREAD_UNINT = 0 };
using event_t = void *;
struct IOLock { int held; };
static IOLock gLockObj;
static IOLock *IOLockAlloc() { return &gLockObj; }
static void IOLockFree(IOLock *) {}
static bool OSCompareAndSwapPtr(void *o, void *n, void *volatile *p) { if (*p != o) return false; *p = n; return true; }
static void IOLockLock(IOLock *l) { l->held = 1; }
static void IOLockUnlock(IOLock *l) { l->held = 0; }
static void clock_interval_to_deadline(uint64_t v, uint64_t scale, uint64_t *out) { *out = (uint64_t)gClock + v * scale; }
static int gSleeps = 0; static int64_t gLongestSleep = 0;
static int IOLockSleepDeadline(IOLock *l, event_t, uint64_t deadline, int) {
    assert(l->held && "the flip wait must sleep holding its lock");
    int64_t d = (int64_t)deadline - gClock; if (d > gLongestSleep) gLongestSleep = d;
    gSleeps++; if ((int64_t)deadline > gClock) gClock = (int64_t)deadline; return 0; }
STATICS

static int gFlipLatch = 1;
static int gFlipLatchWaits = 0, gFlipLatchTimeouts = 0, gFlipLatchMaxUs = 0;
enum Mode { COMPLETE, STUCK, FAIL };
struct Kms {
    Mode mode = COMPLETE;
    bool getFlipPendingStatus(void *, unsigned, int, NvBool *pending) {
        if (mode == FAIL) return false;
        *pending = (mode == STUCK);
        return true;
    }
};
struct FB {
    Kms *fKms=nullptr; void *fDev=this; unsigned fHead=0; bool fSubmissionFaulted=false;
WAITER
    NvBool waitComplete(bool bootstrap) {
WAIT_REGION
        return NV_TRUE; // control's path when it falls through (never reached in fixed region)
    }
};
int main() {
    // A completed flip: one wait recorded, no timeout, a duration captured.
    { Kms k; k.mode=COMPLETE; FB f; f.fKms=&k;
      gFlipLatchWaits=gFlipLatchTimeouts=gFlipLatchMaxUs=0; gFlipLatch=1;
      assert(f.waitComplete(false) == NV_TRUE);
      assert(gFlipLatchWaits == 1 && "a completed flip must count as one latch wait");
      assert(gFlipLatchTimeouts == 0);
      assert(gFlipLatchMaxUs >= 1 && "the wait duration must be recorded"); }
    // A flip whose completion never arrives: timeout counted and the pipe faults.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k;
      gFlipLatchWaits=gFlipLatchTimeouts=0; gFlipLatch=1;
      assert(f.waitComplete(false) == NV_FALSE);
      assert(gFlipLatchTimeouts == 1 && f.fSubmissionFaulted); }
    // A stuck flip under the event-woken wait: the pending flag is re-read at least every 250 us (never a 1 ms sleep,
    // the oversleep that halved the desktop rate on fast panels) and the wait still ends at its 100 ms bound.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k; gSleeps=0; gLongestSleep=0;
      const int64_t t0 = gClock;
      assert(!f.waitFlipDone(100));
      const int64_t took = gClock - t0;
      printf("stuck flip: %d sleeps, longest %lld us, gave up after %lld us\n", gSleeps, (long long)(gLongestSleep / 1000), (long long)(took / 1000));
      assert(gLongestSleep <= 250000 && "a sleep longer than 250 us between pending checks");
      assert(gSleeps >= 300 && "the pending flag must be re-read every 250 us across the bound");
      assert(took >= 100000000 && took < 101000000 && "the wait must end at its 100 ms bound"); }
    // Knob off: the wait is bypassed and no counter moves.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k;
      gFlipLatchWaits=gFlipLatchTimeouts=0; gFlipLatch=0;
      assert(f.waitComplete(false) == NV_TRUE);
      assert(gFlipLatchWaits == 0 && gFlipLatchTimeouts == 0 && !f.fSubmissionFaulted); }
    // Bootstrap commit: no wait, no counters.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k;
      gFlipLatchWaits=0; gFlipLatch=1;
      assert(f.waitComplete(true) == NV_TRUE);
      assert(gFlipLatchWaits == 0); }
    printf("OK\n");
    return 0;
}
'''

def build_and_run(region_text):
    src = fixture.replace('WAIT_REGION', region_text).replace('STATICS', statics).replace('WAITER', body)
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(src)
        subprocess.run(['c++', '-std=c++17', '-O0', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)

r = build_and_run(region)
assert r.returncode == 0 and r.stdout.strip().endswith('OK'), \
    f'fixed wait failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print(r.stdout.strip().splitlines()[0])
print('fixed source: PASS (waits/timeouts/max_us report real values; knob bypass honored; stuck flip re-read every 250 us to the 100 ms bound)')

r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect the dead counters'
print('negative control (pre-fix region): FAILS as required (counters stayed zero)')
print('PASS')
