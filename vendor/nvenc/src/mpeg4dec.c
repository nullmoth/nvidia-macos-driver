// mpeg4dec.c: see mpeg4dec.h. Section numbers refer to ISO/IEC 14496-2.
#include "mpeg4dec.h"
#include "blocklinear.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "class/clc56f.h"
#include "class/cl2080_notification.h"
#include "clc9b0.h"
#include "nvdec_drv.h"

static NvU32 hi(NvU64 a) { return (NvU32)(a >> 8); }
static double now_ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }

static const uint8_t zigzag[64] = {
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63 };
static const uint8_t default_intra[64] = {   // 6.3.3, raster order
    8, 17, 18, 19, 21, 23, 25, 27, 17, 18, 19, 21, 23, 25, 27, 28, 20, 21, 22, 23, 24, 26, 28, 30, 21, 22, 23, 24, 26, 28, 30, 32,
    22, 23, 24, 26, 28, 30, 32, 35, 23, 24, 26, 28, 30, 32, 35, 38, 25, 26, 28, 30, 32, 35, 38, 41, 27, 28, 30, 32, 35, 38, 41, 45 };
static const uint8_t default_nonintra[64] = {
    16, 17, 18, 19, 20, 21, 22, 23, 17, 18, 19, 20, 21, 22, 23, 24, 18, 19, 20, 21, 22, 23, 24, 25, 19, 20, 21, 22, 23, 24, 26, 27,
    20, 21, 22, 23, 25, 26, 27, 28, 21, 22, 23, 24, 26, 27, 28, 30, 22, 23, 24, 26, 27, 28, 30, 31, 23, 24, 25, 27, 28, 30, 31, 33 };

typedef struct { const uint8_t *p; size_t n, bit; } Bits;
static unsigned rb(Bits *b, int k)
{
    unsigned v = 0;
    while (k--) { v <<= 1; if (b->bit < b->n * 8) v |= (b->p[b->bit >> 3] >> (7 - (b->bit & 7))) & 1; b->bit++; }
    return v;
}
// a quantiser matrix: up to 64 values in zigzag order, a 0 ends it and repeats the last value (6.3.3)
static void read_matrix(Bits *b, uint8_t *m)
{
    int last = 0, i = 0;
    for (; i < 64; i++) { const int v = (int)rb(b, 8); if (!v) break; m[zigzag[i]] = (uint8_t)(last = v); }
    for (; i < 64; i++) m[zigzag[i]] = (uint8_t)last;
}

bool mpeg4_next_unit(const uint8_t *buf, size_t n, size_t *pos, const uint8_t **start, size_t *len)
{
    size_t i = *pos;
    while (i + 3 < n && !(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)) i++;
    if (i + 3 >= n) return false;
    size_t e = i + 4;
    while (e + 2 < n && !(buf[e] == 0 && buf[e + 1] == 0 && buf[e + 2] == 1)) e++;
    if (e + 2 >= n) e = n;
    *start = buf + i; *len = e - i; *pos = e;
    return true;
}

bool h263_next_picture(const uint8_t *buf, size_t n, size_t *pos, const uint8_t **start, size_t *len)
{
    size_t i = *pos;
    while (i + 2 < n && !(buf[i] == 0 && buf[i + 1] == 0 && (buf[i + 2] & 0xfc) == 0x80)) i++;
    if (i + 2 >= n) return false;
    size_t e = i + 3;
    while (e + 2 < n && !(buf[e] == 0 && buf[e + 1] == 0 && (buf[e + 2] & 0xfc) == 0x80)) e++;
    if (e + 2 >= n) e = n;
    *start = buf + i; *len = e - i; *pos = e;
    return true;
}

int mpeg4dec_open(Mpeg4Dec *d, Rm *rm, unsigned maxW, unsigned maxH)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 15) & ~15u; d->maxH = (maxH + 31) & ~31u; d->fwd = d->bwd = -1;
    d->pitch = ((size_t)d->maxW + 63) & ~(size_t)63;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "mpeg4dec: channel failed\n"); return -1; }
    const size_t bs = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x10000 }, { &d->bs, bs + 0x10000 }, { &d->offs, 0x1000 }, { &d->status, 0x1000 },
        { &d->hist, 8u << 20 }, { &d->scratch, 0x10000 }, { &d->coloc, (size_t)(d->maxW / 16) * (d->maxH / 16) * 32 + 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++)
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "mpeg4dec: alloc failed\n"); return -1; }
    for (int i = 0; i < MPEG4_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "mpeg4dec: surface alloc failed\n"); return -1; }
    memset(d->hist.cpu, 0, d->hist.size); memset(d->scratch.cpu, 0, d->scratch.size); memset(d->coloc.cpu, 0, d->coloc.size);
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void mpeg4dec_close(Mpeg4Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->offs, &d->status, &d->hist, &d->scratch, &d->coloc, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < MPEG4_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
}

void mpeg4dec_release(Mpeg4Dec *d, int surf) { if (surf >= 0 && surf < MPEG4_SURFACES) d->busy[surf] = false; }

void mpeg4dec_read_nv12(Mpeg4Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, w, (h + 1) / 2, d->pitch);
}

void mpeg4dec_read_i420(Mpeg4Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
{
    const uint8_t *L = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    const unsigned cw = (w + 1) / 2, ch = (h + 1) / 2;
    uint8_t *u = dst + (size_t)w * h, *v = u + (size_t)cw * ch;
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x += 16) {
        unsigned n = w - x < 16 ? w - x : 16;
        memcpy(dst + (size_t)y * w + x, L + nvenc_in_offset(x, y, d->pitch), n);
    }
    for (unsigned y = 0; y < ch; y++) for (unsigned x = 0; x < cw; x++) {
        u[(size_t)y * cw + x] = C[nvenc_in_offset(2 * x, y, d->pitch)];
        v[(size_t)y * cw + x] = C[nvenc_in_offset(2 * x + 1, y, d->pitch)];
    }
}

// VOL header (6.2.3): only rectangular, non-scalable layers
static int parse_vol(Mpeg4Dec *d, Bits *b)
{
    Mpeg4Vol *v = &d->vol; memset(v, 0, sizeof *v);
    rb(b, 1); rb(b, 8);                                         // random_accessible_vol, video_object_type_indication
    v->verid = 1;
    if (rb(b, 1)) { v->verid = (int)rb(b, 4); rb(b, 3); }
    if (rb(b, 4) == 15) rb(b, 16);                              // extended PAR
    if (rb(b, 1)) {                                             // vol_control_parameters
        rb(b, 2); v->low_delay = (int)rb(b, 1);
        if (rb(b, 1)) { rb(b, 15); rb(b, 1); rb(b, 15); rb(b, 1); rb(b, 15); rb(b, 1); rb(b, 3); rb(b, 11); rb(b, 1); rb(b, 15); rb(b, 1); }
    }
    const int shape = (int)rb(b, 2);
    if (shape != 0) { fprintf(stderr, "mpeg4dec: only rectangular video objects\n"); return -1; }
    rb(b, 1); v->time_res = (int)rb(b, 16); rb(b, 1);
    v->time_bits = 1; while ((1 << v->time_bits) < v->time_res) v->time_bits++;
    if (v->time_res <= 1) v->time_bits = 1;
    v->fixed_rate = (int)rb(b, 1); if (v->fixed_rate) rb(b, v->time_bits);
    rb(b, 1); v->width = (int)rb(b, 13); rb(b, 1); v->height = (int)rb(b, 13); rb(b, 1);
    v->interlaced = (int)rb(b, 1); rb(b, 1);                   // obmc_disable
    v->sprite = (int)rb(b, v->verid == 1 ? 1 : 2);
    if (v->sprite == 1 || v->sprite == 2) {
        if (v->sprite != 2) { rb(b, 14); rb(b, 14); rb(b, 14); rb(b, 14); }
        rb(b, 6); rb(b, 2); rb(b, 1); if (v->sprite != 2) rb(b, 1);
    }
    if (rb(b, 1)) { fprintf(stderr, "mpeg4dec: not_8_bit streams are not supported\n"); return -1; }
    v->quant_type = (int)rb(b, 1);
    memcpy(v->intra, default_intra, 64); memcpy(v->nonintra, default_nonintra, 64);
    if (v->quant_type) { if (rb(b, 1)) read_matrix(b, v->intra); if (rb(b, 1)) read_matrix(b, v->nonintra); }
    if (v->verid != 1) v->quarter_sample = (int)rb(b, 1);
    if (!rb(b, 1)) { fprintf(stderr, "mpeg4dec: complexity estimation headers are not supported\n"); return -1; }
    v->resync_marker_disable = (int)rb(b, 1);
    v->data_partitioned = (int)rb(b, 1); if (v->data_partitioned) v->reversible_vlc = (int)rb(b, 1);
    // data partitioning: nvdec_mpeg4_pic_s has no field for it and every VOP ran into the timeout (T14.02)
    if (v->data_partitioned) { fprintf(stderr, "mpeg4dec: data partitioned streams are not supported by NVDEC\n"); return -1; }
    if (v->verid != 1) { if (rb(b, 1)) { fprintf(stderr, "mpeg4dec: newpred is not supported\n"); return -1; } rb(b, 1); }
    if (rb(b, 1)) { fprintf(stderr, "mpeg4dec: scalable layers are not supported\n"); return -1; }
    v->valid = true;
    return 0;
}

static int free_surface(Mpeg4Dec *d)
{
    for (int i = 0; i < MPEG4_SURFACES; i++) if (!d->busy[i] && i != d->fwd && i != d->bwd) return i;
    return -1;
}

typedef struct { int type, rnd, fcode_f, fcode_b, top_field_first, alt_vscan, coded; } VopInfo;

// one VOP (or H.263 picture) through the engine; vop / len from its start code
static int decode_vop(Mpeg4Dec *d, const uint8_t *vop, size_t len, const VopInfo *vi)
{
    const Mpeg4Vol *v = &d->vol;
    if ((unsigned)v->width > d->maxW || (unsigned)v->height > d->maxH) { fprintf(stderr, "mpeg4dec: %dx%d is larger than the session\n", v->width, v->height); return -1; }
    if (vi->type == 3 && v->sprite == 1) { fprintf(stderr, "mpeg4dec: static sprites are not supported\n"); return -1; }
    const bool anchor = vi->type != 2;
    d->cur_w = v->width; d->cur_h = v->height; d->cur_type = vi->type;
    if (!vi->coded) {                                           // not coded: the last anchor is shown again (7.3.1)
        if (d->bwd < 0) return 0;
        d->cur_surf = d->bwd; d->cur_shown = false; d->busy[d->bwd] = true;
        return 1;
    }
    if (vi->type == 2 && (d->fwd < 0 || d->bwd < 0)) return 0;  // a B-VOP without both anchors
    if (vi->type != 0 && d->bwd < 0) return 0;
    const int surf = free_surface(d);
    if (surf < 0) { fprintf(stderr, "mpeg4dec: no free surface\n"); return -1; }
    const int fref = vi->type == 2 ? d->fwd : vi->type ? d->bwd : surf, bref = vi->type == 2 ? d->bwd : fref;

    // the end of the picture: a following VOP start code (found by experiment: H.263 pictures, which end in no 00 00 01
    // code of their own, ran into the 5 ms timeout with MPEG-4's sequence end code or H.263's EOS / PSC)
    static const uint8_t eos[16] = { 0, 0, 1, 0xb6, 0, 0, 1, 0xb6 };
    memcpy(d->bs.cpu, vop, len);
    ((NvU32 *)d->offs.cpu)[0] = 0;
    nvdec_mpeg4_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    memcpy(ps->eos, eos, 16); ps->explicitEOSPresentFlag = 1;
    ps->stream_len = (unsigned)len; ps->slice_count = 1;
    ps->scratch_pic_buffer_size = (unsigned)d->scratch.size;
    ps->FrameWidth = (short)v->width; ps->FrameHeight = (short)v->height;
    ps->vop_time_increment_bitcount = (char)v->time_bits; ps->resync_marker_disable = (char)v->resync_marker_disable;
    ps->tileFormat = 1; ps->gob_height = 0;
    ps->width = v->width; ps->height = v->height;
    ps->FrameStride[0] = ps->FrameStride[1] = (unsigned)d->pitch;
    ps->luma_bot_offset = ps->chroma_bot_offset = (unsigned)(d->pitch * 16 >> 8);   // unused for frames (guess as for MPEG-2)
    ps->HistBufferSize = (unsigned)(d->hist.size >> 8);
    ps->trd[0] = (int)d->pp_time; ps->trb[0] = (int)d->pb_time;
    if (d->t_frame > 0) {                                       // field distances for interlaced direct mode (as ffmpeg derives them)
        const int64_t t = d->t_frame, lnb = d->last_non_b_time, pp = d->pp_time;
        #define RDIV(a, b) (((a) + (b) / 2) / (b))
        ps->trd[1] = (int)((RDIV(lnb, t) - RDIV(lnb - pp, t)) * 2);
        ps->trb[1] = (int)((RDIV(d->cur_time, t) - RDIV(lnb - pp, t)) * 2);
        #undef RDIV
    }
    ps->vop_fcode_forward = (short)vi->fcode_f; ps->vop_fcode_backward = (short)vi->fcode_b;
    ps->interlaced = (unsigned char)v->interlaced; ps->quant_type = (unsigned char)v->quant_type;
    ps->quarter_sample = (unsigned char)v->quarter_sample; ps->short_video_header = v->short_header;
    ps->ptype = (unsigned char)(vi->type == 3 ? 1 : vi->type);  // GMC S-VOPs: no fields for the warp; decoded as P (wrong when GMC is used)
    ps->rnd = (unsigned char)vi->rnd; ps->alternate_vertical_scan_flag = (unsigned char)vi->alt_vscan;
    ps->top_field_flag = (unsigned char)vi->top_field_first;
    memcpy(ps->intra_quant_mat, v->intra, 64); memcpy(ps->nonintra_quant_mat, v->nonintra, 64);
    const int slot[3] = { surf, fref, bref };   // as for MPEG-2: the picture decoded, forward, backward

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_MPEG4);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_MPEG4) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, 0u);
    chan_mthd(c, 0, NVC9B0_SET_SLICE_OFFSETS_BUF_OFFSET, 1, hi(d->offs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(d->hist.gpu));
    // I / P-VOPs write a skip bitmap (one bit per macroblock) at the start of the scratch buffer and 32 bytes per
    // macroblock of co-located data; B-VOPs read both (found by experiment, T14.02). Without the scratch buffer the engine
    // reads address 0.
    chan_mthd(c, 0, NVC9B0_SET_PIC_SCRATCH_BUF_OFFSET, 1, hi(d->scratch.gpu));
    chan_mthd(c, 0, NVC9B0_SET_COLOC_DATA_OFFSET, 1, hi(d->coloc.gpu));
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    for (int i = 0; i < 3; i++) {
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->luma[slot[i]].gpu));
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->chroma[slot[i]].gpu));
    }
    chan_mthd(c, 0, NVC9B0_EXECUTE, 1, 0u);
    const NvU32 val = ++d->semval; volatile NvU32 *sv = d->sem.cpu;
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_A, 3, (NvU32)(d->sem.gpu >> 32), (NvU32)d->sem.gpu, val);
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_D, 1, DRF_DEF(C9B0, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B0, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B0, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    const double t0 = now_ms();
    chan_submit(c);
    while (sv[0] != val && now_ms() - t0 < 1000) { }
    d->engine_ms = now_ms() - t0;
    if (sv[0] != val) {
        NvU64 fa; NvU32 ft; char fs[32];
        fprintf(stderr, "mpeg4dec: no completion (error notifier 0x%x)\n", chan_error(c));
        if (chan_error(c) && !chan_fault(c, &fa, &ft, fs)) fprintf(stderr, "mpeg4dec: MMU fault at 0x%llx type %u %s (bs 0x%llx setup 0x%llx hist 0x%llx scratch 0x%llx)\n",
            (unsigned long long)fa, ft, fs, (unsigned long long)d->bs.gpu, (unsigned long long)d->setup.gpu, (unsigned long long)d->hist.gpu, (unsigned long long)d->scratch.gpu);
        return -1;
    }
    const nvdec_status_s *st = d->status.cpu;
    d->mbs_ok = st->mbs_correctly_decoded; d->mbs_err = st->mbs_in_error; d->err_status = st->error_status;
    if (d->verbose)
        printf("  dec: surf %d %c time %lld fwd %d bwd %d trd %d trb %d fcode %d/%d rnd %d -> %u MBs ok, %u err, status 0x%x, %.2f ms\n", surf, "IPBSN"[vi->type],
               (long long)d->cur_time, fref, bref, ps->trd[0], ps->trb[0], vi->fcode_f, vi->fcode_b, vi->rnd, d->mbs_ok, d->mbs_err, d->err_status, d->engine_ms);
    d->cur_surf = surf; d->cur_shown = true; d->busy[surf] = true;
    if (anchor) { d->fwd = d->bwd; d->bwd = surf; }
    return 1;
}

int mpeg4dec_unit(Mpeg4Dec *d, const uint8_t *start, size_t len)
{
    if (len < 4) return 0;
    const int code = start[3];
    Bits b = { start + 4, len - 4, 0 };
    if (code >= 0x20 && code <= 0x2f) {
        const int r = parse_vol(d, &b);
        if (d->verbose) printf("  VOL: %dx%d verid %d time_res %d (%d bits) interlaced %d sprite %d quant_type %d qpel %d resync_disable %d partitioned %d -> %d\n",
                               d->vol.width, d->vol.height, d->vol.verid, d->vol.time_res, d->vol.time_bits, d->vol.interlaced, d->vol.sprite,
                               d->vol.quant_type, d->vol.quarter_sample, d->vol.resync_marker_disable, d->vol.data_partitioned, r);
        return r ? -1 : 0;
    }
    if (code != 0xb6 || !d->vol.valid) return 0;                // VOS, VO, GOV, user data: nothing the engine needs
    const Mpeg4Vol *v = &d->vol;
    VopInfo vi = { 0 };
    vi.type = (int)rb(&b, 2);
    int incr = 0; while (rb(&b, 1)) incr++;                     // modulo_time_base
    rb(&b, 1);
    // the marker after vop_time_increment must be 1; streams whose VOL gives the wrong bit count (no VOL, old encoders:
    // FATE demo.m4v) are recovered the way ffmpeg's decoder does it: the count after which the following bits read marker,
    // vop_coded = 1, [rounding,] intra_dc_vlc_thr = 0. The engine parses the VOP header itself with the count we give it.
    { Bits t = b; rb(&t, v->time_bits);
      if (!rb(&t, 1)) for (int k = 1; k < 16; k++) {
          Bits u = b; rb(&u, k);
          const unsigned x = vi.type == 1 ? rb(&u, 6) & 0x37 : rb(&u, 5) & 0x1f;
          if (x == (vi.type == 1 ? 0x30u : 0x18u)) { if (d->verbose) printf("  VOP: time increment bits %d -> %d\n", d->vol.time_bits, k); d->vol.time_bits = k; break; }
      } }
    const int inc = (int)rb(&b, v->time_bits); rb(&b, 1);
    vi.coded = (int)rb(&b, 1);
    if (d->verbose > 1) printf("  VOP: type %d modulo %d increment %d coded %d (bits %02x %02x %02x)\n", vi.type, incr, inc, vi.coded, start[4], start[5], start[6]);
    // time stamps (7.7.2.1 / 7.7.2.2): pp_time = TRD, pb_time = TRB
    if (vi.type != 2) {
        d->last_time_base = d->time_base; d->time_base += incr;
        d->cur_time = d->time_base * v->time_res + inc;
        d->pp_time = d->cur_time - d->last_non_b_time; d->last_non_b_time = d->cur_time;
        if (d->pp_time > 0 && (d->t_frame == 0 || d->pp_time < d->t_frame)) d->t_frame = d->pp_time;
    } else {
        d->cur_time = (d->last_time_base + incr) * v->time_res + inc;
        d->pb_time = d->pp_time - (d->last_non_b_time - d->cur_time);
        if (d->pb_time > 0 && d->pb_time < d->t_frame) d->t_frame = d->pb_time;
    }
    if (vi.coded) {
        if (vi.type == 1 || (vi.type == 3 && v->sprite == 2)) vi.rnd = (int)rb(&b, 1);
        rb(&b, 3);                                              // intra_dc_vlc_thr
        if (v->interlaced) { vi.top_field_first = (int)rb(&b, 1); vi.alt_vscan = (int)rb(&b, 1); }
        if (vi.type == 3 && v->sprite == 2) { fprintf(stderr, "mpeg4dec: GMC S-VOPs are not supported\n"); return -1; }
        rb(&b, 5);                                              // vop_quant
        if (vi.type != 0) vi.fcode_f = (int)rb(&b, 3);
        if (vi.type == 2) vi.fcode_b = (int)rb(&b, 3);
    }
    return decode_vop(d, start, len, &vi);
}

// H.263 baseline picture header (H.263 5.1; 14496-2 6.2.5.2 short_video_header). NOT DECODED: with short_video_header
// the engine reports every macroblock decoded in 0.1 ms and writes nothing (T14.02; NVIDIA lists no H.263 decoding), so
// this returns an error unless MPEG4_TRY_H263 is set (to retry the experiment).
int mpeg4dec_h263_picture(Mpeg4Dec *d, const uint8_t *start, size_t len)
{
    if (!getenv("MPEG4_TRY_H263")) { fprintf(stderr, "mpeg4dec: H.263 is not supported by NVDEC (short_video_header writes no picture)\n"); return -1; }
    Bits b = { start, len, 22 };
    rb(&b, 8);                                                  // temporal_reference
    rb(&b, 1); rb(&b, 1); rb(&b, 3);                            // marker, zero, split / camera / freeze
    static const int fw[8] = { 0, 128, 176, 352, 704, 1408, 0, 0 }, fh[8] = { 0, 96, 144, 288, 576, 1152, 0, 0 };
    const int fmt = (int)rb(&b, 3);
    if (!fw[fmt]) { fprintf(stderr, "mpeg4dec: H.263 source format %d (PLUSPTYPE / H.263+) is not supported\n", fmt); return -1; }
    VopInfo vi = { .type = (int)rb(&b, 1), .coded = 1 };
    if (rb(&b, 4)) { fprintf(stderr, "mpeg4dec: H.263 annexes (UMV, SAC, AP, PB) are not supported\n"); return -1; }
    Mpeg4Vol *v = &d->vol;
    if (!v->valid || !v->short_header || v->width != fw[fmt]) {
        memset(v, 0, sizeof *v);
        v->valid = v->short_header = true; v->width = fw[fmt]; v->height = fh[fmt]; v->time_res = 30000; v->time_bits = 15;
        v->resync_marker_disable = 1;
        memcpy(v->intra, default_intra, 64); memcpy(v->nonintra, default_nonintra, 64);
    }
    vi.fcode_f = 1;
    return decode_vop(d, start, len, &vi);
}
