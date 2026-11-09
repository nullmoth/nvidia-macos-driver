#!/usr/bin/env python3
"""NVAccel logs the lines that fire for every client, surface, context or barrier only with the -nvaccelverbose boot argument.
usage: test_nvaccel_log_volume.py [repo root]"""
import pathlib, sys
root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
fails = 0
def check(n, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + n); fails += 0 if ok else 1
check('the verbose gate exists', '#define ALOGV(' in src and '"-nvaccelverbose"' in src)
for k in ('EM prepareBarrier', 'EM completeBarrier', 'EM writeBarrierElement', 'getStampMemory', 'newGLContext', 'newCLContext',
          'newSurface', 'new2DContext', 'newVideoContext', 'newMemoryMap', 'newDisplayMachine', 'newUserClient(type', 'newUserClient(0x%x) -> %s on'):
    check(f'{k}: verbose only', f'ALOGV("{k}' in src and f'ALOG("{k}' not in src)
check('start-up and refusal lines still always log', 'ALOG("IOGraphicsAccelerator2::start -> %d"' in src and 'ALOG("newUserClient(0x%x): NVRM refused' in src)
print(f'{fails} failed' if fails else 'all passed'); sys.exit(1 if fails else 0)
