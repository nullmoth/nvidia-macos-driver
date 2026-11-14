#!/usr/bin/env python3
"""macOS 27 installer fixture. Runs package/install-macos27.sh against a fake ESP, a fake 27 system volume and a fake
27 Data volume. kmutil, chflags, chown, nvram, sw_vers, uname, id and sudo are stubs that record their calls;
PlistBuddy is a recording pass-through to the real one so the config edit is parsed by a real plist parser.
Nothing here touches a real volume."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / 'package/install-macos27.sh'
AUX = ['NVRMFB', 'NVAccel', 'NVRMAGDC', 'NMXAudio']
GB = ['NVMTLDriver.bundle', 'NVIDIAShared.bundle', 'nvmtl']
MUTATING = {'chflags', 'chown'}

STUB = r'''
import json,os,sys
from pathlib import Path
name=Path(sys.argv[0]).name;a=sys.argv[1:];r=Path(os.environ['FIXTURE_ROOT'])
done=any(r.glob('data/Library/NullMoth/backups/*/COMPLETE'))
with (r/'calls.jsonl').open('a') as f:f.write(json.dumps({'cmd':[name]+a,'backup_complete':done})+'\n')
if name=='id':print('0')
elif name=='uname':print('x86_64')
elif name=='sw_vers':print(os.environ.get('FAKE_HOST','26.7.1') if a==['-productVersion'] else '25G241')
elif name=='nvram':print('csr-active-config\t'+os.environ.get('FAKE_CSR','%43%0a%00%00'))
elif name in('chflags','chown'):pass
elif name=='sudo':
 # sudo -u _windowserver /bin/test -r FILE: WindowServer's user is "other" to a root:wheel file
 sys.exit(0 if a[:2]==['-u','_windowserver'] and Path(a[-1]).stat().st_mode&0o004 else 1)
elif name=='kmutil':
 if a[0]=='create':
  if os.environ.get('FAKE_CREATE_FAIL'):sys.exit(9)
  Path(a[a.index('-A')+1]).write_bytes(b'NEW27-AUX')
 elif a[0]=='inspect':
  ids=['com.nullmoth.'+k for k in os.environ.get('FAKE_IDS','NVRMFB NVAccel NVRMAGDC NMXAudio').split()]
  print('\n'.join(i+' 1.0' for i in ids))
 else:sys.exit(99)
elif name=='PlistBuddy':
 os.execv('/usr/libexec/PlistBuddy',['PlistBuddy']+a)
else:sys.exit(99)
'''


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def snapshot(*roots):
    """every file (path, sha) and directory under the roots, minus the installer's own backup directory"""
    out = {}
    for root in roots:
        for p in sorted(Path(root).rglob('*')):
            rel = str(p.relative_to(root.parent))
            if '/backups' in rel or rel.endswith('/backups'):
                continue
            out[rel] = 'dir' if p.is_dir() else sha(p)
    return out


class Install27(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='nullmoth-install27-')
        r = self.root = Path(self.temp.name)
        self.sys, self.data, self.esp, self.payload = r / 'sys', r / 'data', r / 'esp', r / 'payload'
        (r / 'tmp').mkdir()
        # --- fake 27 system volume
        cs = self.sys / 'System/Library/CoreServices'
        cs.mkdir(parents=True)
        self.write_version(cs, '27.0', '26A5425a')
        for rel in ['System/Library/Kernels/kernel', 'System/Library/KernelCollections/BootKernelExtensions.kc',
                    'System/Library/KernelCollections/SystemKernelExtensions.kc']:
            self.put(self.sys / rel, 'x27')
        (self.sys / 'System/Library/Kernels/kernel').chmod(0o755)
        self.put(self.sys / 'usr/share/nvfw/nvidia/615.78.08/old.bin', 'old-fw')
        # --- fake 27 Data volume with an older install, some items absent on purpose
        d = self.data / 'Library'
        for k in ['NVRMFB', 'NVAccel']:
            self.put(d / f'Extensions/{k}.kext/Contents/MacOS/{k}', 'old-' + k)
        self.put(d / 'Extensions/Unrelated.kext/Contents/MacOS/Unrelated', 'keep-me')
        for b in GB:
            self.put(d / f'GPUBundles/{b}/fixture', 'old-' + b)
        self.put(d / 'GPUBundles/nvmtl-allow.txt', 'old-allow\n')
        self.put(d / 'KernelCollections/AuxiliaryKernelExtensions.kc', 'OLD-AUX')
        (d / 'NullMoth').mkdir()
        # --- fake ESP with OpenCore config; Lilu and AMFIPass carry no MinKernel, NVRM must not inherit that
        cfg = self.esp / 'EFI/BOOT'
        (cfg / 'Kexts/Lilu.kext').mkdir(parents=True)
        entries = [{'Arch': 'x86_64', 'BundlePath': n + '.kext', 'Comment': '', 'Enabled': True, 'ExecutablePath': '',
                    'MaxKernel': '', 'MinKernel': '', 'PlistPath': 'Contents/Info.plist'} for n in ('Lilu', 'AMFIPass')]
        self.cfg = cfg / 'config.plist'
        self.cfg.write_bytes(plistlib.dumps({'Kernel': {'Add': entries}, 'Misc': {'Boot': {'ScanPolicy': 524547}}}))
        self.cfg_before = self.cfg.read_bytes()
        # --- payload
        self.make_payload('v1')
        # --- stubs and a script whose tool path points at them
        self.bin = r / 'bin'
        self.bin.mkdir()
        for n in ['id', 'uname', 'sw_vers', 'nvram', 'chflags', 'chown', 'sudo', 'kmutil', 'PlistBuddy']:
            p = self.bin / n
            p.write_text('#!' + sys.executable + '\n' + STUB)
            p.chmod(0o755)
        src = SCRIPT.read_text()
        a = 'PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH'
        b = 'PB=/usr/libexec/PlistBuddy'
        assert src.count(a) == 1 and src.count(b) == 1, 'installer anchors moved'
        src = src.replace(a, f'PATH={self.bin}:/usr/bin:/bin:/usr/sbin:/sbin; export PATH').replace(b, f'PB={self.bin}/PlistBuddy')
        self.script = self.payload / 'install-macos27.sh'
        self.script.write_text(src)
        self.env = dict(os.environ, FIXTURE_ROOT=str(r), TMPDIR=str(r / 'tmp'))
        self.before = snapshot(self.sys, self.data, self.esp)

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def put(p, text):
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    @staticmethod
    def write_version(cs, ver, build):
        (cs / 'SystemVersion.plist').write_bytes(plistlib.dumps({'ProductVersion': ver, 'ProductBuildVersion': build}))

    def make_payload(self, tag):
        p = self.payload
        for k in AUX + ['NVRM']:
            self.put(p / f'Library/Extensions/{k}.kext/Contents/MacOS/{k}', f'{tag}-{k}')
            (p / f'Library/Extensions/{k}.kext/Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleExecutable': k}))
        for b in GB:
            self.put(p / f'Library/GPUBundles/{b}/fixture', f'{tag}-{b}')
        self.put(p / 'Library/GPUBundles/nvmtl-allow.txt', 'WindowServer\n-mtlneg\n*\n')
        self.put(p / 'Users/Shared/nvfw/nvidia/615.78.08/gsp.bin', f'{tag}-fw')

    def run_script(self, *extra, **env):
        args = ['--payload', str(self.payload), '--system', str(self.sys), '--data', str(self.data), '--esp', str(self.esp)]
        return subprocess.run(['/bin/bash', str(self.script), *args, *extra], env=dict(self.env, **env), capture_output=True, text=True)

    def calls(self):
        f = self.root / 'calls.jsonl'
        return [json.loads(l) for l in f.read_text().splitlines()] if f.exists() else []

    def mutating(self):
        return [c for c in self.calls() if c['cmd'][0] in MUTATING or c['cmd'][:2] == ['kmutil', 'create']]

    def entries(self):
        return plistlib.loads(self.cfg.read_bytes())['Kernel']['Add']

    def assert_untouched(self, res=None):
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before, (res.stdout + res.stderr) if res else '')
        self.assertEqual(self.mutating(), [])

    # ---------------------------------------------------------------- arms
    def test_dry_run_prints_every_stage_and_changes_nothing(self):
        r = self.run_script('--dry-run')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        for word in ['backups to', 'aux collection', 'firmware to', "27's /Library/Extensions", 'Metal plugin and allow list',
                     'install the auxiliary collection', 'ESP:']:
            self.assertIn(word, r.stdout)
        self.assertGreaterEqual(r.stdout.count('DRY-RUN would'), 7, r.stdout)
        self.assertIn('nothing was changed', r.stdout)
        self.assert_untouched(r)
        self.assertFalse(list(self.data.glob('Library/NullMoth/backups')), 'a dry run must not even create the backup directory')

    def test_real_run_order_backups_first_and_exact_kext_list(self):
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        order = [l[3:] for l in r.stdout.splitlines() if l.startswith('== ')]
        want = ['preflight', 'backups to', 'aux collection', 'firmware to', "kext bundles", 'Metal plugin', 'install the auxiliary',
                'ESP:', 'verify', 'done']
        pos = [next(i for i, l in enumerate(order) if l.startswith(w)) for w in want]
        self.assertEqual(pos, sorted(pos), f'stage order {order}')
        # every mutating call (chown, chflags, kmutil create) happened after the backup was COMPLETE
        muts = self.mutating()
        self.assertTrue(muts)
        self.assertTrue(all(c['backup_complete'] for c in muts), muts)
        # kmutil create: exactly the four ids, never NVRM, the 27 build, x86_64, --allow-missing-kdk
        create = next(c['cmd'] for c in self.calls() if c['cmd'][:2] == ['kmutil', 'create'])
        ids = [create[i + 1] for i, x in enumerate(create) if x == '-b']
        self.assertEqual(sorted(ids), sorted('com.nullmoth.' + k for k in AUX), ids)
        self.assertNotIn('com.nullmoth.NVRM', ids)
        self.assertEqual(create[create.index('--build') + 1], '26A5425a')
        self.assertEqual(create[create.index('-a') + 1], 'x86_64')
        self.assertIn('--allow-missing-kdk', create)
        self.assertNotIn('install', [c['cmd'][1] for c in self.calls() if c['cmd'][0] == 'kmutil'])
        # collection: noschg before the replace, schg after, and the file is the built one
        flags = [c['cmd'][1:] for c in self.calls() if c['cmd'][0] == 'chflags']
        self.assertEqual([f[0] for f in flags], ['noschg', 'schg'])
        kc = self.data / 'Library/KernelCollections/AuxiliaryKernelExtensions.kc'
        self.assertEqual(kc.read_bytes(), b'NEW27-AUX')
        # bundles, plugin, allow list (WindowServer above *), firmware, ESP kext
        for k in AUX:
            self.assertEqual((self.data / f'Library/Extensions/{k}.kext/Contents/MacOS/{k}').read_text(), f'v1-{k}')
        self.assertFalse((self.data / 'Library/Extensions/NVRM.kext').exists(), 'NVRM goes to the ESP only')
        self.assertEqual((self.data / 'Library/Extensions/Unrelated.kext/Contents/MacOS/Unrelated').read_text(), 'keep-me')
        allow = (self.data / 'Library/GPUBundles/nvmtl-allow.txt').read_text().splitlines()
        self.assertLess(allow.index('WindowServer'), allow.index('*'))
        self.assertEqual((self.sys / 'usr/share/nvfw/nvidia/615.78.08/gsp.bin').read_text(), 'v1-fw')
        self.assertFalse((self.sys / 'usr/share/nvfw/nvidia/615.78.08/old.bin').exists())
        self.assertEqual((self.esp / 'EFI/BOOT/Kexts/NVRM.kext/Contents/MacOS/NVRM').read_text(), 'v1-NVRM')

    def test_config_edit_minkernel_on_every_added_entry_and_others_untouched(self):
        self.assertEqual(self.run_script().returncode, 0)
        e = self.entries()
        self.assertEqual([x['BundlePath'] for x in e], ['Lilu.kext', 'AMFIPass.kext', 'NVRM.kext'])
        nv = [x for x in e if x['BundlePath'] not in ('Lilu.kext', 'AMFIPass.kext')]
        self.assertEqual(len(nv), 1)
        for x in nv:
            self.assertEqual(x['MinKernel'], '27.0.0', x)
            self.assertEqual(x['Arch'], 'x86_64')
            self.assertIs(x['Enabled'], True)
            self.assertEqual(x['ExecutablePath'], 'Contents/MacOS/NVRM')
        self.assertEqual(e[:2], plistlib.loads(self.cfg_before)['Kernel']['Add'])
        self.assertEqual(plistlib.loads(self.cfg.read_bytes())['Misc'], plistlib.loads(self.cfg_before)['Misc'])
        self.assertFalse(list(self.cfg.parent.glob('.config.plist.*')), 'staging copy left behind')

    def test_injected_aux_kexts_are_switched_off_and_rollback_turns_them_back_on(self):
        # The live 27 box's config also injected NVAccel and NMXAudio from the ESP (older builds than the collection's).
        d = plistlib.loads(self.cfg_before)
        d['Kernel']['Add'] += [{'BundlePath': n + '.kext', 'Arch': 'x86_64', 'Enabled': True, 'MinKernel': '27.0.0',
                                'MaxKernel': '', 'ExecutablePath': 'Contents/MacOS/' + n, 'PlistPath': 'Contents/Info.plist'}
                               for n in ('NVAccel', 'NMXAudio')]
        self.cfg.write_bytes(plistlib.dumps(d)); before = self.cfg.read_bytes()
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        e = {x['BundlePath']: x for x in self.entries()}
        self.assertIs(e['NVAccel.kext']['Enabled'], False)
        self.assertIs(e['NMXAudio.kext']['Enabled'], False)
        self.assertEqual(e['NVAccel.kext']['MinKernel'], '27.0.0', 'only Enabled changes')
        self.assertIs(e['NVRM.kext']['Enabled'], True)
        self.assertIs(e['Lilu.kext']['Enabled'], d['Kernel']['Add'][0]['Enabled'], 'other entries untouched')
        r = self.run_script('--rollback')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.cfg.read_bytes(), before)

    def test_second_run_is_idempotent_and_a_changed_payload_updates_in_place(self):
        self.assertEqual(self.run_script().returncode, 0)
        after_first = snapshot(self.sys, self.data, self.esp)
        n_calls = len(self.calls())
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn('already installed', r.stdout)
        self.assertEqual([c for c in self.calls()[n_calls:] if c['cmd'][0] in MUTATING or c['cmd'][:2] == ['kmutil', 'create']], [])
        self.assertEqual(snapshot(self.sys, self.data, self.esp), after_first)
        self.assertEqual(len(list(self.data.glob('Library/NullMoth/backups/*'))), 1, 'a no-op run must not make a second backup')
        # new payload: one NVRM entry still, new bytes in place
        self.make_payload('v2')
        r = self.run_script()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(len([x for x in self.entries() if x['BundlePath'] == 'NVRM.kext']), 1)
        self.assertEqual((self.esp / 'EFI/BOOT/Kexts/NVRM.kext/Contents/MacOS/NVRM').read_text(), 'v2-NVRM')
        self.assertEqual(len(list(self.data.glob('Library/NullMoth/backups/*'))), 2)

    def test_rollback_restores_byte_identical(self):
        self.assertEqual(self.run_script().returncode, 0)
        self.assertNotEqual(snapshot(self.sys, self.data, self.esp), self.before)
        r = self.run_script('--rollback')
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before)
        self.assertEqual(self.cfg.read_bytes(), self.cfg_before)
        self.assertFalse((self.data / 'Library/NullMoth/macos27-install.receipt').exists())
        # collection: noschg before it was put back
        self.assertEqual([c['cmd'][1] for c in self.calls() if c['cmd'][0] == 'chflags'][-1], 'noschg')

    def test_rollback_after_two_installs_goes_back_to_the_original(self):
        self.assertEqual(self.run_script().returncode, 0)
        self.make_payload('v2')
        self.assertEqual(self.run_script().returncode, 0)
        self.assertEqual(self.run_script('--rollback').returncode, 0)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before)

    def test_rollback_refuses_a_tampered_backup(self):
        self.assertEqual(self.run_script().returncode, 0)
        bk = next(self.data.glob('Library/NullMoth/backups/macos27-*'))
        (bk / 'config.plist').write_bytes(b'tampered')
        installed = snapshot(self.sys, self.data, self.esp)
        r = self.run_script('--rollback')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('SHA256SUMS', r.stderr)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), installed)

    def test_failed_collection_build_restores_everything(self):
        r = self.run_script(FAKE_CREATE_FAIL='1')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('kmutil create failed', r.stderr)
        self.assertIn('restored from', r.stderr)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before)

    def test_collection_with_nvrm_inside_is_refused(self):
        r = self.run_script(FAKE_IDS='NVRMFB NVAccel NVRMAGDC NMXAudio NVRM')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('expected exactly', r.stderr)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before)

    # ---------------------------------------------------------------- negative arms
    def test_wrong_os_version_refuses_without_touching_anything(self):
        self.write_version(self.sys / 'System/Library/CoreServices', '26.7.1', '25G241')
        self.before = snapshot(self.sys, self.data, self.esp)
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('macOS 27 only', r.stderr)
        self.assert_untouched(r)

    def test_host_27_needs_local27_and_local27_skips_esp_and_collection(self):
        r = self.run_script(FAKE_HOST='27.0')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('need macOS 26', r.stderr)
        self.assert_untouched(r)
        args = ['--payload', str(self.payload), '--system', str(self.sys), '--data', str(self.data), '--local27']
        r = subprocess.run(['/bin/bash', str(self.script), *args], env=dict(self.env, FAKE_HOST='27.0'), capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.cfg.read_bytes(), self.cfg_before)
        self.assertFalse(any(c['cmd'][:2] == ['kmutil', 'create'] for c in self.calls()))
        self.assertEqual((self.data / 'Library/Extensions/NVAccel.kext/Contents/MacOS/NVAccel').read_text(), 'v1-NVAccel')
        self.assertEqual((self.data / 'Library/KernelCollections/AuxiliaryKernelExtensions.kc').read_text(), 'OLD-AUX')

    def test_allow_list_without_windowserver_above_star_is_refused(self):
        self.put(self.payload / 'Library/GPUBundles/nvmtl-allow.txt', '*\nWindowServer\n')
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('WindowServer', r.stderr)
        self.assert_untouched(r)

    def test_missing_kext_in_payload_is_refused(self):
        import shutil
        shutil.rmtree(self.payload / 'Library/Extensions/NMXAudio.kext')
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('NMXAudio.kext', r.stderr)
        self.assert_untouched(r)

    def test_insufficient_csr_is_refused(self):
        r = self.run_script(FAKE_CSR='%01%00%00%00')
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('csr-active-config', r.stderr)
        self.assert_untouched(r)

    def test_two_opencore_configs_without_choice_is_refused(self):
        oc = self.esp / 'EFI/OC'
        oc.mkdir()
        (oc / 'config.plist').write_bytes(self.cfg_before)
        self.before = snapshot(self.sys, self.data, self.esp)
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('pass --config', r.stderr)
        self.assert_untouched(r)

    def test_duplicate_nvrm_entries_stop_and_restore(self):
        d = plistlib.loads(self.cfg_before)
        d['Kernel']['Add'] += [{'BundlePath': 'NVRM.kext', 'Arch': 'x86_64', 'Enabled': True, 'MinKernel': ''}] * 2
        self.cfg.write_bytes(plistlib.dumps(d))
        self.before = snapshot(self.sys, self.data, self.esp)
        r = self.run_script()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('more than one', r.stderr)
        self.assertEqual(snapshot(self.sys, self.data, self.esp), self.before)

    def test_read_only_system_volume_stops_before_any_change(self):
        share = self.sys / 'usr/share'
        share.chmod(0o555)
        try:
            self.before = snapshot(self.sys, self.data, self.esp)
            r = self.run_script()
        finally:
            share.chmod(0o755)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('read-only', r.stderr)
        self.assert_untouched(r)


if __name__ == '__main__':
    unittest.main(verbosity=2)
