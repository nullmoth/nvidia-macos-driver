#!/usr/bin/env python3
"""Real package copies/hashes, with only Apple's build tools simulated.

This validates packaging on Windows/Linux too; it does not test macOS signing
policy, an actual kmutil collection build, or GPU operation.
"""
import argparse
import hashlib
import importlib.util
import io
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("stage_package", ROOT / "build/stage_package.py")
STAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(STAGE)


class StagePackage(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="nullmoth stage tests ")
        self.root = Path(self.temp.name).resolve()
        self.base = self.root / "upstream pkgroot"
        self.output = self.root / "fork pkgroot"
        self.build = self.root / "built outputs"
        self.build.mkdir()
        self.calls = []
        for name in ("install.sh", "uninstall.sh", "nullmoth-runtime-check.sh", "nullmoth-runtime-check"):
            self.put(self.base / name, b"upstream tool: " + name.encode())
        self.put(self.base / "VERSION", b"1.14.7\n")
        self.put(self.base / "Users/Shared/nvfw/firmware.bin", b"firmware")
        self.put(self.base / "Library/GPUBundles/nvmtl-allow.txt", b"WindowServer\n")
        self.put(self.base / "Library/GPUBundles/NVIDIAShared.bundle/Contents/resource", b"shared resource")
        self.put(self.base / "payload with spaces.txt", b"other payload")
        for relative, name in ((STAGE.PLUGIN, "NVMTLDriver"), (STAGE.FRAMEBUFFER, "NVRMFB"),
                               (STAGE.ACCEL["15"], "NVAccel"), (STAGE.ACCEL["26"], "NVAccel"),
                               (Path("Library/Extensions/NVRM.kext"), "NVRM"),
                               (Path("Library/Extensions/NVRMAGDC.kext"), "NVRMAGDC")):
            self.make_bundle(self.base / relative, name, b"upstream binary")
        self.put(self.base / STAGE.PLUGIN / "Contents/Resources/preserved.txt", b"plugin resources")
        self.put(self.base / STAGE.TRANSLATOR, b"old canonical translator")
        self.put(self.base / STAGE.BUNDLED_TRANSLATOR, b"old preferred translator")
        self.put(self.base / STAGE.RUNTIME / "libvulkan_nouveau.dylib", b"old NVK")
        self.plugin = self.put(self.build / "NVMTLDriver", b"new plugin")
        self.translator = self.put(self.build / "libnvmtl_translate.dylib", b"new translator")
        self.plist = self.put(self.build / "Info.plist", (ROOT / "kexts/NVRMFB/fb/Info.plist").read_bytes())
        self.args = argparse.Namespace(base=self.base, output=self.output, plugin=self.plugin,
                                      translator=self.translator, framebuffer_plist=self.plist,
                                      accel_26=None)
        self.seal_base()

    def test_loader_helper_is_required_before_any_output_is_published(self):
        self.plugin.write_bytes(b"new plugin using nvmtl-air-dis")
        with mock.patch.object(STAGE, "run", self.fake_run), mock.patch.object(STAGE.sys, "platform", "darwin"):
            with self.assertRaises((ValueError, FileNotFoundError)):
                STAGE.stage(self.args)
        self.assertFalse(self.output.exists())

    def test_loader_helper_is_copied_and_old_air_opt_removed(self):
        self.plugin.write_bytes(b"new plugin using nvmtl-air-dis")
        helper=self.put(self.build / "nvmtl-air-dis", b"open-source decoder")
        helper.chmod(0o755)
        self.args.air_dis=helper
        self.put(self.base / STAGE.RUNTIME / "air-opt", b"obsolete tool")
        self.seal_base()
        with mock.patch.object(STAGE, "run", self.fake_run), mock.patch.object(STAGE.sys, "platform", "darwin"), redirect_stdout(io.StringIO()):
            STAGE.stage(self.args)
        self.assertEqual((self.output / STAGE.PLUGIN / "Contents/MacOS/nvmtl-air-dis").read_bytes(), helper.read_bytes())
        self.assertFalse((self.output / STAGE.RUNTIME / "air-opt").exists())
        STAGE.verify_manifest(self.output)

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def put(path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def make_bundle(self, path, name, data):
        self.put(path / "Contents/Info.plist", plistlib.dumps({"CFBundleExecutable": name, "CFBundleIdentifier": "com.nullmoth." + name}))
        self.put(path / "Contents/MacOS" / name, data)
        return path

    @staticmethod
    def snapshot(root):
        return {p.relative_to(root).as_posix(): os.readlink(p) if p.is_symlink() else p.read_bytes()
                for p in root.rglob("*") if p.is_symlink() or p.is_file()}

    def seal_base(self):
        for relative in STAGE.ACCEL.values():
            STAGE.write_manifest(self.base / relative.parent)
        STAGE.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.original_build = self.snapshot(self.build)

    def fake_run(self, *args, check=True):
        args = tuple(str(a) for a in args)
        self.calls.append(args)
        tool, status, stdout = args[0], 0, ""
        if tool == "git":
            stdout = "feed123\n"
        elif tool == "lipo":
            self.assertEqual(args[2:], ("-verify_arch", "x86_64"))
            status = int(Path(args[1]).read_bytes().startswith(b"ARM64"))
        elif tool == "codesign":
            if args[1] == "--force":
                self.assertEqual(args[1:6], ("--force", "--sign", "-", "--timestamp=none", args[5]))
                self.assertTrue(Path(args[5]).is_dir())
            else:
                self.assertEqual(args[1:3], ("--verify", "--strict"))
                target = Path(args[3])
                status = int(target.is_file() and target.read_bytes().startswith(b"BAD-SIGNATURE"))
        elif tool.endswith("/nullmoth-runtime-check"):
            self.assertEqual(args[1], "15.5")
            status = int(Path(args[2]).read_bytes().startswith(b"MINOS26"))
        elif tool.endswith("/bin/nvk-runtime"):
            self.assertEqual(args[1], "check")
            self.assertEqual(Path(args[2]) / "bin/nvk-runtime", Path(tool))
        else:
            self.fail("unexpected command: " + repr(args))
        result = subprocess.CompletedProcess(args, status, stdout, "simulated failure" if status else "")
        if check and status:
            raise ValueError(tool + " failed")
        return result

    def stage(self):
        with mock.patch.object(STAGE.sys, "platform", "darwin"), \
                mock.patch.object(STAGE, "run", side_effect=self.fake_run), redirect_stdout(io.StringIO()):
            return STAGE.stage(self.args)

    def assert_unchanged(self):
        self.assertEqual(self.snapshot(self.base), self.original)
        self.assertEqual(self.snapshot(self.build), self.original_build)
        self.assertFalse(list(self.root.glob(".nullmoth-stage-*")))

    def assert_manifest(self, root):
        # Independently check coverage as well as every digest, including the
        # nested accelerator manifests used by the package layout.
        entries = {}
        for row in (root / "SHA256SUMS").read_text().splitlines():
            checksum, relative = row.split("  ", 1)
            entries[relative] = checksum
            self.assertEqual(hashlib.sha256((root / relative).read_bytes()).hexdigest(), checksum)
        expected = {"./" + p.relative_to(root).as_posix() for p in root.rglob("*")
                    if p.is_file() and not p.is_symlink() and p != root / "SHA256SUMS"}
        self.assertEqual(set(entries), expected)

    def test_pr_outputs_and_manifests_preserve_upstream_tools_and_unsigned_kexts(self):
        self.stage()
        self.assert_unchanged()
        for name in ("install.sh", "uninstall.sh", "nullmoth-runtime-check.sh", "nullmoth-runtime-check", "VERSION"):
            self.assertEqual((self.output / name).read_bytes(), (self.base / name).read_bytes())
        self.assertEqual((self.output / STAGE.PLUGIN / "Contents/MacOS/NVMTLDriver").read_bytes(), b"new plugin")
        self.assertEqual((self.output / STAGE.BUNDLED_TRANSLATOR).read_bytes(),
                         (self.output / STAGE.TRANSLATOR).read_bytes())
        self.assertEqual((self.output / STAGE.TRANSLATOR).read_bytes(), self.translator.read_bytes())
        self.assertEqual((self.output / STAGE.FRAMEBUFFER / "Contents/Info.plist").read_bytes(), self.plist.read_bytes())
        self.assertEqual((self.output / STAGE.PLUGIN / "Contents/Resources/preserved.txt").read_bytes(), b"plugin resources")
        for relative in STAGE.ACCEL.values():
            self.assertEqual((self.output / relative / "Contents/MacOS/NVAccel").read_bytes(), b"upstream binary")
            self.assert_manifest(self.output / relative.parent)
        self.assert_manifest(self.output)

    def test_optional_tahoe_accelerator_replaces_whole_bundle_and_preserves_sequoia(self):
        self.args.accel_26 = self.make_bundle(self.build / "26/NVAccel.kext", "NVAccel", b"Tahoe accelerator")
        self.put(self.base / STAGE.ACCEL["26"] / "Contents/obsolete.txt", b"must not survive overlay")
        self.seal_base()
        self.stage()
        self.assert_unchanged()
        self.assertFalse((self.output / STAGE.ACCEL["26"] / "Contents/obsolete.txt").exists())
        for major, expected in (("26", b"Tahoe accelerator"), ("15", b"upstream binary")):
            self.assertEqual((self.output / STAGE.ACCEL[major] / "Contents/MacOS/NVAccel").read_bytes(), expected)
        self.assertEqual(self.snapshot(self.output / STAGE.ACCEL["15"].parent),
                         self.snapshot(self.base / STAGE.ACCEL["15"].parent))
        self.assertEqual((self.output / STAGE.FRAMEBUFFER / "Contents/MacOS/NVRMFB").read_bytes(), b"upstream binary")
        self.assertEqual((self.output / STAGE.RUNTIME / "libvulkan_nouveau.dylib").read_bytes(), b"old NVK")
        self.assert_manifest(self.output / STAGE.ACCEL["26"].parent)
        self.assert_manifest(self.output)

    def test_complete_framebuffer_and_accelerator_keep_matching_metadata(self):
        self.args.framebuffer = self.make_bundle(self.build / "NVRMFB.kext", "NVRMFB", b"rotation framebuffer")
        self.args.framebuffer_plist = None
        self.args.accel_26 = self.make_bundle(self.build / "26/NVAccel.kext", "NVAccel", b"rotation accelerator")
        active = Path("Library/Extensions/NVAccel.kext")
        self.make_bundle(self.base / active, "NVAccel", b"old active accelerator")
        self.put(self.base / STAGE.FRAMEBUFFER / "Contents/obsolete-signature", b"old seal")
        self.seal_base()
        self.stage()
        self.assert_unchanged()
        self.assertEqual(self.snapshot(self.output / STAGE.FRAMEBUFFER), self.snapshot(self.args.framebuffer))
        self.assertEqual((self.output / STAGE.ACCEL["26"] / "Contents/MacOS/NVAccel").read_bytes(), b"rotation accelerator")
        self.assertEqual(self.snapshot(self.output / active), self.snapshot(self.output / STAGE.ACCEL["26"]))
        self.assertEqual(self.snapshot(self.output / STAGE.ACCEL["15"]), self.snapshot(self.base / STAGE.ACCEL["15"]))
        self.assert_manifest(self.output / STAGE.ACCEL["26"].parent)
        self.assert_manifest(self.output)

    def test_both_os_accelerators_are_replaced_and_have_independent_manifests(self):
        self.args.accel_15 = self.make_bundle(self.build / "15/NVAccel.kext", "NVAccel", b"Sequoia accelerator")
        self.args.accel_26 = self.make_bundle(self.build / "26/NVAccel.kext", "NVAccel", b"Tahoe accelerator")
        self.make_bundle(self.base / "Library/Extensions/NVAccel.kext", "NVAccel", b"old active accelerator")
        self.seal_base()
        self.stage()
        for major, expected in (("15", b"Sequoia accelerator"), ("26", b"Tahoe accelerator")):
            self.assertEqual((self.output / STAGE.ACCEL[major] / "Contents/MacOS/NVAccel").read_bytes(), expected)
            self.assert_manifest(self.output / STAGE.ACCEL[major].parent)
        self.assertEqual((self.output / "Library/Extensions/NVAccel.kext/Contents/MacOS/NVAccel").read_bytes(), b"Tahoe accelerator")
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_nvrm_replacement_removes_stale_resources_and_retains_release_dependencies(self):
        self.args.nvrm = self.make_bundle(self.build / "NVRM.kext", "NVRM", b"rebuilt RM core")
        old_plist = self.base / STAGE.NVRM / "Contents/Info.plist"
        info = plistlib.loads(old_plist.read_bytes())
        info.update(CFBundleVersion="3.0", OSBundleLibraries={"com.apple.kpi.iokit": "8.0.0"})
        old_plist.write_bytes(plistlib.dumps(info))
        self.put(self.base / STAGE.NVRM / "Contents/stale-resource", b"old core resource")
        self.seal_base()
        self.stage()
        target = self.output / STAGE.NVRM
        self.assertEqual((target / "Contents/MacOS/NVRM").read_bytes(), b"rebuilt RM core")
        self.assertFalse((target / "Contents/stale-resource").exists())
        self.assertEqual(plistlib.loads((target / "Contents/Info.plist").read_bytes()), info)
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_wrong_nvrm_architecture_is_rejected_before_staging(self):
        self.args.nvrm = self.make_bundle(self.build / "NVRM.kext", "NVRM", b"ARM64 incompatible")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "lipo failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_agdc_replacement_keeps_complete_bundle_and_refreshes_root_manifest(self):
        self.args.agdc = self.make_bundle(self.build / "NVRMAGDC.kext", "NVRMAGDC", b"stable display slots")
        self.put(self.base / STAGE.AGDC / "Contents/obsolete-signature", b"must not survive overlay")
        self.seal_base()
        self.stage()
        self.assertEqual(self.snapshot(self.output / STAGE.AGDC), self.snapshot(self.args.agdc))
        for major in STAGE.ACCEL:
            self.assertEqual(self.snapshot(self.output / STAGE.ACCEL[major]), self.snapshot(self.base / STAGE.ACCEL[major]))
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_wrong_agdc_architecture_is_rejected_before_publishing(self):
        self.args.agdc = self.make_bundle(self.build / "NVRMAGDC.kext", "NVRMAGDC", b"ARM64 incompatible")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "lipo failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def minecraft_runtime(self, entry=b"native entry", renderer=b"private renderer", with_icd=True):
        runtime = self.build / "Minecraft runtime"
        self.put(runtime / "lib/libnvk_minecraft.dylib", entry)
        self.put(runtime / "lib/libnvk_renderer.dylib", renderer)
        fields = {"schema.version": "1", "target.os": "macos", "target.arch": "x86_64",
                  "minecraft.version": "26.3", "runtime.id": "test-runtime", "presentation": "gpu-copy",
                  "library.path": "lib/libnvk_minecraft.dylib", "library.sha256": hashlib.sha256(entry).hexdigest()}
        self.put(runtime / "nvk-bridge-runtime.properties", "".join(k + "=" + v + "\n" for k, v in fields.items()).encode())
        self.put(runtime / "BUILD.json", b"local build paths must not be packaged")
        if with_icd:
            self.put(runtime / "nvk_bridge_icd.json", json.dumps({
                "file_format_version": "1.0.0",
                "ICD": {"library_path": "./lib/libnvk_minecraft.dylib", "api_version": "1.4.363"},
            }).encode())
        self.args.minecraft_runtime = runtime
        self.original_build = self.snapshot(self.build)
        return runtime

    def test_minecraft_runtime_is_opt_in_curated_and_does_not_replace_default_nvk(self):
        self.minecraft_runtime()
        self.stage()
        runtime = self.output / STAGE.MINECRAFT
        self.assertEqual({p.relative_to(runtime).as_posix() for p in runtime.rglob("*") if p.is_file()},
                         {"lib/libnvk_minecraft.dylib", "lib/libnvk_renderer.dylib",
                          "nvk-bridge-runtime.properties", "nvk_bridge_icd.json"})
        self.assertEqual((self.output / STAGE.RUNTIME / "libvulkan_nouveau.dylib").read_bytes(), b"old NVK")
        self.assertIn("/Library/GPUBundles/nvmtl/minecraft", (self.output / "MINECRAFT-NVK.md").read_text())
        self.assertEqual((self.output / "NVK-APPLICATIONS.md").read_bytes(),
                         (ROOT / "nvk/native-macos/APPLICATIONS.md").read_bytes())
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_legacy_minecraft_runtime_remains_stageable_without_loader_icd(self):
        self.minecraft_runtime(with_icd=False)
        self.stage()
        self.assertFalse((self.output / STAGE.MINECRAFT / "nvk_bridge_icd.json").exists())
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_native_bridge_icd_cannot_select_another_library_or_api_version(self):
        runtime = self.minecraft_runtime()
        for library, version in (("./lib/libnvk_renderer.dylib", "1.4.363"),
                                 ("../outside.dylib", "1.4.363"),
                                 ("/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib", "1.4.363"),
                                 ("./lib/libnvk_minecraft.dylib", "9.0.0")):
            with self.subTest(library=library, version=version):
                (runtime / "nvk_bridge_icd.json").write_text(json.dumps({
                    "file_format_version": "1.0.0", "ICD": {"library_path": library, "api_version": version},
                }))
                self.original_build = self.snapshot(self.build)
                with self.assertRaisesRegex(ValueError, "native bridge ICD must select"):
                    self.stage()
                self.assertFalse(self.output.exists())
                self.assert_unchanged()

    def test_malformed_native_bridge_icd_is_rejected_before_copying(self):
        runtime = self.minecraft_runtime()
        (runtime / "nvk_bridge_icd.json").write_text("{broken")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "invalid native bridge ICD JSON"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_native_bridge_icd_symlink_is_not_packaged(self):
        runtime = self.minecraft_runtime()
        (runtime / "nvk_bridge_icd.json").rename(runtime / "icd-alias.json")
        (runtime / "nvk_bridge_icd.json").symlink_to("icd-alias.json")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "missing regular file"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_minecraft_entry_hash_mismatch_is_rejected_before_copying(self):
        runtime = self.minecraft_runtime()
        (runtime / "lib/libnvk_minecraft.dylib").write_bytes(b"changed after manifest")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "Minecraft entry hash mismatch"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_minecraft_runtime_requiring_tahoe_is_not_packaged_for_sequoia(self):
        self.minecraft_runtime(entry=b"MINOS26")
        with self.assertRaisesRegex(ValueError, "nullmoth-runtime-check failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_minecraft_renderer_wrong_architecture_is_rejected(self):
        self.minecraft_runtime(renderer=b"ARM64 renderer")
        with self.assertRaisesRegex(ValueError, "lipo failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_wrong_framebuffer_architecture_is_rejected_before_copying(self):
        self.args.framebuffer = self.make_bundle(self.build / "NVRMFB.kext", "NVRMFB", b"ARM64-only framebuffer")
        self.seal_base()
        with self.assertRaisesRegex(ValueError, "lipo failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def wine_runtime(self, renderer=b"Wine renderer"):
        runtime = self.build / "Wine runtime"
        payloads = {"lib/libnvk_wine.dylib": b"Wine frontend", "lib/libnvk_renderer.dylib": renderer,
                    "bin/nvk-runtime": b"setup helper", "dxvk/x64/d3d11.dll": b"paired D3D11",
                    "dxvk/x64/dxgi.dll": b"paired DXGI", "dxvk/source-changes.patch": b"sharing repair",
                    "DXVK-BUILD.json": b"{}", "LICENSES/frontend-PolyForm-Noncommercial-1.0.0.txt": b"frontend",
                    "LICENSES/DXVK-zlib.txt": b"DXVK", "LICENSES/Mesa-license.rst": b"Mesa",
                    "LICENSES/Wine-LGPL-2.1.txt": b"Wine"}
        for name, data in payloads.items():
            self.put(runtime / name, data)
        frontend = {"schema_version": 1,
                    "source_files": {name: STAGE.digest(ROOT / name) for name in (
                        "nvk/native-macos/bridge.mm", "nvk/native-macos/present_worker.h",
                        "plugin/nvmtl_fd_token.h", "plugin/nvmtl_rebind_import.h",
                        "nvk/native-macos/present_hud.h", "nvk/wine-macos/build.py")},
                    "commands": [["clang++", "-DNVKBRIDGE_WINE=1"]],
                    "files": {name: STAGE.digest(runtime / name) for name in (
                        "lib/libnvk_wine.dylib", "lib/libnvk_renderer.dylib")}}
        for key, name in (("entry", "lib/libnvk_wine.dylib"), ("renderer", "lib/libnvk_renderer.dylib")):
            frontend[key] = {"path": name, "sha256": frontend["files"][name]}
        payloads["FRONTEND-BUILD.json"] = json.dumps(frontend).encode()
        self.put(runtime / "FRONTEND-BUILD.json", payloads["FRONTEND-BUILD.json"])
        fields = {"schema_version": 1, "kind": "provider", "runtime_id": "test-wine-runtime",
                  "platform": {"os": "macos", "arch": "x86_64", "minimum_macos": "15.5"},
                  "entry": {"path": "lib/libnvk_wine.dylib"}, "renderer": {"path": "lib/libnvk_renderer.dylib"},
                  "dxvk": {"version": "3.1.1+nvk-shared-fallback"},
                  "frontend_build": "FRONTEND-BUILD.json",
                  "launch_defaults": {"DXVK_CONFIG": "dxvk.enableDescriptorBuffer = False"},
                  "files": {name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()}}
        self.put(runtime / "nvk-wine-runtime.json", json.dumps(fields).encode())
        self.put(runtime / "README.md", b"provider instructions")
        self.put(runtime / "BUILD.json", b"{}")
        self.symlink(runtime / "lib/libMoltenVK.dylib", "libnvk_wine.dylib")
        STAGE.write_manifest(runtime)
        self.args.wine_runtime = runtime
        self.original_build = self.snapshot(self.build)
        return runtime

    def test_wine_and_minecraft_coexist_without_replacing_default_nvk_or_shipping_wine(self):
        minecraft = self.minecraft_runtime()
        wine = self.wine_runtime()
        self.put(wine / "wine/unwanted", b"must not ship a Wine installation")
        self.put(wine / "cache/unwanted", b"must not ship local caches")
        self.original_build = self.snapshot(self.build)
        self.stage()
        destination = self.output / STAGE.WINE
        self.assertEqual((destination / "lib/libnvk_wine.dylib").read_bytes(), b"Wine frontend")
        self.assertTrue((destination / "lib/libMoltenVK.dylib").is_symlink())
        self.assertFalse((destination / "wine").exists())
        self.assertFalse((destination / "cache").exists())
        self.assertEqual((self.output / STAGE.MINECRAFT / "lib/libnvk_renderer.dylib").read_bytes(),
                         (minecraft / "lib/libnvk_renderer.dylib").read_bytes())
        self.assertEqual((self.output / STAGE.RUNTIME / "libvulkan_nouveau.dylib").read_bytes(), b"old NVK")
        self.assertIn("/Library/GPUBundles/nvmtl/wine", (self.output / "WINE-NVK.md").read_text())
        self.assert_manifest(destination)
        self.assert_manifest(self.output)
        self.assert_unchanged()

    def test_old_wine_provider_without_frontend_provenance_is_rejected(self):
        runtime = self.wine_runtime()
        manifest = runtime / "nvk-wine-runtime.json"
        fields = json.loads(manifest.read_text())
        del fields["frontend_build"]
        del fields["files"]["FRONTEND-BUILD.json"]
        (runtime / "FRONTEND-BUILD.json").unlink()
        manifest.write_text(json.dumps(fields))
        STAGE.write_manifest(runtime)
        with self.assertRaisesRegex(ValueError, "current frontend provenance"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_inherited_wine_frontend_is_checked_without_an_explicit_replacement(self):
        runtime = self.wine_runtime()
        destination = self.base / STAGE.WINE
        shutil.copytree(runtime, destination, symlinks=True)
        self.args.wine_runtime = None
        record = destination / "FRONTEND-BUILD.json"
        data = json.loads(record.read_text())
        data["source_files"]["nvk/native-macos/bridge.mm"] = "0" * 64
        record.write_text(json.dumps(data))
        self.seal_base()
        with self.assertRaisesRegex(ValueError, "Wine frontend source mismatch"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_frontend_reuse_requires_current_source_wine_build_and_matching_binaries(self):
        runtime = self.wine_runtime()
        record = runtime / "FRONTEND-BUILD.json"
        original = json.loads(record.read_text())
        changes = (
            (lambda data: data["source_files"].update({"nvk/native-macos/bridge.mm": "0" * 64}),
             "Wine frontend source mismatch"),
            (lambda data: data.update(commands=[["clang++"]]), "not built for Wine"),
            (lambda data: data["entry"].update(sha256="0" * 64), "binary/build mismatch"),
            (lambda data: data["files"].update({"lib/libnvk_renderer.dylib": "0" * 64}),
             "binary/build mismatch"),
        )
        for change, message in changes:
            with self.subTest(message=message):
                data = json.loads(json.dumps(original))
                change(data)
                record.write_text(json.dumps(data))
                manifest = runtime / "nvk-wine-runtime.json"
                fields = json.loads(manifest.read_text())
                fields["files"]["FRONTEND-BUILD.json"] = STAGE.digest(record)
                manifest.write_text(json.dumps(fields))
                STAGE.write_manifest(runtime)
                with self.assertRaisesRegex(ValueError, message):
                    self.stage()
                self.assertFalse(self.output.exists())

    def test_mismatched_wine_dxvk_pair_is_rejected_before_publishing(self):
        runtime = self.wine_runtime()
        self.put(runtime / "dxvk/x64/dxgi.dll", b"different DXVK")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "Wine/NVK hash mismatch"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_full_wine_bundle_is_rejected_by_driver_stager(self):
        runtime = self.wine_runtime()
        manifest = runtime / "nvk-wine-runtime.json"
        fields = json.loads(manifest.read_text())
        fields.pop("kind")
        fields["wine"] = {"executable": "wine/bin/wine"}
        manifest.write_text(json.dumps(fields))
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "compact provider"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_wine_runtime_must_support_sequoia_and_have_matching_alias(self):
        runtime = self.wine_runtime(renderer=b"MINOS26")
        with self.assertRaisesRegex(ValueError, "nullmoth-runtime-check failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        alias = runtime / "lib/libMoltenVK.dylib"
        alias.unlink()
        self.symlink(alias, "libnvk_renderer.dylib")
        with self.assertRaisesRegex(ValueError, "incorrect Wine/NVK provider alias"):
            self.stage()
        self.assertFalse(self.output.exists())

    def test_package_without_bundled_translator_uses_the_canonical_library(self):
        (self.base / STAGE.BUNDLED_TRANSLATOR).unlink()
        self.seal_base()
        self.stage()
        self.assertFalse((self.output / STAGE.BUNDLED_TRANSLATOR).exists())
        self.assertEqual((self.output / STAGE.TRANSLATOR).read_bytes(), self.translator.read_bytes())
        self.assert_unchanged()

    def symlink(self, link, target):
        try:
            link.symlink_to(Path(target))
        except OSError as error:
            self.skipTest("symlinks unavailable: " + str(error))

    def test_parent_traversing_translator_symlink_is_rejected_without_publishing(self):
        link = self.base / STAGE.BUNDLED_TRANSLATOR
        link.unlink()
        self.symlink(link, "../../../nvmtl/libnvmtl_translate.dylib")
        self.seal_base()
        with self.assertRaisesRegex(ValueError, "symlink contains parent traversal"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_missing_prebuilt_accelerator_is_rejected(self):
        (self.base / STAGE.ACCEL["15"] / "Contents/MacOS/NVAccel").unlink()
        self.seal_base()
        with self.assertRaisesRegex(ValueError, "missing regular file"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_bad_input_checksum_is_rejected_without_publishing(self):
        self.put(self.base / "VERSION", b"tampered")
        self.original = self.snapshot(self.base)
        with self.assertRaisesRegex(ValueError, "SHA256SUMS mismatch"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_failed_copy_does_not_publish_or_modify_input(self):
        replace = STAGE.replace_file
        def fail_translator(source, destination):
            if destination.name == "libnvmtl_translate.dylib":
                raise OSError("simulated copy failure")
            replace(source, destination)
        with mock.patch.object(STAGE, "replace_file", side_effect=fail_translator), \
                self.assertRaisesRegex(OSError, "simulated copy failure"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_failed_manifest_verification_does_not_publish(self):
        verify = STAGE.verify_manifest
        def fail_candidate(root):
            if root != self.base:
                raise ValueError("candidate checksum failure")
            verify(root)
        with mock.patch.object(STAGE, "verify_manifest", side_effect=fail_candidate), \
                self.assertRaisesRegex(ValueError, "candidate checksum failure"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_wrong_architecture_is_rejected_before_copying(self):
        self.plugin.write_bytes(b"ARM64-only plugin")
        self.original_build = self.snapshot(self.build)
        with self.assertRaisesRegex(ValueError, "lipo failed"):
            self.stage()
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_existing_output_is_not_overwritten(self):
        self.put(self.output / "keep.txt", b"previous package")
        with self.assertRaisesRegex(ValueError, "output already exists"):
            self.stage()
        self.assertEqual((self.output / "keep.txt").read_bytes(), b"previous package")
        self.assert_unchanged()

    def test_output_inside_input_is_rejected(self):
        self.args.output = self.base / "nested"
        with self.assertRaisesRegex(ValueError, "outside the upstream package"):
            self.stage()
        self.assert_unchanged()

    def test_external_symlink_is_rejected_without_touching_its_target(self):
        external = self.put(self.root / "external.dylib", b"must not change")
        self.symlink(self.base / "external.dylib", "../external.dylib")
        self.seal_base()
        with self.assertRaisesRegex(ValueError, "symlink (escapes|contains parent traversal)"):
            self.stage()
        self.assertEqual(external.read_bytes(), b"must not change")
        self.assertFalse(self.output.exists())
        self.assert_unchanged()

    def test_non_macos_refuses(self):
        with mock.patch.object(STAGE.sys, "platform", "win32"), self.assertRaisesRegex(ValueError, "macOS"):
            STAGE.stage(self.args)
        self.assertFalse(self.output.exists())

    def test_manifest_escaping_round_trip(self):
        for name in ("./plain name", "./with\\backslash", "./with\nnewline", "./with\rcarriage"):
            prefix, escaped = STAGE.manifest_name(name)
            self.assertEqual(STAGE.decode_name(escaped) if prefix else escaped, name)


    def test_replacements_keep_release_versions_dependencies_and_fork_personalities(self):
        pairs = [(STAGE.ACCEL["15"], "NVAccel"), (STAGE.ACCEL["26"], "NVAccel"), (STAGE.AGDC, "NVRMAGDC")]
        for relative, name in pairs:
            source = self.make_bundle(self.build / relative, name, b"fork compiled binary")
            upstream = {"CFBundleExecutable": name, "CFBundleIdentifier": "com.nullmoth." + name,
                        "CFBundleVersion": "2.8", "CFBundleShortVersionString": "2.8",
                        "OSBundleLibraries": {"com.apple.kpi.iokit": "8.0"}}
            built = {**upstream, "CFBundleVersion": "0.1", "CFBundleShortVersionString": "0.1",
                     "IOKitPersonalities": {"fork": {"IOGLBundleName": "NVKZinkGLDriver"}},
                     "OSBundleLibraries": {"com.apple.kpi.iokit": "7.0", "com.fork.required": "1.0"}}
            self.put(self.base / relative / "Contents/Info.plist", plistlib.dumps(upstream))
            self.put(source / "Contents/Info.plist", plistlib.dumps(built))
            before = (source / "Contents/MacOS" / name).read_bytes()
            with mock.patch.object(STAGE, "run", side_effect=self.fake_run):
                STAGE.preserve_kext_release_metadata(source, self.base / relative)
            info = plistlib.loads((source / "Contents/Info.plist").read_bytes())
            self.assertEqual(info["CFBundleVersion"], "2.8")
            self.assertEqual(info["CFBundleShortVersionString"], "2.8")
            self.assertEqual(info["OSBundleLibraries"], {"com.apple.kpi.iokit": "8.0", "com.fork.required": "1.0"})
            self.assertEqual(info["IOKitPersonalities"], built["IOKitPersonalities"])
            self.assertEqual((source / "Contents/MacOS" / name).read_bytes(), before)

    def test_kext_metadata_refuses_a_mismatched_bundle_identifier(self):
        source = self.make_bundle(self.build / "mismatch.kext", "NVAccel", b"compiled binary")
        self.put(source / "Contents/Info.plist", plistlib.dumps({"CFBundleIdentifier": "com.other.kext"}))
        with self.assertRaisesRegex(ValueError, "bundle ID differs"):
            STAGE.preserve_kext_release_metadata(source, self.base / STAGE.ACCEL["15"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
