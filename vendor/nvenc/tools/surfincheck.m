// surfincheck.m: does the plugin's surface-in deliver the CPU-written pixels? Writes a pattern into a BGRA IOSurface
// with the CPU, copies the IOSurface-backed texture into a shared buffer on the GPU (which makes the plugin upload the
// surface's pages first), and compares. Repeats with a new pattern each round, so stale uploads show up.
//   ./surfincheck [W H [ROUNDS]]     default 3158x1740, 6 rounds; set /tmp/nvmtl-surfin-mode first to pick the path
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>

int main(int argc, char **argv) { @autoreleasepool {
    int W = argc > 2 ? atoi(argv[1]) : 3158, H = argc > 2 ? atoi(argv[2]) : 1740, N = argc > 3 ? atoi(argv[3]) : 6;
    size_t bpr = ((size_t)W * 4 + 63) & ~(size_t)63;
    IOSurfaceRef s = IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H),
        (id)kIOSurfaceBytesPerElement: @4, (id)kIOSurfaceBytesPerRow: @(bpr), (id)kIOSurfacePixelFormat: @((uint32_t)'BGRA') });
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); id<MTLCommandQueue> q = [dev newCommandQueue];
    MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
    d.usage = MTLTextureUsageShaderRead;
    id<MTLTexture> t = [dev newTextureWithDescriptor:d iosurface:s plane:0];
    id<MTLBuffer> out = [dev newBufferWithLength:(NSUInteger)W * H * 4 options:MTLResourceStorageModeShared];
    int bad_rounds = 0;
    for (int k = 0; k < N; k++) {
        IOSurfaceLock(s, 0, NULL);
        uint8_t *p = IOSurfaceGetBaseAddress(s);
        for (int y = 0; y < H; y++) { uint32_t *row = (uint32_t *)(p + (size_t)y * bpr);
            for (int x = 0; x < W; x++) row[x] = (uint32_t)(x * 2654435761u ^ y * 40503u ^ (uint32_t)k * 0x9e3779b9u) | 0xff000000u; }
        IOSurfaceUnlock(s, 0, NULL);
        id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLBlitCommandEncoder> be = [cb blitCommandEncoder];
        [be copyFromTexture:t sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(W, H, 1)
                   toBuffer:out destinationOffset:0 destinationBytesPerRow:(NSUInteger)W * 4 destinationBytesPerImage:(NSUInteger)W * H * 4];
        [be endEncoding]; [cb commit]; [cb waitUntilCompleted];
        const uint32_t *o = out.contents; size_t mism = 0, first = SIZE_MAX;
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
            uint32_t want = (uint32_t)(x * 2654435761u ^ y * 40503u ^ (uint32_t)k * 0x9e3779b9u) | 0xff000000u;
            if (o[(size_t)y * W + x] != want) { if (first == SIZE_MAX) first = (size_t)y * W + x; mism++; } }
        printf("round %d: %zu of %zu pixels differ%s", k, mism, (size_t)W * H, mism ? "" : "\n");
        if (mism) { printf(" (first at x=%zu y=%zu)\n", first % W, first / W); bad_rounds++; }
    }
    printf("RESULT %s: %d of %d rounds wrong\n", bad_rounds ? "FAIL" : "PASS", bad_rounds, N);
    return bad_rounds ? 1 : 0;
} }
