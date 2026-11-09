#!/usr/bin/env python3
"""List the code relocations of a CUDA cubin (an ELF64 file) with the bytes they patch.

usage: cubin_relocs.py <cubin>...
Reads only the ELF format (System V ABI): section headers, symbol tables and SHT_REL / SHT_RELA records. Used on cubins
that NullMoth's own PTX produced, to record which relocation kinds exist and which instruction bytes they patch."""
import struct
import sys


def rd(b):
    shoff, = struct.unpack_from("<Q", b, 0x28)
    shentsize, shnum, shstrndx = struct.unpack_from("<HHH", b, 0x3A)
    sh = []
    for i in range(shnum):
        n, t, fl, ad, off, sz, lk, inf, al, es = struct.unpack_from("<IIQQQQIIQQ", b, shoff + i * shentsize)
        sh.append(dict(n=n, t=t, ad=ad, off=off, sz=sz, lk=lk, inf=inf, es=es))
    so = sh[shstrndx]["off"]
    for s in sh:
        e = b.index(b"\0", so + s["n"])
        s["name"] = b[so + s["n"]:e].decode()
    return sh


def main():
    for path in sys.argv[1:]:
        b = open(path, "rb").read()
        sh = rd(b)
        print("==", path)
        for r in sh:
            if r["name"].startswith(".nv.constant0"):
                print("%s size=0x%x   (0x380 + parameter bytes on sm_100 and later, 0x160 + parameter bytes before sm_90)" % (r["name"], r["sz"]))
        for r in sh:
            if r["t"] not in (4, 9) or not r["sz"]:
                continue
            tgt, sym = sh[r["inf"]], sh[r["lk"]]
            strt = sh[sym["lk"]]
            esz = 24 if r["t"] == 4 else 16
            for k in range(r["sz"] // esz):
                rec = b[r["off"] + k * esz:r["off"] + (k + 1) * esz]
                off, info = struct.unpack_from("<QQ", rec)
                add = struct.unpack_from("<q", rec, 16)[0] if r["t"] == 4 else None
                si, ty = info >> 32, info & 0xffffffff
                nm, _, _, shndx, val, _ = struct.unpack_from("<IBBHQQ", b, sym["off"] + si * 24)
                name = b[strt["off"] + nm:b.index(b"\0", strt["off"] + nm)].decode()
                code = tgt["name"].startswith(".text")
                ins = b[tgt["off"] + off:tgt["off"] + off + 16].hex() if code else ""
                print("%-34s %s off=0x%x type=%d sym=%s(shndx=%d val=0x%x) addend=%s ins=%s" %
                      (tgt["name"], "RELA" if add is not None else "REL ", off, ty, name, shndx, val, add, ins))


main()
