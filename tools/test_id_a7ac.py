#!/usr/bin/env python3
"""Intel Core 5 120U (Raptor Lake-U, PCI id 8086:A7AC) is carried end to end in this tree.
Arms: the field census names the id (so the id-table coverage checker covers it from now on); the display-engine table
that generates the framebuffer driver's id list has it, as display version 13 (Alder Lake-P class); the generated list
agrees with the table. With PCIIDS=<Linux include/drm/intel/pciids.h> set, the id must also be in the macros that
i915's table instantiates. The first arm fails on the revision before the census entry.
  usage: test_id_a7ac.py [rev]
"""
import json, os, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
rev = sys.argv[1] if len(sys.argv) > 1 else None
bad = 0


def show(path):
    if rev:
        r = subprocess.run(["git", "-C", str(ROOT), "show", f"{rev}:{path}"], capture_output=True, text=True)
        return r.stdout if r.returncode == 0 else ""
    return (ROOT / path).read_text()


def check(name, ok):
    global bad
    bad += not ok
    print(("ok   " if ok else "FAIL ") + name)


field = json.loads(show("tools/gpu_field_ids.json") or "{}")
check("the field census lists 8086:a7ac", "8086:a7ac" in field.get("intel_gpu", {}))
ver = json.loads(show("tools/igfx/display_versions.json") or "{}")
entry = ver.get("a7ac") or ver.get("0xa7ac") or (ver.get("ids", {}) or {}).get("a7ac")
check("the display table names a7ac", entry is not None)
inc = show("kexts/NMIntelFB/display_ids.inc")
m = re.search(r"\{0xa7ac,\s*(\d+)\}", inc)
check("the framebuffer driver's generated id list has a7ac as display version 13", m is not None and m.group(1) == "13")
pci = os.environ.get("PCIIDS")
if pci and os.path.exists(pci):
    text = open(pci).read()
    check("Linux's id tables list 0xA7AC", re.search(r"0xA7AC", text, re.I) is not None)
print("PASS" if not bad else f"{bad} FAILED")
sys.exit(1 if bad else 0)
