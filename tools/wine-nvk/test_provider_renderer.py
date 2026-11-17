#!/usr/bin/env python3
"""Exercise provider renderer upgrades with real signed Mach-O fixtures."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("provider", REPO / "nvk/wine-macos/provider.py")
provider = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provider)


class RendererUpgradeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory(prefix="provider-renderer-test-")
        cls.root = Path(cls.temporary.name)
        source = cls.root / "fixture.c"
        source.write_text("int fixture(void) { return REVISION; }\n")
        for revision in (1, 2):
            binary = cls.root / ("renderer-%d.dylib" % revision)
            subprocess.run(["xcrun", "clang", "-dynamiclib", "-arch", "x86_64",
                            "-mmacosx-version-min=15.0", "-DREVISION=%d" % revision,
                            "-Wl,-install_name,@rpath/libvulkan_nouveau.dylib",
                            str(source), "-o", str(binary)], check=True)
            subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)],
                           check=True, capture_output=True)
        cls.original = cls.root / "renderer-2.dylib"
        cls.private = cls.root / "libnvk_renderer.dylib"
        shutil.copyfile(cls.original, cls.private)
        subprocess.run(["install_name_tool", "-id", "@loader_path/libnvk_renderer.dylib", str(cls.private)],
                       check=True, capture_output=True)
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(cls.private)],
                       check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=self.root)
        self.frontend = Path(self.directory.name)
        (self.frontend / "lib").mkdir()
        shutil.copyfile(self.original, self.frontend / provider.CRITICAL[0])
        shutil.copyfile(self.private, self.frontend / provider.CRITICAL[1])
        self.record = {key: {"path": relative, "sha256": provider.digest(self.frontend / relative)}
                       for key, relative in zip(("entry", "renderer"), provider.CRITICAL[:2])}
        # The build-guard cases exercise provenance after the base runtime check.
        checker = self.frontend / "bin/nvk-runtime"
        checker.parent.mkdir()
        checker.write_text("#!/bin/sh\nexit 0\n")
        checker.chmod(0o755)
        self.save()
        self.addCleanup(self.directory.cleanup)

    def save(self):
        (self.frontend / "BUILD.json").write_text(json.dumps(self.record))

    def test_same_renderer_still_works(self):
        record, update = provider.validate_frontend(self.frontend, self.private)
        self.assertEqual(record, self.record)
        self.assertIsNone(update)

    def test_upgrade_requires_explicit_source(self):
        with self.assertRaisesRegex(ValueError, "select --renderer-source"):
            provider.validate_frontend(self.frontend, self.root / "renderer-1.dylib")

    def test_explicit_upgrade_verifies_derivation_and_records_identity(self):
        _, update = provider.validate_frontend(self.frontend, self.root / "renderer-1.dylib", self.original)
        self.assertEqual(update["source_sha256"], provider.digest(self.original))
        self.assertEqual(update["private_sha256"], provider.digest(self.private))

    def test_wrong_upstream_renderer_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "does not derive"):
            provider.validate_frontend(self.frontend, self.private, self.root / "renderer-1.dylib")

    def test_stale_build_hash_is_rejected(self):
        with (self.frontend / provider.CRITICAL[1]).open("ab") as stream:
            stream.write(b"changed")
        with self.assertRaisesRegex(ValueError, "does not match its build record"):
            provider.validate_frontend(self.frontend, self.private, self.original)

    def test_symlink_is_rejected(self):
        binary = self.frontend / provider.CRITICAL[1]
        binary.unlink()
        binary.symlink_to(self.private)
        with self.assertRaisesRegex(ValueError, "does not match its build record"):
            provider.validate_frontend(self.frontend, self.private, self.original)

    def test_rewritten_hash_does_not_hide_a_broken_signature(self):
        binary = self.frontend / provider.CRITICAL[1]
        data = bytearray(binary.read_bytes())
        data[1024] ^= 1
        binary.write_bytes(data)
        self.record["renderer"]["sha256"] = provider.digest(binary)
        self.save()
        with self.assertRaises(subprocess.CalledProcessError):
            provider.validate_frontend(self.frontend, self.private, self.original)

    def test_provider_build_rejects_missing_hybrid_frontend_provenance(self):
        base = self.frontend
        (base / "nvk-wine-runtime.json").write_text(json.dumps({
            "dxvk": {"version": "3.1.1+nvk-shared-fallback"}}))
        output = base.with_name(base.name + "-must-not-be-created") / "provider"
        with self.assertRaisesRegex(ValueError, "retain hybrid GPU support"):
            provider.build(base, output)
        self.assertFalse(output.parent.exists())

    def test_provider_build_checks_both_inherited_and_explicit_frontend_sources(self):
        base = self.frontend
        (base / "nvk-wine-runtime.json").write_text(json.dumps({
            "dxvk": {"version": "3.1.1+nvk-shared-fallback"},
            "frontend_build": "FRONTEND-BUILD.json"}))
        stale = {**self.record, "schema_version": 1,
                 "source_files": {"nvk/native-macos/bridge.mm": "0" * 64}}
        for explicit in (False, True):
            with self.subTest(explicit=explicit):
                record = base / ("BUILD.json" if explicit else "FRONTEND-BUILD.json")
                record.write_text(json.dumps(stale))
                output = base.with_name(base.name + "-must-not-be-created") / "provider"
                with self.assertRaisesRegex(ValueError, "Wine frontend source mismatch"):
                    provider.build(base, output, frontend=base if explicit else None)
                self.assertFalse(output.parent.exists())


if __name__ == "__main__":
    unittest.main()
