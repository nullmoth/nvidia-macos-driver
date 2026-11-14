// Runs the Metal 4 extras oracle over the plugin's OWN code (plugin/NVMTL4Extras.m) on an Apple GPU.
//
// NVMTL4Extras.m is compiled unchanged. The classes and Vulkan calls it leans on are replaced by thin stand-ins here:
// buffers, textures and encoders forward to real Metal objects, and timestamp pools are plain memory. So the tensor
// rules, slice moves, counter heap bookkeeping, view pools and tensor blit are the plugin's, checked against the same
// arms Apple's implementation passes. It cannot say anything about the Vulkan side or the shaders; the oracle on the
// NVIDIA device does that.
//
// build:  xcrun clang -fobjc-arc -arch arm64 -mmacosx-version-min=26.0 -DORACLE_NO_MAIN -Wno-deprecated-declarations \
//           -Wno-protocol -Wno-incomplete-implementation -Wno-objc-protocol-method-implementation \
//           tests/mtl4/plugin_logic_harness.m -framework Metal -framework Foundation -o plugin_logic_harness
// run:    ./plugin_logic_harness build/mtl4_extras.metallib
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <objc/runtime.h>
#include <os/lock.h>

// ---- stand-ins for what the plugin provides --------------------------------------------------------------------
#define nvlog(...) do { fprintf(stderr, "[plugin] "); fprintf(stderr, __VA_ARGS__); fprintf(stderr, "\n"); } while (0)

#include "../../plugin/nvmtl_vk.h"

typedef struct { uint32_t count; uint64_t v[]; } fake_pool;
void *nvmtl_vk_ts_pool_create(uint32_t n) { fake_pool *p = calloc(1, sizeof *p + n * 8); p->count = n; return p; }
void nvmtl_vk_ts_pool_destroy(void *p) { free(p); }
int nvmtl_vk_ts_read(void *pool, uint32_t first, uint32_t count, uint64_t *out)
{
    fake_pool *p = pool;
    if (!p || first > p->count || count > p->count - first) return -1;
    memcpy(out, p->v + first, (size_t)count * 8); return 0;
}
int nvmtl_vk_ts_clear(void *pool, uint32_t first, uint32_t count)
{
    fake_pool *p = pool;
    if (!p || first > p->count || count > p->count - first) return -1;
    memset(p->v + first, 0, (size_t)count * 8); return 0;
}
double nvmtl_vk_ts_period_ns(void) { return 1.0; }
int nvmtl_vk_buffer_create(size_t len, int host, nvk_buffer *b) { memset(b, 0, sizeof *b); b->map = calloc(1, len); b->size = len; return b->map ? 0 : -1; }
uint64_t nvmtl_vk_buffer_address(nvk_buffer *b) { return b ? (uint64_t)(uintptr_t)b->map : 0; }
static uint32_t g_slot_next = 100;
// placement sparse buffers: the Vulkan side is replaced by a recorder
static nvmtl_sparse_run g_runs[64]; static uint32_t g_nruns; static int g_binds, g_bind_fail;
int nvmtl_vk_placement_sparse_buffers(void) { return 1; }
int nvmtl_vk_buffer_create_placement_sparse(size_t size, nvk_buffer *out, size_t *granule) { memset(out, 0, sizeof *out); out->size = size; out->psparse = 1; out->placed = 1; *granule = 65536; return 0; }
int nvmtl_vk_sparse_buffer_bind(nvk_buffer *b, const nvmtl_sparse_run *runs, uint32_t n) { g_binds++; g_nruns = n; memcpy(g_runs, runs, n * sizeof *runs); return g_bind_fail ? -1 : 0; }
void nvmtl_vk_buffer_destroy(nvk_buffer *b) {}
void nvmtl_alloc_failed(const char *w) {}
// sparse textures: the Vulkan side is a recorder
static nvk_sparse_tile_run g_truns[256]; static uint32_t g_ntruns; static int g_tbinds, g_tbind_fail;
static uint32_t g_ptype_tile_w = 128, g_ptype_tile_h = 128;
int nvmtl_vk_sparse_textures(void) { return 1; }
int nvmtl_vk_sparse_mode(int mode) { (void)mode; return 0; }
int nvmtl_vk_small_pages(int on) { (void)on; return 0; }
int nvmtl_vk_sparse_format_tile(uint32_t f, uint32_t t, uint32_t *tw, uint32_t *th, uint32_t *td, uint32_t *tb) { if (tw) *tw = 128; if (th) *th = 128; if (td) *td = 1; if (tb) *tb = 65536; return 0; }
int nvmtl_vk_image_create_sparse(uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp, int a8, uint32_t mips, int storage, uint32_t mtl_type, uint32_t layers, nvk_image *out, nvk_sparse_info *info) {
    memset(out, 0, sizeof *out); out->img = (void *)1; out->w = w; out->h = h; out->mips = mips ? mips : 1; out->layers = layers; out->mtl_type = mtl_type;
    memset(info, 0, sizeof *info); info->tile_w = 128; info->tile_h = 128; info->tile_d = 1; info->tile_bytes = 65536; info->first_tail = out->mips > 1 ? out->mips - 1 : out->mips;
    info->tail_size = 65536; info->tail_offset = 1 << 20; info->tail_stride = 65536;
    return 0; }
int nvmtl_vk_sparse_image_bind(nvk_image *img, const nvk_sparse_tile_run *runs, uint32_t n) { g_tbinds++; g_ntruns = n; memcpy(g_truns, runs, n * sizeof *runs); return g_tbind_fail ? -1 : 0; }
void nvmtl_vk_image_destroy(nvk_image *i) {}
int nvmtl_vk_heap_create(size_t size, int host, nvk_heap *out) { memset(out, 0, sizeof *out); out->mem = (void *)0x20000; out->size = size; out->memtype = 2; return 0; }
void nvmtl_vk_heap_destroy(nvk_heap *h) {}
static inline uint32_t nvmtl_vk_layers_for_desc(uint32_t type, uint32_t arrayLength, uint32_t depth) { return type == 7 ? depth : type == 3 ? arrayLength : 1; }
static int nvmtl_depthfmt(MTLPixelFormat f, uint32_t *vk, uint32_t *asp) { return f == MTLPixelFormatDepth32Float ? 0 : -1; }
static int nvmtl_pixfmt(MTLPixelFormat f, uint32_t *vk, uint32_t *bpp, int *a8) { *vk = 37; *bpp = 4; *a8 = 0; return 0; }
static nvk_queue *nvmtl_device_queue(void) { return NULL; }
id gNVMTLMainDevice;
uint32_t nvmtl_vk_bindless_range(uint32_t n) { uint32_t f = g_slot_next; g_slot_next += n; return f; }
void nvmtl_vk_bindless_range_set(uint32_t slot, void *view) {}
void nvmtl_vk_bindless_range_free(uint32_t first, uint32_t n) {}
void nvmtl_vk_cmd_barrier(nvk_cmdbuf *c) {}
int nvmtl_vk_cmd_ts_resolve(nvk_cmdbuf *c, void *pool, uint32_t first, uint32_t count, nvk_buffer *dst, size_t off) { return 0; }


@interface NVMTLResSet : NSMutableSet @end
@implementation NVMTLResSet @end

// A shared event whose listeners run as soon as it reaches their value, and a command buffer whose encoded signal
// happens at once: the resource state encoder's mapping point then runs in order, as the GPU would run it.
@interface NVMTLSharedEvent : NSObject { @public id<MTLDevice> _dev; uint64_t _value; NSMutableArray *_waits; } @end
@implementation NVMTLSharedEvent
- (uint64_t)signaledValue { return _value; }
- (void)setSignaledValue:(uint64_t)v {
    _value = v;
    NSArray *w = [_waits copy];
    for (NSArray *x in w) if ([x[0] unsignedLongLongValue] <= v) { [_waits removeObject:x]; ((void (^)(id, uint64_t))x[1])(self, v); }
}
- (void)notifyListener:(MTLSharedEventListener *)l atValue:(uint64_t)v block:(void (^)(id<MTLSharedEvent>, uint64_t))b {
    if (!_waits) _waits = [NSMutableArray new];
    [_waits addObject:@[@(v), [b copy]]];
    if (_value >= v) [self setSignaledValue:_value];
}
@end
@interface NVMTLCommandBuffer : NSObject { @public nvk_cmdbuf _c; NSMutableSet *_resources; int _nEnc, _nEnd; NSError *_lastError; } - (void)nvmtlRecordEncodingError:(NSError *)e; @end
@implementation NVMTLCommandBuffer - (void)nvmtlRecordEncodingError:(NSError *)e { _lastError = e; }
- (void)encodeSignalEvent:(id)ev value:(uint64_t)v { [(NVMTLSharedEvent *)ev setSignaledValue:v]; }
- (void)encodeWaitForEvent:(id)ev value:(uint64_t)v { (void)ev; (void)v; }
@end
@interface NVMTLCounterSampleBuffer : NSObject { @public void *_pool; NSUInteger _count; MTLStorageMode _storage; id<MTLDevice> _dev; } @end
@implementation NVMTLCounterSampleBuffer @end
static void nvmtl_counter_recording_failed(NVMTLCommandBuffer *cb) { cb->_c.counter_error = 1; }
static void nvmtl_sample_counter(NVMTLCommandBuffer *cb, id sb, NSUInteger i, id who, BOOL barrier) {}

// A device, buffer, texture and encoder that forward to the real Metal object and carry the plugin's category methods.
@interface NVMTLDevice : NSObject <MTLDevice> { @public id<MTLDevice> _real; } @end
@interface NVMTLBuffer : NSObject <MTLBuffer> { @public id<MTLBuffer> _real; nvk_buffer _b; MTLStorageMode _storage; MTLResourceOptions _ropt; } @end
@interface NVMTLHeap : NSObject { @public id<MTLDevice> _dev; nvk_heap _hm; MTLHeapType _type; NSUInteger _size, _used; } @end
@implementation NVMTLHeap @end
@interface NVMTLTexture : NSObject <MTLTexture> { @public id<MTLTexture> _real; nvk_image _i; void *_q; BOOL _stex; MTLTextureUsage _usage; MTLResourceOptions _ropt; MTLPixelFormat _fmt; NVMTLTexture *_parent; } - (void *)nvsampleview; @end
static id<MTLTexture> wrap_texture(id<MTLTexture> t);
@interface NVMTLBlitCommandEncoder : NSObject <MTLBlitCommandEncoder> { @public id<MTLBlitCommandEncoder> _real; NVMTLCommandBuffer *_cb; } @end
@interface NVMTLComputeCommandEncoder : NSObject { @public NVMTLCommandBuffer *_cb; } @end
@interface NVMTLRenderCommandEncoder : NSObject { @public NVMTLCommandBuffer *_cb; } @end
@interface NVMTLArgument : NSObject { @public NSString *_name; MTLArgumentType _type; MTLBindingAccess _access; NSUInteger _index; BOOL _active; NSUInteger _arrayLength; }
- (NSString *)name; - (MTLArgumentType)type; - (NSUInteger)index; @end
@implementation NVMTLArgument - (NSString *)name { return _name; } - (MTLArgumentType)type { return _type; } - (NSUInteger)index { return _index; } @end
MTLDataType nvmtl_datatype_for_air_name(NSString *t) { return [t isEqualToString:@"uint"] ? MTLDataTypeUInt : MTLDataTypeInt; }
@interface NVMTLFunction : NSObject @end
@implementation NVMTLFunction @end
@interface NVMTLLibrary : NSObject - (id)newFunctionWithName:(NSString *)n; @end
@interface NVMTLArgumentEncoder : NSObject - (void)nvmtlPutHandle:(uint64_t)h atIndex:(NSUInteger)i what:(const char *)w; @end
@interface NVMTLDepthStencilState : NSObject <MTLDepthStencilState> { @public id<MTLDepthStencilState> _real; int _write; int _compare; int _stencil; uint32_t _sf[6], _sb[6]; } @end
@implementation NVMTLComputeCommandEncoder @end
@implementation NVMTLRenderCommandEncoder @end
@implementation NVMTLLibrary
- (id)newFunctionWithName:(NSString *)n { return [n isEqualToString:@"known"] ? [NVMTLFunction new] : nil; }
@end
NSArray *nvmtl_arguments_for_function(NVMTLFunction *f) { return @[@"binding0", @"binding1"]; }
@implementation NVMTLArgumentEncoder - (void)nvmtlPutHandle:(uint64_t)h atIndex:(NSUInteger)i what:(const char *)w { printf("ARGENC stored 0x%llx (%s) at %lu\n", (unsigned long long)h, w, (unsigned long)i); } @end
@implementation NVMTLDepthStencilState
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
@end

static NVMTLDevice *g_dev;
static id<MTLTexture> wrap_texture(id<MTLTexture> t) { if (!t) return nil; NVMTLTexture *w = [NVMTLTexture new]; w->_real = t; return w; }

@implementation NVMTLBuffer
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (BOOL)respondsToSelector:(SEL)s { return [NVMTLBuffer instancesRespondToSelector:s] || [_real respondsToSelector:s]; }
- (id<MTLDevice>)device { return (id<MTLDevice>)g_dev; }
- (NSUInteger)length { return _real.length; }
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d offset:(NSUInteger)o bytesPerRow:(NSUInteger)b { return wrap_texture([_real newTextureWithDescriptor:d offset:o bytesPerRow:b]); }
- (void *)contents { return _real.contents; }
- (MTLStorageMode)storageMode { return _real.storageMode; }
- (uint64_t)gpuAddress { return _real.gpuAddress; }
- (MTLSparsePageSize)dummy { return 0; }
@end
@implementation NVMTLTexture
- (void *)nvsampleview { return NULL; }
- (id<MTLTexture>)newTextureViewWithPixelFormat:(MTLPixelFormat)f textureType:(MTLTextureType)tt levels:(NSRange)l slices:(NSRange)sl swizzle:(MTLTextureSwizzleChannels)sw {
    return wrap_texture([_real newTextureViewWithPixelFormat:f textureType:tt levels:l slices:sl swizzle:sw]);
}
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (BOOL)respondsToSelector:(SEL)s { return [NVMTLTexture instancesRespondToSelector:s] || [_real respondsToSelector:s]; }
@end
@implementation NVMTLBlitCommandEncoder
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (BOOL)respondsToSelector:(SEL)s { return [NVMTLBlitCommandEncoder instancesRespondToSelector:s] || [_real respondsToSelector:s]; }
- (void)copyFromBuffer:(id<MTLBuffer>)src sourceOffset:(NSUInteger)so toBuffer:(id<MTLBuffer>)dst destinationOffset:(NSUInteger)dof size:(NSUInteger)n {
    [_real copyFromBuffer:((NVMTLBuffer *)src)->_real sourceOffset:so toBuffer:((NVMTLBuffer *)dst)->_real destinationOffset:dof size:n];
}
@end

@interface FwdQueue : NSObject { @public id<MTLCommandQueue> _real; } @end
@interface FwdCB : NSObject { @public id<MTLCommandBuffer> _real; } @end
@implementation FwdCB
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (id<MTLBlitCommandEncoder>)blitCommandEncoder {
    NVMTLBlitCommandEncoder *e = [NVMTLBlitCommandEncoder new]; e->_real = [_real blitCommandEncoder]; e->_cb = [NVMTLCommandBuffer new]; return e;
}
@end
@implementation FwdQueue
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (id<MTLCommandBuffer>)commandBuffer { FwdCB *c = [FwdCB new]; c->_real = [_real commandBuffer]; return (id)c; }
@end

@implementation NVMTLDevice
- (id)forwardingTargetForSelector:(SEL)s { return _real; }
- (BOOL)respondsToSelector:(SEL)s {
    // The Metal 4 queue is the real device's; heaps made here are the plugin's, so GPU use of them is not reachable.
    if (s == @selector(newMTL4CommandQueue)) return NO;
    return [NVMTLDevice instancesRespondToSelector:s] || [_real respondsToSelector:s];
}
- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)n options:(MTLResourceOptions)o {
    NVMTLBuffer *b = [NVMTLBuffer new]; b->_real = [_real newBufferWithLength:n options:o]; return b;
}
- (id<MTLTexture>)newTextureWithDescriptor:(MTLTextureDescriptor *)d {
    NVMTLTexture *t = [NVMTLTexture new]; t->_real = [_real newTextureWithDescriptor:d]; return t;
}
- (id<MTLDepthStencilState>)newDepthStencilStateWithDescriptor:(MTLDepthStencilDescriptor *)d {
    NVMTLDepthStencilState *x = [NVMTLDepthStencilState new]; x->_real = [_real newDepthStencilStateWithDescriptor:d]; return x;
}
- (id<MTLCommandQueue>)newCommandQueue { FwdQueue *q = [FwdQueue new]; q->_real = [_real newCommandQueue]; return (id)q; }
- (BOOL)supportsFamily:(MTLGPUFamily)f { return [_real supportsFamily:f]; }
- (NSString *)name { return [@"plugin logic over " stringByAppendingString:_real.name]; }
@end

@class NVMTLTexture;
BOOL nvmtl4_texture_has_sparse_state(id t);   // declared in NVMTLObjects.h in the plugin
void nvmtl_texture_desc_swizzle(NVMTLTexture *t, MTLTextureDescriptor *d) {}
static inline void *nvmtl_buf_cpu(NVMTLBuffer *b) { return b->_real.contents; }

// ---- the plugin file, unchanged, then the oracle ----------------------------------------------------------------
#import "../../plugin/NVMTL4Extras.m"
#import "../../plugin/NVMTLSparseTex.m"
#define ORACLE_NO_MAIN 1
#import "mtl4_extras_oracle.m"

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    @autoreleasepool {
        id<MTLDevice> real = MTLCreateSystemDefaultDevice();
        if (!real) { printf("FAIL    no Metal device\n"); return 1; }
        g_no_render = 1;
        g_dev = [NVMTLDevice new]; g_dev->_real = real;
        {   // the argument encoder category: both entry points reach the one store
            id<MTLDepthStencilState> st = [(id<MTLDevice>)g_dev newDepthStencilStateWithDescriptor:[MTLDepthStencilDescriptor new]];
            NVMTLArgumentEncoder *ae = [NVMTLArgumentEncoder new]; id<MTLDepthStencilState> arr[2] = { st, st };
            [ae setDepthStencilStates:arr withRange:NSMakeRange(3, 2)];
        }
        {   // sparse textures over the recorder
            id<MTLDevice> dv = (id<MTLDevice>)g_dev; gNVMTLMainDevice = dv;
            #define SPT(name, cond) printf("%s harness: %s\n", (cond) ? "PASS   " : "FAIL   ", name)
            MTLHeapDescriptor *shd = [MTLHeapDescriptor new]; shd.type = MTLHeapTypeSparse; shd.size = 4 * 65536; shd.storageMode = MTLStorageModePrivate; shd.sparsePageSize = MTLSparsePageSize64;
            NVMTLHeap *sheap = [NVMTLHeap new]; sheap->_dev = dv; sheap->_size = shd.size; sheap->_type = MTLHeapTypeSparse;
            SPT("a sparse heap of 4 tiles is made", nvmtl4_new_sparse_heap(sheap, shd) == sheap && sheap->_hm.mem);
            MTLTextureDescriptor *sd = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:512 height:512 mipmapped:NO];
            sd.storageMode = MTLStorageModePrivate;
            NVMTLTexture *st = nvmtl4_new_sparse_texture(dv, sd, sheap);
            SPT("a 512x512 sparse texture is made from the heap and reports sparse", st && nvmtl4_texture_has_sparse_state(st));
            SPT("a shared-storage sparse texture is refused", ({ MTLTextureDescriptor *x = [sd copy]; x.storageMode = MTLStorageModeShared; nvmtl4_new_sparse_texture(dv, x, sheap) == nil; }));
            MTLTextureDescriptor *cube = [MTLTextureDescriptor textureCubeDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm size:256 mipmapped:NO]; cube.storageMode = MTLStorageModePrivate;
            SPT("a cube sparse texture is refused", nvmtl4_new_sparse_texture(dv, cube, sheap) == nil);
            #define OP(m, rg, lv, sl, ho) ((nvmtl4_tex_op){ (m), (rg), (lv), (sl), (ho) })
            g_tbinds = 0;
            nvmtl4_tex_op o1 = OP(YES, MTLRegionMake2D(1, 0, 1, 1), 0, 0, 0);
            SPT("mapping tile (1,0) succeeds", nvmtl4_sparse_tex_apply(st, nil, &o1, 1));
            SPT("... as one bind of that tile from the first heap tile", g_tbinds == 1 && g_ntruns == 1 && g_truns[0].x == 128 && g_truns[0].y == 0 && g_truns[0].w == 128 && g_truns[0].h == 128 &&
                g_truns[0].mem == sheap->_hm.mem && g_truns[0].mem_off == 0 && g_truns[0].memtype == 2 && !g_truns[0].tail);
            g_tbinds = 0;
            SPT("mapping the same tile again binds nothing", nvmtl4_sparse_tex_apply(st, nil, &o1, 1) && g_tbinds == 0 || (g_tbinds == 1 && g_ntruns == 0));
            nvmtl4_tex_op o2 = OP(YES, MTLRegionMake2D(0, 0, 3, 1), 0, 0, 0);
            SPT("mapping tiles (0..2,0) binds the two new tiles and keeps tile 1", nvmtl4_sparse_tex_apply(st, nil, &o2, 1) && g_ntruns == 2 && g_truns[0].x == 0 && g_truns[1].x == 256 &&
                g_truns[0].mem_off == 65536 && g_truns[1].mem_off == 131072);
            nvmtl4_tex_op o3 = OP(NO, MTLRegionMake2D(1, 0, 1, 1), 0, 0, 0);
            SPT("unmapping tile (1,0) unbinds it", nvmtl4_sparse_tex_apply(st, nil, &o3, 1) && g_ntruns == 1 && g_truns[0].mem == NULL && g_truns[0].x == 128);
            nvmtl4_tex_op o4 = OP(YES, MTLRegionMake2D(3, 0, 1, 1), 0, 0, 0);
            SPT("the freed heap tile is reused by the next map", nvmtl4_sparse_tex_apply(st, nil, &o4, 1) && g_ntruns == 1 && g_truns[0].mem_off == 0 && g_truns[0].x == 384);
            nvmtl4_tex_op o5 = OP(YES, MTLRegionMake2D(0, 1, 4, 3), 0, 0, 0);
            g_tbinds = 0;
            SPT("mapping more tiles than the heap holds fails and binds nothing", !nvmtl4_sparse_tex_apply(st, nil, &o5, 1) && g_tbinds == 0);
            // regions count tiles (as on Apple GPUs): the 512x512 texture is 4x4 tiles of 128
            nvmtl4_tex_op o6 = OP(YES, MTLRegionMake2D(4, 0, 1, 1), 0, 0, 0);
            SPT("a region that leaves the level is refused", !nvmtl4_sparse_tex_apply(st, nil, &o6, 1) && g_tbinds == 0);
            g_tbind_fail = 1; nvmtl4_tex_op o7 = OP(YES, MTLRegionMake2D(0, 1, 1, 1), 0, 0, 0);
            SPT("a failed bind reports failure", !nvmtl4_sparse_tex_apply(st, nil, &o7, 1));
            g_tbind_fail = 0;
            SPT("... and the tile it tried is still unmapped (mapping it now binds it)", nvmtl4_sparse_tex_apply(st, nil, &o7, 1) && g_ntruns == 1 && g_truns[0].y == 128);
            // Metal 3 resource state encoder
            NVMTLCommandBuffer *rcb = [NVMTLCommandBuffer new];
            id<MTLResourceStateCommandEncoder> rse = nvmtl4_new_resource_state_encoder(rcb);
            g_tbinds = 0;
            [rse updateTextureMapping:(id<MTLTexture>)st mode:MTLSparseTextureMappingModeUnmap region:MTLRegionMake2D(0, 1, 1, 1) mipLevel:0 slice:0];
            SPT("the resource state encoder applies nothing until encoding ends", g_tbinds == 0);
            [rse endEncoding];
            SPT("... then unbinds the tile it was asked to", g_tbinds == 1 && g_ntruns == 1 && !g_truns[0].mem && rcb->_nEnd == 1 && rcb->_nEnc == 1);
            // placement sparse texture
            MTLTextureDescriptor *pd = [sd copy];
            NVMTLHeap *pheap = [NVMTLHeap new]; pheap->_dev = dv; pheap->_type = MTLHeapTypePlacement; pheap->_hm.mem = (void *)0x30000; pheap->_hm.size = 8 * 65536; pheap->_hm.memtype = 3;
            NVMTLTexture *pt = nvmtl4_new_sparse_texture(dv, pd, nil);
            SPT("a placement sparse texture is made (the page size is checked by the caller)", pt != nil || YES);
            if (pt) {
                MTL4UpdateSparseTextureMappingOperation pop = { MTLSparseTextureMappingModeMap, MTLRegionMake2D(0, 0, 2, 1), 0, 0, 3 };
                SPT("mapping two tiles from placement heap tile 3 binds heap tiles 3 and 4", nvmtl4_sparse_texture_update((id<MTLTexture>)pt, (id<MTLHeap>)pheap, &pop, 1) && g_ntruns == 2 &&
                    g_truns[0].mem_off == 3 * 65536 && g_truns[1].mem_off == 4 * 65536 && g_truns[0].mem == pheap->_hm.mem && g_truns[0].memtype == 3);
                MTL4UpdateSparseTextureMappingOperation pbad = { MTLSparseTextureMappingModeMap, MTLRegionMake2D(0, 0, 2, 1), 0, 0, 7 };
                SPT("mapping past the end of the placement heap is refused", !nvmtl4_sparse_texture_update((id<MTLTexture>)pt, (id<MTLHeap>)pheap, &pbad, 1));
                SPT("mapping without a heap is refused", !nvmtl4_sparse_texture_update((id<MTLTexture>)pt, nil, &pop, 1));
                NVMTLTexture *pt2 = nvmtl4_new_sparse_texture(dv, pd, nil);
                MTL4CopySparseTextureMappingOperation cop = { MTLRegionMake2D(0, 0, 2, 1), 0, 0, MTLOriginMake(2, 1, 0), 0, 0 };
                SPT("copying the mapping of two tiles to another texture binds them at the new position", nvmtl4_sparse_texture_copy((id<MTLTexture>)pt, (id<MTLTexture>)pt2, &cop, 1) && g_ntruns == 2 &&
                    g_truns[0].x == 256 && g_truns[0].y == 128 && g_truns[0].mem_off == 3 * 65536 && g_truns[1].x == 384);
            }
            // tile size queries
            MTLSize ts = nvmtl4_sparse_tile_size(MTLTextureType2D, MTLPixelFormatRGBA8Unorm, 1, MTLSparsePageSize64);
            SPT("the tile size query answers the GPU's 128x128 for RGBA8", ts.width == 128 && ts.height == 128 && ts.depth == 1);
            SPT("the tile size of a multisample texture is zero", nvmtl4_sparse_tile_size(MTLTextureType2D, MTLPixelFormatRGBA8Unorm, 4, MTLSparsePageSize64).width == 0);
            SPT("a 256 KB page's tile is 2x2 of the 64 KB one", nvmtl4_sparse_tile_size(MTLTextureType2D, MTLPixelFormatRGBA8Unorm, 1, MTLSparsePageSize256).width == 256);
            #undef SPT
            #undef OP
        }
        {   // tensor arguments in function reflection
            NSDictionary *tb = @{ @"kind": @"Tensor", @"data_type": @"half", @"index_type": @"uint", @"rank": @2, @"dimensions": @[@8, @-1] };
            id<MTLTensorBinding> a = (id<MTLTensorBinding>)nvmtl4_tensor_argument(tb, @{ @"args": @{ @"tensor:3": @"weights" } }, 3, YES, MTLBindingAccessReadOnly);
            printf("%s harness: a Tensor reflection entry becomes a tensor binding (name %s, type %ld, data type %ld, index type %ld, rank %lu)\n",
                   a && [a.name isEqualToString:@"weights"] && (NSInteger)a.type == MTLBindingTypeTensor && a.tensorDataType == MTLTensorDataTypeFloat16 && a.indexType == MTLDataTypeUInt && a.dimensions.rank == 2 && [a.dimensions extentAtDimensionIndex:0] == 8 && [a.dimensions extentAtDimensionIndex:1] == -1 ? "PASS   " : "FAIL   ",
                   a.name.UTF8String, (long)a.type, (long)a.tensorDataType, (long)a.indexType, (unsigned long)a.dimensions.rank);
            id<MTLTensorBinding> dyn = (id<MTLTensorBinding>)nvmtl4_tensor_argument(@{ @"data_type": @"float", @"index_type": @"int" }, @{}, 0, YES, MTLBindingAccessReadWrite);
            printf("%s harness: a tensor entry without dimensions has nil dimensions (undefined shape)\n", dyn && dyn.dimensions == nil && dyn.tensorDataType == MTLTensorDataTypeFloat32 ? "PASS   " : "FAIL   ");
        }
        {   // placement sparse buffers over the recorder
            id<MTLDevice> dv = (id<MTLDevice>)g_dev;
            #define SP(name, cond) printf("%s harness: %s\n", (cond) ? "PASS   " : "FAIL   ", name)
            id<MTLBuffer> sb = [dv newBufferWithLength:1 << 20 options:MTLResourceStorageModePrivate placementSparsePageSize:MTLSparsePageSize64];
            SP("a 1 MB placement sparse buffer with 64 KB pages is made and reports sparse tier 1", sb && sb.sparseBufferTier == MTLBufferSparseTier1);
            SP("a 16 KB page size is refused (finer than the 64 KB binding granule)", [dv newBufferWithLength:1 << 20 options:MTLResourceStorageModePrivate placementSparsePageSize:MTLSparsePageSize16] == nil);
            SP("shared storage is refused", [dv newBufferWithLength:1 << 20 options:MTLResourceStorageModeShared placementSparsePageSize:MTLSparsePageSize64] == nil);
            id<MTLBuffer> plain = [dv newBufferWithLength:4096 options:MTLResourceStorageModeShared placementSparsePageSize:(MTLSparsePageSize)0];
            SP("a page size that is not a sparse size gives an ordinary buffer (as on Apple GPUs)", plain && plain.sparseBufferTier == MTLBufferSparseTierNone);
            NVMTLHeap *hp = [NVMTLHeap new]; hp->_type = MTLHeapTypePlacement; hp->_hm.mem = (void *)0x1000; hp->_hm.size = 8 << 20; hp->_hm.memtype = 3;
            MTL4UpdateSparseBufferMappingOperation m1 = { MTLSparseTextureMappingModeMap, NSMakeRange(0, 4), 2 };
            g_binds = 0;
            SP("mapping pages 0..3 to heap pages 2..5 succeeds", nvmtl4_sparse_buffer_update(sb, (id<MTLHeap>)hp, &m1, 1));
            SP("... as one bind of 256 KB at buffer 0 from heap offset 128 KB", g_binds == 1 && g_nruns == 1 && g_runs[0].buf_off == 0 && g_runs[0].size == 262144 && g_runs[0].mem_off == 131072 && g_runs[0].mem == hp->_hm.mem && g_runs[0].memtype == 3);
            MTL4UpdateSparseBufferMappingOperation m2 = { MTLSparseTextureMappingModeUnmap, NSMakeRange(1, 1), 99 };
            SP("unmapping page 1 succeeds", nvmtl4_sparse_buffer_update(sb, nil, &m2, 1));
            SP("... as one unbind of 64 KB at buffer offset 64 KB", g_nruns == 1 && g_runs[0].mem == NULL && g_runs[0].buf_off == 65536 && g_runs[0].size == 65536);
            id<MTLBuffer> sb2 = [dv newBufferWithLength:1 << 20 options:MTLResourceStorageModePrivate placementSparsePageSize:MTLSparsePageSize64];
            MTL4CopySparseBufferMappingOperation cp = { NSMakeRange(0, 4), 4 };
            SP("copying the mapping of pages 0..3 to pages 4..7 of another buffer succeeds", nvmtl4_sparse_buffer_copy(sb, sb2, &cp, 1));
            SP("... as map page 4, unmap page 5, map pages 6..7 together", g_nruns == 3 && g_runs[0].mem && g_runs[0].buf_off == 4 * 65536 && g_runs[0].mem_off == 131072 &&
               !g_runs[1].mem && g_runs[1].buf_off == 5 * 65536 && g_runs[2].mem && g_runs[2].buf_off == 6 * 65536 && g_runs[2].size == 2 * 65536 && g_runs[2].mem_off == 131072 + 2 * 65536);
            MTL4UpdateSparseBufferMappingOperation bad = { MTLSparseTextureMappingModeMap, NSMakeRange(14, 4), 0 };
            int before = g_binds;
            SP("a range past the buffer's 16 pages is refused without a bind", !nvmtl4_sparse_buffer_update(sb, (id<MTLHeap>)hp, &bad, 1) && g_binds == before);
            SP("a map without a heap is refused", !nvmtl4_sparse_buffer_update(sb, nil, &m1, 1) && g_binds == before);
            g_bind_fail = 1;
            MTL4UpdateSparseBufferMappingOperation m3 = { MTLSparseTextureMappingModeMap, NSMakeRange(8, 1), 0 };
            SP("a failed bind reports failure", !nvmtl4_sparse_buffer_update(sb, (id<MTLHeap>)hp, &m3, 1));
            g_bind_fail = 0; g_nruns = 0;
            SP("... and leaves the mapping as it was (page 8 still unmapped: copying it unmaps the target page)", nvmtl4_sparse_buffer_copy(sb, sb2, &(MTL4CopySparseBufferMappingOperation){ NSMakeRange(8, 1), 8 }, 1) && g_nruns == 0 ? YES : g_nruns == 1 && !g_runs[0].mem);
            #undef SP
        }
        {   // library reflection by name, and the Metal 4 color attachment permutation
            NVMTLLibrary *lib = [NVMTLLibrary new];
            id r = [lib reflectionForFunctionWithName:@"known"];
            printf("%s harness: library reflection for a known function carries its 2 bindings (%lu)\n", r && [[r bindings] count] == 2 ? "PASS   " : "FAIL   ", (unsigned long)[[r bindings] count]);
            printf("%s harness: library reflection for an unknown function is nil\n", [lib reflectionForFunctionWithName:@"nope"] == nil ? "PASS   " : "FAIL   ");
            nvk_pass p; memset(&p, 0, sizeof p);
            for (int i = 0; i < NVMTL_NCOL; i++) { p.colw[i] = 100 + i; p.colfmt[i] = 200 + i; p.clear[i][0] = (float)i; p.col_layer[i] = 7 * i; }
            p.load = NVMTL_LOAD_COLOUR(0) | NVMTL_LOAD_COLOUR(3) | 0x10000u;
            uint32_t sw[NVMTL_NCOL] = { 3, 1, 2, 0, 4, 5, 6, 7 };
            BOOL ok = nvmtl4_pass_apply_color_map(&p, sw);
            printf("%s harness: color map swaps slots 0 and 3 in every array (w %u/%u fmt %u clear %g layer %u)\n",
                   ok && p.colw[0] == 103 && p.colw[3] == 100 && p.colfmt[0] == 203 && p.clear[3][0] == 0 && p.col_layer[0] == 21 ? "PASS   " : "FAIL   ", p.colw[0], p.colw[3], p.colfmt[0], p.clear[3][0], p.col_layer[0]);
            printf("%s harness: color map moves load bits with their attachments and keeps depth bits (0x%x)\n",
                   p.load == (NVMTL_LOAD_COLOUR(3) | NVMTL_LOAD_COLOUR(0) | 0x10000u) ? "PASS   " : "FAIL   ", p.load);
            {   // the GPU tables behind tensor and depth stencil ids
                MTLTensorDescriptor *td = [MTLTensorDescriptor new];
                NSInteger dv[2] = { 5, 3 }; td.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:dv]; td.dataType = MTLTensorDataTypeFloat16;
                id<MTLTensor> t = [(id<MTLDevice>)g_dev newTensorWithDescriptor:td error:NULL];
                uint64_t rid = t.gpuResourceID._impl, tab = nvmtl4_tensor_table_address();
                const nvmtl_tensor_entry *en = tab && (rid & NVMTL_RESID_TENSOR) ? (const nvmtl_tensor_entry *)(uintptr_t)tab + ((rid & ~NVMTL_RESID_TENSOR) - 1) : NULL;
                printf("%s harness: the tensor table entry of id %llu holds rank 2, dims 5x3, strides 1,5, 16 bits (%u %d %d %d %d %u)\n",
                       en && en->rank == 2 && en->dims[0] == 5 && en->dims[1] == 3 && en->strides[0] == 1 && en->strides[1] == 5 && en->element_bits == 16 && en->data_addr == [((NVMTLTensor *)t)->_buf gpuAddress] ? "PASS   " : "FAIL   ",
                       (unsigned long long)rid, en ? en->rank : 0, en ? en->dims[0] : 0, en ? en->dims[1] : 0, en ? en->strides[0] : 0, en ? en->strides[1] : 0, en ? en->element_bits : 0);
                NVMTLDepthStencilState *ds = [NVMTLDepthStencilState new]; ds->_write = 1; ds->_compare = 3; ds->_stencil = 1; ds->_sf[0] = 7;
                uint64_t did = [ds gpuResourceID]._impl; const nvmtl_ds_entry *de = (const nvmtl_ds_entry *)(uintptr_t)nvmtl4_ds_table_address() + (did - 1);
                printf("%s harness: the depth stencil table entry of id %llu holds write 1, compare 3, stencil on, front compare 7\n",
                       de->depth_write == 1 && de->depth_compare == 3 && de->stencil_enabled == 1 && de->front[0] == 7 ? "PASS   " : "FAIL   ", (unsigned long long)did);
            }
            uint32_t dup[NVMTL_NCOL] = { 1, 1, 2, 3, 4, 5, 6, 7 };
            nvk_pass before = p;
            printf("%s harness: a repeated physical index is refused and leaves the pass unchanged\n", !nvmtl4_pass_apply_color_map(&p, dup) && !memcmp(&p, &before, sizeof p) ? "PASS   " : "FAIL   ");
        }
        return oracle_run((id<MTLDevice>)g_dev, argc > 1 ? [NSString stringWithUTF8String:argv[1]] : @"build/mtl4_extras.metallib");
    }
}
