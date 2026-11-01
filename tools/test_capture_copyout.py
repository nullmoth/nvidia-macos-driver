#!/usr/bin/env python3
"""Exercise production capture-copy recording and import ownership without a GPU."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
vk = (root / "plugin/nvmtl_vk.c").read_text()
start = vk.index("int nvmtl_vk_record_image_to_host(")
record = vk[start:vk.index("\nstatic void img_to(", start)]
start = vk.index("void nvmtl_vk_host_pages_drop(")
budget = vk[start:vk.index("\nstatic nvk_buffer *nvmtl_host_pages(", start)]
event = (root / "plugin/NVMTLEventSync.m").read_text()
start = event.index("static void nvmtl_ws_gpu_copyout(")
copyout = event[start:event.index("\n@interface NVMTLPendingSurfaceWrite", start)]
objects = (root / "plugin/NVMTLObjects.m").read_text()
start = objects.index("#if NVMTL_RELEASE", objects.index("static void nvmtl_conf_knobs(")) + len("#if NVMTL_RELEASE")
prune = "static void prune_release_environment(void) {\n" + objects[start:objects.index("#endif", start)] + "\n}\n"

c_fixture = r'''
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <stdatomic.h>
#include <pthread.h>
#include <mach/mach.h>
#include <vulkan/vulkan_core.h>
#include "nvmtl_vk.h"
static unsigned barriers, copies, uploads, assertions;
static VkImageLayout expectedLayout;
static VkBufferImageCopy seen;
static _Atomic uint64_t g_host_pages_kept;
static uint64_t cap;
static int importFails;
static uint64_t nvmtl_host_pages_cap(void) { return cap; }
int nvmtl_vk_buffer_import_host(void *base,size_t bytes,nvk_buffer *b) {
    if(importFails) return -1;
    *b=(nvk_buffer){.buf=base,.map=base,.size=bytes,.imported=1}; return 0;
}
void nvmtl_vk_buffer_destroy(nvk_buffer *b) { memset(b,0,sizeof *b); }
static uint32_t aspect = VK_IMAGE_ASPECT_COLOR_BIT;
static uint32_t nvmtl_copy_aspect(nvk_image *i) { (void)i; return aspect; }
void nvmtl_vk_image_upload_barrier(nvk_image *i) { (void)i; uploads++; }
static void pvkCmdPipelineBarrier(VkCommandBuffer c, VkPipelineStageFlags src, VkPipelineStageFlags dst,
    VkDependencyFlags flags, uint32_t nm, const VkMemoryBarrier *mb, uint32_t nb,
    const VkBufferMemoryBarrier *bb, uint32_t ni, const VkImageMemoryBarrier *ib) {
    (void)c; (void)flags; (void)nb; (void)bb;
    assert(ni == 1 && ib);
    if (!barriers) {
        assert(src == VK_PIPELINE_STAGE_ALL_COMMANDS_BIT && dst == VK_PIPELINE_STAGE_TRANSFER_BIT);
        assert(ib->oldLayout == expectedLayout && ib->newLayout == VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL);
        assert(ib->srcAccessMask & VK_ACCESS_MEMORY_WRITE_BIT);
    } else {
        assert(src == VK_PIPELINE_STAGE_TRANSFER_BIT && (dst & VK_PIPELINE_STAGE_HOST_BIT));
        assert(ib->oldLayout == VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL && ib->newLayout == expectedLayout);
        assert(nm == 1 && mb->srcAccessMask == VK_ACCESS_TRANSFER_WRITE_BIT && mb->dstAccessMask == VK_ACCESS_HOST_READ_BIT);
    }
    barriers++;
}
static void pvkCmdCopyImageToBuffer(VkCommandBuffer c, VkImage i, VkImageLayout l, VkBuffer b,
    uint32_t count, const VkBufferImageCopy *region) {
    (void)c; (void)i; (void)b;
    assert(barriers == 1 && l == VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL && count == 1);
    seen = *region; copies++;
}
'''
c_checks = r'''
static void *budget_worker(void *ignored) {
    (void)ignored;
    for(unsigned k=0;k<2000;k++) {
        nvk_buffer b={0};
        if(!nvmtl_vk_host_pages_hold((void *)4096,4096,&b)) {
            assert(atomic_load(&g_host_pages_kept)<=cap);
            nvmtl_vk_host_pages_drop(&b);
        }
    }
    return NULL;
}
static int run(nvk_cmdbuf *c, nvk_image *i, nvk_buffer *b, size_t off, size_t row) {
    barriers = copies = uploads = 0;
    if (i) expectedLayout = i->layout;
    int result = nvmtl_vk_record_image_to_host(c, i, b, off, row);
    if (!result) {
        assert(barriers == 2 && copies == 1 && uploads == 1 && i->layout == expectedLayout);
        assert(seen.bufferOffset == off && seen.bufferRowLength == row / i->bpp);
        assert(seen.imageExtent.width == i->w && seen.imageExtent.height == i->h);
    } else assert(!barriers && !copies);
    assertions++;
    return result;
}
int main(void) {
    nvk_buffer held[3]={{0}};
    cap=8192;
    assert(!nvmtl_vk_host_pages_hold((void *)4096,4096,&held[0]));
    assert(!nvmtl_vk_host_pages_hold((void *)8192,4096,&held[1]));
    assert(nvmtl_vk_host_pages_hold((void *)12288,4096,&held[2]));
    nvmtl_vk_host_pages_drop(&held[0]);
    assert(!nvmtl_vk_host_pages_hold((void *)12288,4096,&held[2]));
    nvmtl_vk_host_pages_drop(&held[1]);nvmtl_vk_host_pages_drop(&held[2]);
    assert(!atomic_load(&g_host_pages_kept));
    importFails=1;assert(nvmtl_vk_host_pages_hold((void *)4096,4096,&held[0]));
    assert(!atomic_load(&g_host_pages_kept));importFails=0;
    cap=0;assert(nvmtl_vk_host_pages_hold((void *)4096,4096,&held[0]));cap=8192;
    assert(nvmtl_vk_host_pages_hold((void *)4096,SIZE_MAX,&held[0]));
    pthread_t workers[4];
    for(unsigned k=0;k<4;k++) assert(!pthread_create(&workers[k],NULL,budget_worker,NULL));
    for(unsigned k=0;k<4;k++) assert(!pthread_join(workers[k],NULL));
    assert(!atomic_load(&g_host_pages_kept));
    puts("PASS: shared import budget, failure rollback, disabled cache, overflow and 8000 concurrent lease attempts");
    nvk_cmdbuf c = {.cb=(void *)1};
    nvk_buffer b = {.buf=(void *)2, .imported=1};
    nvk_image i = {.img=(void *)3, .layout=VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL};
    for (unsigned bpp=1; bpp<=4; bpp*=2) for(unsigned w=1; w<=96; w++) for(unsigned h=1; h<=16; h++)
        for(unsigned plane=0; plane<2; plane++) {
            i.w=w; i.h=h; i.bpp=bpp;
            size_t row=((w+3u)&~3u)*bpp, off=plane*4096u;
            b.size=off+(h-1)*row+w*bpp;
            assert(!run(&c,&i,&b,off,row)); // exact last-row bound, padding need not exist after the final texel
            b.size--; assert(run(&c,&i,&b,off,row));
        }
    i.w=8; i.h=8; i.bpp=2; b.size=4096;
    assert(run(&c,&i,&b,1,16)); assert(run(&c,&i,&b,0,0)); assert(run(&c,&i,&b,0,15));
    assert(run(&c,&i,&b,0,((size_t)UINT32_MAX+1)*2));
    assert(run(&c,&i,&b,SIZE_MAX-3,16)); assert(run(&c,&i,&b,4096,16));
    b.size=SIZE_MAX; i.h=UINT32_MAX;
    assert(run(&c,&i,&b,SIZE_MAX-4095,16));
    i.h=8; b.size=4096;
    #define REJECT(field,value) do { __typeof__(field) saved=(field); (field)=(value); assert(run(&c,&i,&b,0,16)); (field)=saved; }while(0)
    REJECT(c.in_rp,1); REJECT(c.cb,NULL); REJECT(i.img,NULL); REJECT(b.buf,NULL); REJECT(b.imported,0);
    REJECT(i.w,0); REJECT(i.h,0); REJECT(i.bpp,0); REJECT(i.samples,4); REJECT(i.bw,4); REJECT(i.bh,4);
    REJECT(i.mips,2); REJECT(i.layers,2); REJECT(i.layout,VK_IMAGE_LAYOUT_UNDEFINED);
    REJECT(i.layout,VK_IMAGE_LAYOUT_PREINITIALIZED); REJECT(aspect,VK_IMAGE_ASPECT_DEPTH_BIT);
    assert(run(NULL,&i,&b,0,16)); assert(run(&c,NULL,&b,0,16)); assert(run(&c,&i,NULL,0,16));
    i.layout=VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
    assert(!run(&c,&i,&b,0,16)); // an unchanged layout still needs a memory dependency
    printf("PASS: %u capture-copy bounds and barrier checks (ASan/UBSan)\n",assertions);
}
'''

objc_fixture = r'''
#import <Foundation/Foundation.h>
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <stdatomic.h>
#include "nvmtl_vk.h"
struct __IOSurface { uintptr_t base; size_t alloc, off, row; unsigned refs, planes; };
typedef struct __IOSurface *IOSurfaceRef;
static _Atomic unsigned imports, destroys, recordings;
static BOOL importFails, recordFails, useHost=YES;
static IOSurfaceRef kept;
size_t nvmtl_vk_host_import_align(void) { return 4096; }
static unsigned IOSurfaceGetPlaneCount(IOSurfaceRef s) { return s->planes; }
static unsigned IOSurfaceGetID(IOSurfaceRef s) { return (unsigned)(uintptr_t)s; }
static size_t IOSurfaceGetAllocSize(IOSurfaceRef s) { return s->alloc; }
static void *IOSurfaceGetBaseAddress(IOSurfaceRef s) { return (void *)s->base; }
static void *IOSurfaceGetBaseAddressOfPlane(IOSurfaceRef s,unsigned p) { (void)p; return (void *)(s->base+s->off); }
static size_t IOSurfaceGetBytesPerRowOfPlane(IOSurfaceRef s,unsigned p) { (void)p; return s->row; }
static void *keepSurface(IOSurfaceRef s) { s->refs++; return s; }
static void releaseSurface(IOSurfaceRef s) { assert(s->refs); s->refs--; }
#define CFRetain(s) keepSurface(s)
#define CFRelease(s) releaseSurface(s)
int nvmtl_vk_host_pages_hold(void *base,size_t bytes,nvk_buffer *b) {
    imports++;
    if(importFails || (uintptr_t)base%4096 || !bytes || bytes%4096) return -1;
    *b=(nvk_buffer){.buf=(void *)(uintptr_t)imports,.map=base,.size=bytes,.imported=1}; return 0;
}
void nvmtl_vk_host_pages_drop(nvk_buffer *b) { if(b->buf) destroys++; memset(b,0,sizeof *b); }
int nvmtl_vk_record_image_to_host(nvk_cmdbuf *c,nvk_image *i,const nvk_buffer *b,size_t off,size_t row) {
    (void)i; assert(c->cb && b->buf && b->size<=kept->alloc && off==kept->off && row==kept->row);
    recordings++; return recordFails ? -1 : 0;
}
@interface NVMTLTexture : NSObject {
@public NVMTLTexture *_parent; IOSurfaceRef _surf; BOOL _vramOn, uncopyable; NSUInteger _plane;
    nvk_image _i; id _capturePages, _hostPageLease; nvk_buffer _hostPages;
}
- (BOOL)nvmtlSurfaceCopyable;
@end
@implementation NVMTLTexture
- (BOOL)nvmtlSurfaceCopyable { return !uncopyable; }
@end
@interface NVMTLTexture (CaptureCopyout)
- (id)nvmtlRecordCaptureCopy:(nvk_cmdbuf *)commands;
@end
static id nvmtl_surface_sync(NVMTLTexture *t) { return t; }
static bool nvmtl_surfout_import(void) { return useHost; }
@interface NVMTLSubmissionPart : NSObject {
@public nvk_cmdbuf commands; NSSet *resources, *surfaces, *copiedSurfaces; NSArray *scratch; unsigned encoders;
}
@end
@implementation NVMTLSubmissionPart @end
@interface NVMTLCommandBuffer : NSObject { @public NSArray *_parts; BOOL _scheduleAfterPublish; }
@end
@implementation NVMTLCommandBuffer @end
'''
objc_checks = r'''
static int shared_pages_checks(void) {
    static struct __IOSurface s={.base=0x400000,.alloc=16384,.row=128,.planes=2};
    NVMTLTexture *a=[NVMTLTexture new], *b=[NVMTLTexture new]; a->_surf=&s; b->_surf=&s;
    setenv("NVMTL_SURFIN_PAGE_CACHE_EXTRA", "1", 1);
    prune_release_environment();
    assert(!getenv("NVMTL_SURFIN_PAGE_CACHE_EXTRA"));
    nvk_buffer *first=nvmtl_surface_host_pages(a,(void *)s.base,s.alloc);
    if(getenv("NVMTL_SURFIN_PAGE_CACHE") && !strcmp(getenv("NVMTL_SURFIN_PAGE_CACHE"),"0")) {
        assert(first==&a->_hostPages && !imports && !a->_hostPageLease);
        puts("PASS: shared page cache rollback switch"); return 0;
    }
    assert(first->buf && imports==1 && s.refs==1);
    assert(nvmtl_surface_host_pages(b,(void *)s.base,s.alloc)==first && imports==1);
    assert(nvmtl_surface_host_pages(b,(void *)(s.base+1),s.alloc)==&b->_hostPages);
    assert(nvmtl_surface_host_pages(b,(void *)s.base,s.alloc-1)==&b->_hostPages);
    __weak id lease=a->_hostPageLease;
    @autoreleasepool { a->_hostPageLease=nil; b->_hostPageLease=nil; a=nil; b=nil; }
    assert(lease && !destroys);
    for(int i=0;i<500 && lease;i++) usleep(10000);
    assert(!lease && destroys==1 && s.refs==0 && !nvmtl_page_cache.count);
    importFails=YES;
    NVMTLTexture *failed=[NVMTLTexture new]; failed->_surf=&s;
    assert(nvmtl_surface_host_pages(failed,(void *)s.base,s.alloc)==&failed->_hostPages);
    assert(!failed->_hostPageLease && !s.refs);
    puts("PASS: shared wrappers, alignment fallback, import failure and idle timer ownership");
    return 0;
}
static NVMTLSubmissionPart *part(NSSet *surfaces) {
    NVMTLSubmissionPart *p=[NVMTLSubmissionPart new]; p->surfaces=surfaces; p->commands.cb=(void *)1; p->encoders=1; return p;
}
static NVMTLCommandBuffer *buffer(NSArray *parts) {
    NVMTLCommandBuffer *b=[NVMTLCommandBuffer new]; b->_parts=parts; b->_scheduleAfterPublish=YES; return b;
}
int main(void) { @autoreleasepool {
    if(getenv("TEST_SHARED_PAGES")) return shared_pages_checks();
    setenv("NVMTL_GPU_COPYOUT_EXTRA","1",1);
    prune_release_environment();
    assert(getenv("NVMTL_GPU_COPYOUT") && !getenv("NVMTL_GPU_COPYOUT_EXTRA"));
    static struct __IOSurface s={.base=0x100000,.alloc=16384,.row=128,.planes=2}; kept=&s;
    NVMTLTexture *t=[NVMTLTexture new]; t->_surf=&s;
    NVMTLTexture *alias=[NVMTLTexture new]; alias->_parent=t;
    NVMTLSubmissionPart *p; NVMTLCommandBuffer *cb;
    @autoreleasepool {
        p=part([NSSet setWithObjects:t,alias,nil]); cb=buffer(@[p]); nvmtl_ws_gpu_copyout(cb);
    }
    if(getenv("NVMTL_GPU_COPYOUT") && !strcmp(getenv("NVMTL_GPU_COPYOUT"),"0")) {
        assert(!imports && !recordings && !p->copiedSurfaces.count);
        puts("PASS: release copy-out rollback switch"); return 0;
    }
    assert(imports==1 && recordings==1 && p->scratch.count==1 && p->copiedSurfaces.count==2);
    // An import replaced in the texture remains owned by the earlier recording, including a quarantined timeout.
    __weak id oldPages=t->_capturePages, newPages;
    NSMutableArray *quarantine=[NSMutableArray new]; [quarantine addObject:p];
    @autoreleasepool {
        s.base+=0x100000;
        NVMTLSubmissionPart *next=part([NSSet setWithObject:t]);
        nvmtl_ws_gpu_copyout(buffer(@[next])); newPages=t->_capturePages;
        assert(imports==2 && !destroys && oldPages && newPages && s.refs==2);
        t->_capturePages=nil;
    }
    @autoreleasepool { assert(oldPages && !newPages && destroys==1 && s.refs==1); }
    @autoreleasepool { cb=nil; p=nil; [quarantine removeAllObjects]; }
    assert(!oldPages && destroys==2 && !s.refs);
    // A final segment must never absorb an earlier event segment's surfaces or publish its copy early.
    NVMTLTexture *earlier=[NVMTLTexture new]; earlier->_surf=&s;
    NVMTLSubmissionPart *first=part([NSSet setWithObject:earlier]), *last=part([NSSet setWithObject:t]);
    NVMTLSubmissionPart *empty=part(nil); empty->encoders=0;
    unsigned n=recordings;
    nvmtl_ws_gpu_copyout(buffer(@[first,last,empty]));
    assert(recordings==n+1 && !first->copiedSurfaces.count && last->copiedSurfaces.count==1 && !empty->scratch.count);
    // Failed recording, unsupported surfaces, and disabled publication ordering retain the ordinary readback path.
    recordFails=YES; NVMTLSubmissionPart *failed=part([NSSet setWithObject:t]);
    nvmtl_ws_gpu_copyout(buffer(@[failed])); assert(!failed->copiedSurfaces.count && !failed->scratch.count); recordFails=NO;
    NVMTLCommandBuffer *unordered=buffer(@[part([NSSet setWithObject:t])]); unordered->_scheduleAfterPublish=NO;
    n=recordings; nvmtl_ws_gpu_copyout(unordered); assert(recordings==n);
    for(unsigned arm=0;arm<5;arm++) {
        s.planes=arm==0?1:2; t->_vramOn=arm==1; t->uncopyable=arm==2; useHost=arm!=3; s.off=arm==4?s.alloc:0;
        NVMTLSubmissionPart *fallback=part([NSSet setWithObject:t]);
        n=recordings; nvmtl_ws_gpu_copyout(buffer(@[fallback])); assert(recordings==n && !fallback->copiedSurfaces.count);
    }
    s.off=0; t->_capturePages=nil; importFails=YES;
    NVMTLSubmissionPart *unimportable=part([NSSet setWithObject:t]);
    nvmtl_ws_gpu_copyout(buffer(@[unimportable])); assert(!unimportable->scratch.count && !unimportable->copiedSurfaces.count);
    puts("PASS: import replacement, quarantine ownership, aliases, event segments, failed recording, import failure and readback fallback");
} }
'''

with tempfile.TemporaryDirectory() as directory:
    path = Path(directory)
    c = path / "copy.c"
    c.write_text(c_fixture + budget + record + c_checks)
    includes = ["-I" + str(root / "plugin"), "-I" + os.environ.get("VULKAN_HEADERS", str(Path.home() / "Vulkan-Headers/include"))]
    subprocess.run(["xcrun", "clang", "-std=c11", "-fsanitize=address,undefined", "-fno-omit-frame-pointer", *includes,
                    str(c), "-o", str(path / "copy")], check=True)
    subprocess.run([str(path / "copy")], check=True)
    m = path / "ownership.m"
    m.write_text(objc_fixture + prune + (root / "plugin/NVMTLCapture.m").read_text() + copyout + objc_checks)
    subprocess.run(["xcrun", "clang", "-fobjc-arc", "-fsanitize=address,undefined", "-fno-omit-frame-pointer", *includes,
                    "-framework", "Foundation", str(m), "-o", str(path / "ownership")], check=True)
    subprocess.run([str(path / "ownership")], check=True, env=dict(os.environ, NVMTL_GPU_COPYOUT="1"))
    subprocess.run([str(path / "ownership")], check=True, env=dict(os.environ, NVMTL_GPU_COPYOUT="0"))
    subprocess.run([str(path / "ownership")], check=True,
                   env=dict(os.environ, TEST_SHARED_PAGES="1", NVMTL_SURFIN_PAGE_CACHE="1"))
    subprocess.run([str(path / "ownership")], check=True,
                   env=dict(os.environ, TEST_SHARED_PAGES="1", NVMTL_SURFIN_PAGE_CACHE="0"))
