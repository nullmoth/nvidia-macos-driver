#!/usr/bin/env python3
"""Run the buffer destructor and Vulkan grace path with host-memory ownership checks."""
import argparse
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--baseline', nargs='?', const='HEAD', metavar='REF',
                    help='exercise source from this Git revision (default: HEAD)')
parser.add_argument('--lifetime-only', action='store_true')
args = parser.parse_args()

def source(path):
    if args.baseline:
        return subprocess.check_output(['git', 'show', f'{args.baseline}:{path}'], cwd=root, text=True)
    return (root / path).read_text()

def function(text, signature):
    start = text.index(signature)
    opening = text.index('{', start)
    depth, end = 1, opening + 1
    while depth:
        depth += (text[end] == '{') - (text[end] == '}')
        end += 1
    return text[start:end]

objects = source('plugin/NVMTLObjects.m')
vk = source('plugin/nvmtl_vk.c')
header = source('plugin/nvmtl_vk.h')
buffer_type = header[header.index('typedef struct { void *buf;'):header.index('} nvk_buffer;') + len('} nvk_buffer;')]
grace_type = vk[vk.index('typedef struct nvmtl_bgrace'):vk.index('static int nvmtl_buffer_grace_on')]
destructor = objects[objects.index('@implementation NVMTLBuffer'):objects.index('@end', objects.index('@implementation NVMTLBuffer'))]
destructor = function(destructor, '- (void)dealloc {')
release_helper = (function(objects, 'static void nvmtl_host_buffer_release(void *retained)')
                  if 'static void nvmtl_host_buffer_release(void *retained)' in objects else '')
backend = '\n'.join(function(vk, signature) for signature in (
    'static void nvmtl_life_abandon(void)\n{',
    'static uint64_t nvmtl_grace_seq_locked(void)\n{',
    'void nvmtl_vk_buffer_destroy(nvk_buffer *b)\n{',
    'static void nvmtl_buffer_grace_reap(void)\n{',
    'static void nvmtl_vk_buffer_destroy_body(nvk_buffer *b)\n{',
    'int nvmtl_vk_cmd_abandon(nvk_cmdbuf *c)\n{'))
fixture = r'''
#import <Foundation/Foundation.h>
#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include "nvmtl_page_life.h"
#include "nvmtl_shared_wc.h"
/*BUFFER_TYPE*/
/*GRACE_TYPE*/
typedef void *VkBuffer, *VkDeviceMemory;
typedef void *VkCommandBuffer;
typedef struct { void *cb; } nvk_cmdbuf;
static int g_state=1, grace=1, failAllocation;
static uint64_t g_cmd_submitted, g_cmd_finished_seq, g_buf_gone, g_alloc_bytes, g_bar1_bytes;
static uint64_t g_cmd_begun, g_cmd_abandoned;
static pthread_mutex_t g_grace_lock=PTHREAD_MUTEX_INITIALIZER;
static uint64_t g_bind_lost_bytes, g_bind_lost;
static void *g_dev;
static __thread int t_grace_reaping;
static unsigned destroyed, freed, callbacks;
enum { NVP_BUFDEL, NVMTL_FX_POOLED, NVMTL_FX_PLACED, NVMTL_LV_DYING };
#define nvlog(...) ((void)0)
#define nvtrace(...) ((void)0)
static int nvmtl_trace_on(void) { return 0; }
static uint64_t nvmtl_vk_buffer_address(nvk_buffer *b) { return 0; }
static uint64_t nvmtl_perf_now(void) { return 0; }
static void nvmtl_perf_note(int kind, uint64_t time, size_t bytes) {}
static int nvmtl_buffer_grace_on(void) { return grace; }
static void nvmtl_fx_note_buffer_gone(nvk_buffer *b, unsigned flags) {}
static void nvmtl_live_relabel(nvk_buffer *b, unsigned kind) {}
static void nvmtl_live_remove(nvk_buffer *b) {}
static void pool_buffer_release(nvk_buffer *b) {}
static void nvmtl_vk_budget_discharge(size_t n) {}
static void pvkDestroyBuffer(void *dev, VkBuffer b, void *allocator) { destroyed++; }
static void pvkFreeMemory(void *dev, VkDeviceMemory m, void *allocator) { freed++; }
static void unmap(void *dev, VkDeviceMemory m) {}
static void (*pvkUnmapMemory)(void *, VkDeviceMemory)=unmap;
static void nvmtl_vk_buffer_destroy_body(nvk_buffer *b);
static void nvmtl_hz_forget(VkCommandBuffer cb) {}
static void nvmtl_cmd_retire(nvk_cmdbuf *c) { c->cb=NULL; }
static void *test_calloc(size_t n,size_t s) { return failAllocation ? NULL : calloc(n,s); }
#define calloc test_calloc
/*BACKEND*/
#undef calloc
static void nvmtl_vk_buffer_retire(nvk_buffer *b) { nvmtl_vk_buffer_destroy(b); }
typedef struct { int img; } nvk_image;
typedef struct { nvk_image img; } Park;
static void nvmtl_vk_image_destroy(nvk_image *i) {}
@interface NVMTLHeap : NSObject
- (void)nvmtlGiveRange:(NSUInteger)o length:(NSUInteger)n;
- (void)nvmtlReleaseSubAllocation:(NSUInteger)n;
@end
@implementation NVMTLHeap
- (void)nvmtlGiveRange:(NSUInteger)o length:(NSUInteger)n {}
- (void)nvmtlReleaseSubAllocation:(NSUInteger)n {}
@end
@interface NVMTLBuffer : NSObject { @public
    nvk_buffer _b, _shadow; void *_hostPtr, *_swc; size_t _hostLen; id _hostDealloc;
    unsigned _nbpk; Park *_bpk; NVMTLHeap *_subHeap;
    NSUInteger _subCharge, _heapOffset; BOOL _subPlaced;
} @end
/*RELEASE_HELPER*/
@implementation NVMTLBuffer
/*DESTRUCTOR*/
@end
static void reset(void) {
    assert(!g_bg.ncool);
    g_state=1; grace=1; failAllocation=0;
    g_cmd_submitted=7; g_cmd_finished_seq=6;
    g_cmd_begun=7; g_cmd_abandoned=0;
    destroyed=freed=callbacks=0; g_alloc_bytes=1<<20;
}
static void releaseBuffer(unsigned char *cpu, BOOL imported, BOOL managed, BOOL complete, BOOL disabled, BOOL begun) {
    reset(); if (complete) g_cmd_finished_seq=7; if (disabled) grace=0;
    if (begun) g_cmd_begun=8;
    @autoreleasepool {
        NVMTLBuffer *b=[NVMTLBuffer new];
        b->_b=(nvk_buffer){.buf=(void *)1,.mem=(void *)2,.map=cpu,.size=32,.alloc=32,.imported=imported};
        if (managed) {
            b->_b.map=NULL; b->_b.imported=0;
            b->_shadow=(nvk_buffer){.buf=(void *)3,.mem=(void *)4,.map=cpu,.size=32,.alloc=32,.imported=1};
        } else if (!imported) { b->_b.map=cpu+16; b->_hostPtr=cpu; b->_hostLen=32; }
        const unsigned expectedFrees=managed ? 2 : 1;
        b->_hostDealloc=^(void *ptr, NSUInteger length) {
#ifndef TEST_LIFETIME_ONLY
            assert(ptr==cpu && length==32);
#endif
            // Imported storage cannot be freed until its VkDeviceMemory is gone.
            if (imported || managed) assert(freed>=1);
            callbacks++;
        };
        b=nil;
        if ((imported || managed) && !complete && !disabled) assert(callbacks==0);
        else assert(callbacks==1);
        if (!complete && !disabled) { assert(freed==0); nvmtl_buffer_grace_reap(); assert(freed==0); }
        g_cmd_finished_seq=7; nvmtl_buffer_grace_reap();
        if (begun) { assert(!callbacks && !destroyed && !freed); g_cmd_submitted=8; g_cmd_finished_seq=8; nvmtl_buffer_grace_reap(); }
        assert(callbacks==1 && destroyed==expectedFrees && freed==expectedFrees);
        nvmtl_buffer_grace_reap(); assert(callbacks==1);
    }
}
static void retireImported(unsigned char *cpu) {
    @autoreleasepool {
        NVMTLBuffer *b=[NVMTLBuffer new];
        b->_b=(nvk_buffer){.buf=(void *)1,.mem=(void *)2,.map=cpu,.size=32,.alloc=32,.imported=1};
        b->_hostDealloc=^(void *ptr,NSUInteger len) {
            // A caller's deallocator can reenter the driver: neither lock may be held here.
            pthread_mutex_lock(&g_grace_lock); pthread_mutex_unlock(&g_grace_lock);
            pthread_mutex_lock(&g_bg_lock); pthread_mutex_unlock(&g_bg_lock);
            assert(ptr==cpu && len==32 && freed>=1); callbacks++;
        };
        b=nil;
    }
}
static void cancellation(unsigned char *cpu) {
    // No new submission is needed after the final pending command is cancelled.
    reset(); g_cmd_finished_seq=7; g_cmd_begun=8;
    retireImported(cpu); assert(g_bg.ncool==1 && !callbacks);
    nvk_cmdbuf c={.cb=(void *)1};
    nvmtl_vk_cmd_abandon(&c);
    assert(!g_bg.ncool && callbacks==1 && freed==1);
    nvmtl_vk_cmd_abandon(&c); // repeated cancellation cannot count twice
    assert(g_cmd_abandoned==1 && callbacks==1);

    // Cancellation cannot bypass an already submitted GPU reader.
    reset(); g_cmd_begun=8;
    retireImported(cpu); c.cb=(void *)1; nvmtl_vk_cmd_abandon(&c);
    assert(g_bg.ncool==1 && !callbacks && !freed);
    g_cmd_finished_seq=7; nvmtl_buffer_grace_reap();
    assert(!g_bg.ncool && callbacks==1 && freed==1);

    // A newer, unrelated cancelled command must not remove the older pending reader.
    reset(); g_cmd_finished_seq=7; g_cmd_begun=8;
    retireImported(cpu);
    ++g_cmd_begun; c.cb=(void *)1; nvmtl_vk_cmd_abandon(&c);
    assert(g_bg.ncool==1 && !callbacks && !freed);
    ++g_cmd_submitted; nvmtl_buffer_grace_reap();
    assert(g_bg.ncool==1 && !callbacks && !freed);
    g_cmd_finished_seq=8; nvmtl_buffer_grace_reap();
    assert(!g_bg.ncool && callbacks==1 && freed==1);

    // Several pending readers all cancelled, including a buffer retired between cancellations.
    reset(); g_cmd_finished_seq=7; g_cmd_begun=10;
    retireImported(cpu);
    c.cb=(void *)1; nvmtl_vk_cmd_abandon(&c);
    retireImported(cpu);
    c.cb=(void *)1; nvmtl_vk_cmd_abandon(&c);
    assert(g_bg.ncool==2 && !callbacks && !freed);
    c.cb=(void *)1; nvmtl_vk_cmd_abandon(&c);
    assert(!g_bg.ncool && callbacks==2 && freed==2);
}
static pthread_mutex_t race_gate=PTHREAD_MUTEX_INITIALIZER;
static void *race_retire(void *cpu) {
    pthread_mutex_lock(&race_gate); pthread_mutex_unlock(&race_gate);
    retireImported(cpu); return NULL;
}
static void *race_cancel(void *unused) {
    pthread_mutex_lock(&race_gate); pthread_mutex_unlock(&race_gate);
    nvk_cmdbuf c={.cb=(void *)1}; nvmtl_vk_cmd_abandon(&c); return NULL;
}
static void cancellationRace(unsigned char *cpu) {
    for (unsigned i=0;i<256;i++) {
        reset(); g_cmd_finished_seq=7; g_cmd_begun=8;
        pthread_t retire,cancel;
        pthread_mutex_lock(&race_gate);
        assert(!pthread_create(&retire,NULL,race_retire,cpu));
        assert(!pthread_create(&cancel,NULL,race_cancel,NULL));
        pthread_mutex_unlock(&race_gate);
        assert(!pthread_join(retire,NULL) && !pthread_join(cancel,NULL));
        assert(!g_bg.ncool && callbacks==1 && freed==1 && g_cmd_abandoned==1);
    }
}
int main(void) { @autoreleasepool {
    unsigned char cpu[32]={0};
    releaseBuffer(cpu,YES,NO,NO,NO,NO);
    releaseBuffer(cpu,NO,YES,NO,NO,NO);
    releaseBuffer(cpu,NO,NO,NO,NO,NO);
    releaseBuffer(cpu,YES,NO,YES,NO,NO);
    releaseBuffer(cpu,YES,NO,NO,YES,NO);
    releaseBuffer(cpu,YES,NO,NO,NO,YES);
    cancellation(cpu);
    cancellationRace(cpu);
    // Allocation failure in the grace path must keep imported pages pinned rather than reuse them early.
    reset(); failAllocation=1;
    @autoreleasepool {
        NVMTLBuffer *b=[NVMTLBuffer new];
        b->_b=(nvk_buffer){.buf=(void *)1,.mem=(void *)2,.map=cpu,.size=32,.alloc=32,.imported=1};
        b->_hostDealloc=^(void *p,NSUInteger n) { callbacks++; };
        b=nil;
        assert(!destroyed && !freed && !callbacks);
    }
    puts("PASS: host callbacks preserve address/length and follow Vulkan free; pending submissions stay protected; cancellation drains without new submits, waits for other pending/in-flight readers, and runs callbacks once; allocation failure retains imported pages");
} }
'''
for token, value in {'BUFFER_TYPE':buffer_type, 'GRACE_TYPE':grace_type, 'BACKEND':backend,
                     'RELEASE_HELPER':release_helper, 'DESTRUCTOR':destructor}.items():
    fixture = fixture.replace(f'/*{token}*/', value)
with tempfile.TemporaryDirectory() as directory:
    p = pathlib.Path(directory)
    (p / 'release.m').write_text(fixture)
    subprocess.run(['clang','-fobjc-arc','-O1','-g','-fsanitize=address,undefined',
                    *(['-DTEST_LIFETIME_ONLY=1'] if args.lifetime_only else []),
                    '-I',str(root/'plugin'),str(p/'release.m'),'-framework','Foundation',
                    '-o',str(p/'release')],check=True)
    subprocess.run([str(p/'release')],check=True,timeout=20)
