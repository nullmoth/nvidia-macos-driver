// vtrandom.m: encode one random NV12 frame with VideoToolbox's hardware encoder (whatever is registered: run it under
// nvtrace to capture the input surface a working NVENC client hands the engine) and write the source frame.
//   ./vtrandom W H src.nv12
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <VideoToolbox/VideoToolbox.h>

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 4) { fprintf(stderr, "usage: vtrandom W H src.nv12\n"); return 2; }
    int W = atoi(argv[1]), H = atoi(argv[2]);
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); (void)dev;   // nullmoth's plugin registers its encoders on device creation
    NSDictionary *spec = @{ (__bridge NSString *)kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: @YES };
    VTCompressionSessionRef s = NULL;
    OSStatus st = VTCompressionSessionCreate(NULL, W, H, kCMVideoCodecType_H264, (__bridge CFDictionaryRef)spec, NULL, NULL, NULL, NULL, &s);
    if (st) { printf("session -> %d\n", (int)st); return 1; }
    CVPixelBufferRef pb = NULL;
    NSDictionary *attr = @{ (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{} };
    CVPixelBufferCreate(NULL, W, H, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, (__bridge CFDictionaryRef)attr, &pb);
    CVPixelBufferLockBaseAddress(pb, 0);
    FILE *f = fopen(argv[3], "wb"); srandom(1234);
    for (int pl = 0; pl < 2; pl++) {
        uint8_t *b = CVPixelBufferGetBaseAddressOfPlane(pb, pl); size_t bpr = CVPixelBufferGetBytesPerRowOfPlane(pb, pl);
        for (int r = 0; r < (pl ? H / 2 : H); r++) { for (int c = 0; c < W; c++) b[r * bpr + c] = (uint8_t)(16 + random() % 220); fwrite(b + r * bpr, 1, W, f); }
    }
    fclose(f);
    CVPixelBufferUnlockBaseAddress(pb, 0);
    __block int n = 0;
    st = VTCompressionSessionEncodeFrameWithOutputHandler(s, pb, CMTimeMake(0, 60), kCMTimeInvalid, NULL, NULL,
        ^(OSStatus s2, VTEncodeInfoFlags fl, CMSampleBufferRef sb) { if (sb) n++; });
    VTCompressionSessionCompleteFrames(s, kCMTimeInvalid);
    printf("encode -> %d, %d sample(s)\n", (int)st, n);
    VTCompressionSessionInvalidate(s);
    return 0;
} }
