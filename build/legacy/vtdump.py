#!/usr/bin/env python3
# vtdump.py <kext binary> <out.json>: for every C++ class the binary defines, the instance size its metaclass
# registers and its virtual table in slot order (own functions by name, inherited/imported ones by relocation name).
import subprocess, sys, re, json, struct
b, out = sys.argv[1], sys.argv[2]
def run(c): return subprocess.run(c, capture_output=True, text=True, errors="replace").stdout
syms = []   # (addr, name) defined
for l in run(["nm", "-arch", "x86_64", "-n", b]).splitlines():
    p = l.split()
    if len(p) == 3 and p[1] not in "Uu": syms.append((int(p[0], 16), p[2]))
syms.sort(); addrs = [a for a, _ in syms]
addr_of = {}
for a_, n_ in syms: addr_of.setdefault(n_, a_)
byaddr = {}
for a, n in syms: byaddr.setdefault(a, n)
# external relocations: address -> symbol
ext = {}
for l in run(["otool", "-arch", "x86_64", "-rv", b]).splitlines():
    p = l.split()
    if len(p) >= 7 and re.fullmatch(r"[0-9a-f]{8,16}", p[0]) and p[4] == "True" and p[5] == "UNSIGND": ext[int(p[0], 16)] = p[-1]
# section contents for reading slots
import bisect
segs = []
txt = run(["otool", "-arch", "x86_64", "-l", b]); cur = {}
for l in txt.splitlines():
    l = l.strip()
    m = re.match(r"(sectname|segname|addr|size|offset) (\S+)", l)
    if m:
        cur[m.group(1)] = m.group(2)
        if m.group(1) == "offset" and "sectname" in cur:
            segs.append((int(cur["addr"], 16), int(cur["size"], 16), int(cur["offset"]), cur["sectname"])); cur = {}
raw = open(b, "rb").read()
# thin offset for fat files
fat = run(["lipo", "-detailed_info", b]); base = 0
m = re.search(r"architecture x86_64.*?offset (\d+)", fat, re.S)
if m: base = int(m.group(1))
def rd(addr):
    for a, s, o, n in segs:
        if a <= addr < a + s: return struct.unpack_from("<Q", raw, base + o + (addr - a))[0]
    return None
res = {}
for a, n in syms:
    m = re.fullmatch(r"__ZTV(\d+)(\w+)", n)
    if not m or len(m.group(2)) != int(m.group(1)): continue
    cls = m.group(2); i = bisect.bisect_right(addrs, a); end = addrs[i] if i < len(addrs) else a + 8 * 4096
    slots = []; p = a + 16
    while p < end:
        v = rd(p)
        if p in ext: slots.append(ext[p])
        elif v: slots.append(byaddr.get(v, "@%x" % v))
        else: slots.append("?0")
        p += 8
    res[cls] = {"slots": slots}
# instance size: the immediate loaded into ecx before the OSMetaClass constructor call inside <cls>::MetaClass::MetaClass()
dis = run(["otool", "-arch", "x86_64", "-tV", b]); cur = None; last = None
for l in dis.splitlines():
    if l.endswith(":") and not l.startswith("\t"):
        m = re.fullmatch(r"__ZN(\d+)(\w+?)9MetaClassC[12]Ev:", l); cur = m.group(2) if m else None; last = None; continue
    if cur:
        m = re.search(r"movl\t\$0x([0-9a-f]+), %ecx", l)
        if m: last = int(m.group(1), 16)
        # the disassembler prints an immediate as a symbol when a symbol happens to sit at that address
        m = re.search(r"movl\t\$(_\w+), %ecx", l)
        if m and m.group(1) in addr_of: last = addr_of[m.group(1)]
        if "OSMetaClassC2EPKcPKS_j" in l and last is not None and cur in res and "size" not in res[cur]: res[cur]["size"] = last
# second source for the size: the metaclass alloc() passes it to operator new (some classes register through a
# shared helper, which the first reader does not see)
cur = None
for l in dis.splitlines():
    if l.endswith(":") and not l.startswith("\t"):
        m = re.fullmatch(r"__ZNK(\d+)(\w+?)9MetaClass5allocEv:", l); cur = m.group(2) if m else None; continue
    if cur and cur in res and "size" not in res[cur]:
        m = re.search(r"movl\t\$0x([0-9a-f]+), %edi", l)
        if m: res[cur]["size"] = int(m.group(1), 16)
json.dump(res, open(out, "w"), indent=0)
print(f"{b.split('/')[-1]}: {len(res)} classes with a vtable, {sum(1 for c in res.values() if 'size' in c)} with a size; relocations read: {len(ext)}")
