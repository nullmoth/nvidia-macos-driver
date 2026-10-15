#!/usr/bin/env python3
"""The "Remove NVIDIA driver" boot entry stays reachable: setup makes the OpenCore picker show it on install, and
removal puts the picker back and clears the stale display settings. Negative controls run the pre-fix code (4ba580e),
which had none of this, and show the entry left hidden and the display settings left in place."""
import os, plistlib, re, subprocess, sys, tempfile, unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BOOT = '7C436110-AB2A-4BBB-A880-FE41995C9F82'
OLD_REV = '4ba580e'


def old_source():
    return subprocess.run(['git', '-C', str(ROOT), 'show', f'{OLD_REV}:app/Resources/nullmoth-setup.sh'],
                          capture_output=True, text=True, check=True).stdout


def extract(text, start_marker, end_marker):
    start = text.index(start_marker)
    end = text.index(end_marker, start) + len(end_marker)
    return text[start:end]


class PickerVisibility(unittest.TestCase):
    """picker_edits() prints exactly the config edits needed so the Remove entry is always visible."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.r = Path(self.temp.name)
        self.new_fn = extract((ROOT / 'app/Resources/nullmoth-setup.sh').read_text(),
                              'picker_edits() {', 'i=$((i+1)); done; }')

    def tearDown(self): self.temp.cleanup()

    def write_config(self, **boot):
        tools = [{'Path': 'NullMothSafe.efi', 'Arguments': '', 'Auxiliary': True, 'TextMode': False},
                 {'Path': 'NullMothSafe.efi', 'Arguments': 'once', 'Auxiliary': True, 'TextMode': False},
                 {'Path': 'OpenShell.efi', 'Auxiliary': True}]
        cfg = {'Misc': {'Boot': boot, 'Tools': tools}}
        p = self.r / 'config.plist'; p.write_bytes(plistlib.dumps(cfg)); return p

    def new_edits(self, config):
        prog = self.new_fn + '\npicker_edits "$1"\n'
        res = subprocess.run(['/bin/bash', '-c', prog, 'fixture', str(config)], capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, res.stderr)
        return [l for l in res.stdout.splitlines() if l]

    def old_edits(self, config):
        # The pre-fix visibility step: tool_index() plus the one TextMode flip. It is the whole of what 4ba580e did for
        # the picker entry - no ShowPicker, no HideAuxiliary, no Auxiliary. Run it and collect the EDITS it would apply.
        old = old_source()
        tool_index = extract(old, 'tool_index() {', 'i=$((i+1)); done; }')
        flip = extract(old, '# The tool asks for Y on screen.', 'EDITS+=("Misc.Tools.$ti.TextMode|-bool|true"); fi')
        prog = (f'TOOL_FILE=NullMothSafe.efi; C="$1"; EDITS=()\n'
                f'get() {{ plutil -extract "$1" raw -o - "$C" 2>/dev/null; }}\n'
                f'{tool_index}\n{flip}\nprintf "%s\\n" "${{EDITS[@]}}"\n')
        res = subprocess.run(['/bin/bash', '-c', prog, 'fixture', str(config)], capture_output=True, text=True)
        self.assertEqual(res.returncode, 0, res.stderr)
        return [l for l in res.stdout.splitlines() if l]

    def test_hidden_entry_is_made_visible(self):
        cfg = self.write_config(ShowPicker=False, HideAuxiliary=True, PickerMode='Apple')
        edits = self.new_edits(cfg)
        self.assertIn('Misc.Boot.ShowPicker|-bool|true', edits)
        self.assertIn('Misc.Boot.HideAuxiliary|-bool|false', edits)
        self.assertIn('Misc.Boot.PickerMode|-string|Builtin', edits)
        # both NullMoth tool entries (indices 0 and 1), never the unrelated OpenShell at index 2
        self.assertIn('Misc.Tools.0.Auxiliary|-bool|false', edits)
        self.assertIn('Misc.Tools.1.Auxiliary|-bool|false', edits)
        self.assertNotIn('Misc.Tools.2.Auxiliary|-bool|false', edits)
        self.assertIn('Misc.Tools.0.TextMode|-bool|true', edits)

    def test_negative_control_old_code_leaves_entry_hidden(self):
        # The exact config the fix targets: picker off, auxiliary hidden, Apple picker, entry auxiliary. The pre-fix
        # code produces no edit that reveals it - at most it flips TextMode, so the user still cannot see the entry.
        cfg = self.write_config(ShowPicker=False, HideAuxiliary=True, PickerMode='Apple')
        old = self.old_edits(cfg)
        self.assertFalse(any('ShowPicker' in e or 'HideAuxiliary' in e or 'PickerMode' in e or 'Auxiliary|' in e for e in old),
                         f'pre-fix code should not reveal the entry, emitted: {old}')

    def test_already_visible_config_needs_no_change(self):
        cfg = self.r / 'ok.plist'
        cfg.write_bytes(plistlib.dumps({'Misc': {'Boot': {'ShowPicker': True, 'HideAuxiliary': False, 'PickerMode': 'Builtin'},
            'Tools': [{'Path': 'NullMothSafe.efi', 'Arguments': '', 'Auxiliary': False, 'TextMode': True}]}}))
        self.assertEqual(self.new_edits(cfg), [])

    def test_external_canopy_picker_is_left_alone(self):
        # External (OpenCanopy) lists tools, so only Apple is forced off; External must survive.
        cfg = self.write_config(ShowPicker=True, PickerMode='External')
        self.assertFalse(any('PickerMode' in e for e in self.new_edits(cfg)))


MOCK = r'''
import os,sys
from pathlib import Path
n=Path(sys.argv[0]).name;a=sys.argv[1:];r=Path(os.environ['FIXTURE_ROOT'])
if n=='id':print(0)
elif n=='diskutil':
 if a[0]=='info':print('Mount Point: '+str(r/'esp'))
elif n=='plutil':os.execv('/usr/bin/plutil',['plutil']+a)
elif n=='mv':os.execv('/bin/mv',['mv']+a)
else:sys.exit(99)
'''


class RemovalRestoresPicker(unittest.TestCase):
    """--remove puts ShowPicker/HideAuxiliary/PickerMode back when this install owns them, and clears the saved
    WindowServer display settings so the driver-less boot comes up at a safe screen mode."""
    def build(self, source_text):
        self.temp = tempfile.TemporaryDirectory(); self.r = Path(self.temp.name)
        self.bin = self.r / 'bin'; self.bin.mkdir()
        for n in ['id', 'diskutil', 'plutil', 'mv']:
            p = self.bin / n; p.write_text('#!' + sys.executable + '\n' + MOCK); p.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + ':/usr/bin:/bin:/usr/sbin:/sbin', FIXTURE_ROOT=str(self.r))
        self.st = self.r / 'Library/NullMoth'; self.st.mkdir(parents=True)
        self.oc = self.r / 'esp/EFI/OC'; self.oc.mkdir(parents=True)
        self.cfg = self.oc / 'config.plist'
        self.config = {'NVRAM': {'Add': {BOOT: {'boot-args': '-v nvfb=1', 'csr-active-config': bytes(4)}}},
            'Misc': {'Security': {'SecureBootModel': 'Disabled'},
                     'Boot': {'ShowPicker': True, 'HideAuxiliary': False, 'PickerMode': 'Builtin'},
                     'Tools': [{'Path': 'NullMothSafe.efi'}]},
            'UEFI': {'Quirks': {'ResizeGpuBars': 13}}, 'Booter': {'Quirks': {'ResizeAppleGpuBars': -1}},
            'Kernel': {'Block': [{'Identifier': 'com.apple.iokit.IONDRVSupport', 'Enabled': True}]}}
        self.cfg.write_bytes(plistlib.dumps(self.config))
        self.state = self.st / 'state'
        self.state.write_text(
            "EFI_UUID='original-esp'\nOCREL='EFI/OC'\nCONFIG_BACKUP_REL='EFI/OC/none'\nCONFIG_SHA_AFTER='changed'\n"
            "ADDED_ARGS=''\nREMOVED_ARGS=''\nOLD_CSR=0\nNEW_CSR=2563\nOLD_SBM='Default'\n"
            "SET_SHOWPICKER='1'\nOLD_SHOWPICKER='false'\nSET_HIDEAUX='1'\nOLD_HIDEAUX='true'\n"
            "SET_PICKERMODE='1'\nOLD_PICKERMODE='Apple'\n")
        self.recover = self.r / 'Library/LaunchDaemons/com.nullmoth.recover.plist'
        self.recover.parent.mkdir(parents=True); self.recover.write_text('keep')
        self.tool = self.oc / 'Tools/NullMothSafe.efi'; self.tool.parent.mkdir(); self.tool.write_bytes(b'keep')
        # a saved display configuration from when the NVIDIA driver drove the screen
        self.prefs = self.r / 'Library/Preferences'; (self.prefs / 'ByHost').mkdir(parents=True)
        self.ws = self.prefs / 'com.apple.windowserver.displays.plist'; self.ws.write_text('bad-mode')
        self.wsbyhost = self.prefs / 'ByHost/com.apple.windowserver.ABC.plist'; self.wsbyhost.write_text('bad-mode')
        source = source_text.replace('PATH=/usr/bin:/bin:/usr/sbin:/sbin\n', '').replace('/Library/', str(self.r) + '/Library/')
        self.script = self.r / 'nullmoth-setup.sh'; self.script.write_text(source)
        un = self.r / 'nullmoth-uninstall.sh'; un.write_text('#!/bin/bash\nexit 0\n'); un.chmod(0o755)

    def run_remove(self):
        return subprocess.run(['/bin/bash', str(self.script), '--remove'], env=self.env, capture_output=True, text=True)

    def tearDown(self):
        if hasattr(self, 'temp'): self.temp.cleanup()

    def test_owned_picker_settings_are_restored_and_prefs_cleared(self):
        self.build((ROOT / 'app/Resources/nullmoth-setup.sh').read_text())
        r = self.run_remove(); self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        boot = plistlib.loads(self.cfg.read_bytes())['Misc']['Boot']
        self.assertEqual(boot['ShowPicker'], False)
        self.assertEqual(boot['HideAuxiliary'], True)
        self.assertEqual(boot['PickerMode'], 'Apple')
        self.assertFalse(self.ws.exists()); self.assertFalse(self.wsbyhost.exists())
        self.assertTrue(list(self.st.glob('display-prefs-*/com.apple.windowserver.displays.plist')))

    def test_unowned_picker_settings_are_left_alone(self):
        self.build((ROOT / 'app/Resources/nullmoth-setup.sh').read_text())
        # this install did not change the picker settings (SET_*=0): removal must not touch the user's own values
        self.state.write_text(self.state.read_text().replace("SET_SHOWPICKER='1'", "SET_SHOWPICKER='0'")
            .replace("SET_HIDEAUX='1'", "SET_HIDEAUX='0'").replace("SET_PICKERMODE='1'", "SET_PICKERMODE='0'"))
        r = self.run_remove(); self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        boot = plistlib.loads(self.cfg.read_bytes())['Misc']['Boot']
        self.assertEqual(boot, {'ShowPicker': True, 'HideAuxiliary': False, 'PickerMode': 'Builtin'})

    def test_negative_control_old_code_neither_restores_nor_clears(self):
        self.build(old_source())
        r = self.run_remove(); self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        boot = plistlib.loads(self.cfg.read_bytes())['Misc']['Boot']
        # pre-fix code left the forced-on picker settings in place and never cleared the stale display settings
        self.assertEqual(boot['ShowPicker'], True)
        self.assertEqual(boot['HideAuxiliary'], False)
        self.assertEqual(boot['PickerMode'], 'Builtin')
        self.assertTrue(self.ws.exists()); self.assertTrue(self.wsbyhost.exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
