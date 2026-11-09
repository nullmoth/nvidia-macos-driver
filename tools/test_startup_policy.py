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
#include <string>
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
    {char b[64]; int n = nvrmFallbackModelName(b, sizeof b, "Turing", "TU106", 0x1F08);
     CHECK("an unnamed RTX 20 id reads as its generation, chip and PCI id", n > 0 && std::string(b) == "NVIDIA Turing GPU (TU106, 10DE:1F08)");}
    CHECK("a 64 MB laptop BAR1 that must grow to 8 GB uses the fallback range when devices already sit above 4 GB", nvrmFallbackWindowAllowed(64ULL << 20, 8ULL << 30, true));
    CHECK("a BAR1 left below 4 GB on a host that routes nothing higher stays put", !nvrmFallbackWindowAllowed(512ULL << 20, 8ULL << 30, false));
    CHECK("a same-size move of a BAR1 below 4 GB with no window stays refused", !nvrmFallbackWindowAllowed(512ULL << 20, 0, true));
    CHECK("a resize that stays under 4 GB with no window stays refused", !nvrmFallbackWindowAllowed(64ULL << 20, 256ULL << 20, true));
    CHECK("a BAR1 the firmware put above 4 GB keeps the old behaviour", nvrmFallbackWindowAllowed(8ULL << 30, 0, false));
    CHECK("order A: a head published before arming keeps arm-then-release", !nvrmReleaseBeforeArming(1, false) && !nvrmReleaseBeforeArming(1, true));
    CHECK("order B: no head and no display anywhere releases first", nvrmReleaseBeforeArming(0, true));
    CHECK("no head yet but connectors not enumerated keeps the hold", !nvrmReleaseBeforeArming(0, false));
    CHECK("a crash report fails the RPC at once, slow or not", nvrmGspTimeoutFatal(true, false, true, 0, 3) && nvrmGspTimeoutFatal(true, true, true, 0, 3));
    CHECK("a dead heartbeat with classification on is fatal at the first timeout", nvrmGspTimeoutFatal(false, true, false, 1, 3));
    CHECK("a dead heartbeat with classification off waits for the third timeout (the 6 s x 3 stall)", !nvrmGspTimeoutFatal(false, false, false, 1, 3) && nvrmGspTimeoutFatal(false, false, false, 3, 3));
    CHECK("a slow but healthy GSP keeps today's count with classification on", !nvrmGspTimeoutFatal(false, true, true, 1, 3) && !nvrmGspTimeoutFatal(false, true, true, 2, 3));
    CHECK("a slow but healthy GSP that keeps timing out is still failed at the threshold", nvrmGspTimeoutFatal(false, true, true, 3, 3));
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
arm("an early release still runs the arming that serves a later hotplug", "if (earlyRelease) s->releaseBootHold(" in nvrm and nvrm.index("if (earlyRelease) s->releaseBootHold(") < nvrm.index('nvrmSysW("debug.nvaccelfb", 1)') and "if (!earlyRelease) s->releaseBootHold(" in nvrm)
arm("bus mastering from firmware is switched off until pass 2 enables it", "off until the RM starts the card" in nvrm and nvrm.index("off until the RM starts the card") < nvrm.index("fPCI->setBusMasterEnable(true)"))
arm("GSP timeout classification is on by default and a boot-arg override still wins", 'rm_write_registry_dword(NULL, NULL, "RmGspTimeoutClassification", 1)' in nvrm and nvrm.index('"RmGspTimeoutClassification"') < nvrm.index('rm_parse_option_string(NULL, regdw)'))
arm("a card the RM cannot name gets a model from the GPU profile", "nvrmFallbackModelName(" in nvrm)
arm("a BAR1 that must grow past 4 GB is no longer refused outright when nothing but the window is missing", "nvrmFallbackWindowAllowed(fwBar1, newSize" in nvrm)
arm("placeLargeBar1 resizes a firmware BAR1 above 8 GB through the proven path", "nvrmBar1ShrinkTarget(bar1Size, rbSupported, nvrmKeepBar1Size())" in nvrm)

if failures:
    print("FAILED:", ", ".join(failures))
    sys.exit(1)
print("PASS")
