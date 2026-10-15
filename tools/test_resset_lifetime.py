#!/usr/bin/env python3
"""A finished command buffer's resources die when the command buffer lets go of them, even inside a pool that never drains.

Geekbench 7 commits from a thread whose autorelease pool is never drained. If clearing, copying or sealing a resource set
autoreleases an array of its members, every resource lands in that pool and lives until the process exits; Super
Resolution then fills VRAM with 9 MB MPS buffers. This compiles the production NVMTLResSet (and, as the negative control,
the one in the committed tree), runs add / seal / copy / clear / drop inside ONE outer pool at the same optimisation as
the plugin, and counts resources that were actually deallocated BEFORE that pool drains."""
import pathlib
import re
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


def resset(objects, header):
    impl = objects[objects.index('@implementation NVMTLResSet'):]
    impl = impl[:impl.index('@end') + 4]
    iface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', header, re.S).group(0)
    return iface + '\n' + impl


HARNESS = r'''
#import <Foundation/Foundation.h>
#include <os/lock.h>
#include <stdatomic.h>
static _Atomic unsigned long gRes2Tick;
static _Atomic long gRefs, gMade, gGone;
static inline void nvmtl_resref(id o, int d) { atomic_fetch_add(&gRefs, d); }
@@RESSET@@
@interface Tok : NSObject @end
@implementation Tok
- (instancetype)init { if ((self = [super init])) atomic_fetch_add(&gMade, 1); return self; }
- (void)dealloc { atomic_fetch_add(&gGone, 1); }
@end
int main(int argc, char **argv) {
    const int rounds = 2000, per = 8;
    long goneBeforeDrain, refsBeforeDrain;
    @autoreleasepool {   /* the app's commit thread: never drained while the work runs */
        for (int r = 0; r < rounds; r++) {
            NVMTLResSet *cb = [NVMTLResSet new];
            for (int k = 0; k < per; k++) [cb addObject:[Tok new]];
            NVMTLResSet *part = [cb nvmtlTakeAll];          /* seal */
            for (int k = 0; k < per; k++) [cb addObject:[Tok new]];
            NVMTLResSet *twin = [cb copy];                   /* a copy, as a second part takes */
            [cb removeAllObjects];                           /* the finished command buffer lets go */
            part = nil; twin = nil; cb = nil;                /* the GPU work is done: no owner is left */
        }
        goneBeforeDrain = atomic_load(&gGone); refsBeforeDrain = atomic_load(&gRefs);
    }
    printf("%ld %ld %ld %ld\n", (long)atomic_load(&gMade), goneBeforeDrain, refsBeforeDrain, (long)atomic_load(&gGone));
    return 0;
}
'''


def run(label, objects, header, d):
    src, exe = pathlib.Path(d) / f'{label}.m', pathlib.Path(d) / label
    src.write_text(HARNESS.replace('@@RESSET@@', resset(objects, header)))
    r = subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O2', '-o', str(exe), str(src), '-framework', 'Foundation'],
                       capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-2000:])
        sys.exit(f'could not compile the {label} NVMTLResSet - check is UNKNOWN, not clean')
    out = subprocess.run([str(exe)], capture_output=True, text=True, timeout=300)
    if out.returncode or len(out.stdout.split()) != 4:
        sys.exit(f'{label} harness exited {out.returncode} - check is UNKNOWN, not clean')
    return [int(x) for x in out.stdout.split()]


new_obj = (root / 'plugin/NVMTLObjects.m').read_text()
new_hdr = (root / 'plugin/NVMTLObjects.h').read_text()
# the set as it stood just before the fix (the commit that made sealing a one-step take, 4434649, had the leak)
BEFORE = '4434649'
git = lambda p: subprocess.run(['git', '-C', str(root), 'show', f'{BEFORE}:{p}'], capture_output=True, text=True)
old_obj, old_hdr = git('plugin/NVMTLObjects.m'), git('plugin/NVMTLObjects.h')
if old_obj.returncode or old_hdr.returncode:
    sys.exit('could not read the NVMTLResSet before the fix - negative control is UNKNOWN, not clean')

with tempfile.TemporaryDirectory() as d:
    made, gone, refs, after = run('new', new_obj, new_hdr, d)
    check('every resource is deallocated before the app\'s pool drains', gone == made, f'{gone} of {made} gone before the drain')
    check('residency counts return to 0 before the drain', refs == 0, f'{refs} left')
    if old_obj.stdout == new_obj:
        print(f'note: {BEFORE} already holds this NVMTLResSet - the negative control below compares it with itself')
    omade, ogone, _, oafter = run('old', old_obj.stdout, old_hdr.stdout, d)
    check(f'negative control: the set at {BEFORE} leaves resources alive in the undrained pool', ogone < omade,
          f'{ogone} of {omade} gone before the drain, {oafter} after it')
sys.exit(1 if fails else 0)
