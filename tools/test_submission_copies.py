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
#include <stdatomic.h>
#include <time.h>
typedef struct { void *cb; } Commands;
typedef struct { int unused; } nvmtl_inflight;
@interface NVMTLSubmissionPart : NSObject { @public
    Commands commands; NSSet *resources; NSArray *surfaces, *scratch, *signals;
    id waitEvent, signalEvent; uint64_t value;
    BOOL waitFolded, aheadEmpty; int aheadFailed; unsigned encoders;
    nvmtl_inflight *inflight;
    NSArray<NVMTLBuffer *> *copyInputs; BOOL copyInputsPrepared, hasHostCopies;
} @end
@implementation NVMTLSubmissionPart @end
@interface NVMTLTexture (Publication)
- (BOOL)nvmtlSurfaceOut;
@end
@implementation NVMTLTexture (Publication)
- (BOOL)nvmtlSurfaceOut { return YES; }
@end
@interface NVMTLCommandBuffer : NSObject { @public
    BOOL _executionDone, _committed; NSError *_cbError;
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
@implementation NVMTLCommandBuffer
- (void)nvmtlMarkScheduled { scheduled++; }
- (void)nvmtlCollectDeviceTiming:(Commands *)commands {}
- (void)nvmtlFinishExecution:(NSError *)error { _cbError=error; _executionDone=YES; }
"""
submission_checks = r"""
@end
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
    puts("PASS: production submission ordering, failed-submit isolation, event publication, host-pointer hold / ahead finish");
} }
"""
with tempfile.TemporaryDirectory() as directory:
    path = pathlib.Path(directory)
    (path / 'test.m').write_text(fixture + helper + sync + checks)
    subprocess.run(['clang', '-fobjc-arc', '-framework', 'Foundation',
                    '-framework', 'Metal', str(path / 'test.m'), '-o', str(path / 'test')], check=True)
    subprocess.run([str(path / 'test')], check=True)

    (path / 'submission.m').write_text(fixture + helper + sync + submission_fixture + prepare + ahead + advance + submission_checks)
    subprocess.run(['clang', '-fobjc-arc', '-framework', 'Foundation',
                    '-framework', 'Metal', str(path / 'submission.m'), '-o', str(path / 'submission')], check=True)
    env = dict(os.environ)
    env.pop('NVMTL_GSUB_HOSTPTR_LIFT', None)
    subprocess.run([str(path / 'submission')], check=True, env=env)
    env['NVMTL_GSUB_HOSTPTR_LIFT'] = '1'
    subprocess.run([str(path / 'submission')], check=True, env=env)
