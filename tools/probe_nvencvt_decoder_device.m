// Build with -fobjc-arc and the Foundation, AVFoundation, CoreMedia, CoreVideo,
// Metal, VideoToolbox and IOKit frameworks.
#import <AVFoundation/AVFoundation.h>
#import <IOKit/IOKitLib.h>
#import <Metal/Metal.h>
#import <VideoToolbox/VideoToolbox.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>

typedef struct { atomic_uint frames, errors; } Output;

static void decoded(void *context, void *source, OSStatus status, VTDecodeInfoFlags flags,
                    CVImageBufferRef image, CMTime pts, CMTime duration)
{
    Output *output = context;
    if (status) atomic_fetch_add(&output->errors, 1);
    if (!status && image) atomic_fetch_add(&output->frames, 1);
}

// Returns a PCI endpoint ID only when the session provides one and it resolves
// to an NVIDIA PCI device. A Metal accelerator ID belongs to a different namespace.
static uint64_t nvencvtPCIRegistryID(VTDecompressionSessionRef session, uint64_t expected)
{
    CFTypeRef value = NULL;
    OSStatus status = VTSessionCopyProperty(session, CFSTR("org.nvenc.decoder-device.v1"), NULL, &value);
    int64_t id = 0;
    if (!status && value && CFGetTypeID(value) == CFNumberGetTypeID() && !CFNumberIsFloatType(value) &&
        !CFNumberGetValue(value, kCFNumberSInt64Type, &id)) id = 0;
    if (value) CFRelease(value);
    if (id <= 0 || (expected && (uint64_t)id != expected)) return 0;

    io_service_t entry = IOServiceGetMatchingService(kIOMainPortDefault, IORegistryEntryIDMatching((uint64_t)id));
    if (!entry) return 0;
    uint32_t vendor = 0;
    if (IOObjectConformsTo(entry, "IOPCIDevice")) {
        CFTypeRef property = IORegistryEntryCreateCFProperty(entry, CFSTR("vendor-id"), NULL, 0);
        if (property && CFGetTypeID(property) == CFDataGetTypeID() && CFDataGetLength(property) == sizeof vendor)
            CFDataGetBytes(property, CFRangeMake(0, sizeof vendor), (UInt8 *)&vendor);
        if (property) CFRelease(property);
    }
    IOObjectRelease(entry);
    return vendor == 0x10de ? (uint64_t)id : 0;
}

static CMSampleBufferRef nextSample(AVAssetReaderTrackOutput *input)
{
    CMSampleBufferRef sample;
    while ((sample = [input copyNextSampleBuffer])) {
        if (CMSampleBufferGetNumSamples(sample)) return sample;
        CFRelease(sample);
    }
    return NULL;
}

int main(int argc, const char **argv)
{
    @autoreleasepool {
        if (argc < 3 || argc > 4 || (strcmp(argv[2], "hw") && strcmp(argv[2], "sw"))) {
            fprintf(stderr, "Usage: %s video.mp4 hw|sw [expected-pci-id]\n", argv[0]);
            return 2;
        }
        char *end = NULL;
        uint64_t expected = argc == 4 ? strtoull(argv[3], &end, 0) : 0;
        if (argc == 4 && (!expected || !end || *end)) return 2;
        bool wantHardware = !strcmp(argv[2], "hw");
        AVURLAsset *asset = [AVURLAsset URLAssetWithURL:[NSURL fileURLWithPath:@(argv[1])] options:nil];
        AVAssetTrack *track = [asset tracksWithMediaType:AVMediaTypeVideo].firstObject;
        NSError *error = nil;
        AVAssetReader *reader = [[AVAssetReader alloc] initWithAsset:asset error:&error];
        if (!track || !reader) { NSLog(@"Cannot read video: %@", error); return 2; }
        AVAssetReaderTrackOutput *input = [AVAssetReaderTrackOutput assetReaderTrackOutputWithTrack:track outputSettings:nil];
        if (![reader canAddOutput:input]) return 2;
        [reader addOutput:input];
        if (![reader startReading]) return 2;
        CMSampleBufferRef sample = nextSample(input);
        if (!sample) return 2;
        // Initialize Metal before VideoToolbox enumerates codecs.
        (void)MTLCopyAllDevices();
        NSDictionary *spec = wantHardware ? @{ (id)kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: @YES }
                                         : @{ (id)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: @NO };
        Output output = {0};
        NSDictionary *attributes = @{ (id)kCVPixelBufferPixelFormatTypeKey:
            @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) };
        VTDecompressionOutputCallbackRecord callback = { decoded, &output };
        VTDecompressionSessionRef session = NULL;
        OSStatus status = VTDecompressionSessionCreate(NULL, CMSampleBufferGetFormatDescription(sample),
                                                        (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)attributes,
                                                        &callback, &session);
        unsigned samples = 0;
        if (!status) {
            while (sample) {
                status = VTDecompressionSessionDecodeFrame(session, sample,
                         kVTDecodeFrame_EnableAsynchronousDecompression, NULL, NULL);
                CFRelease(sample);
                sample = NULL;
                if (status) break;
                samples++;
                sample = nextSample(input);
            }
        }
        if (sample) CFRelease(sample);
        if (!session) { fprintf(stderr, "Session creation: %d\n", (int)status); return 1; }
        OSStatus finish = VTDecompressionSessionFinishDelayedFrames(session);
        OSStatus drain = VTDecompressionSessionWaitForAsynchronousFrames(session);
        CFTypeRef hardware = NULL;
        VTSessionCopyProperty(session, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &hardware);
        bool usedHardware = hardware == kCFBooleanTrue;
        if (hardware) CFRelease(hardware);
        unsigned frames = atomic_load(&output.frames), errors = atomic_load(&output.errors);
        bool ok = !status && !finish && !drain && reader.status == AVAssetReaderStatusCompleted &&
                  samples && frames == samples && !errors && wantHardware == usedHardware;
        uint64_t pciID = ok && usedHardware ? nvencvtPCIRegistryID(session, expected) : 0;
        VTDecompressionSessionInvalidate(session);
        CFRelease(session);
        printf("frames=%u/%u errors=%u hardware=%d pciRegistryID=0x%llx\n",
               frames, samples, errors, usedHardware, (unsigned long long)pciID);
        return !ok ? 1 : expected && !pciID ? 3 : 0;
    }
}
