#!/usr/bin/env python3
"""Reference decoder for BC4 and BC5 blocks (the D3D / Vulkan compressed-texture specification), for comparing a GPU's decode.

A BC4 block is 8 bytes: two 8-bit endpoints and sixteen 3-bit indices. With e0 > e1 the other six palette entries are
interpolated in sevenths of the span; otherwise in fifths, with the last two entries fixed at 0 and 1. The specification
leaves rounding of the interpolation to the hardware within a tolerance, so this decodes both exactly (rational) and with
the usual 8-bit rounding, and `--compare FILE` reports how far a captured decode (one byte per texel, row-major 4x4 per
block) is from each.

    bc45_reference.py --selftest
    bc45_reference.py --compare decoded.bin blocks.bin     # decoded.bin: 16 bytes per block; blocks.bin: 8 per block
"""
import struct
import sys
from fractions import Fraction


def palette(e0, e1):
    if e0 > e1:
        return [Fraction(e0), Fraction(e1)] + [Fraction((7 - i) * e0 + i * e1, 7) for i in range(1, 7)]
    return [Fraction(e0), Fraction(e1)] + [Fraction((5 - i) * e0 + i * e1, 5) for i in range(1, 5)] + [Fraction(0), Fraction(255)]


def decode_block(block):
    e0, e1 = block[0], block[1]
    bits = int.from_bytes(block[2:8], 'little')
    pal = palette(e0, e1)
    return [pal[(bits >> (3 * i)) & 7] for i in range(16)]


def to_byte(value):
    return int(value + Fraction(1, 2))   # round half up, the usual 8-bit conversion


def compare(decoded, blocks):
    worst_exact = worst_rounded = 0
    for n in range(len(blocks) // 8):
        exact = decode_block(blocks[8 * n:8 * n + 8])
        got = decoded[16 * n:16 * n + 16]
        for e, g in zip(exact, got):
            worst_exact = max(worst_exact, abs(float(e) - g))
            worst_rounded = max(worst_rounded, abs(to_byte(e) - g))
    return worst_exact, worst_rounded


def selftest():
    # endpoints 255 and 0, e0 > e1: seven-step palette
    block = bytes([255, 0]) + ((0 << 0) | (1 << 3)).to_bytes(6, 'little')
    px = decode_block(block)
    assert px[0] == 255 and px[1] == 0
    p = palette(255, 0)
    assert p[2] == Fraction(6 * 255, 7) and p[7] == Fraction(255, 7)
    # e0 <= e1: five-step palette with 0 and 255 appended
    p = palette(0, 255)
    assert p[6] == 0 and p[7] == 255 and p[2] == Fraction(4 * 0 + 255, 5)
    # a captured decode that equals the rounded reference is within half a step of the exact one
    blocks = bytes([200, 40]) + bytes(6)
    got = bytes(to_byte(v) for v in decode_block(blocks))
    exact, rounded = compare(got, blocks)
    assert rounded == 0 and exact <= 0.5
    print('bc45_reference selftest: ok')


if __name__ == '__main__':
    if '--selftest' in sys.argv:
        selftest()
    elif '--compare' in sys.argv:
        i = sys.argv.index('--compare')
        decoded = open(sys.argv[i + 1], 'rb').read()
        blocks = open(sys.argv[i + 2], 'rb').read()
        ex, ro = compare(decoded, blocks)
        print(f'largest difference from the exact decode {ex:.3f}/255, from the rounded decode {ro}/255')
        print('rounding-only difference' if ro <= 1 else f'{ro}/255 off the rounded reference: not rounding, the hardware interpolation differs')
    else:
        print(__doc__)
