#!/usr/bin/env python3
"""Decode the DP AUX channel A traffic (the eDP panel's DPCD reads and writes) and the reference-clock strap from an
i915/xe mmiotrace. Register layout from Linux's MIT-licensed i915 display code: the request header is packed MSB-first
into DP_AUX_CH_CTL(A)+4.. (0x64014..), SEND_BUSY is CTL bit 31, the message size is CTL bits 24:20, DONE is bit 30.

usage: igfx_aux.py <trace.mmio.xz>"""
import lzma
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import igfx_trace as T  # noqa: E402

AUX_CTL, AUX_DATA = 0x64010, 0x64014
DSSM = 0x51004
REFCLK_KHZ = {0: 24000, 1: 19200, 2: 38400}     # DSSM bits 31:29


def _bytes(words, n):
    return b"".join(words.get(i, 0).to_bytes(4, "big") for i in range(5))[:n]


def decode(path):
    """-> (dpcd reads {address: byte}, dpcd writes [(address, bytes)], reference clock kHz or None)."""
    bar0 = None
    data, cur, done = {}, None, []
    refclk = None
    with lzma.open(path, "rt", errors="replace") as fh:
        for line in fh:
            if bar0 is None:
                if line.startswith("PCIDEV ") and line.rstrip().endswith(("i915", "xe")) and line.split()[2].startswith("8086"):
                    bar0 = T.bar0_of(line)
                continue
            if line[:2] not in ("R ", "W "):
                continue
            p = line.split()
            off, val = int(p[4], 16) - bar0, int(p[5], 16)
            if off == DSSM and p[0] == "R" and refclk is None:
                refclk = REFCLK_KHZ.get(val >> 29)
            if not (AUX_CTL <= off < AUX_DATA + 20):
                continue
            if p[0] == "W" and off >= AUX_DATA:
                data[(off - AUX_DATA) // 4] = val
            elif p[0] == "W" and off == AUX_CTL and val >> 31:
                cur = {"req": _bytes(data, (val >> 20) & 0x1F)}
                done.append(cur)
                data = {}
            elif cur is not None and p[0] == "R" and off == AUX_CTL and not val >> 31 and (val >> 30) & 1:
                cur["rsize"] = (val >> 20) & 0x1F
            elif cur is not None and p[0] == "R" and off >= AUX_DATA and "rsize" in cur:
                cur.setdefault("rwords", {})[(off - AUX_DATA) // 4] = val
    reads, writes = {}, []
    for t in done:
        r = t["req"]
        if len(r) < 3:
            continue
        cmd, addr = r[0] >> 4, (r[0] & 0xF) << 16 | r[1] << 8 | r[2]
        if cmd == 9 and "rwords" in t:                 # native read; reply byte 0 is the ACK code
            rep = _bytes(t["rwords"], t["rsize"])
            if rep and rep[0] & 0x30 == 0:
                for i, b in enumerate(rep[1:]):
                    reads.setdefault(addr + i, b)
        elif cmd == 8:                                   # native write
            writes.append((addr, r[4:]))
    return reads, writes, refclk


def chosen_link(reads, writes):
    """The link Linux trained: (rate in Mbps, lane count, DSC on), from its LINK_BW_SET / LINK_RATE_SET / LANE_COUNT writes."""
    table = sink_rate_table(reads)
    rate = lanes = None
    dsc = False
    for a, d in writes:
        if a == 0x100 and d:
            rate = d[0] * 270
            if len(d) > 1:
                lanes = d[1] & 0x1F
        elif a == 0x115 and d and d[0] < len(table):
            rate = table[d[0]]
        elif a == 0x101 and d:
            lanes = d[0] & 0x1F
        elif a == 0x160 and d:
            dsc = bool(d[0] & 1)
    return rate, lanes, dsc


def sink_rate_table(reads):
    """eDP 1.4 SUPPORTED_LINK_RATES (DPCD 0x10..0x1F, 200 kHz units) in Mbps, in table order."""
    out = []
    for i in range(8):
        lo = reads.get(0x10 + 2 * i)
        if not lo and not reads.get(0x11 + 2 * i):
            break
        out.append((lo | reads.get(0x11 + 2 * i, 0) << 8) * 200 // 1000)
    return out


if __name__ == "__main__":
    r, w, ref = decode(sys.argv[1])
    print("refclk %s kHz, DPCD rev 0x%02x, max rate 0x%02x, lanes %d, eDP rev 0x%02x, rates %s, DSC %d" % (
        ref, r.get(0, 0), r.get(1, 0), r.get(2, 0) & 0x1F, r.get(0x700, 0), sink_rate_table(r), r.get(0x60, 0) & 1))
    print("trained:", chosen_link(r, w))
