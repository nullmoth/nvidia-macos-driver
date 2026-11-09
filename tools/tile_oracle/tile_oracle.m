/* tile_oracle.m - what a tile function does, measured on an Apple GPU.
   xcrun clang -fobjc-arc tools/tile_oracle/tile_oracle.m -framework Metal -framework Foundation -o /tmp/tile_oracle && /tmp/tile_oracle
   Two attachments (RGBA16F, R32F) are cleared, a tile kernel multiplies red by a constant and adds one to the second, and the
   attachments are read back. The expected values are what the compute emulation in the driver must also produce. */
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
static const char *kSrc =
"#include <metal_stdlib>\nusing namespace metal;\n"
"struct Px { half4 c [[color(0)]]; float d [[color(1)]]; };\n"
"kernel void tile_k(imageblock<Px, imageblock_layout_implicit> ib, ushort2 tid [[thread_position_in_threadgroup]], constant float &g [[buffer(0)]]) {\n"
"  Px v = ib.read(tid); v.c = half4(v.c.x * half(g), v.c.yzw); v.d += 1.0f; ib.write(v, tid); }\n"
"struct Pm { half4 c [[color(0)]]; };\n"
"kernel void tile_ms(imageblock<Pm, imageblock_layout_implicit> ib, ushort2 tid [[thread_position_in_threadgroup]]) {\n"
"  for (ushort s = 0; s < ib.get_num_samples(); s++) { Pm v = ib.read(tid, s, imageblock_data_rate::sample); v.c.x += half(s) * 0.125h; ib.write(v, tid, s, imageblock_data_rate::sample); } }\n";
int main(void) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:[NSString stringWithUTF8String:kSrc] options:nil error:&e]; if (!lib) { NSLog(@"lib %@", e); return 2; }
    MTLTileRenderPipelineDescriptor *td = [MTLTileRenderPipelineDescriptor new];
    td.tileFunction = [lib newFunctionWithName:@"tile_k"];
    td.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA16Float; td.colorAttachments[1].pixelFormat = MTLPixelFormatR32Float;
    td.threadgroupSizeMatchesTileSize = YES;
    id<MTLRenderPipelineState> ps = [dev newRenderPipelineStateWithTileDescriptor:td options:MTLPipelineOptionNone reflection:nil error:&e]; if (!ps) { NSLog(@"tile pso %@", e); return 3; }
    const int W = 70, H = 40;   /* not a multiple of the tile size on purpose */
    MTLTextureDescriptor *d0 = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA16Float width:W height:H mipmapped:NO];
    MTLTextureDescriptor *d1 = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatR32Float width:W height:H mipmapped:NO];
    d0.usage = d1.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; d0.storageMode = d1.storageMode = MTLStorageModeShared;
    id<MTLTexture> t0 = [dev newTextureWithDescriptor:d0], t1 = [dev newTextureWithDescriptor:d1];
    MTLRenderPassDescriptor *rp = [MTLRenderPassDescriptor renderPassDescriptor];
    rp.colorAttachments[0].texture = t0; rp.colorAttachments[0].loadAction = MTLLoadActionClear; rp.colorAttachments[0].clearColor = MTLClearColorMake(0.25, 0.5, 0.75, 1);
    rp.colorAttachments[0].storeAction = MTLStoreActionStore;
    rp.colorAttachments[1].texture = t1; rp.colorAttachments[1].loadAction = MTLLoadActionClear; rp.colorAttachments[1].clearColor = MTLClearColorMake(2, 0, 0, 1);
    rp.colorAttachments[1].storeAction = MTLStoreActionStore;
    rp.tileWidth = 16; rp.tileHeight = 16;
    id<MTLCommandQueue> q = [dev newCommandQueue]; id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLRenderCommandEncoder> re = [cb renderCommandEncoderWithDescriptor:rp];
    float g = 2.0f; [re setRenderPipelineState:ps]; [re setTileBytes:&g length:4 atIndex:0];
    NSLog(@"tile size reported by the encoder: %lux%lu", (unsigned long)re.tileWidth, (unsigned long)re.tileHeight);
    [re dispatchThreadsPerTile:MTLSizeMake(re.tileWidth, re.tileHeight, 1)];
    [re endEncoding]; [cb commit]; [cb waitUntilCompleted];
    uint16_t px[4]; float dv; int bad = 0;
    for (int y = 0; y < H; y += 7) for (int x = 0; x < W; x += 9) {
        [t0 getBytes:px bytesPerRow:W * 8 fromRegion:MTLRegionMake2D(x, y, 1, 1) mipmapLevel:0];
        [t1 getBytes:&dv bytesPerRow:W * 4 fromRegion:MTLRegionMake2D(x, y, 1, 1) mipmapLevel:0];
        __fp16 *h = (__fp16 *)px;
        if (h[0] != 0.5f || h[1] != 0.5f || h[2] != 0.75f || h[3] != 1.0f || dv != 3.0f) { bad++; NSLog(@"(%d,%d): %g %g %g %g | %g", x, y, (double)h[0], (double)h[1], (double)h[2], (double)h[3], dv); }
    }
    /* arm 2: a 4x multisampled attachment; the tile function sees every sample, adds 0.125*sample to red, and the resolve averages them */
    MTLTileRenderPipelineDescriptor *md = [MTLTileRenderPipelineDescriptor new];
    md.tileFunction = [lib newFunctionWithName:@"tile_ms"]; md.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA16Float; md.rasterSampleCount = 4;
    id<MTLRenderPipelineState> mps = [dev newRenderPipelineStateWithTileDescriptor:md options:MTLPipelineOptionNone reflection:nil error:&e];
    if (!mps) { NSLog(@"msaa tile pso %@", e); return 4; }
    MTLTextureDescriptor *dm = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA16Float width:W height:H mipmapped:NO];
    dm.textureType = MTLTextureType2DMultisample; dm.sampleCount = 4; dm.usage = MTLTextureUsageRenderTarget; dm.storageMode = MTLStorageModePrivate;
    id<MTLTexture> tm = [dev newTextureWithDescriptor:dm];
    d0.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead; id<MTLTexture> tr = [dev newTextureWithDescriptor:d0];
    MTLRenderPassDescriptor *rm = [MTLRenderPassDescriptor renderPassDescriptor];
    rm.colorAttachments[0].texture = tm; rm.colorAttachments[0].resolveTexture = tr; rm.colorAttachments[0].loadAction = MTLLoadActionClear;
    rm.colorAttachments[0].clearColor = MTLClearColorMake(0.25, 0.5, 0.75, 1); rm.colorAttachments[0].storeAction = MTLStoreActionMultisampleResolve;
    rm.tileWidth = 16; rm.tileHeight = 16;
    id<MTLCommandBuffer> cb2 = [q commandBuffer]; id<MTLRenderCommandEncoder> r2 = [cb2 renderCommandEncoderWithDescriptor:rm];
    [r2 setRenderPipelineState:mps]; [r2 dispatchThreadsPerTile:MTLSizeMake(16, 16, 1)]; [r2 endEncoding]; [cb2 commit]; [cb2 waitUntilCompleted];
    int badm = 0;
    for (int y = 0; y < H; y += 7) for (int x = 0; x < W; x += 9) {
        [tr getBytes:px bytesPerRow:W * 8 fromRegion:MTLRegionMake2D(x, y, 1, 1) mipmapLevel:0]; __fp16 *h = (__fp16 *)px;
        if (h[0] < 0.4374f || h[0] > 0.4376f || h[1] != 0.5f || h[2] != 0.75f) { badm++; NSLog(@"msaa (%d,%d): %g %g %g", x, y, (double)h[0], (double)h[1], (double)h[2]); }
    }
    printf("%s\n", badm ? "MSAA ARM MISMATCH" : "MSAA ARM: per-sample red = 0.25 + 0.125 * sample, resolved 0.4375, green and blue untouched");
    bad += badm;
    printf("%s\n", bad ? "ORACLE MISMATCH" : "ORACLE: every sampled pixel is (0.5, 0.5, 0.75, 1) and d = 3.0");
    return bad != 0;
}}
