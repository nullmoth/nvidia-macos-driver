#!/usr/bin/env python3
"""razer_kbd_backlight builds the OpenRazer feature report. The expected bytes here are built a second way, straight from the
documented layout, so the C builder is checked against the description and not against itself."""
import os
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def expected(extended, txn, storage, brightness):
    r = bytearray(90)
    r[1] = txn
    r[5] = 3
    r[6], r[7] = (0x0F, 0x04) if extended else (0x03, 0x03)
    r[8], r[9], r[10] = storage, 0x05, brightness
    c = 0
    for b in r[2:88]:
        c ^= b
    r[88] = c
    return bytes(r)


@unittest.skipUnless(shutil.which('cc'), 'needs a C compiler')
class Razer(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.exe = os.path.join(tempfile.mkdtemp(), 'rz')
        r = subprocess.run(['cc', '-o', cls.exe, os.path.join(HERE, 'razer_kbd_backlight.c'), '-framework', 'IOKit', '-framework', 'CoreFoundation'],
                           capture_output=True, text=True)
        if r.returncode:
            r = subprocess.run(['cc', '-o', cls.exe, os.path.join(HERE, 'razer_kbd_backlight.c')], capture_output=True, text=True)
        assert r.returncode == 0, r.stderr[-400:]

    def dry(self, *args):
        out = subprocess.run([self.exe, '--dry-run', *args], capture_output=True, text=True)
        return out.returncode, bytes.fromhex(out.stdout.strip()) if out.stdout.strip() else b''

    def test_a_standard_device_report_matches_the_documented_layout(self):
        self.assertEqual(self.dry('128'), (0, expected(False, 0xFF, 1, 128)))

    def test_an_extended_device_report_uses_class_0f_command_04(self):
        self.assertEqual(self.dry('--extended', '--txn', '0x1f', '255'), (0, expected(True, 0x1F, 1, 255)))

    def test_off_is_brightness_zero(self):
        rc, rep = self.dry('0')
        self.assertEqual(rep[10], 0)
        self.assertEqual(rep, expected(False, 0xFF, 1, 0))

    def test_the_report_is_90_bytes_and_the_checksum_covers_bytes_2_to_87(self):
        rc, rep = self.dry('77')
        self.assertEqual(len(rep), 90)
        c = 0
        for b in rep[2:88]:
            c ^= b
        self.assertEqual(rep[88], c)
        self.assertEqual(rep[89], 0)

    def test_a_brightness_out_of_range_is_refused(self):
        self.assertEqual(self.dry('300')[0], 2)
        self.assertEqual(self.dry('-1')[0], 2)


if __name__ == '__main__':
    unittest.main()
