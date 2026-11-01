// surfvram.m: research probe (VRAM capture path): can our RM client reach the VRAM copy nullmoth's NVAccel keeps for an
// IOSurface? Renders a pattern into a BGRA IOSurface with Metal, asks the driver for the surface's VRAM allocation
// (escape 0xE2, then NV_ESC_RM_DUP_OBJECT into our client), maps it to the GPU and to the CPU (BAR1) and checks the
// pixels. Then tries an NV12 (two-plane) surface. Also times the CPU lock (page-off: copy engine VRAM -> system RAM).
//   ./surfvram W H
// The escape layout below is the driver's published ABI (version 2), declared here, not copied.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include "rm.h"
#include <time.h>
#include "nv_escape.h"
#include "class/cl0070.h"
#include "class/cl50a0.h"

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

#define ESC_SURF_VRAM 0xE2u
typedef struct { uint32_t version, surfaceID, plane, hClient, hSrcClient, hSrcMemory, status, pad; uint64_t size; } SurfShareEsc;

static NSString *kSrc = @
"#include <metal_stdlib>\n using namespace metal;\n"
"kernel void luma(texture2d<float, access::write> t [[texture(0)]], uint2 g [[thread_position_in_grid]]) {\n"
"  if (g.x < t.get_width() && g.y < t.get_height()) t.write(float4(((g.x + 2 * g.y) & 255) / 255.0), g); }\n"
"kernel void fill(texture2d<float, access::write> t [[texture(0)]], uint2 g [[thread_position_in_grid]]) {\n"
"  if (g.x < t.get_width() && g.y < t.get_height()) t.write(float4((g.x & 255) / 255.0, (g.y & 255) / 255.0, ((g.x ^ g.y) & 255) / 255.0, 1), g); }\n";

// share an IOSurface plane's VRAM into our client: returns our handle (0 on failure)
static NvHandle surf_vram(Rm *rm, IOSurfaceRef s, uint32_t plane, uint64_t *size, double *ms)
{
    double t0 = now_ms();
    SurfShareEsc e = { .version = 2, .surfaceID = IOSurfaceGetID(s), .plane = plane, .hClient = rm->client };
    int rc = rm_esc(rm, rm->ctl, ESC_SURF_VRAM, &e, sizeof e);
    printf("  SURF_VRAM surface %u plane %u -> rc %d status 0x%x src client 0x%08x memory 0x%08x size %llu\n", e.surfaceID, plane, rc, e.status,
           e.hSrcClient, e.hSrcMemory, (unsigned long long)e.size);
    if (rc || e.status) return 0;
    NVOS55_PARAMETERS d = { .hClient = rm->client, .hParent = rm->device, .hObject = rm_handle(rm), .hClientSrc = e.hSrcClient, .hObjectSrc = e.hSrcMemory };
    rc = rm_esc(rm, rm->ctl, NV_ESC_RM_DUP_OBJECT, &d, sizeof d);
    printf("  DUP_OBJECT -> rc %d status 0x%x, our handle 0x%08x\n", rc, d.status, d.hObject);
    if (rc || d.status) return 0;
    *size = e.size; *ms = now_ms() - t0;
    return d.hObject;
}

static NvU64 map_gpu(Rm *rm, NvHandle mem, NvU64 size)
{
    NV_MEMORY_ALLOCATION_PARAMS v = { .owner = rm->client, .type = NVOS32_TYPE_IMAGE, .flags = NVOS32_ALLOC_FLAGS_VIRTUAL, .size = size, .hVASpace = rm->vaspace };
    NvHandle hv = rm_handle(rm);
    if (rm_alloc(rm, rm->device, hv, NV50_MEMORY_VIRTUAL, &v)) return 0;
    NVOS46_PARAMETERS m = { .hClient = rm->client, .hDevice = rm->device, .hDma = hv, .hMemory = mem, .length = size,
                            .flags = DRF_DEF(OS46, _FLAGS, _PAGE_KIND, _VIRTUAL) | DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _DISABLE) };
    if (rm_esc(rm, rm->ctl, NV_ESC_RM_MAP_MEMORY_DMA, &m, sizeof m) || m.status) { printf("  MAP_MEMORY_DMA status 0x%x\n", m.status); return 0; }
    return m.dmaOffset;
}

int main(int argc, char **argv) { @autoreleasepool {
    int W = argc > 1 ? atoi(argv[1]) : 1920, H = argc > 2 ? atoi(argv[2]) : 1080;
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    NSError *err = nil;
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[[dev newLibraryWithSource:kSrc options:nil error:&err] newFunctionWithName:@"fill"] error:&err];
    id<MTLCommandQueue> q = [dev newCommandQueue];
    Rm rm = { 0 };
    if (rm_open(&rm) || rm_va_init(&rm)) { printf("rm_open failed\n"); return 1; }

    // ---- BGRA surface rendered by the GPU ----
    size_t bpr = ((size_t)W * 4 + 63) & ~(size_t)63;
    NSDictionary *p = @{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfaceBytesPerElement: @4, (id)kIOSurfaceBytesPerRow: @(bpr),
                         (id)kIOSurfacePixelFormat: @((uint32_t)'BGRA') };
    IOSurfaceRef s = IOSurfaceCreate((__bridge CFDictionaryRef)p);
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
    td.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead;
    id<MTLTexture> t = [dev newTextureWithDescriptor:td iosurface:s plane:0];
    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
    [ce setComputePipelineState:ps]; [ce setTexture:t atIndex:0];
    [ce dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [ce endEncoding];
    [cb commit]; [cb waitUntilCompleted];
    printf("BGRA %dx%d IOSurface %u (bytes per row %zu) rendered by Metal: %s\n", W, H, IOSurfaceGetID(s), bpr, cb.status == MTLCommandBufferStatusCompleted ? "ok" : "FAILED");

    uint64_t size = 0; double ms = 0;
    NvHandle h = surf_vram(&rm, s, 0, &size, &ms);
    if (h) {
        NvU64 va = map_gpu(&rm, h, size);
        printf("  our GPU mapping of the surface's VRAM: 0x%llx (%llu KB); share + dup took %.3f ms\n", (unsigned long long)va, (unsigned long long)(size >> 10), ms);
        RmConn c; void *cpu = NULL;
        NV_STATUS st = rm_map_cpu(&rm, rm.device, h, size, false, &c, &cpu);
        if (!st && cpu) {   // check the pixels through BAR1: Metal wrote (r, g, b) = (x, y, x^y), so memory B = x^y, G = y, R = x
            int bad = 0, checked = 0; size_t pitch = size / (size_t)H >= bpr ? bpr : size / (size_t)H;
            for (int y = 0; y < H; y += 97) for (int x = 0; x < W; x += 61) {
                const uint8_t *px = (const uint8_t *)cpu + (size_t)y * pitch + (size_t)x * 4; checked++;
                if (abs(px[0] - ((x ^ y) & 255)) > 1 || abs(px[1] - (y & 255)) > 1 || abs(px[2] - (x & 255)) > 1) bad++;
            }
            printf("  CPU view through BAR1 (pitch %zu): %d of %d sampled pixels wrong\n", pitch, bad, checked);
            rm_unmap_cpu(&rm, &c);
        } else printf("  CPU mapping refused: 0x%x\n", st);
    }
    // the page-off a CPU lock costs (copy engine VRAM -> system RAM pages)
    double t0 = now_ms(); IOSurfaceLock(s, kIOSurfaceLockReadOnly, NULL); double tl = now_ms() - t0;
    const uint8_t *b = IOSurfaceGetBaseAddress(s);
    printf("  IOSurfaceLock (page-off) %.3f ms; system-RAM pixel (100,50) = %u %u %u (expect %u %u %u)\n", tl, b[50 * bpr + 400], b[50 * bpr + 401], b[50 * bpr + 402], 100 ^ 50, 50, 100);
    IOSurfaceUnlock(s, kIOSurfaceLockReadOnly, NULL);

    // ---- NV12 (two planes), as ScreenCaptureKit hands Parsec ----
    NSDictionary *pl0 = @{ (id)kIOSurfacePlaneWidth: @(W), (id)kIOSurfacePlaneHeight: @(H), (id)kIOSurfacePlaneBytesPerElement: @1, (id)kIOSurfacePlaneBytesPerRow: @(((size_t)W + 63) & ~(size_t)63), (id)kIOSurfacePlaneOffset: @0 };
    size_t ysz = (((size_t)W + 63) & ~(size_t)63) * H;
    NSDictionary *pl1 = @{ (id)kIOSurfacePlaneWidth: @(W / 2), (id)kIOSurfacePlaneHeight: @(H / 2), (id)kIOSurfacePlaneBytesPerElement: @2, (id)kIOSurfacePlaneBytesPerRow: @(((size_t)W + 63) & ~(size_t)63), (id)kIOSurfacePlaneOffset: @(ysz) };
    NSDictionary *pn = @{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfacePixelFormat: @((uint32_t)'420f'), (id)kIOSurfacePlaneInfo: @[pl0, pl1],
                          (id)kIOSurfaceAllocSize: @(ysz * 3 / 2) };
    IOSurfaceRef n = IOSurfaceCreate((__bridge CFDictionaryRef)pn);
    MTLTextureDescriptor *tdl = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm width:W height:H mipmapped:NO];
    tdl.usage = MTLTextureUsageShaderWrite;
    id<MTLTexture> tn = [dev newTextureWithDescriptor:tdl iosurface:n plane:0];
    printf("NV12 %dx%d IOSurface %u, luma plane texture %s\n", W, H, IOSurfaceGetID(n), tn ? "created" : "FAILED");
    if (tn) {
        id<MTLComputePipelineState> pl2 = [dev newComputePipelineStateWithFunction:[[dev newLibraryWithSource:kSrc options:nil error:&err] newFunctionWithName:@"luma"] error:&err];
        cb = [q commandBuffer]; ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:pl2]; [ce setTexture:tn atIndex:0];
        [ce dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [ce endEncoding];
        [cb commit]; [cb waitUntilCompleted];
        printf("  luma rendered by Metal: %s\n", cb.status == MTLCommandBufferStatusCompleted ? "ok" : "FAILED");
    }
    size_t lbpr = ((size_t)W + 63) & ~(size_t)63;
    for (uint32_t pl = 0; pl < 2; pl++) {
        uint64_t sz; double m2; NvHandle hp = surf_vram(&rm, n, pl, &sz, &m2);
        if (!hp || pl) continue;
        RmConn c; void *cpu = NULL;
        if (!rm_map_cpu(&rm, rm.device, hp, sz, false, &c, &cpu) && cpu) {
            int bad = 0, checked = 0;
            for (int y = 0; y < H; y += 97) for (int x = 0; x < W; x += 61) { checked++; if (abs(((const uint8_t *)cpu)[(size_t)y * lbpr + x] - ((x + 2 * y) & 255)) > 1) bad++; }
            printf("  NV12 luma in VRAM (pitch %zu): %d of %d sampled pixels wrong\n", lbpr, bad, checked);
            rm_unmap_cpu(&rm, &c);
        }
    }
    double t1 = now_ms(); IOSurfaceLock(n, kIOSurfaceLockReadOnly, NULL); double tl2 = now_ms() - t1;
    const uint8_t *ny = IOSurfaceGetBaseAddressOfPlane(n, 0);
    printf("  NV12 IOSurfaceLock %.3f ms; system-RAM luma (100,50) = %u (expect %u)\n", tl2, ny[50 * lbpr + 100], (100 + 100) & 255);
    IOSurfaceUnlock(n, kIOSurfaceLockReadOnly, NULL);
    rm_close(&rm);
    return 0;
} }
