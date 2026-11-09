#!/usr/bin/env python3
"""The TUF RGB WMI ids (0x00100056 mode, 0x0010005A mode2, 0x00100057 state) against the keyboard-backlight id 0x00050021, in the
families the census lists. In Linux asus-wmi.c the brightness of the keyboard light is ONLY ever read and written through
ASUS_WMI_DEVID_KBD_BACKLIGHT (kbd_led_read / do_kbd_led_set); the TUF RGB ids carry mode, colour and speed (kbd_rgb_mode: cmd,
mode, r, g, b, speed) and the boot/awake/sleep/shutdown power flags (kbd_rgb_state), not a brightness. So a TUF backend would add
nothing wherever 0x00050021 is present, and these arms prove no family has a TUF id that a bundle without 0x00050021 would need."""
import csv, os, unittest
H = os.path.dirname(os.path.abspath(__file__))
rows = [r for r in csv.DictReader((l for l in open(os.path.join(H, 'fixtures', 'kbl_aml', 'asus_tuf_ids.tsv')) if not l.startswith('#')), delimiter='\t')]
class T(unittest.TestCase):
    def test_state_and_mode_ids_never_appear_without_the_keyboard_id(self):
        for r in rows:
            for k in ('rgb_mode_0x00100056', 'rgb_cmd_0x00100057'):
                self.assertLessEqual(int(r[k]), int(r['kbd_0x00050021']), '%s %s' % (r['family'], k))
    def test_the_families_without_the_keyboard_id_have_no_state_or_mode_id(self):
        for r in rows:
            if int(r['kbd_0x00050021']) == 0:
                self.assertEqual((int(r['rgb_mode_0x00100056']), int(r['rgb_cmd_0x00100057'])), (0, 0), r['family'])
    def test_a15_has_only_mode2_which_is_colour_not_brightness(self):
        a15 = [r for r in rows if r['family'] == 'ASUS TUF Gaming A15'][0]
        self.assertEqual((int(a15['kbd_0x00050021']), int(a15['rgb_mode2_0x0010005A'])), (0, 3))
if __name__ == '__main__': unittest.main()
