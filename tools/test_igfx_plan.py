#!/usr/bin/env python3
"""The iGPU mode planner turns a panel EDID into Intel's transcoder/pipe/plane values. Checked against 20 recorded
laptops (igfx_check.py, 20/20); here a synthetic EDID per display version, including the ADL+ VBLANK_START rule, and a
synthetic VBT for the panel power sequence and backlight PWM (also 20/20 recorded)."""
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "igfx"))
import igfx_plan as P  # noqa: E402


def edid_1080p():
    e = bytearray(128)
    # 1920x1080, htotal 2085 (hblank 165), vtotal 1176 (vblank 96), hsync +48/32, vsync +3/5, 147.0 MHz
    d = bytes([0x6C, 0x39, 0x80, 0xA5, 0x70, 0x38, 0x60, 0x40, 0x30, 0x20, 0x35, 0x00, 0, 0, 0, 0, 0, 0x1A])
    e[54:72] = d
    e[8:18] = bytes.fromhex("09e5e90800000000271d")   # PnP id of a BOE panel (one recorded HP's)
    return bytes(e)


class Plan(unittest.TestCase):
    def test_tiger_lake_writes_vblank_start_at_vactive(self):
        r = P.plan(P.preferred_timing(edid_1080p()), display_version=12)
        self.assertEqual(r["HTOTAL"], (2084 << 16) | 1919)
        self.assertEqual(r["VBLANK"], (1175 << 16) | 1079)
        self.assertEqual(r["PIPESRC"], (1919 << 16) | 1079)
        self.assertEqual(r["PLANE_STRIDE_1"], 120)

    def test_alder_lake_p_and_newer_leave_vblank_start_zero(self):
        # Linux: VBLANK_START is ignored from display version 13; 10 of the 20 recorded laptops prove it
        r = P.plan(P.preferred_timing(edid_1080p()), display_version=13)
        self.assertEqual(r["VBLANK"], (1175 << 16) | 0)

    def test_device_ids_map_to_display_versions(self):
        self.assertEqual(P.display_version(0x9A49), 12)   # Tiger Lake
        self.assertEqual(P.display_version(0x46A6), 13)   # Alder Lake-P
        self.assertEqual(P.display_version(0xA788), 12)   # Raptor Lake-S (HX laptops)
        self.assertIsNone(P.display_version(0x3E9B))      # Coffee Lake: WhateverGreen's, not ours

    def test_negative_control_no_detailed_timing_is_refused(self):
        with self.assertRaises(ValueError):
            P.preferred_timing(bytes(128))

    def test_vbt_panel_type_picks_the_power_sequence_and_backlight(self):
        p = P.vbt_panel(vbt(panel_type=2), edid_1080p())
        self.assertEqual((p["panel_type"], p["t1_t3"], p["t10"], p["t11_t12"], p["pwm_hz"]), (2, 2000, 500, 5000, 200))
        r = P.panel_plan(p, 13)
        self.assertEqual(r["PP_ON_DELAYS"], 0x07D00001)
        self.assertEqual(r["PP_OFF_DELAYS"], 0x01F40001)
        self.assertEqual(r["PP_CYCLE"], 6)
        self.assertEqual(r["BLC_PWM_PCH_CTL2"], 96000)   # 19.2 MHz / 200 Hz

    def test_panel_type_255_matches_the_edid_pnp_id(self):
        p = P.vbt_panel(vbt(panel_type=0xFF, pnp_at=5), edid_1080p())
        self.assertEqual(p["panel_type"], 5)

    def test_arrow_lake_backlight_counts_a_38_4_mhz_clock(self):
        # the 7-245HX recording wrote 192000 for a 200 Hz VBT; a 19.2 MHz clock would give 96000 and a dark-or-dim panel
        r = P.panel_plan(P.vbt_panel(vbt(panel_type=2), edid_1080p()), 14)
        self.assertEqual(r["BLC_PWM_PCH_CTL2"], 192000)
        self.assertNotEqual(r["BLC_PWM_PCH_CTL2"], P.panel_plan(P.vbt_panel(vbt(panel_type=2), edid_1080p()), 13)["BLC_PWM_PCH_CTL2"])

    def test_edp_older_than_1_4_trains_at_max_rate_and_lanes(self):
        dpcd = {0: 0x12, 1: 0x14, 2: 0x84, 0x700: 0x02}
        self.assertEqual(P.link_plan(dpcd, edid_1080p()), (5400, 4))

    def test_edp_1_4_takes_the_lowest_rate_that_carries_the_fastest_mode(self):
        # recorded ASUS V3607: 1080p DTD at 162 MHz, 389 MHz DisplayID type VII mode (1 kHz units), 8 bpc -> 3.24 x 4
        dpcd = {0: 0x14, 1: 0x14, 2: 0x84, 0x700: 0x05}
        for i, r in enumerate((1620, 2160, 2430, 2700, 3240, 4320, 5400)):
            v = r * 1000 // 200
            dpcd[0x10 + 2 * i], dpcd[0x11 + 2 * i] = v & 0xFF, v >> 8
        self.assertEqual(P.link_plan(dpcd, edid_with_displayid(389381)), (3240, 4))

    def test_a_mode_that_only_fits_with_dsc_is_left_out(self):
        # MS-15P2: a 1175 MHz mode cannot ride 5.4 x 4 uncompressed; we plan the 147 MHz DTD instead of DSC
        dpcd = {0: 0x14, 1: 0x14, 2: 0x84, 0x700: 0x05,
                0x10: 1620 * 5 & 0xFF, 0x11: 1620 * 5 >> 8, 0x12: 5400 * 5 & 0xFF, 0x13: 5400 * 5 >> 8}
        self.assertEqual(P.link_plan(dpcd, edid_with_displayid(1175110)), (1620, 4))

    def test_dpll_at_38_4_mhz_halves_the_dco_fraction(self):
        # 19 of 21 recorded laptops wrote CFGCR0 0x00e001a5 / CFGCR1 0x88 for 2.7 Gbps on a 38.4 MHz reference
        r = P.dpll_plan(2700, 38400, 12)
        self.assertEqual((r["DPLL_CFGCR0"], r["DPLL_CFGCR1"]), (0x00E001A5, 0x88))
        self.assertEqual(P.dpll_plan(2700, 19200, 12)["DPLL_CFGCR0"], 0x01C001A5)
        self.assertNotEqual(P.dpll_plan(5400, 38400, 12), r)

    def test_negative_control_not_a_vbt_is_refused(self):
        with self.assertRaises(ValueError):
            P.vbt_blocks(b"\0" * 64)


def edid_with_displayid(clock_khz):
    """The 1080p base EDID plus a DisplayID extension holding one type VII timing (pixel clock - 1, in kHz)."""
    ext = bytearray(128)
    ext[0], ext[1], ext[2] = 0x70, 0x20, 3 + 20
    ext[5], ext[6], ext[7] = 0x22, 0, 20
    ext[8:11] = (clock_khz - 1).to_bytes(3, "little")
    ext[12:14] = (1920 - 1).to_bytes(2, "little")
    base = bytearray(edid_1080p())
    base[126] = 1
    return bytes(base) + bytes(ext)


def vbt(panel_type, pnp_at=None):
    """Minimal VBT: header, BDB with LVDS_OPTIONS (40), EDP power sequences (27), LFP_DATA (42), LFP_BACKLIGHT (43)."""
    import struct
    seqs = b"".join(struct.pack("<5H", 2000, 10, 2000, 500, 5000) if i == (pnp_at if panel_type == 0xFF else panel_type)
                    else struct.pack("<5H", 1, 1, 1, 1, 1000) for i in range(16))
    lfp = bytearray(16 * 20)
    if pnp_at is not None:
        lfp[pnp_at * 20 + 4:pnp_at * 20 + 14] = edid_1080p()[8:18]
    bl = bytes([6]) + b"".join(struct.pack("<BHBBB", 2, 200, 0, 0, 0) for _ in range(16))
    blocks = b"".join(bytes([i]) + struct.pack("<H", len(d)) + d
                      for i, d in ((40, bytes([panel_type]) + bytes(7)), (27, seqs), (42, bytes(lfp)), (43, bl)))
    bdb = b"BIOS_DATA_BLOCK " + struct.pack("<HHH", 243, 22, 22 + len(blocks)) + blocks
    hdr = bytearray(0x30)
    hdr[:4] = b"$VBT"
    struct.pack_into("<I", hdr, 0x1C, len(hdr))
    return bytes(hdr) + bdb


if __name__ == "__main__":
    unittest.main()
