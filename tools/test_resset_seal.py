#!/usr/bin/env python3
"""A command buffer keeps every resource it was given until its work is done.

Compiles the production NVMTLResSet from plugin/NVMTLObjects.m and races encoder threads adding resources against a
thread sealing submission parts. The old seal (copy, then removeAllObjects) must lose resources under that race - the
negative control - and the new nvmtlTakeAll must lose none and leave the residency counts balanced. Also checks that the
command buffer makes its resource set in -init and that sealing no longer clears it separately."""
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


impl = objects[objects.index('@implementation NVMTLResSet'):]
impl = impl[:impl.index('@end') + 4]
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
@interface CB : NSObject { @public NVMTLResSet *_resources; } @end
@implementation CB @end
static long lazyRace(int eager) {
    /* two encoder threads giving one new command buffer its first resources at the same moment, 20000 times */
    long lost = 0;
    for (int k = 0; k < 20000; k++) @autoreleasepool {
        CB *cb = [CB new]; if (eager) cb->_resources = [NVMTLResSet new];
        Tok *x = [Tok new], *y = [Tok new];
        dispatch_group_t g = dispatch_group_create();
        dispatch_queue_t q = dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0);
        dispatch_group_async(g, q, ^{ if (!cb->_resources) cb->_resources = [NVMTLResSet new]; [cb->_resources addObject:x]; });
        dispatch_group_async(g, q, ^{ if (!cb->_resources) cb->_resources = [NVMTLResSet new]; [cb->_resources addObject:y]; });
        dispatch_group_wait(g, DISPATCH_TIME_FOREVER);
        lost += 2 - (long)cb->_resources.count;
    }
    return lost;
}
int main(int argc, char **argv) {
    if (argv[1][0] == 'l' || argv[1][0] == 'e') { printf("%ld\n", lazyRace(argv[1][0] == 'e')); return 0; }
    const int useNew = argv[1][0] == 'n', threads = 4, per = 40000;
    NVMTLResSet *live = [NVMTLResSet new];
    NSMutableArray *parts = [NSMutableArray new];
    __block _Atomic int adders = threads;
    dispatch_group_t g = dispatch_group_create();
    for (int t = 0; t < threads; t++)
        dispatch_group_async(g, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
            for (int k = 0; k < per; k++) @autoreleasepool { [live addObject:[Tok new]]; }
            atomic_fetch_sub(&adders, 1);
        });
    while (atomic_load(&adders) > 0) @autoreleasepool {
        if (useNew) [parts addObject:[live nvmtlTakeAll]];
        else { [parts addObject:[live copy]]; [live removeAllObjects]; }
    }
    dispatch_group_wait(g, DISPATCH_TIME_FOREVER);
    [parts addObject:[live nvmtlTakeAll]];
    long held = 0; for (NVMTLResSet *p in parts) held += (long)p.count;
    long refsWhileHeld = atomic_load(&gRefs);
    [parts removeAllObjects]; live = nil;
    printf("%ld %ld %ld %lu\n", (long)threads * per, held, refsWhileHeld - held, (unsigned long)atomic_load(&gRefs));
    return 0;
}
'''

with tempfile.TemporaryDirectory() as d:
    src, exe = pathlib.Path(d) / 'resset.m', pathlib.Path(d) / 'resset'
    src.write_text(HARNESS)
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-o', str(exe), str(src), '-framework', 'Foundation'],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit('could not compile the production NVMTLResSet - check is UNKNOWN, not clean')
    runs = {}
    for mode in ('old', 'new'):
        out = subprocess.run([str(exe), mode], capture_output=True, text=True, timeout=300).stdout.split()
        runs[mode] = [int(x) for x in out]
    added, held, extra, after = runs['new']
    check('new seal: every resource added while parts are sealed is held by exactly one set', held == added,
          f'{held} held of {added} added')
    check('new seal: residency counts match what the sets hold, and return to 0 when they are gone',
          extra == 0 and after == 0, f'count minus held {extra}, after release {after}')
    oadded, oheld, _, _ = runs['old']
    check('negative control: the old copy-then-clear seal loses resources under the same race', oheld < oadded,
          f'{oheld} held of {oadded} added')

    lazy = subprocess.run([str(exe), 'lazy'], capture_output=True, text=True, timeout=300)
    eager = subprocess.run([str(exe), 'eager'], capture_output=True, text=True, timeout=300)
    lazy_lost = int(lazy.stdout.split()[0]) if lazy.returncode == 0 and lazy.stdout.split() else -1
    eager_lost = int(eager.stdout.split()[0]) if eager.returncode == 0 and eager.stdout.split() else -1
    check('negative control: a set made lazily by two encoder threads loses resources or crashes',
          lazy.returncode != 0 or lazy_lost > 0,
          f'exit {lazy.returncode}, {lazy_lost} lost in 20000 command buffers' if lazy.returncode == 0 else f'crashed, exit {lazy.returncode}')
    check('a set made with the command buffer keeps both threads\' resources', eager.returncode == 0 and eager_lost == 0,
          f'exit {eager.returncode}, {eager_lost} lost in 20000 command buffers')

init = objects[objects.index('@implementation NVMTLCommandBuffer'):]
init = init[init.index('- (instancetype)init {'):]
init = init[:init.index('\n}\n')]
check('the command buffer makes its resource set in -init', '_resources = [NVMTLResSet new];' in init)
check('sealing takes the set in one step and does not clear it separately',
      '[_resources removeAllObjects]' not in eventsync and 'nvmtlTakeAll]' in eventsync)
sys.exit(1 if fails else 0)
