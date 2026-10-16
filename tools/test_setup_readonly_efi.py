#!/usr/bin/env python3
"""A config backup that fails because macOS mounted the OpenCore partition read-only says so and what to do, not only
"could not back up". Runs the setup script's own ro_why with stand-in df and mount; a writable mount adds nothing."""
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'app/Resources/nullmoth-setup.sh').read_text()
start = src.index('ro_why() {')
end = src.index('\n', src.index('then try again"; }', start)) + 1
snippet = src[start:end]
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


def run(flags):
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        (d / 'bin').mkdir()
        (d / 'bin/df').write_text('#!/bin/bash\necho "Filesystem 512-blocks Used Available Capacity Mounted on"\n'
                                  'echo "/dev/disk0s1 409600 1000 408600 1% /Volumes/EFI"\n')
        (d / 'bin/mount').write_text(f'#!/bin/bash\necho "/dev/disk3s1 on / (apfs, sealed, local, read-only, journaled)"\n'
                                     f'echo "/dev/disk0s1 on /Volumes/EFI ({flags})"\n')
        for f in ('df', 'mount'):
            os.chmod(d / 'bin' / f, 0o755)
        script = snippet + 'echo "[could not back up X$(ro_why /Volumes/EFI/EFI/OC/config.plist)]"\n'
        r = subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True,
                           env=dict(os.environ, PATH=f'{d}/bin:/usr/bin:/bin'), timeout=30)
        return r.stdout + r.stderr


out = run('msdos, local, nodev, nosuid, read-only, noowners')
check('a read-only OpenCore partition names the cause and First Aid', 'mounted /Volumes/EFI read-only' in out
      and 'First Aid' in out and 'Fast Startup' in out, out.strip()[-160:])
out = run('msdos, local, nodev, nosuid, noowners')
check('negative control: a writable partition adds nothing', out.strip() == '[could not back up X]', out.strip())
check('every backup stop carries the reason', src.count('stop "could not back up $C$(ro_why "$C")"') == 6
      and 'stop "could not back up $C"' not in src)
sys.exit(1 if fails else 0)
