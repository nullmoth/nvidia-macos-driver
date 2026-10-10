from pathlib import Path
import importlib.util,os,subprocess,unittest,time,json,hashlib
R=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('installer_fixture',R/'tools/test_support_install.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
class Backup(m.Install):
 def setUp(self):
  super().setUp()
  # Use the real system mktemp with templates rewritten to this private filesystem.
  source=self.script.read_text().replace('/var/tmp/',str(self.root/'tmp')+'/');(self.root/'tmp').mkdir();self.script.write_text(source)
  (self.bin/'mktemp').unlink()
  p=self.bin/'date';p.write_text('#!/bin/bash\necho 20261008-000000\n');p.chmod(0o755)
 def calls(self):return [json.loads(l) for l in (self.root/'calls.jsonl').read_text().splitlines()]
 def backup_paths(self):return sorted((self.root/'Library/NullMoth').glob('backup-*'))
 def test_locked_install_refuses_before_preflight_without_deleting_guard(self):
  guard=self.root/'Library/NullMoth/install.lock';guard.mkdir();(guard/'foreign').write_bytes(b'guard-owner')
  r=self.run_install();self.assert_restored(r);self.assertIn('installer guard already exists',r.stderr);self.assertEqual((guard/'foreign').read_bytes(),b'guard-owner');self.assertFalse((self.root/'creates').exists());self.assertFalse(self.backup_paths())
 def test_check_mode_does_not_acquire_or_delete_existing_guard(self):
  guard=self.root/'Library/NullMoth/install.lock';guard.mkdir();(guard/'foreign').write_bytes(b'guard-owner')
  r=self.run_install(CHECK='1');self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertEqual((guard/'foreign').read_bytes(),b'guard-owner');self.assertFalse(self.backup_paths());self.assert_creates(1)
 def test_backup_copy_failure_is_bounded_and_before_install(self):
  wrapper=self.bin/'ditto';wrapper.write_text('#!/bin/bash\ncase "$1 $2" in *GPUBundles/nvmtl\ *NullMoth/backup-*) /usr/bin/printf "%20000s" "" >&2; echo "controlled copy error" >&2; exit 73;; esac\nexec /usr/bin/ditto "$@"\n');wrapper.chmod(0o755)
  r=self.run_install();self.assert_restored(r);self.assertIn('command-status=73',r.stderr);self.assertIn('controlled copy error',r.stderr);self.assertLess(len(r.stderr.encode()),4400);self.assertIn('STOP: back up nvmtl',r.stderr);self.assert_creates(1);self.assertEqual(len(self.backup_paths()),1);self.assertFalse((self.root/'Library/NullMoth/install.lock').exists())
 def test_same_second_runs_have_distinct_immutable_backup_sets(self):
  old=self.root/'Library/GPUBundles/nvmtl/obsolete';old.write_bytes(b'old-only')
  first=self.run_install();self.assertEqual(first.returncode,0,first.stdout+first.stderr);one=self.backup_paths();self.assertEqual(len(one),1);first_snapshot={p.relative_to(one[0]).as_posix():p.read_bytes() for p in one[0].rglob('*') if p.is_file()}
  # The installed replacement no longer contains obsolete. A new recovery copy must not inherit it.
  self.assertFalse(old.exists());second=self.run_install();self.assertEqual(second.returncode,0,second.stdout+second.stderr);two=self.backup_paths();self.assertEqual(len(two),2)
  other=next(p for p in two if p!=one[0]);self.assertFalse((other/'nvmtl/obsolete').exists());self.assertEqual(first_snapshot,{p.relative_to(one[0]).as_posix():p.read_bytes() for p in one[0].rglob('*') if p.is_file()})
 def test_overlapping_install_is_refused_and_first_retains_guard(self):
  helper=self.bin/'kmutil';source=helper.read_text();prefix='''import time
if os.environ.get('FAKE_HOLD') and name=='kmutil' and a[0]=='create' and not (r/'entered').exists():
 (r/'entered').write_text('ready')
 until=time.monotonic()+10
 while not (r/'release').exists():
  if time.monotonic()>until:sys.exit(88)
  time.sleep(0.02)
''';source=source.replace("if name=='id':",prefix+"if name=='id':",1);helper.write_text(source)
  first=subprocess.Popen(['/bin/bash',str(self.script)],env=dict(self.env,FAKE_HOLD='1'),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
  try:
   until=time.monotonic()+5
   while not (self.root/'entered').exists():
    self.assertIsNone(first.poll());self.assertLess(time.monotonic(),until);time.sleep(.02)
   second=self.run_install();self.assert_restored(second);self.assertIn('installer guard already exists',second.stderr);self.assertTrue((self.root/'Library/NullMoth/install.lock').is_dir());self.assertFalse(self.backup_paths())
  finally:
   (self.root/'release').write_text('continue');out,err=first.communicate(timeout=15)
  self.assertEqual(first.returncode,0,out+err);self.assertFalse((self.root/'Library/NullMoth/install.lock').exists());self.assertEqual(len(self.backup_paths()),1)
 def test_failed_live_install_removes_owned_guard_and_retains_complete_backup(self):
  r=self.run_install(FAKE_LIVE_FAIL='1');self.assert_restored(r);self.assertFalse((self.root/'Library/NullMoth/install.lock').exists());backups=self.backup_paths();self.assertEqual(len(backups),1)
  for original,contents in self.old.items():
   rel=original.relative_to(self.root).as_posix()
   if rel.startswith('Library/Extensions/'):saved=backups[0]/rel.removeprefix('Library/Extensions/')
   elif rel.startswith('Library/GPUBundles/'):saved=backups[0]/rel.removeprefix('Library/GPUBundles/')
   elif rel.startswith('Library/NullMoth/'):saved=backups[0]/rel.removeprefix('Library/NullMoth/')
   elif rel.startswith('Users/Shared/nvfw/'):saved=backups[0]/'nvfw'/rel.removeprefix('Users/Shared/nvfw/')
   elif rel.endswith('AuxiliaryKernelExtensions.kc'):saved=backups[0]/'AuxiliaryKernelExtensions.kc'
   else:continue
   self.assertEqual(saved.read_bytes(),contents)
 def test_incomplete_rollback_retains_guard_for_recovery_review(self):
  wrapper=self.bin/'ditto';wrapper.write_text('#!/bin/bash\ncase "$1" in *backup-*/NVRM.kext) exit 74;; esac\nexec /usr/bin/ditto "$@"\n');wrapper.chmod(0o755)
  r=self.run_install(FAKE_LIVE_FAIL='1');self.assertNotEqual(r.returncode,0);self.assertIn('Restore incomplete',r.stderr);self.assertIn('installer guard retained',r.stderr);self.assertTrue((self.root/'Library/NullMoth/install.lock').is_dir());self.assertEqual(len(self.backup_paths()),1)
 def test_library_parent_symlink_is_refused_and_preserved(self):
  original=self.root/'Library';foreign=self.root/'foreign-library';original.rename(foreign);original.symlink_to(foreign,target_is_directory=True)
  r=self.run_install();self.assert_restored(r);self.assertIn('real directory',r.stderr);self.assertTrue(original.is_symlink());self.assertFalse((self.root/'creates').exists())
 def test_state_parent_symlink_is_refused_and_preserved(self):
  original=self.root/'Library/NullMoth';foreign=self.root/'foreign-state';original.rename(foreign);original.symlink_to(foreign,target_is_directory=True)
  r=self.run_install();self.assert_restored(r);self.assertIn('real directory',r.stderr);self.assertTrue(original.is_symlink());self.assertFalse((self.root/'creates').exists())
 def test_writable_parent_is_refused_without_permission_repair(self):
  parent=self.root/'Library/NullMoth';parent.chmod(0o777)
  r=self.run_install();self.assert_restored(r);self.assertIn('writable',r.stderr);self.assertEqual(parent.stat().st_mode&0o777,0o777);self.assertFalse((self.root/'creates').exists())
 def test_wrong_parent_owner_is_refused_without_changes(self):
  r=self.run_install(FAKE_INSTALLER_OWNER='501');self.assert_restored(r);self.assertIn('root-owned',r.stderr);self.assertFalse((self.root/'creates').exists())
 def test_replaced_guard_identity_is_preserved_on_exit(self):
  helper=self.bin/'kmutil';source=helper.read_text();inject="""if name=='kmutil' and a[0]=='create' and not (r/'replaced').exists():
 lock=r/'Library/NullMoth/install.lock';lock.rename(r/'original-owned-lock');lock.mkdir();(lock/'foreign').write_bytes(b'foreign-lock');(r/'replaced').write_text('done')
""";helper.write_text(source.replace("if name=='id':",inject+"if name=='id':",1))
  r=self.run_install();self.assertEqual(r.returncode,0,r.stdout+r.stderr);self.assertIn('guard identity changed',r.stderr);self.assertEqual((self.root/'Library/NullMoth/install.lock/foreign').read_bytes(),b'foreign-lock');self.assertTrue((self.root/'original-owned-lock').is_dir())
 def test_term_signal_after_first_component_copy_restores_previous_files(self):
  helper=self.bin/'ditto';helper.write_text('#!'+m.sys.executable+'\n'+"""import os,sys,subprocess,signal
from pathlib import Path
r=Path(os.environ['FIXTURE_ROOT']);a=sys.argv[1:]
ret=subprocess.run(['/usr/bin/ditto']+a).returncode
if ret:sys.exit(ret)
if a[0].endswith('/Library/Extensions/NVRM.kext') and '/payload/' in a[0] and not (r/'signalled').exists():
 (r/'signalled').write_text('done');os.kill(os.getppid(),signal.SIGTERM)
""");helper.chmod(0o755)
  r=self.run_install();self.assert_restored(r);self.assertIn('signal TERM',r.stderr);self.assertIn('Previous installation restored',r.stderr);self.assertFalse((self.root/'Library/NullMoth/install.lock').exists());self.assertEqual(len(self.backup_paths()),1)
if __name__=='__main__':unittest.main(verbosity=2)
