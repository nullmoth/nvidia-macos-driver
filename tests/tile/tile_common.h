// Shared by the tile shading oracles: arm reporting and small Metal helpers.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

static int g_unexpected = 0, g_arms = 0;
// An arm passes when ok matches want. A negative arm sets want = 0: it must NOT hold, and is reported as an expected FAIL.
static void arm(const char *name, int ok, int want, const char *fmt, ...) __attribute__((format(printf, 4, 5)));
static void arm(const char *name, int ok, int want, const char *fmt, ...) {
    char msg[512]; va_list ap; va_start(ap, fmt); vsnprintf(msg, sizeof msg, fmt, ap); va_end(ap);
    g_arms++;
    int good = (ok != 0) == (want != 0);
    if (!good) g_unexpected++;
    printf("%s  %-58s %s\n", good ? (want ? "PASS" : "FAIL(expected)") : (want ? "FAIL" : "PASS(unexpected)"), name, msg);
}

static id<MTLLibrary> load_lib(id<MTLDevice> dev, const char *path) {
    NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(path)] error:&e];
    if (!lib) { fprintf(stderr, "cannot load %s: %s\n", path, e.localizedDescription.UTF8String); exit(2); }
    return lib;
}
static id<MTLFunction> fn_of(id<MTLLibrary> lib, const char *name) {
    id<MTLFunction> f = [lib newFunctionWithName:@(name)];
    if (!f) { fprintf(stderr, "no function %s\n", name); exit(2); }
    return f;
}
static id<MTLTexture> make_rt(id<MTLDevice> dev, MTLPixelFormat fmt, int w, int h) {
    MTLTextureDescriptor *d = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:fmt width:w height:h mipmapped:NO];
    d.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite;
    d.storageMode = MTLStorageModeShared;
    return [dev newTextureWithDescriptor:d];
}
static id<MTLRenderPipelineState> make_draw_pso(id<MTLDevice> dev, id<MTLLibrary> lib, const char *vs, const char *fs, MTLPixelFormat fmt) {
    MTLRenderPipelineDescriptor *d = [MTLRenderPipelineDescriptor new];
    d.vertexFunction = fn_of(lib, vs); d.fragmentFunction = fn_of(lib, fs);
    d.colorAttachments[0].pixelFormat = fmt;
    NSError *e = nil; id<MTLRenderPipelineState> p = [dev newRenderPipelineStateWithDescriptor:d error:&e];
    if (!p) { fprintf(stderr, "draw pipeline %s/%s: %s\n", vs, fs, e.localizedDescription.UTF8String); exit(2); }
    return p;
}
static void read_rgba8(id<MTLTexture> t, uint8_t *out) {
    [t getBytes:out bytesPerRow:t.width * 4 fromRegion:MTLRegionMake2D(0, 0, t.width, t.height) mipmapLevel:0];
}
