#!/usr/bin/env python3
"""A command buffer keeps every resource it was given until its work is done.

Compiles the production NVMTLResSet from plugin/NVMTLObjects.m and races encoder threads adding resources against a
thread sealing submission parts. The old seal (copy, then removeAllObjects) must lose resources under that race - the
negative control - and the new nvmtlTakeAll must lose none and leave the residency counts balanced. Exercises snapshot
enumeration under ARC, including mutation, nested pools/loops, full completion and break/return/throw. Completed loops
must release members before the outer pool drains; early exits must release them at that drain. Also checks that the
command buffer makes its resource set in -init and that sealing no longer clears it separately."""
import os
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
# the identity-hashed set the class allocates sits just above it in NVMTLObjects.m
impl = objects[objects.index('static NSMutableSet *nvmtl_identity_set('):objects.index('@implementation NVMTLResSet')] + impl
iface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', header, re.S).group(0)
size = re.search(r'^#define NVMTL_RESSET_DM\s+\d+.*$', header, re.M)
if size:
    iface = size.group(0) + '\n' + iface

HARNESS = r'''
#import <Foundation/Foundation.h>
#include <os/lock.h>
#include <stdatomic.h>
#include <assert.h>
static _Atomic unsigned long gRes2Tick;
static _Atomic long gRefs, gMade, gGone;
static inline void nvmtl_resref(id o, int d) { atomic_fetch_add(&gRefs, d); }
''' + iface + '\n' + impl + r'''
@interface Tok : NSObject { @public long serial; } - (long)ping; @end
@implementation Tok
- (instancetype)init { if ((self = [super init])) serial = atomic_fetch_add(&gMade, 1); return self; }
- (long)ping { return serial; }
- (void)dealloc { atomic_fetch_add(&gGone, 1); }
@end
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
static void fill(NVMTLResSet *set, int n) { for (int k = 0; k < n; k++) [set addObject:[Tok new]]; }
static void visit(Tok *object, long base, unsigned char *seen, int n) {
    long index = [object ping] - base;
    assert(index >= 0 && index < n && !seen[index]); seen[index] = 1;
}
__attribute__((noinline)) static void fullEnumeration(int mutate) {
    NVMTLResSet *set = [NVMTLResSet new]; long base = atomic_load(&gMade);
    fill(set, 128); unsigned char visited[128] = {0}; int count = 0;
    for (Tok *object in set) {
        // The snapshot was created before this pool, and must survive all 128 body-pool drains and batch refills.
        @autoreleasepool {
            visit(object, base, visited, 128);
            if (++count == 1 && mutate) {
                dispatch_sync(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ @autoreleasepool {
                    [set removeAllObjects]; [set addObject:[Tok new]];
                } });
                assert(atomic_load(&gGone) == base); // every original is still alive in the snapshot
            }
        }
    }
    assert(count == 128);
    if (mutate) assert(atomic_load(&gGone) == base + 128 && set.count == 1);
    [set removeAllObjects]; set = nil;
}
__attribute__((noinline)) static void earlyEnumeration(int mode) {
    NVMTLResSet *set = [NVMTLResSet new]; fill(set, 128);
    for (Tok *object in set) {
        @autoreleasepool {
            assert([object ping] >= 0); [set removeAllObjects];
            assert(atomic_load(&gGone) == 0 && atomic_load(&gRefs) == 0);
            if (mode == 1) return;
            if (mode == 2) @throw [NSException exceptionWithName:@"EarlyExit" reason:nil userInfo:nil];
            break;
        }
    }
}
__attribute__((noinline)) static void nestedEnumeration(void) {
    NVMTLResSet *set = [NVMTLResSet new]; fill(set, 128);
    unsigned char outer[128] = {0}, inner[128] = {0}; int outerCount = 0, innerCount = 0;
    for (Tok *o in set) { @autoreleasepool {
        visit(o, 0, outer, 128);
        if (++outerCount == 1) {
            for (Tok *i in set) { @autoreleasepool {
                visit(i, 0, inner, 128);
                if (++innerCount == 1) [set removeAllObjects];
            } }
            assert(innerCount == 128 && atomic_load(&gGone) == 0);
        }
    } }
    assert(outerCount == 128 && atomic_load(&gGone) == 128 && atomic_load(&gRefs) == 0);
}
__attribute__((noinline)) static void manualEnumeration(NSMutableArray * __weak *snapshotProbe) {
    NVMTLResSet *set = [NVMTLResSet new]; NSFastEnumerationState st = {0};
    id __unsafe_unretained batch[5];
    assert([set countByEnumeratingWithState:&st objects:batch count:5] == 0);
    assert(st.mutationsPtr && *st.mutationsPtr == 0);
    st = (NSFastEnumerationState){0}; fill(set, 33);
    unsigned char seen[33] = {0}; NSUInteger total = 0, n; unsigned long *mutation = NULL, value = 0;
    while ((n = [set countByEnumeratingWithState:&st objects:batch count:5])) {
        assert(n <= 5 && st.itemsPtr == batch && st.mutationsPtr);
        if (!mutation) {
            mutation = st.mutationsPtr; value = *mutation;
            // Observe the actual pool-owned snapshot container without retaining it.
            *snapshotProbe = (__bridge NSMutableArray *)(void *)st.extra[0];
            [set removeAllObjects];
        }
        assert(st.mutationsPtr == mutation && *mutation == value);
        for (NSUInteger k = 0; k < n; k++) { @autoreleasepool { visit(st.itemsPtr[k], 0, seen, 33); } }
        total += n;
    }
    assert(total == 33 && atomic_load(&gGone) == 33 && atomic_load(&gRefs) == 0);
    assert([set countByEnumeratingWithState:&st objects:batch count:5] == 0);
    assert(st.mutationsPtr == mutation && *mutation == value);
    assert(*snapshotProbe && (*snapshotProbe).count == 0);
}
static int enumerationTest(const char *mode) {
    __weak NSMutableArray *snapshotProbe = nil;
    @autoreleasepool {
        if (!strcmp(mode, "enum-full") || !strcmp(mode, "enum-mutation")) {
            // No pool per round: completed normal submission enumerations must not accumulate resource owners.
            for (int k = 0; k < 200; k++) {
                fullEnumeration(!strcmp(mode, "enum-mutation"));
                assert(atomic_load(&gGone) == atomic_load(&gMade) && atomic_load(&gRefs) == 0);
            }
        } else if (!strcmp(mode, "enum-nested")) nestedEnumeration();
        else if (!strcmp(mode, "enum-manual")) manualEnumeration(&snapshotProbe);
        else {
            int early = !strcmp(mode, "enum-return") ? 1 : !strcmp(mode, "enum-throw") ? 2 : 0;
            BOOL caught = NO;
            @try { earlyEnumeration(early); }
            @catch (NSException *exception) { assert([exception.name isEqualToString:@"EarlyExit"]); caught = YES; }
            assert(caught == (early == 2));
            assert(atomic_load(&gMade) == 128 && atomic_load(&gGone) == 0 && atomic_load(&gRefs) == 0);
        }
    }
    assert(atomic_load(&gGone) == atomic_load(&gMade) && atomic_load(&gRefs) == 0);
    assert(!snapshotProbe);
    puts("PASS"); return 0;
}
static int cacheTest(void) { @autoreleasepool {
    NSMutableArray *tokens=[NSMutableArray new];
    for(unsigned i=0;i<512;i++) [tokens addObject:[Tok new]];
    NVMTLResSet *set=[NVMTLResSet new];
    for(unsigned pass=0;pass<4;pass++) for(id token in tokens) [set addObject:token];
    assert(set.count==tokens.count);
    // More live members than cache slots exercise collisions and MRU eviction.
    for(id token in tokens) {
        [set removeObject:token]; assert(set.count==tokens.count-1 && ![set member:token]);
        [set addObject:token]; assert(set.count==tokens.count && [set member:token]==token);
    }
    NVMTLResSet *part=[set nvmtlTakeAll]; assert(!set.count && part.count==tokens.count);
    for(id token in tokens) [set addObject:token]; assert(set.count==tokens.count);
    NVMTLResSet *copy=[set copy]; assert(copy.count==tokens.count);
    [set removeAllObjects]; assert(!set.count);
    for(id token in tokens) [set addObject:token]; assert(set.count==tokens.count);
    [part removeAllObjects]; [copy removeAllObjects]; [set removeAllObjects];
    assert(!atomic_load(&gRefs));
    puts("PASS: member-cache collisions, repeated adds, removal/re-add, transfer, copy and clear");
    return 0;
} }
int main(int argc, char **argv) {
    if (!strcmp(argv[1],"cache")) return cacheTest();
    if (argc != 2) return 2;
    if (!strncmp(argv[1], "enum-", 5)) return enumerationTest(argv[1]);
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
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O2', '-o', str(exe), str(src), '-framework', 'Foundation'],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit('could not compile the production NVMTLResSet - check is UNKNOWN, not clean')
    for enabled in ('1','0'):
        subprocess.run([str(exe),'cache'],check=True,timeout=30,
                       env={**os.environ,'NVMTL_RESSET_CACHE':enabled})
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

    # the race fires rarely on some machines (measured: 1 run in 6), so the control gets a bounded number of tries
    for _try in range(30):
        lazy = subprocess.run([str(exe), 'lazy'], capture_output=True, text=True, timeout=300)
        if lazy.returncode != 0 or (lazy.stdout.split() and int(lazy.stdout.split()[0]) > 0): break
    eager = subprocess.run([str(exe), 'eager'], capture_output=True, text=True, timeout=300)
    lazy_lost = int(lazy.stdout.split()[0]) if lazy.returncode == 0 and lazy.stdout.split() else -1
    eager_lost = int(eager.stdout.split()[0]) if eager.returncode == 0 and eager.stdout.split() else -1
    check('negative control: a set made lazily by two encoder threads loses resources or crashes',
          lazy.returncode != 0 or lazy_lost > 0,
          f'try {_try + 1}, exit {lazy.returncode}, {lazy_lost} lost in 20000 command buffers' if lazy.returncode == 0 else f'crashed, exit {lazy.returncode}')
    check('a set made with the command buffer keeps both threads\' resources', eager.returncode == 0 and eager_lost == 0,
          f'exit {eager.returncode}, {eager_lost} lost in 20000 command buffers')

    modes = {
        'enum-full': 'full loops release all members before the outer pool drains',
        'enum-mutation': 'cross-thread mutation preserves the snapshot through nested body pools and batch refills',
        'enum-break': 'break keeps snapshot members alive until the pool drains, then releases all of them',
        'enum-return': 'return releases every snapshot member at pool drain',
        'enum-throw': 'throw releases every snapshot member at pool drain',
        'enum-nested': 'nested enumerations hold independent snapshots across mutation',
        'enum-manual': 'empty/multi-batch enumeration has a safe mutation pointer and releases its snapshot at pool drain',
    }

    def run_enumerations(binary, label):
        for mode, name in modes.items():
            result = subprocess.run([str(binary), mode], capture_output=True, text=True, timeout=30)
            check(f'{label}: {name}', result.returncode == 0 and result.stdout.strip() == 'PASS',
                  '' if result.returncode == 0 else f'exit {result.returncode}: {result.stderr[-600:]}')

    run_enumerations(exe, 'ARC -O2')
    sanitizer = ['-fsanitize=address,undefined', '-fno-sanitize-recover=all', '-g']
    for label, flags in [('O0', ['-O0']), ('sanitized', ['-O2', *sanitizer])]:
        binary = pathlib.Path(d) / label
        subprocess.run(['xcrun', 'clang', '-fobjc-arc', *flags, '-o', str(binary), str(src),
                        '-framework', 'Foundation'], check=True)
        run_enumerations(binary, f'ARC {label}')

    controls = [
        ('undrained-members', impl.replace('[snapshot removeAllObjects]; st->extra[0] = 0;', 'st->extra[0] = 0;'),
         'enum-full', 'full-loop test detects snapshot members left in the outer pool'),
        ('unowned-snapshot', impl.replace('NSMutableArray * __autoreleasing snapshot', 'NSMutableArray *snapshot'),
         'enum-mutation', 'mutation test detects an ARC snapshot that dies at the first method return'),
        ('retained-state', impl.replace('(__bridge void *)snapshot', '(__bridge_retained void *)snapshot'),
         'enum-break', 'early-break test detects an unbalanced +1 stored in enumeration state'),
    ]
    for label, control, mode, name in controls:
        assert control != impl, f'{label} negative control did not modify the implementation'
        control_src, binary = pathlib.Path(d) / f'{label}.m', pathlib.Path(d) / label
        control_src.write_text(HARNESS.replace(impl, control, 1))
        subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O2', *sanitizer, '-o', str(binary), str(control_src),
                        '-framework', 'Foundation'], check=True)
        result = subprocess.run([str(binary), mode], capture_output=True, text=True, timeout=30,
                                env={**os.environ, 'ASAN_OPTIONS': 'detect_leaks=0'})
        # A crash only qualifies when the expected assertion or use-after-free actually fired.
        detected = ('heap-use-after-free' in result.stderr or 'AddressSanitizer:DEADLYSIGNAL' in result.stderr
                    if label == 'unowned-snapshot' else 'Assertion failed:' in result.stderr)
        check(f'negative control: {name}', result.returncode != 0 and detected, f'exit {result.returncode}')

init = objects[objects.index('@implementation NVMTLCommandBuffer'):]
init = init[init.index('- (instancetype)init {'):]
init = init[:init.index('\n}\n')]
check('the command buffer makes its resource set in -init', '_resources = [NVMTLResSet new];' in init)
check('sealing takes the set in one step and does not clear it separately',
      '[_resources removeAllObjects]' not in eventsync and 'nvmtlTakeAll]' in eventsync)
sys.exit(1 if fails else 0)
