// bench_surface_wrappers_nv12: like bench_surface_wrappers, but a two-plane NV12 IOSurface (luma R8 + chroma RG8), as Photos and video decoders use.
// A writable wrapper of each plane gets a pattern from a compute pass; a read-only wrapper of each plane then reads it back
// (same command buffer / two command buffers / two queues).
// build: clang -fobjc-arc -O1 -o bench_surface_wrappers_nv12 tools/bench_surface_wrappers_nv12.m -framework Metal -framework Foundation -framework IOSurface -framework CoreVideo
// usage: bench_surface_wrappers_nv12
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <CoreVideo/CoreVideo.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>

static const char *kSrc = "#include <metal_stdlib>\nusing namespace metal;\n"
    "kernel void wy(texture2d<float, access::write> t [[texture(0)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return; t.write(float4(float((p.x * 3 + p.y) & 255u) / 255.0, 0, 0, 1), p);\n}\n"
    "kernel void wc(texture2d<float, access::write> t [[texture(0)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return; t.write(float4(float(p.x & 255u) / 255.0, float(p.y & 255u) / 255.0, 0, 1), p);\n}\n"
    "kernel void ry(texture2d<float, access::read> t [[texture(0)]], device uchar *out [[buffer(0)]], constant uint &w [[buffer(1)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return; out[p.y * w + p.x] = uchar(t.read(p).r * 255.0 + 0.5);\n}\n"
    "kernel void rc(texture2d<float, access::read> t [[texture(0)]], device uchar2 *out [[buffer(0)]], constant uint &w [[buffer(1)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return; float4 c = t.read(p); out[p.y * w + p.x] = uchar2(c.r * 255.0 + 0.5, c.g * 255.0 + 0.5);\n}\n";

int main(void)
{
    @autoreleasepool {
        setvbuf(stdout, NULL, _IONBF, 0);
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); printf("device %s\n", dev.name.UTF8String);
        NSError *e = nil; id<MTLLibrary> lib = [dev newLibraryWithSource:@(kSrc) options:nil error:&e];
        id<MTLComputePipelineState> wy = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"wy"] error:&e], wc = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"wc"] error:&e],
            ry = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"ry"] error:&e], rc = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"rc"] error:&e];
        if (!wy || !wc || !ry || !rc) { printf("pipeline failed\n"); return 2; }
        const int sizes[][2] = { {1920, 1080}, {2016, 1512}, {512, 512}, {1000, 750} }; int bad = 0;
        static const char *modes[] = { "one command buffer", "two command buffers, same queue", "two command buffers, two queues" };
        for (unsigned si = 0; si < 4; si++) for (int mode = 0; mode < 3; mode++) @autoreleasepool {
            const int W = sizes[si][0], H = sizes[si][1];
            NSDictionary *a = @{ (id)kCVPixelBufferIOSurfacePropertiesKey: @{}, (id)kCVPixelBufferMetalCompatibilityKey: @YES };
            CVPixelBufferRef pb = NULL; CVPixelBufferCreate(NULL, W, H, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, (__bridge CFDictionaryRef)a, &pb);
            IOSurfaceRef surf = pb ? CVPixelBufferGetIOSurface(pb) : NULL; if (!surf) { printf("no surface\n"); bad++; continue; }
            const int cw = (W + 1) / 2, ch = (H + 1) / 2;
            MTLTextureDescriptor *wy_d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm width:W height:H mipmapped:NO]; wy_d.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead; wy_d.storageMode = MTLStorageModeShared;
            MTLTextureDescriptor *ry_d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR8Unorm width:W height:H mipmapped:NO]; ry_d.usage = MTLTextureUsageShaderRead; ry_d.storageMode = MTLStorageModeShared;
            MTLTextureDescriptor *wc_d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRG8Unorm width:cw height:ch mipmapped:NO]; wc_d.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead; wc_d.storageMode = MTLStorageModeShared;
            MTLTextureDescriptor *rc_d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRG8Unorm width:cw height:ch mipmapped:NO]; rc_d.usage = MTLTextureUsageShaderRead; rc_d.storageMode = MTLStorageModeShared;
            id<MTLTexture> twy = [dev newTextureWithDescriptor:wy_d iosurface:surf plane:0], try_ = [dev newTextureWithDescriptor:ry_d iosurface:surf plane:0];
            id<MTLTexture> twc = [dev newTextureWithDescriptor:wc_d iosurface:surf plane:1], trc = [dev newTextureWithDescriptor:rc_d iosurface:surf plane:1];
            if (!twy || !try_ || !twc || !trc) { printf("%dx%d wrappers failed\n", W, H); bad++; continue; }
            id<MTLBuffer> oy = [dev newBufferWithLength:(size_t)W * H options:MTLResourceStorageModeShared], oc = [dev newBufferWithLength:(size_t)cw * ch * 2 options:MTLResourceStorageModeShared];
            memset(oy.contents, 0, (size_t)W * H); memset(oc.contents, 0, (size_t)cw * ch * 2); uint32_t wyv = (uint32_t)W, wcv = (uint32_t)cw;
            id<MTLCommandQueue> q1 = [dev newCommandQueue], q2 = mode == 2 ? [dev newCommandQueue] : q1;
            id<MTLCommandBuffer> c1 = [q1 commandBuffer]; id<MTLComputeCommandEncoder> x = [c1 computeCommandEncoder];
            [x setComputePipelineState:wy]; [x setTexture:twy atIndex:0]; [x dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)];
            [x setComputePipelineState:wc]; [x setTexture:twc atIndex:0]; [x dispatchThreads:MTLSizeMake(cw, ch, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [x endEncoding];
            id<MTLCommandBuffer> c2 = mode == 0 ? c1 : [q2 commandBuffer]; id<MTLComputeCommandEncoder> y = [c2 computeCommandEncoder];
            [y setComputePipelineState:ry]; [y setTexture:try_ atIndex:0]; [y setBuffer:oy offset:0 atIndex:0]; [y setBytes:&wyv length:4 atIndex:1]; [y dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)];
            [y setComputePipelineState:rc]; [y setTexture:trc atIndex:0]; [y setBuffer:oc offset:0 atIndex:0]; [y setBytes:&wcv length:4 atIndex:1]; [y dispatchThreads:MTLSizeMake(cw, ch, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [y endEncoding];
            if (mode == 0) { [c1 commit]; [c1 waitUntilCompleted]; } else { [c1 commit]; [c2 commit]; [c1 waitUntilCompleted]; [c2 waitUntilCompleted]; }
            const uint8_t *py = oy.contents, *pc = oc.contents; long wy_bad = 0, wc_bad = 0;
            for (int j = 0; j < H; j += 2) for (int i = 0; i < W; i += 2) if (py[(size_t)j * W + i] != ((i * 3 + j) & 255)) wy_bad++;
            for (int j = 0; j < ch; j += 2) for (int i = 0; i < cw; i += 2) { const uint8_t *p = pc + ((size_t)j * cw + i) * 2; if (p[0] != (i & 255) || p[1] != (j & 255)) wc_bad++; }
            printf("%4dx%-4d  %-34s luma wrong %7ld  chroma wrong %7ld%s\n", W, H, modes[mode], wy_bad, wc_bad, (wy_bad || wc_bad) ? "  <--" : "");
            if (wy_bad || wc_bad) bad++;
            CVPixelBufferRelease(pb);
        }
        printf("%s (%d of 12 cases wrong)\n", bad ? "FAIL" : "OK", bad); return bad ? 1 : 0;
    }
}
