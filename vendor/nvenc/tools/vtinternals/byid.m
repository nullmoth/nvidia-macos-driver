// byid.m: ask VideoToolbox for an encoder by ID (kVTVideoEncoderSpecification_EncoderID), to see whether a plug-in
// bundle is in the registry even when VTCopyVideoEncoderList does not show it.
//   ./byid ENCODER_ID [codec]
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
int main(int argc, char **argv) { @autoreleasepool {
    FourCharCode c = argc > 2 ? (FourCharCode)((argv[2][0] << 24) | (argv[2][1] << 16) | (argv[2][2] << 8) | argv[2][3]) : 'avc1';
    NSDictionary *spec = @{ (__bridge NSString *)kVTVideoEncoderSpecification_EncoderID: @(argv[1]) };
    VTCompressionSessionRef s = NULL;
    OSStatus st = VTCompressionSessionCreate(NULL, 640, 360, c, (__bridge CFDictionaryRef)spec, NULL, NULL, NULL, NULL, &s);
    printf("session for %s -> %d\n", argv[1], (int)st);
    CFArrayRef l = NULL; VTCopyVideoEncoderList(NULL, &l);
    for (NSDictionary *d in (__bridge NSArray *)l) if ([d[@"EncoderID"] containsString:@"probe"]) printf("LISTED %s\n", [[d description] UTF8String]);
} }
