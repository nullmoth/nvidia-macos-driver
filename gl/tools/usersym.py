#!/usr/bin/env python3
# usersym.py <binary> <label> <libroot>...: check every strong undefined import of an x86_64 user binary against the
# exported symbols found under the given library roots (SDK stub libraries or libraries copied from a release image).
# Prints the population beside every count, and runs two controls so a zero can be trusted:
#   _malloc must be found, and _clock_gettime (new in 10.12) must be reported missing on roots older than 10.12.
import os, re, subprocess, sys
binary, label, roots = sys.argv[1], sys.argv[2], sys.argv[3:]
def run(c):
    return subprocess.run(c, capture_output=True, text=True, errors="replace").stdout
have, files = set(), 0
for root in roots:
    paths = [root] if os.path.isfile(root) else [os.path.join(d, f) for d, _, fs in os.walk(root) for f in fs]
    for p in paths:
        if p.endswith(".tbd"):
            t = open(p, errors="replace").read(); n = len(have)
            for m in re.finditer(r"(?:symbols|re-exports|weak-symbols|weak-def-symbols):\s*\[([^\]]*)\]", t, re.S):
                have |= {s.strip().strip("'\"") for s in m.group(1).split(",") if s.strip()}
            files += len(have) > n; continue
        # a file named on the command line is read whatever it is called
        if not os.path.isfile(root) and (os.path.islink(p) or not (p.endswith(".dylib") or "." not in os.path.basename(p))): continue
        out = run(["nm", "-arch", "x86_64", "-gU", p])
        syms = {l.split()[-1] for l in out.splitlines() if l.strip() and not l.endswith(":")}
        # re-exported aliases (memcpy and friends from 10.9 on) exist only in the export trie, which nm does not print
        syms |= set(re.findall(r"^\s+(?:0x[0-9A-Fa-f]+|\[re-export\])\s+(\S+)", run(["xcrun", "dyld_info", "-arch", "x86_64", "-exports", p]), re.M))
        if syms: have |= syms; files += 1
imports = {}
for l in run(["nm", "-m", "-arch", "x86_64", binary]).splitlines():
    m = re.search(r"\(undefined\) (weak )?external (\S+) \(from ([^)]+)\)", l)
    if m: imports.setdefault(m.group(3), {})[m.group(2)] = bool(m.group(1))
print(f"== {os.path.basename(binary)} against {label}: {files} library files read, {len(have)} exported symbols known")
ctl_ok = "_malloc" in have; ctl_neg = "_clock_gettime" not in have
print(f"   control: _malloc found = {ctl_ok}; _memcpy (an alias from 10.9 on) found = {'_memcpy' in have}; _clock_gettime (10.12) absent = {ctl_neg}")
tot = miss = 0
for lib in sorted(imports):
    strong = {s for s, w in imports[lib].items() if not w}; weak = set(imports[lib]) - strong
    m = sorted(s for s in strong if s not in have); mw = sorted(s for s in weak if s not in have)
    tot += len(strong); miss += len(m)
    print(f"   {lib:28s} strong imports {len(strong):4d} missing {len(m):4d}   weak {len(weak):3d} missing {len(mw):3d}")
    for s in m[:60]: print(f"        {s}")
    if len(m) > 60: print(f"        ... {len(m) - 60} more")
print(f"   TOTAL strong imports {tot}, missing {miss}")
