#!/usr/bin/env python3
"""Prepare an EDID override file for the framebuffer driver.

  nvfb-edid.py check  <file>            validate a file the way the driver does
  nvfb-edid.py fix    <in> <out>        recompute block checksums and the extension count
  nvfb-edid.py install <file>           copy to /Library/NullMoth/edid-override/<vendor>-<product>.bin (needs root)

The driver reads a file only when it is owned by root, not writable by group or others, a whole number of 128-byte
blocks, has the EDID header and valid checksums, and carries the manufacturer id and product code of the display it is
named for. Raw EDID dumps from a display (for example `ioreg -lw0 | grep IODisplayEDID`) are the usual starting point."""
import os, shutil, sys

HDR = bytes([0, 255, 255, 255, 255, 255, 255, 0])
DIR = '/Library/NullMoth/edid-override'


def valid(b):
    if len(b) < 128 or len(b) % 128 or b[:8] != HDR or len(b) // 128 != b[126] + 1:
        return False
    return all(sum(b[i:i + 128]) % 256 == 0 for i in range(0, len(b), 128))


def fix(b):
    b = bytearray(b)
    b[126] = len(b) // 128 - 1
    for i in range(0, len(b), 128):
        b[i + 127] = (-sum(b[i:i + 127])) % 256
    return bytes(b)


def name(b):
    return '%02X%02X-%02X%02X.bin' % (b[8], b[9], b[11], b[10])


def main(a):
    if len(a) >= 2 and a[0] == 'check':
        b = open(a[1], 'rb').read()
        print(('ok ' if valid(b) else 'INVALID ') + name(b) if len(b) >= 12 else 'INVALID (too short)')
        return 0 if valid(b) else 1
    if len(a) == 3 and a[0] == 'fix':
        b = open(a[1], 'rb').read()
        if len(b) % 128 or b[:8] != HDR:
            print('not a whole number of EDID blocks with an EDID header'); return 1
        open(a[2], 'wb').write(fix(b)); print('wrote', a[2], name(fix(b))); return 0
    if len(a) == 2 and a[0] == 'install':
        b = open(a[1], 'rb').read()
        if not valid(b):
            print('INVALID: run fix first'); return 1
        os.makedirs(DIR, mode=0o755, exist_ok=True)
        dest = os.path.join(DIR, name(b))
        shutil.copyfile(a[1], dest); os.chmod(dest, 0o644); os.chown(dest, 0, 0)
        print('installed', dest, '(takes effect at the next boot)'); return 0
    print(__doc__); return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
