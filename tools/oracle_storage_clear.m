// oracle_storage_clear.m - does a storage (write_texture_2d_array) clear land for every colour format?
//
// Eight private 2048x2048 textures, usage read|write|renderTarget, each on its own MTLHeap sized to the
// texture plus 64 KB, each with a 2D-array view of its single layer; a device-side argument chain in the
// shape of the game's clear (see oracle_storage_clear.metal) names the view; one command buffer per
// format fills the texture with 0xFF bytes, dispatches 128x128 threadgroups of 8x8, waits, reads the
// whole texture back and counts nonzero bytes. The control is RGBA8Unorm, run first; RG11B10Float second.
//
// Usage: oracle_storage_clear [path/to/libNVMTL.dylib] [--metallib file]
//   With a dylib path the device is the plugin's NVMTLDevice over the NVAccel service (the direct route);
//   without one, the system default device (Apple ground truth, or a shimmed plugin).
//   ORACLE_ONLY=<name>   run one format.   ORACLE_USE_RESOURCE=1   useResource: the view instead of useHeap:.
//   ORACLE_FORMAT_HEX=1  read back through the view's parent as raw bytes (always; printed per format).
// Exit 0 = every format cleared, 1 = some format kept nonzero bytes or its command buffer failed.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <dlfcn.h>
#include <mach/mach_time.h>
#include <string.h>

typedef struct { const char *name; MTLPixelFormat fmt; unsigned bpp; } Fmt;
static const Fmt FORMATS[] = {
    { "RGBA8Unorm",   MTLPixelFormatRGBA8Unorm,    4 },   // negative control: must pass
    { "RG11B10Float", MTLPixelFormatRG11B10Float,  4 },   // the dying target's format
    { "RGBA16Float",  MTLPixelFormatRGBA16Float,   8 },
    { "RGB10A2Unorm", MTLPixelFormatRGB10A2Unorm,  4 },
    { "BGR10A2Unorm", MTLPixelFormatBGR10A2Unorm,  4 },
    { "R32Float",     MTLPixelFormatR32Float,      4 },
    { "RG16Float",    MTLPixelFormatRG16Float,     4 },
    { "RGBA32Float",  MTLPixelFormatRGBA32Float,  16 },
};
enum { DIM = 2048, TG = 8, TILE = 16 };

static id<MTLDevice> deviceFrom(const char *dylib) {
    if (!dylib) return MTLCreateSystemDefaultDevice();
    if (!dlopen(dylib, RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr, "LOAD: %s\n", dlerror()); return nil; }
    io_service_t svc = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    if (!svc) { fprintf(stderr, "no NVAccel service\n"); return nil; }
    id d = ((id (*)(id, SEL, mach_port_t))objc_msgSend)([NSClassFromString(@"NVMTLDevice") alloc],
                                                        NSSelectorFromString(@"initWithAcceleratorPort:"), svc);
    IOObjectRelease(svc);
    return d;
}

static double secs(uint64_t a, uint64_t b) {
    static mach_timebase_info_data_t tb; if (!tb.denom) mach_timebase_info(&tb);
    return (double)(b - a) * tb.numer / tb.denom / 1e9;
}

int main(int argc, char **argv) { @autoreleasepool {
    const char *dylib = NULL; NSString *metallib = nil;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--metallib") && i + 1 < argc) metallib = [NSString stringWithUTF8String:argv[++i]];
        else dylib = argv[i];
    }
    if (!metallib) {
        NSString *exe = [[NSString stringWithUTF8String:argv[0]] stringByResolvingSymlinksInPath];
        metallib = [[exe stringByDeletingLastPathComponent] stringByAppendingPathComponent:@"oracle_storage_clear.metallib"];
    }
    id<MTLDevice> dev = deviceFrom(dylib);
    if (!dev) { printf("NO DEVICE\n"); return 3; }
    printf("DEVICE %s\n", dev.name.UTF8String);
    NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:metallib] error:&err];
    if (!lib) { printf("NO LIBRARY %s: %s\n", metallib.UTF8String, err.localizedDescription.UTF8String); return 4; }
    id<MTLFunction> fn = [lib newFunctionWithName:@"clear_main"];
    id<MTLComputePipelineState> pso = fn ? [dev newComputePipelineStateWithFunction:fn error:&err] : nil;
    if (!pso) { printf("NO PIPELINE: %s\n", err.localizedDescription.UTF8String); return 5; }
    id<MTLCommandQueue> q = [dev newCommandQueue];
    const char *only = getenv("ORACLE_ONLY");
    const bool useResource = getenv("ORACLE_USE_RESOURCE") && *getenv("ORACLE_USE_RESOURCE") == '1';

    // The carried 3D texture: 192x192x96, never read by the kernel, on its own heap like everything else.
    MTLTextureDescriptor *vd = [[MTLTextureDescriptor alloc] init];
    vd.textureType = MTLTextureType3D; vd.pixelFormat = MTLPixelFormatRGBA8Unorm;
    vd.width = 192; vd.height = 192; vd.depth = 96; vd.usage = MTLTextureUsageShaderRead; vd.storageMode = MTLStorageModePrivate;
    MTLHeapDescriptor *vhd = [[MTLHeapDescriptor alloc] init];
    vhd.size = [dev heapTextureSizeAndAlignWithDescriptor:vd].size + 65536; vhd.storageMode = MTLStorageModePrivate;
    id<MTLHeap> vheap = [dev newHeapWithDescriptor:vhd];
    id<MTLTexture> vol = [vheap newTextureWithDescriptor:vd];
    if (!vol) { printf("NO 3D TEXTURE\n"); return 6; }

    // Argument chain in one shared buffer: AB at 0, Foo at 256, the zero Foo at 512.
    id<MTLBuffer> args = [dev newBufferWithLength:4096 options:MTLResourceStorageModeShared];
    uint8_t *ap = args.contents; memset(ap, 0, 4096);
    const uint64_t base = args.gpuAddress;
    *(uint64_t *)(ap + 40) = base + 256;          // AB.foo
    *(uint64_t *)(ap + 256 + 0) = base + 512;     // Foo.next -> zero Foo
    *(uint64_t *)(ap + 256 + 32) = vol.gpuResourceID._impl;

    int failures = 0, ran = 0;
    for (unsigned i = 0; i < sizeof FORMATS / sizeof FORMATS[0]; i++) { @autoreleasepool {
        const Fmt f = FORMATS[i];
        if (only && strcmp(only, f.name)) continue;
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:f.fmt width:DIM height:DIM mipmapped:NO];
        td.usage = MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite | MTLTextureUsageRenderTarget;
        td.storageMode = MTLStorageModePrivate;
        MTLSizeAndAlign sa = [dev heapTextureSizeAndAlignWithDescriptor:td];
        MTLHeapDescriptor *hd = [[MTLHeapDescriptor alloc] init];
        hd.size = sa.size + 65536; hd.storageMode = MTLStorageModePrivate;
        id<MTLHeap> heap = [dev newHeapWithDescriptor:hd];
        id<MTLTexture> tex = [heap newTextureWithDescriptor:td];
        if (!tex) { printf("fmt %s: NO TEXTURE (heap %llu B)\n", f.name, (unsigned long long)hd.size); failures++; continue; }
        id<MTLTexture> view = [tex newTextureViewWithPixelFormat:f.fmt textureType:MTLTextureType2DArray
                                                           levels:NSMakeRange(0, 1) slices:NSMakeRange(0, 1)];
        if (!view) { printf("fmt %s: NO VIEW\n", f.name); failures++; continue; }
        *(uint64_t *)(ap + 256 + 8) = view.gpuResourceID._impl;   // Foo.tex

        const NSUInteger rowB = (NSUInteger)DIM * f.bpp, bytes = rowB * DIM;
        id<MTLBuffer> fill = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared];
        id<MTLBuffer> back = [dev newBufferWithLength:bytes options:MTLResourceStorageModeShared];
        if (!fill || !back) { printf("fmt %s: NO STAGING (%lu B)\n", f.name, (unsigned long)bytes); failures++; continue; }
        memset(fill.contents, 0xFF, bytes); memset(back.contents, 0xAA, bytes);

        // Heap resources are hazard-untracked (the game's are too), so the fill, the clear and the readback
        // are three command buffers each waited on: on the M1 one command buffer let the fill and the clear
        // race and 6 of 8 formats read back part 0xFF.
        id<MTLCommandBuffer> cbFill = [q commandBuffer];
        id<MTLBlitCommandEncoder> b0 = [cbFill blitCommandEncoder];
        [b0 copyFromBuffer:fill sourceOffset:0 sourceBytesPerRow:rowB sourceBytesPerImage:bytes sourceSize:MTLSizeMake(DIM, DIM, 1)
                 toTexture:tex destinationSlice:0 destinationLevel:0 destinationOrigin:MTLOriginMake(0, 0, 0)];
        [b0 endEncoding];
        [cbFill commit]; [cbFill waitUntilCompleted];
        if (cbFill.status != MTLCommandBufferStatusCompleted) {
            printf("fmt %s: FAILED fill command buffer status %ld error %s\n", f.name, (long)cbFill.status,
                   cbFill.error ? cbFill.error.localizedDescription.UTF8String : "(none)");
            fflush(stdout); failures++; continue;
        }
        uint64_t t0 = mach_absolute_time();
        id<MTLCommandBuffer> cb = [q commandBuffer];
        id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:pso];
        [ce setBuffer:args offset:0 atIndex:0];
        if (useResource) [ce useResource:view usage:MTLResourceUsageWrite];
        else [ce useHeap:heap];
        [ce useHeap:vheap];
        [ce dispatchThreadgroups:MTLSizeMake(DIM / TILE, DIM / TILE, 1) threadsPerThreadgroup:MTLSizeMake(TG, TG, 1)];
        [ce endEncoding];
        [cb commit];
        [cb waitUntilCompleted];
        uint64_t t1 = mach_absolute_time();
        ran++;
        if (cb.status != MTLCommandBufferStatusCompleted || cb.error) {
            printf("fmt %s: FAILED clear command buffer status %ld error %s\n", f.name, (long)cb.status,
                   cb.error ? cb.error.localizedDescription.UTF8String : "(none)");
            fflush(stdout); failures++; continue;
        }
        id<MTLCommandBuffer> cbBack = [q commandBuffer];
        id<MTLBlitCommandEncoder> b1 = [cbBack blitCommandEncoder];
        [b1 copyFromTexture:tex sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0, 0, 0) sourceSize:MTLSizeMake(DIM, DIM, 1)
                   toBuffer:back destinationOffset:0 destinationBytesPerRow:rowB destinationBytesPerImage:bytes];
        [b1 endEncoding];
        [cbBack commit]; [cbBack waitUntilCompleted];
        if (cbBack.status != MTLCommandBufferStatusCompleted) {
            printf("fmt %s: FAILED readback command buffer status %ld error %s\n", f.name, (long)cbBack.status,
                   cbBack.error ? cbBack.error.localizedDescription.UTF8String : "(none)");
            fflush(stdout); failures++; continue;
        }
        const uint8_t *rb = back.contents; size_t nonzero = 0, first = (size_t)-1;
        for (size_t k = 0; k < bytes; k++) if (rb[k]) { nonzero++; if (first == (size_t)-1) first = k; }
        if (nonzero == 0)
            printf("fmt %s: OK (0 nonzero of %lu bytes, heap %llu B, %.1f ms)\n", f.name, (unsigned long)bytes,
                   (unsigned long long)hd.size, secs(t0, t1) * 1e3);
        else {
            printf("fmt %s: FAIL (%zu nonzero of %lu bytes; first at byte %zu = texel (%zu,%zu) value 0x%02x; heap %llu B, %.1f ms)\n",
                   f.name, nonzero, (unsigned long)bytes, first, (first % rowB) / f.bpp, first / rowB, rb[first],
                   (unsigned long long)hd.size, secs(t0, t1) * 1e3);
            failures++;
        }
        fflush(stdout);
    } }
    printf("%s: %d formats ran, %d failed\n", failures ? "FAIL" : "PASS", ran, failures);
    return failures ? 1 : 0;
} }
