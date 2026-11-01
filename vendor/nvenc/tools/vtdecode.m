// vtdecode.m: is there a hardware H.264 / HEVC decoder in this process, and does it decode correctly? Creates a Metal
// device first (the driver's plugin registers its decoders when a process does), then decodes every frame of a movie
// twice through VTDecompressionSession: once with a hardware decoder required, once with the software decoder, and
// compares the luma (PSNR). Prints which decoder ran and how fast.
//   ./vtdecode in.mp4 [frames]
//   VTDECODE_REF=ref.yuv: compare the hardware decoder with a reference decode instead (ffmpeg, I420 / yuv420p10le)
//   VTDECODE_SW=1: with VTDECODE_REF, compare the software decoder (Apple's) instead
//   VTDECODE_AVREAD=1: decode through AVAssetReader (alwaysCopiesSampleData NO) instead, compared with VTDECODE_REF
//   VTDECODE_PF=x420: ask for that output format (default 420v)
//   VTDECODE_WH=1 (or WxH): ask for the picture size too (or another size); VTDECODE_GL=1: OpenGL-compatible IOSurfaces (ffmpeg)
//   VTDECODE_ATT=1: print the first decoded frame's attachments (colour, HDR metadata)
//   VTDECODE_SPEED=hw|sw: only the decode speed of the hardware / software decoder (frames released at once)
//   VTDECODE_ASYNC=1: allow asynchronous decompression (as AVFoundation does)
//   VTDECODE_SUPP=1: opt in to VP9 first (VTRegisterSupplementalVideoDecoderIfAvailable), as WebKit and ffmpeg do
// Build: clang -fobjc-arc -O2 vtdecode.m -o vtdecode -framework Foundation -framework AVFoundation -framework CoreMedia
//        -framework CoreVideo -framework VideoToolbox -framework Metal
#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <Metal/Metal.h>
#include <math.h>

typedef struct { int n; NSMutableArray *frames, *pts; double ms; OSStatus err; } Out;

static bool gNoKeep;
static void cb(void *ref, void *src, OSStatus st, VTDecodeInfoFlags f, CVImageBufferRef img, CMTime pts, CMTime dur)
{
    Out *o = ref;
    if (st) { o->err = st; return; }
    if (img && !o->n && getenv("VTDECODE_ATT")) { CFDictionaryRef a = CVBufferCopyAttachments(img, kCVAttachmentMode_ShouldPropagate); NSLog(@"first frame attachments: %@", a); if (a) CFRelease(a); }
    if (img && gNoKeep) { o->n++; return; }   // VTDECODE_SPEED: count only
    if (img) { [o->frames addObject:(__bridge id)img]; [o->pts addObject:@(CMTimeGetSeconds(pts))]; o->n++; }
}

static int decode(NSArray *samples, CMFormatDescriptionRef fd, bool hw, Out *o, bool *usedHW)
{
    NSDictionary *spec = hw ? @{ (id)kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: @YES }
                            : @{ (id)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: @NO };
    const char *pfs = getenv("VTDECODE_PF");   // the output format to ask for (four characters, e.g. x420); default NV12 420v
    const OSType pf = pfs && strlen(pfs) == 4 ? (OSType)pfs[0] << 24 | (OSType)pfs[1] << 16 | (OSType)pfs[2] << 8 | (OSType)pfs[3] : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    NSMutableDictionary *attrs = [@{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf) } mutableCopy];
    if (getenv("VTDECODE_WH")) {   // also the picture size, as ffmpeg and AVFoundation ask (VTDECODE_WH=WxH: another size)
        CMVideoDimensions d = CMVideoFormatDescriptionGetDimensions(fd); int w = d.width, h = d.height;
        if (strchr(getenv("VTDECODE_WH"), 'x')) sscanf(getenv("VTDECODE_WH"), "%dx%d", &w, &h);
        attrs[(id)kCVPixelBufferWidthKey] = @(w); attrs[(id)kCVPixelBufferHeightKey] = @(h);
    }
    if (getenv("VTDECODE_GL")) {   // OpenGL-compatible IOSurfaces, as ffmpeg's videotoolbox hwaccel asks
        attrs[(id)kCVPixelBufferIOSurfacePropertiesKey] = @{};
        attrs[(id)kCVPixelBufferIOSurfaceOpenGLTextureCompatibilityKey] = @YES;
    }
    VTDecompressionOutputCallbackRecord rec = { cb, o };
    VTDecompressionSessionRef s = NULL;
    OSStatus st = VTDecompressionSessionCreate(NULL, fd, (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)attrs, &rec, &s);
    if (st) { printf("  %s decoder: VTDecompressionSessionCreate -> %d\n", hw ? "hardware" : "software", (int)st); return -1; }
    CFBooleanRef b = NULL;
    VTSessionCopyProperty(s, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &b);
    *usedHW = b == kCFBooleanTrue; if (b) CFRelease(b);
    o->frames = [NSMutableArray array]; o->pts = [NSMutableArray array];
    uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
    for (id x in samples) {
        VTDecodeFrameFlags fl = getenv("VTDECODE_ASYNC") ? kVTDecodeFrame_EnableAsynchronousDecompression : 0; VTDecodeInfoFlags info = 0;
        st = VTDecompressionSessionDecodeFrame(s, (__bridge CMSampleBufferRef)x, fl, NULL, &info);
        if (st) { o->err = st; break; }
    }
    VTDecompressionSessionWaitForAsynchronousFrames(s);
    o->ms = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6;
    VTDecompressionSessionInvalidate(s); CFRelease(s);
    return 0;
}

static double psnr(CVPixelBufferRef a, CVPixelBufferRef b)
{
    CVPixelBufferLockBaseAddress(a, kCVPixelBufferLock_ReadOnly); CVPixelBufferLockBaseAddress(b, kCVPixelBufferLock_ReadOnly);
    size_t w = CVPixelBufferGetWidthOfPlane(a, 0), h = CVPixelBufferGetHeightOfPlane(a, 0);
    size_t ra = CVPixelBufferGetBytesPerRowOfPlane(a, 0), rb = CVPixelBufferGetBytesPerRowOfPlane(b, 0);
    const uint8_t *pa = CVPixelBufferGetBaseAddressOfPlane(a, 0), *pb = CVPixelBufferGetBaseAddressOfPlane(b, 0);
    double se = 0;
    for (size_t y = 0; y < h; y++) for (size_t x = 0; x < w; x++) { int d = pa[y * ra + x] - pb[y * rb + x]; se += d * d; }
    CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly); CVPixelBufferUnlockBaseAddress(b, kCVPixelBufferLock_ReadOnly);
    double mse = se / (w * h);
    return mse == 0 ? 99.0 : 10 * log10(255.0 * 255.0 / mse);
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        if (argc < 2) { printf("usage: vtdecode in.mp4 [frames]\n"); return 2; }
        int maxf = argc > 2 ? atoi(argv[2]) : 600;
        if (getenv("VTDECODE_SUPP")) {   // VP9 / AV1: VideoToolbox offers these only after the process opts in (as WebKit and ffmpeg do)
            extern void VTRegisterSupplementalVideoDecoderIfAvailable(CMVideoCodecType) __attribute__((weak_import));
            if (VTRegisterSupplementalVideoDecoderIfAvailable) { VTRegisterSupplementalVideoDecoderIfAvailable('vp09'); printf("supplemental VP9 decoder requested\n"); }
        }
        id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
        printf("Metal device: %s\n", dev ? dev.name.UTF8String : "none");
        AVAsset *as = [AVAsset assetWithURL:[NSURL fileURLWithPath:@(argv[1])]];
        AVAssetTrack *tr = [as tracksWithMediaType:AVMediaTypeVideo].firstObject;
        if (!tr) { printf("no video track\n"); return 1; }
        NSError *e = nil; AVAssetReader *rd = [AVAssetReader assetReaderWithAsset:as error:&e];
        AVAssetReaderTrackOutput *out = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:tr outputSettings:nil];
        [rd addOutput:out]; [rd startReading];
        NSMutableArray *samples = [NSMutableArray array]; CMFormatDescriptionRef fd = NULL;
        CMSampleBufferRef sb;
        while ((int)samples.count < maxf && (sb = [out copyNextSampleBuffer])) {
            if (CMSampleBufferGetNumSamples(sb) > 0) { if (!fd) fd = (CMFormatDescriptionRef)CFRetain(CMSampleBufferGetFormatDescription(sb)); [samples addObject:(__bridge_transfer id)sb]; }
            else CFRelease(sb);
        }
        if (!fd) { printf("no samples\n"); return 1; }
        FourCharCode c = CMFormatDescriptionGetMediaSubType(fd); CMVideoDimensions d = CMVideoFormatDescriptionGetDimensions(fd);
        printf("stream: %c%c%c%c %dx%d, %lu frames; VTIsHardwareDecodeSupported: %d\n", c >> 24, (c >> 16) & 255, (c >> 8) & 255, c & 255,
               d.width, d.height, (unsigned long)samples.count, VTIsHardwareDecodeSupported(c));
        if (getenv("VTDECODE_AVREAD")) {   // decode through AVAssetReader instead (what AVFoundation apps get), compare with VTDECODE_REF
            const char *pfs = getenv("VTDECODE_PF");
            const OSType pf = pfs && strlen(pfs) == 4 ? (OSType)pfs[0] << 24 | (OSType)pfs[1] << 16 | (OSType)pfs[2] << 8 | (OSType)pfs[3] : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
            AVAssetReader *r2 = [AVAssetReader assetReaderWithAsset:as error:&e];
            AVAssetReaderTrackOutput *o2 = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:tr outputSettings:@{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf) }];
            o2.alwaysCopiesSampleData = NO;
            [r2 addOutput:o2];
            if (![r2 startReading]) { printf("RESULT avread failed to start: %s\n", r2.error.description.UTF8String); return 1; }
            FILE *rf = getenv("VTDECODE_REF") ? fopen(getenv("VTDECODE_REF"), "rb") : NULL;
            int n = 0, exact = 0; CMSampleBufferRef s2; const uint64_t t0 = clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW);
            while ((s2 = [o2 copyNextSampleBuffer])) {
                CVPixelBufferRef a = CMSampleBufferGetImageBuffer(s2);
                if (a && rf) {
                    CVPixelBufferLockBaseAddress(a, kCVPixelBufferLock_ReadOnly);
                    const size_t w = CVPixelBufferGetWidth(a), hh = CVPixelBufferGetHeight(a), cw = (w + 1) / 2, ch = (hh + 1) / 2;
                    const int b = pf == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange ? 2 : 1;
                    const size_t ns = w * hh + 2 * cw * ch; uint8_t *ref = malloc(ns * b); bool same = fread(ref, b, ns, rf) == ns;
                    const uint8_t *Y = CVPixelBufferGetBaseAddressOfPlane(a, 0), *C = CVPixelBufferGetBaseAddressOfPlane(a, 1);
                    const size_t ys = CVPixelBufferGetBytesPerRowOfPlane(a, 0), cs = CVPixelBufferGetBytesPerRowOfPlane(a, 1);
                    #define RV(k) (b == 1 ? ref[k] : (ref[2 * (k)] | ref[2 * (k) + 1] << 8))
                    for (size_t y = 0; y < hh && same; y++) for (size_t x = 0; x < w && same; x++)
                        same = (b == 1 ? Y[y * ys + x] : ((const uint16_t *)(Y + y * ys))[x] >> 6) == RV(y * w + x);
                    for (size_t y = 0; y < ch && same; y++) for (size_t x = 0; x < cw && same; x++) for (int k = 0; k < 2 && same; k++)
                        same = (b == 1 ? C[y * cs + 2 * x + k] : ((const uint16_t *)(C + y * cs))[2 * x + k] >> 6) == RV(w * hh + k * cw * ch + y * cw + x);
                    #undef RV
                    exact += same; free(ref);
                    CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly);
                }
                n += a != NULL; CFRelease(s2);
            }
            const double ms = (clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) - t0) / 1e6;
            printf("RESULT avread frames=%d/%lu in %.1f ms (%.0f fps), %d bit-exact vs reference, reader status %ld %s\n", n, (unsigned long)samples.count, ms,
                   n * 1000.0 / ms, exact, (long)r2.status, r2.error ? r2.error.localizedDescription.UTF8String : "");
            return 0;
        }
        if (getenv("VTDECODE_SPEED")) {   // decode speed only: VTDECODE_SPEED=hw|sw, frames released at once (no comparison)
            gNoKeep = true; Out h = {0}; bool hwH = false; const bool wantHw = strcmp(getenv("VTDECODE_SPEED"), "sw") != 0;
            if (decode(samples, fd, wantHw, &h, &hwH)) { printf("RESULT speed %s: no decoder\n", wantHw ? "hw" : "sw"); return 1; }
            printf("RESULT speed %s frames=%d in %.1f ms (%.1f fps), hardware %d, error %d\n", wantHw ? "hw" : "sw", h.n, h.ms, h.n * 1000.0 / h.ms, hwH, (int)h.err);
            return 0;
        }
        const char *refPath = getenv("VTDECODE_REF");
        if (refPath) {   // compare the hardware decoder's frames with a reference decode (I420, or 16-bit LE for 10-bit: ffmpeg's yuv420p10le)
            Out h = {0}; bool hwH = false;
            const bool wantHw = !getenv("VTDECODE_SW");
            if (decode(samples, fd, wantHw, &h, &hwH)) return 1;
            printf("  %s decoder: %d frames in %.1f ms (%.0f fps), using hardware %d, error %d\n", wantHw ? "hardware" : "software", h.n, h.ms, h.n * 1000.0 / h.ms, hwH, (int)h.err);
            FILE *rf = fopen(refPath, "rb"); if (!rf) { perror(refPath); return 1; }
            int exact = 0, first = -1; double worst = 99;
            // the reference is in display order, decoders return pictures in decode order: compare by presentation time
            NSMutableArray *idx = [NSMutableArray array]; for (int i = 0; i < h.n; i++) [idx addObject:@(i)];
            NSMutableArray *pts = h.pts;
            NSArray *order = [idx sortedArrayUsingComparator:^NSComparisonResult(NSNumber *x, NSNumber *y) {
                return [pts[x.intValue] compare:pts[y.intValue]]; }];
            for (int i = 0; i < h.n; i++) {
                CVPixelBufferRef a = (__bridge CVPixelBufferRef)h.frames[[order[i] intValue]];
                CVPixelBufferLockBaseAddress(a, kCVPixelBufferLock_ReadOnly);
                const size_t w = CVPixelBufferGetWidth(a), hh = CVPixelBufferGetHeight(a), cw = (w + 1) / 2, ch = (hh + 1) / 2;
                const OSType pf = CVPixelBufferGetPixelFormatType(a);
                const int b = (pf == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange || pf == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange) ? 2 : 1;
                const size_t n = w * hh + 2 * cw * ch; uint8_t *ref = malloc(n * b);
                if (fread(ref, b, n, rf) != n) { printf("  reference ends at frame %d\n", i); free(ref); CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly); break; }
                #define REF(k) (b == 1 ? ref[k] : (ref[2 * (k)] | ref[2 * (k) + 1] << 8))
                const uint8_t *Y = CVPixelBufferGetBaseAddressOfPlane(a, 0), *C = CVPixelBufferGetBaseAddressOfPlane(a, 1);
                const size_t ys = CVPixelBufferGetBytesPerRowOfPlane(a, 0), cs = CVPixelBufferGetBytesPerRowOfPlane(a, 1);
                double se = 0;
                for (size_t y = 0; y < hh; y++) for (size_t x = 0; x < w; x++) {
                    int v = b == 1 ? Y[y * ys + x] : ((const uint16_t *)(Y + y * ys))[x] >> 6; int d = v - (int)REF(y * w + x); se += d * d; }
                for (size_t y = 0; y < ch; y++) for (size_t x = 0; x < cw; x++) for (int k = 0; k < 2; k++) {
                    int v = b == 1 ? C[y * cs + 2 * x + k] : ((const uint16_t *)(C + y * cs))[2 * x + k] >> 6;
                    int d = v - (int)REF(w * hh + k * cw * ch + y * cw + x); se += d * d; }
                #undef REF
                const double mx = b == 1 ? 255.0 : 1023.0, p = se == 0 ? 99.0 : 10 * log10(mx * mx / (se / n));
                if (p >= 99) exact++; else if (first < 0) first = i;
                if (p < worst) worst = p;
                CVPixelBufferUnlockBaseAddress(a, kCVPixelBufferLock_ReadOnly); free(ref);
            }
            printf("RESULT hw=%d frames=%d/%lu vs reference: %d bit-exact, first difference %d, worst %.2f dB\n", hwH, h.n, (unsigned long)samples.count, exact, first, worst);
            return 0;
        }
        Out h = {0}, s = {0}; bool hwH = false, hwS = false;
        int rh = decode(samples, fd, true, &h, &hwH), rs = decode(samples, fd, false, &s, &hwS);
        if (!rh) printf("  hardware decoder: %d frames in %.1f ms (%.0f fps), using hardware %d, error %d\n", h.n, h.ms, h.n * 1000.0 / h.ms, hwH, (int)h.err);
        if (!rs) printf("  software decoder: %d frames in %.1f ms (%.0f fps), using hardware %d, error %d\n", s.n, s.ms, s.n * 1000.0 / s.ms, hwS, (int)s.err);
        if (!rh && !rs && h.n && s.n) {
            int n = h.n < s.n ? h.n : s.n; double sum = 0, mn = 99;
            for (int i = 0; i < n; i++) {
                double p = psnr((__bridge CVPixelBufferRef)h.frames[i], (__bridge CVPixelBufferRef)s.frames[i]); sum += p; if (p < mn) mn = p;
                if (getenv("VTDECODE_DUMP") && i == atoi(getenv("VTDECODE_DUMP"))) {   // both outputs of frame i as NV12 (tight)
                    for (int w = 0; w < 2; w++) {
                        CVPixelBufferRef pbx = (__bridge CVPixelBufferRef)(w ? s.frames[i] : h.frames[i]);
                        CVPixelBufferLockBaseAddress(pbx, kCVPixelBufferLock_ReadOnly);
                        char fn[64]; snprintf(fn, sizeof fn, "/tmp/vtdecode-%s-%d.nv12", w ? "sw" : "hw", i); FILE *df = fopen(fn, "wb");
                        size_t W = CVPixelBufferGetWidth(pbx), H = CVPixelBufferGetHeight(pbx);
                        for (int pl = 0; pl < 2; pl++) { const uint8_t *b0 = CVPixelBufferGetBaseAddressOfPlane(pbx, pl); size_t st = CVPixelBufferGetBytesPerRowOfPlane(pbx, pl);
                            for (size_t r = 0; r < (pl ? H / 2 : H); r++) fwrite(b0 + r * st, 1, W, df); }
                        fclose(df); CVPixelBufferUnlockBaseAddress(pbx, kCVPixelBufferLock_ReadOnly);
                    }
                }
                if (p < 99 && getenv("VTDECODE_V")) {
                    CVPixelBufferRef a = (__bridge CVPixelBufferRef)h.frames[i], b = (__bridge CVPixelBufferRef)s.frames[i];
                    printf("  frame %d: %.2f dB; hw %zux%zu stride %zu, sw %zux%zu stride %zu, pts hw %.3f sw %.3f\n", i, p,
                           CVPixelBufferGetWidth(a), CVPixelBufferGetHeight(a), CVPixelBufferGetBytesPerRowOfPlane(a, 0),
                           CVPixelBufferGetWidth(b), CVPixelBufferGetHeight(b), CVPixelBufferGetBytesPerRowOfPlane(b, 0), 0.0, 0.0);
                }
            }
            printf("RESULT hw=%d frames=%d/%d psnr_hw_vs_sw avg %.2f min %.2f dB (99 = identical)\n", hwH, h.n, (int)samples.count, sum / n, mn);
        } else printf("RESULT hw=0 (no hardware decode)\n");
    }
    return 0;
}
