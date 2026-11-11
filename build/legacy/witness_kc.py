#!/usr/bin/env python3
# witness_kc.py <reference kext> <kernel collection> <bundle id>: the witness-method audit (see witness.py) for a
# release whose accelerator family exists only inside a kernel collection. Function bytes are cut from the
# collection by symbol and disassembled with llvm-mc.
import subprocess, sys, re, collections, struct, tempfile, os, bisect
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import witness_pairs
MC = os.environ.get("LLVM_MC", "llvm-mc")
def ref_disp(path):
    out = subprocess.run(["otool", "-arch", "x86_64", "-tV", path], capture_output=True, text=True, errors="replace").stdout
    cur = None; d = {}
    for l in out.splitlines():
        if l.endswith(":") and not l.startswith("\t") and not l[0].isdigit(): cur = l[:-1]; d[cur] = collections.Counter(); continue
        if cur is None: continue
        for m in re.finditer(r"(0x[0-9a-f]+)\(%r(?!ip|sp|bp)[a-z0-9]+\)", l): d[cur][int(m.group(1), 16)] += 1
    return d
def kc_disp(kc, want, names):
    if open(kc, "rb").read(4) in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        t = tempfile.mktemp(suffix=".kc"); subprocess.run(["lipo", kc, "-thin", "x86_64", "-output", t], check=True); kc = t
    d = open(kc, "rb").read()
    def cmds(off):
        n = struct.unpack_from("<I", d, off + 16)[0]; p = off + 32
        for _ in range(n):
            c, sz = struct.unpack_from("<II", d, p); yield c, p, sz; p += sz
    ent = None
    for c, p, sz in cmds(0):
        if c == 0x80000035:
            vm, fo, so, _ = struct.unpack_from("<QQII", d, p + 8)
            if d[p + so:d.index(b"\0", p + so)].decode() == want: ent = fo
    if ent is None: raise SystemExit("no " + want)
    segs = []; syms = []
    for c, p, sz in cmds(ent):
        if c == 0x19:
            vm, vs, fo, fs = struct.unpack_from("<QQQQ", d, p + 24); segs.append((vm, fs, fo))
        if c == 0x2:
            so, ns, st, ss = struct.unpack_from("<IIII", d, p + 8)
            for i in range(ns):
                sx, nt, nsct, nd, val = struct.unpack_from("<IBBHQ", d, so + 16 * i)
                if (nt & 0x0e) == 0 or nt & 0xe0 or sx >= ss: continue
                syms.append((val, d[st + sx:d.index(b"\0", st + sx)].decode("latin1")))
    syms.sort(); addrs = [a for a, _ in syms]; res = {}
    def fo_of(a):
        for vm, fs, fo in segs:
            if vm <= a < vm + fs: return fo + (a - vm)
    for a, n in syms:
        if n not in names: continue
        i = bisect.bisect_right(addrs, a); end = addrs[i] if i < len(addrs) else a + 4096
        o = fo_of(a)
        if o is None or end - a > 65536: continue
        hexs = " ".join("0x%02x" % b for b in d[o:o + (end - a)])
        out = subprocess.run([MC, "--disassemble", "-triple=x86_64-apple-macos"], input=hexs, capture_output=True, text=True).stdout
        cnt = collections.Counter()
        for m in re.finditer(r"(?<![\w$])(\d+|0x[0-9a-f]+)\(%r(?!ip|sp|bp)[a-z0-9]+\)", out):
            v = m.group(1); cnt[int(v, 16) if v.startswith("0x") else int(v)] += 1
        res[n] = cnt
    return res
def cls(n):
    m = re.match(r"__ZNK?(\d+)", n); return n[m.end():m.end() + int(m.group(1))] if m else None
ref = ref_disp(sys.argv[1]); need = set()
for c, off in witness_pairs.PAIRS: need |= {n for n, v in ref.items() if cls(n) == c and off in v}
o = kc_disp(sys.argv[2], sys.argv[3], need)
print(f"witness methods wanted {len(need)}, found and disassembled in the collection {len(o)}")
print(f"{'class':28} {'offset':>7} {'witnesses on reference':>23} {'shared':>7} {'still use it':>13} {'identical use':>14}")
for c, off in witness_pairs.PAIRS:
    W = [n for n, v in ref.items() if cls(n) == c and off in v]; sh = [m for m in W if m in o]
    print(f"{c:28} {hex(off):>7} {len(W):>23} {len(sh):>7} {sum(1 for m in sh if off in o[m]):>13} {sum(1 for m in sh if o[m] == ref[m]):>14}")
