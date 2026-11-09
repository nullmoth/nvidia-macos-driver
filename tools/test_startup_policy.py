#!/usr/bin/env python3
"""Startup timing: headless deadline after the settle delay, nvrmsettle override, early exit when no connector has a display.
Compiles kexts/NVRM/nvrm_startup_policy.h on the host and checks the kernel sources use it."""
import os, shutil, subprocess, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[1]
nvrm = (root / "kexts/NVRM/NVRM.cpp").read_text(encoding="utf-8")
accel = (root / "kexts/NVRM/accel/nvrm-accel.cpp").read_text(encoding="utf-8")
failures = []

def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        failures.append(name)

header = root / "kexts/NVRM/nvrm_startup_policy.h"
arm("the policy header exists", header.exists())

if header.exists():
    harness = r'''
#include <cstdio>
#include "nvrm_startup_policy.h"
#define CHECK(name, cond) do { std::printf("%s %s\n", (cond) ? "ok  " : "FAIL", name); if (!(cond)) bad++; } while (0)
int main() {
    int bad = 0;
    CHECK("a 100 s settle moves the 60 s headless deadline to 160 s", nvrmHeadlessDeadlineSec(60, 100000) == 160);
    CHECK("a card that settles in 0.5 s waits 61 s (partial seconds round up)", nvrmHeadlessDeadlineSec(60, 500) == 61);
    CHECK("no auto-go (settle 0) keeps the plain 60 s deadline", nvrmHeadlessDeadlineSec(60, 0) == 60);
    CHECK("a typo of nvrmsettle=99999999 cannot withhold Metal beyond 5 minutes of settle", nvrmHeadlessDeadlineSec(60, 99999999u) == 360);
    {int c[] = {0, 0}; CHECK("two empty connector lists mean no display anywhere", nvrmNoDisplayOnAnyConnector(c, 2));}
    {int c[] = {0, 1}; CHECK("one connected display means a head can arm", !nvrmNoDisplayOnAnyConnector(c, 2));}
    {int c[] = {0, -1}; CHECK("a nub that has not published yet keeps the wait going", !nvrmNoDisplayOnAnyConnector(c, 2));}
    CHECK("no nub at all is not evidence of an empty connector list", !nvrmNoDisplayOnAnyConnector(nullptr, 0));
    CHECK("unplaced BAR1 waits 100 s by default", nvrmAutoGoSettleMs(false, false, false, 0) == 100000);
    CHECK("placed BAR1 on Turing waits 10 s, on others 0.5 s", nvrmAutoGoSettleMs(true, true, false, 0) == 10000 && nvrmAutoGoSettleMs(true, false, false, 0) == 500);
    CHECK("nvrmsettle=10000 overrides the 100 s default on an unplaced BAR1", nvrmAutoGoSettleMs(false, false, true, 10000) == 10000);
    CHECK("nvrmsettle=0 is honoured", nvrmAutoGoSettleMs(false, true, true, 0) == 0);
    CHECK("a 16 GB BAR1 on a card offering 8 GB is resized down to 8 GB", nvrmBar1ShrinkTarget(16ULL << 30, 0x3fe0, false) == (8ULL << 30));
    CHECK("a 32 GB BAR1 is resized down to 8 GB as well", nvrmBar1ShrinkTarget(32ULL << 30, 0x7fe0, false) == (8ULL << 30));
    CHECK("an 8 GB BAR1 is left alone", nvrmBar1ShrinkTarget(8ULL << 30, 0x3fe0, false) == 0);
    CHECK("a 256 MB BAR1 is not touched by the shrink rule", nvrmBar1ShrinkTarget(256ULL << 20, 0x3fe0, false) == 0);
    CHECK("a card that does not offer 8 GB keeps its size", nvrmBar1ShrinkTarget(16ULL << 30, 0x2000 >> 1, false) == 0);
    CHECK("-nvrmkeepbar1 keeps the firmware size", nvrmBar1ShrinkTarget(16ULL << 30, 0x3fe0, true) == 0);
    return bad;
}
'''
    cxx = os.environ.get("CXX") or shutil.which("clang++") or shutil.which("g++")
    with tempfile.TemporaryDirectory(prefix="startup-policy-") as t:
        t = Path(t)
        (t / "t.cpp").write_text(harness)
        subprocess.run([cxx, "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(header.parent), str(t / "t.cpp"), "-o", str(t / "t")], check=True)
        r = subprocess.run([str(t / "t")], capture_output=True, text=True)
        print(r.stdout, end="")
        if r.returncode:
            failures.append("policy harness")

arm("NVAccel places the headless deadline after the published settle delay", "nvrmHeadlessDeadlineSec(" in accel and "me->fHeadlessWaited < me->fHeadlessDeadlineSec" in accel)
arm("NVAccel no longer compares against the fixed 60 s constant", "me->fHeadlessWaited < kHeadlessDeadlineSec" not in accel)
arm("NVRM publishes its settle delay on the PCI device for NVAccel", 'fPCI->setProperty("nvrm-autogo-settle-ms"' in nvrm)
arm("NVRM takes its settle delay from the policy (nvrmsettle override included)", "nvrmAutoGoSettleMs(" in nvrm)
arm("auto-go stops waiting for heads when no connector carries a display", "nvrmNoDisplayAnywhere()" in nvrm and "not waiting for heads" in nvrm)
arm("placeLargeBar1 resizes a firmware BAR1 above 8 GB through the proven path", "nvrmBar1ShrinkTarget(bar1Size, rbSupported, nvrmKeepBar1Size())" in nvrm)

if failures:
    print("FAILED:", ", ".join(failures))
    sys.exit(1)
print("PASS")
