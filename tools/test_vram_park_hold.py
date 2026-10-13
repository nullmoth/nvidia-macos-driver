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


def run(unmaps_parked, requests=20, size=0x40000, console=(0, 0x870000)):
    alloc, granted, refused = Allocator(console), 0, 0
    for _ in range(requests):
        for _attempt in range(8):
            a = alloc.map(size)
            if a < console[0] + console[1] and a + size > console[0]:
                if unmaps_parked:
                    alloc.unmap(a, size)
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
print('PASS: parked VRAM keeps its BAR1 mapping; requests are granted once the console range is held')
