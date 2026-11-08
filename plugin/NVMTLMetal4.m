// NVMTLMetal4.m - the Metal 4 command model, built on this driver's Metal 3 objects.
//
// Metal 4 changes how work is recorded (command allocators, command buffers committed to a queue, argument tables that
// carry GPU addresses and resource IDs, a separate compiler object), not what the GPU does. Every Metal 4 object here is
// therefore a thin adapter over the Metal 3 object that already runs the work:
//   - a Metal 4 command buffer owns one Metal 3 command buffer (made from a private queue, so MetalFX/DLSS encodes recorded
//     into a Metal 4 command buffer land in the same Vulkan command buffer as everything else);
//   - a Metal 4 compute encoder opens a Metal 3 compute, blit or acceleration encoder as the call needs and ends it when the
//     kind changes (Metal 4 has one encoder for all three);
//   - a Metal 4 render encoder wraps one Metal 3 render encoder; calls with the same signature are forwarded unchanged,
//     calls that take GPU addresses resolve the address to its buffer first;
//   - an argument table is a plain record of addresses/resource IDs, pushed onto the Metal 3 encoder when it changes;
//   - the compiler converts the Metal 4 descriptors to Metal 3 ones and uses the device's existing pipeline path.
// Commits go to one private serial Metal 3 queue, so every Metal 4 queue is ordered by commit order.
// NVMTL_METAL4=0 turns the whole layer off (supportsFamily:Metal4 answers NO and no Metal 4 object is made).

#import <Metal/MTL4CommandQueue.h>

static BOOL nvmtl_m4_enabled(void)
{
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("NVMTL_METAL4");
        BOOL knob = !(e && e[0] == '0');
        // The Metal 4 protocols and descriptor classes ship in the OS from macOS 26.
        on = knob && NSClassFromString(@"MTL4ComputePipelineDescriptor") != nil && NSClassFromString(@"MTL4CommandBufferOptions") != nil;
        nvlog("Metal 4: %s (NVMTL_METAL4=%s, MTL4 descriptor classes %s)", on ? "ON" : "off", e ? e : "unset",
              NSClassFromString(@"MTL4ComputePipelineDescriptor") ? "present" : "absent");
    }
    return on == 1;
}

// ---- GPU address -> buffer ----------------------------------------------------------------------------------------------
// An argument table holds addresses, the Metal 3 encoders want buffers. A buffer enters this table the first time the app
// asks for its gpuAddress (it cannot form an address before that), and the entry holds the buffer weakly.
// Boxes in an array sorted by base address (weak pointers cannot live in memmoved C structs under ARC).
@interface NVMTL4Range : NSObject { @public uint64_t base, end; __weak id buf; } @end
@implementation NVMTL4Range @end
static NSMutableArray<NVMTL4Range *> *nvmtl_m4_ranges;
static os_unfair_lock nvmtl_m4_lock = OS_UNFAIR_LOCK_INIT;

// First index whose base is >= `base`.
static NSUInteger nvmtl_m4_lower(uint64_t base)
{
    NSUInteger lo = 0, hi = nvmtl_m4_ranges.count;
    while (lo < hi) { NSUInteger m = (lo + hi) / 2; if (nvmtl_m4_ranges[m]->base < base) lo = m + 1; else hi = m; }
    return lo;
}
// Bumped whenever an existing entry changes owner or length. A buffer whose cached note carries the current epoch
// and the same base and end is still exactly what the table holds for it, so asking again changes nothing.
// Measured on Cyberpunk: every -gpuAddress took this lock, did a binary search through NSArray (a retain and
// release per probe) and re-stored the weak pointer. With nine render threads asking for the same buffers'
// addresses, that was 1,266 of 22,300 busy render-thread samples, half of it spinning on this one lock.
// NVMTL_M4_NOTE_FAST=0 puts every call back through the table.
static _Atomic uint64_t nvmtl_m4_epoch = 1;
static int nvmtl_m4_note_fast(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_M4_NOTE_FAST"); on = !(e && e[0] == '0');
        nvlog("Metal 4: gpuAddress table %s (NVMTL_M4_NOTE_FAST=%s)", on ? "skipped for a buffer already noted at the same range" : "updated on every call", e ? e : "unset"); }
    return on;
}
int nvmtl_m4_note_current(_Atomic uint64_t note[3], uint64_t addr, uint64_t length)
{
    if (!addr || !length) return 1;   // never entered in the table either
    if (!nvmtl_m4_note_fast()) return 0;
    const uint64_t epoch = atomic_load_explicit(&note[2], memory_order_acquire);
    return epoch && epoch == atomic_load_explicit(&nvmtl_m4_epoch, memory_order_acquire) &&
           atomic_load_explicit(&note[0], memory_order_relaxed) == addr &&
           atomic_load_explicit(&note[1], memory_order_relaxed) == addr + length;
}
static void nvmtl_m4_note_locked(id buf, uint64_t addr, uint64_t length);
void nvmtl_m4_note_buffer(id buf, uint64_t addr, uint64_t length)
{
    if (!addr || !length) return;
    os_unfair_lock_lock(&nvmtl_m4_lock);
    nvmtl_m4_note_locked(buf, addr, length);
    os_unfair_lock_unlock(&nvmtl_m4_lock);
}
void nvmtl_m4_note_buffer_cached(id buf, uint64_t addr, uint64_t length, _Atomic uint64_t note[3])
{
    if (!addr || !length) return;
    os_unfair_lock_lock(&nvmtl_m4_lock);
    nvmtl_m4_note_locked(buf, addr, length);
    // Read under the lock, after this call's own bump: a later owner change bumps again and invalidates the note.
    atomic_store_explicit(&note[0], addr, memory_order_relaxed);
    atomic_store_explicit(&note[1], addr + length, memory_order_relaxed);
    atomic_store_explicit(&note[2], atomic_load_explicit(&nvmtl_m4_epoch, memory_order_relaxed), memory_order_release);
    os_unfair_lock_unlock(&nvmtl_m4_lock);
}
static void nvmtl_m4_note_locked(id buf, uint64_t addr, uint64_t length)
{
    if (!nvmtl_m4_ranges) nvmtl_m4_ranges = [NSMutableArray new];
    NSUInteger i = nvmtl_m4_lower(addr);
    if (i < nvmtl_m4_ranges.count && nvmtl_m4_ranges[i]->base == addr) {      // the same base again: the newer buffer owns it
        NVMTL4Range *r = nvmtl_m4_ranges[i];
        id prev = r->buf;
        if (prev != buf || r->end != addr + length) atomic_fetch_add_explicit(&nvmtl_m4_epoch, 1, memory_order_release);
        r->end = addr + length; r->buf = buf;
    } else {
        NVMTL4Range *r = [NVMTL4Range new]; r->base = addr; r->end = addr + length; r->buf = buf;
        [nvmtl_m4_ranges insertObject:r atIndex:i];
        // Drop entries whose buffer is gone when the table has doubled since the last sweep.
        static NSUInteger sweepAt = 1024;
        if (nvmtl_m4_ranges.count >= sweepAt) {
            NSMutableArray *live = [NSMutableArray arrayWithCapacity:nvmtl_m4_ranges.count];
            for (NVMTL4Range *x in nvmtl_m4_ranges) if (x->buf) [live addObject:x];
            nvmtl_m4_ranges = live; sweepAt = MAX((NSUInteger)1024, live.count * 2);
        }
    }
}
// The live buffer whose range holds `addr`, and the offset of `addr` in it. NO when none does (logged by the caller).
static BOOL nvmtl_m4_resolve(uint64_t addr, id<MTLBuffer> *out, NSUInteger *off)
{
    if (!addr) return NO;
    BOOL ok = NO;
    os_unfair_lock_lock(&nvmtl_m4_lock);
    NSUInteger i = nvmtl_m4_ranges ? nvmtl_m4_lower(addr + 1) : 0;   // first range with base > addr; the ones before it may hold addr
    while (i > 0) {
        NVMTL4Range *r = nvmtl_m4_ranges[i - 1];
        id b = r->buf;
        if (b && r->base <= addr && addr < r->end) { *out = b; *off = (NSUInteger)(addr - r->base); ok = YES; break; }
        if (r->base + (1ull << 32) < addr) break;       // far below: nothing earlier can span this
        i--;
    }
    os_unfair_lock_unlock(&nvmtl_m4_lock);
    return ok;
}
static void nvmtl_m4_unresolved(const char *what, uint64_t addr)
{
    static _Atomic int said;
    if (atomic_fetch_add(&said, 1) < 24)
        nvlog("Metal 4: %s: GPU address 0x%llx belongs to no buffer this app asked gpuAddress of - binding left empty", what, (unsigned long long)addr);
}

// Every method below belongs to a Metal 4 class; the plugin is built for macOS 15, where those types do not exist, so each is
// marked as introduced in macOS 26 (callers outside this file are guarded by nvmtl_m4_enabled()).
#pragma clang attribute push (__attribute__((availability(macos, introduced=26.0))), apply_to = objc_method)

// ---- allocator, argument table ------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4Allocator : NSObject <MTL4CommandAllocator> { @public id<MTLDevice> _dev; NSString *_label; } @end
@implementation NVMTL4Allocator
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (uint64_t)allocatedSize { return 0; }
- (void)reset {}   // commands live in the Metal 3 command buffer, which frees itself on completion
@end

enum { NVMTL4_KIND_NONE = 0, NVMTL4_KIND_ADDR = 1, NVMTL4_KIND_HANDLE = 2 };
API_AVAILABLE(macos(26.0))
@interface NVMTL4ArgumentTable : NSObject <MTL4ArgumentTable> {
    @public id<MTLDevice> _dev; NSString *_label; NSUInteger _nb, _nt, _ns;
    uint8_t *_bk, *_tk, *_sk; uint64_t *_b, *_t, *_s; NSUInteger *_stride; BOOL *_hasStride; uint64_t _ver;
}
@end
@implementation NVMTL4ArgumentTable
- (instancetype)initWithDescriptor:(MTL4ArgumentTableDescriptor *)d device:(id<MTLDevice>)dev {
    if (!(self = [super init])) return nil;
    _dev = dev; _label = [d.label copy];
    _nb = MIN(d.maxBufferBindCount, (NSUInteger)64); _nt = MIN(d.maxTextureBindCount, (NSUInteger)256); _ns = MIN(d.maxSamplerStateBindCount, (NSUInteger)64);
    _bk = calloc(_nb + 1, 1); _tk = calloc(_nt + 1, 1); _sk = calloc(_ns + 1, 1);
    _b = calloc(_nb + 1, 8); _t = calloc(_nt + 1, 8); _s = calloc(_ns + 1, 8);
    _stride = calloc(_nb + 1, sizeof(NSUInteger)); _hasStride = calloc(_nb + 1, sizeof(BOOL));
    _ver = 1;
    return self;
}
- (void)dealloc { free(_bk); free(_tk); free(_sk); free(_b); free(_t); free(_s); free(_stride); free(_hasStride); }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setAddress:(MTLGPUAddress)a atIndex:(NSUInteger)i {
    if (i >= _nb) { nvlog("Metal 4: argument table: buffer index %lu is past maxBufferBindCount %lu - ignored", (unsigned long)i, (unsigned long)_nb); return; }
    _bk[i] = a ? NVMTL4_KIND_ADDR : NVMTL4_KIND_NONE; _b[i] = a; _hasStride[i] = NO; _ver++;
}
- (void)setAddress:(MTLGPUAddress)a attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (i >= _nb) { nvlog("Metal 4: argument table: buffer index %lu is past maxBufferBindCount %lu - ignored", (unsigned long)i, (unsigned long)_nb); return; }
    _bk[i] = a ? NVMTL4_KIND_ADDR : NVMTL4_KIND_NONE; _b[i] = a; _stride[i] = st; _hasStride[i] = YES; _ver++;
}
- (void)setResource:(MTLResourceID)r atBufferIndex:(NSUInteger)i {
    if (i >= _nb) { nvlog("Metal 4: argument table: buffer index %lu is past maxBufferBindCount %lu - ignored", (unsigned long)i, (unsigned long)_nb); return; }
    _bk[i] = r._impl ? NVMTL4_KIND_HANDLE : NVMTL4_KIND_NONE; _b[i] = r._impl; _hasStride[i] = NO; _ver++;
}
- (void)setTexture:(MTLResourceID)r atIndex:(NSUInteger)i {
    if (i >= _nt) { nvlog("Metal 4: argument table: texture index %lu is past maxTextureBindCount %lu - ignored", (unsigned long)i, (unsigned long)_nt); return; }
    _tk[i] = r._impl ? NVMTL4_KIND_HANDLE : NVMTL4_KIND_NONE; _t[i] = r._impl; _ver++;
}
- (void)setSamplerState:(MTLResourceID)r atIndex:(NSUInteger)i {
    if (i >= _ns) { nvlog("Metal 4: argument table: sampler index %lu is past maxSamplerStateBindCount %lu - ignored", (unsigned long)i, (unsigned long)_ns); return; }
    _sk[i] = r._impl ? NVMTL4_KIND_HANDLE : NVMTL4_KIND_NONE; _s[i] = r._impl; _ver++;
}
@end

// Stage selector for pushing a table: the Metal 3 encoder method family differs per stage only by name.
typedef NS_ENUM(int, NVMTL4Stage) { NVMTL4_ST_COMPUTE, NVMTL4_ST_VERTEX, NVMTL4_ST_FRAGMENT, NVMTL4_ST_TILE, NVMTL4_ST_OBJECT, NVMTL4_ST_MESH };

API_AVAILABLE(macos(26.0))
static void nvmtl_m4_push_table(NVMTL4ArgumentTable *t, id enc, NVMTL4Stage st)
{
    if (!t || !enc) return;
    for (NSUInteger i = 0; i < t->_nb; i++) {
        if (t->_bk[i] == NVMTL4_KIND_NONE) continue;
        if (t->_bk[i] == NVMTL4_KIND_HANDLE) {        // a resource ID in a buffer slot: the shader reads the 8-byte handle
            uint64_t h = t->_b[i];
            switch (st) {
                case NVMTL4_ST_COMPUTE: [(id<MTLComputeCommandEncoder>)enc setBytes:&h length:8 atIndex:i]; break;
                case NVMTL4_ST_VERTEX: [(id<MTLRenderCommandEncoder>)enc setVertexBytes:&h length:8 atIndex:i]; break;
                case NVMTL4_ST_FRAGMENT: [(id<MTLRenderCommandEncoder>)enc setFragmentBytes:&h length:8 atIndex:i]; break;
                case NVMTL4_ST_OBJECT: [(id<MTLRenderCommandEncoder>)enc setObjectBytes:&h length:8 atIndex:i]; break;
                case NVMTL4_ST_MESH: [(id<MTLRenderCommandEncoder>)enc setMeshBytes:&h length:8 atIndex:i]; break;
                case NVMTL4_ST_TILE: [(id<MTLRenderCommandEncoder>)enc setTileBytes:&h length:8 atIndex:i]; break;
            }
            continue;
        }
        id<MTLBuffer> b = nil; NSUInteger off = 0;
        if (!nvmtl_m4_resolve(t->_b[i], &b, &off)) { nvmtl_m4_unresolved("argument table", t->_b[i]); b = nil; off = 0; }
        switch (st) {
            case NVMTL4_ST_COMPUTE: [(id<MTLComputeCommandEncoder>)enc setBuffer:b offset:off atIndex:i]; break;
            case NVMTL4_ST_VERTEX:
                if (t->_hasStride[i]) [(id<MTLRenderCommandEncoder>)enc setVertexBuffer:b offset:off attributeStride:t->_stride[i] atIndex:i];
                else [(id<MTLRenderCommandEncoder>)enc setVertexBuffer:b offset:off atIndex:i];
                break;
            case NVMTL4_ST_FRAGMENT: [(id<MTLRenderCommandEncoder>)enc setFragmentBuffer:b offset:off atIndex:i]; break;
            case NVMTL4_ST_OBJECT: [(id<MTLRenderCommandEncoder>)enc setObjectBuffer:b offset:off atIndex:i]; break;
            case NVMTL4_ST_MESH: [(id<MTLRenderCommandEncoder>)enc setMeshBuffer:b offset:off atIndex:i]; break;
            case NVMTL4_ST_TILE: [(id<MTLRenderCommandEncoder>)enc setTileBuffer:b offset:off atIndex:i]; break;
        }
    }
    for (NSUInteger i = 0; i < t->_nt; i++) {
        if (t->_tk[i] == NVMTL4_KIND_NONE) continue;
        id<MTLTexture> x = (id<MTLTexture>)nvmtl_resid_lookup(t->_t[i]);
        if (!x) { nvlog("Metal 4: argument table: texture index %lu holds resource ID 0x%llx that is no live texture of this driver", (unsigned long)i, (unsigned long long)t->_t[i]); continue; }
        switch (st) {
            case NVMTL4_ST_COMPUTE: [(id<MTLComputeCommandEncoder>)enc setTexture:x atIndex:i]; break;
            case NVMTL4_ST_VERTEX: [(id<MTLRenderCommandEncoder>)enc setVertexTexture:x atIndex:i]; break;
            case NVMTL4_ST_FRAGMENT: [(id<MTLRenderCommandEncoder>)enc setFragmentTexture:x atIndex:i]; break;
            case NVMTL4_ST_OBJECT: [(id<MTLRenderCommandEncoder>)enc setObjectTexture:x atIndex:i]; break;
            case NVMTL4_ST_MESH: [(id<MTLRenderCommandEncoder>)enc setMeshTexture:x atIndex:i]; break;
            case NVMTL4_ST_TILE: [(id<MTLRenderCommandEncoder>)enc setTileTexture:x atIndex:i]; break;
        }
    }
    for (NSUInteger i = 0; i < t->_ns; i++) {
        if (t->_sk[i] == NVMTL4_KIND_NONE) continue;
        id<MTLSamplerState> x = (id<MTLSamplerState>)nvmtl_resid_lookup(t->_s[i]);
        if (!x) { nvlog("Metal 4: argument table: sampler index %lu holds resource ID 0x%llx that is no live sampler of this driver", (unsigned long)i, (unsigned long long)t->_s[i]); continue; }
        switch (st) {
            case NVMTL4_ST_COMPUTE: [(id<MTLComputeCommandEncoder>)enc setSamplerState:x atIndex:i]; break;
            case NVMTL4_ST_VERTEX: [(id<MTLRenderCommandEncoder>)enc setVertexSamplerState:x atIndex:i]; break;
            case NVMTL4_ST_FRAGMENT: [(id<MTLRenderCommandEncoder>)enc setFragmentSamplerState:x atIndex:i]; break;
            case NVMTL4_ST_OBJECT: [(id<MTLRenderCommandEncoder>)enc setObjectSamplerState:x atIndex:i]; break;
            case NVMTL4_ST_MESH: [(id<MTLRenderCommandEncoder>)enc setMeshSamplerState:x atIndex:i]; break;
            case NVMTL4_ST_TILE: [(id<MTLRenderCommandEncoder>)enc setTileSamplerState:x atIndex:i]; break;
        }
    }
}

// ---- command buffer -----------------------------------------------------------------------------------------------------
// One private serial queue per device carries every Metal 4 command buffer.
static id<MTLCommandQueue> nvmtl_m4_hidden_queue(id<MTLDevice> dev)
{
    static id<MTLCommandQueue> q; static dispatch_once_t once;
    dispatch_once(&once, ^{ q = [dev newCommandQueue]; if (q) q.label = @"Metal 4 commit queue"; });
    return q;
}

@class NVMTL4CommandBuffer;
API_AVAILABLE(macos(26.0))
@interface NVMTL4CommandBuffer : NSObject <MTL4CommandBuffer> {
    @public id<MTLDevice> _dev; NSString *_label; id<MTLCommandBuffer> _u; BOOL _begun, _ended, _committed;
    __weak id _openEnc; NSMutableArray *_sets; NSMutableArray *_groups;
}
- (id<MTLCommandBuffer>)nvmtlUnderlying;
@end

// The driver's command buffer under a Metal 4 one (or under a Metal 3 one, unchanged) - what MetalFX/DLSS record into.
static id<MTLCommandBuffer> nvmtl_m4_underlying(id cb)
{
    if (!cb) return nil;
    if (@available(macOS 26.0, *)) if ([cb isKindOfClass:[NVMTL4CommandBuffer class]]) return [(NVMTL4CommandBuffer *)cb nvmtlUnderlying];
    return cb;
}

// ---- compute / copy encoder ---------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4ComputeEncoder : NSObject <MTL4ComputeCommandEncoder> {
    @public NVMTL4CommandBuffer *_cb; int _kind; id _enc; NSString *_label; BOOL _ended;
    id<MTLComputePipelineState> _ps; NVMTL4ArgumentTable *_table; uint64_t _pushedVer; __weak NVMTL4ArgumentTable *_pushedTable; BOOL _needPush;
    NSMutableDictionary<NSNumber *, NSNumber *> *_tg;
}
@end

static NSSet<NSString *> *nvmtl_m4_blit_selectors(void)
{
    static NSSet *s; static dispatch_once_t once;
    dispatch_once(&once, ^{
        s = [NSSet setWithArray:@[
            @"copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:",
            @"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
            @"copyFromBuffer:sourceOffset:sourceBytesPerRow:sourceBytesPerImage:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:options:",
            @"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
            @"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:options:",
            @"copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
            @"copyFromTexture:sourceSlice:sourceLevel:toTexture:destinationSlice:destinationLevel:sliceCount:levelCount:",
            @"copyFromTexture:toTexture:", @"fillBuffer:range:value:", @"generateMipmapsForTexture:",
            @"optimizeContentsForCPUAccess:", @"optimizeContentsForCPUAccess:slice:level:",
            @"optimizeContentsForGPUAccess:", @"optimizeContentsForGPUAccess:slice:level:",
            @"copyIndirectCommandBuffer:sourceRange:destination:destinationIndex:",
            @"resetCommandsInBuffer:withRange:", @"optimizeIndirectCommandBuffer:withRange:",
        ]];
    });
    return s;
}

@implementation NVMTL4ComputeEncoder
- (instancetype)initWithCommandBuffer:(NVMTL4CommandBuffer *)cb { if ((self = [super init])) { _cb = cb; _tg = [NSMutableDictionary new]; } return self; }
- (id<MTL4CommandBuffer>)commandBuffer { return _cb; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; if (_enc) [(id)_enc setLabel:l]; }
- (MTLStages)stages { return MTLStageDispatch | MTLStageBlit; }
- (void)nvmtlCloseUnderlying { if (_enc) { [(id)_enc endEncoding]; _enc = nil; _kind = 0; } }
// Open (or keep) the Metal 3 compute encoder, with the state set so far.
- (id<MTLComputeCommandEncoder>)nvmtlCompute {
    if (_kind == 1) return _enc;
    [self nvmtlCloseUnderlying];
    id<MTLComputeCommandEncoder> e = [_cb->_u computeCommandEncoder];
    _enc = e; _kind = 1; if (_label) e.label = _label;
    if (_ps) [e setComputePipelineState:_ps];
    [_tg enumerateKeysAndObjectsUsingBlock:^(NSNumber *k, NSNumber *v, BOOL *stop) { [e setThreadgroupMemoryLength:v.unsignedIntegerValue atIndex:k.unsignedIntegerValue]; }];
    _needPush = YES;
    return e;
}
- (id<MTLBlitCommandEncoder>)nvmtlBlit {
    if (_kind == 2) return _enc;
    [self nvmtlCloseUnderlying];
    id<MTLBlitCommandEncoder> e = [_cb->_u blitCommandEncoder];
    _enc = e; _kind = 2; if (_label) e.label = _label;
    return e;
}
- (void)nvmtlPush {
    id<MTLComputeCommandEncoder> e = [self nvmtlCompute];
    if (_needPush || _pushedTable != _table || (_table && _pushedVer != _table->_ver)) {
        if (_table) nvmtl_m4_push_table(_table, e, NVMTL4_ST_COMPUTE);
        _pushedTable = _table; _pushedVer = _table ? _table->_ver : 0; _needPush = NO;
    }
}
- (id)forwardingTargetForSelector:(SEL)sel {
    if ([nvmtl_m4_blit_selectors() containsObject:NSStringFromSelector(sel)]) return [self nvmtlBlit];
    return nil;
}
- (void)setComputePipelineState:(id<MTLComputePipelineState>)ps { _ps = ps; if (_kind == 1) [(id<MTLComputeCommandEncoder>)_enc setComputePipelineState:ps]; }
- (void)setArgumentTable:(id<MTL4ArgumentTable>)t { _table = (NVMTL4ArgumentTable *)t; _needPush = YES; }
- (void)setThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i { _tg[@(i)] = @(len); if (_kind == 1) [(id<MTLComputeCommandEncoder>)_enc setThreadgroupMemoryLength:len atIndex:i]; }
- (void)setImageblockWidth:(NSUInteger)w height:(NSUInteger)h { nvlog("Metal 4: compute setImageblockWidth:height: (tile shading) is not implemented"); }
- (void)dispatchThreadgroups:(MTLSize)g threadsPerThreadgroup:(MTLSize)t { [self nvmtlPush]; [(id<MTLComputeCommandEncoder>)_enc dispatchThreadgroups:g threadsPerThreadgroup:t]; }
- (void)dispatchThreads:(MTLSize)g threadsPerThreadgroup:(MTLSize)t { [self nvmtlPush]; [(id<MTLComputeCommandEncoder>)_enc dispatchThreads:g threadsPerThreadgroup:t]; }
- (void)dispatchThreadgroupsWithIndirectBuffer:(MTLGPUAddress)a threadsPerThreadgroup:(MTLSize)t {
    id<MTLBuffer> b = nil; NSUInteger off = 0;
    if (!nvmtl_m4_resolve(a, &b, &off)) { nvmtl_m4_unresolved("dispatchThreadgroupsWithIndirectBuffer", a); return; }
    [self nvmtlPush]; [(id<MTLComputeCommandEncoder>)_enc dispatchThreadgroupsWithIndirectBuffer:b indirectBufferOffset:off threadsPerThreadgroup:t];
}
- (void)dispatchThreadsWithIndirectBuffer:(MTLGPUAddress)a { nvlog("Metal 4: dispatchThreadsWithIndirectBuffer: (threads, not threadgroups, from a buffer) is not implemented - dispatch dropped"); }
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r { [self nvmtlPush]; [(id<MTLComputeCommandEncoder>)_enc executeCommandsInBuffer:icb withRange:r]; }
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(MTLGPUAddress)a {
    id<MTLBuffer> b = nil; NSUInteger off = 0;
    if (!nvmtl_m4_resolve(a, &b, &off)) { nvmtl_m4_unresolved("executeCommandsInBuffer", a); return; }
    [self nvmtlPush]; [(id<MTLComputeCommandEncoder>)_enc executeCommandsInBuffer:icb indirectBuffer:b indirectBufferOffset:off];
}
// Serial dispatch already orders everything the encoder records; a barrier needs no further command.
- (void)barrierAfterQueueStages:(MTLStages)a beforeStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)barrierAfterStages:(MTLStages)a beforeQueueStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)barrierAfterEncoderStages:(MTLStages)a beforeEncoderStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)updateFence:(id<MTLFence>)f afterEncoderStages:(MTLStages)s { if (!_enc) [self nvmtlCompute]; [(id)_enc updateFence:f]; }
- (void)waitForFence:(id<MTLFence>)f beforeEncoderStages:(MTLStages)s { if (!_enc) [self nvmtlCompute]; [(id)_enc waitForFence:f]; }
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)writeTimestampWithGranularity:(MTL4TimestampGranularity)g intoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i { nvlog("Metal 4: counter heaps are not implemented - timestamp dropped"); }
- (void)buildAccelerationStructure:(id<MTLAccelerationStructure>)a descriptor:(MTL4AccelerationStructureDescriptor *)d scratchBuffer:(MTL4BufferRange)s { nvlog("Metal 4: MTL4 acceleration structure builds are not implemented"); }
- (void)endEncoding { if (_ended) return; _ended = YES; [self nvmtlCloseUnderlying]; _cb->_openEnc = nil; }
- (void)dealloc { if (!_ended) [self nvmtlCloseUnderlying]; }
@end

// ---- render encoder -----------------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4RenderEncoder : NSObject <MTL4RenderCommandEncoder> {
    @public NVMTL4CommandBuffer *_cb; id<MTLRenderCommandEncoder> _enc; NSString *_label; BOOL _ended;
    NVMTL4ArgumentTable *_tab[6]; __weak NVMTL4ArgumentTable *_pushedTab[6]; uint64_t _pushedVer[6];
}
@end
@implementation NVMTL4RenderEncoder
- (id<MTL4CommandBuffer>)commandBuffer { return _cb; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; _enc.label = l; }
- (id)forwardingTargetForSelector:(SEL)sel { return _enc; }   // every call whose signature Metal 3 shares: viewport, scissor, cull, depth bias, stencil, ...
- (void)nvmtlFlushStage:(NVMTL4Stage)st {
    NVMTL4ArgumentTable *t = _tab[st];
    if (!t) return;
    if (_pushedTab[st] == t && _pushedVer[st] == t->_ver) return;
    nvmtl_m4_push_table(t, _enc, st);
    _pushedTab[st] = t; _pushedVer[st] = t->_ver;
}
- (void)nvmtlFlush { for (int s = NVMTL4_ST_VERTEX; s <= NVMTL4_ST_MESH; s++) [self nvmtlFlushStage:(NVMTL4Stage)s]; }
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)ps { [_enc setRenderPipelineState:ps]; }
- (void)setArgumentTable:(id<MTL4ArgumentTable>)t atStages:(MTLRenderStages)stages {
    NVMTL4ArgumentTable *tab = (NVMTL4ArgumentTable *)t;
    struct { MTLRenderStages bit; NVMTL4Stage st; } m[] = { { MTLRenderStageVertex, NVMTL4_ST_VERTEX }, { MTLRenderStageFragment, NVMTL4_ST_FRAGMENT },
        { MTLRenderStageTile, NVMTL4_ST_TILE }, { MTLRenderStageObject, NVMTL4_ST_OBJECT }, { MTLRenderStageMesh, NVMTL4_ST_MESH } };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) if (stages & m[i].bit) { _tab[m[i].st] = tab; _pushedTab[m[i].st] = nil; }
}
- (void)setColorAttachmentMap:(MTLLogicalToPhysicalColorAttachmentMap *)map { nvlog("Metal 4: setColorAttachmentMap: (logical-to-physical colour attachment mapping) is not implemented - identity kept"); }
- (void)drawPrimitives:(MTLPrimitiveType)p vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c { [self nvmtlFlush]; [_enc drawPrimitives:p vertexStart:s vertexCount:c]; }
- (void)drawPrimitives:(MTLPrimitiveType)p vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c instanceCount:(NSUInteger)n { [self nvmtlFlush]; [_enc drawPrimitives:p vertexStart:s vertexCount:c instanceCount:n]; }
- (void)drawPrimitives:(MTLPrimitiveType)p vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c instanceCount:(NSUInteger)n baseInstance:(NSUInteger)bi { [self nvmtlFlush]; [_enc drawPrimitives:p vertexStart:s vertexCount:c instanceCount:n baseInstance:bi]; }
- (BOOL)nvmtlIndex:(MTLGPUAddress)a buffer:(id<MTLBuffer> *)b offset:(NSUInteger *)o what:(const char *)w {
    if (nvmtl_m4_resolve(a, b, o)) return YES;
    nvmtl_m4_unresolved(w, a); return NO;
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)p indexCount:(NSUInteger)n indexType:(MTLIndexType)t indexBuffer:(MTLGPUAddress)a indexBufferLength:(NSUInteger)len {
    id<MTLBuffer> b; NSUInteger o; if (![self nvmtlIndex:a buffer:&b offset:&o what:"drawIndexedPrimitives"]) return;
    [self nvmtlFlush]; [_enc drawIndexedPrimitives:p indexCount:n indexType:t indexBuffer:b indexBufferOffset:o];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)p indexCount:(NSUInteger)n indexType:(MTLIndexType)t indexBuffer:(MTLGPUAddress)a indexBufferLength:(NSUInteger)len instanceCount:(NSUInteger)ic {
    id<MTLBuffer> b; NSUInteger o; if (![self nvmtlIndex:a buffer:&b offset:&o what:"drawIndexedPrimitives"]) return;
    [self nvmtlFlush]; [_enc drawIndexedPrimitives:p indexCount:n indexType:t indexBuffer:b indexBufferOffset:o instanceCount:ic];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)p indexCount:(NSUInteger)n indexType:(MTLIndexType)t indexBuffer:(MTLGPUAddress)a indexBufferLength:(NSUInteger)len
                instanceCount:(NSUInteger)ic baseVertex:(NSInteger)bv baseInstance:(NSUInteger)bi {
    id<MTLBuffer> b; NSUInteger o; if (![self nvmtlIndex:a buffer:&b offset:&o what:"drawIndexedPrimitives"]) return;
    [self nvmtlFlush]; [_enc drawIndexedPrimitives:p indexCount:n indexType:t indexBuffer:b indexBufferOffset:o instanceCount:ic baseVertex:bv baseInstance:bi];
}
- (void)drawPrimitives:(MTLPrimitiveType)p indirectBuffer:(MTLGPUAddress)a {
    id<MTLBuffer> b; NSUInteger o; if (![self nvmtlIndex:a buffer:&b offset:&o what:"drawPrimitives indirect"]) return;
    [self nvmtlFlush]; [_enc drawPrimitives:p indirectBuffer:b indirectBufferOffset:o];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)p indexType:(MTLIndexType)t indexBuffer:(MTLGPUAddress)a indexBufferLength:(NSUInteger)len indirectBuffer:(MTLGPUAddress)ia {
    id<MTLBuffer> b, ib; NSUInteger o, io;
    if (![self nvmtlIndex:a buffer:&b offset:&o what:"drawIndexedPrimitives indirect index"] || ![self nvmtlIndex:ia buffer:&ib offset:&io what:"drawIndexedPrimitives indirect args"]) return;
    [self nvmtlFlush]; [_enc drawIndexedPrimitives:p indexType:t indexBuffer:b indexBufferOffset:o indirectBuffer:ib indirectBufferOffset:io];
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)r { [self nvmtlFlush]; [_enc executeCommandsInBuffer:icb withRange:r]; }
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(MTLGPUAddress)a {
    id<MTLBuffer> b; NSUInteger o; if (![self nvmtlIndex:a buffer:&b offset:&o what:"executeCommandsInBuffer"]) return;
    [self nvmtlFlush]; [_enc executeCommandsInBuffer:icb indirectBuffer:b indirectBufferOffset:o];
}
- (void)setObjectThreadgroupMemoryLength:(NSUInteger)len atIndex:(NSUInteger)i { [_enc setObjectThreadgroupMemoryLength:len atIndex:i]; }
- (void)drawMeshThreadgroups:(MTLSize)g threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m { [self nvmtlFlush]; [_enc drawMeshThreadgroups:g threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m]; }
- (void)drawMeshThreads:(MTLSize)g threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m { [self nvmtlFlush]; [_enc drawMeshThreads:g threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m]; }
- (void)drawMeshThreadgroupsWithIndirectBuffer:(MTLGPUAddress)a threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m {
    id<MTLBuffer> b; NSUInteger off; if (![self nvmtlIndex:a buffer:&b offset:&off what:"drawMeshThreadgroupsWithIndirectBuffer"]) return;
    [self nvmtlFlush]; [_enc drawMeshThreadgroupsWithIndirectBuffer:b indirectBufferOffset:off threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m];
}
- (void)dispatchThreadsPerTile:(MTLSize)s { [self nvmtlFlush]; [_enc dispatchThreadsPerTile:s]; }
- (void)barrierAfterQueueStages:(MTLStages)a beforeStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
- (void)barrierAfterStages:(MTLStages)a beforeQueueStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {}
// A barrier between draws of one render pass: the Metal 3 encoder orders its draws; texture self-dependencies go through
// the encoder's own barrier when the app marks them.
- (void)barrierAfterEncoderStages:(MTLStages)a beforeEncoderStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {
    if ([_enc respondsToSelector:@selector(memoryBarrierWithScope:afterStages:beforeStages:)])
        [_enc memoryBarrierWithScope:MTLBarrierScopeBuffers | MTLBarrierScopeTextures | MTLBarrierScopeRenderTargets afterStages:(MTLRenderStages)a beforeStages:(MTLRenderStages)b];
}
- (void)updateFence:(id<MTLFence>)f afterEncoderStages:(MTLStages)s { [_enc updateFence:f afterStages:(MTLRenderStages)s]; }
- (void)waitForFence:(id<MTLFence>)f beforeEncoderStages:(MTLStages)s { [_enc waitForFence:f beforeStages:(MTLRenderStages)s]; }
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)writeTimestampWithGranularity:(MTL4TimestampGranularity)g afterStage:(MTLRenderStages)st intoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i { nvlog("Metal 4: counter heaps are not implemented - timestamp dropped"); }
- (void)endEncoding { if (_ended) return; _ended = YES; [_enc endEncoding]; _cb->_openEnc = nil; }
- (void)dealloc { if (!_ended) [_enc endEncoding]; }
@end

// ---- command buffer methods ---------------------------------------------------------------------------------------------
@implementation NVMTL4CommandBuffer
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; _u.label = l; }
- (id<MTLCommandBuffer>)nvmtlUnderlying {
    if (!_u) { _u = [nvmtl_m4_hidden_queue(_dev) commandBuffer]; if (_label) _u.label = _label; }
    return _u;
}
// Contract with the MetalFX side: the Metal 3 recording object between begin and end, nil outside that window.
- (NVMTLCommandBuffer *)nvmtl_recordingCommandBuffer { return (_begun && !_ended && [_u isKindOfClass:[NVMTLCommandBuffer class]]) ? (NVMTLCommandBuffer *)_u : nil; }
- (void)beginCommandBufferWithAllocator:(id<MTL4CommandAllocator>)a {
    if (_committed) { nvlog("Metal 4: beginCommandBufferWithAllocator: on a command buffer that was committed - a new recording starts"); _u = nil; _committed = NO; }
    _u = nil; [self nvmtlUnderlying]; _begun = YES; _ended = NO; [_sets removeAllObjects];
}
- (void)beginCommandBufferWithAllocator:(id<MTL4CommandAllocator>)a options:(MTL4CommandBufferOptions *)o { [self beginCommandBufferWithAllocator:a]; }
- (void)endCommandBuffer {
    if (_openEnc) { nvlog("Metal 4: endCommandBuffer with an open encoder - ended for the app"); [(id)_openEnc endEncoding]; }
    _ended = YES;
}
- (id<MTL4ComputeCommandEncoder>)computeCommandEncoder {
    if (!_begun) { nvlog("Metal 4: computeCommandEncoder before beginCommandBufferWithAllocator:"); return nil; }
    NVMTL4ComputeEncoder *e = [[NVMTL4ComputeEncoder alloc] initWithCommandBuffer:self];
    [self nvmtlUnderlying]; _openEnc = e; return e;
}
- (id<MTL4RenderCommandEncoder>)renderCommandEncoderWithDescriptor:(MTL4RenderPassDescriptor *)d {
    if (!_begun) { nvlog("Metal 4: renderCommandEncoderWithDescriptor: before beginCommandBufferWithAllocator:"); return nil; }
    MTLRenderPassDescriptor *p = [MTLRenderPassDescriptor renderPassDescriptor];
    for (NSUInteger i = 0; i < 8; i++) { MTLRenderPassColorAttachmentDescriptor *c = d.colorAttachments[i]; if (c.texture) p.colorAttachments[i] = c; }
    if (d.depthAttachment.texture) p.depthAttachment = d.depthAttachment;
    if (d.stencilAttachment.texture) p.stencilAttachment = d.stencilAttachment;
    p.renderTargetArrayLength = d.renderTargetArrayLength; p.imageblockSampleLength = d.imageblockSampleLength;
    p.threadgroupMemoryLength = d.threadgroupMemoryLength; p.tileWidth = d.tileWidth; p.tileHeight = d.tileHeight;
    p.defaultRasterSampleCount = d.defaultRasterSampleCount; p.renderTargetWidth = d.renderTargetWidth; p.renderTargetHeight = d.renderTargetHeight;
    p.rasterizationRateMap = d.rasterizationRateMap; p.visibilityResultBuffer = d.visibilityResultBuffer;
    id<MTLRenderCommandEncoder> r = [[self nvmtlUnderlying] renderCommandEncoderWithDescriptor:p];
    if (!r) return nil;
    NVMTL4RenderEncoder *e = [NVMTL4RenderEncoder new]; e->_cb = self; e->_enc = r; _openEnc = e; return e;
}
- (id<MTL4RenderCommandEncoder>)renderCommandEncoderWithDescriptor:(MTL4RenderPassDescriptor *)d options:(MTL4RenderEncoderOptions)o {
    if (o & (MTL4RenderEncoderOptionSuspending | MTL4RenderEncoderOptionResuming)) nvlog("Metal 4: suspending/resuming render encoders are not implemented - the pass is encoded as a complete one");
    return [self renderCommandEncoderWithDescriptor:d];
}
- (id<MTL4MachineLearningCommandEncoder>)machineLearningCommandEncoder { nvlog("Metal 4: machine learning encoders are not implemented"); return nil; }
- (void)useResidencySet:(id<MTLResidencySet>)s { if (!_sets) _sets = [NSMutableArray new]; if (s) [_sets addObject:s]; }   // held until commit; residency is implicit here
- (void)useResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self useResidencySet:s[i]]; }
- (void)pushDebugGroup:(NSString *)s { [[self nvmtlUnderlying] pushDebugGroup:s]; }
- (void)popDebugGroup { [[self nvmtlUnderlying] popDebugGroup]; }
- (void)writeTimestampIntoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i { nvlog("Metal 4: counter heaps are not implemented - timestamp dropped"); }
- (void)resolveCounterHeap:(id<MTL4CounterHeap>)h withRange:(NSRange)r intoBuffer:(MTL4BufferRange)b waitFence:(id<MTLFence>)w updateFence:(id<MTLFence>)u { nvlog("Metal 4: counter heaps are not implemented - resolve dropped"); }
@end

// ---- commit feedback ----------------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4Feedback : NSObject <MTL4CommitFeedback> { @public NSError *_err; CFTimeInterval _t0, _t1; } @end
@implementation NVMTL4Feedback
- (NSError *)error { return _err; }
- (CFTimeInterval)GPUStartTime { return _t0; }
- (CFTimeInterval)GPUEndTime { return _t1; }
@end

// MTL4CommitOptions keeps its handlers in a private ivar; read them through the public selector we also wrap.
API_AVAILABLE(macos(26.0))
static NSArray *nvmtl_m4_feedback_handlers(MTL4CommitOptions *o)
{
    if (!o) return nil;
    static Ivar iv; static dispatch_once_t once;
    dispatch_once(&once, ^{
        unsigned n = 0; Ivar *l = class_copyIvarList([MTL4CommitOptions class], &n);
        for (unsigned i = 0; i < n; i++) { const char *nm = ivar_getName(l[i]); if (nm && (strstr(nm, "andler") || strstr(nm, "eedback"))) { iv = l[i]; break; } }
        free(l);
        if (!iv) nvlog("Metal 4: MTL4CommitOptions has no handler ivar this driver recognises - commit feedback handlers will not run");
    });
    if (!iv) return nil;
    id v = object_getIvar(o, iv);
    if ([v isKindOfClass:[NSArray class]]) return v;
    if (v) return @[v];
    return nil;
}

// ---- queue --------------------------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4Queue : NSObject <MTL4CommandQueue> { @public id<MTLDevice> _dev; NSString *_label; dispatch_queue_t _fb; NSMutableArray *_sets; } @end
@implementation NVMTL4Queue
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)commit:(const id<MTL4CommandBuffer> __unsafe_unretained [])cbs count:(NSUInteger)n { [self commit:cbs count:n options:[MTL4CommitOptions new]]; }
- (void)commit:(const id<MTL4CommandBuffer> __unsafe_unretained [])cbs count:(NSUInteger)n options:(MTL4CommitOptions *)options {
    NSArray *handlers = nvmtl_m4_feedback_handlers(options);
    for (NSUInteger i = 0; i < n; i++) {
        NVMTL4CommandBuffer *c = (NVMTL4CommandBuffer *)cbs[i];
        if (![c isKindOfClass:[NVMTL4CommandBuffer class]]) { nvlog("Metal 4: commit: a command buffer that is not this driver's - skipped"); continue; }
        if (!c->_ended) { nvlog("Metal 4: commit: a command buffer that was not ended (endCommandBuffer) - ended for the app"); [c endCommandBuffer]; }
        id<MTLCommandBuffer> u = c->_u;
        if (!u) { nvlog("Metal 4: commit: an empty command buffer"); continue; }
        NSArray *sets = [c->_sets copy];                                    // keep the sets alive until the GPU is done
        BOOL last = (i == n - 1);
        dispatch_queue_t fq = _fb ?: dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0);
        [u addCompletedHandler:^(id<MTLCommandBuffer> done) {
            (void)sets;
            if (!(last && handlers.count)) return;
            NVMTL4Feedback *fb = [NVMTL4Feedback new]; fb->_err = done.error; fb->_t0 = done.GPUStartTime; fb->_t1 = done.GPUEndTime;
            dispatch_async(fq, ^{ for (id h in handlers) ((MTL4CommitFeedbackHandler)h)(fb); });
        }];
        c->_committed = YES; c->_u = nil; c->_begun = NO;                   // a later begin starts a fresh recording
        [u commit];
    }
}
// Queue-level waits and signals ride a command buffer of their own on the private queue, so they order against commits.
- (void)signalEvent:(id<MTLEvent>)e value:(uint64_t)v {
    id<MTLCommandBuffer> c = [nvmtl_m4_hidden_queue(_dev) commandBuffer]; [c encodeSignalEvent:e value:v]; [c commit];
}
- (void)waitForEvent:(id<MTLEvent>)e value:(uint64_t)v {
    id<MTLCommandBuffer> c = [nvmtl_m4_hidden_queue(_dev) commandBuffer]; [c encodeWaitForEvent:e value:v]; [c commit];
}
// The app presents the drawable itself after signalDrawable:. The drawable's contents are flushed by a command buffer that
// presents nothing, so the later -present finds the frame complete.
- (void)signalDrawable:(id<MTLDrawable>)d {
    id<MTLCommandBuffer> c = [nvmtl_m4_hidden_queue(_dev) commandBuffer];
    if ([c isKindOfClass:[NVMTLCommandBuffer class]]) [(NVMTLCommandBuffer *)c nvmtlRecordDrawable:d presentation:^{}];
    [c commit];
}
- (void)waitForDrawable:(id<MTLDrawable>)d {}
- (void)addResidencySet:(id<MTLResidencySet>)s { @synchronized (self) { if (!_sets) _sets = [NSMutableArray new]; if (s && ![_sets containsObject:s]) [_sets addObject:s]; } }
- (void)addResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self addResidencySet:s[i]]; }
- (void)removeResidencySet:(id<MTLResidencySet>)s { @synchronized (self) { [_sets removeObject:s]; } }
- (void)removeResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self removeResidencySet:s[i]]; }
- (void)updateTextureMappings:(id<MTLTexture>)t heap:(id<MTLHeap>)h operations:(const MTL4UpdateSparseTextureMappingOperation [])o count:(NSUInteger)n { nvlog("Metal 4: sparse texture mapping updates through the queue are not implemented"); }
- (void)copyTextureMappingsFromTexture:(id<MTLTexture>)s toTexture:(id<MTLTexture>)d operations:(const MTL4CopySparseTextureMappingOperation [])o count:(NSUInteger)n { nvlog("Metal 4: sparse texture mapping copies through the queue are not implemented"); }
- (void)updateBufferMappings:(id<MTLBuffer>)b heap:(id<MTLHeap>)h operations:(const MTL4UpdateSparseBufferMappingOperation [])o count:(NSUInteger)n { nvlog("Metal 4: sparse buffer mapping updates through the queue are not implemented"); }
- (void)copyBufferMappingsFromBuffer:(id<MTLBuffer>)s toBuffer:(id<MTLBuffer>)d operations:(const MTL4CopySparseBufferMappingOperation [])o count:(NSUInteger)n { nvlog("Metal 4: sparse buffer mapping copies through the queue are not implemented"); }
@end

// ---- compiler -----------------------------------------------------------------------------------------------------------
API_AVAILABLE(macos(26.0))
@interface NVMTL4CompilerTask : NSObject <MTL4CompilerTask> { @public id<MTL4Compiler> _comp; dispatch_semaphore_t _done; MTL4CompilerTaskStatus _st; } @end
@implementation NVMTL4CompilerTask
- (id<MTL4Compiler>)compiler { return _comp; }
- (MTL4CompilerTaskStatus)status { return _st; }
- (void)waitUntilCompleted { if (_done) dispatch_semaphore_wait(_done, DISPATCH_TIME_FOREVER), dispatch_semaphore_signal(_done); }
@end

API_AVAILABLE(macos(26.0))
@interface NVMTL4Compiler : NSObject <MTL4Compiler> { @public id<MTLDevice> _dev; NSString *_label; id<MTL4PipelineDataSetSerializer> _ser; } @end

// A Metal 4 function descriptor as a Metal 3 function.
API_AVAILABLE(macos(26.0))
static id<MTLFunction> nvmtl_m4_function(MTL4FunctionDescriptor *fd, NSError **err)
{
    if (!fd) return nil;
    if ([fd isKindOfClass:[MTL4LibraryFunctionDescriptor class]]) {
        MTL4LibraryFunctionDescriptor *l = (MTL4LibraryFunctionDescriptor *)fd;
        id<MTLFunction> f = l.name ? [l.library newFunctionWithName:l.name] : nil;
        if (!f && err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorFunctionNotFound userInfo:@{ NSLocalizedDescriptionKey: [NSString stringWithFormat:@"Function '%@' not found in the library", l.name ?: @"(nil)"] }];
        return f;
    }
    if ([fd isKindOfClass:[MTL4SpecializedFunctionDescriptor class]]) {
        MTL4SpecializedFunctionDescriptor *s = (MTL4SpecializedFunctionDescriptor *)fd;
        if (![s.functionDescriptor isKindOfClass:[MTL4LibraryFunctionDescriptor class]]) {
            if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: a specialized function must wrap a library function descriptor" }];
            return nil;
        }
        MTL4LibraryFunctionDescriptor *l = (MTL4LibraryFunctionDescriptor *)s.functionDescriptor;
        MTLFunctionDescriptor *d = [MTLFunctionDescriptor functionDescriptor];
        d.name = l.name; d.specializedName = s.specializedName; d.constantValues = s.constantValues;
        return [l.library newFunctionWithDescriptor:d error:err];
    }
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: [NSString stringWithFormat:@"Metal 4: %@ function descriptors are not implemented", NSStringFromClass([fd class])] }];
    return nil;
}

API_AVAILABLE(macos(26.0))
static void nvmtl_m4_convert_attachments(MTL4RenderPipelineColorAttachmentDescriptorArray *src, MTLRenderPipelineColorAttachmentDescriptorArray *dst)
{
    for (NSUInteger i = 0; i < 8; i++) {
        MTL4RenderPipelineColorAttachmentDescriptor *s = src[i]; MTLRenderPipelineColorAttachmentDescriptor *d = dst[i];
        d.pixelFormat = s.pixelFormat; d.blendingEnabled = s.blendingState == MTL4BlendStateEnabled;
        d.sourceRGBBlendFactor = s.sourceRGBBlendFactor; d.destinationRGBBlendFactor = s.destinationRGBBlendFactor; d.rgbBlendOperation = s.rgbBlendOperation;
        d.sourceAlphaBlendFactor = s.sourceAlphaBlendFactor; d.destinationAlphaBlendFactor = s.destinationAlphaBlendFactor; d.alphaBlendOperation = s.alphaBlendOperation;
        d.writeMask = s.writeMask;
    }
}

@implementation NVMTL4Compiler
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (id<MTL4PipelineDataSetSerializer>)pipelineDataSetSerializer { return _ser; }
- (id<MTLLibrary>)newLibraryWithDescriptor:(MTL4LibraryDescriptor *)d error:(NSError **)err {
    if (!d.source) { if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: a library descriptor needs source" }]; return nil; }
    id<MTLLibrary> l = [_dev newLibraryWithSource:d.source options:d.options error:err];
    if (l && d.name) l.label = d.name;
    return l;
}
- (id<MTLDynamicLibrary>)newDynamicLibrary:(id<MTLLibrary>)lib error:(NSError **)err { return [_dev newDynamicLibrary:lib error:err]; }
- (id<MTLDynamicLibrary>)newDynamicLibraryWithURL:(NSURL *)url error:(NSError **)err { return [_dev newDynamicLibraryWithURL:url error:err]; }
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)err {
    id<MTLFunction> f = nvmtl_m4_function(d.computeFunctionDescriptor, err);
    if (!f) return nil;
    MTLComputePipelineDescriptor *p = [MTLComputePipelineDescriptor new];
    if (d.label) p.label = d.label;   /* Metal asserts on a nil label */ p.computeFunction = f;
    p.threadGroupSizeIsMultipleOfThreadExecutionWidth = d.threadGroupSizeIsMultipleOfThreadExecutionWidth;
    if (d.maxTotalThreadsPerThreadgroup) p.maxTotalThreadsPerThreadgroup = d.maxTotalThreadsPerThreadgroup;
    p.supportIndirectCommandBuffers = d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled;
    return [_dev newComputePipelineStateWithDescriptor:p options:MTLPipelineOptionNone reflection:nil error:err];
}
- (id<MTLComputePipelineState>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d dynamicLinkingDescriptor:(MTL4PipelineStageDynamicLinkingDescriptor *)dl compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)err {
    if (dl) nvlog("Metal 4: dynamic linking descriptors are not implemented - the pipeline is built without them");
    return [self newComputePipelineStateWithDescriptor:d compilerTaskOptions:o error:err];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)desc compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)err {
    if ([desc isKindOfClass:[MTL4RenderPipelineDescriptor class]]) {
        MTL4RenderPipelineDescriptor *d = (MTL4RenderPipelineDescriptor *)desc;
        MTLRenderPipelineDescriptor *p = [MTLRenderPipelineDescriptor new];
        if (d.label) p.label = d.label;   /* Metal asserts on a nil label */
        p.vertexFunction = nvmtl_m4_function(d.vertexFunctionDescriptor, err);
        if (d.vertexFunctionDescriptor && !p.vertexFunction) return nil;
        p.fragmentFunction = nvmtl_m4_function(d.fragmentFunctionDescriptor, err);
        if (d.fragmentFunctionDescriptor && !p.fragmentFunction) return nil;
        p.vertexDescriptor = d.vertexDescriptor; p.rasterSampleCount = d.rasterSampleCount;
        p.alphaToCoverageEnabled = d.alphaToCoverageState == MTL4AlphaToCoverageStateEnabled;
        p.alphaToOneEnabled = d.alphaToOneState == MTL4AlphaToOneStateEnabled;
        p.rasterizationEnabled = d.rasterizationEnabled; p.maxVertexAmplificationCount = d.maxVertexAmplificationCount;
        p.inputPrimitiveTopology = d.inputPrimitiveTopology;
        p.supportIndirectCommandBuffers = d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled;
        nvmtl_m4_convert_attachments(d.colorAttachments, p.colorAttachments);
        return [_dev newRenderPipelineStateWithDescriptor:p error:err];
    }
    if ([desc isKindOfClass:[MTL4MeshRenderPipelineDescriptor class]]) {
        MTL4MeshRenderPipelineDescriptor *d = (MTL4MeshRenderPipelineDescriptor *)desc;
        MTLMeshRenderPipelineDescriptor *p = [MTLMeshRenderPipelineDescriptor new];
        if (d.label) p.label = d.label;   /* Metal asserts on a nil label */
        p.objectFunction = nvmtl_m4_function(d.objectFunctionDescriptor, err);
        if (d.objectFunctionDescriptor && !p.objectFunction) return nil;
        p.meshFunction = nvmtl_m4_function(d.meshFunctionDescriptor, err);
        if (d.meshFunctionDescriptor && !p.meshFunction) return nil;
        p.fragmentFunction = nvmtl_m4_function(d.fragmentFunctionDescriptor, err);
        if (d.fragmentFunctionDescriptor && !p.fragmentFunction) return nil;
        p.maxTotalThreadsPerObjectThreadgroup = d.maxTotalThreadsPerObjectThreadgroup; p.maxTotalThreadsPerMeshThreadgroup = d.maxTotalThreadsPerMeshThreadgroup;
        p.requiredThreadsPerObjectThreadgroup = d.requiredThreadsPerObjectThreadgroup; p.requiredThreadsPerMeshThreadgroup = d.requiredThreadsPerMeshThreadgroup;
        p.objectThreadgroupSizeIsMultipleOfThreadExecutionWidth = d.objectThreadgroupSizeIsMultipleOfThreadExecutionWidth;
        p.meshThreadgroupSizeIsMultipleOfThreadExecutionWidth = d.meshThreadgroupSizeIsMultipleOfThreadExecutionWidth;
        p.payloadMemoryLength = d.payloadMemoryLength; p.maxTotalThreadgroupsPerMeshGrid = d.maxTotalThreadgroupsPerMeshGrid;
        p.rasterSampleCount = d.rasterSampleCount;
        p.alphaToCoverageEnabled = d.alphaToCoverageState == MTL4AlphaToCoverageStateEnabled;
        p.alphaToOneEnabled = d.alphaToOneState == MTL4AlphaToOneStateEnabled;
        p.rasterizationEnabled = d.rasterizationEnabled; p.maxVertexAmplificationCount = d.maxVertexAmplificationCount;
        p.supportIndirectCommandBuffers = d.supportIndirectCommandBuffers == MTL4IndirectCommandBufferSupportStateEnabled;
        nvmtl_m4_convert_attachments(d.colorAttachments, p.colorAttachments);
        return [_dev newRenderPipelineStateWithMeshDescriptor:p options:MTLPipelineOptionNone reflection:nil error:err];
    }
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: [NSString stringWithFormat:@"Metal 4: %@ render pipelines are not implemented", NSStringFromClass([desc class])] }];
    return nil;
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d dynamicLinkingDescriptor:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)err {
    if (dl) nvlog("Metal 4: dynamic linking descriptors are not implemented - the pipeline is built without them");
    return [self newRenderPipelineStateWithDescriptor:d compilerTaskOptions:o error:err];
}
- (id<MTLRenderPipelineState>)newRenderPipelineStateBySpecializationWithDescriptor:(MTL4PipelineDescriptor *)d pipeline:(id<MTLRenderPipelineState>)p error:(NSError **)err {
    return [self newRenderPipelineStateWithDescriptor:d compilerTaskOptions:nil error:err];   // specialization is an optimisation; a full build gives the same pipeline
}
- (id<MTL4BinaryFunction>)newBinaryFunctionWithDescriptor:(MTL4BinaryFunctionDescriptor *)d compilerTaskOptions:(MTL4CompilerTaskOptions *)o error:(NSError **)err {
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: binary functions are not implemented" }];
    return nil;
}
- (id<MTL4MachineLearningPipelineState>)newMachineLearningPipelineStateWithDescriptor:(MTL4MachineLearningPipelineDescriptor *)d error:(NSError **)err {
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: machine learning pipelines are not implemented" }];
    return nil;
}
// The completion-handler forms run the synchronous build on a global queue and report through the handler.
- (id<MTL4CompilerTask>)nvmtlTask:(void (^)(void))work {
    NVMTL4CompilerTask *t = [NVMTL4CompilerTask new]; t->_comp = self; t->_done = dispatch_semaphore_create(1); t->_st = MTL4CompilerTaskStatusScheduled;
    dispatch_semaphore_wait(t->_done, DISPATCH_TIME_FOREVER);
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{ t->_st = MTL4CompilerTaskStatusCompiling; work(); t->_st = MTL4CompilerTaskStatusFinished; dispatch_semaphore_signal(t->_done); });
    return t;
}
- (id<MTL4CompilerTask>)newLibraryWithDescriptor:(MTL4LibraryDescriptor *)d completionHandler:(MTLNewLibraryCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLLibrary> l = [self newLibraryWithDescriptor:d error:&e]; h(l, e); }];
}
- (id<MTL4CompilerTask>)newDynamicLibrary:(id<MTLLibrary>)lib completionHandler:(MTLNewDynamicLibraryCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLDynamicLibrary> l = [self newDynamicLibrary:lib error:&e]; h(l, e); }];
}
- (id<MTL4CompilerTask>)newDynamicLibraryWithURL:(NSURL *)url completionHandler:(MTLNewDynamicLibraryCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLDynamicLibrary> l = [self newDynamicLibraryWithURL:url error:&e]; h(l, e); }];
}
- (id<MTL4CompilerTask>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d compilerTaskOptions:(MTL4CompilerTaskOptions *)o completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLComputePipelineState> p = [self newComputePipelineStateWithDescriptor:d compilerTaskOptions:o error:&e]; h(p, e); }];
}
- (id<MTL4CompilerTask>)newComputePipelineStateWithDescriptor:(MTL4ComputePipelineDescriptor *)d dynamicLinkingDescriptor:(MTL4PipelineStageDynamicLinkingDescriptor *)dl compilerTaskOptions:(MTL4CompilerTaskOptions *)o completionHandler:(MTLNewComputePipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLComputePipelineState> p = [self newComputePipelineStateWithDescriptor:d dynamicLinkingDescriptor:dl compilerTaskOptions:o error:&e]; h(p, e); }];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d compilerTaskOptions:(MTL4CompilerTaskOptions *)o completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLRenderPipelineState> p = [self newRenderPipelineStateWithDescriptor:d compilerTaskOptions:o error:&e]; h(p, e); }];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateWithDescriptor:(MTL4PipelineDescriptor *)d dynamicLinkingDescriptor:(MTL4RenderPipelineDynamicLinkingDescriptor *)dl compilerTaskOptions:(MTL4CompilerTaskOptions *)o completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLRenderPipelineState> p = [self newRenderPipelineStateWithDescriptor:d dynamicLinkingDescriptor:dl compilerTaskOptions:o error:&e]; h(p, e); }];
}
- (id<MTL4CompilerTask>)newRenderPipelineStateBySpecializationWithDescriptor:(MTL4PipelineDescriptor *)d pipeline:(id<MTLRenderPipelineState>)p completionHandler:(MTLNewRenderPipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTLRenderPipelineState> r = [self newRenderPipelineStateBySpecializationWithDescriptor:d pipeline:p error:&e]; h(r, e); }];
}
- (id<MTL4CompilerTask>)newBinaryFunctionWithDescriptor:(MTL4BinaryFunctionDescriptor *)d compilerTaskOptions:(MTL4CompilerTaskOptions *)o completionHandler:(MTL4NewBinaryFunctionCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTL4BinaryFunction> f = [self newBinaryFunctionWithDescriptor:d compilerTaskOptions:o error:&e]; h(f, e); }];
}
- (id<MTL4CompilerTask>)newMachineLearningPipelineStateWithDescriptor:(MTL4MachineLearningPipelineDescriptor *)d completionHandler:(MTL4NewMachineLearningPipelineStateCompletionHandler)h {
    return [self nvmtlTask:^{ NSError *e = nil; id<MTL4MachineLearningPipelineState> p = [self newMachineLearningPipelineStateWithDescriptor:d error:&e]; h(p, e); }];
}
@end

// ---- the device's Metal 4 entry points ----------------------------------------------------------------------------------
// Signatures use plain `id` (the Metal 4 protocol types do not exist on macOS 15, which this plugin also loads on) and every
// body is guarded; with the knob off or on an older OS each answers nil, as a device without Metal 4 does.
@implementation NVMTLDevice (NVMTLMetal4)
- (id)newMTL4CommandQueue {
    if (@available(macOS 26.0, *)) {
        if (!nvmtl_m4_enabled() || !nvmtl_m4_hidden_queue(self)) return nil;
        NVMTL4Queue *q = [NVMTL4Queue new]; q->_dev = self; nvlog("Metal 4: newMTL4CommandQueue -> %p", (__bridge void *)q); return q;
    }
    return nil;
}
- (id)newMTL4CommandQueueWithDescriptor:(id)descriptor error:(NSError **)err {
    if (@available(macOS 26.0, *)) {
        MTL4CommandQueueDescriptor *d = descriptor;
        NVMTL4Queue *q = (NVMTL4Queue *)[self newMTL4CommandQueue];
        if (!q) { if (err) *err = [NSError errorWithDomain:MTL4CommandQueueErrorDomain code:MTL4CommandQueueErrorNotPermitted userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4 is off (NVMTL_METAL4=0) or the queue could not be made" }]; return nil; }
        q->_label = [d.label copy]; q->_fb = d.feedbackQueue;
        return q;
    }
    return nil;
}
- (id)newCommandAllocator {
    if (@available(macOS 26.0, *)) { if (!nvmtl_m4_enabled()) return nil; NVMTL4Allocator *a = [NVMTL4Allocator new]; a->_dev = self; return a; }
    return nil;
}
- (id)newCommandAllocatorWithDescriptor:(id)descriptor error:(NSError **)err {
    if (@available(macOS 26.0, *)) {
        MTL4CommandAllocatorDescriptor *d = descriptor;
        NVMTL4Allocator *a = (NVMTL4Allocator *)[self newCommandAllocator];
        if (a) a->_label = [d.label copy];
        return a;
    }
    return nil;
}
- (id)newCommandBuffer {
    if (@available(macOS 26.0, *)) { if (!nvmtl_m4_enabled()) return nil; NVMTL4CommandBuffer *c = [NVMTL4CommandBuffer new]; c->_dev = self; return c; }
    return nil;
}
- (id)newArgumentTableWithDescriptor:(id)descriptor error:(NSError **)err {
    if (@available(macOS 26.0, *)) {
        if (!nvmtl_m4_enabled()) return nil;
        return [[NVMTL4ArgumentTable alloc] initWithDescriptor:descriptor device:self];
    }
    return nil;
}
- (id)newCompilerWithDescriptor:(id)descriptor error:(NSError **)err {
    if (@available(macOS 26.0, *)) {
        MTL4CompilerDescriptor *d = descriptor;
        if (!nvmtl_m4_enabled()) return nil;
        NVMTL4Compiler *c = [NVMTL4Compiler new]; c->_dev = self; c->_label = [d.label copy]; c->_ser = d.pipelineDataSetSerializer; return c;
    }
    return nil;
}
- (id)newArchiveWithURL:(NSURL *)url error:(NSError **)err {
    if (err) *err = [NSError errorWithDomain:MTLLibraryErrorDomain code:MTLLibraryErrorUnsupported userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: pipeline archives are not implemented" }];
    return nil;
}
- (id)newCounterHeapWithDescriptor:(id)descriptor error:(NSError **)err {
    if (err) *err = [NSError errorWithDomain:MTLCounterErrorDomain code:MTLCounterSampleBufferErrorInvalid userInfo:@{ NSLocalizedDescriptionKey: @"Metal 4: counter heaps are not implemented" }];
    return nil;
}
@end
#pragma clang attribute pop
