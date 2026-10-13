#!/usr/bin/env python3
"""The macOS update helper: its launchd file must be valid, and a Mac whose update was never finished must be finished
by the next setup run on the new macOS (and only then)."""
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
src = (root / 'app/Resources/nullmoth-setup.sh').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += not ok


with tempfile.TemporaryDirectory() as d:
    d = pathlib.Path(d)
    # 1. the launchd file setup writes, with $ST filled in, must be a plist launchd can read
    start = src.index('cat > /Library/LaunchDaemons/com.nullmoth.osupdate.plist <<PL\n')
    body = src[src.index('\n', start) + 1:src.index('\nPL\n', start)].replace('$ST', '/Library/NullMoth')
    (d / 'helper.plist').write_text(body)
    lint = subprocess.run(['plutil', '-lint', str(d / 'helper.plist')], capture_output=True).returncode == 0
    check('the update helper plist is valid', lint)
    if lint:
        cmd = subprocess.run(['plutil', '-extract', 'ProgramArguments.2', 'raw', str(d / 'helper.plist')],
                             capture_output=True, text=True).stdout.strip()
        check('it runs finish and logs both streams', cmd.endswith('--update finish >> /Library/NullMoth/osupdate.log 2>&1'))

    # 2. the resume block, run with a stand-in sw_vers
    a = src.index('if [ -z "$UPD" ] && [ $DRY = 0 ] && [ -f "$ST/update-pending" ]; then')
    b = src.index('\nfi\n', src.index('    UPD=finish', a)) + 4
    block = src[a:b]
    shim = d / 'bin'
    shim.mkdir()

    def run(from_major, now, upd='', dry=0):
        st = d / f'st-{from_major}-{now}-{upd}-{dry}'
        st.mkdir()
        if from_major is not None:
            (st / 'update-pending').write_text(f'FROM={from_major}\nBAR0=13\nABAR0=-1\n')
        (shim / 'sw_vers').write_text(f'#!/bin/sh\necho {now}\n')
        os.chmod(shim / 'sw_vers', 0o755)
        script = f'note() {{ :; }}; ST={st}; UPD="{upd}"; DRY={dry}\n{block}\necho "UPD=$UPD"\n'
        out = subprocess.run(['/bin/bash', '-c', script], capture_output=True, text=True,
                             env=dict(os.environ, PATH=f'{shim}:/usr/bin:/bin')).stdout
        return out.strip().splitlines()[-1]

    check('updated 15 -> 26 and never finished: finishes', run('15', '26.7.1') == 'UPD=finish')
    check('still on 15 waiting for the update: nothing', run('15', '15.8.1') == 'UPD=')
    check('no pending update: nothing', run(None, '26.7.1') == 'UPD=')
    check('a dry run changes nothing', run('15', '26.7.1', dry=1) == 'UPD=')
    check('an explicit --update prepare is kept', run('15', '26.7.1', upd='prepare') == 'UPD=prepare')

sys.exit(1 if fails else 0)
