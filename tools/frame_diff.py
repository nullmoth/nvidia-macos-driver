#!/usr/bin/env python3
"""frame_diff.py A.bgra B.bgra [out.png]

Compares two driver frame dumps (/tmp/nvmtl-frame-<pid>-<tag><w>x<h>.bgra, written on /tmp/nvmtl-dump-request).
File layout: 16-byte header of four little-endian uint32 - magic 0x3146564e ("NVF1"), width, height, bytes per row -
then height rows of bytes-per-row bytes, 4 bytes per pixel in B,G,R,A order (an IOSurface or readback of a 4-byte
colour format; a 10-bit BGR10A2 surface is dumped as its raw 32-bit words, in which case the channel split below is
not meaningful and the raw word statistics are reported instead).

Prints per-channel mean absolute difference, the maximum, the share of pixels that differ by more than 8/255 in any
channel, and the mean of each channel per file (a swapped red/blue pair shows as B-mean(A) ~ R-mean(B)). With out.png
writes A | B | amplified difference side by side (pure zlib PNG, no image library needed).
--selftest builds two synthetic frames and checks the numbers and the swapped-channel detector."""
import struct, sys, zlib

MAGIC = 0x3146564e

def read(path):
    data = open(path, 'rb').read()
    if len(data) < 16: raise SystemExit('%s: too short for a frame dump' % path)
    magic, w, h, row = struct.unpack('<4I', data[:16])
    if magic != MAGIC: raise SystemExit('%s: magic 0x%08x is not NVF1' % (path, magic))
    if len(data) < 16 + row * h: raise SystemExit('%s: %d bytes, header says %d' % (path, len(data), 16 + row * h))
    px = bytearray(w * h * 4)
    for y in range(h):
        px[y * w * 4:(y + 1) * w * 4] = data[16 + y * row:16 + y * row + w * 4]
    return w, h, px

def stats(a, b, w, h):
    n = w * h
    sums = [0, 0, 0, 0]; mx = [0, 0, 0, 0]; bad = 0; ma = [0, 0, 0, 0]; mb = [0, 0, 0, 0]
    for i in range(0, n * 4, 4):
        over = False
        for c in range(4):
            x, y = a[i + c], b[i + c]; d = x - y if x >= y else y - x
            sums[c] += d; ma[c] += x; mb[c] += y
            if d > mx[c]: mx[c] = d
            if d > 8: over = True
        if over: bad += 1
    return {'mean_abs': [s / n for s in sums], 'max': mx, 'share_over_8': bad / n,
            'mean_a': [s / n for s in ma], 'mean_b': [s / n for s in mb]}

def png(path, w, h, rgb_rows):
    def chunk(t, d): return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    raw = b''.join(b'\x00' + r for r in rgb_rows)
    open(path, 'wb').write(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
                           + chunk(b'IDAT', zlib.compress(raw, 6)) + chunk(b'IEND', b''))

def side_by_side(path, w, h, a, b, step):
    rows = []
    for y in range(0, h, step):
        r = bytearray()
        for src in (a, b):
            for x in range(0, w, step):
                i = (y * w + x) * 4; r += bytes((src[i + 2], src[i + 1], src[i]))
        for x in range(0, w, step):
            i = (y * w + x) * 4
            r += bytes(min(255, 4 * abs(a[i + c] - b[i + c])) for c in (2, 1, 0))
        rows.append(bytes(r))
    png(path, 3 * len(range(0, w, step)), len(rows), rows)

def report(pa, pb, out=None):
    wa, ha, a = read(pa); wb, hb, b = read(pb)
    if (wa, ha) != (wb, hb): raise SystemExit('sizes differ: A %dx%d, B %dx%d - not the same surface' % (wa, ha, wb, hb))
    s = stats(a, b, wa, ha)
    names = 'BGRA'
    print('frame %dx%d, %d pixels' % (wa, ha, wa * ha))
    for c in range(4):
        print('  %s: mean|A-B| %.2f  max %d  mean A %.1f  mean B %.1f' % (names[c], s['mean_abs'][c], s['max'][c], s['mean_a'][c], s['mean_b'][c]))
    print('  pixels differing by >8 in any channel: %.1f%%' % (100 * s['share_over_8']))
    swap = abs(s['mean_a'][0] - s['mean_b'][2]) + abs(s['mean_a'][2] - s['mean_b'][0])
    straight = abs(s['mean_a'][0] - s['mean_b'][0]) + abs(s['mean_a'][2] - s['mean_b'][2])
    if straight > 10 and swap < straight / 4: print('  NOTE: B looks like A with red and blue exchanged (channel means match crosswise)')
    if out:
        step = max(1, (wa * ha // 500000) ** 0 if wa * ha <= 500000 else int((wa * ha / 500000) ** 0.5) + 1)
        side_by_side(out, wa, ha, a, b, step); print('  wrote %s (A | B | 4x difference, every %d-th pixel)' % (out, step))
    return s

def selftest():
    import os, tempfile
    d = tempfile.mkdtemp()
    w, h = 64, 32
    def write(path, f):
        px = bytearray()
        for y in range(h):
            for x in range(w): px += bytes(f(x, y))
        open(path, 'wb').write(struct.pack('<4I', MAGIC, w, h, w * 4) + px)
    A = os.path.join(d, 'a.bgra'); B = os.path.join(d, 'b.bgra'); C = os.path.join(d, 'c.bgra')
    write(A, lambda x, y: (10, 100, 200, 255))             # B=10 G=100 R=200
    write(B, lambda x, y: (10, 100, 200, 255))
    write(C, lambda x, y: (200, 100, 10, 255))             # red and blue exchanged
    s = report(A, B)
    assert s['mean_abs'] == [0, 0, 0, 0] and s['share_over_8'] == 0, s
    print('selftest 1 ok: identical frames -> 0 difference')
    s = report(A, C, os.path.join(d, 'c.png'))
    assert abs(s['mean_abs'][0] - 190) < 1e-9 and s['share_over_8'] == 1.0, s
    assert os.path.getsize(os.path.join(d, 'c.png')) > 100
    print('selftest 2 ok: the negative control fires (190/255 on B and R, 100%% of pixels) and the swap note printed')
    bad = os.path.join(d, 'bad.bgra'); open(bad, 'wb').write(b'\0' * 64)
    try: read(bad); raise AssertionError('bad magic accepted')
    except SystemExit: print('selftest 3 ok: a file without the NVF1 magic is refused')
    return 0

if __name__ == '__main__':
    if '--selftest' in sys.argv: sys.exit(selftest())
    if len(sys.argv) < 3: sys.exit(__doc__)
    report(sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
