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

    def test_staging_refreshes_sha256sums_and_the_package_gate_requires_the_parts(self):
        t, sha = fixture()
        out = t / "pkgroot"; out.mkdir()
        (out / "SHA256SUMS").write_text("0000  ./other\nffff  ./Library/NullMoth/ryzenadj/ryzenadj\n")
        p, _ = stage(t, sha)
        self.assertEqual(p.returncode, 0, p.stderr)
        sums = (out / "SHA256SUMS").read_text()
        self.assertIn("./other", sums)
        self.assertNotIn("ffff", sums)
        self.assertIn("./Library/NullMoth/kexts/NMSMN.kext/Contents/MacOS/NMSMN", sums)
        self.assertEqual(sums.count("Library/NullMoth/ryzenadj/ryzenadj"), 1)
        # the gate: only the NMSMN part of check_package is exercised here (the codec library arms are in test_package_nvencvt)
        gate = (ROOT / "package/check_package.sh").read_text()
        self.assertIn("the Ryzen power parts are missing", gate)
        self.assertIn("SHA256SUMS has no matching line for $f", gate)
        self.assertIn("com.nullmoth.NMSMN", gate)

    def test_the_gate_fails_without_the_parts_and_passes_with_them(self):
        import shutil
        if shutil.which("clang") is None:
            self.skipTest("needs clang")
        t, sha = fixture()
        root = t / "pkgroot"; m = root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS"; m.mkdir(parents=True)
        (t / "x.c").write_text("int NVEncVT_Register(int r){return r;}\n")
        subprocess.run(["clang", "-arch", "x86_64", "-dynamiclib", "-o", str(m / "libnvencvt.dylib"), str(t / "x.c")], check=True, capture_output=True)
        subprocess.run(["codesign", "-f", "-s", "-", str(m / "libnvencvt.dylib")], check=True, capture_output=True)
        rel = "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
        (root / "SHA256SUMS").write_text(f"{hashlib.sha256((m / 'libnvencvt.dylib').read_bytes()).hexdigest()}  ./{rel}\n")
        gate = lambda: subprocess.run(["bash", str(ROOT / "package/check_package.sh"), str(root)], capture_output=True, text=True)
        r = gate()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("Ryzen power parts are missing", r.stdout)
        self.assertEqual(stage(t, sha)[0].returncode, 0)
        shutil.copy(ROOT / "package/sums_complete.sh", root / "sums_complete.sh")
        (m / "nvmtl-air-dis").write_bytes(b"#!/bin/sh\n"); (m / "nvmtl-air-dis").chmod(0o755)
        subprocess.run(["bash", str(ROOT / "package/sums_refresh.sh"), str(root), "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/nvmtl-air-dis"], check=True, capture_output=True)
        subprocess.run(["bash", str(ROOT / "package/sums_refresh.sh"), str(root), "sums_complete.sh"], check=True, capture_output=True)
        self.assertEqual(gate().returncode, 0, gate().stdout)
        (root / "Library/NullMoth/ryzenadj/ryzenadj").write_bytes(b"changed")
        r = gate()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("no matching line", r.stdout)

    def test_the_installer_does_not_load_it_by_itself(self):
        self.assertNotIn("NMSMN", INSTALL)


if __name__ == "__main__":
    unittest.main()
