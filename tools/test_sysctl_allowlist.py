#!/usr/bin/env python3
"""Every sysctl the shipped kexts declare is on a reviewed list, none is writable by an ordinary user, and the
diagnostics that read physical memory stay behind the nvrmdiag boot-arg and a root check.

Before 2.0.3 debug.nvrm_winraw returned 256 KB of physical memory to any user, beside other panic-hunt leftovers.
The fix removed or gated them, but nothing stopped the next debug switch from shipping: this list does. A new sysctl
fails here until it is added below with the reason it is safe, and the arms show the check fires on an unlisted name,
on CTLFLAG_ANYBODY, and on an ungated diagnostic.
Usage: test_sysctl_allowlist.py [driver checkout]"""
import pathlib
import re
import sys

ROOT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1]
# The kexts the package ships (kexts/NVRM/fb and kexts/NVRM/agdc are older copies that no build uses).
SOURCES = ["kexts/NVRM/NVRM.cpp", "kexts/NVRM/os-xnu2.cpp", "kexts/NVRM/os-nvkms-xnu.cpp", "kexts/NVRM/nvrm_surfshare.cpp",
           "kexts/NVRM/accel/nvrm-accel.cpp", "kexts/NVRMFB/fb/nvrm-fb.cpp", "kexts/NVRMAGDC/nvrm-agdc.cpp",
           "kexts/NMIntelFB/intelfb.cpp"]
# Writable sysctls: root-only (no CTLFLAG_ANYBODY), each a display or WindowServer-path switch support asks users to
# set with sudo. Reviewed 2.6.6.
WRITABLE = {"nvaccel_igpu", "nvaccel_iop", "nvaccelfb", "nvaccel_vm_sticky", "nvaccel_iop_flip", "nvaccel_surface_gpu_on",
            "nvaccel_iop_async", "nvaccel_crc_head", "nvaccel_svdiag", "nvrmfb_agdc", "nvrmfb_vram_display_aware",
            "nvrmfb_vramtest", "nvrmfb_vramtest_mode", "nvrmfb_flip_latch", "nvrmfb_flip_lean", "nvrmfb_flip_interval",
            # Reviewed 2.7.2: the display-link switch the boot guard manages, and the Intel panel's test attach. Both root-only.
            "nvrmfb_agdc_k5", "nmintelfb_attach"}
# Read-only diagnostics that print physical addresses or memory: registered only with boot-arg nvrmdiag=1, root only.
PHYSICAL = {"nvrm_watchpage", "nvrm_physmap", "nvrm_pvsentinel", "nvrm_winwhy", "nvrm_pvchain", "nvrm_winraw"}
DECL = re.compile(r"SYSCTL_(?:INT|UINT|QUAD|PROC|STRING|OPAQUE|LONG|ULONG)\s*\(\s*_debug\s*,\s*OID_AUTO\s*,\s*(\w+)\s*,(.*?)\)\s*;", re.S)
FAILS = []


def arm(name, ok, detail=""):
    print(("ok   " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        FAILS.append(name)


def problems(text):
    out = []
    for m in DECL.finditer(text):
        name, flags = m.group(1), m.group(2)
        if "CTLFLAG_ANYBODY" in flags:
            out.append(f"{name}: writable by any user (CTLFLAG_ANYBODY)")
        if "CTLFLAG_RW" in flags and name not in WRITABLE:
            out.append(f"{name}: writable but not on the reviewed list")
    for name in PHYSICAL:
        if f"sysctl__debug_{name}" not in text:
            continue
        regs = re.findall(rf"^.*sysctl_register_oid\(&sysctl__debug_{name}\).*$", text, re.M)
        gated = all("nv_diag_on()" in r for r in regs) or (regs and "nv_pv_start" in text and
                    re.search(r"if \(nv_diag_on\(\)\) nv_pv_start\(\)", text))
        if not gated:
            out.append(f"{name}: registered without the nvrmdiag boot-arg")
    if "nv_wm_raw_sysctl" in text and not re.search(r"nv_wm_raw_sysctl SYSCTL_HANDLER_ARGS\s*\{[^}]*NV_DIAG_ROOT_ONLY", text, re.S):
        out.append("nvrm_winraw: handler has no root check")
    if re.search(r"SYSCTL_PROC\([^;]*nvrm_\w+[^;]*CTLFLAG_RD[^;]*\)", text) and "NV_DIAG_ROOT_ONLY" not in text and "os-xnu2" in text:
        out.append("physical-memory handlers lost their root check")
    return out


allp = []
for rel in SOURCES:
    p = ROOT / rel
    arm(f"{rel} exists", p.is_file())
    if p.is_file():
        allp += [f"{rel}: {x}" for x in problems(p.read_text(errors="replace"))]
arm("every shipped sysctl is reviewed: none user-writable, writable ones listed, physical-memory ones gated",
    not allp, "; ".join(allp))
# the check itself fires (negative controls on synthetic sources)
arm("an unlisted writable sysctl is caught",
    problems("SYSCTL_INT(_debug, OID_AUTO, nv_new_knob, CTLFLAG_RW | CTLFLAG_LOCKED, &x, 0, \"x\");") != [])
arm("CTLFLAG_ANYBODY is caught",
    problems("SYSCTL_INT(_debug, OID_AUTO, nvaccel_igpu, CTLFLAG_RW | CTLFLAG_ANYBODY, &x, 0, \"x\");") != [])
arm("a physical-memory sysctl registered without the boot-arg is caught",
    problems("SYSCTL_PROC(_debug, OID_AUTO, nvrm_physmap, CTLFLAG_RD, 0, 0, h, \"A\", \"\");\n"
             "static void f(void){ sysctl_register_oid(&sysctl__debug_nvrm_physmap); }") != [])
print(f"{len(FAILS)} failed")
sys.exit(1 if FAILS else 0)
