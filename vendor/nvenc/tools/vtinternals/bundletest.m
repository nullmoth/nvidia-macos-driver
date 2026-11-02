// bundletest.m: does VTRegisterVideoEncoderBundleDirectory load an encoder plug-in bundle from our own folder, and does
// the encoder list then show the bundle's VTEncoderName?
//   ./bundletest DIR [url|path]
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
extern OSStatus VTRegisterVideoEncoderBundleDirectory(const void *dir);

int main(int argc, char **argv) { @autoreleasepool {
    NSString *dir = @(argv[1]);
    bool asPath = argc > 2 && !strcmp(argv[2], "path");
    const void *arg = asPath ? (__bridge const void *)dir : (__bridge const void *)[NSURL fileURLWithPath:dir isDirectory:YES];
    OSStatus st = VTRegisterVideoEncoderBundleDirectory(arg);
    printf("VTRegisterVideoEncoderBundleDirectory(%s %s) -> %d\n", asPath ? "path" : "url", argv[1], (int)st);
    CFArrayRef l = NULL; VTCopyVideoEncoderList(NULL, &l);
    for (NSDictionary *d in (__bridge NSArray *)l)
        if ([[d description] containsString:@"rb0"] || [[d description] containsString:@"probe"] || [[d description] containsString:@"nvenc"])
            printf("LISTED: %s\n", [[d description] stringByReplacingOccurrencesOfString:@"\n" withString:@" "].UTF8String);
    VTCompressionSessionRef s = NULL;
    printf("session create prb0 -> %d\n", (int)VTCompressionSessionCreate(NULL, 640, 360, 'prb0', NULL, NULL, NULL, NULL, NULL, &s));
} }
