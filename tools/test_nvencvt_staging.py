#!/usr/bin/env python3
"""Offline tests for NVEncVT package staging, signing order, and rollback."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
STAGER = ROOT / "package/stage_nvencvt.py"
UPSTREAM_COMMIT = "1e91845d6f37d8bce671d62c14f4390324da3659"
ARTIFACT_VERSION = "v0.9.5-ga10x1"
LOCAL_REVISION = "ga10x-encoding-1"


def write_manifest(root: Path) -> None:
    entries = []
    for path in root.rglob("*"):
        if path.is_file() and path.name != "SHA256SUMS":
            entries.append((path.relative_to(root).as_posix(), hashlib.sha256(path.read_bytes()).hexdigest()))
    (root / "SHA256SUMS").write_text("".join(f"{digest}  {name}\n" for name, digest in sorted(entries)))


def snapshot(root: Path):
    files = {p.relative_to(root).as_posix(): p.read_bytes() for p in root.rglob("*") if p.is_file()}
    dirs = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_dir()}
    return files, dirs


FAKE_CODESIGN = r'''#!/usr/bin/env python3
import os, sys
from pathlib import Path
args=sys.argv[1:]
target=Path(args[-1])
phase='verify' if '--verify' in args else 'sign'
kind='bundle' if target.name=='NVMTLDriver.bundle' else 'library'
with open(os.environ['FAKE_CODESIGN_LOG'],'a') as f: f.write(phase+'-'+kind+'\n')
if phase=='sign' and os.environ.get('FAKE_CODESIGN_FAIL')==kind:
    print('mock signing failure',file=sys.stderr);sys.exit(19)
if phase=='sign':
    if target.is_dir():
        sig=target/'Contents/_CodeSignature/CodeResources';sig.parent.mkdir(parents=True,exist_ok=True);sig.write_text('bundle signature\n')
    else:
        with target.open('ab') as f:f.write(b'\nmock signature\n')
elif kind=='bundle' and not (target/'Contents/_CodeSignature/CodeResources').is_file():
    print('bundle signature missing',file=sys.stderr);sys.exit(20)
elif kind=='library' and b'mock signature' not in target.read_bytes():
    print('library signature missing',file=sys.stderr);sys.exit(21)
'''


class NVEncVTStaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="nvencvt-stage-test-")
        self.root = Path(self.temp.name)
        self.pkg = self.root / "pkgroot"
        self.bundle = self.pkg / "Library/GPUBundles/NVMTLDriver.bundle"
        macos = self.bundle / "Contents/MacOS"
        macos.mkdir(parents=True)
        (self.bundle / "Contents/Info.plist").write_text("fixture plist\n")
        (macos / "NVMTLDriver").write_bytes(b"fixture plugin\n")
        (self.pkg / "licenses").mkdir(parents=True)
        (self.pkg / "licenses/existing-license.txt").write_text("existing\n")
        (self.pkg / "install.sh").write_text("fixture installer\n")
        write_manifest(self.pkg)

        self.build = self.root / "build"
        self.build.mkdir()
        self.library = self.build / "libnvencvt.dylib"
        self.library.write_bytes(b"fixture nvencvt mach-o bytes\n")
        self.metadata_path = self.build / "nvencvt-build-info.json"
        self.metadata = {
            "schema_version": 2,
            "artifact": "libnvencvt.dylib",
            "artifact_version": ARTIFACT_VERSION,
            "local_revision": LOCAL_REVISION,
            "sha256": hashlib.sha256(self.library.read_bytes()).hexdigest(),
            "arch": "x86_64",
            "min_os": "15.0",
            "sdk": "26.5",
            "upstream_repo": "mickys/nvenc",
            "upstream_tag": "v0.9.5",
            "upstream_commit": UPSTREAM_COMMIT,
        }
        self.metadata_path.write_text(json.dumps(self.metadata) + "\n")

        self.bin = self.root / "bin"
        self.bin.mkdir()
        codesign = self.bin / "codesign"
        codesign.write_text(FAKE_CODESIGN)
        codesign.chmod(0o755)
        xattr = self.bin / "xattr"
        xattr.write_text("#!/bin/sh\nexit 0\n")
        xattr.chmod(0o755)
        self.log = self.root / "codesign.log"
        self.env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin", FAKE_CODESIGN_LOG=str(self.log))

    def tearDown(self):
        self.temp.cleanup()

    def run_stage(self, **extra_env):
        return subprocess.run([sys.executable, str(STAGER), str(self.pkg), str(self.build)],
                              env=dict(self.env, **extra_env), capture_output=True, text=True)

    def test_validates_receipt_signs_and_regenerates_full_manifest(self):
        result = self.run_stage()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bundle_lib = self.bundle / "Contents/MacOS/libnvencvt.dylib"
        resources = self.bundle / "Contents/Resources"
        self.assertIn(b"mock signature", bundle_lib.read_bytes())
        self.assertEqual((resources / "nvencvt-build-info.json").read_bytes(), self.metadata_path.read_bytes())
        for source_rel, packaged_name in (
            ("vendor/nvenc/LICENSE", "NVEncVT-MIT-LICENSE.txt"),
            ("vendor/nvenc/THIRD_PARTY_NOTICES.md", "NVEncVT-THIRD_PARTY_NOTICES.md"),
            ("vendor/nvenc/vendor/open-gpu-kernel-modules/COPYING", "NVEncVT-NVIDIA-open-gpu-kernel-modules-COPYING.txt"),
            ("vendor/nvenc/vendor/open-gpu-doc/LICENSE.md", "NVEncVT-open-gpu-doc-LICENSE.md"),
        ):
            source = ROOT / source_rel
            self.assertTrue(source.is_file(), f"missing source notice {source}")
            self.assertEqual((resources / packaged_name).read_bytes(), source.read_bytes())
            self.assertEqual((self.pkg / "licenses" / packaged_name).read_bytes(), source.read_bytes())
        events = self.log.read_text().splitlines()
        self.assertEqual(events[:4], ["sign-library", "verify-library", "sign-bundle", "verify-bundle"])
        self.assertIn("libnvencvt.dylib", (self.pkg / "SHA256SUMS").read_text())
        subprocess.run(["shasum", "-a", "256", "--strict", "-c", "SHA256SUMS", "--quiet"],
                       cwd=self.pkg, check=True)

    def test_bad_build_hash_is_refused_before_package_changes(self):
        before = snapshot(self.pkg)
        self.metadata["sha256"] = "0" * 64
        self.metadata_path.write_text(json.dumps(self.metadata) + "\n")
        result = self.run_stage()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("SHA-256 mismatch", result.stderr)
        self.assertEqual(snapshot(self.pkg), before)
        self.assertFalse(self.log.exists())

    def test_wrong_local_revision_is_refused_before_package_changes(self):
        before = snapshot(self.pkg)
        self.metadata["local_revision"] = "ga10x-encoding-0"
        self.metadata_path.write_text(json.dumps(self.metadata) + "\n")
        result = self.run_stage()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"build metadata local_revision must be {LOCAL_REVISION!r}", result.stderr)
        self.assertEqual(snapshot(self.pkg), before)
        self.assertFalse(self.log.exists())

    def test_package_symlink_that_escapes_root_is_refused_before_signing(self):
        outside = self.root / "outside.txt"
        outside.write_text("not part of pkgroot\n")
        (self.pkg / "foreign-link").symlink_to("../outside.txt")
        write_manifest(self.pkg)
        before = snapshot(self.pkg)
        result = self.run_stage()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("leaves pkgroot", result.stderr)
        self.assertEqual(snapshot(self.pkg), before)
        self.assertFalse(self.log.exists())

    def test_unmanifested_package_file_is_refused_before_signing(self):
        (self.pkg / "unmanifested.txt").write_text("not in base manifest\n")
        before = snapshot(self.pkg)
        result = self.run_stage()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("does not cover this exact package tree", result.stderr)
        self.assertEqual(snapshot(self.pkg), before)
        self.assertFalse(self.log.exists())

    def test_bundle_sign_failure_restores_bundle_licenses_and_manifest(self):
        before = snapshot(self.pkg)
        result = self.run_stage(FAKE_CODESIGN_FAIL="bundle")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("mock signing failure", result.stderr)
        self.assertEqual(snapshot(self.pkg), before)
        self.assertEqual(self.log.read_text().splitlines(), ["sign-library", "verify-library", "sign-bundle"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
