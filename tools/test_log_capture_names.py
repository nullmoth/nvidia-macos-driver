#!/usr/bin/env python3
"""Collected files whose names carry spaces or brackets (crash reports: "Google Chrome Helper (GPU)-....ips") are sent
under a safe name instead of being refused; a symlink is still refused. Ten 1.15 uploads said "A collected filename or
file type was refused" and arrived without those crash reports. Runs the real capture script with its root check and
root-only work folder swapped for test ones, and a stand-in collector."""
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'app/Resources/nullmoth-log-capture.sh').read_text()
swaps = [('[ "$(/usr/bin/id -u)" = 0 ]', 'true'), ('/private/var/root/1401-logs.XXXXXXXX', '"$TEST_TMP/1401-logs.XXXXXXXX"')]
for a, _ in swaps:
    if src.count(a) != 1:
        sys.exit(f'capture script changed shape ({a!r}) - check is UNKNOWN, not clean')
for a, b in swaps:
    src = src.replace(a, b)

COLLECTOR = '''#!/bin/bash
out="$2"; mkdir -p "$out"
echo "kernel lines" > "$out/driver-kernel-log.txt"
echo "a crash" > "$out/macos-Google Chrome Helper (GPU)-2026-10-13-120000.ips"
echo "secret" > "$out/../outside.txt"; ln -s ../outside.txt "$out/link.txt"
exit 0
'''
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    (d / 'bin').mkdir()
    (d / 'bin/nullmoth-log-capture.sh').write_text(src)
    (d / 'bin/nullmoth-log-watch').write_text(COLLECTOR)
    (d / 'bin/nullmoth-setup.sh').write_text('#!/bin/bash\n')
    for f in ('nullmoth-log-capture.sh', 'nullmoth-log-watch', 'nullmoth-setup.sh'):
        os.chmod(d / 'bin' / f, 0o755)
    (d / 'tmp').mkdir()
    out = subprocess.run(['/bin/bash', str(d / 'bin/nullmoth-log-capture.sh')], capture_output=True, text=True,
                         env=dict(os.environ, TEST_TMP=str(d / 'tmp')), timeout=60).stdout
names = [l.split(' ', 2)[1] for l in out.splitlines() if l.startswith('NMCOLLECT_FILE ')]
check('a crash report named with spaces and brackets is sent', 'macos-Google_Chrome_Helper__GPU_-2026-10-13-120000.ips' in names,
      ', '.join(names))
check('every sent name is safe', all(n and n[0].isalnum() and all(c.isalnum() or c in '._-' for c in n) for n in names))
import base64
check('a symlink is never sent or followed', 'link.txt' not in names and base64.b64encode(b'secret\n').decode() not in out)
check('nothing was refused', 'NMCOLLECT_WARNING invalid-file' not in out)
check('ordinary files still go', 'driver-kernel-log.txt' in names)
setup = (root / 'app/Resources/nullmoth-setup.sh').read_text()
body = setup[setup.index('if [ -n "$COLLECT" ]; then'):setup.index('echo "RESULT ok"; exit 0', setup.index('if [ -n "$COLLECT" ]; then'))]
first_query = body.index('lshow ')
for what, anchor in (('panics', '/Library/Logs/DiagnosticReports/*.panic'), ('WindowServer crash reports', 'DiagnosticReports/WindowServer*.ips'),
                     ('OpenCore logs and EFI configs', 'collect_recent_logs "$d" 3 "$mp"/opencore-*.txt')):
    check(f'{what} are copied before the first log query (the 360 s limit cut them off in 72 uploads)', body.index(anchor) < first_query)
check('the previous boot comes before the plugin and crash-window queries',
      body.index('previous-boot-kernel-log.txt') < body.index('driver-plugin-log.txt') < body.index('driver-crash-window.txt'))
sys.exit(1 if fails else 0)
