// vttwo.m: two VideoToolbox encode sessions in one process at the same time (a GPU with one NVENC: the second session has
// to share NVENC0; libnvencvt falls back when NVENC1 cannot be opened). Encodes N frames of a moving pattern in each,
// alternating, and reports the frames and bytes each session produced and the wall time.
//   vttwo [codecA codecB W H N]     default: avc1 hvc1 1920 1080 120 (run with the library injected or the overlay installed)
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <CoreVideo/CoreVideo.h>

typedef struct { int frames; size_t bytes; OSStatus err; } Out;

static void cb(void *ref, void *src, OSStatus st, VTEncodeInfoFlags fl, CMSampleBufferRef sb)
{
    (void)src; (void)fl;
    Out *o = ref;
    if (st) { o->err = st; return; }
    if (sb) { o->frames++; o->bytes += CMSampleBufferGetTotalSampleSize(sb); }
}

static FourCharCode fourcc(const char *s) { return (FourCharCode)s[0] << 24 | (FourCharCode)s[1] << 16 | (FourCharCode)s[2] << 8 | (FourCharCode)s[3]; }

int main(int argc, char **argv)
{
    @autoreleasepool {
        const char *ca = argc > 1 ? argv[1] : "avc1", *cbn = argc > 2 ? argv[2] : "hvc1";
        int W = argc > 3 ? atoi(argv[3]) : 1920, H = argc > 4 ? atoi(argv[4]) : 1080, N = argc > 5 ? atoi(argv[5]) : 120;
        VTCompressionSessionRef s[2] = { NULL, NULL }; Out out[2] = { { 0 } };
        const char *names[2] = { ca, cbn };
        for (int i = 0; i < 2; i++) {
            OSStatus st = VTCompressionSessionCreate(NULL, W, H, fourcc(names[i]), NULL, NULL, NULL, cb, &out[i], &s[i]);
            if (st) { printf("session %d (%s): create -> %d\n", i, names[i], (int)st); return 1; }
            VTSessionSetProperty(s[i], kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
            VTSessionSetProperty(s[i], kVTCompressionPropertyKey_AverageBitRate, (__bridge CFNumberRef)@(8000000));
            CFStringRef id = NULL; VTSessionCopyProperty(s[i], kVTCompressionPropertyKey_EncoderID, NULL, &id);
            printf("session %d (%s): encoder %s\n", i, names[i], id ? [(__bridge NSString *)id UTF8String] : "?");
            if (id) CFRelease(id);
        }
        NSDictionary *attr = @{ (id)kCVPixelBufferIOSurfacePropertiesKey: @{} };
        double t0 = CFAbsoluteTimeGetCurrent();
        for (int f = 0; f < N; f++) {
            for (int i = 0; i < 2; i++) {
                CVPixelBufferRef pb = NULL;
                CVPixelBufferCreate(NULL, W, H, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, (__bridge CFDictionaryRef)attr, &pb);
                CVPixelBufferLockBaseAddress(pb, 0);
                uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(pb, 0), *uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
                size_t ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), uvs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
                for (int r = 0; r < H; r++) for (int c = 0; c < W; c++) y[r * ys + c] = (uint8_t)((c + r + 4 * f + 64 * i) & 0xff);
                for (int r = 0; r < H / 2; r++) memset(uv + r * uvs, 128, (size_t)W);
                CVPixelBufferUnlockBaseAddress(pb, 0);
                OSStatus st = VTCompressionSessionEncodeFrame(s[i], pb, CMTimeMake(f, 60), CMTimeMake(1, 60), NULL, NULL, NULL);
                if (st && !out[i].err) out[i].err = st;
                CVPixelBufferRelease(pb);
            }
        }
        for (int i = 0; i < 2; i++) VTCompressionSessionCompleteFrames(s[i], kCMTimeInvalid);
        double ms = (CFAbsoluteTimeGetCurrent() - t0) * 1000;
        for (int i = 0; i < 2; i++) {
            printf("session %d (%s): %d of %d frames, %zu bytes, error %d\n", i, names[i], out[i].frames, N, out[i].bytes, (int)out[i].err);
            VTCompressionSessionInvalidate(s[i]); CFRelease(s[i]);
        }
        printf("RESULT two sessions %dx%d: %d frames each in %.0f ms (%.1f fps per session)\n", W, H, N, ms, N * 1000.0 / ms);
    }
    return 0;
}
