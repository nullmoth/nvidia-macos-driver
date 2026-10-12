"""Tahoe preparation moves a desktop to MacPro7,1 in every PlatformInfo section that names the model.

1.14.1 changed PlatformInfo.Generic alone. With PlatformInfo.Automatic off macOS reads the SMBIOS/DataHub sections, so the
Mac stayed iMacPro1,1 and the macOS 26 installer still refused it (chat 10-12). Those sections also carry the board ID.
"""
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
source = (ROOT / "app/Resources/nullmoth-setup.sh").read_text()
start = source.index('if [ "$UPD" = prepare ]; then\n  MK=""')
end = source.index("\n# The macOS installer boots", start)
BLOCK = source[start:end]

HARNESS = r'''
set -u
C="$1"; UPD=prepare; KX=/nonexistent; EDITS=(); ADDK=(); FETCH=()
has() { plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }
get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }
ok() { echo "OK $*"; }
stop() { echo "STOP $*"; exit 3; }
kpos() { echo 0; }
fetch_kext() { :; }
''' + BLOCK + r'''
printf 'EDIT %s\n' "${EDITS[@]+"${EDITS[@]}"}"
'''


def run(platform):
    with tempfile.TemporaryDirectory() as td:
        c = pathlib.Path(td) / "config.plist"
        c.write_bytes(plistlib.dumps({"PlatformInfo": platform}))
        p = subprocess.run(["/bin/bash", "-c", HARNESS, "harness", str(c)], capture_output=True, text=True, timeout=30)
        return p.returncode, [l[5:] for l in p.stdout.splitlines() if l.startswith("EDIT ") and l[5:]], p.stdout


class TahoeModelSwitch(unittest.TestCase):
    def test_every_section_and_board_move_when_automatic_is_off(self):
        rc, edits, out = run({
            "Automatic": False,
            "Generic": {"SystemProductName": "iMacPro1,1"},
            "SMBIOS": {"SystemProductName": "iMacPro1,1", "BoardProduct": "Mac-7BA5B2D9E42DDD94"},
            "DataHub": {"SystemProductName": "iMacPro1,1", "BoardProduct": "Mac-7BA5B2D9E42DDD94"},
        })
        self.assertEqual(rc, 0, out)
        self.assertEqual(sorted(edits), sorted([
            "PlatformInfo.Generic.SystemProductName|-string|MacPro7,1",
            "PlatformInfo.SMBIOS.SystemProductName|-string|MacPro7,1",
            "PlatformInfo.DataHub.SystemProductName|-string|MacPro7,1",
            "PlatformInfo.SMBIOS.BoardProduct|-string|Mac-27AD2F918AE68F61",
            "PlatformInfo.DataHub.BoardProduct|-string|Mac-27AD2F918AE68F61",
        ]), out)

    def test_generic_only_config_changes_generic_only(self):
        rc, edits, out = run({"Automatic": True, "Generic": {"SystemProductName": "iMacPro1,1"}})
        self.assertEqual(rc, 0, out)
        self.assertEqual(edits, ["PlatformInfo.Generic.SystemProductName|-string|MacPro7,1"], out)

    def test_a_mac_already_on_macpro71_changes_nothing(self):
        rc, edits, out = run({"Generic": {"SystemProductName": "MacPro7,1"},
                              "SMBIOS": {"SystemProductName": "MacPro7,1", "BoardProduct": "Mac-27AD2F918AE68F61"}})
        self.assertEqual(rc, 0, out)
        self.assertEqual(edits, [], out)

    def test_a_laptop_on_a_dropped_model_is_stopped_not_changed(self):
        rc, edits, out = run({"Generic": {"SystemProductName": "MacBookPro15,3"}})
        self.assertEqual(rc, 3, out)
        self.assertIn("STOP", out)
        self.assertEqual(edits, [], out)


if __name__ == "__main__":
    unittest.main()
