// mpeg4dec.h (M14, T14.02): an MPEG-4 Part 2 (Simple / Advanced Simple) and H.263 (short video header) decode session on
// NVDEC. The VOL header is parsed here (ISO/IEC 14496-2 6.2.3), the VOP header only as far as the setup needs it (type,
// time stamps for B-VOP distances, rounding, f_codes, scan); each VOP goes to the engine from its start code and NVDEC
// parses the rest (nvdec_mpeg4_pic_s). Headers only: open-gpu-doc's nvdec_drv.h / clc9b0.h and the standard.
#pragma once
#include "rm.h"
#include "channel.h"

enum { MPEG4_SURFACES = 8 };

typedef struct {
    bool valid, short_header;       // short_header: H.263 baseline (no VOL; 14496-2 6.2.5.2 / H.263 5.1)
    int verid, width, height, time_res, time_bits, fixed_rate;
    int interlaced, sprite, quant_type, quarter_sample, resync_marker_disable, data_partitioned, reversible_vlc;
    int low_delay;
    uint8_t intra[64], nonintra[64];   // raster order
} Mpeg4Vol;

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;
    size_t pitch;
    RmMem setup, bs, offs, status, hist, scratch, coloc, sem;
    RmMem luma[MPEG4_SURFACES], chroma[MPEG4_SURFACES];
    bool busy[MPEG4_SURFACES];
    int fwd, bwd;                   // older / newer anchor (I / P / S) surfaces, -1 none
    Mpeg4Vol vol;
    // time stamps (in vop_time_increment_resolution units), as 14496-2 7.7.2 / ffmpeg's decoder keep them
    int64_t time_base, last_time_base, last_non_b_time, pp_time, pb_time, t_frame;
    NvU32 semval;
    // the VOP just decoded
    int cur_surf, cur_type, cur_w, cur_h;   // type: 0 I, 1 P, 2 B, 3 S, 4 not coded
    bool cur_shown;                 // false: a not-coded VOP (cur_surf repeats the last anchor)
    int64_t cur_time;
    unsigned mbs_ok, mbs_err, err_status;
    double engine_ms;
    int verbose;                    // 1: one line per VOP, 2: also the VOP headers
} Mpeg4Dec;

int  mpeg4dec_open(Mpeg4Dec *d, Rm *rm, unsigned maxW, unsigned maxH);
void mpeg4dec_close(Mpeg4Dec *d);
// Feed one start-code unit of an MPEG-4 elementary stream: start = pointer to its 00 00 01 xx, len = up to the next
// start code (a VOP goes to the engine as it is). Returns 1 when a VOP was decoded, 0 if not, -1 on error.
int  mpeg4dec_unit(Mpeg4Dec *d, const uint8_t *start, size_t len);
// H.263 (short video header): one picture from its picture start code (22 bits 0000 0000 0000 0000 1000 00).
int  mpeg4dec_h263_picture(Mpeg4Dec *d, const uint8_t *start, size_t len);
void mpeg4dec_release(Mpeg4Dec *d, int surf);
void mpeg4dec_read_i420(Mpeg4Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
void mpeg4dec_read_nv12(Mpeg4Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
// Split an MPEG-4 elementary stream at start codes (00 00 01 xx): *start points at the start code, *len to the next one.
bool mpeg4_next_unit(const uint8_t *buf, size_t n, size_t *pos, const uint8_t **start, size_t *len);
// Split an H.263 stream at picture start codes (00 00 8x / 00 00 8x with the 22-bit PSC byte aligned).
bool h263_next_picture(const uint8_t *buf, size_t n, size_t *pos, const uint8_t **start, size_t *len);
