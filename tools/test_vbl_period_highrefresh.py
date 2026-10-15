#!/usr/bin/env python3
"""The fallback vblank period must reach the real rate of a high-refresh panel.

When NVKMS refuses the hardware vblank, vblPeriodAbs() paces the software timer that delivers vblanks
to WindowServer. The accepted range has to cover real panels up to 500 Hz; a 250 Hz floor dropped
280/300/360 Hz panels to the 60 Hz default, so WindowServer's frame delivery beat against the
scan-out. The real vblPeriodAbs() body is lifted from nvrm-fb.cpp and exercised; the negative control
restores the old 4 ms floor and proves a 300 Hz panel then reports a 60 Hz period."""
import pathlib
import re
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()

start = source.index('uint64_t NVRMNVDAFramebuffer::vblPeriodAbs()')
body_open = source.index('{', start) + 1
# Lift the function body up to its closing brace (single-level, no nested braces inside).
depth = 1
i = body_open
while depth:
    if source[i] == '{': depth += 1
    elif source[i] == '}': depth -= 1
    i += 1
body = source[body_open:i-1]

assert 'q >= 2000000ull' in body, 'the fallback vblank floor no longer covers high-refresh panels (>250 Hz)'

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
struct NvKmsKapiDisplayModeTimings { uint32_t pixelClockHz=0,hTotal=0,vTotal=0; };
// On x86 the mach timebase is 1:1 with ns, so model the conversion as identity for the test.
static void nanoseconds_to_absolutetime(uint64_t ns, uint64_t *abs) { *abs = ns; }
struct FB {
    struct { NvKmsKapiDisplayModeTimings timings; } fMode;
    uint64_t vblPeriodAbs() {
VBL_BODY
    }
};
static FB at(uint32_t w, uint32_t h, uint32_t hz) {
    FB fb; auto &t = fb.fMode.timings;
    t.hTotal = w + 160; t.vTotal = h + 90;
    t.pixelClockHz = (uint32_t)((uint64_t)t.hTotal * t.vTotal * hz); // q == 1e9/hz exactly
    return fb;
}
static uint64_t nsFor(uint32_t hz) { return 1000000000ull / hz; }
int main() {
    // Real high-refresh panels report their own rate, not the 60 Hz default.
    assert(at(2560,1440,300).vblPeriodAbs() == nsFor(300));
    assert(at(2560,1440,280).vblPeriodAbs() == nsFor(280));
    assert(at(1920,1080,360).vblPeriodAbs() == nsFor(360));
    // Rates that already worked are unchanged.
    assert(at(2560,1440,240).vblPeriodAbs() == nsFor(240));
    assert(at(2560,1440,165).vblPeriodAbs() == nsFor(165));
    assert(at(1920,1080,60).vblPeriodAbs()  == nsFor(60));
    // Out of the plausible panel range still falls back to 60 Hz (16666667 ns).
    assert(at(1920,1080,600).vblPeriodAbs() == 16666667ull); // sub-2 ms: implausible clock
    assert(at(1920,1080,10).vblPeriodAbs()  == 16666667ull); // over 50 ms
    { FB z; assert(z.vblPeriodAbs() == 16666667ull); }       // no mode yet
    printf("OK\n");
    return 0;
}
'''

def build_and_run(vbl_body):
    src = fixture.replace('VBL_BODY', vbl_body)
    with tempfile.TemporaryDirectory() as d:
        cpp = pathlib.Path(d) / 'tc.cpp'
        binp = pathlib.Path(d) / 'tc'
        cpp.write_text(src)
        subprocess.run(['c++', '-std=c++17', '-O0', str(cpp), '-o', str(binp)], check=True)
        return subprocess.run([str(binp)], capture_output=True, text=True)

r = build_and_run(body)
assert r.returncode == 0 and r.stdout.strip() == 'OK', \
    f'fixed vblPeriodAbs failed: rc={r.returncode} out={r.stdout!r} err={r.stderr!r}'
print('fixed source: PASS (280/300/360 Hz report their real period; 240/165/60 unchanged)')

control = body.replace('q >= 2000000ull', 'q >= 4000000ull')
assert control != body, 'could not build the negative control'
r = build_and_run(control)
assert r.returncode != 0, 'negative control passed -- the test does not detect the 250 Hz floor bug'
print('negative control (4 ms / 250 Hz floor): FAILS as required (300 Hz fell back to 60 Hz)')
print('PASS')
