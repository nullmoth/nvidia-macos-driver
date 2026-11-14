#!/usr/bin/env python3
"""Exercise production resource tracking and copy selection, including alias and lifetime fallbacks."""
import argparse
from pathlib import Path
import re
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1])
args = parser.parse_args()
objects = (args.source / 'plugin/NVMTLObjects.m').read_text()
header = (args.source / 'plugin/NVMTLObjects.h').read_text()
events = (args.source / 'plugin/NVMTLEventSync.m').read_text()
start = objects.index('static NSMutableSet *nvmtl_identity_set(')
end = objects.index('@end', objects.index('@implementation NVMTLResSet')) + 4
implementation = objects[start:end]
interface = re.search(r'@interface NVMTLResSet : NSMutableSet .*?@end', header, re.S).group()
start = events.index('static NSArray<NVMTLBuffer *> *nvmtl_submission_copies(')
selection = events[start:events.index('\nstatic void nvmtl_prepare_submission_copies', start)]
assert 'addReadOnlyObject:' in interface, 'source has no read-only tracking'

harness = r'''
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <os/lock.h>
#include <stdatomic.h>
#include <assert.h>
static _Atomic unsigned long gRes2Tick;
static _Atomic long refs, made, gone;
static inline void nvmtl_resref(id o, int d) { atomic_fetch_add(&refs, d); }
@@INTERFACE@@
@@IMPLEMENTATION@@
typedef struct { void *map; } Backing;
@interface NVMTLBuffer : NSObject { @public void *_hostPtr; Backing _shadow; BOOL _texSeen; } @end
@implementation NVMTLBuffer
- (instancetype)init { if ((self=[super init])) atomic_fetch_add(&made,1); return self; }
- (void)dealloc { atomic_fetch_add(&gone,1); }
@end
@interface NVMTLTexture : NSObject { @public NSUInteger _usage; NVMTLBuffer *_backBuf; NVMTLTexture *_parent; } @end
@implementation NVMTLTexture @end
@interface NVMTLHeapPin : NSObject { @public NSArray *_buffers; }
- (NSArray *)allCopyCandidates;
@end
@implementation NVMTLHeapPin
- (NSArray *)allCopyCandidates { return _buffers; }
@end
@@SELECTION@@
static NVMTLBuffer *host(void) { NVMTLBuffer *b=[NVMTLBuffer new]; b->_hostPtr=(void *)1; return b; }
static void selectionTests(void) { @autoreleasepool {
    NVMTLResSet *s=[NVMTLResSet new]; NVMTLBuffer *a=host(), *b=host();
    [s addReadOnlyObject:a]; [s addObject:b];
    assert(s.count==2 && ![s isWrittenObject:a] && [s isWrittenObject:b]);
    NSArray *inputs=nvmtl_submission_copies(s,NO), *outputs=nvmtl_submission_copies(s,YES);
    assert(inputs.count==2 && [inputs containsObject:a] && [inputs containsObject:b]);
    assert(outputs.count==1 && outputs[0]==b);
    a->_shadow.map=(void *)1;
    assert(nvmtl_submission_copies(s,YES).count==2); // managed synchronization remains conservative
    a->_shadow.map=NULL; a->_texSeen=YES;
    assert(nvmtl_submission_copies(s,YES).count==2); // aliases may have written the backing
    a->_texSeen=NO;
    assert(nvmtl_submission_copies([NSSet setWithObject:a],YES).count==1); // unknown metadata
    NVMTLHeapPin *heap=[NVMTLHeapPin new]; heap->_buffers=@[a];
    assert(nvmtl_submission_copies([NSSet setWithObject:heap],YES).count==1);
    NVMTLTexture *t=[NVMTLTexture new]; t->_backBuf=a; t->_usage=MTLTextureUsageShaderWrite;
    [s addObject:t]; assert(nvmtl_submission_copies(s,YES).count==2); // backing deduplicated
    [s removeObject:t]; t->_usage=MTLTextureUsageShaderRead; [s addObject:t];
    assert(nvmtl_submission_copies(s,YES).count==1);
    NVMTLTexture *view=[NVMTLTexture new]; view->_parent=t; view->_usage=MTLTextureUsageUnknown;
    [s addObject:view]; assert(nvmtl_submission_copies(s,YES).count==2);
    [s removeObject:view]; [s removeObject:t];
    NVMTLResSet *part=[s nvmtlTakeAll]; assert(!s.count && ![part isWrittenObject:a]);
    for (id o in part) { if ([part isWrittenObject:o]) [s addObject:o]; else [s addReadOnlyObject:o]; }
    assert(![s isWrittenObject:a] && [s isWrittenObject:b]);
    NVMTLResSet *copy=[s copy], *mutable=[s mutableCopy];
    [s addObject:a]; [s addReadOnlyObject:b]; // upgrade read->write; no downgrade write->read
    assert([s isWrittenObject:a] && [s isWrittenObject:b]);
    assert(![copy isWrittenObject:a] && ![mutable isWrittenObject:a]);
    for (unsigned i=0;i<16;i++) [s addObject:[NSObject new]]; // evict MRU entries
    [s addReadOnlyObject:a]; assert([s isWrittenObject:a]);
    [s removeObject:a]; [s addReadOnlyObject:a]; assert(![s isWrittenObject:a]);
    [s addObject:a]; assert([s isWrittenObject:a]); // upgrade cannot be bypassed by a stale MRU
    [s removeAllObjects]; [s addReadOnlyObject:a]; assert(![s isWrittenObject:a]);
} }
static void lifetimeTests(void) {
    @autoreleasepool { // no per-iteration drain, like applications that retain completed command buffers
        long before=atomic_load(&gone), initial=atomic_load(&made);
        for (unsigned r=0;r<2000;r++) {
            NVMTLResSet *s=[NVMTLResSet new];
            for (unsigned i=0;i<8;i++) [s addReadOnlyObject:host()];
            NVMTLResSet *copy=[s copy], *part=[s nvmtlTakeAll];
            [copy removeAllObjects]; copy=nil; part=nil; s=nil;
        }
        assert(atomic_load(&made)-initial==16000);
        fprintf(stderr,"lifetime made=%ld gone=%ld refs=%ld\n",atomic_load(&made)-initial,atomic_load(&gone)-before,atomic_load(&refs));
        assert(atomic_load(&gone)-before==16000 && atomic_load(&refs)==0);
    }
}
static void concurrentUpgrades(void) { @autoreleasepool {
    NVMTLResSet *s=[NVMTLResSet new]; NVMTLBuffer *b=host();
    dispatch_apply(20000,dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^(size_t i){
        if(i&1) [s addReadOnlyObject:b]; else [s addObject:b];
    });
    assert(s.count==1 && [s isWrittenObject:b]);
} }
int main(void) {
    selectionTests(); lifetimeTests(); concurrentUpgrades();
    assert(atomic_load(&refs)==0 && atomic_load(&made)==atomic_load(&gone));
    puts("PASS: uploads unchanged; explicit read-only sources skip copy-back; writes, managed shadows, aliases and unknown metadata stay synchronized; copies/seals preserve access; 16000 resources freed before pool drain; concurrent upgrades balanced");
}
'''
for key, value in [('INTERFACE', interface), ('IMPLEMENTATION', implementation), ('SELECTION', selection)]:
    harness = harness.replace('@@' + key + '@@', value)
for label, flags in [('release', []), ('sanitized', ['-fsanitize=address,undefined'])]:
    with tempfile.TemporaryDirectory(prefix='nvmtl-readonly-', dir='/private/tmp') as directory:
        src, exe = Path(directory) / 'probe.m', Path(directory) / 'probe'
        src.write_text(harness)
        subprocess.run(['xcrun', 'clang', '-fobjc-arc', '-O1', '-g', *flags,
                        str(src), '-framework', 'Foundation', '-framework', 'Metal', '-o', str(exe)], check=True)
        subprocess.run([str(exe)], check=True, timeout=90)
        print('PASS configuration:', label, flush=True)
