// oracle_format_twin.m - does the Metal driver draw a pipeline declared in one channel order into an attachment of the other? Build: xcrun clang -fobjc-arc -framework Metal -framework Foundation tools/oracle_format_twin.m -o /tmp/oracle_format_twin. Apple M1 Pro: all four lines store the shader colour in the TEXTURE order with no error.
#import <Metal/Metal.h>
static NSString *src = @"#include <metal_stdlib>\nusing namespace metal;\n"
"vertex float4 vs(uint v [[vertex_id]]) { float2 p[3] = {float2(-1,-1), float2(3,-1), float2(-1,3)}; return float4(p[v],0,1); }\n"
"fragment float4 fs() { return float4(1.0, 0.5, 0.25, 1.0); }\n";
static void run(id<MTLDevice> d, id<MTLCommandQueue> q, id<MTLLibrary> lib, MTLPixelFormat pipeFmt, MTLPixelFormat texFmt, const char *name) {
    MTLRenderPipelineDescriptor *pd = [MTLRenderPipelineDescriptor new];
    pd.vertexFunction = [lib newFunctionWithName:@"vs"]; pd.fragmentFunction = [lib newFunctionWithName:@"fs"];
    pd.colorAttachments[0].pixelFormat = pipeFmt;
    NSError *e = nil; id<MTLRenderPipelineState> ps = [d newRenderPipelineStateWithDescriptor:pd error:&e];
    if (!ps) { printf("%s: pipeline refused: %s\n", name, e.description.UTF8String); return; }
    MTLTextureDescriptor *td = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:texFmt width:4 height:4 mipmapped:NO];
    td.usage = MTLTextureUsageRenderTarget; td.storageMode = MTLStorageModeShared;
    id<MTLTexture> t = [d newTextureWithDescriptor:td];
    MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor renderPassDescriptor];
    rp.colorAttachments[0].texture = t; rp.colorAttachments[0].loadAction = MTLLoadActionClear; rp.colorAttachments[0].storeAction = MTLStoreActionStore;
    rp.colorAttachments[0].clearColor = MTLClearColorMake(0,0,0,0);
    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLRenderCommandEncoder> en = [cb renderCommandEncoderWithDescriptor:rp];
    [en setRenderPipelineState:ps]; [en drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3]; [en endEncoding];
    [cb commit]; [cb waitUntilCompleted];
    uint8_t px[4] = {0}; [t getBytes:px bytesPerRow:16 fromRegion:MTLRegionMake2D(1,1,1,1) mipmapLevel:0];
    printf("%s: status %ld error %s bytes %u %u %u %u\n", name, (long)cb.status, cb.error ? cb.error.description.UTF8String : "none", px[0], px[1], px[2], px[3]);
}
int main(void) { @autoreleasepool {
    id<MTLDevice> d = MTLCreateSystemDefaultDevice(); id<MTLCommandQueue> q = [d newCommandQueue];
    NSError *e = nil; id<MTLLibrary> lib = [d newLibraryWithSource:src options:nil error:&e]; if (!lib) { puts(e.description.UTF8String); return 1; }
    printf("device %s\n", d.name.UTF8String);
    run(d, q, lib, MTLPixelFormatRGBA8Unorm, MTLPixelFormatRGBA8Unorm, "control pipe RGBA8 -> tex RGBA8");
    run(d, q, lib, MTLPixelFormatBGRA8Unorm, MTLPixelFormatBGRA8Unorm, "control pipe BGRA8 -> tex BGRA8");
    run(d, q, lib, MTLPixelFormatBGRA8Unorm, MTLPixelFormatRGBA8Unorm, "MISMATCH pipe BGRA8 -> tex RGBA8");
    run(d, q, lib, MTLPixelFormatRGBA8Unorm, MTLPixelFormatBGRA8Unorm, "MISMATCH pipe RGBA8 -> tex BGRA8");
} return 0; }
