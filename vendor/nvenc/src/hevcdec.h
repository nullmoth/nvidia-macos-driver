// hevcdec.h (M11): an HEVC decode session on NVDEC. src/hevcparse.c reads the parameter sets and slice headers; this
// applies the reference picture set (8.3.2), builds the initial reference lists (8.3.4), sets up nvdec_hevc_pic_s and the
// engine's buffers (scaling lists, tile sizes, filter lines, co-located MVs) and decodes each picture into a surface. The
// engine parses the slice segment headers and slice data itself. Headers only: open-gpu-doc's nvdec_drv.h / clc9b0.h and
// ITU-T H.265; what the headers do not say is in docs/tasks.md (M11).
#pragma once
#include "rm.h"
#include "channel.h"
#include "hevcparse.h"

enum { HEVC_SURFACES = 16 };      // picture slots: the engine's reference arrays have 16 entries

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;            // allocation size (pixels, multiple of 64)
    int bit_depth, bps, chroma_format;
    size_t pitch;                   // bytes
    RmMem setup, bs, scaling, tiles, filter, coloc, intra, slinfo, status, sem;
    size_t colmv_size, flt_sao, flt_bsd, flt_above, flt_sao_above, flt_edge;
    RmMem luma[HEVC_SURFACES], chroma[HEVC_SURFACES];
    // per surface
    int poc[HEVC_SURFACES];
    int ref[HEVC_SURFACES];         // 0 unused for reference, 1 short-term, 2 long-term
    bool busy[HEVC_SURFACES];       // held by the caller until hevcdec_release
    HevcParams params; HevcPocState pocst;
    bool first_pic, after_eos, skip_rasl;
    NvU32 semval;
    bool verbose;
    bool turing;                    // NVDEC 0xc4b0 (GTX 1650): no SET_INTRA_TOP_BUF_OFFSET
    // the picture just decoded
    int cur_surf, cur_poc, cur_w, cur_h, cur_crop[4];   // crop: left, right, top, bottom (luma samples)
    bool cur_output;                // pic_output_flag (and not a skipped RASL picture)
    bool cur_irap_nro;              // an IRAP with NoRaslOutputFlag: pictures waiting for output before it come first,
    bool cur_no_output_prior;       // or, with NoOutputOfPriorPicsFlag, are discarded (C.5.2.2)
    int max_reorder, max_dec_pic, max_latency;   // of the active SPS (highest sub-layer; latency in pictures, 0 = none)
    unsigned err_status, ok_blocks;
    double engine_ms;
    // the access unit being collected (slice segments with start codes)
    uint8_t *au; size_t auCap, auN; int auSlices; HevcSlice auFirst;
} HevcDec;

int  hevcdec_open(HevcDec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth, int chroma_format);
void hevcdec_close(HevcDec *d);
// Feed one NAL unit (without start code). Returns 1 when a picture was decoded (the previous access unit, finished by
// this NAL; its surface in cur_surf, held until hevcdec_release), 0 if not, -1 on error. hevcdec_flush decodes the last.
int  hevcdec_nal(HevcDec *d, const uint8_t *nal, size_t len);
int  hevcdec_flush(HevcDec *d);
void hevcdec_release(HevcDec *d, int surf);
// Copy a decoded surface out: I420 / I444 cropped (16-bit little-endian samples above 8 bits), or biplanar (NV12 / P010,
// 4:4:4 like NV24) w x h from (x0, y0) (the conformance window's left / top crop).
void hevcdec_read_yuv(HevcDec *d, int surf, uint8_t *dst, const int crop[4], unsigned w, unsigned h);
void hevcdec_read_nv12(HevcDec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h, unsigned x0, unsigned y0);
