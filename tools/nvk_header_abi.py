#!/usr/bin/env python3
"""nvk_header_abi.py - compare the NVIDIA open kernel module headers NVK compiles against, between module versions.

NVK's NVRM backend talks to the resource manager through structs and constants from the open kernel module headers.
A header that changes a struct NVK passes to a control call changes the wire layout silently, so before shipping a
build against one version the structs and defines the backend names are compared with the version the kernel module is
built from.

usage: nvk_header_abi.py [--all] [--selftest] <nvrm source dir> <header list> <reference tree> <tree> [<tree>...]
  <nvrm source dir>  the .c/.h files of the NVRM backend (their identifiers decide what is "used")
  <header list>      one path per line, relative to a module tree (the headers the backend preprocesses to)
  --all              compare every struct and define in the listed headers instead of only the used ones. Run it once
                     as a control: it must report differences when two versions really differ, or "none used" proves
                     nothing."""
import glob, os, re, sys

IDENT = re.compile(r"\bNV[0-9A-Fa-f]*_[A-Z0-9_]+\b")


def structs(txt):
    out = {}
    for m in re.finditer(r"typedef\s+struct\s+(\w+)\s*\{(.*?)\}\s*(\w+)\s*;", txt, re.S):
        body = " ".join(re.sub(r"//.*|/\*.*?\*/", "", m.group(2), flags=re.S).split())
        out[m.group(3)] = out[m.group(1)] = body
    return out


def defines(txt):
    return {m.group(1): m.group(2).strip()
            for m in re.finditer(r"(?m)^\s*#\s*define\s+(\w+)\s+(.*?)\s*(?://.*|/\*.*)?$", txt)}


def load(tree, hdrs):
    s, d = {}, {}
    for h in hdrs:
        p = os.path.join(tree, h)
        if os.path.exists(p):
            t = open(p, errors="ignore").read()
            s.update(structs(t))
            d.update(defines(t))
    return s, d


def compare(ref, cur, used):
    ok = lambda n: used is None or n in used
    return ([n for n in ref[0] if n in cur[0] and ok(n) and ref[0][n] != cur[0][n]],
            [n for n in ref[0] if ok(n) and n not in cur[0]],
            [n for n in ref[1] if n in cur[1] and ok(n) and ref[1][n] != cur[1][n]],
            [n for n in ref[1] if ok(n) and n not in cur[1]])


def selftest():
    a = "typedef struct P { NvU32 x; } P;\n#define K 1\n"
    b = "typedef struct P { NvU32 x; NvU32 y; } P;\n#define K 2\n"
    ra, rb = (structs(a), defines(a)), (structs(b), defines(b))
    ds, ms, dd, md = compare(ra, rb, {"P", "K"})
    r1 = ds == ["P"] and dd == ["K"] and not ms and not md
    r2 = compare(ra, rb, {"Other"}) == ([], [], [], [])
    print(("ok   " if r1 else "FAIL ") + "a changed struct and a changed define are both reported")
    print(("ok   " if r2 else "FAIL ") + "a difference in a name the backend does not use is not reported")
    return 0 if r1 and r2 else 1


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        sys.exit(selftest())
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    srcdir, hlist, ref_tree, trees = args[0], args[1], args[2], args[3:]
    code = "".join(open(f, errors="ignore").read() for f in glob.glob(srcdir + "/*.c") + glob.glob(srcdir + "/*.h"))
    used = None if "--all" in sys.argv else set(IDENT.findall(code))
    hdrs = [l.strip() for l in open(hlist) if l.strip()]
    ref = load(ref_tree, hdrs)
    bad = 0
    print("reference %s, %d headers, %s" % (ref_tree, len(hdrs),
          "every name" if used is None else "%d names the backend uses" % len(used)))
    for t in trees:
        ds, ms, dd, md = compare(ref, load(t, hdrs), used)
        bad += len(ds) + len(dd) + len(ms) + len(md)
        print("== %s: structs differing %d, structs missing %d, defines differing %d, defines missing %d"
              % (t, len(ds), len(ms), len(dd), len(md)))
        for lab, l in (("struct differs", ds), ("struct missing", ms), ("define differs", dd), ("define missing", md)):
            for n in l[:40]:
                print("   %s: %s" % (lab, n))
    sys.exit(1 if bad else 0)
