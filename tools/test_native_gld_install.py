#!/usr/bin/env python3
"""Run the native GLD installer against a private filesystem with mocked kernel tools."""
import hashlib
from pathlib import Path
import plistlib
import struct
import unittest

from primary_install_fixture import Install


class NativeGLDInstall(Install):
    @classmethod
    def setUpClass(cls):
        super().setUpClass()
        # Runtime qualification is mocked, as in the shared installer fixture.
        cls.guard.write_text(cls.guard.read_text().replace(
            '*.dylib)', '*.dylib|*/Contents/MacOS/NVKZinkGLDriver)'))

    def setUp(self):
        super().setUp()
        self.env['FAKE_OS_VERSION'] = '26.7.1'
        for relative in ('Library/Extensions/NVAccel.kext',
                         'Library/NullMoth/kexts/15/NVAccel.kext',
                         'Library/NullMoth/kexts/26/NVAccel.kext'):
            info = self.payload / relative / 'Contents/Info.plist'
            data = plistlib.loads(info.read_bytes()) if info.exists() else {}
            data['IOKitPersonalities'] = {'NVAccel': {'IOGLBundleName': 'NVKZinkGLDriver'}}
            info.write_bytes(plistlib.dumps(data))
        self.gld_info = self.payload / 'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/Info.plist'
        self.gld_info.parent.mkdir(parents=True)
        self.gld_info.write_bytes(plistlib.dumps({'NVKZinkBuildOS': '25G241'}))
        binary = self.gld_info.parent / 'MacOS/NVKZinkGLDriver'
        binary.parent.mkdir()
        load = struct.pack('<6I', 0x32, 24, 1, 0x000f0500, 0x000f0500, 0)
        self.new_gld = struct.pack('<8I', 0xfeedfacf, 0x01000007, 3, 8, 1, len(load), 0, 0) + load
        binary.write_bytes(self.new_gld)
        probe = self.payload / 'Library/GPUBundles/nvmtl/zink/gld_bundle_probe'
        probe.parent.mkdir(parents=True, exist_ok=True)
        probe.write_text('#!/bin/bash\n[ "$1" = NVKZinkGLDriver ] || exit 2\n'
                         '[ "${FAKE_GLD_LOOKUP_FAIL:-0}" != 1 ]\n')
        probe.chmod(0o755)
        old = self.root / 'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/MacOS/NVKZinkGLDriver'
        old.parent.mkdir(parents=True)
        old.write_bytes(b'old-native-gld')
        self.old[old] = old.read_bytes()
        for rel in ('Library/Extensions/NVAccel.kext/Contents/Info.plist',
                    'Library/NullMoth/kexts/26/NVAccel.kext/Contents/Info.plist'):
            info = self.root / rel
            info.parent.mkdir(parents=True, exist_ok=True)
            info.write_bytes(plistlib.dumps({'IOKitPersonalities': {
                'NVAccel': {'IOGLBundleName': 'AppleMetalOpenGLRenderer'}}}))
            self.old[info] = info.read_bytes()
        self.refresh_manifest()

    def refresh_manifest(self):
        rows = [hashlib.sha256(p.read_bytes()).hexdigest() + '  ' + str(p.relative_to(self.payload))
                for p in self.payload.rglob('*') if p.is_file() and p.name != 'SHA256SUMS']
        (self.payload / 'SHA256SUMS').write_text('\n'.join(rows) + '\n')

    def test_complete_native_install_and_boot_fallback(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_creates(2)
        binary = self.root / 'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/MacOS/NVKZinkGLDriver'
        self.assertEqual(binary.read_bytes(), self.new_gld)
        self.assertEqual(binary.stat().st_mode & 0o022, 0)
        for relative in ('Library/Extensions/NVAccel.kext', 'Library/NullMoth/kexts/26/NVAccel.kext'):
            data = plistlib.loads((self.root / relative / 'Contents/Info.plist').read_bytes())
            self.assertEqual(data['IOKitPersonalities']['NVAccel']['IOGLBundleName'], 'NVKZinkGLDriver')
        data = plistlib.loads((self.root / 'Library/NullMoth/kexts/15/NVAccel.kext/Contents/Info.plist').read_bytes())
        self.assertEqual(data['IOKitPersonalities']['NVAccel']['IOGLBundleName'], 'NVKZinkGLDriver')
        self.assertIn('NVKZinkGLDriver.bundle', (self.root / 'Library/NullMoth/lastgood.sh').read_text())

    def test_lookup_failure_restores_plugin_runtime_personalities_and_collection(self):
        result = self.run_install(FAKE_GLD_LOOKUP_FAIL='1')
        self.assert_restored(result)
        self.assertIn('macOS 不信任已安装的 OpenGL 组件', result.stdout + result.stderr)
        self.assert_creates(1)

    def test_sequoia_native_install_uses_sequoia_accelerator(self):
        self.env['FAKE_OS_VERSION'] = '15.8.1'
        result = self.run_install()
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assert_creates(2)
        info = self.root/'Library/Extensions/NVAccel.kext/Contents/Info.plist'
        self.assertEqual(plistlib.loads(info.read_bytes())['IOKitPersonalities']['NVAccel']['IOGLBundleName'], 'NVKZinkGLDriver')
        self.assertEqual((self.root/'Library/NullMoth/os-major').read_text().strip(), '15')

    def test_old_tahoe_only_package_is_not_enabled_on_sequoia(self):
        self.env['FAKE_OS_VERSION'] = '15.8.1'
        info = self.payload/'Library/NullMoth/kexts/15/NVAccel.kext/Contents/Info.plist'
        data = plistlib.loads(info.read_bytes());data['IOKitPersonalities']['NVAccel']['IOGLBundleName']='AppleMetalOpenGLRenderer'
        info.write_bytes(plistlib.dumps(data));self.refresh_manifest()
        result = self.run_install()
        self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertIn('未为 macOS 15 启用原生 Zink',result.stdout+result.stderr)
        self.assertFalse((self.root/'creates').exists())

    def test_sequoia_native_lookup_failure_rolls_back(self):
        self.env['FAKE_OS_VERSION'] = '15.8.1'
        self.assert_restored(self.run_install(FAKE_GLD_LOOKUP_FAIL='1'))
        self.assert_creates(1)

    def test_late_collection_failure_restores_native_installation(self):
        result = self.run_install(FAKE_LIVE_FAIL='1')
        self.assert_restored(result)
        self.assert_creates(2)

    def test_first_native_install_failure_removes_new_plugin(self):
        import shutil
        plugin = self.root / 'Library/GPUBundles/NVKZinkGLDriver.bundle'
        self.old = {p: data for p, data in self.old.items() if not p.is_relative_to(plugin)}
        shutil.rmtree(plugin)
        self.assert_restored(self.run_install(FAKE_GLD_LOOKUP_FAIL='1'))
        self.assertFalse(plugin.exists())

    def test_os_update_keeps_native_gld_selection(self):
        tool = self.bin/'sw_vers'
        tool.write_text(tool.read_text().replace("'25G241' if", "'25G242' if"))
        self.env['FAKE_OS_VERSION'] = '26.7.2'
        result = self.run_install()
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assert_creates(2)
        info = self.root/'Library/Extensions/NVAccel.kext/Contents/Info.plist'
        self.assertEqual(plistlib.loads(info.read_bytes())['IOKitPersonalities']['NVAccel']['IOGLBundleName'], 'NVKZinkGLDriver')
        self.assertEqual((self.root/'Library/GPUBundles/NVKZinkGLDriver.bundle/Contents/MacOS/NVKZinkGLDriver').read_bytes(),self.new_gld)

    def external_extension(self):
        # Model an approved macFUSE-style bundle outside /Library/Extensions.
        source = self.root / 'external drivers' / 'External.kext'
        (source / 'Contents').mkdir(parents=True)
        (source / 'Contents/Info.plist').write_bytes(plistlib.dumps({
            'CFBundleIdentifier': 'org.test.External', 'CFBundleVersion': '1'}))
        tool = self.bin / 'kmutil'
        tool.write_text(tool.read_text() + '''
if name=='kmutil' and a[0]=='inspect':
 p=Path(a[a.index('-A')+1])
 source=r/'external drivers'/'External.kext'
 if p==r/'Library/KernelCollections/AuxiliaryKernelExtensions.kc':
  print('org.test.External\\t1\\t'+str(source))
 elif os.environ.get('FAKE_DROP_EXTERNAL')!='all' and not (
      os.environ.get('FAKE_DROP_EXTERNAL')=='live' and str(p).endswith('.nullmoth-install-new')):
  print('org.test.External 1')
''')
        return source

    def test_external_extension_is_preserved_from_its_permanent_path(self):
        import json
        source = self.external_extension()
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = [json.loads(line) for line in (self.root/'calls.jsonl').read_text().splitlines()]
        builds = [row for row in commands if row[:2] == ['kmutil','create']]
        self.assertEqual(len(builds), 2)
        for row in builds:
            self.assertIn('-p', row)
            self.assertIn(str(source.resolve()), row)
        self.assertTrue((source/'Contents/Info.plist').is_file())

    def test_external_extension_missing_from_live_collection_rolls_back(self):
        self.external_extension()
        result = self.run_install(FAKE_DROP_EXTERNAL='live')
        self.assert_restored(result)
        self.assertIn('新内核集合未包含 org.test.External', result.stdout + result.stderr)
        self.assert_creates(2)

    def test_external_extension_in_writable_parent_is_rejected(self):
        source = self.external_extension()
        source.parent.chmod(0o777)
        result = self.run_install()
        self.assert_restored(result)
        self.assertFalse((self.root/'creates').exists())

    def cached_extension(self, identifier='com.highpoint-tech.kext.HighPointRR', version='4.22.1',
                         installed_id=None, installed_version=None, second_copy=False):
        import json
        source = self.root / 'Library/Extensions/HighPointRR.kext'
        (source / 'Contents').mkdir(parents=True)
        (source / 'Contents/Info.plist').write_bytes(plistlib.dumps({
            'CFBundleIdentifier': installed_id or identifier,
            'CFBundleVersion': installed_version or version}))
        if second_copy:
            other = self.root / 'Library/Extensions/AnotherName.kext/Contents'
            other.mkdir(parents=True)
            (other/'Info.plist').write_bytes((source/'Contents/Info.plist').read_bytes())
        recorded = '/private/var/tmp/nullmoth-remove.deleted/repo/HighPointRR.kext'
        tool = self.bin/'kmutil'
        tool.write_text(tool.read_text() + '''
if name=='kmutil' and a[0]=='inspect':
 p=Path(a[a.index('-A')+1])
 if p==r/'Library/KernelCollections/AuxiliaryKernelExtensions.kc':
  print(''' + repr(identifier+'\t'+version+'\t'+recorded) + ''')
 else: print(''' + repr(identifier+' '+version) + ''')
''')
        return source

    def test_deleted_removal_path_recovers_exact_installed_identity(self):
        import json
        source = self.cached_extension()
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        commands = [json.loads(line) for line in (self.root/'calls.jsonl').read_text().splitlines()]
        creates = [call for call in commands if call[:2]==['kmutil','create']]
        self.assertEqual(len(creates), 2)
        for call in creates:
            self.assertIn(str(source.resolve()), call)
            self.assertFalse(any('nullmoth-remove.deleted' in arg for arg in call))

    def test_stale_removal_reference_check_only_does_not_install(self):
        self.cached_extension()
        result = self.run_install(CHECK='1')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.assertIn('CHECK PASS', result.stdout)
        self.assert_creates(1)
        for path, data in self.old.items(): self.assertEqual(path.read_bytes(), data)

    def test_stale_reference_does_not_accept_same_filename_wrong_identity(self):
        self.cached_extension(installed_id='org.other.HighPointRR')
        self.assert_restored(self.run_install())
        self.assertFalse((self.root/'creates').exists())

    def test_stale_reference_does_not_substitute_different_version(self):
        self.cached_extension(installed_version='9.0')
        self.assert_restored(self.run_install())
        self.assertFalse((self.root/'creates').exists())

    def test_stale_reference_refuses_ambiguous_installed_copies(self):
        self.cached_extension(second_copy=True)
        result = self.run_install()
        self.assert_restored(result)
        self.assertIn('发现多个已安装的',result.stdout+result.stderr)
        self.assertFalse((self.root/'creates').exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
