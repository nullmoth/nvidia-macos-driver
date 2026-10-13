#!/usr/bin/env python3
"""Execute the package installer with mocked system commands inside a private filesystem."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import struct
import subprocess
import sys
import tempfile
import unittest

REPO=Path(__file__).resolve().parents[1]
KEXTS=['NVRM','NVAccel','NVRMFB','NVRMAGDC']
MOCK=r'''
import json,os,sys
from pathlib import Path
name=Path(sys.argv[0]).name;a=sys.argv[1:];r=Path(os.environ['FIXTURE_ROOT'])
with (r/'calls.jsonl').open('a') as f:f.write(json.dumps([name]+a)+'\n')
if name=='id':print('0')
elif name=='uname':print('x86_64')
elif name=='sw_vers':print('25G241' if a==['-buildVersion'] else os.environ.get('FAKE_OS_VERSION','15.8.1'))
elif name=='ioreg':print('+-o GFX0@0  <class IOPCIDevice>\n    {\n      "device-id" = <'+os.environ.get('FAKE_NV_DEVICE','052d0000')+'>\n      "vendor-id" = <de100000>\n    }')
elif name=='nvram':print('boot-args\tnvfb=1 nvaccel=1')
elif name=='stat':
 p=Path(a[-1]);info=p.lstat()
 if a[1]=='%u %Lp':print(os.environ.get('FAKE_INSTALLER_OWNER','0')+' '+format(info.st_mode&0o7777,'o'))
 elif a[1]=='%d:%i':print(str(info.st_dev)+':'+str(info.st_ino))
 else:sys.exit(98)

elif name=='chown':pass
elif name=='sudo':
 # sudo -u _windowserver /bin/test -r FILE: WindowServer's user is "other" to a root:wheel file
 sys.exit(0 if a[:2]==['-u','_windowserver'] and Path(a[-1]).stat().st_mode&0o004 else 1)
elif name=='mktemp':
 counter=r/'temps';n=int(counter.read_text())+1 if counter.exists() else 1;counter.write_text(str(n))
 p=r/('scratch-'+str(n));p.mkdir();print(str(p))
elif name=='kmutil':
 p=Path(a[a.index('-A')+1])
 if a[0]=='create':
  counter=r/'creates';n=int(counter.read_text())+1 if counter.exists() else 1;counter.write_text(str(n))
  if (n==1 and os.environ.get('FAKE_PREFLIGHT_FAIL')) or (n==2 and os.environ.get('FAKE_LIVE_FAIL')):sys.exit(9)
  p.write_bytes(b'NEW26-AUX')
 elif a[0]=='inspect':
  if not p.is_file():sys.exit(7)
  live='nullmoth-install-new' in str(p)
  if live and os.environ.get('FAKE_INSPECT_FAIL'):sys.exit(8)
  for k in ['NVRM','NVAccel','NVRMFB','NVRMAGDC']:
   if not live or k!=os.environ.get('FAKE_MISSING_ID'):print('com.nullmoth.'+k+' 0.1')
else:sys.exit(99)
'''

class Install(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.guard_temp=tempfile.TemporaryDirectory()
        cls.guard=Path(cls.guard_temp.name)/'runtime-check'
        subprocess.run(['xcrun','clang','-O2','-Wall','-Wextra','-Werror','-target','x86_64-apple-macos15.0',str(REPO/'app/RuntimeCheck/main.c'),'-o',str(cls.guard)],check=True)
    @classmethod
    def tearDownClass(cls):cls.guard_temp.cleanup()
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='nullmoth-tahoe-install-');self.root=Path(self.temp.name)
        self.payload=self.root/'payload';self.payload.mkdir()
        for major in (15,26):
            b=self.payload/f'Library/NullMoth/kexts/{major}/NVAccel.kext/Contents/MacOS/NVAccel';b.parent.mkdir(parents=True)
            b.write_bytes(struct.pack('<8I',0xfeedfacf,0x01000007,3,11,0,0,0,0)+bytes([major]))
            (b.parent.parent/'Info.plist').write_bytes(plistlib.dumps({'CFBundleIdentifier':'com.nullmoth.NVAccel','NMTargetOSMajor':major}))
        for k in KEXTS:
            b=self.payload/f'Library/Extensions/{k}.kext/Contents/MacOS/{k}';b.parent.mkdir(parents=True);b.write_bytes(('new-'+k).encode())
        for b in ['NVMTLDriver.bundle','NVIDIAShared.bundle','nvmtl']:
            p=self.payload/'Library/GPUBundles'/b;p.mkdir(parents=True);(p/'fixture').write_text('new-'+b)
        (self.payload/'Library/GPUBundles/nvmtl-allow.txt').write_text('new-allow')
        load=struct.pack('<6I',0x32,24,1,0x000f0500,0x000f0500,0)
        runtime=self.payload/'Library/GPUBundles/nvmtl/libvulkan_nouveau.dylib'
        runtime.write_bytes(struct.pack('<8I',0xfeedfacf,0x01000007,3,6,1,len(load),0,0)+load)
        for folder in [self.root,self.payload]:
            (folder/'nullmoth-runtime-check').write_bytes(self.guard.read_bytes())
            (folder/'nullmoth-runtime-check').chmod(0o755)
            helper=folder/'nullmoth-runtime-check.sh'
            helper.write_bytes((REPO/'app/Resources/nullmoth-runtime-check.sh').read_bytes());helper.chmod(0o755)

        fw=self.payload/'Users/Shared/nvfw';fw.mkdir(parents=True);(fw/'fixture').write_text('new-firmware')
        lines=[hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(self.payload)) for p in self.payload.rglob('*') if p.is_file()]
        (self.payload/'SHA256SUMS').write_text('\n'.join(lines)+'\n')
        self.old={}
        for k in KEXTS:
            p=self.root/f'Library/Extensions/{k}.kext/Contents/MacOS/{k}';p.parent.mkdir(parents=True);p.write_text('old-'+k);self.old[p]=p.read_bytes()
        for b in ['NVMTLDriver.bundle','NVIDIAShared.bundle','nvmtl']:
            p=self.root/'Library/GPUBundles'/b/'fixture';p.parent.mkdir(parents=True);p.write_text('old-'+b);self.old[p]=p.read_bytes()
        for rel,value in [('Library/GPUBundles/nvmtl-allow.txt','old-allow'),('Users/Shared/nvfw/fixture','old-firmware'),('Library/NullMoth/kexts/old-marker','old-cache'),('Library/NullMoth/os-major','15'),('Library/KernelCollections/AuxiliaryKernelExtensions.kc','OLD15-AUX')]:
            p=self.root/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(value);self.old[p]=p.read_bytes()
        other=self.root/'Library/Extensions/NVMeFix.kext/fixture';other.parent.mkdir();other.write_text('unrelated');other.chmod(0o400);self.other=other
        self.bin=self.root/'bin';self.bin.mkdir()
        for name in ['id','uname','sw_vers','ioreg','nvram','chown','mktemp','kmutil','stat','sudo']:
            p=self.bin/name;p.write_text('#!'+sys.executable+'\n'+MOCK);p.chmod(0o755)
        source=(REPO/'package/install.sh').read_text()
        # Every installed path points into the private fixture. Real kmutil/ioreg/id are shadowed.
        for prefix in ['Library/','Users/Shared/']:
            source=source.replace('$HERE/'+prefix,'$HERE/__PAYLOAD_'+prefix.replace('/','_')+'__')
        for prefix in ['/Library/','/System/','/Users/Shared/']:source=source.replace(prefix,str(self.root)+prefix)
        for prefix in ['Library/','Users/Shared/']:
            source=source.replace('$HERE/__PAYLOAD_'+prefix.replace('/','_')+'__','$HERE/'+prefix)
        source=source.replace('require_install_directory /Library\n','require_install_directory '+str(self.root/'Library')+'\n')
        self.script=self.payload/'install.sh';self.script.write_text(source)
        self.env=dict(os.environ,FIXTURE_ROOT=str(self.root),PATH=str(self.bin)+':/usr/bin:/bin:/usr/sbin:/sbin')
    def tearDown(self):self.temp.cleanup()
    def run_install(self,**faults):return subprocess.run(['/bin/bash',str(self.script)],env=dict(self.env,**faults),capture_output=True,text=True)
    def assert_restored(self,result):
        self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
        for p,data in self.old.items():self.assertEqual(p.read_bytes(),data,str(p))
        self.assertEqual(self.other.stat().st_mode&0o777,0o400)
    def assert_creates(self,count):
        self.assertEqual(int((self.root/'creates').read_text()),count)
    def test_unqualified_os_is_refused_before_collection_or_backup(self):
        r=self.run_install(FAKE_OS_VERSION='27.0')
        self.assertNotEqual(r.returncode,0)
        self.assertIn('built for macOS 15 and 26',r.stdout+r.stderr)
        self.assertFalse((self.root/'creates').exists())
        self.assertFalse(list((self.root/'Library/NullMoth').glob('backup-*')))
        for path,expected in self.old.items():self.assertEqual(path.read_bytes(),expected)

    def test_pre_turing_card_is_refused_before_any_change(self):
        # GTX 1060 (10DE:1C03): no GSP firmware, so the driver cannot run it; installing would switch off the
        # firmware screen it is running on
        r=self.run_install(FAKE_NV_DEVICE='031c0000')
        self.assertNotEqual(r.returncode,0)
        self.assertIn('older than the driver supports',r.stdout+r.stderr)
        self.assertFalse((self.root/'creates').exists())
        for path,expected in self.old.items():self.assertEqual(path.read_bytes(),expected)

    def test_turing_laptop_t500_is_accepted(self):
        # Quadro T500 laptop (10DE:1FBB, TU117, PCI class 03:02): NVIDIA's 610.57.04 table names it "NVIDIA T500"
        r=self.run_install(FAKE_NV_DEVICE='bb1f0000');self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('older than the driver supports',r.stdout+r.stderr)

    def test_older_runtime_host_is_refused_before_collection_or_backup(self):
        r=self.run_install(FAKE_OS_VERSION='15.4.9')
        self.assertNotEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertIn('requires macOS 15.5.0',r.stdout+r.stderr)
        self.assertFalse((self.root/'creates').exists())
        for path,value in self.old.items():self.assertEqual(path.read_bytes(),value)
    def test_preflight_failure_does_not_modify_live_files(self):
        self.assert_restored(self.run_install(FAKE_PREFLIGHT_FAIL='1'));self.assert_creates(1)
    def test_live_build_failure_restores_all_previous_files(self):
        self.assert_restored(self.run_install(FAKE_LIVE_FAIL='1'));self.assert_creates(2)
    def test_inspection_failure_restores_previous_files(self):
        self.assert_restored(self.run_install(FAKE_INSPECT_FAIL='1'));self.assert_creates(2)
    def test_exact_nvrm_identifier_is_required(self):
        self.assert_restored(self.run_install(FAKE_MISSING_ID='NVRM'));self.assert_creates(2)
    def test_success_publishes_new_collection_and_manifests(self):
        r=self.run_install();self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.root/'Library/KernelCollections/AuxiliaryKernelExtensions.kc').read_bytes(),b'NEW26-AUX')
        for major in (15,26):
            d=self.root/f'Library/NullMoth/kexts/{major}'
            subprocess.run(['shasum','-a','256','-c','SHA256SUMS','--quiet'],cwd=d,check=True)
        calls=[json.loads(l) for l in (self.root/'calls.jsonl').read_text().splitlines()]
        create=[c for c in calls if c[:2]==['kmutil','create']][-1]
        self.assertEqual(create[create.index('--repository')+1],str(self.root/'Library/Extensions'))
        self.assertEqual(self.other.stat().st_mode&0o777,0o400)
    def test_allow_list_is_readable_by_windowserver_even_from_a_private_copy(self):
        # the privileged helper's umask 077 left the copy at 0600: WindowServer got no Metal device and aborted
        (self.payload/'Library/GPUBundles/nvmtl-allow.txt').chmod(0o600)
        r=self.run_install();self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((self.root/'Library/GPUBundles/nvmtl-allow.txt').stat().st_mode&0o777,0o644)
    def test_external_audited_installer_keeps_archived_payload_unchanged(self):
        audited=self.root/'audited-install.sh';audited.write_bytes(self.script.read_bytes())
        self.script.write_text('#!/bin/bash\necho ARCHIVED_INSTALLER_EXECUTED\nexit 99\n')
        lines=[hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(self.payload)) for p in self.payload.rglob('*') if p.is_file() and p.name!='SHA256SUMS']
        (self.payload/'SHA256SUMS').write_text('\n'.join(lines)+'\n')
        manifest=(self.payload/'SHA256SUMS').read_bytes()
        r=subprocess.run(['/bin/bash',str(audited),'--payload',str(self.payload)],env=self.env,capture_output=True,text=True)
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertNotIn('ARCHIVED_INSTALLER_EXECUTED',r.stdout)
        self.assertEqual((self.payload/'SHA256SUMS').read_bytes(),manifest)
        subprocess.run(['shasum','-a','256','-c','SHA256SUMS','--quiet'],cwd=self.payload,check=True)

if __name__=='__main__':unittest.main(verbosity=2)
