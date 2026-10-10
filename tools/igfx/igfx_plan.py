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


def _dtd(d):
    pclk_khz = (d[0] | d[1] << 8) * 10
    ha = d[2] | (d[4] & 0xF0) << 4
    hb = d[3] | (d[4] & 0x0F) << 8
    va = d[5] | (d[7] & 0xF0) << 4
    vb = d[6] | (d[7] & 0x0F) << 8
    hso = d[8] | (d[11] & 0xC0) << 2
    hsw = d[9] | (d[11] & 0x30) << 4
    vso = (d[10] >> 4) | (d[11] & 0x0C) << 2
    vsw = (d[10] & 0x0F) | (d[11] & 0x03) << 4
    return {"pclk_khz": pclk_khz, "hactive": ha, "htotal": ha + hb, "hsync_start": ha + hso, "hsync_end": ha + hso + hsw,
            "vactive": va, "vtotal": va + vb, "vsync_start": va + vso, "vsync_end": va + vso + vsw,
            "hsync_pos": d[17] >> 1 & 1, "vsync_pos": d[17] >> 2 & 1}


def _displayid_timing(q, unit_khz):
    """DisplayID type I / VII detailed timing (20 bytes): each field is value-1; sync offsets carry polarity in bit 15."""
    u16 = lambda i: int.from_bytes(q[i:i + 2], "little")  # noqa: E731
    ha, hb, hso, hsw = u16(4) + 1, u16(6) + 1, (u16(8) & 0x7FFF) + 1, u16(10) + 1
    va, vb, vso, vsw = u16(12) + 1, u16(14) + 1, (u16(16) & 0x7FFF) + 1, u16(18) + 1
    return {"pclk_khz": (int.from_bytes(q[0:3], "little") + 1) * unit_khz, "hactive": ha, "htotal": ha + hb,
            "hsync_start": ha + hso, "hsync_end": ha + hso + hsw, "vactive": va, "vtotal": va + vb,
            "vsync_start": va + vso, "vsync_end": va + vso + vsw, "hsync_pos": u16(8) >> 15, "vsync_pos": u16(16) >> 15,
            "preferred": q[3] >> 7}


def preferred_timing(edid):
    """The mode Linux lights the panel with: among the preferred modes (the first DTD, and DisplayID timings flagged
    preferred), drm_mode_sort's order - largest area, then highest clock; the base DTD wins a tie because it is listed
    first. Measured on 21 recorded laptops: high-refresh eDP panels flag a DisplayID mode with the DTD's totals and a
    faster clock, and its sync polarity is the one Linux programs."""
    cands = []
    if edid[54] | edid[55]:
        cands.append(_dtd(edid[54:72]))
    for blk in range(1, len(edid) // 128):
        b = edid[blk * 128:(blk + 1) * 128]
        if b[0] != 0x70:
            continue
        p = 5
        while p + 3 <= 5 + b[2] and p + 3 < 128:
            tag, ln = b[p], b[p + 2]
            if tag in (0x03, 0x22):
                for q in range(p + 3, min(p + 3 + ln, 128 - 19), 20):
                    t = _displayid_timing(b[q:q + 20], 10 if tag == 0x03 else 1)
                    if t.pop("preferred"):
                        cands.append(t)
            p += 3 + ln
    if not cands:
        raise ValueError("no preferred detailed timing")
    best = cands[0]
    for t in cands[1:]:
        if (t["hactive"] * t["vactive"], t["pclk_khz"]) > (best["hactive"] * best["vactive"], best["pclk_khz"]):
            best = t
    return best


def vbt_blocks(vbt):
    """BDB blocks of a Video BIOS Table (Linux intel_vbt_defs.h layout): {block id: bytes}."""
    if vbt[:4] != b"$VBT":
        raise ValueError("no $VBT signature")
    bdb = struct.unpack_from("<I", vbt, 0x1C)[0]
    ver, hsz, bsz = struct.unpack_from("<HHH", vbt, bdb + 16)
    p, end, out = bdb + hsz, bdb + bsz, {"version": ver}
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
    # BDB_EDP.edp_max_port_link_rate[panel_type] (VBT 244+, byte 780), Linux units x20 = 10 kbps; 0 = no cap
    cap = 0
    if b["version"] >= 244 and len(b[27]) >= 812:
        cap = struct.unpack_from("<H", b[27], 780 + 2 * pt)[0] * 20 // 100
    return {"panel_type": pt, "t1_t3": t1_t3, "t8": t8, "t9": t9, "t10": t10, "t11_t12": t11_t12, "pwm_hz": hz,
            "max_link_rate": cap}


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


# link rates (Mbps) the combo/C10 PHY can drive (Linux icl_rates / mtl_rates); a sink rate outside this set is unusable
SOURCE_RATES = (1620, 2160, 2430, 2700, 3240, 4320, 5400, 6480, 8100)
DP_EDP_14 = 0x03


def edid_modes(edid):
    """(pixel clock kHz, hactive) for every timing the panel lists: base DTDs, CTA DTDs, DisplayID type I (10 kHz) and
    type VII (1 kHz) timings. eDP high-refresh modes usually live only in the DisplayID extension."""
    out = []
    for blk in range(len(edid) // 128):
        b = edid[blk * 128:(blk + 1) * 128]
        offs = [54, 72, 90, 108] if blk == 0 else (list(range(b[2], 110, 18)) if b[0] == 0x02 and b[2] >= 4 else [])
        for o in offs:
            pc = (b[o] | b[o + 1] << 8) * 10
            if pc:
                out.append((pc, b[o + 2] | (b[o + 4] & 0xF0) << 4))
        if b[0] == 0x70:
            p = 5
            while p + 3 <= 5 + b[2] and p + 3 < 128:
                tag, ln = b[p], b[p + 2]
                if tag in (0x03, 0x22):
                    for q in range(p + 3, min(p + 3 + ln, 128 - 19), 20):
                        unit = 10 if tag == 0x03 else 1
                        out.append(((int.from_bytes(b[q:q + 3], "little") + 1) * unit, int.from_bytes(b[q + 4:q + 6], "little") + 1))
                p += 3 + ln
    return out


def edid_bpp(edid):
    """EDID 1.4 digital bit depth x3, else 24 (Linux uses display_info.bpc; 0 = undefined -> 8 bpc here)."""
    if edid[18:20] >= b"\x01\x04" and edid[20] & 0x80:
        bpc = {1: 6, 2: 8, 3: 10, 4: 12, 5: 14, 6: 16}.get((edid[20] >> 4) & 7)
        if bpc:
            return min(bpc, 10) * 3          # Gen12 eDP pipes go up to 10 bpc for this path
    return 24


def link_plan(dpcd, edid, vbt_max_rate=0):
    """The eDP link (rate Mbps, lanes) Linux trains, measured on 21 recorded laptops:
    eDP < 1.4 -> max rate x max lanes; eDP 1.4+ -> lowest rate, then fewest lanes, that carries the panel's highest-clock
    mode at its EDID bpp (8b/10b: rate x lanes x 0.8 Mbps). We do not drive DSC, so a mode that only fits compressed is
    left out (Linux instead runs such panels at max link + DSC: one recorded MS-15P2)."""
    lanes_max = dpcd.get(2, 0) & 0x1F or 4
    sink = []
    for i in range(8):
        lo, hi = dpcd.get(0x10 + 2 * i), dpcd.get(0x11 + 2 * i, 0)
        if not lo and not hi:
            break
        sink.append(((lo or 0) | hi << 8) * 200 // 1000)
    if not sink:
        top = dpcd.get(1, 0x0A) * 270
        sink = [r for r in (1620, 2700, 5400, 8100) if r <= top]
    common = sorted(set(sink) & set(SOURCE_RATES))
    if vbt_max_rate:
        common = [r for r in common if r <= vbt_max_rate]
    if not common:
        raise ValueError("no link rate common to the panel and the PHY")
    if dpcd.get(0x700, 0) < DP_EDP_14:
        return common[-1], lanes_max
    bpp = edid_bpp(edid)
    cap = common[-1] * lanes_max * 0.8
    clocks = [c for c, _ in edid_modes(edid) if c / 1000 * bpp <= cap]
    if not clocks:
        raise ValueError("no panel mode fits the link uncompressed")
    need = max(clocks) / 1000 * bpp
    for r in common:
        for n in (1, 2, 4):
            if n <= lanes_max and r * n * 0.8 >= need:
                return r, n
    return common[-1], lanes_max


# Linux icl_dp_combo_pll_{24MHz,19_2MHz}_values: link rate -> (dco_integer, dco_fraction, pdiv, kdiv, qdiv_mode, qdiv_ratio)
COMBO_PLL = {
    24000: {5400: (0x151, 0x4000, 2, 1, 0, 0), 2700: (0x151, 0x4000, 2, 2, 0, 0), 1620: (0x151, 0x4000, 4, 2, 0, 0),
            3240: (0x151, 0x4000, 4, 1, 0, 0), 2160: (0x168, 0, 1, 2, 1, 2), 4320: (0x168, 0, 1, 2, 0, 0),
            6480: (0x195, 0, 2, 1, 0, 0), 8100: (0x151, 0x4000, 1, 1, 0, 0)},
    19200: {5400: (0x1A5, 0x7000, 2, 1, 0, 0), 2700: (0x1A5, 0x7000, 2, 2, 0, 0), 1620: (0x1A5, 0x7000, 4, 2, 0, 0),
            3240: (0x1A5, 0x7000, 4, 1, 0, 0), 2160: (0x1C2, 0, 1, 2, 1, 2), 4320: (0x1C2, 0, 1, 2, 0, 0),
            6480: (0x1FA, 0x2000, 2, 1, 0, 0), 8100: (0x1A5, 0x7000, 1, 1, 0, 0)},
}


def dpll_plan(rate, refclk_khz, display_version):
    """Combo PHY DPLL CFGCR0/CFGCR1 for a DP link rate (display 11-13; display 14 uses the C10 PHY, not planned here).
    38.4 MHz uses the 19.2 table with the DCO fraction halved (Linux ehl_combo_pll_div_frac_wa_needed: display 12+)."""
    dco_int, frac, pdiv, kdiv, qmode, qratio = COMBO_PLL[24000 if refclk_khz == 24000 else 19200][rate]
    if display_version >= 12 and refclk_khz == 38400:
        frac = (frac + 1) // 2
    cfgcr0 = frac << 10 | dco_int
    cfgcr1 = qratio << 10 | qmode << 9 | kdiv << 6 | pdiv << 2
    if display_version < 12:
        cfgcr1 |= 3                              # DPLL_CFGCR1_CENTRAL_FREQ_8400 (ICL); TGL+ writes CFSELOVRD_NORMAL_XTAL = 0
    return {"DPLL_CFGCR0": cfgcr0, "DPLL_CFGCR1": cfgcr1}


def ddi_plan(t, bpp, lanes, port=0):
    """TRANS_DDI_FUNC_CTL and DDI_BUF_CTL for an eDP SST link (TGL+ layout): enable, port select (port+1 at 30:27),
    DP SST mode (2 at 26:24), bpc (8/10/6/12 -> 0/1/2/3 at 22:20), sync polarity (V 17, H 16), port width (lanes-1 at 3:1)."""
    bpc = {24: 0, 30: 1, 18: 2, 36: 3}[bpp]
    func = 1 << 31 | (port + 1) << 27 | 2 << 24 | bpc << 20 | t["vsync_pos"] << 17 | t["hsync_pos"] << 16 | (lanes - 1) << 1
    return {"TRANS_DDI_FUNC_CTL": func, "DDI_BUF_CTL": 1 << 31 | (lanes - 1) << 1}


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
