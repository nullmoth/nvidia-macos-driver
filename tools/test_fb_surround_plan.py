#!/usr/bin/env python3
"""Surround planning: members share a height and a refresh rate, sit side by side, and are told apart from other framebuffers.

Compiled from nvrm-fb-safety.h. Arms are named. The wiring (member framebuffers decline to start, the master's raster is the
sum, the commit gives each head its slice) is checked in test_fb_surround_wiring.py."""
import pathlib, subprocess, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
fixture = r'''
#include <cassert>
#include <cstdio>
#include "nvrm-fb-safety.h"
static NvrmSurMode M[kNvrmSurMaxMembers][kNvrmSurMaxModes]; static unsigned N[kNvrmSurMaxMembers];
static void add(unsigned m, unsigned w, unsigned h, unsigned hz) { M[m][N[m]++] = {w, h, hz}; }
int main() {
    unsigned pick[4], tw, h, hz;
    // three 2560x1440 monitors that all run 144 and 60 Hz: 7680x1440 at 144
    for (unsigned m = 0; m < 3; m++) { add(m, 2560, 1440, 144); add(m, 2560, 1440, 60); add(m, 1920, 1080, 60); }
    assert(nvrmSurroundPlan(M, N, 3, pick, &tw, &h, &hz) && tw == 7680 && h == 1440 && hz == 144);
    // one monitor tops out at 120 Hz: the group runs 120? no - 144 is not shared, so 60
    for (unsigned m = 0; m < 4; m++) N[m] = 0;
    add(0, 2560, 1440, 144); add(0, 2560, 1440, 60); add(1, 2560, 1440, 120); add(1, 2560, 1440, 60);
    assert(nvrmSurroundPlan(M, N, 2, pick, &tw, &h, &hz) && tw == 5120 && h == 1440 && hz == 60);
    // 1 Hz tolerance (59.94 vs 60)
    for (unsigned m = 0; m < 4; m++) N[m] = 0;
    add(0, 3840, 2160, 60); add(1, 3840, 2160, 59);
    assert(nvrmSurroundPlan(M, N, 2, pick, &tw, &h, &hz) && tw == 7680 && h == 2160);
    // mixed widths at a shared height: each member takes its widest mode there
    for (unsigned m = 0; m < 4; m++) N[m] = 0;
    add(0, 2560, 1440, 60); add(0, 1920, 1440, 60); add(1, 3440, 1440, 60);
    assert(nvrmSurroundPlan(M, N, 2, pick, &tw, &h, &hz) && tw == 2560 + 3440 && M[0][pick[0]].w == 2560 && M[1][pick[1]].w == 3440);
    unsigned widths[2] = {2560, 3440}; assert(nvrmSurroundOffset(widths, 0) == 0 && nvrmSurroundOffset(widths, 1) == 2560);
    // nothing shared: refused, never a guess
    for (unsigned m = 0; m < 4; m++) N[m] = 0;
    add(0, 2560, 1440, 60); add(1, 1920, 1080, 60);
    assert(!nvrmSurroundPlan(M, N, 2, pick, &tw, &h, &hz));
    assert(!nvrmSurroundPlan(M, N, 1, pick, &tw, &h, &hz) && !nvrmSurroundPlan(M, N, 5, pick, &tw, &h, &hz));   // needs 2..4 members
    // members from the mask: indices 0 and 2 of three connected displays; a bit above the connected count is ignored
    unsigned idx[4]; assert(nvrmSurroundMembers(0x5, 3, idx) == 2 && idx[0] == 0 && idx[1] == 2);
    assert(nvrmSurroundMembers(0x6, 2, idx) == 1 && idx[0] == 1);       // only one member left: the group will not form
    assert(nvrmSurroundMembers(0, 4, idx) == 0);
    // master and members by index
    assert(nvrmSurroundMaster(0x5, 3) == 0 && nvrmSurroundMaster(0x6, 3) == 1 && nvrmSurroundMaster(0x6, 1) == 8);
    assert(!nvrmSurroundIsMember(0x5, 0, 3) && nvrmSurroundIsMember(0x5, 2, 3) && !nvrmSurroundIsMember(0x5, 1, 3));   // index 1 is not in the mask
    assert(!nvrmSurroundIsMember(0x3, 1, 1));                                         // only one display connected: no group, no member
    assert(nvrmSurroundIsMember(0x7, 1, 3) && nvrmSurroundIsMember(0x7, 2, 3) && !nvrmSurroundIsMember(0x7, 0, 3));
    // hot-plug: a member leaves a formed group -> degrade; all return -> re-form; nothing else acts
    assert(nvrmSurroundNext(true, false, 3, 3) == kSurNone);
    assert(nvrmSurroundNext(true, false, 3, 2) == kSurDegrade);                        // one of three unplugged
    assert(nvrmSurroundNext(false, true, 3, 2) == kSurNone);                           // still short: stay separate
    assert(nvrmSurroundNext(false, true, 3, 3) == kSurReform);                         // all back
    assert(nvrmSurroundNext(false, true, 1, 1) == kSurNone && nvrmSurroundNext(false, false, 3, 3) == kSurNone);   // no group configured
    puts("PASS: surround picks a shared height and refresh rate, widest mode per member, offsets by width, refuses when nothing is shared");
}
'''
with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d); (d/'t.cpp').write_text(fixture)
    subprocess.run(['clang++','-std=c++17','-Wall','-Werror',f'-I{root}/kexts/NVRMFB/fb',str(d/'t.cpp'),'-o',str(d/'t')],check=True)
    subprocess.run([str(d/'t')],check=True)
