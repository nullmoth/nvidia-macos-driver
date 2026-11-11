#!/usr/bin/env python3
# importcheck.py <system root copy> <binary>...: every non-weak import of each binary, looked up in the library the
# binary names for it inside the given copy of a release's system files. Re-exported libraries are followed, and
# aliases and re-exported symbols are read from the export tables, so a symbol is reported missing only when no
# library of that release provides it under the name the binary binds. A library that is not in the copy is
# reported as UNREAD, never as clean.
import os, re, subprocess, sys
ROOT = sys.argv[1]
def run(cmd):
    p = subprocess.run(cmd, capture_output=True, text=True, errors="replace"); return p.stdout
_exp = {}
def find(path, loader=None):
    cands = [ROOT + path]
    if path.startswith("@rpath/") or path.startswith("@loader_path/") or path.startswith("@executable_path/"): return None
    m = re.match(r"(.*/([^/]+)\.framework)/Versions/[^/]+/([^/]+)$", path)
    if m: cands += [ROOT + m.group(1) + "/" + m.group(3)] + [ROOT + m.group(1) + "/Versions/%s/%s" % (v, m.group(3)) for v in "ABC"]
    for c in cands:
        if os.path.isfile(c): return c
    return None
def loads(f):
    out = run(["otool", "-arch", "x86_64", "-l", f]); res = []; cmd = None
    for l in out.splitlines():
        l = l.strip()
        if l.startswith("cmd "): cmd = l.split()[1]
        elif l.startswith("name ") and cmd in ("LC_LOAD_DYLIB", "LC_REEXPORT_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_LOAD_UPWARD_DYLIB"):
            res.append((cmd, l.split()[1]))
    return res
def exports(f, depth=0):
    if f in _exp: return _exp[f]
    s = set(); _exp[f] = s
    for l in run(["nm", "-arch", "x86_64", "-gUm", f]).splitlines():
        m = re.search(r"\) (?:weak |private )*external (\S+)", l)
        if m: s.add(m.group(1))
        m = re.search(r"\(indirect\) external (\S+)", l)
        if m: s.add(m.group(1))
    for l in run(["xcrun", "dyld_info", "-arch", "x86_64", "-exports", f]).splitlines():
        m = re.match(r"\s*(?:0x[0-9A-Fa-f]+\s+|\[re-export\]\s+)(\S+)", l)
        if m: s.add(m.group(1))
    if depth < 6:
        for cmd, name in loads(f):
            if cmd == "LC_REEXPORT_DYLIB":
                c = find(name)
                if c: s |= exports(c, depth + 1)
    return s
for b in sys.argv[2:]:
    libs = {}
    for cmd, name in loads(b):
        leaf = os.path.basename(name); leaf = re.sub(r"\.dylib$", "", leaf); leaf = re.sub(r"\.[0-9A-Z]+$", "", leaf) if leaf.startswith("lib") else leaf
        libs[leaf] = (cmd, name)
    imps = {}
    for l in run(["nm", "-m", "-arch", "x86_64", b]).splitlines():
        m = re.search(r"\(undefined\) (weak )?external (\S+) \(from ([^)]+)\)", l)
        if m and not m.group(1): imps.setdefault(m.group(3), set()).add(m.group(2))
    total = miss = unread = 0; lines = []
    for lib, syms in sorted(imps.items()):
        total += len(syms)
        ent = libs.get(lib) or next((v for k, v in libs.items() if k.startswith(lib)), None)
        path = find(ent[1]) if ent else None
        if not path:
            unread += len(syms); lines.append("   UNREAD %s (%d imports): %s is not in this copy" % (lib, len(syms), ent[1] if ent else "?")); continue
        have = exports(path); gone = sorted(syms - have)
        # a flat lookup across everything already read, to tell "moved to another library" from "absent"
        moved = [g for g in gone if any(g in e for e in _exp.values())]
        miss += len(gone)
        if gone: lines.append("   MISSING from %s (%d of %d): %s%s" % (lib, len(gone), len(syms), " ".join(gone[:60]), ("  [present in another library of this release: %s]" % " ".join(moved)) if moved else ""))
    weaklibs = [n for c, n in loads(b) if c == "LC_LOAD_WEAK_DYLIB"]
    hard_absent = [n for c, n in loads(b) if c == "LC_LOAD_DYLIB" and not n.startswith("@") and not find(n)]
    print("%s vs %s: non-weak imports %d, missing %d, unread %d%s" % (b.split("/metal/")[-1], os.path.basename(ROOT), total, miss, unread,
          ("; LIBRARY NOT IN COPY (load fails if truly absent): " + " ".join(hard_absent)) if hard_absent else ""))
    for l in lines: print(l)
