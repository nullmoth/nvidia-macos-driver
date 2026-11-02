#!/usr/bin/env python3
"""The optional NVEncVT library must follow the plugin bundle through install and rollback.

The uninstall script deliberately works on the whole NVMTLDriver.bundle. This test
checks that contract so the dylib does not acquire a second, independently managed
system path.
"""
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("installer_fixture", ROOT / "tools/primary_install_fixture.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class NVEncVTBundleLifecycle(fixture.Install):
    def test_install_carries_library_inside_the_plugin_bundle(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        installed = self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
        self.assertEqual(installed.read_bytes(), b"new-libnvencvt")

    def test_failed_update_restores_the_previous_nested_library(self):
        result = self.run_install(FAKE_LIVE_FAIL="1")
        self.assert_restored(result)
        installed = self.root / "Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"
        self.assertEqual(installed.read_bytes(), b"old-libnvencvt")

    def test_uninstall_and_uninstall_rollback_cover_the_whole_bundle(self):
        source = (ROOT / "package/uninstall.sh").read_text()
        bundle_loop = "for b in NVMTLDriver.bundle NVIDIAShared.bundle nvmtl nvmtl-allow.txt; do"
        self.assertGreaterEqual(source.count(bundle_loop), 3, "backup, removal, and rollback must all cover the bundle")
        self.assertIn('rm -rf "$GB/$b"', source)
        self.assertIn('ditto "$RB/$b" "$GB/$b"', source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
