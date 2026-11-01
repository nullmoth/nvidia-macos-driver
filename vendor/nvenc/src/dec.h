// dec.h (M8): an H.264 decode session on NVDEC. Buffers are allocated once for the largest picture; each picture (a
// frame, or one field of a frame) is parsed, set up (nvdec_h264_pic_s), decoded into one of 17 surfaces (both fields of
// a frame into the same one) and the decoded reference picture marking (H.264 8.2.5, per field) is applied afterwards.
// Completed frames go into a small queue (h264dec_pop). Headers only: open-gpu-doc's nvdec_drv.h / clc9b0.h and the H.264
// standard; what the headers do not say is in docs/tasks.md (T8.03, T8.04).
#pragma once
#include "rm.h"
#include "channel.h"
#include "h264parse.h"

enum { DEC_SURFACES = 17 };

typedef struct {
    int surf;                       // surface index (also the co-located MV slot)
    int frame_num, long_term_frame_idx;
    int top_poc, bot_poc;
    int ref;                        // summary: 0 no field is a reference, 1 short-term, 2 a field is long-term
    int mark[2];                    // per field (top, bottom): 0 unused, 1 short-term, 2 long-term reference
    bool fields;                    // decoded as field pictures (a complementary field pair or a non-paired field)
    bool nonexist;                  // a "non-existing" frame for a frame_num gap (8.2.5.2)
} DecRef;

typedef struct { int surf, top, bot, w, h; } DecOut;

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;            // allocation size (pixels, multiple of 16)
    size_t pitch;
    RmMem setup, bs, offs, status, coloc, hist, mbhist, sem;
    RmMem luma[DEC_SURFACES], chroma[DEC_SURFACES];
    bool busy[DEC_SURFACES];        // held by the caller until h264dec_release (decoded, not yet output)
    int x0[DEC_SURFACES], y0[DEC_SURFACES];   // each surface's crop origin (applied by the read functions)
    DecRef dpb[16]; int ndpb;
    H264Params params; H264PocState poc;
    int max_long_term_frame_idx;    // -1 = no long-term frame indices
    int prev_ref_frame_num;         // PrevRefFrameNum (7.4.3), -1 before the first picture
    int last_ref_surf, last_top, last_bot;   // the newest real reference (non-existing frames point at it)
    struct { bool valid; int surf, par, frame_num, ref, top, bot, w, h; } fld;   // a first field waiting for its second
    DecOut out[4]; int nout;        // completed frames, oldest first
    NvU32 semval;
    // the picture just decoded
    int cur_surf, cur_top, cur_bot, cur_w, cur_h;
    unsigned mbs_ok, mbs_err, err_status;
    double engine_ms;
    bool verbose;
    // the access unit being collected (slices with start codes, as NVDEC reads them)
    uint8_t *au; size_t auCap, auN; NvU32 auOff[1024]; int auSlices; H264Slice auFirst, auLast;
} H264Dec;

int  h264dec_open(H264Dec *d, Rm *rm, unsigned maxW, unsigned maxH);
void h264dec_close(H264Dec *d);
// Feed one NAL unit (without start code). SPS/PPS are stored; slices are collected until the next picture starts.
// Returns the number of frames completed by it (a frame, both fields of one, or a non-paired field; take them with
// h264dec_pop), or -1 on error. h264dec_flush decodes the last collected picture and completes a waiting field.
int  h264dec_nal(H264Dec *d, const uint8_t *nal, size_t len);
int  h264dec_flush(H264Dec *d);
// The oldest completed frame into cur_surf / cur_top / cur_bot / cur_w / cur_h; false when there is none.
bool h264dec_pop(H264Dec *d);
// The decoded picture's surface stays out of reuse until released (it may also stay a reference afterwards).
void h264dec_release(H264Dec *d, int surf);
// Copy a decoded surface out as NV12 (luma and interleaved chroma planes with their own strides; w x h, cropped size).
void h264dec_read_nv12(H264Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
// Copy a decoded surface out as I420 (w x h, the cropped size).
void h264dec_read_i420(H264Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
