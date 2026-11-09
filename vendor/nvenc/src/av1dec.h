// av1dec.h (M9): an AV1 decode session on NVDEC. src/av1parse.c reads the headers; this sets up nvdec_av1_pic_s and the
// engine's buffers (CDF tables, tile info / tile stream info, filter, intra top, temporal MV, segment ID) and decodes each
// frame into a surface. Sources: open-gpu-doc's nvdec_drv.h / clc9b0.h, the AV1 specification, and, for what the headers
// do not say (CDF layout, tile entries, picture slots, global motion, film grain buffers), mesa-nova's NVK AV1 decoder
// (MIT; user's decision 2026-10-19, docs/tasks.md M9).
#pragma once
#include "rm.h"
#include "channel.h"
#include "av1parse.h"

enum { AV1_SURFACES = 16 };       // 8 reference slots, the frame, its film-grain output, grain twins, frames held

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;            // allocation size (pixels, multiple of 128)
    int bit_depth, bps;
    size_t pitch;                   // bytes
    RmMem setup, bs, cdf_def, cdf_out, tileinfo, tilestream, filter, intra, gm, fg, status, sem;
    RmMem cdf[8];                   // the CDF tables saved with each reference slot (what the engine wrote)
    RmMem tmv[AV1_SURFACES], seg[AV1_SURFACES];   // per surface: temporal MVs and segment IDs it wrote
    RmMem luma[AV1_SURFACES], chroma[AV1_SURFACES];
    unsigned w[AV1_SURFACES], h[AV1_SURFACES], coded_w[AV1_SURFACES];   // upscaled size, and width before superres
    bool seg_on[AV1_SURFACES];
    int twin[AV1_SURFACES];         // the film-grain output of a hidden showable frame, for its show_existing_frame (-1 none)      // whether the surface's frame had segmentation (its segment IDs are meaningful)
    RmMem zseg;                     // zero segment IDs
    bool busy[AV1_SURFACES];
    int ref[8];                     // surface in each reference slot
    Av1State st;
    size_t flt_top, flt_vert, flt_cdef, flt_lr, flt_hus, flt_fgs;
    uint8_t *cdf_tab[4]; size_t cdf_size;   // default CDF tables per quantizer context (av1probs.c)
    NvU32 semval, picture_index;
    bool verbose;
    // the frame being collected / just decoded
    Av1Frame f; bool have_hdr;
    size_t bsN; int ntiles; struct { size_t start, end; int row, col; } tile[AV1_MAX_TILES];
    int cur_surf, cur_w, cur_h;
    int dec_surf;                   // the surface the last coded frame went to (hidden ones too; -1 after show_existing_frame)
    bool skipped; unsigned nskipped;   // the last temporal unit had a frame left out for references never decoded; how many so far
    unsigned err_status, ok_blocks;
    double engine_ms;
} Av1Dec;

int  av1dec_open(Av1Dec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth);
void av1dec_close(Av1Dec *d);
// Feed one temporal unit (a container sample: OBUs). Returns the number of frames to show it produced (0 or 1; their
// surface in cur_surf, held until av1dec_release), or -1 on error.
int  av1dec_tu(Av1Dec *d, const uint8_t *buf, size_t n);
void av1dec_release(Av1Dec *d, int surf);
void av1dec_read_i420(Av1Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
void av1dec_read_nv12(Av1Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
