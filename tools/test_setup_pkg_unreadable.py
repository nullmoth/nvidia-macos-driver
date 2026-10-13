#!/usr/bin/env python3
"""A driver package macOS will not let the installer read (the app on an external drive: "Operation not permitted")
stops with what to do, not "checksum does not match - download it again". Runs the setup script's own package-copy
lines with a stand-in cp; another copy failure keeps its own message, and a readable package is copied."""
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'app/Resources/nullmoth-setup.sh').read_text()
start = src.index('PKGDIR=$(mktemp -d /var/tmp/nullmoth-pkg.XXXX)')
end = src.index('PKG=$PKGDIR/$(basename "$PKG")', start)
snippet = src[start:end].replace('/var/tmp/nullmoth-pkg.XXXX', '"$TEST_TMP/nullmoth-pkg.XXXX"')
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


def run(cp_body):
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        (d / 'bin').mkdir()
        (d / 'bin/cp').write_text('#!/bin/bash\n' + cp_body + '\n')
        os.chmod(d / 'bin/cp', 0o755)
        pkg = d / 'nullmoth-nvidia-2.0.0.tar.gz'
        pkg.write_bytes(b'x')
        script = 'stop() { echo "STOP $*"; exit 1; }\nPKG=' + str(pkg) + '\n' + snippet + '\necho COPIED\n'
        r = subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True,
                           env=dict(os.environ, PATH=f'{d}/bin:/usr/bin:/bin', TEST_TMP=str(d)), timeout=60)
        return r.stdout + r.stderr


out = run('echo "cp: $1: Operation not permitted" >&2; exit 1')
check('a package macOS will not let us read says to move the app', 'Move the 1401 app to the Applications folder' in out
      and 'download it again' not in out, out.strip()[-160:])
out = run('echo "cp: disk full" >&2; exit 1')
check('another copy failure keeps its own reason', 'could not copy the package for checking: cp: disk full' in out, out.strip()[-120:])
out = run('exec /bin/cp "$@"')
check('a readable package is copied', 'COPIED' in out, out.strip()[-80:])
sys.exit(1 if fails else 0)
