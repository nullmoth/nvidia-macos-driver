/* Creates an H.264 encoder instance from libnvencvt and sets EnableLowLatencyRateControl on it directly through its
 * CoreMedia base object (the instance is not a VTSession, so the VTSession functions refuse it). No GPU is touched: the
 * hardware is opened by the first frame, not by the instance.
 * usage: nvencvt_lowlatency_probe <libnvencvt.dylib>; built by test_nvencvt_lowlatency.py */
#include <VideoToolbox/VideoToolbox.h>
typedef struct CMBaseProtocolVTable CMBaseProtocolVTable;
typedef struct CMBaseProtocolTable CMBaseProtocolTable;
#include "CMBaseObjectSPI.h"
#include <dlfcn.h>
#include <stdio.h>
#define SETP(o, k, v) CMBaseObjectGetVTable((CMBaseObjectRef)(o))->baseClass->setProperty((CMBaseObjectRef)(o), k, v)
#define COPYP(o, k, a, out) CMBaseObjectGetVTable((CMBaseObjectRef)(o))->baseClass->copyProperty((CMBaseObjectRef)(o), k, a, out)
int main(int argc, char **argv)
{
    void *h = dlopen(argv[1], RTLD_NOW);
    if (!h) { printf("dlopen: %s\n", dlerror()); return 1; }
    OSStatus (*create)(FourCharCode, CFAllocatorRef, void **) = dlsym(h, "NVEncVT_CreateInstance");
    void *enc = NULL;
    OSStatus st = create(kCMVideoCodecType_H264, NULL, &enc);
    printf("create %d enc %p\n", (int)st, enc);
    if (st || !enc) return 2;
    OSStatus s1 = SETP(enc, kVTVideoEncoderSpecification_EnableLowLatencyRateControl, kCFBooleanTrue);
    printf("set LLRC true -> %d\n", (int)s1);
    CFTypeRef v = NULL;
    OSStatus s2 = COPYP(enc, kVTVideoEncoderSpecification_EnableLowLatencyRateControl, NULL, &v);
    printf("copy LLRC -> %d value %s\n", (int)s2, v == kCFBooleanTrue ? "true" : "other");
    OSStatus s3 = SETP(enc, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanTrue);
    CFTypeRef r = NULL; COPYP(enc, kVTCompressionPropertyKey_AllowFrameReordering, NULL, &r);
    printf("set AllowFrameReordering true after LLRC -> %d, reads %s\n", (int)s3, r == kCFBooleanTrue ? "true" : "false");
    CFTypeRef rt = NULL; COPYP(enc, kVTCompressionPropertyKey_RealTime, NULL, &rt);
    printf("RealTime reads %s\n", rt == kCFBooleanTrue ? "true" : "false");
    OSStatus s4 = SETP(enc, kVTCompressionPropertyKey_RealTime, kCFBooleanFalse);
    COPYP(enc, kVTCompressionPropertyKey_RealTime, NULL, &rt);
    printf("set RealTime false -> %d, reads %s (stays true under low latency)\n", (int)s4, rt == kCFBooleanTrue ? "true" : "false");
    return 0;
}
