#!/usr/bin/env python3
"""Exercise optional panel install, removal and rollback in a private filesystem."""
import hashlib
import os
import plistlib
import pty
import subprocess
import unittest
from primary_install_fixture import Install


class IntelPanelInstall(Install):
    def setUp(self):
        super().setUp()
        self.panel = self.payload/'Optional/NMIntelFB.kext'
        binary = self.panel/'Contents/MacOS/NMIntelFB'
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b'new-panel')
        (self.panel/'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'com.nullmoth.NMIntelFB'}))
        sign = self.bin/'codesign'
        sign.write_text('#!/bin/bash\n[ "${PANEL_SIGN_FAIL:-0}" != 1 ]\n')
        sign.chmod(0o755)
        km = self.bin/'kmutil'
        code = km.read_text().replace("for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC']:",
            "if os.environ.get('EXPECT_PANEL') and not os.environ.get('MISSING_PANEL'):print('com.nullmoth.NMIntelFB 0.1')\n  for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC']:")
        km.write_text(code)
        self.refresh_manifest()
        self.live = self.root/'Library/Extensions/NMIntelFB.kext'

    def refresh_manifest(self):
        rows = [hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(self.payload))
                for p in self.payload.rglob('*') if p.is_file() and p.name!='SHA256SUMS']
        (self.payload/'SHA256SUMS').write_text('\n'.join(rows)+'\n')

    def existing(self):
        import shutil
        shutil.copytree(self.panel,self.live)
        (self.live/'Contents/MacOS/NMIntelFB').write_bytes(b'old-panel')
        for p in self.live.rglob('*'):
            if p.is_file(): self.old[p]=p.read_bytes()

    def run_panel(self, choice=None, answer=None, **env):
        cmd=['/bin/bash',str(self.script)]
        if choice: cmd+=['--intel-panel='+choice]
        if answer is None:
            return subprocess.run(cmd,env=dict(self.env,**env),capture_output=True,text=True,timeout=30)
        master,slave=pty.openpty()
        try:
            os.write(master,(answer+'\n').encode())
            return subprocess.run(cmd,env=dict(self.env,**env),stdin=slave,
                                  stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,timeout=30)
        finally:
            os.close(master);os.close(slave)

    def test_noninteractive_default_skips_panel(self):
        r=self.run_panel();self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertFalse(self.live.exists())
        self.assertIn('No interactive input',r.stdout)

    def test_prompt_default_no(self):
        r=self.run_panel(answer='');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertIn('[y/N]',r.stdout);self.assertFalse(self.live.exists())

    def test_prompt_yes_installs_and_fallback_covers_panel(self):
        r=self.run_panel(answer='yes',EXPECT_PANEL='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.live/'Contents/MacOS/NMIntelFB').read_bytes(),b'new-panel')
        self.assertIn('NMIntelFB;', (self.root/'Library/NullMoth/lastgood.sh').read_text())

    def test_existing_opt_in_survives_update_without_prompt(self):
        self.existing();r=self.run_panel(EXPECT_PANEL='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('[y/N]',r.stdout)
        self.assertEqual((self.live/'Contents/MacOS/NMIntelFB').read_bytes(),b'new-panel')

    def test_explicit_no_removes_previous_panel(self):
        self.existing();r=self.run_panel('no');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertFalse(self.live.exists())
        self.assertIn('NMIntelFB;', (self.root/'Library/NullMoth/lastgood.sh').read_text())

    def test_failed_fresh_install_removes_new_panel(self):
        r=self.run_panel('yes',EXPECT_PANEL='1',FAKE_LIVE_FAIL='1');self.assert_restored(r)
        self.assertFalse(self.live.exists())

    def test_failed_update_restores_previous_panel(self):
        self.existing();r=self.run_panel(EXPECT_PANEL='1',FAKE_LIVE_FAIL='1');self.assert_restored(r)

    def test_failed_removal_restores_previous_panel(self):
        self.existing();r=self.run_panel('no',FAKE_LIVE_FAIL='1');self.assert_restored(r)

    def test_removal_rejects_collection_still_containing_panel(self):
        self.existing();r=self.run_panel('no',EXPECT_PANEL='1');self.assert_restored(r)
        self.assertIn('已取消选择的 NMIntelFB',r.stderr);self.assert_creates(1)

    def test_preflight_missing_panel_stops_before_live_changes(self):
        r=self.run_panel('yes',EXPECT_PANEL='1',MISSING_PANEL='1');self.assert_restored(r)
        self.assertFalse(self.live.exists());self.assert_creates(1)

    def test_check_only_does_not_install_panel(self):
        r=self.run_panel('yes',EXPECT_PANEL='1',CHECK='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertFalse(self.live.exists());self.assert_creates(1)

    def test_invalid_signature_stops_before_collection_build(self):
        r=self.run_panel('yes',PANEL_SIGN_FAIL='1');self.assert_restored(r)
        self.assertFalse((self.root/'creates').exists())

    def test_missing_optional_payload_refuses_explicit_yes(self):
        import shutil
        shutil.rmtree(self.panel);self.refresh_manifest()
        r=self.run_panel('yes');self.assert_restored(r)
        self.assertIn('缺少可选的 Intel 内屏 kext',r.stderr)


if __name__=='__main__':unittest.main(verbosity=2)
