#!/usr/bin/env python3
"""asus_nkey_backlight builds the hid-asus feature reports. The expected bytes are built a second way, from the documented layout,
so the C builder is checked against the description and not against itself."""
import os, shutil, subprocess, sys, tempfile, unittest
HERE = os.path.dirname(os.path.abspath(__file__))

def handshake(report_id):                      # { id, "ASUS Tech.Inc.", 0 } : 16 bytes
    return bytes([report_id]) + b'ASUS Tech.Inc.' + b'\0'

def brightness(level):                          # 64 bytes : { 0x5a, 0xba, 0xc5, 0xc4, level, 0... }
    return bytes([0x5a, 0xba, 0xc5, 0xc4, level]) + bytes(59)

@unittest.skipUnless(shutil.which('cc'), 'needs a C compiler')
class T(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.exe = os.path.join(tempfile.mkdtemp(), 'ank')
        extra = ['-framework', 'IOKit', '-framework', 'CoreFoundation'] if sys.platform == 'darwin' else []
        r = subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', '-o', cls.exe, os.path.join(HERE, 'asus_nkey_backlight.c')] + extra, capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
    def dry(self, *a):
        r = subprocess.run([self.exe, '--dry-run'] + list(a), capture_output=True, text=True)
        return r
    def test_handshake_is_16_bytes_for_each_report_id(self):
        self.assertEqual(len(handshake(0x5a)), 16)
        lines = self.dry('2').stdout.split()
        self.assertEqual([bytes.fromhex(x) for x in lines[:3]], [handshake(0x5a), handshake(0x5d), handshake(0x5e)])
    def test_brightness_report_for_every_level(self):
        for lv in range(4):
            lines = self.dry(str(lv)).stdout.split()
            self.assertEqual(bytes.fromhex(lines[3]), brightness(lv), lv)
            self.assertEqual(len(bytes.fromhex(lines[3])), 64)
    def test_levels_outside_0_to_3_are_refused(self):
        for bad in ('4', '-1', '255', 'x'):
            self.assertEqual(self.dry(bad).returncode, 2, bad)
    def test_the_old_three_byte_guess_is_not_what_is_sent(self):
        # a report of {0x5a, 0xba, 0xc5, level} would be the wrong layout: the constant 0xc4 sits before the level
        self.assertNotEqual(bytes.fromhex(self.dry('1').stdout.split()[3])[3], 1)
    @unittest.skipUnless(sys.platform == 'darwin', 'boot-args gate is macOS only')
    def test_without_nmkbl_in_the_boot_args_nothing_is_sent(self):
        args = subprocess.run(['sysctl', '-n', 'kern.bootargs'], capture_output=True, text=True).stdout
        if 'nmkbl=1' in args: self.skipTest('this machine has nmkbl=1')
        self.assertEqual(subprocess.run([self.exe, '2'], capture_output=True, text=True).returncode, 3)
    def test_the_device_list_is_the_one_hid_asus_gives_a_backlight(self):
        src = open(os.path.join(HERE, 'asus_nkey_backlight.c')).read()
        want = ['0x1854', '0x1837', '0x1869', '0x1866', '0x19b6', '0x18c6', '0x1a30', '0x1abe', '0x1b4c']   # hid-asus QUIRK_USE_KBD_BACKLIGHT
        line = [l for l in src.split('\n') if 'kAsusPids[] =' in l][0]
        self.assertEqual([x.strip(' ,;}') for x in line.split('{')[1].split(',') if x.strip(' ;}')], want)
if __name__ == '__main__': unittest.main()
