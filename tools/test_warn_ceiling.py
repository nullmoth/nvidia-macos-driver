#!/usr/bin/env python3
"""Warning ceilings: build/warn_ceiling.sh stops a build whose compiler warnings rose above the committed number in
build/warnings.txt. Arms are named; the gate is measured in both directions (it passes at the ceiling and stops one
above it), and every build script that calls it names a component that has a ceiling."""
import pathlib, re, subprocess, sys, tempfile
root = pathlib.Path(__file__).resolve().parents[1]
gate = root / 'build/warn_ceiling.sh'
table = {}
for line in (root / 'build/warnings.txt').read_text().splitlines():
    if line.strip() and not line.startswith('#'):
        name, n = line.split(); table[name] = int(n)
fails = 0
def arm(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += not ok
def run(*args):
    r = subprocess.run(['bash', str(gate), *map(str, args)], capture_output=True, text=True)
    return r.returncode, r.stdout
for comp, n in sorted(table.items()):
    rc, out = run(comp, n)
    arm(f'{comp} at its ceiling ({n}) passes', rc == 0 and f'warnings {n} (ceiling {n})' in out)
    rc, out = run(comp, n + 1)
    arm(f'{comp} one above its ceiling ({n + 1}) stops the build', rc == 4 and 'STOP' in out)
rc, _ = run('no-such-component', 0)
arm('a component with no line passes at 0', rc == 0)
rc, out = run('no-such-component', 1)
arm('a component with no line has a ceiling of 0: one warning stops it', rc == 4 and 'ceiling of 0' in out)
rc, out = run('plugin', '')
arm('an empty count (a log that was never written) stops instead of passing', rc != 0)
rc, out = run('plugin', 'x')
arm('a count that is not a number stops', rc == 4 and 'not a number' in out)
with tempfile.TemporaryDirectory() as t:
    log = pathlib.Path(t) / 'build.err'
    log.write_text('a.m:1:2: warning: one [-Wx]\n' * (table['plugin'] + 1))
    n = log.read_text().count(': warning:')
    rc, out = run('plugin', n, log)
    arm('a log with one warning more than the plugin ceiling stops, and shows the warnings', rc == 4 and 'a.m:1:2: warning' in out)
# every caller names a component with a line, and every line has a caller
used = set()
for p in list((root / 'build').glob('*.sh')) + list((root / 'kexts').glob('*/build*.sh')):
    for m in re.finditer(r'warn_ceiling\.sh"?\s+([A-Za-z0-9_-]+)', p.read_text()):
        used.add(m.group(1))
        arm(f'{p.relative_to(root)} gates {m.group(1)}, which has a ceiling line', m.group(1) in table)
arm('every ceiling line is used by a build script', set(table) <= used)
print(f'{"FAILED " + str(fails) if fails else "all passed"}')
sys.exit(1 if fails else 0)
