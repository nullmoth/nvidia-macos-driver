/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

#pragma once
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include "nvmtl_vk.h"
#include "nvmtl_multiview.h"
#include "nvmtl_caps_query.h"
#include "nvmtl_sparse_query.h"
#include "nvmtl_sparse_map.h"
#include "nvmtl_counter_heap.h"
#include "nvmtl_icb_range.h"
#include "nvmtl_yuv422.h"
#include "nvmtl_cmd_refusal.h"

static inline NSMutableData *nvmtl_fc_storage(NSUInteger count) {
    const NSUInteger stride = 2 * sizeof(uint32_t) + sizeof(void *) + 16;
    if (!count || count > NSUIntegerMax / stride) return nil;
    return [NSMutableData dataWithLength:count * stride];
}
@class NVMTLHeap;
@interface NVMTLBuffer : NSObject <MTLBuffer> { @public nvk_buffer _b; MTLStorageMode _storage; NVMTLHeap *_heap; NSUInteger _heapOffset;
    void *_hostPtr; size_t _hostLen; id _hostDealloc;
    NVMTLHeap *_subHeap; NSUInteger _subCharge; BOOL _aliased;
    nvk_buffer _shadow; int _syncOut; int _zshadow;
    void *_swc;   /* nvmtl_swc*: this Shared buffer has a VRAM working copy (_b) and its system-memory side in _shadow */
    NSUInteger _dirtyLo, _dirtyHi; } @end
#include <sched.h>
static inline void nvmtl_buf_settle(NVMTLBuffer *b) {
    if (!__atomic_load_n(&b->_zshadow, __ATOMIC_ACQUIRE)) return;
    int owed = 1;
    if (__atomic_compare_exchange_n(&b->_zshadow, &owed, 2, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
        if (b->_shadow.map) memset(b->_shadow.map, 0, b->_shadow.size);
        __atomic_store_n(&b->_zshadow, 0, __ATOMIC_RELEASE); return; }
    while (__atomic_load_n(&b->_zshadow, __ATOMIC_ACQUIRE) == 2) sched_yield();
}
/* a buffer with a working copy and a caller pointer (_swc and _hostPtr): the caller's pages are what the CPU sees and _shadow is only the staging for the transfers */
static inline void *nvmtl_buf_cpu(NVMTLBuffer *b) { nvmtl_buf_settle(b); if (b->_swc && b->_hostPtr) return b->_hostPtr; return b->_shadow.map ? b->_shadow.map : b->_b.map; }
@interface NVMTLTexture : NSObject <MTLTexture> {
  @public nvk_image _i; nvk_queue *_q; IOSurfaceRef _surf; MTLPixelFormat _fmt; NSUInteger _plane;
  NVMTLHeap *_heap; NSUInteger _heapOffset;
  NVMTLHeap *_subHeap; NSUInteger _subCharge;
  BOOL _aliased;
  NVMTLTexture *_parent;
  void *_sampledDepthView;
  void *_ownView;
  uint32_t _mips;
  NVMTLTexture *_viewParent;
  uint32_t _viewType, _viewSlices, _relativeLevel, _relativeSlice;
  uint32_t _baseSlice, _baseLevel;
  uint32_t _surfSeed, _surfGen; BOOL _surfSynced, _surfOwned;
  nvk_buffer _vramBuf; BOOL _vramOn;
  nvk_buffer _hostPages;   // this surface's pages imported for the host copies (nvmtl_host_pages), kept while they hold
  BOOL _stex;
  uint64_t _resid;
  NSUInteger _usage;
  NVMTLBuffer *_backBuf; NSUInteger _backOff, _backBPR;
}
- (nvk_image *)nvi;
- (void *)nvview;
- (void *)nvsampleview;
- (void)nvmtlSurfaceIn;
- (BOOL)nvmtlSurfaceOut;
- (BOOL)nvmtlSurfaceOutDirtied:(BOOL)dirtied;
@end
@interface NVMTLFunction : NSObject <MTLFunction> { @public NSData *_spirv; NSString *_fname; NSString *_stage; NSString *_air; NSDictionary *_fc; NSArray *_embedded; unsigned char _bcHash[32]; BOOL _bcHashed;
  NSDictionary *_refl; NSDictionary *_airNames; NSArray *_arguments;
  id _lib; id _fcv; NSString *_specName; BOOL _needsLink; } @end
NSDictionary *nvmtl_function_reflection(NVMTLFunction *function);
NSDictionary *nvmtl_air_names(NVMTLFunction *function);
uint64_t nvmtl_resid_for_texture(id t);
void nvmtl_resid_texture_gone(uint64_t token);
uint64_t nvmtl_resid_for_sampler(id s);
@interface NVMTLType : NSObject { @public MTLDataType _dataType; } @end
@interface NVMTLVertexAttribute : NSObject { @public NSString *_name; NSUInteger _index; MTLDataType _dataType; } @end
@interface NVMTLTextureReferenceType : NVMTLType { @public MTLDataType _texDataType; MTLTextureType _texType; MTLBindingAccess _access; BOOL _depth; } @end
@interface NVMTLStructType : NVMTLType { @public NSArray *_members; } @end
@interface NVMTLPointerType : NVMTLType { @public MTLDataType _elem; MTLBindingAccess _access; NSUInteger _align, _size; BOOL _isArgBuf; NVMTLStructType *_elemStruct; } @end
// an array member of a reflected struct (MTLArrayType): element type, length and stride from the AIR struct_type_info
@interface NVMTLArrayType : NVMTLType { @public MTLDataType _elem; NSUInteger _len, _stride; } @end
@interface NVMTLStructMember : NSObject { @public NSString *_name; NSUInteger _offset; MTLDataType _dataType; NSUInteger _argIndex; NVMTLArrayType *_arr;
  NVMTLStructType *_struct; NVMTLTextureReferenceType *_texRef; NVMTLPointerType *_ptr; } @end
// _type holds MTLArgumentType values in its underlying NSUInteger; MTLBindingType, the replacement, numbers them the same
@interface NVMTLArgument : NSObject { @public NSString *_name; NSUInteger _type; MTLBindingAccess _access; NSUInteger _index; BOOL _active;
  NSUInteger _arrayLength, _bufAlign, _bufSize; MTLDataType _bufDataType; NVMTLStructType *_bufStruct; NVMTLPointerType *_bufPtr;
  MTLTextureType _texType; MTLDataType _texDataType; BOOL _depth; } @end
@interface NVMTLRenderPipelineReflection : NSObject { @public NSArray *_v, *_f, *_o, *_m; } @end
@interface NVMTLComputePipelineReflection : NSObject { @public NSArray *_a; } @end
NSArray<NVMTLArgument *> *nvmtl_arguments_for_function(NVMTLFunction *fn);
NVMTLArgument *nvmtl_argument_for_buffer(NVMTLFunction *fn, NSUInteger index);
id nvmtl_render_reflection(MTLRenderPipelineDescriptor *d);
id nvmtl_compute_reflection(id<MTLFunction> fn);
id<MTLFunction> nvmtl_link_kernel(MTLComputePipelineDescriptor *d, NSError **err);
NVMTLFunction *nvmtl_kernel_with_stage_in(NVMTLFunction *fn, MTLStageInputOutputDescriptor *sd, NSError **err);
id<MTLFunction> nvmtl_link_stage(id<MTLFunction> k, MTLLinkedFunctions *lf, NSArray *dylibs, NSError **err);
void nvmtl_render_keep_linked(id ps, id<MTLFunction> vf, id<MTLFunction> ff);
void nvmtl_note_compile_dylibs(id lib, NSArray *libraries);
id nvmtl_new_dylib(id<MTLLibrary> lib, NSError **err);
id nvmtl_new_dylib_url(NSURL *url, NSError **err);
id nvmtl_new_binary_archive(MTLBinaryArchiveDescriptor *d, NSError **err);
NSString *nvmtl_archive_refusal(NSArray *archives, NSString *key);
NSString *nvmtl_archive_key_compute(id<MTLFunction> f);
NSString *nvmtl_archive_key_render(MTLRenderPipelineDescriptor *d);
static inline uint32_t nvmtl_vk_layers_for_desc(uint32_t t, uint32_t n, uint32_t depth) { return t == 7 ? (depth ? depth : 1u) : nvmtl_vk_layers_for_type(t, n); }

@interface NVMTLArgumentEncoder : NSObject <MTLArgumentEncoder> {
  @public NSArray<NSDictionary *> *_fields; NSDictionary *_fieldByIndex;
  int64_t *_offAt; NSUInteger _offN;
  NVMTLBuffer *_dst; NSUInteger _dstOffset; NSString *_label;
  NSUInteger _airSize, _airAlign;
} @end
extern id gNVMTLMainDevice;
@interface NVMTLLibrary : NSObject <MTLLibrary> { @public NSDictionary<NSString *, NSData *> *_fns; NSMutableDictionary<NSString *, NSData *> *_compiledFns; NSDictionary<NSString *, NSString *> *_stages, *_airs; NSArray<NSString *> *_externs;
  NSData *_raw; id _appleTwin; BOOL _twinTried; NSString *_rawPath; } @end
@interface NVMTLRenderPipelineState : NSObject <MTLRenderPipelineState> { @public nvk_pipeline _p;
  NSString *_vname, *_fname, *_blend, *_vdesc; MTLRenderPipelineDescriptor *_descriptor; NSMutableDictionary *_samplerVariants;
  NVMTLFunction *_linkedVert, *_linkedFrag; uint32_t _a8mask; } @end
@interface NVMTLCommandQueue : NSObject <MTLCommandQueue> {
  // _resSets: the residency sets this queue was given. A set retains its allocations, so holding the
  // set is what keeps them alive; without this nobody did, and an app that handed its only reference
  // to the queue had every resource in the set freed under work the queue had not finished.
  @public nvk_queue _q; id _dev; id _completionQueue; dispatch_queue_t _callbackOrderQueue; NSMutableArray *_reservations; dispatch_semaphore_t _capacity; NSUInteger _capacityCount; NSMutableArray *_resSets;
} @end
@interface NVMTLCommandBuffer : NSObject <MTLCommandBuffer> {
  NSError *_cbError;
  NSMutableDictionary *_userDictionary;
  @public nvk_cmdbuf _c; NVMTLCommandQueue *_queue; BOOL _committed, _commitMarking, _partsRetiring; BOOL _capacityHeld; MTLCommandBufferStatus _cbstatus;
  NVMTLTexture *_present;
  NSMutableArray *_presentations;
  NSMutableArray *_handlers;
  NSMutableArray *_scratch;
  NSMutableSet *_resources;
  NSMutableSet *_surfDirty;
  NSMutableSet *_surfOverwrites; // contents defined by writes recorded in this command buffer
  unsigned _nEnc, _nEnd, _nHandlers, _nStatus, _nEnqueue, _nLabel, _nPresent, _nWait;
  unsigned _nEncSealed;
  CFTimeInterval _gpuStart, _gpuEnd;
  CFTimeInterval _legacyKernelStart, _legacyKernelEnd;
  uint64_t _cbtSeq, _cbtCreate, _cbtCommit, _cbtWait0; unsigned _cbtND; char _cbtK[40];   // NVMTL_CB_TIME (dev)
  nvmtl_gpu_interval _gpuTiming; BOOL _gpuTimingSeen, _gpuTimingInvalid;
  NSMutableArray *_parts, *_scheduledHandlers;
  NSUInteger _nextPart;
  NSUInteger _nextSubmit;
  id _reservation;
  NSCondition *_completionCondition;
  dispatch_group_t _callbackGroup, _scheduledGroup;
  BOOL _wasScheduled, _executionDone, _completionReady, _scheduleAfterPublish;
  NSMutableArray *_signals;
  NSMutableArray *_waits;
}
- (void)nvmtlReleaseCapacity;
- (void)nvmtlRecordEncodingError:(NSError *)error;
@end
// implemented by the EventSubmission category in NVMTLEventSync.m, so declared on that category rather than on the class
@interface NVMTLCommandBuffer (EventSubmission)
- (void)nvmtlReserveQueue;
- (void)nvmtlReleaseReservation;
- (void)nvmtlRecordWait:(id)event value:(uint64_t)value;
- (void)nvmtlRecordSignal:(id)event value:(uint64_t)value;
- (void)nvmtlSubmitWithEvents;
- (BOOL)nvmtlAdvancePart;
- (BOOL)nvmtlSubmitAhead;
- (void)nvmtlFinishCallbacks;
@end
@interface NVMTLDepthStencilState : NSObject <MTLDepthStencilState> { @public int _write; int _compare;
  int _stencil; uint32_t _sf[6], _sb[6]; } @end
@interface NVMTLSamplerState : NSObject <MTLSamplerState> { @public nvk_sampler _s; NSDictionary *_pixelState; uint64_t _resid; } @end
@interface NVMTLFence : NSObject <MTLFence> { @public id<MTLDevice> _dev; NSString *_label; } @end
@interface NVMTLResidencySet : NSObject <MTLResidencySet> { @public id<MTLDevice> _dev; NSString *_label; NSMutableArray *_allocs; } @end
@interface NVMTLEvent : NSObject <MTLEvent> { @public id<MTLDevice> _dev; NSString *_label; uint64_t _value; NSCondition *_condition; } @end
@interface NVMTLSharedEvent : NSObject <MTLSharedEvent> { @public id<MTLDevice> _dev; NSString *_label; uint64_t _value; NSCondition *_condition; NSMutableArray *_notifications; } @end
@interface NVMTLHeap : NSObject <MTLHeap> {
  @public id<MTLDevice> _dev; NSUInteger _size; NSUInteger _used;
  MTLStorageMode _storage; MTLCPUCacheMode _cache; NSString *_label;
  nvk_heap _hm; MTLHeapType _type;
}
- (void)nvmtlReleaseSubAllocation:(NSUInteger)bytes;
- (void)nvmtlGiveRange:(NSUInteger)off length:(NSUInteger)len;
@end
@interface NVMTLComputePipelineState : NSObject <MTLComputePipelineState> { @public nvk_pipeline _p; NVMTLFunction *_function; NSMutableDictionary *_samplerVariants;
    BOOL _icb;
} @end
@interface NVMTLIndirectCommand : NSObject <MTLIndirectRenderCommand, MTLIndirectComputeCommand> {
  @public id _ps; NSMutableArray *_vbufs; NSMutableArray *_fbufs;
  MTLPrimitiveType _type; NSUInteger _start, _count, _instances, _baseInstance;
  id _ibuf; NSUInteger _ioff; MTLIndexType _itype; NSInteger _baseVertex;
  BOOL _indexed, _armed;
  MTLCullMode _cull; BOOL _hasCull;
  MTLWinding _winding; BOOL _hasWinding;
  MTLTriangleFillMode _fill; BOOL _hasFill;
  MTLDepthClipMode _clip; BOOL _hasClip;
  id _ds; float _bias, _slope, _clamp; BOOL _hasBias;
  id _cps; NSMutableArray *_kbufs; MTLSize _grid, _tg; BOOL _isCompute;
} @end
@interface NVMTLIndirectCommandBuffer : NSObject <MTLIndirectCommandBuffer> {
  @public NSMutableArray<NVMTLIndirectCommand *> *_cmds; id<MTLDevice> _dev; NSString *_label;
} @end
@class NVMTLBuffer;
@interface NVMTLIndirectCommandBuffer () { @public NVMTLBuffer *_recBuf; } @end   // GPU-encoded commands (NVMTL_GPU_ICB=1): the record array a kernel writes
// the pipelines made with supportIndirectCommandBuffers: the candidates a GPU-written command can name
void nvmtl_icb_register_pipeline(id ps, BOOL compute);
NSArray *nvmtl_icb_pipelines(BOOL compute);
int nvmtl_gpu_icb_on(void);
#ifndef NVMTL_NBUFAT
#define NVMTL_NBUFAT 32   /* buffer argument indices an encoder tracks (Metal has 31) */
#endif
@interface NVMTLComputeCommandEncoder : NSObject <MTLComputeCommandEncoder> {
  @public NVMTLCommandBuffer *_cb; NVMTLComputePipelineState *_ps; NSMutableArray *_scratch; NSMutableDictionary *_samplers;
  NSMutableDictionary *_tgLen;
  // The binding table, by Metal buffer index: the buffer, its offset and, for setBytes, the inline length and base.
  // Plain arrays: WAS an NSMutableDictionary of NSArrays, three allocations on every setBuffer (a large part of the
  // ~2.5 us of host time per dispatch against Apple's ~0.3 us). _baN: 0 unbound, 2 = buffer+offset, 4 = setBytes form.
  __strong id _baObj[NVMTL_NBUFAT]; NSUInteger _baOff[NVMTL_NBUFAT], _baLen[NVMTL_NBUFAT], _baBase[NVMTL_NBUFAT];
  uint8_t _baN[NVMTL_NBUFAT];
  NSMutableSet *_resources;
  NSMutableDictionary *_sassTextures, *_sassSamplers;   // by Metal index, for kernels run from machine code
} @end
@interface NVMTLBlitCommandEncoder : NSObject <MTLBlitCommandEncoder> { @public NVMTLCommandBuffer *_cb; __weak NVMTLCommandBuffer *_owner; } @end
@interface NVMTLAccelerationStructure : NVMTLBuffer <MTLAccelerationStructure> { @public nvk_accel _as; nvk_buffer _uid; nvk_buffer _scratch; uint32_t _iftAdd; } @end
@interface NVMTLAccelerationStructureCommandEncoder : NSObject <MTLAccelerationStructureCommandEncoder> { @public NVMTLCommandBuffer *_cb; __weak NVMTLCommandBuffer *_owner; } @end
uint32_t nvmtl_geoms_from_descriptor(id desc, nvmtl_geom *out, uint32_t max, int *instance, uint32_t *instance_count, NSMutableSet *track);
#define NVMTL_NBUFAT 32   /* buffer argument indices an encoder tracks (Metal has 31) */
@interface NVMTLRenderCommandEncoder : NSObject <MTLRenderCommandEncoder> {
  @public NVMTLCommandBuffer *_cb; NVMTLTexture *_target; float _clear[4]; BOOL _begun;
  NVMTLTexture *_resolve; uint32_t _passSamples;
  int _cull; uint32_t _passFmt; int _passDepth;
  uint32_t _load; BOOL _hasVP, _hasSC; float _vp[6]; uint32_t _sc[4];
  NSMutableDictionary *_vertexSamplers, *_fragmentSamplers;
  NSData *_pixelSamplerKey; // sampler specialization depends on values, not the draw or pipeline
  NSMutableArray *_pixelSamplerKeys; // at most eight recent value snapshots, owned by this encoder
  NVMTLRenderPipelineState *_svFor; NSData *_svKey; id _svSel; // the last sampler-variant choice of this encoder (NVMTL_NO_SV_MEMO)
  // the buffer and offset bound at each vertex/fragment index, read by embedded argument binding; plain arrays because
  // Apple's GL rebinds buffers for every draw, and a dictionary of (buffer, offset) arrays allocated on each one
  NVMTLBuffer *_vbufAt[NVMTL_NBUFAT], *_fbufAt[NVMTL_NBUFAT]; NSUInteger _vbufOff[NVMTL_NBUFAT], _fbufOff[NVMTL_NBUFAT];
  NVMTLRenderPipelineState *_ps;
  NSMutableArray *_scratch;
  NVMTLDepthStencilState *_ds;
  NVMTLBuffer *_tfb; NSUInteger _tfbOff, _tfbStride; float _tfScale; BOOL _tfScaleSet;
  NVMTLBuffer *_visBuf;
  uint32_t _visSlot; NSUInteger _visOffset;
  BOOL _visActive, _visPending;
  NSMutableArray *_visRecords;
  nvk_pass _pass; nvk_rtsig _passSig; BOOL _hasRef, _refused;
} @end
@interface NVMTLRenderCommandEncoder ()
// this class's own implementation, which the deferred encoder replays a recorded two-argument call into
- (void)useResource:(id<MTLResource>)r usage:(MTLResourceUsage)u;
- (BOOL)nvmtlBuildPass:(MTLRenderPassDescriptor *)d;
- (BOOL)nvmtlBeginPassLoad:(uint32_t)load;
- (void)nvmtlApplyDepthStencil;
@end

NSArray<NSDictionary *> *nvmtl_embedded_bindings(NVMTLFunction *function);
void nvmtl_bind_embedded(NVMTLCommandBuffer *cb, uint32_t set, NVMTLFunction *fn, NVMTLBuffer *__strong const *bufAt, const NSUInteger *offAt);
void nvmtl_bind_embedded_enc(NVMTLCommandBuffer *cb, uint32_t set, NVMTLFunction *fn, NVMTLComputeCommandEncoder *e);
// the NSArray form of one binding ({buffer, offset} or {buffer, offset, length, base}), for the few readers that want it
static inline NSArray *nvmtl_bufat(NVMTLComputeCommandEncoder *e, NSUInteger i) {
    if (!e || i >= NVMTL_NBUFAT || !e->_baN[i] || !e->_baObj[i]) return nil;
    return e->_baN[i] == 4 ? @[e->_baObj[i], @(e->_baOff[i]), @(e->_baLen[i]), @(e->_baBase[i])] : @[e->_baObj[i], @(e->_baOff[i])];
}
uint64_t nvmtl_resid_for_texture(id t);
uint64_t nvmtl_resid_for_sampler(id s);

@interface NVMTLParallelRenderCommandEncoder : NSObject <MTLParallelRenderCommandEncoder> { @public NVMTLCommandBuffer *_cb; MTLRenderPassDescriptor *_desc;
  NSMutableArray *_subs; NSLock *_lock; BOOL _ended; NSString *_label; } @end

@interface NVMTLCounter : NSObject <MTLCounter> @end
@interface NVMTLCounterSet : NSObject <MTLCounterSet> { @public NSArray *_counters; } @end
@interface NVMTLCounterSampleBuffer : NSObject <MTLCounterSampleBuffer> {
  @public void *_pool; NSUInteger _count; MTLStorageMode _storage; NSString *_label; id<MTLDevice> _dev; } @end
id<MTLCounterSet> nvmtl_timestamp_counter_set(void);

@interface NVMTLBuffer () { @public NSString *_label; MTLResourceOptions _ropt;
  int _resRefs, _rstate, _lruReg, _wantOn; uint64_t _lastUse;
  uint64_t _useEp;
  BOOL _texSeen;
} @end
@interface NVMTLTexture () { @public NSString *_label; MTLResourceOptions _ropt; BOOL _shareable; BOOL _fbo; BOOL _drawable;
  MTLTextureSwizzleChannels _swz; BOOL _hasSwz; void *_swzView;
  void *_tbView, *_tbAlias; BOOL _tbStorage;
  int _resRefs;
  int _nviews;
  int _rstate;
  uint64_t _lastUse;
  int _lruReg, _wantOn;
  uint64_t _useEp;
} @end
void nvmtl_texture_desc_swizzle(NVMTLTexture *t, MTLTextureDescriptor *d);
#include <os/lock.h>
@interface NVMTLResSet : NSMutableSet { @public NSMutableSet *_s; os_unfair_lock _lk; const void *_mru[8]; unsigned _mruNext; }
- (NVMTLResSet *)nvmtlTakeAll;
@end
@interface NVMTLFunction () { @public NSString *_label; } @end
// machine code compiled ahead of time for this function (NVMTLSass.m), nil otherwise
@interface NVMTLFunction () { @public id _sassKernel; } @end
@interface NVMTLLibrary () { @public NSString *_label; } @end
@interface NVMTLCommandQueue () { @public NSString *_label; } @end
@interface NVMTLCommandBuffer () { @public NSString *_label; uint64_t _rep; } @end
@interface NVMTLCommandBuffer () { @public NSMutableSet *_stTwins; } @end
@interface NVMTLTexture () { @public nvk_image _stTwin; BOOL _stTwinTried; BOOL _mipsWritten; } @end
@interface NVMTLDepthStencilState () { @public NSString *_label; } @end
@interface NVMTLSamplerState () { @public NSString *_label; } @end
@interface NVMTLComputePipelineState () { @public NSString *_label; } @end
@interface NVMTLComputePipelineState () { @public uint64_t _resid; NSUInteger _attrT, _askT; } @end
// _sassKernel: machine code that runs before the Vulkan pipeline; _sassOnly: there is no Vulkan pipeline
@interface NVMTLComputePipelineState () { @public id _sassKernel; BOOL _sassOnly; } @end
@interface NVMTLRenderPipelineState () { @public uint64_t _resid; } @end
// macOS 26 required threadgroup sizes, as the pipeline descriptor gave them (0x0x0 = not set). Apps read them back from
// the pipeline; without the getters the read is an unrecognized selector and the app stops.
@interface NVMTLRenderPipelineState () { @public MTLSize _reqObj, _reqMesh; } @end
@interface NVMTLComputePipelineState () { @public MTLSize _reqTG; } @end
// A descriptor's MTLSize property that exists only on macOS 26 and later: read by key so the plugin, built for macOS 15,
// needs no availability check and gets 0x0x0 on an older system that has no such property.
static inline MTLSize nvmtl_desc_size(id d, SEL sel) {
    MTLSize s = { 0, 0, 0 };
    if (!d || ![d respondsToSelector:sel]) return s;
    id v = [d valueForKey:NSStringFromSelector(sel)];
    if ([v isKindOfClass:[NSValue class]] && !strcmp([(NSValue *)v objCType], @encode(MTLSize))) [(NSValue *)v getValue:&s size:sizeof s];
    return s;
}
@interface NVMTLIndirectCommandBuffer () { @public uint64_t _resid; } @end
@interface NVMTLIndirectCommand () - (void)nvmtlCopyFrom:(NVMTLIndirectCommand *)o; @end
NSUInteger nvmtl_air_max_tg(NVMTLFunction *fn);
BOOL nvmtl_air_grid_only(NVMTLFunction *fn);
// _gridOnly: 0 not yet asked, 1 the kernel's result does not depend on its threadgroup shape, -1 it may
@interface NVMTLComputePipelineState () { @public int8_t _gridOnly; } @end
@interface NVMTLRenderCommandEncoder () { @public NSString *_label; } @end
@interface NVMTLRenderCommandEncoder () { @public BOOL _dbOn; float _dbLo, _dbHi; } @end   // setDepthTestMinBound:maxBound:
@interface NVMTLRenderCommandEncoder () { @public uint32_t _vstride[NVMTL_NVIN]; uint32_t _vstrideSet; } @end
@interface NVMTLTexture () { @public uint32_t _y422; NSMutableData *_y422Raw; BOOL _y422GpuWritten; IOSurfaceRef _y422Surf; NSUInteger _y422Plane; uint32_t _y422Seed; BOOL _y422Synced; }
- (void)nvmtlY422SurfaceIn;
- (void)nvmtlY422Upload:(uint32_t)y0 rows:(uint32_t)n;
@end   // GBGR422 / BGRG422: _y422 is the Metal format, _y422Raw the packed bytes
@interface NVMTLRenderCommandEncoder () { @public uint32_t _ampCount; const char *_ampWhy; } @end   // setVertexAmplificationCount:viewMappings: (0 = never set = 1 view)
@interface NVMTLComputeCommandEncoder () { @public NSString *_label; } @end
@interface NVMTLBlitCommandEncoder () { @public NSString *_label; } @end
@interface NVMTLAccelerationStructureCommandEncoder () { @public NSString *_label; } @end
@interface NVMTLHeap () { @public MTLHazardTrackingMode _hazard; } @end
// hazard model (nvmtl_hazard.h): which buffer slots and textures each stage of a pipeline writes, from its reflection
@interface NVMTLRenderPipelineState () { @public int _hzReady; uint32_t _hzBuf[2]; uint64_t _hzTex[2][2]; } @end
@interface NVMTLComputePipelineState () { @public int _hzReady; uint32_t _hzBuf; uint64_t _hzTex[2]; } @end
@interface NVMTLIndirectCommandBuffer () { @public MTLResourceOptions _opts; } @end
@interface NVMTLBuffer () { @public uint32_t _purge; } @end
@interface NVMTLTexture () { @public uint32_t _purge; } @end
@interface NVMTLTexture () { @public BOOL _rbSwap; } @end   // a writable BGR10A2Unorm held as R10G10B10A2 (NVMTLDevice.m)
@interface NVMTLHeap () { @public uint32_t _purge; } @end
#include <os/lock.h>
@interface NVMTLHeap () { @public NSUInteger *_fr; uint32_t _nfr, _cfr; os_unfair_lock _fl; BOOL _backed; } @end
@interface NVMTLBuffer () { @public BOOL _subPlaced; } @end
typedef struct { nvk_image img; uint32_t key[12]; NSUInteger off, len; } nvmtl_park_t;
@interface NVMTLTexture () { @public BOOL _subPlaced, _parkable; uint32_t _pkey[12]; NSUInteger _pkLen; } @end
@interface NVMTLTexture () { @public BOOL _clientWritten; } @end
@interface NVMTLCommandBuffer () { @public NSDictionary *_busyMarks; } @end
@interface NVMTLHeap () { @public nvmtl_park_t *_pk; uint32_t _npk; } @end
// Lazy backing: _lzc = slot size (0 = the heap owns one _hm of its full size). _lzo[slot] = 1 + index into _lzm/_lzb (0 = no
// memory yet); one backing may cover several slots (a resource larger than a slot gets one backing for all of them).
@interface NVMTLHeap () { @public size_t _lzc; uint32_t _lzn, _lzcap, _lzslots; nvk_heap *_lzm; size_t *_lzb; uint16_t *_lzo;
                         os_unfair_lock _lzl; } @end
@interface NVMTLBuffer () { @public nvmtl_park_t *_bpk; uint32_t _nbpk; os_unfair_lock _bpl; } @end
// What this buffer last put in the Metal 4 address table: base, end, and the table epoch at the time. -gpuAddress
// skips the table (one global lock) while all three still hold - see nvmtl_m4_note_current.
@interface NVMTLBuffer () { @public _Atomic uint64_t _m4note[3]; } @end
@interface NVMTLTexture () { @public BOOL _bparkable; } @end
static void nvmtl_heap_park(NVMTLHeap *h, nvk_image *img, const uint32_t *key, NSUInteger off, NSUInteger len);
void nvmtl_heap_back(NVMTLHeap *h, NSUInteger size, int shared);
@interface NVMTLIndirectCommandBuffer () { @public uint32_t _purge; } @end
static inline MTLPurgeableState nvmtl_purge(uint32_t *st, MTLPurgeableState s) {
    if (s < MTLPurgeableStateKeepCurrent || s > MTLPurgeableStateEmpty) {
        static int said; if (!said++) nvlog("setPurgeableState: %lu is no MTLPurgeableState - answered as KeepCurrent", (unsigned long)s);
        s = MTLPurgeableStateKeepCurrent;
    }
    uint32_t prev = s == MTLPurgeableStateKeepCurrent ? __atomic_load_n(st, __ATOMIC_ACQUIRE) : __atomic_exchange_n(st, (uint32_t)s, __ATOMIC_ACQ_REL);
    return prev ? (MTLPurgeableState)prev : MTLPurgeableStateNonVolatile;
}
static inline kern_return_t nvmtl_owner_check(task_id_token_t tok) {
    mach_port_t p = MACH_PORT_NULL;
    if (task_identity_token_get_task_port(tok, TASK_FLAVOR_NAME, &p) != KERN_SUCCESS) return kIOReturnBadArgument;
    mach_port_deallocate(mach_task_self(), p);
    return KERN_SUCCESS;
}
static inline MTLCPUCacheMode nvmtl_res_cache(NVMTLHeap *hp, MTLResourceOptions o) { return hp ? hp->_cache : (MTLCPUCacheMode)(o & 0xF); }
static inline MTLHazardTrackingMode nvmtl_res_hazard(NVMTLHeap *hp, MTLResourceOptions o) {
    if (hp) return hp->_hazard == MTLHazardTrackingModeTracked ? MTLHazardTrackingModeTracked : MTLHazardTrackingModeUntracked;
    return ((o >> MTLResourceHazardTrackingModeShift) & 0x3) == MTLHazardTrackingModeUntracked ? MTLHazardTrackingModeUntracked : MTLHazardTrackingModeTracked;
}
extern unsigned nvmtl_apps1_resolve_miss, nvmtl_apps1_resolve_extra, nvmtl_apps1_ds_resolve_miss;
@interface MTLSharedTextureHandle (NVMTLSPI)
- (instancetype)initWithIOSurface:(IOSurfaceRef)surface label:(NSString *)label;
- (IOSurfaceRef)ioSurface;
@end
@interface MTLIOAccelDevice (NVMTLApps1)
- (id<MTLTexture>)newSharedTextureWithHandle:(MTLSharedTextureHandle *)handle;
@end

NSDictionary *nvmtl_index_argument_fields(NSArray *fields);
// a blit that names a 4:2:2 texture is refused (its packed bytes are not the image's bytes); 1 = refused
int nvmtl_y422_blit_refused(id tex, const char *what);
#include "nvmtl_icb_record.h"
// tile shading (NVMTL_TILE_SHADING=1): a tile pipeline is a render pipeline state object that holds the tile function; its compute
// pipelines (the function rewritten for one tile size) are made on first use
@interface NVMTLRenderPipelineState () { @public NVMTLFunction *_tileFn; NSMutableDictionary *_tileCps; uint32_t _tileSamples; } @end
@interface NVMTLRenderCommandEncoder () { @public NVMTLRenderPipelineState *_tilePs; NVMTLTexture *_colTex[NVMTL_NCOL]; NSMutableDictionary *_tileBuf, *_tileByt, *_tileTex, *_tileTg; uint32_t _tileW, _tileH; NSMutableDictionary *_tileSmp, *_tileVft, *_tileIft, *_tileAs; } @end
int nvmtl_tile_on(void);
#define NVMTL_TILE_TEXTURE_BASE 120u   /* the translator's TILE_TEXTURE_BASE: attachment A is texture 120 + A */
id nvmtl_tile_compute_pipeline(NVMTLRenderPipelineState *tp, uint32_t tw, uint32_t th, NSString **why);
// sparse textures and heaps (NVMTL_SPARSE_TEXTURES=1)
@interface NVMTLHeap () { @public BOOL _sparse; nvmtl_page_pool _pool; uint64_t *_poolBits; } @end
@interface NVMTLTexture () { @public NVMTLHeap *_sparseHeap; NSMutableDictionary *_sparseTiles; int _sparseTailFirst; uint32_t _sparseTailPages; } @end
@interface NVMTLResourceStateCommandEncoder : NSObject <MTLResourceStateCommandEncoder> { @public NVMTLCommandBuffer *_cb; NSString *_label; } @end
id nvmtl_sparse_heap_texture(NVMTLHeap *heap, MTLTextureDescriptor *d);
void nvmtl_sparse_update(id<MTLTexture> tex, MTLSparseTextureMappingMode mode, MTLRegion region, NSUInteger mip, NSUInteger slice);
