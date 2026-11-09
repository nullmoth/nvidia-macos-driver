#!/usr/bin/env python3
"""Compare two NVK dylibs byte for byte and say what differs.

usage: nvk_compare.py <a.dylib> <b.dylib>

A linker derives LC_UUID from the pre-strip file, which records the absolute path of every object file, so two
builds of identical code in different directories differ in exactly the 16 UUID bytes. A package signature adds
a code-signature load command and a larger __LINKEDIT (removing it leaves a couple of load command bytes changed). Both are reported by name; any other
difference is reported as code."""
import struct, sys

def lc_end(b):
    return 32 + struct.unpack_from('<I', b, 20)[0]


def uuid_range(b):
    magic, = struct.unpack_from('<I', b, 0)
    assert magic == 0xfeedfacf, 'not a 64-bit Mach-O'
    ncmds, = struct.unpack_from('<I', b, 16)
    off = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', b, off)
        if cmd == 0x1b:           # LC_UUID
            return off + 8, off + 24
        off += size
    return None

a, b = (open(p, 'rb').read() for p in sys.argv[1:3])
if len(a) != len(b):
    print(f'sizes differ: {len(a)} vs {len(b)} (a signed file is larger; remove the signature first: codesign --remove-signature)')
    sys.exit(1)
ur = uuid_range(a)
diff = [i for i in range(len(a)) if a[i] != b[i]]
uuid = [i for i in diff if ur and ur[0] <= i < ur[1]]
hdr = [i for i in diff if i < lc_end(a) and i not in uuid]   # load commands: signature leftovers
code = [i for i in diff if i not in uuid and i not in hdr]
print(f'{len(diff)} bytes differ: {len(uuid)} in LC_UUID, {len(hdr)} in the load commands (signature), {len(code)} elsewhere')
sys.exit(0 if not code else 1)
