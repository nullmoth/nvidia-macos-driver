#!/usr/bin/env python3
"""BAR recovery when the device memory list is empty: assigned-addresses lookup and BAR size probe, plus the wiring."""
import os, shutil, subprocess, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[1]
nvrm = (root / "kexts/NVRM/NVRM.cpp").read_text(encoding="utf-8")
failures = []

def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok:
        failures.append(name)

header = root / "kexts/NVRM/nvrm_bar_recover.h"
arm("the recovery header exists", header.exists())
if header.exists():
    harness = r'''
#include <cstdio>
#include "nvrm_bar_recover.h"
#define CHECK(name, cond) do { std::printf("%s %s\n", (cond) ? "ok  " : "FAIL", name); if (!(cond)) bad++; } while (0)
int main() {
    int bad = 0;
    // BAR0 32-bit 16 MB @0x62000000, BAR1 64-bit 256 MB @0x4010000000, BAR3 64-bit 32 MB @0x4020000000
    const unsigned cells[] = {
        0x82000010u, 0, 0x62000000u, 0, 0x01000000u,
        0xc3000014u, 0x40, 0x10000000u, 0, 0x10000000u,
        0xc300001cu, 0x40, 0x20000000u, 0, 0x02000000u,
        0x81000020u, 0, 0x00003000u, 0, 0x80u };               // an I/O entry at 0x20 must be ignored
    unsigned long long a = 0, s = 0;
    CHECK("BAR0 comes from its 32-bit memory entry", nvrmAssignedLookup(cells, 20, 0x10, &a, &s) && a == 0x62000000ULL && s == 0x1000000ULL);
    CHECK("BAR1 comes from its 64-bit memory entry above 4 GB", nvrmAssignedLookup(cells, 20, 0x14, &a, &s) && a == 0x4010000000ULL && s == 0x10000000ULL);
    CHECK("BAR3 comes from its entry", nvrmAssignedLookup(cells, 20, 0x1c, &a, &s) && a == 0x4020000000ULL && s == 0x2000000ULL);
    CHECK("an I/O entry is not a memory aperture", !nvrmAssignedLookup(cells, 20, 0x20, &a, &s));
    CHECK("a register with no entry is not found", !nvrmAssignedLookup(cells, 20, 0x24, &a, &s));
    CHECK("an empty property finds nothing", !nvrmAssignedLookup(nullptr, 0, 0x10, &a, &s));
    CHECK("a truncated entry is ignored", !nvrmAssignedLookup(cells, 4, 0x10, &a, &s));
    const unsigned zero[] = { 0x82000010u, 0, 0, 0, 0 };
    CHECK("an entry with no address and size is skipped", !nvrmAssignedLookup(zero, 5, 0x10, &a, &s));
    CHECK("a 32-bit BAR reading 0xff000000 is 16 MB", nvrmBarSizeFromProbe(0xff000000u, 0, false) == 0x1000000ULL);
    CHECK("a 64-bit prefetchable BAR reading hi 0xffffffff lo 0xf000000c is 256 MB", nvrmBarSizeFromProbe(0xf000000cu, 0xffffffffu, true) == 0x10000000ULL);
    CHECK("a 64-bit BAR of 8 GB (hi 0xfffffffe lo 0)", nvrmBarSizeFromProbe(0x0000000cu, 0xfffffffeu, true) == 0x200000000ULL);
    CHECK("an I/O BAR has no memory size", nvrmBarSizeFromProbe(0xffffff01u, 0, false) == 0);
    CHECK("a BAR that reads back zero has no size", nvrmBarSizeFromProbe(0, 0, true) == 0);
    return bad;
}
'''
    cxx = os.environ.get("CXX") or shutil.which("clang++") or shutil.which("g++")
    with tempfile.TemporaryDirectory(prefix="bar-recover-") as t:
        t = Path(t)
        (t / "t.cpp").write_text(harness)
        subprocess.run([cxx, "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(header.parent), str(t / "t.cpp"), "-o", str(t / "t")], check=True)
        r = subprocess.run([str(t / "t")], capture_output=True, text=True)
        print(r.stdout, end="")
        if r.returncode:
            failures.append("recover harness")

arm("readBARs recovers a missing BAR from assigned-addresses or the BAR register", "nvrmAssignedLookup(cells, nCells, off, &a, &sz)" in nvrm and "nvrmBarSizeFromProbe(" in nvrm)
arm("the recovered apertures are published to the device memory list", "bar recovery:" in nvrm and "fPCI->setDeviceMemory(arr)" in nvrm)
arm("sizing a BAR restores the register and the command word", "fPCI->configWrite32(off, lo);" in nvrm and "fPCI->configWrite16(0x04, cmd);" in nvrm)

if failures:
    print("FAILED:", ", ".join(failures)); sys.exit(1)
print("PASS")
