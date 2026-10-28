#!/usr/bin/env python3
"""Exercise committed surface ownership using the production queue helpers."""
import pathlib, subprocess, tempfile
root=pathlib.Path(__file__).resolve().parents[1]
source=(root/'plugin/NVMTLEventSync.m').read_text()
start=source.index('@interface NVMTLPendingSurfaceWrite')
end=source.index('@implementation NVMTLSubmissionPart',start)
helpers=source[start:end]
fixture=r'''
#import <Foundation/Foundation.h>
#include <assert.h>
#include <stdint.h>
#include <stdatomic.h>
#include <objc/runtime.h>
#include <string.h>
#include <stdlib.h>
#include <time.h>
// the process is WindowServer; the kernel's answer to the marks is markAnswer (-1 = an accelerator without them, the
// default for the first arms; 0 = taken), and every mark is logged as "id:pending"
static const char *test_progname(void){return "WindowServer";}
#define getprogname() test_progname()
static int markAnswer=-1; static NSMutableArray *markLog;
// whether NVAccel publishes NVAccelSurfaceWait (its page-off waits for a pending surface): KERNEL_HALF=1 or 0
static int nvmtl_vk_accel_surface_wait(void){return getenv("KERNEL_HALF")&&!strcmp(getenv("KERNEL_HALF"),"1");}
static int nvmtl_vk_surface_gpu_mark(uint32_t sid,int pending){if(sid&&markLog)[markLog addObject:[NSString stringWithFormat:@"%u:%d",sid,pending]];return markAnswer;}
static void nvlog(const char *f,...){(void)f;}
struct __IOSurface {unsigned id,seed;};
// the kernel dirty mark makes the surface device-current, and IOSurface moves the seed when that happens (measured on
// Apple's driver: one seed step per GPU write); dirtyBumpsSeed models that, dirtyCalls counts the marks
static int dirtyBumpsSeed, dirtyCalls; static struct __IOSurface *dirtySurface;
static int nvmtl_vk_surface_dirty(uint32_t s,uint32_t p){(void)p;dirtyCalls++;if(dirtyBumpsSeed&&dirtySurface&&dirtySurface->id==s)dirtySurface->seed++;return 0;}
static void nvtrace(const char *f,...){(void)f;}
// cross-process busy marks (PR #398) are off in WindowServer, which this test runs as; the consumer side is covered
// by test_surface_publication.py
static int busyWrites;
static void nvmtl_busy_write(IOSurfaceRef s,NSUInteger p,uint64_t seq,int done){(void)s;(void)p;(void)seq;(void)done;busyWrites++;}
static unsigned IOSurfaceGetID(IOSurfaceRef s){return s->id;}
static unsigned IOSurfaceGetSeed(IOSurfaceRef s){return s->seed;}
@class NVMTLCommandBuffer;
@interface NVMTLTexture:NSObject{@public IOSurfaceRef _surf; BOOL _vramOn;NSUInteger _plane;}@end
@implementation NVMTLTexture
- (BOOL)nvmtlSurfaceOut{return YES;}
- (BOOL)nvmtlSurfaceOutDirtied:(BOOL)dirtied{(void)dirtied;return YES;}
@end
@interface NVMTLCommandBuffer:NSObject{@public id _queue;NSArray *_parts;NSDictionary *_busyMarks;}@end
@implementation NVMTLCommandBuffer @end
@interface NVMTLSubmissionPart:NSObject{@public NSSet *surfaces;}@end
@implementation NVMTLSubmissionPart @end
'''
checks=r'''
static NVMTLTexture *texture(IOSurfaceRef surface){NVMTLTexture*t=[NVMTLTexture new];t->_surf=surface;t->_vramOn=YES;return t;}
static NVMTLCommandBuffer *writer(id queue,NVMTLTexture*t){NVMTLCommandBuffer*b=[NVMTLCommandBuffer new];b->_queue=queue;NVMTLSubmissionPart*p=[NVMTLSubmissionPart new];p->surfaces=[NSSet setWithObject:t];b->_parts=@[p];return b;}
int main(void){@autoreleasepool{
 struct {unsigned id,seed;} surface={4,7};
 NVMTLTexture*t=texture((IOSurfaceRef)&surface),*alias=texture((IOSurfaceRef)&surface);
 NSObject*q=[NSObject new],*other=[NSObject new];
 NVMTLCommandBuffer*a=writer(q,t),*b=writer(q,t),*c=writer(other,t);
 assert(!nvmtl_surface_pending_write(b,t));
 nvmtl_surface_register_writes(a);
 assert(nvmtl_surface_pending_write(b,t));
 assert(nvmtl_surface_pending_write(b,alias));
 assert(!nvmtl_surface_pending_write(a,t));
 assert(nvmtl_surface_pending_write(c,t));
 struct {unsigned id,seed;} otherSurface={8,7};NVMTLTexture *unrelated=texture((IOSurfaceRef)&otherSurface);
 assert(!nvmtl_surface_pending_write(c,unrelated));
 surface.seed++;assert(!nvmtl_surface_pending_write(b,t));surface.seed--;
 alias->_vramOn=NO;assert(!nvmtl_surface_pending_write(b,alias));alias->_vramOn=YES;
 t->_vramOn=NO;assert(nvmtl_surface_pending_write(b,t));assert(!nvmtl_surface_pending_write(b,alias));t->_vramOn=YES;
 alias->_plane=1;assert(!nvmtl_surface_pending_write(b,alias));alias->_plane=0;
 nvmtl_surface_register_writes(b);nvmtl_surface_finish_writes(a);
 assert(nvmtl_surface_pending_write(a,t));
 nvmtl_surface_finish_writes(b);assert(!nvmtl_surface_pending_write(a,t));
 assert(nvmtl_surface_writers.count==0);
 for(unsigned i=0;i<1000;i++){nvmtl_surface_register_writes(a);assert(nvmtl_surface_pending_write(b,t));nvmtl_surface_finish_writes(a);assert(nvmtl_surface_writers.count==0);}
 __weak NVMTLCommandBuffer *weak;
 @autoreleasepool{NVMTLCommandBuffer*ephemeral=writer(q,t);weak=ephemeral;nvmtl_surface_register_writes(ephemeral);}
 assert(!weak);assert(!nvmtl_surface_pending_write(b,t));
 nvmtl_surface_finish_writes(b);assert(nvmtl_surface_writers.count==0);
 // WindowServer with an accelerator that takes the marks. The commit runs in production order (nvmtlSubmitWithEvents,
 // checked from its source below): the kernel marks, then the pending-write records.
 markAnswer=0; markLog=[NSMutableArray new]; dirtyBumpsSeed=1; dirtySurface=(struct __IOSurface*)&surface; dirtyCalls=0;
 const BOOL half=nvmtl_vk_accel_surface_wait();
 NVMTLCommandBuffer*w1=writer(q,t),*w2=writer(q,t);
 #define COMMIT(w) do{nvmtl_ws_mark_pending(w);nvmtl_surface_register_writes(w);}while(0)
 unsigned seed0=surface.seed;
 COMMIT(w1);
 assert([markLog containsObject:@"4:1"]);        // pending in the kernel either way
 if(half){
  // the capture fix: device-current at commit, which moves the seed, and the record must be stamped after that move,
  // or the next command buffer reads the surface as if its write had landed and imports the CPU copy over the GPU's
  assert(dirtyCalls==1&&surface.seed==seed0+1);
  assert(nvmtl_surface_pending_write(w2,t));
 }else{
  // an accelerator whose page-off does not wait: nothing is marked newest at commit (a CPU lock would page off a
  // surface still being drawn); the record is stamped as before
  assert(dirtyCalls==0&&surface.seed==seed0);
  assert(nvmtl_surface_pending_write(w2,t));
 }
 nvmtl_surface_finish_writes(w1);
 assert([markLog.lastObject isEqual:@"4:0"]&&!nvmtl_surface_pending_write(w2,t));
 // a surface without a shared VRAM copy is never marked newest at commit, and its record is stamped the same way
 t->_vramOn=NO;dirtyCalls=0;[markLog removeAllObjects];const unsigned seed1=surface.seed;
 COMMIT(w1);
 assert(dirtyCalls==0&&surface.seed==seed1&&[markLog containsObject:@"4:1"]&&nvmtl_surface_pending_write(w2,t));
 nvmtl_surface_finish_writes(w1);t->_vramOn=YES;
 // a CPU write after the commit still invalidates the record (later CPU edits import)
 COMMIT(w1);surface.seed++;assert(!nvmtl_surface_pending_write(w2,t));nvmtl_surface_finish_writes(w1);
 assert(nvmtl_surface_writers.count==0);
 // two command buffers in flight write the same surface: the kernel has one pending bit, so the older one finishing
 // first must not release it while the newer one still writes
 [markLog removeAllObjects];
 COMMIT(w1);COMMIT(w2);
 assert([nvmtl_ws_pending_holds countForObject:@4]==2);
 nvmtl_surface_finish_writes(w1);
 assert(![markLog containsObject:@"4:0"]&&[nvmtl_ws_pending_holds countForObject:@4]==1);
 nvmtl_surface_finish_writes(w1);   // a second finish of the same buffer releases nothing more
 assert(![markLog containsObject:@"4:0"]&&[nvmtl_ws_pending_holds countForObject:@4]==1);
 nvmtl_surface_finish_writes(w2);
 assert([markLog.lastObject isEqual:@"4:0"]&&[nvmtl_ws_pending_holds countForObject:@4]==0);
 assert(nvmtl_surface_writers.count==0&&!nvmtl_ws_deferred.count);
 printf("PASS (kernel half %s): aliases, CPU seeds, queues, planes, private backing, latest writer, weak lifetime, 1000 cleanup "
        "cycles, WindowServer commit: %s, CPU edits still import, overlapping writers release the kernel mark once, with "
        "the last of them\n", half?"present":"absent",
        half?"VRAM marked newest at commit and the record stamped after the kernel's seed move"
            :"nothing marked newest at commit (old ordering for an accelerator without the page-off wait)");
}return 0;}
'''
# The commit's order lives in nvmtlSubmitWithEvents, which this harness cannot run: read it from the source. The kernel
# marks can wait for the accelerator lock, so they sit between the two class-lock sections, before the records are
# stamped; inside them the VRAM marks go in before this command buffer's own pending marks.
commit=source[source.index('- (void)nvmtlSubmitWithEvents'):]
commit=commit[:commit.index('\n}\n')]
first_lock_end=commit.index('\n    }\n')
marks=commit.index('nvmtl_ws_mark_pending(self);')
second_lock=commit.index('@synchronized ([NVMTLCommandBuffer class])',first_lock_end)
stamp=commit.index('nvmtl_surface_register_writes(self);')
assert first_lock_end<marks<second_lock<stamp, 'commit order: (class lock) seal, (unlocked) kernel marks, (class lock) records'
assert commit.index('_commitMarking = YES')<first_lock_end and second_lock<commit.index('_commitMarking = NO'), 'no submit-ahead while marking'
assert commit.count('nvmtl_vk_surface_dirty')==0
pend=source[source.index('static void nvmtl_ws_mark_pending('):]
pend=pend[:pend.index('\n}\n')]
assert pend.index('nvmtl_vk_surface_dirty(')<pend.index('nvmtl_ws_mark(n.unsignedIntValue, YES)'), 'VRAM marks before pending marks'
print('PASS: production commit order: kernel marks outside the class lock, VRAM marks before pending marks, records after both')
with tempfile.TemporaryDirectory() as d:
 p=pathlib.Path(d);(p/'test.m').write_text(fixture+helpers+checks)
 subprocess.run(['clang','-fobjc-arc','-framework','Foundation',str(p/'test.m'),'-o',str(p/'test')],check=True)
 import os
 for half in ('1','0'): subprocess.run([str(p/'test')],check=True,env=dict(os.environ,KERNEL_HALF=half))
