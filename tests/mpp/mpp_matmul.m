// Shader-side Metal 4 tensors and MetalPerformancePrimitives matmul2d. Runs mpp_matmul.metal on a real Mac for the reference
// result and on the NVIDIA driver for the comparison. Each arm prints PASS or FAIL with the measured value; NEG arms
// assert something that must be false and pass only when it is.
// usage: mpp_matmul <mpp_matmul.metallib>
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <math.h>
#include <stdlib.h>

static int gFail = 0, gArms = 0;
static void arm(const char *name, BOOL ok, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void arm(const char *name, BOOL ok, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    gArms++; if (!ok) gFail++;
    printf("%s  %-52s %s\n", ok ? "PASS" : "FAIL", name, buf);
}
static void neg(const char *name, BOOL checkHolds, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void neg(const char *name, BOOL checkHolds, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    gArms++; if (checkHolds) gFail++;
    printf("%s  %-52s %s\n", checkHolds ? "FAIL" : "PASS", name, buf);
}

static id<MTLDevice> dev;
static id<MTLLibrary> lib;
static id<MTL4CommandQueue> q;
static id<MTL4CommandAllocator> alloc;

// Metal tensor extents list the innermost (fastest) dimension first.
static MTLTensorExtents *ext2(NSInteger a, NSInteger b) { NSInteger v[2] = { a, b }; return [[MTLTensorExtents alloc] initWithRank:2 values:v]; }

// A tensor over a fresh shared buffer: e0 columns by e1 rows, row stride in elements.
static id<MTLTensor> mktensor(MTLTensorDataType t, size_t es, NSInteger e0, NSInteger e1, NSInteger rowStride, id<MTLBuffer> *bufOut) {
    id<MTLBuffer> b = [dev newBufferWithLength:(size_t)(e1 * rowStride) * es + 64 options:MTLResourceStorageModeShared];
    memset(b.contents, 0, b.length);
    MTLTensorDescriptor *d = [MTLTensorDescriptor new];
    d.dimensions = ext2(e0, e1); d.strides = ext2(1, rowStride); d.dataType = t; d.usage = MTLTensorUsageCompute;
    d.storageMode = MTLStorageModeShared;
    NSError *e = nil;
    id<MTLTensor> tn = [b newTensorWithDescriptor:d offset:0 error:&e];
    if (!tn) printf("setup: tensor failed: %s\n", e.localizedDescription.UTF8String);
    if (bufOut) *bufOut = b;
    return tn;
}

static BOOL dispatch(NSString *kernel, NSArray<id<MTLTensor>> *tensors, MTLSize groups, MTLSize tpg, id<MTLBuffer> extra) {
    NSError *e = nil;
    id<MTLFunction> f = [lib newFunctionWithName:kernel];
    id<MTLComputePipelineState> p = f ? [dev newComputePipelineStateWithFunction:f error:&e] : nil;
    if (!p) { printf("setup: pipeline %s failed: %s\n", kernel.UTF8String, e.localizedDescription.UTF8String); return NO; }
    MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new]; ad.maxBufferBindCount = 8;
    id<MTL4ArgumentTable> at = [dev newArgumentTableWithDescriptor:ad error:&e];
    for (NSUInteger i = 0; i < tensors.count; i++) [at setResource:tensors[i].gpuResourceID atBufferIndex:i];
    if (extra) [at setAddress:extra.gpuAddress atIndex:tensors.count];
    id<MTL4CommandBuffer> cb = [dev newCommandBuffer];
    [alloc reset];
    [cb beginCommandBufferWithAllocator:alloc];
    id<MTL4ComputeCommandEncoder> enc = [cb computeCommandEncoder];
    [enc setComputePipelineState:p];
    [enc setArgumentTable:at];
    [enc dispatchThreadgroups:groups threadsPerThreadgroup:tpg];
    [enc endEncoding];
    [cb endCommandBuffer];
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *ferr = nil;
    MTL4CommitOptions *co = [MTL4CommitOptions new];
    [co addFeedbackHandler:^(id<MTL4CommitFeedback> fb) { ferr = fb.error; dispatch_semaphore_signal(sem); }];
    id<MTL4CommandBuffer> list[1] = { cb };
    [q commit:list count:1 options:co];
    BOOL done = dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC)) == 0;
    if (!done || ferr) printf("setup: %s did not complete: %s\n", kernel.UTF8String, ferr ? ferr.localizedDescription.UTF8String : "timeout");
    return done && !ferr;
}

// C(m,n) = sum_k A(m,k) B(k,n). A is [M][K] with dimension 0 = k, B is [K][N] with dimension 0 = n.
static void ref_mm(const __fp16 *A, const __fp16 *B, float *C, int M, int N, int K, int acc) {
    for (int m = 0; m < M; m++) for (int n = 0; n < N; n++) {
        float s = acc ? C[m * N + n] : 0;
        for (int k = 0; k < K; k++) s += (float)A[m * K + k] * (float)B[k * N + n];
        C[m * N + n] = s;
    }
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 2) { fprintf(stderr, "usage: mpp_matmul <mpp_matmul.metallib>\n"); return 2; }
    dev = MTLCreateSystemDefaultDevice();
    arm("device", dev != nil, "%s", dev.name.UTF8String);
    NSError *e = nil;
    lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    arm("library", lib != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    if (!dev || !lib) return 1;
    q = [dev newMTL4CommandQueue]; alloc = [dev newCommandAllocator];
    srand(5);
    enum { M = 32, N = 48, K = 24 };

    // mm_tile: exactly one 16x16x16 tile
    {
        id<MTLBuffer> ba, bb, bc;
        id<MTLTensor> A = mktensor(MTLTensorDataTypeFloat16, 2, 16, 16, 16, &ba), B = mktensor(MTLTensorDataTypeFloat16, 2, 16, 16, 16, &bb), C = mktensor(MTLTensorDataTypeFloat32, 4, 16, 16, 16, &bc);
        __fp16 *a = ba.contents, *b = bb.contents; float *c = bc.contents, r[256];
        for (int i = 0; i < 256; i++) { a[i] = (__fp16)((rand() % 17 - 8) / 4.0f); b[i] = (__fp16)((rand() % 13 - 6) / 4.0f); }
        BOOL ok = dispatch(@"mm_tile", @[A, B, C], MTLSizeMake(1, 1, 1), MTLSizeMake(32, 1, 1), nil);
        ref_mm(a, b, r, 16, 16, 16, 0);
        float md = 0; for (int i = 0; i < 256; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        arm("mm_tile 16x16x16 matches reference", ok && md < 1e-3f, "maxdiff=%.3g", md);
        int zero = 1; for (int i = 0; i < 256; i++) if (c[i] != 0) zero = 0;
        neg("NEG result is the untouched zero tensor", zero, "zero=%d", zero);
    }
    // mm_sliced: 32x48x24 in 16x16 tiles of C, one simdgroup per tile; the tiles at the edge are clipped
    {
        id<MTLBuffer> ba, bb, bc;
        id<MTLTensor> A = mktensor(MTLTensorDataTypeFloat16, 2, K, M, K, &ba), B = mktensor(MTLTensorDataTypeFloat16, 2, N, K, N, &bb), C = mktensor(MTLTensorDataTypeFloat32, 4, N, M, N, &bc);
        __fp16 *a = ba.contents, *b = bb.contents; float *c = bc.contents; static float r[M * N];
        for (int i = 0; i < M * K; i++) a[i] = (__fp16)((rand() % 17 - 8) / 4.0f);
        for (int i = 0; i < K * N; i++) b[i] = (__fp16)((rand() % 13 - 6) / 4.0f);
        BOOL ok = dispatch(@"mm_sliced", @[A, B, C], MTLSizeMake((N + 15) / 16, (M + 15) / 16, 1), MTLSizeMake(32, 1, 1), nil);
        ref_mm(a, b, r, M, N, K, 0);
        float md = 0; for (int i = 0; i < M * N; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        arm("mm_sliced 32x48x24 in 16x16 tiles matches", ok && md < 1e-2f, "maxdiff=%.3g", md);
        // NEG: one row of tiles only, so the lower half of C stays zero and must differ from the reference
        memset(c, 0, sizeof(float) * M * N);
        dispatch(@"mm_sliced", @[A, B, C], MTLSizeMake((N + 15) / 16, 1, 1), MTLSizeMake(32, 1, 1), nil);
        md = 0; for (int i = 0; i < M * N; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        neg("NEG a missing tile row matches the reference", md < 1e-2f, "maxdiff=%.3g", md);
    }
    // mm_acc_tr: C += A * B^T on existing contents
    {
        id<MTLBuffer> ba, bb, bc;
        id<MTLTensor> A = mktensor(MTLTensorDataTypeFloat16, 2, 16, 16, 16, &ba), B = mktensor(MTLTensorDataTypeFloat16, 2, 16, 16, 16, &bb), C = mktensor(MTLTensorDataTypeFloat32, 4, 16, 16, 16, &bc);
        __fp16 *a = ba.contents, *bt = bb.contents; float *c = bc.contents, c0[256], r[256];
        for (int i = 0; i < 256; i++) { a[i] = (__fp16)((rand() % 17 - 8) / 4.0f); bt[i] = (__fp16)((rand() % 9 - 4) / 2.0f); c[i] = c0[i] = (float)(i % 7); }
        BOOL ok = dispatch(@"mm_acc_tr", @[A, B, C], MTLSizeMake(1, 1, 1), MTLSizeMake(32, 1, 1), nil);
        for (int m = 0; m < 16; m++) for (int n = 0; n < 16; n++) {
            float s = c0[m * 16 + n]; for (int k = 0; k < 16; k++) s += (float)a[m * 16 + k] * (float)bt[n * 16 + k]; r[m * 16 + n] = s; }
        float md = 0; for (int i = 0; i < 256; i++) md = fmaxf(md, fabsf(c[i] - r[i]));
        arm("mm_acc_tr multiply-accumulate with B transposed", ok && md < 1e-2f, "maxdiff=%.3g", md);
    }
    // t_access: extents and strided reads through the handle (3 columns, 4 rows, row stride 8 elements)
    {
        id<MTLBuffer> bt, bo = [dev newBufferWithLength:256 options:MTLResourceStorageModeShared];
        id<MTLTensor> T = mktensor(MTLTensorDataTypeFloat32, 4, 3, 4, 8, &bt);
        float *t = bt.contents, *o = bo.contents; memset(o, 0, 256);
        for (int i = 0; i < 32; i++) t[i] = (float)i;
        BOOL ok = dispatch(@"t_access", @[T], MTLSizeMake(1, 1, 1), MTLSizeMake(16, 1, 1), bo);
        BOOL good = o[0] == 3 && o[1] == 4;
        for (int i = 0; i < 12; i++) good &= fabsf(o[2 + i] - 2.0f * t[(i / 3) * 8 + (i % 3)]) < 1e-6f;
        arm("t_access extents and strided element reads", ok && good, "e0=%g e1=%g o2=%g", o[0], o[1], o[2]);
    }
    printf("%d arms, %d failed\n", gArms, gFail);
    return gFail ? 1 : 0;
} }
