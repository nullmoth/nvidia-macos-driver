#import <Metal/Metal.h>
#include <stdio.h>
// mmd <metallib>: for each right-operand type, B (n 32 x k 32) holds byte pattern (row k, byte j) = (k * 16 + j) & 255;
// A selects one k per output row, so C(m, n) = decoded B(k, n). Prints k, then the 32 decoded values of row k.
static id<MTLDevice> dev; static id<MTL4CommandQueue> q; static id<MTL4CommandAllocator> alloc;
static id<MTLTensor> mk(MTLTensorDataType t, NSInteger e0, NSInteger e1, NSInteger rs, id<MTLBuffer> b) {
    MTLTensorDescriptor *d = [MTLTensorDescriptor new]; NSInteger dv[2] = {e0, e1}, sv[2] = {1, rs};
    d.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:dv]; d.strides = [[MTLTensorExtents alloc] initWithRank:2 values:sv];
    d.dataType = t; d.usage = MTLTensorUsageCompute; NSError *e = nil;
    id<MTLTensor> tn = [b newTensorWithDescriptor:d offset:0 error:&e]; if (!tn) printf("tensor: %s\n", e.localizedDescription.UTF8String); return tn;
}
int main(int argc, char **argv) { @autoreleasepool {
    dev = MTLCreateSystemDefaultDevice(); q = [dev newMTL4CommandQueue]; alloc = [dev newCommandAllocator]; NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    struct { const char *k; MTLTensorDataType t; int bits; } T[] = { {"mm_i4", MTLTensorDataTypeInt4, 4}, {"mm_u4", MTLTensorDataTypeUInt4, 4}, {"mm_i2", MTLTensorDataTypeInt2, 2},
        {"mm_u2", MTLTensorDataTypeUInt2, 2}, {"mm_fp4", MTLTensorDataTypeMetalFloat4E2M1, 4}, {"mm_e4m3", MTLTensorDataTypeMetalFloat8E4M3, 8},
        {"mm_e5m2", MTLTensorDataTypeMetalFloat8E5M2, 8}, {"mm_i8", MTLTensorDataTypeInt8, 8}, {"mm_u8", MTLTensorDataTypeUInt8, 8}, {"mm_b16", MTLTensorDataTypeBFloat16, 16} };
    for (int ti = 0; ti < 10; ti++) {
        id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@(T[ti].k)] error:&e];
        if (!ps) { printf("%s: pipeline %s\n", T[ti].k, e.localizedDescription.UTF8String); continue; }
        const NSInteger brs = 128 * 8 / T[ti].bits;   // B row stride in elements: 128 bytes
        id<MTLBuffer> ab = [dev newBufferWithLength:32 * 16 * 2 options:0], bb = [dev newBufferWithLength:32 * 128 options:0], cb_ = [dev newBufferWithLength:32 * 16 * 4 options:0];
        uint8_t *bp = bb.contents; for (int k = 0; k < 32; k++) for (int j = 0; j < 128; j++) bp[k * 128 + j] = (uint8_t)((k * 16 + j) & 255);
        id<MTLTensor> A = mk(MTLTensorDataTypeFloat16, 32, 16, 32, ab), B = mk(T[ti].t, 32, 32, brs, bb), Cc = mk(MTLTensorDataTypeFloat32, 32, 16, 32, cb_);
        if (!A || !B || !Cc) continue;
        printf("%s:\n", T[ti].k);
        for (int half_ = 0; half_ < 2; half_++) {
            __fp16 *ap = ab.contents; memset(ap, 0, 32 * 16 * 2); for (int m = 0; m < 16; m++) ap[m * 32 + m + 16 * half_] = 1;
            memset(cb_.contents, 0, 32 * 16 * 4);
            MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new]; ad.maxBufferBindCount = 4;
            id<MTL4ArgumentTable> at = [dev newArgumentTableWithDescriptor:ad error:&e];
            [at setResource:A.gpuResourceID atBufferIndex:0]; [at setResource:B.gpuResourceID atBufferIndex:1]; [at setResource:Cc.gpuResourceID atBufferIndex:2];
            id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:[MTLResidencySetDescriptor new] error:&e];
            [rs addAllocation:ab]; [rs addAllocation:bb]; [rs addAllocation:cb_]; [rs addAllocation:A]; [rs addAllocation:B]; [rs addAllocation:Cc]; [rs commit];
            id<MTL4CommandBuffer> cb = [dev newCommandBuffer]; [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder]; [ce setComputePipelineState:ps]; [ce setArgumentTable:at];
            [ce dispatchThreadgroups:MTLSizeMake(1, 1, 1) threadsPerThreadgroup:MTLSizeMake(32, 1, 1)]; [ce endEncoding]; [cb endCommandBuffer];
            dispatch_semaphore_t sem = dispatch_semaphore_create(0); __block NSError *ferr = nil;
            MTL4CommitOptions *co = [MTL4CommitOptions new]; [co addFeedbackHandler:^(id<MTL4CommitFeedback> fb) { ferr = fb.error; dispatch_semaphore_signal(sem); }];
            id<MTL4CommandBuffer> list[1] = { cb }; [q commit:list count:1 options:co];
            dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
            if (ferr) printf("  error %s\n", ferr.localizedDescription.UTF8String);
            const float *c = cb_.contents;
            for (int m = 0; m < 16; m++) { printf("  k%02d:", m + 16 * half_); for (int n = 0; n < 32; n++) printf(" %.9g", c[m * 32 + n]); printf("\n"); }
        }
    }
}}
