#!/usr/bin/env python3
"""Exercise atomic staging, integrity coverage and firmware pairing on real files."""
from pathlib import Path
import hashlib
import json
import plistlib
import os
import subprocess
import sys
import tempfile
import unittest
import tarfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'build'))
import package_zink as pkg
from record_zink_runtime import record
from package_layout import ZINK_REPORTS


class PackageZinkTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='zink package test ')
        self.root = Path(self.temp.name).resolve()
        self.base = self.root / 'base'
        self.zink = self.root / 'zink'
        self.out = self.root / 'output'
        self.commands = []
        self.firmware = '/Library/NullMoth/nvfw/nvidia/615.78.08/gsp_tu10x.bin\n'
        for name in ('install.sh', 'uninstall.sh', 'nullmoth-runtime-check.sh', 'nullmoth-runtime-check', 'VERSION'):
            self.put(self.base / name, b'base')
        for name in ('NVRM', 'NVRMFB', 'NVRMAGDC', 'NVAccel'):
            self.bundle(self.base / 'Library/Extensions' / (name + '.kext'), name)
        for major in ('15', '26'):
            self.bundle(self.base / 'Library/NullMoth/kexts' / major / 'NVAccel.kext', 'NVAccel')
        for name in ('NVMTLDriver', 'NVIDIAShared'):
            self.bundle(self.base / 'Library/GPUBundles' / (name + '.bundle'), name)
        for name in ('libvulkan_nouveau.dylib', 'libvulkan.1.4.362.dylib'):
            self.put(self.base / pkg.BASE_RUNTIME / name, b'base runtime')
        self.put(self.base / pkg.BASE_RUNTIME / 'nvk_icd.json', json.dumps({'ICD': {'library_path': '/Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib'}}).encode())
        wine = self.base / pkg.BASE_RUNTIME / 'wine'
        frontend = {'schema_version': 1, 'source_files': {name: pkg.base_tools.digest(ROOT/name) for name in (
            'nvk/native-macos/bridge.mm', 'nvk/native-macos/present_worker.h',
            'plugin/nvmtl_fd_token.h', 'plugin/nvmtl_rebind_import.h',
            'nvk/native-macos/present_hud.h', 'nvk/wine-macos/build.py')},
            'commands': [['clang++', '-DNVKBRIDGE_WINE=1']], 'files': {}}
        for key, name in (('entry', 'lib/libnvk_wine.dylib'), ('renderer', 'lib/libnvk_renderer.dylib')):
            self.put(wine/name, name.encode())
            frontend['files'][name] = pkg.base_tools.digest(wine/name)
            frontend[key] = {'path': name, 'sha256': frontend['files'][name]}
        self.put(wine/'FRONTEND-BUILD.json', json.dumps(frontend).encode())
        self.put(self.base / 'Users/Shared/nvfw/nvidia/615.78.08/gsp_tu10x.bin', b'firmware')
        self.put(self.base / 'licenses/base-notice.txt', b'keep this notice')
        for name in pkg.ZINK_FILES:
            self.put(self.zink / name, b'25G241\n' if name == 'cgl-os-build.txt' else b'zink ' + name.encode())
        native = {'renderer_original_sha256':pkg.base_tools.digest(self.base/pkg.BASE_RUNTIME/'libvulkan_nouveau.dylib'),
                  'frontend_source_sha256':pkg.base_tools.digest(ROOT/'nvk/native-macos/bridge.mm'),
                  'frontend_headers':{name:pkg.base_tools.digest(ROOT/name) for name in ['plugin/nvmtl_fd_token.h','plugin/nvmtl_rebind_import.h','nvk/native-macos/present_worker.h']},
                  'files':{name:pkg.base_tools.digest(self.zink/name) for name in
                           ['libnvk_renderer.dylib','libnvk_zink_vulkan.dylib']}}
        (self.zink/'NATIVE-BUILD.json').write_text(json.dumps(native))
        (self.zink/'ZINK-BUILD.json').unlink()
        mesa = self.root/'MESA-SOURCE.json'
        mesa.write_text(json.dumps({'commit':pkg.MESA_COMMIT,
            'overlay_sha256':pkg.base_tools.digest(ROOT/'zink/mesa-zink-nvk.patch')}))
        record(self.zink,'zink',mesa); record(self.zink,'cgl')
        pkg.base_tools.write_manifest(self.base)
        self.webkit = self.root/'webkit-build'
        self.bundle(self.webkit/'NVWebKitFixup.kext', 'NVWebKitFixup')
        (self.webkit/'NVWebKitFixup.kext/Contents/Info.plist').write_bytes((ROOT/'kexts/NVWebKitFixup/Info.plist').read_bytes())
        from package_webkit import SOURCES, files
        self.put(self.webkit/'BUILD.json', json.dumps({
            'version':'0.2.0', 'files':files(self.webkit/'NVWebKitFixup.kext'),
            'source_sha256':{name:pkg.base_tools.digest(ROOT/name) for name in SOURCES}}).encode())
        self.original = self.snapshot(self.base)
        self.zink_original = self.snapshot(self.zink)

    def tearDown(self):
        self.temp.cleanup()

    def put(self, path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def bundle(self, path, name):
        self.put(path / 'Contents/Info.plist', plistlib.dumps({'CFBundleExecutable': name}))
        marker = b'\0nmintelfb-frame-abi:2\0' if name in ('NVAccel','NMIntelFB') else b''
        self.put(path / 'Contents/MacOS' / name, b'base executable ' + name.encode() + marker)

    @staticmethod
    def snapshot(root):
        return {str(p.relative_to(root)): p.read_bytes() for p in root.rglob('*') if p.is_file()}

    def fake_run(self, *args):
        self.commands.append(tuple(str(a) for a in args))
        if args[0] == '/bin/bash' and str(args[1]).endswith('build_intel_panel.sh'):
            self.bundle(Path(args[2])/'NMIntelFB.kext', 'NMIntelFB')
            (Path(args[2])/'NVKMS-HEADERS.json').write_text(json.dumps({
                'repository': 'https://github.com/NVIDIA/open-gpu-kernel-modules',
                'tag': '615.78.08', 'commit': '1a82fbfda8f321b056eb3568850f9316c5e465c5',
                'headers_sha256': {'fixture/nvkms-kapi.h': '0'*64},
            }))
        if args[0] == 'strings': return self.firmware
        if args[0] == 'git': return 'test-commit\n'
        if args[0] == 'otool': return 'cmd LC_RPATH\n cmdsize 40\n path /Library/GPUBundles/nvmtl (offset 12)\n'
        if args[0] == 'sw_vers': return '26.7.1\n'
        return ''

    def stage(self, native_gld=False):
        with patch.object(pkg, 'run', side_effect=self.fake_run):
            return pkg.stage(self.base, self.zink, self.out, native_gld=native_gld, webkit_fixup_build=self.webkit)

    def unchanged(self):
        self.assertEqual(self.original, self.snapshot(self.base))
        self.assertEqual(self.zink_original, self.snapshot(self.zink))
        self.assertFalse(list(self.root.glob('.zink-package-*')))

    def add_audio(self, optional=False, identifier='com.nullmoth.NMXAudio'):
        target=self.base/('Optional' if optional else 'Library/Extensions')/'NMXAudio.kext'
        self.bundle(target,'NMXAudio')
        info=plistlib.loads((target/'Contents/Info.plist').read_bytes())
        info.update(CFBundleIdentifier=identifier,CFBundleVersion='1.0.0')
        (target/'Contents/Info.plist').write_bytes(plistlib.dumps(info))
        pkg.base_tools.write_manifest(self.base)
        return target

    def test_unbundled_application_guide_keeps_its_source_link(self):
        self.stage()
        self.assertFalse((self.out/'Guides/Vulkan-Applications.md').exists())
        readme=(self.out/'READ-ME.md').read_text()
        self.assertIn('blob/main/nvk/native-macos/APPLICATIONS.md',readme)
        self.assertNotIn('](Guides/Vulkan-Applications.md)',readme)
        self.unchanged()

    def test_application_guide_is_grouped_under_guides(self):
        guide=(ROOT/'nvk/native-macos/APPLICATIONS.md').read_bytes()
        self.put(self.base/'NVK-APPLICATIONS.md',guide)
        pkg.base_tools.write_manifest(self.base)
        self.original=self.snapshot(self.base)
        self.stage()
        self.assertFalse((self.out/'NVK-APPLICATIONS.md').exists())
        self.assertTrue((self.out/'Guides/Vulkan-Applications.md').is_file())
        self.assertIn('](Guides/Vulkan-Applications.md)',(self.out/'READ-ME.md').read_text())
        self.assertIn('nvk_bridge_icd.json',(self.out/'Guides/Vulkan-Applications.md').read_text())
        self.unchanged()

    def test_audio_is_relocated_without_changing_binary_or_signature(self):
        audio=self.add_audio();before=self.snapshot(audio)
        self.stage()
        self.assertFalse((self.out/'Library/Extensions/NMXAudio.kext').exists())
        self.assertEqual(before,self.snapshot(self.out/'Optional/NMXAudio.kext'))
        self.assertIn('--audio=yes',(self.out/'Optional/Audio.md').read_text())
        identity=json.loads((self.out/'PACKAGE-IDENTITY.json').read_text())
        self.assertTrue(identity['optional_audio']['bundled'])
        self.assertEqual(identity['optional_audio']['default'],'No')
        self.assertEqual(identity['components_sha256']['Optional/NMXAudio.kext/Contents/MacOS/NMXAudio'],
                         pkg.base_tools.digest(audio/'Contents/MacOS/NMXAudio'))
        pkg.base_tools.verify_manifest(self.out)

    def test_existing_optional_audio_is_kept(self):
        audio=self.add_audio(optional=True);before=self.snapshot(audio)
        self.stage();self.assertEqual(before,self.snapshot(self.out/'Optional/NMXAudio.kext'))

    def test_duplicate_audio_payloads_are_refused_atomically(self):
        self.add_audio();self.add_audio(optional=True)
        with self.assertRaisesRegex(ValueError,'duplicate audio'):self.stage()
        self.assertFalse(self.out.exists())

    def test_wrong_audio_identity_is_refused_atomically(self):
        self.add_audio(identifier='org.external.Audio')
        with self.assertRaisesRegex(ValueError,'audio bundle identity'):self.stage()
        self.assertFalse(self.out.exists())

    def test_complete_stage_preserves_payload_and_covers_every_regular_file(self):
        self.stage()
        self.unchanged()
        for name in pkg.ZINK_FILES:
            self.assertEqual((self.out / pkg.RUNTIME / name).read_bytes(), (self.zink / name).read_bytes())
        self.assertEqual((self.out / 'licenses/base-notice.txt').read_bytes(), b'keep this notice')
        self.assertEqual(json.loads((self.out / pkg.RUNTIME / 'nvk_icd.json').read_text())['ICD']['library_path'], './libnvk_renderer.dylib')
        self.assertEqual((self.out / pkg.BASE_RUNTIME / 'nvk_icd.json').read_bytes(), (self.base / pkg.BASE_RUNTIME / 'nvk_icd.json').read_bytes())
        self.assertTrue((self.out/'Optional/NMIntelFB.kext/Contents/MacOS/NMIntelFB').is_file())
        self.assertFalse((self.out/'Library/Extensions/NMIntelFB.kext').exists())
        self.assertFalse(list(self.out.rglob('NMIntelFBVirtual.kext')))
        self.assertEqual((self.out/'Optional/Intel-panel.command').read_bytes(),
                         (ROOT/'tools/intel_panel_control.sh').read_bytes())
        self.assertTrue((self.out/'Optional/Intel-panel.command').stat().st_mode & 0o111)
        identity = json.loads((self.out.with_name(self.out.name+'-build-records')/'Optional/INTEL-PANEL-BUILD.json').read_text())
        for name, sha in identity['files'].items():
            self.assertEqual(pkg.base_tools.digest(self.out/'Optional/NMIntelFB.kext'/name), sha)
        self.assertEqual(identity['frame_bridge_abi'], 2)
        self.assertFalse(identity['experimental_ram_scanout_enabled_by_default'])
        for major, sha in identity['matched_accelerators_sha256'].items():
            self.assertEqual(sha, pkg.base_tools.digest(self.out/'Library/NullMoth/kexts'/major/'NVAccel.kext/Contents/MacOS/NVAccel'))
        for command in ('Install.command', 'Verify.command', 'Uninstall.command'):
            self.assertTrue((self.out / command).stat().st_mode & 0o111)
        entries = {}
        for row in (self.out / 'SHA256SUMS').read_text().splitlines():
            digest, name = row.split('  ', 1)
            entries[name] = digest
            self.assertEqual(hashlib.sha256((self.out / name).read_bytes()).hexdigest(), digest)
        expected = {'./' + p.relative_to(self.out).as_posix() for p in self.out.rglob('*') if p.is_file() and p.name != 'SHA256SUMS'}
        expected.update('./Library/NullMoth/kexts/' + m + '/SHA256SUMS' for m in ('15', '26'))
        self.assertEqual(set(entries), expected)
        self.assertFalse(list(self.out.rglob('NVKZinkGLDriver.bundle')))

    def test_tampered_base_is_rejected_without_output(self):
        self.put(self.base / 'VERSION', b'tampered')
        with self.assertRaisesRegex(ValueError, 'SHA256SUMS mismatch'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_full_package_rejects_a_stale_hybrid_wine_frontend(self):
        record = self.base/pkg.BASE_RUNTIME/'wine/FRONTEND-BUILD.json'
        data = json.loads(record.read_text())
        data['source_files']['nvk/native-macos/bridge.mm'] = '0'*64
        record.write_text(json.dumps(data))
        pkg.base_tools.write_manifest(self.base)
        with self.assertRaisesRegex(ValueError, 'Wine frontend source mismatch'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_old_accelerator_cannot_be_paired_with_ram_scanout(self):
        self.put(self.base/'Library/NullMoth/kexts/15/NVAccel.kext/Contents/MacOS/NVAccel', b'old accelerator')
        pkg.base_tools.write_manifest(self.base)
        with self.assertRaisesRegex(ValueError,'matched frame-ABI-2 NVAccel'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_old_reports_leave_archive_but_runtime_manifests_and_signed_json_stay(self):
        reports = ['BASELINE-IDENTITY.json', 'BASE-RELEASE-IDENTITY.json',
                   'RELEASE-IDENTITY.json', 'MERGE-VALIDATION.json',
                   'CAPTURE-REVIEW.md', 'LOCAL-TEST.md', 'MANIFEST.txt']
        for name in reports:
            self.put(self.base/name,b'historical report')
        self.put(self.base/'provenance/old-build.json',b'older build')
        for name in ZINK_REPORTS:
            self.put(self.base/pkg.RUNTIME/name,b'old Zink report')
        signed = Path('Library/GPUBundles/NVMTLDriver.bundle/Contents/Resources/sass/seed.json')
        manifests = [pkg.BASE_RUNTIME/'wine/nvk-wine-runtime.json',
                     pkg.BASE_RUNTIME/'wine/DXVK-BUILD.json',
                     pkg.BASE_RUNTIME/'video/NVENC-BUILD.json',signed]
        for name in manifests:
            self.put(self.base/name,b'component contract')
        self.put(self.base/'WINE-NVK.md',b'# Wine setup\n')
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage(); self.unchanged()
        records = self.out.with_name(self.out.name+'-build-records')
        for name in reports:
            self.assertFalse((self.out/name).exists())
            self.assertEqual((records/name).read_bytes(),b'historical report')
        self.assertEqual((records/'provenance/old-build.json').read_bytes(),b'older build')
        for name in ZINK_REPORTS:
            self.assertFalse((self.out/pkg.RUNTIME/name).exists())
            self.assertTrue((records/pkg.RUNTIME/name).is_file())
        for name in manifests:
            self.assertEqual((self.out/name).read_bytes(),b'component contract')
        self.assertEqual((self.out/'Guides/Wine-CrossOver.md').read_bytes(),b'# Wine setup\n')
        self.assertEqual({p.name for p in self.out.glob('*.json')},{'PACKAGE-IDENTITY.json'})
        self.assertEqual({p.name for p in self.out.glob('*.md')},{'READ-ME.md'})
        compact = json.loads((self.out/'PACKAGE-IDENTITY.json').read_text())
        self.assertEqual(compact['detailed_build_record_sha256'],pkg.base_tools.digest(records/'PACKAGE-IDENTITY.json'))
        pkg.base_tools.verify_manifest(self.out)

    def test_refresh_guide_in_an_already_organized_package(self):
        guide = Path('Guides/Zink-OpenGL.md')
        self.put(self.base/guide, b'previous release guide\n')
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage(); self.unchanged()
        self.assertIn('system pages', (self.out/guide).read_text())
        records = self.out.with_name(self.out.name+'-build-records')
        self.assertEqual((records/'inherited'/guide).read_bytes(), b'previous release guide\n')
        pkg.base_tools.verify_manifest(self.out)

    def test_existing_external_records_are_not_overwritten(self):
        records=self.out.with_name(self.out.name+'-build-records')
        self.put(records/'keep',b'existing records')
        with self.assertRaisesRegex(ValueError,'Build records must be new'):
            self.stage()
        self.assertFalse(self.out.exists())
        self.assertEqual((records/'keep').read_bytes(),b'existing records')
        self.unchanged()

    def test_signed_shader_resources_survive_transient_cache_filter(self):
        signed = Path('Library/GPUBundles/NVMTLDriver.bundle/Contents/Resources/sass/seed.bin')
        framework = Path('Library/GPUBundles/Signed.framework/Versions/A/Resources/aircache/seed.bin')
        transient = pkg.BASE_RUNTIME / 'spvcache/temporary.bin'
        for relative in (signed, framework, transient):
            self.put(self.base / relative, b'signed seed' if relative != transient else b'transient')
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage()
        self.unchanged()
        self.assertEqual((self.out / signed).read_bytes(), b'signed seed')
        self.assertEqual((self.out / framework).read_bytes(), b'signed seed')
        self.assertFalse((self.out / transient).exists())
        self.assertTrue(any(command[:3] == ('codesign', '--verify', '--strict')
                            and command[-1].endswith('/NVMTLDriver.bundle') for command in self.commands))

    def test_missing_firmware_version_cannot_publish_a_package(self):
        self.firmware = self.firmware.replace('615.78.08', '999.0')
        with self.assertRaisesRegex(ValueError, 'missing regular file'):
            self.stage()
        self.assertFalse(self.out.exists())
        self.unchanged()

    def test_missing_adapter_cannot_publish_partial_installation(self):
        (self.zink / 'libnvk_cgl.dylib').unlink()
        with self.assertRaisesRegex(ValueError, 'missing regular file'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_existing_output_is_not_overwritten(self):
        self.put(self.out / 'keep', b'existing output')
        with self.assertRaisesRegex(ValueError, 'Output must be new'):
            self.stage()
        self.assertEqual((self.out / 'keep').read_bytes(), b'existing output')
        self.unchanged()

    def test_diagnostic_gld_selection_is_rejected(self):
        p = self.base / 'Library/NullMoth/kexts/26/NVAccel.kext/Contents/Info.plist'
        data = plistlib.loads(p.read_bytes())
        data['IOKitPersonalities'] = {'NVAccel': {'IOGLBundleName': 'NVKZinkGLDriver'}}
        p.write_bytes(plistlib.dumps(data))
        pkg.base_tools.write_manifest(self.base)
        with self.assertRaisesRegex(ValueError, 'diagnostic GLD'):
            self.stage()
        self.assertFalse(self.out.exists())

    def native_bundle(self, build='25G241', diagnostic=False):
        bundle = self.zink / 'NVKZinkGLDriver.bundle'
        self.bundle(bundle, 'NVKZinkGLDriver')
        info = bundle / 'Contents/Info.plist'
        data = plistlib.loads(info.read_bytes())
        data.update(NVKZinkExperimentalGLD=True, NVKZinkBuildOS=build,
                    NVKZinkDiagnosticOnly=diagnostic)
        info.write_bytes(plistlib.dumps(data))
        self.put(self.zink / 'gld_bundle_probe', b'lookup checker')
        record(self.zink,'gld')
        self.zink_original = self.snapshot(self.zink)

    def test_native_stage_selects_both_os_accelerators(self):
        self.native_bundle()
        self.put(self.base/'Test-Zink.command',b'old Sequoia test launcher')
        self.put(self.base/pkg.RUNTIME/'zink-tests/old.py',b'old packaged runner')
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage(native_gld=True)
        self.unchanged()
        for relative in ('Library/Extensions/NVAccel.kext', 'Library/NullMoth/kexts/15/NVAccel.kext',
                         'Library/NullMoth/kexts/26/NVAccel.kext'):
            info = plistlib.loads((self.out / relative / 'Contents/Info.plist').read_bytes())
            self.assertEqual(info['IOKitPersonalities']['NVAccel']['IOGLBundleName'], 'NVKZinkGLDriver')
        self.assertEqual(json.loads((self.out/'PACKAGE-IDENTITY.json').read_text())['native_cgl_macos_majors'], ['15','26'])
        self.assertFalse((self.out/'Test-Zink.command').exists())
        self.assertFalse((self.out/pkg.RUNTIME/'zink-tests').exists())
        self.assertTrue((self.out / 'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/MacOS/NVKZinkGLDriver').is_file())
        pkg.base_tools.verify_manifest(self.out)

    def test_native_stage_rejects_diagnostic_bundle(self):
        self.native_bundle(diagnostic=True)
        with self.assertRaisesRegex(ValueError, 'diagnostic GLD'):
            self.stage(native_gld=True)
        self.assertFalse(self.out.exists())
        self.unchanged()

    def test_native_stage_rejects_mismatched_component_provenance(self):
        self.native_bundle(build='25H999')
        with self.assertRaisesRegex(ValueError, 'build provenance differ'):
            self.stage(native_gld=True)
        self.assertFalse(self.out.exists())
        self.unchanged()

    def test_native_stage_accepts_components_generated_on_a_new_build(self):
        (self.zink/'cgl-os-build.txt').write_text('25G242\n')
        record(self.zink,'cgl')
        self.native_bundle(build='25G242')
        self.stage(native_gld=True)
        self.unchanged()

    def test_installer_command_passes_payload_flag_and_preserves_spaces(self):
        # Execute the actual wrapper with a harmless installer and simulated
        # root identity. This never invokes sudo or modifies system paths.
        folder = self.root / 'installer with spaces'
        folder.mkdir()
        self.put(folder / 'Install.command', (ROOT / 'package/Install.command').read_bytes())
        self.put(folder / 'install.sh', b'printf "%s\\n" "$@"\n')
        identity = self.root / 'id'
        self.put(identity, b'#!/bin/sh\nprintf "0\\n"\n')
        identity.chmod(0o755)
        result = subprocess.run(['/bin/bash', str(folder / 'Install.command')],
                                env=dict(os.environ, PATH=str(self.root) + ':/usr/bin:/bin'),
                                check=True, capture_output=True, text=True)
        self.assertEqual(result.stdout.splitlines(), ['--payload', str(folder)])

    def test_private_renderer_pairing_is_checked(self):
        data = json.loads((self.zink/'NATIVE-BUILD.json').read_text())
        data['renderer_original_sha256'] = '0'*64
        (self.zink/'NATIVE-BUILD.json').write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError,'does not match the verified driver'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_frontend_shared_headers_are_part_of_build_provenance(self):
        original = (self.zink/'NATIVE-BUILD.json').read_text()
        for name in ['plugin/nvmtl_rebind_import.h','nvk/native-macos/present_worker.h']:
            with self.subTest(header=name):
                data = json.loads(original)
                data['frontend_headers'][name] = '0'*64
                (self.zink/'NATIVE-BUILD.json').write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError,'frontend header changed'):
                    self.stage()
                self.assertFalse(self.out.exists())

    def test_modified_component_is_rejected(self):
        (self.zink/'libnvk_cgl.dylib').write_bytes(b'modified after building')
        with self.assertRaisesRegex(ValueError,'component hash mismatch'):
            self.stage()
        self.assertFalse(self.out.exists())

    def test_existing_minecraft_and_wine_payloads_survive(self):
        for name in ['minecraft','wine']:
            self.put(self.base/pkg.BASE_RUNTIME/name/'keep',name.encode())
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage(); self.unchanged()
        for name in ['minecraft','wine']:
            self.assertEqual((self.out/pkg.BASE_RUNTIME/name/'keep').read_bytes(),name.encode())

    def test_framework_directory_links_survive_and_metadata_is_excluded(self):
        framework = self.base/pkg.BASE_RUNTIME/'fixture.framework'
        self.put(framework/'Versions/A/Resources/keep',b'resource')
        (framework/'Versions/Current').symlink_to('A',target_is_directory=True)
        (framework/'Resources').symlink_to('Versions/Current/Resources',target_is_directory=True)
        self.put(self.base/pkg.BASE_RUNTIME/'._metadata',b'appledouble')
        self.put(self.base/pkg.BASE_RUNTIME/'.DS_Store',b'finder')
        pkg.base_tools.write_manifest(self.base)
        self.original = self.snapshot(self.base)
        self.stage(); self.unchanged()
        copied = self.out/pkg.BASE_RUNTIME/'fixture.framework'
        self.assertTrue((copied/'Resources').is_symlink())
        self.assertEqual((copied/'Resources/keep').read_bytes(),b'resource')
        self.assertFalse(list(self.out.rglob('._*')))
        self.assertFalse(list(self.out.rglob('.DS_Store')))

    def test_arbitrary_component_overlay_is_rejected(self):
        with self.assertRaisesRegex(ValueError,'Stage matched driver components'):
            pkg.stage(self.base,self.zink,self.out,components=self.root)
        self.assertFalse(self.out.exists())

    def test_archive_uses_pkgroot_and_normalized_ownership(self):
        self.native_bundle()
        archive = self.root/'local-test.tar.gz'
        argv = ['package_zink.py','--base',str(self.base),'--zink',str(self.zink),
                '--stage',str(self.out),'--archive',str(archive),'--webkit-fixup-build',str(self.webkit)]
        with patch.object(sys,'argv',argv),patch.object(pkg,'run',side_effect=self.fake_run):
            pkg.main()
        with tarfile.open(archive) as stream:
            for info in stream.getmembers():
                self.assertTrue(info.name == 'pkgroot' or info.name.startswith('pkgroot/'))
                self.assertEqual((info.uid,info.gid,info.uname,info.gname),(0,0,'root','root'))

    def test_webkit_bundle_is_optional_and_fully_covered(self):
        self.stage()
        component=self.out/'Optional/NVWebKitFixup'
        self.assertEqual(self.snapshot(component/'NVWebKitFixup.kext'),self.snapshot(self.webkit/'NVWebKitFixup.kext'))
        self.assertFalse((component/'BUILD.json').exists())
        self.assertFalse((self.out/'Library/Extensions/NVWebKitFixup.kext').exists())
        self.assertEqual((component/'efi.py').read_bytes(),(ROOT/'package/webkit_fixup_efi.py').read_bytes())
        self.assertIn('Optional/NVWebKitFixup/README.md',(self.out/'READ-ME.md').read_text())
        identity=json.loads((self.out/'PACKAGE-IDENTITY.json').read_text())
        self.assertTrue(identity['optional_webkit_fixup']['bundled'])
        pkg.base_tools.verify_manifest(self.out)

    def test_modified_helper_is_rejected_without_output(self):
        (self.webkit/'NVWebKitFixup.kext/Contents/MacOS/NVWebKitFixup').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError,'hashes do not match'):self.stage()
        self.assertFalse(self.out.exists())

    def test_stale_helper_sources_are_rejected_without_output(self):
        record=json.loads((self.webkit/'BUILD.json').read_text());record['source_sha256']={}
        (self.webkit/'BUILD.json').write_text(json.dumps(record))
        with self.assertRaisesRegex(ValueError,'sources changed'):self.stage()
        self.assertFalse(self.out.exists())

    def test_helper_build_is_required_even_when_other_components_match(self):
        with patch.object(pkg,'run',side_effect=self.fake_run):
            with self.assertRaisesRegex(ValueError,'Supply --webkit-fixup-build'):
                pkg.stage(self.base,self.zink,self.out)
        self.assertFalse(self.out.exists())

    def test_helper_build_record_binds_sources_and_sdk_inputs(self):
        from package_webkit import record as record_helper, files
        lilu=self.root/'Lilu.kext';sdk=self.root/'KernelSDK'
        for relative in ('Contents/MacOS/Lilu','Contents/Resources/Headers/header.hpp','Contents/Resources/Library/plugin_start.cpp'):
            self.put(lilu/relative,relative.encode())
        for relative in ('Headers/sys/kernel.h','Library/x86_64/libkmod.a'):
            self.put(sdk/relative,relative.encode())
        with patch('package_webkit.subprocess.run'):
            record_helper(self.webkit,lilu,sdk)
        identity=json.loads((self.webkit/'BUILD.json').read_text())
        self.assertEqual(identity['files'],files(self.webkit/'NVWebKitFixup.kext'))
        self.assertIn('kexts/NVWebKitFixup/compatibility.hpp',identity['source_sha256'])
        self.assertIn('Contents/Resources/Library/plugin_start.cpp',identity['lilu_sdk_sha256'])
        self.assertIn('Library/x86_64/libkmod.a',identity['kernel_sdk_libraries_sha256'])


if __name__ == '__main__':
    unittest.main(verbosity=2)
