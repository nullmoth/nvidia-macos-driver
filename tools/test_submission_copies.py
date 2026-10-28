#!/usr/bin/env python3
"""Exercise production copy discovery and byte transfers without a GPU."""
import os
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
events = (root / 'plugin/NVMTLEventSync.m').read_text()
start = events.index('static NSArray<NVMTLBuffer *> *nvmtl_submission_copies(')
prepare_start = events.index('static void nvmtl_prepare_submission_copies(', start)
helper = events[start:prepare_start]
prepare = events[prepare_start:events.index('- (BOOL)nvmtlSubmitAhead', prepare_start)]
ahead = events[events.index('- (BOOL)nvmtlSubmitAhead'):events.index('static void nvmtl_gsub_ahead_pass1(void);')]
advance = events[events.index('- (BOOL)nvmtlAdvancePart'):events.index('- (void)nvmtlSubmitWithEvents')]
ws = events[events.index('static const void *const nvmtl_ws_marks_key'):events.index('static void nvmtl_surface_finish_writes(')]
objects = (root / 'plugin/NVMTLObjects.m').read_text()
start = objects.index('static void nvmtl_buffer_sync_in(NVMTLBuffer *b)\n{')
sync = objects[start:]
fixture = r'''
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <assert.h>
#include <stdint.h>
#include <string.h>
typedef struct { void *map; size_t size; } Buffer;
@interface NVMTLBuffer : NSObject { @public
    void *_hostPtr; size_t _hostLen; Buffer _b, _shadow; unsigned _syncOut, _zshadow;
} @end
@implementation NVMTLBuffer @end
@interface NVMTLTexture : NSObject { @public
    NVMTLBuffer *_backBuf; NVMTLTexture *_parent; MTLTextureUsage _usage;
    IOSurfaceRef _surf; NSUInteger _plane; BOOL _vramOn; int _tag;
} @end
@implementation NVMTLTexture @end
static unsigned uploads, downloads, managedCopies;
enum { NVP_SYNCIN, NVP_SYNCOUT };
static uint64_t nvmtl_perf_now(void) { return 0; }
static void nvmtl_perf_note(int kind, uint64_t time, size_t size) {
    if (kind == NVP_SYNCIN) uploads++; else downloads++;
}
static void *nvmtl_device_queue(void) { return NULL; }
static int nvmtl_vk_copy_buffer(void *q, Buffer *from, size_t off,
                               Buffer *to, size_t dst, size_t size) {
    memcpy((char *)to->map + dst, (char *)from->map + off, size); managedCopies++; return 0;
}
#define nvlog(...) ((void)0)
// what the WindowServer capture path (NVMTLEventSync.m) asks of the kernel. The marks are off until an arm turns them
// on; KERNEL_HALF=1 is an accelerator that publishes NVAccelSurfaceWait, 0 one without it. Every kernel mark is logged
// in events as "m<surface>:<pending>", and every VRAM mark as "d<surface>" and counted in dirtyCalls.
static NSMutableArray *events; static unsigned dirtyCalls; static BOOL wsKernelWaits;
static int nvmtl_vk_surface_dirty(uint32_t s,uint32_t p){(void)p;dirtyCalls++;if(events)[events addObject:[NSString stringWithFormat:@"d%u",s]];return 0;}
static BOOL nvmtl_ws_kernel_waits(void){return wsKernelWaits;}
static uint32_t nvmtl_vk_surface_gpu_mark(uint32_t sid,int pending){if(events)[events addObject:[NSString stringWithFormat:@"m%u:%d",sid,pending]];return 0;}
static int nvmtl_vk_accel_surface_wait(void){return getenv("KERNEL_HALF")&&!strcmp(getenv("KERNEL_HALF"),"1");}
static void nvtrace(const char *f,...){(void)f;}
'''
checks = r'''
int main(void) { @autoreleasepool {
    unsigned char cpu[16]={0}, gpu[16]={0}, shadow[16]={0};
    NVMTLBuffer *buffer=[NVMTLBuffer new];
    buffer->_hostPtr=cpu; buffer->_hostLen=16; buffer->_b=(Buffer){gpu,16};
    NVMTLTexture *texture=[NVMTLTexture new]; texture->_backBuf=buffer;
    texture->_usage=MTLTextureUsageShaderRead;
    NVMTLTexture *view=[NVMTLTexture new]; view->_parent=texture;
    view->_usage=MTLTextureUsageShaderRead;
    NVMTLTexture *ordinary=[NVMTLTexture new];
    NSSet *resources=[NSSet setWithObjects:texture,view,ordinary,nil];
    memset(cpu,71,16); // CPU edits after buffer/texture creation.
    for (NVMTLBuffer *b in nvmtl_submission_copies(resources,NO)) nvmtl_buffer_sync_in(b);
    assert(uploads==1 && !memcmp(cpu,gpu,16));
    memset(cpu,93,16); // Read-only textures must not copy stale GPU pixels back.
    for (NVMTLBuffer *b in nvmtl_submission_copies(resources,YES)) nvmtl_buffer_sync_out(b);
    assert(downloads==0 && cpu[0]==93);
    view->_usage=MTLTextureUsageShaderWrite; memset(gpu,107,16);
    for (NVMTLBuffer *b in nvmtl_submission_copies(resources,YES)) nvmtl_buffer_sync_out(b);
    assert(downloads==1 && !memcmp(cpu,gpu,16));
    view->_usage=MTLTextureUsageUnknown;
    assert(nvmtl_submission_copies(resources,YES).count==1);
    view->_usage=MTLTextureUsageRenderTarget;
    assert(nvmtl_submission_copies(resources,YES).count==1);
    resources=[NSSet setWithObjects:buffer,texture,view,nil];
    assert(nvmtl_submission_copies(resources,NO).count==1);
    assert(nvmtl_submission_copies(resources,YES).count==1);
    NVMTLBuffer *managed=[NVMTLBuffer new];
    managed->_b=(Buffer){gpu,16}; managed->_shadow=(Buffer){shadow,16}; managed->_syncOut=1;
    for (NVMTLBuffer *b in nvmtl_submission_copies([NSSet setWithObject:managed],YES)) nvmtl_buffer_sync_out(b);
    assert(managedCopies==1 && !managed->_syncOut && !memcmp(gpu,shadow,16));
    NVMTLBuffer *imported=[NVMTLBuffer new]; imported->_b=(Buffer){gpu,16};
    assert(!nvmtl_submission_copies([NSSet setWithObjects:imported,ordinary,nil],NO));
    assert(!nvmtl_submission_copies([NSSet set],YES));
    puts("PASS: post-creation pixels, texture views, one copy per backing, read-only protection, unknown usage, managed writeback, zero-copy exclusion");
} }
'''
submission_fixture = r"""
#import <objc/message.h>
#import <IOSurface/IOSurface.h>
#include <stdatomic.h>
#include <time.h>
typedef struct { void *cb; } Commands;
typedef struct { int unused; } nvmtl_inflight;
@interface NVMTLSubmissionPart : NSObject { @public
    Commands commands; NSSet *resources; NSArray *surfaces, *scratch, *signals;
    id waitEvent, signalEvent; uint64_t value;
    BOOL waitFolded, aheadEmpty, claimed; int aheadFailed; unsigned encoders;
    nvmtl_inflight *inflight;
    NSArray<NVMTLBuffer *> *copyInputs; BOOL copyInputsPrepared, hasHostCopies;
} @end
@implementation NVMTLSubmissionPart @end
@interface NVMTLTexture (Publication)
- (BOOL)nvmtlSurfaceOut;
- (BOOL)nvmtlSurfaceOutDirtied:(BOOL)dirtied;
@end
// every publication is logged in events as "o<tag>:<dirtied>" (dirtied: the commit's VRAM mark stands, nothing is asked
// of the kernel again)
@implementation NVMTLTexture (Publication)
- (BOOL)nvmtlSurfaceOut { return [self nvmtlSurfaceOutDirtied:NO]; }
- (BOOL)nvmtlSurfaceOutDirtied:(BOOL)dirtied { if (events) [events addObject:[NSString stringWithFormat:@"o%d:%d",_tag,(int)dirtied]]; return YES; }
@end
@interface NVMTLCommandBuffer : NSObject { @public
    BOOL _executionDone, _committed, _commitMarking, _partsRetiring; NSError *_cbError;
    NSUInteger _nextSubmit, _nextPart; NSMutableArray *_parts; double _legacyKernelStart;
}
- (BOOL)nvmtlSubmitAhead;
- (BOOL)nvmtlAdvancePart;
- (void)nvmtlMarkScheduled;
- (void)nvmtlCollectDeviceTiming:(Commands *)commands;
- (void)nvmtlFinishExecution:(NSError *)error;
@end
static _Atomic unsigned long long gsub_inflight, gsub_peak, gsub_ahead, gEvFold, gEvFoldEmpty, gSkipEmpty;
static NSMutableArray *nvmtl_quarantined_parts;
static unsigned begins, waits, finishes, scheduled;
static int submitResult; static BOOL allocationFails;
static unsigned char *testGPU; static unsigned char expectedPixel, writtenPixel;
static int nvmtl_gsub_cap(void) { return 8; }
static BOOL nvmtl_evfold_on(void) { return NO; }
static uint64_t nvmtl_evfold_promised(id event) { return 0; }
static void nvmtl_evfold_census(void *counter, const char *message) {}
static void nvmtl_evfold_promise_part(id part) {}
static void nvmtl_gsub_census(void) {}
static BOOL nvmtl_skip_empty_on(void) { return NO; }
static void nvmtl_vk_cmd_abandon(Commands *cmd) { cmd->cb=NULL; }
static nvmtl_inflight *nvmtl_vk_inflight_new(void) { return allocationFails ? NULL : calloc(1,sizeof(nvmtl_inflight)); }
static void nvmtl_vk_inflight_free(nvmtl_inflight *f) { free(f); }
static void fakeGPU(void) {
    assert(testGPU[0]==expectedPixel); // Submission must see the latest CPU bytes.
    if (!submitResult) memset(testGPU,writtenPixel,16);
}
static int nvmtl_vk_submit_begin(Commands *cmd, nvmtl_inflight *f) { begins++; fakeGPU(); return submitResult; }
static int nvmtl_vk_submit_wait(Commands *cmd) { waits++; fakeGPU(); return submitResult; }
static int nvmtl_gsub_finish(NVMTLSubmissionPart *part) {
    finishes++; nvmtl_vk_inflight_free(part->inflight); part->inflight=NULL;
    atomic_fetch_sub(&gsub_inflight,1); return submitResult;
}
@interface TestEvent : NSObject { @public unsigned char *cpu; unsigned char expected; unsigned signals; }
- (void)setSignaledValue:(uint64_t)value;
@end
@implementation TestEvent
- (void)setSignaledValue:(uint64_t)value { assert(cpu[0]==expected); signals++; }
@end
/*WS*/
@implementation NVMTLCommandBuffer
- (void)nvmtlMarkScheduled { scheduled++; }
- (void)nvmtlCollectDeviceTiming:(Commands *)commands {}
- (void)nvmtlFinishExecution:(NSError *)error { _cbError=error; _executionDone=YES; nvmtl_ws_mark_ready(self); }
"""
submission_checks = r"""
@end
static NSUInteger at(NSString *e) { NSUInteger i=[events indexOfObject:e]; if (i==NSNotFound) { NSLog(@"missing %@ in %@",e,events); abort(); } return i; }
static NSUInteger lastAt(NSString *e) { NSUInteger i=[events indexOfObjectWithOptions:NSEnumerationReverse passingTest:^BOOL(id o,NSUInteger k,BOOL *stop){return [o isEqual:e];}]; assert(i!=NSNotFound); return i; }
static BOOL seen(NSString *e) { return [events containsObject:e]; }
static NSUInteger times(NSString *e) { NSUInteger n=0; for (NSString *o in events) n+=[o isEqual:e]; return n; }
static NSString *mark(IOSurfaceRef s,int pending) { return [NSString stringWithFormat:@"m%u:%d",IOSurfaceGetID(s),pending]; }
static NVMTLTexture *tex(IOSurfaceRef s,int tag,BOOL vram) { NVMTLTexture *t=[NVMTLTexture new]; t->_surf=s; t->_tag=tag; t->_vramOn=vram; return t; }
// a committed command buffer with one part per entry, through the production commit order (nvmtlSubmitWithEvents):
// the VRAM marks, then the pending marks
static NVMTLCommandBuffer *commitParts(NSArray *partSurfaces) {
    NSMutableArray *parts=[NSMutableArray new];
    for (NSArray *surfaces in partSurfaces) {
        NVMTLSubmissionPart *p=[NVMTLSubmissionPart new];
        p->commands.cb=(void *)1; p->encoders=1; p->resources=[NSSet set]; p->surfaces=surfaces; [parts addObject:p];
    }
    NVMTLCommandBuffer *cb=[NVMTLCommandBuffer new]; cb->_committed=YES; cb->_parts=parts;
    nvmtl_ws_mark_pending(cb);
    return cb;
}
static NVMTLCommandBuffer *makeCommand(NSSet *resources, TestEvent *event) {
    NVMTLSubmissionPart *part=[NVMTLSubmissionPart new];
    part->commands.cb=(void *)1; part->resources=resources; part->encoders=1;
    part->signalEvent=event; part->value=1;
    NVMTLCommandBuffer *cb=[NVMTLCommandBuffer new]; cb->_committed=YES;
    cb->_parts=[NSMutableArray arrayWithObject:part]; return cb;
}
int main(void) { @autoreleasepool {
    unsigned char cpu[16]={0}, gpu[16]={0}; testGPU=gpu;
    NVMTLBuffer *b=[NVMTLBuffer new]; b->_hostPtr=cpu; b->_hostLen=16; b->_b=(Buffer){gpu,16};
    NVMTLTexture *t=[NVMTLTexture new]; t->_backBuf=b; t->_usage=MTLTextureUsageShaderWrite;
    NVMTLTexture *v=[NVMTLTexture new]; v->_parent=t; v->_usage=t->_usage;
    NSSet *resources=[NSSet setWithObjects:t,v,nil];
    TestEvent *event=[TestEvent new]; event->cpu=cpu;
    memset(cpu,31,16); expectedPixel=31; writtenPixel=47; event->expected=47;
    NVMTLCommandBuffer *cb=makeCommand(resources,event);
    assert([cb nvmtlAdvancePart]);
    assert(cb->_executionDone && !cb->_cbError && waits==1 && uploads==1 && downloads==1 && event->signals==1);
    // Failed submissions must neither download untrusted bytes nor signal completion.
    submitResult=-1; expectedPixel=61; memset(cpu,61,16);
    cb=makeCommand(resources,event); assert([cb nvmtlAdvancePart]);
    assert(cb->_cbError && uploads==2 && downloads==1 && event->signals==1 && cpu[0]==61);
    submitResult=0; expectedPixel=73; writtenPixel=89; memset(cpu,73,16);
    cb=makeCommand([NSSet setWithObjects:t,v,nil],event); event->expected=89;
    const char *lift=getenv("NVMTL_GSUB_HOSTPTR_LIFT");
    if (!lift) {
        assert(![cb nvmtlSubmitAhead]); // Texture views retain the default host-pointer hold.
        assert(!begins && uploads==2 && !cb->_nextSubmit);
    } else {
        allocationFails=YES;
        assert(![cb nvmtlSubmitAhead] && uploads==2 && !begins && !cb->_nextSubmit);
        allocationFails=NO;
        assert([cb nvmtlSubmitAhead] && begins==1 && uploads==3 && downloads==1);
        memset(cpu,101,16); // Finishing an ahead submission must not upload again.
        assert([cb nvmtlAdvancePart]);
        assert(finishes==1 && waits==2 && uploads==3 && downloads==2 && cpu[0]==89 && event->signals==2);
        assert(cb->_executionDone && !cb->_cbError);
        submitResult=-1; expectedPixel=113; memset(cpu,113,16);
        cb=makeCommand([NSSet setWithObject:v],event);
        assert(![cb nvmtlSubmitAhead]); assert([cb nvmtlAdvancePart]);
        assert(cb->_cbError && downloads==2 && event->signals==2 && !atomic_load(&gsub_inflight));
    }
    // A commit still putting its kernel marks in is not submitted ahead: the GPU would start a write that no mark holds.
    submitResult=0; expectedPixel=gpu[0]; writtenPixel=gpu[0];
    {
        NVMTLSubmissionPart *p=[NVMTLSubmissionPart new]; p->commands.cb=(void *)1; p->encoders=1; p->resources=[NSSet set];
        NVMTLCommandBuffer *marking=[NVMTLCommandBuffer new]; marking->_committed=YES; marking->_commitMarking=YES;
        marking->_parts=[NSMutableArray arrayWithObject:p];
        const unsigned before=begins;
        assert(![marking nvmtlSubmitAhead] && begins==before && !p->inflight);
        // parts being retired under the class lock are never submitted ahead (#251, #340: a part freed under a running copy)
        marking->_commitMarking=NO; marking->_partsRetiring=YES;
        assert(![marking nvmtlSubmitAhead] && begins==before && !p->inflight);
        marking->_partsRetiring=NO;
        assert([marking nvmtlSubmitAhead] && begins==before+1 && p->inflight);
        assert([marking nvmtlAdvancePart] && marking->_executionDone && !marking->_cbError);
    }
    // WindowServer with the kernel marks on. A surface's pending mark holds its swap and any CPU page-off, so it must
    // stay until every write to the surface in flight has landed; with the page-off wait (KERNEL_HALF=1) the commit also
    // marks the VRAM newest, and without it (a plugin paired with an older accelerator) it must not.
    const BOOL half=nvmtl_vk_accel_surface_wait();
    submitResult=0; expectedPixel=gpu[0]; writtenPixel=gpu[0];
    wsKernelWaits=YES; events=[NSMutableArray new]; dirtyCalls=0;
    NSDictionary *props=@{(id)kIOSurfaceWidth:@16,(id)kIOSurfaceHeight:@16,(id)kIOSurfaceBytesPerElement:@4};
    IOSurfaceRef shared=IOSurfaceCreate((__bridge CFDictionaryRef)props), other=IOSurfaceCreate((__bridge CFDictionaryRef)props);
    assert(shared && other);
    // 1. One command buffer whose second part writes the swap surface again.
    NVMTLTexture *swap=tex(shared,1,YES), *overlay=tex(other,2,YES);
    cb=commitParts(@[@[swap,overlay],@[swap]]);
    assert(seen(mark(shared,1)) && seen(mark(other,1)));
    assert(dirtyCalls==(half?3:0));   // VRAM marked newest at commit only when the page-off waits
    // and before the pending marks: a page-off of the surface would otherwise wait for this command buffer while its
    // commit waits for the accelerator lock that page-off holds
    if (half) assert(lastAt([NSString stringWithFormat:@"d%u",IOSurfaceGetID(shared)]) < at(mark(shared,1)));
    assert([cb nvmtlAdvancePart] && !cb->_executionDone);
    if (half) {
        // the swap surface is still being written: no release (the swap would present it before the second write
        // landed) and no publication (it would call the surface current with that write in flight)
        assert(!seen(mark(shared,0)) && !seen(@"o1:1") && !seen(@"o1:0"));
        // the overlay is done: released first, then published without asking the kernel again
        assert(at(mark(other,0)) < at(@"o2:1"));
    } else {
        // the old ordering: every part publishes through the full path as it lands, and no mark is released yet
        assert(seen(@"o1:0") && seen(@"o2:0") && !seen(mark(shared,0)) && !seen(mark(other,0)));
    }
    assert([cb nvmtlAdvancePart] && cb->_executionDone && !cb->_cbError);
    if (half) assert(at(mark(shared,0)) < at(@"o1:1") && times(@"o1:1")==1);
    else assert(at(mark(shared,0)) > lastAt(@"o1:0") && at(mark(other,0)) > lastAt(@"o1:0"));
    assert(dirtyCalls==(half?3:0));   // publication never asked the kernel again
    // 2. Two command buffers in flight write the same surface; the older one finishes first.
    [events removeAllObjects];
    NVMTLTexture *older=tex(shared,3,YES), *newer=tex(shared,4,YES);
    NVMTLCommandBuffer *cbOld=commitParts(@[@[older]]), *cbNew=commitParts(@[@[newer]]);
    assert([cbOld nvmtlAdvancePart] && cbOld->_executionDone);
    assert(!seen(mark(shared,0)));   // the newer writer still holds it: a swap or capture now would read it mid-write
    if (half) assert(!seen(@"o3:1"));
    else assert(seen(@"o3:0"));
    assert([cbNew nvmtlAdvancePart] && cbNew->_executionDone && !cbNew->_cbError);
    if (half) assert(at(mark(shared,0)) < at(@"o3:1") && at(mark(shared,0)) < at(@"o4:1"));
    else assert(at(mark(shared,0)) > at(@"o4:0"));
    // 3. The newer writer fails: its holds go with it and the older writer's landed work is still published.
    [events removeAllObjects];
    older=tex(shared,5,YES); newer=tex(shared,6,YES);
    cbOld=commitParts(@[@[older]]); cbNew=commitParts(@[@[newer]]);
    assert([cbOld nvmtlAdvancePart] && cbOld->_executionDone && !seen(mark(shared,0)));
    submitResult=-1; expectedPixel=gpu[0];
    assert([cbNew nvmtlAdvancePart] && cbNew->_executionDone && cbNew->_cbError);
    submitResult=0;
    assert(seen(mark(shared,0)) && !seen(@"o6:0") && !seen(@"o6:1"));
    if (half) assert(at(mark(shared,0)) < at(@"o5:1"));
    else assert(seen(@"o5:0"));
    // 4. A private copy of the surface (no shared VRAM) in the first part, the shared one in the second. Without the page-off
    // wait the private copy is copied out as its part lands, as before. With it, the copy-out locks the surface, and the
    // lock's page-off would wait for the mark the second part holds: it waits for the release like the shared one.
    [events removeAllObjects]; dirtyCalls=0;
    NVMTLTexture *priv=tex(shared,7,NO), *vram=tex(shared,8,YES);
    cb=commitParts(@[@[priv],@[vram]]);
    assert(dirtyCalls==(half?1:0));
    assert([cb nvmtlAdvancePart] && seen(@"o7:0")==!half && !seen(mark(shared,0)));
    assert([cb nvmtlAdvancePart] && cb->_executionDone);
    if (half) assert(at(mark(shared,0)) < at(@"o7:0") && at(mark(shared,0)) < at(@"o8:1"));
    else assert(at(mark(shared,0)) > at(@"o8:0"));
    // 5. Shared, private, shared again on one surface: the shared texture keeps only its latest place, so the private
    // copy-out does not land after the final shared pixels.
    [events removeAllObjects];
    NVMTLTexture *again=tex(shared,9,YES), *between=tex(shared,10,NO);
    cb=commitParts(@[@[again],@[between],@[again]]);
    for (int k=0; k<3; k++) assert([cb nvmtlAdvancePart]);
    assert(cb->_executionDone && !cb->_cbError);
    if (half) assert(at(mark(shared,0)) < at(@"o10:0") && at(@"o10:0") < at(@"o9:1") && times(@"o9:1")==1);
    else assert(at(@"o9:0") < at(@"o10:0") && at(@"o10:0") < lastAt(@"o9:0") && lastAt(@"o9:0") < at(mark(shared,0)));
    assert(!nvmtl_ws_deferred.count && ![nvmtl_ws_pending_holds countForObject:@(IOSurfaceGetID(shared))]);
    // 6. With the marks off (any process but WindowServer, or an accelerator that does not take them) nothing is
    // marked, and every part publishes its surfaces as it lands.
    wsKernelWaits=NO; [events removeAllObjects]; dirtyCalls=0;
    cb=commitParts(@[@[swap],@[swap]]);
    assert([cb nvmtlAdvancePart] && !cb->_executionDone && [events isEqual:@[@"o1:0"]]);
    assert([cb nvmtlAdvancePart] && cb->_executionDone && [events isEqual:(@[@"o1:0",@"o1:0"])] && !dirtyCalls);
    CFRelease(shared); CFRelease(other);
    printf("PASS (kernel half %s): production submission ordering, failed-submit isolation, event publication, host-pointer "
           "hold / ahead finish, WindowServer marks: %s\n", half?"present":"absent",
           half?"held until every write in flight landed (same buffer, overlapping buffers, failed writer), released "
                "before publication, VRAM marked newest at commit and never again, private copies published as they land"
               :"released after publication at the end of each buffer, counted across overlapping buffers, nothing marked "
                "newest at commit");
} }
"""
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.m').write_text(fixture + helper + sync + checks)
    subprocess.run(['clang', '-fobjc-arc', '-framework', 'Foundation',
                    '-framework', 'Metal', str(path / 'test.m'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

    (path / 'submission.m').write_text(fixture + helper + sync + submission_fixture.replace('/*WS*/', ws) + prepare + ahead + advance + submission_checks)
    subprocess.run(['clang', '-fobjc-arc', '-framework', 'Foundation', '-framework', 'IOSurface',
                    '-framework', 'Metal', str(path / 'submission.m'), '-o', str(path / 'submission')], check=True)
    for half in ('1', '0'):
        env = dict(os.environ, KERNEL_HALF=half)
        env.pop('NVMTL_GSUB_HOSTPTR_LIFT', None)
        subprocess.run([str(path / 'submission')], check=True, env=env)
        env['NVMTL_GSUB_HOSTPTR_LIFT'] = '1'
        subprocess.run([str(path / 'submission')], check=True, env=env)
