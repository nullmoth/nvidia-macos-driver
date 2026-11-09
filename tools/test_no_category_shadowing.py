#!/usr/bin/env python3
"""No Objective-C category in the plugin re-implements a method its class already implements.

A category method replaces the class's own method when the image loads, so a category stub silently disables the real
code. Tile shading never ran because NVMTLGL.m carried empty category stubs for every setTile* selector and
dispatchThreadsPerTile while the real methods sat in NVMTLObjects.m. This scans every .m file in plugin/ and fails on
any selector defined both in `@implementation C` and in `@implementation C (Category)`, or in two categories of C.

usage: test_no_category_shadowing.py [plugin dir]   (calibration: the old NVMTLGL.m must be caught)"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PLUGIN = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "plugin"
IMPL = re.compile(r"^@implementation\s+(\w+)(?:\s*\(\s*(\w*)\s*\))?", re.M)
METHOD = re.compile(r"^([-+])\s*\([^)]*\)\s*([^{;]*?)\s*\{", re.M)


def selector(sig):
    """'setTileBuffer:(id)b offset:(NSUInteger)o atIndex:(NSUInteger)i' -> 'setTileBuffer:offset:atIndex:'"""
    parts = re.findall(r"(\w+)\s*:", re.sub(r"\([^)]*\)", " ", sig))
    return "".join(p + ":" for p in parts) if parts else sig.strip().split()[0]


def definitions(texts):
    """{(class, kind+selector): [(file, category or None)]} for every method body in every @implementation block."""
    out = {}
    for name, text in texts:
        for m in IMPL.finditer(text):
            end = text.find("\n@end", m.end())
            body = text[m.end():end if end >= 0 else len(text)]
            cls, cat = m.group(1), m.group(2)
            for mm in METHOD.finditer(body):
                out.setdefault((cls, mm.group(1) + selector(mm.group(2))), []).append((name, cat))
    return out


def shadowed(texts):
    bad = []
    for (cls, sel), where in sorted(definitions(texts).items()):
        cats = [w for w in where if w[1] is not None]
        if cats and len(where) > 1:
            bad.append(f"{cls} {sel}: " + ", ".join(f"{f}({c or 'class'})" for f, c in where))
    return bad


def tree(plugin_dir):
    return [(p.name, p.read_text(errors="replace")) for p in sorted(pathlib.Path(plugin_dir).glob("*.m"))]


fails = 0
bad = shadowed(tree(PLUGIN))
print(("ok   " if not bad else "FAIL ") + f"no category re-implements a method of its class ({len(bad)} found)")
for b in bad[:20]:
    print("     " + b)
fails += bool(bad)
# calibration: the NVMTLGL.m from before the fix must be caught, or a zero above proves nothing
old = subprocess.run(["git", "-C", str(ROOT), "show", "c6619e54:plugin/NVMTLGL.m"], capture_output=True, text=True)
if old.returncode == 0:
    texts = [t for t in tree(PLUGIN) if t[0] != "NVMTLGL.m"] + [("NVMTLGL.m", old.stdout)]
    hits = [b for b in shadowed(texts) if "dispatchThreadsPerTile" in b or "setTileBuffer:offset:atIndex:" in b]
    print(("ok   " if len(hits) == 2 else "FAIL ") + f"control: the old stubs are caught ({len(hits)} of the 2 named)")
    fails += len(hits) != 2
else:
    print("UNKNOWN control: c6619e54 not in this checkout"); fails += 1
print("FAILED %d" % fails if fails else "PASS all arms")
sys.exit(1 if fails else 0)
