#!/usr/bin/env python3
"""census_kbl_coverage.py classifies synthetic census rows the way each backend's probe would."""
import os, sys, csv, io, unittest, importlib.util
here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('cov', os.path.join(here, 'census_kbl_coverage.py')); cov = importlib.util.module_from_spec(spec); spec.loader.exec_module(cov)
def row(vendor='X', fam='F', g='', v='', k='', e=''):
    return {'vendor': vendor, 'family': fam, cov.G: g, cov.V: v, cov.K: k, cov.E: e}
class T(unittest.TestCase):
    def test_asus_needs_the_keyboard_id(self):
        r = row(g='1 ASUS management (ASUS_WMI_MGMT_GUID) [x]', v='ASUS_WMNB_devids:0x00050011=1/1')
        self.assertEqual(cov.classify(r)['covered'], []); self.assertIn('asus-wmi', cov.classify(r)['refused'][0])
        r = row(g='1 ASUS management (ASUS_WMI_MGMT_GUID) [x]', v='ASUS_WMNB_devids:0x00050021,0x00050011=1/1')
        self.assertEqual(cov.classify(r)['covered'], ['asus-wmi'])
    def test_ideapad_kinds(self):
        self.assertEqual(cov.classify(row(v='Lenovo_VPC2004=1/1;Lenovo_KBLC=1/1;Lenovo_HALS=1/1;Lenovo_SALS=1/1'))['covered'], ['ideapad-kblc'])
        self.assertEqual(cov.classify(row(v='Lenovo_VPC2004=1/1;Lenovo_HALS=1/1;Lenovo_SALS=1/1'))['covered'], ['ideapad-plain'])
        self.assertEqual(cov.classify(row(v='Lenovo_HALS=1/1;Lenovo_SALS=1/1'))['covered'], [])
    def test_dell_needs_the_descriptor_and_razer_is_declined(self):
        smb = 'A80593CE-A997-11DA-B012-B622A1EF5492 Dell SMBIOS call (DELL_WMI_SMBIOS_GUID) [x]'
        desc = ' | 8D9DDCBC-A997-11DA-B012-B622A1EF5492 unlabelled [no source]'
        self.assertEqual(cov.classify(row(g=smb + desc))['covered'], ['dell-smbios'])
        r = cov.classify(row(vendor='RAZER', g=smb))
        self.assertEqual(r['covered'], []); self.assertIn('descriptor', r['refused'][0])
    def test_msi_needs_the_ec_field(self):
        g = 'ABBC0F6E-8EA1-11D1-00A0-C90629100000 MSI platform WMI (MSI_PLATFORM_GUID) [x]'
        self.assertTrue(cov.classify(row(g=g, e='EC.KBBL@236.1/1b=1/1'))['covered'][0].startswith('msi-ec'))
        self.assertEqual(cov.classify(row(g=g))['covered'], [])
    def test_unhandled_method_names(self):
        self.assertEqual(cov.method_names(row(k='EC0.KBLS(0)->=4/6;MFFD.KBLS(2)->=1/1')), ['EC0.KBLS(0)', 'MFFD.KBLS(2)'])
if __name__ == '__main__': unittest.main()
