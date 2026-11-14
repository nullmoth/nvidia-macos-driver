// Tile shading oracle, Metal 4 API: MTL4TileRenderPipelineDescriptor built by MTL4Compiler, tile argument table,
// MTL4RenderCommandEncoder dispatchThreadsPerTile and threadgroup memory per tile. The same arms as arms 1-4 of
// tile_oracle.m. Apple hardware is the truth; the same binary runs on the NVIDIA box.
// usage: tile_oracle_mtl4 <tile.metallib>
#import "tile_common.h"
#include <stdarg.h>

enum { W = 64, H = 48, TS = 16 };
typedef struct { float rect[4], color[4]; } RectU;

static void pat(int x, int y, int px[4]) { px[0] = (x * 4) & 255; px[1] = (y * 5) & 255; px[2] = ((x ^ y) & 1) * 200; px[3] = 255; }
static void cpu_avg(int ts, int tx, int ty, float out[4]) {
    double s[4] = { 0, 0, 0, 0 };
    for (int y = ty * ts; y < (ty + 1) * ts; y++) for (int x = tx * ts; x < (tx + 1) * ts; x++) {
        int p[4]; pat(x, y, p); for (int c = 0; c < 4; c++) s[c] += p[c] / 255.0; }
    for (int c = 0; c < 4; c++) out[c] = (float)(s[c] / (ts * ts));
}
static BOOL near_px(const uint8_t *p, const float e[4], float tol) {
    for (int c = 0; c < 4; c++) if (fabsf(p[c] / 255.0f - e[c]) > tol) return NO;
    return YES;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: %s tile.metallib\n", argv[0]); return 2; }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        printf("device: %s\n", dev.name.UTF8String);
        id<MTLLibrary> lib = load_lib(dev, argv[1]);
        NSError *e = nil;
        id<MTL4Compiler> comp = [dev newCompilerWithDescriptor:[MTL4CompilerDescriptor new] error:&e];
        if (!comp) { fprintf(stderr, "compiler: %s\n", e.localizedDescription.UTF8String); return 2; }
        MTL4LibraryFunctionDescriptor *(^fd)(const char *) = ^MTL4LibraryFunctionDescriptor *(const char *n) {
            MTL4LibraryFunctionDescriptor *d = [MTL4LibraryFunctionDescriptor new]; d.name = @(n); d.library = lib; return d; };

        MTL4RenderPipelineDescriptor *rd = [MTL4RenderPipelineDescriptor new];
        rd.vertexFunctionDescriptor = fd("vs_full"); rd.fragmentFunctionDescriptor = fd("fs_pattern");
        rd.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> psoPattern = [comp newRenderPipelineStateWithDescriptor:rd compilerTaskOptions:nil error:&e];
        MTL4RenderPipelineDescriptor *rr = [MTL4RenderPipelineDescriptor new];
        rr.vertexFunctionDescriptor = fd("vs_rect"); rr.fragmentFunctionDescriptor = fd("fs_const");
        rr.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> psoRect = [comp newRenderPipelineStateWithDescriptor:rr compilerTaskOptions:nil error:&e];
        MTL4TileRenderPipelineDescriptor *td = [MTL4TileRenderPipelineDescriptor new];
        td.tileFunctionDescriptor = fd("tile_avg"); td.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        td.threadgroupSizeMatchesTileSize = YES;
        id<MTLRenderPipelineState> tAvg = [comp newRenderPipelineStateWithDescriptor:td compilerTaskOptions:nil error:&e];
        if (!psoPattern || !psoRect || !tAvg) { fprintf(stderr, "pipelines: %s\n", e.localizedDescription.UTF8String); return 2; }
        arm("0a Metal 4 tile pipeline created and reports its tile shape", tAvg.threadgroupSizeMatchesTileSize && tAvg.imageblockSampleLength == 4, 1,
            "matches %d, imageblock sample %lu", tAvg.threadgroupSizeMatchesTileSize, (unsigned long)tAvg.imageblockSampleLength);

        const int tilesX = W / TS, tilesY = H / TS;
        id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
        id<MTLBuffer> avg = [dev newBufferWithLength:tilesX * tilesY * 16 options:MTLResourceStorageModeShared];
        memset(avg.contents, 0xee, avg.length);
        id<MTLBuffer> ctl = [dev newBufferWithLength:256 options:MTLResourceStorageModeShared];   // tilesX at +0, a RectU at +16
        *(uint32_t *)ctl.contents = tilesX;
        RectU ru = { { -0.5f, -0.5f, 1.0f, 1.0f }, { 200 / 255.0f, 100 / 255.0f, 50 / 255.0f, 1 } };
        memcpy((uint8_t *)ctl.contents + 16, &ru, sizeof ru);

        MTLResidencySetDescriptor *rsd = [MTLResidencySetDescriptor new];
        id<MTLResidencySet> rs = [dev newResidencySetWithDescriptor:rsd error:&e];
        [rs addAllocation:t]; [rs addAllocation:avg]; [rs addAllocation:ctl]; [rs commit];
        id<MTL4CommandQueue> q = [dev newMTL4CommandQueue]; [q addResidencySet:rs];
        id<MTL4CommandAllocator> alloc = [dev newCommandAllocator];
        id<MTL4CommandBuffer> cb = [dev newCommandBuffer];

        MTL4ArgumentTableDescriptor *atd = [MTL4ArgumentTableDescriptor new]; atd.maxBufferBindCount = 4;
        id<MTL4ArgumentTable> tileTable = [dev newArgumentTableWithDescriptor:atd error:&e];
        id<MTL4ArgumentTable> drawTable = [dev newArgumentTableWithDescriptor:atd error:&e];
        [tileTable setAddress:avg.gpuAddress atIndex:0];
        [tileTable setAddress:ctl.gpuAddress atIndex:1];
        [drawTable setAddress:ctl.gpuAddress + 16 atIndex:0];

        MTL4RenderPassDescriptor *pd = [MTL4RenderPassDescriptor new];
        pd.colorAttachments[0].texture = t; pd.colorAttachments[0].loadAction = MTLLoadActionClear;
        pd.colorAttachments[0].storeAction = MTLStoreActionStore; pd.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
        pd.tileWidth = TS; pd.tileHeight = TS;
        const NSUInteger ibl = [tAvg imageblockMemoryLengthForDimensions:MTLSizeMake(TS, TS, 1)];
        pd.threadgroupMemoryLength = ibl + TS * TS * 16;

        [cb beginCommandBufferWithAllocator:alloc];
        [cb useResidencySet:rs];
        id<MTL4RenderCommandEncoder> en = [cb renderCommandEncoderWithDescriptor:pd];
        arm("1 encoder reports the tile size it was given", en.tileWidth == TS && en.tileHeight == TS, 1, "tile %lux%lu", (unsigned long)en.tileWidth, (unsigned long)en.tileHeight);
        [en setRenderPipelineState:psoPattern];
        [en drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
        [en barrierAfterEncoderStages:MTLStageFragment beforeEncoderStages:MTLStageTile visibilityOptions:MTL4VisibilityOptionDevice];
        [en setRenderPipelineState:tAvg];
        [en setArgumentTable:tileTable atStages:MTLRenderStageTile];
        [en setThreadgroupMemoryLength:TS * TS * 16 offset:ibl atIndex:0];
        [en dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
        [en barrierAfterEncoderStages:MTLStageTile beforeEncoderStages:MTLStageVertex visibilityOptions:MTL4VisibilityOptionDevice];
        [en setRenderPipelineState:psoRect];
        [en setArgumentTable:drawTable atStages:MTLRenderStageVertex | MTLRenderStageFragment];
        [en drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4];
        [en endEncoding];
        [cb endCommandBuffer];
        id<MTLSharedEvent> ev = [dev newSharedEvent];
        id<MTL4CommandBuffer> cbs[1] = { cb };
        [q commit:cbs count:1];
        [q signalEvent:ev value:1];
        BOOL done = [ev waitUntilSignaledValue:1 timeoutMS:10000];
        arm("0 command buffer completed", done, 1, "%s", done ? "signalled" : "timed out");

        const float *a = avg.contents; int bad = 0; double worst = 0;
        for (int ty = 0; ty < tilesY; ty++) for (int tx = 0; tx < tilesX; tx++) {
            float ex[4]; cpu_avg(TS, tx, ty, ex);
            for (int c = 0; c < 4; c++) { double d = fabs(a[(ty * tilesX + tx) * 4 + c] - ex[c]); if (d > worst) worst = d; if (d > 0.004) bad++; }
        }
        arm("2 tile kernel read the draw and averaged each tile", bad == 0, 1, "%d of %d values off, worst %.5f", bad, tilesX * tilesY * 4, worst);
        arm("2n NEG the same output is not the 32x32 average", ({ int same = 1; for (int ty = 0; ty < tilesY && same; ty++) for (int tx = 0; tx < tilesX && same; tx++) {
            float ex[4]; cpu_avg(32, tx / 2, ty / 2, ex);
            for (int c = 0; c < 4; c++) if (fabsf(a[(ty * tilesX + tx) * 4 + c] - ex[c]) > 0.004f) same = 0; } same; }), 0, "tile size is observable in the result");
        uint8_t px[W * H * 4]; read_rgba8(t, px);
        int outBad = 0, rcBad = 0, rcN = 0;
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
            const uint8_t *p = px + (y * W + x) * 4;
            if (x >= 16 && x < 48 && y >= 12 && y < 36) { rcN++; if (!near_px(p, ru.color, 1.5f / 255)) rcBad++; }
            else { float ex[4]; cpu_avg(TS, x / TS, y / TS, ex); if (!near_px(p, ex, 2.5f / 255)) outBad++; }
        }
        arm("3 tile kernel wrote its average back into every pixel", outBad == 0, 1, "%d pixels off outside the rect", outBad);
        arm("4 the draw after the tile dispatch landed (pass resumed)", rcBad == 0 && rcN == 32 * 24, 1, "%d of %d rect pixels off", rcBad, rcN);
        printf("%d arms, %d unexpected\n", g_arms, g_unexpected);
        return g_unexpected ? 1 : 0;
    }
}
