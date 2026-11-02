// avwrite.m: write a moving test pattern to a movie through AVAssetWriter (the path most Mac apps take to VideoToolbox),
// read it back with AVAssetReader and compare the luma with what was written. Shows what an ordinary app gets when
// libnvencvt is registered for every process.
//   ./avwrite out.mov avc1|hvc1 W H FRAMES [main10]     main10: 10-bit input (x420) + HEVC Main10 profile
// Prints: RESULT codec=... frames=W/R psnr_avg=... (R = frames read back). The encoder is not visible through
// AVFoundation: touch /tmp/nvencvt.stats first and look for a new session in /tmp/nvencvt-stats.log.
#import <AVFoundation/AVFoundation.h>
#import <CoreVideo/CoreVideo.h>
#import <VideoToolbox/VideoToolbox.h>
#include <math.h>

static void pattern(uint8_t *y, size_t pitch, int w, int h, int f)   // 8-bit luma of frame f: moving bars + gradient
{
    for (int j = 0; j < h; j++) for (int i = 0; i < w; i++)
        y[j * pitch + i] = (uint8_t)(16 + ((((i + f * 8) / 64) & 1) ? 150 : 40) + (j * 60 / h));
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 6) { fprintf(stderr, "usage: avwrite out.mov avc1|hvc1 W H FRAMES [main10]\n"); return 2; }
    NSString *out = @(argv[1]); bool hevc = !strcmp(argv[2], "hvc1"); int W = atoi(argv[3]), H = atoi(argv[4]), N = atoi(argv[5]);
    bool ten = argc > 6 && !strcmp(argv[6], "main10");
    [[NSFileManager defaultManager] removeItemAtPath:out error:nil];
    NSError *err = nil;
    AVAssetWriter *wr = [AVAssetWriter assetWriterWithURL:[NSURL fileURLWithPath:out] fileType:AVFileTypeQuickTimeMovie error:&err];
    if (!wr) { printf("RESULT writer error %s\n", err.description.UTF8String); return 1; }
    NSMutableDictionary *vs = [@{ AVVideoCodecKey: hevc ? AVVideoCodecTypeHEVC : AVVideoCodecTypeH264, AVVideoWidthKey: @(W), AVVideoHeightKey: @(H),
                                  AVVideoCompressionPropertiesKey: [@{ AVVideoAverageBitRateKey: @(W * H * 4) } mutableCopy] } mutableCopy];
    if (ten) vs[AVVideoCompressionPropertiesKey][AVVideoProfileLevelKey] = (__bridge NSString *)kVTProfileLevel_HEVC_Main10_AutoLevel;
    AVAssetWriterInput *in = [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeVideo outputSettings:vs];
    OSType pf = ten ? kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    AVAssetWriterInputPixelBufferAdaptor *ad = [AVAssetWriterInputPixelBufferAdaptor assetWriterInputPixelBufferAdaptorWithAssetWriterInput:in
        sourcePixelBufferAttributes:@{ (id)kCVPixelBufferPixelFormatTypeKey: @(pf), (id)kCVPixelBufferWidthKey: @(W), (id)kCVPixelBufferHeightKey: @(H) }];
    [wr addInput:in];
    if (![wr startWriting]) { printf("RESULT startWriting failed: %s\n", wr.error.description.UTF8String); return 1; }
    [wr startSessionAtSourceTime:kCMTimeZero];
    uint8_t *ref = malloc((size_t)W * H);
    int written = 0;
    for (int f = 0; f < N; f++) {
        while (!in.readyForMoreMediaData) usleep(1000);
        CVPixelBufferRef pb = NULL;
        if (CVPixelBufferPoolCreatePixelBuffer(NULL, ad.pixelBufferPool, &pb)) { printf("no pixel buffer\n"); break; }
        CVPixelBufferLockBaseAddress(pb, 0);
        pattern(ref, W, W, H, f);
        uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(pb, 0), *uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
        size_t py = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), puv = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
        for (int j = 0; j < H; j++) {
            if (ten) for (int i = 0; i < W; i++) ((uint16_t *)(y + j * py))[i] = (uint16_t)(ref[j * W + i] << 8);
            else memcpy(y + j * py, ref + j * W, W);
        }
        for (int j = 0; j < H / 2; j++) {
            if (ten) for (int i = 0; i < W; i++) ((uint16_t *)(uv + j * puv))[i] = 128 << 8; else memset(uv + j * puv, 128, W);
        }
        CVPixelBufferUnlockBaseAddress(pb, 0);
        if (![ad appendPixelBuffer:pb withPresentationTime:CMTimeMake(f, 30)]) { printf("append failed at %d: %s\n", f, wr.error.description.UTF8String); CVPixelBufferRelease(pb); break; }
        CVPixelBufferRelease(pb); written++;
    }
    [in markAsFinished];
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    [wr finishWritingWithCompletionHandler:^{ dispatch_semaphore_signal(done); }];
    dispatch_semaphore_wait(done, DISPATCH_TIME_FOREVER);
    if (wr.status != AVAssetWriterStatusCompleted) { printf("RESULT codec=%s write failed: %s\n", argv[2], wr.error.description.UTF8String); return 1; }

    // read back, 8-bit luma, compare with the pattern
    AVURLAsset *as = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:out] options:nil];
    AVAssetTrack *t = [as tracksWithMediaType:AVMediaTypeVideo].firstObject;
    CMFormatDescriptionRef fd = (__bridge CMFormatDescriptionRef)t.formatDescriptions.firstObject;
    FourCharCode cc = CMFormatDescriptionGetMediaSubType(fd); cc = CFSwapInt32HostToBig(cc);
    AVAssetReader *rd = [AVAssetReader assetReaderWithAsset:as error:&err];
    AVAssetReaderTrackOutput *o = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:t
        outputSettings:@{ (id)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) }];
    if (getenv("AVWRITE_NOCOPY")) o.alwaysCopiesSampleData = NO;   // default YES: the reader copies every decoded frame
    [rd addOutput:o]; [rd startReading];
    int f = 0; double sum = 0, mn = 1e9;
    CMSampleBufferRef sb;
    while ((sb = [o copyNextSampleBuffer])) {
        CVPixelBufferRef pb = CMSampleBufferGetImageBuffer(sb);
        CVPixelBufferLockBaseAddress(pb, kCVPixelBufferLock_ReadOnly);
        uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(pb, 0); size_t py = CVPixelBufferGetBytesPerRowOfPlane(pb, 0);
        pattern(ref, W, W, H, f);
        double se = 0;
        for (int j = 0; j < H; j++) for (int i = 0; i < W; i++) { double d = (double)y[j * py + i] - ref[j * W + i]; se += d * d; }
        double mse = se / ((double)W * H), p = mse > 0 ? 10 * log10(255.0 * 255.0 / mse) : 99;
        sum += p; if (p < mn) mn = p;
        CVPixelBufferUnlockBaseAddress(pb, kCVPixelBufferLock_ReadOnly); CFRelease(sb); f++;
    }
    long long bytes = [[[NSFileManager defaultManager] attributesOfItemAtPath:out error:nil] fileSize];
    printf("RESULT codec=%s%s track=%.4s frames=%d/%d psnr_avg=%.2f psnr_min=%.2f bytes=%lld reader=%ld\n", argv[2], ten ? " main10" : "",
           (char *)&cc, written, f, f ? sum / f : 0, f ? mn : 0, bytes, (long)rd.status);
    return 0;
} }
