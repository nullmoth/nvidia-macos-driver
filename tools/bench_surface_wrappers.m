// bench_surface_wrappers: two Metal textures over ONE IOSurface - a writable wrapper and a read-only wrapper, the way CoreImage uses a
// tile (a compute pass writes it, a later pass in the same command buffer reads it). The second pass must see the first
// one's pixels. Widths whose row is not a multiple of 32 bytes (572 x 4 = 2288) cannot share the family VRAM and take the
// plugin's private-image path; 512 x 4 = 2048 can (control).
// build: clang -fobjc-arc -O1 -o bench_surface_wrappers tools/bench_surface_wrappers.m -framework Metal -framework Foundation -framework IOSurface
// usage: bench_surface_wrappers      cases: write+read in one command buffer / two command buffers / two queues
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>

static const char *kSrc = "#include <metal_stdlib>\nusing namespace metal;\n"
    "kernel void wr(texture2d<float, access::write> t [[texture(0)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return;\n"
    "    t.write(float4(float(p.x & 255u) / 255.0, float(p.y & 255u) / 255.0, float((p.x + p.y) & 255u) / 255.0, 1.0), p);\n}\n"
    "kernel void rd(texture2d<float, access::read> t [[texture(0)]], device uchar4 *out [[buffer(0)]], constant uint &w [[buffer(1)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return;\n"
    "    float4 c = t.read(p); out[p.y * w + p.x] = uchar4(c * 255.0 + 0.5);\n}\n";

int main(void)
{
    @autoreleasepool {
        setvbuf(stdout, NULL, _IONBF, 0);
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); printf("device %s\n", dev.name.UTF8String);
        NSError *e = nil; id<MTLLibrary> lib = [dev newLibraryWithSource:@(kSrc) options:nil error:&e];
        id<MTLComputePipelineState> pw = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"wr"] error:&e];
        id<MTLComputePipelineState> pr = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"rd"] error:&e];
        if (!pw || !pr) { printf("pipeline failed\n"); return 2; }
        const int widths[] = { 512, 572, 580, 576 }, H = 572; int bad = 0;
        static const char *modes[] = { "one command buffer, two passes", "two command buffers, same queue", "two command buffers, two queues" };
        for (unsigned wi = 0; wi < 4; wi++) for (int mode = 0; mode < 3; mode++) {
            @autoreleasepool {
                const int W = widths[wi]; const size_t row = (size_t)W * 4;
                IOSurfaceRef surf = IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfaceBytesPerElement: @4,
                    (id)kIOSurfaceBytesPerRow: @(row), (id)kIOSurfacePixelFormat: @(0x42475241) /* BGRA */ });
                MTLTextureDescriptor *dw = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:W height:H mipmapped:NO];
                dw.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead; dw.storageMode = MTLStorageModeShared;
                MTLTextureDescriptor *dr = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:W height:H mipmapped:NO];
                dr.usage = MTLTextureUsageShaderRead; dr.storageMode = MTLStorageModeShared;
                id<MTLTexture> tw = [dev newTextureWithDescriptor:dw iosurface:surf plane:0];
                id<MTLTexture> tr = [dev newTextureWithDescriptor:dr iosurface:surf plane:0];
                if (!tw || !tr) { printf("%4d x %d  %-34s wrappers failed\n", W, H, modes[mode]); bad++; continue; }
                id<MTLBuffer> out = [dev newBufferWithLength:(size_t)W * H * 4 options:MTLResourceStorageModeShared]; memset(out.contents, 0, (size_t)W * H * 4);
                uint32_t wv = (uint32_t)W;
                id<MTLCommandQueue> q1 = [dev newCommandQueue], q2 = mode == 2 ? [dev newCommandQueue] : q1;
                id<MTLCommandBuffer> c1 = [q1 commandBuffer];
                id<MTLComputeCommandEncoder> a = [c1 computeCommandEncoder];
                [a setComputePipelineState:pw]; [a setTexture:tw atIndex:0]; [a dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [a endEncoding];
                id<MTLCommandBuffer> c2 = mode == 0 ? c1 : [q2 commandBuffer];
                id<MTLComputeCommandEncoder> b = [c2 computeCommandEncoder];
                [b setComputePipelineState:pr]; [b setTexture:tr atIndex:0]; [b setBuffer:out offset:0 atIndex:0]; [b setBytes:&wv length:4 atIndex:1];
                [b dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [b endEncoding];
                if (mode == 0) { [c1 commit]; [c1 waitUntilCompleted]; }
                else { [c1 commit]; [c2 commit]; [c1 waitUntilCompleted]; [c2 waitUntilCompleted]; }
                const uint8_t *o = out.contents; long wrong = 0;
                for (int y = 0; y < H; y += 2) for (int x = 0; x < W; x += 2) { const uint8_t *p = o + ((size_t)y * W + x) * 4;
                    if (p[0] != (x & 255) || p[1] != (y & 255) || p[2] != ((x + y) & 255)) wrong++; }
                printf("%4d x %d (row %zu, row%%32=%zu)  %-34s wrong samples %6ld%s\n", W, H, row, row % 32, modes[mode], wrong, wrong ? "  <-- reader did not see the writer's pixels" : "");
                if (wrong) bad++;
                CFRelease(surf);
            }
        }
        printf("%s (%d of 12 cases wrong)\n", bad ? "FAIL" : "OK", bad); return bad ? 1 : 0;
    }
}
