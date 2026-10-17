#!/usr/bin/env python3
"""The accelerator's present path, run from the shipped source: every frame completes once, after its flip.

The asynchronous swap worker (nvAccelIopAsyncRun), its starter (nvAccelIopAsyncStart) and the copy path's return to the
home surface (nvAccelIopHome) are lifted from nvrm-accel.cpp and run against a stub framebuffer. Arms:
  - a flip on the panel completes its transaction once, and busy clears only after the last queued frame;
  - WindowServer's present waits at most the framebuffer's published period (NVRMFlipWaitUs), then queues the frame;
    a newer present replaces a queued one (counted as dropped) and the newest reaches the panel;
  - a frame the framebuffer answers busy is kept (counted late), still completes, and is not a refusal;
  - a present from another pipe is never queued behind this head's flip;
  - the period property is read through a retained copy and released;
  - HOME answered busy keeps the head marked flipped so the next copied frame tries again; success or a refusal clears it.
Negative controls (each must fail): HOME clearing the flag unconditionally, and a starter that drops instead of queueing."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()

a0 = source.index('// next/nextF: one frame WindowServer presented')
a1 = source.index('static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe)\n{')
async_region = source[a0:a1]
h0 = source.index('static void nvAccelIopHome(IOService *fb, unsigned head)\n{')
h1 = source.index('\n}\n', h0) + 3
home_region = source[h0:h1]

HOME_BUSY = '    if (rr == kIOReturnBusy) return;\n'
assert home_region.count(HOME_BUSY) == 1
home_control = home_region.replace(HOME_BUSY, '')
QUEUE = 'if (__atomic_load_n(&e->busy, __ATOMIC_ACQUIRE) && gIopAsLock) {'
assert async_region.count(QUEUE) == 1
queue_control = async_region.replace(QUEUE, 'if (false) {')

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
typedef int IOReturn; typedef int32_t SInt32; typedef int64_t SInt64;
enum : int { kIOReturnSuccess = 0, kIOReturnBusy = (int)0xe00002d5, kIOReturnIOError = (int)0xe00002ca, kIOReturnNoMemory = (int)0xe00002bd };
#define ALOG(...) ((void)0)
#define NVRM_FLIP_ABI_VERSION 1
struct NVRMFlipRequest { unsigned int version, flags; void *kapiMemory; unsigned int width, height, pitch; int flipResult; unsigned long long allocationCookie; };
static void nvSVZero(void *p, size_t n) { memset(p, 0, n); }
struct OSObject { int rc = 1; virtual ~OSObject() {} void retain() { rc++; } void release() { rc--; } };
struct OSNumber : OSObject { uint64_t v = 0; uint64_t unsigned64BitValue() const { return v; } };
template <class T> static T *nvCast(OSObject *o) { return dynamic_cast<T *>(o); }
#define OSDynamicCast(T, o) nvCast<T>(o)
struct OSSymbol : OSObject { static const OSSymbol *withCStringNoCopy(const char *) { static OSSymbol s; s.rc = 1000; return &s; }
                             void release() const {} };
struct IOService : OSObject {
    IOReturn answer = kIOReturnSuccess; int calls = 0, homeCalls = 0; OSNumber period; int copies = 0;
    IOReturn callPlatformFunction(const OSSymbol *, bool, void *p1, void *, void *, void *) {
        calls++; NVRMFlipRequest *f = (NVRMFlipRequest *)p1; if (f->flags & 1u) homeCalls++; return answer; }
    OSObject *copyProperty(const char *k) { if (strcmp(k, "NVRMFlipWaitUs")) return nullptr; copies++; period.retain(); return &period; }
    bool metaCast(const char *) { return true; }
};
struct IOSurface : OSObject { unsigned id = 0; unsigned getSurfaceID() const { return id; } };
static bool nvSurfWaitReady(unsigned) { return true; }
static const unsigned kNvMaxFB = 5;
static IOSurface *gIopFlipFront[kNvMaxFB], *gIopFlipPrev[kNvMaxFB];
static bool gIopFlipped[kNvMaxFB];
static unsigned gIopFlipLogs;
static int gIopAsync = 1;
static SInt64 gCntIopFlip, gCntIopAsync, gCntIopLate, gCntIopAsyncRefused, gCntIopFlipNo[8], gCntIopAsyncDrop, gCntIopFlipHome;
static SInt64 OSAddAtomic64(SInt64 v, SInt64 *p) { SInt64 o = *p; *p += v; return o; }
static int64_t gClock = 0;
static uint64_t mach_absolute_time() { gClock += 1000; return (uint64_t)gClock; }
static void IOSleep(unsigned ms) { gClock += (int64_t)ms * 1000000; }
enum { kMicrosecondScale = 1000, kMillisecondScale = 1000000, THREAD_UNINT = 0 };
static void clock_interval_to_deadline(uint64_t v, uint64_t scale, uint64_t *out) { *out = (uint64_t)gClock + v * scale; }
using event_t = void *;
struct IOLock { int held; }; static IOLock gL;
static IOLock *IOLockAlloc() { return &gL; } static void IOLockFree(IOLock *) {}
static bool OSCompareAndSwapPtr(void *o, void *n, void *volatile *p) { if (*p != o) return false; *p = n; return true; }
static void IOLockLock(IOLock *l) { assert(!l->held); l->held = 1; } static void IOLockUnlock(IOLock *l) { assert(l->held); l->held = 0; }
static void IOLockWakeup(IOLock *l, event_t, bool) { assert(l->held); }
static int IOLockSleepDeadline(IOLock *l, event_t, uint64_t deadline, int) { assert(l->held); if ((int64_t)deadline > gClock) gClock = (int64_t)deadline; return 0; }
static int gSignals = 0;
extern "C" void _ZN18IOAccelDisplayPipe26signalTransactionInterruptEPv(void *, void *) { gSignals++; }
struct thread_call { void (*fn)(void *, void *); void *p; int pending; };
static thread_call gCall;
static thread_call *thread_call_allocate(void (*fn)(void *, void *), void *p) { gCall.fn = fn; gCall.p = p; gCall.pending = 0; return &gCall; }
static int thread_call_enter(thread_call *c) { c->pending++; return 0; }
static void runWorker() { assert(gCall.pending == 1); gCall.pending = 0; gCall.fn(gCall.p, nullptr); }
@@ASYNC@@
@@HOME@@
static NVRMFlipRequest req() { NVRMFlipRequest f; nvSVZero(&f, sizeof f); f.version = NVRM_FLIP_ABI_VERSION; f.width = 8; f.height = 8; f.pitch = 32; return f; }
int main() {
    IOService fb; fb.period.v = 7570;   // 165 Hz: one period and a quarter
    int pipe = 0, otherPipe = 0;
    IOSurface A, B, C, D, E; A.id = 1; B.id = 2; C.id = 3; D.id = 4; E.id = 5;
    NVRMFlipRequest f = req();
    // A flip on the panel completes its transaction once; the head is free afterwards.
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &A, &f));
    assert(nvAccelIopAsyncBusy(&pipe) && "the transaction is incomplete while its flip is in flight");
    runWorker();
    assert(gSignals == 1 && !nvAccelIopAsyncBusy(&pipe) && gIopFlipFront[0] == &A && gIopFlipped[0]);
    // B in flight; C and D presented meanwhile: each present waits one period at most, D replaces C, D reaches the panel.
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &B, &f));
    const int64_t t0 = gClock;
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &C, &f));
    const int64_t held = gClock - t0;
    printf("present over a flip in flight held WindowServer %lld us (published bound %llu us)\n", (long long)(held / 1000), (unsigned long long)fb.period.v);
    assert(held >= 7570000 && held < 8000000 && "the wait is the framebuffer's period, not the old 50 ms");
    assert(fb.copies >= 1 && fb.period.rc == 1 && "the period is read through a retained copy that is released");
    assert(C.rc == 2 && gCntIopAsyncDrop == 0 && "C is queued, not dropped");
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &D, &f));
    assert(C.rc == 1 && D.rc == 2 && gCntIopAsyncDrop == 1 && "D replaces the queued C, which is released and counted dropped");
    assert(gCall.pending == 1 && "no second worker is started for a queued frame");
    const int signals0 = gSignals;
    runWorker();
    assert(gSignals == signals0 + 2 && "B and D each complete their transaction once");
    assert(gIopFlipFront[0] == &D && gIopFlipPrev[0] == &B && !nvAccelIopAsyncBusy(&pipe) && "the newest queued frame is on the panel and the head is free");
    // A frame answered busy: kept (counted late), completed, not a refusal.
    fb.answer = kIOReturnBusy;
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &E, &f));
    runWorker();
    assert(gCntIopLate == 1 && gCntIopAsyncRefused == 0 && gIopAsync == 1 && E.rc == 1 && gIopFlipFront[0] == &D);
    assert(gSignals == signals0 + 3 && !nvAccelIopAsyncBusy(&pipe) && "a kept frame still completes its transaction");
    fb.answer = kIOReturnSuccess;
    // A present from another pipe is not queued behind this head's flip.
    assert(nvAccelIopAsyncStart(&pipe, &fb, 0, &A, &f));
    const SInt64 drops = gCntIopAsyncDrop;
    assert(nvAccelIopAsyncStart(&otherPipe, &fb, 0, &C, &f));
    assert(gIopAs[0].next == nullptr && gCntIopAsyncDrop == drops + 1 && C.rc == 1 && "another pipe's frame rode this head's queue");
    runWorker();
    // HOME: busy keeps the head marked flipped (the next copied frame tries again); success or a refusal clears it.
    gIopFlipped[0] = true; fb.answer = kIOReturnBusy; nvAccelIopHome(&fb, 0);
    assert(gIopFlipped[0] && "HOME answered busy was not submitted; clearing the flag strands every later copied frame offscreen");
    fb.answer = kIOReturnSuccess; nvAccelIopHome(&fb, 0);
    assert(!gIopFlipped[0] && "HOME on the panel clears the flag");
    gIopFlipped[0] = true; fb.answer = kIOReturnIOError; nvAccelIopHome(&fb, 0);
    assert(!gIopFlipped[0] && "a refused HOME clears the flag, as before");
    printf("OK\n");
    return 0;
}
'''


def build_and_run(async_text, home_text):
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(fixture.replace('@@ASYNC@@', async_text).replace('@@HOME@@', home_text))
        subprocess.run(['c++', '-std=c++17', '-O0', '-Wno-everything', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)


r = build_and_run(async_region, home_region)
assert r.returncode == 0 and r.stdout.strip().endswith('OK'), f'present path failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print(r.stdout.strip().splitlines()[0])
print('fixed source: PASS (one completion per frame, after its flip; bounded present; queue; busy kept; HOME retried)')
r = build_and_run(async_region, home_control)
assert r.returncode != 0, 'negative control passed -- the test does not detect HOME clearing the flag on busy'
print('negative control (HOME clears the flag on busy): FAILS as required')
r = build_and_run(queue_control, home_region)
assert r.returncode != 0, 'negative control passed -- the test does not detect a frame dropped instead of queued'
print('negative control (starter drops instead of queueing): FAILS as required')
print('PASS')
