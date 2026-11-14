/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Metal 4 command submission as an adapter over the Metal 3 objects.
 *
 * A Metal 4 command buffer is not tied to a queue when it is made, may be committed to any Metal 4 queue and may be
 * committed more than once, while the Metal 3 command buffer is begun on a queue (a Vulkan command buffer exists from
 * that moment). So encoding records. Each encoder keeps a list of blocks, endEncoding seals the list into one block on
 * the command buffer, and a commit makes a fresh Metal 3 command buffer on the queue's own Metal 3 queue, replays the
 * blocks into real Metal 3 encoders and commits that. The queue order of commits is the order of the replays.
 *
 * Argument tables bind by GPU address and resource ID. A draw or dispatch takes a snapshot of the table, as the Metal 4
 * headers specify; the snapshot resolves each address back to a buffer and offset through a registry fed by
 * -[NVMTLBuffer gpuAddress], and each resource ID through the same table -gpuResourceID fills.
 *
 * Imported after NVMTLEmbedded.m because the resource ID lookup is private to it.
 */

#import <os/lock.h>

#define NVMTL4 API_AVAILABLE(macos(26.0))

#pragma mark - log once

// Per-frame calls that cannot work would otherwise write one line per call.
static void nvmtl4_note_once(NSString *text)
{
    static NSMutableSet<NSString *> *seen; static os_unfair_lock lk = OS_UNFAIR_LOCK_INIT;
    BOOL fresh;
    os_unfair_lock_lock(&lk);
    if (!seen) seen = [NSMutableSet new];
    fresh = ![seen containsObject:text];
    if (fresh && seen.count < 512) [seen addObject:text];
    os_unfair_lock_unlock(&lk);
    if (fresh) nvlog("mtl4: %s", text.UTF8String);
}

#pragma mark - GPU address registry

@interface NVMTL4AddrEntry : NSObject { @public uint64_t base, len; __weak NVMTLBuffer *buf; } @end
@implementation NVMTL4AddrEntry @end

static NSMutableArray<NVMTL4AddrEntry *> *nvmtl4_addrs;   // sorted by base
static os_unfair_lock nvmtl4_addr_lock = OS_UNFAIR_LOCK_INIT;
static NSUInteger nvmtl4_addr_sweep_at = 1024;

// First index whose base is greater than addr. The caller holds the lock.
static NSUInteger nvmtl4_addr_upper(uint64_t addr)
{
    NSUInteger lo = 0, hi = nvmtl4_addrs.count;
    while (lo < hi) {
        NSUInteger mid = (lo + hi) / 2;
        if (nvmtl4_addrs[mid]->base <= addr) lo = mid + 1; else hi = mid;
    }
    return lo;
}

// Bumped whenever an existing entry changes owner or length. A buffer whose cached note (base, end, epoch) matches the
// current epoch is still exactly what the table holds for it, so -gpuAddress need not take the lock again.
// Measured on Cyberpunk: every -gpuAddress took this lock and searched the table; with nine render threads asking for the
// same buffers' addresses that was 1,266 of 22,300 busy render-thread samples. NVMTL_M4_NOTE_FAST=0 puts every call back
// through the table.
static _Atomic uint64_t nvmtl4_addr_epoch = 1;
static int nvmtl4_note_fast(void)
{
    static int on = -1;
    if (on < 0) { const char *e = getenv("NVMTL_M4_NOTE_FAST"); on = !(e && e[0] == '0');
        nvlog("Metal 4: gpuAddress table %s (NVMTL_M4_NOTE_FAST=%s)", on ? "skipped for a buffer already noted at the same range" : "updated on every call", e ? e : "unset"); }
    return on;
}
int nvmtl4_note_current(_Atomic uint64_t note[3], uint64_t addr, uint64_t length)
{
    if (!addr) return 1;   // never entered in the table either
    if (!nvmtl4_note_fast()) return 0;
    const uint64_t epoch = atomic_load_explicit(&note[2], memory_order_acquire);
    return epoch && epoch == atomic_load_explicit(&nvmtl4_addr_epoch, memory_order_acquire) &&
           atomic_load_explicit(&note[0], memory_order_relaxed) == addr &&
           atomic_load_explicit(&note[1], memory_order_relaxed) == addr + length;
}
static void nvmtl4_note_locked(NVMTLBuffer *b, uint64_t addr, uint64_t len);
// Called from -gpuAddress. A Metal 4 app only knows an address it read from a buffer, so every address it can hand back
// was registered here first.
void nvmtl4_note_buffer(NVMTLBuffer *b, uint64_t addr)
{
    if (!b || !addr) return;
    os_unfair_lock_lock(&nvmtl4_addr_lock);
    nvmtl4_note_locked(b, addr, b->_b.size);
    os_unfair_lock_unlock(&nvmtl4_addr_lock);
}
// The same, and the buffer's note records what the table now holds for it (read under the lock, after this call's own
// bump: a later owner change bumps again and invalidates the note).
void nvmtl4_note_buffer_cached(NVMTLBuffer *b, uint64_t addr, _Atomic uint64_t note[3])
{
    if (!b || !addr) return;
    const uint64_t len = b->_b.size;
    os_unfair_lock_lock(&nvmtl4_addr_lock);
    nvmtl4_note_locked(b, addr, len);
    atomic_store_explicit(&note[0], addr, memory_order_relaxed);
    atomic_store_explicit(&note[1], addr + len, memory_order_relaxed);
    atomic_store_explicit(&note[2], atomic_load_explicit(&nvmtl4_addr_epoch, memory_order_relaxed), memory_order_release);
    os_unfair_lock_unlock(&nvmtl4_addr_lock);
}
static void nvmtl4_note_locked(NVMTLBuffer *b, uint64_t addr, uint64_t len)
{
    if (!nvmtl4_addrs) nvmtl4_addrs = [NSMutableArray new];
    NSUInteger up = nvmtl4_addr_upper(addr);
    if (up > 0) {
        NVMTL4AddrEntry *e = nvmtl4_addrs[up - 1];
        if (e->base == addr) {
            if (e->buf != b || e->len != len) atomic_fetch_add_explicit(&nvmtl4_addr_epoch, 1, memory_order_release);
            if (e->buf != b) { e->buf = b; }
            e->len = len;
            return;
        }
    }
    NVMTL4AddrEntry *e = [NVMTL4AddrEntry new];
    e->base = addr; e->len = len; e->buf = b;
    [nvmtl4_addrs insertObject:e atIndex:up];
    if (nvmtl4_addrs.count >= nvmtl4_addr_sweep_at) {
        // Entries of freed buffers hold only a dead weak reference; drop them so the table follows the live set.
        NSIndexSet *dead = [nvmtl4_addrs indexesOfObjectsPassingTest:^BOOL(NVMTL4AddrEntry *x, NSUInteger i, BOOL *stop) { return x->buf == nil; }];
        [nvmtl4_addrs removeObjectsAtIndexes:dead];
        nvmtl4_addr_sweep_at = MAX((NSUInteger)1024, nvmtl4_addrs.count * 2);
    }
}

// The buffer that contains a GPU address and the byte offset of the address in it.
static NVMTLBuffer *nvmtl4_resolve_address(uint64_t addr, NSUInteger *offset)
{
    if (!addr) return nil;
    NVMTLBuffer *found = nil;
    os_unfair_lock_lock(&nvmtl4_addr_lock);
    NSUInteger up = nvmtl4_addrs ? nvmtl4_addr_upper(addr) : 0;
    // Aliased allocations can overlap, so look back past an entry that does not contain the address.
    for (NSUInteger k = 0; k < 4 && up > k; k++) {
        NVMTL4AddrEntry *e = nvmtl4_addrs[up - 1 - k];
        NVMTLBuffer *b = e->buf;
        if (b && addr >= e->base && addr - e->base < e->len) { found = b; if (offset) *offset = (NSUInteger)(addr - e->base); break; }
    }
    os_unfair_lock_unlock(&nvmtl4_addr_lock);
    return found;
}

@interface NVMTLRenderCommandEncoder (NVMTL4ColorMap)
- (void)nvmtl4SetColorMap:(id)m;   // MTLLogicalToPhysicalColorAttachmentMap, macOS 26
- (void)nvmtl4PrepareColorMapForPipeline:(id<MTLRenderPipelineState>)ps;
@end

#pragma mark - hooks for work that lives elsewhere

// Defined in NVMTL4ML.m, which is imported after this file.
id<MTLBuffer> nvmtl4_tensor_entry_buffer(uint64_t id, NSUInteger *offset);

// Each hook is a plain function pointer that the component owning the feature assigns at load. While one is NULL the
// call it serves reports unsupported, and the commit feedback carries the error.

// Counter heaps. encoder is the live Metal 3 encoder when the call came from inside one, else nil. Returns YES when
// the write was recorded.
BOOL (*nvmtl4_hook_write_timestamp)(id encoder, id<MTLCommandBuffer> cb, id heap, NSUInteger index, NSInteger granularity) = NULL;
BOOL (*nvmtl4_hook_resolve_heap)(id<MTLCommandBuffer> cb, id heap, NSRange range, NVMTLBuffer *dst, NSUInteger dstOffset,
                                 id<MTLFence> waitFence, id<MTLFence> updateFence) = NULL;

// Machine learning encoder. Called when the app asks the command buffer for one. addReplayOp appends a block that runs
// when the command buffer is replayed onto a Metal 3 command buffer, after every other open encoder was closed; the
// block receives that Metal 3 command buffer. The hook returns the encoder, or nil with the reason logged.
// cb is the id<MTL4CommandBuffer>; the result is an id<MTL4MachineLearningCommandEncoder> (typed id so this file's hooks
// need no availability on their own).
id (*nvmtl4_hook_ml_encoder)(id cb, void (^addReplayOp)(void (^op)(id<MTLCommandBuffer> m3cb))) = NULL;

// Tensors that are not wrapped around an app buffer have no public buffer property. The tensor owner provides its
// backing for copies: the buffer and byte offset of element zero, the rank, the dimensions and strides in elements
// (dimension 0 innermost) and the element size in bytes. Wrapped tensors need no hook; their public properties are used.
// tensor is the id<MTLTensor>.
BOOL (*nvmtl4_hook_tensor_backing)(id tensor, __unsafe_unretained id<MTLBuffer> *buffer, NSUInteger *offset, NSUInteger *rank,
                                   NSInteger *dims, NSInteger *strides, size_t *elementSize) = NULL;

// Sparse resource mapping on the queue. kind: 0 updateTextureMappings, 1 copyTextureMappings, 2 updateBufferMappings,
// 3 copyBufferMappings. resource is the resource to map (the destination for a copy), source the copy's source, heap the
// heap pages come from. ops points at count operation structs of the matching MTL4 type, valid for the call. The call
// runs in queue order with m3cb as the Metal 3 command buffer it is ordered against. Returns YES when applied.
BOOL (*nvmtl4_hook_sparse_update)(id<MTLCommandBuffer> m3cb, int kind, id resource, id source, id heap, const void *ops, NSUInteger count) = NULL;

// Tile shading needs no hook of its own. Argument tables at the tile stage bind through the Metal 3 render encoder's
// setTileBuffer:offset:atIndex:, setTileTexture:atIndex: and setTileSamplerState:atIndex:, and dispatchThreadsPerTile:
// and setThreadgroupMemoryLength:offset:atIndex: are sent as they are. Whatever implements those on the render encoder
// implements them here.

#pragma mark - argument table

typedef NS_ENUM(int, NVMTL4Stage) { NVMTL4StageVertex, NVMTL4StageFragment, NVMTL4StageCompute, NVMTL4StageObject, NVMTL4StageMesh, NVMTL4StageTile };

// An immutable copy of an argument table, made when a draw or dispatch is encoded. Kind per slot: 0 never set,
// 1 address, 2 resource ID, 3 set but unresolvable.
NVMTL4 @interface NVMTL4Snap : NSObject {
@public
    NSUInteger nb, nt, ns;
    uint8_t *bk, *tk, *sk, *bstr;
    uint64_t *baddr, *tid, *sid;
    NSUInteger *boff, *bstride;
    NSArray *bobj, *tobj, *sobj;   // NSNull where the slot holds nil
}
@end
@implementation NVMTL4Snap
- (instancetype)initWithBuffers:(NSUInteger)b textures:(NSUInteger)t samplers:(NSUInteger)s {
    if ((self = [super init])) {
        nb = b; nt = t; ns = s;
        bk = calloc(b + 1, 1); bstr = calloc(b + 1, 1); baddr = calloc(b + 1, 8); boff = calloc(b + 1, sizeof(NSUInteger)); bstride = calloc(b + 1, sizeof(NSUInteger));
        tk = calloc(t + 1, 1); tid = calloc(t + 1, 8);
        sk = calloc(s + 1, 1); sid = calloc(s + 1, 8);
    }
    return self;
}
- (void)dealloc { free(bk); free(bstr); free(baddr); free(boff); free(bstride); free(tk); free(tid); free(sk); free(sid); }
@end

NVMTL4 @interface NVMTL4ArgumentTable : NSObject <MTL4ArgumentTable> {
@public
    id _dev; NSString *_label;
    NSUInteger _nb, _nt, _ns; BOOL _strides;
    uint8_t *_bk, *_tk, *_sk, *_bstr;
    uint64_t *_baddr, *_tid, *_sid; NSUInteger *_bstride;
    os_unfair_lock _lk; uint64_t _gen, _cacheGen; NVMTL4Snap *_cache;
}
- (NVMTL4Snap *)nvmtl4Snapshot;
@end

@implementation NVMTL4ArgumentTable
- (instancetype)initWithDescriptor:(MTL4ArgumentTableDescriptor *)d device:(id)dev {
    if ((self = [super init])) {
        _dev = dev; _label = [d.label copy]; _lk = OS_UNFAIR_LOCK_INIT;
        _nb = d.maxBufferBindCount; _nt = d.maxTextureBindCount; _ns = d.maxSamplerStateBindCount; _strides = d.supportAttributeStrides;
        _bk = calloc(_nb + 1, 1); _bstr = calloc(_nb + 1, 1); _baddr = calloc(_nb + 1, 8); _bstride = calloc(_nb + 1, sizeof(NSUInteger));
        _tk = calloc(_nt + 1, 1); _tid = calloc(_nt + 1, 8);
        _sk = calloc(_ns + 1, 1); _sid = calloc(_ns + 1, 8);
        _gen = 1;
    }
    return self;
}
- (void)dealloc { free(_bk); free(_bstr); free(_baddr); free(_bstride); free(_tk); free(_tid); free(_sk); free(_sid); }
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (BOOL)nvmtl4Index:(NSUInteger)i limit:(NSUInteger)n what:(const char *)what {
    if (i < n) return YES;
    nvlog("mtl4: argument table %s index %lu is outside the %lu the table was made with - REFUSED", what, (unsigned long)i, (unsigned long)n);
    return NO;
}
- (void)setAddress:(MTLGPUAddress)a atIndex:(NSUInteger)i {
    if (![self nvmtl4Index:i limit:_nb what:"buffer"]) return;
    os_unfair_lock_lock(&_lk); _bk[i] = 1; _baddr[i] = a; _bstr[i] = 0; _gen++; os_unfair_lock_unlock(&_lk);
}
- (void)setAddress:(MTLGPUAddress)a attributeStride:(NSUInteger)st atIndex:(NSUInteger)i {
    if (![self nvmtl4Index:i limit:_nb what:"buffer"]) return;
    if (!_strides) nvmtl4_note_once(@"argument table: attribute stride bound on a table made without supportAttributeStrides - honoured anyway");
    os_unfair_lock_lock(&_lk); _bk[i] = 1; _baddr[i] = a; _bstr[i] = 1; _bstride[i] = st; _gen++; os_unfair_lock_unlock(&_lk);
}
- (void)setResource:(MTLResourceID)r atBufferIndex:(NSUInteger)i {
    if (![self nvmtl4Index:i limit:_nb what:"buffer"]) return;
    os_unfair_lock_lock(&_lk); _bk[i] = 2; _baddr[i] = r._impl; _bstr[i] = 0; _gen++; os_unfair_lock_unlock(&_lk);
}
- (void)setTexture:(MTLResourceID)r atIndex:(NSUInteger)i {
    if (![self nvmtl4Index:i limit:_nt what:"texture"]) return;
    os_unfair_lock_lock(&_lk); _tk[i] = 2; _tid[i] = r._impl; _gen++; os_unfair_lock_unlock(&_lk);
}
- (void)setSamplerState:(MTLResourceID)r atIndex:(NSUInteger)i {
    if (![self nvmtl4Index:i limit:_ns what:"sampler"]) return;
    os_unfair_lock_lock(&_lk); _sk[i] = 2; _sid[i] = r._impl; _gen++; os_unfair_lock_unlock(&_lk);
}
// The same snapshot object is handed out until a bind changes the table, so a replay recognises an unchanged table by
// pointer and a change by comparing slots.
- (NVMTL4Snap *)nvmtl4Snapshot {
    os_unfair_lock_lock(&_lk);
    if (_cache && _cacheGen == _gen) { NVMTL4Snap *c = _cache; os_unfair_lock_unlock(&_lk); return c; }
    NVMTL4Snap *s = [[NVMTL4Snap alloc] initWithBuffers:_nb textures:_nt samplers:_ns];
    memcpy(s->bk, _bk, _nb); memcpy(s->bstr, _bstr, _nb); memcpy(s->baddr, _baddr, _nb * 8); memcpy(s->bstride, _bstride, _nb * sizeof(NSUInteger));
    memcpy(s->tk, _tk, _nt); memcpy(s->tid, _tid, _nt * 8);
    memcpy(s->sk, _sk, _ns); memcpy(s->sid, _sid, _ns * 8);
    uint64_t gen = _gen;
    os_unfair_lock_unlock(&_lk);

    NSMutableArray *bo = [NSMutableArray arrayWithCapacity:_nb], *to = [NSMutableArray arrayWithCapacity:_nt], *so = [NSMutableArray arrayWithCapacity:_ns];
    for (NSUInteger i = 0; i < s->nb; i++) {
        id o = [NSNull null];
        if (s->bk[i] == 1 && s->baddr[i]) {
            NSUInteger off = 0; NVMTLBuffer *b = nvmtl4_resolve_address(s->baddr[i], &off);
            if (b) { o = b; s->boff[i] = off; }
            else { s->bk[i] = 3; nvlog("mtl4: argument table buffer index %lu holds address 0x%llx that no buffer owns - cannot bind", (unsigned long)i, (unsigned long long)s->baddr[i]); }
        } else if (s->bk[i] == 2 && s->baddr[i]) {
            // A resource ID in a buffer slot names an acceleration structure, a function table or a command buffer; the
            // ones that are buffers here are found by address.
            NSUInteger off = 0;
            // A tensor reaches a shader as a pointer to its table entry. Tensor ids carry their own tag bit
            // (NVMTL_RESID_TENSOR), so they are told apart from every other resource id before the general lookup.
            id r = nvmtl4_tensor_for_resource_id(s->baddr[i]) ? nvmtl4_tensor_entry_buffer(s->baddr[i], &off) : nil;
            if (!r) r = nvmtl_resid_lookup(s->baddr[i]);
            if (!r) r = nvmtl4_resolve_address(s->baddr[i], &off);
            if (r) { o = r; s->boff[i] = off; }
            else { s->bk[i] = 3; nvlog("mtl4: argument table buffer index %lu holds resource ID 0x%llx that names nothing here - cannot bind", (unsigned long)i, (unsigned long long)s->baddr[i]); }
        }
        [bo addObject:o];
    }
    for (NSUInteger i = 0; i < s->nt; i++) {
        id o = [NSNull null];
        if (s->tk[i] && s->tid[i]) {
            id r = nvmtl_resid_lookup(s->tid[i]);
            if (r) o = r;
            else { s->tk[i] = 3; nvlog("mtl4: argument table texture index %lu holds resource ID 0x%llx that names no texture - cannot bind", (unsigned long)i, (unsigned long long)s->tid[i]); }
        }
        [to addObject:o];
    }
    for (NSUInteger i = 0; i < s->ns; i++) {
        id o = [NSNull null];
        if (s->sk[i] && s->sid[i]) {
            id r = nvmtl_resid_lookup(s->sid[i]);
            if (r) o = r;
            else { s->sk[i] = 3; nvlog("mtl4: argument table sampler index %lu holds resource ID 0x%llx that names no sampler - cannot bind", (unsigned long)i, (unsigned long long)s->sid[i]); }
        }
        [so addObject:o];
    }
    s->bobj = bo; s->tobj = to; s->sobj = so;
    os_unfair_lock_lock(&_lk);
    if (_gen == gen) { _cache = s; _cacheGen = gen; }
    os_unfair_lock_unlock(&_lk);
    return s;
}
@end

#pragma mark - replay

typedef void (^nvmtl4_op)(id r);

// State of one replay of a Metal 4 command buffer onto a Metal 3 command buffer.
NVMTL4 @interface NVMTL4Replay : NSObject {
@public
    id<MTLCommandBuffer> cb; NVMTLCommandBuffer *ncb;
    id<MTLRenderCommandEncoder> render;
    id<MTLComputeCommandEncoder> comp;
    id<MTLBlitCommandEncoder> blit;
    id<MTLAccelerationStructureCommandEncoder> asEnc;
    BOOL splitPending, haveArgs, gpuWork, queueIdle;   // indirect threads dispatch: see splitForArgsFrom
    uint32_t args[6];
    id<MTLBuffer> stage;
    BOOL suspended;                   // render is a pass a suspending encoder left open for the next encoder to resume
    NVMTL4Snap *appV, *appF, *appO, *appM, *appT, *appC;   // what the live encoder has bound
    NVMTL4Snap *curC;                 // what the next dispatch must see
    id<MTLComputePipelineState> pso;
    NSMutableDictionary<NSNumber *, NSNumber *> *tg;
    NSUInteger imgW, imgH;
    NSString *encLabel;
    NSMutableArray<NSString *> *errors;
}
- (void)unsupported:(NSString *)what;
- (id<MTLComputeCommandEncoder>)compute;
- (id<MTLBlitCommandEncoder>)blitEnc;
- (void)closeCompute;
- (void)closeAll;
- (void)endForSplit;
- (void)splitForArgsFrom:(NVMTLBuffer *)b offset:(NSUInteger)off;
- (void)endSuspended:(NSString *)why;
- (id<MTLAccelerationStructureCommandEncoder>)asEncoder;
- (void)barrier;
- (void)syncV:(NVMTL4Snap *)v F:(NVMTL4Snap *)f O:(NVMTL4Snap *)o M:(NVMTL4Snap *)m T:(NVMTL4Snap *)t;
@end

static id nvmtl4_nul(id o) { return [o isKindOfClass:[NSNull class]] ? nil : o; }

// Binds what differs between prev and s onto enc for one stage.
NVMTL4 static void nvmtl4_apply(NVMTL4Replay *r, NVMTL4Snap *s, NVMTL4Snap *prev, int st, id enc)
{
    for (NSUInteger i = 0; i < s->nb; i++) {
        if (!s->bk[i]) continue;
        if (s->bk[i] == 3) { [r unsupported:[NSString stringWithFormat:@"argument table buffer index %lu could not be resolved", (unsigned long)i]]; continue; }
        id o = nvmtl4_nul(s->bobj[i]);
        if (prev && i < prev->nb && prev->bk[i] == s->bk[i] && prev->baddr[i] == s->baddr[i] && prev->bstr[i] == s->bstr[i]
            && prev->bstride[i] == s->bstride[i] && nvmtl4_nul(prev->bobj[i]) == o) continue;
        NSUInteger off = s->boff[i];
        BOOL as = o && [o isKindOfClass:[NVMTLAccelerationStructure class]];
        if (o && s->bk[i] == 2 && !as && ![o isKindOfClass:[NVMTLBuffer class]]) {
            [r unsupported:[NSString stringWithFormat:@"setResource:atBufferIndex: with a %s", class_getName([o class])]]; continue; }
        const BOOL str = s->bstr[i] != 0;
        switch (st) {
        case NVMTL4StageVertex: {
            id<MTLRenderCommandEncoder> e = enc;
            if (as && [e respondsToSelector:@selector(setVertexAccelerationStructure:atBufferIndex:)]) [e setVertexAccelerationStructure:o atBufferIndex:i];
            else if (str) [e setVertexBuffer:o offset:off attributeStride:s->bstride[i] atIndex:i];
            else [e setVertexBuffer:o offset:off atIndex:i];
            break; }
        case NVMTL4StageFragment: {
            id<MTLRenderCommandEncoder> e = enc;
            if (as && [e respondsToSelector:@selector(setFragmentAccelerationStructure:atBufferIndex:)]) [e setFragmentAccelerationStructure:o atBufferIndex:i];
            else [e setFragmentBuffer:o offset:off atIndex:i];
            break; }
        case NVMTL4StageObject: [(id<MTLRenderCommandEncoder>)enc setObjectBuffer:o offset:off atIndex:i]; break;
        case NVMTL4StageMesh: [(id<MTLRenderCommandEncoder>)enc setMeshBuffer:o offset:off atIndex:i]; break;
        case NVMTL4StageTile: {
            if (![enc respondsToSelector:@selector(setTileBuffer:offset:atIndex:)]) { [r unsupported:@"argument table at the tile stage (the render encoder has no setTileBuffer:offset:atIndex:)"]; return; }
            [(id<MTLRenderCommandEncoder>)enc setTileBuffer:o offset:off atIndex:i]; break; }
        default: {
            id<MTLComputeCommandEncoder> e = enc;
            if (as) [e setAccelerationStructure:o atBufferIndex:i];
            else if (str) [e setBuffer:o offset:off attributeStride:s->bstride[i] atIndex:i];
            else [e setBuffer:o offset:off atIndex:i];
            break; }
        }
    }
    for (NSUInteger i = 0; i < s->nt; i++) {
        if (!s->tk[i]) continue;
        if (s->tk[i] == 3) { [r unsupported:[NSString stringWithFormat:@"argument table texture index %lu could not be resolved", (unsigned long)i]]; continue; }
        id o = nvmtl4_nul(s->tobj[i]);
        if (prev && i < prev->nt && prev->tk[i] == s->tk[i] && prev->tid[i] == s->tid[i] && nvmtl4_nul(prev->tobj[i]) == o) continue;
        if (st == NVMTL4StageVertex) [(id<MTLRenderCommandEncoder>)enc setVertexTexture:o atIndex:i];
        else if (st == NVMTL4StageFragment) [(id<MTLRenderCommandEncoder>)enc setFragmentTexture:o atIndex:i];
        else if (st == NVMTL4StageObject) [(id<MTLRenderCommandEncoder>)enc setObjectTexture:o atIndex:i];
        else if (st == NVMTL4StageMesh) [(id<MTLRenderCommandEncoder>)enc setMeshTexture:o atIndex:i];
        else if (st == NVMTL4StageTile) [(id<MTLRenderCommandEncoder>)enc setTileTexture:o atIndex:i];
        else [(id<MTLComputeCommandEncoder>)enc setTexture:o atIndex:i];
    }
    for (NSUInteger i = 0; i < s->ns; i++) {
        if (!s->sk[i]) continue;
        if (s->sk[i] == 3) { [r unsupported:[NSString stringWithFormat:@"argument table sampler index %lu could not be resolved", (unsigned long)i]]; continue; }
        id o = nvmtl4_nul(s->sobj[i]);
        if (prev && i < prev->ns && prev->sk[i] == s->sk[i] && prev->sid[i] == s->sid[i] && nvmtl4_nul(prev->sobj[i]) == o) continue;
        if (st == NVMTL4StageVertex) [(id<MTLRenderCommandEncoder>)enc setVertexSamplerState:o atIndex:i];
        else if (st == NVMTL4StageFragment) [(id<MTLRenderCommandEncoder>)enc setFragmentSamplerState:o atIndex:i];
        else if (st == NVMTL4StageObject) [(id<MTLRenderCommandEncoder>)enc setObjectSamplerState:o atIndex:i];
        else if (st == NVMTL4StageMesh) [(id<MTLRenderCommandEncoder>)enc setMeshSamplerState:o atIndex:i];
        else if (st == NVMTL4StageTile) [(id<MTLRenderCommandEncoder>)enc setTileSamplerState:o atIndex:i];
        else [(id<MTLComputeCommandEncoder>)enc setSamplerState:o atIndex:i];
    }
}

@implementation NVMTL4Replay
- (instancetype)init { if ((self = [super init])) { errors = [NSMutableArray new]; tg = [NSMutableDictionary new]; } return self; }
- (void)unsupported:(NSString *)what {
    nvmtl4_note_once([@"unsupported: " stringByAppendingString:what]);
    [errors addObject:[@"unsupported: " stringByAppendingString:what]];
}
- (void)barrier {
    if (comp) [comp memoryBarrierWithScope:MTLBarrierScopeBuffers | MTLBarrierScopeTextures];
    else if (render) [render memoryBarrierWithScope:MTLBarrierScopeBuffers | MTLBarrierScopeTextures | MTLBarrierScopeRenderTargets
                                         afterStages:MTLRenderStageVertex | MTLRenderStageFragment beforeStages:MTLRenderStageVertex | MTLRenderStageFragment];
    else nvmtl_vk_cmd_barrier(&ncb->_c);
}
// The compute encoder, made on demand because a Metal 4 compute encoder also takes copies, which are blit work here.
- (void)endSuspended:(NSString *)why {
    if (!suspended) return;
    [render endEncoding]; render = nil; suspended = NO;
    appV = appF = appO = appM = appT = nil;
    if (why) [self unsupported:why];
}
- (id<MTLComputeCommandEncoder>)compute {
    gpuWork = YES;
    [self endSuspended:@"a suspended render pass was not resumed by the next encoder - it was ended there"];
    if (blit) { [blit endEncoding]; blit = nil; }
    if (asEnc) { [asEnc endEncoding]; asEnc = nil; }
    if (!comp) {
        comp = [cb computeCommandEncoderWithDispatchType:MTLDispatchTypeSerial];
        if (encLabel) comp.label = encLabel;
        appC = nil;
        if (pso) [comp setComputePipelineState:pso];
        for (NSNumber *k in tg) [comp setThreadgroupMemoryLength:tg[k].unsignedIntegerValue atIndex:k.unsignedIntegerValue];
        if (imgW || imgH) [comp setImageblockWidth:imgW height:imgH];
    }
    if (curC && curC != appC) { nvmtl4_apply(self, curC, appC, NVMTL4StageCompute, comp); appC = curC; }
    return comp;
}
- (id<MTLBlitCommandEncoder>)blitEnc {
    gpuWork = YES;
    [self endSuspended:@"a suspended render pass was not resumed by the next encoder - it was ended there"];
    if (comp) { [comp endEncoding]; comp = nil; appC = nil; }
    if (asEnc) { [asEnc endEncoding]; asEnc = nil; }
    if (!blit) { blit = [cb blitCommandEncoder]; if (encLabel) blit.label = encLabel; }
    return blit;
}
// Acceleration structure work goes to its own Metal 3 encoder, switched to and from the compute and blit ones.
- (id<MTLAccelerationStructureCommandEncoder>)asEncoder {
    gpuWork = YES;
    [self endSuspended:@"a suspended render pass was not resumed by the next encoder - it was ended there"];
    if (comp) { [comp endEncoding]; comp = nil; appC = nil; }
    if (blit) { [blit endEncoding]; blit = nil; }
    if (!asEnc) { asEnc = [cb accelerationStructureCommandEncoder]; if (encLabel) asEnc.label = encLabel; }
    return asEnc;
}
- (void)closeCompute {
    if (comp) { [comp endEncoding]; comp = nil; }
    if (blit) { [blit endEncoding]; blit = nil; }
    if (asEnc) { [asEnc endEncoding]; asEnc = nil; }
    appC = nil; curC = nil; pso = nil; [tg removeAllObjects]; imgW = imgH = 0;
}
// An indirect threads dispatch whose arguments the GPU may still be writing cannot be encoded yet: the threadgroup size is
// part of a Metal 3 dispatch. The replay stops here after copying the 24 bytes of arguments to a staging buffer; the
// part is committed, and the rest is replayed when it has finished and the arguments can be read.
- (void)splitForArgsFrom:(NVMTLBuffer *)b offset:(NSUInteger)off {
    stage = [cb.device newBufferWithLength:24 options:MTLResourceStorageModeShared];
    if (!stage) { [self unsupported:@"dispatchThreadsWithIndirectBuffer: the staging buffer for the arguments could not be made"]; return; }
    [[self blitEnc] copyFromBuffer:b sourceOffset:off toBuffer:stage destinationOffset:0 size:24];
    splitPending = YES;
}
// Closes the encoders of a part that ends at a split. Unlike closeCompute it keeps the pipeline, threadgroup memory and
// argument table the compute encoder had, which the next part opens its encoder with.
- (void)endForSplit {
    if (comp) { [comp endEncoding]; comp = nil; }
    if (blit) { [blit endEncoding]; blit = nil; }
    if (asEnc) { [asEnc endEncoding]; asEnc = nil; }
    appC = nil;
}
// End of a replay: nothing may stay open.
- (void)closeAll {
    [self closeCompute];
    [self endSuspended:@"a suspended render pass was never resumed - it was ended at the end of the commit"];
    if (render) { [render endEncoding]; render = nil; }
}
- (void)syncV:(NVMTL4Snap *)v F:(NVMTL4Snap *)f O:(NVMTL4Snap *)o M:(NVMTL4Snap *)m T:(NVMTL4Snap *)t {
    if (t && t != appT) { nvmtl4_apply(self, t, appT, NVMTL4StageTile, render); appT = t; }
    if (v && v != appV) { nvmtl4_apply(self, v, appV, NVMTL4StageVertex, render); appV = v; }
    if (f && f != appF) { nvmtl4_apply(self, f, appF, NVMTL4StageFragment, render); appF = f; }
    if (o && o != appO) { nvmtl4_apply(self, o, appO, NVMTL4StageObject, render); appO = o; }
    if (m && m != appM) { nvmtl4_apply(self, m, appM, NVMTL4StageMesh, render); appM = m; }
}
@end

#pragma mark - command buffer, allocator, feedback

NVMTL4 @interface NVMTL4CommandAllocator : NSObject <MTL4CommandAllocator> {
@public id _dev; NSString *_label; _Atomic uint64_t _bytes; }
@end
@implementation NVMTL4CommandAllocator
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
// Recording is host memory; this is an estimate that grows with what was recorded and returns to zero on reset.
- (uint64_t)allocatedSize { return atomic_load(&_bytes); }
- (void)reset { atomic_store(&_bytes, 0); }
@end

NVMTL4 @interface NVMTL4CommitFeedback : NSObject <MTL4CommitFeedback> {
@public NSError *_error; CFTimeInterval _start, _end; }
@end
@implementation NVMTL4CommitFeedback
- (NSError *)error { return _error; }
- (CFTimeInterval)GPUStartTime { return _start; }
- (CFTimeInterval)GPUEndTime { return _end; }
@end

// One encoder's recording, sealed: the replay steps through its ops one at a time so it can stop at a split and carry on.
NVMTL4 @interface NVMTL4Sealed : NSObject {
@public
    NSArray *ops; BOOL (^open)(NVMTL4Replay *); void (^close)(NVMTL4Replay *); BOOL qb, qa; NSString *label;
}
@end
@implementation NVMTL4Sealed @end

@class NVMTL4Encoder;

NVMTL4 @interface NVMTL4CommandBuffer : NSObject <MTL4CommandBuffer> {
@public
    id _dev; NSString *_label;
    NSMutableArray *_ops;   // blocks for command buffer level work, NVMTL4Sealed for each encoder
    NSMutableArray *_sets;
    NVMTL4CommandAllocator *_alloc;
    BOOL _recording, _ended; __weak NVMTL4Encoder *_open;
}
- (void)nvmtl4Add:(nvmtl4_op)op;
- (void)nvmtl4EncoderEnded:(NVMTL4Encoder *)e;
@end

#pragma mark - encoders

NVMTL4 @interface NVMTL4Encoder : NSObject <MTL4CommandEncoder> {
@public
    NVMTL4CommandBuffer *_cb; NSString *_label; NSMutableArray<nvmtl4_op> *_ops;
    BOOL _qBefore, _qAfter, _ended;
}
- (instancetype)initWithCommandBuffer:(NVMTL4CommandBuffer *)cb;
- (void)rec:(nvmtl4_op)op;
- (void)seal:(BOOL (^)(NVMTL4Replay *))open close:(void (^)(NVMTL4Replay *))close;
- (void)bad:(NSString *)what;
@end

@implementation NVMTL4Encoder
- (instancetype)initWithCommandBuffer:(NVMTL4CommandBuffer *)cb {
    if ((self = [super init])) { _cb = cb; _ops = [NSMutableArray new]; }
    return self;
}
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (id<MTL4CommandBuffer>)commandBuffer { return _cb; }
- (void)rec:(nvmtl4_op)op { if (_ended) { nvmtl4_note_once(@"encoder used after endEncoding - call ignored"); return; } [_ops addObject:[op copy]]; }
// Notes a call this adapter cannot carry out. It is logged when encoded and reported with the commit feedback.
- (void)bad:(NSString *)what { [self rec:^(NVMTL4Replay *r) { [r unsupported:what]; }]; }
- (void)seal:(BOOL (^)(NVMTL4Replay *))open close:(void (^)(NVMTL4Replay *))close {
    if (_ended) return;
    _ended = YES;
    NVMTL4Sealed *sl = [NVMTL4Sealed new];
    sl->ops = [_ops copy]; sl->qb = _qBefore; sl->qa = _qAfter; sl->label = _label; sl->open = [open copy]; sl->close = [close copy];
    NVMTL4CommandBuffer *cb = _cb;
    [cb->_ops addObject:sl];
    _ops = nil; _cb = nil;
    [cb nvmtl4EncoderEnded:self];
}
// Every barrier is a full Vulkan memory barrier: stages and visibility options only narrow what a barrier must cover.
- (void)barrierAfterQueueStages:(MTLStages)a beforeStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v { _qBefore = YES; }
- (void)barrierAfterStages:(MTLStages)a beforeQueueStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v { _qAfter = YES; }
- (void)barrierAfterEncoderStages:(MTLStages)a beforeEncoderStages:(MTLStages)b visibilityOptions:(MTL4VisibilityOptions)v {
    [self rec:^(NVMTL4Replay *r) { [r barrier]; }];
}
- (void)updateFence:(id<MTLFence>)f afterEncoderStages:(MTLStages)s {
    [self rec:^(NVMTL4Replay *r) {
        if (r->comp) [r->comp updateFence:f];
        else if (r->render) [r->render updateFence:f afterStages:MTLRenderStageFragment];
        else if (r->blit) [r->blit updateFence:f];
    }];
}
- (void)waitForFence:(id<MTLFence>)f beforeEncoderStages:(MTLStages)s {
    [self rec:^(NVMTL4Replay *r) {
        if (r->comp) [r->comp waitForFence:f];
        else if (r->render) [r->render waitForFence:f beforeStages:MTLRenderStageVertex];
        else if (r->blit) [r->blit waitForFence:f];
        else nvmtl_vk_cmd_barrier(&r->ncb->_c);
    }];
}
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {
    [self rec:^(NVMTL4Replay *r) {
        if (r->render) [r->render pushDebugGroup:s]; else if (r->comp) [r->comp pushDebugGroup:s]; else if (r->blit) [r->blit pushDebugGroup:s]; else [r->cb pushDebugGroup:s];
    }];
}
- (void)popDebugGroup {
    [self rec:^(NVMTL4Replay *r) {
        if (r->render) [r->render popDebugGroup]; else if (r->comp) [r->comp popDebugGroup]; else if (r->blit) [r->blit popDebugGroup]; else [r->cb popDebugGroup];
    }];
}
- (void)endEncoding { nvlog("mtl4: encoder class %s did not override endEncoding", class_getName([self class])); }
@end

// Resolves a GPU address argument of an encoded call. A miss is reported with the feedback and the call is dropped.
#define NVMTL4_RESOLVE(addr, bvar, ovar, what) \
    NSUInteger ovar = 0; NVMTLBuffer *bvar = nvmtl4_resolve_address((addr), &ovar); \
    if (!bvar) { [self bad:[NSString stringWithFormat:@"%s: GPU address 0x%llx is not inside any buffer - cannot encode", what, (unsigned long long)(addr)]]; return; }

NVMTL4 @interface NVMTL4RenderEncoder : NVMTL4Encoder <MTL4RenderCommandEncoder> {
    MTL4RenderPassDescriptor *_desc; NVMTL4ArgumentTable *_tv, *_tf, *_to, *_tm, *_tt; MTL4RenderEncoderOptions _opts;
}
- (instancetype)initWithCommandBuffer:(NVMTL4CommandBuffer *)cb descriptor:(MTL4RenderPassDescriptor *)d options:(MTL4RenderEncoderOptions)o;
@end

NVMTL4 static MTLRenderPassDescriptor *nvmtl4_pass_descriptor(MTL4RenderPassDescriptor *d)
{
    MTLRenderPassDescriptor *m = [MTLRenderPassDescriptor renderPassDescriptor];
    for (NSUInteger i = 0; i < 8; i++) {
        MTLRenderPassColorAttachmentDescriptor *a = d.colorAttachments[i];
        if (a.texture || a.resolveTexture) m.colorAttachments[i] = a;   // the array copies what it is given
    }
    m.depthAttachment = d.depthAttachment;
    m.stencilAttachment = d.stencilAttachment;
    m.renderTargetArrayLength = d.renderTargetArrayLength;
    m.imageblockSampleLength = d.imageblockSampleLength;
    m.threadgroupMemoryLength = d.threadgroupMemoryLength;
    m.tileWidth = d.tileWidth; m.tileHeight = d.tileHeight;
    m.defaultRasterSampleCount = d.defaultRasterSampleCount;
    m.renderTargetWidth = d.renderTargetWidth; m.renderTargetHeight = d.renderTargetHeight;
    m.rasterizationRateMap = d.rasterizationRateMap;
    m.visibilityResultBuffer = d.visibilityResultBuffer;
    m.visibilityResultType = d.visibilityResultType;
    MTLSamplePosition sp[8]; NSUInteger n = [d getSamplePositions:NULL count:0];
    if (n && n <= 8) { [d getSamplePositions:sp count:n]; [m setSamplePositions:sp count:n]; }
    return m;
}

@implementation NVMTL4RenderEncoder
- (instancetype)initWithCommandBuffer:(NVMTL4CommandBuffer *)cb descriptor:(MTL4RenderPassDescriptor *)d options:(MTL4RenderEncoderOptions)o {
    if ((self = [super initWithCommandBuffer:cb])) { _desc = [d copy]; _opts = o; }
    return self;
}
// The tile size the pass runs with: what the descriptor asked for, else the size the tile emulation picks for the
// pass's attachments (nvmtl_tile_dim), which Apple GPUs choose the same way (32x32, smaller for wide attachments).
- (NSUInteger)nvmtl4TileDim:(int)axis {
    NSUInteger asked = axis ? _desc.tileHeight : _desc.tileWidth;
    if (asked) return asked;
    NSUInteger bytes = 0;
    for (NSUInteger i = 0; i < 8; i++) {
        id<MTLTexture> t = _desc.colorAttachments[i].texture; if (!t) continue;
        size_t bpp = 4; uint32_t vkf = 0, b = 0; int a8 = 0;
        if (!nvmtl_pixfmt(t.pixelFormat, &vkf, &b, &a8) && b) bpp = b;
        bytes += bpp;
    }
    if (_desc.imageblockSampleLength > bytes) bytes = _desc.imageblockSampleLength;
    if (!bytes) bytes = 4;
    static const NSUInteger sizes[][2] = { { 32, 32 }, { 32, 16 }, { 16, 16 }, { 16, 8 }, { 8, 8 } };
    for (unsigned k = 0; k < 5; k++) if (sizes[k][0] * sizes[k][1] * bytes <= 32768u) return sizes[k][axis];
    return 8;
}
- (NSUInteger)tileWidth { return [self nvmtl4TileDim:0]; }
- (NSUInteger)tileHeight { return [self nvmtl4TileDim:1]; }
- (void)endEncoding {
    MTL4RenderPassDescriptor *d = _desc; const MTL4RenderEncoderOptions opts = _opts;
    [self seal:^BOOL(NVMTL4Replay *r) {
        // Suspend and resume: the suspending encoder leaves the Metal 3 pass open and the resuming one carries on in it,
        // which is one render pass, as the app means it. Every encoder of a commit is replayed onto one Metal 3 command
        // buffer, so the pair may be in different Metal 4 command buffers.
        if (opts & MTL4RenderEncoderOptionResuming) {
            if (r->suspended && r->render) { r->suspended = NO; return YES; }
            [r unsupported:@"a resuming render pass has no suspended pass before it - it starts a new pass"];
        } else {
            [r endSuspended:@"a suspended render pass was not resumed by the next encoder - it was ended there"];
        }
        r->gpuWork = YES;
        r->render = [r->cb renderCommandEncoderWithDescriptor:nvmtl4_pass_descriptor(d)];
        if (!r->render) { [r unsupported:@"render pass could not be started - its commands are dropped"]; return NO; }
        if (r->encLabel) r->render.label = r->encLabel;
        r->appV = r->appF = r->appO = r->appM = r->appT = nil;
        return YES;
    } close:^(NVMTL4Replay *r) {
        if (opts & MTL4RenderEncoderOptionSuspending) { if (r->render) r->suspended = YES; return; }
        if (r->render) [r->render endEncoding];
        r->render = nil; r->appV = r->appF = r->appO = r->appM = r->appT = nil;
    }];
    _desc = nil; _tv = _tf = _to = _tm = _tt = nil;
}
// The argument tables are read when a draw is encoded.
- (void)draw:(void (^)(id<MTLRenderCommandEncoder> e))body {
    NVMTL4Snap *sv = [_tv nvmtl4Snapshot], *sf = [_tf nvmtl4Snapshot], *so = [_to nvmtl4Snapshot], *sm = [_tm nvmtl4Snapshot], *st = [_tt nvmtl4Snapshot];
    [self rec:^(NVMTL4Replay *r) { [r syncV:sv F:sf O:so M:sm T:st]; body(r->render); }];
}
- (void)setArgumentTable:(id<MTL4ArgumentTable>)t atStages:(MTLRenderStages)stages {
    NVMTL4ArgumentTable *tb = (NVMTL4ArgumentTable *)t;
    if (t && ![tb isKindOfClass:[NVMTL4ArgumentTable class]]) { [self bad:@"setArgumentTable: with a table this driver did not make"]; return; }
    if (stages & MTLRenderStageVertex) _tv = tb;
    if (stages & MTLRenderStageFragment) _tf = tb;
    if (stages & MTLRenderStageObject) _to = tb;
    if (stages & MTLRenderStageMesh) _tm = tb;
    if (stages & MTLRenderStageTile) _tt = tb;
}
- (void)setColorAttachmentMap:(MTLLogicalToPhysicalColorAttachmentMap *)m {
    [self rec:^(NVMTL4Replay *r) {
        // applied when a pipeline that inherits the map is bound (NVMTL4Glue.m)
        if ([(id)r->render isKindOfClass:[NVMTLRenderCommandEncoder class]]) [(NVMTLRenderCommandEncoder *)r->render nvmtl4SetColorMap:m];
    }];
}
- (void)setRenderPipelineState:(id<MTLRenderPipelineState>)p { [self rec:^(NVMTL4Replay *r) {
    if ([(id)r->render isKindOfClass:[NVMTLRenderCommandEncoder class]]) [(NVMTLRenderCommandEncoder *)r->render nvmtl4PrepareColorMapForPipeline:p];
    [r->render setRenderPipelineState:p]; }]; }
- (void)setViewport:(MTLViewport)v { [self rec:^(NVMTL4Replay *r) { [r->render setViewport:v]; }]; }
- (void)setViewports:(const MTLViewport *)v count:(NSUInteger)n {
    NSData *d = [NSData dataWithBytes:v length:n * sizeof(MTLViewport)];
    [self rec:^(NVMTL4Replay *r) { [r->render setViewports:(const MTLViewport *)d.bytes count:n]; }];
}
- (void)setVertexAmplificationCount:(NSUInteger)n viewMappings:(const MTLVertexAmplificationViewMapping *)m {
    NSData *d = m ? [NSData dataWithBytes:m length:n * sizeof(MTLVertexAmplificationViewMapping)] : nil;
    [self rec:^(NVMTL4Replay *r) { [r->render setVertexAmplificationCount:n viewMappings:(const MTLVertexAmplificationViewMapping *)d.bytes]; }];
}
- (void)setCullMode:(MTLCullMode)m { [self rec:^(NVMTL4Replay *r) { [r->render setCullMode:m]; }]; }
- (void)setDepthClipMode:(MTLDepthClipMode)m { [self rec:^(NVMTL4Replay *r) { [r->render setDepthClipMode:m]; }]; }
- (void)setDepthBias:(float)b slopeScale:(float)s clamp:(float)c { [self rec:^(NVMTL4Replay *r) { [r->render setDepthBias:b slopeScale:s clamp:c]; }]; }
- (void)setDepthTestMinBound:(float)lo maxBound:(float)hi {
    [self rec:^(NVMTL4Replay *r) {
        if ([r->render respondsToSelector:@selector(setDepthTestMinBound:maxBound:)]) [r->render setDepthTestMinBound:lo maxBound:hi];
        else [r unsupported:@"setDepthTestMinBound:maxBound:"];
    }];
}
- (void)setScissorRect:(MTLScissorRect)s { [self rec:^(NVMTL4Replay *r) { [r->render setScissorRect:s]; }]; }
- (void)setScissorRects:(const MTLScissorRect *)s count:(NSUInteger)n {
    NSData *d = [NSData dataWithBytes:s length:n * sizeof(MTLScissorRect)];
    [self rec:^(NVMTL4Replay *r) { [r->render setScissorRects:(const MTLScissorRect *)d.bytes count:n]; }];
}
- (void)setTriangleFillMode:(MTLTriangleFillMode)m { [self rec:^(NVMTL4Replay *r) { [r->render setTriangleFillMode:m]; }]; }
- (void)setBlendColorRed:(float)red green:(float)g blue:(float)b alpha:(float)a { [self rec:^(NVMTL4Replay *r) { [r->render setBlendColorRed:red green:g blue:b alpha:a]; }]; }
- (void)setDepthStencilState:(id<MTLDepthStencilState>)s { [self rec:^(NVMTL4Replay *r) { [r->render setDepthStencilState:s]; }]; }
- (void)setStencilReferenceValue:(uint32_t)v { [self rec:^(NVMTL4Replay *r) { [r->render setStencilReferenceValue:v]; }]; }
- (void)setStencilFrontReferenceValue:(uint32_t)f backReferenceValue:(uint32_t)b { [self rec:^(NVMTL4Replay *r) { [r->render setStencilFrontReferenceValue:f backReferenceValue:b]; }]; }
- (void)setVisibilityResultMode:(MTLVisibilityResultMode)m offset:(NSUInteger)o { [self rec:^(NVMTL4Replay *r) { [r->render setVisibilityResultMode:m offset:o]; }]; }
- (void)setColorStoreAction:(MTLStoreAction)a atIndex:(NSUInteger)i { [self rec:^(NVMTL4Replay *r) { [r->render setColorStoreAction:a atIndex:i]; }]; }
- (void)setDepthStoreAction:(MTLStoreAction)a { [self rec:^(NVMTL4Replay *r) { [r->render setDepthStoreAction:a]; }]; }
- (void)setStencilStoreAction:(MTLStoreAction)a { [self rec:^(NVMTL4Replay *r) { [r->render setStencilStoreAction:a]; }]; }
- (void)setFrontFacingWinding:(MTLWinding)w { [self rec:^(NVMTL4Replay *r) { [r->render setFrontFacingWinding:w]; }]; }
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawPrimitives:t vertexStart:s vertexCount:c]; }];
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c instanceCount:(NSUInteger)n {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawPrimitives:t vertexStart:s vertexCount:c instanceCount:n]; }];
}
- (void)drawPrimitives:(MTLPrimitiveType)t vertexStart:(NSUInteger)s vertexCount:(NSUInteger)c instanceCount:(NSUInteger)n baseInstance:(NSUInteger)bi {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawPrimitives:t vertexStart:s vertexCount:c instanceCount:n baseInstance:bi]; }];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)c indexType:(MTLIndexType)it indexBuffer:(MTLGPUAddress)ib indexBufferLength:(NSUInteger)len {
    [self drawIndexedPrimitives:t indexCount:c indexType:it indexBuffer:ib indexBufferLength:len instanceCount:1 baseVertex:0 baseInstance:0];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)c indexType:(MTLIndexType)it indexBuffer:(MTLGPUAddress)ib indexBufferLength:(NSUInteger)len instanceCount:(NSUInteger)n {
    [self drawIndexedPrimitives:t indexCount:c indexType:it indexBuffer:ib indexBufferLength:len instanceCount:n baseVertex:0 baseInstance:0];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexCount:(NSUInteger)c indexType:(MTLIndexType)it indexBuffer:(MTLGPUAddress)ib indexBufferLength:(NSUInteger)len
                instanceCount:(NSUInteger)n baseVertex:(NSInteger)bv baseInstance:(NSUInteger)bi {
    NVMTL4_RESOLVE(ib, b, off, "drawIndexedPrimitives")
    [self draw:^(id<MTLRenderCommandEncoder> e) {
        [e drawIndexedPrimitives:t indexCount:c indexType:it indexBuffer:b indexBufferOffset:off instanceCount:n baseVertex:bv baseInstance:bi]; }];
}
- (void)drawPrimitives:(MTLPrimitiveType)t indirectBuffer:(MTLGPUAddress)ind {
    NVMTL4_RESOLVE(ind, b, off, "drawPrimitives indirect")
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawPrimitives:t indirectBuffer:b indirectBufferOffset:off]; }];
}
- (void)drawIndexedPrimitives:(MTLPrimitiveType)t indexType:(MTLIndexType)it indexBuffer:(MTLGPUAddress)ib indexBufferLength:(NSUInteger)len indirectBuffer:(MTLGPUAddress)ind {
    NVMTL4_RESOLVE(ib, xb, xoff, "drawIndexedPrimitives indirect index buffer")
    NVMTL4_RESOLVE(ind, b, off, "drawIndexedPrimitives indirect")
    [self draw:^(id<MTLRenderCommandEncoder> e) {
        [e drawIndexedPrimitives:t indexType:it indexBuffer:xb indexBufferOffset:xoff indirectBuffer:b indirectBufferOffset:off]; }];
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)range {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e executeCommandsInBuffer:icb withRange:range]; }];
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(MTLGPUAddress)ind {
    NVMTL4_RESOLVE(ind, b, off, "executeCommandsInBuffer indirect")
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e executeCommandsInBuffer:icb indirectBuffer:b indirectBufferOffset:off]; }];
}
- (void)setObjectThreadgroupMemoryLength:(NSUInteger)l atIndex:(NSUInteger)i {
    [self rec:^(NVMTL4Replay *r) { [r->render setObjectThreadgroupMemoryLength:l atIndex:i]; }];
}
- (void)drawMeshThreadgroups:(MTLSize)g threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawMeshThreadgroups:g threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m]; }];
}
- (void)drawMeshThreads:(MTLSize)g threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m {
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawMeshThreads:g threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m]; }];
}
- (void)drawMeshThreadgroupsWithIndirectBuffer:(MTLGPUAddress)ind threadsPerObjectThreadgroup:(MTLSize)o threadsPerMeshThreadgroup:(MTLSize)m {
    NVMTL4_RESOLVE(ind, b, off, "drawMeshThreadgroupsWithIndirectBuffer")
    [self draw:^(id<MTLRenderCommandEncoder> e) { [e drawMeshThreadgroupsWithIndirectBuffer:b indirectBufferOffset:off threadsPerObjectThreadgroup:o threadsPerMeshThreadgroup:m]; }];
}
- (void)dispatchThreadsPerTile:(MTLSize)t {
    // A tile dispatch reads the tile-stage table as a draw reads the others. WAS a plain record: the tile kernel ran with
    // nothing bound at the tile stage and wrote through whatever its slots held.
    [self draw:^(id<MTLRenderCommandEncoder> e) {
        if ([e respondsToSelector:@selector(dispatchThreadsPerTile:)]) [e dispatchThreadsPerTile:t];
        else nvlog("mtl4: dispatchThreadsPerTile: unsupported by this render encoder - tile dispatch dropped");
    }];
}
- (void)setThreadgroupMemoryLength:(NSUInteger)l offset:(NSUInteger)o atIndex:(NSUInteger)i {
    [self rec:^(NVMTL4Replay *r) { [r->render setThreadgroupMemoryLength:l offset:o atIndex:i]; }];
}
- (void)writeTimestampWithGranularity:(MTL4TimestampGranularity)g afterStage:(MTLRenderStages)st intoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i {
    [self rec:^(NVMTL4Replay *r) {
        if (nvmtl4_hook_write_timestamp && nvmtl4_hook_write_timestamp(r->render, r->cb, h, i, (NSInteger)g)) return;
        [r unsupported:@"counter heap timestamps (no counter heap implementation is attached)"];
    }];
}
@end

#pragma mark - tensors and acceleration structures

NVMTL4 static size_t nvmtl4_tensor_elem(MTLTensorDataType t)
{
    switch (t) {
    case MTLTensorDataTypeFloat32: case MTLTensorDataTypeInt32: case MTLTensorDataTypeUInt32: return 4;
    case MTLTensorDataTypeFloat16: case MTLTensorDataTypeBFloat16: case MTLTensorDataTypeInt16: case MTLTensorDataTypeUInt16: return 2;
    case MTLTensorDataTypeInt8: case MTLTensorDataTypeUInt8: return 1;
    default: return 0;
    }
}

// What a copy needs of a tensor: its buffer, the offset of element zero, rank, dimensions and strides in elements
// (dimension 0 innermost) and the element size.
typedef struct { __unsafe_unretained id<MTLBuffer> buf; NSUInteger off, rank; NSInteger d[16], s[16]; size_t es; } nvmtl4_tview;

NVMTL4 static BOOL nvmtl4_tensor_view(id<MTLTensor> t, nvmtl4_tview *v)
{
    *v = (nvmtl4_tview){ 0 };
    if (nvmtl4_hook_tensor_backing && nvmtl4_hook_tensor_backing(t, &v->buf, &v->off, &v->rank, v->d, v->s, &v->es)) return v->buf != nil && v->rank <= 16;
    id<MTLBuffer> b = t.buffer;   // a wrapped tensor says where it lives; any other needs the hook
    MTLTensorExtents *d = t.dimensions, *st = t.strides;
    v->es = nvmtl4_tensor_elem(t.dataType);
    if (!b || !d || d.rank > 16 || !v->es) return NO;
    v->buf = b; v->off = t.bufferOffset; v->rank = d.rank;
    NSInteger run = 1;
    for (NSUInteger i = 0; i < v->rank; i++) {
        v->d[i] = [d extentAtDimensionIndex:i];
        v->s[i] = st ? [st extentAtDimensionIndex:i] : run;
        run *= v->d[i];
    }
    return YES;
}

typedef struct { NSInteger o1[16], d1[16], o2[16], d2[16]; NSUInteger r1, r3; } nvmtl4_tcopy;

NVMTL4 static void nvmtl4_tensor_extents(MTLTensorExtents *e, NSInteger *out, NSUInteger *rank)
{
    *rank = MIN(e.rank, (NSUInteger)16);
    for (NSUInteger i = 0; i < *rank; i++) out[i] = [e extentAtDimensionIndex:i];
}

// The copy of one tensor region into another as buffer copies: one per run along dimension 0 when both tensors store
// it contiguously, else one per element.
NVMTL4 static void nvmtl4_replay_tensor_copy(NVMTL4Replay *r, id<MTLTensor> src, const NSInteger *so, NSUInteger srank, const NSInteger *sdim,
                                      id<MTLTensor> dst, const NSInteger *dorg, NSUInteger drank, const NSInteger *ddim)
{
    nvmtl4_tview a, b;
    if (!nvmtl4_tensor_view(src, &a) || !nvmtl4_tensor_view(dst, &b)) {
        [r unsupported:@"copyFromTensor: a tensor's backing buffer is not known (the tensor owner provides it through nvmtl4_hook_tensor_backing)"]; return; }
    if (srank != a.rank || drank != b.rank || a.rank != b.rank || a.es != b.es) { [r unsupported:@"copyFromTensor: tensors of different rank or element size"]; return; }
    const NSUInteger R = a.rank;
    for (NSUInteger i = 0; i < R; i++) if (sdim[i] != ddim[i]) { [r unsupported:@"copyFromTensor: source and destination dimensions differ"]; return; }
    for (NSUInteger i = 0; i < R; i++) if (sdim[i] <= 0) return;
    id<MTLBlitCommandEncoder> e = [r blitEnc];
    if (!R) { [e copyFromBuffer:a.buf sourceOffset:a.off toBuffer:b.buf destinationOffset:b.off size:a.es]; return; }
    const BOOL contiguous = a.s[0] == 1 && b.s[0] == 1;
    NSInteger idx[16] = {0};
    for (;;) {
        size_t se = 0, de = 0;
        for (NSUInteger i = 0; i < R; i++) { se += (size_t)(so[i] + (i ? idx[i] : 0)) * (size_t)a.s[i]; de += (size_t)(dorg[i] + (i ? idx[i] : 0)) * (size_t)b.s[i]; }
        if (contiguous) [e copyFromBuffer:a.buf sourceOffset:a.off + se * a.es toBuffer:b.buf destinationOffset:b.off + de * b.es size:(size_t)sdim[0] * a.es];
        else for (NSInteger k = 0; k < sdim[0]; k++)
            [e copyFromBuffer:a.buf sourceOffset:a.off + (se + (size_t)k * (size_t)a.s[0]) * a.es toBuffer:b.buf
            destinationOffset:b.off + (de + (size_t)k * (size_t)b.s[0]) * b.es size:a.es];
        NSUInteger i = 1;
        for (; i < R; i++) { if (++idx[i] < sdim[i]) break; idx[i] = 0; }
        if (i >= R) break;
    }
}

// A Metal 4 acceleration structure descriptor names its buffers by GPU address; the Metal 3 builder takes buffer objects.
// The conversion resolves every address now, while the buffers are known to the registry.
NVMTL4 static NVMTLBuffer *nvmtl4_range_buffer(MTL4BufferRange rg, NSUInteger *off, NSMutableArray<NSString *> *why, const char *what)
{
    if (!rg.bufferAddress) { *off = 0; return nil; }
    NVMTLBuffer *b = nvmtl4_resolve_address(rg.bufferAddress, off);
    if (!b) [why addObject:[NSString stringWithFormat:@"%s: GPU address 0x%llx is not inside any buffer", what, (unsigned long long)rg.bufferAddress]];
    return b;
}

NVMTL4 static void nvmtl4_geometry_common(MTL4AccelerationStructureGeometryDescriptor *g, MTLAccelerationStructureGeometryDescriptor *m, NSMutableArray<NSString *> *why)
{
    m.intersectionFunctionTableOffset = g.intersectionFunctionTableOffset;
    m.opaque = g.opaque;
    m.allowDuplicateIntersectionFunctionInvocation = g.allowDuplicateIntersectionFunctionInvocation;
    if (g.label) m.label = g.label;   // macOS 27 Metal asserts on a nil label
    NSUInteger off = 0; NVMTLBuffer *pd = nvmtl4_range_buffer(g.primitiveDataBuffer, &off, why, "primitive data buffer");
    if (pd) { m.primitiveDataBuffer = pd; m.primitiveDataBufferOffset = off; m.primitiveDataStride = g.primitiveDataStride; m.primitiveDataElementSize = g.primitiveDataElementSize; }
}

// nil with the reasons in why when the descriptor holds something the Metal 3 builder does not carry.
NVMTL4 static MTLAccelerationStructureDescriptor *nvmtl4_as_descriptor(MTL4AccelerationStructureDescriptor *d, NSMutableArray<NSString *> *why)
{
    if ([d isKindOfClass:[MTL4PrimitiveAccelerationStructureDescriptor class]]) {
        MTL4PrimitiveAccelerationStructureDescriptor *pd = (MTL4PrimitiveAccelerationStructureDescriptor *)d;
        MTLPrimitiveAccelerationStructureDescriptor *m = [MTLPrimitiveAccelerationStructureDescriptor descriptor];
        NSMutableArray *gs = [NSMutableArray new];
        for (MTL4AccelerationStructureGeometryDescriptor *g in pd.geometryDescriptors) {
            NSUInteger off = 0;
            if ([g isKindOfClass:[MTL4AccelerationStructureTriangleGeometryDescriptor class]]) {
                MTL4AccelerationStructureTriangleGeometryDescriptor *t = (MTL4AccelerationStructureTriangleGeometryDescriptor *)g;
                MTLAccelerationStructureTriangleGeometryDescriptor *o = [MTLAccelerationStructureTriangleGeometryDescriptor descriptor];
                nvmtl4_geometry_common(g, o, why);
                o.vertexBuffer = nvmtl4_range_buffer(t.vertexBuffer, &off, why, "vertex buffer"); o.vertexBufferOffset = off;
                o.vertexFormat = t.vertexFormat; o.vertexStride = t.vertexStride;
                o.indexBuffer = nvmtl4_range_buffer(t.indexBuffer, &off, why, "index buffer"); o.indexBufferOffset = off;
                o.indexType = t.indexType; o.triangleCount = t.triangleCount;
                o.transformationMatrixBuffer = nvmtl4_range_buffer(t.transformationMatrixBuffer, &off, why, "transformation matrix buffer");
                o.transformationMatrixBufferOffset = off; o.transformationMatrixLayout = t.transformationMatrixLayout;
                [gs addObject:o];
            } else if ([g isKindOfClass:[MTL4AccelerationStructureBoundingBoxGeometryDescriptor class]]) {
                MTL4AccelerationStructureBoundingBoxGeometryDescriptor *b = (MTL4AccelerationStructureBoundingBoxGeometryDescriptor *)g;
                MTLAccelerationStructureBoundingBoxGeometryDescriptor *o = [MTLAccelerationStructureBoundingBoxGeometryDescriptor descriptor];
                nvmtl4_geometry_common(g, o, why);
                o.boundingBoxBuffer = nvmtl4_range_buffer(b.boundingBoxBuffer, &off, why, "bounding box buffer"); o.boundingBoxBufferOffset = off;
                o.boundingBoxStride = b.boundingBoxStride; o.boundingBoxCount = b.boundingBoxCount;
                [gs addObject:o];
            } else {
                [why addObject:[NSString stringWithFormat:@"%s geometry (the Metal 3 builder under this does not carry motion or curve geometry either)", class_getName([g class])]];
            }
        }
        m.geometryDescriptors = gs;
        if (pd.motionKeyframeCount > 1) [why addObject:@"motion keyframes (not carried by the Metal 3 builder under this)"];
        m.usage = d.usage;
        return why.count ? nil : m;
    }
    if ([d isKindOfClass:[MTL4InstanceAccelerationStructureDescriptor class]]) {
        MTL4InstanceAccelerationStructureDescriptor *id_ = (MTL4InstanceAccelerationStructureDescriptor *)d;
        MTLInstanceAccelerationStructureDescriptor *m = [MTLInstanceAccelerationStructureDescriptor descriptor];
        NSUInteger off = 0;
        m.instanceDescriptorBuffer = nvmtl4_range_buffer(id_.instanceDescriptorBuffer, &off, why, "instance descriptor buffer");
        m.instanceDescriptorBufferOffset = off;
        m.instanceDescriptorStride = id_.instanceDescriptorStride;
        m.instanceCount = id_.instanceCount;
        m.instanceDescriptorType = id_.instanceDescriptorType;
        m.instanceTransformationMatrixLayout = id_.instanceTransformationMatrixLayout;
        if (id_.instanceDescriptorType != MTLAccelerationStructureInstanceDescriptorTypeIndirect) [why addObject:@"instance descriptor type other than Indirect (not carried by the Metal 3 builder under this)"];
        if (id_.motionTransformCount) [why addObject:@"motion transforms (not carried by the Metal 3 builder under this)"];
        m.usage = d.usage;
        return why.count ? nil : m;
    }
    [why addObject:[NSString stringWithFormat:@"%s (the Metal 3 builder under this takes primitive and instance descriptors)", class_getName([d class])]];
    return nil;
}

NVMTL4 @interface NVMTL4ComputeEncoder : NVMTL4Encoder <MTL4ComputeCommandEncoder> {
    NVMTL4ArgumentTable *_t; MTLStages _stages;
}
@end

@implementation NVMTL4ComputeEncoder
- (MTLStages)stages { return _stages; }
- (void)endEncoding {
    [self seal:^BOOL(NVMTL4Replay *r) { return YES; } close:^(NVMTL4Replay *r) { [r closeCompute]; }];
    _t = nil;
}
- (void)setArgumentTable:(id<MTL4ArgumentTable>)t {
    NVMTL4ArgumentTable *tb = (NVMTL4ArgumentTable *)t;
    if (t && ![tb isKindOfClass:[NVMTL4ArgumentTable class]]) { [self bad:@"setArgumentTable: with a table this driver did not make"]; return; }
    _t = tb;
}
// A dispatch reads the argument table when it is encoded.
- (void)dispatch:(void (^)(id<MTLComputeCommandEncoder> e))body {
    _stages |= MTLStageDispatch;
    NVMTL4Snap *s = [_t nvmtl4Snapshot];
    [self rec:^(NVMTL4Replay *r) { r->curC = s; body([r compute]); }];
}
- (void)blit:(void (^)(id<MTLBlitCommandEncoder> e))body {
    _stages |= MTLStageBlit;
    [self rec:^(NVMTL4Replay *r) { body([r blitEnc]); }];
}
- (void)setComputePipelineState:(id<MTLComputePipelineState>)p {
    [self rec:^(NVMTL4Replay *r) { r->pso = p; if (r->comp) [r->comp setComputePipelineState:p]; }];
}
- (void)setThreadgroupMemoryLength:(NSUInteger)l atIndex:(NSUInteger)i {
    [self rec:^(NVMTL4Replay *r) { r->tg[@(i)] = @(l); if (r->comp) [r->comp setThreadgroupMemoryLength:l atIndex:i]; }];
}
- (void)setImageblockWidth:(NSUInteger)w height:(NSUInteger)h {
    [self rec:^(NVMTL4Replay *r) { r->imgW = w; r->imgH = h; if (r->comp) [r->comp setImageblockWidth:w height:h]; }];
}
- (void)dispatchThreads:(MTLSize)g threadsPerThreadgroup:(MTLSize)t { [self dispatch:^(id<MTLComputeCommandEncoder> e) { [e dispatchThreads:g threadsPerThreadgroup:t]; }]; }
- (void)dispatchThreadgroups:(MTLSize)g threadsPerThreadgroup:(MTLSize)t { [self dispatch:^(id<MTLComputeCommandEncoder> e) { [e dispatchThreadgroups:g threadsPerThreadgroup:t]; }]; }
- (void)dispatchThreadgroupsWithIndirectBuffer:(MTLGPUAddress)ind threadsPerThreadgroup:(MTLSize)t {
    NVMTL4_RESOLVE(ind, b, off, "dispatchThreadgroupsWithIndirectBuffer")
    [self dispatch:^(id<MTLComputeCommandEncoder> e) { [e dispatchThreadgroupsWithIndirectBuffer:b indirectBufferOffset:off threadsPerThreadgroup:t]; }];
}
// The threadgroup size is part of the arguments, which a Metal 3 dispatch needs when it is encoded. When nothing on the GPU
// can have written the arguments (CPU-visible buffer, no GPU work earlier in this commit, nothing in flight on the queue)
// they are read now. Otherwise the replay splits here (see NVMTL4Replay splitForArgsFrom) and this op runs again in the
// next part with the arguments in hand.
- (void)dispatchThreadsWithIndirectBuffer:(MTLGPUAddress)ind {
    NVMTL4_RESOLVE(ind, b, off, "dispatchThreadsWithIndirectBuffer")
    _stages |= MTLStageDispatch;
    NVMTL4Snap *s = [_t nvmtl4Snapshot];
    [self rec:^(NVMTL4Replay *r) {
        r->curC = s;
        uint32_t a[6]; BOOL have = NO;
        if (r->haveArgs) { memcpy(a, r->args, sizeof a); r->haveArgs = NO; have = YES; }
        else if (!r->gpuWork && r->queueIdle) {
            const uint8_t *p = nvmtl_buf_cpu(b);
            if (p && off + sizeof a <= b->_b.size) { memcpy(a, p + off, sizeof a); have = YES; }
        }
        if (!have) { [r splitForArgsFrom:b offset:off]; return; }
        if (!a[3] || !a[4] || !a[5]) { [r unsupported:@"dispatchThreadsWithIndirectBuffer: the arguments hold a threadgroup size of zero"]; return; }
        [[r compute] dispatchThreads:MTLSizeMake(a[0], a[1], a[2]) threadsPerThreadgroup:MTLSizeMake(a[3], a[4], a[5])];
    }];
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb withRange:(NSRange)range {
    [self dispatch:^(id<MTLComputeCommandEncoder> e) { [e executeCommandsInBuffer:icb withRange:range]; }];
}
- (void)executeCommandsInBuffer:(id<MTLIndirectCommandBuffer>)icb indirectBuffer:(MTLGPUAddress)ind {
    NVMTL4_RESOLVE(ind, b, off, "executeCommandsInBuffer indirect")
    [self dispatch:^(id<MTLComputeCommandEncoder> e) { [e executeCommandsInBuffer:icb indirectBuffer:b indirectBufferOffset:off]; }];
}
- (void)copyFromTexture:(id<MTLTexture>)s toTexture:(id<MTLTexture>)d { [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromTexture:s toTexture:d]; }]; }
- (void)copyFromTexture:(id<MTLTexture>)s sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl toTexture:(id<MTLTexture>)d destinationSlice:(NSUInteger)ds
       destinationLevel:(NSUInteger)dl sliceCount:(NSUInteger)sc levelCount:(NSUInteger)lc {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromTexture:s sourceSlice:ss sourceLevel:sl toTexture:d destinationSlice:ds destinationLevel:dl sliceCount:sc levelCount:lc]; }];
}
- (void)copyFromTexture:(id<MTLTexture>)s sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)sz
              toTexture:(id<MTLTexture>)d destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dorg {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromTexture:s sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:sz toTexture:d destinationSlice:ds destinationLevel:dl destinationOrigin:dorg]; }];
}
- (void)copyFromTexture:(id<MTLTexture>)s sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)sz
               toBuffer:(id<MTLBuffer>)d destinationOffset:(NSUInteger)doff destinationBytesPerRow:(NSUInteger)bpr destinationBytesPerImage:(NSUInteger)bpi {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromTexture:s sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:sz toBuffer:d destinationOffset:doff destinationBytesPerRow:bpr destinationBytesPerImage:bpi]; }];
}
- (void)copyFromTexture:(id<MTLTexture>)s sourceSlice:(NSUInteger)ss sourceLevel:(NSUInteger)sl sourceOrigin:(MTLOrigin)so sourceSize:(MTLSize)sz
               toBuffer:(id<MTLBuffer>)d destinationOffset:(NSUInteger)doff destinationBytesPerRow:(NSUInteger)bpr destinationBytesPerImage:(NSUInteger)bpi options:(MTLBlitOption)opt {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromTexture:s sourceSlice:ss sourceLevel:sl sourceOrigin:so sourceSize:sz toBuffer:d destinationOffset:doff destinationBytesPerRow:bpr destinationBytesPerImage:bpi options:opt]; }];
}
- (void)copyFromBuffer:(id<MTLBuffer>)s sourceOffset:(NSUInteger)so toBuffer:(id<MTLBuffer>)d destinationOffset:(NSUInteger)doff size:(NSUInteger)n {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromBuffer:s sourceOffset:so toBuffer:d destinationOffset:doff size:n]; }];
}
- (void)copyFromBuffer:(id<MTLBuffer>)s sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)bpr sourceBytesPerImage:(NSUInteger)bpi sourceSize:(MTLSize)sz
             toTexture:(id<MTLTexture>)d destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dorg {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromBuffer:s sourceOffset:so sourceBytesPerRow:bpr sourceBytesPerImage:bpi sourceSize:sz toTexture:d destinationSlice:ds destinationLevel:dl destinationOrigin:dorg]; }];
}
- (void)copyFromBuffer:(id<MTLBuffer>)s sourceOffset:(NSUInteger)so sourceBytesPerRow:(NSUInteger)bpr sourceBytesPerImage:(NSUInteger)bpi sourceSize:(MTLSize)sz
             toTexture:(id<MTLTexture>)d destinationSlice:(NSUInteger)ds destinationLevel:(NSUInteger)dl destinationOrigin:(MTLOrigin)dorg options:(MTLBlitOption)opt {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyFromBuffer:s sourceOffset:so sourceBytesPerRow:bpr sourceBytesPerImage:bpi sourceSize:sz toTexture:d destinationSlice:ds destinationLevel:dl destinationOrigin:dorg options:opt]; }];
}
- (void)copyFromTensor:(id<MTLTensor>)s sourceOrigin:(MTLTensorExtents *)so sourceDimensions:(MTLTensorExtents *)sd toTensor:(id<MTLTensor>)d
     destinationOrigin:(MTLTensorExtents *)dorg destinationDimensions:(MTLTensorExtents *)dd {
    _stages |= MTLStageBlit;
    nvmtl4_tcopy c; NSUInteger r2, r4;
    nvmtl4_tensor_extents(so, c.o1, &c.r1); nvmtl4_tensor_extents(sd, c.d1, &r2); nvmtl4_tensor_extents(dorg, c.o2, &c.r3); nvmtl4_tensor_extents(dd, c.d2, &r4);
    if (c.r1 != r2 || c.r3 != r4) { [self bad:@"copyFromTensor: origin and dimension ranks differ"]; return; }
    [self rec:^(NVMTL4Replay *r) { nvmtl4_replay_tensor_copy(r, s, c.o1, c.r1, c.d1, d, c.o2, c.r3, c.d2); }];
}
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 270000
- (void)copyFromTensor:(id<MTLTensor>)s sourceOrigin:(MTLTensorExtents *)so sourceDimensions:(MTLTensorExtents *)sd sourcePlane:(MTLTensorPlaneType)sp toTensor:(id<MTLTensor>)d
     destinationOrigin:(MTLTensorExtents *)dorg destinationDimensions:(MTLTensorExtents *)dd destinationPlane:(MTLTensorPlaneType)dp API_AVAILABLE(macos(27.0)) {
    // The tensors made here have a data plane only.
    if (sp != MTLTensorPlaneTypeData || dp != MTLTensorPlaneTypeData) { [self bad:@"copyFromTensor: an auxiliary plane (tensors here have a data plane only)"]; return; }
    [self copyFromTensor:s sourceOrigin:so sourceDimensions:sd toTensor:d destinationOrigin:dorg destinationDimensions:dd];
}
#endif
- (void)generateMipmapsForTexture:(id<MTLTexture>)t { [self blit:^(id<MTLBlitCommandEncoder> e) { [e generateMipmapsForTexture:t]; }]; }
- (void)fillBuffer:(id<MTLBuffer>)b range:(NSRange)range value:(uint8_t)v { [self blit:^(id<MTLBlitCommandEncoder> e) { [e fillBuffer:b range:range value:v]; }]; }
- (void)optimizeContentsForGPUAccess:(id<MTLTexture>)t { [self blit:^(id<MTLBlitCommandEncoder> e) { [e optimizeContentsForGPUAccess:t]; }]; }
- (void)optimizeContentsForGPUAccess:(id<MTLTexture>)t slice:(NSUInteger)s level:(NSUInteger)l { [self blit:^(id<MTLBlitCommandEncoder> e) { [e optimizeContentsForGPUAccess:t slice:s level:l]; }]; }
- (void)optimizeContentsForCPUAccess:(id<MTLTexture>)t { [self blit:^(id<MTLBlitCommandEncoder> e) { [e optimizeContentsForCPUAccess:t]; }]; }
- (void)optimizeContentsForCPUAccess:(id<MTLTexture>)t slice:(NSUInteger)s level:(NSUInteger)l { [self blit:^(id<MTLBlitCommandEncoder> e) { [e optimizeContentsForCPUAccess:t slice:s level:l]; }]; }
- (void)resetCommandsInBuffer:(id<MTLIndirectCommandBuffer>)b withRange:(NSRange)range { [self blit:^(id<MTLBlitCommandEncoder> e) { [e resetCommandsInBuffer:b withRange:range]; }]; }
- (void)copyIndirectCommandBuffer:(id<MTLIndirectCommandBuffer>)s sourceRange:(NSRange)sr destination:(id<MTLIndirectCommandBuffer>)d destinationIndex:(NSUInteger)di {
    [self blit:^(id<MTLBlitCommandEncoder> e) { [e copyIndirectCommandBuffer:s sourceRange:sr destination:d destinationIndex:di]; }];
}
- (void)optimizeIndirectCommandBuffer:(id<MTLIndirectCommandBuffer>)b withRange:(NSRange)range { [self blit:^(id<MTLBlitCommandEncoder> e) { [e optimizeIndirectCommandBuffer:b withRange:range]; }]; }
// Acceleration structure commands run on the Metal 3 acceleration structure encoder; the descriptor and scratch address
// are converted when the command is encoded.
- (void)as:(void (^)(id<MTLAccelerationStructureCommandEncoder> e))body {
    _stages |= MTLStageAccelerationStructure;
    [self rec:^(NVMTL4Replay *r) { body([r asEncoder]); }];
}
- (BOOL)scratch:(MTL4BufferRange)rg into:(NVMTLBuffer **)b offset:(NSUInteger *)off what:(const char *)what {
    *b = nvmtl4_resolve_address(rg.bufferAddress, off);
    if (!*b) { [self bad:[NSString stringWithFormat:@"%s: GPU address 0x%llx is not inside any buffer", what, (unsigned long long)rg.bufferAddress]]; return NO; }
    return YES;
}
- (MTLAccelerationStructureDescriptor *)convert:(MTL4AccelerationStructureDescriptor *)d what:(const char *)what {
    NSMutableArray<NSString *> *why = [NSMutableArray new];
    MTLAccelerationStructureDescriptor *m = nvmtl4_as_descriptor(d, why);
    if (!m) for (NSString *w in why) [self bad:[NSString stringWithFormat:@"%s: %@", what, w]];
    return m;
}
- (void)buildAccelerationStructure:(id<MTLAccelerationStructure>)a descriptor:(MTL4AccelerationStructureDescriptor *)d scratchBuffer:(MTL4BufferRange)sr {
    NVMTLBuffer *sb; NSUInteger so;
    MTLAccelerationStructureDescriptor *m = [self convert:d what:"buildAccelerationStructure"];
    if (!m || ![self scratch:sr into:&sb offset:&so what:"buildAccelerationStructure scratch"]) return;
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e buildAccelerationStructure:a descriptor:m scratchBuffer:sb scratchBufferOffset:so]; }];
}
- (void)refitAccelerationStructure:(id<MTLAccelerationStructure>)a descriptor:(MTL4AccelerationStructureDescriptor *)d destination:(id<MTLAccelerationStructure>)dst scratchBuffer:(MTL4BufferRange)sr {
    NVMTLBuffer *sb; NSUInteger so;
    MTLAccelerationStructureDescriptor *m = [self convert:d what:"refitAccelerationStructure"];
    if (!m || ![self scratch:sr into:&sb offset:&so what:"refitAccelerationStructure scratch"]) return;
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e refitAccelerationStructure:a descriptor:m destination:dst scratchBuffer:sb scratchBufferOffset:so]; }];
}
- (void)refitAccelerationStructure:(id<MTLAccelerationStructure>)a descriptor:(MTL4AccelerationStructureDescriptor *)d destination:(id<MTLAccelerationStructure>)dst
                     scratchBuffer:(MTL4BufferRange)sr options:(MTLAccelerationStructureRefitOptions)o {
    NVMTLBuffer *sb; NSUInteger so;
    MTLAccelerationStructureDescriptor *m = [self convert:d what:"refitAccelerationStructure"];
    if (!m || ![self scratch:sr into:&sb offset:&so what:"refitAccelerationStructure scratch"]) return;
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e refitAccelerationStructure:a descriptor:m destination:dst scratchBuffer:sb scratchBufferOffset:so options:o]; }];
}
- (void)copyAccelerationStructure:(id<MTLAccelerationStructure>)s toAccelerationStructure:(id<MTLAccelerationStructure>)d {
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e copyAccelerationStructure:s toAccelerationStructure:d]; }];
}
- (void)writeCompactedAccelerationStructureSize:(id<MTLAccelerationStructure>)a toBuffer:(MTL4BufferRange)rg {
    NVMTLBuffer *b; NSUInteger off;
    if (![self scratch:rg into:&b offset:&off what:"writeCompactedAccelerationStructureSize"]) return;
    // Metal 4 writes the size as a 64-bit value.
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e writeCompactedAccelerationStructureSize:a toBuffer:b offset:off sizeDataType:MTLDataTypeULong]; }];
}
- (void)copyAndCompactAccelerationStructure:(id<MTLAccelerationStructure>)s toAccelerationStructure:(id<MTLAccelerationStructure>)d {
    [self as:^(id<MTLAccelerationStructureCommandEncoder> e) { [e copyAndCompactAccelerationStructure:s toAccelerationStructure:d]; }];
}
- (void)writeTimestampWithGranularity:(MTL4TimestampGranularity)g intoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i {
    [self rec:^(NVMTL4Replay *r) {
        id<MTLComputeCommandEncoder> e = [r compute];
        if (nvmtl4_hook_write_timestamp && nvmtl4_hook_write_timestamp(e, r->cb, h, i, (NSInteger)g)) return;
        [r unsupported:@"counter heap timestamps (no counter heap implementation is attached)"];
    }];
}
@end

#pragma mark - command buffer

@implementation NVMTL4CommandBuffer
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)nvmtl4Add:(nvmtl4_op)op { [_ops addObject:[op copy]]; }
- (void)nvmtl4EncoderEnded:(NVMTL4Encoder *)e { if (_open == e) _open = nil; }
- (void)beginCommandBufferWithAllocator:(id<MTL4CommandAllocator>)a {
    // The recording of an earlier use is dropped: a Metal 4 command buffer is reused by beginning it again.
    _ops = [NSMutableArray new]; _sets = [NSMutableArray new]; _ended = NO; _recording = YES; _open = nil;
    _alloc = (NVMTL4CommandAllocator *)a;
    if (a && ![_alloc isKindOfClass:[NVMTL4CommandAllocator class]]) { nvlog("mtl4: beginCommandBufferWithAllocator: an allocator this driver did not make - recording continues without it"); _alloc = nil; }
}
- (void)beginCommandBufferWithAllocator:(id<MTL4CommandAllocator>)a options:(MTL4CommandBufferOptions *)o {
    [self beginCommandBufferWithAllocator:a];
}
- (void)endCommandBuffer {
    if (!_recording) { nvlog("mtl4: endCommandBuffer on a command buffer that was not begun - REFUSED"); return; }
    if (_open) nvlog("mtl4: endCommandBuffer with an encoder still open - that encoder's commands are dropped");
    _recording = NO; _ended = YES; _open = nil;
    if (_alloc) atomic_fetch_add(&_alloc->_bytes, 256 + 64 * (uint64_t)_ops.count);
}
- (BOOL)nvmtl4CanEncode:(const char *)what {
    if (!_recording) { nvlog("mtl4: %s on a command buffer that is not recording - REFUSED", what); return NO; }
    if (_open) { nvlog("mtl4: %s while another encoder is open - REFUSED", what); return NO; }
    return YES;
}
- (id<MTL4RenderCommandEncoder>)renderCommandEncoderWithDescriptor:(MTL4RenderPassDescriptor *)d {
    return [self renderCommandEncoderWithDescriptor:d options:MTL4RenderEncoderOptionNone];
}
- (id<MTL4RenderCommandEncoder>)renderCommandEncoderWithDescriptor:(MTL4RenderPassDescriptor *)d options:(MTL4RenderEncoderOptions)o {
    if (![self nvmtl4CanEncode:"renderCommandEncoderWithDescriptor:"]) return nil;
    if (!d) { nvlog("mtl4: renderCommandEncoderWithDescriptor: nil descriptor - REFUSED"); return nil; }
    NVMTL4RenderEncoder *e = [[NVMTL4RenderEncoder alloc] initWithCommandBuffer:self descriptor:d options:o];
    _open = e; return e;
}
- (id<MTL4ComputeCommandEncoder>)computeCommandEncoder {
    if (![self nvmtl4CanEncode:"computeCommandEncoder"]) return nil;
    NVMTL4ComputeEncoder *e = [[NVMTL4ComputeEncoder alloc] initWithCommandBuffer:self];
    _open = e; return e;
}
- (id<MTL4MachineLearningCommandEncoder>)machineLearningCommandEncoder {
    if (![self nvmtl4CanEncode:"machineLearningCommandEncoder"]) return nil;
    if (!nvmtl4_hook_ml_encoder) { nvmtl4_note_once(@"unsupported: machineLearningCommandEncoder - no machine learning encoder is attached"); return nil; }
    __weak NVMTL4CommandBuffer *wself = self;
    return nvmtl4_hook_ml_encoder(self, ^(void (^op)(id<MTLCommandBuffer> m3cb)) {
        [wself nvmtl4Add:^(NVMTL4Replay *r) { [r closeAll]; op(r->cb); }];
    });
}
- (void)useResidencySet:(id<MTLResidencySet>)s { if (s) [_sets addObject:s]; }
- (void)useResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) if (s[i]) [_sets addObject:s[i]]; }
- (void)pushDebugGroup:(NSString *)s { [self nvmtl4Add:^(NVMTL4Replay *r) { [r->cb pushDebugGroup:s]; }]; }
- (void)popDebugGroup { [self nvmtl4Add:^(NVMTL4Replay *r) { [r->cb popDebugGroup]; }]; }
- (void)writeTimestampIntoHeap:(id<MTL4CounterHeap>)h atIndex:(NSUInteger)i {
    [self nvmtl4Add:^(NVMTL4Replay *r) {
        if (nvmtl4_hook_write_timestamp && nvmtl4_hook_write_timestamp(nil, r->cb, h, i, (NSInteger)MTL4TimestampGranularityRelaxed)) return;
        [r unsupported:@"counter heap timestamps (no counter heap implementation is attached)"];
    }];
}
- (void)resolveCounterHeap:(id<MTL4CounterHeap>)h withRange:(NSRange)range intoBuffer:(MTL4BufferRange)br waitFence:(id<MTLFence>)wf updateFence:(id<MTLFence>)uf {
    NSUInteger off = 0; NVMTLBuffer *b = nvmtl4_resolve_address(br.bufferAddress, &off);
    [self nvmtl4Add:^(NVMTL4Replay *r) {
        if (b && nvmtl4_hook_resolve_heap && nvmtl4_hook_resolve_heap(r->cb, h, range, b, off, wf, uf)) return;
        [r unsupported:b ? @"counter heap resolve (no counter heap implementation is attached)" : @"counter heap resolve into an address no buffer owns"];
    }];
}
@end

#pragma mark - drawable gate

static const char kNVMTL4Gate, kNVMTL4Event;

static dispatch_queue_t nvmtl4_present_queue(void)
{
    static dispatch_queue_t q; static dispatch_once_t once;
    dispatch_once(&once, ^{ q = dispatch_queue_create("nvmtl4.present", dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_USER_INTERACTIVE, 0)); });
    return q;
}

// Replaces one present method of a drawable class with one that waits for the gate signalDrawable attached.
static BOOL nvmtl4_gate_method(Class c, SEL sel)
{
    Method m = class_getInstanceMethod(c, sel);
    if (!m) return NO;
    IMP orig = method_getImplementation(m);
    const char *types = method_getTypeEncoding(m);
    id block;
    if (sel == @selector(present))
        block = ^(id self_) {
            dispatch_group_t g = objc_getAssociatedObject(self_, &kNVMTL4Gate);
            if (!g) { ((void (*)(id, SEL))orig)(self_, sel); return; }
            objc_setAssociatedObject(self_, &kNVMTL4Gate, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            dispatch_group_notify(g, nvmtl4_present_queue(), ^{
                ((void (*)(id, SEL))orig)(self_, sel);
                NVMTLEvent *ev = objc_getAssociatedObject(self_, &kNVMTL4Event); if (ev) ev.signaledValue = 1; });
        };
    else
        block = ^(id self_, double t) {
            dispatch_group_t g = objc_getAssociatedObject(self_, &kNVMTL4Gate);
            if (!g) { ((void (*)(id, SEL, double))orig)(self_, sel, t); return; }
            objc_setAssociatedObject(self_, &kNVMTL4Gate, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
            dispatch_group_notify(g, nvmtl4_present_queue(), ^{
                ((void (*)(id, SEL, double))orig)(self_, sel, t);
                NVMTLEvent *ev = objc_getAssociatedObject(self_, &kNVMTL4Event); if (ev) ev.signaledValue = 1; });
        };
    // On the drawable's own class, so a method inherited from a framework superclass is not changed for everyone.
    class_replaceMethod(c, sel, imp_implementationWithBlock(block), types);
    return YES;
}

// Gates the present methods of the drawable's class, once per class. NO when a present method cannot be found.
static BOOL nvmtl4_gate_install(id drawable)
{
    static NSMutableDictionary<NSString *, NSNumber *> *done; static os_unfair_lock lk = OS_UNFAIR_LOCK_INIT;
    Class c = object_getClass(drawable);
    NSString *key = NSStringFromClass(c);
    os_unfair_lock_lock(&lk);
    if (!done) done = [NSMutableDictionary new];
    NSNumber *state = done[key];
    if (!state) {
        BOOL ok = nvmtl4_gate_method(c, @selector(present));
        ok = nvmtl4_gate_method(c, @selector(presentAtTime:)) && ok;
        ok = nvmtl4_gate_method(c, @selector(presentAfterMinimumDuration:)) && ok;
        done[key] = state = @(ok);
        nvlog("mtl4: drawable class %s present methods %s", key.UTF8String, ok ? "gated behind signalDrawable" : "NOT all found - signalDrawable waits on the CPU instead");
    }
    BOOL ok = state.boolValue;
    os_unfair_lock_unlock(&lk);
    return ok;
}

#pragma mark - queue

// Collects the Metal 3 command buffers of one commit into one feedback.
NVMTL4 @interface NVMTL4Commit : NSObject {
@public NSLock *lk; NSError *err; CFTimeInterval t0, t1; }
@end
@implementation NVMTL4Commit
- (instancetype)init { if ((self = [super init])) lk = [NSLock new]; return self; }
- (void)fail:(NSString *)why {
    [lk lock];
    if (!err) err = [NSError errorWithDomain:@"MTL4CommandQueueErrorDomain" code:MTL4CommandQueueErrorInternal userInfo:@{ NSLocalizedDescriptionKey: why }];
    [lk unlock];
}
- (void)merge:(NSError *)e start:(CFTimeInterval)s end:(CFTimeInterval)en {
    [lk lock];
    if (e && !err) err = e;
    if (s > 0 && (t0 == 0 || s < t0)) t0 = s;
    if (en > t1) t1 = en;
    [lk unlock];
}
@end

NVMTL4 @interface NVMTL4CommandQueue : NSObject <MTL4CommandQueue> {
@public
    id _dev; NSString *_label; NVMTLCommandQueue *_inner; dispatch_queue_t _fbq;
    NSMutableArray *_sets; NSLock *_lock; NSMutableArray<NSString *> *_pending;
    // Queue work runs in the order it was asked for. A commit that has to split (indirect threads dispatch) keeps the queue
    // busy until its last part is committed; work asked for meanwhile waits in _deferred, and the worker drains it.
    dispatch_queue_t _worker; NSMutableArray *_deferred; BOOL _active, _halted, _draining;
    _Atomic int _incomplete;   // parts committed to the Metal 3 queue and not yet finished
}
- (void)nvmtl4Submit:(void (^)(void))work;
- (void)nvmtl4Halted;
- (void)nvmtl4ChainDone;
@end

// One commit being replayed. The replay of every command buffer in it is a list of items stepped one at a time, so a split
// can stop it in the middle of an encoder and a later part carry on from the same place.
NVMTL4 @interface NVMTL4Run : NSObject {
@public
    NVMTL4CommandQueue *q; NVMTL4Replay *r; NSArray *top; NSUInteger ti, ii; BOOL inSealed, skipInner, aborted;
    NVMTL4Commit *agg; dispatch_group_t g; NSArray *alive; NSString *label; unsigned parts;
}
- (BOOL)runPart;
- (void)continueAfterSplit;
@end

@implementation NVMTL4Run
// Replays up to the end or the next split into a new Metal 3 command buffer and commits it. YES when the commit is whole.
- (BOOL)runPart {
    dispatch_group_enter(g);
    id<MTLCommandBuffer> m = [q->_inner commandBuffer];
    if (!m) { [agg fail:@"the queue could not begin a command buffer"]; dispatch_group_leave(g); return YES; }
    if (label) m.label = label;
    r->cb = m; r->ncb = (NVMTLCommandBuffer *)m; r->splitPending = NO;
    r->queueIdle = atomic_load(&q->_incomplete) == 0;
    BOOL halted = NO;
    while (ti < top.count && !halted) {
        id item = top[ti];
        if (![item isKindOfClass:[NVMTL4Sealed class]]) { ((nvmtl4_op)item)(r); ti++; continue; }
        NVMTL4Sealed *sl = item;
        if (!inSealed) {
            // Work from earlier encoders must be visible to this one when the app asked for a queue barrier.
            if (sl->qb && !r->suspended) nvmtl_vk_cmd_barrier(&r->ncb->_c);
            r->encLabel = sl->label;
            skipInner = !sl->open(r); inSealed = YES; ii = 0;
        }
        while (!skipInner && ii < sl->ops.count) {
            nvmtl4_op op = sl->ops[ii]; op(r);
            if (r->splitPending) { halted = YES; break; }
            ii++;
        }
        if (halted) break;
        sl->close(r); r->encLabel = nil;
        if (sl->qa && !r->suspended) nvmtl_vk_cmd_barrier(&r->ncb->_c);
        inSealed = NO; ti++;
    }
    if (halted) { [r endForSplit]; [q nvmtl4Halted]; ++parts; }
    else { [r closeAll]; }
    for (NSString *e in r->errors) [agg fail:e];
    [r->errors removeAllObjects];
    if (halted) nvlog("mtl4 split: part %u of this commit ends at an indirect threads dispatch; the rest is replayed once it has finished (the threadgroup size is read from the GPU buffer)", parts);
    NVMTL4Run *run = self; NSArray *keep = alive;
    [m addCompletedHandler:^(id<MTLCommandBuffer> d) {
        (void)keep;
        [run->agg merge:d.error start:d.GPUStartTime end:d.GPUEndTime];
        atomic_fetch_sub(&run->q->_incomplete, 1);
        if (!halted) { dispatch_group_leave(run->g); return; }
        if (d.error) run->aborted = YES;
        dispatch_async(run->q->_worker, ^{ [run continueAfterSplit]; });
    }];
    atomic_fetch_add(&q->_incomplete, 1);
    [m commit];
    return !halted;
}
// On the queue's worker, after the part before the split has finished.
- (void)continueAfterSplit {
    if (aborted) {
        [agg fail:@"the rest of the commit was not run because an earlier part of it failed"];
        dispatch_group_leave(g); [q nvmtl4ChainDone]; return;
    }
    memcpy(r->args, r->stage.contents, sizeof r->args);
    r->haveArgs = YES; r->stage = nil;
    BOOL whole = [self runPart];   // takes the next part's place in the group before the finished part's is given up
    dispatch_group_leave(g);
    if (whole) [q nvmtl4ChainDone];
}
@end

@implementation NVMTL4CommandQueue
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (void)nvmtl4Unsupported:(NSString *)what {
    nvmtl4_note_once([@"unsupported: " stringByAppendingString:what]);
    [_lock lock]; [_pending addObject:[@"unsupported: " stringByAppendingString:what]]; [_lock unlock];
}
- (void)nvmtl4Halted { [_lock lock]; _halted = YES; [_lock unlock]; }
- (void)nvmtl4ChainDone { [_lock lock]; _halted = NO; [_lock unlock]; [self nvmtl4Idle]; }
// Runs queue work now, or after the work ahead of it when a split commit still holds the queue.
- (void)nvmtl4Submit:(void (^)(void))work {
    [_lock lock];
    if (_active || _deferred.count) { [_deferred addObject:[work copy]]; [_lock unlock]; return; }
    _active = YES;
    [_lock unlock];
    work();
    [self nvmtl4Idle];
}
// Runs the deferred work in order until it is gone or a split commit takes the queue again. One thread drains at a time.
- (void)nvmtl4Idle {
    [_lock lock];
    if (_draining) { [_lock unlock]; return; }
    _draining = YES;
    for (;;) {
        if (_halted || !_deferred.count) { if (!_halted) _active = NO; _draining = NO; [_lock unlock]; return; }
        void (^w)(void) = _deferred.firstObject; [_deferred removeObjectAtIndex:0];
        [_lock unlock];
        w();
        [_lock lock];
    }
}
- (void)commit:(const id<MTL4CommandBuffer> [])cbs count:(NSUInteger)n { [self nvmtl4Commit:cbs count:n options:nil]; }
- (void)commit:(const id<MTL4CommandBuffer> [])cbs count:(NSUInteger)n options:(MTL4CommitOptions *)options { [self nvmtl4Commit:cbs count:n options:options]; }
- (void)nvmtl4Commit:(const id<MTL4CommandBuffer> [])cbs count:(NSUInteger)n options:(MTL4CommitOptions *)options {
    NVMTL4Commit *agg = [NVMTL4Commit new];
    dispatch_group_t g = dispatch_group_create();
    [_lock lock];
    NSArray *sets = [_sets copy];
    for (NSString *p in _pending) [agg fail:p];
    [_pending removeAllObjects];
    [_lock unlock];
    // Every command buffer of one commit is replayed in order onto the same Metal 3 command buffer (or, after a split, the
    // same chain of them). That keeps their order on the queue and lets a suspended render pass be resumed by an encoder
    // of a later command buffer of the commit.
    NSMutableArray *top = [NSMutableArray new], *held = [NSMutableArray arrayWithArray:sets];
    NSString *label = nil;
    for (NSUInteger i = 0; i < n; i++) {
        NVMTL4CommandBuffer *c = (NVMTL4CommandBuffer *)cbs[i];
        if (![c isKindOfClass:[NVMTL4CommandBuffer class]]) { nvlog("mtl4: commit: a command buffer this driver did not make - REFUSED"); [agg fail:@"command buffer was not made by this device"]; continue; }
        if (!c->_ended) { nvlog("mtl4: commit: command buffer %s was not ended with endCommandBuffer - REFUSED", c->_label.UTF8String ?: ""); [agg fail:@"command buffer was not ended"]; continue; }
        if (!label) label = c->_label;
        [top addObjectsFromArray:c->_ops];
        [held addObjectsFromArray:c->_sets];
    }
    NVMTL4Run *run = nil;
    if (top.count || !agg->err) {
        run = [NVMTL4Run new];
        run->q = self; run->top = top; run->agg = agg; run->g = g; run->label = label; run->r = [NVMTL4Replay new];
        // The resources the sets name stay alive until the GPU is done, as the app promised they would be. The allocations
        // are taken now, so a set changed after the commit does not release what this work uses.
        NSMutableArray *alive = [NSMutableArray new];
        for (id<MTLResidencySet> rs in held) { [alive addObject:rs]; [alive addObjectsFromArray:[rs allAllocations]]; }
        run->alive = alive;
    }
    id disp = nil;
    if (options) {
        disp = [options respondsToSelector:NSSelectorFromString(@"commitFeedbackDispatch")] ? [options valueForKey:@"commitFeedbackDispatch"] : nil;
        if (!disp || ![disp respondsToSelector:NSSelectorFromString(@"executeBlocksWithCommitFeedback:")]) {
            nvlog("mtl4: commit: the commit options expose no feedback dispatch on this system - feedback handlers cannot run");
            disp = nil;
        }
    }
    dispatch_queue_t fq = _fbq ?: dispatch_get_global_queue(QOS_CLASS_DEFAULT, 0);
    [self nvmtl4Submit:^{
        if (run) [run runPart];
        // Registered once the first part holds its place in the group; before that the group is empty and would fire.
        if (!disp) return;
        dispatch_group_notify(g, fq, ^{
            NVMTL4CommitFeedback *f = [NVMTL4CommitFeedback new];
            f->_error = agg->err; f->_start = agg->t0; f->_end = agg->t1;
            ((void (*)(id, SEL, id))objc_msgSend)(disp, NSSelectorFromString(@"executeBlocksWithCommitFeedback:"), f);
        });
    }];
}
- (void)signalEvent:(id<MTLEvent>)e value:(uint64_t)v {
    [self nvmtl4Submit:^{ id<MTLCommandBuffer> m = [self->_inner commandBuffer]; [m encodeSignalEvent:e value:v]; [m commit]; }];
}
- (void)waitForEvent:(id<MTLEvent>)e value:(uint64_t)v {
    [self nvmtl4Submit:^{ id<MTLCommandBuffer> m = [self->_inner commandBuffer]; [m encodeWaitForEvent:e value:v]; [m commit]; }];
}
// The app presents the drawable itself after this call, and the picture only reaches the surface when the queue's work
// is done. The Metal 3 path keeps its present until the command buffer completes; here the present is the app's own call,
// so the drawable's present methods are gated: a drawable that signalDrawable marked runs its present on a serial queue
// once an empty command buffer, committed behind the queue's work, completes (the queue completes in order).
// NVMTL4_DRAIN_DRAWABLE=1 returns to waiting on the CPU.
- (void)signalDrawable:(id<MTLDrawable>)d {
    static int drain = -1; if (drain < 0) { const char *e = getenv("NVMTL4_DRAIN_DRAWABLE"); drain = e && *e && *e != '0'; }
    dispatch_group_t g = nil;
    if (!drain && d && nvmtl4_gate_install(d)) {
        g = dispatch_group_create(); dispatch_group_enter(g);
        objc_setAssociatedObject(d, &kNVMTL4Gate, g, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        NVMTLEvent *done = [NVMTLEvent new]; done->_dev = _dev;   // signalled once the present has run: what waitForDrawable waits for
        objc_setAssociatedObject(d, &kNVMTL4Event, done, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    }
    dispatch_semaphore_t sem = g ? nil : dispatch_semaphore_create(0);
    [self nvmtl4Submit:^{
        id<MTLCommandBuffer> m = [self->_inner commandBuffer];
        if (!m) { nvlog("mtl4: signalDrawable: the queue could not begin a command buffer - drawable not gated"); if (g) dispatch_group_leave(g); if (sem) dispatch_semaphore_signal(sem); return; }
        [m addCompletedHandler:^(id<MTLCommandBuffer> x) { if (g) dispatch_group_leave(g); if (sem) dispatch_semaphore_signal(sem); }];
        [m commit];
    }];
    if (sem) {
        dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER);
        nvmtl4_note_once(@"signalDrawable waits on the CPU for the queue to drain before returning");
    }
}
// The drawable is available once the present signalDrawable gated for it has run. The queue waits for that on an event
// the present signals, so nothing is blocked on the CPU. A drawable with no gated present is available already.
- (void)waitForDrawable:(id<MTLDrawable>)d {
    NVMTLEvent *ev = d ? objc_getAssociatedObject(d, &kNVMTL4Event) : nil;
    if (!ev || ev.signaledValue >= 1) return;
    [self nvmtl4Submit:^{ id<MTLCommandBuffer> m = [self->_inner commandBuffer]; [m encodeWaitForEvent:ev value:1]; [m commit]; }];
}
- (void)addResidencySet:(id<MTLResidencySet>)s { if (s) { [_lock lock]; [_sets addObject:s]; [_lock unlock]; } }
- (void)addResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self addResidencySet:s[i]]; }
- (void)removeResidencySet:(id<MTLResidencySet>)s { if (s) { [_lock lock]; [_sets removeObjectIdenticalTo:s]; [_lock unlock]; } }
- (void)removeResidencySets:(const id<MTLResidencySet> __unsafe_unretained [])s count:(NSUInteger)n { for (NSUInteger i = 0; i < n; i++) [self removeResidencySet:s[i]]; }
// Sparse mapping runs in queue order: it gets a Metal 3 command buffer of its own behind everything committed so far,
// and the owner of sparse resources (nvmtl4_hook_sparse_update) binds the pages against it.
- (void)nvmtl4Sparse:(int)kind resource:(id)res source:(id)src heap:(id)heap ops:(const void *)ops size:(size_t)sz count:(NSUInteger)n what:(NSString *)what {
    if (!nvmtl4_hook_sparse_update) {
        [self nvmtl4Unsupported:[what stringByAppendingString:@" (this device creates no sparse or placement-sparse resources, so there is nothing to map)"]];
        return;
    }
    NSData *copy = [NSData dataWithBytes:ops length:sz * n];
    [self nvmtl4Submit:^{
        id<MTLCommandBuffer> m = [self->_inner commandBuffer];
        BOOL ok = m && nvmtl4_hook_sparse_update(m, kind, res, src, heap, copy.bytes, n);
        if (m) [m commit];
        if (!ok) [self nvmtl4Unsupported:[what stringByAppendingString:@" was refused by the sparse resource owner"]];
    }];
}
- (void)updateTextureMappings:(id<MTLTexture>)t heap:(id<MTLHeap>)h operations:(const MTL4UpdateSparseTextureMappingOperation [])ops count:(NSUInteger)n {
    [self nvmtl4Sparse:0 resource:t source:nil heap:h ops:ops size:sizeof(MTL4UpdateSparseTextureMappingOperation) count:n what:@"updateTextureMappings"]; }
- (void)copyTextureMappingsFromTexture:(id<MTLTexture>)s toTexture:(id<MTLTexture>)d operations:(const MTL4CopySparseTextureMappingOperation [])ops count:(NSUInteger)n {
    [self nvmtl4Sparse:1 resource:d source:s heap:nil ops:ops size:sizeof(MTL4CopySparseTextureMappingOperation) count:n what:@"copyTextureMappings"]; }
- (void)updateBufferMappings:(id<MTLBuffer>)b heap:(id<MTLHeap>)h operations:(const MTL4UpdateSparseBufferMappingOperation [])ops count:(NSUInteger)n {
    [self nvmtl4Sparse:2 resource:b source:nil heap:h ops:ops size:sizeof(MTL4UpdateSparseBufferMappingOperation) count:n what:@"updateBufferMappings"]; }
- (void)copyBufferMappingsFromBuffer:(id<MTLBuffer>)s toBuffer:(id<MTLBuffer>)d operations:(const MTL4CopySparseBufferMappingOperation [])ops count:(NSUInteger)n {
    [self nvmtl4Sparse:3 resource:d source:s heap:nil ops:ops size:sizeof(MTL4CopySparseBufferMappingOperation) count:n what:@"copyBufferMappings"]; }
@end

#pragma mark - device entry points

@interface NVMTLDevice (MTL4Core)
- (id<MTL4CommandQueue>)newMTL4CommandQueue NVMTL4;
- (id<MTL4CommandQueue>)newMTL4CommandQueueWithDescriptor:(MTL4CommandQueueDescriptor *)d error:(NSError **)error NVMTL4;
- (id<MTL4CommandAllocator>)newCommandAllocator NVMTL4;
- (id<MTL4CommandAllocator>)newCommandAllocatorWithDescriptor:(MTL4CommandAllocatorDescriptor *)d error:(NSError **)error NVMTL4;
- (id<MTL4CommandBuffer>)newCommandBuffer NVMTL4;
- (id<MTL4ArgumentTable>)newArgumentTableWithDescriptor:(MTL4ArgumentTableDescriptor *)d error:(NSError **)error NVMTL4;
@end

@implementation NVMTLDevice (MTL4Core)
- (id<MTL4CommandQueue>)newMTL4CommandQueue { return [self newMTL4CommandQueueWithDescriptor:nil error:NULL]; }
- (id<MTL4CommandQueue>)newMTL4CommandQueueWithDescriptor:(MTL4CommandQueueDescriptor *)d error:(NSError **)error {
    id<MTLCommandQueue> inner = [self newCommandQueue];
    if (!inner) {
        nvlog("newMTL4CommandQueue: the Metal 3 queue under it FAILED");
        if (error) *error = [NSError errorWithDomain:@"MTL4CommandQueueErrorDomain" code:MTL4CommandQueueErrorInternal userInfo:@{ NSLocalizedDescriptionKey: @"queue creation failed" }];
        return nil;
    }
    NVMTL4CommandQueue *q = [NVMTL4CommandQueue new];
    q->_dev = self; q->_inner = (NVMTLCommandQueue *)inner; q->_label = [d.label copy]; q->_fbq = d.feedbackQueue;
    q->_sets = [NSMutableArray new]; q->_pending = [NSMutableArray new]; q->_lock = [NSLock new];
    q->_deferred = [NSMutableArray new]; q->_worker = dispatch_queue_create("nvmtl4.queue.worker", DISPATCH_QUEUE_SERIAL);
    if (q->_label) inner.label = q->_label;
    nvlog("newMTL4CommandQueue -> %p (over Metal 3 queue %p)", (__bridge void *)q, (__bridge void *)inner);
    return q;
}
- (id<MTL4CommandAllocator>)newCommandAllocator { return [self newCommandAllocatorWithDescriptor:nil error:NULL]; }
- (id<MTL4CommandAllocator>)newCommandAllocatorWithDescriptor:(MTL4CommandAllocatorDescriptor *)d error:(NSError **)error {
    NVMTL4CommandAllocator *a = [NVMTL4CommandAllocator new]; a->_dev = self; a->_label = [d.label copy]; return a;
}
- (id<MTL4CommandBuffer>)newCommandBuffer {
    NVMTL4CommandBuffer *c = [NVMTL4CommandBuffer new]; c->_dev = self; c->_ops = [NSMutableArray new]; c->_sets = [NSMutableArray new]; return c;
}
- (id<MTL4ArgumentTable>)newArgumentTableWithDescriptor:(MTL4ArgumentTableDescriptor *)d error:(NSError **)error {
    if (!d) { nvlog("newArgumentTableWithDescriptor: nil descriptor - REFUSED"); if (error) *error = [NSError errorWithDomain:NSCocoaErrorDomain code:NSFileReadUnknownError userInfo:@{ NSLocalizedDescriptionKey: @"nil descriptor" }]; return nil; }
    return [[NVMTL4ArgumentTable alloc] initWithDescriptor:d device:self];
}
@end
