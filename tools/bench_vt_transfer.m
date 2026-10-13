// bench_vt_transfer: time VideoToolbox's Metal pixel transfer (the scaler Photos and Preview use). Every transfer makes
// VT build its kernel again with new function constants, so this measures the driver's per-call cost: function creation,
// pipeline lookup, reflection, variant selection, dispatch, and the result check.
// usage: bench_vt_transfer [srcW srcH dstW dstH [iterations]]    default 4032 3024 1024 768 40
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <VideoToolbox/VideoToolbox.h>
#import <CoreVideo/CoreVideo.h>
#include <stdio.h>
#include <stdlib.h>

static double now_ms(void) { return clock_gettime_nsec_np(CLOCK_UPTIME_RAW) / 1e6; }

static CVPixelBufferRef make_buffer(int w, int h, int fill)
{
    CVPixelBufferRef pb = NULL;
    NSDictionary *attr = @{ (id)kCVPixelBufferIOSurfacePropertiesKey: @{}, (id)kCVPixelBufferMetalCompatibilityKey: @YES };
    if (CVPixelBufferCreate(kCFAllocatorDefault, w, h, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, (__bridge CFDictionaryRef)attr, &pb)) return NULL;
    CVPixelBufferLockBaseAddress(pb, 0);
    for (int p = 0; p < 2; p++) {
        uint8_t *base = CVPixelBufferGetBaseAddressOfPlane(pb, p); size_t row = CVPixelBufferGetBytesPerRowOfPlane(pb, p), ph = CVPixelBufferGetHeightOfPlane(pb, p), pw = CVPixelBufferGetWidthOfPlane(pb, p) * (p ? 2 : 1);
        for (size_t y = 0; y < ph; y++) for (size_t x = 0; x < pw; x++) base[y * row + x] = fill ? (uint8_t)(16 + ((x * 7 + y * 3 + p * 50) & 0x7f)) : 0;
    }
    CVPixelBufferUnlockBaseAddress(pb, 0);
    return pb;
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        int sw = argc > 4 ? atoi(argv[1]) : 4032, sh = argc > 4 ? atoi(argv[2]) : 3024, dw = argc > 4 ? atoi(argv[3]) : 1024, dh = argc > 4 ? atoi(argv[4]) : 768;
        int n = argc > 5 ? atoi(argv[5]) : 40;
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        printf("device %s\n", dev.name.UTF8String);
        CVPixelBufferRef src = make_buffer(sw, sh, 1);
        if (!src) { printf("cannot create source\n"); return 2; }
        double total = 0, first = 0, best = 1e9; int bad = 0; uint64_t sum = 1469598103934665603ull;
        for (int i = 0; i < n; i++) {
            CVPixelBufferRef dst = make_buffer(dw, dh, 0);
            VTPixelTransferSessionRef s = NULL;
            VTPixelTransferSessionCreate(kCFAllocatorDefault, &s);
            double t0 = now_ms();
            OSStatus st = VTPixelTransferSessionTransferImage(s, src, dst);
            double dt = now_ms() - t0;
            if (st) { printf("transfer %d failed %d\n", i, (int)st); bad++; }
            // the output must not hold a flat zero column: sample 64 columns of the luma plane
            CVPixelBufferLockBaseAddress(dst, kCVPixelBufferLock_ReadOnly);
            uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(dst, 0); size_t row = CVPixelBufferGetBytesPerRowOfPlane(dst, 0);
            int zero = 0; for (int c = 0; c < dw; c += dw / 64) { int nz = 0; for (int r = 0; r < dh; r += 8) nz |= y[(size_t)r * row + c] != 0; zero += !nz; }
            CVPixelBufferUnlockBaseAddress(dst, kCVPixelBufferLock_ReadOnly);
            if (zero) bad++;
            if (i == 0) {   // fingerprint of the whole first result (both planes), to compare two drivers byte for byte
                CVPixelBufferLockBaseAddress(dst, kCVPixelBufferLock_ReadOnly);
                for (int p = 0; p < 2; p++) { const uint8_t *b = CVPixelBufferGetBaseAddressOfPlane(dst, p); size_t r = CVPixelBufferGetBytesPerRowOfPlane(dst, p), ph = CVPixelBufferGetHeightOfPlane(dst, p), pw = CVPixelBufferGetWidthOfPlane(dst, p) * (p ? 2 : 1);
                    for (size_t yy = 0; yy < ph; yy++) for (size_t xx = 0; xx < pw; xx++) sum = (sum ^ b[yy * r + xx]) * 1099511628211ull; }
                CVPixelBufferUnlockBaseAddress(dst, kCVPixelBufferLock_ReadOnly);
            }
            if (i == 0) first = dt; else { total += dt; if (dt < best) best = dt; }
            VTPixelTransferSessionInvalidate(s); CFRelease(s); CFRelease(dst);
        }
        printf("%dx%d -> %dx%d  first %.1f ms  later avg %.1f ms  best %.1f ms  (%d runs, %d bad)  output fingerprint %016llx\n", sw, sh, dw, dh, first, n > 1 ? total / (n - 1) : 0, best, n, bad, (unsigned long long)sum);
        return bad ? 1 : 0;
    }
}
