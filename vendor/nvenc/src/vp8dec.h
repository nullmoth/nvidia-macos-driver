// vp8dec.h (M14, T14.04): a VP8 decode session on NVDEC. NVDEC parses the frame header and the macroblock data itself
// (nvdec_vp8_pic_s carries only sizes, the key frame flag, the first partition's size and buffers) and keeps the
// probabilities in a buffer of its own; the frame header is parsed here only as far as reference management needs it
// (RFC 6386 9.2-9.8: golden / altref refresh and copies, refresh_last). Sources: open-gpu-doc's nvdec_drv.h / clc9b0.h,
// RFC 6386 (tables), and for what no header says (the probability buffer's layout, the bitstream start, strides) averne's
// FFmpeg "nvtegra" hwaccel (GPL, Tegra NVDEC; used as a reference for facts only, user's decision 2026-10-20).
#pragma once
#include "rm.h"
#include "channel.h"

enum { VP8_SURFACES = 8 };

typedef struct {
    int key, version, show, first_part_size, width, height;   // frame tag (9.1) and key frame start (width / height in pixels)
    int refresh_golden, refresh_alt, copy_to_golden, copy_to_alt, sign_bias_golden, sign_bias_alt, refresh_entropy, refresh_last;
    int segmentation, update_seg_data, filter_level, partitions;
} Vp8Header;

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;
    size_t pitch;
    RmMem setup, bs, prob, hist, status, sem;
    RmMem luma[VP8_SURFACES], chroma[VP8_SURFACES];
    bool busy[VP8_SURFACES];
    int golden, altref, last;       // reference surfaces (-1 none)
    int width, height;              // from the last key frame
    NvU32 semval;
    // the frame just decoded
    Vp8Header h;
    int cur_surf;
    unsigned mbs_ok, mbs_err, err_status, result;
    double engine_ms;
    int verbose;
} Vp8Dec;

int  vp8dec_open(Vp8Dec *d, Rm *rm, unsigned maxW, unsigned maxH);
void vp8dec_close(Vp8Dec *d);
// Decode one compressed frame. Returns 1 when a frame was decoded (cur_surf; h.show says whether it is shown), 0 when
// it was skipped (an inter frame before the first key frame), -1 on error.
int  vp8dec_frame(Vp8Dec *d, const uint8_t *buf, size_t len);
void vp8dec_release(Vp8Dec *d, int surf);
void vp8dec_read_i420(Vp8Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
void vp8dec_read_nv12(Vp8Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
int  vp8_parse_header(const uint8_t *buf, size_t len, Vp8Header *h);
