// Tile shading oracle, explicit imageblock: fragment shaders write and read an imageblock structure, a tile kernel
// resolves it into a texture. Apple hardware is the truth; the same binary runs on the NVIDIA box.
// usage: tile_oracle_explicit <tile.metallib>
#import "tile_common.h"
#include <stdarg.h>

enum { W = 64, H = 48, TS = 16, GBBYTES = 16 };
typedef struct { float rect[4], color[4]; } RectU;

static id<MTLDevice> dev; static id<MTLCommandQueue> q; static id<MTLLibrary> lib;

static id<MTLRenderPipelineState> frag_pso(const char *vs, const char *fs) {
    MTLRenderPipelineDescriptor *d = [MTLRenderPipelineDescriptor new];
    d.vertexFunction = fn_of(lib, vs); d.fragmentFunction = fn_of(lib, fs);
    d.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
    d.colorAttachments[0].writeMask = MTLColorWriteMaskNone;   // the fragment writes the imageblock, not the colour
    NSError *e = nil; id<MTLRenderPipelineState> p = [dev newRenderPipelineStateWithDescriptor:d error:&e];
    if (!p) { fprintf(stderr, "pipeline %s/%s: %s\n", vs, fs, e.localizedDescription.UTF8String); exit(2); }
    return p;
}
static id<MTLRenderPipelineState> tile_pso(const char *name, BOOL matches) {
    MTLTileRenderPipelineDescriptor *d = [MTLTileRenderPipelineDescriptor new];
    d.tileFunction = fn_of(lib, name);
    d.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
    d.threadgroupSizeMatchesTileSize = matches;
    NSError *e = nil;
    id<MTLRenderPipelineState> p = [dev newRenderPipelineStateWithTileDescriptor:d options:MTLPipelineOptionNone reflection:nil error:&e];
    if (!p) { fprintf(stderr, "tile pipeline %s: %s\n", name, e.localizedDescription.UTF8String); exit(2); }
    return p;
}
static void rect_draw(id<MTLRenderCommandEncoder> e, id<MTLRenderPipelineState> p, float x, float y, float w, float h, NSUInteger instances) {
    RectU u = { { x, y, w, h }, { 0, 0, 0, 0 } };
    [e setRenderPipelineState:p];
    [e setVertexBytes:&u length:sizeof u atIndex:0]; [e setFragmentBytes:&u length:sizeof u atIndex:0];
    [e drawPrimitives:MTLPrimitiveTypeTriangleStrip vertexStart:0 vertexCount:4 instanceCount:instances];
}

int main(int argc, char **argv) {
    @autoreleasepool {
        if (argc < 2) { fprintf(stderr, "usage: %s tile.metallib\n", argv[0]); return 2; }
        dev = MTLCreateSystemDefaultDevice(); q = [dev newCommandQueue]; lib = load_lib(dev, argv[1]);
        printf("device: %s\n", dev.name.UTF8String);
        id<MTLRenderPipelineState> init = frag_pso("vs_full", "fs_gb_init"), acc = frag_pso("vs_rect", "fs_gb_acc");
        id<MTLRenderPipelineState> resolve = tile_pso("tile_resolve", YES);

        id<MTLTexture> rt = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
        id<MTLTexture> out = make_rt(dev, MTLPixelFormatRGBA8Unorm, W, H);
        MTLRenderPassDescriptor *pd = [MTLRenderPassDescriptor renderPassDescriptor];
        pd.colorAttachments[0].texture = rt; pd.colorAttachments[0].loadAction = MTLLoadActionClear;
        pd.colorAttachments[0].storeAction = MTLStoreActionStore; pd.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
        pd.tileWidth = TS; pd.tileHeight = TS; NSUInteger isl = MAX(MAX(init.imageblockSampleLength, acc.imageblockSampleLength), resolve.imageblockSampleLength);
        if (getenv("TILE_DEBUG")) printf("  imageblockSampleLength init %lu acc %lu resolve %lu\n", (unsigned long)init.imageblockSampleLength, (unsigned long)acc.imageblockSampleLength, (unsigned long)resolve.imageblockSampleLength);
        arm("1a pipelines report the imageblock sample length", isl >= GBBYTES, 1, "init %lu acc %lu resolve %lu (struct is %d bytes)", (unsigned long)init.imageblockSampleLength, (unsigned long)acc.imageblockSampleLength, (unsigned long)resolve.imageblockSampleLength, GBBYTES);
        pd.imageblockSampleLength = isl;

        id<MTLCommandBuffer> cb = [q commandBuffer];
        id<MTLRenderCommandEncoder> e = [cb renderCommandEncoderWithDescriptor:pd];
        [e setRenderPipelineState:init];
        [e drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
        // the rect [16,48) x [12,36) is drawn twice in one draw (two instances): a pixel in it must see the first
        // instance's write when the second runs, in raster order
        rect_draw(e, acc, -0.5f, -0.5f, 1.0f, 1.0f, 2);
        [e setRenderPipelineState:resolve];
        [e setTileTexture:out atIndex:0];
        [e dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
        [e endEncoding]; [cb commit]; [cb waitUntilCompleted];
        arm("0 command buffer completed", cb.status == MTLCommandBufferStatusCompleted, 1, "status %ld%s", (long)cb.status,
            cb.error ? cb.error.localizedDescription.UTF8String : "");

        uint8_t px[W * H * 4]; read_rgba8(out, px);
        if (getenv("TILE_DEBUG")) for (int i = 0; i < 4; i++) { int x = 2 + i * 20, y = 5 + i * 10; const uint8_t *p = px + (y * W + x) * 4; printf("  px(%d,%d) = %d %d %d %d\n", x, y, p[0], p[1], p[2], p[3]); }
        int badA = 0, badB1 = 0, badB3 = 0, n1 = 0, n3 = 0;
        for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
            const uint8_t *p = px + (y * W + x) * 4;
            int inRect = x >= 16 && x < 48 && y >= 12 && y < 36;
            if (p[0] != ((x * 4) & 255) || p[1] != ((y * 5) & 255)) badA++;
            // b / 8 in the blue channel: b = 1 outside the rect, 3 inside (init 1, two accumulating instances)
            int want = inRect ? 3 : 1;
            int got = (int)(p[2] / 255.0 * 8.0 + 0.5);
            if (inRect) { n3++; if (got != want) badB3++; } else { n1++; if (got != want) badB1++; }
        }
        arm("1 fragment imageblock write reached the tile kernel (members a)", badA == 0, 1, "%d of %d pixels off", badA, W * H);
        arm("2 untouched imageblock cells keep the first draw's value (b = 1)", badB1 == 0, 1, "%d of %d pixels off", badB1, n1);
        arm("3 two overlapping instances accumulate in raster order (b = 3)", badB3 == 0, 1, "%d of %d pixels off", badB3, n3);
        arm("3n NEG b = 2 is not what the rect holds", ({ int all2 = 1; for (int y = 12; y < 36 && all2; y++) for (int x = 16; x < 48 && all2; x++) {
            int got = (int)(px[(y * W + x) * 4 + 2] / 255.0 * 8.0 + 0.5); if (got != 2) all2 = 0; } all2; }), 0, "raster order is observable in the result");

        // Every member type: float4, int, uint, uchar4, uchar. The first draw writes them, the rect (two instances) changes them.
        {
            id<MTLRenderPipelineState> init2 = frag_pso("vs_full", "fs_gb2_init"), acc2 = frag_pso("vs_rect", "fs_gb2_acc");
            id<MTLRenderPipelineState> dump = tile_pso("tile_gb2_dump", YES);
            id<MTLBuffer> out2 = [dev newBufferWithLength:W * H * 8 * 4 options:MTLResourceStorageModeShared];
            memset(out2.contents, 0xee, out2.length);
            MTLRenderPassDescriptor *pd2 = [MTLRenderPassDescriptor renderPassDescriptor];
            pd2.colorAttachments[0].texture = rt; pd2.colorAttachments[0].loadAction = MTLLoadActionClear;
            pd2.colorAttachments[0].storeAction = MTLStoreActionStore; pd2.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0);
            pd2.tileWidth = TS; pd2.tileHeight = TS;
            pd2.imageblockSampleLength = MAX(MAX(init2.imageblockSampleLength, acc2.imageblockSampleLength), dump.imageblockSampleLength);
            id<MTLCommandBuffer> cb2 = [q commandBuffer];
            id<MTLRenderCommandEncoder> e2 = [cb2 renderCommandEncoderWithDescriptor:pd2];
            [e2 setRenderPipelineState:init2];
            [e2 drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
            rect_draw(e2, acc2, -0.5f, -0.5f, 1.0f, 1.0f, 2);
            [e2 setRenderPipelineState:dump];
            [e2 setTileBuffer:out2 offset:0 atIndex:0];
            uint32_t wd = W; [e2 setTileBytes:&wd length:4 atIndex:1];
            [e2 dispatchThreadsPerTile:MTLSizeMake(TS, TS, 1)];
            [e2 endEncoding]; [cb2 commit]; [cb2 waitUntilCompleted];
            const uint32_t *o2 = out2.contents; int bad[5] = { 0 }, n = 0;
            for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
                const uint32_t *r = o2 + (y * W + x) * 8;
                int inRect = x >= 16 && x < 48 && y >= 12 && y < 36, k = inRect ? 2 : 0;   // two instances
                float f[4] = { x + k, y + k, 0.5f + k, 1.0f + k };
                for (int c = 0; c < 4; c++) { float got; memcpy(&got, &r[c], 4); if (got != f[c]) { bad[0]++; break; } }
                if ((int32_t)r[4] != x - 10 - 3 * k) bad[1]++;
                if (r[5] != (uint32_t)(y * 3 + 5 * k)) bad[2]++;
                if (r[6] != ((uint32_t)((x & 255) + k) | ((uint32_t)(y & 255) << 8) | (7u << 16) | (9u << 24))) bad[3]++;
                if (r[7] != (uint32_t)(((x + y) & 255) + 2 * k)) bad[4]++;
                n++;
            }
            arm("4 float4 member through fragment draws and a tile kernel", bad[0] == 0, 1, "%d of %d pixels off", bad[0], n);
            arm("5 int member (negative values, decrement)", bad[1] == 0, 1, "%d of %d pixels off", bad[1], n);
            arm("6 uint member", bad[2] == 0, 1, "%d of %d pixels off", bad[2], n);
            arm("7 uchar4 member", bad[3] == 0, 1, "%d of %d pixels off", bad[3], n);
            arm("8 uchar member", bad[4] == 0, 1, "%d of %d pixels off", bad[4], n);
        }
        printf("%d arms, %d unexpected\n", g_arms, g_unexpected);
        return g_unexpected ? 1 : 0;
    }
}
