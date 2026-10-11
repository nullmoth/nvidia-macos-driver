#!/usr/bin/env python3
"""NVRMFBClaim retires IONDRVFramebuffer on the NVIDIA GPU only (#119: with IONameMatch "display" alone it also held the
IOFramebuffer category on a muxless laptop's Intel GPU, whose panel then had no framebuffer at all).

Reads both fb Info.plists and NVRM.kext's, then applies IOPCIFamily's masked compares (IOPCIPrimaryMatch: the
(device << 16 | vendor) word; IOPCIClassMatch: the class/revision register) to the devices that matter."""
import pathlib
import plistlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
FB = [ROOT / "kexts/NVRMFB/fb/Info.plist", ROOT / "kexts/NVRM/fb/Info.plist"]


def matches(spec, word):
    value, _, mask = spec.partition("&")
    value, mask = int(value, 16), int(mask or "0xffffffff", 16)
    return (word & mask) == (value & mask)


def claims(p, vendor, device, cls, subvendor=0):
    # IOPCIPrimaryMatch looks at the primary ids only (IOPCIMatch would also try the subsystem vendor)
    return matches(p["IOPCIPrimaryMatch"], device << 16 | vendor) and matches(p["IOPCIClassMatch"], cls << 8)


class FBClaim(unittest.TestCase):
    def setUp(self):
        self.claim = plistlib.loads(FB[0].read_bytes())["IOKitPersonalities"]["NVRMFBClaim"]

    def test_both_fb_plists_identical(self):
        self.assertEqual(FB[0].read_bytes(), FB[1].read_bytes())

    def test_same_vendor_and_class_match_as_nvrm(self):
        nvrm = plistlib.loads((ROOT / "kexts/NVRM/Info.plist").read_bytes())["IOKitPersonalities"]["NVRM"]
        for key in ("IOPCIPrimaryMatch", "IOPCIClassMatch"):
            self.assertEqual(self.claim[key], nvrm[key], key)
        self.assertNotIn("IOPCIMatch", self.claim)

    def test_still_outranks_the_firmware_framebuffer(self):
        self.assertEqual(self.claim["IOProviderClass"], "IOPCIDevice")
        self.assertEqual(self.claim["IONameMatch"], "display")
        self.assertEqual(self.claim["IOMatchCategory"], "IOFramebuffer")
        self.assertGreater(self.claim["IOProbeScore"], 20000)   # IONDRVFramebuffer (.Display_boot)

    def test_devices(self):
        cases = [
            ("RTX 5060", 0x10DE, 0x2D05, 0x030000, 0, True),
            ("Quadro T500, 3D controller", 0x10DE, 0x1FBB, 0x030200, 0, True),
            ("Iris Xe (Tiger Lake)", 0x8086, 0x9A49, 0x030000, 0x17AA, False),
            ("Intel GPU, NVIDIA subsystem", 0x8086, 0x9A49, 0x030000, 0x10DE, False),
            ("NVIDIA HD Audio", 0x10DE, 0x22BE, 0x040300, 0, False),
        ]
        for name, vendor, device, cls, sub, want in cases:
            with self.subTest(name):
                self.assertEqual(claims(self.claim, vendor, device, cls, sub), want)


if __name__ == "__main__":
    unittest.main()
