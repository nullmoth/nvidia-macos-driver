#!/usr/bin/env python3
"""Decode a Linux mmiotrace of Intel's i915/xe driver (from a 1401 probe upload) into the display programming it did:
transcoder timings, pipe source size, DDI/transcoder function, plane setup, panel power and backlight. Offsets come
from Linux's MIT-licensed i915 display headers. This is the reference our macOS iGPU driver must reproduce per machine.

usage: igfx_trace.py <trace.mmio.xz> [--all]"""
import lzma
import re
import sys

# transcoder/pipe/plane register blocks repeat per instance; A at these bases, B/C/D at +0x1000 steps (Linux _PIPE/_TRANS)
TRANS = {0x00: "HTOTAL", 0x04: "HBLANK", 0x08: "HSYNC", 0x0C: "VTOTAL", 0x10: "VBLANK", 0x14: "VSYNC", 0x1C: "PIPESRC",
         0x400: "TRANS_DDI_FUNC_CTL"}
NAMES = {0x61200: "PP_STATUS", 0x61204: "PP_CONTROL", 0xC8250: "BLC_PWM_PCH_CTL1", 0xC8254: "BLC_PWM_PCH_CTL2",
         0x64000: "DDI_BUF_CTL_A"}


def name(off):
    if off in NAMES:
        return NAMES[off]
    if 0x60000 <= off < 0x64000:
        inst, reg = divmod(off - 0x60000, 0x1000)
        if reg in TRANS:
            return "%s_%s" % (TRANS[reg], "ABCD"[inst])
    if 0x70000 <= off < 0x74000:
        inst, reg = divmod(off - 0x70000, 0x1000)
        if reg == 0x008:
            return "TRANSCONF_%s" % "ABCD"[inst]
    if 0x70180 <= off < 0x74000:  # PLANE_*_1_A at 0x70180.. (CTL 0x80, STRIDE 0x88, POS 0x8C, SIZE 0x90, SURF 0x9C)
        inst, reg = divmod(off - 0x70000, 0x1000)
        plane, r = divmod(reg - 0x180, 0x100)
        sub = {0x00: "PLANE_CTL", 0x08: "PLANE_STRIDE", 0x0C: "PLANE_POS", 0x10: "PLANE_SIZE", 0x1C: "PLANE_SURF"}.get(r)
        if sub and 0 <= plane < 8:
            return "%s_%d_%s" % (sub, plane + 1, "ABCD"[inst])
    if 0x64000 <= off < 0x64500 and (off - 0x64000) % 0x100 == 0:
        return "DDI_BUF_CTL_%s" % "ABCDE"[(off - 0x64000) // 0x100]
    return None


def bar0_of(header_line):
    # PCIDEV <bus-devfn> <vendor-device> <irq> <bar0> ...: bit 0..3 are flags, the base is the rest
    return int(header_line.split()[4], 16) & ~0xF


def decode(path):
    bar0 = None
    last = {}
    seq = []
    with lzma.open(path, "rt", errors="replace") as f:
        for line in f:
            if bar0 is None and line.startswith("PCIDEV ") and line.split()[2].startswith("8086") and line.rstrip().endswith(("i915", "xe")):
                bar0 = bar0_of(line)
                continue
            if bar0 is None or not line.startswith("W "):
                continue
            p = line.split()
            # W <width> <timestamp> <map-id> <phys-addr> <value> <pc> <pid>
            addr, val = int(p[4], 16), int(p[5], 16)
            off = addr - bar0
            if not (0 <= off < 0x200000):
                continue
            n = name(off)
            if n:
                last[n] = val
                seq.append((n, val))
    return bar0, last, seq


def mode(last, t="A"):
    def pair(reg):
        v = last.get("%s_%s" % (reg, t))
        return None if v is None else ((v & 0xFFFF) + 1, ((v >> 16) & 0xFFFF) + 1)
    h, v = pair("HTOTAL"), pair("VTOTAL")
    src = last.get("PIPESRC_%s" % t)
    if not (h and v):
        return None
    return {"hactive": h[0], "htotal": h[1], "vactive": v[0], "vtotal": v[1],
            "pipesrc": None if src is None else (((src >> 16) & 0xFFFF) + 1, (src & 0xFFFF) + 1)}


if __name__ == "__main__":
    bar0, last, seq = decode(sys.argv[1])
    print("BAR0 0x%x, %d named display writes" % (bar0 or 0, len(seq)))
    for t in "ABCD":
        m = mode(last, t)
        if m:
            print("transcoder %s: %s" % (t, m))
    for n in sorted(k for k in last if not re.match(r"(H|V)(TOTAL|BLANK|SYNC)_", k)):
        print("  %-24s 0x%08x" % (n, last[n]))
    if "--all" in sys.argv:
        for n, v in seq:
            print("W %-24s 0x%08x" % (n, v))
