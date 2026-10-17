#!/usr/bin/env python3
"""A flip the framebuffer answers busy keeps the frame; it is not a refusal.

The framebuffer answers kIOReturnBusy when the previous flip is still on its way to the panel (one period late). Such
a frame stays off the panel and the next present follows the flip. Counting it as a refusal would copy the frame over
a flip in flight and, three in a row, switch the asynchronous swap off for the session. nvAccelFlipOutcome is lifted
from nvrm-accel.cpp; the source contract checks that the kept branch of the asynchronous worker leaves the refusal run
alone and that the accelerator's wait for a flip in flight is bounded by the framebuffer's period, not a constant. The
negative control is the two-way rule (success or refused) and fails the busy arm."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()

o0 = source.index('enum { kNvFlipShown = 0')
o1 = source.index('\n}\n', source.index('static int nvAccelFlipOutcome(')) + 3
lifted = source[o0:o1]

# the worker's kept branch neither counts a refusal nor touches the refusal run
k0 = source.index('} else if (outcome == kNvFlipKept) {')
k1 = source.index('} else {', k0)
kept = source[k0:k1]
assert 'gIopAsyncRefuseRun' not in kept and 'gCntIopAsyncRefused' not in kept, 'a busy flip counts as a refusal again'
assert 'gCntIopLate' in kept, 'a kept frame is no longer counted'
assert 'sysctl_register_oid(&sysctl__debug_nvaccel_iop_late);' in source, 'the kept-frame counter is defined but never registered as a sysctl'
# the synchronous path keeps the frame too, before the refusal branch copies it
s0 = source.index('static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe)\n{')
sync = source[s0:source.index('static int nvaccel_crc_sysctl SYSCTL_HANDLER_ARGS\n', s0)]
assert sync.index('nvAccelFlipOutcome(rr) == kNvFlipKept') < sync.index('flip REFUSED'), 'the synchronous path copies a busy flip'
# the wait for a flip in flight is bounded by the framebuffer's published period
start = source.index('static bool nvAccelIopAsyncStart(')
assert 'nvAccelFlipWaitUs(fb)' in source[start:s0], 'the accelerator waits a constant for a flip in flight again'
assert 'copyProperty("NVRMFlipWaitUs")' in source, 'the period is not read from the framebuffer (as a retained copy)'
# past the bound the frame is queued for the worker, not dropped, and the worker takes the queued frame before it
# clears busy (otherwise a present could start a second worker on the same head)
starter = source[start:s0]
assert 'e->next = s;' in starter and starter.index('e->next = s;') < starter.index('OSAddAtomic64(1, &gCntIopAsyncDrop);\n        return true;'), \
    'a frame presented while the previous flip is still in flight past the bound is dropped instead of queued'
worker = source[source.index('static void nvAccelIopAsyncRun('):start]
assert worker.index('queued = e->next; e->next = nullptr;') < worker.index('__atomic_store_n(&e->busy, 0, __ATOMIC_RELEASE); IOLockWakeup'), \
    'the worker frees the head before it has taken the queued frame'
assert 'if (queued) { s = queued; goto again; }' in worker, 'the queued frame never reaches the panel'
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
assert fb.count('publishFlipWait();') >= 2 and 'setProperty("NVRMFlipWaitUs"' in fb, 'the framebuffer does not publish its period'

control = r'''
enum { kNvFlipShown = 0, kNvFlipKept = 1, kNvFlipRefused = 2 };
static int nvAccelFlipOutcome(IOReturn rr) { return rr == kIOReturnSuccess ? kNvFlipShown : kNvFlipRefused; }
'''

fixture = r'''
#include <cassert>
#include <cstdio>
typedef int IOReturn;
enum { kIOReturnSuccess = 0, kIOReturnBusy = 0xe00002d5, kIOReturnIOError = 0xe00002c0, kIOReturnNotReady = 0xe00002d4 };
LIFTED
int main() {
    assert(nvAccelFlipOutcome(kIOReturnSuccess) == kNvFlipShown && "a flip NVKMS took is on the panel");
    assert(nvAccelFlipOutcome(kIOReturnBusy) == kNvFlipKept && "busy keeps the frame: the previous flip is still landing");
    assert(nvAccelFlipOutcome(kIOReturnIOError) == kNvFlipRefused && "an I/O error is a refusal (copy)");
    assert(nvAccelFlipOutcome(kIOReturnNotReady) == kNvFlipRefused && "a faulted head is a refusal (copy)");
    printf("OK\n");
    return 0;
}
'''


def build_and_run(text):
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'; binp = pathlib.Path(d) / 'tc'
        cpp.write_text(fixture.replace('LIFTED', text))
        subprocess.run(['c++', '-std=c++17', '-O0', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)


r = build_and_run(lifted)
assert r.returncode == 0 and r.stdout.strip().endswith('OK'), f'outcome rule failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print('fixed source: PASS (busy keeps the frame; refusal run untouched; wait bounded by the framebuffer period)')
r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect busy counted as a refusal'
print('negative control (two-way rule): FAILS as required')
print('PASS')
