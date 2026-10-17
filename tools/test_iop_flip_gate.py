#!/usr/bin/env python3
"""The display pipe must not scan out, or page off, a VRAM copy the plugin never wrote.

A zero-copy flip presents the surface's VRAM copy whenever the family's "VRAM newest" bit is set. That bit
can be set on a copy nothing rendered into (a fresh, zero-filled allocation), and the copied-frame path
pages such a copy off over the system-memory pixels the plugin had copied out. Either way the panel shows
black while the real frame exists. The two decisions are lifted from nvrm-accel.cpp and exercised in both
directions; the negative control is the pre-fix decision (the family bit alone) and must flip the
unwritten copy."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
accel = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
stubs = (root / 'kexts/NVRM/accel/nvaccel_stubs.h').read_text()

def lift(name):
    i = accel.index('static inline bool %s(' % name)
    return accel[i:accel.index('\n', i) + 1]

helpers = lift('nvAccelIopVramFlippable') + lift('nvAccelIopCopyNeedsPageoff')

helpers += lift('nvAccelPluginWrote')

def body(signature):
    # the definition, not the forward declaration: the signature line followed by the opening brace
    i = accel.index(signature + '\n{')
    return accel[i:accel.index('\n}\n', i)]

# the record exists on the resource and the dirty escape writes the allocation cookie into it
assert 'unsigned long long fPluginWroteCookie = 0;' in stubs, 'NVResource lost fPluginWroteCookie'
dirty = body('static unsigned nvAccelSurfDirty(void *cache, NVResource *res, unsigned plane)')
assert 'res->fPluginWroteCookie = vm ? vm->fAllocationCookie : 0;' in dirty, 'the surface-dirty escape no longer records the plugin write'
# both call sites consult the helpers through the cookie comparison, not the raw family bit
flip = body('static bool nvAccelIopFlip(IOService *fb, unsigned head, IOSurface *s, void *pipe)')
assert 'nvAccelIopVramFlippable(vramNewest, nvAccelPluginWrote(res->fPluginWroteCookie, vm->fAllocationCookie))' in flip, \
    'the zero-copy flip no longer checks the plugin write against the allocation it submits'
copy = body('static bool nvAccelSurfCopyReady(IOSurface *s, unsigned head)')
assert 'nvAccelIopCopyNeedsPageoff(false, nvAccelResPluginWrote(res))' in copy, 'the copied-frame path no longer checks the plugin write before paging off'

control = '''
static inline bool nvAccelIopVramFlippable(bool vramNewest, bool) { return vramNewest; }
static inline bool nvAccelIopCopyNeedsPageoff(bool sysmemCurrent, bool) { return !sysmemCurrent; }
static inline bool nvAccelPluginWrote(unsigned long long wroteCookie, unsigned long long) { return wroteCookie != 0; }
'''

fixture = r'''
#include <cstdio>
%s
int main() {
    int bad = 0;
    // flip: the family bit alone must not flip an unwritten copy
    if (nvAccelIopVramFlippable(true, false))  { puts("FAIL an unwritten VRAM copy marked newest was flipped"); bad++; }
    if (!nvAccelIopVramFlippable(true, true))  { puts("FAIL a written VRAM copy marked newest was not flipped"); bad++; }
    if (nvAccelIopVramFlippable(false, true))  { puts("FAIL a written copy that is not newest was flipped"); bad++; }
    // copy: an unwritten copy is read from system memory without a page-off
    if (nvAccelIopCopyNeedsPageoff(false, false)) { puts("FAIL a stale system copy was paged off from VRAM the plugin never wrote"); bad++; }
    if (!nvAccelIopCopyNeedsPageoff(false, true)) { puts("FAIL a stale system copy with a written VRAM copy was not paged off"); bad++; }
    if (nvAccelIopCopyNeedsPageoff(true, true))   { puts("FAIL a current system copy was paged off"); bad++; }
    // the record follows the allocation: VRAM re-allocated under the resource (new cookie) counts as unwritten again
    if (nvAccelPluginWrote(7, 9))   { puts("FAIL a write recorded on an earlier allocation authorised a fresh one"); bad++; }
    if (!nvAccelPluginWrote(7, 7))  { puts("FAIL a write on the current allocation was not recognised"); bad++; }
    if (nvAccelPluginWrote(0, 0))   { puts("FAIL a resource with no VRAM at all counted as written"); bad++; }
    return bad;
}
'''

def run(code, label):
    with tempfile.TemporaryDirectory() as d:
        src = pathlib.Path(d) / 'gate.cpp'
        src.write_text(fixture % code)
        exe = pathlib.Path(d) / 'gate'
        subprocess.run(['clang++', '-std=c++17', '-o', str(exe), str(src)], check=True)
        r = subprocess.run([str(exe)], capture_output=True, text=True)
        print('%s: rc %d%s' % (label, r.returncode, (' ' + r.stdout.strip().replace('\n', ' | ')) if r.stdout.strip() else ''))
        return r.returncode

ok = run(helpers, 'shipped gate (unwritten VRAM is never flipped or paged off)')
assert ok == 0, 'the shipped gate failed an arm'
bad = run(control, 'control: the family bit alone')
assert bad > 0, 'the control did not fire: the test cannot see the difference the fix makes'
print('ok: zero-copy flips and page-offs require a plugin write; the pre-fix gate flips an unwritten copy (%d arm(s) fired)' % bad)
