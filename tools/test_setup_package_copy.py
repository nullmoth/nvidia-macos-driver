#!/usr/bin/env python3
"""Root setup hashes and unpacks its own copy of the package, and reads the user's profile without root."""
import hashlib,os,subprocess,tempfile,unittest
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
SOURCE=(ROOT/'app/Resources/nullmoth-setup.sh').read_text()
def block(start,end):
    assert SOURCE.count(start)==1 and SOURCE.count(end)==1
    return start+SOURCE.split(start,1)[1].split(end,1)[0]+end+'\n'

class SetupPackageCopy(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)
        self.bin=self.root/'bin';self.bin.mkdir()
    def tearDown(self):self.temp.cleanup()
    def test_checked_package_cannot_be_swapped_afterwards(self):
        pkg=self.root/'user/nullmoth-nvidia-1.1.0.tar.gz';pkg.parent.mkdir();pkg.write_bytes(b'genuine')
        body=block('step "Checking the driver package"','ok "package matches its SHA-256"').replace('/var/tmp/',str(self.root)+'/')
        script=self.root/'check.sh'
        script.write_text('step(){ :; }; ok(){ :; }; stop(){ echo "STOP $*"; exit 1; }\nPKG=$1; SHA=$2\n'+body+'echo "$PKG"\n')
        r=subprocess.run(['/bin/bash',str(script),str(pkg),hashlib.sha256(b'genuine').hexdigest()],capture_output=True,text=True)
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        checked=Path(r.stdout.strip());pkg.write_bytes(b'swapped')
        self.assertNotEqual(checked,pkg);self.assertEqual(checked.name,pkg.name);self.assertEqual(checked.read_bytes(),b'genuine')
    def test_profile_is_read_as_the_console_user(self):
        for name,body in [('stat','echo fixture-user\n'),('sudo','echo "$*" >> "$(dirname "$0")/sudo-calls"; shift 2; exec "$@"\n')]:
            p=self.bin/name;p.write_text('#!/bin/bash\n'+body);p.chmod(0o755)
        profile=self.root/'profile.json';profile.write_text('{"model":"fixture"}');st=self.root/'state';st.mkdir()
        script=self.root/'profile.sh';script.write_text('PROFILE=$1; ST=$2\n'+block('if [ -n "$PROFILE" ] && [ -f "$PROFILE" ]; then','  rm -f "$PROFILE"\nfi'))
        r=subprocess.run(['/bin/bash',str(script),str(profile),str(st)],env=dict(os.environ,PATH=str(self.bin)+':/usr/bin:/bin'),capture_output=True,text=True)
        self.assertEqual(r.returncode,0,r.stdout+r.stderr)
        self.assertEqual((st/'system-profile.json').read_text(),'{"model":"fixture"}')
        self.assertIn('-u fixture-user cat '+str(profile),(self.bin/'sudo-calls').read_text());self.assertFalse(profile.exists())

if __name__=='__main__':unittest.main(verbosity=2)
