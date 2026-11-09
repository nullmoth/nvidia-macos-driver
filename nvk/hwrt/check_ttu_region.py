#!/usr/bin/env python3
"""check_ttu_region.py <sass.bin> | --selftest

Judges the hardware-query region in a NAK binary against the field table in docs/HWRT.md (section "Query region"),
word by word: opcode order, the TTUOPEN fresh bit, the TTUCCTL length, the five store offsets with their register
pairs, TTUGO, the six load offsets with their pairs and scoreboards, the closing bit on the last load only, and the
drain that waits on all six scoreboards. The table was measured on an RTX 5060 (32 of 32 rays exact against the CPU
reference, nvk/hwrt/check_ttu_abi.py); nothing here is a copy of another compiler's output.

--selftest builds a correct region from the table, requires the judge to accept it, then breaks one field at a time
and requires the judge to name each break.
"""
import struct
import sys

TTUOPEN, TTUCCTL, TTUST, TTUGO, TTULD, DRAIN = 0x3d0, 0x9d4, 0x3d1, 0x3d3, 0x3d2, 0x918
STORE_OFFSETS = [0, 64, 80, 128, 512]
LOAD_OFFSETS = [896, 960, 976, 800, 784, 768]
TTU_OPS = {TTUOPEN, TTUCCTL, TTUST, TTUGO, TTULD}


def field(lo, hi, a, b):
    v = (hi << 64) | lo
    return (v >> a) & ((1 << (b - a)) - 1)


def load(path):
    b = open(path, "rb").read()
    return [struct.unpack_from("<QQ", b, o) for o in range(0, len(b), 16)]


def region(words):
    idx = [i for i, (lo, _) in enumerate(words) if lo & 0xfff in TTU_OPS]
    if len(idx) != 14 or idx != list(range(idx[0], idx[0] + 14)):
        return None, "expected one contiguous run of 14 query words, found %s" % idx
    if idx[0] + 15 > len(words):
        return None, "the drain is missing"
    return words[idx[0]:idx[0] + 15], None


def judge(reg):
    """Returns a list of named failures; empty means the region matches the table."""
    bad = []
    ops = [lo & 0xfff for lo, _ in reg]
    want = [TTUOPEN, TTUCCTL] + [TTUST] * 5 + [TTUGO] + [TTULD] * 6 + [DRAIN]
    if ops != want:
        bad.append("opcode order %s" % [hex(o) for o in ops])
        return bad
    lo, hi = reg[0]
    if field(lo, hi, 72, 73) != 1:
        bad.append("TTUOPEN fresh-traversal bit not set")
    lo, hi = reg[1]
    if field(lo, hi, 40, 56) != 12:
        bad.append("TTUCCTL length is not 12")
    for i, off in enumerate(STORE_OFFSETS):
        lo, hi = reg[2 + i]
        if field(lo, hi, 40, 56) != off:
            bad.append("store %d offset %d, want %d" % (i, field(lo, hi, 40, 56), off))
        a, c = field(lo, hi, 64, 72), field(lo, hi, 32, 40)
        if a % 2 or c != a + 2:
            bad.append("store %d registers R%d/R%d are not an aligned pair of pairs" % (i, a, c))
    for i, off in enumerate(LOAD_OFFSETS):
        lo, hi = reg[8 + i]
        if field(lo, hi, 40, 56) != off:
            bad.append("load %d offset %d, want %d" % (i, field(lo, hi, 40, 56), off))
        a, c = field(lo, hi, 16, 24), field(lo, hi, 64, 72)
        if a % 2 or c != a + 2:
            bad.append("load %d registers R%d/R%d are not an aligned pair of pairs" % (i, a, c))
        if field(lo, hi, 81, 84) != 7:
            bad.append("load %d type field is not 7" % i)
        if field(lo, hi, 74, 75) != (1 if i == 5 else 0):
            bad.append("load %d close bit wrong" % i)
        if field(lo, hi, 110, 113) != i:
            bad.append("load %d does not write scoreboard %d (writes %d)" % (i, i, field(lo, hi, 110, 113)))
    lo, hi = reg[14]
    if field(lo, hi, 116, 122) != 0x3f:
        bad.append("drain does not wait on all six scoreboards")
    return bad


def put(w, a, b, v):
    lo, hi = w
    x = (hi << 64) | lo
    m = ((1 << (b - a)) - 1) << a
    x = (x & ~m) | ((v << a) & m)
    return x & ((1 << 64) - 1), x >> 64


def build(first_reg=2):
    reg = []
    reg.append(put((TTUOPEN, 0), 72, 73, 1))
    reg.append(put(put((TTUCCTL, 0), 40, 56, 12)[0:2], 0, 0, 0))
    r = first_reg
    for off in STORE_OFFSETS:
        w = (TTUST, 0)
        w = put(w, 40, 56, off); w = put(w, 64, 72, r); w = put(w, 32, 40, r + 2)
        reg.append(w); r += 4
    reg.append((TTUGO, 0))
    for i, off in enumerate(LOAD_OFFSETS):
        w = (TTULD, 0)
        w = put(w, 40, 56, off); w = put(w, 16, 24, r); w = put(w, 64, 72, r + 2)
        w = put(w, 81, 84, 7); w = put(w, 74, 75, 1 if i == 5 else 0); w = put(w, 110, 113, i)
        reg.append(w); r += 4
    reg.append(put((DRAIN, 0), 116, 122, 0x3f))
    return reg


def selftest():
    fails = 0

    def arm(name, ok):
        nonlocal fails
        print(("ok   " if ok else "FAIL ") + name)
        fails += (not ok)

    good = build()
    arm("a region built from the table is accepted", judge(good) == [])
    muts = {
        "wrong store offset": (3, 40, 56, 96),
        "odd store register": (4, 64, 72, 5),
        "pair gap in a load": (9, 64, 72, 99),
        "missing fresh bit": (0, 72, 73, 0),
        "wrong TTUCCTL length": (1, 40, 56, 11),
        "close bit on a middle load": (9, 74, 75, 1),
        "close bit missing on the last load": (13, 74, 75, 0),
        "two loads share a scoreboard": (10, 110, 113, 0),
        "drain misses a scoreboard": (14, 116, 122, 0x1f),
        "wrong load offset": (12, 40, 56, 800),
    }
    for name, (i, a, b, v) in muts.items():
        reg = list(good)
        reg[i] = put(reg[i], a, b, v)
        arm("control: %s is named" % name, judge(reg) != [])
    reg = list(good)
    reg[7] = (TTUST, 0)
    arm("control: a swapped opcode is named", judge(reg) != [])
    print("%d arms failed" % fails)
    return 1 if fails else 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        sys.exit(selftest())
    reg, err = region(load(sys.argv[1]))
    if err:
        print("REGION_DIFF " + err)
        sys.exit(1)
    bad = judge(reg)
    for b in bad:
        print("DIFF " + b)
    print("REGION_MATCH" if not bad else "REGION_DIFF %d" % len(bad))
    sys.exit(1 if bad else 0)
