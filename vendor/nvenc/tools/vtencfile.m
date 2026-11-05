// vtencfile.m (M12, T12.05): encode synthetic frames through VTCompressionSession and write the samples to a file:
// AV1 (av01) as IVF with temporal delimiters put back (for dav1d / ffmpeg), H.264 / HEVC as Annex B (parameter sets first).
// Prints the encoder VideoToolbox picked and the format description's extensions; also writes the source frames.
//   ./vtencfile av01|hvc1|avc1 W H FRAMES out.ivf src.nv12      VT_BITRATE=<bit/s>  VT_10BIT=1 (x420 input)  VT_REALTIME=1
//   VT_444=1: 444v input (x444 with VT_10BIT; HEVC 10-bit asks for Main10), src written as planar yuv444p (M13)
//   VT_CBR=<bit/s>: ConstantBitRate  VT_MAXRATE=<bit/s>: DataRateLimits [rate / 8 bytes, 1 s] (VBR with AverageBitRate)
//   VT_FPS=<n>: frame rate (default 60: timestamps, ExpectedFrameRate, the rate budget per frame)
//   VT_HDR=pq|hlg: BT.2020 PQ (+ HDR metadata) / HLG colour properties
//   VT_REORDER=1: AllowFrameReordering (B pictures; checks decode / presentation time stamps)  VT_SRC=<file.nv12>: frames
//   from a raw NV12 file (looped) instead of the synthetic pattern  VT_KEYINT=<n>: MaxKeyFrameInterval
//   VT_PF=<fourcc>: grey frames in that format (what does VideoToolbox hand the encoder? NVENCVT_LOG=1 shows it)
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <VideoToolbox/VideoToolbox.h>
#include <math.h>
#include <time.h>
static double now(void) { return clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) / 1e9; }

static int W, H, N; static FILE *gOut; static int gFrames; static bool gTen, g444; static OSType gPf; static FILE *gSrcIn; static int gFps = 60;
static CMTime gLastDts; static int gBad, gReordered, gSync;

static void fill(CVPixelBufferRef pb, int f, FILE *src)
{
    CVPixelBufferLockBaseAddress(pb, 0);
    if (gPf) {   // VT_PF: mid-grey in any format
        if (CVPixelBufferIsPlanar(pb)) for (size_t p = 0; p < CVPixelBufferGetPlaneCount(pb); p++)
            memset(CVPixelBufferGetBaseAddressOfPlane(pb, p), 128, CVPixelBufferGetBytesPerRowOfPlane(pb, p) * CVPixelBufferGetHeightOfPlane(pb, p));
        else memset(CVPixelBufferGetBaseAddress(pb), 128, CVPixelBufferGetBytesPerRow(pb) * CVPixelBufferGetHeight(pb));
        CVPixelBufferUnlockBaseAddress(pb, 0); return;
    }
    uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(pb, 0), *uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
    size_t ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), uvs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
    if (gSrcIn) {   // VT_SRC: the next NV12 frame of the file (from the start again at its end); copied to src as it is
        uint8_t *buf = malloc((size_t)W * H * 3 / 2);
        if (fread(buf, 1, (size_t)W * H * 3 / 2, gSrcIn) != (size_t)W * H * 3 / 2) { rewind(gSrcIn); if (fread(buf, 1, (size_t)W * H * 3 / 2, gSrcIn)) {} }
        for (int r = 0; r < H; r++) {   // 10-bit buffers (x420): 8-bit samples widened, value << 8
            if (gTen) for (int c = 0; c < W; c++) ((uint16_t *)(y + r * ys))[c] = (uint16_t)(buf[(size_t)r * W + c] << 8);
            else memcpy(y + r * ys, buf + (size_t)r * W, (size_t)W);
        }
        for (int r = 0; r < H / 2; r++) {
            if (gTen) for (int c = 0; c < W; c++) ((uint16_t *)(uv + r * uvs))[c] = (uint16_t)(buf[(size_t)W * H + (size_t)r * W + c] << 8);
            else memcpy(uv + r * uvs, buf + (size_t)W * H + (size_t)r * W, (size_t)W);
        }
        fwrite(buf, 1, (size_t)W * H * 3 / 2, src); free(buf);
        CVPixelBufferUnlockBaseAddress(pb, 0); return;
    }
    uint8_t *row = malloc((size_t)W);
    for (int r = 0; r < H; r++) {
        for (int c = 0; c < W; c++) {
            int v = ((c + r + f * 8) & 255) / 2 + (((((c - f * 4) >> 6) ^ (r >> 6)) & 1) ? 90 : 0);
            if (abs(c - (f * 24) % W) < 16) v = 235;
            row[c] = (uint8_t)(16 + v * 219 / 255);
            if (gTen) ((uint16_t *)(y + r * ys))[c] = (uint16_t)(row[c] << 8); else y[r * ys + c] = row[c];
        }
        fwrite(row, 1, (size_t)W, src);
    }
    if (g444) {   // full-resolution Cb Cr interleaved; the source file gets the U plane, then the V plane
        uint8_t *u = malloc((size_t)W * H), *v = malloc((size_t)W * H);
        for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) {
            const uint8_t cb = (uint8_t)(128 + 60 * sin((c / 2 + f) * 0.02) + ((c ^ r) & 8 ? 6 : 0)), cr = (uint8_t)(128 + 60 * cos((r / 2 + f) * 0.03) - ((c + r) & 4 ? 5 : 0));
            u[(size_t)r * W + c] = cb; v[(size_t)r * W + c] = cr;
            if (gTen) { ((uint16_t *)(uv + r * uvs))[2 * c] = (uint16_t)(cb << 8); ((uint16_t *)(uv + r * uvs))[2 * c + 1] = (uint16_t)(cr << 8); }
            else { uv[r * uvs + 2 * c] = cb; uv[r * uvs + 2 * c + 1] = cr; }
        }
        fwrite(u, 1, (size_t)W * H, src); fwrite(v, 1, (size_t)W * H, src); free(u); free(v);
        free(row); CVPixelBufferUnlockBaseAddress(pb, 0); return;
    }
    for (int r = 0; r < H / 2; r++) {
        for (int c = 0; c < W; c++) {
            row[c] = (uint8_t)(c & 1 ? 128 + 60 * cos((r + f) * 0.03) : 128 + 60 * sin((c / 2 + f) * 0.02));
            if (gTen) ((uint16_t *)(uv + r * uvs))[c] = (uint16_t)(row[c] << 8); else uv[r * uvs + c] = row[c];
        }
        fwrite(row, 1, (size_t)W, src);
    }
    free(row);
    CVPixelBufferUnlockBaseAddress(pb, 0);
}

static void out_cb(void *ref, void *src, OSStatus st, VTEncodeInfoFlags fl, CMSampleBufferRef sb)
{
    if (st || !sb) { printf("frame error %d\n", (int)st); return; }
    {   // time stamps: decode times increase, no sample decodes after it is shown
        CMTime pts = CMSampleBufferGetPresentationTimeStamp(sb), dts = CMSampleBufferGetDecodeTimeStamp(sb);
        if (CMTIME_IS_NUMERIC(dts)) {
            if (CMTimeCompare(dts, pts) > 0 || (gFrames && CMTimeCompare(dts, gLastDts) <= 0)) { gBad++; if (gBad <= 5) printf("bad time stamps: sample %d pts %lld/%d dts %lld/%d\n", gFrames, pts.value, pts.timescale, dts.value, dts.timescale); }
            if (CMTimeCompare(dts, pts) < 0 && gFrames) gReordered++;
            gLastDts = dts;
        }
        CFArrayRef a = CMSampleBufferGetSampleAttachmentsArray(sb, false);
        CFBooleanRef ns = a && CFArrayGetCount(a) ? CFDictionaryGetValue(CFArrayGetValueAtIndex(a, 0), kCMSampleAttachmentKey_NotSync) : NULL;
        if (!ns || !CFBooleanGetValue(ns)) gSync++;
        if (getenv("VT_TS")) printf("sample %d: pts %lld dts %lld size %zu%s\n", gFrames, pts.value, CMTIME_IS_NUMERIC(dts) ? dts.value : -999, CMSampleBufferGetTotalSampleSize(sb), !ns || !CFBooleanGetValue(ns) ? " sync" : "");
    }
    CMBlockBufferRef bb = CMSampleBufferGetDataBuffer(sb); size_t n = CMBlockBufferGetDataLength(bb);
    uint8_t *d = malloc(n + 2); CMBlockBufferCopyDataBytes(bb, 0, n, d + 2);
    CMVideoCodecType c = CMFormatDescriptionGetMediaSubType(CMSampleBufferGetFormatDescription(sb));
    if (c == 'av01') {   // IVF frame: the temporal delimiter OBU, then the sample
        d[0] = 0x12; d[1] = 0; uint32_t sz = (uint32_t)n + 2; uint64_t pts = (uint64_t)gFrames;
        fwrite(&sz, 4, 1, gOut); fwrite(&pts, 8, 1, gOut); fwrite(d, 1, sz, gOut);
    } else {   // Annex B: the format description's parameter sets before the first sample, start codes for the 4-byte lengths
        static const uint8_t sc[4] = { 0, 0, 0, 1 };
        CMFormatDescriptionRef fd = CMSampleBufferGetFormatDescription(sb);
        for (size_t i = 0, cnt = 1; gFrames == 0 && i < cnt; i++) {
            const uint8_t *ps; size_t pl;
            if ((c == 'hvc1' ? CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fd, i, &ps, &pl, &cnt, NULL)
                             : CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fd, i, &ps, &pl, &cnt, NULL))) break;
            fwrite(sc, 1, 4, gOut); fwrite(ps, 1, pl, gOut);
        }
        for (size_t o = 2; o + 4 <= n + 2;) {
            const size_t l = (size_t)d[o] << 24 | (size_t)d[o + 1] << 16 | (size_t)d[o + 2] << 8 | d[o + 3];
            fwrite(sc, 1, 4, gOut); fwrite(d + o + 4, 1, l, gOut); o += 4 + l;
        }
    }
    if (gFrames == 0) {
        CFDictionaryRef ext = CMFormatDescriptionGetExtensions(CMSampleBufferGetFormatDescription(sb));
        double t = now(); fprintf(stderr, "format description extensions: %s\n", [[(__bridge NSDictionary *)ext description] UTF8String]);
        if (now() - t > 0.1) printf("printing the extensions took %.1f s\n", now() - t);
    }
    gFrames++; free(d);
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        if (argc < 7) { fprintf(stderr, "usage: vtencfile av01|hvc1|avc1 W H FRAMES out src.nv12\n"); return 2; }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); (void)dev;   // the driver's plugin registers encoders then
        const char *cs = argv[1]; CMVideoCodecType codec = (CMVideoCodecType)((uint32_t)cs[0] << 24 | (uint32_t)cs[1] << 16 | (uint32_t)cs[2] << 8 | (uint32_t)cs[3]);
        if (getenv("VT_FPS")) gFps = atoi(getenv("VT_FPS"));
        W = atoi(argv[2]); H = atoi(argv[3]); N = atoi(argv[4]); gTen = getenv("VT_10BIT") != NULL; g444 = getenv("VT_444") != NULL;
        if (getenv("VT_PF") && strlen(getenv("VT_PF")) == 4) { const char *q = getenv("VT_PF"); gPf = (OSType)q[0] << 24 | (OSType)q[1] << 16 | (OSType)q[2] << 8 | (OSType)q[3]; }
        gOut = fopen(argv[5], "wb"); FILE *src = fopen(argv[6], "wb");
        if (codec == 'av01') { uint8_t ih[32] = "DKIF"; ih[6] = 32; memcpy(ih + 8, "AV01", 4); ih[12] = W; ih[13] = W >> 8; ih[14] = H; ih[15] = H >> 8; ih[16] = (uint8_t)gFps; ih[20] = 1; ih[24] = N; fwrite(ih, 1, 32, gOut); }
        NSDictionary *spec = @{ (id)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: @YES };
        OSType pf = gTen ? kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
        if (g444) pf = gTen ? kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange : kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange;
        if (gPf) pf = gPf;
        NSDictionary *pbAttr = @{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf), (id)kCVPixelBufferWidthKey: @(W), (id)kCVPixelBufferHeightKey: @(H),
                                  (id)kCVPixelBufferIOSurfacePropertiesKey: @{} };
        VTCompressionSessionRef enc = NULL;
        OSStatus st = VTCompressionSessionCreate(NULL, W, H, codec, (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)pbAttr, NULL, out_cb, NULL, &enc);
        if (st) { printf("VTCompressionSessionCreate -> %d\n", (int)st); return 1; }
        CFStringRef eid = NULL; VTSessionCopyProperty(enc, kVTCompressionPropertyKey_EncoderID, NULL, &eid);
        printf("encoder: %s\n", eid ? CFStringGetCStringPtr(eid, kCFStringEncodingUTF8) : "?");
        if (getenv("VT_BITRATE")) VTSessionSetProperty(enc, kVTCompressionPropertyKey_AverageBitRate, (__bridge CFNumberRef)@(atoi(getenv("VT_BITRATE"))));
        if (getenv("VT_CBR")) printf("ConstantBitRate -> %d\n", (int)VTSessionSetProperty(enc, kVTCompressionPropertyKey_ConstantBitRate, (__bridge CFNumberRef)@(atoi(getenv("VT_CBR")))));
        if (getenv("VT_MAXRATE")) printf("DataRateLimits -> %d\n", (int)VTSessionSetProperty(enc, kVTCompressionPropertyKey_DataRateLimits, (__bridge CFArrayRef)@[ @(atoi(getenv("VT_MAXRATE")) / 8), @1 ]));
        if (gTen && codec == 'hvc1') VTSessionSetProperty(enc, kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main10_AutoLevel);
        if (getenv("VT_HDR")) {   // VT_HDR=pq|hlg: BT.2020 with that transfer, plus mastering display / content light level for pq
            const bool pq = !strcmp(getenv("VT_HDR"), "pq");
            VTSessionSetProperty(enc, kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_2020);
            VTSessionSetProperty(enc, kVTCompressionPropertyKey_TransferFunction, pq ? kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ : kCVImageBufferTransferFunction_ITU_R_2100_HLG);
            VTSessionSetProperty(enc, kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_2020);
            if (pq) {
                static const uint8_t m[24] = { 0x33, 0xc2, 0x86, 0xc4, 0x1d, 0x4c, 0x0b, 0xb8, 0x84, 0xd0, 0x3e, 0x80, 0x3d, 0x13, 0x40, 0x42, 0x00, 0x98, 0x96, 0x80, 0x00, 0x00, 0x00, 0x32 }, c[4] = { 0x03, 0xe8, 0x01, 0x90 };
                CFDataRef md = CFDataCreate(NULL, m, 24), cd = CFDataCreate(NULL, c, 4);
                printf("MasteringDisplayColorVolume -> %d, ContentLightLevelInfo -> %d\n", (int)VTSessionSetProperty(enc, kVTCompressionPropertyKey_MasteringDisplayColorVolume, md),
                       (int)VTSessionSetProperty(enc, kVTCompressionPropertyKey_ContentLightLevelInfo, cd));
                CFRelease(md); CFRelease(cd);
            }
        }
        if (getenv("VT_REORDER")) VTSessionSetProperty(enc, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanTrue);
        else VTSessionSetProperty(enc, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
        if (getenv("VT_KEYINT")) VTSessionSetProperty(enc, kVTCompressionPropertyKey_MaxKeyFrameInterval, (__bridge CFNumberRef)@(atoi(getenv("VT_KEYINT"))));
        if (getenv("VT_SRC")) gSrcIn = fopen(getenv("VT_SRC"), "rb");
        if (getenv("VT_REALTIME")) VTSessionSetProperty(enc, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
        VTSessionSetProperty(enc, kVTCompressionPropertyKey_ExpectedFrameRate, (__bridge CFNumberRef)@(gFps));
        CVPixelBufferPoolRef pool = VTCompressionSessionGetPixelBufferPool(enc);
        double t0 = now();
        for (int f = 0; f < N; f++) {
            CVPixelBufferRef pb = NULL;
            if (g444 || gPf) CVPixelBufferCreate(NULL, W, H, pf, (__bridge CFDictionaryRef)pbAttr, &pb);   // the pool has the encoder's first format
            else CVPixelBufferPoolCreatePixelBuffer(NULL, pool, &pb);
            fill(pb, f, src);
            st = VTCompressionSessionEncodeFrame(enc, pb, CMTimeMake(f, gFps), CMTimeMake(1, gFps), NULL, NULL, NULL);
            CVPixelBufferRelease(pb);
            if (st) { printf("EncodeFrame %d -> %d\n", f, (int)st); break; }
        }
        VTCompressionSessionCompleteFrames(enc, kCMTimeInvalid);
        double ms = (now() - t0) * 1000;
        VTCompressionSessionInvalidate(enc); CFRelease(enc);
        fclose(gOut); fclose(src);
        printf("RESULT %d of %d frames in %.1f ms (%.0f fps), %d reordered, %d sync, %d bad time stamps\n", gFrames, N, ms, gFrames * 1000.0 / ms, gReordered, gSync, gBad);
    }
    return 0;
}
