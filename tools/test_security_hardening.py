#!/usr/bin/env python3
"""The driver's part of the security batch from the external review of 1.15.0, checked without the card.

- The scripts that run as root (install.sh, uninstall.sh) fix PATH and stay executable.
- The kernel gates are present in the source (the binary and the live boot are checked on the card).

The 1401 app's part (login items, log collection, hardened runtime) is tested in 1401 (mac/tools)."""
import os
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


for f in ('package/install.sh', 'package/uninstall.sh'):
    text = (root / f).read_text()
    check(f'{f} fixes PATH before running anything',
          'PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH' in '\n'.join(text.splitlines()[:12]))
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

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
