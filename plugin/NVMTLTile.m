/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Tile shading: MTLTileRenderPipelineDescriptor pipelines, the render encoder's tile bindings and
 * dispatchThreadsPerTile, tile and imageblock sizes.
 *
 * There is no tile memory on this hardware, so a tile function runs as a compute dispatch between two parts of a render
 * pass, one workgroup per tile:
 *
 *   1. the Vulkan render pass is ended (a pass that has not begun first applies its Clear actions);
 *   2. each colour attachment the function reads is copied into a storage image padded to whole tiles, the plane,
 *      unless the plane already holds the newest contents, and bound where the translated function expects its
 *      imageblock (translator tile_mode: the top of the storage-image band, binding 576 + 3 * attachment);
 *   3. the tile bindings (buffers, bytes, textures, samplers, threadgroup memory) are replayed into a compute encoder
 *      that exists only for this dispatch, over a saved copy of the vertex-stage binding table so the draws around the
 *      tile function keep the bindings they made;
 *   4. one workgroup per tile is dispatched, with the tile size pushed after the dispatch region;
 *   5. planes the function wrote are copied back before the next draw begins a pass, or before the encoder ends.
 *
 * The next draw sets a render pipeline, as Metal requires after a tile pipeline, and that begins a new Vulkan pass with
 * every attachment loaded.
 */

#pragma mark - storage planes

// Reflection JSON, read defensively.
static NSString *nvmtl_tile_str(NSDictionary *d, NSString *k) { id v = d[k]; return [v isKindOfClass:[NSString class]] ? v : @""; }
static NSUInteger nvmtl_tile_num(NSDictionary *d, NSString *k) { id v = d[k]; return [v isKindOfClass:[NSNumber class]] ? [v unsignedIntegerValue] : 0; }

#define NVMTL_TILE_STEX_BASE 96u    /* storage-image index of the first plane: binding 576 of the band that starts at 480 */
#define NVMTL_TILE_STEX_PLANES 24u
#define NVMTL_TILE_EXPLICIT_BASE 120u   /* storage-image index of explicit-imageblock member 0: binding 600 */
#define NVMTL_TILE_EXPLICIT_MAX 8u
#define NVMTL_TILE_FLAG_KERNEL 4u   /* translator tile_mode::TILE_KERNEL */
#define NVMTL_TILE_FLAG_PERSISTENT 8u   /* translator tile_mode::PERSISTENT_TG */
#define NVMTL_TILE_MEMORY_SLOT 28u  /* Metal buffer slot a persistent tile function reads its tile memory through (tile_mode::TILE_MEMORY_BUFFER_SLOT) */
#define NVMTL_TILE_BUDGET 32768u    /* bytes of tile memory Apple GPUs give a tile (measured: 32x32 RGBA8, 32x16 for 4 x RGBA32F) */

// A storage image the size of the render area rounded up to whole tiles. A tile function addresses every pixel of every
// tile it runs, including the part of an edge tile that lies outside the render target, and an image of exactly the
// target's size would be read and written out of bounds there.
@interface NVMTLTileImage : NSObject { @public nvk_image _i; uint32_t _w, _h; }
@end
@implementation NVMTLTileImage
- (void)dealloc { nvmtl_vk_image_destroy(&_i); }
@end

static NSMutableArray<NVMTLTileImage *> *g_tile_pool;
static pthread_mutex_t g_tile_pool_lock = PTHREAD_MUTEX_INITIALIZER;
#define NVMTL_TILE_POOL_MAX 8

static NVMTLTileImage *nvmtl_tile_image_take(NVMTLCommandBuffer *cb, uint32_t w, uint32_t h, uint32_t vkfmt, uint32_t bpp) {
    NVMTLTileImage *img = nil;
    pthread_mutex_lock(&g_tile_pool_lock);
    for (NSUInteger k = 0; k < g_tile_pool.count; k++) {
        NVMTLTileImage *c = g_tile_pool[k];
        if (c->_w == w && c->_h == h && c->_i.fmt == vkfmt) { img = c; [g_tile_pool removeObjectAtIndex:k]; break; }
    }
    pthread_mutex_unlock(&g_tile_pool_lock);
    if (!img) {
        img = [NVMTLTileImage new]; img->_w = w; img->_h = h;
        if (nvmtl_vk_tile_plane_create(w, h, vkfmt, bpp, &img->_i)) {
            nvlog("tile: a %ux%u storage plane of VkFormat %u cannot be made - FAILED", w, h, vkfmt);
            return nil;
        }
    }
    // zero: the part outside the render target is never copied in, and a pooled image holds another pass's pixels
    const float zero[4] = { 0, 0, 0, 0 };
    if (nvmtl_vk_cmd_clear_image(&cb->_c, &img->_i, zero)) { nvlog("tile: clearing a storage plane FAILED"); return nil; }
    // the command buffer owns the plane until the GPU is done, then the pool takes it back
    [cb addCompletedHandler:^(id<MTLCommandBuffer> done) {
        pthread_mutex_lock(&g_tile_pool_lock);
        if (!g_tile_pool) g_tile_pool = [NSMutableArray new];
        if (g_tile_pool.count < NVMTL_TILE_POOL_MAX) [g_tile_pool addObject:img];
        pthread_mutex_unlock(&g_tile_pool_lock);
    }];
    return img;
}

// The plane format that stands in for an attachment of VkFormat `vk`: its own when storage-capable, else the nearest one
// that is, filled and drained by blits that convert. Returns -1 when there is none.
static int nvmtl_tile_plane_format(uint32_t vk, uint32_t bpp, uint32_t *pfmt, uint32_t *pbpp, BOOL *blit) {
    if (nvmtl_vk_srgb_format(vk)) { *pfmt = VK_FORMAT_R16G16B16A16_SFLOAT; *pbpp = 8; *blit = YES; return 0; }
    if (nvmtl_vk_format_is_storage(vk)) { *pfmt = vk; *pbpp = bpp; *blit = NO; return 0; }
    if (vk == VK_FORMAT_B8G8R8A8_UNORM) { *pfmt = VK_FORMAT_R8G8B8A8_UNORM; *pbpp = 4; *blit = YES; return 0; }
    return -1;
}

// The storage image a member of an explicit imageblock structure lives in, by the Metal type name the translator
// (tile_mode, passes::fragment_imageblock_format) fixes: the fragment shaders' own accesses and a tile function's staging
// must agree on it.
typedef struct { const char *name; uint32_t vk, bytes, align; } nvmtl_tile_member_t;
static const nvmtl_tile_member_t *nvmtl_tile_member(NSString *name) {
    static const nvmtl_tile_member_t table[] = {
        { "half", VK_FORMAT_R16_SFLOAT, 2, 2 }, { "half4", VK_FORMAT_R16G16B16A16_SFLOAT, 8, 8 },
        { "float", VK_FORMAT_R32_SFLOAT, 4, 4 }, { "float4", VK_FORMAT_R32G32B32A32_SFLOAT, 16, 16 },
        { "uint", VK_FORMAT_R32_UINT, 4, 4 }, { "uint4", VK_FORMAT_R32G32B32A32_UINT, 16, 16 },
        { "int", VK_FORMAT_R32_SINT, 4, 4 }, { "int4", VK_FORMAT_R32G32B32A32_SINT, 16, 16 },
        { "uchar", VK_FORMAT_R8_UINT, 1, 1 }, { "uchar4", VK_FORMAT_R8G8B8A8_UINT, 4, 4 },
        { "ushort", VK_FORMAT_R16_UINT, 2, 2 },
    };
    for (unsigned i = 0; i < sizeof table / sizeof *table; i++) if (!strcmp(table[i].name, name.UTF8String)) return &table[i];
    return NULL;
}
// The Metal type name of a reflected AirType ({"Vec":{"scalar":"Half","lanes":4}} or {"Scalar":"Half"}), or nil.
static NSString *nvmtl_tile_air_type_name(id ty) {
    static NSDictionary *scalar;
    if (!scalar) scalar = @{ @"Half": @"half", @"Float": @"float", @"UInt": @"uint", @"SInt": @"int", @"UChar": @"uchar", @"UShort": @"ushort" };
    if (![ty isKindOfClass:[NSDictionary class]]) return nil;
    id s = ty[@"Scalar"], v = ty[@"Vec"];
    if ([s isKindOfClass:[NSString class]]) return scalar[s];
    if ([v isKindOfClass:[NSDictionary class]]) {
        NSString *b = scalar[v[@"scalar"]]; NSUInteger lanes = nvmtl_tile_num(v, @"lanes");
        return b && lanes == 4 ? [b stringByAppendingString:@"4"] : nil;
    }
    return nil;
}
// Member type names, in order, of an explicit imageblock layout from reflection; nil when a member has no plane format.
static NSArray<NSString *> *nvmtl_tile_layout_names(NSDictionary *layout, NSUInteger *size) {
    id st = layout[@"type_layout"][@"Struct"];
    if (![st isKindOfClass:[NSArray class]]) return nil;
    NSMutableArray *names = [NSMutableArray new]; NSUInteger end = 0, align = 1;
    for (NSDictionary *m in st) {
        NSString *n = nvmtl_tile_air_type_name(m[@"ty"]);
        const nvmtl_tile_member_t *f = n ? nvmtl_tile_member(n) : NULL;
        if (!f) return nil;
        [names addObject:n]; end = MAX(end, nvmtl_tile_num(m, @"offset") + f->bytes); align = MAX(align, (NSUInteger)f->align);
    }
    if (size) *size = (end + align - 1) / align * align;
    return names.count && names.count <= NVMTL_TILE_EXPLICIT_MAX ? names : nil;
}

#pragma mark - tile pipeline state

@interface NVMTLTileRenderPipelineState : NVMTLRenderPipelineState {
@public
    NVMTLFunction *_tileFn;
    MTLTileRenderPipelineDescriptor *_tdesc;
    NVMTLComputePipelineState *_baseCPS;         // the tile function as a compute pipeline, translated as a tile function
    uint32_t _natt, _attVk[NVMTL_NCOL], _attBpp[NVMTL_NCOL];
    NSArray<NSDictionary *> *_implicit;          // reflection: implicit_imageblock_attachments
    NSUInteger _sampleBytes;
    BOOL _matches;
    NSArray<NSString *> *_explicitNames;         // member type names of the explicit imageblock the function stages, or nil
    NSMutableDictionary<NSString *, NVMTLComputePipelineState *> *_cpsBySig;   // persistent tile memory: one pipeline per layout of the arguments
}
- (BOOL)nvmtlIsTilePipeline;
@end

#pragma mark - encoder state

@interface NVMTLTileState : NSObject {
@public
    NVMTLTileRenderPipelineState *_ps;
    NSMutableDictionary<NSNumber *, id> *_bufs;     // index -> @[buffer, offset] or NSData (bytes)
    NSMutableDictionary<NSNumber *, id> *_texs;
    NSMutableDictionary<NSNumber *, id> *_samps;    // index -> @[sampler, lodMin, lodMax]
    NSMutableDictionary<NSNumber *, id> *_tgs;      // index -> @[length, offset]
    NVMTLTileImage *_plane[NVMTL_NCOL];
    NVMTLBuffer *_tilemem;                               // persistent tile memory: threadgroupMemoryLength bytes per tile
    NVMTLTileImage *_eplane[NVMTL_TILE_EXPLICIT_MAX];   // explicit imageblock members, one storage image each
    NSString *_ename[NVMTL_TILE_EXPLICIT_MAX];
    BOOL _stale[NVMTL_NCOL];                        // the attachment changed since the plane was filled
    BOOL _dirty[NVMTL_NCOL];                        // the plane holds writes the attachment does not
    BOOL _blit[NVMTL_NCOL];                         // the plane is a stand-in format: copies convert
    BOOL _loadConsumed;                             // the pass's Clear/Load actions have been carried out
    uint32_t _tw, _th;
    NSMutableSet<NSString *> *_said;
}
@end
@implementation NVMTLTileState
@end

static char kNVMTLTileState;
static NVMTLTileState *nvmtl_tile_state(NVMTLRenderCommandEncoder *e, BOOL create) {
    NVMTLTileState *s = objc_getAssociatedObject(e, &kNVMTLTileState);
    if (!s && create) {
        s = [NVMTLTileState new];
        s->_bufs = [NSMutableDictionary new]; s->_texs = [NSMutableDictionary new];
        s->_samps = [NSMutableDictionary new]; s->_tgs = [NSMutableDictionary new];
        s->_said = [NSMutableSet new];
        for (int i = 0; i < NVMTL_NCOL; i++) s->_stale[i] = YES;
        objc_setAssociatedObject(e, &kNVMTLTileState, s, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    }
    return s;
}
static BOOL nvmtl_tile_say_once(NVMTLTileState *s, NSString *what) {
    if ([s->_said containsObject:what]) return NO;
    [s->_said addObject:what]; return YES;
}

#pragma mark - sizes

// The tile size of a pass: what its descriptor asked for, else the largest of Apple's sizes whose imageblock fits the
// tile memory.
static void nvmtl_tile_size(NVMTLRenderCommandEncoder *e, uint32_t *w, uint32_t *h) {
    if (e->_pass.tile_w && e->_pass.tile_h) { *w = e->_pass.tile_w; *h = e->_pass.tile_h; return; }
    uint32_t bytes = 0;
    for (int i = 0; i < NVMTL_NCOL; i++) if (e->_pass.col[i]) bytes += e->_pass.col[i]->bpp ? e->_pass.col[i]->bpp : 4u;
    if (e->_pass.tile_ib_sample > bytes) bytes = e->_pass.tile_ib_sample;
    if (!bytes) bytes = 4;
    static const uint32_t sizes[][2] = { { 32, 32 }, { 32, 16 }, { 16, 16 }, { 16, 8 }, { 8, 8 } };
    for (unsigned k = 0; k < sizeof sizes / sizeof *sizes; k++)
        if (sizes[k][0] * sizes[k][1] * bytes <= NVMTL_TILE_BUDGET) { *w = sizes[k][0]; *h = sizes[k][1]; return; }
    *w = 8; *h = 8;
}
NSUInteger nvmtl_tile_dim(NVMTLRenderCommandEncoder *e, int axis) {
    uint32_t w, h; nvmtl_tile_size(e, &w, &h);
    return axis ? h : w;
}
void nvmtl_tile_note_pass(NVMTLRenderCommandEncoder *e, MTLRenderPassDescriptor *d) {
    e->_pass.tile_w = (uint32_t)d.tileWidth; e->_pass.tile_h = (uint32_t)d.tileHeight;
    e->_pass.tile_tg_bytes = (uint32_t)d.threadgroupMemoryLength; e->_pass.tile_ib_sample = (uint32_t)d.imageblockSampleLength;
}

#pragma mark - translator scope

// While a tile function is translated or its pipelines are built, the translator is told it is one and the pipeline
// layouts carry the tile block.
typedef struct { uint32_t was; } nvmtl_tile_scope_t;
static nvmtl_tile_scope_t nvmtl_tile_scope_begin(uint32_t flags) {
    nvmtl_tile_scope_t s = { g_tile_xlate_flags };
    g_tile_xlate_flags = flags; if (xlate_tile_flags) xlate_tile_flags(flags);
    nvmtl_vk_tile_scope(1);
    return s;
}
static void nvmtl_tile_scope_end(nvmtl_tile_scope_t *s) {
    nvmtl_vk_tile_scope(0);
    g_tile_xlate_flags = s->was; if (xlate_tile_flags) xlate_tile_flags(s->was);
    memset(g_tile_xlate_offsets, 0, sizeof g_tile_xlate_offsets); if (xlate_tile_tg_offsets) xlate_tile_tg_offsets(g_tile_xlate_offsets, 32);
}
#define NVMTL_TILE_SCOPE(flags) nvmtl_tile_scope_t nvmtl_tile_scope_var __attribute__((cleanup(nvmtl_tile_scope_end))) = nvmtl_tile_scope_begin(flags)
// byte offsets of the threadgroup arguments in tile memory, for the translations made inside the scope
static void nvmtl_tile_scope_offsets(const uint32_t offsets[32]) {
    memcpy(g_tile_xlate_offsets, offsets, sizeof g_tile_xlate_offsets);
    if (xlate_tile_tg_offsets) xlate_tile_tg_offsets(g_tile_xlate_offsets, 32);
}

// The tile function as a compute pipeline translated as a tile function. Call inside a scope.
static NVMTLComputePipelineState *nvmtl_tile_make_cps(NVMTLFunction *f, NSString **why) {
    NVMTLComputePipelineState *cps = [NVMTLComputePipelineState new];
    cps->_function = f; cps->_attrT = nvmtl_air_max_tg(f);
    // an empty-but-present threadgroup table makes the variant path translate afresh instead of returning the function's
    // compute SPIR-V; index 31 with length 0 sizes nothing
    NSData *spv = nvmtl_translate_variant(f, nil, @{ @"31": @0 }, why);
    if (!spv.length) return nil;
    if (nvmtl_vk_tile_pipeline_create(spv.bytes, spv.length, &cps->_p)) { if (why) *why = @"vkCreateComputePipelines FAILED"; return nil; }
    return cps;
}

#pragma mark - pipeline creation


id<MTLRenderPipelineState> nvmtl_tile_pipeline_create(MTLTileRenderPipelineDescriptor *desc, NSError **err) {
    if (err) *err = nil;
    if (!desc) return nvmtl_fail(err, @"tile pipeline: no descriptor - REFUSED");
    if (nvmtl_vk_init()) return nvmtl_fail(err, @"tile pipeline: Vulkan is not up in this process");
    if (!xlate_tile_flags) return nvmtl_fail(err, @"tile pipeline: this translator has no tile mode - unsupported");
    id<MTLFunction> fn = desc.tileFunction;
    if (!fn || ![(id)fn isKindOfClass:[NVMTLFunction class]])
        return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: REFUSED - the tile function is not ours (%s)", fn ? object_getClassName(fn) : "nil"]);
    NVMTLFunction *f = (NVMTLFunction *)fn;
    if (f->_needsLink) { NSError *le = nil; id<MTLFunction> lk = nvmtl_link_stage(fn, desc.linkedFunctions, desc.preloadedLibraries, &le); if (!lk) { if (err) *err = le; return nil; } f = (NVMTLFunction *)lk; }
    if (f.functionType != MTLFunctionTypeKernel)
        return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: \"%@\" is a %@ function; a fragment-function tile shader is unsupported",
                                f->_fname, f->_stage.length ? f->_stage : @"non-kernel"]);
    if (!f->_air) return nvmtl_fail(err, @"tile pipeline: the tile function kept no AIR to translate as a tile function - unsupported");

    NVMTLTileRenderPipelineState *ps = [NVMTLTileRenderPipelineState new];
    ps->_tileFn = f; ps->_tdesc = [desc copy]; ps->_matches = desc.threadgroupSizeMatchesTileSize;
    uint32_t bytes = 0;
    for (uint32_t i = 0; i < NVMTL_NCOL; i++) {
        MTLPixelFormat pf = desc.colorAttachments[i].pixelFormat;
        if (pf == MTLPixelFormatInvalid) continue;
        uint32_t vk = 0, bpp = 0; int a8 = 0;
        if (nvmtl_pixfmt(pf, &vk, &bpp, &a8) || a8)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: colour attachment %u pixel format %lu is unsupported as a tile attachment", i, (unsigned long)pf]);
        ps->_attVk[i] = vk; ps->_attBpp[i] = bpp; ps->_natt = i + 1; bytes += bpp;
    }
    if (desc.rasterSampleCount > 1)
        return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: rasterSampleCount %lu - a multisampled imageblock is unsupported", (unsigned long)desc.rasterSampleCount]);

    NSDictionary *refl = nvmtl_function_reflection(f);
    ps->_implicit = [refl[@"implicit_imageblock_attachments"] isKindOfClass:[NSArray class]] ? refl[@"implicit_imageblock_attachments"] : @[];
    for (NSDictionary *a in ps->_implicit) {
        if (nvmtl_tile_num(a, @"data_rate") != 0)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: \"%@\" reads imageblock attachment %lu at data rate %lu; a per-sample imageblock is unsupported",
                                    f->_fname, (unsigned long)nvmtl_tile_num(a, @"attachment"), (unsigned long)nvmtl_tile_num(a, @"data_rate")]);
        NSUInteger att = nvmtl_tile_num(a, @"attachment");
        if (att >= NVMTL_NCOL || ps->_attVk[att] == 0)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: \"%@\" uses imageblock attachment %lu and the descriptor declares no colour attachment there", f->_fname, (unsigned long)att]);
    }
    // An explicit-layout imageblock: its structure is kept in storage images beside the attachments, one per member
    if ([f->_air containsString:@"!\"air.imageblock\", !\"explicit\""]) {
        NSArray *layouts = refl[@"imageblock_layouts"]; NSUInteger sz = 0;
        NSArray<NSString *> *names = [layouts isKindOfClass:[NSArray class]] && layouts.count ? nvmtl_tile_layout_names(layouts[0], &sz) : nil;
        if (!names)
            return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: \"%@\" has an explicit imageblock with a member type, or more than %u members, that is unsupported", f->_fname, NVMTL_TILE_EXPLICIT_MAX]);
        ps->_explicitNames = names;
        bytes = (uint32_t)(((NSUInteger)bytes + sz + 7) & ~(NSUInteger)7);
    }
    ps->_sampleBytes = bytes;

    // the tile function as a compute pipeline, translated as a tile function: planes at the band, tile origin added
    NVMTLComputePipelineState *cps = nil;
    {
        NVMTL_TILE_SCOPE(NVMTL_TILE_FLAG_KERNEL);
        NSString *why = nil;
        cps = nvmtl_tile_make_cps(f, &why);
        if (!cps) return nvmtl_fail(err, [NSString stringWithFormat:@"tile pipeline: \"%@\" did not translate as a tile function: %@", f->_fname, why ?: @"?"]);
    }
    ps->_baseCPS = cps;
    ps->_cpsBySig = [NSMutableDictionary new];
    nvlog("tile pipeline: \"%s\" %u attachment(s), %lu implicit plane(s) -> %p", f->_fname.UTF8String, ps->_natt, (unsigned long)ps->_implicit.count, (__bridge void *)ps);
    return ps;
}

// Bytes one sample of the imageblock takes for a pipeline: the colour attachments it declares, plus the explicit
// imageblock structure its fragment function keeps, rounded to that structure's 8-byte alignment (Apple answers 24 for a
// 16-byte structure beside one RGBA8 attachment, and 4 for the attachment alone).
NSUInteger nvmtl_tile_sample_bytes(NVMTLRenderPipelineState *p) {
    if ([p isKindOfClass:[NVMTLTileRenderPipelineState class]]) return ((NVMTLTileRenderPipelineState *)p)->_sampleBytes;
    MTLRenderPipelineDescriptor *d = p->_descriptor;
    if (!d) return 0;
    NSUInteger bytes = 0, block = 0;
    for (NSUInteger i = 0; i < NVMTL_NCOL; i++) {
        MTLPixelFormat pf = d.colorAttachments[i].pixelFormat;
        uint32_t vk = 0, bpp = 0; int a8 = 0;
        if (pf != MTLPixelFormatInvalid && !nvmtl_pixfmt(pf, &vk, &bpp, &a8)) bytes += bpp;
    }
    if (p->_linkedFrag) {
        id fb = nvmtl_function_reflection(p->_linkedFrag)[@"fragment_imageblock"];
        if ([fb isKindOfClass:[NSDictionary class]]) block = nvmtl_tile_num(fb, @"sample_size");
    }
    if (block) bytes = (bytes + block + 7) & ~(NSUInteger)7;
    return bytes;
}

@implementation NVMTLTileRenderPipelineState
- (BOOL)nvmtlIsTilePipeline { return YES; }
- (NSString *)label { return _tdesc.label; }
- (BOOL)supportIndirectCommandBuffers { return NO; }
- (MTLResourceID)gpuResourceID { MTLResourceID r; r._impl = 0; return r; }
- (BOOL)threadgroupSizeMatchesTileSize { return _matches; }
- (NSUInteger)maxTotalThreadsPerThreadgroup { NSUInteger m = _tdesc.maxTotalThreadsPerThreadgroup; return m ? m : 1024; }
- (NSUInteger)imageblockSampleLength { return _sampleBytes; }
- (NSUInteger)imageblockMemoryLengthForDimensions:(MTLSize)d { return d.width * d.height * _sampleBytes; }
- (MTLSize)requiredThreadsPerTileThreadgroup {
    if (@available(macos 26.0, *)) return _tdesc.requiredThreadsPerThreadgroup;
    return MTLSizeMake(0, 0, 0);
}
- (nullable MTLTileRenderPipelineDescriptor *)tileDescriptor { return _tdesc; }
@end

@implementation NVMTLDevice (NVMTLTile)
- (id<MTLRenderPipelineState>)newRenderPipelineStateWithTileDescriptor:(MTLTileRenderPipelineDescriptor *)d options:(MTLPipelineOption)o
                                                            reflection:(MTLAutoreleasedRenderPipelineReflection *)r error:(NSError **)err {
    id<MTLRenderPipelineState> ps = nvmtl_tile_pipeline_create(d, err);
    if (r) {
        *r = nil;
        if (ps && (o & (MTLPipelineOptionArgumentInfo | MTLPipelineOptionBufferTypeInfo))) {
            NVMTLRenderPipelineReflection *refl = [NVMTLRenderPipelineReflection new];
            refl->_t = nvmtl_arguments_for_function(((NVMTLTileRenderPipelineState *)ps)->_tileFn);
            *r = (MTLRenderPipelineReflection *)refl;
        }
    }
    return ps;
}
- (void)newRenderPipelineStateWithTileDescriptor:(MTLTileRenderPipelineDescriptor *)d options:(MTLPipelineOption)o
                               completionHandler:(MTLNewRenderPipelineStateWithReflectionCompletionHandler)h {
    MTLTileRenderPipelineDescriptor *snapshot = [d copy];
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        NSError *e = nil; MTLAutoreleasedRenderPipelineReflection refl = nil;
        id<MTLRenderPipelineState> ps = [self newRenderPipelineStateWithTileDescriptor:snapshot options:o reflection:&refl error:&e];
        h(ps, refl, e);
    });
}
@end

#pragma mark - encoder

@implementation NVMTLRenderCommandEncoder (NVMTLTile)

- (BOOL)nvmtlTileInterceptPipeline:(id<MTLRenderPipelineState>)ps {
    // every draw's pipeline passes here: one pointer compare
    static Class tileClass; if (!tileClass) tileClass = [NVMTLTileRenderPipelineState class];
    if (object_getClass(ps) != tileClass) return NO;
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    ts->_ps = (NVMTLTileRenderPipelineState *)ps;
    nvmtl_retain_resource(_cb, ps);
    return YES;
}

// Carry the pass's own actions out, once, and leave the Vulkan pass if one is open.
- (void)nvmtlTileLeavePass:(NVMTLTileState *)ts {
    if (_begun) {
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO;
        for (int i = 0; i < NVMTL_NCOL; i++) ts->_stale[i] = YES;   // the draws since the last copy changed the attachments
    } else if (!ts->_loadConsumed) {
        // nothing has begun the pass: its Clear actions still have to happen, once, before the tile function reads
        switch (nvmtl_vk_pass_kind(&_pass)) {
        case 1: break;
        case 2:
            if (nvmtl_vk_cmd_empty_pass(&_cb->_c, &_pass)) nvlog("tile: the pass's clears FAILED - the tile function reads unspecified contents");
            break;
        default:
            if (!(_load & 1u) && _target && nvmtl_vk_cmd_clear_image(&_cb->_c, [_target nvi], _clear)) nvlog("tile: the pass's clear FAILED");
        }
        for (int i = 0; i < NVMTL_NCOL; i++) ts->_stale[i] = YES;
    }
    if (!ts->_loadConsumed) {
        ts->_loadConsumed = YES;
        _pass.load = _load = NVMTL_LOAD_ALL;   // draws that follow add to what is there
    }
}

// Copy planes the tile function wrote back into their attachments, so that a pass can begin or the encoder end.
- (void)nvmtlTileFlush:(NVMTLTileState *)ts {
    for (int a = 0; a < NVMTL_NCOL; a++) {
        if (!ts->_dirty[a]) continue;
        ts->_dirty[a] = NO;
        nvk_image *att = _pass.col[a]; NVMTLTileImage *pl = ts->_plane[a];
        if (!att || !pl) continue;
        const uint32_t w = MIN(_pass.colw[a], _pass.w), h = MIN(_pass.colh[a], _pass.h);
        int rc = ts->_blit[a] ? (_pass.col_level[a] || _pass.col_layer[a] ? -1 : nvmtl_vk_cmd_blit_image_2d(&_cb->_c, &pl->_i, att, w, h))
                              : nvmtl_vk_cmd_copy_image_sub(&_cb->_c, &pl->_i, 0, 0, 0, 0, att, _pass.col_level[a], _pass.col_layer[a], 0, 0, w, h);
        if (rc) { nvlog("tile: copying the tile function's writes back into colour attachment %d FAILED - they are lost", a); continue; }
        nvmtl_vk_tile_image_rest(&_cb->_c, att);
    }
}

// Storage images for the members of an explicit imageblock, made once for the pass and shared by the fragment shaders and
// the tile functions that name them. They are made outside any Vulkan pass: their layout is GENERAL for good.
- (BOOL)nvmtlTileEnsureExplicit:(NVMTLTileState *)ts names:(NSArray<NSString *> *)names {
    if (!ts->_tw) nvmtl_tile_size(self, &ts->_tw, &ts->_th);
    const uint32_t pw = (_pass.w + ts->_tw - 1) / ts->_tw * ts->_tw, ph = (_pass.h + ts->_th - 1) / ts->_th * ts->_th;
    for (NSUInteger n = 0; n < names.count && n < NVMTL_TILE_EXPLICIT_MAX; n++) {
        if (ts->_eplane[n]) {
            if (![ts->_ename[n] isEqualToString:names[n]]) {
                nvlog("tile: imageblock member %lu is %s in one function and %s in another of this pass - REFUSED", (unsigned long)n, ts->_ename[n].UTF8String, names[n].UTF8String);
                return NO;
            }
            continue;
        }
        const nvmtl_tile_member_t *f = nvmtl_tile_member(names[n]);
        if (!f) { nvlog("tile: imageblock member %lu has type %s, which has no storage-image plane - unsupported", (unsigned long)n, names[n].UTF8String); return NO; }
        NVMTLTileImage *img = nvmtl_tile_image_take(_cb, pw, ph, f->vk, f->bytes);
        if (!img) return NO;
        if (nvmtl_vk_cmd_prepare_sampled_image(&_cb->_c, &img->_i, 1)) { nvlog("tile: an imageblock plane could not be made ready - FAILED"); return NO; }
        ts->_eplane[n] = img; ts->_ename[n] = names[n];
    }
    return YES;
}
- (void)nvmtlTileBindExplicit:(NVMTLTileState *)ts set:(uint32_t)set count:(NSUInteger)count {
    for (NSUInteger n = 0; n < count && n < NVMTL_TILE_EXPLICIT_MAX; n++)
        if (ts->_eplane[n] && nvmtl_vk_bind_storage_view(&_cb->_c, set, NVMTL_TILE_EXPLICIT_BASE + (uint32_t)n, ts->_eplane[n]->_i.view))
            nvlog("tile: binding imageblock member %lu FAILED", (unsigned long)n);
}

// A draw pipeline whose fragment function reads or writes an imageblock: the planes must exist, and be bound, before
// the draw. A pass that has begun without them is ended and resumed, because a plane takes its layout outside a pass.
- (void)nvmtlTileDrawPipeline:(NVMTLRenderPipelineState *)p {
    if (!p->_p.ib_mask) return;
    NSDictionary *fb = p->_linkedFrag ? nvmtl_function_reflection(p->_linkedFrag)[@"fragment_imageblock"] : nil;
    NSArray *members = [fb isKindOfClass:[NSDictionary class]] ? fb[@"members"] : nil;
    if (![members isKindOfClass:[NSArray class]] || !members.count || members.count > NVMTL_TILE_EXPLICIT_MAX) {
        nvlog("tile: a fragment function uses an imageblock with %lu member(s); at most %u are supported", (unsigned long)members.count, NVMTL_TILE_EXPLICIT_MAX); return; }
    NSMutableArray<NSString *> *names = [NSMutableArray new];
    for (NSDictionary *m in members) [names addObject:nvmtl_tile_str(m, @"type_name")];
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    BOOL missing = NO;
    for (NSUInteger n = 0; n < names.count; n++) if (!ts->_eplane[n]) missing = YES;
    if (missing) {
        if (_begun) [self nvmtlTileLeavePass:ts];
        if (![self nvmtlTileEnsureExplicit:ts names:names]) return;
    }
    [self nvmtlTileBindExplicit:ts set:1 count:names.count];
}

- (void)nvmtlTileBeforePassBegins {
    NVMTLTileState *ts = nvmtl_tile_state(self, NO);
    if (ts) [self nvmtlTileFlush:ts];
}
- (void)nvmtlTileEndEncoding {
    NVMTLTileState *ts = nvmtl_tile_state(self, NO);
    if (!ts) return;
    [self nvmtlTileFlush:ts];
    objc_setAssociatedObject(self, &kNVMTLTileState, nil, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}

#pragma mark bindings

- (void)setTileBytes:(const void *)bytes length:(NSUInteger)length atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (!bytes || !length) { [ts->_bufs removeObjectForKey:@(i)]; return; }
    ts->_bufs[@(i)] = [NSData dataWithBytes:bytes length:length];
}
- (void)setTileBuffer:(id<MTLBuffer>)b offset:(NSUInteger)o atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (!b) { [ts->_bufs removeObjectForKey:@(i)]; return; }
    ts->_bufs[@(i)] = @[b, @(o)];
}
- (void)setTileBufferOffset:(NSUInteger)o atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    NSArray *was = ts->_bufs[@(i)];
    if (![was isKindOfClass:[NSArray class]]) { nvlog("tile: setTileBufferOffset at index %lu with no buffer bound there - ignored", (unsigned long)i); return; }
    ts->_bufs[@(i)] = @[was[0], @(o)];
}
- (void)setTileBuffers:(const id<MTLBuffer> __unsafe_unretained [])b offsets:(const NSUInteger *)o withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileBuffer:b[k] offset:o[k] atIndex:r.location + k];
}
- (void)setTileTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (t) ts->_texs[@(i)] = t; else [ts->_texs removeObjectForKey:@(i)];
}
- (void)setTileTextures:(const id<MTLTexture> __unsafe_unretained [])t withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileTexture:t[k] atIndex:r.location + k];
}
- (void)setTileSamplerState:(id<MTLSamplerState>)s lodMinClamp:(float)lo lodMaxClamp:(float)hi atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (s) ts->_samps[@(i)] = @[s, @(lo), @(hi)]; else [ts->_samps removeObjectForKey:@(i)];
}
- (void)setTileSamplerState:(id<MTLSamplerState>)s atIndex:(NSUInteger)i { [self setTileSamplerState:s lodMinClamp:0 lodMaxClamp:FLT_MAX atIndex:i]; }
- (void)setTileSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileSamplerState:s[k] atIndex:r.location + k];
}
- (void)setTileSamplerStates:(const id<MTLSamplerState> __unsafe_unretained [])s lodMinClamps:(const float *)lo lodMaxClamps:(const float *)hi withRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileSamplerState:s[k] lodMinClamp:lo[k] lodMaxClamp:hi[k] atIndex:r.location + k];
}
- (void)setTileAccelerationStructure:(id)a atBufferIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (nvmtl_tile_say_once(ts, @"accel")) nvlog("tile: setTileAccelerationStructure is unsupported - a tile function that traces rays reads an empty structure");
}
- (void)setTileVisibleFunctionTable:(id)t atBufferIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (nvmtl_tile_say_once(ts, @"vft")) nvlog("tile: setTileVisibleFunctionTable is unsupported - a tile function that calls through a table has nothing to call");
}
- (void)setTileVisibleFunctionTables:(const id __unsafe_unretained [])t withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileVisibleFunctionTable:t[k] atBufferIndex:r.location + k];
}
- (void)setTileIntersectionFunctionTable:(id)t atBufferIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (nvmtl_tile_say_once(ts, @"ift")) nvlog("tile: setTileIntersectionFunctionTable is unsupported - a tile function that intersects through a table has nothing to call");
}
- (void)setTileIntersectionFunctionTables:(const id __unsafe_unretained [])t withBufferRange:(NSRange)r {
    for (NSUInteger k = 0; k < r.length; k++) [self setTileIntersectionFunctionTable:t[k] atBufferIndex:r.location + k];
}
- (void)setThreadgroupMemoryLength:(NSUInteger)length offset:(NSUInteger)offset atIndex:(NSUInteger)i {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    if (length) ts->_tgs[@(i)] = @[@(length), @(offset)]; else [ts->_tgs removeObjectForKey:@(i)];
}

#pragma mark dispatch

- (BOOL)nvmtlTilePlanes:(NVMTLTileState *)ts tiles:(uint32_t[2])tiles {
    const uint32_t pw = tiles[0] * ts->_tw, ph = tiles[1] * ts->_th;
    NVMTLTileRenderPipelineState *tp = ts->_ps;
    for (NSDictionary *a in tp->_implicit) {
        const uint32_t att = (uint32_t)nvmtl_tile_num(a, @"attachment");
        const uint32_t binding = (uint32_t)nvmtl_tile_num(a, @"binding");
        nvk_image *im = _pass.col[att];
        if (!im || !_pass.colview[att]) {
            nvlog("tile: \"%s\" uses attachment %u and the pass has no colour texture there - REFUSED", tp->_tileFn->_fname.UTF8String, att);
            return NO;
        }
        if (binding < 480 + NVMTL_TILE_STEX_BASE || binding >= 480 + NVMTL_TILE_STEX_BASE + NVMTL_TILE_STEX_PLANES) {
            nvlog("tile: \"%s\" attachment %u reflects binding %u, outside the plane band - unsupported (translator without tile mode?)", tp->_tileFn->_fname.UTF8String, att, binding);
            return NO;
        }
        if (im->samples > 1) { nvlog("tile: attachment %u is multisampled - a multisampled imageblock is unsupported", att); return NO; }
        if (im->mtl_type == 7) { nvlog("tile: attachment %u is a slice of a 3D texture - unsupported", att); return NO; }
        if (_pass.colfmt[att] && _pass.colfmt[att] != im->fmt) {
            nvlog("tile: attachment %u is a %u view of a VkFormat %u texture; a tile function on a reinterpreting format view is unsupported", att, _pass.colfmt[att], im->fmt);
            return NO;
        }
        uint32_t pfmt = 0, pbpp = 0; BOOL blit = NO;
        if (nvmtl_tile_plane_format(im->fmt, im->bpp, &pfmt, &pbpp, &blit)) {
            nvlog("tile: attachment %u (VkFormat %u) has no storage stand-in - unsupported", att, im->fmt); return NO;
        }
        if (blit && (_pass.col_level[att] || _pass.col_layer[att])) {
            nvlog("tile: attachment %u needs a converting plane and is a level/slice of its texture - unsupported", att); return NO;
        }
        NVMTLTileImage *pl = ts->_plane[att];
        if (!pl || pl->_w != pw || pl->_h != ph || pl->_i.fmt != pfmt) {
            if (pl) [self nvmtlTileFlush:ts];   // a plane that is replaced hands its writes back first
            pl = nvmtl_tile_image_take(_cb, pw, ph, pfmt, pbpp);
            if (!pl) return NO;
            ts->_plane[att] = pl; ts->_blit[att] = blit; ts->_stale[att] = YES; ts->_dirty[att] = NO;
        }
        if (ts->_stale[att]) {
            const uint32_t w = MIN(_pass.colw[att], _pass.w), h = MIN(_pass.colh[att], _pass.h);
            int rc = blit ? nvmtl_vk_cmd_blit_image_2d(&_cb->_c, im, &pl->_i, w, h)
                          : nvmtl_vk_cmd_copy_image_sub(&_cb->_c, im, _pass.col_level[att], _pass.col_layer[att], 0, 0, &pl->_i, 0, 0, 0, 0, w, h);
            if (rc) { nvlog("tile: copying colour attachment %u into its storage plane FAILED", att); return NO; }
            ts->_stale[att] = NO;
        }
        nvmtl_vk_cmd_prepare_sampled_image(&_cb->_c, &pl->_i, 1);
        if (nvmtl_vk_tile_bind_plane(&_cb->_c, binding - 480, pl->_i.view)) { nvlog("tile: binding the plane of attachment %u FAILED", att); return NO; }
        NSString *acc = nvmtl_tile_str(a, @"access");
        if ([acc isEqualToString:@"WriteOnly"] || [acc isEqualToString:@"ReadWrite"]) ts->_dirty[att] = YES;
    }
    return YES;
}

- (void)dispatchThreadsPerTile:(MTLSize)tpt {
    NVMTLTileState *ts = nvmtl_tile_state(self, YES);
    NVMTLTileRenderPipelineState *tp = ts->_ps;
    if (_refused) return;
    if (!tp) { nvlog("tile: dispatchThreadsPerTile with no tile pipeline set - REFUSED"); return; }
    if (!tpt.width || !tpt.height || !tpt.depth) return;   // as Metal: nothing to run
    if (!ts->_tw) nvmtl_tile_size(self, &ts->_tw, &ts->_th);
    const uint32_t tw = ts->_tw, th = ts->_th;
    MTLSize req = MTLSizeMake(0, 0, 0);
    if (@available(macos 26.0, *)) req = tp->_tdesc.requiredThreadsPerThreadgroup;
    if (req.width && (req.width != tpt.width || req.height != tpt.height || req.depth != tpt.depth)) {
        nvlog("tile: dispatchThreadsPerTile %lux%lux%lu differs from the pipeline's required %lux%lux%lu - REFUSED", (unsigned long)tpt.width, (unsigned long)tpt.height, (unsigned long)tpt.depth,
              (unsigned long)req.width, (unsigned long)req.height, (unsigned long)req.depth);
        return;
    }
    if (tp->_matches && (tpt.width != tw || tpt.height != th)) {
        nvlog("tile: the pipeline says its threadgroup matches the tile (%ux%u) and %lux%lu threads per tile were dispatched - REFUSED", tw, th, (unsigned long)tpt.width, (unsigned long)tpt.height);
        return;
    }
    if (tpt.width * tpt.height * tpt.depth > tp.maxTotalThreadsPerThreadgroup) {
        nvlog("tile: %lux%lux%lu threads per tile exceeds the pipeline's %lu - REFUSED", (unsigned long)tpt.width, (unsigned long)tpt.height, (unsigned long)tpt.depth, (unsigned long)tp.maxTotalThreadsPerThreadgroup);
        return;
    }
    for (int a = 0; a < NVMTL_NCOL; a++) if (_pass.col[a] && _pass.col[a]->samples > 1) {
        nvlog("tile: a tile function on a multisampled pass is unsupported"); return; }
    if (!_pass.w || !_pass.h) { nvlog("tile: the pass has no size - REFUSED"); return; }
    uint32_t tiles[2] = { (_pass.w + tw - 1) / tw, (_pass.h + th - 1) / th };

    [self nvmtlTileLeavePass:ts];
    // writes of the draws (or of an earlier tile function) must reach the copies and the storage reads that follow
    nvmtl_vk_cmd_barrier(&_cb->_c);
    void *saved = nvmtl_vk_tile_table_save(&_cb->_c);
    BOOL ok = [self nvmtlTilePlanes:ts tiles:tiles];
    if (ok && tp->_explicitNames) {
        ok = [self nvmtlTileEnsureExplicit:ts names:tp->_explicitNames];
        if (ok) [self nvmtlTileBindExplicit:ts set:0 count:tp->_explicitNames.count];
    }
    // Tile memory outlives the dispatch when the pass gave it a size: the function then keeps each threadgroup argument's
    // contents in a buffer that is one slice per tile, and the translated function is a variant for where the arguments sit
    const uint32_t tgbytes = _pass.tile_tg_bytes, tgstride = (tgbytes + 15u) & ~15u;
    const BOOL persistent = ok && tgbytes && ts->_tgs.count;
    uint32_t offs[32] = { 0 };
    NSMutableString *sig = [NSMutableString string];
    if (persistent) {
        for (NSNumber *k in [[ts->_tgs allKeys] sortedArrayUsingSelector:@selector(compare:)]) {
            const NSUInteger idx = k.unsignedIntegerValue, len = [ts->_tgs[k][0] unsignedIntegerValue], off = [ts->_tgs[k][1] unsignedIntegerValue];
            if (idx >= 32 || off + len > tgbytes) {
                nvlog("tile: threadgroup argument %lu (%lu bytes at %lu) does not fit the pass's %u bytes of tile memory - REFUSED", (unsigned long)idx, (unsigned long)len, (unsigned long)off, tgbytes);
                ok = NO; break;
            }
            offs[idx] = (uint32_t)off; [sig appendFormat:@"%lu@%lu;", (unsigned long)idx, (unsigned long)off];
        }
        if (ok && ts->_bufs[@(NVMTL_TILE_MEMORY_SLOT)]) {
            nvlog("tile: setTileBuffer at index %u collides with the slot persistent tile memory is read through - unsupported", NVMTL_TILE_MEMORY_SLOT);
            ok = NO;
        }
    }
    if (ok) {
        NVMTL_TILE_SCOPE(NVMTL_TILE_FLAG_KERNEL | (persistent ? NVMTL_TILE_FLAG_PERSISTENT : 0u));
        NVMTLComputePipelineState *cps = tp->_baseCPS;
        if (persistent) {
            nvmtl_tile_scope_offsets(offs);
            cps = tp->_cpsBySig[sig];
            if (!cps) {
                NSString *why = nil;
                cps = nvmtl_tile_make_cps(tp->_tileFn, &why);
                if (cps) tp->_cpsBySig[sig] = cps;
                else { nvlog("tile: \"%s\" did not translate with persistent tile memory (%s) - unsupported", tp->_tileFn->_fname.UTF8String, why.UTF8String ?: "?"); ok = NO; }
            }
            if (ok && !ts->_tilemem) {
                const uint64_t size = (uint64_t)tiles[0] * tiles[1] * tgstride;
                NVMTLBuffer *mem = size <= (1ull << 30) ? (NVMTLBuffer *)[(id<MTLDevice>)gNVMTLMainDevice newBufferWithLength:(NSUInteger)size options:MTLResourceStorageModePrivate] : nil;
                if (!mem) { nvlog("tile: %llu bytes of tile memory (%ux%u tiles x %u bytes) cannot be made - REFUSED", (unsigned long long)size, tiles[0], tiles[1], tgstride); ok = NO; }
                else {
                    nvmtl_retain_resource(_cb, mem);
                    nvmtl_vk_cmd_barrier(&_cb->_c);
                    if (nvmtl_vk_cmd_fill_buffer(&_cb->_c, &mem->_b, 0, (size_t)size, 0)) nvlog("tile: clearing the tile memory FAILED");
                    nvmtl_vk_cmd_barrier(&_cb->_c);
                    ts->_tilemem = mem;
                }
            }
            if (ok && nvmtl_vk_bind_buffer(&_cb->_c, NVMTL_SET_VERTEX, NVMTL_TILE_MEMORY_SLOT, &ts->_tilemem->_b, 0)) { nvlog("tile: binding the tile memory FAILED"); ok = NO; }
        }
        if (ok) {
            NVMTLComputeCommandEncoder *ce = [NVMTLComputeCommandEncoder new];
            ce->_cb = _cb;
            [ce setComputePipelineState:cps];
            for (NSNumber *k in [[ts->_bufs allKeys] sortedArrayUsingSelector:@selector(compare:)]) {
                id v = ts->_bufs[k];
                if ([v isKindOfClass:[NSData class]]) [ce setBytes:[v bytes] length:[v length] atIndex:k.unsignedIntegerValue];
                else [ce setBuffer:v[0] offset:[v[1] unsignedIntegerValue] atIndex:k.unsignedIntegerValue];
            }
            for (NSNumber *k in ts->_texs) {
                if (k.unsignedIntegerValue >= NVMTL_TILE_STEX_BASE) {
                    nvlog("tile: setTileTexture at index %lu collides with the imageblock plane band (from %u) - unsupported", (unsigned long)k.unsignedIntegerValue, NVMTL_TILE_STEX_BASE);
                    continue; }
                [ce setTexture:ts->_texs[k] atIndex:k.unsignedIntegerValue];
            }
            for (NSNumber *k in ts->_samps) { NSArray *sm = ts->_samps[k]; [ce setSamplerState:sm[0] lodMinClamp:[sm[1] floatValue] lodMaxClamp:[sm[2] floatValue] atIndex:k.unsignedIntegerValue]; }
            for (NSNumber *k in ts->_tgs) [ce setThreadgroupMemoryLength:[ts->_tgs[k][0] unsignedIntegerValue] atIndex:k.unsignedIntegerValue];
            nvmtl_vk_tile_push(&_cb->_c, &cps->_p, tw, th, tiles[0], tgstride / 4);
            [ce dispatchThreadgroups:MTLSizeMake(tiles[0], tiles[1], 1) threadsPerThreadgroup:tpt];
            if (nvmtl_tile_say_once(ts, @"first")) nvlog("tile: \"%s\" dispatched %ux%u tile(s) of %ux%u, %lux%lu threads each%s", tp->_tileFn->_fname.UTF8String, tiles[0], tiles[1], tw, th, (unsigned long)tpt.width, (unsigned long)tpt.height, persistent ? ", tile memory kept" : "");
        }
    }
    nvmtl_vk_cmd_barrier(&_cb->_c);
    nvmtl_vk_tile_table_restore(&_cb->_c, saved);
}

@end
