// macOS 26 Metal 3 gaps oracle: four macOS 26 additions a Metal 3 app can use without asking for Metal 4.
//
//   1. requiredThreadsPerThreadgroup and the render pipeline's requiredThreadsPer{Tile,Object,Mesh}Threadgroup getters
//   2. the depth bounds test, setDepthTestMinBound:maxBound: on a render encoder
//   3. -[MTLTexture newTextureViewWithDescriptor:]
//   4. setColorAttachmentMap: on a Metal 3 render encoder
//
// Runs on Apple hardware to record the reference, and on the NVIDIA driver to compare. A selector the device does not
// answer makes its arms FAIL with "absent" instead of crashing, so one run lists every gap. CONTROL lines check that an
// arm separates the right answer from the wrong one this driver used to give (they fire as built when the arm is sound).
//
// Exit code: 0 when every arm passed, 1 otherwise. Last line: "RESULT <arms> arms, <failed> failed".
// usage: gaps26 <gaps26.metallib>
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <stdio.h>
#include <math.h>

static int gFail = 0, gArms = 0;
static void arm(const char *name, BOOL ok, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void arm(const char *name, BOOL ok, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    gArms++; if (!ok) gFail++;
    printf("%s  %-64s %s\n", ok ? "PASS" : "FAIL", name, buf);
}
static void control(const char *name, BOOL fired, const char *fmt, ...) __attribute__((format(printf, 3, 4)));
static void control(const char *name, BOOL fired, const char *fmt, ...) {
    char buf[256]; va_list ap; va_start(ap, fmt); vsnprintf(buf, sizeof buf, fmt, ap); va_end(ap);
    printf("CONTROL %-61s %s: %s\n", name, fired ? "fired as built" : "DID NOT FIRE", buf);
}
#define KV(k, fmt, ...) printf("%-48s = " fmt "\n", k, ##__VA_ARGS__)
#define HAS(obj, sel) [(id)(obj) respondsToSelector:@selector(sel)]

static id<MTLDevice> dev;
static id<MTLLibrary> lib;
static id<MTLCommandQueue> queue;

static BOOL sizeIs(MTLSize s, NSUInteger w, NSUInteger h, NSUInteger d) { return s.width == w && s.height == h && s.depth == d; }

// --- 1 · required threadgroup sizes ----------------------------------------------------------------------------
static void requiredThreads(void) {
    NSError *e = nil;
    id<MTLFunction> fn = [lib newFunctionWithName:@"k_fill"];
    MTLComputePipelineDescriptor *pd = [MTLComputePipelineDescriptor new];
    pd.computeFunction = fn; pd.requiredThreadsPerThreadgroup = MTLSizeMake(64, 1, 1);
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithDescriptor:pd options:0 reflection:nil error:&e];
    if (!ps) { arm("1 compute pipeline with requiredThreadsPerThreadgroup builds", NO, "%s", e.localizedDescription.UTF8String); return; }
    id<MTLComputePipelineState> p3 = [dev newComputePipelineStateWithFunction:fn error:&e];
    BOOL has = HAS(ps, requiredThreadsPerThreadgroup);
    arm("1 compute pipeline answers requiredThreadsPerThreadgroup", has, "%s", has ? "yes" : "absent");
    if (has) {
        MTLSize r = ps.requiredThreadsPerThreadgroup, u = p3 ? p3.requiredThreadsPerThreadgroup : MTLSizeMake(9, 9, 9);
        arm("1 required 64x1x1 reads back 64x1x1", sizeIs(r, 64, 1, 1), "%lux%lux%lu", (unsigned long)r.width, (unsigned long)r.height, (unsigned long)r.depth);
        arm("1 pipeline from a function alone reads back 0x0x0", sizeIs(u, 0, 0, 0), "%lux%lux%lu", (unsigned long)u.width, (unsigned long)u.height, (unsigned long)u.depth);
    }
    arm("1 maxTotalThreadsPerThreadgroup is 64 when 64x1x1 is required", ps.maxTotalThreadsPerThreadgroup == 64, "%lu", (unsigned long)ps.maxTotalThreadsPerThreadgroup);
    control("1 the claim 'a required size leaves the total at 1024' is false", ps.maxTotalThreadsPerThreadgroup != 1024, "%lu", (unsigned long)ps.maxTotalThreadsPerThreadgroup);
    KV("threadExecutionWidth", "%lu", (unsigned long)ps.threadExecutionWidth);

    id<MTLBuffer> out = [dev newBufferWithLength:4096 * 4 options:MTLResourceStorageModeShared];
    memset(out.contents, 0, 4096 * 4);
    id<MTLCommandBuffer> cb = [queue commandBuffer];
    id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
    [ce setComputePipelineState:ps]; [ce setBuffer:out offset:0 atIndex:0];
    [ce dispatchThreads:MTLSizeMake(4096, 1, 1) threadsPerThreadgroup:MTLSizeMake(64, 1, 1)];
    [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];
    const uint32_t *u = out.contents; BOOL ok = cb.status == MTLCommandBufferStatusCompleted;
    for (uint32_t i = 0; ok && i < 4096; i++) ok = u[i] == i + 1;
    arm("1 dispatch with the required size writes 4096 values", ok, "u[0] %u u[4095] %u", u[0], u[4095]);

    MTLRenderPipelineDescriptor *rd = [MTLRenderPipelineDescriptor new];
    rd.vertexFunction = [lib newFunctionWithName:@"fs_v"]; rd.fragmentFunction = [lib newFunctionWithName:@"fs_red"];
    rd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm;
    id<MTLRenderPipelineState> rp = [dev newRenderPipelineStateWithDescriptor:rd error:&e];
    if (!rp) { arm("1 render pipeline builds", NO, "%s", e.localizedDescription.UTF8String); return; }
    BOOL hr = HAS(rp, requiredThreadsPerTileThreadgroup) && HAS(rp, requiredThreadsPerObjectThreadgroup) && HAS(rp, requiredThreadsPerMeshThreadgroup);
    arm("1 render pipeline answers requiredThreadsPer{Tile,Object,Mesh}Threadgroup", hr, "%s", hr ? "yes" : "absent");
    if (hr) {
        MTLSize t = rp.requiredThreadsPerTileThreadgroup, o = rp.requiredThreadsPerObjectThreadgroup, m = rp.requiredThreadsPerMeshThreadgroup;
        KV("render pipeline required tile / object / mesh", "%lux%lux%lu / %lux%lux%lu / %lux%lux%lu",
           (unsigned long)t.width, (unsigned long)t.height, (unsigned long)t.depth, (unsigned long)o.width, (unsigned long)o.height,
           (unsigned long)o.depth, (unsigned long)m.width, (unsigned long)m.height, (unsigned long)m.depth);
        arm("1 a vertex/fragment pipeline requires 0x0x0 for tile, object and mesh", sizeIs(t, 0, 0, 0) && sizeIs(o, 0, 0, 0) && sizeIs(m, 0, 0, 0), "see line above");
    }
}

// --- 2 · depth bounds ------------------------------------------------------------------------------------------
// Two cases separate the readings: stored 0.25 / fragment 0.9 and stored 0.75 / fragment 0.1, with bounds [0.5, 1].
// Apple discards the first and draws the second: the test reads the STORED attachment depth.
static uint32_t depthBoundsCase(float stored, float frag, BOOL bounds, id<MTLRenderPipelineState> ps, id<MTLDepthStencilState> ds) {
    MTLTextureDescriptor *cdsc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:16 height:16 mipmapped:NO];
    cdsc.usage = MTLTextureUsageRenderTarget; cdsc.storageMode = MTLStorageModeShared;
    MTLTextureDescriptor *ddsc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatDepth32Float width:16 height:16 mipmapped:NO];
    ddsc.usage = MTLTextureUsageRenderTarget; ddsc.storageMode = MTLStorageModePrivate;
    id<MTLTexture> col = [dev newTextureWithDescriptor:cdsc], dep = [dev newTextureWithDescriptor:ddsc];
    id<MTLBuffer> zb = [dev newBufferWithLength:16 options:MTLResourceStorageModeShared];
    *(float *)zb.contents = frag;
    MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor new];
    rp.colorAttachments[0].texture = col; rp.colorAttachments[0].loadAction = MTLLoadActionClear;
    rp.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1); rp.colorAttachments[0].storeAction = MTLStoreActionStore;
    rp.depthAttachment.texture = dep; rp.depthAttachment.loadAction = MTLLoadActionClear; rp.depthAttachment.clearDepth = stored;
    rp.depthAttachment.storeAction = MTLStoreActionDontCare;
    id<MTLCommandBuffer> cb = [queue commandBuffer];
    id<MTLRenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
    [re setRenderPipelineState:ps]; [re setDepthStencilState:ds];
    if (bounds) {
        if (HAS(re, setDepthTestMinBound:maxBound:)) [re setDepthTestMinBound:0.5f maxBound:1.0f];
        else { [re endEncoding]; return 0xDEADu; }
    }
    [re setVertexBuffer:zb offset:0 atIndex:0];
    [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
    [re endEncoding];
    [cb commit]; [cb waitUntilCompleted];
    if (cb.error) return 0xBADu;
    uint32_t px = 0;
    [col getBytes:&px bytesPerRow:64 fromRegion:MTLRegionMake2D(8, 8, 1, 1) mipmapLevel:0];
    return px;
}
static void depthBounds(void) {
    NSError *e = nil;
    MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
    pd.vertexFunction = [lib newFunctionWithName:@"fs_v"]; pd.fragmentFunction = [lib newFunctionWithName:@"fs_red"];
    pd.colorAttachments[0].pixelFormat = MTLPixelFormatBGRA8Unorm; pd.depthAttachmentPixelFormat = MTLPixelFormatDepth32Float;
    id<MTLRenderPipelineState> ps = [dev newRenderPipelineStateWithDescriptor:pd error:&e];
    if (!ps) { arm("2 depth bounds pipeline builds", NO, "%s", e.localizedDescription.UTF8String); return; }
    MTLDepthStencilDescriptor *dd = [MTLDepthStencilDescriptor new]; dd.depthCompareFunction = MTLCompareFunctionAlways; dd.depthWriteEnabled = NO;
    id<MTLDepthStencilState> ds = [dev newDepthStencilStateWithDescriptor:dd];
    const uint32_t RED = 0xFFFF0000u, BLACK = 0xFF000000u;
    uint32_t ctl = depthBoundsCase(0.25f, 0.9f, NO, ps, ds);
    uint32_t a = depthBoundsCase(0.25f, 0.9f, YES, ps, ds);
    uint32_t b = depthBoundsCase(0.75f, 0.1f, YES, ps, ds);
    uint32_t ctl2 = depthBoundsCase(0.25f, 0.9f, NO, ps, ds);   // a later encoder starts with the test off again
    KV("depthBounds: control / stored.25-frag.9 / stored.75-frag.1 / control again", "%08x / %08x / %08x / %08x", ctl, a, b, ctl2);
    arm("2 no bounds set: the triangle is drawn (red)", ctl == RED, "%08x", ctl);
    arm("2 bounds [0.5,1], stored 0.25, fragment 0.9: discarded (black)", a == BLACK, "%08x", a);
    arm("2 bounds [0.5,1], stored 0.75, fragment 0.1: drawn (red)", b == RED, "%08x", b);
    arm("2 a new encoder starts with the test off (red)", ctl2 == RED, "%08x", ctl2);
    control("2 the claim 'the bounds are ignored' is false", a != RED, "%08x", a);
    control("2 the claim 'the test reads the fragment depth' is false", !(a == RED && b == BLACK), "%08x / %08x", a, b);
}

// --- 3 · newTextureViewWithDescriptor: -------------------------------------------------------------------------
static BOOL readTexel(id<MTLTexture> t, float out[4]) {
    NSError *e = nil;
    static id<MTLComputePipelineState> ps;
    if (!ps) ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"k_read"] error:&e];
    if (!ps || !t) return NO;
    id<MTLBuffer> b = [dev newBufferWithLength:16 options:MTLResourceStorageModeShared];
    id<MTLCommandBuffer> cb = [queue commandBuffer];
    id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
    [ce setComputePipelineState:ps]; [ce setTexture:t atIndex:0]; [ce setBuffer:b offset:0 atIndex:0];
    [ce dispatchThreads:MTLSizeMake(1, 1, 1) threadsPerThreadgroup:MTLSizeMake(1, 1, 1)];
    [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];
    memcpy(out, b.contents, 16);
    return cb.status == MTLCommandBufferStatusCompleted;
}
static BOOL near4(const float v[4], int r, int g, int b, int a) {
    const int w[4] = { r, g, b, a };
    for (int i = 0; i < 4; i++) if (fabsf(v[i] * 255.0f - (float)w[i]) > 0.5f) return NO;
    return YES;
}
static void textureView(void) {
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:4 height:4 mipmapped:YES];
    td.usage = MTLTextureUsageShaderRead | MTLTextureUsagePixelFormatView; td.storageMode = MTLStorageModeShared;
    id<MTLTexture> t = [dev newTextureWithDescriptor:td];
    // level 0 texels are bytes 11 22 33 44, level 1 texels 55 66 77 88 (R G B A)
    uint8_t l0[4 * 4 * 4], l1[2 * 2 * 4];
    for (int i = 0; i < 16; i++) { l0[4 * i] = 0x11; l0[4 * i + 1] = 0x22; l0[4 * i + 2] = 0x33; l0[4 * i + 3] = 0x44; }
    for (int i = 0; i < 4; i++) { l1[4 * i] = 0x55; l1[4 * i + 1] = 0x66; l1[4 * i + 2] = 0x77; l1[4 * i + 3] = 0x88; }
    [t replaceRegion:MTLRegionMake2D(0, 0, 4, 4) mipmapLevel:0 withBytes:l0 bytesPerRow:16];
    [t replaceRegion:MTLRegionMake2D(0, 0, 2, 2) mipmapLevel:1 withBytes:l1 bytesPerRow:8];
    BOOL has = HAS(t, newTextureViewWithDescriptor:);
    arm("3 texture answers newTextureViewWithDescriptor:", has, "%s", has ? "yes" : "absent");
    if (!has) return;

    MTLTextureViewDescriptor *dflt = [MTLTextureViewDescriptor new];
    KV("view descriptor defaults: format / type", "%lu / %lu", (unsigned long)dflt.pixelFormat, (unsigned long)dflt.textureType);
    KV("view descriptor defaults: levels / slices", "{%lu,%lu} / {%lu,%lu}", (unsigned long)dflt.levelRange.location, (unsigned long)dflt.levelRange.length,
       (unsigned long)dflt.sliceRange.location, (unsigned long)dflt.sliceRange.length);
    KV("view descriptor defaults: swizzle", "%u %u %u %u", dflt.swizzle.red, dflt.swizzle.green, dflt.swizzle.blue, dflt.swizzle.alpha);

    MTLTextureViewDescriptor *v1 = [MTLTextureViewDescriptor new];
    v1.pixelFormat = MTLPixelFormatBGRA8Unorm; v1.textureType = MTLTextureType2D;
    v1.levelRange = NSMakeRange(1, 1); v1.sliceRange = NSMakeRange(0, 1);
    v1.swizzle = MTLTextureSwizzleChannelsMake(MTLTextureSwizzleRed, MTLTextureSwizzleGreen, MTLTextureSwizzleBlue, MTLTextureSwizzleAlpha);
    id<MTLTexture> a = [t newTextureViewWithDescriptor:v1];
    arm("3 BGRA8 view of level 1 is made", a != nil, "%p", (__bridge void *)a);
    if (a) {
        arm("3 the view reports BGRA8, 1 level, parent level 1", a.pixelFormat == MTLPixelFormatBGRA8Unorm && a.mipmapLevelCount == 1 && a.parentRelativeLevel == 1,
            "format %lu levels %lu parent level %lu", (unsigned long)a.pixelFormat, (unsigned long)a.mipmapLevelCount, (unsigned long)a.parentRelativeLevel);
        float v[4] = { -1, -1, -1, -1 }; BOOL ok = readTexel(a, v);
        arm("3 a shader reads level 1 through BGRA8: 77 66 55 88", ok && near4(v, 0x77, 0x66, 0x55, 0x88), "%.0f %.0f %.0f %.0f",
            v[0] * 255, v[1] * 255, v[2] * 255, v[3] * 255);
        control("3 the claim 'the view reads level 0' is false", !near4(v, 0x33, 0x22, 0x11, 0x44), "%.0f %.0f", v[0] * 255, v[1] * 255);
    }
    MTLTextureViewDescriptor *v2 = [MTLTextureViewDescriptor new];
    v2.pixelFormat = MTLPixelFormatRGBA8Unorm; v2.textureType = MTLTextureType2D;
    v2.levelRange = NSMakeRange(0, 1); v2.sliceRange = NSMakeRange(0, 1);
    v2.swizzle = MTLTextureSwizzleChannelsMake(MTLTextureSwizzleAlpha, MTLTextureSwizzleBlue, MTLTextureSwizzleGreen, MTLTextureSwizzleRed);
    id<MTLTexture> b = [t newTextureViewWithDescriptor:v2];
    arm("3 swizzled (ABGR) view of level 0 is made", b != nil, "%p", (__bridge void *)b);
    if (b) {
        float v[4] = { -1, -1, -1, -1 }; BOOL ok = readTexel(b, v);
        arm("3 a shader reads level 0 through the swizzle: 44 33 22 11", ok && near4(v, 0x44, 0x33, 0x22, 0x11), "%.0f %.0f %.0f %.0f",
            v[0] * 255, v[1] * 255, v[2] * 255, v[3] * 255);
        control("3 the claim 'the swizzle is dropped' is false", !near4(v, 0x11, 0x22, 0x33, 0x44), "%.0f", v[0] * 255);
    }
    // What a descriptor left at its defaults makes. Recorded, not judged: the defaults are Apple's to define.
    id<MTLTexture> d = [t newTextureViewWithDescriptor:dflt];
    if (d) KV("view from default descriptor: format / type / levels", "%lu / %lu / %lu", (unsigned long)d.pixelFormat, (unsigned long)d.textureType, (unsigned long)d.mipmapLevelCount);
    else KV("view from default descriptor", "nil");
}

// --- 4 · colour attachment map on a Metal 3 encoder ------------------------------------------------------------
// Apple accepts the map and changes nothing: a Metal 3 pipeline always uses the identity mapping.
static void colorMap(void) {
    NSError *e = nil;
    MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
    pd.vertexFunction = [lib newFunctionWithName:@"fs_v"]; pd.fragmentFunction = [lib newFunctionWithName:@"fs_two"];
    pd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm; pd.colorAttachments[1].pixelFormat = MTLPixelFormatRGBA8Unorm;
    id<MTLRenderPipelineState> pso = [dev newRenderPipelineStateWithDescriptor:pd error:&e];
    if (!pso) { arm("4 two-output pipeline builds", NO, "%s", e.localizedDescription.UTF8String); return; }
    id<MTLBuffer> zb = [dev newBufferWithLength:16 options:MTLResourceStorageModeShared];
    *(float *)zb.contents = 0.5f;
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:8 height:8 mipmapped:NO];
    td.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; td.storageMode = MTLStorageModeShared;
    const uint32_t RED = 0xFF0000FFu, GREEN = 0xFF00FF00u;   // RGBA8 little-endian words
    for (int scenario = 0; scenario < 3; scenario++) {
        id<MTLTexture> t0 = [dev newTextureWithDescriptor:td], t1 = [dev newTextureWithDescriptor:td];
        MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor new];
        rp.supportColorAttachmentMapping = YES;
        for (int i = 0; i < 2; i++) {
            rp.colorAttachments[i].texture = i ? t1 : t0; rp.colorAttachments[i].loadAction = MTLLoadActionClear;
            rp.colorAttachments[i].storeAction = MTLStoreActionStore; rp.colorAttachments[i].clearColor = MTLClearColorMake(0, 0, 1, 1);
        }
        MTLLogicalToPhysicalColorAttachmentMap *swap = [MTLLogicalToPhysicalColorAttachmentMap new];
        [swap setPhysicalIndex:1 forLogicalIndex:0]; [swap setPhysicalIndex:0 forLogicalIndex:1];
        id<MTLCommandBuffer> cb = [queue commandBuffer];
        id<MTLRenderCommandEncoder> enc = [cb renderCommandEncoderWithDescriptor:rp];
        if (scenario >= 1 && !HAS(enc, setColorAttachmentMap:)) {
            [enc endEncoding];
            arm("4 render encoder answers setColorAttachmentMap:", NO, "absent");
            return;
        }
        if (scenario == 1) [enc setColorAttachmentMap:swap];
        [enc setRenderPipelineState:pso];
        if (scenario == 2) [enc setColorAttachmentMap:swap];
        [enc setVertexBuffer:zb offset:0 atIndex:0];
        [enc drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
        [enc endEncoding]; [cb commit]; [cb waitUntilCompleted];
        uint32_t a[64], b[64];
        [t0 getBytes:a bytesPerRow:32 fromRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0];
        [t1 getBytes:b bytesPerRow:32 fromRegion:MTLRegionMake2D(0, 0, 8, 8) mipmapLevel:0];
        char n[128];
        snprintf(n, sizeof n, "4 Metal 3, %s: output 0 on attachment 0, 1 on 1",
                 scenario == 0 ? "no map" : scenario == 1 ? "swap before the pipeline" : "swap after the pipeline");
        arm(n, a[27] == RED && b[27] == GREEN && cb.status == MTLCommandBufferStatusCompleted, "a %08x b %08x", a[27], b[27]);
        if (scenario == 1) control("4 the claim 'a Metal 3 swap puts green on attachment 0' is false", a[27] != GREEN, "a %08x", a[27]);
    }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: %s <gaps26.metallib>\n", argv[0]); return 2; }
        dev = MTLCreateSystemDefaultDevice();
        if (!dev) { printf("RESULT UNKNOWN: no Metal device\n"); return 2; }
        NSError *e = nil;
        lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
        if (!lib) { printf("RESULT UNKNOWN: library did not load: %s\n", e.localizedDescription.UTF8String); return 2; }
        queue = [dev newCommandQueue];
        KV("device", "%s", dev.name.UTF8String);
        KV("os", "%s", [NSProcessInfo processInfo].operatingSystemVersionString.UTF8String);
        requiredThreads();
        depthBounds();
        textureView();
        colorMap();
        printf("RESULT %d arms, %d failed\n", gArms, gFail);
        return gFail ? 1 : 0;
    }
}
