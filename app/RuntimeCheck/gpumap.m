// nullmoth-gpumap: the measurements behind "Map my GPU" (experimental, opt-in). One test per run, chosen by the only
// argument (identity | correct | bench | corpus); prints one JSON object. The app runs each test as its own process
// with a time limit, so a test that hangs the GPU or crashes costs that test only.
//
// It runs as its own process for the reason nullmoth-metal-devices does: the app's library validation refuses the
// NVIDIA Metal plugin. It changes nothing on the Mac: no display modes, no files, no settings.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <stdio.h>

static id<MTLDevice> pick(void)
{
    id<MTLDevice> best = nil;
    for (id<MTLDevice> d in MTLCopyAllDevices()) {
        if ([d.name rangeOfString:@"NVIDIA"].location != NSNotFound) return d;
        if (!best && !d.lowPower) best = d;
    }
    return best ? best : MTLCreateSystemDefaultDevice();
}

static void emit(NSDictionary *o)
{
    NSData *j = [NSJSONSerialization dataWithJSONObject:o options:NSJSONWritingSortedKeys error:nil];
    fwrite(j.bytes, 1, j.length, stdout); fputc('\n', stdout);
}

static NSString *const kSrc = @
"#include <metal_stdlib>\n"
"using namespace metal;\n"
"kernel void fill(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { o[i] = i * 2654435761u + 7u; }\n"
"kernel void copyk(device const float4 *a [[buffer(0)]], device float4 *b [[buffer(1)]], uint i [[thread_position_in_grid]]) { b[i] = a[i]; }\n"
"kernel void fma32(device float *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {\n"
"  float a = float(i) * 1e-6f, b = 1.0001f, c = 0.9999f, x = a, y = a + 1.0f, z = a + 2.0f, w = a + 3.0f;\n"
"  for (int k = 0; k < 256; k++) { x = fma(x, b, c); y = fma(y, c, b); z = fma(z, b, c); w = fma(w, c, b); }\n"
"  o[i] = x + y + z + w; }\n"
"kernel void fma16(device half *o [[buffer(0)]], uint i [[thread_position_in_grid]]) {\n"
"  half a = half(i & 1023) * 0.001h, b = 1.001h, c = 0.999h, x = a, y = a + 1.0h, z = a + 2.0h, w = a + 3.0h;\n"
"  for (int k = 0; k < 256; k++) { x = fma(x, b, c); y = fma(y, c, b); z = fma(z, b, c); w = fma(w, c, b); }\n"
"  o[i] = x + y + z + w; }\n"
"kernel void blur(texture2d<float, access::read> s [[texture(0)]], texture2d<float, access::write> d [[texture(1)]],\n"
"                 uint2 p [[thread_position_in_grid]]) {\n"
"  float4 acc = 0; for (int dy = -1; dy <= 1; dy++) for (int dx = -1; dx <= 1; dx++)\n"
"    acc += s.read(uint2(clamp(int2(p) + int2(dx, dy), int2(0), int2(s.get_width() - 1, s.get_height() - 1))));\n"
"  d.write(acc / 9.0f, p); }\n";

// Small kernels that each use one feature games and apps lean on. Each compiles and runs on its own; a refusal or a
// wrong result names the feature, which is what the analysis needs.
static NSArray<NSArray<NSString *> *> *corpus(void)
{
    return @[
        @[@"simdgroup_sum", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { o[i] = simd_sum(1u); }", @"32"],
        @[@"atomic_add", @"kernel void t(device atomic_uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { atomic_fetch_add_explicit(o, 1u, memory_order_relaxed); }", @"atomic"],
        @[@"threadgroup_memory", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]], uint l [[thread_position_in_threadgroup]]) { threadgroup uint s[64]; s[l] = l; threadgroup_barrier(mem_flags::mem_threadgroup); o[i] = s[63 - l]; }", @"tg"],
        @[@"half_math", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { half h = half(i & 7) * 0.5h; o[i] = uint(h * 2.0h); }", @"half"],
        @[@"int64", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { ulong v = ulong(i) << 33; o[i] = uint(v >> 33); }", @"id"],
        @[@"fast_math", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { float s = fast::sin(0.0f) + fast::exp(0.0f); o[i] = uint(s); }", @"one"],
        @[@"precise_trig", @"kernel void t(device uint *o [[buffer(0)]], uint i [[thread_position_in_grid]]) { float s = precise::cos(0.0f); o[i] = uint(s); }", @"one"],
    ];
}

static NSString *errStr(NSError *e) { return e ? [e.localizedDescription substringToIndex:MIN((NSUInteger)240, e.localizedDescription.length)] : @""; }

static double runGrid(id<MTLCommandQueue> q, id<MTLComputePipelineState> ps, NSArray<id<MTLBuffer>> *bufs,
                      NSUInteger n, NSString **err)
{
    id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLComputeCommandEncoder> e = [cb computeCommandEncoder];
    [e setComputePipelineState:ps];
    for (NSUInteger i = 0; i < bufs.count; i++) [e setBuffer:bufs[i] offset:0 atIndex:i];
    NSUInteger tg = MIN(ps.maxTotalThreadsPerThreadgroup, (NSUInteger)256);
    [e dispatchThreads:MTLSizeMake(n, 1, 1) threadsPerThreadgroup:MTLSizeMake(tg, 1, 1)];
    [e endEncoding];
    [cb commit]; [cb waitUntilCompleted];
    if (cb.status == MTLCommandBufferStatusError) { if (err) *err = errStr(cb.error); return -1; }
    const double g = cb.GPUEndTime - cb.GPUStartTime;
    return g > 0 ? g : -1;
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        const char *test = argc > 1 ? argv[1] : "identity";
        id<MTLDevice> dev = pick();
        if (!dev) { emit(@{ @"test": @(test), @"error": @"no Metal device" }); return 2; }
        NSMutableDictionary *o = [@{ @"test": @(test), @"device": dev.name ?: @"" } mutableCopy];

        if (!strcmp(test, "identity")) {
            NSMutableArray *fam = [NSMutableArray array];
            struct { MTLGPUFamily f; const char *n; } fams[] = {
                {MTLGPUFamilyCommon1, "Common1"}, {MTLGPUFamilyCommon2, "Common2"}, {MTLGPUFamilyCommon3, "Common3"},
                {MTLGPUFamilyMac2, "Mac2"}, {MTLGPUFamilyMetal3, "Metal3"}, {MTLGPUFamilyApple7, "Apple7"},
                {MTLGPUFamilyApple8, "Apple8"}, {MTLGPUFamilyApple9, "Apple9"} };
            for (unsigned i = 0; i < sizeof fams / sizeof fams[0]; i++) if ([dev supportsFamily:fams[i].f]) [fam addObject:@(fams[i].n)];
            [o addEntriesFromDictionary:@{
                @"registryID": @(dev.registryID), @"families": fam, @"lowPower": @(dev.lowPower), @"removable": @(dev.removable),
                @"unifiedMemory": @(dev.hasUnifiedMemory), @"workingSetBytes": @(dev.recommendedMaxWorkingSetSize),
                @"maxBufferBytes": @(dev.maxBufferLength), @"maxThreadgroupMemory": @(dev.maxThreadgroupMemoryLength),
                @"maxThreadsPerThreadgroup": @[@(dev.maxThreadsPerThreadgroup.width), @(dev.maxThreadsPerThreadgroup.height), @(dev.maxThreadsPerThreadgroup.depth)],
                @"raytracing": @(dev.supportsRaytracing), @"functionPointers": @(dev.supportsFunctionPointers),
                @"argumentBuffersTier": @((int)dev.argumentBuffersSupport), @"readWriteTextureTier": @((int)dev.readWriteTextureSupport),
                @"bcTextures": @(dev.supportsBCTextureCompression), @"depth24Stencil8": @(dev.depth24Stencil8PixelFormatSupported),
                @"location": @((int)dev.location), @"peerGroupID": @(dev.peerGroupID) }];
            emit(o); return 0;
        }

        NSError *e = nil;
        id<MTLCommandQueue> q = [dev newCommandQueue];

        if (!strcmp(test, "corpus")) {
            NSMutableArray *res = [NSMutableArray array];
            for (NSArray<NSString *> *c in corpus()) {
                NSString *src = [@"#include <metal_stdlib>\nusing namespace metal;\n" stringByAppendingString:c[1]];
                NSError *ce = nil;
                id<MTLLibrary> lib = [dev newLibraryWithSource:src options:nil error:&ce];
                id<MTLFunction> fn = [lib newFunctionWithName:@"t"];
                id<MTLComputePipelineState> ps = fn ? [dev newComputePipelineStateWithFunction:fn error:&ce] : nil;
                if (!ps) { [res addObject:@{ @"name": c[0], @"ok": @NO, @"stage": lib ? @"pipeline" : @"compile", @"error": errStr(ce) }]; continue; }
                const NSUInteger n = 64;
                id<MTLBuffer> b = [dev newBufferWithLength:n * 4 options:MTLResourceStorageModeShared];
                memset(b.contents, 0, n * 4);
                NSString *re = nil;
                const double t = runGrid(q, ps, @[b], n, &re);
                const uint32_t *v = b.contents; bool ok = t >= 0;
                for (NSUInteger i = 0; ok && i < n; i++) {
                    const uint32_t want = [c[2] isEqualToString:@"32"] ? 32u : [c[2] isEqualToString:@"tg"] ? (uint32_t)(63 - i % 64)
                        : [c[2] isEqualToString:@"half"] ? (uint32_t)(i & 7) : [c[2] isEqualToString:@"id"] ? (uint32_t)i
                        : [c[2] isEqualToString:@"one"] ? 1u : 0u;
                    if ([c[2] isEqualToString:@"atomic"]) { ok = v[0] == n; break; }
                    if ([c[2] isEqualToString:@"32"]) { ok = v[i] == 32u || v[i] == ps.threadExecutionWidth; continue; }
                    ok = v[i] == want;
                }
                [res addObject:@{ @"name": c[0], @"ok": @(ok), @"stage": t < 0 ? @"run" : @"result", @"error": re ?: @"",
                                  @"simdWidth": @(ps.threadExecutionWidth) }];
            }
            o[@"results"] = res; emit(o); return 0;
        }

        id<MTLLibrary> lib = [dev newLibraryWithSource:kSrc options:nil error:&e];
        if (!lib) { o[@"error"] = [@"compile: " stringByAppendingString:errStr(e)]; emit(o); return 3; }
        id<MTLComputePipelineState> (^pso)(NSString *) = ^id<MTLComputePipelineState>(NSString *name) {
            id<MTLFunction> f = [lib newFunctionWithName:name]; NSError *pe = nil;
            return f ? [dev newComputePipelineStateWithFunction:f error:&pe] : nil;
        };

        if (!strcmp(test, "correct")) {
            const NSUInteger n = 1 << 20;
            id<MTLBuffer> b = [dev newBufferWithLength:n * 4 options:MTLResourceStorageModeShared];
            memset(b.contents, 0, n * 4);
            id<MTLComputePipelineState> ps = pso(@"fill"); NSString *re = nil;
            const double t = ps ? runGrid(q, ps, @[b], n, &re) : -1;
            NSUInteger bad = 0; const uint32_t *v = b.contents;
            for (NSUInteger i = 0; i < n; i++) if (v[i] != (uint32_t)(i * 2654435761u + 7u)) bad++;
            // a blit between private and shared memory, the path uploads and readbacks take
            const NSUInteger bytes = 64u << 20;
            id<MTLBuffer> src = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared];
            id<MTLBuffer> mid = [dev newBufferWithLength:bytes options:MTLResourceStorageModePrivate];
            id<MTLBuffer> dst = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared];
            uint32_t *s = src.contents; for (NSUInteger i = 0; i < bytes / 4; i++) s[i] = (uint32_t)(i ^ 0x5a5a5a5au);
            memset(dst.contents, 0, bytes);
            id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
            [bl copyFromBuffer:src sourceOffset:0 toBuffer:mid destinationOffset:0 size:bytes];
            [bl copyFromBuffer:mid sourceOffset:0 toBuffer:dst destinationOffset:0 size:bytes];
            [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
            const bool blitOk = cb.status != MTLCommandBufferStatusError && !memcmp(src.contents, dst.contents, bytes);
            [o addEntriesFromDictionary:@{ @"fillWords": @(n), @"fillWrong": @(bad), @"fillOk": @(t >= 0 && bad == 0),
                                           @"fillError": re ?: @"", @"blitRoundTripOk": @(blitOk),
                                           @"blitError": cb.status == MTLCommandBufferStatusError ? errStr(cb.error) : @"" }];
            emit(o); return (t >= 0 && bad == 0 && blitOk) ? 0 : 1;
        }

        if (!strcmp(test, "bench")) {
            NSMutableDictionary *r = [NSMutableDictionary dictionary];
            const NSUInteger n4 = 16u << 20;   // 16 Mi float4 = 256 MB each way
            id<MTLBuffer> a = [dev newBufferWithLength:n4 * 16 options:MTLResourceStorageModePrivate];
            id<MTLBuffer> b = [dev newBufferWithLength:n4 * 16 options:MTLResourceStorageModePrivate];
            id<MTLComputePipelineState> ck = pso(@"copyk"), f32 = pso(@"fma32"), f16 = pso(@"fma16"), bl = pso(@"blur");
            if (a && b && ck) {
                runGrid(q, ck, @[a, b], n4, nil);   // warm-up
                double best = 1e9; for (int k = 0; k < 3; k++) { double t = runGrid(q, ck, @[a, b], n4, nil); if (t > 0 && t < best) best = t; }
                if (best < 1e9) r[@"copyGBps"] = @(round(2.0 * n4 * 16 / best / 1e9 * 10) / 10);
            }
            const NSUInteger nt = 1u << 22;
            id<MTLBuffer> ob = [dev newBufferWithLength:nt * 4 options:MTLResourceStorageModePrivate];
            for (NSArray *p in @[@[@"fp32GFlops", f32 ?: (id)NSNull.null], @[@"fp16GFlops", f16 ?: (id)NSNull.null]]) {
                if (p[1] == NSNull.null) continue;
                runGrid(q, p[1], @[ob], nt, nil);
                double best = 1e9; for (int k = 0; k < 3; k++) { double t = runGrid(q, p[1], @[ob], nt, nil); if (t > 0 && t < best) best = t; }
                if (best < 1e9) r[p[0]] = @(round((double)nt * 256 * 4 * 2 / best / 1e9));   // 4 chains x 256 FMA x 2 flops
            }
            if (bl) {
                MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:4096 height:4096 mipmapped:NO];
                td.usage = MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite; td.storageMode = MTLStorageModePrivate;
                id<MTLTexture> s = [dev newTextureWithDescriptor:td], d = [dev newTextureWithDescriptor:td];
                double best = 1e9;
                for (int k = 0; k < 4 && s && d; k++) {
                    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> en = [cb computeCommandEncoder];
                    [en setComputePipelineState:bl]; [en setTexture:s atIndex:0]; [en setTexture:d atIndex:1];
                    [en dispatchThreads:MTLSizeMake(4096, 4096, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)];
                    [en endEncoding]; [cb commit]; [cb waitUntilCompleted];
                    const double t = cb.GPUEndTime - cb.GPUStartTime;
                    if (k && t > 0 && t < best) best = t;
                }
                if (best < 1e9) r[@"blur4kMs"] = @(round(best * 1e5) / 100);
            }
            o[@"results"] = r; emit(o); return 0;
        }

        o[@"error"] = @"unknown test"; emit(o); return 2;
    }
}
