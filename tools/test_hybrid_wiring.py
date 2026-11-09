#!/usr/bin/env python3
"""The hybrid switch is wired where it must be, and off by default. Reads the kernel sources.
   python3 test_hybrid_wiring.py [tree-root]      (default: this tree; pass an older tree to see the arms fail)"""
import re, sys
from pathlib import Path

root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
def rd(p):
    f = root / p
    return f.read_text(encoding="utf-8") if f.exists() else ""
accel, nvrm, ifb = rd("kexts/NVRM/accel/nvrm-accel.cpp"), rd("kexts/NVRM/NVRM.cpp"), rd("kexts/NMIntelFB/intelfb.cpp")
hdr, abi = rd("kexts/NVRM/accel/nm_hybrid.h"), rd("kexts/NVRM/accel/nm_hybrid_abi.h")
fails = []
def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok: fails.append(name)

arm("the accelerator's switch starts at 0 (off)", re.search(r"static int gNmHybrid = 0;", accel) is not None)
arm("the accelerator reads the switch only through nmh_level_parse", "gNmHybrid = nmh_level_parse(present, lv)" in accel)
arm("the changed-pixel copy runs only when the switch is on and the panel is another GPU's (no VRAM address)",
    re.search(r"if \(gNmHybrid >= NMH_COPY && !q\.phys && !fb->metaCast\(\"NVRMNVDAFramebuffer\"\) && !dst30\s*&& \(srcFmt == NV_IOP_FMT_BGRA \|\| srcFmt == NV_IOP_FMT_L10R\)\)", accel) is not None)
arm("an HDR source (half float) or a 30-bit scanout never takes the 8-bit hybrid copy", "!dst30" in accel and "srcFmt == NV_IOP_FMT_BGRA || srcFmt == NV_IOP_FMT_L10R)) {" in accel)
arm("the old copy loop is still the else branch, so switch off is the old behaviour", "    } else {\n    for (unsigned y = 0; y < h; y++) {" in accel)
arm("import is tried only at level 2 and only on a framebuffer that is not NVIDIA's", "gNmHybrid >= NMH_IMPORT && nvAccelHybridImport(fb, head, s)" in accel and "fb->metaCast(\"NVRMNVDAFramebuffer\")) return false;" in accel)
arm("a copied frame after an imported one takes the plane home first", "nvAccelHybridHome(fb, head);" in accel and "nmh_shadow_invalidate(&gHyShadow[head].sh)" in accel)
arm("the surface is flushed from the CPU's caches before the other GPU reads it", "kIOMemoryIncoherentIOFlush" in accel)
arm("the surface's pixels are final before it is flipped to (same checks as the copy path)",
    accel.count("nvSurfWaitReady(id);\n    if (!nvAccelSurfCopyReady(s, head)) return true;") == 1)
arm("the panel driver takes the panel only with the switch on", re.search(r"nmh_level_parse\(present, lv\);\s*if \(!fHyLevel\) return nullptr;", ifb) is not None)
arm("the panel driver writes translation-table entries only in a run of equal unused entries", "nmh_find_free_run(ggttRead" in ifb)
arm("the panel driver never edits entries of the firmware surface", "fwLo, fwHi" in ifb)
arm("a flip that never lands ends import for the boot and puts the firmware surface back", "fHyLevel = NMH_COPY;" in ifb and "hybridWriteSurf(fSurf, fStrideReg);" in ifb)
arm("going home is honoured even after import was switched off", 0 <= ifb.find("q->op == NMH_OP_HOME") < ifb.find("fHyLevel < NMH_IMPORT || !fRegs"))
arm("auto-go waits for head 4 only when the switch is on", "hyWait = present && (lv == 1 || lv == 2)" in nvrm and "if (hyWait) {" in nvrm)
arm("that wait is bounded by the same 20 iterations as the NVIDIA heads", re.search(r"for \(i = 0; i < 20; i\+\+\) \{\n[^\n]*\n\s*if \(hyWait\)", nvrm) is not None)
arm("the ABI header names one entry point and a version", "NMH_FN_IMPORT" in abi and "NMH_ABI_VERSION    1u" in abi)
arm("the logic header is plain C (no kernel headers)", "#include <IOKit" not in hdr and "#include <stdint.h>" in hdr)
print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
