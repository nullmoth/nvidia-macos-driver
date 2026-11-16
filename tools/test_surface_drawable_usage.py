#!/usr/bin/env python3
"""Exercise production drawable policy and import fallback/cleanup without installing a driver."""
from pathlib import Path
import subprocess
import sys
import tempfile

root=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(root/'tools/rotation'))
from test_support import method
source=(root/'plugin/NVMTLDevice.m').read_text()
policy=method(source,'static BOOL nvmtl_surface_widen_usage(')+'\n'+method(source,'static NSUInteger nvmtl_surface_usage(')
image=method(source,'static int nvmtl_surf_wrap_whole(')+'\n'+method(source,'static int nvmtl_surface_image_create(')
fixture=r'''
#include <assert.h>
#include <stdint.h>
#include <string.h>
#include <stdio.h>
typedef unsigned BOOL;
typedef unsigned long NSUInteger;
#define YES 1
#define MTLResourceStorageModeShift 4
#define MTLStorageModeManaged 1
#define MTLTextureUsageUnknown 0
#define MTLTextureUsageShaderRead 1
#define MTLTextureUsageShaderWrite 2
#define MTLTextureUsageRenderTarget 4
static const char *program;
#define getprogname() program
@@POLICY@@
#define VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT 0x10u
typedef struct { int storage; unsigned usage; void *img; } nvk_image;
typedef struct { int allocated; } nvk_buffer;
typedef struct { uint64_t _vramBorn; nvk_buffer _vramBuf; BOOL _vramOn; uint32_t _surfSeed; BOOL _surfSynced; NSUInteger _ropt; nvk_image _surfaceLinear; nvk_image _i; nvk_buffer _hostPages; } NVMTLTexture;
typedef void *IOSurfaceRef;
static unsigned g_surf_vram,g_surf_private;
static unsigned image_drops,buffer_drops,private_requests,alias_requests,host_requests;
static int drop_storage,drop_render,alias_failure,private_failure,tag_current;
static int optimal_storage=1,import_failure,typed_fail_at;
static size_t surface_width=512,surface_element=4,surface_ew=1;
#define nvlog(...) ((void)0)
static unsigned IOSurfaceGetPlaneCount(IOSurfaceRef s) { return 0; }
static unsigned IOSurfaceGetBytesPerRow(IOSurfaceRef s) { return 2048; }
static size_t IOSurfaceGetWidth(IOSurfaceRef s) { return surface_width; }
static size_t IOSurfaceGetBytesPerElement(IOSurfaceRef s) { return surface_element; }
static size_t IOSurfaceGetElementWidth(IOSurfaceRef s) { return surface_ew; }
static unsigned IOSurfaceGetHeight(IOSurfaceRef s) { return 64; }
static unsigned IOSurfaceGetID(IOSurfaceRef s) { return 1; }
static unsigned IOSurfaceGetSeed(IOSurfaceRef s) { return 7; }
static int nvmtl_vk_surface_share_on(void) { return 1; }
static int nvmtl_vk_format_is_storage(uint32_t f) { return 1; }
static int nvmtl_vram_tag_current(IOSurfaceRef s, unsigned p, uint32_t seed) { return tag_current; }
static int nvmtl_vram_keep_get(int host,IOSurfaceRef s,NSUInteger p,size_t n,nvk_buffer *b,uint64_t *born) { if(host)host_requests++;b->allocated=!host&&!import_failure;if(born)*born=10;return host||import_failure?-1:0; }
static int nvmtl_vk_image_create_buffer_alias(uint32_t w,uint32_t h,uint32_t f,uint32_t bpp,int a8,int storage,int render,nvk_buffer *b,size_t offset,size_t row,nvk_image *out) { alias_requests++;if(alias_failure)return -1;assert(b->allocated);out->img=(void*)2;out->storage=storage&&!drop_storage;out->usage=render&&!drop_render?VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT:0;return 0; }
static void nvmtl_vk_image_destroy(nvk_image *i) { assert(i->img);image_drops++;memset(i,0,sizeof *i); }
static void nvmtl_vk_buffer_destroy(nvk_buffer *b) { assert(b->allocated);buffer_drops++;b->allocated=0; }
static void nvmtl_vram_keep_put(int host,IOSurfaceRef surf,NSUInteger plane,nvk_buffer *b,uint64_t born) { nvmtl_vk_buffer_destroy(b); }
static int nvmtl_vk_image_create_typed(uint32_t w,uint32_t h,uint32_t f,uint32_t bpp,int a8,unsigned mips,int storage,unsigned type,unsigned layers,nvk_image *out) { private_requests++;if(private_failure||typed_fail_at==(int)private_requests)return -1;out->img=(void*)3;out->storage=storage?optimal_storage:0;return 0; }
@@IMAGE@@
static void reset(void) { g_surf_vram=g_surf_private=image_drops=buffer_drops=private_requests=alias_requests=host_requests=0;drop_storage=drop_render=alias_failure=private_failure=tag_current=import_failure=typed_fail_at=0;optimal_storage=1;surface_width=512;surface_element=4;surface_ew=1; }
int main(int argc,char **argv) {
 assert(argc==2);program=argv[1];int ws=!strcmp(program,"WindowServer");
 assert(nvmtl_surface_usage(1,0,0)==(ws?1:5));
 assert(nvmtl_surface_usage(1,0,1)==(ws?1:7));
 assert(nvmtl_surface_usage(1,1,1)==1);
 assert(nvmtl_surface_usage(3,0,0)==(ws?3:7));
 assert(nvmtl_surface_usage(3,1,1)==3);
 for(unsigned fbo=0;fbo<2;fbo++)for(unsigned storage=0;storage<2;storage++)
  assert(nvmtl_surface_usage(MTLTextureUsageUnknown,fbo,storage)==(!ws&&!fbo&&storage?7:MTLTextureUsageUnknown));
 NVMTLTexture t={0};reset();
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,44,4,0,1));
 assert(t._vramOn && t._i.storage && alias_requests==1 && !private_requests && !buffer_drops && !image_drops);
 // Partial width/height must not import VRAM or mark untouched pixels current.
 for(unsigned shorter=0;shorter<2;shorter++) {
  memset(&t,0,sizeof t);reset();tag_current=1;
  assert(!nvmtl_surface_image_create(&t,(void*)1,0,shorter?512:256,shorter?32:64,44,4,0,1));
  assert(!t._vramOn && !t._surfaceLinear.img && !alias_requests && private_requests==1 && host_requests==1);
  assert(!t._surfSynced && !g_surf_vram && g_surf_private==1);
 }
 // Visible pixels, not row padding, determine coverage. Packed elements
 // round up without overflowing; invalid or unrepresentable layouts refuse.
 surface_width=511;assert(nvmtl_surf_wrap_whole((void*)1,511,64,4));
 surface_width=513;surface_ew=2;surface_element=8;
 assert(!nvmtl_surf_wrap_whole((void*)1,513,64,4));
 assert(nvmtl_surf_wrap_whole((void*)1,514,64,4));
 surface_width=SIZE_MAX;surface_ew=1;assert(!nvmtl_surf_wrap_whole((void*)1,512,64,4));
 surface_element=0;assert(!nvmtl_surf_wrap_whole((void*)1,512,64,4));
 // Unknown sRGB usage needs compatible UNORM storage writes. Preserve the
 // successful linear import and use the existing tiled publication bridge.
 // Native storage and compatible-view-only storage are both valid; neither
 // makes the fresh tiled pixels synchronized before the first import.
 for(int managed=0;managed<2;managed++) for(int storage=1;storage<=2;storage++) {
  memset(&t,0,sizeof t);reset();drop_storage=1;tag_current=1;optimal_storage=storage;
  if(managed)t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
  assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,50,4,0,1));
  assert(t._vramOn && t._i.storage==storage && t._i.img==(void*)3 && t._surfaceLinear.img==(void*)2);
  assert(!t._surfaceLinear.storage && t._vramBuf.allocated && t._vramBorn==10 && !t._surfSynced);
  assert(private_requests==1 && alias_requests==1 && !buffer_drops && !image_drops && !host_requests);
  assert(g_surf_vram==1 && !g_surf_private);
  // Compatible-view storage must not expose the base sRGB view as writable.
  const BOOL base_storage=t._i.storage==1;
  assert(nvmtl_surface_usage(MTLTextureUsageUnknown,0,base_storage)==(!ws&&base_storage?7:MTLTextureUsageUnknown));
 }
 memset(&t,0,sizeof t);reset();drop_storage=1;
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,44,4,0,0));
 assert(t._vramOn && !t._i.storage && !private_requests && !image_drops && !buffer_drops);
 // A genuine import refusal, including limited-BAR allocation pressure,
 // retains the original private/host-page fallback without a dangling alias.
 memset(&t,0,sizeof t);reset();import_failure=1;
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,50,4,0,1));
 assert(!t._vramOn && !t._surfaceLinear.img && !alias_requests && private_requests==1 && host_requests==1);
 assert(!image_drops && !buffer_drops && !g_surf_vram && g_surf_private==1);
 // A format may support storage without render targets. Both
 // initial writers and initial readers need one
 // renderable private backing, including when their descriptor is Unknown.
 for(unsigned storage=0;storage<2;storage++) {
  memset(&t,0,sizeof t);reset();drop_render=1;
  surface_width=128;surface_element=8;
  assert(!nvmtl_surface_image_create(&t,(void*)1,0,128,64,97,8,0,storage));
  assert(!t._vramOn && t._i.storage && alias_requests==1 && private_requests==1 && buffer_drops==1 && image_drops==1);
 }
 memset(&t,0,sizeof t);reset();alias_failure=1;
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,44,4,0,1));
 assert(!t._vramOn && t._i.storage && private_requests==1 && buffer_drops==1 && !image_drops);
 // A fresh Managed GPU image must not inherit the shared image's "synced"
 // state before importing its pixels. Its publication image stays owned.
 memset(&t,0,sizeof t);reset();tag_current=1;t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
 surface_width=128;surface_element=8;
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,128,64,97,8,0,1));
 assert(t._vramOn && t._i.img==(void*)3 && t._surfaceLinear.img==(void*)2 && !t._surfSynced);
 assert(private_requests==1 && !buffer_drops && !image_drops);
 // Plain read-only imports, including WindowServer's, retain direct VRAM.
 memset(&t,0,sizeof t);reset();tag_current=1;t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
 surface_width=128;surface_element=8;
 assert(!nvmtl_surface_image_create(&t,(void*)1,0,128,64,97,8,0,0));
 assert(t._vramOn && !t._surfaceLinear.img && t._surfSynced && !private_requests);
 for(int managed=0;managed<2;managed++) {
  // Tiled allocation failure releases the alias/import before private retry.
  memset(&t,0,sizeof t);reset();drop_storage=1;typed_fail_at=1;
  if(managed)t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
  assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,50,4,0,1));
  assert(!t._vramOn && !t._surfaceLinear.img && t._i.img==(void*)3);
  assert(private_requests==2 && buffer_drops==1 && image_drops==1 && host_requests==1);
  // Allocation can succeed but drop storage; reject it and release each
  // image and the import exactly once, keeping the old fallback semantics.
  memset(&t,0,sizeof t);reset();drop_storage=1;optimal_storage=0;
  if(managed)t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
  assert(!nvmtl_surface_image_create(&t,(void*)1,0,512,64,50,4,0,1));
  assert(!t._vramOn && !t._surfaceLinear.img && !t._i.storage);
  assert(private_requests==2 && buffer_drops==1 && image_drops==2 && host_requests==1);
  memset(&t,0,sizeof t);reset();drop_storage=1;private_failure=1;
  if(managed)t._ropt=MTLStorageModeManaged<<MTLResourceStorageModeShift;
  assert(nvmtl_surface_image_create(&t,(void*)1,0,512,64,50,4,0,1)==-1);
  assert(!t._vramOn && !t._i.img && !t._surfaceLinear.img && private_requests==2);
  assert(buffer_drops==1 && image_drops==1 && !host_requests);
 }
 puts("PASS: app/WindowServer and FBO policy, sRGB/native storage bridges, direct readers, private fallback and exact cleanup");
}
'''.replace('@@POLICY@@',policy).replace('@@IMAGE@@',image)
with tempfile.TemporaryDirectory() as d:
 p=Path(d);(p/'test.c').write_text(fixture)
 subprocess.run(['clang','-std=c11','-O1','-fsanitize=address,undefined',str(p/'test.c'),'-o',str(p/'test')],check=True)
 for process in ('Photo Booth','WindowServer'):
  subprocess.run([str(p/'test'),process],check=True)
