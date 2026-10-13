// bench_pipeline_build: how long N pipelines take to build asynchronously versus one at a time, and whether every one of them
// computes the right answer while builds overlap.
//
// Each run makes its kernels unique (a per-run salt in every constant), so no shader cache can answer. Kernels are
// integer-only, so the CPU computes the exact expected output. Render pipelines share one vertex function with
// distinct fragment functions, so several builds touch the same function object at once.
//
// build: clang -fobjc-arc -O1 -o bench_pipeline_build tools/bench_pipeline_build.m -framework Metal -framework Foundation
//        NVMTL_PIPELINE_THREADS=1 ./bench_pipeline_build 48 24 0   (one build at a time, the old behaviour)
//        ./bench_pipeline_build 48 24 3                              (default width, plus 3 stress rounds)
// usage: bench_pipeline_build [kernels] [render] [stress-rounds]      default 32 kernels, 16 render pipelines, 0 stress rounds
#import <Metal/Metal.h>
#import <sys/sysctl.h>
#import <stdio.h>
#import <stdlib.h>

#define STEPS 28
#define TG 64
#define GRID (TG * 16)
typedef struct { int op; uint32_t a, b, s; } step_t;

static uint32_t rng_state;
static uint32_t rnd(void) { rng_state = rng_state * 1664525u + 1013904223u; return rng_state ^ (rng_state >> 15); }

static void make_steps(step_t *st) {
    for (int i = 0; i < STEPS; i++) {
        st[i].op = (int)(rnd() % 7); st[i].a = rnd() | 1u; st[i].b = rnd(); st[i].s = 1 + rnd() % 30;
        if (st[i].op == 6) st[i].s = 2 + rnd() % 4;
    }
}
static uint32_t run_steps(const step_t *st, uint32_t x) {
    for (int i = 0; i < STEPS; i++) {
        const step_t *t = &st[i];
        switch (t->op) {
        case 0: x = x * t->a + t->b; break;
        case 1: x ^= x >> t->s; break;
        case 2: x ^= x << t->s; break;
        case 3: if (x & t->a) x += t->b; else x ^= t->a; break;
        case 4: x = (x << t->s) | (x >> (32 - t->s)); break;
        case 5: switch (x & 3u) { case 0: x += t->a; break; case 1: x ^= t->b; break; case 2: x *= 3u; break; default: x -= t->a; } break;
        case 6: for (uint32_t k = 0; k < t->s; k++) x = x * 0x9e3779b1u + k + t->b; break;
        }
    }
    return x;
}
static void emit_steps(NSMutableString *src, const step_t *st) {
    for (int i = 0; i < STEPS; i++) {
        const step_t *t = &st[i];
        switch (t->op) {
        case 0: [src appendFormat:@"    x = x * %uu + %uu;\n", t->a, t->b]; break;
        case 1: [src appendFormat:@"    x ^= x >> %uu;\n", t->s]; break;
        case 2: [src appendFormat:@"    x ^= x << %uu;\n", t->s]; break;
        case 3: [src appendFormat:@"    if (x & %uu) x += %uu; else x ^= %uu;\n", t->a, t->b, t->a]; break;
        case 4: [src appendFormat:@"    x = (x << %uu) | (x >> %uu);\n", t->s, 32 - t->s]; break;
        case 5: [src appendFormat:@"    switch (x & 3u) { case 0: x += %uu; break; case 1: x ^= %uu; break; case 2: x *= 3u; break; default: x -= %uu; }\n", t->a, t->b, t->a]; break;
        case 6: [src appendFormat:@"    for (uint k = 0; k < %uu; k++) x = x * 0x9e3779b1u + k + %uu;\n", t->s, t->b]; break;
        }
    }
}

typedef struct { step_t st[STEPS]; uint32_t color; } kernel_t;

static NSString *make_source(kernel_t *ks, int nk, int nr, uint32_t salt) {
    NSMutableString *src = [NSMutableString stringWithString:@"#include <metal_stdlib>\nusing namespace metal;\n"];
    for (int i = 0; i < nk; i++) {
        make_steps(ks[i].st);
        [src appendFormat:@"kernel void k%d_%08x(device const uint *in [[buffer(0)]], device uint *out [[buffer(1)]],\n"
                           "    uint gid [[thread_position_in_grid]], uint lid [[thread_position_in_threadgroup]]) {\n"
                           "    threadgroup uint tg[%d];\n    uint x = in[gid];\n", i, salt, TG];
        emit_steps(src, ks[i].st);
        [src appendFormat:@"    tg[lid] = x;\n    threadgroup_barrier(mem_flags::mem_threadgroup);\n"
                           "    out[gid] = x + tg[(lid + 1u) %% %du];\n}\n", TG];
    }
    [src appendString:@"struct VOut { float4 p [[position]]; };\n"
                       "vertex VOut vfull(uint v [[vertex_id]]) { float2 q = float2((v << 1) & 2, v & 2); VOut o; o.p = float4(q * 2.0 - 1.0, 0, 1); return o; }\n"];
    for (int i = 0; i < nr; i++) {
        ks[i].color = rnd();
        const uint32_t c = ks[i].color;
        [src appendFormat:@"fragment float4 f%d_%08x(VOut in [[stage_in]]) { return float4(%u.0/255.0, %u.0/255.0, %u.0/255.0, 1.0); }\n",
                          i, salt, c & 255, (c >> 8) & 255, (c >> 16) & 255];
    }
    return src;
}

static int check_compute(id<MTLDevice> dev, id<MTLCommandQueue> q, id<MTLComputePipelineState> ps, const kernel_t *k) {
    if (!ps) return 0;
    id<MTLBuffer> in = [dev newBufferWithLength:GRID * 4 options:MTLResourceStorageModeShared];
    id<MTLBuffer> out = [dev newBufferWithLength:GRID * 4 options:MTLResourceStorageModeShared];
    uint32_t *pi = in.contents; for (int i = 0; i < GRID; i++) pi[i] = (uint32_t)i * 2654435761u + 7u;
    memset(out.contents, 0, GRID * 4);
    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> e = [cb computeCommandEncoder];
    [e setComputePipelineState:ps]; [e setBuffer:in offset:0 atIndex:0]; [e setBuffer:out offset:0 atIndex:1];
    [e dispatchThreadgroups:MTLSizeMake(GRID / TG, 1, 1) threadsPerThreadgroup:MTLSizeMake(TG, 1, 1)];
    [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
    uint32_t pre[GRID]; for (int i = 0; i < GRID; i++) pre[i] = run_steps(k->st, pi[i]);
    const uint32_t *po = out.contents; int bad = 0;
    for (int i = 0; i < GRID; i++) { const int base = i / TG * TG; if (po[i] != pre[i] + pre[base + (i % TG + 1) % TG]) bad++; }
    return bad == 0;
}
static int check_render(id<MTLDevice> dev, id<MTLCommandQueue> q, id<MTLRenderPipelineState> ps, uint32_t color) {
    if (!ps) return 0;
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:4 height:4 mipmapped:NO];
    td.usage = MTLTextureUsageRenderTarget; td.storageMode = MTLStorageModeManaged;
    id<MTLTexture> t = [dev newTextureWithDescriptor:td];
    MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor renderPassDescriptor];
    rp.colorAttachments[0].texture = t; rp.colorAttachments[0].loadAction = MTLLoadActionClear;
    rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0); rp.colorAttachments[0].storeAction = MTLStoreActionStore;
    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:rp];
    [e setRenderPipelineState:ps]; [e drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3]; [e endEncoding];
    id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder]; [bl synchronizeResource:t]; [bl endEncoding];
    [cb commit]; [cb waitUntilCompleted];
    uint8_t px[4 * 4 * 4]; [t getBytes:px bytesPerRow:16 fromRegion:MTLRegionMake2D(0, 0, 4, 4) mipmapLevel:0];
    int bad = 0;
    for (int i = 0; i < 16; i++) {
        const int r = px[i * 4], g = px[i * 4 + 1], b = px[i * 4 + 2];
        if (abs(r - (int)(color & 255)) > 1 || abs(g - (int)((color >> 8) & 255)) > 1 || abs(b - (int)((color >> 16) & 255)) > 1) bad++;
    }
    return bad == 0;
}

typedef struct { double secs; int ok, total; } result_t;

static result_t build_set(id<MTLDevice> dev, id<MTLCommandQueue> q, int nk, int nr, BOOL async, const char *label) {
    const uint32_t salt = rnd();
    kernel_t *ks = calloc((size_t)(nk > nr ? nk : nr), sizeof *ks);
    NSString *src = make_source(ks, nk, nr, salt);
    NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:src options:nil error:&err];
    if (!lib) { printf("library: %s\n", err.description.UTF8String); exit(2); }
    NSMutableArray *kf = [NSMutableArray array], *ff = [NSMutableArray array];
    for (int i = 0; i < nk; i++) [kf addObject:[lib newFunctionWithName:[NSString stringWithFormat:@"k%d_%08x", i, salt]]];
    for (int i = 0; i < nr; i++) [ff addObject:[lib newFunctionWithName:[NSString stringWithFormat:@"f%d_%08x", i, salt]]];
    id<MTLFunction> vf = [lib newFunctionWithName:@"vfull"];
    NSMutableArray *cps = [NSMutableArray array], *rps = [NSMutableArray array];
    for (int i = 0; i < nk; i++) [cps addObject:NSNull.null];
    for (int i = 0; i < nr; i++) [rps addObject:NSNull.null];
    const double t0 = CFAbsoluteTimeGetCurrent();
    if (async) {
        dispatch_group_t g = dispatch_group_create();
        for (int i = 0; i < nk; i++) {
            dispatch_group_enter(g);
            [dev newComputePipelineStateWithFunction:kf[i] completionHandler:^(id<MTLComputePipelineState> ps, NSError *e) { @synchronized (cps) { if (ps) cps[i] = ps; } dispatch_group_leave(g); }];
        }
        for (int i = 0; i < nr; i++) {
            MTLRenderPipelineDescriptor *d = [MTLRenderPipelineDescriptor new];
            d.vertexFunction = vf; d.fragmentFunction = ff[i]; d.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
            dispatch_group_enter(g);
            [dev newRenderPipelineStateWithDescriptor:d completionHandler:^(id<MTLRenderPipelineState> ps, NSError *e) { @synchronized (rps) { if (ps) rps[i] = ps; } dispatch_group_leave(g); }];
        }
        dispatch_group_wait(g, DISPATCH_TIME_FOREVER);
    } else {
        for (int i = 0; i < nk; i++) { id ps = [dev newComputePipelineStateWithFunction:kf[i] error:nil]; if (ps) cps[i] = ps; }
        for (int i = 0; i < nr; i++) {
            MTLRenderPipelineDescriptor *d = [MTLRenderPipelineDescriptor new];
            d.vertexFunction = vf; d.fragmentFunction = ff[i]; d.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
            id ps = [dev newRenderPipelineStateWithDescriptor:d error:nil]; if (ps) rps[i] = ps;
        }
    }
    result_t r = { CFAbsoluteTimeGetCurrent() - t0, 0, nk + nr };
    for (int i = 0; i < nk; i++) r.ok += check_compute(dev, q, cps[i] == NSNull.null ? nil : cps[i], &ks[i]);
    for (int i = 0; i < nr; i++) r.ok += check_render(dev, q, rps[i] == NSNull.null ? nil : rps[i], ks[i].color);
    printf("%-26s %3d pipelines in %6.2f s (%5.1f ms each)  correct %d/%d\n", label, r.total, r.secs, r.secs * 1000 / r.total, r.ok, r.total);
    fflush(stdout);
    free(ks);
    return r;
}

// Async and sync builds of the same functions at once, from several threads, round after round.
static int stress(id<MTLDevice> dev, id<MTLCommandQueue> q, int rounds) {
    const int nk = 12, nr = 6; const uint32_t salt = rnd();
    static kernel_t ks_store[12]; kernel_t *ks = ks_store; NSString *src = make_source(ks, nk, nr, salt);
    id<MTLLibrary> lib = [dev newLibraryWithSource:src options:nil error:nil];
    NSMutableArray *kf = [NSMutableArray array], *ff = [NSMutableArray array];
    for (int i = 0; i < nk; i++) [kf addObject:[lib newFunctionWithName:[NSString stringWithFormat:@"k%d_%08x", i, salt]]];
    for (int i = 0; i < nr; i++) [ff addObject:[lib newFunctionWithName:[NSString stringWithFormat:@"f%d_%08x", i, salt]]];
    id<MTLFunction> vf = [lib newFunctionWithName:@"vfull"];
    __block int bad = 0;
    for (int round = 0; round < rounds; round++) {
        dispatch_group_t g = dispatch_group_create();
        for (int i = 0; i < nk; i++) {
            dispatch_group_enter(g);
            [dev newComputePipelineStateWithFunction:kf[i] completionHandler:^(id<MTLComputePipelineState> ps, NSError *e) {
                if (!check_compute(dev, q, ps, &ks[i])) __sync_fetch_and_add(&bad, 1); dispatch_group_leave(g); }];
        }
        for (int i = 0; i < nr; i++) {
            MTLRenderPipelineDescriptor *d = [MTLRenderPipelineDescriptor new];
            d.vertexFunction = vf; d.fragmentFunction = ff[i]; d.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
            dispatch_group_enter(g);
            [dev newRenderPipelineStateWithDescriptor:d completionHandler:^(id<MTLRenderPipelineState> ps, NSError *e) {
                if (!check_render(dev, q, ps, ks[i].color)) __sync_fetch_and_add(&bad, 1); dispatch_group_leave(g); }];
        }
        dispatch_apply(4, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^(size_t t) {
            for (int i = (int)t; i < nk; i += 4) if (!check_compute(dev, q, [dev newComputePipelineStateWithFunction:kf[i] error:nil], &ks[i])) __sync_fetch_and_add(&bad, 1);
        });
        dispatch_group_wait(g, DISPATCH_TIME_FOREVER);
    }
    printf("stress: %d rounds of %d async + %d sync builds of shared functions: %d wrong\n", rounds, nk + nr, nk, bad);
    return bad;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        const int nk = argc > 1 ? atoi(argv[1]) : 32, nr = argc > 2 ? atoi(argv[2]) : 16, rounds = argc > 3 ? atoi(argv[3]) : 0;
        if (nk < 1 || nk > 512 || nr < 0 || nr > 512) { printf("bad counts\n"); return 2; }
        rng_state = (uint32_t)(CFAbsoluteTimeGetCurrent() * 1000.0) ^ (uint32_t)getpid();
        int pc = 0, lc = 0; size_t sz = sizeof pc; sysctlbyname("hw.physicalcpu", &pc, &sz, NULL, 0); sz = sizeof lc; sysctlbyname("hw.logicalcpu", &lc, &sz, NULL, 0);
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); id<MTLCommandQueue> q = [dev newCommandQueue];
        printf("device %s | %d physical / %d logical cores | %.0f GB\n", dev.name.UTF8String, pc, lc, NSProcessInfo.processInfo.physicalMemory / 1073741824.0);
        result_t s = build_set(dev, q, nk, nr, NO, "one at a time (sync)");
        result_t a = build_set(dev, q, nk, nr, YES, "all at once (async)");
        printf("async speed-up: %.2fx\n", s.secs / a.secs);
        int bad = (s.ok != s.total) + (a.ok != a.total);
        if (rounds > 0) bad += stress(dev, q, rounds);
        printf("%s\n", bad ? "FAIL" : "OK");
        return bad ? 1 : 0;
    }
}
