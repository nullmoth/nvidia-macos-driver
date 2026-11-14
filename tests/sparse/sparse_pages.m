// Ruler for placement sparse textures at every sparse page size (16, 64, 256 KB) in 2D and 3D: the tile size the device
// reports, then three tiles mapped from a placement heap, the whole texture written by a blit, and the texels read
// back per tile. A mapped tile holds what was written; an unmapped one reads zero. Apple GPU answers: reference_m1.txt.
// build: clang -fobjc-arc sparse_pages.m -framework Metal -framework Foundation -o sparse_pages
#import <Metal/Metal.h>
#include <stdio.h>
int main(void) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTL4CommandQueue> q4 = [dev newMTL4CommandQueue];
    MTLSparsePageSize P[] = { MTLSparsePageSize16, MTLSparsePageSize64, MTLSparsePageSize256 }; const char *pn[] = { "16K", "64K", "256K" };
    const NSUInteger W = 1024, H = 1024;
    for (int p = 0; p < 3; p++) for (int dim3 = 0; dim3 < 2; dim3++) {
        const MTLTextureType tt = dim3 ? MTLTextureType3D : MTLTextureType2D; const NSUInteger D = dim3 ? 2 : 1;
        MTLSize ts = [dev sparseTileSizeWithTextureType:tt pixelFormat:MTLPixelFormatRGBA8Unorm sampleCount:1 sparsePageSize:P[p]];
        const NSUInteger page = [dev sparseTileSizeInBytesForSparsePageSize:P[p]];
        printf("%s %s: tile %lux%lux%lu, %lu bytes", pn[p], dim3 ? "3D" : "2D", (unsigned long)ts.width, (unsigned long)ts.height, (unsigned long)ts.depth, (unsigned long)page);
        if (!ts.width || !page) { printf(" - no tile\n"); continue; }
        MTLHeapDescriptor *hd = [MTLHeapDescriptor new]; hd.type = MTLHeapTypePlacement; hd.storageMode = MTLStorageModePrivate; hd.size = page * 8;
        hd.maxCompatiblePlacementSparsePageSize = P[p];
        id<MTLHeap> heap = [dev newHeapWithDescriptor:hd];
        MTLTextureDescriptor *td = [MTLTextureDescriptor new]; td.textureType = tt; td.pixelFormat = MTLPixelFormatRGBA8Unorm; td.width = W; td.height = H; td.depth = D;
        td.storageMode = MTLStorageModePrivate; td.usage = MTLTextureUsageShaderRead; td.placementSparsePageSize = P[p];
        id<MTLTexture> tex = [dev newTextureWithDescriptor:td];
        if (!heap || !tex) { printf(" - heap %p texture %p\n", heap, tex); continue; }
        // tiles (1,0,0), (2,3,0) and, in 3D, (0,0,1); heap pages 0, 1, 2
        const NSUInteger mt[3][3] = { {1, 0, 0}, {2, 3, 0}, {0, 0, 1} }; const int nmap = dim3 ? 3 : 2;
        MTL4UpdateSparseTextureMappingOperation ops[3];
        for (int k = 0; k < nmap; k++)
            ops[k] = (MTL4UpdateSparseTextureMappingOperation){ MTLSparseTextureMappingModeMap,
                MTLRegionMake3D(mt[k][0], mt[k][1], mt[k][2], 1, 1, 1), 0, 0, (NSUInteger)k };   // regions and heap offsets count tiles
        [q4 updateTextureMappings:tex heap:heap operations:ops count:(NSUInteger)nmap];
        // write every texel (0xAB at each byte), then read back the whole level, on the same Metal 4 queue
        const NSUInteger bytes = W * H * D * 4;
        id<MTLBuffer> src = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared], dst = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared];
        memset(src.contents, 0xAB, bytes); memset(dst.contents, 0x55, bytes);
        id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:[MTLResidencySetDescriptor new] error:&e];
        [rs addAllocation:heap]; [rs addAllocation:tex]; [rs addAllocation:src]; [rs addAllocation:dst]; [rs commit];
        id<MTL4CommandAllocator> al = [dev newCommandAllocator];
        id<MTL4CommandBuffer> cb = [dev newCommandBuffer]; [cb beginCommandBufferWithAllocator:al]; [cb useResidencySet:rs];
        id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce copyFromBuffer:src sourceOffset:0 sourceBytesPerRow:W * 4 sourceBytesPerImage:W * H * 4 sourceSize:MTLSizeMake(W, H, D) toTexture:tex destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
        [ce barrierAfterEncoderStages:MTLStageBlit beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
        [ce copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(W, H, D) toBuffer:dst destinationOffset:0 destinationBytesPerRow:W * 4 destinationBytesPerImage:W * H * 4];
        [ce endEncoding]; [cb endCommandBuffer];
        dispatch_semaphore_t sem = dispatch_semaphore_create(0); __block NSError *ferr = nil;
        MTL4CommitOptions *co = [MTL4CommitOptions new]; [co addFeedbackHandler:^(id<MTL4CommitFeedback> fb) { ferr = fb.error; dispatch_semaphore_signal(sem); }];
        id<MTL4CommandBuffer> list[1] = { cb }; [q4 commit:list count:1 options:co];
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC));
        if (ferr) { printf(" - error %s\n", ferr.localizedDescription.UTF8String); continue; }
        // per tile: written (all 0xAB), zero (all 0), or mixed; print the written tiles and any mixed tile
        const uint8_t *b = dst.contents; const NSUInteger gx = W / ts.width, gy = H / ts.height, gz = (D + ts.depth - 1) / ts.depth;
        NSUInteger written = 0, zero = 0, mixed = 0; printf(" | written:");
        for (NSUInteger z = 0; z < gz; z++) for (NSUInteger y = 0; y < gy; y++) for (NSUInteger x = 0; x < gx; x++) {
            NSUInteger ab = 0, zz = 0, tot = 0;
            for (NSUInteger dz = 0; dz < ts.depth && z * ts.depth + dz < D; dz++) for (NSUInteger dy = 0; dy < ts.height; dy++) for (NSUInteger dx = 0; dx < ts.width; dx++) {
                const uint8_t *px = b + (((z * ts.depth + dz) * H + y * ts.height + dy) * W + x * ts.width + dx) * 4;
                tot++; if (px[0] == 0xAB && px[3] == 0xAB) ab++; else if (!px[0] && !px[3]) zz++;
            }
            if (ab == tot) { written++; printf(" (%lu,%lu,%lu)", (unsigned long)x, (unsigned long)y, (unsigned long)z); }
            else if (zz == tot) zero++;
            else { mixed++; printf(" MIXED(%lu,%lu,%lu %lu/%lu)", (unsigned long)x, (unsigned long)y, (unsigned long)z, (unsigned long)ab, (unsigned long)tot); }
        }
        printf(" | %lu written %lu zero %lu mixed\n", (unsigned long)written, (unsigned long)zero, (unsigned long)mixed);
    }
}}
