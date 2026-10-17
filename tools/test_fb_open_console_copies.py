#!/usr/bin/env python3
"""No copy of the boot console can fault when WindowServer opens the framebuffer.

A kernel panic in WindowServer -> IOFramebuffer::open -> postOpen -> memcpy (source unmapped, 1920x1080x4 bytes) came
from WhateverGreen: Lilu routes IOFramebuffer::initFB to WhateverGreen, whose first call copies the boot console from
the address it saved at boot, and macOS can free that mapping before the NVIDIA display opens. gfxrst=4 turns that
copy off. These arms keep the driver's own side clean: nothing of ours runs around postOpen, no copy reads a console
pointer, a console on another GPU is never mapped, and both the driver and the installer name the WhateverGreen setting.

Each arm also runs against a source that has the defect and must fire there."""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
FB = ROOT / "kexts/NVRMFB/fb/nvrm-fb.cpp"
NVRM = ROOT / "kexts/NVRM/NVRM.cpp"
INSTALL = ROOT / "package/install.sh"

COPY_CALL = re.compile(r"\b(memcpy|memmove|bcopy|bcopy_nc)\s*\(")
CONSOLE_WORDS = re.compile(r"\b(cons\w*|fCons\w*|v_baseAddr|v_baseaddr|consoleVinfo|vinfo)\b")


def call_args(src, start):
    """The argument text of the call whose '(' is at start, split on top-level commas."""
    depth, args, cur = 0, [], []
    for ch in src[start:]:
        if ch == "(":
            depth += 1
            if depth == 1:
                continue
        elif ch == ")":
            depth -= 1
            if depth == 0:
                args.append("".join(cur))
                return args
        elif ch == "," and depth == 1:
            args.append("".join(cur))
            cur = []
            continue
        cur.append(ch)
    return args


def console_copies(src):
    """Copies whose source argument names a console pointer: memcpy/memmove(dst, src, n), bcopy(src, dst, n)."""
    found = []
    for m in COPY_CALL.finditer(src):
        args = call_args(src, m.end() - 1)
        if len(args) < 3:
            continue
        source = args[0] if m.group(1).startswith("bcopy") else args[1]
        if CONSOLE_WORDS.search(source):
            found.append(f"{m.group(1)}({', '.join(a.strip() for a in args)})")
    return found


def has_open_override(src):
    return bool(re.search(r"IOReturn\s+(NVRMNVDAFramebuffer::)?open\s*\(\s*(void)?\s*\)", src))


def maps_foreign_console(src):
    return "withAddressRange(fConsPhys" in src


def function_body(src, signature):
    i = src.index(signature)
    j = src.index("{", i)
    depth = 0
    for k in range(j, len(src)):
        depth += {"{": 1, "}": -1}.get(src[k], 0)
        if depth == 0:
            return src[j:k + 1]
    raise AssertionError(f"unterminated body for {signature}")


NOBOOT_RETURN = re.compile(r'PE_parse_boot_argn\("-nvrmnobootscreen"[^;]*\)\)\s*\{[^{}]*\breturn;\s*\}')
FOREIGN_RETURN = re.compile(r"if \(const char \*(\w+) = otherDisplayHolding\(fPCI, phys, size\)\)\s*\{[^{}]*\breturn;\s*\}")


def snapshot_checks_before_mapping(body):
    """-nvrmnobootscreen and another GPU's console both return before the console is mapped."""
    m = body.find("withAddressRange(phys")
    off, foreign = NOBOOT_RETURN.search(body), FOREIGN_RETURN.search(body)
    return m > 0 and bool(off) and bool(foreign) and off.end() <= m and foreign.end() <= m


def holder(windows, phys, size):
    """The Python twin of otherDisplayHolding's range test: the first window that holds the whole console."""
    for name, pa, length in windows:
        if phys >= pa and size <= length and phys - pa <= length - size:
            return name
    return None


def fb_warns_about_weg(src):
    return ('getMetaClassWithName' in src and 'withCStringNoCopy("WhateverGreen")' in src and '"gfxrst"' in src
            and "gfxrst == 4" in src)


def installer_warns_about_weg(src):
    return "as.vit9696.WhateverGreen" in src and "gfxrst=4" in src


class OpenPathCopies(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fb, cls.nvrm, cls.install = FB.read_text(), NVRM.read_text(), INSTALL.read_text()

    def test_nothing_of_ours_wraps_ioframebuffer_open(self):
        # The build in the panics overrode open() around IOFramebuffer::open; release builds must not bring it back.
        self.assertFalse(has_open_override(self.fb))
        self.assertTrue(has_open_override("IOReturn NVRMNVDAFramebuffer::open()\n{\n    return super::open();\n}\n"),
                        "control: an open() override must be detected")

    def test_no_copy_reads_a_console_pointer_in_the_framebuffer(self):
        self.assertEqual(console_copies(self.fb), [])

    def test_no_copy_reads_a_console_pointer_in_nvrm(self):
        self.assertEqual(console_copies(self.nvrm), [])

    def test_a_console_copy_is_detected(self):
        bad = ("memcpy(callbackBuffer, reinterpret_cast<uint8_t *>(cons.v_baseAddr), rowBytes * height);\n"
               "bcopy((void *)fConsKva, dst, n);\nmemcpy(&word, pOutData, sizeof(word));\n")
        found = console_copies(bad)
        self.assertEqual(len(found), 2, found)

    def test_a_console_on_another_gpu_is_not_mapped(self):
        self.assertFalse(maps_foreign_console(self.fb))
        self.assertTrue(maps_foreign_console(
            "if (!cm) cm = IOMemoryDescriptor::withAddressRange(fConsPhys, (IOByteCount)fConsSize, 0, NULL);"),
            "control: the old fallback mapping must be detected")

    def test_snapshot_skips_before_mapping(self):
        body = function_body(self.nvrm, "void NVRM::snapshotBootScreen()")
        self.assertTrue(snapshot_checks_before_mapping(body))
        no_return = re.sub(r"(otherDisplayHolding\(fPCI, phys, size\)\)\s*\{[^{}]*?)\breturn;", r"\1", body, count=1)
        self.assertNotEqual(no_return, body)
        self.assertFalse(snapshot_checks_before_mapping(no_return), "control: a check that does not return must fail")
        never = body.replace("if (const char *other = otherDisplayHolding", "if (false && otherDisplayHolding")
        self.assertFalse(snapshot_checks_before_mapping(never), "control: a disabled check must fail")
        late = body.replace("withAddressRange(phys", "withPhysicalRange(phys", 1)
        self.assertIn("static const char *otherDisplayHolding(", self.nvrm.split("void NVRM::snapshotBootScreen()")[0],
                      "otherDisplayHolding is defined before its caller")
        self.assertIn("phys >= pa && size <= len && phys - pa <= len - size",
                      function_body(self.nvrm, "static const char *otherDisplayHolding("),
                      "the range test is the one test_console_window_range_test exercises")
        self.assertFalse(snapshot_checks_before_mapping(late), "control: a body whose mapping is not found must fail")

    def test_console_window_range_test(self):
        # Tiger Lake laptop: the console in the Intel GPU's aperture, this card's BARs elsewhere.
        igpu = [("IGPU", 0x4000000000, 256 << 20)]
        self.assertEqual(holder(igpu, 0x4000000000, 7680 * 1080), "IGPU")
        self.assertIsNone(holder(igpu, 0x6000000000, 7680 * 1080), "a console outside every window is nobody's")
        self.assertIsNone(holder(igpu, 0x4000000000 + (256 << 20) - 4096, 7680 * 1080),
                          "a console running past the window end is not held")
        self.assertIsNone(holder([("IGPU", 0x4000000000, 4096)], 0x4000000000, 7680 * 1080))

    def test_framebuffer_names_the_whatevergreen_setting(self):
        self.assertTrue(fb_warns_about_weg(self.fb))
        self.assertFalse(fb_warns_about_weg(self.fb.replace("gfxrst == 4", "gfxrst == 1")),
                         "control: a check for the wrong value must fail")

    def test_installer_names_the_whatevergreen_setting(self):
        self.assertTrue(installer_warns_about_weg(self.install))
        self.assertFalse(installer_warns_about_weg(self.install.replace("as.vit9696.WhateverGreen", "x")),
                         "control: an installer without the check must fail")


if __name__ == "__main__":
    unittest.main()
