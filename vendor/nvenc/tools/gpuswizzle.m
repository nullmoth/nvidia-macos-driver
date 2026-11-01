// gpuswizzle.m: prototype for T5.06: tile an NV12 IOSurface into NVENC's input layout with a Metal compute kernel
// (instead of the CPU pass), time it against the CPU tiler and check the result byte for byte.
//   ./gpuswizzle W H [N]
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <CoreVideo/CoreVideo.h>
#include "blocklinear.h"
#include <time.h>

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

// one thread per 16-byte run of the destination: dst offset -> (x, y) of the source plane (inverse of nvenc_in_offset)
static NSString *kSrc = @
"#include <metal_stdlib>\n"
"using namespace metal;\n"
"struct P { uint pitch; uint width; uint rows; uint bpp; };\n"
"kernel void tile(texture2d<uint, access::read> src [[texture(0)]], device uint4 *dst [[buffer(0)]], constant P &p [[buffer(1)]],\n"
"                 uint gid [[thread_position_in_grid]]) {\n"
"    uint o = gid * 16, band = p.pitch * 16;\n"
"    uint k = o / band, rem = o % band, t = rem / 1024, in = rem % 1024;\n"
"    uint x = t * 64 + ((in >> 8) & 1) * 32 + ((in >> 6) & 1) * 16;\n"
"    uint y = k * 16 + (in >> 9) * 8 + ((in >> 7) & 1) * 4 + ((in >> 4) & 3);\n"
"    uint v[16];\n"
"    for (uint i = 0; i < 16; i++) {\n"
"        uint xb = x + i;\n"
"        if (y < p.rows && xb < p.width) { uint px = xb / p.bpp; uint4 c = src.read(uint2(px, y)); v[i] = p.bpp == 2 ? ((xb & 1) ? c.g : c.r) : c.r; }\n"
"        else v[i] = 0;\n"
"    }\n"
"    dst[gid] = uint4(v[0] | v[1] << 8 | v[2] << 16 | v[3] << 24, v[4] | v[5] << 8 | v[6] << 16 | v[7] << 24,\n"
"                     v[8] | v[9] << 8 | v[10] << 16 | v[11] << 24, v[12] | v[13] << 8 | v[14] << 16 | v[15] << 24);\n"
"}\n"
"// coalesced: the source plane as a buffer (stride bytes per row), one 16-byte load and store per thread\n"
"struct Q { uint pitch; uint width; uint rows; uint stride; };\n"
"kernel void tile16(device const uint4 *src [[buffer(2)]], device uint4 *dst [[buffer(0)]], constant Q &p [[buffer(1)]],\n"
"                   uint gid [[thread_position_in_grid]]) {\n"
"    uint o = gid * 16, band = p.pitch * 16;\n"
"    uint k = o / band, rem = o % band, t = rem / 1024, in = rem % 1024;\n"
"    uint x = t * 64 + ((in >> 8) & 1) * 32 + ((in >> 6) & 1) * 16;\n"
"    uint y = k * 16 + (in >> 9) * 8 + ((in >> 7) & 1) * 4 + ((in >> 4) & 3);\n"
"    dst[gid] = (y < p.rows && x < p.width) ? src[(y * p.stride + x) / 16] : uint4(0);\n"
"}\n";

typedef struct { uint32_t pitch, width, rows, bpp; } Params;

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 3) { fprintf(stderr, "usage: gpuswizzle W H [N]\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]), N = argc > 3 ? atoi(argv[3]) : 20;
    size_t pitch = ((size_t)(W + 15) / 16 * 16 + 63) & ~(size_t)63, hmb = (size_t)(H + 15) / 16;
    size_t lumaSz = (nvenc_in_plane_size(pitch, hmb * 16) + 255) & ~(size_t)255, chromaSz = nvenc_in_plane_size(pitch, hmb * 8);
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    printf("device: %s\n", dev.name.UTF8String);
    NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:kSrc options:nil error:&err];
    if (!lib) { printf("compile: %s\n", err.description.UTF8String); return 1; }
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"tile"] error:&err];
    if (!ps) { printf("pipeline: %s\n", err.description.UTF8String); return 1; }
    id<MTLCommandQueue> q = [dev newCommandQueue];

    // the source: an IOSurface-backed NV12 pixel buffer, as ScreenCaptureKit delivers
    CVPixelBufferRef pb = NULL;
    NSDictionary *attr = @{ (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{}, (__bridge NSString *)kCVPixelBufferMetalCompatibilityKey: @YES };
    CVPixelBufferCreate(NULL, W, H, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, (__bridge CFDictionaryRef)attr, &pb);
    CVPixelBufferLockBaseAddress(pb, 0);
    for (int pl = 0; pl < 2; pl++) {
        uint8_t *b = CVPixelBufferGetBaseAddressOfPlane(pb, pl); size_t bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, pl);
        for (int r = 0; r < (pl ? (H + 1) / 2 : H); r++) for (int c = 0; c < W; c++) b[r * bpr + c] = (uint8_t)(r * 7 + c * 3 + pl * 50 + (c >> 4));
    }
    CVPixelBufferUnlockBaseAddress(pb, 0);
    IOSurfaceRef surf = CVPixelBufferGetIOSurface(pb);
    MTLTextureDescriptor *dl = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Uint width:W height:H mipmapped:NO];
    MTLTextureDescriptor *dc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRG8Uint width:(W + 1) / 2 height:(H + 1) / 2 mipmapped:NO];
    dl.usage = dc.usage = MTLTextureUsageShaderRead;
    id<MTLTexture> tl = [dev newTextureWithDescriptor:dl iosurface:surf plane:0], tc = [dev newTextureWithDescriptor:dc iosurface:surf plane:1];
    if (!tl || !tc) { printf("IOSurface textures failed\n"); return 1; }
    id<MTLBuffer> out = [dev newBufferWithLength:lumaSz + chromaSz options:MTLResourceStorageModeShared];
    memset(out.contents, 0, out.length);

    Params pl = { (uint32_t)pitch, (uint32_t)W, (uint32_t)H, 1 }, pc = { (uint32_t)pitch, (uint32_t)W, (uint32_t)(H + 1) / 2, 2 };
    double wall = 0, gpu = 0, wallMin = 1e9;
    for (int i = 0; i < N; i++) {
        double t0 = now_ms();
        id<MTLCommandBuffer> cb = [q commandBuffer];
        id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:ps];
        [ce setTexture:tl atIndex:0]; [ce setBuffer:out offset:0 atIndex:0]; [ce setBytes:&pl length:sizeof pl atIndex:1];
        [ce dispatchThreads:MTLSizeMake(lumaSz / 16, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        [ce setTexture:tc atIndex:0]; [ce setBuffer:out offset:lumaSz atIndex:0]; [ce setBytes:&pc length:sizeof pc atIndex:1];
        [ce dispatchThreads:MTLSizeMake(chromaSz / 16, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        [ce endEncoding];
        [cb commit]; [cb waitUntilCompleted];
        double t = now_ms() - t0;
        if (cb.status != MTLCommandBufferStatusCompleted) { printf("command buffer: %s\n", cb.error.description.UTF8String); return 1; }
        if (i >= 2) { wall += t; gpu += (cb.GPUEndTime - cb.GPUStartTime) * 1000; if (t < wallMin) wallMin = t; }
    }
    int n = N - 2;
    // coalesced version: IOSurface planes wrapped as buffers (no copy)
    id<MTLComputePipelineState> ps16 = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"tile16"] error:&err];
    IOSurfaceLock(surf, kIOSurfaceLockReadOnly, NULL);
    size_t allocSz = (IOSurfaceGetAllocSize(surf) + 16383) & ~(size_t)16383;
    id<MTLBuffer> sbuf = [dev newBufferWithBytesNoCopy:IOSurfaceGetBaseAddress(surf) length:allocSz options:MTLResourceStorageModeShared deallocator:nil];
    size_t off0 = (uint8_t *)IOSurfaceGetBaseAddressOfPlane(surf, 0) - (uint8_t *)IOSurfaceGetBaseAddress(surf);
    size_t off1 = (uint8_t *)IOSurfaceGetBaseAddressOfPlane(surf, 1) - (uint8_t *)IOSurfaceGetBaseAddress(surf);
    struct { uint32_t pitch, width, rows, stride; } ql = { (uint32_t)pitch, (uint32_t)W, (uint32_t)H, (uint32_t)IOSurfaceGetBytesPerRowOfPlane(surf, 0) },
        qc = { (uint32_t)pitch, (uint32_t)W, (uint32_t)(H + 1) / 2, (uint32_t)IOSurfaceGetBytesPerRowOfPlane(surf, 1) };
    double wall16 = 0, gpu16 = 0;
    for (int i = 0; sbuf && ps16 && i < N; i++) {
        double t0 = now_ms();
        id<MTLCommandBuffer> cb = [q commandBuffer];
        id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:ps16];
        [ce setBuffer:sbuf offset:off0 atIndex:2]; [ce setBuffer:out offset:0 atIndex:0]; [ce setBytes:&ql length:sizeof ql atIndex:1];
        [ce dispatchThreads:MTLSizeMake(lumaSz / 16, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        [ce setBuffer:sbuf offset:off1 atIndex:2]; [ce setBuffer:out offset:lumaSz atIndex:0]; [ce setBytes:&qc length:sizeof qc atIndex:1];
        [ce dispatchThreads:MTLSizeMake(chromaSz / 16, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
        [ce endEncoding];
        [cb commit]; [cb waitUntilCompleted];
        if (i >= 2) { wall16 += now_ms() - t0; gpu16 += (cb.GPUEndTime - cb.GPUStartTime) * 1000; }
    }
    IOSurfaceUnlock(surf, kIOSurfaceLockReadOnly, NULL);
    printf("coalesced (IOSurface as buffer): %s, GPU %.3f ms, wall %.3f ms\n", sbuf ? "ok" : "no-copy buffer FAILED", gpu16 / n, wall16 / n);
    // reference: the CPU tiler from the same pixels
    uint8_t *ref = calloc(1, lumaSz + chromaSz);
    CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
    nvenc_in_tile_plane(ref, CVPixelBufferGetBaseAddressOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 0), W, H, pitch);   // warm-up
    double c0 = now_ms();
    nvenc_in_tile_plane(ref, CVPixelBufferGetBaseAddressOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 0), W, H, pitch);
    nvenc_in_tile_plane(ref + lumaSz, CVPixelBufferGetBaseAddressOfPlane(pb, 1), CVPixelBufferGetBytesPerRowOfPlane(pb, 1), W, (H + 1) / 2, pitch);
    double cpu = now_ms() - c0;
    CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
    // compare only the bytes the CPU tiler writes (inside the picture)
    size_t diff = 0; const uint8_t *g = out.contents;
    for (int r = 0; r < H; r++) for (int c = 0; c < W; c += 16) { size_t o = nvenc_in_offset(c, r, pitch); size_t len = W - c < 16 ? W - c : 16; diff += memcmp(g + o, ref + o, len) != 0; }
    for (int r = 0; r < (H + 1) / 2; r++) for (int c = 0; c < W; c += 16) { size_t o = lumaSz + nvenc_in_offset(c, r, pitch); size_t len = W - c < 16 ? W - c : 16; diff += memcmp(g + o, ref + o, len) != 0; }
    printf("%dx%d NV12: GPU kernel %.3f ms (GPU time), %.3f ms wall avg (min %.3f) incl. submit+wait; CPU tiler %.3f ms; mismatching runs: %zu\n",
           W, H, gpu / n, wall / n, wallMin, cpu, diff);
    return diff ? 1 : 0;
} }
