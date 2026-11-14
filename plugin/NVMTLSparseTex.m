/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Sparse textures: a colour texture whose memory is bound tile by tile, from a sparse heap (the heap hands out tiles on
// demand) or from a placement heap (the caller names the heap tile). 2D, 2D array and 3D textures with colour formats.
// Imported by NVMTLDevice.m after NVMTL4Extras.m.
//
// Tiles take Apple's shapes for each sparse page size (measured on an Apple GPU): a 64 KB tile is the standard 2D shape
// (RGBA8 128x128), a 16 KB tile half its width and height, a 256 KB tile twice both, and a 3D texture tiles with depth 1.
// NVK makes the image in that layout (nvmtl_vk_sparse_mode: 16 KB tiles in small-page mode, depth-1 3D tiles), so its
// Vulkan tile is the 16 KB or 64 KB page; a 256 KB tile is a group of 2x2 Vulkan tiles. The tile tables below are kept
// in Vulkan tiles; a placement heap offset counts the texture's pages, a sparse heap hands out blocks of its own page
// (16 KB) or of 64 KB.
//
// A mapping change is a queue operation that Vulkan does not order against work already submitted, so every change
// waits for the device to go idle (nvmtl_vk_sparse_image_bind). The Metal 3 resource state encoder applies its
// operations when encoding ends, which is before the command buffer's own earlier encoders run; the Metal 4 queue
// operations are applied at the call, in queue order.

#import "NVMTL4Counters.h"

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
#pragma clang diagnostic ignored "-Wobjc-protocol-method-implementation"
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

#define NVMTL4_SPARSE_BLOCK 65536u

// ---------------------------------------------------------------------------------------------------------------
// Tile pool of a sparse heap
// ---------------------------------------------------------------------------------------------------------------

@interface NVMTL4SparsePool : NSObject { @public void *mem; uint32_t memtype; NSUInteger ntiles, hint, blockBytes; NSInteger pageSize; uint8_t *refs; } @end
@implementation NVMTL4SparsePool
- (void)dealloc { free(refs); }
@end
static char kNVMTL4SparsePool;

// Bytes of a sparse page size, 0 for a value that is not one
static NSUInteger nvmtl4_page_bytes(NSInteger ps)
{
    return ps == MTLSparsePageSize16 ? 16384 : ps == MTLSparsePageSize64 ? 65536 : ps == MTLSparsePageSize256 ? 262144 : 0;
}

// The NVK sparse layout for a page size: 16 KB tiles for 16 KB pages, and depth-1 tiles for 3D textures throughout
static int nvmtl4_sparse_mode_for(NSInteger ps) { return (ps == MTLSparsePageSize16 ? 1 : 0) | 2; }

// Allocates `count` consecutive tiles in `refs` (a working copy of the pool's reference counts); NSNotFound when none.
static NSUInteger nvmtl4_pool_alloc(uint8_t *refs, NSUInteger ntiles, NSUInteger count, NSUInteger hint)
{
    for (NSUInteger pass = 0; pass < 2; pass++) {
        NSUInteger from = pass ? 0 : hint, to = pass ? MIN(hint + count, ntiles) : ntiles, run = 0;
        for (NSUInteger i = from; i < to; i++) {
            run = refs[i] ? 0 : run + 1;
            if (run == count) { NSUInteger s = i + 1 - count; for (NSUInteger k = 0; k < count; k++) refs[s + k] = 1; return s; }
        }
    }
    return NSNotFound;
}

id nvmtl4_new_sparse_heap(NVMTLHeap *h, MTLHeapDescriptor *hd)
{
    if (!nvmtl_vk_sparse_textures()) {
        nvlog("newHeapWithDescriptor: a sparse heap cannot be built - the Vulkan device has no sparse residency for images");
        return nil;
    }
    const NSInteger ps = nvmtl4_page_bytes(hd.sparsePageSize) ? hd.sparsePageSize : MTLSparsePageSize64;
    // a heap of 16 KB pages hands out 16 KB blocks, allocated in 4 KB GPU pages (NVK small-page mode) so each maps alone
    const NSUInteger block = ps == MTLSparsePageSize16 ? 16384 : NVMTL4_SPARSE_BLOCK;
    const size_t size = (hd.size + block - 1) / block * block;
    const int was = ps == MTLSparsePageSize16 ? nvmtl_vk_small_pages(1) : -1;
    const int bad = nvmtl_vk_heap_create(size, 0, &h->_hm);
    if (was >= 0) nvmtl_vk_small_pages(was);
    if (bad) { nvlog("newHeapWithDescriptor: SPARSE %lu bytes FAILED", (unsigned long)hd.size); return nil; }
    NVMTL4SparsePool *p = [NVMTL4SparsePool new];
    p->mem = h->_hm.mem; p->memtype = h->_hm.memtype; p->blockBytes = block; p->pageSize = ps; p->ntiles = size / block; p->refs = calloc(p->ntiles, 1);
    if (!p->refs) { nvmtl_vk_heap_destroy(&h->_hm); return nil; }
    objc_setAssociatedObject(h, &kNVMTL4SparsePool, p, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    nvlog("newHeapWithDescriptor: SPARSE %lu bytes = %lu blocks of %lu KB (page size %lu KB) -> %p", (unsigned long)size, (unsigned long)p->ntiles,
          (unsigned long)block / 1024, (unsigned long)nvmtl4_page_bytes(ps) / 1024, (__bridge void *)h);
    return h;
}

// ---------------------------------------------------------------------------------------------------------------
// Sparse texture state
// ---------------------------------------------------------------------------------------------------------------

@interface NVMTL4SparseTex : NSObject {
    @public nvk_sparse_info info; NSUInteger w, h, d, mips, layers; uint32_t type;
    NVMTLHeap *heap; NVMTL4SparsePool *pool; BOOL placement;
    NSUInteger pageBytes, kx, ky;   // the app's sparse page, and the 2x2 group of Vulkan tiles a 256 KB tile is
    NSUInteger *base; NSUInteger ntiles; nvmtl4_sparse_page *tab;   // tile table of every (layer, mip) below the tail
    nvmtl4_sparse_page *tails; NSUInteger ntails, tp;               // the mip tail's pages: tp per layer (or tp in all), ntails slots
}
@end
@implementation NVMTL4SparseTex
- (void)dealloc { free(base); free(tab); free(tails); }
- (NSUInteger)levelW:(NSUInteger)m { return MAX((NSUInteger)1, w >> m); }
- (NSUInteger)levelH:(NSUInteger)m { return MAX((NSUInteger)1, h >> m); }
- (NSUInteger)levelD:(NSUInteger)m { return type == 7 ? MAX((NSUInteger)1, d >> m) : 1; }
- (NSUInteger)gridX:(NSUInteger)m { return ([self levelW:m] + info.tile_w - 1) / info.tile_w; }
- (NSUInteger)gridY:(NSUInteger)m { return ([self levelH:m] + info.tile_h - 1) / info.tile_h; }
- (NSUInteger)gridZ:(NSUInteger)m { return ([self levelD:m] + info.tile_d - 1) / info.tile_d; }
- (NSUInteger)arrayLayers { return type == 7 ? 1 : layers; }
- (NSUInteger)pageBlocks { return pool ? (pageBytes + pool->blockBytes - 1) / pool->blockBytes : 1; }   // sparse heap blocks of one page
- (NSUInteger)metalTileW { return info.tile_w * kx; }
- (NSUInteger)metalTileH { return info.tile_h * ky; }
@end
static char kNVMTL4SparseTex;

static NVMTL4SparseTex *nvmtl4_sparse_state(id t)
{
    if (![t isKindOfClass:[NVMTLTexture class]]) return nil;
    NVMTLTexture *x = t;
    return objc_getAssociatedObject(x->_parent ? x->_parent : x, &kNVMTL4SparseTex);
}
BOOL nvmtl4_texture_has_sparse_state(id t) { return nvmtl4_sparse_state(t) != nil; }

@implementation NVMTLTexture (MTL4Sparse)
- (NSUInteger)firstMipmapInTail { NVMTL4SparseTex *s = nvmtl4_sparse_state(self); return s ? s->info.first_tail : 0; }
- (NSUInteger)tailSizeInBytes { NVMTL4SparseTex *s = nvmtl4_sparse_state(self); return s && s->info.first_tail < s->mips ? (NSUInteger)s->info.tail_size : 0; }
@end

// ---------------------------------------------------------------------------------------------------------------
// Creation
// ---------------------------------------------------------------------------------------------------------------

BOOL nvmtl4_texture_is_placement_sparse(MTLTextureDescriptor *d)
{
    if (!d || ![d respondsToSelector:@selector(placementSparsePageSize)]) return NO;
    const NSInteger ps = ((NSInteger (*)(id, SEL))objc_msgSend)(d, @selector(placementSparsePageSize));
    return ps == MTLSparsePageSize16 || ps == MTLSparsePageSize64 || ps == MTLSparsePageSize256;
}

// Makes a sparse texture. `heap` is the sparse heap that supplies its tiles, or nil for a placement sparse texture.
id nvmtl4_new_sparse_texture(id<MTLDevice> dev, MTLTextureDescriptor *d, NVMTLHeap *heap)
{
    const char *who = heap ? "sparse heap newTextureWithDescriptor" : "newTextureWithDescriptor (placement sparse)";
    if (!nvmtl_vk_sparse_textures()) { nvlog("%s: cannot be built - the Vulkan device has no sparse residency for images", who); return nil; }
    NVMTL4SparsePool *hp = heap ? objc_getAssociatedObject(heap, &kNVMTL4SparsePool) : nil;
    const NSInteger ps = heap ? (hp ? hp->pageSize : MTLSparsePageSize64) : ((NSInteger (*)(id, SEL))objc_msgSend)(d, @selector(placementSparsePageSize));
    if (!nvmtl4_page_bytes(ps)) { nvlog("%s: sparse page size %ld is not a sparse page size - REFUSED", who, (long)ps); return nil; }
    const uint32_t type = (uint32_t)d.textureType;
    if (!(type == MTLTextureType2D || type == MTLTextureType2DArray || type == MTLTextureType3D) || d.sampleCount != 1 ||
        d.storageMode != MTLStorageModePrivate) {
        nvlog("%s: REFUSED - a sparse texture is private storage, single sample, 2D, 2D array or 3D (type %lu, samples %lu, storage %lu)", who,
              (unsigned long)d.textureType, (unsigned long)d.sampleCount, (unsigned long)d.storageMode);
        return nil;
    }
    uint32_t vkf = 0, bpp = 0, dvk = 0, dasp = 0; int a8 = 0;
    if (nvmtl_depthfmt(d.pixelFormat, &dvk, &dasp) == 0) { nvlog("%s: depth and stencil formats are unsupported as sparse textures - REFUSED", who); return nil; }
    if (nvmtl_pixfmt(d.pixelFormat, &vkf, &bpp, &a8)) { nvlog("%s: pixel format %lu has no VkFormat - REFUSED", who, (unsigned long)d.pixelFormat); return nil; }
    NVMTLTexture *t = [NVMTLTexture new];
    t->_ropt = (MTLResourceOptions)d.cpuCacheMode | ((MTLResourceOptions)d.hazardTrackingMode << MTLResourceHazardTrackingModeShift) |
               ((MTLResourceOptions)d.storageMode << MTLResourceStorageModeShift);
    t->_fmt = d.pixelFormat;
    const uint32_t layers = nvmtl_vk_layers_for_desc(type, (uint32_t)d.arrayLength, (uint32_t)d.depth);
    NVMTL4SparseTex *st = [NVMTL4SparseTex new];
    // Apple's tile shapes: NVK lays the image out with 16 KB tiles (in small-page mode) or 64 KB ones, 3D with depth 1
    const int prevMode = nvmtl_vk_sparse_mode(nvmtl4_sparse_mode_for(ps));
    if (prevMode < 0 && ps == MTLSparsePageSize16) { nvlog("%s: 16 KB sparse pages need NVK's sparse tile mode, which this NVK lacks - REFUSED", who); return nil; }
    const int prevSmall = ps == MTLSparsePageSize16 ? nvmtl_vk_small_pages(1) : -1;
    const int made = nvmtl_vk_image_create_sparse((uint32_t)d.width, (uint32_t)d.height, vkf, bpp, a8, (uint32_t)d.mipmapLevelCount, (d.usage & MTLTextureUsageShaderWrite) != 0,
                                                  type, layers, &t->_i, &st->info);
    if (prevSmall >= 0) nvmtl_vk_small_pages(prevSmall);
    if (prevMode >= 0) nvmtl_vk_sparse_mode(prevMode);
    if (made) {
        nvlog("%s: FAILED - the sparse image could not be made (%lux%lu, format %lu)", who, (unsigned long)d.width, (unsigned long)d.height, (unsigned long)d.pixelFormat);
        return nil;
    }
    const NSUInteger vtile = ps == MTLSparsePageSize16 ? 16384 : NVMTL4_SPARSE_BLOCK;
    if (st->info.tile_bytes != vtile || (hp && hp->blockBytes != vtile)) {
        nvlog("%s: the GPU's tile is %u bytes, not the %lu-byte block this page size maps - REFUSED", who, st->info.tile_bytes, (unsigned long)vtile);
        nvmtl_vk_image_destroy(&t->_i);
        return nil;
    }
    st->pageBytes = nvmtl4_page_bytes(ps); st->kx = st->ky = ps == MTLSparsePageSize256 ? 2 : 1;
    t->_q = nvmtl_device_queue(); t->_stex = t->_i.storage == 1; t->_usage = d.usage;   // 2 = a view-only storage marker, not a storage image
    nvmtl_texture_desc_swizzle(t, d);
    st->w = d.width; st->h = d.height; st->d = d.depth; st->mips = t->_i.mips; st->layers = layers; st->type = type;
    st->heap = heap; st->placement = heap == nil;
    if (heap) st->pool = objc_getAssociatedObject(heap, &kNVMTL4SparsePool);
    if (heap && !st->pool) { nvlog("%s: the heap has no tile pool - REFUSED", who); nvmtl_vk_image_destroy(&t->_i); return nil; }
    const NSUInteger al = [st arrayLayers], tailAt = MIN((NSUInteger)st->info.first_tail, st->mips);
    st->base = calloc(al * st->mips + 1, sizeof *st->base);
    NSUInteger n = 0;
    for (NSUInteger l = 0; l < al; l++) for (NSUInteger m = 0; m < tailAt; m++) {
        st->base[l * st->mips + m] = n;
        n += [st gridX:m] * [st gridY:m] * [st gridZ:m];
    }
    st->ntiles = n; st->tab = calloc(MAX(n, (NSUInteger)1), sizeof *st->tab);
    st->tp = (st->info.tail_size + st->pageBytes - 1) / st->pageBytes;
    st->ntails = tailAt < st->mips ? (st->info.single_tail ? 1 : al) * st->tp : 0;
    st->tails = calloc(MAX(st->ntails, (NSUInteger)1), sizeof *st->tails);
    if (!st->base || !st->tab || !st->tails) { nvmtl_vk_image_destroy(&t->_i); return nil; }
    objc_setAssociatedObject(t, &kNVMTL4SparseTex, st, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    nvlog("%s: %lux%lu %lu mips -> %p (tiles %ux%ux%u, %lu below the tail, tail from level %u of %lu, %llu-byte tail)", who, (unsigned long)d.width, (unsigned long)d.height,
          (unsigned long)st->mips, (__bridge void *)t, st->info.tile_w, st->info.tile_h, st->info.tile_d, (unsigned long)n, st->info.first_tail, (unsigned long)st->mips,
          (unsigned long long)st->info.tail_size);
    return t;
}

id nvmtl4_new_placement_sparse_texture(id<MTLDevice> dev, MTLTextureDescriptor *d) { return nvmtl4_new_sparse_texture(dev, d, nil); }
id nvmtl4_new_sparse_heap_texture(NVMTLHeap *heap, MTLTextureDescriptor *d) { return nvmtl4_new_sparse_texture(heap->_dev, d, heap); }

// ---------------------------------------------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------------------------------------------

typedef struct { BOOL map; MTLRegion region; NSUInteger level, slice, heapOffset; } nvmtl4_tex_op;

// Tile coordinate ranges of a region at a level; NO (logged) when the region leaves the level. Metal gives the region in
// the texture's tiles (measured, and the API's own words); the ranges returned are in Vulkan tiles, of which a 256 KB
// tile spans 2x2 (an edge tile may have fewer).
static BOOL nvmtl4_region_tiles(NVMTL4SparseTex *s, MTLRegion r, NSUInteger m, NSUInteger *x0, NSUInteger *x1, NSUInteger *y0, NSUInteger *y1, NSUInteger *z0, NSUInteger *z1)
{
    const NSUInteger gx = [s gridX:m], gy = [s gridY:m], gz = [s gridZ:m];
    const NSUInteger mx = (gx + s->kx - 1) / s->kx, my = (gy + s->ky - 1) / s->ky;   // the level in the app's tiles
    const NSUInteger dd = s->type == 7 ? r.size.depth : 1, oz = s->type == 7 ? r.origin.z : 0;
    if (!r.size.width || !r.size.height || !dd || r.origin.x + r.size.width > mx || r.origin.y + r.size.height > my || oz + dd > gz ||
        (s->type != 7 && (r.origin.z || r.size.depth > 1))) {
        nvlog("sparse texture mapping: tile region %lu,%lu,%lu %lux%lux%lu leaves level %lu (%lux%lux%lu tiles) - REFUSED",
              (unsigned long)r.origin.x, (unsigned long)r.origin.y, (unsigned long)r.origin.z, (unsigned long)r.size.width, (unsigned long)r.size.height,
              (unsigned long)r.size.depth, (unsigned long)m, (unsigned long)mx, (unsigned long)my, (unsigned long)gz);
        return NO;
    }
    *x0 = r.origin.x * s->kx; *x1 = MIN((r.origin.x + r.size.width) * s->kx, gx);
    *y0 = r.origin.y * s->ky; *y1 = MIN((r.origin.y + r.size.height) * s->ky, gy);
    *z0 = oz; *z1 = oz + dd;
    return YES;
}

static void nvmtl4_release_page(NVMTL4SparseTex *s, uint8_t *refs, nvmtl4_sparse_page *p, NSUInteger blocks)
{
    if (s->pool && p->mem && p->mem == s->pool->mem) { const NSUInteger first = p->off / s->pool->blockBytes; for (NSUInteger k = 0; k < blocks; k++) if (refs[first + k]) refs[first + k]--; }
    memset(p, 0, sizeof *p);
}

// Applies `ops` to the texture as one transaction: the new table is built on copies, bound with one Vulkan operation, and
// only then replaces the old one. `heap` is the placement heap of the operations (Metal 4), nil for a sparse heap.
static BOOL nvmtl4_sparse_tex_apply(NVMTLTexture *tex, NVMTLHeap *heap, const nvmtl4_tex_op *ops, NSUInteger count)
{
    NVMTL4SparseTex *s = nvmtl4_sparse_state(tex);
    if (!s) { nvlog("sparse texture mapping: %s is not a sparse texture this device made - REFUSED", tex ? object_getClassName(tex) : "nil"); return NO; }
    if (s->placement && count && !heap) {
        for (NSUInteger k = 0; k < count; k++) if (ops[k].map) { nvlog("sparse texture mapping: a map operation on a placement sparse texture needs a placement heap - REFUSED"); return NO; }
    }
    nvmtl4_sparse_page *tab = calloc(MAX(s->ntiles, (NSUInteger)1), sizeof *tab), *tails = calloc(MAX(s->ntails, (NSUInteger)1), sizeof *tails);
    uint8_t *refs = s->pool ? malloc(s->pool->ntiles) : NULL;
    if (!tab || !tails || (s->pool && !refs)) { free(tab); free(tails); free(refs); return NO; }
    memcpy(tab, s->tab, s->ntiles * sizeof *tab); memcpy(tails, s->tails, s->ntails * sizeof *tails);
    if (refs) @synchronized (s->pool) { memcpy(refs, s->pool->refs, s->pool->ntiles); }
    const NSUInteger al = [s arrayLayers], pageBlocks = [s pageBlocks];
    BOOL ok = YES;
    for (NSUInteger k = 0; ok && k < count; k++) {
        const nvmtl4_tex_op *op = &ops[k];
        if (op->slice >= al || op->level >= s->mips) { nvlog("sparse texture mapping: slice %lu level %lu is outside the texture (%lu slices, %lu levels) - REFUSED", (unsigned long)op->slice, (unsigned long)op->level, (unsigned long)al, (unsigned long)s->mips); ok = NO; break; }
        if (op->map && s->placement && (!heap || heap->_type != MTLHeapTypePlacement || !heap->_hm.mem)) { nvlog("sparse texture mapping: op %lu maps tiles but %s - REFUSED", (unsigned long)k, !heap ? "no heap was given" : "the heap is not a placement heap with memory"); ok = NO; break; }
        if (op->level >= s->info.first_tail) {
            // the mip tail, as Metal addresses it: tiles origin.x .. origin.x + width - 1 of the layer's tail (height 1)
            const NSUInteger ti = s->info.single_tail ? 0 : op->slice;
            if ((ti + 1) * s->tp > s->ntails || !op->region.size.width || op->region.origin.x + op->region.size.width > s->tp) {
                nvlog("sparse texture mapping: tail tiles %lu..%lu leave the %lu-tile mip tail - REFUSED", (unsigned long)op->region.origin.x,
                      (unsigned long)(op->region.origin.x + op->region.size.width), (unsigned long)s->tp);
                ok = NO; break;
            }
            for (NSUInteger i = 0; ok && i < op->region.size.width; i++) {
                nvmtl4_sparse_page *pg = &tails[ti * s->tp + op->region.origin.x + i];
                nvmtl4_release_page(s, refs, pg, pageBlocks);
                if (!op->map) continue;
                if (s->placement) {
                    if ((op->heapOffset + i + 1) * s->pageBytes > heap->_hm.size) { nvlog("sparse texture mapping: tail heap page %lu is past the heap - REFUSED", (unsigned long)(op->heapOffset + i)); ok = NO; break; }
                    *pg = (nvmtl4_sparse_page){ heap->_hm.mem, heap->_hm.memtype, (uint64_t)(op->heapOffset + i) * s->pageBytes };
                } else {
                    const NSUInteger at = nvmtl4_pool_alloc(refs, s->pool->ntiles, pageBlocks, s->pool->hint);
                    if (at == NSNotFound) { nvlog("sparse texture mapping: the sparse heap has no run of %lu free blocks for a mip tail page - REFUSED", (unsigned long)pageBlocks); ok = NO; break; }
                    *pg = (nvmtl4_sparse_page){ s->pool->mem, s->pool->memtype, (uint64_t)at * s->pool->blockBytes };
                }
            }
            continue;
        }
        NSUInteger x0, x1, y0, y1, z0, z1;
        if (!nvmtl4_region_tiles(s, op->region, op->level, &x0, &x1, &y0, &y1, &z0, &z1)) { ok = NO; break; }
        const NSUInteger gx = [s gridX:op->level], gy = [s gridY:op->level], b = s->base[op->slice * s->mips + op->level];
        // heap pages are counted per app tile, x fastest; a 256 KB tile puts its 2x2 Vulkan tiles in its page in row order
        const NSUInteger mtx = (x1 - x0 + s->kx - 1) / s->kx, mty = (y1 - y0 + s->ky - 1) / s->ky;
        for (NSUInteger z = z0; ok && z < z1; z++) for (NSUInteger y = y0; ok && y < y1; y++) for (NSUInteger x = x0; x < x1; x++) {
            nvmtl4_sparse_page *p = &tab[b + (z * gy + y) * gx + x];
            if (!op->map) { nvmtl4_release_page(s, refs, p, 1); continue; }
            if (s->placement) {
                const NSUInteger seq = ((z - z0) * mty + (y - y0) / s->ky) * mtx + (x - x0) / s->kx, sub = ((y - y0) % s->ky) * s->kx + (x - x0) % s->kx;
                if ((op->heapOffset + seq + 1) * s->pageBytes > heap->_hm.size) { nvlog("sparse texture mapping: heap page %lu is past the heap - REFUSED", (unsigned long)(op->heapOffset + seq)); ok = NO; break; }
                *p = (nvmtl4_sparse_page){ heap->_hm.mem, heap->_hm.memtype, (uint64_t)(op->heapOffset + seq) * s->pageBytes + (uint64_t)sub * s->info.tile_bytes };
            } else if (!p->mem) {   // already mapped tiles keep their memory
                const NSUInteger at = nvmtl4_pool_alloc(refs, s->pool->ntiles, 1, s->pool->hint);
                if (at == NSNotFound) { nvlog("sparse texture mapping: the sparse heap is out of tiles (%lu) - REFUSED", (unsigned long)s->pool->ntiles); ok = NO; break; }
                *p = (nvmtl4_sparse_page){ s->pool->mem, s->pool->memtype, (uint64_t)at * s->pool->blockBytes };
            }
        }
    }
    // The changed tiles and tails become one bind.
    nvk_sparse_tile_run *runs = NULL; uint32_t nruns = 0;
    if (ok) {
        runs = calloc(s->ntiles + s->ntails + 1, sizeof *runs);
        if (!runs) ok = NO;
    }
    if (ok) {
        for (NSUInteger l = 0; l < al; l++) for (NSUInteger m = 0; m < MIN((NSUInteger)s->info.first_tail, s->mips); m++) {
            const NSUInteger gx = [s gridX:m], gy = [s gridY:m], gz = [s gridZ:m], b = s->base[l * s->mips + m];
            const NSUInteger W = [s levelW:m], H = [s levelH:m], D = [s levelD:m];
            for (NSUInteger z = 0; z < gz; z++) for (NSUInteger y = 0; y < gy; y++) for (NSUInteger x = 0; x < gx; x++) {
                const NSUInteger i = b + (z * gy + y) * gx + x;
                if (tab[i].mem == s->tab[i].mem && tab[i].off == s->tab[i].off) continue;
                nvk_sparse_tile_run *r = &runs[nruns++];
                r->mem = tab[i].mem; r->memtype = tab[i].memtype; r->mem_off = tab[i].off; r->mip = (uint32_t)m; r->layer = (uint32_t)l;
                r->x = (int32_t)(x * s->info.tile_w); r->y = (int32_t)(y * s->info.tile_h); r->z = (int32_t)(z * s->info.tile_d);
                r->w = (uint32_t)MIN((NSUInteger)s->info.tile_w, W - x * s->info.tile_w); r->h = (uint32_t)MIN((NSUInteger)s->info.tile_h, H - y * s->info.tile_h);
                r->d = (uint32_t)MIN((NSUInteger)s->info.tile_d, D - z * s->info.tile_d);
            }
        }
        for (NSUInteger t = 0; t < s->ntails; t++) {   // one opaque bind per changed tail page
            if (tails[t].mem == s->tails[t].mem && tails[t].off == s->tails[t].off) continue;
            const NSUInteger layer = t / s->tp, pg = t % s->tp;
            nvk_sparse_tile_run *r = &runs[nruns++];
            r->tail = 1; r->mem = tails[t].mem; r->memtype = tails[t].memtype; r->mem_off = tails[t].off;
            r->tail_off = s->info.tail_offset + (s->info.single_tail ? 0 : (uint64_t)layer * s->info.tail_stride) + (uint64_t)pg * s->pageBytes;
            r->tail_size = MIN((uint64_t)s->pageBytes, s->info.tail_size - (uint64_t)pg * s->pageBytes);
            r->layer = (uint32_t)layer;
        }
        ok = nruns == 0 || nvmtl_vk_sparse_image_bind(&tex->_i, runs, nruns) == 0;
    }
    if (ok) {
        @synchronized (s) {
            memcpy(s->tab, tab, s->ntiles * sizeof *tab); memcpy(s->tails, tails, s->ntails * sizeof *tails);
            if (heap) { if (!s->heap) s->heap = heap; }
        }
        if (refs) @synchronized (s->pool) { memcpy(s->pool->refs, refs, s->pool->ntiles); }
    } else nvlog("sparse texture mapping: FAILED - the mapping of %p is unchanged", (__bridge void *)tex);
    free(runs); free(tab); free(tails); free(refs);
    return ok;
}

// Metal 4 queue operations -----------------------------------------------------------------------------------------

BOOL nvmtl4_sparse_texture_update(id<MTLTexture> texture, id<MTLHeap> heap, const MTL4UpdateSparseTextureMappingOperation *ops, NSUInteger count)
{
    nvmtl4_tex_op *o = calloc(MAX(count, (NSUInteger)1), sizeof *o);
    if (!o) return NO;
    for (NSUInteger k = 0; k < count; k++)
        o[k] = (nvmtl4_tex_op){ ops[k].mode == MTLSparseTextureMappingModeMap, ops[k].textureRegion, ops[k].textureLevel, ops[k].textureSlice, ops[k].heapOffset };
    const BOOL ok = nvmtl4_sparse_tex_apply((NVMTLTexture *)texture, [(id)heap isKindOfClass:[NVMTLHeap class]] ? (NVMTLHeap *)heap : nil, o, count);
    free(o);
    return ok;
}

BOOL nvmtl4_sparse_texture_copy(id<MTLTexture> src, id<MTLTexture> dst, const MTL4CopySparseTextureMappingOperation *ops, NSUInteger count)
{
    NVMTL4SparseTex *a = nvmtl4_sparse_state(src), *b = nvmtl4_sparse_state(dst);
    if (!a || !b) { nvlog("copyTextureMappings: both textures must be sparse textures this device made - REFUSED"); return NO; }
    if (a->info.tile_w != b->info.tile_w || a->info.tile_h != b->info.tile_h || a->info.tile_d != b->info.tile_d || a->pageBytes != b->pageBytes) { nvlog("copyTextureMappings: tile sizes differ - REFUSED"); return NO; }
    NVMTLTexture *dt = (NVMTLTexture *)dst;
    // A copy is expressed as map operations onto the source's own memory, so the destination shares the tiles.
    nvmtl4_sparse_page *tab = calloc(MAX(b->ntiles, (NSUInteger)1), sizeof *tab), *tails = calloc(MAX(b->ntails, (NSUInteger)1), sizeof *tails);
    if (!tab || !tails) { free(tab); free(tails); return NO; }
    memcpy(tab, b->tab, b->ntiles * sizeof *tab); memcpy(tails, b->tails, b->ntails * sizeof *tails);
    // A page of the destination's own sparse heap that two textures now share is counted twice, so it is freed with the last.
    uint8_t *refs = b->pool ? malloc(b->pool->ntiles) : NULL;
    if (refs) @synchronized (b->pool) { memcpy(refs, b->pool->refs, b->pool->ntiles); }
    #define NVMTL4_SHARE(DST, SRC, BLOCKS) do { \
        if (refs && (DST).mem == b->pool->mem) { const NSUInteger f_ = (DST).off / b->pool->blockBytes; for (NSUInteger q_ = 0; q_ < (BLOCKS); q_++) if (refs[f_ + q_]) refs[f_ + q_]--; } \
        (DST) = (SRC); \
        if (refs && (DST).mem == b->pool->mem) { const NSUInteger f_ = (DST).off / b->pool->blockBytes; for (NSUInteger q_ = 0; q_ < (BLOCKS); q_++) if (refs[f_ + q_] < 255) refs[f_ + q_]++; } } while (0)
    BOOL ok = YES;
    for (NSUInteger k = 0; ok && k < count; k++) {
        const MTL4CopySparseTextureMappingOperation *op = &ops[k];
        if (op->sourceSlice >= [a arrayLayers] || op->destinationSlice >= [b arrayLayers] || op->sourceLevel >= a->mips || op->destinationLevel >= b->mips) { ok = NO; break; }
        if (op->sourceLevel >= a->info.first_tail || op->destinationLevel >= b->info.first_tail) {
            if (op->sourceLevel < a->info.first_tail || op->destinationLevel < b->info.first_tail) { nvlog("copyTextureMappings: a mip tail can only be copied to a mip tail - REFUSED"); ok = NO; break; }
            // tail tiles origin.x .. + width of the source tail onto the destination tail from destinationOrigin.x
            const NSUInteger sa = (a->info.single_tail ? 0 : op->sourceSlice) * a->tp, da = (b->info.single_tail ? 0 : op->destinationSlice) * b->tp;
            if (op->sourceRegion.origin.x + op->sourceRegion.size.width > a->tp || op->destinationOrigin.x + op->sourceRegion.size.width > b->tp) {
                nvlog("copyTextureMappings: tail tiles leave the mip tail - REFUSED"); ok = NO; break; }
            for (NSUInteger i = 0; i < op->sourceRegion.size.width; i++)
                NVMTL4_SHARE(tails[da + op->destinationOrigin.x + i], a->tails[sa + op->sourceRegion.origin.x + i], [b pageBlocks]);
            continue;
        }
        NSUInteger x0, x1, y0, y1, z0, z1;
        if (!nvmtl4_region_tiles(a, op->sourceRegion, op->sourceLevel, &x0, &x1, &y0, &y1, &z0, &z1)) { ok = NO; break; }
        const NSUInteger dx = op->destinationOrigin.x * b->kx, dy = op->destinationOrigin.y * b->ky, dz = b->type == 7 ? op->destinationOrigin.z : 0;   // in tiles
        const NSUInteger agx = [a gridX:op->sourceLevel], agy = [a gridY:op->sourceLevel], bgx = [b gridX:op->destinationLevel], bgy = [b gridY:op->destinationLevel], bgz = [b gridZ:op->destinationLevel];
        const NSUInteger ab = a->base[op->sourceSlice * a->mips + op->sourceLevel], bb = b->base[op->destinationSlice * b->mips + op->destinationLevel];
        for (NSUInteger z = z0; ok && z < z1; z++) for (NSUInteger y = y0; ok && y < y1; y++) for (NSUInteger x = x0; x < x1; x++) {
            const NSUInteger tx = dx + (x - x0), ty = dy + (y - y0), tz = dz + (z - z0);
            if (tx >= bgx || ty >= bgy || tz >= bgz) { nvlog("copyTextureMappings: the destination tile is outside the destination level - REFUSED"); ok = NO; break; }
            NVMTL4_SHARE(tab[bb + (tz * bgy + ty) * bgx + tx], a->tab[ab + (z * agy + y) * agx + x], 1);
        }
    }
    nvk_sparse_tile_run *runs = ok ? calloc(b->ntiles + b->ntails + 1, sizeof *runs) : NULL; uint32_t nruns = 0;
    if (ok && !runs) ok = NO;
    if (ok) {
        const NSUInteger al = [b arrayLayers];
        for (NSUInteger l = 0; l < al; l++) for (NSUInteger m = 0; m < MIN((NSUInteger)b->info.first_tail, b->mips); m++) {
            const NSUInteger gx = [b gridX:m], gy = [b gridY:m], gz = [b gridZ:m], bs = b->base[l * b->mips + m], W = [b levelW:m], H = [b levelH:m], D = [b levelD:m];
            for (NSUInteger z = 0; z < gz; z++) for (NSUInteger y = 0; y < gy; y++) for (NSUInteger x = 0; x < gx; x++) {
                const NSUInteger i = bs + (z * gy + y) * gx + x;
                if (tab[i].mem == b->tab[i].mem && tab[i].off == b->tab[i].off) continue;
                nvk_sparse_tile_run *r = &runs[nruns++];
                r->mem = tab[i].mem; r->memtype = tab[i].memtype; r->mem_off = tab[i].off; r->mip = (uint32_t)m; r->layer = (uint32_t)l;
                r->x = (int32_t)(x * b->info.tile_w); r->y = (int32_t)(y * b->info.tile_h); r->z = (int32_t)(z * b->info.tile_d);
                r->w = (uint32_t)MIN((NSUInteger)b->info.tile_w, W - x * b->info.tile_w); r->h = (uint32_t)MIN((NSUInteger)b->info.tile_h, H - y * b->info.tile_h);
                r->d = (uint32_t)MIN((NSUInteger)b->info.tile_d, D - z * b->info.tile_d);
            }
        }
        for (NSUInteger t = 0; t < b->ntails; t++) {
            if (tails[t].mem == b->tails[t].mem && tails[t].off == b->tails[t].off) continue;
            const NSUInteger layer = t / b->tp, pg = t % b->tp;
            nvk_sparse_tile_run *r = &runs[nruns++];
            r->tail = 1; r->mem = tails[t].mem; r->memtype = tails[t].memtype; r->mem_off = tails[t].off;
            r->tail_off = b->info.tail_offset + (b->info.single_tail ? 0 : (uint64_t)layer * b->info.tail_stride) + (uint64_t)pg * b->pageBytes;
            r->tail_size = MIN((uint64_t)b->pageBytes, b->info.tail_size - (uint64_t)pg * b->pageBytes); r->layer = (uint32_t)layer;
        }
        ok = nruns == 0 || nvmtl_vk_sparse_image_bind(&dt->_i, runs, nruns) == 0;
    }
    if (ok) {
        @synchronized (b) { memcpy(b->tab, tab, b->ntiles * sizeof *tab); memcpy(b->tails, tails, b->ntails * sizeof *tails); if (a->heap && !b->heap) b->heap = a->heap; }
        if (refs) @synchronized (b->pool) { memcpy(b->pool->refs, refs, b->pool->ntiles); }
    } else nvlog("copyTextureMappings: FAILED - the mapping of %p is unchanged", (__bridge void *)dst);
    #undef NVMTL4_SHARE
    free(runs); free(tab); free(tails); free(refs);
    return ok;
}

// ---------------------------------------------------------------------------------------------------------------
// Metal 3 resource state encoder
// ---------------------------------------------------------------------------------------------------------------

@interface NVMTLResourceStateCommandEncoder : NSObject <MTLResourceStateCommandEncoder> {
    @public NVMTLCommandBuffer *_cb; NSString *_label; NSMutableArray *_ops;   // each: @[texture, NSValue ops]
}
@end

@implementation NVMTLResourceStateCommandEncoder
- (id<MTLDevice>)device { return (id<MTLDevice>)gNVMTLMainDevice; }
- (NSString *)label { return _label; }
- (void)setLabel:(NSString *)l { _label = [l copy]; }
- (void)insertDebugSignpost:(NSString *)s {}
- (void)pushDebugGroup:(NSString *)s {}
- (void)popDebugGroup {}
- (void)queue:(id<MTLTexture>)t op:(nvmtl4_tex_op)op
{
    if (![(id)t isKindOfClass:[NVMTLTexture class]]) { nvlog("resource state encoder: %s is not a texture this device made - REFUSED", t ? object_getClassName(t) : "nil"); return; }
    if (!_ops) _ops = [NSMutableArray new];
    [_ops addObject:@[t, [NSData dataWithBytes:&op length:sizeof op]]];
}
- (void)updateTextureMapping:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode region:(MTLRegion)region mipLevel:(NSUInteger)level slice:(NSUInteger)slice
{
    [self queue:texture op:(nvmtl4_tex_op){ mode == MTLSparseTextureMappingModeMap, region, level, slice, 0 }];
}
- (void)updateTextureMappings:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode regions:(const MTLRegion[])regions mipLevels:(const NSUInteger[])levels
                       slices:(const NSUInteger[])slices numRegions:(NSUInteger)n
{
    for (NSUInteger i = 0; i < n; i++) [self updateTextureMapping:texture mode:mode region:regions[i] mipLevel:levels[i] slice:slices[i]];
}
- (void)updateTextureMapping:(id<MTLTexture>)texture mode:(MTLSparseTextureMappingMode)mode indirectBuffer:(id<MTLBuffer>)buffer indirectBufferOffset:(NSUInteger)offset
{
    // The buffer is read when encoding ends: it must be CPU readable and filled by then (struct MTLMapIndirectBufferFormat).
    const uint8_t *p = [(id)buffer isKindOfClass:[NVMTLBuffer class]] ? nvmtl_buf_cpu((NVMTLBuffer *)buffer) : NULL;
    if (!p || offset + 4 > buffer.length) { nvlog("resource state encoder: the indirect mapping buffer is not CPU readable - REFUSED"); return; }
    uint32_t n; memcpy(&n, p + offset, 4);
    if (offset + 4 + (NSUInteger)n * sizeof(MTLMapIndirectArguments) > buffer.length) { nvlog("resource state encoder: %u indirect mappings do not fit the buffer - REFUSED", n); return; }
    const MTLMapIndirectArguments *a = (const MTLMapIndirectArguments *)(p + offset + 4);
    for (uint32_t i = 0; i < n; i++)
        [self updateTextureMapping:texture mode:mode region:MTLRegionMake3D(a[i].regionOriginX, a[i].regionOriginY, a[i].regionOriginZ, a[i].regionSizeWidth, a[i].regionSizeHeight, a[i].regionSizeDepth)
                          mipLevel:a[i].mipMapLevel slice:a[i].sliceId];
}
- (void)updateFence:(id<MTLFence>)f {}
- (void)waitForFence:(id<MTLFence>)f {}
- (void)barrierAfterQueueStages:(MTLStages)after beforeStages:(MTLStages)before {}
- (void)endEncoding
{
    NVMTLCommandBuffer *cb = _cb; if (!cb) return;
    _cb = nil; cb->_nEnd++;
    NSArray *ops = _ops; _ops = nil;
    if (!ops.count) return;
    // Metal applies the mappings at this point of the command buffer: after the work encoded before this encoder has
    // run, before the work encoded after it. The command buffer signals an event here and holds the rest of its
    // submission on the CPU until the mappings are bound (a held part is never handed to Vulkan, so the bind's wait for
    // the device to go idle cannot wait on it). WAS: the mappings were bound while encoding ended, before any of this
    // command buffer had run, so earlier work in it read the new mapping.
    NVMTLSharedEvent *ev = [NVMTLSharedEvent new]; ev->_dev = (id<MTLDevice>)gNVMTLMainDevice;
    static MTLSharedEventListener *listener; static dispatch_once_t once;
    dispatch_once(&once, ^{ listener = [[MTLSharedEventListener alloc] initWithDispatchQueue:
        dispatch_queue_create("nvmtl.sparse.mapping", DISPATCH_QUEUE_SERIAL)]; });
    __weak NVMTLCommandBuffer *weakCB = cb;
    [ev notifyListener:listener atValue:1 block:^(id<MTLSharedEvent> e, uint64_t v) {
        (void)v;
        BOOL ok = YES;
        NSUInteger i = 0;
        // consecutive operations on one texture are applied together
        while (i < ops.count) {
            NVMTLTexture *t = ops[i][0]; NSMutableData *all = [NSMutableData new];
            for (; i < ops.count && ops[i][0] == t; i++) [all appendData:ops[i][1]];
            if (!nvmtl4_sparse_tex_apply(t, nil, all.bytes, all.length / sizeof(nvmtl4_tex_op))) ok = NO;
        }
        if (!ok) [weakCB nvmtlRecordEncodingError:[NSError errorWithDomain:MTLCommandBufferErrorDomain code:MTLCommandBufferErrorInternal
            userInfo:@{NSLocalizedDescriptionKey: @"Sparse texture mapping update failed"}]];
        e.signaledValue = 2;   // release the rest of the command buffer, also after a failed bind
    }];
    [cb encodeSignalEvent:ev value:1];
    [cb encodeWaitForEvent:ev value:2];
}
@end

id nvmtl4_new_resource_state_encoder(id cbObj)
{
    NVMTLCommandBuffer *cb = cbObj;
    NVMTLResourceStateCommandEncoder *e = [NVMTLResourceStateCommandEncoder new];
    e->_cb = cb; cb->_nEnc++;
    return e;
}

// ---------------------------------------------------------------------------------------------------------------
// Device queries
// ---------------------------------------------------------------------------------------------------------------

// Tile size in texels for a texture type and pixel format, zero when the format has no sparse tiling here.
MTLSize nvmtl4_sparse_tile_size(NSUInteger textureType, NSUInteger pixelFormat, NSUInteger sampleCount, NSInteger pageSize)
{
    uint32_t vkf = 0, bpp = 0, tw = 0, th = 0, td = 0; int a8 = 0;
    if (sampleCount > 1 || !(textureType == MTLTextureType2D || textureType == MTLTextureType2DArray || textureType == MTLTextureType3D)) return MTLSizeMake(0, 0, 0);
    if (!nvmtl4_page_bytes(pageSize)) return MTLSizeMake(0, 0, 0);
    if (nvmtl_pixfmt((MTLPixelFormat)pixelFormat, &vkf, &bpp, &a8)) return MTLSizeMake(0, 0, 0);
    // the tile NVK makes for this page size; a 256 KB tile is 2x2 of the 64 KB one (Apple's shapes, measured)
    const int prev = nvmtl_vk_sparse_mode(nvmtl4_sparse_mode_for(pageSize));
    if (prev < 0 && pageSize == MTLSparsePageSize16) return MTLSizeMake(0, 0, 0);
    const int bad = nvmtl_vk_sparse_format_tile(vkf, (uint32_t)textureType, &tw, &th, &td, NULL);
    if (prev >= 0) nvmtl_vk_sparse_mode(prev);
    if (bad) return MTLSizeMake(0, 0, 0);
    const NSUInteger k = pageSize == MTLSparsePageSize256 ? 2 : 1;
    return MTLSizeMake(tw * k, th * k, textureType == MTLTextureType3D ? td : 1);
}

@implementation NVMTLDevice (MTL4Sparse)
- (NSUInteger)sparseTileSizeInBytes { return nvmtl_vk_sparse_textures() ? NVMTL4_SPARSE_BLOCK : 0; }
- (NSUInteger)sparseTileSizeInBytesForSparsePageSize:(MTLSparsePageSize)pageSize { return nvmtl_vk_sparse_textures() ? nvmtl4_page_bytes(pageSize) : 0; }
@end

#pragma clang diagnostic pop
