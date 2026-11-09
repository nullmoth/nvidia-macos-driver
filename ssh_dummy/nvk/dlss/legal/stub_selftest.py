#!/usr/bin/env python3
"""usage: stub_selftest.py <mesa tree>   -- checks the __assertfail stand-in words produced by nak_stub.py.

Arms (named sentences) are printed as they run; a negative arm proves a changed encoder source is refused."""
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import nak_stub  # noqa: E402

tree = sys.argv[1]
fails = 0


def check(name, ok):
    global fails
    print(("ok   " if ok else "FAIL ") + name)
    fails += 0 if ok else 1


def bits(lo, hi, a, b):
    return ((lo | (hi << 64)) >> a) & ((1 << (b - a)) - 1)


for sm in (75, 80, 89):
    stl, sth, exl, exh = nak_stub.nak_words(tree, sm)
    check("sm%d: the EXIT word equals the one the cache linker has always shipped" % sm,
          (exl, exh) == (0x794d, 0x000fea0003800000))
    check("sm%d: the store is opcode 0x986 and reads address zero (RZ + URZ, offset 0)" % sm,
          bits(stl, sth, 0, 12) == 0x986 and bits(stl, sth, 24, 32) == 255 and bits(stl, sth, 64, 70) == 63 and
          bits(stl, sth, 40, 64) == 0 and bits(stl, sth, 72, 73) == 1 and bits(stl, sth, 90, 91) == 0)
    check("sm%d: the store is unpredicated and 32 bits wide" % sm,
          bits(stl, sth, 12, 16) == 7 and bits(stl, sth, 73, 76) == 4)
check("pre-SM80 and SM80+ differ only in the memory-ordering field",
      (nak_stub.nak_words(tree, 75)[1] ^ nak_stub.nak_words(tree, 80)[1]) == (1 << (79 - 64)))

d = tempfile.mkdtemp()
try:
    os.makedirs(d + "/src/nouveau/compiler/nak")
    s = open(tree + "/src/nouveau/compiler/nak/sm70_encode.rs").read()
    assert s.count("e.set_opcode(0x94d);") == 1
    open(d + "/src/nouveau/compiler/nak/sm70_encode.rs", "w").write(s.replace("e.set_opcode(0x94d);", "e.set_opcode(0x94e);"))
    try:
        nak_stub.nak_words(d, 80)
        refused = False
    except AssertionError:
        refused = True
    check("a changed EXIT opcode in the encoder source is refused", refused)
finally:
    shutil.rmtree(d)
sys.exit(1 if fails else 0)
