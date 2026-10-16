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
// not WindowServer here: the kernel frame marks are never taken
static int nvmtl_vk_surface_gpu_mark(uint32_t sid,int pending){(void)sid;(void)pending;return -1;}
static void nvlog(const char *f,...){(void)f;}
struct __IOSurface {unsigned id,seed;};
static unsigned IOSurfaceGetID(IOSurfaceRef s){return s->id;}
static unsigned IOSurfaceGetSeed(IOSurfaceRef s){return s->seed;}
@class NVMTLCommandBuffer;
@interface NVMTLTexture:NSObject{@public IOSurfaceRef _surf; BOOL _vramOn;NSUInteger _plane;}@end
@implementation NVMTLTexture @end
@interface NVMTLCommandBuffer:NSObject{@public id _queue;NSArray *_parts;}@end
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
 puts("PASS: aliases, CPU seeds, queues, planes, private backing, latest writer, weak lifetime, 1000 cleanup cycles");
}return 0;}
'''
with tempfile.TemporaryDirectory() as d:
 p=pathlib.Path(d);(p/'test.m').write_text(fixture+helpers+checks)
 subprocess.run(['clang','-fobjc-arc','-framework','Foundation',str(p/'test.m'),'-o',str(p/'test')],check=True)
 subprocess.run([str(p/'test')],check=True)
