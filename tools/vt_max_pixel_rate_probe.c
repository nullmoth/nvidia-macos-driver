/* Asks the HEVC decoder macOS picks for a small stream how many pixels per second it can decode, the way the Screen Sharing
   viewer does: a format description built from a fixed hvcC record, a decompression session, then a read of
   kVTDecompressionPropertyKey_MaxDecoderPixelRate. Decodes nothing. usage: ratequery [require-hardware] */
#include <VideoToolbox/VideoToolbox.h>
#include <dlfcn.h>
#include <stdio.h>
#include <string.h>
static const unsigned char HVCC[113] = {
0x01,0x01,0x60,0x00,0x00,0x00,0xB0,0x00,0x00,0x00,0x00,0x00,0x99,0xF0,0x00,0xFC,0xFD,0xF8,0xF8,0x00,0x00,0x0F,0x03,
0xA0,0x00,0x01,0x00,0x18,0x40,0x01,0x0C,0x01,0xFF,0xFF,0x01,0x60,0x00,0x00,0x03,0x00,0xB0,0x00,0x00,0x03,0x00,0x00,0x03,0x00,0x99,0x1B,0x02,0x40,
0xA1,0x00,0x01,0x00,0x2C,0x42,0x01,0x01,0x01,0x60,0x00,0x00,0x03,0x00,0xB0,0x00,0x00,0x03,0x00,0x00,0x03,0x00,0x99,0xA0,0x01,0xE0,0x20,0x02,0x1C,0x58,0x81,0xBB,0x91,0x48,0xCB,0x9F,0xC4,0xFC,0x2F,0xE8,0x6F,0xD4,0xA6,0xA0,0x40,0x40,0x54,0x80,0x40,
0xA2,0x00,0x01,0x00,0x07,0x44,0x01,0xC0,0x72,0xF0,0x5B,0x24 };
int main(int argc, char **argv)
{
    CFDataRef hv = CFDataCreate(NULL, HVCC, sizeof HVCC);
    CFDictionaryRef atoms = CFDictionaryCreate(NULL, (const void *[]){ CFSTR("hvcC") }, (const void *[]){ hv }, 1,
                                               &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionaryRef ext = CFDictionaryCreate(NULL, (const void *[]){ kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms },
                                             (const void *[]){ atoms }, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CMVideoFormatDescriptionRef fd = NULL;
    OSStatus st = CMVideoFormatDescriptionCreate(NULL, kCMVideoCodecType_HEVC, 1280, 720, ext, &fd);
    printf("format description: %d\n", (int)st);
    if (st) return 1;
    CFMutableDictionaryRef spec = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    if (argc > 1) CFDictionarySetValue(spec, kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder, kCFBooleanTrue);
    VTDecompressionOutputCallbackRecord cb = { 0 };
    VTDecompressionSessionRef s = NULL;
    st = VTDecompressionSessionCreate(NULL, fd, spec, NULL, &cb, &s);
    printf("session: %d\n", (int)st);
    if (st || !s) return 2;
    CFStringRef *key = dlsym(RTLD_DEFAULT, "kVTDecompressionPropertyKey_MaxDecoderPixelRate");
    printf("key symbol: %s\n", key ? "found" : "missing"); if (key) { char b[128]; CFStringGetCString(*key, b, sizeof b, kCFStringEncodingUTF8); printf("key string: %s\n", b); }
    if (key) {
        CFTypeRef v = NULL;
        st = VTSessionCopyProperty(s, *key, NULL, &v);
        printf("MaxDecoderPixelRate: status %d value ", (int)st);
        if (v) CFShow(v); else printf("(none)\n");
    }
    CFTypeRef hw = NULL;
    st = VTSessionCopyProperty(s, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &hw);
    printf("UsingHardwareAcceleratedVideoDecoder: status %d value %s\n", (int)st, hw ? (hw == kCFBooleanTrue ? "yes" : "no") : "(none)");
    return 0;
}
