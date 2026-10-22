#!/usr/bin/env python3
"""The flip completion wait is measured in refresh periods, returns only once the flip is on the panel, and reports.

A committed flip latches at the next vblank, so the wait runs in steps of one period and a quarter (from the mode's
raster and pixel clock); a step that misses counts as a late flip and the wait goes on, because the caller completes
the transaction (WindowServer then reuses the surface the flip replaced) as soon as submitConfig returns. Only a flip
still pending 100 ms after its submit faults the head. A prior flip that is merely late costs one present on the frame
path (IN_PROGRESS, the frame stays on the panel). The real wait regions are lifted from nvrm-fb.cpp and exercised.
Two negative controls must fail: the constant 100 ms region (no late count) and the region that let a late frame go
before its flip landed (the surface it replaced could still be scanned out while WindowServer reused it)."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()


def lift_body(name):
    """The body of a single-level member function, by its definition line."""
    start = source.index(name)
    body_open = source.index('{', start) + 1
    depth, i = 1, body_open
    while depth:
        if source[i] == '{': depth += 1
        elif source[i] == '}': depth -= 1
        i += 1
    return source[body_open:i - 1]


# the latch region of submitConfig: from the gate to the fault at its end
start = source.index('if (bootstrap || !gFlipLatch) return NV_TRUE;')
end = source.index('fSubmissionFaulted = true; return NV_FALSE;', start)
end = source.index('\n', end)
region = source[start:end]
# the prior-completion wait that precedes a new submit (the if block's body, closing brace included)
sc = source.index('NvBool NVRMNVDAFramebuffer::submitConfig(')
p0 = source.index('if (fSubmissionSeen) {', sc) + len('if (fSubmissionSeen) {')
p1 = source.index('    const bool tracked', p0)
prior_region = source[p0:p1]
# the event-woken wait, lifted whole so the test runs the shipped loop
w0 = source.index('static IOLock *sFlipWaitLock;')
w1 = source.index('\n}\n', source.index('bool NVRMNVDAFramebuffer::waitFlipDone(uint64_t limitUs)')) + 3
waiter = source[w0:w1]
statics = waiter[:waiter.index('void NVRMNVDAFramebuffer::flipWake')]
body = waiter[waiter.index('bool NVRMNVDAFramebuffer::waitFlipDone'):].replace('NVRMNVDAFramebuffer::', '')
vbl_body = lift_body('uint64_t NVRMNVDAFramebuffer::vblPeriodAbs()')
wait_us_body = lift_body('uint64_t NVRMNVDAFramebuffer::flipWaitUs()')
prior_body = lift_body('int NVRMNVDAFramebuffer::priorFlipState()')
stuck_line = source[source.index('static const uint64_t kFlipStuckUs'):]
stuck_line = stuck_line[:stuck_line.index('\n')]

assert 'gFlipLatchWaits' in region and 'gFlipLatchTimeouts' in region, \
    'the completion wait no longer updates the flip-latch counters'
assert 'waitFlipDone(flipWaitUs())' in region, \
    'the latch wait is bounded by a constant again instead of the refresh period'

# the region a first draft of this change had: the frame path let go after one period, before its flip landed
early_control = region.replace('    for (;;) {', '    if (frame) return NV_TRUE;\n    for (;;) {', 1)
assert early_control != region

control = r'''    if (bootstrap || !gFlipLatch) return NV_TRUE;
    OSIncrementAtomic((volatile SInt32 *)&gFlipLatchWaits);
    const uint64_t latch0 = mach_absolute_time();
    if (waitFlipDone(100000)) {
        uint64_t ns = 0; absolutetime_to_nanoseconds(mach_absolute_time() - latch0, &ns);
        const int us = (int)(ns / 1000);
        if (us > gFlipLatchMaxUs) gFlipLatchMaxUs = us;
        return NV_TRUE;
    }
    OSIncrementAtomic((volatile SInt32 *)&gFlipLatchTimeouts);
    FBLOG("presentation blocked: submitted completion unavailable on head %u", fHead);
    fSubmissionFaulted = true; return NV_FALSE;'''

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
using NvU32=uint32_t; using SInt32=int32_t; using NvBool=bool;
enum { NV_TRUE=1, NV_FALSE=0, NVKMS_KAPI_LAYER_PRIMARY_IDX=0 };
enum NvKmsFlipResult { NV_KMS_FLIP_RESULT_SUCCESS = 0, NV_KMS_FLIP_RESULT_INVALID_PARAMS, NV_KMS_FLIP_RESULT_IN_PROGRESS };
#define FBLOG(...) ((void)0)
static int64_t gClock = 0;   // nanoseconds, the absolute-time unit here
static uint64_t mach_absolute_time() { gClock += 1000; return (uint64_t)gClock; } // +1 us per call
static void IOSleep(unsigned) { gClock += 1000000; }
static SInt32 OSIncrementAtomic(volatile SInt32 *p) { return (*p)++; }
static void absolutetime_to_nanoseconds(uint64_t t, uint64_t *ns) { *ns = t; }
static void nanoseconds_to_absolutetime(uint64_t ns, uint64_t *abs) { *abs = ns; }
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

static char gPresentMsg[8];
#define PRESENT_STATS_LOG() ((void)0)
static bool presentStatsNote(unsigned, uint64_t, bool, bool, bool, uint32_t) { return false; }   // exercised by test_present_stats.py
static int gFlipLatch = 1;
static int gFlipLatchWaits = 0, gFlipLatchTimeouts = 0, gFlipLatchMaxUs = 0, gFlipLatchLate = 0;
static int gFlipLatchDeferred = 0;
STUCK_LINE
enum Mode { COMPLETE, STUCK, FAIL, LATE };
struct Kms {
    Mode mode = COMPLETE; int polls = 0, lateFor = 0;
    bool getFlipPendingStatus(void *, unsigned, int, NvBool *pending) {
        if (mode == FAIL) return false;
        polls++;
        *pending = (mode == STUCK) || (mode == LATE && polls <= lateFor);
        return true;
    }
};
struct NvKmsKapiDisplayModeTimings { uint32_t pixelClockHz=0,hTotal=0,vTotal=0; };
struct Rep { NvKmsFlipResult flipResult = NV_KMS_FLIP_RESULT_SUCCESS; };
struct FB {
    Kms *fKms=nullptr; void *fDev=this; unsigned fHead=0; bool fSubmissionFaulted=false, fSubmissionSeen=false;
    uint64_t fFlipSubmitAbs = 0; unsigned fLatchLateLogs = 0;
    int faultLogs = 0; const char *faultWhy = "";
    void flipFaultLog(const char *why) { faultLogs++; faultWhy = why; }
    enum { kPriorDone = 0, kPriorLate = 1, kPriorStuck = 2 };
    struct { NvKmsKapiDisplayModeTimings timings; } fMode;
    void mode(uint32_t pclk, uint32_t ht, uint32_t vt) { fMode.timings.pixelClockHz = pclk; fMode.timings.hTotal = ht; fMode.timings.vTotal = vt; }
    uint64_t vblPeriodAbs() {
VBL_BODY
    }
    uint64_t flipWaitUs() {
WAIT_US_BODY
    }
    int priorFlipState() {
PRIOR_BODY
    }
WAITER
    // the prior-completion wait, as submitConfig runs it before a new submit
    NvBool priorWait(Rep *rep, bool frame) {
        if (fSubmissionSeen) {
PRIOR_REGION
        return NV_TRUE;
    }
    // the latch wait after an accepted submit
    NvBool waitComplete(bool bootstrap, bool frame) {
        const uint64_t submit0 = mach_absolute_time();
WAIT_REGION
        return NV_TRUE; // control's path when it falls through (never reached in fixed region)
    }
};
static void reset() { gFlipLatchWaits = gFlipLatchTimeouts = gFlipLatchMaxUs = gFlipLatchLate = gFlipLatchDeferred = 0; gFlipLatch = 1; gSleeps = 0; gLongestSleep = 0; }
int main() {
    // Deferred (debug.nvrmfb_flip_latch=2, #300): the submit returns without waiting; the next submit verifies the flip.
    { Kms k; k.mode=COMPLETE; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset(); gFlipLatch = 2;
      assert(f.waitComplete(false, true) == NV_TRUE);
      assert(gFlipLatchDeferred == 1 && gFlipLatchWaits == 0 && gSleeps == 0 && "deferred mode must not wait for the flip"); }
    // A completed flip: one wait recorded, no timeout, a duration captured.
    { Kms k; k.mode=COMPLETE; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      assert(f.waitComplete(false, true) == NV_TRUE);
      assert(gFlipLatchWaits == 1 && "a completed flip must count as one latch wait");
      assert(gFlipLatchTimeouts == 0);
      assert(gFlipLatchMaxUs >= 1 && "the wait duration must be recorded"); }
    // The bound follows the mode's raster and pixel clock, not a constant.
    { FB f;
      f.mode(148500000, 2200, 1125); const uint64_t hz60 = f.flipWaitUs();
      f.mode(405000000, 2200, 1115); const uint64_t hz165 = f.flipWaitUs();
      f.mode(237600000, 4400, 2250); const uint64_t hz24 = f.flipWaitUs();
      f.mode(0, 0, 0);               const uint64_t none = f.flipWaitUs();
      printf("flip wait: 60 Hz %llu us, 165 Hz %llu us, 24 Hz %llu us, no timings %llu us\n",
             (unsigned long long)hz60, (unsigned long long)hz165, (unsigned long long)hz24, (unsigned long long)none);
      assert(hz60 >= 20500 && hz60 <= 21500 && "60 Hz: one period and a quarter is about 20.8 ms");
      assert(hz165 >= 7000 && hz165 <= 8200 && "165 Hz: about 7.6 ms, a 60 Hz constant would hold the frame three periods");
      assert(hz24 >= 50000 && hz24 <= 54000 && "24 Hz: about 52 ms, a 100 ms bound was never the panel's period either");
      assert(none == hz60 && "without timings the 60 Hz default period paces the wait"); }
    // 540 Hz: the bound is about 2.3 ms, not the fallback timer's 2 ms floor (which would make it 20.8 ms).
    { FB f; f.mode(1199880000, 2000, 1111); const uint64_t hz540 = f.flipWaitUs();
      printf("flip wait: 540 Hz %llu us\n", (unsigned long long)hz540);
      assert(hz540 >= 2200 && hz540 <= 2400 && "540 Hz: one period and a quarter is about 2.3 ms"); }
    // A flip that lands well past one period on the frame path: counted late once, and submitConfig returns only after
    // it is on the panel (the surface it replaces may be scanned out until then, and the caller completes the transaction).
    { Kms k; k.mode=LATE; k.lateFor = 200; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      const int64_t t0 = gClock;
      assert(f.waitComplete(false, true) == NV_TRUE && "a late flip is not a fault");
      const int64_t took = gClock - t0;
      printf("late flip on the frame path: returned after %lld us, %d polls (landed at poll %d)\n", (long long)(took / 1000), k.polls, k.lateFor + 1);
      assert(k.polls > k.lateFor && "the frame path returned while its flip was still pending: the old surface may still be on the panel");
      assert(gFlipLatchTimeouts == 1 && !f.fSubmissionFaulted && "a late flip counts once as late");
      assert(f.faultLogs == 0 && "a late flip that lands is not a head fault and leaves no fault line");
      assert(gLongestSleep <= 250000 && "a sleep longer than 250 us between pending checks"); }
    // A flip that never lands faults at the 100 ms stuck limit, on the frame path and on a mode set alike.
    for (int frame = 0; frame < 2; frame++) {
      Kms k; k.mode=STUCK; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      const int64_t t0 = gClock;
      assert(f.waitComplete(false, frame != 0) == NV_FALSE);
      const int64_t took = gClock - t0;
      printf("stuck flip (%s): faulted after %lld us\n", frame ? "frame" : "mode set", (long long)(took / 1000));
      assert(gFlipLatchTimeouts == 1 && f.fSubmissionFaulted);
      assert(f.faultLogs == 1 && f.faultWhy[0] == 's' && "the fault leaves exactly one line, naming the submitted flip");
      assert(took >= 100000000 && took < 125000000 && "a stuck flip faults at the stuck limit, not before"); }
    // A prior flip still landing: this present is skipped as IN_PROGRESS within one period, the head is not faulted.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k; f.mode(405000000, 2200, 1115); reset();
      f.fSubmissionSeen = true; f.fFlipSubmitAbs = mach_absolute_time(); Rep rep;
      const int64_t t0 = gClock;
      assert(f.priorWait(&rep, true) == NV_FALSE);
      const int64_t took = gClock - t0;
      printf("late prior flip at 165 Hz: present skipped after %lld us\n", (long long)(took / 1000));
      assert(rep.flipResult == NV_KMS_FLIP_RESULT_IN_PROGRESS && "a late prior flip answers IN_PROGRESS, not a fault");
      assert(!f.fSubmissionFaulted && gFlipLatchLate == 1);
      assert(took < 9000000 && "WindowServer's present is held at most one period and a quarter"); }
    // A prior flip still pending 100 ms after its submit is a stuck display: the head faults as before.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      f.fSubmissionSeen = true; f.fFlipSubmitAbs = mach_absolute_time() - 150000000ull; Rep rep;
      assert(f.priorWait(&rep, true) == NV_FALSE);
      assert(f.fSubmissionFaulted && gFlipLatchLate == 0 && "a flip pending 150 ms after its submit faults, it is not merely late");
      assert(f.faultLogs == 1 && f.faultWhy[0] == 'p' && "the fault leaves exactly one line, naming the prior flip"); }
    // A prior flip that lands late is waited for by a mode set (no present to skip) and then accepted.
    { Kms k; k.mode=LATE; k.lateFor = 200; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      f.fSubmissionSeen = true; f.fFlipSubmitAbs = mach_absolute_time(); Rep rep;
      assert(f.priorWait(&rep, false) == NV_TRUE && !f.fSubmissionFaulted && "a mode set waits a late flip out");
      assert(gFlipLatchLate == 0 && "a mode set's wait is not a skipped present"); }
    // Knob off: the wait is bypassed and no counter moves.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset(); gFlipLatch=0;
      assert(f.waitComplete(false, true) == NV_TRUE);
      assert(gFlipLatchWaits == 0 && gFlipLatchTimeouts == 0 && !f.fSubmissionFaulted); }
    // Bootstrap commit: no wait, no counters.
    { Kms k; k.mode=STUCK; FB f; f.fKms=&k; f.mode(148500000, 2200, 1125); reset();
      assert(f.waitComplete(true, false) == NV_TRUE);
      assert(gFlipLatchWaits == 0); }
    printf("OK\n");
    return 0;
}
'''


def build_and_run(region_text):
    src = (fixture.replace('WAIT_REGION', region_text).replace('PRIOR_REGION', prior_region)
           .replace('STATICS', statics).replace('WAITER', body).replace('VBL_BODY', vbl_body)
           .replace('WAIT_US_BODY', wait_us_body).replace('PRIOR_BODY', prior_body).replace('STUCK_LINE', stuck_line))
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(src)
        subprocess.run(['c++', '-std=c++17', '-O0', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)


r = build_and_run(region)
assert r.returncode == 0 and r.stdout.strip().endswith('OK'), \
    f'fixed wait failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
for line in r.stdout.strip().splitlines()[:-1]:
    print(line)
print('fixed source: PASS (one-period steps from the mode; returns only once the flip landed; a late prior flip costs a present; 100 ms faults)')

r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect the constant 100 ms hold'
print('negative control (constant 100 ms region): FAILS as required')
r = build_and_run(early_control)
assert r.returncode != 0, 'negative control passed -- the test does not detect a frame completed before its flip landed'
print('negative control (frame let go before its flip landed): FAILS as required')
# The fault line carries what a log reader needs to tell a stuck head from a late one: the time since the submit and the
# latch counters. A line that names only the head cannot say whether the 100 ms limit was reached.
helper = source[source.index('void NVRMNVDAFramebuffer::flipFaultLog'):]
helper = helper[:helper.index('\n}\n')]
for needle in ('us since its submit', 'latch waits %d timeouts %d late %d max %d us', 'gFlipLatchTimeouts', 'gFlipLatchMaxUs'):
    assert needle in helper, f'the head-fault line lost {needle!r}'
assert source.count('presentation blocked') == 1, 'a head fault logs outside flipFaultLog'
print('head-fault line: carries time since submit and the latch counters')
print('PASS')
