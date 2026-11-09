#!/usr/bin/env python3
"""Power-off and restart: the plan that hands the BAR layout back, and the wiring of the shutdown handler."""
import os, shutil, subprocess, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[1]
nvrm = (root / "kexts/NVRM/NVRM.cpp").read_text(encoding="utf-8")
failures = []

def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        failures.append(name)

header = root / "kexts/NVRM/nvrm_shutdown_plan.h"
arm("the shutdown plan header exists", header.exists())
if header.exists():
    harness = r'''
#include <cstdio>
#include "nvrm_shutdown_plan.h"
#define CHECK(name, cond) do { std::printf("%s %s\n", (cond) ? "ok  " : "FAIL", name); if (!(cond)) bad++; } while (0)
static NvrmBarUndo placed() {
    NvrmBarUndo u = {}; u.valid = true; u.cmd = 0x0407; u.resized = true; u.rbCtlOff = 0x254; u.rbCtlOld = 0x00000800;
    u.lo14 = 0xd000000c; u.hi18 = 0; u.lo1c = 0xe000000c; u.hi20 = 0; u.rp24 = 0xe001d001; u.rp28 = 0; u.rp2c = 0; return u;
}
static int indexOf(const NvrmStep *s, unsigned n, int kind, unsigned off, unsigned from = 0) {
    for (unsigned i = from; i < n; i++) if (s[i].kind == kind && s[i].off == off) return (int)i;
    return -1;
}
int main() {
    int bad = 0; NvrmStep s[16];
    NvrmBarUndo none = {};
    CHECK("a BAR layout that was never changed needs no restore", nvrmBuildShutdownPlan(&none, s, 16) == 0);
    CHECK("a null undo record is refused", nvrmBuildShutdownPlan(nullptr, s, 16) == 0);
    NvrmBarUndo u = placed();
    unsigned n = nvrmBuildShutdownPlan(&u, s, 16);
    CHECK("a placed layout produces a plan", n > 0 && n <= 16);
    CHECK("the first step turns memory decode and bus mastering off", s[0].kind == kStepCmd16 && (s[0].value & 0x6) == 0);
    int rb = indexOf(s, n, kStepExt32, 0x254), b1 = indexOf(s, n, kStepCfg32, 0x14);
    CHECK("the Resizable BAR size is put back after decode is off and before the BARs", rb > 0 && rb < b1 && s[rb].value == 0x800);
    CHECK("BAR1 and BAR3 return to the firmware's values",
          s[indexOf(s, n, kStepCfg32, 0x14)].value == 0xd000000c && s[indexOf(s, n, kStepCfg32, 0x1c)].value == 0xe000000c);
    int close_ = indexOf(s, n, kStepRootPort32, 0x24), open_ = indexOf(s, n, kStepRootPort32, 0x24, close_ + 1);
    int up = indexOf(s, n, kStepRootPort32, 0x28);
    CHECK("the root port window is closed, its upper halves written, then reopened at the old range",
          close_ >= 0 && s[close_].value == 0x0000fff0 && up > close_ && open_ > up && s[open_].value == 0xe001d001);
    CHECK("the last step leaves the firmware's decode setting and no bus mastering", s[n - 1].kind == kStepCmd16 && s[n - 1].value == 0x0403);
    NvrmBarUndo u2 = placed(); u2.resized = false;
    n = nvrmBuildShutdownPlan(&u2, s, 16);
    CHECK("a layout that was only moved (no resize) writes no Resizable BAR control", n > 0 && indexOf(s, n, kStepExt32, 0x254) < 0);
    CHECK("a plan buffer that is too small is refused rather than overrun", nvrmBuildShutdownPlan(&u, s, 4) == 0);
    CHECK("standby is the default", nvrmShutdownMode(false, 0, true) == kShutdownStandby);
    CHECK("full shutdown needs the request, no open user client and a live adapter", nvrmShutdownMode(true, 0, true) == kShutdownFull);
    CHECK("an open user client falls back to standby", nvrmShutdownMode(true, 1, true) == kShutdownStandby);
    CHECK("no adapter falls back to standby", nvrmShutdownMode(true, 0, false) == kShutdownStandby);
    return bad;
}
'''
    cxx = os.environ.get("CXX") or shutil.which("clang++") or shutil.which("g++")
    with tempfile.TemporaryDirectory(prefix="shutdown-plan-") as t:
        t = Path(t)
        (t / "t.cpp").write_text(harness)
        subprocess.run([cxx, "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(header.parent), str(t / "t.cpp"), "-o", str(t / "t")], check=True)
        r = subprocess.run([str(t / "t")], capture_output=True, text=True)
        print(r.stdout, end="")
        if r.returncode:
            failures.append("plan harness")

arm("NVRM handles system power-off and restart", "void NVRM::systemWillShutdown(IOOptionBits specifier)" in nvrm)
arm("the shutdown handler runs the plan placeLargeBar1's undo record feeds", "nvrmBuildShutdownPlan(&fBarUndo" in nvrm and "undo.valid = true; fBarUndo = undo;" in nvrm)
arm("the RM standby on shutdown is bounded", "waited < 8000" in nvrm)
arm("a full RM shutdown runs only when asked for and every user client has closed", "nvrmShutdownMode(askedFull, open, fAdapter)" in nvrm and "rm_shutdown_adapter(NULL, &s->fNv)" in nvrm)
arm("-nvrmnoshutdown keeps the old behaviour", '"-nvrmnoshutdown"' in nvrm)

if failures:
    print("FAILED:", ", ".join(failures)); sys.exit(1)
print("PASS")
