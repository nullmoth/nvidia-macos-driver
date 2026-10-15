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
static int32_t gClock = 0;
static uint64_t mach_absolute_time() { gClock += 1000; return (uint64_t)gClock; } // +1 us per call
static void IOSleep(unsigned) { gClock += 1000; }
static SInt32 OSIncrementAtomic(volatile SInt32 *p) { return (*p)++; }

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
    src = fixture.replace('WAIT_REGION', region_text)
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(src)
        subprocess.run(['c++', '-std=c++17', '-O0', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)

r = build_and_run(region)
assert r.returncode == 0 and r.stdout.strip() == 'OK', \
    f'fixed wait failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print('fixed source: PASS (waits/timeouts/max_us report real values; knob bypass honored)')

r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect the dead counters'
print('negative control (pre-fix region): FAILS as required (counters stayed zero)')
print('PASS')
