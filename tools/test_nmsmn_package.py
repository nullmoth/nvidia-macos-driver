#!/usr/bin/env python3
"""package/stage_nmsmn.sh: the Ryzen power parts ride along in a driver package, and only checked ones do. Arms: a NullMoth
NMSMN kext and a binary with the matching SHA-256 are staged where install.sh's whole-folder copy and the Mac app's setup
look; a wrong hash, a kext with another bundle id, a symlinked binary and a link inside the kext are refused and stage
nothing; the installer never loads NMSMN by itself (it is not in install.sh's kext lists)."""
import hashlib
import pathlib
import plistlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
STAGE = ROOT / "package/stage_nmsmn.sh"
INSTALL = (ROOT / "package/install.sh").read_text()


def fixture(bundle="com.nullmoth.NMSMN", link=False):
    t = pathlib.Path(tempfile.mkdtemp())
    k = t / "NMSMN.kext/Contents"; (k / "MacOS").mkdir(parents=True)
    (k / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": bundle}))
    (k / "MacOS/NMSMN").write_bytes(b"kext")
    if link:
        (k / "MacOS/l").symlink_to("/etc/passwd")
    b = t / "ryzenadj"; b.write_bytes(b"binary")
    return t, hashlib.sha256(b"binary").hexdigest()


def stage(t, sha, binary=None):
    out = t / "pkgroot"
    p = subprocess.run(["/bin/bash", str(STAGE), str(t / "NMSMN.kext"), str(binary or t / "ryzenadj"), sha, str(out)], capture_output=True, text=True)
    return p, out


class Stage(unittest.TestCase):
    def test_checked_parts_are_staged_where_the_installer_and_the_app_look(self):
        t, sha = fixture()
        p, out = stage(t, sha)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertTrue((out / "Library/NullMoth/kexts/NMSMN.kext/Contents/MacOS/NMSMN").exists())
        self.assertEqual((out / "Library/NullMoth/ryzenadj/ryzenadj").read_bytes(), b"binary")

    def test_a_binary_that_does_not_match_stages_nothing(self):
        t, _ = fixture()
        p, out = stage(t, "0" * 64)
        self.assertNotEqual(p.returncode, 0)
        self.assertFalse((out / "Library/NullMoth/ryzenadj").exists())

    def test_another_kext_a_link_and_a_bad_hash_are_refused(self):
        t, sha = fixture(bundle="com.example.Other")
        self.assertNotEqual(stage(t, sha)[0].returncode, 0)
        t, sha = fixture(link=True)
        self.assertNotEqual(stage(t, sha)[0].returncode, 0)
        t, sha = fixture()
        self.assertNotEqual(stage(t, "ABC")[0].returncode, 0)
        link = t / "ryzenadj-link"; link.symlink_to(t / "ryzenadj")
        self.assertNotEqual(stage(t, sha, binary=link)[0].returncode, 0)

    def test_the_installer_does_not_load_it_by_itself(self):
        self.assertNotIn("NMSMN", INSTALL)


if __name__ == "__main__":
    unittest.main()
