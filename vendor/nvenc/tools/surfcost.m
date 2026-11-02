// surfcost.m: research probe (VRAM capture path): per-frame costs of today's surface paths at a given size.
//   B1 NV12 two-plane IOSurface rendered by Metal (the plugin's private-image fallback: CPU copy-out)
//   B2 the same planes as two one-plane IOSurfaces (imported VRAM, no copy)
//   B3 BGRA IOSurface rendered by Metal, then locked by the CPU (page-off: copy engine VRAM -> system RAM)
//   C  a BGRA frame copied by the CPU into VRAM through a BAR1 mapping (as NVAccel's display copy path), and read back
//   ./surfcost W H [N]
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include "rm.h"
#include <sys/resource.h>
#include <time.h>

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
static double cpu_ms(void) { struct rusage r; getrusage(RUSAGE_SELF, &r); return (r.ru_utime.tv_sec + r.ru_stime.tv_sec) * 1e3 + (r.ru_utime.tv_usec + r.ru_stime.tv_usec) / 1e3; }

static NSString *kSrc = @
"#include <metal_stdlib>\n using namespace metal;\n"
"kernel void f1(texture2d<float, access::write> t [[texture(0)]], constant uint &k [[buffer(0)]], uint2 g [[thread_position_in_grid]]) {\n"
"  if (g.x < t.get_width() && g.y < t.get_height()) t.write(float4(((g.x + g.y + k) & 255) / 255.0), g); }\n"
"kernel void f2(texture2d<float, access::write> t [[texture(0)]], constant uint &k [[buffer(0)]], uint2 g [[thread_position_in_grid]]) {\n"
"  if (g.x < t.get_width() && g.y < t.get_height()) t.write(float4(((g.x + k) & 255) / 255.0, ((g.y + k) & 255) / 255.0, 0, 1), g); }\n";

static IOSurfaceRef plane_surface(int w, int h, int bpe, uint32_t fmt)
{
    size_t bpr = ((size_t)w * bpe + 63) & ~(size_t)63;
    return IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(w), (id)kIOSurfaceHeight: @(h), (id)kIOSurfaceBytesPerElement: @(bpe),
        (id)kIOSurfaceBytesPerRow: @(bpr), (id)kIOSurfacePixelFormat: @(fmt) });
}

int main(int argc, char **argv) { @autoreleasepool {
    int W = argc > 1 ? atoi(argv[1]) : 3840, H = argc > 2 ? atoi(argv[2]) : 2160, N = argc > 3 ? atoi(argv[3]) : 30;
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:kSrc options:nil error:&err];
    id<MTLComputePipelineState> p1 = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"f1"] error:&err];
    id<MTLComputePipelineState> p2 = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"f2"] error:&err];
    id<MTLCommandQueue> q = [dev newCommandQueue];
    // render luma + chroma textures; returns wall and CPU ms per frame (commit + wait included)
    double (^run)(id<MTLTexture>, id<MTLTexture>, double *) = ^double(id<MTLTexture> tl, id<MTLTexture> tc, double *cpu) {
        double w = 0, c = 0;
        for (uint32_t k = 0; k < (uint32_t)N; k++) {
            double t0 = now_ms(), c0 = cpu_ms();
            id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
            [ce setComputePipelineState:p1]; [ce setTexture:tl atIndex:0]; [ce setBytes:&k length:4 atIndex:0];
            [ce dispatchThreads:MTLSizeMake(tl.width, tl.height, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)];
            if (tc) { [ce setComputePipelineState:p2]; [ce setTexture:tc atIndex:0];
                      [ce dispatchThreads:MTLSizeMake(tc.width, tc.height, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; }
            [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];
            if (k >= 3) { w += now_ms() - t0; c += cpu_ms() - c0; }
        }
        *cpu = c / (N - 3); return w / (N - 3);
    };
    MTLTextureDescriptor *dl = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm width:W height:H mipmapped:NO];
    MTLTextureDescriptor *dc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRG8Unorm width:W / 2 height:H / 2 mipmapped:NO];
    dl.usage = dc.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead;

    // B1: NV12, two planes in one IOSurface
    size_t lbpr = ((size_t)W + 63) & ~(size_t)63, ysz = lbpr * H;
    IOSurfaceRef nv = IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfacePixelFormat: @((uint32_t)'420f'),
        (id)kIOSurfacePlaneInfo: @[ @{ (id)kIOSurfacePlaneWidth: @(W), (id)kIOSurfacePlaneHeight: @(H), (id)kIOSurfacePlaneBytesPerElement: @1, (id)kIOSurfacePlaneBytesPerRow: @(lbpr), (id)kIOSurfacePlaneOffset: @0 },
                                    @{ (id)kIOSurfacePlaneWidth: @(W / 2), (id)kIOSurfacePlaneHeight: @(H / 2), (id)kIOSurfacePlaneBytesPerElement: @2, (id)kIOSurfacePlaneBytesPerRow: @(lbpr), (id)kIOSurfacePlaneOffset: @(ysz) } ],
        (id)kIOSurfaceAllocSize: @(ysz * 3 / 2) });
    double c1, w1 = run([dev newTextureWithDescriptor:dl iosurface:nv plane:0], [dev newTextureWithDescriptor:dc iosurface:nv plane:1], &c1);
    // B2: the same planes as two one-plane surfaces
    IOSurfaceRef sl = plane_surface(W, H, 1, 'L008'), sc = plane_surface(W / 2, H / 2, 2, '2C08');
    double c2, w2 = run([dev newTextureWithDescriptor:dl iosurface:sl plane:0], [dev newTextureWithDescriptor:dc iosurface:sc plane:0], &c2);
    printf("%dx%d NV12 rendered by Metal, per frame (commit + wait):\n", W, H);
    printf("  B1 two-plane IOSurface (plugin fallback, CPU copy-out): %.3f ms wall, %.3f ms CPU\n", w1, c1);
    printf("  B2 two one-plane IOSurfaces (VRAM import):              %.3f ms wall, %.3f ms CPU\n", w2, c2);

    // B3: BGRA rendered by Metal, then a CPU lock each frame
    IOSurfaceRef bg = plane_surface(W, H, 4, 'BGRA');
    MTLTextureDescriptor *db = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
    db.usage = MTLTextureUsageShaderWrite;
    id<MTLTexture> tb = [dev newTextureWithDescriptor:db iosurface:bg plane:0];
    double wr = 0, wl = 0, cl = 0;
    for (uint32_t k = 0; k < (uint32_t)N; k++) {
        double t0 = now_ms();
        id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:p2]; [ce setTexture:tb atIndex:0]; [ce setBytes:&k length:4 atIndex:0];
        [ce dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];
        double t1 = now_ms(), c0 = cpu_ms();
        IOSurfaceLock(bg, kIOSurfaceLockReadOnly, NULL); volatile uint8_t x = ((uint8_t *)IOSurfaceGetBaseAddress(bg))[12345]; (void)x; IOSurfaceUnlock(bg, kIOSurfaceLockReadOnly, NULL);
        if (k >= 3) { wr += t1 - t0; wl += now_ms() - t1; cl += cpu_ms() - c0; }
    }
    printf("  B3 BGRA rendered by Metal: %.3f ms wall; CPU lock after it (page-off): %.3f ms wall, %.3f ms CPU\n", wr / (N - 3), wl / (N - 3), cl / (N - 3));

    // C: CPU copy of one BGRA frame into VRAM through BAR1 (write-combined), as the display copy path does
    Rm rm = { 0 };
    if (rm_open(&rm) || rm_va_init(&rm)) return 1;
    size_t fsz = (size_t)W * H * 4;
    RmMem vm;
    if (rm_mem_alloc(&rm, &vm, fsz, 0x10000, false, true, false)) { printf("vidmem alloc failed\n"); return 1; }
    uint32_t *src = malloc(fsz); for (size_t i = 0; i < fsz / 4; i++) src[i] = (uint32_t)(i * 2654435761u);
    double tw = 0, tm = 0, tr = 0;
    for (int k = 0; k < 6; k++) {
        double t0 = now_ms(); volatile uint32_t *d = vm.cpu; for (size_t i = 0; i < fsz / 4; i++) d[i] = src[i]; double t1 = now_ms();
        memcpy(vm.cpu, src, fsz); double t2 = now_ms();
        if (k >= 1) { tw += t1 - t0; tm += t2 - t1; }
    }
    double t3 = now_ms(); volatile uint32_t sum = 0; const volatile uint32_t *rd = vm.cpu; for (size_t i = 0; i < fsz / 4 / 16; i++) sum += rd[i * 16]; tr = now_ms() - t3;
    printf("C  %dx%d BGRA frame (%zu MB) CPU -> VRAM through BAR1: word loop %.3f ms, memcpy %.3f ms; reading it back: ~%.1f ms per frame (one word per 64 B sampled: %.3f ms)\n",
           W, H, fsz >> 20, tw / 5, tm / 5, tr * 16, tr);
    rm_mem_free(&rm, &vm); rm_close(&rm);
    return 0;
} }
