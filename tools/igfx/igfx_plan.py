#!/usr/bin/env python3
"""Plan the Intel display programming for a panel from its EDID: the transcoder timing, pipe source and primary plane
values Intel's driver writes for that mode (register layout from Linux's MIT-licensed i915 display headers). The iGPU
driver runs this same plan; igfx_check.py compares it with what Linux's driver actually wrote on each recorded machine.

usage: igfx_plan.py <panel.edid>"""
import json
import os
import struct
import sys

VERSIONS = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "display_versions.json")))["ids"]


def display_version(device_id):
    """Intel display IP version for an iGPU PCI device ID (Linux's pciids.h families), or None if unknown."""
    e = VERSIONS.get("%04x" % device_id)
    return e and e["display_version"]


def preferred_timing(edid):
    """The first detailed timing descriptor (EDID 1.3+ puts the preferred mode there)."""
    d = edid[54:72]
    pclk_khz = (d[0] | d[1] << 8) * 10
    if not pclk_khz:
        raise ValueError("no detailed timing in the first descriptor")
    ha = d[2] | (d[4] & 0xF0) << 4
    hb = d[3] | (d[4] & 0x0F) << 8
    va = d[5] | (d[7] & 0xF0) << 4
    vb = d[6] | (d[7] & 0x0F) << 8
    hso = d[8] | (d[11] & 0xC0) << 2
    hsw = d[9] | (d[11] & 0x30) << 4
    vso = (d[10] >> 4) | (d[11] & 0x0C) << 2
    vsw = (d[10] & 0x0F) | (d[11] & 0x03) << 4
    return {"pclk_khz": pclk_khz, "hactive": ha, "htotal": ha + hb, "hsync_start": ha + hso, "hsync_end": ha + hso + hsw,
            "vactive": va, "vtotal": va + vb, "vsync_start": va + vso, "vsync_end": va + vso + vsw}


def vbt_blocks(vbt):
    """BDB blocks of a Video BIOS Table (Linux intel_vbt_defs.h layout): {block id: bytes}."""
    if vbt[:4] != b"$VBT":
        raise ValueError("no $VBT signature")
    bdb = struct.unpack_from("<I", vbt, 0x1C)[0]
    hsz, bsz = struct.unpack_from("<HH", vbt, bdb + 18)
    p, end, out = bdb + hsz, bdb + bsz, {}
    while p + 3 <= end:
        bid, size = vbt[p], struct.unpack_from("<H", vbt, p + 1)[0]
        out[bid] = vbt[p + 3:p + 3 + size]
        p += 3 + size
    return out


def vbt_panel(vbt, edid):
    """The eDP panel's power sequence (100 us units) and backlight PWM frequency from the VBT, picked the way Linux does:
    BDB_LVDS_OPTIONS.panel_type, or 255 = the entry whose PnP id matches the EDID's (bytes 8..17), else entry 0."""
    b = vbt_blocks(vbt)
    pt = b[40][0] if 40 in b else 0
    if pt == 0xFF:
        lfp = b.get(42, b"")
        i = lfp.find(edid[8:18])
        pt = i // (len(lfp) // 16) if i >= 0 else 0
    t1_t3, t8, t9, t10, t11_t12 = struct.unpack_from("<5H", b[27], pt * 10)
    hz = None
    if 43 in b:
        esz = b[43][0]
        hz = struct.unpack_from("<H", b[43], 1 + pt * esz + 1)[0]
    return {"panel_type": pt, "t1_t3": t1_t3, "t8": t8, "t9": t9, "t10": t10, "t11_t12": t11_t12, "pwm_hz": hz}


def rawclk_hz(display_version):
    # measured on 21 recorded laptops: TGL/ADL/RPL program the PWM period against 19.2 MHz, Arrow Lake (14) against 38.4
    return 38400000 if display_version >= 14 else 19200000


def panel_plan(p, display_version):
    """Panel power + backlight registers. Backlight on/off delays are done in software, so their fields hold 1."""
    regs = {"PP_ON_DELAYS": p["t1_t3"] << 16 | 1,
            "PP_OFF_DELAYS": p["t10"] << 16 | 1,
            "PP_CYCLE": p["t11_t12"] // 1000 + 1}      # PP_CONTROL bits 8:4, in 100 ms units, rounded up past the delay
    if p["pwm_hz"]:
        regs["BLC_PWM_PCH_CTL2"] = round(rawclk_hz(display_version) / p["pwm_hz"])
    return regs


def pack(lo, hi):
    """Intel timing registers hold (value - 1) in each 16-bit half."""
    return ((hi - 1) << 16) | (lo - 1)


def plan(t, display_version=12, bpp=4, stride_align=64):
    # Linux intel_display.c: from display version 13 (ADL-P on) the hardware ignores VBLANK_START and the driver writes
    # 1 (0 in the register; the pipe's vblank start moved to TRANS_SET_CONTEXT_LATENCY). Version 12 writes vactive
    # (+ set-context latency, 0 here).
    vblank_start = 1 if display_version >= 13 else t["vactive"]
    regs = {
        "HTOTAL": pack(t["hactive"], t["htotal"]),
        "HBLANK": pack(t["hactive"], t["htotal"]),
        "HSYNC": pack(t["hsync_start"], t["hsync_end"]),
        "VTOTAL": pack(t["vactive"], t["vtotal"]),
        "VBLANK": pack(vblank_start, t["vtotal"]),
        "VSYNC": pack(t["vsync_start"], t["vsync_end"]),
        "PIPESRC": pack(t["vactive"], t["hactive"]),          # width-1 in the high half, height-1 in the low half
        "PLANE_SIZE_1": pack(t["hactive"], t["vactive"]),     # height-1 high, width-1 low
    }
    # linear XRGB8888: stride is programmed in 64-byte units
    regs["PLANE_STRIDE_1"] = (t["hactive"] * bpp + stride_align - 1) // stride_align
    return regs


if __name__ == "__main__":
    t = preferred_timing(open(sys.argv[1], "rb").read())
    print(t)
    for k, v in plan(t).items():
        print("  %-16s 0x%08x" % (k, v))
