// surfread.m: research probe (desktop path): how fast the CPU reads an IOSurface's pages, the way the plugin's surface-in
// does (IOSurfaceLock read-only, then copy the plane into a cached buffer), for a CPU-written and a GPU-written surface.
//   ./surfread [W H [N]]      default 3158x1816 (Safari's layer in the motion test), 10 rounds
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <smmintrin.h>
#include <time.h>

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

static void copy_stream(void *dst, const void *src, size_t n)   // SSE4.1 streaming loads, as the plugin's surface copy
{
    const __m128i *s = (const __m128i *)src; __m128i *d = (__m128i *)dst;
    for (size_t i = 0; i < n / 64; i++, s += 4, d += 4) {
        __m128i a = _mm_stream_load_si128((__m128i *)s), b = _mm_stream_load_si128((__m128i *)s + 1);
        __m128i c = _mm_stream_load_si128((__m128i *)s + 2), e = _mm_stream_load_si128((__m128i *)s + 3);
        _mm_store_si128(d, a); _mm_store_si128(d + 1, b); _mm_store_si128(d + 2, c); _mm_store_si128(d + 3, e);
    }
}

// one locked read of the whole plane: lock time, copy time (memcpy or streaming)
static void read_once(IOSurfaceRef s, void *buf, int stream, double *lock_ms, double *copy_ms)
{
    double t0 = now_ms();
    IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL);
    double t1 = now_ms();
    const void *base = IOSurfaceGetBaseAddress(s); size_t n = IOSurfaceGetAllocSize(s) & ~(size_t)63;
    if (stream) copy_stream(buf, base, n); else memcpy(buf, base, n);
    double t2 = now_ms();
    IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);
    *lock_ms = t1 - t0; *copy_ms = t2 - t1;
}

static IOSurfaceRef make(int W, int H)
{
    size_t bpr = ((size_t)W * 4 + 63) & ~(size_t)63;
    return IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfaceBytesPerElement: @4,
        (id)kIOSurfaceBytesPerRow: @(bpr), (id)kIOSurfacePixelFormat: @((uint32_t)'BGRA') });
}

static void report(const char *what, IOSurfaceRef s, void *buf, int N, void (^before)(int))
{
    for (int stream = 0; stream < 2; stream++) {
        double l = 0, c = 0, l0 = 0, c0 = 0;
        for (int k = 0; k < N; k++) {
            if (before) before(k);
            double lk, ck; read_once(s, buf, stream, &lk, &ck);
            if (k == 0) { l0 = lk; c0 = ck; } else { l += lk; c += ck; }
        }
        double mb = IOSurfaceGetAllocSize(s) / 1048576.0;
        printf("%-34s %-9s first: lock %6.2f ms copy %6.2f ms | later: lock %6.2f ms copy %6.2f ms (%.1f GB/s)\n", what,
               stream ? "stream" : "memcpy", l0, c0, l / (N - 1), c / (N - 1), mb / 1024.0 / (c / (N - 1) / 1e3));
    }
}

int main(int argc, char **argv) { @autoreleasepool {
    int W = argc > 2 ? atoi(argv[1]) : 3158, H = argc > 2 ? atoi(argv[2]) : 1816, N = argc > 3 ? atoi(argv[3]) : 10;
    size_t bpr = ((size_t)W * 4 + 63) & ~(size_t)63;
    void *buf = aligned_alloc(64, (bpr * H + 4095) & ~(size_t)4095);
    printf("%dx%d BGRA, pitch %zu, %.1f MB\n", W, H, bpr, bpr * H / 1048576.0);

    // 1) written by the CPU each round (as CoreGraphics paints a layer), read back
    IOSurfaceRef a = make(W, H);
    report("CPU-written surface", a, buf, N, ^(int k) {
        IOSurfaceLock(a, 0, NULL); memset(IOSurfaceGetBaseAddress(a), k, IOSurfaceGetAllocSize(a)); IOSurfaceUnlock(a, 0, NULL); });

    // 2) the same surface again, not written in between
    report("CPU-written, unchanged since", a, buf, N, nil);

    // 3) rendered by Metal each round (the read pages the VRAM copy off first)
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); id<MTLCommandQueue> q = [dev newCommandQueue];
    IOSurfaceRef g = make(W, H);
    MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
    d.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead;
    id<MTLTexture> t = [dev newTextureWithDescriptor:d iosurface:g plane:0];
    report("Metal-rendered surface", g, buf, N, ^(int k) {
        MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor renderPassDescriptor];
        rp.colorAttachments[0].texture = t; rp.colorAttachments[0].loadAction = MTLLoadActionClear;
        rp.colorAttachments[0].clearColor = MTLClearColorMake(k / 10.0, 0.5, 0.25, 1); rp.colorAttachments[0].storeAction = MTLStoreActionStore;
        id<MTLCommandBuffer> cb = [q commandBuffer]; [[cb renderCommandEncoderWithDescriptor:rp] endEncoding]; [cb commit]; [cb waitUntilCompleted]; });
    return 0;
} }
