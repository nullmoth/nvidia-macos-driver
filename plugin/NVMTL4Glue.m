/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Connects the Metal 4 command path (NVMTL4Core.m) to the components that own counter heaps and tensors
// (NVMTL4Extras.m). The core reaches them only through its hook pointers, so either side builds and tests alone.

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"

static BOOL nvmtl4_glue_write_timestamp(id encoder, id<MTLCommandBuffer> cb, id heap, NSUInteger index, NSInteger granularity)
{
    (void)encoder;   // the sample is recorded on the command buffer, inside or outside the open pass alike
    if (![(id)cb isKindOfClass:[NVMTLCommandBuffer class]]) return NO;
    return nvmtl4_counter_heap_write((NVMTLCommandBuffer *)cb, heap, index, granularity == MTL4TimestampGranularityPrecise);
}

static BOOL nvmtl4_glue_resolve_heap(id<MTLCommandBuffer> cb, id heap, NSRange range, NVMTLBuffer *dst, NSUInteger dstOffset,
                                     id<MTLFence> waitFence, id<MTLFence> updateFence)
{
    // Replay runs every command of one commit in order on one Metal 3 command buffer, so the fences order nothing
    // that is not already ordered there.
    (void)waitFence; (void)updateFence;
    if (![(id)cb isKindOfClass:[NVMTLCommandBuffer class]]) return NO;
    return nvmtl4_counter_heap_resolve((NVMTLCommandBuffer *)cb, heap, range, dst, dstOffset);
}

static BOOL nvmtl4_glue_tensor_backing(id tensor, __unsafe_unretained id<MTLBuffer> *buffer, NSUInteger *offset, NSUInteger *rank,
                                       NSInteger *dims, NSInteger *strides, size_t *elementSize)
{
    if (![tensor isKindOfClass:[NVMTLTensor class]]) return NO;
    NVMTLTensor *t = tensor;
    // A sub-byte element has no byte size; the core's copy works in whole elements, so those tensors are not offered.
    if (!t->_buf || !t->_L.es || t->_L.rank > MTL_TENSOR_MAX_RANK) return NO;
    *buffer = t->_buf; *offset = t->_off; *rank = t->_L.rank; *elementSize = t->_L.es;
    for (NSUInteger i = 0; i < t->_L.rank; i++) { dims[i] = t->_L.d[i]; strides[i] = t->_L.s[i]; }
    return YES;
}

static BOOL nvmtl4_glue_sparse_update(id<MTLCommandBuffer> m3cb, int kind, id resource, id source, id heap, const void *ops, NSUInteger count)
{
    // Kinds 0/1 map texture tiles, 2/3 buffer pages (update, copy). The bind waits for the Vulkan device to go idle, but work
    // committed to this queue may not have reached Vulkan yet (submission is asynchronous), so first drain the queue:
    // an empty command buffer completes only after everything committed before it.
    id<MTLCommandBuffer> drain = [m3cb.commandQueue commandBuffer];
    [drain commit]; [drain waitUntilCompleted];
    if (kind == 0) return nvmtl4_sparse_texture_update(resource, heap, ops, count);
    if (kind == 1) return nvmtl4_sparse_texture_copy(source, resource, ops, count);
    if (kind == 2) return nvmtl4_sparse_buffer_update(resource, heap, ops, count);
    if (kind == 3) return nvmtl4_sparse_buffer_copy(source, resource, ops, count);
    return NO;
}

__attribute__((constructor)) static void nvmtl4_glue_install(void)
{
    nvmtl4_hook_sparse_update = nvmtl4_glue_sparse_update;
    nvmtl4_hook_write_timestamp = nvmtl4_glue_write_timestamp;
    nvmtl4_hook_resolve_heap = nvmtl4_glue_resolve_heap;
    nvmtl4_hook_tensor_backing = nvmtl4_glue_tensor_backing;
}

#pragma clang diagnostic pop

// Metal 4 logical-to-physical colour attachment mapping on the Metal 3 render encoder that replays a Metal 4 one.
// A pipeline built with MTL4LogicalToPhysicalColorAttachmentMappingStateInherited writes its logical output L to the
// physical attachment the encoder's map names; every other pipeline writes L to L. The pass begins when the first
// pipeline is bound, so the attachments are permuted into the slots the pipeline writes before it begins; a bound
// pipeline that needs a different permutation ends the pass and begins it again with every attachment loaded.
@interface NVMTLRenderCommandEncoder (NVMTL4ColorMapPrivate)
- (void)nvmtlCloseVisibility;
@end
static const void *const kNVMTL4ColorMap = &kNVMTL4ColorMap;       // NSData: uint32_t[NVMTL_NCOL], the encoder's map
static const void *const kNVMTL4ColorApplied = &kNVMTL4ColorApplied; // NSData: uint32_t[NVMTL_NCOL], permutation in effect
static const void *const kNVMTL4ColorBase = &kNVMTL4ColorBase;     // NSData: nvk_pass as the app described it

@implementation NVMTLRenderCommandEncoder (NVMTL4ColorMap)
- (void)nvmtl4SetColorMap:(id)m   // MTLLogicalToPhysicalColorAttachmentMap; only a macOS 26 Metal 4 encoder calls this
{
    uint32_t phys[NVMTL_NCOL];
    for (uint32_t l = 0; l < NVMTL_NCOL; l++)
        phys[l] = m ? (uint32_t)((NSUInteger(*)(id, SEL, NSUInteger))objc_msgSend)(m, @selector(getPhysicalIndexForLogicalIndex:), l) : l;
    objc_setAssociatedObject(self, kNVMTL4ColorMap, [NSData dataWithBytes:phys length:sizeof phys], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}
// The full permutation for a pipeline: its used logical outputs go where the map says, the remaining slots take the
// physical attachments nobody claimed, in order, so the result is always a permutation the pass can be built from.
static BOOL nvmtl4_color_perm(const uint32_t *map, uint32_t usedMask, uint32_t *out)
{
    uint32_t taken = 0;
    for (uint32_t l = 0; l < NVMTL_NCOL; l++) {
        out[l] = UINT32_MAX;
        if (!(usedMask & (1u << l))) continue;
        if (map[l] >= NVMTL_NCOL || (taken & (1u << map[l]))) return NO;
        out[l] = map[l]; taken |= 1u << map[l];
    }
    uint32_t next = 0;
    for (uint32_t l = 0; l < NVMTL_NCOL; l++) {
        if (out[l] != UINT32_MAX) continue;
        while (taken & (1u << next)) next++;
        out[l] = next; taken |= 1u << next;
    }
    return YES;
}
- (void)nvmtl4PrepareColorMapForPipeline:(id<MTLRenderPipelineState>)ps
{
    if (![(id)ps isKindOfClass:[NVMTLRenderPipelineState class]]) return;
    NVMTLRenderPipelineState *p = (NVMTLRenderPipelineState *)ps;
    uint32_t want[NVMTL_NCOL], used = 0;
    for (uint32_t l = 0; l < NVMTL_NCOL && l < p->_p.ncol; l++) if (p->_p.cfmts[l]) used |= 1u << l;
    NSData *mapData = objc_getAssociatedObject(self, kNVMTL4ColorMap);
    extern BOOL nvmtl4_pipeline_inherits_color_mapping(id state);
    if (mapData && nvmtl4_pipeline_inherits_color_mapping(ps)) {
        if (!nvmtl4_color_perm(mapData.bytes, used, want)) {
            nvlog("mtl4: colour attachment map sends two logical outputs to one physical attachment - REFUSED, identity kept");
            for (uint32_t l = 0; l < NVMTL_NCOL; l++) want[l] = l;
        }
    } else for (uint32_t l = 0; l < NVMTL_NCOL; l++) want[l] = l;
    NSData *applied = objc_getAssociatedObject(self, kNVMTL4ColorApplied);
    uint32_t cur[NVMTL_NCOL];
    if (applied) memcpy(cur, applied.bytes, sizeof cur); else for (uint32_t l = 0; l < NVMTL_NCOL; l++) cur[l] = l;
    if (!memcmp(cur, want, sizeof cur)) return;
    NSData *baseData = objc_getAssociatedObject(self, kNVMTL4ColorBase);
    if (!baseData) {
        baseData = [NSData dataWithBytes:&_pass length:sizeof _pass];
        objc_setAssociatedObject(self, kNVMTL4ColorBase, baseData, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    }
    nvk_pass np; memcpy(&np, baseData.bytes, sizeof np);
    if (_begun) {
        // Ending and beginning again keeps what is drawn so far: every attachment is stored at the end and loaded at
        // the new beginning, so nothing is cleared twice.
        [self nvmtlCloseVisibility];
        nvmtl_vk_cmd_end_render(&_cb->_c);
        _begun = NO;
        np.load = NVMTL_LOAD_ALL;
        NSMutableData *b2 = [baseData mutableCopy]; ((nvk_pass *)b2.mutableBytes)->load = NVMTL_LOAD_ALL;
        objc_setAssociatedObject(self, kNVMTL4ColorBase, b2, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        static int said; if (said++ < 8) nvlog("mtl4: colour attachment map changed inside a pass - pass ended and begun again with its contents kept");
    }
    if (!nvmtl4_pass_apply_color_map(&np, want)) return;
    _pass = np;
    objc_setAssociatedObject(self, kNVMTL4ColorApplied, [NSData dataWithBytes:want length:sizeof want], OBJC_ASSOCIATION_RETAIN_NONATOMIC);
}
@end
