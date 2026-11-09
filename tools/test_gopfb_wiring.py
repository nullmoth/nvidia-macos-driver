#!/usr/bin/env python3
"""NMGopFB is wired where it must be and off by default. Reads the kernel sources.
   python3 test_gopfb_wiring.py [tree-root]      (default: this tree; pass an older tree to see the arms fail)"""
import re, sys
from pathlib import Path

root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]
def rd(p):
    f = root / p
    return f.read_text(encoding="utf-8") if f.exists() else ""
src, hdr = rd("kexts/NMGopFB/gopfb.cpp"), rd("kexts/NMGopFB/nm_gopfb.h")
plist, build = rd("kexts/NMGopFB/Info.plist"), rd("kexts/NMGopFB/build.sh")
accel, ifb = rd("kexts/NVRM/accel/nvrm-accel.cpp"), rd("kexts/NMIntelFB/intelfb.cpp")
fails = []
def arm(name, ok):
    print(("ok   " if ok else "FAIL ") + name)
    if not ok: fails.append(name)

probe = src[src.find("NMGopFB::probe"):src.find("// Finds the firmware framebuffer")]
arm("the kext exists", bool(src and hdr and plist))
arm("probe reads the switch only through nmh_level_parse and declines at level 0, before touching the device",
    re.search(r"fHyLevel = nmh_level_parse\(present, lv\);\s*if \(!fHyLevel\) return nullptr;", probe) is not None
    and probe.find("if (!fHyLevel) return nullptr;") < probe.find("configRead16"))
arm("a boot-arg turns it off even with the switch on", '"-nmgopfboff"' in probe)
arm("the choice is the tested nmgop_select, with NMXGPU's presence and the tester's override passed in",
    "nmgop_select(ven, dev," in probe and 'serviceMatching("NMXGPU")' in probe and '"nmgopid"' in probe)
arm("the framebuffer is checked by nmgop_geometry against the BARs before it is mapped",
    0 <= src.find("nmgop_geometry(") < src.find("withAddressRange(g.phys"))
arm("a refused framebuffer fails start() so the firmware framebuffer keeps the device",
    "leaving the firmware screen\", nmgop_why_name(why)); return false;" in src and "if (!adoptFirmwareFramebuffer(pci)) return false;" in src)
arm("the mapping is write-combined at the descriptor, not only at map time", "(kIOWriteCombineCache << kCacheShift)" in src)
arm("it never writes the display engine, the PCI config space or any BAR register",
    not re.search(r"configWrite|ioWrite|\bwr\(|\*\s*\(volatile uint32_t \*\)\s*\(f?[Rr]egs", src) and "mapDeviceMemoryWithRegister" not in src)
arm("it publishes head 4 of the NVIDIA accelerator like NMIntelFB",
    'setProperty("IOFBDependentID", 0x4E56524D00000001ull, 64)' in src and 'setProperty("IOFBDependentIndex", 4ull, 32)' in src)
arm("scan-out is answered with a CPU mapping and no VRAM address", "r->phys = 0; r->actualSize = 0;" in src and "NVRM_VRAM_FN_SCANOUT" in src)
arm("import is refused with a reason (the accelerator copies) and going home is always honoured",
    "q->result = NMH_E_PROVIDER;" in src and "q->op == NMH_OP_HOME" in src)
arm("only AMD display-class devices are matched by the personality", "0x00001002&amp;0x0000ffff" in plist and "0x00008086" not in plist and "0x000010de" not in plist.lower())
arm("it supports macOS 15 and 26 only", "version_major != 24 && version_major != 25" in src and "-mmacosx-version-min" in build)
arm("the MIT notice and the link to PrimeBridge are in the file header",
    "https://github.com/myvoow/PrimeBridge" in src and "MIT License, Copyright (c) 2026 myvoow" in src and "MIT License, Copyright (c) 2026 myvoow" in hdr)
arm("the logic header is plain C (no kernel headers)", "#include <IOKit" not in hdr and "#include <stdint.h>" in hdr)
arm("the accelerator counts NMGopFB as a head of its own only with the switch on (never when it is off)",
    'if (gNmHybrid && s->metaCast("NMGopFB")) break;' in accel and "if (gNmHybrid) if (OSDictionary *gm = serviceMatching(\"NMGopFB\"))" in accel
    and 'strcmp(owner, "NMGopFB") == 0))) {' in accel)
arm("NMIntelFB is untouched", "NMGopFB" not in ifb)
arm("NMIntelFB still declines unless the switch is on", re.search(r"nmh_level_parse\(present, lv\);\s*if \(!fHyLevel\) return nullptr;", ifb) is not None)
print(f"{len(fails)} failed")
sys.exit(1 if fails else 0)
