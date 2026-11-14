// Tile shading oracle, Metal 3 API: tile pipelines, tile bindings, dispatchThreadsPerTile, threadgroup memory per tile,
// implicit imageblock access to a colour attachment. Apple hardware is the truth; the same binary runs on the NVIDIA box.
// usage: tile_oracle <tile.metallib>
#import "tile_common.h"
#include <stdarg.h>

enum { W = 64, H = 48, TS = 16 };
typedef struct { float rect[4], color[4]; } RectU;

static id<MTLDevice> dev; static id<MTLCommandQueue> q; static id<MTLLibrary> lib;
static id<MTLRenderPipelineState> psoPattern, psoRect;

static id<MTLRenderPipelineState> tile_pso(const char *name, MTLPixelFormat fmt, BOOL matches) {
    MTLTileRenderPipelineDescriptor *d = [MTLTileRenderPipelineDescriptor new];
    d.tileFunction = fn_of(lib, name);
    d.colorAttachments[0].pixelFormat = fmt;
    d.threadgroupSizeMatchesTileSize = matches;
    NSError *e = nil;
    id<MTLRenderPipelineState> p = [dev newRenderPipelineStateWithTileDescriptor:d options:MTLPipelineOptionNone reflection:nil error:&e];
    if (!p) { fprintf(stderr, "tile pipeline %s: %s\n", name, e.localizedDescription.UTF8String); exit(2); }
    return p;
}
static NSUInteger g_tgbytes = 0;   // MTLRenderPassDescriptor.threadgroupMemoryLength of the passes that follow
static MTLRenderPassDescriptor *pass_for(id<MTLTexture> t, MTLLoadAction load, double r, double g, double b, double a) {
    MTLRenderPassDescriptor *p = [MTLRenderPassDescriptor renderPassDescriptor];
    p.colorAttachments[0].texture = t; p.colorAttachments[0].loadAction = load;
    p.colorAttachments[0].clearColor = MTLClearColorMake(r, g, b, a);
    p.colorAttachments[0].storeAction = MTLStoreActionStore;
    p.tileWidth = TS; p.tileHeight = TS; p.threadgroupMemoryLength = g_tgbytes;
    return p;
}
static void draw_pattern(id<MTLRenderCommandEncoder> e) {
    [e setRenderPipelineState:psoPattern];
    [e drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
}
static void draw_rect(id<MTLRenderCommandEncoder> e, float x, float y, float w, float h, const float c[4]) {
    RectU u = { { x, y, w, h }, { c[0], c[1], c[2], c[3] } };
    [e setRenderPipelineState:psoRect];
    [e setVertexBytes:&u length:sizeof u atIndex:0]; [e setFragmentBytes:&u length:sizeof u atIndex:0];
    [e drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4];
}
static void pat(int x, int y, int px[4]) { px[0] = (x * 4) & 255; px[1] = (y * 5) & 255; px[2] = ((x ^ y) & 1) * 200; px[3] = 255; }

// CPU expectation of the per-tile average of the pattern, for tiles of ts x ts.
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
        dev = MTLCreateSystemDefaultDevice(); q = [dev newCommandQueue]; lib = load_lib(dev, argv[1]);
        printf("device: %s\n", dev.name.UTF8String);
        psoPattern = make_draw_pso(dev, lib, "vs_full", "fs_pattern", MTLPixelFormatRGBA8Unorm);
        psoRect = make_draw_pso(dev, lib, "vs_rect", "fs_const", MTLPixelFormatRGBA8Unorm);
        id<MTLRenderPipelineState> tAvg = tile_pso("tile_avg", MTLPixelFormatRGBA8Unorm, YES);
        const int tilesX = W / TS, tilesY = H / TS;

        // Arms 1-4: draw, per-tile average, write back, draw again.
        {
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
            id<MTLBuffer> avg = [dev newBufferWithLength:tilesX * tilesY * 16 options:MTLResourceStorageModeShared];
            memset(avg.contents, 0xee, avg.length);
            const NSUInteger ibl = [tAvg imageblockMemoryLengthForDimensions:MTLSizeMake(TS, TS, 1)];
            g_tgbytes = ibl + TS * TS * 16;
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 0, 0, 0, 0)];
            arm("1 encoder reports the tile size it was given", e.tileWidth == TS && e.tileHeight == TS, 1, "tile %lux%lu", (unsigned long)e.tileWidth, (unsigned long)e.tileHeight);
            draw_pattern(e);
            [e setRenderPipelineState:tAvg];
            [e setTileBuffer:avg offset:0 atIndex:0];
            uint32_t tx = tilesX; [e setTileBytes:&tx length:4 atIndex:1];
            if (getenv("TILE_DEBUG")) printf("  imageblockMemoryLength %lu sampleLength %lu\n", (unsigned long)ibl, (unsigned long)tAvg.imageblockSampleLength);
            [e setThreadgroupMemoryLength:TS * TS * 16 offset:ibl atIndex:0];
            g_tgbytes = 0;
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            const float rc[4] = { 200 / 255.0f, 100 / 255.0f, 50 / 255.0f, 1 };
            draw_rect(e, -0.5f, -0.5f, 1.0f, 1.0f, rc);
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            arm("0 command buffer completed", cb.status == MTLCommandBufferStatusCompleted, 1, "status %ld", (long)cb.status);
            const float *a = avg.contents; int bad = 0; double worst = 0;
            if (getenv("TILE_DEBUG")) { for (int i = 0; i < 6; i++) { float ex[4]; cpu_avg(TS, i % tilesX, i / tilesX, ex); printf("  tile %d got %.4f %.4f %.4f %.4f exp %.4f %.4f %.4f %.4f\n", i, a[i*4], a[i*4+1], a[i*4+2], a[i*4+3], ex[0], ex[1], ex[2], ex[3]); } }
            for (int ty = 0; ty < tilesY; ty++) for (int tx2 = 0; tx2 < tilesX; tx2++) {
                float ex[4]; cpu_avg(TS, tx2, ty, ex);
                for (int c = 0; c < 4; c++) { double d = fabs(a[(ty * tilesX + tx2) * 4 + c] - ex[c]); if (d > worst) worst = d; if (d > 0.004) bad++; }
            }
            arm("2 tile kernel read the draw and averaged each tile", bad == 0, 1, "%d of %d values off, worst %.5f", bad, tilesX * tilesY * 4, worst);
            arm("2n NEG the same output is not the 32x32 average", ({ int same = 1; for (int ty = 0; ty < tilesY && same; ty++) for (int tx2 = 0; tx2 < tilesX && same; tx2++) {
                float ex[4]; cpu_avg(32, tx2 / 2, ty / 2, ex);
                for (int c = 0; c < 4; c++) if (fabsf(a[(ty * tilesX + tx2) * 4 + c] - ex[c]) > 0.004f) same = 0; } same; }), 0, "tile size is observable in the result");
            uint8_t px[W * H * 4]; read_rgba8(t, px);
            int wbBad = 0, rcBad = 0, outBad = 0, rcN = 0;
            for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
                const uint8_t *p = px + (y * W + x) * 4;
                int inRect = x >= 16 && x < 48 && y >= 12 && y < 36;
                if (inRect) { rcN++; if (!near_px(p, rc, 1.5f / 255)) rcBad++; }
                else { float ex[4]; cpu_avg(TS, x / TS, y / TS, ex); if (!near_px(p, ex, 2.5f / 255)) outBad++; }
            }
            wbBad = outBad;
            arm("3 tile kernel wrote its average back into every pixel", wbBad == 0, 1, "%d pixels off outside the rect", wbBad);
            arm("4 the draw after the tile dispatch landed (pass resumed)", rcBad == 0 && rcN == 32 * 24, 1, "%d of %d rect pixels off", rcBad, rcN);
        }

        // Arm 5: tile and thread coordinates on a size that is not a multiple of the tile.
        {
            const int w = 70, h = 50;
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, w, h);
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 0, 0, 0, 0)];
            [e setRenderPipelineState:tile_pso("tile_ids", MTLPixelFormatRGBA8Unorm, YES)];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            uint8_t px[70 * 50 * 4]; read_rgba8(t, px); int bad = 0;
            for (int y = 0; y < h; y++) for (int x = 0; x < w; x++) {
                const uint8_t *p = px + (y * w + x) * 4;
                if (p[0] != x / TS || p[1] != y / TS || p[2] != x % TS || p[3] != y % TS) bad++; }
            arm("5 tile and thread ids land on the right pixel, partial tiles too", bad == 0, 1, "%d of %d pixels off", bad, w * h);
        }

        // Arm 6: two threadgroup memory arguments of different sizes.
        {
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
            id<MTLBuffer> hist = [dev newBufferWithLength:tilesX * tilesY * 16 * 4 options:MTLResourceStorageModeShared];
            memset(hist.contents, 0xee, hist.length);
            id<MTLRenderPipelineState> tHist = tile_pso("tile_hist", MTLPixelFormatRGBA8Unorm, YES);
            const NSUInteger ibh = [tHist imageblockMemoryLengthForDimensions:MTLSizeMake(TS, TS, 1)];
            g_tgbytes = ibh + 64 + TS * TS * 4;
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 0, 0, 0, 0)];
            draw_pattern(e);
            [e setRenderPipelineState:tHist];
            [e setTileBuffer:hist offset:0 atIndex:0];
            uint32_t tx = tilesX; [e setTileBytes:&tx length:4 atIndex:1];
            [e setThreadgroupMemoryLength:64 offset:ibh atIndex:0];
            [e setThreadgroupMemoryLength:TS * TS * 4 offset:ibh + 64 atIndex:1];
            g_tgbytes = 0;
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            const uint32_t *hs = hist.contents; int bad = 0;
            for (int ty = 0; ty < tilesY; ty++) for (int tx2 = 0; tx2 < tilesX; tx2++) {
                uint32_t ex[16] = { 0 };
                for (int y = ty * TS; y < (ty + 1) * TS; y++) for (int x = tx2 * TS; x < (tx2 + 1) * TS; x++) {
                    float v = ((x * 4) & 255) / 255.0f; int b = (int)(v * 15.999f); if (b > 15) b = 15; ex[b]++; }
                for (int b = 0; b < 16; b++) if (hs[(ty * tilesX + tx2) * 16 + b] != ex[b]) bad++;
            }
            arm("6 per-tile threadgroup memory (atomic histogram + scratch)", bad == 0, 1, "%d of %d bins off", bad, tilesX * tilesY * 16);
        }

        // Arm 8: a pass with no draw: the Clear is applied before the tile kernel reads it.
        {
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 10 / 255.0, 20 / 255.0, 30 / 255.0, 1)];
            [e setRenderPipelineState:tile_pso("tile_addone", MTLPixelFormatRGBA8Unorm, YES)];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            uint8_t px[W * H * 4]; read_rgba8(t, px); int bad = 0;
            for (int i = 0; i < W * H; i++) { const uint8_t *p = px + i * 4; if (p[0] != 12 || p[1] != 20 || p[2] != 30 || p[3] != 255) bad++; }
            arm("8 clear applied before a draw-less tile pass, two dispatches chain", bad == 0, 1, "%d of %d pixels off (expect 12,20,30,255)", bad, W * H);
        }

        // Arm 9: what a tile kernel reads after a draw, texel by texel.
        {
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
            id<MTLBuffer> dump = [dev newBufferWithLength:W * H * 16 options:MTLResourceStorageModeShared];
            memset(dump.contents, 0xee, dump.length);
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 0, 0, 0, 0)];
            draw_pattern(e);
            [e setRenderPipelineState:tile_pso("tile_dump", MTLPixelFormatRGBA8Unorm, YES)];
            [e setTileBuffer:dump offset:0 atIndex:0];
            uint32_t wd = W; [e setTileBytes:&wd length:4 atIndex:1];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            const float *d = dump.contents; int bad = 0;
            for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
                int p[4]; pat(x, y, p);
                for (int c = 0; c < 4; c++) if (fabsf(d[(y * W + x) * 4 + c] - p[c] / 255.0f) > 0.002f) bad++; }
            if (getenv("TILE_DEBUG")) printf("  texel (3,5) read %.4f %.4f %.4f %.4f\n", d[(5*W+3)*4], d[(5*W+3)*4+1], d[(5*W+3)*4+2], d[(5*W+3)*4+3]);
            arm("9 tile kernel reads exactly what the draw wrote", bad == 0, 1, "%d of %d values off", bad, W * H * 4);
        }

        // Arm 10: tile threadgroup memory persists across dispatches of one pass, with a draw between them.
        {
            id<MTLTexture> t = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
            id<MTLBuffer> out = [dev newBufferWithLength:tilesX * tilesY * 256 * 4 options:MTLResourceStorageModeShared];
            memset(out.contents, 0xee, out.length);
            id<MTLRenderPipelineState> tw = tile_pso("tile_tg_write", MTLPixelFormatRGBA8Unorm, YES), tr = tile_pso("tile_tg_read", MTLPixelFormatRGBA8Unorm, YES);
            g_tgbytes = 1024 + 4096;
            id<MTLCommandBuffer> cb = [q commandBuffer];
            id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pass_for(t, MTLLoadActionClear, 0, 0, 0, 0)];
            g_tgbytes = 0;
            [e setRenderPipelineState:tw];
            [e setThreadgroupMemoryLength:4096 offset:1024 atIndex:0];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            draw_pattern(e);
            [e setRenderPipelineState:tr];
            [e setTileBuffer:out offset:0 atIndex:0];
            uint32_t tx = tilesX; [e setTileBytes:&tx length:4 atIndex:1];
            [e setThreadgroupMemoryLength:4096 offset:1024 atIndex:0];
            [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
            const uint32_t *o = out.contents; int bad = 0;
            for (int ty = 0; ty < tilesY; ty++) for (int tx2 = 0; tx2 < tilesX; tx2++) for (int i = 0; i < 256; i++) {
                uint32_t ex = ((uint32_t)ty << 24) | ((uint32_t)tx2 << 16) | (uint32_t)i;
                if (o[(ty * tilesX + tx2) * 256 + i] != ex) bad++; }
            arm("10 tile threadgroup memory persists across dispatches and a draw", bad == 0, 1, "%d of %d words off", bad, tilesX * tilesY * 256);
        }
        printf("%d arms, %d unexpected\n", g_arms, g_unexpected);
        return g_unexpected ? 1 : 0;
    }
}
