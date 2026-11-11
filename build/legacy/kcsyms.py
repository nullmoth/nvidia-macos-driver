#!/usr/bin/env python3
# kcsyms.py <kernel collection> <out dir>: list the kexts inside an x86_64 kernel collection and write each one's
# exported (defined, external) symbols to <out dir>/<bundle id>.syms. Reads the Mach-O fileset by hand.
import struct, sys, os, subprocess, tempfile
kc, out = sys.argv[1], sys.argv[2]; os.makedirs(out, exist_ok=True)
# thin the collection to x86_64 first if it is a fat file
head = open(kc, "rb").read(8)
if head[:4] in (b"\xca\xfe\xba\xbe", b"\xca\xfe\xba\xbf"):
    t = tempfile.mktemp(suffix=".kc"); rc = subprocess.run(["lipo", kc, "-thin", "x86_64", "-output", t]).returncode
    if rc != 0: print("STOP: no x86_64 slice in", kc); sys.exit(2)
    kc = t
d = open(kc, "rb").read()
magic, cpu, sub, ftype, ncmds, szcmds, flags, res = struct.unpack_from("<IiiIIIII", d, 0)
if magic != 0xfeedfacf: print("STOP: not a 64-bit Mach-O:", hex(magic)); sys.exit(2)
LC_FILESET_ENTRY = 0x80000035; LC_SYMTAB = 0x2; LC_SEGMENT_64 = 0x19
def cmds(off):
    m, c, s, ft, n, sz, fl, r = struct.unpack_from("<IiiIIIII", d, off); p = off + 32
    for _ in range(n):
        cmd, size = struct.unpack_from("<II", d, p); yield cmd, p, size; p += size
entries = []
for cmd, p, size in cmds(0):
    if cmd == LC_FILESET_ENTRY:
        vmaddr, fileoff, stroff, _ = struct.unpack_from("<QQII", d, p + 8)
        name = d[p + stroff:d.index(b"\0", p + stroff)].decode()
        entries.append((name, fileoff))
print(f"{os.path.basename(sys.argv[1])}: file type {ftype}, {len(entries)} kexts in the collection")
total = 0
for name, off in entries:
    syms = set()
    for cmd, p, size in cmds(off):
        if cmd == LC_SYMTAB:
            symoff, nsyms, stroff, strsize = struct.unpack_from("<IIII", d, p + 8)
            for i in range(nsyms):
                strx, ntype, nsect, ndesc, value = struct.unpack_from("<IBBHQ", d, symoff + 16 * i)
                if ntype & 0x01 and (ntype & 0x0e) != 0 and strx < strsize:   # external and defined
                    e = d.index(b"\0", stroff + strx); syms.add(d[stroff + strx:e].decode("latin1"))
    open(os.path.join(out, name + ".syms"), "w").write("\n".join(sorted(syms)) + "\n"); total += len(syms)
print(f"wrote {len(entries)} symbol lists, {total} symbols; accelerator family present: {any('IOAcceleratorFamily2' in n for n, _ in entries)}")
for n, _ in entries:
    if any(k in n for k in ("IOAccelerator", "IOGraphics", "IOSurface", "AppleGraphicsControl", "IOACPIFamily", "com.apple.kernel")):
        print("   ", n, sum(1 for _ in open(os.path.join(out, n + ".syms"))))
