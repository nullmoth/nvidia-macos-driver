#!/usr/bin/env python3
"""Replay the VRAM park loop: an allocation over the console is parked and keeps its BAR1 mapping, so the allocator
cannot hand the same BAR1 address back. Unmapping a parked block returns its address to the free pool; one machine then
logged the same address on every attempt (600 times) and every surface request was refused (black screen, cursor)."""
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
start = source.index('bool NVRMNVDAFramebuffer::vramGrant(')
grant = source[start:source.index('mem = m; kva = k; phys = p;', start)]
park = grant[grant.index('if (gNParkedForever <'):grant.index('} else {', grant.index('if (gNParkedForever <'))]


class Allocator:
    """BAR1 address space as NVKMS hands it out: lowest free range first; an unmapped range is free again."""

    def __init__(self, console):
        self.console = console
        self.mapped = []

    def map(self, size):
        addr = 0
        for a, n in sorted(self.mapped):
            if addr + size <= a:
                break
            addr = max(addr, a + n)
        self.mapped.append((addr, size))
        return addr

    def unmap(self, addr, size):
        self.mapped.remove((addr, size))


def run(unmaps_parked, requests=20, size=0x40000, console=(0, 0x870000), ceiling=None, guard=False):
    """unmaps_parked: 1.15 (a parked block's address is free again). ceiling: requests larger than the park ceiling are
    released, not parked. guard: such a request is replaced by a block covering only the overlap (2 MB aligned)."""
    alloc, granted, refused, parked = Allocator(console), 0, 0, 0
    cend = console[0] + console[1]
    for _ in range(requests):
        for _attempt in range(8):
            a = alloc.map(size)
            if a < cend and a + size > console[0]:
                if unmaps_parked:
                    alloc.unmap(a, size)
                elif ceiling is not None and parked + size > ceiling:
                    alloc.unmap(a, size)
                    g = ((cend - a) + 0x1FFFFF) & ~0x1FFFFF
                    if guard and g < size and parked + g <= ceiling:
                        ga = alloc.map(g)
                        if ga < cend and ga + g > console[0]:
                            parked += g
                        else:
                            alloc.unmap(ga, g)
                else:
                    parked += size
                continue
            granted += 1
            break
        else:
            refused += 1
    return granted, refused

holds = 'unmapMemory' not in park and '.k = k;' in park
counts = '+ gVramParkedBytes;' in grant
print('park branch keeps the mapping:', holds, '| parked bytes count against the budget:', counts)
granted, refused = run(unmaps_parked=not holds)
print(f'replay of 20 surface requests over an 8.8 MB console: {granted} granted, {refused} refused')
# the model itself: unmapping parked blocks grants nothing, holding them grants every request once the console is covered
assert run(unmaps_parked=True)[0] == 0 and run(unmaps_parked=False)[0] >= 15
assert holds and counts, 'parked blocks must stay mapped and count against the BAR1 budget'
assert granted >= 15, 'requests must be granted once the console range is held'
# A desktop surface bigger than the park ceiling (RTX 2070 Super report: 256 MB BAR1 -> 32 MB ceiling, 1280x1024 boot
# console = 8 MB at the start of BAR1, 3840x2560 surfaces = 39,321,600 bytes, the same address logged 432-752 times).
big = dict(requests=4, size=(39321600 + 0xFFFF) & ~0xFFFF, console=(0, 8388608), ceiling=32 << 20)
guarded = 'holding a guard block over the overlap instead' in grant and 'gp.size = g;' in grant
print('source holds a guard block when a surface is too big to park:', guarded)
capped_only, _ = run(unmaps_parked=False, **big)
with_guard, _ = run(unmaps_parked=False, guard=True, **big)
print(f'replay of 4 3840x2560 surfaces over an 8 MB console, 32 MB ceiling: capped park {capped_only} granted, '
      f'with guard {with_guard} granted')
assert capped_only == 0, 'negative control: parking only what fits under the ceiling must loop on this machine'
assert guarded and with_guard == 4, 'surfaces larger than the park ceiling must be granted once a guard holds the overlap'
nophys = grant[grant.index('if (!p) {'):grant.index('if (onScanout || onConsole) {')] if 'if (!p) {' in grant else ''
print('a block with no physical address is released, not parked:', bool(nophys) and 'freeMemory' in nophys and 'break;' in nophys
      and 'gParkedForever' not in nophys)
assert nophys and 'freeMemory' in nophys and 'break;' in nophys and 'gParkedForever' not in nophys, (
    'a block with no physical address (RM mapping table full: RTX 3050 / RTX 5070 at ~20 min) must be released and the '
    'request refused - parking it held one more mapping per attempt')
print('PASS: parked VRAM keeps its BAR1 mapping; requests are granted once the console range is held, '
      'including surfaces larger than the park ceiling')
