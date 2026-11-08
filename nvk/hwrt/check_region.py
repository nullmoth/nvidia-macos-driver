"""Compare the TTU region in a NAK binary with the GPU-proven reference region, register fields masked."""
import struct, sys
TTU = {0x3d0, 0x3d1, 0x3d2, 0x3d3, 0x9d4}
def region(path):
    b = open(path, 'rb').read()
    w = [struct.unpack_from('<QQ', b, o) for o in range(0, len(b), 16)]
    idx = [i for i, (lo, hi) in enumerate(w) if lo & 0xfff in TTU]
    assert len(idx) == 14 and idx == list(range(idx[0], idx[0] + 14)), idx
    return w[idx[0]:idx[0] + 15]          # 14 TTU words + the drain
def regs(i, lo, hi):
    op = lo & 0xfff
    if op == 0x3d1: return (hi & 0xff, (lo >> 32) & 0xff)          # lo pair 64..72, hi pair 32..40
    if op == 0x3d2: return ((lo >> 16) & 0xff, hi & 0xff)          # lo pair 16..24, hi pair 64..72
    return ()
def masked(lo, hi):
    op = lo & 0xfff
    if op == 0x3d1: lo &= ~(0xff << 32); hi &= ~0xff
    if op == 0x3d2: lo &= ~(0xff << 16); hi &= ~0xff
    hi &= ~(0x3f << 52)                                            # wait mask depends on the code before
    return lo, hi
ref, new = region(sys.argv[1]), region(sys.argv[2])
bad = 0
for i, ((rl, rh), (nl, nh)) in enumerate(zip(ref, new)):
    same = masked(rl, rh) == masked(nl, nh)
    r = regs(i, nl, nh)
    pair_ok = not r or (r[0] % 2 == 0 and r[1] == r[0] + 2)
    if not (same and pair_ok): bad += 1
    print(f"{i:2d} op={nl & 0xfff:#05x} ref={rl:016x}:{rh:016x} new={nl:016x}:{nh:016x} {'OK' if same and pair_ok else 'DIFF'} regs={r}")
dl, dh = new[14]
print('drain waits', hex((dh >> 52) & 0x3f), 'opcode', hex(dl & 0xfff))
assert dl & 0xfff == 0x918 and (dh >> 52) & 0x3f == 0x3f
print('REGION_MATCH' if bad == 0 else f'REGION_DIFF {bad}')
