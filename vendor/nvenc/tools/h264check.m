// h264check.m: decode an Annex B H.264 or HEVC stream (told apart by the first NAL unit) with Apple's software decoder and compare every picture with the source
// NV12 frames (tightly packed, concatenated). Lists the NAL units first (only the first 12 unless -v).
//   ./h264check [-v] W H in.h264 src.nv12 [dump.pgm]      dump.pgm: luma of the last decoded picture
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#include <CommonCrypto/CommonDigest.h>
#include <math.h>

int main(int argc, char **argv) { @autoreleasepool {
    int ai = 1; bool verbose = false;
    if (argc > 1 && !strcmp(argv[1], "-v")) { verbose = true; ai++; }
    if (argc - ai < 4) { fprintf(stderr, "usage: h264check [-v] W H in.h264 src.nv12 [dump.pgm]\n"); return 2; }
    int W = atoi(argv[ai]), H = atoi(argv[ai + 1]), CH = (H + 1) / 2;
    NSData *es = [NSData dataWithContentsOfFile:@(argv[ai + 2])], *src = [NSData dataWithContentsOfFile:@(argv[ai + 3])];
    const char *dump = argc - ai > 4 ? argv[ai + 4] : NULL;
    if (!es || !src) { printf("cannot read inputs\n"); return 1; }
    size_t frameSz = (size_t)W * H + (size_t)W * CH, nSrc = src.length / frameSz;
    const uint8_t *p = es.bytes; size_t n = es.length;
    // split on start codes
    NSMutableArray *nals = [NSMutableArray array];
    size_t i = 0, start = SIZE_MAX;
    while (i + 3 <= n) {
        if (p[i] == 0 && p[i + 1] == 0 && (p[i + 2] == 1 || (i + 3 < n && p[i + 2] == 0 && p[i + 3] == 1))) {
            size_t sc = p[i + 2] == 1 ? 3 : 4;
            if (start != SIZE_MAX) { size_t e = i; while (e > start && p[e - 1] == 0) e--; [nals addObject:[NSData dataWithBytes:p + start length:e - start]]; }
            i += sc; start = i;
        } else i++;
    }
    if (start != SIZE_MAX && start < n) [nals addObject:[NSData dataWithBytes:p + start length:n - start]];
    // one sample per picture: a slice that starts a picture (H.264 first_mb_in_slice == 0, HEVC
    // first_slice_segment_in_pic_flag: the first bit after the NAL header is 1) starts a new one
    bool hevc = nals.count && ((((const uint8_t *)[nals[0] bytes])[0] >> 1) & 0x3f) == 32;   // starts with a VPS
    NSData *vps = nil, *sps = nil, *pps = nil; NSMutableArray *samples = [NSMutableArray array]; int shown = 0;
    for (NSData *nal in nals) {
        const uint8_t *b = nal.bytes; int t = hevc ? (b[0] >> 1) & 0x3f : b[0] & 0x1f, hl = hevc ? 2 : 1;
        if (verbose || shown++ < 12) printf("NAL type %2d, %6lu bytes, header 0x%02x\n", t, (unsigned long)nal.length, b[0]);
        bool slice = hevc ? t < 32 : (t == 1 || t == 5);
        if (hevc ? t == 32 : false) vps = nal;
        else if (t == (hevc ? 33 : 7)) sps = nal; else if (t == (hevc ? 34 : 8)) pps = nal;
        else if (slice) {
            if (!samples.count || ((int)nal.length > hl && (b[hl] & 0x80))) [samples addObject:[NSMutableData data]];
            uint32_t be = CFSwapInt32HostToBig((uint32_t)nal.length);
            [samples.lastObject appendBytes:&be length:4]; [samples.lastObject appendData:nal];
        }
    }
    if (!sps || !pps || (hevc && !vps) || !samples.count) { printf("missing parameter sets/slices\n"); return 1; }
    CMVideoFormatDescriptionRef fmt = NULL; OSStatus st;
    if (hevc) {
        const uint8_t *ps[3] = { vps.bytes, sps.bytes, pps.bytes }; size_t pl[3] = { vps.length, sps.length, pps.length };
        st = CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL, 3, ps, pl, 4, NULL, &fmt);
    } else {
        const uint8_t *ps[2] = { sps.bytes, pps.bytes }; size_t pl[2] = { sps.length, pps.length };
        st = CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, ps, pl, 4, &fmt);
    }
    if (st) { printf("format description from the parameter sets -> %d\n", (int)st); return 1; }
    CMVideoDimensions d = CMVideoFormatDescriptionGetDimensions(fmt);
    printf("SPS says %dx%d, %lu picture(s), %zu source frame(s)\n", d.width, d.height, (unsigned long)samples.count, nSrc);
    NSDictionary *spec = @{ (__bridge NSString *)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: @NO };
    // FULLRANGE=1: full-range output (420f), for streams from full-range input (Parsec): else the decoder converts to video range
    NSDictionary *attr = @{ (__bridge NSString *)kCVPixelBufferPixelFormatTypeKey: @(getenv("FULLRANGE") ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) };
    VTDecompressionSessionRef dec = NULL;
    st = VTDecompressionSessionCreate(NULL, fmt, (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)attr, NULL, &dec);
    if (st) { printf("decoder create -> %d\n", (int)st); return 1; }
    if (getenv("LAG")) {   // output lag: feed pictures one by one without draining, count how many the decoder holds back
        __block int outs = 0; int maxLag = 0;
        for (NSUInteger k = 0; k < samples.count; k++) {
            NSData *sample = samples[k]; CMBlockBufferRef bb = NULL;
            CMBlockBufferCreateWithMemoryBlock(NULL, (void *)sample.bytes, sample.length, kCFAllocatorNull, NULL, 0, sample.length, 0, &bb);
            CMSampleBufferRef sb = NULL; size_t ss = sample.length;
            CMSampleTimingInfo ti = { .duration = CMTimeMake(1, 60), .presentationTimeStamp = CMTimeMake((int64_t)k, 60), .decodeTimeStamp = kCMTimeInvalid };
            CMSampleBufferCreateReady(NULL, bb, fmt, 1, 1, &ti, 1, &ss, &sb);
            VTDecompressionSessionDecodeFrameWithOutputHandler(dec, sb, 0, NULL, ^(OSStatus s, VTDecodeInfoFlags fl, CVImageBufferRef img, CMTime pts, CMTime dur) { if (img) outs++; });
            CFRelease(sb); CFRelease(bb);
            int lag = (int)k + 1 - outs; if (lag > maxLag) maxLag = lag;
        }
        VTDecompressionSessionWaitForAsynchronousFrames(dec);
        printf("RESULT output lag: up to %d picture(s) held back by the decoder (%d of %lu output in the end)\n", maxLag, outs, (unsigned long)samples.count);
        return 0;
    }
    double sumY = 0, sumC = 0, minY = 1e9, minC = 1e9; int decoded = 0, errors = 0;
    for (NSUInteger k = 0; k < samples.count; k++) {
        NSData *sample = samples[k];
        CMBlockBufferRef bb = NULL;
        CMBlockBufferCreateWithMemoryBlock(NULL, (void *)sample.bytes, sample.length, kCFAllocatorNull, NULL, 0, sample.length, 0, &bb);
        CMSampleBufferRef sb = NULL; size_t ss = sample.length;
        CMSampleTimingInfo ti = { .duration = CMTimeMake(1, 60), .presentationTimeStamp = CMTimeMake((int64_t)k, 60), .decodeTimeStamp = kCMTimeInvalid };
        CMSampleBufferCreateReady(NULL, bb, fmt, 1, 1, &ti, 1, &ss, &sb);
        __block CVPixelBufferRef out = NULL; __block OSStatus ds = 0;
        VTDecompressionSessionDecodeFrameWithOutputHandler(dec, sb, 0, NULL, ^(OSStatus s, VTDecodeInfoFlags fl, CVImageBufferRef img, CMTime pts, CMTime dur) {
            ds = s; if (img) out = CVPixelBufferRetain(img); });
        VTDecompressionSessionWaitForAsynchronousFrames(dec);
        CFRelease(sb); CFRelease(bb);
        if (ds || !out) { printf("picture %lu: decode failed, status %d\n", (unsigned long)k, (int)ds); errors++; continue; }
        decoded++;
        if (getenv("DECODED_MD5")) {   // MD5 of each decoded picture as tightly packed NV12 (as ffmpeg -f framemd5 -pix_fmt nv12)
            CC_MD5_CTX m; CC_MD5_Init(&m); unsigned char dg[16];
            CVPixelBufferLockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
            for (int pl2 = 0; pl2 < 2; pl2++) {
                const uint8_t *o = CVPixelBufferGetBaseAddressOfPlane(out, pl2); size_t os = CVPixelBufferGetBytesPerRowOfPlane(out, pl2);
                for (int r = 0; r < (pl2 ? CH : H); r++) CC_MD5_Update(&m, o + r * os, (CC_LONG)W);
            }
            CVPixelBufferUnlockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
            CC_MD5_Final(dg, &m);
            printf("md5 %lu ", (unsigned long)k); for (int i = 0; i < 16; i++) printf("%02x", dg[i]); printf("\n");
        }
        if (getenv("DECODED_NV12")) {   // every decoded picture, tightly packed NV12, appended in decode order
            static FILE *df; if (!df) df = fopen(getenv("DECODED_NV12"), "wb");
            CVPixelBufferLockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
            for (int pl2 = 0; pl2 < 2 && df; pl2++) {
                const uint8_t *o = CVPixelBufferGetBaseAddressOfPlane(out, pl2); size_t os = CVPixelBufferGetBytesPerRowOfPlane(out, pl2);
                for (int r = 0; r < (pl2 ? CH : H); r++) fwrite(o + r * os, 1, W, df);
            }
            CVPixelBufferUnlockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
        }
        if (k < nSrc) {
            CVPixelBufferLockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
            const uint8_t *sy = (const uint8_t *)src.bytes + k * frameSz, *suv = sy + (size_t)W * H;
            double se[2] = { 0, 0 }; size_t cnt[2] = { 0, 0 };
            for (int pl2 = 0; pl2 < 2; pl2++) {
                const uint8_t *o = CVPixelBufferGetBaseAddressOfPlane(out, pl2); size_t os = CVPixelBufferGetBytesPerRowOfPlane(out, pl2);
                const uint8_t *s = pl2 ? suv : sy; int rows = pl2 ? CH : H;
                for (int r = 0; r < rows; r++) for (int c = 0; c < W; c++) { double df = (double)o[r * os + c] - s[r * W + c]; se[pl2] += df * df; cnt[pl2]++; }
            }
            double py = 10 * log10(255.0 * 255.0 / (se[0] / cnt[0] + 1e-9)), pc = 10 * log10(255.0 * 255.0 / (se[1] / cnt[1] + 1e-9));
            sumY += py; sumC += pc; if (py < minY) minY = py; if (pc < minC) minC = pc;
            if (verbose || samples.count <= 8) printf("picture %lu: %lu bytes, luma %.2f dB, chroma %.2f dB\n", (unsigned long)k, (unsigned long)sample.length, py, pc);
            if (dump && k + 1 == samples.count) {
                FILE *f = fopen(dump, "wb"); fprintf(f, "P5 %d %d 255\n", W, H);
                const uint8_t *o = CVPixelBufferGetBaseAddressOfPlane(out, 0); size_t os = CVPixelBufferGetBytesPerRowOfPlane(out, 0);
                for (int r = 0; r < H; r++) fwrite(o + r * os, 1, W, f); fclose(f);
            }
            CVPixelBufferUnlockBaseAddress(out, kCVPixelBufferLock_ReadOnly);
        }
        CVPixelBufferRelease(out);
    }
    int m = decoded < (int)nSrc ? decoded : (int)nSrc;
    printf("RESULT decoded %dx%d, luma PSNR %.2f dB, chroma PSNR %.2f dB (average of %d), min luma %.2f, min chroma %.2f, %d decode errors\n",
           d.width, d.height, m ? sumY / m : 0, m ? sumC / m : 0, m, m ? minY : 0, m ? minC : 0, errors);
    return errors ? 1 : 0;
} }
