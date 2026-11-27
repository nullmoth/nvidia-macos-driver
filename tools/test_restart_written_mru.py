#!/usr/bin/env python3
"""A buffer read before an event boundary and written after it is still known as written.

-nvmtlSealPartWithSignal:restart:YES (encodeSignalEvent:/encodeWaitForEvent:) hands the part's resources back to the
command buffer's set through -addObject: and then marks the members the part only read as read-only again
(-nvmtlSyncWrittenFrom:). NVMTLResSet's -addObject: returns early on an MRU hit, so if those read-only members stayed in
the MRU, a write binding of one after the boundary would stay read-only: nvmtl_submission_copies(..., writtenOnly) then
skips its sync-out and the CPU never sees the GPU's write.

Compiles the production NVMTLResSet from plugin/NVMTLObjects.m, replays the seal's own `if (restart)` lines from
plugin/NVMTLEventSync.m, and checks the next part marks the buffer written. Negative control: the same hand-back with
a read-only sync that leaves the MRU alone (the first 2.9 port) loses the write. Also checks that a part which failed
to submit still reports "scheduled" (addScheduledHandler: blocks run)."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
objects = (root / 'plugin/NVMTLObjects.m').read_text()
header = (root / 'plugin/NVMTLObjects.h').read_text()
eventsync = (root / 'plugin/NVMTLEventSync.m').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


seal = eventsync[eventsync.index('- (void)nvmtlSealPartWithSignal:'):]
seal = seal[:seal.index('\n}\n')]
lines = [l.strip() for l in seal.splitlines() if l.strip().startswith('if (restart)') and 'nvmtl_vk_cmd_begin' not in l]
check('the seal hands its resources back on a restart', any('addObject:' in l for l in lines), f'{len(lines)} line(s)')
production = '\n'.join(lines).replace('(NVMTLResSet *)part->resources', 'part').replace('(NVMTLResSet *)_resources', 'res')

impl = objects[objects.index('@implementation NVMTLResSet'):]
impl = impl[:impl.index('@end') + 4]
impl = objects[objects.index('static NSMutableSet *nvmtl_identity_set('):objects.index('@implementation NVMTLResSet')] + impl
iface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', header, re.S).group(0)

HARNESS = r'''
#import <Foundation/Foundation.h>
#include <os/lock.h>
#include <stdatomic.h>
static _Atomic unsigned long gRes2Tick;
static _Atomic long gRefs;
static inline void nvmtl_resref(id o, int d) { atomic_fetch_add(&gRefs, d); }
''' + iface + '\n' + impl + r'''
@interface Tok : NSObject @end
@implementation Tok @end
static void mru_blind_restart(NVMTLResSet *part, NVMTLResSet *res) {   /* the first 2.9 port */
    for (id o in part) [res addObject:o];
    os_unfair_lock_lock(&part->_lk); NSMutableSet *ms = [part->_s copy], *mw = [part->_w copy]; os_unfair_lock_unlock(&part->_lk);
    os_unfair_lock_lock(&res->_lk); for (id o in ms) if (![mw member:o]) [res->_w removeObject:o]; os_unfair_lock_unlock(&res->_lk);
}
static void production_restart(NVMTLResSet *part, NVMTLResSet *res) {
    const BOOL restart = YES;
''' + production + r'''
}
/* 1: src (read before the boundary, written after it) is written in the next part and dst (written before) still is */
static int scenario(int blind, int others) { @autoreleasepool {
    NVMTLResSet *res = [NVMTLResSet new];
    Tok *src = [Tok new], *dst = [Tok new];
    NSMutableArray *keep = [NSMutableArray new];
    [res addReadOnlyObject:src]; [res addObject:dst];          /* blit copyFromBuffer:src toBuffer:dst */
    for (int i = 0; i < others; i++) { Tok *t = [Tok new]; [keep addObject:t]; [res addObject:t]; }
    NVMTLResSet *part = [res nvmtlTakeAll];                    /* encodeSignalEvent: seals with restart:YES */
    if (blind) mru_blind_restart(part, res); else production_restart(part, res);
    BOOL handedBack = [res member:src] && [res member:dst] && ![res isWrittenObject:src] && [res isWrittenObject:dst];
    [res addObject:src];                                       /* a compute encoder then writes src */
    NVMTLResSet *next = [res nvmtlTakeAll];
    return handedBack && [next isWrittenObject:src] && [next isWrittenObject:dst];
} }
int main(void) {
    int prod = 0, blind = 0, n = 0;
    for (int others = 0; others <= 12; others++, n++) { prod += scenario(0, others); blind += scenario(1, others); }
    printf("%d %d %d %ld\n", prod, blind, n, (long)atomic_load(&gRefs));
    return 0;
}
'''
with tempfile.TemporaryDirectory(prefix='nvmtl-restart-mru-') as d:
    d = pathlib.Path(d)
    (d / 't.m').write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-framework', 'Foundation', '-o', str(d / 't'), str(d / 't.m')],
                       capture_output=True, text=True)
    check('the production NVMTLResSet and the seal\'s restart lines compile', r.returncode == 0, r.stderr[-800:])
    if r.returncode == 0:
        prod, blind, n, refs = map(int, subprocess.run([str(d / 't')], capture_output=True, text=True, timeout=60).stdout.split())
        check('a buffer read before the boundary and written after it is written in the next part (0..12 other members)',
              prod == n, f'{prod}/{n}')
        check('negative control: a read-only sync that leaves the MRU alone loses that write', blind < n, f'{blind}/{n} kept')
        check('residency references balance', refs == 0, f'{refs}')

adv = eventsync[eventsync.index('- (BOOL)nvmtlAdvancePart {'):]
adv = adv[:adv.index('if (result == -2)')]
check('a part that failed to submit still marks the command buffer scheduled',
      re.search(r'if \(result\) \[self nvmtlMarkScheduled\];\s*$', adv) is not None)

print('PASS' if not fails else f'FAILED {fails}')
sys.exit(1 if fails else 0)
