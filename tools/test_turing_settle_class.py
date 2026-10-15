#!/usr/bin/env python3
"""Run setup's real nv_device_id() + Turing-settle block against stub ioreg output.

A muxless Turing laptop dGPU has no display outputs and enumerates as a 3D controller (class 0302), not VGA
(0300). nv_device_id() used to match 0300 only, so NVDEV was empty and nvrmsettle=10000 was never added (#119).
The negative control restores the 0300-only match and shows the muxless card regresses to no settle.
"""
import os
import pathlib
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"


def between(s, start, end, include_end=True):
    a = s.index(start)
    b = s.index(end, a) + (len(end) if include_end else 0)
    return s[a:b]


def settle_block(old_class_only=False):
    s = SETUP.read_text()
    block = between(s, "nv_device_id() {", '[ "$TURING" = 1 ] && WANT_ARGS="$WANT_ARGS nvrmsettle=10000"')
    assert block.count("class-code") == 2, "expected the 0300 and 0302 class matches in the block"
    if old_class_only:  # the pre-fix function: VGA only, no 3D-controller fallback
        block = block.replace('    /"class-code" = <00000302>/ { c=302 }\n', "")
        block = block.replace('print (vga != "" ? vga : d3d)', "print vga")
    return 'WANT_ARGS=""\n' + block + '\necho "NVDEV=$NVDEV"\necho "WANT_ARGS=$WANT_ARGS"\n'


def dev(vendor, devid, cls):
    # one ioreg device record with the three properties nv_device_id reads
    return ('  +-o pci%s@0  <class IOPCIDevice>\n'
            '      "vendor-id" = <%s>\n'
            '      "device-id" = <%s>\n'
            '      "class-code" = <%s>\n') % (devid, vendor, devid, cls)


NV = "de100000"           # 0x10de, byte-swapped as ioreg prints it
INTEL = "86800000"        # 0x8086
# device-id bytes (little-endian in ioreg) -> the 4-hex id the function prints
TU117_3D = ("951f0000", "1f95")   # GTX 1650 Ti class muxless laptop part, class 0302
TU102_VGA = ("071e0000", "1e07")  # a Turing VGA display card, class 0300
AMPERE_VGA = ("04250000", "2504")  # a non-Turing VGA display card, class 0300


def run(fixture, old_class_only=False):
    with tempfile.TemporaryDirectory() as d:
        binp = pathlib.Path(d, "bin"); binp.mkdir()
        ioreg = binp / "ioreg"
        ioreg.write_text("#!/bin/bash\ncat <<'IOREG'\n" + fixture + "IOREG\n")
        ioreg.chmod(0o755)
        script = pathlib.Path(d, "s.sh"); script.write_text(settle_block(old_class_only))
        env = dict(os.environ, PATH=str(binp) + ":/usr/bin:/bin")
        out = subprocess.run(["/bin/bash", str(script)], capture_output=True, text=True, env=env, check=True).stdout
        vals = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
        return vals["NVDEV"], "nvrmsettle=10000" in vals["WANT_ARGS"]


class TuringSettleClass(unittest.TestCase):
    def test_muxless_turing_3d_controller_gets_the_settle(self):
        nvdev, settle = run(dev(NV, TU117_3D[0], "00000302"))
        self.assertEqual(nvdev, TU117_3D[1])
        self.assertTrue(settle, "a muxless Turing 0302 dGPU must get nvrmsettle=10000")

    def test_vga_display_card_is_preferred_over_a_3d_controller(self):
        # compute (0302) listed first; the real display card (0300) still decides, and it is not Turing -> no settle
        fixture = dev(NV, TU117_3D[0], "00000302") + dev(NV, AMPERE_VGA[0], "00000300")
        nvdev, settle = run(fixture)
        self.assertEqual(nvdev, AMPERE_VGA[1], "the 0300 display card must win over a 0302 compute card")
        self.assertFalse(settle, "the non-Turing display card decides; no settle")

    def test_vga_turing_card_is_unchanged(self):
        nvdev, settle = run(dev(NV, TU102_VGA[0], "00000300"))
        self.assertEqual(nvdev, TU102_VGA[1])
        self.assertTrue(settle)

    def test_no_nvidia_card_means_no_settle(self):
        nvdev, settle = run(dev(INTEL, "9a490000", "00000300"))
        self.assertEqual(nvdev, "")
        self.assertFalse(settle)

    def test_negative_control_old_vga_only_match_regresses(self):
        # with the 0300-only match restored, the muxless 0302 card is missed and loses the settle
        nvdev, settle = run(dev(NV, TU117_3D[0], "00000302"), old_class_only=True)
        self.assertEqual(nvdev, "")
        self.assertFalse(settle, "control proves the test detects the fix: 0300-only misses the muxless dGPU")


if __name__ == "__main__":
    unittest.main(verbosity=2)
