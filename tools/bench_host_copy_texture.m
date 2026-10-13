// bench_host_copy_texture: the picture's pixels live in memory the app owns. The app wraps that memory in a no-copy MTLBuffer, makes a
// texture over it, THEN a CPU thread writes the pixels (decoders fill big images in strips), and the GPU samples it.
// If the buffer was imported zero-copy, the GPU sees the pixels written later. If the driver had to COPY it (import
// refused), the GPU keeps the copy it made at creation: the strips written afterwards are missing.
// build: clang -fobjc-arc -O1 -include unistd.h -o bench_host_copy_texture tools/bench_host_copy_texture.m -framework Metal -framework Foundation
// usage: bench_host_copy_texture [width height]   modes: untouched memory / pre-faulted memory / written before wrapping
#import <Metal/Metal.h>
#import <sys/mman.h>
#import <stdio.h>
#import <stdlib.h>
#import <string.h>

static const char *kSrc =
    "#include <metal_stdlib>\nusing namespace metal;\n"
    "kernel void rd(texture2d<float, access::read> t [[texture(0)]], device uint *out [[buffer(0)]], constant uint &w [[buffer(1)]], uint2 p [[thread_position_in_grid]]) {\n"
    "    if (p.x >= t.get_width() || p.y >= t.get_height()) return;\n"
    "    float4 c = t.read(p);\n"
    "    out[p.y * w + p.x] = uint(c.b * 255.0 + 0.5) | (uint(c.g * 255.0 + 0.5) << 8) | (uint(c.r * 255.0 + 0.5) << 16) | (uint(c.a * 255.0 + 0.5) << 24);\n"
    "}\n";

static uint32_t expect(int x, int y) { return (uint32_t)(x & 255) | ((uint32_t)(y & 255) << 8) | ((uint32_t)((x + y) & 255) << 16) | 0xff000000u; }
static void fill(uint8_t *mem, int w, int x0, int x1, int h)
{
    for (int y = 0; y < h; y++) for (int x = x0; x < x1; x++) *(uint32_t *)(mem + ((size_t)y * w + x) * 4) = expect(x, y);
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        const int W = argc > 2 ? atoi(argv[1]) : 2560, H = argc > 2 ? atoi(argv[2]) : 1440;
        const size_t N = (size_t)W * H * 4;
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); printf("device %s, image %dx%d = %zu bytes\n", dev.name.UTF8String, W, H, N);
        NSError *e = nil; id<MTLLibrary> lib = [dev newLibraryWithSource:@(kSrc) options:nil error:&e];
        id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"rd"] error:&e];
        if (!ps) { printf("pipeline failed\n"); return 2; }
        id<MTLCommandQueue> q = [dev newCommandQueue];
        static const char *names[] = { "memory untouched, pixels written after wrapping", "memory pre-faulted (memset 0), pixels written after wrapping",
                                       "pixels written BEFORE wrapping (control)", "4 strips written after wrapping, in order, with a gap",
                                       "MISALIGNED (malloc) memory -> copy fallback, pixels after wrapping", "MISALIGNED (malloc) memory, 4 strips after wrapping", "MISALIGNED (malloc) memory, pixels BEFORE wrapping (control)" };
        int bad = 0;
        for (int mode = 0; mode < 7; mode++) for (int rep = 0; rep < 3; rep++) {
            @autoreleasepool {
                const int mis = mode >= 4; uint8_t *base = mis ? malloc(N + 4096) : NULL; uint8_t *mem = mis ? base + 16 : mmap(NULL, N, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
                if (mis) memset(mem, 0, N);
                if (mode == 1) memset(mem, 0, N);
                if (mode == 2 || mode == 6) fill(mem, W, 0, W, H);
                id<MTLBuffer> buf = [dev newBufferWithBytesNoCopy:mem length:N options:MTLResourceStorageModeShared deallocator:nil];
                if (!buf) { printf("mode %d: no buffer\n", mode); continue; }
                MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
                d.storageMode = MTLStorageModeShared; d.usage = MTLTextureUsageShaderRead;
                id<MTLTexture> tex = [buf newTextureWithDescriptor:d offset:0 bytesPerRow:(size_t)W * 4];
                if (!tex) { printf("mode %d: no texture over the buffer\n", mode); continue; }
                if (mode == 0 || mode == 1 || mode == 4) fill(mem, W, 0, W, H);
                if (mode == 3 || mode == 5) { for (int s = 0; s < 4; s++) { fill(mem, W, s * (W / 4), (s + 1) * (W / 4), H); usleep(2000); } }
                id<MTLBuffer> out = [dev newBufferWithLength:(size_t)W * H * 4 options:MTLResourceStorageModeShared]; memset(out.contents, 0, (size_t)W * H * 4);
                uint32_t wv = (uint32_t)W;
                id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> c = [cb computeCommandEncoder];
                [c setComputePipelineState:ps]; [c setTexture:tex atIndex:0]; [c setBuffer:out offset:0 atIndex:0]; [c setBytes:&wv length:4 atIndex:1];
                [c dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [c endEncoding]; [cb commit]; [cb waitUntilCompleted];
                const uint32_t *o = out.contents; long wrong = 0; int firstX = -1, lastX = -1;
                for (int y = 0; y < H; y += 2) for (int x = 0; x < W; x += 2) if (o[(size_t)y * W + x] != expect(x, y)) { wrong++; if (firstX < 0 || x < firstX) firstX = x; if (x > lastX) lastX = x; }
                printf("mode %d rep %d  %-62s wrong samples %7ld%s\n", mode, rep + 1, names[mode], wrong, wrong ? "  <-- GPU did not see the pixels" : "");
                if (wrong) bad++;
                if (mis) free(base); else munmap(mem, N);
            }
        }
        printf("%s (%d of 21 cases wrong)\n", bad ? "FAIL" : "OK", bad);
        return bad ? 1 : 0;
    }
}
