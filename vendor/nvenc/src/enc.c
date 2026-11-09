// enc.c: see enc.h.
#include "enc.h"
#include "h264enctmpl.h"
#include "blocklinear.h"
#include "h264hdr.h"
#include "hevchdr.h"
#include "av1hdr.h"
#include "av1probs.h"
#include "av1enctmpl.h"
#include "hevcenctmpl.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include "class/clc56f.h"
#include "class/cl2080_notification.h"
#include "clc9b7.h"
#define NV_NVENC_8_2 1
#include "nvenc_drv.h"

// picture setup buffer: the 512-byte header, then one control of each kind
enum { OFF_ME = 0x400, OFF_MD = 0x500, OFF_Q = 0x600, OFF_SLICE = 0x700, OFF_AV1_TILE = 0x800, SETUP_SIZE = 0x1000 };   // room for ENC_MAX_SLICES controls
enum { LOG2_MAX_FRAME_NUM = 8 };

static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

// AV1: the RC data buffer's activity and pass-2 areas (the driver's offsets: 0x100 + n, 0x100 + 2n; n = 64 bytes per MB, in pages)
static size_t av1_rc_area(const Enc *e) { return (((size_t)e->wmb * e->hmb * 64) + 0xfff) & ~(size_t)0xfff; }

// rate control: the anchors (I / P) per GOP, as nvenc_drv.h defines Np, (gop_length + num_b_frames - 1) / (num_b_frames + 1);
// NVIDIA's CBR trace (P only) has GOP - 1. With 2 B pictures GOP - 1 made the RC start far too high with two passes (T7.09)
static int rc_np(const Enc *e)
{
    const int gop = e->cfg.idr_period, bf = e->cfg.bframes > 0 ? e->cfg.bframes : 0;
    return gop > 0 ? (gop + bf - 1) / (bf + 1) : 0x7fffffff;
}

// The picture setup's magic: the NVENC class in bits 31..16, version 0, revision 6 (nvenc_drv.h: 0xC9B70006 for Ada's
// NVENC 8.2, 0xB4B70006 for NVENC 6.6 = the GTX 1650's TU117); the setup layout is the same for every version
static NvU32 enc_magic(Enc *e) { const NvU32 c = rm_class(&e->rm, RM_CLS_NVENC); return c ? (c << 16) | 0x0006u : NV_NVENC_DRV_MAGIC_VALUE; }

int enc_open(Enc *e, const EncConfig *cfg)
{
    memset(e, 0, sizeof *e);
    e->cfg = *cfg;
    if (e->cfg.num_refs < 1) e->cfg.num_refs = 1;
    if (e->cfg.num_refs > ENC_MAX_REFS) e->cfg.num_refs = ENC_MAX_REFS;
    if (e->cfg.codec != ENC_AV1 && e->cfg.num_refs > 3) e->cfg.num_refs = 3;
    // H.264 4:4:4: the engine's CABAC coder only knows 4:2:0 (broken IDRs, then an engine exception, RM error 0x2f on P
    // frames); CAVLC 4:4:4 is correct (M13 T13.02)
    if (cfg->codec == ENC_H264 && (cfg->chroma444 || cfg->lossless)) e->cfg.cabac = false;
    if (cfg->lossless) { e->cfg.qp_i = e->cfg.qp_p = 0; e->cfg.rc_mode = 0; }   // lossless: constant QP 0
    int W = cfg->width, H = cfg->height;
    e->wmb = (W + 15) / 16; e->hmb = (H + 15) / 16;
    Rm *rm = &e->rm; rm->verbose = cfg->verbose;
    if (rm_open(rm)) return -1;
    rm->verbose = cfg->verbose;
    if (rm_va_init(rm)) return -1;

    // input: NVENC's input layout (block_height 2); the engine reads 8-12 KB past the chroma plane, hence the slack.
    // HEVC: planes cover whole 32-row CTBs (the engine pads the source to the CTB grid itself, but reads that far)
    bool hevc = cfg->codec == ENC_HEVC, av1 = cfg->codec == ENC_AV1;
    // AV1 encoding came with Ada's NVENC (0xc9b7); NVENC 6.6 (Turing, 0xb4b7) and Ampere's have none
    if (av1 && rm_class(rm, RM_CLS_NVENC) != 0xc9b7) { fprintf(stderr, "enc: this GPU's NVENC (class 0x%x) has no AV1\n", rm_class(rm, RM_CLS_NVENC)); return -1; }
    // 10-bit (HEVC Main 10): input and reconstructed samples are 16 bits; the surface configs keep the pitch in samples,
    // the planes and field offsets double (nullmoth's libnvenc_hevc, docs/reports/T6.14.md)
    if ((!hevc && !av1) || e->cfg.bit_depth != 10) e->cfg.bit_depth = 8;
    e->bps = e->cfg.bit_depth > 8 ? 2 : 1;
    // AV1: whole 64x64 superblocks
    size_t inRows = av1 ? (((size_t)H + 63) & ~(size_t)63) : hevc ? (((size_t)e->hmb * 16 + 31) & ~(size_t)31) : (size_t)e->hmb * 16;
    e->pitch = ((size_t)e->wmb * 16 + 63) & ~(size_t)63;
    e->lumaSz = (nvenc_in_plane_size(e->pitch * e->bps, inRows) + 255) & ~(size_t)255;
    e->chromaSz = nvenc_in_plane_size(e->pitch * e->bps, inRows / 2);
    if (cfg->chroma444 && !av1) e->chromaSz = 2 * e->lumaSz;   // 4:4:4: two full-size planes (or one interleaved plane twice as wide)
    // reconstructed pictures: block-linear block_height 2, rows rounded up to 32 (the engine writes that far)
    e->rPitch = ((size_t)e->wmb * 16 + 255) & ~(size_t)255;
    const size_t rRows = av1 ? (((size_t)H + 63) & ~(size_t)63) : (((size_t)e->hmb * 16 + 31) & ~(size_t)31);
    e->rLuma = (e->rPitch * e->bps * rRows + 255) & ~(size_t)255;
    e->rSlot = (e->rLuma + e->rPitch * e->bps * (av1 ? rRows / 2 : (((size_t)e->hmb * 8 + 31) & ~(size_t)31)) + 0xffff) & ~(size_t)0xffff;
    if (cfg->chroma444 && !av1) e->rSlot = (3 * e->rLuma + 0xffff) & ~(size_t)0xffff;   // 4:4:4: chroma twice the luma plane
    if (hevc || av1) e->rSlot += 0x10000;   // nullmoth's HEVC slots are 64 KB larger than the planes (T6.03); what goes there is unknown
    // NVENC 6.6 (GTX 1650) has no HEVC B pictures (NVIDIA's driver refuses hevc_nvenc -bf on it): P only there
    if (hevc && e->cfg.bframes > 0 && rm_class(rm, RM_CLS_NVENC) == 0xb4b7) e->cfg.bframes = 0;
    int slots = e->cfg.num_refs + 1;
    if (e->cfg.bframes > 0) { if (e->cfg.num_refs < 2) e->cfg.num_refs = 2; slots = e->cfg.num_refs + 2; e->bSlot = slots - 1; }
    e->nSlots = slots; e->av1LastShown = -1; e->av1HidDisp = -1;
    e->aqStride = (((size_t)(H + 31) / 32 * 256) + 0xfff) & ~(size_t)0xfff;   // AQ: a 256-byte row per CTB row
    e->av1Hq = cfg->codec == ENC_AV1 && (cfg->quality || cfg->bframes > 0);
    size_t mbs = (size_t)e->wmb * e->hmb;
    size_t histSz = ((((size_t)(W - 1) + 16) / 16 + 16) * H264_HIST_BLOCK_SIZE + 0xffff) & ~(size_t)0xffff;
    if (hevc) histSz = ((size_t)(W + 31) / 32 * 768 + 0xffff) & ~(size_t)0xffff;   // HEVC: 768 bytes per CTB column (nullmoth's size, T6.06)
    // NVENC 6.6 (GTX 1650, class 0xb4b7) keeps HEVC history for the whole frame: 768 bytes per CTB plus half a row
    // (NVIDIA's Linux driver: 27648 / 1589760 / 6312960 at 256x128 / 1080p / 4K; docs/traces/gtx1650-linux). With Ada's
    // size the second P picture faulted (MMU fault)
    if (hevc && rm_class(rm, RM_CLS_NVENC) == 0xb4b7) {
        const size_t cw = ((size_t)W + 31) / 32, ch = ((size_t)H + 31) / 32;
        histSz = (768 * cw * ch + 384 * cw + 0xffff) & ~(size_t)0xffff;
    }
    e->av1SbCols = (W + 63) / 64; e->av1SbRows = (H + 63) / 64;
    if (av1) {   // tiles (uniform spacing): the fewest the spec allows, at most 64 superblocks (4096) wide and 2304 superblocks
        // (4096x2304) in area (A.3, 5.9.15); ENC_AV1_TILE_ROWS_LOG2 asks for more rows
        int cl = 0, rl = 0;
        while ((e->av1SbCols + (1 << cl) - 1) >> cl > 64) cl++;
        while (((e->av1SbCols + (1 << cl) - 1) >> cl) * ((e->av1SbRows + (1 << rl) - 1) >> rl) > 2304) rl++;
        if (getenv("ENC_AV1_TILE_ROWS_LOG2") && atoi(getenv("ENC_AV1_TILE_ROWS_LOG2")) > rl) rl = atoi(getenv("ENC_AV1_TILE_ROWS_LOG2"));
        e->av1TileColsLog2 = cl; e->av1TileRowsLog2 = rl;
        e->av1TileW = (e->av1SbCols + (1 << cl) - 1) >> cl; e->av1TileH = (e->av1SbRows + (1 << rl) - 1) >> rl;
        e->av1TileCols = (e->av1SbCols + e->av1TileW - 1) / e->av1TileW; e->av1TileRows = (e->av1SbRows + e->av1TileH - 1) / e->av1TileH;
    }
    if (av1)   // nvenc_drv.h: AV1_HIST_PIXEL_FRAME_SIZE + AV1_HIST_MV_SB_SIZE per superblock (generous: per superblock of the frame)
        histSz = ((size_t)AV1_HIST_PIXEL_FRAME_SIZE + (size_t)e->av1SbCols * e->av1SbRows * AV1_HIST_MV_SB_SIZE + (1 << 20) + 0xffff) & ~(size_t)0xffff;
    size_t meSz = (mbs * 64 + 0xffff) & ~(size_t)0xffff;   // ME predictor data per MB (the reference client uses 4 MB at any size)
    if (e->cfg.depth < 1) e->cfg.depth = 1;
    if (e->cfg.depth > ENC_MAX_DEPTH) e->cfg.depth = ENC_MAX_DEPTH;
    for (int d = 0; d < e->cfg.depth; d++)
        if (rm_mem_alloc(rm, &e->in[d], e->lumaSz + e->chromaSz + (64 << 10), 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &e->setup[d], SETUP_SIZE, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &e->bs[d], (mbs * 384 + (1 << 20) + 0xffff) & ~(size_t)0xffff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &e->status[d], av1 ? 0x40000 : 0x10000, 0x1000, true, true, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
        else memset(e->in[d].cpu, 0, e->in[d].size);
    if (rm_mem_alloc(rm, &e->recon, e->rSlot * (size_t)slots, 0x10000, false, false, true) ||
        rm_mem_alloc(rm, &e->hist, histSz, 0x1000, false, false, true) ||
        rm_mem_alloc(rm, &e->coloc, (mbs * 64 + 0xffff) & ~(size_t)0xffff, 0x1000, false, false, true) ||
        rm_mem_alloc(rm, &e->colocB, (mbs * 64 + 0xffff) & ~(size_t)0xffff, 0x1000, false, false, true) ||
        rm_mem_alloc(rm, &e->mepred[0], meSz, 0x1000, false, false, true) ||
        rm_mem_alloc(rm, &e->mepred[1], meSz, 0x1000, false, false, true) ||
        rm_mem_alloc(rm, &e->rcproc, 0x10000, 0x1000, true, true, true) ||
        rm_mem_alloc(rm, &e->rcdata, av1 ? 0x100 + 3 * av1_rc_area(e) + 0x1000 : 0x10000 + (size_t)ENC_MAX_DEPTH * ((((size_t)H + 31) / 32 * 256 + 0xfff) & ~(size_t)0xfff), 0x1000, true, true, true) ||
        rm_mem_alloc(rm, &e->sem, 0x1000, 0x1000, true, true, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
    memset(e->rcproc.cpu, 0, e->rcproc.size); memset(e->rcdata.cpu, 0, e->rcdata.size);
    if (av1 || !e->cfg.rc_mode) e->cfg.two_pass = false;
    if (e->cfg.two_pass) {   // the first pass's own outputs (NVIDIA's status buffer for it: 2.3 MB at 1080p, per-MB statistics)
        for (int d = 0; d < e->cfg.depth; d++)
            if (rm_mem_alloc(rm, &e->setup1[d], SETUP_SIZE, 0x1000, true, true, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
        if (rm_mem_alloc(rm, &e->bs1, e->bs[0].size, 0x1000, false, false, true) ||
            rm_mem_alloc(rm, &e->status1, ((mbs + 3) * 256 + 0x1ffff) & ~(size_t)0xffff, 0x1000, false, false, true) ||
            rm_mem_alloc(rm, &e->hist1, histSz, 0x1000, false, false, true) ||
            rm_mem_alloc(rm, &e->rec1, e->rSlot, 0x10000, false, false, true) ||
            rm_mem_alloc(rm, &e->coloc1, (mbs * 64 + 0xffff) & ~(size_t)0xffff, 0x1000, false, false, true) ||
            rm_mem_alloc(rm, &e->mepred1, meSz, 0x1000, false, false, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
    }
    if (!av1 && (e->cfg.has_mdcv || e->cfg.has_cll) && !(e->seiBuf = malloc(e->bs[0].size + 256))) return -1;
    if (av1) {
        // as NVIDIA's Linux driver sets them up (docs/traces/av1-linux, M12): the four default CDF tables (av1probs.c, the
        // same layout NVDEC uses) 0x3000 apart, the microcode picking one by quantizer (prob_tables_offset); two buffers for
        // the tables the engine writes (a P frame reads the previous frame's); the entropy / filter line buffers; the RC
        // process buffer with the words the driver puts there before the first frame
        if (rm_mem_alloc(rm, &e->av1ProbIn, 4 * 0x3000, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &e->av1Prob, (size_t)(slots + 1) * 0x10000, 0x1000, true, true, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
        memset(e->av1ProbIn.cpu, 0, e->av1ProbIn.size); memset(e->av1Prob.cpu, 0, e->av1Prob.size);
        for (int q = 0; q < 4; q++) av1_default_probs(q, (uint8_t *)e->av1ProbIn.cpu + q * 0x3000);
        for (int i = 0; i < 6; i++) {
            if (rm_mem_alloc(rm, &e->av1Line[i], 1 << 20, 0x1000, true, true, true)) { fprintf(stderr, "enc: allocation failed\n"); return -1; }
            memset(e->av1Line[i].cpu, 0, e->av1Line[i].size);
        }
        memcpy(e->rcproc.cpu, av1_tmpl_rcprocess, sizeof av1_tmpl_rcprocess);
        e->av1TuCap = 2 * e->bs[0].size + 4096;                     // room for a hidden anchor in front of the frame
        for (int d = 0; d < e->cfg.depth; d++) if (!(e->av1Tu[d] = malloc(e->av1TuCap))) return -1;
        if (!(e->av1Hid = malloc(e->bs[0].size + 4096))) return -1;
    }
    ((volatile NvU32 *)e->sem.cpu)[0] = 0;
    if (!e->cfg.fps) e->cfg.fps = 60;
    if (!e->cfg.vbv_bits) e->cfg.vbv_bits = e->cfg.bitrate;
    if (!e->cfg.max_bitrate) e->cfg.max_bitrate = e->cfg.bitrate;
    if (!av1 && e->cfg.rc_mode) {
        // the RC process buffer's initial state (nvenc_persistent_state_s), which the driver fills (nvenc_drv.h 2.9:
        // NVENC_RC_INIT_COMPLEXITY): P / B / I complexity in bits per pixel at Qs 1 (27.4; NVIDIA's values at 1080p: 15-19 /
        // 5-6 / 35-41), the GOP's P / B counts, the CPB half full. Left at zero the model thinks bits are free: the first IDR
        // took ~37 frames' budget (4.6 Mbit at QP 21 for 120 kbit frames) and the rest of a short clip starved. T7.09: +0.6-1.0
        // VMAF at equal rate over 900 frames, the rate on target (7.21 for 7.2 Mbit/s instead of 7.44). ENC_RCINIT=wp,wb,wi,full
        int wp = 16, wb = 6, wi = 38; double full = 0.5;
        if (getenv("ENC_RCINIT")) sscanf(getenv("ENC_RCINIT"), "%d,%d,%d,%lf", &wp, &wb, &wi, &full);
        nvenc_persistent_state_s *ps = e->rcproc.cpu;
        const int w[3] = { wp, wb, wi };
        for (int k = 0; k < 3; k++) { ps->Wpbi[k] = w[k]; for (int t = 0; t < 3; t++) ps->Whist[k][t] = w[k]; }
        const int gop = e->cfg.idr_period, np = rc_np(e);
        if (gop > 0) { ps->np = np << 4; ps->nb = (gop - 1 - np) << 4; }
        ps->nal_cpb_fullness = ps->vcl_cpb_fullness = (int)(full * e->cfg.vbv_bits * 16);
    }

    NV_NVENC_ALLOCATION_PARAMETERS ep = { .size = sizeof ep, .engineInstance = (NvU32)cfg->engine };
    if (chan_create(rm, &e->chan, cfg->engine ? NV2080_ENGINE_TYPE_NVENC1 : NV2080_ENGINE_TYPE_NVENC0, rm_class(rm, RM_CLS_NVENC), &ep)) return -1;
    chan_mthd(&e->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVENC));
    // completion event (see enc_encode)
    if (!cfg->spin && rm_event_open(rm, &e->ev, cfg->engine ? NV2080_NOTIFIERS_NVENC1 : NV2080_NOTIFIERS_NVENC0)) {
        if (cfg->verbose) fprintf(stderr, "enc: no NVENC event - spinning\n");
        memset(&e->ev, 0, sizeof e->ev);
    }
    return 0;
}

void enc_close(Enc *e)
{
    if (e->ev.h) rm_event_close(&e->rm, &e->ev);
    chan_destroy(&e->chan);
    RmMem *m[] = { &e->recon, &e->hist, &e->coloc, &e->colocB, &e->mepred[0], &e->mepred[1], &e->sem, &e->rcproc, &e->rcdata, &e->av1ProbIn, &e->av1Prob,
                   &e->av1Line[0], &e->av1Line[1], &e->av1Line[2], &e->av1Line[3], &e->av1Line[4], &e->av1Line[5],
                   &e->bs1, &e->status1, &e->hist1, &e->rec1, &e->coloc1, &e->mepred1 };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) if (m[i]->hPhys) rm_mem_free(&e->rm, m[i]);
    for (int d = 0; d < ENC_MAX_DEPTH; d++) { free(e->av1Tu[d]); e->av1Tu[d] = NULL; }
    free(e->av1Hid); e->av1Hid = NULL; free(e->seiBuf); e->seiBuf = NULL;
    for (int d = 0; d < ENC_MAX_DEPTH; d++) {
        RmMem *pm[] = { &e->in[d], &e->setup[d], &e->bs[d], &e->status[d], &e->setup1[d] };
        for (int i = 0; i < 5; i++) if (pm[i]->hPhys) rm_mem_free(&e->rm, pm[i]);
    }
    rm_close(&e->rm);
}

static H264Params hdr_params(const Enc *e)
{
    return (H264Params){ .width = e->cfg.width, .height = e->cfg.height, .colour = { e->cfg.colour[0], e->cfg.colour[1], e->cfg.colour[2] }, .profile_idc = e->cfg.chroma444 || e->cfg.lossless ? 244 : e->cfg.h264_profile == 100 ? 100 : 77, .level_idc = e->wmb * e->hmb > 36864 ? 60 : 51, .chroma444 = e->cfg.chroma444, .lossless = e->cfg.lossless,
        .log2_max_frame_num_minus4 = LOG2_MAX_FRAME_NUM - 4, .poc_type = e->cfg.bframes > 0 ? 0 : 2, .log2_max_poc_lsb_minus4 = 4,
        .num_reorder = e->cfg.bframes > 0 ? 1 : 0, .num_ref_frames = e->cfg.num_refs,
        .num_ref_idx_l0_minus1 = e->cfg.num_refs - 1, .entropy_cabac = e->cfg.cabac, .transform_8x8 = e->cfg.h264_profile == 100 || e->cfg.chroma444, .pic_init_qp = 26, .vui = 1, .full_range = e->cfg.full_range };
}

// ENC_HEVC_OFF=ts,cqd,sao,amp,sis: experiment switches (T6.05), applied to the headers and the engine setup alike
static int hevc_off(const char *k) { const char *v = getenv("ENC_HEVC_OFF"); return v && strstr(v, k) != NULL; }

static HevcParams hevc_params(const Enc *e)
{
    return (HevcParams){ .width = e->cfg.width, .height = e->cfg.height, .log2_max_poc_lsb_minus4 = 4,
        .max_dec_pic_buffering = e->cfg.num_refs + 1 + e->cfg.dpb_extra, .sps_rps_negative = e->cfg.num_refs, .num_reorder = e->cfg.bframes > 0 ? 1 : 0,
        .num_ref_idx_l0_default = e->cfg.num_refs, .pic_init_qp = 26, .vui = 1, .full_range = e->cfg.full_range,
        .colour = { e->cfg.colour[0], e->cfg.colour[1], e->cfg.colour[2] }, .min_cb8 = e->cfg.quality && !e->cfg.no_cu8 && !hevc_off("cu8"), .bit_depth = e->cfg.bit_depth, .chroma444 = e->cfg.chroma444, .lossless = e->cfg.lossless,
        .no_sdh = 1,   /* the engine does not apply sign data hiding: with it signalled, every residual decodes wrong (T6.05) */
        .no_ts = hevc_off("ts") || e->cfg.bit_depth > 8,   /* nullmoth's 10-bit setup turns transform skip off */ .no_cqd = hevc_off("cqd"), .no_sao = hevc_off("sao"), .no_amp = hevc_off("amp"),
        .sis = hevc_off("sis") };
}

size_t enc_headers(Enc *e, uint8_t *out)
{
    if (e->cfg.codec == ENC_AV1) return 0;   // the sequence header travels in each key frame's temporal unit
    if (e->cfg.codec == ENC_HEVC) {
        HevcParams hp = hevc_params(e);
        size_t n = hevc_write_vps(&hp, out);
        n += hevc_write_sps(&hp, out + n);
        return n + hevc_write_pps(&hp, out + n);
    }
    H264Params hp = hdr_params(e);
    size_t n = h264_write_sps(&hp, out);
    return n + h264_write_pps(&hp, out + n);
}

// 8-bit rows widened to 16-bit samples (value << 8) for a 10-bit session, then tiled like P010
static void load_widened(Enc *e, uint8_t *dst, const uint8_t *src, size_t stride, size_t width, size_t rows)
{
    uint16_t *buf = malloc(width * 2 * rows);
    if (!buf) return;
    for (size_t r = 0; r < rows; r++) for (size_t c = 0; c < width; c++) buf[r * width + c] = (uint16_t)(src[r * stride + c] << 8);
    // the plane's pitch: the luma pitch, or twice it for a row wider than that (4:4:4 interleaved chroma)
    nvenc_in_tile_plane(dst, (const uint8_t *)buf, width * 2, width * 2, rows, e->pitch * 2 * (width > e->pitch ? 2 : 1));
    free(buf);
}

// Spatial adaptive quantization (T7.07, T7.08): a QP offset per 32x32 CTB from its luma variance, as NVIDIA's driver computes
// them for -spatial_aq (docs/traces/quality-linux vsaq): about log2(variance + 1) - 8.6, clipped to +-strength (-8 on flat
// CTBs, a few steps up on detailed ones; the rate control rebalances). The engine reads them with aqMode 3 (external delta
// QP map) from the RC data at the picture control's act_stat_offset: one signed byte per CTB, rows 256 bytes apart.
static void aq_compute(Enc *e, const void *y, size_t stride, int bytes)
{
    const int slot = (int)(e->seq % (NvU32)e->cfg.depth);
    e->aqValid[slot] = false;
    if (!e->cfg.aq || e->cfg.codec != ENC_HEVC) return;
    const int W = e->cfg.width, H = e->cfg.height, cw = (W + 31) / 32, ch = (H + 31) / 32, k = e->cfg.aq;
    signed char *a = (signed char *)e->rcdata.cpu + 0x10000 + (size_t)slot * e->aqStride;
    for (int by = 0; by < ch; by++) for (int bx = 0; bx < cw; bx++) {
        const int x0 = bx * 32, y0 = by * 32, w = W - x0 < 32 ? W - x0 : 32, h = H - y0 < 32 ? H - y0 : 32;
        uint64_t sum = 0, ss = 0;
        for (int r = 0; r < h; r++) {
            if (bytes == 1) { const uint8_t *p = (const uint8_t *)y + (size_t)(y0 + r) * stride + x0; for (int c = 0; c < w; c++) { sum += p[c]; ss += (uint32_t)p[c] * p[c]; } }
            else { const uint16_t *p = (const uint16_t *)((const uint8_t *)y + (size_t)(y0 + r) * stride) + x0; for (int c = 0; c < w; c++) { const uint32_t v = p[c] >> 8; sum += v; ss += v * v; } }
        }
        const int n = w * h; const double var = ((double)ss - (double)sum * sum / n) / n, o = log2(var + 1) - 8.6;
        a[by * 256 + bx] = (signed char)(o > k ? k : o < -k ? -k : (o < 0 ? o - 0.5 : o + 0.5));
    }
    e->aqValid[slot] = true;
}

// 16-bit samples as P010 has them (10 bits in the high bits): NVENC takes them as they are, in the 8-bit layout with
// twice the bytes per row (low-bit alignment gives garbage, T6.14)
static void load_16(Enc *e, uint8_t *dst, const uint16_t *src, size_t stride, size_t width, size_t rows)
{
    nvenc_in_tile_plane(dst, (const uint8_t *)src, stride, width * 2, rows, e->pitch * 2);
}

void enc_load_p010(Enc *e, const uint16_t *y, size_t yStride, const uint16_t *uv, size_t uvStride)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    aq_compute(e, y, yStride, 2);
    load_16(e, in->cpu, y, yStride, (size_t)e->cfg.width, (size_t)e->cfg.height);
    load_16(e, (uint8_t *)in->cpu + e->lumaSz, uv, uvStride, (size_t)(e->cfg.width + 1) & ~(size_t)1, (size_t)(e->cfg.height + 1) / 2);
}

void enc_load_nv12(Enc *e, const uint8_t *y, size_t yStride, const uint8_t *uv, size_t uvStride)
{
    aq_compute(e, y, yStride, 1);
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    if (e->bps == 2) {
        load_widened(e, in->cpu, y, yStride, (size_t)e->cfg.width, (size_t)e->cfg.height);
        load_widened(e, (uint8_t *)in->cpu + e->lumaSz, uv, uvStride, (size_t)(e->cfg.width + 1) & ~(size_t)1, (size_t)(e->cfg.height + 1) / 2);
        return;
    }
    nvenc_in_tile_plane(in->cpu, y, yStride, (size_t)e->cfg.width, (size_t)e->cfg.height, e->pitch);
    nvenc_in_tile_plane((uint8_t *)in->cpu + e->lumaSz, uv, uvStride, (size_t)e->cfg.width, (size_t)(e->cfg.height + 1) / 2, e->pitch);
}

void enc_load_yuv444(Enc *e, const uint8_t *y, size_t ys, const uint8_t *u, size_t us, const uint8_t *v, size_t vs)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    const size_t W = (size_t)e->cfg.width, H = (size_t)e->cfg.height;
    uint8_t *buf = malloc(2 * W * H); if (!buf) return;           // Cb Cr interleaved, twice the pitch
    for (size_t r = 0; r < H; r++) for (size_t c = 0; c < W; c++) { buf[r * 2 * W + 2 * c] = u[r * us + c]; buf[r * 2 * W + 2 * c + 1] = v[r * vs + c]; }
    if (e->bps == 2) {   // Main 4:4:4 10 from 8-bit planes: widened like enc_load_nv12 does
        load_widened(e, in->cpu, y, ys, W, H); load_widened(e, (uint8_t *)in->cpu + e->lumaSz, buf, 2 * W, 2 * W, H);
        free(buf); return;
    }
    nvenc_in_tile_plane(in->cpu, y, ys, W, H, e->pitch);
    nvenc_in_tile_plane((uint8_t *)in->cpu + e->lumaSz, buf, 2 * W, 2 * W, H, 2 * e->pitch);
    free(buf);
}
void enc_load_nv24(Enc *e, const uint8_t *y, size_t ys, const uint8_t *uv, size_t uvs)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    nvenc_in_tile_plane(in->cpu, y, ys, (size_t)e->cfg.width, (size_t)e->cfg.height, e->pitch);
    nvenc_in_tile_plane((uint8_t *)in->cpu + e->lumaSz, uv, uvs, 2 * (size_t)e->cfg.width, (size_t)e->cfg.height, 2 * e->pitch);
}

// x444 / xf44: 16-bit samples (10 bits high) in two planes, the chroma plane Cb Cr interleaved: HEVC Main 4:4:4 10
void enc_load_x444(Enc *e, const uint16_t *y, size_t ys, const uint16_t *uv, size_t uvs)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    load_16(e, in->cpu, y, ys, (size_t)e->cfg.width, (size_t)e->cfg.height);
    nvenc_in_tile_plane((uint8_t *)in->cpu + e->lumaSz, (const uint8_t *)uv, uvs, 4 * (size_t)e->cfg.width, (size_t)e->cfg.height, 4 * e->pitch);
}

void enc_load_l10r(Enc *e, const uint8_t *src, size_t stride)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    nvenc_in_tile_l10r(in->cpu, (uint8_t *)in->cpu + e->lumaSz, src, stride, (size_t)e->cfg.width, (size_t)e->cfg.height, e->pitch * 2);
}

void enc_load_bgra(Enc *e, const uint8_t *bgra, size_t stride)
{
    RmMem *in = &e->in[e->seq % (NvU32)e->cfg.depth];
    nvenc_in_tile_bgra(in->cpu, (uint8_t *)in->cpu + e->lumaSz, bgra, stride, (size_t)e->cfg.width, (size_t)e->cfg.height, e->pitch);
}

// ME control: predictor sources, search stamps and FBM settings as the reference client sets them (docs/traces,
// T3.02); an IDR only uses the spatial and constant hints.
static void setup_me(nvenc_h264_me_control_s *me, bool p)
{
    me->refinement_mode = 1; me->refine_on_search_enable = 1;
    me->fps_mvcost = 1; me->sps_mvcost = 1; me->sps_cost_func = 1; me->partDecisionMadeByFPP = 1;
    nvenc_h264_me_hint_cfg_s *h = &me->predsrc;
    h->self_spatial_search = 1; h->self_spatial_enable = 1; h->const_mv_search = 1; h->const_mv_enable = 1;
    nvenc_h264_me_stamp_s *sh = &me->shape0;
    if (!p) { sh[0].bitmask[0] = 0x1c1c0000; sh[0].bitmask[1] = 0x1c; return; }
    h->self_temporal_stamp_l0 = 4; h->self_temporal_stamp_l1 = 5; h->self_temporal_search = 1; h->self_temporal_enable = 1;
    h->coloc_stamp_l0 = 1; h->coloc_stamp_l1 = 1;
    h->self_spatial_stamp_l0 = 2; h->self_spatial_stamp_l1 = 3; h->const_mv_stamp_l1 = 1;
    h->stamp_refidx1_stamp = 6; h->stamp_refidx2_stamp = 6; h->stamp_refidx3_stamp = 6;
    for (int i = 0; i < 7; i++) { sh[i].bitmask[0] = 0x1e0c0000; sh[i].bitmask[1] = 0x0c; sh[i].hor_adjust = 1; }
    me->spatial_hint_pattern = 6; me->temporal_hint_pattern = 4; me->Cu16partDecisionMadeByFPP = 1;
    me->fbm_op_winner_num_p_frame = 1; me->fbm_op_winner_num_b_frame_l0 = 1; me->fbm_op_winner_num_b_frame_l1 = 1;
    me->fbm_select_best_cu32_parttype_num = 3; me->sps_evaluate_merge_cand_num = 5; me->fps_quad_thresh_hold = 50;
    me->external_hint_order = 1; me->coloc_hint_order = 1;
    me->hint_type0 = 0; me->hint_type1 = 1; me->hint_type2 = 2; me->hint_type3 = 3; me->hint_type4 = 4;   // must cover 0x1f
}

// MD control: mode decision as the reference client sets it. intra_ssd_cnt_* must be >= 1 or the engine hangs (T2.05e).
static void setup_md(nvenc_h264_md_control_s *md, bool p, bool high)
{
    if (high) { md->intra_luma8x8_mode_enable = 0x1ff; md->priority_ipred_type_ip1 = 3; }   // High: all nine 8x8 intra modes (T7.02)
    md->intra_luma16x16_mode_enable = 0xf; md->intra_chroma_mode_enable = 0xf; md->inter_penalty_factor_for_ip1 = 16;
    md->l0_part_16x16_enable = 1; md->l0_part_16x8_enable = 1; md->l0_part_8x16_enable = 1; md->l0_part_8x8_enable = 1;
    md->pskip_enable = 1;
    md->bias_inter_16x8 = -1; md->bias_inter_8x16 = -1; md->bias_inter_8x8 = -1; md->bias_pskip = -1; md->bias_bdir = 2;
    md->bias_intra_over_inter = -2; md->bias_intra_16x16 = -4; md->bias_intra_8x8 = -2; md->bias_intra_4x4 = -1;
    md->intra_nxn_bias_multiplier = 32; md->intra_most_prob_bias_multiplier = 3; md->pskip_bias_multiplier = 2;
    md->pskip_esc_threshold = 9; md->bdirect_esc_threshold = 19;
    md->tempmv_wt_spread_threshold = 0xffff; md->tempmv_wt_distort_threshold = 0xffff;
    md->mv_cost_enable = 1; md->early_intra_mode_type_16x16dc = 1; md->early_intra_mode_type_16x16h = 1; md->early_intra_mode_type_16x16v = 1;
    md->ip_search_mode = 7; md->multiply_bias_with_lambda = 1; md->early_termination_ip1 = 1; if (!high) md->priority_ipred_type_ip1 = 1;
    md->intra_ssd_cnt_4x4 = 1; md->intra_ssd_cnt_8x8 = 1; md->intra_ssd_cnt_16x16 = 1; md->skip_evaluate_enable = 1;
    md->rdo_level = 1; md->tu_search_num = 3; md->num_1div8_lambda_ssd = 1;
    md->luma_residual_zero_eval = p ? 2 : 0;
}

// HEVC controls: the values nullmoth's libnvenc_hevc uses (docs/traces/vttest-hevc-256x128-controls.txt, T6.03/T6.05);
// IDR and P pictures use the same ME, MD and quant controls.
static void setup_me_hevc(nvenc_h264_me_control_s *me)
{
    me->refinement_mode = 1; me->refine_on_search_enable = 1; me->fps_mvcost = 1; me->sps_mvcost = 1; me->sps_cost_func = 1;
    nvenc_h264_me_hint_cfg_s *h = &me->predsrc;
    h->self_temporal_stamp_l0 = 1; h->self_temporal_stamp_l1 = 1; h->self_temporal_search = 1; h->self_temporal_enable = 1;
    h->self_spatial_search = 1; h->self_spatial_enable = 1;
    h->const_mv_stamp_l0 = 2; h->const_mv_stamp_l1 = 2; h->const_mv_search = 1; h->const_mv_enable = 1;
    h->stamp_refidx1_stamp = 3; h->stamp_refidx2_stamp = 3; h->stamp_refidx3_stamp = 3;
    nvenc_h264_me_stamp_s *sh = &me->shape0;
    sh[0].bitmask[0] = 0xff0c0c00; sh[0].bitmask[1] = 0xc0c; sh[0].hor_adjust = 1;
    sh[1].bitmask[0] = 0x1e1e0c00; sh[1].bitmask[1] = 0xc; sh[1].hor_adjust = 1; sh[1].ver_adjust = 1;
    sh[2].bitmask[0] = 0xff0c0c00; sh[2].bitmask[1] = 0xc0c; sh[2].hor_adjust = 1;
    sh[3].bitmask[0] = 0x1e0c0000; sh[3].bitmask[1] = 0xc; sh[3].hor_adjust = 1;
    me->partDecisionMadeByFPP = 1; me->penalty_factor_FPP = 15; me->spatial_hint_pattern = 6; me->temporal_hint_pattern = 4;
    me->Cu16partDecisionMadeByFPP = 1; me->fbm_op_winner_num_p_frame = 1; me->fbm_op_winner_num_b_frame_l0 = 1;
    me->fbm_op_winner_num_b_frame_l1 = 1; me->fbm_select_best_cu32_parttype_num = 3; me->sps_evaluate_merge_cand_num = 3;
    me->fps_quad_thresh_hold = 50; me->external_hint_order = 1; me->coloc_hint_order = 1;
    me->hint_type0 = 0; me->hint_type1 = 1; me->hint_type2 = 2; me->hint_type3 = 3; me->hint_type4 = 4;
}

static void setup_md_hevc(nvenc_h265_md_control_s *md)
{
    md->h265_intra_luma4x4_mode_enable = md->h265_intra_luma8x8_mode_enable = 0x55555557;
    md->h265_intra_luma16x16_mode_enable = md->h265_intra_luma32x32_mode_enable = 0x55555557;
    md->h265_intra_luma_mode_enable_leftbits = 0xb6d;
    md->cu16x16_part_2Nx2N_enable = 1; md->cu16x16_part_2NxN_enable = 1; md->cu16x16_part_Nx2N_enable = 1; md->cu16x16_part_NxN_enable = 1;
    md->cu32x32_part_2Nx2N_enable = 1; md->cu32x32_part_2NxN_enable = 1; md->cu32x32_part_Nx2N_enable = 1;
    md->cu32x32_part_2NxnU_enable = 1; md->cu32x32_part_2NxnD_enable = 1; md->cu32x32_part_nLx2N_enable = 1; md->cu32x32_part_nRx2N_enable = 1;
    md->intra_mode_4x4_enable = 1; md->intra_mode_8x8_enable = 1; md->intra_mode_16x16_enable = 1; md->intra_mode_32x32_enable = 1;
    md->h265_intra_chroma_mode_enable = 0x1f;
    md->tusize_4x4_enable = 1; md->tusize_8x8_enable = 1; md->tusize_16x16_enable = 1; md->tusize_32x32_enable = 1;
    md->pskip_enable = 1; md->mv_cost_enable = 1; md->ip_search_mode = 7; md->tu_search_num_cu16 = 2; md->tu_search_num_cu32 = 1;
    md->intra_ssd_cnt_4x4 = 1; md->intra_ssd_cnt_8x8 = 1; md->intra_ssd_cnt_16x16 = 1; md->intra_ssd_cnt_32x32 = 1;
    const char *x = getenv("ENC_HEVC_MD");   // experiments: name=value,... (rdo, skip, ssd, mvp, tu16, tu32, allmodes, lambda)
    for (; x && *x; x = strchr(x, ',') ? strchr(x, ',') + 1 : NULL) {
        char k[16] = ""; int v = 0;
        if (sscanf(x, "%15[a-z0-9]=%d", k, &v) != 2) break;
        if (!strcmp(k, "rdo")) md->rdo_level = v;
        else if (!strcmp(k, "skip")) md->skip_evaluate_enable_cu8 = md->skip_evaluate_enable_cu16 = md->skip_evaluate_enable_cu32 = v;
        else if (!strcmp(k, "ssd")) md->intra_ssd_cnt_4x4 = md->intra_ssd_cnt_8x8 = md->intra_ssd_cnt_16x16 = md->intra_ssd_cnt_32x32 = v;
        else if (!strcmp(k, "mvp")) md->mv_cost_predictor_control = v;
        else if (!strcmp(k, "tu16")) md->tu_search_num_cu16 = v;
        else if (!strcmp(k, "tu32")) md->tu_search_num_cu32 = v;
        else if (!strcmp(k, "allmodes") && v) {
            md->h265_intra_luma4x4_mode_enable = md->h265_intra_luma8x8_mode_enable = 0xffffffffu;
            md->h265_intra_luma16x16_mode_enable = md->h265_intra_luma32x32_mode_enable = 0xffffffffu;
            md->h265_intra_luma_mode_enable_leftbits = 0x3fffffff;
        }
        else if (!strcmp(k, "lambda")) md->num_1div8_lambda_ssd = v;
    }
}

static void setup_q_hevc(nvenc_h265_quant_control_s *q)
{
    q->qpp_run_vector_4x4 = 0x56b; q->qpp_run_vector_8x8[0] = 0xaaff; q->qpp_run_vector_8x8[1] = 0xffaa; q->qpp_run_vector_8x8[2] = 0xf;
    q->qpp_luma8x8_cost = 15; q->qpp_luma16x16_cost = 15; q->qpp_chroma_cost = 15;
}

// the H.265 picture setup (nvenc_h265_drv_pic_setup_s, 1024 bytes) and its controls
static void setup_hevc(Enc *e, RmMem *setup, RmMem *bs, bool idr, int nRefs, bool b, unsigned poc)
{
    int W = e->cfg.width, H = e->cfg.height;
    nvenc_h265_drv_pic_setup_s *ps = setup->cpu;
    ps->magic = enc_magic(e);
    nvenc_h265_surface_cfg_s ic = { .frame_width_minus1 = (unsigned short)(W - 1), .frame_height_minus1 = (unsigned short)(H - 1),
        .sfc_pitch = (unsigned short)e->pitch, .sfc_pitch_chroma = (unsigned short)e->pitch, .block_height = 2 };
    // reference/output pictures: the coded (CTB-aligned) size; the input keeps the display size and the engine pads it
    HevcParams hp = hevc_params(e);
    int cw = (int)hevc_coded_width(&hp), ch = (int)hevc_coded_height(&hp);
    nvenc_h265_surface_cfg_s rc = ic; rc.sfc_pitch = rc.sfc_pitch_chroma = (unsigned short)e->rPitch;
    rc.frame_width_minus1 = (unsigned short)(cw - 1); rc.frame_height_minus1 = (unsigned short)(ch - 1);
    // nullmoth's sets the field offsets of the reference/output surfaces to half a plane (unused in frame mode)
    rc.luma_bot_offset = (unsigned)((size_t)ch / 2 * e->rPitch * e->bps / 256);
    rc.chroma_bot_offset = (unsigned)((((size_t)ch / 2 + 31) & ~(size_t)31) / 2 * e->rPitch * e->bps / 256);
    // 4:4:4 (T13.01): chroma Cb / Cr interleaved at full width (like NV24), input and reconstruction alike, the chroma
    // pitch twice the luma's. (Planar input via memory_mode 1 and the CHROMA_U / _V methods, or a planar reconstruction,
    // gave 8-18 dB chroma.)
    if (e->cfg.chroma444) { ic.sfc_pitch_chroma = (unsigned short)(2 * e->pitch); rc.sfc_pitch_chroma = (unsigned short)(2 * e->rPitch); }
    ps->input_cfg = ic; ps->refpic_cfg = rc; ps->outputpic_cfg = rc;
    nvenc_h265_sps_data_s *sp = &ps->sps_data;
    sp->chroma_format_idc = e->cfg.chroma444 ? 3 : 1; sp->log2_max_pic_order_cnt_lsb_minus4 = 4; sp->min_cu_size = e->cfg.quality && !e->cfg.no_cu8 && !hevc_off("cu8") ? 0 : 1; sp->max_cu_size = 2;
    sp->min_tu_size = 0; sp->max_tu_size = 3; sp->max_transform_depth_inter = 3; sp->max_transform_depth_intra = 3;
    sp->amp_enabled_flag = !hp.no_amp; sp->num_short_term_ref_pic_sets = 1; sp->sample_adaptive_offset_enabled_flag = !hp.no_sao;
    sp->strong_intra_smoothing_enabled_flag = hp.sis ? 1 : 0;
    sp->bit_depth_minus_8 = (unsigned)(e->cfg.bit_depth - 8);
    nvenc_h265_pps_data_s *pp = &ps->pps_data;   // matches hevc_write_pps()
    pp->num_ref_idx_l0_active_minus1 = (unsigned)(e->cfg.num_refs - 1); pp->transform_skip_enabled_flag = !hp.no_ts;
    pp->transquant_bypass_enabled_flag = e->cfg.lossless;
    pp->pps_loop_filter_across_slices_enabled_flag = 1; pp->cu_qp_delta_enabled_flag = !hp.no_cqd; pp->cabac_init_present_flag = 1;
    pp->sign_data_hiding_enabled_flag = !hp.no_sdh;
    if (e->cfg.bit_depth > 8) {
        // nullmoth's 10-bit setup also sets an undocumented bit (pps_data reserved1, byte 7 bit 3) and the reserved word at
        // 0x314 to 0x0222 (0x0999 in 8-bit, which we leave 0); meanings unknown (T6.14)
        ((NvU8 *)pp)[7] |= 0x08;
        ((NvU8 *)ps)[0x314] = 0x22; ((NvU8 *)ps)[0x315] = 0x02;
    }
    nvenc_h265_rc_s *r = &ps->rate_control;
    { const int slot = (int)(e->seq % (NvU32)e->cfg.depth);   // spatial AQ (aq_compute)
      if (e->cfg.aq && e->aqValid[slot]) { r->aqMode = 3; ps->pic_control.act_stat_offset = 0x10000 + (NvU32)(slot * e->aqStride); } }
    const int qp_b = e->cfg.qp_p + 2 > 51 ? 51 : e->cfg.qp_p + 2;   // B pictures (not references) 2 above P
    r->QP[0] = (unsigned char)e->cfg.qp_p; r->QP[1] = (unsigned char)qp_b; r->QP[2] = (unsigned char)e->cfg.qp_i;
    // 10-bit extends the QP range down by 6 * (bit_depth - 8): nullmoth's sets minQP -12
    for (int i = 0; i < 3; i++) { r->minQP[i] = (unsigned char)(e->cfg.bit_depth > 8 ? -12 : 0); r->maxQP[i] = 51; r->rhopbi[i] = 256; }
    r->framerate = e->cfg.fps; r->gop_length = e->cfg.idr_period > 0 ? (unsigned)e->cfg.idr_period : 0xffffffffu;
    if (e->cfg.rc_mode) {   // as the H.264 path (M3, T3.06-T3.08): NAL HRD, fixed-point rate per pixel and frame
        double pixels = (double)W * H, perFrame = (double)e->cfg.bitrate / e->cfg.fps;
        r->hrd_type = 2; r->maxQPD = 6; r->baseQPD = 3;
        r->framerate = e->cfg.fps << 8;
        r->buffersize = (unsigned)e->cfg.vbv_bits; r->nal_cpb_size = r->vcl_cpb_size = e->cfg.vbv_bits;
        r->nal_bitrate = r->vcl_bitrate = e->cfg.max_bitrate;
        r->Np = e->cfg.idr_period > 0 ? (e->cfg.idr_period - 1) << 4 : 0x7fffffff;
        r->Ravg = r->R = (int)(perFrame / pixels * 256);
        // VBR: R = the maximum rate per frame (as NVIDIA's driver sets AV1's, T12.03); with R = the average, a 15 / 30 Mbit/s
        // VBR came out at 18 Mbit/s on crowd_run, with the maximum 15.4 (T7.05)
        if (e->cfg.rc_mode == 1 || e->cfg.rc_mode == 17) r->R = (int)((double)e->cfg.max_bitrate / e->cfg.fps / pixels * 256);
        r->Bmin = 0; r->i_size_ratio_x = 1; r->i_size_ratio_y = 1;   // single_frame_vbv stays 0 (T3.08)
        // NVIDIA's CBR fields (docs/traces/quality-linux vcbr, T7.08): HRD type 34, P / B / I weights, Np = the GOP's anchors,
        // a minimum buffer level; with 2 B pictures +0.5-1.1 VMAF on crowd_run / park_joy at 6 Mbit/s, P only the same
        if ((e->cfg.rc_mode & 3) == 2) {   // CBR only (with Ravg 0 VBR ran at its maximum rate: T7.09)
            r->hrd_type = 34; r->rhopbi[0] = 286; r->rhopbi[1] = 317; r->rhopbi[2] = 256;
            r->Np = rc_np(e); r->Bmin = (int)(r->buffersize / 81920); r->Ravg = 0;
        }
    }

    int ctbW = (W + 31) / 32, ctbH = (H + 31) / 32, ctbs = ctbW * ctbH, lg = 0;
    while ((1 << lg) < ctbs) lg++;
    nvenc_h265_pic_control_s *pc = &ps->pic_control;
    memset(pc->l0, -2, sizeof pc->l0); memset(pc->l1, -2, sizeof pc->l1);
    memset(pc->poc_entry_l0, -1, sizeof pc->poc_entry_l0); memset(pc->poc_entry_l1, -1, sizeof pc->poc_entry_l1);
    pc->pic_output_flag = 1;
    pc->slice_control_offset = OFF_SLICE; pc->me_control_offset = OFF_ME; pc->md_control_offset = OFF_MD; pc->q_control_offset = OFF_Q;
    pc->hist_buf_size = (unsigned)e->hist.size; pc->bitstream_buf_size = (unsigned)bs->size;
    pc->pic_type = idr ? 3 : b ? 1 : 0; pc->ref_pic_flag = !b; pc->slice_mode = 0;
    pc->codec = 5; pc->ceil_log2_pic_size_in_ctbs = (unsigned)lg; pc->sao_luma_mode = hp.no_sao ? 0 : 31; pc->sao_chroma_mode = hp.no_sao ? 0 : 31;
    pc->nal_unit_type = idr ? 19 : b ? 0 : 1; pc->nuh_temporal_id_plus1 = 1; pc->e4byteStartCode = 1;   // B: TRAIL_N
    pc->qpfifo = 1; pc->enable_source_image_padding = 1; pc->strips_in_frame = 1; pc->chroma_skip_threshold_4x4 = 8;
    pc->pic_order_cnt_lsb = (unsigned short)(poc & 0xff);
    if (!idr) {   // inline short-term RPS (T6.07, T6.11; B pictures as NVIDIA's driver, docs/traces/bframes-linux)
        // the reference entries (SET_IN_REF_PIC<k>, poc_entry): list 0 first (earlier pictures, nearest first), then list 1
        // (a B picture: the newest anchor, after it in display order); P: list 0 = the DPB, B: list 0 = slotOf[1..]
        const int nL0 = b ? nRefs - 1 : nRefs, nL1 = b ? 1 : 0, nE = nL0 + nL1;
        int ent[ENC_MAX_REFS + 1], td[ENC_MAX_REFS + 1];
        for (int k = 0; k < nL0; k++) ent[k] = e->slotOf[b ? k + 1 : k];
        if (b) ent[nL0] = e->slotOf[0];
        for (int k = 0; k < nE; k++) td[k] = (int)poc - (int)e->slotPoc[ent[k]] / 2;   // slotPoc holds 2 x display index
        nvenc_h265_strps_s *rps = &pc->short_term_rps;
        rps->num_negative_pics = (short)nL0; rps->num_positive_pics = (short)nL1;
        memset(pc->temp_dist_l0, -1, sizeof pc->temp_dist_l0); memset(pc->temp_dist_l1, -1, sizeof pc->temp_dist_l1);
        for (int r = 0; r < 8; r++) for (int c2 = 0; c2 < 8; c2++) pc->dist_scale_factor[r][c2] = -1;   // unused -1
        int prev = 0;
        for (int i = 0; i < nL0; i++) {   // l0: 2 x DPB slot; delta_poc_s0: distance to the previous entry, minus 1
            pc->l0[i] = (char)(2 * ent[i]); pc->poc_entry_l0[i] = (char)i; pc->temp_dist_l0[i] = (char)td[i];
            rps->delta_poc_s0_minus1[i] = (short)(td[i] - prev - 1); prev = td[i];
            rps->used_by_curr_pic_s0_flag |= (unsigned short)(1u << i);
        }
        if (b) {
            pc->l1[0] = (char)(2 * ent[nL0]); pc->poc_entry_l1[0] = (char)nL0; pc->temp_dist_l1[0] = (char)td[nL0];
            rps->delta_poc_s1_minus1[0] = (short)(-td[nL0] - 1); rps->used_by_curr_pic_s1_flag = 1;
        }
        // MV scaling between references (8.5.3.2.7): distScaleFactor = (tb * tx + 32) >> 6, tx = (16384 + |td| / 2) / td,
        // stored [td entry][tb entry]; 256 = 1.0
        for (int i = 0; i < nE; i++) for (int j = 0; j < nE; j++) {
            const int tb = td[i], tdd = td[j], tx = (16384 + (tdd < 0 ? -tdd : tdd) / 2) / tdd, f = (tb * tx + 32) >> 6;
            pc->dist_scale_factor[j][i] = (short)(f < -4096 ? -4096 : f > 4095 ? 4095 : f);
        }
        // same_poc: an 8x8 bit matrix over pairs of entries, bit i * 8 + j set when entries i and j are the same picture
        // (the diagonal: 0x201 for two). With only bit 0 the engine scaled MVs between a reference and itself (T6.11)
        for (int i = 0; i < nE; i++) pc->same_poc[(i * 9) / 32] |= 1u << ((i * 9) % 32);
        // diff_pic_order_cnt_zero, by byte j: 0 below the list 1 count, else 0xff shifted by the list 0 count (NVIDIA's values)
        for (int j = 0; j < 8; j++) pc->diff_pic_order_cnt_zero[j / 4] |= (NvU32)(j < nL1 ? 0 : (0xffu << nL0) & 0xff) << (8 * (j % 4));
    }
    nvenc_h265_slice_control_s *sc = (void *)((NvU8 *)setup->cpu + OFF_SLICE);
    pc->num_forced_slices_minus1 = 0;
    // slice_type numbered like pic_type (0 P, 1 B, 2 I), not as the header's comment says (0 B, 1 P): with 1 the engine
    // faulted on the first P picture; nullmoth's library sends 0 (T6.07)
    sc->num_ctus = (unsigned)ctbs; sc->qp_avr = (unsigned)(idr ? e->cfg.qp_i : b ? qp_b : e->cfg.qp_p); sc->slice_type = idr ? 2 : b ? 1 : 0;
    sc->slice_loop_filter_across_slices_enabled_flag = 1; sc->slice_sao_luma_flag = !hp.no_sao; sc->slice_sao_chroma_flag = !hp.no_sao;
    if (!idr) { sc->num_ref_idx_active_override_flag = 1; sc->num_ref_idx_l0_active_minus1 = (unsigned)(b ? nRefs - 2 : nRefs - 1); sc->num_ref_idx_l1_active_minus1 = 0; }
    setup_me_hevc((void *)((NvU8 *)setup->cpu + OFF_ME));
    if (b) ((nvenc_h264_me_control_s *)((NvU8 *)setup->cpu + OFF_ME))->Cu16partDecisionMadeByFPP = 1;   // NVIDIA's one B difference
    setup_md_hevc((void *)((NvU8 *)setup->cpu + OFF_MD));
    setup_q_hevc((void *)((NvU8 *)setup->cpu + OFF_Q));
    if (e->cfg.quality) {   // NVIDIA's -preset p7 -tune hq controls (T7.08; with 8x8 coding blocks); the MV search range by size
        NvU8 *base = setup->cpu;
        memcpy(base + OFF_ME, idr ? hevc_p7_me_idr : b ? hevc_p7_me_b : hevc_p7_me_p, sizeof hevc_p7_me_p);
        memcpy(base + OFF_MD, idr ? hevc_p7_md_idr : b ? hevc_p7_md_b : hevc_p7_md_p, sizeof hevc_p7_md_p);
        memcpy(base + OFF_Q, hevc_p7_q, sizeof hevc_p7_q);
        nvenc_h264_me_control_s *me = (void *)(base + OFF_ME);
        const bool big = e->cfg.width > 1920 || e->cfg.height > 1088;   // the driver's ranges at 1080p and 4K
        me->limit_mv.left_mvx_int = big ? -2048 : -1024; me->limit_mv.right_mvx_int = big ? 2047 : 1023;
        me->limit_mv.top_mvy_int = big ? -512 : -256; me->limit_mv.bottom_mvy_int = big ? 511 : 255;
    }
}

// ---- AV1 (M12): NVIDIA's Linux driver's picture setups (src/av1enctmpl.h, traced at 256x128), patched in the fields that
// change with the picture size and the frame (found by comparing traces at 256x128, 640x360 and 1920x1080 and across
// frames). Ada reads nvenc_av1_drv_pic_setup_s with a fourth 32-byte surface config at 0x64 (a copy of the reference
// picture's), so the offsets below are the header's plus 0x20 from there on. References: the previous frames, newest
// first (one in the driver's low-latency mode, up to four in p7 / hq; T12.07); CDEF and loop restoration on (the engine picks CDEF per block from two strengths
// and leaves restoration off; with av1Hq from four, and self-guided restoration on luma); constant quantizer per frame type or the driver's RC.
static void put32(void *b, size_t off, NvU32 v) { memcpy((NvU8 *)b + off, &v, 4); }
static void put16(void *b, size_t off, NvU16 v) { memcpy((NvU8 *)b + off, &v, 2); }
// loop filter level from the base quantizer index: the driver's choices were 7 at 80 (key) and 13 at 100 (P); libaom's
// "pick from q" rule rounded the same way (av1_pick_filter_level, 8-bit), minus 4 on key frames
static int av1_lf_level(int q, bool key)
{
    static const int ac[] = { 4, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22 };   // Ac_Qlookup below 16
    const int acq = q < 16 ? ac[q] : (q * 1191 + 1250) / 1000;   // close to Ac_Qlookup[q] in the low range (8-bit)
    int l = (acq * 20723 + 1015158 + (1 << 17)) >> 18;
    if (key) l -= 4;
    return l < 0 ? 0 : l > 63 ? 63 : l;
}

// B pictures' quantizer index: the driver's 126 for P 100 (ab2s), not references
static int av1_qp_b(const Enc *e) { return e->cfg.qp_p + 26 > 255 ? 255 : e->cfg.qp_p + 26; }

static void setup_av1(Enc *e, RmMem *setup, RmMem *bs, bool idr, bool isB, int q, int lf, int nRefs, unsigned poc)
{
    NvU8 *b = setup->cpu;
    const int W = e->cfg.width, H = e->cfg.height;
    const size_t rows = ((size_t)H + 63) & ~(size_t)63;
    memcpy(b, idr ? av1_tmpl_key : av1_tmpl_p, sizeof av1_tmpl_key);
    memcpy(b + OFF_AV1_TILE, idr ? av1_tmpl_key_tile : av1_tmpl_p_tile, sizeof av1_tmpl_key_tile);
    // surface configs: reference, input, output, and the extra one (dims, pitch, plane offsets in units of 256: luma
    // "bottom" = half a plane, chroma after the luma plane, chroma "bottom" half a chroma plane on)
    // pitches in samples, plane offsets in bytes (10-bit: 16-bit samples, offsets double; as HEVC Main 10)
    const NvU32 dims = (NvU32)(W - 1) | (NvU32)(H - 1) << 16, rp = (NvU32)e->rPitch, ip = (NvU32)e->pitch, rpb = rp * (NvU32)e->bps;
    for (int k = 0; k < 4; k++) {
        const size_t o = 4 + 0x20 * (size_t)k; const bool in = k == 1;
        put32(b, o, dims); put32(b, o + 4, in ? ip | ip << 16 : rp | rp << 16);
        if (in) { put32(b, o + 0x14, (NvU32)(e->lumaSz >> 8)); continue; }
        put32(b, o + 0x10, (NvU32)(rows / 2 * rpb >> 8)); put32(b, o + 0x14, (NvU32)(e->rLuma >> 8));
        put32(b, o + 0x18, (NvU32)((e->rLuma + rows / 4 * rpb) >> 8));
    }
    if (e->cfg.bit_depth > 8) put32(b, 0x84, *(NvU32 *)(b + 0x84) | 2u << 7 | 2u << 11);   // seqhdr: bit depth, input bit depth - 8
    for (int i = 0; i < 4; i++) b[0x98 + i] = (NvU8)lf;            // frame header: loop filter levels
    if (e->av1Hq) {   // the default preset (B mode / quality; docs/traces/bframes-linux ab2s): its MD / ME controls (the same for key
        // frames and anchors), CDEF with four luma strengths and luma self-guided loop restoration (frame header in av1_assemble)
        memcpy(b + 0x1b8, isB ? av1_tmpl_bf_b_mdme : av1_tmpl_bf_p_mdme, sizeof av1_tmpl_bf_b_mdme);
        memcpy(b + 0xa8, av1_tmpl_hq_fh, 0x18); memcpy(b + 0xc4, av1_tmpl_hq_fh + 0x1c, 4);   // not 0xc0..0xc3 (base_q_idx, set above)
    }
    if (!idr) {   // references (docs/traces/bframes-linux ahq, NVIDIA's -preset p7 -tune hq: up to 4, newest first): list 0
        // entry i = reference type LAST + i = REF_PIC<i>, temporal distance in half frames, position = the type; unused
        // entries -1; compound prediction from one direction (unicompound) once there are two. B pictures (ab2s): list 0 =
        // the anchors before (slotOf[1..]), list 1 = ALTREF (type 6) = the newest anchor, after it (negative distance),
        // compound from both directions, ALTREF's sign bias 1, picture type B, not a reference
        const int nL0 = isB ? nRefs - 1 : nRefs, *L0 = isB ? e->slotOf + 1 : e->slotOf;
        for (int i = 0; i < 4; i++) {
            b[0x128 + i] = (NvU8)(i < nL0 ? i : 0xff); b[0x136 + i] = (NvU8)(i < nL0 ? i : 0xff);
            b[0x12f + i] = (NvU8)(i < nL0 ? poc - e->slotPoc[L0[i]] : 0xff);
        }
        b[0x144] = (NvU8)((b[0x144] & ~0xe0) | nL0 << 5);           // num_ref_idx_l0_active
        if (nL0 >= 2) b[0x8d] |= 4;                                 // unicompound_enable
        if (isB) {
            b[0x12c] = 6; b[0x13a] = 6; b[0x133] = (NvU8)(poc - e->slotPoc[e->slotOf[0]]);   // l1, poc_entry_l1, temp_dist_l1
            b[0x145] = (NvU8)((b[0x145] & ~7) | 1);                 // num_ref_idx_l1_active
            b[0x8d] |= 2; b[0x97] = 1;                              // bicompound_enable, ref_frame_sign_bias[ALTREF]
            b[0x160] = (NvU8)((b[0x160] & ~7) | 1);                 // pic_type B, ref_pic_flag 0
        }
    }
    b[0xc0] = (NvU8)q;                                              // frame header: base_q_idx
    b[0xc9] = (NvU8)e->cfg.qp_p; b[0xca] = (NvU8)av1_qp_b(e); b[0xcb] = (NvU8)e->cfg.qp_i;   // RC: QP[P, B, I]
    put32(b, 0x154, (NvU32)e->hist.size); put32(b, 0x158, (NvU32)bs->size); put32(b, 0x15c, 0);   // pic_control buffer sizes
    put32(b, 0x16c, 0x1000); put32(b, 0x18c, 0x1000);              // mpec / stats fifo offsets in the status (not enabled)
    put32(b, 0x178, 0x100 + (NvU32)av1_rc_area(e)); put32(b, 0x174, 0x100 + 2 * (NvU32)av1_rc_area(e));   // act / pass-2 in RC data
    put32(b, 0x180, 0x100u | (NvU32)e->av1SbCols << 16); put32(b, 0x184, (NvU32)e->av1SbRows);   // one strip: the whole frame
    put32(b, 0x31c, 0x80000000u | (NvU32)((size_t)e->wmb * e->hmb * 56 > 202500 ? (size_t)e->wmb * e->hmb * 56 : 202500));   // gpTimer
    // tiles: one strip (the whole frame, one engine) with the uniform grid in raster order; one tile control each (corners
    // inclusive, then the next tile's corners; the last names itself). The CDFs carried on come from tile 0. NVIDIA's driver
    // splits frames above 4K into two strips, one per NVENC engine, with filters off at the strip edge (docs/traces/
    // bframes-linux a8k); one strip keeps the loop filters across every tile edge, as the decoder applies them
    const int nt = e->av1TileCols * e->av1TileRows;
    put32(b, 0x144, (*(NvU32 *)(b + 0x144) & ~0x07fff800u) | (NvU32)(nt - 1) << 19);   // tile_idx_start 0, num_strip_tiles_minus1
    put32(b, 0x148, (*(NvU32 *)(b + 0x148) & ~0xff0u) | (NvU32)(nt - 1) << 4);         // num_tiles_minus1
    put32(b, 0x160, (*(NvU32 *)(b + 0x160) & ~0x7fffff80u) | (NvU32)e->av1TileCols << 7 | (NvU32)e->av1TileRows << 15);   // ctx tile 0
    for (int i = 0; i < nt; i++) {
        NvU8 *t = b + OFF_AV1_TILE + 0x20 * (size_t)i;
        const int j = i + 1 < nt ? i + 1 : i;
        if (i) memcpy(t, b + OFF_AV1_TILE, 0x20);
        int c[2][4];
        for (int k = 0; k < 2; k++) {
            const int x = (k ? j : i) % e->av1TileCols, y = (k ? j : i) / e->av1TileCols;
            c[k][0] = x * e->av1TileW; c[k][1] = y * e->av1TileH;
            c[k][2] = (x + 1) * e->av1TileW < e->av1SbCols ? (x + 1) * e->av1TileW - 1 : e->av1SbCols - 1;
            c[k][3] = (y + 1) * e->av1TileH < e->av1SbRows ? (y + 1) * e->av1TileH - 1 : e->av1SbRows - 1;
        }
        for (int k = 0; k < 4; k++) { put16(t, 2 * (size_t)k, (NvU16)c[0][k]); put16(t, 0x10 + 2 * (size_t)k, (NvU16)c[1][k]); }
        t[0xa] = (NvU8)q;                                           // qp_avr
        if (i) t[0xb] &= (NvU8)~0x08;                               // tile_end_update_cdf: tile 0 only
        if (isB) t[0xb] = (NvU8)((t[0xb] & ~3) | 1);                // tile_type B
        t[0xc] = (NvU8)i;                                           // tile_idx
    }
    if (e->cfg.rc_mode) {   // rate control as the driver sets it up (docs/traces/av1-linux cbr / vbr; M12, T12.03)
        put32(b, 0x88, *(NvU32 *)(b + 0x88) | 0x2000);               // frame header: delta_q_present (block-level deltas)
        nvenc_av1_rc_s r; memset(&r, 0, sizeof r);
        const bool vbr = e->cfg.rc_mode == 1 || e->cfg.rc_mode == 17;
        const double pixels = (double)W * H, perFrame = (double)e->cfg.bitrate / e->cfg.fps;
        r.hrd_type = 2;
        r.QP[0] = (unsigned char)e->cfg.qp_p; r.QP[1] = (unsigned char)(e->cfg.qp_p + 16 > 255 ? 255 : e->cfg.qp_p + 16); r.QP[2] = (unsigned char)e->cfg.qp_i;
        for (int i = 0; i < 3; i++) { r.minQP[i] = 0; r.maxQP[i] = 255; }
        r.maxQPD = 32; r.baseQPD = 16; r.rhopbi[0] = vbr ? 316 : 294; r.rhopbi[1] = vbr ? 414 : 330; r.rhopbi[2] = 256;
        r.framerate = (unsigned)e->cfg.fps << 8;
        r.buffersize = r.nal_cpb_size = r.vcl_cpb_size = (unsigned)e->cfg.vbv_bits;
        r.nal_bitrate = r.vcl_bitrate = (unsigned)(vbr ? e->cfg.max_bitrate : e->cfg.bitrate);
        r.gop_length = e->cfg.idr_period > 0 ? (unsigned)e->cfg.idr_period : 0xffffffffu;
        r.Np = e->cfg.idr_period > 0 ? (unsigned)e->cfg.idr_period - 1 : 0;   // as the driver (the anchor formula: -0.3 VMAF, T7.09)
        r.Ravg = (unsigned)(perFrame / pixels * 256); r.R = (unsigned)((double)r.nal_bitrate / e->cfg.fps / pixels * 256);
        r.Bmin = vbr ? 0 : r.buffersize / 81920;                     // the driver's CBR value (122 for a 10 Mbit buffer)
        r.i_size_ratio_x = 1; r.i_size_ratio_y = 1;
        memcpy(b + 0xc8, &r, sizeof r);
    }
}

// AV1 picture setup and methods, in the driver's order (docs/traces/av1-linux): codec type 5 with force-output, tile
// statistics, the app timer and sub-frame mode 2; no colocated data; luma and chroma by one address each (the surface
// configs give the chroma planes); on P frames the ME predictors and reference picture 0. Probability tables: key frames
// read the default tables (by quantizer), P frames the tables the previous frame wrote.
static void submit_av1(Enc *e, int ds, bool idr, bool isB, int slot, int nRefs, unsigned poc)
{
    Chan *c = &e->chan;
    RmMem *setup = &e->setup[ds], *in = &e->in[ds], *bs = &e->bs[ds], *status = &e->status[ds];
    const int q = idr ? e->cfg.qp_i : isB ? av1_qp_b(e) : e->cfg.qp_p, lf = av1_lf_level(q, idr);
    setup_av1(e, setup, bs, idr, isB, q, lf, nRefs, poc);
    const NvU64 recOut = e->recon.gpu + (NvU64)slot * e->rSlot, inVa = e->extInput ? e->extInput : in->gpu;
    // CDFs: a key frame reads the default tables, an inter frame those its primary reference saved (LAST for anchors,
    // ALTREF for B pictures: slotOf[0] either way); each frame saves its own in its slot's table (B pictures: scratch)
    const NvU64 probIn = idr ? e->av1ProbIn.gpu : e->av1Prob.gpu + (NvU64)e->slotOf[0] * 0x10000;
    const NvU64 probOut = e->av1Prob.gpu + (NvU64)(isB ? e->nSlots : slot) * 0x10000;
    RmMem *meIn = &e->mepred[e->seq & 1], *meOut = &e->mepred[(e->seq + 1) & 1];
    chan_mthd(c, 0, NVC9B7_SET_APPLICATION_ID, 1, (NvU32)NVC9B7_SET_APPLICATION_ID_ID_NVENC_AV1);
    // rate control: the driver uses the MB-row modes (17 VBR, 18 CBR)
    const NvU32 rcm = e->cfg.rc_mode == 1 ? 17u : e->cfg.rc_mode == 2 ? 18u : (NvU32)e->cfg.rc_mode;
    chan_mthd(c, 0, NVC9B7_SET_CONTROL_PARAMS, 1, 0x00041905u | DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _RCMODE, rcm));
    chan_mthd(c, 0, NVC9B7_SET_PICTURE_INDEX, 1, e->seq);
    chan_mthd(c, 0, NVC9B7_SET_IN_DRV_PIC_SETUP, 1, (NvU32)(setup->gpu >> 8));
    chan_mthd(c, 0, NVC9B7_SET_IOHISTORY, 1, (NvU32)(e->hist.gpu >> 8));
    if (!idr) { chan_mthd(c, 0, NVC9B7_SET_IN_MEPRED_DATA, 1, (NvU32)(meIn->gpu >> 8)); chan_mthd(c, 0, NVC9B7_SET_OUT_MEPRED_DATA, 1, (NvU32)(meOut->gpu >> 8)); }
    chan_mthd(c, 0, NVC9B7_SET_IO_RC_PROCESS, 1, (NvU32)(e->rcproc.gpu >> 8));
    chan_mthd(c, 0, NVC9B7_SET_OUT_REF_PIC_LUMA, 1, (NvU32)(recOut >> 8)); chan_mthd(c, 0, NVC9B7_SET_OUT_REF_PIC_CHROMA, 1, (NvU32)(recOut >> 8));
    chan_mthd(c, 0, NVC9B7_SET_OUT_ENC_STATUS, 1, (NvU32)(status->gpu >> 8));
    chan_mthd(c, 0, NVC9B7_SET_OUT_BITSTREAM, 1, (NvU32)(bs->gpu >> 8));
    chan_mthd(c, 0, NVC9B7_SET_IN_CUR_PIC, 1, (NvU32)(inVa >> 8));
    chan_mthd(c, 0, NVC9B7_SET_IN_CUR_PIC_CHROMA_U, 1, (NvU32)(inVa >> 8)); chan_mthd(c, 0, NVC9B7_SET_IN_CUR_PIC_CHROMA_V, 1, (NvU32)(inVa >> 8));
    chan_mthd(c, 0, NVC9B7_SET_IN_RCDATA, 1, (NvU32)(e->rcdata.gpu >> 8));
    chan_mthd(c, 0, NVC9B7_SET_IN_AV1_PROBABILITY_DATA, 1, (NvU32)(probIn >> 8));
    chan_mthd(c, 0, NVC9B7_SET_OUT_AV1_PROBABILITY_DATA, 1, (NvU32)(probOut >> 8));
    static const NvU32 lm[6] = { NVC9B7_SET_IOENTROPY_ABOVE_MV_DATA, NVC9B7_SET_IOENTROPY_ABOVE_CTX_DATA, NVC9B7_SET_IODEBLOCK_TOPROW,
                                 NVC9B7_SET_IODEBLOCK_LEFTCOLUMN, NVC9B7_SET_IOCDEF_LEFTCOLUMN, NVC9B7_SET_IOLR_LEFTCOLUMN };
    for (int i = 0; i < 6; i++) chan_mthd(c, 0, lm[i], 1, (NvU32)(e->av1Line[i].gpu >> 8));
    for (int i = 0; !idr && i < nRefs; i++) {   // REF_PIC<reference type>: anchors LAST.. = newest first; B: LAST.. = slotOf[1..], ALTREF = slotOf[0]
        const NvU32 type = isB ? (i ? (NvU32)i - 1 : 6u) : (NvU32)i;
        const NvU64 ra = e->recon.gpu + (NvU64)e->slotOf[i] * e->rSlot;
        chan_mthd(c, 0, NVC9B7_SET_IN_REF_PIC0_LUMA + 4 * type, 1, (NvU32)(ra >> 8)); chan_mthd(c, 0, NVC9B7_SET_IN_REF_PIC0_CHROMA + 4 * type, 1, (NvU32)(ra >> 8));
    }
    e->pend[ds].lf = lf;
}

// two passes (T7.09), as NVIDIA's driver runs -multipass fullres (docs/traces/quality-linux vmp, HEVC at p4): the first pass
// is the frame's setup with two_pass_rc 1 and cheaper mode decision (its MD block, no transform skip, quantizer cost vectors,
// SAO; FPP partition decision, one ME winner); the second has two_pass_rc 2 and takes MV hints from the first pass's motion
// vectors (its ME predictor sources and search shapes). H.264 (not traced): only two_pass_rc, unless ENC_MP_HINT is set.
static void two_pass_setups(Enc *e, RmMem *setup, RmMem *setup1, bool hevc)
{
    NvU8 *s2 = setup->cpu, *s1 = setup1->cpu;
    memcpy(s1, s2, SETUP_SIZE);
    nvenc_h264_me_control_s *me1 = (void *)(s1 + OFF_ME);
    const bool hint = hevc ? !getenv("ENC_MP_NOHINT") : getenv("ENC_MP_HINT") != NULL;
    if (hint) { memcpy(s2 + OFF_ME + 12, hevc_mp2_me + 12, 16); memcpy(s2 + OFF_ME + 44, hevc_mp2_me + 44, 48); }   // predsrc, shape0-3
    if (hevc) {
        nvenc_h265_drv_pic_setup_s *p2 = (void *)s2, *p1 = (void *)s1;
        p2->rate_control.two_pass_rc = 2; p1->rate_control.two_pass_rc = 1;
        p1->pps_data.transform_skip_enabled_flag = 0;
        me1->Cu16partDecisionMadeByFPP = 1; me1->fbm_op_winner_num_p_frame = 1;
        memcpy(s1 + OFF_MD, hevc_mp1_md, sizeof hevc_mp1_md); memset(s1 + OFF_Q, 0, 16);
        nvenc_h265_slice_control_s *sc = (void *)(s1 + OFF_SLICE);
        sc->qp_slice_max = 51; sc->slice_sao_luma_flag = 0; sc->slice_sao_chroma_flag = 0;
    } else {
        ((nvenc_h264_drv_pic_setup_s *)s2)->rate_control.two_pass_rc = 2; ((nvenc_h264_drv_pic_setup_s *)s1)->rate_control.two_pass_rc = 1;
    }
}

int enc_submit(Enc *e, bool force_idr, NvU32 *ticket) { return enc_submit_ex(e, force_idr, false, -1, ticket); }

int enc_submit_ex(Enc *e, bool force_idr, bool isB, int disp, NvU32 *ticket)
{
    Chan *c = &e->chan;
    int W = e->cfg.width, H = e->cfg.height, ds = (int)(e->seq % (NvU32)e->cfg.depth);
    RmMem *setup = &e->setup[ds], *in = &e->in[ds], *bs = &e->bs[ds], *status = &e->status[ds];
    if (enc_inflight(e) >= e->cfg.depth) return -1;
    const bool b = isB && e->cfg.bframes > 0 && e->nRefs >= 2;
    // the IDR period applies to pictures in coding order (enc_submit); with display indices (enc_submit_ex) the caller places
    // the IDRs (force_idr) after coding the pictures before them, else B pictures would reference across the IDR
    bool idr = !b && (force_idr || e->forceIdr || e->seq == 0 || (disp < 0 && e->cfg.idr_period > 0 && e->sinceIdr >= (unsigned)e->cfg.idr_period));
    e->forceIdr = false;
    if (idr) { e->frame_num = 0; e->sinceIdr = 0; e->nRefs = 0; if (disp >= 0) e->idrDisp = disp; }
    int slot = b ? e->bSlot : e->nextSlot;                    // where this picture's reconstruction goes
    int nRefs = idr ? 0 : e->nRefs;
    // POC: from the display index (enc_submit_ex), else from the count since the IDR (one picture after the other)
    const unsigned poc = disp >= 0 ? 2u * (unsigned)(disp - e->idrDisp) : 2u * e->sinceIdr;
    // B: list 0 = the anchors before it (slotOf[1..]), list 1 = the newest anchor (slotOf[0], after it in display order)
    const int nL0 = b ? nRefs - 1 : nRefs;
    const int *L0 = b ? e->slotOf + 1 : e->slotOf;

    // ---- picture setup ----
    memset(setup->cpu, 0, SETUP_SIZE);
    bool hevc = e->cfg.codec == ENC_HEVC;
    const bool av1 = e->cfg.codec == ENC_AV1;
    if (hevc) { setup_hevc(e, setup, bs, idr, nRefs, b, poc / 2); goto methods; }   // HEVC POCs: one per picture
    {
    nvenc_h264_drv_pic_setup_s *ps = setup->cpu;
    ps->magic = enc_magic(e);
    nvenc_h264_surface_cfg_s ic = { .frame_width_minus1 = (unsigned short)(W - 1), .frame_height_minus1 = (unsigned short)(H - 1),
        .sfc_pitch = (unsigned short)e->pitch, .sfc_pitch_chroma = (unsigned short)e->pitch, .block_height = 2 };
    nvenc_h264_surface_cfg_s rc = ic; rc.sfc_pitch = rc.sfc_pitch_chroma = (unsigned short)e->rPitch;
    if (e->cfg.chroma444) { ic.sfc_pitch_chroma = (unsigned short)(2 * e->pitch); rc.sfc_pitch_chroma = (unsigned short)(2 * e->rPitch); }   // as HEVC 4:4:4
    ps->input_cfg = ic; ps->refpic_cfg = rc; ps->outputpic_cfg = rc;   // plane offsets 0: each plane has its own method
    H264Params hp = hdr_params(e);
    ps->sps_data.profile_idc = (unsigned)hp.profile_idc; ps->sps_data.level_idc = (unsigned)hp.level_idc; ps->sps_data.chroma_format_idc = e->cfg.chroma444 ? 3 : 1;
    ps->sps_data.lossless_qpprime_flag = e->cfg.lossless;
    ps->sps_data.pic_order_cnt_type = (unsigned)hp.poc_type; ps->sps_data.log2_max_frame_num_minus4 = LOG2_MAX_FRAME_NUM - 4;
    ps->sps_data.log2_max_pic_order_cnt_lsb_minus4 = (unsigned)hp.log2_max_poc_lsb_minus4;
    ps->sps_data.frame_mbs_only = 1;
    ps->pps_data.num_ref_idx_l0_active_minus1 = (unsigned)(e->cfg.num_refs - 1);
    ps->pps_data.deblocking_filter_control_present_flag = 1;
    // High / CABAC as nullmoth's libnvenc_h264 sets them (profile_idc, entropy and 8x8 transform flags; T7.02)
    ps->pps_data.entropy_coding_mode_flag = e->cfg.cabac; ps->pps_data.transform_8x8_mode_flag = e->cfg.h264_profile == 100 || e->cfg.chroma444;
    nvenc_h264_rc_s *r = &ps->rate_control;
    // QP[] = P, B, I; B pictures (not references) 2 above P, as encoders usually do
    const int qp_b = e->cfg.qp_p + 2 > 51 ? 51 : e->cfg.qp_p + 2;
    r->QP[0] = (unsigned char)e->cfg.qp_p; r->QP[1] = (unsigned char)qp_b; r->QP[2] = (unsigned char)e->cfg.qp_i;
    for (int i = 0; i < 3; i++) { r->minQP[i] = 0; r->maxQP[i] = 51; r->rhopbi[i] = 256; }
    r->framerate = e->cfg.fps; r->gop_length = 0xffffffffu;
    if (e->cfg.rc_mode) {   // field encodings as the reference client fills them (it runs const QP, though): T3.06
        double pixels = (double)W * H, perFrame = (double)e->cfg.bitrate / e->cfg.fps;
        r->hrd_type = 2;                                     // NAL HRD
        r->maxQPD = 6; r->baseQPD = 3;
        r->framerate = e->cfg.fps << 8;
        r->buffersize = (unsigned)e->cfg.vbv_bits; r->nal_cpb_size = r->vcl_cpb_size = e->cfg.vbv_bits;
        r->nal_bitrate = e->cfg.max_bitrate; r->vcl_bitrate = e->cfg.max_bitrate;
        r->gop_length = e->cfg.idr_period > 0 ? (unsigned)e->cfg.idr_period : 0xffffffffu;
        r->Np = e->cfg.idr_period > 0 ? (e->cfg.idr_period - 1) << 4 : 0x7fffffff;
        r->Ravg = r->R = (int)(perFrame / pixels * 256);       // bits per pixel per frame, 23.8
        if (e->cfg.rc_mode == 1 || e->cfg.rc_mode == 17) r->R = (int)((double)e->cfg.max_bitrate / e->cfg.fps / pixels * 256);   // VBR: R = maximum (as HEVC)
        r->Bmin = 0; r->iSizeRatioX = 1; r->iSizeRatioY = 1;
        // NVIDIA's CBR fields, as for HEVC (docs/traces/quality-linux hcbr, T7.08): with 2 B pictures +0.7-0.8 VMAF
        if ((e->cfg.rc_mode & 3) == 2) {   // CBR only (with Ravg 0 VBR ran at its maximum rate: T7.09)
            r->hrd_type = 34; r->rhopbi[0] = 286; r->rhopbi[1] = 317; r->rhopbi[2] = 256;
            r->Np = rc_np(e); r->Bmin = (int)(r->buffersize / 81920); r->Ravg = 0;
        }
        // single_frame_VBV stays 0: set to 1 (with a one-frame VBV) the rate went to 3x the target at QP 5 (T3.08). A
        // one-frame VBV by itself works: frames stay under bitrate/fps.
    }

    nvenc_h264_pic_control_s *pc = &ps->pic_control;
    memset(pc->l0, -2, sizeof pc->l0); memset(pc->l1, -2, sizeof pc->l1);   // -2 = unused, as the reference client
    for (int i = 0; i < nL0; i++) {                            // field-indexed DPB slots; distances in frames
        pc->l0[i] = (char)(2 * L0[i]); pc->temp_dist_l0[i] = (char)(((int)poc - (int)e->slotPoc[L0[i]]) / 2);
    }
    if (b) { pc->l1[0] = (char)(2 * e->slotOf[0]); pc->temp_dist_l1[0] = (char)(((int)poc - (int)e->slotPoc[e->slotOf[0]]) / 2); }
    // DistScaleFactor (8.4.1.2.3) per list 0 reference against list 1's first, 256 in P pictures, -1 unused (as NVIDIA's
    // driver sets them, docs/traces/bframes-linux); diff_pic_order_cnt_zero copied from its B pictures (one L0, one L1)
    for (int i = 0; i < 8; i++) for (int j = 0; j < 8; j++) pc->dist_scale_factor[i][j] = b ? -1 : 256;   // [refidx1][refidx0]
    if (b) {
        for (int i = 0; i < nL0; i++) {
            const int tb = pc->temp_dist_l0[i], td = pc->temp_dist_l0[i] - pc->temp_dist_l1[0];
            const int tx = (16384 + (td < 0 ? -td : td) / 2) / td, dsf = (tb * tx + 32) >> 6;
            pc->dist_scale_factor[0][i] = (short)(dsf < -1024 ? -1024 : dsf > 1023 ? 1023 : dsf);
        }
        pc->diff_pic_order_cnt_zero[0] = 0xff00ff00u; pc->diff_pic_order_cnt_zero[1] = 0xff00fffeu;
    } else if (!idr) { pc->diff_pic_order_cnt_zero[0] = pc->diff_pic_order_cnt_zero[1] = 0xfcfcfcfcu; }
    pc->frame_num = (unsigned short)e->frame_num; pc->pic_order_cnt_lsb = (unsigned short)(poc & 0xff);
    pc->slice_control_offset = OFF_SLICE; pc->me_control_offset = OFF_ME; pc->md_control_offset = OFF_MD; pc->q_control_offset = OFF_Q;
    pc->hist_buf_size = (unsigned)e->hist.size; pc->bitstream_buf_size = (unsigned)bs->size;
    pc->pic_type = idr ? 3 : b ? 1 : 0; pc->ref_pic_flag = !b; pc->cabac_zero_word_enable = 1; pc->e4byteStartCode = 1;
    pc->slice_mode = 0;   // 1 hangs the engine (T2.05e); one control with all MBs = one slice

    // slices: one slice control per slice, whole MB rows, under slice_mode 0 (slice_mode 1 hangs the engine: T2.05e, T3.09)
    int nsl = e->cfg.slices < 1 ? 1 : e->cfg.slices > ENC_MAX_SLICES ? ENC_MAX_SLICES : e->cfg.slices;
    if (nsl > e->hmb) nsl = e->hmb;
    pc->num_forced_slices_minus1 = (unsigned char)(nsl - 1);
    nvenc_h264_slice_control_s *sc = (void *)((NvU8 *)setup->cpu + OFF_SLICE);
    for (int k = 0; k < nsl; k++) {
        int mb = ((k + 1) * e->hmb / nsl - k * e->hmb / nsl) * e->wmb;   // rows spread evenly
        sc[k].num_mb = (unsigned)mb; sc[k].qp_avr = (unsigned)(idr ? e->cfg.qp_i : b ? qp_b : e->cfg.qp_p); sc[k].qp_slice_min = 0; sc[k].qp_slice_max = 51;
        if (!idr) { sc[k].num_ref_idx_active_override_flag = 1; sc[k].num_ref_idx_l0_active_minus1 = (unsigned)(nL0 - 1); sc[k].num_ref_idx_l1_active_minus1 = 0; }
    }

    setup_me((void *)((NvU8 *)setup->cpu + OFF_ME), !idr);
    setup_md((void *)((NvU8 *)setup->cpu + OFF_MD), !idr, e->cfg.h264_profile == 100 || e->cfg.chroma444);
    if (e->cfg.quality || b) {   // NVIDIA's controls for this picture type (docs/traces/bframes-linux, -preset p4; size-independent):
        NvU8 *base = setup->cpu;       // B pictures always (temporal direct, list 1 / bi partitions); I / P with EncConfig.quality
        memcpy(base + OFF_ME, idr ? h264_tmpl_me_idr : b ? h264_tmpl_me_b : h264_tmpl_me_p, 0x100);
        memcpy(base + OFF_MD, idr ? h264_tmpl_md_idr : b ? h264_tmpl_md_b : h264_tmpl_md_p, 0x100);
        memcpy(base + OFF_Q, h264_tmpl_q, 0x100);
        if (e->cfg.h264_profile == 100 || e->cfg.chroma444) {   // High: the 8x8 transform's intra modes (the trace is Main profile)
            nvenc_h264_md_control_s *md = (void *)(base + OFF_MD); md->intra_luma8x8_mode_enable = 0x1ff;
        }
    }
    if (!(e->cfg.quality || b)) {   // real time: the quant control (dead zones, QP post-processing) was left empty before T15.09,
        NvU8 *base = setup->cpu;       // which cost ~8 VMAF at the same rate on the GTX 1650 (encodeN; +3-4 through VideoToolbox)
        if (rm_class(&e->rm, RM_CLS_NVENC) == 0xb4b7) {   // GTX 1650: NVIDIA's own p4 controls for that card, also 30 % faster
            memcpy(base + OFF_ME, idr ? h264_nv66_me_idr : h264_nv66_me_p, 0x100);
            memcpy(base + OFF_MD, idr ? h264_nv66_md_idr : h264_nv66_md_p, 0x100);
            memcpy(base + OFF_Q, h264_nv66_q, 0x100);
            if (e->cfg.h264_profile == 100 || e->cfg.chroma444) { nvenc_h264_md_control_s *md = (void *)(base + OFF_MD); md->intra_luma8x8_mode_enable = 0x1ff; }
        } else {   // Ada: NVIDIA's p4 quant table (as quality mode). RTX 4090, VideoToolbox at the same rate: +3.1 / +2.7 VMAF
            // (crowd / park 1080p), +2.1 (crowd 4K), same speed. ENC_H264_RT_QTABLE=0 leaves the control empty as before.
            const char *q = getenv("ENC_H264_RT_QTABLE");
            if (!q || q[0] != '0') memcpy(base + OFF_Q, h264_tmpl_q, 0x100);
        }
    }
    if (e->cfg.la_pass) {   // lookahead pictures (la.c): NVIDIA's controls for them (docs/traces/quality-linux vla, T7.09)
        NvU8 *base = setup->cpu;
        memcpy(base + OFF_ME, idr ? h264_la_me_idr : h264_la_me_p, 0x100); memcpy(base + OFF_MD, idr ? h264_la_md_idr : h264_la_md_p, 0x100);
        memcpy(base + OFF_Q, h264_la_q, 0x100);
    }
    }

    // ---- methods ----
methods:;
    // two passes for this picture: HEVC only for I / P (B pictures in one pass gave the same quality at 6.0 instead of 7.6 ms
    // per 1080p frame); H.264 for every picture (one-pass B pictures among two-pass anchors undershot the rate by a quarter)
    const bool mp = e->cfg.two_pass && !(b && hevc);
    if (mp) two_pass_setups(e, setup, &e->setup1[ds], hevc);
    // the 64 KB push buffer starts over once the channel fetched everything: tried from 32 KB on, waited for past 48 KB (a
    // frame takes well under 1 KB; AV1 once skipped this and ran off the end after ~200 frames)
    if (chan_push_used(c) > 0x8000 && !chan_rewind(c) && chan_push_used(c) > 0xc000) {
        const double t0 = now_ms();
        while (!chan_rewind(c) && now_ms() - t0 < 3000) { struct timespec ts = { 0, 50000 }; nanosleep(&ts, NULL); }
        if (chan_push_used(c) > 0xc000) { fprintf(stderr, "enc: push buffer not fetched\n"); return -1; }
    }
    if (av1) { submit_av1(e, ds, idr, b, slot, nRefs, poc); goto execute; }
    NvU64 recOut = e->recon.gpu + (NvU64)slot * e->rSlot;
    NvU64 inVa = e->extInput ? e->extInput : in->gpu;
    RmMem *meIn = &e->mepred[e->seq & 1], *meOut = &e->mepred[(e->seq + 1) & 1];
    // HEVC: application ID 3 and codec type 4 (not in clc9b7.h; seen in nullmoth's library, T6.03)
    NvU32 appId = hevc ? (NvU32)NVC9B7_SET_APPLICATION_ID_ID_NVENC_H265 : (NvU32)NVC9B7_SET_APPLICATION_ID_ID_NVENC_H264;
    if (getenv("ENC_PROBE_APPID")) appId = (NvU32)atoi(getenv("ENC_PROBE_APPID"));   // T13.03 probe: documented IDs only (2 VP8, 4 VP9)
    // two passes (T7.09): pass 1 writes the RC statistics and its own outputs into scratch buffers, then pass 2 (the real
    // encode) reads them; NVIDIA's driver pushes the pair back to back (vmp)
    for (int pass = mp ? 1 : 0; pass <= (mp ? 2 : 0); pass++) {
    const bool p1 = pass == 1;
    const NvU64 rec = p1 ? e->rec1.gpu : recOut;
    chan_mthd(c, 0, NVC9B7_SET_APPLICATION_ID, 1, appId);
    chan_mthd(c, 0, NVC9B7_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _CODEC_TYPE, hevc ? 4u : NVC9B7_SET_CONTROL_PARAMS_CODEC_TYPE_H264) |
              DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _FORCE_OUT_PIC, 1) | DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _RCMODE, (NvU32)e->cfg.rc_mode) |
              DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _RCSTAT_WRITE, p1 ? 1u : 0u) | DRF_NUM(C9B7, _SET_CONTROL_PARAMS, _RCSTAT_READ, pass == 2 ? 1u : 0u));
    chan_mthd(c, 0, NVC9B7_SET_PICTURE_INDEX, 1, e->seq);
    struct { NvU32 m; NvU64 a; } bm[] = {
        { NVC9B7_SET_IN_DRV_PIC_SETUP, p1 ? e->setup1[ds].gpu : setup->gpu }, { NVC9B7_SET_IOHISTORY, p1 ? e->hist1.gpu : e->hist.gpu },
        { hevc ? NVC9B7_SET_OUT_REF_PIC_LUMA : NVC9B7_SET_OUT_COLOC_DATA, hevc ? rec : p1 ? e->coloc1.gpu : b ? e->colocB.gpu : e->coloc.gpu },   // HEVC: no colocated data
        { NVC9B7_SET_OUT_REF_PIC_LUMA, rec }, { NVC9B7_SET_OUT_REF_PIC_CHROMA, rec + e->rLuma },
        { NVC9B7_SET_OUT_ENC_STATUS, p1 ? e->status1.gpu : status->gpu }, { NVC9B7_SET_OUT_BITSTREAM, p1 ? e->bs1.gpu : bs->gpu },
        { NVC9B7_SET_IN_CUR_PIC, inVa }, { NVC9B7_SET_IN_CUR_PIC_CHROMA_U, inVa + e->lumaSz },
        { NVC9B7_SET_IN_CUR_PIC_CHROMA_V, inVa + e->lumaSz }, { NVC9B7_SET_IO_RC_PROCESS, e->rcproc.gpu },
        { NVC9B7_SET_IN_RCDATA, e->rcdata.gpu } };
    for (size_t i = 0; i < sizeof bm / sizeof bm[0]; i++) chan_mthd(c, 0, bm[i].m, 1, (NvU32)(bm[i].a >> 8));
    if (!idr) {
        if (!hevc) chan_mthd(c, 0, NVC9B7_SET_IN_COLOC_DATA, 1, (NvU32)(e->coloc.gpu >> 8));
        chan_mthd(c, 0, NVC9B7_SET_IN_MEPRED_DATA, 1, (NvU32)((pass == 2 ? e->mepred1.gpu : meIn->gpu) >> 8));
        chan_mthd(c, 0, NVC9B7_SET_OUT_MEPRED_DATA, 1, (NvU32)((p1 ? e->mepred1.gpu : meOut->gpu) >> 8));
        for (int i = 0; i < nRefs; i++) {
            // H.264: REF_PIC<n> by DPB slot (l0 = 2 x slot). HEVC: by position in list 0 (poc_entry_l0[i] = i), while l0
            // still carries 2 x slot (nullmoth's third picture: l0[0] 2, the reference in REF_PIC0; T6.07)
            // HEVC entries: list 0 first, then list 1 (a B picture: list 0 = slotOf[1..], list 1 = slotOf[0])
            const int es = hevc && b ? (i < nRefs - 1 ? e->slotOf[i + 1] : e->slotOf[0]) : e->slotOf[i];
            // NVENC 6.6 (GTX 1650, 0xb4b7) indexes HEVC references by DPB slot as H.264 does (NVIDIA's Linux driver: the
            // reference of l0[0] = 2 in REF_PIC1; docs/traces/gtx1650-linux): by list position the second P picture read an
            // unset REF_PIC and faulted
            const bool bySlot = !hevc || rm_class(&e->rm, RM_CLS_NVENC) == 0xb4b7;
            NvU32 n = bySlot ? (NvU32)es : (NvU32)i;
            NvU64 ra = e->recon.gpu + (NvU64)es * e->rSlot;
            chan_mthd(c, 0, NVC9B7_SET_IN_REF_PIC0_LUMA + 4 * n, 1, (NvU32)(ra >> 8));
            chan_mthd(c, 0, NVC9B7_SET_IN_REF_PIC0_CHROMA + 4 * n, 1, (NvU32)((ra + e->rLuma) >> 8));
        }
    }
    if (p1) chan_mthd(c, 0, NVC9B7_EXECUTE, 1, 0u);
    }
execute:
    chan_mthd(c, 0, NVC9B7_EXECUTE, 1, 0u);
    NvU32 payload = ++e->seq;
    chan_mthd(c, 0, NVC9B7_SEMAPHORE_A, 3, (NvU32)(e->sem.gpu >> 32), (NvU32)e->sem.gpu, payload);
    chan_mthd(c, 0, NVC9B7_SEMAPHORE_D, 1, DRF_DEF(C9B7, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B7, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B7, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    // event: the host waits for that release, then raises the channel's non-stall interrupt (NVK's way), which RM
    // counts on the NVENC<n> notifier. (The engine's own SEMAPHORE_D AWAKEN bit produced no event: T5.04.)
    if (e->ev.h) {
        chan_mthd(c, 0, NVC56F_SEM_ADDR_LO, 5, (NvU32)e->sem.gpu, (NvU32)(e->sem.gpu >> 32), payload, 0u,
                  DRF_DEF(C56F, _SEM_EXECUTE, _OPERATION, _ACQ_CIRC_GEQ) | DRF_DEF(C56F, _SEM_EXECUTE, _PAYLOAD_SIZE, _32BIT));
        chan_mthd(c, 0, NVC56F_NON_STALL_INTERRUPT, 1, 0u);
    }
    memset(status->cpu, 0, av1 ? 0x880 : 256);
    if (getenv("ENC_DUMP_SETUP")) {   // diagnostics: each picture setup (with its controls) as DIR/encN.bin
        char path[256]; snprintf(path, sizeof path, "%s/enc%u.bin", getenv("ENC_DUMP_SETUP"), payload);
        FILE *df = fopen(path, "wb"); if (df) { fwrite(setup->cpu, 1, SETUP_SIZE, df); fclose(df); }
    }
    e->pend[ds].idr = idr; e->pend[ds].frame_num = e->frame_num; e->pend[ds].poc = av1 ? poc / 2 : poc;
    e->pend[ds].t0 = now_ms(); e->pend[ds].slot = slot; e->pend[ds].nRefs = nRefs; e->pend[ds].b = b;
    // AV1: an anchor with B pictures before it in display order is hidden until the last of them is shown
    e->pend[ds].hidden = av1 && !idr && !b && disp >= 0 && e->cfg.bframes > 0 && disp > e->av1LastShown + 1;
    if (e->pend[ds].hidden) e->av1HidDisp = disp;
    else if (disp >= 0) e->av1LastShown = b && disp == e->av1HidDisp - 1 ? e->av1HidDisp : disp;
    for (int i = 0; i < nRefs; i++) { e->pend[ds].refs[i] = e->slotOf[i]; e->pend[ds].refOh[i] = (int)(e->slotPoc[e->slotOf[i]] / 2); }
    chan_submit(c);
    *ticket = payload;

    if (b) return 0;                                          // B pictures are not references: DPB, frame_num and count unchanged
    e->slotPoc[slot] = poc;
    // DPB (decided by submission order): this picture becomes the most recent reference; the oldest drops out past num_refs
    int keep = e->nRefs < e->cfg.num_refs ? e->nRefs : e->cfg.num_refs - 1;
    if (idr) keep = 0;
    memmove(e->slotOf + 1, e->slotOf, sizeof e->slotOf[0] * (size_t)keep);
    e->slotOf[0] = slot; e->nRefs = keep + 1;
    for (int s = 0; s <= e->cfg.num_refs; s++) {             // next free slot: one not in the DPB
        int used = 0; for (int i = 0; i < e->nRefs; i++) used |= e->slotOf[i] == s;
        if (!used) { e->nextSlot = s; break; }
    }
    e->frame_num = (e->frame_num + 1) % (1u << LOG2_MAX_FRAME_NUM);
    e->sinceIdr++;
    return 0;
}

// AV1: the temporal unit around the engine's tiles, with the headers NVIDIA's driver writes for this setup (docs/traces/
// av1-linux): sequence header with CDEF and restoration on; frame header with the engine's base quantizer, loop filter
// level and CDEF damping (status), the setup's CDEF strengths (0 and luma primary 4; av1Hq: luma primary 0, 2, 4, 9),
// restoration off on every plane (av1Hq: self-guided on luma, 64x64 units), TX_MODE_SELECT; inter frames: switchable interpolation filters, reference_select, CDFs from LAST (primary
// reference 0) and saved at the end of the frame. Reference slots = reconstruction slots; LAST, LAST2, LAST3, GOLDEN name
// the references newest first (EncConfig.num_refs, up to 4), the other reference types repeat the oldest.
static Av1SeqParams av1_seq(const Enc *e)
{
    return (Av1SeqParams){ .width = e->cfg.width, .height = e->cfg.height, .bit_depth = e->cfg.bit_depth, .full_range = e->cfg.full_range,
                           .colour = { e->cfg.colour[0], e->cfg.colour[1], e->cfg.colour[2] },
                           .enable_cdef = true, .enable_restoration = true };
}
size_t enc_av1_config(Enc *e, uint8_t *out) { Av1SeqParams sp = av1_seq(e); return av1_write_sequence_header(&sp, out); }

static int av1_assemble(Enc *e, int ds, EncFrame *out)
{
    const NvU8 *sb = e->status[ds].cpu;
    const nvenc_av1_pic_stat_s *st = (const void *)sb;
    const NvU32 *ts = (const NvU32 *)(sb + 0x100);                 // tile statistics (tile_stat_offset 0x100): offset, size, 16 bytes each
    const bool key = e->pend[ds].idr;
    Av1SeqParams sp = av1_seq(e);
    const int lf = st->loop_filter_level;
    const bool isB = e->pend[ds].b;
    Av1FrameParams fp = { .frame_type = key ? 0 : 1, .hidden = e->pend[ds].hidden, .order_hint = (int)e->pend[ds].poc,
        .refresh_frame_flags = isB ? 0 : 1 << e->pend[ds].slot, .primary_ref_frame = key ? 7 : isB ? 6 : 0, .base_q_idx = st->av1_start_QP, .lf_level = { lf, lf, lf, lf },
        .cdef_damping = (int)st->cdef_damping_minus_3 + 3, .cdef_bits = e->av1Hq ? 2 : 1, .cdef_uv = { 0, 0, 0, 0 },
        .cdef_y = { 0, e->av1Hq ? 8 : 16, 16, 36 }, .lr_type = { e->av1Hq ? 3 : 0, 0, 0 },
        .tx_mode_select = true, .reference_select = !key, .interp_filter = 4, .tile_size_bytes = 4,
        .tile_cols_log2 = e->av1TileColsLog2, .tile_rows_log2 = e->av1TileRowsLog2, .context_update_tile_id = 0, .delta_q_present = e->cfg.rc_mode != 0 };
    for (int i = 0; i < 7; i++) {   // LAST, LAST2, LAST3, GOLDEN = the references newest first; the rest repeat the oldest
        // B pictures: LAST.. = the anchors before (refs[1..]), BWDREF / ALTREF2 / ALTREF = the anchor after (refs[0])
        const int nr = e->pend[ds].nRefs;
        const int r = !nr ? 0 : isB ? (i >= 4 ? 0 : 1 + (i < nr - 1 ? i : nr - 2)) : (i < nr ? i : nr - 1);
        fp.ref_frame_idx[i] = e->pend[ds].nRefs ? e->pend[ds].refs[r] : 0; fp.ref_order_hint[i] = e->pend[ds].nRefs ? e->pend[ds].refOh[r] : (int)e->pend[ds].poc - 1;
    }
    const int nt = e->av1TileCols * e->av1TileRows;
    const uint8_t *tile[128]; size_t tsz[128], total = 0;
    if (getenv("ENC_AV1_LOG")) fprintf(stderr, "av1: frame %u %s: %u bits, q %u, lf %u (asked %d), cdef damping %u, %u tiles, tile 0 at %u size %u\n",
                                       e->pend[ds].poc, key ? "key" : "inter", st->total_bit_count, st->av1_start_QP, lf, e->pend[ds].lf,
                                       st->cdef_damping_minus_3 + 3, st->num_tiles, ts[0], ts[1]);
    if (st->num_tiles != nt || nt > 128) return -1;
    for (int i = 0; i < nt; i++) {   // the engine leaves room for the tile size fields between the tiles
        tile[i] = (const uint8_t *)e->bs[ds].cpu + ts[4 * i]; tsz[i] = ts[4 * i + 1]; total += tsz[i] + 4;
        if (!tsz[i] || ts[4 * i] + tsz[i] > e->bs[ds].size) return -1;
    }
    if (total + 1024 > e->av1TuCap) return -1;
    if (fp.hidden) {   // kept for the next shown frame's temporal unit
        e->av1HidN = av1_write_frame(&sp, &fp, tile, tsz, nt, e->av1Hid);
        e->av1HidSlot = e->pend[ds].slot;                          // set here (in completion order): later submissions run ahead
        out->data = e->av1Hid; out->size = 0; out->hidden = true;
        return 0;
    }
    uint8_t *o = e->av1Tu[ds]; size_t n = av1_write_temporal_delimiter(o);
    if (key) n += av1_write_sequence_header(&sp, o + n);
    if (key && (e->cfg.has_mdcv || e->cfg.has_cll)) n += av1_write_hdr_metadata(e->cfg.has_mdcv ? e->cfg.mdcv : NULL, e->cfg.has_cll ? e->cfg.cll : NULL, o + n);
    if (e->av1HidN) { memcpy(o + n, e->av1Hid, e->av1HidN); n += e->av1HidN; e->av1HidN = 0; }
    n += av1_write_frame(&sp, &fp, tile, tsz, nt, o + n);
    out->data = o; out->size = n;
    return 0;
}

size_t enc_av1_show_existing(Enc *e, uint8_t *out)
{
    const size_t n = av1_write_temporal_delimiter(out);
    return n + av1_write_show_existing(e->av1HidSlot, out + n);
}

int enc_wait(Enc *e, NvU32 ticket, EncFrame *out)
{
    memset(out, 0, sizeof *out);
    Chan *c = &e->chan;
    int ds = (int)((ticket - 1) % (NvU32)e->cfg.depth);
    volatile NvU32 *sv = e->sem.cpu;
    double t0 = e->pend[ds].t0;
    // spin 50 us, then sleep on the event (re-checking the semaphore between reading and waiting on the count); later
    // frames release higher payloads
    for (;;) {
        if ((int32_t)(*sv - ticket) >= 0 || chan_error(c)) break;
        double el = now_ms() - t0;
        if (el > 3000) break;
        if (e->ev.h && el > 0.05) {
            int64_t seen = rm_event_wait(&e->ev, 0, 0);
            if ((int32_t)(*sv - ticket) >= 0) break;
            if (seen >= 0) rm_event_wait(&e->ev, (uint32_t)seen, 2000);
        }
    }
    out->ms = now_ms() - t0;
    e->waited = ticket;

    nvenc_pic_stat_s *st = e->status[ds].cpu;
    if (getenv("ENC_AV1_LOG") && e->cfg.codec == ENC_AV1) {
        const NvU32 *w = e->status[ds].cpu; fprintf(stderr, "status:");
        for (int i = 0; i < 32; i++) fprintf(stderr, "%s%08x", i % 8 ? " " : "\n  ", w[i]);
        fprintf(stderr, "\n");
    }
    out->idr = e->pend[ds].idr; out->frame_num = e->pend[ds].frame_num; out->poc = e->pend[ds].poc;
    out->bits = st->total_bit_count; out->ucode_error = st->ucode_error_status; out->avg_qp = st->avgQP;
    out->intra_mbs = st->intra_mb_count; out->inter_mbs = st->inter_mb_count;
    out->intra_cost = st->total_intra_cost; out->inter_cost = st->total_inter_cost;
    if ((int32_t)(*sv - ticket) < 0) {
        fprintf(stderr, "enc: frame %u %s (notifier 0x%x info32 0x%x)\n", ticket, chan_error(c) ? "engine error" : "timeout",
                chan_error(c), ((volatile NvNotification *)c->notifier.cpu)[0].info32);
        NvU64 fa; NvU32 ft; char fs[32];
        if (chan_error(c) && !chan_fault(c, &fa, &ft, fs)) fprintf(stderr, "enc: MMU fault at 0x%llx type %u %s\n", (unsigned long long)fa, ft, fs);
        e->forceIdr = true;
        return -1;
    }
    // error_status is 2 on good frames too: judge by the microcode error and the bit count
    if (st->ucode_error_status || !st->total_bit_count || st->total_bit_count / 8 >= e->bs[ds].size) { e->forceIdr = true; return -1; }
    out->data = (const uint8_t *)e->bs[ds].cpu + st->bitstream_start_pos; out->size = st->total_bit_count / 8;
    if (out->idr && (e->cfg.has_mdcv || e->cfg.has_cll) && e->seiBuf) {   // HDR metadata in front of the key frame's slices
        const size_t sn = hdr_write_sei(e->cfg.codec == ENC_HEVC, e->cfg.has_mdcv ? e->cfg.mdcv : NULL, e->cfg.has_cll ? e->cfg.cll : NULL, e->seiBuf);
        memcpy(e->seiBuf + sn, out->data, out->size); out->data = e->seiBuf; out->size += sn;
    }
    if (e->cfg.codec == ENC_AV1) return av1_assemble(e, ds, out);
    return 0;
}

int enc_encode(Enc *e, bool force_idr, EncFrame *out)
{
    NvU32 t;
    if (enc_submit(e, force_idr, &t)) { memset(out, 0, sizeof *out); return -1; }
    return enc_wait(e, t, out);
}

int enc_encode_ex(Enc *e, bool force_idr, bool b, int disp, EncFrame *out)
{
    NvU32 t;
    if (enc_submit_ex(e, force_idr, b, disp, &t)) { memset(out, 0, sizeof *out); return -1; }
    return enc_wait(e, t, out);
}
