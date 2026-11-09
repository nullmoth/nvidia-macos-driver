// mpeg2dec.h (M14, T14.01): an MPEG-2 / MPEG-1 video decode session on NVDEC. The stream is fed one start-code unit at a
// time; the sequence / extension / picture headers are parsed here (ISO/IEC 13818-2 6.2, 11172-2 2.4.2), the slices go
// to the engine with their start codes and NVDEC parses them itself (nvdec_mpeg2_pic_s). Headers only: open-gpu-doc's
// nvdec_drv.h / clc9b0.h and the standard; what they do not say is in docs/tasks.md (M14).
#pragma once
#include "rm.h"
#include "channel.h"

enum { MPEG2_SURFACES = 8 };

typedef struct {
    bool mpeg1;                     // no sequence extension seen (ISO/IEC 11172-2)
    int width, height;              // horizontal / vertical_size (with the extensions)
    int progressive_sequence, chroma_format;
    int frame_rate_code;
    uint8_t intra[64], nonintra[64];   // quantiser matrices in raster order (chroma ones: 4:2:2 / 4:4:4 only, not kept)
} Mpeg2Seq;

typedef struct {
    int temporal_reference, coding_type;   // 1 I, 2 P, 3 B, 4 D
    int f_code[2][2];                      // [forward/backward][horizontal/vertical]
    int full_pel[2];                       // MPEG-1
    int intra_dc_precision, picture_structure, top_field_first, frame_pred_frame_dct, concealment_motion_vectors;
    int q_scale_type, intra_vlc_format, alternate_scan, repeat_first_field, progressive_frame;
} Mpeg2Pic;

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;            // allocation size (pixels, multiple of 32)
    size_t pitch;
    RmMem setup, bs, offs, status, hist, sem;
    RmMem luma[MPEG2_SURFACES], chroma[MPEG2_SURFACES];
    bool busy[MPEG2_SURFACES];      // held by the caller until mpeg2dec_release
    int fwd, bwd;                   // the two most recent anchor (I / P) frames' surfaces: older, newer (-1 none)
    bool haveSeq;
    Mpeg2Seq seq; Mpeg2Pic pic;
    NvU32 semval;
    // the picture being collected
    bool inPic; uint8_t *au; size_t auCap, auN; NvU32 auOff[4096]; int auSlices;
    int fieldSurf;                  // second field of a field-picture pair goes here (-1: the next picture starts a frame)
    // the picture (frame, or a frame's second field) just completed
    int cur_surf, cur_type, cur_tref, cur_w, cur_h;
    bool cur_frame_done;            // a whole frame is complete (frame picture or second field)
    int skipped;                    // frames left out so far: B pictures without both anchors (open GOP at the start)
    bool skipping, skipField2;      // the picture being skipped; the next skipped field completes a field pair
    unsigned mbs_ok, mbs_err, err_status;
    double engine_ms;
    bool verbose;
} Mpeg2Dec;

int  mpeg2dec_open(Mpeg2Dec *d, Rm *rm, unsigned maxW, unsigned maxH);
void mpeg2dec_close(Mpeg2Dec *d);
// Feed one start-code unit: code = the byte after 00 00 01, data/len = the bytes after it up to the next start code.
// Returns 1 when a picture was decoded (finished by this unit), 0 if not, -1 on error. mpeg2dec_flush decodes the last one.
int  mpeg2dec_unit(Mpeg2Dec *d, int code, const uint8_t *data, size_t len);
int  mpeg2dec_flush(Mpeg2Dec *d);
// Until a sequence header arrives: this size, default matrices, 4:2:0, progressive (a client that sends none: VideoToolbox)
void mpeg2dec_default_sequence(Mpeg2Dec *d, int width, int height, bool mpeg1);
void mpeg2dec_release(Mpeg2Dec *d, int surf);
// Copy a decoded surface out as I420 (w x h).
void mpeg2dec_read_i420(Mpeg2Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
void mpeg2dec_read_nv12(Mpeg2Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
// Split an elementary stream at start codes: returns false at the end. *code = the start code value, *data / *len = its payload.
bool mpeg2_next_unit(const uint8_t *buf, size_t n, size_t *pos, int *code, const uint8_t **data, size_t *len);
