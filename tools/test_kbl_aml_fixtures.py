#!/usr/bin/env python3
"""The keyboard-backlight methods the ACPI census listed as unhandled (KBLS, SKBL), read from the real disassembled DSDTs of the
families concerned (tools/fixtures/kbl_aml/, excerpts of iasl -d output). Each arm states what the method does, so a backend is
built only where the body is a plain write or a call into something already driven."""
import os, re, unittest
H = os.path.dirname(os.path.abspath(__file__))
def fx(n): return open(os.path.join(H, 'fixtures', 'kbl_aml', n)).read()
def body(text, name):
    m = re.search(r'Method \(%s, \d, \w+\)\s*\{' % name, text)
    assert m, name
    i, depth = m.end(), 1
    while depth:
        depth += {'{': 1, '}': -1}.get(text[i], 0); i += 1
    return text[m.end():i - 1]
class T(unittest.TestCase):
    def test_thinkpad_mlcg_mlcs_are_wrappers_over_kbls(self):
        t = fx('thinkpad_kbls.dsl')
        self.assertIn('KBLS (Zero, Zero)', body(t, 'MLCG'))                      # the get: command 0
        self.assertIn('KBLS (One, Arg0)', body(t, 'MLCS'))                       # the set: command 1 with the level word
        self.assertIn('SMI (0x14, 0x09, Arg0, Arg1, Zero)', body(t, 'KBLS'))     # KBLS itself is the firmware call, nothing else
        self.assertIn('Return (Local0)', body(t, 'MLCS'))                        # MLCS returns the status with bit 31 as the error
        self.assertIn('0x80000000', body(t, 'MLCS'))
    def test_asus_kbls_is_a_read_only_getter(self):
        b = body(fx('asus_kbls.dsl'), 'KBLS')
        self.assertIn('Return (DUBF)', b)
        for field in ('KBLM', 'KBS1', 'KBS2', 'KBS3', 'KBSP', 'KBBS'):
            self.assertRegex(b, r'DUBF \[\w+\] = %s' % field)                    # every EC field is only READ into the buffer
        self.assertNotRegex(b, r'\b(KBLM|KBS1|KBS2|KBS3|KBSP|KBBS)\s*=')        # and none is written
    def test_honor_skbl_is_a_stub(self):
        b = body(fx('skbl.dsl'), 'SKBL')
        self.assertIn('STAT = One', b); self.assertNotIn('QSMI', b); self.assertNotRegex(b, r'EC|ECWT')
    def test_yoga_skbl_is_an_smi_helper(self):
        t = fx('skbl.dsl')
        yoga = t[t.index('Yoga 14sARE'):]
        self.assertIn('QSMI (0x14, 0x04, Arg0, Zero, Zero)', body(yoga, 'SKBL'))
if __name__ == '__main__': unittest.main()
