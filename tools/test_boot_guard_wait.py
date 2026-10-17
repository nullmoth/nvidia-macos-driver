#!/usr/bin/env python3
"""The boot guard counts starts that never lit a display; a reset during the driver's own start-up wait is not one.

Two starts in a row without a lit head switch the driver off for the next boot. The 10 s Turing settle and the 100 s
small-BAR1 wait show the boot logo for that long, and a Mac reset twice during that wait was switched off although it
works. The guard now takes the count back for the wait and takes it again before arming, so a hang before the wait
(RM never came up) and a dark screen after it (the head never lit) still count. Only the boot's own written increment
is taken back: an increment that could not be written, a pause that could not be written, a second worker, or a display
brought up by hand during the wait must not erase an earlier strike or add one. The guard's own functions are lifted
from nvrm_off.h and run against a fake NVRAM; the negative control is the guard without the wait exemption, which must
switch a working Mac off after two resets."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'kexts/NVRM/nvrm_off.h').read_text()
body = src[src.index('#define NVRM_GUARD_VAR'):]
for fn in ('nvrm_guard_count_start', 'nvrm_guard_pause_for_wait', 'nvrm_guard_resume_after_wait', 'nvrm_guard_display_up', 'nvrm_driver_off'):
    assert fn + '(' in body, fn

fixture = r'''
#include <cassert>
#include <cstdio>
#include <cstring>
#include <map>
#include <string>
#include <vector>
typedef int SInt32;
// ---- a fake NVRAM: the calls the guard makes, over a map that survives "reboots"; two knobs model a registry that
// is not published yet (fromPath returns NULL) and a write that fails ----
static bool gOptionsGone, gWritesFail;
struct OSObject { virtual ~OSObject() {} void release() { delete this; } };
struct OSData : OSObject {
    std::vector<unsigned char> b;
    static OSData *withBytes(const void *p, unsigned n) { OSData *d = new OSData; d->b.assign((const unsigned char *)p, (const unsigned char *)p + n); return d; }
    unsigned getLength() const { return (unsigned)b.size(); }
    const void *getBytesNoCopy() const { return b.data(); }
};
template <class T> T *OSDynamicCast_impl(OSObject *o) { return dynamic_cast<T *>(o); }
#define OSDynamicCast(T, o) OSDynamicCast_impl<T>(o)
static std::map<std::string, std::vector<unsigned char>> gNvram;
struct IORegistryEntry : OSObject {
    static IORegistryEntry *fromPath(const char *, int) { return gOptionsGone ? nullptr : new IORegistryEntry; }
    OSObject *copyProperty(const char *k) { auto it = gNvram.find(k); return it == gNvram.end() ? nullptr : OSData::withBytes(it->second.data(), (unsigned)it->second.size()); }
    bool setProperty(const char *k, OSData *d) { if (gWritesFail) return false; gNvram[k] = d->b; return true; }
    void removeProperty(const char *k) { if (!gWritesFail) gNvram.erase(k); }
};
static int gIODTPlane = 0;
typedef void *thread_t;
static thread_t gThread = (thread_t)1;                     // which auto-go worker is running
static thread_t current_thread() { return gThread; }
static bool PE_parse_boot_argn(const char *, void *, unsigned) { return false; }
static int IOLog(const char *, ...) { return 0; }
static bool OSCompareAndSwap(SInt32 oldv, SInt32 newv, volatile SInt32 *p) { if (*p != oldv) return false; *p = newv; return true; }
''' + body + r'''
static unsigned starts() { auto it = gNvram.find(NVRM_GUARD_VAR); return it == gNvram.end() ? 0 : it->second[0]; }
// ---- one boot per process: the scenario word says what this boot did; NVRAM comes in on argv[2] and goes out on stdout
int main(int argc, char **argv) {
    const std::string sc = argc > 1 ? argv[1] : "";
    if (argc > 2) for (const char *p = argv[2]; *p;) { const char *e = strchr(p, '='); const char *c = strchr(p, ','); if (!c) c = p + strlen(p);
        std::string k(p, e), v(e + 1, c); gNvram[k] = std::vector<unsigned char>(v.begin(), v.end()); p = *c ? c + 1 : c; }
    const bool off = nvrm_driver_off();
    if (!off) {
        gOptionsGone = sc.find("nooptions-at-start") != std::string::npos;      // registry not published when NVRM started
        nvrm_guard_count_start();
        gOptionsGone = false;
        const bool wait = sc.find("wait") != std::string::npos;
        if (wait) {
            gWritesFail = sc.find("pausefail") != std::string::npos;             // the pause could not be written
            nvrm_guard_pause_for_wait(100000);
            gWritesFail = false;
            const bool two = sc.find("twoworkers") != std::string::npos;         // a second card's worker, another thread
            if (two) { gThread = (thread_t)2; nvrm_guard_pause_for_wait(100000); gThread = (thread_t)1; }
            if (sc.find("reset-in-wait") != std::string::npos) goto out;          // the user reset the Mac during the wait
            const bool byhand = sc.find("byhand") != std::string::npos;          // a hand-run go lit the display during the wait
            if (byhand) nvrm_guard_display_up("by hand");
            // the other worker's settle may end first: its resume must not put back a count it never took
            if (two) { gThread = (thread_t)2; nvrm_guard_resume_after_wait(byhand); gThread = (thread_t)1; }
            nvrm_guard_resume_after_wait(byhand);
            if (byhand) goto out;
        }
        if (sc.find("dark") != std::string::npos) goto out;                      // armed, the head never lit
        if (sc.find("hang") != std::string::npos) goto out;                      // never reached the wait
        nvrm_guard_display_up("test");
    }
out:
    printf("off=%d starts=%u ok=%d\n", off, starts(), gNvram.count(NVRM_GUARD_OK_VAR) ? 1 : 0);
    for (auto &kv : gNvram) { printf("NV %s=", kv.first.c_str()); for (unsigned char c : kv.second) printf("%c", c); printf("\n"); }
    return 0;
}
'''


def build(h):
    d = tempfile.mkdtemp()
    s = pathlib.Path(d, 'g.cpp'); s.write_text(fixture.replace(body, h))
    exe = pathlib.Path(d, 'g')
    r = subprocess.run(['clang++', '-std=c++17', '-Wno-unused-function', '-o', str(exe), str(s)], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return exe


def boots(exe, scenarios):
    """Run the scenarios as consecutive boots; return (off, starts, ok) per boot."""
    nv, out = '', []
    for sc in scenarios:
        r = subprocess.run([str(exe), sc, nv], capture_output=True, text=True, check=True)
        first = r.stdout.splitlines()[0].split()
        out.append(tuple(int(f.split('=')[1]) for f in first))
        nv = ','.join(l[3:] for l in r.stdout.splitlines() if l.startswith('NV '))
    return out


fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


exe = build(body)
r = boots(exe, ['good', 'good'])
check('a start that lights its display leaves no count', [x[:2] for x in r] == [(0, 0), (0, 0)], r)
r = boots(exe, ['hang', 'hang', 'good'])
check('two starts that hang before the wait switch the driver off on the third boot', r[2][0] == 1, r)
r = boots(exe, ['wait-dark', 'wait-dark', 'good'])
check('two starts that arm and never light a head switch the driver off', r[2][0] == 1, r)
r = boots(exe, ['wait-reset-in-wait', 'wait-reset-in-wait', 'wait-reset-in-wait', 'wait'])
check('three resets during the start-up wait never switch a working Mac off', [x[0] for x in r] == [0, 0, 0, 0], r)
check('...and the count is 0 while the wait runs (a reset there leaves nothing behind)', r[0][1] == 0, r)
r = boots(exe, ['wait-reset-in-wait', 'hang', 'wait-reset-in-wait', 'hang', 'good'])
check('a reset in the wait does not erase a real hang: two hangs still trip the guard', r[4][0] == 1, r)
r = boots(exe, ['wait'])
check('after the wait the start is counted again, then cleared by the lit display', r[0][:2] == (0, 0), r)
# the four ways the exemption could lie, each from the review of the first version
r = boots(exe, ['hang', 'wait-nooptions-at-start-reset-in-wait', 'good'])
check('an increment that was never written is not taken back: the earlier strike survives a reset in the wait', r[2][1] == 1 or r[2][0] == 0 and r[1][1] == 1, r)
r = boots(exe, ['wait-pausefail-dark', 'good'])
check('a pause that could not be written does not count the start twice', r[0][1] == 1 and r[1][0] == 0, r)
r = boots(exe, ['wait-twoworkers-dark', 'good'])
check('two auto-go workers (two cards): the other worker neither pauses nor resumes the first one\'s pause', r[0][1] == 1 and r[1][0] == 0, r)
r = boots(exe, ['wait-twoworkers-dark', 'wait-twoworkers-dark', 'good'])
check('...and two dark boots with two workers still trip the guard (no count lost to the other worker)', r[2][0] == 1, r)
r = boots(exe, ['wait-byhand', 'good'])
check('a display brought up by hand during the wait leaves no strike behind', r[0][1] == 0 and r[0][2] == 1 and r[1][0] == 0, r)
# the accelerator's 60 s headless fallback is NOT a good start: a card whose connectors carry no display clears the guard
# in the framebuffer's enumeration (connector list as evidence); reaching the deadline without it means the framebuffer
# never enumerated, which the guard must keep counting
accel = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
h0 = accel.index('static void headlessCheck(')
headless = accel[h0:accel.index('void deliverModeChange()', h0)]
check('the headless fallback does not clear the guard', 'nvrm_guard_display_up(' not in headless)
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
check('the framebuffer clears it on a connector list with no display', 'if (n > 0 && gNDpy == 0) fbGuardCleared(' in fb)
# negative control: the guard without the wait exemption switches the Mac off after two resets during the wait
control = body.replace('nvrm_guard_pause_for_wait(unsigned ms)\n{', 'nvrm_guard_pause_for_wait(unsigned ms)\n{ (void)ms; return;')
assert control != body
r = boots(build(control), ['wait-reset-in-wait', 'wait-reset-in-wait', 'wait'])
check('negative control: without the exemption two resets in the wait switch the driver off', r[2][0] == 1, r)
print(f'boot guard wait: {15 - fails} of 15 arms')
raise SystemExit(1 if fails else 0)
