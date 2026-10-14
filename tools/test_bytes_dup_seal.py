#!/usr/bin/env python3
"""The repeated-setBytes cache never outlives the ring chunk it points at.

_g79BRing holds ring chunks without owning them. The chunks belong to the command buffer's scratch list, which sealing a
submission part hands to that part; the part frees them when it completes, possibly while the command buffer is still
being encoded. If the cache survived the seal, the next identical setBytes bound and retained a freed chunk, and the
compute encoder's binding table released it later (mediaanalysisd crashes in endEncoding / .cxx_destruct).

Checks the source: the cache is declared unretained (so this rule is needed at all), the seal hands the scratch list
over and clears the cache in the same method, and the finished command buffer clears it too. Negative control: the
v1.15.0 release, which shipped without it, must fail the seal check."""
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


def method(src, sig):
    i = src.find(sig)
    if i < 0:
        return None
    j = src.find('\n}\n', i)
    return src[i:j]


def seal_clears(eventsync):
    seal = method(eventsync, '- (void)nvmtlSealPartWithSignal:')
    if seal is None:
        return None
    hands_over = 'part->scratch = [_scratch copy]' in seal and '[_scratch removeAllObjects]' in seal
    clears = re.search(r'memset\(_g79BRing, 0, sizeof _g79BRing\)', seal) is not None
    return hands_over, clears


objects = (root / 'plugin/NVMTLObjects.m').read_text()
eventsync = (root / 'plugin/NVMTLEventSync.m').read_text()
check('the cache is declared unretained (the reason this rule exists)',
      '__unsafe_unretained NVMTLBuffer *_g79BRing[' in objects)
r = seal_clears(eventsync)
if r is None:
    sys.exit('nvmtlSealPartWithSignal not found - check is UNKNOWN, not clean')
check('sealing a part hands the scratch list over', r[0])
check('sealing a part clears the setBytes cache in the same step', r[1])
seal = method(eventsync, '- (void)nvmtlSealPartWithSignal:')
HOLD = 'if (restart) for (id o in (NVMTLResSet *)part->resources) [(NVMTLResSet *)_resources addObject:o];'
check('a seal that goes on recording keeps holding what the binding tables still name', HOLD in seal)
fin = method(eventsync, '- (void)nvmtlFinishExecution:')
check('a finished command buffer clears the setBytes cache', fin is not None and 'memset(_g79BRing, 0, sizeof _g79BRing)' in fin)

old = subprocess.run(['git', '-C', str(root), 'show', 'v1.15.0:plugin/NVMTLEventSync.m'], capture_output=True, text=True)
if old.returncode:
    sys.exit('could not read NVMTLEventSync.m at v1.15.0 - negative control is UNKNOWN, not clean')
if old.stdout == eventsync:
    print('note: v1.15.0 already holds this change - the negative control compares it with itself')
ro = seal_clears(old.stdout)
check('negative control: the v1.15.0 seal leaves the cache pointing at chunks it handed away',
      ro is not None and ro[0] and not ro[1])
sys.exit(1 if fails else 0)
