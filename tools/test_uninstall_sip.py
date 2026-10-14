#!/usr/bin/env python3
"""The uninstaller stops before touching anything when SIP's filesystem protection is on (removal used to fail half-way
with "Operation not permitted" on every bundle), and goes on when it is off. Runs the real script with stand-in id,
csrutil and sw_vers; an unreadable build number makes the SIP-off arm stop right after the check, before any file work."""
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
scripts = [root / 'package/uninstall.sh', root / 'app/Resources/nullmoth-uninstall.sh']
SIP_ON = 'System Integrity Protection status: enabled.'
CUSTOM_FS_ON = ('System Integrity Protection status: unknown (Custom Configuration).\n\nConfiguration:\n'
                '\tKext Signing: disabled\n\tFilesystem Protections: enabled\n')
CUSTOM_OFF = ('System Integrity Protection status: unknown (Custom Configuration).\n\nConfiguration:\n'
              '\tKext Signing: disabled\n\tFilesystem Protections: disabled\n')
fails = 0


def run(script, csr):
    with tempfile.TemporaryDirectory() as d:
        b = pathlib.Path(d)
        for name, body in (('id', 'echo 0'), ('sw_vers', 'echo not-a-build'), ('csrutil', 'cat "$CSR_FILE"'),
                           ('kmutil', 'echo kmutil-called >> "$CALLS"; exit 1')):
            (b / name).write_text(f'#!/bin/sh\n{body}\n'); os.chmod(b / name, 0o755)
        (b / 'csr.txt').write_text(csr)
        env = dict(os.environ, PATH=f'{b}:/usr/bin:/bin', CSR_FILE=str(b / 'csr.txt'), CALLS=str(b / 'calls'))
        # package/uninstall.sh fixes PATH for root; the stand-in commands only take effect on a copy without that line
        body = script.read_text().replace('PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH', '')
        (b / 'under-test.sh').write_text(body)
        r = subprocess.run(['/bin/bash', str(b / 'under-test.sh')], capture_output=True, text=True, env=env, timeout=60)
        return r.returncode, r.stderr, (b / 'calls').exists()


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += not ok


for s in scripts:
    for label, csr in (('SIP on', SIP_ON), ('custom SIP with filesystem protection on', CUSTOM_FS_ON)):
        rc, err, kmutil = run(s, csr)
        check(f'{s.name}: {label} stops before any change', rc == 1 and 'System Integrity Protection is on' in err
              and 'Nothing was changed' in err and not kmutil)
    rc, err, kmutil = run(s, CUSTOM_OFF)
    check(f'{s.name}: filesystem protection off passes the check', rc == 1 and 'cannot identify the current macOS build' in err
          and 'System Integrity Protection is on' not in err)
sys.exit(1 if fails else 0)
