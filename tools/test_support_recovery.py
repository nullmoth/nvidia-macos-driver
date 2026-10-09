#!/usr/bin/env python3
"""Exercise collection removal and recovery restart decisions with mocked system commands."""
import json,os
from pathlib import Path
import subprocess,sys,tempfile,unittest

ROOT=Path(__file__).resolve().parents[1]
MOCK=r'''
import json,os,sys
from pathlib import Path
name=Path(sys.argv[0]).name;a=sys.argv[1:];r=Path(os.environ['FIXTURE_ROOT'])
with (r/'calls.jsonl').open('a') as f:f.write(json.dumps([name]+a)+'\n')
if name=='id':print(0)
elif name=='sw_vers':print('25G241')
elif name=='kmutil':
 path=Path(a[a.index('-A')+1])
 if a[0]=='create':
  if os.environ.get('FAKE_CREATE_FAIL'):sys.exit(9)
  repo=Path(a[a.index('--repository')+1])
  assert (repo/'Other.kext/Contents/MacOS/Other').read_bytes()==b'fixture'
  assert not any((repo/(k+'.kext')).exists() for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC'])
  if not os.environ.get('FAKE_EMPTY'):path.write_bytes(b'NEW26_NON_NV')
 elif a[0]=='inspect':
  if os.environ.get('FAKE_FINAL_INSPECT_FAIL') and path==r/'Library/KernelCollections/AuxiliaryKernelExtensions.kc':sys.exit(8)
  if os.environ.get('FAKE_INSPECT_FAIL') or (os.environ.get('FAKE_BACKUP_INSPECT_FAIL') and 'backup' in str(path)):sys.exit(8)
  if not path.exists():sys.exit(7)
  if b'WITH_NV' in path.read_bytes() or os.environ.get('FAKE_NV_RETAINED'):print('com.nullmoth.NVAccel')
  else:print('org.example.Other')
elif name=='rm':
 if os.environ.get('FAKE_REMOVE_TREE_FAIL') and str(r/'Library/Extensions/NVAccel.kext') in a:sys.exit(9)
 if os.environ.get('FAKE_TERM_AFTER_NVRM') and str(r/'Library/Extensions/NVRM.kext') in a:
  import signal,subprocess;subprocess.run(['/bin/rm']+a);os.kill(os.getppid(),signal.SIGTERM);sys.exit(0)
 os.execv('/bin/rm',['rm']+a)
elif name=='nvram':
 p=r/'remove-flag'
 if a[0]=='-d':
  if os.environ.get('FAKE_DELETE_FAIL'):sys.exit(2)
  p.unlink()
 elif not p.exists():sys.exit(1)
elif name in ['sleep','shutdown']:pass
else:sys.exit(99)
'''

class Recovery(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name);self.bin=self.root/'bin';self.bin.mkdir()
        for name in ['id','sw_vers','kmutil','nvram','sleep','shutdown','rm']:
            p=self.bin/name;p.write_text('#!'+sys.executable+'\n'+MOCK);p.chmod(0o755)
        self.env=dict(os.environ,PATH=str(self.bin)+':/usr/bin:/bin:/usr/sbin:/sbin',FIXTURE_ROOT=str(self.root))
        self.ext=self.root/'Library/Extensions';self.ext.mkdir(parents=True)
        for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC','Other']:
            p=self.ext/(k+'.kext/Contents/MacOS')/k;p.parent.mkdir(parents=True);p.write_bytes(b'fixture')
        self.kc=self.root/'Library/KernelCollections/AuxiliaryKernelExtensions.kc';self.kc.parent.mkdir(parents=True);self.kc.write_bytes(b'LIVE_WITH_NV')
        self.version=self.root/'Library/NullMoth/driver-version';self.version.parent.mkdir(parents=True);self.version.write_text('1.0.7')
        self.backup=self.root/'backup';self.backup.mkdir();(self.backup/'AuxiliaryKernelExtensions.kc').write_bytes(b'PREVIOUS_SAFE')
        (self.backup/'macos-build').write_text('24H32')
        source=(ROOT/'package/uninstall.sh').read_text()
        for prefix in ['/Library/','/System/']:source=source.replace(prefix,str(self.root)+prefix)
        self.script=self.root/'uninstall.sh';self.script.write_text(source)
    def tearDown(self):self.temp.cleanup()
    def run_remove(self,backup=None,**faults):
        return subprocess.run(['/bin/bash',str(self.script),str(self.backup if backup is None else backup)],env=dict(self.env,**faults),capture_output=True,text=True)
    def assert_new_collection(self,r):
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual(self.kc.read_bytes(),b'NEW26_NON_NV')
        self.assertFalse(self.version.exists())
    def test_cross_build_backup_is_never_restored(self):self.assert_new_collection(self.run_remove())
    def test_legacy_backup_without_build_record_is_not_restored(self):
        (self.backup/'macos-build').unlink();self.assert_new_collection(self.run_remove())
    def test_same_build_backup_cannot_replace_current_repository(self):
        (self.backup/'macos-build').write_text('25G241');self.assert_new_collection(self.run_remove())
    def test_rebuild_repository_preserves_other_drivers(self):
        self.assert_new_collection(self.run_remove())
        calls=[json.loads(l) for l in (self.root/'calls.jsonl').read_text().splitlines()]
        create=next(c for c in calls if c[:2]==['kmutil','create'])
        self.assertIn('--repository',create)
        self.assertTrue((self.ext/'Other.kext/Contents/MacOS/Other').exists())
    def test_new_build_failure_does_not_publish_partial_collection(self):
        r=self.run_remove(FAKE_CREATE_FAIL='1');self.assertNotEqual(r.returncode,0);self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
        self.assertTrue((self.ext/'NVRM.kext').exists());self.assertTrue((self.ext/'NVAccel.kext').exists())
    def test_new_inspection_failure_does_not_publish(self):
        r=self.run_remove(FAKE_INSPECT_FAIL='1');self.assertNotEqual(r.returncode,0);self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
    def test_empty_output_does_not_publish(self):
        r=self.run_remove(FAKE_EMPTY='1');self.assertNotEqual(r.returncode,0);self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
    def test_retained_driver_is_not_published(self):
        r=self.run_remove(FAKE_NV_RETAINED='1');self.assertNotEqual(r.returncode,0);self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
    def test_no_other_kexts_removes_only_known_collection_files(self):
        import shutil
        shutil.rmtree(self.ext/'Other.kext');sidecar=Path(str(self.kc)+'.unrelated-backup');sidecar.write_bytes(b'keep')
        r=self.run_remove();self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertFalse(self.kc.exists());self.assertEqual(sidecar.read_bytes(),b'keep')
    def test_invalid_backup_stops_before_removing_files(self):
        r=self.run_remove(backup=self.root/'missing');self.assertNotEqual(r.returncode,0);self.assertTrue((self.ext/'NVAccel.kext').exists())
    def test_failed_tree_removal_is_not_reported_as_success(self):
        r=self.run_remove(FAKE_REMOVE_TREE_FAIL='1');self.assertNotEqual(r.returncode,0)
        self.assertTrue((self.ext/'NVAccel.kext').exists());self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
        self.assertEqual(self.version.read_text(),'1.0.7')
    def test_term_signal_mid_removal_restores_previous_files(self):
        r=self.run_remove(FAKE_TERM_AFTER_NVRM='1');self.assertNotEqual(r.returncode,0);self.assertIn('signal TERM',r.stderr)
        self.assertTrue((self.ext/'NVRM.kext/Contents/MacOS/NVRM').exists());self.assertEqual(self.kc.read_bytes(),b'LIVE_WITH_NV')
        self.assertEqual(self.version.read_text(),'1.0.7');self.assertFalse(Path(str(self.kc)+'.nullmoth-remove-new').exists())
    def test_final_inspection_failure_is_not_reported_as_success(self):
        r=self.run_remove(FAKE_FINAL_INSPECT_FAIL='1');self.assertNotEqual(r.returncode,0)
        self.assertIn('cannot verify',r.stderr)
        self.assertEqual(self.version.read_text(),'1.0.7')
    def recovery(self,**faults):
        source=(ROOT/'app/Resources/nullmoth-setup.sh').read_text();start="cat > \"$ST/nullmoth-recover.sh\" <<'RS'\n";end='\nRS\n'
        assert source.count(start)==1;body=source.split(start,1)[1].split(end,1)[0].replace('PATH=/usr/bin:/bin:/usr/sbin:/sbin','')
        body=body.replace('/Library/',str(self.root)+'/Library/').replace('/sbin/shutdown',str(self.bin/'shutdown'))
        helper=self.root/'Library/NullMoth/nullmoth-setup.sh';helper.parent.mkdir(parents=True,exist_ok=True)
        helper.write_text('#!/bin/bash\n[ "${FAKE_REMOVE_FAIL:-0}" = 1 ] && exit 8\necho "RESULT ok"\n');helper.chmod(0o755)
        (self.root/'remove-flag').write_text('pending');script=self.root/'recover.sh';script.write_text(body)
        r=subprocess.run(['/bin/bash',str(script)],env=dict(self.env,**faults),capture_output=True,text=True)
        calls=[json.loads(l) for l in (self.root/'calls.jsonl').read_text().splitlines()]
        return r,calls
    def test_failed_recovery_keeps_trigger_and_does_not_reboot(self):
        r,c=self.recovery(FAKE_REMOVE_FAIL='1');self.assertNotEqual(r.returncode,0);self.assertTrue((self.root/'remove-flag').exists());self.assertFalse(any(x[0]=='shutdown' for x in c))
    def test_successful_recovery_clears_trigger_then_reboots_once(self):
        r,c=self.recovery();self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertFalse((self.root/'remove-flag').exists());self.assertEqual(sum(x[0]=='shutdown' for x in c),1)
    def test_failed_trigger_clear_does_not_reboot(self):
        r,c=self.recovery(FAKE_DELETE_FAIL='1');self.assertNotEqual(r.returncode,0);self.assertTrue((self.root/'remove-flag').exists());self.assertFalse(any(x[0]=='shutdown' for x in c))

if __name__=='__main__':unittest.main(verbosity=2)
