#!/usr/bin/env python3
"""Send logs carries the user's own OpenCore config with the machine's identity removed (10-09: failures on users' own
EFIs could only be guessed from symptoms). Runs the setup's real step on a fake EFI partition."""
import os
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
START = '    cfs=("$mp"/EFI/OC/config.plist)'
END = '  { echo; echo "== NullMoth NVRAM flags'


class EfiUpload(unittest.TestCase):
    def test_config_is_uploaded_with_serials_removed_and_settings_kept(self):
        s = SETUP.read_text()
        self.assertEqual((s.count(START), s.count(END)), (1, 1), "anchors moved in nullmoth-setup.sh")
        t = tempfile.mkdtemp()
        mp, col = os.path.join(t, "esp"), os.path.join(t, "collect")
        os.makedirs(os.path.join(mp, "EFI/OC/Kexts/Lilu.kext"))
        os.makedirs(col)
        with open(os.path.join(mp, "EFI/OC/config.plist"), "wb") as f:
            plistlib.dump({"PlatformInfo": {"Generic": {"SystemProductName": "iMacPro1,1", "SystemSerialNumber": "C02SECRET",
                                                        "MLB": "C02BOARDSECRET", "SystemUUID": "UUID-SECRET", "ROM": b"\x11\x22"}},
                           "Kernel": {"Patch": [{"Comment": "algrey | Fix PAT", "Enabled": True}]}}, f)
        config = pathlib.Path(mp) / 'EFI/OC/config.plist'
        for i in range(40):
            backup = config.with_name(config.name + '.nullmoth-' + str(i))
            backup.write_bytes(config.read_bytes())
            os.utime(backup, (1000 + i, 1000 + i))
        body = s[s.index(START):s.index(END)]
        r = subprocess.run(["/bin/bash", "-c", f'mp="{mp}"; COLLECT="{col}"; n=0; collection_errors=0\nfor d in disk9s1; do\n{body}\necho "n=$n"'],
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("n=3", r.stdout)
        backup_names = sorted(pathlib.Path(col).glob('disk9s1-efi-config.plist.nullmoth-*.txt'))
        self.assertEqual([p.name for p in backup_names], ['disk9s1-efi-config.plist.nullmoth-38.txt', 'disk9s1-efi-config.plist.nullmoth-39.txt'])
        for p in backup_names:
            self.assertNotIn(b'C02SECRET', p.read_bytes())
        up = open(os.path.join(col, "disk9s1-efi-config.plist.txt"), "rb").read()
        for secret in (b"C02SECRET", b"C02BOARDSECRET", b"UUID-SECRET", b"ESI="):
            self.assertNotIn(secret, up)
        self.assertIn(b"iMacPro1,1", up)
        self.assertIn(b"algrey | Fix PAT", up)
        self.assertIn("Lilu.kext", open(os.path.join(col, "disk9s1-efi-files.txt")).read())


if __name__ == "__main__":
    unittest.main()
