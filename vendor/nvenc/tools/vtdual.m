// vtdual.m: two VideoToolbox H.264 sessions encoding at the same time from two threads (M6, T6.01): with libnvencvt
// they should land on NVENC0 and NVENC1. Reports per-session frame rate.
//   DYLD_INSERT_LIBRARIES=~/nvenc-dev/libnvencvt.dylib NVENCVT_LOG=1 ./vtdual W H FRAMES
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>

static double ms[2]; static long bytes[2], errs[2];

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 4) { fprintf(stderr, "usage: vtdual W H FRAMES\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]), F = atoi(argv[3]);
    dispatch_group_t g = dispatch_group_create();
    for (int k = 0; k < 2; k++)
        dispatch_group_async(g, dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
            CVPixelBufferRef pb = NULL;
            NSDictionary *attr = @{ (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{} };
            CVPixelBufferCreate(NULL, W, H, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, (__bridge CFDictionaryRef)attr, &pb);
            CVPixelBufferLockBaseAddress(pb, 0);
            for (int p = 0; p < 2; p++) memset(CVPixelBufferGetBaseAddressOfPlane(pb, p), 100 + 20 * k + p * 10,
                                               CVPixelBufferGetBytesPerRowOfPlane(pb, p) * (p ? H / 2 : H));
            CVPixelBufferUnlockBaseAddress(pb, 0);
            NSDictionary *spec = @{ (__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: @YES };
            VTCompressionSessionRef cs = NULL;
            if (VTCompressionSessionCreate(NULL, W, H, kCMVideoCodecType_H264, (__bridge CFDictionaryRef)spec, NULL, NULL, NULL, NULL, &cs)) { errs[k] = -1; return; }
            VTSessionSetProperty(cs, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
            __block long nb = 0, ne = 0;
            CFAbsoluteTime t0 = CFAbsoluteTimeGetCurrent();
            for (int f = 0; f < F; f++)
                VTCompressionSessionEncodeFrameWithOutputHandler(cs, pb, CMTimeMake(f, 60), CMTimeMake(1, 60), NULL, NULL,
                    ^(OSStatus s2, VTEncodeInfoFlags fl, CMSampleBufferRef sb) { if (s2 || !sb) ne++; else nb += CMSampleBufferGetTotalSampleSize(sb); });
            VTCompressionSessionCompleteFrames(cs, kCMTimeInvalid);
            ms[k] = (CFAbsoluteTimeGetCurrent() - t0) * 1000; bytes[k] = nb; errs[k] = ne;
            VTCompressionSessionInvalidate(cs); CFRelease(cs); CVPixelBufferRelease(pb);
        });
    dispatch_group_wait(g, DISPATCH_TIME_FOREVER);
    for (int k = 0; k < 2; k++) printf("session %d: %d frames in %.1f ms (%.0f fps), %ld bytes, %ld errors\n", k, F, ms[k], F * 1000 / ms[k], bytes[k], errs[k]);
    return errs[0] || errs[1];
} }
