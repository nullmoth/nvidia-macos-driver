// vtprobe.m: work out VideoToolbox's encoder plugin interface black-box (M4, T4.02/T4.03). Registers a stub encoder in
// this process (VTRegisterVideoEncoderWithInfo), whose factory and every encoder class slot log their index and raw
// arguments, then creates a compression session that must use it and encodes a few frames.
//   ./vtprobe [CLASS_VERSION] [RETURN]        RETURN: what the slots return (default 0); SLOT_RET_<n>=<status> per slot
// CMBaseObject declarations: WebKit's CMBaseObjectSPI.h (vendor/webkit-spi). The encoder class layout is what this
// tool finds out; slots are just numbered here.
#import <Foundation/Foundation.h>
#import <VideoToolbox/VideoToolbox.h>
#include <malloc/malloc.h>
typedef struct CMBaseProtocolVTable CMBaseProtocolVTable;   // the header is written for C++
typedef struct CMBaseProtocolTable CMBaseProtocolTable;
#include "CMBaseObjectSPI.h"

extern CMBaseClassID VTVideoEncoderGetClassID(void);
extern OSStatus VTRegisterVideoEncoderWithInfo(FourCharCode, CFDictionaryRef, void *);

#define NSLOTS 24
static int gRet;
static int ret_for(int slot) { char k[32]; snprintf(k, sizeof k, "SLOT_RET_%d", slot); return getenv(k) ? atoi(getenv(k)) : gRet; }

static void show(const char *what, int slot, uint64_t a[6])
{
    // raw values first (flushed), then descriptions of CF objects on the heap (malloc_size() > 0: safe to inspect)
    printf("%s %2d: %#llx %#llx %#llx %#llx %#llx %#llx\n", what, slot, (unsigned long long)a[0], (unsigned long long)a[1],
           (unsigned long long)a[2], (unsigned long long)a[3], (unsigned long long)a[4], (unsigned long long)a[5]);
    fflush(stdout);
    for (int i = 0; i < 6; i++) {
        const void *p = (const void *)a[i];
        if (a[i] > 0x100000 && malloc_size(p) >= 16) {
            CFTypeID t = CFGetTypeID((CFTypeRef)p);
            if (t == CFStringGetTypeID()) printf("    arg%d: \"%s\"\n", i, [(__bridge NSString *)p UTF8String]);
            else if (t == CFDictionaryGetTypeID() || t == CFArrayGetTypeID() || t == CFNumberGetTypeID()) printf("    arg%d: %s\n", i, [[(__bridge id)p description] UTF8String]);
            else if (t < 1000) { CFStringRef d = CFCopyTypeIDDescription(t); printf("    arg%d: <%s> %p\n", i, [(__bridge NSString *)d UTF8String], p); CFRelease(d); }
        } else if (a[i] >= 0x20202020 && a[i] <= 0x7a7a7a7a) {
            char cc[5] = { (char)(a[i] >> 24), (char)(a[i] >> 16), (char)(a[i] >> 8), (char)a[i], 0 };
            int ok = 1; for (int k = 0; k < 4; k++) ok &= cc[k] >= 0x20 && cc[k] < 0x7f;
            if (ok) printf("    arg%d: '%s'\n", i, cc);
        }
    }
    fflush(stdout);
}

#define SLOT(n) static OSStatus slot##n(uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) \
    { uint64_t a[6] = { a0, a1, a2, a3, a4, a5 }; show("slot", n, a); return ret_for(n); }
SLOT(0) SLOT(1) SLOT(2) SLOT(3) SLOT(4) SLOT(5) SLOT(6) SLOT(7) SLOT(8) SLOT(9) SLOT(10) SLOT(11)
SLOT(12) SLOT(13) SLOT(14) SLOT(15) SLOT(16) SLOT(17) SLOT(18) SLOT(19) SLOT(20) SLOT(21) SLOT(22) SLOT(23)

#pragma pack(push, 4)   // like CMBaseClass: pointers right after the 32-bit version (nullmoth's encoder class is laid out so)
typedef struct { CMBaseClassVersion version; void *slots[NSLOTS]; } ProbeEncoderClass;

// typed guesses (TYPED=1): slot 0 startSession, 1 encodeFrame, 2 completeFrames, 3 copySupportedPropertyDictionary
#include <dlfcn.h>
static void *gSession;
static OSStatus t_start(CMBaseObjectRef enc, void *session, CMVideoDimensions dims, uint64_t a3, uint64_t a4)
{
    Dl_info di; const char *n = dladdr((void *)a4, &di) && di.dli_sname ? di.dli_sname : "?";
    printf("startSession: session %p dims %dx%d a3 %#llx a4 %#llx (%s)\n", session, dims.width, dims.height, (unsigned long long)a3, (unsigned long long)a4, n);
    if (a4 > 0x100000) { const uint32_t *w = (const uint32_t *)a4; printf("    a4 points at %08x %08x %08x %08x\n", w[0], w[1], w[2], w[3]); }
    fflush(stdout); gSession = session; return 0;
}
extern OSStatus VTEncoderSessionEmitEncodedFrame(void *session, void *frame, OSStatus status, VTEncodeInfoFlags flags, CMSampleBufferRef sb);
static OSStatus t_encode(CMBaseObjectRef enc, void *frame, CVImageBufferRef img, CMTime pts, CMTime dur, CFDictionaryRef props, uint32_t *flags)
{
    printf("encodeFrame: frame %p image %p pts %lld/%d (flags %u) dur %lld/%d props %p infoFlags* %p\n", frame, img, pts.value, pts.timescale,
           pts.flags, dur.value, dur.timescale, props, flags); fflush(stdout);
    if (getenv("EMIT")) {   // emit a sample made from an Annex B file (SPS, PPS, one slice): EMIT=<file.h264>
        NSData *es = [NSData dataWithContentsOfFile:@(getenv("EMIT"))]; const uint8_t *p = es.bytes; size_t n = es.length;
        size_t st[8], cnt = 0;   // NAL starts after 00 00 00 01
        for (size_t i = 0; i + 4 <= n && cnt < 8; i++) if (!p[i] && !p[i + 1] && !p[i + 2] && p[i + 3] == 1) st[cnt++] = i + 4;
        const uint8_t *ps[2] = { p + st[0], p + st[1] }; size_t pl[2] = { st[1] - 4 - st[0], st[2] - 4 - st[1] };
        CMVideoFormatDescriptionRef fmt = NULL;
        OSStatus s1 = CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, ps, pl, 4, &fmt);
        size_t sl = n - st[2]; uint8_t *buf = malloc(sl + 4); buf[0] = sl >> 24; buf[1] = sl >> 16; buf[2] = sl >> 8; buf[3] = sl; memcpy(buf + 4, p + st[2], sl);
        CMBlockBufferRef bb = NULL; CMBlockBufferCreateWithMemoryBlock(NULL, buf, sl + 4, kCFAllocatorMalloc, NULL, 0, sl + 4, 0, &bb);
        CMSampleTimingInfo ti = { .duration = dur, .presentationTimeStamp = pts, .decodeTimeStamp = kCMTimeInvalid };
        size_t ssz = sl + 4; CMSampleBufferRef sb = NULL;
        OSStatus s2 = CMSampleBufferCreateReady(NULL, bb, fmt, 1, 1, &ti, 1, &ssz, &sb);
        OSStatus s3 = VTEncoderSessionEmitEncodedFrame(gSession, frame, 0, 0, sb);
        printf("  emit: fmt %d sample %d -> %d\n", (int)s1, (int)s2, (int)s3); fflush(stdout);
        CFRelease(sb); CFRelease(bb); CFRelease(fmt);
    }
    return 0;
}
static OSStatus t_complete(CMBaseObjectRef enc, CMTime until)
{
    printf("completeFrames: until %lld/%d flags %u\n", until.value, until.timescale, until.flags); fflush(stdout); return 0;
}
static OSStatus t_props(CMBaseObjectRef enc, CFDictionaryRef *out)
{
    // same shape as VTSessionCopySupportedPropertyDictionary's result
    NSDictionary *rw = @{ (__bridge NSString *)kVTPropertyTypeKey: (__bridge NSString *)kVTPropertyType_Number,
                          (__bridge NSString *)kVTPropertyReadWriteStatusKey: (__bridge NSString *)kVTPropertyReadWriteStatus_ReadWrite };
    NSDictionary *rwb = @{ (__bridge NSString *)kVTPropertyTypeKey: (__bridge NSString *)kVTPropertyType_Boolean,
                           (__bridge NSString *)kVTPropertyReadWriteStatusKey: (__bridge NSString *)kVTPropertyReadWriteStatus_ReadWrite };
    NSDictionary *d = @{ (__bridge NSString *)kVTCompressionPropertyKey_AverageBitRate: rw,
                         (__bridge NSString *)kVTCompressionPropertyKey_RealTime: rwb,
                         (__bridge NSString *)kVTCompressionPropertyKey_MaxKeyFrameInterval: rw };
    printf("copySupportedPropertyDictionary(%p)\n", out); fflush(stdout);
    *out = CFBridgingRetain(d);
    return 0;
}
#pragma pack(pop)
typedef struct { CMBaseVTable base; const ProbeEncoderClass *encoderClass; } ProbeEncoderVTable;

static OSStatus b_invalidate(CMBaseObjectRef o) { printf("base invalidate %p\n", o); return 0; }
static void b_finalize(CMBaseObjectRef o) { printf("base finalize %p\n", o); }
static CFStringRef b_desc(CMBaseObjectRef o) { return CFSTR("vtprobe encoder"); }
static OSStatus b_copyProperty(CMBaseObjectRef o, CFStringRef key, CFAllocatorRef al, void *out)
{
    printf("base copyProperty \"%s\"\n", [(__bridge NSString *)key UTF8String]); fflush(stdout);
    return kCMBaseObjectError_ValueNotAvailable;
}
static OSStatus b_setProperty(CMBaseObjectRef o, CFStringRef key, CFTypeRef v)
{
    printf("base setProperty \"%s\" = %s\n", [(__bridge NSString *)key UTF8String], [[(__bridge id)v description] UTF8String]); fflush(stdout);
    return 0;
}

static CMBaseClass gBase = { .version = 1, .derivedStorageSize = 256, .invalidate = b_invalidate, .finalize = b_finalize,
                             .copyDebugDescription = b_desc, .copyProperty = b_copyProperty, .setProperty = b_setProperty };
static ProbeEncoderClass gEnc;
static CMBaseProtocolTable gProto = { 1, 0, NULL };
static void *gZeros[64];
static Boolean b_equal(CMBaseObjectRef a, CMBaseObjectRef b) { return a == b; }
static OSStatus b_barrier(CMBaseObjectRef o) { return 0; }
static ProbeEncoderVTable gVT = { .base = { NULL, &gBase }, .encoderClass = &gEnc };

static OSStatus factory(uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5)
{
    uint64_t a[6] = { a0, a1, a2, a3, a4, a5 }; show("factory", 0, a);
    // the decoder's factory is (codecType, allocator, &instance); try the same: the out pointer is the last non-zero arg
    CMBaseObjectRef *out = (CMBaseObjectRef *)(a2 > 0x1000 ? a2 : a3);
    if (getenv("FACTORY_OUT")) out = (CMBaseObjectRef *)a[atoi(getenv("FACTORY_OUT"))];
    CMBaseObjectRef obj = NULL;
    OSStatus st = CMDerivedObjectCreate((CFAllocatorRef)a1, &gVT.base, VTVideoEncoderGetClassID(), &obj);
    printf("CMDerivedObjectCreate -> %d, %p (out pointer %p)\n", (int)st, obj, out); fflush(stdout);
    if (!st) *out = obj;
    return st;
}

int main(int argc, char **argv) { @autoreleasepool {
    gEnc.version = argc > 1 ? (CMBaseClassVersion)atoi(argv[1]) : 1;
    if (getenv("BASE_VERSION")) gBase.version = (CMBaseClassVersion)atoi(getenv("BASE_VERSION"));
    if (getenv("PROTO")) gBase.protocolTable = &gProto;
    if (getenv("EQUAL")) { gBase.equal = b_equal; gBase.notificationBarrier = b_barrier; }
    if (getenv("RESERVED")) gVT.base.reserved = !strcmp(getenv("RESERVED"), "enc") ? (const void *)&gEnc : (const void *)gZeros;
    gRet = argc > 2 ? atoi(argv[2]) : 0;
    void *s[NSLOTS] = { slot0, slot1, slot2, slot3, slot4, slot5, slot6, slot7, slot8, slot9, slot10, slot11,
                        slot12, slot13, slot14, slot15, slot16, slot17, slot18, slot19, slot20, slot21, slot22, slot23 };
    memcpy(gEnc.slots, s, sizeof s);
    if (getenv("TYPED")) {
        gEnc.slots[0] = t_start; gEnc.slots[1] = t_encode; gEnc.slots[2] = t_complete; gEnc.slots[3] = t_props;
        if (getenv("NULL4")) for (int i = 4; i < NSLOTS; i++) gEnc.slots[i] = NULL;   // like nullmoth's class: four functions
    }
    NSDictionary *info = @{ @"CMClassImplementationID": @"org.nvenc.vtprobe", @"CMClassImplementationName": @"vtprobe stub",
                            @"VTRating": @2000, @"VTIsHardwareAccelerated": @YES };
    OSStatus st = VTRegisterVideoEncoderWithInfo('avc1', (__bridge CFDictionaryRef)info, (void *)factory);
    printf("register -> %d (class version %d, slots return %d)\n", (int)st, (int)gEnc.version, gRet);
    NSDictionary *spec = @{ (__bridge NSString *)kVTVideoEncoderSpecification_EncoderID: @"org.nvenc.vtprobe" };
    VTCompressionSessionRef cs = NULL;
    st = VTCompressionSessionCreate(NULL, 256, 128, kCMVideoCodecType_H264, (__bridge CFDictionaryRef)spec, NULL, NULL, NULL, NULL, &cs);
    printf("VTCompressionSessionCreate -> %d\n", (int)st); fflush(stdout);
    if (st) return 1;
    printf("-- set AverageBitRate, RealTime\n"); fflush(stdout);
    printf("   -> %d\n", (int)VTSessionSetProperty(cs, kVTCompressionPropertyKey_AverageBitRate, (__bridge CFTypeRef)@2000000));
    printf("   -> %d\n", (int)VTSessionSetProperty(cs, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue));
    printf("-- prepare\n"); fflush(stdout);
    printf("   -> %d\n", (int)VTCompressionSessionPrepareToEncodeFrames(cs));
    CVPixelBufferRef pb = NULL;
    NSDictionary *attr = @{ (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{} };
    CVPixelBufferCreate(NULL, 256, 128, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, (__bridge CFDictionaryRef)attr, &pb);
    printf("pixel buffer %p\n", pb);
    for (int f = 0; f < 2; f++) {
        printf("-- encode frame %d\n", f); fflush(stdout);
        st = VTCompressionSessionEncodeFrameWithOutputHandler(cs, pb, CMTimeMake(f, 60), CMTimeMake(1, 60), NULL, NULL,
            ^(OSStatus s2, VTEncodeInfoFlags fl, CMSampleBufferRef sb) {
                printf("   output handler: status %d flags %#x sample %p, %zu bytes, pts %lld\n", (int)s2, fl, sb, sb ? CMSampleBufferGetTotalSampleSize(sb) : 0,
                       sb ? CMSampleBufferGetPresentationTimeStamp(sb).value : -1); fflush(stdout); });
        printf("   -> %d\n", (int)st);
    }
    printf("-- complete\n"); fflush(stdout);
    printf("   -> %d\n", (int)VTCompressionSessionCompleteFrames(cs, kCMTimeInvalid));
    printf("-- invalidate\n"); fflush(stdout);
    VTCompressionSessionInvalidate(cs); CFRelease(cs);
    printf("done\n");
    return 0;
} }
