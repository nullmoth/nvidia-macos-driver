#!/usr/bin/env python3
# vtdump_kc.py <kernel collection> <bundle id> <out.json>: the same dump as vtdump.py (virtual tables in slot order,
# registered instance sizes) for one kext inside an x86_64 kernel collection, where every slot is already bound.
import struct, sys, re, json, bisect, subprocess, tempfile
kc, want, out = sys.argv[1], sys.argv[2], sys.argv[3]
boot = sys.argv[4] if len(sys.argv) > 4 else None
def thin(path):
    if open(path, "rb").read(4) in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
        t = tempfile.mktemp(suffix=".kc"); subprocess.run(["lipo", path, "-thin", "x86_64", "-output", t], check=True); return t
    return path
def symbols_of(path):
    """(base address, {address: name}) of every fileset entry in a collection"""
    b = open(thin(path), "rb").read(); names = {}
    def cm(off):
        n = struct.unpack_from("<I", b, off + 16)[0]; p = off + 32
        for _ in range(n):
            c, sz = struct.unpack_from("<II", b, p); yield c, p, sz; p += sz
    lo = None; ents = []
    for c, p, sz in cm(0):
        if c == 0x19:
            vm, vs = struct.unpack_from("<QQ", b, p + 24)
            if vs and (lo is None or vm < lo): lo = vm
        if c == 0x80000035: ents.append(struct.unpack_from("<QQII", b, p + 8)[1])
    for off in ents:
        for c, p, sz in cm(off):
            if c == 0x2:
                so, ns, st, ss = struct.unpack_from("<IIII", b, p + 8)
                for i in range(ns):
                    sx, nt, nsct, nd, val = struct.unpack_from("<IBBHQ", b, so + 16 * i)
                    if (nt & 0x0e) == 0 or nt & 0xe0 or sx >= ss: continue
                    nm = b[st + sx:b.index(b"\0", st + sx)].decode("latin1")
                    if nm: names.setdefault(val, nm)
    return lo, names
boot_base, boot_names = symbols_of(boot) if boot else (None, {})
if open(kc, "rb").read(4) in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
    t = tempfile.mktemp(suffix=".kc"); subprocess.run(["lipo", kc, "-thin", "x86_64", "-output", t], check=True); kc = t
d = open(kc, "rb").read()
def cmds(off):
    n = struct.unpack_from("<I", d, off + 16)[0]; p = off + 32
    for _ in range(n):
        cmd, size = struct.unpack_from("<II", d, p); yield cmd, p, size; p += size
def segs(off):
    r = []
    for cmd, p, size in cmds(off):
        if cmd == 0x19:
            name = d[p + 8:p + 24].split(b"\0")[0].decode(); vmaddr, vmsize, fileoff, filesize = struct.unpack_from("<QQQQ", d, p + 24)
            r.append((name, vmaddr, vmsize, fileoff, filesize))
    return r
top = segs(0); base = min(v for n, v, s, o, f in top if s)
entries = []
for cmd, p, size in cmds(0):
    if cmd == 0x80000035:
        vmaddr, fileoff, stroff, _ = struct.unpack_from("<QQII", d, p + 8)
        entries.append((d[p + stroff:d.index(b"\0", p + stroff)].decode(), fileoff))
name_of = {}; mine = []; myoff = None
for name, off in entries:
    for cmd, p, size in cmds(off):
        if cmd == 0x2:
            symoff, nsyms, stroff, strsize = struct.unpack_from("<IIII", d, p + 8)
            for i in range(nsyms):
                strx, ntype, nsect, ndesc, value = struct.unpack_from("<IBBHQ", d, symoff + 16 * i)
                if (ntype & 0x0e) == 0 or ntype & 0xe0 or strx >= strsize: continue      # undefined or debug entry
                s = d[stroff + strx:d.index(b"\0", stroff + strx)].decode("latin1")
                if not s: continue
                name_of.setdefault(value, s)
                if name == want: mine.append((value, s))
    if name == want: myoff = off
if myoff is None: print("STOP: no", want, "in", kc); sys.exit(2)
allsegs = top + [x for _, off in entries for x in segs(off)]
def fo(addr):
    for n, v, s, o, f in allsegs:
        if v <= addr < v + f: return o + (addr - v)
    return None
mine.sort(); maddr = [a for a, _ in mine]; res = {}; unnamed = 0; total = 0
for a, n in mine:
    m = re.fullmatch(r"__ZTV(\d+)(\w+)", n)
    if not m or len(m.group(2)) != int(m.group(1)): continue
    i = bisect.bisect_right(maddr, a); end = maddr[i] if i < len(maddr) else a + 8 * 4096
    slots = []; p = a + 16
    while p < end:
        o = fo(p)
        if o is None: break
        v = struct.unpack_from("<Q", d, o)[0]
        if v == 0: slots.append("?0")
        else:
            level = (v >> 30) & 3      # 0: bound into the boot collection, 1: into this collection
            if level == 0 and boot_base is not None: nm = boot_names.get(boot_base + (v & 0x3FFFFFFF))
            elif level == 1: nm = name_of.get(base + (v & 0x3FFFFFFF))
            else: nm = None
            slots.append(nm if nm else "@"); unnamed += nm is None
        total += 1; p += 8
    res[m.group(2)] = {"slots": slots}
for a, n in mine:
    m = re.fullmatch(r"__ZN(\d+)(\w+?)9MetaClassC[12]Ev", n)
    if not m or m.group(2) not in res or "size" in res[m.group(2)]: continue
    o = fo(a)
    if o is None: continue
    code = d[o:o + 96]; last = None; i = 0
    while i < len(code) - 5:
        if code[i] == 0xB9: last = struct.unpack_from("<I", code, i + 1)[0]; i += 5; continue
        if code[i] in (0xE8, 0xE9) and last is not None: res[m.group(2)]["size"] = last; break
        i += 1
json.dump(res, open(out, "w"), indent=0)
print(f"{want}: {len(res)} classes with a vtable, {sum(1 for c in res.values() if 'size' in c)} with a size, {total} slots read, {unnamed} bound to a function with no symbol")
