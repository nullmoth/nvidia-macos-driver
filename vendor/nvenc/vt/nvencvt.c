// nvencvt.c: a VideoToolbox H.264 encoder on top of src/enc.c (M4). Interface: docs/vt-encoder.md.
// Built as libnvencvt.dylib. Injected, it registers the encoder in the process (constructor), e.g.
//   DYLD_INSERT_LIBRARIES=~/nvenc-dev/libnvencvt.dylib ./vttest hw avc1 1920 1080 60
// Loaded otherwise (the Metal plugin with NVMTL_NVENCVT set), call NVEncVT_Register(rating), or register
// NVEncVT_CreateInstance as the factory yourself. Environment: NVENCVT_RATING (default 1100,
// above nullmoth's 1000), NVENCVT_NO_REGISTER, NVENCVT_LOG, NVENCVT_DEPTH, NVENCVT_ENGINE, NVENCVT_STATS=<file> (or touch
// /tmp/nvencvt.stats: statistics every 2 s to /tmp/nvencvt-stats.log, also in apps that get no environment).
#include "enc.h"
#include "av1hdr.h"
#include "rm.h"
#include "ctrl/ctrl0080/ctrl0080gpu.h"
#include "clc9b7.h"
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <dlfcn.h>
#include <execinfo.h>
#include <objc/runtime.h>
#include <malloc/malloc.h>
#include <mach-o/dyld.h>
#include <mach-o/getsect.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/stat.h>

// ---- CMBaseObject / encoder class declarations (WebKit's CMBaseObjectSPI.h, written for C++) ----
typedef struct CMBaseProtocolVTable CMBaseProtocolVTable;
typedef struct CMBaseProtocolTable CMBaseProtocolTable;
#include "CMBaseObjectSPI.h"

#ifndef NVENC_VERSION
#define NVENC_VERSION "dev"   // build.sh passes git describe --tags --always --dirty
#endif
typedef CMBaseObjectRef VTVideoEncoderRef;
typedef void *VTEncoderSessionRef;
typedef void *VTEncoderFrameRef;
extern CMBaseClassID VTVideoEncoderGetClassID(void);
extern OSStatus VTRegisterVideoEncoderWithInfo(FourCharCode, CFDictionaryRef, void *);
extern OSStatus VTEncoderSessionEmitEncodedFrame(VTEncoderSessionRef, VTEncoderFrameRef, OSStatus, VTEncodeInfoFlags, CMSampleBufferRef);
extern OSStatus VTEncoderSessionSetPixelBufferAttributes(VTEncoderSessionRef, CFDictionaryRef);

#pragma pack(push, 4)
typedef struct {
    uint32_t version;
    OSStatus (*startSession)(VTVideoEncoderRef, VTEncoderSessionRef, CMVideoDimensions);
    OSStatus (*encodeFrame)(VTVideoEncoderRef, VTEncoderFrameRef, CVImageBufferRef, CMTime, CMTime, CFDictionaryRef, VTEncodeInfoFlags *);
    OSStatus (*completeFrames)(VTVideoEncoderRef, CMTime);
    OSStatus (*copySupportedPropertyDictionary)(VTVideoEncoderRef, CFDictionaryRef *);
    void *reserved[8];
} EncoderClass;
#pragma pack(pop)
typedef struct { CMBaseVTable base; const EncoderClass *encoderClass; } EncoderVTable;

#define ENCODER_ID "org.nvenc.videoencoder.avc"
#define ENCODER_ID_HEVC "org.nvenc.videoencoder.hevc"
#define ENCODER_ID_AV1 "org.nvenc.videoencoder.av1"
enum { CODEC_AV1 = 0x61763031 };   // 'av01' (kCMVideoCodecType_AV1)
static bool gLog;
static FILE *gLogF;   // NVENCVT_LOG: stderr; flag file /tmp/nvencvt.log (apps started by launchd): /tmp/nvencvt-<pid>.log
#define LOG(...) do { if (gLog) { FILE *lf_ = gLogF ? gLogF : stderr; fprintf(lf_, "nvencvt: " __VA_ARGS__); fputc('\n', lf_); fflush(lf_); } } while (0)

enum { VT_MAX_B = 3 };
enum { GPU_NONE = 0, GPU_ADA = 1, GPU_TU117 = 2, GPU_AMPERE = 3 };   // the GPUs the codecs register on
extern int gNvencvtGpu;                               // which of them (set by NVEncVT_Register)

typedef struct {
    pthread_mutex_t lock;
    VTEncoderSessionRef session;
    int width, height;
    bool hevc;                      // hvc1 session (else avc1)
    bool av1;                       // av01 session (M12, T12.05)
    bool ten;                       // HEVC Main 10 (ProfileLevel HEVC_Main10_*): 10-bit input, 10-bit stream
    int h264Profile;                // ProfileLevel asked for: 66 Baseline, 77 Main, 100 High; 0 = not set (High)
    int entropy;                    // H264EntropyMode asked for: 0 CAVLC, 1 CABAC, -1 not set
    bool fullRange;                 // input 420f / xf20 (full range, encoded as is): VUI video_full_range_flag (T7.01)
    bool c444;                      // first frame 444v / 444f (or x444 / xf44 for HEVC Main 10): 4:4:4 stream (M13)
    bool opened, failed;
    int engine;
    Enc enc;
    CMVideoFormatDescriptionRef fmt;
    // properties
    int bitrate, maxKeyInterval, fps, quality;
    bool fpsKnown;                  // fps from ExpectedFrameRate or the first frame's duration; else taken from the first two
    CMTime firstPts;                // time stamps (ffmpeg sets neither: a 50 fps stream got the bit budget of 60 fps)
    int refs;                       // reference frames (ReferenceBufferCount; 0 = default)
    int maxRate, maxVbv;            // DataRateLimits: the first limit as bit/s and its window in bits (0 = none): VBR up to it
    bool cbr, speed;                // ConstantBitRate set (bitrate = it); PrioritizeEncodingSpeedOverQuality
    // colour description (H.273 codes; 0 = not given): the ColorPrimaries / TransferFunction / YCbCrMatrix properties, else the
    // first frame's attachments; HDR metadata as the SEI payloads (MasteringDisplayColorVolume 24 bytes, ContentLightLevelInfo 4)
    int colour[3];
    uint8_t mdcv[24], cll[4]; bool hasMdcv, hasCll;
    // diagnostic dump (flag file /tmp/nvencvt.dump): encoded stream from a forced IDR on, and the first 30 input frames
    FILE *dumpEs, *dumpYuv; int dumpYuvLeft; bool dumpIdr;
    bool realTime;
    bool reorder;                   // AllowFrameReordering (true unless the client turns it off)
    unsigned frames;
    // B pictures (T7.03; AllowFrameReordering true and RealTime not true, H.264 Main/High 4:2:0 and HEVC): up to bf
    // input frames wait (retained) for the anchor after them; the anchor is encoded first, then they are as B pictures
    int bf;                         // B pictures between anchors, 0 = off
    struct { VTEncoderFrameRef frame; CVPixelBufferRef img; CMTime pts, dur; double tEntry; int disp; } pend[VT_MAX_B + 1];
    int npend, disp, sinceKey;
    CMTime ptsRing[64];             // input presentation times by display index: decode time stamps come from them
    unsigned coded;                 // pictures submitted (= the decode position of the next one)
    // frames in flight: encodeFrame submits, the completion thread waits (in order) and emits
    struct { VTEncoderFrameRef frame; CMTime pts, dur, dts; NvU32 ticket; bool ok, b, showAfter; double tEntry, tCall, tTile; } q[ENC_MAX_DEPTH];
    // AV1 with B pictures: the hidden anchor's frame, emitted (as the temporal unit that shows it) after the last B picture
    // before it (q[].showAfter); AV1 temporal units stay in display order, so no decode time stamps
    struct { VTEncoderFrameRef frame; CMTime pts, dur; bool valid; } hid;
    // statistics (NVENCVT_STATS=<file> or the flag file /tmp/nvencvt.stats: a line every 2 s to that file /
    // /tmp/nvencvt-stats.log)
    FILE *bcFile; int bcX, bcY, bcS; long bcLast; unsigned bcN, bcBack, bcRep, bcSkip; double bcT0; unsigned bcSid, bcSeed, bcSeedPre; double bcPts;
    struct { unsigned sid, seed; long v; double t, pts; } bcRing[16]; unsigned bcRingN; unsigned bcSeen[32], bcSeenN; int bcAll;   // frame-counter check (diagnostics)
    FILE *statsFile; double statsT0; unsigned sN, sIdr, sFail; double sCall, sCallMax, sTile, sEng, sEngMax, sTotal, sTotalMax, sBytes;
    OSType lastPf;
    unsigned qh, qt, depth;
    pthread_t thread; bool threadOn, stop;
    pthread_cond_t cv;
} State;

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

static void stats_print(State *s);
static void stats_frame(State *s, double call, double tile, double eng, double total, size_t bytes, bool idr, bool fail)
{
    if (!s->statsFile) return;
    s->sN++; s->sIdr += idr; s->sFail += fail; s->sBytes += bytes;
    s->sCall += call; if (call > s->sCallMax) s->sCallMax = call;
    s->sTile += tile; s->sEng += eng; if (eng > s->sEngMax) s->sEngMax = eng;
    s->sTotal += total; if (total > s->sTotalMax) s->sTotalMax = total;
    if (now_ms() - s->statsT0 >= 2000) stats_print(s);
}

static void stats_print(State *s)
{
    if (!s->statsFile || !s->sN) return;
    double t = now_ms();
    double sec = (t - s->statsT0) / 1000, n = s->sN;
    char pf[5] = { (char)(s->lastPf >> 24), (char)(s->lastPf >> 16), (char)(s->lastPf >> 8), (char)s->lastPf, 0 };
    time_t now = time(NULL); char ts[32]; strftime(ts, sizeof ts, "%H:%M:%S", localtime(&now));
    fprintf(s->statsFile, "%s %s pid %d %dx%d %s NVENC%d: %.1f fps, call %.3f ms (max %.3f; tiling %.3f), engine %.3f ms (max %.3f), "
            "entry->emit %.3f ms (max %.3f), %.1f KB/frame, %u IDR, %u failed, rate %d bit/s rt %d\n", ts, getprogname(), getpid(),
            s->width, s->height, pf, s->engine, n / sec, s->sCall / n, s->sCallMax, s->sTile / n, s->sEng / n, s->sEngMax,
            s->sTotal / n, s->sTotalMax, s->sBytes / n / 1024, s->sIdr, s->sFail, s->bitrate, s->realTime);
    fflush(s->statsFile);
    s->statsT0 = t; s->sN = s->sIdr = s->sFail = 0; s->sCall = s->sCallMax = s->sTile = s->sEng = s->sEngMax = s->sTotal = s->sTotalMax = s->sBytes = 0;
}

static int gEngineUse[2];   // active sessions per NVENC engine in this process (guarded by gEngineLock)
static pthread_mutex_t gEngineLock = PTHREAD_MUTEX_INITIALIZER;

static State *state(VTVideoEncoderRef e) { return *(State **)CMBaseObjectGetDerivedStorage(e); }

// ---- properties ----
static CFDictionaryRef gSupported;
static CFDictionaryRef supported(void)
{
    static pthread_once_t once = PTHREAD_ONCE_INIT;
    void build(void);
    pthread_once(&once, build);
    return gSupported;
}
static CFDictionaryRef prop(CFStringRef type, CFStringRef rw)
{
    const void *k[] = { kVTPropertyTypeKey, kVTPropertyReadWriteStatusKey }, *v[] = { type, rw };
    return CFDictionaryCreate(NULL, k, v, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}
void build(void)
{
    CFMutableDictionaryRef d = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionaryRef num = prop(kVTPropertyType_Number, kVTPropertyReadWriteStatus_ReadWrite);
    CFDictionaryRef bol = prop(kVTPropertyType_Boolean, kVTPropertyReadWriteStatus_ReadWrite);
    CFDictionaryRef enm = prop(kVTPropertyType_Enumeration, kVTPropertyReadWriteStatus_ReadWrite);
    CFDictionaryRef numRO = prop(kVTPropertyType_Number, kVTPropertyReadWriteStatus_ReadOnly);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_AverageBitRate, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_MaxKeyFrameInterval, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ExpectedFrameRate, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_Quality, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ReferenceBufferCount, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_RealTime, bol);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_AllowFrameReordering, bol);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ProfileLevel, enm);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_H264EntropyMode, enm);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_DataRateLimits, prop(kVTPropertyType_Enumeration, kVTPropertyReadWriteStatus_ReadWrite));
    CFDictionarySetValue(d, kVTCompressionPropertyKey_NumberOfPendingFrames, numRO);
    // colour description and HDR metadata (T7.05), exact CBR and speed over quality (macOS 12-13 keys)
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ColorPrimaries, enm);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_TransferFunction, enm);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_YCbCrMatrix, enm);
    CFDictionaryRef dat = prop(kVTPropertyType_Enumeration, kVTPropertyReadWriteStatus_ReadWrite);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_MasteringDisplayColorVolume, dat);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ContentLightLevelInfo, dat);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_ConstantBitRate, num);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, bol);
    CFDictionarySetValue(d, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder, prop(kVTPropertyType_Boolean, kVTPropertyReadWriteStatus_ReadOnly));
    gSupported = d;
}

static int num(CFTypeRef v, int dflt)
{
    int x = dflt;
    if (v && CFGetTypeID(v) == CFNumberGetTypeID()) { double f; CFNumberGetValue(v, kCFNumberDoubleType, &f); x = (int)f; }
    return x;
}

// Profiles we produce: H.264 Baseline/Main/High (a P-only CAVLC Main stream decodes on all three) at every level,
// HEVC Main and Main 10 (T6.14). Registered as SupportedSelectionProperties, so VideoToolbox picks another encoder
// (Apple's) when an app asks for a profile not listed (HEVC Main10, H.264 High 4:2:2 ...), as Apple's encoders declare.
static CFArrayRef profiles(bool hevc)
{
    static CFArrayRef list[2]; static dispatch_once_t once;
    dispatch_once(&once, ^{
        CFMutableArrayRef h = CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks), v = CFArrayCreateMutable(NULL, 0, &kCFTypeArrayCallBacks);
        static const char *prof[] = { "Baseline", "Main", "High" };
        static const char *lev[] = { "1_3", "3_0", "3_1", "3_2", "4_0", "4_1", "4_2", "5_0", "5_1", "5_2", "AutoLevel" };
        for (int i = 0; i < 3; i++) for (int j = 0; j < 11; j++) {
            CFStringRef x = CFStringCreateWithFormat(NULL, NULL, CFSTR("H264_%s_%s"), prof[i], lev[j]); CFArrayAppendValue(h, x); CFRelease(x);
        }
        CFArrayAppendValue(h, CFSTR("H264_ConstrainedBaseline_AutoLevel")); CFArrayAppendValue(h, CFSTR("H264_ConstrainedHigh_AutoLevel"));
        CFArrayAppendValue(v, kVTProfileLevel_HEVC_Main_AutoLevel); CFArrayAppendValue(v, kVTProfileLevel_HEVC_Main10_AutoLevel);
        list[0] = h; list[1] = v;
    });
    return list[hevc];
}
static bool profile_ok(const State *s, CFTypeRef v)
{
    CFArrayRef l = profiles(s->hevc);
    return v && CFGetTypeID(v) == CFStringGetTypeID() && CFArrayContainsValue(l, CFRangeMake(0, CFArrayGetCount(l)), v);
}

static OSStatus set_pixel_attrs(State *s);

// CoreVideo colour strings <-> ITU-T H.273 codes (kind 0 primaries, 1 transfer, 2 matrix)
static const struct { int kind, code; const CFStringRef *str; } gColour[] = {
    { 0, 1, &kCVImageBufferColorPrimaries_ITU_R_709_2 }, { 0, 5, &kCVImageBufferColorPrimaries_EBU_3213 }, { 0, 6, &kCVImageBufferColorPrimaries_SMPTE_C },
    { 0, 9, &kCVImageBufferColorPrimaries_ITU_R_2020 }, { 0, 11, &kCVImageBufferColorPrimaries_DCI_P3 }, { 0, 12, &kCVImageBufferColorPrimaries_P3_D65 },
    { 1, 1, &kCVImageBufferTransferFunction_ITU_R_709_2 }, { 1, 7, &kCVImageBufferTransferFunction_SMPTE_240M_1995 }, { 1, 8, &kCVImageBufferTransferFunction_Linear },
    { 1, 13, &kCVImageBufferTransferFunction_sRGB }, { 1, 14, &kCVImageBufferTransferFunction_ITU_R_2020 }, { 1, 16, &kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ },
    { 1, 17, &kCVImageBufferTransferFunction_SMPTE_ST_428_1 }, { 1, 18, &kCVImageBufferTransferFunction_ITU_R_2100_HLG },
    { 2, 1, &kCVImageBufferYCbCrMatrix_ITU_R_709_2 }, { 2, 6, &kCVImageBufferYCbCrMatrix_ITU_R_601_4 }, { 2, 7, &kCVImageBufferYCbCrMatrix_SMPTE_240M_1995 },
    { 2, 9, &kCVImageBufferYCbCrMatrix_ITU_R_2020 } };
static int colour_code(int kind, CFTypeRef v)
{
    if (!v || CFGetTypeID(v) != CFStringGetTypeID()) return 0;
    for (size_t i = 0; i < sizeof gColour / sizeof gColour[0]; i++) if (gColour[i].kind == kind && CFEqual(v, *gColour[i].str)) return gColour[i].code;
    return 0;
}
static CFStringRef colour_str(int kind, int code)
{
    for (size_t i = 0; i < sizeof gColour / sizeof gColour[0]; i++) if (gColour[i].kind == kind && gColour[i].code == code) return *gColour[i].str;
    return NULL;
}
static bool copy_data(CFTypeRef v, uint8_t *dst, CFIndex n)
{
    if (!v || CFGetTypeID(v) != CFDataGetTypeID() || CFDataGetLength(v) != n) return false;
    CFDataGetBytes(v, CFRangeMake(0, n), dst); return true;
}

static OSStatus b_setProperty(CMBaseObjectRef o, CFStringRef key, CFTypeRef v)
{
    State *s = state(o);
    pthread_mutex_lock(&s->lock);
    OSStatus st = noErr;
    if (CFEqual(key, kVTCompressionPropertyKey_AverageBitRate)) s->bitrate = num(v, 0);
    else if (CFEqual(key, kVTCompressionPropertyKey_MaxKeyFrameInterval)) s->maxKeyInterval = num(v, 0);
    else if (CFEqual(key, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration)) { if (!s->maxKeyInterval && num(v, 0) > 0) s->maxKeyInterval = num(v, 0) * (s->fps ? s->fps : 60); }
    else if (CFEqual(key, kVTCompressionPropertyKey_ExpectedFrameRate)) { s->fps = num(v, 0); s->fpsKnown = s->fps > 0; }
    else if (CFEqual(key, kVTCompressionPropertyKey_ReferenceBufferCount)) s->refs = num(v, 0);
    else if (CFEqual(key, kVTCompressionPropertyKey_Quality)) { double q = 0; if (v && CFGetTypeID(v) == CFNumberGetTypeID()) CFNumberGetValue(v, kCFNumberDoubleType, &q); s->quality = (int)(q * 100); }
    else if (CFEqual(key, kVTCompressionPropertyKey_RealTime)) s->realTime = v && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v);
    else if (CFEqual(key, kVTCompressionPropertyKey_AllowFrameReordering)) s->reorder = v && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v);
    else if (CFEqual(key, kVTCompressionPropertyKey_ProfileLevel) && !profile_ok(s, v)) st = kVTPropertyNotSupportedErr;
    else if (CFEqual(key, kVTCompressionPropertyKey_ProfileLevel) && !s->hevc && !s->av1) {
        char pr[64] = ""; CFStringGetCString(v, pr, sizeof pr, kCFStringEncodingUTF8);
        s->h264Profile = strstr(pr, "Baseline") ? 66 : strstr(pr, "High") ? 100 : 77;
    }
    else if (CFEqual(key, kVTCompressionPropertyKey_H264EntropyMode) && v && CFGetTypeID(v) == CFStringGetTypeID())
        s->entropy = CFEqual(v, kVTH264EntropyMode_CABAC) ? 1 : 0;
    else if (CFEqual(key, kVTCompressionPropertyKey_ProfileLevel) && s->hevc) {
        bool ten = CFEqual(v, kVTProfileLevel_HEVC_Main10_AutoLevel);
        if (ten != s->ten && !s->opened) { s->ten = ten; set_pixel_attrs(s); }
    }
    else if (CFEqual(key, kVTCompressionPropertyKey_DataRateLimits)) {   // [bytes, seconds, ...]: the first pair caps the rate (VBR)
        s->maxRate = 0;
        if (v && CFGetTypeID(v) == CFArrayGetTypeID() && CFArrayGetCount(v) >= 2) {
            double bytes = 0, sec = 0; CFTypeRef b0 = CFArrayGetValueAtIndex(v, 0), s0 = CFArrayGetValueAtIndex(v, 1);
            if (CFGetTypeID(b0) == CFNumberGetTypeID() && CFGetTypeID(s0) == CFNumberGetTypeID()) {
                CFNumberGetValue(b0, kCFNumberDoubleType, &bytes); CFNumberGetValue(s0, kCFNumberDoubleType, &sec);
                if (bytes > 0 && sec > 0) { s->maxRate = (int)(bytes * 8 / sec > 2e9 ? 2e9 : bytes * 8 / sec); s->maxVbv = (int)(bytes * 8 > 2e9 ? 2e9 : bytes * 8); }
            }
        }
    }
    else if (CFEqual(key, kVTCompressionPropertyKey_ConstantBitRate)) { s->bitrate = num(v, 0); s->cbr = s->bitrate > 0; }
    else if (CFEqual(key, kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality)) s->speed = v && CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v);
    else if (CFEqual(key, kVTCompressionPropertyKey_ColorPrimaries)) s->colour[0] = colour_code(0, v);
    else if (CFEqual(key, kVTCompressionPropertyKey_TransferFunction)) s->colour[1] = colour_code(1, v);
    else if (CFEqual(key, kVTCompressionPropertyKey_YCbCrMatrix)) s->colour[2] = colour_code(2, v);
    else if (CFEqual(key, kVTCompressionPropertyKey_MasteringDisplayColorVolume)) s->hasMdcv = copy_data(v, s->mdcv, 24);
    else if (CFEqual(key, kVTCompressionPropertyKey_ContentLightLevelInfo)) s->hasCll = copy_data(v, s->cll, 4);
    else if (CFEqual(key, kVTCompressionPropertyKey_ProfileLevel) || CFEqual(key, kVTCompressionPropertyKey_H264EntropyMode)) { /* accepted */ }
    else st = kVTPropertyNotSupportedErr;
    if (gLog) { CFStringRef d = v ? CFCopyDescription(v) : NULL; char k[128], vb[256] = "";
        CFStringGetCString(key, k, sizeof k, kCFStringEncodingUTF8); if (d) { CFStringGetCString(d, vb, sizeof vb, kCFStringEncodingUTF8); CFRelease(d); }
        LOG("set %s = %s -> %d", k, vb, (int)st); }
    pthread_mutex_unlock(&s->lock);
    return st;
}

// H.264 profile and entropy coding: High + CABAC (+ 8x8 transform) unless the client asks for less: Baseline gets Main
// with CAVLC (a P-only CAVLC stream; T7.02), Main gets CABAC unless H264EntropyMode says CAVLC
static bool h264_high(const State *s) { return s->h264Profile == 0 || s->h264Profile == 100; }
static bool h264_cabac(const State *s) { return s->h264Profile != 66 && s->entropy != 0; }

static OSStatus b_copyProperty(CMBaseObjectRef o, CFStringRef key, CFAllocatorRef al, void *out)
{
    State *s = state(o); CFTypeRef v = NULL; int n;
    if (CFEqual(key, kVTCompressionPropertyKey_AverageBitRate)) { n = s->bitrate; v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_MaxKeyFrameInterval)) { n = s->maxKeyInterval; v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_ExpectedFrameRate)) { n = s->fps; v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_ReferenceBufferCount)) { n = s->enc.cfg.num_refs ? s->enc.cfg.num_refs : (s->refs ? s->refs : ENC_MAX_REFS); v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_RealTime)) v = CFRetain(s->realTime ? kCFBooleanTrue : kCFBooleanFalse);
    else if (CFEqual(key, kVTCompressionPropertyKey_AllowFrameReordering)) v = CFRetain((s->opened ? s->bf > 0 : s->reorder) ? kCFBooleanTrue : kCFBooleanFalse);
    else if (CFEqual(key, kVTCompressionPropertyKey_ProfileLevel) && !s->av1) v = CFRetain(s->hevc ? (s->ten ? kVTProfileLevel_HEVC_Main10_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel)
                                                                             : h264_high(s) ? kVTProfileLevel_H264_High_AutoLevel : kVTProfileLevel_H264_Main_AutoLevel);
    else if (CFEqual(key, kVTCompressionPropertyKey_H264EntropyMode) && !s->hevc && !s->av1) v = CFRetain(h264_cabac(s) ? kVTH264EntropyMode_CABAC : kVTH264EntropyMode_CAVLC);
    else if (CFEqual(key, kVTCompressionPropertyKey_ConstantBitRate)) { n = s->cbr ? s->bitrate : 0; v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality)) v = CFRetain(s->speed ? kCFBooleanTrue : kCFBooleanFalse);
    else if ((CFEqual(key, kVTCompressionPropertyKey_ColorPrimaries) && colour_str(0, s->colour[0])) || (CFEqual(key, kVTCompressionPropertyKey_TransferFunction) && colour_str(1, s->colour[1])) ||
             (CFEqual(key, kVTCompressionPropertyKey_YCbCrMatrix) && colour_str(2, s->colour[2])))
        v = CFRetain(colour_str(CFEqual(key, kVTCompressionPropertyKey_ColorPrimaries) ? 0 : CFEqual(key, kVTCompressionPropertyKey_TransferFunction) ? 1 : 2,
                                s->colour[CFEqual(key, kVTCompressionPropertyKey_ColorPrimaries) ? 0 : CFEqual(key, kVTCompressionPropertyKey_TransferFunction) ? 1 : 2]));
    else if (CFEqual(key, kVTCompressionPropertyKey_MasteringDisplayColorVolume) && s->hasMdcv) v = CFDataCreate(al, s->mdcv, 24);
    else if (CFEqual(key, kVTCompressionPropertyKey_ContentLightLevelInfo) && s->hasCll) v = CFDataCreate(al, s->cll, 4);
    else if (CFEqual(key, kVTCompressionPropertyKey_NumberOfPendingFrames)) { n = s->npend; v = CFNumberCreate(al, kCFNumberIntType, &n); }
    else if (CFEqual(key, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder)) v = CFRetain(kCFBooleanTrue);
    else { if (gLog) { char k[128]; CFStringGetCString(key, k, sizeof k, kCFStringEncodingUTF8); LOG("copy %s: not available", k); } return kCMBaseObjectError_ValueNotAvailable; }
    *(CFTypeRef *)out = v;
    return noErr;
}

// the input formats we take: NV12 or BGRA (BGRA is converted by us, in the tiling pass, much faster than VideoToolbox's
// conversion: T5.05); a Main 10 session takes 16-bit 4:2:0 (x420 / xf20, P010 layout). Set again when the profile changes.
static OSStatus set_pixel_attrs(State *s)
{
    if (!s->session) return noErr;
    int f[12] = { kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVPixelFormatType_32BGRA };
    // Main 10: P010 or l10r (ScreenCaptureKit's 10-bit RGB, what Parsec sends; converted by us: else VideoToolbox converts it
    // through Metal on its serial queue, 20+ ms per 4K frame and frames queued behind it, T6.14)
    if (s->ten) { f[0] = kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange; f[1] = kCVPixelFormatType_420YpCbCr10BiPlanarFullRange; f[2] = kCVPixelFormatType_ARGB2101010LEPacked; }
    int nf = 3;
    if (s->av1) {   // AV1: 8 or 10-bit, decided by the first frame (VideoToolbox converts only buffers in none of these)
        f[0] = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange; f[1] = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange; f[2] = kCVPixelFormatType_32BGRA;
        f[3] = kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange; f[4] = kCVPixelFormatType_420YpCbCr10BiPlanarFullRange; f[5] = kCVPixelFormatType_ARGB2101010LEPacked;
        nf = 6;
    } else {   // 4:4:4 when the input is (M13): H.264 High 4:4:4 Predictive 8-bit, HEVC Main 4:4:4 (10)
        f[nf++] = s->ten ? kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange : kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange;
        f[nf++] = s->ten ? kCVPixelFormatType_444YpCbCr10BiPlanarFullRange : kCVPixelFormatType_444YpCbCr8BiPlanarFullRange;
    }
    if (access("/tmp/nvencvt.anyfmt", F_OK) == 0) {   // diagnostics: offer every format a capture source might deliver, log what comes
        int all[] = { kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
                      kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, kCVPixelFormatType_32BGRA,
                      kCVPixelFormatType_ARGB2101010LEPacked, kCVPixelFormatType_64RGBAHalf, kCVPixelFormatType_30RGB,
                      kCVPixelFormatType_64ARGB, kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange, kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange, kCVPixelFormatType_32ARGB };
        nf = 12; memcpy(f, all, sizeof all);
    }
    CFNumberRef n[12]; for (int i = 0; i < nf; i++) n[i] = CFNumberCreate(NULL, kCFNumberIntType, &f[i]);
    CFArrayRef fmts = CFArrayCreate(NULL, (const void **)n, nf, &kCFTypeArrayCallBacks);
    CFNumberRef w = CFNumberCreate(NULL, kCFNumberIntType, &s->width), h = CFNumberCreate(NULL, kCFNumberIntType, &s->height);
    CFDictionaryRef ios = CFDictionaryCreate(NULL, NULL, NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *k[] = { kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey };
    const void *v[] = { fmts, w, h, ios };
    CFDictionaryRef attrs = CFDictionaryCreate(NULL, k, v, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus st = VTEncoderSessionSetPixelBufferAttributes(s->session, attrs);
    CFRelease(attrs); CFRelease(ios); CFRelease(w); CFRelease(h); CFRelease(fmts); for (int i = 0; i < nf; i++) CFRelease(n[i]);
    return st;
}

// ---- encoder functions ----
static OSStatus e_startSession(VTVideoEncoderRef e, VTEncoderSessionRef session, CMVideoDimensions dims)
{
    State *s = state(e);
    s->session = session; s->width = dims.width; s->height = dims.height;
    // NVENC 6.6 (GTX 1650) encodes H.264 up to 4096x4096 (larger: a microcode error on every frame, T15.11); HEVC goes to 8192x8192
    if (gNvencvtGpu == GPU_TU117 && !s->hevc && !s->av1 && (s->width > 4096 || s->height > 4096)) {
        LOG("startSession %dx%d: H.264 on the GTX 1650 stops at 4096x4096", s->width, s->height);
        return kVTCouldNotFindVideoEncoderErr;
    }
    OSStatus st = set_pixel_attrs(s);
    LOG("startSession %dx%d (pixel buffer attributes -> %d)", s->width, s->height, (int)st);
    return noErr;
}

static void *completion(void *arg);

// The format description gets the colour description and HDR metadata too, so AVAssetWriter writes colr / mdcv / clli
// (and players read them without parsing the stream)
static void add_fmt_extensions(State *s)
{
    CFDictionaryRef ext = CMFormatDescriptionGetExtensions(s->fmt);
    CFMutableDictionaryRef m = ext ? CFDictionaryCreateMutableCopy(NULL, 0, ext) : CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const CFStringRef keys[3] = { kCMFormatDescriptionExtension_ColorPrimaries, kCMFormatDescriptionExtension_TransferFunction, kCMFormatDescriptionExtension_YCbCrMatrix };
    for (int k = 0; k < 3; k++) { CFStringRef v = colour_str(k, s->enc.cfg.colour[k] ? s->enc.cfg.colour[k] : 1); if (v) CFDictionarySetValue(m, keys[k], v); }
    if (s->enc.cfg.has_mdcv) { CFDataRef d = CFDataCreate(NULL, s->enc.cfg.mdcv, 24); CFDictionarySetValue(m, kCMFormatDescriptionExtension_MasteringDisplayColorVolume, d); CFRelease(d); }
    if (s->enc.cfg.has_cll) { CFDataRef d = CFDataCreate(NULL, s->enc.cfg.cll, 4); CFDictionarySetValue(m, kCMFormatDescriptionExtension_ContentLightLevelInfo, d); CFRelease(d); }
    CMVideoDimensions dm = CMVideoFormatDescriptionGetDimensions(s->fmt);
    CMVideoFormatDescriptionRef f2 = NULL;
    if (!CMVideoFormatDescriptionCreate(NULL, CMFormatDescriptionGetMediaSubType(s->fmt), dm.width, dm.height, m, &f2)) { CFRelease(s->fmt); s->fmt = f2; }
    CFRelease(m);
}

static OSStatus open_encoder(State *s)
{
    // reference frames (every codec): ReferenceBufferCount if the client set it, else NVENCVT_REFS, else 3. Pictures that come back
    // (window switching, blinking content) cost far less with more references: 1080p, three pictures cycling, HEVC:
    // 27.8 Mbit/s with one, 1.7 with three (T6.11); steady motion about -2 %, +0.1 ms per frame. No reordering, no added latency.
    int refs = s->refs > 0 ? s->refs : getenv("NVENCVT_REFS") ? atoi(getenv("NVENCVT_REFS")) : 3;
    { FILE *rf = fopen("/tmp/nvencvt.refs", "r"); int v = 0;   // diagnostics: overrides everything, read at each session start
      if (rf) { if (fscanf(rf, "%d", &v) == 1 && v > 0) refs = v; fclose(rf); } }
    refs = refs < 1 ? 1 : refs > ENC_MAX_REFS ? ENC_MAX_REFS : refs;
    // HEVC: announce 2 pictures more than the references need in the VPS/SPS DPB size. With exactly refs + 1, a hardware
    // decoder (AMD Polaris through macOS Sonoma, Parsec client) overwrote references still in use: moving windows showed
    // content ahead of the background (T6.12). NVENCVT_DPB_EXTRA or /tmp/nvencvt.dpb (0..2) override.
    int dpb_extra = getenv("NVENCVT_DPB_EXTRA") ? atoi(getenv("NVENCVT_DPB_EXTRA")) : 2;
    { FILE *df = fopen("/tmp/nvencvt.dpb", "r"); int v = 0;
      if (df) { if (fscanf(df, "%d", &v) == 1 && v >= 0) dpb_extra = v; fclose(df); } }
    dpb_extra = dpb_extra < 0 ? 0 : dpb_extra > 2 ? 2 : dpb_extra;
    if (!s->av1 && refs > 3) refs = 3;   // AV1 takes 4 (LAST .. GOLDEN), H.264 / HEVC 3
    EncConfig c = { .codec = s->av1 ? ENC_AV1 : s->hevc ? ENC_HEVC : ENC_H264, .width = s->width, .height = s->height, .num_refs = refs, .dpb_extra = dpb_extra, .fps = s->fps > 0 ? s->fps : 60,
                    .idr_period = s->maxKeyInterval > 0 ? s->maxKeyInterval : 0, .bit_depth = s->ten ? 10 : 8, .full_range = s->fullRange,
                    .h264_profile = h264_high(s) ? 100 : 77, .cabac = h264_cabac(s), .chroma444 = s->c444 };
    // B pictures when the client allows reordering and is not real time (OBS / AVAssetWriter recordings, not Parsec): 2 by
    // default (NVENCVT_BFRAMES=0..3). H.264 needs Main or High in 4:2:0 (Baseline and 4:4:4 stay P-only); AV1: hidden ALTREFs.
    // Not real time also gets NVIDIA's slower ME / MD controls (H.264 quality preset, ~+0.6 dB)
    s->bf = s->reorder && !s->realTime && !s->c444 && (s->hevc || s->av1 || s->h264Profile != 66)
          ? (getenv("NVENCVT_BFRAMES") ? atoi(getenv("NVENCVT_BFRAMES")) : 2) : 0;
    s->bf = s->bf < 0 ? 0 : s->bf > VT_MAX_B ? VT_MAX_B : s->bf;
    if (s->bf && refs < 2) refs = 2;
    c.num_refs = refs; c.bframes = s->bf; c.quality = !s->realTime && !s->speed && !getenv("NVENCVT_FAST");
    {   // the quality controls only where they keep up: their cost per 1080p frame (HEVC NVIDIA's p7 with 8x8 CUs 5.6 ms, H.264
        // 2.7, AV1 2.2) scaled by the picture size must stay under 80 % of the frame period (HEVC 4K: 22 ms, so not at 60 fps)
        const double ms = (s->hevc ? 5.6 : s->av1 ? 2.2 : 2.7) * s->width * s->height / (1920.0 * 1080.0), fps = s->fps > 0 ? s->fps : 60;
        // HEVC: without 8x8 coding units the p7 controls cost 2.5 ms per 1080p frame (10 at 4K) for most of the gain (T7.10)
        const double msMid = 2.5 * s->width * s->height / (1920.0 * 1080.0);
        if (c.quality && ms * fps > 800 && s->hevc && msMid * fps <= 800 && !getenv("NVENCVT_QUALITY")) { c.no_cu8 = true; LOG("quality controls without 8x8 CUs: %.1f ms per frame at %.0f fps", msMid, fps); }
        else if (c.quality && ms * fps > 800 && !getenv("NVENCVT_QUALITY")) { c.quality = false; LOG("quality controls off: %.1f ms per frame at %.0f fps", ms, fps); }
        // GTX 1650 (NVENC 6.6): H.264 with B pictures (they always get NVIDIA's p4 controls) costs ~5.4 ms per 1080p frame, 21 at
        // 4K (T15.10), twice the RTX 4090's; where that does not keep up, P only with the card's real-time controls (14 ms at 4K)
        const double msB = 5.4 * s->width * s->height / (1920.0 * 1080.0);
        if (gNvencvtGpu == GPU_TU117 && !s->hevc && !s->av1 && (s->bf > 0 || c.quality) && msB * fps > 800 && !getenv("NVENCVT_QUALITY")) {
            LOG("GTX 1650: B pictures and quality controls off (H.264 %.1f ms per frame at %.0f fps)", msB, fps);
            s->bf = 0; c.bframes = 0; c.quality = false;
        }
    }
    // constant QP from Quality (0..1) when there is no bitrate; else CBR (MB-row), one-frame VBV in real time mode
    int qp = s->quality ? 40 - s->quality * 22 / 100 : 23;
    c.qp_i = qp; c.qp_p = qp + 2;
    if (s->av1) { c.qp_i = qp * 4 - 12; c.qp_p = c.qp_i + 20; }   // AV1 quantizer indices (0..255): 23 -> 80 / 100, the driver's pair
    if (s->bitrate > 0) {
        c.rc_mode = 18; c.bitrate = s->bitrate; c.max_bitrate = s->bitrate;
        c.vbv_bits = s->realTime ? s->bitrate / c.fps * 2 : s->bitrate;
        if (!s->cbr && s->maxRate > s->bitrate) { c.rc_mode = 17; c.max_bitrate = s->maxRate; c.vbv_bits = s->maxVbv; }   // DataRateLimits: VBR
        c.two_pass = getenv("NVENCVT_TWOPASS") != NULL;   // two passes per frame (T7.09; enc.c), H.264 / HEVC
    }
    // colour description (H.273) and HDR metadata; RGB input is converted with BT.709's matrix by us, so that is signalled
    for (int k = 0; k < 3; k++) c.colour[k] = s->colour[k];
    if (s->lastPf == kCVPixelFormatType_32BGRA || s->lastPf == kCVPixelFormatType_ARGB2101010LEPacked) c.colour[2] = 1;
    if (s->hasMdcv) { memcpy(c.mdcv, s->mdcv, 24); c.has_mdcv = true; }
    if (s->hasCll) { memcpy(c.cll, s->cll, 4); c.has_cll = true; }
    c.depth = getenv("NVENCVT_DEPTH") ? atoi(getenv("NVENCVT_DEPTH")) : 2;   // frames in flight
    // the engine with fewer sessions in this process (a second session runs on NVENC1, in parallel)
    pthread_mutex_lock(&gEngineLock);
    c.engine = gEngineUse[1] < gEngineUse[0] ? 1 : 0;
    if (getenv("NVENCVT_ENGINE")) c.engine = atoi(getenv("NVENCVT_ENGINE")) & 1;
    gEngineUse[c.engine]++; s->engine = c.engine;
    pthread_mutex_unlock(&gEngineLock);
    int eo = enc_open(&s->enc, &c);
    if (eo && c.engine == 1) {   // one NVENC only (GTX 1650, RTX 4060): the second session shares NVENC0
        enc_close(&s->enc);
        LOG("no NVENC1: on NVENC0");
        pthread_mutex_lock(&gEngineLock); gEngineUse[1]--; gEngineUse[0]++; pthread_mutex_unlock(&gEngineLock);
        c.engine = 0; s->engine = 0;
        eo = enc_open(&s->enc, &c);
    }
    if (eo) {
        enc_close(&s->enc);
        pthread_mutex_lock(&gEngineLock); gEngineUse[s->engine]--; pthread_mutex_unlock(&gEngineLock);
        return kVTVideoEncoderMalfunctionErr;
    }
    s->depth = (unsigned)s->enc.cfg.depth;
    // enc_open may turn B pictures off (HEVC on NVENC 6.6, GTX 1650): the reordering here must follow
    if (s->bf != s->enc.cfg.bframes) { LOG("B pictures %d -> %d (this NVENC)", s->bf, s->enc.cfg.bframes); s->bf = s->enc.cfg.bframes; }
    uint8_t hdr[256]; size_t n = s->av1 ? 0 : enc_headers(&s->enc, hdr);
    if (s->av1) {   // av1C (AV1 Codec ISO Media File Format Binding 2.3): 4 bytes, then the sequence header OBU
        uint8_t cfg[64] = { 0x81, (uint8_t)av1_level_for(s->width, s->height), (uint8_t)((s->ten ? 0x40 : 0) | 0x0c), 0 };
        const size_t cn = 4 + enc_av1_config(&s->enc, cfg + 4);
        CFDataRef d = CFDataCreate(NULL, cfg, (CFIndex)cn);
        const void *ak[] = { CFSTR("av1C") }, *av[] = { d };
        CFDictionaryRef atoms = CFDictionaryCreate(NULL, ak, av, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        const void *ek[] = { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMFormatDescriptionExtension_FullRangeVideo }, *ev[] = { atoms, s->fullRange ? kCFBooleanTrue : kCFBooleanFalse };
        CFDictionaryRef ext = CFDictionaryCreate(NULL, ek, ev, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        OSStatus st = CMVideoFormatDescriptionCreate(NULL, CODEC_AV1, s->width, s->height, ext, &s->fmt);
        CFRelease(ext); CFRelease(atoms); CFRelease(d);
        LOG("AV1%s encoder open on NVENC%d: %dx%d refs %d range %s rc_mode %d bitrate %d vbv %d q %d/%d idr_period %d fps %d bframes %d -> fmt %d", s->ten ? " 10-bit" : "", c.engine,
            c.width, c.height, s->enc.cfg.num_refs, c.full_range ? "full" : "video", c.rc_mode, c.bitrate, c.vbv_bits, c.qp_i, c.qp_p, c.idr_period, c.fps, c.bframes, (int)st);
        if (st) { enc_close(&s->enc); return st; }
        goto opened;
    }
    // the parameter sets (H.264: SPS + PPS, HEVC: VPS + SPS + PPS) from the Annex B header block
    const uint8_t *ps[3]; size_t pl[3]; int cnt = 0, want = s->hevc ? 3 : 2;
    for (size_t i = 0; i + 4 <= n && cnt < want; i++)
        if (!hdr[i] && !hdr[i + 1] && !hdr[i + 2] && hdr[i + 3] == 1) { ps[cnt++] = hdr + i + 4; i += 3; }
    for (int k = 0; k < cnt; k++) pl[k] = (size_t)((k + 1 < cnt ? ps[k + 1] - 4 : hdr + n) - ps[k]);
    OSStatus st = cnt != want ? kVTVideoEncoderMalfunctionErr : s->hevc
        ? CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL, 3, ps, pl, 4, NULL, &s->fmt)
        : CMVideoFormatDescriptionCreateFromH264ParameterSets(NULL, 2, ps, pl, 4, &s->fmt);
    LOG("%s%s%s encoder open on NVENC%d: %dx%d refs %d dpb+%d range %s profile %d cabac %d rc_mode %d bitrate %d vbv %d qp %d/%d idr_period %d fps %d bframes %d quality %d -> fmt %d", s->hevc ? "HEVC" : "H.264", s->ten ? " Main10" : "", s->c444 ? " 4:4:4" : "", c.engine, c.width, c.height, c.num_refs, c.dpb_extra, c.full_range ? "full" : "video", s->hevc ? 0 : s->enc.cfg.chroma444 ? 244 : c.h264_profile, s->hevc ? 0 : (int)s->enc.cfg.cabac,
        c.rc_mode, c.bitrate, c.vbv_bits, c.qp_i, c.qp_p, c.idr_period, c.fps, c.bframes, (int)c.quality, (int)st);
    if (st) { enc_close(&s->enc); return st; }
opened:
    add_fmt_extensions(s);
    LOG("colour %d/%d/%d%s%s", s->enc.cfg.colour[0], s->enc.cfg.colour[1], s->enc.cfg.colour[2], s->enc.cfg.has_mdcv ? ", mastering display" : "", s->enc.cfg.has_cll ? ", content light level" : "");
    s->opened = true;
    s->stop = false;
    const char *sf = getenv("NVENCVT_STATS");
    if (sf || access("/tmp/nvencvt.stats", F_OK) == 0) { s->statsFile = fopen(sf ? sf : "/tmp/nvencvt-stats.log", "a"); s->statsT0 = now_ms(); }
    s->threadOn = pthread_create(&s->thread, NULL, completion, s) == 0;
    if (!s->threadOn) { enc_close(&s->enc); s->opened = false; return kVTVideoEncoderMalfunctionErr; }
    return noErr;
}

static void emit(State *s, VTEncoderFrameRef frame, CMTime pts, CMTime dur, CMTime dts, bool b, const EncFrame *fr, OSStatus err)
{
    if (err) { VTEncoderSessionEmitEncodedFrame(s->session, frame, err, 0, NULL); return; }
    if (s->dumpEs) { fwrite(fr->data, 1, fr->size, s->dumpEs); fflush(s->dumpEs); }
    // AV1: the temporal unit without its temporal delimiter (ISOBMFF samples carry none); else Annex B (4-byte start
    // codes) -> AVCC (4-byte big-endian lengths), same size
    const size_t skip = s->av1 && fr->size >= 2 && fr->data[0] == 0x12 && fr->data[1] == 0 ? 2 : 0;
    uint8_t *buf = malloc(fr->size);
    memcpy(buf, fr->data + skip, fr->size - skip);
    size_t start = SIZE_MAX;
    for (size_t i = 0; !s->av1 && i + 4 <= fr->size; i++) {
        if (!buf[i] && !buf[i + 1] && !buf[i + 2] && buf[i + 3] == 1) {
            if (start != SIZE_MAX) { uint32_t l = (uint32_t)(i - start - 4); buf[start] = l >> 24; buf[start + 1] = l >> 16; buf[start + 2] = l >> 8; buf[start + 3] = l; }
            start = i; i += 3;
        }
    }
    if (!s->av1 && start != SIZE_MAX) { uint32_t l = (uint32_t)(fr->size - start - 4); buf[start] = l >> 24; buf[start + 1] = l >> 16; buf[start + 2] = l >> 8; buf[start + 3] = l; }
    CMBlockBufferRef bb = NULL; CMSampleBufferRef sb = NULL;
    OSStatus st = CMBlockBufferCreateWithMemoryBlock(NULL, buf, fr->size, kCFAllocatorMalloc, NULL, 0, fr->size - skip, 0, &bb);
    CMSampleTimingInfo ti = { .duration = dur, .presentationTimeStamp = pts, .decodeTimeStamp = dts };
    size_t sz = fr->size - skip;
    if (!st) st = CMSampleBufferCreateReady(NULL, bb, s->fmt, 1, 1, &ti, 1, &sz, &sb);
    if (!st) {
        CFArrayRef att = CMSampleBufferGetSampleAttachmentsArray(sb, true);
        CFMutableDictionaryRef a = (CFMutableDictionaryRef)CFArrayGetValueAtIndex(att, 0);
        CFDictionarySetValue(a, kCMSampleAttachmentKey_NotSync, fr->idr ? kCFBooleanFalse : kCFBooleanTrue);
        CFDictionarySetValue(a, kCMSampleAttachmentKey_DependsOnOthers, fr->idr ? kCFBooleanFalse : kCFBooleanTrue);
        if (s->bf && !s->av1) CFDictionarySetValue(a, kCMSampleAttachmentKey_IsDependedOnByOthers, b ? kCFBooleanFalse : kCFBooleanTrue);
    }
    VTEncoderSessionEmitEncodedFrame(s->session, frame, st, 0, st ? NULL : sb);
    if (sb) CFRelease(sb);
    if (bb) CFRelease(bb);
}

static void *completion(void *arg)
{
    State *s = arg;
    pthread_mutex_lock(&s->lock);
    for (;;) {
        while (s->qh == s->qt && !s->stop) pthread_cond_wait(&s->cv, &s->lock);
        if (s->qh == s->qt) break;
        unsigned i = s->qh % s->depth;
        VTEncoderFrameRef frame = s->q[i].frame; CMTime pts = s->q[i].pts, dur = s->q[i].dur, dts = s->q[i].dts; NvU32 t = s->q[i].ticket;
        bool ok = s->q[i].ok, b = s->q[i].b, showAfter = s->q[i].showAfter;
        double tEntry = s->q[i].tEntry, tCall = s->q[i].tCall, tTile = s->q[i].tTile;
        pthread_mutex_unlock(&s->lock);
        EncFrame fr; memset(&fr, 0, sizeof fr);
        int rc = ok ? enc_wait(&s->enc, t, &fr) : -1;
        if (rc) LOG("frame %u: encode failed (ucode error %u)", t, fr.ucode_error);
        else if (gLog && (t <= 3 || fr.idr)) LOG("frame %u %s: %zu bytes, %.3f ms, avgQP %u", t, fr.idr ? "IDR" : b ? "B" : "P", fr.size, fr.ms, fr.avg_qp);
        if (!rc && fr.hidden) { s->hid.frame = frame; s->hid.pts = pts; s->hid.dur = dur; s->hid.valid = true; }   // AV1: shown later
        else emit(s, frame, pts, dur, dts, b, &fr, rc ? kVTVideoEncoderMalfunctionErr : noErr);   // fr.data stays valid until qh moves on
        if (showAfter && s->hid.valid) {
            uint8_t se[16]; EncFrame sf; memset(&sf, 0, sizeof sf); sf.data = se; sf.size = enc_av1_show_existing(&s->enc, se);
            emit(s, s->hid.frame, s->hid.pts, s->hid.dur, kCMTimeInvalid, false, &sf, noErr); s->hid.valid = false;
        }
        pthread_mutex_lock(&s->lock);
        stats_frame(s, tCall, tTile, fr.ms, now_ms() - tEntry, rc ? 0 : fr.size, fr.idr, rc != 0);
        s->qh++;
        pthread_cond_broadcast(&s->cv);
    }
    pthread_mutex_unlock(&s->lock);
    return NULL;
}

static void drain(State *s) { while (s->qh != s->qt) pthread_cond_wait(&s->cv, &s->lock); }   // lock held

// Load a frame into the encoder's next input buffer (lock held; the format was checked by e_encodeFrame)
static void load_image(State *s, CVPixelBufferRef img)
{
    OSType pf = CVPixelBufferGetPixelFormatType(img);
    bool bgra = pf == kCVPixelFormatType_32BGRA;
    bool p010 = pf == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange || pf == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange;
    bool l10r = pf == kCVPixelFormatType_ARGB2101010LEPacked;
    bool nv24 = pf == kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange || pf == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange;
    bool x444 = pf == kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange || pf == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange;
    CVPixelBufferLockBaseAddress(img, kCVPixelBufferLock_ReadOnly);
    if (nv24) enc_load_nv24(&s->enc, CVPixelBufferGetBaseAddressOfPlane(img, 0), CVPixelBufferGetBytesPerRowOfPlane(img, 0),
                            CVPixelBufferGetBaseAddressOfPlane(img, 1), CVPixelBufferGetBytesPerRowOfPlane(img, 1));
    else if (x444) enc_load_x444(&s->enc, CVPixelBufferGetBaseAddressOfPlane(img, 0), CVPixelBufferGetBytesPerRowOfPlane(img, 0),
                                 CVPixelBufferGetBaseAddressOfPlane(img, 1), CVPixelBufferGetBytesPerRowOfPlane(img, 1));
    else if (l10r) enc_load_l10r(&s->enc, CVPixelBufferGetBaseAddress(img), CVPixelBufferGetBytesPerRow(img));
    else if (p010) enc_load_p010(&s->enc, CVPixelBufferGetBaseAddressOfPlane(img, 0), CVPixelBufferGetBytesPerRowOfPlane(img, 0),
                            CVPixelBufferGetBaseAddressOfPlane(img, 1), CVPixelBufferGetBytesPerRowOfPlane(img, 1));
    else if (bgra) enc_load_bgra(&s->enc, CVPixelBufferGetBaseAddress(img), CVPixelBufferGetBytesPerRow(img));
    else enc_load_nv12(&s->enc, CVPixelBufferGetBaseAddressOfPlane(img, 0), CVPixelBufferGetBytesPerRowOfPlane(img, 0),
                       CVPixelBufferGetBaseAddressOfPlane(img, 1), CVPixelBufferGetBytesPerRowOfPlane(img, 1));
    CVPixelBufferUnlockBaseAddress(img, kCVPixelBufferLock_ReadOnly);
}

// Wait for a free slot, load the frame and start encoding it (lock held). disp < 0: no reordering (enc_submit)
static void submit(State *s, VTEncoderFrameRef frame, CVPixelBufferRef img, CMTime pts, CMTime dur, double tEntry, bool idr, bool b, int disp)
{
    while (s->qt - s->qh >= s->depth) pthread_cond_wait(&s->cv, &s->lock);   // a free input slot
    double tTile0 = now_ms();
    load_image(s, img);
    unsigned i = s->qt % s->depth;
    s->q[i].frame = frame; s->q[i].pts = pts; s->q[i].dur = dur; s->q[i].b = b; s->q[i].showAfter = false;
    // decode time stamps with reordering: the presentation time one picture earlier in display order (num_reorder_frames
    // 1: a B picture decodes when it is shown, an anchor one picture before the first B picture that waits for it)
    CMTime d = CMTIME_IS_NUMERIC(dur) ? dur : CMTimeMake(1, s->fps > 0 ? s->fps : 60);
    // without reordering the decode time is the presentation time: clients that asked for B pictures (ffmpeg -bf 2) need one
    // on every sample ("DTS is invalid"), also when this encoder gives them none (HEVC on the GTX 1650, H.264 Baseline)
    s->q[i].dts = s->av1 ? kCMTimeInvalid : disp < 0 ? pts : s->coded == 0 ? CMTimeSubtract(pts, d) : s->ptsRing[(s->coded - 1) % 64];
    s->q[i].tEntry = tEntry; s->q[i].tTile = now_ms() - tTile0;
    s->q[i].ok = (disp < 0 ? enc_submit(&s->enc, idr, &s->q[i].ticket) : enc_submit_ex(&s->enc, idr, b, disp, &s->q[i].ticket)) == 0;
    s->q[i].tCall = now_ms() - tEntry;
    s->qt++; s->coded++;
    pthread_cond_broadcast(&s->cv);
}

// Encode the waiting frames (lock held): the newest as the anchor, then the others as B pictures in display order. After
// an encode error the encoder starts over with an IDR (enc.c): then they go in display order as IDR / P pictures.
static void flush_group(State *s)
{
    int n = s->npend; s->npend = 0;
    __typeof__(s->pend[0]) g[VT_MAX_B + 1];
    memcpy(g, s->pend, sizeof g[0] * (size_t)n);
    const bool restart = s->enc.forceIdr;
    for (int k = 0; k < n; k++) {
        int j = restart ? k : k == 0 ? n - 1 : k - 1;
        submit(s, g[j].frame, g[j].img, g[j].pts, g[j].dur, g[j].tEntry, false, !restart && k > 0, g[j].disp);
        if (s->av1 && !restart && n > 1 && k == n - 1) s->q[(s->qt - 1) % s->depth].showAfter = true;   // then show the anchor
        CVPixelBufferRelease(g[j].img);
    }
}

static OSStatus e_encodeFrame(VTVideoEncoderRef e, VTEncoderFrameRef frame, CVImageBufferRef img, CMTime pts, CMTime dur,
                              CFDictionaryRef props, VTEncodeInfoFlags *infoOut)
{
    State *s = state(e);
    double tEntry = now_ms();
    pthread_mutex_lock(&s->lock);
    OSStatus st = noErr;
    // the stream's range follows the first frame: full-range YCbCr (420f, xf20) is encoded as it is and signalled in the
    // VUI; BGRA and l10r are converted by us to video range, and 420v / x420 are video range (T7.01)
    if (!s->opened) { OSType p0 = CVPixelBufferGetPixelFormatType(img);
        s->fullRange = p0 == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange || p0 == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange ||
                       p0 == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange || p0 == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange;
        s->c444 = !s->av1 && (p0 == kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange || p0 == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange ||
                              (s->ten && (p0 == kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange || p0 == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange))); }
    if (!s->opened && s->av1) { OSType p0 = CVPixelBufferGetPixelFormatType(img);   // AV1: 10-bit when the input is
        s->ten = p0 == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange || p0 == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange || p0 == kCVPixelFormatType_ARGB2101010LEPacked; }
    if (!s->opened) {   // colour not given as properties: the first frame's attachments (T7.05); its format for the RGB rule
        static const CFStringRef *keys[3] = { &kCVImageBufferColorPrimariesKey, &kCVImageBufferTransferFunctionKey, &kCVImageBufferYCbCrMatrixKey };
        for (int k = 0; k < 3; k++) if (!s->colour[k]) { CFTypeRef a = CVBufferCopyAttachment(img, *keys[k], NULL); s->colour[k] = colour_code(k, a); if (a) CFRelease(a); }
        s->lastPf = CVPixelBufferGetPixelFormatType(img);
    }
    if (!s->opened && !s->fpsKnown) {   // the frame rate from the first frame's duration, if it has one
        if (CMTIME_IS_NUMERIC(dur) && CMTimeGetSeconds(dur) > 0.0005 && CMTimeGetSeconds(dur) < 1.5) { s->fps = (int)(1 / CMTimeGetSeconds(dur) + 0.5); s->fpsKnown = true; }
        s->firstPts = pts;
    } else if (s->opened && !s->fpsKnown && s->frames == 1 && CMTIME_IS_NUMERIC(pts) && CMTIME_IS_NUMERIC(s->firstPts)) {
        // else from the first two time stamps; the rate control reads the frame rate for every picture (the streams carry none)
        const double dt = CMTimeGetSeconds(CMTimeSubtract(pts, s->firstPts));
        if (dt > 0.0005 && dt < 1.5) {
            const int f = (int)(1 / dt + 0.5);
            if (f != s->enc.cfg.fps) {
                LOG("frame rate %d from the time stamps (was %d)", f, s->enc.cfg.fps);
                if (s->realTime && s->bitrate > 0 && s->enc.cfg.vbv_bits == s->bitrate / s->enc.cfg.fps * 2) s->enc.cfg.vbv_bits = s->bitrate / f * 2;
                s->enc.cfg.fps = s->fps = f;
            }
        }
        s->fpsKnown = true;
    }
    if (!s->opened && !s->failed && (st = open_encoder(s))) s->failed = true;
    if (s->failed) { pthread_mutex_unlock(&s->lock); VTEncoderSessionEmitEncodedFrame(s->session, frame, kVTVideoEncoderMalfunctionErr, 0, NULL); return kVTVideoEncoderMalfunctionErr; }
    OSType pf = CVPixelBufferGetPixelFormatType(img);
    bool bgra = pf == kCVPixelFormatType_32BGRA;
    bool p010 = pf == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange || pf == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange;
    bool l10r = pf == kCVPixelFormatType_ARGB2101010LEPacked;
    bool nv24 = pf == kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange || pf == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange;
    bool x444 = pf == kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange || pf == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange;
    if (s->c444 ? !(s->ten ? x444 : nv24) :   // a 4:4:4 session takes only its own format, a 4:2:0 one none of them
        ((pf != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange && pf != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange && !bgra && !((p010 || l10r) && s->ten)) ||
         (bgra && s->ten)) ||
        (int)CVPixelBufferGetWidth(img) < s->width || (int)CVPixelBufferGetHeight(img) < s->height) {
        { OSType be = CFSwapInt32HostToBig(pf); LOG("unexpected pixel buffer %.4s %zux%zu", (char *)&be, CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img)); }
        pthread_mutex_unlock(&s->lock);
        VTEncoderSessionEmitEncodedFrame(s->session, frame, kVTPixelTransferNotSupportedErr, 0, NULL);
        return noErr;
    }
    if (gLog && s->frames == 0) { OSType be = CFSwapInt32HostToBig(pf); CFTypeRef m = CVBufferCopyAttachment(img, kCVImageBufferYCbCrMatrixKey, NULL), t = CVBufferCopyAttachment(img, kCVImageBufferTransferFunctionKey, NULL);
        char ms[64] = "-", ts[64] = "-"; if (m) { CFStringGetCString(m, ms, sizeof ms, kCFStringEncodingUTF8); CFRelease(m); } if (t) { CFStringGetCString(t, ts, sizeof ts, kCFStringEncodingUTF8); CFRelease(t); }
        LOG("first frame %.4s %zux%zu matrix %s transfer %s", (char *)&be, CVPixelBufferGetWidth(img), CVPixelBufferGetHeight(img), ms, ts); }
    s->lastPf = pf;
    { IOSurfaceRef ps = CVPixelBufferGetIOSurface(img); s->bcSeedPre = ps ? IOSurfaceGetSeed(ps) : 0; }   // diagnostics: the seed before our lock
    CVPixelBufferLockBaseAddress(img, kCVPixelBufferLock_ReadOnly);
    if (!s->dumpEs && (s->frames % 30) == 0 && access("/tmp/nvencvt.dump", F_OK) == 0) {   // start a dump (diagnostics)
        char p[128]; snprintf(p, sizeof p, "/tmp/nvencvt-dump-%d.es", (int)getpid()); s->dumpEs = fopen(p, "wb");
        snprintf(p, sizeof p, "/tmp/nvencvt-dump-%d.nv12", (int)getpid()); s->dumpYuv = bgra || p010 || l10r || s->c444 ? NULL : fopen(p, "wb");
        s->dumpYuvLeft = 30; s->dumpIdr = true;
        if (s->dumpEs) { uint8_t h[256]; size_t n = enc_headers(&s->enc, h); fwrite(h, 1, n, s->dumpEs); }
        LOG("dump started (%dx%d, %s)", s->width, s->height, bgra ? "BGRA: stream only" : "NV12");
    }
    // Diagnostics: /tmp/nvencvt.barcode = "x y block": frames carry a 32-bit counter as 32 black / white blocks of `block`
    // pixels from (x, y) in the luma plane (vm/tahoe/bench/metal-anim.m draws one). Logs to /tmp/nvencvt-barcode.log when the
    // counter goes backwards (an older frame reached the encoder), repeats or skips, with a summary every 2 s.
    if ((s->frames % 120) == 0 && !s->bcFile) {
        FILE *f = fopen("/tmp/nvencvt.barcode", "r");
        if (f) { if (fscanf(f, "%d %d %d %d", &s->bcX, &s->bcY, &s->bcS, &s->bcAll) >= 3 && s->bcS >= 4) { s->bcFile = fopen("/tmp/nvencvt-barcode.log", "a"); s->bcLast = -1; s->bcT0 = now_ms();
                     if (s->bcFile) fprintf(s->bcFile, "pid %d session %dx%d: checking a counter at %d,%d block %d\n", (int)getpid(), s->width, s->height, s->bcX, s->bcY, s->bcS); }
                 fclose(f); }
    }
    if (s->bcFile && s->bcX + 32 * s->bcS <= s->width && s->bcY + s->bcS <= s->height) {
        // BGRA and l10r (packed 10-bit RGB, which Parsec hands over in some modes): the blocks are black / white, so byte 1
        // stands in for luma (BGRA: green; l10r: green bits 0-5 sit in bits 2-7 of byte 1)
        const bool rgb = bgra || l10r;
        const uint8_t *y = rgb ? CVPixelBufferGetBaseAddress(img) : CVPixelBufferGetBaseAddressOfPlane(img, 0);
        const size_t st = rgb ? CVPixelBufferGetBytesPerRow(img) : CVPixelBufferGetBytesPerRowOfPlane(img, 0);
        const size_t bpx = rgb ? 4 : (p010 || x444 ? 2 : 1), bof = rgb || p010 || x444 ? 1 : 0;
        IOSurfaceRef bs = CVPixelBufferGetIOSurface(img);
        const unsigned bsid = bs ? IOSurfaceGetID(bs) : 0, bseed = bs ? IOSurfaceGetSeed(bs) : 0;
        unsigned long v = 0; const size_t row = (size_t)(s->bcY + s->bcS / 2) * st;
        for (int i = 0; i < 32; i++) if (y[row + (size_t)(s->bcX + i * s->bcS + s->bcS / 2) * bpx + bof] > 128) v |= 1ul << i;
        {   // first sight of each surface: what it is
            unsigned k = 0; while (k < s->bcSeenN && s->bcSeen[k] != bsid) k++;
            if (k == s->bcSeenN && k < 32 && bs) {
                s->bcSeen[s->bcSeenN++] = bsid; OSType bpf = IOSurfaceGetPixelFormat(bs), bbe = CFSwapInt32HostToBig(bpf);
                fprintf(s->bcFile, "[%d] %.3f surface %u: %.4s %zux%zu planes %zu alloc %zu row %zu (frame %u)\n", (int)getpid(), now_ms() / 1000, bsid, (char *)&bbe,
                        IOSurfaceGetWidth(bs), IOSurfaceGetHeight(bs), IOSurfaceGetPlaneCount(bs), IOSurfaceGetAllocSize(bs), st, s->frames);
            }
        }
        if (s->bcAll) fprintf(s->bcFile, "[%d] %.3f FRAME surface %u seed %u prelock %u counter %lu pts %.3f\n", (int)getpid(), now_ms() / 1000, bsid, bseed, s->bcSeedPre, v, CMTimeGetSeconds(pts));
        {   unsigned r = s->bcRingN++ % 16;
            s->bcRing[r].sid = bsid; s->bcRing[r].seed = bseed; s->bcRing[r].v = (long)v; s->bcRing[r].t = now_ms(); s->bcRing[r].pts = CMTimeGetSeconds(pts); }
        if (s->bcLast >= 0 && (long)v < s->bcLast && s->bcLast - (long)v < 100) {   // the last 16 frames, oldest first
            for (unsigned i = s->bcRingN > 16 ? s->bcRingN - 16 : 0; i < s->bcRingN; i++) { unsigned r = i % 16;
                fprintf(s->bcFile, "[%d]    surface %u seed %u counter %ld pts %.3f at %.1f ms\n", (int)getpid(), s->bcRing[r].sid, s->bcRing[r].seed, s->bcRing[r].v, s->bcRing[r].pts, s->bcRing[r].t - s->bcRing[(s->bcRingN - 1) % 16].t); }
        }
        if (s->bcLast >= 0) {
            if ((long)v < s->bcLast) { s->bcBack++; fprintf(s->bcFile, "[%d] %.3f BACKWARD %lu after %ld (frame %u) surface %u seed %u pts %.3f, before: surface %u seed %u pts %.3f\n", (int)getpid(), now_ms() / 1000, v, s->bcLast, s->frames,
                                              bsid, bseed, CMTimeGetSeconds(pts), s->bcSid, s->bcSeed, s->bcPts); }
            else if ((long)v == s->bcLast) s->bcRep++;
            else if ((long)v > s->bcLast + 1) s->bcSkip += (unsigned)((long)v - s->bcLast - 1);
        }
        s->bcLast = (long)v; s->bcN++; s->bcSid = bsid; s->bcSeed = bseed; s->bcPts = CMTimeGetSeconds(pts);
        if (now_ms() - s->bcT0 >= 2000) { fprintf(s->bcFile, "[%d] %.3f %u frames: counter %lu, %u backward, %u repeated, %u skipped\n", (int)getpid(), now_ms() / 1000, s->bcN, v, s->bcBack, s->bcRep, s->bcSkip);
                                          fflush(s->bcFile); s->bcT0 = now_ms(); s->bcN = s->bcBack = s->bcRep = s->bcSkip = 0; }
    }
    if (s->dumpYuv && s->dumpYuvLeft > 0) {   // the input as Parsec delivered it, tightly packed NV12
        for (int pl = 0; pl < 2; pl++) {
            const uint8_t *b = CVPixelBufferGetBaseAddressOfPlane(img, pl); size_t st = CVPixelBufferGetBytesPerRowOfPlane(img, pl);
            int rows = pl ? (s->height + 1) / 2 : s->height;
            for (int r = 0; r < rows; r++) fwrite(b + (size_t)r * st, 1, (size_t)s->width, s->dumpYuv);
        }
        if (--s->dumpYuvLeft == 0) { fclose(s->dumpYuv); s->dumpYuv = NULL; }
    }
    CVPixelBufferUnlockBaseAddress(img, kCVPixelBufferLock_ReadOnly);
    bool forceIdr = false;
    if (props) { CFTypeRef k = CFDictionaryGetValue(props, kVTEncodeFrameOptionKey_ForceKeyFrame); forceIdr = k && CFGetTypeID(k) == CFBooleanGetTypeID() && CFBooleanGetValue(k); }
    if (s->bf) {   // reordering (T7.03): anchors wait for nothing, the frames before one wait for it
        s->ptsRing[s->disp % 64] = pts;
        const int period = s->enc.cfg.idr_period;
        const bool key = forceIdr || s->dumpIdr || s->disp == 0 || (period > 0 && s->sinceKey >= period);
        s->dumpIdr = false;
        if (key) { flush_group(s); submit(s, frame, img, pts, dur, tEntry, true, false, s->disp); s->sinceKey = 1; }
        else {
            CVPixelBufferRetain(img);
            s->pend[s->npend].frame = frame; s->pend[s->npend].img = img; s->pend[s->npend].pts = pts; s->pend[s->npend].dur = dur;
            s->pend[s->npend].tEntry = tEntry; s->pend[s->npend].disp = s->disp; s->npend++;
            s->sinceKey++;
            if (s->npend > s->bf) flush_group(s);
        }
        s->disp++; s->frames++;
        pthread_mutex_unlock(&s->lock);
        if (infoOut) *infoOut = kVTEncodeInfo_Asynchronous;
        return noErr;
    }
    submit(s, frame, img, pts, dur, tEntry, forceIdr || s->dumpIdr, false, -1); s->dumpIdr = false;
    s->frames++;
    // RealTime (streaming): return only once this frame is emitted, so the output reaches the app inside its encode call.
    // Parsec picks up output when it encodes the next frame otherwise: one frame period of extra latency (T5.05).
    bool sync = s->realTime || getenv("NVENCVT_SYNC");
    if (sync) { unsigned mine = s->qt; while (s->qh < mine && s->threadOn) pthread_cond_wait(&s->cv, &s->lock); }
    pthread_mutex_unlock(&s->lock);
    if (infoOut) *infoOut = sync ? 0 : kVTEncodeInfo_Asynchronous;
    return noErr;
}

static OSStatus e_completeFrames(VTVideoEncoderRef e, CMTime until)
{
    (void)until;
    State *s = state(e);
    pthread_mutex_lock(&s->lock); if (s->opened) flush_group(s); drain(s); pthread_mutex_unlock(&s->lock);
    return noErr;
}
static OSStatus e_copySupported(VTVideoEncoderRef e, CFDictionaryRef *out) { (void)e; *out = CFRetain(supported()); return noErr; }

static OSStatus b_invalidate(CMBaseObjectRef o)
{
    State *s = state(o);
    pthread_mutex_lock(&s->lock);
    if (s->threadOn) {
        if (s->opened) flush_group(s);
        drain(s); s->stop = true; pthread_cond_broadcast(&s->cv);
        pthread_mutex_unlock(&s->lock); pthread_join(s->thread, NULL); pthread_mutex_lock(&s->lock);
        s->threadOn = false;
    }
    if (s->statsFile) { stats_print(s); fclose(s->statsFile); s->statsFile = NULL; }
    if (s->bcFile) { fclose(s->bcFile); s->bcFile = NULL; }
    if (s->opened) {
        enc_close(&s->enc); s->opened = false;
        if (s->dumpEs) { fclose(s->dumpEs); s->dumpEs = NULL; } if (s->dumpYuv) { fclose(s->dumpYuv); s->dumpYuv = NULL; }
        pthread_mutex_lock(&gEngineLock); gEngineUse[s->engine]--; pthread_mutex_unlock(&gEngineLock);
    }
    if (s->fmt) { CFRelease(s->fmt); s->fmt = NULL; }
    LOG("invalidate after %u frames", s->frames);
    pthread_mutex_unlock(&s->lock);
    return noErr;
}
static void b_finalize(CMBaseObjectRef o)
{
    State *s = state(o);
    if (!s) return;
    b_invalidate(o);
    pthread_mutex_destroy(&s->lock); pthread_cond_destroy(&s->cv);
    free(s); *(State **)CMBaseObjectGetDerivedStorage(o) = NULL;
}
static CFStringRef b_desc(CMBaseObjectRef o) { (void)o; return CFSTR("nvenc H.264 encoder"); }

static CMBaseClass gBase = { .version = 1, .derivedStorageSize = sizeof(void *), .invalidate = b_invalidate, .finalize = b_finalize,
                             .copyDebugDescription = b_desc, .copyProperty = b_copyProperty, .setProperty = b_setProperty };
static const EncoderClass gEnc = { .version = 1, .startSession = e_startSession, .encodeFrame = e_encodeFrame,
                                   .completeFrames = e_completeFrames, .copySupportedPropertyDictionary = e_copySupported };
static const EncoderVTable gVT = { .base = { NULL, &gBase }, .encoderClass = &gEnc };

__attribute__((visibility("default"))) OSStatus NVEncVT_CreateInstance(FourCharCode codec, CFAllocatorRef al, VTVideoEncoderRef *out)
{
    if (codec != kCMVideoCodecType_H264 && codec != kCMVideoCodecType_HEVC && codec != CODEC_AV1) return kVTCouldNotFindVideoEncoderErr;
    CMBaseObjectRef obj = NULL;
    OSStatus st = CMDerivedObjectCreate(al, &gVT.base, VTVideoEncoderGetClassID(), &obj);
    if (st) return st;
    State *s = calloc(1, sizeof *s);
    s->entropy = -1;
    s->reorder = true;   // VideoToolbox's default for AllowFrameReordering (AVAssetWriter leaves it unset and gets B pictures)
    pthread_mutex_init(&s->lock, NULL); pthread_cond_init(&s->cv, NULL);
    s->hevc = codec == kCMVideoCodecType_HEVC; s->av1 = codec == CODEC_AV1;
    *(State **)CMBaseObjectGetDerivedStorage(obj) = s;
    *out = obj;
    LOG("instance %p (%s)", obj, s->av1 ? "av01" : s->hevc ? "hvc1" : "avc1");
    return noErr;
}

// The encoder drives Ada's NVENC class (NVC9B7_VIDEO_ENCODER, RTX 40) and NVENC 6.6 (0xb4b7, TU117: GTX 1650). Registering
// on a GPU without either would make VideoToolbox pick an encoder that cannot start, so look at the GPU's class list.
// GA10x (0xc7b7, RTX 30) is listed too: its NVDEC decodes AV1, so the decoders register there, but its NVENC is not Ada's,
// so the encoders stay away (NVENCVT_AMPERE_ENC=1 tries them anyway).
static int gpu_nvenc_kind(void)
{
    Rm rm = { 0 };
    if (rm_open(&rm)) { LOG("no NVIDIA RM device"); return GPU_NONE; }
    const NvU32 cls = rm_class(&rm, RM_CLS_NVENC);
    rm_close(&rm);
    return cls == 0xc9b7 ? GPU_ADA : cls == 0xb4b7 ? GPU_TU117 : cls == 0xc7b7 ? GPU_AMPERE : GPU_NONE;
}

static OSStatus register_one(FourCharCode codec, CFStringRef id, CFStringRef name, int rating)
{
    CFNumberRef r = CFNumberCreate(NULL, kCFNumberIntType, &rating);
    const void *sk[] = { kVTCompressionPropertyKey_ProfileLevel }, *sv[] = { profiles(codec == kCMVideoCodecType_HEVC) };
    CFDictionaryRef sel = CFDictionaryCreate(NULL, sk, sv, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *k[] = { CFSTR("CMClassImplementationID"), CFSTR("CMClassImplementationName"), CFSTR("VTRating"), CFSTR("VTIsHardwareAccelerated"),
                        kVTVideoEncoderList_SupportedSelectionProperties };
    const void *v[] = { id, name, r, kCFBooleanTrue, sel };
    // AV1: no profile list (Main, 8 / 10-bit, is all there is)
    CFDictionaryRef info = CFDictionaryCreate(NULL, k, v, codec == CODEC_AV1 ? 4 : 5, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus st = VTRegisterVideoEncoderWithInfo(codec, info, (void *)NVEncVT_CreateInstance);
    CFRelease(info); CFRelease(sel); CFRelease(r);
    return st;
}

// The plugin calls NVEncVT_Register in every process it is enabled for, while the Metal device is created, so the
// check must be cheap: the GPU's PCI device ID from the IORegistry (~0.1 ms) instead of an RM client and the class list
// (6-9 ms). Ada (AD102-AD107: RTX 40, RTX Ada, L4/L40) is device IDs 0x2600-0x28ff; TU117 (GTX 1650, T400 / T600 / T1000:
// NVENC 6.6, for which NullMoth has no encoder) 0x1f80-0x1fff. GA10x (consumer Ampere: RTX 30, the 3060 Ti among them) is
// 0x2200-0x25ff; its NVDEC (0xc7b0) decodes AV1 (NVIDIA's Video Codec SDK 11.0 added GA10x and AV1 Main profile decoding),
// its NVENC (0xc7b7) is not Ada's, so only the decoders register there. Other Turing and Ampere cards keep NullMoth's
// encoders.
// -1: no NVIDIA GPU in the registry.
// the card's name for the encoder names, from the GPU's IORegistry "model" ("NVIDIA GeForce RTX 4090" -> "RTX 4090")
static char gCardName[64] = "GPU";
static void card_name_from(io_object_t d)
{
    CFTypeRef m = IORegistryEntryCreateCFProperty(d, CFSTR("model"), NULL, 0);
    if (m && CFGetTypeID(m) == CFDataGetTypeID() && CFDataGetLength(m) > 0) {
        char b[96] = ""; CFIndex n = CFDataGetLength(m) < (CFIndex)sizeof b - 1 ? CFDataGetLength(m) : (CFIndex)sizeof b - 1;
        CFDataGetBytes(m, CFRangeMake(0, n), (UInt8 *)b); b[n] = 0;
        const char *p = b;
        if (!strncmp(p, "NVIDIA ", 7)) p += 7;
        if (!strncmp(p, "GeForce ", 8)) p += 8;
        if (*p) snprintf(gCardName, sizeof gCardName, "%s", p);
    }
    if (m) CFRelease(m);
}

int gNvencvtGpu = GPU_NONE;   // what NVEncVT_Register found (vt/nvencvt_dec.c: AV1 on Ada and on GA10x)

static int pci_gpu_kind(void)
{
    io_iterator_t it = 0; io_object_t d; int found = -1;
    if (IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching("IOPCIDevice"), &it) != KERN_SUCCESS) return -1;
    while ((d = IOIteratorNext(it))) {
        CFTypeRef ven = IORegistryEntryCreateCFProperty(d, CFSTR("vendor-id"), NULL, 0), dev = IORegistryEntryCreateCFProperty(d, CFSTR("device-id"), NULL, 0);
        unsigned v = 0, id = 0;
        if (ven && CFGetTypeID(ven) == CFDataGetTypeID() && CFDataGetLength(ven) >= 2) CFDataGetBytes(ven, CFRangeMake(0, 2), (UInt8 *)&v);
        if (dev && CFGetTypeID(dev) == CFDataGetTypeID() && CFDataGetLength(dev) >= 2) CFDataGetBytes(dev, CFRangeMake(0, 2), (UInt8 *)&id);
        if (ven) CFRelease(ven);
        if (dev) CFRelease(dev);
        if (v == 0x10de && found < 1) {
            found = id >= 0x2600 && id <= 0x28ff ? GPU_ADA : id >= 0x1f80 && id <= 0x1fff ? GPU_TU117
                  : id >= 0x2200 && id <= 0x25ff ? GPU_AMPERE : GPU_NONE;
            if (found > 0) card_name_from(d);
        }
        IOObjectRelease(d);
    }
    IOObjectRelease(it);
    return found;
}

// ---- encoder names ----
// VTRegisterVideoEncoderWithInfo always names an encoder "Dynamically Registered <codec> Video Encoder": it passes that
// name, not ours, to the registry entry it creates (VideoToolbox's vtRegisterVideoEncoderInternal), and plug-in bundles,
// which can carry a VTEncoderName, are only accepted when Apple signed them (CoreMedia's figRegistryAddResultItem checks
// kFigRegistryAppleCodeSignRequirement for every folder VideoToolbox scans; T7.00). The entry's description is a
// mutable dictionary that VideoToolbox reads VTEncoderName / VTCodecName from when it lists encoders, so we rename our
// entries there: find VideoToolbox's encoder FigRegistry (a pointer in VideoToolbox's data sections to a FigRegistry of
// class com.apple.videotoolbox.videoencoder), our items by ID, and set the two names if the dictionary is mutable and
// still holds VideoToolbox's default name. Anything unexpected: leave the default name. NVENCVT_NO_RENAME=1 skips it.
typedef OSStatus (*FigRegistryCreateFn)(CFAllocatorRef, CFStringRef, void **);
typedef OSStatus (*FigRegistryCopyFilteredItemListFn)(void *, void *, void *, CFArrayRef *, int *);
typedef OSStatus (*FigRegistryItemCopyDescriptionFn)(const void *, CFDictionaryRef *);
// mutable: Foundation's mutable dictionary class (CFDictionaryCreateMutable makes one); _CFDictionaryIsMutable only knows
// the old CF-native type and says 0 for every dictionary on macOS 26, frozen copies are __NSFrozenDictionaryM
static bool dict_is_mutable(CFDictionaryRef d) { const char *c = object_getClassName((id)d); return c && !strcmp(c, "__NSDictionaryM"); }

static void *find_vt_encoder_registry(void)
{
    FigRegistryCreateFn create = (FigRegistryCreateFn)dlsym(RTLD_DEFAULT, "FigRegistryCreate");
    if (!create) return NULL;
    void *mine = NULL;   // a registry of our own: its isa and type ID identify FigRegistry objects
    if (create(NULL, CFSTR("org.nvenc.registry-probe"), &mine) || !mine) return NULL;
    uintptr_t isa = ((const uintptr_t *)mine)[0]; CFTypeID tid = CFGetTypeID(mine);
    CFRelease(mine);
    CFStringRef cls = CFSTR("com.apple.videotoolbox.videoencoder");
    for (uint32_t i = 0; i < _dyld_image_count(); i++) {
        const char *n = _dyld_get_image_name(i);
        if (!n || !strstr(n, "/VideoToolbox.framework/")) continue;
        const struct mach_header_64 *h = (const struct mach_header_64 *)_dyld_get_image_header(i);
        const struct load_command *lc = (const struct load_command *)(h + 1);
        for (uint32_t k = 0; k < h->ncmds; k++, lc = (const struct load_command *)((const char *)lc + lc->cmdsize)) {
            if (lc->cmd != LC_SEGMENT_64) continue;
            const struct segment_command_64 *sg = (const struct segment_command_64 *)lc;
            // the registry pointer is written at run time: writable __DATA / __DATA_DIRTY only, not __DATA_CONST
            if (strncmp(sg->segname, "__DATA", 6) || !strcmp(sg->segname, "__DATA_CONST") || !(sg->initprot & VM_PROT_WRITE)) continue;
            const struct section_64 *sc = (const struct section_64 *)(sg + 1);
            for (uint32_t j = 0; j < sg->nsects; j++) {
                unsigned long size = 0;
                const uintptr_t *w = (const uintptr_t *)getsectiondata(h, sc[j].segname, sc[j].sectname, &size);
                for (unsigned long q = 0; w && q < size / sizeof *w; q++) {
                    const uintptr_t *o = (const uintptr_t *)w[q];
                    if ((uintptr_t)o < 0x100000000ull || ((uintptr_t)o & 7) || !malloc_zone_from_ptr(o) || malloc_size(o) < 64) continue;
                    if (o[0] != isa || CFGetTypeID((CFTypeRef)o) != tid) continue;   // a FigRegistry
                    CFTypeRef c = ((CFTypeRef const *)o)[2];
                    if (c && CFGetTypeID(c) == CFStringGetTypeID() && CFEqual(c, cls)) return (void *)o;
                }
            }
        }
    }
    return NULL;
}

static void rename_encoders(const char *const ids[], const char *const names[], const char *const codecs[], int n)
{
    if (!n || getenv("NVENCVT_NO_RENAME")) return;
    FigRegistryCopyFilteredItemListFn list = (FigRegistryCopyFilteredItemListFn)dlsym(RTLD_DEFAULT, "FigRegistryCopyFilteredItemList");
    FigRegistryItemCopyDescriptionFn desc = (FigRegistryItemCopyDescriptionFn)dlsym(RTLD_DEFAULT, "FigRegistryItemCopyDescription");
    if (!list || !desc) { LOG("rename: registry functions missing"); return; }
    void *reg = find_vt_encoder_registry();
    if (!reg) { LOG("rename: VideoToolbox's encoder registry not found"); return; }
    CFArrayRef items = NULL; int cnt = 0;
    if (list(reg, NULL, NULL, &items, &cnt) || !items) { LOG("rename: no item list"); return; }
    int done = 0;
    for (CFIndex i = 0; i < CFArrayGetCount(items); i++) {
        CFDictionaryRef d = NULL;
        if (desc(CFArrayGetValueAtIndex(items, i), &d) || !d) continue;
        CFTypeRef iid = CFDictionaryGetValue(d, CFSTR("CMClassImplementationID")), cur = CFDictionaryGetValue(d, CFSTR("VTEncoderName"));
        for (int k = 0; k < n && iid && CFGetTypeID(iid) == CFStringGetTypeID(); k++) {
            CFStringRef want = CFStringCreateWithCString(NULL, ids[k], kCFStringEncodingUTF8);
            if (CFEqual(iid, want)) LOG("rename: %s: name %s, class %s", ids[k], cur && CFGetTypeID(cur) == CFStringGetTypeID() && CFStringHasPrefix(cur, CFSTR("Dynamically Registered")) ? "default" : "other", object_getClassName((id)d));
            if (CFEqual(iid, want) && cur && CFGetTypeID(cur) == CFStringGetTypeID() &&
                CFStringHasPrefix(cur, CFSTR("Dynamically Registered")) && dict_is_mutable(d)) {
                CFStringRef nm = CFStringCreateWithCString(NULL, names[k], kCFStringEncodingUTF8), cn = CFStringCreateWithCString(NULL, codecs[k], kCFStringEncodingUTF8);
                CFDictionarySetValue((CFMutableDictionaryRef)d, CFSTR("VTEncoderName"), nm);
                CFDictionarySetValue((CFMutableDictionaryRef)d, CFSTR("VTCodecName"), cn);
                CFRelease(nm); CFRelease(cn); done++;
            }
            CFRelease(want);
        }
        CFRelease(d);
    }
    CFRelease(items);
    LOG("rename: %d of %d encoder names set", done, n);
}

// registers the H.264 (avc1) and HEVC (hvc1) encoders; returns the H.264 registration's status
__attribute__((visibility("default"))) OSStatus NVEncVT_Register(int rating)
{
    int gpu = getenv("NVENCVT_ANY_GPU") ? GPU_ADA : pci_gpu_kind();
    if (gpu < 0) gpu = gpu_nvenc_kind();   // no PCI match (unusual): ask RM for the class list
    if (gpu <= 0) {
        LOG("GPU is neither Ada, TU117 nor GA10x (NVENC class c9b7 / b4b7 / c7b7): not registering");
        return kVTCouldNotFindVideoEncoderErr;
    }
    gNvencvtGpu = gpu;
    // Not when the driver plugin was loaded from inside VTCompressionSessionCreate: VideoToolbox enumerates the Metal devices
    // while it creates a session, which loads the plugin (and so registers us) in processes without a Metal device of their
    // own (command-line tools). That session already has Apple's encoder and the next one would get ours: ffmpeg opens one
    // session for the parameter sets and another for the frames, and wrote Apple's parameter sets with our slices
    // (undecodable files; T7.06). Such processes keep Apple's encoders; nvenc-run (injection) registers before any session.
    // NVENCVT_BT=1 logs the call stack, NVENCVT_LATE=1 registers anyway.
    {
        void *bt[64]; const int nb = backtrace(bt, 64); bool inCreate = false;
        for (int i = 0; i < nb; i++) {
            Dl_info di;
            if (!dladdr(bt[i], &di)) continue;
            if (getenv("NVENCVT_BT")) LOG("bt %d %s %s", i, di.dli_fname && strrchr(di.dli_fname, '/') ? strrchr(di.dli_fname, '/') + 1 : "?", di.dli_sname ? di.dli_sname : "?");
            if (di.dli_sname && strstr(di.dli_sname, "VTCompressionSessionCreate")) inCreate = true;
        }
        if (inCreate && !getenv("NVENCVT_LATE")) {
            LOG("loaded inside VTCompressionSessionCreate (%s): not registering the encoders", getprogname());
            extern OSStatus NVDecVT_Register(int rating);
            NVDecVT_Register(rating);
            return kVTCouldNotFindVideoEncoderErr;
        }
    }
    // "NVENC - H.264 - (NVIDIA RTX 4090) - mickys/nvenc - 0.7.0": codec, the card, this project and its version (git describe)
    char nameAvc[160], nameHevc[160], nameAv1[160];
    const char *ver = NVENC_VERSION[0] == 'v' ? &NVENC_VERSION[1] : NVENC_VERSION;
    snprintf(nameAvc, sizeof nameAvc, "NVENC - H.264 - (NVIDIA %s) - mickys/nvenc - %s", gCardName, ver);
    snprintf(nameHevc, sizeof nameHevc, "NVENC - HEVC - (NVIDIA %s) - mickys/nvenc - %s", gCardName, ver);
    snprintf(nameAv1, sizeof nameAv1, "NVENC - AV1 - (NVIDIA %s) - mickys/nvenc - %s", gCardName, ver);
    CFStringRef na = CFStringCreateWithCString(NULL, nameAvc, kCFStringEncodingUTF8), nh = CFStringCreateWithCString(NULL, nameHevc, kCFStringEncodingUTF8);
    // GA10x (RTX 30): the NVENC there is not Ada's, and our encoder setups are written for NVENC 8.2 (Ada) and 6.6
    // (TU117). Registering them anyway would make VideoToolbox pick an encoder that cannot start (NullMoth's own
    // encoders stay for these cards), so only the decoders register on GA10x. NVENCVT_AMPERE_ENC=1 tries them anyway.
    const bool encoders = gpu != GPU_AMPERE || getenv("NVENCVT_AMPERE_ENC");
    if (!encoders) LOG("GA10x (RTX 30): the decoders only (NVENCVT_AMPERE_ENC=1 registers the encoders too)");
    OSStatus st = encoders ? register_one(kCMVideoCodecType_H264, CFSTR(ENCODER_ID), na, rating) : kVTCouldNotFindVideoEncoderErr;
    OSStatus sh = !encoders || getenv("NVENCVT_NO_HEVC") ? kVTCouldNotFindVideoEncoderErr
                : register_one(kCMVideoCodecType_HEVC, CFSTR(ENCODER_ID_HEVC), nh, rating);
    CFStringRef n1 = CFStringCreateWithCString(NULL, nameAv1, kCFStringEncodingUTF8);
    // AV1: Ada only (NVENC 6.6 has none)
    OSStatus s1 = !encoders || getenv("NVENCVT_NO_AV1") || gpu != GPU_ADA ? kVTCouldNotFindVideoEncoderErr : register_one(CODEC_AV1, CFSTR(ENCODER_ID_AV1), n1, rating);
    CFRelease(na); CFRelease(nh); CFRelease(n1);
    LOG("registered %s -> %d, %s -> %d, %s -> %d (rating %d) in %s", ENCODER_ID, (int)st, ENCODER_ID_HEVC, (int)sh, ENCODER_ID_AV1, (int)s1, rating, getprogname());
    const char *ids[3], *names[3], *codecs[3]; int nreg = 0;
    if (!st) { ids[nreg] = ENCODER_ID; names[nreg] = nameAvc; codecs[nreg++] = "H.264"; }
    if (!sh) { ids[nreg] = ENCODER_ID_HEVC; names[nreg] = nameHevc; codecs[nreg++] = "HEVC"; }
    if (!s1) { ids[nreg] = ENCODER_ID_AV1; names[nreg] = nameAv1; codecs[nreg++] = "AV1"; }
    rename_encoders(ids, names, codecs, nreg);
    extern OSStatus NVDecVT_Register(int rating);   // vt/nvencvt_dec.c: the H.264 decoder (NVDEC), same rating
    NVDecVT_Register(rating);
    return st;
}

// System-wide settings for apps that do not get our environment variables (started from the Dock / Finder):
// /usr/local/etc/nvencvt.conf, lines NAME=VALUE for NVENCVT_* / NVDEC_* names (e.g. NVENCVT_NO_DEC=1: no decoders),
// '#' comments. Every app reads it, so it is used only when root-owned and not writable by others; a variable already
// in the environment wins.
static void load_conf(void)
{
    const char *path = getenv("NVENCVT_CONF") ? getenv("NVENCVT_CONF") : "/usr/local/etc/nvencvt.conf";
    struct stat st;
    if (stat(path, &st) || !S_ISREG(st.st_mode) || (!getenv("NVENCVT_CONF") && (st.st_uid != 0 || (st.st_mode & 022)))) return;
    FILE *f = fopen(path, "r"); if (!f) return;
    char line[256];
    while (fgets(line, sizeof line, f)) {
        char *e = strchr(line, '='), *nl = strpbrk(line, "\r\n");
        if (nl) *nl = 0;
        if (line[0] == '#' || !e || (strncmp(line, "NVENCVT_", 8) && strncmp(line, "NVDEC_", 6))) continue;
        *e = 0;
        for (char *c = line; *c; c++) if (!((*c >= 'A' && *c <= 'Z') || (*c >= '0' && *c <= '9') || *c == '_')) { *line = 0; break; }
        if (*line) setenv(line, e + 1, 0);
    }
    fclose(f);
}

__attribute__((constructor)) static void init(void)
{
    load_conf();
    // register by itself only when injected (DYLD_INSERT_LIBRARIES); loaded by the Metal plugin (NVMTL_NVENCVT),
    // the plugin calls NVEncVT_Register
    gLog = getenv("NVENCVT_LOG") != NULL;
    if (!gLog && access("/tmp/nvencvt.log", F_OK) == 0) {
        char p[64]; snprintf(p, sizeof p, "/tmp/nvencvt-%d.log", (int)getpid());
        if ((gLogF = fopen(p, "a"))) gLog = true;
    }
    const char *ins = getenv("DYLD_INSERT_LIBRARIES");
    if (ins && strstr(ins, "nvencvt") && !getenv("NVENCVT_NO_REGISTER"))
        NVEncVT_Register(getenv("NVENCVT_RATING") ? atoi(getenv("NVENCVT_RATING")) : 1100);
}
