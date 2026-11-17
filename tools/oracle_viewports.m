// Pixel readback for Metal viewport arrays, scissor broadcast, and encoder state restoration.
// Build: xcrun clang -fobjc-arc -O1 tools/oracle_viewports.m -framework Foundation \
//        -framework Metal -framework IOKit -o /tmp/oracle_viewports
// Run: oracle_viewports [path/to/isolated/NVMTLDriver]
// An explicit plugin path uses NVAccel directly; the installed bundle is not modified.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOKit/IOKitLib.h>
#import <objc/message.h>
#include <dlfcn.h>

static id<MTLDevice> deviceFrom(const char *path) {
    if (!path) return MTLCreateSystemDefaultDevice();
    if (!dlopen(path, RTLD_NOW | RTLD_LOCAL)) { fprintf(stderr, "LOAD: %s\n", dlerror()); return nil; }
    io_service_t svc = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("NVAccel"));
    if (!svc) return nil;
    id d = ((id (*)(id, SEL, mach_port_t))objc_msgSend)([NSClassFromString(@"NVMTLDevice") alloc],
        NSSelectorFromString(@"initWithAcceleratorPort:"), svc);
    IOObjectRelease(svc);
    return d;
}

int main(int argc, char **argv) { @autoreleasepool {
    id<MTLDevice> d = deviceFrom(argc > 1 ? argv[1] : NULL);
    if (!d) { fprintf(stderr, "NO DEVICE\n"); return 2; }
    NSError *error = nil;
    NSString *source = @"#include <metal_stdlib>\nusing namespace metal;\n"
        "struct V { float4 pos [[position]]; uint view [[viewport_array_index]]; float4 color; };\n"
        "vertex V vs(uint v [[vertex_id]], uint i [[instance_id]]) {\n"
        " float2 p[3]={float2(-1,-1),float2(3,-1),float2(-1,3)};\n"
        " V o; o.pos=float4(p[v],0,1); o.view=i; o.color=i?float4(0,1,0,1):float4(1,0,0,1); return o; }\n"
        "fragment float4 fs(V v [[stage_in]]) { return v.color; }\n";
    id<MTLLibrary> lib = [d newLibraryWithSource:source options:nil error:&error];
    if (!lib) { NSLog(@"%@", error); return 2; }
    MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
    pd.vertexFunction = [lib newFunctionWithName:@"vs"]; pd.fragmentFunction = [lib newFunctionWithName:@"fs"];
    pd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
    id<MTLRenderPipelineState> ps = [d newRenderPipelineStateWithDescriptor:pd error:&error];
    pd.depthAttachmentPixelFormat = MTLPixelFormatDepth32Float;
    id<MTLRenderPipelineState> depthPS = [d newRenderPipelineStateWithDescriptor:pd error:&error];
    if (!ps || !depthPS) { NSLog(@"%@", error); return 2; }
    id<MTLCommandQueue> q = [d newCommandQueue];
    const char *names[] = { "before-pipeline", "per-viewport-scissors", "reset-single-viewport",
        "after-pipeline", "scissors-before-viewports", "reset-single-scissor", "pass-restart",
        "grow-shrink-grow", "sixteen-viewports" };
    int failures = 0;
    for (unsigned mode = 0; mode < sizeof names / sizeof *names; mode++) {
        NSUInteger width = mode == 8 ? 32 : 8;
        MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm
            width:width height:4 mipmapped:NO];
        td.usage = MTLTextureUsageRenderTarget; td.storageMode = MTLStorageModePrivate;
        id<MTLTexture> texture = [d newTextureWithDescriptor:td];
        id<MTLCommandBuffer> cb = [q commandBuffer];
        MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor renderPassDescriptor];
        rp.colorAttachments[0].texture = texture;
        rp.colorAttachments[0].loadAction = MTLLoadActionClear;
        rp.colorAttachments[0].storeAction = MTLStoreActionStore;
        if (mode == 6) {
            td.pixelFormat = MTLPixelFormatDepth32Float;
            rp.depthAttachment.texture = [d newTextureWithDescriptor:td];
            rp.depthAttachment.loadAction = MTLLoadActionClear;
            rp.depthAttachment.storeAction = MTLStoreActionDontCare;
        }
        id<MTLRenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
        MTLViewport vp[16] = {{0,0,4,4,0,1}, {4,0,4,4,0,1}};
        MTLScissorRect sc[2] = {{0,0,2,4}, {6,0,2,4}};
        NSUInteger count = mode == 8 ? 16 : 2;
        if (mode == 8) for (NSUInteger i = 0; i < count; i++) vp[i] = (MTLViewport){i*2,0,2,4,0,1};
        if (mode >= 3) [re setRenderPipelineState:ps];
        if (mode == 4) [re setScissorRects:sc count:2];
        [re setViewports:vp count:count];
        if (mode == 1 || mode == 5) [re setScissorRects:sc count:2];
        if (mode != 1 && mode != 4) [re setScissorRect:(MTLScissorRect){0,0,width,4}];
        if (mode == 2 || mode == 7) [re setViewport:(MTLViewport){0,0,width,4,0,1}];
        if (mode == 7) [re setViewports:vp count:count];
        if (mode < 3) [re setRenderPipelineState:ps];
        if (mode == 6) {
            // Changing attachment shape closes/restarts the driver's render pass. Both
            // viewport and per-viewport scissor arrays must survive that restart.
            [re setScissorRects:sc count:2];
            [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3 instanceCount:2];
            [re setRenderPipelineState:depthPS];
        }
        [re drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3 instanceCount:mode == 2 ? 1 : count];
        [re endEncoding];
        id<MTLBuffer> pixels = [d newBufferWithLength:1024 options:MTLResourceStorageModeShared];
        id<MTLBlitCommandEncoder> bl = [cb blitCommandEncoder];
        [bl copyFromTexture:texture sourceSlice:0 sourceLevel:0 sourceOrigin:MTLOriginMake(0,0,0)
            sourceSize:MTLSizeMake(width,4,1) toBuffer:pixels destinationOffset:0
            destinationBytesPerRow:256 destinationBytesPerImage:1024];
        [bl endEncoding]; [cb commit]; [cb waitUntilCompleted];
        unsigned bad = 0, red = 0, green = 0;
        BOOL clipped = mode == 1 || mode == 4 || mode == 6;
        for (NSUInteger y = 0; y < 4; y++) for (NSUInteger x = 0; x < width; x++) {
            const uint8_t *p = (const uint8_t *)pixels.contents + y*256 + x*4;
            BOOL r = mode == 2 || (clipped || mode == 8 ? x < 2 : x < 4);
            BOOL g = mode != 2 && (clipped ? x >= 6 : x >= (mode == 8 ? 2 : 4));
            bad += p[0] != (r ? 255 : 0) || p[1] != (g ? 255 : 0) || p[2] != 0;
            red += p[0] == 255; green += p[1] == 255;
        }
        BOOL failed = bad || cb.status != MTLCommandBufferStatusCompleted;
        printf("%s: %s red=%u green=%u mismatches=%u status=%lu\n", names[mode], failed ? "FAIL" : "PASS",
            red, green, bad, (unsigned long)cb.status);
        if (cb.error) NSLog(@"%@", cb.error);
        failures += failed;
    }
    return failures ? 1 : 0;
} }
