#!/usr/bin/env python3
"""The security batch from the external review of 1.15.0, checked where it can be checked without the card.

- The login item only points at a program whose every parent directory is closed to other users.
- Scripts that run as root fix PATH; log collection never follows a symbolic link when it relaxes permissions.
- The 1401 app is signed with the hardened runtime and library validation, and its Metal device list comes from a
  helper that is not: library validation refuses an ad-hoc signed bundle, which is what the NVIDIA Metal plugin is.
- The kernel gates are present in the source (the binary and the live boot are checked on the card).

Pass a built 1401.app as the second argument to also check its signatures."""
import os
import pathlib
import plistlib
import shutil
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
built = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else None
setup = (root / 'app/Resources/nullmoth-setup.sh').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


# --- no login item or background job: setup writes none and removes the ones earlier versions left ---
check('setup installs no login item', 'cat > "$AGENT"' not in setup and 'com.nullmoth.crashcheck</string>' not in setup)
check('setup installs no WindowServer restart or display tracker',
      'com.nullmoth.wsreset</string>' not in setup and 'com.nullmoth.displaytrace</string>' not in setup)
check('an install removes the jobs an earlier version left',
      'for j in /Library/LaunchAgents/com.nullmoth.crashcheck.plist /Library/LaunchDaemons/com.nullmoth.wsreset.plist' in setup
      and 'launchctl bootout system "$j"' in setup)
check('log collection relaxes permissions without following links', 'chmod -R -h a+rX "$COLLECT"' in setup
      and 'chmod -R a+rX "$COLLECT"' not in setup)
for f in ('package/uninstall.sh', 'app/Resources/nullmoth-log-capture.sh', 'app/Resources/nullmoth-operation-capture.sh'):
    text = (root / f).read_text()
    check(f'{f} fixes PATH before running anything',
          'PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH' in '\n'.join(text.splitlines()[:12]))
for f in ('package/uninstall.sh', 'app/Resources/nullmoth-log-capture.sh', 'app/Resources/nullmoth-setup.sh'):
    check(f'{f} is still executable', os.access(root / f, os.X_OK))

# --- kernel gates in the source ---
nvrm = (root / 'kexts/NVRM/NVRM.cpp').read_text()
osx = (root / 'kexts/NVRM/os-xnu.cpp').read_text()
check('RM administrator check reads the caller uid', 'return (kauth_getuid() == 0) ? NV_TRUE : NV_FALSE;' in osx
      and 'os_is_administrator(void) { NV_XNU_TRC; return NV_TRUE; }' not in osx)
check('first-time GPU bring-up refuses non-root callers', 'if (pass > fPassDone && kauth_getuid() != 0)' in nvrm)
check('event waiters are capped', 'kNvEvtMaxSleepers = 256' in nvrm and 'OSDecrementAtomic(&gNvEvtSleepers)' in nvrm)
check('bind-fd rejects out-of-range values and clears a stale context when bound to a real fd',
      'if (v > 0x7FFFFFFFLL || v < -0x80000000LL) return kIOReturnBadArgument;' in nvrm and 'if (me->fBound >= 0 && me->fMmap.valid)' in nvrm)

# --- app signing: hardened app, unhardened Metal helper ---
build = (root / 'app/build.sh').read_text()
main = (root / 'app/Sources/main.swift').read_text()
check('the app is signed with the hardened runtime and library validation',
      'codesign --force --runtime-version 15.0.0 --options runtime,library --entitlements "$ENT" -s - "$APP"' in build)
check('the app may send Apple Events (Restart) under the hardened runtime', 'com.apple.security.automation.apple-events' in build)
check('the Metal helper is signed without the hardened runtime',
      'codesign --force -s - "$APP/Contents/Resources/nullmoth-metal-devices"' in build)
check('the report takes Metal devices from the helper', '"metal": metal.names' in main and 'nullmoth-metal-devices' in main)

clang = shutil.which('clang') or '/usr/bin/clang'
with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    (t / 'p.c').write_text('int f(void){return 42;}\n')
    (t / 'm.c').write_text('#include <dlfcn.h>\n#include <stdio.h>\nint main(int c,char**v){void*h=dlopen(v[1],RTLD_NOW);'
                           'puts(h?"LOADED":"REFUSED");return 0;}\n')
    try:
        subprocess.run([clang, '-bundle', t / 'p.c', '-o', t / 'p.bundle'], check=True, capture_output=True, timeout=120)
        subprocess.run([clang, t / 'm.c', '-o', t / 'plain'], check=True, capture_output=True, timeout=120)
        shutil.copy(t / 'plain', t / 'hardened')
        for f, opts in (('p.bundle', []), ('plain', []), ('hardened', ['--options', 'runtime,library'])):
            subprocess.run(['/usr/bin/codesign', '-f', '-s', '-', *opts, t / f], check=True, capture_output=True, timeout=60)
        plain = subprocess.run([t / 'plain', t / 'p.bundle'], capture_output=True, text=True, timeout=30).stdout.strip()
        hard = subprocess.run([t / 'hardened', t / 'p.bundle'], capture_output=True, text=True, timeout=30).stdout.strip()
        check('library validation refuses an ad-hoc bundle (why the Metal list runs in a helper)', hard == 'REFUSED', hard)
        check('negative control: an unhardened ad-hoc process loads the same bundle', plain == 'LOADED', plain)
    except (OSError, subprocess.SubprocessError) as e:
        check('library-validation probe could build and run', False, str(e)[:120])

if built:
    def flags(p):
        r = subprocess.run(['/usr/bin/codesign', '-dv', p], capture_output=True, text=True, timeout=60)
        return next((ln for ln in r.stderr.splitlines() if 'flags=' in ln), '')
    app_flags, helper_flags = flags(built), flags(built / 'Contents/Resources/nullmoth-metal-devices')
    check('built app: hardened runtime and library validation', 'runtime' in app_flags and 'library-validation' in app_flags, app_flags)
    check('built Metal helper: no hardened runtime', helper_flags and 'runtime' not in helper_flags, helper_flags)
    ent = subprocess.run(['/usr/bin/codesign', '-d', '--entitlements', '-', '--xml', built], capture_output=True, timeout=60).stdout
    check('built app: Apple Events entitlement', plistlib.loads(ent).get('com.apple.security.automation.apple-events') is True if ent else False)
    r = subprocess.run([built / 'Contents/Resources/nullmoth-metal-devices'], capture_output=True, text=True, timeout=30)
    check('built Metal helper lists this Mac\'s Metal devices', r.returncode == 0 and r.stdout.strip() != '', r.stdout.strip()[:60])

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
