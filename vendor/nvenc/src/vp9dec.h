// vp9dec.h (M10): a VP9 decode session on NVDEC. src/vp9parse.c parses the headers and keeps the probability contexts;
// this sets up nvdec_vp9_pic_s, the probability table (nvdec_vp9EntropyProbs_t), the segmentation maps, column MVs,
// tile sizes and filter buffer, decodes each frame into a surface and feeds the engine's counters back for backward
// adaptation. 4:2:0, 8/10/12-bit (one bit depth per session: 16-bit samples for 10/12). Headers only: open-gpu-doc's nvdec_drv.h / clc9b0.h and the VP9 specification;
// what the headers do not say is in docs/tasks.md (M10).
#pragma once
#include "rm.h"
#include "channel.h"
#include "vp9parse.h"

enum { VP9_SURFACES = 12 };         // 8 reference slots + the frame being decoded + frames the caller still holds

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;            // allocation size (pixels, multiple of 64)
    int bit_depth, bps;             // the session's bit depth; bytes per sample (1, or 2 for 10/12-bit)
    size_t pitch;                   // bytes
    RmMem setup, bs, probs, counts, seg[2], tiles, colmv[2], filter, intra, status, sem;
    RmMem luma[VP9_SURFACES], chroma[VP9_SURFACES];
    unsigned w[VP9_SURFACES], h[VP9_SURFACES];  // the size each surface was decoded at
    bool busy[VP9_SURFACES];        // held by the caller until vp9dec_release
    int ref[8];                     // surface in each reference slot (-1 = empty)
    Vp9State st;
    int seg_prev, mv_prev;          // which seg[] holds PrevSegmentIds, which colmv[] the previous frame's MVs
    int last_w, last_h, last_show;  // the previous decoded frame (compute_image_size, 7.2.6)
    bool have_last;
    bool turing;                    // NVDEC 0xc4b0 (GTX 1650): no SET_INTRA_TOP_BUF_OFFSET
    size_t flt_above, flt_bsd;      // offsets in the filter buffer
    NvU32 semval;
    bool verbose;
    // the frame just decoded
    Vp9Frame f;
    int cur_surf, cur_w, cur_h;     // the picture to show (valid when vp9dec_frame returned 1)
    int dec_surf;                   // the surface the last coded frame went to (also hidden ones; -1 for show_existing_frame)
    unsigned err_status, ok_blocks;
    double engine_ms;
} Vp9Dec;

int  vp9dec_open(Vp9Dec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth);
void vp9dec_close(Vp9Dec *d);
// Decode one frame (a superframe already split, vp9_superframe). Returns 1 when a picture is to be shown (cur_surf,
// held until vp9dec_release), 0 for a hidden frame, -1 on error.
int  vp9dec_frame(Vp9Dec *d, const uint8_t *buf, size_t n);
void vp9dec_release(Vp9Dec *d, int surf);
// Take over another session's stream state (probability contexts, reference slots and their pictures, the previous
// frame's size) into a freshly opened, at least as large session `to` (same Rm, same bit depth): for a stream that grows
// past the session it started in. `from` can be closed afterwards.
int  vp9dec_migrate(Vp9Dec *to, const Vp9Dec *from);
void vp9dec_read_nv12(Vp9Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
// I420 (8-bit) or yuv420p10le / 12le (16-bit little-endian samples, value in the low bits) for 10/12-bit sessions
void vp9dec_read_i420(Vp9Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
