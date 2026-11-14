/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Entry points the Metal 4 command buffer and encoders call to record into an MTL4CounterHeap and to resolve it.
// The heap itself (NVMTL4CounterHeap) is made by -[NVMTLDevice newCounterHeapWithDescriptor:error:] in NVMTL4Extras.m.
//
// A heap entry is one MTL4TimestampHeapEntry (8 bytes): the raw GPU timestamp in ticks of the Vulkan timestamp query.
// -[MTLDevice queryTimestampFrequency] is the tick rate, so (entry1 - entry0) / frequency is seconds.
//
// Recording goes through the same query pools as the Metal 3 counter sample buffers, so the rules are the same:
// a write is only valid outside a render pass (the Vulkan side defers it to the end of the pass) and a resolve
// into a buffer must be recorded after the writes it reads and outside a render pass.

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

@class NVMTLCommandBuffer;
#include "nvmtl_vk.h"

#ifdef __cplusplus
extern "C" {
#endif

// Records a GPU timestamp into entry `index` of `heap` at this point of `cb`. `precise` asks for the timestamp to be
// taken when all earlier work has finished (bottom of pipe) instead of when the command reaches the front of the pipe.
// Returns NO and marks the command buffer as failed when `heap` is not one of ours, `index` is outside the heap, or
// the query could not be recorded; the reason is logged.
BOOL nvmtl4_counter_heap_write(NVMTLCommandBuffer *cb, id heap, NSUInteger index, BOOL precise);

// Records a copy of entries [range.location, range.location + range.length) of `heap` into `dst` at byte offset
// `dstOffset` (8-byte aligned, 8 bytes per entry). Same failure behaviour as the write.
BOOL nvmtl4_counter_heap_resolve(NVMTLCommandBuffer *cb, id heap, NSRange range, id<MTLBuffer> dst, NSUInteger dstOffset);

// The tensor an app stored in an argument buffer by its MTLTensor gpuResourceID, or nil. For the code that fills or
// reads argument tables and for the shader side of tensors.
id nvmtl4_tensor_for_resource_id(uint64_t resourceID);

// The depth stencil state stored in an argument buffer by its MTLDepthStencilState gpuResourceID, or nil.
id nvmtl4_depth_stencil_for_resource_id(uint64_t resourceID);

// Metal 4 color attachment mapping for the render encoder: permutes the per-attachment arrays of a pass so logical
// output L lands in physical attachment physical[L] (NVMTL_NCOL entries). Apply it to the pass state before the pass
// begins. Returns NO, leaving the pass as it was, when `physical` is out of range or has a repeat. A Metal 3 encoder
// ignores its map (measured on an Apple GPU), so only the Metal 4 encoder calls this.
BOOL nvmtl4_pass_apply_color_map(nvk_pass *p, const uint32_t *physical);

// Placement sparse buffers (Metal 4 queue operations updateBufferMappings:heap:operations:count: and
// copyBufferMappingsFromBuffer:toBuffer:operations:count:). Both return NO, leaving the mapping as it was, when an
// operation is outside the buffer or the heap, a map operation has no placement heap, or the bind failed. Memory
// binding is not ordered against earlier submissions by Vulkan, so the bind waits for the device to go idle:
// the queue must have submitted every earlier command buffer before calling.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
BOOL nvmtl4_sparse_buffer_update(id<MTLBuffer> buffer, id<MTLHeap> heap, const MTL4UpdateSparseBufferMappingOperation *ops, NSUInteger count);
BOOL nvmtl4_sparse_buffer_copy(id<MTLBuffer> src, id<MTLBuffer> dst, const MTL4CopySparseBufferMappingOperation *ops, NSUInteger count);
// Placement sparse textures and sparse heap textures (updateTextureMappings:heap:operations:count: and
// copyTextureMappingsFromTexture:toTexture:operations:count:). `heap` is the placement heap of map operations. Same rules as the buffer calls.
BOOL nvmtl4_sparse_texture_update(id<MTLTexture> texture, id<MTLHeap> heap, const MTL4UpdateSparseTextureMappingOperation *ops, NSUInteger count);
BOOL nvmtl4_sparse_texture_copy(id<MTLTexture> src, id<MTLTexture> dst, const MTL4CopySparseTextureMappingOperation *ops, NSUInteger count);
#pragma clang diagnostic pop

#ifdef __cplusplus
}
#endif
