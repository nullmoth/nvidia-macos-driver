// Metal 4 command submission oracle: queue, allocator, command buffer, argument table, encoders, barriers, feedback.
// Runs on Apple hardware for the reference result and on the NVIDIA driver for the comparison. Each arm prints PASS or
// FAIL with the measured value; an arm named NEG asserts something that must be false and passes only when it is.
// usage: mtl4core <mtl4core.metallib>
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#import <QuartzCore/QuartzCore.h>
#include <stdio.h>

static int gFail = 0, gArms = 0;
static void arm(const char *name, BOOL ok, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void arm(const char *name, BOOL ok, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    gArms++; if (!ok) gFail++;
    printf("%s  %-52s %s\n", ok ? "PASS" : "FAIL", name, buf);
}
// A negative arm: the check is expected to be false; the arm passes when it is.
static void neg(const char *name, BOOL checkHolds, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void neg(const char *name, BOOL checkHolds, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    gArms++; if (checkHolds) gFail++;
    printf("%s  %-52s %s\n", checkHolds ? "FAIL" : "PASS", name, buf);
}

static id<MTLDevice> dev;
static id<MTL4CommandAllocator> alloc;
static id<MTL4CommandQueue> q1, q2;
static id<MTLLibrary> lib;

// Commits and waits through the commit feedback. Returns the feedback error, or a timeout marker.
static NSError *runOn(id<MTL4CommandQueue> q, id<MTL4CommandBuffer> cb, int *calls, double *span) {
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *err = nil; __block int n = 0; __block double t0 = 0, t1 = 0;
    MTL4CommitOptions *o = [MTL4CommitOptions new];
    [o addFeedbackHandler:^(id<MTL4CommitFeedback> f) { err = f.error; n++; t0 = f.GPUStartTime; t1 = f.GPUEndTime; dispatch_semaphore_signal(sem); }];
    id<MTL4CommandBuffer> list[1] = { cb };
    [q commit:list count:1 options:o];
    if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC))) {
        if (calls) *calls = -1;
        return [NSError errorWithDomain:@"oracle" code:1 userInfo:@{ NSLocalizedDescriptionKey: @"feedback timeout" }];
    }
    usleep(20000);   // a second call, if the implementation makes one, would land here
    if (calls) *calls = n;
    if (span) *span = t1 - t0;
    return err;
}

static NSError *runN(id<MTL4CommandQueue> q, const id<MTL4CommandBuffer> *list, NSUInteger n, int *calls) {
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);
    __block NSError *err = nil; __block int c = 0;
    MTL4CommitOptions *o = [MTL4CommitOptions new];
    [o addFeedbackHandler:^(id<MTL4CommitFeedback> f) { err = f.error; c++; dispatch_semaphore_signal(sem); }];
    [q commit:list count:n options:o];
    if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC))) return [NSError errorWithDomain:@"oracle" code:1 userInfo:@{ NSLocalizedDescriptionKey: @"feedback timeout" }];
    usleep(20000);
    if (calls) *calls = c;
    return err;
}

static id<MTLBuffer> mk(NSUInteger len) {
    id<MTLBuffer> b = [dev newBufferWithLength:len options:MTLResourceStorageModeShared];
    memset(b.contents, 0, len);
    return b;
}
static id<MTLComputePipelineState> cpso(NSString *name) {
    NSError *e = nil;
    id<MTLFunction> f = [lib newFunctionWithName:name];
    id<MTLComputePipelineState> p = f ? [dev newComputePipelineStateWithFunction:f error:&e] : nil;
    if (!p) printf("setup: compute pipeline %s failed: %s\n", name.UTF8String, e.localizedDescription.UTF8String);
    return p;
}
static id<MTL4ArgumentTable> table(NSUInteger nb, NSUInteger nt, NSUInteger ns) {
    MTL4ArgumentTableDescriptor *d = [MTL4ArgumentTableDescriptor new];
    d.maxBufferBindCount = nb; d.maxTextureBindCount = nt; d.maxSamplerStateBindCount = ns;
    NSError *e = nil;
    id<MTL4ArgumentTable> t = [dev newArgumentTableWithDescriptor:d error:&e];
    if (!t) printf("setup: argument table failed: %s\n", e.localizedDescription.UTF8String);
    return t;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: %s mtl4core.metallib\n", argv[0]); return 2; }
        dev = MTLCreateSystemDefaultDevice();
        if (!dev) { printf("FAIL  no device\n"); return 1; }
        printf("device: %s\n", dev.name.UTF8String);
        NSError *e = nil;
        lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]] error:&e];
        if (!lib) { printf("FAIL  library: %s\n", e.localizedDescription.UTF8String); return 1; }

        q1 = [dev newMTL4CommandQueue];
        MTL4CommandQueueDescriptor *qd = [MTL4CommandQueueDescriptor new]; qd.label = @"oracle-q2";
        q2 = [dev newMTL4CommandQueueWithDescriptor:qd error:&e];
        alloc = [dev newCommandAllocator];
        id<MTL4CommandBuffer> cb = [dev newCommandBuffer];
        arm("objects: queue x2, allocator, command buffer", q1 && q2 && alloc && cb, "q1 %p q2 %p alloc %p cb %p", (__bridge void *)q1, (__bridge void *)q2, (__bridge void *)alloc, (__bridge void *)cb);
        if (!(q1 && q2 && alloc && cb)) return 1;

        // ---- buffers and textures the arms use, all in one residency set ----
        const NSUInteger N = 65536;
        id<MTLBuffer> verts2 = mk(64), color2 = mk(16), verts = mk(64), color = mk(16), base = mk(16), outA = mk(1024), outB = mk(N * 4), cpSrc = mk(256), cpDst = mk(256), pre = mk(N * 4);
        float *v = verts.contents; v[0] = -0.8f; v[1] = -0.8f; v[2] = 0.8f; v[3] = -0.8f; v[4] = 0.0f; v[5] = 0.8f;
        float *v2 = verts2.contents; v2[0] = -0.9f; v2[1] = 0.9f; v2[2] = -0.5f; v2[3] = 0.9f; v2[4] = -0.9f; v2[5] = 0.5f;
        float *c2 = color2.contents; c2[0] = 1.0f; c2[1] = 0.0f; c2[2] = 0.0f; c2[3] = 1.0f;
        float *c = color.contents; c[0] = 0.8f; c[1] = 0.6f; c[2] = 0.4f; c[3] = 1.0f;
        *(uint32_t *)base.contents = 7;
        memset(outA.contents, 0xEE, 1024);

        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:64 height:64 mipmapped:NO];
        td.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; td.storageMode = MTLStorageModeShared;
        id<MTLTexture> rt = [dev newTextureWithDescriptor:td], rtClear = [dev newTextureWithDescriptor:td], rtMesh = [dev newTextureWithDescriptor:td], rtS1 = [dev newTextureWithDescriptor:td], rtS2 = [dev newTextureWithDescriptor:td], rtS3 = [dev newTextureWithDescriptor:td];
        MTLTextureDescriptor *sd = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:1 height:1 mipmapped:NO];
        sd.usage = MTLTextureUsageShaderRead; sd.storageMode = MTLStorageModeShared;
        id<MTLTexture> srcTex = [dev newTextureWithDescriptor:sd];
        const uint8_t texel[4] = { 128, 128, 255, 255 };   // x 0.5, 0.5, 1.0, 1.0
        [srcTex replaceRegion:MTLRegionMake2D(0, 0, 1, 1) mipmapLevel:0 withBytes:texel bytesPerRow:4];
        MTLSamplerDescriptor *smd = [MTLSamplerDescriptor new]; smd.supportArgumentBuffers = YES;
        id<MTLSamplerState> samp = [dev newSamplerStateWithDescriptor:smd];

        MTLResidencySetDescriptor *rd = [MTLResidencySetDescriptor new];
        id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:rd error:&e];
        if (!rs) { printf("FAIL  residency set: %s\n", e.localizedDescription.UTF8String); return 1; }
        for (id<MTLAllocation> a in @[ verts2, color2, verts, color, base, outA, outB, cpSrc, cpDst, pre, rt, rtClear, rtMesh, rtS1, rtS2, rtS3, srcTex ]) [rs addAllocation:a];
        [rs commit];
        [q1 addResidencySet:rs]; [q2 addResidencySet:rs];

        // ---- 1. clear + draw a triangle through an argument table ----
        {
            MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
            pd.vertexFunction = [lib newFunctionWithName:@"tri_v"]; pd.fragmentFunction = [lib newFunctionWithName:@"tri_f"];
            pd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
            id<MTLRenderPipelineState> ps = [dev newRenderPipelineStateWithDescriptor:pd error:&e];
            if (!ps) { printf("FAIL  render pipeline: %s\n", e.localizedDescription.UTF8String); gFail++; }
            id<MTL4ArgumentTable> tv = table(4, 0, 0), tf = table(4, 4, 4);
            [tv setAddress:verts.gpuAddress atIndex:0];
            [tf setAddress:color.gpuAddress atIndex:0];
            [tf setTexture:srcTex.gpuResourceID atIndex:0];
            [tf setSamplerState:samp.gpuResourceID atIndex:0];

            for (int pass = 0; pass < 2; pass++) {
                id<MTLTexture> target = pass == 0 ? rt : rtClear;
                MTL4RenderPassDescriptor *rp = [MTL4RenderPassDescriptor new];
                rp.colorAttachments[0].texture = target;
                rp.colorAttachments[0].loadAction = MTLLoadActionClear; rp.colorAttachments[0].storeAction = MTLStoreActionStore;
                rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 1, 1);
                [alloc reset];
                [cb beginCommandBufferWithAllocator:alloc];
                [cb useResidencySet:rs];
                id<MTL4RenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
                if (pass == 0) {
                    [re setRenderPipelineState:ps];
                    [re setViewport:(MTLViewport){ 0, 0, 64, 64, 0, 1 }];
                    [re setArgumentTable:tv atStages:MTLRenderStageVertex];
                    [re setArgumentTable:tf atStages:MTLRenderStageFragment];
                    [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
                }
                [re endEncoding];
                [cb endCommandBuffer];
                int calls = 0; NSError *err = runOn(q1, cb, &calls, NULL);
                if (pass == 0) arm("1 render commit completes", !err && calls == 1, "error %s feedback calls %d", err ? err.localizedDescription.UTF8String : "none", calls);
            }
            uint8_t px[4] = {0}, corner[4] = {0}, clr[4] = {0};
            [rt getBytes:px bytesPerRow:256 fromRegion:MTLRegionMake2D(32, 40, 1, 1) mipmapLevel:0];
            [rt getBytes:corner bytesPerRow:256 fromRegion:MTLRegionMake2D(1, 62, 1, 1) mipmapLevel:0];
            [rtClear getBytes:clr bytesPerRow:256 fromRegion:MTLRegionMake2D(32, 40, 1, 1) mipmapLevel:0];
            // BGRA bytes. color * texel = (0.4, 0.3, 0.4) -> R 102, G 77, B 102
            BOOL inside = abs(px[2] - 102) <= 3 && abs(px[1] - 77) <= 3 && abs(px[0] - 102) <= 3;
            arm("1 triangle centre = colour x texture", inside, "BGRA %u %u %u %u (want ~102 77 102 255)", px[0], px[1], px[2], px[3]);
            arm("1 outside the triangle keeps the clear colour", corner[0] > 250 && corner[1] < 3 && corner[2] < 3, "BGRA %u %u %u %u (want 255 0 0 255)", corner[0], corner[1], corner[2], corner[3]);
            neg("1 NEG clear-only pass holds the triangle colour", abs(clr[2] - 102) <= 3 && abs(clr[1] - 77) <= 3, "BGRA %u %u %u %u", clr[0], clr[1], clr[2], clr[3]);
            neg("1 NEG centre is the un-textured colour (0.8 0.6 0.4)", abs(px[2] - 204) <= 3 && abs(px[1] - 153) <= 3, "BGRA %u %u %u %u", px[0], px[1], px[2], px[3]);
        }

        // ---- 2. compute kernel writing a buffer bound by address + offset ----
        id<MTLComputePipelineState> fillP = cpso(@"fill_k"), addP = cpso(@"add_k");
        {
            id<MTL4ArgumentTable> t = table(4, 0, 0);
            [t setAddress:outA.gpuAddress + 256 atIndex:0];
            [t setAddress:base.gpuAddress atIndex:1];
            [alloc reset];
            [cb beginCommandBufferWithAllocator:alloc];
            [cb useResidencySet:rs];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:fillP];
            [ce setArgumentTable:t];
            [ce dispatchThreads:MTLSizeMake(16, 1, 1) threadsPerThreadgroup:MTLSizeMake(16, 1, 1)];
            [ce endEncoding];
            [cb endCommandBuffer];
            NSError *err = runOn(q1, cb, NULL, NULL);
            uint32_t *w = outA.contents; BOOL okIn = YES, okSent = YES;
            for (int i = 0; i < 16; i++) if (w[64 + i] != 7u + i) okIn = NO;
            for (int i = 0; i < 64; i++) if (w[i] != 0xEEEEEEEEu) okSent = NO;
            for (int i = 80; i < 256; i++) if (w[i] != 0xEEEEEEEEu) okSent = NO;
            arm("2 kernel writes at address+256", !err && okIn, "word[64..] %u %u %u (want 7 8 9) error %s", w[64], w[65], w[66], err ? "yes" : "none");
            arm("2 bytes outside the bound range untouched", okSent, "word[0] %08x word[80] %08x", w[0], w[80]);
            neg("2 NEG result is at offset 0 of the buffer", w[0] == 7u, "word[0] %08x", w[0]);
        }

        // ---- 3. compute encoder copy + fill ----
        {
            uint8_t *s = cpSrc.contents; for (int i = 0; i < 256; i++) s[i] = (uint8_t)i;
            [alloc reset];
            [cb beginCommandBufferWithAllocator:alloc];
            [cb useResidencySet:rs];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce fillBuffer:cpDst range:NSMakeRange(0, 256) value:0x11];
            [ce barrierAfterEncoderStages:MTLStageBlit beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
            [ce copyFromBuffer:cpSrc sourceOffset:32 toBuffer:cpDst destinationOffset:64 size:64];
            [ce barrierAfterEncoderStages:MTLStageBlit beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
            [ce fillBuffer:cpDst range:NSMakeRange(192, 32) value:0xAB];
            [ce endEncoding];
            [cb endCommandBuffer];
            NSError *err = runOn(q1, cb, NULL, NULL);
            uint8_t *d = cpDst.contents; BOOL cp = YES, f1 = YES, f2 = YES;
            for (int i = 0; i < 64; i++) if (d[64 + i] != (uint8_t)(32 + i)) cp = NO;
            for (int i = 0; i < 64; i++) if (d[i] != 0x11) f1 = NO;
            for (int i = 192; i < 224; i++) if (d[i] != 0xAB) f2 = NO;
            arm("3 copyFromBuffer lands at the destination offset", !err && cp, "dst[64..66] %u %u %u (want 32 33 34)", d[64], d[65], d[66]);
            arm("3 fillBuffer fills only its range", f1 && f2 && d[128] == 0x11 && d[224] == 0x11, "dst[0] %02x dst[192] %02x dst[224] %02x", d[0], d[192], d[224]);
            neg("3 NEG fill value reaches the copied range", d[64] == 0xAB, "dst[64] %02x", d[64]);
        }

        // ---- 4. barriers: kernel A writes, barrier, kernel B reads and adds ----
        {
            id<MTL4ArgumentTable> ta = table(4, 0, 0), tb = table(4, 0, 0);
            [ta setAddress:outB.gpuAddress atIndex:0]; [ta setAddress:base.gpuAddress atIndex:1];
            [tb setAddress:outB.gpuAddress atIndex:0];
            for (int variant = 0; variant < 2; variant++) {
                memset(outB.contents, 0, N * 4);
                [alloc reset];
                [cb beginCommandBufferWithAllocator:alloc];
                [cb useResidencySet:rs];
                if (variant == 0) {
                    id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
                    [ce setComputePipelineState:fillP]; [ce setArgumentTable:ta];
                    [ce dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
                    [ce barrierAfterEncoderStages:MTLStageDispatch beforeEncoderStages:MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
                    [ce setComputePipelineState:addP]; [ce setArgumentTable:tb];
                    [ce dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
                    [ce endEncoding];
                } else {
                    id<MTL4ComputeCommandEncoder> c1 = [cb computeCommandEncoder];
                    [c1 setComputePipelineState:fillP]; [c1 setArgumentTable:ta];
                    [c1 dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
                    [c1 endEncoding];
                    id<MTL4ComputeCommandEncoder> c2 = [cb computeCommandEncoder];
                    [c2 barrierAfterQueueStages:MTLStageDispatch beforeStages:MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
                    [c2 setComputePipelineState:addP]; [c2 setArgumentTable:tb];
                    [c2 dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
                    [c2 endEncoding];
                }
                [cb endCommandBuffer];
                NSError *err = runOn(q1, cb, NULL, NULL);
                uint32_t *w = outB.contents; NSUInteger bad = 0, first = N;
                for (NSUInteger i = 0; i < N; i++) if (w[i] != 7u + (uint32_t)i + 1000u) { if (!bad++) first = i; }
                arm(variant == 0 ? "4 in-encoder barrier orders B after A" : "4 queue barrier orders encoder 2 after encoder 1", !err && !bad,
                    "%lu of %lu wrong (first %ld) word[N-1] %u (want %u)", (unsigned long)bad, (unsigned long)N, bad ? (long)first : -1L, w[N - 1], 7u + (uint32_t)(N - 1) + 1000u);
                if (variant == 0) {
                    uint32_t *p = pre.contents; memset(p, 0, N * 4);
                    neg("4 NEG a buffer only B touched equals A+B", w[0] == p[0] + 1000u + 7u && p[0] != 0, "pre[0] %u", p[0]);
                }
            }
        }

        // ---- 5. feedback ----
        {
            [alloc reset];
            [cb beginCommandBufferWithAllocator:alloc];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce endEncoding];
            [cb endCommandBuffer];
            int calls = 0; double span = 0;
            NSError *err = runOn(q1, cb, &calls, &span);
            arm("5 commit feedback fires once with no error", !err && calls == 1, "calls %d error %s GPU span %.6f s", calls, err ? err.localizedDescription.UTF8String : "none", span);
            neg("5 NEG feedback fires twice", calls == 2, "calls %d", calls);
        }

        // ---- 6. one command buffer on either queue, queue order across queues through an event ----
        {
            id<MTLEvent> ev = [dev newEvent];
            id<MTL4ArgumentTable> ta = table(4, 0, 0), tb = table(4, 0, 0);
            [ta setAddress:outB.gpuAddress atIndex:0]; [ta setAddress:base.gpuAddress atIndex:1]; [tb setAddress:outB.gpuAddress atIndex:0];
            memset(outB.contents, 0, N * 4);
            id<MTL4CommandBuffer> a = [dev newCommandBuffer], b = [dev newCommandBuffer];
            [alloc reset];
            [a beginCommandBufferWithAllocator:alloc]; [a useResidencySet:rs];
            id<MTL4ComputeCommandEncoder> ce = [a computeCommandEncoder];
            [ce setComputePipelineState:fillP]; [ce setArgumentTable:ta];
            [ce dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
            [ce endEncoding]; [a endCommandBuffer];
            [b beginCommandBufferWithAllocator:alloc]; [b useResidencySet:rs];
            ce = [b computeCommandEncoder];
            [ce setComputePipelineState:addP]; [ce setArgumentTable:tb];
            [ce dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
            [ce endEncoding]; [b endCommandBuffer];
            // b goes to q2 first; q2 waits for q1's signal, which q1 sends after a.
            [q2 waitForEvent:ev value:1];
            __block int calls = 0; (void)calls;
            dispatch_semaphore_t sem = dispatch_semaphore_create(0);
            MTL4CommitOptions *o2 = [MTL4CommitOptions new];
            [o2 addFeedbackHandler:^(id<MTL4CommitFeedback> f) { calls++; dispatch_semaphore_signal(sem); }];
            id<MTL4CommandBuffer> lb[1] = { b };
            [q2 commit:lb count:1 options:o2];
            usleep(100000);
            uint32_t early = ((uint32_t *)outB.contents)[5];
            NSError *err = runOn(q1, a, NULL, NULL);
            [q1 signalEvent:ev value:1];
            long timeout = dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
            uint32_t *w = outB.contents; NSUInteger bad = 0;
            for (NSUInteger i = 0; i < N; i++) if (w[i] != 7u + (uint32_t)i + 1000u) bad++;
            arm("6 queue 2 waits for queue 1's event, then runs B", !err && !timeout && !bad, "%lu wrong, b ran %s", (unsigned long)bad, timeout ? "never" : "yes");
            neg("6 NEG B ran before the event was signalled", early != 0, "word[5] before signal %u", early);
            // the same command buffer object, begun again, on the other queue (Apple asserts on a second commit without a new begin)
            memset(outB.contents, 0, N * 4);
            [alloc reset];
            [a beginCommandBufferWithAllocator:alloc]; [a useResidencySet:rs];
            ce = [a computeCommandEncoder];
            [ce setComputePipelineState:fillP]; [ce setArgumentTable:ta];
            [ce dispatchThreads:MTLSizeMake(N, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
            [ce endEncoding]; [a endCommandBuffer];
            err = runOn(q2, a, NULL, NULL);
            arm("6 a command buffer re-begun commits to the other queue", !err && ((uint32_t *)outB.contents)[9] == 16u, "word[9] %u (want 16)", ((uint32_t *)outB.contents)[9]);
        }

        // ---- 7. mesh draw through object/mesh/fragment argument tables ----
        {
            MTLMeshRenderPipelineDescriptor *md = [MTLMeshRenderPipelineDescriptor new];
            md.meshFunction = [lib newFunctionWithName:@"mesh_m"]; md.fragmentFunction = [lib newFunctionWithName:@"mesh_f"];
            md.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
            id<MTLRenderPipelineState> mps = [dev newRenderPipelineStateWithMeshDescriptor:md options:MTLPipelineOptionNone reflection:nil error:&e];
            if (!mps) { arm("7 mesh pipeline builds", NO, "%s", e.localizedDescription.UTF8String); }
            else {
                id<MTL4ArgumentTable> tm = table(4, 0, 0), tf = table(4, 0, 0);
                [tm setAddress:verts.gpuAddress atIndex:0];
                [tf setAddress:color.gpuAddress atIndex:0];
                MTL4RenderPassDescriptor *rp = [MTL4RenderPassDescriptor new];
                rp.colorAttachments[0].texture = rtMesh;
                rp.colorAttachments[0].loadAction = MTLLoadActionClear; rp.colorAttachments[0].storeAction = MTLStoreActionStore;
                rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 1, 1);
                [alloc reset];
                [cb beginCommandBufferWithAllocator:alloc];
                [cb useResidencySet:rs];
                id<MTL4RenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
                [re setRenderPipelineState:mps];
                [re setViewport:(MTLViewport){ 0, 0, 64, 64, 0, 1 }];
                [re setArgumentTable:tm atStages:MTLRenderStageMesh];
                [re setArgumentTable:tf atStages:MTLRenderStageFragment];
                [re drawMeshThreadgroups:MTLSizeMake(1, 1, 1) threadsPerObjectThreadgroup:MTLSizeMake(1, 1, 1) threadsPerMeshThreadgroup:MTLSizeMake(3, 1, 1)];
                [re endEncoding];
                [cb endCommandBuffer];
                NSError *err = runOn(q1, cb, NULL, NULL);
                uint8_t px[4] = {0}, corner[4] = {0};
                [rtMesh getBytes:px bytesPerRow:256 fromRegion:MTLRegionMake2D(32, 40, 1, 1) mipmapLevel:0];
                [rtMesh getBytes:corner bytesPerRow:256 fromRegion:MTLRegionMake2D(1, 62, 1, 1) mipmapLevel:0];
                // colour (0.8 0.6 0.4) -> BGRA 102 153 204
                arm("7 mesh triangle centre = table colour", !err && abs(px[2] - 204) <= 3 && abs(px[1] - 153) <= 3 && abs(px[0] - 102) <= 3, "BGRA %u %u %u %u (want ~102 153 204 255)", px[0], px[1], px[2], px[3]);
                arm("7 outside the mesh triangle keeps the clear colour", corner[0] > 250 && corner[2] < 3, "BGRA %u %u %u %u", corner[0], corner[1], corner[2], corner[3]);
                neg("7 NEG the corner holds the mesh colour", abs(corner[2] - 204) <= 3, "BGRA %u %u %u %u", corner[0], corner[1], corner[2], corner[3]);
            }
        }


        // ---- 8. suspend and resume: one render pass over several encoders ----
        {
            MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
            pd.vertexFunction = [lib newFunctionWithName:@"tri_v"]; pd.fragmentFunction = [lib newFunctionWithName:@"tri_f"];
            pd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
            id<MTLRenderPipelineState> ps = [dev newRenderPipelineStateWithDescriptor:pd error:&e];
            id<MTL4ArgumentTable> tvA = table(4, 0, 0), tfA = table(4, 4, 4), tvB = table(4, 0, 0), tfB = table(4, 4, 4);
            [tvA setAddress:verts.gpuAddress atIndex:0]; [tfA setAddress:color.gpuAddress atIndex:0];
            [tfA setTexture:srcTex.gpuResourceID atIndex:0]; [tfA setSamplerState:samp.gpuResourceID atIndex:0];
            [tvB setAddress:verts2.gpuAddress atIndex:0]; [tfB setAddress:color2.gpuAddress atIndex:0];
            [tfB setTexture:srcTex.gpuResourceID atIndex:0]; [tfB setSamplerState:samp.gpuResourceID atIndex:0];
            id<MTL4CommandBuffer> cb2 = [dev newCommandBuffer];
            void (^draw)(id<MTL4RenderCommandEncoder>, id<MTL4ArgumentTable>, id<MTL4ArgumentTable>) = ^(id<MTL4RenderCommandEncoder> re, id<MTL4ArgumentTable> tv, id<MTL4ArgumentTable> tf) {
                [re setRenderPipelineState:ps];
                [re setViewport:(MTLViewport){ 0, 0, 64, 64, 0, 1 }];
                [re setArgumentTable:tv atStages:MTLRenderStageVertex];
                [re setArgumentTable:tf atStages:MTLRenderStageFragment];
                [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
            };
            MTL4RenderPassDescriptor *(^pass)(id<MTLTexture>) = ^MTL4RenderPassDescriptor *(id<MTLTexture> t) {
                MTL4RenderPassDescriptor *rp = [MTL4RenderPassDescriptor new];
                rp.colorAttachments[0].texture = t;
                rp.colorAttachments[0].loadAction = MTLLoadActionClear; rp.colorAttachments[0].storeAction = MTLStoreActionStore;
                rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 1, 1);
                return rp;
            };
            // Metal 4 only allows the pair in different command buffers of one commit (header rules 1-6), so: a chain of three
            // command buffers (suspending, both, resuming), a pair over two, and a second pass that does not resume (it clears,
            // so the first triangle is gone; two encoders in one command buffer is legal for that).
            id<MTL4CommandBuffer> cb3 = [dev newCommandBuffer];
            for (int variant = 0; variant < 3; variant++) {
                id<MTLTexture> t = variant == 0 ? rtS1 : variant == 1 ? rtS2 : rtS3;
                MTL4RenderPassDescriptor *rp = pass(t);
                [alloc reset];
                [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs];
                id<MTL4RenderCommandEncoder> r1 = [cb renderCommandEncoderWithDescriptor:rp options:variant == 2 ? MTL4RenderEncoderOptionNone : MTL4RenderEncoderOptionSuspending];
                draw(r1, tvA, tfA); [r1 endEncoding];
                NSError *err = nil;
                if (variant == 2) {
                    id<MTL4RenderCommandEncoder> r2 = [cb renderCommandEncoderWithDescriptor:rp options:MTL4RenderEncoderOptionNone];
                    draw(r2, tvB, tfB); [r2 endEncoding];
                    [cb endCommandBuffer];
                    err = runOn(q1, cb, NULL, NULL);
                } else {
                    [cb endCommandBuffer];
                    if (variant == 0) {
                        [cb2 beginCommandBufferWithAllocator:alloc]; [cb2 useResidencySet:rs];
                        id<MTL4RenderCommandEncoder> rm = [cb2 renderCommandEncoderWithDescriptor:rp options:MTL4RenderEncoderOptionResuming | MTL4RenderEncoderOptionSuspending];
                        draw(rm, tvB, tfB); [rm endEncoding]; [cb2 endCommandBuffer];
                    }
                    id<MTL4CommandBuffer> last = variant == 0 ? cb3 : cb2;
                    [last beginCommandBufferWithAllocator:alloc]; [last useResidencySet:rs];
                    id<MTL4RenderCommandEncoder> r2 = [last renderCommandEncoderWithDescriptor:rp options:MTL4RenderEncoderOptionResuming];
                    if (variant == 1) draw(r2, tvB, tfB);
                    [r2 endEncoding]; [last endCommandBuffer];
                    id<MTL4CommandBuffer> list3[3] = { cb, cb2, cb3 }, list2[2] = { cb, cb2 };
                    err = variant == 0 ? runN(q1, list3, 3, NULL) : runN(q1, list2, 2, NULL);
                }
                uint8_t a[4] = {0}, b[4] = {0}, k[4] = {0};
                [t getBytes:a bytesPerRow:256 fromRegion:MTLRegionMake2D(32, 40, 1, 1) mipmapLevel:0];
                [t getBytes:b bytesPerRow:256 fromRegion:MTLRegionMake2D(6, 6, 1, 1) mipmapLevel:0];
                [t getBytes:k bytesPerRow:256 fromRegion:MTLRegionMake2D(60, 3, 1, 1) mipmapLevel:0];
                BOOL hasA = abs(a[2] - 102) <= 3 && abs(a[1] - 77) <= 3, hasB = abs(b[2] - 128) <= 3 && b[1] < 5 && b[0] < 5 && b[3] > 250;   // B = red x texel 0.5
                if (variant < 2) {
                    arm(variant == 0 ? "8 pass chained over three command buffers" : "8 pass resumed in the next command buffer of the commit",
                        !err && hasA && hasB, "tri A BGRA %u %u %u, tri B BGRA %u %u %u (want A 102 77 102, B 0 0 128) error %s", a[0], a[1], a[2], b[0], b[1], b[2], err ? err.localizedDescription.UTF8String : "none");
                    arm(variant == 0 ? "8 clear applied once across the chain" : "8 clear applied once across the split pass", k[0] > 250 && k[1] < 3 && k[2] < 3, "BGRA %u %u %u", k[0], k[1], k[2]);
                } else {
                    arm("8 non-resuming second pass starts a new pass (clears)", !err && hasB && k[0] > 250, "tri B present %d", hasB);
                    neg("8 NEG the first triangle survives a non-resuming pass", hasA, "tri A BGRA %u %u %u", a[0], a[1], a[2]);
                }
            }
        }

        // ---- 9. acceleration structures built, refit, copied and compacted through a Metal 4 compute encoder ----
        // Measured on the M1 Pro (Apple's own Metal 4): build, refit, copy and compacted-size commands in a Metal 4 compute
        // encoder execute nothing (output untouched), while tracing a structure built through Metal 3 works. So the reference
        // device cannot answer for these commands; they run only elsewhere or when MTL4_ORACLE_AS=1 forces them.
        BOOL asRef = [dev.name hasPrefix:@"Apple M1"] && !getenv("MTL4_ORACLE_AS");
        if (asRef) printf("SKIP  9 acceleration structure commands: Apple's Metal 4 on this GPU executes none (measured); set MTL4_ORACLE_AS=1 to run anyway\n");
        else if (dev.supportsRaytracing) {
            id<MTLComputePipelineState> rtP = cpso(@"rt_k"), rtI = cpso(@"rt_inst_k");
            id<MTLBuffer> vb = mk(36), rays = mk(16), out = mk(16), out2 = mk(16), csize = mk(16), idesc = mk(256);
            float *vv = vb.contents; const float tri[9] = { -1, -1, 0, 1, -1, 0, 0, 1, 0 }; memcpy(vv, tri, 36);
            float *rr = rays.contents; rr[0] = 0; rr[1] = 0; rr[2] = 5; rr[3] = 5;
            MTL4AccelerationStructureTriangleGeometryDescriptor *g = [MTL4AccelerationStructureTriangleGeometryDescriptor new];
            g.vertexBuffer = MTL4BufferRangeMake(vb.gpuAddress, 36); g.vertexFormat = MTLAttributeFormatFloat3; g.vertexStride = 12; g.triangleCount = 1; g.opaque = YES;
            MTL4PrimitiveAccelerationStructureDescriptor *pd = [MTL4PrimitiveAccelerationStructureDescriptor new];
            pd.geometryDescriptors = @[ g ]; pd.usage = MTLAccelerationStructureUsageRefit;
            MTLAccelerationStructureSizes sz = [dev accelerationStructureSizesWithDescriptor:pd];
            id<MTLAccelerationStructure> as1 = [dev newAccelerationStructureWithSize:sz.accelerationStructureSize];
            id<MTLAccelerationStructure> as2 = [dev newAccelerationStructureWithSize:sz.accelerationStructureSize];
            NSUInteger scN = MAX(sz.buildScratchBufferSize, sz.refitScratchBufferSize) + 256;
            id<MTLBuffer> scratch = [dev newBufferWithLength:scN options:MTLResourceStorageModePrivate];
            MTLAccelerationStructureSizes isz;
            MTL4InstanceAccelerationStructureDescriptor *td = [MTL4InstanceAccelerationStructureDescriptor new];
            td.instanceDescriptorBuffer = MTL4BufferRangeMake(idesc.gpuAddress, 72); td.instanceCount = 1;
            td.instanceDescriptorType = MTLAccelerationStructureInstanceDescriptorTypeIndirect; td.instanceDescriptorStride = 72;
            // Apple's size query throws on the Metal 4 instance descriptor, so the size comes from the Metal 3 twin
            MTLInstanceAccelerationStructureDescriptor *td3 = [MTLInstanceAccelerationStructureDescriptor descriptor];
            td3.instanceCount = 1; td3.instanceDescriptorType = MTLAccelerationStructureInstanceDescriptorTypeIndirect;
            isz = [dev accelerationStructureSizesWithDescriptor:td3];
            id<MTLAccelerationStructure> tlas = [dev newAccelerationStructureWithSize:isz.accelerationStructureSize];
            id<MTLBuffer> iscratch = [dev newBufferWithLength:isz.buildScratchBufferSize + 256 options:MTLResourceStorageModePrivate];
            MTLResidencySetDescriptor *rd2 = [MTLResidencySetDescriptor new];
            id<MTLResidencySet> rs2 = [dev newResidencySetWithDescriptor:rd2 error:&e];
            for (id<MTLAllocation> a in @[ vb, rays, out, out2, csize, idesc, as1, as2, tlas, scratch, iscratch ]) [rs2 addAllocation:a];
            [rs2 commit]; [q1 addResidencySet:rs2];
            // instance 0: BLAS as1 moved +20 in x
            struct { float m[12]; uint32_t options, mask, ift, user; uint64_t asid; } inst;
            memset(&inst, 0, sizeof inst);
            inst.m[0] = 1; inst.m[1] = 0; inst.m[2] = 0; inst.m[3] = 0; inst.m[4] = 1; inst.m[5] = 0; inst.m[6] = 0; inst.m[7] = 0; inst.m[8] = 1; inst.m[9] = 20; inst.m[10] = 0; inst.m[11] = 0;
            inst.mask = 0xFF; inst.asid = as1.gpuResourceID._impl;
            memcpy(idesc.contents, &inst, sizeof inst);

            id<MTL4ArgumentTable> ta = table(4, 0, 0);
            [ta setResource:as1.gpuResourceID atBufferIndex:0]; [ta setAddress:rays.gpuAddress atIndex:1]; [ta setAddress:out.gpuAddress atIndex:2];
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs2];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce buildAccelerationStructure:as1 descriptor:pd scratchBuffer:MTL4BufferRangeMake(scratch.gpuAddress, scN)];
            [ce endEncoding];
            ce = [cb computeCommandEncoder];
            [ce barrierAfterQueueStages:MTLStageAccelerationStructure beforeStages:MTLStageAccelerationStructure | MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
            [ce writeCompactedAccelerationStructureSize:as1 toBuffer:MTL4BufferRangeMake(csize.gpuAddress, 8)];
            [ce copyAccelerationStructure:as1 toAccelerationStructure:as2];
            [ce barrierAfterEncoderStages:MTLStageAccelerationStructure beforeEncoderStages:MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
            [ce setComputePipelineState:rtP]; [ce setArgumentTable:ta];
            [ce dispatchThreads:MTLSizeMake(2, 1, 1) threadsPerThreadgroup:MTLSizeMake(2, 1, 1)];
            [ce endEncoding]; [cb endCommandBuffer];
            NSError *err = runOn(q1, cb, NULL, NULL);
            float *o = out.contents; uint64_t cs = *(uint64_t *)csize.contents;
            arm("9 ray hits the triangle built on a Metal 4 encoder", !err && fabsf(o[0] - 1.0f) < 1e-3f, "distance %.4f (want 1), error %s", o[0], err ? err.localizedDescription.UTF8String : "none");
            arm("9 ray beside the triangle misses", o[1] == -1.0f, "result %.4f (want -1)", o[1]);
            neg("9 NEG the missing ray reports a hit distance", o[1] > 0, "result %.4f", o[1]);
            arm("9 compacted size written as 64 bits", cs > 0 && cs <= sz.accelerationStructureSize, "compacted %llu of %lu", (unsigned long long)cs, (unsigned long)sz.accelerationStructureSize);

            // the copy traces the same
            [ta setResource:as2.gpuResourceID atBufferIndex:0]; [ta setAddress:out2.gpuAddress atIndex:2];
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs2];
            ce = [cb computeCommandEncoder]; [ce setComputePipelineState:rtP]; [ce setArgumentTable:ta];
            [ce dispatchThreads:MTLSizeMake(2, 1, 1) threadsPerThreadgroup:MTLSizeMake(2, 1, 1)];
            [ce endEncoding]; [cb endCommandBuffer];
            err = runOn(q1, cb, NULL, NULL);
            o = out2.contents;
            arm("9 copied structure traces like the source", !err && fabsf(o[0] - 1.0f) < 1e-3f && o[1] == -1.0f, "distances %.4f %.4f", o[0], o[1]);

            // refit after moving the triangle by +10 in x: the ray at x=10 hits, the one at 0 misses
            for (int i = 0; i < 3; i++) vv[i * 3] += 10.0f;
            rr[0] = 10; rr[1] = 0; rr[2] = 0; rr[3] = 0;
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs2];
            ce = [cb computeCommandEncoder];
            [ce refitAccelerationStructure:as1 descriptor:pd destination:as1 scratchBuffer:MTL4BufferRangeMake(scratch.gpuAddress, scN)];
            [ce barrierAfterEncoderStages:MTLStageAccelerationStructure beforeEncoderStages:MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
            [ta setResource:as1.gpuResourceID atBufferIndex:0]; [ta setAddress:out.gpuAddress atIndex:2];
            [ce setComputePipelineState:rtP]; [ce setArgumentTable:ta];
            [ce dispatchThreads:MTLSizeMake(2, 1, 1) threadsPerThreadgroup:MTLSizeMake(2, 1, 1)];
            [ce endEncoding]; [cb endCommandBuffer];
            err = runOn(q1, cb, NULL, NULL);
            o = out.contents;
            arm("9 refit follows the moved vertices", !err && fabsf(o[0] - 1.0f) < 1e-3f && o[1] == -1.0f, "distances %.4f %.4f (want 1 -1)", o[0], o[1]);
            neg("9 NEG the old position still hits after the refit", o[1] > 0, "result %.4f", o[1]);

            // instance structure over as1 shifted +20 more: ray at x=30 hits, x=0 misses
            rr[0] = 30; rr[1] = 0; rr[2] = 0; rr[3] = 0;
            id<MTL4ArgumentTable> tt = table(4, 0, 0);
            [tt setResource:tlas.gpuResourceID atBufferIndex:0]; [tt setAddress:rays.gpuAddress atIndex:1]; [tt setAddress:out2.gpuAddress atIndex:2];
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs2];
            ce = [cb computeCommandEncoder];
            [ce buildAccelerationStructure:tlas descriptor:td scratchBuffer:MTL4BufferRangeMake(iscratch.gpuAddress, isz.buildScratchBufferSize)];
            [ce barrierAfterEncoderStages:MTLStageAccelerationStructure beforeEncoderStages:MTLStageDispatch visibilityOptions:MTL4VisibilityOptionDevice];
            [ce setComputePipelineState:rtI]; [ce setArgumentTable:tt];
            [ce dispatchThreads:MTLSizeMake(2, 1, 1) threadsPerThreadgroup:MTLSizeMake(2, 1, 1)];
            [ce endEncoding]; [cb endCommandBuffer];
            err = runOn(q1, cb, NULL, NULL);
            o = out2.contents;
            arm("9 instance structure traces the shifted triangle", !err && fabsf(o[0] - 1.0f) < 1e-3f && o[1] == -1.0f, "distances %.4f %.4f (want 1 -1)", o[0], o[1]);
        } else printf("SKIP  9 no ray tracing on this device\n");

        // ---- 10. drawable: waitForDrawable, render, signalDrawable, present ----
        {
            CAMetalLayer *layer = [CAMetalLayer layer]; layer.device = dev; layer.drawableSize = CGSizeMake(64, 64);
            layer.pixelFormat = MTLPixelFormatBGRA8Unorm; layer.framebufferOnly = NO;
            id<CAMetalDrawable> d1 = [layer nextDrawable], d2 = nil;
            dispatch_semaphore_t p1 = dispatch_semaphore_create(0), p2 = dispatch_semaphore_create(0);
            [d1 addPresentedHandler:^(id<MTLDrawable> x) { dispatch_semaphore_signal(p1); }];
            if (d1) {
                [q1 waitForDrawable:d1];
                MTL4RenderPassDescriptor *rp = [MTL4RenderPassDescriptor new];
                rp.colorAttachments[0].texture = d1.texture; rp.colorAttachments[0].loadAction = MTLLoadActionClear;
                rp.colorAttachments[0].storeAction = MTLStoreActionStore; rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 1, 0, 1);
                [alloc reset]; [cb beginCommandBufferWithAllocator:alloc];
                id<MTL4RenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp]; [re endEncoding]; [cb endCommandBuffer];
                NSError *err = runOn(q1, cb, NULL, NULL);
                [q1 signalDrawable:d1];
                [d1 present];
                long w = dispatch_semaphore_wait(p1, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC));
                uint8_t px[4] = {0}; [d1.texture getBytes:px bytesPerRow:256 fromRegion:MTLRegionMake2D(10, 10, 1, 1) mipmapLevel:0];
                arm("10 drawable rendered, signalled and presented", !err && !w && px[1] > 250 && px[0] < 3 && px[2] < 3, "presented %s, BGRA %u %u %u", w ? "never" : "yes", px[0], px[1], px[2]);
                d2 = [layer nextDrawable];
                [d2 addPresentedHandler:^(id<MTLDrawable> x) { dispatch_semaphore_signal(p2); }];
                neg("10 NEG a drawable that was never presented reports presented", dispatch_semaphore_wait(p2, dispatch_time(DISPATCH_TIME_NOW, 300 * 1000000)) == 0, "never presented");
            } else arm("10 drawable", NO, "nextDrawable returned nil");
        }

        // ---- 11. tensor copy in a compute encoder ----
        {
            MTLTensorDescriptor *tdsc = [MTLTensorDescriptor new];
            NSInteger dims[2] = { 8, 4 };
            tdsc.dimensions = [[MTLTensorExtents alloc] initWithRank:2 values:dims]; tdsc.dataType = MTLTensorDataTypeFloat32;
            tdsc.usage = MTLTensorUsageCompute; 
            id<MTLTensor> ta_ = [dev newTensorWithDescriptor:tdsc error:&e], tb_ = [dev newTensorWithDescriptor:tdsc error:&e];
            if (!ta_ || !tb_) arm("11 tensors created", NO, "%s", e.localizedDescription.UTF8String);
            else {
                float src[32], zero[32] = {0}, got[32];
                for (int i = 0; i < 32; i++) src[i] = (float)(i + 1);
                NSInteger hs[2] = { 1, 8 }, org0[2] = { 0, 0 }, full[2] = { 8, 4 };
                MTLTensorExtents *eh = [[MTLTensorExtents alloc] initWithRank:2 values:hs], *eo = [[MTLTensorExtents alloc] initWithRank:2 values:org0], *ef = [[MTLTensorExtents alloc] initWithRank:2 values:full];
                [ta_ replaceSliceOrigin:eo sliceDimensions:ef withBytes:src strides:eh];
                [tb_ replaceSliceOrigin:eo sliceDimensions:ef withBytes:zero strides:eh];
                MTLResidencySetDescriptor *rd3 = [MTLResidencySetDescriptor new];
                id<MTLResidencySet> rs3 = [dev newResidencySetWithDescriptor:rd3 error:&e];
                [rs3 addAllocation:ta_]; [rs3 addAllocation:tb_]; [rs3 commit]; [q1 addResidencySet:rs3];
                // a 4 x 2 region from (2,1) of A into (0,2) of B
                NSInteger so[2] = { 2, 1 }, sd[2] = { 4, 2 }, dorg[2] = { 0, 2 };
                [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs3];
                id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
                [ce copyFromTensor:ta_ sourceOrigin:[[MTLTensorExtents alloc] initWithRank:2 values:so] sourceDimensions:[[MTLTensorExtents alloc] initWithRank:2 values:sd]
                          toTensor:tb_ destinationOrigin:[[MTLTensorExtents alloc] initWithRank:2 values:dorg] destinationDimensions:[[MTLTensorExtents alloc] initWithRank:2 values:sd]];
                [ce endEncoding]; [cb endCommandBuffer];
                NSError *err = runOn(q1, cb, NULL, NULL);
                [tb_ getBytes:got strides:eh fromSliceOrigin:eo sliceDimensions:ef];
                BOOL ok = YES;
                for (int y = 0; y < 4; y++) for (int x = 0; x < 8; x++) {
                    BOOL in = x < 4 && y >= 2 && y < 4;
                    float want = in ? src[(y - 1) * 8 + (x + 2)] : 0.0f;
                    if (got[y * 8 + x] != want) ok = NO;
                }
                arm("11 tensor region copied, the rest untouched", !err && ok, "B[0,2] %.0f B[3,3] %.0f B[0,0] %.0f (want 11 22 0), error %s", got[16], got[27], got[0], err ? "yes" : "none");
                neg("11 NEG B[0,0] holds the source corner", got[0] == src[0], "B[0,0] %.0f", got[0]);
            }
        }

        // ---- 12. sparse buffer mapping on the queue ----
        if (@available(macOS 26.4, *)) { if (dev.supportsPlacementSparse) {
            MTLHeapDescriptor *hd = [MTLHeapDescriptor new];
            hd.type = MTLHeapTypePlacement; hd.size = 4 * 16384; hd.storageMode = MTLStorageModePrivate; hd.sparsePageSize = MTLSparsePageSize16; hd.maxCompatiblePlacementSparsePageSize = MTLSparsePageSize16;
            id<MTLHeap> heap = [dev newHeapWithDescriptor:hd];
            id<MTLBuffer> sp = [dev newBufferWithLength:4 * 16384 options:MTLResourceStorageModePrivate placementSparsePageSize:MTLSparsePageSize16];
            id<MTLBuffer> alias = heap ? [heap newBufferWithLength:16384 options:MTLResourceStorageModePrivate offset:0] : nil;
            if (!heap || !sp || !alias) arm("12 sparse buffer, heap and alias created", NO, "heap %p sparse %p alias %p", (__bridge void *)heap, (__bridge void *)sp, (__bridge void *)alias);
            else {
                id<MTLBuffer> shown = mk(16384);
                MTLResidencySetDescriptor *rd4 = [MTLResidencySetDescriptor new];
                id<MTLResidencySet> rs4 = [dev newResidencySetWithDescriptor:rd4 error:&e];
                [rs4 addAllocation:sp]; [rs4 addAllocation:alias]; [rs4 addAllocation:heap]; [rs4 addAllocation:shown]; [rs4 commit]; [q1 addResidencySet:rs4];
                MTL4UpdateSparseBufferMappingOperation op = { MTLSparseTextureMappingModeMap, NSMakeRange(1, 1), 0 };   // page 1 of the sparse buffer -> heap offset 0
                [q1 updateBufferMappings:sp heap:heap operations:&op count:1];
                id<MTL4ArgumentTable> t = table(4, 0, 0);
                [t setAddress:sp.gpuAddress + 16384 atIndex:0]; [t setAddress:base.gpuAddress atIndex:1];
                [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs4];
                id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
                [ce setComputePipelineState:fillP]; [ce setArgumentTable:t];
                [ce dispatchThreads:MTLSizeMake(16, 1, 1) threadsPerThreadgroup:MTLSizeMake(16, 1, 1)];
                [ce barrierAfterEncoderStages:MTLStageDispatch beforeEncoderStages:MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
                [ce copyFromBuffer:alias sourceOffset:0 toBuffer:shown destinationOffset:0 size:64];
                [ce endEncoding]; [cb endCommandBuffer];
                NSError *err = runOn(q1, cb, NULL, NULL);
                uint32_t *w = shown.contents;
                arm("12 write through a mapped sparse page lands in the heap", !err && w[0] == 7u && w[15] == 22u, "heap words %u %u (want 7 22), error %s", w[0], w[15], err ? "yes" : "none");
                // unmap, remap to nothing: the alias keeps its data (the heap owns it)
                MTL4UpdateSparseBufferMappingOperation un = { MTLSparseTextureMappingModeUnmap, NSMakeRange(1, 1), 0 };
                [q1 updateBufferMappings:sp heap:nil operations:&un count:1];
                arm("12 unmapping a page is accepted", YES, "no error");
                neg("12 NEG the heap word equals an untouched word", w[1] == 0u, "heap word[1] %u", w[1]);
            }
        } else printf("SKIP  12 no placement sparse on this device\n"); } else printf("SKIP  12 needs macOS 26.4\n");


        // ---- 13. dispatchThreadsWithIndirectBuffer: arguments written by the CPU, by the GPU in the same commit, and a later commit ----
        {
            id<MTLComputePipelineState> argsP = cpso(@"args_k");
            id<MTLBuffer> cpuArgs = mk(24), nbuf = mk(16), gpuArgs = [dev newBufferWithLength:24 options:MTLResourceStorageModePrivate];
            id<MTLBuffer> res = mk(1024);
            *(uint32_t *)nbuf.contents = 40;
            uint32_t ca[6] = { 100, 1, 1, 32, 1, 1 };   // 100 threads in groups of 32: the last group is partial
            memcpy(cpuArgs.contents, ca, 24);
            MTLResidencySetDescriptor *rd5 = [MTLResidencySetDescriptor new];
            id<MTLResidencySet> rs5 = [dev newResidencySetWithDescriptor:rd5 error:&e];
            for (id<MTLAllocation> a in @[ cpuArgs, nbuf, gpuArgs, res ]) [rs5 addAllocation:a];
            [rs5 commit]; [q1 addResidencySet:rs5];
            id<MTL4ArgumentTable> tr = table(4, 0, 0), tg2 = table(4, 0, 0), tadd = table(4, 0, 0);
            [tr setAddress:res.gpuAddress atIndex:0]; [tr setAddress:base.gpuAddress atIndex:1];
            [tg2 setAddress:gpuArgs.gpuAddress atIndex:0]; [tg2 setAddress:nbuf.gpuAddress atIndex:1];
            [tadd setAddress:res.gpuAddress atIndex:0];

            // CPU-written arguments, first thing in the commit
            memset(res.contents, 0xEE, 1024);
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs5];
            id<MTL4ComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:fillP]; [ce setArgumentTable:tr];
            [ce dispatchThreadsWithIndirectBuffer:cpuArgs.gpuAddress];
            [ce endEncoding]; [cb endCommandBuffer];
            NSError *err = runOn(q1, cb, NULL, NULL);
            uint32_t *w = res.contents; BOOL ok = YES;
            for (int i = 0; i < 100; i++) if (w[i] != 7u + i) ok = NO;
            arm("13 indirect threads dispatch, CPU-written arguments", !err && ok && w[100] == 0xEEEEEEEEu, "word[0] %u word[99] %u word[100] %08x (want 7 106 eeeeeeee), error %s", w[0], w[99], w[100], err ? "yes" : "none");
            neg("13 NEG the grid is rounded up to whole threadgroups", w[100] != 0xEEEEEEEEu, "word[100] %08x", w[100]);

            // GPU-written arguments (private buffer), consumed in the same commit; then a second commit that depends on the result
            memset(res.contents, 0xEE, 1024);
            [alloc reset]; [cb beginCommandBufferWithAllocator:alloc]; [cb useResidencySet:rs]; [cb useResidencySet:rs5];
            ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:argsP]; [ce setArgumentTable:tg2];
            [ce dispatchThreads:MTLSizeMake(1, 1, 1) threadsPerThreadgroup:MTLSizeMake(1, 1, 1)];
            [ce barrierAfterEncoderStages:MTLStageDispatch beforeEncoderStages:MTLStageDispatch | MTLStageBlit visibilityOptions:MTL4VisibilityOptionDevice];
            [ce setComputePipelineState:fillP]; [ce setArgumentTable:tr];
            [ce dispatchThreadsWithIndirectBuffer:gpuArgs.gpuAddress];
            [ce endEncoding];
            [cb endCommandBuffer];
            id<MTL4CommandBuffer> cbB = [dev newCommandBuffer];
            [cbB beginCommandBufferWithAllocator:alloc]; [cbB useResidencySet:rs]; [cbB useResidencySet:rs5];
            ce = [cbB computeCommandEncoder];
            [ce setComputePipelineState:addP]; [ce setArgumentTable:tadd];
            [ce dispatchThreads:MTLSizeMake(64, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
            [ce endEncoding]; [cbB endCommandBuffer];
            // both commits are issued before either is waited for
            dispatch_semaphore_t s1 = dispatch_semaphore_create(0), s2 = dispatch_semaphore_create(0);
            __block int f1 = 0, f2 = 0; __block NSError *e1 = nil, *e2 = nil;
            MTL4CommitOptions *o1 = [MTL4CommitOptions new], *o2 = [MTL4CommitOptions new];
            [o1 addFeedbackHandler:^(id<MTL4CommitFeedback> f) { e1 = f.error; f1++; dispatch_semaphore_signal(s1); }];
            [o2 addFeedbackHandler:^(id<MTL4CommitFeedback> f) { e2 = f.error; f2++; dispatch_semaphore_signal(s2); }];
            id<MTL4CommandBuffer> l1[1] = { cb }, l2[1] = { cbB };
            [q1 commit:l1 count:1 options:o1];
            [q1 commit:l2 count:1 options:o2];
            long t1 = dispatch_semaphore_wait(s1, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
            long t2 = dispatch_semaphore_wait(s2, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC));
            usleep(20000);
            ok = YES; BOOL ok2 = YES;
            for (int i = 0; i < 40; i++) if (w[i] != 7u + i + 1000u) ok = NO;
            for (int i = 40; i < 64; i++) if (w[i] != 0xEEEEEEEEu + 1000u) ok2 = NO;
            arm("13 indirect threads dispatch, GPU-written arguments", !t1 && !e1 && f1 == 1 && ok, "word[0] %u word[39] %u (want 1007 1046), feedback calls %d error %s", w[0], w[39], f1, e1 ? "yes" : "none");
            arm("13 the commit after it ran behind the whole first commit", !t2 && !e2 && f2 == 1 && ok2, "word[40] %08x (want %08x), feedback calls %d", w[40], 0xEEEEEEEEu + 1000u, f2);
            neg("13 NEG the grid follows the GPU-written count (not 100)", w[50] == 0xEEEEEEEEu + 1000u && w[41] != 0xEEEEEEEEu + 1000u, "word[41] %08x", w[41]);
        }

        // ---- 14. logical-to-physical colour attachment map: a pipeline that inherits the encoder's map ----
        {
            MTLTextureDescriptor *t2 = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:16 height:16 mipmapped:NO];
            t2.usage = MTLTextureUsageRenderTarget; t2.storageMode = MTLStorageModeShared;
            id<MTLTexture> a0 = [dev newTextureWithDescriptor:t2], a1 = [dev newTextureWithDescriptor:t2];
            id<MTLBuffer> big = mk(64); float *bv = big.contents;
            bv[0] = -1.0f; bv[1] = -1.0f; bv[2] = 3.0f; bv[3] = -1.0f; bv[4] = -1.0f; bv[5] = 3.0f;   // covers the target
            [rs addAllocation:a0]; [rs addAllocation:a1]; [rs addAllocation:big]; [rs commit];
            id<MTL4Compiler> comp = [dev newCompilerWithDescriptor:[MTL4CompilerDescriptor new] error:&e];
            MTL4LibraryFunctionDescriptor *vf = [MTL4LibraryFunctionDescriptor new]; vf.library = lib; vf.name = @"tri_v";
            MTL4LibraryFunctionDescriptor *ff = [MTL4LibraryFunctionDescriptor new]; ff.library = lib; ff.name = @"two_f";
            MTL4RenderPipelineDescriptor *pd = [MTL4RenderPipelineDescriptor new];
            pd.vertexFunctionDescriptor = vf; pd.fragmentFunctionDescriptor = ff;
            pd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm; pd.colorAttachments[1].pixelFormat = MTLPixelFormatBGRA8Unorm;
            pd.colorAttachmentMappingState = MTL4LogicalToPhysicalColorAttachmentMappingStateInherited;
            id<MTLRenderPipelineState> ps = comp ? [comp newRenderPipelineStateWithDescriptor:pd compilerTaskOptions:nil error:&e] : nil;
            arm("14 pipeline that inherits the colour map builds", ps != nil, "error %s", e ? e.localizedDescription.UTF8String : "none");
            MTL4RenderPassDescriptor *rp = [MTL4RenderPassDescriptor new];
            rp.supportColorAttachmentMapping = YES;
            for (int i = 0; i < 2; i++) {
                rp.colorAttachments[i].texture = i ? a1 : a0;
                rp.colorAttachments[i].loadAction = MTLLoadActionClear; rp.colorAttachments[i].storeAction = MTLStoreActionStore;
                rp.colorAttachments[i].clearColor = MTLClearColorMake(0, 0, 0, 1);
            }
            MTLLogicalToPhysicalColorAttachmentMap *map = [MTLLogicalToPhysicalColorAttachmentMap new];
            [map setPhysicalIndex:1 forLogicalIndex:0]; [map setPhysicalIndex:0 forLogicalIndex:1];
            id<MTL4ArgumentTable> tv = table(4, 0, 0); [tv setAddress:big.gpuAddress atIndex:0];
            [alloc reset];
            [cb beginCommandBufferWithAllocator:alloc];
            [cb useResidencySet:rs];
            id<MTL4RenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
            [re setColorAttachmentMap:map];
            if (ps) [re setRenderPipelineState:ps];
            [re setViewport:(MTLViewport){ 0, 0, 16, 16, 0, 1 }];
            [re setArgumentTable:tv atStages:MTLRenderStageVertex];
            if (ps) [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
            [re endEncoding];
            [cb endCommandBuffer];
            NSError *err = runOn(q1, cb, NULL, NULL);
            uint8_t p0[4] = {0}, p1[4] = {0};
            [a0 getBytes:p0 bytesPerRow:64 fromRegion:MTLRegionMake2D(8, 8, 1, 1) mipmapLevel:0];
            [a1 getBytes:p1 bytesPerRow:64 fromRegion:MTLRegionMake2D(8, 8, 1, 1) mipmapLevel:0];
            // BGRA bytes: physical 0 gets logical 1 (green), physical 1 gets logical 0 (red)
            arm("14 physical attachment 0 holds logical output 1 (green)", !err && p0[1] > 250 && p0[2] < 5, "BGRA %u %u %u %u error %s", p0[0], p0[1], p0[2], p0[3], err ? "yes" : "none");
            arm("14 physical attachment 1 holds logical output 0 (red)", !err && p1[2] > 250 && p1[1] < 5, "BGRA %u %u %u %u", p1[0], p1[1], p1[2], p1[3]);
            neg("14 NEG the map was ignored (attachment 0 red)", p0[2] > 250 && p0[1] < 5, "BGRA %u %u %u %u", p0[0], p0[1], p0[2], p0[3]);
        }

        printf("\n%d arms, %d failed\n", gArms, gFail);
        return gFail ? 1 : 0;
    }
}
