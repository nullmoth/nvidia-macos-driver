#!/usr/bin/env python3
"""The driver needs Lilu + AMFIPass on in OpenCore. 1.9.0 logs (10-10, Ryzen 9 9900X, same SIP bits as a working
machine): AMFIPass was in the config but off, WindowServer refused the plugin ("mapping process is a platform binary,
but mapped file is not") and the screen stayed black with a cursor. Runs the setup's real step on fake configs."""
import os
import pathlib
import plistlib
import subprocess
import tempfile
import unittest
import zipfile
import hashlib

SETUP = pathlib.Path(__file__).resolve().parents[1] / "app/Resources/nullmoth-setup.sh"
START = "# AMFIPass (a Lilu plugin) is what lets WindowServer"
END = "# The macOS installer boots with a small GPU BAR"


def kext(name, enabled=True):
    return {"Arch": "Any", "BundlePath": name, "Enabled": enabled, "ExecutablePath": f"Contents/MacOS/{name[:-5]}",
            "PlistPath": "Contents/Info.plist", "Comment": "", "MaxKernel": "", "MinKernel": ""}


def fake_download(t, name, good=True):
    """A local curl that serves a zip of <name>; the pinned SHA-256 is swapped for the fake's own when good=True."""
    z = os.path.join(t, name + ".zip")
    with zipfile.ZipFile(z, "w") as f:
        f.writestr(f"{name}/Contents/Info.plist", "<plist/>")
    b = os.path.join(t, "bin"); os.makedirs(b, exist_ok=True)
    with open(os.path.join(b, "curl"), "w") as f:
        f.write(f'#!/bin/bash\nwhile [ $# -gt 0 ]; do [ "$1" = -o ] && {{ cp "{z}" "$2"; exit 0; }}; shift; done; exit 22\n')
    os.chmod(os.path.join(b, "curl"), 0o755)
    return b, hashlib.sha256(open(z, "rb").read()).hexdigest() if good else "0" * 64


def run(kexts, folders=("Lilu.kext", "AMFIPass.kext"), download=None):
    s = SETUP.read_text()
    assert s.count(START) == 1 and s.count(END) == 1, "anchors moved in nullmoth-setup.sh"
    t = tempfile.mkdtemp()
    oc = os.path.join(t, "EFI/OC")
    for f in folders:
        os.makedirs(os.path.join(oc, "Kexts", f))
    cfg = os.path.join(oc, "config.plist")
    with open(cfg, "wb") as fh:
        plistlib.dump({"Kernel": {"Add": kexts}}, fh)
    hs, he = "# Kexts the driver's OpenCore setup needs", "booted_part() {"
    assert s.count(hs) == 1 and s.count(he) == 1, "helper anchors moved in nullmoth-setup.sh"
    block = 'KT=""\n' + s[s.index(hs):s.index(he)] + s[s.index(START):s.index(END)]
    env = dict(os.environ)
    if download:
        b, sha = fake_download(t, *download)
        env["PATH"] = b + ":" + env["PATH"]
        for real in ("261aebb9dc83adb6515c96405653f53c3c50b09c89babdb91fb41f7b33a1cd2a",
                     "07b266145906db41f4b13a7938fbb173ea28888cc1fa65f84417f8820adc961e"):
            block = block.replace(real, sha)
    script = (f'C="{cfg}"; EDITS=(); has() {{ plutil -extract "$1" raw -o - "$C" >/dev/null 2>&1; }}\n'
              'get() { plutil -extract "$1" raw -o - "$C" 2>/dev/null; }\n'
              'stop() { echo "STOP $*"; exit 3; }\n'
              + block
              + '\necho "EDITS ${EDITS[*]+${EDITS[*]}}"; echo "NEEDAMFIPASS $NEEDAMFIPASS"\n')
    r = subprocess.run(["/bin/bash", "-c", script], capture_output=True, text=True, timeout=60, env=env)
    return r.returncode, r.stdout


class AmfiPass(unittest.TestCase):
    def test_amfipass_present_but_off_is_turned_on(self):
        rc, out = run([kext("Lilu.kext"), kext("AMFIPass.kext", False)])
        self.assertEqual(rc, 0, out)
        self.assertIn("CHANGE Kernel -> Add: turn AMFIPass.kext on", out)
        self.assertIn("EDITS Kernel.Add.1.Enabled|-bool|true", out)

    def test_negative_control_both_on_changes_nothing(self):
        rc, out = run([kext("Lilu.kext"), kext("AMFIPass.kext")])
        self.assertEqual(rc, 0, out)
        self.assertNotIn("CHANGE", out)
        self.assertIn("NEEDAMFIPASS 0", out)

    def test_missing_entry_with_the_kext_on_disk_is_added(self):
        rc, out = run([kext("Lilu.kext")])
        self.assertEqual(rc, 0, out)
        self.assertIn("NEEDAMFIPASS 1", out)

    def test_no_amfipass_anywhere_is_downloaded_and_added(self):
        # 1.10 stopped here and sent users to a 1401 rebuild that also lacked it (10-10, 9 uploads)
        rc, out = run([kext("Lilu.kext")], folders=("Lilu.kext",), download=("AMFIPass.kext", True))
        self.assertEqual(rc, 0, out)
        self.assertIn("CHANGE Kexts: add AMFIPass.kext", out)
        self.assertIn("NEEDAMFIPASS 2", out)

    def test_negative_control_a_download_with_the_wrong_hash_stops_before_any_change(self):
        rc, out = run([kext("Lilu.kext")], folders=("Lilu.kext",), download=("AMFIPass.kext", False))
        self.assertEqual(rc, 3, out)
        self.assertIn("did not match its SHA-256; nothing was changed", out)

    def test_no_lilu_either_both_are_added(self):
        rc, out = run([], folders=("AMFIPass.kext",), download=("Lilu.kext", True))
        self.assertEqual(rc, 0, out)
        self.assertIn("CHANGE Kernel -> Add: add Lilu.kext", out)
        self.assertIn("CHANGE Kernel -> Add: add AMFIPass.kext", out)

    def test_amfipass_before_lilu_stops(self):
        rc, out = run([kext("AMFIPass.kext"), kext("Lilu.kext")])
        self.assertEqual(rc, 3, out)
        self.assertIn("loads before Lilu.kext", out)


if __name__ == "__main__":
    unittest.main()
