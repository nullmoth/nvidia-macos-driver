/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// The Metal 4 / macOS 26 and 27 device surface that is not a queue, an encoder or a compiler: counter heaps, tensors,
// texture view pools, sparse tiers, and the honest refusals for what this device does not build.
// Imported by NVMTLDevice.m after NVMTLObjects.m. Behaviour was measured against Apple's own implementation (an M1
// running macOS 27) with the oracles in tests/mtl4/; where Apple's answer is stated below it was observed there.

#import <objc/message.h>
#import "NVMTL4Counters.h"
#import "NVMTLTensor.h"
#import "NVMTLResourceIDs.h"
#include <limits.h>

// Everything here runs only for an app that already holds a macOS 26 object (a Metal 4 queue, a tensor, a counter
// heap): the selectors cannot be called on an older system. The category methods below have no interface declaration
// to carry an availability attribute, so the check is switched off for this file rather than guarded call by call.
// macOS 27 additions (tensor planes, buffer attachments) are reached by selector with plain `id` and NSInteger types,
// so the file also builds with the macOS 26 SDK of the build Mac.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
// A category supplying a protocol method the primary class leaves to it is the intent here, not a duplicate.
#pragma clang diagnostic ignored "-Wobjc-protocol-method-implementation"
// The binding classes take MTLBinding properties from NVMTLArgument's methods, and MTLArgumentType is the type it stores.
#pragma clang diagnostic ignored "-Wobjc-protocol-property-synthesis"
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
#define NVMTL4_PLANE_DATA 0
#define NVMTL4_PLANE_SCALES 1

// ---------------------------------------------------------------------------------------------------------------
// Counter heaps
// ---------------------------------------------------------------------------------------------------------------

// Apple's limit on a timestamp heap, measured: "Requested Heap size is too large (requested N max is 4096)".
#define NVMTL4_COUNTER_HEAP_MAX 4096u

@interface NVMTL4CounterHeap : NSObject <MTL4CounterHeap> {
    @public NVMTLCounterSampleBuffer *_sb; NSUInteger _count; MTL4CounterHeapType _type; NSString *_label;
}
@end

@implementation NVMTL4CounterHeap
- (NSString *)label { @synchronized (self) { return _label; } }
- (void)setLabel:(NSString *)l { @synchronized (self) { _label = [l copy]; } }
- (NSUInteger)count { return _count; }
- (MTL4CounterHeapType)type { return _type; }
- (NSData *)resolveCounterRange:(NSRange)r {
    if (!r.length || r.location > _count || r.length > _count - r.location) {
        nvlog("counter heap resolveCounterRange: %lu+%lu is outside this heap's %lu entries - nil",
              (unsigned long)r.location, (unsigned long)r.length, (unsigned long)_count);
        return nil;
    }
    NSMutableData *d = [NSMutableData dataWithLength:r.length * sizeof(MTL4TimestampHeapEntry)];
    if (nvmtl_vk_ts_read(_sb->_pool, (uint32_t)r.location, (uint32_t)r.length, d.mutableBytes)) {
        nvlog("counter heap resolveCounterRange: read FAILED - nil");
        return nil;
    }
    return d;
}
- (void)invalidateCounterRange:(NSRange)r {
    // Apple aborts on a range outside the heap; here it is refused and logged so a bad range is visible but not fatal.
    if (r.location > _count || r.length > _count - r.location) {
        nvlog("counter heap invalidateCounterRange: %lu+%lu is outside this heap's %lu entries - REFUSED",
              (unsigned long)r.location, (unsigned long)r.length, (unsigned long)_count);
        return;
    }
    if (r.length && nvmtl_vk_ts_clear(_sb->_pool, (uint32_t)r.location, (uint32_t)r.length))
        nvlog("counter heap invalidateCounterRange: clear FAILED");
}
@end

BOOL nvmtl4_counter_heap_write(NVMTLCommandBuffer *cb, id heap, NSUInteger index, BOOL precise)
{
    if (!cb || ![heap isKindOfClass:[NVMTL4CounterHeap class]]) {
        nvlog("counter heap write: %s is not a counter heap this device made - not sampled", heap ? object_getClassName(heap) : "nil");
        if (cb) nvmtl_counter_recording_failed(cb);
        return NO;
    }
    NVMTL4CounterHeap *h = heap;
    nvmtl_sample_counter(cb, h->_sb, index, heap, precise);
    return !cb->_c.counter_error;
}

BOOL nvmtl4_counter_heap_resolve(NVMTLCommandBuffer *cb, id heap, NSRange range, id<MTLBuffer> dst, NSUInteger dstOffset)
{
    if (!cb) return NO;
    NVMTL4CounterHeap *h = [heap isKindOfClass:[NVMTL4CounterHeap class]] ? heap : nil;
    NVMTLBuffer *b = [dst isKindOfClass:[NVMTLBuffer class]] ? (NVMTLBuffer *)dst : nil;
    if (!h || !b || !range.length || range.location > h->_count || range.length > h->_count - range.location) {
        nvlog("counter heap resolve: %s - REFUSED (range %lu+%lu of %lu entries)",
              !h ? "not a counter heap this device made" : !b ? "no destination buffer this device made" : "empty or outside range",
              (unsigned long)range.location, (unsigned long)range.length, h ? (unsigned long)h->_count : 0ul);
        nvmtl_counter_recording_failed(cb);
        return NO;
    }
    if (!cb->_resources) cb->_resources = [NVMTLResSet new];
    [cb->_resources addObject:b];
    [cb->_resources addObject:h->_sb];
    if (nvmtl_vk_cmd_ts_resolve(&cb->_c, h->_sb->_pool, (uint32_t)range.location, (uint32_t)range.length, &b->_b, dstOffset)) {
        nvmtl_counter_recording_failed(cb);
        return NO;
    }
    return YES;
}

static NSError *nvmtl4_heap_error(NSString *domain, NSString *why)
{
    return [NSError errorWithDomain:domain code:1 userInfo:@{NSLocalizedDescriptionKey: why}];
}

// ---------------------------------------------------------------------------------------------------------------
// Tensors
// ---------------------------------------------------------------------------------------------------------------

// Bits per element of a tensor data type, 0 when it is not a data plane type. `format` is set for the macOS 26.4 and
// 27 block formats (4-bit, 2-bit, 8-bit float and 4-bit float), which carry alignment rules and may carry a scales plane.
// The numeric values are the SDK's; the enum names are newer than the build SDK.
static unsigned nvmtl4_tensor_bits(MTLTensorDataType t, BOOL *format)
{
    *format = NO;
    switch ((NSInteger)t) {
        case MTLTensorDataTypeFloat32: case MTLTensorDataTypeInt32: case MTLTensorDataTypeUInt32: return 32;
        case MTLTensorDataTypeFloat16: case MTLTensorDataTypeBFloat16: case MTLTensorDataTypeInt16: case MTLTensorDataTypeUInt16: return 16;
        case MTLTensorDataTypeInt8: case MTLTensorDataTypeUInt8: return 8;
        case 141: case 142: *format = YES; return 8;     // E5M2, E4M3
        case 143: case 144: case 148: *format = YES; return 4;   // Int4, UInt4, E2M1
        case 149: case 150: *format = YES; return 2;     // UInt2, Int2
        default: return 0;                                // 145 (UE8M0) is a scales type only
    }
}
#define NVMTL4_DT_UE8M0 145

static NSUInteger nvmtl4_extents_read(MTLTensorExtents *e, NSInteger *out)
{
    const NSUInteger r = MIN(e ? e.rank : 0, (NSUInteger)MTL_TENSOR_MAX_RANK);
    for (NSUInteger i = 0; i < r; i++) out[i] = [e extentAtDimensionIndex:i];
    return r;
}

static MTLTensorExtents *nvmtl4_extents_make(NSUInteger rank, const NSInteger *v) { return [[MTLTensorExtents alloc] initWithRank:rank values:v]; }

static NSError *nvmtl4_tensor_error(NSString *why, BOOL unsupported)
{
    if (unsupported) nvlog("tensor: unsupported - %s", why.UTF8String);
    else nvlog("tensor: descriptor REFUSED - %s", why.UTF8String);
    return [NSError errorWithDomain:MTLTensorDomain code:MTLTensorErrorInvalidDescriptor
        userInfo:@{NSLocalizedDescriptionKey: [@"Tensor Descriptor Validation\n" stringByAppendingString:why]}];
}

// Fills packedBytes and needBytes from d, s and ebits; NO when they overflow.
static BOOL nvmtl4_layout_sizes(nvmtl4_tensor_layout *L)
{
    size_t elems = 1, last = 1, bits;
    for (NSUInteger i = 0; i < L->rank; i++) {
        size_t span;
        if (__builtin_mul_overflow(elems, (size_t)L->d[i], &elems)) return NO;
        if (__builtin_mul_overflow((size_t)(L->d[i] - 1), (size_t)L->s[i], &span) || __builtin_add_overflow(last, span, &last)) return NO;
    }
    if (__builtin_mul_overflow(elems, (size_t)L->ebits, &bits)) return NO;
    L->packedBytes = (bits + 7) / 8;
    if (__builtin_mul_overflow(last, (size_t)L->ebits, &bits)) return NO;
    L->needBytes = (bits + 7) / 8;
    return YES;
}

typedef struct {
    nvmtl4_tensor_layout L, S;   // data plane and (when hasS) scales plane
    BOOL hasS; NSInteger bf0;    // scales: one per bf0 elements along dimension 0
} nvmtl4_parsed;

// Validates a tensor descriptor the way Apple does (messages and rules observed on an M1) and fills the layout.
// `wrapped` = the tensor is made over a caller's buffer, so the strides are required and checked; otherwise it is
// packed and any strides are ignored.
static BOOL nvmtl4_tensor_parse(MTLTensorDescriptor *desc, BOOL wrapped, nvmtl4_parsed *P, NSError **err)
{
#define NVT_FAIL(unsup, ...) do { if (err) *err = nvmtl4_tensor_error([NSString stringWithFormat:__VA_ARGS__], unsup); return NO; } while (0)
    memset(P, 0, sizeof *P);
    nvmtl4_tensor_layout *L = &P->L;
    if (!desc) NVT_FAIL(NO, @"no descriptor");
    if (desc.dimensions.rank > MTL_TENSOR_MAX_RANK) NVT_FAIL(NO, @"tensor.dimensions.rank (%lu) exceeds %d", (unsigned long)desc.dimensions.rank, MTL_TENSOR_MAX_RANK);
    L->rank = nvmtl4_extents_read(desc.dimensions, L->d);
    for (NSUInteger i = 0; i < L->rank; i++)
        if (L->d[i] <= 0) NVT_FAIL(NO, @"[tensor.dimensions extentAtDimensionIndex:%lu] (%ld) should be greater than 0", (unsigned long)i, (long)L->d[i]);
    BOOL format;
    L->ebits = nvmtl4_tensor_bits(desc.dataType, &format);
    L->es = L->ebits >= 8 ? L->ebits / 8 : 0;
    if (!L->ebits) {
        if ((NSInteger)desc.dataType == NVMTL4_DT_UE8M0) NVT_FAIL(NO, @"MTLTensorDataTypeMetalFloat8UE8M0 is not a valid data plane data type");
        NVT_FAIL(NO, @"data type %ld is not a tensor data type", (long)desc.dataType);
    }
    if (format && L->rank && L->d[0] % 32)
        NVT_FAIL(NO, @"[tensor.dimensions extentAtDimensionIndex:0] (%ld) should be aligned to 32 elements when tensor.dataType is a format MTLTensorDataType.", (long)L->d[0]);

    // macOS 27: a scales auxiliary plane. Block-scaled formats carry one 8-bit scale per block of elements along dimension 0.
    if ([desc respondsToSelector:@selector(auxiliaryPlanes)]) {
        id aux = ((id (*)(id, SEL))objc_msgSend)(desc, @selector(auxiliaryPlanes));
        id sdsc = aux ? ((id (*)(id, SEL, NSInteger))objc_msgSend)(aux, @selector(descriptorForPlane:), NVMTL4_PLANE_SCALES) : nil;
        if (sdsc) {
            const NSInteger sdt = ((NSInteger (*)(id, SEL))objc_msgSend)(sdsc, @selector(dataType));
            MTLTensorExtents *bf = ((id (*)(id, SEL))objc_msgSend)(sdsc, @selector(blockFactors));
            if (sdt != NVMTL4_DT_UE8M0) NVT_FAIL(NO, @"MTLTensorDataType (%ld) is not a valid data type for MTLTensorPlaneTypeScales", (long)sdt);
            if (!format) NVT_FAIL(NO, @"%ld is not a valid data plane data type for a tensor with a scales auxiliary plane", (long)desc.dataType);
            if (bf.rank != L->rank) NVT_FAIL(NO, @"Dimensions must have the same rank (%lu) as block factors for MTLTensorPlaneTypeScales (%lu)", (unsigned long)L->rank, (unsigned long)bf.rank);
            NSInteger b[MTL_TENSOR_MAX_RANK]; nvmtl4_extents_read(bf, b);
            if (L->rank < 1 || b[0] < 1) NVT_FAIL(NO, @"MTLTensorPlaneTypeScales block factor extent at dimension index 0 must be greater than 0");
            for (NSUInteger i = 1; i < L->rank; i++)
                if (b[i] != 1) NVT_FAIL(NO, @"MTLTensorPlaneTypeScales block factor extent at dimension index %lu (%ld) must be 1", (unsigned long)i, (long)b[i]);
            nvmtl4_tensor_layout *S = &P->S;
            S->rank = L->rank; S->ebits = 8; S->es = 1; P->hasS = YES; P->bf0 = b[0];
            S->d[0] = (L->d[0] + b[0] - 1) / b[0];
            for (NSUInteger i = 1; i < L->rank; i++) S->d[i] = L->d[i];
            S->s[0] = 1;
            if (L->rank > 1) S->s[1] = (S->d[0] + 3) / 4 * 4;   // a scale row starts on a 4-byte boundary (measured)
            for (NSUInteger i = 2; i < L->rank; i++) S->s[i] = S->s[i - 1] * S->d[i - 1];
            if (!nvmtl4_layout_sizes(S)) NVT_FAIL(NO, @"the scales plane addresses more bytes than fit in memory");
            // Apple reports the tight size of the scales in the allocation: product of the scale dimensions.
            size_t tight = 1; for (NSUInteger i = 0; i < S->rank; i++) tight *= (size_t)S->d[i];
            S->packedBytes = tight;
        }
    }

    if (wrapped) {
        NSInteger sv[MTL_TENSOR_MAX_RANK];
        if (L->rank && !desc.strides) NVT_FAIL(NO, @"tensor.strides should not be nil when tensor.dimensions.rank > 0 (%lu)", (unsigned long)L->rank);
        const NSUInteger sr = nvmtl4_extents_read(desc.strides, sv);
        if (L->rank && desc.strides.rank != L->rank)
            NVT_FAIL(NO, @"tensor.strides.rank (%lu) should match the tensor.dimensions.rank (%lu)", (unsigned long)desc.strides.rank, (unsigned long)L->rank);
        for (NSUInteger i = 0; i < sr && i < L->rank; i++) L->s[i] = sv[i];
        if (L->rank && L->s[0] != 1) NVT_FAIL(NO, @"[tensor.strides extentAtDimensionIndex:0] (%ld) should be 1", (long)L->s[0]);
        for (NSUInteger i = 0; i < L->rank; i++)
            if (L->s[i] < 0) NVT_FAIL(NO, @"Stride (%ld) at index %lu should be greater than or equal to 0", (long)L->s[i], (unsigned long)i);
        if (format) {
            const NSInteger al = 1024 / (NSInteger)L->ebits;   // 128 bytes in elements
            for (NSUInteger i = 1; i < L->rank; i++)
                if (L->s[i] % al)
                    NVT_FAIL(NO, @"[tensor.strides extentAtDimensionIndex:%lu] (%ld elements) must be aligned to %ld elements (128 byte aligned) when tensor.dataType is a %u-bit format", (unsigned long)i, (long)L->s[i], (long)al, L->ebits);
        }
        if (desc.usage & MTLTensorUsageMachineLearning) {
            if (L->rank > 1 && (L->s[1] * (NSInteger)L->ebits) % 512)
                NVT_FAIL(NO, @"[tensor.strides extentAtDimensionIndex:1] (%ld) should be aligned to 64 bytes when using MTLTensorUsageMachineLearning", (long)L->s[1]);
            for (NSUInteger i = 2; i < L->rank; i++)
                if (L->s[i] != L->s[i - 1] * L->d[i - 1])
                    NVT_FAIL(NO, @"[tensor.strides extentAtDimensionIndex:%lu] (%ld) should be strides[i-1] * dimensions[i-1] when using MTLTensorUsageMachineLearning", (unsigned long)i, (long)L->s[i]);
        }
    } else {
        for (NSUInteger i = 0; i < L->rank; i++) L->s[i] = i ? L->s[i - 1] * L->d[i - 1] : 1;
    }
    if (wrapped && P->hasS) {
        // over a buffer the scales follow the data's own strides: scale row i starts every strides[i] / block factor
        // scales (measured: fp8 rows of 128 and 256 elements put scale rows 4 and 8 apart, fp4 rows of 256 and 512
        // elements 8 and 16 apart)
        nvmtl4_tensor_layout *S = &P->S; const size_t tight = S->packedBytes;
        for (NSUInteger i = 1; i < L->rank; i++) if (P->bf0 > 0 && L->s[i] % P->bf0 == 0) S->s[i] = L->s[i] / P->bf0;
        if (!nvmtl4_layout_sizes(S)) NVT_FAIL(NO, @"the scales plane addresses more bytes than fit in memory");
        S->packedBytes = tight;
    }
    if (!nvmtl4_layout_sizes(L)) NVT_FAIL(NO, @"tensor.dimensions and strides address more bytes than fit in memory");
    return YES;
#undef NVT_FAIL
}

static NSMapTable *nvmtl4_tensor_ids;
static os_unfair_lock nvmtl4_tensor_ids_lock = OS_UNFAIR_LOCK_INIT;
static uint64_t nvmtl4_tensor_id_seq;
// A host-visible table of fixed-size entries the GPU reads by resource id.
typedef struct { nvk_buffer buf; uint32_t capacity, stride; int failed; } nvmtl4_table;
static nvmtl4_table nvmtl4_tensor_table = { .capacity = NVMTL_TENSOR_TABLE_CAPACITY, .stride = sizeof(nvmtl_tensor_entry) };
static nvmtl4_table nvmtl4_ds_table = { .capacity = NVMTL_DS_TABLE_CAPACITY, .stride = sizeof(nvmtl_ds_entry) };

// Entry storage for 1-based id; NULL (logged once) when the table does not exist or the id is past its capacity.
static void *nvmtl4_table_entry(nvmtl4_table *t, uint64_t id, const char *what)
{
    if (!t->buf.map && !t->failed) {
        if (nvmtl_vk_buffer_create((size_t)t->capacity * t->stride, 1, &t->buf) || !t->buf.map) {
            t->failed = 1; nvlog("%s table: FAILED - the %u-entry GPU table could not be allocated", what, t->capacity);
        } else memset(t->buf.map, 0, (size_t)t->capacity * t->stride);
    }
    if (t->failed) return NULL;
    if (id == 0 || id > t->capacity) { nvlog("%s table: id %llu is past the %u-entry table - the id has no GPU entry", what, (unsigned long long)id, t->capacity); return NULL; }
    return (char *)t->buf.map + (size_t)(id - 1) * t->stride;
}
uint64_t nvmtl4_tensor_table_address(void) { os_unfair_lock_lock(&nvmtl4_tensor_ids_lock); (void)nvmtl4_table_entry(&nvmtl4_tensor_table, 1, "tensor"); uint64_t a = nvmtl4_tensor_table.buf.map ? nvmtl_vk_buffer_address(&nvmtl4_tensor_table.buf) : 0; os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock); return a; }
// The tensor table as a buffer object, so a dispatch binds entry `id` the way it binds any buffer (buffer = the table, offset
// = the entry). A shader's tensor argument is then a pointer to its nvmtl_tensor_entry. The table lives as long as the
// process, so the wrapper never frees it. Returns nil (logged by the table) when the table or the entry does not exist.
static NVMTLBuffer *nvmtl4_tensor_table_wrapper;
id<MTLBuffer> nvmtl4_tensor_entry_buffer(uint64_t id, NSUInteger *offset)
{
    id &= ~NVMTL_RESID_TENSOR;   // accepts the resource id or the bare table index
    os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
    void *entry = nvmtl4_table_entry(&nvmtl4_tensor_table, id, "tensor");
    if (entry && !nvmtl4_tensor_table_wrapper) {
        NVMTLBuffer *w = [NVMTLBuffer new];
        w->_storage = MTLStorageModeShared; w->_ropt = (MTLResourceOptions)MTLStorageModeShared << MTLResourceStorageModeShift;
        w->_b = nvmtl4_tensor_table.buf;
        nvmtl4_tensor_table_wrapper = w;
    }
    NVMTLBuffer *w = entry ? nvmtl4_tensor_table_wrapper : nil;
    os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
    if (w && offset) *offset = (NSUInteger)(id - 1) * sizeof(nvmtl_tensor_entry);
    return w;
}
uint64_t nvmtl4_ds_table_address(void) { os_unfair_lock_lock(&nvmtl4_tensor_ids_lock); (void)nvmtl4_table_entry(&nvmtl4_ds_table, 1, "depth stencil"); uint64_t a = nvmtl4_ds_table.buf.map ? nvmtl_vk_buffer_address(&nvmtl4_ds_table.buf) : 0; os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock); return a; }

@implementation NVMTLTensorAuxPlane
- (MTLTensorDataType)dataType { return _t->_sdt; }
- (MTLTensorExtents *)blockFactors {
    NSInteger b[MTL_TENSOR_MAX_RANK];
    for (NSUInteger i = 0; i < _t->_L.rank; i++) b[i] = i ? 1 : _t->_bf0;
    return nvmtl4_extents_make(_t->_L.rank, b);
}
- (id<MTLBuffer>)buffer { return _t->_wrapped ? _t->_sbuf : nil; }
- (NSUInteger)bufferOffset { return _t->_wrapped ? _t->_soff : 0; }
- (NSInteger)planeType { return NVMTL4_PLANE_SCALES; }
@end

@implementation NVMTLTensor
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { @synchronized (self) { return _label; } }
- (void)setLabel:(NSString *)l { @synchronized (self) { _label = [l copy]; } }
- (MTLCPUCacheMode)cpuCacheMode { return _cache; }
- (MTLStorageMode)storageMode { return _storage; }
- (MTLHazardTrackingMode)hazardTrackingMode { return _hazard; }
- (MTLResourceOptions)resourceOptions {
    return ((MTLResourceOptions)_storage << MTLResourceStorageModeShift) | (MTLResourceOptions)_cache
         | ((MTLResourceOptions)_hazard << MTLResourceHazardTrackingModeShift);
}
- (MTLPurgeableState)setPurgeableState:(MTLPurgeableState)s { return _wrapped ? MTLPurgeableStateNonVolatile : [_buf setPurgeableState:s]; }
- (id<MTLHeap>)heap { return nil; }
- (NSUInteger)heapOffset { return 0; }
- (NSUInteger)allocatedSize { return _wrapped ? _L.needBytes : _L.packedBytes + (_hasScales ? _S.packedBytes : 0); }
- (void)makeAliasable {}
- (BOOL)isAliasable { return NO; }
- (kern_return_t)setOwnerWithIdentity:(task_id_token_t)t { return [_buf setOwnerWithIdentity:t]; }
- (MTLResourceID)gpuResourceID {
    // A small registry id, as Apple hands out: it names the tensor in an argument buffer and is looked up with
    // nvmtl4_tensor_for_resource_id (see NVMTLResourceIDs.h for the layout the shader side reads).
    @synchronized (self) {
        if (!_rid) {
            os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
            if (!nvmtl4_tensor_ids) nvmtl4_tensor_ids = [[NSMapTable alloc]
                initWithKeyOptions:NSPointerFunctionsOpaqueMemory | NSPointerFunctionsIntegerPersonality
                valueOptions:NSPointerFunctionsWeakMemory capacity:0];
            _rid = ++nvmtl4_tensor_id_seq;
            [nvmtl4_tensor_ids setObject:self forKey:(__bridge id)(void *)(uintptr_t)_rid];
            nvmtl_tensor_entry *en = nvmtl4_table_entry(&nvmtl4_tensor_table, _rid, "tensor");
            if (en) {
                memset(en, 0, sizeof *en);
                en->data_addr = _buf.gpuAddress + (_wrapped ? _off : 0);
                en->rank = (uint32_t)_L.rank; en->dtype = (uint32_t)_dt; en->element_bits = _L.ebits;
                for (NSUInteger k = 0; k < _L.rank; k++) { en->dims[k] = (int32_t)_L.d[k]; en->strides[k] = (int32_t)_L.s[k]; }
                if (_hasScales) {
                    en->scales_addr = _sbuf.gpuAddress + _soff; en->block_factor0 = (uint32_t)_bf0;
                    for (NSUInteger k = 0; k < _S.rank; k++) { en->scale_dims[k] = (int32_t)_S.d[k]; en->scale_strides[k] = (int32_t)_S.s[k]; }
                }
            }
            os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
        }
    }
    MTLResourceID r; r._impl = _rid ? (NVMTL_RESID_TENSOR | _rid) : 0; return r;
}
- (void)dealloc {
    if (_rid) {
        os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
        [nvmtl4_tensor_ids removeObjectForKey:(__bridge id)(void *)(uintptr_t)_rid];
        os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
    }
}
- (id<MTLBuffer>)buffer { return _wrapped ? _buf : nil; }
- (NSUInteger)bufferOffset { return _wrapped ? _off : 0; }
- (MTLTensorExtents *)strides { return _wrapped ? nvmtl4_extents_make(_L.rank, _L.s) : nil; }
- (MTLTensorExtents *)dimensions { return nvmtl4_extents_make(_L.rank, _L.d); }
- (MTLTensorDataType)dataType { return _dt; }
- (MTLTensorUsage)usage { return _usage; }
- (NSArray *)auxiliaryPlanes {   // macOS 27
    if (!_hasScales) return @[];
    NVMTLTensorAuxPlane *p = [NVMTLTensorAuxPlane new]; p->_t = self;
    return @[p];
}

// Copies n elements of `bits` bits each between packed element arrays. Sub-byte elements are packed low bits first.
static void nvmtl4_bits_copy(char *dst, size_t di, const char *src, size_t si, size_t n, unsigned bits)
{
    if (bits >= 8) { const size_t b = bits / 8; memcpy(dst + di * b, src + si * b, n * b); return; }
    const size_t per = 8 / bits; const unsigned mask = (1u << bits) - 1;
    if (!(di % per) && !(si % per) && !(n % per)) { memcpy(dst + di / per, src + si / per, n / per); return; }
    for (size_t k = 0; k < n; k++) {
        const size_t a = si + k, b = di + k;
        const unsigned v = ((uint8_t)src[a / per] >> ((a % per) * bits)) & mask, sh = (unsigned)(b % per) * bits;
        uint8_t *p = (uint8_t *)dst + b / per;
        *p = (uint8_t)((*p & ~(mask << sh)) | (v << sh));
    }
}

// Moves the slice between the host array and one plane of the tensor, element by element, clipping to the tensor: an
// element of the slice that falls outside is skipped (Apple does the same; measured). `toTensor` = replace, else get.
- (void)nvmtlMove:(void *)host hostStrides:(MTLTensorExtents *)hs origin:(MTLTensorExtents *)org dims:(MTLTensorExtents *)sd
            plane:(NSInteger)plane toTensor:(BOOL)toTensor
{
    const char *who = toTensor ? "tensor replaceSliceOrigin" : "tensor getBytes";
    const nvmtl4_tensor_layout *PL = plane == NVMTL4_PLANE_SCALES ? &_S : &_L;
    id<MTLBuffer> pbuf = plane == NVMTL4_PLANE_SCALES ? _sbuf : _buf;
    const NSUInteger poff = plane == NVMTL4_PLANE_SCALES ? _soff : (_wrapped ? _off : 0);
    if (plane != NVMTL4_PLANE_DATA && !(plane == NVMTL4_PLANE_SCALES && _hasScales)) { nvlog("%s: plane %ld does not exist on this tensor - REFUSED", who, (long)plane); return; }
    if (!host || !hs || !org || !sd || org.rank != PL->rank || sd.rank != PL->rank || hs.rank != PL->rank) {
        nvlog("%s: origin, slice dimensions and strides must each have the plane's rank %lu - REFUSED", who, (unsigned long)PL->rank);
        return;
    }
    char *base = [pbuf contents];
    if (!base) { nvlog("%s: this tensor's storage is not CPU accessible - REFUSED", who); return; }
    base += poff;
    NSInteger o[MTL_TENSOR_MAX_RANK], n[MTL_TENSOR_MAX_RANK], h[MTL_TENSOR_MAX_RANK], idx[MTL_TENSOR_MAX_RANK] = {0};
    nvmtl4_extents_read(org, o); nvmtl4_extents_read(sd, n); nvmtl4_extents_read(hs, h);
    const NSUInteger R = PL->rank; const unsigned bits = PL->ebits;
    for (NSUInteger i = 0; i < R; i++)
        if (o[i] < 0 || n[i] < 0 || h[i] < 0) { nvlog("%s: negative origin, size or stride at dimension %lu - REFUSED", who, (unsigned long)i); return; }
    if (!R) {
        if (toTensor) nvmtl4_bits_copy(base, 0, host, 0, 1, bits); else nvmtl4_bits_copy(host, 0, base, 0, 1, bits);
        return;
    }
    for (NSUInteger i = 0; i < R; i++) if (!n[i]) return;
    for (;;) {
        // One run along dimension 0, clipped at the tensor's edge.
        BOOL inside = YES; size_t telem = 0, helem = 0;
        for (NSUInteger i = 0; i < R; i++) {
            const NSInteger at = o[i] + (i ? idx[i] : 0);
            if (i && at >= PL->d[i]) { inside = NO; break; }
            telem += (size_t)at * (size_t)PL->s[i];
            helem += (size_t)(i ? idx[i] : 0) * (size_t)h[i];
        }
        if (inside && o[0] < PL->d[0]) {
            const size_t run = (size_t)MIN(n[0], PL->d[0] - o[0]);
            if (h[0] == 1) {
                if (toTensor) nvmtl4_bits_copy(base, telem, host, helem, run, bits); else nvmtl4_bits_copy(host, helem, base, telem, run, bits);
            } else for (size_t k = 0; k < run; k++) {
                if (toTensor) nvmtl4_bits_copy(base, telem + k, host, helem + k * (size_t)h[0], 1, bits);
                else nvmtl4_bits_copy(host, helem + k * (size_t)h[0], base, telem + k, 1, bits);
            }
        }
        NSUInteger i = 1;
        for (; i < R; i++) { if (++idx[i] < n[i]) break; idx[i] = 0; }
        if (i >= R) break;
    }
    if (toTensor && pbuf.storageMode == MTLStorageModeManaged && [pbuf respondsToSelector:@selector(didModifyRange:)])
        [pbuf didModifyRange:NSMakeRange(poff, PL->needBytes)];
}
- (void)replaceSliceOrigin:(MTLTensorExtents *)o sliceDimensions:(MTLTensorExtents *)d withBytes:(const void *)bytes strides:(MTLTensorExtents *)s {
    [self nvmtlMove:(void *)bytes hostStrides:s origin:o dims:d plane:NVMTL4_PLANE_DATA toTensor:YES];
}
- (void)getBytes:(void *)bytes strides:(MTLTensorExtents *)s fromSliceOrigin:(MTLTensorExtents *)o sliceDimensions:(MTLTensorExtents *)d {
    [self nvmtlMove:bytes hostStrides:s origin:o dims:d plane:NVMTL4_PLANE_DATA toTensor:NO];
}
- (void)getBytes:(void *)bytes strides:(MTLTensorExtents *)s fromSliceOrigin:(MTLTensorExtents *)o sliceDimensions:(MTLTensorExtents *)d
           plane:(NSInteger)plane {   // macOS 27
    [self nvmtlMove:bytes hostStrides:s origin:o dims:d plane:plane toTensor:NO];
}
- (void)replaceSliceOrigin:(MTLTensorExtents *)o sliceDimensions:(MTLTensorExtents *)d plane:(NSInteger)plane
                 withBytes:(const void *)bytes strides:(MTLTensorExtents *)s {   // macOS 27
    [self nvmtlMove:(void *)bytes hostStrides:s origin:o dims:d plane:plane toTensor:YES];
}
@end

id nvmtl4_tensor_for_resource_id(uint64_t rid)
{
    if (!(rid & NVMTL_RESID_TENSOR)) return nil;
    rid &= ~NVMTL_RESID_TENSOR;
    if (!rid) return nil;
    os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
    id t = [nvmtl4_tensor_ids objectForKey:(__bridge id)(void *)(uintptr_t)rid];
    os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
    return t;
}

// Makes the tensor over the caller's buffers when `wrapped`, else over one fresh buffer holding both planes.
static id<MTLTensor> nvmtl4_tensor_make(id<MTLDevice> dev, MTLTensorDescriptor *desc, const nvmtl4_parsed *P,
                                        id<MTLBuffer> buffer, NSUInteger off, id<MTLBuffer> sbuf, NSUInteger soff, BOOL wrapped, NSError **err)
{
    NVMTLTensor *t = [NVMTLTensor new];
    t->_dev = dev; t->_L = P->L; t->_dt = desc.dataType; t->_usage = desc.usage; t->_wrapped = wrapped; t->_off = off;
    t->_storage = desc.storageMode; t->_cache = desc.cpuCacheMode;
    // Apple reports a default-hazard tensor as tracked (measured), the mode a standalone resource gets.
    t->_hazard = desc.hazardTrackingMode == MTLHazardTrackingModeDefault ? MTLHazardTrackingModeTracked : desc.hazardTrackingMode;
    if (P->hasS) { t->_hasScales = YES; t->_S = P->S; t->_bf0 = P->bf0; t->_sdt = (MTLTensorDataType)NVMTL4_DT_UE8M0; }
    if (!wrapped) {
        // Managed storage has no meaning on this device (Apple 27 deprecates it too): the backing buffer is shared.
        const MTLStorageMode st = t->_storage == MTLStorageModePrivate ? MTLStorageModePrivate : MTLStorageModeShared;
        const MTLResourceOptions opt = ((MTLResourceOptions)st << MTLResourceStorageModeShift) | (MTLResourceOptions)t->_cache;
        const size_t scalesAt = P->hasS ? (P->L.packedBytes + 127) / 128 * 128 : 0;
        const size_t total = MAX(scalesAt + (P->hasS ? P->S.needBytes : 0) + (P->hasS ? 0 : P->L.packedBytes), (size_t)16);
        buffer = [dev newBufferWithLength:total options:opt];
        if (!buffer) {
            nvlog("newTensor: FAILED - the %zu-byte backing buffer could not be allocated", total);
            if (err) *err = [NSError errorWithDomain:MTLTensorDomain code:MTLTensorErrorInternalError
                userInfo:@{NSLocalizedDescriptionKey: @"Tensor allocation failed"}];
            return nil;
        }
        if (st == MTLStorageModeShared) memset([buffer contents], 0, total);
        if (P->hasS) { sbuf = buffer; soff = scalesAt; }
    }
    t->_buf = buffer; t->_sbuf = sbuf; t->_soff = soff;
    nvlog("newTensor: rank %lu %zu bytes%s %s -> %p", (unsigned long)P->L.rank, P->L.packedBytes, P->hasS ? " + scales" : "", wrapped ? "over a caller buffer" : "own buffer", (__bridge void *)t);
    return t;
}

static NSError *nvmtl4_buffer_error(NSString *why) { return nvmtl4_tensor_error(why, NO); }

// Checks that one plane's buffer can carry it: alignment, size and storage mode (Apple's three rules, observed).
static BOOL nvmtl4_plane_check_buffer(const char *plane, MTLTensorDescriptor *desc, const nvmtl4_tensor_layout *L, size_t align,
                                      id<MTLBuffer> buf, NSUInteger off, NSError **err)
{
    if (!buf) { if (err) *err = nvmtl4_buffer_error([NSString stringWithFormat:@"no buffer for plane %s", plane]); return NO; }
    if (off % align) {
        if (err) *err = nvmtl4_buffer_error([NSString stringWithFormat:@"Buffer offset for plane %s (%lu bytes) should be aligned to the tensor data type size (%zu bytes)", plane, (unsigned long)off, align]);
        return NO;
    }
    if (off > buf.length || L->needBytes > buf.length - off) {
        if (err) *err = nvmtl4_buffer_error([NSString stringWithFormat:@"Buffer attachment for plane %s should be at least %zu bytes long", plane, L->needBytes + off]);
        return NO;
    }
    if (desc.storageMode != buf.storageMode) {
        if (err) *err = nvmtl4_buffer_error([NSString stringWithFormat:@"Tensor's storage mode (0x%lx) should match the storage mode of buffer for plane %s (0x%lx)", (unsigned long)desc.storageMode, plane, (unsigned long)buf.storageMode]);
        return NO;
    }
    return YES;
}

static BOOL nvmtl4_tensor_check_buffers(MTLTensorDescriptor *desc, const nvmtl4_parsed *P, id<MTLBuffer> buf, NSUInteger off,
                                        id<MTLBuffer> sbuf, NSUInteger soff, NSError **err)
{
    BOOL format; nvmtl4_tensor_bits(desc.dataType, &format);
    if (!nvmtl4_plane_check_buffer("MTLTensorPlaneTypeData", desc, &P->L, format ? 128 : MAX(P->L.es, (size_t)1), buf, off, err)) return NO;
    if (P->hasS) {
        if (!sbuf) { if (err) *err = nvmtl4_buffer_error(@"the scales plane needs a buffer attachment (newTensorWithDescriptor:attachments:error:)"); return NO; }
        return nvmtl4_plane_check_buffer("MTLTensorPlaneTypeScales", desc, &P->S, 1, sbuf, soff, err);
    }
    return YES;
}

// ---------------------------------------------------------------------------------------------------------------
// Placement sparse buffers
// ---------------------------------------------------------------------------------------------------------------

// A placement sparse buffer is private storage with no memory of its own. Its pages (the sparse page size the buffer was
// made with) are mapped to pages of a placement heap by queue operations; an unmapped page reads zeros and drops writes.
// The page table below mirrors what is bound, because copying mappings between buffers needs to know it.
typedef struct { void *mem; uint32_t memtype; uint64_t off; } nvmtl4_sparse_page;
@interface NVMTL4SparseState : NSObject { @public NSUInteger pageBytes, npages; nvmtl4_sparse_page *tab; NSMutableSet *heaps; } @end
@implementation NVMTL4SparseState
- (void)dealloc { free(tab); }
@end
static char kNVMTL4SparseState;

@implementation NVMTLBuffer (MTL4Extras)
- (id<MTLTensor>)newTensorWithDescriptor:(MTLTensorDescriptor *)desc offset:(NSUInteger)offset error:(NSError **)error
{
    nvmtl4_parsed P; NSError *e = nil;
    if (!nvmtl4_tensor_parse(desc, YES, &P, &e) || !nvmtl4_tensor_check_buffers(desc, &P, self, offset, nil, 0, &e)) { if (error) *error = e; return nil; }
    if (error) *error = nil;
    return nvmtl4_tensor_make([self device], desc, &P, self, offset, nil, 0, YES, error);
}
- (MTLBufferSparseTier)sparseBufferTier { return objc_getAssociatedObject(self, &kNVMTL4SparseState) ? MTLBufferSparseTier1 : MTLBufferSparseTierNone; }
@end

// ---------------------------------------------------------------------------------------------------------------
// Texture views and view pools
// ---------------------------------------------------------------------------------------------------------------

@implementation NVMTLTexture (MTL4Extras)
- (id<MTLTexture>)newTextureViewWithDescriptor:(MTLTextureViewDescriptor *)d
{
    if (!d) { nvlog("newTextureViewWithDescriptor: no descriptor - REFUSED"); return nil; }
    return [self newTextureViewWithPixelFormat:d.pixelFormat textureType:d.textureType levels:d.levelRange slices:d.sliceRange swizzle:d.swizzle];
}
- (MTLTextureSparseTier)sparseTextureTier { return nvmtl4_texture_has_sparse_state(self) ? MTLTextureSparseTier1 : MTLTextureSparseTierNone; }   // tier 1: no shader LOD clamp or residency queries
@end

@interface NVMTLTextureViewPool : NSObject <MTLTextureViewPool> {
    @public id<MTLDevice> _dev; NSString *_label; NSMutableArray *_slots; NSUInteger _count; uint32_t _first;
}
@end

// A texture's resource id is its bindless heap slot under this tag (see nvmtl_resid_for_texture in NVMTLEmbedded.m).
#define NVMTL4_RESID_HEAP (1ull << 62)

// A pool owns a run of consecutive bindless heap slots, so the id of view i is baseResourceID + i, as on Apple GPUs,
// and a shader reading the id from an argument buffer indexes the heap with it exactly as it does for any texture.
@implementation NVMTLTextureViewPool
- (id<MTLDevice>)device { return _dev; }
- (NSString *)label { return _label; }
- (NSUInteger)resourceViewCount { return _count; }
- (MTLResourceID)baseResourceID { MTLResourceID r; r._impl = NVMTL4_RESID_HEAP | _first; return r; }
- (void)dealloc { if (_first) nvmtl_vk_bindless_range_free(_first, (uint32_t)_count); }
- (MTLResourceID)nvmtlStore:(id<MTLTexture>)v atIndex:(NSUInteger)i
{
    MTLResourceID none; none._impl = 0;
    if (i >= _count) { nvlog("texture view pool: index %lu is outside the pool's %lu views - REFUSED", (unsigned long)i, (unsigned long)_count); return none; }
    if (!v) return none;
    if (![(id)v isKindOfClass:[NVMTLTexture class]]) { nvlog("texture view pool: a %s is not a texture this device made - REFUSED", object_getClassName(v)); return none; }
    @synchronized (self) { _slots[i] = v; }
    nvmtl_vk_bindless_range_set(_first + (uint32_t)i, [(NVMTLTexture *)v nvsampleview]);
    return [self baseResourceIDPlus:i];
}
- (MTLResourceID)baseResourceIDPlus:(NSUInteger)i { MTLResourceID r; r._impl = (NVMTL4_RESID_HEAP | _first) + i; return r; }
- (MTLResourceID)setTextureView:(id<MTLTexture>)texture atIndex:(NSUInteger)index {
    MTLResourceID none; none._impl = 0;
    if (!texture) { nvlog("texture view pool setTextureView: no texture - REFUSED"); return none; }
    return [self nvmtlStore:texture atIndex:index];
}
- (MTLResourceID)setTextureView:(id<MTLTexture>)texture descriptor:(MTLTextureViewDescriptor *)descriptor atIndex:(NSUInteger)index {
    MTLResourceID none; none._impl = 0;
    if (!texture || !descriptor) { nvlog("texture view pool setTextureView:descriptor: no texture or descriptor - REFUSED"); return none; }
    id<MTLTexture> v = [texture newTextureViewWithDescriptor:descriptor];
    if (!v) { nvlog("texture view pool setTextureView:descriptor: the view could not be made - REFUSED"); return none; }
    return [self nvmtlStore:v atIndex:index];
}
- (MTLResourceID)setTextureViewFromBuffer:(id<MTLBuffer>)buffer descriptor:(MTLTextureDescriptor *)descriptor offset:(NSUInteger)offset
                              bytesPerRow:(NSUInteger)bpr atIndex:(NSUInteger)index {
    MTLResourceID none; none._impl = 0;
    if (!buffer || !descriptor) { nvlog("texture view pool setTextureViewFromBuffer: no buffer or descriptor - REFUSED"); return none; }
    id<MTLTexture> v = [buffer newTextureWithDescriptor:descriptor offset:offset bytesPerRow:bpr];
    if (!v) { nvlog("texture view pool setTextureViewFromBuffer: the texture could not be made - REFUSED"); return none; }
    return [self nvmtlStore:v atIndex:index];
}
- (MTLResourceID)copyResourceViewsFromPool:(id<MTLResourceViewPool>)src sourceRange:(NSRange)r destinationIndex:(NSUInteger)di {
    MTLResourceID none; none._impl = 0;
    NVMTLTextureViewPool *s = [(id)src isKindOfClass:[NVMTLTextureViewPool class]] ? (NVMTLTextureViewPool *)src : nil;
    if (!s || r.location > s->_count || r.length > s->_count - r.location || di > _count || r.length > _count - di) {
        nvlog("texture view pool copyResourceViewsFromPool: %s - REFUSED", !s ? "not a pool this device made" : "range outside a pool");
        return none;
    }
    MTLResourceID first = none;
    for (NSUInteger k = 0; k < r.length; k++) {
        id v; @synchronized (s) { v = s->_slots[r.location + k]; }
        if ([v isKindOfClass:[NSNull class]]) v = nil;
        if (v) [self nvmtlStore:v atIndex:di + k];
        else { @synchronized (self) { _slots[di + k] = [NSNull null]; } nvmtl_vk_bindless_range_set(_first + (uint32_t)(di + k), NULL); }
        if (!k) first = [self baseResourceIDPlus:di];
    }
    return first;
}
@end

// ---------------------------------------------------------------------------------------------------------------
// Blit: copy between tensors
// ---------------------------------------------------------------------------------------------------------------

#define NVMTL4_TENSOR_COPY_MAX_RUNS 65536u

// One plane of a tensor as a blit sees it.
static BOOL nvmtl4_tensor_plane(NVMTLTensor *t, NSInteger plane, const nvmtl4_tensor_layout **L, id<MTLBuffer> *buf, NSUInteger *off)
{
    if (plane == NVMTL4_PLANE_DATA) { *L = &t->_L; *buf = t->_buf; *off = t->_wrapped ? t->_off : 0; return YES; }
    if (plane == NVMTL4_PLANE_SCALES && t->_hasScales) { *L = &t->_S; *buf = t->_sbuf; *off = t->_soff; return YES; }
    return NO;
}

// Copies a slice between two tensor planes with buffer copies, one per run contiguous in both.
static void nvmtl4_tensor_copy(NVMTLBlitCommandEncoder *enc, id<MTLTensor> src, MTLTensorExtents *so, MTLTensorExtents *sd, NSInteger sp,
                               id<MTLTensor> dst, MTLTensorExtents *dof, MTLTensorExtents *dd, NSInteger dp)
{
    NVMTLTensor *s = [(id)src isKindOfClass:[NVMTLTensor class]] ? (NVMTLTensor *)src : nil;
    NVMTLTensor *d = [(id)dst isKindOfClass:[NVMTLTensor class]] ? (NVMTLTensor *)dst : nil;
    if (!s || !d) { nvlog("blit copyFromTensor: a tensor this device did not make - REFUSED"); return; }
    const nvmtl4_tensor_layout *SL, *DL; id<MTLBuffer> sb, db; NSUInteger soff, doff;
    if (!nvmtl4_tensor_plane(s, sp, &SL, &sb, &soff) || !nvmtl4_tensor_plane(d, dp, &DL, &db, &doff)) {
        nvlog("blit copyFromTensor: plane %ld to plane %ld does not exist on these tensors - REFUSED", (long)sp, (long)dp);
        return;
    }
    const NSUInteger R = SL->rank;
    NSInteger a[16], an[16], b[16], bn[16];
    if (DL->rank != R || SL->ebits != DL->ebits || so.rank != R || sd.rank != R || dof.rank != R || dd.rank != R) {
        nvlog("blit copyFromTensor: ranks or element sizes differ (%lu vs %lu, %u vs %u bits) - REFUSED", (unsigned long)R, (unsigned long)DL->rank, SL->ebits, DL->ebits);
        return;
    }
    nvmtl4_extents_read(so, a); nvmtl4_extents_read(sd, an); nvmtl4_extents_read(dof, b); nvmtl4_extents_read(dd, bn);
    for (NSUInteger i = 0; i < R; i++) {
        if (an[i] != bn[i] || a[i] < 0 || b[i] < 0 || an[i] < 0 || a[i] + an[i] > SL->d[i] || b[i] + bn[i] > DL->d[i]) {
            nvlog("blit copyFromTensor: dimension %lu: source slice %ld+%ld of %ld, destination %ld+%ld of %ld - REFUSED", (unsigned long)i,
                  (long)a[i], (long)an[i], (long)SL->d[i], (long)b[i], (long)bn[i], (long)DL->d[i]);
            return;
        }
        if (!an[i]) return;
    }
    const unsigned bits = SL->ebits;
    if (!R) { [enc copyFromBuffer:sb sourceOffset:soff toBuffer:db destinationOffset:doff size:MAX(bits / 8u, 1u)]; return; }
    // A run is the longest span contiguous in both tensors: dimension 0, extended over the next dimension while the
    // slice covers the whole of the one before it and both tensors are packed across the pair.
    size_t run = (size_t)an[0]; NSUInteger j = 1;
    for (; j < R; j++) {
        const NSUInteger p = j - 1;
        if (an[p] != SL->d[p] || an[p] != DL->d[p] || SL->s[j] != SL->s[p] * SL->d[p] || DL->s[j] != DL->s[p] * DL->d[p]) break;
        run *= (size_t)an[j];
    }
    size_t runs = 1;
    for (NSUInteger i = j; i < R; i++) runs *= (size_t)an[i];
    if (runs > NVMTL4_TENSOR_COPY_MAX_RUNS) {
        // Too many separate runs for buffer copies: the strided copy is done by the CPU after a wait would be wrong
        // inside a command stream, so the limit is reported.
        nvlog("blit copyFromTensor: %zu separate runs exceeds the limit of %u - REFUSED", runs, NVMTL4_TENSOR_COPY_MAX_RUNS);
        return;
    }
    NSInteger idx[16] = {0};
    for (size_t r = 0; r < runs; r++) {
        size_t se = 0, de = 0;
        for (NSUInteger i = 0; i < R; i++) {
            const NSInteger k = i < j ? 0 : idx[i];
            se += (size_t)(a[i] + k) * (size_t)SL->s[i];
            de += (size_t)(b[i] + k) * (size_t)DL->s[i];
        }
        // Sub-byte elements: a copy is whole bytes, so the run and both starts must fall on byte boundaries.
        if ((se * bits) % 8 || (de * bits) % 8 || (run * bits) % 8) {
            nvlog("blit copyFromTensor: a %u-bit slice that does not start and end on byte boundaries (element %zu -> %zu, run %zu) - REFUSED", bits, se, de, run);
            return;
        }
        [enc copyFromBuffer:sb sourceOffset:soff + se * bits / 8 toBuffer:db destinationOffset:doff + de * bits / 8 size:run * bits / 8];
        for (NSUInteger i = j; i < R; i++) { if (++idx[i] < an[i]) break; idx[i] = 0; }
    }
}

@implementation NVMTLBlitCommandEncoder (MTL4Extras)
- (void)copyFromTensor:(id<MTLTensor>)src sourceOrigin:(MTLTensorExtents *)so sourceDimensions:(MTLTensorExtents *)sd
              toTensor:(id<MTLTensor>)dst destinationOrigin:(MTLTensorExtents *)dof destinationDimensions:(MTLTensorExtents *)dd
{
    nvmtl4_tensor_copy(self, src, so, sd, NVMTL4_PLANE_DATA, dst, dof, dd, NVMTL4_PLANE_DATA);
}
// macOS 27: the same copy with a plane named on each side.
- (void)copyFromTensor:(id<MTLTensor>)src sourceOrigin:(MTLTensorExtents *)so sourceDimensions:(MTLTensorExtents *)sd sourcePlane:(NSInteger)sp
              toTensor:(id<MTLTensor>)dst destinationOrigin:(MTLTensorExtents *)dof destinationDimensions:(MTLTensorExtents *)dd destinationPlane:(NSInteger)dp
{
    nvmtl4_tensor_copy(self, src, so, sd, sp, dst, dof, dd, dp);
}
@end

// ---------------------------------------------------------------------------------------------------------------
// Library, argument encoder, command encoders
// ---------------------------------------------------------------------------------------------------------------

static char kNVMTL4ReflBindings;

// MTLFunctionReflection is Apple's class (macOS 26); the plugin also loads on older systems, so the subclass that
// carries our bindings is made on first use instead of being linked against a class that may not exist.
static id nvmtl4_function_reflection_object(NSArray *bindings)
{
    static Class cls; static dispatch_once_t once;
    dispatch_once(&once, ^{
        Class sup = NSClassFromString(@"MTLFunctionReflection");
        if (!sup) return;
        cls = objc_allocateClassPair(sup, "NVMTLFunctionReflection", 0);
        if (!cls) return;
        class_addMethod(cls, @selector(bindings), imp_implementationWithBlock(^NSArray *(id self_) {
            return objc_getAssociatedObject(self_, &kNVMTL4ReflBindings) ?: @[];
        }), "@@:");
        // The user annotation attribute is not carried through the translated AIR: nil, as for a function without one.
        class_addMethod(cls, @selector(userAnnotation), imp_implementationWithBlock(^NSString *(id self_) { return nil; }), "@@:");
        objc_registerClassPair(cls);
    });
    if (!cls) return nil;
    id o = [[cls alloc] init];
    objc_setAssociatedObject(o, &kNVMTL4ReflBindings, bindings, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    return o;
}

@implementation NVMTLLibrary (MTL4Extras)
// Reflection of a function by name, before any pipeline exists: the same bindings a compute pipeline's reflection lists.
- (MTLFunctionReflection *)reflectionForFunctionWithName:(NSString *)name
{
    id<MTLFunction> f = name ? [self newFunctionWithName:name] : nil;
    if (!f) { nvlog("library reflectionForFunctionWithName: \"%s\" is not a function of this library - nil", name.UTF8String ?: ""); return nil; }
    NSArray *bindings = nvmtl_arguments_for_function((NVMTLFunction *)f);
    id r = nvmtl4_function_reflection_object(bindings);
    if (!r) nvlog("library reflectionForFunctionWithName: the system has no MTLFunctionReflection class - nil");
    return r;
}
@end

// A depth stencil state names itself to the GPU by a resource id, as a tensor does (macOS 26 MTLDepthStencilState
// gpuResourceID). The id is stored in an argument buffer or indirect command; the GPU side resolving it back to the
// state is shader work this device does not do yet, which the first store says.
static NSMapTable *nvmtl4_ds_ids;
static uint64_t nvmtl4_ds_id_seq;
static char kNVMTL4DepthStencilID;

@implementation NVMTLDepthStencilState (MTL4Extras)
- (MTLResourceID)gpuResourceID
{
    @synchronized (self) {
        NSNumber *n = objc_getAssociatedObject(self, &kNVMTL4DepthStencilID);
        if (!n) {
            os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
            if (!nvmtl4_ds_ids) nvmtl4_ds_ids = [[NSMapTable alloc]
                initWithKeyOptions:NSPointerFunctionsOpaqueMemory | NSPointerFunctionsIntegerPersonality
                valueOptions:NSPointerFunctionsWeakMemory capacity:0];
            n = @(++nvmtl4_ds_id_seq);
            [nvmtl4_ds_ids setObject:self forKey:(__bridge id)(void *)(uintptr_t)n.unsignedLongLongValue];
            nvmtl_ds_entry *en = nvmtl4_table_entry(&nvmtl4_ds_table, n.unsignedLongLongValue, "depth stencil");
            if (en) {
                memset(en, 0, sizeof *en);
                en->depth_write = (uint32_t)_write; en->depth_compare = (uint32_t)_compare; en->stencil_enabled = (uint32_t)_stencil;
                for (int k = 0; k < 6; k++) { en->front[k] = (uint32_t)_sf[k]; en->back[k] = (uint32_t)_sb[k]; }
            }
            os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
            objc_setAssociatedObject(self, &kNVMTL4DepthStencilID, n, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
        }
        MTLResourceID r; r._impl = n.unsignedLongLongValue; return r;
    }
}
@end

id nvmtl4_depth_stencil_for_resource_id(uint64_t rid)
{
    if (!rid) return nil;
    os_unfair_lock_lock(&nvmtl4_tensor_ids_lock);
    id o = [nvmtl4_ds_ids objectForKey:(__bridge id)(void *)(uintptr_t)rid];
    os_unfair_lock_unlock(&nvmtl4_tensor_ids_lock);
    return o;
}

@implementation NVMTLArgumentEncoder (MTL4Extras)
- (void)setDepthStencilState:(id<MTLDepthStencilState>)state atIndex:(NSUInteger)index
{
    static int said;
    if (state && !said++) nvlog("argument encoder setDepthStencilState: stores the state's resource id; a shader resolving it is unsupported on the GPU side (said once)");
    [self nvmtlPutHandle:state ? [state gpuResourceID]._impl : 0 atIndex:index what:"DepthStencilState"];
}
- (void)setDepthStencilStates:(const id<MTLDepthStencilState> __unsafe_unretained [])states withRange:(NSRange)r
{
    for (NSUInteger k = 0; k < r.length; k++) [self setDepthStencilState:states ? states[k] : nil atIndex:r.location + k];
}
@end

// MTLCommandEncoder barrierAfterQueueStages:beforeStages: orders this queue's earlier work against the stages after
// the barrier. The Vulkan barrier used for every other barrier here orders all stages, which is at least as strong.
@implementation NVMTLComputeCommandEncoder (MTL4Extras)
- (void)barrierAfterQueueStages:(MTLStages)after beforeStages:(MTLStages)before { nvmtl_vk_cmd_barrier(&_cb->_c); }
@end
@implementation NVMTLBlitCommandEncoder (MTL4Barrier)
- (void)barrierAfterQueueStages:(MTLStages)after beforeStages:(MTLStages)before { nvmtl_vk_cmd_barrier(&_cb->_c); }
@end
@implementation NVMTLRenderCommandEncoder (MTL4Extras)
- (void)barrierAfterQueueStages:(MTLStages)after beforeStages:(MTLStages)before { nvmtl_vk_cmd_barrier(&_cb->_c); }
- (void)setColorAttachmentMap:(MTLLogicalToPhysicalColorAttachmentMap *)map
{
    // Measured on an Apple GPU: on a Metal 3 encoder this call is accepted and changes nothing, because a Metal 3 render
    // pipeline is always built with the identity mapping (only Metal 4 pipelines can inherit the encoder's map, see
    // MTL4LogicalToPhysicalColorAttachmentMappingState). The Metal 4 encoder applies the map with
    // nvmtl4_pass_apply_color_map before its pass begins.
    static int said;
    if (map && !said++) nvlog("render encoder setColorAttachmentMap: a Metal 3 pipeline uses the identity mapping, so the map has no effect, as on Apple GPUs (said once)");
}
@end

// ---------------------------------------------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------------------------------------------

@implementation NVMTLDevice (MTL4Extras)

- (id<MTL4CounterHeap>)newCounterHeapWithDescriptor:(MTL4CounterHeapDescriptor *)desc error:(NSError **)error
{
    NSError *e = nil;
    if (!desc || desc.type != MTL4CounterHeapTypeTimestamp)
        e = nvmtl4_heap_error(@"MTLCounterHeapDomain", @"Type must be MTLCounterHeapTypeTimestamp");
    else if (!desc.count)
        e = nvmtl4_heap_error(@"MTLTimestampCounterHeapDomain", @"Heap size cannot be 0");
    else if (desc.count > NVMTL4_COUNTER_HEAP_MAX)
        e = nvmtl4_heap_error(@"MTLTimestampCounterHeapDomain",
            [NSString stringWithFormat:@"Requested Heap size is too large (requested %lu max is %u)", (unsigned long)desc.count, NVMTL4_COUNTER_HEAP_MAX]);
    if (e) {
        nvlog("newCounterHeap: refused - %s", e.localizedDescription.UTF8String);
        if (error) *error = e;
        return nil;
    }
    void *pool = nvmtl_vk_ts_pool_create((uint32_t)desc.count);
    if (!pool) {
        nvlog("newCounterHeap: FAILED - the timestamp query pool could not be made (%lu entries)", (unsigned long)desc.count);
        if (error) *error = nvmtl4_heap_error(@"MTLTimestampCounterHeapDomain", @"The timestamp query pool could not be created");
        return nil;
    }
    NVMTLCounterSampleBuffer *sb = [NVMTLCounterSampleBuffer new];
    sb->_pool = pool; sb->_count = desc.count; sb->_storage = MTLStorageModeShared; sb->_dev = self;
    NVMTL4CounterHeap *h = [NVMTL4CounterHeap new];
    h->_sb = sb; h->_count = desc.count; h->_type = MTL4CounterHeapTypeTimestamp;
    if (error) *error = nil;
    return h;
}

- (NSUInteger)sizeOfCounterHeapEntry:(MTL4CounterHeapType)type
{
    return type == MTL4CounterHeapTypeTimestamp ? sizeof(MTL4TimestampHeapEntry) : 0;
}

- (uint64_t)queryTimestampFrequency
{
    // Heap entries are raw Vulkan timestamp ticks of nvmtl_vk_ts_period_ns() nanoseconds each.
    const double period = nvmtl_vk_ts_period_ns();
    if (period <= 0) {
        static int said; if (!said++) nvlog("queryTimestampFrequency: the GPU has no timestamp queries - 0 (said once)");
        return 0;
    }
    return (uint64_t)llround(1e9 / period);
}

- (MTLSizeAndAlign)tensorSizeAndAlignWithDescriptor:(MTLTensorDescriptor *)desc
{
    // Apple's own answer cannot be measured: its implementation crashes on the M1 in this call. A buffer-backed
    // descriptor (strides set) is sized by its strides, any other is packed; the buffer is 16-byte aligned.
    MTLSizeAndAlign sa = { 0, 0 };
    nvmtl4_parsed P; NSError *e = nil;
    if (!nvmtl4_tensor_parse(desc, desc.strides != nil, &P, &e)) return sa;
    sa.size = (P.L.needBytes > P.L.packedBytes && desc.strides) ? P.L.needBytes : P.L.packedBytes;
    if (P.hasS) sa.size = (sa.size + 127) / 128 * 128 + P.S.needBytes;
    sa.align = MAX(P.L.es, (size_t)16);
    return sa;
}

- (id<MTLTensor>)newTensorWithDescriptor:(MTLTensorDescriptor *)desc error:(NSError **)error
{
    nvmtl4_parsed P; NSError *e = nil;
    if (!nvmtl4_tensor_parse(desc, NO, &P, &e)) { if (error) *error = e; return nil; }
    if (error) *error = nil;
    return nvmtl4_tensor_make(self, desc, &P, nil, 0, nil, 0, NO, error);
}

- (id<MTLTextureViewPool>)newTextureViewPoolWithDescriptor:(MTLResourceViewPoolDescriptor *)desc error:(NSError **)error
{
    if (!desc || !desc.resourceViewCount || desc.resourceViewCount > (1u << 20)) {
        nvlog("newTextureViewPool: refused - %lu views", desc ? (unsigned long)desc.resourceViewCount : 0ul);
        if (error) *error = [NSError errorWithDomain:@"MTLResourceViewPoolDomain" code:1
            userInfo:@{NSLocalizedDescriptionKey: @"Could not create texture view pool"}];
        return nil;
    }
    const uint32_t first = nvmtl_vk_bindless_range((uint32_t)desc.resourceViewCount);
    if (!first) {
        nvlog("newTextureViewPool: FAILED - the bindless heap has no run of %lu slots", (unsigned long)desc.resourceViewCount);
        if (error) *error = [NSError errorWithDomain:@"MTLResourceViewPoolDomain" code:2
            userInfo:@{NSLocalizedDescriptionKey: @"Could not create texture view pool"}];
        return nil;
    }
    NVMTLTextureViewPool *p = [NVMTLTextureViewPool new];
    p->_dev = self; p->_count = desc.resourceViewCount; p->_first = first; p->_slots = [NSMutableArray arrayWithCapacity:p->_count];
    for (NSUInteger i = 0; i < p->_count; i++) [p->_slots addObject:[NSNull null]];
    p->_label = [desc.label copy];
    if (error) *error = nil;
    return p;
}

// Placement sparse buffers and textures need Vulkan sparse residency for buffers and for images on the queue. Without both
// the answer stays no, so callers take their non-sparse path.
- (BOOL)supportsPlacementSparse { return nvmtl_vk_placement_sparse_buffers() && nvmtl_vk_sparse_textures() ? YES : NO; }

- (id<MTLBuffer>)newBufferWithLength:(NSUInteger)length options:(MTLResourceOptions)options placementSparsePageSize:(MTLSparsePageSize)pageSize
{
    // Apple's own behaviour, measured: a page size that is not one of the three sparse sizes gives an ordinary buffer;
    // a sparse buffer must be private storage.
    if (pageSize != MTLSparsePageSize16 && pageSize != MTLSparsePageSize64 && pageSize != MTLSparsePageSize256) return [self newBufferWithLength:length options:options];
    const NSUInteger pageBytes = pageSize == MTLSparsePageSize16 ? 16384 : pageSize == MTLSparsePageSize64 ? 65536 : 262144;
    if (((options >> MTLResourceStorageModeShift) & 0xF) != MTLStorageModePrivate || !length) {
        nvlog("newBufferWithLength:%lu placementSparsePageSize: REFUSED - a placement sparse buffer needs private storage and a length", (unsigned long)length);
        return nil;
    }
    if (!nvmtl_vk_placement_sparse_buffers()) {
        nvlog("newBufferWithLength:%lu placementSparsePageSize: cannot be built - the Vulkan device has no sparse residency for buffers", (unsigned long)length);
        return nil;
    }
    NVMTLBuffer *b = [NVMTLBuffer new];
    size_t granule = 0;
    // 16 KB pages are finer than the 64 KB big pages VRAM normally binds in: NVK's small-page mode makes this buffer
    // bind in 4 KB units (and heaps made for 16 KB pages allocate 4 KB pages, see newHeapWithDescriptor).
    const int was = pageBytes < 65536 ? nvmtl_vk_small_pages(1) : -1;
    const int made = nvmtl_vk_buffer_create_placement_sparse(length, &b->_b, &granule);
    if (was >= 0) nvmtl_vk_small_pages(was);
    if (made) { nvmtl_alloc_failed("newBufferWithLength"); return nil; }
    if (pageBytes % granule) {
        nvlog("newBufferWithLength:%lu placementSparsePageSize: REFUSED - a %lu-byte page is finer than the GPU's %zu-byte binding granule",
              (unsigned long)length, (unsigned long)pageBytes, granule);
        nvmtl_vk_buffer_destroy(&b->_b);
        return nil;
    }
    b->_storage = MTLStorageModePrivate;
    b->_ropt = options & ((MTLResourceOptions)0xF | ((MTLResourceOptions)0x3 << MTLResourceHazardTrackingModeShift));
    NVMTL4SparseState *st = [NVMTL4SparseState new];
    st->pageBytes = pageBytes; st->npages = (length + pageBytes - 1) / pageBytes;
    st->tab = calloc(st->npages, sizeof *st->tab); st->heaps = [NSMutableSet new];
    if (!st->tab) { nvmtl_vk_buffer_destroy(&b->_b); return nil; }
    objc_setAssociatedObject(b, &kNVMTL4SparseState, st, OBJC_ASSOCIATION_RETAIN_NONATOMIC);
    nvlog("newBufferWithLength:%lu placementSparsePageSize:%lu KB -> %p (%lu pages, none mapped)", (unsigned long)length, (unsigned long)pageBytes / 1024, (__bridge void *)b, (unsigned long)st->npages);
    return b;
}

@end


// ---------------------------------------------------------------------------------------------------------------
// macOS 27: tensors over per-plane buffers
// ---------------------------------------------------------------------------------------------------------------


@implementation NVMTLDevice (MTL4Extras27)
- (id)newTensorWithDescriptor:(MTLTensorDescriptor *)desc attachments:(id)att error:(NSError **)error
{
    nvmtl4_parsed P; NSError *e = nil;
    typedef id (*bufFn)(id, SEL, NSInteger); typedef NSUInteger (*offFn)(id, SEL, NSInteger);
    id<MTLBuffer> data = ((bufFn)objc_msgSend)(att, @selector(bufferForPlane:), NVMTL4_PLANE_DATA);
    const NSUInteger off = ((offFn)objc_msgSend)(att, @selector(offsetForPlane:), NVMTL4_PLANE_DATA);
    id<MTLBuffer> sbuf = ((bufFn)objc_msgSend)(att, @selector(bufferForPlane:), NVMTL4_PLANE_SCALES);
    const NSUInteger soff = ((offFn)objc_msgSend)(att, @selector(offsetForPlane:), NVMTL4_PLANE_SCALES);
    if (!nvmtl4_tensor_parse(desc, YES, &P, &e) || !nvmtl4_tensor_check_buffers(desc, &P, data, off, sbuf, soff, &e)) {}
    if (!e && !P.hasS && sbuf) e = nvmtl4_tensor_error(@"a scales buffer was attached but the descriptor has no scales auxiliary plane", NO);
    if (e) { if (error) *error = e; return nil; }
    if (error) *error = nil;
    return nvmtl4_tensor_make(self, desc, &P, data, off, sbuf, soff, YES, error);
}
@end

// Metal 4 color attachment mapping: logical color output L is drawn to physical attachment physical[L]. Every
// per-attachment array of a pass is permuted, so the pass drawn afterwards has the logical outputs in the slots the
// pipeline writes. Call it on the pass state before the pass begins (or after ending the pass and rebuilding with
// load actions that keep the contents). `physical` has NVMTL_NCOL entries, each below NVMTL_NCOL; an out-of-range or
// repeated entry leaves the pass untouched and returns NO.
BOOL nvmtl4_pass_apply_color_map(nvk_pass *p, const uint32_t *physical)
{
    uint32_t seen = 0;
    for (uint32_t l = 0; l < NVMTL_NCOL; l++) {
        if (physical[l] >= NVMTL_NCOL || (seen & (1u << physical[l]))) {
            nvlog("color attachment map: logical %u -> physical %u is outside 0..%d or repeated - REFUSED", l, physical[l], NVMTL_NCOL - 1);
            return NO;
        }
        seen |= 1u << physical[l];
    }
    nvk_pass o = *p;
    uint32_t load = p->load & ~(NVMTL_LOAD_COLOUR(0) | NVMTL_LOAD_COLOUR(1) | NVMTL_LOAD_COLOUR(2) | NVMTL_LOAD_COLOUR(3) |
                                NVMTL_LOAD_COLOUR(4) | NVMTL_LOAD_COLOUR(5) | NVMTL_LOAD_COLOUR(6) | NVMTL_LOAD_COLOUR(7));
    for (uint32_t l = 0; l < NVMTL_NCOL; l++) {
        const uint32_t s = physical[l];
        p->col[l] = o.col[s]; p->colview[l] = o.colview[s]; p->colw[l] = o.colw[s]; p->colh[l] = o.colh[s];
        p->res[l] = o.res[s]; p->resview[l] = o.resview[s]; p->resw[l] = o.resw[s]; p->resh[l] = o.resh[s];
        memcpy(p->clear[l], o.clear[s], sizeof p->clear[l]);
        p->colfmt[l] = o.colfmt[s]; p->resfmt[l] = o.resfmt[s];
        p->col_level[l] = o.col_level[s]; p->col_layer[l] = o.col_layer[s];
        if (o.load & NVMTL_LOAD_COLOUR(s)) load |= NVMTL_LOAD_COLOUR(l);
    }
    p->load = load;
    return YES;
}

// Rebinds the pages of a buffer in `ops` order. Each op is validated against the buffer first; one Vulkan bind carries them all.
static BOOL nvmtl4_sparse_rebind(NVMTLBuffer *b, NVMTL4SparseState *st, const nvmtl4_sparse_page *want, const BOOL *changed)
{
    // Contiguous changed pages that map consecutive memory (or all unmap) become one run.
    NSUInteger nruns = 0; nvmtl_sparse_run *runs = calloc(st->npages, sizeof *runs);
    if (!runs) return NO;
    for (NSUInteger p = 0; p < st->npages; p++) {
        if (!changed[p]) continue;
        const uint64_t at = (uint64_t)p * st->pageBytes;
        const uint64_t size = MIN((uint64_t)st->pageBytes, (uint64_t)b->_b.size - at);
        if (nruns) {
            nvmtl_sparse_run *r = &runs[nruns - 1];
            const BOOL adjacent = r->buf_off + r->size == at;
            if (adjacent && !want[p].mem && !r->mem) { r->size += size; continue; }
            if (adjacent && want[p].mem && r->mem == want[p].mem && r->mem_off + r->size == want[p].off) { r->size += size; continue; }
        }
        // a final partial page is bound whole (the granule is a page); the buffer's size is not past it in Vulkan terms
        runs[nruns++] = (nvmtl_sparse_run){ want[p].mem, want[p].memtype, at, st->pageBytes, want[p].off };
    }
    int rc = nruns ? nvmtl_vk_sparse_buffer_bind(&b->_b, runs, (uint32_t)nruns) : 0;
    free(runs);
    return rc == 0;
}

BOOL nvmtl4_sparse_buffer_update(id<MTLBuffer> buffer, id<MTLHeap> heap, const MTL4UpdateSparseBufferMappingOperation *ops, NSUInteger count)
{
    NVMTLBuffer *b = [(id)buffer isKindOfClass:[NVMTLBuffer class]] ? (NVMTLBuffer *)buffer : nil;
    NVMTL4SparseState *st = b ? objc_getAssociatedObject(b, &kNVMTL4SparseState) : nil;
    if (!st) { nvlog("updateBufferMappings: %s is not a placement sparse buffer this device made - REFUSED", buffer ? object_getClassName(buffer) : "nil"); return NO; }
    NVMTLHeap *h = [(id)heap isKindOfClass:[NVMTLHeap class]] ? (NVMTLHeap *)heap : nil;
    nvmtl4_sparse_page *want = calloc(st->npages, sizeof *want); BOOL *changed = calloc(st->npages, sizeof *changed);
    if (!want || !changed) { free(want); free(changed); return NO; }
    memcpy(want, st->tab, st->npages * sizeof *want);
    BOOL ok = YES;
    for (NSUInteger k = 0; ok && k < count; k++) {
        const MTL4UpdateSparseBufferMappingOperation *op = &ops[k];
        if (op->bufferRange.location > st->npages || op->bufferRange.length > st->npages - op->bufferRange.location) {
            nvlog("updateBufferMappings: op %lu range %lu+%lu is outside the buffer's %lu pages - REFUSED", (unsigned long)k,
                  (unsigned long)op->bufferRange.location, (unsigned long)op->bufferRange.length, (unsigned long)st->npages);
            ok = NO; break;
        }
        const BOOL map = op->mode == MTLSparseTextureMappingModeMap;
        if (map && (!h || h->_type != MTLHeapTypePlacement || !h->_hm.mem)) {
            nvlog("updateBufferMappings: op %lu maps pages but %s - REFUSED", (unsigned long)k, !heap ? "no heap was given" : "the heap is not a placement heap with memory");
            ok = NO; break;
        }
        if (map && ((uint64_t)op->heapOffset + op->bufferRange.length) * st->pageBytes > h->_hm.size) {
            nvlog("updateBufferMappings: op %lu maps heap pages %lu..+%lu past the heap's %zu bytes - REFUSED", (unsigned long)k, (unsigned long)op->heapOffset, (unsigned long)op->bufferRange.length, h->_hm.size);
            ok = NO; break;
        }
        for (NSUInteger i = 0; i < op->bufferRange.length; i++) {
            const NSUInteger p = op->bufferRange.location + i;
            want[p] = map ? (nvmtl4_sparse_page){ h->_hm.mem, h->_hm.memtype, (uint64_t)(op->heapOffset + i) * st->pageBytes } : (nvmtl4_sparse_page){ 0 };
            changed[p] = YES;
        }
    }
    if (ok) ok = nvmtl4_sparse_rebind(b, st, want, changed);
    if (ok) {
        @synchronized (st) {
            memcpy(st->tab, want, st->npages * sizeof *want);
            if (h) [st->heaps addObject:h];   // the memory stays alive for as long as pages of it may be mapped
        }
    } else nvlog("updateBufferMappings: FAILED - the mapping of %p is unchanged", (__bridge void *)buffer);
    free(want); free(changed);
    return ok;
}

BOOL nvmtl4_sparse_buffer_copy(id<MTLBuffer> src, id<MTLBuffer> dst, const MTL4CopySparseBufferMappingOperation *ops, NSUInteger count)
{
    NVMTLBuffer *sb = [(id)src isKindOfClass:[NVMTLBuffer class]] ? (NVMTLBuffer *)src : nil, *db = [(id)dst isKindOfClass:[NVMTLBuffer class]] ? (NVMTLBuffer *)dst : nil;
    NVMTL4SparseState *ss = sb ? objc_getAssociatedObject(sb, &kNVMTL4SparseState) : nil, *ds = db ? objc_getAssociatedObject(db, &kNVMTL4SparseState) : nil;
    if (!ss || !ds) { nvlog("copyBufferMappings: both buffers must be placement sparse buffers this device made - REFUSED"); return NO; }
    if (ss->pageBytes != ds->pageBytes) { nvlog("copyBufferMappings: page sizes differ (%lu vs %lu) - REFUSED", (unsigned long)ss->pageBytes, (unsigned long)ds->pageBytes); return NO; }
    nvmtl4_sparse_page *want = calloc(ds->npages, sizeof *want); BOOL *changed = calloc(ds->npages, sizeof *changed);
    if (!want || !changed) { free(want); free(changed); return NO; }
    memcpy(want, ds->tab, ds->npages * sizeof *want);
    BOOL ok = YES;
    for (NSUInteger k = 0; ok && k < count; k++) {
        const MTL4CopySparseBufferMappingOperation *op = &ops[k];
        if (op->sourceRange.location > ss->npages || op->sourceRange.length > ss->npages - op->sourceRange.location ||
            op->destinationOffset > ds->npages || op->sourceRange.length > ds->npages - op->destinationOffset) {
            nvlog("copyBufferMappings: op %lu source %lu+%lu to %lu is outside the buffers - REFUSED", (unsigned long)k, (unsigned long)op->sourceRange.location,
                  (unsigned long)op->sourceRange.length, (unsigned long)op->destinationOffset);
            ok = NO; break;
        }
        for (NSUInteger i = 0; i < op->sourceRange.length; i++) { want[op->destinationOffset + i] = ss->tab[op->sourceRange.location + i]; changed[op->destinationOffset + i] = YES; }
    }
    if (ok) ok = nvmtl4_sparse_rebind(db, ds, want, changed);
    if (ok) @synchronized (ds) {
        memcpy(ds->tab, want, ds->npages * sizeof *want);
        @synchronized (ss) { for (id h in ss->heaps) [ds->heaps addObject:h]; }
    }
    else nvlog("copyBufferMappings: FAILED - the mapping of %p is unchanged", (__bridge void *)dst);
    free(want); free(changed);
    return ok;
}

// ---------------------------------------------------------------------------------------------------------------
// Tensor arguments in function reflection
// ---------------------------------------------------------------------------------------------------------------

@interface NVMTLTensorArgument : NVMTLArgument <MTLTensorBinding> {
    @public MTLTensorDataType _tdt; MTLDataType _indexType; NSInteger _rank; NSInteger _dims[MTL_TENSOR_MAX_RANK]; BOOL _hasDims;
} @end

@implementation NVMTLTensorArgument
- (BOOL)isArgument { return YES; }
- (MTLTensorDataType)tensorDataType { return _tdt; }
- (MTLDataType)indexType { return _indexType; }
- (MTLTensorExtents *)dimensions { return _hasDims ? nvmtl4_extents_make((NSUInteger)_rank, _dims) : nil; }
- (NSArray *)auxiliaryPlanes { return @[]; }   // macOS 27: a shader parameter carries no auxiliary plane descriptions
@end

static MTLTensorDataType nvmtl4_tensor_dtype_for_name(NSString *n)
{
    static NSDictionary *tab; static dispatch_once_t once;
    dispatch_once(&once, ^{ tab = @{
        @"float": @(MTLTensorDataTypeFloat32), @"half": @(MTLTensorDataTypeFloat16), @"bfloat": @(MTLTensorDataTypeBFloat16),
        @"char": @(MTLTensorDataTypeInt8), @"uchar": @(MTLTensorDataTypeUInt8), @"short": @(MTLTensorDataTypeInt16),
        @"ushort": @(MTLTensorDataTypeUInt16), @"int": @(MTLTensorDataTypeInt32), @"uint": @(MTLTensorDataTypeUInt32),
        @"int8_t": @(MTLTensorDataTypeInt8), @"uint8_t": @(MTLTensorDataTypeUInt8), @"int16_t": @(MTLTensorDataTypeInt16),
        @"uint16_t": @(MTLTensorDataTypeUInt16), @"int32_t": @(MTLTensorDataTypeInt32), @"uint32_t": @(MTLTensorDataTypeUInt32),
        // the packed formats, by the SDK's values (the enum names are newer than the build SDK)
        @"int4b_format": @143, @"uint4b_format": @144, @"int2b_format": @150, @"uint2b_format": @149,
        @"metal_fp4_e2m1_format": @148, @"metal_fp8_e4m3_format": @142, @"metal_fp8_e5m2_format": @141, @"metal_fp8_ue8m0_format": @145 }; });
    NSNumber *v = [n isKindOfClass:[NSString class]] ? tab[n] : nil;
    return v ? (MTLTensorDataType)v.integerValue : MTLTensorDataTypeNone;
}

NVMTLArgument *nvmtl4_tensor_argument(NSDictionary *b, NSDictionary *names, NSUInteger index, BOOL active, MTLBindingAccess access);

// The tensor argument a shader declares, from its AIR type name: tensor<T, dextents<I, rank>> or tensor<T, extents<I, e0,
// e1, ...>> (dynamic extents report -1, as Apple GPUs report them). T may carry an address space and the metal:: of a
// packed format. nil when the name is not a tensor type.
NVMTLArgument *nvmtl4_tensor_argument_from_type(NSString *tn, NSDictionary *names, NSUInteger index, BOOL active)
{
    if (![tn isKindOfClass:[NSString class]] || ![tn hasPrefix:@"tensor<"] || ![tn hasSuffix:@">"]) return nil;
    NSString *body = [tn substringWithRange:NSMakeRange(7, tn.length - 8)];
    // the top-level comma between the element type and the extents
    NSInteger depth = 0, cut = -1;
    for (NSUInteger i = 0; i < body.length && cut < 0; i++) {
        const unichar c = [body characterAtIndex:i];
        if (c == '<') depth++; else if (c == '>') depth--; else if (c == ',' && depth == 0) cut = (NSInteger)i;
    }
    if (cut < 0) return nil;
    NSString *elem = [[body substringToIndex:(NSUInteger)cut] stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
    NSString *ext = [[body substringFromIndex:(NSUInteger)cut + 1] stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
    for (NSString *pfx in @[@"device ", @"constant ", @"threadgroup ", @"const ", @"metal::"])
        if ([elem hasPrefix:pfx]) elem = [elem substringFromIndex:pfx.length];
    elem = [elem stringByReplacingOccurrencesOfString:@"metal::" withString:@""];
    NSMutableDictionary *b = [NSMutableDictionary new];
    b[@"data_type"] = elem;
    BOOL dyn = [ext hasPrefix:@"dextents<"];
    if (!dyn && ![ext hasPrefix:@"extents<"]) return nil;
    NSString *inner = [ext substringWithRange:NSMakeRange(dyn ? 9 : 8, ext.length - (dyn ? 9 : 8) - 1)];
    NSArray<NSString *> *parts = [inner componentsSeparatedByString:@","];
    if (parts.count < 2) return nil;
    b[@"index_type"] = [parts[0] stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
    NSMutableArray *dims = [NSMutableArray new];
    if (dyn) { const NSInteger r = parts[1].integerValue; for (NSInteger i = 0; i < r; i++) [dims addObject:@(-1)]; }
    else for (NSUInteger i = 1; i < parts.count; i++) {
        NSString *v = [parts[i] stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceCharacterSet]];
        [dims addObject:([v rangeOfCharacterFromSet:[[NSCharacterSet decimalDigitCharacterSet] invertedSet]].location == NSNotFound) ? @(v.integerValue) : @(-1)];
    }
    b[@"dimensions"] = dims; b[@"rank"] = @(dims.count);
    // Apple GPUs report every tensor argument as read-write (measured)
    return nvmtl4_tensor_argument(b, names, index, active, MTLBindingAccessReadWrite);
}

NVMTLArgument *nvmtl4_tensor_argument(NSDictionary *b, NSDictionary *names, NSUInteger index, BOOL active, MTLBindingAccess access)
{
    NVMTLTensorArgument *a = [NVMTLTensorArgument new];
    a->_type = (MTLArgumentType)MTLBindingTypeTensor; a->_index = index; a->_active = active; a->_access = access; a->_arrayLength = 1;
    a->_name = names[@"args"][[NSString stringWithFormat:@"tensor:%lu", (unsigned long)index]] ?: [NSString stringWithFormat:@"tensor%lu", (unsigned long)index];
    a->_tdt = nvmtl4_tensor_dtype_for_name(b[@"data_type"]);
    NSString *it = b[@"index_type"];
    a->_indexType = [it isKindOfClass:[NSString class]] ? nvmtl_datatype_for_air_name(it) : MTLDataTypeInt;
    NSArray *dims = [b[@"dimensions"] isKindOfClass:[NSArray class]] ? b[@"dimensions"] : nil;
    const NSInteger rank = [b[@"rank"] respondsToSelector:@selector(integerValue)] ? [b[@"rank"] integerValue] : (NSInteger)dims.count;
    if (dims && rank >= 0 && rank <= MTL_TENSOR_MAX_RANK && (NSInteger)dims.count == rank) {
        a->_hasDims = YES; a->_rank = rank;
        for (NSInteger i = 0; i < rank; i++) a->_dims[i] = [dims[i] respondsToSelector:@selector(integerValue)] ? [dims[i] integerValue] : -1;
    }
    if (a->_tdt == MTLTensorDataTypeNone) nvlog("reflection: tensor argument %lu has data type '%s', which is not a tensor data type here", (unsigned long)index, [b[@"data_type"] description].UTF8String ?: "");
    return a;
}

// ---------------------------------------------------------------------------------------------------------------
// Hooks the Metal 4 command buffer and encoders call (declared in NVMTL4Core.m)
// ---------------------------------------------------------------------------------------------------------------

static BOOL nvmtl4_extras_write_timestamp(id encoder, id<MTLCommandBuffer> m3cb, id heap, NSUInteger index, NSInteger granularity)
{
    return nvmtl4_counter_heap_write((NVMTLCommandBuffer *)m3cb, heap, index, granularity == 1);
}

static BOOL nvmtl4_extras_resolve_heap(id<MTLCommandBuffer> m3cb, id heap, NSRange range, NVMTLBuffer *dst, NSUInteger dstOffset, id<MTLFence> wait, id<MTLFence> update)
{
    return nvmtl4_counter_heap_resolve((NVMTLCommandBuffer *)m3cb, heap, range, dst, dstOffset);
}

// Backing store of any tensor made here, wrapped or not: buffer, byte offset of element zero, shape and strides.
static BOOL nvmtl4_extras_tensor_backing(id tensor, __unsafe_unretained id<MTLBuffer> *buffer, NSUInteger *offset, NSUInteger *rank,
                                         NSInteger *dims, NSInteger *strides, size_t *elementSize)
{
    NVMTLTensor *t = [tensor isKindOfClass:[NVMTLTensor class]] ? tensor : nil;
    if (!t) return NO;
    *buffer = t->_buf; *offset = t->_wrapped ? t->_off : 0; *rank = t->_L.rank; *elementSize = t->_L.es;
    for (NSUInteger i = 0; i < t->_L.rank; i++) { dims[i] = t->_L.d[i]; strides[i] = t->_L.s[i]; }
    return YES;
}

// Queue sparse operations: kind 0 updateTexture, 1 copyTexture, 2 updateBuffer, 3 copyBuffer.
static BOOL nvmtl4_extras_sparse_update(id<MTLCommandBuffer> m3cb, int kind, id resource, id source, id heap, const void *ops, NSUInteger count)
{
    switch (kind) {
        case 0: return nvmtl4_sparse_texture_update(resource, heap, ops, count);
        case 1: return nvmtl4_sparse_texture_copy(source, resource, ops, count);
        case 2: return nvmtl4_sparse_buffer_update(resource, heap, ops, count);
        case 3: return nvmtl4_sparse_buffer_copy(source, resource, ops, count);
        default: nvlog("sparse update: kind %d is unknown - REFUSED", kind); return NO;
    }
}

// The Metal 4 core declares the hook pointers; its integration defines NVMTL4_CORE_HOOKS before this file so they are set at load.
#ifdef NVMTL4_CORE_HOOKS
__attribute__((constructor)) static void nvmtl4_extras_register_hooks(void)
{
    nvmtl4_hook_write_timestamp = nvmtl4_extras_write_timestamp;
    nvmtl4_hook_resolve_heap = nvmtl4_extras_resolve_heap;
    nvmtl4_hook_tensor_backing = nvmtl4_extras_tensor_backing;
    nvmtl4_hook_sparse_update = nvmtl4_extras_sparse_update;
}
#endif

// A texture descriptor that asks for placement sparse pages is refused, never answered with an ordinary texture.
BOOL nvmtl4_texture_refused_sparse(MTLTextureDescriptor *d, const char *who)
{
    if (!d || ![d respondsToSelector:@selector(placementSparsePageSize)]) return NO;
    const NSInteger ps = ((NSInteger (*)(id, SEL))objc_msgSend)(d, @selector(placementSparsePageSize));
    if (ps != MTLSparsePageSize16 && ps != MTLSparsePageSize64 && ps != MTLSparsePageSize256) return NO;
    nvlog("%s: placementSparsePageSize %ld is unsupported - placement sparse textures are not built, texture refused", who, (long)ps);
    return YES;
}

#pragma clang diagnostic pop
