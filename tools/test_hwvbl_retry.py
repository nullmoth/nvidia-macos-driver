#!/usr/bin/env python3
"""A refused hardware-vblank registration is tried again, a bounded number of times.

When NVKMS refuses the vblank callback, WindowServer's VBL is the free-running fallback timer: the right rate, but not
locked to the panel's scan-out, so frames beat against it. One try at 60 mode ticks left such a display on the timer
for the whole session. hwVblRetryDue is lifted from nvrm-fb.cpp: due every 300 ticks, a dozen tries, then final. The
negative control is the single-shot rule and fails the retry arm."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()

k0 = source.index('static const unsigned kHwVblRetryTicks')
k1 = source.index('\n}\n', source.index('static bool hwVblRetryDue(')) + 3
lifted = source[k0:k1]
assert 'fHwVblTries == 0 || hwVblRetryDue(' in source, 'vblFire no longer retries a refused registration'
assert 'if (me->fModeSetTicks == 0) me->fHwVblTries = 0;' in source, 'losing the mode no longer starts the tries over'


def body_of(signature):
    start = source.index(signature)
    i = source.index('{', start) + 1; depth = 1
    while depth:
        depth += {'{': 1, '}': -1}.get(source[i], 0); i += 1
    return source[start:i]


# switchMode keeps fModeSet set, so the tick reset above never fires on a resolution or refresh change
assert 'fHwVblTries = 0;' in body_of('IOReturn NVRMNVDAFramebuffer::switchMode('), 'a mode switch does not start the tries over'
assert 'fHwVblTries = 0;' in body_of('bool NVRMNVDAFramebuffer::applyMode()'), 'a mode set does not start the tries over'
# a retry can register after stop()'s first unregister; stop() unregisters again once the timer is drained
stop = body_of('void NVRMNVDAFramebuffer::stop(IOService *provider)')
drain = stop.index('*calls[i] = nullptr;')
assert stop.count('unregisterVblankIntrCallback') == 2 and stop.rindex('unregisterVblankIntrCallback') > drain, \
    'a callback registered by a retry racing stop() survives teardown'

control = r'''
static const unsigned kHwVblRetryTicks = 300, kHwVblMaxTries = 12;
static bool hwVblRetryDue(unsigned, unsigned) { return false; }
'''

fixture = r'''
#include <cassert>
#include <cstdio>
LIFTED
int main() {
    assert(!hwVblRetryDue(1, 0) && "right after a refusal nothing is due");
    assert(!hwVblRetryDue(1, 299) && "a retry before 300 ticks would hammer NVKMS every tick");
    assert(hwVblRetryDue(1, 300) && "a refused registration is tried again after 300 ticks");
    assert(hwVblRetryDue(11, 100000) && "the twelfth try is still made");
    assert(!hwVblRetryDue(12, 100000) && "after a dozen refusals the timer stays the VBL for good");
    unsigned tries = 1, lastTick = 60, made = 0;
    for (unsigned tick = 61; tick < 20000; tick++)
        if (hwVblRetryDue(tries, tick - lastTick)) { tries++; lastTick = tick; made++; }
    printf("retries over 20000 ticks: %u (last at tick %u)\n", made, lastTick);
    assert(made == 11 && "the retries are bounded");
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
assert r.returncode == 0 and r.stdout.strip().endswith('OK'), f'retry rule failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print(r.stdout.strip().splitlines()[0])
print('fixed source: PASS (retry after 300 ticks, a dozen tries, then final)')
r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect a single-shot registration'
print('negative control (single try): FAILS as required')
print('PASS')
