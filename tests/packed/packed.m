// Ruler for packed tensor element types (int2/int4/fp4/fp8 formats): an element access reads and writes the byte that holds
// the element (measured on an Apple GPU, reference_m1.txt). build: xcrun metal -std=metal4.1 -c packed.metal -o packed.air &&
// xcrun metallib packed.air -o packed.metallib; clang -fobjc-arc packed.m -framework Metal -framework Foundation -o packed
#import <Metal/Metal.h>
#include <stdio.h>
// pk <metallib>: for each packed tensor format, fill a [32 x 2] tensor's bytes with a known pattern, read every element
// through the shader, and print what it reads; then write elements from the shader and print the raw bytes
int main(int argc, char **argv) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    id<MTL4CommandQueue> q = [dev newMTL4CommandQueue]; id<MTL4CommandAllocator> alloc = [dev newCommandAllocator];
    struct { const char *k; MTLTensorDataType t; int isf; } R[] = {
        {"rd_int4", MTLTensorDataTypeInt4, 0}, {"rd_uint4", MTLTensorDataTypeUInt4, 0}, {"rd_int2", MTLTensorDataTypeInt2, 0}, {"rd_uint2", MTLTensorDataTypeUInt2, 0},
        {"rd_fp8e4m3", MTLTensorDataTypeMetalFloat8E4M3, 1}, {"rd_fp8e5m2", MTLTensorDataTypeMetalFloat8E5M2, 1}, {"rd_fp8ue8m0", MTLTensorDataTypeMetalFloat8UE8M0, 1}, {"rd_fp4", MTLTensorDataTypeMetalFloat4E2M1, 1},
        {"wr_int4", MTLTensorDataTypeInt4, 0}, {"wr_uint4", MTLTensorDataTypeUInt4, 0}, {"wr_int2", MTLTensorDataTypeInt2, 0}, {"wr_fp8e4m3", MTLTensorDataTypeMetalFloat8E4M3, 1}, {"wr_fp4", MTLTensorDataTypeMetalFloat4E2M1, 1} };
    for (int r = 0; r < 13; r++) {
        id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@(R[r].k)] error:&e];
        if (!ps) { printf("%s: pipeline FAILED %s\n", R[r].k, e.localizedDescription.UTF8String); continue; }
        MTLTensorDescriptor *d = [MTLTensorDescriptor new]; d.dataType = R[r].t;
        NSInteger dims[2] = {32, 2}, strides[2] = {1, 512};  // row stride 128-byte aligned for format types (bytes = elements*bits/8)
        d.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:dims];
        d.usage = MTLTensorUsageCompute;
        id<MTLBuffer> tb = [dev newBufferWithLength:4096 options:MTLResourceStorageModeShared];
        uint8_t *b = tb.contents; for (int i = 0; i < 4096; i++) b[i] = (uint8_t)((i * 37 + 11) & 0xFF);
        // bits per element: from the size Metal reports for a tensor of exactly this shape
        d.strides = [[MTLTensorExtents alloc] initWithRank:2 values:strides];
        id<MTLTensor> t = [tb newTensorWithDescriptor:d offset:0 error:&e];
        if (!t) { printf("%s: tensor FAILED %s\n", R[r].k, e.localizedDescription.UTF8String); continue; }
        id<MTLBuffer> ob = [dev newBufferWithLength:64 * 4 options:MTLResourceStorageModeShared];
        MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new]; ad.maxBufferBindCount = 4;
        id<MTL4ArgumentTable> at = [dev newArgumentTableWithDescriptor:ad error:&e];
        [at setResource:t.gpuResourceID atBufferIndex:0]; [at setAddress:ob.gpuAddress atIndex:1];
        id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:[MTLResidencySetDescriptor new] error:&e];
        [rs addAllocation:tb]; [rs addAllocation:ob]; [rs addAllocation:t]; [rs commit];
        id<MTL4CommandBuffer> cb = [dev newCommandBuffer]; [alloc reset]; [cb beginCommandBufferWithAllocator:alloc];
        [cb useResidencySet:rs];
        id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:ps]; [ce setArgumentTable:at];
        [ce dispatchThreads:MTLSizeMake(32, 2, 1) threadsPerThreadgroup:MTLSizeMake(32, 2, 1)]; [ce endEncoding]; [cb endCommandBuffer];
        dispatch_semaphore_t sem = dispatch_semaphore_create(0); __block NSError *ferr = nil;
        MTL4CommitOptions *co = [MTL4CommitOptions new]; [co addFeedbackHandler:^(id<MTL4CommitFeedback> fb) { ferr = fb.error; dispatch_semaphore_signal(sem); }];
        id<MTL4CommandBuffer> list[1] = { cb }; [q commit:list count:1 options:co];
        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
        printf("%s:", R[r].k);
        if (R[r].k[0] == 'r') for (int i = 0; i < 64; i++) { if (R[r].isf) printf(" %g", ((float *)ob.contents)[i]); else printf(" %d", ((int *)ob.contents)[i]); }
        else { for (int i = 0; i < 24; i++) printf(" %02x", b[i]); printf(" | row1:"); for (int i = 0; i < 8; i++) printf(" %02x", b[512 * (R[r].t == MTLTensorDataTypeInt2 ? 1 : 1) / 1 + i]); }
        printf("   (%s)\n", ferr ? ferr.localizedDescription.UTF8String : "ok");
    }
    printf("bytes 0..7:"); for (int i = 0; i < 8; i++) printf(" %02x", (uint8_t)((i * 37 + 11) & 0xFF)); printf("\n");
}}
