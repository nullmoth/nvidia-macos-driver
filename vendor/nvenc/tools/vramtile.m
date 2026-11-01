// vramtile.m: research probe (VRAM capture path): the whole GPU-resident input path on today's driver, BGRA only.
// A Metal kernel reads a GPU-rendered BGRA IOSurface (its VRAM copy, imported by the Metal plugin) and writes NV12 in
// NVENC's input layout into a second IOSurface (also VRAM). Our RM client takes that surface's VRAM (escape 0xE2 +
// DUP) and NVENC encodes straight from it. Checks the bytes against a CPU reference with the same integer math and
// times the kernel.
//   ./vramtile W H out.h264 ref.nv12
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include "enc.h"
#include "blocklinear.h"
#include <time.h>
#include "nv_escape.h"
#include "class/cl50a0.h"

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

#define ESC_SURF_VRAM 0xE2u
typedef struct { uint32_t version, surfaceID, plane, hClient, hSrcClient, hSrcMemory, status, pad; uint64_t size; } SurfShareEsc;

static NSString *kSrc = @
"#include <metal_stdlib>\n using namespace metal;\n"
"kernel void pattern(texture2d<float, access::write> t [[texture(0)]], uint2 g [[thread_position_in_grid]]) {\n"
"  if (g.x < t.get_width() && g.y < t.get_height()) t.write(float4((g.x % 255) / 255.0, (g.y % 255) / 255.0, (((g.x >> 5) ^ (g.y >> 5)) & 1) ? 0.9 : 0.15, 1), g); }\n"
"struct P { uint pitch; uint width; uint rows; uint lumaRuns; uint dstW; };\n"
"static inline uint ylum(float4 c) { uint r = uint(c.r * 255 + 0.5), g = uint(c.g * 255 + 0.5), b = uint(c.b * 255 + 0.5); return 16 + ((47 * r + 157 * g + 16 * b + 128) >> 8); }\n"
// one thread per 16-byte run of the output (luma runs first, then chroma runs); BT.709 video range like nvenc_in_tile_bgra's scalar reference
"kernel void tile(texture2d<float, access::read> src [[texture(0)]], texture2d<uint, access::write> dst [[texture(1)]], constant P &p [[buffer(0)]],\n"
"                 uint gid [[thread_position_in_grid]]) {\n"
"    bool chroma = gid >= p.lumaRuns; uint o = (chroma ? gid - p.lumaRuns : gid) * 16, band = p.pitch * 16;\n"
"    uint k = o / band, rem = o % band, t = rem / 1024, in = rem % 1024;\n"
"    uint x = t * 64 + ((in >> 8) & 1) * 32 + ((in >> 6) & 1) * 16;\n"
"    uint y = k * 16 + (in >> 9) * 8 + ((in >> 7) & 1) * 4 + ((in >> 4) & 3);\n"
"    uint v[16];\n"
"    uint W = p.width, H = p.rows;\n"
"    for (uint i = 0; i < 16; i++) v[i] = 0;\n"
"    if (!chroma) { if (y < H) for (uint i = 0; i < 16; i++) if (x + i < W) v[i] = ylum(src.read(uint2(x + i, y))); }\n"
"    else if (2 * y < H) for (uint i = 0; i < 16; i += 2) if (x + i < W) {\n"
"        uint y1 = min(2 * y + 1, H - 1), xa = x + i, xb = min(x + i + 1, W - 1); int R = 0, G = 0, B = 0;\n"
"        float4 c[4] = { src.read(uint2(xa, 2 * y)), src.read(uint2(xb, 2 * y)), src.read(uint2(xa, y1)), src.read(uint2(xb, y1)) };\n"
"        for (int j = 0; j < 4; j++) { R += int(c[j].r * 255 + 0.5); G += int(c[j].g * 255 + 0.5); B += int(c[j].b * 255 + 0.5); }\n"
"        v[i] = uint(128 + ((-26 * R - 87 * G + 112 * B + 512) >> 10)); v[i + 1] = uint(128 + ((112 * R - 102 * G - 10 * B + 512) >> 10)); }\n"
"    uint4 w = uint4(v[0] | v[1] << 8 | v[2] << 16 | v[3] << 24, v[4] | v[5] << 8 | v[6] << 16 | v[7] << 24, v[8] | v[9] << 8 | v[10] << 16 | v[11] << 24, v[12] | v[13] << 8 | v[14] << 16 | v[15] << 24);\n"
"    uint li = (chroma ? p.lumaRuns * 16 : 0) / 16 + (chroma ? gid - p.lumaRuns : gid);\n"
"    dst.write(w, uint2(li % p.dstW, li / p.dstW));\n"
"}\n";

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 5) { fprintf(stderr, "usage: vramtile W H out.h264 ref.nv12\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]), CH = (H + 1) / 2;
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:kSrc options:nil error:&err];
    if (!lib) { printf("compile: %s\n", err.description.UTF8String); return 1; }
    id<MTLComputePipelineState> pp = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"pattern"] error:&err];
    id<MTLComputePipelineState> pt = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"tile"] error:&err];
    id<MTLCommandQueue> q = [dev newCommandQueue];

    // the encoder session tells us the input layout (pitch, plane sizes)
    Enc e; EncConfig cfg = { .width = W, .height = H, .qp_i = 20, .qp_p = 22, .num_refs = 1 };
    if (enc_open(&e, &cfg)) return 1;
    size_t total = e.lumaSz + e.chromaSz;

    // source: a BGRA surface the GPU renders (stands for a WindowServer frame)
    size_t bpr = ((size_t)W * 4 + 63) & ~(size_t)63;
    IOSurfaceRef src = IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(W), (id)kIOSurfaceHeight: @(H), (id)kIOSurfaceBytesPerElement: @4,
        (id)kIOSurfaceBytesPerRow: @(bpr), (id)kIOSurfacePixelFormat: @((uint32_t)'BGRA') });
    MTLTextureDescriptor *ts = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:W height:H mipmapped:NO];
    ts.usage = MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite;
    id<MTLTexture> tsrc = [dev newTextureWithDescriptor:ts iosurface:src plane:0];
    // destination: one-plane surface of 16-byte elements holding NVENC's layout (pitch bytes per row)
    uint32_t dstW = (uint32_t)(e.pitch / 16), dstH = (uint32_t)((total + e.pitch - 1) / e.pitch);
    IOSurfaceRef dst = IOSurfaceCreate((__bridge CFDictionaryRef)@{ (id)kIOSurfaceWidth: @(dstW), (id)kIOSurfaceHeight: @(dstH), (id)kIOSurfaceBytesPerElement: @16,
        (id)kIOSurfaceBytesPerRow: @(e.pitch), (id)kIOSurfacePixelFormat: @((uint32_t)'RGhA') });
    MTLTextureDescriptor *tdd = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA32Uint width:dstW height:dstH mipmapped:NO];
    tdd.usage = MTLTextureUsageShaderWrite;
    id<MTLTexture> tdst = [dev newTextureWithDescriptor:tdd iosurface:dst plane:0];
    if (!tsrc || !tdst) { printf("textures failed\n"); return 1; }

    id<MTLCommandBuffer> cb = [q commandBuffer]; id<MTLComputeCommandEncoder> ce = [cb computeCommandEncoder];
    [ce setComputePipelineState:pp]; [ce setTexture:tsrc atIndex:0];
    [ce dispatchThreads:MTLSizeMake(W, H, 1) threadsPerThreadgroup:MTLSizeMake(16, 16, 1)]; [ce endEncoding]; [cb commit]; [cb waitUntilCompleted];

    struct { uint32_t pitch, width, rows, lumaRuns, dstW; } p = { (uint32_t)e.pitch, (uint32_t)W, (uint32_t)H, (uint32_t)(e.lumaSz / 16), dstW };
    double gpu = 0, wall = 0; int N = 20;
    for (int i = 0; i < N; i++) {
        double t0 = now_ms();
        cb = [q commandBuffer]; ce = [cb computeCommandEncoder];
        [ce setComputePipelineState:pt]; [ce setTexture:tsrc atIndex:0]; [ce setTexture:tdst atIndex:1]; [ce setBytes:&p length:sizeof p atIndex:0];
        [ce dispatchThreads:MTLSizeMake(total / 16, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)]; [ce endEncoding];
        [cb commit]; [cb waitUntilCompleted];
        if (i >= 2) { gpu += (cb.GPUEndTime - cb.GPUStartTime) * 1000; wall += now_ms() - t0; }
    }
    printf("%dx%d BGRA (VRAM) -> NVENC-layout NV12 (VRAM) by Metal: %.3f ms GPU, %.3f ms wall (submit + wait)\n", W, H, gpu / (N - 2), wall / (N - 2));

    // our RM client takes the destination surface's VRAM
    SurfShareEsc s = { .version = 2, .surfaceID = IOSurfaceGetID(dst), .plane = 0, .hClient = e.rm.client };
    if (rm_esc(&e.rm, e.rm.ctl, ESC_SURF_VRAM, &s, sizeof s) || s.status) { printf("SURF_VRAM status 0x%x\n", s.status); return 1; }
    NVOS55_PARAMETERS d = { .hClient = e.rm.client, .hParent = e.rm.device, .hObject = rm_handle(&e.rm), .hClientSrc = s.hSrcClient, .hObjectSrc = s.hSrcMemory };
    if (rm_esc(&e.rm, e.rm.ctl, NV_ESC_RM_DUP_OBJECT, &d, sizeof d) || d.status) { printf("DUP status 0x%x\n", d.status); return 1; }
    NV_MEMORY_ALLOCATION_PARAMS v = { .owner = e.rm.client, .type = NVOS32_TYPE_IMAGE, .flags = NVOS32_ALLOC_FLAGS_VIRTUAL, .size = s.size, .hVASpace = e.rm.vaspace };
    NvHandle hv = rm_handle(&e.rm);
    if (rm_alloc(&e.rm, e.rm.device, hv, NV50_MEMORY_VIRTUAL, &v)) return 1;
    NVOS46_PARAMETERS m = { .hClient = e.rm.client, .hDevice = e.rm.device, .hDma = hv, .hMemory = d.hObject, .length = s.size,
                            .flags = DRF_DEF(OS46, _FLAGS, _PAGE_KIND, _VIRTUAL) | DRF_DEF(OS46, _FLAGS, _CACHE_SNOOP, _DISABLE) };
    if (rm_esc(&e.rm, e.rm.ctl, NV_ESC_RM_MAP_MEMORY_DMA, &m, sizeof m) || m.status) { printf("MAP_DMA status 0x%x\n", m.status); return 1; }
    printf("  destination VRAM in our client: %llu KB at GPU 0x%llx\n", (unsigned long long)(s.size >> 10), (unsigned long long)m.dmaOffset);

    // reference: same integer math on the CPU, tiled by the CPU tiler; compare through a BAR1 view
    IOSurfaceLock(src, kIOSurfaceLockReadOnly, NULL);
    const uint8_t *sp = IOSurfaceGetBaseAddress(src);
    uint8_t *ry = malloc((size_t)W * H), *ruv = malloc((size_t)W * CH);
    for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) { const uint8_t *x = sp + r * bpr + 4 * c; ry[r * W + c] = (uint8_t)(16 + ((47 * x[2] + 157 * x[1] + 16 * x[0] + 128) >> 8)); }
    for (int r = 0; r < CH; r++) for (int c = 0; c < W; c += 2) {
        int R = 0, G = 0, B = 0;
        for (int dy = 0; dy < 2; dy++) for (int dx = 0; dx < 2; dx++) { int rr = 2 * r + dy < H ? 2 * r + dy : H - 1, cc = c + dx < W ? c + dx : W - 1;
            const uint8_t *x = sp + rr * bpr + 4 * cc; R += x[2]; G += x[1]; B += x[0]; }
        ruv[r * W + c] = (uint8_t)(128 + ((-26 * R - 87 * G + 112 * B + 512) >> 10)); ruv[r * W + c + 1] = (uint8_t)(128 + ((112 * R - 102 * G - 10 * B + 512) >> 10));
    }
    IOSurfaceUnlock(src, kIOSurfaceLockReadOnly, NULL);
    FILE *f = fopen(argv[4], "wb"); fwrite(ry, 1, (size_t)W * H, f); fwrite(ruv, 1, (size_t)W * CH, f); fclose(f);
    uint8_t *ref = calloc(1, total);
    nvenc_in_tile_plane(ref, ry, W, W, H, e.pitch); nvenc_in_tile_plane(ref + e.lumaSz, ruv, W, W, CH, e.pitch);
    RmConn c; void *cpu = NULL;
    if (!rm_map_cpu(&e.rm, e.rm.device, d.hObject, s.size, false, &c, &cpu) && cpu) {
        size_t diff = 0;
        for (int r = 0; r < H; r++) for (int x = 0; x < W; x += 16) { size_t o = nvenc_in_offset(x, r, e.pitch); diff += memcmp((uint8_t *)cpu + o, ref + o, W - x < 16 ? W - x : 16) != 0; }
        for (int r = 0; r < CH; r++) for (int x = 0; x < W; x += 16) { size_t o = e.lumaSz + nvenc_in_offset(x, r, e.pitch); diff += memcmp((uint8_t *)cpu + o, ref + o, W - x < 16 ? W - x : 16) != 0; }
        printf("  VRAM vs CPU reference: %zu mismatching 16-byte runs\n", diff);
        rm_unmap_cpu(&e.rm, &c);
    }
    // NVENC encodes straight from the GPU-written VRAM
    e.extInput = m.dmaOffset;
    EncFrame fr; int rc = enc_encode(&e, true, &fr);
    printf("  NVENC from VRAM: %s, %.3f ms, %u bits\n", rc ? "FAILED" : "ok", fr.ms, fr.bits);
    if (!rc) { uint8_t hdr[256]; size_t hn = enc_headers(&e, hdr); f = fopen(argv[3], "wb"); fwrite(hdr, 1, hn, f); fwrite(fr.data, 1, fr.size, f); fclose(f); }
    enc_close(&e);
    return rc;
} }
