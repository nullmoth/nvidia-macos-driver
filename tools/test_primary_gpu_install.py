"""Exercise selector opt-in/update/rollback without the separate 1401 repository."""
import hashlib,importlib.util,json,os,plistlib,subprocess,unittest,tempfile
from pathlib import Path
R=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('installer_fixture',R/'tools/primary_install_fixture.py')
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)

class PrimaryInstall(m.Install):
    def setUp(self):
        super().setUp()
        for root in (self.payload,self.root):
            for name in ('NVRM','NVAccel'):
                path=root/f'Library/Extensions/{name}.kext/Contents/Info.plist'
                path.write_bytes(plistlib.dumps({'IOKitPersonalities':{name:{'IOPCIPrimaryMatch':'0x000010de&0x0000ffff'}}}))
        for os_major in (15,26):
            path=self.payload/f'Library/NullMoth/kexts/{os_major}/NVAccel.kext/Contents/Info.plist'
            p=plistlib.loads(path.read_bytes());p['IOKitPersonalities']={'NVAccel':{'IOPCIPrimaryMatch':'0x000010de&0x0000ffff'}};path.write_bytes(plistlib.dumps(p))
        selector=self.root/'Library/Extensions/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector'
        selector.parent.mkdir(parents=True);selector.write_bytes(b'previous-selector')
        self.old[selector]=selector.read_bytes()
        fake=self.bin/'codesign';fake.write_text('#!/bin/bash\n[ "${FAKE_SIGN_FAIL:-0}" != 1 ]\n');fake.chmod(0o755)
        km=self.bin/'kmutil';s=km.read_text().replace("for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC']:","if os.environ.get('EXPECT_PRIMARY'):print('org.hackintosh.experimental.NVPrimarySelector 0.1.0')\n  for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC']:")
        km.write_text(s)
        self.refresh_manifest()
    def refresh_manifest(self):
        lines=[hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(self.payload)) for p in self.payload.rglob('*') if p.is_file() and p.name!='SHA256SUMS']
        (self.payload/'SHA256SUMS').write_text('\n'.join(lines)+'\n')
    def opt_in(self,partial=False):
        for name in ('NVRM',) if partial else ('NVRM','NVAccel'):
            path=self.root/f'Library/Extensions/{name}.kext/Contents/Info.plist'
            p=plistlib.loads(path.read_bytes());p['IOKitPersonalities'][name]['IOPropertyMatch']={'nullmoth-auto-primary':True};path.write_bytes(plistlib.dumps(p));self.old[path]=path.read_bytes()
    def test_update_retains_primary_in_live_and_both_os_caches(self):
        self.opt_in();r=self.run_install(EXPECT_PRIMARY='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        for rel,name in [('Library/Extensions/NVRM.kext','NVRM'),('Library/Extensions/NVAccel.kext','NVAccel'),('Library/NullMoth/kexts/15/NVAccel.kext','NVAccel'),('Library/NullMoth/kexts/26/NVAccel.kext','NVAccel')]:
            d=self.root/rel;p=plistlib.loads((d/'Contents/Info.plist').read_bytes())
            self.assertEqual(p['IOKitPersonalities'][name]['IOPropertyMatch'],{'nullmoth-auto-primary':True})
        self.assertEqual((self.root/'Library/Extensions/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector').read_bytes(),b'previous-selector')
        for major in (15,26):subprocess.run(['shasum','-a','256','-c','SHA256SUMS','--quiet'],cwd=self.root/f'Library/NullMoth/kexts/{major}',check=True)
        subprocess.run(['shasum','-a','256','-c','SHA256SUMS','--quiet'],cwd=self.payload,check=True)
    def test_partial_matching_stops_before_collection_or_live_changes(self):
        self.opt_in(partial=True);r=self.run_install();self.assert_restored(r)
        self.assertIn('incomplete primary-GPU',r.stdout+r.stderr);self.assertFalse((self.root/'creates').exists())
    def test_primary_preflight_sign_failure_is_before_live_replacement(self):
        self.opt_in();r=self.run_install(FAKE_SIGN_FAIL='1',EXPECT_PRIMARY='1');self.assert_restored(r)
        self.assertFalse((self.root/'creates').exists())
    def test_primary_live_failure_restores_matching_selector_cache_and_kc(self):
        self.opt_in();r=self.run_install(FAKE_LIVE_FAIL='1',EXPECT_PRIMARY='1');self.assert_restored(r)
        self.assert_creates(2)
    def test_first_opt_in_requires_packaged_or_installed_selector(self):
        p=self.root/'Library/Extensions/NVPrimarySelector.kext'
        import shutil
        shutil.rmtree(p)
        r=self.run_install(NM_PRIMARY_GPU='1');self.assertNotEqual(r.returncode,0)
        self.assertIn('needs NVPrimarySelector',r.stdout+r.stderr);self.assertFalse((self.root/'creates').exists())
    def test_first_opt_in_can_stage_packaged_selector(self):
        p=self.payload/'Library/Extensions/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector'
        p.parent.mkdir(parents=True);p.write_bytes(b'packaged-selector');self.refresh_manifest()
        r=self.run_install(NM_PRIMARY_GPU='1',EXPECT_PRIMARY='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.root/'Library/Extensions/NVPrimarySelector.kext/Contents/MacOS/NVPrimarySelector').read_bytes(),b'packaged-selector')
    def test_existing_conflicting_package_match_is_not_overwritten(self):
        self.opt_in()
        path=self.payload/'Library/Extensions/NVRM.kext/Contents/Info.plist'
        p=plistlib.loads(path.read_bytes());p['IOKitPersonalities']['NVRM']['IOPropertyMatch']={'pcidebug':'1:0:0'}
        path.write_bytes(plistlib.dumps(p));self.refresh_manifest()
        r=self.run_install(EXPECT_PRIMARY='1');self.assert_restored(r)
        self.assertFalse((self.root/'creates').exists())
    def test_check_only_keeps_installed_selector_and_payload_unchanged(self):
        self.opt_in();r=self.run_install(CHECK='1',EXPECT_PRIMARY='1')
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        for p,data in self.old.items():self.assertEqual(p.read_bytes(),data)
        subprocess.run(['shasum','-a','256','-c','SHA256SUMS','--quiet'],cwd=self.payload,check=True)
    def test_upstream_fallback_job_retains_selector_in_restoration_list(self):
        self.opt_in();r=self.run_install(EXPECT_PRIMARY='1')
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        fallback=(self.root/'Library/NullMoth/lastgood.sh').read_text()
        self.assertIn('for k in NVRM NVAccel NVRMFB NVRMAGDC NVPrimarySelector;',fallback)
        self.assertIn('shutdown -r now',fallback)

if __name__=='__main__':unittest.main(verbosity=2)
