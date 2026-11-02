// vc1dec.h (M14, T14.03): a VC-1 (SMPTE 421M: Simple / Main = WMV3, Advanced = WVC1) decode session on NVDEC. The engine
// parses the picture headers and the macroblock layer itself; the sequence header (Simple / Main: the 4-byte STRUCT_C,
// Advanced: sequence header + entry point) is parsed here, and the picture header only as far as reference management
// and output order need it (frame coding mode, picture type). Sources: open-gpu-doc's nvdec_drv.h / clc9b0.h, SMPTE 421M,
// and for what no header says (which setup fields the engine uses, the bitstream framing, buffer sizes) averne's FFmpeg
// "nvtegra" hwaccel (GPL, Tegra NVDEC; facts only, user's decision 2026-10-20).
#pragma once
#include "rm.h"
#include "channel.h"

enum { VC1_SURFACES = 8 };

typedef struct {
    bool valid, advanced;
    int width, height;              // coded size (Simple / Main: from the container; Advanced: max_coded / entry point)
    // sequence
    int postprocflag, pulldown, interlace, tfcntrflag, finterpflag, psf, multires, syncmarker, rangered, maxbframes;
    int hrd_buckets;
    // entry point (Simple / Main: sequence)
    int loopfilter, fastuvmc, extended_mv, extended_dmv, dquant, vstransform, overlap, quantizer, panscan, refdist_flag;
    int range_mapy_flag, range_mapuv_flag;
} Vc1Seq;

typedef struct {
    Rm *rm; Chan chan;
    unsigned maxW, maxH;
    size_t pitch;
    RmMem setup, bs, offs, status, coloc, hist, scratch, sem;
    RmMem luma[VC1_SURFACES], chroma[VC1_SURFACES];
    bool busy[VC1_SURFACES];
    int fwd, bwd;                   // older / newer anchor surfaces (-1 none)
    Vc1Seq seq;
    NvU32 semval;
    // the picture being collected (Advanced: frame / field / slices)
    uint8_t *au; size_t auCap, auN; NvU32 auOff[1024]; int auSlices; bool inPic, auHasCode;
    int fieldSurf;                  // second field goes into the first field's surface
    int fptype;                     // the field pair's FPTYPE, for the second field (its header has none)
    // the frame just decoded
    int cur_surf, cur_type, cur_w, cur_h;   // type: 0 I, 1 P, 2 B, 3 BI, 4 skipped (cur_surf = the last anchor again)
    bool cur_shown;                 // false: a skipped picture
    unsigned mbs_ok, mbs_err, err_status;
    double engine_ms;
    int verbose;
} Vc1Dec;

int  vc1dec_open(Vc1Dec *d, Rm *rm, unsigned maxW, unsigned maxH);
void vc1dec_close(Vc1Dec *d);
// Simple / Main: the 4-byte sequence header (STRUCT_C) and the frame size from the container
int  vc1dec_simple_main_header(Vc1Dec *d, const uint8_t structc[4], int width, int height);
// Simple / Main: one frame (as the RCV / ASF container stores it, without start codes). Returns 1 when a frame was
// decoded (or a skipped frame repeats the last anchor), 0 if not, -1 on error.
int  vc1dec_simple_main_frame(Vc1Dec *d, const uint8_t *buf, size_t len);
// Advanced: one unit from its start code (00 00 01 xx ...) up to the next. Returns 1 when a frame was completed by it,
// 0 if not, -1 on error; vc1dec_flush completes the last one.
int  vc1dec_advanced_unit(Vc1Dec *d, const uint8_t *start, size_t len);
int  vc1dec_flush(Vc1Dec *d);
void vc1dec_release(Vc1Dec *d, int surf);
void vc1dec_read_i420(Vc1Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h);
void vc1dec_read_nv12(Vc1Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h);
