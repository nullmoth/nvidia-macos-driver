#!/usr/bin/env python3
"""hwrt_layout_check.py <dump dir> | --selftest

Judges recorded bottom-level structures against the MEASURED rows of the layout table in docs/HWRT.md. Every row is one
sentence, printed PASS / FAIL / UNMEASURED. The dump directory holds files named blas_<case>.bin as captured by the procedure
in docs/HWRT.md ("Capture"): blas_A (one triangle (0,0,0) (1,0,0) (0,1,0)), blas_B (the same shifted +8 in x), blas_n02 ..
blas_n64 (that many triangles on a strip), blas_poke_<v><axis> (A with one vertex component moved by +32). A row whose files
are missing is UNMEASURED, never PASS. Exit status 1 on any FAIL.

--selftest builds synthetic dumps that satisfy every row, requires all PASS, then breaks one field at a time and requires the
matching row to FAIL.
"""
import glob
import os
import struct
import sys

SIZES = {"A": 2560, "B": 2560, "n02": 2560, "n04": 2560, "n08": 2560, "n16": 3584, "n32": 3712, "n64": 5632}
PRIMS = {"A": 1, "B": 1, "n02": 2, "n04": 4, "n08": 8, "n16": 16, "n32": 32, "n64": 64}
SECTION = {"n16": 0x800, "n32": 0x880, "n64": 0xA00}
UNMEASURED = [
    ("R20", "the leaf packing of vertices 1 and 2 (bit widths, scale exponent at 0x47c)"),
    ("R21", "the 8-byte child records at 0x380 (pointer or index, quantised bounds)"),
    ("R22", "the quantisation frame at 0x180..0x37f"),
    ("R23", "a top-level structure: header, instance record, transform, mask and flag fields"),
    ("R24", "ray flags other than the instance mask in the query request"),
    ("R25", "candidate reporting for non-opaque triangles and for boxes"),
    ("R26", "a query that starts at a bottom-level root"),
    ("R27", "inclusive or exclusive tmin and tmax"),
    ("R28", "the facing bit under a mirrored instance transform"),
    ("R29", "whether the hardware reads the self address at 0xd0"),
]


def u32(b, o):
    return struct.unpack_from("<I", b, o)[0]


def u64(b, o):
    return struct.unpack_from("<Q", b, o)[0]


def f32(b, o):
    return struct.unpack_from("<f", b, o)[0]


def load(d):
    return {os.path.basename(p)[5:-4]: open(p, "rb").read() for p in glob.glob(os.path.join(d, "blas_*.bin"))}


def rows(dumps):
    """Yields (row id, sentence, verdict, detail); verdict is PASS, FAIL or UNMEASURED."""
    base = {k: v for k, v in dumps.items() if k in SIZES}

    def each(rid, sentence, fn):
        if not base:
            return (rid, sentence, "UNMEASURED", "no dumps")
        bad = [k for k, b in sorted(base.items()) if not fn(k, b)]
        return (rid, sentence, "FAIL" if bad else "PASS", ("failed on " + ",".join(bad)) if bad else "%d dumps" % len(base))

    out = [
        each("R01", "word 0 of a bottom-level structure is 0x00100003", lambda k, b: u32(b, 0) == 0x00100003),
        each("R02", "word 1 is 7 for a prefer-fast-trace build", lambda k, b: u32(b, 4) == 7),
        each("R03", "word 2 is the total size in bytes and equals the dump length", lambda k, b: u32(b, 8) == len(b)),
        each("R04", "the u64 at 0x20 is 0x380: the root node sits at a fixed offset", lambda k, b: u64(b, 0x20) == 0x380),
        each("R05", "the u64 at 0x40 is all ones", lambda k, b: u64(b, 0x40) == 0xFFFFFFFFFFFFFFFF),
        each("R06", "the u32 at 0x48 is 0x180", lambda k, b: u32(b, 0x48) == 0x180),
        each("R07", "the u32 at 0x58 is the geometry count (1: one geometry was passed)", lambda k, b: u32(b, 0x58) == 1),
        each("R08", "the u32 at 0x60 is the primitive count", lambda k, b: u32(b, 0x60) == PRIMS[k]),
        each("R09", "the u64 at 0xd0 is the structure's own 256-aligned device address (nonzero, low 8 bits clear)",
             lambda k, b: u64(b, 0xD0) != 0 and u64(b, 0xD0) & 0xFF == 0),
        each("R10", "total sizes: 1 to 8 prims 2560, 16 prims 3584, 32 prims 3712, 64 prims 5632", lambda k, b: len(b) == SIZES[k]),
        each("R11", "the u32 at 0x10 is where the geometry section starts: 0x400 up to 8 prims, then 0x800, 0x880, 0xa00",
             lambda k, b: u32(b, 0x10) == SECTION.get(k, 0x400)),
    ]

    s12 = "vertex 0 is stored as three verbatim floats at the start of the geometry section"
    if "A" in dumps and all("poke_v0" + a in dumps for a in "xyz"):
        a = dumps["A"]
        sec = u32(a, 0x10)
        ok = True
        for i, ax in enumerate("xyz"):
            p = dumps["poke_v0" + ax]
            ok &= abs(f32(p, sec + 4 * i) - f32(a, sec + 4 * i) - 32.0) < 1e-3
            ok &= all(p[sec + 4 * j: sec + 4 * j + 4] == a[sec + 4 * j: sec + 4 * j + 4] for j in range(3) if j != i)
        out.append(("R12", s12, "PASS" if ok else "FAIL", ""))
    else:
        out.append(("R12", s12, "UNMEASURED", "needs blas_A and the three v0 pokes"))

    s13 = "the box maximum is three floats at 0x90 into the geometry section (a maximum over the vertices)"
    if "A" in dumps and "poke_v1x" in dumps and "poke_v0x" in dumps:
        a = dumps["A"]
        o = u32(a, 0x10) + 0x90
        ok = abs(f32(dumps["poke_v1x"], o) - f32(a, o) - 32.0) < 1e-3 and abs(f32(dumps["poke_v0x"], o) - f32(a, o) - 31.0) < 1e-3
        out.append(("R13", s13, "PASS" if ok else "FAIL", ""))
    else:
        out.append(("R13", s13, "UNMEASURED", "needs blas_A, poke_v1x, poke_v0x"))

    out += [(rid, s, "UNMEASURED", "see docs/HWRT.md procedure") for rid, s in UNMEASURED]
    return out


def main(d):
    fails = 0
    for rid, sentence, verdict, detail in rows(load(d)):
        print("%-10s %s  [%s]%s" % (verdict, sentence, rid, ("  " + detail) if detail else ""))
        fails += verdict == "FAIL"
    return 1 if fails else 0


def synth():
    d = {}
    for k, n in PRIMS.items():
        b = bytearray(SIZES[k])
        struct.pack_into("<I", b, 0, 0x00100003)
        struct.pack_into("<I", b, 4, 7)
        struct.pack_into("<I", b, 8, SIZES[k])
        struct.pack_into("<I", b, 0x10, SECTION.get(k, 0x400))
        struct.pack_into("<Q", b, 0x20, 0x380)
        struct.pack_into("<Q", b, 0x40, 0xFFFFFFFFFFFFFFFF)
        struct.pack_into("<I", b, 0x48, 0x180)
        struct.pack_into("<I", b, 0x58, 1)
        struct.pack_into("<I", b, 0x60, n)
        struct.pack_into("<Q", b, 0xD0, 0x5DE0000 + 0x10000 * n)
        d[k] = bytes(b)
    sec = 0x400
    a = bytearray(d["A"])
    struct.pack_into("<3f", a, sec, 0.0, 0.0, 0.0)
    struct.pack_into("<3f", a, sec + 0x90, 1.0, 1.0, 0.0)
    d["A"] = bytes(a)
    # vertices (0,0,0) (1,0,0) (0,1,0): the maximum is (1, 1, 0)
    for ax_i, ax in enumerate("xyz"):
        p = bytearray(d["A"])
        struct.pack_into("<f", p, sec + 4 * ax_i, 32.0)                      # v0 moves
        struct.pack_into("<f", p, sec + 0x90 + 4 * ax_i, (1.0, 1.0, 0.0)[ax_i] + (31.0 if ax == "x" else 32.0 if ax == "z" else 31.0))
        d["poke_v0" + ax] = bytes(p)
    p = bytearray(d["A"])
    struct.pack_into("<f", p, sec + 0x90, 33.0)                              # v1x was the maximum x
    d["poke_v1x"] = bytes(p)
    return d


def selftest():
    fails = 0

    def arm(name, ok):
        nonlocal fails
        print(("ok   " if ok else "FAIL ") + name)
        fails += not ok

    good = synth()
    got = {r[0]: r[2] for r in rows(good)}
    measured = ["R%02d" % i for i in range(1, 14)]
    arm("synthetic dumps satisfying the table pass every measured row", all(got[r] == "PASS" for r in measured))
    arm("rows nobody has measured are reported UNMEASURED, never PASS", all(got[r] == "UNMEASURED" for r in ("R20", "R21", "R22", "R23", "R27")))
    arm("an empty directory is all UNMEASURED", all(r[2] == "UNMEASURED" for r in rows({})))
    breaks = [("R01", 0, 0xDEADBEEF, "<I"), ("R03", 8, 77, "<I"), ("R04", 0x20, 0x400, "<Q"), ("R06", 0x48, 0x100, "<I"),
              ("R08", 0x60, 99, "<I"), ("R09", 0xD0, 0x5DE0001, "<Q"), ("R11", 0x10, 0x500, "<I")]
    for rid, off, val, fmt in breaks:
        m = dict(good)
        b = bytearray(m["n08"])
        struct.pack_into(fmt, b, off, val)
        m["n08"] = bytes(b)
        arm("control: breaking the field behind %s makes it FAIL" % rid, {r[0]: r[2] for r in rows(m)}[rid] == "FAIL")
    m = dict(good)
    b = bytearray(m["poke_v0x"])
    struct.pack_into("<f", b, 0x400, 5.0)
    m["poke_v0x"] = bytes(b)
    arm("control: a vertex-0 poke that moves the wrong float makes R12 FAIL", {r[0]: r[2] for r in rows(m)}["R12"] == "FAIL")
    print("%d arms failed" % fails)
    return 1 if fails else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        sys.exit(selftest())
    sys.exit(main(sys.argv[1]))
