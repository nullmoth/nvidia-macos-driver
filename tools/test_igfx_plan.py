#!/usr/bin/env python3
"""The iGPU mode planner turns a panel EDID into Intel's transcoder/pipe/plane values. Checked against 20 recorded
laptops (igfx_check.py, 20/20); here a synthetic EDID per display version, including the ADL+ VBLANK_START rule."""
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


if __name__ == "__main__":
    unittest.main()
