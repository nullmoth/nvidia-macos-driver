// nvencvt_dec.c (M8, T8.06; M9-M11, M14): the H.264, HEVC, VP9, AV1, MPEG-1/2 and MPEG-4 Part 2 decoders in libnvencvt, as VideoToolbox decoder plugins.
// Interface: Apple's SPI declarations published in WebKit (vendor/webkit-spi/VTVideoDecoderSPI.h, CMBaseObjectSPI.h); the
// three class slots that header leaves "TBD" are given the shapes their names suggest and log every call. Decoding:
// src/dec.c (H.264), src/hevcdec.c (HEVC), src/vp9dec.c (VP9), src/av1dec.c (AV1), src/mpeg2dec.c (MPEG-1/2) and
// src/mpeg4dec.c (MPEG-4 Part 2) on NVDEC.
// Registered by NVEncVT_Register next to the encoders (NVENCVT_NO_DEC=1 leaves it out), so every way the encoder gets
// into a process (NullMoth's NVMTL_NVENCVT hook, nvenc-run) brings the decoder too.
#include "dec.h"
#include "vp9dec.h"
#include "av1dec.h"
#include "hevcdec.h"
#include "mpeg2dec.h"
#include "mpeg4dec.h"
#include <CoreFoundation/CoreFoundation.h>
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <dlfcn.h>
#include <dispatch/dispatch.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>
static double nowms(void) { return clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) / 1e6; }

typedef struct CMBaseProtocolVTable CMBaseProtocolVTable;
typedef struct CMBaseProtocolTable CMBaseProtocolTable;
#include "CMBaseObjectSPI.h"

typedef CMBaseObjectRef VTVideoDecoderRef;
typedef void *VTVideoDecoderSession;
typedef void *VTVideoDecoderFrame;
extern CMBaseClassID VTVideoDecoderGetClassID(void);
extern CVPixelBufferPoolRef VTDecoderSessionGetPixelBufferPool(VTVideoDecoderSession);
extern OSStatus VTDecoderSessionSetPixelBufferAttributes(VTVideoDecoderSession, CFDictionaryRef);
extern CFDictionaryRef VTDecoderSessionGetDestinationPixelBufferAttributes(VTVideoDecoderSession);
extern OSStatus VTDecoderSessionEmitDecodedFrame(VTVideoDecoderSession, VTVideoDecoderFrame, OSStatus, VTDecodeInfoFlags, CVImageBufferRef);

#pragma pack(push, 4)
typedef struct {
    uint32_t version;
    OSStatus (*startSession)(VTVideoDecoderRef, VTVideoDecoderSession, CMVideoFormatDescriptionRef);
    OSStatus (*decodeFrame)(VTVideoDecoderRef, VTVideoDecoderFrame, CMSampleBufferRef, VTDecodeFrameFlags, VTDecodeInfoFlags *);
    OSStatus (*copySupportedPropertyDictionary)(VTVideoDecoderRef, CFDictionaryRef *);
    OSStatus (*setProperties)(VTVideoDecoderRef, CFDictionaryRef);
    OSStatus (*copySerializableProperties)(VTVideoDecoderRef, CFAllocatorRef, CFDictionaryRef *);
    Boolean  (*canAcceptFormatDescription)(VTVideoDecoderRef, CMFormatDescriptionRef);
    OSStatus (*finishDelayedFrames)(VTVideoDecoderRef);
    void *reserved[3];
} DecoderClass;
#pragma pack(pop)
typedef struct { CMBaseVTable base; const DecoderClass *decoderClass; } DecoderVTable;

#define DECODER_ID "org.nvenc.videodecoder.avc"
#define DECODER_ID_VP9 "org.nvenc.videodecoder.vp9"
#define DECODER_ID_AV1 "org.nvenc.videodecoder.av1"
#define DECODER_ID_HEVC "org.nvenc.videodecoder.hevc"
#define DECODER_ID_MPEG2 "org.nvenc.videodecoder.mpeg2"
#define DECODER_ID_MPEG1 "org.nvenc.videodecoder.mpeg1"
#define DECODER_ID_MPEG4 "org.nvenc.videodecoder.mpeg4"
static bool is_mpeg12(FourCharCode c) { return c == kCMVideoCodecType_MPEG2Video || c == kCMVideoCodecType_MPEG1Video; }
enum { CODEC_VP9 = 0x76703039, CODEC_AV1 = 0x61763031 };   // 'vp09', 'av01' (kCMVideoCodecType_VP9 / _AV1)
enum { CODEC_HEV1 = 0x68657631, CODEC_AVC3 = 0x61766333 };  // 'hev1', 'avc3': sample entries macOS has no decoder for
static bool gDLog; static FILE *gDLogF;
#define DLOG(...) do { if (gDLog) { FILE *lf_ = gDLogF ? gDLogF : stderr; fprintf(lf_, "nvdecvt: " __VA_ARGS__); fputc('\n', lf_); fflush(lf_); } } while (0)

typedef struct {
    pthread_mutex_t lock;
    VTVideoDecoderSession session;
    CMVideoFormatDescriptionRef fmt;
    FourCharCode codec;
    Rm rm; H264Dec dec; bool opened;
    Rm *vrm; Vp9Dec *vp9;           // VP9: on the heap, so that the next session can take them over (park_vp9)
    Av1Dec *av1;                    // AV1: likewise (with vrm)
    HevcDec *hevc;                  // HEVC (with rm)
    Mpeg2Dec *m2;                   // MPEG-1/2 (with rm)
    Mpeg4Dec *m4;                   // MPEG-4 Part 2 (with rm)
    int nalLen, w, h;
    OSType pf;                      // output pixel format (VP9: depends on the bit depth)
    CVPixelBufferPoolRef ownPool;   // our own pool, made with the client's destination attributes (see set_output)
    CFDictionaryRef colour;         // colour attachments for the output (from the format description)
    unsigned frames, errors;
    double tIn, tEngine, tPool, tCopy, tEmit, tOut, tLast;   // ms: inside decodeFrame, engine wait, pool, untile, emit; between calls
    uint8_t *tmp; size_t tmpCap;    // a non-contiguous sample, flattened
    // asynchronous output (T8.09): pictures waiting for the output thread (copy, emit, release), in decode order
    struct OutJob { VTVideoDecoderFrame frame; OSStatus res; VTDecodeInfoFlags fl; CVPixelBufferRef pb; int surf, rel; unsigned w, h, cx, cy, usCopy, usEmit; } q[4];
    unsigned qh, qt; bool async, thrOn, stop; pthread_t thr; pthread_cond_t cv;
} DState;
typedef struct OutJob OutJob;

static DState *dstate(VTVideoDecoderRef o) { return *(DState **)CMBaseObjectGetDerivedStorage(o); }

// ---- VP9 sessions hand their stream over ----
// ffmpeg re-creates the VideoToolbox session whenever a VP9 frame changes size (inter-frame resizing, spatial SVC); the
// next frame then refers to pictures only the old session has. So a VP9 decoder is parked for a few seconds when its
// session ends, and the next VP9 session of the same bit depth in this process takes it over (a larger one copies its
// references over, vp9dec_migrate). A new stream starts with a key frame, which resets everything taken over.
static pthread_mutex_t gParkLock = PTHREAD_MUTEX_INITIALIZER;
static Rm *gParkRm; static Vp9Dec *gParkDec; static unsigned gParkGen;

static void free_vp9(Rm *rm, Vp9Dec *d) { if (d) { vp9dec_close(d); free(d); } if (rm) { rm_close(rm); free(rm); } }

static void park_vp9(Rm *rm, Vp9Dec *d)
{
    pthread_mutex_lock(&gParkLock);
    Rm *orm = gParkRm; Vp9Dec *od = gParkDec;
    gParkRm = rm; gParkDec = d; const unsigned gen = ++gParkGen;
    pthread_mutex_unlock(&gParkLock);
    free_vp9(orm, od);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        pthread_mutex_lock(&gParkLock);
        Rm *r = NULL; Vp9Dec *x = NULL;
        if (gParkGen == gen) { r = gParkRm; x = gParkDec; gParkRm = NULL; gParkDec = NULL; }
        pthread_mutex_unlock(&gParkLock);
        free_vp9(r, x);
    });
}

// AV1 the same way, without growing: a session is sized for the sequence header's largest frame
static Rm *gParkRmA; static Av1Dec *gParkA; static unsigned gParkGenA;
static void free_av1(Rm *rm, Av1Dec *d) { if (d) { av1dec_close(d); free(d); } if (rm) { rm_close(rm); free(rm); } }
static void park_av1(Rm *rm, Av1Dec *d)
{
    pthread_mutex_lock(&gParkLock);
    Rm *orm = gParkRmA; Av1Dec *od = gParkA;
    gParkRmA = rm; gParkA = d; const unsigned gen = ++gParkGenA;
    pthread_mutex_unlock(&gParkLock);
    free_av1(orm, od);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 3 * NSEC_PER_SEC), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
        pthread_mutex_lock(&gParkLock);
        Rm *r = NULL; Av1Dec *x = NULL;
        if (gParkGenA == gen) { r = gParkRmA; x = gParkA; gParkRmA = NULL; gParkA = NULL; }
        pthread_mutex_unlock(&gParkLock);
        free_av1(r, x);
    });
}
static int open_av1(DState *s, unsigned w, unsigned h, int depth)
{
    pthread_mutex_lock(&gParkLock);
    Rm *prm = NULL; Av1Dec *pd = NULL;
    if (gParkA && gParkA->bit_depth == depth && gParkA->maxW >= w && gParkA->maxH >= h) { prm = gParkRmA; pd = gParkA; gParkRmA = NULL; gParkA = NULL; gParkGenA++; }
    pthread_mutex_unlock(&gParkLock);
    if (pd) { s->vrm = prm; s->av1 = pd; DLOG("startSession: took over the previous session's stream"); return 0; }
    Rm *rm = calloc(1, sizeof *rm); Av1Dec *d = calloc(1, sizeof *d);
    if (!rm || !d || rm_open(rm) || rm_va_init(rm) || av1dec_open(d, rm, w, h, depth)) { free(d); if (rm) { rm_close(rm); free(rm); } return -1; }
    s->vrm = rm; s->av1 = d;
    return 0;
}

// A decoder for a new session: the parked one if it fits (or a larger one that takes over its stream), else a new one.
static int open_vp9(DState *s, unsigned w, unsigned h, int depth)
{
    pthread_mutex_lock(&gParkLock);
    Rm *prm = NULL; Vp9Dec *pd = NULL;
    if (gParkDec && gParkDec->bit_depth == depth) { prm = gParkRm; pd = gParkDec; gParkRm = NULL; gParkDec = NULL; gParkGen++; }
    pthread_mutex_unlock(&gParkLock);
    if (pd && pd->maxW >= w && pd->maxH >= h) { s->vrm = prm; s->vp9 = pd; DLOG("startSession: took over the previous session's stream"); return 0; }
    Rm *rm = prm ? prm : calloc(1, sizeof *rm);
    Vp9Dec *d = calloc(1, sizeof *d);
    if (!rm || !d || (!prm && (rm_open(rm) || rm_va_init(rm))) ||
        vp9dec_open(d, rm, pd && pd->maxW > w ? pd->maxW : w, pd && pd->maxH > h ? pd->maxH : h, depth)) {
        free(d); if (pd) { vp9dec_close(pd); free(pd); } if (rm) { rm_close(rm); free(rm); }
        return -1;
    }
    if (pd) { vp9dec_migrate(d, pd); vp9dec_close(pd); free(pd); DLOG("startSession: took over the previous session's stream (grown)"); }
    s->vrm = rm; s->vp9 = d;
    return 0;
}

// ---- base object ----
static void out_drain(DState *s);
static OSStatus b_invalidate(CMBaseObjectRef o)
{
    DState *s = dstate(o); if (!s) return noErr;
    pthread_mutex_lock(&s->lock);
    if (s->thrOn && !pthread_equal(pthread_self(), s->thr)) {   // the waiting pictures out, then the output thread ends
        out_drain(s);
        s->stop = true; pthread_cond_broadcast(&s->cv);
        pthread_mutex_unlock(&s->lock); pthread_join(s->thr, NULL); pthread_mutex_lock(&s->lock);
        s->thrOn = false;
    }
    if (s->opened) {
        if (s->codec == CODEC_VP9) { park_vp9(s->vrm, s->vp9); s->vrm = NULL; s->vp9 = NULL; }
        else if (s->codec == CODEC_AV1) { park_av1(s->vrm, s->av1); s->vrm = NULL; s->av1 = NULL; }
        else if (s->codec == kCMVideoCodecType_HEVC) { hevcdec_close(s->hevc); free(s->hevc); s->hevc = NULL; rm_close(&s->rm); }
        else if (is_mpeg12(s->codec)) { mpeg2dec_close(s->m2); free(s->m2); s->m2 = NULL; rm_close(&s->rm); }
        else if (s->codec == kCMVideoCodecType_MPEG4Video) { mpeg4dec_close(s->m4); free(s->m4); s->m4 = NULL; rm_close(&s->rm); }
        else { h264dec_close(&s->dec); rm_close(&s->rm); }
        s->opened = false;
    }
    if (s->fmt) { CFRelease(s->fmt); s->fmt = NULL; }
    if (s->ownPool) { CVPixelBufferPoolRelease(s->ownPool); s->ownPool = NULL; }
    if (s->colour) { CFRelease(s->colour); s->colour = NULL; }
    if (s->frames) DLOG("per frame: decodeFrame %.2f ms (engine %.2f, pool %.2f, copy %.2f, emit %.2f), outside %.2f ms",
         s->tIn / s->frames, s->tEngine / s->frames, s->tPool / s->frames, s->tCopy / s->frames, s->tEmit / s->frames, s->tOut / s->frames);
    DLOG("invalidate after %u frames, %u errors", s->frames, s->errors);
    pthread_mutex_unlock(&s->lock);
    return noErr;
}
static void b_finalize(CMBaseObjectRef o)
{
    DState *s = dstate(o); if (!s) return;
    b_invalidate(o); pthread_cond_destroy(&s->cv); pthread_mutex_destroy(&s->lock); free(s->tmp); free(s);
    *(DState **)CMBaseObjectGetDerivedStorage(o) = NULL;
}
static CFStringRef b_desc(CMBaseObjectRef o)
{
    const FourCharCode c = dstate(o)->codec;
    return c == CODEC_VP9 ? CFSTR("nvenc VP9 decoder (NVDEC)") : c == CODEC_AV1 ? CFSTR("nvenc AV1 decoder (NVDEC)") :
           c == kCMVideoCodecType_HEVC ? CFSTR("nvenc HEVC decoder (NVDEC)") : is_mpeg12(c) ? CFSTR("nvenc MPEG-1/2 decoder (NVDEC)") :
           c == kCMVideoCodecType_MPEG4Video ? CFSTR("nvenc MPEG-4 Part 2 decoder (NVDEC)") :
           CFSTR("nvenc H.264 decoder (NVDEC)");
}
static OSStatus b_copyProperty(CMBaseObjectRef o, CFStringRef key, CFAllocatorRef al, void *out)
{
    (void)o; (void)al;
    if (CFEqual(key, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder)) { *(CFTypeRef *)out = kCFBooleanTrue; return noErr; }
    if (gDLog) { char k[128]; CFStringGetCString(key, k, sizeof k, kCFStringEncodingUTF8); DLOG("copy %s: not available", k); }
    return kCMBaseObjectError_ValueNotAvailable;
}
static OSStatus b_setProperty(CMBaseObjectRef o, CFStringRef key, CFTypeRef v)
{
    (void)o; (void)v;
    if (gDLog) { char k[128]; CFStringGetCString(key, k, sizeof k, kCFStringEncodingUTF8); DLOG("set %s (ignored)", k); }
    return noErr;
}

// ---- decoder class ----
static OSStatus set_output(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt, OSType pf, bool full);

// VP9: size from the format description, bit depth and range from its vpcC atom (VP Codec ISO Media File Format Binding:
// version/flags, profile, level, bitDepth(4) chromaSubsampling(3) videoFullRangeFlag(1), ...)
static OSStatus start_vp9(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    const CMVideoDimensions dim = CMVideoFormatDescriptionGetDimensions(fmt);
    int depth = 8, chroma = 1; bool full = false;
    CFDictionaryRef atoms = CMFormatDescriptionGetExtension(fmt, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
    CFDataRef vpcc = atoms && CFGetTypeID(atoms) == CFDictionaryGetTypeID() ? CFDictionaryGetValue(atoms, CFSTR("vpcC")) : NULL;
    if (vpcc && CFGetTypeID(vpcc) == CFDataGetTypeID() && CFDataGetLength(vpcc) >= 8) {
        const uint8_t *b = CFDataGetBytePtr(vpcc);
        depth = b[6] >> 4; chroma = (b[6] >> 1) & 7; full = b[6] & 1;
    }
    // 4:2:0 (chromaSubsampling 0 or 1), 8 or 10 bits (12-bit decodes, but CoreVideo has no 12-bit 4:2:0 biplanar format)
    // TU117 (GTX 1650): no frames of 64 or less (vp9dec.c), so another decoder takes those; GA10x is not tried with
    // those sizes either (NVIDIA lists 128x128 as VP9's minimum), so they are refused there as well: only Ada takes them.
    extern int gNvencvtGpu;
    const bool ok = chroma <= 1 && (depth == 8 || depth == 10) && dim.width <= 8192 && dim.height <= 8192 &&
                    (gNvencvtGpu == 1 || (dim.width > 64 && dim.height > 64));
    DLOG("startSession: VP9 %dx%d, %d-bit, chroma %d, %s range%s", dim.width, dim.height, depth, chroma, full ? "full" : "video",
         vpcc ? "" : " (no vpcC: assumed)");
    if (!ok) return kVTVideoDecoderUnsupportedDataFormatErr;
    s->w = dim.width; s->h = dim.height;
    if (open_vp9(s, (unsigned)dim.width, (unsigned)dim.height, depth)) {
        DLOG("startSession: NVDEC session failed"); return kVTVideoDecoderMalfunctionErr;
    }
    s->opened = true;
    const OSType pf = depth == 8 ? (full ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
                                 : (full ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange);
    return set_output(s, session, fmt, pf, full);
}

// AV1: the av1C atom (AV1 Codec ISO Media File Format Binding: marker/version, profile/level, tier, high_bitdepth,
// twelve_bit, monochrome, subsampling, ..., then the configuration OBUs, normally the sequence header), whose sequence
// header the decoder reads first: it gives the largest frame size, which the session is sized for
static OSStatus start_av1(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    const CMVideoDimensions dim = CMVideoFormatDescriptionGetDimensions(fmt);
    CFDictionaryRef atoms = CMFormatDescriptionGetExtension(fmt, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
    CFDataRef av1c = atoms && CFGetTypeID(atoms) == CFDictionaryGetTypeID() ? CFDictionaryGetValue(atoms, CFSTR("av1C")) : NULL;
    int depth = 8; unsigned W = (unsigned)dim.width, H = (unsigned)dim.height; bool full = false, ok = true;
    const uint8_t *cfg = NULL; size_t cfgN = 0;
    if (av1c && CFGetTypeID(av1c) == CFDataGetTypeID() && CFDataGetLength(av1c) >= 4) {
        const uint8_t *b = CFDataGetBytePtr(av1c);
        const int high = (b[2] >> 6) & 1, twelve = (b[2] >> 5) & 1, ssx = (b[2] >> 3) & 1, ssy = (b[2] >> 2) & 1, mono = (b[2] >> 4) & 1;
        depth = twelve ? 12 : high ? 10 : 8;
        ok = (ssx && ssy) || mono;
        cfg = b + 4; cfgN = (size_t)CFDataGetLength(av1c) - 4;
        // the sequence header: largest frame size and colour range
        for (size_t pos = 0; pos < cfgN;) {
            Av1Obu o; if (av1_obu(cfg + pos, cfgN - pos, &o)) break;
            Av1Seq sq;
            if (o.type == AV1_OBU_SEQUENCE_HEADER && !av1_parse_sequence_header(&sq, cfg + pos + o.header_len, o.payload_len)) {
                if ((unsigned)sq.max_frame_width_minus_1 + 1 > W) W = (unsigned)sq.max_frame_width_minus_1 + 1;
                if ((unsigned)sq.max_frame_height_minus_1 + 1 > H) H = (unsigned)sq.max_frame_height_minus_1 + 1;
                full = sq.color_range;
            }
            pos += o.header_len + o.payload_len;
        }
    }
    ok = ok && (depth == 8 || depth == 10) && W <= 8192 && H <= 8192 && dim.width >= 128 && dim.height >= 128;
    DLOG("startSession: AV1 %dx%d (largest %ux%u), %d-bit, %s range%s%s", dim.width, dim.height, W, H, depth, full ? "full" : "video",
         av1c ? "" : " (no av1C: assumed)", ok ? "" : " - NOT SUPPORTED");
    if (!ok) return kVTVideoDecoderUnsupportedDataFormatErr;
    s->w = dim.width; s->h = dim.height;
    if (open_av1(s, W, H, depth)) { DLOG("startSession: NVDEC session failed"); return kVTVideoDecoderMalfunctionErr; }
    s->opened = true;
    if (cfgN) av1dec_tu(s->av1, cfg, cfgN);   // the sequence header (samples may leave it out)
    const OSType pf = depth == 8 ? (full ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
                                 : (full ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange);
    return set_output(s, session, fmt, pf, full);
}

// HEVC: the parameter sets of the hvcC atom (the session is sized from the largest SPS and fed all of them first). What
// NVDEC on Ada decodes and CoreVideo has a biplanar format for: 4:2:0 and 4:4:4, 8 or 10 bits, up to 8192x8192.
static OSStatus start_hevc(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    size_t count = 0; int nalLen = 4;
    OSStatus st = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fmt, 0, NULL, NULL, &count, &nalLen);
    if (st || !count) { DLOG("startSession: no HEVC parameter sets (%d)", (int)st); return kVTVideoDecoderBadDataErr; }
    s->nalLen = nalLen;
    HevcParams *pp = calloc(1, sizeof *pp); if (!pp) return kVTVideoDecoderMalfunctionErr;
    unsigned W = 0, H = 0; int depth = 0, cdepth = 0, cf = -1, cw = 0, ch = 0; bool full = false;
    for (size_t i = 0; i < count; i++) {
        const uint8_t *ps; size_t n;
        if (CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fmt, i, &ps, &n, NULL, NULL) || n < 2 || ((ps[0] >> 1) & 63) != HEVC_NAL_SPS) continue;
        const int id = hevc_parse_sps(pp, ps, n); if (id < 0) continue;
        const HevcSps *sp = &pp->sps[id];
        if ((unsigned)sp->width > W) W = (unsigned)sp->width;
        if ((unsigned)sp->height > H) H = (unsigned)sp->height;
        if (cf < 0) {
            depth = sp->bit_depth_luma; cdepth = sp->bit_depth_chroma; cf = sp->chroma_format_idc; full = sp->video_full_range_flag;
            cw = sp->width - sp->conf_win_left - sp->conf_win_right; ch = sp->height - sp->conf_win_top - sp->conf_win_bottom;
        }
    }
    free(pp);
    const bool ok = cf >= 0 && (cf == 1 || cf == 3) && (depth == 8 || depth == 10) && cdepth == depth && W <= 8192 && H <= 8192;
    DLOG("startSession: HEVC %dx%d (coded %ux%u), %d-bit, chroma %d, %s range%s", cw, ch, W, H, depth, cf, full ? "full" : "video", ok ? "" : " - NOT SUPPORTED");
    if (!ok) return kVTVideoDecoderUnsupportedDataFormatErr;
    s->w = cw; s->h = ch;
    if (!(s->hevc = calloc(1, sizeof *s->hevc)) || rm_open(&s->rm) || rm_va_init(&s->rm) || hevcdec_open(s->hevc, &s->rm, W, H, depth, cf)) {
        DLOG("startSession: NVDEC session failed"); free(s->hevc); s->hevc = NULL; return kVTVideoDecoderMalfunctionErr;
    }
    s->opened = true;
    for (size_t i = 0; i < count; i++) {                        // the decoder keeps its own parameter sets
        const uint8_t *ps; size_t n;
        if (!CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fmt, i, &ps, &n, NULL, NULL) && n) hevcdec_nal(s->hevc, ps, n);
    }
    const OSType pf = cf == 3 ? (depth == 8 ? (full ? kCVPixelFormatType_444YpCbCr8BiPlanarFullRange : kCVPixelFormatType_444YpCbCr8BiPlanarVideoRange)
                                            : (full ? kCVPixelFormatType_444YpCbCr10BiPlanarFullRange : kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange))
                              : (depth == 8 ? (full ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
                                            : (full ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange));
    return set_output(s, session, fmt, pf, full);
}

// MPEG-1/2 (M14): sized from the format description (NVDEC: up to 4080x4080, 4:2:0); the sequence header normally
// travels in the samples, else (or until then) a default sequence of that size is assumed
static OSStatus start_mpeg2(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    const CMVideoDimensions dm = CMVideoFormatDescriptionGetDimensions(fmt);
    if (gDLog) { CFDictionaryRef ex = CMFormatDescriptionGetExtensions(fmt); CFStringRef ds = ex ? CFCopyDescription(ex) : NULL; char b[1024] = "(none)";
                 if (ds) { CFStringGetCString(ds, b, sizeof b, kCFStringEncodingUTF8); CFRelease(ds); } for (char *c = b; *c; c++) if (*c == '\n') *c = ' ';
                 DLOG("startSession: MPEG-%d %dx%d, extensions %s", s->codec == kCMVideoCodecType_MPEG1Video ? 1 : 2, dm.width, dm.height, b); }
    if (dm.width <= 0 || dm.height <= 0 || dm.width > 4080 || dm.height > 4080) return kVTVideoDecoderUnsupportedDataFormatErr;
    s->w = dm.width; s->h = dm.height;
    if (!(s->m2 = calloc(1, sizeof *s->m2)) || rm_open(&s->rm) || rm_va_init(&s->rm) || mpeg2dec_open(s->m2, &s->rm, (unsigned)dm.width, (unsigned)dm.height)) {
        DLOG("startSession: NVDEC session failed"); free(s->m2); s->m2 = NULL; return kVTVideoDecoderMalfunctionErr;
    }
    mpeg2dec_default_sequence(s->m2, dm.width, dm.height, s->codec == kCMVideoCodecType_MPEG1Video);
    s->m2->haveSeq = false;   // a sequence header in the stream replaces it; mpeg2dec_unit falls back to it below
    s->opened = true;
    return set_output(s, session, fmt, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, false);
}

// MPEG-4 Part 2 (M14, T14.02): the VOL from the esds atom's decoder specific info (its start codes found by a scan);
// Simple and Advanced Simple profiles (Apple's own decoder takes Simple only); no data partitioning, no GMC
static OSStatus start_mpeg4(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    const CMVideoDimensions dm = CMVideoFormatDescriptionGetDimensions(fmt);
    // NVDEC's MPEG-4 limit is 8192 macroblocks (1080p: 8160); larger pictures (2064x1088, 2048x2048) come back with every
    // macroblock flagged as an error and take 10-20 times as long (GTX 1650, T15.11)
    if (dm.width <= 0 || dm.height <= 0 || dm.width > 2048 || dm.height > 2048 || ((dm.width + 15) / 16) * ((dm.height + 15) / 16) > 8192)
        return kVTVideoDecoderUnsupportedDataFormatErr;
    s->w = dm.width; s->h = dm.height;
    if (!(s->m4 = calloc(1, sizeof *s->m4)) || rm_open(&s->rm) || rm_va_init(&s->rm) || mpeg4dec_open(s->m4, &s->rm, (unsigned)dm.width, (unsigned)dm.height)) {
        DLOG("startSession: NVDEC session failed"); free(s->m4); s->m4 = NULL; return kVTVideoDecoderMalfunctionErr;
    }
    s->opened = true;
    CFDictionaryRef atoms = CMFormatDescriptionGetExtension(fmt, kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms);
    CFDataRef esds = atoms && CFGetTypeID(atoms) == CFDictionaryGetTypeID() ? CFDictionaryGetValue(atoms, CFSTR("esds")) : NULL;
    int units = 0;
    if (esds && CFGetTypeID(esds) == CFDataGetTypeID()) {
        const uint8_t *p = CFDataGetBytePtr(esds); const size_t n = (size_t)CFDataGetLength(esds);
        size_t pos = 0; const uint8_t *st; size_t len;
        while (mpeg4_next_unit(p, n, &pos, &st, &len)) { if (mpeg4dec_unit(s->m4, st, len) < 0) return kVTVideoDecoderUnsupportedDataFormatErr; units++; }
    }
    DLOG("startSession: MPEG-4 %dx%d, esds %ld bytes, %d units, VOL %s", dm.width, dm.height, esds ? (long)CFDataGetLength(esds) : -1L, units,
         s->m4->vol.valid ? "ok" : "not yet (expected in the samples)");
    return set_output(s, session, fmt, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, false);
}

static OSStatus d_startSession(VTVideoDecoderRef o, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt)
{
    DState *s = dstate(o);
    pthread_mutex_lock(&s->lock);
    s->session = session; s->fmt = (CMVideoFormatDescriptionRef)CFRetain(fmt);
    if (s->codec == CODEC_VP9) { OSStatus r = start_vp9(s, session, fmt); pthread_mutex_unlock(&s->lock); return r; }
    if (s->codec == CODEC_AV1) { OSStatus r = start_av1(s, session, fmt); pthread_mutex_unlock(&s->lock); return r; }
    if (s->codec == kCMVideoCodecType_HEVC) { OSStatus r = start_hevc(s, session, fmt); pthread_mutex_unlock(&s->lock); return r; }
    if (is_mpeg12(s->codec)) { OSStatus r = start_mpeg2(s, session, fmt); pthread_mutex_unlock(&s->lock); return r; }
    if (s->codec == kCMVideoCodecType_MPEG4Video) { OSStatus r = start_mpeg4(s, session, fmt); pthread_mutex_unlock(&s->lock); return r; }
    size_t count = 0; int nalLen = 4;
    OSStatus st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, 0, NULL, NULL, &count, &nalLen);
    if (st || !count) { DLOG("startSession: no H.264 parameter sets (%d)", (int)st); pthread_mutex_unlock(&s->lock); return kVTVideoDecoderBadDataErr; }
    s->nalLen = nalLen;
    // size the session from the SPS (coded size), check what NVDEC on Ada decodes: 8-bit 4:2:0
    H264Params *pp = calloc(1, sizeof *pp); int sps = -1;
    for (size_t i = 0; i < count; i++) {
        const uint8_t *ps; size_t n;
        if (CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, i, &ps, &n, NULL, NULL) || !n) continue;
        if ((ps[0] & 31) == H264_NAL_SPS && !h264_parse_sps(pp, ps, n)) for (int k = 0; k < 32; k++) if (pp->sps[k].valid) { sps = k; break; }
    }
    if (sps < 0) { free(pp); pthread_mutex_unlock(&s->lock); return kVTVideoDecoderBadDataErr; }
    const H264Sps *sp = &pp->sps[sps];
    const unsigned W = (unsigned)(sp->pic_width_in_mbs_minus1 + 1) * 16, H = (unsigned)(sp->pic_height_in_map_units_minus1 + 1) * 16 * (2u - (unsigned)sp->frame_mbs_only_flag);
    const bool ok = sp->chroma_format_idc == 1 && sp->bit_depth_luma_minus8 == 0 && sp->bit_depth_chroma_minus8 == 0 && W <= 4096 && H <= 4096;
    s->w = sp->width; s->h = sp->height;
    const bool full = sp->video_full_range_flag;
    DLOG("startSession: %dx%d (coded %ux%u) profile %d chroma %d depth %d %s", s->w, s->h, W, H, sp->profile_idc, sp->chroma_format_idc,
         sp->bit_depth_luma_minus8 + 8, ok ? "" : "- NOT SUPPORTED");
    free(pp);
    if (!ok) { pthread_mutex_unlock(&s->lock); return kVTVideoDecoderUnsupportedDataFormatErr; }
    if (rm_open(&s->rm) || rm_va_init(&s->rm) || h264dec_open(&s->dec, &s->rm, W, H)) {
        DLOG("startSession: NVDEC session failed"); pthread_mutex_unlock(&s->lock); return kVTVideoDecoderMalfunctionErr;
    }
    s->opened = true;
    for (size_t i = 0; i < count; i++) {                        // the decoder keeps its own parameter sets
        const uint8_t *ps; size_t n;
        if (!CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, i, &ps, &n, NULL, NULL) && n) h264dec_nal(&s->dec, ps, n);
    }
    st = set_output(s, session, fmt, full ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, full);
    pthread_mutex_unlock(&s->lock);
    return st;
}

// The pixel buffer attributes for the session (our format, the range the stream says) and the colour attachments
static OSStatus set_output(DState *s, VTVideoDecoderSession session, CMVideoFormatDescriptionRef fmt, OSType pf, bool full)
{
    OSStatus st;
    s->pf = pf;
    if (gDLog) {   // what the client asked for (T8.06: matching it avoids a conversion inside EmitDecodedFrame)
        CFDictionaryRef dst = VTDecoderSessionGetDestinationPixelBufferAttributes(session);
        CFStringRef ds = dst ? CFCopyDescription(dst) : NULL; char buf[2048] = "(none)";
        if (ds) { CFStringGetCString(ds, buf, sizeof buf, kCFStringEncodingUTF8); CFRelease(ds); }
        for (char *c = buf; *c; c++) if (*c == '\n') *c = ' ';
        DLOG("destination attributes: %s", buf);
    }
    CFNumberRef npf = CFNumberCreate(NULL, kCFNumberSInt32Type, &pf), nw = CFNumberCreate(NULL, kCFNumberIntType, &s->w), nh = CFNumberCreate(NULL, kCFNumberIntType, &s->h);
    CFDictionaryRef ios = CFDictionaryCreate(NULL, NULL, NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *k[] = { kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey };
    const void *v[] = { npf, nw, nh, ios };
    CFDictionaryRef attrs = CFDictionaryCreate(NULL, k, v, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    // When the client asks for the format we produce, hand out buffers made exactly to its attributes: otherwise
    // VideoToolbox copies every frame into a compatible one inside EmitDecodedFrame (ffmpeg asks for OpenGL-compatible
    // IOSurfaces: 18.7 ms per 4K frame).
    CFDictionaryRef dst = VTDecoderSessionGetDestinationPixelBufferAttributes(session);
    if (dst) CFRetain(dst);   // also for our own pool below (released at its end)
    CFNumberRef dpf = dst ? CFDictionaryGetValue(dst, kCVPixelBufferPixelFormatTypeKey) : NULL; OSType want = 0;
    if (!getenv("NVDEC_OWNATTRS") && dpf && CFGetTypeID(dpf) == CFNumberGetTypeID() && CFNumberGetValue(dpf, kCFNumberSInt32Type, &want) && want == pf) {
        CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(NULL, 0, dst);
        CFDictionarySetValue(m, kCVPixelBufferWidthKey, nw); CFDictionarySetValue(m, kCVPixelBufferHeightKey, nh);
        if (!CFDictionaryContainsKey(m, kCVPixelBufferIOSurfacePropertiesKey)) CFDictionarySetValue(m, kCVPixelBufferIOSurfacePropertiesKey, ios);
        CFRelease(attrs); attrs = m;
    }
    if (!getenv("NVDEC_NOMETAL")) {   // MediaToolbox's conformer copies every frame that is not Metal-compatible (T8.06)
        CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(NULL, 0, attrs);
        CFDictionarySetValue(m, kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
        CFRelease(attrs); attrs = m;
    }
    st = VTDecoderSessionSetPixelBufferAttributes(session, attrs);
    CFRelease(attrs); CFRelease(ios); CFRelease(npf); CFRelease(nw); CFRelease(nh);
    // Our own pool, made with the client's destination attributes verbatim (PR #1). A client that asks for an
    // OpenGL-compatible buffer (ffmpeg, mpv: kCVPixelBufferIOSurfaceOpenGLTextureCompatibilityKey) cannot have one on
    // this system: CVPixelBufferIsCompatibleWithAttributes answers 0 for every buffer, even one made with those very
    // attributes. VideoToolbox then transfers every decoded picture into a buffer of its own (14-32 ms per 4K frame
    // through the GPU driver's Metal path, RTX 3060 Ti and RTX 4090), instead of handing the decoder's buffer over. The picture is copied into the
    // buffer we hand out, so if that buffer carries the destination's own attributes VideoToolbox has nothing to transfer.
    // Only when such a buffer holds our picture as it is: the client's pixel format is ours (one format, not a list) and
    // its size, if it names one, is the picture's; anything else (BGRA, 8-bit for a 10-bit stream, a scaled size) keeps
    // the session's pool and VideoToolbox's conversion. NVDEC_NOOWNPOOL=1 leaves it out. (T8.07 measured the transfer.)
    if (s->ownPool) { CVPixelBufferPoolRelease(s->ownPool); s->ownPool = NULL; }
    if (dst && !getenv("NVDEC_NOOWNPOOL")) {
        int dw = s->w, dh = s->h; CFNumberRef x;
        if ((x = CFDictionaryGetValue(dst, kCVPixelBufferWidthKey)) && CFGetTypeID(x) == CFNumberGetTypeID()) CFNumberGetValue(x, kCFNumberIntType, &dw);
        if ((x = CFDictionaryGetValue(dst, kCVPixelBufferHeightKey)) && CFGetTypeID(x) == CFNumberGetTypeID()) CFNumberGetValue(x, kCFNumberIntType, &dh);
        const bool fits = want == pf && dw == s->w && dh == s->h;
        if (fits) {
            CFMutableDictionaryRef m = CFDictionaryCreateMutableCopy(NULL, 0, dst);   // the size made explicit (a pool needs one)
            CFNumberRef w2 = CFNumberCreate(NULL, kCFNumberIntType, &s->w), h2 = CFNumberCreate(NULL, kCFNumberIntType, &s->h);
            CFDictionarySetValue(m, kCVPixelBufferWidthKey, w2); CFDictionarySetValue(m, kCVPixelBufferHeightKey, h2);
            CFRelease(w2); CFRelease(h2);
            CVPixelBufferPoolRef pool = NULL;
            if (CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL, m, &pool) == kCVReturnSuccess && pool) s->ownPool = pool;
            else if (pool) CVPixelBufferPoolRelease(pool);
            CFRelease(m);
        }
        DLOG("  our own pool: %s", s->ownPool ? "yes (the client's destination attributes)" : fits ? "no (CVPixelBufferPoolCreate failed)"
                                    : "no (the client's format or size is not the picture's: the session's pool, VideoToolbox converts)");
    }
    if (dst) CFRelease(dst);
    CFMutableDictionaryRef col = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    // colour description and HDR metadata from the format description (the extension keys are the attachment keys), as
    // Apple's decoders attach them
    const CFStringRef ck[] = { kCMFormatDescriptionExtension_ColorPrimaries, kCMFormatDescriptionExtension_TransferFunction, kCMFormatDescriptionExtension_YCbCrMatrix,
                               kCMFormatDescriptionExtension_MasteringDisplayColorVolume, kCMFormatDescriptionExtension_ContentLightLevelInfo };
    for (int i = 0; i < 5; i++) { CFTypeRef x = CMFormatDescriptionGetExtension(fmt, ck[i]); if (x) CFDictionarySetValue(col, ck[i], x); }
    s->colour = col;
    DLOG("startSession: pixel buffer attributes -> %d (%s range)", (int)st, full ? "full" : "video");
    return noErr;
}

// A buffer for the picture: from our own pool (made with the client's destination attributes, so that VideoToolbox
// hands the picture over instead of transferring it) or the session's pool, or for a frame of another size (VP9 / AV1
// resizing) one made with the pool's attributes but its own size (the pool's Width / Height keys would win over
// CVPixelBufferCreate's arguments)
static OSStatus output_buffer(DState *s, unsigned w, unsigned h, CVPixelBufferRef *pb)
{
    if (!getenv("NVDEC_KEEPSIZE") && ((int)w != s->w || (int)h != s->h)) {
        // a new frame size: give VideoToolbox pixel buffer attributes of that size (else it converts each picture to the
        // old size for clients like ffmpeg that keep their session across the change: AV1 after a new sequence header)
        DLOG("frame size %dx%d -> %ux%u: new pixel buffer attributes", s->w, s->h, w, h);
        s->w = (int)w; s->h = (int)h;
        set_output(s, s->session, s->fmt, s->pf, s->pf == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange || s->pf == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange);
    }
    CVPixelBufferPoolRef pool = s->ownPool ? s->ownPool : VTDecoderSessionGetPixelBufferPool(s->session);
    if (pool && (int)w == s->w && (int)h == s->h) return CVPixelBufferPoolCreatePixelBuffer(NULL, pool, pb);
    CFDictionaryRef pa = pool ? CVPixelBufferPoolGetPixelBufferAttributes(pool) : NULL;
    CFMutableDictionaryRef a = pa ? CFDictionaryCreateMutableCopy(NULL, 0, pa)
                                  : CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFDictionaryRemoveValue(a, kCVPixelBufferWidthKey); CFDictionaryRemoveValue(a, kCVPixelBufferHeightKey);
    const OSStatus r = CVPixelBufferCreate(NULL, w, h, s->pf, a, pb);
    CFRelease(a);
    return r;
}

// ---- output (T8.09) ----
// A decoded picture is copied into its pixel buffer (untiled), emitted, and its surface released. Clients that allow
// asynchronous decompression (kVTDecodeFrame_EnableAsynchronousDecompression: AVFoundation) get that from an output
// thread while the engine decodes the next picture; synchronous ones (ffmpeg) inside decodeFrame. Every frame of an
// asynchronous run goes through the queue (errors and dropped frames too), so they come out in decode order.
static void out_copy(DState *s, OutJob *j)
{
    if (!j->pb || j->surf < 0 || j->res) return;
    const double tc = nowms();
    CVPixelBufferRef pb = j->pb;
    CVPixelBufferLockBaseAddress(pb, 0);
    uint8_t *y = CVPixelBufferGetBaseAddressOfPlane(pb, 0), *uv = CVPixelBufferGetBaseAddressOfPlane(pb, 1);
    const size_t ys = CVPixelBufferGetBytesPerRowOfPlane(pb, 0), uvs = CVPixelBufferGetBytesPerRowOfPlane(pb, 1);
    if (s->codec == CODEC_VP9) vp9dec_read_nv12(s->vp9, j->surf, y, ys, uv, uvs, j->w, j->h);
    else if (s->codec == CODEC_AV1) av1dec_read_nv12(s->av1, j->surf, y, ys, uv, uvs, j->w, j->h);
    else if (s->codec == kCMVideoCodecType_HEVC) hevcdec_read_nv12(s->hevc, j->surf, y, ys, uv, uvs, j->w, j->h, j->cx, j->cy);
    else if (is_mpeg12(s->codec)) mpeg2dec_read_nv12(s->m2, j->surf, y, ys, uv, uvs, j->w, j->h);
    else if (s->codec == kCMVideoCodecType_MPEG4Video) mpeg4dec_read_nv12(s->m4, j->surf, y, ys, uv, uvs, j->w, j->h);
    else h264dec_read_nv12(&s->dec, j->surf, y, ys, uv, uvs, j->w, j->h);
    CVPixelBufferUnlockBaseAddress(pb, 0);
    if (s->colour && !getenv("NVDEC_NOCOLOUR")) CVBufferSetAttachments(pb, s->colour, kCVAttachmentMode_ShouldPropagate);
    j->usCopy = (unsigned)((nowms() - tc) * 1000);
}

static void out_release(DState *s, const OutJob *j)   // lock held
{
    if (j->rel < 0) return;
    if (s->codec == CODEC_VP9) vp9dec_release(s->vp9, j->rel);
    else if (s->codec == CODEC_AV1) av1dec_release(s->av1, j->rel);
    else if (s->codec == kCMVideoCodecType_HEVC) hevcdec_release(s->hevc, j->rel);
    else if (is_mpeg12(s->codec)) mpeg2dec_release(s->m2, j->rel);
    else if (s->codec == kCMVideoCodecType_MPEG4Video) mpeg4dec_release(s->m4, j->rel);
    else h264dec_release(&s->dec, j->rel);
}

static void out_emit(DState *s, OutJob *j)
{
    const double te = nowms();
    VTDecoderSessionEmitDecodedFrame(s->session, j->frame, j->res, j->fl, j->pb);
    if (j->pb) CVPixelBufferRelease(j->pb);
    j->pb = NULL; j->usEmit = (unsigned)((nowms() - te) * 1000);
}

static void *out_thread(void *arg)
{
    DState *s = arg;
    pthread_mutex_lock(&s->lock);
    for (;;) {
        while (s->qh == s->qt && !s->stop) pthread_cond_wait(&s->cv, &s->lock);
        if (s->qh == s->qt) break;
        OutJob j = s->q[s->qh % 4];
        pthread_mutex_unlock(&s->lock);
        const bool copied = j.pb && j.surf >= 0 && !j.res;
        out_copy(s, &j); out_emit(s, &j);
        pthread_mutex_lock(&s->lock);
        out_release(s, &j);
        if (copied) s->tCopy += j.usCopy / 1000.0;
        s->tEmit += j.usEmit / 1000.0;
        s->qh++;
        pthread_cond_broadcast(&s->cv);
    }
    pthread_mutex_unlock(&s->lock);
    return NULL;
}

static void out_drain(DState *s) { while (s->qh != s->qt) pthread_cond_wait(&s->cv, &s->lock); }   // lock held

// hand a frame's output over (lock held): queued in an asynchronous run, else done here
static void out_deliver(DState *s, OutJob *j)
{
    if (s->async) {
        if (!s->thrOn) { s->stop = false; s->thrOn = pthread_create(&s->thr, NULL, out_thread, s) == 0; }
        if (s->thrOn) {
            while (s->qt - s->qh >= 2) pthread_cond_wait(&s->cv, &s->lock);   // at most two pictures waiting
            s->q[s->qt % 4] = *j; s->qt++;
            pthread_cond_broadcast(&s->cv);
            return;
        }
    }
    out_drain(s);
    const bool copied = j->pb && j->surf >= 0 && !j->res;
    out_copy(s, j); out_release(s, j); out_emit(s, j);
    if (copied) s->tCopy += j->usCopy / 1000.0;
    s->tEmit += j->usEmit / 1000.0;
}

// asynchronous output needs a surface beyond what the stream holds as references: not for streams that use (nearly) all
static bool out_async_ok(DState *s)
{
    if (s->codec == kCMVideoCodecType_HEVC) return s->hevc && s->hevc->max_dec_pic <= HEVC_SURFACES - 2;
    if (s->codec == CODEC_VP9 || s->codec == CODEC_AV1 || is_mpeg12(s->codec) || s->codec == kCMVideoCodecType_MPEG4Video) return true;
    int refs = 0;
    for (int i = 0; i < 32; i++) if (s->dec.params.sps[i].max_num_ref_frames > refs) refs = s->dec.params.sps[i].max_num_ref_frames;
    return refs <= DEC_SURFACES - 3;
}

// VP9: one sample is one chunk (in MP4 / WebM a superframe: hidden frames, then the one shown frame); emits the shown
// picture. A sample that shows nothing emits the frame it decoded: ffmpeg splits superframes and feeds hidden frames one
// by one, keeps each returned picture as that frame and shows it itself on show_existing_frame (T10.04; reporting such a
// frame dropped made ffmpeg tear the session down). A resized stream gets buffers of the decoded size.
static OSStatus decode_vp9(DState *s, VTVideoDecoderFrame frame, const uint8_t *p, size_t len)
{
    size_t off[8], fl[8]; const int nf = vp9_superframe(p, len, off, fl);
    int shown = -1; OSStatus res = noErr;
    for (int i = 0; i < nf && !res; i++) {
        const int r = vp9dec_frame(s->vp9, p + off[i], fl[i]);
        if (r < 0) { res = kVTVideoDecoderBadDataErr; break; }
        if (!s->vp9->f.show_existing_frame) s->tEngine += s->vp9->engine_ms;
        if (r == 1) { if (shown >= 0) vp9dec_release(s->vp9, shown); shown = s->vp9->cur_surf; }
    }
    if (!res && shown < 0 && s->vp9->dec_surf >= 0 && nf > 0) { shown = s->vp9->dec_surf; s->vp9->busy[shown] = true; }
    OutJob j = { .frame = frame, .surf = -1, .rel = -1 };
    if (!res && shown >= 0) {
        j.w = s->vp9->w[shown]; j.h = s->vp9->h[shown]; j.rel = shown;
        const double tp = nowms();
        if (output_buffer(s, j.w, j.h, &j.pb) || !j.pb) res = kVTVideoDecoderMalfunctionErr; else j.surf = shown;
        s->tPool += nowms() - tp;
        s->frames++;
    }
    if (gDLog && (s->frames <= 3 || res)) DLOG("decodeFrame %p: %zu bytes, %d frames -> %d, shown %d, status 0x%x, %.2f ms", frame, len, nf,
                                              (int)res, shown, s->vp9->err_status, s->vp9->engine_ms);
    j.res = res; j.fl = (!res && shown < 0) ? kVTDecodeInfo_FrameDropped : 0;
    out_deliver(s, &j);
    return res;
}

// AV1: one sample is one temporal unit (OBUs; in MP4 one shown frame, hidden frames before it). As with VP9, a sample
// that shows nothing emits the frame it decoded (ffmpeg feeds frames one by one).
static OSStatus decode_av1(DState *s, VTVideoDecoderFrame frame, const uint8_t *p, size_t len)
{
    OSStatus res = noErr; int shown = -1;
    const int r = av1dec_tu(s->av1, p, len);
    if (r < 0) res = kVTVideoDecoderBadDataErr;
    else {
        s->tEngine += s->av1->engine_ms;
        if (r == 1) shown = s->av1->cur_surf;
        else if (s->av1->dec_surf >= 0) {   // a hidden frame: what its show_existing_frame will show (with its film grain)
            shown = s->av1->twin[s->av1->dec_surf] >= 0 ? s->av1->twin[s->av1->dec_surf] : s->av1->dec_surf;
            s->av1->busy[shown] = true;
        }
    }
    // a frame left out for references never decoded (src/av1dec.c): reported as such, which ffmpeg counts as a decode
    // error; an empty picture with noErr would make it restart the session (videotoolbox_decoder_callback)
    if (!res && shown < 0 && s->av1->skipped) res = kVTVideoDecoderReferenceMissingErr;
    OutJob j = { .frame = frame, .surf = -1, .rel = -1 };
    if (!res && shown >= 0) {
        j.w = s->av1->w[shown]; j.h = s->av1->h[shown]; j.rel = shown;
        const double tp = nowms();
        if (output_buffer(s, j.w, j.h, &j.pb) || !j.pb) res = kVTVideoDecoderMalfunctionErr; else j.surf = shown;
        s->tPool += nowms() - tp;
        s->frames++;
    }
    if (gDLog && (s->frames <= 3 || res)) DLOG("decodeFrame %p: %zu bytes -> %d, shown %d, status 0x%x, %.2f ms", frame, len, (int)res, shown,
                                              s->av1->err_status, s->av1->engine_ms);
    j.res = res; j.fl = (!res && shown < 0) ? kVTDecodeInfo_FrameDropped : 0;
    out_deliver(s, &j);
    return res;
}

// HEVC: one sample is one access unit (one picture), emitted in decode order like H.264's. A picture not to be output
// (pic_output_flag 0, or a RASL picture after the CRA decoding started at, which is skipped) is reported dropped.
static OSStatus decode_hevc(DState *s, VTVideoDecoderFrame frame, const uint8_t *p, size_t len)
{
    HevcDec *d = s->hevc; int r = 0; OSStatus res = noErr;
    size_t pos = 0; const uint8_t *nal; size_t n;
    while (r >= 0 && hevc_next_lenpfx(p, len, s->nalLen, &pos, &nal, &n)) { int q = hevcdec_nal(d, nal, n); if (q) r = q; }
    if (r >= 0) { int q = hevcdec_flush(d); if (q) r = q; }
    if (r < 0) res = kVTVideoDecoderBadDataErr;
    OutJob j = { .frame = frame, .surf = -1, .rel = -1 }; bool out = false;
    if (!res && r == 1) {
        s->tEngine += d->engine_ms;
        out = d->cur_output; j.rel = d->cur_surf;
        if (out) {
            j.w = (unsigned)d->cur_w; j.h = (unsigned)d->cur_h; j.cx = (unsigned)d->cur_crop[0]; j.cy = (unsigned)d->cur_crop[2];
            const double tp = nowms();
            if (output_buffer(s, j.w, j.h, &j.pb) || !j.pb) res = kVTVideoDecoderMalfunctionErr; else j.surf = d->cur_surf;
            s->tPool += nowms() - tp;
        }
        s->frames++;
    }
    if (gDLog && (s->frames <= 3 || res)) DLOG("decodeFrame %p: %zu bytes -> %d, picture %d (POC %d%s), status 0x%x, %.2f ms", frame, len, (int)res, r,
                                              d->cur_poc, out ? "" : ", not output", d->err_status, d->engine_ms);
    j.res = res; j.fl = (!res && !out) ? kVTDecodeInfo_FrameDropped : 0;
    out_deliver(s, &j);
    return res;
}

// MPEG-1/2: one sample is one coded frame (a frame picture or both fields), emitted in decode order like H.264's; a B
// picture without both anchors (broken link at the start) is reported dropped
static OSStatus decode_mpeg2(DState *s, VTVideoDecoderFrame frame, const uint8_t *p, size_t len)
{
    Mpeg2Dec *d = s->m2; int r = 0; OSStatus res = noErr;
    size_t pos = 0; int code; const uint8_t *data; size_t n; int units = 0, first = -1;
    while (r >= 0 && mpeg2_next_unit(p, len, &pos, &code, &data, &n)) {
        if (first < 0) first = code;
        units++;
        if (code == 0x00 && !d->haveSeq) { d->haveSeq = true; DLOG("no sequence header before the first picture: default sequence %dx%d", d->seq.width, d->seq.height); }
        const int q = mpeg2dec_unit(d, code, data, n); if (q) r = q;
    }
    if (r >= 0) { const int q = mpeg2dec_flush(d); if (q) r = q; }
    if (r < 0) res = kVTVideoDecoderBadDataErr;
    OutJob j = { .frame = frame, .surf = -1, .rel = -1 }; const bool out = !res && r == 1;
    if (out) {
        s->tEngine += d->engine_ms; const double tp = nowms();
        j.w = (unsigned)d->cur_w; j.h = (unsigned)d->cur_h; j.rel = d->cur_surf;
        if (output_buffer(s, j.w, j.h, &j.pb) || !j.pb) res = kVTVideoDecoderMalfunctionErr; else j.surf = d->cur_surf;
        s->tPool += nowms() - tp;
        s->frames++;
    }
    if (gDLog && (s->frames <= 3 || res)) DLOG("decodeFrame %p: %zu bytes, %d units (first 0x%02x) -> %d, picture %d (%c), status 0x%x, %.2f ms", frame, len, units,
                                              first & 0xff, (int)res, r, " IPBD"[d->cur_type & 7], d->err_status, d->engine_ms);
    j.res = res; j.fl = (!res && !out) ? kVTDecodeInfo_FrameDropped : 0;
    out_deliver(s, &j);
    return res;
}

// MPEG-4: one sample is one VOP (with a VOL before the first, maybe), emitted in decode order; a not-coded VOP (and a
// B-VOP without both anchors) is reported dropped
static OSStatus decode_mpeg4(DState *s, VTVideoDecoderFrame frame, const uint8_t *p, size_t len)
{
    Mpeg4Dec *d = s->m4; int r = 0, pics = 0; OSStatus res = noErr;
    size_t pos = 0; const uint8_t *st; size_t n;
    while (r >= 0 && mpeg4_next_unit(p, len, &pos, &st, &n)) { const int q = mpeg4dec_unit(d, st, n); if (q < 0) r = q; else if (q == 1) { r = 1; pics++; } }
    if (r < 0) res = kVTVideoDecoderBadDataErr;
    OutJob j = { .frame = frame, .surf = -1, .rel = -1 }; const bool out = !res && r == 1 && d->cur_shown;
    if (!res && r == 1) j.rel = d->cur_surf;
    if (out) {
        s->tEngine += d->engine_ms; const double tp = nowms();
        j.w = (unsigned)d->cur_w; j.h = (unsigned)d->cur_h;
        if (output_buffer(s, j.w, j.h, &j.pb) || !j.pb) res = kVTVideoDecoderMalfunctionErr; else j.surf = d->cur_surf;
        s->tPool += nowms() - tp;
        s->frames++;
    }
    if (gDLog && (s->frames <= 3 || res || pics > 1)) DLOG("decodeFrame %p: %zu bytes -> %d, %d VOP(s) (%c%s), status 0x%x, %.2f ms", frame, len, (int)res, pics,
                                              "IPBSN"[d->cur_type % 5], out ? "" : ", not shown", d->err_status, d->engine_ms);
    j.res = res; j.fl = (!res && !out) ? kVTDecodeInfo_FrameDropped : 0;
    out_deliver(s, &j);
    return res;
}

static OSStatus d_decodeFrame(VTVideoDecoderRef o, VTVideoDecoderFrame frame, CMSampleBufferRef sb, VTDecodeFrameFlags flags, VTDecodeInfoFlags *info)
{
    DState *s = dstate(o);
    if (info) *info = 0;
    const double tA = nowms();
    pthread_mutex_lock(&s->lock);
    // asynchronous output when the client allows it (NVDEC_SYNC=1: never)
    s->async = (flags & kVTDecodeFrame_EnableAsynchronousDecompression) && s->opened && !getenv("NVDEC_SYNC") && out_async_ok(s);
    if (s->async && info) *info |= kVTDecodeInfo_Asynchronous;
    if (s->tLast > 0) s->tOut += tA - s->tLast;
    OSStatus res = noErr; CVPixelBufferRef pb = NULL;
    CMBlockBufferRef bb = CMSampleBufferGetDataBuffer(sb);
    size_t len = bb ? CMBlockBufferGetDataLength(bb) : 0, avail = 0; char *p = NULL;
    if (!len || !s->opened) res = kVTVideoDecoderBadDataErr;
    else if (CMBlockBufferGetDataPointer(bb, 0, &avail, NULL, &p) || avail < len) {
        if (s->tmpCap < len) { free(s->tmp); s->tmp = malloc(len); s->tmpCap = s->tmp ? len : 0; }
        if (!s->tmp || CMBlockBufferCopyDataBytes(bb, 0, len, s->tmp)) res = kVTVideoDecoderBadDataErr; else p = (char *)s->tmp;
    }
    if (s->codec == CODEC_VP9 || s->codec == CODEC_AV1 || s->codec == kCMVideoCodecType_HEVC || is_mpeg12(s->codec) || s->codec == kCMVideoCodecType_MPEG4Video) {
        res = res ? res : s->codec == CODEC_AV1 ? decode_av1(s, frame, (const uint8_t *)p, len) :
                          is_mpeg12(s->codec) ? decode_mpeg2(s, frame, (const uint8_t *)p, len) :
                          s->codec == kCMVideoCodecType_MPEG4Video ? decode_mpeg4(s, frame, (const uint8_t *)p, len) :
                          s->codec == kCMVideoCodecType_HEVC ? decode_hevc(s, frame, (const uint8_t *)p, len) : decode_vp9(s, frame, (const uint8_t *)p, len);
        if (res) s->errors++;
        s->tLast = nowms(); s->tIn += s->tLast - tA;
        pthread_mutex_unlock(&s->lock);
        return res;
    }
    int r = 0;
    if (!res) {
        size_t pos = 0; const uint8_t *nal; size_t n;
        while (r >= 0 && h264_next_lenpfx((const uint8_t *)p, len, s->nalLen, &pos, &nal, &n)) { int q = h264dec_nal(&s->dec, nal, n); if (q) r = q; }
        if (r >= 0) { int q = h264dec_flush(&s->dec); if (q) r = q; }   // one sample is one access unit
        if (r < 0) res = kVTVideoDecoderBadDataErr;
        // completed frames: normally one; a non-paired field before it adds another, of which only the newest is emitted
        int got = 0;
        while (h264dec_pop(&s->dec)) { if (got) h264dec_release(&s->dec, got - 1); got = s->dec.cur_surf + 1; }
        r = r < 0 ? r : got ? 1 : 0;
    }
    if (!res && r == 1) {
        s->tEngine += s->dec.engine_ms; double tp = nowms();
        CVPixelBufferPoolRef pool = VTDecoderSessionGetPixelBufferPool(s->session); OSStatus cst = pool ? CVPixelBufferPoolCreatePixelBuffer(NULL, pool, &pb) : -1;
        if (!cst && pb && gDLog && s->frames == 0) {   // does the client consider our buffer compatible? (else VT transfers every frame)
            typedef Boolean (*CompatFn)(CVPixelBufferRef, CFDictionaryRef);
            CompatFn compat = (CompatFn)dlsym(RTLD_DEFAULT, "CVPixelBufferIsCompatibleWithAttributes");
            CFDictionaryRef dst = VTDecoderSessionGetDestinationPixelBufferAttributes(s->session);
            if (compat && dst) {
                DLOG("our buffer compatible with the destination attributes: %d", (int)compat(pb, dst));
                CFDictionaryRef pa = pool ? CVPixelBufferPoolGetPixelBufferAttributes(pool) : NULL;
                CFStringRef ds = pa ? CFCopyDescription(pa) : NULL; char buf[1024] = "(none)";
                if (ds) { CFStringGetCString(ds, buf, sizeof buf, kCFStringEncodingUTF8); CFRelease(ds); }
                for (char *c = buf; *c; c++) if (*c == '\n') *c = ' ';
                DLOG("  pool attributes: %s", buf);
                CFDictionaryRef ios = CVBufferCopyAttachment(pb, kCVPixelBufferIOSurfacePropertiesKey, NULL);
                DLOG("  buffer: IOSurface %p, bytes per row %zu/%zu", CVPixelBufferGetIOSurface(pb), CVPixelBufferGetBytesPerRowOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 1));
                if (ios) CFRelease(ios);
                CFIndex n = CFDictionaryGetCount(dst); const void **ks = malloc(sizeof(void *) * (size_t)n), **vs = malloc(sizeof(void *) * (size_t)n);
                CFDictionaryGetKeysAndValues(dst, ks, vs);
                for (CFIndex i = 0; i < n; i++) {
                    CFDictionaryRef one = CFDictionaryCreate(NULL, &ks[i], &vs[i], 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
                    char kn[128]; CFStringGetCString(ks[i], kn, sizeof kn, kCFStringEncodingUTF8);
                    DLOG("  compatible with {%s} alone: %d", kn, (int)compat(pb, one)); CFRelease(one);
                }
                free(ks); free(vs);
            } else DLOG("CVPixelBufferIsCompatibleWithAttributes %s", compat ? "found, no destination attributes" : "not found");
        }
        OutJob j = { .frame = frame, .surf = -1, .rel = s->dec.cur_surf, .pb = pb };
        if (cst || !pb) res = kVTVideoDecoderMalfunctionErr;
        else { s->tPool += nowms() - tp; j.surf = s->dec.cur_surf; j.w = (unsigned)CVPixelBufferGetWidth(pb); j.h = (unsigned)CVPixelBufferGetHeight(pb); }
        pb = NULL; s->frames++;
        j.res = res; out_deliver(s, &j);
    } else {
        OutJob j = { .frame = frame, .res = res, .fl = !res ? kVTDecodeInfo_FrameDropped : 0, .surf = -1, .rel = -1 };
        out_deliver(s, &j);
    }
    if (res) s->errors++;
    if (gDLog && (s->frames <= 3 || res)) DLOG("decodeFrame %p: %zu bytes -> %d, picture %d, %u MBs ok, %u err, %.2f ms", frame, len, (int)res, r,
                                              s->dec.mbs_ok, s->dec.mbs_err, s->dec.engine_ms);
    s->tLast = nowms(); s->tIn += s->tLast - tA;
    pthread_mutex_unlock(&s->lock);
    return res;
}

static OSStatus d_copySupported(VTVideoDecoderRef o, CFDictionaryRef *out)
{
    (void)o;
    // VideoToolbox forwards a property query to the decoder only for keys listed here (as with the encoder)
    const void *pk[] = { kVTPropertyTypeKey, kVTPropertyReadWriteStatusKey };
    const void *pv[] = { kVTPropertyType_Boolean, kVTPropertyReadWriteStatus_ReadOnly };
    CFDictionaryRef ro = CFDictionaryCreate(NULL, pk, pv, 2, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    const void *k[] = { kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder };
    const void *v[] = { ro };
    *out = CFDictionaryCreate(NULL, k, v, 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFRelease(ro);
    return noErr;
}
static OSStatus d_setProperties(VTVideoDecoderRef o, CFDictionaryRef d) { (void)o; DLOG("setProperties %p", d); return noErr; }
static OSStatus d_copySerializable(VTVideoDecoderRef o, CFAllocatorRef al, CFDictionaryRef *out)
{
    (void)o; DLOG("copySerializableProperties allocator %p out %p", al, out);
    if (out) *out = CFDictionaryCreate(NULL, NULL, NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    return noErr;
}
static Boolean d_canAccept(VTVideoDecoderRef o, CMFormatDescriptionRef f) { (void)o; DLOG("canAcceptFormatDescription %p: no", f); return false; }
static OSStatus d_finishDelayed(VTVideoDecoderRef o)
{
    DState *s = dstate(o); DLOG("finishDelayedFrames");
    pthread_mutex_lock(&s->lock); if (!s->thrOn || !pthread_equal(pthread_self(), s->thr)) out_drain(s); pthread_mutex_unlock(&s->lock);
    return noErr;
}

static CMBaseClass gDBase = { .version = 1, .derivedStorageSize = sizeof(void *), .invalidate = b_invalidate, .finalize = b_finalize,
                              .copyDebugDescription = b_desc, .copyProperty = b_copyProperty, .setProperty = b_setProperty };
static const DecoderClass gDec = { .version = 1, .startSession = d_startSession, .decodeFrame = d_decodeFrame,
                                   .copySupportedPropertyDictionary = d_copySupported, .setProperties = d_setProperties,
                                   .copySerializableProperties = d_copySerializable, .canAcceptFormatDescription = d_canAccept,
                                   .finishDelayedFrames = d_finishDelayed };
static const DecoderVTable gDVT = { .base = { NULL, &gDBase }, .decoderClass = &gDec };

__attribute__((visibility("default"))) OSStatus NVDecVT_CreateInstance(FourCharCode codec, CFAllocatorRef al, VTVideoDecoderRef *out)
{
    const FourCharCode entry = codec;   // 'hev1' / 'avc3' (parameter sets may also come in band) decode as HEVC / H.264
    if (codec == CODEC_HEV1) codec = kCMVideoCodecType_HEVC; else if (codec == CODEC_AVC3) codec = kCMVideoCodecType_H264;
    if (codec != kCMVideoCodecType_H264 && codec != kCMVideoCodecType_HEVC && codec != CODEC_VP9 && codec != CODEC_AV1 && !is_mpeg12(codec) && codec != kCMVideoCodecType_MPEG4Video) return kVTCouldNotFindVideoDecoderErr;
    CMBaseObjectRef obj = NULL;
    OSStatus st = CMDerivedObjectCreate(al, &gDVT.base, VTVideoDecoderGetClassID(), &obj);
    if (st) return st;
    DState *s = calloc(1, sizeof *s);
    pthread_mutex_init(&s->lock, NULL); pthread_cond_init(&s->cv, NULL);
    s->codec = codec;
    *(DState **)CMBaseObjectGetDerivedStorage(obj) = s;
    *out = obj;
    DLOG("instance %p (%s)", obj, entry == CODEC_HEV1 ? "hev1" : entry == CODEC_AVC3 ? "avc3" : codec == CODEC_VP9 ? "vp09" : codec == CODEC_AV1 ? "av01" : codec == kCMVideoCodecType_HEVC ? "hvc1" :
         codec == kCMVideoCodecType_MPEG2Video ? "mp2v" : codec == kCMVideoCodecType_MPEG1Video ? "mp1v" :
         codec == kCMVideoCodecType_MPEG4Video ? "mp4v" : "avc1");
    return noErr;
}

// The NVDEC class of this GPU (0 = none): the decoders ask RM for it once per process, when they register. GA10x (0xc7b0,
// RTX 30) and Ada (0xc9b0) have AV1; Turing (0xc4b0) and GA100 (0xc6b0) have none.
static int gNvdecClass = -1;
static NvU32 nvdec_class(void)
{
    if (gNvdecClass < 0) {
        Rm rm = { 0 };
        if (rm_open(&rm)) gNvdecClass = 0;
        else { gNvdecClass = (int)rm_class(&rm, RM_CLS_NVDEC); rm_close(&rm); }
    }
    return (NvU32)gNvdecClass;
}

// Called by NVEncVT_Register once the GPU checked out (Ada, GA10x or TU117). Rated above NullMoth's own H.264 / HEVC decoders (1000). HEVC,
// VP9 and AV1 too (NVENCVT_NO_HEVCDEC / _VP9DEC / _AV1DEC=1 leave one out).
// On GA10x (RTX 30) the VP9 and AV1 decoders register: their method IDs are the same as Ada's (clc7b0.h and clc9b0.h
// agree on every one of them) and GA10x's NVDEC has both. The H.264 / HEVC / MPEG decoders were written for Ada's NVDEC
// (0xc9b0) and Turing's (0xc4b0) and were not tried on 0xc7b0, and a decoder that registers and then fails is worse than
// none (VideoToolbox does not fall back to another decoder once it picked ours). NVENCVT_AMPERE_ALL_DEC=1 registers
// them all anyway.
OSStatus NVDecVT_Register(int rating)
{
    gDLog = getenv("NVENCVT_LOG") != NULL;
    if (!gDLog && access("/tmp/nvencvt.log", F_OK) == 0) { char p[64]; snprintf(p, sizeof p, "/tmp/nvencvt-%d.log", (int)getpid()); if ((gDLogF = fopen(p, "a"))) gDLog = true; }
    if (getenv("NVENCVT_NO_DEC")) return kVTCouldNotFindVideoDecoderErr;
    extern int gNvencvtGpu;   // vt/nvencvt.c: 1 Ada, 2 TU117 (GTX 1650: NVDEC 0xc4b0 has no AV1), 3 GA10x (RTX 30)
    const bool ampere = gNvencvtGpu == 3;
    const bool av1Vp9Only = ampere && !getenv("NVENCVT_AMPERE_ALL_DEC");
    typedef OSStatus (*RegFn)(FourCharCode, CFDictionaryRef, void *);
    RegFn reg = (RegFn)dlsym(RTLD_DEFAULT, "VTRegisterVideoDecoderWithInfo");
    if (!reg) { DLOG("VTRegisterVideoDecoderWithInfo not found"); return kVTCouldNotFindVideoDecoderErr; }
    CFNumberRef nr = CFNumberCreate(NULL, kCFNumberIntType, &rating);
    const void *k[] = { CFSTR("CMClassImplementationID"), CFSTR("CMClassImplementationName"), CFSTR("VTRating"), CFSTR("VTIsHardwareAccelerated") };
    const void *v[] = { CFSTR(DECODER_ID), CFSTR("NVDEC - H.264 - mickys/nvenc"), nr, kCFBooleanTrue };
    CFDictionaryRef info = CFDictionaryCreate(NULL, k, v, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    OSStatus st = av1Vp9Only ? kVTCouldNotFindVideoDecoderErr : reg(kCMVideoCodecType_H264, info, (void *)NVDecVT_CreateInstance);
    // 'avc3' / 'hev1' sample entries (ffmpeg writes hev1 for HEVC in MP4 by default): VideoToolbox has no decoder for them
    // at all (-12906, Apple's included); ours decodes them like avc1 / hvc1 (NVENCVT_NO_HEV1=1 leaves both out)
    if (!av1Vp9Only && !getenv("NVENCVT_NO_HEV1")) { OSStatus s3 = reg(CODEC_AVC3, info, (void *)NVDecVT_CreateInstance); DLOG("registered %s for avc3 -> %d", DECODER_ID, (int)s3); }
    CFRelease(info);
    DLOG("registered %s -> %d (rating %d)", DECODER_ID, (int)st, rating);
    if (!av1Vp9Only && !getenv("NVENCVT_NO_HEVCDEC")) {   // HEVC (NVENCVT_NO_HEVCDEC=1 leaves it out): above NullMoth's own HEVC decoder too
        const void *vh[] = { CFSTR(DECODER_ID_HEVC), CFSTR("NVDEC - HEVC - mickys/nvenc"), nr, kCFBooleanTrue };
        CFDictionaryRef ih = CFDictionaryCreate(NULL, k, vh, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        OSStatus sh = reg(kCMVideoCodecType_HEVC, ih, (void *)NVDecVT_CreateInstance);
        if (!getenv("NVENCVT_NO_HEV1")) { OSStatus s1 = reg(CODEC_HEV1, ih, (void *)NVDecVT_CreateInstance); DLOG("registered %s for hev1 -> %d", DECODER_ID_HEVC, (int)s1); }
        CFRelease(ih);
        DLOG("registered %s -> %d (rating %d)", DECODER_ID_HEVC, (int)sh, rating);
    }
    if (!getenv("NVENCVT_NO_VP9DEC")) {
        const void *v9[] = { CFSTR(DECODER_ID_VP9), CFSTR("NVDEC - VP9 - mickys/nvenc"), nr, kCFBooleanTrue };
        CFDictionaryRef i9 = CFDictionaryCreate(NULL, k, v9, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        OSStatus s9 = reg(CODEC_VP9, i9, (void *)NVDecVT_CreateInstance);
        CFRelease(i9);
        DLOG("registered %s -> %d (rating %d)", DECODER_ID_VP9, (int)s9, rating);
        // VideoToolbox offers VP9 decoders only to a process that opted in (public API, macOS 11+; WebKit and ffmpeg call
        // it themselves). Opting in for the process lets AVFoundation apps decode VP9 too; on Macs without Apple's own
        // VP9 decoder this decoder is then the only one (T10.04). NVENCVT_NO_VP9SUPP=1 leaves it to the app.
        typedef void (*SuppFn)(CMVideoCodecType);
        SuppFn supp = (SuppFn)dlsym(RTLD_DEFAULT, "VTRegisterSupplementalVideoDecoderIfAvailable");
        if (supp && !getenv("NVENCVT_NO_VP9SUPP")) { supp(CODEC_VP9); DLOG("opted in to VP9 decoding for this process"); }
    }
    // AV1: the NVDEC classes that have it (GA10x 0xc7b0 = RTX 30, Ada 0xc9b0). The class is the truth, not the card's name:
    // a decoder that registers on a card without AV1 would take the session away from Apple's own decoder and then fail.
    // Ada's and Turing's class is known from the card (the PCI device ID), so RM is asked only for GA10x and for cards the
    // registry does not name - and RM answering 0 (no driver) means no AV1 decoder at all.
    const NvU32 nvcls = gNvencvtGpu == 1 ? 0xc9b0 : gNvencvtGpu == 2 ? 0xc4b0 : nvdec_class();
    const bool hasAv1 = nvcls == 0xc7b0 || nvcls == 0xc9b0;
    DLOG("NVDEC class 0x%x%s", nvcls, hasAv1 ? " (AV1)" : " (no AV1)");
    if (!getenv("NVENCVT_NO_AV1DEC") && hasAv1) {   // AV1 likewise (NVENCVT_NO_AV1DEC=1 leaves it out)
        const void *va[] = { CFSTR(DECODER_ID_AV1), CFSTR("NVDEC - AV1 - mickys/nvenc"), nr, kCFBooleanTrue };
        CFDictionaryRef ia = CFDictionaryCreate(NULL, k, va, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        OSStatus sa = reg(CODEC_AV1, ia, (void *)NVDecVT_CreateInstance);
        CFRelease(ia);
        DLOG("registered %s -> %d (rating %d)", DECODER_ID_AV1, (int)sa, rating);
        typedef void (*SuppFn)(CMVideoCodecType);
        SuppFn supp = (SuppFn)dlsym(RTLD_DEFAULT, "VTRegisterSupplementalVideoDecoderIfAvailable");
        if (supp && !getenv("NVENCVT_NO_VP9SUPP")) { supp(CODEC_AV1); DLOG("opted in to AV1 decoding for this process"); }
    }
    if (!av1Vp9Only && !getenv("NVENCVT_NO_MPEG2DEC")) {   // MPEG-1 / MPEG-2 and MPEG-4 Part 2 (M14): above Apple's software decoders
        const struct { FourCharCode c; const char *id, *name; } m[] = {
            { kCMVideoCodecType_MPEG2Video, DECODER_ID_MPEG2, "NVDEC - MPEG-2 - mickys/nvenc" },
            { kCMVideoCodecType_MPEG1Video, DECODER_ID_MPEG1, "NVDEC - MPEG-1 - mickys/nvenc" },
            { kCMVideoCodecType_MPEG4Video, DECODER_ID_MPEG4, "NVDEC - MPEG-4 Part 2 - mickys/nvenc" } };
        for (int i = 0; i < 3; i++) {
            if (m[i].c == kCMVideoCodecType_MPEG4Video && getenv("NVENCVT_NO_MPEG4DEC")) continue;
            CFStringRef cid = CFStringCreateWithCString(NULL, m[i].id, kCFStringEncodingUTF8), cn = CFStringCreateWithCString(NULL, m[i].name, kCFStringEncodingUTF8);
            const void *vm[] = { cid, cn, nr, kCFBooleanTrue };
            CFDictionaryRef im = CFDictionaryCreate(NULL, k, vm, 4, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            OSStatus sm = reg(m[i].c, im, (void *)NVDecVT_CreateInstance);
            CFRelease(im); CFRelease(cid); CFRelease(cn);
            DLOG("registered %s -> %d (rating %d)", m[i].id, (int)sm, rating);
        }
    }
    CFRelease(nr);
    return st;
}
