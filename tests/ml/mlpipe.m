// Metal 4 machine learning pipeline oracle: a 2-layer network (4 -> 8 relu -> 3) packaged by metal-package-builder -ml,
// run through MTL4MachineLearningCommandEncoder with MTLTensor inputs and outputs, compared with a CPU reference.
// Each arm prints PASS or FAIL with the measured value; NEG arms assert something that must be false.
// usage: mlpipe <mlp.mtlpackage> <weights.txt> [io2.mtlpackage]   (weights.txt: W1[8x4] b1[8] W2[3x8] b2[3] as whitespace separated floats)
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <math.h>

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

static void forward(const float *W1, const float *b1, const float *W2, const float *b2, const float *x, float *out) {
    float h[8];
    for (int j = 0; j < 8; j++) { float s = b1[j]; for (int i = 0; i < 4; i++) s += W1[j*4+i]*x[i]; h[j] = s > 0 ? s : 0; }
    for (int k = 0; k < 3; k++) { float s = b2[k]; for (int j = 0; j < 8; j++) s += W2[k*8+j]*h[j]; out[k] = s; }
}

// Metal tensor extents list the innermost (fastest) dimension first, so a [1,4] model input is (4,1).
static MTLTensorExtents *ext2(NSInteger a, NSInteger b) { NSInteger v[2] = { a, b }; return [[MTLTensorExtents alloc] initWithRank:2 values:v]; }

// Pipeline for the first function of a package, with the given input dimensions at buffer index 0..n-1.
static id<MTL4MachineLearningPipelineState> mkpso(id<MTLDevice> dev, id<MTL4Compiler> comp, NSString *pkg, int nin, NSError **e) {
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:pkg] error:e];
    if (!lib) return nil;
    MTL4LibraryFunctionDescriptor *fd = [MTL4LibraryFunctionDescriptor new];
    fd.library = lib; fd.name = lib.functionNames.firstObject;
    MTL4MachineLearningPipelineDescriptor *pd = [MTL4MachineLearningPipelineDescriptor new];
    pd.machineLearningFunctionDescriptor = fd;
    for (int i = 0; i < nin; i++) [pd setInputDimensions:ext2(4, 1) atBufferIndex:i];
    return [comp newMachineLearningPipelineStateWithDescriptor:pd error:e];
}

static id<MTLTensor> mktensor(id<MTLDevice> dev, int n, const float *init) {
    MTLTensorDescriptor *td = [MTLTensorDescriptor new];
    td.dimensions = ext2(n, 1); td.dataType = MTLTensorDataTypeFloat32; td.usage = MTLTensorUsageMachineLearning;
    td.storageMode = MTLStorageModeShared;
    id<MTLTensor> t = [dev newTensorWithDescriptor:td error:nil];
    if (t && init) [t replaceSliceOrigin:ext2(0, 0) sliceDimensions:ext2(n, 1) withBytes:init strides:ext2(1, n)];
    return t;
}

static NSError *runML(id<MTL4CommandQueue> q, id<MTL4CommandAllocator> al, id<MTL4CommandBuffer> cb,
                      id<MTL4MachineLearningPipelineState> pso, id<MTL4ArgumentTable> at, id<MTLHeap> heap, BOOL *done) {
    [al reset];
    [cb beginCommandBufferWithAllocator:al];
    id<MTL4MachineLearningCommandEncoder> enc = [cb machineLearningCommandEncoder];
    if (!enc) { *done = NO; return [NSError errorWithDomain:@"oracle" code:2 userInfo:@{ NSLocalizedDescriptionKey: @"encoder nil" }]; }
    [enc setPipelineState:pso];
    [enc setArgumentTable:at];
    [enc dispatchNetworkWithIntermediatesHeap:heap];
    [enc endEncoding];
    [cb endCommandBuffer];
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *ferr = nil;
    MTL4CommitOptions *co = [MTL4CommitOptions new];
    [co addFeedbackHandler:^(id<MTL4CommitFeedback> f) { ferr = f.error; dispatch_semaphore_signal(sem); }];
    id<MTL4CommandBuffer> list[1] = { cb };
    [q commit:list count:1 options:co];
    *done = dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC)) == 0;
    return ferr;
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 3) { fprintf(stderr, "usage: mlpipe <pkg.mtlpackage> <weights.txt>\n"); return 2; }
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    arm("device", dev != nil, "%s", dev.name.UTF8String);
    if (!dev) return 1;

    float W1[32], b1[8], W2[24], b2[3];
    { FILE *f = fopen(argv[2], "r"); if (!f) { printf("FAIL weights file\n"); return 1; }
      for (int i = 0; i < 32; i++) fscanf(f, "%f", &W1[i]);
      for (int i = 0; i < 8; i++) fscanf(f, "%f", &b1[i]);
      for (int i = 0; i < 24; i++) fscanf(f, "%f", &W2[i]);
      for (int i = 0; i < 3; i++) fscanf(f, "%f", &b2[i]); fclose(f); }
    float x[4] = { 0.5f, -1.0f, 2.0f, 0.25f }, ref[3];
    forward(W1, b1, W2, b2, x, ref);

    NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    arm("ml library from .mtlpackage", lib != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    if (!lib) return 1;
    NSString *fname = lib.functionNames.firstObject;
    arm("library exposes a function", fname != nil, "%s", fname.UTF8String);

    id<MTL4Compiler> comp = [dev newCompilerWithDescriptor:[MTL4CompilerDescriptor new] error:&e];
    arm("compiler", comp != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    if (!comp) return 1;
    MTL4LibraryFunctionDescriptor *fd = [MTL4LibraryFunctionDescriptor new];
    fd.library = lib; fd.name = fname;
    MTL4MachineLearningPipelineDescriptor *pd = [MTL4MachineLearningPipelineDescriptor new];
    pd.machineLearningFunctionDescriptor = fd;
    [pd setInputDimensions:ext2(4, 1) atBufferIndex:0];
    id<MTL4MachineLearningPipelineState> pso = [comp newMachineLearningPipelineStateWithDescriptor:pd error:&e];
    arm("ML pipeline state", pso != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    if (!pso) return 1;
    printf("info  intermediatesHeapSize=%lu bindings=%lu\n", (unsigned long)pso.intermediatesHeapSize, (unsigned long)pso.reflection.bindings.count);
    for (id<MTLBinding> b in pso.reflection.bindings)
        printf("info  binding name=%s index=%lu type=%ld\n", b.name.UTF8String, (unsigned long)b.index, (long)b.type);

    MTLTensorDescriptor *td = [MTLTensorDescriptor new];
    td.dimensions = ext2(4, 1);
    td.dataType = MTLTensorDataTypeFloat32; td.usage = MTLTensorUsageMachineLearning;
    td.storageMode = MTLStorageModeShared;
    id<MTLTensor> tin = [dev newTensorWithDescriptor:td error:&e];
    arm("input tensor", tin != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    td.dimensions = ext2(3, 1);
    id<MTLTensor> tout = [dev newTensorWithDescriptor:td error:&e];
    arm("output tensor", tout != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    if (!tin || !tout) return 1;
    printf("info  in strides=%ld,%ld out strides=%ld,%ld\n", (long)[tin.strides extentAtDimensionIndex:0], (long)[tin.strides extentAtDimensionIndex:1],
           (long)[tout.strides extentAtDimensionIndex:0], (long)[tout.strides extentAtDimensionIndex:1]);

    [tin replaceSliceOrigin:ext2(0, 0) sliceDimensions:ext2(4, 1) withBytes:x strides:ext2(1, 4)];

    MTLHeapDescriptor *hd = [MTLHeapDescriptor new];
    hd.size = pso.intermediatesHeapSize ? pso.intermediatesHeapSize : 4096; hd.storageMode = MTLStorageModePrivate;
    id<MTLHeap> heap = [dev newHeapWithDescriptor:hd];
    arm("intermediates heap", heap != nil, "size=%lu", (unsigned long)hd.size);

    MTL4ArgumentTableDescriptor *ad = [MTL4ArgumentTableDescriptor new];
    ad.maxBufferBindCount = 4;
    id<MTL4ArgumentTable> at = [dev newArgumentTableWithDescriptor:ad error:&e];
    arm("argument table", at != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
    [at setResource:tin.gpuResourceID atBufferIndex:0];
    [at setResource:tout.gpuResourceID atBufferIndex:1];

    id<MTL4CommandQueue> q = [dev newMTL4CommandQueue];
    id<MTL4CommandAllocator> al = [dev newCommandAllocator];
    id<MTL4CommandBuffer> cb = [dev newCommandBuffer];
    BOOL done = NO;
    NSError *ferr = runML(q, al, cb, pso, at, heap, &done);
    arm("ML command encoder + command buffer completes", done && !ferr, "%s", ferr ? ferr.localizedDescription.UTF8String : (done ? "ok" : "timeout"));

    float out[3] = { 0 };
    [tout getBytes:out strides:ext2(1, 3) fromSliceOrigin:ext2(0, 0) sliceDimensions:ext2(3, 1)];
    float md = 0; for (int i = 0; i < 3; i++) md = fmaxf(md, fabsf(out[i] - ref[i]));
    printf("info  out=%.6f %.6f %.6f ref=%.6f %.6f %.6f\n", out[0], out[1], out[2], ref[0], ref[1], ref[2]);
    arm("network output matches CPU reference (1e-4)", md < 1e-4f, "maxdiff=%.3g", md);
    neg("NEG output is the untouched zero tensor", out[0] == 0 && out[1] == 0 && out[2] == 0, "out0=%.6f", out[0]);

    // A second dispatch with a different input proves the dispatch re-reads the input tensor.
    float x2[4] = { -1.5f, 0.75f, 0.1f, 3.0f }, ref2[3];
    forward(W1, b1, W2, b2, x2, ref2);
    [tin replaceSliceOrigin:ext2(0, 0) sliceDimensions:ext2(4, 1) withBytes:x2 strides:ext2(1, 4)];
    id<MTL4CommandBuffer> cb2 = [dev newCommandBuffer];
    ferr = runML(q, al, cb2, pso, at, heap, &done);
    [tout getBytes:out strides:ext2(1, 3) fromSliceOrigin:ext2(0, 0) sliceDimensions:ext2(3, 1)];
    md = 0; for (int i = 0; i < 3; i++) md = fmaxf(md, fabsf(out[i] - ref2[i]));
    arm("second input re-dispatched (1e-4)", done && !ferr && md < 1e-4f, "maxdiff=%.3g", md);

    MTL4MachineLearningPipelineDescriptor *bad = [MTL4MachineLearningPipelineDescriptor new];
    NSError *be = nil;
    id<MTL4MachineLearningPipelineState> bp = [comp newMachineLearningPipelineStateWithDescriptor:bad error:&be];
    neg("NEG pipeline without a function is created", bp != nil, "err=%s", be ? be.localizedDescription.UTF8String : "none");

    // Two inputs and two outputs: pins down the argument table order (inputs first, then outputs, in model order),
    // and covers a tensor whose rows are padded to 64 bytes.
    if (argc > 3) {
        id<MTL4MachineLearningPipelineState> p2 = mkpso(dev, comp, @(argv[3]), 2, &e);
        arm("io2 pipeline state", p2 != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
        if (p2) {
            float a[4] = { 1, 2, 3, 4 }, bb[4] = { 0.5f, 0.5f, 0.5f, 0.5f };
            id<MTLTensor> ta = mktensor(dev, 4, a), tb = mktensor(dev, 4, bb), y0 = mktensor(dev, 4, NULL), y1 = mktensor(dev, 4, NULL);
            // a wrapped copy of b with a 64-byte row stride (Apple requires offset 0 for machine learning tensors)
            id<MTLBuffer> pb = [dev newBufferWithLength:1024 options:MTLResourceStorageModeShared];
            memset(pb.contents, 0, 1024);
            memcpy((char *)pb.contents, bb, sizeof bb);
            MTLTensorDescriptor *wd = [MTLTensorDescriptor new];
            wd.dimensions = ext2(4, 1); wd.strides = ext2(1, 16); wd.dataType = MTLTensorDataTypeFloat32;
            wd.usage = MTLTensorUsageMachineLearning; wd.storageMode = MTLStorageModeShared;
            id<MTLTensor> tbPad = [pb newTensorWithDescriptor:wd offset:0 error:&e];
            arm("wrapped padded tensor", tbPad != nil, "%s", e ? e.localizedDescription.UTF8String : "ok");
            MTL4ArgumentTableDescriptor *a2 = [MTL4ArgumentTableDescriptor new]; a2.maxBufferBindCount = 8;
            id<MTL4ArgumentTable> t2 = [dev newArgumentTableWithDescriptor:a2 error:&e];
            id<MTLHeap> h2 = heap;
            float want0[4] = { 0.5f, 1.5f, 2.5f, 3.5f }, want1[4] = { 2, 4, 6, 8 }, g0[4], g1[4];
            for (int variant = 0; variant < 3; variant++) {
                id<MTLTensor> inA = ta, inB = variant == 1 ? tbPad : tb;
                if (variant == 2) { inA = tb; inB = ta; }   // swapped inputs: a - b changes, a * 2 changes
                [t2 setResource:inA.gpuResourceID atBufferIndex:0]; [t2 setResource:inB.gpuResourceID atBufferIndex:1];
                [t2 setResource:y0.gpuResourceID atBufferIndex:2]; [t2 setResource:y1.gpuResourceID atBufferIndex:3];
                id<MTL4CommandBuffer> c3 = [dev newCommandBuffer];
                ferr = runML(q, al, c3, p2, t2, h2, &done);
                [y0 getBytes:g0 strides:ext2(1, 4) fromSliceOrigin:ext2(0, 0) sliceDimensions:ext2(4, 1)];
                [y1 getBytes:g1 strides:ext2(1, 4) fromSliceOrigin:ext2(0, 0) sliceDimensions:ext2(4, 1)];
                float m0 = 0, m1 = 0;
                for (int i = 0; i < 4; i++) { m0 = fmaxf(m0, fabsf(g0[i] - want0[i])); m1 = fmaxf(m1, fabsf(g1[i] - want1[i])); }
                if (variant == 2) {
                    neg("NEG swapped inputs still give a-b and a*2", m0 < 1e-5f && m1 < 1e-5f, "y0diff=%.3g y1diff=%.3g", m0, m1);
                } else {
                    arm(variant ? "io2 padded-stride input: y0=a-b, y1=a*2" : "io2 y0=a-b, y1=a*2 (inputs 0,1 then outputs 2,3)",
                        done && !ferr && m0 < 1e-5f && m1 < 1e-5f, "y0diff=%.3g y1diff=%.3g", m0, m1);
                }
            }
        }
    }

    printf("%d arms, %d failed\n", gArms, gFail);
    return gFail ? 1 : 0;
} }
