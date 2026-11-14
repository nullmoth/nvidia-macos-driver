#import <Metal/Metal.h>
#include <stdio.h>
// sc <metallib>: B (k 32 x n 32, transposed, fp8 e4m3 = 1.0 everywhere, int8 = 1, fp4 = 1.0) with a ue8m0 scale plane
// (block 32 along k: one scale per n) whose bytes are (100 + j) & 255 except scale j = 3 -> 0 and j = 4 -> 255.
// A selects k = m, so C(m, n) = 1 * 2^(scale(n) - 127). Prints C row 0 and the scale-plane bytes Apple reports.
int main(int argc, char **argv) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTL4CommandQueue> q = [dev newMTL4CommandQueue]; id<MTL4CommandAllocator> alloc = [dev newCommandAllocator];
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    struct { const char *k; MTLTensorDataType t; uint8_t one; int bits; } T[] = { {"sc_e4m3", MTLTensorDataTypeMetalFloat8E4M3, 0x38, 8}, {"sc_fp4", MTLTensorDataTypeMetalFloat4E2M1, 0x22, 4}, {"sc_i8", MTLTensorDataTypeInt8, 1, 8} };
    for (int ti = 0; ti < 3; ti++) {
        id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@(T[ti].k)] error:&e];
        if (!ps) { printf("%s: pipeline %s\n", T[ti].k, e.localizedDescription.UTF8String); continue; }
        id<MTLBuffer> ab = [dev newBufferWithLength:32 * 16 * 2 options:0], bb = [dev newBufferWithLength:32 * 128 options:0],
                      sb = [dev newBufferWithLength:4096 options:0], cb_ = [dev newBufferWithLength:32 * 16 * 4 options:0];
        memset(bb.contents, T[ti].one, 32 * 128);
        uint8_t *s = sb.contents; for (int j = 0; j < 4096; j++) s[j] = (uint8_t)((100 + j) & 255); s[3] = 0; s[4] = 255;
        __fp16 *ap = ab.contents; for (int m = 0; m < 16; m++) ap[m * 32 + m] = 1;
        MTLTensorDescriptor *d = [MTLTensorDescriptor new]; NSInteger dv[2] = {32, 32}, sv[2] = {1, 128 * 8 / T[ti].bits};
        d.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:dv]; d.strides = [[MTLTensorExtents alloc] initWithRank:2 values:sv];
        d.dataType = T[ti].t; d.usage = MTLTensorUsageCompute;
        MTLTensorAuxiliaryPlaneDescriptor *pd = [MTLTensorAuxiliaryPlaneDescriptor new]; pd.dataType = MTLTensorDataTypeMetalFloat8UE8M0;
        NSInteger bf[2] = {32, 1}; pd.blockFactors = [[MTLTensorExtents alloc] initWithRank:2 values:bf];
        MTLTensorAuxiliaryPlaneDescriptorMap *map = [MTLTensorAuxiliaryPlaneDescriptorMap new]; [map setDescriptor:pd forPlane:MTLTensorPlaneTypeScales];
        d.auxiliaryPlanes = map;
        MTLTensorBufferAttachments *att = [MTLTensorBufferAttachments new]; [att setBuffer:bb offset:0 forPlane:MTLTensorPlaneTypeData]; [att setBuffer:sb offset:0 forPlane:MTLTensorPlaneTypeScales];
        id<MTLTensor> B = [dev newTensorWithDescriptor:d attachments:att error:&e];
        if (!B) { printf("%s: tensor %s\n", T[ti].k, e.localizedDescription.UTF8String); continue; }
        for (id<MTLTensorAuxiliaryPlane> p in B.auxiliaryPlanes) {
            printf("%s scales plane: type %ld", T[ti].k, (long)p.planeType);
            if ([(id)p respondsToSelector:@selector(dimensions)]) { MTLTensorExtents *x = [(id)p dimensions]; printf(" dims"); for (NSInteger r = 0; r < x.rank; r++) printf(" %ld", (long)[x extentAtDimensionIndex:r]); }
            if ([(id)p respondsToSelector:@selector(strides)]) { MTLTensorExtents *x = [(id)p strides]; printf(" strides"); for (NSInteger r = 0; r < x.rank; r++) printf(" %ld", (long)[x extentAtDimensionIndex:r]); }
            printf("\n");
        }
        MTLTensorDescriptor *ad_ = [MTLTensorDescriptor new]; NSInteger adv[2] = {32, 16}, asv[2] = {1, 32};
        ad_.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:adv]; ad_.strides = [[MTLTensorExtents alloc] initWithRank:2 values:asv]; ad_.dataType = MTLTensorDataTypeFloat16; ad_.usage = MTLTensorUsageCompute;
        id<MTLTensor> A = [ab newTensorWithDescriptor:ad_ offset:0 error:&e];
        MTLTensorDescriptor *cd = [MTLTensorDescriptor new]; cd.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:adv]; cd.strides = [[MTLTensorExtents alloc] initWithRank:2 values:asv]; cd.dataType = MTLTensorDataTypeFloat32; cd.usage = MTLTensorUsageCompute;
        NSInteger cdv[2] = {32, 16}; cd.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:cdv];
        id<MTLTensor> Cc = [cb_ newTensorWithDescriptor:cd offset:0 error:&e];
        MTL4ArgumentTableDescriptor *atd = [MTL4ArgumentTableDescriptor new]; atd.maxBufferBindCount = 4;
        id<MTL4ArgumentTable> at = [dev newArgumentTableWithDescriptor:atd error:&e];
        [at setResource:A.gpuResourceID atBufferIndex:0]; [at setResource:B.gpuResourceID atBufferIndex:1]; [at setResource:Cc.gpuResourceID atBufferIndex:2];
        id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:[MTLResidencySetDescriptor new] error:&e];
        for (id<MTLAllocation> x in @[ab, bb, sb, cb_, A, B, Cc]) [rs addAllocation:x]; [rs commit];
        id<MTL4CommandBuffer> cb = [dev newCommandBuffer]; [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs];
        id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder]; [ce setComputePipelineState:ps]; [ce setArgumentTable:at];
        [ce dispatchThreadgroups:MTLSizeMake(1, 1, 1) threadsPerThreadgroup:MTLSizeMake(32, 1, 1)]; [ce endEncoding]; [cb endCommandBuffer];
        dispatch_semaphore_t sem = dispatch_semaphore_create(0); __block NSError *ferr = nil;
        MTL4CommitOptions *co = [MTL4CommitOptions new]; [co addFeedbackHandler:^(id<MTL4CommitFeedback> fb) { ferr = fb.error; dispatch_semaphore_signal(sem); }];
        id<MTL4CommandBuffer> list[1] = { cb }; [q commit:list count:1 options:co];
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
        if (ferr) printf("  error %s\n", ferr.localizedDescription.UTF8String);
        const float *c = cb_.contents;
        for (int m = 0; m < 2; m++) { printf("%s m%d:", T[ti].k, m); for (int n = 0; n < 32; n++) printf(" %.9g", c[m * 32 + n]); printf("\n"); }
    }
}}
