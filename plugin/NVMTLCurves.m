/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// Ray-traced curves on Vulkan, which has no curve geometry. Each curve segment becomes an axis-aligned box, and its
// control points go into a record the translator's curve intersector reads from the structure's side table, the buffer
// bound at buffer index 31 beside the structure. The layout is shared with translator native/air_curves.rs:
//
//   word 2*i     user instance id of instance i (an instance structure)
//   word 2*i+1   word offset of instance i's curve block, 0 = none (word 1 for a primitive structure)
//   block b      b[0] = geometry count, b[1+g] = offset of geometry g's records from b, 0 = not a curve geometry
//   record       control points 0..3 as (x, y, z, radius), MTLCurveBasis, control points per segment, MTLCurveType,
//                MTLCurveEndCaps: NVMTL_CURVE_RECORD_WORDS words per segment
//
// A primitive structure keeps its block at word 2 of its own side table. The boxes and records are written on the GPU
// by nvcurve.metal from the app's control point, radius and index buffers, wherever those live, in the build's own
// command buffer; an instance structure copies the blocks of the structures it instances into its table on the GPU.

#include "nvcurve_lib.h"
#define NVMTL_CURVE_RECORD_WORDS 20

typedef struct { uint32_t nseg, n, ncp, basis, type, caps, cstride, rstride, cfmt, rfmt, index16, pad; } nvmtl_curve_params;

static id<MTLComputePipelineState> nvmtl_curve_pipeline(id<MTLDevice> dev)
{
    static id<MTLComputePipelineState> ps; static pthread_mutex_t lk = PTHREAD_MUTEX_INITIALIZER; static int tried;
    pthread_mutex_lock(&lk);
    if (!tried) {
        tried = 1; NSError *err = nil;
        dispatch_data_t dd = dispatch_data_create(nvcurve_metallib, nvcurve_metallib_len, NULL, DISPATCH_DATA_DESTRUCTOR_DEFAULT);
        id<MTLLibrary> lib = [dev newLibraryWithData:dd error:&err];
        id<MTLFunction> f = [lib newFunctionWithName:@"nvmtl_curve_build"];
        ps = f ? [dev newComputePipelineStateWithFunction:f error:&err] : nil;
        if (!ps) nvlog("acceleration structure: the curve build kernel did not build (%s) - curve geometry is dropped", err.description.UTF8String);
    }
    pthread_mutex_unlock(&lk);
    return ps;
}

// The kernel's reading of one curve geometry, or NO (logged) when its buffers or formats cannot be read.
static BOOL nvmtl_curve_params_of(MTLAccelerationStructureCurveGeometryDescriptor *g, uint32_t nseg, nvmtl_curve_params *p)
{
    NVMTLBuffer *cb = (NVMTLBuffer *)g.controlPointBuffer, *rb = (NVMTLBuffer *)g.radiusBuffer, *ib = (NVMTLBuffer *)g.indexBuffer;
    if (!cb || !rb || !ib) { nvlog("acceleration structure: curve geometry without %s - dropped", !cb ? "control points" : !rb ? "radii" : "indices"); return NO; }
    uint32_t cfmt, csz;
    switch (g.controlPointFormat) {
    case MTLAttributeFormatFloat3: cfmt = 0; csz = 12; break;
    case MTLAttributeFormatFloat4: cfmt = 1; csz = 16; break;
    case MTLAttributeFormatFloat2: cfmt = 2; csz = 8; break;
    case MTLAttributeFormatHalf3:  cfmt = 3; csz = 6; break;
    case MTLAttributeFormatHalf4:  cfmt = 4; csz = 8; break;
    case MTLAttributeFormatHalf2:  cfmt = 5; csz = 4; break;
    default: nvlog("acceleration structure: curve control point format %lu not carried - dropped", (unsigned long)g.controlPointFormat); return NO;
    }
    const uint32_t rfmt = g.radiusFormat == MTLAttributeFormatHalf ? 1 : 0, rsz = rfmt ? 2 : 4;
    if (g.radiusFormat != MTLAttributeFormatHalf && g.radiusFormat != MTLAttributeFormatFloat) {
        nvlog("acceleration structure: curve radius format %lu not carried - dropped", (unsigned long)g.radiusFormat); return NO; }
    const uint32_t n = (uint32_t)g.segmentControlPointCount;
    if (n < 2 || n > 4) { nvlog("acceleration structure: %u control points per curve segment not carried - dropped", n); return NO; }
    const size_t cstride = g.controlPointStride ? g.controlPointStride : csz, rstride = g.radiusStride ? g.radiusStride : rsz;
    const size_t ncp = g.controlPointCount, isz = g.indexType == MTLIndexTypeUInt16 ? 2 : 4;
    // the kernel reads within the buffers it is given; out-of-range segments become empty boxes
    if (g.controlPointBufferOffset + (ncp ? (ncp - 1) * cstride + csz : 0) > cb->_b.size ||
        g.radiusBufferOffset + (ncp ? (ncp - 1) * rstride + rsz : 0) > rb->_b.size ||
        g.indexBufferOffset + (size_t)nseg * isz > ib->_b.size) {
        nvlog("acceleration structure: curve geometry (%zu control points, %u segments) overruns its buffers - dropped", ncp, nseg); return NO; }
    *p = (nvmtl_curve_params){ nseg, n, (uint32_t)ncp, (uint32_t)g.curveBasis, (uint32_t)g.curveType, (uint32_t)g.curveEndCaps,
                               (uint32_t)cstride, (uint32_t)rstride, cfmt, rfmt, isz == 2, 0 };
    return YES;
}

// Gives every curve geometry of a primitive build its boxes and writes the structure's side table: word 1 locates the
// curve block at word 2 (0 when there are no curves, which also clears an earlier build's records). Returns the block's
// word count. Runs in the build's command buffer before the structure is built.
static size_t nvmtl_curve_encode(NVMTLCommandBuffer *cb, MTLAccelerationStructureDescriptor *desc, nvmtl_geom *geoms, uint32_t n,
                                 NVMTLAccelerationStructure *dst)
{
    if (!cb->_scratch) cb->_scratch = [NSMutableArray new];
    NSArray *gd = [desc isKindOfClass:[MTLPrimitiveAccelerationStructureDescriptor class]] ? ((MTLPrimitiveAccelerationStructureDescriptor *)desc).geometryDescriptors : nil;
    nvmtl_curve_params prm[NVMTL_MAX_GEOMS]; uint32_t recAt[NVMTL_MAX_GEOMS] = {0};
    size_t words = 1 + n, boxBytes = 0; BOOL any = NO;
    id<MTLComputePipelineState> ps = nil;
    for (uint32_t i = 0; i < n; i++) {
        nvmtl_geom *o = &geoms[i];
        if (!o->curve) continue;
        if (!ps && !(ps = nvmtl_curve_pipeline(cb->_queue->_dev))) { o->count = 0; continue; }
        if (!o->count || !nvmtl_curve_params_of(gd[o->curve - 1], o->count, &prm[i])) { o->count = 0; continue; }
        recAt[i] = (uint32_t)words; words += (size_t)o->count * NVMTL_CURVE_RECORD_WORDS; boxBytes += (size_t)o->count * 24; any = YES;
    }
    if (!any && !dst->_uid.buf) return 0;
    const size_t headWords = 2 + (any ? 1 + n : 0), tableWords = any ? 2 + words : 2;
    if (dst->_uid.size < tableWords * 4) {
        if (dst->_uid.buf) { NVMTLBuffer *old = [NVMTLBuffer new]; old->_b = dst->_uid; [cb->_scratch addObject:old]; }
        memset(&dst->_uid, 0, sizeof dst->_uid);
        if (nvmtl_vk_buffer_create(tableWords * 4, 0, &dst->_uid)) {
            nvlog("build acceleration structure: curve side table (%zu B) FAILED - curves are dropped", tableWords * 4);
            memset(&dst->_uid, 0, sizeof dst->_uid);
            for (uint32_t i = 0; i < n; i++) if (geoms[i].curve) geoms[i].count = 0;
            return 0;
        }
    }
    NVMTLBuffer *head = [NVMTLBuffer new]; [cb->_scratch addObject:head];
    if (nvmtl_vk_buffer_create(headWords * 4, 1, &head->_b) || !head->_b.map) { nvlog("build acceleration structure: curve side table staging FAILED"); return 0; }
    uint32_t *w = (uint32_t *)head->_b.map; w[0] = 0; w[1] = any ? 2 : 0;
    if (any) { w[2] = n; for (uint32_t i = 0; i < n; i++) w[3 + i] = recAt[i]; }
    if (nvmtl_vk_cmd_copy_buffer(&cb->_c, &head->_b, 0, &dst->_uid, 0, headWords * 4)) nvlog("build acceleration structure: curve side table copy FAILED");
    if (!any) return 0;
    NVMTLBuffer *boxes = [NVMTLBuffer new], *recs = [NVMTLBuffer new]; [cb->_scratch addObject:boxes]; [cb->_scratch addObject:recs];
    const size_t recWords = words - (1 + n);
    if (nvmtl_vk_buffer_create(boxBytes, 0, &boxes->_b) || nvmtl_vk_buffer_create(recWords * 4, 0, &recs->_b)) {
        nvlog("build acceleration structure: curve box / record buffers FAILED - curves are dropped");
        for (uint32_t i = 0; i < n; i++) if (geoms[i].curve) geoms[i].count = 0;
        return 0;
    }
    NVMTLComputeCommandEncoder *ce = (NVMTLComputeCommandEncoder *)[cb computeCommandEncoder];
    [ce setComputePipelineState:ps];
    size_t boxAt = 0;
    for (uint32_t i = 0; i < n; i++) {
        nvmtl_geom *o = &geoms[i];
        if (!o->curve || !o->count) continue;
        MTLAccelerationStructureCurveGeometryDescriptor *g = gd[o->curve - 1];
        [ce setBuffer:g.controlPointBuffer offset:g.controlPointBufferOffset atIndex:0];
        [ce setBuffer:g.radiusBuffer offset:g.radiusBufferOffset atIndex:1];
        [ce setBuffer:g.indexBuffer offset:g.indexBufferOffset atIndex:2];
        [ce setBuffer:boxes offset:boxAt atIndex:3];
        [ce setBuffer:recs offset:(size_t)(recAt[i] - (1 + n)) * 4 atIndex:4];
        [ce setBytes:&prm[i] length:sizeof prm[i] atIndex:5];
        [ce dispatchThreads:MTLSizeMake(o->count, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
        o->boxes = nvmtl_vk_buffer_address(&boxes->_b) + boxAt; o->box_stride = 24;
        // every box candidate is decided by the curve intersector
        o->opaque = 0;
        boxAt += (size_t)o->count * 24;
    }
    [ce endEncoding];
    nvmtl_vk_cmd_barrier(&cb->_c);
    if (nvmtl_vk_cmd_copy_buffer(&cb->_c, &recs->_b, 0, &dst->_uid, (2 + 1 + n) * 4, recWords * 4)) nvlog("build acceleration structure: curve records copy FAILED");
    nvmtl_vk_cmd_barrier(&cb->_c);
    return words;
}
