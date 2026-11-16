// Isolated GPU regression/benchmark for temporary texture allocation and retirement.
// clang -fobjc-arc -O2 tools/bench_texture_churn.m -framework Foundation -framework Metal -framework IOKit -o bench_texture_churn
// bench_texture_churn /absolute/path/NVMTLDriver [frames=160] [textures=16] [threads=1]
// Three frames per thread stay in flight; every texture's four corners are read back.
// Sizes include an 8 MiB image which must retain the ordinary allocation path.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#include <dlfcn.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>

@interface NSObject (NVMTLProbe)
- (instancetype)initWithAcceleratorPort:(mach_port_t)port;
@end

static double milliseconds(clockid_t clock) {
    struct timespec t; clock_gettime(clock, &t);
    return t.tv_sec * 1000.0 + t.tv_nsec / 1e6;
}
static int compare(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}
static unsigned channel(int frame, int texture, unsigned seed, int c) {
    const unsigned fm[] = {7, 11, 17}, tm[] = {19, 13, 5};
    return ((unsigned)frame * fm[c] + (unsigned)texture * tm[c] + seed * 31) % 200 + 24;
}

typedef struct { int failed; double create_ms, encode_ms; } result;
static result run(id<MTLDevice> dev, int frames, int count, unsigned seed) { @autoreleasepool {
    id<MTLCommandQueue> queue = [dev newCommandQueue];
    if (!queue) return (result){.failed = 1};
    enum { RING = 3, CORNERS = 4, STRIDE = CORNERS * 256 };
    id<MTLBuffer> read[RING]; id<MTLCommandBuffer> cbs[RING] = {nil}; int previous[RING];
    for (int k = 0; k < RING; k++) {
        read[k] = [dev newBufferWithLength:STRIDE * count options:MTLResourceStorageModeShared];
        if (!read[k]) return (result){.failed = 1};
    }
    __block int bad = 0;
    void (^verify)(id<MTLCommandBuffer>, id<MTLBuffer>, int) = ^(id<MTLCommandBuffer> done, id<MTLBuffer> data, int frame) {
        if (done.status != MTLCommandBufferStatusCompleted) {
            fprintf(stderr, "command failed: %s\n", done.error.description.UTF8String); bad++; return;
        }
        const unsigned char *px = data.contents;
        for (int i = 0; i < count; i++) for (int corner = 0; corner < CORNERS; corner++) {
            const unsigned char *p = px + i * STRIDE + corner * 256;
            int r = channel(frame, i, seed, 0), g = channel(frame, i, seed, 1), b = channel(frame, i, seed, 2);
            if (abs(p[0] - b) > 1 || abs(p[1] - g) > 1 || abs(p[2] - r) > 1 || p[3] != 255) {
                if (bad++ < 5) fprintf(stderr, "thread=%u frame=%d texture=%d corner=%d pixel=%u,%u,%u,%u expected=%d,%d,%d,255\n",
                    seed, frame, i, corner, p[0], p[1], p[2], p[3], b, g, r);
            }
        }
    };
    double *cpu = calloc((size_t)frames, sizeof *cpu), *create = calloc((size_t)frames, sizeof *create);
    if (!cpu || !create) { free(cpu); free(create); return (result){.failed = 1}; }
    const unsigned sizes[][2] = {{128,128}, {256,256}, {512,384}, {640,360}, {960,540}, {384,272}, {2048,1024}, {192,136}};
    for (int f = 0; f < frames; f++) @autoreleasepool {
        int slot = f % RING;
        if (cbs[slot]) { [cbs[slot] waitUntilCompleted]; verify(cbs[slot], read[slot], previous[slot]); cbs[slot] = nil; }
        double begin = milliseconds(CLOCK_THREAD_CPUTIME_ID);
        NSMutableArray<id<MTLTexture>> *textures = [NSMutableArray new];
        for (int i = 0; i < count; i++) {
            unsigned k = ((unsigned)(i + f) + seed) % 8;
            MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                width:sizes[k][0] height:sizes[k][1] mipmapped:NO];
            td.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; td.storageMode = MTLStorageModePrivate;
            id<MTLTexture> t = [dev newTextureWithDescriptor:td];
            if (!t) { fprintf(stderr, "texture allocation failed\n"); free(cpu); free(create); return (result){.failed = 1}; }
            [textures addObject:t];
        }
        create[f] = milliseconds(CLOCK_THREAD_CPUTIME_ID) - begin;
        id<MTLCommandBuffer> cb = [queue commandBuffer];
        if (!cb) { free(cpu); free(create); return (result){.failed = 1}; }
        for (int i = 0; i < count; i++) {
            MTLRenderPassDescriptor *pd = [MTLRenderPassDescriptor renderPassDescriptor];
            pd.colorAttachments[0].texture = textures[i];
            pd.colorAttachments[0].loadAction = MTLLoadActionClear; pd.colorAttachments[0].storeAction = MTLStoreActionStore;
            pd.colorAttachments[0].clearColor = MTLClearColorMake(channel(f, i, seed, 0) / 255., channel(f, i, seed, 1) / 255., channel(f, i, seed, 2) / 255., 1);
            id<MTLRenderCommandEncoder> enc = [cb renderCommandEncoderWithDescriptor:pd];
            if (!enc) { free(cpu); free(create); return (result){.failed = 1}; }
            [enc endEncoding];
        }
        id<MTLBlitCommandEncoder> blit = [cb blitCommandEncoder];
        if (!blit) { free(cpu); free(create); return (result){.failed = 1}; }
        for (int i = 0; i < count; i++) for (int corner = 0; corner < CORNERS; corner++) {
            id<MTLTexture> t = textures[i];
            MTLOrigin origin = MTLOriginMake((corner & 1) ? t.width - 1 : 0, (corner & 2) ? t.height - 1 : 0, 0);
            [blit copyFromTexture:t sourceSlice:0 sourceLevel:0 sourceOrigin:origin sourceSize:MTLSizeMake(1,1,1)
                toBuffer:read[slot] destinationOffset:i * STRIDE + corner * 256 destinationBytesPerRow:256 destinationBytesPerImage:256];
        }
        [blit endEncoding]; [cb commit]; cbs[slot] = cb; previous[slot] = f;
        cpu[f] = milliseconds(CLOCK_THREAD_CPUTIME_ID) - begin;
    }
    for (int k = 0; k < RING; k++) if (cbs[k]) { [cbs[k] waitUntilCompleted]; verify(cbs[k], read[k], previous[k]); cbs[k] = nil; }
    qsort(cpu + 4, (size_t)frames - 4, sizeof *cpu, compare);
    qsort(create + 4, (size_t)frames - 4, sizeof *create, compare);
    result output = {bad, create[4 + (frames - 4) / 2], cpu[4 + (frames - 4) / 2]};
    free(cpu); free(create);
    return output;
} }

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 2) return 2;
    int frames = argc > 2 ? atoi(argv[2]) : 160, count = argc > 3 ? atoi(argv[3]) : 16, threads = argc > 4 ? atoi(argv[4]) : 1;
    if (frames < 8 || frames > 4096 || count < 1 || count > 128 || threads < 1 || threads > 8) return 2;
    if (!dlopen(argv[1], RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr, "%s\n", dlerror()); return 2; }
    io_service_t svc = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    id<MTLDevice> dev = [[NSClassFromString(@"NVMTLDevice") alloc] initWithAcceleratorPort:svc];
    if (svc) IOObjectRelease(svc);
    if (!dev) return 2;
    result *results = calloc((size_t)threads, sizeof *results); if (!results) return 2;
    double start = milliseconds(CLOCK_PROCESS_CPUTIME_ID), wall = milliseconds(CLOCK_MONOTONIC);
    dispatch_apply((size_t)threads, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^(size_t i) {
        results[i] = run(dev, frames, count, (unsigned)i);
    });
    double process = milliseconds(CLOCK_PROCESS_CPUTIME_ID) - start, elapsed = milliseconds(CLOCK_MONOTONIC) - wall;
    int bad = 0;
    for (int i = 0; i < threads; i++) {
        bad += results[i].failed;
        printf("%s thread=%d frames=%d textures=%d bad=%d median_create_cpu_ms=%.6f median_encode_submit_cpu_ms=%.6f\n",
            results[i].failed ? "FAIL" : "PASS", i, frames, count, results[i].failed, results[i].create_ms, results[i].encode_ms);
    }
    free(results);
    printf("%s threads=%d total_process_cpu_ms=%.3f wall_ms=%.3f\n", bad ? "FAIL" : "PASS", threads, process, elapsed);
    return bad ? 1 : 0;
} }
