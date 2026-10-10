#!/usr/bin/env python3
"""The iGPU trace decoder reads a Linux mmiotrace of Intel's driver and recovers the panel mode it programmed. A small
synthetic trace in the probe's format (PCIDEV header + W lines); the real ones come from 1401 probe uploads."""
import lzma
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "igfx"))
import igfx_trace as T  # noqa: E402

BAR0 = 0x6103000000


def trace(writes, driver="i915"):
    lines = ["VERSION 20070824", "PCIDEV 0010 808646a6 a2 %x 0 400000000c 0 %s" % (BAR0 | 4, driver)]
    for off, val in writes:
        lines.append("W 4 1.000000 1 0x%x 0x%x 0x0 0" % (BAR0 + off, val))
    p = os.path.join(tempfile.mkdtemp(), "t.mmio.xz")
    with lzma.open(p, "wt") as f:
        f.write("\n".join(lines) + "\n")
    return p


class Decode(unittest.TestCase):
    def test_recovers_the_panel_mode_intels_driver_programmed(self):
        # 1920x1080, htotal 2085, vtotal 1176: what one uploaded laptop's eDP panel EDID asks for
        p = trace([(0x60000, (2084 << 16) | 1919), (0x6000C, (1175 << 16) | 1079), (0x6001C, (1919 << 16) | 1079),
                   (0x70008, 0x80000000)])
        bar0, last, seq = T.decode(p)
        self.assertEqual(bar0, BAR0)
        self.assertEqual(T.mode(last, "A"), {"hactive": 1920, "htotal": 2085, "vactive": 1080, "vtotal": 1176,
                                             "pipesrc": (1920, 1080)})
        self.assertEqual(last["TRANSCONF_A"], 0x80000000)

    def test_negative_control_writes_outside_the_igpu_are_ignored(self):
        p = trace([(0x2000000 + 0x60000, 0x12345678)])   # past the register window: another mapping
        bar0, last, seq = T.decode(p)
        self.assertEqual(seq, [])
        self.assertIsNone(T.mode(last, "A"))


if __name__ == "__main__":
    unittest.main()
