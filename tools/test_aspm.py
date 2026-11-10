#!/usr/bin/env python3
"""PCIe ASPM L1 on the GPU's link: the decision (Linux pcie_aspm's rule) and the wiring, plus the BAR facts published."""
import shutil, subprocess, sys, tempfile
from pathlib import Path

root = Path(__file__).resolve().parents[1]
nvrm = (root / "kexts/NVRM/NVRM.cpp").read_text(encoding="utf-8")
failures = []
def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok: failures.append(name)
hdr = root / "kexts/NVRM/nvrm_aspm.h"
arm("the ASPM header exists", hdr.exists())
harness = r'''
#include <cstdio>
#include <cstring>
#include "nvrm_aspm.h"
#define CHECK(name, cond) do { const bool c_ = (cond); std::printf("%s %s\n", c_ ? "ok  " : "FAIL", name); if (!c_) bad++; } while (0)
// Link Capabilities: ASPM support in 11:10, L1 exit latency in 17:15. Device Capabilities: acceptable L1 latency in 11:9.
static NvrmAspmEnd end(unsigned support, unsigned exitLat, unsigned accept, unsigned ctl) {
    NvrmAspmEnd e = {}; e.present = true; e.lnkCap = (support << 10) | (exitLat << 15); e.devCap = accept << 9; e.lnkCtl = ctl; return e;
}
int main() {
    int bad = 0; unsigned ec = 0, uc = 0;
    NvrmAspmEnd ep = end(3, 2, 7, 0x0040), up = end(3, 3, 0, 0x0000);
    CHECK("both ends support L1 and the card accepts any latency: L1 on at both, other bits kept",
          nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmEnabled && ec == 0x0042 && uc == 0x0002);
    ep = end(1, 2, 7, 0); up = end(3, 2, 0, 0);
    CHECK("a card that supports only L0s is left alone", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmEndpointNoL1);
    ep = end(3, 2, 7, 0); up = end(0, 2, 0, 0);
    CHECK("an upstream port with no ASPM is left alone", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmUpstreamNoL1);
    ep = end(3, 2, 3, 0); up = end(3, 5, 0, 0);
    CHECK("an exit latency of 16-32 us on a path the card accepts only 8 us of is refused", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmLatency);
    ep = end(3, 3, 4, 0); up = end(3, 4, 0, 0);
    CHECK("an exit latency equal to the acceptable one is allowed", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmEnabled);
    ep = end(3, 2, 7, 0x0002); up = end(3, 2, 0, 0x0002);
    CHECK("firmware that already enabled L1 on both ends is not touched", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmAlready);
    ep = end(3, 2, 7, 0x0002); up = end(3, 2, 0, 0x0000);
    CHECK("L1 on only one end is completed", nvrmAspmPlan(&ep, &up, &ec, &uc) == kAspmEnabled && uc == 0x0002);
    NvrmAspmEnd none = {};
    CHECK("no PCIe capability on either end is refused", nvrmAspmPlan(&none, &up, &ec, &uc) == kAspmNoCapability && nvrmAspmPlan(&ep, nullptr, &ec, &uc) == kAspmNoCapability);
    CHECK("every outcome has a sentence the app can show", std::strlen(nvrmAspmWhyString(kAspmLatency)) > 10 && std::strcmp(nvrmAspmWhyString(kAspmEnabled), "L1 enabled") == 0);
    CHECK("rebar state: none without the capability, on above 256 MB, off at 256 MB or less",
          !std::strcmp(nvrmRebarState(false, 0), "none") && !std::strcmp(nvrmRebarState(true, 8ULL << 30), "on") &&
          !std::strcmp(nvrmRebarState(true, 256ULL << 20), "off") && !std::strcmp(nvrmRebarState(true, 64ULL << 20), "off"));
    return bad;
}
'''
if hdr.exists():
    cxx = shutil.which("clang++") or shutil.which("g++")
    with tempfile.TemporaryDirectory(prefix="aspm-") as t:
        t = Path(t); (t / "t.cpp").write_text(harness)
        subprocess.run([cxx, "-std=c++17", "-Wall", "-Wextra", "-Werror", "-I", str(hdr.parent), str(t / "t.cpp"), "-o", str(t / "t")], check=True)
        r = subprocess.run([str(t / "t")], capture_output=True, text=True); print(r.stdout, end="")
        if r.returncode: failures.append("aspm harness")
arm("ASPM L1 is enabled once the adapter is up", "fPassDone = 2;\n        enableAspmL1();" in nvrm)
arm("ASPM L1 is enabled again after a wake", "if (ok) enableAspmL1();" in nvrm)
arm("the firmware's ASPM setting is put back at shutdown", "restoreAspm();\n    super::systemWillShutdown" in nvrm)
arm("-nvrmnoaspm switches it off", '"-nvrmnoaspm"' in nvrm)
body = nvrm[nvrm.index("void NVRM::enableAspmL1()"):nvrm.index("void NVRM::restoreAspm()")]
arm("L1 is asked for only with nvrmaspm=1: the opt-in is read before any config register",
    'PE_parse_boot_argn("nvrmaspm"' in body and "on != 1" in body and body.index('"nvrmaspm"') < body.index("configRead"))
sd = nvrm[nvrm.index("void NVRM::systemWillShutdown"):nvrm.index("// ---- PCIe link power")]
arm("shutdown keeps the interrupt on through the standby and turns it off before the BAR writes",
    sd.count("fIntSrc->disable()") == 1 and sd.index("kernel_thread_start") < sd.index("fIntSrc->disable()") < sd.index("nvrmBuildShutdownPlan"))
arm("the BAR facts are published for the app", all(k in nvrm for k in ('"nvrm-bar1-mb"', '"nvrm-bar1-firmware-mb"', '"nvrm-rebar"', '"nvrm-rebar-max-mb"', '"nvrm-above-4g"', '"nvrm-aspm"')))
if failures:
    print("FAILED:", ", ".join(failures)); sys.exit(1)
print("PASS")
